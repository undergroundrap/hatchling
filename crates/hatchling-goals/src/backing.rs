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
        let Some(dir) = self.existing(GOALS_DIR, uid)? else {
            return Ok(Vec::new());
        };
        owner_dir::record_files(&dir, GOAL_SUFFIX)?
            .iter()
            .map(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(GOAL_SUFFIX))
                    .and_then(|stem| stem.parse::<GoalId>().ok())
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
