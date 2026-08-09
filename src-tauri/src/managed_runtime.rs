//! Verified launch contract for app-managed Python services.
//!
//! Linux and Windows releases never execute a path merely because it exists.
//! Every service lives in an atomically promoted generation and is described by
//! a structured manifest.  Resolution revalidates the target, generation
//! identity, provenance, complete file inventory, sizes, and SHA-256 digests
//! before returning an absolute program plus fixed argv.  No shell, `PATH`
//! lookup, system Python, Git, or `uv` participates in a production launch.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const MANIFEST_NAME: &str = ".lsdj-launch-manifest.json";
const GENERATION_NAME: &str = ".lsdj-generation";
const SCHEMA_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 32 * 1024 * 1024;

const STATIC_ENV_KEYS: &[&str] = &[
    "DO_NOT_TRACK",
    "HF_HUB_DISABLE_TELEMETRY",
    "HF_HUB_OFFLINE",
    "NO_COLOR",
    "PYTHONDONTWRITEBYTECODE",
    "PYTHONNOUSERSITE",
    "PYTHONUTF8",
];

const EPHEMERAL_ENV_KEYS: &[&str] = &[
    "LSDJ_API_CAPABILITY",
    "LSDJ_ASSETS_HOME",
    "LSDJ_CACHE_HOME",
    "LSDJ_CONFIG_HOME",
    "LSDJ_DATA_HOME",
    "LSDJ_STAGING_HOME",
    "LSDJ_WORKER_LAUNCH_TOKEN",
    "MAGENTA_HOME",
    "SA3_HOME",
    "SA3_LORAS_HOME",
    "SA3_MLX_HOME",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "WINDIR",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Service {
    Mrt2,
    Sa3,
    #[allow(dead_code)]
    Sa3Cuda,
}

impl Service {
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::Mrt2 => "mrt2",
            Self::Sa3 => "sa3-tflite",
            Self::Sa3Cuda => "sa3-pytorch-cuda",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CommandSpec {
    pub(crate) program: String,
    #[serde(default)]
    pub(crate) argv: Vec<String>,
    pub(crate) cwd: String,
    #[serde(default)]
    pub(crate) environment: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) ephemeral_environment: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileSeal {
    path: String,
    size: u64,
    sha256: String,
    executable: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LaunchManifest {
    schema_version: u32,
    target: String,
    generation: String,
    provenance: BTreeMap<String, String>,
    services: BTreeMap<String, CommandSpec>,
    files: Vec<FileSeal>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerationIdentity<'a> {
    schema_version: u32,
    target: &'a str,
    provenance: &'a BTreeMap<String, String>,
    services: &'a BTreeMap<String, CommandSpec>,
}

/// A fully verified command description.  Dynamic arguments are appended as
/// individual argv items and ephemeral secrets are accepted only through the
/// manifest's explicit allowlist.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedCommand {
    root: PathBuf,
    program: PathBuf,
    cwd: PathBuf,
    argv: Vec<OsString>,
    environment: BTreeMap<String, String>,
    ephemeral_environment: BTreeSet<String>,
    host_environment: Vec<(OsString, OsString)>,
    generation: String,
    target: String,
}

impl VerifiedCommand {
    #[allow(dead_code)]
    pub(crate) fn generation(&self) -> &str {
        &self.generation
    }

    #[allow(dead_code)]
    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    pub(crate) fn into_command(
        self,
        extra_args: impl IntoIterator<Item = OsString>,
        ephemeral: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Command, String> {
        // Revalidate immediately before spawn.  A model-manager mutation after
        // an earlier status probe never inherits authority to execute.
        let service = service_for_program(&self.root, &self.program, &self.generation)?;
        let refreshed = resolve_at(&self.root, &service, &self.target)?;
        if refreshed.generation != self.generation || refreshed.program != self.program {
            return Err("managed runtime changed while preparing launch".into());
        }

        let mut command = Command::new(&self.program);
        command.env_clear().current_dir(&self.cwd).args(&self.argv);
        for (name, value) in &self.environment {
            command.env(name, value);
        }
        let mut seen = BTreeSet::new();
        for (name, value) in self.host_environment.into_iter().chain(ephemeral) {
            let Some(name) = name.to_str() else {
                return Err("managed runtime environment name is not UTF-8".into());
            };
            if !self.ephemeral_environment.contains(name)
                || !EPHEMERAL_ENV_KEYS.contains(&name)
                || self.environment.contains_key(name)
                || !seen.insert(name.to_string())
            {
                return Err(format!(
                    "managed runtime rejected undeclared environment key {name:?}"
                ));
            }
            command.env(name, value);
        }
        command.args(extra_args);
        Ok(command)
    }
}

fn service_for_program(root: &Path, program: &Path, generation: &str) -> Result<String, String> {
    let manifest = read_manifest(root)?;
    if manifest.generation != generation {
        return Err("managed runtime generation is stale".into());
    }
    manifest
        .services
        .iter()
        .find_map(|(name, spec)| {
            checked_relative(&spec.program)
                .ok()
                .map(|relative| (name, root.join(relative)))
                .filter(|(_, candidate)| candidate == program)
                .map(|(name, _)| name.clone())
        })
        .ok_or_else(|| "managed runtime service disappeared".into())
}

pub(crate) fn host_target() -> String {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu".into(),
        ("x86_64", "windows") => "x86_64-pc-windows-msvc".into(),
        ("aarch64", "macos") => "aarch64-apple-darwin".into(),
        (arch, os) => format!("{arch}-{os}"),
    }
}

/// Stable service layout consumed by both shipping branches.
pub(crate) fn service_root(assets: &Path, service: Service) -> PathBuf {
    assets
        .join("backend")
        .join("services")
        .join(service.wire_name())
        .join("current")
}

pub(crate) fn resolve(assets: &Path, service: Service) -> Result<VerifiedCommand, String> {
    let mut verified = resolve_at(
        &service_root(assets, service),
        service.wire_name(),
        &host_target(),
    )?;
    verified.host_environment = crate::platform_paths::get().managed_child_env()?;
    Ok(verified)
}

fn resolve_at(root: &Path, service: &str, target: &str) -> Result<VerifiedCommand, String> {
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("managed runtime is unavailable: {error}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("managed runtime root is not a trusted directory".into());
    }
    let manifest = read_manifest(root)?;
    validate_manifest(root, &manifest, target)?;
    let spec = manifest
        .services
        .get(service)
        .ok_or_else(|| format!("managed runtime does not provide {service}"))?;
    validate_command_spec(spec)?;
    let program = root.join(checked_relative(&spec.program)?);
    let cwd = root.join(checked_relative(&spec.cwd)?);
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("cannot canonicalize managed runtime root: {error}"))?;
    for (kind, path) in [("program", &program), ("working directory", &cwd)] {
        let canonical = fs::canonicalize(path)
            .map_err(|error| format!("cannot canonicalize managed runtime {kind}: {error}"))?;
        if !canonical.starts_with(&canonical_root) {
            return Err(format!("managed runtime {kind} escapes its generation"));
        }
    }
    let program_metadata = fs::symlink_metadata(&program)
        .map_err(|error| format!("cannot inspect managed runtime program: {error}"))?;
    if program_metadata.file_type().is_symlink() || !program_metadata.is_file() {
        return Err("managed runtime program is not a regular file".into());
    }
    let cwd_metadata = fs::symlink_metadata(&cwd)
        .map_err(|error| format!("cannot inspect managed runtime working directory: {error}"))?;
    if cwd_metadata.file_type().is_symlink() || !cwd_metadata.is_dir() {
        return Err("managed runtime working directory is not a directory".into());
    }

    Ok(VerifiedCommand {
        root: root.to_path_buf(),
        program,
        cwd,
        argv: spec.argv.iter().map(OsString::from).collect(),
        environment: spec.environment.clone(),
        ephemeral_environment: spec.ephemeral_environment.iter().cloned().collect(),
        host_environment: Vec::new(),
        generation: manifest.generation,
        target: manifest.target,
    })
}

fn read_manifest(root: &Path) -> Result<LaunchManifest, String> {
    let path = root.join(MANIFEST_NAME);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("managed runtime manifest is unavailable: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("managed runtime manifest is not a regular file".into());
    }
    if metadata.len() == 0 || metadata.len() > MAX_MANIFEST_BYTES {
        return Err("managed runtime manifest has an invalid size".into());
    }
    let bytes = fs::read(path).map_err(|error| format!("cannot read runtime manifest: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("managed runtime manifest is invalid: {error}"))
}

fn validate_manifest(root: &Path, manifest: &LaunchManifest, target: &str) -> Result<(), String> {
    if manifest.schema_version != SCHEMA_VERSION {
        return Err("managed runtime manifest schema is unsupported".into());
    }
    if manifest.target != target {
        return Err(format!(
            "managed runtime target mismatch: expected {target}, found {}",
            manifest.target
        ));
    }
    let expected_generation =
        generation_id(&manifest.target, &manifest.provenance, &manifest.services)?;
    if manifest.generation != expected_generation {
        return Err("managed runtime manifest generation identity is invalid".into());
    }
    let generation_path = root.join(GENERATION_NAME);
    let generation = read_regular_bounded(&generation_path, 128)?;
    if generation != format!("{}\n", manifest.generation).as_bytes() {
        return Err("managed runtime generation stamp is stale".into());
    }
    if manifest.services.is_empty() || manifest.files.is_empty() {
        return Err("managed runtime manifest is incomplete".into());
    }
    for spec in manifest.services.values() {
        validate_command_spec(spec)?;
    }

    let mut declared = BTreeSet::new();
    for seal in &manifest.files {
        let relative = checked_relative(&seal.path)?;
        if seal.path == MANIFEST_NAME || !declared.insert(seal.path.clone()) {
            return Err("managed runtime file inventory is invalid".into());
        }
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("managed runtime file is missing: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "managed runtime file is not regular: {}",
                seal.path
            ));
        }
        if metadata.len() != seal.size {
            return Err(format!("managed runtime file size changed: {}", seal.path));
        }
        if hash_file(&path)? != seal.sha256 {
            return Err(format!(
                "managed runtime file digest changed: {}",
                seal.path
            ));
        }
        if executable(&metadata) != seal.executable {
            return Err(format!("managed runtime file mode changed: {}", seal.path));
        }
    }

    let actual = inventory_paths(root)?;
    if actual != declared {
        return Err("managed runtime contains missing or unexpected files".into());
    }
    Ok(())
}

fn validate_command_spec(spec: &CommandSpec) -> Result<(), String> {
    checked_relative(&spec.program)?;
    checked_relative(&spec.cwd)?;
    if spec.argv.iter().any(|value| value.contains('\0')) {
        return Err("managed runtime argv contains NUL".into());
    }
    let mut ephemeral = BTreeSet::new();
    for name in &spec.ephemeral_environment {
        if !EPHEMERAL_ENV_KEYS.contains(&name.as_str()) || !ephemeral.insert(name) {
            return Err(format!(
                "managed runtime ephemeral environment is invalid: {name}"
            ));
        }
    }
    for (name, value) in &spec.environment {
        if !STATIC_ENV_KEYS.contains(&name.as_str())
            || value.contains('\0')
            || spec.ephemeral_environment.contains(name)
        {
            return Err(format!(
                "managed runtime static environment is invalid: {name}"
            ));
        }
    }
    Ok(())
}

fn checked_relative(value: &str) -> Result<PathBuf, String> {
    if value.is_empty() || value.contains('\0') || value.contains('\\') {
        return Err("managed runtime path is invalid".into());
    }
    let path = PathBuf::from(value);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("managed runtime path must be a contained relative path".into());
    }
    Ok(path)
}

fn inventory_paths(root: &Path) -> Result<BTreeSet<String>, String> {
    let mut paths = BTreeSet::new();
    walk_files(root, root, &mut |relative, _path, metadata| {
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "managed runtime contains a symbolic link: {}",
                relative.display()
            ));
        }
        if metadata.is_file() && relative != Path::new(MANIFEST_NAME) {
            paths.insert(path_wire(relative)?);
        }
        Ok(())
    })?;
    Ok(paths)
}

fn inventory(root: &Path) -> Result<Vec<FileSeal>, String> {
    let mut files = Vec::new();
    walk_files(root, root, &mut |relative, path, metadata| {
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "managed runtime candidate contains a symbolic link: {}",
                relative.display()
            ));
        }
        if metadata.is_file() && relative != Path::new(MANIFEST_NAME) {
            files.push(FileSeal {
                path: path_wire(relative)?,
                size: metadata.len(),
                sha256: hash_file(path)?,
                executable: executable(metadata),
            });
        }
        Ok(())
    })?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

fn walk_files(
    root: &Path,
    directory: &Path,
    visit: &mut impl FnMut(&Path, &Path, &fs::Metadata) -> Result<(), String>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read managed runtime directory: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot enumerate managed runtime directory: {error}"))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect managed runtime entry: {error}"))?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "managed runtime entry escaped its root")?;
        visit(relative, &path, &metadata)?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            walk_files(root, &path, visit)?;
        } else if !metadata.is_file() && !metadata.file_type().is_symlink() {
            return Err("managed runtime contains an unsupported filesystem entry".into());
        }
    }
    Ok(())
}

fn path_wire(path: &Path) -> Result<String, String> {
    let mut pieces = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => pieces.push(
                value
                    .to_str()
                    .ok_or("managed runtime path is not valid UTF-8")?,
            ),
            _ => return Err("managed runtime path is not relative".into()),
        }
    }
    Ok(pieces.join("/"))
}

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("cannot open managed runtime file for hashing: {error}"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot hash managed runtime file: {error}"))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn generation_id(
    target: &str,
    provenance: &BTreeMap<String, String>,
    services: &BTreeMap<String, CommandSpec>,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&GenerationIdentity {
        schema_version: SCHEMA_VERSION,
        target,
        provenance,
        services,
    })
    .map_err(|error| format!("cannot serialize runtime generation identity: {error}"))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn read_regular_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect managed runtime stamp: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max {
        return Err("managed runtime stamp is not a bounded regular file".into());
    }
    fs::read(path).map_err(|error| format!("cannot read managed runtime stamp: {error}"))
}

#[cfg(unix)]
fn executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_metadata: &fs::Metadata) -> bool {
    false
}

/// Seal a fully built candidate.  Callers must have already downloaded every
/// external artifact through the native pinned downloader and completed all
/// offline setup/warm-up checks.  The promotion validator calls [`resolve_at`]
/// before and after the rename.
pub(crate) fn seal_candidate(
    root: &Path,
    target: &str,
    provenance: BTreeMap<String, String>,
    services: BTreeMap<String, CommandSpec>,
) -> Result<String, String> {
    if root.join(MANIFEST_NAME).exists() {
        fs::remove_file(root.join(MANIFEST_NAME))
            .map_err(|error| format!("cannot replace runtime manifest: {error}"))?;
    }
    for spec in services.values() {
        validate_command_spec(spec)?;
    }
    let generation = generation_id(target, &provenance, &services)?;
    write_synced(
        &root.join(GENERATION_NAME),
        format!("{generation}\n").as_bytes(),
    )?;
    let manifest = LaunchManifest {
        schema_version: SCHEMA_VERSION,
        target: target.into(),
        generation: generation.clone(),
        provenance,
        services,
        files: inventory(root)?,
    };
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot serialize runtime manifest: {error}"))?;
    write_synced(&root.join(MANIFEST_NAME), &bytes)?;
    for service in manifest.services.keys() {
        resolve_at(root, service, target)?;
    }
    Ok(generation)
}

pub(crate) fn validate_candidate(root: &Path, service: Service) -> Result<(), String> {
    resolve_at(root, service.wire_name(), &host_target()).map(|_| ())
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = File::create(path)
        .map_err(|error| format!("cannot create managed runtime stamp: {error}"))?;
    file.write_all(bytes)
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("cannot sync managed runtime stamp: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lsdj-managed-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn command(program: &str) -> CommandSpec {
        CommandSpec {
            program: program.into(),
            argv: vec!["--model-free".into()],
            cwd: "runtime".into(),
            environment: BTreeMap::from([
                ("HF_HUB_OFFLINE".into(), "1".into()),
                ("PYTHONNOUSERSITE".into(), "1".into()),
            ]),
            ephemeral_environment: vec!["LSDJ_ASSETS_HOME".into(), "LSDJ_API_CAPABILITY".into()],
        }
    }

    fn install(root: &Path, services: &[&str]) {
        fs::create_dir_all(root.join("runtime/bin")).unwrap();
        fs::write(root.join("runtime/bin/python"), b"verified interpreter").unwrap();
        fs::write(root.join("runtime/backend.py"), b"verified adapter").unwrap();
        let services = services
            .iter()
            .map(|name| ((*name).into(), command("runtime/bin/python")))
            .collect();
        seal_candidate(
            root,
            "x86_64-pc-windows-msvc",
            BTreeMap::from([
                ("requirementsSha256".into(), "a".repeat(64)),
                ("sourceRevision".into(), "b".repeat(40)),
            ]),
            services,
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn install_spawnable(root: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let program = root.join("runtime/bin/backend");
        fs::create_dir_all(program.parent().unwrap()).unwrap();
        fs::write(
            &program,
            b"#!/bin/sh\nprintf '%s|%s|%s|%s|%s' \"$1\" \"$2\" \"${LSDJ_API_CAPABILITY-unset}\" \"${LSDJ_WORKER_LAUNCH_TOKEN-unset}\" \"${HOME-unset}\"\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&program, permissions).unwrap();
        fs::write(root.join("runtime/backend.py"), b"verified adapter").unwrap();
        let spec = CommandSpec {
            program: "runtime/bin/backend".into(),
            argv: vec!["fixed".into()],
            cwd: "runtime".into(),
            environment: BTreeMap::from([("HF_HUB_OFFLINE".into(), "1".into())]),
            ephemeral_environment: vec![
                "LSDJ_API_CAPABILITY".into(),
                "LSDJ_WORKER_LAUNCH_TOKEN".into(),
                "SYSTEMROOT".into(),
                "WINDIR".into(),
                "TEMP".into(),
                "TMP".into(),
            ],
        };
        seal_candidate(
            root,
            &host_target(),
            BTreeMap::from([("sourceRevision".into(), "b".repeat(40))]),
            BTreeMap::from([("mrt2".into(), spec)]),
        )
        .unwrap();
    }

    #[cfg(unix)]
    fn promote_spawnable(label: &str) -> (PathBuf, PathBuf) {
        let root = root(label);
        let candidate = root.join("candidate");
        let home = root.join("home");
        let backup = root.join("backup");
        install_spawnable(&candidate);
        crate::runtime_installer::promotion::promote(&candidate, &home, &backup, |path| {
            resolve_at(path, "mrt2", &host_target()).map(|_| ())
        })
        .unwrap();
        (root, home)
    }

    #[test]
    fn clean_host_fails_closed_and_install_produces_structured_commands() {
        let root = root("clean host with spaces 资产");
        assert!(resolve_at(&root, "mrt2", "x86_64-pc-windows-msvc").is_err());
        install(&root, &["mrt2", "sa3-tflite"]);
        for service in ["mrt2", "sa3-tflite"] {
            let resolved = resolve_at(&root, service, "x86_64-pc-windows-msvc").unwrap();
            assert!(resolved.program().is_absolute());
            let command = resolved
                .into_command(
                    [OsString::from("--port"), OsString::from("1234")],
                    [(
                        OsString::from("LSDJ_ASSETS_HOME"),
                        root.as_os_str().to_owned(),
                    )],
                )
                .unwrap();
            assert_eq!(command.get_program(), root.join("runtime/bin/python"));
            assert_eq!(
                command
                    .get_args()
                    .map(|value| value.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                ["--model-free", "--port", "1234"]
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn tamper_stale_target_unknown_schema_and_unexpected_files_fail_closed() {
        let root = root("tamper");
        install(&root, &["mrt2"]);
        fs::write(root.join("runtime/backend.py"), b"tampered adapter").unwrap();
        assert!(resolve_at(&root, "mrt2", "x86_64-pc-windows-msvc")
            .unwrap_err()
            .contains("digest changed"));

        fs::remove_dir_all(&root).unwrap();
        install(&root, &["mrt2"]);
        fs::write(root.join(GENERATION_NAME), b"stale\n").unwrap();
        assert!(resolve_at(&root, "mrt2", "x86_64-pc-windows-msvc")
            .unwrap_err()
            .contains("generation stamp"));

        fs::remove_dir_all(&root).unwrap();
        install(&root, &["mrt2"]);
        assert!(resolve_at(&root, "mrt2", "x86_64-unknown-linux-gnu")
            .unwrap_err()
            .contains("target mismatch"));

        let manifest_path = root.join(MANIFEST_NAME);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["schemaVersion"] = 999.into();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(resolve_at(&root, "mrt2", "x86_64-pc-windows-msvc")
            .unwrap_err()
            .contains("schema"));

        fs::remove_dir_all(&root).unwrap();
        install(&root, &["mrt2"]);
        fs::write(root.join("runtime/injected.py"), b"untrusted").unwrap();
        assert!(resolve_at(&root, "mrt2", "x86_64-pc-windows-msvc")
            .unwrap_err()
            .contains("unexpected"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn traversal_symlink_and_undeclared_secret_environment_are_rejected() {
        let root = root("escape");
        fs::create_dir_all(root.join("runtime/bin")).unwrap();
        fs::write(root.join("runtime/bin/python"), b"python").unwrap();
        let mut services = BTreeMap::new();
        services.insert("mrt2".into(), command("../system-python"));
        assert!(
            seal_candidate(&root, "x86_64-pc-windows-msvc", BTreeMap::new(), services)
                .unwrap_err()
                .contains("contained relative")
        );

        fs::remove_dir_all(&root).unwrap();
        install(&root, &["mrt2"]);
        for name in [
            "HF_TOKEN",
            "HUGGING_FACE_HUB_TOKEN",
            "PATH",
            "HOME",
            "PYTHONPATH",
            "LSDJ_GENERATION_CMD",
            "LSDJ_SIDECAR_CMD",
            "LSDJ_ALLOW_UNVERIFIED_MRT2_CUDA",
            "LSDJ_ALLOW_UNVERIFIED_SA3_CUDA",
        ] {
            let resolved = resolve_at(&root, "mrt2", "x86_64-pc-windows-msvc").unwrap();
            assert!(resolved
                .into_command([], [(OsString::from(name), OsString::from("secret"))])
                .unwrap_err()
                .contains("undeclared"));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_dir_all(&root).unwrap();
            install(&root, &["mrt2"]);
            fs::remove_file(root.join("runtime/backend.py")).unwrap();
            symlink("/etc/hosts", root.join("runtime/backend.py")).unwrap();
            assert!(resolve_at(&root, "mrt2", "x86_64-pc-windows-msvc").is_err());
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn promotion_recovery_preserves_last_known_good_generation() {
        let root = root("promotion");
        let home = root.join("home");
        let backup = root.join("backup");
        let candidate = root.join("candidate");
        fs::create_dir_all(&root).unwrap();
        install(&home, &["mrt2", "sa3-tflite"]);
        let old_generation = resolve_at(&home, "mrt2", "x86_64-pc-windows-msvc")
            .unwrap()
            .generation()
            .to_string();
        install(&candidate, &["mrt2", "sa3-tflite"]);
        fs::write(candidate.join("runtime/backend.py"), b"candidate crashed").unwrap();
        let validate = |path: &Path| {
            resolve_at(path, "mrt2", "x86_64-pc-windows-msvc")
                .and_then(|_| resolve_at(path, "sa3-tflite", "x86_64-pc-windows-msvc"))
                .map(|_| ())
        };
        assert!(
            crate::runtime_installer::promotion::promote(&candidate, &home, &backup, validate)
                .is_err()
        );
        assert_eq!(
            resolve_at(&home, "mrt2", "x86_64-pc-windows-msvc")
                .unwrap()
                .generation(),
            old_generation
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn promoted_runtime_revalidates_at_the_real_spawn_boundary() {
        let (root, home) = promote_spawnable("spawn unicode 资产");
        let output = resolve_at(&home, "mrt2", &host_target())
            .unwrap()
            .into_command(
                [OsString::from("dynamic")],
                [
                    (
                        OsString::from("LSDJ_API_CAPABILITY"),
                        OsString::from("capability-secret"),
                    ),
                    (
                        OsString::from("LSDJ_WORKER_LAUNCH_TOKEN"),
                        OsString::from("worker-secret"),
                    ),
                ],
            )
            .unwrap()
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "fixed|dynamic|capability-secret|worker-secret|unset"
        );
        let _ = fs::remove_dir_all(root);

        let assert_race_rejected = |label: &str, mutate: &dyn Fn(&Path)| {
            let (root, home) = promote_spawnable(label);
            let verified = resolve_at(&home, "mrt2", &host_target()).unwrap();
            mutate(&home);
            assert!(verified
                .into_command([], std::iter::empty::<(OsString, OsString)>())
                .is_err());
            let _ = fs::remove_dir_all(root);
        };

        assert_race_rejected("spawn missing", &|home| {
            fs::remove_file(home.join("runtime/bin/backend")).unwrap();
        });
        assert_race_rejected("spawn tampered", &|home| {
            fs::write(home.join("runtime/backend.py"), b"tampered adapter").unwrap();
        });
        assert_race_rejected("spawn unexpected", &|home| {
            fs::write(home.join("runtime/injected.py"), b"untrusted").unwrap();
        });
        assert_race_rejected("spawn stale target", &|home| {
            let path = home.join(MANIFEST_NAME);
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            manifest["target"] = "stale-unknown-target".into();
            fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        });
        assert_race_rejected("spawn symlink", &|home| {
            use std::os::unix::fs::symlink;
            let program = home.join("runtime/bin/backend");
            fs::remove_file(&program).unwrap();
            symlink("/bin/true", program).unwrap();
        });
    }

    #[test]
    fn windows_host_essentials_are_explicit_and_other_host_values_stay_cleared() {
        let root = root("windows host environment");
        install(&root, &["mrt2"]);
        let manifest_path = root.join(MANIFEST_NAME);
        let mut manifest: LaunchManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let spec = manifest.services.get_mut("mrt2").unwrap();
        spec.ephemeral_environment.extend(
            ["SYSTEMROOT", "WINDIR", "TEMP", "TMP"]
                .into_iter()
                .map(str::to_string),
        );
        fs::remove_file(&manifest_path).unwrap();
        seal_candidate(
            &root,
            "x86_64-pc-windows-msvc",
            manifest.provenance,
            manifest.services,
        )
        .unwrap();

        let mut verified = resolve_at(&root, "mrt2", "x86_64-pc-windows-msvc").unwrap();
        verified.host_environment = vec![
            (OsString::from("SYSTEMROOT"), OsString::from(r"C:\Windows")),
            (OsString::from("WINDIR"), OsString::from(r"C:\Windows")),
            (OsString::from("TEMP"), OsString::from(r"C:\LSDJ\tmp")),
            (OsString::from("TMP"), OsString::from(r"C:\LSDJ\tmp")),
        ];
        let command = verified
            .into_command(
                [],
                [(OsString::from("LSDJ_API_CAPABILITY"), OsString::from("cap"))],
            )
            .unwrap();
        let environment: BTreeMap<_, _> = command
            .get_envs()
            .filter_map(|(name, value)| {
                value.map(|value| {
                    (
                        name.to_string_lossy().into_owned(),
                        value.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect();
        assert_eq!(environment.get("SYSTEMROOT"), Some(&r"C:\Windows".into()));
        assert_eq!(environment.get("WINDIR"), Some(&r"C:\Windows".into()));
        assert_eq!(environment.get("TEMP"), Some(&r"C:\LSDJ\tmp".into()));
        assert_eq!(environment.get("TMP"), Some(&r"C:\LSDJ\tmp".into()));
        assert_eq!(environment.get("LSDJ_API_CAPABILITY"), Some(&"cap".into()));
        for forbidden in [
            "PATH",
            "HOME",
            "HF_TOKEN",
            "HUGGING_FACE_HUB_TOKEN",
            "PYTHONPATH",
            "LSDJ_GENERATION_CMD",
            "LSDJ_SIDECAR_CMD",
            "LSDJ_ALLOW_UNVERIFIED_MRT2_CUDA",
            "LSDJ_ALLOW_UNVERIFIED_SA3_CUDA",
        ] {
            assert!(!environment.contains_key(forbidden));
        }
        let _ = fs::remove_dir_all(root);
    }
}
