//! `hatchling-sys`: the one crate that may contain `unsafe`.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! Every other crate in the workspace denies `unsafe_code`. This crate
//! owns the entire unsafe surface of the cell boundary: process forking,
//! Linux namespaces, mount-table construction, and seccomp-bpf filter
//! installation. Each module exposes a thin *safe* wrapper whose
//! contract is documented; the `unsafe` blocks inside are small,
//! audited, and each carries a `SAFETY` comment explaining why the call
//! is sound.
//!
//! The wrappers are deliberately low-level: policy (which id map, which
//! mounts, which syscall allowlist) lives in the caller
//! (`hatchling-broker`'s cell spawner). This crate answers "how", never
//! "what".

#![allow(
    unsafe_code,
    reason = "hatchling-sys is the single sanctioned unsafe crate (repo AGENTS.md): it wraps fork/execve/prctl, which rustix does not expose, and every unsafe block carries a SAFETY comment"
)]
#![warn(missing_docs)]

pub mod cgroup;
pub mod error;
pub mod fds;
pub mod idmap;
pub mod mounts;
pub mod namespaces;
pub mod net;
pub mod ownership;
pub mod peercred;
pub mod process;
pub mod seccomp;
pub mod sync_pipe;
#[cfg(test)]
mod test_socket;
pub mod umask;

pub use error::Error;
pub use fds::{
    cloexec_fds_above_stderr, close_fds_above_stderr_except, fd_flags, keep_across_exec, open_fds,
};
pub use idmap::IdMap;
pub use mounts::{BindMount, MountPlan};
pub use namespaces::{unshare_ipc, unshare_mount, unshare_net, unshare_pid, unshare_user};
pub use ownership::{
    DirShare, chmod_socket, create_private_dir, dir_share, ensure_private_dir, hand_over_socket,
    owner_traverses, share_dir_with_cell,
};
pub use peercred::{RawPeerCred, socket_peercred};
pub use process::{
    Child, ChildPid, ForkResult, Limit, become_cell_user, child_exit_code, clear_groups,
    drop_capabilities, euid, exec, exit_child, fork, hold_to, reap_as_init, self_pidfd,
    set_no_new_privs, set_not_dumpable, set_pdeathsig,
};
pub use seccomp::{SeccompProfile, SockFilter};
pub use sync_pipe::{ChildToParent, ParentToChild, handshake_pipes};
pub use umask::{current_umask, with_umask};
