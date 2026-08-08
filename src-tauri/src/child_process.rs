//! Shared process-tree supervision for every long-lived backend command.
//!
//! Development commands often have a wrapper topology (`uv` -> Python -> model
//! workers), so owning only [`Child`] is not enough. [`SupervisedChild`] keeps
//! the platform tree-lifetime primitive alive for exactly as long as the
//! service:
//!
//! - Unix children lead a fresh process group. A small, syscall-only watchdog
//!   inherited during spawn observes a close-on-exec pipe, and kills the group
//!   if the Rust host exits without running destructors.
//! - Windows children are created suspended, assigned to a kill-on-close Job
//!   Object, and only then resumed. Suspending closes the otherwise unavoidable
//!   race where the child could create an untracked descendant before job
//!   assignment.
//!
//! The handle also centralises bounded readiness polling, graceful/forced
//! shutdown, startup-failure cleanup, and diagnostic redaction.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const FORCE_WAIT: Duration = Duration::from_secs(2);
const TREE_REAP_SWEEPS: usize = 100;
const DIAGNOSTIC_BYTES: usize = 16 * 1024;
const DIAGNOSTIC_LINES: usize = 128;
const DIAGNOSTIC_LINE_BYTES: usize = 2048;
const CHILD_LINE_BYTES: usize = 64 * 1024;

/// Result of a bounded readiness wait.
#[derive(Debug)]
pub(crate) enum Readiness {
    Ready,
    Exited(ExitStatus),
    TimedOut,
}

/// What happened while stopping a supervised service.
#[derive(Debug)]
pub(crate) struct ShutdownReport {
    pub(crate) status: Option<ExitStatus>,
    pub(crate) forced: bool,
}

/// Emit one bounded lifecycle diagnostic when graceful teardown needed force or
/// supervision itself failed. Normal graceful exits stay quiet.
pub(crate) fn log_shutdown(label: &str, result: io::Result<ShutdownReport>) {
    let message = match result {
        Ok(report) if report.forced => {
            let status = report
                .status
                .map(|status| status.to_string())
                .unwrap_or_else(|| "status unavailable".to_string());
            Some(format!(
                "lsdj-app: {label}: forced process-tree shutdown ({status})"
            ))
        }
        Err(error) => Some(format!(
            "lsdj-app: {label}: process-tree shutdown failed: {error}"
        )),
        Ok(_) => None,
    };
    if let Some(message) = message {
        eprintln!("{}", sanitize_diagnostic(&message));
    }
}

/// A child plus the platform primitive that owns all of its descendants.
pub(crate) struct SupervisedChild {
    child: Child,
    exit_status: Option<ExitStatus>,
    tree_cleaned: bool,
    #[cfg(unix)]
    parent_guard: Option<std::os::fd::OwnedFd>,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

/// Spawn `command` under process-tree supervision.
///
/// The input remains a [`Command`], rather than a shell string, so executable,
/// arguments, environment, CWD, and stdio stay structured and paths containing
/// spaces or Unicode pass to the OS unchanged.
pub(crate) fn spawn_grouped(command: &mut Command) -> io::Result<SupervisedChild> {
    scrub_child_environment(command);
    #[cfg(unix)]
    {
        spawn_unix(command)
    }

    #[cfg(windows)]
    {
        spawn_windows(command)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let child = command.spawn()?;
        Ok(SupervisedChild {
            child,
            exit_status: None,
            tree_cleaned: false,
        })
    }
}

/// Remove credentials and interpreter/package-manager injection knobs from all
/// supervised children. Application-owned values such as `UV_CACHE_DIR` and
/// `HF_HUB_OFFLINE` remain explicit on the command; ambient user/session values
/// may not alter executable discovery, dependency resolution, or diagnostics.
fn scrub_child_environment(command: &mut Command) {
    const KEYS: &[&str] = &[
        "HF_TOKEN",
        "HUGGING_FACE_HUB_TOKEN",
        "PYTHONPATH",
        "PYTHONHOME",
        "PYTHONSTARTUP",
        "PYTHONINSPECT",
        "PYTHONBREAKPOINT",
        "PYTHONUSERBASE",
        "PYTHONWARNINGS",
        "PYTHONPYCACHEPREFIX",
        "UV_CONFIG_FILE",
        "UV_PROJECT",
        "UV_WORKING_DIR",
        "UV_CONSTRAINT",
        "UV_BUILD_CONSTRAINT",
        "UV_OVERRIDE",
        "UV_EXCLUDE",
        "UV_FIND_LINKS",
        "UV_INDEX",
        "UV_INDEX_URL",
        "UV_EXTRA_INDEX_URL",
        "UV_DEFAULT_INDEX",
        "UV_INDEX_STRATEGY",
        "UV_INSECURE_HOST",
        "UV_KEYRING_PROVIDER",
        "UV_NO_VERIFY_HASHES",
        "UV_SYSTEM_PYTHON",
        "UV_PYTHON",
        "UV_PYTHON_DOWNLOADS",
        "PIP_CONFIG_FILE",
        "PIP_CONSTRAINT",
        "PIP_REQUIRE_VIRTUALENV",
        "PIP_INDEX_URL",
        "PIP_EXTRA_INDEX_URL",
        "PIP_TRUSTED_HOST",
        "PIP_FIND_LINKS",
    ];
    for key in KEYS {
        command.env_remove(key);
    }
}

impl SupervisedChild {
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Poll the leader. If it exited, also remove any descendants it left
    /// behind before returning the status.
    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let status = self.child.try_wait()?;
        if let Some(status) = status {
            self.exit_status = Some(status);
            self.cleanup_remaining_tree();
        }
        Ok(status)
    }

    /// Wait for the leader and clean up descendants left by a wrapper that
    /// returned before its worker.
    pub(crate) fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.exit_status {
            return Ok(status);
        }
        let status = self.child.wait()?;
        self.exit_status = Some(status);
        self.cleanup_remaining_tree();
        Ok(status)
    }

    /// Poll a service-specific readiness probe until it succeeds, the child
    /// exits, or the timeout expires. Readiness transport stays with the owner
    /// (TCP for generation/sidecars); lifecycle and timeout semantics live here.
    pub(crate) fn wait_for_readiness(
        &mut self,
        timeout: Duration,
        mut probe: impl FnMut() -> io::Result<bool>,
    ) -> io::Result<Readiness> {
        let deadline = Instant::now() + timeout;
        loop {
            if probe()? {
                return Ok(Readiness::Ready);
            }
            if let Some(status) = self.try_wait()? {
                return Ok(Readiness::Exited(status));
            }
            if Instant::now() >= deadline {
                return Ok(Readiness::TimedOut);
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    /// Give a service a bounded grace period, then force its entire tree down.
    ///
    /// Unix sends `SIGTERM` to the group first. Windows has no generally safe
    /// graceful signal for GUI/background processes, so its grace period lets a
    /// protocol shutdown (socket EOF, for example) complete before the Job is
    /// terminated.
    pub(crate) fn shutdown(&mut self, grace: Duration) -> io::Result<ShutdownReport> {
        self.shutdown_with_hook(grace, || {})
    }

    fn shutdown_with_hook(
        &mut self,
        grace: Duration,
        after_graceful_signal: impl FnOnce(),
    ) -> io::Result<ShutdownReport> {
        if let Some(status) = self.try_wait()? {
            return Ok(ShutdownReport {
                status: Some(status),
                forced: false,
            });
        }

        #[cfg(unix)]
        self.signal_unix_group(libc::SIGTERM);
        after_graceful_signal();

        if let Some(status) = self.wait_direct_timeout(grace)? {
            self.cleanup_remaining_tree();
            return Ok(ShutdownReport {
                status: Some(status),
                forced: false,
            });
        }

        let status = self.force_kill()?;
        Ok(ShutdownReport {
            status,
            forced: true,
        })
    }

    /// Immediately terminate the complete tree and wait a bounded amount of
    /// time for the leader to become reapable.
    pub(crate) fn force_kill(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            self.cleanup_remaining_tree();
            return Ok(Some(status));
        }
        self.force_tree();
        let status = self.wait_direct_timeout(FORCE_WAIT)?;
        self.cleanup_remaining_tree();
        if self.exit_status.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "process leader did not exit after tree termination",
            ));
        }
        Ok(status)
    }

    fn wait_direct_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.exit_status = Some(status);
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(
                POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    fn force_tree(&mut self) {
        #[cfg(unix)]
        self.signal_unix_group(libc::SIGKILL);

        #[cfg(windows)]
        if let Some(job) = self.job.as_ref() {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::JobObjects::TerminateJobObject;
            // SAFETY: the handle is a live Job Object owned by `self`.
            unsafe {
                TerminateJobObject(job.as_raw_handle() as _, 1);
            }
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = self.child.kill();
        }
    }

    fn cleanup_remaining_tree(&mut self) {
        if self.tree_cleaned {
            return;
        }
        self.force_tree();

        #[cfg(unix)]
        {
            // Closing the pipe also wakes the abnormal-exit watchdog if it won a
            // race with the group signal.
            self.parent_guard.take();
            let group = -(self.id() as libc::pid_t);
            for _ in 0..TREE_REAP_SWEEPS {
                // SAFETY: signal 0 probes the supervised group without sending a
                // signal. The group id was fixed at spawn.
                if unsafe { libc::kill(group, 0) } == -1 {
                    break;
                }
                // SAFETY: target only the supervised process group.
                unsafe {
                    libc::kill(group, libc::SIGKILL);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        #[cfg(windows)]
        {
            // KILL_ON_JOB_CLOSE is the abnormal-exit guarantee; explicit job
            // termination above makes normal cleanup deterministic.
            self.job.take();
        }
        self.tree_cleaned = true;
    }

    #[cfg(unix)]
    fn signal_unix_group(&self, signal: libc::c_int) {
        let group = -(self.id() as libc::pid_t);
        // SAFETY: this pid is the process-group leader created in `spawn_unix`.
        unsafe {
            libc::kill(group, signal);
        }
    }
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        if self.exit_status.is_none() {
            let _ = self.force_kill();
        } else {
            self.cleanup_remaining_tree();
        }
    }
}

#[cfg(unix)]
fn spawn_unix(command: &mut Command) -> io::Result<SupervisedChild> {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;

    let mut pipe_fds = [0; 2];
    // SAFETY: valid storage for the two returned descriptors.
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful `pipe` call returned ownership of both fds.
    let read_guard = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
    let write_guard = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };

    for fd in pipe_fds {
        // SAFETY: both descriptors are live. CLOEXEC prevents either end leaking
        // into unrelated commands spawned by the host/service.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1
        {
            return Err(io::Error::last_os_error());
        }
    }

    let max_fd = unix_close_bound();
    let read_fd = pipe_fds[0];
    let write_fd = pipe_fds[1];
    // SAFETY: this closure uses only async-signal-safe syscalls between fork and
    // exec. The watchdog child never returns into Rust or allocates.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            let service_pid = libc::getpid();
            let watchdog = libc::fork();
            if watchdog == -1 {
                return Err(io::Error::last_os_error());
            }
            if watchdog == 0 {
                // The watchdog shares the service process group, so the host's
                // graceful group signal reaches it too. It must survive that
                // signal in order to observe a host crash during the grace
                // period; the later group SIGKILL remains unignorable.
                libc::signal(libc::SIGTERM, libc::SIG_IGN);
                libc::close(write_fd);
                // Close the command's stdio and Rust's private exec-error pipe so
                // this watcher cannot keep either alive. Its only input is the
                // dedicated host-lifetime pipe.
                let mut fd = 3;
                while fd < max_fd {
                    if fd != read_fd {
                        libc::close(fd);
                    }
                    fd += 1;
                }
                unix_watch_parent(read_fd, service_pid);
            }
            libc::close(read_fd);
            Ok(())
        });
    }

    let child = command.spawn()?;
    drop(read_guard);
    Ok(SupervisedChild {
        child,
        exit_status: None,
        tree_cleaned: false,
        parent_guard: Some(write_guard),
    })
}

#[cfg(unix)]
fn unix_close_bound() -> libc::c_int {
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: `limit` points to writable storage for `getrlimit`.
    let result = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) };
    if result == 0 {
        // SAFETY: initialized on success.
        let current = unsafe { limit.assume_init() }.rlim_cur;
        current.min(65_536) as libc::c_int
    } else {
        1024
    }
}

/// Watch the Rust host and the command leader without invoking any Rust runtime
/// after `fork`. This function never returns.
#[cfg(unix)]
unsafe fn unix_watch_parent(read_fd: libc::c_int, service_pid: libc::pid_t) -> ! {
    loop {
        let mut poll_fd = libc::pollfd {
            fd: read_fd,
            events: libc::POLLIN | libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        // SAFETY: `poll_fd` is valid for one element. A short timeout also lets
        // us notice the service leader's exit and clean its leftovers.
        let polled = unsafe { libc::poll(&mut poll_fd, 1, 100) };
        if polled > 0 && poll_fd.revents != 0 {
            let mut byte = 0u8;
            // SAFETY: valid one-byte destination; the host never writes, so EOF
            // is the expected event.
            if unsafe { libc::read(read_fd, (&mut byte as *mut u8).cast(), 1) } == 0 {
                // SAFETY: the watchdog shares the service's process group.
                unsafe { libc::kill(-service_pid, libc::SIGKILL) };
                unsafe { libc::_exit(0) };
            }
        }
        // Children are reparented as soon as their parent exits, even while the
        // leader is still a zombie waiting for the Rust host to reap it.
        if unsafe { libc::getppid() } != service_pid {
            unsafe { libc::kill(-service_pid, libc::SIGKILL) };
            unsafe { libc::_exit(0) };
        }
    }
}

#[cfg(windows)]
fn spawn_windows(command: &mut Command) -> io::Result<SupervisedChild> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;

    command.creation_flags(CREATE_SUSPENDED);
    let mut child = command.spawn()?;

    // SAFETY: null security/name pointers request an unnamed Job with defaults.
    let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw_job.is_null() {
        let error = io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    // SAFETY: ownership of the newly-created handle transfers to `OwnedHandle`.
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job as _) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: live Job handle and correctly-sized information structure.
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle() as _,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    // SAFETY: both handles are live and owned for the remainder of this scope.
    if unsafe { AssignProcessToJobObject(job.as_raw_handle() as _, child.as_raw_handle() as _) }
        == 0
    {
        let error = io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }

    if let Err(error) = resume_windows_process(child.id()) {
        // Closing a kill-on-close Job takes down the still-suspended child.
        drop(job);
        let _ = child.wait();
        return Err(error);
    }

    Ok(SupervisedChild {
        child,
        exit_status: None,
        tree_cleaned: false,
        job: Some(job),
    })
}

#[cfg(windows)]
fn resume_windows_process(process_id: u32) -> io::Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // A newly-created suspended process has exactly one thread. Enumerating it
    // is necessary because `std::process::Child` exposes the process handle but
    // not CreateProcessW's primary-thread handle.
    // SAFETY: system snapshot request with no process filter.
    let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw_snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ownership of the snapshot handle transfers here.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot as _) };
    let mut entry = THREADENTRY32::default();
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    // SAFETY: snapshot and entry pointers are valid.
    let mut has_entry = unsafe { Thread32First(snapshot.as_raw_handle() as _, &mut entry) } != 0;
    while has_entry {
        if entry.th32OwnerProcessID == process_id {
            // SAFETY: request the minimal right for the enumerated thread.
            let raw_thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !raw_thread.is_null() {
                // SAFETY: ownership of the opened thread handle transfers here.
                let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread as _) };
                // SAFETY: the process was created with CREATE_SUSPENDED, so its
                // primary thread has a positive suspend count.
                if unsafe { ResumeThread(thread.as_raw_handle() as _) } != u32::MAX {
                    return Ok(());
                }
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: same live snapshot and initialized entry.
        has_entry = unsafe { Thread32Next(snapshot.as_raw_handle() as _, &mut entry) } != 0;
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "cannot find suspended child thread",
    ))
}

/// A bounded, already-redacted tail suitable for UI-facing crash diagnostics.
#[derive(Debug, Default)]
pub(crate) struct DiagnosticTail {
    lines: VecDeque<String>,
    bytes: usize,
    omitted: usize,
}

impl DiagnosticTail {
    pub(crate) fn push(&mut self, line: &str) {
        let line = sanitize_diagnostic(line);
        let bytes = line.len();
        while !self.lines.is_empty()
            && (self.lines.len() >= DIAGNOSTIC_LINES || self.bytes + bytes > DIAGNOSTIC_BYTES)
        {
            if let Some(removed) = self.lines.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.len());
                self.omitted += 1;
            }
        }
        self.bytes += bytes;
        self.lines.push_back(line);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub(crate) fn render(&self) -> String {
        let mut rendered = self.lines.iter().cloned().collect::<Vec<_>>().join("\n");
        if self.omitted > 0 {
            let prefix = format!("[{} earlier diagnostic lines omitted]\n", self.omitted);
            rendered.insert_str(0, &prefix);
        }
        rendered
    }
}

/// Read newline-delimited child output without ever allocating in proportion to
/// a child-controlled line. Once the cap is reached, the remainder of that line
/// is discarded through the next newline; the bounded prefix is still delivered
/// so callers can retain useful diagnostics or attempt structured parsing.
pub(crate) fn read_bounded_lines(
    mut reader: impl Read,
    mut on_line: impl FnMut(&str),
) -> io::Result<()> {
    let mut chunk = [0u8; 8192];
    let mut line = Vec::with_capacity(CHILD_LINE_BYTES.min(8192));
    let mut discarding = false;

    loop {
        let read = match reader.read(&mut chunk) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if read == 0 {
            break;
        }
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let decoded = String::from_utf8_lossy(&line);
                on_line(&decoded);
                line.clear();
                discarding = false;
            } else if !discarding {
                if line.len() < CHILD_LINE_BYTES {
                    line.push(byte);
                } else {
                    discarding = true;
                }
            }
        }
    }

    if !line.is_empty() || discarding {
        let decoded = String::from_utf8_lossy(&line);
        on_line(&decoded);
    }
    Ok(())
}

/// Redact provider credentials and bound a single child-origin diagnostic for
/// logs, error returns, and UI emission. Keeping this as the only diagnostic
/// boundary helper prevents one structured-output path from bypassing the same
/// rules used by stderr tails.
pub(crate) fn sanitize_diagnostic(line: &str) -> String {
    let mut line = truncate_utf8(line, DIAGNOSTIC_LINE_BYTES).to_string();
    redact_url_credentials(&mut line);

    // Longest/specific provider spellings go first. Identifier-boundary checks
    // keep the generic `token`/`secret` forms from matching their suffixes.
    const KEYS: &[(&str, bool)] = &[
        ("hugging_face_hub_token", false),
        ("hugging-face-hub-token", false),
        ("authorization", true),
        ("refresh_token", false),
        ("refresh-token", false),
        ("access_token", false),
        ("access-token", false),
        ("client_secret", false),
        ("client-secret", false),
        ("clientsecret", false),
        ("accesskey", false),
        ("api_key", false),
        ("api-key", false),
        ("apikey", false),
        ("hf_token", false),
        ("hf-token", false),
        ("password", false),
        ("passwd", false),
        ("credential", false),
        ("token", false),
        ("secret", false),
    ];
    for &(key, consume_remainder) in KEYS {
        redact_key_values(&mut line, key, consume_remainder);
    }
    redact_bearer_values(&mut line);
    truncate_utf8(&line, DIAGNOSTIC_LINE_BYTES).to_string()
}

fn identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn redact_key_values(line: &mut String, key: &str, consume_remainder: bool) {
    let mut search_from = 0;
    loop {
        let lower = line[search_from..].to_ascii_lowercase();
        let Some(relative) = lower.find(key) else {
            break;
        };
        let key_start = search_from + relative;
        let key_end = key_start + key.len();
        let bytes = line.as_bytes();
        if key_start > 0 && identifier_byte(bytes[key_start - 1]) {
            search_from = key_end;
            continue;
        }
        if key_end < bytes.len() && identifier_byte(bytes[key_end]) {
            search_from = key_end;
            continue;
        }

        let mut cursor = key_end;
        if matches!(line.as_bytes().get(cursor), Some(b'\'' | b'"')) {
            cursor += 1;
        }
        while matches!(line.as_bytes().get(cursor), Some(b' ' | b'\t')) {
            cursor += 1;
        }
        if !matches!(line.as_bytes().get(cursor), Some(b'=' | b':')) {
            search_from = key_end;
            continue;
        }
        cursor += 1;
        while matches!(line.as_bytes().get(cursor), Some(b' ' | b'\t')) {
            cursor += 1;
        }

        let quote = line
            .as_bytes()
            .get(cursor)
            .copied()
            .filter(|b| matches!(b, b'\'' | b'"'));
        if quote.is_some() {
            cursor += 1;
        }
        let value_start = cursor;
        let value_end = if let Some(quote) = quote {
            line.as_bytes()[value_start..]
                .iter()
                .position(|&byte| byte == quote)
                .map_or(line.len(), |offset| value_start + offset)
        } else if consume_remainder {
            line.len()
        } else {
            line.as_bytes()[value_start..]
                .iter()
                .position(|byte| matches!(byte, b' ' | b'\t' | b',' | b';' | b'&' | b'}' | b']'))
                .map_or(line.len(), |offset| value_start + offset)
        };
        if value_end > value_start {
            line.replace_range(value_start..value_end, "[REDACTED]");
            search_from = value_start + "[REDACTED]".len();
        } else {
            search_from = key_end;
        }
    }
}

fn redact_bearer_values(line: &mut String) {
    let mut search_from = 0;
    loop {
        let lower = line[search_from..].to_ascii_lowercase();
        let Some(relative) = lower.find("bearer") else {
            break;
        };
        let start = search_from + relative;
        let end = start + "bearer".len();
        let bytes = line.as_bytes();
        if (start > 0 && identifier_byte(bytes[start - 1]))
            || (end < bytes.len() && identifier_byte(bytes[end]))
        {
            search_from = end;
            continue;
        }
        let mut value_start = end;
        while matches!(line.as_bytes().get(value_start), Some(b' ' | b'\t')) {
            value_start += 1;
        }
        let value_end = line.as_bytes()[value_start..]
            .iter()
            .position(|byte| {
                matches!(
                    byte,
                    b' ' | b'\t' | b',' | b';' | b'"' | b'\'' | b'}' | b']'
                )
            })
            .map_or(line.len(), |offset| value_start + offset);
        if value_end > value_start {
            line.replace_range(value_start..value_end, "[REDACTED]");
            search_from = value_start + "[REDACTED]".len();
        } else {
            search_from = end;
        }
    }
}

fn redact_url_credentials(line: &mut String) {
    let mut search_from = 0;
    while let Some(scheme_offset) = line[search_from..].find("://") {
        let credentials_start = search_from + scheme_offset + 3;
        let remainder = &line[credentials_start..];
        let authority_end = remainder.find(['/', ' ', '\t']).unwrap_or(remainder.len());
        let Some(at) = remainder[..authority_end].rfind('@') else {
            search_from = credentials_start + authority_end;
            continue;
        };
        line.replace_range(credentials_start..credentials_start + at, "[REDACTED]");
        search_from = credentials_start + "[REDACTED]@".len();
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    const HELPER_ROLE: &str = "LSDJ_PROCESS_HELPER_ROLE";
    const HELPER_PID_FILE: &str = "LSDJ_PROCESS_HELPER_PID_FILE";

    fn helper_command(role: &str, pid_file: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .args([
                "--ignored",
                "--exact",
                "child_process::tests::process_helper",
                "--nocapture",
            ])
            .env(HELPER_ROLE, role)
            .env(HELPER_PID_FILE, pid_file);
        command
    }

    fn test_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lsdj-supervisor-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn wait_for_pids(path: &Path) -> (u32, u32) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(contents) = std::fs::read_to_string(path) {
                let pids = contents
                    .split_whitespace()
                    .filter_map(|value| value.parse::<u32>().ok())
                    .collect::<Vec<_>>();
                if pids.len() == 2 {
                    return (pids[0], pids[1]);
                }
            }
            assert!(
                Instant::now() < deadline,
                "helper did not report its process tree"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_until_gone(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if !process_is_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[cfg(unix)]
    fn process_is_alive(pid: u32) -> bool {
        // SAFETY: signal 0 probes without signalling.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    #[cfg(windows)]
    fn process_is_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
        // SAFETY: read-only liveness handle for a test-owned pid.
        let process = unsafe {
            OpenProcess(
                SYNCHRONIZE_ACCESS | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if process.is_null() {
            return false;
        }
        // SAFETY: live process handle and zero timeout.
        let result = unsafe { WaitForSingleObject(process, 0) };
        // SAFETY: close the handle opened above.
        unsafe { CloseHandle(process) };
        result == WAIT_TIMEOUT
    }

    #[cfg(not(any(unix, windows)))]
    fn process_is_alive(_pid: u32) -> bool {
        false
    }

    /// Process-tree stand-in built from the test executable itself, so lifecycle
    /// coverage is portable and needs no Python, shell, or downloaded fixture.
    #[test]
    #[ignore]
    #[allow(clippy::zombie_processes)] // the supervisor tests intentionally own/reap the tree
    fn process_helper() {
        let role = std::env::var(HELPER_ROLE).expect("helper role");
        let pid_file = PathBuf::from(std::env::var_os(HELPER_PID_FILE).expect("pid file"));
        match role.as_str() {
            "host" => {
                let mut command = helper_command("child", &pid_file);
                let _child = spawn_grouped(&mut command).expect("host spawns supervised child");
                let _ = wait_for_pids(&pid_file);
                // Deliberately bypass destructors, matching a panic/abort-style
                // host exit. OS/job/watchdog lifetime must still remove the tree.
                std::process::exit(0);
            }
            "host-shutdown" => {
                let mut command = helper_command("child", &pid_file);
                let mut child = spawn_grouped(&mut command).expect("host spawns supervised child");
                let _ = wait_for_pids(&pid_file);
                let marker = pid_file.with_extension("shutdown");
                let _ = child.shutdown_with_hook(Duration::from_secs(30), || {
                    std::fs::write(marker, b"signalled").expect("write shutdown marker");
                });
            }
            "child" | "startup-failure" => {
                #[cfg(unix)]
                // SAFETY: make graceful shutdown exhaust its deadline so the
                // explicit teardown test covers the forced path.
                unsafe {
                    libc::signal(libc::SIGTERM, libc::SIG_IGN);
                }
                let grandchild = helper_command("grandchild", &pid_file)
                    .spawn()
                    .expect("spawn grandchild");
                std::fs::write(
                    &pid_file,
                    format!("{} {}", std::process::id(), grandchild.id()),
                )
                .expect("write pid file");
                if role == "startup-failure" {
                    std::process::exit(23);
                }
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
            "grandchild" => {
                #[cfg(unix)]
                // SAFETY: see the child role above.
                unsafe {
                    libc::signal(libc::SIGTERM, libc::SIG_IGN);
                }
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
            other => panic!("unknown helper role {other}"),
        }
    }

    #[test]
    fn explicit_shutdown_removes_child_and_grandchild() {
        let dir = test_dir("shutdown");
        let pid_file = dir.join("pids");
        let mut child = spawn_grouped(&mut helper_command("child", &pid_file)).unwrap();
        let (child_pid, grandchild_pid) = wait_for_pids(&pid_file);

        let report = child.shutdown(Duration::from_millis(100)).unwrap();
        assert!(report.forced, "helpers ignore graceful shutdown");
        assert!(report.status.is_some(), "leader should be reaped");
        assert!(
            wait_until_gone(child_pid),
            "child survived explicit shutdown"
        );
        assert!(
            wait_until_gone(grandchild_pid),
            "grandchild survived explicit shutdown"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_failure_removes_grandchild() {
        let dir = test_dir("startup-failure");
        let pid_file = dir.join("pids");
        let mut child = spawn_grouped(&mut helper_command("startup-failure", &pid_file)).unwrap();
        let (_child_pid, grandchild_pid) = wait_for_pids(&pid_file);

        let status = child.wait().unwrap();
        assert!(!status.success(), "startup stand-in must fail");
        assert!(
            wait_until_gone(grandchild_pid),
            "startup failure left its grandchild running"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn terminal_status_is_stable_across_repeated_poll_shutdown_and_wait() {
        let dir = test_dir("cached-status");
        let pid_file = dir.join("pids");
        let mut child = spawn_grouped(&mut helper_command("startup-failure", &pid_file)).unwrap();
        let _ = wait_for_pids(&pid_file);

        let deadline = Instant::now() + Duration::from_secs(10);
        let first = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "helper did not exit");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(!first.success());
        assert_eq!(child.try_wait().unwrap(), Some(first));

        // Once terminal, shutdown must return the cached status without sending
        // a signal to the old pid/process-group, and wait must remain idempotent.
        let report = child.shutdown(Duration::ZERO).unwrap();
        assert_eq!(report.status, Some(first));
        assert!(!report.forced);
        assert_eq!(child.wait().unwrap(), first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn abnormal_host_exit_removes_child_and_grandchild() {
        let dir = test_dir("abnormal-host");
        let pid_file = dir.join("pids");
        let status = helper_command("host", &pid_file).status().unwrap();
        assert!(status.success(), "host helper failed: {status}");
        let (child_pid, grandchild_pid) = wait_for_pids(&pid_file);

        assert!(
            wait_until_gone(child_pid),
            "child survived abnormal host exit"
        );
        assert!(
            wait_until_gone(grandchild_pid),
            "grandchild survived abnormal host exit"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn host_exit_during_grace_still_removes_sigterm_resistant_tree() {
        let dir = test_dir("host-exit-during-grace");
        let pid_file = dir.join("pids");
        let marker = pid_file.with_extension("shutdown");
        let mut host = helper_command("host-shutdown", &pid_file)
            .spawn()
            .expect("spawn host helper");
        let (child_pid, grandchild_pid) = wait_for_pids(&pid_file);

        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "host never entered shutdown grace"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // `Child::kill` is SIGKILL on Unix: destructors cannot close the parent
        // guard. The watchdog must survive the preceding group SIGTERM, observe
        // the kernel closing that guard, and kill the resistant descendants.
        host.kill().expect("kill host during grace");
        let _ = host.wait();

        assert!(
            wait_until_gone(child_pid),
            "child survived host death during grace"
        );
        assert!(
            wait_until_gone(grandchild_pid),
            "grandchild survived host death during grace"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn readiness_reports_ready_exit_and_timeout() {
        let dir = test_dir("readiness");
        let pid_file = dir.join("pids");
        let mut running = spawn_grouped(&mut helper_command("child", &pid_file)).unwrap();
        let _ = wait_for_pids(&pid_file);
        let mut polls = 0;
        let ready = running
            .wait_for_readiness(Duration::from_secs(1), || {
                polls += 1;
                Ok(polls == 2)
            })
            .unwrap();
        assert!(matches!(ready, Readiness::Ready));
        let timed_out = running
            .wait_for_readiness(Duration::from_millis(30), || Ok(false))
            .unwrap();
        assert!(matches!(timed_out, Readiness::TimedOut));
        let _ = running.force_kill();

        let failure_file = dir.join("failure-pids");
        let mut failed =
            spawn_grouped(&mut helper_command("startup-failure", &failure_file)).unwrap();
        let _ = wait_for_pids(&failure_file);
        let exited = failed
            .wait_for_readiness(Duration::from_secs(5), || Ok(false))
            .unwrap();
        assert!(matches!(exited, Readiness::Exited(status) if !status.success()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn diagnostics_are_bounded_and_redacted() {
        let mut diagnostics = DiagnosticTail::default();
        diagnostics.push(
            "download https://url-user:url-pass@example.com HF_TOKEN=hf-secret \
             HUGGING_FACE_HUB_TOKEN: hub-secret client_secret=oauth-secret \
             access-token=access-secret api_key=api-secret \
             Authorization: Bearer auth-secret",
        );
        diagnostics.push(
            r#"{"HF_TOKEN":"json-hf-secret","client_secret":"json-oauth-secret",\
                 "Authorization":"Bearer json-auth-secret"}"#,
        );
        let redacted = diagnostics.render();
        for secret in [
            "url-user",
            "url-pass",
            "hf-secret",
            "hub-secret",
            "oauth-secret",
            "access-secret",
            "api-secret",
            "auth-secret",
            "json-hf-secret",
            "json-oauth-secret",
            "json-auth-secret",
        ] {
            assert!(!redacted.contains(secret), "leaked {secret}: {redacted}");
        }
        assert!(redacted.contains("[REDACTED]"));
        for index in 0..1000 {
            diagnostics.push(&format!("line-{index} {}", "x".repeat(200)));
        }
        let rendered = diagnostics.render();
        assert!(rendered.contains("omitted"));
        assert!(rendered.len() <= DIAGNOSTIC_BYTES + 100);
    }

    #[test]
    fn bounded_line_reader_discards_multi_megabyte_line_without_losing_next_line() {
        let mut input = vec![b'x'; 4 * 1024 * 1024];
        input.extend_from_slice(b"\nnext\r\nfinal-without-newline");
        let mut lines = Vec::new();
        read_bounded_lines(std::io::Cursor::new(input), |line| {
            lines.push(line.to_string())
        })
        .unwrap();

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].len(), CHILD_LINE_BYTES);
        assert!(lines[0].bytes().all(|byte| byte == b'x'));
        assert_eq!(lines[1], "next");
        assert_eq!(lines[2], "final-without-newline");
    }

    #[test]
    fn supervised_children_scrub_credentials_and_injection_environment() {
        let mut command = Command::new("unused");
        for key in [
            "HF_TOKEN",
            "HUGGING_FACE_HUB_TOKEN",
            "PYTHONPATH",
            "PYTHONHOME",
            "UV_CONFIG_FILE",
            "UV_OVERRIDE",
            "UV_INDEX_URL",
            "PIP_CONFIG_FILE",
            "PIP_INDEX_URL",
        ] {
            command.env(key, "attacker-controlled");
        }
        command.env("UV_CACHE_DIR", "app-controlled-cache");
        command.env("HF_HUB_OFFLINE", "1");

        scrub_child_environment(&mut command);
        let environment = command.get_envs().collect::<Vec<_>>();
        for key in [
            "HF_TOKEN",
            "HUGGING_FACE_HUB_TOKEN",
            "PYTHONPATH",
            "PYTHONHOME",
            "UV_CONFIG_FILE",
            "UV_OVERRIDE",
            "UV_INDEX_URL",
            "PIP_CONFIG_FILE",
            "PIP_INDEX_URL",
        ] {
            assert!(
                environment
                    .iter()
                    .any(|(name, value)| *name == key && value.is_none()),
                "{key} was not explicitly removed"
            );
        }
        assert!(environment.iter().any(|(name, value)| {
            *name == "UV_CACHE_DIR" && value == &Some(std::ffi::OsStr::new("app-controlled-cache"))
        }));
        assert!(environment.iter().any(|(name, value)| {
            *name == "HF_HUB_OFFLINE" && value == &Some(std::ffi::OsStr::new("1"))
        }));
    }
}
