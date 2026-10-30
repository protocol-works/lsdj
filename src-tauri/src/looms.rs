//! Looms over the generated-songs library: one JSON document per loom in
//! `generated_songs/looms/<id>.json`, next to the songs its nodes name (decision 8
//! of the loom plan).
//!
//! The shell stores a loom opaquely, like a song recipe: it validates the id, the
//! name and the size, stamps the name and `updatedAt`, and serves the document
//! back. It never resolves a song path from it — the webview loads every node
//! through the scoped `TrackSource {kind:'song'}` boundary and validates the shape
//! on read. No library event fires: the webview is the only writer and refreshes
//! its own list after a save or delete.
//!
//! [`crate::songs::SongLibrary`] owns the songs dir and its lock, so it exposes the
//! loom operations; this module is the id rules plus the filesystem steps, like
//! [`crate::playlists`], and the frame window of the node cue (`audition_song_clip`).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::library;

/// The looms folder inside the songs folder. The library watcher is non-recursive
/// and keys on audio extensions, so writes here never emit.
pub const LOOMS_DIR: &str = "looms";

const EXTENSION: &str = "json";

/// The largest loom document read or written. A loom is a small graph over song
/// filenames; 2 MiB is thousands of nodes.
pub const MAX_LOOM_BYTES: u64 = 2 * 1024 * 1024;

/// One loom as the bar menu lists it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoomSummary {
    pub id: String,
    pub name: String,
    pub updated_at: u64,
    pub nodes: usize,
}

/// A loom id is `^[a-z0-9][a-z0-9-]{7,39}$` (the webview mints a lowercased
/// `crypto.randomUUID()`). The charset alone keeps it one plain filename.
pub fn validate_loom_id(id: &str) -> Result<(), String> {
    let valid = (8..=40).contains(&id.len())
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if valid {
        Ok(())
    } else {
        Err("invalid loom id".to_string())
    }
}

/// The listing row for a loom document: its `name`, `updatedAt` and node count,
/// each defaulting when absent (the shell never trusts the shape beyond this).
pub fn summarise(id: &str, loom: &Value) -> LoomSummary {
    LoomSummary {
        id: id.to_string(),
        name: loom["name"].as_str().unwrap_or_default().to_string(),
        updated_at: loom["updatedAt"].as_f64().map_or(0, |ms| ms as u64),
        nodes: loom["nodes"].as_array().map_or(0, Vec::len),
    }
}

fn looms_dir(songs_dir: &Path) -> PathBuf {
    songs_dir.join(LOOMS_DIR)
}

fn file_name(id: &str) -> String {
    format!("{id}.{EXTENSION}")
}

/// Read and parse one loom by id, scoped to the looms folder and capped in size.
pub fn read(songs_dir: &Path, id: &str) -> Result<Value, String> {
    validate_loom_id(id)?;
    let bytes = library::read_scoped(&looms_dir(songs_dir), &file_name(id), MAX_LOOM_BYTES)
        .map_err(|e| format!("cannot read loom {id}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("cannot parse loom {id}: {e}"))
}

/// Every loom on disk, newest first. A missing folder is an empty list; a file
/// whose stem is not a loom id, or that does not parse, is skipped rather than
/// breaking the menu.
pub fn list(songs_dir: &Path) -> Result<Vec<LoomSummary>, String> {
    let entries = match std::fs::read_dir(looms_dir(songs_dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot read looms folder: {e}")),
    };
    let mut looms: Vec<LoomSummary> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some(EXTENSION))
        .filter_map(|path| path.file_stem()?.to_str().map(str::to_string))
        .filter_map(|id| read(songs_dir, &id).ok().map(|loom| summarise(&id, &loom)))
        .collect();
    looms.sort_by_key(|loom| std::cmp::Reverse(loom.updated_at));
    Ok(looms)
}

/// Create or replace a loom. `name` goes through [`library::validate_title`] and is
/// stamped into the document, `updatedAt` is stamped with `now` (ms) only when
/// absent, and the rest is written as received. The write is atomic (a temp file
/// renamed over the target), so a crash mid-save never leaves half a loom.
pub fn save(
    songs_dir: &Path,
    id: &str,
    name: &str,
    mut loom: Value,
    now: u64,
) -> Result<LoomSummary, String> {
    validate_loom_id(id)?;
    let name = library::validate_title(name)?;
    let document = loom.as_object_mut().ok_or("a loom must be a JSON object")?;
    document.insert("name".to_string(), Value::from(name));
    document.entry("updatedAt").or_insert(Value::from(now));
    let bytes = serde_json::to_vec(&loom).map_err(|e| format!("cannot serialise loom: {e}"))?;
    if bytes.len() as u64 > MAX_LOOM_BYTES {
        return Err("loom is too large".to_string());
    }
    let dir = looms_dir(songs_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create looms folder: {e}"))?;
    // A dotted, non-.json name the listing skips while it's being written.
    let tmp = dir.join(format!(".{id}.tmp"));
    std::fs::write(&tmp, &bytes)
        .and_then(|()| std::fs::rename(&tmp, dir.join(file_name(id))))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write loom: {e}")
        })?;
    Ok(summarise(id, &loom))
}

/// Move a loom to the OS Trash (recoverable). The songs it names are untouched.
pub fn delete(songs_dir: &Path, id: &str) -> Result<(), String> {
    validate_loom_id(id)?;
    let path = library::scoped_path(&looms_dir(songs_dir), &file_name(id))
        .map_err(|_| format!("no loom {id}"))?;
    trash::delete(&path).map_err(|e| format!("cannot move loom to Trash: {e}"))
}

/// The `[first, last)` 48 kHz frames a node cue plays of a `total`-frame song:
/// `start..end` seconds (`None` = to the end) clamped into the track. A start at or
/// past the end, or an end at or before the start, is refused — nothing to cue.
pub fn audition_frames(
    start: f64,
    end: Option<f64>,
    total: usize,
) -> Result<(usize, usize), String> {
    if !start.is_finite() || end.is_some_and(|end| !end.is_finite()) {
        return Err("the cue window must be finite seconds".to_string());
    }
    let rate = lsdj_engine::SAMPLE_RATE as f64;
    let frame = |seconds: f64| ((seconds.max(0.0) * rate).round() as usize).min(total);
    let first = frame(start);
    let last = end.map_or(total, frame);
    if first >= total {
        return Err(format!(
            "the cue starts at {start:.1}s, past the song's end ({:.1}s)",
            total as f64 / rate
        ));
    }
    if last <= first {
        return Err("the cue ends before it starts".to_string());
    }
    Ok((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "0f8b2c1e-7d4a-4c3b-9e2f-5a6b7c8d9e0f";

    fn temp_songs_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lsdj-looms-{tag}-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loom_ids_are_one_lowercase_plain_name() {
        assert!(validate_loom_id(ID).is_ok());
        assert!(validate_loom_id("abcd1234").is_ok());
        assert!(validate_loom_id(&"a".repeat(40)).is_ok());
        let too_long = "a".repeat(41);
        for bad in [
            "abc1234",
            too_long.as_str(),
            "ABCD1234",
            "-abcd1234",
            "../abcd1234",
            "abcd/1234",
            "abcd.1234",
            "abcd 1234",
        ] {
            assert!(validate_loom_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn summarise_reads_name_updated_and_node_count_with_defaults() {
        let loom = json!({"name": "Set", "updatedAt": 1_730_000_000_123u64, "nodes": [{}, {}]});
        assert_eq!(
            summarise(ID, &loom),
            LoomSummary {
                id: ID.into(),
                name: "Set".into(),
                updated_at: 1_730_000_000_123,
                nodes: 2
            }
        );
        assert_eq!(summarise(ID, &json!({})).nodes, 0);
    }

    #[test]
    fn save_round_trips_and_stamps_name_and_missing_updated_at() {
        let dir = temp_songs_dir("round-trip");
        let loom = json!({"version": 1, "name": "old", "nodes": [{"file": "Dub.wav"}]});
        let saved = save(&dir, ID, "  Warm-up ", loom, 42).unwrap();
        assert_eq!(
            saved,
            LoomSummary {
                id: ID.into(),
                name: "Warm-up".into(),
                updated_at: 42,
                nodes: 1
            }
        );
        assert_eq!(
            read(&dir, ID).unwrap(),
            json!({"version": 1, "name": "Warm-up", "updatedAt": 42, "nodes": [{"file": "Dub.wav"}]})
        );
        // A webview-stamped updatedAt is kept.
        let kept = save(&dir, ID, "Warm-up", json!({"updatedAt": 7, "nodes": []}), 42).unwrap();
        assert_eq!(kept.updated_at, 7);
        assert!(save(&dir, ID, " ", json!({}), 42).is_err());
        assert!(save(&dir, ID, "Warm-up", json!([]), 42).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_oversized_loom_is_refused_and_nothing_is_written() {
        let dir = temp_songs_dir("cap");
        let big = json!({"nodes": [], "pad": "x".repeat(MAX_LOOM_BYTES as usize)});
        assert!(save(&dir, ID, "Big", big, 1).unwrap_err().contains("too large"));
        assert!(read(&dir, ID).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_is_newest_first_and_skips_foreign_files() {
        let dir = temp_songs_dir("list");
        assert!(list(&dir).unwrap().is_empty());
        save(&dir, "older-loom", "Older", json!({"updatedAt": 100}), 0).unwrap();
        save(&dir, "newer-loom", "Newer", json!({"updatedAt": 200}), 0).unwrap();
        std::fs::write(dir.join(LOOMS_DIR).join("Not An Id.json"), "{}").unwrap();
        std::fs::write(dir.join(LOOMS_DIR).join("broken-loom.json"), "{").unwrap();
        let ids: Vec<String> = list(&dir).unwrap().into_iter().map(|l| l.id).collect();
        assert_eq!(ids, ["newer-loom", "older-loom"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn audition_frames_clamp_into_the_track_and_refuse_an_empty_cue() {
        let total = 96_000; // 2 s at 48 kHz
        assert_eq!(audition_frames(0.5, Some(1.0), total), Ok((24_000, 48_000)));
        assert_eq!(audition_frames(1.0, None, total), Ok((48_000, total)));
        assert_eq!(audition_frames(-1.0, Some(1e9), total), Ok((0, total)));
        assert!(audition_frames(2.0, None, total).is_err());
        assert!(audition_frames(5.0, Some(6.0), total).is_err());
        assert!(audition_frames(1.0, Some(1.0), total).is_err());
        assert!(audition_frames(1.0, Some(0.5), total).is_err());
        assert!(audition_frames(f64::NAN, None, total).is_err());
        assert!(audition_frames(0.0, Some(f64::INFINITY), total).is_err());
    }

    #[test]
    fn delete_and_read_of_a_missing_or_bad_id_are_errors() {
        let dir = temp_songs_dir("missing");
        assert!(delete(&dir, ID).unwrap_err().contains("no loom"));
        assert!(read(&dir, ID).is_err());
        assert!(delete(&dir, "../registry").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
