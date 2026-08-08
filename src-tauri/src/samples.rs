//! The generated-samples library: the on-disk folder (`~/Documents/LSDJ/
//! generated_samples`) plus a JSON registry, the short-loop counterpart of
//! [`crate::songs`]. It is the home for the sounds that used to die at session end —
//! deck freeze captures, deck generated pads, and the Media Explorer's short
//! SFX/Music compositions (ADR-0022) — so they survive a relaunch and reload into a
//! deck loop slot.
//!
//! Same shape as the songs library — a folder of audio files plus `registry.json`
//! reconciled against disk on every read — so it shares the filesystem + security
//! helpers in [`crate::library`]. A sample entry carries one extra field the songs
//! entry doesn't: `one_shot`, the loop-vs-one-shot verdict reload needs to install
//! the sample back into a slot the way it was made.

use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::library;

/// One row of the sample registry — what the webview shows and loads from. Mirrors
/// [`crate::songs::SongEntry`] plus `oneShot`. `serde` camelCase so the field names
/// match the TS `SampleEntry`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleEntry {
    /// The `.wav` filename inside the folder — the registry identity.
    pub file: String,
    /// Display label: the prompt for a generated/composed sample, or the filename
    /// stem for a freeze / a file added by hand.
    pub title: String,
    /// The prompt that generated the sample; `None` for a freeze capture or a file
    /// LSDJ didn't generate.
    pub prompt: Option<String>,
    /// The source/engine: an engine (`sfx`/`music`/`magenta`) for a generated or
    /// composed sample, `"freeze"` for a deck capture, or `None` for a hand-added
    /// file (label "Imported").
    pub model: Option<String>,
    /// Whether the sample plays ONCE (a one-shot) or loops — reload uses it to pick
    /// the engine's one-shot vs loop install. A freeze and a hand-added file are
    /// loops. Defaulted so an older/partial registry row stays loadable.
    #[serde(default)]
    pub one_shot: bool,
}

/// The metadata the webview sends with a sample to persist (deck pad / composed
/// clip). The WAV bytes ride in the same binary frame, immediately after this JSON
/// (see `commands`). A freeze is persisted server-side via `save_loop_slot`, which
/// builds the equivalent entry from the engine slot.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSample {
    pub title: String,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub one_shot: bool,
}

/// The samples folder plus a lock serialising registry read-modify-write — auto-save
/// fires from both decks and the explorer, and a delete races with them. Held in
/// Tauri managed state for the app's life; the path is fixed at startup from the
/// user's Documents folder, never a webview-supplied path.
pub struct SampleLibrary {
    dir: std::path::PathBuf,
    lock: Mutex<()>,
}

impl SampleLibrary {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self {
            dir,
            lock: Mutex::new(()),
        }
    }

    /// The folder samples are written to (for the "Open samples folder" reveal).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reconcile the registry against the folder and return the current sample list.
    /// Writes the reconciled registry back — only when the reconcile changed it —
    /// so a hand-added or hand-deleted file is remembered without every read
    /// becoming a write (see [`crate::songs::SongLibrary::list`]). Called at
    /// webview startup and when the Samples tab opens.
    pub fn list(&self) -> Result<Vec<SampleEntry>, String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create samples folder: {e}"))?;
        let existing = library::load_registry(&self.dir);
        let reconciled = reconcile(existing.clone(), &library::audio_files(&self.dir)?);
        if reconciled != existing {
            library::save_registry(&self.dir, &reconciled)?;
        }
        Ok(reconciled)
    }

    /// Write a sample to disk under a non-clobbering name, record it in the registry,
    /// and return the stored entry. The WAV is supplied by the caller (a deck pad's
    /// backend response, or a freeze's slot buffer encoded by `save_loop_slot`).
    pub fn record(&self, new: NewSample, wav: &[u8]) -> Result<SampleEntry, String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create samples folder: {e}"))?;
        let stem = library::safe_stem(&new.title, "sample");
        let path = library::unique_wav_path(&self.dir, &stem, |p| p.exists())
            .ok_or("too many samples with this name")?;
        std::fs::write(&path, wav).map_err(|e| format!("cannot write sample: {e}"))?;
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("written sample has no filename")?
            .to_string();
        let entry = SampleEntry {
            file: file.clone(),
            title: new.title,
            prompt: new.prompt,
            model: new.model,
            one_shot: new.one_shot,
        };
        let mut entries: Vec<SampleEntry> = library::load_registry(&self.dir);
        entries.retain(|e| e.file != file);
        entries.push(entry.clone());
        library::save_registry(&self.dir, &entries)?;
        Ok(entry)
    }

    /// Read one sample's bytes, scoped to the folder (`name` is a plain filename). The
    /// bytes are returned over binary IPC, like a song read.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        library::read_scoped(&self.dir, name, library::MAX_AUDIO_BYTES)
    }

    /// Move a sample to the OS Trash and drop it from the registry, so the list and
    /// the folder stay in sync without waiting for the next scan.
    pub fn remove(&self, name: &str) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let target = library::scoped_path(&self.dir, name)?;
        trash::delete(&target).map_err(|e| format!("cannot move sample to Trash: {e}"))?;
        let mut entries: Vec<SampleEntry> = library::load_registry(&self.dir);
        entries.retain(|e| e.file != name);
        library::save_registry(&self.dir, &entries)?;
        Ok(())
    }
}

/// Reconcile a loaded registry against the filenames on disk: keep known entries (in
/// registry order) whose file survives, then append any on-disk file the registry
/// doesn't know yet as a hand-added loop (`prompt`/`model` = `None`,
/// `one_shot = false`). Pure, so it is unit-tested without the filesystem.
fn reconcile(existing: Vec<SampleEntry>, disk: &[String]) -> Vec<SampleEntry> {
    let on_disk: std::collections::HashSet<&str> = disk.iter().map(String::as_str).collect();
    let mut out: Vec<SampleEntry> = existing
        .into_iter()
        .filter(|e| on_disk.contains(e.file.as_str()))
        .collect();
    let known: std::collections::HashSet<String> = out.iter().map(|e| e.file.clone()).collect();
    for file in disk {
        if !known.contains(file) {
            out.push(SampleEntry {
                title: library::title_from_file(file),
                file: file.clone(),
                prompt: None,
                model: None,
                one_shot: false,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, model: Option<&str>, one_shot: bool) -> SampleEntry {
        SampleEntry {
            file: file.to_string(),
            title: file.trim_end_matches(".wav").to_string(),
            prompt: model.map(|_| "a prompt".to_string()),
            model: model.map(str::to_string),
            one_shot,
        }
    }

    #[test]
    fn reconcile_keeps_known_entries_in_order_and_drops_missing() {
        let existing = vec![
            entry("freeze.wav", Some("freeze"), false),
            entry("gone.wav", Some("sfx"), true),
        ];
        let disk = vec!["freeze.wav".to_string()];
        let out = reconcile(existing, &disk);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].file, "freeze.wav");
        assert_eq!(out[0].model.as_deref(), Some("freeze"));
        assert!(!out[0].one_shot);
    }

    #[test]
    fn reconcile_adds_hand_dropped_files_as_loops_with_no_model() {
        let existing = vec![entry("pad.wav", Some("music"), false)];
        let disk = vec!["pad.wav".to_string(), "break.aiff".to_string()];
        let out = reconcile(existing, &disk);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].file, "pad.wav");
        // The hand-dropped file is appended as a loop with no prompt/model.
        assert_eq!(out[1].file, "break.aiff");
        assert_eq!(out[1].title, "break");
        assert!(out[1].prompt.is_none());
        assert!(out[1].model.is_none());
        assert!(!out[1].one_shot, "a hand-added sample defaults to a loop");
    }

    #[test]
    fn entry_without_one_shot_field_defaults_to_loop() {
        // An older / partial registry row (no `oneShot`) must still deserialise — the
        // scan rebuilds files regardless, but provenance should survive too.
        let row: SampleEntry =
            serde_json::from_str(r#"{"file":"x.wav","title":"x","prompt":null,"model":"freeze"}"#)
                .expect("row without oneShot deserialises");
        assert!(!row.one_shot);
    }
}
