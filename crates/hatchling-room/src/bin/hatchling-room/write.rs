//! Writing a file in the Room, and making an exact edit in one: done by
//! the supervisor itself, as the Room's user.
//!
//! Neither is a job. Nothing is started, nothing is kept, and each is
//! over before the supervisor answers. A file is never changed where it
//! lies: what it is to hold is written beside it, under a name of its
//! own, and put in its place in one step, so that nothing reading the
//! file sees half of it, and a write that fails leaves the file as it
//! was.
//!
//! Only a file is written over or edited. A directory, a link, or
//! anything else at the path is left alone and said: a link is never
//! followed to what it names.
//!
//! An edit is exact. The text to replace must be in the file as it was
//! given, at one place unless every place was asked for: text that isn't
//! there, or is there more than once, changes nothing, and the places it
//! is at are named.

use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use hatchling_core::room::{EDIT_FILE_BYTES, EDIT_PLACES, Edited, NoEdit, NoWrite, Wrote};
use rustix::fs::{CWD, OFlags, RenameFlags};

/// The mode a file made here has, before the umask takes from it.
const NEW_FILE_MODE: u32 = 0o644;

/// How many files this supervisor has written beside their places: part
/// of each one's name, so no two share one.
static WRITTEN: AtomicU64 = AtomicU64::new(0);

/// What is at a path.
enum There {
    /// Nothing.
    Nothing,
    /// A file, with this mode.
    File(u32),
    /// A directory, a link, or something else that isn't a file.
    Other,
}

/// What is at `path`, without following a link there.
fn there(path: &Path) -> io::Result<There> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_file() => Ok(There::File(meta.mode() & 0o7777)),
        Ok(_) => Ok(There::Other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(There::Nothing),
        Err(error) => Err(error),
    }
}

/// How a file is put in its place.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Put {
    /// Only where nothing is: a file that has appeared there stays.
    New,
    /// Over the file there, which is to have this mode again.
    Over(u32),
}

/// How many names beside a file are tried before a write is given up:
/// a name a job made first is never written over, nor removed.
const BESIDE_TRIES: u32 = 16;

/// Puts `bytes` at `path` in one step: written beside it under a name of
/// its own, then given the path. What was written beside it is removed
/// if it doesn't get there; a name something else made first is passed
/// over, and never removed (#573).
fn put(path: &Path, bytes: &[u8], how: Put) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let (beside, file) = beside(dir)?;
    let placed = filled(file, bytes, how).and_then(|()| match how {
        // It is given the name only if nothing has that name: a rename
        // that replaces nothing. There are no hard links in the Room.
        Put::New => rustix::fs::renameat_with(CWD, &beside, CWD, path, RenameFlags::NOREPLACE)
            .map_err(io::Error::from),
        Put::Over(_) => std::fs::rename(&beside, path),
    });
    if placed.is_err() {
        let _ = std::fs::remove_file(&beside);
    }
    placed
}

/// A new file in `dir`, made by this write alone, and its path: one
/// that was there first, whatever made it, is passed over. Made only
/// where nothing is (`O_EXCL`), it is never a link's target.
fn beside(dir: &Path) -> io::Result<(std::path::PathBuf, File)> {
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for _ in 0..BESIDE_TRIES {
        let count = WRITTEN.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(".hatchling-write-{}-{count}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(NEW_FILE_MODE)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}

/// Writes `bytes` to `file`, just made, with the mode `how` asks.
fn filled(mut file: File, bytes: &[u8], how: Put) -> io::Result<()> {
    file.write_all(bytes)?;
    if let Put::Over(mode) = how {
        file.set_permissions(Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Why a write didn't happen, from the error that stopped it.
fn no_write(error: &io::Error) -> NoWrite {
    use io::ErrorKind::{
        AlreadyExists, IsADirectory, NotADirectory, PermissionDenied, ReadOnlyFilesystem,
    };
    let kind = error.kind();
    if matches!(kind, PermissionDenied | ReadOnlyFilesystem) {
        NoWrite::Denied
    } else if matches!(kind, NotADirectory | IsADirectory) {
        NoWrite::NotAFile
    } else if kind == AlreadyExists {
        NoWrite::Exists
    } else {
        NoWrite::Failed
    }
}

/// Writes the file at `path` with `bytes`: a new file, with the
/// directories it is under made if they aren't there, or one in place of
/// the file there when `replace` is set, with that file's mode.
pub(crate) fn wrote(path: &Path, bytes: &[u8], replace: bool) -> Result<Wrote, NoWrite> {
    let how = match there(path).map_err(|error| no_write(&error))? {
        There::Other => return Err(NoWrite::NotAFile),
        There::File(_) if !replace => return Err(NoWrite::Exists),
        There::File(mode) => Put::Over(mode),
        There::Nothing => Put::New,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| no_write(&error))?;
    }
    put(path, bytes, how).map_err(|error| no_write(&error))?;
    Ok(Wrote {
        size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        replaced: how != Put::New,
    })
}

/// Why an edit didn't happen, from the error that stopped it.
fn no_edit(error: &io::Error) -> NoEdit {
    use io::ErrorKind::{NotADirectory, NotFound, PermissionDenied, ReadOnlyFilesystem};
    let kind = error.kind();
    if matches!(kind, NotFound | NotADirectory) {
        NoEdit::NoSuchFile
    } else if matches!(kind, PermissionDenied | ReadOnlyFilesystem) {
        NoEdit::Denied
    } else {
        NoEdit::Failed
    }
}

/// The whole of the file at `path`, as text: a file, no longer than
/// `most`, that is UTF-8 with no NUL in it.
///
/// It is opened without waiting and without following a link, and no
/// more than `most` and one byte of it is read.
fn text(path: &Path, most: u64) -> Result<String, NoEdit> {
    let flags = OFlags::NONBLOCK
        .union(OFlags::NOCTTY)
        .union(OFlags::NOFOLLOW);
    let file: File = OpenOptions::new()
        .read(true)
        .custom_flags(i32::try_from(flags.bits()).unwrap_or_default())
        .open(path)
        .map_err(|error| no_edit(&error))?;
    let meta = file.metadata().map_err(|error| no_edit(&error))?;
    if !meta.is_file() {
        return Err(NoEdit::NotAFile);
    }
    if meta.len() > most {
        return Err(NoEdit::TooBig);
    }
    let mut bytes = Vec::new();
    file.take(most.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| no_edit(&error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > most {
        return Err(NoEdit::TooBig);
    }
    if bytes.contains(&0) {
        return Err(NoEdit::NotText);
    }
    String::from_utf8(bytes).map_err(|_| NoEdit::NotText)
}

/// The line the byte at `offset` of `text` is on, counted from 1.
fn line_of(text: &str, offset: usize) -> u64 {
    let before = text
        .bytes()
        .take(offset)
        .filter(|byte| *byte == b'\n')
        .count();
    u64::try_from(before).unwrap_or(u64::MAX).saturating_add(1)
}

/// [`edited`], given its two texts as the bytes they came as. Bytes that
/// aren't text can't be in a file of text, and aren't put in one.
pub(crate) fn edited_bytes(
    path: &Path,
    old: &[u8],
    new: &[u8],
    all: bool,
) -> Result<Edited, NoEdit> {
    let Ok(new) = std::str::from_utf8(new) else {
        return Err(NoEdit::NotText);
    };
    let Ok(old) = std::str::from_utf8(old) else {
        return Err(NoEdit::NotFound);
    };
    edited(path, old, new, all)
}

/// Replaces `old` with `new` in the file at `path`: at the one place it
/// is, or at every place when `all` is set. Places don't overlap, and are
/// counted from the start of the file.
pub(crate) fn edited(path: &Path, old: &str, new: &str, all: bool) -> Result<Edited, NoEdit> {
    let mode = match there(path).map_err(|error| no_edit(&error))? {
        There::Nothing => return Err(NoEdit::NoSuchFile),
        There::Other => return Err(NoEdit::NotAFile),
        There::File(mode) => mode,
    };
    let text = text(path, EDIT_FILE_BYTES)?;
    // An empty text is everywhere and nowhere: it is no place.
    let places: Vec<usize> = if old.is_empty() {
        Vec::new()
    } else {
        text.match_indices(old).map(|(offset, _)| offset).collect()
    };
    let Some(first) = places.first().copied() else {
        return Err(NoEdit::NotFound);
    };
    if places.len() > 1 && !all {
        return Err(NoEdit::Ambiguous {
            places: u32::try_from(places.len()).unwrap_or(u32::MAX),
            lines: places
                .iter()
                .take(EDIT_PLACES)
                .map(|offset| line_of(&text, *offset))
                .collect(),
        });
    }
    let changed = text.replace(old, new);
    let size = u64::try_from(changed.len()).unwrap_or(u64::MAX);
    if size > EDIT_FILE_BYTES {
        return Err(NoEdit::TooBig);
    }
    put(path, changed.as_bytes(), Put::Over(mode)).map_err(|error| no_edit(&error))?;
    Ok(Edited {
        replaced: u32::try_from(places.len()).unwrap_or(u32::MAX),
        line: line_of(&text, first),
        size,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A directory of this test's own, empty.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-room-write-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn mode_of(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o7777
    }

    /// The names in `dir`, in order.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A name beside the file that something else made first is passed
    /// over, never written over nor removed: the write is made under a
    /// name of its own (#573).
    #[test]
    fn a_name_made_first_beside_the_file_is_passed_over_and_kept() {
        let dir = scratch("planted");
        let next = WRITTEN.load(Ordering::Relaxed);
        let planted: Vec<PathBuf> = (next..next + 8)
            .map(|count| dir.join(format!(".hatchling-write-{}-{count}", std::process::id())))
            .collect();
        for path in &planted {
            std::fs::write(path, "a job's").unwrap();
        }
        let path = dir.join("notes.txt");
        assert!(
            wrote(
                &path,
                b"the agent's
",
                false
            )
            .is_ok()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"the agent's
"
        );
        for path in &planted {
            assert_eq!(std::fs::read(path).unwrap(), b"a job's", "{path:?}");
        }
        // Every name passed over, the write is refused, and still nothing
        // of what was there is removed.
        let next = WRITTEN.load(Ordering::Relaxed);
        let planted: Vec<PathBuf> = (next..next + 4 * u64::from(BESIDE_TRIES))
            .map(|count| dir.join(format!(".hatchling-write-{}-{count}", std::process::id())))
            .collect();
        for path in &planted {
            std::fs::write(path, "a job's").unwrap();
        }
        assert!(wrote(&dir.join("other.txt"), b"x", false).is_err());
        assert!(!dir.join("other.txt").exists());
        for path in &planted {
            assert_eq!(std::fs::read(path).unwrap(), b"a job's", "{path:?}");
        }
    }

    /// A new file is written whole, with the directories it is under, as
    /// a file anyone may read and nobody may run; and nothing is left
    /// beside it.
    #[test]
    fn a_new_file_is_written_whole_with_its_directories() {
        let dir = scratch("new");
        let path = dir.join("a/b/notes.txt");
        let wrote = wrote(&path, b"TODO: buy milk\n", false).unwrap();
        assert_eq!(
            wrote,
            Wrote {
                size: 15,
                replaced: false
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"TODO: buy milk\n");
        assert_eq!(mode_of(&path) & 0o111, 0, "a new file can be run");
        assert_eq!(mode_of(&path) & 0o400, 0o400);
        assert_eq!(names(&dir.join("a/b")), ["notes.txt"]);
        // An empty file is a file, and so is one of bytes that aren't text.
        let empty = dir.join("empty");
        assert_eq!(wrote_at(&empty, b""), (0, false));
        assert_eq!(std::fs::read(&empty).unwrap(), b"");
        let bytes = dir.join("bytes");
        assert_eq!(wrote_at(&bytes, &[0, 0xff, b'\n']), (3, false));
        assert_eq!(std::fs::read(&bytes).unwrap(), [0, 0xff, b'\n']);
    }

    /// What a write of `bytes` to a path where nothing is to be replaced
    /// came to.
    fn wrote_at(path: &Path, bytes: &[u8]) -> (u64, bool) {
        let wrote = wrote(path, bytes, false).unwrap();
        (wrote.size, wrote.replaced)
    }

    /// A file that is there stays as it is unless it is to be replaced;
    /// replaced, it is the new file with the old one's mode.
    #[test]
    fn a_file_there_stays_unless_it_is_to_be_replaced() {
        let dir = scratch("there");
        let path = dir.join("run.sh");
        std::fs::write(&path, b"old\n").unwrap();
        std::fs::set_permissions(&path, Permissions::from_mode(0o750)).unwrap();
        assert_eq!(wrote(&path, b"new\n", false), Err(NoWrite::Exists));
        assert_eq!(std::fs::read(&path).unwrap(), b"old\n");
        assert_eq!(names(&dir), ["run.sh"]);
        let wrote = wrote(&path, b"newer\n", true).unwrap();
        assert_eq!(
            wrote,
            Wrote {
                size: 6,
                replaced: true
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"newer\n");
        assert_eq!(mode_of(&path), 0o750);
        assert_eq!(names(&dir), ["run.sh"]);
        // A file that appears where a new one was to go stays: the new
        // one is given the name only if nothing has it.
        let late = dir.join("late");
        std::fs::write(&late, b"first\n").unwrap();
        let error = put(&late, b"second\n", Put::New).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(no_write(&error), NoWrite::Exists);
        assert_eq!(std::fs::read(&late).unwrap(), b"first\n");
        assert_eq!(names(&dir), ["late", "run.sh"]);
    }

    /// Only a file is written over. A directory, a link, and a path under
    /// a file are each left alone, to be replaced or not; a link's target
    /// is as it was.
    #[test]
    fn only_a_file_is_written_over() {
        let dir = scratch("other");
        let target = dir.join("target");
        std::fs::write(&target, b"kept\n").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let folder = dir.join("folder");
        std::fs::create_dir(&folder).unwrap();
        for replace in [false, true] {
            assert_eq!(wrote(&folder, b"x", replace), Err(NoWrite::NotAFile));
            assert_eq!(wrote(&link, b"x", replace), Err(NoWrite::NotAFile));
            assert_eq!(
                wrote(&target.join("under"), b"x", replace),
                Err(NoWrite::NotAFile)
            );
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"kept\n");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(names(&dir), ["folder", "link", "target"]);
    }

    /// Each way a write is stopped is said as what it is.
    #[test]
    fn each_error_that_stops_a_write_or_an_edit_has_its_word() {
        use io::ErrorKind::{
            AlreadyExists, IsADirectory, NotADirectory, NotFound, PermissionDenied,
            ReadOnlyFilesystem, StorageFull,
        };
        for (kind, write, edit) in [
            (PermissionDenied, NoWrite::Denied, NoEdit::Denied),
            (ReadOnlyFilesystem, NoWrite::Denied, NoEdit::Denied),
            (NotADirectory, NoWrite::NotAFile, NoEdit::NoSuchFile),
            (IsADirectory, NoWrite::NotAFile, NoEdit::Failed),
            (AlreadyExists, NoWrite::Exists, NoEdit::Failed),
            (NotFound, NoWrite::Failed, NoEdit::NoSuchFile),
            (StorageFull, NoWrite::Failed, NoEdit::Failed),
        ] {
            let error = io::Error::from(kind);
            assert_eq!(no_write(&error), write, "{kind:?}");
            assert_eq!(no_edit(&error), edit, "{kind:?}");
        }
        // Where the Room's user may not write, nothing is written and
        // nothing is left. Root may write anywhere, and is not asked.
        if !rustix::process::geteuid().is_root() {
            let dir = scratch("denied");
            let closed = dir.join("closed");
            std::fs::create_dir(&closed).unwrap();
            std::fs::write(closed.join("there"), b"as it was\n").unwrap();
            std::fs::set_permissions(&closed, Permissions::from_mode(0o555)).unwrap();
            assert_eq!(
                wrote(&closed.join("new"), b"x", false),
                Err(NoWrite::Denied)
            );
            assert_eq!(
                wrote(&closed.join("there"), b"x", true),
                Err(NoWrite::Denied)
            );
            assert_eq!(
                edited(&closed.join("there"), "it", "so", false),
                Err(NoEdit::Denied)
            );
            assert_eq!(std::fs::read(closed.join("there")).unwrap(), b"as it was\n");
            assert_eq!(names(&closed), ["there"]);
            std::fs::set_permissions(&closed, Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// An edit replaces the one place its text is at, and says the line
    /// that place starts on and how long the file is now. The rest of the
    /// file, its line ends and its mode are as they were.
    #[test]
    fn an_edit_replaces_the_one_place_its_text_is_at() {
        let dir = scratch("edit");
        let path = dir.join("app.py");
        std::fs::write(
            &path,
            "import os\r\n\r\n# TODO: handle errors\r\nprint('caf\u{e9}')\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, Permissions::from_mode(0o640)).unwrap();
        let edited = edited(&path, "# TODO: handle errors", "try:\n    pass", false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "import os\r\n\r\ntry:\n    pass\r\nprint('caf\u{e9}')\n"
        );
        assert_eq!(
            edited,
            Edited {
                replaced: 1,
                line: 3,
                size: 43
            }
        );
        assert_eq!(mode_of(&path), 0o640);
        assert_eq!(names(&dir), ["app.py"]);
        // Text that runs over lines is one place, on the line it starts.
        let again = super::edited(&path, "pass\r\nprint", "raise\nprint", false).unwrap();
        assert_eq!((again.replaced, again.line), (1, 4));
        // Putting nothing in a text's place takes it out.
        let gone = super::edited(&path, "import os\r\n\r\n", "", false).unwrap();
        assert_eq!((gone.replaced, gone.line), (1, 1));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "try:\n    raise\nprint('caf\u{e9}')\n"
        );
    }

    /// Text that is at more than one place changes nothing unless every
    /// place was asked for, and the places are named: how many, and the
    /// lines the first of them start on.
    #[test]
    fn text_at_more_than_one_place_changes_nothing_and_names_them() {
        let dir = scratch("places");
        let path = dir.join("list");
        let before = "x = 1\ny = 2\nx = 1; x = 1\n";
        std::fs::write(&path, before).unwrap();
        assert_eq!(
            edited(&path, "x = 1", "x = 3", false),
            Err(NoEdit::Ambiguous {
                places: 3,
                lines: vec![1, 3, 3]
            })
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let every = edited(&path, "x = 1", "x = 3", true).unwrap();
        assert_eq!((every.replaced, every.line, every.size), (3, 1, 25));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "x = 3\ny = 2\nx = 3; x = 3\n"
        );
        // Every place asked for, where there is one, is that one.
        let one = edited(&path, "y = 2", "y = 4", true).unwrap();
        assert_eq!((one.replaced, one.line), (1, 2));
        // Places don't overlap: five of a letter hold two pairs of it.
        std::fs::write(&path, "aaaaa\n").unwrap();
        assert_eq!(
            edited(&path, "aa", "b", false),
            Err(NoEdit::Ambiguous {
                places: 2,
                lines: vec![1, 1]
            })
        );
        // No more places are named than may be, however many there are.
        let many = "z\n".repeat(EDIT_PLACES + 3);
        std::fs::write(&path, &many).unwrap();
        let Err(NoEdit::Ambiguous { places, lines }) = edited(&path, "z", "q", false) else {
            unreachable!("z is on every line");
        };
        assert_eq!(usize::try_from(places).unwrap(), EDIT_PLACES + 3);
        assert_eq!(lines, (1..=8).collect::<Vec<u64>>());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), many);
    }

    /// Text that isn't in the file as it was given changes nothing; and
    /// only a file of text is edited, one no longer than a file may be
    /// before the edit and after it.
    #[test]
    fn what_cannot_be_edited_is_left_as_it_was() {
        let dir = scratch("not");
        let path = dir.join("notes");
        std::fs::write(&path, "one\ntwo\n").unwrap();
        for old in ["three", "One", "one \n", "two\n\n", ""] {
            assert_eq!(
                edited(&path, old, "x", false),
                Err(NoEdit::NotFound),
                "{old:?}"
            );
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        assert_eq!(
            edited(&dir.join("absent"), "a", "b", false),
            Err(NoEdit::NoSuchFile)
        );
        assert_eq!(
            edited(&path.join("under"), "a", "b", false),
            Err(NoEdit::NoSuchFile)
        );
        assert_eq!(edited(&dir, "a", "b", false), Err(NoEdit::NotAFile));
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(edited(&link, "one", "1", false), Err(NoEdit::NotAFile));
        // Nor is a link opened, were one put there after it was looked at.
        assert_eq!(text(&link, 100), Err(NoEdit::Failed));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        let binary = dir.join("binary");
        std::fs::write(&binary, b"one\0two").unwrap();
        assert_eq!(edited(&binary, "one", "1", false), Err(NoEdit::NotText));
        std::fs::write(&binary, b"caf\xe9 one").unwrap();
        assert_eq!(edited(&binary, "one", "1", false), Err(NoEdit::NotText));
        assert_eq!(std::fs::read(&binary).unwrap(), b"caf\xe9 one");
        // Text to find that isn't text is nowhere in a file of text, and
        // what isn't text is never put in one; text is taken as it is.
        assert_eq!(
            edited_bytes(&path, b"\xff", b"x", false),
            Err(NoEdit::NotFound)
        );
        assert_eq!(
            edited_bytes(&path, b"one", b"\xff", false),
            Err(NoEdit::NotText)
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\ntwo\n");
        let taken = edited_bytes(&path, b"two", "deux \u{e9}".as_bytes(), false).unwrap();
        assert_eq!((taken.replaced, taken.line), (1, 2));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "one\ndeux \u{e9}\n"
        );
        std::fs::write(&path, "one\ntwo\n").unwrap();
        // A file as long as one may be is read; a byte more is too many.
        assert_eq!(text(&path, 8).unwrap(), "one\ntwo\n");
        assert_eq!(text(&path, 7), Err(NoEdit::TooBig));
        // An edit that would make the file longer than one may be isn't
        // made.
        let most = usize::try_from(EDIT_FILE_BYTES).unwrap();
        let full = dir.join("full");
        std::fs::write(&full, "y".repeat(most - 1) + "z").unwrap();
        assert_eq!(edited(&full, "z", "zz", false), Err(NoEdit::TooBig));
        assert_eq!(std::fs::metadata(&full).unwrap().len(), EDIT_FILE_BYTES);
        let same = edited(&full, "z", "w", false).unwrap();
        assert_eq!((same.replaced, same.size), (1, EDIT_FILE_BYTES));
        assert_eq!(names(&dir), ["binary", "full", "link", "notes"]);
    }

    #[test]
    fn a_line_is_counted_from_one_by_the_line_ends_before_it() {
        let text = "a\nbb\n\nc";
        for (offset, line) in [(0, 1), (1, 1), (2, 2), (4, 2), (5, 3), (6, 4), (99, 4)] {
            assert_eq!(line_of(text, offset), line, "{offset}");
        }
        assert_eq!(line_of("", 0), 1);
    }
}
