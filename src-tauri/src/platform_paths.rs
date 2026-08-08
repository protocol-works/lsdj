//! Host-owned cross-platform filesystem contract (issue #107).
//!
//! Rust resolves every application root once, before any Python process starts.
//! Children inherit the explicit `LSDJ_*_HOME` variables installed by
//! [`configure`]; Python must consume those values rather than reconstructing an
//! operating-system path from `$HOME`.
//!
//! The roots have deliberately different ownership:
//! - `config`: small durable settings and credentials.
//! - `data`: user-created songs, samples, and registries.
//! - `cache`: disposable, reproducible files.
//! - `assets`: downloaded model weights and runtimes.
//! - `staging`: incomplete downloads/installs. It is on the same filesystem as
//!   `assets`, so a validated future installer can promote atomically.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tauri::Manager;

const APP_ID: &str = "works.protocol.lsdj";
const APP_NAME: &str = "LSDJ";
const APP_SLUG: &str = "lsdj";

/// The operating-system families whose path policy differs. Kept independent
/// of `cfg!` so every mapping is unit-tested on every CI host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // non-host variants are exercised by portable unit tests
enum Platform {
    MacOs,
    Windows,
    Linux,
}

/// Platform-native unscoped directories. Production gets these from Tauri's
/// directory resolver (known-folder APIs on Windows, XDG on Linux); tests feed
/// synthetic roots so spaces and non-ASCII profiles are covered everywhere.
#[derive(Clone, Debug)]
struct NativeDirs {
    home: PathBuf,
    config: PathBuf,
    data: PathBuf,
    local_data: PathBuf,
    cache: PathBuf,
    documents: PathBuf,
}

/// The canonical application roots plus the actual backend asset locations.
/// The latter can temporarily point at a legacy macOS directory if an old
/// install cannot be renamed (for example because of permissions), ensuring a
/// migration failure never makes existing models disappear.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    config: PathBuf,
    data: PathBuf,
    cache: PathBuf,
    assets: PathBuf,
    staging: PathBuf,
    magenta_base: PathBuf,
    sa3_home: PathBuf,
    loras_home: PathBuf,
    legacy_data: Option<PathBuf>,
}

impl AppPaths {
    pub fn config(&self) -> &Path {
        &self.config
    }

    pub fn data(&self) -> &Path {
        &self.data
    }

    pub fn cache(&self) -> &Path {
        &self.cache
    }

    pub fn assets(&self) -> &Path {
        &self.assets
    }

    pub fn staging(&self) -> &Path {
        &self.staging
    }

    pub fn magenta_base(&self) -> &Path {
        &self.magenta_base
    }

    pub fn sa3_home(&self) -> &Path {
        &self.sa3_home
    }

    pub fn loras_home(&self) -> &Path {
        &self.loras_home
    }

    /// The old macOS Documents brand root, used only to migrate generated
    /// libraries independently when a destination already contains other data.
    pub fn legacy_data(&self) -> Option<&Path> {
        self.legacy_data.as_deref()
    }

    fn backend_env(&self) -> [(OsString, OsString); 8] {
        [
            pair("LSDJ_CONFIG_HOME", &self.config),
            pair("LSDJ_DATA_HOME", &self.data),
            pair("LSDJ_CACHE_HOME", &self.cache),
            pair("LSDJ_ASSETS_HOME", &self.assets),
            pair("LSDJ_STAGING_HOME", &self.staging),
            // Compatibility variables consumed by upstream magenta-rt-v2 and
            // the current SA3 entry points. Rust remains their source of truth.
            pair("MAGENTA_HOME", &self.magenta_base),
            pair("SA3_MLX_HOME", &self.sa3_home),
            pair("SA3_LORAS_HOME", &self.loras_home),
        ]
    }
}

fn pair(name: &str, value: &Path) -> (OsString, OsString) {
    (OsString::from(name), value.as_os_str().to_owned())
}

fn platform() -> Platform {
    #[cfg(target_os = "macos")]
    return Platform::MacOs;
    #[cfg(target_os = "windows")]
    return Platform::Windows;
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    return Platform::Linux;
}

fn resolve(platform: Platform, native: NativeDirs) -> AppPaths {
    let (config, data, cache, assets, staging, legacy_data) = match platform {
        Platform::MacOs => {
            // Preserve all current user-visible locations: generated media stays
            // in Documents/LSDJ, model assets stay in Application Support/LSDJ,
            // and shell settings remain under Tauri's bundle identifier.
            let assets = native.data.join(APP_NAME);
            (
                native.config.join(APP_ID),
                native.documents.join(APP_NAME),
                native.cache.join(APP_ID),
                assets.clone(),
                assets.join(".staging"),
                Some(native.documents.join("LSDJai")),
            )
        }
        Platform::Windows => {
            // Everything is non-roaming and deliberately shallow. `LSDJ`
            // replaces the reverse-DNS identifier to preserve path budget for
            // Python environments and model filenames when long paths are off.
            let base = native.local_data.join(APP_NAME);
            (
                base.join("config"),
                base.join("data"),
                base.join("cache"),
                base.join("assets"),
                base.join("staging"),
                None,
            )
        }
        Platform::Linux => {
            // Tauri's native bases follow XDG_CONFIG_HOME, XDG_DATA_HOME, and
            // XDG_CACHE_HOME, falling back respectively to ~/.config,
            // ~/.local/share, and ~/.cache when variables are absent/invalid.
            let data = native.data.join(APP_SLUG);
            (
                native.config.join(APP_SLUG),
                data.clone(),
                native.cache.join(APP_SLUG),
                data.join("assets"),
                data.join("staging"),
                None,
            )
        }
    };
    AppPaths {
        magenta_base: assets.clone(),
        sa3_home: assets.join("stable-audio-3"),
        loras_home: assets.join("sa3-loras"),
        config,
        data,
        cache,
        assets,
        staging,
        legacy_data,
    }
}

fn native_dirs(
    app: &tauri::App,
    platform: Platform,
) -> Result<NativeDirs, Box<dyn std::error::Error>> {
    let paths = app.path();
    match platform {
        Platform::MacOs => {
            let data = paths.data_dir()?;
            Ok(NativeDirs {
                home: paths.home_dir().unwrap_or_else(|_| data.clone()),
                config: paths.config_dir()?,
                local_data: data.clone(),
                cache: paths.cache_dir()?,
                documents: paths.document_dir().unwrap_or_else(|_| data.clone()),
                data,
            })
        }
        Platform::Windows => {
            let local_data = paths.local_data_dir()?;
            Ok(NativeDirs {
                home: local_data.clone(),
                config: local_data.clone(),
                data: local_data.clone(),
                cache: local_data.clone(),
                documents: local_data.clone(),
                local_data,
            })
        }
        Platform::Linux => {
            let data = paths.data_dir()?;
            Ok(NativeDirs {
                home: data.clone(),
                config: paths.config_dir()?,
                local_data: data.clone(),
                cache: paths.cache_dir()?,
                documents: data.clone(),
                data,
            })
        }
    }
}

static CONFIGURED: OnceLock<AppPaths> = OnceLock::new();

/// Resolve, prepare, and publish the path contract. This must be called in
/// Tauri setup before starting any sidecar, server, watcher, or installer.
pub fn configure(app: &tauri::App) -> Result<&'static AppPaths, Box<dyn std::error::Error>> {
    if let Some(paths) = CONFIGURED.get() {
        return Ok(paths);
    }
    let platform = platform();
    let native = native_dirs(app, platform)?;
    let mut paths = resolve(platform, native.clone());
    prepare_roots(&paths)?;
    if platform == Platform::MacOs {
        migrate_macos(&mut paths, &native);
    }

    // Explicit dev/user overrides remain supported, but the resolved value is
    // captured into the host contract and then passed to every child.
    if let Some(value) = nonempty_env("MAGENTA_HOME") {
        paths.magenta_base = PathBuf::from(value);
    }
    if let Some(value) = nonempty_env("SA3_MLX_HOME") {
        paths.sa3_home = PathBuf::from(value);
    }
    if let Some(value) = nonempty_env("SA3_LORAS_HOME") {
        paths.loras_home = PathBuf::from(value);
    }

    for (name, value) in paths.backend_env() {
        std::env::set_var(name, value);
    }
    CONFIGURED
        .set(paths)
        .map_err(|_| io::Error::new(io::ErrorKind::AlreadyExists, "paths already configured"))?;
    Ok(CONFIGURED.get().expect("paths were just configured"))
}

fn nonempty_env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn prepare_roots(paths: &AppPaths) -> io::Result<()> {
    for dir in [
        paths.config(),
        paths.data(),
        paths.cache(),
        paths.assets(),
        paths.staging(),
    ] {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// Return the configured contract. Calling this before [`configure`] is a
/// programming error: doing so would reintroduce independent path guessing.
pub fn get() -> &'static AppPaths {
    CONFIGURED
        .get()
        .expect("platform paths must be configured before services start")
}

/// Resolve the interpreter in a virtual environment without assuming Unix's
/// `bin/python` layout on Windows.
pub fn venv_python(venv: &Path) -> PathBuf {
    venv_python_for(platform(), venv)
}

fn venv_python_for(platform: Platform, venv: &Path) -> PathBuf {
    match platform {
        Platform::Windows => venv.join("Scripts").join("python.exe"),
        Platform::MacOs | Platform::Linux => venv.join("bin").join("python"),
    }
}

/// Move one legacy directory only when its destination is absent. `rename` is
/// atomic on the same filesystem; after success, a restart observes the
/// destination and does nothing. On failure the caller receives the old path,
/// keeping existing user data visible.
pub fn migrate_legacy_dir(new_dir: &Path, old_dir: &Path) -> PathBuf {
    if new_dir.exists() || !old_dir.is_dir() {
        return new_dir.to_path_buf();
    }
    if let Some(parent) = new_dir.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!(
                "lsdj-app: could not prepare data directory {}: {error}",
                parent.display()
            );
            return old_dir.to_path_buf();
        }
    }
    match std::fs::rename(old_dir, new_dir) {
        Ok(()) => {
            eprintln!("lsdj-app: migrated data → {}", new_dir.display());
            new_dir.to_path_buf()
        }
        Err(error) => {
            eprintln!(
                "lsdj-app: could not migrate {} to {}: {error}",
                old_dir.display(),
                new_dir.display()
            );
            old_dir.to_path_buf()
        }
    }
}

fn migrate_macos(paths: &mut AppPaths, native: &NativeDirs) {
    let old_brand = native.data.join("LSDJai");
    let new_magenta = paths.assets.join("magenta-rt-v2");
    let branded_magenta = migrate_legacy_dir(&new_magenta, &old_brand.join("magenta-rt-v2"));
    let branded_sa3 = migrate_legacy_dir(
        &paths.assets.join("stable-audio-3"),
        &old_brand.join("stable-audio-3"),
    );
    let branded_loras = migrate_legacy_dir(
        &paths.assets.join("sa3-loras"),
        &old_brand.join("sa3-loras"),
    );

    paths.sa3_home = branded_sa3;
    paths.loras_home = branded_loras;
    if branded_magenta.starts_with(&old_brand) {
        paths.magenta_base = old_brand;
        return;
    }

    // Pre-model-manager installs lived under Documents/Magenta. Preserve the
    // old location if a same-volume rename cannot complete.
    let old_magenta_base = native.home.join("Documents").join("Magenta");
    let migrated = migrate_legacy_dir(&new_magenta, &old_magenta_base.join("magenta-rt-v2"));
    paths.magenta_base = migrated
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| paths.assets.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(profile: &str) -> NativeDirs {
        let home = PathBuf::from(profile);
        NativeDirs {
            config: home.join("config base"),
            data: home.join("data base"),
            local_data: home.join("local data"),
            cache: home.join("cache base"),
            documents: home.join("Documents"),
            home,
        }
    }

    #[test]
    fn macos_preserves_existing_user_visible_locations() {
        let roots = resolve(Platform::MacOs, native("/Users/DJ Name"));
        assert_eq!(roots.data, Path::new("/Users/DJ Name/Documents/LSDJ"));
        assert_eq!(
            roots.assets,
            Path::new("/Users/DJ Name/data base/LSDJ")
        );
        assert_eq!(
            roots.config,
            Path::new("/Users/DJ Name/config base/works.protocol.lsdj")
        );
    }

    #[test]
    fn windows_roots_are_non_roaming_shallow_and_unicode_safe() {
        let roots = resolve(Platform::Windows, native(r"C:\Users\Zoë 王"));
        let base = Path::new(r"C:\Users\Zoë 王").join("local data").join("LSDJ");
        assert_eq!(roots.config, base.join("config"));
        assert_eq!(roots.data, base.join("data"));
        assert_eq!(roots.cache, base.join("cache"));
        assert_eq!(roots.assets, base.join("assets"));
        assert_eq!(roots.staging, base.join("staging"));
        assert!(!roots.config.to_string_lossy().contains(APP_ID));
    }

    #[test]
    fn linux_roots_follow_xdg_native_bases_and_keep_staging_with_assets() {
        let roots = resolve(Platform::Linux, native("/home/DJ 名"));
        assert_eq!(roots.config, Path::new("/home/DJ 名/config base/lsdj"));
        assert_eq!(roots.data, Path::new("/home/DJ 名/data base/lsdj"));
        assert_eq!(roots.cache, Path::new("/home/DJ 名/cache base/lsdj"));
        assert_eq!(roots.assets, roots.data.join("assets"));
        assert_eq!(roots.staging, roots.data.join("staging"));
    }

    #[test]
    fn linux_standard_xdg_fallbacks_are_scoped_to_lsdj() {
        let home = PathBuf::from("/home/DJ Name");
        let roots = resolve(
            Platform::Linux,
            NativeDirs {
                config: home.join(".config"),
                data: home.join(".local/share"),
                local_data: home.join(".local/share"),
                cache: home.join(".cache"),
                documents: home.clone(),
                home: home.clone(),
            },
        );
        assert_eq!(roots.config, home.join(".config/lsdj"));
        assert_eq!(roots.data, home.join(".local/share/lsdj"));
        assert_eq!(roots.cache, home.join(".cache/lsdj"));
    }

    #[test]
    fn venv_python_handles_both_layouts_without_parsing_a_command_string() {
        let root = Path::new("/profiles/DJ Name/模型/.venv");
        assert_eq!(
            venv_python_for(Platform::Windows, root),
            root.join("Scripts").join("python.exe")
        );
        assert_eq!(
            venv_python_for(Platform::Linux, root),
            root.join("bin").join("python")
        );
    }

    #[test]
    fn backend_environment_preserves_spaces_and_non_ascii() {
        let roots = resolve(Platform::Linux, native("/home/DJ Name/音楽"));
        let values: std::collections::HashMap<_, _> = roots.backend_env().into_iter().collect();
        assert_eq!(
            values
                .get(std::ffi::OsStr::new("LSDJ_ASSETS_HOME"))
                .map(OsString::as_os_str),
            Some(roots.assets.as_os_str()),
        );
        assert_eq!(
            values
                .get(std::ffi::OsStr::new("SA3_MLX_HOME"))
                .map(OsString::as_os_str),
            Some(roots.sa3_home.as_os_str()),
        );
    }

    #[test]
    fn migration_is_atomic_and_restart_safe() {
        let root = std::env::temp_dir().join(format!(
            "lsdj-path-migrate-{}-{}",
            std::process::id(),
            "音楽 folder"
        ));
        let old = root.join("old brand").join("models");
        let new = root.join("new brand").join("models");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("weight.bin"), b"model").unwrap();

        assert_eq!(migrate_legacy_dir(&new, &old), new);
        assert_eq!(std::fs::read(new.join("weight.bin")).unwrap(), b"model");
        // A restart is a no-op and leaves the promoted destination intact.
        assert_eq!(migrate_legacy_dir(&new, &old), new);
        assert!(!old.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_migration_keeps_the_legacy_directory_active() {
        let root = std::env::temp_dir().join(format!(
            "lsdj-path-migrate-failure-{}",
            std::process::id()
        ));
        let old = root.join("legacy").join("models");
        let blocked_parent = root.join("blocked");
        let new = blocked_parent.join("models");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("weight.bin"), b"model").unwrap();
        std::fs::write(&blocked_parent, b"not a directory").unwrap();

        assert_eq!(migrate_legacy_dir(&new, &old), old);
        assert_eq!(std::fs::read(old.join("weight.bin")).unwrap(), b"model");
        assert!(!new.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
