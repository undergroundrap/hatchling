//! File-descriptor hygiene across the exec into a cell.
//!
//! CLOEXEC on every fd the broker opens is the first line; this is the
//! structural one: nothing above stderr crosses the exec, whatever
//! anyone forgot to mark.

use std::os::fd::{AsRawFd, BorrowedFd};

use crate::Error;

/// Marks every fd above stderr close-on-exec: after the next `execve`
/// only fds 0, 1 and 2 exist.
///
/// This is `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)`. The fds stay open
/// until the exec itself, so a setup-failure report pipe still works if
/// the exec fails, and they are gone in the new image either way. Stdio is
/// untouched: the range starts at 3.
///
/// # Errors
///
/// Returns [`Error::Process`] if the syscall fails (a kernel older than
/// 5.11 has no `CLOSE_RANGE_CLOEXEC`). The caller must fail closed: never
/// exec with inherited fds.
pub fn cloexec_fds_above_stderr() -> Result<(), Error> {
    let first: libc::c_uint = 3;
    // SAFETY: with CLOSE_RANGE_CLOEXEC, close_range closes nothing; it only
    // sets FD_CLOEXEC on the range, which invalidates no descriptor this
    // process owns. The arguments are plain integers.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            first,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if ret == 0 {
        Ok(())
    } else {
        Err(Error::Process {
            operation: "close_range(3.., CLOSE_RANGE_CLOEXEC)",
            source: std::io::Error::last_os_error(),
        })
    }
}

/// Lets `fd` cross the next exec: clears its `FD_CLOEXEC` flag, and
/// nothing else.
///
/// For the one descriptor a cell's program is given on purpose, after
/// [`cloexec_fds_above_stderr`] marked every other one.
///
/// # Errors
///
/// Returns [`Error::Process`] if the flag can't be cleared.
pub fn keep_across_exec(fd: BorrowedFd<'_>) -> Result<(), Error> {
    rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty()).map_err(|source| Error::Process {
        operation: "fcntl(F_SETFD, 0)",
        source: std::io::Error::from_raw_os_error(source.raw_os_error()),
    })
}

/// Closes every fd above stderr except `keep`: afterwards only fds 0, 1
/// and 2 and the kept ones are open in this process.
///
/// For a forked child that never execs (a cell's reaper), which
/// close-on-exec never cleans up: otherwise it would hold every
/// descriptor its parent had open at the fork, for as long as it lives.
/// The fds between the kept ones are closed with `close_range`, one call
/// per gap; `keep` may be in any order and repeat itself.
///
/// The closed fds may still be named by handles this process inherited:
/// the caller must neither use nor drop any of those afterwards. A forked
/// child that only waits and then leaves through
/// [`exit_child`](crate::exit_child) does neither.
///
/// # Errors
///
/// Returns [`Error::Process`] if a `close_range` fails (a kernel older
/// than 5.9 has none). The caller must fail closed.
pub fn close_fds_above_stderr_except(keep: &[BorrowedFd<'_>]) -> Result<(), Error> {
    let lowest_kept_from = |from: libc::c_uint| {
        keep.iter()
            .filter_map(|fd| libc::c_uint::try_from(fd.as_raw_fd()).ok())
            .filter(|&fd| fd >= from)
            .min()
    };
    let mut low: libc::c_uint = 3;
    // Each pass steps past one kept fd, so `keep.len()` passes pass them all.
    for _ in 0..keep.len() {
        let Some(fd) = lowest_kept_from(low) else {
            break;
        };
        if fd > low {
            close_range(low, fd - 1)?;
        }
        low = fd.saturating_add(1);
    }
    close_range(low, libc::c_uint::MAX)
}

/// `close_range(first, last, 0)`: closes every fd from `first` to `last`.
fn close_range(first: libc::c_uint, last: libc::c_uint) -> Result<(), Error> {
    let flags: libc::c_uint = 0;
    // SAFETY: close_range closes descriptors by number and touches no
    // memory. That nothing uses the closed fds again is the caller's
    // contract (see `close_fds_above_stderr_except`).
    let ret = unsafe { libc::syscall(libc::SYS_close_range, first, last, flags) };
    if ret == 0 {
        Ok(())
    } else {
        Err(Error::Process {
            operation: "close_range",
            source: std::io::Error::last_os_error(),
        })
    }
}

/// The open fds below `limit`, ascending: what this process holds.
#[must_use]
pub fn open_fds(limit: i32) -> Vec<i32> {
    (0..limit).filter(|&fd| fd_flags(fd).is_some()).collect()
}

/// An fd's `FD_*` flags, or `None` when it is not open.
#[must_use]
pub fn fd_flags(fd: i32) -> Option<i32> {
    // SAFETY: F_GETFD only reads the descriptor's flags; on an fd that is
    // not open it fails with EBADF and touches nothing.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    (flags >= 0).then_some(flags)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::process::{ForkResult, child_exit_code, exit_child, fork};

    /// An inheritable duplicate of stdout: `dup` never sets `FD_CLOEXEC`.
    fn inheritable() -> i32 {
        // SAFETY: dup of an open fd; the new fd is owned (and closed) by
        // the caller.
        let fd = unsafe { libc::dup(1) };
        assert!(fd > 2, "dup failed or returned stdio");
        fd
    }

    fn close(fd: i32) {
        // SAFETY: closes an fd this test opened and owns.
        unsafe {
            libc::close(fd);
        }
    }

    #[test]
    fn a_kept_fd_loses_only_its_close_on_exec_flag() {
        // In a child: the flag changes must not leak into the test runner.
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let fd = inheritable();
                if cloexec_fds_above_stderr().is_err() {
                    exit_child(99);
                }
                if fd_flags(fd) != Some(libc::FD_CLOEXEC) {
                    exit_child(2);
                }
                // SAFETY: `fd` is this child's own open descriptor, and it
                // outlives the borrow.
                let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
                if keep_across_exec(borrowed).is_err() {
                    exit_child(3);
                }
                exit_child(if fd_flags(fd) == Some(0) { 0 } else { 4 });
            }
        };
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        let code = child_exit_code(status);
        if code == 99 {
            return;
        }
        assert_eq!(
            code, 0,
            "exit 2: not marked first; 3: the clear failed; 4: still close-on-exec"
        );
        // A descriptor that isn't open is an error, not a silent success.
        // SAFETY: the borrow is only handed to fcntl, which fails with EBADF.
        let closed = unsafe { BorrowedFd::borrow_raw(i32::MAX - 1) };
        assert!(matches!(
            keep_across_exec(closed),
            Err(Error::Process {
                operation: "fcntl(F_SETFD, 0)",
                ..
            })
        ));
    }

    #[test]
    fn fds_above_stderr_become_cloexec_and_stdio_is_untouched() {
        // In a child: the flags change must not leak into the test runner.
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let fd = inheritable();
                let stdio: Vec<Option<i32>> = (0..3).map(fd_flags).collect();
                if fd_flags(fd) != Some(0) {
                    exit_child(2);
                }
                if cloexec_fds_above_stderr().is_err() {
                    // Environmental: a kernel without CLOSE_RANGE_CLOEXEC.
                    exit_child(99);
                }
                if fd_flags(fd) != Some(libc::FD_CLOEXEC) {
                    exit_child(3);
                }
                let after: Vec<Option<i32>> = (0..3).map(fd_flags).collect();
                exit_child(if after == stdio { 0 } else { 4 });
            }
        };
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        let code = child_exit_code(status);
        if code == 99 {
            return;
        }
        assert_eq!(
            code, 0,
            "exit 2: the dup was not inheritable; 3: not marked close-on-exec; \
             4: stdio flags changed"
        );
    }

    /// Puts an inheritable fd on each of `fds`: known numbers to check.
    fn dup_onto(src: i32, fds: &[i32]) -> bool {
        fds.iter().all(|&fd| {
            // SAFETY: dup2 onto a number this forked child then owns.
            unsafe { libc::dup2(src, fd) == fd }
        })
    }

    /// The kept fds, borrowed for the call.
    fn kept(fds: &[i32]) -> Vec<BorrowedFd<'static>> {
        fds.iter()
            // SAFETY: each fd stays open for the borrow: the forked child
            // only closes the others, and exits right after checking.
            .map(|&fd| unsafe { BorrowedFd::borrow_raw(fd) })
            .collect()
    }

    #[test]
    fn only_stdio_and_the_kept_fds_survive_the_close() {
        // In a child: closing fds must not touch the test runner's.
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // 200 and 201 are kept and adjacent, 203 kept twice and out
                // of order; 202, 204, 205 and the source (below 200) are not.
                let src = inheritable();
                if !dup_onto(src, &[200, 201, 202, 203, 204, 205]) {
                    exit_child(2);
                }
                if close_fds_above_stderr_except(&kept(&[203, 201, 200, 203])).is_err() {
                    exit_child(3);
                }
                exit_child(if open_fds(400) == [0, 1, 2, 200, 201, 203] {
                    0
                } else {
                    4
                });
            }
        };
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        assert_eq!(
            child_exit_code(status),
            0,
            "exit 2: dup2 failed; 3: the close failed; 4: the wrong fds are open"
        );
    }

    #[test]
    fn close_range_closes_exactly_its_span_and_refuses_an_empty_one() {
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let src = inheritable();
                if !dup_onto(src, &[300, 301, 302]) {
                    exit_child(2);
                }
                let spanned = close_range(301, 301).is_ok()
                    && fd_flags(300).is_some()
                    && fd_flags(301).is_none()
                    && fd_flags(302).is_some();
                // first > last is not a range: the kernel refuses it.
                let refused = close_range(302, 301).is_err() && fd_flags(302).is_some();
                exit_child(match (spanned, refused) {
                    (true, true) => 0,
                    (false, _) => 3,
                    (true, false) => 4,
                });
            }
        };
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        assert_eq!(
            child_exit_code(status),
            0,
            "3: the span; 4: the empty range"
        );
    }

    #[test]
    fn open_fds_lists_exactly_what_is_open() {
        // In a single-threaded child: in the multi-threaded test runner a
        // closed fd number can be reused by another test at once.
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let fd = inheritable();
                let open = open_fds(fd + 1);
                let sorted = open.windows(2).all(|pair| pair.first() < pair.last());
                if !open.contains(&fd) || open.first() != Some(&0) || !sorted {
                    exit_child(2);
                }
                // An inheritable fd has flags 0, and still counts as open.
                if fd_flags(fd) != Some(0) {
                    exit_child(3);
                }
                close(fd);
                if open_fds(fd + 1).contains(&fd) || fd_flags(fd).is_some() {
                    exit_child(4);
                }
                exit_child(0);
            }
        };
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        assert_eq!(
            child_exit_code(status),
            0,
            "exit 2: the open fd was not listed (or the list is not sorted \
             from 0); 3: an inheritable fd did not read as flags 0; 4: a \
             closed fd still read as open"
        );
    }
}
