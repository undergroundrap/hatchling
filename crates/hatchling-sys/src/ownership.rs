//! Ownership transfer for the cell's socket surface.
//!
//! The broker transfers ownership of its listen socket (and the
//! socket's directory) to the cell's uid base before forking, so the
//! mapped agent can traverse the directory and connect. `chown(2)`
//! never follows symlinks here: the spawner verifies the socket is a
//! real socket first.

use std::io;
use std::path::Path;

use crate::Error;

/// Changes the owner of `path` to `uid` and its group to `gid`.
/// Symlinks are not followed.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if the `chown` syscall fails.
pub fn chown_ids(path: &Path, uid: u32, gid: u32) -> Result<(), Error> {
    // SAFETY: `Uid::from_raw` just wraps the validated u32 uid; the
    // unsafety is rustix's marker for raw-id construction, and the
    // value here came from the validated cell configuration.
    let owner = unsafe { rustix::fs::Uid::from_raw(uid) };
    // SAFETY: `Gid::from_raw` just wraps the u32 gid, as for the uid above.
    let group = unsafe { rustix::fs::Gid::from_raw(gid) };
    rustix::fs::chown(path, Some(owner), Some(group)).map_err(|source| Error::Ownership {
        operation: "chown",
        source: io::Error::from_raw_os_error(source.raw_os_error()),
    })
}

/// Creates `path` as a new directory only this process's user can use.
///
/// The directory is made with mode 0700 and must not exist already: a
/// directory or a symlink planted at `path` fails the call, and nothing
/// is followed. The result is then checked through a handle opened
/// without following symlinks: a directory, owned by the effective uid,
/// closed to group and others ([`private_dir_ok`]).
///
/// # Errors
///
/// Returns [`Error::Ownership`] if the directory can't be made (an
/// existing entry's error is `AlreadyExists`), or if the check fails.
pub fn create_private_dir(path: &Path) -> Result<(), Error> {
    let failed = |operation| {
        move |source: rustix::io::Errno| Error::Ownership {
            operation,
            source: io::Error::from_raw_os_error(source.raw_os_error()),
        }
    };
    rustix::fs::mkdir(path, rustix::fs::Mode::from_raw_mode(0o700)).map_err(failed("mkdir"))?;
    let handle = open_dir_nofollow(path).map_err(failed("open the new directory"))?;
    let stat = rustix::fs::fstat(&handle).map_err(failed("fstat"))?;
    if private_dir_ok(
        stat.st_uid,
        stat.st_mode,
        rustix::process::geteuid().as_raw(),
    ) {
        Ok(())
    } else {
        Err(Error::Ownership {
            operation: "check the new directory",
            source: io::Error::from(io::ErrorKind::PermissionDenied),
        })
    }
}

/// Opens `path` only if it is a directory itself: a symlink, even to a
/// directory, or anything else fails.
fn open_dir_nofollow(path: &Path) -> Result<rustix::fd::OwnedFd, rustix::io::Errno> {
    const FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::RDONLY
        .union(rustix::fs::OFlags::DIRECTORY)
        .union(rustix::fs::OFlags::NOFOLLOW)
        .union(rustix::fs::OFlags::CLOEXEC);
    rustix::fs::open(path, FLAGS, rustix::fs::Mode::empty())
}

/// Whether a directory with owner `uid` and mode `mode` is private to
/// the process whose effective uid is `euid`: owned by it, with no
/// permission for group or others.
#[must_use]
#[allow(
    clippy::verbose_bit_mask,
    reason = "the permission bits read as a mask: group and other are 0o077"
)]
pub const fn private_dir_ok(uid: u32, mode: u32, euid: u32) -> bool {
    uid == euid && mode & 0o077 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_private_dir_is_new_ours_and_closed_to_everyone_else() {
        let dir = owned_test_dir().with_extension("private");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fresh = dir.join("fresh");
        create_private_dir(&fresh).unwrap();
        let meta = std::fs::metadata(&fresh).unwrap();
        assert!(meta.is_dir());
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777,
            0o700
        );
        // Anything already there fails the call, a symlink included, and
        // the symlink's target is left alone.
        let elsewhere = dir.join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let planted = dir.join("planted");
        std::os::unix::fs::symlink(&elsewhere, &planted).unwrap();
        for taken in [&fresh, &planted] {
            let err = create_private_dir(taken).unwrap_err();
            assert!(
                matches!(&err, Error::Ownership { operation: "mkdir", source } if source.kind() == io::ErrorKind::AlreadyExists),
                "{err:?}"
            );
        }
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_check_opens_only_a_real_directory() {
        // If the new directory were swapped for a symlink or a file
        // between its mkdir and the check, the open itself must fail.
        let dir = owned_test_dir().with_extension("open");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        std::fs::write(dir.join("file"), b"x").unwrap();
        assert!(open_dir_nofollow(&dir.join("real")).is_ok());
        // Not followed: with O_DIRECTORY, Linux answers a symlink "not a
        // directory" (without O_DIRECTORY it would be ELOOP).
        assert_eq!(
            open_dir_nofollow(&dir.join("link")).unwrap_err(),
            rustix::io::Errno::NOTDIR
        );
        assert_eq!(
            open_dir_nofollow(&dir.join("file")).unwrap_err(),
            rustix::io::Errno::NOTDIR
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dir_is_private_only_when_ours_and_closed_to_group_and_others() {
        assert!(private_dir_ok(1000, 0o40700, 1000));
        assert!(private_dir_ok(0, 0o40500, 0));
        assert!(!private_dir_ok(1001, 0o40700, 1000), "another user's");
        assert!(!private_dir_ok(1000, 0o40710, 1000), "group may enter");
        assert!(!private_dir_ok(1000, 0o40701, 1000), "others may enter");
        assert!(!private_dir_ok(1000, 0o40770, 1000));
    }

    /// Scratch dir for the chown fixture. Mutation runs point `TMPDIR`
    /// into the workspace scratch tree, and the workspace filesystem
    /// here denies chown(2) even to root; this fixture must not inherit
    /// `TMPDIR`.
    fn owned_test_dir() -> PathBuf {
        Path::new("/var/tmp").join(format!("hatchling-sys-chown-test-{}", std::process::id()))
    }

    #[test]
    fn chown_to_current_uid_is_a_noop_success() {
        // Chowning a file we own to our own uid must succeed without
        // privilege; anything else is an environment problem, not a
        // logic problem, so only assert the already-owned fast path.
        let dir = owned_test_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let probe = dir.join("probe");
        std::fs::write(&probe, b"x").unwrap();
        let uid = rustix::process::getuid().as_raw();
        let gid = rustix::process::getgid().as_raw();
        chown_ids(&probe, uid, gid).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn chown_missing_path_is_an_error() {
        let err = chown_ids(
            Path::new("/nonexistent/hatchling-sys-chown-probe"),
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Ownership { .. }));
    }
}
