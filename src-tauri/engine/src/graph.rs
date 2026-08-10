//! The bare mix, ported from the Spike A `rt_engine` callback
//! (`spike/rust-audio/engine/src/bin/rt_engine.rs`): per-deck 3-band EQ, a
//! per-deck volume fader, an equal-power crossfade, and the master limiter
//! (feed-forward compressor → makeup cancellation → clip-guard ceiling).
//!
//! Built OFF the RT thread (the `fundsp` nodes allocate at construction). On the
//! RT path only `tick` / arithmetic run — both alloc-free on a pre-built node.
//!
//! ## Parity discipline (Spike A, `docs/spike-rust-audio.md`)
//!
//! The exact Chromium-vs-fundsp waveform parity was already proven offline in
//! Spike A against an `OfflineAudioContext` golden (`spike/rust-audio/golden/`):
//! the EQ shelves/bell match to ~1e-6, the limiter holds the two invariants
//! (ceiling exact, sub-threshold transparency). The headless CI tests in this
//! crate verify the *curve* (EQ kill/flat/boost levels) and the limiter
//! *invariants*, not a waveform diff — the diff was the spike's job, not CI's.

use fundsp::prelude32::*;

use crate::fx::{FxInsert, FxKind};
use crate::{CHANNELS, DECK_COUNT, MASTER_CEILING, SAMPLE_RATE};

/// Shelf Q matching the Web Audio fixed-slope shelves (S = 1 → Q = 1/√2). The
/// offline parity renderer uses the same value; see `spike/rust-audio`. Spike A
/// confirmed this matches WA's RBJ S=1 shelves to ~1e-6.
const EQ_SHELF_Q: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Centre/flat knob value: both halves of the curve meet at 0 dB here.
const EQ_FLAT: f32 = 0.5;
/// Boost at knob = 1 (`frontend/src/audio/eq.ts` `EQ_BOOST_DB`).
const EQ_BOOST_DB: f32 = 6.0;
/// Kill at knob = 0 (`frontend/src/audio/eq.ts` `EQ_KILL_DB`).
const EQ_KILL_DB: f32 = -40.0;

/// EQ band centre frequencies (`frontend/src/audio/eq.ts` `EQ_FILTERS`). The low
/// and high are true shelves; the mid is a peaking bell with gain.
const EQ_LOW_HZ: f32 = 250.0;
const EQ_MID_HZ: f32 = 1_000.0;
const EQ_HIGH_HZ: f32 = 2_500.0;
/// Mid bell Q (`frontend/src/audio/eq.ts`, matched at 0.7 in Spike A — WA
/// peaking Q is linear, so it passes straight to fundsp).
const EQ_MID_Q: f32 = 0.7;

/// EQ gain smoothing: the `follow()` halfway-response time. A knob move glides
/// the band's linear gain over this rather than stepping the biquad coefficients
/// (which removes the zipper); the filter keeps its delay-line state across the
/// change (no rebuild/reset, so no click). `follow()` is a three-pole one-pole
/// cascade, so the full settle is a few × this — 15 ms sits in the 10–30 ms
/// de-click band: tight enough to track a fast cut, slow enough to be inaudible.
/// Tunable by ear on the live stack (`docs/native-migration-hardware-checklist`).
const EQ_SMOOTH_SECS: f32 = 0.015;

/// One stateful 3-band EQ chain per (deck, channel). `fundsp` filters are
/// stateful, so each channel needs its own instance.
const EQ_CHAINS: usize = DECK_COUNT * CHANNELS as usize;

// --- Master limiter (M17 feed-forward compressor) constants ---
//
// `frontend/src/audio/master.ts`. Spike A established fundsp's `limiter` cannot
// reproduce Web Audio's `DynamicsCompressorNode` body, so this is a standard
// hand-rolled feed-forward compressor whose CONTRACT is two invariants (ceiling +
// sub-threshold transparency), not a waveform match.

/// Compressor threshold (dBFS): gain reduction begins above this.
const LIMITER_THRESHOLD_DB: f32 = -6.0;
/// High ratio → a limiter, not a gentle compressor.
const LIMITER_RATIO: f32 = 20.0;
/// Attack / release times (seconds). Fast attack catches transients; slow
/// release avoids pumping.
const LIMITER_ATTACK_SECONDS: f32 = 0.002;
const LIMITER_RELEASE_SECONDS: f32 = 0.25;

/// The implicit makeup Web Audio's `DynamicsCompressor` applies on EVERYTHING is
/// `(1/fullScaleGain)^0.6`; in dB that is `-0.6 * FULL_SCALE_GAIN_DB` where
/// `FULL_SCALE_GAIN_DB = thr - thr/ratio = -6 - (-6/20) = -5.7`. So the implicit
/// makeup is `-0.6 * -5.7 = +3.42 dB`. We CANCEL it (apply the inverse,
/// `10^(-3.42/20) ≈ 0.6745`) so the limiter is level-transparent below threshold
/// — the sub-threshold-transparency invariant. Mirrors `LIMITER_MAKEUP_DB` in
/// `master.ts`. Computed (not const-folded) so the `powf`s stay exact; see
/// `MasterLimiter::new`.
const LIMITER_FULL_SCALE_GAIN_DB: f32 =
    LIMITER_THRESHOLD_DB - LIMITER_THRESHOLD_DB / LIMITER_RATIO;
const LIMITER_MAKEUP_DB: f32 = -0.6 * LIMITER_FULL_SCALE_GAIN_DB;

/// The hand-rolled feed-forward master limiter: peak detector → gain computer
/// (hard knee, threshold −6 dB, ratio 20) → attack/release envelope smoothing →
/// makeup cancellation. The clip guard (a ±MASTER_CEILING clamp) runs after this
/// in `mix_frame` and guarantees the ceiling invariant unconditionally.
///
/// The gain it applies is shared across L and R (a stereo-linked limiter, like
/// the Web Audio `DynamicsCompressorNode`), so the image is preserved. Its
/// per-sample `gain` is exposed as telemetry (master gain reduction in dB).
struct MasterLimiter {
    /// Smoothed gain currently applied (1.0 = no reduction). Envelope state.
    envelope_gain: f32,
    /// Threshold as a linear magnitude (`10^(thr_db/20)`); precomputed.
    threshold_lin: f32,
    /// Web Audio's implicit makeup `10^(makeup_db/20) ≈ 1.482` (+3.42 dB),
    /// applied on EVERYTHING by the `DynamicsCompressor`; precomputed.
    implicit_makeup: f32,
    /// Compensating gain `10^(-makeup_db/20) ≈ 0.6745` (−3.42 dB) cancelling the
    /// implicit makeup so sub-threshold passes at unity; precomputed.
    makeup_cancel: f32,
    /// Per-sample attack coefficient (toward more reduction).
    attack_coeff: f32,
    /// Per-sample release coefficient (toward less reduction).
    release_coeff: f32,
}

impl MasterLimiter {
    fn new(sample_rate: f32) -> Self {
        // One-pole smoothing coefficients: `exp(-1 / (tau * sr))`. The envelope
        // moves toward the target gain at the attack rate when clamping down and
        // the release rate when opening back up.
        let attack_coeff = (-1.0 / (LIMITER_ATTACK_SECONDS * sample_rate)).exp();
        let release_coeff = (-1.0 / (LIMITER_RELEASE_SECONDS * sample_rate)).exp();
        MasterLimiter {
            envelope_gain: 1.0,
            threshold_lin: 10f32.powf(LIMITER_THRESHOLD_DB / 20.0),
            implicit_makeup: 10f32.powf(LIMITER_MAKEUP_DB / 20.0),
            makeup_cancel: 10f32.powf(-LIMITER_MAKEUP_DB / 20.0),
            attack_coeff,
            release_coeff,
        }
    }

    /// Process one stereo frame. Returns the limited `(l, r)` PRE clip-guard
    /// (the guard is applied by the caller) and the **compressor** gain reduction
    /// applied this frame as a linear factor in `(0, 1]` (1.0 = no reduction),
    /// for telemetry.
    ///
    /// The returned reduction is the compressor envelope ALONE, NOT including the
    /// fixed makeup-cancellation staging gain — so it is an honest account of net
    /// level change (0 dB / 1.0 sub-threshold, where the body is transparent),
    /// matching `getMasterGainReduction` in the Web Audio engine.
    #[inline]
    fn process(&mut self, l: f32, r: f32) -> (f32, f32, f32) {
        // Peak/level detector: the stereo peak drives a single linked gain.
        let peak = l.abs().max(r.abs());

        // Gain computer (hard knee, ratio R): below threshold the target gain is
        // 1; above it the overshoot in dB is reduced by the ratio, so the target
        // linear gain is `(peak/thr)^(1/R - 1)`.
        let target_gain = if peak > self.threshold_lin {
            (peak / self.threshold_lin).powf(1.0 / LIMITER_RATIO - 1.0)
        } else {
            1.0
        };

        // Attack/release envelope: clamp DOWN fast (attack) when the target asks
        // for more reduction than we currently apply, open UP slowly (release)
        // otherwise. Standard one-pole on the gain.
        let coeff = if target_gain < self.envelope_gain {
            self.attack_coeff
        } else {
            self.release_coeff
        };
        self.envelope_gain = target_gain + coeff * (self.envelope_gain - target_gain);

        // Reproduce the Web Audio chain faithfully: the `DynamicsCompressor`
        // applies its IMPLICIT makeup (+3.42 dB) on top of the reduction, and the
        // engine then CANCELS it (−3.42 dB) so inserting the limiter is
        // level-transparent below threshold. The two makeup gains cancel exactly,
        // leaving the compressor envelope as the net effect — which is precisely
        // the sub-threshold-transparency invariant. Telemetry reports the
        // envelope alone (the transparent staging excluded), an honest account of
        // net level change.
        let applied = self.envelope_gain * self.implicit_makeup * self.makeup_cancel;
        (l * applied, r * applied, self.envelope_gain)
    }

    fn reset(&mut self) {
        self.envelope_gain = 1.0;
    }
}

/// A control-value linear glide for the RT path (issue: MCP finding #10 — an
/// agent "walking the fader" was a series of audible steps). `set` stores a
/// target and a frame count; `tick` advances one frame toward it. The per-frame
/// step is recomputed off the remainder so the landing is exact, and
/// `frames = 0` snaps — the instant move every pre-ramp caller keeps.
struct Ramp {
    current: f32,
    target: f32,
    frames_left: u32,
}

impl Ramp {
    fn new(value: f32) -> Self {
        Ramp { current: value, target: value, frames_left: 0 }
    }

    /// Aim at `target`, landing `frames` frames from now (0 = snap immediately).
    fn set(&mut self, target: f32, frames: u32) {
        self.target = target;
        self.frames_left = frames;
        if frames == 0 {
            self.current = target;
        }
    }

    /// Still gliding? (Lets `mix_frame` skip per-frame work once landed.)
    fn active(&self) -> bool {
        self.frames_left > 0
    }

    /// Advance one frame and return the value to apply.
    #[inline]
    fn tick(&mut self) -> f32 {
        if self.frames_left > 0 {
            self.current += (self.target - self.current) / self.frames_left as f32;
            self.frames_left -= 1;
        }
        self.current
    }
}

/// One frame's mix output: the master pair the speakers get, the headphone cue
/// pair, and the master limiter's applied gain (for the gain-reduction meter).
pub(crate) struct FrameOut {
    pub(crate) master: (f32, f32),
    pub(crate) cue: (f32, f32),
    pub(crate) gain_reduction: f32,
}

/// The mix graph: per-channel EQ chains, per-deck volume, the crossfade gains,
/// and the master limiter. Holds no ring state — `render()` feeds it frame
/// samples and reads back the mixed, limited pair.
pub(crate) struct MixGraph {
    /// Layout: `[deckA_L, deckA_R, deckB_L, deckB_R]`. Boxed trait objects so the
    /// chain type need not be named; **built once and never rebuilt** — each band's
    /// gain is an audio-rate input driven by `eq_gains` (below), so `set_eq` only
    /// stores a new target and the filter keeps its delay-line state.
    eq: Vec<Box<dyn AudioUnit>>,
    /// Per-deck EQ band gains (LINEAR amplitude), `[low, mid, high]`, default 1.0
    /// (flat). One `Shared` triple PER DECK, read by BOTH the deck's L and R
    /// chains so they stay coefficient-identical. `set_eq` stores the target here
    /// (a lock-free atomic) and each chain's `follow()` glides toward it on the RT
    /// tick — no rebuild, no state reset (so no click), smoothed (so no zipper).
    eq_gains: [[Shared; 3]; DECK_COUNT],
    /// Per-deck Color FX insert (ADR-0008), placed post-EQ / pre-fader. At the
    /// effect's rest position it is a bit-exact dry passthrough. Rebuilt /
    /// reconfigured off the RT path by `set_fx` / `set_fx_amount`; `mix_frame`
    /// only ticks the pre-built nodes.
    fx: [FxInsert; DECK_COUNT],
    /// Per-deck channel-fader volume (linear, default 1.0), applied before the
    /// crossfade. Set by `set_volume` (instant) / `set_volume_ramped` (a linear
    /// glide the RT `mix_frame` ticks per frame — MCP finding #10).
    volumes: [Ramp; DECK_COUNT],
    /// Per-deck chain-head trim (linear, default 1.0). Applied PRE-EQ at the very
    /// head so EQ kills stay the performer's move (M17 gain staging). Set in dB by
    /// `set_trim` (non-RT).
    trims: [f32; DECK_COUNT],
    /// Per-deck on-air gate (default true). Off-air mutes the deck's contribution
    /// to the MASTER sum only — the per-deck channel meter (and, later, the cue
    /// tap) still see the live signal (M10 primed deck). Non-RT `set_on_air`.
    on_air: [bool; DECK_COUNT],
    /// Per-deck Color FX enable (default false = no effect selected). When false
    /// `mix_frame` skips the insert entirely (a pure dry passthrough); `set_fx`
    /// enables it, `clear_fx` disables it again — mirroring `setFx(null)` removing
    /// the effect in the Web Audio engine.
    fx_enabled: [bool; DECK_COUNT],
    /// Per-deck headphone-cue (PFL) tap (default false). A cued deck's post-EQ/FX,
    /// PRE-fader signal sums into the cue bus regardless of its fader / crossfade /
    /// on-air state (the point of pre-fade listen). Non-RT `set_cue`.
    cue: [bool; DECK_COUNT],
    /// Cue/master blend for the headphone feed (0 = cue bus only, 1 = master),
    /// default [`crate::INITIAL_CUE_MIX`]. Non-RT `set_cue_mix`.
    cue_mix: f32,
    /// Equal-power crossfade gains, one per deck. `gains[0]` weights deck A,
    /// `gains[1]` weights deck B. Recomputed by `set_crossfade` (non-RT) — or,
    /// while `xfade` glides, per frame by `mix_frame` from the ramped position.
    gains: [f32; DECK_COUNT],
    /// The crossfader POSITION, rampable (MCP finding #10). The equal-power law
    /// is applied to the ramped position each frame (not to the gains directly),
    /// so a glide holds constant power the whole way across.
    xfade: Ramp,
    /// The master limiter (feed-forward compressor); ticked per frame on the RT
    /// path. Its applied gain feeds the gain-reduction telemetry.
    limiter: MasterLimiter,
}

/// Map a knob value `v ∈ [0, 1]` to a band gain in dB, matching
/// `eqValueToDb` in `frontend/src/audio/eq.ts`: 0 → −40 dB kill, 0.5 → 0 dB,
/// 1 → +6 dB, linear within each half.
fn eq_value_to_db(value: f32) -> f32 {
    let v = value.clamp(0.0, 1.0);
    if v >= EQ_FLAT {
        ((v - EQ_FLAT) / (1.0 - EQ_FLAT)) * EQ_BOOST_DB
    } else {
        (1.0 - v / EQ_FLAT) * EQ_KILL_DB
    }
}

/// Linear gain (`10^(dB/20)`) for a band knob value.
fn eq_value_to_gain(value: f32) -> f32 {
    10f32.powf(eq_value_to_db(value) / 20.0)
}

/// Build one (deck, channel) EQ chain reading a deck's three band-gain `Shared`s.
/// `lowshelf` 250 Hz → `bell` (peaking) 1000 Hz Q 0.7 → `highshelf` 2500 Hz. Each
/// band uses fundsp's settable-INPUT form (input 0 = audio, 1 = centre Hz, 2 = Q,
/// 3 = linear gain): the centre/Q are constants and the gain is the band's
/// `Shared`, one-pole-smoothed by `follow()`. So a `set_eq` (storing a new gain in
/// the `Shared`) glides the coefficients instead of stepping them (no zipper) while
/// the SVF keeps its delay-line state (no rebuild/reset → no click). The SVF math
/// is identical to the fixed `*_hz` forms, so the settled curve still matches Web
/// Audio. Allocates — call OFF the RT path only.
fn build_eq_chain(gains: &[Shared; 3]) -> Box<dyn AudioUnit> {
    let low = (pass() | dc((EQ_LOW_HZ, EQ_SHELF_Q)) | (var(&gains[0]) >> follow(EQ_SMOOTH_SECS)))
        >> lowshelf();
    let mid = (pass() | dc((EQ_MID_HZ, EQ_MID_Q)) | (var(&gains[1]) >> follow(EQ_SMOOTH_SECS)))
        >> bell();
    let high = (pass() | dc((EQ_HIGH_HZ, EQ_SHELF_Q)) | (var(&gains[2]) >> follow(EQ_SMOOTH_SECS)))
        >> highshelf();
    let node = low >> mid >> high;
    let mut boxed: Box<dyn AudioUnit> = Box::new(node);
    boxed.set_sample_rate(SAMPLE_RATE as f64);
    boxed.reset();
    boxed
}

impl MixGraph {
    /// Build the graph off-thread. EQ defaults flat (every band at 0.5 → unity);
    /// volume defaults 1.0; crossfade centred.
    pub(crate) fn new() -> Self {
        // One linear-gain Shared per band per deck, seeded flat (1.0 = unity).
        let eq_gains: [[Shared; 3]; DECK_COUNT] =
            std::array::from_fn(|_| [shared(1.0), shared(1.0), shared(1.0)]);
        // Both channels of a deck read that deck's gain triple, so L and R stay
        // coefficient-identical. No warm-up is needed: `follow()` snaps to its
        // first input on its first tick (its `reset()` leaves the one-pole coeff
        // at 1.0 for sample 0), so each chain is flat from the first rendered
        // sample.
        let eq = (0..EQ_CHAINS)
            .map(|chain| build_eq_chain(&eq_gains[chain / CHANNELS as usize]))
            .collect();

        let mut graph = MixGraph {
            eq,
            eq_gains,
            fx: std::array::from_fn(|_| FxInsert::new(SAMPLE_RATE as f32)),
            volumes: std::array::from_fn(|_| Ramp::new(1.0)),
            trims: [1.0; DECK_COUNT],
            on_air: [true; DECK_COUNT],
            fx_enabled: [false; DECK_COUNT],
            cue: [false; DECK_COUNT],
            cue_mix: crate::INITIAL_CUE_MIX,
            gains: [0.0; DECK_COUNT],
            xfade: Ramp::new(0.5),
            limiter: MasterLimiter::new(SAMPLE_RATE as f32),
        };
        // Centre crossfade by default (equal-power 0.5).
        graph.set_crossfade(0.5);
        graph
    }

    /// Set the crossfader position in `[0, 1]` (0 = full deck A, 1 = full deck
    /// B) and recompute the equal-power gains. Non-RT (called from a control
    /// path); cheap arithmetic, but it writes `self.gains` which the RT
    /// `mix_frame` reads — see the note in `lib.rs` on the single-threaded
    /// ownership that keeps this sound.
    pub(crate) fn set_crossfade(&mut self, position: f32) {
        self.set_crossfade_ramped(position, 0);
    }

    /// Like `set_crossfade`, but glide there over `frames` frames (0 = instant):
    /// the RT `mix_frame` walks the POSITION linearly and re-applies the
    /// equal-power law each frame, so the glide holds constant power throughout
    /// (MCP finding #10 — stepwise agent fades were audible).
    pub(crate) fn set_crossfade_ramped(&mut self, position: f32, frames: u32) {
        let p = position.clamp(0.0, 1.0);
        self.xfade.set(p, frames);
        if frames == 0 {
            self.apply_crossfade(p);
        }
    }

    /// The equal-power law: gain_a = cos(p·π/2), gain_b = sin(p·π/2). At p = 0.5
    /// both are cos(π/4) = sin(π/4), matching the Spike A constant mix.
    fn apply_crossfade(&mut self, position: f32) {
        let angle = position * std::f32::consts::FRAC_PI_2;
        self.gains[0] = angle.cos();
        self.gains[1] = angle.sin();
    }

    /// Set a deck's channel-fader volume (linear, 0..1+). Non-RT; writes
    /// `self.volumes`, read by the RT `mix_frame`.
    pub(crate) fn set_volume(&mut self, deck: usize, gain: f32) {
        self.volumes[deck].set(gain, 0);
    }

    /// Like `set_volume`, but glide there linearly over `frames` frames
    /// (0 = instant) — `mix_frame` ticks the ramp (MCP finding #10).
    pub(crate) fn set_volume_ramped(&mut self, deck: usize, gain: f32, frames: u32) {
        self.volumes[deck].set(gain, frames);
    }

    /// Set a deck's chain-head trim in dB (0 dB = unity). Stored linear; applied
    /// pre-EQ in `mix_frame`. Non-RT.
    pub(crate) fn set_trim(&mut self, deck: usize, db: f32) {
        self.trims[deck] = 10f32.powf(db / 20.0);
    }

    /// Set a deck's on-air state. Off-air zeroes its master contribution but not
    /// its metered channel level. Non-RT.
    pub(crate) fn set_on_air(&mut self, deck: usize, on: bool) {
        self.on_air[deck] = on;
    }

    /// Remove a deck's Color FX (no effect selected): the insert is skipped — a
    /// pure dry passthrough — until the next `set_fx`. Non-RT.
    pub(crate) fn clear_fx(&mut self, deck: usize) {
        self.fx_enabled[deck] = false;
    }

    /// Set a deck's headphone-cue (PFL) tap. Non-RT.
    pub(crate) fn set_cue(&mut self, deck: usize, on: bool) {
        self.cue[deck] = on;
    }

    /// Set the cue/master headphone blend in `[0, 1]` (0 = cue only, 1 = master).
    /// Non-RT.
    pub(crate) fn set_cue_mix(&mut self, position: f32) {
        self.cue_mix = position.clamp(0.0, 1.0);
    }

    /// Set a deck's EQ band knob value in `[0, 1]` by storing the band's smoothed
    /// target gain — **no rebuild, no state reset**.
    ///
    /// This is option (b) the old rebuild path foreshadowed: the gain is driven
    /// through a `shared()`/`var` into fundsp's settable `lowshelf()`/`bell()`/
    /// `highshelf()` (gain as an audio-rate input), so a knob move is a single
    /// lock-free atomic store, not a `Box::new` + `reset()`. The filter keeps its
    /// delay-line state (no click) and a `follow()` one-pole glides the gain (no
    /// zipper). `Shared::set_value` takes `&self` and is lock-free, but the RT
    /// `var` reader and this writer are still mutually excluded by `&mut self` (the
    /// host drains commands before `render` — see the note in `lib.rs`); the
    /// `eq_gains` `Shared`s are simply the wait-free hand-off the device wrapper
    /// needs. `mix_frame` only ticks the pre-built chain — no alloc, ever.
    pub(crate) fn set_eq(&mut self, deck: usize, band: usize, value: f32) {
        self.eq_gains[deck][band].set_value(eq_value_to_gain(value));
    }

    /// Switch a deck's Color FX effect, rebuilding the effect's nodes **off the RT
    /// path** (it takes `&mut self`, so it can never overlap a `mix_frame` call —
    /// the same ownership argument as `set_eq`). The new effect lands at its rest
    /// position (bypassed); the control layer re-applies the knob amount. Selecting
    /// an effect also ENABLES the insert — it is skipped entirely while no effect
    /// is selected (`clear_fx`), matching `setFx(null)` removing the effect.
    pub(crate) fn set_fx(&mut self, deck: usize, kind: FxKind) {
        self.fx[deck].set_kind(kind);
        self.fx_enabled[deck] = true;
    }

    /// Set a deck's Color FX knob amount in `[0, 1]`, reconfiguring the effect's
    /// parameters off the RT path. Within the effect's dead zone the insert is a
    /// bit-exact dry passthrough.
    pub(crate) fn set_fx_amount(&mut self, deck: usize, amount: f32) {
        self.fx[deck].set_amount(amount);
    }

    /// Set a deck's gated beat period (ADR-0025) — the synced dub echo's clock.
    /// Non-RT; arithmetic only.
    pub(crate) fn set_beat_period(&mut self, deck: usize, period_seconds: Option<f32>) {
        self.fx[deck].set_beat_period(period_seconds);
    }

    /// Process one frame: per-deck chain-head trim, per-deck EQ both channels, the
    /// Color FX insert, per-deck volume, equal-power crossfade + on-air gate, the
    /// master limiter, then the clip-guard ceiling clamp. Also derives the
    /// headphone cue feed.
    /// `decks[d] = (left, right)` pre-EQ. Fills `deck_levels[d]` with each deck's
    /// post-fader magnitude (for the channel meters — taken BEFORE the crossfade
    /// weight and the on-air gate, so a faded-out / off-air deck still meters its
    /// live level). Returns the mixed/limited/clamped master `(left, right)`, the
    /// headphone cue `(left, right)`, and the master limiter's gain reduction this
    /// frame as a linear factor in `(0, 1]` (1.0 = no reduction), for telemetry.
    ///
    /// RT-safe: only `tick` on pre-built nodes (alloc-free) and arithmetic.
    #[inline]
    pub(crate) fn mix_frame(
        &mut self,
        decks: [(f32, f32); DECK_COUNT],
        deck_levels: &mut [f32; DECK_COUNT],
    ) -> FrameOut {
        let mut in1 = [0.0f32; 1];
        let mut out1 = [0.0f32; 1];

        // A gliding crossfade re-applies the equal-power law from the ramped
        // position each frame; once landed this is skipped and `gains` holds.
        if self.xfade.active() {
            let p = self.xfade.tick();
            self.apply_crossfade(p);
        }

        let mut mixed_l = 0.0f32;
        let mut mixed_r = 0.0f32;
        // Pre-fade-listen bus: cued decks' post-FX signal, independent of faders.
        let mut cue_l = 0.0f32;
        let mut cue_r = 0.0f32;

        for (d, (l, r)) in decks.into_iter().enumerate() {
            // Chain-head trim (M17): pre-EQ, so EQ kills stay the performer's move.
            let trim = self.trims[d];
            let l = l * trim;
            let r = r * trim;

            let li = d * CHANNELS as usize;
            in1[0] = l;
            self.eq[li].tick(&in1, &mut out1);
            let l = out1[0];
            in1[0] = r;
            self.eq[li + 1].tick(&in1, &mut out1);
            let r = out1[0];

            // Color FX insert (ADR-0008): post-EQ, pre-fader. Skipped entirely when
            // no effect is selected (a pure dry passthrough); otherwise bit-exact
            // dry within the effect's dead zone.
            let (l, r) = if self.fx_enabled[d] {
                self.fx[d].process(l, r)
            } else {
                (l, r)
            };

            // Pre-fade cue tap (PFL): a cued deck feeds the headphone bus from here
            // — post-EQ/FX but BEFORE the fader, crossfade, and on-air gate.
            if self.cue[d] {
                cue_l += l;
                cue_r += r;
            }

            // Channel fader. The post-fader magnitude drives the channel meter —
            // captured here, BEFORE the crossfade weight and the on-air gate.
            let volume = self.volumes[d].tick();
            let fader_l = l * volume;
            let fader_r = r * volume;
            deck_levels[d] = fader_l.abs().max(fader_r.abs());

            // Equal-power crossfade weight, gated by on-air (off-air contributes
            // nothing to the master sum but is still metered above).
            let on_air = if self.on_air[d] { 1.0 } else { 0.0 };
            let g = self.gains[d] * on_air;
            mixed_l += fader_l * g;
            mixed_r += fader_r * g;
        }

        // Master limiter: feed-forward compressor (thr −6 dB, ratio 20, attack
        // 2 ms, release 250 ms) with the implicit makeup cancelled, then the clip
        // guard (a hard ±MASTER_CEILING clamp) which GUARANTEES the ceiling
        // invariant regardless of what the compressor's attack lets through. The
        // body need not bit-match Chromium (implementation-defined); only the two
        // invariants — ceiling and sub-threshold transparency — must hold.
        let (lim_l, lim_r, applied_gain) = self.limiter.process(mixed_l, mixed_r);

        let mut ol = lim_l.clamp(-MASTER_CEILING, MASTER_CEILING);
        let mut or = lim_r.clamp(-MASTER_CEILING, MASTER_CEILING);

        // Flush any denormal that slipped through (belt-and-braces; FTZ/DAZ on
        // the device thread handles the rest).
        if ol.abs() < 1.0e-30 {
            ol = 0.0;
        }
        if or.abs() < 1.0e-30 {
            or = 0.0;
        }

        // Headphone feed: blend the PFL cue bus with the master per `cue_mix`
        // (0 = cue only, 1 = master), then clip-guard so the phones never clip.
        let mix = self.cue_mix;
        let cue_out_l = ((1.0 - mix) * cue_l + mix * ol).clamp(-MASTER_CEILING, MASTER_CEILING);
        let cue_out_r = ((1.0 - mix) * cue_r + mix * or).clamp(-MASTER_CEILING, MASTER_CEILING);

        FrameOut {
            master: (ol, or),
            cue: (cue_out_l, cue_out_r),
            gain_reduction: applied_gain,
        }
    }

    /// Reset all EQ filter state and the limiter envelope (e.g. on a hard engine
    /// reset). Non-RT.
    #[allow(dead_code)] // wired up by Engine::reset in a later slice
    pub(crate) fn reset(&mut self) {
        for node in &mut self.eq {
            node.reset();
        }
        for insert in &mut self.fx {
            insert.reset();
        }
        self.limiter.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The equal-power crossfade law `set_crossfade` recomputes: the two deck
    /// gains hold CONSTANT POWER (`a² + b² ≈ 1`) across the whole sweep — no dip
    /// or bump through the centre. This is the invariant the deleted Web Audio
    /// `engine.test.ts` `equalPowerGains` test guarded; it now lives only here.
    #[test]
    fn crossfade_holds_constant_power_across_the_sweep() {
        let mut graph = MixGraph::new();
        for &position in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            graph.set_crossfade(position);
            let power = graph.gains[0] * graph.gains[0] + graph.gains[1] * graph.gains[1];
            assert!(
                (power - 1.0).abs() < 1e-6,
                "constant power at p={position}: a²+b²={power} (gains {:?})",
                graph.gains,
            );
        }
    }

    /// A ramped crossfade (MCP finding #10) walks the position across `frames`
    /// frames: deck A's gain falls monotonically (no audible step), the
    /// equal-power invariant holds at every frame of the glide (the law is
    /// re-applied to the ramped POSITION, not interpolated on the gains), and
    /// the landing is exact.
    #[test]
    fn ramped_crossfade_glides_monotonically_and_lands_exact() {
        let mut graph = MixGraph::new();
        graph.set_crossfade(0.0);
        let frames = 100;
        graph.set_crossfade_ramped(1.0, frames);

        let mut levels = [0.0f32; DECK_COUNT];
        let mut last_gain_a = graph.gains[0];
        for frame in 0..frames {
            graph.mix_frame([(0.0, 0.0); DECK_COUNT], &mut levels);
            let power = graph.gains[0] * graph.gains[0] + graph.gains[1] * graph.gains[1];
            assert!(
                (power - 1.0).abs() < 1e-5,
                "constant power mid-glide at frame {frame}: a²+b²={power}",
            );
            assert!(
                graph.gains[0] <= last_gain_a + 1e-6,
                "deck A gain rose at frame {frame}: {} -> {}",
                last_gain_a,
                graph.gains[0],
            );
            last_gain_a = graph.gains[0];
        }
        assert!((graph.gains[0] - 0.0).abs() < 1e-6, "deck A silent after the glide");
        assert!((graph.gains[1] - 1.0).abs() < 1e-6, "deck B full after the glide");

        // Landed: further frames hold the endpoint (the ramp goes inactive).
        graph.mix_frame([(0.0, 0.0); DECK_COUNT], &mut levels);
        assert!((graph.gains[1] - 1.0).abs() < 1e-6, "endpoint holds after landing");
    }

    /// A ramped volume glides linearly to the target and lands exact; an
    /// un-ramped `set_volume` still snaps (frames = 0 keeps the instant move).
    #[test]
    fn ramped_volume_glides_and_lands_exact() {
        let mut graph = MixGraph::new();
        graph.set_crossfade(0.0); // full deck A
        let frames = 50;
        graph.set_volume_ramped(0, 0.0, frames);

        let mut levels = [0.0f32; DECK_COUNT];
        let mut last_level = f32::MAX;
        for frame in 0..frames {
            graph.mix_frame([(0.5, 0.5), (0.0, 0.0)], &mut levels);
            assert!(
                levels[0] <= last_level + 1e-6,
                "deck A level rose at frame {frame}: {last_level} -> {}",
                levels[0],
            );
            last_level = levels[0];
        }
        assert!(last_level.abs() < 1e-6, "deck A faded to silence after the glide");

        graph.set_volume(0, 1.0);
        graph.mix_frame([(0.5, 0.5), (0.0, 0.0)], &mut levels);
        assert!(
            (levels[0] - 0.5).abs() < 1e-6,
            "un-ramped set_volume snaps back instantly (got {})",
            levels[0],
        );
    }

    /// Out-of-range positions clamp to the endpoints rather than running the
    /// trig past `[0, 1]` (which would overshoot the equal-power law).
    #[test]
    fn crossfade_clamps_out_of_range_positions() {
        let mut graph = MixGraph::new();
        graph.set_crossfade(0.0);
        let at_zero = graph.gains;
        graph.set_crossfade(-1.0);
        assert_eq!(graph.gains, at_zero, "negative position clamps to 0.0");

        graph.set_crossfade(1.0);
        let at_one = graph.gains;
        graph.set_crossfade(2.0);
        assert_eq!(graph.gains, at_one, "position past 1.0 clamps to 1.0");
    }

    /// The endpoints hand the mix wholly to one deck: full deck A at p=0
    /// (a≈1, b≈0), full deck B at p=1 (reversed).
    #[test]
    fn crossfade_endpoints_favour_one_deck() {
        let mut graph = MixGraph::new();
        graph.set_crossfade(0.0);
        assert!((graph.gains[0] - 1.0).abs() < 1e-6, "deck A full at p=0");
        assert!(graph.gains[1].abs() < 1e-6, "deck B silent at p=0");

        graph.set_crossfade(1.0);
        assert!(graph.gains[0].abs() < 1e-6, "deck A silent at p=1");
        assert!((graph.gains[1] - 1.0).abs() < 1e-6, "deck B full at p=1");
    }
}
