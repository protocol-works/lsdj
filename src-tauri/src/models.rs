//! Model manager (issue #43): status, install, and delete for the two model
//! families — Magenta deck models and the Stable Audio 3 stack — surfaced in the
//! settings-drawer panel. Rust owns the lifecycle (mirrors ADR-0022); the actual
//! downloads are orchestrated by Rust:
//!
//! - **Magenta** → the frozen sidecar's `--init-resources` / `--download-model`
//!   modes (`backend/lsdj/sidecar.py`), which reuse `magenta_rt.cli` verbatim and
//!   stream a JSON progress contract on stdout. Resources are fetched first so a
//!   freshly downloaded model is actually loadable (a model's two files are not
//!   enough without `resources/musiccoca` + `resources/spectrostream`).
//! - **Stable Audio 3** → download the app-bundled immutable manifest's source,
//!   runtime, dependency, and model artifacts over native HTTPS; verify every
//!   SHA-256; extract with bounded native archive APIs; then build, warm, and
//!   atomically promote the candidate without a shell or external tools.
//!
//! Status facts mirror the stable conventions in `backend/lsdj/paths.py` and
//! `backend/lsdj/sa3.py` (the two-file model layout, the SA3 candidate list, the
//! four readiness states). The webview never gets filesystem access — the same
//! trust boundary as the rest of the library surface.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::child_process::{
    read_bounded_lines, sanitize_diagnostic, DiagnosticTail, SupervisedChild,
};
use crate::runtime_installer::archive::{
    extract_tar_gz_cancellable, extract_zip_cancellable, ArchiveLimits,
};
use crate::runtime_installer::download::{
    client as installer_client, download_verified, link_or_copy_verified, verify_file_cancellable,
    PinnedArtifact,
};
use crate::runtime_installer::promotion;

/// The official models the manager offers to download (mirrors
/// `engine.KNOWN_MODELS`). This is the installable catalog, NOT a discovery gate:
/// `discover_installed` finds any model folder, but only these can be downloaded.
pub const INSTALLABLE_MODELS: &[&str] = &["mrt2_small", "mrt2_base"];

// Canonical SA3 readiness states — the exact identifiers `sa3.readiness` uses.
const SA3_MISSING: &str = "missing";
const SA3_VENV_MISSING: &str = "venv_missing";
const SA3_NOT_WARMED: &str = "not_warmed";
const SA3_READY: &str = "ready";
const SA3_FAILED: &str = "failed";

const WARMED_STAMP: &str = ".lsdj-warmed";

// Records the source (`sa3-pin.json` repo + commit) the in-app installer fetched,
// so `model_status` can tell when the installed checkout has drifted from a bumped
// pin and offer an in-app update. Written by Rust after a fetch (the shell
// installer doesn't know the commit). Lives beside `.lsdj-warmed` in optimized/mlx.
const SOURCE_STAMP: &str = ".lsdj-source.json";
const INSTALL_MANIFEST_STAMP: &str = ".lsdj-install-manifest.json";
const MLX_REQUIREMENTS_LOCK: &str = include_str!("../../scripts/sa3-requirements.lock");
const TFLITE_REQUIREMENTS_LOCK: &str = include_str!("../../scripts/sa3-tflite-requirements.lock");
const TFLITE_WHEEL_PIN_JSON: &str = include_str!("../../sa3-tflite-wheels.json");
const MRT2_PIN_JSON: &str = include_str!("../../mrt2-pytorch-pin.json");
const MRT2_WHEEL_PIN_JSON: &str = include_str!("../../mrt2-pytorch-wheels.json");
const MRT2_LINUX_LOCK: &str =
    include_str!("../../backend/runtime-locks/mrt2-pytorch-linux-x86_64.txt");
const MRT2_WINDOWS_LOCK: &str =
    include_str!("../../backend/runtime-locks/mrt2-pytorch-windows-x86_64.txt");
const TFLITE_PROVENANCE_STAMP: &str = ".lsdj-provenance.json";
const MRT2_IDENTITY_STAMP: &str = ".lsdj-mrt2-install";

const BACKEND_SOURCES: &[(&str, &[u8])] = &[
    (
        "__init__.py",
        include_bytes!("../../backend/lsdj/__init__.py"),
    ),
    (
        "controller.py",
        include_bytes!("../../backend/lsdj/controller.py"),
    ),
    ("engine.py", include_bytes!("../../backend/lsdj/engine.py")),
    ("frozen.py", include_bytes!("../../backend/lsdj/frozen.py")),
    ("loras.py", include_bytes!("../../backend/lsdj/loras.py")),
    ("mrt2.py", include_bytes!("../../backend/lsdj/mrt2.py")),
    (
        "mrt2_pytorch.py",
        include_bytes!("../../backend/lsdj/mrt2_pytorch.py"),
    ),
    (
        "runtime_paths.py",
        include_bytes!("../../backend/lsdj/runtime_paths.py"),
    ),
    ("sa3.py", include_bytes!("../../backend/lsdj/sa3.py")),
    (
        "sa3_audio.py",
        include_bytes!("../../backend/lsdj/sa3_audio.py"),
    ),
    (
        "sa3_contract.py",
        include_bytes!("../../backend/lsdj/sa3_contract.py"),
    ),
    (
        "sidecar.py",
        include_bytes!("../../backend/lsdj/sidecar.py"),
    ),
    ("worker.py", include_bytes!("../../backend/lsdj/worker.py")),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sa3Backend {
    Mlx,
    Tflite,
}

impl Sa3Backend {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Mlx => "mlx",
            Self::Tflite => "tflite",
        }
    }

    fn runtime_dir(self, checkout: &Path) -> PathBuf {
        checkout.join("optimized").join(match self {
            Self::Mlx => "mlx",
            Self::Tflite => "tflite",
        })
    }

    fn script(self) -> &'static str {
        match self {
            Self::Mlx => "sa3_mlx.py",
            Self::Tflite => "sa3_tflite.py",
        }
    }

    fn requirements(self) -> (&'static str, &'static str) {
        match self {
            Self::Mlx => ("sa3-requirements.lock", MLX_REQUIREMENTS_LOCK),
            Self::Tflite => ("sa3-tflite-requirements.lock", TFLITE_REQUIREMENTS_LOCK),
        }
    }
}

fn sa3_backend_for(os: &str, arch: &str) -> Result<Sa3Backend, String> {
    match (os, arch) {
        ("macos", "aarch64") => Ok(Sa3Backend::Mlx),
        ("linux", "x86_64") | ("windows", "x86_64") => Ok(Sa3Backend::Tflite),
        _ => Err(format!("Stable Audio 3 is unsupported on {os}/{arch}")),
    }
}

fn host_sa3_backend() -> Result<Sa3Backend, String> {
    sa3_backend_for(std::env::consts::OS, std::env::consts::ARCH)
}

// --- Host-resolved paths (mirrors the explicit Python environment) --------

/// The `magenta-rt-v2` data root. `MAGENTA_HOME`'s compatibility semantics
/// append this segment; the host owns the resolved base.
fn magenta_home() -> PathBuf {
    crate::platform_paths::get()
        .magenta_base()
        .join("magenta-rt-v2")
}

/// The Magenta models dir (`paths.models_dir()`).
pub fn magenta_models_dir() -> PathBuf {
    #[cfg(feature = "managed-runtime")]
    if managed_mrt2_host() {
        return crate::managed_runtime::service_root(
            crate::platform_paths::get().assets(),
            crate::managed_runtime::Service::Mrt2,
        )
        .join("models");
    }
    magenta_home().join("models")
}

/// The host-owned home for the Stable Audio 3 checkout.
fn sa3_app_home() -> PathBuf {
    crate::platform_paths::get().sa3_home().to_path_buf()
}

/// Whether the shared resources a model load needs are present — without these
/// (`mrt models init` fetches them) a model's two files cannot load.
fn resources_present() -> bool {
    #[cfg(feature = "managed-runtime")]
    if managed_mrt2_host() {
        let root = crate::managed_runtime::service_root(
            crate::platform_paths::get().assets(),
            crate::managed_runtime::Service::Mrt2,
        );
        let pin = mrt2_pin();
        return validate_mrt2_identity(&root, &pin).is_ok()
            && mrt2_snapshot_present(&root, "musiccoca", &pin.processor);
    }
    let resources = magenta_home().join("resources");
    resources.join("musiccoca").is_dir() && resources.join("spectrostream").is_dir()
}

#[cfg(feature = "managed-runtime")]
fn managed_mrt2_host() -> bool {
    matches!(
        crate::managed_runtime::host_target().as_str(),
        "x86_64-unknown-linux-gnu" | "x86_64-pc-windows-msvc"
    )
}

#[cfg(feature = "managed-runtime")]
fn mrt2_snapshot_present(root: &Path, install_name: &str, snapshot: &SnapshotPin) -> bool {
    snapshot.files.iter().all(|file| {
        let Ok(metadata) =
            std::fs::symlink_metadata(root.join("models").join(install_name).join(&file.path))
        else {
            return false;
        };
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() == file.size
    })
}

/// SA3 checkout roots to probe, in order (mirrors `sa3._checkout_candidates`).
fn sa3_candidates() -> Vec<PathBuf> {
    vec![sa3_app_home()]
}

/// The SA3 install state + resolved checkout for this platform's backend.
fn sa3_status() -> (&'static str, Option<PathBuf>) {
    let Ok(backend) = host_sa3_backend() else {
        return (SA3_FAILED, None);
    };
    let mut first_with_runtime: Option<PathBuf> = None;
    for checkout in sa3_candidates() {
        let runtime = backend.runtime_dir(&checkout);
        if !runtime.is_dir() {
            continue;
        }
        if first_with_runtime.is_none() {
            first_with_runtime = Some(checkout.clone());
        }

        // A manifest marks an app-managed install. Never let its legacy stamps
        // bypass current trust policy: readiness means the manifest, runtime,
        // provenance, warm-up, and all eight model hashes validate. Only an
        // older hand-installed checkout with no app manifest uses the historical
        // interpreter/script/stamp heuristic below.
        let manifest = checkout.join(INSTALL_MANIFEST_STAMP);
        if !matches!(
            std::fs::symlink_metadata(&manifest),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ) {
            let state = if validate_sa3_install(&checkout, &sa3_pin(), backend).is_ok() {
                SA3_READY
            } else {
                SA3_FAILED
            };
            return (state, Some(checkout));
        }
        // Preserve the historical hand-installed MLX probe on macOS. Portable
        // installs require the authenticated app manifest and provenance stamp.
        if backend == Sa3Backend::Tflite {
            return (SA3_FAILED, Some(checkout));
        }
        let python = crate::platform_paths::venv_python(&runtime.join(".venv"));
        let script = runtime.join("scripts").join(backend.script());
        if !(python.is_file() && script.is_file()) {
            continue;
        }
        let state = if runtime.join(WARMED_STAMP).is_file() {
            SA3_READY
        } else {
            SA3_NOT_WARMED
        };
        return (state, Some(checkout));
    }
    match first_with_runtime {
        Some(checkout) => (SA3_VENV_MISSING, Some(checkout)),
        None => (SA3_MISSING, None),
    }
}

/// The source an SA3 checkout was installed from (or the one currently pinned):
/// the `sa3-pin.json` repo + commit. Serialised to the model-manager UI so it can
/// show what's installed vs what's available, and persisted in the checkout's
/// source stamp.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Sa3Source {
    repo: String,
    commit: String,
}

fn source_stamp_path(checkout: &Path) -> PathBuf {
    host_sa3_backend()
        .map(|backend| backend.runtime_dir(checkout).join(SOURCE_STAMP))
        .unwrap_or_else(|_| checkout.join(SOURCE_STAMP))
}

/// The source recorded in a checkout's stamp, or `None` when absent (a checkout
/// installed before stamping existed, or placed by hand) or unreadable.
fn read_source_stamp(checkout: &Path) -> Option<Sa3Source> {
    let data = std::fs::read_to_string(source_stamp_path(checkout)).ok()?;
    serde_json::from_str(&data).ok()
}

/// Record what was fetched into the checkout. A secure candidate is never
/// promoted without this provenance marker.
fn write_source_stamp(checkout: &Path, source: &Sa3Source) -> Result<(), String> {
    let json = serde_json::to_string_pretty(source)
        .map_err(|error| format!("cannot serialize SA3 source stamp: {error}"))?;
    write_synced(&source_stamp_path(checkout), json.as_bytes())
}

/// The currently pinned source (`sa3-pin.json`).
fn pinned_source() -> Sa3Source {
    let pin = sa3_pin();
    Sa3Source {
        repo: pin.repo,
        commit: pin.commit,
    }
}

/// Two commits match when either is a prefix of the other (tolerates short vs.
/// full SHAs); repos match after trimming a trailing slash.
fn sources_match(a: &Sa3Source, b: &Sa3Source) -> bool {
    let repo_eq = a.repo.trim_end_matches('/') == b.repo.trim_end_matches('/');
    let commit_eq = !a.commit.is_empty()
        && !b.commit.is_empty()
        && (a.commit.starts_with(&b.commit) || b.commit.starts_with(&a.commit));
    repo_eq && commit_eq
}

/// Whether an in-app update should be offered: a present checkout whose recorded
/// source differs from the pin — or one with no stamp at all (we can't prove it
/// matches, so it's updatable). A missing install is never "update available"
/// (that's a plain install). Pure, so the policy is unit-tested.
fn sa3_update_available(installed: Option<&Sa3Source>, pinned: &Sa3Source, present: bool) -> bool {
    if !present {
        return false;
    }
    match installed {
        Some(src) => !sources_match(src, pinned),
        None => true,
    }
}

/// Sum of file sizes under `path`, following file symlinks (HF weights symlink
/// into the shared cache; the target size is the meaningful "how big is this").
/// Best-effort: unreadable entries are skipped, and a symlinked directory is not
/// traversed (so it cannot loop).
pub(crate) fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            total += dir_size(&entry.path());
        } else if file_type.is_symlink() {
            if let Ok(meta) = std::fs::metadata(entry.path()) {
                if meta.is_file() {
                    total += meta.len();
                }
            }
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

/// On-disk size of the SA3 checkout, cached and invalidated by the `.lsdj-warmed`
/// stamp's mtime. The checkout is a uv venv plus warmed weights — many files to
/// walk — and `model_status` is re-fetched on every drawer open and
/// `models://changed`, so an unwarmed/changing checkout is walked but a settled
/// one is summed once.
fn sa3_checkout_size(checkout: &Path) -> u64 {
    static CACHE: Mutex<Option<(PathBuf, std::time::SystemTime, u64)>> = Mutex::new(None);
    let stamp_mtime = host_sa3_backend()
        .ok()
        .and_then(|backend| {
            std::fs::metadata(backend.runtime_dir(checkout).join(WARMED_STAMP)).ok()
        })
        .and_then(|metadata| metadata.modified().ok());
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let (Some(mtime), Some((path, cached_mtime, size))) = (stamp_mtime, cache.as_ref()) {
        if path == checkout && *cached_mtime == mtime {
            return *size;
        }
    }
    let size = dir_size(checkout);
    if let Some(mtime) = stamp_mtime {
        *cache = Some((checkout.to_path_buf(), mtime, size));
    }
    size
}

/// Every installed Magenta model, discovered by its files (mirrors
/// `engine.available_models`): a `<name>/` dir with `<name>.mlxfn` +
/// `<name>_state.safetensors`. Sorted.
pub fn discover_installed(models_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir(models_dir) else {
        return names;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if dir.join(format!("{name}.mlxfn")).is_file()
            && dir.join(format!("{name}_state.safetensors")).is_file()
        {
            names.push(name);
        }
    }
    names.sort();
    names
}

// --- Status DTO ------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledModel {
    name: String,
    size_bytes: u64,
    /// True when the model's files are present but the shared resources a load
    /// needs are not — the manager flags it rather than mislabelling it "ready".
    needs_resources: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MagentaStatus {
    models_dir: String,
    resources_present: bool,
    installable: Vec<&'static str>,
    installed: Vec<InstalledModel>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sa3Status {
    state: &'static str,
    backend: Option<&'static str>,
    size_bytes: u64,
    /// Exact unique model bytes in the selected backend's pinned manifest. The
    /// UI shows this before install; source/runtime/wheel overhead is additional.
    download_bytes: u64,
    checkout: Option<String>,
    /// The source the installed checkout was fetched from (`None` when the
    /// checkout predates stamping or was placed by hand).
    installed_source: Option<Sa3Source>,
    /// The source currently pinned (`sa3-pin.json`).
    pinned_source: Sa3Source,
    /// True when an installed checkout differs from the pin (or is unstamped) —
    /// the manager offers an in-place update.
    update_available: bool,
}

/// The in-flight install in the status snapshot, so a reopened manager reflects
/// it without having seen the live `model://progress` events. `name` is the model
/// for Magenta, `""` for SA3.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveInstall {
    family: Family,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStatus {
    magenta: MagentaStatus,
    sa3: Sa3Status,
    /// The installed SA3 LoRA adapters (issue #66), listed with the models so
    /// the manager drawer and the generate pickers share one snapshot.
    loras: Vec<crate::loras::LoraInfo>,
    installing: Option<ActiveInstall>,
}

fn status(active: Option<(Family, String)>) -> ModelStatus {
    let models_dir = magenta_models_dir();
    let resources = resources_present();
    #[cfg(feature = "managed-runtime")]
    let installed = if managed_mrt2_host() {
        let root = models_dir.parent().unwrap_or(&models_dir);
        let pin = mrt2_pin();
        if validate_mrt2_identity(root, &pin).is_ok() {
            pin.models
                .iter()
                .filter(|(name, snapshot)| mrt2_snapshot_present(root, name, snapshot))
                .map(|(name, _)| InstalledModel {
                    name: name.clone(),
                    size_bytes: dir_size(&models_dir.join(name)),
                    needs_resources: !resources,
                })
                .collect()
        } else {
            Vec::new()
        }
    } else {
        discover_installed(&models_dir)
            .into_iter()
            .map(|name| InstalledModel {
                size_bytes: dir_size(&models_dir.join(&name)),
                name,
                needs_resources: !resources,
            })
            .collect()
    };
    #[cfg(not(feature = "managed-runtime"))]
    let installed = discover_installed(&models_dir)
        .into_iter()
        .map(|name| InstalledModel {
            size_bytes: dir_size(&models_dir.join(&name)),
            name,
            needs_resources: !resources,
        })
        .collect();
    let (sa3_state, sa3_checkout) = sa3_status();
    let backend = host_sa3_backend().ok();
    let download_bytes = backend
        .and_then(|backend| model_download_bytes(&sa3_pin(), backend).ok())
        .unwrap_or(0);
    let sa3_size = sa3_checkout
        .as_ref()
        .map(|c| sa3_checkout_size(c))
        .unwrap_or(0);
    let pinned = pinned_source();
    let installed_source = sa3_checkout.as_ref().and_then(|c| read_source_stamp(c));
    let update_available =
        sa3_update_available(installed_source.as_ref(), &pinned, sa3_state != SA3_MISSING);
    ModelStatus {
        magenta: MagentaStatus {
            models_dir: models_dir.to_string_lossy().into_owned(),
            resources_present: resources,
            installable: INSTALLABLE_MODELS.to_vec(),
            installed,
        },
        sa3: Sa3Status {
            state: sa3_state,
            backend: backend.map(Sa3Backend::wire_name),
            size_bytes: sa3_size,
            download_bytes,
            checkout: sa3_checkout.map(|c| c.to_string_lossy().into_owned()),
            installed_source,
            pinned_source: pinned,
            update_available,
        },
        loras: crate::loras::discover(&crate::loras::loras_dir()),
        installing: active.map(|(family, name)| ActiveInstall { family, name }),
    }
}

// --- Install / delete ------------------------------------------------------

/// Which family a command targets. `lowercase` serde is the single source of the
/// wire spelling (`"magenta"`/`"sa3"`), used both ways.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Magenta,
    Sa3,
    /// SA3 LoRA adapters (issue #66) — same progress/changed channels, its own
    /// import commands (an adapter needs a source + optional base, not a name).
    Lora,
}

/// The `model://progress` payload the webview renders as a live install bar.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelProgress {
    family: Family,
    name: String,
    stage: String,
    message: Option<String>,
    file: Option<String>,
}

fn emit(
    app: &AppHandle,
    family: Family,
    name: &str,
    stage: &str,
    message: Option<String>,
    file: Option<String>,
) {
    let _ = app.emit(
        "model://progress",
        ModelProgress {
            family,
            name: name.to_string(),
            stage: sanitize_diagnostic(stage),
            message: message.as_deref().map(sanitize_diagnostic),
            file: file.as_deref().map(sanitize_diagnostic),
        },
    );
}

/// The pinned SA3 source (`sa3-pin.json`, the single bump point). Compiled in so
/// a released binary carries the pin it was built with.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArchivePin {
    #[serde(flatten)]
    artifact: PinnedArtifact,
    archive_root: String,
    max_files: u64,
    max_expanded_bytes: u64,
    #[serde(default = "default_archive_format")]
    archive_format: String,
}

fn default_archive_format() -> String {
    "tar.gz".into()
}

impl ArchivePin {
    fn limits(&self) -> ArchiveLimits {
        ArchiveLimits {
            max_files: self.max_files,
            max_expanded_bytes: self.max_expanded_bytes,
            materialize_safe_links: false,
        }
    }

    fn extract(
        &self,
        source: std::fs::File,
        destination: &Path,
        materialize_safe_links: bool,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let limits = ArchiveLimits {
            materialize_safe_links,
            ..self.limits()
        };
        match self.archive_format.as_str() {
            "tar.gz" => extract_tar_gz_cancellable(
                source,
                destination,
                &self.archive_root,
                limits,
                is_cancelled,
            ),
            "zip" if !materialize_safe_links => extract_zip_cancellable(
                source,
                destination,
                &self.archive_root,
                limits,
                is_cancelled,
            ),
            "zip" => Err("ZIP runtime archives may not contain links".into()),
            _ => Err("runtime archive format is unsupported".into()),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UvPin {
    target: String,
    version: String,
    #[serde(flatten)]
    archive: ArchivePin,
    executable: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PythonPin {
    target: String,
    version: String,
    #[serde(flatten)]
    archive: ArchivePin,
    executable: String,
}

#[derive(Clone, Deserialize)]
struct RuntimePin {
    requirements: String,
    python: Vec<PythonPin>,
    uv: Vec<UvPin>,
}

#[derive(Clone, Deserialize)]
struct ModelArtifactPin {
    path: String,
    sha256: String,
    size: u64,
}

impl ModelArtifactPin {
    fn filename(&self) -> Result<&str, String> {
        let filename = self
            .path
            .strip_prefix("MLX/")
            .ok_or("SA3 model path must be below MLX/")?;
        if filename.is_empty()
            || filename.contains('/')
            || filename.contains('\\')
            || filename == "."
            || filename == ".."
        {
            return Err("SA3 model path is unsafe".into());
        }
        Ok(filename)
    }

    fn artifact(&self, models: &ModelsPin) -> Result<PinnedArtifact, String> {
        let filename = self.filename()?;
        Ok(PinnedArtifact {
            url: format!(
                "https://huggingface.co/{}/resolve/{}/MLX/{filename}?download=true",
                models.repo, models.revision
            ),
            sha256: self.sha256.clone(),
            size: self.size,
        })
    }
}

#[derive(Clone, Deserialize)]
struct ModelsPin {
    repo: String,
    revision: String,
    artifacts: Vec<ModelArtifactPin>,
}

#[derive(Clone, Deserialize)]
struct Sa3Pin {
    repo: String,
    commit: String,
    source: ArchivePin,
    runtime: RuntimePin,
    models: ModelsPin,
}

const SA3_PIN_JSON: &str = include_str!("../../sa3-pin.json");
const TFLITE_PIN_JSON: &str = include_str!("../../sa3-tflite-pin.json");

fn sa3_pin() -> Sa3Pin {
    serde_json::from_str(SA3_PIN_JSON).expect("sa3-pin.json is valid JSON")
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TfliteRuntimePin {
    repo: String,
    revision: String,
    subdirectory: String,
    entrypoint: String,
    requirements_lock: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TfliteArtifactPin {
    path: String,
    install_path: String,
    sha256: String,
    size: u64,
}

impl TfliteArtifactPin {
    fn artifact(&self, models: &TfliteModelsPin) -> PinnedArtifact {
        PinnedArtifact {
            url: format!(
                "https://huggingface.co/{}/resolve/{}/{}?download=true",
                models.repo, models.revision, self.path
            ),
            sha256: self.sha256.clone(),
            size: self.size,
        }
    }
}

#[derive(Clone, Deserialize)]
struct TfliteModelsPin {
    repo: String,
    revision: String,
    precision: String,
    shared: Vec<TfliteArtifactPin>,
    bundles: BTreeMap<String, Vec<TfliteArtifactPin>>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TflitePin {
    schema_version: u32,
    runtime: TfliteRuntimePin,
    models: TfliteModelsPin,
}

fn tflite_pin() -> TflitePin {
    serde_json::from_str(TFLITE_PIN_JSON).expect("sa3-tflite-pin.json is valid JSON")
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WheelPin {
    package: String,
    version: String,
    filename: String,
    #[serde(flatten)]
    artifact: PinnedArtifact,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WheelManifest {
    schema_version: u32,
    python: String,
    common: Vec<WheelPin>,
    targets: BTreeMap<String, Vec<WheelPin>>,
}

fn wheel_manifest() -> WheelManifest {
    serde_json::from_str(TFLITE_WHEEL_PIN_JSON).expect("sa3-tflite-wheels.json is valid JSON")
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotFilePin {
    path: String,
    size: u64,
    sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotPin {
    repository: String,
    revision: String,
    files: Vec<SnapshotFilePin>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Mrt2RuntimePin {
    python: Vec<PythonPin>,
    uv: Vec<UvPin>,
    locks: BTreeMap<String, String>,
    wheel_manifest_sha256: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Mrt2Pin {
    schema_version: u32,
    runtime: Mrt2RuntimePin,
    models: BTreeMap<String, SnapshotPin>,
    processor: SnapshotPin,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Mrt2WheelManifest {
    schema_version: u32,
    python: String,
    targets: BTreeMap<String, Vec<WheelPin>>,
}

fn mrt2_pin() -> Mrt2Pin {
    serde_json::from_str(MRT2_PIN_JSON).expect("mrt2-pytorch-pin.json is valid JSON")
}

fn mrt2_wheel_manifest() -> Mrt2WheelManifest {
    serde_json::from_str(MRT2_WHEEL_PIN_JSON).expect("mrt2-pytorch-wheels.json is valid JSON")
}

/// Shared install state: at most one install runs at a time; the running stage's
/// child is parked here so [`InstallManager::cancel`] / shutdown can reach it.
/// `active` names the in-flight job so `model_status` can report it — the manager
/// reflects an install even after the drawer was closed and reopened (the live
/// `model://progress` events are missed while it's unmounted).
pub(crate) struct InstallShared {
    busy: AtomicBool,
    cancelled: AtomicBool,
    current_child: Mutex<Option<SupervisedChild>>,
    active: Mutex<Option<(Family, String)>>,
}

/// Owns the in-flight install child (Tauri managed state). Mirrors the
/// supervise + cancel + `RunEvent::Exit` teardown pattern of [`crate::sidecar`]
/// and [`crate::generation`] — a multi-minute install must not orphan on quit.
pub struct InstallManager {
    shared: Arc<InstallShared>,
}

impl InstallManager {
    pub fn new() -> Self {
        InstallManager {
            shared: Arc::new(InstallShared {
                busy: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                current_child: Mutex::new(None),
                active: Mutex::new(None),
            }),
        }
    }

    /// The in-flight install `(family, name)`, for `model_status`. `name` is the
    /// model for Magenta, `""` for SA3.
    pub fn active_install(&self) -> Option<(Family, String)> {
        self.shared
            .active
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Start an install on a background thread; progress arrives as
    /// `model://progress` events and a final `models://changed` tells the UI to
    /// re-fetch status. Errors immediately if an install is already running or a
    /// Magenta name is unknown.
    pub fn install(
        &self,
        app: AppHandle,
        family: Family,
        name: Option<String>,
        update: bool,
    ) -> Result<(), String> {
        match family {
            Family::Magenta => {
                let name = name.ok_or("a model name is required")?;
                if !INSTALLABLE_MODELS.contains(&name.as_str()) {
                    return Err(format!("unknown model '{name}'"));
                }
                let model = name.clone();
                self.start(app, family, name, move |progress, shared| {
                    install_magenta(progress, shared, &model)
                })
            }
            // `model://progress` carries the model name for Magenta, "" for SA3.
            Family::Sa3 => self.start(app, family, String::new(), move |progress, shared| {
                install_sa3(progress, shared, update)
            }),
            Family::Lora => Err("adapters are installed via install_lora".into()),
        }
    }

    /// Import an SA3 LoRA adapter (issue #66) on the same install thread and
    /// event channels; `spec` names the source (HuggingFace repo or local path)
    /// and an optional explicit base.
    pub fn install_lora(
        &self,
        app: AppHandle,
        spec: crate::loras::ImportSpec,
    ) -> Result<(), String> {
        let name = spec.display_name()?;
        self.start(app, Family::Lora, name, move |progress, shared| {
            crate::loras::install(progress, shared, &spec)
        })
    }

    /// The shared install-thread dance: claim the single install slot, run `job`
    /// with a progress sink wired to `model://progress` (as `family`/`name`),
    /// then emit the terminal event and `models://changed`.
    fn start(
        &self,
        app: AppHandle,
        family: Family,
        name: String,
        job: impl FnOnce(&Progress, &InstallShared) -> Result<(), String> + Send + 'static,
    ) -> Result<(), String> {
        if self.shared.busy.swap(true, Ordering::AcqRel) {
            return Err("an install is already running".into());
        }
        self.shared.cancelled.store(false, Ordering::Release);
        *self.shared.active.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((family, name.clone()));
        let shared = self.shared.clone();
        std::thread::Builder::new()
            .name("lsdj-model-install".into())
            .spawn(move || {
                let progress_app = app.clone();
                let progress = move |stage: &str, message: Option<String>, file: Option<String>| {
                    emit(&progress_app, family, &name, stage, message, file);
                };
                let result = job(&progress, &shared);
                *shared
                    .current_child
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = None;
                match result {
                    Ok(()) => emit(&app, family, "", "done", None, None),
                    // A user cancel is a clean stop, not a failure — the UI must
                    // not surface it as an error.
                    Err(_) if shared.cancelled.load(Ordering::Acquire) => {
                        emit(&app, family, "", "cancelled", None, None)
                    }
                    Err(message) => emit(&app, family, "", "error", Some(message), None),
                }
                // Clear the active job BEFORE the refresh so a reopened manager
                // sees the install as finished, not stuck.
                *shared.active.lock().unwrap_or_else(|p| p.into_inner()) = None;
                // Re-fetch status either way (a partial/failed install changes nothing
                // on disk that looks installed, but sizes / readiness may have moved).
                let _ = app.emit("models://changed", ());
                shared.busy.store(false, Ordering::Release);
            })
            .map_err(|e| {
                self.shared.busy.store(false, Ordering::Release);
                *self.shared.active.lock().unwrap_or_else(|p| p.into_inner()) = None;
                format!("cannot start install: {e}")
            })?;
        Ok(())
    }

    /// Cancel an in-flight install: flag it and kill the running stage's child.
    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Release);
        if let Some(mut child) = self
            .shared
            .current_child
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            let _ = child.force_kill();
        }
    }

    /// `RunEvent::Exit` teardown — kill any in-flight install (Tauri does not drop
    /// managed state on a macOS quit).
    pub fn shutdown(&self) {
        self.cancel();
    }
}

impl Default for InstallManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for InstallManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn cancelled(shared: &InstallShared) -> Result<(), String> {
    if shared.cancelled.load(Ordering::Acquire) {
        Err("cancelled".into())
    } else {
        Ok(())
    }
}

pub(crate) fn is_cancelled(shared: &InstallShared) -> bool {
    shared.cancelled.load(Ordering::Acquire)
}

/// Publish a newly spawned child to cancellation and close the spawn/park race.
///
/// `cancel()` stores the flag before taking `current_child`. If cancellation
/// lands after the process is spawned but before this lock is acquired, the
/// second flag check below takes responsibility for terminating the child.
fn park_child(shared: &InstallShared, child: SupervisedChild) -> Result<(), String> {
    let mut current = shared
        .current_child
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    *current = Some(child);
    if shared.cancelled.load(Ordering::Acquire) {
        let mut child = current.take().expect("newly parked child is present");
        drop(current);
        let _ = child.force_kill();
        return Err("cancelled".into());
    }
    Ok(())
}

/// Run `cmd` to completion, feeding each stdout line to `on_line` and draining
/// stderr to the app log (so the pipe cannot fill and deadlock). Parks the child
/// in `shared` so cancel/shutdown can kill it. Returns an error on a non-zero
/// exit, a cancel, or a spawn/wait failure.
pub(crate) fn stream_child(
    shared: &InstallShared,
    label: &str,
    mut cmd: Command,
    mut on_line: impl FnMut(&str),
) -> Result<(), String> {
    cancelled(shared)?;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = crate::child_process::spawn_grouped(&mut cmd)
        .map_err(|error| sanitize_diagnostic(&format!("{label}: cannot spawn ({error})")))?;
    let stdout = child.take_stdout().expect("piped stdout");
    let stderr = child.take_stderr().expect("piped stderr");
    park_child(shared, child)?;

    let drain_label = label.to_string();
    let stderr_drain = std::thread::spawn(move || {
        let mut diagnostics = DiagnosticTail::default();
        if let Err(error) = read_bounded_lines(stderr, |line| {
            diagnostics.push(line);
        }) {
            diagnostics.push(&format!("stderr read failed: {error}"));
        }
        (drain_label, diagnostics)
    });
    let stdout_result = read_bounded_lines(stdout, |line| {
        on_line(line);
    });
    let (drain_label, diagnostics) = stderr_drain
        .join()
        .unwrap_or_else(|_| (label.to_string(), DiagnosticTail::default()));

    // Reclaim the child to read its exit status; cancel() may have taken it.
    let Some(mut child) = shared
        .current_child
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
    else {
        return Err("cancelled".into());
    };
    let status = child
        .wait()
        .map_err(|error| sanitize_diagnostic(&format!("{label}: wait failed ({error})")))?;
    cancelled(shared)?;
    stdout_result
        .map_err(|error| sanitize_diagnostic(&format!("{label}: stdout read failed ({error})")))?;
    if !status.success() {
        let detail = diagnostics.render();
        let message = if detail.is_empty() {
            format!("{label}: exited with {status}")
        } else {
            format!("{label}: exited with {status}; diagnostics:\n{detail}")
        };
        return Err(sanitize_diagnostic(&message));
    }
    if !diagnostics.is_empty() {
        eprintln!("lsdj-app: {drain_label}: {}", diagnostics.render());
    }
    Ok(())
}

/// One parsed line of the sidecar's JSON progress contract.
#[derive(Deserialize)]
#[cfg(any(not(feature = "managed-runtime"), test))]
struct SidecarLine {
    event: String,
    file: Option<String>,
    stage: Option<String>,
    message: Option<String>,
}

/// A progress sink: `(stage, message, file)`. Injected so the install driver is
/// decoupled from `AppHandle` — production wires it to a `model://progress`
/// emit; tests record the events while the install actually runs.
pub(crate) type Progress = dyn Fn(&str, Option<String>, Option<String>);

fn install_magenta(progress: &Progress, shared: &InstallShared, name: &str) -> Result<(), String> {
    #[cfg(feature = "managed-runtime")]
    {
        return install_mrt2_managed(progress, shared, name);
    }

    #[cfg(not(feature = "managed-runtime"))]
    {
        progress("download", None, None);
        let mut cmd = crate::sidecar::sidecar_base_command().map_err(|e| e.to_string())?;
        if !resources_present() {
            // Fetch the shared resources first, in the same child — without them the
            // downloaded model cannot load.
            cmd.arg("--init-resources");
        }
        cmd.args(["--download-model", name]);
        run_download(progress, shared, cmd)
    }
}

#[cfg(feature = "managed-runtime")]
fn install_mrt2_managed(
    progress: &Progress,
    shared: &InstallShared,
    name: &str,
) -> Result<(), String> {
    let pin = mrt2_pin();
    validate_mrt2_pin(&pin)?;
    let target = host_installer_target()?;
    if !matches!(
        target,
        "x86_64-unknown-linux-gnu" | "x86_64-pc-windows-msvc"
    ) {
        return Err("the managed PyTorch MRT2 runtime is Linux/Windows x86-64 only".into());
    }
    let snapshot = pin.models.get(name).ok_or("MRT2 model pin is missing")?;
    let python = pin
        .runtime
        .python
        .iter()
        .find(|item| item.target == target)
        .ok_or("MRT2 Python pin is missing")?;
    let uv = pin
        .runtime
        .uv
        .iter()
        .find(|item| item.target == target)
        .ok_or("MRT2 uv pin is missing")?;
    let paths = crate::platform_paths::get();
    let home =
        crate::managed_runtime::service_root(paths.assets(), crate::managed_runtime::Service::Mrt2);
    let work = paths.staging().join("mrt2").join(target);
    let candidate = work.join("candidate");
    let backup = paths.staging().join("mrt2-previous");
    std::fs::create_dir_all(&work)
        .map_err(|error| format!("cannot create MRT2 staging: {error}"))?;
    if let Some(parent) = home.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create MRT2 service root: {error}"))?;
    }
    let cancelled_now = || is_cancelled(shared);
    promotion::recover(&home, &backup, |root| {
        validate_mrt2_candidate(root, &pin, name, &cancelled_now)
    })?;
    if candidate.exists() {
        std::fs::remove_dir_all(&candidate)
            .map_err(|error| format!("cannot clear interrupted MRT2 candidate: {error}"))?;
    }

    let reusable = validate_mrt2_identity(&home, &pin).is_ok()
        && crate::managed_runtime::validate_candidate(&home, crate::managed_runtime::Service::Mrt2)
            .is_ok();
    if reusable {
        copy_regular_tree(&home, &candidate)?;
        // `copy_regular_tree` prefers hard links. Unlink every stamp that will
        // be rewritten so refreshing the candidate can never truncate the
        // still-active generation through a shared inode.
        for metadata in [
            crate::managed_runtime::MANIFEST_NAME,
            ".lsdj-generation",
            MRT2_IDENTITY_STAMP,
        ] {
            let path = candidate.join(metadata);
            if path.exists() {
                std::fs::remove_file(path)
                    .map_err(|error| format!("cannot refresh MRT2 generation metadata: {error}"))?;
            }
        }
        let backend = candidate.join("lsdj_backend");
        if backend.exists() {
            std::fs::remove_dir_all(&backend)
                .map_err(|error| format!("cannot refresh MRT2 backend sources: {error}"))?;
        }
    } else {
        std::fs::create_dir_all(&candidate)
            .map_err(|error| format!("cannot create MRT2 candidate: {error}"))?;
        install_mrt2_runtime(progress, shared, &work, &candidate, target, python, uv)?;
    }
    install_backend_sources(&candidate)?;
    install_mrt2_snapshot(progress, shared, &work, &candidate, name, snapshot)?;
    install_mrt2_snapshot(
        progress,
        shared,
        &work,
        &candidate,
        "musiccoca",
        &pin.processor,
    )?;
    write_mrt2_identity(&candidate, &pin)?;
    materialize_contained_file_links(&candidate)?;
    seal_mrt2_candidate(&candidate, &pin, python)?;
    validate_mrt2_candidate(&candidate, &pin, name, &cancelled_now)?;
    progress("promote", None, None);
    promotion::promote(&candidate, &home, &backup, |root| {
        validate_mrt2_candidate(root, &pin, name, &cancelled_now)
    })?;
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

#[cfg(feature = "managed-runtime")]
fn mrt2_identity(pin: &Mrt2Pin) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(MRT2_PIN_JSON.as_bytes());
    digest.update(MRT2_WHEEL_PIN_JSON.as_bytes());
    digest.update(
        mrt2_lock_for(&crate::managed_runtime::host_target())
            .unwrap_or_default()
            .as_bytes(),
    );
    for (name, bytes) in BACKEND_SOURCES {
        digest.update(name.as_bytes());
        digest.update(bytes);
    }
    let _ = pin;
    format!("{}\n", hex::encode(digest.finalize()))
}

#[cfg(feature = "managed-runtime")]
fn write_mrt2_identity(root: &Path, pin: &Mrt2Pin) -> Result<(), String> {
    write_synced(
        &root.join(MRT2_IDENTITY_STAMP),
        mrt2_identity(pin).as_bytes(),
    )
}

#[cfg(feature = "managed-runtime")]
fn validate_mrt2_identity(root: &Path, pin: &Mrt2Pin) -> Result<(), String> {
    let path = root.join(MRT2_IDENTITY_STAMP);
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|error| format!("MRT2 install identity is missing: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("MRT2 install identity is not a regular file".into());
    }
    if std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read MRT2 identity: {error}"))?
        != mrt2_identity(pin)
    {
        return Err("MRT2 install identity is stale".into());
    }
    Ok(())
}

#[cfg(feature = "managed-runtime")]
fn copy_regular_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(source)
        .map_err(|error| format!("cannot inspect reusable MRT2 runtime: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("reusable MRT2 runtime is not a trusted directory".into());
    }
    std::fs::create_dir_all(destination)
        .map_err(|error| format!("cannot create MRT2 candidate directory: {error}"))?;
    for entry in std::fs::read_dir(source)
        .map_err(|error| format!("cannot enumerate reusable MRT2 runtime: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("cannot enumerate reusable MRT2 runtime: {error}"))?;
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&from)
            .map_err(|error| format!("cannot inspect reusable MRT2 artifact: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("reusable MRT2 runtime contains a symbolic link".into());
        }
        if metadata.is_dir() {
            copy_regular_tree(&from, &to)?;
        } else if metadata.is_file() {
            if std::fs::hard_link(&from, &to).is_err() {
                std::fs::copy(&from, &to)
                    .map_err(|error| format!("cannot copy reusable MRT2 artifact: {error}"))?;
            }
        } else {
            return Err("reusable MRT2 runtime contains an unsupported entry".into());
        }
    }
    Ok(())
}

#[cfg(feature = "managed-runtime")]
fn download_mrt2_wheelhouse(
    progress: &Progress,
    shared: &InstallShared,
    work: &Path,
    target: &str,
) -> Result<(PathBuf, Vec<WheelPin>), String> {
    let pins = mrt2_wheel_pins_for(target)?;
    let directory = work.join("wheelhouse");
    if directory.exists() {
        std::fs::remove_dir_all(&directory)
            .map_err(|error| format!("cannot clear interrupted MRT2 wheelhouse: {error}"))?;
    }
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create MRT2 wheelhouse: {error}"))?;
    let client = installer_client()?;
    for pin in &pins {
        cancelled(shared)?;
        progress("fetch", None, Some(pin.filename.clone()));
        download_verified(
            &client,
            &pin.artifact,
            &directory.join(&pin.filename),
            None,
            || is_cancelled(shared),
        )?;
    }
    verify_wheelhouse(&directory, &pins, &|| is_cancelled(shared))?;
    Ok((directory, pins))
}

#[cfg(feature = "managed-runtime")]
fn install_mrt2_runtime(
    progress: &Progress,
    shared: &InstallShared,
    work: &Path,
    candidate: &Path,
    target: &str,
    python: &PythonPin,
    uv: &UvPin,
) -> Result<(), String> {
    let client = installer_client()?;
    let blobs = work.join("blobs");
    std::fs::create_dir_all(&blobs)
        .map_err(|error| format!("cannot create MRT2 blob staging: {error}"))?;
    progress("fetch", None, Some(format!("Python {}", python.version)));
    let python_archive = blobs.join("python.tar.gz");
    download_verified(
        &client,
        &python.archive.artifact,
        &python_archive,
        None,
        || is_cancelled(shared),
    )?;
    let python_root = candidate.join("runtime").join(".python");
    python.archive.extract(
        std::fs::File::open(&python_archive)
            .map_err(|error| format!("cannot open MRT2 Python: {error}"))?,
        &python_root,
        true,
        &|| is_cancelled(shared),
    )?;
    let runtime_python = python_root.join(&python.executable);
    verify_python_version(shared, &runtime_python, &python.version)?;

    progress("fetch", None, Some(format!("uv {}", uv.version)));
    let uv_archive = blobs.join(if uv.archive.archive_format == "zip" {
        "uv.zip"
    } else {
        "uv.tar.gz"
    });
    download_verified(&client, &uv.archive.artifact, &uv_archive, None, || {
        is_cancelled(shared)
    })?;
    let uv_root = work.join("uv");
    if uv_root.exists() {
        std::fs::remove_dir_all(&uv_root)
            .map_err(|error| format!("cannot clear MRT2 uv staging: {error}"))?;
    }
    uv.archive.extract(
        std::fs::File::open(&uv_archive)
            .map_err(|error| format!("cannot open MRT2 uv: {error}"))?,
        &uv_root,
        false,
        &|| is_cancelled(shared),
    )?;
    let uv_executable = uv_root.join(&uv.executable);
    verify_uv_version(shared, &uv_executable, &uv.version)?;
    let (wheelhouse, wheels) = download_mrt2_wheelhouse(progress, shared, work, target)?;

    let runtime = candidate.join("runtime");
    let venv = runtime.join(".venv");
    let cache = work.join("uv-cache");
    let mut create = uv_command(&uv_executable, &runtime, &cache);
    create
        .args([
            "venv",
            "--relocatable",
            "--no-managed-python",
            "--no-python-downloads",
            "--no-config",
            "--link-mode",
            "copy",
            "--python",
        ])
        .arg(&runtime_python)
        .arg(&venv);
    stream_child(shared, "mrt2-venv", create, |_| {})?;
    let venv_python = crate::platform_paths::venv_python(&venv);
    let mut install = uv_command(&uv_executable, &runtime, &cache);
    install
        .args(["pip", "install", "--python"])
        .arg(&venv_python);
    configure_portable_install(&mut install, &wheelhouse, &wheels, &|| is_cancelled(shared))?;
    stream_child(shared, "mrt2-dependencies", install, |_| {})?;
    let mut check = Command::new(&venv_python);
    check
        .current_dir(&runtime)
        .env("HF_HUB_OFFLINE", "1")
        .args([
            "-c",
            "import torch, transformers, safetensors, sentencepiece, resampy",
        ]);
    stream_child(shared, "mrt2-runtime-check", check, |_| {})
}

#[cfg(feature = "managed-runtime")]
fn install_mrt2_snapshot(
    progress: &Progress,
    shared: &InstallShared,
    work: &Path,
    candidate: &Path,
    install_name: &str,
    snapshot: &SnapshotPin,
) -> Result<(), String> {
    let client = installer_client()?;
    let token = std::env::var("HF_TOKEN")
        .ok()
        .or_else(|| std::env::var("HUGGING_FACE_HUB_TOKEN").ok());
    for file in &snapshot.files {
        cancelled(shared)?;
        let artifact = snapshot_artifact(snapshot, file)?;
        progress("fetch", None, Some(format!("{install_name}/{}", file.path)));
        let staged = work
            .join("blobs")
            .join("snapshots")
            .join(install_name)
            .join(&file.path);
        download_verified(&client, &artifact, &staged, token.as_deref(), || {
            is_cancelled(shared)
        })?;
        let destination = candidate.join("models").join(install_name).join(&file.path);
        link_or_copy_verified(&staged, &destination, &artifact, &|| is_cancelled(shared))?;
    }
    Ok(())
}

#[cfg(feature = "managed-runtime")]
fn verify_mrt2_snapshot(
    root: &Path,
    install_name: &str,
    snapshot: &SnapshotPin,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    for file in &snapshot.files {
        verify_file_cancellable(
            &root.join("models").join(install_name).join(&file.path),
            &snapshot_artifact(snapshot, file)?,
            is_cancelled,
        )
        .map_err(|error| {
            format!(
                "MRT2 {install_name}/{} failed integrity: {error}",
                file.path
            )
        })?;
    }
    Ok(())
}

#[cfg(feature = "managed-runtime")]
fn validate_mrt2_candidate(
    root: &Path,
    pin: &Mrt2Pin,
    model: &str,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    validate_mrt2_identity(root, pin)?;
    verify_mrt2_snapshot(
        root,
        model,
        pin.models.get(model).ok_or("MRT2 model pin is missing")?,
        is_cancelled,
    )?;
    verify_mrt2_snapshot(root, "musiccoca", &pin.processor, is_cancelled)?;
    crate::managed_runtime::validate_candidate(root, crate::managed_runtime::Service::Mrt2)
}

/// Spawn the download tooling and map its JSON progress contract onto the sink.
/// Takes the fully-built command so the spawn+parse path is testable against a
/// stub without mutating the process environment.
#[cfg(any(not(feature = "managed-runtime"), test))]
fn run_download(progress: &Progress, shared: &InstallShared, cmd: Command) -> Result<(), String> {
    let mut last_error: Option<String> = None;
    let result = stream_child(shared, "download-model", cmd, |line| {
        let Ok(parsed) = serde_json::from_str::<SidecarLine>(line) else {
            return;
        };
        match parsed.event.as_str() {
            // The keyed stage label is the user-facing wording; only the file path
            // (data) rides along. Upstream `message`/`done` lines are not shown.
            "stage" => progress(parsed.stage.as_deref().unwrap_or("download"), None, None),
            "file" => progress("download", None, parsed.file),
            "error" => {
                last_error = Some(sanitize_diagnostic(
                    parsed.message.as_deref().unwrap_or("download failed"),
                ));
            }
            _ => {}
        }
    });
    // Prefer the tooling's own error message over the generic non-zero exit.
    result.map_err(|exit_err| sanitize_diagnostic(&last_error.unwrap_or(exit_err)))
}

fn install_sa3(progress: &Progress, shared: &InstallShared, _update: bool) -> Result<(), String> {
    let pin = sa3_pin();
    validate_sa3_pin(&pin)?;
    let backend = host_sa3_backend()?;
    if backend == Sa3Backend::Tflite {
        validate_tflite_pin(&tflite_pin(), &pin)?;
    }
    let uv = host_uv_pin(&pin)?;
    let python = host_python_pin(&pin)?;
    let staging = crate::platform_paths::get().staging().join("sa3");
    let work = staging.join(&pin.commit);
    let candidate = work.join("candidate");
    let backup = staging.join("previous");
    let home = sa3_app_home();
    std::fs::create_dir_all(&work)
        .map_err(|error| format!("cannot create SA3 staging root: {error}"))?;
    if let Some(parent) = home.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create SA3 asset root: {error}"))?;
    }

    // Finish (or roll back) a prior process that stopped between promotion
    // renames before doing any network work.
    let install_cancelled = || shared.cancelled.load(Ordering::Acquire);
    promotion::recover(&home, &backup, |path| {
        validate_sa3_install_cancellable(path, &pin, backend, &install_cancelled)
    })?;
    cancelled(shared)?;
    build_sa3_candidate(
        progress,
        shared,
        &pin,
        backend,
        (uv, python),
        &work,
        &candidate,
    )?;
    cancelled(shared)?;
    progress("promote", None, None);
    promotion::promote(&candidate, &home, &backup, |path| {
        validate_sa3_install_cancellable(path, &pin, backend, &install_cancelled)
    })?;
    // Verified blobs are hard-linked into the promoted tree. Removing retry
    // state here reclaims only the staging directory entries, not model bytes.
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

fn build_sa3_candidate(
    progress: &Progress,
    shared: &InstallShared,
    pin: &Sa3Pin,
    backend: Sa3Backend,
    runtime_pins: (&UvPin, &PythonPin),
    work: &Path,
    candidate: &Path,
) -> Result<(), String> {
    let (uv, python) = runtime_pins;
    let blobs = work.join("blobs");
    std::fs::create_dir_all(&blobs)
        .map_err(|error| format!("cannot create SA3 blob staging: {error}"))?;
    if candidate.exists() {
        std::fs::remove_dir_all(candidate)
            .map_err(|error| format!("cannot clear interrupted SA3 candidate: {error}"))?;
    }

    let client = installer_client()?;
    progress("fetch", None, Some("Stable Audio 3 source".into()));
    let source_archive = blobs.join("stable-audio-3.tar.gz");
    download_verified(&client, &pin.source.artifact, &source_archive, None, || {
        shared.cancelled.load(Ordering::Acquire)
    })?;
    cancelled(shared)?;

    progress("extract", None, None);
    let source = std::fs::File::open(&source_archive)
        .map_err(|error| format!("cannot open verified SA3 source: {error}"))?;
    pin.source.extract(source, candidate, false, &|| {
        shared.cancelled.load(Ordering::Acquire)
    })?;
    validate_source_layout(candidate, backend)?;
    cancelled(shared)?;

    progress("fetch", None, Some(format!("uv {}", uv.version)));
    let uv_archive = blobs.join("uv.tar.gz");
    download_verified(&client, &uv.archive.artifact, &uv_archive, None, || {
        shared.cancelled.load(Ordering::Acquire)
    })?;
    let uv_dir = work.join("uv");
    if uv_dir.exists() {
        std::fs::remove_dir_all(&uv_dir)
            .map_err(|error| format!("cannot clear interrupted uv runtime: {error}"))?;
    }
    let source = std::fs::File::open(&uv_archive)
        .map_err(|error| format!("cannot open verified uv archive: {error}"))?;
    uv.archive.extract(source, &uv_dir, false, &|| {
        shared.cancelled.load(Ordering::Acquire)
    })?;
    let uv_executable = uv_dir.join(&uv.executable);
    if !uv_executable.is_file() {
        return Err("verified uv archive did not contain the pinned executable".into());
    }
    verify_uv_version(shared, &uv_executable, &uv.version)?;

    progress("fetch", None, Some(format!("Python {}", python.version)));
    let python_archive = blobs.join("python.tar.gz");
    download_verified(
        &client,
        &python.archive.artifact,
        &python_archive,
        None,
        || shared.cancelled.load(Ordering::Acquire),
    )?;
    let runtime = backend.runtime_dir(candidate);
    let python_dir = runtime.join(".python");
    let source = std::fs::File::open(&python_archive)
        .map_err(|error| format!("cannot open verified Python archive: {error}"))?;
    python.archive.extract(source, &python_dir, true, &|| {
        shared.cancelled.load(Ordering::Acquire)
    })?;
    let python_executable = python_dir.join(&python.executable);
    if !python_executable.is_file() {
        return Err("verified Python archive did not contain the pinned executable".into());
    }
    verify_python_version(shared, &python_executable, &python.version)?;

    let hf_token = std::env::var("HF_TOKEN")
        .ok()
        .or_else(|| std::env::var("HUGGING_FACE_HUB_TOKEN").ok());
    install_sa3_models(
        progress,
        shared,
        pin,
        backend,
        &blobs.join("models"),
        candidate,
        hf_token.as_deref(),
    )?;

    let portable_wheels = if backend == Sa3Backend::Tflite {
        Some(download_wheelhouse(progress, shared, work)?)
    } else {
        None
    };

    progress("install", None, None);
    let (requirements_name, requirements_lock) = backend.requirements();
    let requirements = runtime.join(requirements_name);
    write_synced(&requirements, requirements_lock.as_bytes())?;
    run_sa3_setup(
        shared,
        &uv_executable,
        &python_executable,
        &runtime,
        work,
        requirements_name,
        portable_wheels
            .as_ref()
            .map(|(directory, pins)| (directory.as_path(), pins.as_slice())),
    )?;
    warm_sa3(shared, &runtime, work, backend)?;
    if backend == Sa3Backend::Tflite {
        install_backend_sources(candidate)?;
    }
    write_source_stamp(candidate, &pinned_source())?;
    if backend == Sa3Backend::Tflite {
        write_tflite_provenance(&runtime, &tflite_pin())?;
    }
    write_synced(
        &candidate.join(INSTALL_MANIFEST_STAMP),
        install_manifest(backend).as_bytes(),
    )?;
    if backend == Sa3Backend::Tflite {
        materialize_contained_file_links(candidate)?;
        seal_sa3_candidate(candidate, pin, python)?;
    }
    validate_sa3_install_cancellable(candidate, pin, backend, &|| {
        shared.cancelled.load(Ordering::Acquire)
    })
}

fn install_sa3_models(
    progress: &Progress,
    shared: &InstallShared,
    pin: &Sa3Pin,
    backend: Sa3Backend,
    model_blobs: &Path,
    candidate: &Path,
    hf_token: Option<&str>,
) -> Result<(), String> {
    let client = installer_client()?;
    let artifacts = model_artifacts(pin, backend)?;
    for (install_path, artifact) in artifacts {
        cancelled(shared)?;
        progress(
            "fetch",
            None,
            Some(install_path.to_string_lossy().into_owned()),
        );
        let staged = model_blobs.join(&install_path);
        download_verified(&client, &artifact, &staged, hf_token, || {
            shared.cancelled.load(Ordering::Acquire)
        })?;
        link_or_copy_verified(&staged, &candidate.join(&install_path), &artifact, &|| {
            shared.cancelled.load(Ordering::Acquire)
        })?;
    }
    Ok(())
}

fn model_artifacts(
    pin: &Sa3Pin,
    backend: Sa3Backend,
) -> Result<BTreeMap<PathBuf, PinnedArtifact>, String> {
    match backend {
        Sa3Backend::Mlx => {
            let mut artifacts = BTreeMap::new();
            for model in &pin.models.artifacts {
                let filename = model.filename()?;
                let install_path = PathBuf::from("optimized/mlx/models/mlx").join(filename);
                artifacts.insert(install_path, model.artifact(&pin.models)?);
            }
            Ok(artifacts)
        }
        Sa3Backend::Tflite => tflite_artifacts(&tflite_pin()),
    }
}

fn model_download_bytes(pin: &Sa3Pin, backend: Sa3Backend) -> Result<u64, String> {
    model_artifacts(pin, backend)?
        .values()
        .try_fold(0u64, |total, artifact| {
            total
                .checked_add(artifact.size)
                .ok_or_else(|| "SA3 model download size overflow".to_string())
        })
}

fn tflite_artifacts(pin: &TflitePin) -> Result<BTreeMap<PathBuf, PinnedArtifact>, String> {
    let mut artifacts = BTreeMap::new();
    for model in pin
        .models
        .shared
        .iter()
        .chain(pin.models.bundles.values().flatten())
    {
        let install_path = checked_install_path(&model.install_path)?;
        let artifact = model.artifact(&pin.models);
        artifact.validate()?;
        if let Some(existing) = artifacts.insert(install_path.clone(), artifact.clone()) {
            if existing.url != artifact.url
                || existing.sha256 != artifact.sha256
                || existing.size != artifact.size
            {
                return Err(format!(
                    "TFLite model manifest disagrees about {}",
                    install_path.display()
                ));
            }
        }
    }
    Ok(artifacts)
}

fn checked_install_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if value.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        || !value.starts_with("models/tflite/")
    {
        return Err("TFLite model install path is unsafe".into());
    }
    Ok(PathBuf::from("optimized/tflite").join(path))
}

fn normalized_package(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '_' | '.' => '-',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

fn wheel_pins_for(target: &str) -> Result<Vec<WheelPin>, String> {
    let manifest = wheel_manifest();
    if manifest.schema_version != 1 || manifest.python != "3.11" {
        return Err("TFLite wheel manifest schema/Python version is unsupported".into());
    }
    let platform = manifest
        .targets
        .get(target)
        .ok_or_else(|| format!("no pinned TFLite wheel set exists for {target}"))?
        .clone();
    let expected_count = match target {
        "x86_64-unknown-linux-gnu" => 33,
        "x86_64-pc-windows-msvc" => 34,
        _ => return Err(format!("TFLite wheels are unsupported for {target}")),
    };
    let mut pins = manifest.common;
    pins.extend(platform);
    if pins.len() != expected_count {
        return Err(format!(
            "TFLite wheel manifest has {} artifacts; expected {expected_count}",
            pins.len()
        ));
    }

    let lock = TFLITE_REQUIREMENTS_LOCK.to_ascii_lowercase();
    let mut packages = BTreeSet::new();
    let mut filenames = BTreeSet::new();
    for pin in &pins {
        pin.artifact.validate()?;
        let package = normalized_package(&pin.package);
        if package.is_empty()
            || pin.version.is_empty()
            || !packages.insert(package.clone())
            || !filenames.insert(pin.filename.clone())
            || pin.filename.contains('/')
            || pin.filename.contains('\\')
            || pin.filename == "."
            || pin.filename == ".."
            || !pin.filename.ends_with(".whl")
            || !pin
                .artifact
                .url
                .starts_with("https://files.pythonhosted.org/")
            || !pin.artifact.url.ends_with(&pin.filename)
            || !lock.contains(&format!(
                "{}=={}",
                package,
                pin.version.to_ascii_lowercase()
            ))
            || !lock.contains(&format!("--hash=sha256:{}", pin.artifact.sha256))
        {
            return Err(format!(
                "TFLite wheel pin is unsafe or disagrees with the lock: {}",
                pin.filename
            ));
        }
    }
    pins.sort_by(|left, right| left.filename.cmp(&right.filename));
    Ok(pins)
}

fn snapshot_artifact(
    snapshot: &SnapshotPin,
    file: &SnapshotFilePin,
) -> Result<PinnedArtifact, String> {
    if file.path.is_empty()
        || file.path.contains('/')
        || file.path.contains('\\')
        || file.path == "."
        || file.path == ".."
    {
        return Err("MRT2 snapshot artifact path is unsafe".into());
    }
    let artifact = PinnedArtifact {
        url: format!(
            "https://huggingface.co/{}/resolve/{}/{}?download=true",
            snapshot.repository, snapshot.revision, file.path
        ),
        sha256: file.sha256.clone(),
        size: file.size,
    };
    artifact.validate()?;
    Ok(artifact)
}

fn mrt2_lock_for(target: &str) -> Result<&'static str, String> {
    match target {
        "x86_64-unknown-linux-gnu" => Ok(MRT2_LINUX_LOCK),
        "x86_64-pc-windows-msvc" => Ok(MRT2_WINDOWS_LOCK),
        _ => Err(format!("MRT2 is unsupported for {target}")),
    }
}

fn mrt2_wheel_pins_for(target: &str) -> Result<Vec<WheelPin>, String> {
    let manifest = mrt2_wheel_manifest();
    if manifest.schema_version != 1 || manifest.python != "3.12" {
        return Err("MRT2 wheel manifest schema/Python version is unsupported".into());
    }
    let pins = manifest
        .targets
        .get(target)
        .ok_or_else(|| format!("no pinned MRT2 wheel set exists for {target}"))?
        .clone();
    let expected_count = match target {
        "x86_64-unknown-linux-gnu" => 56,
        "x86_64-pc-windows-msvc" => 38,
        _ => return Err(format!("MRT2 wheels are unsupported for {target}")),
    };
    if pins.len() != expected_count {
        return Err(format!(
            "MRT2 wheel manifest must contain {expected_count} artifacts"
        ));
    }
    let lock = mrt2_lock_for(target)?.to_ascii_lowercase();
    let mut packages = BTreeSet::new();
    let mut filenames = BTreeSet::new();
    for pin in &pins {
        pin.artifact.validate()?;
        let package = normalized_package(&pin.package);
        let trusted_url = pin
            .artifact
            .url
            .starts_with("https://files.pythonhosted.org/")
            || pin
                .artifact
                .url
                .starts_with("https://download-r2.pytorch.org/");
        if !trusted_url
            || !packages.insert(package.clone())
            || !filenames.insert(pin.filename.clone())
            || pin.filename.contains('/')
            || pin.filename.contains('\\')
            || !pin.filename.ends_with(".whl")
            || !lock.contains(&format!(
                "{}=={}",
                package,
                pin.version.to_ascii_lowercase()
            ))
            || !lock.contains(&format!("--hash=sha256:{}", pin.artifact.sha256))
        {
            return Err(format!(
                "MRT2 wheel pin disagrees with its lock: {}",
                pin.filename
            ));
        }
    }
    Ok(pins)
}

fn validate_snapshot(snapshot: &SnapshotPin, expected: &BTreeSet<&str>) -> Result<(), String> {
    if snapshot.revision.len() != 40
        || !snapshot
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || snapshot.repository.split('/').count() != 2
    {
        return Err("MRT2 snapshot repository/revision is invalid".into());
    }
    let mut actual = BTreeSet::new();
    for file in &snapshot.files {
        snapshot_artifact(snapshot, file)?;
        if !actual.insert(file.path.as_str()) {
            return Err("MRT2 snapshot contains a duplicate artifact".into());
        }
    }
    if &actual != expected {
        return Err("MRT2 snapshot artifact inventory is incomplete".into());
    }
    Ok(())
}

fn validate_mrt2_pin(pin: &Mrt2Pin) -> Result<(), String> {
    if pin.schema_version != 1
        || content_digest(MRT2_WHEEL_PIN_JSON.as_bytes()) != pin.runtime.wheel_manifest_sha256
    {
        return Err("MRT2 runtime manifest identity is invalid".into());
    }
    for target in ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"] {
        let lock = mrt2_lock_for(target)?;
        if pin.runtime.locks.get(target) != Some(&content_digest(lock.as_bytes())) {
            return Err("MRT2 dependency lock digest is stale".into());
        }
        mrt2_wheel_pins_for(target)?;
        let python = pin
            .runtime
            .python
            .iter()
            .find(|item| item.target == target)
            .ok_or("MRT2 Python runtime pin is missing")?;
        let uv = pin
            .runtime
            .uv
            .iter()
            .find(|item| item.target == target)
            .ok_or("MRT2 uv runtime pin is missing")?;
        python.archive.artifact.validate()?;
        uv.archive.artifact.validate()?;
        if !python.version.starts_with("3.12.") || uv.version != "0.11.7" {
            return Err("MRT2 runtime tool version is inconsistent".into());
        }
    }
    let model_files = [
        "aoti.py",
        "codec_shapes.json",
        "config.json",
        "configuration_magenta_rt2.py",
        "cudagraph.py",
        "depthformer.py",
        "layers.py",
        "model.safetensors",
        "modeling_magenta_rt2.py",
        "musiccoca.py",
        "processing_musiccoca.py",
        "spectrostream.py",
    ]
    .into_iter()
    .collect();
    if pin.models.keys().cloned().collect::<BTreeSet<_>>()
        != ["mrt2_base".to_string(), "mrt2_small".to_string()]
            .into_iter()
            .collect()
    {
        return Err("MRT2 model catalog is incomplete".into());
    }
    for snapshot in pin.models.values() {
        validate_snapshot(snapshot, &model_files)?;
    }
    let processor_files = [
        "mel_params.npz",
        "music_encoder.pt",
        "quantizer.pt",
        "spm.model",
        "text_encoder.pt",
    ]
    .into_iter()
    .collect();
    validate_snapshot(&pin.processor, &processor_files)
}

fn verify_wheelhouse(
    directory: &Path,
    pins: &[WheelPin],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    let expected = pins
        .iter()
        .map(|pin| pin.filename.clone())
        .collect::<BTreeSet<_>>();
    let entries = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot enumerate TFLite wheelhouse: {error}"))?;
    let mut actual = BTreeSet::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot enumerate TFLite wheelhouse: {error}"))?;
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| format!("cannot inspect TFLite wheel: {error}"))?;
        let Some(filename) = entry.file_name().to_str().map(str::to_owned) else {
            return Err("TFLite wheel filename is not UTF-8".into());
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || !actual.insert(filename) {
            return Err("TFLite wheelhouse contains a non-regular or duplicate entry".into());
        }
    }
    if actual != expected {
        return Err("TFLite wheelhouse contains missing or unexpected artifacts".into());
    }
    for pin in pins {
        verify_file_cancellable(&directory.join(&pin.filename), &pin.artifact, is_cancelled)
            .map_err(|error| {
                format!("TFLite wheel {} failed verification: {error}", pin.filename)
            })?;
    }
    Ok(())
}

fn download_wheelhouse(
    progress: &Progress,
    shared: &InstallShared,
    work: &Path,
) -> Result<(PathBuf, Vec<WheelPin>), String> {
    let pins = wheel_pins_for(host_installer_target()?)?;
    let directory = work.join("wheelhouse");
    if directory.exists() {
        std::fs::remove_dir_all(&directory)
            .map_err(|error| format!("cannot clear interrupted TFLite wheelhouse: {error}"))?;
    }
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create TFLite wheelhouse: {error}"))?;
    let client = installer_client()?;
    for pin in &pins {
        cancelled(shared)?;
        progress("fetch", None, Some(pin.filename.clone()));
        download_verified(
            &client,
            &pin.artifact,
            &directory.join(&pin.filename),
            None,
            || shared.cancelled.load(Ordering::Acquire),
        )?;
    }
    verify_wheelhouse(&directory, &pins, &|| {
        shared.cancelled.load(Ordering::Acquire)
    })?;
    Ok((directory, pins))
}

fn verify_uv_version(
    shared: &InstallShared,
    executable: &Path,
    expected: &str,
) -> Result<(), String> {
    let mut command = Command::new(executable);
    command.arg("--version");
    let mut output = String::new();
    stream_child(shared, "verify-uv", command, |line| {
        if output.is_empty() {
            output.push_str(line);
        }
    })?;
    let actual = output.split_whitespace().nth(1).unwrap_or_default();
    if actual != expected {
        return Err(format!(
            "verified uv executable reported version {actual:?}, expected {expected}"
        ));
    }
    Ok(())
}

fn verify_python_version(
    shared: &InstallShared,
    executable: &Path,
    expected: &str,
) -> Result<(), String> {
    let mut command = Command::new(executable);
    command.args(["-c", "import platform; print(platform.python_version())"]);
    let mut output = String::new();
    stream_child(shared, "verify-python", command, |line| {
        if output.is_empty() {
            output.push_str(line);
        }
    })?;
    if output != expected {
        return Err(format!(
            "verified Python executable reported version {output:?}, expected {expected}"
        ));
    }
    Ok(())
}

fn run_sa3_setup(
    shared: &InstallShared,
    uv: &Path,
    runtime_python: &Path,
    runtime_dir: &Path,
    work: &Path,
    requirements_name: &str,
    portable_wheels: Option<(&Path, &[WheelPin])>,
) -> Result<(), String> {
    let venv = runtime_dir.join(".venv");
    let cache = work.join("uv-cache");

    let mut create_venv = uv_command(uv, runtime_dir, &cache);
    create_venv.args([
        "venv",
        "--relocatable",
        "--no-managed-python",
        "--no-python-downloads",
        "--no-config",
        "--link-mode",
        "copy",
        "--python",
    ]);
    create_venv.arg(runtime_python);
    create_venv.arg(&venv);
    stream_child(shared, "sa3-venv", create_venv, |_| {})?;

    let python = crate::platform_paths::venv_python(&venv);
    if !python.is_file() {
        return Err("uv did not create the platform virtual-environment interpreter".into());
    }
    let mut install_dependencies = uv_command(uv, runtime_dir, &cache);
    install_dependencies
        .args(["pip", "install", "--python"])
        .arg(&python);
    if let Some((wheelhouse, pins)) = portable_wheels {
        configure_portable_install(&mut install_dependencies, wheelhouse, pins, &|| {
            shared.cancelled.load(Ordering::Acquire)
        })?;
    } else {
        let requirements = runtime_dir.join(requirements_name);
        install_dependencies
            .args([
                "--require-hashes",
                "--only-binary",
                ":all:",
                "--link-mode",
                "copy",
                "--default-index",
                "https://pypi.org/simple",
                "--no-config",
                "-r",
            ])
            .arg(&requirements);
    }
    stream_child(shared, "sa3-dependencies", install_dependencies, |_| {})
}

fn configure_portable_install(
    command: &mut Command,
    wheelhouse: &Path,
    pins: &[WheelPin],
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    verify_wheelhouse(wheelhouse, pins, is_cancelled)?;
    command
        .args([
            "--offline",
            "--no-index",
            "--no-deps",
            "--only-binary",
            ":all:",
            "--link-mode",
            "copy",
            "--no-config",
        ])
        .env("UV_OFFLINE", "1")
        .env("UV_NO_INDEX", "1")
        .env_remove("ALL_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("HTTP_PROXY");
    for pin in pins {
        command.arg(wheelhouse.join(&pin.filename));
    }
    Ok(())
}

fn uv_command(uv: &Path, cwd: &Path, cache: &Path) -> Command {
    let mut command = Command::new(uv);
    command
        .current_dir(cwd)
        .env("UV_CACHE_DIR", cache)
        .env_remove("UV_INSECURE_HOST")
        .env_remove("UV_INDEX")
        .env_remove("UV_INDEX_URL")
        .env_remove("UV_EXTRA_INDEX_URL")
        .env_remove("UV_NO_VERIFY_HASHES")
        .env_remove("PIP_INDEX_URL")
        .env_remove("PIP_EXTRA_INDEX_URL")
        .env_remove("PIP_TRUSTED_HOST");
    command
}

fn install_backend_sources(candidate: &Path) -> Result<(), String> {
    let package = candidate.join("lsdj_backend").join("lsdj");
    for (filename, bytes) in BACKEND_SOURCES {
        write_synced(&package.join(filename), bytes)?;
    }
    write_synced(
        &candidate.join("lsdj_backend").join("launch.py"),
        b"from lsdj.frozen import main\nmain()\n",
    )
}

fn relative_wire(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "managed SA3 launcher escaped its candidate")?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            std::path::Component::Normal(value) => parts.push(
                value
                    .to_str()
                    .ok_or("managed SA3 launcher path is not UTF-8")?,
            ),
            _ => return Err("managed SA3 launcher path is unsafe".into()),
        }
    }
    if parts.is_empty() {
        return Err("managed SA3 launcher path is empty".into());
    }
    Ok(parts.join("/"))
}

fn content_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

fn seal_sa3_candidate(
    candidate: &Path,
    pin: &Sa3Pin,
    python_pin: &PythonPin,
) -> Result<(), String> {
    let runtime = Sa3Backend::Tflite.runtime_dir(candidate);
    let program = crate::platform_paths::venv_python(&runtime.join(".venv"));
    let mut provenance = BTreeMap::new();
    provenance.insert("source.repository".into(), pin.repo.clone());
    provenance.insert("source.revision".into(), pin.commit.clone());
    provenance.insert("source.sha256".into(), pin.source.artifact.sha256.clone());
    provenance.insert("python.version".into(), python_pin.version.clone());
    provenance.insert(
        "python.sha256".into(),
        python_pin.archive.artifact.sha256.clone(),
    );
    provenance.insert(
        "requirements.sha256".into(),
        content_digest(TFLITE_REQUIREMENTS_LOCK.as_bytes()),
    );
    provenance.insert(
        "wheels.sha256".into(),
        content_digest(TFLITE_WHEEL_PIN_JSON.as_bytes()),
    );
    let portable = tflite_pin();
    provenance.insert("models.repository".into(), portable.models.repo);
    provenance.insert("models.revision".into(), portable.models.revision);

    let environment = [
        ("DO_NOT_TRACK", "1"),
        ("HF_HUB_DISABLE_TELEMETRY", "1"),
        ("HF_HUB_OFFLINE", "1"),
        ("NO_COLOR", "1"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("PYTHONNOUSERSITE", "1"),
        ("PYTHONUTF8", "1"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    let ephemeral_environment = [
        "LSDJ_API_CAPABILITY",
        "LSDJ_ASSETS_HOME",
        "LSDJ_CACHE_HOME",
        "LSDJ_CONFIG_HOME",
        "LSDJ_DATA_HOME",
        "LSDJ_STAGING_HOME",
        "MAGENTA_HOME",
        "SA3_HOME",
        "SA3_LORAS_HOME",
        "SA3_MLX_HOME",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let spec = crate::managed_runtime::CommandSpec {
        program: relative_wire(candidate, &program)?,
        argv: vec!["launch.py".into(), "--generation-server".into()],
        cwd: "lsdj_backend".into(),
        environment,
        ephemeral_environment,
    };
    crate::managed_runtime::seal_candidate(
        candidate,
        &crate::managed_runtime::host_target(),
        provenance,
        [(
            crate::managed_runtime::Service::Sa3.wire_name().into(),
            spec,
        )]
        .into_iter()
        .collect(),
    )?;
    Ok(())
}

fn materialize_contained_file_links(root: &Path) -> Result<(), String> {
    fn collect(directory: &Path, links: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in std::fs::read_dir(directory)
            .map_err(|error| format!("cannot scan managed runtime links: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("cannot scan managed runtime links: {error}"))?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| format!("cannot inspect managed runtime link: {error}"))?;
            if metadata.file_type().is_symlink() {
                links.push(path);
            } else if metadata.is_dir() {
                collect(&path, links)?;
            } else if !metadata.is_file() {
                return Err("managed runtime contains an unsupported filesystem entry".into());
            }
        }
        Ok(())
    }

    let canonical_root = std::fs::canonicalize(root)
        .map_err(|error| format!("cannot canonicalize managed runtime candidate: {error}"))?;
    let mut links = Vec::new();
    collect(root, &mut links)?;
    for link in links {
        let target = std::fs::canonicalize(&link)
            .map_err(|error| format!("cannot resolve managed runtime link: {error}"))?;
        let target_metadata = std::fs::metadata(&target)
            .map_err(|error| format!("cannot inspect managed runtime link target: {error}"))?;
        if !target.starts_with(&canonical_root) || !target_metadata.is_file() {
            return Err("managed runtime link is not a contained regular file".into());
        }
        let replacement =
            link.with_file_name(format!(".lsdj-materialize-{:032x}", rand::random::<u128>()));
        std::fs::copy(&target, &replacement)
            .map_err(|error| format!("cannot materialize managed runtime link: {error}"))?;
        std::fs::remove_file(&link)
            .map_err(|error| format!("cannot replace managed runtime link: {error}"))?;
        std::fs::rename(&replacement, &link)
            .map_err(|error| format!("cannot commit materialized runtime file: {error}"))?;
    }
    Ok(())
}

#[cfg(feature = "managed-runtime")]
fn seal_mrt2_candidate(candidate: &Path, pin: &Mrt2Pin, python: &PythonPin) -> Result<(), String> {
    let program = crate::platform_paths::venv_python(&candidate.join("runtime").join(".venv"));
    let mut provenance = BTreeMap::new();
    provenance.insert(
        "runtime.pin.sha256".into(),
        content_digest(MRT2_PIN_JSON.as_bytes()),
    );
    provenance.insert(
        "runtime.wheels.sha256".into(),
        content_digest(MRT2_WHEEL_PIN_JSON.as_bytes()),
    );
    provenance.insert("python.version".into(), python.version.clone());
    provenance.insert(
        "python.sha256".into(),
        python.archive.artifact.sha256.clone(),
    );
    provenance.insert(
        "processor.repository".into(),
        pin.processor.repository.clone(),
    );
    provenance.insert("processor.revision".into(), pin.processor.revision.clone());
    for (name, snapshot) in &pin.models {
        provenance.insert(
            format!("model.{name}.repository"),
            snapshot.repository.clone(),
        );
        provenance.insert(format!("model.{name}.revision"), snapshot.revision.clone());
    }
    let environment = [
        ("DO_NOT_TRACK", "1"),
        ("HF_HUB_DISABLE_TELEMETRY", "1"),
        ("HF_HUB_OFFLINE", "1"),
        ("NO_COLOR", "1"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("PYTHONNOUSERSITE", "1"),
        ("PYTHONUTF8", "1"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    let ephemeral_environment = [
        "LSDJ_API_CAPABILITY",
        "LSDJ_ASSETS_HOME",
        "LSDJ_CACHE_HOME",
        "LSDJ_CONFIG_HOME",
        "LSDJ_DATA_HOME",
        "LSDJ_STAGING_HOME",
        "MAGENTA_HOME",
        "SA3_HOME",
        "SA3_LORAS_HOME",
        "SA3_MLX_HOME",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let spec = crate::managed_runtime::CommandSpec {
        program: relative_wire(candidate, &program)?,
        argv: vec!["launch.py".into()],
        cwd: "lsdj_backend".into(),
        environment,
        ephemeral_environment,
    };
    crate::managed_runtime::seal_candidate(
        candidate,
        &crate::managed_runtime::host_target(),
        provenance,
        [(
            crate::managed_runtime::Service::Mrt2.wire_name().into(),
            spec,
        )]
        .into_iter()
        .collect(),
    )?;
    Ok(())
}

fn warm_sa3(
    shared: &InstallShared,
    runtime_dir: &Path,
    work: &Path,
    backend: Sa3Backend,
) -> Result<(), String> {
    let python = crate::platform_paths::venv_python(&runtime_dir.join(".venv"));
    let script = runtime_dir.join("scripts").join(backend.script());
    let warm_dir = work.join("warm");
    if warm_dir.exists() {
        std::fs::remove_dir_all(&warm_dir)
            .map_err(|error| format!("cannot clear interrupted SA3 warm-up: {error}"))?;
    }
    std::fs::create_dir_all(&warm_dir)
        .map_err(|error| format!("cannot create SA3 warm-up directory: {error}"))?;
    for (dit, decoder) in [
        ("sm-sfx", "same-s"),
        ("sm-music", "same-s"),
        ("medium", "same-l"),
    ] {
        cancelled(shared)?;
        let output = warm_dir.join(format!("{dit}.wav"));
        let mut command = Command::new(&python);
        command
            .current_dir(runtime_dir)
            // All required weights were downloaded and verified by Rust. Force
            // the upstream helper offline so it cannot silently fetch a mutable
            // replacement during candidate validation.
            .env("HF_HUB_OFFLINE", "1")
            .arg(&script)
            .args([
                "--prompt",
                "setup warm-up",
                "--dit",
                dit,
                "--decoder",
                decoder,
                "--seconds",
                "1",
                "--steps",
                "1",
                "--out",
            ])
            .arg(&output);
        if backend == Sa3Backend::Tflite {
            command.args(["--precision", "fp32", "--threads", "4"]);
        }
        stream_child(shared, "sa3-warm", command, |_| {})?;
        if !output.is_file() {
            return Err(format!("SA3 warm-up did not produce {dit} output"));
        }
    }
    write_synced(&runtime_dir.join(WARMED_STAMP), b"")?;
    let _ = std::fs::remove_dir_all(warm_dir);
    Ok(())
}

fn validate_source_layout(checkout: &Path, backend: Sa3Backend) -> Result<(), String> {
    let runtime = backend.runtime_dir(checkout);
    if !runtime.is_dir() || !runtime.join("scripts").join(backend.script()).is_file() {
        return Err("verified source archive has an unexpected SA3 layout".into());
    }
    Ok(())
}

fn validate_sa3_install(checkout: &Path, pin: &Sa3Pin, backend: Sa3Backend) -> Result<(), String> {
    validate_sa3_install_cancellable(checkout, pin, backend, &|| false)
}

fn validate_sa3_install_cancellable(
    checkout: &Path,
    pin: &Sa3Pin,
    backend: Sa3Backend,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    if is_cancelled() {
        return Err("cancelled".into());
    }
    validate_source_layout(checkout, backend)?;
    let runtime = backend.runtime_dir(checkout);
    let python = crate::platform_paths::venv_python(&runtime.join(".venv"));
    if !python.is_file() {
        return Err("SA3 virtual-environment interpreter is missing".into());
    }
    if !runtime.join(WARMED_STAMP).is_file() {
        return Err("SA3 warm-up stamp is missing".into());
    }
    let runtime_python = host_python_pin(pin)?;
    if !runtime
        .join(".python")
        .join(&runtime_python.executable)
        .is_file()
    {
        return Err("pinned SA3 Python runtime is missing".into());
    }
    if read_source_stamp(checkout).as_ref() != Some(&pinned_source()) {
        return Err("SA3 source provenance does not match the pin".into());
    }
    let manifest_path = checkout.join(INSTALL_MANIFEST_STAMP);
    let manifest_metadata = std::fs::symlink_metadata(&manifest_path)
        .map_err(|error| format!("cannot inspect SA3 install manifest: {error}"))?;
    if manifest_metadata.file_type().is_symlink() || !manifest_metadata.is_file() {
        return Err("SA3 install manifest is not a regular file".into());
    }
    let installed_manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read SA3 install manifest: {error}"))?;
    if installed_manifest != install_manifest(backend) {
        return Err("SA3 install manifest does not match this application".into());
    }
    if backend == Sa3Backend::Tflite {
        validate_tflite_provenance(&runtime, &tflite_pin())?;
    }
    validate_sa3_model_artifacts(checkout, pin, backend, is_cancelled)?;
    if backend == Sa3Backend::Tflite {
        crate::managed_runtime::validate_candidate(checkout, crate::managed_runtime::Service::Sa3)?;
    }
    Ok(())
}

fn validate_sa3_model_artifacts(
    checkout: &Path,
    pin: &Sa3Pin,
    backend: Sa3Backend,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(), String> {
    for (install_path, artifact) in model_artifacts(pin, backend)? {
        if let Err(error) =
            verify_file_cancellable(&checkout.join(&install_path), &artifact, is_cancelled)
        {
            if error == "cancelled" {
                return Err(error);
            }
            return Err(format!(
                "SA3 model {} failed integrity validation: {error}",
                install_path.display()
            ));
        }
    }
    Ok(())
}

fn install_manifest(backend: Sa3Backend) -> &'static str {
    match backend {
        Sa3Backend::Mlx => SA3_PIN_JSON,
        Sa3Backend::Tflite => TFLITE_PIN_JSON,
    }
}

fn tflite_provenance(pin: &TflitePin) -> serde_json::Value {
    serde_json::json!({
        "runtime": {
            "repo": pin.runtime.repo,
            "revision": pin.runtime.revision,
        },
        "models": {
            "repo": pin.models.repo,
            "revision": pin.models.revision,
        },
    })
}

fn write_tflite_provenance(runtime: &Path, pin: &TflitePin) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(&tflite_provenance(pin))
        .map_err(|error| format!("cannot serialize TFLite provenance: {error}"))?;
    write_synced(&runtime.join(TFLITE_PROVENANCE_STAMP), &bytes)
}

fn validate_tflite_provenance(runtime: &Path, pin: &TflitePin) -> Result<(), String> {
    let bytes = std::fs::read(runtime.join(TFLITE_PROVENANCE_STAMP))
        .map_err(|error| format!("cannot read TFLite provenance: {error}"))?;
    let actual: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("TFLite provenance is invalid: {error}"))?;
    if actual != tflite_provenance(pin) {
        return Err("TFLite runtime/model provenance does not match the application pin".into());
    }
    Ok(())
}

fn validate_sa3_pin(pin: &Sa3Pin) -> Result<(), String> {
    if pin.commit.len() != 40 || !pin.commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("SA3 source revision must be a full immutable commit".into());
    }
    pin.source.artifact.validate()?;
    if !pin.source.artifact.url.contains(&pin.commit)
        || !pin.source.archive_root.ends_with(&pin.commit)
        || pin.source.max_files == 0
        || pin.source.max_expanded_bytes == 0
    {
        return Err("SA3 source archive metadata does not match its revision".into());
    }
    if pin.runtime.requirements != "sa3-requirements.lock" {
        return Err("SA3 runtime pin is incomplete".into());
    }
    for uv in &pin.runtime.uv {
        uv.archive.artifact.validate()?;
        if uv.target.is_empty()
            || uv.version.is_empty()
            || !uv.archive.artifact.url.contains(&uv.version)
            || uv.executable.is_empty()
            || uv.executable.contains('/')
            || uv.executable.contains('\\')
            || !matches!(uv.archive.archive_format.as_str(), "tar.gz" | "zip")
            || (uv.archive.archive_format == "zip" && !uv.archive.artifact.url.ends_with(".zip"))
        {
            return Err("uv runtime pin is inconsistent".into());
        }
    }
    for python in &pin.runtime.python {
        python.archive.artifact.validate()?;
        if python.target.is_empty()
            || python.version.split('.').count() != 3
            || !python.archive.artifact.url.contains(&python.version)
            || python.executable.is_empty()
            || python.executable.starts_with('/')
            || python.executable.contains("..")
        {
            return Err("Python runtime pin is inconsistent".into());
        }
    }
    if pin.models.revision.len() != 40
        || !pin
            .models
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || pin.models.repo.split('/').count() != 2
        || pin.models.repo.contains("..")
    {
        return Err("SA3 model revision/repository pin is invalid".into());
    }
    let expected: std::collections::BTreeSet<_> = [
        "dit_medium_f16.npz",
        "dit_sm-music_f16.npz",
        "dit_sm-sfx_f16.npz",
        "same_l_decoder_f32.npz",
        "same_l_encoder_f32.npz",
        "same_s_decoder_f32.npz",
        "same_s_encoder_f32.npz",
        "t5gemma_f16.npz",
    ]
    .into_iter()
    .collect();
    let mut actual = std::collections::BTreeSet::new();
    for model in &pin.models.artifacts {
        let filename = model.filename()?;
        model.artifact(&pin.models)?.validate()?;
        if !actual.insert(filename) {
            return Err("SA3 model manifest contains a duplicate artifact".into());
        }
    }
    if actual != expected {
        return Err("SA3 model manifest does not cover every inference artifact".into());
    }
    Ok(())
}

fn validate_tflite_pin(pin: &TflitePin, source: &Sa3Pin) -> Result<(), String> {
    if pin.schema_version != 1
        || pin.runtime.repo != source.repo
        || pin.runtime.revision != source.commit
        || pin.runtime.subdirectory != "optimized/tflite"
        || pin.runtime.entrypoint != "scripts/sa3_tflite.py"
        || pin.runtime.requirements_lock != "scripts/sa3-tflite-requirements.lock"
        || pin.models.revision.len() != 40
        || !pin
            .models
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || pin.models.repo.split('/').count() != 2
        || pin.models.precision != "fp32"
    {
        return Err("TFLite runtime/model pin is inconsistent".into());
    }
    let artifacts = tflite_artifacts(pin)?;
    if artifacts.len() != 8 {
        return Err("TFLite manifest must cover exactly eight unique model artifacts".into());
    }
    let expected: std::collections::BTreeSet<_> = [
        "optimized/tflite/models/tflite/t5gemma/encoder_fp16.tflite",
        "optimized/tflite/models/tflite/sa3-sm-music/dit_fp32.tflite",
        "optimized/tflite/models/tflite/sa3-sm-sfx/dit_fp32.tflite",
        "optimized/tflite/models/tflite/sa3-m/dit_fp32.tflite",
        "optimized/tflite/models/tflite/same-s/enc_fp32.tflite",
        "optimized/tflite/models/tflite/same-s/dec_fp32.tflite",
        "optimized/tflite/models/tflite/same-l/enc_fp32.tflite",
        "optimized/tflite/models/tflite/same-l/dec_fp32.tflite",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    if artifacts
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != expected
    {
        return Err("TFLite manifest does not cover every inference artifact".into());
    }
    wheel_pins_for("x86_64-unknown-linux-gnu")?;
    wheel_pins_for("x86_64-pc-windows-msvc")?;
    Ok(())
}

fn host_uv_pin(pin: &Sa3Pin) -> Result<&UvPin, String> {
    let target = host_installer_target()?;
    let uv = pin
        .runtime
        .uv
        .iter()
        .find(|artifact| artifact.target == target)
        .ok_or("no pinned uv runtime exists for this platform")?;
    uv.archive.artifact.validate()?;
    if uv.version.is_empty()
        || !uv.archive.artifact.url.contains(&uv.version)
        || uv.executable.contains('/')
        || uv.executable.contains('\\')
        || uv.executable.is_empty()
    {
        return Err("uv runtime pin is inconsistent".into());
    }
    Ok(uv)
}

fn host_python_pin(pin: &Sa3Pin) -> Result<&PythonPin, String> {
    let target = host_installer_target()?;
    pin.runtime
        .python
        .iter()
        .find(|artifact| artifact.target == target)
        .ok_or_else(|| "no pinned Python runtime exists for this platform".into())
}

fn host_installer_target() -> Result<&'static str, String> {
    installer_target_for(std::env::consts::OS, std::env::consts::ARCH)
}

fn installer_target_for(os: &str, arch: &str) -> Result<&'static str, String> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => Err(format!("no pinned SA3 runtime exists for {os}/{arch}")),
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create install metadata directory: {error}"))?;
    }
    let mut file = std::fs::File::create(path)
        .map_err(|error| format!("cannot create install metadata: {error}"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot sync install metadata: {error}"))
}

// --- Tauri commands --------------------------------------------------------

#[tauri::command]
pub fn model_status(installer: tauri::State<'_, InstallManager>) -> ModelStatus {
    status(installer.active_install())
}

#[tauri::command]
pub fn install_model(
    installer: tauri::State<'_, InstallManager>,
    app: AppHandle,
    family: Family,
    name: Option<String>,
) -> Result<(), String> {
    installer.install(app, family, name, false)
}

/// Update an installed family in place to the pinned source. For SA3 this
/// re-fetches the pinned checkout (swapping it in) and rebuilds + re-warms;
/// progress and completion arrive on the same `model://progress` /
/// `models://changed` channels as an install.
#[tauri::command]
pub fn update_model(
    installer: tauri::State<'_, InstallManager>,
    app: AppHandle,
    family: Family,
) -> Result<(), String> {
    // Update is SA3-only: it re-fetches the pinned checkout. Magenta models are
    // versioned individually and have no pinned-source/update path.
    if family != Family::Sa3 {
        return Err("update is only supported for Stable Audio 3".into());
    }
    installer.install(app, family, None, true)
}

#[tauri::command]
pub fn cancel_install(installer: tauri::State<'_, InstallManager>) {
    installer.cancel();
}

/// Reveal a family's folder in the OS file manager so the user can inspect or
/// remove models natively (in-app deletion is intentionally absent for the two
/// model families — moving multi-GB weights to the Trash fails on
/// iCloud-managed / dataless files; adapters are small and DO get an in-app
/// delete — and the watcher reflects a native delete live anyway). Magenta
/// opens its models dir; SA3 opens its checkout (or the app-owned SA3 home if
/// not installed yet); LoRA opens the adapter registry. Creates the folder if
/// it does not exist.
#[tauri::command]
pub fn open_model_folder(app: AppHandle, family: Family) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = match family {
        Family::Magenta => magenta_models_dir(),
        Family::Sa3 => sa3_status().1.unwrap_or_else(sa3_app_home),
        Family::Lora => crate::loras::loras_dir(),
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create folder: {e}"))?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| format!("cannot open folder: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn discover_lists_only_folders_with_both_files() {
        let tmp = std::env::temp_dir().join(format!("lsdj-models-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // Complete model.
        touch(&tmp.join("mrt2_small").join("mrt2_small.mlxfn"));
        touch(&tmp.join("mrt2_small").join("mrt2_small_state.safetensors"));
        // Partial model (missing the safetensors) — must not appear.
        touch(&tmp.join("half").join("half.mlxfn"));
        // A drop-in model with an unknown name — must appear.
        touch(&tmp.join("custom_x").join("custom_x.mlxfn"));
        touch(&tmp.join("custom_x").join("custom_x_state.safetensors"));

        assert_eq!(
            discover_installed(&tmp),
            vec!["custom_x".to_string(), "mrt2_small".to_string()]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dir_size_sums_files_recursively() {
        let tmp = std::env::temp_dir().join(format!("lsdj-size-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(tmp.join("sub").join("b.bin"), vec![0u8; 50]).unwrap();
        assert_eq!(dir_size(&tmp), 150);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sa3_pin_parses() {
        let pin = sa3_pin();
        assert!(pin.repo.starts_with("https://"));
        validate_sa3_pin(&pin).unwrap();
        assert_eq!(pin.commit.len(), 40);
        assert_eq!(pin.models.artifacts.len(), 8);
        assert!(pin.source.artifact.url.starts_with("https://"));

        let portable = tflite_pin();
        validate_tflite_pin(&portable, &pin).unwrap();
        assert_eq!(tflite_artifacts(&portable).unwrap().len(), 8);
        assert_eq!(
            model_download_bytes(&pin, Sa3Backend::Tflite).unwrap(),
            14_138_994_904
        );
    }

    #[test]
    fn mrt2_runtime_models_remote_code_and_wheels_are_fully_pinned() {
        let pin = mrt2_pin();
        validate_mrt2_pin(&pin).unwrap();
        assert_eq!(
            mrt2_wheel_pins_for("x86_64-unknown-linux-gnu")
                .unwrap()
                .len(),
            56
        );
        assert_eq!(
            mrt2_wheel_pins_for("x86_64-pc-windows-msvc").unwrap().len(),
            38
        );
        for (name, snapshot) in &pin.models {
            assert_eq!(snapshot.revision.len(), 40);
            assert!(snapshot
                .files
                .iter()
                .any(|file| file.path == "model.safetensors"));
            for required_code in [
                "configuration_magenta_rt2.py",
                "modeling_magenta_rt2.py",
                "depthformer.py",
                "layers.py",
                "musiccoca.py",
                "processing_musiccoca.py",
                "spectrostream.py",
                "cudagraph.py",
                "aoti.py",
            ] {
                assert!(
                    snapshot.files.iter().any(|file| file.path == required_code),
                    "{name} omits executable remote-code artifact {required_code}"
                );
            }
            for file in &snapshot.files {
                let artifact = snapshot_artifact(snapshot, file).unwrap();
                assert!(artifact.url.contains(&snapshot.revision));
                assert!(!artifact.url.contains("/resolve/main/"));
            }
        }
        assert_eq!(pin.processor.files.len(), 5);
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_materializes_contained_file_links_and_rejects_escapes() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "lsdj-materialize-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        std::fs::write(root.join("runtime").join("python"), b"runtime").unwrap();
        symlink("python", root.join("runtime").join("python3")).unwrap();
        materialize_contained_file_links(&root).unwrap();
        let materialized = root.join("runtime").join("python3");
        assert!(!std::fs::symlink_metadata(&materialized)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&materialized).unwrap(), b"runtime");

        let outside = root.with_extension("outside");
        std::fs::write(&outside, b"outside").unwrap();
        symlink(&outside, root.join("escape")).unwrap();
        assert!(materialize_contained_file_links(&root)
            .unwrap_err()
            .contains("contained regular file"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(outside);
    }

    #[test]
    fn sa3_backend_mapping_is_explicit_and_fail_closed() {
        assert_eq!(
            sa3_backend_for("macos", "aarch64").unwrap(),
            Sa3Backend::Mlx
        );
        for os in ["linux", "windows"] {
            assert_eq!(sa3_backend_for(os, "x86_64").unwrap(), Sa3Backend::Tflite);
        }
        assert_eq!(
            installer_target_for("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            installer_target_for("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            installer_target_for("windows", "x86_64").unwrap(),
            "x86_64-pc-windows-msvc"
        );
        for target in [
            ("macos", "x86_64"),
            ("linux", "aarch64"),
            ("freebsd", "x86_64"),
        ] {
            assert!(sa3_backend_for(target.0, target.1).is_err());
        }
    }

    #[test]
    fn tflite_provenance_round_trips_and_rejects_drift() {
        let root = std::env::temp_dir().join(format!(
            "lsdj-tflite-provenance-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let pin = tflite_pin();
        write_tflite_provenance(&root, &pin).unwrap();
        validate_tflite_provenance(&root, &pin).unwrap();
        std::fs::write(root.join(TFLITE_PROVENANCE_STAMP), b"{}").unwrap();
        assert!(validate_tflite_provenance(&root, &pin)
            .unwrap_err()
            .contains("does not match"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tflite_install_paths_cannot_escape_the_runtime() {
        assert_eq!(
            checked_install_path("models/tflite/same-s/dec_fp32.tflite").unwrap(),
            PathBuf::from("optimized/tflite/models/tflite/same-s/dec_fp32.tflite")
        );
        for unsafe_path in [
            "../outside",
            "/absolute",
            "models/tflite/../../outside",
            "models\\tflite\\outside",
        ] {
            assert!(checked_install_path(unsafe_path).is_err());
        }
    }

    #[test]
    fn portable_wheel_sets_are_complete_and_platform_specific() {
        let linux = wheel_pins_for("x86_64-unknown-linux-gnu").unwrap();
        let windows = wheel_pins_for("x86_64-pc-windows-msvc").unwrap();
        assert_eq!(linux.len(), 33);
        assert_eq!(windows.len(), 34);
        assert!(linux.iter().any(|pin| pin.package == "pydantic-core"));
        assert!(windows.iter().any(|pin| pin.package == "colorama"));
        assert!(wheel_pins_for("aarch64-unknown-linux-gnu").is_err());
    }

    #[test]
    fn portable_wheelhouse_rejects_missing_unexpected_and_tampered_files() {
        use sha2::{Digest, Sha256};

        let root = std::env::temp_dir().join(format!(
            "lsdj-wheelhouse-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut pins = wheel_pins_for("x86_64-unknown-linux-gnu").unwrap();
        for pin in &mut pins {
            let bytes = pin.filename.as_bytes();
            pin.artifact.size = bytes.len() as u64;
            pin.artifact.sha256 = hex::encode(Sha256::digest(bytes));
            std::fs::write(root.join(&pin.filename), bytes).unwrap();
        }
        verify_wheelhouse(&root, &pins, &|| false).unwrap();

        let missing = root.join(&pins[0].filename);
        std::fs::remove_file(&missing).unwrap();
        assert!(verify_wheelhouse(&root, &pins, &|| false)
            .unwrap_err()
            .contains("missing or unexpected"));
        std::fs::write(&missing, pins[0].filename.as_bytes()).unwrap();

        std::fs::write(root.join("surprise.whl"), b"surprise").unwrap();
        assert!(verify_wheelhouse(&root, &pins, &|| false)
            .unwrap_err()
            .contains("missing or unexpected"));
        std::fs::remove_file(root.join("surprise.whl")).unwrap();

        std::fs::write(&missing, vec![b'x'; pins[0].artifact.size as usize]).unwrap();
        assert!(verify_wheelhouse(&root, &pins, &|| false)
            .unwrap_err()
            .contains("verification"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn portable_dependency_install_is_strictly_offline_and_exact() {
        use sha2::{Digest, Sha256};

        let root = std::env::temp_dir().join(format!(
            "lsdj-offline-wheels-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let mut pins = wheel_pins_for("x86_64-unknown-linux-gnu").unwrap();
        for pin in &mut pins {
            let bytes = pin.filename.as_bytes();
            pin.artifact.size = bytes.len() as u64;
            pin.artifact.sha256 = hex::encode(Sha256::digest(bytes));
            std::fs::write(root.join(&pin.filename), bytes).unwrap();
        }
        let mut command = Command::new("uv");
        configure_portable_install(&mut command, &root, &pins, &|| false).unwrap();
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for required in ["--offline", "--no-index", "--no-deps", "--no-config"] {
            assert!(args.iter().any(|arg| arg == required));
        }
        assert!(!args.iter().any(|arg| arg.starts_with("http")));
        assert_eq!(
            args.iter().filter(|arg| arg.ends_with(".whl")).count(),
            pins.len()
        );
        let env = command
            .get_envs()
            .map(|(name, value)| {
                (
                    name.to_string_lossy().into_owned(),
                    value.map(|item| item.to_string_lossy().into_owned()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(env.get("UV_OFFLINE"), Some(&Some("1".into())));
        assert_eq!(env.get("UV_NO_INDEX"), Some(&Some("1".into())));
        for proxy in ["ALL_PROXY", "HTTPS_PROXY", "HTTP_PROXY"] {
            assert_eq!(env.get(proxy), Some(&None));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn app_managed_model_validation_hashes_all_eight_artifacts() {
        use sha2::{Digest, Sha256};

        let root = std::env::temp_dir().join(format!(
            "lsdj-model-integrity-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let model_dir = root
            .join("optimized")
            .join("mlx")
            .join("models")
            .join("mlx");
        std::fs::create_dir_all(&model_dir).unwrap();
        let mut pin = sa3_pin();
        for (index, model) in pin.models.artifacts.iter_mut().enumerate() {
            let bytes = format!("fixture-{index}").into_bytes();
            model.size = bytes.len() as u64;
            model.sha256 = hex::encode(Sha256::digest(&bytes));
            std::fs::write(model_dir.join(model.filename().unwrap()), bytes).unwrap();
        }

        validate_sa3_model_artifacts(&root, &pin, Sa3Backend::Mlx, &|| false).unwrap();
        let tampered = pin.models.artifacts[3].filename().unwrap();
        let original_size = pin.models.artifacts[3].size as usize;
        std::fs::write(model_dir.join(tampered), vec![b'x'; original_size]).unwrap();
        let error =
            validate_sa3_model_artifacts(&root, &pin, Sa3Backend::Mlx, &|| false).unwrap_err();
        assert!(error.contains("SHA-256"), "unexpected error: {error}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn source_stamp_round_trips() {
        let tmp = std::env::temp_dir().join(format!("lsdj-stamp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("optimized").join("mlx")).unwrap();
        // Absent before writing.
        assert_eq!(read_source_stamp(&tmp), None);
        let src = Sa3Source {
            repo: "https://github.com/brxs/stable-audio-3".into(),
            commit: "abc123def456".into(),
        };
        write_source_stamp(&tmp, &src).unwrap();
        assert_eq!(read_source_stamp(&tmp), Some(src));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn update_available_reflects_source_drift() {
        let pin = Sa3Source {
            repo: "https://github.com/brxs/stable-audio-3".into(),
            commit: "36ef97776ee12375".into(),
        };
        // A missing install is a plain install, never "update available".
        assert!(!sa3_update_available(None, &pin, false));
        // Present but unstamped (legacy / hand-placed) — can't prove a match.
        assert!(sa3_update_available(None, &pin, true));
        // Exact match.
        assert!(!sa3_update_available(Some(&pin.clone()), &pin, true));
        // Short-SHA stamp vs full-SHA pin (prefix) counts as a match.
        let short = Sa3Source {
            repo: pin.repo.clone(),
            commit: "36ef977".into(),
        };
        assert!(!sa3_update_available(Some(&short), &pin, true));
        // A different commit, or a different repo (e.g. after reverting to
        // upstream), is updatable.
        let other_commit = Sa3Source {
            repo: pin.repo.clone(),
            commit: "deadbeef".into(),
        };
        assert!(sa3_update_available(Some(&other_commit), &pin, true));
        let other_repo = Sa3Source {
            repo: "https://github.com/Stability-AI/stable-audio-3".into(),
            commit: pin.commit.clone(),
        };
        assert!(sa3_update_available(Some(&other_repo), &pin, true));
        // A trailing slash on the repo is ignored.
        let slash = Sa3Source {
            repo: format!("{}/", pin.repo),
            commit: pin.commit.clone(),
        };
        assert!(!sa3_update_available(Some(&slash), &pin, true));
    }

    // --- End-to-end install: actually run the pipeline against a stub backend.
    // These spawn real processes and exercise the full spawn → stream-parse →
    // progress → on-disk-result path (no weights, no GUI, no network). Env is set
    // ONLY on the child Command (never process-global), so they can't race the
    // sidecar tests that share this binary's environment.

    #[cfg(unix)]
    fn shared() -> InstallShared {
        InstallShared {
            busy: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            current_child: Mutex::new(None),
            active: Mutex::new(None),
        }
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_between_spawn_and_park_terminates_the_child() {
        let shared = shared();
        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 30");
        let child = crate::child_process::spawn_grouped(&mut command).expect("spawn child");
        let pid = child.id() as libc::pid_t;

        // Model the precise race: cancel() observed an empty slot after the OS
        // spawn completed but before stream_child published the handle.
        shared.cancelled.store(true, Ordering::Release);
        assert_eq!(park_child(&shared, child), Err("cancelled".into()));
        assert!(
            shared
                .current_child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_none(),
            "cancelled child must not remain parked"
        );
        // SAFETY: signal 0 only probes whether the already-recorded pid exists.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "child survived cancellation"
        );
    }

    #[cfg(unix)]
    fn write_exec(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // A stand-in for the frozen sidecar's `--download-model` mode: writes the two
    // model files into $MAGENTA_HOME and emits the JSON progress contract.
    #[cfg(unix)]
    const STUB_SIDECAR: &str = r#"#!/bin/sh
name=""
while [ $# -gt 0 ]; do
  case "$1" in
    --download-model) name="$2"; shift 2 ;;
    *) shift ;;
  esac
done
dir="$MAGENTA_HOME/magenta-rt-v2/models/$name"
mkdir -p "$dir"
: > "$dir/$name.mlxfn"
: > "$dir/${name}_state.safetensors"
printf '{"event":"stage","stage":"download","label":"%s"}\n' "$name"
printf '{"event":"file","file":"models/%s/%s_state.safetensors"}\n' "$name" "$name"
printf '{"event":"done"}\n'
"#;

    #[cfg(unix)]
    #[test]
    fn install_magenta_runs_the_tooling_and_the_model_appears() {
        let tmp = std::env::temp_dir().join(format!("lsdj-install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let models = tmp.join("magenta-rt-v2").join("models");
        std::fs::create_dir_all(&models).unwrap();
        let stub = tmp.join("stub.sh");
        write_exec(&stub, STUB_SIDECAR);

        // The stub stands in for the frozen sidecar; MAGENTA_HOME is set on the
        // CHILD only (not the test process), so this can't race other tests.
        let mut cmd = Command::new("sh");
        cmd.arg(&stub).env("MAGENTA_HOME", &tmp);
        cmd.args(["--download-model", "mrt2_small"]);

        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let progress = move |stage: &str, _message: Option<String>, file: Option<String>| {
            sink.lock().unwrap().push((stage.to_string(), file));
        };
        let result = run_download(&progress, &shared(), cmd);

        assert!(result.is_ok(), "install failed: {result:?}");
        // The install actually populated the models dir — discovery now sees it.
        assert_eq!(discover_installed(&models), vec!["mrt2_small".to_string()]);
        let recorded = events.lock().unwrap();
        assert!(recorded.iter().any(|(_, file)| {
            file.as_deref() == Some("models/mrt2_small/mrt2_small_state.safetensors")
        }));
        drop(recorded);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn run_download_reports_a_tooling_error() {
        let tmp = std::env::temp_dir().join(format!("lsdj-install-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let stub = tmp.join("fail.sh");
        write_exec(
            &stub,
            "#!/bin/sh\nprintf '{\"event\":\"error\",\"message\":\"no weights\"}\\n'\nexit 1\n",
        );
        let mut cmd = Command::new("sh");
        cmd.arg(&stub);

        let noop = |_: &str, _: Option<String>, _: Option<String>| {};
        let result = run_download(&noop, &shared(), cmd);
        assert_eq!(result, Err("no weights".to_string()));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn run_download_sanitizes_and_bounds_structured_tooling_errors() {
        let tmp = std::env::temp_dir().join(format!("lsdj-install-secret-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let stub = tmp.join("fail-secret.sh");
        let message = format!(
            "HF_TOKEN=hf-secret HUGGING_FACE_HUB_TOKEN=hub-secret \
             client_secret=oauth-secret access_token=access-secret \
             api_key=api-secret Authorization: Bearer auth-secret {}",
            "x".repeat(5000)
        );
        let json = serde_json::json!({"event": "error", "message": message});
        write_exec(
            &stub,
            &format!("#!/bin/sh\nprintf '%s\\n' '{}'\nexit 1\n", json),
        );
        let mut cmd = Command::new("sh");
        cmd.arg(&stub);

        let noop = |_: &str, _: Option<String>, _: Option<String>| {};
        let error = run_download(&noop, &shared(), cmd).expect_err("stub must fail");
        assert!(
            error.len() <= 2048,
            "unbounded error length: {}",
            error.len()
        );
        for secret in [
            "hf-secret",
            "hub-secret",
            "oauth-secret",
            "access-secret",
            "api-secret",
            "auth-secret",
        ] {
            assert!(!error.contains(secret), "leaked {secret}: {error}");
        }
        assert!(error.contains("[REDACTED]"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn uv_command_keeps_unicode_paths_structured() {
        let root = Path::new("/tmp/LSDJ profile ü with spaces");
        let executable = root.join("runtime tools").join("uv");
        let cwd = root.join("Stable Audio 3");
        let cache = root.join("staging cache");
        let venv = cwd.join(".venv");
        let mut command = uv_command(&executable, &cwd, &cache);
        command.arg("venv").arg(&venv);

        assert_eq!(command.get_program(), executable.as_os_str());
        assert_eq!(command.get_current_dir(), Some(cwd.as_path()));
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args[0], std::ffi::OsStr::new("venv"));
        assert_eq!(args[1], venv.as_os_str());
    }

    #[cfg(unix)]
    #[test]
    fn cancel_kills_the_whole_process_group() {
        use std::time::Duration;
        let tmp = std::env::temp_dir().join(format!("lsdj-cancel-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let marker = tmp.join("grandchild-ran");
        let pidfile = tmp.join("grandchild-pid");
        let stub = tmp.join("stub.sh");
        // A backgrounded grandchild (like `uv run`'s python) that would write the
        // marker; the child then blocks. Killing only the immediate child orphans
        // the grandchild, which survives to write the marker — the bug. "started"
        // is echoed AFTER the fork and its pid is on disk first, so the stdout
        // line is the synchronisation point — no timing budget to blow through
        // under suite load. The long sleeps are ceilings, never waited out on
        // the passing path.
        write_exec(
            &stub,
            &format!(
                "#!/bin/sh\n( sleep 30; : > \"{}\" ) &\necho $! > \"{}\"\necho started\nsleep 30\n",
                marker.display(),
                pidfile.display()
            ),
        );

        let shared = shared();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut cmd = Command::new("sh");
                cmd.arg(&stub);
                let _ = stream_child(&shared, "cancel-test", cmd, |line| {
                    if line == "started" {
                        let _ = started_tx.send(());
                    }
                });
            });
            // stdout lines only flow once the child is parked in `shared`, so
            // after "started" the take cannot miss — and the grandchild exists.
            started_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("stub never reported started");
            let mut child = shared
                .current_child
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
                .expect("child parked before its stdout flowed");
            let _ = child.force_kill();
        });

        // The group kill signals the grandchild atomically; its reaping (by
        // launchd, once orphaned) is not — poll the pid until it is gone.
        let pid: libc::pid_t = std::fs::read_to_string(&pidfile)
            .expect("pidfile written before started")
            .trim()
            .parse()
            .expect("pidfile holds a pid");
        let mut gone = false;
        for _ in 0..1000 {
            // SAFETY: signal 0 probes liveness without signalling; a stale pid
            // at worst delays the loop, it cannot kill anything.
            if unsafe { libc::kill(pid, 0) } == -1 {
                gone = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(gone, "grandchild survived the group kill");
        assert!(!marker.exists(), "grandchild wrote past the group kill");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
