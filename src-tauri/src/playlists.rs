//! Playlists over the generated-songs library: one `.m3u8` per playlist in
//! `generated_songs/playlists/`, so a set is a standard, hand-editable file that
//! Finder shows and other players import, and copying the songs folder carries the
//! sets with it.
//!
//! The format is the minimal extended M3U: `#EXTM3U`, then per song an
//! `#EXTINF:-1,<title>` line (readability only — the registry stays authoritative)
//! and the song's path relative to the playlist (`../Dub.wav`). Reading keys on the
//! path line alone and keeps only its filename, so a hand-edited absolute path still
//! resolves — and can only ever resolve to a file in the songs folder, because every
//! list is reconciled against that folder's own listing.
//!
//! [`crate::songs::SongLibrary`] owns the songs dir and its lock, so it exposes the
//! playlist operations; this module is the format plus the filesystem steps, with
//! the parse/write/reconcile pieces pure and unit-tested.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::library;

/// The playlists folder inside the songs folder. The library watcher is
/// non-recursive and keys on audio extensions, so writes here never emit.
pub const PLAYLISTS_DIR: &str = "playlists";

const EXTENSION: &str = "m3u8";

/// One playlist as the webview and MCP see it: its name (the file stem) and its
/// song `file`s in play order.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Playlist {
    pub name: String,
    pub files: Vec<String>,
}

/// The song filenames a playlist's text lists, in order. Comments, `#EXT…` lines,
/// and blank lines are skipped; a path keeps only its final component.
pub fn parse_m3u8(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| Path::new(line).file_name()?.to_str().map(str::to_string))
        .collect()
}

/// Render a playlist as `.m3u8` text. `title_of` looks a file's display title up in
/// the registry; a file without one falls back to its stem. Control characters are
/// blanked so a title can never break the one-entry-per-line shape.
pub fn write_m3u8(files: &[String], title_of: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::from("#EXTM3U\n");
    for file in files {
        let title = title_of(file).unwrap_or_else(|| library::title_from_file(file));
        let title: String = title
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        out.push_str(&format!("#EXTINF:-1,{title}\n../{file}\n"));
    }
    out
}

/// Keep only files present in the songs folder (the folder is the truth, as for the
/// registry) and drop repeats, keeping each file's first position.
pub fn reconcile(files: Vec<String>, disk: &[String]) -> Vec<String> {
    let on_disk: HashSet<&str> = disk.iter().map(String::as_str).collect();
    let mut seen: HashSet<String> = HashSet::new();
    files
        .into_iter()
        .filter(|f| on_disk.contains(f.as_str()) && seen.insert(f.clone()))
        .collect()
}

fn playlists_dir(songs_dir: &Path) -> PathBuf {
    songs_dir.join(PLAYLISTS_DIR)
}

fn is_playlist(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

/// The playlist names on disk, sorted case-insensitively. A missing folder is an
/// empty list, not an error — no playlist has been made yet.
fn names(songs_dir: &Path) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(playlists_dir(songs_dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read playlists folder: {e}")),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_playlist(path))
        .filter_map(|path| path.file_stem()?.to_str().map(str::to_string))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    Ok(names)
}

/// Resolve an EXISTING playlist by its listed name. Goes through
/// [`library::scoped_path`], so a webview-supplied name can never reach outside the
/// playlists folder, yet a hand-made file whose name `safe_stem` would rewrite
/// (`a.b.m3u8`) is still addressable.
fn existing_path(songs_dir: &Path, name: &str) -> Result<PathBuf, String> {
    library::scoped_path(&playlists_dir(songs_dir), &format!("{name}.{EXTENSION}"))
        .map_err(|_| format!("no playlist named {name}"))
}

/// Read one playlist file into its file list (unreconciled).
fn read_files(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read playlist: {e}"))?;
    Ok(parse_m3u8(&text))
}

/// Every playlist, each reconciled against `disk` (the songs folder's audio files).
/// The reconcile is not written back: a dropped line is gone for good at the next
/// save, and a song restored from the Trash before then reappears.
pub fn list(songs_dir: &Path, disk: &[String]) -> Result<Vec<Playlist>, String> {
    names(songs_dir)?
        .into_iter()
        .map(|name| {
            let files = read_files(&existing_path(songs_dir, &name)?)?;
            Ok(Playlist {
                name,
                files: reconcile(files, disk),
            })
        })
        .collect()
}

/// Create or replace a playlist with exactly `files` (reconciled against `disk`).
/// An existing playlist is addressed by its own name; a new one's name goes through
/// `safe_stem`, the write boundary for an untrusted name.
pub fn save(
    songs_dir: &Path,
    name: &str,
    files: Vec<String>,
    disk: &[String],
    title_of: impl Fn(&str) -> Option<String>,
) -> Result<Playlist, String> {
    if name.trim().is_empty() {
        return Err("playlist name cannot be empty".to_string());
    }
    let dir = playlists_dir(songs_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create playlists folder: {e}"))?;
    let (name, path) = match existing_path(songs_dir, name) {
        Ok(path) => (name.to_string(), path),
        Err(_) => {
            let stem = library::safe_stem(name, "playlist");
            let path = dir.join(format!("{stem}.{EXTENSION}"));
            (stem, path)
        }
    };
    let files = reconcile(files, disk);
    std::fs::write(&path, write_m3u8(&files, title_of))
        .map_err(|e| format!("cannot write playlist: {e}"))?;
    Ok(Playlist { name, files })
}

/// Rename a playlist's file. A name another playlist already has is refused, not
/// suffixed — the DJ picked that name and should choose again. Compared
/// case-insensitively, since macOS volumes usually are; a case-only change of the
/// playlist's own name is allowed.
pub fn rename(songs_dir: &Path, from: &str, to: &str, disk: &[String]) -> Result<Playlist, String> {
    if to.trim().is_empty() {
        return Err("playlist name cannot be empty".to_string());
    }
    let source = existing_path(songs_dir, from)?;
    let stem = library::safe_stem(to, "playlist");
    let lower = stem.to_lowercase();
    if names(songs_dir)?
        .iter()
        .any(|n| n != from && n.to_lowercase() == lower)
    {
        return Err(format!("a playlist named {stem} already exists"));
    }
    let target = playlists_dir(songs_dir).join(format!("{stem}.{EXTENSION}"));
    std::fs::rename(&source, &target).map_err(|e| format!("cannot rename playlist: {e}"))?;
    Ok(Playlist {
        name: stem,
        files: reconcile(read_files(&target)?, disk),
    })
}

/// Move a playlist to the OS Trash (recoverable). Its songs are untouched.
pub fn delete(songs_dir: &Path, name: &str) -> Result<(), String> {
    let path = existing_path(songs_dir, name)?;
    trash::delete(&path).map_err(|e| format!("cannot move playlist to Trash: {e}"))
}

/// Drop `file` from every playlist that lists it (a song delete). Only playlists
/// that change are rewritten.
pub fn strip(
    songs_dir: &Path,
    file: &str,
    title_of: impl Fn(&str) -> Option<String>,
) -> Result<(), String> {
    for name in names(songs_dir)? {
        let path = existing_path(songs_dir, &name)?;
        let files = read_files(&path)?;
        if files.iter().any(|f| f == file) {
            let kept: Vec<String> = files.into_iter().filter(|f| f != file).collect();
            std::fs::write(&path, write_m3u8(&kept, &title_of))
                .map_err(|e| format!("cannot write playlist: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn temp_songs_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lsdj-playlists-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn m3u8_round_trips_awkward_titles_and_names() {
        let files = strings(&["Dub, Part 2.wav", "Süße Nacht #3.wav", "plain.mp3"]);
        let text = write_m3u8(&files, |f| match f {
            "Dub, Part 2.wav" => Some("Dub, part two # live".to_string()),
            "Süße Nacht #3.wav" => Some("Line\nbreak".to_string()),
            _ => None,
        });
        assert_eq!(
            text,
            "#EXTM3U\n\
             #EXTINF:-1,Dub, part two # live\n../Dub, Part 2.wav\n\
             #EXTINF:-1,Line break\n../Süße Nacht #3.wav\n\
             #EXTINF:-1,plain\n../plain.mp3\n"
        );
        assert_eq!(parse_m3u8(&text), files);
    }

    #[test]
    fn parse_tolerates_hand_edits() {
        let text = "\u{feff}#EXTM3U\r\n\r\n# a comment\r\n/Users/me/songs/Dub.wav\r\nTake.wav\r\n";
        assert_eq!(parse_m3u8(text), strings(&["Dub.wav", "Take.wav"]));
    }

    #[test]
    fn reconcile_drops_missing_files_and_dedupes_keeping_the_first() {
        let disk = strings(&["a.wav", "b.wav", "c.wav"]);
        let out = reconcile(strings(&["b.wav", "gone.wav", "a.wav", "b.wav"]), &disk);
        assert_eq!(out, strings(&["b.wav", "a.wav"]));
    }

    #[test]
    fn a_missing_playlists_folder_lists_as_empty() {
        let dir = temp_songs_dir("empty");
        assert_eq!(list(&dir, &[]).unwrap(), Vec::<Playlist>::new());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_list_rename_and_collision() {
        let dir = temp_songs_dir("rename");
        let disk = strings(&["a.wav", "b.wav"]);
        let saved = save(
            &dir,
            "Warm/up",
            strings(&["b.wav", "x.wav", "a.wav"]),
            &disk,
            |_| None,
        )
        .unwrap();
        // The name is a single safe stem; the missing file is dropped on save.
        assert_eq!(
            saved,
            Playlist {
                name: "Warm-up".into(),
                files: strings(&["b.wav", "a.wav"])
            }
        );
        save(&dir, "Peak", strings(&["a.wav"]), &disk, |_| None).unwrap();

        let err = rename(&dir, "Warm-up", "peak", &disk).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        let renamed = rename(&dir, "Warm-up", "Opener", &disk).unwrap();
        assert_eq!(renamed.files, strings(&["b.wav", "a.wav"]));
        let names: Vec<String> = list(&dir, &disk)
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, strings(&["Opener", "Peak"]));
        assert!(rename(&dir, "../Peak", "x", &disk).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn strip_removes_a_file_from_every_playlist() {
        let dir = temp_songs_dir("strip");
        let disk = strings(&["a.wav", "b.wav"]);
        save(&dir, "One", strings(&["a.wav", "b.wav"]), &disk, |_| None).unwrap();
        save(&dir, "Two", strings(&["b.wav"]), &disk, |_| None).unwrap();
        strip(&dir, "b.wav", |_| None).unwrap();
        let all = list(&dir, &disk).unwrap();
        assert_eq!(all[0].files, strings(&["a.wav"]));
        assert!(all[1].files.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
