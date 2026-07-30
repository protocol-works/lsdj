//! Model manager (issue #43): status, install, and delete for the two model
//! families — Magenta deck models and the Stable Audio 3 stack — surfaced in the
//! settings-drawer panel. Rust owns the lifecycle (mirrors ADR-0022); the actual
//! downloads are delegated to Python/shell tooling Rust orchestrates:
//!
//! - **Magenta** → the frozen sidecar's `--init-resources` / `--download-model`
//!   modes (`backend/lsdj/sidecar.py`), which reuse `magenta_rt.cli` verbatim and
//!   stream a JSON progress contract on stdout. Resources are fetched first so a
//!   freshly downloaded model is actually loadable (a model's two files are not
//!   enough without `resources/musiccoca` + `resources/spectrostream`).
//! - **Stable Audio 3** → `curl` the pinned source tarball (`sa3-pin.json`),
//!   `tar`-extract it, and run `scripts/sa3-install.sh` (build+warm steps; no
//!   git, no tty, no system Python 3.11).
//!
//! Status facts mirror the stable conventions in `backend/lsdj/paths.py` and
//! `backend/lsdj/sa3.py` (the two-file model layout, the SA3 candidate list, the
//! four readiness states). The webview never gets filesystem access — the same
//! trust boundary as the rest of the library surface.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// The official models the manager offers to download (mirrors
/// `engine.KNOWN_MODELS`). This is the installable catalog, NOT a discovery gate:
/// `discover_installed` finds any model folder, but only these can be downloaded.
pub const INSTALLABLE_MODELS: &[&str] = &["mrt2_small", "mrt2_base"];

// Canonical SA3 readiness states — the exact identifiers `sa3.readiness` uses.
const SA3_MISSING: &str = "missing";
const SA3_VENV_MISSING: &str = "venv_missing";
const SA3_NOT_WARMED: &str = "not_warmed";
const SA3_READY: &str = "ready";

const WARMED_STAMP: &str = ".lsdj-warmed";

// Records the source (`sa3-pin.json` repo + commit) the in-app installer fetched,
// so `model_status` can tell when the installed checkout has drifted from a bumped
// pin and offer an in-app update. Written by Rust after a fetch (the shell
// installer doesn't know the commit). Lives beside `.lsdj-warmed` in optimized/mlx.
const SOURCE_STAMP: &str = ".lsdj-source.json";

// --- Path resolution (mirrors backend/lsdj/paths.py + sa3.py) --------------

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The `magenta-rt-v2` data root: `(MAGENTA_HOME or ~/Documents/Magenta)/
/// magenta-rt-v2`. The `magenta-rt-v2` segment is ALWAYS appended, even when
/// `MAGENTA_HOME` is set — matching `paths.magenta_home()`.
fn magenta_home() -> PathBuf {
    let base = std::env::var_os("MAGENTA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join("Documents").join("Magenta"));
    base.join("magenta-rt-v2")
}

/// The Magenta models dir (`paths.models_dir()`).
pub fn magenta_models_dir() -> PathBuf {
    magenta_home().join("models")
}

/// The app-owned data root for model weights — `~/Library/Application Support/
/// LSDJai`. Kept out of `~/Documents` (which users may sync to iCloud, where
/// multi-GB weights don't belong and Finder ops on offloaded files fail, -8013).
pub(crate) fn app_support_base() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Application Support")
        .join("LSDJai")
}

/// The app-owned home for the Stable Audio 3 checkout — where in-app installs go
/// and the first place the resolver looks (kept out of `~/Documents`, like the
/// Magenta weights).
fn sa3_app_home() -> PathBuf {
    app_support_base().join("stable-audio-3")
}

/// Decide the one-time migration: `Some((from, to))` when a prior install lives
/// at `old_rtv2` and `new_rtv2` doesn't exist yet, else `None`. Pure (no I/O
/// beyond the existence checks) so the policy is unit-testable.
fn migration_move(new_rtv2: &Path, old_rtv2: &Path) -> Option<(PathBuf, PathBuf)> {
    (!new_rtv2.exists() && old_rtv2.is_dir())
        .then(|| (old_rtv2.to_path_buf(), new_rtv2.to_path_buf()))
}

/// Point `MAGENTA_HOME` at the app-owned data dir and migrate a prior
/// `~/Documents/Magenta` install into it (a same-volume rename — instant, no
/// multi-GB copy). A pre-set `MAGENTA_HOME` (a dev/user override) wins. Must run
/// once at startup BEFORE any backend process is spawned, so the children — and
/// `magenta_rt.paths`, which reads the env at import — inherit the new location.
pub fn ensure_magenta_home() {
    if std::env::var_os("MAGENTA_HOME").is_some() {
        return; // respect an explicit override (dev, or a custom location)
    }
    let base = app_support_base();
    let old_base = home_dir().join("Documents").join("Magenta");
    if let Some((from, to)) = migration_move(&base.join("magenta-rt-v2"), &old_base.join("magenta-rt-v2")) {
        let _ = std::fs::create_dir_all(&base);
        if std::fs::rename(&from, &to).is_err() {
            // Cross-volume / perms: keep the existing install rather than strand it.
            std::env::set_var("MAGENTA_HOME", &old_base);
            return;
        }
        eprintln!("lsdj-app: migrated model weights → {}", to.display());
    }
    std::env::set_var("MAGENTA_HOME", &base);
}

/// Whether the shared resources a model load needs are present — without these
/// (`mrt models init` fetches them) a model's two files cannot load.
fn resources_present() -> bool {
    let resources = magenta_home().join("resources");
    resources.join("musiccoca").is_dir() && resources.join("spectrostream").is_dir()
}

/// SA3 checkout roots to probe, in order (mirrors `sa3._checkout_candidates`).
fn sa3_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(override_home) = std::env::var_os("SA3_MLX_HOME") {
        if !override_home.is_empty() {
            candidates.push(PathBuf::from(override_home));
        }
    }
    candidates.push(sa3_app_home()); // the app-owned home — where in-app installs go
    candidates
}

/// The SA3 install state + the resolved checkout root (mirrors `sa3.readiness`):
/// the first candidate with an `optimized/mlx` dir, classified `missing` /
/// `venv_missing` / `not_warmed` / `ready`.
fn sa3_status() -> (&'static str, Option<PathBuf>) {
    let mut first_with_mlx: Option<PathBuf> = None;
    for checkout in sa3_candidates() {
        let mlx = checkout.join("optimized").join("mlx");
        if !mlx.is_dir() {
            continue;
        }
        if first_with_mlx.is_none() {
            first_with_mlx = Some(checkout.clone());
        }
        let python = mlx.join(".venv").join("bin").join("python");
        let script = mlx.join("scripts").join("sa3_mlx.py");
        if !(python.is_file() && script.is_file()) {
            continue;
        }
        let state = if mlx.join(WARMED_STAMP).is_file() {
            SA3_READY
        } else {
            SA3_NOT_WARMED
        };
        return (state, Some(checkout));
    }
    match first_with_mlx {
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
    checkout.join("optimized").join("mlx").join(SOURCE_STAMP)
}

/// The source recorded in a checkout's stamp, or `None` when absent (a checkout
/// installed before stamping existed, or placed by hand) or unreadable.
fn read_source_stamp(checkout: &Path) -> Option<Sa3Source> {
    let data = std::fs::read_to_string(source_stamp_path(checkout)).ok()?;
    serde_json::from_str(&data).ok()
}

/// Record what was fetched into the checkout. Best-effort: a failed write just
/// means the next `model_status` treats the checkout as updatable.
fn write_source_stamp(checkout: &Path, source: &Sa3Source) {
    if let Ok(json) = serde_json::to_string_pretty(source) {
        let _ = std::fs::write(source_stamp_path(checkout), json);
    }
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
    let stamp_mtime = std::fs::metadata(checkout.join("optimized").join("mlx").join(WARMED_STAMP))
        .and_then(|m| m.modified())
        .ok();
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
    size_bytes: u64,
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
    let installed = discover_installed(&models_dir)
        .into_iter()
        .map(|name| {
            let size_bytes = dir_size(&models_dir.join(&name));
            InstalledModel {
                name,
                size_bytes,
                needs_resources: !resources,
            }
        })
        .collect();
    let (sa3_state, sa3_checkout) = sa3_status();
    let sa3_size = sa3_checkout.as_ref().map(|c| sa3_checkout_size(c)).unwrap_or(0);
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
            size_bytes: sa3_size,
            checkout: sa3_checkout.map(|c| c.to_string_lossy().into_owned()),
            installed_source,
            pinned_source: pinned,
            update_available,
        },
        loras: crate::loras::discover(&crate::loras::loras_dir()),
        installing: active.map(|(family, name)| ActiveInstall {
            family,
            name,
        }),
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

fn emit(app: &AppHandle, family: Family, name: &str, stage: &str, message: Option<String>, file: Option<String>) {
    let _ = app.emit(
        "model://progress",
        ModelProgress {
            family,
            name: name.to_string(),
            stage: stage.to_string(),
            message,
            file,
        },
    );
}

/// The pinned SA3 source (`sa3-pin.json`, the single bump point). Compiled in so
/// a released binary carries the pin it was built with.
#[derive(Deserialize)]
struct Sa3Pin {
    repo: String,
    commit: String,
}

fn sa3_pin() -> Sa3Pin {
    const PIN: &str = include_str!("../../sa3-pin.json");
    serde_json::from_str(PIN).expect("sa3-pin.json is valid JSON")
}

fn sa3_install_script() -> PathBuf {
    if let Some(path) = std::env::var_os("LSDJ_SA3_INSTALL_SH") {
        return PathBuf::from(path);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/sa3-install.sh")
}

/// Shared install state: at most one install runs at a time; the running stage's
/// child is parked here so [`InstallManager::cancel`] / shutdown can reach it.
/// `active` names the in-flight job so `model_status` can report it — the manager
/// reflects an install even after the drawer was closed and reopened (the live
/// `model://progress` events are missed while it's unmounted).
pub(crate) struct InstallShared {
    busy: AtomicBool,
    cancelled: AtomicBool,
    current_child: Mutex<Option<Child>>,
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
        self.shared.active.lock().unwrap_or_else(|p| p.into_inner()).clone()
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
                *shared.current_child.lock().unwrap_or_else(|p| p.into_inner()) = None;
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
        if let Some(mut child) = self.shared.current_child.lock().unwrap_or_else(|p| p.into_inner()).take() {
            kill_group(&mut child);
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

/// Kill the child's whole process group (it was spawned as a group leader, so its
/// pgid equals its pid), then reap the leader. This takes down `uv run`'s python
/// grandchild and any shell descendants — killing only the leader would orphan
/// them and leave the download running. The grandchildren are reparented to
/// launchd, which reaps them.
fn kill_group(child: &mut Child) {
    let group = -(child.id() as libc::pid_t);
    // SAFETY: `kill(2)` with a negative pid signals the process group; the pid is a
    // live child we own. A failure (already-exited group) is ignored.
    unsafe {
        libc::kill(group, libc::SIGKILL);
    }
    let _ = child.wait();
    // A descendant that was mid-fork during the sweep can miss the signal — but
    // it is still IN the group (fork inherits the pgid), so re-sweep until the
    // group has no members (signal 0 probes without signalling). One pass
    // suffices in practice; the bound keeps a stray unkillable member from
    // spinning this thread forever.
    for _ in 0..100 {
        // SAFETY: as above; signal 0 sends nothing.
        if unsafe { libc::kill(group, 0) } == -1 {
            break;
        }
        unsafe {
            libc::kill(group, libc::SIGKILL);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub(crate) fn cancelled(shared: &InstallShared) -> Result<(), String> {
    if shared.cancelled.load(Ordering::Acquire) {
        Err("cancelled".into())
    } else {
        Ok(())
    }
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
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Run the child as its own process-group leader so a cancel can kill the whole
    // tree: the install runs `uv run python …` (and sa3-install.sh shells further),
    // and killing only the immediate child would orphan the real worker — which
    // keeps the stdout pipe open and wedges the reader below. See `kill_group`.
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{label}: cannot spawn ({e})"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    *shared.current_child.lock().unwrap_or_else(|p| p.into_inner()) = Some(child);

    let drain_label = label.to_string();
    let stderr_drain = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("lsdj-app: {drain_label}: {line}");
        }
    });
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        on_line(&line);
    }
    let _ = stderr_drain.join();

    // Reclaim the child to read its exit status; cancel() may have taken it.
    let Some(mut child) = shared.current_child.lock().unwrap_or_else(|p| p.into_inner()).take() else {
        return Err("cancelled".into());
    };
    let status = child.wait().map_err(|e| format!("{label}: wait failed ({e})"))?;
    cancelled(shared)?;
    if !status.success() {
        return Err(format!("{label}: exited with {status}"));
    }
    Ok(())
}

/// One parsed line of the sidecar's JSON progress contract.
#[derive(Deserialize)]
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

/// Spawn the download tooling and map its JSON progress contract onto the sink.
/// Takes the fully-built command so the spawn+parse path is testable against a
/// stub without mutating the process environment.
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
            "error" => last_error = parsed.message.or(Some("download failed".into())),
            _ => {}
        }
    });
    // Prefer the tooling's own error message over the generic non-zero exit.
    result.map_err(|exit_err| last_error.unwrap_or(exit_err))
}

fn install_sa3(progress: &Progress, shared: &InstallShared, update: bool) -> Result<(), String> {
    let (_state, existing) = sa3_status();
    let (checkout, fetched) = match existing {
        // Resume an existing checkout (venv_missing / not_warmed / ready) — the
        // installer is idempotent (the `.lsdj-warmed` stamp gates re-warming). An
        // explicit update instead re-fetches the pinned source, swapping it in
        // (fetch_sa3_checkout backs up and restores on failure) and re-building.
        Some(checkout) if !update => (checkout, false),
        _ => (fetch_sa3_checkout(progress, shared)?, true),
    };
    cancelled(shared)?;
    run_sa3_installer(progress, shared, &checkout, &sa3_install_script())?;
    if fetched {
        // Stamp the fetched source so a later pin bump shows as "update available".
        write_source_stamp(&checkout, &pinned_source());
    }
    Ok(())
}

/// Run the SA3 build+warm script (`install.sh -y --python 3.11` + warm). Takes
/// the checkout + script paths so the run is testable against a stub without
/// touching the process environment.
fn run_sa3_installer(
    progress: &Progress,
    shared: &InstallShared,
    checkout: &Path,
    script: &Path,
) -> Result<(), String> {
    progress("install", None, None);
    let mut cmd = Command::new("bash");
    cmd.arg(script).arg(checkout);
    // The script's stdout (warming N/3 …) is English and un-keyed; the "install"
    // stage label carries the wording. stderr is drained to the app log already.
    stream_child(shared, "sa3-install", cmd, |_line| {})
}

/// Fetch + extract the pinned SA3 source into the conventional home, returning
/// the checkout root. Extracts to a temp dir and renames the single archive top
/// dir into place, so a partial fetch never leaves a broken checkout.
fn fetch_sa3_checkout(progress: &Progress, shared: &InstallShared) -> Result<PathBuf, String> {
    let pin = sa3_pin();
    let home = sa3_app_home();
    let url = format!("{}/archive/{}.tar.gz", pin.repo.trim_end_matches('/'), pin.commit);
    let tmp = std::env::temp_dir().join(format!("lsdj-sa3-{}", pin.commit));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create temp dir: {e}"))?;
    let tarball = tmp.join("sa3.tar.gz");

    progress("fetch", None, None);
    let mut curl = Command::new("curl");
    curl.args(["-fLsS", "-o"]).arg(&tarball).arg(&url);
    stream_child(shared, "curl", curl, |_| {})?;
    cancelled(shared)?;

    progress("extract", None, None);
    let extract = tmp.join("extract");
    std::fs::create_dir_all(&extract).map_err(|e| format!("cannot create extract dir: {e}"))?;
    let mut tar = Command::new("tar");
    tar.arg("-xzf").arg(&tarball).arg("-C").arg(&extract);
    stream_child(shared, "tar", tar, |_| {})?;

    let top = single_subdir(&extract).ok_or("unexpected SA3 archive layout")?;
    if let Some(parent) = home.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create SA3 home: {e}"))?;
    }
    // Swap the new checkout in non-destructively: if anything is already at `home`
    // (a stale partial — a valid one wouldn't reach here), move it aside first and
    // restore it if the rename fails, so a failure never destroys it irrecoverably.
    if home.exists() {
        let backup = home.with_extension("old");
        let _ = std::fs::remove_dir_all(&backup);
        std::fs::rename(&home, &backup).map_err(|e| format!("cannot stage SA3 home: {e}"))?;
        if let Err(e) = std::fs::rename(&top, &home) {
            let _ = std::fs::rename(&backup, &home);
            return Err(format!("cannot place SA3 checkout: {e}"));
        }
        let _ = std::fs::remove_dir_all(&backup);
    } else {
        std::fs::rename(&top, &home).map_err(|e| format!("cannot place SA3 checkout: {e}"))?;
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(home)
}

/// The single immediate subdirectory of `dir`, or `None` if there is not exactly
/// one (a GitHub source archive extracts to one `<repo>-<sha>/` dir).
fn single_subdir(dir: &Path) -> Option<PathBuf> {
    let mut found: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if entry.file_type().ok()?.is_dir() {
            if found.is_some() {
                return None;
            }
            found = Some(entry.path());
        }
    }
    found
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

        assert_eq!(discover_installed(&tmp), vec!["custom_x".to_string(), "mrt2_small".to_string()]);
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
        assert!(!pin.commit.is_empty());
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
        write_source_stamp(&tmp, &src);
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
        let short = Sa3Source { repo: pin.repo.clone(), commit: "36ef977".into() };
        assert!(!sa3_update_available(Some(&short), &pin, true));
        // A different commit, or a different repo (e.g. after reverting to
        // upstream), is updatable.
        let other_commit = Sa3Source { repo: pin.repo.clone(), commit: "deadbeef".into() };
        assert!(sa3_update_available(Some(&other_commit), &pin, true));
        let other_repo = Sa3Source {
            repo: "https://github.com/Stability-AI/stable-audio-3".into(),
            commit: pin.commit.clone(),
        };
        assert!(sa3_update_available(Some(&other_repo), &pin, true));
        // A trailing slash on the repo is ignored.
        let slash = Sa3Source { repo: format!("{}/", pin.repo), commit: pin.commit.clone() };
        assert!(!sa3_update_available(Some(&slash), &pin, true));
    }

    #[test]
    fn migration_moves_a_prior_install_only_when_the_new_dir_is_absent() {
        let tmp = std::env::temp_dir().join(format!("lsdj-migrate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let new_rtv2 = tmp.join("new").join("magenta-rt-v2");
        let old_rtv2 = tmp.join("old").join("magenta-rt-v2");

        // No prior install → nothing to move.
        assert_eq!(migration_move(&new_rtv2, &old_rtv2), None);

        // Prior install present, new dir absent → move it.
        std::fs::create_dir_all(&old_rtv2).unwrap();
        assert_eq!(
            migration_move(&new_rtv2, &old_rtv2),
            Some((old_rtv2.clone(), new_rtv2.clone())),
        );

        // New dir already exists (already migrated) → leave the old one be.
        std::fs::create_dir_all(&new_rtv2).unwrap();
        assert_eq!(migration_move(&new_rtv2, &old_rtv2), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // --- End-to-end install: actually run the pipeline against a stub backend.
    // These spawn real processes and exercise the full spawn → stream-parse →
    // progress → on-disk-result path (no weights, no GUI, no network). Env is set
    // ONLY on the child Command (never process-global), so they can't race the
    // sidecar tests that share this binary's environment.

    fn shared() -> InstallShared {
        InstallShared {
            busy: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            current_child: Mutex::new(None),
            active: Mutex::new(None),
        }
    }

    fn write_exec(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // A stand-in for the frozen sidecar's `--download-model` mode: writes the two
    // model files into $MAGENTA_HOME and emits the JSON progress contract.
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

    #[test]
    fn run_download_reports_a_tooling_error() {
        let tmp = std::env::temp_dir().join(format!("lsdj-install-err-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let stub = tmp.join("fail.sh");
        write_exec(&stub, "#!/bin/sh\nprintf '{\"event\":\"error\",\"message\":\"no weights\"}\\n'\nexit 1\n");
        let mut cmd = Command::new("sh");
        cmd.arg(&stub);

        let noop = |_: &str, _: Option<String>, _: Option<String>| {};
        let result = run_download(&noop, &shared(), cmd);
        assert_eq!(result, Err("no weights".to_string()));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn run_sa3_installer_runs_the_script_and_reports_the_stage() {
        let tmp = std::env::temp_dir().join(format!("lsdj-sa3install-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let checkout = tmp.join("co");
        std::fs::create_dir_all(&checkout).unwrap();
        let marker = tmp.join("ran");
        let stub = tmp.join("install.sh");
        // The script's own stdout is intentionally NOT streamed to the UI (it is
        // English and un-keyed); the touched marker proves it actually ran.
        write_exec(&stub, &format!("#!/bin/sh\n: > \"{}\"\nexit 0\n", marker.display()));

        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        let progress = move |stage: &str, _message: Option<String>, _file: Option<String>| {
            sink.lock().unwrap().push(stage.to_string());
        };
        let result = run_sa3_installer(&progress, &shared(), &checkout, &stub);

        assert!(result.is_ok(), "sa3 install failed: {result:?}");
        assert!(marker.exists(), "the installer script did not run");
        assert!(events.lock().unwrap().iter().any(|stage| stage == "install"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

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
            kill_group(&mut child);
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
