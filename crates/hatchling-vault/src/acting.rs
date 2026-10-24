//! A root process acting for other users (WO-017).
//!
//! Run as root, the operator's vault commands work on a vault, a key and
//! a keyring that belong to the vault keeper's user, and on stores that
//! belong to the broker's. [`act_for`] names each user and the
//! directories it owns; there, what is opened is held to that user's
//! ownership, and what is made is given to it.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The users a root process acts for, each with the directories it acts
/// for them in: each added once, by [`act_for`], and never taken back.
static ACTING: Mutex<Vec<Acting>> = Mutex::new(Vec::new());

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
/// which owns the vault, its key and its keyring, and for the broker's,
/// which owns the broker's stores. Anywhere else, root opens only what is
/// its own, as before.
///
/// # Errors
///
/// Refused unless this process runs as root, for root itself, for a user
/// it acts for already, and in a directory it acts for another user in.
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
    let mut acting = ACTING.lock().unwrap_or_else(PoisonError::into_inner);
    if acting.iter().any(|known| known.owner == owner) {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            "this process already acts for that user",
        ));
    }
    if let Some(taken) = dirs.iter().find(|dir| {
        acting
            .iter()
            .any(|known| known.dirs.iter().any(|acted| acted == **dir))
    }) {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "this process already acts for another user in {}",
                taken.display()
            ),
        ));
    }
    acting.push(Acting {
        owner,
        dirs: dirs.iter().map(|dir| dir.to_path_buf()).collect(),
    });
    drop(acting);
    Ok(())
}

/// The user whose `dir` is, to this process: the one it acts for there
/// ([`act_for`], which only root can), or its own real uid.
pub(crate) fn owner_in(dir: &Path) -> u32 {
    ACTING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|acting| acting.dirs.iter().any(|acted| acted == dir))
        .map_or_else(|| rustix::process::getuid().as_raw(), |acting| acting.owner)
}

/// Gives `file`, just made, to `owner` ([`hatchling_sys::give_to`]).
pub(crate) fn give(file: &std::fs::File, owner: u32) -> std::io::Result<()> {
    hatchling_sys::give_to(file, owner).map_err(std::io::Error::other)
}
