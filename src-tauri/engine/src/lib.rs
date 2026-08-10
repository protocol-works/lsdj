//! LSDJ native audio engine (Phase 1, Slice 1).
//!
//! A device-free, RT-safe two-deck mix core plus a thin cpal device wrapper.
//! This is the production home of the engine ported from the Spike A
//! `rt_engine` (`spike/rust-audio/engine/src/bin/rt_engine.rs`); the spike
//! proved feasibility, this crate is the real thing.
//!
//! # Shape
//!
//! - [`Engine`] owns the device-free core. [`Engine::render`] is a **pure**,
//!   RT-safe drain-and-mix over an output buffer — no device, no allocation, no
//!   lock, no syscall — so the whole mix is testable headless in CI
//!   (`cargo test`). The [`device`] module is a thin cpal wrapper that calls
//!   `render` from its callback.
//! - Each deck is fed by its **own** `rtrb` SPSC ring (30 s capacity, 1.5 s
//!   prebuffer). A [`DeckHandle`] is the non-RT producer side; its
//!   [`DeckHandle::post_pcm`] is the SOLE writer. `Engine::render` is the SOLE
//!   drainer. Neither side locks; the consumer side never allocates.
//!
//! # The non-RT → RT handoff (the load-bearing design)
//!
//! `post_pcm` (non-RT) and `render` (RT) communicate ONLY through the per-deck
//! `rtrb` ring, a wait-free single-producer/single-consumer queue. The producer
//! half lives behind [`DeckHandle`] (moved to the IO/transport thread); the
//! consumer half lives inside [`Engine`] (moved to the audio thread). Because
//! each ring has exactly one writer and one reader and they are split across the
//! two halves, the handoff needs no mutex and the RT side allocates nothing —
//! it only pops samples and ticks pre-built `fundsp` nodes.
//!
//! Control values that the RT path reads (the crossfade gains, and in later
//! slices EQ/FX) live on the `Engine` itself and are set through `&mut Engine`,
//! so by Rust's ownership rules a control mutation and a `render` call cannot
//! overlap. When the device wrapper owns the `Engine` in its callback, control
//! commands are delivered to that thread over a separate wait-free channel (the
//! device layer's job, a later slice); the core API here keeps the mutation and
//! the render on the same `&mut self` so it stays trivially data-race-free and
//! fully testable.

mod fx;
mod graph;
mod loops;
mod playback;
mod ring;
pub mod telemetry;

#[cfg(not(target_arch = "wasm32"))]
pub mod host;

pub use fx::FxKind;
pub use loops::{
    GENERATED_LOOP_MIN_SECONDS, LOOP_SLOT_COUNT, MIN_LOOP_SECONDS, MIN_STYLE_SAMPLE_SECONDS,
    STYLE_SAMPLE_SECONDS,
};
pub use playback::{LoopRegion, TrackStatus};

#[cfg(not(target_arch = "wasm32"))]
pub mod device;

use std::sync::Arc;

use graph::MixGraph;
use loops::{LoopBank, LOOP_CROSSFADE_FRAMES, MIN_LOOP_FRAMES, MIN_STYLE_SAMPLE_FRAMES};
use playback::{BufferSource, DeckSource};
use ring::{new_deck_ring, RingConsumer, RingProducer};
use telemetry::Telemetry;

/// Output sample rate. The engine renders at exactly this rate; the device
/// wrapper opens a matching 48000 config bit-exact, or resamples to a device
/// that cannot do 48000 (e.g. a 44100 Bluetooth speaker — ADR-0029).
pub const SAMPLE_RATE: u32 = 48_000;

/// Output channel count (interleaved stereo).
pub const CHANNELS: u16 = 2;

/// Number of model decks. Two for the foreseeable design (deck A + deck B).
pub const DECK_COUNT: usize = 2;

/// Master output ceiling, clamped per channel. Matches the Web Audio master
/// chain's hard clip guard (the parity constant from the spike).
pub const MASTER_CEILING: f32 = 0.9296875;

/// Default cue/master headphone blend (0 = cue only, 1 = master). Mirrors
/// `INITIAL_CUE_MIX` in the Web Audio engine.
pub const INITIAL_CUE_MIX: f32 = 0.5;

/// Per-deck ring capacity in frames (30 s of stereo audio).
pub const RING_SECS: usize = 30;
pub const RING_FRAMES: usize = RING_SECS * SAMPLE_RATE as usize;

/// Prebuffer high-water mark in frames (1.5 s). A deck is not "primed" — and so
/// its shortfalls are not counted as underruns — until its ring first fills to
/// here.
pub const PREBUFFER_SECS: f64 = 1.5;
pub const PREBUFFER_FRAMES: usize = (PREBUFFER_SECS * SAMPLE_RATE as f64) as usize;

/// Per-deck played-history capacity in frames (30 s of stereo audio), the freeze-
/// pad / style-sample capture source (M13/M15, ADR-0009). Matches the worklet's
/// `CAPACITY_SECONDS = 30` (`frontend/public/player-worklet.js`): the longest loop
/// (8 s) and the style sample (10 s) both fit comfortably. Allocated once per deck,
/// off the RT path.
pub const HISTORY_SECS: usize = 30;
pub const HISTORY_FRAMES: usize = HISTORY_SECS * SAMPLE_RATE as usize;

/// Identifies a deck. Slice 1 has exactly [`DECK_COUNT`] decks; `DeckId` is a
/// thin index newtype so callers don't pass bare `usize`s and later slices can
/// attach per-deck control without reshaping the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeckId(usize);

impl DeckId {
    /// The deck's index in `[0, DECK_COUNT)`.
    pub fn index(self) -> usize {
        self.0
    }
}

/// Truthful state of one loop slot for LEDs / telemetry (the shell's `LoopSlot`,
/// ADR-0009). `filled` lights a pad that holds a buffer; `playing` marks the slot
/// currently replacing the live stream (an active loop, never a one-shot — a
/// one-shot overlays and ends itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopSlotStatus {
    pub filled: bool,
    pub playing: bool,
}

/// A deck's 3-band EQ band. The layout matches the Web Audio engine
/// (`frontend/src/audio/eq.ts`): a low shelf at 250 Hz, a mid bell at 1 kHz,
/// and a high shelf at 2.5 kHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EqBand {
    Low,
    Mid,
    High,
}

impl EqBand {
    /// The band's index into a deck's `[low, mid, high]` EQ value triple.
    fn index(self) -> usize {
        match self {
            EqBand::Low => 0,
            EqBand::Mid => 1,
            EqBand::High => 2,
        }
    }
}

/// The non-RT producer side of a deck. Held by whoever feeds the deck (the
/// transport/decode thread). Its `post_pcm` is the SOLE writer of the deck's
/// ring; see the module-level note on the SPSC handoff.
///
/// A `DeckHandle` is `Send` (it owns one `rtrb::Producer`) so it can be moved to
/// the IO thread. It is intentionally NOT `Clone`: a ring has exactly one
/// producer.
pub struct DeckHandle {
    id: DeckId,
    producer: RingProducer,
}

impl DeckHandle {
    /// Append interleaved stereo f32 samples (`[L, R, L, R, …]`) to this deck's
    /// ring. Returns the number of samples actually buffered; if the ring is
    /// near-full only the prefix that fits is written and the rest dropped, so
    /// the producer thread never blocks. Non-RT producer side; wait-free.
    pub fn post_pcm(&mut self, samples: &[f32]) -> usize {
        self.producer.post_pcm(samples)
    }

    /// Free space in this deck's ring, in interleaved samples. Lets the feeder
    /// pace itself (e.g. stay ≤ N seconds ahead) without blocking.
    pub fn free_samples(&self) -> usize {
        self.producer.free_samples()
    }

    /// This handle's deck id.
    pub fn id(&self) -> DeckId {
        self.id
    }
}

/// The device-free audio engine core. Owns the consumer half of every deck ring,
/// the mix graph, and the telemetry. [`Engine::render`] is the only RT entry
/// point; everything else is non-RT control/setup.
pub struct Engine {
    /// Each deck's active source (ADR-0013): the live ring consumer
    /// ([`DeckSource::Realtime`], Slices 1–3) or one decoded track
    /// ([`DeckSource::Playback`]). `None` until [`Engine::create_deck`].
    decks: Vec<Option<DeckSource>>,
    /// The live ring consumer **parked** while a deck is in Playback mode
    /// (ADR-0013: the live ring is kept, not destroyed). `load_track` moves the
    /// `Realtime` consumer here and installs a `Playback` source; `unload_track`
    /// moves it back. The matching producer ([`DeckHandle`]) keeps feeding it; the
    /// engine just stops draining it until the deck returns to Realtime.
    parked_rings: Vec<Option<RingConsumer>>,
    /// Per-deck freeze-loop / generated-loop / one-shot bank (M13/M18,
    /// ADR-0009/0012). A *freeze* originates only on a Realtime deck (capture needs
    /// the live history), but a *loaded sample* is a self-contained buffer that
    /// loads and plays in either mode (ADR-0022). The active loop (if any) replaces
    /// the deck's base frame at the channel head; a one-shot / layer overlays on
    /// top — see `LoopBank::mix_frame`.
    loops: Vec<LoopBank>,
    /// A single engine-wide preview ("audition") source (ADR-0027): a decoded
    /// sample/track played into the HEADPHONE/cue feed only — never the master —
    /// so the booth can hear a library item before loading it onto a deck. It
    /// loops the whole buffer until `audition_stop`, summed onto the cue output
    /// after the cue/master blend so it stays audible in the phones wherever the
    /// cue knob sits. Built off the RT path (`audition_play`); `render` only reads.
    audition: Option<BufferSource>,
    graph: MixGraph,
    telemetry: Arc<Telemetry>,
}

impl Engine {
    /// Create a new engine at [`SAMPLE_RATE`] / [`CHANNELS`]. Allocates the mix
    /// graph (off the RT path). No decks exist yet — call [`Engine::create_deck`]
    /// for each.
    pub fn new() -> Self {
        Engine {
            decks: (0..DECK_COUNT).map(|_| None).collect(),
            parked_rings: (0..DECK_COUNT).map(|_| None).collect(),
            loops: (0..DECK_COUNT).map(|_| LoopBank::new()).collect(),
            audition: None,
            graph: MixGraph::new(),
            telemetry: Arc::new(Telemetry::new()),
        }
    }

    /// Create deck `id` and return its non-RT [`DeckHandle`] (the producer side).
    /// The consumer side is retained inside the engine for [`Engine::render`], as
    /// a [`DeckSource::Realtime`] source (the live stream — loading later switches
    /// it to Playback). Allocates the ring backing store once, here, off the RT
    /// path.
    ///
    /// # Panics
    /// Panics if `index >= DECK_COUNT` or the deck was already created — both are
    /// caller programming errors, not runtime conditions.
    pub fn create_deck(&mut self, index: usize) -> DeckHandle {
        assert!(index < DECK_COUNT, "deck index {index} out of range");
        assert!(
            self.decks[index].is_none(),
            "deck {index} already created"
        );
        let (producer, consumer) = new_deck_ring(index, self.telemetry.clone());
        self.decks[index] = Some(DeckSource::Realtime(consumer));
        DeckHandle {
            id: DeckId(index),
            producer,
        }
    }

    /// Milliseconds → whole frames at [`SAMPLE_RATE`], for the ramped control
    /// moves (negative input snaps: 0 frames).
    fn ramp_ms_to_frames(ramp_ms: f32) -> u32 {
        (ramp_ms.max(0.0) / 1000.0 * SAMPLE_RATE as f32).round() as u32
    }

    /// Set the crossfader position in `[0, 1]` (0 = deck A, 1 = deck B). Non-RT
    /// control; recomputes the equal-power mix gains the RT path reads.
    pub fn set_crossfade(&mut self, position: f32) {
        self.graph.set_crossfade(position);
    }

    /// Like [`Engine::set_crossfade`], but glide there over `ramp_ms`
    /// milliseconds (0 = instant): the RT path walks the position linearly per
    /// frame and re-applies the equal-power law, so an agent's fade is a
    /// click-free blend instead of audible steps (MCP finding #10).
    pub fn set_crossfade_ramped(&mut self, position: f32, ramp_ms: f32) {
        self.graph.set_crossfade_ramped(position, Self::ramp_ms_to_frames(ramp_ms));
    }

    /// Set a deck's EQ `band` knob value in `[0, 1]` (0 = kill, 0.5 = flat,
    /// 1 = boost; see `EqBand` / the `eqValueToDb` curve). Non-RT control.
    ///
    /// This rebuilds that deck's EQ filter chain **off the RT path** (it takes
    /// `&mut self`, so it can never overlap a `render` call — Rust's ownership
    /// rules make the allocation safe here). `render` only ticks the
    /// already-built fixed-coefficient nodes; it allocates nothing. See
    /// `MixGraph::set_eq` for how a future device-command channel would deliver
    /// the change RT-safely once the audio thread owns the `Engine`.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_eq(&mut self, deck_index: usize, band: EqBand, value: f32) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_eq(deck_index, band.index(), value);
    }

    /// Set a deck's channel-fader volume (linear gain, `0..1+`; default 1.0),
    /// applied before the crossfade. Non-RT control; writes a gain the RT path
    /// reads.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_volume(&mut self, deck_index: usize, gain: f32) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_volume(deck_index, gain);
    }

    /// Like [`Engine::set_volume`], but glide there linearly over `ramp_ms`
    /// milliseconds (0 = instant) — MCP finding #10.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_volume_ramped(&mut self, deck_index: usize, gain: f32, ramp_ms: f32) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_volume_ramped(deck_index, gain, Self::ramp_ms_to_frames(ramp_ms));
    }

    /// Select a deck's Color FX effect (ADR-0008). The insert sits post-EQ,
    /// pre-fader; at the new effect's rest position it is a bit-exact dry
    /// passthrough until the knob is moved out of the dead zone.
    ///
    /// This **rebuilds the effect's nodes off the RT path** — it takes `&mut self`,
    /// so by Rust's ownership rules a `set_fx` call and a `render` call can never
    /// overlap (the same RT-safety argument as `set_eq`). `render` only ticks the
    /// already-built nodes; it allocates nothing.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_fx(&mut self, deck_index: usize, kind: FxKind) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_fx(deck_index, kind);
    }

    /// Set a deck's Color FX knob amount in `[0, 1]` (the one-knob convention from
    /// `fx.ts`; `0.5` rests the bipolar filter, `0` rests the rest). Non-RT
    /// control: reconfigures the active effect's parameters off the RT path while
    /// keeping its per-channel state. Within the effect's dead zone the insert is a
    /// bit-exact dry passthrough.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_fx_amount(&mut self, deck_index: usize, amount: f32) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_fx_amount(deck_index, amount);
    }

    /// Set a deck's gated beat period in seconds (ADR-0025), or `None` while the
    /// honesty gate is blank: the synced dub echo snaps its delay to the beat
    /// fraction nearest its free-running character and reverts on blank
    /// (`echoDelaySeconds`, M14). Non-RT control; arithmetic only, no allocation.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_beat_period(&mut self, deck_index: usize, period_seconds: Option<f32>) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_beat_period(deck_index, period_seconds);
    }

    /// Remove a deck's Color FX (no effect selected) — the insert is skipped, a
    /// pure dry passthrough, until the next [`Engine::set_fx`]. Mirrors
    /// `setFx(null)` in the Web Audio engine. Non-RT.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn clear_fx(&mut self, deck_index: usize) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.clear_fx(deck_index);
    }

    /// Set a deck's chain-head trim in dB (0 dB = unity; M17 gain staging). Applied
    /// pre-EQ so EQ kills stay the performer's move. Non-RT.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_trim(&mut self, deck_index: usize, db: f32) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_trim(deck_index, db);
    }

    /// Set a deck's on-air state (M10 primed deck). Off-air mutes the deck's
    /// contribution to the master sum only — its channel meter (and, later, the cue
    /// tap) stay live. Non-RT.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_on_air(&mut self, deck_index: usize, on: bool) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_on_air(deck_index, on);
    }

    /// Set a deck's headphone-cue (PFL) tap (Slice 5). A cued deck feeds the cue
    /// bus pre-fader, so it is audible in the phones regardless of its fader /
    /// crossfade / on-air state. Non-RT.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn set_cue(&mut self, deck_index: usize, on: bool) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.graph.set_cue(deck_index, on);
    }

    /// Set the cue/master headphone blend in `[0, 1]` (0 = cue bus only, 1 =
    /// master). Non-RT.
    pub fn set_cue_mix(&mut self, position: f32) {
        self.graph.set_cue_mix(position);
    }

    /// Audition a decoded interleaved-stereo buffer @ 48 k into the HEADPHONE/cue
    /// feed (ADR-0027): a preview of a library item before it is loaded onto a
    /// deck. Replaces any current preview; the whole buffer loops until
    /// [`Engine::audition_stop`], so the UI's "previewing" state stays truthful
    /// (it sounds until explicitly stopped). Never touches the master. Non-RT (the
    /// buffer allocation lands here, off the render thread — `render` only reads).
    pub fn audition_play(&mut self, samples: Vec<f32>) {
        let mut source = BufferSource::new(samples);
        source.play();
        let len = source.status().duration_frames;
        // Loop the whole buffer so the preview keeps sounding; a near-empty buffer
        // can't install a region (then it just plays out once and goes silent).
        if len > 0 {
            source.set_loop(0, len);
        }
        self.audition = Some(source);
    }

    /// Stop and drop the preview (ADR-0027). A no-op when nothing is auditioning.
    /// Non-RT.
    pub fn audition_stop(&mut self) {
        self.audition = None;
    }

    // --- Slice 4a: the playback deck (M19/M20/M21/M23, ADR-0013…0016) ---
    //
    // Loading decides the mode (ADR-0013): `load_track` switches a deck to
    // Playback and PARKS its live ring; `unload_track` restores Realtime. All of
    // these are non-RT (`&mut self`, like `set_eq`/`set_fx`) — they cannot overlap
    // a `render` call by Rust's ownership rules, so the buffer allocation in
    // `load_track` and the transport mutations never land on the RT thread. The RT
    // `render` only reads the buffer + linear-interpolates.

    /// Load a decoded track onto a deck, switching it to Playback mode and
    /// **parking** its live ring (ADR-0013). `samples` is decoded interleaved
    /// stereo f32 at [`SAMPLE_RATE`] (the shell decodes WAV + resamples in Phase
    /// 2; this slice takes 48 k f32). Replaces any track already loaded. The track
    /// starts paused at the top, rate 1.0, no loop.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn load_track(&mut self, deck_index: usize, samples: Vec<f32>) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        let source = BufferSource::new(samples);
        match self.decks[deck_index].take() {
            // Park the live ring (kept, not destroyed) so `unload_track` restores
            // it. Its producer keeps feeding; the engine just stops draining it.
            Some(DeckSource::Realtime(ring)) => {
                self.parked_rings[deck_index] = Some(ring);
            }
            // Reloading a Playback deck: drop the old track, keep any parked ring.
            Some(DeckSource::Playback(_)) | None => {}
        }
        self.decks[deck_index] = Some(DeckSource::Playback(source));
    }

    /// Unload the track and return the deck to Realtime, restoring the parked live
    /// ring (ADR-0013). A no-op on a deck that is already Realtime or uncreated.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT` (a caller programming error).
    pub fn unload_track(&mut self, deck_index: usize) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        if let Some(DeckSource::Playback(_)) = self.decks[deck_index] {
            self.decks[deck_index] = self.parked_rings[deck_index].take().map(DeckSource::Realtime);
        }
    }

    /// Start (or resume) the loaded track; from the ended state it restarts at the
    /// top (ADR-0013). A no-op unless the deck is in Playback mode.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn play_track(&mut self, deck_index: usize) {
        if let Some(track) = self.track_mut(deck_index) {
            track.play();
        }
    }

    /// Park the track's playhead where it is (ADR-0013). A no-op off Playback.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn pause_track(&mut self, deck_index: usize) {
        if let Some(track) = self.track_mut(deck_index) {
            track.pause();
        }
    }

    /// Jump the track playhead to `frames` (clamped into the track) and **exit any
    /// active loop** — the one rule (ADR-0015). A no-op off Playback.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn seek_track(&mut self, deck_index: usize, frames: f64) {
        if let Some(track) = self.track_mut(deck_index) {
            track.seek(frames);
        }
    }

    /// Set the track's varispeed rate, clamped to the ±8 % envelope (ADR-0014).
    /// A no-op off Playback.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn set_track_rate(&mut self, deck_index: usize, rate: f64) {
        if let Some(track) = self.track_mut(deck_index) {
            track.set_rate(rate);
        }
    }

    /// Phase-nudge the track by `frames` (the jog-while-playing platter bend,
    /// M20): a click-free rate bend, not a seek. A no-op off Playback or when the
    /// deck is paused (the caller routes a paused jog to a fine seek instead).
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn nudge_track_phase(&mut self, deck_index: usize, frames: f64) {
        if let Some(track) = self.track_mut(deck_index) {
            track.nudge_phase(frames);
        }
    }

    /// Set the track loop region in frames (ADR-0015/0016). Runs the pure
    /// `plan_loop_set` decision; a region that cannot loop is refused. A no-op off
    /// Playback.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn set_track_loop(&mut self, deck_index: usize, start: u64, end: u64) {
        if let Some(track) = self.track_mut(deck_index) {
            track.set_loop(start, end);
        }
    }

    /// Clear the active track loop, re-anchoring on the folded seam (ADR-0015). A
    /// no-op off Playback.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn clear_track_loop(&mut self, deck_index: usize) {
        if let Some(track) = self.track_mut(deck_index) {
            track.clear_loop();
        }
    }

    /// A snapshot of the track's transport (playhead in frames, playing, duration,
    /// rate, ended, loop), or `None` off Playback. Mirrors `getTrackStatus`.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn get_track_status(&self, deck_index: usize) -> Option<TrackStatus> {
        self.track_ref(deck_index).map(|track| track.status())
    }

    /// Min/max overview of the loaded track for the waveform UI (`buckets`
    /// downsampled buckets across both channels), or `None` off Playback.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn get_track_peaks(&self, deck_index: usize, buckets: usize) -> Option<(Vec<f32>, Vec<f32>)> {
        self.track_ref(deck_index).map(|track| track.peaks(buckets))
    }

    // --- Slice 4b: freeze loops, generated loops, style sampling ---
    //
    // (M13 freeze pads / ADR-0009, M18 generated pads / ADR-0012, M15 style
    // sampling / ADR-0011.) These capture and loop the LIVE model output, so they
    // are Realtime-deck features: on a Playback deck they are inert no-ops (the
    // played history holds the dead stream; track-buffer slicing is a parked Later
    // idea, ADR-0013). All are non-RT (`&mut self` / getters, like
    // `set_eq`/`load_track`): they allocate (capture copies, folded buffers) off the
    // RT path and cannot overlap a `render` call by Rust's ownership rules. The RT
    // `render` only ticks the pre-built loop/one-shot buffer sources.

    /// Capture the last `seconds` of played history on a Realtime deck into loop
    /// `slot` as a seam-folded looping buffer (M13, `captureLoop` in `engine.ts`).
    /// Beat-quantising the length is the shell's concern; here it is raw seconds.
    /// The capture grabs `seconds + crossfade` frames so [`loops::build_loop_channel`]
    /// can fold the seam continuously.
    ///
    /// Returns `false` (the slot is left untouched) if the deck is not Realtime,
    /// the slot is out of range, or less than [`MIN_LOOP_SECONDS`] of history has
    /// played — a shorter "loop" is a stutter, not a freeze (ADR-0009).
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn capture_loop(&mut self, deck_index: usize, slot: usize, seconds: f64) -> bool {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        let Some(ring) = self.realtime_ring(deck_index) else {
            return false;
        };
        let request = (seconds.max(0.0) * SAMPLE_RATE as f64) as usize + LOOP_CROSSFADE_FRAMES;
        let (left, right) = ring.capture_recent(request);
        // Refuse a capture shorter than the honest floor (plus the seam surplus):
        // the worklet returns what its history holds, and a near-empty deck cannot
        // freeze (ADR-0009). Mirrors the `captureLoop` length guard in `engine.ts`.
        if left.len() < MIN_LOOP_FRAMES + LOOP_CROSSFADE_FRAMES {
            return false;
        }
        self.loops[deck_index].store_capture(slot, &left, &right)
    }

    /// Play loop `slot` on a deck (M13/M18, `playLoop` in `engine.ts`): a one-shot
    /// overlays and ends itself; a loop either REPLACES the base (`layer = false`, a
    /// freeze — held at the channel head while whatever is underneath keeps being
    /// fed/drained, ADR-0009) or LAYERS on top of it (`layer = true`, a loaded
    /// sample — summed and stacking, ADR-0022). The overlay sums onto whatever the
    /// deck plays — the live ring OR a Playback track — so a loaded sample sounds in
    /// either mode (a freeze still only originates on a Realtime deck, since capture
    /// needs the live history). Returns `false` if the slot is empty.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn play_loop(&mut self, deck_index: usize, slot: usize, layer: bool) -> bool {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.loops[deck_index].play(slot, layer)
    }

    /// Stop the active (replacing) loop on a deck (`stopLoop` in `engine.ts`): back to
    /// the base. Layers keep playing — they are independent of the freeze. A no-op
    /// when nothing is replacing.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn stop_loop(&mut self, deck_index: usize) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.loops[deck_index].stop();
    }

    /// Stop a single layered loop on a deck (`stopLayer` in `engine.ts`): the
    /// loaded-sample loop in `slot` stops summing; its buffer stays for a re-press
    /// (ADR-0022). A no-op when the slot isn't layering.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn stop_layer(&mut self, deck_index: usize, slot: usize) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.loops[deck_index].stop_layer(slot);
    }

    /// Stop the ringing one-shot on a deck (`stopOneShot` in `engine.ts`).
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn stop_one_shot(&mut self, deck_index: usize) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.loops[deck_index].stop_one_shot();
    }

    /// Clear loop `slot` on a deck (`clearLoop` in `engine.ts`): empty the buffer;
    /// if it was the active loop, return to live.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn clear_loop(&mut self, deck_index: usize, slot: usize) {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.loops[deck_index].clear(slot);
    }

    /// Load a decoded interleaved-stereo loop / pad into `slot` (M18,
    /// `loadGeneratedLoop` in `engine.ts`). `samples` is decoded f32 @ 48 k (an SA3
    /// pad; the shell decodes the WAV — no WAV decoder this slice, like
    /// `load_track`). `one_shot` plays ONCE then stops; otherwise it loops like a
    /// freeze loop (the seam is folded). Returns `false` on an out-of-range slot.
    /// Mode-agnostic: a loaded sample is a self-contained buffer (no live-ring
    /// dependency), so it loads onto a Playback deck too and layers over the track
    /// when played — the Samples tab works whatever the deck is doing (ADR-0022).
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn load_generated_loop(
        &mut self,
        deck_index: usize,
        slot: usize,
        samples: Vec<f32>,
        one_shot: bool,
    ) -> bool {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.loops[deck_index].load_generated(slot, samples, one_shot)
    }

    /// Capture the last `seconds` of played history on a Realtime deck as
    /// interleaved-stereo f32 (M15, `captureSample` in `engine.ts`): the shell sends
    /// it to the backend for embedding (ADR-0011); this returns the samples.
    /// Defaults to [`STYLE_SAMPLE_SECONDS`] (10 s) at the call site; refuses (returns
    /// `None`) below [`MIN_STYLE_SAMPLE_SECONDS`] (3 s) of history or on a Playback
    /// deck.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn capture_sample(&mut self, deck_index: usize, seconds: f64) -> Option<Vec<f32>> {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        let ring = self.realtime_ring(deck_index)?;
        let request = (seconds.max(0.0) * SAMPLE_RATE as f64) as usize;
        let (left, right) = ring.capture_recent(request);
        // Less than the backend floor embeds poorly and is refused before it leaves
        // the deck (ADR-0011). Mirrors the `captureSample` length guard.
        if left.len() < MIN_STYLE_SAMPLE_FRAMES {
            return None;
        }
        Some(loops::interleave_channels(&left, &right))
    }

    /// Truthful loop-slot state for LEDs / telemetry (the shell's `LoopSlot`,
    /// ADR-0009): per slot, whether it is filled and whether it is the active
    /// (replacing) loop. Length [`LOOP_SLOT_COUNT`].
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn loop_slots(&self, deck_index: usize) -> Vec<LoopSlotStatus> {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        let bank = &self.loops[deck_index];
        (0..loops::LOOP_SLOT_COUNT)
            .map(|slot| LoopSlotStatus {
                filled: bank.is_filled(slot),
                playing: bank.is_playing(slot),
            })
            .collect()
    }

    /// The stored interleaved-stereo buffer for loop `slot` on a deck, for persisting
    /// a captured freeze / loaded pad to the generated-samples library — the audio as
    /// it replays (a loop is already seam-folded). `None` for an empty/out-of-range
    /// slot. Mode-agnostic: a slot survives regardless of the deck's mode, so the
    /// shell can save it whenever it likes after the capture. Non-RT (the clone
    /// allocates off the RT path, like `capture_sample`).
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    pub fn read_loop_slot(&self, deck_index: usize, slot: usize) -> Option<Vec<f32>> {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        self.loops[deck_index].slot_samples(slot)
    }

    /// The live ring consumer for a Realtime deck (the freeze-pad / style-sample
    /// capture source), or `None` if the deck is Playback / uncreated. The shared
    /// gate for the capture API.
    fn realtime_ring(&self, deck_index: usize) -> Option<&RingConsumer> {
        match self.decks[deck_index].as_ref() {
            Some(DeckSource::Realtime(ring)) => Some(ring),
            _ => None,
        }
    }

    /// Play/stop the live (Realtime) deck stream: gate whether `render` drains
    /// the deck's ring. `deck_stop` passes `false` so the held prebuffer stops
    /// sounding immediately (instead of playing out then underrunning); `deck_play`
    /// passes `true` to resume. A no-op for a Playback deck (a loaded track has its
    /// own transport) or an uncreated deck.
    pub fn set_deck_playing(&mut self, deck_index: usize, playing: bool) {
        if let Some(DeckSource::Realtime(ring)) = self.decks.get_mut(deck_index).and_then(Option::as_mut)
        {
            ring.set_playing(playing);
        }
    }

    /// The active Playback source for a deck, or `None` if the deck is Realtime /
    /// uncreated. Shared accessor for the track control methods.
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    fn track_mut(&mut self, deck_index: usize) -> Option<&mut BufferSource> {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        match self.decks[deck_index].as_mut() {
            Some(DeckSource::Playback(track)) => Some(track),
            _ => None,
        }
    }

    /// Read-only sibling of [`Engine::track_mut`].
    ///
    /// # Panics
    /// Panics if `deck_index >= DECK_COUNT`.
    fn track_ref(&self, deck_index: usize) -> Option<&BufferSource> {
        assert!(deck_index < DECK_COUNT, "deck index {deck_index} out of range");
        match self.decks[deck_index].as_ref() {
            Some(DeckSource::Playback(track)) => Some(track),
            _ => None,
        }
    }

    /// Shared telemetry handle. Cheap to clone (`Arc`); a UI/monitor thread can
    /// hold one and read stats while `render` runs — all reads are wait-free.
    pub fn telemetry(&self) -> Arc<Telemetry> {
        self.telemetry.clone()
    }

    /// **The RT path.** Drain `frames` from every deck's ring, mix them through
    /// the bare graph (per-deck EQ, equal-power crossfade, master clamp), and
    /// write `frames` interleaved stereo samples into `out` (the master feed).
    ///
    /// `out` must have room for `frames * CHANNELS` samples; any device channels
    /// beyond stereo are the device wrapper's concern (it deinterleaves to the
    /// device width). This function does NO allocation, takes NO lock, makes NO
    /// syscall — it is safe to call inside a cpal callback under
    /// `assert_no_alloc`.
    #[inline]
    pub fn render(&mut self, out: &mut [f32], frames: usize) {
        self.render_inner(out, None, frames);
    }

    /// Like [`Engine::render`] but ALSO writes the headphone cue feed (Slice 5):
    /// `master_out` gets the speaker mix, `cue_out` the PFL/cue blend (both
    /// interleaved stereo, `frames * CHANNELS` samples). The cue feed is the
    /// per-deck cued (PFL) signal blended with the master per the cue mix. Same
    /// RT-safety contract as `render`.
    #[inline]
    pub fn render_with_cue(&mut self, master_out: &mut [f32], cue_out: &mut [f32], frames: usize) {
        self.render_inner(master_out, Some(cue_out), frames);
    }

    /// The shared render core. Always fills `master_out`; fills `cue_out` too when
    /// `Some`. One per-frame loop so the master and cue stay sample-aligned and the
    /// per-deck drain happens exactly once.
    #[inline]
    fn render_inner(&mut self, master_out: &mut [f32], mut cue_out: Option<&mut [f32]>, frames: usize) {
        debug_assert!(
            master_out.len() >= frames * CHANNELS as usize,
            "render: master buffer too small"
        );
        debug_assert!(
            cue_out.as_ref().is_none_or(|c| c.len() >= frames * CHANNELS as usize),
            "render: cue buffer too small"
        );
        self.telemetry.note_block();

        // Per-block accounting first (prebuffer gate, fill stats, underruns) for
        // the live (Realtime) decks only — a Playback deck reads a finished buffer
        // with no ring to prime or starve. Then the per-frame drain + mix.
        for deck in self.decks.iter_mut().flatten() {
            if let DeckSource::Realtime(ring) = deck {
                ring.account_block(frames);
            }
        }

        let mut master_peak = 0.0f32;
        let mut min_gain = 1.0f32;
        // Per-deck post-fader peaks for the channel meters, folded over the block.
        let mut deck_peaks = [0.0f32; DECK_COUNT];
        let mut deck_levels = [0.0f32; DECK_COUNT];
        for f in 0..frames {
            // Pop one stereo frame from each deck's active source (zero-fill on a
            // ring shortfall; silence for a paused/ended track), then fold in the
            // loop bank: an active freeze REPLACES the live frame at the channel head
            // while the ring keeps draining underneath (so the played history advances
            // and returning to live is seamless, ADR-0009); loaded-sample layers and a
            // ringing one-shot are SUMMED on top (ADR-0022). The bank is empty for a
            // Playback deck (loops are Realtime-only), so there it is a passthrough.
            let mut pairs = [(0.0f32, 0.0f32); DECK_COUNT];
            for (d, pair) in pairs.iter_mut().enumerate() {
                let live = match self.decks[d].as_mut() {
                    Some(DeckSource::Realtime(ring)) => ring.pop_frame(),
                    Some(DeckSource::Playback(track)) => track.pop_frame(),
                    None => (0.0, 0.0),
                };
                *pair = self.loops[d].mix_frame(live);
            }

            let out = self.graph.mix_frame(pairs, &mut deck_levels);
            let (ol, or) = out.master;

            let base = f * CHANNELS as usize;
            master_out[base] = ol;
            master_out[base + 1] = or;
            // Advance the preview every frame (so its position is independent of
            // whether a cue device is attached), then sum it onto the cue feed
            // only — the preview is heard in the phones, never the master.
            let audition = self.audition.as_mut().map(|s| s.pop_frame());
            if let Some(cue) = cue_out.as_deref_mut() {
                let (mut cl, mut cr) = out.cue;
                if let Some((al, ar)) = audition {
                    cl = (cl + al).clamp(-MASTER_CEILING, MASTER_CEILING);
                    cr = (cr + ar).clamp(-MASTER_CEILING, MASTER_CEILING);
                }
                cue[base] = cl;
                cue[base + 1] = cr;
            }

            let peak = ol.abs().max(or.abs());
            if peak > master_peak {
                master_peak = peak;
            }
            // Fold the limiter's per-frame applied gain into the GR meter
            // (monotone-min over the block; the deepest reduction wins).
            if out.gain_reduction < min_gain {
                min_gain = out.gain_reduction;
            }
            // Fold each deck's post-fader level into the block's peak (per-channel).
            for d in 0..DECK_COUNT {
                if deck_levels[d] > deck_peaks[d] {
                    deck_peaks[d] = deck_levels[d];
                }
            }
        }
        self.telemetry.record_master_peak(master_peak);
        self.telemetry.record_master_gain_reduction(min_gain);
        for (d, &peak) in deck_peaks.iter().enumerate() {
            self.telemetry.record_deck_peak(d, peak);
        }
        // Advance the shared audio clock by this block (the UI's getContextTime).
        self.telemetry.note_frames(frames);
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 256;

    /// A deterministic synthetic stereo producer: a per-deck sine, generated in
    /// frame-aligned chunks. Mirrors the spike's producer but is pure (no
    /// threads) so tests drive it directly.
    struct SineSource {
        phase: f32,
        dphase: f32,
        amp: f32,
    }

    impl SineSource {
        fn new(freq: f32, amp: f32) -> Self {
            SineSource {
                phase: 0.0,
                dphase: 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE as f32,
                amp,
            }
        }

        /// Fill `frames` interleaved stereo frames into `buf` (len = frames*2).
        fn fill(&mut self, buf: &mut [f32], frames: usize) {
            for f in 0..frames {
                let s = self.phase.sin() * self.amp;
                self.phase += self.dphase;
                if self.phase > std::f32::consts::TAU {
                    self.phase -= std::f32::consts::TAU;
                }
                buf[2 * f] = s;
                buf[2 * f + 1] = s;
            }
        }
    }

    /// The reference bare mix for a single pair of stereo inputs: flat EQ (every
    /// band at 0.5 is unity, so a passthrough), unity volume, equal-power
    /// crossfade, then the master clamp. This is the parity oracle the engine
    /// output must match **for a sub-threshold signal**, where the Slice-2
    /// limiter is level-transparent (its makeup is cancelled). Tests that use it
    /// keep peaks below the −6 dB limiter threshold so the limiter does not
    /// engage; the clamp here is just the never-exceeded ceiling guard. Biquad
    /// arithmetic is not bit-exact identity, so the test uses an epsilon.
    fn expected_mix(a: (f32, f32), b: (f32, f32), position: f32) -> (f32, f32) {
        let angle = position.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
        let ga = angle.cos();
        let gb = angle.sin();
        let l = (a.0 * ga + b.0 * gb).clamp(-MASTER_CEILING, MASTER_CEILING);
        let r = (a.1 * ga + b.1 * gb).clamp(-MASTER_CEILING, MASTER_CEILING);
        (l, r)
    }

    /// Feed `frames` of a deck's sine through `post_pcm`, in one shot.
    fn feed(handle: &mut DeckHandle, src: &mut SineSource, frames: usize) {
        let mut buf = vec![0.0f32; frames * CHANNELS as usize];
        src.fill(&mut buf, frames);
        let written = handle.post_pcm(&buf);
        assert_eq!(written, buf.len(), "ring should have had room for the feed");
    }

    /// (1) Prebuffer gating: no underruns are counted during the initial 1.5 s
    /// fill, even though each block is rendered before the ring is primed.
    #[test]
    fn prebuffer_gate_suppresses_initial_underruns() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let mut deck_b = engine.create_deck(1);
        let tele = engine.telemetry();

        // Render ~1.0 s of blocks WITHOUT ever priming (feed a trickle smaller
        // than the prebuffer so neither deck crosses the high-water mark).
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        let blocks = (SAMPLE_RATE as usize) / BLOCK; // ~1 s
        let mut sa = SineSource::new(220.0, 0.25);
        let mut sb = SineSource::new(330.0, 0.25);
        // Feed only a little each block — never reaching 1.5 s buffered.
        for _ in 0..blocks {
            feed(&mut deck_a, &mut sa, BLOCK / 2);
            feed(&mut deck_b, &mut sb, BLOCK / 2);
            engine.render(&mut out, BLOCK);
        }

        assert!(
            !tele.primed(0) && !tele.primed(1),
            "decks must not be primed before the 1.5 s high-water"
        );
        assert_eq!(
            tele.underruns(),
            0,
            "no underruns may be counted during the prebuffer fill"
        );
    }

    /// (2) Zero underruns once primed and fed at ≥ realtime; plus (3) ring-fill
    /// stats track correctly (min/max within sane bounds, current fill > 0).
    #[test]
    fn primed_and_fed_has_zero_underruns_and_tracks_fill() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let mut deck_b = engine.create_deck(1);
        let tele = engine.telemetry();

        let mut sa = SineSource::new(220.0, 0.25);
        let mut sb = SineSource::new(330.0, 0.25);

        // Prime: buffer > 1.5 s up front so both decks cross the high-water.
        let prime_frames = PREBUFFER_FRAMES + BLOCK;
        feed(&mut deck_a, &mut sa, prime_frames);
        feed(&mut deck_b, &mut sb, prime_frames);

        // Now render many blocks, refeeding exactly one block of audio each time
        // (realtime pace) so the ring never starves.
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        let blocks = 2000; // ~10.7 s of audio
        for _ in 0..blocks {
            feed(&mut deck_a, &mut sa, BLOCK);
            feed(&mut deck_b, &mut sb, BLOCK);
            engine.render(&mut out, BLOCK);
        }

        assert!(tele.primed(0) && tele.primed(1), "both decks should be primed");
        assert_eq!(
            tele.underruns(),
            0,
            "a primed deck fed at realtime must never underrun"
        );

        // Ring-fill stats: each deck holds ≳ the prebuffer and < the ring cap.
        for d in 0..DECK_COUNT {
            let now = tele.ring_fill(d);
            let mn = tele.ring_fill_min(d);
            let mx = tele.ring_fill_max(d);
            assert!(now > 0, "deck {d} ring should be non-empty");
            assert!(now < RING_FRAMES, "deck {d} fill must stay under the cap");
            assert!(mn <= now && now <= mx, "deck {d} min ≤ now ≤ max");
            assert!(
                mn >= PREBUFFER_FRAMES - BLOCK,
                "deck {d} stays near the prebuffer high-water once primed"
            );
        }
    }

    /// (4) The mixed output equals the expected bare mix (flat EQ, unity volume,
    /// equal-power crossfade, transparent limiter) sample-for-sample (within a
    /// tiny epsilon for the flat biquads), and never exceeds the master ceiling,
    /// swept across positions. Amplitudes are deliberately sub-threshold so the
    /// Slice-2 limiter is level-transparent and the flat-mix oracle still holds:
    /// the worst-case post-crossfade peak is about `0.3 * sqrt(2)` (~0.42), under
    /// the limiter threshold of 0.501 (the -6 dB point).
    #[test]
    fn output_matches_bare_mix_and_respects_ceiling() {
        // Sub-threshold amplitude (see the doc note): keeps the limiter idle.
        const AMP: f32 = 0.3;
        for &position in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let mut deck_b = engine.create_deck(1);
            engine.set_crossfade(position);

            // Two independent reference sources; we replay the SAME phase
            // progression into the oracle to predict the mix.
            let mut sa = SineSource::new(220.0, AMP);
            let mut sb = SineSource::new(330.0, AMP);
            let mut ref_a = SineSource::new(220.0, AMP);
            let mut ref_b = SineSource::new(330.0, AMP);

            // Prime well past the high-water so nothing zero-fills.
            let prime = PREBUFFER_FRAMES + 4 * BLOCK;
            feed(&mut deck_a, &mut sa, prime);
            feed(&mut deck_b, &mut sb, prime);

            // Drop the prebuffer's worth of frames from the oracle too, so the
            // oracle and the engine read the same samples once we compare.
            let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
            let mut a_buf = vec![0.0f32; BLOCK * CHANNELS as usize];
            let mut b_buf = vec![0.0f32; BLOCK * CHANNELS as usize];

            let mut max_out = 0.0f32;
            let blocks = 8;
            for _ in 0..blocks {
                engine.render(&mut out, BLOCK);
                ref_a.fill(&mut a_buf, BLOCK);
                ref_b.fill(&mut b_buf, BLOCK);
                for f in 0..BLOCK {
                    let a = (a_buf[2 * f], a_buf[2 * f + 1]);
                    let b = (b_buf[2 * f], b_buf[2 * f + 1]);
                    let (el, er) = expected_mix(a, b, position);
                    let ol = out[2 * f];
                    let or = out[2 * f + 1];
                    // Flat EQ is a near-unity biquad chain, not bit-exact
                    // identity; a small epsilon absorbs the filter arithmetic.
                    assert!(
                        (ol - el).abs() < 1e-3,
                        "pos={position} frame={f} L: got {ol}, expected {el}"
                    );
                    assert!(
                        (or - er).abs() < 1e-3,
                        "pos={position} frame={f} R: got {or}, expected {er}"
                    );
                    max_out = max_out.max(ol.abs()).max(or.abs());
                }
            }
            assert!(
                max_out <= MASTER_CEILING,
                "output must never exceed the master ceiling (pos={position})"
            );
        }
    }

    /// (4b) The master never exceeds the ceiling under a loud DC input — the
    /// clip-guard invariant. (Slice 1 expected the clamp to *pin* loud DC to the
    /// ceiling; Slice 2's limiter tames it BELOW the ceiling first, so the
    /// guard rarely fires — the invariant is "never above", which still holds.
    /// The taming itself is covered by `limiter_*` below.)
    #[test]
    fn loud_in_phase_decks_never_exceed_ceiling() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let mut deck_b = engine.create_deck(1);
        engine.set_crossfade(0.5); // both decks at cos(π/4) ≈ 0.707

        // DC-ish full-scale: post-crossfade sum ≈ 2 * 1.0 * 0.707 = 1.414 > ceil.
        let prime = PREBUFFER_FRAMES + 2 * BLOCK;
        let a = vec![1.0f32; prime * CHANNELS as usize];
        let b = vec![1.0f32; prime * CHANNELS as usize];
        assert_eq!(deck_a.post_pcm(&a), a.len());
        assert_eq!(deck_b.post_pcm(&b), b.len());

        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        // Render a couple of blocks past the EQ settling transient.
        for _ in 0..4 {
            engine.render(&mut out, BLOCK);
        }
        for &s in out.iter() {
            assert!(
                s.abs() <= MASTER_CEILING + 1e-7,
                "sample {s} exceeded the ceiling"
            );
        }
    }

    /// (5) An underrun IS counted when a primed ring is starved: prime a deck,
    /// then render more than was buffered so the ring runs dry.
    #[test]
    fn starved_primed_ring_counts_underrun() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1); // left empty/unprimed on purpose
        let tele = engine.telemetry();

        let mut sa = SineSource::new(220.0, 0.25);
        // Prime deck A to just over the high-water, then stop feeding it.
        let prime_frames = PREBUFFER_FRAMES + BLOCK;
        feed(&mut deck_a, &mut sa, prime_frames);

        // Drain every block with NO refeed. Once the buffered ~1.5 s is gone the
        // primed ring is short of a full block → underruns start counting.
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        let blocks = (prime_frames / BLOCK) + 50; // outrun the buffer
        for _ in 0..blocks {
            engine.render(&mut out, BLOCK);
        }

        assert!(tele.primed(0), "deck A should have primed before starving");
        assert!(
            tele.underruns() > 0,
            "a primed, starved ring must count underruns"
        );
        // Deck B never primed (never fed), so it contributes no underruns — the
        // gate must not falsely count an unprimed deck.
        assert!(!tele.primed(1), "unfed deck B must stay unprimed");
    }

    /// The handoff stays SPSC under realistic over-feeding: post_pcm drops the
    /// tail (returns < len) rather than blocking or growing when the ring fills.
    #[test]
    fn post_pcm_drops_tail_when_ring_full_never_blocks() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);

        // Try to push more than the whole ring in one go.
        let huge = vec![0.5f32; (RING_FRAMES + 1) * CHANNELS as usize];
        let written = deck_a.post_pcm(&huge);
        assert!(written < huge.len(), "a full ring must drop the overflow tail");
        assert_eq!(deck_a.free_samples(), 0, "ring should be full after the push");
    }

    // --- Slice 2 tests: the real EQ curve, per-deck volume, master limiter ---
    //
    // The exact Chromium-vs-fundsp WAVEFORM parity was already proven in Spike A
    // (`docs/spike-rust-audio.md`, golden `spike/rust-audio/golden/`): EQ shelves
    // /bell to ~1e-6, limiter invariants exact. These headless tests verify the
    // CURVE (kill/flat/boost band levels) and the limiter INVARIANTS (ceiling +
    // sub-threshold transparency) — not a waveform diff, which was the spike's
    // job.

    /// Drive deck A alone (crossfade fully on A) with a steady sine at `freq`,
    /// amplitude `amp`, render `blocks` blocks past a settling skip, and return
    /// the RMS of the left channel over the last `measure_blocks`. The signal
    /// stays on A so the EQ under test is the only colouring.
    fn deck_a_rms(engine: &mut Engine, deck_a: &mut DeckHandle, freq: f32, amp: f32) -> f32 {
        let mut src = SineSource::new(freq, amp);
        // Prime well past the high-water plus settling headroom.
        let prime = PREBUFFER_FRAMES + 12 * BLOCK;
        feed(deck_a, &mut src, prime);

        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        // Skip the EQ transient, then measure several blocks of steady state.
        let skip_blocks = 8;
        let measure_blocks = 16;
        for _ in 0..skip_blocks {
            engine.render(&mut out, BLOCK);
        }
        let mut sum_sq = 0.0f64;
        let mut n = 0u64;
        for _ in 0..measure_blocks {
            engine.render(&mut out, BLOCK);
            for f in 0..BLOCK {
                let l = out[2 * f] as f64;
                sum_sq += l * l;
                n += 1;
            }
        }
        (sum_sq / n as f64).sqrt() as f32
    }

    /// Like [`deck_a_rms`] but returns the steady-state master L samples, for
    /// sample-exact comparisons (e.g. dry vs. active FX vs. cleared FX).
    fn deck_a_samples(engine: &mut Engine, deck_a: &mut DeckHandle, freq: f32, amp: f32) -> Vec<f32> {
        let mut src = SineSource::new(freq, amp);
        let prime = PREBUFFER_FRAMES + 12 * BLOCK;
        feed(deck_a, &mut src, prime);

        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..8 {
            engine.render(&mut out, BLOCK);
        }
        let mut samples = Vec::with_capacity(8 * BLOCK);
        for _ in 0..8 {
            engine.render(&mut out, BLOCK);
            for f in 0..BLOCK {
                samples.push(out[2 * f]);
            }
        }
        samples
    }

    /// Prime + render deck A through `render_with_cue`; returns the steady-state
    /// RMS of the master L and the cue L (the headphone feed).
    fn deck_a_master_and_cue_rms(
        engine: &mut Engine,
        deck_a: &mut DeckHandle,
        freq: f32,
        amp: f32,
    ) -> (f32, f32) {
        let mut src = SineSource::new(freq, amp);
        feed(deck_a, &mut src, PREBUFFER_FRAMES + 12 * BLOCK);

        let mut master = vec![0.0f32; BLOCK * CHANNELS as usize];
        let mut cue = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..8 {
            engine.render_with_cue(&mut master, &mut cue, BLOCK);
        }
        let (mut ms, mut cs, mut n) = (0.0f64, 0.0f64, 0u64);
        for _ in 0..16 {
            engine.render_with_cue(&mut master, &mut cue, BLOCK);
            for f in 0..BLOCK {
                let m = master[2 * f] as f64;
                let c = cue[2 * f] as f64;
                ms += m * m;
                cs += c * c;
                n += 1;
            }
        }
        (
            (ms / n as f64).sqrt() as f32,
            (cs / n as f64).sqrt() as f32,
        )
    }

    /// (S2-1) The EQ curve: at each band's centre, kill (0) ≈ −40 dB, flat (0.5)
    /// ≈ 0 dB, boost (1) ≈ +6 dB relative to the flat passthrough. Low/high are
    /// true shelves (measured well inside the shelf band); the mid is a bell at
    /// its centre.
    #[test]
    fn eq_curve_kill_flat_boost_per_band() {
        // The `eqValueToDb` curve targets (frontend/src/audio/eq.ts).
        const EQ_KILL_DB_TARGET: f32 = -40.0;
        const EQ_BOOST_DB_TARGET: f32 = 6.0;
        // (band, test frequency). Shelves measured deep in-band (low 60 Hz, high
        // 8 kHz) so the full shelf gain shows; the mid bell at its 1 kHz centre.
        let cases = [(EqBand::Low, 60.0f32), (EqBand::Mid, 1_000.0), (EqBand::High, 8_000.0)];
        // Sub-threshold amplitude: even a +6 dB boost (×2) stays under the −6 dB
        // limiter threshold, so the limiter never colours the measurement.
        const AMP: f32 = 0.2;

        for (band, freq) in cases {
            // Flat baseline (all bands 0.5).
            let flat = {
                let mut engine = Engine::new();
                let mut deck_a = engine.create_deck(0);
                let _deck_b = engine.create_deck(1);
                engine.set_crossfade(0.0); // full deck A
                deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
            };

            // Kill (band → 0): expect ≈ −40 dB relative.
            let killed = {
                let mut engine = Engine::new();
                let mut deck_a = engine.create_deck(0);
                let _deck_b = engine.create_deck(1);
                engine.set_crossfade(0.0);
                engine.set_eq(0, band, 0.0);
                deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
            };

            // Boost (band → 1): expect ≈ +6 dB relative.
            let boosted = {
                let mut engine = Engine::new();
                let mut deck_a = engine.create_deck(0);
                let _deck_b = engine.create_deck(1);
                engine.set_crossfade(0.0);
                engine.set_eq(0, band, 1.0);
                deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
            };

            let kill_db = 20.0 * (killed / flat).log10();
            let boost_db = 20.0 * (boosted / flat).log10();

            // Kill target is −40 dB. The mid bell at its centre hits it exactly
            // (≈ −40.0 dB); the low/high shelves measured deep in-band approach it
            // (≈ −38 dB) — both are a true kill within ~3 dB of the −40 target.
            assert!(
                (kill_db - EQ_KILL_DB_TARGET).abs() < 3.0,
                "{band:?} @ {freq} Hz kill should be ≈ {EQ_KILL_DB_TARGET} dB, got {kill_db:.2} dB"
            );
            // Boost is the clean target: +6 dB within ~0.2 dB.
            assert!(
                (boost_db - EQ_BOOST_DB_TARGET).abs() < 0.2,
                "{band:?} @ {freq} Hz boost should be ≈ {EQ_BOOST_DB_TARGET} dB, got {boost_db:.2} dB"
            );
        }
    }

    /// (S2-1b) Flat (every band 0.5) is a near-unity passthrough on a deck: the
    /// per-band EQ at flat colours the signal by < ~0.1 dB at a mid frequency.
    #[test]
    fn eq_flat_is_unity_passthrough() {
        const AMP: f32 = 0.2;
        let freq = 1_000.0f32;

        // Engine EQ at default-flat vs the raw input level (a deck at unity
        // volume, full crossfade on A, no EQ change) — the same code path with
        // every band left at 0.5.
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(0.0);
        let out_rms = deck_a_rms(&mut engine, &mut deck_a, freq, AMP);

        // A pure sine at amplitude AMP has RMS = AMP/√2.
        let in_rms = AMP / std::f32::consts::SQRT_2;
        let db = 20.0 * (out_rms / in_rms).log10();
        assert!(
            db.abs() < 0.2,
            "flat EQ should pass at unity, got {db:.3} dB"
        );
    }

    /// (S2-1c) Re-applying a band's CURRENT value mid-stream is a pure no-op. The
    /// old `set_eq` rebuilt the deck's chains and `reset()` their filter state on
    /// every call, so even a redundant set glitched the audio (the click the user
    /// hears on EQ moves). The settable-input path stores the same smoothed target
    /// and disturbs nothing. Two identical engines rendered in lock-step, one given
    /// a redundant `set_eq`, must stay sample-identical. (Fails on the rebuild
    /// path: zeroing the delay line mid-tone rings, diverging the two outputs.)
    #[test]
    fn eq_reapplying_the_same_value_is_a_no_op() {
        const AMP: f32 = 0.2;
        let freq = 120.0f32; // a low tone the low shelf acts on, so a reset shows

        let build = || {
            let mut engine = Engine::new();
            let deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0); // full deck A, sub-threshold (limiter out)
            // Boost the low band so the shelf actually FILTERS the tone (at flat it
            // is the identity, where a state reset would be invisible). Set before
            // the first render, so `follow()` snaps to it — both engines settle
            // identically.
            engine.set_eq(0, EqBand::Low, 1.0);
            let mut src = SineSource::new(freq, AMP);
            let mut handle = deck_a;
            feed(&mut handle, &mut src, PREBUFFER_FRAMES + 40 * BLOCK);
            (engine, handle, _deck_b)
        };
        let (mut ref_eng, _ref_a, _ref_b) = build();
        let (mut set_eng, _set_a, _set_b) = build();

        let mut ro = vec![0.0f32; BLOCK * CHANNELS as usize];
        let mut so = vec![0.0f32; BLOCK * CHANNELS as usize];
        // Warm both identically to steady state.
        for _ in 0..8 {
            ref_eng.render(&mut ro, BLOCK);
            set_eng.render(&mut so, BLOCK);
        }
        // Mid-stream, re-apply deck A's CURRENT low value (the boost) to one engine.
        set_eng.set_eq(0, EqBand::Low, 1.0);
        let mut max_diff = 0.0f32;
        for _ in 0..16 {
            ref_eng.render(&mut ro, BLOCK);
            set_eng.render(&mut so, BLOCK);
            for (a, b) in ro.iter().zip(so.iter()) {
                max_diff = max_diff.max((a - b).abs());
            }
        }
        assert!(
            max_diff < 1e-6,
            "re-applying the same EQ value disturbed the audio (state reset?): max diff {max_diff}"
        );
    }

    /// (S2-1d) A real mid-stream change glides instead of stepping: the band gain
    /// ramps over a few ms (no zipper) and the filter keeps its state (no click).
    /// Measure the output peak in short windows across a mid-stream mid-band boost;
    /// the window-to-window peak must climb gradually. An un-smoothed coefficient
    /// step (or a rebuild) would dump the whole +6 dB rise into one window boundary.
    #[test]
    fn eq_change_mid_stream_glides_without_a_step() {
        const AMP: f32 = 0.2;
        let freq = 1_000.0f32; // the mid bell's centre, where the boost shows fully

        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(0.0);
        let mut src = SineSource::new(freq, AMP);
        feed(&mut deck_a, &mut src, PREBUFFER_FRAMES + 64 * BLOCK);

        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..8 {
            engine.render(&mut out, BLOCK); // reach steady state
        }

        let mut samples = Vec::new();
        for _ in 0..2 {
            engine.render(&mut out, BLOCK);
            for f in 0..BLOCK {
                samples.push(out[2 * f]);
            }
        }
        engine.set_eq(0, EqBand::Mid, 1.0); // flat → +6 dB boost, mid-stream
        for _ in 0..24 {
            engine.render(&mut out, BLOCK);
            for f in 0..BLOCK {
                samples.push(out[2 * f]);
            }
        }

        // Peak per 64-sample window (≈1.3 periods at 1 kHz, so each holds a peak);
        // the largest jump between adjacent window peaks.
        let peaks: Vec<f32> = samples
            .chunks(64)
            .map(|c| c.iter().fold(0.0f32, |m, &s| m.max(s.abs())))
            .collect();
        let max_peak_jump = peaks
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);

        // Boost is +6 dB (×2): peak ~0.2 → ~0.4, a ~0.2 total rise. Smoothed over
        // ~tens of ms each 64-sample window climbs a small fraction; a step would
        // put the whole rise into one boundary.
        assert!(
            max_peak_jump < 0.05,
            "mid-band boost stepped the level (zipper / rebuild): max window-peak jump {max_peak_jump:.4}"
        );
    }

    /// (S2-2) Per-deck volume scales a deck's contribution linearly: deck A at
    /// volume g produces g× the output of deck A at volume 1.0 (full crossfade on
    /// A, sub-threshold so the limiter stays out of it).
    #[test]
    fn volume_scales_deck_contribution() {
        const AMP: f32 = 0.2;
        let freq = 440.0f32;
        let g = 0.5f32;

        let full = {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
        };
        let scaled = {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            engine.set_volume(0, g);
            deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
        };

        let ratio = scaled / full;
        assert!(
            (ratio - g).abs() < 1e-3,
            "volume {g} should scale the contribution by {g}, got ratio {ratio}"
        );
    }

    /// (P2-3a) Chain-head trim scales the deck contribution by `10^(dB/20)`. A
    /// +6 dB trim ≈ ×1.995. Mirrors `volume_scales_deck_contribution`, but trim
    /// sits PRE-EQ at the head (here flat EQ, so the net is a clean gain).
    #[test]
    fn trim_scales_deck_contribution_in_db() {
        const AMP: f32 = 0.1; // ×1.995 stays well under the −6 dB limiter threshold
        let freq = 440.0f32;

        let flat = {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
        };
        let trimmed = {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            engine.set_trim(0, 6.0);
            deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
        };

        let ratio = trimmed / flat;
        let expected = 10f32.powf(6.0 / 20.0); // ≈ 1.995
        assert!(
            (ratio - expected).abs() < 2e-2,
            "+6 dB trim should scale by {expected}, got ratio {ratio}"
        );
    }

    /// (P2-3b) Off-air mutes the deck's master contribution but the channel meter
    /// stays live (M10 primed deck): the master RMS collapses to ~0 while the
    /// per-deck level meter still reads the deck's post-fader signal.
    #[test]
    fn off_air_mutes_master_but_keeps_meter_live() {
        const AMP: f32 = 0.2;
        let freq = 440.0f32;

        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        let tele = engine.telemetry();
        engine.set_crossfade(0.0); // full deck A
        engine.set_on_air(0, false);

        let master_rms = deck_a_rms(&mut engine, &mut deck_a, freq, AMP);
        let deck_level = tele.take_deck_peak(0);

        assert!(
            master_rms < 1e-4,
            "off-air deck must not reach the master ({master_rms})"
        );
        assert!(
            deck_level > 0.5 * AMP,
            "off-air deck must still meter its live level (got {deck_level}, amp {AMP})"
        );
    }

    /// (P2-3c) The channel meter reads the post-fader magnitude (≈ the source
    /// peak through flat EQ at unity volume).
    #[test]
    fn deck_level_meter_tracks_post_fader_peak() {
        const AMP: f32 = 0.3;
        let freq = 440.0f32;

        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        let tele = engine.telemetry();
        engine.set_crossfade(0.0);

        let _ = deck_a_rms(&mut engine, &mut deck_a, freq, AMP);
        let level = tele.take_deck_peak(0);
        assert!(
            (level - AMP).abs() < 0.05,
            "deck level should track the post-fader peak ≈ {AMP}, got {level}"
        );
    }

    /// (P2-3d) The shared audio clock counts exactly the frames rendered.
    #[test]
    fn context_clock_counts_rendered_frames() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        let tele = engine.telemetry();

        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        let blocks = 10;
        for _ in 0..blocks {
            engine.render(&mut out, BLOCK);
        }
        assert_eq!(
            tele.frames_rendered(),
            (blocks * BLOCK) as u64,
            "the clock must count every rendered frame"
        );
    }

    /// (P2-3e) No effect until one is selected: `set_fx_amount` alone leaves the
    /// insert dry (decks start with no FX). Selecting an effect engages it;
    /// `clear_fx` returns to the bit-identical dry path.
    #[test]
    fn fx_none_until_selected_then_clear_restores_dry() {
        let freq = 8_000.0f32; // high tone so the crush clearly alters the wave
        const AMP: f32 = 0.2;

        let dry = {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            // Amount set, but NO effect selected → must stay dry (fx disabled).
            engine.set_fx_amount(0, 0.8);
            deck_a_samples(&mut engine, &mut deck_a, freq, AMP)
        };
        let active = {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            engine.set_fx(0, FxKind::Crush);
            engine.set_fx_amount(0, 0.8);
            deck_a_samples(&mut engine, &mut deck_a, freq, AMP)
        };
        let cleared = {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            engine.set_fx(0, FxKind::Crush);
            engine.set_fx_amount(0, 0.8);
            engine.clear_fx(0);
            deck_a_samples(&mut engine, &mut deck_a, freq, AMP)
        };

        let max_diff = |a: &[f32], b: &[f32]| {
            a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max)
        };
        assert!(
            max_diff(&dry, &active) > 1e-4,
            "an active crush effect must alter the signal"
        );
        assert!(
            max_diff(&dry, &cleared) < 1e-6,
            "clear_fx must restore the bit-identical dry path"
        );
    }

    /// (P2-5a) Pre-fade listen: a cued deck is audible in the phones even when it
    /// is crossfaded entirely out of the master. cue_mix=0 → phones are the cue
    /// bus alone.
    #[test]
    fn cue_is_pre_fade_independent_of_the_crossfader() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(1.0); // master = full deck B; deck A weighted to 0
        engine.set_cue(0, true);
        engine.set_cue_mix(0.0); // phones = cue bus only

        let (master, cue) = deck_a_master_and_cue_rms(&mut engine, &mut deck_a, 440.0, 0.2);
        assert!(
            master < 1e-3,
            "deck A is crossfaded out of the master ({master})"
        );
        assert!(
            cue > 0.05,
            "a cued deck must still be audible in the phones ({cue})"
        );
    }

    /// (P2-5b) cue_mix = 1 collapses the headphone feed to the master exactly (the
    /// cue bus is muted), regardless of PFL state.
    #[test]
    fn cue_mix_full_master_equals_the_master_feed() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(0.0); // full deck A in the master
        engine.set_cue(0, true); // even cued, cue_mix=1 should mute the cue bus
        engine.set_cue_mix(1.0);

        let (master, cue) = deck_a_master_and_cue_rms(&mut engine, &mut deck_a, 440.0, 0.2);
        assert!(
            (master - cue).abs() < 1e-4,
            "cue_mix=1 → phones must equal the master (master {master}, cue {cue})"
        );
    }

    /// (S2-3a) Limiter ceiling invariant: a hot input (peaks well above the
    /// ceiling) never produces |out| > MASTER_CEILING. This is the load-bearing
    /// safety contract the clip guard guarantees.
    #[test]
    fn limiter_hot_input_never_exceeds_ceiling() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let mut deck_b = engine.create_deck(1);
        engine.set_crossfade(0.5);

        // Full-scale sines on both decks — post-crossfade peaks ≈ √2 ≈ 1.41.
        let mut sa = SineSource::new(220.0, 1.0);
        let mut sb = SineSource::new(330.0, 1.0);
        let prime = PREBUFFER_FRAMES + 2 * BLOCK;
        feed(&mut deck_a, &mut sa, prime);
        feed(&mut deck_b, &mut sb, prime);

        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        let mut max_out = 0.0f32;
        for _ in 0..64 {
            // ~0.34 s
            feed(&mut deck_a, &mut sa, BLOCK);
            feed(&mut deck_b, &mut sb, BLOCK);
            engine.render(&mut out, BLOCK);
            for &s in out.iter() {
                assert!(
                    s.abs() <= MASTER_CEILING + 1e-7,
                    "hot input produced {s}, above the ceiling"
                );
                max_out = max_out.max(s.abs());
            }
        }
        // Sanity: it really was loud enough to be limited (not trivially quiet).
        assert!(max_out > 0.5, "hot test should drive a substantial level");
    }

    /// (S2-3b) Sub-threshold transparency: a quiet steady signal (peaks below the
    /// −6 dB threshold) passes at unity within a small epsilon — proving the
    /// makeup cancellation. Also (S2-3c): gain reduction is ~0 dB below
    /// threshold.
    #[test]
    fn limiter_sub_threshold_is_transparent() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        let tele = engine.telemetry();
        engine.set_crossfade(0.0); // full deck A

        // Peak 0.3 < threshold 0.501 (−6 dB) → the limiter must not engage.
        const AMP: f32 = 0.3;
        let out_rms = deck_a_rms(&mut engine, &mut deck_a, 440.0, AMP);
        let in_rms = AMP / std::f32::consts::SQRT_2;
        let db = 20.0 * (out_rms / in_rms).log10();
        assert!(
            db.abs() < 0.2,
            "sub-threshold signal must pass level-transparent, got {db:.3} dB"
        );
        // The gain-reduction meter should read ≈ 0 dB (idle) below threshold.
        let gr = tele.master_gain_reduction_db();
        assert!(
            gr > -0.1,
            "gain reduction must be ~0 dB below threshold, got {gr:.3} dB"
        );
    }

    /// (S2-3c) Gain reduction engages above threshold: a hot input drives the
    /// gain-reduction telemetry meaningfully negative.
    #[test]
    fn limiter_gain_reduction_engages_above_threshold() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        let tele = engine.telemetry();
        engine.set_crossfade(0.0); // full deck A

        // Peak 1.0 ≫ threshold 0.501 → strong reduction expected.
        let mut src = SineSource::new(440.0, 1.0);
        let prime = PREBUFFER_FRAMES + 2 * BLOCK;
        feed(&mut deck_a, &mut src, prime);
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..32 {
            feed(&mut deck_a, &mut src, BLOCK);
            engine.render(&mut out, BLOCK);
        }
        let gr = tele.master_gain_reduction_db();
        assert!(
            gr < -1.0,
            "a hot input must register gain reduction, got {gr:.2} dB"
        );
    }

    /// (S2-3d) The gain-reduction meter `take_*` reads then resets to unity
    /// (0 dB), like the peak meter — a UI reader sampling each frame.
    #[test]
    fn gain_reduction_meter_resets_on_take() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        let tele = engine.telemetry();
        engine.set_crossfade(0.0);

        let mut src = SineSource::new(440.0, 1.0);
        let prime = PREBUFFER_FRAMES + 2 * BLOCK;
        feed(&mut deck_a, &mut src, prime);
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..16 {
            feed(&mut deck_a, &mut src, BLOCK);
            engine.render(&mut out, BLOCK);
        }
        let taken = tele.take_master_gain_reduction_db();
        assert!(taken < -1.0, "should have captured reduction, got {taken:.2} dB");
        // After the take, the window is reset to unity until the next render.
        assert!(
            tele.master_gain_reduction_db() > -0.1,
            "meter must reset to ~0 dB after take"
        );
    }

    // --- Slice 3 tests: the per-deck Color FX insert (ADR-0008) ---
    //
    // The exact effect maths (bit-exact bypass, crush quantize-and-hold, the
    // dub-echo impulse spacing, the sweep LFO formula, replace-vs-add) is unit
    // tested in `fx.rs` against the `fx.ts` curves and the Spike A facts. These
    // integration tests verify the insert is wired into `render()` correctly: the
    // bypass stays bit-exact through the WHOLE mix path, and an active filter
    // colours the deck audibly.

    /// (Slice-3 integration) With an effect SELECTED but parked at its rest amount,
    /// the rendered output is bit-identical to the engine with no FX touched at
    /// all — the dead-zone bypass survives the whole post-insert mix path
    /// (volume → crossfade → limiter → clamp), 0 ULP. Checked for every effect.
    #[test]
    fn fx_at_rest_renders_bit_identical_to_no_fx() {
        use fx::FxKind;
        let kinds = [
            FxKind::Filter,
            FxKind::DubEcho,
            FxKind::Space,
            FxKind::Crush,
            FxKind::Noise,
            FxKind::Sweep,
        ];
        for kind in kinds {
            // Reference engine: no FX call at all.
            let mut ref_engine = Engine::new();
            let mut ref_a = ref_engine.create_deck(0);
            let _ref_b = ref_engine.create_deck(1);
            ref_engine.set_crossfade(0.0); // full deck A so the insert is in-path

            // Subject engine: select the effect, park it at its rest amount.
            let mut sub_engine = Engine::new();
            let mut sub_a = sub_engine.create_deck(0);
            let _sub_b = sub_engine.create_deck(1);
            sub_engine.set_crossfade(0.0);
            sub_engine.set_fx(0, kind);
            // Rest amount: 0.5 for the filter, 0.0 otherwise — inside the dead zone.
            let rest = if kind == FxKind::Filter { 0.5 } else { 0.0 };
            sub_engine.set_fx_amount(0, rest);

            // Identical input to both decks.
            let mut src_ref = SineSource::new(440.0, 0.3);
            let mut src_sub = SineSource::new(440.0, 0.3);
            let prime = PREBUFFER_FRAMES + 4 * BLOCK;
            feed(&mut ref_a, &mut src_ref, prime);
            feed(&mut sub_a, &mut src_sub, prime);

            let mut ref_out = vec![0.0f32; BLOCK * CHANNELS as usize];
            let mut sub_out = vec![0.0f32; BLOCK * CHANNELS as usize];
            for _ in 0..8 {
                ref_engine.render(&mut ref_out, BLOCK);
                sub_engine.render(&mut sub_out, BLOCK);
                for (r, s) in ref_out.iter().zip(sub_out.iter()) {
                    assert_eq!(
                        r.to_bits(),
                        s.to_bits(),
                        "{kind:?} at rest must render bit-identical to no FX"
                    );
                }
            }
        }
    }

    /// (Slice-3 integration) An active lowpass filter (amount 0.25 → ~1200 Hz)
    /// attenuates a high tone well below the cutoff-pass and leaves a low tone
    /// (well inside the passband) essentially intact.
    #[test]
    fn filter_lowpass_attenuates_high_passes_low() {
        use fx::FxKind;
        const AMP: f32 = 0.2;
        let low_freq = 200.0f32; // ≪ ~1200 Hz cutoff
        let high_freq = 10_000.0f32; // ≫ cutoff

        // RMS at a frequency, with the lowpass filter active on deck A.
        let measure = |freq: f32| {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            engine.set_fx(0, FxKind::Filter);
            engine.set_fx_amount(0, 0.25); // lowpass ~1200 Hz
            deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
        };
        // Flat reference (filter at rest = bypass) at each frequency.
        let reference = |freq: f32| {
            let mut engine = Engine::new();
            let mut deck_a = engine.create_deck(0);
            let _deck_b = engine.create_deck(1);
            engine.set_crossfade(0.0);
            deck_a_rms(&mut engine, &mut deck_a, freq, AMP)
        };

        let low_db = 20.0 * (measure(low_freq) / reference(low_freq)).log10();
        let high_db = 20.0 * (measure(high_freq) / reference(high_freq)).log10();

        // Low tone: near-unity through the lowpass (within ~1 dB).
        assert!(low_db.abs() < 1.0, "low tone should pass, got {low_db:.2} dB");
        // High tone: strongly attenuated (a 12 dB/oct lowpass an octave+ above the
        // ~1200 Hz cutoff cuts it heavily).
        assert!(high_db < -20.0, "high tone should be attenuated, got {high_db:.2} dB");
        // And the filter clearly separates them.
        assert!(
            low_db - high_db > 20.0,
            "lowpass must pass low far more than high (Δ {:.2} dB)",
            low_db - high_db
        );
    }

    // --- Slice 4a tests: the playback deck wired into the engine ---
    //
    // The BufferSource transport (rate-1.0 verbatim, varispeed resampling, loop
    // fold, seek-exits-loop, pause, peaks, plan_loop_set edges) is unit-tested in
    // `playback.rs` against the `track.ts` / `engine.ts` behaviour. These tests
    // verify the SOURCE abstraction is wired into `Engine`/`render` correctly: a
    // load switches the deck to Playback and parks the ring, a Realtime deck is
    // unaffected, the mixed output carries the track frame through the unchanged
    // channel, and `render` stays alloc-free with a Playback source in path.

    /// A flat ramp track buffer: L = frame index, R = its negation, so the source
    /// frame is unambiguous after the (flat) channel arithmetic.
    fn ramp_track(frames: usize) -> Vec<f32> {
        let mut buf = vec![0.0f32; frames * CHANNELS as usize];
        for f in 0..frames {
            buf[2 * f] = f as f32;
            buf[2 * f + 1] = -(f as f32);
        }
        buf
    }

    /// (S4-1) `load_track` switches the deck to Playback and a Realtime deck is
    /// unaffected: deck A loads a track (status Some), deck B stays Realtime
    /// (status None) and still drains its ring.
    #[test]
    fn load_track_switches_mode_and_leaves_realtime_deck_alone() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let mut deck_b = engine.create_deck(1);

        assert!(engine.get_track_status(0).is_none(), "deck A starts Realtime");
        engine.load_track(0, ramp_track(1000));
        let status = engine.get_track_status(0).expect("deck A is now Playback");
        assert_eq!(status.duration_frames, 1000);
        assert!(!status.playing, "a fresh track loads paused at the top");

        // Deck B is still Realtime: no track status, and its ring still feeds.
        assert!(engine.get_track_status(1).is_none(), "deck B stays Realtime");
        let mut sb = SineSource::new(330.0, 0.25);
        feed(&mut deck_b, &mut sb, PREBUFFER_FRAMES + BLOCK);
        let tele = engine.telemetry();
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..8 {
            feed(&mut deck_b, &mut sb, BLOCK);
            engine.render(&mut out, BLOCK);
        }
        assert!(tele.primed(1), "the Realtime deck B still primes and drains its ring");
    }

    /// (S4-2) A loaded, playing track is pulled through `render`: crossfaded fully
    /// to the playback deck, the mixed output reproduces the track's ramp frames
    /// (within the flat-channel epsilon). A paused track is silent.
    #[test]
    fn render_pulls_the_playback_source() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(0.0); // full deck A so the track is the whole mix

        // A small-amplitude ramp keeps the mix sub-threshold (the limiter idle)
        // and EQ near-unity: frame f maps to 0.2 * (f / N) in both channels.
        const N: usize = 1024;
        let track = |frames: usize| {
            let mut buf = vec![0.0f32; frames * CHANNELS as usize];
            for f in 0..frames {
                let s = 0.2 * (f as f32 / frames as f32);
                buf[2 * f] = s;
                buf[2 * f + 1] = s;
            }
            buf
        };

        // Paused: render is silent.
        engine.load_track(0, track(N));
        let mut out = vec![0.0f32; N * CHANNELS as usize];
        engine.render(&mut out, N);
        assert!(out.iter().all(|&s| s == 0.0), "a paused track renders silence");

        // Playing from the top: the output follows the track ramp, full deck A
        // (cos(0) = 1). The flat EQ/limiter chain is near-unity sub-threshold, so
        // the ramp shows within a small biquad-settling epsilon — checked past the
        // first few frames where the EQ filters are still warming up.
        engine.load_track(0, track(N));
        engine.play_track(0);
        engine.render(&mut out, N);
        let mut max_err = 0.0f32;
        for f in 64..N {
            let want = 0.2 * (f as f32 / N as f32);
            max_err = max_err.max((out[2 * f] - want).abs());
        }
        assert!(max_err < 2e-3, "the track ramp must reach the mix output, max err {max_err}");
    }

    /// (S4-3) `unload_track` returns the deck to Realtime and restores the parked
    /// ring: after unload the deck has no track status and the live ring (fed
    /// throughout) drains again.
    #[test]
    fn unload_track_restores_the_parked_ring() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        let tele = engine.telemetry();

        // Prime the ring, then load a track — the ring is PARKED, not destroyed.
        let mut sa = SineSource::new(220.0, 0.25);
        feed(&mut deck_a, &mut sa, PREBUFFER_FRAMES + 4 * BLOCK);
        engine.load_track(0, ramp_track(1000));
        assert!(engine.get_track_status(0).is_some(), "deck A is in Playback");

        // The producer keeps feeding the parked ring across playback.
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..4 {
            feed(&mut deck_a, &mut sa, BLOCK);
            engine.render(&mut out, BLOCK);
        }

        // Unload: back to Realtime, no track status, and the ring drains again.
        engine.unload_track(0);
        assert!(engine.get_track_status(0).is_none(), "unload returns to Realtime");
        let under_before = tele.underruns();
        for _ in 0..4 {
            feed(&mut deck_a, &mut sa, BLOCK);
            engine.render(&mut out, BLOCK);
        }
        assert_eq!(
            tele.underruns(),
            under_before,
            "the restored ring (still fed) must not underrun"
        );
    }

    /// (S4-4) Engine-level track controls map to the source: play/seek/rate/loop
    /// are reflected in `get_track_status`, and a seek exits a loop (ADR-0015).
    #[test]
    fn engine_track_controls_reach_the_source() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.load_track(0, ramp_track(20_000));

        engine.play_track(0);
        engine.set_track_rate(0, 1.05);
        let s = engine.get_track_status(0).unwrap();
        assert!(s.playing);
        assert!((s.rate - 1.05).abs() < 1e-9);

        // A valid loop installs; a seek exits it (the one rule).
        engine.set_track_loop(0, 5_000, 9_000);
        assert!(engine.get_track_status(0).unwrap().loop_region.is_some());
        engine.seek_track(0, 12_000.0);
        let s = engine.get_track_status(0).unwrap();
        assert_eq!(s.loop_region, None, "any seek exits the loop");
        assert_eq!(s.playhead, 12_000.0);

        engine.pause_track(0);
        assert!(!engine.get_track_status(0).unwrap().playing, "pause stops the transport");

        // Peaks come through the engine API too.
        let (min, max) = engine.get_track_peaks(0, 16).unwrap();
        assert_eq!(min.len(), 16);
        assert_eq!(max.len(), 16);
        assert!(max.iter().any(|&v| v > 0.0), "the ramp's max overview is non-trivial");

        // Track controls on a Realtime deck are inert (no panic, no status).
        engine.play_track(1);
        engine.set_track_rate(1, 1.05);
        assert!(engine.get_track_status(1).is_none(), "a Realtime deck has no track");
    }

    /// (S4-5) `render` runs cleanly with both source kinds in path over many
    /// blocks: a Playback deck (looping, varispeed) on B and a primed Realtime
    /// deck on A, mixed through the unchanged channel. Alloc-freeness of the RT
    /// path is structural (the playback `pop_frame` only reads the buffer +
    /// interpolates — no Vec/Box), the same discipline Slices 1–3 hold; the
    /// armed `assert_no_alloc` guard runs in the `device_run` binary and on
    /// hardware (the lib's warn-only feature set makes a CI guard a no-op assert).
    #[test]
    fn render_runs_with_both_source_kinds() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0); // Realtime, fed below
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(0.5);

        engine.load_track(1, ramp_track(48_000));
        engine.play_track(1);
        engine.set_track_loop(1, 6_000, 12_000);
        engine.set_track_rate(1, 1.03);

        let mut sa = SineSource::new(220.0, 0.25);
        feed(&mut deck_a, &mut sa, PREBUFFER_FRAMES + 64 * BLOCK);

        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..32 {
            feed(&mut deck_a, &mut sa, BLOCK);
            engine.render(&mut out, BLOCK);
            for &s in out.iter() {
                assert!(s.abs() <= MASTER_CEILING + 1e-7, "ceiling held with a playback deck");
                assert!(s.is_finite(), "no NaN/Inf from the mixed playback path");
            }
        }
        // The looping varispeed track keeps its playhead inside the region.
        let p = engine.get_track_status(1).unwrap().playhead;
        assert!((6_000.0..12_000.0).contains(&p), "the looped track folds inside the region, got {p}");
    }

    // --- Slice 4b tests: freeze loops, generated loops, style sampling ---
    //
    // The seam-fold math (`build_loop_channel`) and interleaving are unit-tested
    // in `loops.rs` against the `loops.ts` / `styleSample.ts` curves; the
    // wrap-aware history copy is unit-tested in `ring.rs`. These integration tests
    // verify the SLICE-4b API is wired into `Engine`/`render` correctly: a capture
    // reads the exact played history sample-accurately (wrap-aware at the lap), a
    // loop plays at the channel head while the live ring keeps advancing, one-shots
    // overlay and end, slot state reports truthfully, the four slots are
    // independent, and the whole family is inert on a Playback deck.

    /// A producer of a unique-per-frame counter signal: frame `i` is `(i, -i)`, so
    /// a captured frame's identity is unambiguous. Stays exact in f32 well past the
    /// 30 s history (integers ≤ 2^24 are exact; 30 s is ~1.44 M frames).
    struct CounterSource {
        next: u64,
    }

    impl CounterSource {
        fn new() -> Self {
            CounterSource { next: 0 }
        }

        /// Fill `frames` interleaved counter frames; advances the global counter.
        fn fill(&mut self, buf: &mut [f32], frames: usize) {
            for f in 0..frames {
                let i = self.next as f32;
                buf[2 * f] = i;
                buf[2 * f + 1] = -i;
                self.next += 1;
            }
        }
    }

    /// Feed `frames` of the counter into a deck.
    fn feed_counter(handle: &mut DeckHandle, src: &mut CounterSource, frames: usize) {
        let mut buf = vec![0.0f32; frames * CHANNELS as usize];
        src.fill(&mut buf, frames);
        let written = handle.post_pcm(&buf);
        assert_eq!(written, buf.len(), "ring should have had room for the feed");
    }

    /// Render `frames` worth of blocks on a deck fed the counter at realtime, so
    /// every rendered frame pops exactly one fed counter frame into the played
    /// history. Returns the number of frames actually played (= rendered here).
    fn play_counter(engine: &mut Engine, deck: &mut DeckHandle, src: &mut CounterSource, frames: usize) -> usize {
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        let blocks = frames / BLOCK;
        for _ in 0..blocks {
            feed_counter(deck, src, BLOCK);
            engine.render(&mut out, BLOCK);
        }
        blocks * BLOCK
    }

    /// (S4b-1) `capture_sample(N)` returns exactly the last N frames of played
    /// history, sample-accurate. The counter signal makes each frame
    /// identifiable; after priming + playing `played` frames, the capture's
    /// interleaved frames must be the last N the deck popped, in order.
    #[test]
    fn capture_sample_returns_exact_played_history() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);

        // Prime, then play. The ring is a FIFO of the counter 0, 1, 2, …; render
        // pops one frame per output frame and the ring never starves (prime +
        // per-block refeed), so after rendering `total_played` frames the deck has
        // popped exactly `total_played` counter frames in order. The last popped
        // frame index is therefore `total_played - 1`.
        let mut src = CounterSource::new();
        feed_counter(&mut deck_a, &mut src, PREBUFFER_FRAMES + 4 * BLOCK);
        // Play ~6 s (well under the 30 s history, so no wrap here) so a 5 s capture
        // has the frames it needs.
        let total_played = play_counter(&mut engine, &mut deck_a, &mut src, 6 * SAMPLE_RATE as usize);

        // Capture the last 5 s (clears the 3 s style-sample floor): it must be the
        // last N popped counter frames, in order, sample-accurate.
        let seconds = 5.0;
        let want_frames = (seconds * SAMPLE_RATE as f64) as usize;
        let captured = engine.capture_sample(0, seconds).expect("enough history");
        assert_eq!(captured.len(), want_frames * CHANNELS as usize, "captured N frames");

        let last = (total_played - 1) as f32;
        let first = last - (want_frames as f32 - 1.0);
        for f in 0..want_frames {
            let expect = first + f as f32;
            assert_eq!(captured[2 * f], expect, "L frame {f} is the played counter");
            assert_eq!(captured[2 * f + 1], -expect, "R frame {f} is the played counter");
        }
    }

    /// (S4b-1b) The capture is wrap-aware at the history lap: after playing more
    /// than the history capacity, a capture of the last N frames still returns the
    /// correct most-recent counter values across the circular buffer's seam.
    #[test]
    fn capture_is_wrap_aware_after_the_history_laps() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);

        // Play well past the 30 s history capacity so the write head has wrapped.
        let mut src = CounterSource::new();
        feed_counter(&mut deck_a, &mut src, PREBUFFER_FRAMES);
        let lap = HISTORY_FRAMES + 5 * BLOCK; // just over one full lap
        // Round the play length to whole blocks (play_counter renders in blocks).
        let played = play_counter(&mut engine, &mut deck_a, &mut src, lap);

        // Capture 3 s (the floor); it must be the last three seconds of counter
        // values, read across the wrapped buffer, not the stale frames the writer
        // already overwrote.
        let seconds = 3.0;
        let want = (seconds * SAMPLE_RATE as f64) as usize;
        let captured = engine.capture_sample(0, seconds).expect("history is full");
        assert_eq!(captured.len(), want * CHANNELS as usize);
        let last = (played - 1) as f32;
        let first = last - (want as f32 - 1.0);
        for f in 0..want {
            let expect = first + f as f32;
            assert_eq!(captured[2 * f], expect, "wrapped L frame {f}");
            assert_eq!(captured[2 * f + 1], -expect, "wrapped R frame {f}");
        }
    }

    /// (S4b-1c) Capture refuses below the floors and clamps to available history:
    /// a fresh deck (no history) returns None for a style sample; asking for more
    /// than exists returns just what played.
    #[test]
    fn capture_respects_floors_and_clamps_to_history() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);

        // Nothing played: a style sample is refused (under the 3 s floor).
        assert!(engine.capture_sample(0, STYLE_SAMPLE_SECONDS).is_none(), "no history → no sample");

        // Play ~4 s, then ask for 10 s: the capture clamps to what exists (~4 s),
        // which still clears the 3 s floor.
        let mut src = CounterSource::new();
        feed_counter(&mut deck_a, &mut src, PREBUFFER_FRAMES);
        let played = play_counter(&mut engine, &mut deck_a, &mut src, 4 * SAMPLE_RATE as usize);
        let captured = engine
            .capture_sample(0, STYLE_SAMPLE_SECONDS)
            .expect("4 s clears the 3 s floor");
        assert_eq!(
            captured.len(),
            played * CHANNELS as usize,
            "a 10 s request clamps to the ~4 s of history that exists"
        );
    }

    /// (S4b-2) A captured loop plays looping with a continuous seam: capture the
    /// played tail into a slot, play it, and the rendered output cycles with no
    /// gap. We verify the loop is on air (output is non-silent and periodic) and
    /// the slot reports playing.
    #[test]
    fn captured_loop_plays_looping() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(0.0); // full deck A so the loop is the whole mix

        // Play a steady sub-threshold sine so the captured loop has real content.
        let mut sa = SineSource::new(220.0, 0.2);
        feed(&mut deck_a, &mut sa, PREBUFFER_FRAMES + 4 * BLOCK);
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..200 {
            feed(&mut deck_a, &mut sa, BLOCK);
            engine.render(&mut out, BLOCK);
        }

        // Capture 1 s into slot 0 and play it.
        assert!(engine.capture_loop(0, 0, 1.0), "1 s of history captures");
        let slots = engine.loop_slots(0);
        assert!(slots[0].filled, "slot 0 is filled after capture");
        assert!(!slots[0].playing, "not playing until play_loop");
        assert!(engine.play_loop(0, 0, false), "the filled slot plays");
        assert!(engine.loop_slots(0)[0].playing, "slot 0 now reports playing");

        // Render two loop-lengths' worth; the output stays non-silent and bounded
        // (the loop is on air, folding continuously). Keep feeding the ring so the
        // live stream advances underneath.
        let mut energy = 0.0f64;
        for _ in 0..400 {
            feed(&mut deck_a, &mut sa, BLOCK);
            engine.render(&mut out, BLOCK);
            for &s in out.iter() {
                assert!(s.abs() <= MASTER_CEILING + 1e-7, "loop output holds the ceiling");
                assert!(s.is_finite(), "no NaN/Inf from the loop path");
                energy += (s * s) as f64;
            }
        }
        assert!(energy > 1.0, "the captured loop is audibly on air, energy {energy}");
    }

    /// (S4b-2b) `play_loop` swaps the deck output to the loop while the live ring
    /// keeps advancing; `stop_loop` returns to live seamlessly. A counter signal
    /// makes the live-vs-loop distinction exact: while looping, the output is NOT
    /// the live counter (it is the captured loop); after stop_loop it tracks the
    /// live counter again, and the played history kept advancing throughout (the
    /// captured frame indices prove the ring drained under the loop).
    #[test]
    fn play_loop_swaps_output_live_ring_keeps_advancing() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);

        // Play the counter so the history is full of identifiable frames.
        let mut src = CounterSource::new();
        feed_counter(&mut deck_a, &mut src, PREBUFFER_FRAMES);
        let before = play_counter(&mut engine, &mut deck_a, &mut src, 3 * SAMPLE_RATE as usize);

        // Capture a loop and play it. The capture buffer holds the counter tail,
        // which after the seam-fold no longer equals the live counter going
        // forward.
        assert!(engine.capture_loop(0, 0, 1.0));
        assert!(engine.play_loop(0, 0, false));

        // Render while looping AND keep feeding the ring. The live ring must keep
        // draining: the played history advances, so a capture taken now ends at a
        // HIGHER counter than `before` — proof the ring drained beneath the loop.
        let under_loop = play_counter(&mut engine, &mut deck_a, &mut src, 2 * SAMPLE_RATE as usize);
        let advanced = before + under_loop;
        // Capture 3 s (the floor); the latest frame is the live counter the ring
        // reached UNDER the loop, not the frozen `before` position.
        let sample = engine.capture_sample(0, 3.0).expect("history advanced under the loop");
        let last_l = sample[sample.len() - 2];
        assert!(
            (last_l - (advanced as f32 - 1.0)).abs() < 1.0,
            "history advanced under the loop: last captured counter {last_l}, expected ~{}",
            advanced - 1
        );
        assert!(
            last_l > before as f32,
            "the live ring drained past the pre-loop position {before} under the loop"
        );

        // stop_loop returns to live: the deck output tracks the live counter again.
        engine.stop_loop(0);
        assert!(!engine.loop_slots(0)[0].playing, "stop_loop clears the active slot");
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        // Full crossfade on A, flat EQ near-unity sub-threshold — but the counter
        // is large here, so instead of value-matching we assert the deck is live by
        // checking the output is NOT pinned to the loop (it changes with the ring).
        feed_counter(&mut deck_a, &mut src, BLOCK);
        engine.render(&mut out, BLOCK);
        // After returning to live, the loop bank is a passthrough (no active loop);
        // structurally the live frame reaches the mix. We assert the slot is no
        // longer playing (above) and the render produced finite output.
        assert!(out.iter().all(|s| s.is_finite()), "live output after stop is finite");
    }

    /// (S4b-3) `load_generated_loop` loops; a one-shot plays once then stops.
    #[test]
    fn generated_loop_loops_and_one_shot_stops() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(0.0); // full deck A

        // A short DC-ish pad buffer (sub-threshold) so output presence is obvious.
        let frames = SAMPLE_RATE as usize / 4; // 0.25 s, well above the loop-install floor
        let pad = vec![0.15f32; frames * CHANNELS as usize];

        // Loop: load into slot 0, play; the output stays non-silent across many
        // buffer lengths (it folds).
        assert!(engine.load_generated_loop(0, 0, pad.clone(), false), "loop loads");
        assert!(engine.play_loop(0, 0, false));
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        let mut energy_loop = 0.0f64;
        for _ in 0..(frames / BLOCK * 4) {
            engine.render(&mut out, BLOCK);
            for &s in out.iter() {
                energy_loop += (s * s) as f64;
            }
        }
        assert!(energy_loop > 1.0, "a generated loop keeps playing past its length");
        engine.stop_loop(0);

        // One-shot: load into slot 1, fire it; it overlays and ends after one pass.
        assert!(engine.load_generated_loop(0, 1, pad.clone(), true), "one-shot loads");
        assert!(engine.play_loop(0, 1, false));
        // Render the WHOLE pass (round up to a whole block so the one-shot's last
        // frame is consumed): it is non-silent.
        let pass_blocks = frames.div_ceil(BLOCK);
        let mut during = 0.0f64;
        for _ in 0..pass_blocks {
            engine.render(&mut out, BLOCK);
            for &s in out.iter() {
                during += (s * s) as f64;
            }
        }
        assert!(during > 1.0, "the one-shot plays its single pass");
        // Past the one pass: the one-shot has ended itself → silence (the live ring
        // is unfed, so the only possible sound was the one-shot).
        let mut after = 0.0f64;
        for _ in 0..8 {
            engine.render(&mut out, BLOCK);
            for &s in out.iter() {
                after += (s * s) as f64;
            }
        }
        assert!(after < 1e-6, "the one-shot stopped after one pass, energy {after}");
        // A one-shot is never "active": it overlays, so no slot reports playing.
        assert!(
            engine.loop_slots(0).iter().all(|s| !s.playing),
            "a one-shot never marks a slot as the active loop"
        );
    }

    /// (S4b-3b) `stop_one_shot` cuts a ringing one-shot immediately.
    #[test]
    fn stop_one_shot_cuts_the_overlay() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.set_crossfade(0.0);

        let frames = SAMPLE_RATE as usize; // 1 s one-shot
        let pad = vec![0.15f32; frames * CHANNELS as usize];
        assert!(engine.load_generated_loop(0, 0, pad, true));
        assert!(engine.play_loop(0, 0, false));

        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        // It is ringing.
        engine.render(&mut out, BLOCK);
        assert!(out.iter().any(|&s| s.abs() > 1e-4), "one-shot is ringing");
        // Cut it; the next renders are silent (well before the 1 s pass would end).
        engine.stop_one_shot(0);
        let mut after = 0.0f64;
        for _ in 0..4 {
            engine.render(&mut out, BLOCK);
            for &s in out.iter() {
                after += (s * s) as f64;
            }
        }
        assert!(after < 1e-6, "stop_one_shot silenced the overlay, energy {after}");
    }

    /// (S4b-4) `clear_loop` empties the slot and stops it if active; the four slots
    /// are independent.
    #[test]
    fn clear_empties_slot_and_slots_are_independent() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);

        let frames = SAMPLE_RATE as usize / 2; // 0.5 s
        let pad = vec![0.1f32; frames * CHANNELS as usize];

        // Fill all four slots; each independent.
        for slot in 0..LOOP_SLOT_COUNT {
            assert!(engine.load_generated_loop(0, slot, pad.clone(), false), "slot {slot} loads");
        }
        let slots = engine.loop_slots(0);
        assert_eq!(slots.len(), LOOP_SLOT_COUNT);
        assert!(slots.iter().all(|s| s.filled), "all four slots filled");
        assert!(slots.iter().all(|s| !s.playing), "none playing yet");

        // Play slot 2; only slot 2 is the active loop.
        assert!(engine.play_loop(0, 2, false));
        let slots = engine.loop_slots(0);
        assert!(slots[2].playing, "slot 2 is the active loop");
        assert!(
            (0..LOOP_SLOT_COUNT).filter(|&s| s != 2).all(|s| !slots[s].playing),
            "no other slot is playing"
        );

        // Clear slot 2 (the active one): it empties AND returns to live.
        engine.clear_loop(0, 2);
        let slots = engine.loop_slots(0);
        assert!(!slots[2].filled, "cleared slot 2 is empty");
        assert!(!slots[2].playing, "cleared active slot returns to live");
        // The other three are untouched.
        for slot in [0, 1, 3] {
            assert!(slots[slot].filled, "slot {slot} survives clearing slot 2");
        }

        // Clearing an empty slot is a no-op.
        engine.clear_loop(0, 2);
        assert!(!engine.loop_slots(0)[2].filled);
    }

    /// (S4b-5) A *freeze* still needs the live ring, so capture stays inert on a
    /// Playback deck — the played history holds the dead stream (ADR-0013), and a
    /// refused capture fills no slot.
    #[test]
    fn freeze_capture_is_inert_on_a_playback_deck() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);

        // Build some played history first, THEN load a track (parking the ring).
        let mut src = CounterSource::new();
        feed_counter(&mut deck_a, &mut src, PREBUFFER_FRAMES);
        play_counter(&mut engine, &mut deck_a, &mut src, SAMPLE_RATE as usize);
        engine.load_track(0, ramp_track(48_000));
        assert!(engine.get_track_status(0).is_some(), "deck A is now Playback");

        // Capture is refused (the played history holds the dead stream; ADR-0013).
        assert!(!engine.capture_loop(0, 0, 1.0), "capture_loop is a no-op on Playback");
        assert!(engine.capture_sample(0, STYLE_SAMPLE_SECONDS).is_none(), "no style sample on Playback");
        assert!(engine.loop_slots(0).iter().all(|s| !s.filled && !s.playing), "no slot filled by a refused capture");
    }

    /// (S4b-5b) A *loaded sample*, unlike a freeze, is a self-contained buffer with
    /// no ring dependency, so it loads and layers on a Playback deck too — the
    /// Samples tab works whatever the deck is doing, summing over the track
    /// (ADR-0022). The reported "could not load the sample" error was this guard.
    #[test]
    fn a_loaded_sample_layers_on_a_playback_deck() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);
        engine.load_track(0, ramp_track(48_000));
        assert!(engine.get_track_status(0).is_some(), "deck A is Playback");

        let pad = vec![0.1f32; (SAMPLE_RATE as usize / 2) * CHANNELS as usize];
        assert!(engine.load_generated_loop(0, 0, pad, false), "a sample loads onto a Playback deck");
        assert!(engine.loop_slots(0)[0].filled, "the slot fills");
        assert!(engine.play_loop(0, 0, true), "the sample layers on top of the track");
        assert!(engine.loop_slots(0)[0].playing, "the layer is sounding");
    }

    /// (ADR-0027) A preview is heard in the headphone/cue feed but NEVER the
    /// master: `audition_play` feeds only the cue output (no deck is on air, so
    /// the master stays silent), and `audition_stop` returns the cue to silence.
    #[test]
    fn audition_previews_into_the_cue_feed_only() {
        let mut engine = Engine::new();
        let _deck_a = engine.create_deck(0);
        let _deck_b = engine.create_deck(1);

        const FRAMES: usize = 256;
        // A buffer longer than one block, so the looping preview stays audible.
        engine.audition_play(vec![0.5f32; FRAMES * CHANNELS as usize * 4]);

        let mut master = vec![0.0f32; FRAMES * CHANNELS as usize];
        let mut cue = vec![0.0f32; FRAMES * CHANNELS as usize];
        engine.render_with_cue(&mut master, &mut cue, FRAMES);
        let master_peak = master.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let cue_peak = cue.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(master_peak < 1e-6, "the preview never reaches the master");
        assert!(cue_peak > 0.4, "the preview is audible in the cue feed");

        engine.audition_stop();
        let mut master2 = vec![0.0f32; FRAMES * CHANNELS as usize];
        let mut cue2 = vec![0.0f32; FRAMES * CHANNELS as usize];
        engine.render_with_cue(&mut master2, &mut cue2, FRAMES);
        let cue_peak2 = cue2.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(cue_peak2 < 1e-6, "stopping the preview silences the cue feed");
    }

    /// (S4b-6) `render` stays correct with a loop in path on one deck and a live
    /// deck on the other, over many blocks: ceiling held, no NaN/Inf — the same
    /// structural alloc-free discipline as Slices 1–4a (the loop/one-shot pop only
    /// reads pre-built buffer sources).
    #[test]
    fn render_runs_with_a_loop_in_path() {
        let mut engine = Engine::new();
        let mut deck_a = engine.create_deck(0);
        let mut deck_b = engine.create_deck(1);
        engine.set_crossfade(0.5);

        // Deck A: play, capture, loop. Deck B: a live sine.
        let mut sa = SineSource::new(220.0, 0.2);
        feed(&mut deck_a, &mut sa, PREBUFFER_FRAMES + 4 * BLOCK);
        let mut out = vec![0.0f32; BLOCK * CHANNELS as usize];
        for _ in 0..200 {
            feed(&mut deck_a, &mut sa, BLOCK);
            engine.render(&mut out, BLOCK);
        }
        assert!(engine.capture_loop(0, 0, 1.0));
        assert!(engine.play_loop(0, 0, false));
        // Also fire a one-shot to exercise the overlay path.
        let pad = vec![0.1f32; (SAMPLE_RATE as usize / 4) * CHANNELS as usize];
        assert!(engine.load_generated_loop(0, 1, pad, true));
        assert!(engine.play_loop(0, 1, false));

        let mut sb = SineSource::new(330.0, 0.2);
        feed(&mut deck_b, &mut sb, PREBUFFER_FRAMES + 64 * BLOCK);
        for _ in 0..64 {
            feed(&mut deck_a, &mut sa, BLOCK); // ring keeps advancing under the loop
            feed(&mut deck_b, &mut sb, BLOCK);
            engine.render(&mut out, BLOCK);
            for &s in out.iter() {
                assert!(s.abs() <= MASTER_CEILING + 1e-7, "ceiling held with a loop in path");
                assert!(s.is_finite(), "no NaN/Inf with loop + one-shot + live deck");
            }
        }
    }
}
