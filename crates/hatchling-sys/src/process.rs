//! Process primitives: fork, exec, supervised child handles, `NoNewPrivs`.
//!
//! The wrappers are minimal on purpose. Code running in a fork child
//! must stay async-signal-safe until `exec`: only raw syscalls and
//! no-allocation logic. The spawner is responsible for that discipline;
//! these wrappers just make the syscalls callable.
//!
//! Child supervision goes through [`Child`]: a pidfd-pinned handle
//! with bounded waits and kill-on-drop. There is no unbounded wait and
//! no signalling primitive that accepts a bare pid — raw pids are not
//! identities (`0` and `-1` have group/all meanings to `kill(2)`), so
//! they cannot name a signal target here.

use std::cell::Cell;
use std::ffi::CString;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::time::Duration;

use rustix::process::{Pid, PidfdFlags, Signal, WaitId, WaitidOptions, WaitidStatus};

use crate::Error;

/// The outcome of [`fork`].
#[derive(Debug)]
pub enum ForkResult {
    /// The parent: `child` supervises the forked child (pidfd-pinned,
    /// bounded waits, kill-on-drop).
    Parent {
        /// Supervision handle for the forked child.
        child: Child,
    },
    /// The child: single-threaded, async-signal-safe until `exec`.
    Child,
}

/// Forks the current process.
///
/// In the parent, supervision ownership of the child is taken
/// immediately: the returned [`Child`] pins the child with a pidfd, so
/// signalling and waiting can never touch a reused pid. If the pidfd
/// cannot be opened the child is `SIGKILL`ed and reaped and an error is
/// returned — fail closed, never an unsupervised child.
///
/// # Errors
///
/// Returns [`Error::Process`] if the `fork` syscall or the pidfd setup
/// fails.
///
/// # Safety contract (caller)
///
/// In the child, only async-signal-safe operations are sound until
/// `exec` replaces the image. In practice the spawner forks before any
/// other thread exists and the child performs only raw syscalls.
pub fn fork() -> Result<ForkResult, Error> {
    // Every child is born supervised (see `arm_pdeathsig_in_child`);
    // registered once per process, before the first fork.
    let registered = *ATFORK_REGISTERED.get_or_init(|| {
        // SAFETY: registers only a child handler, which makes one
        // async-signal-safe syscall; no prepare or parent handlers.
        unsafe { libc::pthread_atfork(None, None, Some(arm_pdeathsig_in_child)) }
    });
    if registered != 0 {
        return Err(Error::Process {
            operation: "pthread_atfork",
            source: io::Error::from_raw_os_error(registered),
        });
    }
    // SAFETY: fork is sound to call; the child-safety discipline is the
    // caller's contract, documented above.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(Error::Process {
            operation: "fork",
            source: io::Error::last_os_error(),
        });
    }
    if pid == 0 {
        return Ok(ForkResult::Child);
    }
    // Parent: this pid just came from fork, so it is unambiguously our
    // child and the validation below cannot fail — but the type still
    // goes through it, so no raw pid ever reaches the signal path.
    let pid = ChildPid::new(pid).ok_or_else(|| Error::Process {
        operation: "fork",
        source: io::Error::other("fork returned an invalid pid"),
    })?;
    let child = Child::new(pid).inspect_err(|_| {
        // pidfd_open failed: the child exists but cannot be supervised
        // pidfd-style. Fail closed — it must not outlive this error.
        // Signalling with the just-returned pid is sound here (and only
        // here): the child is ours and unreaped, so no reuse window
        // exists.
        //
        // Defense in depth at the syscall boundary: re-validate the
        // raw pid before using it. `ChildPid` checks `> 1` at
        // construction, but the value is re-read here, and a 0/-1
        // would make kill(2) address our own group or every process —
        // and make waitpid(2) reap an unrelated child.
        if is_safe_kill_target(pid.get()) {
            // SAFETY: kill on our own unreaped child (pid > 1, never a
            // group or broadcast target); failure is ignored, the reap
            // below is what matters.
            unsafe {
                libc::kill(pid.get(), libc::SIGKILL);
            }
            // SAFETY: waitpid on our own child with a null status slot;
            // blocks only until the SIGKILL above lands.
            unsafe {
                libc::waitpid(pid.get(), std::ptr::null_mut(), 0);
            }
        }
        // Without a trustworthy pid there is nothing safe to signal or
        // reap; the Error below still propagates. Unreachable without a
        // corrupted accessor — in correct code the pid just came from
        // fork and `ChildPid::new` already accepted it.
    })?;
    Ok(ForkResult::Parent { child })
}

/// Whether a failed `poll` should simply be retried: only `EINTR` (a
/// signal interrupted the wait). Every other error fails closed.
///
/// Pure and table-tested: the error path cannot be reached from a live
/// test without fault injection, so the retry decision is pinned here
/// instead (an inverted check would retry a real error forever, or give
/// up on a harmless signal).
const fn is_retryable_poll_error(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::Interrupted)
}

/// A validated child pid: the pid of a direct child of this process.
///
/// The invariant is `> 0` and not this process's own pid. Raw pids are
/// not identities — `0` addresses the caller's process group and `-1`
/// every signallable process to `kill(2)` — so every wait and signal
/// primitive in this module takes a [`ChildPid`], never a bare `i32`.
/// There is no way to spell "signal my own process group" with this
/// type: the Issue A mutant (`pid == 0` → `pid != 0`) handed the
/// child the parent branch with pid 0, and the old `terminate(0)`
/// became `kill(0, SIGKILL)`. [`ChildPid::new`] rejects 0 at
/// construction, so that path cannot be built anymore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChildPid(i32);

impl ChildPid {
    /// Validates a raw pid as naming a child process. Returns `None`
    /// for 0, negative pids, and this process's own pid.
    #[must_use]
    pub fn new(pid: i32) -> Option<Self> {
        // SAFETY: getpid takes no arguments and always succeeds.
        let own = unsafe { libc::getpid() };
        if pid > 0 && pid != own {
            Some(Self(pid))
        } else {
            None
        }
    }

    /// The raw pid, for `/proc` paths and diagnostics. Never a signal
    /// target: signalling goes through [`Child`]'s pidfd.
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }
}

/// Whether `raw` is safe to hand to `kill(2)` or `waitpid(2)` as a
/// single-process target.
///
/// Pure and unit-tested: the syscall boundary re-validates what
/// [`ChildPid`] promises at construction, because the raw value is
/// re-read at the call site. A corrupted accessor (the
/// `ChildPid::get → 1` mutant) or a future refactor must not be able
/// to turn a signal into `kill(0, ...)` (own group) or `kill(-1, ...)`
/// (every process) — or a `waitpid` into a reap of an unrelated child.
#[must_use]
const fn is_safe_kill_target(raw: i32) -> bool {
    raw > 1
}

/// Whether `raw` is safe to hand to `kill(2)` as a process-group
/// target, given our own process group `own_group`.
///
/// Extends [`is_safe_kill_target`]: a group id equal to our own group
/// would signal every process in it, so it is refused too.
#[must_use]
const fn is_safe_pgid_target(raw: i32, own_group: i32) -> bool {
    is_safe_kill_target(raw) && raw != own_group
}

/// Ownership of a direct child process: pidfd-pinned signalling,
/// bounded waits, and kill-on-drop.
///
/// The pidfd pins the *process*, not the pid number: signalling and
/// waiting through it can never touch a reused pid, and the `0`/`-1`
/// special meanings of `kill(2)` do not exist for
/// `pidfd_send_signal`. Dropping a `Child` that was never reaped sends
/// `SIGKILL` first — no live orphans — then reaps without blocking.
#[derive(Debug)]
pub struct Child {
    pid: ChildPid,
    pidfd: OwnedFd,
    /// Whether the child has been reaped. A pidfd stays readable after
    /// its process is reaped, so without this flag a second
    /// `wait_deadline` would see "readable" → `waitid` → `ECHILD` →
    /// `None` and misreport the already-reaped child as timed out.
    reaped: Cell<bool>,
}

impl AsFd for Child {
    /// The child's pidfd: for fd hygiene, the descriptors a process keeps.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }
}

/// Grace period after a deadline SIGKILL for the child to die and be
/// reaped. A normal child dies near-instantly; only uninterruptible
/// sleep outlives this, and no signal can reach that state anyway.
const REAP_GRACE: Duration = Duration::from_secs(5);

/// Grace for the best-effort reap in [`Child`]'s `Drop`. The SIGKILL is
/// already in flight when the reap starts, so this returns as soon as
/// the child dies — it only ever waits the full grace for an
/// unkillable child, and it never blocks unboundedly.
const DROP_REAP_GRACE: Duration = Duration::from_secs(1);

impl Child {
    /// Takes supervision ownership of a child pid: opens a pidfd for
    /// it.
    ///
    /// Fails closed when `pidfd_open` is unavailable: supervising
    /// without a pidfd would fall back to `kill(2)` pid semantics,
    /// which this type exists to forbid.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Process`] if `pidfd_open` fails.
    pub fn new(pid: ChildPid) -> Result<Self, Error> {
        // ChildPid already guarantees `> 0`, so this cannot fail — the
        // error arm is defensive, keeping the function total.
        let raw = Pid::from_raw(pid.get()).ok_or_else(|| Error::Process {
            operation: "pidfd_open",
            source: io::Error::new(io::ErrorKind::InvalidInput, "pid is not positive"),
        })?;
        let pidfd = rustix::process::pidfd_open(raw, PidfdFlags::empty()).map_err(|source| {
            Error::Process {
                operation: "pidfd_open",
                source: io::Error::from_raw_os_error(source.raw_os_error()),
            }
        })?;
        Ok(Self {
            pid,
            pidfd,
            reaped: Cell::new(false),
        })
    }

    /// The validated pid of the supervised child.
    #[must_use]
    pub const fn pid(&self) -> ChildPid {
        self.pid
    }

    /// Sends `SIGKILL` to exactly this process via its pidfd.
    /// Best-effort: an already-exited child is `Ok`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Process`] if the signal cannot be sent to a
    /// still-existing child.
    pub fn terminate(&self) -> Result<(), Error> {
        match rustix::process::pidfd_send_signal(self.pidfd.as_fd(), Signal::Kill) {
            // The child already exited: not an error for the caller.
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(source) => Err(Error::Process {
                operation: "pidfd_send_signal",
                source: io::Error::from_raw_os_error(source.raw_os_error()),
            }),
        }
    }

    /// Sends `SIGKILL` to the child's entire process group.
    ///
    /// The child must be a process-group leader (spawned with its own
    /// group, e.g. `std::os::unix::process::CommandExt::process_group(0)`);
    /// the group ID is then the child's pid, which [`ChildPid`]
    /// guarantees is positive and not our own — so this cannot spell
    /// `kill(0, SIGKILL)` (own group) or `kill(-1, SIGKILL)` (every
    /// process). Callers that did not put the child in its own group
    /// must not use this: the group ID would be some other process's.
    ///
    /// Pidfds cannot address process groups, so this is the one
    /// signalling primitive that goes through `kill(2)` group
    /// semantics instead of [`terminate`](Self::terminate)'s pidfd.
    /// The `ChildPid` validation is what keeps the 0/-1 footguns out.
    /// Use this when the child may have spawned grandchildren (build
    /// tools do): `terminate` only reaches the direct child, and
    /// orphaned grandchildren would outlive supervision. Best-effort:
    /// an already-exited group is `Ok`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Process`] if the signal cannot be sent to a
    /// still-existing group.
    pub fn terminate_group(&self) -> Result<(), Error> {
        let raw = self.pid.get();
        // SAFETY: getpgrp takes no arguments and always succeeds.
        let own_group = unsafe { libc::getpgrp() };
        // Defense in depth at the syscall boundary: `ChildPid`
        // validates `> 0` at construction, but the raw value is re-read
        // here. A group id <= 1 would make kill(2) address our own
        // group (0) or every process (-1); a group id equal to our own
        // group would signal every process in it — whatever the type
        // invariant claims.
        if !is_safe_pgid_target(raw, own_group) {
            return Err(Error::Process {
                operation: "kill_process_group",
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "refusing to signal pgid <= 1 or our own group",
                ),
            });
        }
        // `is_safe_pgid_target` already established `raw > 1`, so this
        // cannot fail — the error arm is defensive, keeping the
        // function total.
        let pgid = Pid::from_raw(raw).ok_or_else(|| Error::Process {
            operation: "kill_process_group",
            source: io::Error::new(io::ErrorKind::InvalidInput, "pgid is not positive"),
        })?;
        match rustix::process::kill_process_group(pgid, Signal::Kill) {
            // The group already exited: not an error for the caller.
            Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
            Err(source) => Err(Error::Process {
                operation: "kill_process_group",
                source: io::Error::from_raw_os_error(source.raw_os_error()),
            }),
        }
    }

    /// Polls the child without blocking, returning its raw wait status
    /// (see [`child_exit_code`]) if it has already exited and been
    /// reaped.
    ///
    /// Returns `Ok(None)` while the child is still running, or after it
    /// was already reaped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Process`] if `waitid` fails.
    pub fn try_wait(&self) -> Result<Option<i32>, Error> {
        loop {
            match rustix::process::waitid(
                WaitId::PidFd(self.pidfd.as_fd()),
                WaitidOptions::EXITED | WaitidOptions::NOHANG,
            ) {
                // Not running: either still alive, or already reaped by an
                // earlier wait — nothing to report either way.
                Ok(None) | Err(rustix::io::Errno::CHILD) => return Ok(None),
                Ok(Some(status)) => {
                    self.reaped.set(true);
                    return Ok(Some(raw_status(&status)));
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(source) => {
                    return Err(Error::Process {
                        operation: "waitid",
                        source: io::Error::from_raw_os_error(source.raw_os_error()),
                    });
                }
            }
        }
    }

    /// Waits up to `timeout` for the child to exit, returning its raw
    /// wait status (see [`child_exit_code`]).
    ///
    /// This is the only blocking wait in the crate, and it is bounded:
    /// when the deadline passes with the child still alive, the child
    /// is `SIGKILL`ed via its pidfd, reaped, and
    /// [`Error::ProcessTimeout`] is returned. Fail closed — never a
    /// hang, never an orphan. (A child that exits just after the
    /// deadline counts as timed out: the deadline is the contract.)
    ///
    /// # Errors
    ///
    /// Returns [`Error::ProcessTimeout`] when the deadline expires, or
    /// [`Error::Process`] if the wait itself fails — or if the child
    /// was already reaped by an earlier wait (a second wait has no
    /// status to report; it must not be misread as a timeout).
    pub fn wait_deadline(&self, timeout: Duration) -> Result<i32, Error> {
        if self.reaped.get() {
            return Err(Error::Process {
                operation: "wait on reaped child",
                source: io::Error::new(io::ErrorKind::InvalidInput, "child was already reaped"),
            });
        }
        // Bounded wait for a clean exit. EINTR inside is retried by
        // poll_once, so None here means the deadline genuinely passed
        // with the child alive.
        if let Some(status) = self.poll_once(timeout)? {
            return Ok(status);
        }
        // Still alive past the deadline: fail closed. SIGKILL goes to
        // exactly this process via its pidfd — never a group, never a
        // reused pid — then reap (bounded), then report. (`try_wait`
        // marks the child reaped if the reap succeeds.)
        let _ = self.terminate();
        let _ = self.poll_once(REAP_GRACE);
        Err(Error::ProcessTimeout {
            operation: "child wait",
            timeout,
        })
    }

    /// One bounded poll step: waits up to `timeout` for the pidfd to
    /// become readable (the child exited), then reaps via the pidfd.
    /// Returns `Ok(None)` only when the deadline genuinely passed.
    fn poll_once(&self, timeout: Duration) -> Result<Option<i32>, Error> {
        // `poll` takes milliseconds as c_int: saturate, never wrap.
        let ms = i32::try_from(timeout.as_millis().min(u128::from(u32::MAX))).unwrap_or(i32::MAX);
        let mut pfd = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            // SAFETY: `pfd` is a valid one-element pollfd array; `poll`
            // only reads it.
            let ret = unsafe { libc::poll(&raw mut pfd, 1, ms) };
            // poll returns 1 on child exit, 0 on timeout, exactly -1 on
            // error. Matching -1 exactly keeps the error check free of
            // equivalent mutants, as the old `wait_pid` did.
            if ret == -1 {
                let err = io::Error::last_os_error();
                if is_retryable_poll_error(err.kind()) {
                    continue; // spurious wakeup: poll again
                }
                return Err(Error::Process {
                    operation: "poll",
                    source: err,
                });
            }
            if ret == 0 {
                // Timeout: the deadline passed with the child alive.
                return Ok(None);
            }
            // Readable: the child exited. Reap via the pidfd — a
            // spurious wakeup would report None and the deadline path
            // below treats it as timed out, but pidfd POLLIN is
            // edge-reliable so this does not happen.
            return self.try_wait();
        }
    }
}

impl Drop for Child {
    /// Kill-on-drop: a `Child` that was never reaped must not outlive
    /// its owner as a live orphan. Sends `SIGKILL` via the pidfd, then
    /// reaps — bounded, never blocking unboundedly. Best-effort and
    /// infallible by construction: `Drop` must never hang or panic.
    fn drop(&mut self) {
        let _ = self.terminate();
        let _ = self.poll_once(DROP_REAP_GRACE);
    }
}

/// Encodes a `waitid` status in the classic `waitpid` status-int layout
/// so [`child_exit_code`] keeps a single interpretation: a normal exit
/// puts the code in bits 8..15, signal death puts the signal number in
/// the low 7 bits (plus the core-dump bit, as `waitpid` sets it).
fn raw_status(status: &WaitidStatus) -> i32 {
    if let Some(code) = status.exit_status() {
        // The kernel reports 8-bit exit codes, so the shift cannot
        // overflow; the try_from keeps this total (no panics here).
        return i32::try_from(code).unwrap_or(0) << 8;
    }
    if let Some(sig) = status.terminating_signal() {
        let dumped = if status.dumped() { 0x80 } else { 0 };
        return i32::try_from(sig).unwrap_or(0) | dumped;
    }
    // Unreachable with WEXITED (only exited/killed/dumped children are
    // reported); 0 keeps this total rather than panicking.
    0
}

/// Opens a pidfd for the calling process.
///
/// A supervisor opens this on itself *before* forking; the child inherits
/// the fd across the fork and uses it with [`set_pdeathsig`] to verify the
/// supervisor is still alive — pid-free, so the check works across pid
/// namespaces where numeric pids are meaningless.
///
/// # Errors
///
/// Returns [`Error::Process`] if `pidfd_open` fails.
pub fn self_pidfd() -> Result<OwnedFd, Error> {
    // SAFETY: getpid takes no arguments and always succeeds; the result
    // is always positive, so the None arm is defensive.
    let pid = Pid::from_raw(unsafe { libc::getpid() }).ok_or_else(|| Error::Process {
        operation: "pidfd_open",
        source: io::Error::new(io::ErrorKind::InvalidInput, "getpid returned non-positive"),
    })?;
    rustix::process::pidfd_open(pid, PidfdFlags::empty()).map_err(|source| Error::Process {
        operation: "pidfd_open",
        source: io::Error::from_raw_os_error(source.raw_os_error()),
    })
}

/// Arranges for the kernel to deliver `SIGKILL` to the caller when its
/// parent thread dies, and verifies the parent is still alive via pidfd.
///
/// A fork child calls this immediately after `fork` (before any
/// `exec`): if the supervisor dies, the child dies with it instead of
/// becoming an orphan. `parent` is a pidfd the supervisor opened on
/// itself via [`self_pidfd`] *before* forking (inherited across the
/// fork); after the `prctl`, the pidfd is polled with a zero timeout —
/// readable means the supervisor already exited, i.e. the caller was
/// orphaned between `fork` and arming, so the caller must exit rather
/// than continue unsupervised.
///
/// Pidfds name the task, not the numeric pid, so unlike a `getppid()`
/// comparison this works across pid namespaces: pid 1 of a new pid
/// namespace always sees `getppid() == 0`, which a pid comparison
/// misreads as "parent died" and kills every cell before `exec`.
///
/// # Errors
///
/// Returns [`Error::Process`] if the `prctl` fails, the poll fails, or
/// the parent is already gone: the caller must then exit, not continue.
pub fn set_pdeathsig(parent: BorrowedFd<'_>) -> Result<(), Error> {
    // SAFETY: PR_SET_PDEATHSIG with SIGKILL only affects the caller;
    // the remaining args are unused.
    let ret = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) };
    if ret != 0 {
        return Err(Error::Process {
            operation: "prctl(PDEATHSIG)",
            source: io::Error::last_os_error(),
        });
    }
    // Zero-timeout liveness poll: a pidfd becomes readable (POLLIN) when
    // its process terminates. This closes the race where the supervisor
    // dies between `fork` and the `prctl` above — `PR_SET_PDEATHSIG` only
    // fires on *future* parent death, so an already-dead supervisor would
    // otherwise leave the child unsupervised forever.
    //
    // Fail-closed on any nonzero return: with nfds=1, `poll` returns 1
    // exactly when the fd reports an event — POLLIN (the supervisor
    // exited) or POLLERR/POLLNVAL (our view of the supervisor is
    // broken) — and -1 on error. A zero return is the only
    // "supervisor demonstrably alive" outcome, so the return value
    // alone decides; no `revents` check is needed (and none is left
    // for mutants to hide behind: the single `!=` is pinned by the
    // live-parent test below).
    let mut pfd = libc::pollfd {
        fd: parent.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is a valid one-element pollfd array; a zero timeout
    // never blocks.
    let ret = unsafe { libc::poll(&raw mut pfd, 1, 0) };
    if ret != 0 {
        return Err(Error::Process {
            operation: "pdeathsig parent check",
            source: io::Error::other("supervisor pidfd not demonstrably alive"),
        });
    }
    Ok(())
}

/// Sets `NoNewPrivs` on the current process.
///
/// After this, `execve` can never gain new privileges — the cell's
/// agent binary runs with no more privilege than the spawner had at
/// this point, whatever its file mode bits claim.
///
/// Returns `Ok(true)` if the flag was newly set by this call,
/// `Ok(false)` if it was already set (the call is idempotent).
///
/// # Errors
///
/// Returns [`Error::Process`] if the `prctl` fails.
pub fn set_no_new_privs() -> Result<bool, Error> {
    // SAFETY: PR_GET_NO_NEW_PRIVS takes no args and always succeeds.
    let before = unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) };
    // SAFETY: prctl with PR_SET_NO_NEW_PRIVS and arg 1 is a pure
    // flag-set on the calling process; the remaining args are unused.
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        return Err(Error::Process {
            operation: "prctl(NO_NEW_PRIVS)",
            source: io::Error::last_os_error(),
        });
    }
    Ok(before != 1)
}

/// Replaces the process image.
///
/// `path` is the in-cell absolute path of the binary, `argv` its
/// argument vector (by convention `argv[0]` names the binary), and `env`
/// the explicit environment — nothing is inherited implicitly.
///
/// `last` runs just before `execve`, after the last allocation:
/// [`hold_to`] an address space smaller than the one this process was
/// forked with would leave it nothing to allocate with.
///
/// This function never returns successfully: it yields the error when
/// `execve` fails.
///
/// # Errors
///
/// Returns [`Error::InvalidExecArg`] if an argument contains an interior
/// NUL, `last`'s own error without the `execve`, or [`Error::Process`] if
/// `execve` itself fails.
#[must_use]
pub fn exec(
    path: &std::path::Path,
    argv: &[&str],
    env: &[(&str, &str)],
    last: impl FnOnce() -> Result<(), Error>,
) -> Error {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return Error::InvalidExecArg {
            reason: String::from("binary path contains an interior NUL"),
        };
    };
    let mut c_argv: Vec<CString> = Vec::with_capacity(argv.len());
    for a in argv {
        let Ok(s) = CString::new(*a) else {
            return Error::InvalidExecArg {
                reason: String::from("argv entry contains an interior NUL"),
            };
        };
        c_argv.push(s);
    }
    let mut c_env: Vec<CString> = Vec::with_capacity(env.len());
    for (k, v) in env {
        match CString::new(format!("{k}={v}")) {
            Ok(s) => c_env.push(s),
            Err(_) => {
                return Error::InvalidExecArg {
                    reason: String::from("env entry contains an interior NUL"),
                };
            }
        }
    }
    let argv_ptrs: Vec<*const libc::c_char> = c_argv
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let env_ptrs: Vec<*const libc::c_char> = c_env
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    if let Err(error) = last() {
        return error;
    }
    // SAFETY: the pointer arrays are NUL-terminated, borrow live
    // CStrings for the duration of the call, and execve only returns on
    // error. argv/env aliasing rules for execve are satisfied.
    unsafe {
        libc::execve(
            c_path.as_ptr(),
            argv_ptrs.as_ptr().cast_mut(),
            env_ptrs.as_ptr().cast_mut(),
        );
    }
    Error::Process {
        operation: "execve",
        source: io::Error::last_os_error(),
    }
}

/// The effective uid of the current process.
///
/// The broker uses this to pick the default id-map base: an
/// unprivileged broker can only map its own uid.
#[must_use]
pub fn euid() -> u32 {
    // SAFETY: `geteuid` takes no arguments and always succeeds.
    unsafe { libc::geteuid() }
}

/// Whether a process with pid `pid` exists: one this process may not
/// signal counts. A pid of 0 or below names no one process.
#[must_use]
pub fn pid_alive(pid: i32) -> bool {
    Pid::from_raw(pid).is_some_and(|pid| {
        matches!(
            rustix::process::test_kill_process(pid),
            Ok(()) | Err(rustix::io::Errno::PERM)
        )
    })
}

/// Drops every supplementary group: `setgroups(0, NULL)`.
///
/// A root broker's cell would otherwise carry the broker's groups into
/// its user namespace, where, once `setgroups` is denied there, it can't
/// drop them. It needs `CAP_SETGID`, so a cell calls it while it is
/// still the broker's root child, before `unshare(CLONE_NEWUSER)`.
///
/// # Errors
///
/// Returns [`Error::Process`] if the call fails, as it does without
/// `CAP_SETGID`.
pub fn clear_groups() -> Result<(), Error> {
    // SAFETY: a count of zero with a null list is `setgroups`' "drop
    // every supplementary group"; nothing is read through the pointer.
    let ret = unsafe { libc::setgroups(0, std::ptr::null()) };
    if ret != 0 {
        return Err(Error::Process {
            operation: "setgroups(0)",
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

/// Makes this process not dumpable.
///
/// No core dump, and its `/proc` files, `/proc/<pid>/mem` among them,
/// owned by root and closed to any process without `CAP_SYS_PTRACE`. A
/// cell's init shares its uid with the program it runs; this keeps the
/// program out of the init's memory.
///
/// # Errors
///
/// Returns [`Error::Process`] if the `prctl` fails.
pub fn set_not_dumpable() -> Result<(), Error> {
    prctl_checked(libc::PR_SET_DUMPABLE, [0, 0, 0, 0], "prctl(SET_DUMPABLE)")
}

/// Holds this process, and every process it starts, to a core-file size
/// of exactly one byte, soft and hard: a crash leaves the host no core.
///
/// One byte, not zero. A host whose `core_pattern` pipes cores to a crash
/// handler (apport, systemd-coredump) never applies the limit to the
/// pipe, except that the kernel aborts the dump when it is exactly one;
/// and a core file smaller than a page is never written. A cell can't
/// lift it: raising a hard limit takes `CAP_SYS_RESOURCE` in the host's
/// user namespace, and the cell's seccomp filter refuses any change to it.
///
/// # Errors
///
/// [`Error::Process`] if the kernel refuses: a hard limit of 0 inherited
/// from the host can't be raised to 1 without `CAP_SYS_RESOURCE`.
pub fn hold_core_dumps_off() -> Result<(), Error> {
    rustix::process::setrlimit(
        rustix::process::Resource::Core,
        rustix::process::Rlimit {
            current: Some(1),
            maximum: Some(1),
        },
    )
    .map_err(|source| Error::Process {
        operation: "setrlimit(RLIMIT_CORE)",
        source: source.into(),
    })
}

/// A limit the kernel holds a process, and every process it starts, to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// `RLIMIT_NPROC`: how many tasks, threads included, its user may
    /// hold in its user namespace.
    Processes,
    /// `RLIMIT_AS`: its address space, in bytes.
    AddressSpace,
    /// `RLIMIT_FSIZE`: the largest file it may write, in bytes.
    FileSize,
    /// `RLIMIT_NOFILE`: one more than the highest descriptor it may open.
    OpenFiles,
}

impl Limit {
    const fn resource(self) -> rustix::process::Resource {
        match self {
            Self::Processes => rustix::process::Resource::Nproc,
            Self::AddressSpace => rustix::process::Resource::As,
            Self::FileSize => rustix::process::Resource::Fsize,
            Self::OpenFiles => rustix::process::Resource::Nofile,
        }
    }
}

/// Holds this process to `value` for `limit`.
///
/// `value` becomes both its soft and its hard limit, or the hard limit
/// it already holds does, if that is lower. Raising a hard limit takes `CAP_SYS_RESOURCE` in the initial user
/// namespace, which no cell holds: a cell can't lift it.
///
/// # Errors
///
/// Returns [`Error::Process`] if the kernel refuses the limit.
pub fn hold_to(limit: Limit, value: u64) -> Result<(), Error> {
    let resource = limit.resource();
    let value = rustix::process::getrlimit(resource)
        .maximum
        .map_or(value, |held| held.min(value));
    rustix::process::setrlimit(
        resource,
        rustix::process::Rlimit {
            current: Some(value),
            maximum: Some(value),
        },
    )
    .map_err(|source| Error::Process {
        operation: "setrlimit",
        source: source.into(),
    })
}

/// Waits as a pid namespace's init until `program` exits, and returns its
/// exit code (see [`child_exit_code`]); 1 if the wait itself fails.
///
/// Every child that exits on the way is reaped, the orphans reparented
/// here among them. When the init exits, the kernel kills whatever is
/// left in its namespace, so nothing the program started outlives it.
#[must_use]
pub fn reap_as_init(program: &Child) -> i32 {
    loop {
        match rustix::process::wait(rustix::process::WaitOptions::empty()) {
            Ok(Some((pid, status))) => {
                if pid.as_raw_nonzero().get() == program.pid.get() {
                    program.reaped.set(true);
                    return child_exit_code(i32::try_from(status.as_raw()).unwrap_or(1));
                }
            }
            Err(rustix::io::Errno::INTR) => {}
            Ok(None) | Err(_) => return 1,
        }
    }
}

/// Exits the current process immediately without running destructors.
///
/// For fork children that must not touch parent state (no `Drop`, no
/// flushing, no TLS teardown): the only sound exit after a failed
/// spawn step.
pub fn exit_child(code: i32) -> ! {
    // SAFETY: `_exit` terminates the process; it diverges.
    unsafe { libc::_exit(code) }
}

/// Converts a raw `waitpid` status to an exit code: the child's exit
/// code, or `128 + signal` when it died from a signal, or `1` when the
/// status is neither (stopped/continued — never expected here).
#[must_use]
pub const fn child_exit_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

/// Drops to the cell's mapped user/group after the parent writes the
/// uid/gid maps.
///
/// In a fresh user namespace the child starts as the overflow uid
/// (its host uid is not yet mapped). After the parent writes the
/// `0 <base> 1` maps, `setresuid(0, 0, 0)` / `setresgid(0, 0, 0)`
/// makes the child uid/gid 0 *inside* the namespace — which the
/// kernel reports as the mapped host uid/gid outside. This must run
/// before any mount or filesystem setup so the cell root is owned by
/// the cell user.
///
/// # Errors
///
/// Returns [`Error::Process`] if either `setresuid` or `setresgid`
/// fails.
pub fn become_cell_user() -> Result<(), Error> {
    // SAFETY: setresgid to 0 in a user namespace where the caller
    // holds CAP_SETGID (granted to the namespace creator) is sound;
    // failure is reported, never assumed.
    let ret = unsafe { libc::setresgid(0, 0, 0) };
    if ret != 0 {
        return Err(Error::Process {
            operation: "setresgid(0,0,0)",
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: setresuid to 0 in a user namespace where the caller
    // holds CAP_SETUID (granted to the namespace creator) is sound;
    // failure is reported, never assumed.
    let ret = unsafe { libc::setresuid(0, 0, 0) };
    if ret != 0 {
        return Err(Error::Process {
            operation: "setresuid(0,0,0)",
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

/// The highest capability number the bounding-set scan tries. The
/// kernel's own last capability is lower; `PR_CAPBSET_DROP` past it
/// answers `EINVAL`, which ends the scan.
const CAP_SCAN_MAX: libc::c_ulong = 63;

/// `_LINUX_CAPABILITY_VERSION_3`: two 32-bit words per set.
const CAPABILITY_VERSION_3: u32 = 0x2008_0522;

/// The `capset`/`capget` header.
#[repr(C)]
struct CapHeader {
    version: u32,
    pid: libc::c_int,
}

/// One 32-bit word of each capability set.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

/// The securebits a cell runs under, each locked: uid 0 is never
/// special (`NOROOT`), a uid change never adjusts capabilities
/// (`NO_SETUID_FIXUP`), capabilities are never kept across one
/// (`KEEP_CAPS` off), and none can be raised into the ambient set.
///
/// Written as its value: built with `|`, each `|` -> `^` would be the
/// same number (the bits are disjoint). `NOROOT` 0x01 and its lock 0x02,
/// `NO_SETUID_FIXUP` 0x04 and its lock 0x08, the `KEEP_CAPS` lock 0x20,
/// `NO_CAP_AMBIENT_RAISE` 0x40 and its lock 0x80. The test of
/// `prctl_checked` rebuilds it from the kernel's names.
const CELL_SECUREBITS: libc::c_int = 0xef;

/// A `prctl` with explicit `unsigned long` arguments (a variadic `int`
/// would leave the upper half of each register unset).
fn prctl_checked(
    option: libc::c_int,
    values: [libc::c_ulong; 4],
    operation: &'static str,
) -> Result<(), Error> {
    let [arg2, arg3, arg4, arg5] = values;
    // SAFETY: every option this module passes takes integer arguments
    // only and changes nothing but the calling thread's credentials.
    let ret = unsafe { libc::prctl(option, arg2, arg3, arg4, arg5) };
    if ret == 0 {
        Ok(())
    } else {
        Err(Error::Process {
            operation,
            source: io::Error::last_os_error(),
        })
    }
}

/// `value` as a `prctl` argument.
fn prctl_arg(value: libc::c_int, operation: &'static str) -> Result<libc::c_ulong, Error> {
    libc::c_ulong::try_from(value).map_err(|_| Error::Process {
        operation,
        source: io::Error::from(io::ErrorKind::InvalidInput),
    })
}

/// Drops every capability the process holds, for good.
///
/// Inside its own user namespace a cell is uid 0, and uid 0 there holds
/// every capability the namespace grants. This takes them all away, so
/// capabilities stop at the boundary: the bounding set is emptied (no
/// `execve` can gain one back), the ambient set cleared, the securebits
/// locked so uid 0 is never special again, and the effective, permitted
/// and inheritable sets emptied.
///
/// Call it after [`become_cell_user`], once the privileged mount and
/// namespace work is done, and before seccomp, which allows none of
/// these calls.
///
/// # Errors
///
/// Returns [`Error::Process`] if any step fails.
pub fn drop_capabilities() -> Result<(), Error> {
    for cap in 0..=CAP_SCAN_MAX {
        match prctl_checked(libc::PR_CAPBSET_DROP, [cap, 0, 0, 0], "prctl(CAPBSET_DROP)") {
            Ok(()) => {}
            // Past the kernel's last capability: the bounding set is empty.
            Err(Error::Process { source, .. }) if source.raw_os_error() == Some(libc::EINVAL) => {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    let clear_all = prctl_arg(libc::PR_CAP_AMBIENT_CLEAR_ALL, "prctl(CAP_AMBIENT)")?;
    prctl_checked(
        libc::PR_CAP_AMBIENT,
        [clear_all, 0, 0, 0],
        "prctl(CAP_AMBIENT)",
    )?;
    let bits = prctl_arg(CELL_SECUREBITS, "prctl(SET_SECUREBITS)")?;
    prctl_checked(
        libc::PR_SET_SECUREBITS,
        [bits, 0, 0, 0],
        "prctl(SET_SECUREBITS)",
    )?;
    let header = CapHeader {
        version: CAPABILITY_VERSION_3,
        pid: 0,
    };
    let data = [CapData::default(); 2];
    // SAFETY: capset reads one header and two data words, both valid
    // for the call's duration; it changes only this thread's sets.
    let ret =
        unsafe { libc::syscall(libc::SYS_capset, std::ptr::from_ref(&header), data.as_ptr()) };
    if ret != 0 {
        return Err(Error::Process {
            operation: "capset",
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

/// The six arguments [`raw_syscall_inert`] passes: all `-1` as full
/// `c_long`s, the set its measurement covers.
///
/// Typed on purpose: untyped `-1` literals through the variadic
/// `libc::syscall` are `i32`s, which arrive in the kernel as
/// `0x0000_0000_ffff_ffff` in every 64-bit argument register, not `-1`.
const INERT_ARGS: [libc::c_long; 6] = [-1; 6];

/// `pthread_atfork`'s result for [`arm_pdeathsig_in_child`], computed
/// once per process by the first [`fork`].
static ATFORK_REGISTERED: std::sync::OnceLock<libc::c_int> = std::sync::OnceLock::new();

/// `pthread_atfork` child handler: arms `PR_SET_PDEATHSIG(SIGKILL)` in
/// every child the moment `fork` creates it, before [`fork`] returns.
///
/// Defense in depth for the window, and the code, between `fork` and a
/// caller's own [`set_pdeathsig`]: whatever the child runs next, even if
/// the code deciding which side of the fork it is on is wrong, it dies
/// with the thread that forked it. Found by mutation testing: a `fork`
/// mutant that told the child "fork failed" sent a copy of the
/// multi-threaded test runner back into libtest, where it deadlocked on
/// an inherited lock and outlived the run. Callers that arm
/// `set_pdeathsig` still must: it adds the liveness check this cannot.
///
/// # Safety
///
/// Called by libc in the child of a `fork`, where only async-signal-safe
/// operations are sound; it makes one raw syscall.
unsafe extern "C" fn arm_pdeathsig_in_child() {
    // SAFETY: PR_SET_PDEATHSIG with SIGKILL only affects the caller; a
    // raw syscall, async-signal-safe as a fork child handler must be.
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) };
}

/// Issue a raw syscall with all six arguments `-1`.
///
/// This is a test-only primitive for the adversarial suite: it
/// proves the cell's seccomp filter kills denied syscalls. It must
/// never be used by the runtime itself to bypass safe wrappers.
///
/// # Under a working filter: nothing runs
///
/// Seccomp evaluates the filter at syscall entry, before the kernel
/// dispatches the call, so a denied number is `SIGSYS`-killed
/// (`SECCOMP_RET_KILL_PROCESS`) without executing, whatever its
/// arguments. That is the only context the suite calls this in.
///
/// # Without one: `-1` is not inert for every number
///
/// Measured (kernel 6.6.87.2) with no filter, one throwaway
/// user/pid/ipc/net/uts/mount namespace per call, over all 346 denied
/// `x86_64` numbers in the table: 320 fail with no side effect
/// (`EINVAL`, `EFAULT`, `EBADF`, `ENOSYS`, `EPERM`, ...; the 12 newest
/// numbers are `ENOSYS` on that kernel, so newer kernels need a
/// re-measure). The other 26 run, because they take no arguments or
/// read `-1` as a valid value:
///
/// * `fork`/`vfork` create a child (`vfork` then shares the caller's
///   stack: undefined behaviour, contained to the caller);
/// * `pause` blocks until a signal arrives;
/// * `sync` flushes every filesystem (host-wide, harmless);
/// * `msgget(-1, -1)` creates a mode-0777 System V message queue with
///   key `0xffffffff` in the caller's IPC namespace, which outlives
///   the caller;
/// * `umask`, `alarm`, `setsid`, `eventfd`, `inotify_init`,
///   `munlockall`, `flock` (the deprecated `LOCK_MAND` no-op), the
///   `setre*id`/`setres*id`/`setfs*id` no-change forms, `personality`
///   (query form), `sched_yield` and the id getters touch nothing
///   outside the caller.
///
/// None is destructive or privilege-changing. The list is why the
/// suite only calls this as PID 1 inside a cell (the probe refuses
/// otherwise), and why the denied-catalog driver fails fast: under a
/// filter broken outright the first survivor is `open` (nr 2), and
/// nothing in the list above is ever reached.
///
/// # Safety contract
///
/// The `unsafe` is contained here because `libc::syscall` is variadic
/// and cannot be made safe by the type system; no pointers are passed.
#[must_use]
pub fn raw_syscall_inert(nr: u32) -> i64 {
    let [a0, a1, a2, a3, a4, a5] = INERT_ARGS;
    // SAFETY: `libc::syscall` with a syscall number and six `c_long`
    // arguments (see above for what they do); the return value is a
    // plain integer. No pointers are passed, so there is no aliasing
    // or lifetime concern.
    let ret = unsafe { libc::syscall(libc::c_long::from(nr), a0, a1, a2, a3, a4, a5) };
    // `c_long` is `i64` on 64-bit (identity) and `i32` on 32-bit
    // (widening); the conversion is portable.
    #[allow(
        clippy::useless_conversion,
        reason = "identity on 64-bit, widening on 32-bit"
    )]
    i64::from(ret)
}

/// The real uid and effective gid the kernel recorded when this
/// process image was exec'd, read from the auxiliary vector
/// (`AT_UID`, `AT_EGID`) without a syscall.
///
/// For code that runs under a seccomp filter denying `getuid` and
/// `getegid` (the probe inside a cell): the kernel writes the
/// auxiliary vector at `execve`, translated into the process's own
/// user namespace, so the values are the ids the process started
/// with. Nothing in the probe changes its ids after exec.
#[must_use]
pub fn exec_uid_egid() -> (u32, u32) {
    // SAFETY: `getauxval` reads the process's auxiliary vector: it takes
    // an integer key and returns an integer (0 if absent — `AT_UID` and
    // `AT_EGID` are always present for an ELF exec on Linux).
    let uid = unsafe { libc::getauxval(libc::AT_UID) };
    // SAFETY: as above, for `AT_EGID`.
    let egid = unsafe { libc::getauxval(libc::AT_EGID) };
    (
        u32::try_from(uid).unwrap_or(u32::MAX),
        u32::try_from(egid).unwrap_or(u32::MAX),
    )
}

#[cfg(test)]
mod tests {
    /// How long a test waits to reap a child it killed: a `SIGKILL`ed
    /// child is reaped in milliseconds, so a child that was never killed
    /// fails the wait quickly instead of holding the run.
    const REAP: Duration = Duration::from_secs(5);

    #[test]
    fn drop_capabilities_reports_the_bounding_set_step_it_may_not_take() {
        // Unprivileged, outside a user namespace, emptying the bounding
        // set needs CAP_SETPCAP: the first step is refused with EPERM, and
        // the call fails there, naming it, rather than taking the refusal
        // for the end of the set (EINVAL). As root it would strip this test
        // process.
        if euid() == 0 {
            return;
        }
        let refused = drop_capabilities();
        assert!(
            matches!(
                refused,
                Err(Error::Process {
                    operation: "prctl(CAPBSET_DROP)",
                    ..
                })
            ),
            "{refused:?}"
        );
    }

    #[test]
    fn set_not_dumpable_makes_the_process_not_dumpable() {
        // In a child: not dumpable, this test process's /proc files would
        // belong to root.
        match fork().expect("fork") {
            ForkResult::Parent { child } => {
                let status = child.wait_deadline(REAP).expect("child");
                assert_eq!(
                    child_exit_code(status),
                    0,
                    "1: the call failed; 2: still dumpable"
                );
            }
            ForkResult::Child => {
                if set_not_dumpable().is_err() {
                    exit_child(1);
                }
                // SAFETY: PR_GET_DUMPABLE takes no further arguments and
                // only reads this process's flag.
                let dumpable = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
                exit_child(if dumpable == 0 { 0 } else { 2 });
            }
        }
    }

    #[test]
    fn hold_to_sets_both_limits_and_never_raises_the_hard_one() {
        // In a child: the limits it sets are its own.
        match fork().expect("fork") {
            ForkResult::Parent { child } => {
                let status = child.wait_deadline(REAP).expect("child");
                assert_eq!(
                    child_exit_code(status),
                    0,
                    "1: a call failed; 2: a limit wasn't held"
                );
            }
            ForkResult::Child => {
                // Unlimited before (the address space and file size) or
                // above the value: both limits set to it. A value above the
                // hard limit (open files, the second time): held where it was.
                let calls = [
                    (Limit::Processes, 100),
                    (Limit::AddressSpace, 1 << 40),
                    (Limit::FileSize, 1 << 30),
                    (Limit::OpenFiles, 64),
                    (Limit::OpenFiles, 128),
                ];
                if calls
                    .iter()
                    .any(|&(limit, value)| hold_to(limit, value).is_err())
                {
                    exit_child(1);
                }
                let held = [
                    (Limit::Processes, 100),
                    (Limit::AddressSpace, 1 << 40),
                    (Limit::FileSize, 1 << 30),
                    (Limit::OpenFiles, 64),
                ]
                .iter()
                .all(|&(limit, value)| {
                    let held = rustix::process::getrlimit(limit.resource());
                    (held.current, held.maximum) == (Some(value), Some(value))
                });
                exit_child(if held { 0 } else { 2 });
            }
        }
    }

    #[test]
    fn exec_runs_its_last_step_and_answers_with_its_error() {
        // In a child: an exec that ran would end it with false's 1.
        match fork().expect("fork") {
            ForkResult::Parent { child } => {
                let status = child.wait_deadline(REAP).expect("child");
                assert_eq!(
                    child_exit_code(status),
                    0,
                    "1: exec ran past its failed last step; 2: another error"
                );
            }
            ForkResult::Child => {
                let refused = exec(std::path::Path::new("/bin/false"), &["false"], &[], || {
                    Err(Error::InvalidExecArg {
                        reason: String::from("last"),
                    })
                });
                let answered = matches!(
                    &refused,
                    Error::InvalidExecArg { reason } if reason == "last"
                );
                exit_child(if answered { 0 } else { 2 });
            }
        }
        let ran = Cell::new(false);
        let missing = exec(
            std::path::Path::new("/nonexistent/hatchling-exec"),
            &["missing"],
            &[("KEY", "value")],
            || {
                ran.set(true);
                Ok(())
            },
        );
        assert!(ran.get());
        assert!(matches!(
            missing,
            Error::Process {
                operation: "execve",
                ..
            }
        ));
    }

    #[test]
    fn reap_as_init_returns_its_programs_code_and_reaps_the_others() {
        // In a child of its own: a wait for any child must take none of
        // this test process's.
        match fork().expect("fork helper") {
            ForkResult::Parent { child } => {
                let status = child
                    .wait_deadline(Duration::from_secs(10))
                    .expect("helper");
                assert_eq!(
                    child_exit_code(status),
                    0,
                    "1: a fork failed; 2: not the program's code; 3: a child left unreaped"
                );
            }
            ForkResult::Child => {
                // A child that exits at once, then the program, which exits
                // 7 a moment later: the first is reaped on the way.
                let Ok(ForkResult::Parent { child: _early }) =
                    fork().map_err(|_| ()).and_then(|forked| match forked {
                        ForkResult::Child => exit_child(3),
                        parent @ ForkResult::Parent { .. } => Ok(parent),
                    })
                else {
                    exit_child(1);
                };
                let Ok(ForkResult::Parent { child: program }) =
                    fork().map_err(|_| ()).and_then(|forked| match forked {
                        ForkResult::Child => {
                            std::thread::sleep(Duration::from_millis(200));
                            exit_child(7)
                        }
                        parent @ ForkResult::Parent { .. } => Ok(parent),
                    })
                else {
                    exit_child(1);
                };
                let code = reap_as_init(&program);
                let left = rustix::process::wait(rustix::process::WaitOptions::NOHANG);
                exit_child(if code != 7 {
                    2
                } else if matches!(left, Err(rustix::io::Errno::CHILD)) {
                    0
                } else {
                    3
                });
            }
        }
    }

    #[test]
    fn clear_groups_needs_the_right_to_set_groups() {
        // As root it would drop this test process's groups; that path is
        // the root cell test's. Unprivileged, the kernel refuses it.
        if euid() == 0 {
            return;
        }
        let refused = clear_groups();
        assert!(
            matches!(
                refused,
                Err(Error::Process {
                    operation: "setgroups(0)",
                    ..
                })
            ),
            "{refused:?}"
        );
    }

    /// The calling thread's effective, permitted and inheritable sets,
    /// each as one 64-bit mask.
    fn capabilities() -> [u64; 3] {
        let mut header = CapHeader {
            version: CAPABILITY_VERSION_3,
            pid: 0,
        };
        let mut data = [CapData::default(); 2];
        // SAFETY: capget writes the header and two data words, both
        // valid and writable for the call's duration.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_capget,
                std::ptr::from_mut(&mut header),
                data.as_mut_ptr(),
            )
        };
        assert_eq!(ret, 0, "capget");
        let [low, high] = data;
        let join = |low: u32, high: u32| (u64::from(high) << 32) | u64::from(low);
        [
            join(low.effective, high.effective),
            join(low.permitted, high.permitted),
            join(low.inheritable, high.inheritable),
        ]
    }

    /// Whether `cap` is still in the bounding set.
    fn in_bounding_set(cap: libc::c_ulong) -> bool {
        // SAFETY: PR_CAPBSET_READ only reads this thread's bounding set.
        unsafe { libc::prctl(libc::PR_CAPBSET_READ, cap, 0_u64, 0_u64, 0_u64) == 1 }
    }

    /// The two helpers under `drop_capabilities`: a `prctl` that fails says
    /// which, and a negative argument is refused before any call.
    #[test]
    fn a_prctl_reports_its_own_failure_and_takes_no_negative_argument() {
        prctl_checked(libc::PR_SET_KEEPCAPS, [0, 0, 0, 0], "prctl(SET_KEEPCAPS)").unwrap();
        let Err(Error::Process { operation, source }) =
            prctl_checked(9999, [0, 0, 0, 0], "prctl(9999)")
        else {
            unreachable!("an unknown option must fail");
        };
        assert_eq!(operation, "prctl(9999)");
        assert_eq!(source.raw_os_error(), Some(libc::EINVAL));
        assert_eq!(prctl_arg(4, "x").unwrap(), 4);
        assert!(matches!(
            prctl_arg(-1, "x"),
            Err(Error::Process { operation: "x", .. })
        ));
        assert_eq!(CAPABILITY_VERSION_3, 0x2008_0522);
        assert_eq!(CAP_SCAN_MAX, 63);
        let named = [
            libc::SECBIT_NOROOT,
            libc::SECBIT_NOROOT_LOCKED,
            libc::SECBIT_NO_SETUID_FIXUP,
            libc::SECBIT_NO_SETUID_FIXUP_LOCKED,
            libc::SECBIT_KEEP_CAPS_LOCKED,
            libc::SECBIT_NO_CAP_AMBIENT_RAISE,
            libc::SECBIT_NO_CAP_AMBIENT_RAISE_LOCKED,
        ];
        assert_eq!(named.iter().fold(0, |all, &bit| all | bit), CELL_SECUREBITS);
        assert_eq!(
            named.iter().sum::<libc::c_int>(),
            CELL_SECUREBITS,
            "disjoint bits"
        );
        assert_eq!(
            CELL_SECUREBITS & libc::SECBIT_KEEP_CAPS,
            0,
            "KEEP_CAPS stays off"
        );
    }

    ///, on a real kernel and with no seccomp in the way: in a fresh
    /// user namespace the process holds every capability there (it can
    /// make a mount namespace, which needs `CAP_SYS_ADMIN`); after
    /// `drop_capabilities` it holds none, the bounding set is empty, the
    /// securebits are locked, and the same call fails with `EPERM`, for
    /// lack of the capability and nothing else.
    #[test]
    fn a_dropped_cell_holds_no_capability_even_as_namespace_root() {
        const SKIP: i32 = 99;
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                if crate::unshare_user().is_err() {
                    exit_child(SKIP);
                }
                let [effective, ..] = capabilities();
                let cap_sys_admin = u64::from(1_u32 << 21);
                // A user namespace that grants no capabilities (Ubuntu's
                // AppArmor restriction lets unshare succeed and withholds
                // them) can't host a cell: skip, as the other cell tests do.
                if effective & cap_sys_admin == 0 || !in_bounding_set(21) {
                    exit_child(SKIP);
                }
                if drop_capabilities().is_err() {
                    exit_child(11);
                }
                if capabilities() != [0, 0, 0] {
                    exit_child(12);
                }
                if (0..=CAP_SCAN_MAX).any(in_bounding_set) {
                    exit_child(13);
                }
                // SAFETY: PR_GET_SECUREBITS only reads them.
                let bits =
                    unsafe { libc::prctl(libc::PR_GET_SECUREBITS, 0_u64, 0_u64, 0_u64, 0_u64) };
                if bits != CELL_SECUREBITS {
                    exit_child(14);
                }
                match crate::unshare_mount() {
                    Err(Error::Namespace { source, .. })
                        if source.raw_os_error() == Some(libc::EPERM) =>
                    {
                        exit_child(0)
                    }
                    Ok(()) | Err(_) => exit_child(15),
                }
            }
        };
        let status = child
            .wait_deadline(std::time::Duration::from_secs(30))
            .expect("bounded wait");
        let code = child_exit_code(status);
        if code == SKIP && std::env::var("HATCHLING_REQUIRE_CELL").as_deref() != Ok("1") {
            return;
        }
        assert_eq!(code, 0, "capability drop failed at step {code}");
    }

    use std::io;

    #[test]
    fn only_eintr_is_a_retryable_poll_error() {
        // Pins `is_retryable_poll_error`: `true`/`false` replacements
        // and any inversion of the EINTR check fail one of these.
        use super::is_retryable_poll_error;
        assert!(is_retryable_poll_error(io::ErrorKind::Interrupted));
        for kind in [
            io::ErrorKind::InvalidInput,
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::OutOfMemory,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::Other,
        ] {
            assert!(
                !is_retryable_poll_error(kind),
                "{kind:?} must fail closed, not retry"
            );
        }
    }
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_pid_is_alive_until_its_process_is_reaped() {
        assert!(pid_alive(i32::try_from(std::process::id()).unwrap()));
        assert!(pid_alive(1), "init, which this may not signal");
        assert!(!pid_alive(0) && !pid_alive(-1));
        let child = match fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => exit_child(0),
        };
        let pid = child.pid().get();
        let _ = child.wait_deadline(std::time::Duration::from_secs(10)).unwrap();
        assert!(!pid_alive(pid), "reaped: gone");
    }

    #[test]
    fn core_dumps_are_held_to_one_byte_and_a_zero_hard_limit_stays() {
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        let child = match fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let held = std::panic::catch_unwind(|| {
                    hold_core_dumps_off().unwrap();
                    let core = getrlimit(Resource::Core);
                    assert_eq!((core.current, core.maximum), (Some(1), Some(1)));
                    let zero = Rlimit {
                        current: Some(0),
                        maximum: Some(0),
                    };
                    setrlimit(Resource::Core, zero).unwrap();
                    // Only CAP_SYS_RESOURCE raises a hard limit.
                    assert_eq!(hold_core_dumps_off().is_err(), euid() != 0);
                });
                exit_child(i32::from(held.is_err()));
            }
        };
        let status = child.wait_deadline(Duration::from_secs(5)).unwrap();
        assert_eq!(child_exit_code(status), 0);
    }

    #[test]
    fn a_childs_fd_is_its_open_pidfd() {
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => exit_child(0),
        };
        let fd = child.as_fd().as_raw_fd();
        assert!(fd > 2, "{fd}");
        assert!(crate::fd_flags(fd).is_some(), "the pidfd is open");
        let status = child.wait_deadline(REAP).expect("bounded wait");
        assert_eq!(child_exit_code(status), 0);
    }

    #[test]
    fn every_fork_child_is_born_with_pdeathsig_armed() {
        // Pins `arm_pdeathsig_in_child`: the child's very first act is
        // to read its parent-death signal, which must already be SIGKILL.
        match fork().expect("fork") {
            ForkResult::Parent { child } => {
                let status = child.wait_deadline(REAP).expect("wait");
                assert_eq!(
                    child_exit_code(status),
                    0,
                    "the child was born without PDEATHSIG armed"
                );
            }
            ForkResult::Child => {
                let mut sig: libc::c_int = 0;
                // SAFETY: PR_GET_PDEATHSIG writes one c_int through a
                // valid pointer; a raw syscall, sound in a fork child.
                let ret = unsafe { libc::prctl(libc::PR_GET_PDEATHSIG, &raw mut sig, 0, 0, 0) };
                let armed = ret == 0 && sig == libc::SIGKILL;
                exit_child(i32::from(!armed));
            }
        }
    }

    #[test]
    fn inert_args_are_the_measured_all_minus_one_set() {
        // `raw_syscall_inert`'s docs record what every denied number
        // does with exactly these arguments; this pins them (a `delete -`
        // mutant would silently invalidate that measurement).
        assert_eq!(INERT_ARGS, [-1; 6]);
    }

    #[test]
    fn exec_ids_match_the_live_credentials() {
        // Pins `exec_uid_egid`: the ids the kernel recorded at exec are
        // the live ids (the test process never changes them). Every
        // constant-tuple replacement fails on a non-root runner (the
        // the mutation test box runs as uid/gid 1000).
        assert_eq!(
            exec_uid_egid(),
            (
                rustix::process::getuid().as_raw(),
                rustix::process::getegid().as_raw()
            )
        );
    }

    /// Asserts a child is gone AND reaped: raw `waitpid` must report
    /// ECHILD — no live process, no zombie left behind. Polls briefly
    /// because SIGKILL delivery is asynchronous. If `waitpid` reaps
    /// anything here (`ret > 0`), the code under test left a zombie and
    /// the test fails: `Child`'s kill-on-drop and the deadline path
    /// both reap reliably, so there is no race to excuse it.
    fn assert_reaped(pid: i32) {
        let start = Instant::now();
        loop {
            let mut status = 0;
            // SAFETY: waitpid on our own child; WNOHANG never blocks.
            let ret = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
            if ret == -1 {
                let err = io::Error::last_os_error();
                assert_eq!(
                    err.raw_os_error(),
                    Some(libc::ECHILD),
                    "waitpid({pid}) failed unexpectedly: {err}"
                );
                return;
            }
            assert_eq!(
                ret, 0,
                "waitpid({pid}) reaped a zombie the code under test left behind"
            );
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "child {pid} is still alive"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Forks a child that sleeps until killed. Kill-on-drop on the
    /// returned handle means even a panicking test leaves no orphan.
    fn sleeper() -> Child {
        match fork().expect("fork sleeper") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // Die with the test thread that forked us, however the
                // test ends (test code is never mutated, so this holds
                // even when the kill-on-drop under test is broken or the
                // watchdog abandons a hung call before cleanup runs).
                let _ = rustix::process::set_parent_process_death_signal(Some(Signal::Kill));
                // Backstop: never outlive a test run by minutes.
                std::thread::sleep(Duration::from_secs(120));
                exit_child(0)
            }
        }
    }

    #[test]
    fn fork_distinguishes_parent_and_child() {
        match fork().expect("fork") {
            ForkResult::Parent { child } => {
                assert!(child.pid().get() > 0);
                let status = child.wait_deadline(REAP).expect("wait");
                assert_eq!(child_exit_code(status), 0);
            }
            ForkResult::Child => exit_child(0),
        }
    }

    #[test]
    fn child_pid_rejects_zero_negative_and_self() {
        // Mutants: `>` → `>=` accepts 0; `&&` → `||` accepts 0 and -1;
        // `!=` → `==` accepts self and rejects everything else.
        assert!(ChildPid::new(0).is_none());
        assert!(ChildPid::new(-1).is_none());
        assert!(ChildPid::new(i32::MIN).is_none());
        // SAFETY: getpid takes no arguments and always succeeds.
        let own = unsafe { libc::getpid() };
        assert!(ChildPid::new(own).is_none());
        let valid = ChildPid::new(i32::MAX).expect("i32::MAX is a valid pid shape");
        assert_eq!(valid.get(), i32::MAX);
    }

    #[test]
    fn inverted_fork_branch_cannot_signal_the_process_group() {
        // Issue A acceptance: the `pid == 0` → `pid != 0` mutant
        // handed the fork child the parent branch with pid 0, and the
        // old `terminate(0)` called `kill(0, SIGKILL)` — the whole
        // process group. Drive the mutant's exact input through the new
        // API: the type rejects it at construction, so no signal target
        // can be built — and prove it with a sibling sleeper sharing
        // our process group, which must survive.
        let sibling = sleeper();
        assert!(ChildPid::new(0).is_none(), "pid 0 must not validate");
        assert!(ChildPid::new(-1).is_none(), "pid -1 must not validate");
        assert!(
            sibling.try_wait().expect("try_wait").is_none(),
            "process group must survive the inverted-fork input"
        );
        // Kill-on-drop reaps the sibling.
    }

    #[test]
    fn wait_deadline_returns_real_status() {
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => exit_child(42),
        };
        let status = child.wait_deadline(REAP).expect("wait");
        assert_eq!(child_exit_code(status), 42);
    }

    #[test]
    fn wait_deadline_reports_signal_death() {
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // SAFETY: getpid takes no arguments and always succeeds.
                let me = unsafe { libc::getpid() };
                // SAFETY: signalling ourselves.
                unsafe {
                    libc::kill(me, libc::SIGTERM);
                }
                exit_child(99);
            }
        };
        let status = child.wait_deadline(REAP).expect("wait");
        assert_eq!(child_exit_code(status), 128 + libc::SIGTERM);
    }

    #[test]
    fn wait_deadline_kills_and_reaps_on_timeout() {
        // Fail-closed contract: the overstaying child is SIGKILLed via
        // its pidfd and reaped, and the timeout is reported.
        // Mutants: removed terminate → child alive → assert_reaped
        // fails; removed reap → zombie → assert_reaped fails; Err →
        // Ok → expect_err fails.
        let child = sleeper();
        let pid = child.pid().get();
        let err = child
            .wait_deadline(Duration::from_millis(200))
            .expect_err("overstaying child must time out");
        assert!(
            matches!(err, Error::ProcessTimeout { .. }),
            "expected ProcessTimeout, got {err:?}"
        );
        assert_reaped(pid);
    }

    #[test]
    fn try_wait_is_nonblocking_and_reports_exit() {
        let child = sleeper();
        assert_eq!(child.try_wait().expect("try_wait live"), None);
        child.terminate().expect("terminate");
        // Reap via the deadline path; polling afterwards reports None:
        // the child was already reaped (ECHILD is not an error here).
        let status = child.wait_deadline(REAP).expect("reap");
        assert_eq!(child_exit_code(status), 128 + libc::SIGKILL);
        assert_eq!(child.try_wait().expect("try_wait reaped"), None);
    }

    #[test]
    fn second_wait_deadline_on_reaped_child_is_an_error_not_a_timeout() {
        // A pidfd stays readable after its process is reaped: without
        // reaped-state tracking, a second wait_deadline would see
        // "readable" → waitid → ECHILD → None and misreport the
        // already-reaped child as timed out. The second wait must
        // fail loudly instead.
        // Mutants: removed reaped check → second wait returns
        // ProcessTimeout → the matches! fails; removed flag set in
        // try_wait → same.
        let child = sleeper();
        child.terminate().expect("terminate");
        let status = child.wait_deadline(REAP).expect("reap");
        assert_eq!(child_exit_code(status), 128 + libc::SIGKILL);
        let err = child
            .wait_deadline(REAP)
            .expect_err("second wait on a reaped child must fail");
        assert!(
            !matches!(err, Error::ProcessTimeout { .. }),
            "already-reaped child misreported as timed out: {err:?}"
        );
        assert!(
            matches!(err, Error::Process { .. }),
            "expected Error::Process, got {err:?}"
        );
    }

    #[test]
    fn drop_kills_and_reaps() {
        // Kill-on-drop: even without any explicit wait, dropping the
        // handle leaves no live orphan and no zombie.
        // Mutants: removed terminate → child alive → assert_reaped
        // fails; removed reap → zombie → assert_reaped fails.
        let child = sleeper();
        let pid = child.pid().get();
        drop(child);
        assert_reaped(pid);
    }

    #[test]
    fn terminate_kills_exactly_the_target() {
        // Two sleepers: terminating one leaves the other running — the
        // pidfd pins the process, not the number.
        let a = sleeper();
        let b = sleeper();
        let b_pid = b.pid().get();
        a.terminate().expect("terminate");
        let status = a.wait_deadline(REAP).expect("reap a");
        assert_eq!(child_exit_code(status), 128 + libc::SIGKILL);
        assert_eq!(
            b.try_wait().expect("try_wait b"),
            None,
            "sibling must survive"
        );
        drop(b);
        assert_reaped(b_pid);
    }

    #[test]
    fn poll_retries_after_signal_interruption() {
        // The EINTR branch of poll_once: a signal arriving mid-poll
        // must not become an error or a premature timeout.
        // Mutant `== Interrupted` → `!= Interrupted`: EINTR becomes
        // Err and this test fails.
        //
        // The handler is installed in a fork child (single-threaded —
        // signal-safe to do so) and the signal goes to that child, not
        // to the test process: parallel tests are unaffected.
        #[allow(
            clippy::missing_const_for_fn,
            reason = "extern fns cannot be const; 1.85 clippy suggests it anyway"
        )]
        extern "C" fn noop(_sig: libc::c_int) {}
        let child = match fork().expect("fork A") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // SAFETY: installing a signal handler in a
                // single-threaded fork child before any other work.
                unsafe {
                    // libc::signal takes the handler as sighandler_t, an
                    // integer: converting the fn item is the documented
                    // use, and there is no safe alternative spelling.
                    #[allow(
                        clippy::missing_const_for_fn,
                        reason = "extern fns cannot be const; older clippy suggests it anyway"
                    )]
                    // `function_casts_as_integer` only exists on newer
                    // clippy; the `unknown_lints` allow keeps 1.85.0
                    // accepting the attribute below.
                    #[allow(unknown_lints)]
                    #[allow(
                        clippy::as_conversions,
                        clippy::fn_to_numeric_cast,
                        function_casts_as_integer,
                        reason = "libc::signal takes the handler as an integer; a fn-item-to-integer cast is the only way to install it"
                    )]
                    libc::signal(libc::SIGURG, noop as libc::sighandler_t);
                }
                let grandchild = match fork().expect("fork B") {
                    ForkResult::Parent { child } => child,
                    ForkResult::Child => {
                        // B: interrupt A's poll, then exit with a
                        // distinctive code after A has resumed waiting.
                        std::thread::sleep(Duration::from_millis(100));
                        // SAFETY: getppid always succeeds; kill on our
                        // parent (A).
                        let a_pid = unsafe { libc::getppid() };
                        // SAFETY: signalling our parent.
                        unsafe {
                            libc::kill(a_pid, libc::SIGURG);
                        }
                        std::thread::sleep(Duration::from_secs(2));
                        exit_child(7);
                    }
                };
                let status = grandchild
                    .wait_deadline(REAP)
                    .expect("wait_deadline must survive EINTR");
                if child_exit_code(status) != 7 {
                    exit_child(1);
                }
                exit_child(0);
            }
        };
        let status = child
            .wait_deadline(Duration::from_secs(60))
            .expect("wait A");
        assert_eq!(
            child_exit_code(status),
            0,
            "A's wait must survive EINTR and see B's exit 7"
        );
    }

    #[test]
    fn pdeathsig_arms_when_parent_alive() {
        // Happy path: the parent opens a pidfd on itself before forking;
        // the child arms against it pid-free. The pidfd is inherited
        // across the fork, so no fd number needs communicating.
        // Mutant `&` → `|` / `!=` → `==` on the POLLIN check: Ok becomes
        // Err → the child exits 1.
        let parent_fd = self_pidfd().expect("self pidfd");
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                if set_pdeathsig(parent_fd.as_fd()).is_err() {
                    exit_child(1);
                }
                exit_child(0);
            }
        };
        let status = child.wait_deadline(REAP).expect("wait");
        assert_eq!(child_exit_code(status), 0);
    }

    #[test]
    fn pdeathsig_rejects_dead_parent() {
        // The race check, pid-free and deterministic: a pidfd for an
        // already-exited (and reaped) process must fail the arming — the
        // caller was orphaned before the prctl. Pidfds pin the task
        // struct, so PID reuse cannot confuse this. A live self-pidfd
        // must still succeed, and a closed fd (POLLNVAL) must fail
        // closed like a broken view of the supervisor.
        let dead = match fork().expect("fork short-lived") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => exit_child(0),
        };
        let status = dead.wait_deadline(REAP).expect("reap");
        assert_eq!(child_exit_code(status), 0);
        let probe = match fork().expect("fork probe") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let dead_rejected = set_pdeathsig(dead.pidfd.as_fd()).is_err();
                let live_accepted = self_pidfd().is_ok_and(|me| set_pdeathsig(me.as_fd()).is_ok());
                // A closed fd number: poll reports POLLNVAL, which must
                // fail closed. The probe child is single-threaded with a
                // private fd table, so nothing can reuse the number
                // between close and poll — deterministic.
                let closed_raw = {
                    let fd = self_pidfd().expect("pidfd for close test");
                    let raw = fd.as_raw_fd();
                    drop(fd);
                    raw
                };
                let closed_rejected = {
                    // SAFETY: `closed_raw` names a closed fd in this
                    // single-threaded child; it is only handed to
                    // poll(2), which defines POLLNVAL for closed fds —
                    // no I/O occurs.
                    set_pdeathsig(unsafe { BorrowedFd::borrow_raw(closed_raw) }).is_err()
                };
                exit_child(i32::from(
                    !(dead_rejected && live_accepted && closed_rejected),
                ));
            }
        };
        let status = probe.wait_deadline(REAP).expect("wait probe");
        assert_eq!(child_exit_code(status), 0);
    }

    #[test]
    fn pdeathsig_kills_orphan_when_parent_dies() {
        // A forks B; B arms PDEATHSIG(A) and confirms it is armed; A
        // exits; B must die with A instead of becoming an orphan. The
        // test observes B's death via kill(b_pid, 0) → ESRCH.
        let (mut test_sock, mut a_sock) = UnixStream::pair().expect("socketpair");
        let a = match fork().expect("fork A") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // A keeps both socket ends: it reads B's "armed" byte
                // from test_sock (the peer of B's a_sock) and forwards
                // B's pid on test_sock for the main test to read from
                // a_sock.
                // A opens a pidfd on itself before forking B; B inherits
                // it and arms pid-free — no getppid() involved, so this
                // also works when B is pid 1 of a new pid namespace.
                let a_pidfd = self_pidfd().expect("A self pidfd");
                let b = match fork().expect("fork B") {
                    ForkResult::Parent { child } => child,
                    ForkResult::Child => {
                        if set_pdeathsig(a_pidfd.as_fd()).is_err() {
                            exit_child(2);
                        }
                        // Armed: tell A, then wait to be killed. Exit 3
                        // means we survived A's death — an orphan.
                        a_sock.write_all(b"R").expect("B armed");
                        // SAFETY: `sleep` takes no pointers; it can only
                        // return early on signal.
                        unsafe {
                            libc::sleep(30);
                        }
                        exit_child(3);
                    }
                };
                let b_pid = b.pid().get();
                // Deliberate: B must die via PDEATHSIG when we exit, not
                // via our supervision — forget the handle.
                #[allow(
                    clippy::mem_forget,
                    reason = "the test deliberately leaks B's supervision handle so B's death comes from PDEATHSIG, not from kill-on-drop"
                )]
                std::mem::forget(b);
                // Wait for B's "armed" byte before exiting: otherwise A
                // could die first and B would (correctly) take the
                // race-check exit instead of the PDEATHSIG path.
                let mut armed = [0u8; 1];
                test_sock.read_exact(&mut armed).expect("read armed");
                test_sock
                    .write_all(&b_pid.to_ne_bytes())
                    .expect("write b pid");
                exit_child(0);
            }
        };
        drop(test_sock);
        let mut pid_bytes = [0u8; 4];
        a_sock.read_exact(&mut pid_bytes).expect("read b pid");
        let b_pid = i32::from_ne_bytes(pid_bytes);
        let status = a.wait_deadline(REAP).expect("wait A");
        assert_eq!(child_exit_code(status), 0);
        // B must be gone: reparented and reaped by init → ESRCH. (A
        // zombie would still answer kill(pid, 0), so this also proves
        // B was reaped, not just dead.)
        let start = Instant::now();
        loop {
            // SAFETY: kill with signal 0 only probes existence.
            let ret = unsafe { libc::kill(b_pid, 0) };
            if ret != 0 {
                assert_eq!(
                    io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH),
                    "unexpected kill(b_pid, 0) failure"
                );
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "B survived its parent's death: orphaned process {b_pid}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn become_cell_user_succeeds_as_root() {
        // Mutants: the `ret != 0` checks inverted to `==` (for both
        // setresgid and setresuid). As root, both calls succeed
        // (ret == 0); the inverted check would return Err on success.
        // This test only runs usefully as root; otherwise it passes
        // vacuously (the failure-case test below covers non-root).
        if euid() != 0 {
            return;
        }
        become_cell_user().expect("become_cell_user as root must succeed");
    }

    #[test]
    fn become_cell_user_fails_for_unprivileged_caller() {
        // Mutant: become_cell_user replaced with Ok(()).
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                if euid() == 0 {
                    // As root: drop to uid 65534 (losing
                    // CAP_SETUID/CAP_SETGID), then become_cell_user
                    // must FAIL (it tries setresgid/setresuid to 0).
                    // SAFETY: setuid in a single-threaded fork child.
                    let ret = unsafe { libc::setuid(65534) };
                    if ret != 0 {
                        exit_child(99);
                    }
                }
                // Unprivileged (either dropped above, or already
                // non-root): become_cell_user must fail with EPERM.
                // The no-op mutant would return Ok, reported as exit 1.
                match become_cell_user() {
                    Ok(()) => exit_child(1),
                    Err(_) => exit_child(0),
                }
            }
        };
        let status = child.wait_deadline(REAP).expect("wait");
        assert_eq!(
            child_exit_code(status),
            0,
            "become_cell_user must fail for unprivileged caller"
        );
    }

    #[test]
    fn child_exit_code_converts_status() {
        // Cover the status-int layout raw_status synthesizes.
        let exited = |code: i32| {
            let child = match fork().expect("fork") {
                ForkResult::Parent { child } => child,
                ForkResult::Child => exit_child(code),
            };
            let status = child.wait_deadline(REAP).expect("wait");
            assert_eq!(child_exit_code(status), code);
        };
        exited(0);
        exited(1);
        exited(42);
    }

    #[test]
    fn set_no_new_privs_actually_sets_the_flag() {
        // The start state is environment-dependent: this sandbox runs
        // with NoNewPrivs=1 from PID 1 down, so the flag may already
        // be set before the first call. Observe it independently of
        // the function under test so the return-value contract is
        // pinned in every environment.
        // SAFETY: PR_GET_NO_NEW_PRIVS takes no args and always succeeds.
        let started_set = unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } == 1;
        let newly_set = set_no_new_privs().expect("set_no_new_privs");
        // The flag must be set after the call. Where the flag starts
        // clear this kills the whole-function mutants (they never
        // issue the SET prctl); where it starts set it passes
        // trivially — the return check below still bites there.
        // SAFETY: same as above.
        let now_set = unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } == 1;
        assert!(now_set, "NoNewPrivs must be set after set_no_new_privs");
        // The return reports whether THIS call newly set the flag.
        // Kills the `Ok(true)` whole-function mutant and the `!=` →
        // `==` inversion in every environment. (The `Ok(false)`
        // mutant is provably equivalent where the flag starts set;
        // see the exclusion in.cargo/mutants.toml.)
        assert_eq!(
            newly_set, !started_set,
            "return must report whether this call newly set the flag"
        );
        // Idempotent: a second call succeeds and reports already-set,
        // in every environment (the first call guarantees the flag).
        let again = set_no_new_privs().expect("set_no_new_privs again");
        assert!(!again, "second call must report the flag already set");
    }

    #[test]
    fn euid_reports_current_effective_uid() {
        let reported = euid();
        // SAFETY: geteuid takes no arguments and always succeeds.
        let actual = unsafe { libc::geteuid() };
        assert_eq!(reported, actual);
    }

    #[test]
    fn is_safe_kill_target_rejects_non_signallable_pids() {
        // Table test: kills the `>` mutants (`<`, `<=`, `>=`, `==`,
        // `!=`) and the whole-function `true`/`false` replacements.
        // 0 addresses our own group and -1 every process to kill(2);
        // both would make waitpid(2) reap an unrelated child.
        for raw in [0, 1, -1, -100, i32::MIN] {
            assert!(
                !is_safe_kill_target(raw),
                "raw pid {raw} must never be a kill(2)/waitpid(2) target"
            );
        }
        for raw in [2, 3, 100, i32::MAX] {
            assert!(
                is_safe_kill_target(raw),
                "raw pid {raw} is a valid single-process target"
            );
        }
    }

    #[test]
    fn is_safe_pgid_target_rejects_unsafe_group_ids() {
        // 999 stands in for our own group, 12345 for a foreign child
        // group. Kills the `&& → ||` mutant (row 2: `false || true`
        // would wrongly accept), the `!= → ==` mutant (row 5), and the
        // whole-function `true`/`false` replacements.
        assert!(is_safe_pgid_target(12345, 999));
        assert!(!is_safe_pgid_target(1, 999));
        assert!(!is_safe_pgid_target(0, 999));
        assert!(!is_safe_pgid_target(-1, 999));
        assert!(!is_safe_pgid_target(999, 999));
        assert!(!is_safe_pgid_target(-100, 999));
    }

    #[test]
    fn child_pid_get_reports_the_real_child_pid() {
        // Kills the `ChildPid::get → 0/1/-1` mutants fast: the reported
        // pid must be > 1 AND must name a real child of this process
        // (ppid in /proc/<pid>/stat matches our own pid). With the
        // mutant, get() returns 0/1/-1, so the `> 1` assert fires for
        // all three in milliseconds — no waiting, so this is a catch,
        // not a 32 s cargo-mutants timeout. The ppid check proves the
        // value is really our child, not just a plausible number.
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => exit_child(0),
        };
        let reported = child.pid.get();
        assert!(reported > 1, "child pid must be > 1, got {reported}");
        // Field 4 (ppid) of /proc/<pid>/stat, parsed from the last ')'
        // so a comm containing spaces or parens cannot shift fields.
        // The child is unreaped, so it exists as a zombie at least.
        let stat = std::fs::read_to_string(format!("/proc/{reported}/stat"))
            .expect("reported child pid must exist in /proc");
        let paren = stat.rfind(')').expect("stat must contain ')'");
        // `)` is ASCII, so `paren + 1` is a char boundary — but the
        // slicing lint cannot know that; `.get` keeps it total.
        let after_paren = stat.get(paren + 1..).expect("paren + 1 is a char boundary");
        let ppid: i32 = after_paren
            .split_whitespace()
            .nth(1)
            .expect("stat must have a ppid field")
            .parse()
            .expect("ppid must parse as i32");
        // SAFETY: getpid takes no arguments and always succeeds.
        let own = unsafe { libc::getpid() };
        assert_eq!(ppid, own, "reported pid {reported} is not our child");
        // Drop reaps the exited child (bounded 1 s grace); under the
        // get() mutant the asserts above already failed fast.
    }
}
