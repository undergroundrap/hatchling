//! File-descriptor hygiene across the exec into a cell.
//!
//! CLOEXEC on every fd the broker opens is the first line; this is the
//! structural one: nothing above stderr crosses the exec, whatever
//! anyone forgot to mark.

use std::os::fd::{AsFd as _, AsRawFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

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
    seal_inherited_fds().map_err(|source| Error::Process {
        operation: "close_range(3.., CLOSE_RANGE_CLOEXEC)",
        source,
    })
}

/// The descriptor syscall alone, safe to run in a pre-exec hook.
pub(crate) fn seal_inherited_fds() -> std::io::Result<()> {
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
        Err(std::io::Error::last_os_error())
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

/// Replaces stdin, stdout and stderr with the current root's `/dev/null`.
/// Call after entering the new root and before exec. This discards any
/// authority carried by the original stdio descriptors.
///
/// # Errors
///
/// Returns [`Error::Process`] if the device cannot be opened or any
/// descriptor cannot be replaced. The caller must refuse the exec.
pub fn null_stdio() -> Result<(), Error> {
    let failed = |operation| {
        move |source: rustix::io::Errno| Error::Process {
            operation,
            source: std::io::Error::from_raw_os_error(source.raw_os_error()),
        }
    };
    let null = rustix::fs::open(
        "/dev/null",
        rustix::fs::OFlags::RDWR.union(rustix::fs::OFlags::CLOEXEC),
        rustix::fs::Mode::empty(),
    )
    .map_err(failed("open /dev/null"))?;
    let source = rustix::io::fcntl_dupfd_cloexec(&null, 3)
        .map_err(failed("fcntl(/dev/null, F_DUPFD_CLOEXEC)"))?;
    // If a stdio slot was empty, open may have claimed it. Drop that
    // handle before replacing any slot so it cannot close the new fd.
    drop(null);
    rustix::stdio::dup2_stdin(&source).map_err(failed("dup2(/dev/null, 0)"))?;
    rustix::stdio::dup2_stdout(&source).map_err(failed("dup2(/dev/null, 1)"))?;
    rustix::stdio::dup2_stderr(&source).map_err(failed("dup2(/dev/null, 2)"))
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

/// Takes ownership of the character device this process inherited at
/// `fd`.
///
/// One a launcher that may open it opened for this process, which may
/// not (WO-017: the vault keeper's security keys). It must be above
/// stderr and a character device, and is marked close-on-exec again.
///
/// Call it once for a descriptor: the file it returns owns it.
///
/// # Errors
///
/// [`Error::Process`] if `fd` is stdio, isn't open, or isn't a character
/// device; the descriptor is left as it was.
pub fn inherited_device(fd: i32) -> Result<std::fs::File, Error> {
    let refused = |why: &str| Error::Process {
        operation: "adopt an inherited device",
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, why.to_owned()),
    };
    if fd <= 2 {
        return Err(refused("stdio is not a device"));
    }
    if fd_flags(fd).is_none() {
        return Err(refused("the descriptor is not open"));
    }
    // SAFETY: `fd` is open (checked above), and the borrow ends before
    // anything closes it.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let device = rustix::fs::fstat(borrowed).is_ok_and(|stat| {
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::CharacterDevice
    });
    if !device {
        return Err(refused("the descriptor is not a character device"));
    }
    rustix::io::fcntl_setfd(borrowed, rustix::io::FdFlags::CLOEXEC).map_err(|source| {
        Error::Process {
            operation: "fcntl(F_SETFD, FD_CLOEXEC)",
            source: std::io::Error::from_raw_os_error(source.raw_os_error()),
        }
    })?;
    // SAFETY: `fd` is an open device this process inherited, and the
    // caller hands its ownership over: nothing else here owns it.
    let owned = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    Ok(std::fs::File::from(owned))
}

/// What a descriptor handed to a cell as its stdio is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioKind {
    /// A pipe.
    Pipe,
    /// `/dev/null`.
    Null,
    /// One end of an unnamed pair of local sockets.
    PairedSocket,
    /// A file opened to be written only: a log.
    WrittenFile,
    /// A terminal. A cell's filter refuses the `ioctl` that would type
    /// into it (`TIOCSTI`).
    Terminal,
    /// The journal's stream socket, which systemd connects a service's
    /// output to.
    Journal,
}

impl StdioKind {
    /// Whether it's only for output: a cell's stdout or stderr may be,
    /// its stdin may not.
    #[must_use]
    pub const fn output_only(self) -> bool {
        matches!(self, Self::WrittenFile | Self::Terminal | Self::Journal)
    }
}

/// Where the journal takes a service's output.
const JOURNAL_STREAM: &[u8] = b"/run/systemd/journal/stdout";

/// What `fd` is, when it's one a cell may have as its stdio.
///
/// It may be a pipe, `/dev/null`, one end of an unnamed pair of local
/// sockets, a file opened to be written only, a terminal, or the
/// journal's stream socket.
///
/// Anything else is refused, since a cell's stdio reaches past its own
/// root. An `O_PATH` descriptor or a directory lets its holder open what's
/// under it (`openat(1, ".ssh/id_ed25519")`); a file it may read is the
/// host's to read; another device is the host's hardware; and a socket
/// connected to another named one (a container runtime's, a session bus)
/// is a way into that service.
///
/// # Errors
///
/// [`Error::Process`] saying what it is instead, or why it can't be told.
pub fn stdio_kind(fd: BorrowedFd<'_>) -> Result<StdioKind, Error> {
    use rustix::fs::{FileType, OFlags};
    let refused = |what: String| Error::Process {
        operation: "check a cell's stdio",
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, what),
    };
    let flags = rustix::fs::fcntl_getfl(fd)
        .map_err(|err| refused(format!("its flags can't be read: {err}")))?;
    if flags.contains(OFlags::PATH) {
        return Err(refused(String::from(
            "an O_PATH descriptor, which names a place rather than a stream",
        )));
    }
    let stat =
        rustix::fs::fstat(fd).map_err(|err| refused(format!("it can't be examined: {err}")))?;
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Fifo => Ok(StdioKind::Pipe),
        FileType::CharacterDevice if stat.st_rdev == rustix::fs::makedev(1, 3) => {
            Ok(StdioKind::Null)
        }
        FileType::CharacterDevice if rustix::termios::isatty(fd) => Ok(StdioKind::Terminal),
        // Opened to be written only: what's in it isn't the cell's to read.
        FileType::RegularFile
            if flags.contains(OFlags::WRONLY) && !flags.contains(OFlags::RDWR) =>
        {
            Ok(StdioKind::WrittenFile)
        }
        // An unnamed peer, as a socket pair's is: no address at all, or an
        // empty one. A named or an abstract one is another service's.
        FileType::Socket => match rustix::net::getpeername(fd) {
            Ok(None) => Ok(StdioKind::PairedSocket),
            Ok(Some(rustix::net::SocketAddrAny::Unix(peer)))
                if peer.path().is_none_or(|path| path.to_bytes().is_empty())
                    && peer.abstract_name().is_none_or(<[u8]>::is_empty) =>
            {
                Ok(StdioKind::PairedSocket)
            }
            Ok(Some(rustix::net::SocketAddrAny::Unix(peer)))
                if peer
                    .path()
                    .is_some_and(|path| path.to_bytes() == JOURNAL_STREAM) =>
            {
                Ok(StdioKind::Journal)
            }
            Ok(Some(_)) => Err(refused(String::from(
                "a socket connected to a named one, or not a local one",
            ))),
            Err(err) => Err(refused(format!("a socket with no peer: {err}"))),
        },
        other @ (FileType::RegularFile
        | FileType::Directory
        | FileType::Symlink
        | FileType::CharacterDevice
        | FileType::BlockDevice
        | FileType::Unknown) => Err(refused(format!("a {other:?}"))),
    }
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
    wait_readable_until(fds, None)
}

/// As [`wait_readable`], for no longer than `limit`.
///
/// When `limit` passes with none of `fds` ready, every flag is false. A
/// wait a signal interrupts is waited again for what's left of `limit`.
/// Part of a millisecond is waited as a whole one, so a short `limit`
/// never spins.
///
/// # Errors
///
/// Returns [`Error::Process`] if the wait fails.
pub fn wait_readable_for(fds: &[BorrowedFd<'_>], limit: Duration) -> Result<Vec<bool>, Error> {
    wait_readable_until(fds, Some(Instant::now() + limit))
}

/// How long `poll` is told to wait for `left`: its milliseconds rounded
/// up, and no more than `poll` can take.
fn poll_millis(left: Duration) -> i32 {
    let millis = left.as_nanos().div_ceil(1_000_000);
    i32::try_from(millis).unwrap_or(i32::MAX)
}

fn wait_readable_until(
    fds: &[BorrowedFd<'_>],
    deadline: Option<Instant>,
) -> Result<Vec<bool>, Error> {
    use rustix::event::{PollFd, PollFlags, poll};
    let mut polled: Vec<PollFd<'_>> = fds
        .iter()
        .map(|fd| PollFd::new(fd, PollFlags::IN))
        .collect();
    loop {
        let timeout = deadline.map_or(-1, |deadline| {
            poll_millis(deadline.saturating_duration_since(Instant::now()))
        });
        match poll(&mut polled, timeout) {
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

/// The most a write into a pipe can carry and still be atomic: Linux's
/// `PIPE_BUF`.
pub const PIPE_ATOMIC_BYTES: usize = 4096;

/// Whether `fd` is a pipe, or a FIFO: what [`write_whole_to_pipe`]
/// writes into. Never a file, a socket or a terminal.
#[must_use]
pub fn is_pipe(fd: BorrowedFd<'_>) -> bool {
    rustix::fs::fstat(fd).is_ok_and(|stat| {
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Fifo
    })
}

/// Writes `bytes` into the pipe `fd` in one write, whole or not at all,
/// then closes it: for a secret going straight into the program at the
/// pipe's other end (0.9: a room disk's key into cryptsetup).
///
/// Only into a pipe ([`is_pipe`]), where nothing is kept once it's read,
/// and only up to [`PIPE_ATOMIC_BYTES`], which a pipe takes whole. The
/// write doesn't wait: a pipe that's full, or that no one reads from,
/// fails it rather than hold the writer.
///
/// # Errors
///
/// [`Error::Process`] when `fd` isn't a pipe, `bytes` is too long, or the
/// write doesn't go whole. Nothing is written then.
pub fn write_whole_to_pipe(fd: OwnedFd, bytes: &[u8]) -> Result<(), Error> {
    use rustix::fs::OFlags;
    let failed = |source: std::io::Error| Error::Process {
        operation: "write into a pipe",
        source,
    };
    let refused = |why: &str| failed(std::io::Error::new(std::io::ErrorKind::InvalidInput, why));
    if !is_pipe(fd.as_fd()) {
        return Err(refused("it isn't a pipe"));
    }
    if bytes.len() > PIPE_ATOMIC_BYTES {
        return Err(refused("more than a pipe takes whole"));
    }
    let raw = |source: rustix::io::Errno| failed(std::io::Error::from(source));
    let flags = rustix::fs::fcntl_getfl(&fd).map_err(raw)?;
    rustix::fs::fcntl_setfl(&fd, flags | OFlags::NONBLOCK).map_err(raw)?;
    let written = rustix::io::write(&fd, bytes).map_err(raw)?;
    // Closed now: its reader sees the end as soon as it has the bytes.
    drop(fd);
    if written == bytes.len() {
        Ok(())
    } else {
        Err(refused("the pipe took part of it"))
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::IntoRawFd as _;
    use std::time::Duration;

    use super::*;

    /// A cell's stdio is a pipe, `/dev/null` or a socket pair's end, and
    /// nothing else: not a place to open files under, a file, another
    /// device, or a socket connected to a named one.
    #[test]
    fn a_cells_stdio_is_a_stream_and_nothing_else() {
        use std::os::linux::net::SocketAddrExt as _;
        use std::os::unix::net::{UnixListener, UnixStream};
        let (read, write) = pipe().unwrap();
        assert_eq!(stdio_kind(read.as_fd()).unwrap(), StdioKind::Pipe);
        assert_eq!(stdio_kind(write.as_fd()).unwrap(), StdioKind::Pipe);
        let null = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(stdio_kind(null.as_fd()).unwrap(), StdioKind::Null);
        for kind in [StdioKind::Pipe, StdioKind::Null, StdioKind::PairedSocket] {
            assert!(!kind.output_only(), "{kind:?}");
        }
        for kind in [
            StdioKind::WrittenFile,
            StdioKind::Terminal,
            StdioKind::Journal,
        ] {
            assert!(kind.output_only(), "{kind:?}");
        }
        let (ours, theirs) = UnixStream::pair().unwrap();
        assert_eq!(stdio_kind(ours.as_fd()).unwrap(), StdioKind::PairedSocket);
        drop(theirs);
        let dir = std::env::temp_dir().join(format!("hatchling-stdio-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("file");
        std::fs::write(&file, b"the host's").unwrap();
        // A log, opened to be written only.
        let log = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        assert_eq!(stdio_kind(log.as_fd()).unwrap(), StdioKind::WrittenFile);
        let listening = UnixListener::bind(dir.join("service.sock")).unwrap();
        let connected = UnixStream::connect(dir.join("service.sock")).unwrap();
        // A service in the abstract namespace, as a session bus can be.
        let abstract_name = format!("hatchling-stdio-{}", std::process::id());
        let address =
            std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
        let abstract_listening = UnixListener::bind_addr(&address).unwrap();
        let abstract_connected = UnixStream::connect_addr(&address).unwrap();
        let place = rustix::fs::open(
            &dir,
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::DIRECTORY,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let refused = [
            ("O_PATH", place),
            (
                "directory",
                OwnedFd::from(std::fs::File::open(&dir).unwrap()),
            ),
            ("file", OwnedFd::from(std::fs::File::open(&file).unwrap())),
            (
                "file read and written",
                OwnedFd::from(
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&file)
                        .unwrap(),
                ),
            ),
            (
                "device",
                OwnedFd::from(std::fs::File::open("/dev/zero").unwrap()),
            ),
            ("named", OwnedFd::from(connected)),
            ("abstract", OwnedFd::from(abstract_connected)),
            ("listening", OwnedFd::from(listening)),
        ];
        for (what, fd) in &refused {
            let error = stdio_kind(fd.as_fd()).unwrap_err().to_string();
            assert!(error.contains("check a cell's stdio"), "{what}: {error}");
        }
        drop((refused, abstract_listening));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An inherited device is adopted, close-on-exec again; stdio, a
    /// descriptor that isn't open, and one that isn't a device are
    /// refused and left as they were.
    #[test]
    fn an_inherited_device_is_adopted_and_nothing_else() {
        let null = std::fs::File::open("/dev/null").unwrap();
        keep_across_exec(null.as_fd()).unwrap();
        let raw = null.into_raw_fd();
        let adopted = inherited_device(raw).unwrap();
        assert_eq!(adopted.as_raw_fd(), raw);
        assert_ne!(
            fd_flags(raw).unwrap() & libc::FD_CLOEXEC,
            0,
            "close-on-exec again"
        );
        let (read, _write) = pipe().unwrap();
        assert!(inherited_device(read.as_raw_fd()).is_err());
        assert!(fd_flags(read.as_raw_fd()).is_some(), "left as it was");
        assert!(inherited_device(2).is_err());
        drop(adopted);
        assert!(inherited_device(raw).is_err(), "no longer open");
    }
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
    fn null_stdio_replaces_open_and_closed_slots() {
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                close(0);
                let replaced = null_stdio().is_ok()
                    && [
                        rustix::stdio::stdin(),
                        rustix::stdio::stdout(),
                        rustix::stdio::stderr(),
                    ]
                    .into_iter()
                    .all(|fd| stdio_kind(fd).ok() == Some(StdioKind::Null));
                exit_child(i32::from(!replaced));
            }
        };
        assert_eq!(
            child_exit_code(child.wait_deadline(Duration::from_secs(5)).unwrap()),
            0
        );
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
                if seal_inherited_fds().is_err() {
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

    /// A timed wait gives up at its limit with nothing ready, not before
    /// it and not long after, and still says at once what is ready. An
    /// untimed wait goes on until something is.
    #[test]
    fn a_timed_wait_gives_up_at_its_limit_and_an_untimed_one_does_not() {
        use std::io::Write as _;
        use std::sync::mpsc;
        let (read, write) = pipe().unwrap();
        let mut writer = std::fs::File::from(write);
        // A wait that never gives up is ended here, so it fails its
        // check instead of holding the test.
        let (finished, watching) = mpsc::channel::<()>();
        let (late_read, late_write) = pipe().unwrap();
        std::thread::scope(|scope| {
            let mut rescue = writer.try_clone().unwrap();
            scope.spawn(move || {
                if watching.recv_timeout(Duration::from_secs(8)).is_err() {
                    let _ = rescue.write_all(b"!");
                }
            });
            let limit = Duration::from_millis(150);
            let started = Instant::now();
            let ready = wait_readable_for(&[read.as_fd()], limit).unwrap();
            let waited = started.elapsed();
            assert_eq!(ready, [false], "nothing was ready after {waited:?}");
            assert!(waited >= limit, "gave up early, after {waited:?}");
            assert!(waited < Duration::from_secs(5), "waited {waited:?}");

            // Nothing is ready for a while: an untimed wait is still
            // waiting when it comes.
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                std::fs::File::from(late_write).write_all(b"x").unwrap();
            });
            let started = Instant::now();
            assert_eq!(wait_readable(&[late_read.as_fd()]).unwrap(), [true]);
            assert!(started.elapsed() >= Duration::from_millis(250));

            writer.write_all(b"x").unwrap();
            let started = Instant::now();
            assert_eq!(
                wait_readable_for(&[read.as_fd()], Duration::from_secs(30)).unwrap(),
                [true]
            );
            assert!(started.elapsed() < Duration::from_secs(5));
            assert_eq!(
                wait_readable_for(&[read.as_fd()], Duration::ZERO).unwrap(),
                [true],
                "no limit left still looks once"
            );
            let _ = finished.send(());
        });
    }

    /// Part of a millisecond is a whole one to `poll`, so a wait with a
    /// little left sleeps instead of spinning; a whole number stays itself,
    /// nothing left is no wait, and more than `poll` takes is its most.
    #[test]
    fn poll_is_told_whole_milliseconds_rounded_up() {
        assert_eq!(poll_millis(Duration::ZERO), 0);
        assert_eq!(poll_millis(Duration::from_nanos(1)), 1);
        assert_eq!(poll_millis(Duration::from_millis(7)), 7);
        assert_eq!(poll_millis(Duration::from_micros(7_001)), 8);
        assert_eq!(poll_millis(Duration::from_secs(u64::MAX)), i32::MAX);
    }

    /// What a pipe's reading end holds now, without waiting.
    fn waiting_in(read: &OwnedFd) -> Vec<u8> {
        let flags = rustix::fs::fcntl_getfl(read).unwrap();
        rustix::fs::fcntl_setfl(read, flags | rustix::fs::OFlags::NONBLOCK).unwrap();
        let mut held = vec![0u8; 128 * 1024];
        let read = rustix::io::read(read, &mut held).unwrap_or(0);
        held.truncate(read);
        held
    }

    /// Bytes go into a pipe whole, in one write, and the writing end is
    /// closed after. Into anything that isn't a pipe (a file, a socket),
    /// nothing goes; nor into a pipe that's full, which fails at once
    /// rather than wait; nor more than a pipe takes whole.
    #[test]
    fn bytes_go_into_a_pipe_whole_or_not_at_all() {
        use std::os::unix::net::UnixStream;
        let (read, write) = pipe().unwrap();
        assert!(is_pipe(read.as_fd()) && is_pipe(write.as_fd()));
        write_whole_to_pipe(write, b"sixty-four bytes, or so").unwrap();
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut std::fs::File::from(read), &mut got).unwrap();
        assert_eq!(
            got, b"sixty-four bytes, or so",
            "not written whole, or not closed"
        );

        let dir = std::env::temp_dir().join(format!("hatchling-fds-pipe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("file");
        let file = std::fs::File::create(&path).unwrap();
        assert!(!is_pipe(file.as_fd()));
        assert!(
            write_whole_to_pipe(OwnedFd::from(file), b"kept").is_err(),
            "a file was taken for a pipe"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            0,
            "written into a file"
        );
        std::fs::remove_dir_all(&dir).unwrap();

        let (socket, peer) = UnixStream::pair().unwrap();
        assert!(!is_pipe(socket.as_fd()));
        assert!(
            write_whole_to_pipe(OwnedFd::from(socket), b"sent").is_err(),
            "a socket was taken for a pipe"
        );
        let mut sent = Vec::new();
        std::io::Read::read_to_end(&mut &peer, &mut sent).unwrap();
        assert!(sent.is_empty(), "written into a socket");

        // A full pipe, already set not to wait: the write fails at once,
        // on a thread of its own so a write that waits fails this test
        // rather than hang it.
        let (read, write) = pipe().unwrap();
        let flags = rustix::fs::fcntl_getfl(&write).unwrap();
        rustix::fs::fcntl_setfl(&write, flags | rustix::fs::OFlags::NONBLOCK).unwrap();
        let mut filled = 0;
        while let Ok(more) = rustix::io::write(&write, &[1u8; 4096]) {
            filled += more;
        }
        let (told, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = told.send(write_whole_to_pipe(write, b"late").is_err());
        });
        assert_eq!(
            answer.recv_timeout(Duration::from_secs(10)).ok(),
            Some(true),
            "a full pipe held the writer, or took it"
        );
        let waiting = waiting_in(&read);
        assert_eq!(waiting.len(), filled, "a full pipe took part of it");

        // A full pipe that would wait: the write is set not to, and fails.
        let (read, write) = pipe().unwrap();
        let room = rustix::pipe::fcntl_getpipe_size(&write).unwrap();
        rustix::io::write(&write, &vec![3u8; room]).unwrap();
        let (told, answer) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = told.send(write_whole_to_pipe(write, b"late").is_err());
        });
        assert_eq!(
            answer.recv_timeout(Duration::from_secs(10)).ok(),
            Some(true),
            "a full pipe that waits held the writer, or took it"
        );
        assert_eq!(waiting_in(&read).len(), room, "a full pipe took part of it");

        // As much as a pipe takes whole goes; one byte more doesn't.
        let (read, write) = pipe().unwrap();
        let whole = vec![4u8; PIPE_ATOMIC_BYTES];
        write_whole_to_pipe(write, &whole).unwrap();
        assert_eq!(
            waiting_in(&read),
            whole,
            "all a pipe takes whole wasn't written"
        );
        let (read, write) = pipe().unwrap();
        let long = vec![2u8; PIPE_ATOMIC_BYTES + 1];
        assert!(
            write_whole_to_pipe(write, &long).is_err(),
            "more than a pipe takes whole was taken"
        );
        assert!(
            waiting_in(&read).is_empty(),
            "too much for one write, written"
        );
    }
}
