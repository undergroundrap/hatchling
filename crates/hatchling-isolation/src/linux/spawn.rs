//! Building and running a cell on Linux: the endpoint handover, the
//! staging tree, the fork and its handshake, the in-cell setup, and the
//! cell's supervision.

use std::fmt::Write as _;
use std::io::Read as _;
use std::net::TcpListener;
use std::os::fd::{AsFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use hatchling_sys::{
    Child, ChildToParent, ForkResult, IdMap, MountPlan, ParentToChild, SeccompProfile,
    handshake_pipes,
};

use super::identity::id_map;
use super::view::{mount_plan, view};
use crate::{
    CONNECTION_FD_VAR, CellSpec, Endpoint, Error, Exit, Filter, Limits, PROGRAM, PlatformError,
};

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
/// socket becomes the principal's, and its directory, which stays the
/// broker's, lets the principal traverse it and nothing more.
///
/// Inside the cell the agent is uid 0, which the kernel maps to the
/// principal on the host. The socket directory is bind-mounted
/// read-only into the cell, so host ownership and mode bits are what
/// the agent faces: a root-owned 0600 socket (the default) would reject
/// the agent's `connect` before `SO_PEERCRED` ever runs. Chowning the
/// socket (not chmodding it world-writable) keeps the peer gate as the
/// enforcement point.
///
/// The directory isn't given away: a cell's user that owned it could
/// swap the socket for a symlink between spawns, and send the next
/// handover to a file the link names. Both steps go through handles
/// opened without following symlinks
/// ([`hatchling_sys::share_dir_with_cell`],
/// [`hatchling_sys::hand_over_socket`]); for an unprivileged broker,
/// whose cells run as its own user, neither changes anything.
///
/// # Errors
///
/// Returns [`Error::Spawn`] if the directory or the socket isn't what it
/// should be, or can't be changed. Fails closed: a cell whose agent
/// cannot reach the broker is never spawned.
fn hand_over_endpoint(endpoint: &Endpoint, uid: u32) -> Result<(), Error> {
    // The group goes too: the cell's gid map maps the same number
    // (`id_map(principal)` for both maps).
    hatchling_sys::hand_over_socket(&endpoint.socket, uid, uid).map_err(|source| Error::Spawn {
        reason: format!(
            "cannot hand the broker socket {} to the cell (uid {uid}): {source}              (run the broker with privilege to transfer ownership, or chown it beforehand)",
            endpoint.socket.display()
        ),
    })?;
    hatchling_sys::share_dir_with_cell(&endpoint.dir, uid).map_err(|source| Error::Spawn {
        reason: format!(
            "cannot let the cell (uid {uid}) reach the broker socket dir {}: {source}",
            endpoint.dir.display()
        ),
    })
}

/// A cell's staging directory, where its mount tree is built before
/// `pivot_root`.
///
/// It is made exclusively, with a name no one can predict, inside this
/// process's private staging root (see [`StagingRoot`]), so another local
/// user can neither pre-create it to block or redirect a spawn nor reach
/// into it. Dropping it removes the directory and releases its share of
/// the root. [`isolate`] creates it before the fork, so a failed handshake
/// removes it; [`LinuxCell`] takes it over after.
#[derive(Debug)]
struct Staging {
    dir: PathBuf,
}

impl Staging {
    /// Makes a new staging directory in the staging root.
    fn create() -> Result<Self, Error> {
        let root = StagingRoot::enter()?;
        create_unique(&root, "cell-", random_name).map_or_else(
            |error| {
                StagingRoot::leave();
                Err(error)
            },
            |dir| Ok(Self { dir }),
        )
    }

    fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        StagingRoot::leave();
    }
}

/// This process's staging root: one directory in the temp dir, made
/// exclusively with mode 0700 and a name no one can predict, shared by
/// the process's live cells and removed when the last one is gone. While
/// any is live, it is held ([`hatchling_sys::hold_dir`]): a broker's
/// sweep of a dead broker's leftovers leaves a held root alone (WO-012).
#[derive(Debug)]
struct StagingRoot {
    path: Option<PathBuf>,
    held: Option<hatchling_sys::HeldLock>,
    cells: usize,
}

static STAGING_ROOT: Mutex<StagingRoot> = Mutex::new(StagingRoot {
    path: None,
    held: None,
    cells: 0,
});

impl StagingRoot {
    /// The process's root, made now in the temp dir if no live cell holds
    /// it, for one more cell.
    fn enter() -> Result<PathBuf, Error> {
        Self::enter_in(&STAGING_ROOT, &std::env::temp_dir())
    }

    /// One cell fewer; the last one out removes the root.
    fn leave() {
        Self::leave_in(&STAGING_ROOT);
    }

    /// [`StagingRoot::enter`] on `state`, making the root in `parent`.
    fn enter_in(state: &Mutex<Self>, parent: &Path) -> Result<PathBuf, Error> {
        let mut root = state.lock().unwrap_or_else(PoisonError::into_inner);
        let path = if let Some(path) = &root.path {
            path.clone()
        } else {
            let prefix = format!("hatchling-staging-{}-", std::process::id());
            let path = create_unique(parent, &prefix, random_name)?;
            let held = hatchling_sys::hold_dir(&path).map_err(|source| {
                let _ = std::fs::remove_dir(&path);
                Error::Spawn {
                    reason: format!("cannot hold staging root {}: {source}", path.display()),
                }
            })?;
            root.path = Some(path.clone());
            root.held = Some(held);
            path
        };
        root.cells += 1;
        drop(root);
        Ok(path)
    }

    /// [`StagingRoot::leave`] on `state`.
    fn leave_in(state: &Mutex<Self>) {
        let mut root = state.lock().unwrap_or_else(PoisonError::into_inner);
        root.cells = root.cells.saturating_sub(1);
        if root.cells == 0 {
            if let Some(path) = root.path.take() {
                let _ = std::fs::remove_dir(path);
            }
            root.held = None;
        }
    }
}

/// How many names [`create_unique`] tries before giving up.
const STAGING_NAME_ATTEMPTS: usize = 8;

/// Makes a private directory `prefix` + a name from `name` in `parent`
/// ([`hatchling_sys::create_private_dir`]), trying a fresh name while one
/// is already taken: a planted directory or symlink is skipped, never
/// used or followed.
fn create_unique(
    parent: &Path,
    prefix: &str,
    mut name: impl FnMut() -> Result<String, Error>,
) -> Result<PathBuf, Error> {
    for _ in 0..STAGING_NAME_ATTEMPTS {
        let dir = parent.join(format!("{prefix}{}", name()?));
        match hatchling_sys::create_private_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(hatchling_sys::Error::Ownership { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(Error::Spawn {
                    reason: format!("cannot create staging dir {}: {source}", dir.display()),
                });
            }
        }
    }
    Err(Error::Spawn {
        reason: format!("every staging name tried in {} was taken", parent.display()),
    })
}

/// 128 bits from the kernel's random source, as hex: a name no one can
/// guess.
fn random_name() -> Result<String, Error> {
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|source| Error::Spawn {
            reason: format!("cannot read /dev/urandom for a staging name: {source}"),
        })?;
    Ok(bytes
        .iter()
        .fold(String::with_capacity(32), |mut name, byte| {
            let _ = write!(name, "{byte:02x}");
            name
        }))
}

/// A spawned cell: the intermediate child under supervision and the
/// resources the reaper needs.
///
/// The child is an owned [`Child`]: pidfd-pinned signalling, bounded
/// waits, and kill-on-drop. Dropping the cell without waiting
/// SIGKILLs it — no orphans — and removes the staging directory
/// best-effort.
///
/// Each field cleans itself up when dropped, and fields drop in the
/// order they're declared, which is the order a cell abandoned without a
/// wait must be taken down in: the child is killed and reaped, then the
/// cgroup is killed and drained, and only then is the staging directory,
/// which holds the cell's mount tree, removed.
#[derive(Debug)]
pub struct LinuxCell {
    /// The cell child (the spawner's direct child: setup, then the
    /// agent's reaper). Owned supervision: kill-on-drop, bounded wait.
    child: Child,
    /// Owned resource leaf; kills all remaining tasks on every exit path.
    cgroup: Option<hatchling_sys::cgroup::CellCgroup>,
    /// Staging directory holding the cell's mount tree; removed on
    /// wait, or on drop after the child and its cgroup.
    staging: Option<Staging>,
    /// The child->parent pipe: the child reports setup failures here
    /// before the agent image replaces it.
    child_to_parent: ChildToParent,
    /// The cell's relay listener, until the owner takes it.
    relay: Option<TcpListener>,
    /// The owner's end of the cell's connection, until the owner takes it.
    connection: Option<UnixStream>,
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

    fn connection(&mut self) -> Option<UnixStream> {
        self.connection.take()
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
        if let Some(group) = &mut self.cgroup {
            group
                .finish()
                .map_err(|source| Error::Wait(PlatformError::new(source)))?;
        }
        // The pipe reaches EOF once every child has exited or exec'd
        // (CLOEXEC); anything written before that is a setup failure
        // report. Bounded: a leaked writer cannot hang the broker.
        let report = self
            .child_to_parent
            .read_to_end_deadline(report_deadline)
            .map_err(|source| Error::Wait(PlatformError::new(source)))?;
        self.child_to_parent.close_read();
        drop(self.staging.take());
        if report.first() == Some(&b'E') {
            let message: Vec<u8> = report.iter().skip(1).copied().collect();
            let reason = String::from_utf8_lossy(&message).into_owned();
            return Err(Error::Spawn { reason });
        }
        Ok(status)
    }
}

/// Spawns the cell and execs the spec's program inside it.
///
/// On success the program runs as PID 2 under Hatchling's init in its pid namespace,
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
#[allow(
    clippy::too_many_lines,
    reason = "resource configuration and the fork handshake are audited together in fail-closed order"
)]
pub(super) fn isolate(spec: &CellSpec) -> Result<LinuxCell, Error> {
    // Limits precede fork and placement; the child cannot execute or fork
    // until the parent places it and sends the release byte.
    let cgroup = spec
        .cgroup
        .as_ref()
        .map(|parent| {
            hatchling_sys::cgroup::CellCgroup::create(
                parent,
                hatchling_sys::cgroup::CgroupLimits {
                    processes: spec.limits.processes,
                    memory: spec.limits.memory,
                    cpu_millis: spec.limits.cpu_millis,
                },
            )
        })
        .transpose()
        .map_err(|source| Error::Start(PlatformError::new(source)))?;
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
    let staging = Staging::create()?;
    let new_root = staging.path().join("root");
    std::fs::create_dir(&new_root).map_err(|source| Error::Spawn {
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
    // The cell's connection: the program gets one end, the owner the
    // other, and no one else either.
    let connection = if spec.connection {
        Some(UnixStream::pair().map_err(|source| Error::Spawn {
            reason: format!("cannot create the cell's connection: {source}"),
        })?)
    } else {
        None
    };

    match hatchling_sys::fork().map_err(|source| Error::Start(PlatformError::new(source)))? {
        ForkResult::Parent { child } => {
            // From here on the staging directory must not leak: dropped on
            // any failed parent-side handshake step, it removes itself.
            // Only the child sends on its end; holding it here would keep
            // the receive below from ever seeing the child give up.
            let relay = relay.map(|(_, (ours, theirs))| {
                drop(theirs);
                ours
            });
            // The program's end is the child's alone: a copy here would
            // keep the connection open past the program.
            let connection = connection.map(|(ours, theirs)| {
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
                cgroup.as_ref(),
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
            Ok(LinuxCell {
                child,
                staging: Some(staging),
                child_to_parent,
                relay,
                connection,
                cgroup,
            })
        }
        ForkResult::Child => {
            // The staging directory is the parent's to remove; the child
            // only builds its mount tree there.
            let _staging = std::mem::ManuallyDrop::new(staging);
            let relay = relay.map(|(port, (ours, theirs))| {
                drop(ours);
                (port, theirs)
            });
            let connection = connection.map(|(ours, theirs)| {
                drop(ours);
                theirs
            });
            child_main(
                spec,
                &plan,
                broker_pidfd,
                (child_to_parent, parent_to_child),
                relay,
                connection,
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
    cgroup: Option<&hatchling_sys::cgroup::CellCgroup>,
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
    if let Some(group) = cgroup {
        group
            .place(child.pid().get())
            .map_err(|source| Error::Start(PlatformError::new(source)))?;
    }
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
    connection: Option<UnixStream>,
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
    // A seccomp kill dumps core: held to one byte, no core reaches the
    // host's crash handler, from every process of the cell. Set here,
    // before the user namespace, where a root broker may still raise a
    // hard limit of 0 (sudo's) to it; the cell's filter refuses any
    // change to it later.
    if let Err(source) = hatchling_sys::hold_core_dumps_off() {
        fail(
            &mut child_to_parent,
            format!(
                "hold core dumps off (a hard core limit of 0 can't be raised to the 1 that stops a crash handler without root): {source}"
            ),
        );
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
            // The connection is the program's: the reaper keeps no end.
            drop(connection);
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
            run_as_init(spec, child_to_parent, connection)
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
fn run_as_init(
    spec: &CellSpec,
    mut child_to_parent: ChildToParent,
    connection: Option<UnixStream>,
) -> ! {
    let program = match hatchling_sys::fork() {
        Ok(ForkResult::Child) => exec_cell_program(spec, child_to_parent, connection),
        Ok(ForkResult::Parent { child }) => {
            // The connection is the program's: the init keeps no end, and
            // drops it before the close below can close it under its handle.
            drop(connection);
            child
        }
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
fn exec_cell_program(
    spec: &CellSpec,
    mut child_to_parent: ChildToParent,
    connection: Option<UnixStream>,
) -> ! {
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
    // Except its connection, which the program is meant to have.
    let connection = connection.map(|connection| {
        if let Err(source) = hatchling_sys::keep_across_exec(connection.as_fd()) {
            fail(
                &mut child_to_parent,
                format!("keep the connection: {source}"),
            );
        }
        std::os::fd::IntoRawFd::into_raw_fd(connection)
    });
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
    let err = exec_program(spec, connection);
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
fn exec_program(spec: &CellSpec, connection: Option<RawFd>) -> hatchling_sys::Error {
    let argv: Vec<&str> = spec.argv.iter().map(String::as_str).collect();
    let connection = connection.map(|fd| fd.to_string());
    let env = program_env(&spec.env, connection.as_deref());
    hatchling_sys::exec(Path::new(PROGRAM), &argv, &env, || {
        hold_to_limits(&spec.limits, spec.cgroup.is_some())
    })
}

/// The program's environment: the spec's, with [`CONNECTION_FD_VAR`]
/// naming `connection` when there is one. The spec's own value for that
/// variable is dropped either way: only the spawn says where a
/// connection is.
fn program_env<'a>(
    env: &'a [(String, String)],
    connection: Option<&'a str>,
) -> Vec<(&'a str, &'a str)> {
    env.iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .filter(|(key, _)| *key != CONNECTION_FD_VAR)
        .chain(connection.map(|fd| (CONNECTION_FD_VAR, fd)))
        .collect()
}

/// Holds this process to each limit `limits` sets; one it leaves `None`
/// stays as it was.
///
/// # Errors
///
/// The first limit the kernel refuses.
fn hold_to_limits(limits: &Limits, grouped: bool) -> Result<(), hatchling_sys::Error> {
    [
        (
            hatchling_sys::Limit::Processes,
            if grouped { None } else { limits.processes },
        ),
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
    use std::os::unix::fs::PermissionsExt as _;

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

    /// A fresh, empty scratch directory for one staging test.
    fn staging_scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-isolation-staging-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    /// Names from `names`, in order, as [`create_unique`] asks for them.
    fn names_from(names: &[&str]) -> impl FnMut() -> Result<String, Error> {
        let mut names: Vec<String> = names.iter().rev().map(|name| (*name).to_owned()).collect();
        move || Ok(names.pop().unwrap_or_else(|| String::from("exhausted")))
    }

    #[test]
    fn a_planted_staging_name_is_skipped_never_used_or_followed() {
        // Another user's directory, or a symlink to somewhere the
        // broker can write, planted at the name a spawn would use.
        let scratch = staging_scratch("planted");
        let elsewhere = scratch.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::create_dir(scratch.join("cell-taken")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, scratch.join("cell-link")).unwrap();
        let made =
            create_unique(&scratch, "cell-", names_from(&["taken", "link", "fresh"])).unwrap();
        assert_eq!(made, scratch.join("cell-fresh"));
        let perms = std::fs::metadata(&made).unwrap().permissions().mode();
        assert_eq!(perms & 0o777, 0o700);
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(scratch.join("cell-taken"))
                .unwrap()
                .count(),
            0
        );
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn creating_a_staging_dir_gives_up_after_its_attempts() {
        let scratch = staging_scratch("give-up");
        std::fs::create_dir(scratch.join("cell-same")).unwrap();
        let mut asked = 0;
        let result = create_unique(&scratch, "cell-", || {
            asked += 1;
            Ok(String::from("same"))
        });
        assert!(
            matches!(&result, Err(Error::Spawn { reason }) if reason.starts_with("every staging name tried")),
            "{result:?}"
        );
        assert_eq!(asked, STAGING_NAME_ATTEMPTS);
        // Any other failure is reported at once.
        let missing = scratch.join("missing");
        let result = create_unique(&missing, "cell-", names_from(&["a"]));
        assert!(
            matches!(&result, Err(Error::Spawn { reason }) if reason.starts_with("cannot create staging dir")),
            "{result:?}"
        );
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn staging_names_are_unguessable() {
        let first = random_name().unwrap();
        let second = random_name().unwrap();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn the_staging_root_is_shared_and_removed_by_the_last_cell_out() {
        let scratch = staging_scratch("root");
        let state = Mutex::new(StagingRoot {
            path: None,
            held: None,
            cells: 0,
        });
        let first = StagingRoot::enter_in(&state, &scratch).unwrap();
        let second = StagingRoot::enter_in(&state, &scratch).unwrap();
        assert_eq!(first, second, "one root for every live cell");
        assert_eq!(first.parent(), Some(scratch.as_path()));
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(&format!("hatchling-staging-{}-", std::process::id())));
        let mode = std::fs::metadata(&first).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        assert!(hatchling_sys::in_use(&first), "held while a cell uses it");
        StagingRoot::leave_in(&state);
        assert!(first.exists(), "a live cell still holds it");
        assert!(hatchling_sys::in_use(&first));
        StagingRoot::leave_in(&state);
        assert!(!first.exists(), "the last cell out removes it");
        assert!(state.lock().unwrap().held.is_none(), "and lets it go");
        let third = StagingRoot::enter_in(&state, &scratch).unwrap();
        assert_ne!(third, first, "a new root, with a new name");
        StagingRoot::leave_in(&state);
        StagingRoot::leave_in(&state);
        assert!(!third.exists());
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn the_process_staging_root_lives_in_the_temp_dir_until_its_last_user_leaves() {
        // Other tests' cells share this root, so it may outlive this
        // test's hold for a moment; it must go once they're done.
        let root = StagingRoot::enter().unwrap();
        assert_eq!(root.parent(), Some(std::env::temp_dir().as_path()));
        assert!(root.is_dir());
        let staging = Staging::create().unwrap();
        assert_eq!(staging.path().parent(), Some(root.as_path()));
        drop(staging);
        assert!(root.is_dir(), "this test still holds it");
        StagingRoot::leave();
        let gone = (0..600).any(|_| {
            let gone = !root.exists();
            if !gone {
                std::thread::sleep(Duration::from_millis(50));
            }
            gone
        });
        assert!(gone, "the last user out removes the root");
    }

    #[test]
    fn a_staging_dir_is_private_and_removed_on_drop() {
        let staging = Staging::create().unwrap();
        let dir = staging.path().to_path_buf();
        let root = dir.parent().unwrap().to_path_buf();
        assert!(
            dir.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("cell-")
        );
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("hatchling-staging-")
        );
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        drop(staging);
        assert!(!dir.exists());
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
        // Mutant: hand_over_endpoint replaced with an error. A real socket
        // in a traversable dir owned by the map base must pass.
        use std::os::unix::net::UnixListener;
        let dir = make_socket_dir("ok");
        let (endpoint, principal) = endpoint_in(&dir);
        let _listener = UnixListener::bind(&endpoint.socket).unwrap();
        hand_over_endpoint(&endpoint, principal).unwrap();
    }

    #[test]
    fn hand_over_endpoint_rejects_a_non_socket() {
        // A regular file at the socket path must fail, not pass.
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
    fn an_untraversable_socket_dir_is_refused_or_fixed_by_whoever_owns_it() {
        // Unprivileged, the cell is the broker's own user: a dir its owner
        // can't traverse is refused before anything is built. A root broker owns
        // the dir and opens it to the cell's group (mode 0710).
        use std::os::unix::fs::MetadataExt as _;
        use std::os::unix::net::UnixListener;
        let dir = make_socket_dir("notrav");
        let (endpoint, principal) = endpoint_in(&dir);
        let _listener = UnixListener::bind(&endpoint.socket).unwrap();
        std::fs::set_permissions(&*dir, std::fs::Permissions::from_mode(0o600)).unwrap();
        let result = hand_over_endpoint(&endpoint, principal);
        let mode = std::fs::symlink_metadata(&*dir).unwrap().mode() & 0o7777;
        std::fs::set_permissions(&*dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        if hatchling_sys::euid() == 0 {
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(mode, 0o710);
        } else {
            // The socket comes first, and can't be opened through a dir its
            // owner can't traverse.
            assert!(
                matches!(&result, Err(Error::Spawn { reason })
                    if reason.starts_with("cannot hand the broker socket ")
                        && reason.contains("Permission denied")),
                "hand_over_endpoint must refuse an untraversable dir, got {result:?}"
            );
        }
    }

    #[test]
    fn a_cells_wait_returns_the_child_status() {
        // Mutants: the wait replaced with a constant. Fork a child that
        // exits with a distinctive code; wait must return the real
        // status, not a constant.
        let staging_dir = Staging::create().unwrap();
        let staging = staging_dir.path().to_path_buf();
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
            staging: Some(staging_dir),
            child_to_parent,
            relay: None,
            connection: None,
            cgroup: None,
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
        let staging_dir = Staging::create().unwrap();
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
            staging: Some(staging_dir),
            child_to_parent,
            relay: None,
            connection: None,
            cgroup: None,
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
            parent_after_fork(
                child,
                uid_map,
                gid_map,
                &mut c2p,
                &mut p2c,
                SHORT_DEADLINE,
                None,
            )
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
            parent_after_fork(
                child,
                uid_map,
                gid_map,
                &mut c2p,
                &mut p2c,
                SHORT_DEADLINE,
                None,
            )
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
            parent_after_fork(
                child,
                uid_map,
                gid_map,
                &mut c2p,
                &mut p2c,
                SHORT_DEADLINE,
                None,
            )
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
        let staging_dir = Staging::create().unwrap();
        let staging = staging_dir.path().to_path_buf();
        match hatchling_sys::fork().unwrap() {
            ForkResult::Parent { child } => {
                let pid = child.pid().get();
                let (c2p, _p2c) = handshake_pipes().unwrap();
                let cell = LinuxCell {
                    child,
                    staging: Some(staging_dir),
                    child_to_parent: c2p,
                    relay: None,
                    connection: None,
                    cgroup: None,
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
        memory: None,
        cpu_millis: None,
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
                memory: None,
                cpu_millis: None,
                ..Limits::default()
            };
            if hold_to_limits(&some, false).is_err() {
                return 1;
            }
            if !held_to(&some, &before) {
                return 2;
            }
            if hold_to_limits(&HELD, false).is_err() {
                return 1;
            }
            // Open files stays at 32: a limit is never raised.
            let raised = Limits {
                open_files: Some(32),
                memory: None,
                cpu_millis: None,
                ..HELD
            };
            if held_to(&raised, &before) { 0 } else { 2 }
        });
    }

    #[test]
    fn a_grouped_cell_keeps_nproc_for_the_cgroup_to_enforce() {
        in_child(
            "1: refused; 2: grouped cell changed NPROC or lost another limit",
            || {
                let before = limits_now();
                if hold_to_limits(&HELD, true).is_err() {
                    return 1;
                }
                let grouped = Limits {
                    processes: None,
                    ..HELD
                };
                if held_to(&grouped, &before) { 0 } else { 2 }
            },
        );
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
            connection: false,
            exposures: Vec::new(),
            tools: Vec::new(),
            relay: None,
            filter: Filter::Program,
            limits: HELD,
            cgroup: None,
        };
        in_child("1: no error; 2: not held to its limits", || {
            let before = limits_now();
            if exec_program(&spec, None).to_string().is_empty() {
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
        // It waited out its timeout rather than answering at once. A
        // socket's receive timeout counts in the kernel's ticks, and can
        // end up to one tick early.
        assert!(started.elapsed() >= Duration::from_millis(150));
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
    fn a_cell_and_its_owner_talk_over_the_cells_connection_alone() {
        use std::io::{BufRead as _, Write as _};
        if !crate::linux::cell_available() {
            return;
        }
        let scratch = Scratch::new("connection");
        let (mut spec, _) = specs(&scratch);
        spec.filter = Filter::Cli;
        spec.program = PathBuf::from("/bin/bash");
        // The program finds its end where the variable says, says hello on
        // it, and waits for the owner's answer. A spec's own value for the
        // variable never reaches it.
        spec.argv = vec![
            String::from(PROGRAM),
            String::from("-c"),
            format!(
                "echo \"hello ${CONNECTION_FD_VAR}\" >&\"${CONNECTION_FD_VAR}\" && \
                 read -r answer <&\"${CONNECTION_FD_VAR}\" && [ \"$answer\" = ok ]"
            ),
        ];
        spec.env = vec![(String::from(CONNECTION_FD_VAR), String::from("2"))];
        spec.connection = true;
        let mut cell = isolate(&spec).unwrap();
        let connection = cell.connection().unwrap();
        assert!(cell.connection().is_none(), "handed over once");
        connection
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut hello = String::new();
        std::io::BufReader::new(&connection)
            .read_line(&mut hello)
            .unwrap();
        let fd: Option<i32> = hello
            .strip_prefix("hello ")
            .and_then(|fd| fd.trim().parse().ok());
        assert!(
            fd.is_some_and(|fd| fd > 2),
            "the connection is a descriptor above stdio, got {hello:?}"
        );
        (&connection).write_all(b"ok\n").unwrap();
        let exit = within_watchdog("connection cell", move || cell.wait());
        assert_eq!(exit.unwrap().code, 0);
        let (plain, _) = specs(&scratch);
        let mut cell = isolate(&plain).unwrap();
        assert!(cell.connection().is_none(), "a cell without a connection");
        let exit = within_watchdog("plain cell", move || cell.wait());
        assert_eq!(exit.unwrap().code, 0);
    }

    #[test]
    fn only_the_spawn_names_the_connection_in_the_programs_environment() {
        let env = vec![
            (String::from("A"), String::from("1")),
            (String::from(CONNECTION_FD_VAR), String::from("2")),
        ];
        assert_eq!(program_env(&env, None), vec![("A", "1")]);
        assert_eq!(
            program_env(&env, Some("7")),
            vec![("A", "1"), (CONNECTION_FD_VAR, "7")]
        );
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
            matches!(&result, Err(Error::Spawn { reason }) if reason.starts_with("cannot hand the broker socket ")),
            "got {:?}",
            result.map(|cell| cell.pid())
        );
    }
}
