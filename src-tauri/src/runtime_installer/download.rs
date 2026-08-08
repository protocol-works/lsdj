//! Authenticated-transport download plus application-controlled SHA-256.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// One immutable artifact in the app-bundled trust manifest.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PinnedArtifact {
    pub(crate) url: String,
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

impl PinnedArtifact {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let url = reqwest::Url::parse(&self.url)
            .map_err(|error| format!("artifact URL is invalid: {error}"))?;
        if url.scheme() != "https" {
            return Err("artifact URL must use HTTPS".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("artifact URL must not contain credentials".into());
        }
        let digest = hex::decode(&self.sha256)
            .map_err(|_| "artifact SHA-256 is not hexadecimal".to_string())?;
        if digest.len() != 32 {
            return Err("artifact SHA-256 must contain exactly 32 bytes".into());
        }
        if self.size == 0 {
            return Err("artifact size must be non-zero".into());
        }
        Ok(())
    }
}

/// HTTP client used only from the installer's dedicated blocking worker.
/// Redirects may not downgrade authenticated transport.
pub(crate) fn client() -> Result<Client, String> {
    Client::builder()
        .user_agent(concat!("LSDJ/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .redirect(Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" {
                attempt.error("artifact redirect attempted to leave HTTPS")
            } else if attempt.previous().len() >= 10 {
                attempt.error("artifact redirect limit exceeded")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|error| format!("cannot create HTTPS client: {error}"))
}

/// Download to a sibling `.part`, checking the expected byte count and digest
/// while streaming. A previously verified destination is reused, which makes a
/// retry after interruption deterministic without trusting a partial file.
pub(crate) fn download_verified(
    client: &Client,
    artifact: &PinnedArtifact,
    destination: &Path,
    bearer_token: Option<&str>,
    is_cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    artifact.validate()?;
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("artifact destination must not be a symbolic link".into());
        }
        Ok(metadata) if metadata.is_file() => {
            if verify_file(destination, artifact).is_ok() {
                return Ok(());
            }
            fs::remove_file(destination)
                .map_err(|error| format!("cannot replace invalid cached artifact: {error}"))?;
        }
        Ok(_) => return Err("artifact destination exists but is not a file".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect artifact destination: {error}")),
    }

    let parent = destination
        .parent()
        .ok_or("artifact destination has no parent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create artifact staging directory: {error}"))?;
    let partial = partial_path(destination)?;
    if partial.exists() {
        fs::remove_file(&partial)
            .map_err(|error| format!("cannot discard interrupted artifact: {error}"))?;
    }

    let result = (|| {
        if is_cancelled() {
            return Err("cancelled".into());
        }
        let mut request = client.get(&artifact.url);
        if let Some(token) = bearer_token.filter(|token| !token.is_empty()) {
            request = request.bearer_auth(token);
        }
        let mut response = request
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("artifact download failed: {error}"))?;
        if response.url().scheme() != "https" {
            return Err("artifact response did not use HTTPS".into());
        }
        if let Some(length) = response.content_length() {
            if length > artifact.size {
                return Err(format!(
                    "artifact response exceeds pinned size ({} > {})",
                    length, artifact.size
                ));
            }
        }

        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&partial)
            .map_err(|error| format!("cannot create partial artifact: {error}"))?;
        copy_and_verify(&mut response, file, artifact, is_cancelled)?;
        fs::rename(&partial, destination)
            .map_err(|error| format!("cannot commit verified artifact: {error}"))?;
        sync_parent(parent);
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    result
}

fn copy_and_verify(
    reader: &mut impl Read,
    mut file: File,
    artifact: &PinnedArtifact,
    is_cancelled: impl Fn() -> bool,
) -> Result<(), String> {
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if is_cancelled() {
            return Err("cancelled".into());
        }
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("cannot read artifact response: {error}"))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or("artifact byte count overflow")?;
        if total > artifact.size {
            return Err(format!(
                "artifact exceeds pinned size (more than {})",
                artifact.size
            ));
        }
        hasher.update(&buffer[..count]);
        file.write_all(&buffer[..count])
            .map_err(|error| format!("cannot write partial artifact: {error}"))?;
    }
    if total != artifact.size {
        return Err(format!(
            "artifact size mismatch (expected {}, received {total})",
            artifact.size
        ));
    }
    let actual = hex::encode(hasher.finalize());
    if actual != artifact.sha256.to_ascii_lowercase() {
        return Err(format!(
            "artifact SHA-256 mismatch (expected {}, received {actual})",
            artifact.sha256
        ));
    }
    file.flush()
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot sync verified artifact: {error}"))?;
    Ok(())
}

pub(crate) fn verify_file(path: &Path, artifact: &PinnedArtifact) -> Result<(), String> {
    artifact.validate()?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("cannot inspect artifact: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("artifact must not be a symbolic link".into());
    }
    if !metadata.is_file() || metadata.len() != artifact.size {
        return Err("artifact size does not match the manifest".into());
    }
    let mut file = File::open(path).map_err(|error| format!("cannot open artifact: {error}"))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher).map_err(|error| format!("cannot hash artifact: {error}"))?;
    let actual = hex::encode(hasher.finalize());
    if actual != artifact.sha256.to_ascii_lowercase() {
        return Err("artifact SHA-256 does not match the manifest".into());
    }
    Ok(())
}

/// Place a verified staged blob into a candidate install without copying when
/// the filesystem supports hard links. The link is created by the application
/// only after the source hash is verified; archive-provided links are rejected.
pub(crate) fn link_or_copy_verified(
    staged: &Path,
    destination: &Path,
    artifact: &PinnedArtifact,
) -> Result<(), String> {
    verify_file(staged, artifact)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create model directory: {error}"))?;
    }
    if destination.exists() {
        fs::remove_file(destination)
            .map_err(|error| format!("cannot replace staged model: {error}"))?;
    }
    if fs::hard_link(staged, destination).is_err() {
        fs::copy(staged, destination)
            .map_err(|error| format!("cannot copy staged model: {error}"))?;
        verify_file(destination, artifact)?;
    }
    Ok(())
}

fn partial_path(destination: &Path) -> Result<PathBuf, String> {
    let name = destination
        .file_name()
        .ok_or("artifact destination has no file name")?;
    let mut partial_name = OsString::from(name);
    partial_name.push(".part");
    Ok(destination.with_file_name(partial_name))
}

fn sync_parent(parent: &Path) {
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(bytes: &[u8]) -> PinnedArtifact {
        PinnedArtifact {
            url: "https://example.test/artifact".into(),
            sha256: hex::encode(Sha256::digest(bytes)),
            size: bytes.len() as u64,
        }
    }

    #[test]
    fn rejects_non_https_credentials_and_malformed_hashes() {
        let mut pin = artifact(b"ok");
        pin.url = "http://example.test/file".into();
        assert!(pin.validate().unwrap_err().contains("HTTPS"));
        pin.url = "https://user:secret@example.test/file".into();
        assert!(pin.validate().unwrap_err().contains("credentials"));
        pin.url = "https://example.test/file".into();
        pin.sha256 = "00".into();
        assert!(pin.validate().unwrap_err().contains("32 bytes"));
    }

    #[test]
    fn copy_is_bounded_and_hash_verified() {
        let tmp = std::env::temp_dir().join(format!(
            "lsdj-download-copy-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let pin = artifact(b"verified bytes");
        let out = tmp.join("artifact");
        let file = File::create(&out).unwrap();
        copy_and_verify(&mut &b"verified bytes"[..], file, &pin, || false).unwrap();
        verify_file(&out, &pin).unwrap();

        let too_long = PinnedArtifact {
            size: 3,
            ..artifact(b"bad")
        };
        let file = File::create(tmp.join("too-long")).unwrap();
        assert!(
            copy_and_verify(&mut &b"four"[..], file, &too_long, || false)
                .unwrap_err()
                .contains("exceeds")
        );

        let bad_hash = PinnedArtifact {
            sha256: "00".repeat(32),
            ..artifact(b"same size")
        };
        let file = File::create(tmp.join("bad-hash")).unwrap();
        assert!(
            copy_and_verify(&mut &b"same size"[..], file, &bad_hash, || false)
                .unwrap_err()
                .contains("SHA-256 mismatch")
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cancellation_stops_before_more_bytes_are_written() {
        let pin = artifact(b"will not be copied");
        let tmp = std::env::temp_dir().join(format!("lsdj-download-cancel-{}", std::process::id()));
        let file = File::create(&tmp).unwrap();
        assert_eq!(
            copy_and_verify(&mut &b"will not be copied"[..], file, &pin, || true),
            Err("cancelled".into())
        );
        assert_eq!(fs::metadata(&tmp).unwrap().len(), 0);
        let _ = fs::remove_file(tmp);
    }

    #[cfg(unix)]
    #[test]
    fn cached_artifacts_must_not_be_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("lsdj-download-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target");
        let cached = root.join("cached");
        fs::write(&target, b"verified bytes").unwrap();
        symlink(&target, &cached).unwrap();

        assert!(verify_file(&cached, &artifact(b"verified bytes"))
            .unwrap_err()
            .contains("symbolic link"));
        let _ = fs::remove_dir_all(root);
    }
}
