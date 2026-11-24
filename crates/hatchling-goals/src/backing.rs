//! Where goals and tracking are kept: the store and the feed decide
//! everything about a record, and a backing keeps its bytes (WO-047).
//!
//! [`GoalsSource`] reads, and is all the feed's reader holds;
//! [`GoalsBacking`] writes too, and only the store holds one. [`Files`]
//! is the layout before WO-047, kept for the migration and for tests:
//! `<root>/goals/<uid>/<id>.json` and `<root>/tracking/<uid>/log.ndjson`,
//! each directory `0700` and owned by the store's user. The broker's
//! sealed backing keeps the same records encrypted.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use hatchling_memory::owner_dir;

use crate::Error;
use crate::name::GoalId;
use crate::read::{self, GOAL_SUFFIX, GOALS_DIR, TRACKING_DIR, TRACKING_LOG};

/// The read side of where goals and tracking are kept.
pub trait GoalsSource: Send + Sync + std::fmt::Debug {
    /// The bytes of user `uid`'s goal `id`, or `None` when there's none.
    ///
    /// # Errors
    ///
    /// When it can't be read.
    fn goal(&self, uid: u32, id: &GoalId) -> Result<Option<Vec<u8>>, Error>;

    /// The ids of user `uid`'s goals, in no order.
    ///
    /// # Errors
    ///
    /// When they can't be listed, or a name isn't a goal's.
    fn goal_ids(&self, uid: u32) -> Result<Vec<GoalId>, Error>;

    /// The bytes of each of user `uid`'s tracking entries, in the order
    /// they were appended, none ending in a newline.
    ///
    /// # Errors
    ///
    /// When the log can't be read.
    fn tracking(&self, uid: u32) -> Result<Vec<Vec<u8>>, Error>;
}

/// Where goals and tracking are kept, read and written.
pub trait GoalsBacking: GoalsSource {
    /// Keeps `bytes` as user `uid`'s goal `id`, whole or not at all.
    ///
    /// # Errors
    ///
    /// When it can't be written; the goal kept before is then in force.
    fn put_goal(&self, uid: u32, id: &GoalId, bytes: &[u8]) -> Result<(), Error>;

    /// Appends to user `uid`'s tracking log the entry `make` builds for
    /// the next sequence number, holding the log's lock from counting to
    /// keeping, so two writers never share a number. `make` may refuse,
    /// and then nothing is appended. An entry must hold no newline.
    ///
    /// # Errors
    ///
    /// As `make`, or when the log can't be read or written.
    fn append_tracking(
        &self,
        uid: u32,
        make: &mut dyn FnMut(u64) -> Result<Vec<u8>, Error>,
    ) -> Result<(), Error>;
}

/// Goals and tracking as files under `root`, owned by `owner`.
#[derive(Debug)]
pub struct Files {
    root: PathBuf,
    owner: u32,
}

impl Files {
    /// Files under `root`, owned by `owner`.
    #[must_use]
    pub fn new(root: &Path, owner: u32) -> Self {
        Self {
            root: root.to_path_buf(),
            owner,
        }
    }

    /// Files under `root`, owned by this process's user.
    #[must_use]
    pub fn of_this_user(root: &Path) -> Self {
        Self::new(root, rustix::process::getuid().as_raw())
    }

    /// User `uid`'s directory under `kind`, if it's there; refused when
    /// it isn't a `0700` directory of the owner's, at every level.
    fn existing(&self, kind: &str, uid: u32) -> Result<Option<PathBuf>, Error> {
        let uid = uid.to_string();
        Ok(owner_dir::existing_chain(
            &self.root,
            &[kind, &uid],
            self.owner,
        )?)
    }

    /// User `uid`'s directory under `kind`, made `0700` when absent.
    fn made(&self, kind: &str, uid: u32) -> Result<PathBuf, Error> {
        let uid = uid.to_string();
        Ok(owner_dir::ensure_chain(
            &self.root,
            &[kind, &uid],
            self.owner,
        )?)
    }

    /// Every user with goals or tracking here, by uid, sorted.
    ///
    /// # Errors
    ///
    /// When a directory can't be read.
    pub fn users(&self) -> Result<Vec<u32>, Error> {
        let mut users = Vec::new();
        for kind in [GOALS_DIR, TRACKING_DIR] {
            let dir = self.root.join(kind);
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(Error::Io { source }),
            };
            for entry in entries {
                let entry = entry.map_err(|source| Error::Io { source })?;
                if let Some(uid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                {
                    users.push(uid);
                }
            }
        }
        users.sort_unstable();
        users.dedup();
        Ok(users)
    }

    /// The name of each of user `uid`'s goal files, without its suffix:
    /// `None` for one that isn't valid UTF-8. A name needn't be a goal
    /// id; [`GoalsSource::goal_ids`] refuses one that isn't, and a
    /// migration leaves it where it is.
    ///
    /// # Errors
    ///
    /// When the directory isn't the owner's `0700` one, or can't be read.
    pub fn goal_names(&self, uid: u32) -> Result<Vec<Option<String>>, Error> {
        let Some(dir) = self.existing(GOALS_DIR, uid)? else {
            return Ok(Vec::new());
        };
        Ok(owner_dir::record_files(&dir, GOAL_SUFFIX)?
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(GOAL_SUFFIX))
                    .map(str::to_owned)
            })
            .collect())
    }

    /// Removes user `uid`'s goal `id`, if it's there.
    ///
    /// # Errors
    ///
    /// When it's there and can't be removed.
    pub fn remove_goal(&self, uid: u32, id: &GoalId) -> Result<(), Error> {
        let Some(dir) = self.existing(GOALS_DIR, uid)? else {
            return Ok(());
        };
        match std::fs::remove_file(read::goal_path(&dir, id)) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io { source }),
        }
    }

    /// Removes user `uid`'s tracking log, if it's there.
    ///
    /// # Errors
    ///
    /// When it's there and can't be removed.
    pub fn remove_tracking(&self, uid: u32) -> Result<(), Error> {
        let Some(dir) = self.existing(TRACKING_DIR, uid)? else {
            return Ok(());
        };
        match std::fs::remove_file(dir.join(TRACKING_LOG)) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(Error::Io { source }),
        }
    }

    /// Counts the entries in a log: every entry ends in exactly one
    /// newline, and an entry's text is JSON-escaped, so newlines in the
    /// file are entry boundaries.
    pub(crate) fn count_lines(file: &std::fs::File) -> Result<u64, Error> {
        let mut reader: &std::fs::File = file;
        let mut count: u64 = 0;
        let mut buf = [0u8; 8192];
        loop {
            let read = reader
                .read(&mut buf)
                .map_err(|source| Error::Io { source })?;
            let Some(chunk) = buf.get(..read) else {
                return Err(Error::Corrupt);
            };
            if chunk.is_empty() {
                return Ok(count);
            }
            for byte in chunk {
                if *byte == b'\n' {
                    count = count.saturating_add(1);
                }
            }
        }
    }
}

impl GoalsSource for Files {
    fn goal(&self, uid: u32, id: &GoalId) -> Result<Option<Vec<u8>>, Error> {
        let Some(dir) = self.existing(GOALS_DIR, uid)? else {
            return Ok(None);
        };
        Ok(owner_dir::read_bytes(&read::goal_path(&dir, id))?)
    }

    fn goal_ids(&self, uid: u32) -> Result<Vec<GoalId>, Error> {
        self.goal_names(uid)?
            .iter()
            .map(|name| {
                name.as_deref()
                    .and_then(|stem| stem.parse().ok())
                    .ok_or(Error::Corrupt)
            })
            .collect()
    }

    fn tracking(&self, uid: u32) -> Result<Vec<Vec<u8>>, Error> {
        let Some(dir) = self.existing(TRACKING_DIR, uid)? else {
            return Ok(Vec::new());
        };
        let Some(bytes) = owner_dir::read_bytes(&dir.join(TRACKING_LOG))? else {
            return Ok(Vec::new());
        };
        Ok(read::lines(&bytes))
    }
}

impl GoalsBacking for Files {
    fn put_goal(&self, uid: u32, id: &GoalId, bytes: &[u8]) -> Result<(), Error> {
        let dir = self.made(GOALS_DIR, uid)?;
        owner_dir::write_atomic(&dir, &read::goal_path(&dir, id), bytes)?;
        Ok(())
    }

    fn append_tracking(
        &self,
        uid: u32,
        make: &mut dyn FnMut(u64) -> Result<Vec<u8>, Error>,
    ) -> Result<(), Error> {
        use std::os::unix::fs::OpenOptionsExt as _;

        let dir = self.made(TRACKING_DIR, uid)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(owner_dir::FILE_MODE)
            .open(dir.join(TRACKING_LOG))
            .map_err(|source| Error::Io { source })?;
        // Released when `file` drops, on every return path.
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).map_err(|errno| {
            Error::Io {
                source: std::io::Error::from(errno),
            }
        })?;
        let mut line = make(Self::count_lines(&file)?)?;
        if line.contains(&b'\n') {
            return Err(Error::Corrupt);
        }
        line.push(b'\n');
        // The whole line in one write on an `O_APPEND` descriptor:
        // nothing already in the log can be altered.
        (&file)
            .write_all(&line)
            .map_err(|source| Error::Io { source })?;
        file.sync_all().map_err(|source| Error::Io { source })
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    /// A `0700` root of a test's.
    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-goals-backing-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        made(&root);
        root
    }

    fn made(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// User `uid`'s directory under `kind`, as the store makes it.
    fn user_dir(root: &Path, kind: &str, uid: u32) -> PathBuf {
        let dir = root.join(kind).join(uid.to_string());
        made(&root.join(kind));
        made(&dir);
        dir
    }

    #[test]
    fn the_users_are_each_uid_with_goals_or_tracking_once_in_order() {
        let root = test_root("users");
        let files = Files::of_this_user(&root);
        assert!(files.users().unwrap().is_empty(), "neither directory there");
        for (kind, uid) in [
            (GOALS_DIR, 9),
            (GOALS_DIR, 3),
            (TRACKING_DIR, 3),
            (TRACKING_DIR, 5),
        ] {
            user_dir(&root, kind, uid);
        }
        made(&root.join(GOALS_DIR).join("not-a-uid"));
        assert_eq!(files.users().unwrap(), [3, 5, 9]);
        // A directory that can't be listed is said, never taken for empty.
        std::fs::remove_dir_all(root.join(TRACKING_DIR)).unwrap();
        std::fs::write(root.join(TRACKING_DIR), b"not a directory").unwrap();
        assert!(matches!(files.users(), Err(Error::Io { .. })));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removing_what_isnt_there_is_fine_and_a_failure_is_said() {
        let root = test_root("remove");
        let files = Files::of_this_user(&root);
        let id: GoalId = "g-a".parse().unwrap();
        // Nothing there, not even the directories.
        files.remove_goal(7, &id).unwrap();
        files.remove_tracking(7).unwrap();
        // The directories there, the files not.
        let goals = user_dir(&root, GOALS_DIR, 7);
        let tracking = user_dir(&root, TRACKING_DIR, 7);
        files.remove_goal(7, &id).unwrap();
        files.remove_tracking(7).unwrap();
        // The files there: removed.
        files.put_goal(7, &id, b"{}").unwrap();
        files
            .append_tracking(7, &mut |_| Ok(b"{}".to_vec()))
            .unwrap();
        files.remove_goal(7, &id).unwrap();
        files.remove_tracking(7).unwrap();
        assert!(!read::goal_path(&goals, &id).exists());
        assert!(!tracking.join(TRACKING_LOG).exists());
        // A file that can't be removed is said.
        std::fs::create_dir(read::goal_path(&goals, &id)).unwrap();
        std::fs::create_dir(tracking.join(TRACKING_LOG)).unwrap();
        assert!(matches!(files.remove_goal(7, &id), Err(Error::Io { .. })));
        assert!(matches!(files.remove_tracking(7), Err(Error::Io { .. })));
        let _ = std::fs::remove_dir_all(&root);
    }
}
