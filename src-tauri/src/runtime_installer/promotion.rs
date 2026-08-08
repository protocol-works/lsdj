//! Recoverable same-filesystem promotion for validated installs.

use std::fs::{self, File};
use std::path::Path;

/// Resolve a promotion interrupted between renames. A ready current install
/// wins; an absent or invalid current install is replaced by the preserved one.
pub(crate) fn recover(
    home: &Path,
    backup: &Path,
    validate: impl Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let rejected = backup.with_extension("rejected");
    if rejected.exists() {
        fs::remove_dir_all(&rejected)
            .map_err(|error| format!("cannot clear rejected install: {error}"))?;
    }
    if !backup.exists() {
        return Ok(());
    }
    if !backup.is_dir() {
        return Err("installer backup exists but is not a directory".into());
    }
    let backup_parent = backup
        .parent()
        .ok_or("installer backup has no parent directory")?;
    let home_parent = home
        .parent()
        .ok_or("install home has no parent directory")?;
    ensure_same_filesystem(backup_parent, home_parent)?;
    if !home.exists() {
        validate(backup)
            .map_err(|error| format!("preserved install is not ready for recovery: {error}"))?;
        fs::rename(backup, home)
            .map_err(|error| format!("cannot restore interrupted install: {error}"))?;
        sync_parent(home);
        return Ok(());
    }
    match validate(home) {
        Ok(()) => {
            // The new install was promoted and the process stopped before cleanup.
            // It is ready, so the old backup is no longer part of rollback state.
            fs::remove_dir_all(backup)
                .map_err(|error| format!("cannot clear completed install backup: {error}"))?;
            return Ok(());
        }
        Err(error) if error == "cancelled" => return Err(error),
        Err(_) => {}
    }

    validate(backup)
        .map_err(|error| format!("preserved install is not ready for recovery: {error}"))?;

    fs::rename(home, &rejected)
        .map_err(|error| format!("cannot quarantine interrupted install: {error}"))?;
    if let Err(error) = fs::rename(backup, home) {
        let _ = fs::rename(&rejected, home);
        return Err(format!("cannot restore previous install: {error}"));
    }
    fs::remove_dir_all(rejected)
        .map_err(|error| format!("cannot clear rejected install after rollback: {error}"))?;
    sync_parent(home);
    Ok(())
}

/// Validate the candidate, preserve the current tree, rename the candidate into
/// place, and validate it once more before discarding the previous tree.
pub(crate) fn promote(
    candidate: &Path,
    home: &Path,
    backup: &Path,
    validate: impl Fn(&Path) -> Result<(), String> + Copy,
) -> Result<(), String> {
    if candidate == home || candidate == backup || home == backup {
        return Err("installer promotion paths must be distinct".into());
    }
    validate(candidate).map_err(|error| format!("candidate is not ready: {error}"))?;
    let candidate_parent = candidate
        .parent()
        .ok_or("candidate install has no parent directory")?;
    let home_parent = home
        .parent()
        .ok_or("install home has no parent directory")?;
    let backup_parent = backup
        .parent()
        .ok_or("installer backup has no parent directory")?;
    ensure_same_filesystem(candidate_parent, home_parent)?;
    ensure_same_filesystem(backup_parent, home_parent)?;
    recover(home, backup, validate)?;

    if home.exists() {
        fs::rename(home, backup)
            .map_err(|error| format!("cannot preserve previous install: {error}"))?;
        sync_parent(home);
    }
    if let Err(error) = fs::rename(candidate, home) {
        if backup.exists() && !home.exists() {
            let _ = fs::rename(backup, home);
        }
        return Err(format!("cannot promote candidate install: {error}"));
    }
    sync_parent(home);

    if let Err(error) = validate(home) {
        let _ = fs::rename(home, candidate);
        if backup.exists() {
            fs::rename(backup, home)
                .map_err(|restore| format!(
                    "promoted install failed validation ({error}); previous install could not be restored: {restore}"
                ))?;
        }
        sync_parent(home);
        return Err(format!("promoted install failed validation: {error}"));
    }

    // A crash before this cleanup is harmless: `recover` observes a ready home
    // on the next attempt and removes the stale backup.
    if backup.exists() {
        fs::remove_dir_all(backup)
            .map_err(|error| format!("cannot clear previous install after promotion: {error}"))?;
    }
    Ok(())
}

/// Promotion uses rename for the commit point, so all three trees must live on
/// one volume. The host path contract intentionally places staging beside the
/// asset root; this check also rejects a misconfigured cross-volume override.
pub(crate) fn ensure_same_filesystem(left: &Path, right: &Path) -> Result<(), String> {
    same_filesystem(left, right).and_then(|same| {
        if same {
            Ok(())
        } else {
            Err("installer staging and asset roots are on different filesystems".into())
        }
    })
}

#[cfg(unix)]
fn same_filesystem(left: &Path, right: &Path) -> Result<bool, String> {
    use std::os::unix::fs::MetadataExt;
    let left = fs::metadata(left)
        .map_err(|error| format!("cannot inspect installer staging filesystem: {error}"))?;
    let right = fs::metadata(right)
        .map_err(|error| format!("cannot inspect installer asset filesystem: {error}"))?;
    Ok(left.dev() == right.dev())
}

#[cfg(windows)]
fn same_filesystem(left: &Path, right: &Path) -> Result<bool, String> {
    use std::path::Component;
    let left = fs::canonicalize(left)
        .map_err(|error| format!("cannot resolve installer staging volume: {error}"))?;
    let right = fs::canonicalize(right)
        .map_err(|error| format!("cannot resolve installer asset volume: {error}"))?;
    let volume = |path: &Path| match path.components().next() {
        Some(Component::Prefix(prefix)) => {
            Some(prefix.as_os_str().to_string_lossy().to_lowercase())
        }
        _ => None,
    };
    Ok(volume(&left).is_some() && volume(&left) == volume(&right))
}

#[cfg(not(any(unix, windows)))]
fn same_filesystem(_left: &Path, _right: &Path) -> Result<bool, String> {
    Err("atomic installer promotion is unsupported on this platform".into())
}

fn sync_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("lsdj-promote-{label}-{}", std::process::id()))
    }

    fn install(path: &Path, value: &str, ready: bool) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("value"), value).unwrap();
        if ready {
            fs::write(path.join("ready"), b"").unwrap();
        }
    }

    fn validate(path: &Path) -> Result<(), String> {
        if path.join("ready").is_file() {
            Ok(())
        } else {
            Err("missing readiness marker".into())
        }
    }

    #[test]
    fn successful_promotion_replaces_ready_install_and_cleans_backup() {
        let root = root("success");
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let candidate = root.join("candidate");
        let backup = root.join("backup");
        install(&home, "old", true);
        install(&candidate, "new", true);
        promote(&candidate, &home, &backup, validate).unwrap();
        assert_eq!(fs::read_to_string(home.join("value")).unwrap(), "new");
        assert!(!candidate.exists());
        assert!(!backup.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn staging_and_assets_must_share_a_filesystem() {
        let root = root("filesystem");
        let _ = fs::remove_dir_all(&root);
        let left = root.join("left");
        let right = root.join("right");
        fs::create_dir_all(&left).unwrap();
        fs::create_dir_all(&right).unwrap();
        ensure_same_filesystem(&left, &right).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn post_rename_validation_failure_restores_previous_install() {
        let root = root("rollback");
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let candidate = root.join("candidate");
        let backup = root.join("backup");
        install(&home, "old", true);
        install(&candidate, "new", true);
        let validate_after_rename = |path: &Path| {
            if path == home {
                Err("simulated final validation failure".into())
            } else {
                validate(path)
            }
        };
        let error = promote(&candidate, &home, &backup, validate_after_rename).unwrap_err();
        assert!(error.contains("failed validation"));
        assert_eq!(fs::read_to_string(home.join("value")).unwrap(), "old");
        assert_eq!(fs::read_to_string(candidate.join("value")).unwrap(), "new");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn retry_recovers_each_interrupted_promotion_state() {
        let root = root("recover");
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let backup = root.join("backup");

        // Interrupted after moving the previous home aside.
        install(&backup, "old", true);
        recover(&home, &backup, validate).unwrap();
        assert_eq!(fs::read_to_string(home.join("value")).unwrap(), "old");

        // Interrupted after moving an invalid candidate into home.
        fs::rename(&home, &backup).unwrap();
        install(&home, "broken", false);
        recover(&home, &backup, validate).unwrap();
        assert_eq!(fs::read_to_string(home.join("value")).unwrap(), "old");

        // Interrupted after a ready candidate was promoted.
        fs::rename(&home, &backup).unwrap();
        install(&home, "new", true);
        recover(&home, &backup, validate).unwrap();
        assert_eq!(fs::read_to_string(home.join("value")).unwrap(), "new");
        assert!(!backup.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_never_restores_an_unvalidated_backup() {
        let root = root("invalid-backup");
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let backup = root.join("backup");
        install(&backup, "broken", false);

        let error = recover(&home, &backup, validate).unwrap_err();
        assert!(error.contains("not ready for recovery"));
        assert!(!home.exists());
        assert!(backup.exists());
        let _ = fs::remove_dir_all(root);
    }
}
