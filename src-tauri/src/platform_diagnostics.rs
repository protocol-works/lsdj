//! Structured desktop/runtime diagnostics for platform support.
//!
//! The command returns facts and stable advisory codes, not prose. The webview
//! can localise those codes, support bundles can retain the evidence, and a
//! headless test can validate classification without pretending that CI has a
//! real PipeWire/ALSA/MIDI desktop.

use std::path::Path;

#[cfg(any(target_os = "linux", test))]
use std::collections::HashMap;

use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformDiagnostics {
    platform: &'static str,
    architecture: &'static str,
    runtime_mode: &'static str,
    developer_fallback_allowed: bool,
    roots: RootDiagnostics,
    linux: Option<LinuxDiagnostics>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RootDiagnostics {
    config: String,
    data: String,
    cache: String,
    assets: String,
    staging: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LinuxDiagnostics {
    distribution_id: Option<String>,
    distribution_version: Option<String>,
    distribution_support: &'static str,
    session_type: &'static str,
    audio_backend: &'static str,
    pipewire_socket_detected: bool,
    pulse_socket_detected: bool,
    alsa_devices_detected: bool,
    midi_sequencer_access: &'static str,
    advisories: Vec<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimePolicy {
    mode: &'static str,
    developer_fallback_allowed: bool,
}

/// Pure release-policy table. The feature pair is kept explicit so tests cover
/// every supported build shape without pretending the current host is macOS,
/// Windows, or Linux. The mutually-exclusive feature guard in `lib.rs` rejects
/// the otherwise ambiguous `(true, true)` build before this value is observed.
const fn runtime_policy(bundled_backend: bool, managed_runtime: bool) -> RuntimePolicy {
    if bundled_backend {
        RuntimePolicy {
            mode: "bundled",
            developer_fallback_allowed: false,
        }
    } else if managed_runtime {
        RuntimePolicy {
            mode: "managed",
            developer_fallback_allowed: false,
        }
    } else {
        RuntimePolicy {
            mode: "developer",
            developer_fallback_allowed: true,
        }
    }
}

const fn compiled_runtime_policy() -> RuntimePolicy {
    runtime_policy(
        cfg!(feature = "bundled-backend"),
        cfg!(feature = "managed-runtime"),
    )
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LinuxEvidence {
    pipewire_socket: bool,
    pulse_socket: bool,
    alsa_devices: bool,
    midi_sequencer: MidiSequencerAccess,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MidiSequencerAccess {
    Available,
    PermissionDenied,
    Missing,
}

#[cfg(any(target_os = "linux", test))]
impl MidiSequencerAccess {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::PermissionDenied => "permissionDenied",
            Self::Missing => "missing",
        }
    }
}

/// Return the host facts that explain Linux path, desktop-session, audio, MIDI,
/// and runtime-launch behavior. No probe opens an audio or MIDI stream.
#[tauri::command]
pub fn platform_diagnostics() -> PlatformDiagnostics {
    let paths = crate::platform_paths::get();
    let runtime = compiled_runtime_policy();
    let roots = RootDiagnostics {
        config: display(paths.config()),
        data: display(paths.data()),
        cache: display(paths.cache()),
        assets: display(paths.assets()),
        staging: display(paths.staging()),
    };
    PlatformDiagnostics {
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        runtime_mode: runtime.mode,
        developer_fallback_allowed: runtime.developer_fallback_allowed,
        roots,
        linux: collect_linux(),
    }
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(target_os = "linux")]
fn collect_linux() -> Option<LinuxDiagnostics> {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let distribution = parse_os_release(&os_release);
    let distribution_id = distribution.get("ID").cloned();
    let distribution_version = distribution.get("VERSION_ID").cloned();
    let distribution_support =
        distribution_support(distribution_id.as_deref(), distribution_version.as_deref());
    let session_type = session_type(|name| std::env::var(name).ok());

    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute());
    let evidence = LinuxEvidence {
        pipewire_socket: runtime_dir
            .as_ref()
            .is_some_and(|dir| dir.join("pipewire-0").exists()),
        pulse_socket: runtime_dir
            .as_ref()
            .is_some_and(|dir| dir.join("pulse/native").exists()),
        alsa_devices: Path::new("/dev/snd").is_dir(),
        midi_sequencer: midi_sequencer_access(Path::new("/dev/snd/seq")),
    };
    Some(linux_diagnostics(
        distribution_id,
        distribution_version,
        distribution_support,
        session_type,
        evidence,
    ))
}

#[cfg(not(target_os = "linux"))]
fn collect_linux() -> Option<LinuxDiagnostics> {
    None
}

#[cfg(any(target_os = "linux", test))]
fn linux_diagnostics(
    distribution_id: Option<String>,
    distribution_version: Option<String>,
    distribution_support: &'static str,
    session_type: &'static str,
    evidence: LinuxEvidence,
) -> LinuxDiagnostics {
    let audio_backend = if evidence.pipewire_socket {
        "pipewireAlsa"
    } else if evidence.pulse_socket {
        "pulseAlsa"
    } else {
        "alsa"
    };
    LinuxDiagnostics {
        distribution_id,
        distribution_version,
        distribution_support,
        session_type,
        audio_backend,
        pipewire_socket_detected: evidence.pipewire_socket,
        pulse_socket_detected: evidence.pulse_socket,
        alsa_devices_detected: evidence.alsa_devices,
        midi_sequencer_access: evidence.midi_sequencer.as_str(),
        advisories: advisory_codes(distribution_support, session_type, evidence),
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_os_release(content: &str) -> HashMap<String, String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            if key.is_empty()
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte == b'_')
            {
                return None;
            }
            let value = value.trim();
            let value = if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            Some((key.to_string(), value.chars().take(128).collect()))
        })
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn distribution_support(id: Option<&str>, version: Option<&str>) -> &'static str {
    if !id.is_some_and(|id| id.eq_ignore_ascii_case("ubuntu")) {
        return if id.is_some() { "community" } else { "unknown" };
    }
    match version.and_then(version_pair) {
        Some((major, minor)) if (major, minor) >= (22, 4) => "supported",
        Some(_) => "unsupportedVersion",
        None => "unknown",
    }
}

#[cfg(any(target_os = "linux", test))]
fn version_pair(version: &str) -> Option<(u32, u32)> {
    let mut pieces = version.split('.');
    let major = pieces.next()?.parse().ok()?;
    let minor = pieces.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
}

#[cfg(any(target_os = "linux", test))]
fn session_type(get: impl Fn(&str) -> Option<String>) -> &'static str {
    match get("XDG_SESSION_TYPE")
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("wayland") => "wayland",
        Some("x11") => "x11",
        _ if get("WAYLAND_DISPLAY").is_some() => "wayland",
        _ if get("DISPLAY").is_some() => "x11",
        _ => "unknown",
    }
}

#[cfg(any(target_os = "linux", test))]
fn advisory_codes(
    distribution_support: &str,
    session_type: &str,
    evidence: LinuxEvidence,
) -> Vec<&'static str> {
    let mut codes = Vec::new();
    if distribution_support != "supported" {
        codes.push("linux.distribution.notSupported");
    }
    if session_type == "unknown" {
        codes.push("linux.session.notDetected");
    }
    if !evidence.alsa_devices {
        codes.push("linux.audio.alsaDevicesMissing");
    }
    match evidence.midi_sequencer {
        MidiSequencerAccess::Available => {}
        MidiSequencerAccess::PermissionDenied => {
            codes.push("linux.midi.sequencerPermissionDenied");
        }
        MidiSequencerAccess::Missing => codes.push("linux.midi.sequencerMissing"),
    }
    codes
}

#[cfg(target_os = "linux")]
fn midi_sequencer_access(path: &Path) -> MidiSequencerAccess {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if !path.exists() {
        return MidiSequencerAccess::Missing;
    }
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return MidiSequencerAccess::PermissionDenied;
    };
    // `access` checks the real user's read/write permission without opening an
    // ALSA sequencer client or changing device state.
    if unsafe { libc::access(path.as_ptr(), libc::R_OK | libc::W_OK) } == 0 {
        MidiSequencerAccess::Available
    } else {
        MidiSequencerAccess::PermissionDenied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ubuntu_2204_and_newer_are_the_only_supported_distribution_contract() {
        assert_eq!(
            distribution_support(Some("ubuntu"), Some("22.04")),
            "supported"
        );
        assert_eq!(
            distribution_support(Some("Ubuntu"), Some("24.04")),
            "supported"
        );
        assert_eq!(
            distribution_support(Some("ubuntu"), Some("20.04")),
            "unsupportedVersion"
        );
        assert_eq!(
            distribution_support(Some("fedora"), Some("42")),
            "community"
        );
        assert_eq!(distribution_support(None, None), "unknown");
    }

    #[test]
    fn os_release_parser_does_not_evaluate_shell_syntax() {
        let parsed = parse_os_release(
            "ID=ubuntu\nVERSION_ID=\"22.04\"\nNAME='Ubuntu Linux'\nBAD-KEY=value\n",
        );
        assert_eq!(parsed.get("ID").map(String::as_str), Some("ubuntu"));
        assert_eq!(parsed.get("VERSION_ID").map(String::as_str), Some("22.04"));
        assert_eq!(parsed.get("NAME").map(String::as_str), Some("Ubuntu Linux"));
        assert!(!parsed.contains_key("BAD-KEY"));
    }

    #[test]
    fn desktop_session_uses_xdg_then_safe_display_fallbacks() {
        assert_eq!(
            session_type(|name| (name == "XDG_SESSION_TYPE").then(|| "wayland".into())),
            "wayland"
        );
        assert_eq!(
            session_type(|name| (name == "DISPLAY").then(|| ":99".into())),
            "x11"
        );
        assert_eq!(session_type(|_| None), "unknown");
    }

    #[test]
    fn diagnostics_report_transport_evidence_without_claiming_hardware() {
        assert_eq!(MidiSequencerAccess::Available.as_str(), "available");
        assert_eq!(MidiSequencerAccess::Missing.as_str(), "missing");
        let evidence = LinuxEvidence {
            pipewire_socket: true,
            pulse_socket: true,
            alsa_devices: false,
            midi_sequencer: MidiSequencerAccess::PermissionDenied,
        };
        let diagnostics = linux_diagnostics(
            Some("ubuntu".into()),
            Some("22.04".into()),
            "supported",
            "wayland",
            evidence,
        );
        assert_eq!(diagnostics.audio_backend, "pipewireAlsa");
        assert_eq!(
            diagnostics.advisories,
            [
                "linux.audio.alsaDevicesMissing",
                "linux.midi.sequencerPermissionDenied"
            ]
        );
    }

    #[test]
    fn runtime_policy_table_allows_fallback_only_for_featureless_development() {
        assert_eq!(
            runtime_policy(true, false),
            RuntimePolicy {
                mode: "bundled",
                developer_fallback_allowed: false,
            }
        );
        assert_eq!(
            runtime_policy(false, true),
            RuntimePolicy {
                mode: "managed",
                developer_fallback_allowed: false,
            }
        );
        assert_eq!(
            runtime_policy(false, false),
            RuntimePolicy {
                mode: "developer",
                developer_fallback_allowed: true,
            }
        );
    }

    #[test]
    fn compiled_runtime_policy_matches_the_selected_release_feature() {
        let expected = runtime_policy(
            cfg!(feature = "bundled-backend"),
            cfg!(feature = "managed-runtime"),
        );
        assert_eq!(compiled_runtime_policy(), expected);
        if cfg!(feature = "bundled-backend") || cfg!(feature = "managed-runtime") {
            assert!(!expected.developer_fallback_allowed);
        } else {
            assert_eq!(expected.mode, "developer");
            assert!(expected.developer_fallback_allowed);
        }
    }

    #[test]
    fn diagnostics_serialization_exposes_the_compiled_runtime_policy() {
        let runtime = compiled_runtime_policy();
        let value = serde_json::to_value(PlatformDiagnostics {
            platform: "test",
            architecture: "test-arch",
            runtime_mode: runtime.mode,
            developer_fallback_allowed: runtime.developer_fallback_allowed,
            roots: RootDiagnostics {
                config: "/config".into(),
                data: "/data".into(),
                cache: "/cache".into(),
                assets: "/assets".into(),
                staging: "/staging".into(),
            },
            linux: None,
        })
        .unwrap();
        assert_eq!(value["runtimeMode"], runtime.mode);
        assert_eq!(
            value["developerFallbackAllowed"],
            runtime.developer_fallback_allowed
        );
        assert!(value.get("runtime_mode").is_none());
    }
}
