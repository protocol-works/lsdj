//! Strict native `.tar.gz` extraction for authenticated installer artifacts.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ArchiveLimits {
    pub(crate) max_files: u64,
    pub(crate) max_expanded_bytes: u64,
    /// Some signed runtime distributions contain convenience symlinks. When
    /// enabled, safe in-root file links are materialized as regular copies;
    /// no archive-controlled link is ever created on disk.
    pub(crate) materialize_safe_links: bool,
}

/// Extract a gzip-compressed tar whose every entry must live below one exact
/// top-level directory. Links and non-file/directory entries are never created.
/// The caller supplies limits from the authenticated app manifest.
#[cfg(test)]
pub(crate) fn extract_tar_gz(
    source: impl Read,
    destination: &Path,
    expected_root: &str,
    limits: ArchiveLimits,
) -> Result<(), String> {
    extract_tar_gz_cancellable(source, destination, expected_root, limits, &|| false)
}

/// Cancellation-aware production entry point. The cancellation reader sits
/// below gzip so it also interrupts decoder reads of archive metadata, while
/// file/link copies check between bounded chunks.
pub(crate) fn extract_tar_gz_cancellable(
    source: impl Read,
    destination: &Path,
    expected_root: &str,
    limits: ArchiveLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    validate_root(expected_root)?;
    if limits.max_files == 0 || limits.max_expanded_bytes == 0 {
        return Err("archive limits must be non-zero".into());
    }
    prepare_empty_destination(destination)?;

    let metadata_allowance = limits
        .max_files
        .checked_mul(4096)
        .and_then(|bytes| bytes.checked_add(1024 * 1024))
        .ok_or("archive decompression limit overflow")?;
    let decompressed_limit = limits
        .max_expanded_bytes
        .checked_add(metadata_allowance)
        .ok_or("archive decompression limit overflow")?;
    let source = CancellationReader {
        inner: source,
        is_cancelled,
    };
    let decoder = GzDecoder::new(source);
    // This counts every decompressed tar byte, including GNU long-name and
    // local PAX records that the tar crate consumes before yielding an entry.
    let decoder = BudgetReader::new(decoder, decompressed_limit);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("cannot read archive: {error}"))?;
    let mut count = 0u64;
    let mut materialized_count = 0u64;
    let mut expanded = 0u64;
    let mut seen = HashSet::new();
    let mut materialized = HashSet::new();
    let mut deferred_links = Vec::new();

    for entry in entries {
        if is_cancelled() {
            return Err("cancelled".into());
        }
        let mut entry = entry.map_err(|error| format!("cannot read archive entry: {error}"))?;
        count = count.checked_add(1).ok_or("archive file count overflow")?;
        if count > limits.max_files {
            return Err(format!(
                "archive contains more than {} entries",
                limits.max_files
            ));
        }

        let entry_type = entry.header().entry_type();
        if entry_type.is_pax_global_extensions() {
            // `tar` applies local PAX/GNU path extensions to the following
            // entry before yielding it. Global headers are yielded separately;
            // they contain metadata only, never filesystem content. Count and
            // bound them, then discard them without trusting path-like keys.
            let size = entry
                .header()
                .size()
                .map_err(|error| format!("archive metadata has invalid size: {error}"))?;
            expanded = expanded
                .checked_add(size)
                .ok_or("archive expanded size overflow")?;
            if expanded > limits.max_expanded_bytes {
                return Err(format!(
                    "archive expands beyond {} bytes",
                    limits.max_expanded_bytes
                ));
            }
            copy_cancellable(&mut entry, &mut io::sink(), is_cancelled)
                .map_err(|error| format!("cannot read archive metadata: {error}"))?;
            continue;
        }
        let is_link = entry_type.is_symlink() || entry_type.is_hard_link();
        if is_link && !limits.materialize_safe_links {
            return Err("archive links are not permitted".into());
        }
        if !(entry_type.is_file() || entry_type.is_dir() || is_link) {
            return Err(format!(
                "archive contains unsupported special entry type {:?}",
                entry_type
            ));
        }

        let relative = checked_relative_path(&entry, expected_root)?;
        // Windows/macOS default filesystems are case-insensitive. Reject an
        // archive that is only unambiguous on a case-sensitive extraction host.
        let key = relative.to_string_lossy().to_lowercase();
        if !seen.insert(key) {
            return Err("archive contains a duplicate path".into());
        }
        for ancestor in relative
            .ancestors()
            .filter(|path| !path.as_os_str().is_empty())
        {
            let key = ancestor.to_string_lossy().to_lowercase();
            if materialized.insert(key) {
                materialized_count = materialized_count
                    .checked_add(1)
                    .ok_or("archive materialized file count overflow")?;
                if materialized_count > limits.max_files {
                    return Err(format!(
                        "archive materializes more than {} filesystem entries",
                        limits.max_files
                    ));
                }
            }
        }
        let output = destination.join(&relative);

        if is_link {
            let target =
                checked_link_target(&entry, &relative, expected_root, entry_type.is_hard_link())?;
            deferred_links.push((output, destination.join(target)));
            continue;
        }

        if entry_type.is_dir() {
            fs::create_dir_all(&output)
                .map_err(|error| format!("cannot create archive directory: {error}"))?;
            set_safe_permissions(&output, true, false)?;
            continue;
        }
        if relative.as_os_str().is_empty() {
            return Err("archive root must be a directory".into());
        }

        let size = entry
            .header()
            .size()
            .map_err(|error| format!("archive entry has invalid size: {error}"))?;
        expanded = expanded
            .checked_add(size)
            .ok_or("archive expanded size overflow")?;
        if expanded > limits.max_expanded_bytes {
            return Err(format!(
                "archive expands beyond {} bytes",
                limits.max_expanded_bytes
            ));
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create archive parent: {error}"))?;
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .map_err(|error| format!("cannot create archive file: {error}"))?;
        let copied = copy_cancellable(&mut entry, &mut file, is_cancelled)
            .map_err(|error| format!("cannot extract archive file: {error}"))?;
        if copied != size {
            return Err("archive entry ended before its declared size".into());
        }
        let executable = entry.header().mode().unwrap_or(0) & 0o111 != 0;
        set_safe_permissions(&output, false, executable)?;
    }

    if count == 0 {
        return Err("archive is empty".into());
    }
    for (output, target) in deferred_links {
        if is_cancelled() {
            return Err("cancelled".into());
        }
        let metadata = fs::metadata(&target)
            .map_err(|_| "archive link target is missing or not a regular file".to_string())?;
        if !metadata.is_file() {
            return Err("archive link target is not a regular file".into());
        }
        expanded = expanded
            .checked_add(metadata.len())
            .ok_or("archive expanded size overflow")?;
        if expanded > limits.max_expanded_bytes {
            return Err(format!(
                "archive expands beyond {} bytes",
                limits.max_expanded_bytes
            ));
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create archive link parent: {error}"))?;
        }
        let mut source = File::open(&target)
            .map_err(|error| format!("cannot open safe archive link target: {error}"))?;
        let mut destination = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .map_err(|error| format!("cannot create materialized archive link: {error}"))?;
        copy_cancellable(&mut source, &mut destination, is_cancelled)
            .map_err(|error| format!("cannot materialize safe archive link: {error}"))?;
        set_safe_permissions(&output, false, is_executable(&target))?;
    }
    Ok(())
}

/// Extract a ZIP archive under the same portable path, count, and expanded
/// byte policy as [`extract_tar_gz_cancellable`]. Official uv Windows builds
/// use ZIP; archive-provided links and special files remain forbidden.
pub(crate) fn extract_zip_cancellable(
    source: impl Read + Seek,
    destination: &Path,
    expected_root: &str,
    limits: ArchiveLimits,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    validate_root(expected_root)?;
    if limits.max_files == 0 || limits.max_expanded_bytes == 0 {
        return Err("archive limits must be non-zero".into());
    }
    prepare_empty_destination(destination)?;
    let mut archive = zip::ZipArchive::new(source)
        .map_err(|error| format!("cannot read ZIP archive: {error}"))?;
    if archive.is_empty() {
        return Err("archive is empty".into());
    }
    if archive.len() as u64 > limits.max_files {
        return Err(format!(
            "archive contains more than {} entries",
            limits.max_files
        ));
    }

    let mut expanded = 0u64;
    let mut seen = HashSet::new();
    let mut materialized = HashSet::new();
    let mut materialized_count = 0u64;
    for index in 0..archive.len() {
        if is_cancelled() {
            return Err("cancelled".into());
        }
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("cannot read ZIP entry: {error}"))?;
        let raw = entry.name_raw();
        if raw.is_empty()
            || raw[0] == b'/'
            || raw[0] == b'\\'
            || raw.iter().any(|byte| *byte == b'\\' || *byte == 0)
        {
            return Err("archive contains an absolute or platform-ambiguous path".into());
        }
        let text =
            std::str::from_utf8(raw).map_err(|_| "archive path is not valid UTF-8".to_string())?;
        let relative = checked_text_relative_path(text, expected_root)?;
        let key = relative.to_string_lossy().to_lowercase();
        if !seen.insert(key) {
            return Err("archive contains a duplicate path".into());
        }
        for ancestor in relative
            .ancestors()
            .filter(|path| !path.as_os_str().is_empty())
        {
            let key = ancestor.to_string_lossy().to_lowercase();
            if materialized.insert(key) {
                materialized_count = materialized_count
                    .checked_add(1)
                    .ok_or("archive materialized file count overflow")?;
                if materialized_count > limits.max_files {
                    return Err(format!(
                        "archive materializes more than {} filesystem entries",
                        limits.max_files
                    ));
                }
            }
        }

        let unix_kind = entry.unix_mode().unwrap_or(0) & 0o170000;
        if unix_kind == 0o120000 || !(entry.is_file() || entry.is_dir()) {
            return Err("archive contains a link or unsupported special entry".into());
        }
        let output = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)
                .map_err(|error| format!("cannot create archive directory: {error}"))?;
            set_safe_permissions(&output, true, false)?;
            continue;
        }
        if relative.as_os_str().is_empty() {
            return Err("archive root must be a directory".into());
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or("archive expanded size overflow")?;
        if expanded > limits.max_expanded_bytes {
            return Err(format!(
                "archive expands beyond {} bytes",
                limits.max_expanded_bytes
            ));
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create archive parent: {error}"))?;
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .map_err(|error| format!("cannot create archive file: {error}"))?;
        let copied = copy_cancellable(&mut entry, &mut file, is_cancelled)
            .map_err(|error| format!("cannot extract archive file: {error}"))?;
        if copied != entry.size() {
            return Err("archive entry ended before its declared size".into());
        }
        let executable = entry.unix_mode().unwrap_or(0) & 0o111 != 0;
        set_safe_permissions(&output, false, executable)?;
    }
    Ok(())
}

struct CancellationReader<'a, R> {
    inner: R,
    is_cancelled: &'a dyn Fn() -> bool,
}

impl<R: Read> Read for CancellationReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if (self.is_cancelled)() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        self.inner.read(buffer)
    }
}

struct BudgetReader<R> {
    inner: R,
    remaining: u64,
}

impl<R> BudgetReader<R> {
    fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<R: Read> Read for BudgetReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive decompressed data exceeds its budget",
                )),
            };
        }
        let allowed = usize::try_from(self.remaining.min(buffer.len() as u64))
            .expect("bounded by the buffer length");
        let count = self.inner.read(&mut buffer[..allowed])?;
        self.remaining = self.remaining.saturating_sub(count as u64);
        Ok(count)
    }
}

fn copy_cancellable(
    reader: &mut impl Read,
    writer: &mut impl Write,
    is_cancelled: &dyn Fn() -> bool,
) -> io::Result<u64> {
    let mut total = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(total);
        }
        writer.write_all(&buffer[..count])?;
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("archive copy byte count overflow"))?;
    }
}

fn validate_root(root: &str) -> Result<(), String> {
    if root.is_empty()
        || root == "."
        || root == ".."
        || root.contains('/')
        || root.contains('\\')
        || root.contains(':')
    {
        return Err("archive root is unsafe".into());
    }
    Ok(())
}

fn checked_relative_path<R: Read>(
    entry: &tar::Entry<'_, R>,
    expected_root: &str,
) -> Result<PathBuf, String> {
    let raw = entry.path_bytes();
    if raw.is_empty()
        || raw[0] == b'/'
        || raw[0] == b'\\'
        || raw.iter().any(|byte| *byte == b'\\' || *byte == 0)
    {
        return Err("archive contains an absolute or platform-ambiguous path".into());
    }
    let text =
        std::str::from_utf8(&raw).map_err(|_| "archive path is not valid UTF-8".to_string())?;
    checked_text_relative_path(text, expected_root)
}

fn checked_text_relative_path(text: &str, expected_root: &str) -> Result<PathBuf, String> {
    if text.len() > 4096 {
        return Err("archive path exceeds the portable length limit".into());
    }
    let mut segments: Vec<&str> = text.split('/').collect();
    if segments.last() == Some(&"") {
        segments.pop();
    }
    if segments.is_empty() || segments[0] != expected_root {
        return Err("archive path escapes or does not match its pinned root".into());
    }
    if segments.len() > 129 {
        return Err("archive path exceeds the nesting limit".into());
    }
    for segment in &segments {
        validate_portable_segment(segment)?;
    }
    let mut relative = PathBuf::new();
    for segment in &segments[1..] {
        relative.push(segment);
    }
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("archive path is not a safe relative path".into());
    }
    Ok(relative)
}

fn validate_portable_segment(segment: &str) -> Result<(), String> {
    if segment.is_empty()
        || segment == "."
        || segment == ".."
        || segment.ends_with(' ')
        || segment.ends_with('.')
        || segment.chars().any(|character| {
            character <= '\u{1f}' || matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*')
        })
    {
        return Err("archive path is not portable to Windows".into());
    }
    let stem = segment
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| number.len() == 1 && matches!(number.as_bytes()[0], b'1'..=b'9'));
    if reserved {
        return Err("archive path uses a reserved Windows device name".into());
    }
    Ok(())
}

fn checked_link_target<R: Read>(
    entry: &tar::Entry<'_, R>,
    link_path: &Path,
    expected_root: &str,
    hard_link: bool,
) -> Result<PathBuf, String> {
    let target = entry
        .link_name()
        .map_err(|error| format!("archive link target is invalid: {error}"))?
        .ok_or("archive link has no target")?;
    let target = target
        .to_str()
        .ok_or("archive link target is not valid UTF-8")?;
    if target.is_empty()
        || target.starts_with('/')
        || target.starts_with('\\')
        || target.contains('\\')
    {
        return Err("archive link target is absolute or platform-ambiguous".into());
    }

    let mut resolved = if hard_link {
        PathBuf::new()
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_path_buf()
    };
    let mut segments = target.split('/').peekable();
    if hard_link && segments.peek() == Some(&expected_root) {
        segments.next();
    }
    for segment in segments {
        match segment {
            "" | "." => {}
            ".." => {
                if !resolved.pop() {
                    return Err("archive link target escapes its pinned root".into());
                }
            }
            normal => {
                validate_portable_segment(normal)?;
                resolved.push(normal);
            }
        }
    }
    if resolved.as_os_str().is_empty() {
        return Err("archive link target does not name a file".into());
    }
    Ok(resolved)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

fn prepare_empty_destination(destination: &Path) -> Result<(), String> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("archive destination is not a real directory".into());
            }
            let mut entries = fs::read_dir(destination)
                .map_err(|error| format!("cannot inspect archive destination: {error}"))?;
            if entries.next().is_some() {
                return Err("archive destination must be empty".into());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(destination)
                .map_err(|error| format!("cannot create archive destination: {error}"))?;
        }
        Err(error) => {
            return Err(format!("cannot inspect archive destination: {error}"));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_safe_permissions(path: &Path, directory: bool, executable: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if directory || executable {
        0o755
    } else {
        0o644
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot set archive entry permissions: {error}"))
}

#[cfg(not(unix))]
fn set_safe_permissions(_path: &Path, _directory: bool, _executable: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Cursor;

    fn fixture(entries: &[(&str, tar::EntryType, &[u8], Option<&str>)]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, kind, body, link) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(*kind);
            header.set_mode(if kind.is_file() { 0o644 } else { 0o755 });
            header.set_size(body.len() as u64);
            set_raw_name(&mut header, name);
            if let Some(link) = link {
                header.set_link_name(link).unwrap();
            }
            header.set_cksum();
            builder.append(&header, *body).unwrap();
        }
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap()
    }

    fn zip_fixture(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);
        for (name, body) in entries {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn set_raw_name(header: &mut tar::Header, name: &str) {
        assert!(name.len() < 100);
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
    }

    fn temp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lsdj-archive-{label}-{}", std::process::id()))
    }

    fn limits() -> ArchiveLimits {
        ArchiveLimits {
            max_files: 10,
            max_expanded_bytes: 1024,
            materialize_safe_links: false,
        }
    }

    #[test]
    fn extracts_regular_files_below_the_pinned_root() {
        let bytes = fixture(&[
            ("root/", tar::EntryType::Directory, b"", None),
            ("root/nested/", tar::EntryType::Directory, b"", None),
            (
                "root/nested/file.txt",
                tar::EntryType::Regular,
                b"safe",
                None,
            ),
        ]);
        let out = temp("valid");
        let _ = fs::remove_dir_all(&out);
        extract_tar_gz(&bytes[..], &out, "root", limits()).unwrap();
        assert_eq!(fs::read(out.join("nested/file.txt")).unwrap(), b"safe");
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn rejects_parent_absolute_and_windows_ambiguous_paths() {
        for (label, name) in [
            ("parent", "root/../../escape"),
            ("absolute", "/root/escape"),
            ("backslash", "root\\..\\escape"),
            ("drive", "root/C:/escape"),
        ] {
            let bytes = fixture(&[(name, tar::EntryType::Regular, b"bad", None)]);
            let out = temp(label);
            let escaped = out.parent().unwrap().join("escape");
            let _ = fs::remove_dir_all(&out);
            let _ = fs::remove_file(&escaped);
            assert!(extract_tar_gz(&bytes[..], &out, "root", limits()).is_err());
            assert!(!escaped.exists());
            let _ = fs::remove_dir_all(out);
        }
    }

    #[test]
    fn rejects_windows_reserved_invalid_and_case_colliding_paths() {
        for (label, name) in [
            ("con", "root/CON"),
            ("device-extension", "root/com1.txt"),
            ("lpt", "root/LPT9.log"),
            ("trailing-dot", "root/name."),
            ("trailing-space", "root/name "),
            ("invalid-char", "root/na<me"),
            ("control", "root/na\u{1f}me"),
        ] {
            let bytes = fixture(&[(name, tar::EntryType::Regular, b"bad", None)]);
            let out = temp(label);
            let _ = fs::remove_dir_all(&out);
            let error = extract_tar_gz(&bytes[..], &out, "root", limits()).unwrap_err();
            assert!(error.contains("Windows"), "{name}: {error}");
            let _ = fs::remove_dir_all(out);
        }

        let colliding = fixture(&[
            ("root/Model.npz", tar::EntryType::Regular, b"one", None),
            ("root/model.npz", tar::EntryType::Regular, b"two", None),
        ]);
        let out = temp("case-collision");
        let _ = fs::remove_dir_all(&out);
        assert!(extract_tar_gz(&colliding[..], &out, "root", limits())
            .unwrap_err()
            .contains("duplicate"));
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn rejects_symlinks_hardlinks_and_device_entries() {
        for (label, kind, link) in [
            ("symlink", tar::EntryType::Symlink, Some("../../escape")),
            ("hardlink", tar::EntryType::Link, Some("../../escape")),
            ("device", tar::EntryType::Char, None),
        ] {
            let bytes = fixture(&[("root/bad", kind, b"", link)]);
            let out = temp(label);
            let _ = fs::remove_dir_all(&out);
            let error = extract_tar_gz(&bytes[..], &out, "root", limits()).unwrap_err();
            assert!(
                error.contains("links") || error.contains("special"),
                "{error}"
            );
            let _ = fs::remove_dir_all(out);
        }
    }

    #[test]
    fn materializes_only_in_root_file_links_as_regular_files() {
        let bytes = fixture(&[
            ("root/target", tar::EntryType::Regular, b"safe", None),
            ("root/alias", tar::EntryType::Symlink, b"", Some("target")),
        ]);
        let out = temp("safe-link");
        let _ = fs::remove_dir_all(&out);
        let mut link_limits = limits();
        link_limits.materialize_safe_links = true;
        extract_tar_gz(&bytes[..], &out, "root", link_limits).unwrap();
        assert_eq!(fs::read(out.join("alias")).unwrap(), b"safe");
        assert!(!fs::symlink_metadata(out.join("alias"))
            .unwrap()
            .file_type()
            .is_symlink());
        let _ = fs::remove_dir_all(out);

        let escaping = fixture(&[(
            "root/link",
            tar::EntryType::Symlink,
            b"",
            Some("../../escape"),
        )]);
        let out = temp("escaping-link");
        let _ = fs::remove_dir_all(&out);
        assert!(extract_tar_gz(&escaping[..], &out, "root", link_limits)
            .unwrap_err()
            .contains("escapes"));
        let _ = fs::remove_dir_all(out);

        let escaping_hardlink =
            fixture(&[("root/link", tar::EntryType::Link, b"", Some("../escape"))]);
        let out = temp("escaping-hardlink");
        let _ = fs::remove_dir_all(&out);
        assert!(
            extract_tar_gz(&escaping_hardlink[..], &out, "root", link_limits)
                .unwrap_err()
                .contains("escapes")
        );
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn rejects_entry_count_and_expanded_size_bombs() {
        let many = fixture(&[
            ("root/a", tar::EntryType::Regular, b"a", None),
            ("root/b", tar::EntryType::Regular, b"b", None),
        ]);
        let out = temp("count");
        let _ = fs::remove_dir_all(&out);
        assert!(extract_tar_gz(
            &many[..],
            &out,
            "root",
            ArchiveLimits {
                max_files: 1,
                max_expanded_bytes: 1024,
                materialize_safe_links: false,
            },
        )
        .unwrap_err()
        .contains("entries"));
        let _ = fs::remove_dir_all(&out);

        let large = fixture(&[("root/large", tar::EntryType::Regular, b"12345", None)]);
        assert!(extract_tar_gz(
            &large[..],
            &out,
            "root",
            ArchiveLimits {
                max_files: 2,
                max_expanded_bytes: 4,
                materialize_safe_links: false,
            },
        )
        .unwrap_err()
        .contains("expands beyond"));
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn counts_implicit_directories_against_the_file_limit() {
        let nested = fixture(&[("root/a/b/c/file", tar::EntryType::Regular, b"safe", None)]);
        let out = temp("implicit-count");
        let _ = fs::remove_dir_all(&out);
        assert!(extract_tar_gz(
            &nested[..],
            &out,
            "root",
            ArchiveLimits {
                max_files: 3,
                max_expanded_bytes: 1024,
                materialize_safe_links: false,
            },
        )
        .unwrap_err()
        .contains("filesystem entries"));
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn accepts_bounded_global_pax_metadata_without_materializing_it() {
        let bytes = fixture(&[
            (
                "pax_global_header",
                tar::EntryType::XGlobalHeader,
                b"comment=fixture",
                None,
            ),
            ("root/file", tar::EntryType::Regular, b"safe", None),
        ]);
        let out = temp("pax-global");
        let _ = fs::remove_dir_all(&out);
        extract_tar_gz(&bytes[..], &out, "root", limits()).unwrap();
        assert_eq!(fs::read(out.join("file")).unwrap(), b"safe");
        assert!(!out.join("pax_global_header").exists());
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn cancellation_is_observed_during_decoder_and_entry_reads() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let bytes = fixture(&[(
            "root/large",
            tar::EntryType::Regular,
            &vec![7u8; 512 * 1024],
            None,
        )]);
        let out = temp("cancelled");
        let _ = fs::remove_dir_all(&out);
        let checks = AtomicUsize::new(0);
        let result = extract_tar_gz_cancellable(
            &bytes[..],
            &out,
            "root",
            ArchiveLimits {
                max_files: 10,
                max_expanded_bytes: 1024 * 1024,
                materialize_safe_links: false,
            },
            &|| checks.fetch_add(1, Ordering::AcqRel) >= 2,
        );
        assert!(
            result.unwrap_err().contains("cancelled"),
            "cancellation should remain identifiable"
        );
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn bounds_hidden_gnu_long_name_metadata_before_tar_materializes_it() {
        let encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(1);
        header.set_cksum();
        let long_name = format!("root/{}", "a".repeat(2 * 1024 * 1024));
        builder
            .append_data(&mut header, long_name, &b"x"[..])
            .unwrap();
        let encoder = builder.into_inner().unwrap();
        let bytes = encoder.finish().unwrap();

        let out = temp("hidden-metadata-budget");
        let _ = fs::remove_dir_all(&out);
        let error = extract_tar_gz(
            &bytes[..],
            &out,
            "root",
            ArchiveLimits {
                max_files: 1,
                max_expanded_bytes: 1,
                materialize_safe_links: false,
            },
        )
        .unwrap_err();
        assert!(
            error.contains("decompressed data exceeds"),
            "unexpected error: {error}"
        );
        let _ = fs::remove_dir_all(out);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symbolic_link_destination() {
        use std::os::unix::fs::symlink;

        let bytes = fixture(&[("root/file", tar::EntryType::Regular, b"safe", None)]);
        let root = temp("destination-link-root");
        let target = temp("destination-link-target");
        let out = root.join("out");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&target);
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&target).unwrap();
        symlink(&target, &out).unwrap();

        assert!(extract_tar_gz(&bytes[..], &out, "root", limits())
            .unwrap_err()
            .contains("real directory"));
        assert!(!target.join("file").exists());
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(target);
    }

    #[test]
    fn zip_extracts_one_pinned_root_with_portable_paths() {
        let bytes = zip_fixture(&[("uv-x86_64-pc-windows-msvc/uv.exe", b"fixture")]);
        let out = temp("zip-safe");
        let _ = fs::remove_dir_all(&out);
        extract_zip_cancellable(
            Cursor::new(bytes),
            &out,
            "uv-x86_64-pc-windows-msvc",
            limits(),
            &|| false,
        )
        .unwrap();
        assert_eq!(fs::read(out.join("uv.exe")).unwrap(), b"fixture");
        let _ = fs::remove_dir_all(out);
    }

    #[test]
    fn zip_rejects_traversal_wrong_roots_and_expansion_over_budget() {
        for (label, name, expected) in [
            ("zip-traversal", "root/../escape", "portable"),
            ("zip-wrong-root", "other/file", "pinned root"),
        ] {
            let bytes = zip_fixture(&[(name, b"fixture")]);
            let out = temp(label);
            let _ = fs::remove_dir_all(&out);
            let error =
                extract_zip_cancellable(Cursor::new(bytes), &out, "root", limits(), &|| false)
                    .unwrap_err();
            assert!(error.contains(expected), "unexpected error: {error}");
            let _ = fs::remove_dir_all(out);
        }

        let bytes = zip_fixture(&[("root/file", b"too large")]);
        let out = temp("zip-budget");
        let _ = fs::remove_dir_all(&out);
        let error = extract_zip_cancellable(
            Cursor::new(bytes),
            &out,
            "root",
            ArchiveLimits {
                max_files: 10,
                max_expanded_bytes: 1,
                materialize_safe_links: false,
            },
            &|| false,
        )
        .unwrap_err();
        assert!(error.contains("expands beyond"));
        let _ = fs::remove_dir_all(out);
    }
}
