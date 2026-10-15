//! Ownership for the cell's socket surface, and the broker's private
//! directories.
//!
//! The broker hands its listen socket to the cell's uid before forking,
//! so the mapped agent can connect, and lets the cell traverse the
//! socket's directory, which stays the broker's. Every change goes
//! through a handle opened without following symlinks: whatever is at a
//! path when it's opened is what's checked and changed, and a symlink
//! swapped in can't redirect a change to a file it points at.

use std::io;
use std::path::Path;

use crate::Error;

/// An [`Error::Ownership`] for `operation`, from a rustix error.
fn failed(operation: &'static str) -> impl Fn(rustix::io::Errno) -> Error {
    move |source| Error::Ownership {
        operation,
        source: io::Error::from_raw_os_error(source.raw_os_error()),
    }
}

/// An [`Error::Ownership`] for `operation` that the check itself made.
fn refused(operation: &'static str, why: &str) -> Error {
    Error::Ownership {
        operation,
        source: io::Error::new(io::ErrorKind::PermissionDenied, why),
    }
}

/// The raw ids as rustix takes them.
const fn ids(uid: u32, gid: u32) -> (rustix::fs::Uid, rustix::fs::Gid) {
    // SAFETY: `Uid::from_raw` only wraps the u32; the unsafety is
    // rustix's marker for raw-id construction, and the value comes from
    // the validated cell identity or the process itself.
    let owner = unsafe { rustix::fs::Uid::from_raw(uid) };
    // SAFETY: `Gid::from_raw` only wraps the u32, as for the uid above.
    let group = unsafe { rustix::fs::Gid::from_raw(gid) };
    (owner, group)
}

/// Gives the socket at `path` to `uid` and `gid`, the cell's.
///
/// `path` is opened without following symlinks, and what was opened
/// must itself be a socket; its owner is then changed through that
/// handle. A socket the cell already owns is left as it is.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if nothing is at `path`, if what is
/// there isn't a socket (a symlink to one included), or if the change
/// is refused.
pub fn hand_over_socket(path: &Path, uid: u32, gid: u32) -> Result<(), Error> {
    const FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::PATH
        .union(rustix::fs::OFlags::NOFOLLOW)
        .union(rustix::fs::OFlags::CLOEXEC);
    let handle = rustix::fs::open(path, FLAGS, rustix::fs::Mode::empty())
        .map_err(failed("open the socket"))?;
    let stat = rustix::fs::fstat(&handle).map_err(failed("fstat the socket"))?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Socket {
        return Err(refused("check the socket", "not a socket"));
    }
    if stat.st_uid == uid {
        return Ok(());
    }
    let (owner, group) = ids(uid, gid);
    rustix::fs::chownat(
        &handle,
        "",
        Some(owner),
        Some(group),
        rustix::fs::AtFlags::EMPTY_PATH,
    )
    .map_err(failed("chown the socket"))
}

/// What the broker does with its socket's directory for a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirShare {
    /// The cell is the broker's own user: the owner's bits already let it
    /// through.
    Keep,
    /// Make it the broker's, give the cell's group traverse alone, and no
    /// one else anything (mode 0710).
    Share,
    /// Neither the broker's nor the cell's: refuse it.
    Refuse,
}

/// What [`share_dir_with_cell`] does with a directory owned by `owner`.
///
/// The broker runs as `euid` and the cell as `cell`. A root broker takes
/// back a directory the cell owns: earlier versions handed the whole
/// directory over.
#[must_use]
pub const fn dir_share(owner: u32, euid: u32, cell: u32) -> DirShare {
    if cell == euid {
        if owner == euid {
            DirShare::Keep
        } else {
            DirShare::Refuse
        }
    } else if owner == euid || owner == cell {
        DirShare::Share
    } else {
        DirShare::Refuse
    }
}

/// Lets the cell running as `cell` traverse the socket directory at `path`.
///
/// The directory stays the broker's ([`dir_share`]): the cell can reach
/// the socket in it, and can't add, remove or replace anything there.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if `path` isn't a directory itself (a
/// symlink to one included), if it belongs to anyone but the broker or
/// the cell, if the cell couldn't traverse it, or if a change is refused.
pub fn share_dir_with_cell(path: &Path, cell: u32) -> Result<(), Error> {
    let handle = open_dir_nofollow(path).map_err(failed("open the socket directory"))?;
    let stat = rustix::fs::fstat(&handle).map_err(failed("fstat the socket directory"))?;
    let euid = rustix::process::geteuid().as_raw();
    match dir_share(stat.st_uid, euid, cell) {
        DirShare::Keep if owner_traverses(stat.st_mode) => Ok(()),
        DirShare::Keep => Err(refused(
            "check the socket directory",
            "its owner, the cell's user, can't traverse it",
        )),
        DirShare::Share => {
            let (owner, group) = ids(euid, cell);
            rustix::fs::fchown(&handle, Some(owner), Some(group))
                .map_err(failed("chown the socket directory"))?;
            rustix::fs::fchmod(&handle, rustix::fs::Mode::from_raw_mode(0o710))
                .map_err(failed("chmod the socket directory"))
        }
        DirShare::Refuse => Err(refused(
            "check the socket directory",
            "it belongs to neither the broker nor the cell",
        )),
    }
}

/// Whether a directory's owner may traverse it: the owner-execute bit.
#[must_use]
pub const fn owner_traverses(mode: u32) -> bool {
    mode & 0o100 != 0
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
        Err(refused(
            "check the new directory",
            "it isn't the broker's alone",
        ))
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
    fn a_socket_is_handed_over_only_if_it_is_one_itself() {
        use std::os::unix::net::UnixListener;
        let dir = owned_test_dir().with_extension("socket");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("broker.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let euid = rustix::process::geteuid().as_raw();
        let group = rustix::process::getegid().as_raw();
        // Already the cell's (an unprivileged broker's own): left alone.
        hand_over_socket(&socket, euid, group).unwrap();
        // A symlink to the socket, a file, or nothing: refused.
        std::os::unix::fs::symlink(&socket, dir.join("link")).unwrap();
        std::fs::write(dir.join("file"), b"x").unwrap();
        for (name, operation) in [
            ("link", "check the socket"),
            ("file", "check the socket"),
            ("missing", "open the socket"),
        ] {
            let err = hand_over_socket(&dir.join(name), euid, group).unwrap_err();
            assert!(
                matches!(&err, Error::Ownership { operation: op, .. } if *op == operation),
                "{name}: {err:?}"
            );
        }
        // Unprivileged, giving it to anyone else is refused, not skipped.
        if euid != 0 {
            let err = hand_over_socket(&socket, euid + 1, group).unwrap_err();
            assert!(
                matches!(
                    &err,
                    Error::Ownership {
                        operation: "chown the socket",
                        ..
                    }
                ),
                "{err:?}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn as_root_the_socket_is_the_cells_and_its_directory_stays_the_brokers() {
        use std::os::unix::fs::MetadataExt as _;
        use std::os::unix::net::UnixListener;
        if rustix::process::geteuid().as_raw() != 0 {
            return;
        }
        let dir = owned_test_dir().with_extension("root");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        let socket = dir.join("broker.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let cell = 65_000;
        share_dir_with_cell(&dir, cell).unwrap();
        hand_over_socket(&socket, cell, cell).unwrap();
        let dir_meta = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!((dir_meta.uid(), dir_meta.gid()), (0, cell));
        assert_eq!(dir_meta.mode() & 0o7777, 0o710);
        let socket_meta = std::fs::symlink_metadata(&socket).unwrap();
        assert_eq!((socket_meta.uid(), socket_meta.gid()), (cell, cell));
        // A directory an earlier version gave the cell is taken back.
        let (owner, group) = ids(cell, cell);
        rustix::fs::chown(&dir, Some(owner), Some(group)).unwrap();
        share_dir_with_cell(&dir, cell).unwrap();
        let dir_meta = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!((dir_meta.uid(), dir_meta.gid()), (0, cell));
        // Anyone else's is refused.
        let (stranger, _) = ids(cell + 1, cell);
        rustix::fs::chown(&dir, Some(stranger), None).unwrap();
        let err = share_dir_with_cell(&dir, cell).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Ownership {
                    operation: "check the socket directory",
                    ..
                }
            ),
            "{err:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_socket_directory_is_shared_only_as_a_real_directory() {
        let dir = owned_test_dir().with_extension("share");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("link")).unwrap();
        let euid = rustix::process::geteuid().as_raw();
        for name in ["link", "missing"] {
            let err = share_dir_with_cell(&dir.join(name), euid).unwrap_err();
            assert!(
                matches!(
                    &err,
                    Error::Ownership {
                        operation: "open the socket directory",
                        ..
                    }
                ),
                "{name}: {err:?}"
            );
        }
        // The broker's own user as the cell: kept if the owner can
        // traverse it, refused if not. (Root traverses regardless, so the
        // refusal shows only unprivileged.)
        share_dir_with_cell(&dir.join("real"), euid).unwrap();
        if euid != 0 {
            std::fs::set_permissions(
                dir.join("real"),
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            )
            .unwrap();
            let err = share_dir_with_cell(&dir.join("real"), euid).unwrap_err();
            assert!(
                matches!(
                    &err,
                    Error::Ownership {
                        operation: "check the socket directory",
                        ..
                    }
                ),
                "{err:?}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_directory_is_shared_only_by_its_broker_or_its_cell() {
        // Unprivileged: the cell is the broker's own user.
        assert_eq!(dir_share(1000, 1000, 1000), DirShare::Keep);
        assert_eq!(dir_share(1001, 1000, 1000), DirShare::Refuse);
        // Root: the broker's own, or one an earlier version gave the cell.
        assert_eq!(dir_share(0, 0, 999), DirShare::Share);
        assert_eq!(dir_share(999, 0, 999), DirShare::Share);
        assert_eq!(dir_share(1000, 0, 999), DirShare::Refuse);
    }

    #[test]
    fn only_the_owner_execute_bit_lets_the_owner_traverse() {
        assert!(owner_traverses(0o40_700));
        assert!(owner_traverses(0o40_100));
        assert!(!owner_traverses(0o40_600));
        assert!(!owner_traverses(0o40_011));
        assert!(!owner_traverses(0o40_000));
    }
}
