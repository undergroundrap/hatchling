//! Walking a folder in the Room for it to be handed to the owner
//! (WO-043): what it holds, by the Room's listings, before anything in it
//! is read.
//!
//! - **The Room's word, held to the bounds as it is found.** Folders and
//!   files are counted as each listing names them, so nothing the Room says
//!   is kept past the bounds: at most [`FOLDERS`] folders, the one walked
//!   among them, [`FOLDER_FILES`] files of at most [`FILE_BYTES`] each and
//!   [`FOLDER_BYTES`] in all, each path the Library's rules'
//!   ([`check_paths`]) and within [`PATH_BYTES`] whole.
//! - **What isn't a file or a folder is left out, and counted**: links,
//!   other mounts (the workspace's inbox among them, by its name too), and
//!   devices, pipes and sockets. Nothing left out is opened, and its name
//!   is held to no rule: it is never kept.
//! - **Names, held to the owner's computer.** A name that isn't UTF-8, or
//!   breaks a rule of the Library's, refuses the folder; so do two names in
//!   one folder that are one but for case, and so two paths. The words
//!   never name the path: it is the agent's.
//! - **A folder is the one its parent listed.** Each folder's listing is
//!   held to the inode its parent's said of it.
//! - **One deadline**, checked before each listing.
//!
//! What a listing says a file is, its length among it, is the Room's word:
//! the hand-over holds each read to it ([`crate::hand_over`]). A walk isn't
//! one snapshot: a file added to a folder already listed is missed.

use std::collections::{HashSet, VecDeque};
use std::time::Instant;

use hatchling_core::LeftOut;
use hatchling_core::room::{Kind, PATH_BYTES, WORKSPACE};

use crate::library::{FILE_BYTES, FOLDER_BYTES, FOLDER_FILES, check_paths};
use crate::room_ops::Rooms;

/// The most folders a walk lists, the one walked among them.
pub(crate) const FOLDERS: usize = 256;

/// Said of a name that isn't plain text.
pub(crate) const NOT_PLAIN: &str =
    "a name in the folder isn't plain ASCII: rename it, then give the folder";

/// Said of a folder with more folders than a walk lists.
pub(crate) const TOO_MANY_FOLDERS: &str =
    "the folder holds more than 256 folders, its own among them, the most one handed over may";

/// Said of a folder with more files than one handed over may hold.
pub(crate) const TOO_MANY_FILES: &str =
    "the folder holds more than 512 files, the most one handed over may";

/// Said of a folder holding a file larger than one handed over may be.
pub(crate) const FILE_TOO_LARGE: &str =
    "a file in the folder is larger than 8 MiB, the most a file handed over may be";

/// Said of a folder larger than one handed over may be.
pub(crate) const TOO_LARGE: &str =
    "the folder's files are larger than 32 MiB together, the most a folder handed over may be";

/// Said of a path whose whole, the folder's own path before it, is too
/// long for the Room to be asked for.
pub(crate) const PATH_TOO_LONG: &str =
    "a path in the folder, with the folder's own, is longer than 1024 bytes";

/// Said of two names in one folder that are one but for case.
pub(crate) const CASE_TWINS: &str = "two names in one folder are the same but for case";

/// Said of a folder that changed while it was walked or read.
pub(crate) const CHANGED: &str =
    "the folder changed while it was given: give it again once nothing is changing it";

/// Said of a folder with no file in it.
pub(crate) const EMPTY: &str = "the folder holds no file to give";

/// Said of a folder that took longer to give than one may.
pub(crate) const TOO_SLOW: &str = "the folder took longer than 5 minutes to give";

/// What a walk found to hand over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Walked {
    /// Each file, by its path in the folder and its listed length, in the
    /// walk's order.
    pub files: Vec<(String, u64)>,
    /// Their listed lengths together.
    pub bytes: u64,
    /// What was left out, by kind.
    pub left_out: LeftOut,
}

/// One folder waiting to be listed: its path in the folder walked, the
/// inode its parent listed it with (none for the one walked).
struct Waiting {
    path: String,
    ino: Option<u64>,
}

/// Walks the folder at the absolute `root` through `rooms`, until `until`.
///
/// # Errors
///
/// Why the folder can't be handed over, for the agent: never a path in it.
pub(crate) fn walk(rooms: &dyn Rooms, root: &str, until: Instant) -> Result<Walked, String> {
    let at_workspace = root == WORKSPACE;
    let mut waiting = VecDeque::from([Waiting {
        path: String::new(),
        ino: None,
    }]);
    let mut folders = 1_usize;
    let mut walked = Walked {
        files: Vec::new(),
        bytes: 0,
        left_out: LeftOut::default(),
    };
    while let Some(folder) = waiting.pop_front() {
        if Instant::now() >= until {
            return Err(String::from(TOO_SLOW));
        }
        let listing = rooms.list(&whole(root, &folder.path))?;
        if folder.ino.is_some_and(|ino| ino != listing.ino) {
            return Err(String::from(CHANGED));
        }
        let mut siblings = HashSet::new();
        for entry in listing.entries {
            let left = &mut walked.left_out;
            match entry.kind {
                Kind::Link => left.links = left.links.saturating_add(1),
                Kind::Mount => left.mounts = left.mounts.saturating_add(1),
                Kind::Other => left.others = left.others.saturating_add(1),
                // The workspace's inbox is the owner's, whatever it is said
                // to be: never handed back.
                Kind::File | Kind::Folder
                    if at_workspace && folder.path.is_empty() && entry.name == b"inbox" =>
                {
                    left.mounts = left.mounts.saturating_add(1);
                }
                Kind::File | Kind::Folder => {
                    let name =
                        std::str::from_utf8(&entry.name).map_err(|_| String::from(NOT_PLAIN))?;
                    if !siblings.insert(name.to_ascii_lowercase()) {
                        return Err(String::from(CASE_TWINS));
                    }
                    let path = if folder.path.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{}/{name}", folder.path)
                    };
                    check_paths([path.as_str()]).map_err(String::from)?;
                    if whole(root, &path).len() > PATH_BYTES {
                        return Err(String::from(PATH_TOO_LONG));
                    }
                    if entry.kind == Kind::Folder {
                        folders += 1;
                        if folders > FOLDERS {
                            return Err(String::from(TOO_MANY_FOLDERS));
                        }
                        waiting.push_back(Waiting {
                            path,
                            ino: Some(entry.ino),
                        });
                    } else {
                        found(&mut walked, path, entry.size)?;
                    }
                }
            }
        }
    }
    if walked.files.is_empty() {
        return Err(String::from(EMPTY));
    }
    // Two paths that are one but for case, or a file where another has a
    // folder, differ first at two names in one folder: refused there, so
    // the Library's check of every path when it keeps them finds none.
    Ok(walked)
}

/// Counts the file at `path` of `size` into `walked`, within the bounds.
fn found(walked: &mut Walked, path: String, size: u64) -> Result<(), String> {
    if u64::try_from(walked.files.len()).unwrap_or(u64::MAX) >= FOLDER_FILES {
        return Err(String::from(TOO_MANY_FILES));
    }
    if size > FILE_BYTES {
        return Err(String::from(FILE_TOO_LARGE));
    }
    walked.bytes = walked.bytes.saturating_add(size);
    if walked.bytes > FOLDER_BYTES {
        return Err(String::from(TOO_LARGE));
    }
    walked.files.push((path, size));
    Ok(())
}

/// The absolute path of `path` in the folder at `root`.
pub(crate) fn whole(root: &str, path: &str) -> String {
    if path.is_empty() {
        root.to_owned()
    } else {
        format!("{root}/{path}")
    }
}

#[cfg(test)]
#[path = "folder_walk/tests.rs"]
pub(crate) mod tests;
