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
pub(crate) const fn ids(uid: u32, gid: u32) -> (rustix::fs::Uid, rustix::fs::Gid) {
    // SAFETY: `Uid::from_raw` only wraps the u32; the unsafety is
    // rustix's marker for raw-id construction, and the value comes from
    // the validated cell identity or the process itself.
    let owner = unsafe { rustix::fs::Uid::from_raw(uid) };
    // SAFETY: `Gid::from_raw` only wraps the u32, as for the uid above.
    let group = unsafe { rustix::fs::Gid::from_raw(gid) };
    (owner, group)
}

/// Gives the file open at `file` to `uid`, its group as it is.
///
/// What a root process writing for another user does with what it makes
/// there (WO-017). A file is left as it is when `uid` is this process's
/// own effective uid.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if the change is refused.
pub fn give_to(file: impl rustix::fd::AsFd, uid: u32) -> Result<(), Error> {
    if uid == rustix::process::geteuid().as_raw() {
        return Ok(());
    }
    let (owner, _) = ids(uid, 0);
    rustix::fs::fchown(file, Some(owner), None).map_err(failed("give a file to its owner"))
}

/// [`give_to`] for the directory at `path`, opened without following a
/// symlink.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if `path` isn't a directory itself, or
/// the change is refused.
pub fn give_dir_to(path: &Path, uid: u32) -> Result<(), Error> {
    let handle = open_dir_nofollow(path).map_err(failed("open a directory to give"))?;
    give_to(&handle, uid)
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
    let (handle, stat) = open_socket_nofollow(path)?;
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

/// Sets the socket at `path` to permission bits `mode`.
///
/// As [`hand_over_socket`], `path` is opened without following symlinks
/// and must be a socket itself; the mode is set on what was opened,
/// through `/proc/self/fd`, never by looking `path` up again.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if nothing is at `path`, if what is
/// there isn't a socket, or if the change is refused.
pub fn chmod_socket(path: &Path, mode: u32) -> Result<(), Error> {
    use rustix::fd::AsRawFd as _;
    let (handle, _) = open_socket_nofollow(path)?;
    let opened = format!("/proc/self/fd/{}", handle.as_raw_fd());
    rustix::fs::chmod(opened.as_str(), rustix::fs::Mode::from_raw_mode(mode))
        .map_err(failed("chmod the socket"))
}

/// Opens `path` without following symlinks, and only if it is a socket
/// itself: the handle and what it is.
fn open_socket_nofollow(path: &Path) -> Result<(rustix::fd::OwnedFd, rustix::fs::Stat), Error> {
    const FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::PATH
        .union(rustix::fs::OFlags::NOFOLLOW)
        .union(rustix::fs::OFlags::CLOEXEC);
    let handle = rustix::fs::open(path, FLAGS, rustix::fs::Mode::empty())
        .map_err(failed("open the socket"))?;
    let stat = rustix::fs::fstat(&handle).map_err(failed("fstat the socket"))?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::Socket {
        Ok((handle, stat))
    } else {
        Err(refused("check the socket", "not a socket"))
    }
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

/// Lets the cell running as uid `cell` and gid `cell_gid` traverse the
/// socket directory at `path`.
///
/// The directory stays the broker's ([`dir_share`]): the cell can reach
/// the socket in it, and can't add, remove or replace anything there.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if `path` isn't a directory itself (a
/// symlink to one included), if it belongs to anyone but the broker or
/// the cell, if the cell couldn't traverse it, or if a change is refused.
pub fn share_dir_with_cell(path: &Path, cell: u32, cell_gid: u32) -> Result<(), Error> {
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
            let (owner, group) = ids(euid, cell_gid);
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
/// closed to group and others ([`private_dir_ok`]). Its mode is then
/// set to 0700 exactly through that handle.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if the directory can't be made (an
/// existing entry's error is `AlreadyExists`), if the check fails, or
/// if the mode change is refused.
pub fn create_private_dir(path: &Path) -> Result<(), Error> {
    // Any umask can only remove bits from 0700, so the directory is never
    // open to anyone else; the fchmod below restores the owner's bits a
    // strict umask took. The umask itself isn't touched: a caller may
    // already hold its lock (`with_umask` isn't reentrant).
    rustix::fs::mkdir(path, rustix::fs::Mode::from_raw_mode(0o700)).map_err(failed("mkdir"))?;
    let handle = open_dir_nofollow(path).map_err(failed("open the new directory"))?;
    let stat = rustix::fs::fstat(&handle).map_err(failed("fstat"))?;
    if !private_dir_ok(
        stat.st_uid,
        stat.st_mode,
        rustix::process::geteuid().as_raw(),
    ) {
        return Err(refused(
            "check the new directory",
            "it isn't the broker's alone",
        ));
    }
    rustix::fs::fchmod(&handle, rustix::fs::Mode::from_raw_mode(0o700))
        .map_err(failed("chmod the new directory"))
}

/// Makes `path` a directory only this process's user can use.
///
/// A new one is made as by [`create_private_dir`]. One already there
/// must be a directory itself, owned by the effective uid; its mode is
/// then set to 0700 through a handle opened without following symlinks.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if the directory can't be made, if what
/// is at `path` isn't a directory of this process's user, or if the
/// change is refused.
pub fn ensure_private_dir(path: &Path) -> Result<(), Error> {
    match create_private_dir(path) {
        Err(Error::Ownership { source, .. }) if source.kind() == io::ErrorKind::AlreadyExists => {}
        made => return made,
    }
    let handle = open_dir_nofollow(path).map_err(failed("open the directory"))?;
    let stat = rustix::fs::fstat(&handle).map_err(failed("fstat the directory"))?;
    if stat.st_uid != rustix::process::geteuid().as_raw() {
        return Err(refused("check the directory", "it isn't the broker's"));
    }
    rustix::fs::fchmod(&handle, rustix::fs::Mode::from_raw_mode(0o700))
        .map_err(failed("chmod the directory"))
}

/// Writes `bytes` to a new file `name` in the directory `dir`, for
/// `owner`, who is given it (WO-017: the spawner writing an approved
/// delivery into the owner's workspace).
///
/// `dir` is opened without following a link, and must be a directory
/// `owner` owns: what is written lands in a directory of theirs, whatever
/// they change on the way to it. `name` is created there, relative to
/// that handle, never replacing anything and never through a link
/// (`O_EXCL | O_NOFOLLOW`), mode 0600, and given to `owner` before a
/// byte is written.
///
/// # Errors
///
/// Returns [`Error::Ownership`] if `dir` isn't a directory of `owner`'s,
/// `name` isn't one plain name, the file exists or can't be made, or it
/// can't be given or written.
pub fn write_new_file_for(dir: &Path, name: &str, bytes: &[u8], owner: u32) -> Result<(), Error> {
    use std::io::Write as _;
    /// How the file is made: new, and never through a symlink.
    const FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::WRONLY
        .union(rustix::fs::OFlags::CREATE)
        .union(rustix::fs::OFlags::EXCL)
        .union(rustix::fs::OFlags::NOFOLLOW)
        .union(rustix::fs::OFlags::CLOEXEC);
    if !plain_name(name) {
        return Err(refused(
            "make a file in the directory",
            "not one plain name",
        ));
    }
    let handle = open_dir_nofollow(dir).map_err(failed("open the directory"))?;
    let stat = rustix::fs::fstat(&handle).map_err(failed("fstat the directory"))?;
    if stat.st_uid != owner {
        return Err(refused(
            "make a file in the directory",
            "the directory isn't the owner's",
        ));
    }
    let made = rustix::fs::openat(&handle, name, FLAGS, rustix::fs::Mode::from_raw_mode(0o600))
        .map_err(failed("make the file"))?;
    let mut file = std::fs::File::from(made);
    give_to(&file, owner)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| Error::Ownership {
            operation: "write the file",
            source,
        })
}

/// Gives the file at `path` to `owner`, made first if absent: empty,
/// 0600 (WO-017: a log the broker's user appends to, beside a directory
/// it may not make files in). It is opened without following a link,
/// and must be a regular file.
///
/// # Errors
///
/// [`Error::Ownership`] when it can't be opened or made, isn't a regular
/// file, or can't be given.
pub fn hand_file_to(path: &Path, owner: u32) -> Result<(), Error> {
    /// How the file is opened: to append, made if absent, never through
    /// a symlink.
    const FLAGS: rustix::fs::OFlags = rustix::fs::OFlags::WRONLY
        .union(rustix::fs::OFlags::APPEND)
        .union(rustix::fs::OFlags::CREATE)
        .union(rustix::fs::OFlags::NOFOLLOW)
        .union(rustix::fs::OFlags::NONBLOCK)
        .union(rustix::fs::OFlags::CLOEXEC);
    let file = rustix::fs::open(path, FLAGS, rustix::fs::Mode::from_raw_mode(0o600))
        .map_err(failed("open the file"))?;
    let stat = rustix::fs::fstat(&file).map_err(failed("fstat the file"))?;
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
        return Err(refused("give the file", "not a regular file"));
    }
    give_to(&file, owner)
}

/// Whether `name` is one plain name in a directory: not empty, not `.` or
/// `..`, with no `/` and no NUL.
fn plain_name(name: &str) -> bool {
    !(name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0'))
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
    /// A file is handed over made if absent (empty, 0600) and kept if
    /// present, never through a link, and only a regular file; given to
    /// another user only as root.
    #[test]
    fn a_file_is_handed_over_made_if_absent_and_never_through_a_link() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = std::env::temp_dir().join(format!("hatchling-hand-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let me = rustix::process::geteuid().as_raw();
        let log = dir.join("activity");
        hand_file_to(&log, me).unwrap();
        let made = std::fs::symlink_metadata(&log).unwrap();
        assert_eq!((made.len(), made.permissions().mode() & 0o777), (0, 0o600));
        std::fs::write(&log, b"kept").unwrap();
        hand_file_to(&log, me).unwrap();
        assert_eq!(std::fs::read(&log).unwrap(), b"kept");
        std::os::unix::fs::symlink(dir.join("elsewhere"), dir.join("linked")).unwrap();
        assert!(hand_file_to(&dir.join("linked"), me).is_err());
        assert!(!dir.join("elsewhere").exists());
        let refused = hand_file_to(&dir, me).unwrap_err();
        assert!(refused.to_string().contains("open the file"), "{refused}");
        // A device opens, and is refused for what it is (given to root
        // as root, and to this user otherwise: nothing changes).
        let device_owner = std::fs::metadata("/dev/null").unwrap().uid();
        let refused = hand_file_to(Path::new("/dev/null"), me).unwrap_err();
        assert!(refused.to_string().contains("not a regular file"), "{refused}");
        assert_eq!(std::fs::metadata("/dev/null").unwrap().uid(), device_owner);
        if me == 0 {
            hand_file_to(&log, 4343).unwrap();
            assert_eq!(std::fs::symlink_metadata(&log).unwrap().uid(), 4343);
        } else {
            assert!(hand_file_to(&log, me.wrapping_add(4242)).is_err());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One plain name: no path, no NUL, neither `.` nor `..`; a name
    /// that merely starts with dots is plain.
    #[test]
    fn a_plain_name_is_one_name_in_a_directory() {
        for plain in ["report.md", "...", "..a", ".hidden"] {
            assert!(plain_name(plain), "{plain:?}");
        }
        for not in ["", ".", "..", "a/b", "/", "a\0b"] {
            assert!(!plain_name(not), "{not:?}");
        }
    }

    /// A new file is written only in a directory of its owner's, reached
    /// without a link, under one plain name, never over another file;
    /// written for another user only as root.
    #[test]
    fn a_new_file_is_written_only_in_its_owners_directory() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let dir = std::env::temp_dir().join(format!("hatchling-new-file-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let me = rustix::process::geteuid().as_raw();
        write_new_file_for(&dir, "report.md", b"hello", me).unwrap();
        let made = std::fs::metadata(dir.join("report.md")).unwrap();
        assert_eq!((made.uid(), made.permissions().mode() & 0o777), (me, 0o600));
        assert_eq!(std::fs::read(dir.join("report.md")).unwrap(), b"hello");
        // Never over a file, a link included.
        assert!(write_new_file_for(&dir, "report.md", b"again", me).is_err());
        std::os::unix::fs::symlink("/etc/passwd", dir.join("linked")).unwrap();
        assert!(write_new_file_for(&dir, "linked", b"x", me).is_err());
        // Refused by name, before the kernel is asked: "a/b" would
        // otherwise be made in the directory "a".
        std::fs::create_dir(dir.join("a")).unwrap();
        for name in ["", ".", "..", "a/b", "a\0b"] {
            let refused = write_new_file_for(&dir, name, b"x", me).unwrap_err();
            assert!(
                refused.to_string().contains("not one plain name"),
                "{name:?}"
            );
        }
        assert!(!dir.join("a").join("b").exists());
        // Not through a linked directory, nor into another owner's.
        std::os::unix::fs::symlink(&dir, dir.with_extension("link")).unwrap();
        assert!(write_new_file_for(&dir.with_extension("link"), "via", b"x", me).is_err());
        // Refused before anything is made: not refused later, when the file
        // couldn't be given away.
        let theirs = write_new_file_for(&dir, "theirs", b"x", me.wrapping_add(4242)).unwrap_err();
        assert!(theirs.to_string().contains("isn't the owner's"), "{theirs}");
        assert!(!dir.join("theirs").exists());
        if me == 0 {
            std::os::unix::fs::chown(&dir, Some(4242), None).unwrap();
            write_new_file_for(&dir, "given", b"g", 4242).unwrap();
            assert_eq!(std::fs::metadata(dir.join("given")).unwrap().uid(), 4242);
        }
        std::fs::remove_file(dir.with_extension("link")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A file is given to another user only as root; given to this
    /// process's own user, it is left as it is, and a symlink is never
    /// opened as the directory it names.
    #[test]
    fn a_file_is_given_to_its_owner() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = std::env::temp_dir().join(format!("hatchling-give-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let file = std::fs::File::create(dir.join("f")).unwrap();
        let euid = rustix::process::geteuid().as_raw();
        give_to(&file, euid).unwrap();
        give_dir_to(&dir, euid).unwrap();
        let other = euid.wrapping_add(4242);
        let given = give_to(&file, other);
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(give_dir_to(&link, euid).is_err());
        if euid == 0 {
            given.unwrap();
            give_dir_to(&dir, other).unwrap();
            assert_eq!(file.metadata().unwrap().uid(), other);
            assert_eq!(std::fs::metadata(&dir).unwrap().uid(), other);
        } else {
            assert!(given.is_err());
            assert_eq!(file.metadata().unwrap().uid(), euid);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
    fn a_socket_mode_is_set_only_on_a_socket_itself() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::os::unix::net::UnixListener;
        let dir = owned_test_dir().with_extension("chmod");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("broker.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        chmod_socket(&socket, 0o660).unwrap();
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&socket), 0o660);
        chmod_socket(&socket, 0o600).unwrap();
        assert_eq!(mode(&socket), 0o600);
        // A symlink to the socket or to a file is refused, and neither
        // target changes.
        let file = dir.join("file");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&socket, dir.join("to-socket")).unwrap();
        std::os::unix::fs::symlink(&file, dir.join("to-file")).unwrap();
        for name in ["to-socket", "to-file", "file"] {
            let err = chmod_socket(&dir.join(name), 0o666).unwrap_err();
            assert!(
                matches!(
                    &err,
                    Error::Ownership {
                        operation: "check the socket",
                        ..
                    }
                ),
                "{name}: {err:?}"
            );
        }
        assert_eq!(mode(&socket), 0o600);
        assert_eq!(mode(&file), 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_socket_itself_opens_as_one() {
        use std::os::unix::net::UnixListener;
        let dir = owned_test_dir().with_extension("open-socket");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("broker.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let (_, stat) = open_socket_nofollow(&socket).unwrap();
        assert_eq!(
            rustix::fs::FileType::from_raw_mode(stat.st_mode),
            rustix::fs::FileType::Socket
        );
        assert_eq!(stat.st_uid, rustix::process::geteuid().as_raw());
        std::fs::write(dir.join("file"), b"x").unwrap();
        std::os::unix::fs::symlink(&socket, dir.join("to-socket")).unwrap();
        for name in ["to-socket", "file"] {
            assert!(
                matches!(
                    open_socket_nofollow(&dir.join(name)),
                    Err(Error::Ownership {
                        operation: "check the socket",
                        ..
                    })
                ),
                "{name}"
            );
        }
        assert!(matches!(
            open_socket_nofollow(&dir.join("missing")),
            Err(Error::Ownership {
                operation: "open the socket",
                ..
            })
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_private_dir_is_0700_whatever_the_umask() {
        use crate::{ForkResult, child_exit_code, exit_child, fork};
        let dir = owned_test_dir().with_extension("umask");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let made = dir.join("made");
        // In a child: a umask that would also strip the owner's bits must
        // not reach the other tests' files.
        let child = match fork().expect("fork") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let _ = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o377));
                if create_private_dir(&made).is_err() {
                    exit_child(2);
                }
                let Ok(stat) = rustix::fs::stat(&made) else {
                    exit_child(3)
                };
                exit_child(if stat.st_mode & 0o777 == 0o700 { 0 } else { 4 });
            }
        };
        let status = child
            .wait_deadline(std::time::Duration::from_secs(30))
            .expect("bounded wait");
        assert_eq!(
            child_exit_code(status),
            0,
            "exit 2: not made; 3: not found; 4: not 0700"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_existing_dir_is_made_private_only_if_it_is_ours_itself() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = owned_test_dir().with_extension("ensure");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("open")).unwrap();
        std::fs::set_permissions(dir.join("open"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
        ensure_private_dir(&dir.join("open")).unwrap();
        assert_eq!(mode(&dir.join("open")), 0o700, "an existing one is pinned");
        ensure_private_dir(&dir.join("fresh")).unwrap();
        assert_eq!(mode(&dir.join("fresh")), 0o700, "a missing one is made");
        // A symlink to a directory is refused, and the target is untouched.
        std::fs::create_dir(dir.join("target")).unwrap();
        std::fs::set_permissions(dir.join("target"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::os::unix::fs::symlink(dir.join("target"), dir.join("link")).unwrap();
        let err = ensure_private_dir(&dir.join("link")).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Ownership {
                    operation: "open the directory",
                    ..
                }
            ),
            "{err:?}"
        );
        assert_eq!(mode(&dir.join("target")), 0o755);
        // Anything else that fails is reported as it is.
        let err = ensure_private_dir(&dir.join("missing").join("deeper")).unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Ownership {
                    operation: "mkdir",
                    ..
                }
            ),
            "{err:?}"
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
        // A gid of the cell's own, not its uid's number.
        let (cell, cell_gid) = (65_000, 65_100);
        share_dir_with_cell(&dir, cell, cell_gid).unwrap();
        hand_over_socket(&socket, cell, cell_gid).unwrap();
        let dir_meta = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!((dir_meta.uid(), dir_meta.gid()), (0, cell_gid));
        assert_eq!(dir_meta.mode() & 0o7777, 0o710);
        let socket_meta = std::fs::symlink_metadata(&socket).unwrap();
        assert_eq!((socket_meta.uid(), socket_meta.gid()), (cell, cell_gid));
        // A directory an earlier version gave the cell is taken back.
        let (owner, group) = ids(cell, cell_gid);
        rustix::fs::chown(&dir, Some(owner), Some(group)).unwrap();
        share_dir_with_cell(&dir, cell, cell_gid).unwrap();
        let dir_meta = std::fs::symlink_metadata(&dir).unwrap();
        assert_eq!((dir_meta.uid(), dir_meta.gid()), (0, cell_gid));
        // Anyone else's is refused.
        let (stranger, _) = ids(cell + 1, cell);
        rustix::fs::chown(&dir, Some(stranger), None).unwrap();
        let err = share_dir_with_cell(&dir, cell, cell_gid).unwrap_err();
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
            let err = share_dir_with_cell(&dir.join(name), euid, euid).unwrap_err();
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
        share_dir_with_cell(&dir.join("real"), euid, euid).unwrap();
        if euid != 0 {
            std::fs::set_permissions(
                dir.join("real"),
                std::os::unix::fs::PermissionsExt::from_mode(0o600),
            )
            .unwrap();
            let err = share_dir_with_cell(&dir.join("real"), euid, euid).unwrap_err();
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
