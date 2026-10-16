//! The Tauri IPC command surface for the audio engine (Phase 2, step 2).
//!
//! One `#[tauri::command]` per [`Host`] control op, plus the read-back commands
//! (`engine_telemetry`, `track_status`, `loop_slots`). Each command wraps the
//! matching [`Host`] method, which enqueues a command over the host's wait-free
//! channel to the render thread (or, for read-backs, reads the published
//! snapshot / telemetry atomics). Nothing here touches the cpal callback.
//!
//! # Trust boundary
//!
//! The webview is an untrusted caller (`.claude/rules/security.md`): every deck
//! index, band, FX name, slot, and numeric is validated/clamped HERE before it
//! reaches the engine, whose `&mut self` methods `assert!` on a bad deck index (a
//! programming-error contract, not a runtime one). An out-of-range deck/slot is a
//! silent no-op rather than a panic that would take down the render thread; an
//! unknown FX name is rejected with an `Err` the webview can surface. The engine
//! itself already clamps the *values* (crossfade, volume, EQ, rate) into range.
//!
//! The argument/return types are plain serde structs/enums so the surface is
//! self-describing to the webview; the engine types stay serde-free (the DTOs
//! convert at the boundary).

use serde::{Deserialize, Serialize};
use serde_json::json;
use lsdj_engine::host::{Health, Host};
use lsdj_engine::{
    EqBand, FxKind, LoopRegion, LoopSlotStatus, TrackStatus, CHANNELS, DECK_COUNT, LOOP_SLOT_COUNT,
    SAMPLE_RATE,
};

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;

use crate::analysis::live::AnalysisFeed;
use crate::analysis::track::{DeckTrack, TrackAnalysis};
use crate::library;
use crate::midi::notes::NoteSteering;
use crate::playlists::Playlist;
use crate::samples::{NewSample, SampleEntry, SampleLibrary};
use crate::sidecar::{PcmTaps, Sidecars};
use crate::songs::{NewSong, SongEntry, SongLibrary};
use crate::store::{
    GenerationField, GenerationPatch, InterfaceState, InterfaceStore, PerformanceSnap,
};

/// Reject a deck index outside `[0, DECK_COUNT)`. A bad index from the webview is
/// a no-op (the command returns without touching the engine), never a panic. Shared
/// with the MCP server (`crate::mcp`), so an agent's deck index hits the same guard.
pub(crate) fn valid_deck(deck: usize) -> bool {
    deck < DECK_COUNT
}

/// Reject a loop-slot index outside `[0, LOOP_SLOT_COUNT)`.
fn valid_slot(slot: usize) -> bool {
    slot < LOOP_SLOT_COUNT
}

/// The EQ band as it crosses the IPC boundary. Mirrors the engine [`EqBand`] but
/// is a serde enum so the webview names bands by intent, not a magic index.
/// `JsonSchema` too, so the MCP tool surface (`crate::mcp`) reuses it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum EqBandArg {
    Low,
    Mid,
    High,
}

impl From<EqBandArg> for EqBand {
    fn from(band: EqBandArg) -> Self {
        match band {
            EqBandArg::Low => EqBand::Low,
            EqBandArg::Mid => EqBand::Mid,
            EqBandArg::High => EqBand::High,
        }
    }
}

/// The Color FX kind as it crosses the IPC boundary (the six `fx.ts` effects).
/// A serde enum so an unknown effect name is a clean deserialization `Err` rather
/// than a silent fallback. `JsonSchema` too, for the MCP tool surface (`crate::mcp`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FxKindArg {
    Filter,
    DubEcho,
    Space,
    Crush,
    Noise,
    Sweep,
}

impl From<FxKindArg> for FxKind {
    fn from(kind: FxKindArg) -> Self {
        match kind {
            FxKindArg::Filter => FxKind::Filter,
            FxKindArg::DubEcho => FxKind::DubEcho,
            FxKindArg::Space => FxKind::Space,
            FxKindArg::Crush => FxKind::Crush,
            FxKindArg::Noise => FxKind::Noise,
            FxKindArg::Sweep => FxKind::Sweep,
        }
    }
}

/// A loop region (`[start, end)` in frames) for the wire. Mirrors the engine
/// [`LoopRegion`].
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LoopRegionDto {
    pub start: u64,
    pub end: u64,
}

impl From<LoopRegion> for LoopRegionDto {
    fn from(region: LoopRegion) -> Self {
        LoopRegionDto {
            start: region.start,
            end: region.end,
        }
    }
}

/// A playback deck's transport for the wire. Mirrors the engine [`TrackStatus`]
/// (positions in frames; the webview converts to seconds where needed).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackStatusDto {
    pub playhead: f64,
    pub playing: bool,
    pub duration_frames: u64,
    pub rate: f64,
    pub ended: bool,
    pub loop_region: Option<LoopRegionDto>,
}

impl From<TrackStatus> for TrackStatusDto {
    fn from(status: TrackStatus) -> Self {
        TrackStatusDto {
            playhead: status.playhead,
            playing: status.playing,
            duration_frames: status.duration_frames,
            rate: status.rate,
            ended: status.ended,
            loop_region: status.loop_region.map(LoopRegionDto::from),
        }
    }
}

/// One loop slot's truthful state for LEDs / telemetry (the shell's `LoopSlot`).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LoopSlotDto {
    pub filled: bool,
    pub playing: bool,
}

impl From<LoopSlotStatus> for LoopSlotDto {
    fn from(slot: LoopSlotStatus) -> Self {
        LoopSlotDto {
            filled: slot.filled,
            playing: slot.playing,
        }
    }
}

/// The engine health snapshot for the wire (the `engine_telemetry` return).
/// Mirrors [`Health`]: per-deck input-ring fill + the output-ring fill +
/// underruns + master peak + master gain reduction. BPM is intentionally absent
/// for now (no tempo analysis yet).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthDto {
    pub output_ring_frames: usize,
    pub deck_ring_frames: Vec<usize>,
    pub deck_underruns: u64,
    pub output_underruns: u64,
    pub master_peak: f32,
    pub master_gain_reduction_db: f32,
    /// Per-deck post-fader level (the channel meters, `getLevel`).
    pub deck_levels: Vec<f32>,
    /// Frames rendered since start — the shared audio clock (`getContextTime`).
    pub context_frames: u64,
}

impl From<Health> for HealthDto {
    fn from(health: Health) -> Self {
        HealthDto {
            output_ring_frames: health.output_ring_frames,
            deck_ring_frames: health.deck_ring_frames.to_vec(),
            deck_underruns: health.deck_underruns,
            output_underruns: health.output_underruns,
            master_peak: health.master_peak,
            master_gain_reduction_db: health.master_gain_reduction_db,
            deck_levels: health.deck_levels.to_vec(),
            context_frames: health.context_frames,
        }
    }
}

/// A loaded track's min/max envelope for the wire (the waveform overview). Empty
/// vecs when no track is loaded on the deck.
#[derive(Debug, Clone, Serialize)]
pub struct TrackPeaksDto {
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

/// One IPC round-trip carrying everything the per-frame UI reads back: health
/// (meters, clock, ring stats), each deck's transport, and each deck's loop
/// slots. The webview polls THIS once per animation frame and caches it, so the
/// many synchronous `getLevel`/`getTrackStatus`/… getters read a fresh local
/// snapshot instead of issuing one IPC call each.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineSnapshotDto {
    pub health: HealthDto,
    /// Per-deck transport (`null` off Playback), indexed by deck.
    pub tracks: Vec<Option<TrackStatusDto>>,
    /// Per-deck loop-slot status, indexed by deck then slot.
    pub loops: Vec<Vec<LoopSlotDto>>,
}

// --- Mixer / channel control ---

#[tauri::command]
pub fn set_crossfade(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    position: f32,
) {
    state.set_crossfade(position);
    store.set_crossfade(position);
}

#[tauri::command]
pub fn set_eq(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    band: EqBandArg,
    value: f32,
) {
    if valid_deck(deck) {
        state.set_eq(deck, band.into(), value);
        store.set_eq(deck, band.into(), value);
    }
}

#[tauri::command]
pub fn set_volume(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    gain: f32,
) {
    if valid_deck(deck) {
        state.set_volume(deck, gain);
        store.set_volume(deck, gain);
    }
}

#[tauri::command]
pub fn set_fx(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    kind: FxKindArg,
) {
    if valid_deck(deck) {
        state.set_fx(deck, kind.into());
        store.set_fx(deck, kind.into());
    }
}

#[tauri::command]
pub fn set_fx_amount(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    amount: f32,
) {
    if valid_deck(deck) {
        state.set_fx_amount(deck, amount);
        store.set_fx_amount(deck, amount);
    }
}

/// Remove a deck's Color FX (no effect selected) — mirrors `setFx(null)`.
#[tauri::command]
pub fn clear_fx(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
) {
    if valid_deck(deck) {
        state.clear_fx(deck);
        store.clear_fx(deck);
    }
}

/// Chain-head trim in dB (M17 gain staging; 0 dB = unity).
#[tauri::command]
pub fn set_trim(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    db: f32,
) {
    if valid_deck(deck) {
        state.set_trim(deck, db);
        store.set_trim(deck, db);
    }
}

/// On-air state (M10 primed deck): off-air mutes the master feed only.
#[tauri::command]
pub fn set_on_air(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    on: bool,
) {
    if valid_deck(deck) {
        state.set_on_air(deck, on);
        store.set_on_air(deck, on);
    }
}

/// Headphone-cue (PFL) tap for a deck (Slice 5).
#[tauri::command]
pub fn set_cue(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    on: bool,
) {
    if valid_deck(deck) {
        state.set_cue(deck, on);
        store.set_cue(deck, on);
    }
}

/// Cue/master headphone blend (0 = cue only, 1 = master).
#[tauri::command]
pub fn set_cue_mix(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    position: f32,
) {
    state.set_cue_mix(position);
    store.set_cue_mix(position);
}

/// Preview a decoded buffer into the headphone/cue feed (ADR-0027). The payload is
/// raw interleaved-stereo f32 PCM (the webview decodes + resamples a WAV, like
/// [`load_track`]); no header — the preview is engine-wide, not per-deck, and never
/// reaches the master.
#[tauri::command]
pub fn audition_play(
    state: tauri::State<'_, Host>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let payload = raw_payload(&request)?;
    state.audition_play(pcm_from_le_bytes(payload));
    Ok(())
}

/// Stop the headphone preview (ADR-0027).
#[tauri::command]
pub fn audition_stop(state: tauri::State<'_, Host>) {
    state.audition_stop();
}

/// Resolve the folder takes are written to: the webview-chosen `folder`, or the OS
/// Downloads folder when none is set (recordings' historical default). `folder` is a
/// directory the user picked through the native dialog; only the filename is minted
/// server-side, so a take can never land outside the directory the user chose.
fn recordings_dir(app: &tauri::AppHandle, folder: &str) -> Result<std::path::PathBuf, String> {
    if folder.is_empty() {
        app.path()
            .download_dir()
            .map_err(|e| format!("no Downloads folder: {e}"))
    } else {
        Ok(std::path::PathBuf::from(folder))
    }
}

/// Start recording the master bus (exactly the speaker feed), streaming it straight
/// to a 16-bit PCM WAV in `folder` (empty = the OS Downloads folder, today's
/// default) and returning the file path. Streaming to disk means the take is bounded
/// by free space, not RAM — only the WAV format's own 32-bit ceiling (~6 h at 48 kHz
/// stereo) caps the length, far past the old 30-min in-memory limit.
///
/// The path is minted here, at start, because the file is opened now: `name` is an
/// untrusted display stem, sanitised to a single filename component via
/// [`library::safe_stem`] (so it can't escape the folder). [`library::create_unique_wav`]
/// then opens a fresh take atomically (`create_new`), so the pick-and-open is one step
/// — no window for another process to slip a file or symlink into the path, and an
/// existing take is never truncated.
#[tauri::command]
pub fn start_recording(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    app: tauri::AppHandle,
    folder: String,
    name: String,
) -> Result<String, String> {
    let dir = recordings_dir(&app, &folder)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create recordings folder: {e}"))?;
    let stem = library::safe_stem(&name, "lsdj-recording");
    let (file, path) = library::create_unique_wav(&dir, &stem)?;
    if let Err(e) = state.start_recording(file) {
        // The take never started (e.g. one already in progress); don't leave the
        // empty file we just created lying around.
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    let path = path.to_string_lossy().into_owned();
    // The recorder's state lives in the store (ADR-0020): a webview reload —
    // or an agent — reads the truth instead of a local flag.
    store.set_recording(true, Some(path.clone()));
    Ok(path)
}

/// Stop recording: the engine flushes the remaining audio, patches the WAV header,
/// and closes the file that [`start_recording`] opened. The path was already
/// returned at start, so nothing comes back here but success or a write error.
#[tauri::command]
pub fn stop_recording(
    state: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
) -> Result<(), String> {
    let result = state.stop_recording();
    if result.is_ok() {
        // Keep the finished take's path readable until the next one starts.
        let path = store.snapshot().recording.path;
        store.set_recording(false, path);
    }
    result
}

/// Reveal the recordings folder in the OS file manager (driven from Rust, no opener
/// capability granted to the webview). Empty `folder` falls back to Downloads — where
/// takes land by default — and the folder is created first so it opens even before the
/// first recording.
#[tauri::command]
pub fn open_recordings_folder(app: tauri::AppHandle, folder: String) -> Result<(), String> {
    let dir = recordings_dir(&app, &folder)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create recordings folder: {e}"))?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| format!("cannot open recordings folder: {e}"))
}

// --- Playback deck transport ---

/// Decode interleaved-stereo f32 (little-endian) from a raw byte payload. The
/// webview ships decoded PCM as bytes (a multi-MB track as a JSON number array is
/// not viable — Tauri's binary IPC carries it as `Vec<u8>`); any trailing partial
/// frame is ignored.
fn pcm_from_le_bytes(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Read a little-endian `u32` at `offset` from a binary payload prefix.
fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset + 4;
    let slice = bytes.get(offset..end)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// The raw bytes of a binary IPC request. The webview invokes the binary commands
/// with a `Uint8Array` as the sole argument, which Tauri's IPC delivers as an
/// [`InvokeBody::Raw`](tauri::ipc::InvokeBody::Raw) (the fast binary path, not a
/// JSON args map). The command MUST read the body here: binding a `payload: Vec<u8>`
/// parameter instead makes Tauri look for a JSON "payload" key and reject the raw
/// bytes ("expected a value for key payload but the IPC call used a bytes payload").
fn raw_payload<'a>(request: &'a tauri::ipc::Request<'_>) -> Result<&'a [u8], String> {
    match request.body() {
        tauri::ipc::InvokeBody::Raw(bytes) => Ok(bytes.as_slice()),
        tauri::ipc::InvokeBody::Json(_) => Err("expected a raw bytes payload".to_string()),
    }
}

/// The audio extensions the folder browser surfaces (mirrors the frontend
/// `AUDIO_FILE` regex). Compared case-insensitively.
const AUDIO_EXTENSIONS: [&str; 7] = ["wav", "mp3", "flac", "m4a", "ogg", "aif", "aiff"];

/// Whether `path` has one of [`AUDIO_EXTENSIONS`] (case-insensitive).
fn is_audio_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Upper bound on a file [`read_audio_file`] will return. Generous for a long
/// high-res track yet bounds a pathological huge file (a blind read into memory).
const MAX_AUDIO_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// List the audio file NAMES directly inside `dir` (non-recursive), sorted
/// case-insensitively (matching the web path's `localeCompare`). `dir` is a folder
/// the user just chose in the native picker (WKWebView has no File System Access
/// API). Only names are returned — [`read_audio_file`] re-derives and re-validates
/// the path from `dir` + name, so the webview never supplies a path to read. A read
/// error (missing / not a directory / no permission) surfaces to the webview.
#[tauri::command]
pub fn list_audio_files(dir: String) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(&dir).map_err(|e| format!("cannot read folder: {e}"))?;
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_audio_file(path))
        .filter_map(|path| path.file_name()?.to_str().map(str::to_string))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    Ok(names)
}

/// Read one audio file the user picked, SCOPED to the folder they chose: `name`
/// must be a plain filename resolving to a regular file directly inside `dir`. The
/// webview is an untrusted caller (module header / `.claude/rules/security.md`), so
/// THIS — not the extension — is the boundary: without it a raw path would let a
/// compromised webview read any file the user can (SSH keys, keychains, cookie DBs).
/// Canonicalising both sides defeats `..` traversal and symlinks that escape the
/// folder; the regular-file + size checks reject FIFOs / devices / huge files that a
/// blind `read` would hang or OOM on. `async` keeps the read off the main thread.
/// Returns the bytes as binary (a track is many MB — `ipc::Response`).
#[tauri::command]
pub async fn read_audio_file(dir: String, name: String) -> Result<tauri::ipc::Response, String> {
    read_scoped_audio(&dir, &name).map(tauri::ipc::Response::new)
}

/// The scoped folder read behind [`read_audio_file`] and the shell-side track
/// load ([`load_track_file`], ADR-0030): `name` must be a plain filename
/// resolving to a regular audio file directly inside `dir`.
fn read_scoped_audio(dir: &str, name: &str) -> Result<Vec<u8>, String> {
    // `name` must be a single ordinary path component — never a path, "..", or
    // absolute — so it cannot point outside `dir`.
    let mut comps = std::path::Path::new(&name).components();
    if !matches!(
        (comps.next(), comps.next()),
        (Some(std::path::Component::Normal(_)), None)
    ) {
        return Err("invalid file name".to_string());
    }
    // Canonicalise both sides and require the resolved file is a DIRECT CHILD of the
    // resolved chosen folder — this rejects `..` and any symlink whose target
    // escapes the folder.
    let base = std::fs::canonicalize(dir).map_err(|e| format!("cannot resolve folder: {e}"))?;
    let target =
        std::fs::canonicalize(base.join(name)).map_err(|e| format!("cannot resolve file: {e}"))?;
    if target.parent() != Some(base.as_path()) {
        return Err("file is outside the chosen folder".to_string());
    }
    if !is_audio_file(&target) {
        return Err("not an audio file".to_string());
    }
    let meta = std::fs::metadata(&target).map_err(|e| format!("cannot stat file: {e}"))?;
    if !meta.is_file() {
        return Err("not a regular file".to_string());
    }
    if meta.len() > MAX_AUDIO_BYTES {
        return Err("file is too large".to_string());
    }
    std::fs::read(&target).map_err(|e| format!("cannot read file: {e}"))
}

/// The generated-songs take list, reconciled against the folder (the [`SongLibrary`]
/// owns the registry + scan). The webview calls this at startup to restore the list.
/// `async` keeps the directory scan + registry rewrite off the main thread, like the
/// sibling `read_generated_song` / `save_generated_song`.
#[tauri::command]
pub async fn list_generated_songs(
    library: tauri::State<'_, SongLibrary>,
) -> Result<Vec<SongEntry>, String> {
    library.list()
}

/// Persist a freshly composed take and return its registry entry (the webview keeps
/// the filename to reload / delete the take later). Invoked over binary IPC with a
/// payload of `[u32 LE meta-JSON byte-length][meta JSON utf-8][WAV bytes]` — a song
/// WAV is many MB (a full render is 100 MB+), so a JSON args map would be megabytes of
/// text (mirrors `load_track` / `deck_embed_sample`). The meta carries the title,
/// prompt, and model; the title is untrusted and sanitised to one filename component
/// inside [`SongLibrary::record`]. `async` keeps the write off the main thread — a big
/// WAV would otherwise stall the UI, like the read in [`read_audio_file`].
#[tauri::command]
pub async fn save_generated_song(
    library: tauri::State<'_, SongLibrary>,
    request: tauri::ipc::Request<'_>,
) -> Result<SongEntry, String> {
    let payload = raw_payload(&request)?;
    let meta_len = read_u32_le(payload, 0).ok_or("save payload too short")? as usize;
    let meta_end = 4 + meta_len;
    let meta_bytes = payload.get(4..meta_end).ok_or("save payload meta truncated")?;
    let meta: NewSong =
        serde_json::from_slice(meta_bytes).map_err(|e| format!("bad song metadata: {e}"))?;
    library.record(meta, &payload[meta_end..])
}

/// Read one persisted song's bytes back, scoped to the songs folder (the webview
/// supplies only the plain filename). Returns the bytes as binary — a track is many MB
/// (`ipc::Response`). `async` keeps the read off the main thread, like
/// [`read_audio_file`].
#[tauri::command]
pub async fn read_generated_song(
    library: tauri::State<'_, SongLibrary>,
    name: String,
) -> Result<tauri::ipc::Response, String> {
    library.read(&name).map(tauri::ipc::Response::new)
}

/// Cut `start..end` seconds (`end` omitted = to the end) of a persisted song into
/// the 44.1 kHz 16-bit stereo WAV Stable Audio 3 takes as init audio, for the
/// Generate tab's audio-to-audio source. Binary, like [`read_generated_song`].
#[tauri::command]
pub async fn song_source_clip(
    library: tauri::State<'_, SongLibrary>,
    name: String,
    start: f64,
    end: Option<f64>,
) -> Result<tauri::ipc::Response, String> {
    library
        .source_clip(&name, start, end)
        .map(|clip| tauri::ipc::Response::new(clip.wav))
}

/// Delete a persisted song: move it to the OS Trash (recoverable), drop it from the
/// registry and from every playlist, so the take list and the folder stay in sync
/// without a restart. `async`
/// keeps the trash + registry I/O off the main thread, like the sibling commands.
#[tauri::command]
pub async fn delete_generated_song(
    library: tauri::State<'_, SongLibrary>,
    name: String,
) -> Result<(), String> {
    library.remove(&name)
}

/// Set (1–5) or clear (`None`) a song's star rating and return the updated row. The
/// rating lives in the registry, so it survives a relaunch and MCP sees it. Out of
/// range or an unknown `name` is an `Err`; `async` keeps the registry write off the
/// main thread, like the sibling commands.
#[tauri::command]
pub async fn rate_generated_song(
    library: tauri::State<'_, SongLibrary>,
    name: String,
    rating: Option<u8>,
) -> Result<SongEntry, String> {
    library.rate(&name, rating, true)
}

/// Retitle a song and return the updated row. Only the registry title changes — the
/// file stays the identity decks, MCP, and the re-list hold. The title is untrusted
/// and validated in [`SongLibrary::rename`].
#[tauri::command]
pub async fn rename_generated_song(
    library: tauri::State<'_, SongLibrary>,
    name: String,
    title: String,
) -> Result<SongEntry, String> {
    library.rename(&name, &title)
}

/// Replace a song's tags and return the updated row carrying the shell-normalised
/// list (lowercased, `#` dropped, spaces to `-`), which the webview adopts.
#[tauri::command]
pub async fn tag_generated_song(
    library: tauri::State<'_, SongLibrary>,
    name: String,
    tags: Vec<String>,
) -> Result<SongEntry, String> {
    library.tag(&name, tags)
}

/// Every playlist (`generated_songs/playlists/*.m3u8`), each reconciled against the
/// songs on disk. `async` keeps the folder reads off the main thread.
#[tauri::command]
pub async fn list_playlists(
    library: tauri::State<'_, SongLibrary>,
) -> Result<Vec<Playlist>, String> {
    library.list_playlists()
}

/// Create or replace a playlist with exactly `files` (create, add, remove, and
/// reorder all send the whole list) and return what was stored — the name as a safe
/// stem, files missing from the songs folder dropped, duplicates collapsed.
#[tauri::command]
pub async fn save_playlist(
    library: tauri::State<'_, SongLibrary>,
    name: String,
    files: Vec<String>,
) -> Result<Playlist, String> {
    library.save_playlist(&name, files)
}

/// Rename a playlist and return it under its new name; a name already taken is
/// refused rather than suffixed.
#[tauri::command]
pub async fn rename_playlist(
    library: tauri::State<'_, SongLibrary>,
    from: String,
    to: String,
) -> Result<Playlist, String> {
    library.rename_playlist(&from, &to)
}

/// Move a playlist to the OS Trash (recoverable). The songs it listed are untouched.
#[tauri::command]
pub async fn delete_playlist(
    library: tauri::State<'_, SongLibrary>,
    name: String,
) -> Result<(), String> {
    library.delete_playlist(&name)
}

/// Open the generated-songs folder in the OS file manager so the user can copy songs
/// out. Creates it first, so the button opens a real folder even before the first
/// save. The opener plugin is driven from Rust (not the webview), so no opener
/// capability is granted to the untrusted webview.
#[tauri::command]
pub fn open_songs_folder(
    library: tauri::State<'_, SongLibrary>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let dir = library.dir();
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create songs folder: {e}"))?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| format!("cannot open songs folder: {e}"))
}

// --- Generated samples (short loops): the SampleLibrary counterpart of the songs
// commands above (ADR-0022). Same binary-IPC save/read shapes; the registry carries
// the extra `oneShot` reload needs.

/// The generated-samples list, reconciled against the folder. The webview calls this
/// at startup and when the Samples tab opens (a deck auto-saves out-of-band, so a
/// re-list surfaces a freeze/pad made since).
#[tauri::command]
pub async fn list_generated_samples(
    library: tauri::State<'_, SampleLibrary>,
) -> Result<Vec<SampleEntry>, String> {
    library.list()
}

/// Persist a sample whose WAV the webview already holds (a deck generated pad's
/// backend response, or a Samples-tab composition) and return its registry entry.
/// Same binary frame as [`save_generated_song`]: `[u32 LE meta-JSON byte-length]
/// [meta JSON utf-8][WAV bytes]`. The untrusted title is sanitised to one filename
/// component inside [`SampleLibrary::record`].
#[tauri::command]
pub async fn save_generated_sample(
    library: tauri::State<'_, SampleLibrary>,
    request: tauri::ipc::Request<'_>,
) -> Result<SampleEntry, String> {
    let payload = raw_payload(&request)?;
    let meta_len = read_u32_le(payload, 0).ok_or("save payload too short")? as usize;
    let meta_end = 4 + meta_len;
    let meta_bytes = payload.get(4..meta_end).ok_or("save payload meta truncated")?;
    let meta: NewSample =
        serde_json::from_slice(meta_bytes).map_err(|e| format!("bad sample metadata: {e}"))?;
    library.record(meta, &payload[meta_end..])
}

/// Read one persisted sample's bytes back as binary, scoped to the samples folder
/// (the webview supplies only the plain filename), for reloading into a deck slot.
#[tauri::command]
pub async fn read_generated_sample(
    library: tauri::State<'_, SampleLibrary>,
    name: String,
) -> Result<tauri::ipc::Response, String> {
    library.read(&name).map(tauri::ipc::Response::new)
}

/// Delete a persisted sample: move it to the OS Trash and drop it from the registry.
#[tauri::command]
pub async fn delete_generated_sample(
    library: tauri::State<'_, SampleLibrary>,
    name: String,
) -> Result<(), String> {
    library.remove(&name)
}

/// Set (1–5) or clear (`None`) a sample's star rating and return the updated row.
#[tauri::command]
pub async fn rate_generated_sample(
    library: tauri::State<'_, SampleLibrary>,
    name: String,
    rating: Option<u8>,
) -> Result<SampleEntry, String> {
    library.rate(&name, rating, true)
}

/// Retitle a sample (registry title only, never the file) and return the updated row.
#[tauri::command]
pub async fn rename_generated_sample(
    library: tauri::State<'_, SampleLibrary>,
    name: String,
    title: String,
) -> Result<SampleEntry, String> {
    library.rename(&name, &title)
}

/// Replace a sample's tags and return the updated row with the normalised list.
#[tauri::command]
pub async fn tag_generated_sample(
    library: tauri::State<'_, SampleLibrary>,
    name: String,
    tags: Vec<String>,
) -> Result<SampleEntry, String> {
    library.tag(&name, tags)
}

/// Save `start..end` seconds of a song as a NEW library row (the crop panel); the
/// original is untouched. Name and window are validated in [`SongLibrary::crop`].
#[tauri::command]
pub async fn crop_generated_song(
    library: tauri::State<'_, SongLibrary>,
    name: String,
    start: f64,
    end: f64,
) -> Result<SongEntry, String> {
    library.crop(&name, start, end)
}

/// [`crop_generated_song`] for a sample.
#[tauri::command]
pub async fn crop_generated_sample(
    library: tauri::State<'_, SampleLibrary>,
    name: String,
    start: f64,
    end: f64,
) -> Result<SampleEntry, String> {
    library.crop(&name, start, end)
}

/// A library file's waveform for the crop panel (it is on no deck, so
/// `track_peaks` can't serve it), plus its length in seconds.
#[derive(Debug, Clone, Serialize)]
pub struct ClipPeaksDto {
    pub min: Vec<f32>,
    pub max: Vec<f32>,
    pub duration: f64,
}

/// The folder a crop-panel command reads from: `"songs"` or `"samples"`.
fn crop_library_dir<'a>(
    songs: &'a SongLibrary,
    samples: &'a SampleLibrary,
    library: &str,
) -> Result<&'a std::path::Path, String> {
    match library {
        "songs" => Ok(songs.dir()),
        "samples" => Ok(samples.dir()),
        _ => Err(format!("unknown library {library}")),
    }
}

/// Read a library file scoped to its folder, with its extension as a decode hint.
fn read_library_clip(
    songs: &SongLibrary,
    samples: &SampleLibrary,
    library: &str,
    name: &str,
) -> Result<(Vec<u8>, Option<String>), String> {
    let dir = crop_library_dir(songs, samples, library)?;
    let bytes = library::read_scoped(dir, name, library::MAX_AUDIO_BYTES)?;
    let extension = std::path::Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_string);
    Ok((bytes, extension))
}

#[tauri::command]
pub async fn library_clip_peaks(
    songs: tauri::State<'_, SongLibrary>,
    samples: tauri::State<'_, SampleLibrary>,
    library: String,
    name: String,
    buckets: usize,
) -> Result<ClipPeaksDto, String> {
    let (bytes, extension) = read_library_clip(&songs, &samples, &library, &name)?;
    let (min, max, duration) = crate::decode::clip_peaks(bytes, extension.as_deref(), buckets)?;
    Ok(ClipPeaksDto { min, max, duration })
}

/// The crop exactly as Save would write it, for the panel's selection preview.
/// Binary, like [`read_generated_song`]; nothing is written.
#[tauri::command]
pub async fn library_crop_preview(
    songs: tauri::State<'_, SongLibrary>,
    samples: tauri::State<'_, SampleLibrary>,
    library: String,
    name: String,
    start: f64,
    end: f64,
) -> Result<tauri::ipc::Response, String> {
    let (bytes, extension) = read_library_clip(&songs, &samples, &library, &name)?;
    crate::decode::crop_wav(bytes, extension.as_deref(), start, end).map(tauri::ipc::Response::new)
}

/// Open the generated-samples folder in the OS file manager (driven from Rust, no
/// opener capability granted to the webview), creating it first.
#[tauri::command]
pub fn open_samples_folder(
    library: tauri::State<'_, SampleLibrary>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let dir = library.dir();
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create samples folder: {e}"))?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| format!("cannot open samples folder: {e}"))
}

/// Persist a deck freeze capture to the samples library. Unlike a generated pad, a
/// freeze's audio lives ONLY in the engine slot (the webview never holds its WAV), so
/// this reads the EXACT stored slot buffer through the engine — any loop length, no
/// drift — encodes it as a float32 WAV, and records it. An empty slot or bad
/// deck/slot is an `Err` the webview surfaces. Blocks briefly on the engine
/// round-trip + write, like [`capture_sample`].
#[tauri::command]
pub fn save_loop_slot(
    state: tauri::State<'_, Host>,
    library: tauri::State<'_, SampleLibrary>,
    deck: usize,
    slot: usize,
    meta: NewSample,
) -> Result<SampleEntry, String> {
    if !valid_deck(deck) || !valid_slot(slot) {
        return Err("invalid deck or slot".to_string());
    }
    let samples = state
        .read_loop_slot(deck, slot)
        .ok_or("the loop slot is empty")?;
    library.record(meta, &encode_wav_f32(&samples))
}

/// Encode interleaved-stereo f32 samples as a 48 kHz IEEE-float WAV (`WAVE_FORMAT_
/// IEEE_FLOAT`, the same shape the backend's `float32_wav` produces and the webview's
/// `decodeAudioData` reads). A minimal float PCM WAV writer, mirroring the recorder's
/// streaming int16 WAV but lossless for a loop's f32 buffer.
fn encode_wav_f32(samples: &[f32]) -> Vec<u8> {
    let channels = CHANNELS as u32;
    let sample_rate = SAMPLE_RATE;
    let bits = 32u32;
    let byte_rate = sample_rate * channels * bits / 8;
    let block_align = (channels * bits / 8) as u16;
    let data_len = (samples.len() * 4) as u32;

    let mut wav = Vec::with_capacity(44 + samples.len() * 4);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    wav.extend_from_slice(&(channels as u16).to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&(bits as u16).to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

/// Where a deck track loads from (ADR-0030): the same scoped references the
/// webview already lists and fetches bytes with — never a raw path. The shell
/// resolves, reads, decodes, and analyses; decoded PCM never crosses the IPC
/// boundary in either direction.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TrackSource {
    /// A file directly inside the folder the user picked in the native dialog
    /// (the [`read_audio_file`] scoping rules).
    Folder { dir: String, name: String },
    /// A generated song in the library (its registry filename).
    Song { name: String },
}

/// What a track load returns: the numbers the webview's deck state needs.
/// The band profile ships separately ([`track_bands`], fetched once and
/// cached), the peaks via the existing [`track_peaks`].
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedTrackDto {
    pub duration: f64,
    /// The offline honesty-gated tempo (grid-refined when a grid exists).
    pub bpm: Option<f64>,
    pub grid: Option<crate::analysis::grid::Beatgrid>,
}

/// The CPU-heavy half of a load (ADR-0030): decode + resample to 48 kHz, run
/// the offline analyses (coarse tempo → beatgrid → bands), and lay the buffer
/// out in the engine wire format. Touches no engine or analysis state, so it
/// runs on a blocking thread — seconds of work for a long track must not sit
/// on the async-command runtime and starve other IPC.
struct AnalyzedTrack {
    interleaved: Vec<f32>,
    duration: f64,
    bpm: Option<f64>,
    grid: Option<crate::analysis::grid::Beatgrid>,
    bands: crate::analysis::bands::TrackBands,
}

fn analyze_track(bytes: Vec<u8>, extension: Option<&str>) -> Result<AnalyzedTrack, String> {
    let decoded = crate::decode::decode_to_48k(bytes, extension)?;
    let coarse = crate::analysis::beat::track_bpm(&decoded.left, &decoded.right, SAMPLE_RATE as f64);
    let grid =
        crate::analysis::grid::track_beatgrid(&decoded.left, &decoded.right, SAMPLE_RATE as f64, coarse);
    let bands = crate::analysis::bands::track_bands(&decoded.left, &decoded.right, SAMPLE_RATE as f64);
    // The refined grid BPM and the coarse verdict collapse to one number (M20).
    let bpm = grid.map(|g| g.bpm).or(coarse);
    Ok(AnalyzedTrack {
        interleaved: decoded.interleaved(),
        duration: decoded.duration_seconds(),
        bpm,
        grid,
        bands,
    })
}

/// The install half: hand the analysed buffer to the engine and set the
/// deck's echo clock from the track tempo. The LIVE analysis is reset first
/// and **waited on** — its blank hand-off must land before the track period
/// below, or a stale blank from the analysis thread could stomp the
/// freshly-set clock (the ownership hand-off, ADR-0025).
fn install_track(
    host: &Host,
    feed: &AnalysisFeed,
    tracks: &TrackAnalysis,
    deck: usize,
    analyzed: AnalyzedTrack,
) -> LoadedTrackDto {
    feed.reset_blocking(deck, host.health().context_frames as f64);
    host.load_track(deck, analyzed.interleaved);
    host.set_beat_period(deck, analyzed.bpm.map(|b| (60.0 / b) as f32));
    tracks.set(
        deck,
        DeckTrack {
            bpm: analyzed.bpm,
            bands: analyzed.bands,
        },
    );
    LoadedTrackDto {
        duration: analyzed.duration,
        bpm: analyzed.bpm,
        grid: analyzed.grid,
    }
}

/// Load a track onto a deck by scoped reference (ADR-0030): the shell reads,
/// decodes (symphonia), resamples, analyses, and installs — the webview gets
/// back `{duration, bpm, grid}` and fetches bands/peaks once. The decode +
/// analyses run on a `spawn_blocking` thread: seconds of CPU for a long
/// track, which must block neither the main thread nor the async runtime.
#[tauri::command]
pub async fn load_track_file(
    state: tauri::State<'_, Host>,
    feed: tauri::State<'_, AnalysisFeed>,
    tracks: tauri::State<'_, TrackAnalysis>,
    songs: tauri::State<'_, SongLibrary>,
    deck: usize,
    source: TrackSource,
) -> Result<LoadedTrackDto, String> {
    if !valid_deck(deck) {
        return Err("invalid deck".to_string());
    }
    let (bytes, name) = match source {
        TrackSource::Folder { dir, name } => (read_scoped_audio(&dir, &name)?, name),
        TrackSource::Song { name } => (songs.read(&name)?, name),
    };
    let extension = std::path::Path::new(&name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    let analyzed =
        tauri::async_runtime::spawn_blocking(move || analyze_track(bytes, extension.as_deref()))
            .await
            .map_err(|e| format!("track analysis failed: {e}"))??;
    Ok(install_track(&state, &feed, &tracks, deck, analyzed))
}

/// Load an in-memory take onto a deck (the freshly composed WAV that may not
/// be on disk yet, ADR-0030's bytes variant). The payload is a little-endian
/// `u32` deck index followed by the CONTAINER bytes (a WAV, not decoded PCM)
/// — they cross once, and nothing crosses back.
#[tauri::command]
pub async fn load_track_bytes(
    state: tauri::State<'_, Host>,
    feed: tauri::State<'_, AnalysisFeed>,
    tracks: tauri::State<'_, TrackAnalysis>,
    request: tauri::ipc::Request<'_>,
) -> Result<LoadedTrackDto, String> {
    let payload = raw_payload(&request)?;
    let Some(deck) = read_u32_le(payload, 0).map(|d| d as usize) else {
        return Err("load_track_bytes payload too short".to_string());
    };
    if !valid_deck(deck) {
        return Err("invalid deck".to_string());
    }
    let bytes = payload[4..].to_vec();
    let analyzed = tauri::async_runtime::spawn_blocking(move || analyze_track(bytes, None))
        .await
        .map_err(|e| format!("track analysis failed: {e}"))??;
    Ok(install_track(&state, &feed, &tracks, deck, analyzed))
}

/// The loaded track's band profile as binary: `[u32 LE hop count][low f32
/// LE…][mid…][high…]` (hop = 512 frames). Fetched once per load and cached
/// webview-side — the `track_peaks` pattern; an `Err` with no track loaded
/// (`ipc::Response` cannot ride an `Option`).
#[tauri::command]
pub fn track_bands(
    tracks: tauri::State<'_, TrackAnalysis>,
    deck: usize,
) -> Result<tauri::ipc::Response, String> {
    tracks
        .bands_payload(deck)
        .map(tauri::ipc::Response::new)
        .ok_or_else(|| "no track loaded".to_string())
}

#[tauri::command]
pub fn unload_track(
    state: tauri::State<'_, Host>,
    tracks: tauri::State<'_, TrackAnalysis>,
    deck: usize,
) {
    if valid_deck(deck) {
        state.unload_track(deck);
        tracks.clear(deck);
        // The track's echo clock leaves with it; the deck free-runs until the
        // live gate re-acquires (which then owns the period again).
        state.set_beat_period(deck, None);
    }
}

#[tauri::command]
pub fn play_track(state: tauri::State<'_, Host>, deck: usize) {
    if valid_deck(deck) {
        state.play_track(deck);
    }
}

#[tauri::command]
pub fn pause_track(state: tauri::State<'_, Host>, deck: usize) {
    if valid_deck(deck) {
        state.pause_track(deck);
    }
}

#[tauri::command]
pub fn seek_track(state: tauri::State<'_, Host>, deck: usize, frames: f64) {
    if valid_deck(deck) {
        state.seek_track(deck, frames);
    }
}

#[tauri::command]
pub fn set_track_rate(
    state: tauri::State<'_, Host>,
    tracks: tauri::State<'_, TrackAnalysis>,
    deck: usize,
    rate: f64,
) {
    if valid_deck(deck) {
        state.set_track_rate(deck, rate);
        // The synced echo's musical clock follows varispeed (the M14 consumer
        // rule applied to the new tempo authority); untouched with no track.
        if let Some(period) = tracks.beat_period_at_rate(deck, rate) {
            state.set_beat_period(deck, period);
        }
    }
}

#[tauri::command]
pub fn nudge_track_phase(state: tauri::State<'_, Host>, deck: usize, frames: f64) {
    if valid_deck(deck) {
        state.nudge_track_phase(deck, frames);
    }
}

#[tauri::command]
pub fn set_track_loop(state: tauri::State<'_, Host>, deck: usize, start: u64, end: u64) {
    if valid_deck(deck) {
        state.set_track_loop(deck, start, end);
    }
}

#[tauri::command]
pub fn clear_track_loop(state: tauri::State<'_, Host>, deck: usize) {
    if valid_deck(deck) {
        state.clear_track_loop(deck);
    }
}

// --- Freeze / generated loops, one-shots, style sampling ---

#[tauri::command]
pub fn capture_loop(state: tauri::State<'_, Host>, deck: usize, slot: usize, seconds: f64) {
    if valid_deck(deck) && valid_slot(slot) {
        state.capture_loop(deck, slot, seconds);
    }
}

/// Play a loop slot. `layer = false` REPLACES the live stream (a freeze, ADR-0009);
/// `layer = true` SUMS the loop on top, stacking with other layers (a loaded sample,
/// ADR-0022). The shell picks `layer` from the slot's provenance.
#[tauri::command]
pub fn play_loop(state: tauri::State<'_, Host>, deck: usize, slot: usize, layer: bool) {
    if valid_deck(deck) && valid_slot(slot) {
        state.play_loop(deck, slot, layer);
    }
}

#[tauri::command]
pub fn stop_loop(state: tauri::State<'_, Host>, deck: usize) {
    if valid_deck(deck) {
        state.stop_loop(deck);
    }
}

/// Stop one layered loop (a loaded sample stops summing; its buffer stays for a
/// re-press). ADR-0022.
#[tauri::command]
pub fn stop_layer(state: tauri::State<'_, Host>, deck: usize, slot: usize) {
    if valid_deck(deck) && valid_slot(slot) {
        state.stop_layer(deck, slot);
    }
}

#[tauri::command]
pub fn stop_one_shot(state: tauri::State<'_, Host>, deck: usize) {
    if valid_deck(deck) {
        state.stop_one_shot(deck);
    }
}

#[tauri::command]
pub fn clear_loop(state: tauri::State<'_, Host>, deck: usize, slot: usize) {
    if valid_deck(deck) && valid_slot(slot) {
        state.clear_loop(deck, slot);
    }
}

/// Load a decoded loop/pad into a slot. The payload prefixes the interleaved f32
/// PCM with three little-endian `u32`s: deck, slot, and the one-shot flag (0/1).
/// A one-shot plays once; otherwise it loops (seam folded). Binary IPC, like
/// [`load_track`].
#[tauri::command]
pub fn load_generated_loop(
    state: tauri::State<'_, Host>,
    request: tauri::ipc::Request<'_>,
) -> Result<bool, String> {
    let payload = raw_payload(&request)?;
    let (Some(deck), Some(slot), Some(one_shot)) = (
        read_u32_le(payload, 0).map(|d| d as usize),
        read_u32_le(payload, 4).map(|s| s as usize),
        read_u32_le(payload, 8).map(|f| f != 0),
    ) else {
        return Err("load_generated_loop payload too short".to_string());
    };
    // The engine's verdict (accepted / refused) flows back to the webview so a
    // refusal surfaces honestly instead of as a phantom "could not be decoded".
    if valid_deck(deck) && valid_slot(slot) {
        Ok(state.load_generated_loop(deck, slot, pcm_from_le_bytes(&payload[12..]), one_shot))
    } else {
        Ok(false)
    }
}

/// Capture the last `seconds` of played history on a Realtime deck (M15 style
/// sampling), or the track window around the playhead on a Playback deck.
/// Round-trips through the render thread and returns the captured
/// interleaved-stereo samples, or `None` below the floor / for a bad deck index.
#[tauri::command]
pub fn capture_sample(state: tauri::State<'_, Host>, deck: usize, seconds: f64) -> Option<Vec<f32>> {
    if !valid_deck(deck) {
        return None;
    }
    state.capture_sample(deck, seconds)
}

// --- Read-back ---

/// The engine health snapshot (per-deck ring fill + underruns + master peak +
/// gain reduction). The webview polls this for the buffer-health / level meters.
#[tauri::command]
pub fn engine_telemetry(state: tauri::State<'_, Host>) -> HealthDto {
    state.health().into()
}

/// A deck's track transport, or `null` off Playback / for a bad deck index.
#[tauri::command]
pub fn track_status(state: tauri::State<'_, Host>, deck: usize) -> Option<TrackStatusDto> {
    if !valid_deck(deck) {
        return None;
    }
    state.track_status(deck).map(TrackStatusDto::from)
}

/// A deck's loop-slot status (filled / playing per slot), length
/// [`LOOP_SLOT_COUNT`]; an empty vec for a bad deck index.
#[tauri::command]
pub fn loop_slots(state: tauri::State<'_, Host>, deck: usize) -> Vec<LoopSlotDto> {
    if !valid_deck(deck) {
        return Vec::new();
    }
    state
        .loop_slots(deck)
        .iter()
        .map(|&slot| LoopSlotDto::from(slot))
        .collect()
}

/// A loaded track's min/max envelope at `buckets` resolution (the waveform
/// overview), or `null` off Playback / for a bad deck or zero buckets. Fetched
/// once per track load (the webview caches it), so the render-thread round-trip
/// is well off any hot path.
#[tauri::command]
pub fn track_peaks(state: tauri::State<'_, Host>, deck: usize, buckets: usize) -> Option<TrackPeaksDto> {
    if !valid_deck(deck) || buckets == 0 {
        return None;
    }
    state
        .track_peaks(deck, buckets)
        .map(|(min, max)| TrackPeaksDto { min, max })
}

// --- Deck inference control (forwarded to the Python sidecars, part 4) ---

#[tauri::command]
pub fn deck_play(
    state: tauri::State<'_, Sidecars>,
    engine: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    notes: tauri::State<'_, NoteSteering>,
    deck: usize,
) {
    if valid_deck(deck) {
        // The store owns the transport (ADR-0020) AND the ordering (phase D):
        // start_transport flips stopped→playing atomically under the store
        // lock, so a second tap racing the first one's round-trip is a plain
        // no-op — it must not re-arm the worker or reset held note steering
        // (the webview's playPendingRef guard, retired).
        if !store.start_transport(deck) {
            return;
        }
        // Open the engine gate first, so the sidecar's PCM drains the moment it
        // arrives, then tell the worker to start generating.
        engine.set_deck_playing(deck, true);
        state.send(deck, &json!({ "type": "play" }).to_string());
        // A fresh stream starts unsteered (ADR-0023): the worker just reset its
        // conditioning, so the note service drops the matching held state.
        notes.reset(deck);
        // Drum-sit is deck config, not a held gesture (issue #50): re-assert
        // the authored tri-state over the fresh stream. Ordering holds — the
        // play command was written above, and the service's per-deck lane is
        // FIFO behind it.
        notes.reassert_drums(deck);
    }
}

#[tauri::command]
pub fn deck_stop(
    state: tauri::State<'_, Sidecars>,
    engine: tauri::State<'_, Host>,
    store: tauri::State<'_, InterfaceStore>,
    notes: tauri::State<'_, NoteSteering>,
    deck: usize,
) {
    if valid_deck(deck) {
        // Silence the deck immediately (stop draining the held buffer), then tell
        // the worker to stop generating. Without the engine gate the buffered ~1.5 s
        // would keep playing and then underrun (the native stop bug).
        engine.set_deck_playing(deck, false);
        state.send(deck, &json!({ "type": "stop" }).to_string());
        store.set_playing(deck, false);
        notes.reset(deck);
    }
}

#[tauri::command]
pub fn deck_set_prompt(state: tauri::State<'_, Sidecars>, deck: usize, prompt: String) {
    if valid_deck(deck) {
        state.send(deck, &json!({ "type": "set_prompt", "prompt": prompt }).to_string());
    }
}

/// Switch a deck's model (an in-process sidecar restart, reusing the deck ring).
/// Validates the deck index and the model name at the trust boundary; the running
/// sidecar is left untouched if the respawn fails.
///
/// MUST be `async`. The restart `join()`s the outgoing reader thread, which can be
/// mid-`emit`/`Channel::send` to the webview — and a sync command runs on the MAIN
/// (event-loop) thread, the very thread that services those webview sends. On the
/// main thread the join would wait for the reader while the reader waits for the
/// main thread: a hard deadlock (it surfaced switching to the slower-loading
/// `mrt2_base`). `async` runs the command off the main thread, so the freed main
/// thread drains the reader's send, the reader exits, and the join completes.
#[tauri::command]
pub async fn deck_set_model(
    state: tauri::State<'_, Sidecars>,
    deck: usize,
    model: String,
) -> Result<(), String> {
    if !valid_deck(deck) {
        return Err("invalid deck".into());
    }
    if model.is_empty() || model.len() > 64 {
        return Err("invalid model name".into());
    }
    state.restart(deck, &model)
}

/// Embed a captured style sample (M15) into a deck's sidecar. The payload is
/// `[u32 LE deck][u32 LE id length][id utf-8][interleaved f32 LE PCM]` — binary
/// (the PCM is multi-MB), like `load_track`. The native shell routes the embed to
/// the deck's sidecar over the control socket (the generation server has no deck
/// workers).
#[tauri::command]
pub fn deck_embed_sample(
    state: tauri::State<'_, Sidecars>,
    request: tauri::ipc::Request<'_>,
) -> Result<(), String> {
    let payload = raw_payload(&request)?;
    let (Some(deck), Some(id_len)) = (
        read_u32_le(payload, 0).map(|d| d as usize),
        read_u32_le(payload, 4).map(|l| l as usize),
    ) else {
        return Err("deck_embed_sample payload too short".to_string());
    };
    let id_end = 8 + id_len;
    let Some(id) = payload.get(8..id_end).and_then(|b| std::str::from_utf8(b).ok()) else {
        return Err("deck_embed_sample invalid sample id".to_string());
    };
    if valid_deck(deck) {
        state.embed(deck, id, &payload[id_end..]);
    }
    Ok(())
}

/// Set a deck's performance-surface config (issue #48): arm/disarm, key,
/// scale, and note mode. Routed through the note-steering service — the same
/// single sender the hardware and MCP use — which applies the ADR-0023 chunk
/// knob on an armed transition and mirrors the store. A bad deck or key is a
/// silent no-op at this trust boundary.
#[tauri::command]
pub fn set_deck_performance(
    notes: tauri::State<'_, NoteSteering>,
    deck: usize,
    perf: PerformanceSnap,
) {
    if valid_deck(deck) && perf.key < 12 {
        notes.set_performance(deck, perf);
    }
}

/// Play one note from the on-screen keyboard (issue #49): the webview's
/// computer-keyboard surface, scoped to a single deck. Routed through the same
/// single note-steering sender the hardware and MCP use — which snaps the raw
/// pitch to the deck's key/scale, holds it on the surface's own ledger, and
/// mirrors the store. It does NOT arm the deck — routing is independent of the
/// steering switch; the note conditions generation regardless. A bad deck or an
/// out-of-range pitch (the MIDI 0–127 wire width) is a silent no-op at this
/// trust boundary.
#[tauri::command]
pub fn deck_keyboard_note(
    notes: tauri::State<'_, NoteSteering>,
    deck: usize,
    pitch: u8,
    down: bool,
) {
    if valid_deck(deck) && pitch < 128 {
        notes.keyboard_event_deck(deck, pitch, down);
    }
}

/// The standalone MIDI-keyboard window's label (issue #49); shared by the
/// toggle command and the `piano` capability scope.
const PIANO_WINDOW_LABEL: &str = "piano";

/// Mirror the keyboard window's visibility into the store so the drawer's
/// toggle button reflects it (the window is shell-owned; the webview projects).
fn set_piano_window_open(app: &tauri::AppHandle, open: bool) {
    if let Some(store) = app.try_state::<InterfaceStore>() {
        store.set_piano_window_open(open);
    }
}

/// Show / hide / create the standalone MIDI-keyboard window (issue #49): the
/// drawer's toggle button calls this. The first call builds the window
/// (`index.html?window=piano`, which `main.tsx` renders as the piano); later
/// calls flip its visibility. A close is intercepted to HIDE the window, so the
/// toggle re-shows the same one with its routing/octave state intact.
#[tauri::command]
pub fn toggle_piano_window(app: tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(PIANO_WINDOW_LABEL) {
        let showing = win.is_visible().unwrap_or(false);
        if showing {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
        set_piano_window_open(&app, !showing);
        return;
    }
    let built = tauri::WebviewWindowBuilder::new(
        &app,
        PIANO_WINDOW_LABEL,
        tauri::WebviewUrl::App("index.html?window=piano".into()),
    )
    .title("MIDI Keyboard")
    .inner_size(560.0, 260.0)
    .min_inner_size(440.0, 220.0)
    .build();
    if let Ok(win) = built {
        set_piano_window_open(&app, true);
        let app_for_close = app.clone();
        let win_for_close = win.clone();
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = win_for_close.hide();
                set_piano_window_open(&app_for_close, false);
            }
        });
    }
}

/// Drum conditioning as it crosses the IPC boundary (issue #50): 'suppress'
/// keeps drums out (sit beside another deck), 'auto' hands the choice back to
/// the model. Binary, matching the `magenta-realtime` reference's `drumless`
/// toggle (suppress vs masked) — the model also accepts a force flag but the
/// reference never exposes it, so LSDJ doesn't either. A serde enum so an
/// unknown mode is a clean deserialization `Err`; `JsonSchema` too, so the MCP
/// tool surface (`crate::mcp`) reuses it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DrumModeArg {
    Suppress,
    Auto,
}

impl From<DrumModeArg> for Option<bool> {
    fn from(mode: DrumModeArg) -> Self {
        match mode {
            DrumModeArg::Suppress => Some(false),
            DrumModeArg::Auto => None,
        }
    }
}

/// Set a deck's drum conditioning (issue #50): the tri-state "drum-sit"
/// mixing control. Routed through the note-steering service — the same
/// single sender the MCP tool uses (ADR-0031) — which records the authored
/// state (re-asserted on the play edge), sends the wire flag, and mirrors
/// the store. A bad deck is a silent no-op at this trust boundary.
#[tauri::command]
pub fn set_deck_drums(notes: tauri::State<'_, NoteSteering>, deck: usize, mode: DrumModeArg) {
    if valid_deck(deck) {
        notes.set_drums(deck, mode.into());
    }
}

/// Set a deck's drum-conditioning strength (issue #50): the `cfg_drums`
/// guidance scale behind the drum-sit control. Clamped to the product/reference
/// range [0.0, 5.0] at this trust boundary — the UI slider already caps there,
/// and `cfg_drums` above ~5 drifts out of distribution (docs/spike-mrt2.md), so
/// no caller (a future MCP strength setter included) can reach the OOD zone.
/// The engine's own valid bound is wider ([-1, 7]) but 5 is the exposed max.
/// Routed through the note-steering service like the mode. A bad deck is a
/// silent no-op.
#[tauri::command]
pub fn set_deck_drums_strength(
    notes: tauri::State<'_, NoteSteering>,
    deck: usize,
    strength: f32,
) {
    if valid_deck(deck) {
        notes.set_drums_strength(deck, strength.clamp(0.0, 5.0));
    }
}

/// Edit a deck's live generation operating point (issue #84): a PARTIAL patch —
/// only the changed field(s) cross the boundary, so the note-steering service
/// merges them onto its authoritative value under one lock and clamps the result
/// (`GenerationSnap::clamped`, the trust-boundary guard). Sending only the delta
/// means a rapid second edit can't rebuild from a stale webview snapshot and
/// revert the first. Routed through the same single deck control-frame sender the
/// drum controls use — which sends the merged frame to the worker, mirrors the
/// store, and re-sends on a fresh worker's `ready`. A bad deck is a silent no-op.
#[tauri::command]
pub fn set_deck_generation(
    notes: tauri::State<'_, NoteSteering>,
    deck: usize,
    patch: GenerationPatch,
) {
    if valid_deck(deck) {
        notes.patch_generation(deck, patch);
    }
}

/// Reset one generation param to the reference baseline (issue #84): the reset
/// target is the shell's own `DEFAULT_*` (`GenerationField::reset_patch`), so the
/// frontend's per-knob reset only names the field and never holds a copy of the
/// default. Same merge-under-lock path as an edit. A bad deck is a silent no-op.
#[tauri::command]
pub fn reset_deck_generation(
    notes: tauri::State<'_, NoteSteering>,
    deck: usize,
    field: GenerationField,
) {
    if valid_deck(deck) {
        notes.reset_generation_field(deck, field);
    }
}

// --- Analysis PCM tap (gap 1: re-feed the TS beat/loudness/band analysis) ---

/// Subscribe a deck's realtime model PCM to a webview [`Channel`]. The sidecar
/// reader tees each frame as raw interleaved-stereo f32 LE bytes; the webview
/// reinterprets them as the `Float32Array` its analysis consumes (ADR-0017:
/// analysis stays in TypeScript). One subscriber per deck — a re-subscribe
/// replaces the prior channel.
#[tauri::command]
pub fn subscribe_deck_pcm(
    taps: tauri::State<'_, PcmTaps>,
    deck: usize,
    channel: Channel<InvokeResponseBody>,
) {
    if valid_deck(deck) {
        taps.set(deck, Some(channel));
    }
}

#[tauri::command]
pub fn unsubscribe_deck_pcm(taps: tauri::State<'_, PcmTaps>, deck: usize) {
    if valid_deck(deck) {
        taps.set(deck, None);
    }
}

/// Reset a deck's live beat analysis (ADR-0025: estimates never span streams).
/// The webview invokes this on the transport discontinuities it drives (play /
/// prime / stop / mode switch); sidecar-origin discontinuities (model switch,
/// worker death) reset shell-side without it. The engine-frame origin is
/// captured here and rides the reset message, so tracker, gates, and origin
/// re-anchor atomically in stream order.
#[tauri::command]
pub fn analysis_reset(
    state: tauri::State<'_, Host>,
    feed: tauri::State<'_, AnalysisFeed>,
    deck: usize,
) {
    if valid_deck(deck) {
        feed.reset(deck, state.health().context_frames as f64);
    }
}

/// The consolidated per-frame read-back: health + every deck's transport + every
/// deck's loop slots in one round-trip. The webview polls this each animation
/// frame and serves the synchronous getters from the cached result.
#[tauri::command]
pub fn engine_snapshot(state: tauri::State<'_, Host>) -> EngineSnapshotDto {
    let tracks = (0..DECK_COUNT)
        .map(|deck| state.track_status(deck).map(TrackStatusDto::from))
        .collect();
    let loops = (0..DECK_COUNT)
        .map(|deck| {
            state
                .loop_slots(deck)
                .iter()
                .map(|&slot| LoopSlotDto::from(slot))
                .collect()
        })
        .collect();
    EngineSnapshotDto {
        health: state.health().into(),
        tracks,
        loops,
    }
}

/// The full interface-state snapshot the webview hydrates its projection from on
/// mount (ADR-0020). Thereafter the projection follows the `store://changed` event;
/// this is the one-shot initial read. Unlike `engine_snapshot` (the per-frame audio
/// read-back), this is the *semantic / audio-param* state the store owns.
#[tauri::command]
pub fn store_snapshot(store: tauri::State<'_, InterfaceStore>) -> InterfaceState {
    store.snapshot()
}

/// Mirror a realtime deck's model read-back into the store. It is derived in the
/// webview from worker status (`ready`/`model_loading`), so — unlike the mixer
/// setters — the webview WRITES the current value up (no engine effect); the store
/// holds it for an MCP read (ADR-0020). `playing` is deliberately NOT mirrored:
/// the store owns the transport, written only by `deck_play`/`deck_stop` (any
/// controller) and the sidecar status relay.
#[tauri::command]
pub fn set_deck_model(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    model: Option<String>,
) {
    if valid_deck(deck) {
        store.set_deck_model(deck, model);
    }
}

/// Set or clear one hot-cue pad (ADR-0020 phase D): the store owns the
/// points — the UI computes the snapped position and emits this intent, the
/// MCP cue tools write the same mutation, and the pads project the snapshot.
/// A no-track deck or an out-of-range pad is a silent store no-op.
#[tauri::command]
pub fn set_deck_cue_point(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    index: usize,
    seconds: Option<f64>,
) {
    if valid_deck(deck) && seconds.is_none_or(f64::is_finite) {
        store.set_deck_cue(deck, index, seconds);
    }
}

/// Record which source a deck plays (M19, ADR-0020 phase D): the webview's
/// load flow writes it (the load orchestration stays webview-side until the
/// transport inverts), so an agent sees playback vs realtime natively.
#[tauri::command]
pub fn set_deck_mode(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    mode: crate::store::PlayModeSnap,
) {
    if valid_deck(deck) {
        store.set_deck_mode(deck, mode);
    }
}

/// Mirror a playback deck's loaded-track identity into the store (`None` clears it
/// on unload / a realtime deck). A read-back the webview writes up; no engine effect.
#[tauri::command]
pub fn set_deck_track(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    track: Option<crate::store::TrackIdentitySnap>,
) {
    if valid_deck(deck) {
        store.set_deck_track(deck, track);
    }
}

/// Mirror a playback deck's live transport (playhead / rate / loop) into the store.
/// A read-back the webview writes up at a throttled cadence (the playhead moves every
/// audio frame); `None` clears it on unload / a realtime deck. No engine effect.
#[tauri::command]
pub fn set_deck_transport(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    transport: Option<crate::store::TransportSnap>,
) {
    if valid_deck(deck) {
        store.set_deck_transport(deck, transport);
    }
}

/// Mirror a deck's freeze/sample loop-slot labels into the store. A read-back the
/// webview writes up when its slots change; no engine effect.
#[tauri::command]
pub fn set_deck_loop_labels(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    labels: Vec<Option<String>>,
) {
    if valid_deck(deck) {
        store.set_deck_loop_labels(deck, labels);
    }
}

// --- Style-pad intents (ADR-0020 phase B: the store owns the arrangement;
// the webview projects and emits these; the shell style sender drives the
// worker). Validation lives in the store mutations — one copy of the rules
// for UI, hardware, and MCP; a rejected intent is a silent no-op here (the
// UI disables invalid gestures; MCP tools report through their own path).

#[tauri::command]
pub fn style_add_target(store: tauri::State<'_, InterfaceStore>, deck: usize, text: String) {
    if valid_deck(deck) {
        store.style_add_target(deck, &text);
    }
}

#[tauri::command]
pub fn style_add_sample_target(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    label: String,
    sample: String,
) {
    if valid_deck(deck) {
        store.style_add_sample_target(deck, &label, &sample);
    }
}

#[tauri::command]
pub fn style_move_target(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    text: String,
    x: f32,
    y: f32,
) {
    if valid_deck(deck) && x.is_finite() && y.is_finite() {
        store.style_move_target(deck, &text, x, y);
    }
}

#[tauri::command]
pub fn style_remove_target(store: tauri::State<'_, InterfaceStore>, deck: usize, text: String) {
    if valid_deck(deck) {
        store.style_remove_target(deck, &text);
    }
}

#[tauri::command]
pub fn style_rename_target(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    from: String,
    to: String,
) {
    if valid_deck(deck) {
        store.style_rename_target(deck, &from, &to);
    }
}

#[tauri::command]
pub fn style_toggle_selection(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    text: String,
) {
    if valid_deck(deck) {
        store.style_toggle_selection(deck, &text);
    }
}

#[tauri::command]
pub fn style_fan_out(store: tauri::State<'_, InterfaceStore>, deck: usize) {
    if valid_deck(deck) {
        store.style_fan_out(deck);
    }
}

#[tauri::command]
pub fn style_set_cursor(store: tauri::State<'_, InterfaceStore>, deck: usize, x: f32, y: f32) {
    if valid_deck(deck) && x.is_finite() && y.is_finite() {
        store.set_deck_cursor(deck, crate::store::PadPointSnap { x, y });
    }
}

/// Load a preset's arrangement (text targets + cursor) wholesale — the crate
/// browser's and hardware's load path; entries were validated when the
/// preset was parsed, and sampled chips never enter a preset.
#[tauri::command]
pub fn style_apply_preset(
    store: tauri::State<'_, InterfaceStore>,
    deck: usize,
    targets: Vec<crate::store::StyleTargetSnap>,
    cursor: crate::store::PadPointSnap,
) {
    if valid_deck(deck) {
        store.style_apply_preset(deck, crate::style::sanitize_preset_targets(targets), cursor);
    }
}

/// Mirror the primed-off-air read-back into the store (the transport-CUE LED
/// state) — the deck's prime/play flow writes it up on change.
#[tauri::command]
pub fn set_deck_primed(store: tauri::State<'_, InterfaceStore>, deck: usize, primed: bool) {
    if valid_deck(deck) {
        store.set_deck_primed(deck, primed);
    }
}
