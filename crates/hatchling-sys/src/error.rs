//! Typed errors for `hatchling-sys`.
//!
//! Every failure is a variant; callers match exhaustively. There are no
//! stringly errors.

use thiserror::Error;

/// All errors producible by `hatchling-sys`.
#[derive(Debug, Error)]
pub enum Error {
    /// A delegated cgroup could not be configured or cleaned up.
    #[error("cgroup operation failed: {operation} at {path}: {source}")]
    Cgroup {
        /// Which operation failed.
        operation: &'static str,
        /// The operator's directory or this cell's leaf.
        path: std::path::PathBuf,
        /// The underlying failure.
        source: std::io::Error,
    },
    /// An id map was constructed with invalid parameters.
    #[error("invalid id map: {reason}")]
    InvalidIdMap {
        /// Why the map was rejected.
        reason: String,
    },

    /// Writing a uid/gid map for a child process failed.
    #[error("failed to write id maps for pid {pid}: {source}")]
    IdMapWrite {
        /// The child the maps were for.
        pid: i32,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// A namespace operation failed.
    #[error("namespace operation failed: {operation}: {source}")]
    Namespace {
        /// Which operation failed (`unshare(user)`, ...).
        operation: &'static str,
        /// The underlying OS failure.
        source: std::io::Error,
    },

    /// A mount-table operation failed.
    #[error("mount operation failed: {operation}: {source}")]
    Mount {
        /// Which operation failed.
        operation: &'static str,
        /// The underlying OS failure.
        source: std::io::Error,
    },

    /// The mount plan was rejected at build time.
    #[error("invalid mount plan: {reason}")]
    InvalidMountPlan {
        /// Why the plan was rejected.
        reason: String,
    },

    /// A process operation failed: fork, exec, wait, prctl.
    #[error("process operation failed: {operation}: {source}")]
    Process {
        /// Which operation failed.
        operation: &'static str,
        /// The underlying OS failure.
        source: std::io::Error,
    },

    /// A bounded wait on a child process expired. The wait is fail
    /// closed: the child was `SIGKILL`ed via its pidfd and reaped before
    /// this error was returned — never a hang, never an orphan.
    #[error("process wait timed out: {operation} after {timeout:?}")]
    ProcessTimeout {
        /// Which wait expired.
        operation: &'static str,
        /// The deadline that was exceeded.
        timeout: std::time::Duration,
    },

    /// A syscall name is not in the per-arch table.
    #[error("unknown syscall name for this architecture: {name}")]
    UnknownSyscall {
        /// The rejected name.
        name: String,
    },

    /// Installing the seccomp filter failed.
    #[error("seccomp installation failed: {source}")]
    SeccompInstall {
        /// The underlying OS failure.
        source: std::io::Error,
    },

    /// A value could not be converted to a C string (interior NUL).
    #[error("invalid value for exec: {reason}")]
    InvalidExecArg {
        /// Why the argument was rejected.
        reason: String,
    },

    /// A lock file could not be opened or locked (`lock::try_lock`).
    #[error("lock failed: {operation}: {source}")]
    Lock {
        /// Which operation failed.
        operation: &'static str,
        /// The underlying OS failure.
        source: std::io::Error,
    },

    /// A tree could not be walked or copied (`copy::copy_tree`): an
    /// entry refused, past a bound, or an operation that failed.
    #[error("copy failed at {path:?}: {operation}: {source}")]
    Copy {
        /// Which operation failed.
        operation: &'static str,
        /// The entry, relative to the tree (empty: the tree itself).
        path: String,
        /// The underlying failure, or why the entry was refused.
        source: std::io::Error,
    },

    /// A disposable tree could not be removed (`tree::remove_tree`).
    #[error("removal failed: {operation}: {source}")]
    Removal {
        /// Which operation failed.
        operation: &'static str,
        /// The underlying OS failure.
        source: std::io::Error,
    },

    /// An ownership transfer failed.
    #[error("ownership operation failed: {operation}: {source}")]
    Ownership {
        /// Which operation failed (`chown`).
        operation: &'static str,
        /// The underlying OS failure.
        source: std::io::Error,
    },

    /// A cell network operation failed: its loopback, its relay
    /// listener, or handing the listener out.
    #[error("network operation failed: {operation}: {source}")]
    Net {
        /// What was being done.
        operation: &'static str,
        /// The underlying OS error.
        source: std::io::Error,
    },

    /// Reading the kernel peer credentials (`SO_PEERCRED`) failed.
    #[error("failed to read socket peer credentials: {source}")]
    PeerCredentials {
        /// The underlying OS failure.
        source: std::io::Error,
    },
}
