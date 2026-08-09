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
use std::process::{Command, ExitStatus};
use std::sync::Mutex;
use std::time::Duration;

use crate::child_process::{Readiness, SupervisedChild};

/// The supervised generation server: its chosen loopback port (exposed to the
/// webview via `app_info`) and the child process. Held in Tauri managed state;
/// dropping it kills the child.
pub struct GenerationServer {
    state: Mutex<GenerationProcessState>,
}

trait GenerationProcess: Send {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>>;
    fn shutdown(&mut self) -> io::Result<()>;
}

impl GenerationProcess for SupervisedChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        SupervisedChild::try_wait(self)
    }

    fn shutdown(&mut self) -> io::Result<()> {
        let report = SupervisedChild::shutdown(self, Duration::from_millis(500))?;
        crate::child_process::log_shutdown("generation server", Ok(report));
        Ok(())
    }
}

enum GenerationProcessState {
    Stopped,
    Running {
        port: u16,
        capability: String,
        process: Box<dyn GenerationProcess>,
    },
    /// Shutdown was requested, but the supervisor could not prove that the
    /// complete process tree was reaped. The handle stays owned here so every
    /// later quiesce/resume can retry; an installer must not rename through it.
    Uncertain {
        process: Box<dyn GenerationProcess>,
        was_running: bool,
    },
}

struct GenerationSpawnFailure {
    error: io::Error,
    uncertain_process: Option<Box<dyn GenerationProcess>>,
}

impl GenerationSpawnFailure {
    fn reaped(error: io::Error) -> Self {
        Self {
            error,
            uncertain_process: None,
        }
    }
}

impl From<io::Error> for GenerationSpawnFailure {
    fn from(error: io::Error) -> Self {
        Self::reaped(error)
    }
}

impl GenerationServer {
    /// Spawn the generation server — started with the app. Never fails the app: a
    /// failed spawn yields `port() == None` and generation is simply unreachable (the
    /// webview surfaces that as fetch errors).
    pub fn start() -> GenerationServer {
        let server = GenerationServer {
            state: Mutex::new(GenerationProcessState::Stopped),
        };
        if let Err(error) = server.resume() {
            // A fresh managed install intentionally has no runtime yet. The
            // model manager calls `resume` immediately after first promotion.
            eprintln!("lsdj-app: generation server unavailable: {error}");
        }
        server
    }

    fn spawn(
        capability: &str,
    ) -> Result<(u16, Box<dyn GenerationProcess>), GenerationSpawnFailure> {
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
        let readiness = child.wait_for_readiness(Duration::from_millis(1500), || {
            Ok(TcpStream::connect(addr).is_ok())
        });
        finish_generation_startup(port, Box::new(child), readiness)
    }

    fn resume_with_spawn(
        &self,
        spawn: impl FnOnce(
            &str,
        ) -> Result<(u16, Box<dyn GenerationProcess>), GenerationSpawnFailure>,
    ) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::mem::replace(&mut *state, GenerationProcessState::Stopped);
        match previous {
            GenerationProcessState::Stopped => {}
            GenerationProcessState::Running {
                port,
                capability,
                mut process,
            } => match process.try_wait() {
                Ok(None) => {
                    *state = GenerationProcessState::Running {
                        port,
                        capability,
                        process,
                    };
                    return Ok(());
                }
                Ok(Some(_)) => {}
                Err(error) => {
                    *state = GenerationProcessState::Uncertain {
                        process,
                        was_running: true,
                    };
                    return Err(error);
                }
            },
            GenerationProcessState::Uncertain {
                mut process,
                was_running,
            } => {
                if let Err(error) = process.shutdown() {
                    *state = GenerationProcessState::Uncertain {
                        process,
                        was_running,
                    };
                    return Err(error);
                }
            }
        }
        let capability = crate::local_auth::generate_capability();
        let (port, process) = match spawn(&capability) {
            Ok(spawned) => spawned,
            Err(spawn) => {
                if let Some(process) = spawn.uncertain_process {
                    *state = GenerationProcessState::Uncertain {
                        process,
                        was_running: false,
                    };
                }
                return Err(spawn.error);
            }
        };
        println!("lsdj-app: generation server on 127.0.0.1:{port}");
        *state = GenerationProcessState::Running {
            port,
            capability,
            process,
        };
        Ok(())
    }

    /// Return the port and capability from one lock acquisition. Neither half is
    /// ever observable without the other across promotion/resume transitions.
    pub fn connection(&self) -> Option<(u16, String)> {
        match &*self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            GenerationProcessState::Running {
                port, capability, ..
            } => Some((*port, capability.clone())),
            GenerationProcessState::Stopped | GenerationProcessState::Uncertain { .. } => None,
        }
    }

    /// Start (or recover) the service from the currently promoted verified
    /// generation. A running healthy child is left untouched. This is called on
    /// startup and after every managed SA3 promotion/rollback.
    pub fn resume(&self) -> io::Result<()> {
        self.resume_with_spawn(Self::spawn)
    }

    /// Stop and reap the service before its managed generation is renamed.
    /// Returns whether a live child was present so tests/lifecycle diagnostics
    /// can distinguish first install from an update.
    pub fn quiesce(&self) -> io::Result<bool> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let previous = std::mem::replace(&mut *state, GenerationProcessState::Stopped);
        let (mut process, was_running) = match previous {
            GenerationProcessState::Stopped => return Ok(false),
            GenerationProcessState::Running { process, .. } => (process, true),
            GenerationProcessState::Uncertain {
                process,
                was_running,
            } => (process, was_running),
        };
        match process.shutdown() {
            Ok(()) => Ok(was_running),
            Err(error) => {
                *state = GenerationProcessState::Uncertain {
                    process,
                    was_running,
                };
                Err(error)
            }
        }
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

fn finish_generation_startup(
    port: u16,
    mut process: Box<dyn GenerationProcess>,
    readiness: io::Result<Readiness>,
) -> Result<(u16, Box<dyn GenerationProcess>), GenerationSpawnFailure> {
    match readiness {
        Ok(Readiness::Ready | Readiness::TimedOut) => {
            // Preserve the existing macOS contract: a slow-but-running
            // service is advertised optimistically after the bounded wait.
            Ok((port, process))
        }
        Ok(Readiness::Exited(status)) => Err(GenerationSpawnFailure::reaped(io::Error::other(
            format!("generation server exited before binding ({status})"),
        ))),
        Err(readiness_error) => match process.shutdown() {
            Ok(()) => Err(GenerationSpawnFailure::reaped(readiness_error)),
            Err(cleanup_error) => Err(GenerationSpawnFailure {
                error: io::Error::new(
                    readiness_error.kind(),
                    format!(
                        "{readiness_error}; generation startup cleanup also failed: {cleanup_error}"
                    ),
                ),
                uncertain_process: Some(process),
            }),
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
        assert_eq!(server.connection(), None);

        std::env::remove_var("LSDJ_GENERATION_CMD");
    }
}

#[cfg(test)]
mod process_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    struct RetryProcess {
        shutdowns: Arc<AtomicUsize>,
    }

    impl GenerationProcess for RetryProcess {
        fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            Ok(None)
        }

        fn shutdown(&mut self) -> io::Result<()> {
            if self.shutdowns.fetch_add(1, Ordering::AcqRel) == 0 {
                Err(io::Error::other("first reap is uncertain"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn failed_sa3_reap_retains_ownership_until_a_positive_retry() {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let server = GenerationServer {
            state: Mutex::new(GenerationProcessState::Running {
                port: 4321,
                capability: "capability".to_string(),
                process: Box::new(RetryProcess {
                    shutdowns: shutdowns.clone(),
                }),
            }),
        };

        assert!(server.quiesce().is_err());
        assert_eq!(server.connection(), None);
        assert!(matches!(
            &*server.state.lock().unwrap(),
            GenerationProcessState::Uncertain { .. }
        ));
        assert_eq!(shutdowns.load(Ordering::Acquire), 1);

        assert!(matches!(server.quiesce(), Ok(true)));
        assert!(matches!(
            &*server.state.lock().unwrap(),
            GenerationProcessState::Stopped
        ));
        assert_eq!(shutdowns.load(Ordering::Acquire), 2);
    }

    #[test]
    fn readiness_error_with_failed_cleanup_retains_ownership_until_second_shutdown() {
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let startup = finish_generation_startup(
            4321,
            Box::new(RetryProcess {
                shutdowns: shutdowns.clone(),
            }),
            Err(io::Error::other("readiness OS error")),
        );
        let failure = match startup {
            Err(failure) => failure,
            Ok(_) => panic!("readiness error must fail startup"),
        };
        assert!(failure.error.to_string().contains("readiness OS error"));
        assert!(failure
            .error
            .to_string()
            .contains("startup cleanup also failed"));
        assert!(failure.uncertain_process.is_some());
        assert_eq!(shutdowns.load(Ordering::Acquire), 1);

        let server = GenerationServer {
            state: Mutex::new(GenerationProcessState::Stopped),
        };
        let error = server
            .resume_with_spawn(move |_| Err(failure))
            .unwrap_err();
        assert!(error.to_string().contains("readiness OS error"));
        assert_eq!(server.connection(), None);
        assert!(matches!(
            &*server.state.lock().unwrap(),
            GenerationProcessState::Uncertain { .. }
        ));

        // The startup cleanup was the first shutdown attempt. Quiesce owns the
        // second attempt and cannot expose Stopped until that positive reap.
        assert!(matches!(server.quiesce(), Ok(false)));
        assert!(matches!(
            &*server.state.lock().unwrap(),
            GenerationProcessState::Stopped
        ));
        assert_eq!(shutdowns.load(Ordering::Acquire), 2);
    }
}
