//! Generation server supervision (Phase 2 / native gap 2).
//!
//! The native shell hosts the realtime decks (the inference sidecars, [`crate::sidecar`])
//! and serves the frontend from the Tauri asset host, so FastAPI no longer serves
//! the UI. This module supervises the Stable Audio 3 generation service on a
//! loopback port. In managed Linux/Windows builds Magenta rendering belongs to
//! the Rust gateway (`crate::magenta_gateway`), so this child receives no MRT2
//! paths or dependencies. The bundled macOS backend retains its existing
//! combined `/api/render` + `/api/generate` behavior.
//!
//! Mirrors the sidecar's spawn/supervise/Drop-kill pattern. Started with the app; a
//! failed spawn just leaves generation unreachable (the UI already surfaces those as
//! fetch errors), with `port() == None`.

use std::io;
use std::net::{TcpListener, TcpStream};
#[cfg(not(feature = "managed-runtime"))]
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use crate::child_process::{Readiness, SupervisedChild};

/// The supervised generation server: its chosen loopback port (exposed to the
/// webview via `app_info`) and the child process. Held in Tauri managed state;
/// dropping it kills the child.
pub struct GenerationServer {
    state: Mutex<GenerationState>,
}

struct GenerationState {
    port: Option<u16>,
    capability: Option<String>,
    child: Option<SupervisedChild>,
}

impl GenerationServer {
    /// Spawn the generation server — started with the app. Never fails the app: a
    /// failed spawn yields `port() == None` and generation is simply unreachable (the
    /// webview surfaces that as fetch errors).
    pub fn start() -> GenerationServer {
        let server = GenerationServer {
            state: Mutex::new(GenerationState {
                port: None,
                capability: None,
                child: None,
            }),
        };
        if let Err(error) = server.resume() {
            // A fresh managed install intentionally has no runtime yet. The
            // model manager calls `resume` immediately after first promotion.
            eprintln!("lsdj-app: generation server unavailable: {error}");
        }
        server
    }

    fn spawn(capability: &str) -> io::Result<(u16, SupervisedChild)> {
        // Pick a free loopback port, then hand it to the child (uvicorn binds it).
        // The brief drop→rebind window on loopback is benign.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            listener.local_addr()?.port()
        };
        let mut command = generation_command(port, capability)?;
        let mut child = crate::child_process::spawn_grouped(&mut command)?;

        // Confirm the child actually came up before advertising the port — a
        // failed launch (bad CWD / import error) or a lost port race would
        // otherwise leave the app pointing the webview at a dead port. Bounded so
        // a slow-but-working server is reported optimistically rather than
        // blocking the window; a child that EXITS is reported as a failure.
        let addr = ("127.0.0.1", port);
        match child.wait_for_readiness(Duration::from_millis(1500), || {
            Ok(TcpStream::connect(addr).is_ok())
        })? {
            Readiness::Ready | Readiness::TimedOut => {
                // Preserve the existing macOS contract: a slow-but-running
                // service is advertised optimistically after the bounded wait.
                Ok((port, child))
            }
            Readiness::Exited(status) => Err(io::Error::other(format!(
                "generation server exited before binding ({status})"
            ))),
        }
    }

    /// The loopback port the generation server bound, or `None` if disabled / not
    /// running. The webview reads this through `app_info` to build the API base URL.
    pub fn port(&self) -> Option<u16> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .port
    }

    /// The in-memory capability paired with [`port`](Self::port). Never persisted.
    pub fn capability(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .capability
            .clone()
    }

    /// Start (or recover) the service from the currently promoted verified
    /// generation. A running healthy child is left untouched. This is called on
    /// startup and after every managed SA3 promotion/rollback.
    pub fn resume(&self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(child) = state.child.as_mut() {
            if child.try_wait()?.is_none() {
                return Ok(());
            }
            state.child = None;
            state.port = None;
            state.capability = None;
        }
        let capability = crate::local_auth::generate_capability();
        let (port, child) = Self::spawn(&capability)?;
        println!("lsdj-app: generation server on 127.0.0.1:{port}");
        state.port = Some(port);
        state.capability = Some(capability);
        state.child = Some(child);
        Ok(())
    }

    /// Stop and reap the service before its managed generation is renamed.
    /// Returns whether a live child was present so tests/lifecycle diagnostics
    /// can distinguish first install from an update.
    pub fn quiesce(&self) -> io::Result<bool> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.port = None;
        state.capability = None;
        let Some(mut child) = state.child.take() else {
            return Ok(false);
        };
        let report = child.shutdown(Duration::from_millis(500))?;
        crate::child_process::log_shutdown("generation server", Ok(report));
        Ok(true)
    }

    /// Kill the generation server child. Called explicitly from the app's
    /// `RunEvent::Exit` handler because Tauri does NOT drop managed state on a
    /// macOS quit (`process::exit` skips destructors), so [`Drop`] alone would
    /// leak the process.
    pub fn shutdown(&self) {
        if let Err(error) = self.quiesce() {
            crate::child_process::log_shutdown("generation server", Err(error));
        }
    }
}

impl Drop for GenerationServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Build the command that launches the FastAPI generation server. A release uses
/// `LSDJ_BACKEND_BIN --generation-server`; dev is overridable via
/// `LSDJ_GENERATION_CMD` and defaults to `uv run python -m lsdj.controller`.
/// `--port` is always appended.
pub fn generation_command(port: u16, capability: &str) -> io::Result<Command> {
    #[cfg(feature = "managed-runtime")]
    {
        use std::ffi::OsString;

        let paths = crate::platform_paths::get();
        let ephemeral = paths
            .backend_env()
            .into_iter()
            // The managed SA3 interpreter has its own dependency closure and
            // receives no MRT2 location. This makes accidental `/api/render`
            // use fail closed instead of coupling the services again.
            .filter(|(name, _)| name.to_str() != Some("MAGENTA_HOME"))
            .chain(std::iter::once((
                OsString::from("LSDJ_API_CAPABILITY"),
                OsString::from(capability),
            )));
        crate::managed_runtime::resolve(
            paths.assets(),
            crate::managed_runtime::Service::Sa3,
        )
        .and_then(|resolved| {
            resolved.into_command(["--port".into(), port.to_string().into()], ephemeral)
        })
        .map_err(io::Error::other)
    }

    // The release bundle shares one frozen dependency tree with the deck
    // sidecars. Its dispatcher needs an explicit mode because both CLIs accept
    // `--port`; the exact OsString also preserves paths containing spaces.
    #[cfg(not(feature = "managed-runtime"))]
    if let Some(program) = std::env::var_os("LSDJ_BACKEND_BIN") {
        let mut cmd = Command::new(program);
        cmd.env("LSDJ_API_CAPABILITY", capability);
        cmd.args(["--generation-server", "--port", &port.to_string()]);
        return Ok(cmd);
    }

    #[cfg(not(feature = "managed-runtime"))]
    {
        let overridden = std::env::var("LSDJ_GENERATION_CMD");
        let spec = overridden
            .clone()
            .unwrap_or_else(|_| "uv run python -m lsdj.controller".to_string());
        let mut parts = spec.split_whitespace();
        let program = parts.next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "empty LSDJ_GENERATION_CMD")
        })?;
        let mut cmd = Command::new(program);
        cmd.env("LSDJ_API_CAPABILITY", capability);
        cmd.args(parts);
        cmd.args(["--port", &port.to_string()]);
        if overridden.is_err() {
            // The default `uv run` needs the backend project dir as its CWD. A packaged
            // build returned through LSDJ_BACKEND_BIN above and never reaches this path.
            cmd.current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../backend"));
        }
        Ok(cmd)
    }
}

#[cfg(all(test, not(feature = "managed-runtime")))]
mod tests {
    use super::*;

    #[test]
    fn generation_command_appends_flags_and_a_failed_spawn_yields_no_port() {
        // SAFETY-ish: no other test reads LSDJ_GENERATION_CMD, so this single test
        // owns it for both assertions (kept in one fn to avoid a parallel env race).
        std::env::set_var("LSDJ_GENERATION_CMD", "echo hi");

        // The override is split into program + args with `--port` always appended.
        let cmd = generation_command(5123, "test-capability-0123456789abcdef").unwrap();
        let argv: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "echo");
        assert_eq!(argv, ["hi", "--port", "5123"]);
        assert!(cmd.get_envs().any(|(key, value)| {
            key == "LSDJ_API_CAPABILITY"
                && value.is_some_and(|value| value == "test-capability-0123456789abcdef")
        }));

        // Now-always-on `start()` never fails the app: a command that exits without
        // binding the port (echo) degrades to no advertised port.
        let server = GenerationServer::start();
        assert_eq!(server.port(), None);

        std::env::remove_var("LSDJ_GENERATION_CMD");
    }
}
