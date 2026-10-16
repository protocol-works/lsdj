//! The generated-songs library: the on-disk folder (`~/Documents/LSDJ/
//! generated_songs`) plus a JSON registry recording each take's prompt and model, so
//! the webview can restore its take list across launches.
//!
//! # The registry and the scan
//!
//! `registry.json` in the folder maps each `.wav` to its display title, the prompt
//! that composed it, and the engine/model used. [`SongLibrary::list`] reconciles it
//! against what is actually on disk on every read (the webview calls it at startup):
//! files added by hand appear with `model = None` ("none"), and files deleted from
//! the folder drop out. So the folder is the source of truth; the registry only adds
//! the provenance the filesystem can't carry.
//!
//! The filesystem + security helpers (the `safe_stem` write boundary, the
//! `scoped_path` read/delete boundary, the registry IO) live in [`crate::library`],
//! shared with the parallel [`crate::samples`] library; this module is just the
//! `SongEntry` schema and the song-specific reconcile/record on top.

use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::library;
use crate::playlists::{self, Playlist};

/// A LoRA slot captured in the effective song-generation request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GenerationLora {
    pub name: String,
    pub strength: f64,
}

/// Stable Audio 3 text steering captured for an Advanced generation. Optional
/// CFG/APG means guidance was off; the explicit seed always records the used take.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sa3SteeringRecipe {
    pub negative_prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cfg: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apg: Option<f64>,
    pub seed: u32,
}

/// The current recipe shape accepted from this version of the webview. Registry
/// rows store recipes as opaque JSON so a future version can evolve this shape
/// without making today's shell discard the entire registry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GenerationRecipe {
    pub version: u32,
    pub prompt: String,
    pub engine: String,
    pub seconds: f64,
    pub loras: Vec<GenerationLora>,
    pub sa3: Option<Sa3SteeringRecipe>,
    /// The library take this one grew from (audio-to-audio); absent for a take
    /// composed from text alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<SourceRecipe>,
}

/// Where an audio-to-audio take came from: the source `file`, the window of it
/// sent as init audio, and how the model was told to use it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SourceRecipe {
    pub file: String,
    /// `"remix"`, `"replace"` or `"continue"`.
    pub mode: String,
    pub start: f64,
    pub end: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub init_noise_level: Option<f64>,
    /// Output seconds regenerated; everything outside is the source, kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inpaint_range: Option<[f64; 2]>,
}

/// One row of the song registry — what the webview shows and loads from. `serde`
/// camelCase so the field names match the TS `SongEntry`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongEntry {
    /// The `.wav` filename inside the folder — the registry identity.
    pub file: String,
    /// Display label: the prompt plus its session id for a composed take, or the
    /// filename stem for a file added by hand.
    pub title: String,
    /// The composition prompt; `None` for a file LSDJ didn't generate.
    pub prompt: Option<String>,
    /// The engine/model that composed the take; `None` ("none") for a hand-added file.
    pub model: Option<String>,
    /// Opaque on read for forward compatibility; the frontend validates version
    /// and shape before offering recall.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<serde_json::Value>,
    /// The DJ's 1–5 star rating; absent when unrated, so old rows stay readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    /// The DJ's own normalised words for the take (`library::normalise_tags`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl library::RegistryEntry for SongEntry {
    fn file(&self) -> &str {
        &self.file
    }
}

/// The metadata the webview sends with a freshly composed take. The WAV bytes ride in
/// the same binary frame, immediately after this JSON (see `commands`).
#[derive(Deserialize)]
pub struct NewSong {
    pub title: String,
    pub prompt: String,
    pub model: String,
    #[serde(default)]
    pub recipe: Option<GenerationRecipe>,
}

/// The songs folder plus a lock serialising registry read-modify-write — auto-save
/// can fire for two decks at once, and a delete races with both. Held in Tauri
/// managed state for the app's life. The path is fixed at startup from the user's
/// Documents folder; nothing the webview sends can redirect it.
pub struct SongLibrary {
    dir: std::path::PathBuf,
    lock: Mutex<()>,
}

impl SongLibrary {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self {
            dir,
            lock: Mutex::new(()),
        }
    }

    /// The folder songs are written to (for the "Open songs folder" reveal).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Reconcile the registry against the folder and return the current take list.
    /// Writes the reconciled registry back — only when the reconcile changed it —
    /// so a hand-added or hand-deleted file is remembered without every read
    /// becoming a write (concurrent readers were serialising on the disk write
    /// past the MCP client timeout). Called at webview startup.
    pub fn list(&self) -> Result<Vec<SongEntry>, String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create songs folder: {e}"))?;
        let existing = library::load_registry(&self.dir);
        let reconciled = reconcile(existing.clone(), &library::audio_files(&self.dir)?);
        if reconciled != existing {
            library::save_registry(&self.dir, &reconciled)?;
        }
        Ok(reconciled)
    }

    /// Write a freshly composed take to disk under a non-clobbering name, record it in
    /// the registry, and return the stored entry (the webview keeps the filename to
    /// reload or delete the take later).
    pub fn record(&self, new: NewSong, wav: &[u8]) -> Result<SongEntry, String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create songs folder: {e}"))?;
        let stem = library::safe_stem(&new.title, "song");
        let path = library::unique_wav_path(&self.dir, &stem, |p| p.exists())
            .ok_or("too many songs with this name")?;
        std::fs::write(&path, wav).map_err(|e| format!("cannot write song: {e}"))?;
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("written song has no filename")?
            .to_string();
        let recipe = new
            .recipe
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| format!("cannot serialise song recipe: {e}"))?;
        let entry = SongEntry {
            file: file.clone(),
            title: new.title,
            prompt: Some(new.prompt),
            model: Some(new.model),
            recipe,
            rating: None,
            tags: Vec::new(),
        };
        let mut entries: Vec<SongEntry> = library::load_registry(&self.dir);
        entries.retain(|e| e.file != file);
        entries.push(entry.clone());
        library::save_registry(&self.dir, &entries)?;
        Ok(entry)
    }

    /// Read one song's bytes, scoped to the folder (`name` is a plain filename, never
    /// a path). The bytes are large, so the caller returns them over binary IPC.
    pub fn read(&self, name: &str) -> Result<Vec<u8>, String> {
        library::read_scoped(&self.dir, name, library::MAX_AUDIO_BYTES)
    }

    /// Cut `start..end` seconds of a song into Stable Audio 3 init audio (scoped
    /// like [`Self::read`]; see [`crate::decode::source_clip`]).
    pub fn source_clip(
        &self,
        name: &str,
        start: f64,
        end: Option<f64>,
    ) -> Result<crate::decode::SourceClip, String> {
        let extension = Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_string);
        crate::decode::source_clip(self.read(name)?, extension.as_deref(), start, end)
    }

    /// Move a song to the OS Trash (recoverable) and drop it from the registry, so the
    /// take list and the folder stay in sync without waiting for the next scan.
    pub fn remove(&self, name: &str) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let target = library::scoped_path(&self.dir, name)?;
        trash::delete(&target).map_err(|e| format!("cannot move song to Trash: {e}"))?;
        let mut entries: Vec<SongEntry> = library::load_registry(&self.dir);
        entries.retain(|e| e.file != name);
        library::save_registry(&self.dir, &entries)?;
        playlists::strip(&self.dir, name, title_lookup(&entries))
    }

    /// Set or clear (`None`) a song's 1–5 star rating; returns the updated row.
    /// `overwrite: false` refuses to replace an existing rating
    /// ([`library::check_rating_overwrite`]) — the agent's default.
    pub fn rate(
        &self,
        name: &str,
        rating: Option<u8>,
        overwrite: bool,
    ) -> Result<SongEntry, String> {
        library::validate_rating(rating)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        library::update_entry(&self.dir, name, |e: &mut SongEntry| {
            library::check_rating_overwrite(e.rating, rating, overwrite)?;
            e.rating = rating;
            Ok(())
        })
    }

    /// Change a song's display title — never its file, the identity everything
    /// else holds (curation plan, decision 6). Returns the updated row.
    pub fn rename(&self, name: &str, title: &str) -> Result<SongEntry, String> {
        let title = library::validate_title(title)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        library::update_entry(&self.dir, name, |e: &mut SongEntry| {
            e.title = title;
            Ok(())
        })
    }

    /// Replace a song's tags with the normalised `tags`; returns the updated row.
    pub fn tag(&self, name: &str, tags: Vec<String>) -> Result<SongEntry, String> {
        let tags = library::normalise_tags(&tags)?;
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        library::update_entry(&self.dir, name, |e: &mut SongEntry| {
            e.tags = tags;
            Ok(())
        })
    }

    /// Every playlist, reconciled against the songs on disk.
    pub fn list_playlists(&self) -> Result<Vec<Playlist>, String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        playlists::list(&self.dir, &self.disk()?)
    }

    /// Create or replace a playlist with exactly `files` — the one write path for
    /// create, add, remove, and reorder, so the webview sends the whole list and
    /// there is no per-operation race.
    pub fn save_playlist(&self, name: &str, files: Vec<String>) -> Result<Playlist, String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let entries: Vec<SongEntry> = library::load_registry(&self.dir);
        playlists::save(
            &self.dir,
            name,
            files,
            &self.disk()?,
            title_lookup(&entries),
        )
    }

    /// Rename a playlist; a name already taken is refused.
    pub fn rename_playlist(&self, from: &str, to: &str) -> Result<Playlist, String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        playlists::rename(&self.dir, from, to, &self.disk()?)
    }

    /// Move a playlist to the OS Trash; its songs stay.
    pub fn delete_playlist(&self, name: &str) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        playlists::delete(&self.dir, name)
    }

    /// The audio files in the songs folder, created first so a fresh install lists
    /// cleanly.
    fn disk(&self) -> Result<Vec<String>, String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("cannot create songs folder: {e}"))?;
        library::audio_files(&self.dir)
    }
}

/// A file → registry title lookup for the playlist `#EXTINF` lines.
fn title_lookup(entries: &[SongEntry]) -> impl Fn(&str) -> Option<String> + '_ {
    |file| {
        entries
            .iter()
            .find(|e| e.file == file)
            .map(|e| e.title.clone())
    }
}

/// Reconcile a loaded registry against the filenames actually on disk: keep known
/// entries (in registry order — i.e. composition order) whose file survives, then
/// append any on-disk file the registry doesn't know yet as a hand-added song
/// (`prompt`/`model` = `None`). Pure, so it is unit-tested without the filesystem.
fn reconcile(existing: Vec<SongEntry>, disk: &[String]) -> Vec<SongEntry> {
    let on_disk: std::collections::HashSet<&str> = disk.iter().map(String::as_str).collect();
    let mut out: Vec<SongEntry> = existing
        .into_iter()
        .filter(|e| on_disk.contains(e.file.as_str()))
        .collect();
    let known: std::collections::HashSet<String> = out.iter().map(|e| e.file.clone()).collect();
    for file in disk {
        if !known.contains(file) {
            out.push(SongEntry {
                title: library::title_from_file(file),
                file: file.clone(),
                prompt: None,
                model: None,
                recipe: None,
                rating: None,
                tags: Vec::new(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(file: &str, model: Option<&str>) -> SongEntry {
        SongEntry {
            file: file.to_string(),
            title: file.trim_end_matches(".wav").to_string(),
            prompt: model.map(|_| "a prompt".to_string()),
            model: model.map(str::to_string),
            recipe: None,
            rating: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn reconcile_keeps_known_entries_in_order_and_drops_missing() {
        let existing = vec![entry("first.wav", Some("track")), entry("gone.wav", Some("sfx"))];
        let disk = vec!["first.wav".to_string()];
        let out = reconcile(existing, &disk);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].file, "first.wav");
        assert_eq!(out[0].model.as_deref(), Some("track"));
    }

    #[test]
    fn reconcile_adds_hand_dropped_files_with_no_model() {
        let existing = vec![entry("first.wav", Some("track"))];
        let disk = vec!["first.wav".to_string(), "mixtape.mp3".to_string()];
        let out = reconcile(existing, &disk);
        assert_eq!(out.len(), 2);
        // The known entry keeps its provenance and its place…
        assert_eq!(out[0].file, "first.wav");
        // …and the hand-dropped file is appended with no prompt/model ("none").
        assert_eq!(out[1].file, "mixtape.mp3");
        assert_eq!(out[1].title, "mixtape");
        assert!(out[1].prompt.is_none());
        assert!(out[1].model.is_none());
        assert!(out[1].recipe.is_none());
    }

    #[test]
    fn recipe_round_trips_and_old_rows_remain_readable() {
        let recipe = GenerationRecipe {
            version: 1,
            prompt: "warm dub".to_string(),
            engine: "track".to_string(),
            seconds: 120.0,
            loras: vec![GenerationLora {
                name: "medium/dub".to_string(),
                strength: 1.25,
            }],
            sa3: Some(Sa3SteeringRecipe {
                negative_prompt: "vocals".to_string(),
                cfg: Some(3.0),
                apg: Some(1.0),
                seed: 42,
            }),
            source: None,
        };
        let row = SongEntry {
            file: "dub.wav".to_string(),
            title: "Dub".to_string(),
            prompt: Some("warm dub".to_string()),
            model: Some("track".to_string()),
            recipe: Some(serde_json::to_value(recipe).unwrap()),
            rating: None,
            tags: Vec::new(),
        };
        let encoded = serde_json::to_string(&row).unwrap();
        let decoded: SongEntry = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, row);

        let legacy: SongEntry = serde_json::from_str(
            r#"{"file":"old.wav","title":"Old","prompt":"dub","model":"track"}"#,
        )
        .unwrap();
        assert!(legacy.recipe.is_none());

        let basic = SongEntry {
            file: "basic.wav".to_string(),
            title: "Basic".to_string(),
            prompt: Some("dub".to_string()),
            model: Some("track".to_string()),
            recipe: Some(
                serde_json::to_value(GenerationRecipe {
                    version: 1,
                    prompt: "dub".to_string(),
                    engine: "track".to_string(),
                    seconds: 120.0,
                    loras: vec![],
                    sa3: Some(Sa3SteeringRecipe {
                        negative_prompt: String::new(),
                        cfg: None,
                        apg: None,
                        seed: 55,
                    }),
                    source: None,
                })
                .unwrap(),
            ),
            rating: None,
            tags: Vec::new(),
        };
        let encoded_basic = serde_json::to_string(&basic).unwrap();
        assert!(!encoded_basic.contains("\"cfg\""));
        assert!(!encoded_basic.contains("\"apg\""));
        assert_eq!(
            serde_json::from_str::<SongEntry>(&encoded_basic).unwrap(),
            basic
        );
    }

    #[test]
    fn unknown_future_recipe_shape_survives_library_reconciliation() {
        let dir = std::env::temp_dir().join(format!(
            "lsdj-future-song-recipe-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("future.wav"), b"RIFF future bytes").unwrap();
        let future_recipe = serde_json::json!({
            "version": 2,
            "prompt": { "segments": ["dub", "ambient"] },
            "engine": 7,
            "seconds": "until-done",
            "loras": { "stack": [] },
            "sa3": ["future", "shape"]
        });
        let row = SongEntry {
            file: "future.wav".to_string(),
            title: "Future title".to_string(),
            prompt: Some("legacy display prompt".to_string()),
            model: Some("track".to_string()),
            recipe: Some(future_recipe.clone()),
            rating: None,
            tags: Vec::new(),
        };
        std::fs::write(
            dir.join("registry.json"),
            serde_json::to_vec(&vec![row]).unwrap(),
        )
        .unwrap();

        let songs = SongLibrary::new(dir.clone());
        let rows = songs.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Future title");
        assert_eq!(rows[0].prompt.as_deref(), Some("legacy display prompt"));
        assert_eq!(rows[0].model.as_deref(), Some("track"));
        assert_eq!(rows[0].recipe.as_ref(), Some(&future_recipe));

        let rewritten: Vec<SongEntry> =
            serde_json::from_slice(&std::fs::read(dir.join("registry.json")).unwrap()).unwrap();
        assert_eq!(rewritten[0].recipe.as_ref(), Some(&future_recipe));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_fresh_library_instance_restores_the_recorded_recipe() {
        let dir = std::env::temp_dir().join(format!(
            "lsdj-song-recipe-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ));
        std::fs::remove_dir_all(&dir).ok();
        let recipe = GenerationRecipe {
            version: 1,
            prompt: "warm dub".to_string(),
            engine: "track".to_string(),
            seconds: 120.0,
            loras: vec![GenerationLora {
                name: "medium/dub".to_string(),
                strength: 1.25,
            }],
            sa3: Some(Sa3SteeringRecipe {
                negative_prompt: "vocals".to_string(),
                cfg: Some(3.0),
                apg: Some(1.0),
                seed: 42,
            }),
            source: None,
        };

        let first = SongLibrary::new(dir.clone());
        let recorded = first
            .record(
                NewSong {
                    title: "Dub".to_string(),
                    prompt: "warm dub".to_string(),
                    model: "track".to_string(),
                    recipe: Some(recipe.clone()),
                },
                b"RIFF test bytes",
            )
            .unwrap();
        drop(first);

        let restarted = SongLibrary::new(dir.clone());
        let rows = restarted.list().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].file, recorded.file);
        assert_eq!(
            rows[0].recipe.as_ref(),
            Some(&serde_json::to_value(&recipe).unwrap())
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp_library(tag: &str) -> (std::path::PathBuf, SongLibrary) {
        let dir = std::env::temp_dir().join(format!("lsdj-song-{tag}-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let songs = SongLibrary::new(dir.clone());
        songs
            .record(
                NewSong {
                    title: "Dub".to_string(),
                    prompt: "warm dub".to_string(),
                    model: "track".to_string(),
                    recipe: None,
                },
                b"RIFF test bytes",
            )
            .unwrap();
        (dir, songs)
    }

    #[test]
    fn rate_rename_and_tag_round_trip_through_a_fresh_instance() {
        let (dir, songs) = temp_library("curate");
        songs.rate("Dub.wav", Some(4), true).unwrap();
        let renamed = songs.rename("Dub.wav", "  Late Night Dub ").unwrap();
        assert_eq!(renamed.title, "Late Night Dub");
        let tagged = songs
            .tag(
                "Dub.wav",
                vec!["#Deep House".to_string(), "deep-house".to_string()],
            )
            .unwrap();
        assert_eq!(tagged.tags, ["deep-house"]);
        drop(songs);

        let rows = SongLibrary::new(dir.clone()).list().unwrap();
        assert_eq!(rows[0].file, "Dub.wav", "a rename never touches the file");
        assert_eq!(rows[0].title, "Late Night Dub");
        assert_eq!(rows[0].prompt.as_deref(), Some("warm dub"));
        assert_eq!(rows[0].model.as_deref(), Some("track"));
        assert_eq!(rows[0].rating, Some(4));
        assert_eq!(rows[0].tags, ["deep-house"]);

        // Without `overwrite` a different rating is refused and the stored one
        // kept; restating the same stars passes.
        let guarded = SongLibrary::new(dir.clone());
        assert!(guarded.rate("Dub.wav", Some(2), false).is_err());
        assert!(guarded.rate("Dub.wav", None, false).is_err());
        assert_eq!(
            guarded.rate("Dub.wav", Some(4), false).unwrap().rating,
            Some(4)
        );

        let cleared = SongLibrary::new(dir.clone())
            .rate("Dub.wav", None, true)
            .unwrap();
        assert_eq!(cleared.rating, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn curation_rejects_bad_input_and_unknown_files_without_writing() {
        let (dir, songs) = temp_library("reject");
        songs.rate("Dub.wav", Some(3), true).unwrap();
        assert!(songs.rate("Dub.wav", Some(0), true).is_err());
        assert!(songs.rate("Dub.wav", Some(6), true).is_err());
        assert!(songs.rate("nope.wav", Some(3), true).is_err());
        assert!(songs.rename("Dub.wav", "").is_err());
        assert!(songs.rename("Dub.wav", "   ").is_err());
        assert!(songs.rename("Dub.wav", &"x".repeat(201)).is_err());
        assert!(songs.tag("Dub.wav", vec!["a\u{0}b".to_string()]).is_err());
        let row = &songs.list().unwrap()[0];
        assert_eq!((row.title.as_str(), row.rating), ("Dub", Some(3)));
        assert!(row.tags.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reconcile_keeps_rating_and_tags_and_old_rows_default_them() {
        let mut rated = entry("first.wav", Some("track"));
        rated.rating = Some(5);
        rated.tags = vec!["banger".to_string()];
        let out = reconcile(vec![rated.clone()], &["first.wav".to_string()]);
        assert_eq!(out, vec![rated]);

        let legacy: SongEntry = serde_json::from_str(
            r#"{"file":"old.wav","title":"Old","prompt":"dub","model":"track"}"#,
        )
        .unwrap();
        assert_eq!(legacy.rating, None);
        assert!(legacy.tags.is_empty());
        // Unrated, untagged rows serialise without the new keys.
        let encoded = serde_json::to_string(&legacy).unwrap();
        assert!(!encoded.contains("rating") && !encoded.contains("tags"));
    }

    #[test]
    fn playlists_save_and_list_with_registry_titles() {
        let (dir, songs) = temp_library("playlist");
        songs.rename("Dub.wav", "Dub, the long one").unwrap();
        let saved = songs
            .save_playlist(
                "Warm-up",
                vec!["Dub.wav".to_string(), "gone.wav".to_string()],
            )
            .unwrap();
        assert_eq!(saved.files, ["Dub.wav"]);
        let text = std::fs::read_to_string(dir.join("playlists/Warm-up.m3u8")).unwrap();
        assert_eq!(text, "#EXTM3U\n#EXTINF:-1,Dub, the long one\n../Dub.wav\n");
        assert_eq!(songs.list_playlists().unwrap(), vec![saved]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
