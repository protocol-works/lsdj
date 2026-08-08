//! LSDJ native shell — the Tauri v2 app host (Phase 2).
//!
//! This embeds the React frontend, starts the Rust audio engine and the native
//! MIDI service (ADR-0031), and exposes the engine control surface to the
//! webview over IPC.
//!
//! # The audio host lifecycle (the load-bearing bit)
//!
//! On `setup` we build a [`Host`] ([`lsdj_engine::host`]). `Host::new` builds
//! the [`Engine`], creates its two decks, and KEEPS the engine on a dedicated
//! **render thread** — control commands and the RT render both need `&mut Engine`,
//! and some control ops allocate (rebuilding `fundsp` nodes, taking a decoded
//! buffer), so they must NOT run in the cpal callback. The render thread owns the
//! engine, drains a wait-free command channel, and renders into an output ring;
//! the cpal callback only drains that ring (ADR-style decoupling — see the `host`
//! module docs and its latency note).
//!
//! `Host::new` also returns the two [`DeckHandle`]s — the non-RT producer side of
//! each deck's input ring. They are the sidecar PCM feed's writers; a later step
//! moves them onto the sidecar transport thread. Until then they are held in
//! managed state so they stay alive (dropping a producer would close its ring).
//!
//! We then start the cpal device via [`engine_device::open_main_stream`], which
//! drains the host's output ring in its callback. In a sandbox / headless CI
//! there is often no exact-48000/f32 device; that path returns
//! [`DeviceError::Unavailable`] and we continue with no stream — the host's render
//! thread keeps filling the ring (nothing drains it, which is fine), so control
//! and read-back still work and the window still opens.
//!
//! The [`Host`] is held in Tauri **managed state** so every `#[tauri::command]`
//! can drive it; managed state lives for the app's lifetime, so the render thread
//! (and the device stream) run until shutdown.

use std::sync::Mutex;

use lsdj_engine::device::{self as engine_device, AudioStream, DeviceError};
use lsdj_engine::host::Host;
use lsdj_engine::DeckHandle;
use tauri::Manager;

mod analysis;
mod child_process;
mod commands;
mod decode;
mod generation;
mod library;
mod local_auth;
mod loras;
#[cfg(feature = "managed-runtime")]
mod magenta_gateway;
#[cfg_attr(not(feature = "managed-runtime"), allow(dead_code))]
mod managed_runtime;
mod mcp;
mod midi;
mod models;
mod platform_diagnostics;
mod platform_paths;
mod runtime_installer;
mod samples;
mod settings;
mod sidecar;
mod songs;
mod store;
mod style;
mod style_send;
mod watcher;

#[cfg(all(feature = "bundled-backend", feature = "managed-runtime"))]
compile_error!("bundled-backend and managed-runtime are mutually exclusive");

/// The default per-deck model the sidecars load (mirrors `controller.py`
/// `DEFAULT_MODEL`).
const DEFAULT_MODEL: &str = "mrt2_small";

/// The release-only PyInstaller ONEDIR executable under Tauri's resource dir.
/// Kept as a pure join so the bundle contract has a cheap unit test.
#[cfg(any(feature = "bundled-backend", test))]
fn bundled_backend_path(resource_dir: &std::path::Path) -> std::path::PathBuf {
    resource_dir.join("lsdj_backend").join("lsdj_backend")
}

/// Stable launcher seam for app-managed platform runtimes.  The launcher is an
/// ordinary native executable promoted atomically by the model/runtime manager;
/// it may host PyTorch MRT2, TFLite SA3, or both without packaging knowing the
/// Python environment's internal layout.
#[cfg(any(feature = "managed-runtime", test))]
fn managed_backend_path(assets_dir: &std::path::Path) -> std::path::PathBuf {
    assets_dir
        .join("backend")
        .join("current")
        .join(if cfg!(windows) {
            "lsdj_backend.exe"
        } else {
            "lsdj_backend"
        })
}

/// Point every Python-backed service at the signed runtime inside the app.
/// Developer builds deliberately omit the feature/resource and retain their
/// source-tree `uv run` defaults. A release build fails during setup rather than
/// opening a deceptively healthy but silent shell when its payload is absent.
#[cfg(feature = "bundled-backend")]
fn configure_bundled_backend(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let backend = bundled_backend_path(&app.path().resource_dir()?);
    if !backend.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("bundled backend is missing: {}", backend.display()),
        )
        .into());
    }
    std::env::set_var("LSDJ_BACKEND_BIN", &backend);
    Ok(())
}

#[cfg(all(not(feature = "bundled-backend"), feature = "managed-runtime"))]
fn configure_bundled_backend(_app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let backend = managed_backend_path(platform_paths::get().assets());
    // A packaged build must never inherit a developer override or fall through
    // to a system `uv`/Python.  The marker makes the command builders fail with
    // an actionable first-run error while #110/#111 install the verified runtime.
    std::env::remove_var("LSDJ_BACKEND_BIN");
    std::env::set_var("LSDJ_MANAGED_BACKEND_REQUIRED", "1");
    if backend.is_file() {
        std::env::set_var("LSDJ_BACKEND_BIN", backend);
    }
    Ok(())
}

#[cfg(not(any(feature = "bundled-backend", feature = "managed-runtime")))]
fn configure_bundled_backend(_app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}

/// Tauri-managed audio state held ALONGSIDE the [`Host`]: the running output
/// streams (kept alive so their Drop does not stop audio), the current device
/// choices, and whether the main device actually started.
///
/// The `Host` is managed separately so the commands can take it as
/// `tauri::State<'_, Host>` directly. This struct holds the things the commands
/// do not need but the app must keep alive.
///
/// Two streams, dual-mode (ADR-0021): the MAIN stream always drains the master
/// ring onto channels 1/2; the CUE stream depends on the chosen cue device:
/// - **combined** (cue = "same as main", an empty `cue_name` or one equal to
///   `main_name`): the cue rides the main device's channels 3/4 (the FLX4 phones
///   jack) and `cue_stream` is `None`.
/// - **split** (a different `cue_name`): a separate `cue_stream` drains the cue
///   ring onto its own device's 1/2, so the cue reaches any second output.
struct AudioState {
    /// The MAIN output stream — master → ch 1/2, and cue → ch 3/4 in combined
    /// mode. Kept alive; replaced when the main device (or the combined/split
    /// mode) changes. `None` in the sandbox/headless case.
    main_stream: Mutex<Option<AudioStream>>,
    /// The CUE output stream in SPLIT mode (a separate device); `None` in combined
    /// mode (the cue rides the main stream's 3/4).
    cue_stream: Mutex<Option<AudioStream>>,
    /// Whether the main device came up at startup (the `app_info` flag).
    device_started: bool,
    /// The current main device name (empty = system default), so a cue-only switch
    /// can recompute the combined/split topology.
    main_name: Mutex<String>,
    /// The current cue device name (empty = "same as main" → combined).
    cue_name: Mutex<String>,
}

/// Deck producer handles NOT owned by a sidecar (sidecars disabled, or a spawn
/// failed) — held in managed state only to keep their input rings open (dropping
/// a producer closes its ring). Empty when every deck has a live sidecar.
struct IdleHandles(#[allow(dead_code)] Mutex<Vec<DeckHandle>>);

/// A sidecar status line for the webview (the `('status', dict)` worker output,
/// or a synthetic `worker_died`). Emitted on the `sidecar://status` event.
#[derive(Clone, serde::Serialize)]
struct SidecarStatus {
    deck: usize,
    /// The raw status JSON from the worker; the webview parses it.
    json: String,
}

/// Build the host (engine + render thread + decks), start the cpal device that
/// drains the host's output ring, and return the [`Host`], the [`AudioState`]
/// holding the stream, and the two deck producer handles (for the sidecar feed).
/// The device-start path is graceful: a missing device leaves the host running
/// headlessly with `device_started = false`.
fn start_audio() -> (Host, AudioState, [DeckHandle; lsdj_engine::DECK_COUNT]) {
    let (host, master, cue, deck_handles) = Host::new();

    // Start combined on the default device: master → 1/2 and cue → 3/4 if the
    // device is ≥4-channel (the FLX4), exactly as before. A separate cue device is
    // opted into later via `set_cue_device`. These are the ORIGINAL ring consumers
    // matching the render thread's producers, so no ring install is needed yet.
    let (main_stream, device_started) =
        match engine_device::open_main_stream(None, master, Some(cue)) {
            Ok(stream) => {
                let info = stream.info();
                // Non-RT setup logging only; the RT callback itself logs nothing.
                println!(
                    "lsdj-app: audio device started — device='{}' channels={} rate={} buffer={:?}",
                    info.device_name, info.device_channels, info.sample_rate, info.buffer_frames
                );
                (Some(stream), true)
            }
            Err(DeviceError::Unavailable(msg)) => {
                // Expected in a sandbox / headless CI: no exact-48000/f32 device.
                // Log and continue with no stream — the host renders into the ring,
                // the window opens, control/read-back work.
                eprintln!("lsdj-app: audio device unavailable ({msg}) — continuing without audio");
                (None, false)
            }
            Err(DeviceError::Stream(msg)) => {
                eprintln!("lsdj-app: audio stream error ({msg}) — continuing without audio");
                (None, false)
            }
        };

    let state = AudioState {
        main_stream: Mutex::new(main_stream),
        cue_stream: Mutex::new(None),
        device_started,
        main_name: Mutex::new(String::new()),
        cue_name: Mutex::new(String::new()),
    };
    (host, state, deck_handles)
}

fn sidecar_status_sink(
    app: tauri::AppHandle,
    idx: usize,
    status_feed: analysis::live::AnalysisFeed,
) -> Box<dyn FnMut(String) + Send> {
    Box::new(move |json| {
        use tauri::{Emitter, Manager};
        if let Some(event) = sidecar::status_event(&json) {
            if let Some(store) = app.try_state::<store::InterfaceStore>() {
                match event.as_str() {
                    "worker_died" | "startup_failed" => store.set_worker_health(idx, true, false),
                    "model_loading" | "warming" => store.set_worker_health(idx, false, true),
                    "ready" => store.set_worker_health(idx, false, false),
                    _ => {}
                }
            }
            if event == "ready" {
                if let Some(sender) = app.try_state::<style_send::StyleSender>() {
                    sender.resend(idx);
                }
                if let Some(notes) = app.try_state::<midi::notes::NoteSteering>() {
                    notes.reassert_generation(idx);
                }
            }
        }
        if sidecar::transport_ended(&json) {
            if let Some(store) = app.try_state::<store::InterfaceStore>() {
                store.set_playing(idx, false);
            }
            let origin = app
                .try_state::<Host>()
                .map_or(0.0, |host| host.health().context_frames as f64);
            status_feed.reset(idx, origin);
            if let Some(notes) = app.try_state::<midi::notes::NoteSteering>() {
                notes.reset(idx);
            }
        }
        let _ = app.emit("sidecar://status", SidecarStatus { deck: idx, json });
    })
}

/// Spawn two MLX processes on macOS, or the #109 one-process/two-state PyTorch
/// worker on Linux and Windows. A failed launch leaves the corresponding rings
/// silent without failing the rest of the app.
fn start_sidecars(
    app: &tauri::AppHandle,
    handles: [DeckHandle; lsdj_engine::DECK_COUNT],
    taps: &sidecar::PcmTaps,
    feed: &analysis::live::AnalysisFeed,
) -> (sidecar::Sidecars, Vec<DeckHandle>) {
    const DECK_IDS: [&str; lsdj_engine::DECK_COUNT] = ["a", "b"];
    if matches!(
        sidecar::mrt2_runtime_for_platform().as_deref(),
        Ok("pytorch-cuda")
    ) {
        let models = std::array::from_fn(|_| DEFAULT_MODEL.to_string());
        let sinks = [
            sidecar_status_sink(app.clone(), 0, feed.clone()),
            sidecar_status_sink(app.clone(), 1, feed.clone()),
        ];
        let mut shared = sidecar::SharedSidecar::parked(
            models,
            handles,
            sinks,
            taps.clone(),
            feed.clone(),
        );
        if let Err(error) = shared.activate() {
            // A fresh managed install has no runtime yet. Keep both permanent
            // DeckHandles parked inside the supervisor; the first MRT2 install
            // activates this same topology without restarting the app.
            eprintln!("lsdj-app: shared MRT2 sidecar unavailable: {error}");
        }
        return (sidecar::Sidecars::new_shared(shared), Vec::new());
    }

    let mut decks = Vec::new();
    for (idx, handle) in handles.into_iter().enumerate() {
        let deck_id = DECK_IDS[idx];
        match sidecar::Sidecar::spawn(
            deck_id,
            idx,
            DEFAULT_MODEL,
            handle,
            sidecar_status_sink(app.clone(), idx, feed.clone()),
            taps.clone(),
            feed.clone(),
        ) {
            Ok(sidecar) => decks.push(Some(sidecar)),
            Err(error) => {
                eprintln!("lsdj-app: deck {deck_id} sidecar spawn failed: {error}");
                decks.push(None);
            }
        }
    }
    (sidecar::Sidecars::new(decks), Vec::new())
}

/// Report the app version and whether the cpal device came up. Lets the frontend
/// (and the integration harness) confirm the shell loaded and the device-start
/// path ran. The full engine surface lives in [`commands`].
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    audio_device_started: bool,
    /// The loopback port the generation server bound (`None` if disabled / not
    /// running). The webview builds the `/api/*` base URL from it (gap 2).
    generation_port: Option<u16>,
    /// Per-launch bearer capability for the generation service. It exists only in
    /// Rust state/the webview process and is never written to settings or logs.
    generation_capability: Option<String>,
    /// Managed Linux/Windows route Magenta through a separate Rust-owned
    /// gateway. On bundled macOS these mirror the combined generation service,
    /// preserving its established behavior.
    magenta_port: Option<u16>,
    magenta_capability: Option<String>,
    /// The loopback port the MCP server bound (`None` only if the loopback bind
    /// failed — the server is otherwise always on), and the bearer token a client must
    /// present (ADR-0020 Phase 2). Surfaced so the client config can point at
    /// `http://127.0.0.1:<mcpPort>/mcp` with `Authorization: Bearer <mcpToken>`.
    mcp_port: Option<u16>,
    mcp_token: Option<String>,
}

#[tauri::command]
fn app_info(
    app: tauri::AppHandle,
    state: tauri::State<'_, AudioState>,
    generation: tauri::State<'_, generation::GenerationServer>,
    mcp: tauri::State<'_, mcp::McpServer>,
) -> AppInfo {
    let generation_connection = generation.connection();
    #[cfg(feature = "managed-runtime")]
    let (magenta_port, magenta_capability) = {
        let gateway = app.state::<magenta_gateway::MagentaGateway>();
        (gateway.port(), gateway.capability())
    };
    #[cfg(not(feature = "managed-runtime"))]
    let (magenta_port, magenta_capability) = generation_connection
        .clone()
        .map_or((None, None), |(port, capability)| (Some(port), Some(capability)));
    #[cfg(not(feature = "managed-runtime"))]
    let _ = app;
    AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        audio_device_started: state.device_started,
        generation_port: generation_connection.as_ref().map(|(port, _)| *port),
        generation_capability: generation_connection.map(|(_, capability)| capability),
        magenta_port,
        magenta_capability,
        mcp_port: mcp.port(),
        mcp_token: mcp.token(),
    }
}

/// Mint a new MCP bearer token, persist it, and swap it in live (the Settings
/// "Rotate token" button). Returns the new token; errors if the server isn't running.
#[tauri::command]
fn rotate_mcp_token(mcp: tauri::State<'_, mcp::McpServer>) -> Result<String, String> {
    mcp.rotate()
        .ok_or_else(|| "the MCP server is not running".to_string())
}

/// Rebind the MCP server to a user-chosen port, persist it, and restart the serving
/// task (the Settings port field). Returns the new port; errors (e.g. the port is
/// taken) leave the running server untouched.
#[tauri::command]
fn set_mcp_port(port: u16, mcp: tauri::State<'_, mcp::McpServer>) -> Result<u16, String> {
    mcp.set_port(port)
}

/// One selectable output device for the picker (serde camelCase → `cueCapable`).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputDeviceDto {
    name: String,
    channels: u16,
    cue_capable: bool,
}

/// Enumerate the output devices the engine can open, with their channel count and
/// whether they can carry the headphone cue (≥4 channels → master 1/2, cue 3/4).
#[tauri::command]
fn list_output_devices() -> Vec<OutputDeviceDto> {
    engine_device::list_output_devices()
        .into_iter()
        .map(|d| OutputDeviceDto {
            name: d.name,
            channels: d.channels,
            cue_capable: d.cue_capable,
        })
        .collect()
}

/// The webview message for a switch that could not land because the engine's
/// command queue was momentarily full — the caller keeps the old stream and the
/// UI invites a retry.
const ENGINE_BUSY: &str = "the audio engine was momentarily busy — try switching again";

/// True when the cue device choice means "ride the main device" (combined mode):
/// an empty cue name ("same as main") or one equal to the main device.
fn is_combined(main_name: &str, cue_name: &str) -> bool {
    cue_name.is_empty() || cue_name == main_name
}

/// Convert a stored device name into the `open_*` selector (empty → the system
/// default `None`).
fn selector(name: &str) -> Option<&str> {
    (!name.is_empty()).then_some(name)
}

/// (Re)open the MAIN stream for the given device choices. In combined mode the cue
/// rides the main device's channels 3/4 (and any split cue stream is dropped); in
/// split mode the main stream is master-only and the existing cue stream is left
/// running. Opens the new device FIRST and only swaps the render thread's ring(s)
/// on success, so a failed open leaves current audio untouched. Briefly gaps the
/// master (you are changing the master device, or moving the cue onto it).
fn reopen_main(
    host: &Host,
    audio: &AudioState,
    main_name: &str,
    cue_name: &str,
) -> Result<(), String> {
    let combined = is_combined(main_name, cue_name);
    let (master_ring, master_consumer) = host.new_output_ring();
    // In combined mode build a fresh cue ring too, so the cue rides the main
    // device's 3/4; `open_main_stream` drops it if the device is < 4 channels.
    let (cue_ring, cue_consumer) = if combined {
        let (ring, consumer) = host.new_output_ring();
        (Some(ring), Some(consumer))
    } else {
        (None, None)
    };
    let stream =
        engine_device::open_main_stream(selector(main_name), master_consumer, cue_consumer)
            .map_err(|e| e.to_string())?;
    if !host.install_master_ring(master_ring) {
        return Err(ENGINE_BUSY.into());
    }
    if let Some(cue_ring) = cue_ring {
        // Combined: the cue now rides the main stream's 3/4 — install its ring
        // (best-effort; the cue is secondary) and drop any split cue stream.
        host.install_cue_ring(cue_ring);
        *audio.cue_stream.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
    let info = stream.info();
    println!(
        "lsdj-app: main output switched — device='{}' channels={} combined_cue={combined}",
        info.device_name, info.device_channels
    );
    *audio.main_stream.lock().unwrap_or_else(|p| p.into_inner()) = Some(stream);
    Ok(())
}

/// (Re)open the SPLIT cue stream on its own device, leaving the main stream (and
/// the master ring) completely untouched — the property that lets a cue-device
/// switch never interrupt the audience's master.
fn reopen_cue_split(host: &Host, audio: &AudioState, cue_name: &str) -> Result<(), String> {
    let (cue_ring, cue_consumer) = host.new_output_ring();
    let stream = engine_device::open_cue_stream(selector(cue_name), cue_consumer)
        .map_err(|e| e.to_string())?;
    if !host.install_cue_ring(cue_ring) {
        return Err(ENGINE_BUSY.into());
    }
    let info = stream.info();
    println!(
        "lsdj-app: cue output switched — device='{}' channels={}",
        info.device_name, info.device_channels
    );
    *audio.cue_stream.lock().unwrap_or_else(|p| p.into_inner()) = Some(stream);
    Ok(())
}

/// Switch the MAIN output device by name (EMPTY = the system default). Rebuilds
/// the main stream (carrying the cue on 3/4 if the current cue choice is
/// combined); a split cue stream keeps playing on its own device, undisturbed.
#[tauri::command]
fn set_main_device(
    host: tauri::State<'_, Host>,
    audio: tauri::State<'_, AudioState>,
    store: tauri::State<'_, store::InterfaceStore>,
    app: tauri::AppHandle,
    name: String,
) -> Result<(), String> {
    let cue_name = audio
        .cue_name
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    reopen_main(&host, &audio, &name, &cue_name)?;
    *audio.main_name.lock().unwrap_or_else(|p| p.into_inner()) = name.clone();
    // Persistence follows ownership (ADR-0020 phase A): a successful switch
    // records into the store (the picker projects it) and the settings file
    // (boot hydration re-applies it) — localStorage is out of the loop.
    settings::update(&app, |s| s.main_device = name.clone());
    store.set_output_devices(name, cue_name);
    Ok(())
}

/// Switch the CUE output device by name (EMPTY = "same as main" → combined, the
/// FLX4 phones jack on channels 3/4). A split→split change touches ONLY the cue
/// stream (the master is never interrupted); transitions into or out of combined
/// also reopen the main stream (a brief master gap, the rarer case).
#[tauri::command]
fn set_cue_device(
    host: tauri::State<'_, Host>,
    audio: tauri::State<'_, AudioState>,
    store: tauri::State<'_, store::InterfaceStore>,
    app: tauri::AppHandle,
    name: String,
) -> Result<(), String> {
    let main_name = audio
        .main_name
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let was_combined = {
        let cue_name = audio.cue_name.lock().unwrap_or_else(|p| p.into_inner());
        // Re-selecting the already-active cue device would tear down and rebuild
        // the cue stream for no change (a needless cue glitch) — short-circuit.
        if name == *cue_name {
            return Ok(());
        }
        is_combined(&main_name, &cue_name)
    };
    if is_combined(&main_name, &name) {
        // Cue rides the main device's 3/4: reopen main with cue duty (it also drops
        // any split cue stream).
        reopen_main(&host, &audio, &main_name, &name)?;
    } else {
        // Split: open/replace the cue stream on its own device — the user's actual
        // intent, fatal if it fails. Once it returns the cue is live on the new
        // device, so the switch has succeeded.
        reopen_cue_split(&host, &audio, &name)?;
        if was_combined {
            // Leaving combined: reopen main master-only so the cue isn't left
            // duplicated on the old main device's 3/4. Best-effort — the cue
            // switch already succeeded and the master is untouched on failure
            // (reopen_main swaps only on success), so we log rather than fail and
            // leave `cue_name` consistent with the live cue. Any stray duplicate
            // clears on the next main switch.
            if let Err(e) = reopen_main(&host, &audio, &main_name, &name) {
                eprintln!(
                    "lsdj-app: cue moved to '{name}', but clearing it from the old \
                     main device's 3/4 failed: {e}"
                );
            }
        }
    }
    *audio.cue_name.lock().unwrap_or_else(|p| p.into_inner()) = name.clone();
    let main_name = audio
        .main_name
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    settings::update(&app, |s| s.cue_device = name.clone());
    store.set_output_devices(main_name, name);
    Ok(())
}

/// Set (and persist) the recordings folder — "" = Downloads. The picker's
/// native dialog supplies real paths; the recorder recreates or falls back
/// at start, so no validation beyond ownership is needed here.
#[tauri::command]
fn set_recordings_folder(
    store: tauri::State<'_, store::InterfaceStore>,
    app: tauri::AppHandle,
    folder: String,
) {
    settings::update(&app, |s| s.recordings_folder = folder.clone());
    store.set_recordings_folder(folder);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Native file/folder picker for the media browser's folder tab (WKWebView
        // has no File System Access API).
        .plugin(tauri_plugin_dialog::init())
        // Reveal the generated-songs folder in Finder (open_songs_folder); the
        // webview can't download, so songs are written to disk and opened natively.
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Resolve every filesystem root once and pass the contract to Python
            // through inherited environment variables. This also performs the
            // restart-safe macOS model migration. MUST precede every service.
            platform_paths::configure(app)?;
            // Release backends are resolved only after the host-owned asset root
            // exists. macOS points at a bundled executable; Windows/Linux point
            // at the atomically promoted managed-runtime launcher.
            configure_bundled_backend(app)?;
            // Start the audio host (engine + render thread + device), then spawn
            // the per-deck inference sidecars fed by the deck handles. Everything
            // is held in managed state for the app's lifetime.
            let (host, audio_state, deck_handles) = start_audio();
            // Hydrate the shell-persisted settings (ADR-0020 phase A): the
            // persisted output devices apply HERE, before the webview exists —
            // this replaces App.tsx's localStorage boot replay, so the store
            // (seeded below) is the pickers' single truth. Best-effort: a
            // vanished device leaves the default stream running.
            let shell_settings = settings::load(&app.handle().clone());
            if !shell_settings.main_device.is_empty() || !shell_settings.cue_device.is_empty() {
                let main = &shell_settings.main_device;
                let cue = &shell_settings.cue_device;
                match reopen_main(&host, &audio_state, main, cue) {
                    Ok(()) => {
                        *audio_state
                            .main_name
                            .lock()
                            .unwrap_or_else(|p| p.into_inner()) = main.clone();
                        if !is_combined(main, cue) {
                            match reopen_cue_split(&host, &audio_state, cue) {
                                Ok(()) => {
                                    *audio_state
                                        .cue_name
                                        .lock()
                                        .unwrap_or_else(|p| p.into_inner()) = cue.clone();
                                }
                                Err(e) => eprintln!(
                                    "lsdj-app: persisted cue device '{cue}' not applied: {e}"
                                ),
                            }
                        } else {
                            *audio_state
                                .cue_name
                                .lock()
                                .unwrap_or_else(|p| p.into_inner()) = cue.clone();
                        }
                    }
                    Err(e) => {
                        eprintln!("lsdj-app: persisted main device '{main}' not applied: {e}")
                    }
                }
            }
            // The per-deck analysis PCM taps (gap 1): the sidecars tee model PCM
            // into these, the webview subscribes via subscribe_deck_pcm.
            let taps = sidecar::PcmTaps::new(lsdj_engine::DECK_COUNT);
            // The shell's beat-analysis threads (ADR-0025): the same tee feeds
            // them; they publish the gated value into the store and the echo
            // clock into the engine. Spawned before the sidecars so the tee
            // closures capture live senders from the first PCM frame.
            let analysis_feed = analysis::live::spawn(app.handle().clone());
            let (sidecars, idle_handles) =
                start_sidecars(&app.handle().clone(), deck_handles, &taps, &analysis_feed);
            // The sa3/Magenta generation server (gap 2): the gen-only FastAPI on a
            // loopback port the webview fetches; started with the app.
            let generation_server = generation::GenerationServer::start();
            #[cfg(feature = "managed-runtime")]
            let magenta_gateway = magenta_gateway::MagentaGateway::start();
            // The generated-songs library: the durable-data root from the platform
            // contract plus a JSON registry the take list restores from. On macOS
            // this remains Documents/LSDJ. Auto-save / list / load / delete all go
            // through it.
            let paths = platform_paths::get();
            let songs_dir = paths.legacy_data().map_or_else(
                || paths.data().join("generated_songs"),
                |legacy| {
                    platform_paths::migrate_legacy_dir(
                        &paths.data().join("generated_songs"),
                        &legacy.join("generated_songs"),
                    )
                },
            );
            app.manage(songs::SongLibrary::new(songs_dir.clone()));
            // The generated-samples library: the short-loop counterpart of the songs
            // folder (ADR-0022), the home for deck freezes / generated pads / composed
            // SFX-Music that used to die at session end. Same fixed-folder + registry
            // discipline.
            let samples_dir = paths.legacy_data().map_or_else(
                || paths.data().join("generated_samples"),
                |legacy| {
                    platform_paths::migrate_legacy_dir(
                        &paths.data().join("generated_samples"),
                        &legacy.join("generated_samples"),
                    )
                },
            );
            app.manage(samples::SampleLibrary::new(samples_dir.clone()));
            // Watch both library folders so the Media Explorer tabs live-reload on a
            // change (a deck auto-saving a sample, a hand-drop/-delete); Rust owns the
            // watch and emits `library://changed` (ADR-0022).
            watcher::watch_libraries(app.handle().clone(), songs_dir, samples_dir);
            // The in-app model manager (issue #43): own the install lifecycle and
            // watch the Magenta models dir so the manager + deck picker live-reload
            // when a model folder appears/disappears (`models://changed`).
            app.manage(models::InstallManager::new());
            watcher::watch_models(app.handle().clone(), models::magenta_models_dir());
            // The LoRA adapter registry (issue #66) reflects native adds/deletes
            // live, like the models dir.
            watcher::watch_loras(app.handle().clone(), loras::loras_dir());
            app.manage(host);
            // The shell-level interface-state store (ADR-0020): the single source of
            // truth for the semantic / audio-param interface state the webview
            // projects. Mutated by the same commands that drive the engine, it emits
            // `store://changed` on every change.
            app.manage(store::InterfaceStore::new(app.handle().clone()));
            // Seed the store with the hydrated settings so the webview's
            // pickers project the persisted choices from the first snapshot.
            {
                let store_state = app.state::<store::InterfaceStore>();
                store_state.set_output_devices(
                    shell_settings.main_device.clone(),
                    shell_settings.cue_device.clone(),
                );
                store_state.set_recordings_folder(shell_settings.recordings_folder.clone());
                // Hydrate the persisted style-pad arrangements (ADR-0020
                // phase B). The settings file is user-editable, so the same
                // trust boundary as a preset load applies.
                for (deck, style) in shell_settings
                    .deck_styles
                    .iter()
                    .enumerate()
                    .take(lsdj_engine::DECK_COUNT)
                {
                    if !style.targets.is_empty() {
                        store_state.style_apply_preset(
                            deck,
                            style::sanitize_preset_targets(style.targets.clone()),
                            style.cursor,
                        );
                    }
                }
                // Hydrate the mixer (ADR-0020 phase C): engine + store from
                // the settings file, the shipped defaults when a deck has no
                // entry yet — Rust owns the boot values, and the webview's
                // localStorage replay (and its per-field synced gates) is
                // gone. Cue (PFL) deliberately never persists.
                let host = app.state::<Host>();
                for deck in 0..lsdj_engine::DECK_COUNT {
                    let mixer = shell_settings
                        .deck_mixers
                        .get(deck)
                        .cloned()
                        .unwrap_or_default();
                    host.set_volume(deck, mixer.volume);
                    store_state.set_volume(deck, mixer.volume);
                    for (band, value) in [
                        (lsdj_engine::EqBand::Low, mixer.eq.low),
                        (lsdj_engine::EqBand::Mid, mixer.eq.mid),
                        (lsdj_engine::EqBand::High, mixer.eq.high),
                    ] {
                        host.set_eq(deck, band, value);
                        store_state.set_eq(deck, band, value);
                    }
                    host.set_trim(deck, mixer.trim_db);
                    store_state.set_trim(deck, mixer.trim_db);
                    match mixer.fx_kind {
                        Some(kind) => {
                            let kind: lsdj_engine::FxKind = kind.into();
                            host.set_fx(deck, kind);
                            host.set_fx_amount(deck, mixer.fx_amount);
                            store_state.set_fx(deck, kind);
                            store_state.set_fx_amount(deck, mixer.fx_amount);
                        }
                        None => {
                            host.clear_fx(deck);
                            store_state.clear_fx(deck);
                        }
                    }
                    // Hydrate the live generation params (issue #84) into the
                    // store so the drawer sliders project the persisted tuning
                    // from the first snapshot. The note-steering service (managed
                    // below) also seeds its authored copy, and re-sends it to the
                    // worker on `ready`. Seeded here, before the persistence
                    // watcher, so the hydrate can't echo back to disk.
                    // Clamp on the way in: settings.json is a file trust
                    // boundary, so a hand-edited out-of-range value is pinned
                    // here (the note-steering hydrate below clamps its copy too).
                    let generation = shell_settings
                        .deck_generations
                        .get(deck)
                        .copied()
                        .unwrap_or_default()
                        .clamped();
                    store_state.set_deck_generation(deck, generation);
                }
                host.set_crossfade(shell_settings.crossfade);
                store_state.set_crossfade(shell_settings.crossfade);
                host.set_cue_mix(shell_settings.cue_mix);
                store_state.set_cue_mix(shell_settings.cue_mix);
            }
            // The shell style sender (ADR-0020 phase B): the store owns the
            // arrangement, this service owns the worker blend — computed,
            // throttled, and re-sent on worker `ready`. The settings watcher
            // persists the store's settings slice (styles + mixer, phases
            // B+C), debounced; registered after hydration so the hydrate
            // itself doesn't echo straight back into the settings file.
            let style_sender = style_send::StyleSender::start(app.handle().clone());
            style_sender.watch_store(&app.state::<store::InterfaceStore>());
            settings::watch_persistence(
                app.handle().clone(),
                &app.state::<store::InterfaceStore>(),
            );
            app.manage(style_sender);
            // The shell note-steering service (issue #48, ADR-0031): the single
            // sender for note/drum conditioning — native MIDI, MCP, and the UI
            // all go through it.
            app.manage(midi::notes::NoteSteering::new(app.handle().clone()));
            // Seed the service's authored generation params (issue #84) from
            // the settings file — the value `reassert_generation` re-sends to
            // each fresh worker on `ready` (no worker exists yet, so this only
            // records; the store was seeded above). Runs before any worker can
            // announce `ready`.
            {
                let notes = app.state::<midi::notes::NoteSteering>();
                for deck in 0..lsdj_engine::DECK_COUNT {
                    let generation = shell_settings
                        .deck_generations
                        .get(deck)
                        .copied()
                        .unwrap_or_default();
                    notes.hydrate_generation(deck, generation);
                }
            }
            // Native MIDI I/O (ADR-0031): controller binding, translation, LEDs,
            // and the performance input — the webview shim is gone. The LED
            // painter follows the store through the in-process watcher.
            let midi_service = midi::MidiService::start(app.handle().clone());
            midi_service.watch_store(&app.state::<store::InterfaceStore>());
            app.manage(midi_service);
            app.manage(audio_state);
            app.manage(sidecars);
            app.manage(taps);
            app.manage(analysis_feed);
            app.manage(analysis::track::TrackAnalysis::new(lsdj_engine::DECK_COUNT));
            app.manage(generation_server);
            #[cfg(feature = "managed-runtime")]
            app.manage(magenta_gateway);
            // The native MCP server (ADR-0020 Phase 2): an external agent as a
            // co-DJ. Always on, loopback-only, token-guarded; its tools mutate the
            // same managed state the IPC commands do. Reaches that state through the
            // cloned AppHandle.
            app.manage(mcp::McpServer::start(app.handle().clone()));
            app.manage(IdleHandles(Mutex::new(idle_handles)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_info,
            platform_diagnostics::platform_diagnostics,
            rotate_mcp_token,
            set_mcp_port,
            list_output_devices,
            set_main_device,
            set_cue_device,
            set_recordings_folder,
            commands::set_crossfade,
            commands::set_eq,
            commands::set_volume,
            commands::set_fx,
            commands::set_fx_amount,
            commands::clear_fx,
            commands::set_trim,
            commands::set_on_air,
            commands::set_cue,
            commands::set_cue_mix,
            commands::audition_play,
            commands::audition_stop,
            commands::start_recording,
            commands::stop_recording,
            commands::open_recordings_folder,
            commands::list_audio_files,
            commands::read_audio_file,
            commands::list_generated_songs,
            commands::save_generated_song,
            commands::read_generated_song,
            commands::delete_generated_song,
            commands::open_songs_folder,
            commands::list_generated_samples,
            commands::save_generated_sample,
            commands::read_generated_sample,
            commands::delete_generated_sample,
            commands::open_samples_folder,
            commands::save_loop_slot,
            commands::load_track_file,
            commands::load_track_bytes,
            commands::track_bands,
            commands::unload_track,
            commands::play_track,
            commands::pause_track,
            commands::seek_track,
            commands::set_track_rate,
            commands::nudge_track_phase,
            commands::set_track_loop,
            commands::clear_track_loop,
            commands::capture_loop,
            commands::play_loop,
            commands::stop_loop,
            commands::stop_layer,
            commands::stop_one_shot,
            commands::clear_loop,
            commands::load_generated_loop,
            commands::capture_sample,
            commands::engine_telemetry,
            commands::track_status,
            commands::loop_slots,
            commands::track_peaks,
            commands::engine_snapshot,
            commands::store_snapshot,
            commands::set_deck_model,
            commands::set_deck_cue_point,
            commands::set_deck_mode,
            commands::set_deck_track,
            commands::set_deck_transport,
            commands::set_deck_loop_labels,
            commands::style_add_target,
            commands::style_add_sample_target,
            commands::style_move_target,
            commands::style_remove_target,
            commands::style_rename_target,
            commands::style_toggle_selection,
            commands::style_fan_out,
            commands::style_set_cursor,
            commands::style_apply_preset,
            commands::set_deck_primed,
            commands::set_deck_performance,
            commands::deck_keyboard_note,
            commands::toggle_piano_window,
            commands::set_deck_drums,
            commands::set_deck_drums_strength,
            commands::set_deck_generation,
            commands::reset_deck_generation,
            commands::deck_play,
            commands::deck_stop,
            commands::deck_set_prompt,
            commands::deck_set_model,
            commands::deck_embed_sample,
            commands::subscribe_deck_pcm,
            commands::unsubscribe_deck_pcm,
            commands::analysis_reset,
            midi::midi_status,
            midi::midi_monitor,
            midi::midi_select,
            models::model_status,
            models::install_model,
            models::update_model,
            models::cancel_install,
            models::open_model_folder,
            loras::install_lora,
            loras::delete_lora,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        // Tauri does NOT drop managed state on a macOS quit (tao's event loop ends
        // in `process::exit`, which skips destructors), so the spawned Python
        // servers' `Drop` would never run — leaking them as orphans. Kill them
        // explicitly on `RunEvent::Exit`. (The sidecars also self-terminate on the
        // socket EOF; the generation server has no parent link, so this is the only
        // thing that reaps it.)
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                use tauri::Manager;
                app.state::<generation::GenerationServer>().shutdown();
                #[cfg(feature = "managed-runtime")]
                app.state::<magenta_gateway::MagentaGateway>().shutdown();
                app.state::<sidecar::Sidecars>().shutdown();
                app.state::<models::InstallManager>().shutdown();
                app.state::<mcp::McpServer>().shutdown();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::{bundled_backend_path, is_combined, managed_backend_path};

    #[test]
    fn bundled_backend_lives_under_the_tauri_resource_dir() {
        assert_eq!(
            bundled_backend_path(std::path::Path::new(
                "/Applications/LSDJ.app/Contents/Resources"
            )),
            std::path::Path::new(
                "/Applications/LSDJ.app/Contents/Resources/lsdj_backend/lsdj_backend"
            )
        );
    }

    #[test]
    fn managed_backend_has_one_stable_promoted_launcher_path() {
        let expected_name = if cfg!(windows) {
            "lsdj_backend.exe"
        } else {
            "lsdj_backend"
        };
        assert_eq!(
            managed_backend_path(std::path::Path::new("/profile with spaces/资产")),
            std::path::Path::new("/profile with spaces/资产")
                .join("backend/current")
                .join(expected_name)
        );
    }

    /// The cue rides the main device (combined) when no separate cue device is
    /// chosen — an empty cue name is the "same as main" sentinel.
    #[test]
    fn empty_cue_is_combined() {
        assert!(is_combined("", "")); // default main + default cue
        assert!(is_combined("DDJ-FLX4", "")); // named main, "same as main" cue
    }

    /// Naming the cue device the same as the main device is also combined (the
    /// guard that stops the same physical device opening two streams).
    #[test]
    fn cue_equal_to_main_is_combined() {
        assert!(is_combined("DDJ-FLX4", "DDJ-FLX4"));
        assert!(is_combined("", "")); // both default → same device → combined
    }

    /// A cue device distinct from the main device is split (its own stream).
    #[test]
    fn distinct_cue_device_is_split() {
        assert!(!is_combined("MacBook Speakers", "DDJ-FLX4"));
        assert!(!is_combined("", "DDJ-FLX4")); // default main, a named cue device
        assert!(!is_combined("DDJ-FLX4", "Built-in Output"));
    }
}
