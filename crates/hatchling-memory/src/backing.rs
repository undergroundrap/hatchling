//! Where a memory store keeps its items (WO-047).
//!
//! [`MemoryStore`](crate::MemoryStore) decides everything about an item:
//! its scope's binding, its bounds, its TTL, what recall returns. It keeps
//! the item's bytes through a [`Backing`]: [`Files`], one JSON file per
//! item under the store's root as before, or the broker's sealed store,
//! which this crate can't name (the agent's cell links this crate, and the
//! vault is the trusted side's alone).
//!
//! A backing is told a scope's [`Space`] and an item's [`MemoryId`] and
//! keeps bytes: it never parses an item, and never decides which scope a
//! caller may reach.

use std::path::{Path, PathBuf};

use crate::item::MemoryId;
use crate::{Error, owner_dir};

/// A scope's place: the segments its binding resolves to.
///
/// Such as `["user", "1000"]`, `["project", "1000", "notes"]` or
/// `["session", "<id>"]`. Built by the store alone, from the
/// broker-asserted principal.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Space(Vec<String>);

impl Space {
    /// The space of `segments`.
    #[must_use]
    pub const fn new(segments: Vec<String>) -> Self {
        Self(segments)
    }

    /// Its segments, in order.
    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.0
    }
}

/// Where items are kept.
pub trait Backing: Send + Sync + std::fmt::Debug {
    /// The bytes kept for `id` in `space`, or `None` when there are none.
    ///
    /// # Errors
    ///
    /// When they can't be read.
    fn get(&self, space: &Space, id: &MemoryId) -> Result<Option<Vec<u8>>, Error>;

    /// Keeps `bytes` for `id` in `space`, whole or not at all.
    ///
    /// # Errors
    ///
    /// When they can't be kept; any earlier bytes for `id` are then as
    /// they were.
    fn put(&self, space: &Space, id: &MemoryId, bytes: &[u8]) -> Result<(), Error>;

    /// Removes `id` from `space`: `Ok(false)` when nothing was kept.
    ///
    /// # Errors
    ///
    /// When it can't be removed.
    fn remove(&self, space: &Space, id: &MemoryId) -> Result<bool, Error>;

    /// The ids kept in `space`, in no set order.
    ///
    /// # Errors
    ///
    /// When they can't be read.
    fn ids(&self, space: &Space) -> Result<Vec<MemoryId>, Error>;

    /// Every space holding items: the store's own maintenance alone walks
    /// it, never an agent-reachable op.
    ///
    /// # Errors
    ///
    /// When they can't be read.
    fn spaces(&self) -> Result<Vec<Space>, Error>;

    /// The recall index kept for `space` (WO-048), or `None` when none is:
    /// the store then builds it from the items. A backing that keeps none
    /// has every recall read every item.
    ///
    /// # Errors
    ///
    /// When it can't be read.
    fn get_index(&self, _space: &Space) -> Result<Option<Vec<u8>>, Error> {
        Ok(None)
    }

    /// Keeps `bytes` as `space`'s recall index, whole or not at all. A
    /// backing that keeps none drops it.
    ///
    /// # Errors
    ///
    /// When it can't be kept; the index before is then as it was.
    fn put_index(&self, _space: &Space, _bytes: &[u8]) -> Result<(), Error> {
        Ok(())
    }
}

/// An item's file suffix. Only files with it are items.
const ITEM_SUFFIX: &str = ".json";

/// Items as files: `<root>/<segments...>/<id>.json`.
///
/// Each directory is `0700` and owned by `owner`, each file written whole
/// (temp, fsync, rename, directory fsync). The store's layout before
/// WO-047, and what a migration reads.
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

    /// The directory of `space`, made `0700` level by level, never through
    /// a link ([`owner_dir::ensure_chain`]).
    fn dir(&self, space: &Space) -> Result<PathBuf, Error> {
        let segments: Vec<&str> = space.segments().iter().map(String::as_str).collect();
        owner_dir::ensure_chain(&self.root, &segments, self.owner)
    }

    /// The file of `id` in `dir`. The id's alphabet has no separator, so
    /// joining is safe by construction.
    fn path(dir: &Path, id: &MemoryId) -> PathBuf {
        dir.join(format!("{}{ITEM_SUFFIX}", id.as_str()))
    }

    /// The directories under `dir` that are directories, by name.
    fn subdirs(dir: &Path) -> Result<Vec<String>, Error> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(Error::Io { source }),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| Error::Io { source })?;
            if entry
                .file_type()
                .map_err(|source| Error::Io { source })?
                .is_dir()
            {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        Ok(names)
    }
}

impl Backing for Files {
    fn get(&self, space: &Space, id: &MemoryId) -> Result<Option<Vec<u8>>, Error> {
        owner_dir::read_bytes(&Self::path(&self.dir(space)?, id))
    }

    fn put(&self, space: &Space, id: &MemoryId, bytes: &[u8]) -> Result<(), Error> {
        let dir = self.dir(space)?;
        owner_dir::write_atomic(&dir, &Self::path(&dir, id), bytes)
    }

    fn remove(&self, space: &Space, id: &MemoryId) -> Result<bool, Error> {
        match std::fs::remove_file(Self::path(&self.dir(space)?, id)) {
            Ok(()) => Ok(true),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::Io { source }),
        }
    }

    fn ids(&self, space: &Space) -> Result<Vec<MemoryId>, Error> {
        let mut ids = Vec::new();
        for path in owner_dir::record_files(&self.dir(space)?, ITEM_SUFFIX)? {
            let id = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(ITEM_SUFFIX))
                .and_then(|stem| stem.parse::<MemoryId>().ok());
            // A file no id names isn't an item: left alone, as scans
            // always have.
            if let Some(id) = id {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn spaces(&self) -> Result<Vec<Space>, Error> {
        let mut spaces = Vec::new();
        let space =
            |segments: &[&str]| Space::new(segments.iter().map(|&s| s.to_owned()).collect());
        for uid in Self::subdirs(&self.root.join("user"))? {
            spaces.push(space(&["user", &uid]));
        }
        for uid in Self::subdirs(&self.root.join("project"))? {
            for name in Self::subdirs(&self.root.join("project").join(&uid))? {
                spaces.push(space(&["project", &uid, &name]));
            }
        }
        for session in Self::subdirs(&self.root.join("session"))? {
            spaces.push(space(&["session", &session]));
        }
        Ok(spaces)
    }
}
