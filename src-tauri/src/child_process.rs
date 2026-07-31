//! Process-tree supervision for Python services launched through wrappers such as
//! `uv run`. Killing only the immediate [`Child`] would orphan the real Python
//! process, so every supervised command gets its own process group and teardown
//! signals the whole group before reaping its leader.

use std::io;
use std::process::{Child, Command};

/// Spawn `command` as the leader of a fresh process group. Descendants inherit
/// that group, which lets [`kill_group`] take down wrappers and their workers in
/// one operation.
pub(crate) fn spawn_grouped(command: &mut Command) -> io::Result<Child> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()
}

/// Kill a supervised child's entire process group and reap the immediate child.
///
/// The bounded re-sweep closes the same mid-fork race guarded by the model
/// installer's teardown: a descendant forked during the first signal remains in
/// the group and is caught by a subsequent pass.
pub(crate) fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let group = -(child.id() as libc::pid_t);
        // SAFETY: `child` was created by [`spawn_grouped`], so its live pid is
        // also the process-group id. A negative pid targets that group.
        unsafe {
            libc::kill(group, libc::SIGKILL);
        }
        let _ = child.wait();
        for _ in 0..100 {
            // SAFETY: signal 0 probes group liveness without signalling it.
            if unsafe { libc::kill(group, 0) } == -1 {
                break;
            }
            // SAFETY: as above, target only the supervised process group.
            unsafe {
                libc::kill(group, libc::SIGKILL);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}
