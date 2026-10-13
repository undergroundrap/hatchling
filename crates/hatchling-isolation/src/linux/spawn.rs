//! Building and running a cell on Linux: the endpoint handover, the
//! staging tree, the fork and its handshake, the in-cell setup, and the
//! cell's supervision.

use std::net::TcpListener;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use hatchling_sys::{
    Child, ChildToParent, ForkResult, IdMap, MountPlan, ParentToChild, SeccompProfile,
    handshake_pipes,
};

use super::identity::id_map;
use super::view::{mount_plan, view};
use crate::{CellSpec, Endpoint, Error, Exit, Filter, Limits, PROGRAM, PlatformError};

/// Bound for the cell child's ready byte in [`parent_after_fork`]: the
/// child only unshares two namespaces and writes one byte before this,
/// so a minute is generous — past it the child is stuck and gets
/// `SIGKILL`ed (fail closed, never a hung broker).
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound for the release byte in `child_main`: the parent only writes
/// the id maps before releasing, so the same generosity applies — the
/// child must not wait on a dead parent forever.
const RELEASE_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound for the cell's relay listener: the child brings its
/// loopback up and opens one listener right after its release, so a
/// minute is as generous as the ready byte's.
const RELAY_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound for collecting a child's `E`-report after it failed: the
/// report is a few bytes; EOF follows the child's death promptly now
/// that every fork arms `PR_SET_PDEATHSIG`.
const REPORT_TIMEOUT: Duration = Duration::from_secs(30);
/// Explicit maximum runtime of a cell, enforced by the reaper
/// (the cell child's second fork): past it the agent is `SIGKILL`ed.
/// An agent task that cannot finish in an hour is a runaway by
/// definition — the old code waited forever.
const MAX_CELL_RUNTIME: Duration = Duration::from_secs(3600);

/// Grace added to the broker's outer wait past [`MAX_CELL_RUNTIME`].
/// The broker's timer starts at spawn, the reaper's after fork+setup,
/// so without a grace the broker would SIGKILL the reaper a hair
/// before the reaper's own timeout fires and reports 124. The grace
/// makes the reaper's bounded wait the primary enforcer (clean 124
/// exit); the broker's outer wait stays a pure backstop for a wedged
/// reaper — which cannot wedge, since it is one bounded wait plus an
/// exit, so the grace is never consumed in practice.
const OUTER_WAIT_GRACE: Duration = Duration::from_secs(30);
/// Outer wait deadline for [`LinuxCell`]'s wait: [`MAX_CELL_RUNTIME`]
/// plus [`OUTER_WAIT_GRACE`]. Named (rather than inlined at the call
/// site) so the pinning test below can assert the `+` — the `-` mutant
/// would make the broker SIGKILL a healthy reaper a minute early.
const CELL_WAIT_DEADLINE: Duration =
    Duration::from_secs(MAX_CELL_RUNTIME.as_secs() + OUTER_WAIT_GRACE.as_secs());

/// Makes the broker endpoint reachable by the cell's principal: the
/// socket and its directory become the principal's, so the mapped
/// agent can traverse the directory and connect to the socket.
///
/// Inside the cell the agent is uid 0, which the kernel maps to the
/// principal on the host. The socket directory is bind-mounted
/// read-only into the cell, so host ownership and mode bits are what
/// the agent faces: a root-owned 0600 socket (the default) would reject
/// the agent's `connect` before `SO_PEERCRED` ever runs. Chowning the
/// socket (not chmodding it world-writable) keeps the peer gate as the
/// enforcement point.
///
/// Paths already owned by the target uid are left alone, which makes
/// the unprivileged case (broker uid == map base) a no-op instead of
/// an `EPERM` from an unprivileged `chown`. Anything else that cannot
/// be chowned fails closed: silently spawning an unreachable cell
/// would wedge the broker's only agent.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if the socket or its directory cannot
/// be inspected, cannot be chowned to the principal, or the directory
/// lacks owner-traverse permission afterwards.
/// Fails closed: a cell whose agent cannot reach the broker is never
/// spawned.
fn hand_over_endpoint(endpoint: &Endpoint, uid: u32) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt as _;

    let socket_meta =
        std::fs::symlink_metadata(&endpoint.socket).map_err(|source| Error::Spawn {
            reason: format!(
                "cannot inspect broker socket {}: {source}",
                endpoint.socket.display()
            ),
        })?;
    if !socket_meta.file_type().is_socket() {
        return Err(Error::Spawn {
            reason: format!(
                "broker socket path {} is not a socket",
                endpoint.socket.display()
            ),
        });
    }
    chown_to_cell(&endpoint.socket, uid, "broker socket")?;
    chown_to_cell(&endpoint.dir, uid, "broker socket directory")?;
    // The agent needs owner-traverse on the directory to reach the
    // socket through the read-only bind mount. Ownership was just
    // transferred above, so the owner bits are the agent's bits.
    let dir_mode = std::fs::metadata(&endpoint.dir)
        .map_err(|source| Error::Spawn {
            reason: format!(
                "cannot inspect broker socket dir {}: {source}",
                endpoint.dir.display()
            ),
        })?
        .mode();
    if !socket_dir_traversable(dir_mode) {
        return Err(Error::Spawn {
            reason: format!(
                "broker socket dir {} mode {dir_mode:06o} lacks owner traverse: the cell agent \
                 (uid {uid}) could not reach the broker socket",
                endpoint.dir.display(),
            ),
        });
    }
    Ok(())
}

/// Whether the cell agent can traverse the socket directory: the
/// owner-execute bit must be set on the permission bits. The agent
/// reaches the broker socket through this directory (bind-mounted
/// read-only into the cell), so without owner-traverse the connect
/// fails before `SO_PEERCRED` ever runs.
///
/// Takes the raw `mode()` value and masks to the permission bits
/// itself: a file-type bit must never fake a traverse bit.
///
/// A pure function so the bit logic is unit-testable without
/// privilege. Reaching this check through `hand_over_endpoint` with
/// a traverse-less directory needs the stat to succeed while
/// owner-execute is missing, which only a permission-bypassing
/// (root) broker can observe — the bit logic itself needs no such
/// setup.
const fn socket_dir_traversable(dir_mode: u32) -> bool {
    dir_mode & 0o777 & 0o100 != 0
}

/// Changes a path's owner to the cell uid, unless it already is.
/// Symlinks are never followed: the broker socket must be a real
/// socket, and its directory a real directory.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if the ownership probe or the chown
/// fails. An unprivileged broker cannot chown a foreign-owned path;
/// that fails closed with an actionable message instead of an
/// unreachable cell.
fn chown_to_cell(path: &Path, uid: u32, what: &str) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt as _;

    let current = std::fs::symlink_metadata(path)
        .map_err(|source| Error::Spawn {
            reason: format!("cannot inspect {what} {}: {source}", path.display()),
        })?
        .uid();
    if current == uid {
        return Ok(());
    }
    // The group goes too: the cell's gid map maps the same number
    // (`id_map(principal)` for both maps). Before the cell drops to its
    // ids, it builds its mounts as the host uid that forked it, and a
    // root broker's run cell stages inside its handed-over run
    // directory: owner-only, and owned by the cell. Its namespace
    // capabilities reach only inodes whose owner *and* group are mapped,
    // so a root group left behind makes the directory untraversable and
    // every scheduled run fails at its first mount.
    // rustix::fs::chown is the safe wrapper; no unsafe leaves
    // hatchling-sys.
    hatchling_sys::chown_ids(path, uid, uid).map_err(|source| Error::Spawn {
        reason: format!(
            "cannot chown {what} {} to cell uid {uid}: {source} \
             (run the broker with privilege to transfer ownership, or chown it beforehand)",
            path.display(),
        ),
    })?;
    Ok(())
}

/// RAII guard for the cell staging directory: removes it on drop
/// unless [`StagingGuard::defuse`] releases ownership first.
///
/// [`isolate`] creates the staging tree before forking; if the fork
/// succeeds but a parent-side handshake step fails, the child is gone
/// but the directory would leak without this guard. The child's
/// staging cleanup happens in its own namespace via `pivot_root` —
///
/// this guard only covers the parent path.
struct StagingGuard {
    dir: PathBuf,
    defused: bool,
}

impl StagingGuard {
    const fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            defused: false,
        }
    }

    /// Releases the directory: the caller takes over cleanup
    /// responsibility (normally [`LinuxCell`]'s wait).
    fn defuse(mut self) -> PathBuf {
        self.defused = true;
        std::mem::replace(&mut self.dir, PathBuf::new())
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if !self.defused {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// A spawned cell: the intermediate child under supervision and the
/// resources the reaper needs.
///
/// The child is an owned [`Child`]: pidfd-pinned signalling, bounded
/// waits, and kill-on-drop. Dropping the cell without waiting
/// SIGKILLs it — no orphans — and removes the staging directory
/// best-effort.
#[derive(Debug)]
pub struct LinuxCell {
    /// The cell child (the spawner's direct child: setup, then the
    /// agent's reaper). Owned supervision: kill-on-drop, bounded wait.
    child: Child,
    /// Staging directory holding the cell's mount tree; removed on
    /// wait (or best-effort on drop).
    staging_dir: PathBuf,
    /// The child->parent pipe: the child reports setup failures here
    /// before the agent image replaces it.
    child_to_parent: ChildToParent,
    /// The cell's relay listener, until the owner takes it.
    relay: Option<TcpListener>,
}

impl crate::Cell for LinuxCell {
    /// The cell child's pid, for diagnostics and logging only. Never
    /// a signal target: signalling goes through the owned [`Child`]'s
    /// pidfd (see [`hatchling_sys::ChildPid::get`]).
    fn pid(&self) -> i32 {
        self.child.pid().get()
    }

    /// Waits for the cell to terminate. The reaper exits with the
    /// agent's exit code, or `124` if the agent exceeded
    /// [`MAX_CELL_RUNTIME`] (mirroring GNU timeout).
    ///
    /// The wait is bounded by [`MAX_CELL_RUNTIME`] plus
    /// [`OUTER_WAIT_GRACE`]: the reaper enforces the runtime with its
    /// own bounded wait (exiting 124 on timeout); past the outer
    /// deadline the cell is `SIGKILL`ed via its pidfd and
    /// [`Error::Wait`] is returned. Fails closed if a child
    /// reported a setup failure on the pipe, and always removes the
    /// staging directory afterwards.
    fn wait(self) -> Result<Exit, Error> {
        self.wait_with_deadlines(CELL_WAIT_DEADLINE, REPORT_TIMEOUT)
            .map(|status| Exit {
                code: hatchling_sys::child_exit_code(status),
                status,
            })
    }

    fn relay(&mut self) -> Option<TcpListener> {
        self.relay.take()
    }
}

impl LinuxCell {
    /// Test seam for the cell's wait: the same logic with explicit
    /// deadlines for the child wait and the setup-failure report read,
    /// returning the reaper's raw `waitpid` status. The two waits need
    /// different bounds: the child may legitimately run up to an hour,
    /// but the report pipe must reach EOF promptly once the child is
    /// gone — a leaked writer (or a mutant that never closes it) would
    /// otherwise turn every broken read path into an hour-long hang.
    /// Production deadlines are an hour and 30 s; a broken wait would
    /// sit those out past any test runner's patience, so tests pass a
    /// couple of seconds here.
    fn wait_with_deadlines(
        mut self,
        child_deadline: Duration,
        report_deadline: Duration,
    ) -> Result<i32, Error> {
        let status = self
            .child
            .wait_deadline(child_deadline)
            .map_err(|source| Error::Wait(PlatformError::new(source)))?;
        // The pipe reaches EOF once every child has exited or exec'd
        // (CLOEXEC); anything written before that is a setup failure
        // report. Bounded: a leaked writer cannot hang the broker.
        let report = self
            .child_to_parent
            .read_to_end_deadline(report_deadline)
            .map_err(|source| Error::Wait(PlatformError::new(source)))?;
        self.child_to_parent.close_read();
        let _ = std::fs::remove_dir_all(&self.staging_dir);
        if report.first() == Some(&b'E') {
            let message: Vec<u8> = report.iter().skip(1).copied().collect();
            let reason = String::from_utf8_lossy(&message).into_owned();
            return Err(Error::Spawn { reason });
        }
        Ok(status)
    }
}

impl Drop for LinuxCell {
    /// Best-effort staging cleanup for a cell abandoned without a
    /// wait. The child is `SIGKILL`ed and reaped BEFORE the staging
    /// dir is touched: the cell's mount tree lives there, and removing
    /// it under a live child can fail (`EBUSY`) or leave a half-removed
    /// tree. The `Child`'s own `Drop` repeats the terminate+reap
    /// harmlessly when the fields drop right after.
    fn drop(&mut self) {
        let _ = self.child.terminate();
        // Bounded reap: the SIGKILL is already in flight, so this
        // returns as soon as the child dies. Already-reaped is an
        // ignored error, not a hang.
        let _ = self.child.wait_deadline(Duration::from_secs(5));
        let _ = std::fs::remove_dir_all(&self.staging_dir);
    }
}

/// Counter disambiguating concurrent cell spawns from this process.
/// `hatchling-cell-{pid}` alone collides when one broker process
/// spawns two cells concurrently, so the staging name also carries the
/// spawning thread id and this counter (the same pattern as the probe
/// staging names).
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A staging directory unique to this spawn.
///
/// The mount tree is built here, then handed to `pivot_root`.
/// Uniqueness matters: two concurrent spawns sharing a staging dir
/// would corrupt each other's mount trees.
fn staging_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "hatchling-cell-{}-{:?}-{}",
        std::process::id(),
        std::thread::current().id(),
        STAGING_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Spawns the cell and execs the spec's program inside it.
///
/// On success the program is running as PID 1 in the new pid namespace,
/// confined by the mount namespace, the seccomp allowlist, and
/// `NoNewPrivs`. The returned cell must be waited on (directly, or by a
/// reaper thread) to avoid a zombie and to clean up the staging
/// directory.
///
/// # Errors
///
/// Returns [`Error`] if the endpoint handover, id-map setup, mount
/// construction, or any spawn step fails. Failures before the program
/// image is exec'd are fail-closed: the child exits without running
/// anything.
pub(super) fn isolate(spec: &CellSpec) -> Result<LinuxCell, Error> {
    // The agent reaches the broker through the socket directory, which
    // is bind-mounted read-only into the cell. Inside the cell the
    // agent runs as the map base (a host uid), so the host socket and
    // its directory must be owned by that uid: otherwise filesystem
    // permissions reject the agent before SO_PEERCRED ever runs.
    // Ownership transfer happens here, before the fork, while the
    // broker still has its full privilege. A cell without an endpoint
    // has no socket.
    if let Some(endpoint) = &spec.endpoint {
        hand_over_endpoint(endpoint, spec.identity.principal)?;
    }
    // Staging: the mount tree is built here, then pivot_rooted into.
    let staging_dir = staging_dir();
    std::fs::create_dir_all(&staging_dir).map_err(|source| Error::Spawn {
        reason: format!(
            "cannot create cell staging dir {}: {source}",
            staging_dir.display()
        ),
    })?;
    std::fs::set_permissions(&staging_dir, std::fs::Permissions::from_mode(0o700)).map_err(
        |source| Error::Spawn {
            reason: format!("cannot chmod cell staging dir: {source}"),
        },
    )?;
    let new_root = staging_dir.join("root");
    std::fs::create_dir_all(&new_root).map_err(|source| Error::Spawn {
        reason: format!("cannot create cell root dir: {source}"),
    })?;

    let plan = mount_plan(view(spec)?, &new_root)?;
    let uid_map = id_map(spec.identity.principal, spec.identity.count)?;
    let gid_map = id_map(spec.identity.principal, spec.identity.count)?;
    let (child_to_parent, parent_to_child) = handshake_pipes().map_err(|source| Error::Spawn {
        reason: format!("cannot create cell handshake pipes: {source}"),
    })?;
    // The child's PDEATHSIG liveness fd: the broker opens a pidfd on
    // itself before the fork; the child inherits it and polls it pid-free
    // after arming (see `hatchling_sys::set_pdeathsig`). The broker is
    // alive here (it blocks in `parent_after_fork` below), so the check
    // passes.
    let broker_pidfd = hatchling_sys::self_pidfd().map_err(|source| Error::Spawn {
        reason: format!("cannot open broker pidfd: {source}"),
    })?;
    // The relay's channel: the child sends its listener on one
    // end, the owner receives it on the other.
    let relay = match spec.relay {
        Some(port) => Some((
            port,
            UnixStream::pair().map_err(|source| Error::Spawn {
                reason: format!("cannot create the relay channel: {source}"),
            })?,
        )),
        None => None,
    };

    match hatchling_sys::fork().map_err(|source| Error::Start(PlatformError::new(source)))? {
        ForkResult::Parent { child } => {
            // From here on the staging directory must not leak: the
            // guard removes it if any parent-side handshake step fails.
            let staging_guard = StagingGuard::new(staging_dir);
            // Only the child sends on its end; holding it here would keep
            // the receive below from ever seeing the child give up.
            let relay = relay.map(|(_, (ours, theirs))| {
                drop(theirs);
                ours
            });
            let mut child_to_parent = child_to_parent;
            let mut parent_to_child = parent_to_child;
            let child = parent_after_fork(
                child,
                uid_map,
                gid_map,
                &mut child_to_parent,
                &mut parent_to_child,
                READY_TIMEOUT,
            )?;
            let relay = match relay.map(|channel| receive_relay(&channel, RELAY_TIMEOUT)) {
                None => None,
                Some(Ok(listener)) => Some(listener),
                Some(Err(error)) => {
                    // Fail closed: the child dies (SIGKILL, bounded reap)
                    // before its report, if any, is read.
                    drop(child);
                    let report = child_to_parent
                        .read_to_end_deadline(REPORT_TIMEOUT)
                        .unwrap_or_default();
                    return Err(with_report(error, &report));
                }
            };
            let staging_dir = staging_guard.defuse();
            Ok(LinuxCell {
                child,
                staging_dir,
                child_to_parent,
                relay,
            })
        }
        ForkResult::Child => {
            let relay = relay.map(|(port, (ours, theirs))| {
                drop(ours);
                (port, theirs)
            });
            child_main(
                spec,
                &plan,
                broker_pidfd,
                (child_to_parent, parent_to_child),
                relay,
            )
        }
    }
}

/// Receives the cell's relay listener on `channel`, within
/// `timeout`, and checks it is one.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if nothing arrives in time, or what arrives
/// is not a listening socket.
fn receive_relay(channel: &UnixStream, timeout: Duration) -> Result<TcpListener, Error> {
    let failed = |reason: String| Error::Spawn {
        reason: format!("the cell's relay listener did not arrive: {reason}"),
    };
    channel
        .set_read_timeout(Some(timeout))
        .map_err(|source| failed(source.to_string()))?;
    let fd = hatchling_sys::net::recv_fd(channel.as_fd())
        .map_err(|source| failed(source.to_string()))?;
    let listening = hatchling_sys::net::is_listening(fd.as_fd())
        .map_err(|source| failed(source.to_string()))?;
    if !listening {
        return Err(failed(String::from(
            "what arrived is not a listening socket",
        )));
    }
    Ok(TcpListener::from(fd))
}

/// `error`, with what the child reported, if it reported anything.
fn with_report(error: Error, report: &[u8]) -> Error {
    match (error, report.split_first()) {
        (Error::Spawn { reason }, Some((b'E', message))) => Error::Spawn {
            reason: format!(
                "{reason}; the child reported {}",
                String::from_utf8_lossy(message)
            ),
        },
        (error, _) => error,
    }
}

/// The reaper's fds: it never execs, so close-on-exec never cleans it
/// up, and it would hold every descriptor the owner had open at the spawn
/// (a skill copy still being written, a connection's socket) for the
/// cell's whole life. It keeps only what it uses: the broker's pidfd, the
/// agent's, and the report pipe.
fn reaper_keeps_only(
    broker_pidfd: &OwnedFd,
    agent: &Child,
    report: &ChildToParent,
) -> Result<(), hatchling_sys::Error> {
    let keep: Vec<_> = [broker_pidfd.as_fd(), agent.as_fd()]
        .into_iter()
        .chain(report.fds())
        .collect();
    hatchling_sys::close_fds_above_stderr_except(&keep)
}

/// The reaper once the cell is forked: the cell's mapped user (uid/gid 0
/// inside the namespace, host-visible as the configured base uid), its
/// death signal armed again against the broker, and no capabilities
/// . A change of user clears the death signal, and a root
/// broker's child changes it here.
fn settle_reaper(broker_pidfd: OwnedFd) -> Result<(), String> {
    hatchling_sys::become_cell_user().map_err(|source| format!("become cell user: {source}"))?;
    hatchling_sys::set_pdeathsig(broker_pidfd.as_fd())
        .map_err(|source| format!("pdeathsig: {source}"))?;
    drop(broker_pidfd);
    hatchling_sys::drop_capabilities().map_err(|source| format!("drop capabilities: {source}"))
}

/// The child's side of the relay: its loopback up, a listener
/// on `port`, sent to the owner on `channel`. The child keeps no copy.
fn hand_out_relay(port: u16, channel: &UnixStream) -> Result<(), hatchling_sys::Error> {
    hatchling_sys::net::loopback_up()?;
    let listener = hatchling_sys::net::loopback_listener(port)?;
    hatchling_sys::net::send_fd(channel.as_fd(), listener.as_fd())
}

/// Parent side after fork: synchronize with the child, write the id
/// maps, release the child into mount construction.
///
/// Takes the owned child and returns it on success; on failure the
/// child is dropped (SIGKILL + bounded reap via its pidfd — no
/// orphans) before the error is returned.
fn parent_after_fork(
    child: Child,
    uid_map: IdMap,
    gid_map: IdMap,
    child_to_parent: &mut ChildToParent,
    parent_to_child: &mut ParentToChild,
    ready_timeout: Duration,
) -> Result<Child, Error> {
    // Wait for the child's ready byte: EOF or anything else means it
    // died or failed before the namespaces were ready. If the child
    // reported 'E' (error), include its message for diagnostics.
    // Bounded: a stuck child cannot hang the broker.
    let ready = child_to_parent
        .read_byte_deadline(ready_timeout)
        .map_err(|source| Error::Spawn {
            reason: format!("cannot read cell child ready byte: {source}"),
        })?;
    if ready != Some(b'R') {
        let detail = if ready == Some(b'E') {
            let msg = child_to_parent
                .read_to_end_deadline(REPORT_TIMEOUT)
                .unwrap_or_default();
            format!(": child reported {}", String::from_utf8_lossy(&msg))
        } else {
            String::new()
        };
        // Fail closed: dropping the owned child SIGKILLs and reaps it
        // (bounded); the pipe ends close with their owners.
        drop(child);
        return Err(Error::Spawn {
            reason: format!("cell child failed before namespaces were ready{detail}"),
        });
    }
    // The raw pid is only a `/proc` path component for the id maps —
    // never a signal target (see `ChildPid::get`).
    if let Err(source) = hatchling_sys::idmap::write_maps(child.pid().get(), &uid_map, &gid_map) {
        drop(child);
        return Err(Error::Identity(PlatformError::new(source)));
    }
    // Release the child into mount construction, then drop our write
    // end: the parent never writes again. The child's read end stays
    // open so `wait` still sees EOF only after every child is gone.
    if let Err(source) = parent_to_child.write_byte(b'D') {
        drop(child);
        return Err(Error::Spawn {
            reason: format!("cannot release cell child into mount setup: {source}"),
        });
    }
    parent_to_child.close_write();
    child_to_parent.close_write();
    Ok(child)
}

/// Returns true if the child must exit due to a bad release byte.
///
/// The parent releases the child into mount construction with b'D';
/// anything else (wrong byte, EOF, I/O error) means the parent died
/// or the handshake failed, and the child must not proceed.
///
/// The `!` is the mutant target: deleting it inverts the logic,
/// causing the child to exit on the correct byte and proceed on a
/// wrong one.
const fn should_exit_on_release(released: &Result<Option<u8>, hatchling_sys::Error>) -> bool {
    !matches!(released, Ok(Some(b'D')))
}

/// Child side: namespaces, mounts, pid namespace, seccomp, exec.
///
/// Diverges; only returns to [`hatchling_sys::exit_child`] on failure,
/// after reporting the reason on the child->parent pipe.
///
/// `broker_pidfd` is a pidfd the broker opened on itself before the fork
/// (inherited by this child): the first thing this child does is arm
/// `PR_SET_PDEATHSIG` and poll it pid-free, so a dead broker can never
/// leave this child running unsupervised. The reaper (second fork's
/// parent) is covered by the same arm — it *is* this child — and the
/// agent arms it against the reaper before exec. A change of user clears
/// the signal, so each side arms again once it is the cell's user (a
/// root broker's child changes it; ). `PR_SET_PDEATHSIG`
/// survives `execve`, so the agent's arm covers its whole lifetime, not
/// just the pre-exec window.
#[allow(
    clippy::too_many_lines,
    reason = "the cell is set up in the one order the kernel allows, step after step; split up, the order would be harder to check"
)]
fn child_main(
    spec: &CellSpec,
    plan: &MountPlan,
    broker_pidfd: OwnedFd,
    (mut child_to_parent, mut parent_to_child): (ChildToParent, ParentToChild),
    relay: Option<(u16, UnixStream)>,
) -> ! {
    let fail = |child_to_parent: &mut ChildToParent, reason: String| -> ! {
        fail_cell(child_to_parent, &reason)
    };
    // Arm first: if the broker is gone, die with it rather than
    // continuing setup unsupervised. The broker is alive here (it
    // blocks in `parent_after_fork`), so the pidfd liveness check
    // inside `set_pdeathsig` passes. The fd is kept for the reaper
    // below, which arms again after it becomes the cell's user ;
    // pid 1 drops its copy at once.
    if let Err(source) = hatchling_sys::set_pdeathsig(broker_pidfd.as_fd()) {
        fail(&mut child_to_parent, format!("pdeathsig: {source}"));
    }
    if let Err(reason) = drop_inherited_groups(hatchling_sys::euid(), hatchling_sys::clear_groups)
        .map_err(|source| format!("setgroups: {source}"))
        .and_then(|()| {
            hatchling_sys::unshare_user().map_err(|source| format!("unshare(user): {source}"))
        })
    {
        fail(&mut child_to_parent, reason);
    }
    if let Err(source) = hatchling_sys::unshare_mount() {
        fail(&mut child_to_parent, format!("unshare(mount): {source}"));
    }
    // The cell's own network and IPC namespaces: its loopback and
    // nothing else, so no host abstract unix socket, host TCP, netlink, or
    // SysV IPC object is reachable. The broker socket is unaffected: it is
    // a pathname socket, found through the read-only `/sock` bind (the
    // mount namespace), not by name in the network namespace. Fail closed:
    // a cell that cannot get its own namespaces is never spawned into the
    // host's.
    if let Err(source) = hatchling_sys::unshare_net() {
        fail(&mut child_to_parent, format!("unshare(net): {source}"));
    }
    if let Err(source) = hatchling_sys::unshare_ipc() {
        fail(&mut child_to_parent, format!("unshare(ipc): {source}"));
    }
    // Signal readiness; the parent writes the id maps, then releases
    // us. The parent->child read end is no longer needed after the
    // release byte. The child->parent write end stays open: `fail()`
    // reports mount/seccomp/exec failures on it after this point, and
    // a successful exec closes it via CLOEXEC.
    if child_to_parent.write_byte(b'R').is_err() {
        hatchling_sys::exit_child(1);
    }
    // Bounded: the parent writes the id maps and the release byte
    // promptly; a dead parent must not wedge the child here forever.
    // Any non-`b'D` outcome (timeout included) exits fail-closed.
    let released = parent_to_child.read_byte_deadline(RELEASE_TIMEOUT);
    parent_to_child.close_read();
    if should_exit_on_release(&released) {
        hatchling_sys::exit_child(1);
    }
    // The relay: the cell's loopback up and one listener on its
    // port, sent to the owner while this process still holds its
    // namespace's capabilities. The cell keeps no copy.
    if let Some((port, channel)) = relay {
        if let Err(source) = hand_out_relay(port, &channel) {
            fail(&mut child_to_parent, format!("relay: {source}"));
        }
        drop(channel);
    }
    if let Err(source) = hatchling_sys::unshare_pid() {
        fail(&mut child_to_parent, format!("unshare(pid): {source}"));
    }
    // The reaper's PDEATHSIG liveness fd for the agent below: opened on
    // this process before the fork and inherited by the pid-1 child,
    // which polls it pid-free after arming. Pidfds name the task, not
    // the numeric pid, so this works across the pid namespace boundary —
    // pid 1 always sees getppid() == 0, which a pid comparison misreads
    // as "parent died" and kills every cell before exec.
    let reaper_pidfd = match hatchling_sys::self_pidfd() {
        Ok(fd) => fd,
        Err(source) => fail(&mut child_to_parent, format!("reaper pidfd: {source}")),
    };
    match hatchling_sys::fork() {
        Ok(ForkResult::Parent { child: agent }) => {
            // Reaper: the agent's backstop. Bounded wait — a hung
            // agent is SIGKILLed via its pidfd past MAX_CELL_RUNTIME,
            // never left running. Exits with the agent's status, or
            // 124 on timeout (mirroring GNU timeout).
            // The child->parent pipe's read end is closed here so the
            // broker's `wait` sees EOF; the agent's copies stay open
            // until exec (CLOEXEC) or exit.
            // The reaper's own pidfd is not needed here — the pid-1
            // child holds the inherited copy for its liveness check.
            drop(reaper_pidfd);
            if let Err(source) = reaper_keeps_only(&broker_pidfd, &agent, &child_to_parent) {
                fail(
                    &mut child_to_parent,
                    format!("close inherited fds: {source}"),
                );
            }
            if let Err(reason) = settle_reaper(broker_pidfd) {
                fail(&mut child_to_parent, reason);
            }
            child_to_parent.close_read();
            agent.wait_deadline(MAX_CELL_RUNTIME).map_or_else(
                |_| hatchling_sys::exit_child(124),
                |status| hatchling_sys::exit_child(hatchling_sys::child_exit_code(status)),
            )
        }
        Ok(ForkResult::Child) => {
            drop(broker_pidfd);
            // PID 1 of the new pid namespace: arm PDEATHSIG against
            // the reaper. `PR_SET_PDEATHSIG` survives `execve` (it is
            // cleared only for set-user/group-ID or file-capability
            // binaries, which the agent never is, and by a change of
            // user, armed again below), so the agent stays covered for
            // its whole lifetime: if the reaper dies, the agent is
            // SIGKILLed rather than orphaned.
            if let Err(source) = hatchling_sys::set_pdeathsig(reaper_pidfd.as_fd()) {
                fail(&mut child_to_parent, format!("pdeathsig: {source}"));
            }
            // The mount plan, applied by the cell's pid 1: a
            // procfs of the cell's own pid namespace can be mounted only
            // from inside it, while a whole one is still visible, so the
            // tree is built here, before the old root is detached. The
            // plan ends as the cell's mapped user (see MountPlan::apply).
            if let Err(source) = plan.apply() {
                fail(&mut child_to_parent, format!("mount plan: {source}"));
            }
            // A root broker's child changed user in the plan, which
            // cleared the death signal: armed again.
            if let Err(source) = hatchling_sys::set_pdeathsig(reaper_pidfd.as_fd()) {
                fail(&mut child_to_parent, format!("pdeathsig: {source}"));
            }
            // The liveness fd served its purpose: dropped before the
            // program is forked, so it never leaks into the program.
            drop(reaper_pidfd);
            run_as_init(spec, child_to_parent)
        }
        Err(_) => fail(&mut child_to_parent, String::from("fork for pid namespace")),
    }
}

/// Reports `reason` on the child->parent pipe, as a setup failure, and
/// exits: the cell never starts.
fn fail_cell(child_to_parent: &mut ChildToParent, reason: &str) -> ! {
    let mut msg = Vec::with_capacity(reason.len() + 1);
    msg.push(b'E');
    msg.extend_from_slice(reason.as_bytes());
    let _ = child_to_parent.write_all(&msg);
    child_to_parent.close_write();
    hatchling_sys::exit_child(1);
}

/// The cell's pid 1 once its view is built: Hatchling's init.
///
/// It forks the program, which runs as pid 2, and stays. It never runs
/// the program's code, so the parent-death signal it holds against the
/// reaper is one the program can't clear: the program may clear its
/// own, and still dies with its supervisor, since the kernel kills a pid
/// namespace whose init dies. Until then it reaps every child that exits,
/// and it ends with the program's exit code. It holds no capability,
/// can't be dumped or traced by the program it shares a uid with, and
/// runs under `no_new_privs`.
fn run_as_init(spec: &CellSpec, mut child_to_parent: ChildToParent) -> ! {
    let program = match hatchling_sys::fork() {
        Ok(ForkResult::Child) => exec_cell_program(spec, child_to_parent),
        Ok(ForkResult::Parent { child }) => child,
        Err(source) => fail_cell(&mut child_to_parent, &format!("fork the program: {source}")),
    };
    // It never execs, so nothing it inherited closes by itself: it keeps
    // only the program's pidfd and the report pipe, as the reaper does.
    let keep: Vec<_> = std::iter::once(program.as_fd())
        .chain(child_to_parent.fds())
        .collect();
    if let Err(source) = hatchling_sys::close_fds_above_stderr_except(&keep) {
        let _ = program.terminate();
        fail_cell(
            &mut child_to_parent,
            &format!("init: close inherited fds: {source}"),
        );
    }
    let hardened = hatchling_sys::set_not_dumpable()
        .and_then(|()| hatchling_sys::drop_capabilities())
        .and_then(|()| hatchling_sys::set_no_new_privs());
    if let Err(source) = hardened {
        let _ = program.terminate();
        fail_cell(&mut child_to_parent, &format!("init: {source}"));
    }
    // The program reports its own setup failures, and its exec closes the
    // pipe: the init's copy must not hold the broker's wait open.
    child_to_parent.close_write();
    hatchling_sys::exit_child(hatchling_sys::reap_as_init(&program))
}

/// The cell's program, pid 2: capabilities dropped, every fd but stdio
/// closed on exec, `no_new_privs`, its working directory, its seccomp
/// filter, then the exec. Setup failures are reported on the pipe.
fn exec_cell_program(spec: &CellSpec, mut child_to_parent: ChildToParent) -> ! {
    let fail = |child_to_parent: &mut ChildToParent, reason: String| -> ! {
        fail_cell(child_to_parent, &reason)
    };
    // Capabilities stop at the boundary: uid 0 in its own
    // namespace, the program holds none. Nothing below needs one,
    // and seccomp would refuse these calls later.
    if let Err(source) = hatchling_sys::drop_capabilities() {
        fail(&mut child_to_parent, format!("drop capabilities: {source}"));
    }
    // Nothing but stdio crosses the exec: every fd above
    // stderr is marked close-on-exec, whatever the broker forgot to
    // mark. The report pipe stays usable until the exec itself.
    if let Err(source) = hatchling_sys::cloexec_fds_above_stderr() {
        fail(&mut child_to_parent, format!("close_range: {source}"));
    }
    if let Err(source) = hatchling_sys::set_no_new_privs() {
        fail(&mut child_to_parent, format!("no_new_privs: {source}"));
    }
    // Set before seccomp narrows what the cell may call.
    if let Some(dir) = spec.workdir.as_deref() {
        if let Err(source) = std::env::set_current_dir(dir) {
            fail(&mut child_to_parent, format!("chdir({dir}): {source}"));
        }
    }
    let profile = match seccomp_profile(spec.filter) {
        Ok(profile) => profile,
        Err(source) => fail(&mut child_to_parent, format!("seccomp profile: {source}")),
    };
    if let Err(source) = hatchling_sys::seccomp::install(&profile) {
        fail(&mut child_to_parent, format!("seccomp install: {source}"));
    }
    let err = exec_program(spec);
    fail(&mut child_to_parent, format!("exec agent: {err}"));
}

/// The seccomp profile for a filter class: a work-tree program's adds
/// the file calls it needs to edit its granted directories ;
/// every other program gets the agent's.
fn seccomp_profile(filter: Filter) -> Result<SeccompProfile, hatchling_sys::Error> {
    match filter {
        Filter::Cli => SeccompProfile::harness_cli(),
        Filter::WorkTree => SeccompProfile::harness(),
        Filter::Program => SeccompProfile::agent(),
    }
}

/// Execs the spec's program at [`PROGRAM`] with exactly its argv and
/// environment, held to its limits from just before the exec. Returns
/// only on failure.
fn exec_program(spec: &CellSpec) -> hatchling_sys::Error {
    let argv: Vec<&str> = spec.argv.iter().map(String::as_str).collect();
    let env: Vec<(&str, &str)> = spec
        .env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    hatchling_sys::exec(Path::new(PROGRAM), &argv, &env, || {
        hold_to_limits(&spec.limits)
    })
}

/// Holds this process to each limit `limits` sets; one it leaves `None`
/// stays as it was.
///
/// # Errors
///
/// The first limit the kernel refuses.
fn hold_to_limits(limits: &Limits) -> Result<(), hatchling_sys::Error> {
    [
        (hatchling_sys::Limit::Processes, limits.processes),
        (hatchling_sys::Limit::AddressSpace, limits.address_space),
        (hatchling_sys::Limit::FileSize, limits.file_size),
        (hatchling_sys::Limit::OpenFiles, limits.open_files),
    ]
    .into_iter()
    .try_for_each(|(limit, value)| {
        value.map_or(Ok(()), |value| hatchling_sys::hold_to(limit, value))
    })
}

/// Drops a root broker's supplementary groups with `clear`, which would
/// otherwise follow the cell into its user namespace, where it couldn't
/// drop them. The cell's child calls it before `unshare(CLONE_NEWUSER)`,
/// with its `euid` and [`hatchling_sys::clear_groups`]. An unprivileged
/// broker has none it may drop, so `clear` isn't called.
///
/// # Errors
///
/// What `clear` returns.
fn drop_inherited_groups(
    euid: u32,
    clear: impl FnOnce() -> Result<(), hatchling_sys::Error>,
) -> Result<(), hatchling_sys::Error> {
    if euid == 0 { clear() } else { Ok(()) }
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn only_a_root_broker_drops_its_groups() {
        let refused = || {
            Err(hatchling_sys::Error::Process {
                operation: "setgroups(0)",
                source: std::io::Error::from_raw_os_error(1),
            })
        };
        // As root, the drop is made, and its failure is the child's.
        let mut cleared = false;
        assert!(
            drop_inherited_groups(0, || {
                cleared = true;
                Ok(())
            })
            .is_ok()
        );
        assert!(cleared, "a root broker's cell drops its groups");
        assert!(drop_inherited_groups(0, refused).is_err());
        // Unprivileged, nothing is tried: there is nothing it may drop.
        assert!(drop_inherited_groups(1000, refused).is_ok());
    }
    use crate::linux::view::tests::{Scratch, specs, within_watchdog};
    use crate::{Cell, CellIdentity};

    /// Short deadline for the handshake/wait tests below. Production
    /// uses 60 s / ~1 h; a broken wait must fail in seconds here, not
    /// past the mutation runner's per-test timeout.
    const SHORT_DEADLINE: Duration = Duration::from_secs(2);

    /// Scratch dir for fixtures that assert real ownership changes.
    ///
    /// Mutation runs point `TMPDIR` into the workspace scratch tree, and
    /// the workspace filesystem here denies chown(2) even to root. These
    /// fixtures must not inherit `TMPDIR`; `/var/tmp` is a real
    /// filesystem where chown works.
    ///
    /// Outside `TMPDIR`, nothing else ever clears these, so the guard
    /// removes the directory when the test ends, pass or fail.
    fn owned_test_dir(tag: &str) -> OwnedTestDir {
        OwnedTestDir(
            Path::new("/var/tmp").join(format!("hatchling-isolation-{tag}-{}", std::process::id())),
        )
    }

    /// A test's scratch directory, removed on drop so a failing
    /// assertion cannot leak it.
    struct OwnedTestDir(PathBuf);

    impl std::ops::Deref for OwnedTestDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for OwnedTestDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for OwnedTestDir {
        fn drop(&mut self) {
            if std::fs::remove_dir_all(&self.0).is_err() {
                // A test that failed mid-way can leave a directory
                // without owner search or write permission (the
                // untraversable-dir fixture): restore it, then retry.
                restore_owner_access(&self.0);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    /// Gives the owner full access to `path` and every directory below
    /// it, without following symlinks.
    fn restore_owner_access(path: &Path) {
        let is_dir = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
        if !is_dir {
            return;
        }
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            restore_owner_access(&entry.path());
        }
    }

    #[test]
    fn an_owned_test_dir_is_removed_even_when_left_untraversable() {
        let dir = owned_test_dir("guard");
        let _ = std::fs::remove_dir_all(&*dir);
        let inner = dir.join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("file"), b"x").unwrap();
        // What a panicking untraversable-dir test leaves behind.
        std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::set_permissions(&*dir, std::fs::Permissions::from_mode(0o600)).unwrap();
        let path = dir.to_path_buf();
        drop(dir);
        assert!(
            std::fs::symlink_metadata(&path).is_err(),
            "{} was left behind",
            path.display()
        );
    }

    #[test]
    fn socket_dir_traverse_bit_logic() {
        // Mutants: `& 0o777` -> `|` (permission mask), `& 0o100` ->
        // `|`/`^` (traverse-bit test), `!= 0` -> `== 0`. A
        // traverse-less directory must stay rejected and a
        // traversable one accepted; the file-type bits must not fake
        // a traverse bit through the mask.
        assert!(socket_dir_traversable(0o040_700));
        assert!(socket_dir_traversable(0o040_711));
        assert!(!socket_dir_traversable(0o040_600));
        assert!(!socket_dir_traversable(0o040_666));
        assert!(!socket_dir_traversable(0o100_600));
        assert!(socket_dir_traversable(0o100_700));
    }

    #[test]
    fn staging_guard_removes_dir_on_drop() {
        // Mutants: Drop replaced with (), or the `!` deleted (so it
        // removes even when defused, or never removes).
        let dir = std::env::temp_dir().join(format!(
            "hatchling-isolation-guard-drop-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        {
            let _guard = StagingGuard::new(dir.clone());
        }
        assert!(
            !dir.exists(),
            "StagingGuard::drop must remove the directory"
        );
    }

    #[test]
    fn staging_guard_defuse_releases_the_dir() {
        // Mutants: defuse replaced with Default::default(), or drop's
        // `!` deleted. Defused guards must not delete; defuse must
        // return the original path.
        let dir = std::env::temp_dir().join(format!(
            "hatchling-isolation-guard-defuse-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let released = {
            let guard = StagingGuard::new(dir.clone());
            guard.defuse()
        };
        assert_eq!(released, dir);
        assert!(dir.exists(), "defused guard must not remove the directory");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_dir_names_are_unique_per_spawn() {
        // Two concurrent spawns from one broker process must never
        // share a staging tree. Mutant: counter removed (pid+tid only)
        // — same-thread consecutive spawns would collide.
        let first = staging_dir();
        let second = staging_dir();
        assert_ne!(first, second);
        assert!(
            first
                .file_name()
                .expect("staging dir has a name")
                .to_string_lossy()
                .starts_with("hatchling-cell-")
        );
    }

    #[test]
    fn staging_dir_names_are_unique_across_threads() {
        // The counter is process-wide (AtomicU64): 8 threads racing
        // staging_dir() must all get distinct names. Mutant: counter
        // replaced with a thread-local or removed — cross-thread
        // collisions would appear.
        let names = std::thread::scope(|s| {
            (0..8)
                .map(|_| s.spawn(|| (0..16).map(|_| staging_dir()).collect::<Vec<_>>()))
                .flat_map(|h| h.join().expect("worker thread"))
                .collect::<Vec<_>>()
        });
        let unique: HashSet<_> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "staging dir names must be unique across threads"
        );
    }

    #[test]
    fn chown_to_cell_is_a_noop_when_already_owned() {
        // Mutants: chown_to_cell replaced with Ok(()), or the
        // `current == uid` check inverted. A path already owned by the
        // target uid must succeed without chowning (no privilege
        // needed); the early return is the observable behavior.
        let dir = owned_test_dir("chown");
        std::fs::create_dir_all(&dir).unwrap();
        let euid = hatchling_sys::euid();
        // The temp dir is owned by us; chown to our own uid is a no-op.
        chown_to_cell(&dir, euid, "test dir").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chown_to_cell_changes_ownership() {
        // Mutants: chown_to_cell replaced with Ok(()), or the
        // `current == uid` check inverted to `!=`. As root, chown a
        // fresh dir from root to 65534 and verify the uid actually
        // changed. The no-op mutant leaves it at 0; the inverted
        // check returns early when the uids differ, also leaving it
        // at 0. Only meaningful as root.
        use std::os::unix::fs::MetadataExt as _;
        if hatchling_sys::euid() != 0 {
            return;
        }
        let dir = owned_test_dir("chown2");
        std::fs::create_dir_all(&dir).unwrap();
        let target = 65534u32;
        chown_to_cell(&dir, target, "test dir").unwrap();
        let meta = std::fs::symlink_metadata(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            meta.uid(),
            target,
            "chown_to_cell must actually transfer ownership"
        );
        assert_eq!(
            meta.gid(),
            target,
            "chown_to_cell must transfer the group too: the cell's gid map maps only that number"
        );
    }

    #[test]
    fn chown_to_cell_fails_closed_when_chown_impossible() {
        // Mutants: chown_to_cell replaced with Ok(()), or the
        // `current == uid` check inverted to `!=`. A path whose owner
        // differs from the target and cannot be chowned must fail
        // closed, not stay silently foreign-owned. Unprivileged,
        // chowning to any other uid fails with EPERM — no privilege
        // needed to observe the failure path. (As root the chown
        // would succeed; the root test above covers that half.)
        if hatchling_sys::euid() == 0 {
            return;
        }
        let dir = owned_test_dir("chown-fail");
        std::fs::create_dir_all(&dir).unwrap();
        let euid = hatchling_sys::euid();
        // Owned by us, target is a uid we can never chown to: the
        // no-op mutant returns Ok, the inverted check returns early
        // Ok, the real code hits EPERM and fails closed.
        let err = chown_to_cell(&dir, euid.wrapping_add(1), "test dir").unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(err, Error::Spawn { .. }),
            "chown to an unchownable uid must fail closed, got {err:?}"
        );
    }

    fn make_socket_dir(tag: &str) -> OwnedTestDir {
        let dir = owned_test_dir(&format!("sock-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The endpoint `<dir>/broker.sock`, and the principal a default
    /// identity hands it to.
    fn endpoint_in(dir: &Path) -> (Endpoint, u32) {
        let endpoint = Endpoint {
            socket: dir.join("broker.sock"),
            dir: dir.to_path_buf(),
        };
        let principal = super::super::identity::default_principal(hatchling_sys::euid());
        (endpoint, principal)
    }

    #[test]
    fn hand_over_endpoint_accepts_a_real_socket() {
        // Mutants: hand_over_endpoint replaced with Ok(()), the
        // `!is_socket()` check deleted, the dir-mode mask/test
        // operators swapped. A real socket in a traversable dir owned
        // by the map base must pass.
        use std::os::unix::net::UnixListener;
        let dir = make_socket_dir("ok");
        let (endpoint, principal) = endpoint_in(&dir);
        let _listener = UnixListener::bind(&endpoint.socket).unwrap();
        hand_over_endpoint(&endpoint, principal).unwrap();
    }

    #[test]
    fn hand_over_endpoint_rejects_a_non_socket() {
        // Mutant: the `!` deleted from `!is_socket()`. A regular file
        // at the socket path must fail (not silently pass).
        let dir = make_socket_dir("nonsock");
        let (endpoint, principal) = endpoint_in(&dir);
        std::fs::write(&endpoint.socket, b"not a socket").unwrap();
        let result = hand_over_endpoint(&endpoint, principal);
        assert!(
            matches!(&result, Err(Error::Spawn { reason }) if reason.contains("not a socket")),
            "hand_over_endpoint must reject a non-socket, got {result:?}"
        );
    }

    #[test]
    fn hand_over_endpoint_rejects_missing_socket() {
        // Mutant: hand_over_endpoint replaced with Ok(()). A missing
        // socket path must fail closed.
        let dir = make_socket_dir("missing");
        let (endpoint, principal) = endpoint_in(&dir);
        let result = hand_over_endpoint(&endpoint, principal);
        assert!(
            matches!(result, Err(Error::Spawn { .. })),
            "hand_over_endpoint must reject a missing socket"
        );
    }

    #[test]
    fn hand_over_endpoint_rejects_untraversable_dir() {
        // Mutants: `&` -> `|`/`^` in the mode mask, `==` -> `!=` in the
        // traverse check. A dir without owner-traverse must fail even
        // though the socket itself is fine.
        //
        // Privilege note: as root, permission checks are bypassed, so
        // the socket stat succeeds and the explicit traverse-bit check
        // fires ("lacks owner traverse"). As non-root, the stat itself
        // fails with EACCES ("cannot inspect broker socket"). Both are
        // rejections of the untraversable dir; accept either.
        use std::os::unix::net::UnixListener;
        let dir = make_socket_dir("notrav");
        let (endpoint, principal) = endpoint_in(&dir);
        let _listener = UnixListener::bind(&endpoint.socket).unwrap();
        // Remove owner-traverse (and everything else) from the dir.
        std::fs::set_permissions(&*dir, std::fs::Permissions::from_mode(0o600)).unwrap();
        let result = hand_over_endpoint(&endpoint, principal);
        // Restore permissions so cleanup works.
        std::fs::set_permissions(&*dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(&result, Err(Error::Spawn { reason })
                if reason.contains("lacks owner traverse")
                    || reason.contains("cannot inspect broker socket")),
            "hand_over_endpoint must reject an untraversable dir, got {result:?}"
        );
    }

    #[test]
    fn a_cells_wait_returns_the_child_status() {
        // Mutants: the wait replaced with a constant. Fork a child that
        // exits with a distinctive code; wait must return the real
        // status, not a constant.
        let staging =
            std::env::temp_dir().join(format!("hatchling-isolation-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).unwrap();
        let (mut child_to_parent, _parent_to_child) = handshake_pipes().unwrap();
        // Child exits immediately with code 42; the pipe stays open
        // just long enough for wait to read EOF.
        let child = match hatchling_sys::process::fork().unwrap() {
            ForkResult::Parent { child } => {
                // Parent: close our copy of the write end, exactly as
                // isolate does after fork; otherwise read_to_end in
                // wait() never sees EOF.
                child_to_parent.close_write();
                child
            }
            ForkResult::Child => {
                // Child: exit with a distinctive code via the
                // async-signal-safe hatchling-sys wrapper.
                hatchling_sys::exit_child(42);
            }
        };
        let cell = LinuxCell {
            child,
            staging_dir: staging.clone(),
            child_to_parent,
            relay: None,
        };
        // The production wait delegates to `wait_with_deadlines` with
        // the hour-long `CELL_WAIT_DEADLINE` for the child and the 30 s
        // `REPORT_TIMEOUT` for the report read; the child exits
        // immediately here, so neither deadline comes into play.
        // Calling the real wait (not the seam) pins the delegation
        // itself, and the exit code's translation.
        let exit = within_watchdog("LinuxCell::wait", move || cell.wait()).unwrap();
        assert_eq!(exit.code, 42);
        assert_eq!(hatchling_sys::child_exit_code(exit.status), 42);
        assert!(!staging.exists(), "wait must remove the staging dir");
    }

    #[test]
    fn a_cells_wait_reports_child_setup_failure() {
        // Mutant: `report.first() == Some(&b'E')` with == -> !=. A child
        // that writes an E-prefixed failure report must produce
        // Error::Spawn, not Ok.
        let staging = std::env::temp_dir().join(format!(
            "hatchling-isolation-wait-err-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).unwrap();
        let (mut child_to_parent, _parent_to_child) = handshake_pipes().unwrap();
        let child = match hatchling_sys::process::fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // Child: write a failure report on the pipe, then exit.
                // The ChildToParent handle was duplicated across fork;
                // writing here is what the real child_main does.
                let _ = child_to_parent.write_all(b"Etest failure");
                child_to_parent.close_write();
                hatchling_sys::exit_child(1);
            }
        };
        // Parent: close our copy of the write end, exactly as isolate
        // does after fork; otherwise read_to_end in wait() never sees EOF.
        child_to_parent.close_write();
        let cell = LinuxCell {
            child,
            staging_dir: staging,
            child_to_parent,
            relay: None,
        };
        // Child wrote an E-report: wait must surface it as Spawn.
        let result = cell.wait_with_deadlines(SHORT_DEADLINE, SHORT_DEADLINE);
        assert!(
            matches!(&result, Err(Error::Spawn { reason }) if reason == "test failure"),
            "wait must report the child's E-prefixed failure, got {result:?}"
        );
    }

    #[test]
    fn dropped_child_is_killed_and_reaped() {
        // The failure paths rely on `Child`'s drop being kill-and-reap
        // (the drop IS kill-and-reap). A mutant that forgets the child
        // instead of dropping it leaves a live sleeper here.
        let child = fork_sleeper();
        let pid = child.pid().get();
        drop(child);
        // Clean up before asserting: a mutant that breaks kill-on-drop
        // must not leak a sleeper into the suite.
        if std::fs::metadata(format!("/proc/{pid}")).is_ok() {
            cleanup_leaked_child(pid);
        }
        assert_reaped(pid);
    }

    /// Forks a child that sleeps until killed, returning its
    /// supervised handle. The child never touches the pipes.
    fn fork_sleeper() -> Child {
        match hatchling_sys::fork().expect("fork sleeper") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // Die with the test thread that forked us, however the
                // test ends (test code is never mutated, so this holds
                // even when the kill-on-drop under test is broken or the
                // watchdog abandons a hung call before cleanup runs).
                let _ = rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::Kill,
                ));
                // Backstop: never outlive a test run by minutes.
                std::thread::sleep(Duration::from_secs(120));
                hatchling_sys::exit_child(0)
            }
        }
    }

    /// Asserts the process is gone (reaped): `/proc/<pid>` vanishes
    /// only after the zombie is reaped. `Child`'s drop reaps before
    /// it returns, so this is deterministic.
    ///
    /// If the child survived, it is `SIGKILL`ed and reaped FIRST (via
    /// `cleanup_leaked_child`, which deliberately avoids the
    /// production `Child` paths under test), and then the assertion
    /// fails. Cleanup-then-fail, in that order: asserting first would
    /// catch the mutant but leak a sleeper into the suite; cleaning
    /// without failing would hide the mutant entirely.
    fn assert_reaped(pid: i32) {
        let survived = std::fs::metadata(format!("/proc/{pid}")).is_ok();
        if survived {
            cleanup_leaked_child(pid);
        }
        assert!(!survived, "child {pid} was not reaped; kill-on-drop failed");
    }

    /// Best-effort cleanup for a leaked test child: SIGKILL it directly
    /// and reap the zombie, so a mutant that breaks `Child`'s
    /// kill-on-drop cannot leak a sleeper into the suite.
    ///
    /// This deliberately does NOT go through `Child::terminate` or
    /// `Child`'s drop: those are the production paths under test, and
    /// the mutant that leaked the child may have broken them too.
    /// Test code is never mutated, so this direct path stays sound.
    /// `rustix` (a dev-dependency) keeps the helper free of `unsafe`,
    /// which this crate forbids.
    ///
    /// Soundness: the test is the child's parent and has not reaped
    /// it — an unreaped child keeps its pid reserved, so the pid
    /// cannot have been reused by an unrelated process.
    /// (Reconstructing a `Child` from a pid you do NOT own would be
    /// unsafe — that pattern must never leave this test module.)
    fn cleanup_leaked_child(pid: i32) {
        use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};
        if let Some(pid) = Pid::from_raw(pid) {
            // Best-effort: the child may already be gone.
            let _ = kill_process(pid, Signal::Kill);
            // Reap the zombie. Blocking is sound here: the child is a
            // test sleeper in interruptible sleep, so SIGKILL
            // terminates it promptly; if it was already gone, waitpid
            // errors at once.
            let _ = waitpid(Some(pid), WaitOptions::empty());
        }
    }

    #[test]
    fn parent_after_fork_rejects_wrong_ready_byte() {
        // Mutants: parent_after_fork replaced with Ok(child), or the
        // `ready != Some(b'R')` check inverted to `==`. Write a wrong
        // ready byte; the function must fail closed with the
        // "failed before namespaces were ready" error. The Ok(child)
        // mutant would return Ok; the inverted check would proceed
        // past the ready-byte validation (and fail later in
        // write_maps with a different error).
        let (mut c2p, mut p2c) = handshake_pipes().unwrap();
        // Simulate the child writing a wrong ready byte.
        c2p.write_byte(b'X').unwrap();
        // Close the child's write end so the parent's read sees the
        // byte (not EOF from an open-but-idle pipe).
        c2p.close_write();
        // A real supervised child: the failure path must SIGKILL and
        // reap it (no orphans), which a fake pid could never prove.
        let child = fork_sleeper();
        let pid = child.pid().get();
        let uid_map = IdMap::new(65534, 1).unwrap();
        let gid_map = IdMap::new(65534, 1).unwrap();
        let err = within_watchdog("parent_after_fork", move || {
            parent_after_fork(child, uid_map, gid_map, &mut c2p, &mut p2c, SHORT_DEADLINE)
        })
        .expect_err("wrong ready byte must fail");
        assert!(
            matches!(err, Error::Spawn { ref reason } if reason.contains("failed before namespaces were ready")),
            "expected ready-byte failure, got {err:?}"
        );
        assert_reaped(pid);
    }

    #[test]
    fn parent_after_fork_accepts_correct_ready_byte() {
        // Mutant: `ready != Some(b'R')` inverted to `==`. With the
        // correct ready byte, the function must NOT return the
        // "failed before namespaces were ready" error — it must
        // proceed to write_maps (which fails without a user
        // namespace, returning Identity). The inverted check would
        // return the ready-byte error instead.
        let (mut c2p, mut p2c) = handshake_pipes().unwrap();
        c2p.write_byte(b'R').unwrap();
        c2p.close_write();
        // A real child in no user namespace: write_maps fails on it
        // with EPERM, which is exactly the Identity path under test.
        // The failure path must still reap it.
        let child = fork_sleeper();
        let pid = child.pid().get();
        let uid_map = IdMap::new(65534, 1).unwrap();
        let gid_map = IdMap::new(65534, 1).unwrap();
        let err = within_watchdog("parent_after_fork", move || {
            parent_after_fork(child, uid_map, gid_map, &mut c2p, &mut p2c, SHORT_DEADLINE)
        })
        .expect_err("write_maps without userns must fail");
        // Must be an Identity error (from write_maps), NOT the
        // ready-byte error. The inverted `==` mutant would produce
        // the ready-byte error here.
        assert!(
            matches!(err, Error::Identity(_)),
            "correct ready byte must proceed to write_maps (Identity error), got {err:?}"
        );
        assert_reaped(pid);
    }

    #[test]
    fn parent_after_fork_includes_child_error_message() {
        // Mutant: `ready == Some(b'E')` inverted to `!=` in the detail
        // construction. When the child reports 'E' followed by a
        // message, the parent must include that message in the
        // Spawn error. The inverted mutant would skip reading the
        // message (detail empty), losing the diagnostic.
        let (mut c2p, mut p2c) = handshake_pipes().unwrap();
        // Simulate the child failing with 'E' + message.
        c2p.write_byte(b'E').unwrap();
        c2p.write_all(b"unshare(mount): operation not permitted")
            .unwrap();
        c2p.close_write();
        let child = fork_sleeper();
        let pid = child.pid().get();
        let uid_map = IdMap::new(65534, 1).unwrap();
        let gid_map = IdMap::new(65534, 1).unwrap();
        let err = within_watchdog("parent_after_fork", move || {
            parent_after_fork(child, uid_map, gid_map, &mut c2p, &mut p2c, SHORT_DEADLINE)
        })
        .expect_err("'E' from child must fail");
        assert!(
            matches!(err, Error::Spawn { ref reason }
                if reason.contains("failed before namespaces were ready")
                    && reason.contains("unshare(mount): operation not permitted")),
            "child error message must be included in diagnostic, got {err:?}"
        );
        assert_reaped(pid);
    }

    #[test]
    fn release_byte_check_exits_on_wrong_byte() {
        // Mutant: delete `!` in should_exit_on_release (inverts the
        // logic). The correct b'D' must NOT trigger exit; anything
        // else (wrong byte, EOF, I/O error) must. The inverted mutant
        // would return true for b'D' (exit on correct) and false for
        // wrong bytes (proceed on wrong).
        use hatchling_sys::Error as SysError;
        assert!(
            !should_exit_on_release(&Ok(Some(b'D'))),
            "correct release byte b'D' must not trigger exit"
        );
        assert!(
            should_exit_on_release(&Ok(Some(b'X'))),
            "wrong release byte must trigger exit"
        );
        assert!(
            should_exit_on_release(&Ok(None)),
            "EOF on release pipe must trigger exit"
        );
        // I/O error case: construct via a closed pipe read (bounded:
        // the deadline variants are the production paths; the error
        // is immediate here since the read end is closed).
        let (mut c2p, mut p2c) = handshake_pipes().unwrap();
        p2c.close_read();
        // Reading from a pipe with the read end closed gives an error.
        // (We use the ChildToParent end to generate an Err variant.)
        c2p.close_write();
        let err_result: Result<Option<u8>, SysError> =
            c2p.read_byte_deadline(Duration::from_secs(5)).map(|_| None);
        // If read_byte succeeded (unlikely), skip; otherwise verify.
        if let Err(e) = err_result {
            assert!(
                should_exit_on_release(&Err(e)),
                "I/O error on release pipe must trigger exit"
            );
        }
    }

    /// Pins the `+` in [`CELL_WAIT_DEADLINE`]: the `-` mutant (3570s)
    /// fails this assertion. Without the pin, no test observes the 60s
    /// difference between the two deadlines. (3630 = 3600s
    /// [`MAX_CELL_RUNTIME`] + 30s [`OUTER_WAIT_GRACE`]; keep in sync.)
    #[test]
    fn cell_wait_deadline_is_runtime_plus_grace() {
        assert_eq!(CELL_WAIT_DEADLINE, Duration::from_secs(3630));
    }

    /// Fork a dummy child and wrap it in a [`LinuxCell`] — no cell
    /// pipeline needed. Returns the child's pid, the cell, and the
    /// staging dir. The child arm either exits at once or sleeps so the
    /// parent can drop the cell under a live child.
    fn dummy_cell(sleeper: bool) -> (i32, LinuxCell, PathBuf) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let staging = std::env::temp_dir().join(format!(
            "hatchling-isolation-cell-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&staging).unwrap();
        match hatchling_sys::fork().unwrap() {
            ForkResult::Parent { child } => {
                let pid = child.pid().get();
                let (c2p, _p2c) = handshake_pipes().unwrap();
                let cell = LinuxCell {
                    child,
                    staging_dir: staging.clone(),
                    child_to_parent: c2p,
                    relay: None,
                };
                (pid, cell, staging)
            }
            ForkResult::Child => {
                if sleeper {
                    std::thread::sleep(Duration::from_secs(15));
                }
                hatchling_sys::exit_child(0)
            }
        }
    }

    /// Pins `pid()`: the `-> 0/1/-1` mutants fail the assertion.
    #[test]
    fn a_cells_pid_is_its_childs() {
        let (pid, cell, _staging) = dummy_cell(false);
        assert!(pid > 1);
        assert_eq!(cell.pid(), pid);
        // Drop reaps the exited dummy and removes the staging dir.
        drop(cell);
    }

    /// Pins `Drop for LinuxCell`: the `-> ()` mutant skips the
    /// staging-dir removal (the child is still reaped by `Child`'s own
    /// drop, so the dir assertion is the killer).
    #[test]
    fn drop_kills_child_and_removes_staging() {
        let (pid, cell, staging) = dummy_cell(true);
        assert!(staging.exists());
        drop(cell);
        // The drop SIGKILLs and reaps the live child: its /proc entry
        // must vanish (a zombie would still have one).
        let proc_path = PathBuf::from(format!("/proc/{pid}"));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while proc_path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "drop did not kill the cell child"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!staging.exists(), "drop did not remove the staging dir");
    }

    #[test]
    fn only_a_work_tree_program_runs_under_the_harness_profile() {
        let agent = SeccompProfile::agent().unwrap().allowed_numbers();
        assert_eq!(
            seccomp_profile(Filter::WorkTree).unwrap().allowed_numbers(),
            SeccompProfile::harness().unwrap().allowed_numbers()
        );
        assert_eq!(
            seccomp_profile(Filter::Program).unwrap().allowed_numbers(),
            agent
        );
        #[cfg(target_arch = "x86_64")]
        assert_ne!(
            seccomp_profile(Filter::WorkTree).unwrap().allowed_numbers(),
            agent,
            "the harness profile is wider than the agent's"
        );
    }

    /// The limits the tests below hold a forked child to.
    const HELD: Limits = Limits {
        processes: Some(100),
        address_space: Some(1 << 40),
        file_size: Some(1 << 30),
        open_files: Some(64),
    };

    /// Whether this process is held to exactly `limits`, soft and hard,
    /// each limit it leaves `None` as `before` was.
    fn held_to(limits: &Limits, before: &[rustix::process::Rlimit; 4]) -> bool {
        use rustix::process::{Resource, getrlimit};
        [
            (Resource::Nproc, limits.processes),
            (Resource::As, limits.address_space),
            (Resource::Fsize, limits.file_size),
            (Resource::Nofile, limits.open_files),
        ]
        .iter()
        .zip(before)
        .all(|(&(resource, value), before)| {
            let held = getrlimit(resource);
            value.map_or(held == *before, |value| {
                (held.current, held.maximum) == (Some(value), Some(value))
            })
        })
    }

    /// This process's four limits, in [`held_to`]'s order.
    fn limits_now() -> [rustix::process::Rlimit; 4] {
        use rustix::process::{Resource, getrlimit};
        [
            getrlimit(Resource::Nproc),
            getrlimit(Resource::As),
            getrlimit(Resource::Fsize),
            getrlimit(Resource::Nofile),
        ]
    }

    /// Runs `child` in a forked child and asserts it ended with 0;
    /// `codes` says what its other exit codes mean.
    fn in_child(codes: &str, child: impl FnOnce() -> i32) {
        match hatchling_sys::fork().expect("fork") {
            ForkResult::Parent { child } => {
                let status = child.wait_deadline(Duration::from_secs(5)).expect("child");
                assert_eq!(hatchling_sys::child_exit_code(status), 0, "{codes}");
            }
            ForkResult::Child => {
                let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(child));
                hatchling_sys::exit_child(code.unwrap_or(3));
            }
        }
    }

    #[test]
    fn hold_to_limits_sets_the_limits_given_and_leaves_the_rest() {
        in_child("1: refused; 2: not held as given", || {
            let before = limits_now();
            let some = Limits {
                open_files: Some(32),
                ..Limits::default()
            };
            if hold_to_limits(&some).is_err() {
                return 1;
            }
            if !held_to(&some, &before) {
                return 2;
            }
            if hold_to_limits(&HELD).is_err() {
                return 1;
            }
            // Open files stays at 32: a limit is never raised.
            let raised = Limits {
                open_files: Some(32),
                ..HELD
            };
            if held_to(&raised, &before) { 0 } else { 2 }
        });
    }

    #[test]
    fn exec_program_holds_the_program_to_its_limits_and_fails_outside_a_cell() {
        // Outside a cell nothing is mounted at /agent: the exec fails and
        // returns its error, the process image untouched, after the
        // limits were set.
        if Path::new(PROGRAM).exists() {
            return;
        }
        let spec = CellSpec {
            identity: CellIdentity {
                principal: hatchling_sys::euid(),
                count: 1,
            },
            program: PathBuf::from("/bin/true"),
            argv: vec![String::from(PROGRAM)],
            env: vec![(String::from("K"), String::from("V"))],
            workdir: None,
            endpoint: None,
            exposures: Vec::new(),
            tools: Vec::new(),
            relay: None,
            filter: Filter::Program,
            limits: HELD,
        };
        in_child("1: no error; 2: not held to its limits", || {
            let before = limits_now();
            if exec_program(&spec).to_string().is_empty() {
                return 1;
            }
            if held_to(&HELD, &before) { 0 } else { 2 }
        });
    }

    /// A real cell, end to end: the program runs isolated and its exit
    /// code reaches the owner; a failing exec is reported, not hidden.
    /// A field of this process's `/proc/self/status`, its value trimmed.
    fn status(field: &str) -> Option<String> {
        std::fs::read_to_string("/proc/self/status")
            .ok()?
            .lines()
            .find_map(|line| {
                line.strip_prefix(&format!("{field}:"))
                    .map(str::trim)
                    .map(String::from)
            })
    }

    /// In a forked child: a user namespace of its own, its uid mapped to
    /// 0, as a cell's processes have. Whether that worked.
    fn own_user_namespace() -> bool {
        let euid = hatchling_sys::euid();
        let Ok(pid) = i32::try_from(std::process::id()) else {
            return false;
        };
        hatchling_sys::unshare_user().is_ok()
            && IdMap::new(euid, 1)
                .is_ok_and(|map| hatchling_sys::idmap::write_maps(pid, &map, &map).is_ok())
    }

    #[test]
    fn a_relay_listener_arrives_only_as_a_listening_socket_in_time() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        hatchling_sys::net::send_fd(theirs.as_fd(), listener.as_fd()).unwrap();
        let received = receive_relay(&ours, SHORT_DEADLINE).unwrap();
        assert_eq!(received.local_addr().unwrap().port(), port);
        let connected = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        hatchling_sys::net::send_fd(theirs.as_fd(), connected.as_fd()).unwrap();
        let refused = receive_relay(&ours, SHORT_DEADLINE);
        assert!(
            matches!(&refused, Err(Error::Spawn { reason })
                if reason == "the cell's relay listener did not arrive: what arrived is not a listening socket"),
            "{refused:?}"
        );
        let started = std::time::Instant::now();
        let late = receive_relay(&ours, Duration::from_millis(200));
        assert!(
            matches!(&late, Err(Error::Spawn { reason })
                if reason.starts_with("the cell's relay listener did not arrive: ")),
            "{late:?}"
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(started.elapsed() < SHORT_DEADLINE);
    }

    #[test]
    fn a_spawn_failure_carries_what_the_child_reported() {
        let spawn = || Error::Spawn {
            reason: String::from("no relay"),
        };
        assert!(matches!(
            with_report(spawn(), b"Ebind failed"),
            Error::Spawn { reason } if reason == "no relay; the child reported bind failed"
        ));
        for report in [&b""[..], b"R", b"Xoops"] {
            assert!(matches!(
                with_report(spawn(), report),
                Error::Spawn { reason } if reason == "no relay"
            ));
        }
        let waited = Error::Wait(PlatformError::new(hatchling_sys::Error::Net {
            operation: "receive fd",
            source: std::io::Error::other("gone"),
        }));
        assert!(matches!(with_report(waited, b"Eboom"), Error::Wait(_)));
    }

    #[test]
    fn the_reaper_settles_as_the_cells_user_holding_no_capability() {
        if !crate::linux::cell_available() {
            return;
        }
        let broker = hatchling_sys::self_pidfd().unwrap();
        let child = match hatchling_sys::fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // Outside a namespace of its own, an unprivileged process
                // cannot become uid 0.
                let refused = hatchling_sys::euid() == 0
                    || settle_reaper(broker.try_clone().unwrap())
                        .is_err_and(|reason| reason.starts_with("become cell user: "));
                let settled = own_user_namespace()
                    && settle_reaper(broker).is_ok()
                    && status("Uid").as_deref() == Some("0\t0\t0\t0")
                    && status("CapEff").as_deref() == Some("0000000000000000")
                    && status("CapPrm").as_deref() == Some("0000000000000000");
                hatchling_sys::exit_child(match (refused, settled) {
                    (true, true) => 0,
                    (false, _) => 1,
                    (true, false) => 2,
                });
            }
        };
        let status = child.wait_deadline(SHORT_DEADLINE).unwrap();
        assert_eq!(hatchling_sys::child_exit_code(status), 0);
    }

    #[test]
    fn a_cell_hands_out_one_listener_on_its_own_loopback() {
        if !crate::linux::cell_available() {
            return;
        }
        let (ours, theirs) = UnixStream::pair().unwrap();
        let child = match hatchling_sys::fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                drop(ours);
                let handed = own_user_namespace()
                    && hatchling_sys::unshare_net().is_ok()
                    && hand_out_relay(8787, &theirs).is_ok();
                hatchling_sys::exit_child(i32::from(!handed));
            }
        };
        drop(theirs);
        let listener = receive_relay(&ours, SHORT_DEADLINE).unwrap();
        assert_eq!(
            listener.local_addr().unwrap(),
            std::net::SocketAddr::from(([127, 0, 0, 1], 8787))
        );
        let status = child.wait_deadline(SHORT_DEADLINE).unwrap();
        assert_eq!(hatchling_sys::child_exit_code(status), 0);
    }

    #[test]
    fn a_cli_cell_hands_its_relay_listener_to_its_owner_once() {
        if !crate::linux::cell_available() {
            return;
        }
        let scratch = Scratch::new("relay");
        let (mut spec, _) = specs(&scratch);
        spec.filter = Filter::Cli;
        spec.relay = Some(8787);
        let mut cell = isolate(&spec).unwrap();
        let listener = cell.relay().unwrap();
        assert_eq!(listener.local_addr().unwrap().port(), 8787);
        assert!(cell.relay().is_none(), "handed over once");
        let exit = within_watchdog("relay cell", move || cell.wait());
        assert_eq!(exit.unwrap().code, 0);
        let (plain, _) = specs(&scratch);
        let mut cell = isolate(&plain).unwrap();
        assert!(cell.relay().is_none(), "a cell without a relay");
        let exit = within_watchdog("plain cell", move || cell.wait());
        assert_eq!(exit.unwrap().code, 0);
    }

    #[test]
    fn a_real_cell_runs_its_program_and_reports_how_it_ended() {
        if !crate::linux::cell_available() {
            return;
        }
        let scratch = Scratch::new("isolate");
        let (ok, _) = specs(&scratch);
        let exit = within_watchdog("isolate /bin/true", move || {
            isolate(&ok).and_then(Cell::wait)
        });
        assert_eq!(exit.unwrap().code, 0);
        let mut failing = specs(&scratch).0;
        failing.program = crate::linux::false_witness();
        let exit = within_watchdog("isolate /bin/false", move || {
            isolate(&failing).and_then(Cell::wait)
        });
        assert_eq!(exit.unwrap().code, 1);
        // A work directory that does not exist in the cell is a setup
        // failure, reported by name: the program never runs.
        let mut lost = specs(&scratch).0;
        lost.workdir = Some(String::from("/nowhere"));
        let result = within_watchdog("isolate lost", move || isolate(&lost).and_then(Cell::wait));
        assert!(
            matches!(&result, Err(Error::Spawn { reason }) if reason.starts_with("chdir(/nowhere)")),
            "got {result:?}"
        );
    }

    /// In a forked child, as the reaper is: after the close, only stdio,
    /// the broker's pidfd, the agent's and the report pipe are open; a
    /// pipe it does not use is closed.
    #[test]
    fn the_reaper_keeps_only_the_broker_the_agent_and_the_report_pipe() {
        use std::os::fd::AsRawFd as _;
        let child = match hatchling_sys::fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let broker = hatchling_sys::self_pidfd().unwrap();
                let (report, _unused) = handshake_pipes().unwrap();
                let agent = match hatchling_sys::fork().unwrap() {
                    ForkResult::Parent { child } => child,
                    ForkResult::Child => hatchling_sys::exit_child(0),
                };
                let mut expected: Vec<i32> = [broker.as_raw_fd(), agent.as_fd().as_raw_fd()]
                    .into_iter()
                    .chain(report.fds().map(|fd| fd.as_raw_fd()))
                    .chain(0..3)
                    .collect();
                expected.sort_unstable();
                let kept = reaper_keeps_only(&broker, &agent, &report).is_ok()
                    && hatchling_sys::open_fds(1024) == expected;
                let _ = agent.wait_deadline(SHORT_DEADLINE);
                hatchling_sys::exit_child(i32::from(!kept));
            }
        };
        let status = child.wait_deadline(SHORT_DEADLINE).unwrap();
        assert_eq!(hatchling_sys::child_exit_code(status), 0);
    }

    /// The reaper never execs, so it must close what it inherited: a
    /// socket the owner held when the cell was spawned is closed, end to
    /// end, once the owner drops it, while the cell still runs.
    #[test]
    fn a_cells_reaper_keeps_none_of_its_owners_descriptors() {
        use std::io::Read as _;
        if !crate::linux::cell_available() {
            return;
        }
        let scratch = Scratch::new("reaper-fds");
        let (mut spec, _) = specs(&scratch);
        // A cell that stays up: GNU sleep (a multi-call coreutils picks
        // its tool by argv[0], which is the cell's program path), under
        // the CLI filter (the agent's refuses one of its calls: SIGSYS).
        spec.program = ["/usr/bin/gnusleep", "/bin/sleep"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.exists())
            .unwrap();
        spec.filter = Filter::Cli;
        spec.argv = vec![String::from(PROGRAM), String::from("60")];
        let (ours, theirs) = UnixStream::pair().unwrap();
        let cell = isolate(&spec).unwrap();
        drop(theirs);
        // EOF as soon as no process holds the other end; with the
        // reaper's copy it would come only when the cell ends.
        ours.set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let read = (&ours).read(&mut [0_u8; 1]);
        // A grace for a cell that died instead of sleeping to show as a
        // zombie, so its exit can never pass for the reaper's close.
        std::thread::sleep(Duration::from_millis(100));
        let running =
            std::fs::read_to_string(format!("/proc/{}/stat", cell.pid())).is_ok_and(|stat| {
                stat.rsplit(')')
                    .next()
                    .is_some_and(|rest| !rest.trim_start().starts_with('Z'))
            });
        drop(cell);
        assert!(
            matches!(read, Ok(0)),
            "{read:?}: the reaper holds the owner's socket"
        );
        assert!(
            running,
            "the cell had already ended, so the EOF proves nothing"
        );
    }

    #[test]
    fn a_cell_with_a_missing_endpoint_is_never_built() {
        let scratch = Scratch::new("no-endpoint");
        let (mut spec, _) = specs(&scratch);
        spec.endpoint = Some(Endpoint {
            socket: scratch.0.join("sock").join("absent.sock"),
            dir: scratch.0.join("sock"),
        });
        let result = isolate(&spec);
        assert!(
            matches!(&result, Err(Error::Spawn { reason }) if reason.starts_with("cannot inspect broker socket ")),
            "got {:?}",
            result.map(|cell| cell.pid())
        );
    }
}
