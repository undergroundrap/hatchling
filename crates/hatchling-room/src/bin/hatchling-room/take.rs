//! A piece of a file in the workspace, for the file to be handed to the
//! owner (WO-041): read by the supervisor itself, as the Room's user.
//!
//! The file is opened from the workspace's own directory, beneath it: no
//! step of its path may be a link, lead out of the workspace or onto
//! another file system. It is opened without waiting, and only a file is
//! read, so a pipe with nobody writing to it can't hold the one thread
//! there is.
//!
//! Each piece is read on its own, and nothing is kept between pieces. A
//! piece says the file's length and which file it is, as they were before
//! it was read; a file that changed while the piece was read is said to
//! have, and no piece of it is handed back. Whoever puts the pieces
//! together holds the file to being the same from its first to its last.
//!
//! This is honesty and liveness, not secrecy: the Room's user can copy
//! anything it can read into the workspace, and hand that over.

use std::fs::{File, Metadata};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::Path;

use hatchling_core::room::{NoTake, TAKE_BYTES, Took, WORKSPACE};
use rustix::fs::{Mode, OFlags, ResolveFlags};
use rustix::io::Errno;

/// Why a file couldn't be opened, from the kernel's word for it.
pub(crate) const fn not_opened(errno: Errno) -> NoTake {
    match errno {
        Errno::NOENT | Errno::NOTDIR => NoTake::NoSuchFile,
        Errno::ACCESS | Errno::PERM => NoTake::Denied,
        // A link at any step: none is followed.
        Errno::LOOP => NoTake::Link,
        // A step out of the workspace, or onto another file system.
        Errno::XDEV => NoTake::Outside,
        // A socket, or a device with nothing behind it.
        Errno::NXIO | Errno::OPNOTSUPP => NoTake::NotAFile,
        _ => NoTake::Failed,
    }
}

/// When the file last changed, in nanoseconds since the epoch: a time
/// before it is said as the epoch.
fn changed(meta: &Metadata) -> u64 {
    let seconds = u64::try_from(meta.ctime()).unwrap_or(0);
    let nanos = u64::try_from(meta.ctime_nsec()).unwrap_or(0);
    seconds.saturating_mul(1_000_000_000).saturating_add(nanos)
}

/// Why a path in the workspace wasn't opened beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unopened {
    /// The path isn't under [`WORKSPACE`].
    Outside,
    /// The workspace itself couldn't be opened.
    Failed,
    /// The kernel's word for why the path couldn't be.
    Kernel(Errno),
}

/// What a path beneath the workspace is opened as: only ever to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Opening {
    /// Whatever is there: whether it is a file is looked at once open.
    Anything,
    /// A folder, or nothing.
    Folder,
}

/// What is at `path`, a path in the Room under [`WORKSPACE`], opened to
/// read, without waiting, as `opening` says, beneath `workspace`, the
/// directory that is the workspace here: no step of it a link, out of the
/// workspace or onto another file system.
pub(crate) fn beneath(workspace: &Path, path: &str, opening: Opening) -> Result<OwnedFd, Unopened> {
    let with = match opening {
        Opening::Anything => OFlags::empty(),
        Opening::Folder => OFlags::DIRECTORY,
    };
    let inner = path
        .strip_prefix(WORKSPACE)
        .and_then(|rest| rest.strip_prefix('/'))
        .ok_or(Unopened::Outside)?;
    let root = rustix::fs::open(
        workspace,
        OFlags::PATH
            .union(OFlags::DIRECTORY)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )
    .map_err(|_| Unopened::Failed)?;
    rustix::fs::openat2(
        &root,
        inner,
        OFlags::RDONLY
            .union(OFlags::NONBLOCK)
            .union(OFlags::NOCTTY)
            .union(OFlags::CLOEXEC)
            .union(with),
        Mode::empty(),
        ResolveFlags::BENEATH
            .union(ResolveFlags::NO_SYMLINKS)
            .union(ResolveFlags::NO_MAGICLINKS)
            .union(ResolveFlags::NO_XDEV),
    )
    .map_err(Unopened::Kernel)
}

/// The file at `path`, a path in the Room under [`WORKSPACE`], opened
/// beneath `workspace`, the directory that is the workspace here.
fn opened(workspace: &Path, path: &str) -> Result<File, NoTake> {
    let file = beneath(workspace, path, Opening::Anything).map_err(|why| match why {
        Unopened::Outside => NoTake::Outside,
        Unopened::Failed => NoTake::Failed,
        Unopened::Kernel(errno) => not_opened(errno),
    })?;
    Ok(File::from(file))
}

/// What is said of an open file: whether it is a file, and which file it
/// is, as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seen {
    /// Whether it is a regular file.
    pub(crate) file: bool,
    /// Its length, in bytes.
    pub(crate) size: u64,
    /// Its inode number.
    pub(crate) ino: u64,
    /// When it last changed, in nanoseconds since the epoch.
    pub(crate) changed: u64,
}

/// A file a piece is read from: what is said of it, and its bytes from a
/// place, as an open file gives them.
pub(crate) trait Readable {
    /// What is said of it now.
    fn seen(&self) -> std::io::Result<Seen>;
    /// Reads into `bytes` from byte `at`: how many were read, none at its
    /// end.
    fn read_from(&self, bytes: &mut [u8], at: u64) -> std::io::Result<usize>;
}

impl Readable for File {
    fn seen(&self) -> std::io::Result<Seen> {
        let meta = self.metadata()?;
        Ok(Seen {
            file: meta.is_file(),
            size: meta.size(),
            ino: meta.ino(),
            changed: changed(&meta),
        })
    }

    fn read_from(&self, bytes: &mut [u8], at: u64) -> std::io::Result<usize> {
        self.read_at(bytes, at)
    }
}

/// A piece of the file at `path` in the workspace: at most [`TAKE_BYTES`]
/// of it from byte `at`, with its length and which file it is.
pub(crate) fn took(workspace: &Path, path: &str, at: u64) -> Result<Took, NoTake> {
    piece(&opened(workspace, path)?, at)
}

/// A piece of `file`, at most [`TAKE_BYTES`] of it from byte `at`, read
/// only if it is a file, and handed back only if it is the same file, as
/// long and unchanged, after the piece is read as before.
pub(crate) fn piece(file: &impl Readable, at: u64) -> Result<Took, NoTake> {
    let before = file.seen().map_err(|_| NoTake::Failed)?;
    if !before.file {
        return Err(NoTake::NotAFile);
    }
    // A piece is asked for from where the last one ended: a file that
    // ends before it is shorter than it was.
    if at > before.size {
        return Err(NoTake::Changed);
    }
    let wanted = usize::try_from(before.size - at)
        .unwrap_or(usize::MAX)
        .min(TAKE_BYTES);
    let mut bytes = vec![0_u8; wanted];
    let mut filled = 0_usize;
    while let Some(rest) = bytes.get_mut(filled..).filter(|rest| !rest.is_empty()) {
        let offset = at.saturating_add(u64::try_from(filled).unwrap_or(u64::MAX));
        match file.read_from(rest, offset) {
            // The file is shorter than it was: it changed.
            Ok(0) => return Err(NoTake::Changed),
            Ok(count) => filled = filled.saturating_add(count),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(NoTake::Failed),
        }
    }
    let after = file.seen().map_err(|_| NoTake::Failed)?;
    if after != before {
        return Err(NoTake::Changed);
    }
    Ok(Took {
        bytes,
        size: before.size,
        ino: before.ino,
        changed: before.changed,
    })
}
