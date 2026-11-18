//! A page of what a folder in the workspace holds, for the folder to be
//! handed to the owner (WO-043): read by the supervisor itself, as the
//! Room's user.
//!
//! The folder is opened as a file to take is ([`crate::take::beneath`]):
//! beneath the workspace, through no link and onto no other file system.
//! The workspace itself may be listed, its inbox seen as another mount.
//! Each name in it is looked at following no link: one on another mount,
//! the inbox among them, is said to be a mount, whatever it is.
//!
//! A page is read on its own, and nothing is kept between pages. Its
//! names are read whole, put in the order of their bytes, and read again
//! after the page: a folder whose names, inode or change time aren't the
//! same after as before is said to have changed, and no page of it is
//! handed back. Whoever puts the pages together holds the folder to being
//! the same from its first to its last.
//!
//! This is honesty and liveness, not a boundary: a change time is as fine
//! as the kernel's clock, and the Room's user can change a folder between
//! two looks at it. What a page says a name is, is never trusted on its
//! own: a file in it is read by a take, which decides.

use std::ffi::CString;
use std::os::fd::OwnedFd;
use std::path::Path;

use hatchling_core::room::{
    Entry, Kind, LIST_FOLDER_NAMES, LIST_PAGE, Listed, NoList, WORKSPACE, is_name,
};
use rustix::fs::{AtFlags, Dir, FileType, StatxFlags};
use rustix::io::Errno;

/// What is said of the folder itself: which folder it is, and on which
/// mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seen {
    /// Its inode number.
    pub(crate) ino: u64,
    /// When it last changed, in nanoseconds since the epoch.
    pub(crate) changed: u64,
    /// The mount it is on.
    pub(crate) mount: u64,
}

/// What is said of one name in the folder, following no link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Looked {
    /// What it is.
    pub(crate) kind: Kind,
    /// Its inode number.
    pub(crate) ino: u64,
    /// Its length, in bytes.
    pub(crate) size: u64,
    /// The mount it is on.
    pub(crate) mount: u64,
}

/// A folder a page is read from.
pub(crate) trait Folder {
    /// What is said of it now.
    fn seen(&self) -> Result<Seen, NoList>;
    /// All its names, from its start, but `.` and `..`: more than
    /// [`LIST_FOLDER_NAMES`] are too many.
    fn names(&mut self) -> Result<Vec<Vec<u8>>, NoList>;
    /// What is said of the name `name` in it.
    fn look(&self, name: &[u8]) -> Result<Looked, NoList>;
}

/// Why a folder couldn't be opened, from the kernel's word for it.
pub(crate) const fn not_opened(errno: Errno) -> NoList {
    match errno {
        Errno::NOENT => NoList::NoSuchFolder,
        // A step, or the last, that isn't a folder.
        Errno::NOTDIR => NoList::NotAFolder,
        Errno::ACCESS | Errno::PERM => NoList::Denied,
        // A link at any step: none is followed.
        Errno::LOOP => NoList::Link,
        // A step out of the workspace, or onto another file system.
        Errno::XDEV => NoList::Outside,
        _ => NoList::Failed,
    }
}

/// Why a folder, or a name in it, couldn't be read or looked at, once it
/// was open.
pub(crate) const fn not_read(errno: Errno) -> NoList {
    match errno {
        // The folder, or a name in it, went while it was read.
        Errno::NOENT => NoList::Changed,
        Errno::ACCESS | Errno::PERM => NoList::Denied,
        _ => NoList::Failed,
    }
}

/// When a thing last changed, in nanoseconds since the epoch: a time
/// before it is said as the epoch.
pub(crate) fn nanos(seconds: i64, nanos: u32) -> u64 {
    u64::try_from(seconds)
        .unwrap_or(0)
        .saturating_mul(1_000_000_000)
        .saturating_add(u64::from(nanos))
}

/// What is asked of the folder itself.
const SEEN: StatxFlags = StatxFlags::INO
    .union(StatxFlags::CTIME)
    .union(StatxFlags::MNT_ID);

/// What is asked of a name in it.
const LOOKED: StatxFlags = StatxFlags::TYPE
    .union(StatxFlags::INO)
    .union(StatxFlags::SIZE)
    .union(StatxFlags::MNT_ID);

/// What the folder is, from what the kernel said of it, with `mask` the
/// fields it filled in: all of [`SEEN`], or it isn't known.
pub(crate) fn seen_from(
    mask: u32,
    ino: u64,
    seconds: i64,
    nanoseconds: u32,
    mount: u64,
) -> Result<Seen, NoList> {
    if !StatxFlags::from_bits_retain(mask).contains(SEEN) {
        return Err(NoList::Failed);
    }
    Ok(Seen {
        ino,
        changed: nanos(seconds, nanoseconds),
        mount,
    })
}

/// What a name is, from what the kernel said of it, with `mask` the
/// fields it filled in: all of [`LOOKED`], or it isn't known.
pub(crate) fn looked_from(
    mask: u32,
    mode: u16,
    ino: u64,
    size: u64,
    mount: u64,
) -> Result<Looked, NoList> {
    if !StatxFlags::from_bits_retain(mask).contains(LOOKED) {
        return Err(NoList::Failed);
    }
    Ok(Looked {
        kind: kind_of(mode),
        ino,
        size,
        mount,
    })
}

/// What a name is, by its mode.
pub(crate) fn kind_of(mode: u16) -> Kind {
    match FileType::from_raw_mode(u32::from(mode)) {
        FileType::RegularFile => Kind::File,
        FileType::Directory => Kind::Folder,
        FileType::Symlink => Kind::Link,
        FileType::Fifo
        | FileType::Socket
        | FileType::CharacterDevice
        | FileType::BlockDevice
        | FileType::Unknown => Kind::Other,
    }
}

/// A folder open in the workspace: its descriptor, and its names read
/// from another descriptor of the same open folder.
struct Opened {
    fd: OwnedFd,
    dir: Dir,
}

impl Opened {
    /// The folder at `path`, the workspace or a path in the Room under it,
    /// opened beneath `workspace`.
    fn open(workspace: &Path, path: &str) -> Result<Self, NoList> {
        // The workspace itself is its own `.`, beneath itself.
        let path = if path == WORKSPACE {
            format!("{WORKSPACE}/.")
        } else {
            path.to_owned()
        };
        let fd = crate::take::beneath(workspace, &path, crate::take::Opening::Folder).map_err(
            |why| match why {
                crate::take::Unopened::Outside => NoList::Outside,
                crate::take::Unopened::Failed => NoList::Failed,
                crate::take::Unopened::Kernel(errno) => not_opened(errno),
            },
        )?;
        let copy = rustix::io::fcntl_dupfd_cloexec(&fd, 0).map_err(|_| NoList::Failed)?;
        let dir = Dir::new(copy).map_err(|_| NoList::Failed)?;
        Ok(Self { fd, dir })
    }
}

impl Folder for Opened {
    fn seen(&self) -> Result<Seen, NoList> {
        let seen = rustix::fs::statx(&self.fd, "", AtFlags::EMPTY_PATH, SEEN).map_err(not_read)?;
        seen_from(
            seen.stx_mask,
            seen.stx_ino,
            seen.stx_ctime.tv_sec,
            seen.stx_ctime.tv_nsec,
            seen.stx_mnt_id,
        )
    }

    fn names(&mut self) -> Result<Vec<Vec<u8>>, NoList> {
        self.dir.rewind();
        let mut names = Vec::new();
        while let Some(entry) = self.dir.read() {
            let entry = entry.map_err(not_read)?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if names.len() >= usize::try_from(LIST_FOLDER_NAMES).unwrap_or(usize::MAX) {
                return Err(NoList::TooMany);
            }
            names.push(name.to_vec());
        }
        Ok(names)
    }

    fn look(&self, name: &[u8]) -> Result<Looked, NoList> {
        let name = CString::new(name).map_err(|_| NoList::Failed)?;
        let flags = AtFlags::SYMLINK_NOFOLLOW.union(AtFlags::NO_AUTOMOUNT);
        let seen = rustix::fs::statx(&self.fd, &name, flags, LOOKED).map_err(not_read)?;
        looked_from(
            seen.stx_mask,
            seen.stx_mode,
            seen.stx_ino,
            seen.stx_size,
            seen.stx_mnt_id,
        )
    }
}

/// A page of the folder at `path` in the workspace: its names after
/// `after`, at most [`LIST_PAGE`] of them, each with what it is.
pub(crate) fn listed(workspace: &Path, path: &str, after: &[u8]) -> Result<Listed, NoList> {
    page(&mut Opened::open(workspace, path)?, after)
}

/// A page of `folder` after `after`, handed back only if the folder is
/// the same, its names, inode and change time, after the page is read as
/// before.
pub(crate) fn page(folder: &mut impl Folder, after: &[u8]) -> Result<Listed, NoList> {
    let before = folder.seen()?;
    let mut names = folder.names()?;
    names.sort_unstable();
    // A name read twice: the folder changed while it was read.
    if names.windows(2).any(|pair| pair.first() == pair.get(1)) {
        return Err(NoList::Changed);
    }
    // What no folder can hold isn't handed back as a name.
    if !names.iter().all(|name| is_name(name)) {
        return Err(NoList::Failed);
    }
    let total = u32::try_from(names.len()).map_err(|_| NoList::TooMany)?;
    let mut entries = Vec::new();
    for name in names
        .iter()
        .filter(|name| name.as_slice() > after)
        .take(LIST_PAGE)
    {
        let looked = folder.look(name)?;
        let (kind, ino, size) = if looked.mount == before.mount {
            match looked.kind {
                Kind::File => (Kind::File, looked.ino, looked.size),
                Kind::Folder => (Kind::Folder, looked.ino, 0),
                kind @ (Kind::Link | Kind::Mount | Kind::Other) => (kind, 0, 0),
            }
        } else {
            // On another mount: what is there isn't the workspace's.
            (Kind::Mount, 0, 0)
        };
        entries.push(Entry {
            name: name.clone(),
            kind,
            ino,
            size,
        });
    }
    let mut again = folder.names()?;
    again.sort_unstable();
    if again != names || folder.seen()? != before {
        return Err(NoList::Changed);
    }
    Ok(Listed {
        ino: before.ino,
        changed: before.changed,
        total,
        entries,
    })
}
