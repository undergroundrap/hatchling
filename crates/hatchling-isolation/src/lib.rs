//! The isolation seam: one trait between the broker's policy
//! and a platform's mechanism.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! The broker decides **who** is isolated and **with what**: which host
//! directories a cell sees and whether it may write them, which filter
//! class it runs under, its program, arguments and environment, and the
//! identity it runs as. It writes those decisions into a [`CellSpec`].
//! An [`IsolationBackend`] decides only **how**: it builds a cell that
//! holds exactly what the spec grants, runs the program in it, and
//! supervises it to its end. A backend executes; it never widens a
//! grant.
//!
//! The vocabulary is the broker's, not a platform's: a cell, its
//! identity, what it is exposed to, its filter class, its supervision.
//! Linux implements it with user, mount, network, IPC and PID
//! namespaces, a bind-mounted read-only root, seccomp and `NoNewPrivs`
//! ([`linux`]). Another platform would implement the same contracts
//! with its own primitives, extending the trait where this broker's
//! needs grow; nothing here names a Linux primitive.
//!
//! # Owner transparency
//!
//! Hatchling isolates workloads from each other, never the runtime from
//! its owner. Every backend must meet this, method by method, or it does
//! not ship:
//!
//! - **Identity:** a cell runs as a host principal the owner can see
//!   ([`IsolationBackend::identity`]), never a hidden one.
//! - **View:** what a cell sees is fully described, before it runs, by
//!   [`IsolationBackend::view`]: every host path, its in-cell name and
//!   whether it is writable. The cell gets exactly that view.
//! - **Supervision:** every cell and helper ends under the owner's
//!   control: bounded waits, an exit the owner can read, and kill on
//!   drop. Nothing outlives its handle.
//! - **Nothing sealed from the owner:** a backend may keep a cell away
//!   from the host, never the host's owner away from the cell.
//!
//! [`conformance`] holds the executable part of these contracts. Every
//! backend's test suite runs it.

pub mod conformance;
#[cfg(target_os = "linux")]
pub mod linux;

use std::num::NonZeroU64;
use std::path::PathBuf;
use std::time::Duration;

/// Where a cell sees its program.
pub const PROGRAM: &str = "/agent";

/// Where a cell sees the directory of the broker endpoint it may reach.
pub const ENDPOINT_DIR: &str = "/sock";

/// The variable that tells a cell's program which descriptor holds its
/// connection to its owner ([`CellSpec::connection`]), in decimal.
pub const CONNECTION_FD_VAR: &str = "HATCHLING_CONNECTION_FD";

/// The backend for the platform this build runs on.
#[cfg(target_os = "linux")]
pub type Platform = linux::Linux;

/// The backend for the platform this build runs on.
#[cfg(target_os = "linux")]
#[must_use]
pub const fn platform() -> Platform {
    linux::Linux
}

/// One platform's isolation mechanism.
///
/// Each method's contract is stated without naming a platform
/// primitive. The owner-transparency contract in the [crate
/// documentation](crate) binds every method.
pub trait IsolationBackend {
    /// A running cell under supervision.
    type Cell: Cell;
    /// A helper process the broker started, under supervision.
    type Process: Supervised;

    /// The principal the broker runs as: the owner's process.
    fn owner(&self) -> u32;

    /// The identity a cell runs as, from the operator's configured
    /// principal (`None` for the platform's default) and how many
    /// principals the cell spans.
    ///
    /// The identity must be one the owner's privilege can grant, and
    /// never the owner's own privileged principal.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] for an identity this platform refuses.
    fn identity(&self, principal: Option<u32>, count: u32) -> Result<CellIdentity, Error>;

    /// The identity a cell runs as on the owner's subordinate ids: a
    /// principal and group of its own, never the owner's or another
    /// account's, granted through the platform's helpers
    /// ([`Mapping::Subordinate`]).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Identity`] if there is none to grant: a platform
    /// without subordinate ids (the default), a privileged owner (which
    /// grants identities itself), no range or an unsafe one, or helpers
    /// that can't be trusted.
    fn subordinate_identity(&self) -> Result<CellIdentity, Error> {
        Err(Error::Identity(PlatformError::new(std::io::Error::other(
            "this platform has no subordinate identities",
        ))))
    }

    /// The principal the broker appears as inside a cell with
    /// `identity`: what the cell's peer check must expect.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawn`] if the platform cannot say.
    fn broker_in_cell(&self, identity: &CellIdentity) -> Result<u32, Error>;

    /// Everything a cell built from `spec` would see, in order: the
    /// endpoint directory (if granted), the program, each granted
    /// exposure, then the read-only runtime support the program itself
    /// needs to start. Nothing else.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawn`] if a granted path does not exist or the
    /// program's runtime support cannot be determined.
    fn view(&self, spec: &CellSpec) -> Result<CellView, Error>;

    /// Starts `spec`'s program in a new cell holding exactly what
    /// [`view`](Self::view) reports for `spec`, under its filter class,
    /// as its identity. A spec with an endpoint makes that endpoint reachable
    /// by the cell's identity, and nothing wider.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if any step fails. Failure is closed: no
    /// program runs in a half-built cell.
    fn isolate(&self, spec: &CellSpec) -> Result<Self::Cell, Error>;

    /// Takes supervision of a helper process the broker just started.
    /// On failure the helper is killed and reaped first: it is never
    /// left unsupervised.
    ///
    /// # Errors
    ///
    /// Returns [`SuperviseError`] if the helper cannot be supervised.
    fn supervise(&self, child: &mut std::process::Child) -> Result<Self::Process, SuperviseError>;
}

/// A running cell. Dropping it without [`wait`](Self::wait)ing ends the
/// cell: nothing outlives its handle.
pub trait Cell {
    /// The cell's process id, for diagnostics and logs only.
    fn pid(&self) -> i32;

    /// Waits for the cell to end, within the platform's bound on a
    /// cell's runtime, and reports how it ended.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Spawn`] if the cell reported a setup failure, or
    /// [`Error::Wait`] if waiting failed or the runtime bound passed.
    fn wait(self) -> Result<Exit, Error>;

    /// The listener of the cell's relay port, once: the owner
    /// serves it. `None` for a cell without one, or once taken.
    fn relay(&mut self) -> Option<std::net::TcpListener>;

    /// The owner's end of the cell's connection, once. `None` for a cell
    /// without one, or once taken.
    fn connection(&mut self) -> Option<std::os::unix::net::UnixStream>;
}

/// A helper process under supervision. Dropping it ends the helper.
pub trait Supervised {
    /// How the helper ended, if it has; `None` while it runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if its state cannot be read.
    fn try_wait(&self) -> Result<Option<Exit>, Error>;

    /// Waits up to `timeout` for the helper to end. Past it the helper
    /// is killed and reaped, and the wait fails.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] on a timeout or a failed wait.
    fn wait_deadline(&self, timeout: Duration) -> Result<Exit, Error>;

    /// Kills the helper.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Wait`] if it cannot be signalled.
    fn terminate(&self) -> Result<(), Error>;
}

/// How a cell or helper ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// The exit code: the program's own, or `128 + n` for an end by
    /// signal `n` (the shell's convention).
    pub code: i32,
    /// The platform's raw status, for logs only.
    pub status: i32,
}

/// The identity a cell runs as: a host principal and group, visible to
/// the owner, how many principals the cell spans from them, and how they
/// are granted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellIdentity {
    /// The host principal the cell's own root appears as.
    pub principal: u32,
    /// The host group the cell's own root group appears as: the same
    /// number as `principal` for a [`Mapping::Direct`] identity.
    pub group: u32,
    /// How many principals, from `principal`, the cell spans.
    pub count: u32,
    /// How the identity is granted.
    pub mapping: Mapping,
}

impl CellIdentity {
    /// A [`Mapping::Direct`] identity: `count` principals from
    /// `principal`, its group the same number.
    #[must_use]
    pub const fn direct(principal: u32, count: u32) -> Self {
        Self {
            principal,
            group: principal,
            count,
            mapping: Mapping::Direct,
        }
    }
}

/// How a cell's identity is granted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mapping {
    /// By the broker itself: any principal for a privileged owner, its
    /// own for an unprivileged one.
    #[default]
    Direct,
    /// By the platform's helpers, from the owner's subordinate ids: a
    /// principal of the cell's own, granted without privilege (WO-014).
    Subordinate,
}

/// The broker's decisions for one cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellSpec {
    /// Who the cell runs as.
    pub identity: CellIdentity,
    /// The host path of the program. The cell sees it at [`PROGRAM`].
    pub program: PathBuf,
    /// The program's arguments, its own path first.
    pub argv: Vec<String>,
    /// The program's whole environment: nothing of the host's leaks in.
    pub env: Vec<(String, String)>,
    /// Where in the cell the program starts, if not at the root.
    pub workdir: Option<String>,
    /// The broker endpoint the cell may reach, if any.
    pub endpoint: Option<Endpoint>,
    /// Whether the program gets a connection to its owner: one end of a
    /// connected stream socket, open in the program at the descriptor
    /// [`CONNECTION_FD_VAR`] names, while the owner takes the other
    /// ([`Cell::connection`]). Unlike an endpoint, nothing outside the
    /// cell can reach it: no other process holds an end.
    pub connection: bool,
    /// Further host directories the cell sees, in order.
    pub exposures: Vec<Exposure>,
    /// Host programs the cell may run besides its own, each seen
    /// read-only at its name with what it needs to start, in order.
    pub tools: Vec<Tool>,
    /// A TCP port on the cell's own loopback: the cell's only
    /// network. The owner receives its listener ([`Cell::relay`]) and
    /// answers it; the cell keeps no copy.
    pub relay: Option<u16>,
    /// What the program may ask of the platform.
    pub filter: Filter,
    /// What the program, and everything it starts, is held to.
    pub limits: Limits,
    /// The delegated parent for this cell's resource group, if configured.
    /// The cell itself never sees this host path.
    pub cgroup: Option<PathBuf>,
    /// A private writable `tmpfs` at `/scratch` of at most this many
    /// bytes, if any: the one writable place no host path backs. Its pages
    /// are the cell's memory, which the resource group bounds, and it is
    /// gone with the cell, however the cell ends.
    pub scratch: Option<NonZeroU64>,
}

/// What a cell's program, and everything it starts, is held to.
///
/// Program limits are both soft and hard, so the cell can't lift them;
/// values above the broker's hard limit are held at that limit. `None`
/// leaves the inherited limit. Whole-cell memory and CPU require a group;
/// unset values there inherit its parent's bound. With a group, the process
/// bound counts every task in the cell; without one it is a real-uid rlimit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    /// How many processes, threads included, the cell may hold at once.
    pub processes: Option<u64>,
    /// The program's address space, in bytes. That isn't its memory: a
    /// runtime may reserve far more address space than it uses.
    pub address_space: Option<u64>,
    /// The largest file the program may write, in bytes.
    pub file_size: Option<u64>,
    /// How many files the program may hold open at once.
    pub open_files: Option<u64>,
    /// Memory for the whole cell, in bytes; requires a delegated group.
    pub memory: Option<u64>,
    /// CPU allowance in thousandths of a core; requires a delegated group.
    pub cpu_millis: Option<u64>,
}

/// A broker endpoint a cell may reach. Its directory is seen read-only
/// at [`ENDPOINT_DIR`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The endpoint itself.
    pub socket: PathBuf,
    /// The directory holding it.
    pub dir: PathBuf,
}

/// A host directory a cell sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exposure {
    /// The host directory.
    pub source: PathBuf,
    /// Its name in the cell, relative to the cell's root.
    pub name: String,
    /// Whether the cell may write it. Read-only unless granted.
    pub writable: bool,
    /// What it is, for error messages.
    pub label: &'static str,
}

/// A host program a cell may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// The host program.
    pub source: PathBuf,
    /// Its name in the cell, relative to the cell's root: usually its own
    /// host path, so the cell's programs find it where they expect.
    pub name: String,
}

/// What a cell's program may ask of the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// The calls a program needs to talk to the broker and compute, and
    /// no more.
    Program,
    /// Those, plus the file calls a program needs to edit the
    /// directories it was granted writable (a harness's work tree).
    WorkTree,
    /// Those, plus what a real command-line program needs:
    /// threads and child processes, pipes, polling, and TCP to its relay.
    /// Its cell also gets the null, zero and random devices, and a view
    /// of its own processes.
    Cli,
    /// Those, plus what a headless browser needs (WO-029): messages with
    /// descriptors between its own processes, and shared memory. Its cell
    /// is a CLI cell's in every other way.
    Browser,
}

impl Filter {
    /// Whether the cell runs a program with processes of its own: it gets
    /// the null, zero and random devices, and a view of its processes.
    #[must_use]
    pub const fn runs_processes(self) -> bool {
        matches!(self, Self::Cli | Self::Browser)
    }
}

/// Everything one cell sees, as [`IsolationBackend::view`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellView {
    /// Each host path the cell sees, in the order the cell is built.
    pub entries: Vec<ViewEntry>,
    /// The cell's relay port, its only network, if it has one.
    pub relay: Option<u16>,
    /// Whether the cell sees its own processes: none of the
    /// host's, and no other cell's.
    pub processes: bool,
    /// The size of the cell's private scratch `tmpfs`, if it has one.
    pub scratch: Option<NonZeroU64>,
}

/// One host path a cell sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewEntry {
    /// The host path, resolved.
    pub source: PathBuf,
    /// Its name in the cell, relative to the cell's root.
    pub name: String,
    /// Whether it is a directory.
    pub dir: bool,
    /// Whether the cell may write it.
    pub writable: bool,
    /// Why the cell sees it.
    pub origin: Origin,
}

/// Why a cell sees a host path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The directory of the endpoint the spec grants.
    Endpoint,
    /// The spec's program.
    Program,
    /// One of the spec's exposures.
    Granted,
    /// One of the spec's tools.
    Tool,
    /// A device a CLI cell writes: the null, zero or random
    /// device, and nothing else.
    Device,
    /// Read-only support the program needs to start, added by the
    /// backend (shared libraries, on Linux).
    Runtime,
}

/// An isolation failure.
///
/// Each variant names the step that failed; the message is the
/// platform's own.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The cell's identity was refused.
    #[error(transparent)]
    Identity(PlatformError),
    /// The cell's view could not be built.
    #[error(transparent)]
    View(PlatformError),
    /// The cell could not be started.
    #[error(transparent)]
    Start(PlatformError),
    /// Waiting on a cell or helper failed.
    #[error(transparent)]
    Wait(PlatformError),
    /// A step failed, as described.
    #[error("{reason}")]
    Spawn {
        /// What failed.
        reason: String,
    },
}

/// A platform's own error, carried unchanged.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct PlatformError(Box<dyn std::error::Error + Send + Sync + 'static>);

impl PlatformError {
    /// Wraps a platform's error.
    pub fn new(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Box::new(source))
    }
}

/// Why a helper could not be supervised.
#[derive(Debug, thiserror::Error)]
pub enum SuperviseError {
    /// Its process id is not one supervision can hold.
    #[error("helper pid failed validation")]
    InvalidPid,
    /// The platform refused supervision.
    #[error(transparent)]
    Refused(PlatformError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_errors_carry_the_platforms_own_message() {
        let error = Error::Identity(PlatformError::new(std::io::Error::other("refused map")));
        assert_eq!(error.to_string(), "refused map");
        let error = Error::Spawn {
            reason: String::from("no such dir"),
        };
        assert_eq!(error.to_string(), "no such dir");
        assert_eq!(
            SuperviseError::InvalidPid.to_string(),
            "helper pid failed validation"
        );
        let refused =
            SuperviseError::Refused(PlatformError::new(std::io::Error::other("no pidfd")));
        assert_eq!(refused.to_string(), "no pidfd");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_platform_backend_is_the_linux_one() {
        let backend: Platform = platform();
        assert_eq!(backend, linux::Linux);
    }

    #[test]
    fn the_cell_layout_is_absolute() {
        assert_eq!(PROGRAM, "/agent");
        assert_eq!(ENDPOINT_DIR, "/sock");
    }
}
