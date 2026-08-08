//! Generation server supervision (Phase 2 / native gap 2).
//!
//! The native shell hosts the realtime decks (the inference sidecars, [`crate::sidecar`])
//! and serves the frontend from the Tauri asset host, so FastAPI no longer serves
//! the UI. But the Stable Audio 3 / Magenta pad+track GENERATION still lives behind
//! HTTP (`/api/render`, `/api/generate`). This module spawns the FastAPI generation
//! server on a loopback port — the controller is generation-only: no deck workers, no
//! static mount — and the webview fetches it via `getApiBaseUrl()`.
//!
//! Mirrors the sidecar's spawn/supervise/Drop-kill pattern. Started with the app; a
//! failed spawn just leaves generation unreachable (the UI already surfaces those as
//! fetch errors), with `port() == None`.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use crate::child_process::{Readiness, SupervisedChild};

/// The supervised generation server: its chosen loopback port (exposed to the
/// webview via `app_info`) and the child process. Held in Tauri managed state;
/// dropping it kills the child.
pub struct GenerationServer {
    port: Option<u16>,
    child: Mutex<Option<SupervisedChild>>,
}

impl GenerationServer {
    /// Spawn the generation server — started with the app. Never fails the app: a
    /// failed spawn yields `port() == None` and generation is simply unreachable (the
    /// webview surfaces that as fetch errors).
    pub fn start() -> GenerationServer {
        match Self::spawn() {
            Ok((port, child)) => {
                println!("lsdj-app: generation server on 127.0.0.1:{port}");
                GenerationServer {
                    port: Some(port),
                    child: Mutex::new(Some(child)),
                }
            }
            Err(e) => {
                eprintln!("lsdj-app: generation server spawn failed: {e}");
                GenerationServer {
                    port: None,
                    child: Mutex::new(None),
                }
            }
        }
    }

    fn spawn() -> io::Result<(u16, SupervisedChild)> {
        // Pick a free loopback port, then hand it to the child (uvicorn binds it).
        // The brief drop→rebind window on loopback is benign.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            listener.local_addr()?.port()
        };
        let mut command = generation_command(port)?;
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
        self.port
    }

    /// Kill the generation server child. Called explicitly from the app's
    /// `RunEvent::Exit` handler because Tauri does NOT drop managed state on a
    /// macOS quit (`process::exit` skips destructors), so [`Drop`] alone would
    /// leak the process.
    pub fn shutdown(&self) {
        if let Some(mut child) = self.child.lock().unwrap_or_else(|p| p.into_inner()).take() {
            crate::child_process::log_shutdown(
                "generation server",
                child.shutdown(Duration::from_millis(500)),
            );
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
pub fn generation_command(port: u16) -> io::Result<Command> {
    // The release bundle shares one frozen dependency tree with the deck
    // sidecars. Its dispatcher needs an explicit mode because both CLIs accept
    // `--port`; the exact OsString also preserves paths containing spaces.
    if let Some(program) = std::env::var_os("LSDJ_BACKEND_BIN") {
        let mut cmd = Command::new(program);
        cmd.args(["--generation-server", "--port", &port.to_string()]);
        return Ok(cmd);
    }

    // A portable package must wait for #110/#111's verified app-managed
    // adapter. Never turn a missing runtime into an implicit dependency on a
    // user's system Python, `uv`, Git, or shell.
    if !crate::runtime_launch::developer_fallback_allowed() {
        return Err(crate::runtime_launch::unavailable("stableAudio"));
    }

    let overridden = std::env::var("LSDJ_GENERATION_CMD");
    let spec = overridden
        .clone()
        .unwrap_or_else(|_| "uv run python -m lsdj.controller".to_string());
    let mut parts = spec.split_whitespace();
    let program = parts.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "empty LSDJ_GENERATION_CMD")
    })?;
    let mut cmd = Command::new(program);
    cmd.args(parts);
    cmd.args(["--port", &port.to_string()]);
    if overridden.is_err() {
        // The default `uv run` needs the backend project dir as its CWD. A packaged
        // build returned through LSDJ_BACKEND_BIN above and never reaches this path.
        cmd.current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../backend"));
    }
    Ok(cmd)
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
        let cmd = generation_command(5123).unwrap();
        let argv: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "echo");
        assert_eq!(argv, ["hi", "--port", "5123"]);

        // Now-always-on `start()` never fails the app: a command that exits without
        // binding the port (echo) degrades to no advertised port.
        let server = GenerationServer::start();
        assert_eq!(server.port(), None);

        std::env::remove_var("LSDJ_GENERATION_CMD");
    }

}
