//! Shared filesystem + security helpers for the on-disk media libraries
//! ([`crate::songs`] generated songs, [`crate::samples`] generated samples). The
//! folder layout is the same for both — a folder of audio files plus a JSON
//! `registry.json` carrying the provenance the filesystem can't — so the traversal
//! defenses (`safe_stem` for a write, `scoped_path` for a read/delete) live here
//! ONCE rather than copy-pasted per library: a fix to the boundary cannot then miss
//! one of them (see `.claude/rules/security.md`).
//!
//! Each library layers its own registry entry type, `reconcile`, and `record` on
//! top of these — the per-entry shape stays boringly explicit in its own module.

use std::fs::{File, OpenOptions};
use std::path::{Component, Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// The registry file living inside a library folder. Excluded from the audio scan
/// (it is not an audio file).
pub const REGISTRY_FILE: &str = "registry.json";

/// The audio extensions a scan surfaces (mirrors the folder browser's
/// `commands::AUDIO_EXTENSIONS`), compared case-insensitively — so a file dropped in
/// by hand in any of these formats is picked up, not just our own `.wav`.
pub const AUDIO_EXTENSIONS: [&str; 7] = ["wav", "mp3", "flac", "m4a", "ogg", "aif", "aiff"];

/// A generous per-file read cap so a pathological file can't OOM the webview
/// (mirrors `commands::MAX_AUDIO_BYTES`).
pub const MAX_AUDIO_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The longest filename stem a take/sample gets. A prompt can be thousands of chars
/// (even a pasted JSON spec), but a single filename component is OS-capped
/// (~255 bytes), so the filename takes only the first MAX_STEM_CHARS — the registry
/// carries the full title/prompt, the file is just an identifier.
pub const MAX_STEM_CHARS: usize = 80;

/// Reduce an untrusted title to a SINGLE safe filename stem: every character that
/// isn't alphanumeric, space, `-`, `_`, or `#` becomes `-` (so no `/`, `\`, `.`, or
/// other separator survives) and an empty result falls back to `fallback`. With no
/// separator able to survive, the name stays one path component and cannot escape
/// the library folder — the boundary for a webview-supplied title.
pub fn safe_stem(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '#') {
                c
            } else {
                '-'
            }
        })
        .collect();
    // Take only the first MAX_STEM_CHARS: a prompt can be thousands of chars now, and
    // a filename that long would blow the OS component limit (the write would fail).
    let capped: String = cleaned.trim().chars().take(MAX_STEM_CHARS).collect();
    let trimmed = capped.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// A non-clobbering path inside `dir`: `<stem>.wav`, else `<stem> (2).wav`,
/// `<stem> (3).wav`, … Auto-save fires on every generation and session ids restart
/// each launch, so two runs can mint the same display name — never overwrite an
/// earlier take. Returns `None` when every candidate up to the bound is taken, so
/// the caller errors rather than clobbering. `exists` is injected so the search is
/// unit-testable without the filesystem.
pub fn unique_wav_path(dir: &Path, stem: &str, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let first = dir.join(format!("{stem}.wav"));
    if !exists(&first) {
        return Some(first);
    }
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({n}).wav"));
        if !exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Atomically create a fresh, empty `<stem>.wav` (else `<stem> (2).wav`, …) inside
/// `dir` and hand back the open file with its path. Unlike picking a free path with
/// [`unique_wav_path`] and opening it afterwards, this opens each candidate with
/// `create_new` (`O_CREAT | O_EXCL`), so there is no window between the check and the
/// open: a candidate that already exists — including a symlink, which `O_EXCL` refuses
/// to follow — is never truncated; we just move to the next number. The caller
/// streams into the returned file. Errors if every candidate up to the bound is taken
/// or the open fails for another reason (e.g. permissions).
pub fn create_unique_wav(dir: &Path, stem: &str) -> Result<(File, PathBuf), String> {
    for n in 1..10_000 {
        let candidate = if n == 1 {
            dir.join(format!("{stem}.wav"))
        } else {
            dir.join(format!("{stem} ({n}).wav"))
        };
        match OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            // The name is taken (a real file or a symlink O_EXCL won't follow) — try
            // the next number rather than clobber it.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot create recording file: {e}")),
        }
    }
    Err("too many recordings with this name".into())
}

/// The audio filenames directly inside `dir` (non-recursive), sorted
/// case-insensitively. `registry.json` and any non-audio file are skipped.
pub fn audio_files(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("cannot read library folder: {e}"))?;
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_audio_file(path))
        .filter_map(|path| path.file_name()?.to_str().map(str::to_string))
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    Ok(names)
}

/// Whether `path` has one of [`AUDIO_EXTENSIONS`] (case-insensitive).
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// The display title for a hand-added file: its name without the extension.
pub fn title_from_file(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(file)
        .to_string()
}

fn registry_path(dir: &Path) -> PathBuf {
    dir.join(REGISTRY_FILE)
}

/// Load a library's registry, treating a missing or corrupt file as empty — the scan
/// rebuilds the list from disk regardless, so a damaged registry only loses
/// provenance, never the files. Generic over the entry type so each library reads
/// its own schema.
pub fn load_registry<T: DeserializeOwned>(dir: &Path) -> Vec<T> {
    std::fs::read(registry_path(dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Write a library's registry as pretty JSON.
pub fn save_registry<T: Serialize>(dir: &Path, entries: &[T]) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(entries)
        .map_err(|e| format!("cannot serialise registry: {e}"))?;
    std::fs::write(registry_path(dir), json).map_err(|e| format!("cannot write registry: {e}"))
}

/// A registry row that knows its own identity — the `file` every library keys on —
/// so [`update_entry`] can find it without knowing the row's schema.
pub trait RegistryEntry {
    fn file(&self) -> &str;
}

/// Read-modify-write ONE registry row: load the registry, find the row whose `file`
/// is `file` (an error if there is none — a stale row must not be resurrected), let
/// `apply` change it, save, and return the updated row. The caller holds its
/// library's lock; rating, renaming, and tagging are all this one write.
pub fn update_entry<T>(
    dir: &Path,
    file: &str,
    apply: impl FnOnce(&mut T) -> Result<(), String>,
) -> Result<T, String>
where
    T: RegistryEntry + Clone + Serialize + DeserializeOwned,
{
    let mut entries: Vec<T> = load_registry(dir);
    let entry = entries
        .iter_mut()
        .find(|e| e.file() == file)
        .ok_or_else(|| format!("no library entry named {file}"))?;
    apply(entry)?;
    let updated = entry.clone();
    save_registry(dir, &entries)?;
    Ok(updated)
}

/// A star rating is 1–5, or `None` to clear it — there is no zero star (clearing
/// is a click on the current star, curation plan decision 3).
pub fn validate_rating(rating: Option<u8>) -> Result<(), String> {
    match rating {
        Some(stars) if !(1..=5).contains(&stars) => Err(format!("rating must be 1–5, got {stars}")),
        _ => Ok(()),
    }
}

/// Refuse to replace a rating already on a row unless the caller asked to: the
/// DJ rates in the app while an agent works, and a blind write would erase their
/// number. Rating an unrated row, or restating the same stars, always passes.
pub fn check_rating_overwrite(
    current: Option<u8>,
    next: Option<u8>,
    overwrite: bool,
) -> Result<(), String> {
    match current {
        Some(stars) if !overwrite && current != next => Err(format!(
            "already rated {stars} stars; pass overwrite: true to replace that rating"
        )),
        _ => Ok(()),
    }
}

/// The longest display title a rename accepts. Looser than [`MAX_STEM_CHARS`]: a
/// title is only a label in the registry, never a filename.
pub const MAX_TITLE_CHARS: usize = 200;

/// Validate a webview-supplied display title (decision 7 of the curation plan):
/// trimmed, non-empty, no control characters, at most [`MAX_TITLE_CHARS`]. Returns
/// the trimmed title to store.
pub fn validate_title(title: &str) -> Result<String, String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err("title cannot be empty".to_string());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("title cannot contain control characters".to_string());
    }
    if trimmed.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("title is longer than {MAX_TITLE_CHARS} characters"));
    }
    Ok(trimmed.to_string())
}

/// The longest single tag, and the most tags one row carries.
pub const MAX_TAG_CHARS: usize = 32;
pub const MAX_TAGS: usize = 16;

/// Normalise a webview-supplied tag list (decision 8): each tag trimmed, lowercased,
/// one leading `#` dropped, inner whitespace runs folded to `-` (so a tag is one
/// search term), empties dropped, deduped keeping the first. A control character, an
/// over-long tag, or too many tags is refused rather than silently clipped, so the
/// DJ never loses a word without being told.
pub fn normalise_tags(tags: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for raw in tags {
        if raw.chars().any(char::is_control) {
            return Err("tags cannot contain control characters".to_string());
        }
        let lower = raw.trim().to_lowercase();
        let bare = lower.strip_prefix('#').unwrap_or(&lower);
        let tag = bare.split_whitespace().collect::<Vec<_>>().join("-");
        if tag.is_empty() || out.contains(&tag) {
            continue;
        }
        if tag.chars().count() > MAX_TAG_CHARS {
            return Err(format!(
                "tag \"{tag}\" is longer than {MAX_TAG_CHARS} characters"
            ));
        }
        out.push(tag);
    }
    if out.len() > MAX_TAGS {
        return Err(format!("a row can carry at most {MAX_TAGS} tags"));
    }
    Ok(out)
}

/// Read one file's bytes scoped to `dir` (`name` is a plain filename, never a path —
/// see [`scoped_path`]), refusing a non-file or one larger than `max_bytes`. The
/// read/delete boundary for a webview-supplied name.
pub fn read_scoped(dir: &Path, name: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    let target = scoped_path(dir, name)?;
    let meta = std::fs::metadata(&target).map_err(|e| format!("cannot stat file: {e}"))?;
    if !meta.is_file() {
        return Err("not a regular file".to_string());
    }
    if meta.len() > max_bytes {
        return Err("file is too large".to_string());
    }
    std::fs::read(&target).map_err(|e| format!("cannot read file: {e}"))
}

/// The title a crop of `title` is recorded under.
pub fn crop_title(title: &str) -> String {
    format!("{title} (crop)")
}

/// Cut `start..end` seconds of library file `name` (scoped like [`read_scoped`])
/// into a fresh WAV beside it, named after `title`, and return the new file's name.
/// The original is only read. See [`crate::decode::crop_wav`].
pub fn write_crop(
    dir: &Path,
    name: &str,
    title: &str,
    start: f64,
    end: f64,
) -> Result<String, String> {
    use std::io::Write;
    let bytes = read_scoped(dir, name, MAX_AUDIO_BYTES)?;
    let extension = Path::new(name).extension().and_then(|ext| ext.to_str());
    let wav = crate::decode::crop_wav(bytes, extension, start, end)?;
    let (mut file, path) = create_unique_wav(dir, &safe_stem(title, "crop"))?;
    if let Err(e) = file.write_all(&wav) {
        // Never leave a truncated crop for the folder scan to list.
        let _ = std::fs::remove_file(&path);
        return Err(format!("cannot write crop: {e}"));
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .ok_or_else(|| "written crop has no filename".to_string())
}

/// Overwrite `name` in place with its `start..end` crop, so its registry row (rating,
/// tags, playlists) still points at it. A copy of the original goes to the Trash
/// first (recoverable), then the crop is renamed over it atomically, so the file
/// is never missing for a re-list to drop. WAV only: the crop is WAV bytes.
pub fn replace_with_crop(dir: &Path, name: &str, start: f64, end: f64) -> Result<(), String> {
    let target = scoped_path(dir, name)?;
    if !target
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
    {
        return Err("only a .wav can be replaced; save as a new clip instead".to_string());
    }
    let bytes = read_scoped(dir, name, MAX_AUDIO_BYTES)?;
    let wav = crate::decode::crop_wav(bytes, Some("wav"), start, end)?;
    // A non-audio name the folder scan skips while it's being written.
    let tmp = dir.join(format!(".{name}.crop-tmp"));
    if let Err(e) = std::fs::write(&tmp, &wav) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot write crop: {e}"));
    }
    let backup_dir = std::env::temp_dir().join(format!("lsdj-crop-{}", rand::random::<u64>()));
    let backup = backup_dir.join(name);
    let trashed = std::fs::create_dir_all(&backup_dir)
        .and_then(|()| std::fs::copy(&target, &backup))
        .map_err(|e| e.to_string())
        .and_then(|_| trash::delete(&backup).map_err(|e| e.to_string()));
    let _ = std::fs::remove_dir_all(&backup_dir);
    if let Err(e) = trashed {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot move the original to Trash: {e}"));
    }
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot swap the crop in: {e}")
    })
}

/// Resolve `name` to a regular file that is a DIRECT CHILD of `dir`, rejecting paths,
/// `..`, and symlinks that escape the folder. The read/delete boundary: `name` comes
/// from the webview, so without this a crafted name could reach any file the user
/// can.
pub fn scoped_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let mut comps = Path::new(name).components();
    if !matches!(
        (comps.next(), comps.next()),
        (Some(Component::Normal(_)), None)
    ) {
        return Err("invalid file name".to_string());
    }
    let base =
        std::fs::canonicalize(dir).map_err(|e| format!("cannot resolve library folder: {e}"))?;
    let target =
        std::fs::canonicalize(base.join(name)).map_err(|e| format!("cannot resolve file: {e}"))?;
    if target.parent() != Some(base.as_path()) {
        return Err("file is outside the library folder".to_string());
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn safe_stem_keeps_a_normal_take_name() {
        assert_eq!(safe_stem("late night dub #2", "song"), "late night dub #2");
    }

    #[test]
    fn safe_stem_strips_path_separators() {
        // No separator may survive, so the sanitised name is always ONE path
        // component — the write boundary for an untrusted webview title.
        let stem = safe_stem("a/b\\c", "song");
        assert_eq!(stem, "a-b-c");
        assert!(!stem.contains('/') && !stem.contains('\\'));
    }

    #[test]
    fn safe_stem_neutralises_traversal() {
        let stem = safe_stem("../../etc/passwd", "song");
        assert!(!stem.contains('/'), "separator survived: {stem}");
        assert!(!stem.contains(".."), "dot-dot survived: {stem}");
        assert_ne!(stem, "..");
    }

    #[test]
    fn safe_stem_falls_back_when_empty() {
        assert_eq!(safe_stem("   ", "song"), "song");
        assert_eq!(safe_stem("", "sample"), "sample");
    }

    #[test]
    fn safe_stem_caps_a_long_prompt_so_the_filename_fits() {
        // A pasted JSON / paragraph prompt must not produce an over-long filename
        // (the write would fail with ENAMETOOLONG); the registry keeps the full text.
        let stem = safe_stem(&"hyperpop ballad ".repeat(400), "song");
        assert!(stem.chars().count() <= MAX_STEM_CHARS, "stem too long: {stem}");
        assert!(!stem.is_empty());
    }

    #[test]
    fn unique_wav_path_suffixes_around_existing_takes() {
        let dir = Path::new("/lib");
        let taken: HashSet<PathBuf> = ["/lib/Take.wav", "/lib/Take (2).wav"]
            .iter()
            .map(PathBuf::from)
            .collect();
        let path = unique_wav_path(dir, "Take", |p| taken.contains(p)).unwrap();
        assert_eq!(path, dir.join("Take (3).wav"));
    }

    #[test]
    fn unique_wav_path_gives_up_rather_than_clobber() {
        // Every candidate "exists" → no free name → None, so `record` errors instead
        // of truncating an earlier take.
        assert!(unique_wav_path(Path::new("/lib"), "Take", |_| true).is_none());
    }

    #[test]
    fn create_unique_wav_opens_a_fresh_take_without_clobbering() {
        let dir = std::env::temp_dir().join(format!("lsdj-rec-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // First take gets the bare name; the file exists and is empty.
        let (file, first) = create_unique_wav(&dir, "Take").unwrap();
        drop(file);
        assert_eq!(first, dir.join("Take.wav"));
        assert!(first.exists());

        // Second take must not truncate the first — it suffixes the next number.
        std::fs::write(&first, b"already here").unwrap();
        let (_file, second) = create_unique_wav(&dir, "Take").unwrap();
        assert_eq!(second, dir.join("Take (2).wav"));
        assert_eq!(std::fs::read(&first).unwrap(), b"already here", "earlier take is untouched");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn validate_title_trims_and_refuses_bad_titles() {
        assert_eq!(validate_title("  Late Dub  ").unwrap(), "Late Dub");
        assert!(validate_title("").is_err());
        assert!(validate_title("   ").is_err());
        assert!(validate_title("a\nb").is_err());
        assert!(validate_title("a\u{7}b").is_err());
        assert!(validate_title(&"x".repeat(MAX_TITLE_CHARS)).is_ok());
        assert!(validate_title(&"x".repeat(MAX_TITLE_CHARS + 1)).is_err());
        // The cap counts characters, not bytes.
        assert!(validate_title(&"é".repeat(MAX_TITLE_CHARS)).is_ok());
    }

    #[test]
    fn normalise_tags_folds_case_hash_and_whitespace_and_dedupes() {
        let tags: Vec<String> = [
            "  Deep   House ",
            "#Banger",
            "banger",
            "",
            "  ",
            "#",
            "drone",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            normalise_tags(&tags).unwrap(),
            ["deep-house", "banger", "drone"]
        );
        // Only ONE leading `#` is dropped.
        assert_eq!(normalise_tags(&["##x".to_string()]).unwrap(), ["#x"]);
    }

    #[test]
    fn normalise_tags_refuses_control_chars_and_caps() {
        assert!(normalise_tags(&["a\tb".to_string()]).is_err());
        assert!(normalise_tags(&["x".repeat(MAX_TAG_CHARS)]).is_ok());
        assert!(normalise_tags(&["x".repeat(MAX_TAG_CHARS + 1)]).is_err());
        let many: Vec<String> = (0..=MAX_TAGS).map(|n| format!("t{n}")).collect();
        assert!(normalise_tags(&many).is_err());
        // Duplicates do not count toward the cap.
        let dupes: Vec<String> = (0..40).map(|_| "same".to_string()).collect();
        assert_eq!(normalise_tags(&dupes).unwrap(), ["same"]);
    }
}
