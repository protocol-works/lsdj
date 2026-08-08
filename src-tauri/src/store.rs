//! The shell-level interface-state store (ADR-0020, issue #37 Phase 1).
//!
//! Rust is the single source of truth for the instrument's **semantic/identity +
//! audio-param** interface state; the webview is a unidirectional projection of it.
//! The on-screen UI, the hardware (MIDI), and — later — an MCP agent are symmetric
//! peer controllers: each emits an intent that mutates this one store; the store
//! emits a [`STORE_CHANGED_EVENT`] change event carrying the fresh snapshot; the
//! webview re-renders from it.
//!
//! # Layering (what lives here vs. the engine)
//!
//! The real-time audio core ([`lsdj_engine`]) stays the truth of *what the audio is
//! doing* — gains, EQ coefficients, crossfade, loop regions, buffers, and the live
//! read-backs (playhead, levels, ring fill) the webview already polls via
//! `engine_snapshot`. This store is the truth of *what the instrument shows*: the
//! values that were set. A mutation forwards the audio-affecting change to the
//! engine / sidecar as the commands already do, **and** records it here so the
//! projection (and a future MCP `resources` read) has one authoritative copy with
//! no read-back getters to bolt on.
//!
//! Per ADR-0020 (accepted with the issue #37 narrowing), **ephemeral view state**
//! (active tab, scroll/highlight, in-progress form fields, the
//! loaded-but-not-confirmed selection) deliberately stays in React and is *not*
//! held here.
//!
//! # Testability
//!
//! The mutation logic lives in pure [`InterfaceState`] methods (no `AppHandle`, no
//! IPC — unit-tested directly). [`InterfaceStore`] is the thin shell wrapper that
//! locks the state, applies a mutation, and emits the snapshot.

use std::sync::{mpsc, Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use lsdj_engine::{EqBand, FxKind, DECK_COUNT};

/// The Tauri event the webview subscribes to for store changes. Each mutation emits
/// it with the full fresh [`InterfaceState`] snapshot (the state is small —
/// semantic/audio params, never audio buffers — so carrying it whole is simpler
/// than diffing and the projection just replaces its cache).
pub const STORE_CHANGED_EVENT: &str = "store://changed";

/// The default drum-conditioning strength (issue #50): the `cfg_drums`
/// guidance scale a deck starts with. 4.0 matches the `magenta-realtime`
/// reference's `DEFAULT_CFG_DRUMS` (its UI caps the scale at [0, 5]); the
/// model's own default of 1.0 barely bites (docs/spike-mrt2.md). Shared with
/// the note-steering service, whose `DrumConditioning::default` lands here.
pub const DEFAULT_DRUM_STRENGTH: f32 = 4.0;

/// The reset-to-default baseline for the live generation params (issue #84):
/// the `magenta-realtime` reference operating point (`defaultParams.ts`). This
/// is the runtime source of truth (reassert-on-`ready` overwrites the engine's
/// own init value), so a deck starts here and each per-knob reset returns to it.
/// Cross-boundary duplication that must stay in step: the Python engine
/// constants (`engine.py`, its init/reset value) and the frontend first-paint
/// fallback (`GENERATION_FALLBACK`). Keep all three equal.
pub const DEFAULT_TEMPERATURE: f32 = 1.1;
pub const DEFAULT_TOP_K: u32 = 50;
pub const DEFAULT_CFG_MUSICCOCA: f32 = 1.6;
pub const DEFAULT_CFG_NOTES: f32 = 2.4;

// The exposed slider bounds for the live generation params (issue #84), matching
// the `magenta-realtime` reference (`Settings.tsx`): the two CFG scales share
// [0, 5], temperature is [floor, 3], top-k is [1, 1024]. Enforced at every write
// path by `GenerationSnap::clamped` — a value above ~5 cfg drifts out of
// distribution, and a temperature of 0 divides the sampling logits by zero.
pub const GEN_CFG_MIN: f32 = 0.0;
pub const GEN_CFG_MAX: f32 = 5.0;
pub const GEN_TEMPERATURE_MIN: f32 = 0.05;
pub const GEN_TEMPERATURE_MAX: f32 = 3.0;
pub const GEN_TOP_K_MIN: u32 = 1;
pub const GEN_TOP_K_MAX: u32 = 1024;

/// A Color FX kind as it appears in the snapshot — a serde camelCase enum mirroring
/// the frontend `FxKind` (the six `fx.ts` effects), so the projection names the
/// effect by intent rather than a magic index. `Deserialize` too — it
/// round-trips through the shell settings file (ADR-0020 phase C).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FxKindSnap {
    Filter,
    DubEcho,
    Space,
    Crush,
    Noise,
    Sweep,
}

impl From<FxKind> for FxKindSnap {
    fn from(kind: FxKind) -> Self {
        match kind {
            FxKind::Filter => FxKindSnap::Filter,
            FxKind::DubEcho => FxKindSnap::DubEcho,
            FxKind::Space => FxKindSnap::Space,
            FxKind::Crush => FxKindSnap::Crush,
            FxKind::Noise => FxKindSnap::Noise,
            FxKind::Sweep => FxKindSnap::Sweep,
        }
    }
}

impl From<FxKindSnap> for FxKind {
    fn from(kind: FxKindSnap) -> Self {
        match kind {
            FxKindSnap::Filter => FxKind::Filter,
            FxKindSnap::DubEcho => FxKind::DubEcho,
            FxKindSnap::Space => FxKind::Space,
            FxKindSnap::Crush => FxKind::Crush,
            FxKindSnap::Noise => FxKind::Noise,
            FxKindSnap::Sweep => FxKind::Sweep,
        }
    }
}

/// Which source a deck plays (M19, ADR-0013): the realtime model stream or a
/// loaded track.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlayModeSnap {
    Realtime,
    Playback,
}

/// Hot-cue pads per loaded track (mirrors the webview's `HOT_CUE_COUNT`).
pub const HOT_CUE_COUNT: usize = 8;

/// A deck's three-band EQ in the snapshot (each 0..1, mirroring the frontend
/// `Record<EqBand, number>`). `Deserialize` too — it round-trips through the
/// shell settings file (ADR-0020 phase C).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EqSnap {
    pub low: f32,
    pub mid: f32,
    pub high: f32,
}

/// A deck's Color FX in the snapshot: the active effect (or `None`) plus the knob
/// amount. The amount persists across a kind change exactly as the frontend keeps
/// it — `set_fx` records the kind, the follow-up `set_fx_amount` records the rest
/// value the deck re-applies.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FxSnap {
    pub kind: Option<FxKindSnap>,
    pub amount: f32,
}

/// A loaded track's identity in the store (a playback-deck read-back the store
/// mirrors). `Deserialize` too — it crosses as a `set_deck_track` command argument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackIdentitySnap {
    pub title: String,
    /// Offline beat-tracker BPM, or `None` when the gate refuses a number.
    pub bpm: Option<f64>,
    pub duration_seconds: f64,
}

/// An active loop region on a playback deck, in track seconds (mirrors the frontend
/// `TrackLoop`). `Deserialize` too — it crosses as a `set_deck_transport` argument.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopRegionSnap {
    pub start_seconds: f64,
    pub end_seconds: f64,
}

/// A playback deck's live transport read-back (a throttled mirror the webview writes
/// up): the playhead, varispeed rate, and the active loop region. `None` on a realtime
/// deck or with no track. The playhead is mirrored at a throttled cadence (the webview
/// caps it ~4 Hz) so this read-back doesn't churn `store://changed` at the audio poll
/// rate; an agent reads the resource on demand, so coarse freshness is enough.
/// `Deserialize` too — it crosses as a `set_deck_transport` argument.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransportSnap {
    pub playhead_seconds: f64,
    /// Varispeed rate (1.0 = as recorded); effective BPM is `track.bpm * rate`.
    pub rate: f64,
    pub loop_region: Option<LoopRegionSnap>,
    /// Whether the playback transport is rolling. The deck-level `playing`
    /// reflects only the realtime stream (MCP finding #13 — an agent watching
    /// it saw a rolling track as idle), so playback observability lives here.
    /// `default` keeps an older webview mirror payload deserialising.
    #[serde(default)]
    pub playing: bool,
}

/// A point on the 2D style pad (0..1 each axis). `Deserialize`/`JsonSchema` too — the
/// cursor crosses as a `set_deck_style` / `set_style_cursor` argument (UI/MIDI and MCP).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PadPointSnap {
    pub x: f32,
    pub y: f32,
}

/// One style-pad target: a prompt at a pad position. The store owns the
/// arrangement (ADR-0020 phase B); a sampled chip (ADR-0011) carries its
/// session-only embedding id in `sample` — held here so there is exactly one
/// target list, but excluded from shell persistence and stripped when the
/// worker (whose cache holds the embedding) dies. `Deserialize`/`JsonSchema`
/// too — targets cross as MCP `set_style` arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StyleTargetSnap {
    pub x: f32,
    pub y: f32,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
}

/// The note mode a steering surface authors in (ADR-0023): chord-follow maps
/// held pitches to "model decides the articulation"; onset marks fresh presses
/// so the performer owns the attack timing. `Deserialize`/`JsonSchema` too —
/// it crosses as a `set_deck_notes` / MCP `set_notes` argument.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NoteModeSnap {
    Chord,
    Onset,
}

/// The key/scale a performance surface snaps to (issue #48). Chromatic is
/// the no-snap escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ScaleSnap {
    Major,
    Minor,
    PentatonicMinor,
    Chromatic,
}

/// A deck's performance-surface config (issue #48, ADR-0031): whether the
/// surface is armed (armed decks take pad/keyboard notes AND run the small
/// ADR-0023 performance chunk), the key/scale the notes snap to, and the
/// note mode (chord-follow or on-grid onset). Owned by the shell
/// note-steering service; the store holds it for the projection.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceSnap {
    pub armed: bool,
    /// Key root as a pitch class (0 = C … 11 = B).
    pub key: u8,
    pub scale: ScaleSnap,
    pub mode: NoteModeSnap,
}

impl Default for PerformanceSnap {
    fn default() -> Self {
        PerformanceSnap {
            armed: false,
            key: 0,
            scale: ScaleSnap::Major,
            mode: NoteModeSnap::Chord,
        }
    }
}

/// A realtime deck's note steering (ADR-0023): the held MIDI pitches and the
/// note mode. The shell note-steering service owns the pitches→multihot
/// mapping and drives the worker directly (ADR-0031); the store holds the
/// authored state so every surface projects the same truth. `Deserialize`/
/// `JsonSchema` too — it crosses as an MCP `set_notes` argument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoteSteeringSnap {
    /// Held MIDI pitches (0..=127).
    pub pitches: Vec<u8>,
    pub mode: NoteModeSnap,
}

/// A beat anchor the phase consumers can trust (M20/ADR-0025): the
/// pushed-frame index of a recent beat and the gated tempo it belongs to.
/// Published as a pair — a clock, not two independent readings — so a
/// consumer can never mix an anchor with a fresher tempo.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveBeatSnap {
    pub anchor_frame: f64,
    pub bpm: f64,
}

/// A deck's live beat analysis (ADR-0025), written by the shell's analysis
/// thread at most ~once per second: the honesty-gated readout (`None` =
/// blank, the feature), the latest estimate confidence, the phase clock, and
/// the stream origin in engine context frames (captured at reset — the
/// mapping from the anchor's pushed-frame domain onto engine time). A
/// MEASUREMENT, not a controller value: its mutation records without
/// forwarding anything.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisSnap {
    pub bpm: Option<f64>,
    pub confidence: f64,
    pub live_beat: Option<LiveBeatSnap>,
    pub origin_frames: f64,
}

impl Default for AnalysisSnap {
    fn default() -> Self {
        AnalysisSnap {
            bpm: None,
            confidence: 0.0,
            live_beat: None,
            origin_frames: 0.0,
        }
    }
}

/// A deck's live generation operating point (issue #84): the sampling /
/// guidance params the reference `magenta-realtime` apps expose. Deck config
/// like the drum-sit — it survives transport transitions; the note-steering
/// service re-sends it to a fresh worker on `ready`. Written through the shell
/// note-steering service (the single deck control-frame sender), persisted, and
/// projected down for the drawer sliders. `Deserialize` too — it is reused as
/// the persisted shape (settings.rs) and hydrated back at boot, like [`EqSnap`].
/// (Live edits cross as a [`GenerationPatch`], not a whole snap.)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationSnap {
    /// Sampling temperature (randomness). The shell floors it off zero.
    pub temperature: f32,
    /// Sampling top-k.
    pub top_k: u32,
    /// Prompt/style adherence (`cfg_musiccoca`) — guides all generation.
    pub cfg_musiccoca: f32,
    /// Note adherence (`cfg_notes`) — bites only while note-steering.
    pub cfg_notes: f32,
}

impl Default for GenerationSnap {
    fn default() -> Self {
        GenerationSnap {
            temperature: DEFAULT_TEMPERATURE,
            top_k: DEFAULT_TOP_K,
            cfg_musiccoca: DEFAULT_CFG_MUSICCOCA,
            cfg_notes: DEFAULT_CFG_NOTES,
        }
    }
}

impl GenerationSnap {
    /// Clamp every field to its exposed range (issue #84). The single
    /// trust-boundary guard shared by every write path — pure, so it is
    /// unit-tested directly. Temperature floors off zero (0 NaNs the sampler),
    /// the two CFG scales share `[0, 5]`, and top-k is `[1, 1024]`.
    pub fn clamped(self) -> Self {
        GenerationSnap {
            temperature: self.temperature.clamp(GEN_TEMPERATURE_MIN, GEN_TEMPERATURE_MAX),
            top_k: self.top_k.clamp(GEN_TOP_K_MIN, GEN_TOP_K_MAX),
            cfg_musiccoca: self.cfg_musiccoca.clamp(GEN_CFG_MIN, GEN_CFG_MAX),
            cfg_notes: self.cfg_notes.clamp(GEN_CFG_MIN, GEN_CFG_MAX),
        }
    }
}

/// A partial edit to a deck's generation params (issue #84): only the changed
/// field(s) cross the IPC boundary, so the shell merges them onto its
/// authoritative value under the note-steering lock. A rapid second edit can
/// never rebuild from a stale webview snapshot and revert the first
/// (`GenerationPatch::apply`, then `GenerationSnap::clamped`).
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GenerationPatch {
    pub temperature: Option<f32>,
    pub top_k: Option<u32>,
    pub cfg_musiccoca: Option<f32>,
    pub cfg_notes: Option<f32>,
}

impl GenerationPatch {
    /// Apply the set fields onto `base` (unclamped — the caller clamps the
    /// merged result once).
    pub fn apply(self, mut base: GenerationSnap) -> GenerationSnap {
        if let Some(v) = self.temperature {
            base.temperature = v;
        }
        if let Some(v) = self.top_k {
            base.top_k = v;
        }
        if let Some(v) = self.cfg_musiccoca {
            base.cfg_musiccoca = v;
        }
        if let Some(v) = self.cfg_notes {
            base.cfg_notes = v;
        }
        base
    }
}

/// One generation param, for a per-knob reset-to-default (issue #84). The reset
/// target is the shell's own `DEFAULT_*` baseline (`reset_patch`), so the
/// frontend's reset (↺) only names the field — it never holds a copy of the
/// reference default that could drift from the engine's.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GenerationField {
    Temperature,
    TopK,
    CfgMusiccoca,
    CfgNotes,
}

impl GenerationField {
    /// The single-field patch that resets this param to the reference baseline.
    pub fn reset_patch(self) -> GenerationPatch {
        match self {
            GenerationField::Temperature => GenerationPatch {
                temperature: Some(DEFAULT_TEMPERATURE),
                ..GenerationPatch::default()
            },
            GenerationField::TopK => GenerationPatch {
                top_k: Some(DEFAULT_TOP_K),
                ..GenerationPatch::default()
            },
            GenerationField::CfgMusiccoca => GenerationPatch {
                cfg_musiccoca: Some(DEFAULT_CFG_MUSICCOCA),
                ..GenerationPatch::default()
            },
            GenerationField::CfgNotes => GenerationPatch {
                cfg_notes: Some(DEFAULT_CFG_NOTES),
                ..GenerationPatch::default()
            },
        }
    }
}

/// One deck's state in the store: the mixer channel plus the realtime-deck
/// read-backs the store mirrors (model / playing). Not `Copy` — `model` is a
/// `String`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeckSnap {
    pub volume: f32,
    pub eq: EqSnap,
    /// Chain-head trim in dB (M17 gain staging; 0 dB = unity).
    pub trim_db: f32,
    /// Headphone-cue (PFL) tap on/off.
    pub cue: bool,
    /// On-air (M10 primed deck): off-air mutes only the master feed.
    pub on_air: bool,
    pub fx: FxSnap,
    /// The realtime deck's loaded model name — a sidecar read-back the store
    /// mirrors (the webview derives it from worker status and writes it up); `None`
    /// before the worker reports ready.
    pub model: Option<String>,
    /// Whether the realtime deck is generating — a derived read-back the store
    /// mirrors (set by play/stop, cleared on model-load / worker-death).
    pub playing: bool,
    /// Which source the deck plays (M19, ADR-0013): the realtime model stream
    /// or a loaded track. Written by the webview's load flow (the load
    /// orchestration stays webview-side until the transport inverts, phase E);
    /// recorded here so an agent sees the mode without a webview round-trip.
    pub mode: PlayModeSnap,
    /// Hot-cue points on the loaded track, in track seconds, one per pad (empty
    /// with no track). OWNED here since ADR-0020 phase D: pads reset with the
    /// track identity (`set_track`), mutate through the `set_deck_cue_point`
    /// intent (UI) and the MCP cue tools, and the webview projects them. The
    /// jump itself stays a plain seek.
    pub cues: Vec<Option<f64>>,
    /// The loaded track's identity on a playback deck (a read-back the store
    /// mirrors), or `None` on a realtime deck / with no track.
    pub track: Option<TrackIdentitySnap>,
    /// The playback deck's live transport (playhead / rate / loop) — a throttled
    /// read-back the webview mirrors up, `None` on a realtime deck / with no track.
    pub transport: Option<TransportSnap>,
    /// The freeze/sample loop-slot labels, one per pad (`None` for an empty slot or
    /// an unlabelled freeze) — a read-back the store mirrors. Empty until the deck
    /// reports its slots.
    pub loop_labels: Vec<Option<String>>,
    /// The realtime deck's 2D style-pad targets (prompt + position). OWNED
    /// here since ADR-0020 phase B: the webview projects and emits intents;
    /// the shell style sender blends and drives the worker. (The writer-flag
    /// adoption gate this replaced is gone — a projection has nothing to
    /// adopt, which retires the whole echo-race class.)
    pub style_targets: Vec<StyleTargetSnap>,
    /// Which style targets are selected into the active blend (the net mask,
    /// one bool per target) — mirrored up by the webview so the pad LEDs can
    /// burn selected targets bright and dim the rest (ADR-0031: LEDs read the
    /// store). Empty = no mask (every target pad lit full).
    pub style_selected: Vec<bool>,
    /// The 2D style-pad cursor (the blend point).
    pub cursor: PadPointSnap,
    /// Whether the deck is primed off-air (the transport-CUE LED state) — a
    /// read-back the webview mirrors up; the deck's prime/play flow owns it.
    pub primed: bool,
    /// The performance-surface config (issue #48) — armed/key/scale/mode,
    /// written through the shell note-steering service.
    pub performance: PerformanceSnap,
    /// The realtime deck's note steering (ADR-0023), or `None` when unsteered.
    /// Cleared on transport transitions — a discontinuity resets conditioning.
    pub notes: Option<NoteSteeringSnap>,
    /// Drum conditioning (ADR-0023): `None` = the model decides, `false` =
    /// suppress drums ("sit beside"). The product is binary (suppress vs auto,
    /// like the `magenta-realtime` `drumless` toggle); `Some(true)` (force) is
    /// a valid model flag the engine still accepts but no LSDJ surface emits.
    /// Unlike `notes` this is deck config (issue #50): it survives transport
    /// transitions — the steering service re-asserts it on the play edge.
    pub drums: Option<bool>,
    /// The drum-conditioning strength (issue #50): the `cfg_drums` guidance
    /// scale the worker applies every chunk regardless of `drums` (like the
    /// reference). Deck config like `drums`; defaults to `DEFAULT_DRUM_STRENGTH`
    /// (the measured sweet spot, not the library's weaker default).
    pub drums_strength: f32,
    /// The deck's live generation operating point (issue #84): the tunable
    /// sampling/guidance params, written through the note-steering service and
    /// re-sent to a fresh worker on `ready`. Deck config that persists.
    pub generation: GenerationSnap,
    /// The deck's live beat analysis (ADR-0025) — a shell-written measurement,
    /// blank until the honesty gate acquires.
    pub analysis: AnalysisSnap,
    /// The worker crashed and has not been restarted (the status relay writes
    /// it — the same shell-side source the webview's reducer reads, so an
    /// agent sees a dead deck without a webview round-trip).
    pub worker_died: bool,
    /// The worker is reloading for a model switch.
    pub switching_model: bool,
    /// The deck's hardware SHIFT is held — written by the native MIDI
    /// translator (the state's origin); the webview's copy projects it for
    /// the cross-deck jog steering until Phase D consolidates.
    pub shift_held: bool,
}

impl Default for DeckSnap {
    fn default() -> Self {
        DeckSnap {
            volume: 1.0,
            eq: EqSnap {
                low: 0.5,
                mid: 0.5,
                high: 0.5,
            },
            trim_db: 0.0,
            cue: false,
            // Decks are audible by default; off-air is the deliberate primed state.
            on_air: true,
            fx: FxSnap {
                kind: None,
                amount: 0.0,
            },
            model: None,
            playing: false,
            mode: PlayModeSnap::Realtime,
            cues: Vec::new(),
            track: None,
            transport: None,
            loop_labels: Vec::new(),
            style_targets: Vec::new(),
            style_selected: Vec::new(),
            cursor: PadPointSnap { x: 0.5, y: 0.5 },
            primed: false,
            performance: PerformanceSnap::default(),
            notes: None,
            drums: None,
            drums_strength: DEFAULT_DRUM_STRENGTH,
            generation: GenerationSnap::default(),
            analysis: AnalysisSnap::default(),
            worker_died: false,
            switching_model: false,
            shift_held: false,
        }
    }
}

/// The shell recorder's state (ADR-0028): whether a take is streaming to
/// disk and where. Written by the recording commands themselves, so a
/// webview reload (or an agent) reads the truth instead of a local flag.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingSnap {
    pub active: bool,
    pub path: Option<String>,
}

/// The authoritative interface state — the snapshot shape the webview projects.
///
/// Pre-hydration it holds neutral defaults; on boot the webview replays its
/// persisted mixer settings through the same set commands (which record here), so
/// the store converges to the real values before the controls render. View state is
/// intentionally absent (it stays in React — the ADR-0020 narrowing).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceState {
    /// Per-deck mixer channel, indexed by deck (length [`DECK_COUNT`]).
    pub decks: Vec<DeckSnap>,
    /// Equal-power crossfader position (0 = deck A, 1 = deck B).
    pub crossfade: f32,
    /// Cue/master headphone blend (0 = cue only, 1 = master).
    pub cue_mix: f32,
    /// The shell recorder's state (see [`RecordingSnap`]).
    pub recording: RecordingSnap,
    /// The chosen MAIN output device name ("" = system default) — shell-
    /// persisted (ADR-0020 phase A); the webview picker is a projection.
    pub main_device: String,
    /// The chosen CUE output device name ("" = same as main).
    pub cue_device: String,
    /// The recordings folder ("" = Downloads).
    pub recordings_folder: String,
    /// Whether the standalone MIDI-keyboard window (issue #49) is visible — a
    /// shell-owned window-lifecycle read-back the drawer's toggle button mirrors
    /// (the window lives shell-side; the webview only reflects it).
    pub piano_window_open: bool,
}

impl Default for InterfaceState {
    fn default() -> Self {
        InterfaceState {
            decks: vec![DeckSnap::default(); DECK_COUNT],
            crossfade: 0.5,
            cue_mix: 0.5,
            recording: RecordingSnap::default(),
            main_device: String::new(),
            cue_device: String::new(),
            recordings_folder: String::new(),
            piano_window_open: false,
        }
    }
}

impl InterfaceState {
    /// A mutable handle to a deck's channel, or `None` for an out-of-range index —
    /// a bad index is a silent no-op (the store never panics on a caller's index,
    /// matching the `commands.rs` trust boundary).
    fn deck_mut(&mut self, deck: usize) -> Option<&mut DeckSnap> {
        self.decks.get_mut(deck)
    }

    pub fn set_crossfade(&mut self, position: f32) {
        self.crossfade = position;
    }

    pub fn set_cue_mix(&mut self, position: f32) {
        self.cue_mix = position;
    }

    pub fn set_piano_window_open(&mut self, open: bool) {
        self.piano_window_open = open;
    }

    pub fn set_volume(&mut self, deck: usize, gain: f32) {
        if let Some(d) = self.deck_mut(deck) {
            d.volume = gain;
        }
    }

    pub fn set_eq(&mut self, deck: usize, band: EqBand, value: f32) {
        if let Some(d) = self.deck_mut(deck) {
            match band {
                EqBand::Low => d.eq.low = value,
                EqBand::Mid => d.eq.mid = value,
                EqBand::High => d.eq.high = value,
            }
        }
    }

    pub fn set_trim(&mut self, deck: usize, db: f32) {
        if let Some(d) = self.deck_mut(deck) {
            d.trim_db = db;
        }
    }

    pub fn set_cue(&mut self, deck: usize, on: bool) {
        if let Some(d) = self.deck_mut(deck) {
            d.cue = on;
        }
    }

    pub fn set_on_air(&mut self, deck: usize, on: bool) {
        if let Some(d) = self.deck_mut(deck) {
            d.on_air = on;
        }
    }

    /// Select a deck's Color FX. Records the kind AND the kind's rest amount —
    /// the engine's insert swap lands at rest (bypassed), so the store mirrors
    /// it in the same write (ADR-0020 phase C: one discrete command, no
    /// follow-up amount write whose absence leaves a stale knob in a snapshot).
    pub fn set_fx(&mut self, deck: usize, kind: FxKind) {
        if let Some(d) = self.deck_mut(deck) {
            d.fx.kind = Some(kind.into());
            d.fx.amount = kind.rest_position();
        }
    }

    pub fn set_fx_amount(&mut self, deck: usize, amount: f32) {
        if let Some(d) = self.deck_mut(deck) {
            d.fx.amount = amount;
        }
    }

    /// Remove a deck's Color FX (no effect selected); the knob parks at zero,
    /// like the frontend's `setFx(null)`.
    pub fn clear_fx(&mut self, deck: usize) {
        if let Some(d) = self.deck_mut(deck) {
            d.fx.kind = None;
            d.fx.amount = 0.0;
        }
    }

    pub fn set_model(&mut self, deck: usize, model: Option<String>) {
        if let Some(d) = self.deck_mut(deck) {
            d.model = model;
        }
    }

    pub fn set_playing(&mut self, deck: usize, playing: bool) {
        if let Some(d) = self.deck_mut(deck) {
            if d.playing != playing {
                // A transport transition is a stream discontinuity: held
                // note steering resets with it (ADR-0023) — the worker
                // clears its engine state on the play/stop commands, and the
                // store must never keep claiming steering the worker dropped.
                // Drum conditioning is deck config, not a held gesture
                // (issue #50): the mirror persists, and the steering service
                // re-asserts it to the worker on the play edge.
                d.notes = None;
            }
            d.playing = playing;
        }
    }

    /// Start the transport if it is stopped; returns whether THIS call
    /// started it. `deck_play`'s idempotence guard (ADR-0020 phase D): the
    /// store lock is the ordering, so a second tap racing the first one's
    /// round-trip can never re-arm the worker or reset the note steering —
    /// the job the webview's `playPendingRef` used to do with a local flag.
    pub fn start_transport(&mut self, deck: usize) -> bool {
        match self.deck_mut(deck) {
            Some(d) if !d.playing => {
                d.notes = None;
                d.playing = true;
                true
            }
            _ => false,
        }
    }

    /// Record which source the deck plays (M19; webview-written until the
    /// transport inverts, phase E).
    pub fn set_mode(&mut self, deck: usize, mode: PlayModeSnap) {
        if let Some(d) = self.deck_mut(deck) {
            d.mode = mode;
        }
    }

    pub fn set_track(&mut self, deck: usize, track: Option<TrackIdentitySnap>) {
        if let Some(d) = self.deck_mut(deck) {
            // The cue pads live and die with the track identity (phase D):
            // a different pressing gets fresh pads IN THE SAME WRITE, so no
            // snapshot can ever pair a new track with the previous track's
            // points (the stale window the webview's cuesSyncedRef gate used
            // to fence). A redundant push of the same identity keeps them.
            if track.is_none() {
                d.cues = Vec::new();
            } else if d.track != track {
                d.cues = vec![None; HOT_CUE_COUNT];
            }
            d.track = track;
        }
    }

    pub fn set_transport(&mut self, deck: usize, transport: Option<TransportSnap>) {
        if let Some(d) = self.deck_mut(deck) {
            d.transport = transport;
        }
    }

    pub fn set_loop_labels(&mut self, deck: usize, labels: Vec<Option<String>>) {
        if let Some(d) = self.deck_mut(deck) {
            d.loop_labels = labels;
        }
    }

    /// Add a text target at the clearest spawn slot (ADR-0020 phase B: the
    /// add semantics — trim, length cap, dup rule, target cap, spawn
    /// geometry — live here, one copy for UI, hardware, and MCP). Returns
    /// whether the pad changed.
    pub fn style_add_target(&mut self, deck: usize, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() || text.len() > crate::style::MAX_TARGET_TEXT {
            return false;
        }
        let Some(d) = self.deck_mut(deck) else { return false };
        if d.style_targets.len() >= crate::style::MAX_TARGETS
            || d.style_targets.iter().any(|t| t.text == text)
        {
            return false;
        }
        let existing: Vec<(f32, f32)> = d.style_targets.iter().map(|t| (t.x, t.y)).collect();
        let (x, y) = crate::style::spawn_position(&existing);
        d.style_targets.push(StyleTargetSnap {
            x,
            y,
            text: text.to_string(),
            sample: None,
        });
        d.style_selected.push(false);
        true
    }

    /// Add a sampled chip (ADR-0011): a session-only embedding id under a
    /// display label; same cap/spawn rules, dup keyed on the label.
    pub fn style_add_sample_target(&mut self, deck: usize, label: &str, sample: &str) -> bool {
        let label = label.trim();
        if label.is_empty() || sample.is_empty() {
            return false;
        }
        let Some(d) = self.deck_mut(deck) else { return false };
        if d.style_targets.len() >= crate::style::MAX_TARGETS
            || d.style_targets.iter().any(|t| t.text == label)
        {
            return false;
        }
        let existing: Vec<(f32, f32)> = d.style_targets.iter().map(|t| (t.x, t.y)).collect();
        let (x, y) = crate::style::spawn_position(&existing);
        d.style_targets.push(StyleTargetSnap {
            x,
            y,
            text: label.to_string(),
            sample: Some(sample.to_string()),
        });
        d.style_selected.push(false);
        true
    }

    /// Move a target (identified by its unique text) to a clamped position.
    pub fn style_move_target(&mut self, deck: usize, text: &str, x: f32, y: f32) {
        if let Some(d) = self.deck_mut(deck) {
            if let Some(t) = d.style_targets.iter_mut().find(|t| t.text == text) {
                t.x = crate::style::clamp01(x);
                t.y = crate::style::clamp01(y);
            }
        }
    }

    /// Remove a target and its selection entry.
    pub fn style_remove_target(&mut self, deck: usize, text: &str) {
        if let Some(d) = self.deck_mut(deck) {
            if let Some(index) = d.style_targets.iter().position(|t| t.text == text) {
                d.style_targets.remove(index);
                if index < d.style_selected.len() {
                    d.style_selected.remove(index);
                }
            }
        }
    }

    /// Rename a text target in place (position and selection kept). A rename
    /// that empties, overflows, collides, or touches a sampled chip (whose
    /// label names a captured moment, not a prompt) is rejected — the same
    /// quiet rule the webview's editor applied. Returns whether it renamed.
    pub fn style_rename_target(&mut self, deck: usize, from: &str, to: &str) -> bool {
        let to = to.trim();
        if to.is_empty() || to.len() > crate::style::MAX_TARGET_TEXT {
            return false;
        }
        let Some(d) = self.deck_mut(deck) else { return false };
        if to != from && d.style_targets.iter().any(|t| t.text == to) {
            return false;
        }
        match d.style_targets.iter_mut().find(|t| t.text == from) {
            Some(t) if t.sample.is_none() => {
                t.text = to.to_string();
                true
            }
            _ => false,
        }
    }

    /// Toggle a target in or out of the net selection (the blend mask the
    /// pad LEDs mirror).
    pub fn style_toggle_selection(&mut self, deck: usize, text: &str) {
        if let Some(d) = self.deck_mut(deck) {
            if let Some(index) = d.style_targets.iter().position(|t| t.text == text) {
                if index < d.style_selected.len() {
                    d.style_selected[index] = !d.style_selected[index];
                }
            }
        }
    }

    /// The tidy-up gesture: centre the cursor and fan the targets onto the
    /// spawn circle in order.
    pub fn style_fan_out(&mut self, deck: usize) {
        if let Some(d) = self.deck_mut(deck) {
            for (index, target) in d.style_targets.iter_mut().enumerate() {
                let (x, y) = crate::style::circle_slot(index);
                target.x = x;
                target.y = y;
            }
            d.cursor = PadPointSnap { x: 0.5, y: 0.5 };
        }
    }

    /// Replace the pad wholesale (a preset load, an MCP arrangement): text
    /// targets only, selection cleared, cursor set. Invalid entries are
    /// dropped at the trust boundary before this is called.
    pub fn style_apply_preset(
        &mut self,
        deck: usize,
        targets: Vec<StyleTargetSnap>,
        cursor: PadPointSnap,
    ) {
        if let Some(d) = self.deck_mut(deck) {
            let count = targets.len().min(crate::style::MAX_TARGETS);
            d.style_targets = targets.into_iter().take(count).collect();
            d.style_selected = vec![false; count];
            d.cursor = PadPointSnap {
                x: crate::style::clamp01(cursor.x),
                y: crate::style::clamp01(cursor.y),
            };
        }
    }

    /// Set one hot-cue pad's point in track seconds, or clear it (`None`). A no-track
    /// deck (empty cue vec) or an out-of-range pad is a no-op — the MCP tool validates
    /// and reports first.
    pub fn set_cue_point(&mut self, deck: usize, index: usize, seconds: Option<f64>) {
        if let Some(d) = self.deck_mut(deck) {
            if let Some(slot) = d.cues.get_mut(index) {
                *slot = seconds;
            }
        }
    }

    /// Set just the style-pad cursor (the blend point), leaving the targets.
    pub fn set_cursor(&mut self, deck: usize, cursor: PadPointSnap) {
        if let Some(d) = self.deck_mut(deck) {
            d.cursor = PadPointSnap {
                x: crate::style::clamp01(cursor.x),
                y: crate::style::clamp01(cursor.y),
            };
        }
    }

    /// Mirror the primed-off-air read-back (the transport-CUE LED state).
    pub fn set_primed(&mut self, deck: usize, primed: bool) {
        if let Some(d) = self.deck_mut(deck) {
            d.primed = primed;
        }
    }

    /// Record the performance-surface config (issue #48).
    pub fn set_performance(&mut self, deck: usize, perf: PerformanceSnap) {
        if let Some(d) = self.deck_mut(deck) {
            d.performance = perf;
        }
    }

    /// Replace a deck's note steering wholesale (`None` = unsteered) — full
    /// state, never a delta, the ADR-0023 idempotence rule.
    pub fn set_notes(&mut self, deck: usize, notes: Option<NoteSteeringSnap>) {
        if let Some(d) = self.deck_mut(deck) {
            d.notes = notes;
        }
    }

    pub fn set_drums_strength(&mut self, deck: usize, strength: f32) {
        if let Some(d) = self.deck_mut(deck) {
            d.drums_strength = strength;
        }
    }

    pub fn set_drums(&mut self, deck: usize, drums: Option<bool>) {
        if let Some(d) = self.deck_mut(deck) {
            d.drums = drums;
        }
    }

    /// Record a deck's live generation operating point (issue #84).
    pub fn set_generation(&mut self, deck: usize, generation: GenerationSnap) {
        if let Some(d) = self.deck_mut(deck) {
            d.generation = generation;
        }
    }

    /// Record a deck's live beat analysis (ADR-0025) — a measurement the
    /// shell's analysis thread writes; nothing forwards to the engine here
    /// (the thread drives the echo clock through the [`Host`] itself).
    pub fn set_analysis(&mut self, deck: usize, analysis: AnalysisSnap) {
        if let Some(d) = self.deck_mut(deck) {
            d.analysis = analysis;
        }
    }

    /// Record the worker's health from a status event: a crash sets `died`
    /// (until a reload begins), a model switch sets `switching`, and `ready`
    /// clears both — the same transitions the webview reducer derives from
    /// the identical events, so the two views cannot diverge. A dead or
    /// reloading worker drops its sample cache, so the sampled style chips
    /// (whose embeddings lived in that cache, ADR-0011) strip with it.
    pub fn set_worker_health(&mut self, deck: usize, died: bool, switching: bool) {
        if let Some(d) = self.deck_mut(deck) {
            d.worker_died = died;
            d.switching_model = switching;
            if died || switching {
                let keep: Vec<bool> = d.style_targets.iter().map(|t| t.sample.is_none()).collect();
                let mut kept = keep.iter();
                d.style_targets.retain(|_| *kept.next().unwrap_or(&true));
                let mut kept = keep.iter();
                d.style_selected.retain(|_| *kept.next().unwrap_or(&true));
            }
        }
    }

    /// Record the deck's hardware SHIFT held-state (the native translator is
    /// the origin; this is a plain shell-side write, not a mirror).
    pub fn set_shift_held(&mut self, deck: usize, held: bool) {
        if let Some(d) = self.deck_mut(deck) {
            d.shift_held = held;
        }
    }

    /// Record the shell recorder's state (active + the take's path).
    pub fn set_recording(&mut self, active: bool, path: Option<String>) {
        self.recording = RecordingSnap { active, path };
    }

    /// Record the chosen output devices (shell-persisted settings).
    pub fn set_output_devices(&mut self, main: String, cue: String) {
        self.main_device = main;
        self.cue_device = cue;
    }

    /// Record the recordings folder ("" = Downloads).
    pub fn set_recordings_folder(&mut self, folder: String) {
        self.recordings_folder = folder;
    }
}

/// The shell-level store: the locked [`InterfaceState`] plus the [`AppHandle`] used
/// to broadcast changes. Held in Tauri managed state for the app's lifetime so every
/// controller path (UI/MIDI commands today, MCP tools later) mutates the one copy.
/// An in-process store-change listener (see [`InterfaceStore::watch`]).
type StoreWatcher = Box<dyn Fn(&InterfaceState) + Send + Sync>;

pub struct InterfaceStore {
    state: Mutex<InterfaceState>,
    /// The ordered publication queue: snapshots are enqueued UNDER the state
    /// lock — so queue order IS mutation order — and drained by the single
    /// publisher thread, which emits to the webview and runs the watcher
    /// fan-out. Publishing after the lock dropped (the old shape) let two
    /// mutating threads invert: a snapshot cloned before a change could be
    /// emitted after it, and the gate-free projection adopted the stale one
    /// (the play-button-lights-late bug — `deck_play` on the main thread
    /// racing a streaming deck's analysis tick).
    publish: mpsc::Sender<InterfaceState>,
    /// In-process change listeners (the native LED painter, ADR-0031), called
    /// with each published snapshot in mutation order, on the publisher
    /// thread — the Rust-side equivalent of the webview's `store://changed`
    /// subscription, without a serde round-trip.
    watchers: Arc<Mutex<Vec<StoreWatcher>>>,
}

impl InterfaceStore {
    pub fn new(app: AppHandle) -> Self {
        Self::with_emitter(move |snapshot| {
            let _ = app.emit(STORE_CHANGED_EVENT, snapshot);
        })
    }

    /// The store with a custom webview emitter — the seam the ordering test
    /// uses (a real `AppHandle` needs a running Tauri app). Spawns the
    /// publisher thread; it lives as long as the store (the channel
    /// disconnects when the store drops, and the thread exits with it).
    fn with_emitter(emit: impl Fn(&InterfaceState) + Send + 'static) -> Self {
        let watchers: Arc<Mutex<Vec<StoreWatcher>>> = Arc::new(Mutex::new(Vec::new()));
        let (publish, inbox) = mpsc::channel::<InterfaceState>();
        {
            let watchers = Arc::clone(&watchers);
            std::thread::Builder::new()
                .name("lsdj-store-publish".into())
                .spawn(move || {
                    while let Ok(snapshot) = inbox.recv() {
                        emit(&snapshot);
                        for watcher in
                            watchers.lock().unwrap_or_else(|p| p.into_inner()).iter()
                        {
                            watcher(&snapshot);
                        }
                    }
                })
                .expect("failed to spawn lsdj store publisher thread");
        }
        InterfaceStore {
            state: Mutex::new(InterfaceState::default()),
            publish,
            watchers,
        }
    }

    /// Register an in-process change listener (never unregistered — watchers
    /// live as long as the app, like the managed state that owns them).
    pub fn watch(&self, watcher: impl Fn(&InterfaceState) + Send + Sync + 'static) {
        self.watchers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Box::new(watcher));
    }

    /// The current snapshot — what the webview hydrates from on mount (`store_snapshot`).
    pub fn snapshot(&self) -> InterfaceState {
        self.lock().clone()
    }

    /// Apply a mutation under the lock, then queue the fresh snapshot for
    /// publication. The enqueue happens UNDER the lock so publication order is
    /// mutation order across threads (commands on the main thread, the
    /// analysis ticks on theirs); serialisation and the watcher fan-out stay
    /// off the mutex — they run on the publisher thread. A poisoned lock is
    /// recovered (a panic in another holder must not wedge every later
    /// control).
    ///
    /// A mutation that leaves the state unchanged emits nothing — many mirror writers
    /// re-push identical values (a boot replay, a `track?.cues` reference change with
    /// the same points), and a redundant `store://changed` would re-render every
    /// projection consumer for no reason.
    fn mutate(&self, f: impl FnOnce(&mut InterfaceState)) {
        let mut state = self.lock();
        let before = state.clone();
        f(&mut state);
        if *state == before {
            return;
        }
        let _ = self.publish.send(state.clone());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InterfaceState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn set_crossfade(&self, position: f32) {
        self.mutate(|s| s.set_crossfade(position));
    }

    pub fn set_cue_mix(&self, position: f32) {
        self.mutate(|s| s.set_cue_mix(position));
    }

    pub fn set_piano_window_open(&self, open: bool) {
        self.mutate(|s| s.set_piano_window_open(open));
    }

    pub fn set_volume(&self, deck: usize, gain: f32) {
        self.mutate(|s| s.set_volume(deck, gain));
    }

    pub fn set_eq(&self, deck: usize, band: EqBand, value: f32) {
        self.mutate(|s| s.set_eq(deck, band, value));
    }

    pub fn set_trim(&self, deck: usize, db: f32) {
        self.mutate(|s| s.set_trim(deck, db));
    }

    pub fn set_cue(&self, deck: usize, on: bool) {
        self.mutate(|s| s.set_cue(deck, on));
    }

    pub fn set_on_air(&self, deck: usize, on: bool) {
        self.mutate(|s| s.set_on_air(deck, on));
    }

    pub fn set_fx(&self, deck: usize, kind: FxKind) {
        self.mutate(|s| s.set_fx(deck, kind));
    }

    pub fn set_fx_amount(&self, deck: usize, amount: f32) {
        self.mutate(|s| s.set_fx_amount(deck, amount));
    }

    pub fn clear_fx(&self, deck: usize) {
        self.mutate(|s| s.clear_fx(deck));
    }

    /// Mirror a realtime deck's model read-back. The webview derives it from
    /// worker status (`ready`/`model_loading`) and writes the current value up;
    /// the store holds it for MCP reads.
    pub fn set_deck_model(&self, deck: usize, model: Option<String>) {
        self.mutate(move |s| s.set_model(deck, model));
    }

    /// Set a realtime deck's transport. The store OWNS `playing` (ADR-0020): the
    /// `deck_play`/`deck_stop` commands write it for every controller (UI, MIDI,
    /// MCP), the sidecar status relay drops it when a worker dies or reloads, and
    /// the webview's button is a projection of this value — never a writer.
    pub fn set_playing(&self, deck: usize, playing: bool) {
        self.mutate(move |s| s.set_playing(deck, playing));
    }

    /// Start the transport if stopped; returns whether THIS call started it
    /// (the `deck_play` idempotence guard, phase D).
    pub fn start_transport(&self, deck: usize) -> bool {
        let mut started = false;
        self.mutate(|s| started = s.start_transport(deck));
        started
    }

    /// Record which source the deck plays (M19; the webview's load flow
    /// writes it until the transport inverts).
    pub fn set_deck_mode(&self, deck: usize, mode: PlayModeSnap) {
        self.mutate(move |s| s.set_mode(deck, mode));
    }

    /// Mirror the loaded track's identity (a playback-deck read-back). The webview
    /// writes it on load / unload; `None` clears it.
    pub fn set_deck_track(&self, deck: usize, track: Option<TrackIdentitySnap>) {
        self.mutate(move |s| s.set_track(deck, track));
    }

    /// Mirror a playback deck's live transport (playhead / rate / loop). The webview
    /// owns the read-back and writes the current value up at a throttled cadence;
    /// `None` clears it on unload / a realtime deck.
    pub fn set_deck_transport(&self, deck: usize, transport: Option<TransportSnap>) {
        self.mutate(move |s| s.set_transport(deck, transport));
    }

    /// Mirror the freeze/sample loop-slot labels (a read-back the webview writes up
    /// when its slots change).
    pub fn set_deck_loop_labels(&self, deck: usize, labels: Vec<Option<String>>) {
        self.mutate(move |s| s.set_loop_labels(deck, labels));
    }

    /// The style-pad intents (ADR-0020 phase B): one semantic surface for
    /// the UI, the hardware, and MCP — the webview projects the result.
    pub fn style_add_target(&self, deck: usize, text: &str) -> bool {
        let mut added = false;
        self.mutate(|s| added = s.style_add_target(deck, text));
        added
    }

    pub fn style_add_sample_target(&self, deck: usize, label: &str, sample: &str) -> bool {
        let mut added = false;
        self.mutate(|s| added = s.style_add_sample_target(deck, label, sample));
        added
    }

    pub fn style_move_target(&self, deck: usize, text: &str, x: f32, y: f32) {
        self.mutate(|s| s.style_move_target(deck, text, x, y));
    }

    pub fn style_remove_target(&self, deck: usize, text: &str) {
        self.mutate(|s| s.style_remove_target(deck, text));
    }

    pub fn style_rename_target(&self, deck: usize, from: &str, to: &str) -> bool {
        let mut renamed = false;
        self.mutate(|s| renamed = s.style_rename_target(deck, from, to));
        renamed
    }

    pub fn style_toggle_selection(&self, deck: usize, text: &str) {
        self.mutate(|s| s.style_toggle_selection(deck, text));
    }

    pub fn style_fan_out(&self, deck: usize) {
        self.mutate(|s| s.style_fan_out(deck));
    }

    pub fn style_apply_preset(
        &self,
        deck: usize,
        targets: Vec<StyleTargetSnap>,
        cursor: PadPointSnap,
    ) {
        self.mutate(move |s| s.style_apply_preset(deck, targets, cursor));
    }

    /// Set one hot-cue pad's point (MCP `set_hot_cue` / `clear_hot_cue`). The webview
    /// adopts the change and re-renders the pad; jump stays a transport seek.
    pub fn set_deck_cue(&self, deck: usize, index: usize, seconds: Option<f64>) {
        self.mutate(move |s| s.set_cue_point(deck, index, seconds));
    }

    /// Set just the style-pad cursor (MCP `set_style_cursor`). `DeckColumn` adopts it
    /// and re-pushes the blended prompt to the worker.
    pub fn set_deck_cursor(&self, deck: usize, cursor: PadPointSnap) {
        self.mutate(move |s| s.set_cursor(deck, cursor));
    }

    /// Mirror the primed-off-air read-back (the transport-CUE LED state).
    pub fn set_deck_primed(&self, deck: usize, primed: bool) {
        self.mutate(move |s| s.set_primed(deck, primed));
    }

    /// Record a deck's performance-surface config (written by the shell
    /// note-steering service — UI and hardware both go through it).
    pub fn set_deck_performance(&self, deck: usize, perf: PerformanceSnap) {
        self.mutate(move |s| s.set_performance(deck, perf));
    }

    /// Replace a deck's note steering (UI/MIDI writes it up; MCP `set_notes` writes
    /// it for the webview to adopt and drive the worker — ADR-0023 over ADR-0020's
    /// projection). `None` = unsteered.
    pub fn set_deck_notes(&self, deck: usize, notes: Option<NoteSteeringSnap>) {
        self.mutate(move |s| s.set_notes(deck, notes));
    }

    /// Set a deck's drum conditioning tri-state (`None` = the model decides).
    pub fn set_deck_drums(&self, deck: usize, drums: Option<bool>) {
        self.mutate(move |s| s.set_drums(deck, drums));
    }

    /// Set a deck's drum-conditioning strength (issue #50): the `cfg_drums`
    /// guidance scale mirrored for the webview slider.
    pub fn set_deck_drums_strength(&self, deck: usize, strength: f32) {
        self.mutate(move |s| s.set_drums_strength(deck, strength));
    }

    /// Set a deck's live generation operating point (issue #84): the tunable
    /// sampling/guidance params mirrored for the drawer sliders.
    pub fn set_deck_generation(&self, deck: usize, generation: GenerationSnap) {
        self.mutate(move |s| s.set_generation(deck, generation));
    }

    /// Record a deck's live beat analysis (ADR-0025) — written by the shell's
    /// analysis thread at the estimate cadence; the no-change suppression in
    /// [`InterfaceStore::mutate`] keeps a held (or blank) reading silent.
    pub fn set_analysis(&self, deck: usize, analysis: AnalysisSnap) {
        self.mutate(move |s| s.set_analysis(deck, analysis));
    }

    /// Record the worker's health from the status relay (crash / model
    /// switch / ready).
    pub fn set_worker_health(&self, deck: usize, died: bool, switching: bool) {
        self.mutate(move |s| s.set_worker_health(deck, died, switching));
    }

    /// Record a deck's hardware SHIFT held-state (native translator origin).
    pub fn set_deck_shift(&self, deck: usize, held: bool) {
        self.mutate(move |s| s.set_shift_held(deck, held));
    }

    /// Record the shell recorder's state (the recording commands write it).
    pub fn set_recording(&self, active: bool, path: Option<String>) {
        self.mutate(move |s| s.set_recording(active, path));
    }

    /// Record the chosen output devices (the device commands write it after
    /// a successful switch; boot hydration seeds it from the settings file).
    pub fn set_output_devices(&self, main: String, cue: String) {
        self.mutate(move |s| s.set_output_devices(main, cue));
    }

    /// Record the recordings folder ("" = Downloads).
    pub fn set_recordings_folder(&self, folder: String) {
        self.mutate(move |s| s.set_recordings_folder(folder));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_one_channel_per_deck_at_neutral() {
        let state = InterfaceState::default();
        assert_eq!(state.decks.len(), DECK_COUNT);
        assert_eq!(state.crossfade, 0.5);
        assert_eq!(state.cue_mix, 0.5);
        for deck in &state.decks {
            assert_eq!(deck.volume, 1.0);
            assert_eq!(deck.eq, EqSnap { low: 0.5, mid: 0.5, high: 0.5 });
            assert!(deck.on_air);
            assert!(!deck.cue);
            assert_eq!(deck.fx.kind, None);
            assert_eq!(deck.model, None);
            assert!(!deck.playing);
            assert!(deck.cues.is_empty());
            assert_eq!(deck.track, None);
            assert_eq!(deck.transport, None);
            assert!(deck.loop_labels.is_empty());
            assert!(deck.style_targets.is_empty());
            assert_eq!(deck.cursor, PadPointSnap { x: 0.5, y: 0.5 });
            assert_eq!(deck.notes, None);
            assert_eq!(deck.drums, None);
            assert_eq!(deck.drums_strength, DEFAULT_DRUM_STRENGTH);
            assert_eq!(deck.generation, GenerationSnap::default());
        }
    }

    #[test]
    fn generation_params_default_to_the_reference_baseline() {
        let generation = GenerationSnap::default();
        assert_eq!(generation.temperature, DEFAULT_TEMPERATURE);
        assert_eq!(generation.top_k, DEFAULT_TOP_K);
        assert_eq!(generation.cfg_musiccoca, DEFAULT_CFG_MUSICCOCA);
        assert_eq!(generation.cfg_notes, DEFAULT_CFG_NOTES);
    }

    #[test]
    fn generation_is_mirrored_per_deck() {
        let mut state = InterfaceState::default();
        let tuned = GenerationSnap {
            temperature: 0.7,
            top_k: 20,
            cfg_musiccoca: 3.0,
            cfg_notes: 1.0,
        };
        state.set_generation(0, tuned);
        assert_eq!(state.decks[0].generation, tuned);
        // The other deck keeps the baseline.
        assert_eq!(state.decks[1].generation, GenerationSnap::default());
    }

    #[test]
    fn generation_clamped_pins_every_field_to_its_exposed_range() {
        // Over the top: each field clamps to its own max, cfg shared at 5.
        let over = GenerationSnap {
            temperature: 9.0,
            top_k: 9999,
            cfg_musiccoca: 12.0,
            cfg_notes: 7.5,
        }
        .clamped();
        assert_eq!(over.temperature, GEN_TEMPERATURE_MAX);
        assert_eq!(over.top_k, GEN_TOP_K_MAX);
        assert_eq!(over.cfg_musiccoca, GEN_CFG_MAX);
        assert_eq!(over.cfg_notes, GEN_CFG_MAX);
        // Under the floor: temperature floors off zero, top-k off zero, cfg at 0.
        let under = GenerationSnap {
            temperature: 0.0,
            top_k: 0,
            cfg_musiccoca: -3.0,
            cfg_notes: -1.0,
        }
        .clamped();
        assert_eq!(under.temperature, GEN_TEMPERATURE_MIN);
        assert_eq!(under.top_k, GEN_TOP_K_MIN);
        assert_eq!(under.cfg_musiccoca, GEN_CFG_MIN);
        assert_eq!(under.cfg_notes, GEN_CFG_MIN);
        // In range: untouched, and the baseline is already within range.
        assert_eq!(GenerationSnap::default().clamped(), GenerationSnap::default());
    }

    #[test]
    fn generation_patch_merges_only_its_set_fields() {
        let base = GenerationSnap {
            temperature: 0.8,
            top_k: 30,
            cfg_musiccoca: 2.0,
            cfg_notes: 3.0,
        };
        let merged = GenerationPatch { top_k: Some(64), ..GenerationPatch::default() }.apply(base);
        // Only top-k changed; the other three ride along unchanged.
        assert_eq!(
            merged,
            GenerationSnap {
                temperature: 0.8,
                top_k: 64,
                cfg_musiccoca: 2.0,
                cfg_notes: 3.0,
            }
        );
    }

    #[test]
    fn generation_field_reset_targets_the_shell_baseline() {
        // A reset patch names one field and carries the shell's own default —
        // the frontend never supplies the value.
        let base = GenerationSnap {
            temperature: 0.2,
            top_k: 5,
            cfg_musiccoca: 0.5,
            cfg_notes: 0.5,
        };
        let reset = GenerationField::CfgMusiccoca.reset_patch().apply(base);
        assert_eq!(reset.cfg_musiccoca, DEFAULT_CFG_MUSICCOCA);
        // The others are untouched by a single-field reset.
        assert_eq!(reset.temperature, 0.2);
        assert_eq!(reset.top_k, 5);
    }

    #[test]
    fn note_and_drum_steering_are_mirrored_per_deck() {
        let mut state = InterfaceState::default();
        state.set_notes(
            0,
            Some(NoteSteeringSnap {
                pitches: vec![60, 64, 67],
                mode: NoteModeSnap::Chord,
            }),
        );
        state.set_drums(0, Some(false));
        state.set_drums_strength(0, 3.0);
        assert_eq!(state.decks[0].drums_strength, 3.0);
        assert_eq!(state.decks[1].drums_strength, DEFAULT_DRUM_STRENGTH);
        assert_eq!(state.decks[0].notes.as_ref().unwrap().pitches, vec![60, 64, 67]);
        assert_eq!(state.decks[0].drums, Some(false));
        // The other deck is untouched.
        assert_eq!(state.decks[1].notes, None);
        assert_eq!(state.decks[1].drums, None);
        // Clearing returns to unsteered.
        state.set_notes(0, None);
        state.set_drums(0, None);
        assert_eq!(state.decks[0].notes, None);
        assert_eq!(state.decks[0].drums, None);
    }

    #[test]
    fn analysis_is_blank_by_default_and_mirrored_per_deck() {
        let mut state = InterfaceState::default();
        assert_eq!(state.decks[0].analysis, AnalysisSnap::default());
        assert_eq!(state.decks[0].analysis.bpm, None);
        state.set_analysis(
            0,
            AnalysisSnap {
                bpm: Some(128.0),
                confidence: 0.62,
                live_beat: Some(LiveBeatSnap {
                    anchor_frame: 96_000.0,
                    bpm: 128.0,
                }),
                origin_frames: 48_000.0,
            },
        );
        assert_eq!(state.decks[0].analysis.bpm, Some(128.0));
        assert_eq!(
            state.decks[0].analysis.live_beat,
            Some(LiveBeatSnap {
                anchor_frame: 96_000.0,
                bpm: 128.0
            })
        );
        // The other deck is untouched, and an out-of-range deck is a no-op.
        assert_eq!(state.decks[1].analysis, AnalysisSnap::default());
        state.set_analysis(9, AnalysisSnap::default());
    }

    #[test]
    fn transport_transitions_reset_notes_but_keep_drum_conditioning() {
        let mut state = InterfaceState::default();
        state.set_playing(0, true);
        state.set_notes(
            0,
            Some(NoteSteeringSnap {
                pitches: vec![60],
                mode: NoteModeSnap::Onset,
            }),
        );
        state.set_drums(0, Some(false));
        state.set_drums_strength(0, 3.0);
        // Re-asserting the same transport state is not a discontinuity.
        state.set_playing(0, true);
        assert!(state.decks[0].notes.is_some());
        // A stop is: held note steering resets with the stream (ADR-0023),
        // but drum conditioning is deck config (issue #50) — mode AND strength
        // persist, and the steering service re-asserts them on the play edge.
        state.set_playing(0, false);
        assert_eq!(state.decks[0].notes, None);
        assert_eq!(state.decks[0].drums, Some(false));
        assert_eq!(state.decks[0].drums_strength, 3.0);
        // Notes set while stopped die at the next play — a fresh stream
        // starts unsteered, exactly like the worker's engine. Drums stick.
        state.set_notes(
            0,
            Some(NoteSteeringSnap {
                pitches: vec![62],
                mode: NoteModeSnap::Chord,
            }),
        );
        state.set_playing(0, true);
        assert_eq!(state.decks[0].notes, None);
        assert_eq!(state.decks[0].drums, Some(false));
    }

    #[test]
    fn style_add_spawns_on_the_circle_and_enforces_trim_dup_and_cap() {
        let mut state = InterfaceState::default();
        // Trim; the spawn slot comes from the geometry (empty pad → slot 0).
        assert!(state.style_add_target(0, "  dub  "));
        assert_eq!(state.decks[0].style_targets[0].text, "dub");
        let (x0, y0) = crate::style::circle_slot(0);
        assert_eq!(state.decks[0].style_targets[0].x, x0);
        assert_eq!(state.decks[0].style_targets[0].y, y0);
        // Selection grows in step, unselected.
        assert_eq!(state.decks[0].style_selected, vec![false]);
        // Duplicates and empties are rejected.
        assert!(!state.style_add_target(0, "dub"));
        assert!(!state.style_add_target(0, "   "));
        // The cap holds at MAX_TARGETS.
        for i in 0..crate::style::MAX_TARGETS {
            state.style_add_target(0, &format!("t{i}"));
        }
        assert_eq!(state.decks[0].style_targets.len(), crate::style::MAX_TARGETS);
        assert!(!state.style_add_target(0, "one too many"));
        // The other deck is untouched.
        assert!(state.decks[1].style_targets.is_empty());
    }

    #[test]
    fn style_move_clamps_and_remove_keeps_selection_aligned() {
        let mut state = InterfaceState::default();
        state.style_add_target(0, "a");
        state.style_add_target(0, "b");
        state.style_toggle_selection(0, "b");
        assert_eq!(state.decks[0].style_selected, vec![false, true]);
        // Move clamps into the unit square.
        state.style_move_target(0, "a", -0.5, 1.5);
        assert_eq!(state.decks[0].style_targets[0].x, 0.0);
        assert_eq!(state.decks[0].style_targets[0].y, 1.0);
        // Removing "a" keeps "b" selected — the mask tracks its target.
        state.style_remove_target(0, "a");
        assert_eq!(state.decks[0].style_targets.len(), 1);
        assert_eq!(state.decks[0].style_targets[0].text, "b");
        assert_eq!(state.decks[0].style_selected, vec![true]);
    }

    #[test]
    fn style_rename_keeps_position_and_rejects_collisions_and_sample_chips() {
        let mut state = InterfaceState::default();
        state.style_add_target(0, "dub");
        state.style_add_target(0, "punk");
        state.style_add_sample_target(0, "Deck B sample 1", "sample:b:1");
        let position = (state.decks[0].style_targets[0].x, state.decks[0].style_targets[0].y);
        // Rename keeps the position; collisions and empties are quiet no-ops.
        assert!(state.style_rename_target(0, "dub", "deep dub"));
        assert_eq!(state.decks[0].style_targets[0].text, "deep dub");
        assert_eq!(
            (state.decks[0].style_targets[0].x, state.decks[0].style_targets[0].y),
            position
        );
        assert!(!state.style_rename_target(0, "deep dub", "punk"));
        assert!(!state.style_rename_target(0, "punk", "  "));
        // A sampled chip's label names a captured moment — not renameable.
        assert!(!state.style_rename_target(0, "Deck B sample 1", "nice loop"));
    }

    #[test]
    fn style_fan_out_circles_the_targets_and_centres_the_cursor() {
        let mut state = InterfaceState::default();
        state.style_add_target(0, "a");
        state.style_add_target(0, "b");
        state.style_move_target(0, "a", 0.9, 0.9);
        state.set_cursor(0, PadPointSnap { x: 0.1, y: 0.1 });
        state.style_fan_out(0);
        let (x0, y0) = crate::style::circle_slot(0);
        let (x1, y1) = crate::style::circle_slot(1);
        assert_eq!(state.decks[0].style_targets[0].x, x0);
        assert_eq!(state.decks[0].style_targets[0].y, y0);
        assert_eq!(state.decks[0].style_targets[1].x, x1);
        assert_eq!(state.decks[0].style_targets[1].y, y1);
        assert_eq!(state.decks[0].cursor, PadPointSnap { x: 0.5, y: 0.5 });
    }

    #[test]
    fn style_apply_preset_replaces_wholesale_and_clears_selection() {
        let mut state = InterfaceState::default();
        state.style_add_target(0, "old");
        state.style_toggle_selection(0, "old");
        state.style_apply_preset(
            0,
            vec![StyleTargetSnap {
                x: 0.2,
                y: 0.8,
                text: "dub".to_string(),
                sample: None,
            }],
            PadPointSnap { x: 0.3, y: 0.4 },
        );
        assert_eq!(state.decks[0].style_targets.len(), 1);
        assert_eq!(state.decks[0].style_targets[0].text, "dub");
        assert_eq!(state.decks[0].style_selected, vec![false]);
        assert_eq!(state.decks[0].cursor, PadPointSnap { x: 0.3, y: 0.4 });
    }

    #[test]
    fn worker_death_strips_sampled_chips_and_their_selection() {
        let mut state = InterfaceState::default();
        state.style_add_target(0, "dub");
        state.style_add_sample_target(0, "Deck B sample 1", "sample:b:1");
        state.style_toggle_selection(0, "dub");
        state.style_toggle_selection(0, "Deck B sample 1");
        // The dying worker takes its embedding cache — the chip goes with it,
        // the text target (and its selection) survives.
        state.set_worker_health(0, true, false);
        assert_eq!(state.decks[0].style_targets.len(), 1);
        assert_eq!(state.decks[0].style_targets[0].text, "dub");
        assert_eq!(state.decks[0].style_selected, vec![true]);
        assert!(state.decks[0].worker_died);
    }

    #[test]
    fn loop_labels_are_mirrored_per_deck() {
        let mut state = InterfaceState::default();
        state.set_loop_labels(0, vec![Some("kick".to_string()), None]);
        assert_eq!(state.decks[0].loop_labels, vec![Some("kick".to_string()), None]);
        assert!(state.decks[1].loop_labels.is_empty());
    }

    #[test]
    fn track_identity_is_mirrored_and_cleared_per_deck() {
        let mut state = InterfaceState::default();
        state.set_track(
            0,
            Some(TrackIdentitySnap {
                title: "Take 1".to_string(),
                bpm: Some(128.0),
                duration_seconds: 180.0,
            }),
        );
        let track = state.decks[0].track.as_ref().unwrap();
        assert_eq!(track.title, "Take 1");
        assert_eq!(track.bpm, Some(128.0));
        assert_eq!(state.decks[1].track, None);
        // Unload clears it.
        state.set_track(0, None);
        assert_eq!(state.decks[0].track, None);
    }

    #[test]
    fn transport_is_mirrored_and_cleared_per_deck() {
        let mut state = InterfaceState::default();
        state.set_transport(
            0,
            Some(TransportSnap {
                playhead_seconds: 12.5,
                rate: 1.08,
                loop_region: Some(LoopRegionSnap {
                    start_seconds: 8.0,
                    end_seconds: 16.0,
                }),
                playing: true,
            }),
        );
        let transport = state.decks[0].transport.as_ref().unwrap();
        assert_eq!(transport.playhead_seconds, 12.5);
        assert_eq!(transport.rate, 1.08);
        assert_eq!(transport.loop_region.unwrap().end_seconds, 16.0);
        assert!(transport.playing);
        // The other deck is untouched.
        assert_eq!(state.decks[1].transport, None);
        // Unload / realtime clears it.
        state.set_transport(0, None);
        assert_eq!(state.decks[0].transport, None);
    }

    #[test]
    fn realtime_read_backs_are_mirrored_per_deck() {
        let mut state = InterfaceState::default();
        state.set_model(0, Some("mrt2_base".to_string()));
        state.set_playing(0, true);
        assert_eq!(state.decks[0].model.as_deref(), Some("mrt2_base"));
        assert!(state.decks[0].playing);
        // The other deck is untouched.
        assert_eq!(state.decks[1].model, None);
        assert!(!state.decks[1].playing);
    }

    fn pressing(title: &str) -> TrackIdentitySnap {
        TrackIdentitySnap {
            title: title.to_string(),
            bpm: Some(128.0),
            duration_seconds: 120.0,
        }
    }

    #[test]
    fn cue_pads_live_and_die_with_the_track_identity() {
        let mut state = InterfaceState::default();
        // A load opens a fresh bank in the same write as the identity.
        state.set_track(0, Some(pressing("Warehouse Anthem")));
        assert_eq!(state.decks[0].cues, vec![None; HOT_CUE_COUNT]);
        state.set_cue_point(0, 1, Some(12.5));
        // A redundant identity push (the webview mirror re-fires) keeps them…
        state.set_track(0, Some(pressing("Warehouse Anthem")));
        assert_eq!(state.decks[0].cues[1], Some(12.5));
        // …a DIFFERENT pressing resets them in the same write (no snapshot can
        // pair the new track with the old points)…
        state.set_track(0, Some(pressing("Second Pressing")));
        assert_eq!(state.decks[0].cues, vec![None; HOT_CUE_COUNT]);
        // …and an unload drops the bank.
        state.set_track(0, None);
        assert!(state.decks[0].cues.is_empty());
        assert!(state.decks[1].cues.is_empty());
    }

    #[test]
    fn set_cue_point_sets_or_clears_one_pad_in_range() {
        let mut state = InterfaceState::default();
        // A no-track deck (empty cue vec) is a silent no-op — the MCP tool reports it.
        state.set_cue_point(0, 0, Some(4.0));
        assert!(state.decks[0].cues.is_empty());
        // With a loaded track, set one pad and clear it; the neighbours are untouched.
        state.set_track(0, Some(pressing("Warehouse Anthem")));
        state.set_cue_point(0, 1, Some(12.5));
        assert_eq!(state.decks[0].cues[1], Some(12.5));
        state.set_cue_point(0, 1, None);
        assert_eq!(state.decks[0].cues, vec![None; HOT_CUE_COUNT]);
        // An out-of-range pad on a loaded deck is a no-op too.
        state.set_cue_point(0, 99, Some(1.0));
        assert_eq!(state.decks[0].cues, vec![None; HOT_CUE_COUNT]);
    }

    #[test]
    fn start_transport_starts_once_and_records_the_mode() {
        let mut state = InterfaceState::default();
        assert_eq!(state.decks[0].mode, PlayModeSnap::Realtime);
        state.set_mode(0, PlayModeSnap::Playback);
        assert_eq!(state.decks[0].mode, PlayModeSnap::Playback);
        assert_eq!(state.decks[1].mode, PlayModeSnap::Realtime);
        // The idempotence guard: only the first start reports true — a second
        // tap must not re-arm the worker or reset held steering.
        assert!(state.start_transport(1));
        assert!(state.decks[1].playing);
        assert!(!state.start_transport(1));
        state.set_playing(1, false);
        assert!(state.start_transport(1));
    }

    #[test]
    fn set_cursor_moves_the_blend_point_leaving_targets() {
        let mut state = InterfaceState::default();
        state.style_add_target(0, "a");
        state.set_cursor(0, PadPointSnap { x: 0.7, y: 0.3 });
        assert_eq!(state.decks[0].cursor, PadPointSnap { x: 0.7, y: 0.3 });
        // The targets are left exactly as they were.
        assert_eq!(state.decks[0].style_targets.len(), 1);
        assert_eq!(state.decks[0].style_targets[0].text, "a");
        // And the cursor clamps into the unit square.
        state.set_cursor(0, PadPointSnap { x: -1.0, y: 2.0 });
        assert_eq!(state.decks[0].cursor, PadPointSnap { x: 0.0, y: 1.0 });
    }

    #[test]
    fn mixer_mutations_record_per_deck() {
        let mut state = InterfaceState::default();
        state.set_crossfade(0.25);
        state.set_cue_mix(0.0);
        state.set_volume(1, 0.6);
        state.set_eq(0, EqBand::Low, 0.1);
        state.set_eq(0, EqBand::High, 0.9);
        state.set_trim(1, -3.0);
        state.set_cue(0, true);
        state.set_on_air(1, false);

        assert_eq!(state.crossfade, 0.25);
        assert_eq!(state.cue_mix, 0.0);
        assert_eq!(state.decks[1].volume, 0.6);
        assert_eq!(state.decks[0].eq.low, 0.1);
        assert_eq!(state.decks[0].eq.high, 0.9);
        // The mid band is untouched by a low/high write.
        assert_eq!(state.decks[0].eq.mid, 0.5);
        assert_eq!(state.decks[1].trim_db, -3.0);
        assert!(state.decks[0].cue);
        assert!(!state.decks[1].on_air);
    }

    #[test]
    fn fx_select_parks_the_amount_at_rest_and_clear_at_zero() {
        // Phase C: set_fx records kind + the kind's rest amount in ONE write —
        // the engine's insert swap lands at rest, and a snapshot between the
        // old two-write sequence must never pair the new kind with the stale
        // amount. clear_fx parks at zero, like the webview's setFx(null).
        let mut state = InterfaceState::default();
        state.set_fx(0, FxKind::DubEcho);
        state.set_fx_amount(0, 0.7);
        assert_eq!(state.decks[0].fx.kind, Some(FxKindSnap::DubEcho));
        assert_eq!(state.decks[0].fx.amount, 0.7);

        // A kind swap lands at the new kind's rest (filter is bipolar: 0.5).
        state.set_fx(0, FxKind::Filter);
        assert_eq!(state.decks[0].fx.kind, Some(FxKindSnap::Filter));
        assert_eq!(state.decks[0].fx.amount, 0.5);

        state.clear_fx(0);
        assert_eq!(state.decks[0].fx.kind, None);
        assert_eq!(state.decks[0].fx.amount, 0.0);
    }

    #[test]
    fn out_of_range_deck_is_a_silent_no_op() {
        let mut state = InterfaceState::default();
        // Bad index must not panic and must not touch a valid deck.
        state.set_volume(DECK_COUNT, 0.0);
        state.set_eq(99, EqBand::Mid, 0.0);
        state.set_fx(7, FxKind::Crush);
        assert_eq!(state.decks[0], DeckSnap::default());
        assert_eq!(state.decks[DECK_COUNT - 1], DeckSnap::default());
    }

    #[test]
    fn snapshot_serialises_camelcase_for_the_webview() {
        let mut state = InterfaceState::default();
        state.set_fx(0, FxKind::DubEcho);
        let json = serde_json::to_string(&state).unwrap();
        // The projection reads these keys; lock the wire shape.
        assert!(json.contains("\"cueMix\""));
        assert!(json.contains("\"onAir\""));
        assert!(json.contains("\"trimDb\""));
        assert!(json.contains("\"dubEcho\""));
    }

    /// Concurrent mutators must never publish a stale snapshot after a fresher
    /// one: the enqueue happens under the state lock, so the publisher thread
    /// sees mutation order. The old emit-after-unlock shape let a streaming
    /// deck's analysis tick overwrite `deck_play`'s fresh transport in the
    /// projection (the play-button-lights-late bug).
    #[test]
    fn snapshots_publish_in_mutation_order_across_threads() {
        const WRITES: usize = 200;
        let store = InterfaceStore::with_emitter(|_| {});
        let (tx, rx) = mpsc::channel();
        store.watch(move |state| {
            let _ = tx.send((state.decks[0].volume, state.decks[1].volume));
        });
        std::thread::scope(|s| {
            for deck in 0..2 {
                let store = &store;
                s.spawn(move || {
                    // Descending from just below the 1.0 default, so each
                    // deck's column is monotonic from the very first snapshot.
                    for i in 0..WRITES {
                        store.set_volume(deck, (WRITES - 1 - i) as f32 / WRITES as f32);
                    }
                });
            }
        });
        // Disconnect the queue so the publisher drains, exits, and drops the
        // watcher (and with it the channel) — `rx` then ends deterministically.
        drop(store);
        let published: Vec<(f32, f32)> = rx.iter().collect();
        assert_eq!(published.len(), 2 * WRITES);
        // Each deck's writes were monotonic, so any published regression means
        // a stale snapshot overtook a fresher one.
        for pair in published.windows(2) {
            assert!(
                pair[1].0 <= pair[0].0 && pair[1].1 <= pair[0].1,
                "stale snapshot published after a fresher one: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(*published.last().unwrap(), (0.0, 0.0));
    }
}
