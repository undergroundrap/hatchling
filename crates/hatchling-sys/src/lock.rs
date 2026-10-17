//! An exclusive lock a process holds for its life (WO-012).
//!
//! A serving broker holds one beside its socket, so a second broker, or
//! `hatchling-broker clean`, can tell that one is serving: the kernel
//! lets the lock go when the process ends, however it ends, so a broker
//! killed with `SIGKILL` leaves none behind.

use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

use rustix::fs::{FlockOperation, Mode, OFlags};

use crate::Error;

/// A lock held until dropped, or until the process ends.
#[derive(Debug)]
pub struct HeldLock(std::fs::File);

impl Drop for HeldLock {
    fn drop(&mut self) {
        // Closing the file lets the lock go too; this says so first.
        let _ = rustix::fs::flock(&self.0, FlockOperation::Unlock);
    }
}

/// Takes the lock at `path` without waiting: `None` while another holds
/// it.
///
/// A new lock file is made `0600`; one already there must be a regular
/// file of this process's effective user, and is opened without following
/// a symlink.
///
/// # Errors
///
/// Returns [`Error::Lock`] if the file can't be opened or made, if it
/// isn't a regular file of this user, or if locking fails otherwise.
pub fn try_lock(path: &Path) -> Result<Option<HeldLock>, Error> {
    let failed = |operation: &'static str| {
        move |source: rustix::io::Errno| Error::Lock {
            operation,
            source: io::Error::from_raw_os_error(source.raw_os_error()),
        }
    };
    let fd = rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(failed("open the lock"))?;
    let file = std::fs::File::from(fd);
    let metadata = file.metadata().map_err(|source| Error::Lock {
        operation: "stat the lock",
        source,
    })?;
    if !metadata.file_type().is_file() || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(Error::Lock {
            operation: "check the lock",
            source: io::Error::new(
                io::ErrorKind::PermissionDenied,
                "it isn't a file of this user",
            ),
        });
    }
    match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(HeldLock(file))),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
        Err(source) => Err(failed("take the lock")(source)),
    }
}

/// Holds a shared lock on the directory at `path`, opened without
/// following a symlink, until dropped: a mark that the directory is in
/// use, which [`in_use`] sees from another process.
///
/// # Errors
///
/// Returns [`Error::Lock`] if `path` isn't a directory itself, or if the
/// lock can't be taken.
pub fn hold_dir(path: &Path) -> Result<HeldLock, Error> {
    let failed = |operation: &'static str| {
        move |source: rustix::io::Errno| Error::Lock {
            operation,
            source: io::Error::from_raw_os_error(source.raw_os_error()),
        }
    };
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(failed("open the directory"))?;
    let file = std::fs::File::from(fd);
    rustix::fs::flock(&file, FlockOperation::NonBlockingLockShared)
        .map_err(failed("hold the directory"))?;
    Ok(HeldLock(file))
}

/// Claims the directory at `path` for this process alone, opened without
/// following a symlink, until dropped: `None` if a process holds it
/// ([`hold_dir`]), or it can't be opened as a directory itself, or
/// locked. Held while it is removed, no one can take it meanwhile.
#[must_use]
pub fn claim_dir(path: &Path) -> Option<HeldLock> {
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .ok()?;
    let file = std::fs::File::from(fd);
    rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive)
        .ok()
        .map(|()| HeldLock(file))
}

/// Whether a process holds the directory at `path` ([`hold_dir`]), or it
/// can't be told: one that can't be opened as a directory itself, or
/// locked, counts as in use.
#[must_use]
pub fn in_use(path: &Path) -> bool {
    claim_dir(path).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_directory_is_in_use_until_let_go() {
        let dir = std::env::temp_dir().join(format!("hatchling-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root")).unwrap();
        let root = dir.join("root");
        assert!(!in_use(&root), "no one holds it");
        let held = hold_dir(&root).unwrap();
        let another = hold_dir(&root).unwrap();
        assert!(in_use(&root), "held");
        drop(held);
        assert!(in_use(&root), "still held by another");
        drop(another);
        assert!(!in_use(&root), "let go");
        // A claim keeps everyone else out until it is dropped.
        let claimed = claim_dir(&root).unwrap();
        assert!(claim_dir(&root).is_none() && in_use(&root));
        assert!(hold_dir(&root).is_err(), "no one holds it meanwhile");
        drop(claimed);
        assert!(claim_dir(&root).is_some());
        assert!(claim_dir(&dir.join("missing")).is_none());
        // What can't be told counts as in use; a symlink is never followed.
        std::os::unix::fs::symlink(&root, dir.join("link")).unwrap();
        assert!(in_use(&dir.join("link")) && in_use(&dir.join("missing")));
        assert!(matches!(
            hold_dir(&dir.join("link")),
            Err(Error::Lock { operation: "open the directory", .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_holder_at_a_time_and_none_after_it_lets_go() {
        let dir = std::env::temp_dir().join(format!("hatchling-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broker.sock.lock");
        let held = try_lock(&path).unwrap().unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            0o600,
            "made owner-only"
        );
        assert!(try_lock(&path).unwrap().is_none(), "held: none for another");
        drop(held);
        assert!(try_lock(&path).unwrap().is_some(), "let go: free again");
        // A symlink is never followed, and a directory is no lock.
        let elsewhere = dir.join("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, dir.join("link.lock")).unwrap();
        assert!(matches!(
            try_lock(&dir.join("link.lock")),
            Err(Error::Lock {
                operation: "open the lock",
                ..
            })
        ));
        assert!(!elsewhere.exists(), "never followed");
        std::fs::create_dir(dir.join("dir.lock")).unwrap();
        assert!(try_lock(&dir.join("dir.lock")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
