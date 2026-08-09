//! SA3 LoRA adapter manager (issue #66, ADR-0028): import, list, and delete
//! Stable Audio 3 LoRA finetunes, mirroring the model manager's split — Rust
//! owns the lifecycle, `backend/lsdj/loras.py` owns the generate-time read.
//!
//! The registry is a directory layout, discovered from the filesystem like the
//! models (no central index file):
//!
//! ```text
//! <host-resolved assets>/sa3-loras/<base>/<slug>/
//!     adapter_model.safetensors      (the adapter — any single *.safetensors)
//!     adapter_config.json            (PEFT convention only)
//!     lora.json                      (import manifest: source / type / rank)
//! ```
//!
//! `<base>` is `small` (the 1024-wide sm-sfx / sm-music DiTs) or `medium`
//! (1536-wide). The trust boundary is ADR-0028's: **only `.safetensors` is
//! accepted** — pickle-backed files (`.ckpt`/`.pt`/`.pth`/`.bin`) are refused
//! before any read, and validation parses only the safetensors JSON header
//! (shapes + metadata), never tensor data. The base is inferred from the
//! adapter's own layer widths (a `lora_A` runs `rank × fan_in`, and the two
//! DiTs share no width), with the PEFT config's `base_model_name_or_path` and
//! an explicit user choice as fallbacks for adapters whose shapes are
//! rank-only (`-xs`).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::models::{cancelled, dir_size, is_cancelled, InstallShared, Progress};
use crate::runtime_installer::download::{
    client as installer_client, download_verified, fetch_bytes_bounded, PinnedArtifact,
};

/// The two DiT families an adapter can ride (`loras.BASES` in Python).
pub const BASES: &[&str] = &["small", "medium"];

// Layer widths that identify a base from adapter shapes alone — measured from
// the pinned fork's `dit_mlx.py` (embed 1024, ff-inner 4096, cond 768) and
// `dit_mlx_medium.py` (embed 1536, ff-inner 6144). The sets are disjoint, so
// one matching fan-in decides.
const SMALL_WIDTHS: &[u64] = &[1024, 4096, 768];
const MEDIUM_WIDTHS: &[u64] = &[1536, 6144];

// Pickle-backed extensions ADR-0028 refuses outright.
const PICKLE_EXTS: &[&str] = &["ckpt", "pt", "pth", "bin"];

// A safetensors JSON header beyond this is not a plausible adapter — bail
// before allocating attacker-controlled sizes.
const MAX_HEADER_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HF_INFO_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ADAPTER_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

const MANIFEST: &str = "lora.json";

/// The registry root resolved by the Rust host. An explicit dev/user override is
/// captured into this contract during startup; callers never guess an OS path.
pub fn loras_dir() -> PathBuf {
    crate::platform_paths::get().loras_home().to_path_buf()
}

// --- Registry discovery ----------------------------------------------------

/// One installed adapter, for the manager UI and the generate pickers.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoraInfo {
    /// `<base>/<slug>` — the name the generate request sends to the backend.
    name: String,
    base: String,
    slug: String,
    size_bytes: u64,
    /// Import manifest facts, when present (absent for hand-placed adapters).
    source: Option<String>,
    adapter_type: Option<String>,
    rank: Option<u32>,
}

/// The import manifest written beside the adapter (`lora.json`).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoraManifest {
    source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    convention: String,
    adapter_type: String,
    rank: Option<u32>,
    #[serde(default)]
    files: Vec<LoraArtifactRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LoraArtifactRecord {
    filename: String,
    size: u64,
    sha256: String,
}

/// A well-formed adapter directory: exactly one `*.safetensors` inside (the
/// same rule as `loras._adapter_file` in Python and the runtime's resolver).
fn adapter_file(dir: &Path) -> Option<PathBuf> {
    let directory = std::fs::symlink_metadata(dir).ok()?;
    if directory.file_type().is_symlink() || !directory.is_dir() {
        return None;
    }
    let canonical_dir = std::fs::canonicalize(dir).ok()?;
    let entries = std::fs::read_dir(dir).ok()?;
    let mut hits: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| {
                !metadata.file_type().is_symlink()
                    && metadata.is_file()
                    && path.extension().is_some_and(|ext| ext == "safetensors")
                    && std::fs::canonicalize(path)
                        .is_ok_and(|canonical| canonical.starts_with(&canonical_dir))
            })
        })
        .collect();
    if hits.len() == 1 {
        hits.pop()
    } else {
        None
    }
}

/// One path segment of an adapter name: no separators, no leading dot — a name
/// can only ever address a directory INSIDE the registry (mirrors `loras._SLUG`).
fn valid_slug(slug: &str) -> bool {
    let mut chars = slug.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Parse a client-supplied `<base>/<slug>` name; anything malformed is refused
/// before it can touch the filesystem.
fn parse_name(name: &str) -> Result<(&str, &str), String> {
    let (base, slug) = name.split_once('/').unwrap_or((name, ""));
    if !BASES.contains(&base) || !valid_slug(slug) {
        return Err(format!("unknown adapter '{name}'"));
    }
    Ok((base, slug))
}

/// Every installed adapter under `root`, sorted by name. Best-effort like the
/// model discovery: unreadable entries and malformed directories are skipped.
pub fn discover(root: &Path) -> Vec<LoraInfo> {
    let mut adapters = Vec::new();
    let Ok(root_metadata) = std::fs::symlink_metadata(root) else {
        return adapters;
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return adapters;
    }
    let Ok(canonical_root) = std::fs::canonicalize(root) else {
        return adapters;
    };
    for base in BASES {
        let base_dir = root.join(base);
        let Ok(base_metadata) = std::fs::symlink_metadata(&base_dir) else {
            continue;
        };
        if base_metadata.file_type().is_symlink()
            || !base_metadata.is_dir()
            || !std::fs::canonicalize(&base_dir).is_ok_and(|path| path.starts_with(&canonical_root))
        {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&base_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let dir = entry.path();
            let slug = entry.file_name().to_string_lossy().into_owned();
            let trusted_dir = std::fs::symlink_metadata(&dir).is_ok_and(|metadata| {
                !metadata.file_type().is_symlink()
                    && metadata.is_dir()
                    && std::fs::canonicalize(&dir)
                        .is_ok_and(|path| path.starts_with(&canonical_root))
            });
            if !trusted_dir || !valid_slug(&slug) || adapter_file(&dir).is_none() {
                continue;
            }
            let manifest: Option<LoraManifest> = std::fs::read_to_string(dir.join(MANIFEST))
                .ok()
                .and_then(|data| serde_json::from_str(&data).ok());
            if manifest.as_ref().is_some_and(|item| {
                !item.files.is_empty() && validate_registry_generation(&dir).is_err()
            }) {
                continue;
            }
            adapters.push(LoraInfo {
                name: format!("{base}/{slug}"),
                base: (*base).to_string(),
                slug,
                size_bytes: dir_size(&dir),
                source: manifest.as_ref().map(|m| m.source.clone()),
                adapter_type: manifest.as_ref().map(|m| m.adapter_type.clone()),
                rank: manifest.as_ref().and_then(|m| m.rank),
            });
        }
    }
    adapters.sort_by(|a, b| a.name.cmp(&b.name));
    adapters
}

/// The installed-adapter names, for the watcher's changed-set comparison.
pub fn discover_names(root: &Path) -> std::collections::BTreeSet<String> {
    discover(root).into_iter().map(|info| info.name).collect()
}

// --- Validation (the ADR-0028 trust boundary) ------------------------------

/// What the safetensors header + config told us about an adapter.
#[derive(Debug, PartialEq)]
pub struct AdapterFacts {
    convention: Convention,
    adapter_type: String,
    rank: Option<u32>,
    /// The base inferred from layer widths / the PEFT config; `None` when the
    /// adapter is shape-anonymous (rank-only `-xs` tensors, no config hint).
    inferred_base: Option<&'static str>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Convention {
    /// SA3-native `train_lora.py` output — config in the safetensors metadata.
    Native,
    /// HuggingFace `peft` — config in a sibling `adapter_config.json`.
    Peft,
}

impl Convention {
    fn as_str(self) -> &'static str {
        match self {
            Convention::Native => "native",
            Convention::Peft => "peft",
        }
    }
}

fn is_pickle(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            let lower = ext.to_ascii_lowercase();
            PICKLE_EXTS.contains(&lower.as_str())
        })
}

/// The parsed safetensors header: tensor name → shape, plus `__metadata__`.
/// Reads ONLY the 8-byte length + JSON header — never tensor data, never a
/// pickle (`mx.load`-equivalent trust posture without loading anything).
type SafetensorsHeader = (BTreeMap<String, Vec<u64>>, BTreeMap<String, String>);

fn read_safetensors_header(path: &Path) -> Result<SafetensorsHeader, String> {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    if is_pickle(path) {
        return Err(format!(
            "refusing the pickle-format adapter '{file_name}' — only .safetensors \
             adapters are accepted (a .ckpt/.pt is unpickled by torch.load and can \
             execute arbitrary code)"
        ));
    }
    if path.extension().is_none_or(|ext| ext != "safetensors") {
        return Err(format!("'{file_name}' is not a .safetensors adapter"));
    }
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("cannot open '{file_name}': {e}"))?;
    let mut len_bytes = [0u8; 8];
    file.read_exact(&mut len_bytes)
        .map_err(|_| format!("'{file_name}' is not a safetensors file"))?;
    let header_len = u64::from_le_bytes(len_bytes);
    if header_len == 0 || header_len > MAX_HEADER_BYTES {
        return Err(format!("'{file_name}' is not a safetensors file"));
    }
    let mut header = vec![0u8; header_len as usize];
    file.read_exact(&mut header)
        .map_err(|_| format!("'{file_name}' is not a safetensors file"))?;
    let parsed: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&header)
        .map_err(|_| format!("'{file_name}' is not a safetensors file"))?;

    let mut tensors = BTreeMap::new();
    let mut metadata = BTreeMap::new();
    for (key, value) in parsed {
        if key == "__metadata__" {
            if let Some(map) = value.as_object() {
                for (meta_key, meta_value) in map {
                    if let Some(text) = meta_value.as_str() {
                        metadata.insert(meta_key.clone(), text.to_string());
                    }
                }
            }
            continue;
        }
        let shape = value
            .get("shape")
            .and_then(|s| s.as_array())
            .map(|dims| dims.iter().filter_map(|d| d.as_u64()).collect::<Vec<_>>())
            .unwrap_or_default();
        tensors.insert(key, shape);
    }
    Ok((tensors, metadata))
}

/// Tensor keys of the SA3-native convention
/// (`<layer>.parametrizations.weight.0.<param>`).
const NATIVE_MARKER: &str = ".parametrizations.weight.0.";

/// Infer the base from the fan-in widths of 2-D `lora_A` tensors (shape
/// `rank × fan_in`; the DiT widths are disjoint between the bases). `Err` on a
/// contradiction, `Ok(None)` when nothing matched (rank-only shapes).
fn base_from_widths(tensors: &BTreeMap<String, Vec<u64>>) -> Result<Option<&'static str>, String> {
    let mut small = false;
    let mut medium = false;
    for (key, shape) in tensors {
        let is_lora_a = key.ends_with(".lora_A.weight") || key.ends_with(".lora_A");
        if !is_lora_a || shape.len() != 2 {
            continue;
        }
        let fan_in = shape[1];
        small |= SMALL_WIDTHS.contains(&fan_in);
        medium |= MEDIUM_WIDTHS.contains(&fan_in);
    }
    match (small, medium) {
        (true, true) => Err(
            "the adapter mixes small-DiT and medium-DiT layer widths — not a valid \
             SA3 LoRA"
                .into(),
        ),
        (true, false) => Ok(Some("small")),
        (false, true) => Ok(Some("medium")),
        (false, false) => Ok(None),
    }
}

/// The PEFT `adapter_config.json` fields the importer reads.
#[derive(Deserialize, Default)]
struct PeftConfig {
    r: Option<u32>,
    use_dora: Option<bool>,
    base_model_name_or_path: Option<String>,
}

/// The SA3-native `lora_config` metadata fields the importer reads.
#[derive(Deserialize, Default)]
struct NativeConfig {
    adapter_type: Option<String>,
    rank: Option<u32>,
}

fn base_from_model_name(model_name: &str) -> Option<&'static str> {
    let lower = model_name.to_ascii_lowercase();
    if lower.contains("medium") {
        Some("medium")
    } else if lower.contains("small") || lower.contains("sm-") {
        Some("small")
    } else {
        None
    }
}

/// Infer a rank from the tensors when no config declares it: `lora_A` is
/// `rank × fan_in`, `M_xs` is `rank × rank`.
fn rank_from_tensors(tensors: &BTreeMap<String, Vec<u64>>) -> Option<u32> {
    for (key, shape) in tensors {
        let is_rank_first =
            key.ends_with(".lora_A.weight") || key.ends_with(".lora_A") || key.ends_with(".M_xs");
        if is_rank_first {
            if let Some(&rank) = shape.first() {
                return u32::try_from(rank).ok();
            }
        }
    }
    None
}

/// Validate one adapter file structurally (ADR-0028): recognised convention,
/// inferable type/rank, and a base read from the shapes. `peft_config_path` is
/// the sibling `adapter_config.json` when the file follows that convention.
pub fn validate_adapter(path: &Path) -> Result<AdapterFacts, String> {
    let (tensors, metadata) = read_safetensors_header(path)?;

    if tensors.keys().any(|key| key.contains(NATIVE_MARKER)) {
        let config: NativeConfig = metadata
            .get("lora_config")
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        // Legacy 'dora' means 'dora-rows' (the paper-correct default) —
        // mirrors the runtime's resolve_adapter_type.
        let adapter_type = match config.adapter_type.as_deref() {
            None => "lora".to_string(),
            Some("dora") => "dora-rows".to_string(),
            Some(other) => other.to_string(),
        };
        return Ok(AdapterFacts {
            convention: Convention::Native,
            adapter_type,
            rank: config.rank.or_else(|| rank_from_tensors(&tensors)),
            inferred_base: base_from_widths(&tensors)?,
        });
    }

    if tensors.keys().any(|key| key.ends_with(".lora_A.weight")) {
        let config_path = path.with_file_name("adapter_config.json");
        if !config_path.is_file() {
            return Err("the PEFT adapter is missing its adapter_config.json sibling".into());
        }
        let config: PeftConfig = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .ok_or("the adapter's adapter_config.json is unreadable")?;
        let inferred_base = match base_from_widths(&tensors)? {
            Some(base) => Some(base),
            None => config
                .base_model_name_or_path
                .as_deref()
                .and_then(base_from_model_name),
        };
        return Ok(AdapterFacts {
            convention: Convention::Peft,
            adapter_type: if config.use_dora.unwrap_or(false) {
                "dora-rows".to_string()
            } else {
                "lora".to_string()
            },
            rank: config.r.or_else(|| rank_from_tensors(&tensors)),
            inferred_base,
        });
    }

    Err(
        "not a recognised SA3 LoRA (no SA3-native parametrization keys and no PEFT \
         lora_A/lora_B keys)"
            .into(),
    )
}

// --- Import ----------------------------------------------------------------

/// What to import: exactly one of a HuggingFace repo id or a local path
/// (a `.safetensors` file or a PEFT adapter directory), plus an optional
/// explicit base for shape-anonymous adapters.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ImportSpec {
    pub hf_repo: Option<String>,
    pub path: Option<String>,
    pub base: Option<String>,
}

impl ImportSpec {
    /// The display name for progress events; doubles as the early shape check
    /// (exactly one source, well-formed repo id, known base).
    pub fn display_name(&self) -> Result<String, String> {
        if let Some(base) = self.base.as_deref() {
            if !BASES.contains(&base) {
                return Err(format!("unknown base '{base}'"));
            }
        }
        match (self.hf_repo.as_deref(), self.path.as_deref()) {
            (Some(repo), None) => normalize_hf_repo(repo)
                .ok_or_else(|| format!("'{repo}' is not a HuggingFace repo id")),
            (None, Some(path)) => Ok(Path::new(path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()),
            _ => Err("exactly one of a HuggingFace repo or a local path is required".into()),
        }
    }
}

/// `owner/name`, each segment alphanumeric-headed with `. _ -` allowed — enough
/// to build the API/resolve URLs without the id smuggling path or query parts.
fn valid_hf_repo(repo: &str) -> bool {
    match repo.split_once('/') {
        Some((owner, name)) => valid_slug(owner) && valid_slug(name),
        None => false,
    }
}

/// Accept what people actually paste as a repo: a bare `owner/name` id or a
/// full huggingface.co URL (scheme/host, a `/tree/…` suffix, query, fragment).
/// Returns the canonical id, or `None` when no valid id is inside.
fn normalize_hf_repo(input: &str) -> Option<String> {
    let mut rest = input.trim();
    for prefix in ["https://", "http://"] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
            break;
        }
    }
    for host in ["www.huggingface.co/", "huggingface.co/", "hf.co/"] {
        if let Some(stripped) = rest.strip_prefix(host) {
            rest = stripped;
            break;
        }
    }
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut segments = rest.split('/').filter(|segment| !segment.is_empty());
    let repo = format!("{}/{}", segments.next()?, segments.next()?);
    valid_hf_repo(&repo).then_some(repo)
}

/// A registry slug derived from a repo/file name: invalid characters become
/// `-`, a leading non-alphanumeric is stripped.
fn slugify(name: &str) -> Result<String, String> {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = cleaned
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();
    if valid_slug(&slug) {
        Ok(slug)
    } else {
        Err(format!("cannot derive an adapter name from '{name}'"))
    }
}

/// Pick the adapter files to download from a HF repo's file list: the
/// conventional `adapter_model.safetensors`, or the single `*.safetensors` —
/// plus the sibling `adapter_config.json` when present. Pickle-only repos get
/// the trust-boundary refusal, not a generic "nothing found". The file list is
/// untrusted (it comes back from the HF API): only root-level names made of
/// slug characters are considered, so a name can neither escape the download
/// dir nor smuggle URL path/query parts.
fn choose_hf_files(filenames: &[String]) -> Result<Vec<String>, String> {
    let config = filenames
        .iter()
        .find(|name| name.as_str() == "adapter_config.json");
    let safetensors: Vec<&String> = filenames
        .iter()
        .filter(|name| name.ends_with(".safetensors") && valid_slug(name))
        .collect();
    let adapter = if safetensors
        .iter()
        .any(|name| name.as_str() == "adapter_model.safetensors")
    {
        "adapter_model.safetensors".to_string()
    } else {
        match safetensors.as_slice() {
            [single] => (*single).clone(),
            [] => {
                let has_pickle = filenames
                    .iter()
                    .any(|name| is_pickle(Path::new(name.as_str())));
                return Err(if has_pickle {
                    "the repo only ships pickle-format weights (.ckpt/.pt/.bin), \
                     which are refused — only .safetensors adapters are accepted"
                        .into()
                } else {
                    "no .safetensors adapter in the repo".into()
                });
            }
            _ => {
                return Err(
                    "the repo holds more than one .safetensors — not a single adapter".into(),
                )
            }
        }
    };
    let mut files = vec![adapter];
    if let Some(config) = config {
        files.push(config.clone());
    }
    Ok(files)
}

/// The HF model-info response — only the file list is read.
#[derive(Deserialize)]
struct HfModelInfo {
    sha: String,
    siblings: Vec<HfSibling>,
}

#[derive(Deserialize)]
struct HfSibling {
    rfilename: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<HfLfs>,
}

#[derive(Deserialize)]
struct HfLfs {
    sha256: String,
    size: u64,
}

/// Run one adapter import to completion (on the install thread): fetch or copy
/// the files, validate them (ADR-0028), resolve the base, and place the adapter
/// directory into the registry.
pub(crate) fn install(
    progress: &Progress,
    shared: &InstallShared,
    spec: &ImportSpec,
) -> Result<(), String> {
    let staged = match (spec.hf_repo.as_deref(), spec.path.as_deref()) {
        (Some(repo), None) => {
            let repo = normalize_hf_repo(repo)
                .ok_or_else(|| format!("'{repo}' is not a HuggingFace repo id"))?;
            fetch_hf_adapter(progress, shared, &repo)?
        }
        (None, Some(path)) => stage_local_adapter(Path::new(path))?,
        _ => return Err("exactly one of a HuggingFace repo or a local path is required".into()),
    };
    let result = (|| {
        cancelled(shared)?;
        progress("install", None, None);
        let facts = validate_adapter(&staged.adapter)?;
        let base = resolve_base(&facts, spec.base.as_deref())?;
        let slug = slugify(&staged.slug_seed)?;
        place_adapter(&loras_dir(), base, &slug, &staged, &facts)
    })();
    // The HF staging dir is cleaned up on every exit; a local import's source
    // files are the user's and stay put.
    if let Some(temp) = &staged.temp {
        drop(StagingCleanup(Some(temp.clone())));
    }
    result
}

/// A staged (temp or user-supplied) adapter before validation: the
/// `.safetensors` path, its optional PEFT config sibling, and what to derive
/// the registry slug from.
struct StagedAdapter {
    adapter: PathBuf,
    config: Option<PathBuf>,
    slug_seed: String,
    source: String,
    revision: Option<String>,
    files: Vec<LoraArtifactRecord>,
    /// The temp dir to clean up after placing (None for a local import, whose
    /// source files are the user's and must stay put).
    temp: Option<PathBuf>,
}

/// Reconcile the inferred base with an explicit choice. An explicit base wins
/// only when the shapes are silent; a contradiction is refused with the
/// reasoning (the issue's "incompatible adapters refused with clear reasoning").
fn resolve_base(facts: &AdapterFacts, explicit: Option<&str>) -> Result<&'static str, String> {
    match (facts.inferred_base, explicit) {
        (Some(inferred), Some(chosen)) if inferred != chosen => Err(format!(
            "the adapter's layer widths identify the {inferred} DiT — it cannot ride \
             the {chosen} base"
        )),
        (Some(inferred), _) => Ok(inferred),
        (None, Some(chosen)) => BASES
            .iter()
            .find(|base| **base == chosen)
            .copied()
            .ok_or_else(|| format!("unknown base '{chosen}'")),
        (None, None) => Err(
            "cannot tell which DiT this adapter rides (its tensors are rank-only) — \
             pick the base (small or medium) explicitly"
                .into(),
        ),
    }
}

fn create_private_staging(parent: &Path, prefix: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create adapter staging root: {error}"))?;
    for _ in 0..32 {
        let directory = parent.join(format!("{prefix}-{:032x}", rand::random::<u128>()));
        match std::fs::create_dir(&directory) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| format!("cannot protect adapter staging: {error}"))?;
                }
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create adapter staging: {error}")),
        }
    }
    Err("cannot allocate a unique adapter staging directory".into())
}

struct StagingCleanup(Option<PathBuf>);

impl Drop for StagingCleanup {
    fn drop(&mut self) {
        let Some(path) = self.0.take() else { return };
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let _ = std::fs::remove_file(path);
            }
            Ok(metadata) if metadata.is_dir() => {
                let _ = std::fs::remove_dir_all(path);
            }
            _ => {}
        }
    }
}

fn hash_record(path: &Path) -> Result<LoraArtifactRecord, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect staged adapter artifact: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("staged adapter artifact is not a regular file".into());
    }
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_slug(name))
        .ok_or("staged adapter filename is unsafe")?
        .to_string();
    let bytes = std::fs::read(path)
        .map_err(|error| format!("cannot hash staged adapter artifact: {error}"))?;
    Ok(LoraArtifactRecord {
        filename,
        size: metadata.len(),
        sha256: hex::encode(Sha256::digest(bytes)),
    })
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("cannot create staged adapter artifact: {error}"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot sync staged adapter artifact: {error}"))
}

/// Fetch an adapter from an immutable Hugging Face commit. Metadata and files
/// use the app's bounded/cancellable HTTPS client; LFS artifacts are verified
/// against the repository-reported SHA-256 before they become import inputs.
fn fetch_hf_adapter(
    progress: &Progress,
    shared: &InstallShared,
    repo: &str,
) -> Result<StagedAdapter, String> {
    if !valid_hf_repo(repo) {
        return Err(format!("'{repo}' is not a HuggingFace repo id"));
    }
    let temp = create_private_staging(
        &crate::platform_paths::get().staging().join("loras"),
        "download",
    )?;
    let mut cleanup = StagingCleanup(Some(temp.clone()));

    progress("fetch", None, None);
    let client = installer_client()?;
    let token = std::env::var("HF_TOKEN")
        .ok()
        .or_else(|| std::env::var("HUGGING_FACE_HUB_TOKEN").ok());
    let info_bytes = fetch_bytes_bounded(
        &client,
        &format!("https://huggingface.co/api/models/{repo}?blobs=true"),
        token.as_deref(),
        MAX_HF_INFO_BYTES,
        || is_cancelled(shared),
    )
    .map_err(|error| format!("cannot reach the HuggingFace repo '{repo}': {error}"))?;
    cancelled(shared)?;
    let info: HfModelInfo = serde_json::from_slice(&info_bytes)
        .map_err(|_| format!("unexpected HuggingFace response for '{repo}'"))?;
    if info.sha.len() != 40 || !info.sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("HuggingFace did not return a full immutable repository revision".into());
    }
    let filenames: Vec<String> = info
        .siblings
        .iter()
        .map(|sibling| sibling.rfilename.clone())
        .collect();
    let files = choose_hf_files(&filenames)?;

    let mut adapter = None;
    let mut config = None;
    let mut records = Vec::new();
    for file in &files {
        cancelled(shared)?;
        progress("download", None, Some(file.clone()));
        let dest = temp.join(file);
        let sibling = info
            .siblings
            .iter()
            .find(|sibling| sibling.rfilename == *file)
            .ok_or("chosen adapter file disappeared from repository metadata")?;
        let url = format!(
            "https://huggingface.co/{repo}/resolve/{}/{file}?download=true",
            info.sha
        );
        if let Some(lfs) = &sibling.lfs {
            let bound = if file.ends_with(".safetensors") {
                MAX_ADAPTER_BYTES
            } else {
                MAX_CONFIG_BYTES
            };
            if lfs.size == 0
                || lfs.size > bound
                || sibling.size.is_some_and(|size| size != lfs.size)
            {
                return Err(format!(
                    "repository metadata has an invalid size for '{file}'"
                ));
            }
            let artifact = PinnedArtifact {
                url,
                sha256: lfs.sha256.clone(),
                size: lfs.size,
            };
            download_verified(&client, &artifact, &dest, token.as_deref(), || {
                is_cancelled(shared)
            })?;
        } else {
            if file.ends_with(".safetensors") {
                return Err("HuggingFace adapter weights lack SHA-256 LFS provenance".into());
            }
            let bytes =
                fetch_bytes_bounded(&client, &url, token.as_deref(), MAX_CONFIG_BYTES, || {
                    is_cancelled(shared)
                })?;
            write_private_file(&dest, &bytes)?;
        }
        records.push(hash_record(&dest)?);
        if file.ends_with(".safetensors") {
            adapter = Some(dest);
        } else {
            config = Some(dest);
        }
    }
    let retained_temp = cleanup.0.take().ok_or("adapter staging disappeared")?;
    Ok(StagedAdapter {
        adapter: adapter.ok_or("the repo download produced no adapter")?,
        config,
        // The repo's own name seeds the slug (`owner/name` → `name`).
        slug_seed: repo.split('/').next_back().unwrap_or(repo).to_string(),
        source: format!("https://huggingface.co/{repo}"),
        revision: Some(info.sha),
        files: records,
        temp: Some(retained_temp),
    })
}

/// Stage a local import: a `.safetensors` file (native, or PEFT with its
/// sibling config next to it) or a PEFT adapter directory.
fn stage_local_adapter(path: &Path) -> Result<StagedAdapter, String> {
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    if is_pickle(path) {
        return Err(format!(
            "refusing the pickle-format adapter '{file_name}' — only .safetensors \
             adapters are accepted (a .ckpt/.pt is unpickled by torch.load and can \
             execute arbitrary code)"
        ));
    }
    let source_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect local adapter source: {error}"))?;
    if source_metadata.file_type().is_symlink() {
        return Err("local adapter source must not be a symbolic link or reparse point".into());
    }
    let adapter = if source_metadata.is_dir() {
        adapter_file(path)
            .ok_or_else(|| format!("expected one .safetensors adapter in '{file_name}'"))?
    } else if source_metadata.is_file() {
        path.to_path_buf()
    } else {
        return Err("local adapter source is not a regular file or directory".into());
    };
    let source_root = if source_metadata.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .ok_or("local adapter file has no parent directory")?
            .to_path_buf()
    };
    let canonical_root = std::fs::canonicalize(&source_root)
        .map_err(|error| format!("cannot canonicalize local adapter root: {error}"))?;
    let canonical_adapter = std::fs::canonicalize(&adapter)
        .map_err(|error| format!("cannot canonicalize local adapter: {error}"))?;
    if !canonical_adapter.starts_with(&canonical_root) {
        return Err("local adapter escapes its selected source directory".into());
    }
    let config_path = adapter.with_file_name("adapter_config.json");
    let slug_seed = adapter
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    // A generic HF artifact name would collide across adapters; the folder
    // name is the distinctive one.
    let slug_seed = if slug_seed == "adapter_model" {
        adapter
            .parent()
            .and_then(|parent| parent.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or(slug_seed)
    } else {
        slug_seed
    };
    let config = match std::fs::symlink_metadata(&config_path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            let canonical = std::fs::canonicalize(&config_path)
                .map_err(|error| format!("cannot canonicalize adapter config: {error}"))?;
            if !canonical.starts_with(&canonical_root) {
                return Err("adapter config escapes its selected source directory".into());
            }
            Some(config_path)
        }
        Ok(_) => return Err("adapter config must not be a symbolic link or reparse point".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot inspect adapter config: {error}")),
    };
    let mut files = vec![hash_record(&adapter)?];
    if let Some(config) = &config {
        files.push(hash_record(config)?);
    }
    Ok(StagedAdapter {
        adapter,
        config,
        slug_seed,
        source: canonical_root.to_string_lossy().into_owned(),
        revision: None,
        files,
        temp: None,
    })
}

/// Copy the staged files into `<root>/<base>/<slug>/` (built in a sibling temp
/// dir, renamed into place so a failure never leaves a half-adapter), write the
/// manifest, and clean the staging dir up.
fn place_adapter(
    root: &Path,
    base: &str,
    slug: &str,
    staged: &StagedAdapter,
    facts: &AdapterFacts,
) -> Result<(), String> {
    let dest = root.join(base).join(slug);
    match std::fs::symlink_metadata(&dest) {
        Ok(_) => {
            return Err(format!(
                "an adapter named '{base}/{slug}' is already installed — delete it first"
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect adapter destination: {error}")),
    }
    std::fs::create_dir_all(root.join(base))
        .map_err(|e| format!("cannot create the adapter registry: {e}"))?;
    let base_dir = root.join(base);
    let root_metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("cannot inspect adapter registry: {error}"))?;
    let base_metadata = std::fs::symlink_metadata(&base_dir)
        .map_err(|error| format!("cannot inspect adapter base directory: {error}"))?;
    let canonical_root = std::fs::canonicalize(root)
        .map_err(|error| format!("cannot canonicalize adapter registry: {error}"))?;
    let canonical_base = std::fs::canonicalize(&base_dir)
        .map_err(|error| format!("cannot canonicalize adapter base directory: {error}"))?;
    if root_metadata.file_type().is_symlink()
        || base_metadata.file_type().is_symlink()
        || !root_metadata.is_dir()
        || !base_metadata.is_dir()
        || !canonical_base.starts_with(&canonical_root)
    {
        return Err("adapter registry path is not a trusted contained directory".into());
    }
    recover_import_staging(&base_dir)?;
    let staging = create_private_staging(&base_dir, ".lsdj-import")?;

    let place = (|| -> Result<(), String> {
        let adapter_name = staged
            .adapter
            .file_name()
            .ok_or("the adapter path has no file name")?;
        std::fs::copy(&staged.adapter, staging.join(adapter_name))
            .map_err(|e| format!("cannot copy the adapter: {e}"))?;
        if let Some(config) = &staged.config {
            std::fs::copy(config, staging.join("adapter_config.json"))
                .map_err(|e| format!("cannot copy adapter_config.json: {e}"))?;
        }
        let manifest = LoraManifest {
            source: staged.source.clone(),
            revision: staged.revision.clone(),
            convention: facts.convention.as_str().to_string(),
            adapter_type: facts.adapter_type.clone(),
            rank: facts.rank,
            files: staged.files.clone(),
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|e| format!("cannot write the manifest: {e}"))?;
        write_private_file(&staging.join(MANIFEST), json.as_bytes())?;
        validate_registry_generation(&staging)?;
        std::fs::rename(&staging, &dest).map_err(|e| format!("cannot place the adapter: {e}"))
    })();
    if place.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    place
}

fn recover_import_staging(base_dir: &Path) -> Result<(), String> {
    let entries = match std::fs::read_dir(base_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot inspect adapter staging: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot inspect adapter staging: {error}"))?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".lsdj-import-") {
            continue;
        }
        let metadata = std::fs::symlink_metadata(entry.path())
            .map_err(|error| format!("cannot inspect interrupted adapter staging: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err("interrupted adapter staging is not a trusted directory".into());
        }
        std::fs::remove_dir_all(entry.path())
            .map_err(|error| format!("cannot recover interrupted adapter staging: {error}"))?;
    }
    Ok(())
}

fn validate_registry_generation(directory: &Path) -> Result<(), String> {
    let adapter =
        adapter_file(directory).ok_or("adapter generation has no unique regular weights")?;
    let manifest_path = directory.join(MANIFEST);
    let metadata = std::fs::symlink_metadata(&manifest_path)
        .map_err(|error| format!("cannot inspect adapter provenance: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("adapter provenance is not a regular file".into());
    }
    let manifest: LoraManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .map_err(|error| format!("cannot read adapter provenance: {error}"))?,
    )
    .map_err(|error| format!("adapter provenance is invalid: {error}"))?;
    if manifest.files.is_empty() {
        return Err("adapter provenance has no artifact inventory".into());
    }
    let expected = manifest
        .files
        .iter()
        .map(|record| (record.filename.clone(), record))
        .collect::<BTreeMap<_, _>>();
    let adapter_name = adapter
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("adapter filename is unsafe")?;
    if !expected.contains_key(adapter_name) {
        return Err("adapter provenance does not inventory its weights".into());
    }
    for (filename, record) in expected {
        if !valid_slug(&filename)
            || record.size == 0
            || hex::decode(&record.sha256).map_or(true, |digest| digest.len() != 32)
        {
            return Err("adapter provenance contains an invalid artifact".into());
        }
        let actual = hash_record(&directory.join(&filename))?;
        if actual.size != record.size || actual.sha256 != record.sha256.to_ascii_lowercase() {
            return Err(format!(
                "adapter artifact '{filename}' failed provenance validation"
            ));
        }
    }
    Ok(())
}

// --- Tauri commands --------------------------------------------------------

/// Import an adapter from a HuggingFace repo id or a local path (issue #66).
/// Runs on the shared install thread; progress arrives as `model://progress`
/// with family `lora`, completion as `models://changed`.
#[tauri::command]
pub fn install_lora(
    installer: tauri::State<'_, crate::models::InstallManager>,
    app: tauri::AppHandle,
    spec: ImportSpec,
) -> Result<(), String> {
    installer.install_lora(app, spec)
}

/// Delete an installed adapter. In-app deletion is fine here, unlike the model
/// families: an adapter is a small re-downloadable artifact in the app-owned
/// dir (never iCloud-managed), and the issue asks for the full
/// install/list/delete lifecycle.
#[tauri::command]
pub fn delete_lora(app: tauri::AppHandle, name: String) -> Result<(), String> {
    use tauri::Emitter;
    let (base, slug) = parse_name(&name)?;
    let root = loras_dir();
    let dir = root.join(base).join(slug);
    let root_metadata = std::fs::symlink_metadata(&root)
        .map_err(|error| format!("cannot inspect adapter registry: {error}"))?;
    let dir_metadata =
        std::fs::symlink_metadata(&dir).map_err(|_| format!("unknown adapter '{name}'"))?;
    let contained = !root_metadata.file_type().is_symlink()
        && root_metadata.is_dir()
        && !dir_metadata.file_type().is_symlink()
        && dir_metadata.is_dir()
        && std::fs::canonicalize(&root).is_ok_and(|canonical_root| {
            std::fs::canonicalize(&dir)
                .is_ok_and(|canonical_dir| canonical_dir.starts_with(canonical_root))
        });
    if !contained || adapter_file(&dir).is_none() {
        return Err(format!("unknown adapter '{name}'"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot delete '{name}': {e}"))?;
    let _ = app.emit("models://changed", ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("lsdj-lora-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        tmp
    }

    /// Build a minimal safetensors file: the 8-byte length + JSON header, with
    /// zeroed tensor data appended (sizes don't matter — only the header is read).
    fn write_safetensors(path: &Path, tensors: &[(&str, &[u64])], metadata: &[(&str, &str)]) {
        let mut header = serde_json::Map::new();
        if !metadata.is_empty() {
            let meta: serde_json::Map<String, serde_json::Value> = metadata
                .iter()
                .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
                .collect();
            header.insert("__metadata__".into(), meta.into());
        }
        for (name, shape) in tensors {
            header.insert(
                name.to_string(),
                serde_json::json!({"dtype": "F16", "shape": shape, "data_offsets": [0, 0]}),
            );
        }
        let json = serde_json::to_vec(&serde_json::Value::Object(header)).unwrap();
        let mut bytes = (json.len() as u64).to_le_bytes().to_vec();
        bytes.extend_from_slice(&json);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn a_pickle_extension_is_refused_before_any_read() {
        let tmp = temp_root("pickle");
        let path = tmp.join("adapter.ckpt");
        std::fs::write(&path, b"not even opened").unwrap();
        let error = validate_adapter(&path).unwrap_err();
        assert!(error.contains("pickle"), "unexpected error: {error}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_non_safetensors_file_is_refused() {
        let tmp = temp_root("garbage");
        let path = tmp.join("adapter.safetensors");
        std::fs::write(&path, b"RIFF not a safetensors").unwrap();
        assert!(validate_adapter(&path).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_peft_adapter_yields_its_facts_and_medium_base() {
        let tmp = temp_root("peft");
        let path = tmp.join("adapter_model.safetensors");
        write_safetensors(
            &path,
            &[
                (
                    "base_model.model.transformer.layers.0.self_attn.to_qkv.lora_A.weight",
                    &[64, 1536],
                ),
                (
                    "base_model.model.transformer.layers.0.self_attn.to_qkv.lora_B.weight",
                    &[7680, 64],
                ),
            ],
            &[],
        );
        std::fs::write(
            tmp.join("adapter_config.json"),
            r#"{"r": 64, "lora_alpha": 128, "use_dora": false,
                "base_model_name_or_path": "stabilityai/stable-audio-3-medium"}"#,
        )
        .unwrap();
        let facts = validate_adapter(&path).unwrap();
        assert_eq!(facts.convention, Convention::Peft);
        assert_eq!(facts.adapter_type, "lora");
        assert_eq!(facts.rank, Some(64));
        assert_eq!(facts.inferred_base, Some("medium"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_peft_adapter_without_its_config_is_refused() {
        let tmp = temp_root("peft-noconfig");
        let path = tmp.join("adapter_model.safetensors");
        write_safetensors(
            &path,
            &[
                ("x.lora_A.weight", &[8, 1024]),
                ("x.lora_B.weight", &[1024, 8]),
            ],
            &[],
        );
        let error = validate_adapter(&path).unwrap_err();
        assert!(
            error.contains("adapter_config.json"),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_native_adapter_reads_its_metadata_config_and_small_base() {
        let tmp = temp_root("native");
        let path = tmp.join("finetune.safetensors");
        write_safetensors(
            &path,
            &[
                (
                    "transformer.layers.0.self_attn.to_qkv.parametrizations.weight.0.lora_A",
                    &[16, 1024],
                ),
                (
                    "transformer.layers.0.self_attn.to_qkv.parametrizations.weight.0.lora_B",
                    &[3072, 16],
                ),
            ],
            &[(
                "lora_config",
                r#"{"adapter_type": "dora", "rank": 16, "alpha": 32}"#,
            )],
        );
        let facts = validate_adapter(&path).unwrap();
        assert_eq!(facts.convention, Convention::Native);
        assert_eq!(facts.adapter_type, "dora-rows");
        assert_eq!(facts.rank, Some(16));
        assert_eq!(facts.inferred_base, Some("small"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn an_xs_adapter_is_shape_anonymous_and_needs_an_explicit_base() {
        let tmp = temp_root("xs");
        let path = tmp.join("finetune-xs.safetensors");
        write_safetensors(
            &path,
            &[(
                "transformer.layers.0.self_attn.to_qkv.parametrizations.weight.0.M_xs",
                &[32, 32],
            )],
            &[("lora_config", r#"{"adapter_type": "lora-xs", "rank": 32}"#)],
        );
        let facts = validate_adapter(&path).unwrap();
        assert_eq!(facts.inferred_base, None);
        assert_eq!(facts.rank, Some(32));
        // Unresolvable without a choice; resolvable with one; a contradiction
        // elsewhere is refused.
        assert!(resolve_base(&facts, None).is_err());
        assert_eq!(resolve_base(&facts, Some("medium")), Ok("medium"));
        let shaped = AdapterFacts {
            convention: Convention::Native,
            adapter_type: "lora".into(),
            rank: Some(16),
            inferred_base: Some("medium"),
        };
        assert!(resolve_base(&shaped, Some("small")).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_random_safetensors_is_not_a_lora() {
        let tmp = temp_root("notlora");
        let path = tmp.join("weights.safetensors");
        write_safetensors(&path, &[("model.embed.weight", &[512, 1024])], &[]);
        let error = validate_adapter(&path).unwrap_err();
        assert!(
            error.contains("not a recognised"),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn hf_file_choice_prefers_the_convention_and_refuses_pickle_only_repos() {
        let names = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // The conventional pair.
        assert_eq!(
            choose_hf_files(&names(&[
                "README.md",
                "adapter_config.json",
                "adapter_model.safetensors"
            ]))
            .unwrap(),
            vec!["adapter_model.safetensors", "adapter_config.json"]
        );
        // A single differently-named safetensors, no config (SA3-native).
        assert_eq!(
            choose_hf_files(&names(&["README.md", "maqam.safetensors"])).unwrap(),
            vec!["maqam.safetensors"]
        );
        // Pickle-only → the trust-boundary refusal, by name.
        let error = choose_hf_files(&names(&["adapter.ckpt", "README.md"])).unwrap_err();
        assert!(error.contains("pickle"), "unexpected error: {error}");
        // Nothing usable at all.
        assert!(choose_hf_files(&names(&["README.md"])).is_err());
        // Ambiguous.
        assert!(choose_hf_files(&names(&["a.safetensors", "b.safetensors"])).is_err());
    }

    #[test]
    fn adapter_names_parse_and_hostile_ones_are_refused() {
        assert_eq!(parse_name("medium/maqam").unwrap(), ("medium", "maqam"));
        for hostile in [
            "maqam",
            "large/maqam",
            "medium/../maqam",
            "medium/.hidden",
            "medium/",
            "medium/sub/dir",
            "MEDIUM/maqam",
        ] {
            assert!(parse_name(hostile).is_err(), "accepted {hostile:?}");
        }
    }

    #[test]
    fn hf_repo_ids_validate() {
        assert!(valid_hf_repo("motiftechnologies/stable-audio-3-maqam-lora"));
        for hostile in [
            "no-slash",
            "a/b/c",
            "../x/y",
            "a/..",
            "a/b?x=1",
            "https://huggingface.co/a/b",
        ] {
            assert!(!valid_hf_repo(hostile), "accepted {hostile:?}");
        }
    }

    #[test]
    fn pasted_repo_forms_normalize_to_the_canonical_id() {
        let id = "motiftechnologies/stable-audio-3-maqam-lora";
        let pasted_forms = [
            id.to_string(),
            format!("  {id}  "),
            format!("https://huggingface.co/{id}"),
            format!("https://huggingface.co/{id}/"),
            format!("https://huggingface.co/{id}/tree/main"),
            format!("https://huggingface.co/{id}?not-for-all-audiences=true"),
            format!("http://www.huggingface.co/{id}#model-card"),
            format!("hf.co/{id}"),
            format!("huggingface.co/{id}"),
        ];
        for pasted in &pasted_forms {
            assert_eq!(
                normalize_hf_repo(pasted).as_deref(),
                Some(id),
                "failed to normalize {pasted:?}"
            );
        }
        for rejected in ["", "no-slash", "https://huggingface.co", "a/../b"] {
            assert_eq!(normalize_hf_repo(rejected), None, "accepted {rejected:?}");
        }
        // A foreign URL degrades to its first two path-ish segments — a
        // nonexistent HF repo id that 404s at huggingface.co. The id can never
        // route a request to the foreign host (the API/resolve URLs are built
        // on our own base), so this is harmless, not a smuggling vector.
        assert_eq!(
            normalize_hf_repo("https://evil.example/a/b?x").as_deref(),
            Some("evil.example/a")
        );
    }

    #[test]
    fn a_local_import_lands_in_the_registry_and_discovery_sees_it() {
        let root = temp_root("import");
        let source_dir = root.join("source");
        std::fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("maqam.safetensors");
        write_safetensors(
            &source,
            &[(
                "transformer.layers.0.self_attn.to_out.parametrizations.weight.0.lora_A",
                &[8, 1536],
            )],
            &[("lora_config", r#"{"adapter_type": "lora", "rank": 8}"#)],
        );

        let registry = root.join("registry");
        let staged = stage_local_adapter(&source).unwrap();
        let facts = validate_adapter(&source).unwrap();
        let base = resolve_base(&facts, None).unwrap();
        place_adapter(&registry, base, "maqam", &staged, &facts).unwrap();

        // The user's source file stays put; the registry holds the copy.
        assert!(source.is_file());
        let installed = discover(&registry);
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "medium/maqam");
        assert_eq!(installed[0].adapter_type.as_deref(), Some("lora"));
        assert_eq!(installed[0].rank, Some(8));

        // A second import under the same name is refused, not overwritten.
        let error = place_adapter(&registry, base, "maqam", &staged, &facts).unwrap_err();
        assert!(
            error.contains("already installed"),
            "unexpected error: {error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discovery_skips_malformed_directories() {
        let root = temp_root("discover");
        // Well-formed.
        let good = root.join("small").join("crackle");
        std::fs::create_dir_all(&good).unwrap();
        write_safetensors(
            &good.join("crackle.safetensors"),
            &[("x.lora_A", &[4, 1024])],
            &[],
        );
        // No safetensors.
        std::fs::create_dir_all(root.join("small").join("empty")).unwrap();
        // Two safetensors — ambiguous, skipped (matches the Python resolver).
        let two = root.join("medium").join("two");
        std::fs::create_dir_all(&two).unwrap();
        write_safetensors(&two.join("a.safetensors"), &[("x.lora_A", &[4, 1536])], &[]);
        write_safetensors(&two.join("b.safetensors"), &[("x.lora_A", &[4, 1536])], &[]);
        // A dot-dir never becomes a name.
        let hidden = root.join("medium").join(".importing");
        std::fs::create_dir_all(&hidden).unwrap();
        write_safetensors(
            &hidden.join("x.safetensors"),
            &[("x.lora_A", &[4, 1536])],
            &[],
        );

        let names: Vec<String> = discover(&root).into_iter().map(|info| info.name).collect();
        assert_eq!(names, vec!["small/crackle".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_and_local_import_reject_symlink_escape_paths() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink-escape");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let weights = outside.join("escape.safetensors");
        write_safetensors(&weights, &[("x.lora_A", &[4, 1024])], &[]);

        let registry = root.join("registry");
        std::fs::create_dir_all(registry.join("small")).unwrap();
        symlink(&outside, registry.join("small").join("escaped-dir")).unwrap();
        assert!(discover(&registry).is_empty());

        let linked_file = root.join("linked.safetensors");
        symlink(&weights, &linked_file).unwrap();
        let error = match stage_local_adapter(&linked_file) {
            Ok(_) => panic!("symlink import unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.contains("symbolic link"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn interrupted_random_staging_is_recovered_without_touching_other_entries() {
        let root = temp_root("recover-staging");
        let base = root.join("small");
        std::fs::create_dir_all(&base).unwrap();
        let interrupted = base.join(".lsdj-import-0123456789abcdef");
        std::fs::create_dir_all(&interrupted).unwrap();
        std::fs::write(interrupted.join("partial"), b"partial").unwrap();
        let unrelated = base.join("leave-me");
        std::fs::create_dir_all(&unrelated).unwrap();

        recover_import_staging(&base).unwrap();
        assert!(!interrupted.exists());
        assert!(unrelated.is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn slugs_are_derived_and_sanitised() {
        assert_eq!(
            slugify("stable-audio-3-maqam-lora").unwrap(),
            "stable-audio-3-maqam-lora"
        );
        assert_eq!(slugify("My Adapter (v2)").unwrap(), "My-Adapter--v2-");
        assert_eq!(slugify("..sneaky").unwrap(), "sneaky");
        assert!(slugify("...").is_err());
        assert!(slugify("").is_err());
    }
}
