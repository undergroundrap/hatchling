//! Supervising a helper process on Linux: a pidfd-pinned [`Child`]
//! with bounded waits and kill-on-drop.

use std::time::Duration;

use hatchling_sys::{Child, ChildPid};

use crate::{Error, Exit, PlatformError, SuperviseError, Supervised};

/// A helper process under pidfd supervision. Dropping it kills and
/// reaps the helper.
#[derive(Debug)]
pub struct LinuxProcess {
    child: Child,
}

impl Supervised for LinuxProcess {
    fn try_wait(&self) -> Result<Option<Exit>, Error> {
        self.child
            .try_wait()
            .map(|status| status.map(exit))
            .map_err(|source| Error::Wait(PlatformError::new(source)))
    }

    fn wait_deadline(&self, timeout: Duration) -> Result<Exit, Error> {
        self.child
            .wait_deadline(timeout)
            .map(exit)
            .map_err(|source| Error::Wait(PlatformError::new(source)))
    }

    fn terminate(&self) -> Result<(), Error> {
        self.child
            .terminate()
            .map_err(|source| Error::Wait(PlatformError::new(source)))
    }
}

/// How a process ended, from its raw `waitpid` status.
const fn exit(status: i32) -> Exit {
    Exit {
        code: hatchling_sys::child_exit_code(status),
        status,
    }
}

/// Wraps the just-spawned helper in pidfd supervision. If that fails the
/// helper is killed and reaped at once: it is never left unsupervised.
///
/// # Errors
///
/// Returns [`SuperviseError::InvalidPid`] for a pid supervision cannot
/// hold, or [`SuperviseError::Refused`] if no pidfd can be opened on it.
pub(super) fn supervise(
    mut std_child: std::process::Child,
) -> Result<LinuxProcess, SuperviseError> {
    let pid = i32::try_from(std_child.id())
        .ok()
        .and_then(ChildPid::new)
        .ok_or(SuperviseError::InvalidPid);
    match pid.and_then(|pid| {
        Child::new(pid).map_err(|source| SuperviseError::Refused(PlatformError::new(source)))
    }) {
        Ok(child) => Ok(LinuxProcess { child }),
        Err(err) => {
            // Just spawned and unreaped: the pid cannot have been reused.
            let _ = std_child.kill();
            let _ = std_child.wait();
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    fn spawn(script: &str) -> std::process::Child {
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Whether the process is gone: `/proc/<pid>` vanishes only once it
    /// is reaped.
    fn gone(pid: u32) -> bool {
        let proc = format!("/proc/{pid}");
        std::fs::metadata(proc).is_err()
    }

    #[test]
    fn a_supervised_helper_reports_its_exit_code() {
        let mut child = spawn("exit 7");
        let process = supervise(child).unwrap();
        let exit = process.wait_deadline(Duration::from_secs(10)).unwrap();
        assert_eq!(exit.code, 7);
        assert_eq!(hatchling_sys::child_exit_code(exit.status), 7);
    }

    #[test]
    fn a_running_helper_has_no_exit_until_it_is_ended() {
        let mut child = spawn("sleep 30");
        let pid = child.id();
        let process = supervise(child).unwrap();
        assert_eq!(process.try_wait().unwrap(), None);
        process.terminate().unwrap();
        let exit = process.wait_deadline(Duration::from_secs(10)).unwrap();
        assert_eq!(exit.code, 128 + 9, "killed by SIGKILL");
        assert!(gone(pid), "the helper is reaped");
        // Once reaped there is nothing left to report, and a second
        // blocking wait is an error, never a timeout.
        assert_eq!(process.try_wait().unwrap(), None);
        assert!(matches!(
            process.wait_deadline(Duration::from_secs(1)),
            Err(Error::Wait(_))
        ));
    }

    #[test]
    fn an_ended_helper_reports_through_try_wait() {
        let mut child = spawn("exit 3");
        let process = supervise(child).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let exit = loop {
            if let Some(exit) = process.try_wait().unwrap() {
                break exit;
            }
            assert!(std::time::Instant::now() < deadline, "never ended");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            exit,
            Exit {
                code: 3,
                status: exit.status
            }
        );
    }

    #[test]
    fn a_wait_past_its_deadline_kills_the_helper() {
        let mut child = spawn("sleep 30");
        let pid = child.id();
        let process = supervise(child).unwrap();
        assert!(matches!(
            process.wait_deadline(Duration::from_millis(200)),
            Err(Error::Wait(_))
        ));
        assert!(gone(pid));
    }

    #[test]
    fn a_dropped_helper_is_killed_and_reaped() {
        let mut child = spawn("sleep 30");
        let pid = child.id();
        drop(supervise(child).unwrap());
        assert!(gone(pid));
    }

    #[test]
    fn a_reaped_helper_cannot_be_supervised_and_is_not_left_running() {
        let mut child = spawn("exit 0");
        child.wait().unwrap();
        // Its pid is gone: no pidfd can be opened on it.
        let refused = supervise(child);
        assert!(
            matches!(refused, Err(SuperviseError::Refused(_))),
            "got {refused:?}"
        );
    }

    #[test]
    fn exit_translates_the_raw_status() {
        // A raw status of exit code 5 (5 << 8), and of SIGTERM (15).
        assert_eq!(
            exit(5 << 8),
            Exit {
                code: 5,
                status: 5 << 8
            }
        );
        assert_eq!(
            exit(15),
            Exit {
                code: 128 + 15,
                status: 15
            }
        );
    }
}
