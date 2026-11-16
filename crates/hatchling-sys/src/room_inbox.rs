//! The Room's inbox (WO-041): where a file the owner hands the agent is
//! put, by root, on the room disk.
//!
//! The inbox is a directory of its own at the encrypted volume's root,
//! beside the workspace. The Room sees it read-only, `noexec`, at
//! `/workspace/inbox`, a name inside its own workspace that it can reach
//! but this never writes through. A file is put only through the
//! volume's pinned root: never by a path, never through a link, never
//! over another file. The owner's name for it never reaches the disk: it
//! is named `NNNN-<stem>`, by a number this picks and never gives twice,
//! and a stem whose grammar this checks.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, AsRawFd as _, BorrowedFd, OwnedFd};

use rustix::fs::{AtFlags, CWD, FileType, Mode, OFlags, ResolveFlags};
use rustix::io::Errno;

use crate::Error;
use crate::room_mounts::{failed, refused, require_control};

/// The most bytes one file in the inbox holds.
pub const FILE_BYTES: usize = 8 * 1024 * 1024;

/// The most files the inbox holds.
pub const FILES: usize = 64;

/// The most bytes the inbox holds, its files together.
pub const BYTES: u64 = 256 * 1024 * 1024;

/// The longest stem a file in the inbox is named with.
pub const STEM_BYTES: usize = 64;

/// The highest number a file in the inbox is given.
const HIGHEST: u32 = 999_999_999;

/// The most digits a file's number has.
const DIGITS: usize = 9;

/// Where the last number given is kept, so a number is never given twice:
/// not after its file is removed, nor after the highest is. Only this
/// writes it, and it is no file of the inbox's.
const NUMBERED: &str = ".numbered";

/// Where the next [`NUMBERED`] is written before it takes that name.
const NUMBERED_NEXT: &str = ".numbered.new";

/// The inbox's mode: root's, and every user may read it, the Room's
/// among them.
const MODE: u32 = 0o755;

/// A file in the inbox, once it is written: read-only to everyone, root
/// included but for root's override.
const FILE_MODE: u32 = 0o444;

/// Why a file wasn't put in the inbox.
#[derive(Debug, thiserror::Error)]
pub enum PutError {
    /// The inbox doesn't take it: its name, its size, or no room for it.
    /// Said as the owner is told.
    #[error("{0}")]
    Refused(&'static str),
    /// The volume or the inbox wasn't as it must be, or a write failed.
    #[error(transparent)]
    Failed(#[from] Error),
}

/// A stem that isn't one.
const NOT_A_STEM: &str = "that isn't a name the Room's inbox takes";

/// A file past [`FILE_BYTES`], as the owner is told.
pub const TOO_LARGE: &str = "the Room's inbox takes a file of at most 8 MiB";

/// An inbox holding [`FILES`] files.
const NO_MORE_FILES: &str = "the Room's inbox is full: it holds 64 files";

/// An inbox with no room for a file in [`BYTES`].
const NO_MORE_BYTES: &str = "the Room's inbox is full: this file would take it past 256 MiB";

/// An inbox that has given [`HIGHEST`].
const NO_MORE_NUMBERS: &str = "the Room's inbox has given every number it has";

/// A room disk with no space left for the file: the Room's own files can
/// fill the disk the inbox is on.
const DISK_FULL: &str = "the Room's disk is full: the file wasn't put in its inbox";

/// A name to remove that no put gives.
const NOT_A_NAME: &str = "that isn't the name of a file in the Room's inbox";

/// A name to remove that no file in the inbox has.
const NO_SUCH_FILE: &str = "no file of that name is in the Room's inbox";

/// Whether `said` is one of the inbox's own refusals ([`PutError::Refused`]),
/// word for word: plain words for the owner, and nothing of the machine.
#[must_use]
pub fn is_refusal(said: &str) -> bool {
    [
        NOT_A_STEM,
        TOO_LARGE,
        NO_MORE_FILES,
        NO_MORE_BYTES,
        NO_MORE_NUMBERS,
        DISK_FULL,
        NOT_A_NAME,
        NO_SUCH_FILE,
    ]
    .contains(&said)
}

/// Whether `stem` may end the name of a file in the inbox: one to
/// [`STEM_BYTES`] bytes of ASCII letters, digits, `.`, `_` and `-`, not
/// starting with a dot. It is never a path, a hidden name, `.` or `..`.
#[must_use]
pub fn is_stem(stem: &str) -> bool {
    !stem.is_empty()
        && stem.len() <= STEM_BYTES
        && !stem.starts_with('.')
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Makes the inbox of the volume whose root is `volume` the Room's to
/// read: root's, mode `0755`. An inbox made before it was read in the
/// Room is `0700`.
///
/// # Errors
///
/// Refuses a volume root or an inbox that isn't root's alone, an inbox
/// reached by a link or across a mount; and a failed change of mode.
pub fn let_the_room_read(volume: BorrowedFd<'_>) -> Result<(), Error> {
    readable_inbox(volume, 0).map(drop)
}

/// The inbox of the volume whose root is `volume`, opened, and the
/// Room's to read: the volume's root and the inbox held to being
/// `trusted`'s alone (root's, but for a test, which isn't root).
fn readable_inbox(volume: BorrowedFd<'_>, trusted: u32) -> Result<OwnedFd, Error> {
    const OPERATION: &str = "let the room read its inbox";
    require_control(volume, trusted, "verify room volume root ownership")?;
    let inbox = open_inbox(volume)?;
    require_control(inbox.as_fd(), trusted, "verify room inbox ownership")?;
    let stat = rustix::fs::fstat(&inbox).map_err(failed(OPERATION))?;
    if stat.st_mode & 0o7777 != MODE {
        rustix::fs::fchmod(&inbox, Mode::from_raw_mode(MODE)).map_err(failed(OPERATION))?;
    }
    Ok(inbox)
}

/// Puts `bytes` in the inbox of the volume whose root is `volume`.
///
/// The file is new, named `NNNN-<stem>`: numbered one past the highest
/// number the inbox has given, written whole, made read-only, synced,
/// then named. Returns its name.
///
/// The caller holds the room disk's turn, and `volume` is the root of
/// its authenticated, mounted volume.
///
/// # Errors
///
/// [`PutError::Refused`]: a stem that isn't one ([`is_stem`]), a file
/// past [`FILE_BYTES`], an inbox already holding [`FILES`] files or with
/// no room for this one in [`BYTES`], or a disk with no space for it.
/// [`PutError::Failed`]: a volume or an inbox that isn't root's, or a
/// write that failed. Either way, no file is left in the inbox.
pub fn put(volume: BorrowedFd<'_>, stem: &str, bytes: &[u8]) -> Result<String, PutError> {
    put_as(volume, stem, bytes, 0)
}

/// [`put`], held to the volume and the inbox being `trusted`'s.
fn put_as(
    volume: BorrowedFd<'_>,
    stem: &str,
    bytes: &[u8],
    trusted: u32,
) -> Result<String, PutError> {
    if !is_stem(stem) {
        return Err(PutError::Refused(NOT_A_STEM));
    }
    if bytes.len() > FILE_BYTES {
        return Err(PutError::Refused(TOO_LARGE));
    }
    let inbox = readable_inbox(volume, trusted)?;
    let number = next(&held(&inbox)?, bytes.len())?;
    // The number is spent before its file is written: a put that fails
    // after this leaves a gap, never a number given twice.
    record(&inbox, number)?;
    let name = format!("{number:04}-{stem}");
    write_new(&inbox, &name, bytes)?;
    Ok(name)
}

/// Whether `name` is one a file in the inbox is given: a number of four
/// to nine digits, a hyphen, and a stem ([`is_stem`]).
#[must_use]
pub fn is_name(name: &str) -> bool {
    number_of(name.as_bytes()).is_some()
        && name.split_once('-').is_some_and(|(_, stem)| is_stem(stem))
}

/// A file in the inbox, as the owner is shown it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxFile {
    /// Its name in the inbox.
    pub name: String,
    /// Its size.
    pub bytes: u64,
}

/// The files in the inbox of the volume whose root is `volume`, by
/// number: each regular file named as a put names one ([`is_name`]).
///
/// The caller holds the room disk's turn, as for [`put`].
///
/// # Errors
///
/// A volume or an inbox that isn't root's, and an inbox that can't be
/// read.
pub fn list(volume: BorrowedFd<'_>) -> Result<Vec<InboxFile>, Error> {
    list_as(volume, 0)
}

/// [`list`], held to the volume and the inbox being `trusted`'s.
fn list_as(volume: BorrowedFd<'_>, trusted: u32) -> Result<Vec<InboxFile>, Error> {
    const OPERATION: &str = "list the room inbox";
    let inbox = readable_inbox(volume, trusted)?;
    let mut files = Vec::new();
    for entry in rustix::fs::Dir::read_from(&inbox).map_err(failed(OPERATION))? {
        let entry = entry.map_err(failed(OPERATION))?;
        let Ok(name) = entry.file_name().to_str() else {
            continue;
        };
        if !is_name(name) {
            continue;
        }
        let stat = rustix::fs::statat(&inbox, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(failed(OPERATION))?;
        if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile {
            files.push(InboxFile {
                name: name.to_owned(),
                bytes: u64::try_from(stat.st_size).unwrap_or(u64::MAX),
            });
        }
    }
    files.sort_by_key(|file| number_of(file.name.as_bytes()));
    Ok(files)
}

/// Takes the file `name` out of the inbox of the volume whose root is
/// `volume`. Its number is never given again.
///
/// The caller holds the room disk's turn, as for [`put`].
///
/// # Errors
///
/// [`PutError::Refused`]: a name a put doesn't give ([`is_name`]), or no
/// file of that name in the inbox. [`PutError::Failed`]: a volume or an
/// inbox that isn't root's, or a removal that failed.
pub fn remove(volume: BorrowedFd<'_>, name: &str) -> Result<(), PutError> {
    remove_as(volume, name, 0)
}

/// [`remove`], held to the volume and the inbox being `trusted`'s.
fn remove_as(volume: BorrowedFd<'_>, name: &str, trusted: u32) -> Result<(), PutError> {
    const OPERATION: &str = "remove a file from the room inbox";
    if !is_name(name) {
        return Err(PutError::Refused(NOT_A_NAME));
    }
    let inbox = readable_inbox(volume, trusted)?;
    match rustix::fs::unlinkat(&inbox, name, AtFlags::empty()) {
        Ok(()) => {}
        Err(Errno::NOENT) => return Err(PutError::Refused(NO_SUCH_FILE)),
        Err(errno) => return Err(failed(OPERATION)(errno).into()),
    }
    rustix::fs::fsync(&inbox).map_err(|errno| failed(OPERATION)(errno).into())
}

/// The inbox, opened to be read and written: under the volume's root by
/// its one fixed name, never through a link or across a mount.
fn open_inbox(volume: BorrowedFd<'_>) -> Result<OwnedFd, Error> {
    rustix::fs::openat2(
        volume,
        "inbox",
        OFlags::RDONLY
            .union(OFlags::DIRECTORY)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
        ResolveFlags::BENEATH
            .union(ResolveFlags::NO_SYMLINKS)
            .union(ResolveFlags::NO_MAGICLINKS)
            .union(ResolveFlags::NO_XDEV),
    )
    .map_err(failed("open the room inbox"))
}

/// What the inbox holds already.
#[derive(Debug, Default, PartialEq, Eq)]
struct Held {
    /// Its files: everything in it but where its numbers are kept.
    files: usize,
    /// Their bytes, its regular files' together.
    bytes: u64,
    /// The highest number it has given: kept, or a file's.
    highest: u32,
}

/// What `inbox` holds: every name in it counts, whatever it is.
fn held(inbox: &OwnedFd) -> Result<Held, Error> {
    const OPERATION: &str = "read the room inbox";
    let mut held = Held {
        highest: given(inbox)?,
        ..Held::default()
    };
    for entry in rustix::fs::Dir::read_from(inbox).map_err(failed(OPERATION))? {
        let entry = entry.map_err(failed(OPERATION))?;
        let name = entry.file_name().to_bytes();
        if [
            &b"."[..],
            b"..",
            NUMBERED.as_bytes(),
            NUMBERED_NEXT.as_bytes(),
        ]
        .contains(&name)
        {
            continue;
        }
        let stat = rustix::fs::statat(inbox, entry.file_name(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(failed(OPERATION))?;
        held.files += 1;
        if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile {
            held.bytes = held
                .bytes
                .saturating_add(u64::try_from(stat.st_size).unwrap_or(u64::MAX));
        }
        held.highest = held.highest.max(number_of(name).unwrap_or(0));
    }
    Ok(held)
}

/// The number a file's `name` starts with: four to [`DIGITS`] digits and
/// a hyphen.
fn number_of(name: &[u8]) -> Option<u32> {
    let digits = name.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if !(4..=DIGITS).contains(&digits) || name.get(digits) != Some(&b'-') {
        return None;
    }
    std::str::from_utf8(name.get(..digits)?).ok()?.parse().ok()
}

/// The last number `inbox` has given, as kept in it: none, before the
/// first.
fn given(inbox: &OwnedFd) -> Result<u32, Error> {
    const OPERATION: &str = "read the room inbox's numbers";
    let opened = rustix::fs::openat(
        inbox,
        NUMBERED,
        OFlags::RDONLY
            .union(OFlags::NOFOLLOW)
            .union(OFlags::NONBLOCK)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    );
    let file = match opened {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(0),
        Err(errno) => return Err(failed(OPERATION)(errno)),
    };
    let stat = rustix::fs::fstat(&file).map_err(failed(OPERATION))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(refused(
            OPERATION,
            "where its numbers are kept isn't a file",
        ));
    }
    let mut text = String::new();
    std::fs::File::from(file)
        .take(16)
        .read_to_string(&mut text)
        .map_err(|source| Error::Mount {
            operation: OPERATION,
            source,
        })?;
    text.trim_end()
        .parse()
        .map_err(|_| refused(OPERATION, "where its numbers are kept holds no number"))
}

/// Keeps `number` in `inbox` as the last it has given.
fn record(inbox: &OwnedFd, number: u32) -> Result<(), PutError> {
    let made = rustix::fs::openat(
        inbox,
        NUMBERED_NEXT,
        OFlags::WRONLY
            .union(OFlags::CREATE)
            .union(OFlags::TRUNC)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::from_raw_mode(0o644),
    )
    .map_err(failed("keep the room inbox's numbers"))?;
    let mut file = std::fs::File::from(made);
    file.write_all(format!("{number}\n").as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|source| written("keep the room inbox's numbers", source))?;
    drop(file);
    rustix::fs::renameat(inbox, NUMBERED_NEXT, inbox, NUMBERED)
        .map_err(|errno| written("keep the room inbox's numbers", errno.into()))
}

/// The number the next file of `size` bytes is given, when the inbox has
/// room for it.
fn next(held: &Held, size: usize) -> Result<u32, PutError> {
    let size = u64::try_from(size).unwrap_or(u64::MAX);
    if held.files >= FILES {
        return Err(PutError::Refused(NO_MORE_FILES));
    }
    if held.bytes.saturating_add(size) > BYTES {
        return Err(PutError::Refused(NO_MORE_BYTES));
    }
    if held.highest >= HIGHEST {
        return Err(PutError::Refused(NO_MORE_NUMBERS));
    }
    Ok(held.highest + 1)
}

/// Whether `error` is a disk with no space left: the Room's to fill.
fn is_full(error: &std::io::Error) -> bool {
    [
        Some(Errno::NOSPC.raw_os_error()),
        Some(Errno::DQUOT.raw_os_error()),
    ]
    .contains(&error.raw_os_error())
}

/// What a write that failed, doing `operation`, is: a disk that is full
/// is said as the owner is told.
fn written(operation: &'static str, source: std::io::Error) -> PutError {
    if is_full(&source) {
        PutError::Refused(DISK_FULL)
    } else {
        PutError::Failed(Error::Mount { operation, source })
    }
}

/// Writes `bytes` into `inbox` as a new file `name`: an unnamed file,
/// written whole, made read-only and synced, then linked in by its name,
/// which never replaces anything. A put that fails leaves nothing.
fn write_new(inbox: &OwnedFd, name: &str, bytes: &[u8]) -> Result<(), PutError> {
    let made = rustix::fs::openat(
        inbox,
        ".",
        OFlags::WRONLY.union(OFlags::TMPFILE).union(OFlags::CLOEXEC),
        Mode::from_raw_mode(0o600),
    )
    .map_err(failed("make a file in the room inbox"))?;
    let mut file = std::fs::File::from(made);
    file.write_all(bytes)
        .map_err(|source| written("write a file in the room inbox", source))?;
    rustix::fs::fchmod(&file, Mode::from_raw_mode(FILE_MODE))
        .map_err(failed("make a file in the room inbox read-only"))?;
    file.sync_all()
        .map_err(|source| written("sync a file in the room inbox", source))?;
    // An unnamed file is named through its descriptor: never over a name
    // that is there, whatever is there.
    let unnamed = format!("/proc/self/fd/{}", file.as_raw_fd());
    rustix::fs::linkat(CWD, unnamed.as_str(), inbox, name, AtFlags::SYMLINK_FOLLOW)
        .map_err(|errno| written("name a file in the room inbox", errno.into()))?;
    drop(file);
    if let Err(errno) = rustix::fs::fsync(inbox) {
        // Named and not known to be kept: it is taken out again, so a
        // put that is said to have failed left nothing.
        let _ = rustix::fs::unlinkat(inbox, name, AtFlags::empty());
        return Err(failed("sync the room inbox")(errno).into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::{Path, PathBuf};

    /// A volume of this test's own: a root with an inbox in it, as the
    /// room disk's is but this user's, not root's.
    struct Volume {
        root: PathBuf,
        pinned: OwnedFd,
    }

    impl Volume {
        fn new(test: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "hatchling-room-inbox-{test}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::create_dir(root.join("inbox")).unwrap();
            std::fs::set_permissions(root.join("inbox"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let pinned = rustix::fs::open(
                &root,
                OFlags::PATH.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC),
                Mode::empty(),
            )
            .unwrap();
            Self { root, pinned }
        }

        fn inbox(&self) -> PathBuf {
            self.root.join("inbox")
        }

        fn put(&self, stem: &str, bytes: &[u8]) -> Result<String, PutError> {
            put_as(self.pinned.as_fd(), stem, bytes, crate::euid())
        }

        /// The inbox's files: what it holds but where its numbers are kept.
        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.inbox())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .filter(|name| name != NUMBERED)
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Volume {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn said(error: &impl std::fmt::Display) -> String {
        error.to_string()
    }

    /// What the inbox refused, as the owner is told it.
    fn refusal(put: Result<String, PutError>) -> &'static str {
        match put {
            Err(PutError::Refused(why)) => why,
            other => unreachable!("not refused: {other:?}"),
        }
    }

    /// Each refusal says the bound it holds to.
    #[test]
    fn a_refusal_says_its_bound() {
        let mib = |bytes: u64| format!("{} MiB", bytes / (1024 * 1024));
        assert!(TOO_LARGE.ends_with(&mib(u64::try_from(FILE_BYTES).unwrap())));
        let files = format!("{FILES} files");
        assert!(NO_MORE_FILES.ends_with(&files));
        assert!(NO_MORE_BYTES.ends_with(&mib(BYTES)));
        assert_eq!(HIGHEST.to_string().len(), DIGITS);
    }

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().mode() & 0o7777
    }

    #[test]
    fn a_stem_is_short_plain_ascii_and_never_hidden() {
        for stem in [
            "notes.txt",
            "a",
            "A-b_c.9",
            "report.tar.gz",
            "-dash",
            "x.",
            "a..b",
        ] {
            assert!(is_stem(stem), "{stem:?} was refused");
        }
        let longest = "x".repeat(STEM_BYTES);
        assert!(is_stem(&longest));
        for stem in [
            "",
            ".",
            "..",
            ".hidden",
            "a/b",
            "../up",
            "/abs",
            "a\\b",
            "nul\0",
            "sp ace",
            "tab\t",
            "new\nline",
            "colon:",
        ] {
            assert!(!is_stem(stem), "{stem:?} was taken");
        }
        let too_long = format!("{longest}x");
        assert!(!is_stem(&too_long));
        // Letters past ASCII, and a mark that turns text around.
        for other in ['\u{e9}', '\u{202e}'] {
            let stem = format!("caf{other}txt");
            assert!(!is_stem(&stem), "{stem:?} was taken");
        }
    }

    #[test]
    fn a_number_is_four_to_nine_digits_and_a_hyphen() {
        assert_eq!(number_of(b"0001-a"), Some(1));
        assert_eq!(number_of(b"9999-"), Some(9999));
        assert_eq!(number_of(b"0420-x"), Some(420));
        assert_eq!(number_of(b"12345-x"), Some(12345));
        assert_eq!(number_of(b"999999999-x"), Some(HIGHEST));
        for name in [
            &b"001-a"[..],
            b"0001a",
            b"00a1-a",
            b"0001",
            b"",
            b"-001-a",
            b"+001-a",
            b"1234567890-a",
        ] {
            assert_eq!(number_of(name), None, "{name:?}");
        }
    }

    #[test]
    fn the_next_number_is_past_the_highest_when_there_is_room() {
        let some = Held {
            files: 3,
            bytes: 10,
            highest: 7,
        };
        assert_eq!(next(&some, 1).unwrap(), 8);
        assert_eq!(next(&Held::default(), 0).unwrap(), 1);
        let room = BYTES - 10;
        assert_eq!(next(&some, usize::try_from(room).unwrap()).unwrap(), 8);
        let over = next(&some, usize::try_from(room + 1).unwrap()).map(|n| n.to_string());
        assert_eq!(refusal(over), NO_MORE_BYTES);
        let most = Held {
            files: FILES - 1,
            ..Held::default()
        };
        assert_eq!(next(&most, 0).unwrap(), 1);
        let full = Held {
            files: FILES,
            ..Held::default()
        };
        assert_eq!(
            refusal(next(&full, 0).map(|n| n.to_string())),
            NO_MORE_FILES
        );
        let last = Held {
            highest: HIGHEST - 1,
            ..Held::default()
        };
        assert_eq!(next(&last, 0).unwrap(), HIGHEST);
        let used = Held {
            highest: HIGHEST,
            ..Held::default()
        };
        assert_eq!(
            refusal(next(&used, 0).map(|n| n.to_string())),
            NO_MORE_NUMBERS
        );
    }

    #[test]
    fn a_full_disk_is_said_plainly() {
        let full = std::io::Error::from_raw_os_error(Errno::NOSPC.raw_os_error());
        let quota = std::io::Error::from_raw_os_error(Errno::DQUOT.raw_os_error());
        let broken = std::io::Error::from_raw_os_error(Errno::IO.raw_os_error());
        assert!(is_full(&full) && is_full(&quota) && !is_full(&broken));
        assert!(!is_full(&std::io::Error::other("no code")));
        assert_eq!(refusal(Err(written("write", full))), DISK_FULL);
        let failed = written("write", broken);
        assert!(matches!(failed, PutError::Failed(_)), "{failed:?}");
        assert!(said(&failed).contains("write"), "{failed}");
    }

    #[test]
    fn a_file_is_put_whole_numbered_and_read_only() {
        let volume = Volume::new("put");
        assert_eq!(
            volume.put("notes.txt", b"from the owner").unwrap(),
            "0001-notes.txt"
        );
        assert_eq!(volume.put("notes.txt", b"again").unwrap(), "0002-notes.txt");
        assert_eq!(volume.put("empty", b"").unwrap(), "0003-empty");
        let first = volume.inbox().join("0001-notes.txt");
        assert_eq!(std::fs::read(&first).unwrap(), b"from the owner");
        assert_eq!(
            std::fs::read(volume.inbox().join("0002-notes.txt")).unwrap(),
            b"again"
        );
        assert_eq!(mode(&first), 0o444, "read-only to everyone");
        assert_eq!(std::fs::metadata(&first).unwrap().uid(), crate::euid());
        assert_eq!(
            volume.names(),
            ["0001-notes.txt", "0002-notes.txt", "0003-empty"],
            "nothing else is left in the inbox"
        );
        // A number is never given twice: not after its file is taken
        // out, nor after the highest is.
        std::fs::remove_file(volume.inbox().join("0002-notes.txt")).unwrap();
        assert_eq!(volume.put("next", b"n").unwrap(), "0004-next");
        std::fs::remove_file(volume.inbox().join("0004-next")).unwrap();
        std::fs::remove_file(volume.inbox().join("0003-empty")).unwrap();
        assert_eq!(volume.put("next", b"m").unwrap(), "0005-next");
        let large = vec![b'x'; FILE_BYTES];
        assert_eq!(volume.put("large", &large).unwrap(), "0006-large");
        assert_eq!(
            std::fs::read(volume.inbox().join("0006-large")).unwrap(),
            large
        );
        assert_eq!(
            std::fs::read_to_string(volume.inbox().join(NUMBERED)).unwrap(),
            "6\n"
        );
    }

    #[test]
    fn a_stem_or_a_size_the_inbox_doesnt_take_writes_nothing() {
        let volume = Volume::new("refused");
        for stem in ["../escape", ".hidden", "a/b", ""] {
            assert_eq!(refusal(volume.put(stem, b"x")), NOT_A_STEM);
        }
        let large = vec![b'x'; FILE_BYTES + 1];
        assert_eq!(refusal(volume.put("large", &large)), TOO_LARGE);
        assert!(volume.names().is_empty(), "{:?}", volume.names());
        assert!(
            !volume.inbox().join(NUMBERED).exists(),
            "a number was spent"
        );
        assert!(!volume.root.join("escape").exists());
    }

    #[test]
    fn a_full_inbox_takes_no_more() {
        let volume = Volume::new("full");
        for at in 0..FILES {
            std::fs::write(volume.inbox().join(format!("other-{at}")), b"").unwrap();
        }
        assert_eq!(refusal(volume.put("one-more", b"x")), NO_MORE_FILES);
        std::fs::remove_file(volume.inbox().join("other-0")).unwrap();
        assert_eq!(volume.put("one-more", b"x").unwrap(), "0001-one-more");

        let volume = Volume::new("bytes");
        let half = vec![b'x'; usize::try_from(BYTES / 2).unwrap()];
        std::fs::write(volume.inbox().join("0001-a"), &half).unwrap();
        std::fs::write(volume.inbox().join("0002-b"), half.get(1..).unwrap()).unwrap();
        assert_eq!(volume.put("fits", b"x").unwrap(), "0003-fits");
        assert_eq!(refusal(volume.put("over", b"x")), NO_MORE_BYTES);
    }

    #[test]
    fn what_the_inbox_holds_is_counted_whatever_it_is() {
        let volume = Volume::new("held");
        std::fs::write(volume.inbox().join("0007-a"), b"abc").unwrap();
        std::fs::create_dir(volume.inbox().join("0009-dir")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", volume.inbox().join("0002-link")).unwrap();
        std::fs::write(volume.inbox().join("plain"), b"de").unwrap();
        std::fs::write(volume.inbox().join(NUMBERED_NEXT), b"99\n").unwrap();
        let inbox = open_inbox(volume.pinned.as_fd()).unwrap();
        let counted = Held {
            files: 4,
            bytes: 5,
            highest: 9,
        };
        assert_eq!(held(&inbox).unwrap(), counted);
        // The number kept counts when it is the highest, and only then.
        std::fs::write(volume.inbox().join(NUMBERED), b"8\n").unwrap();
        assert_eq!(held(&inbox).unwrap(), counted);
        std::fs::write(volume.inbox().join(NUMBERED), b"12\n").unwrap();
        assert_eq!(held(&inbox).unwrap().highest, 12);
    }

    #[test]
    fn where_the_numbers_are_kept_must_be_a_file_holding_one() {
        let volume = Volume::new("numbered");
        let inbox = open_inbox(volume.pinned.as_fd()).unwrap();
        assert_eq!(given(&inbox).unwrap(), 0);
        std::fs::write(volume.inbox().join(NUMBERED), b"41\n").unwrap();
        assert_eq!(given(&inbox).unwrap(), 41);
        std::fs::write(volume.inbox().join(NUMBERED), b"forty").unwrap();
        let refused = given(&inbox).unwrap_err();
        assert!(said(&refused).contains("holds no number"), "{refused}");
        std::fs::write(volume.inbox().join(NUMBERED), "7".repeat(40)).unwrap();
        assert!(given(&inbox).is_err(), "read past what a number takes");
        std::fs::remove_file(volume.inbox().join(NUMBERED)).unwrap();
        std::fs::create_dir(volume.inbox().join(NUMBERED)).unwrap();
        let refused = given(&inbox).unwrap_err();
        assert!(said(&refused).contains("isn't a file"), "{refused}");
        std::fs::remove_dir(volume.inbox().join(NUMBERED)).unwrap();
        let outside = volume.root.join("outside");
        std::fs::write(&outside, b"30\n").unwrap();
        std::os::unix::fs::symlink(&outside, volume.inbox().join(NUMBERED)).unwrap();
        assert!(given(&inbox).is_err(), "read through a link");
        // A put with nowhere to keep its number puts nothing.
        assert!(volume.put("a", b"x").is_err());
        assert!(volume.names().is_empty(), "{:?}", volume.names());
        assert_eq!(std::fs::read(&outside).unwrap(), b"30\n");
    }

    #[test]
    fn a_put_never_follows_a_link_and_never_replaces_a_file() {
        let volume = Volume::new("links");
        let outside = volume.root.join("outside");
        std::fs::write(&outside, b"kept").unwrap();
        // A link where the next number is written isn't written through:
        // the put fails, and spends nothing.
        std::os::unix::fs::symlink(&outside, volume.inbox().join(NUMBERED_NEXT)).unwrap();
        assert!(volume.put("a", b"new").is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"kept");
        std::fs::remove_file(volume.inbox().join(NUMBERED_NEXT)).unwrap();
        assert_eq!(volume.put("a", b"new").unwrap(), "0001-a");
        assert_eq!(
            std::fs::read(volume.inbox().join("0001-a")).unwrap(),
            b"new"
        );
        // A name that is there, a file or a link, is never replaced: the
        // put fails, and leaves nothing of its own behind.
        let inbox = open_inbox(volume.pinned.as_fd()).unwrap();
        std::os::unix::fs::symlink(&outside, volume.inbox().join("0002-b")).unwrap();
        for name in ["0001-a", "0002-b"] {
            let refused = write_new(&inbox, name, b"other").unwrap_err();
            assert!(said(&refused).contains("name a file"), "{refused}");
        }
        assert_eq!(
            std::fs::read(volume.inbox().join("0001-a")).unwrap(),
            b"new"
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"kept");
        assert_eq!(volume.names(), ["0001-a", "0002-b"]);
        // An inbox that is a link is never opened.
        std::fs::remove_dir_all(volume.inbox()).unwrap();
        std::fs::create_dir(volume.root.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink("elsewhere", volume.inbox()).unwrap();
        let refused = volume.put("a", b"x").unwrap_err();
        assert!(said(&refused).contains("open the room inbox"), "{refused}");
        assert_eq!(
            std::fs::read_dir(volume.root.join("elsewhere"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn a_put_needs_the_volume_and_the_inbox_kept_from_others() {
        let volume = Volume::new("control");
        for (path, open, which) in [
            (volume.inbox(), 0o775, "inbox ownership"),
            (volume.root.clone(), 0o757, "volume root ownership"),
        ] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(open)).unwrap();
            let refused = said(&volume.put("a", b"x").unwrap_err());
            assert!(refused.contains(which), "{refused}");
            assert!(refused.contains("controlled by root"), "{refused}");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(volume.names().is_empty());
        // Held to root, as the spawner's are, this user's volume isn't one.
        let refused = put(volume.pinned.as_fd(), "a", b"x").unwrap_err();
        if crate::euid() != 0 {
            assert!(said(&refused).contains("controlled by root"), "{refused}");
        }
    }

    #[test]
    fn the_room_is_let_read_its_inbox_and_only_that() {
        let volume = Volume::new("read");
        std::fs::set_permissions(volume.inbox(), std::fs::Permissions::from_mode(0o700)).unwrap();
        readable_inbox(volume.pinned.as_fd(), crate::euid()).unwrap();
        assert_eq!(mode(&volume.inbox()), 0o755);
        // One that is already readable is left as it is: its mode isn't
        // set again, so nothing about it changes.
        let changed = |path: &Path| {
            let meta = std::fs::symlink_metadata(path).unwrap();
            (meta.ctime(), meta.ctime_nsec())
        };
        let before = changed(&volume.inbox());
        std::thread::sleep(std::time::Duration::from_millis(20));
        readable_inbox(volume.pinned.as_fd(), crate::euid()).unwrap();
        assert_eq!(mode(&volume.inbox()), 0o755);
        assert_eq!(
            changed(&volume.inbox()),
            before,
            "a readable inbox was changed"
        );
        // A put makes it readable too: the Room is never told of a file
        // it can't read.
        std::fs::set_permissions(volume.inbox(), std::fs::Permissions::from_mode(0o700)).unwrap();
        volume.put("a", b"x").unwrap();
        assert_eq!(mode(&volume.inbox()), 0o755);
        // An inbox others may write isn't made readable: it is refused.
        std::fs::set_permissions(volume.inbox(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let refused = readable_inbox(volume.pinned.as_fd(), crate::euid()).unwrap_err();
        assert!(said(&refused).contains("controlled by root"), "{refused}");
        assert_eq!(mode(&volume.inbox()), 0o777);
        let refused = let_the_room_read(volume.pinned.as_fd());
        if crate::euid() != 0 {
            assert!(said(&refused.unwrap_err()).contains("controlled by root"));
        }
    }

    #[test]
    fn the_inbox_knows_its_own_refusals_word_for_word() {
        for said in [
            NOT_A_STEM,
            TOO_LARGE,
            NO_MORE_FILES,
            NO_MORE_BYTES,
            NO_MORE_NUMBERS,
        ] {
            assert!(is_refusal(said), "{said}");
        }
        for said in [DISK_FULL, NOT_A_NAME, NO_SUCH_FILE] {
            assert!(is_refusal(said), "{said}");
        }
        for said in [
            "",
            "open the room inbox: Permission denied",
            "the Room's inbox is full: it holds 64 files and more",
        ] {
            assert!(!is_refusal(said), "{said}");
        }
    }

    #[test]
    fn a_name_is_a_number_a_hyphen_and_a_stem() {
        for name in [
            "0001-a",
            "0001-report.pdf",
            "123456789-a_b-c.txt",
            "0042-0042-x",
        ] {
            assert!(is_name(name), "{name}");
        }
        for name in [
            "",
            "0001",
            "0001-",
            "001-a",
            "1234567890-a",
            "0001-.a",
            "0001-a/b",
            "a-0001",
            "-0001-a",
            "0001_a",
            ".numbered",
            "0001-a\0",
            "0001-é",
        ] {
            assert!(!is_name(name), "{name:?}");
        }
    }

    #[test]
    fn the_inbox_lists_its_files_by_number_and_nothing_else() {
        let volume = Volume::new("list");
        assert_eq!(list_as(volume.pinned.as_fd(), crate::euid()).unwrap(), []);
        for stem in ["b", "a", "c"] {
            volume.put(stem, stem.repeat(3).as_bytes()).unwrap();
        }
        // Ten numbers on: listed by number, not by name.
        std::fs::write(volume.inbox().join("0010-z"), b"zz").unwrap();
        // What a put never names isn't a file of the inbox's.
        std::fs::write(volume.inbox().join("stray"), b"s").unwrap();
        std::fs::create_dir(volume.inbox().join("0011-dir")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", volume.inbox().join("0012-link")).unwrap();
        let listed = list_as(volume.pinned.as_fd(), crate::euid()).unwrap();
        let shown: Vec<(&str, u64)> = listed
            .iter()
            .map(|file| (file.name.as_str(), file.bytes))
            .collect();
        assert_eq!(
            shown,
            [("0001-b", 3), ("0002-a", 3), ("0003-c", 3), ("0010-z", 2)]
        );
    }

    #[test]
    fn a_file_is_removed_by_its_name_and_its_number_is_never_given_again() {
        let volume = Volume::new("remove");
        let first = volume.put("a", b"1").unwrap();
        let second = volume.put("b", b"2").unwrap();
        remove_as(volume.pinned.as_fd(), &second, crate::euid()).unwrap();
        assert_eq!(volume.names(), std::slice::from_ref(&first));
        assert_eq!(volume.put("c", b"3").unwrap(), "0003-c");
        // Gone already, and a name no put gives: refused, and the inbox
        // is as it was.
        for (name, why) in [
            (second.as_str(), NO_SUCH_FILE),
            ("0001-a/../0001-a", NOT_A_NAME),
            ("../inbox/0001-a", NOT_A_NAME),
            (NUMBERED, NOT_A_NAME),
            ("0001", NOT_A_NAME),
        ] {
            match remove_as(volume.pinned.as_fd(), name, crate::euid()) {
                Err(PutError::Refused(said)) => assert_eq!(said, why, "{name}"),
                other => unreachable!("{name}: not refused: {other:?}"),
            }
        }
        assert_eq!(volume.names(), [first, "0003-c".to_owned()]);
        assert!(volume.inbox().join(NUMBERED).exists());
    }

    #[test]
    fn a_list_or_a_removal_needs_the_inbox_kept_from_others() {
        let volume = Volume::new("others");
        let name = volume.put("a", b"1").unwrap();
        std::fs::set_permissions(volume.inbox(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let listed = list_as(volume.pinned.as_fd(), crate::euid()).unwrap_err();
        assert!(
            said(&listed).contains("verify room inbox ownership"),
            "{listed}"
        );
        match remove_as(volume.pinned.as_fd(), &name, crate::euid()) {
            Err(PutError::Failed(error)) => {
                assert!(
                    said(&error).contains("verify room inbox ownership"),
                    "{error}"
                );
            }
            other => unreachable!("not refused: {other:?}"),
        }
        std::fs::set_permissions(volume.inbox(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(volume.names(), [name]);
    }
}
