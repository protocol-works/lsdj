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
use std::sync::{mpsc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::analysis::media::{AnalyzedLibrary, TrackInfo};
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
    /// The DJ's 1–5 star rating; absent when unrated, so old rows stay readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    /// The DJ's own normalised words for the sample (`library::normalise_tags`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// BPM + key, filled in the background ([`crate::analysis::media`]).
    #[serde(flatten)]
    pub info: TrackInfo,
}

impl library::RegistryEntry for SampleEntry {
    fn file(&self) -> &str {
        &self.file
    }
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
    /// Kicks the background BPM/key pass ([`crate::analysis::media::spawn`]).
    analyzer: OnceLock<mpsc::Sender<()>>,
}

impl SampleLibrary {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self {
            dir,
            lock: Mutex::new(()),
            analyzer: OnceLock::new(),
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
        self.kick_analysis();
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
            rating: None,
            tags: Vec::new(),
            info: TrackInfo::default(),
        };
        let mut entries: Vec<SampleEntry> = library::load_registry(&self.dir);
        entries.retain(|e| e.file != file);
        entries.push(entry.clone());
        library::save_registry(&self.dir, &entries)?;
        self.kick_analysis();
        Ok(entry)
    }

    fn kick_analysis(&self) {
        if let Some(kick) = self.analyzer.get() {
            let _ = kick.send(());
        }
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

    /// Set or clear (`None`) a sample's 1–5 star rating; returns the updated row.
    /// `overwrite: false` refuses to replace an existing rating
    /// ([`library::check_rating_overwrite`]) — the agent's default.
    pub fn rate(
        &self,
        name: &str,
        rating: Option<u8>,
        overwrite: bool,
    ) -> Result<SampleEntry, String> {
        library::validate_rating(rating)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        library::update_entry(&self.dir, name, |e: &mut SampleEntry| {
            library::check_rating_overwrite(e.rating, rating, overwrite)?;
            e.rating = rating;
            Ok(())
        })
    }

    /// Change a sample's display title, never its file (see
    /// [`crate::songs::SongLibrary::rename`]). Returns the updated row.
    pub fn rename(&self, name: &str, title: &str) -> Result<SampleEntry, String> {
        let title = library::validate_title(title)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        library::update_entry(&self.dir, name, |e: &mut SampleEntry| {
            e.title = title;
            Ok(())
        })
    }

    /// Replace a sample's tags with the normalised `tags`; returns the updated row.
    pub fn tag(&self, name: &str, tags: Vec<String>) -> Result<SampleEntry, String> {
        let tags = library::normalise_tags(&tags)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        library::update_entry(&self.dir, name, |e: &mut SampleEntry| {
            e.tags = tags;
            Ok(())
        })
    }

    /// Save `start..end` seconds of sample `name` as a NEW file and registry row
    /// titled "<title> (crop)", carrying everything but the rating over; the
    /// original is untouched. `name` must be a row in the registry. With `replace`,
    /// the crop overwrites the original instead and its row keeps everything.
    pub fn crop(
        &self,
        name: &str,
        start: f64,
        end: f64,
        replace: bool,
    ) -> Result<SampleEntry, String> {
        let original = {
            let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            library::load_registry::<SampleEntry>(&self.dir)
                .into_iter()
                .find(|e| e.file == name)
                .ok_or_else(|| format!("no library entry named {name}"))?
        };
        if replace {
            library::replace_with_crop(&self.dir, name, start, end)?;
            let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
            return library::update_entry(&self.dir, name, |e: &mut SampleEntry| {
                // A crop can sit in a different key: re-analyse it.
                e.info = TrackInfo::default();
                Ok(())
            });
        }
        let title = library::crop_title(&original.title);
        // Decoded outside the lock: a long take takes a moment to cut, and a
        // rating or a re-list must not wait on it.
        let file = library::write_crop(&self.dir, name, &title, start, end)?;
        let entry = SampleEntry {
            file,
            title,
            rating: None,
            // A crop can sit in a different key: re-analyse it.
            info: TrackInfo::default(),
            ..original
        };
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut entries: Vec<SampleEntry> = library::load_registry(&self.dir);
        // A re-list may already have adopted the new file as hand-added.
        entries.retain(|e| e.file != entry.file);
        entries.push(entry.clone());
        library::save_registry(&self.dir, &entries)?;
        Ok(entry)
    }
}

impl AnalyzedLibrary for SampleLibrary {
    const LIBRARY: &'static str = "samples";

    fn set_analyzer(&self, kick: mpsc::Sender<()>) {
        let _ = self.analyzer.set(kick);
    }

    fn pending_analysis(&self) -> Vec<String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        library::load_registry::<SampleEntry>(&self.dir)
            .into_iter()
            .filter(|e| !e.info.is_current())
            .map(|e| e.file)
            .collect()
    }

    fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        SampleLibrary::read(self, name)
    }

    fn set_info(&self, name: &str, info: TrackInfo) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        library::update_entry(&self.dir, name, |e: &mut SampleEntry| {
            e.info = info;
            Ok(())
        })
        .map(|_| ())
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
                rating: None,
                tags: Vec::new(),
                info: TrackInfo::default(),
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
            rating: None,
            tags: Vec::new(),
            info: TrackInfo::default(),
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

    #[test]
    fn rate_rename_and_tag_round_trip_and_reject_bad_input() {
        let dir =
            std::env::temp_dir().join(format!("lsdj-sample-curate-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let samples = SampleLibrary::new(dir.clone());
        samples
            .record(
                NewSample {
                    title: "Riff".to_string(),
                    prompt: None,
                    model: Some("freeze".to_string()),
                    one_shot: true,
                },
                b"RIFF test bytes",
            )
            .unwrap();
        samples.rate("Riff.wav", Some(2), true).unwrap();
        samples.rename("Riff.wav", "Gritty riff").unwrap();
        samples.tag("Riff.wav", vec!["Drone".to_string()]).unwrap();
        assert!(samples.rate("Riff.wav", Some(6), true).is_err());
        assert!(samples.rename("Riff.wav", "a\nb").is_err());
        assert!(samples.tag("missing.wav", vec![]).is_err());
        drop(samples);

        let rows = SampleLibrary::new(dir.clone()).list().unwrap();
        assert_eq!(rows[0].file, "Riff.wav");
        assert_eq!(rows[0].title, "Gritty riff");
        assert_eq!(rows[0].rating, Some(2));
        assert_eq!(rows[0].tags, ["drone"]);
        assert!(
            rows[0].one_shot,
            "curation leaves the rest of the row alone"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reconcile_keeps_rating_and_tags_and_old_rows_default_them() {
        let mut rated = entry("pad.wav", Some("music"), false);
        rated.rating = Some(1);
        rated.tags = vec!["drone".to_string()];
        assert_eq!(
            reconcile(vec![rated.clone()], &["pad.wav".to_string()]),
            vec![rated]
        );
        let legacy: SampleEntry =
            serde_json::from_str(r#"{"file":"x.wav","title":"x","prompt":null,"model":"freeze"}"#)
                .unwrap();
        assert_eq!(legacy.rating, None);
        assert!(legacy.tags.is_empty());
    }
}
