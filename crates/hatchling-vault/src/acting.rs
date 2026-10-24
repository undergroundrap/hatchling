//! A root process acting for another user (WO-017).
//!
//! Run as root, the operator's vault commands work on a vault, a key and
//! a keyring that belong to the vault keeper's user. [`act_for`] names
//! that user and the directories it owns; there, what is opened is held
//! to that user's ownership, and what is made is given to it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The user a root process acts for, and the directories it acts for them
/// in: set once, by [`act_for`].
static ACTING: OnceLock<Acting> = OnceLock::new();

/// See [`ACTING`].
#[derive(Debug)]
struct Acting {
    owner: u32,
    dirs: Vec<PathBuf>,
}

/// Lets this process, run as root, act for `owner` in `dirs` (WO-017).
///
/// In each, a store, a key file or a keyring is opened as `owner`'s (it
/// must be theirs), and what this process makes there is given to
/// `owner`. What the operator's commands do for the vault keeper's user,
/// which owns the vault, its key and its keyring. Anywhere else, root
/// opens only what is its own, as before.
///
/// # Errors
///
/// Refused unless this process runs as root, for root itself, and once
/// it acts for a user already.
pub fn act_for(owner: u32, dirs: &[&Path]) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    if rustix::process::getuid().as_raw() != 0 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "only root acts for another user",
        ));
    }
    if owner == 0 {
        return Err(Error::new(ErrorKind::InvalidInput, "root acts for itself"));
    }
    let acting = Acting {
        owner,
        dirs: dirs.iter().map(|dir| dir.to_path_buf()).collect(),
    };
    ACTING.set(acting).map_err(|_| {
        Error::new(
            ErrorKind::AlreadyExists,
            "this process already acts for a user",
        )
    })
}

/// The user whose `dir` is, to this process: the one it acts for there
/// ([`act_for`]), or its own real uid.
pub(crate) fn owner_in(dir: &Path) -> u32 {
    let own = rustix::process::getuid().as_raw();
    ACTING
        .get()
        .filter(|acting| own == 0 && acting.dirs.iter().any(|acted| acted == dir))
        .map_or(own, |acting| acting.owner)
}

/// Gives `file`, just made, to `owner` ([`hatchling_sys::give_to`]).
pub(crate) fn give(file: &std::fs::File, owner: u32) -> std::io::Result<()> {
    hatchling_sys::give_to(file, owner).map_err(std::io::Error::other)
}
