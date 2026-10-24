//! File-descriptor hygiene across the exec into a cell.
//!
//! CLOEXEC on every fd the broker opens is the first line; this is the
//! structural one: nothing above stderr crosses the exec, whatever
//! anyone forgot to mark.

use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};

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

/// Takes this process's stdin for itself: a close-on-exec copy of it,
/// with stdin itself pointed at `/dev/null`.
///
/// For a helper handed a channel as its stdin, which must not pass that
/// channel on to anything it starts: they inherit `/dev/null` instead.
///
/// # Errors
///
/// Returns [`Error::Process`] if stdin can't be copied, or `/dev/null`
/// opened or put in its place.
pub fn take_stdin() -> Result<OwnedFd, Error> {
    let failed = |operation: &'static str| {
        move |source: rustix::io::Errno| Error::Process {
            operation,
            source: std::io::Error::from_raw_os_error(source.raw_os_error()),
        }
    };
    let taken = rustix::io::fcntl_dupfd_cloexec(rustix::stdio::stdin(), 3)
        .map_err(failed("fcntl(0, F_DUPFD_CLOEXEC)"))?;
    let null = rustix::fs::open(
        "/dev/null",
        rustix::fs::OFlags::RDONLY.union(rustix::fs::OFlags::CLOEXEC),
        rustix::fs::Mode::empty(),
    )
    .map_err(failed("open /dev/null"))?;
    rustix::stdio::dup2_stdin(&null).map_err(failed("dup2(/dev/null, 0)"))?;
    Ok(taken)
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

/// Puts `fds` on the descriptor numbers `first`, `first + 1`, and so on,
/// in order, each open across the next exec.
///
/// For a program that is handed descriptors at fixed numbers: Chromium's
/// `--remote-debugging-pipe` reads fd 3 and writes fd 4. For a forked child about to exec. Whatever was open at a target number
/// is closed, so the caller must neither use nor drop a handle that named
/// one afterwards. A source may itself sit on another's target: each is
/// first copied above every target, close-on-exec, and only then are the
/// copies put in place, and closed.
///
/// # Errors
///
/// Returns [`Error::InvalidExecArg`] if `first` is a stdio number (those
/// aren't this function's to replace) or the targets run past the largest
/// descriptor number, and [`Error::Process`] if a copy or a placement
/// fails.
pub fn place_across_exec(fds: &[BorrowedFd<'_>], first: i32) -> Result<(), Error> {
    let invalid = |reason: &str| Error::InvalidExecArg {
        reason: reason.to_owned(),
    };
    if first <= 2 {
        return Err(invalid("descriptors are placed above stderr"));
    }
    let above = i32::try_from(fds.len())
        .ok()
        .and_then(|count| first.checked_add(count))
        .ok_or_else(|| invalid("too many descriptors to place"))?;
    let copies = fds
        .iter()
        .map(|fd| {
            rustix::io::fcntl_dupfd_cloexec(fd, above).map_err(|source| Error::Process {
                operation: "fcntl(F_DUPFD_CLOEXEC)",
                source: std::io::Error::from_raw_os_error(source.raw_os_error()),
            })
        })
        .collect::<Result<Vec<OwnedFd>, Error>>()?;
    for (target, copy) in (first..above).zip(&copies) {
        // SAFETY: dup2 onto a descriptor number. It closes whatever was
        // open there, which the caller's contract covers, and the new
        // descriptor is left to the exec; no memory is touched.
        let placed = unsafe { libc::dup2(copy.as_raw_fd(), target) };
        if placed != target {
            return Err(Error::Process {
                operation: "dup2",
                source: std::io::Error::last_os_error(),
            });
        }
    }
    Ok(())
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

/// A pipe: its read end and its write end, both close-on-exec.
///
/// # Errors
///
/// Returns [`Error::Process`] if the pipe can't be made.
pub fn pipe() -> Result<(OwnedFd, OwnedFd), Error> {
    rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(|source| Error::Process {
        operation: "pipe2(O_CLOEXEC)",
        source: std::io::Error::from_raw_os_error(source.raw_os_error()),
    })
}

/// Waits until one of `fds` is readable or hung up, however long that
/// takes, and says which: one flag for each, in order. A wait a signal
/// interrupts is waited again.
///
/// # Errors
///
/// Returns [`Error::Process`] if the wait fails.
pub fn wait_readable(fds: &[BorrowedFd<'_>]) -> Result<Vec<bool>, Error> {
    use rustix::event::{PollFd, PollFlags, poll};
    let mut polled: Vec<PollFd<'_>> = fds
        .iter()
        .map(|fd| PollFd::new(fd, PollFlags::IN))
        .collect();
    loop {
        match poll(&mut polled, -1) {
            Ok(_) => break,
            Err(rustix::io::Errno::INTR) => {}
            Err(source) => {
                return Err(Error::Process {
                    operation: "poll",
                    source: std::io::Error::from_raw_os_error(source.raw_os_error()),
                });
            }
        }
    }
    Ok(polled.iter().map(|fd| !fd.revents().is_empty()).collect())
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd as _, IntoRawFd as _};
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
    fn taking_stdin_leaves_null_in_its_place() {
        use std::io::{Read as _, Write as _};
        // In a child: stdin is the test runner's otherwise.
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let Ok((mut ours, theirs)) = std::os::unix::net::UnixStream::pair() else {
                    exit_child(2)
                };
                // SAFETY: both fds are open; dup2 replaces stdin with the
                // pair's end, which is what this test sets up.
                if unsafe { libc::dup2(theirs.as_raw_fd(), 0) } != 0 {
                    exit_child(3);
                }
                drop(theirs);
                let Ok(taken) = take_stdin() else {
                    exit_child(4)
                };
                let mut taken = std::os::unix::net::UnixStream::from(taken);
                // The copy is the channel, close-on-exec, above stderr.
                let flags = fd_flags(taken.as_raw_fd());
                if taken.as_raw_fd() <= 2 || flags != Some(libc::FD_CLOEXEC) {
                    exit_child(5);
                }
                let mut byte = [0u8; 1];
                if ours.write_all(b"x").is_err() || taken.read_exact(&mut byte).is_err() {
                    exit_child(6);
                }
                // Stdin now reads nothing at all.
                let mut rest = Vec::new();
                let stdin_empty = std::io::stdin()
                    .read_to_end(&mut rest)
                    .is_ok_and(|n| n == 0);
                exit_child(if byte == *b"x" && stdin_empty { 0 } else { 7 });
            }
        };
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        assert_eq!(
            child_exit_code(status),
            0,
            "2: no pair; 3: no dup2; 4: take failed; 5: the copy isn't close-on-exec above \
             stderr; 6: the copy isn't the channel; 7: stdin isn't /dev/null"
        );
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

    /// Whether `fd` is the far end of `peer`: a byte written there arrives.
    fn reaches(fd: i32, peer: &mut std::os::unix::net::UnixStream) -> bool {
        use std::io::Read as _;
        let byte = [0x5a_u8];
        // SAFETY: writes one byte from a live buffer to an open fd.
        let wrote = unsafe { libc::write(fd, byte.as_ptr().cast(), 1) };
        let mut got = [0u8; 1];
        wrote == 1 && peer.read_exact(&mut got).is_ok() && got == byte
    }

    /// `stream`, moved above every number a test places on: a pair's ends
    /// are the lowest free numbers, which may be the targets.
    fn high(stream: std::os::unix::net::UnixStream) -> std::os::unix::net::UnixStream {
        let moved = rustix::io::fcntl_dupfd_cloexec(&stream, 100).expect("dup above the targets");
        drop(stream);
        std::os::unix::net::UnixStream::from(moved)
    }

    /// Runs `body` in a forked child and returns its exit code.
    fn in_child(body: impl FnOnce() -> i32) -> i32 {
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => exit_child(body()),
        };
        let status = child
            .wait_deadline(Duration::from_secs(30))
            .expect("bounded wait");
        child_exit_code(status)
    }

    #[test]
    fn placed_fds_sit_on_their_numbers_in_order_and_cross_the_exec() {
        let code = in_child(|| {
            let (Ok((a, a_end)), Ok((b, b_end))) = (
                std::os::unix::net::UnixStream::pair(),
                std::os::unix::net::UnixStream::pair(),
            ) else {
                return 2;
            };
            let (mut a, mut b) = (high(a), high(b));
            if place_across_exec(&[a_end.as_fd(), b_end.as_fd()], 3).is_err() {
                return 3;
            }
            // The originals may have been on 3 or 4 and were replaced:
            // let go of them without closing a placed descriptor.
            let _replaced = a_end.into_raw_fd();
            let _replaced = b_end.into_raw_fd();
            if fd_flags(3) != Some(0) || fd_flags(4) != Some(0) {
                return 4;
            }
            if !reaches(3, &mut a) || !reaches(4, &mut b) {
                return 5;
            }
            0
        });
        assert_eq!(
            code, 0,
            "2: no pairs; 3: placing failed; 4: not inheritable; 5: not in order"
        );
    }

    #[test]
    fn a_source_on_another_sources_target_is_not_lost() {
        let code = in_child(|| {
            let (Ok((a, a_end)), Ok((b, b_end))) = (
                std::os::unix::net::UnixStream::pair(),
                std::os::unix::net::UnixStream::pair(),
            ) else {
                return 2;
            };
            let (mut a, mut b) = (high(a), high(b));
            // `a_end` on 4 and `b_end` on 3, then placed swapped: a one-step
            // placement would put `a_end` on 3 over `b_end` and then copy
            // it to 4 as well.
            // SAFETY: dup2 of an open fd onto a number this child gives up.
            let on4 = unsafe { libc::dup2(a_end.as_raw_fd(), 4) };
            // SAFETY: as above.
            let on3 = unsafe { libc::dup2(b_end.as_raw_fd(), 3) };
            if (on4, on3) != (4, 3) {
                return 3;
            }
            let _replaced = a_end.into_raw_fd();
            let _replaced = b_end.into_raw_fd();
            // SAFETY: 4 is open in this child, and stays open for the call.
            let on4 = unsafe { BorrowedFd::borrow_raw(4) };
            // SAFETY: as is 3.
            let on3 = unsafe { BorrowedFd::borrow_raw(3) };
            if place_across_exec(&[on4, on3], 3).is_err() {
                return 4;
            }
            if !reaches(3, &mut a) || !reaches(4, &mut b) {
                return 5;
            }
            0
        });
        assert_eq!(
            code, 0,
            "2: no pairs; 3: no setup; 4: placing failed; 5: a source was lost"
        );
    }

    #[test]
    fn stdio_is_not_placed_over_and_the_copies_are_closed() {
        for first in [0, 1, 2] {
            assert!(
                matches!(
                    place_across_exec(&[], first),
                    Err(Error::InvalidExecArg { .. })
                ),
                "{first}"
            );
        }
        assert!(place_across_exec(&[], i32::MAX).is_ok());
        assert!(matches!(
            place_across_exec(&[std::io::stdout().as_fd()], i32::MAX),
            Err(Error::InvalidExecArg { .. })
        ));
        let code = in_child(|| {
            let Ok((a, a_end)) = std::os::unix::net::UnixStream::pair() else {
                return 2;
            };
            let _peer = high(a);
            let before: std::collections::BTreeSet<i32> = open_fds(1024).into_iter().collect();
            if place_across_exec(&[a_end.as_fd()], 3).is_err() {
                return 3;
            }
            // Nothing new but fd 3: the copy made above the targets, on the
            // lowest free number, is gone.
            let after: std::collections::BTreeSet<i32> = open_fds(1024).into_iter().collect();
            if after.difference(&before).any(|&fd| fd != 3) {
                return 4;
            }
            0
        });
        assert_eq!(code, 0, "2: no pair; 3: placing failed; 4: a copy leaked");
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

    /// A pipe carries what's written to its read end, both ends close on
    /// exec; a wait on several descriptors says which is readable, and a
    /// hung-up one counts.
    #[test]
    fn a_pipe_carries_bytes_and_a_wait_says_which_end_is_ready() {
        use std::io::{Read as _, Write as _};
        let (read, write) = pipe().unwrap();
        for end in [&read, &write] {
            let flags = rustix::io::fcntl_getfd(end).unwrap();
            assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
        }
        let (quiet, _quiet_write) = pipe().unwrap();
        let mut writer = std::fs::File::from(write);
        writer.write_all(b"x").unwrap();
        assert_eq!(
            wait_readable(&[quiet.as_fd(), read.as_fd()]).unwrap(),
            [false, true]
        );
        let mut byte = [0_u8; 1];
        std::fs::File::from(read.try_clone().unwrap())
            .read_exact(&mut byte)
            .unwrap();
        assert_eq!(&byte, b"x");
        drop(writer);
        assert_eq!(
            wait_readable(&[read.as_fd(), quiet.as_fd()]).unwrap(),
            [true, false],
            "a hung-up end is ready"
        );
    }
}
