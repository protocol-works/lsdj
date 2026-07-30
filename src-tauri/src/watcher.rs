//! A filesystem watcher on the media-library folders (`generated_songs` /
//! `generated_samples`). When an audio file appears or disappears — a deck
//! auto-saving a sample out-of-band (ADR-0022), or a file dropped in / deleted by
//! hand — it emits a `library://changed` Tauri event so the matching Media Explorer
//! tab re-lists. Rust owns the watch and emits; the webview never gets filesystem
//! access (the trust boundary, like the rest of the library surface).
//!
//! Two guards keep it honest:
//! - `registry.json` changes are ignored. Reconcile-on-list rewrites that file on
//!   every read, so reacting to it would loop (emit → re-list → write → emit …).
//! - A short debounce coalesces a burst (one save fires several create/modify
//!   events; a deck saving four freezes fires more) into one emit per library.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{Event, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

/// The `library://changed` payload: which library the webview should re-list.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryChanged {
    /// `"songs"` or `"samples"` — the Media Explorer keys its re-list on this.
    library: &'static str,
}

/// How long to coalesce a burst of FS events before emitting. One emit per library
/// per quiet window keeps the webview from thrashing its re-list.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Watch both library folders and emit `library://changed` on a real change. Spawns
/// a thread that OWNS the watcher (keeping it alive for the app's life) and drains
/// its events; `app` is cloned in to emit. Best-effort: if a watch can't be
/// installed (a platform limit), that tab simply keeps its re-list-on-open / on-mount
/// behaviour — never a hard failure.
pub fn watch_libraries(app: AppHandle, songs_dir: PathBuf, samples_dir: PathBuf) {
    std::thread::Builder::new()
        .name("lsdj-library-watch".into())
        .spawn(move || {
            // Create the folders so the watch installs even before the first save.
            let _ = std::fs::create_dir_all(&songs_dir);
            let _ = std::fs::create_dir_all(&samples_dir);
            // Match event paths against the canonical dirs (the OS reports real paths,
            // e.g. macOS resolves /Users symlinks) AND the as-watched dirs, so a path
            // prefix check is robust. `starts_with` is pure (no fs access), so it
            // works for a deleted file's now-gone path too.
            let songs = [
                std::fs::canonicalize(&songs_dir).unwrap_or_else(|_| songs_dir.clone()),
                songs_dir.clone(),
            ];
            let samples = [
                std::fs::canonicalize(&samples_dir).unwrap_or_else(|_| samples_dir.clone()),
                samples_dir.clone(),
            ];

            let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("lsdj-app: library watcher unavailable ({e}); tabs re-list on open");
                    return;
                }
            };
            for dir in [&songs_dir, &samples_dir] {
                if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
                    eprintln!("lsdj-app: cannot watch {} ({e})", dir.display());
                }
            }

            // Drain + debounce: block for the first event of a burst, coalesce the
            // rest within DEBOUNCE windows, then emit once per changed library.
            loop {
                let Ok(first) = rx.recv() else {
                    return; // the watcher (sole sender) was dropped — app shutdown.
                };
                let mut hit_songs = false;
                let mut hit_samples = false;
                let (s, p) = classify(&first, &songs, &samples);
                hit_songs |= s;
                hit_samples |= p;
                while let Ok(ev) = rx.recv_timeout(DEBOUNCE) {
                    let (s, p) = classify(&ev, &songs, &samples);
                    hit_songs |= s;
                    hit_samples |= p;
                }
                if hit_songs {
                    let _ = app.emit("library://changed", LibraryChanged { library: "songs" });
                }
                if hit_samples {
                    let _ = app.emit("library://changed", LibraryChanged { library: "samples" });
                }
            }
        })
        .expect("failed to spawn lsdj library-watch thread");
}

/// Watch the Magenta models dir and emit `models://changed` when the set of
/// *complete* models changes — a model folder dropped in (issue #43), an install
/// finishing, or a delete. Unlike the flat library watch, models are nested
/// subdirs (`<name>/<name>.mlxfn` + `<name>_state.safetensors`), so this watches
/// RECURSIVELY and keys on the two-file discovery convention rather than a flat
/// audio file: it re-scans after each settled burst and emits only when the
/// complete-model set actually changed, so a half-written / partial folder never
/// fires. Best-effort, like [`watch_libraries`].
pub fn watch_models(app: AppHandle, models_dir: PathBuf) {
    std::thread::Builder::new()
        .name("lsdj-models-watch".into())
        .spawn(move || {
            let _ = std::fs::create_dir_all(&models_dir);
            let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("lsdj-app: models watcher unavailable ({e}); manager re-lists on open");
                    return;
                }
            };
            if let Err(e) = watcher.watch(&models_dir, RecursiveMode::Recursive) {
                eprintln!("lsdj-app: cannot watch {} ({e})", models_dir.display());
                return;
            }

            let mut last: BTreeSet<String> =
                crate::models::discover_installed(&models_dir).into_iter().collect();
            loop {
                if rx.recv().is_err() {
                    return; // the watcher (sole sender) was dropped — app shutdown.
                }
                // Coalesce the burst, then re-scan once it settles.
                while rx.recv_timeout(DEBOUNCE).is_ok() {}
                let now: BTreeSet<String> =
                    crate::models::discover_installed(&models_dir).into_iter().collect();
                if now != last {
                    last = now;
                    let _ = app.emit("models://changed", ());
                }
            }
        })
        .expect("failed to spawn lsdj models-watch thread");
}

/// Watch the LoRA adapter registry (issue #66) and emit `models://changed` when
/// the set of complete adapters changes — an import finishing, a hand-drop, or a
/// delete (in-app or native). Same shape as [`watch_models`]: recursive watch,
/// settled-burst re-scan, emit only on a real set change so the importer's
/// staging dir never fires. Best-effort.
pub fn watch_loras(app: AppHandle, loras_dir: PathBuf) {
    std::thread::Builder::new()
        .name("lsdj-loras-watch".into())
        .spawn(move || {
            let _ = std::fs::create_dir_all(&loras_dir);
            let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("lsdj-app: loras watcher unavailable ({e}); manager re-lists on open");
                    return;
                }
            };
            if let Err(e) = watcher.watch(&loras_dir, RecursiveMode::Recursive) {
                eprintln!("lsdj-app: cannot watch {} ({e})", loras_dir.display());
                return;
            }

            let mut last = crate::loras::discover_names(&loras_dir);
            loop {
                if rx.recv().is_err() {
                    return; // the watcher (sole sender) was dropped — app shutdown.
                }
                // Coalesce the burst, then re-scan once it settles.
                while rx.recv_timeout(DEBOUNCE).is_ok() {}
                let now = crate::loras::discover_names(&loras_dir);
                if now != last {
                    last = now;
                    let _ = app.emit("models://changed", ());
                }
            }
        })
        .expect("failed to spawn lsdj loras-watch thread");
}

/// Classify one FS event: `(touched_songs, touched_samples)`. Only an audio-file
/// change in one of the watched folders counts; `registry.json` and any other path
/// are ignored. `songs`/`samples` are the candidate dir prefixes (canonical + as
/// watched).
fn classify(
    event: &notify::Result<Event>,
    songs: &[PathBuf],
    samples: &[PathBuf],
) -> (bool, bool) {
    let Ok(event) = event else {
        return (false, false);
    };
    let mut hit_songs = false;
    let mut hit_samples = false;
    for path in &event.paths {
        if !is_audio_change(path) {
            continue;
        }
        if songs.iter().any(|d| path.starts_with(d)) {
            hit_songs = true;
        } else if samples.iter().any(|d| path.starts_with(d)) {
            hit_samples = true;
        }
    }
    (hit_songs, hit_samples)
}

/// Whether `path` is a change worth a re-list: an audio file, not `registry.json`
/// (our own reconcile-on-list rewrites that on every read — reacting would loop).
/// Keyed on the extension/name only, so it holds for a deleted file's gone path.
fn is_audio_change(path: &Path) -> bool {
    if path.file_name().and_then(|n| n.to_str()) == Some(crate::library::REGISTRY_FILE) {
        return false;
    }
    crate::library::is_audio_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(path: &str) -> notify::Result<Event> {
        Ok(Event {
            kind: notify::EventKind::Any,
            paths: vec![PathBuf::from(path)],
            attrs: Default::default(),
        })
    }

    #[test]
    fn classify_routes_an_audio_file_to_its_library() {
        let songs = [PathBuf::from("/lib/songs")];
        let samples = [PathBuf::from("/lib/samples")];
        assert_eq!(classify(&ev("/lib/songs/take.wav"), &songs, &samples), (true, false));
        assert_eq!(classify(&ev("/lib/samples/riff.wav"), &songs, &samples), (false, true));
    }

    #[test]
    fn classify_ignores_the_registry_and_non_audio_files() {
        // The registry is rewritten on every list — reacting to it would loop.
        let songs = [PathBuf::from("/lib/songs")];
        let samples = [PathBuf::from("/lib/samples")];
        assert_eq!(classify(&ev("/lib/songs/registry.json"), &songs, &samples), (false, false));
        assert_eq!(classify(&ev("/lib/samples/notes.txt"), &songs, &samples), (false, false));
    }

    #[test]
    fn classify_ignores_a_path_outside_the_watched_folders() {
        let songs = [PathBuf::from("/lib/songs")];
        let samples = [PathBuf::from("/lib/samples")];
        assert_eq!(classify(&ev("/elsewhere/take.wav"), &songs, &samples), (false, false));
    }
}
