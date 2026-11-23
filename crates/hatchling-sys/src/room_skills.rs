//! The Room's skills (WO-046): a copy of each skill the owner enabled,
//! put on the room disk by root, for the agent to read and run in its
//! computer.
//!
//! The skills are a directory of their own at the encrypted volume's
//! root, beside the inbox. The Room sees it read-only, `noexec`, at
//! `/workspace/skills`: a helper runs there through an interpreter
//! (`python3 /workspace/skills/<name>/outline.py`), never as a program of
//! its own. Each skill is a directory named for it, holding the files its
//! signed manifest lists, as the broker verified them, and `.digest`, the
//! digest the owner enabled.
//!
//! A skill is written whole under a name no skill has (`.new-<name>`),
//! then given its name: the Room never sees half of one. Everything is
//! reached through the volume's pinned root: never by a path from it,
//! never through a link, never across a mount. Names and paths are held
//! to a grammar this checks, whatever the manifest allowed.

use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags, ResolveFlags};
use rustix::io::Errno;

use crate::Error;
use crate::room_mounts::{failed, refused, require_control};

/// The most skills the Room holds.
pub const SKILLS: usize = 64;

/// The most files one skill holds in the Room, its `.digest` aside.
pub const SKILL_FILES: usize = 64;

/// The most bytes one skill holds in the Room, its files together.
pub const SKILL_BYTES: usize = 1024 * 1024;

/// The longest name of a skill, or of a step of a path in one.
const NAME_BYTES: usize = 128;

/// The most steps a path in a skill has.
const DEPTH: usize = 8;

/// Where a skill's digest is kept, in its directory.
pub const DIGEST: &str = ".digest";

/// What a skill being written is named, before its own name.
const NEW: &str = ".new-";

/// What a skill being taken out is named, before it is gone.
const OLD: &str = ".old-";

/// The skills directory's, and each skill's directories', mode: root's,
/// and every user may read them, the Room's among them.
const MODE: u32 = 0o755;

/// A file of a skill's: read-only to everyone, root included but for
/// root's override.
const FILE_MODE: u32 = 0o444;

/// Why a skill wasn't put in the Room.
#[derive(Debug, thiserror::Error)]
pub enum PutError {
    /// The Room doesn't take it: its name, a path, its size or count.
    #[error("{0}")]
    Refused(&'static str),
    /// The volume or the skills directory wasn't as it must be, or a
    /// write failed.
    #[error(transparent)]
    Failed(#[from] Error),
}

/// A file of a skill's, as it is put in the Room.
#[derive(Debug, Clone, Copy)]
pub struct SkillFile<'a> {
    /// Its path in the skill.
    pub path: &'a str,
    /// Its bytes, as the broker verified them.
    pub bytes: &'a [u8],
}

/// A skill the Room holds: its name, and the digest it was put at; empty
/// when it has none this can read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldSkill {
    /// Its name.
    pub name: String,
    /// Its digest, lowercase hex; empty when unreadable.
    pub digest: String,
}

/// Whether `name` is one a skill, or a step of a path in one, can have in
/// the Room: letters, digits, `.`, `_` and `-`, not starting with `.`, at
/// most 128 bytes.
#[must_use]
pub fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= NAME_BYTES
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether `path` is one a file of a skill's can have in the Room: steps
/// each a name ([`is_name`]), at most eight of them, joined by `/`.
#[must_use]
pub fn is_path(path: &str) -> bool {
    let steps: Vec<&str> = path.split('/').collect();
    steps.len() <= DEPTH && steps.iter().all(|step| is_name(step))
}

/// Whether `digest` is lowercase hex SHA-256.
fn is_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Makes the skills directory of the volume whose root is `volume`, if it
/// has none, and the Room's to read: root's, mode `0755`.
///
/// # Errors
///
/// Refuses a volume root or a skills directory that isn't root's alone,
/// one reached by a link or across a mount; and a failed change.
pub fn prepare(volume: BorrowedFd<'_>) -> Result<(), Error> {
    skills_dir(volume, 0).map(drop)
}

/// The skills directory, made if absent, opened, and the Room's to read:
/// the volume's root and it held to being `trusted`'s alone.
fn skills_dir(volume: BorrowedFd<'_>, trusted: u32) -> Result<OwnedFd, Error> {
    const OPERATION: &str = "let the room read its skills";
    require_control(volume, trusted, "verify room volume root ownership")?;
    match rustix::fs::mkdirat(volume, "skills", Mode::from_raw_mode(0o700)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(errno) => return Err(failed("make the room skills directory")(errno)),
    }
    let skills = open_beneath(volume, "skills", OFlags::RDONLY.union(OFlags::DIRECTORY))
        .map_err(failed("open the room skills directory"))?;
    require_control(skills.as_fd(), trusted, "verify room skills ownership")?;
    rustix::fs::fchmod(&skills, Mode::from_raw_mode(MODE)).map_err(failed(OPERATION))?;
    Ok(skills)
}

/// `name` under `parent`, opened with `flags`: never through a link,
/// never across a mount, never out of `parent`.
fn open_beneath(parent: BorrowedFd<'_>, name: &str, flags: OFlags) -> Result<OwnedFd, Errno> {
    rustix::fs::openat2(
        parent,
        name,
        flags.union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC),
        Mode::empty(),
        ResolveFlags::BENEATH
            .union(ResolveFlags::NO_SYMLINKS)
            .union(ResolveFlags::NO_MAGICLINKS)
            .union(ResolveFlags::NO_XDEV),
    )
}

/// The skills the Room holds on the volume whose root is `volume`, by name.
///
/// Each directory named as a skill is ([`is_name`]) is one, with its
/// digest. What is left of a put or a removal that didn't finish is
/// taken out first.
///
/// The caller holds the room disk's turn.
///
/// # Errors
///
/// A volume or a skills directory that isn't root's, or one that can't
/// be read.
pub fn held(volume: BorrowedFd<'_>) -> Result<Vec<HeldSkill>, Error> {
    held_as(volume, 0)
}

/// [`held`], held to the volume and the skills directory being
/// `trusted`'s.
fn held_as(volume: BorrowedFd<'_>, trusted: u32) -> Result<Vec<HeldSkill>, Error> {
    const OPERATION: &str = "list the room skills";
    let skills = skills_dir(volume, trusted)?;
    tidy(&skills)?;
    let mut held = Vec::new();
    for name in names(&skills, OPERATION)? {
        if !is_name(&name) {
            continue;
        }
        let stat = rustix::fs::statat(&skills, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(failed(OPERATION))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            continue;
        }
        let digest = digest_of(&skills, &name).unwrap_or_default();
        held.push(HeldSkill { name, digest });
    }
    held.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(held)
}

/// The entries of `directory`, but `.` and `..`, by name: a name that
/// isn't UTF-8 among them, lossily.
fn names(directory: &OwnedFd, operation: &'static str) -> Result<Vec<String>, Error> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(directory).map_err(failed(operation))? {
        let entry = entry.map_err(failed(operation))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != "." && name != ".." {
            names.push(name);
        }
    }
    Ok(names)
}

/// The digest a skill directory `name` keeps, when it is one.
fn digest_of(skills: &OwnedFd, name: &str) -> Option<String> {
    let skill = open_beneath(
        skills.as_fd(),
        name,
        OFlags::RDONLY.union(OFlags::DIRECTORY),
    )
    .ok()?;
    let file = open_beneath(skill.as_fd(), DIGEST, OFlags::RDONLY).ok()?;
    let mut text = String::new();
    std::fs::File::from(file)
        .take(65)
        .read_to_string(&mut text)
        .ok()?;
    is_digest(&text).then_some(text)
}

/// Takes out what a put or a removal that didn't finish left: every
/// `.new-` and `.old-` entry.
fn tidy(skills: &OwnedFd) -> Result<(), Error> {
    for name in names(skills, "tidy the room skills")? {
        if name.starts_with(NEW) || name.starts_with(OLD) {
            remove_tree(skills, &name, 0)?;
        }
    }
    Ok(())
}

/// Takes `name` out of `parent`: a file, or a directory and all it holds,
/// never through a link. Deeper than a skill can be is refused.
fn remove_tree(parent: &OwnedFd, name: &str, depth: usize) -> Result<(), Error> {
    const OPERATION: &str = "take a skill out of the room";
    let stat = match rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(()),
        Err(errno) => return Err(failed(OPERATION)(errno)),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return match rustix::fs::unlinkat(parent, name, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => Ok(()),
            Err(errno) => Err(failed(OPERATION)(errno)),
        };
    }
    if depth > DEPTH {
        return Err(refused(
            OPERATION,
            "a skill directory is deeper than a skill can be",
        ));
    }
    let directory = open_beneath(
        parent.as_fd(),
        name,
        OFlags::RDONLY.union(OFlags::DIRECTORY),
    )
    .map_err(failed(OPERATION))?;
    for entry in names(&directory, OPERATION)? {
        remove_tree(&directory, &entry, depth.saturating_add(1))?;
    }
    drop(directory);
    match rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR) {
        Ok(()) | Err(Errno::NOENT) => Ok(()),
        Err(errno) => Err(failed(OPERATION)(errno)),
    }
}

/// Puts the skill `name`, at `digest`, holding `files`, in the Room on
/// the volume whose root is `volume`, in place of any it held by that
/// name.
///
/// It is written whole under `.new-<name>`, read-only, synced, then
/// named: the Room sees the skill as it was or as it is, never between.
///
/// The caller holds the room disk's turn, and `volume` is the root of its
/// authenticated, mounted volume.
///
/// # Errors
///
/// [`PutError::Refused`]: a name, a digest or a path that isn't one, a
/// path given twice, more than [`SKILL_FILES`] files or
/// [`SKILL_BYTES`] bytes, or a Room already holding [`SKILLS`] others.
/// [`PutError::Failed`]: a volume or a skills directory that isn't
/// root's, or a write that failed. Either way, the skill the Room held by
/// that name is as it was, or gone.
pub fn put(
    volume: BorrowedFd<'_>,
    name: &str,
    digest: &str,
    files: &[SkillFile<'_>],
) -> Result<(), PutError> {
    put_as(volume, name, digest, files, 0)
}

/// [`put`], held to the volume and the skills directory being
/// `trusted`'s.
fn put_as(
    volume: BorrowedFd<'_>,
    name: &str,
    digest: &str,
    files: &[SkillFile<'_>],
    trusted: u32,
) -> Result<(), PutError> {
    if !is_name(name) {
        return Err(PutError::Refused(
            "that isn't a name a skill has in the Room",
        ));
    }
    if !is_digest(digest) {
        return Err(PutError::Refused("that isn't a skill's digest"));
    }
    if files.len() > SKILL_FILES {
        return Err(PutError::Refused(
            "a skill holds at most 64 files in the Room",
        ));
    }
    let bytes = files
        .iter()
        .map(|file| file.bytes.len())
        .fold(0usize, usize::saturating_add);
    if bytes > SKILL_BYTES {
        return Err(PutError::Refused("a skill holds at most 1 MiB in the Room"));
    }
    let mut paths: Vec<&str> = files.iter().map(|file| file.path).collect();
    if paths.iter().any(|path| !is_path(path) || *path == DIGEST) {
        return Err(PutError::Refused(
            "that isn't a path a file has in a skill in the Room",
        ));
    }
    paths.sort_unstable();
    paths.dedup();
    if paths.len() != files.len() {
        return Err(PutError::Refused("a skill names a file twice"));
    }
    let skills = skills_dir(volume, trusted)?;
    tidy(&skills)?;
    let others = held_as(volume, trusted)?
        .into_iter()
        .filter(|skill| skill.name != name)
        .count();
    if others >= SKILLS {
        return Err(PutError::Refused("the Room holds 64 skills already"));
    }
    let new = format!("{NEW}{name}");
    write_skill(&skills, &new, digest, files)?;
    let old = format!("{OLD}{name}");
    match rustix::fs::renameat(&skills, name, &skills, old.as_str()) {
        Ok(()) | Err(Errno::NOENT) => {}
        Err(errno) => return Err(failed("set aside a skill in the room")(errno).into()),
    }
    rustix::fs::renameat_with(&skills, new.as_str(), &skills, name, RenameFlags::NOREPLACE)
        .map_err(failed("name a skill in the room"))?;
    remove_tree(&skills, &old, 0)?;
    rustix::fs::fsync(&skills).map_err(failed("sync the room skills"))?;
    Ok(())
}

/// Writes `files` and `digest` into a new directory `new` under `skills`,
/// root's alone until it is whole, then the Room's to read.
fn write_skill(
    skills: &OwnedFd,
    new: &str,
    digest: &str,
    files: &[SkillFile<'_>],
) -> Result<(), PutError> {
    rustix::fs::mkdirat(skills, new, Mode::from_raw_mode(0o700))
        .map_err(failed("make a skill in the room"))?;
    let root = open_beneath(skills.as_fd(), new, OFlags::RDONLY.union(OFlags::DIRECTORY))
        .map_err(failed("open a skill in the room"))?;
    let mut directories = vec![root];
    for file in files {
        let mut steps: Vec<&str> = file.path.split('/').collect();
        let Some(leaf) = steps.pop() else {
            return Err(PutError::Refused(
                "that isn't a path a file has in a skill in the Room",
            ));
        };
        let mut at = open_beneath(skills.as_fd(), new, OFlags::RDONLY.union(OFlags::DIRECTORY))
            .map_err(failed("open a skill in the room"))?;
        for step in steps {
            match rustix::fs::mkdirat(&at, step, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(errno) => return Err(failed("make a directory in a skill")(errno).into()),
            }
            let next = open_beneath(at.as_fd(), step, OFlags::RDONLY.union(OFlags::DIRECTORY))
                .map_err(failed("open a directory in a skill"))?;
            directories
                .push(rustix::io::dup(&next).map_err(failed("hold a directory in a skill"))?);
            at = next;
        }
        write_file(&at, leaf, file.bytes)?;
    }
    let first = directories
        .first()
        .ok_or(PutError::Refused("a skill wasn't made"))?;
    write_file(first, DIGEST, digest.as_bytes())?;
    for directory in &directories {
        rustix::fs::fchmod(directory, Mode::from_raw_mode(MODE))
            .map_err(failed("let the room read a skill"))?;
        rustix::fs::fsync(directory).map_err(failed("sync a skill in the room"))?;
    }
    Ok(())
}

/// Writes `bytes` to a new file `name` in `directory`, read-only, synced.
fn write_file(directory: &OwnedFd, name: &str, bytes: &[u8]) -> Result<(), PutError> {
    let made = rustix::fs::openat(
        directory,
        name,
        OFlags::WRONLY
            .union(OFlags::CREATE)
            .union(OFlags::EXCL)
            .union(OFlags::NOFOLLOW)
            .union(OFlags::CLOEXEC),
        Mode::from_raw_mode(0o600),
    )
    .map_err(failed("make a file in a skill"))?;
    let mut file = std::fs::File::from(made);
    file.write_all(bytes)
        .map_err(|source| written("write a file in a skill", source))?;
    rustix::fs::fchmod(&file, Mode::from_raw_mode(FILE_MODE))
        .map_err(failed("make a file in a skill read-only"))?;
    file.sync_all()
        .map_err(|source| written("sync a file in a skill", source))
}

/// What a write that failed, doing `operation`, is: a disk that is full
/// is said as the owner is told.
fn written(operation: &'static str, source: std::io::Error) -> PutError {
    let full = [
        Some(Errno::NOSPC.raw_os_error()),
        Some(Errno::DQUOT.raw_os_error()),
    ]
    .contains(&source.raw_os_error());
    if full {
        PutError::Refused("the room disk is full")
    } else {
        PutError::Failed(Error::Mount { operation, source })
    }
}

/// Takes the skill `name` out of the Room on the volume whose root is
/// `volume`. Whether it held one.
///
/// The caller holds the room disk's turn, as for [`put`].
///
/// # Errors
///
/// [`PutError::Refused`]: a name a skill can't have ([`is_name`]).
/// [`PutError::Failed`]: a volume or a skills directory that isn't
/// root's, or a removal that failed.
pub fn remove(volume: BorrowedFd<'_>, name: &str) -> Result<bool, PutError> {
    remove_as(volume, name, 0)
}

/// [`remove`], held to the volume and the skills directory being
/// `trusted`'s.
fn remove_as(volume: BorrowedFd<'_>, name: &str, trusted: u32) -> Result<bool, PutError> {
    if !is_name(name) {
        return Err(PutError::Refused(
            "that isn't a name a skill has in the Room",
        ));
    }
    let skills = skills_dir(volume, trusted)?;
    let old = format!("{OLD}{name}");
    match rustix::fs::renameat(&skills, name, &skills, old.as_str()) {
        Ok(()) => {}
        Err(Errno::NOENT) => return Ok(false),
        Err(errno) => return Err(failed("set aside a skill in the room")(errno).into()),
    }
    remove_tree(&skills, &old, 0)?;
    rustix::fs::fsync(&skills).map_err(failed("sync the room skills"))?;
    Ok(true)
}

#[cfg(test)]
#[path = "room_skills/tests.rs"]
mod tests;
