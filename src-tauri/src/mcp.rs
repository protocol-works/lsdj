//! In-process MCP server (ADR-0020 Phase 2): an external AI agent (Claude Desktop /
//! Claude Code) as a co-DJ. Hosted inside the Tauri process, **always on**,
//! **loopback-only**, guarded by a **per-session bearer token**. Tools
//! mutate the one interface store (the same validated path UI and MIDI take), so an
//! agent's move is reflected on screen (the bidirectional projection); resources
//! read the store. A generation tool proxies the loopback generation server to
//! compose audio into the samples library, where the folder watcher surfaces it.
//!
//! Mirrors the generation server's spawn/supervise/shutdown discipline
//! ([`crate::generation`]): a disabled or failed start just leaves the endpoint
//! unadvertised (`port() == None`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::extract::Request;
use axum::http::{header::AUTHORIZATION, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, ListResourcesResult, PaginatedRequestParams, RawResource,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::tower::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, RoleServer, ServerHandler};
use serde::Deserialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tokio_util::sync::CancellationToken;

use crate::commands::{valid_deck, DrumModeArg, EqBandArg, FxKindArg};
use crate::generation::GenerationServer;
use crate::samples::{NewSample, SampleLibrary};
use crate::sidecar::Sidecars;
use crate::songs::{NewSong, SongLibrary};
use crate::midi::notes::NoteSteering;
use crate::store::{InterfaceStore, NoteModeSnap, PadPointSnap, PlayModeSnap, StyleTargetSnap};
use lsdj_engine::host::Host;
use lsdj_engine::FxKind;

/// Ceiling for a ramped mixer move (`ramp_ms` on set_crossfade / set_volume):
/// long enough for any show-length blend, short enough that a typo'd value
/// can't leave a fader gliding for minutes.
const MAX_RAMP_MS: f32 = 60_000.0;

/// The MCP request handler. Holds the [`AppHandle`] so a tool reaches the same
/// Tauri-managed state (`Host`, `InterfaceStore`, sidecars) the IPC commands drive —
/// no second copy of the control surface.
#[derive(Clone)]
pub struct McpHandler {
    app: AppHandle,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CrossfadeArgs {
    /// Crossfader position, 0 = deck A, 1 = deck B.
    position: f32,
    /// Optional glide time in milliseconds: the engine walks the fader there as
    /// a click-free linear blend (equal-power the whole way). Omit or 0 for an
    /// instant move. The UI fader shows the destination while the audio glides.
    ramp_ms: Option<f32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeckGainArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Channel volume, 0..1.
    gain: f32,
    /// Optional glide time in milliseconds (engine-side linear fade, click-free).
    /// Omit or 0 for an instant move.
    ramp_ms: Option<f32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeckEqArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// EQ band.
    band: EqBandArg,
    /// EQ amount, 0..1 (0.5 = flat).
    value: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CueMixArgs {
    /// Headphone cue/master blend, 0 = cue only, 1 = master.
    position: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeckFxArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Color FX kind.
    kind: FxKindArg,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeckArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct FxAmountArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Color FX amount/intensity, 0..1.
    amount: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TrimArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Channel trim in dB (0 = unity).
    db: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CueArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Headphone-cue (PFL) tap on/off.
    on: bool,
}

/// The pad-generation engines `generate_sample` exposes: Stable Audio 3 `sfx`/`music`
/// (via `/api/generate`), and `magenta` (the Magenta pad renderer, M18, via
/// `/api/render`). All write to the *samples* library; SA3's long-form `track` is a
/// separate tool (the songs library).
#[derive(Debug, Clone, Copy, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum SampleEngine {
    Sfx,
    Music,
    Magenta,
}

impl SampleEngine {
    /// The wire value: an SA3 `/api/generate` kind, or `"magenta"` which
    /// [`McpHandler::generate_clip`] routes to `/api/render` instead.
    fn as_str(self) -> &'static str {
        match self {
            SampleEngine::Sfx => "sfx",
            SampleEngine::Music => "music",
            SampleEngine::Magenta => "magenta",
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GenerateTrackArgs {
    /// Deck index to load the finished track onto: 0 = A, 1 = B.
    deck: usize,
    /// Text prompt describing the track to generate.
    prompt: String,
    /// Length in seconds (the server caps tracks at 380 s).
    seconds: f32,
    /// Optional LoRA adapter stack (max 4) applied to the generation — names
    /// from list_loras; track generations ride the "medium" base DiT.
    loras: Option<Vec<LoraArg>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GenerateSampleArgs {
    /// Text prompt describing the sound to generate.
    prompt: String,
    /// Clip length in seconds (the server validates the range per engine).
    seconds: f32,
    /// Engine: "sfx" / "music" (Stable Audio 3), or "magenta" (the Magenta renderer).
    kind: SampleEngine,
    /// Whether the clip plays once (a one-shot) instead of looping. Defaults to loop.
    #[serde(default)]
    one_shot: bool,
    /// Optional LoRA adapter stack (max 4) — names from list_loras; sfx/music
    /// ride the "small" base DiT. Not supported by the "magenta" engine.
    loras: Option<Vec<LoraArg>>,
}

/// One LoRA adapter riding a generation — mirrors the webview generator's
/// `LoraChoice` and the server's `loras[]` contract (ADR-0028).
#[derive(Debug, Clone, serde::Serialize, Deserialize, schemars::JsonSchema)]
struct LoraArg {
    /// Adapter name from list_loras (`<base>/<slug>`).
    name: String,
    /// Blend strength, 0-4 (0 = bit-exact bypass, ~1 = as trained). Defaults to 1.
    #[serde(default = "default_lora_strength")]
    strength: f32,
}

fn default_lora_strength() -> f32 {
    1.0
}

/// The `/api/generate` request body, matching the generation server's contract
/// (`prompt`/`seconds`/`kind`). Pulled out so the wire shape is unit-testable. `kind`
/// is the wire string (`sfx`/`music`/`track`).
fn generate_request_body(
    prompt: &str,
    seconds: f32,
    kind: &str,
    loras: &[LoraArg],
) -> serde_json::Value {
    let mut body = json!({ "prompt": prompt, "seconds": seconds, "kind": kind });
    if !loras.is_empty() {
        body["loras"] = json!(loras);
    }
    body
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct HotCueArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Hot-cue pad index (0-based).
    index: usize,
    /// Cue position in track seconds.
    seconds: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct HotCuePadArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Hot-cue pad index (0-based).
    index: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetStyleArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// The full set of style-pad targets (prompt + x/y position, 0..1) to install.
    targets: Vec<StyleTargetSnap>,
    /// The blend cursor on the pad (x/y, 0..1).
    cursor: PadPointSnap,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StyleCursorArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Cursor x (0..1).
    x: f32,
    /// Cursor y (0..1).
    y: f32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetModelArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// The realtime model to load (restarts the deck's worker).
    model: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetPromptArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// The text prompt the realtime deck should generate from.
    prompt: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetNotesArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// The held MIDI pitches (0..=127); empty clears the steering.
    pitches: Vec<u8>,
    /// Note mode; omitted means chord-follow (the forgiving default).
    mode: Option<NoteModeSnap>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SetDrumsArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// 'suppress' keeps drums out, 'auto' hands the choice back to the model.
    mode: DrumModeArg,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LoadFromLibraryArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// The library `file` name (from list_songs / list_samples).
    file: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SeekArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Position in track seconds.
    seconds: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TempoArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Target playback tempo in BPM (varispeed; clamped to the deck's range).
    bpm: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct BeatLoopArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Loop length in beats (e.g. 4).
    beats: u32,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DeckPadArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// Loop/sample pad slot index (0-based).
    slot: usize,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct OnAirArgs {
    /// Deck index: 0 = A, 1 = B.
    deck: usize,
    /// On air (audio reaches the master) when true; off air (prep — generating but
    /// audible only in the cue) when false.
    on: bool,
}

/// How many hot-cue pads a deck currently has — the loaded track's cue-bank size, 0
/// with no track. Read from the store snapshot so the cue tools validate before
/// writing (and report "no track" / "out of range" rather than silently no-op).
fn cue_pad_count(store: &InterfaceStore, deck: usize) -> usize {
    store
        .snapshot()
        .decks
        .get(deck)
        .map(|d| d.cues.len())
        .unwrap_or(0)
}

/// Which source a deck currently plays, from the store snapshot — so the
/// transport tools route to the stream or the loaded track coherently instead
/// of driving the realtime worker under a playback deck.
fn deck_mode(store: &InterfaceStore, deck: usize) -> Option<PlayModeSnap> {
    store.snapshot().decks.get(deck).map(|d| d.mode)
}

/// Session-unique id keying an agent generation's `start` to its `done`/`error`
/// (`mcp://generation`), so the webview retires the matching pending row.
fn next_generation_job() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// How many generation jobs `GenerationJobs` remembers. A show generates a
/// handful; the cap only bounds a runaway session.
const MAX_TRACKED_JOBS: usize = 16;

/// The agent generation jobs the MCP surface tracks (#8): `generate_track`
/// answers immediately with a job id and the work continues in a spawned task —
/// a full track generates at ~2.3 s of audio per wall-clock second, which
/// outlives MCP client timeouts (observed live: a 240 s track killed the tool
/// call at 60 s). Tauri-managed (app-wide), so a reconnecting MCP session still
/// sees jobs the previous session started. Ids are [`next_generation_job`]'s —
/// the same ids the `mcp://generation` UI events carry.
#[derive(Default)]
pub struct GenerationJobs(Mutex<VecDeque<GenerationJob>>);

struct GenerationJob {
    id: u64,
    kind: &'static str,
    title: String,
    prompt: String,
    deck: Option<usize>,
    started: Instant,
    /// `None` while running; then the tool-style result and how long it took.
    outcome: Option<(Result<String, String>, Duration)>,
}

impl GenerationJobs {
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<GenerationJob>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Record a job as running. Evicts the oldest *finished* job past the cap
    /// (running jobs are never dropped — their `finish` must still land).
    fn begin(&self, id: u64, kind: &'static str, title: &str, prompt: &str, deck: Option<usize>) {
        let mut jobs = self.lock();
        if jobs.len() >= MAX_TRACKED_JOBS {
            if let Some(oldest_done) = jobs.iter().position(|j| j.outcome.is_some()) {
                jobs.remove(oldest_done);
            }
        }
        jobs.push_back(GenerationJob {
            id,
            kind,
            title: title.to_string(),
            prompt: prompt.to_string(),
            deck,
            started: Instant::now(),
            outcome: None,
        });
    }

    /// Record a running job's result (the message `generate_track` would have
    /// returned synchronously, or the error).
    fn finish(&self, id: u64, result: Result<String, String>) {
        let mut jobs = self.lock();
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            let took = job.started.elapsed();
            job.outcome = Some((result, took));
        }
    }

    /// The `generation_status` payload: every tracked job, newest first.
    fn report(&self) -> String {
        let jobs: Vec<_> = self
            .lock()
            .iter()
            .rev()
            .map(|job| {
                let (status, detail, elapsed) = match &job.outcome {
                    None => ("running", None, job.started.elapsed()),
                    Some((Ok(message), took)) => ("done", Some(message.clone()), *took),
                    Some((Err(message), took)) => ("failed", Some(message.clone()), *took),
                };
                json!({
                    "job": job.id,
                    "kind": job.kind,
                    "title": job.title,
                    "prompt": job.prompt,
                    "deck": job.deck,
                    "status": status,
                    "elapsedSeconds": elapsed.as_secs(),
                    "detail": detail,
                })
            })
            .collect();
        if jobs.is_empty() {
            "no generation jobs yet — generate_track starts one".to_string()
        } else {
            json!({ "jobs": jobs }).to_string()
        }
    }
}

/// The MCP path's counterpart of the webview's `randomSongTitle()`
/// (`frontend/src/media/songTitle.ts`, same word lists): a throwaway-but-pleasant
/// two-word name, so a long prompt never becomes the row title or the on-disk
/// filename — the prompt still rides in the registry.
fn pleasant_title() -> String {
    const ADJECTIVES: [&str; 24] = [
        "Velvet", "Neon", "Crimson", "Glass", "Midnight", "Golden", "Hollow", "Electric",
        "Lunar", "Crystal", "Phantom", "Sapphire", "Wild", "Quiet", "Burning", "Frozen",
        "Cosmic", "Scarlet", "Faded", "Molten", "Paper", "Iron", "Silent", "Amber",
    ];
    const NOUNS: [&str; 24] = [
        "Mirage", "Halo", "Cathedral", "Tide", "Echo", "Bloom", "Pulse", "Horizon",
        "Ember", "Drift", "Reverie", "Cascade", "Vortex", "Lullaby", "Static", "Aurora",
        "Monsoon", "Eclipse", "Requiem", "Afterglow", "Cinder", "Spire", "Solstice", "Comet",
    ];
    // Clock nanos pick well enough for a name — no rand dependency.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    format!(
        "{} {}",
        ADJECTIVES[nanos % ADJECTIVES.len()],
        NOUNS[(nanos / ADJECTIVES.len()) % NOUNS.len()]
    )
}

/// Normalise every tool's input schema into the plainest shape MCP clients
/// handle: inline local `#/$defs/*` refs, then strip the `null` variants
/// schemars emits for `Option<…>` params.
///
/// schemars emits nested param types (`PadPointSnap`, `StyleTargetSnap`, the
/// enum args…) as `$ref`s into `$defs`, and rmcp 1.8 hardcodes its schemars
/// settings (no `inline_subschemas` hook). At least one MCP client (Claude
/// Code) strips `$defs`/`$ref` before surfacing the schema to the model,
/// leaving those params untyped — the model then sends a JSON *string* where
/// a struct is expected (observed live with `set_style`'s cursor). With every
/// ref inlined there is nothing to strip.
fn normalize_tool_schemas(router: &mut ToolRouter<McpHandler>) {
    for route in router.map.values_mut() {
        let mut schema = serde_json::Value::Object(route.attr.input_schema.as_ref().clone());
        if let Some(defs) = schema.get("$defs").and_then(|d| d.as_object()).cloned() {
            inline_refs(&mut schema, &defs, 0);
        }
        strip_null_variants(&mut schema, 0);
        let serde_json::Value::Object(mut object) = schema else {
            unreachable!("schema root stays an object");
        };
        object.remove("$defs");
        route.attr.input_schema = Arc::new(object);
    }
}

/// Replace `{"$ref": "#/$defs/X", …siblings}` with X's schema merged under the
/// siblings (siblings win — schemars puts the per-field doc there). Depth-capped:
/// the arg types are small and non-recursive; a cycle would be a bug here, not a
/// schema to honour.
fn inline_refs(
    value: &mut serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            let resolved = map
                .get("$ref")
                .and_then(|r| r.as_str())
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .and_then(|name| defs.get(name))
                .and_then(|d| d.as_object())
                .cloned();
            if let Some(def) = resolved {
                map.remove("$ref");
                for (key, sub) in def {
                    map.entry(key).or_insert(sub);
                }
            }
            for sub in map.values_mut() {
                inline_refs(sub, defs, depth + 1);
            }
        }
        serde_json::Value::Array(items) => {
            for sub in items {
                inline_refs(sub, defs, depth + 1);
            }
        }
        _ => {}
    }
}

/// Strip the `null` variants schemars emits for `Option<…>` params:
/// `"type": ["number", "null"]` and `anyOf: [X, {"type": "null"}]`. At least
/// one MCP client (Claude Code) drops array-valued `type`s and `anyOf`
/// wrappers when surfacing the schema, leaving the param untyped — the model
/// then sends the value as a JSON *string* the server's serde rejects
/// (observed live with `ramp_ms` and `loras`, session 4). Optionality already
/// lives in `required`, and serde accepts an explicit null regardless of the
/// schema, so the null variant carries nothing a tool-calling client needs.
fn strip_null_variants(value: &mut serde_json::Value, depth: usize) {
    if depth > 16 {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Array(types)) = map.get_mut("type") {
                types.retain(|t| t != "null");
                if let [only] = types.as_slice() {
                    let only = only.clone();
                    map.insert("type".to_owned(), only);
                }
            }
            // `anyOf: [X, {"type": "null"}]` collapses to X merged under the
            // field's own siblings (siblings win — the per-field doc is there,
            // same rule as inline_refs).
            let sole_branch = map.get("anyOf").and_then(|v| v.as_array()).and_then(|branches| {
                let mut real = branches
                    .iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) != Some("null"));
                match (real.next().and_then(|b| b.as_object()), real.next()) {
                    (Some(only), None) if branches.len() > 1 => Some(only.clone()),
                    _ => None,
                }
            });
            if let Some(branch) = sole_branch {
                map.remove("anyOf");
                for (key, sub) in branch {
                    map.entry(key).or_insert(sub);
                }
            }
            for sub in map.values_mut() {
                strip_null_variants(sub, depth + 1);
            }
        }
        serde_json::Value::Array(items) => {
            for sub in items {
                strip_null_variants(sub, depth + 1);
            }
        }
        _ => {}
    }
}

#[tool_router]
impl McpHandler {
    pub fn new(app: AppHandle) -> Self {
        let mut tool_router = Self::tool_router();
        normalize_tool_schemas(&mut tool_router);
        Self { app, tool_router }
    }

    /// Move the crossfader — forwarded to the engine and recorded in the store
    /// exactly as the UI/MIDI `set_crossfade` command does, so the on-screen
    /// crossfader follows (the bidirectional projection).
    #[tool(description = "Set the crossfader position (0 = deck A, 1 = deck B). Optional \
                          ramp_ms glides there engine-side (click-free, equal-power).")]
    async fn set_crossfade(
        &self,
        Parameters(CrossfadeArgs { position, ramp_ms }): Parameters<CrossfadeArgs>,
    ) -> String {
        // Clamp to the engine's range BEFORE recording, so a `lsdj://interface-state`
        // read reports what the audio actually does (the engine clamps too) — an agent
        // must not observe an out-of-range value it never hears (ADR-0020).
        let position = position.clamp(0.0, 1.0);
        let ramp_ms = ramp_ms.unwrap_or(0.0).clamp(0.0, MAX_RAMP_MS);
        self.app
            .state::<Host>()
            .set_crossfade_ramped(position, ramp_ms);
        // The store records the DESTINATION at once (the audio walks to it) —
        // the UI fader jumps ahead of the glide, same as a ramped volume.
        self.app.state::<InterfaceStore>().set_crossfade(position);
        if ramp_ms > 0.0 {
            format!("crossfade gliding to {position} over {ramp_ms} ms")
        } else {
            format!("crossfade set to {position}")
        }
    }

    #[tool(description = "Set a deck's channel volume (0..1). deck 0 = A, 1 = B. Optional \
                          ramp_ms glides there engine-side (a click-free linear fade).")]
    async fn set_volume(
        &self,
        Parameters(DeckGainArgs { deck, gain, ramp_ms }): Parameters<DeckGainArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let gain = gain.clamp(0.0, 1.0); // keep the store honest (see set_crossfade)
        let ramp_ms = ramp_ms.unwrap_or(0.0).clamp(0.0, MAX_RAMP_MS);
        self.app.state::<Host>().set_volume_ramped(deck, gain, ramp_ms);
        self.app.state::<InterfaceStore>().set_volume(deck, gain);
        if ramp_ms > 0.0 {
            format!("deck {deck} volume gliding to {gain} over {ramp_ms} ms")
        } else {
            format!("deck {deck} volume = {gain}")
        }
    }

    #[tool(description = "Set a deck's EQ band (low/mid/high) amount (0..1; 0.5 = flat).")]
    async fn set_eq(
        &self,
        Parameters(DeckEqArgs { deck, band, value }): Parameters<DeckEqArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let value = value.clamp(0.0, 1.0); // keep the store honest (see set_crossfade)
        self.app.state::<Host>().set_eq(deck, band.into(), value);
        self.app.state::<InterfaceStore>().set_eq(deck, band.into(), value);
        format!("deck {deck} eq updated")
    }

    #[tool(description = "Set the headphone cue/master blend (0 = cue only, 1 = master).")]
    async fn set_cue_mix(
        &self,
        Parameters(CueMixArgs { position }): Parameters<CueMixArgs>,
    ) -> String {
        let position = position.clamp(0.0, 1.0); // keep the store honest (see set_crossfade)
        self.app.state::<Host>().set_cue_mix(position);
        self.app.state::<InterfaceStore>().set_cue_mix(position);
        format!("cue mix = {position}")
    }

    #[tool(description = "Select a deck's Color FX: filter, dubEcho, space, crush, noise, or sweep.")]
    async fn set_fx(
        &self,
        Parameters(DeckFxArgs { deck, kind }): Parameters<DeckFxArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        // A kind-swap lands the engine at the new effect's REST amount (so it
        // starts bypassed); the store's set_fx records kind + rest in one write.
        let kind: FxKind = kind.into();
        self.app.state::<Host>().set_fx(deck, kind);
        self.app.state::<InterfaceStore>().set_fx(deck, kind);
        format!("deck {deck} fx selected")
    }

    #[tool(description = "Remove a deck's Color FX.")]
    async fn clear_fx(&self, Parameters(DeckArgs { deck }): Parameters<DeckArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.app.state::<Host>().clear_fx(deck);
        self.app.state::<InterfaceStore>().clear_fx(deck);
        format!("deck {deck} fx cleared")
    }

    #[tool(description = "Set a deck's Color FX amount/intensity (0..1) — how hard the \
                          selected effect is driven. deck 0 = A, 1 = B.")]
    async fn set_fx_amount(
        &self,
        Parameters(FxAmountArgs { deck, amount }): Parameters<FxAmountArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let amount = amount.clamp(0.0, 1.0); // keep the store honest (see set_crossfade)
        self.app.state::<Host>().set_fx_amount(deck, amount);
        self.app.state::<InterfaceStore>().set_fx_amount(deck, amount);
        format!("deck {deck} fx amount = {amount}")
    }

    #[tool(description = "Set a deck's channel trim in dB (0 = unity gain). deck 0 = A, 1 = B.")]
    async fn set_trim(&self, Parameters(TrimArgs { deck, db }): Parameters<TrimArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.app.state::<Host>().set_trim(deck, db);
        self.app.state::<InterfaceStore>().set_trim(deck, db);
        format!("deck {deck} trim = {db} dB")
    }

    #[tool(description = "Toggle a deck's headphone cue (PFL) tap on or off. deck 0 = A, 1 = B.")]
    async fn set_cue(&self, Parameters(CueArgs { deck, on }): Parameters<CueArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.app.state::<Host>().set_cue(deck, on);
        self.app.state::<InterfaceStore>().set_cue(deck, on);
        format!("deck {deck} cue {}", if on { "on" } else { "off" })
    }

    #[tool(
        description = "Start a deck: a realtime deck starts generating; a playback deck \
                       resumes its loaded track. deck 0 = A, 1 = B."
    )]
    async fn deck_play(&self, Parameters(DeckArgs { deck }): Parameters<DeckArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        // A playback deck's PLAY drives the track, not the worker — route through
        // the webview's mode-aware play(), like the on-air gesture.
        if deck_mode(&self.app.state::<InterfaceStore>(), deck) == Some(PlayModeSnap::Playback) {
            self.emit_deck_command(deck, "play", None);
            return format!("deck {deck} resuming its loaded track");
        }
        // The same flow as the `deck_play` command: the store's atomic
        // start_transport is the idempotence guard (phase D) — a play on an
        // already-running deck is a no-op that must not reset held steering.
        if !self.app.state::<InterfaceStore>().start_transport(deck) {
            return format!("deck {deck} already playing");
        }
        self.app.state::<Host>().set_deck_playing(deck, true);
        self.app
            .state::<Sidecars>()
            .send(deck, &json!({ "type": "play" }).to_string());
        // A fresh stream starts unsteered (ADR-0023).
        self.app.state::<crate::midi::notes::NoteSteering>().reset(deck);
        format!("deck {deck} playing")
    }

    #[tool(
        description = "Stop a deck: a realtime deck stops generating; a playback deck \
                       pauses its loaded track in place. deck 0 = A, 1 = B."
    )]
    async fn deck_stop(&self, Parameters(DeckArgs { deck }): Parameters<DeckArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        // A playback deck's STOP pauses the track (pads stop with it) — the
        // webview's mode-aware stop(), not the realtime worker's.
        if deck_mode(&self.app.state::<InterfaceStore>(), deck) == Some(PlayModeSnap::Playback) {
            self.emit_deck_command(deck, "stop", None);
            return format!("deck {deck} pausing its loaded track");
        }
        self.app.state::<Host>().set_deck_playing(deck, false);
        self.app
            .state::<Sidecars>()
            .send(deck, &json!({ "type": "stop" }).to_string());
        self.app.state::<InterfaceStore>().set_playing(deck, false);
        format!("deck {deck} stopped")
    }

    /// The way back from `load_track`: the webview owns the unload flow
    /// (`leavePlayback`), so this validates the mode and asks it to run — the
    /// load-flow pattern in reverse.
    #[tool(
        description = "Eject a deck's loaded track and return the deck to the realtime \
                       (live-generation) stream — the way back from load_track. A track \
                       that was playing hands straight back to the live stream. \
                       deck 0 = A, 1 = B."
    )]
    async fn eject(&self, Parameters(DeckArgs { deck }): Parameters<DeckArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        if deck_mode(&self.app.state::<InterfaceStore>(), deck) != Some(PlayModeSnap::Playback) {
            return format!("deck {deck} is already live (realtime) — nothing to eject");
        }
        self.emit_deck_command(deck, "eject", None);
        format!("ejecting deck {deck} — handing back to the realtime stream")
    }

    /// Set a hot-cue point on a playback deck's loaded track. Writes the store; the
    /// webview adopts the change and lights the pad (the bidirectional projection). A
    /// realtime deck / no track, or an out-of-range pad, comes back as a message.
    #[tool(
        description = "Set a hot-cue point on a deck's loaded track at the given time \
                       (track seconds). deck 0 = A, 1 = B; index is the 0-based pad."
    )]
    async fn set_hot_cue(
        &self,
        Parameters(HotCueArgs {
            deck,
            index,
            seconds,
        }): Parameters<HotCueArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let store = self.app.state::<InterfaceStore>();
        let pads = cue_pad_count(&store, deck);
        if pads == 0 {
            return format!("deck {deck} has no loaded track, so no hot cues");
        }
        if index >= pads {
            return format!("hot-cue pad {index} is out of range (deck {deck} has {pads})");
        }
        store.set_deck_cue(deck, index, Some(seconds));
        format!("deck {deck} hot cue {index} set to {seconds:.2}s")
    }

    #[tool(description = "Clear a hot-cue pad on a deck's loaded track. deck 0 = A, 1 = B.")]
    async fn clear_hot_cue(
        &self,
        Parameters(HotCuePadArgs { deck, index }): Parameters<HotCuePadArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let store = self.app.state::<InterfaceStore>();
        if index >= cue_pad_count(&store, deck) {
            return format!("deck {deck} has no hot-cue pad {index}");
        }
        store.set_deck_cue(deck, index, None);
        format!("deck {deck} hot cue {index} cleared")
    }

    /// Jump the deck's track to a hot cue — a transport seek straight to the engine
    /// (the cue point is read from the store), like the UI's filled-pad tap.
    #[tool(
        description = "Jump (seek) a deck's track to a previously set hot cue. \
                       deck 0 = A, 1 = B."
    )]
    async fn jump_to_hot_cue(
        &self,
        Parameters(HotCuePadArgs { deck, index }): Parameters<HotCuePadArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let cue = self
            .app
            .state::<InterfaceStore>()
            .snapshot()
            .decks
            .get(deck)
            .and_then(|d| d.cues.get(index).copied().flatten());
        match cue {
            Some(seconds) => {
                let frames = seconds * f64::from(lsdj_engine::SAMPLE_RATE);
                self.app.state::<Host>().seek_track(deck, frames);
                format!("deck {deck} jumped to hot cue {index} ({seconds:.2}s)")
            }
            None => format!("deck {deck} has no hot cue at pad {index}"),
        }
    }

    /// Replace a realtime deck's whole style pad (targets + cursor). Writes the store;
    /// `DeckColumn` adopts it and pushes the blended prompt to the worker.
    #[tool(
        description = "Replace a realtime deck's style pad: the targets (each a prompt at \
                       an x/y position, 0..1) and the blend cursor (x/y, 0..1). \
                       deck 0 = A, 1 = B."
    )]
    async fn set_style(
        &self,
        Parameters(SetStyleArgs {
            deck,
            targets,
            cursor,
        }): Parameters<SetStyleArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        // An external arrangement replaces the pad wholesale (the store owns
        // the semantics — ADR-0020 phase B; sampled chips can't come from
        // outside, the sanitiser drops them).
        let targets = crate::style::sanitize_preset_targets(targets);
        let count = targets.len();
        self.app
            .state::<InterfaceStore>()
            .style_apply_preset(deck, targets, cursor);
        format!("deck {deck} style set ({count} target(s))")
    }

    #[tool(
        description = "Move a realtime deck's style-pad blend cursor (x, y in 0..1), \
                       leaving its targets. deck 0 = A, 1 = B."
    )]
    async fn set_style_cursor(
        &self,
        Parameters(StyleCursorArgs { deck, x, y }): Parameters<StyleCursorArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.app
            .state::<InterfaceStore>()
            .set_deck_cursor(deck, PadPointSnap { x, y });
        format!("deck {deck} style cursor set to ({x:.2}, {y:.2})")
    }

    /// Switch a realtime deck's model — restarts its worker. The UI reflects the
    /// switch through the worker's model-loading/ready events (which the reducer
    /// mirrors back up), so no separate store write is needed.
    #[tool(
        description = "Switch a realtime deck's model (restarts the deck's worker). The \
                       new model takes a few seconds to load; the interface-state \
                       resource reflects it once the worker reports ready. deck 0 = A, 1 = B."
    )]
    async fn set_model(
        &self,
        Parameters(SetModelArgs { deck, model }): Parameters<SetModelArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        if model.is_empty() || model.len() > 64 {
            return "invalid model name".to_string();
        }
        match self.app.state::<Sidecars>().restart(deck, &model) {
            Ok(()) => format!("deck {deck} switching to model {model}"),
            Err(e) => format!("could not switch deck {deck} to {model}: {e}"),
        }
    }

    /// Steer a realtime deck's harmony (ADR-0023). Routed through the shell
    /// note-steering service (ADR-0031) — the single sender native MIDI and the
    /// UI use: it builds the wire multihot, drives the worker directly, and
    /// mirrors the store; the webview only displays.
    #[tool(
        description = "Steer a realtime deck's harmony with held MIDI pitches \
                       (0-127); an empty list clears the steering (the model plays \
                       freely). mode 'chord' (the default) lets the model choose \
                       the articulation; 'onset' marks the pitches as fresh attacks. \
                       Steering resets on play/stop/model switch — start the deck \
                       first, then steer. A change is heard once the deck's buffered \
                       audio drains (a few seconds). deck 0 = A, 1 = B."
    )]
    async fn set_notes(
        &self,
        Parameters(SetNotesArgs { deck, pitches, mode }): Parameters<SetNotesArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        if pitches.iter().any(|&pitch| pitch > 127) {
            return "pitches must be MIDI note numbers 0-127".to_string();
        }
        let count = pitches.len();
        let mode = mode.unwrap_or(NoteModeSnap::Chord);
        self.app
            .state::<NoteSteering>()
            .apply_external(deck, &pitches, mode);
        if count == 0 {
            return format!("deck {deck} note steering cleared");
        }
        format!("deck {deck} steering {count} held note(s)")
    }

    /// Set a realtime deck's drum conditioning (ADR-0023) — the same shell
    /// service as `set_notes` (it sends the wire flag and mirrors the store).
    #[tool(
        description = "Set a realtime deck's drum conditioning: 'suppress' keeps \
                       drums out (sit beside another deck), 'auto' hands the choice \
                       back to the model. Deck config (issue #50): it sticks across \
                       play/stop/model switch — the shell re-asserts it over each \
                       fresh stream — until 'auto' hands it back. deck 0 = A, 1 = B."
    )]
    async fn set_drums(
        &self,
        Parameters(SetDrumsArgs { deck, mode }): Parameters<SetDrumsArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.app.state::<NoteSteering>().set_drums(deck, mode.into());
        format!(
            "deck {deck} drums {}",
            match mode {
                DrumModeArg::Suppress => "suppressed",
                DrumModeArg::Auto => "back to the model",
            }
        )
    }

    /// Set a realtime deck's generation prompt. Routed through the style pad as one
    /// centred target so it shows on the pad and drives the worker — the same path the
    /// UI takes (the bidirectional projection), not a hidden raw override.
    #[tool(
        description = "Set a realtime deck's generation prompt (appears on the style \
                       pad as a single target). deck 0 = A, 1 = B."
    )]
    async fn set_prompt(
        &self,
        Parameters(SetPromptArgs { deck, prompt }): Parameters<SetPromptArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let center = PadPointSnap { x: 0.5, y: 0.5 };
        self.app.state::<InterfaceStore>().style_apply_preset(
            deck,
            crate::style::sanitize_preset_targets(vec![StyleTargetSnap {
                x: 0.5,
                y: 0.5,
                text: prompt.clone(),
                sample: None,
            }]),
            center,
        );
        format!("deck {deck} prompt set to \"{prompt}\"")
    }

    /// Observe the whole instrument: the same snapshot the `lsdj://interface-state`
    /// resource serves, exposed as a tool because many MCP clients (Claude Code among
    /// them) surface tools but not resources to the agent loop — without this the co-DJ
    /// is blind. Read-only; no store write.
    #[tool(
        description = "Observe the whole instrument — returns the interface-state \
                       snapshot (both decks, mixer, FX, style pads, cues, transport, \
                       models) as JSON, the same data as the lsdj://interface-state \
                       resource. Call it to see current state before and after a move. \
                       Note: a deck's top-level `playing` covers the REALTIME stream \
                       only; on a playback deck read `transport.playing`."
    )]
    async fn get_state(&self) -> String {
        let snapshot = self.app.state::<InterfaceStore>().snapshot();
        serde_json::to_string(&snapshot)
            .unwrap_or_else(|e| format!("could not serialise the interface state: {e}"))
    }

    /// Run a blocking library scan off the async runtime. `SongLibrary`/
    /// `SampleLibrary` reads take a lock and touch the filesystem; on a tokio
    /// worker, concurrent scans serialised past the MCP client's timeout
    /// (observed live: a batched `load_track` returned -32001).
    async fn scan_library<T, F>(&self, scan: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&AppHandle) -> Result<T, String> + Send + 'static,
    {
        let app = self.app.clone();
        tokio::task::spawn_blocking(move || scan(&app))
            .await
            .map_err(|e| format!("library scan task failed: {e}"))?
    }

    #[tool(
        description = "List the generated songs/tracks available to load onto a deck — \
                       each has a `file` (pass to load_track) plus title + prompt."
    )]
    async fn list_songs(&self) -> String {
        match self.scan_library(|app| app.state::<SongLibrary>().list()).await {
            Ok(entries) => serde_json::to_string(&entries)
                .unwrap_or_else(|e| format!("could not serialise songs: {e}")),
            Err(e) => format!("could not list songs: {e}"),
        }
    }

    #[tool(
        description = "List the generated samples/loops available to load onto a deck's \
                       pad — each has a `file` (pass to load_sample)."
    )]
    async fn list_samples(&self) -> String {
        match self.scan_library(|app| app.state::<SampleLibrary>().list()).await {
            Ok(entries) => serde_json::to_string(&entries)
                .unwrap_or_else(|e| format!("could not serialise samples: {e}")),
            Err(e) => format!("could not list samples: {e}"),
        }
    }

    #[tool(
        description = "List the installed LoRA style adapters for generation. Each has a \
                       `name` (pass in generate_track / generate_sample `loras`) and a \
                       `base`: \"medium\" adapters ride generate_track, \"small\" ones \
                       ride generate_sample kind sfx/music."
    )]
    async fn list_loras(&self) -> String {
        // Same blocking-fs rule as the library scans (finding #2).
        let list = tokio::task::spawn_blocking(|| {
            crate::loras::discover(&crate::loras::loras_dir())
        })
        .await;
        match list {
            Ok(adapters) => serde_json::to_string(&adapters)
                .unwrap_or_else(|e| format!("could not serialise adapters: {e}")),
            Err(e) => format!("could not list LoRA adapters: {e}"),
        }
    }

    /// Load a generated song onto a deck (flipping it to playback). The webview owns
    /// the decode + beatgrid analysis (ADR-0017), so this validates the file and asks
    /// the webview to run its load flow — the same path the Media Explorer's "load to
    /// deck" takes, so the deck shows the track, overview, and cues.
    #[tool(
        description = "Load a generated song/track (by its `file` from list_songs) onto a \
                       deck, flipping it to playback. deck 0 = A, 1 = B."
    )]
    async fn load_track(
        &self,
        Parameters(LoadFromLibraryArgs { deck, file }): Parameters<LoadFromLibraryArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let entries = match self.scan_library(|app| app.state::<SongLibrary>().list()).await {
            Ok(entries) => entries,
            Err(e) => return format!("could not read the song library: {e}"),
        };
        let Some(entry) = entries.into_iter().find(|e| e.file == file) else {
            return format!("no song named {file} — call list_songs for the available files");
        };
        let title = entry.title;
        let _ = self.app.emit(
            "mcp://load-track",
            json!({ "deck": deck, "file": file, "title": title }),
        );
        format!("loading \"{title}\" onto deck {deck}")
    }

    /// Load a generated sample/loop onto a deck's pad bank. Like load_track, the webview
    /// runs its sample-load flow (decode + slot install) so the pad reflects it.
    #[tool(
        description = "Load a generated sample/loop (by its `file` from list_samples) onto \
                       a deck's pad. deck 0 = A, 1 = B."
    )]
    async fn load_sample(
        &self,
        Parameters(LoadFromLibraryArgs { deck, file }): Parameters<LoadFromLibraryArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let entries = match self.scan_library(|app| app.state::<SampleLibrary>().list()).await {
            Ok(entries) => entries,
            Err(e) => return format!("could not read the sample library: {e}"),
        };
        let Some(entry) = entries.into_iter().find(|e| e.file == file) else {
            return format!("no sample named {file} — call list_samples for the available files");
        };
        let (label, one_shot) = (entry.title, entry.one_shot);
        let _ = self.app.emit(
            "mcp://load-sample",
            json!({ "deck": deck, "file": file, "oneShot": one_shot, "label": label }),
        );
        format!("loading \"{label}\" onto deck {deck}")
    }

    /// Ask the webview to run a track-transport gesture on a deck. Transport that the
    /// webview owns (rate/loop/sync state, or a seek the position poll reflects) is
    /// driven through the deck's own methods so the UI follows — the load-flow pattern.
    fn emit_deck_command(&self, deck: usize, command: &str, value: Option<f64>) {
        let _ = self.app.emit(
            "mcp://deck-command",
            json!({ "deck": deck, "command": command, "value": value }),
        );
    }

    /// Tell the webview what an agent generation is doing (`mcp://generation`) —
    /// a multi-second/minute proxy call is otherwise invisible in the UI, and the
    /// co-DJ is a second operator the human should be able to watch. The Media
    /// Explorer mirrors a pending row from `start` and retires it on `done`/`error`.
    #[allow(clippy::too_many_arguments)]
    fn emit_generation(
        &self,
        job: u64,
        phase: &str,
        kind: &str,
        prompt: &str,
        title: &str,
        deck: Option<usize>,
        one_shot: bool,
    ) {
        let _ = self.app.emit(
            "mcp://generation",
            json!({
                "job": job,
                "phase": phase,
                "kind": kind,
                "prompt": prompt,
                "title": title,
                "deck": deck,
                "oneShot": one_shot,
            }),
        );
    }

    #[tool(description = "Seek a deck's loaded track to a position in seconds. Seeking \
                          releases an active beat loop. deck 0 = A, 1 = B.")]
    async fn seek_track(&self, Parameters(SeekArgs { deck, seconds }): Parameters<SeekArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.emit_deck_command(deck, "seek", Some(seconds));
        format!("deck {deck} seeking to {seconds:.2}s")
    }

    /// Set a deck's tempo in BPM — converted to a varispeed rate from the loaded
    /// track's own BPM (read from the store), then clamped to the deck's range by the
    /// webview.
    #[tool(
        description = "Set a deck's playback tempo in BPM (varispeed; clamped to the \
                       deck's range). Needs a loaded track with a known BPM. deck 0 = A, 1 = B."
    )]
    async fn set_tempo(&self, Parameters(TempoArgs { deck, bpm }): Parameters<TempoArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        if bpm <= 0.0 {
            return "bpm must be positive".to_string();
        }
        let snapshot = self.app.state::<InterfaceStore>().snapshot();
        let base = snapshot
            .decks
            .get(deck)
            .and_then(|d| d.track.as_ref())
            .and_then(|track| track.bpm);
        let Some(base) = base.filter(|b| *b > 0.0) else {
            return format!("deck {deck} has no track with a known BPM to set tempo on");
        };
        let rate = bpm / base;
        self.emit_deck_command(deck, "rate", Some(rate));
        format!("deck {deck} tempo → {bpm:.1} BPM (rate {rate:.3})")
    }

    #[tool(
        description = "Beat-match (sync) a deck's track to the other deck's tempo. Both \
                       decks need a known BPM — a library track can carry `bpm: null` \
                       (no analysis; see get_state), which cannot beat-match. \
                       deck 0 = A, 1 = B."
    )]
    async fn sync_deck(&self, Parameters(DeckArgs { deck }): Parameters<DeckArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.emit_deck_command(deck, "sync", None);
        format!("deck {deck} syncing to the other deck")
    }

    #[tool(description = "Set a beat loop on a deck's track (length in beats, e.g. 4). A \
                          later seek_track releases the loop. deck 0 = A, 1 = B.")]
    async fn beat_loop(&self, Parameters(BeatLoopArgs { deck, beats }): Parameters<BeatLoopArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.emit_deck_command(deck, "beatloop", Some(f64::from(beats)));
        format!("deck {deck} {beats}-beat loop")
    }

    /// Fire a deck's loop/sample pad — the webview runs the exact pad-press gesture
    /// (`toggleLoopPad`), so quantise, layering, and the pad UI all follow.
    #[tool(
        description = "Toggle a deck's loop/sample pad (0-based slot). A filled slot \
                       plays or stops it (loops layer over the deck; one-shots fire \
                       once); an EMPTY slot captures a freeze loop from the deck's live \
                       stream. Slot contents are in get_state loopLabels. deck 0 = A, 1 = B."
    )]
    async fn toggle_pad(&self, Parameters(DeckPadArgs { deck, slot }): Parameters<DeckPadArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let slots = self
            .app
            .state::<InterfaceStore>()
            .snapshot()
            .decks
            .get(deck)
            .map(|d| d.loop_labels.len())
            .unwrap_or(0);
        if slot >= slots {
            return format!("invalid slot {slot} — deck {deck} has {slots} pad slots");
        }
        self.emit_deck_command(deck, "pad", Some(slot as f64));
        format!("deck {deck} pad {slot} toggled")
    }

    /// Bring a realtime deck on air (its audio reaches the master) or off air — the
    /// prep/primed gesture: it keeps generating but is audible only in the cue. Routed
    /// through the deck's own play/prime so the on-screen status + cue LED follow.
    #[tool(
        description = "Bring a realtime deck on air (to the master) or off air (prep: the \
                       deck starts — or keeps — generating, audible only in the headphone \
                       cue; no separate deck_play needed). deck 0 = A, 1 = B."
    )]
    async fn set_on_air(&self, Parameters(OnAirArgs { deck, on }): Parameters<OnAirArgs>) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        self.emit_deck_command(deck, if on { "onair" } else { "offair" }, None);
        format!(
            "deck {deck} going {}",
            if on { "on air" } else { "off air (prep)" }
        )
    }

    /// Generate a clip via the loopback generation server and save it to the samples
    /// library — the agent composes audio that lands in the Samples tab (the folder
    /// watcher surfaces it), ready to load onto a deck. Failure modes (server off,
    /// prompt too long, bad length) come back as the tool's message, like the deck
    /// guards above, rather than failing the call.
    #[tool(
        description = "Generate a short audio clip from a text prompt and save it to the \
                       samples library, where it appears in the Samples tab ready to load \
                       onto a deck. kind: \"sfx\" or \"music\" (Stable Audio 3), or \
                       \"magenta\" (the Magenta pad renderer). Optional `loras` applies \
                       installed style adapters (list_loras; sfx/music only)."
    )]
    async fn generate_sample(
        &self,
        Parameters(args): Parameters<GenerateSampleArgs>,
    ) -> String {
        let job = next_generation_job();
        let title = pleasant_title();
        let (kind, one_shot) = (args.kind, args.one_shot);
        let prompt = args.prompt.clone();
        self.emit_generation(job, "start", kind.as_str(), &prompt, &title, None, one_shot);
        let result = self.generate_sample_inner(args, &title).await;
        self.emit_generation(
            job,
            if result.is_ok() { "done" } else { "error" },
            kind.as_str(),
            &prompt,
            &title,
            None,
            one_shot,
        );
        match result {
            Ok(message) | Err(message) => message,
        }
    }

    /// The fallible body of [`generate_sample`], so the proxy + save can use `?` and the
    /// tool flattens the result to one message.
    async fn generate_sample_inner(
        &self,
        args: GenerateSampleArgs,
        title: &str,
    ) -> Result<String, String> {
        let GenerateSampleArgs {
            prompt,
            seconds,
            kind,
            one_shot,
            loras,
        } = args;
        let wav = self
            .generate_clip(&prompt, seconds, kind.as_str(), &loras.unwrap_or_default())
            .await?;
        let entry = self.app.state::<SampleLibrary>().record(
            NewSample {
                title: title.to_string(),
                prompt: Some(prompt),
                model: Some(kind.as_str().to_string()),
                one_shot,
            },
            &wav,
        )?;
        Ok(format!(
            "generated a {} sample \"{}\", saved to the samples library as {}",
            kind.as_str(),
            entry.title,
            entry.file
        ))
    }

    /// POST a generation request to the loopback server and return the WAV bytes.
    /// Shared by [`generate_sample`] (sfx/music/magenta → samples) and
    /// [`generate_track`] (track → songs), reusing the server's prompt/length
    /// validation. `magenta` routes to the Magenta renderer (`/api/render`, body
    /// `{prompt, seconds}`); the rest are Stable Audio 3 (`/api/generate`).
    async fn generate_clip(
        &self,
        prompt: &str,
        seconds: f32,
        kind: &str,
        loras: &[LoraArg],
    ) -> Result<Vec<u8>, String> {
        let port = self
            .app
            .state::<GenerationServer>()
            .port()
            .ok_or("the generation server is not running")?;
        // sa3 generation is serialised; a full track (medium model) can take minutes,
        // so allow generous headroom but never wait forever for a wedged worker.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .map_err(|e| format!("could not build the http client: {e}"))?;
        let (path, body) = if kind == "magenta" {
            if !loras.is_empty() {
                return Err(
                    "the magenta engine does not take LoRA adapters — use kind \
                     \"sfx\" or \"music\" (or generate_track)"
                        .to_string(),
                );
            }
            ("/api/render", json!({ "prompt": prompt, "seconds": seconds }))
        } else {
            (
                "/api/generate",
                generate_request_body(prompt, seconds, kind, loras),
            )
        };
        let response = client
            .post(format!("http://127.0.0.1:{port}{path}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("generation request failed: {e}"))?;
        if !response.status().is_success() {
            // The server returns a JSON `{detail}` (FastAPI HTTPException); surface it.
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            return Err(format!("generation failed ({status}): {detail}"));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|e| format!("could not read the generated audio: {e}"))
    }

    /// Generate a full track and load it onto a deck (the user's "compose a track and
    /// drop it on a deck"). Saves to the songs library, then asks the webview to load
    /// it — the same path as `load_track`, so the deck flips to playback and shows it.
    ///
    /// Async (#8): answers immediately with a job id and spawns the work — a full
    /// track generates at ~2.3 s of audio per wall-clock second, which outlives MCP
    /// client timeouts (a 240 s track died at the client's 60 s live). The spawned
    /// task lands its result in [`GenerationJobs`] for `generation_status`.
    #[tool(
        description = "Generate a full track (Stable Audio 3, long-form) from a text \
                       prompt, save it to the songs library, and load it onto a deck \
                       (flipping it to playback). Returns immediately with a job id — \
                       generation runs in the background at roughly 2.3 s of audio per \
                       second, so keep mixing and poll generation_status (or watch \
                       get_state for the deck to flip). Optional `loras` applies \
                       installed style adapters (see list_loras). deck 0 = A, 1 = B."
    )]
    async fn generate_track(
        &self,
        Parameters(GenerateTrackArgs {
            deck,
            prompt,
            seconds,
            loras,
        }): Parameters<GenerateTrackArgs>,
    ) -> String {
        if !valid_deck(deck) {
            return format!("invalid deck {deck}");
        }
        let job = next_generation_job();
        let title = pleasant_title();
        self.emit_generation(job, "start", "track", &prompt, &title, Some(deck), false);
        self.app
            .state::<GenerationJobs>()
            .begin(job, "track", &title, &prompt, Some(deck));
        let handler = self.clone();
        let loras = loras.unwrap_or_default();
        let spawned_title = title.clone();
        let spawned_prompt = prompt.clone();
        tauri::async_runtime::spawn(async move {
            let result = handler
                .generate_track_inner(deck, spawned_prompt.clone(), seconds, &spawned_title, &loras)
                .await;
            handler.emit_generation(
                job,
                if result.is_ok() { "done" } else { "error" },
                "track",
                &spawned_prompt,
                &spawned_title,
                Some(deck),
                false,
            );
            handler.app.state::<GenerationJobs>().finish(job, result);
        });
        let eta = (seconds / 2.3).round() as u32;
        format!(
            "track generation started as job {job}: \"{title}\" ({seconds:.0}s) will load \
             onto deck {deck} when done, roughly {eta}s from now. Keep mixing — poll \
             generation_status to see it land."
        )
    }

    #[tool(
        description = "Status of this app run's generation jobs (generate_track runs in \
                       the background): running/done/failed per job with elapsed seconds \
                       and, once finished, what loaded where. No arguments."
    )]
    async fn generation_status(&self) -> String {
        self.app.state::<GenerationJobs>().report()
    }

    /// The fallible body of [`generate_track`].
    async fn generate_track_inner(
        &self,
        deck: usize,
        prompt: String,
        seconds: f32,
        title: &str,
        loras: &[LoraArg],
    ) -> Result<String, String> {
        let wav = self.generate_clip(&prompt, seconds, "track", loras).await?;
        let entry = self.app.state::<SongLibrary>().record(
            NewSong {
                title: title.to_string(),
                prompt,
                model: "track".to_string(),
                recipe: None,
            },
            &wav,
        )?;
        let _ = self.app.emit(
            "mcp://load-track",
            json!({ "deck": deck, "file": entry.file, "title": entry.title }),
        );
        Ok(format!(
            "generated \"{}\" and loading it onto deck {deck}",
            entry.title
        ))
    }
}

/// The URI the interface-state snapshot is served at — the agent reads this to
/// observe the whole instrument (the store snapshot, ADR-0020).
const STORE_RESOURCE_URI: &str = "lsdj://interface-state";

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpHandler {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive], so build from default and set the public
        // fields: advertise BOTH tools and resources so the client lists the store.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.instructions = Some(
            "LSDJ — a generative DJ instrument. Call the `get_state` tool (or read the \
             `lsdj://interface-state` resource) to observe the decks, mixer, and FX; call \
             the other tools to mix, drive the decks, and generate audio into the samples \
             library as a co-DJ."
                .to_string(),
        );
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                RawResource::new(STORE_RESOURCE_URI, "Interface state").no_annotation()
            ],
            next_cursor: None,
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        if request.uri != STORE_RESOURCE_URI {
            return Err(McpError::resource_not_found(
                format!("unknown resource: {}", request.uri),
                None,
            ));
        }
        let snapshot = self.app.state::<InterfaceStore>().snapshot();
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            json,
            STORE_RESOURCE_URI,
        )]))
    }
}

/// The supervised MCP server: its loopback port and the bearer token (surfaced via
/// `app_info`). The token is **shared + mutable** (`Arc<RwLock<String>>`) so
/// [`rotate`](McpServer::rotate) swaps it in live without a restart, and **persisted**
/// at `token_path` so a client config stays valid across launches. The **port** is
/// likewise persisted at `port_path` and user-settable ([`set_port`](McpServer::set_port)),
/// which rebinds + restarts the serving task. The live port + the task's cancel token
/// sit behind a `Mutex` so a restart can swap them. Held in Tauri managed state;
/// dropping it (or `shutdown`) stops the task.
pub struct McpServer {
    app: AppHandle,
    token: Arc<RwLock<String>>,
    /// Where the token is persisted (under the app data dir); `None` if the dir can't
    /// be resolved (then the token is in-memory only).
    token_path: Option<PathBuf>,
    /// Where the chosen port is persisted, so it's stable across launches and the
    /// config snippet doesn't churn; `None` if the dir can't be resolved.
    port_path: Option<PathBuf>,
    running: Mutex<RunningServer>,
}

/// The live serving task: the bound port (`None` if no bind succeeded) and the token
/// that stops it.
struct RunningServer {
    port: Option<u16>,
    cancel: CancellationToken,
}

impl McpServer {
    /// Start the MCP server — **always on**. Never fails the app: a failed bind yields
    /// `port() == None` and the endpoint is simply unadvertised. Prefers the persisted
    /// port (so the config is stable across launches), falling back to an ephemeral
    /// port — which is then persisted so it's reused next time. Every request must
    /// carry the bearer token (also persisted).
    pub fn start(app: AppHandle) -> McpServer {
        let token_path = token_file(&app);
        let token_string = match &token_path {
            Some(path) => load_or_generate_token(path),
            None => generate_token(),
        };
        let token = Arc::new(RwLock::new(token_string));

        let port_path = port_file(&app);
        let desired = port_path.as_deref().and_then(load_port);
        let running = spawn_server(&app, &token, desired);

        // Remember the actually-bound port so an ephemeral assignment is reused.
        if let (Some(port), Some(path)) = (running.port, &port_path) {
            save_port(path, port);
        }

        McpServer {
            app,
            token,
            token_path,
            port_path,
            running: Mutex::new(running),
        }
    }

    /// The loopback port the server is bound to, or `None` if no bind succeeded.
    pub fn port(&self) -> Option<u16> {
        lock_running(&self.running).port
    }

    /// The current bearer token a client must present.
    pub fn token(&self) -> Option<String> {
        Some(read_lock(&self.token).clone())
    }

    /// Mint a NEW token, persist it, and swap it in live so the middleware accepts it
    /// at once (a leaked token is invalidated without restarting). Returns the new token.
    pub fn rotate(&self) -> Option<String> {
        let next = generate_token();
        if let Some(path) = &self.token_path {
            save_token(path, &next);
        }
        *write_lock(&self.token) = next.clone();
        Some(next)
    }

    /// Rebind the server to `new_port`, restart the serving task, and persist it so it
    /// holds across launches. Binds the new port BEFORE stopping the old task, so a
    /// failed bind (port taken or privileged) leaves the running server untouched.
    /// Returns the new port.
    pub fn set_port(&self, new_port: u16) -> Result<u16, String> {
        if new_port < 1024 {
            return Err("choose a port between 1024 and 65535".to_string());
        }
        // Bind first; if this fails the old server keeps serving.
        let (listener, port) =
            bind_loopback(new_port).map_err(|e| format!("could not bind port {new_port}: {e}"))?;
        let cancel = serve(self.app.clone(), self.token.clone(), listener, port);

        let previous = {
            let mut running = lock_running(&self.running);
            std::mem::replace(
                &mut *running,
                RunningServer {
                    port: Some(port),
                    cancel,
                },
            )
        };
        previous.cancel.cancel();
        if let Some(path) = &self.port_path {
            save_port(path, port);
        }
        Ok(port)
    }

    /// Stop the serving task (graceful shutdown). Called from the app's `Exit` handler.
    pub fn shutdown(&self) {
        lock_running(&self.running).cancel.cancel();
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Ok(running) = self.running.get_mut() {
            running.cancel.cancel();
        }
    }
}

/// Bind a loopback TCP listener on `port` (0 = ephemeral) and return it with the
/// actually-bound port, ready to hand to tokio.
fn bind_loopback(port: u16) -> std::io::Result<(std::net::TcpListener, u16)> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    let actual = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    Ok((listener, actual))
}

/// Spawn the streamable-HTTP serving task on `listener`; returns the token that stops
/// it. The handler reaches the app's managed state through the cloned `AppHandle`, and
/// the auth middleware reads the shared token fresh per request.
fn serve(
    app: AppHandle,
    token: Arc<RwLock<String>>,
    listener: std::net::TcpListener,
    port: u16,
) -> CancellationToken {
    let service = StreamableHttpService::new(
        move || Ok(McpHandler::new(app.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(axum::middleware::from_fn_with_state(token, require_token));

    let cancel = CancellationToken::new();
    let serve_cancel = cancel.clone();
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(listener) => listener,
            Err(e) => {
                eprintln!("lsdj-app: MCP server tokio listener failed: {e}");
                return;
            }
        };
        println!("lsdj-app: MCP server on http://127.0.0.1:{port}/mcp");
        let result = axum::serve(listener, router)
            .with_graceful_shutdown(async move { serve_cancel.cancelled().await })
            .await;
        if let Err(e) = result {
            eprintln!("lsdj-app: MCP server stopped: {e}");
        }
    });
    cancel
}

/// Bind + serve, preferring `desired` (the persisted / user port) and falling back to
/// an ephemeral port if that bind fails, so the server still comes up. `port == None`
/// only if even the ephemeral bind failed.
fn spawn_server(app: &AppHandle, token: &Arc<RwLock<String>>, desired: Option<u16>) -> RunningServer {
    let bound = desired
        .and_then(|port| match bind_loopback(port) {
            Ok(bound) => Some(bound),
            Err(e) => {
                eprintln!("lsdj-app: MCP server bind {port} failed ({e}); using an ephemeral port");
                None
            }
        })
        .or_else(|| match bind_loopback(0) {
            Ok(bound) => Some(bound),
            Err(e) => {
                eprintln!("lsdj-app: MCP server bind failed: {e}");
                None
            }
        });
    match bound {
        Some((listener, port)) => RunningServer {
            port: Some(port),
            cancel: serve(app.clone(), token.clone(), listener, port),
        },
        None => RunningServer {
            port: None,
            cancel: CancellationToken::new(),
        },
    }
}

fn lock_running(running: &Mutex<RunningServer>) -> std::sync::MutexGuard<'_, RunningServer> {
    running.lock().unwrap_or_else(|p| p.into_inner())
}

/// Reject any request that does not carry `Authorization: Bearer <token>`. The token
/// is read fresh each request from the shared lock, so a `rotate` takes effect at
/// once. The server is loopback-only, but the token stops another local process from
/// driving the instrument without the user's config.
async fn require_token(
    axum::extract::State(token): axum::extract::State<Arc<RwLock<String>>>,
    request: Request,
    next: Next,
) -> Response {
    let presented = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok());
    let expected = format!("Bearer {}", *read_lock(&token));
    let ok = presented.is_some_and(|p| constant_time_eq(p.as_bytes(), expected.as_bytes()));
    if ok {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response()
    }
}

/// Constant-time byte comparison for the bearer-token check, so a wrong token can't be
/// recovered byte-by-byte through early-exit timing. Loopback + a 256-bit token already
/// make a timing attack impractical; this is the cheap, conventional hardening (no new
/// dependency — `subtle`/`ring` would do the same).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Recover a poisoned lock — a panic in another holder must not wedge auth/rotation.
fn read_lock(lock: &RwLock<String>) -> std::sync::RwLockReadGuard<'_, String> {
    lock.read().unwrap_or_else(|p| p.into_inner())
}
fn write_lock(lock: &RwLock<String>) -> std::sync::RwLockWriteGuard<'_, String> {
    lock.write().unwrap_or_else(|p| p.into_inner())
}

/// The token file under the app data dir (`None` if it can't be resolved).
fn token_file(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("mcp-token"))
}

/// The port file under the app data dir (`None` if it can't be resolved).
fn port_file(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("mcp-port"))
}

/// Read the persisted port — a plain decimal `u16` ≥ 1024 (privileged ports are
/// rejected, like [`McpServer::set_port`]); `None` (ephemeral) if absent or invalid.
fn load_port(path: &Path) -> Option<u16> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .filter(|port| *port >= 1024)
}

/// Persist the chosen port (best-effort) so it's reused next launch.
fn save_port(path: &Path, port: u16) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, port.to_string());
}

/// Read the persisted token, or mint + save a new one (first run / empty file).
fn load_or_generate_token(path: &Path) -> String {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let token = generate_token();
    save_token(path, &token);
    token
}

/// Persist the token owner-read/write only — it's a secret (out of the repo and
/// logs; on disk like an SSH key).
fn save_token(path: &Path, token: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(path, token).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

/// A bearer token: 32 random bytes, hex-encoded.
fn generate_token() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        constant_time_eq, generate_request_body, generate_token, inline_refs,
        load_or_generate_token, normalize_tool_schemas, pleasant_title, save_token,
        strip_null_variants, GenerationJobs, LoraArg, McpHandler, MAX_TRACKED_JOBS,
    };
    use serde_json::json;

    #[test]
    fn inline_refs_flattens_local_defs_keeping_ref_siblings() {
        // The set_style shape that failed live: a $ref'd struct param whose type
        // information a client stripped along with $defs. After inlining, the
        // schema must carry the full shape with NO $ref left — and the $ref's
        // sibling description (the per-field doc) must win over the def's own.
        let defs = json!({
            "PadPoint": {
                "description": "the type doc",
                "type": "object",
                "properties": { "x": { "type": "number" }, "y": { "type": "number" } },
                "required": ["x", "y"]
            },
            "Band": { "type": "string", "enum": ["low", "mid", "high"] }
        });
        let mut schema = json!({
            "type": "object",
            "properties": {
                "cursor": { "$ref": "#/$defs/PadPoint", "description": "the field doc" },
                "targets": { "type": "array", "items": { "$ref": "#/$defs/PadPoint" } },
                "band": { "$ref": "#/$defs/Band" }
            }
        });
        inline_refs(&mut schema, defs.as_object().unwrap(), 0);
        let cursor = &schema["properties"]["cursor"];
        assert_eq!(cursor["type"], "object");
        assert_eq!(cursor["description"], "the field doc");
        assert!(cursor.get("$ref").is_none());
        assert_eq!(cursor["properties"]["x"]["type"], "number");
        // Nested position: the array's items inline too.
        let items = &schema["properties"]["targets"]["items"];
        assert!(items.get("$ref").is_none());
        assert_eq!(items["required"], json!(["x", "y"]));
        // Enum defs surface their values — the client can finally see them.
        assert_eq!(schema["properties"]["band"]["enum"], json!(["low", "mid", "high"]));
    }

    #[test]
    fn strip_null_variants_types_optional_params() {
        // The session-4 live failure: schemars emits Option<…> params as
        // draft-2020-12 nullable shapes, a client drops the array-valued
        // `type`/`anyOf`, and the untyped param arrives as a string.
        let mut schema = json!({
            "type": "object",
            "properties": {
                "ramp_ms": { "format": "float", "type": ["number", "null"] },
                "loras": {
                    "items": {
                        "type": "object",
                        "properties": { "sample": { "type": ["string", "null"] } }
                    },
                    "type": ["array", "null"]
                },
                "mode": {
                    "anyOf": [
                        { "description": "the type doc", "enum": ["chord", "onset"], "type": "string" },
                        { "type": "null" }
                    ],
                    "description": "the field doc"
                }
            }
        });
        strip_null_variants(&mut schema, 0);
        assert_eq!(schema["properties"]["ramp_ms"]["type"], "number");
        assert_eq!(schema["properties"]["loras"]["type"], "array");
        // Nested optional fields normalise too.
        let sample = &schema["properties"]["loras"]["items"]["properties"]["sample"];
        assert_eq!(sample["type"], "string");
        // The Option anyOf collapses onto the field; the per-field doc wins.
        let mode = &schema["properties"]["mode"];
        assert!(mode.get("anyOf").is_none());
        assert_eq!(mode["type"], "string");
        assert_eq!(mode["enum"], json!(["chord", "onset"]));
        assert_eq!(mode["description"], "the field doc");
    }

    #[test]
    fn normalized_schemas_carry_no_client_hostile_shapes() {
        // Walk every real tool schema post-normalisation: no $ref/$defs, no
        // nullable type array, no Option anyOf — the shapes MCP clients have
        // been observed to strip (sessions 3 and 4).
        fn check(tool: &str, value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(map) => {
                    assert!(
                        !map.contains_key("$ref") && !map.contains_key("$defs"),
                        "{tool}: $ref/$defs survived normalisation"
                    );
                    if let Some(types) = map.get("type").and_then(|t| t.as_array()) {
                        assert!(
                            !types.iter().any(|t| t == "null"),
                            "{tool}: nullable type array survived"
                        );
                    }
                    if let Some(branches) = map.get("anyOf").and_then(|b| b.as_array()) {
                        assert!(
                            !branches
                                .iter()
                                .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("null")),
                            "{tool}: Option anyOf survived"
                        );
                    }
                    map.values().for_each(|sub| check(tool, sub));
                }
                serde_json::Value::Array(items) => items.iter().for_each(|sub| check(tool, sub)),
                _ => {}
            }
        }
        let mut router = McpHandler::tool_router();
        normalize_tool_schemas(&mut router);
        for route in router.map.values() {
            let schema = serde_json::Value::Object(route.attr.input_schema.as_ref().clone());
            check(route.attr.name.as_ref(), &schema);
        }
        // And the live-failure params specifically end up plainly typed.
        let schema_of = |name: &str| {
            let route = router.map.values().find(|r| r.attr.name == name).unwrap();
            serde_json::Value::Object(route.attr.input_schema.as_ref().clone())
        };
        assert_eq!(schema_of("set_crossfade")["properties"]["ramp_ms"]["type"], "number");
        assert_eq!(schema_of("generate_track")["properties"]["loras"]["type"], "array");
    }

    #[test]
    fn generation_jobs_report_running_then_finished() {
        let jobs = GenerationJobs::default();
        assert!(jobs.report().starts_with("no generation jobs"));
        jobs.begin(7, "track", "Velvet Mirage", "hyperfocus chiptune", Some(1));
        let running: serde_json::Value = serde_json::from_str(&jobs.report()).unwrap();
        assert_eq!(running["jobs"][0]["job"], 7);
        assert_eq!(running["jobs"][0]["status"], "running");
        assert_eq!(running["jobs"][0]["deck"], 1);
        assert_eq!(running["jobs"][0]["detail"], json!(null));
        jobs.finish(7, Ok("loaded onto deck 1".to_string()));
        let done: serde_json::Value = serde_json::from_str(&jobs.report()).unwrap();
        assert_eq!(done["jobs"][0]["status"], "done");
        assert_eq!(done["jobs"][0]["detail"], "loaded onto deck 1");
        jobs.begin(8, "track", "Neon Halo", "acid techno", Some(0));
        jobs.finish(8, Err("generation failed (500)".to_string()));
        // Newest first, failures surfaced as such.
        let both: serde_json::Value = serde_json::from_str(&jobs.report()).unwrap();
        assert_eq!(both["jobs"][0]["job"], 8);
        assert_eq!(both["jobs"][0]["status"], "failed");
        assert_eq!(both["jobs"][1]["job"], 7);
    }

    #[test]
    fn generation_jobs_evict_finished_before_running() {
        let jobs = GenerationJobs::default();
        // Job 0 finished, the rest still running — past the cap the finished
        // one goes; a running job must never be dropped mid-flight.
        for id in 0..MAX_TRACKED_JOBS as u64 {
            jobs.begin(id, "track", "t", "p", None);
        }
        jobs.finish(0, Ok("done".to_string()));
        jobs.begin(99, "track", "t", "p", None);
        let report: serde_json::Value = serde_json::from_str(&jobs.report()).unwrap();
        let ids: Vec<_> = report["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["job"].as_u64().unwrap())
            .collect();
        assert_eq!(ids.len(), MAX_TRACKED_JOBS);
        assert!(!ids.contains(&0), "the finished job should have been evicted");
        assert!(ids.contains(&99) && ids.contains(&1));
    }

    #[test]
    fn pleasant_title_is_two_words_not_the_prompt() {
        let title = pleasant_title();
        assert_eq!(title.split(' ').count(), 2);
        assert!(title.len() < 30); // never a runaway prompt-length name
    }

    #[test]
    fn generate_body_matches_the_server_contract() {
        // The keys + the wire `kind` value must match what `/api/generate` validates.
        let body = generate_request_body("warm pad", 4.0, "music", &[]);
        assert_eq!(body["prompt"], "warm pad");
        assert_eq!(body["seconds"], 4.0);
        assert_eq!(body["kind"], "music");
        // No adapters → no `loras` key at all (the server treats absent and [] alike,
        // but absent is the documented no-LoRA shape).
        assert!(body.get("loras").is_none());
        assert_eq!(
            generate_request_body("epic", 60.0, "track", &[])["kind"],
            "track"
        );

        // With adapters the stack serialises as the server's `loras[]` contract.
        let stacked = generate_request_body(
            "chiptune",
            30.0,
            "track",
            &[LoraArg {
                name: "medium/zentai-chiptune".to_string(),
                strength: 1.2,
            }],
        );
        assert_eq!(stacked["loras"][0]["name"], "medium/zentai-chiptune");
        assert_eq!(stacked["loras"][0]["strength"], 1.2f32);
    }

    #[test]
    fn token_is_64_hex_chars_and_unique() {
        let token = generate_token();
        assert_eq!(token.len(), 64); // 32 bytes, two hex chars each
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        // Random — two draws differ.
        assert_ne!(token, generate_token());
    }

    #[test]
    fn token_persists_across_loads_and_a_rewrite_rotates_it() {
        let dir = std::env::temp_dir().join(format!("lsdj-mcp-{}", generate_token()));
        let path = dir.join("mcp-token");
        // First load mints + persists; the second reuses the same value (stable
        // across launches).
        let first = load_or_generate_token(&path);
        assert_eq!(load_or_generate_token(&path), first);
        // A rewrite (what `rotate` does) changes the persisted value.
        let rotated = generate_token();
        save_token(&path, &rotated);
        assert_ne!(rotated, first);
        assert_eq!(load_or_generate_token(&path), rotated);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn constant_time_eq_accepts_only_an_exact_match() {
        // Locks correctness, not the timing property (which can't be asserted without
        // flakiness): the bearer-token check must accept only an exact byte match.
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"Bearer abc", b"Bearer abc"));
        // Same length, one byte off — rejected.
        assert!(!constant_time_eq(b"Bearer abc", b"Bearer abd"));
        // A differing length (a prefix) — rejected.
        assert!(!constant_time_eq(b"Bearer abc", b"Bearer abcd"));
    }
}
