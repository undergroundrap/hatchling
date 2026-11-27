//! Memory sealed where it lives (WO-047).
//!
//! The items the owner approved the agent to remember are kept in
//! [`Store::MEMORY`], under the vault's key, in the `sealed` directory of
//! the memory root: a copy of the disk reads none of them without what
//! opens the vault. [`hatchling_memory::MemoryStore`] decides everything
//! about an item; this keeps its bytes ([`Backing`]), as values that are
//! whole or not at all ([`crate::sealed_blobs`]):
//!
//! - each item, under the key of its space and id;
//! - each space's index, the ids it holds;
//! - the registry, every space that holds items, for maintenance and the
//!   migration.
//!
//! **Order.** An item's value is written before its space's index, and
//! removed before it: a crash can leave an index naming an item that's
//! gone (recall skips it) or an item no index names yet (recalled by id,
//! listed once it's written again), never an item read half-written, or a
//! removed item still recalled by its id. One lock serializes it all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hatchling_memory::{Backing, MemoryId, Space};
use hatchling_vault::{OpensStores, SealedVault, Store, StoreKeys};

use crate::sealed_blobs::{self, Failure};

/// The directory the sealed store is, under the memory root.
pub const SEALED_DIR: &str = "sealed";

/// One writer at a time, across every connection's store.
static LOCK: Mutex<()> = Mutex::new(());

/// The sealed store's root for the memory root `root`.
#[must_use]
pub fn sealed_root(root: &Path) -> PathBuf {
    root.join(SEALED_DIR)
}

/// The memory items' [`Backing`] in [`Store::MEMORY`].
#[derive(Debug)]
pub struct SealedMemory {
    root: PathBuf,
    keys: StoreKeys,
}

impl SealedMemory {
    /// The backing of the memory root `root`, its key taken from `keys`.
    ///
    /// # Errors
    ///
    /// When `keys` doesn't hold the memory store at that root.
    pub fn new(root: &Path, keys: &dyn OpensStores) -> Result<Self, hatchling_vault::Error> {
        let root = sealed_root(root);
        let keys = keys.keys_of(&[(Store::MEMORY, &root)])?;
        Ok(Self { root, keys })
    }

    fn vault(&self) -> Result<SealedVault<'_>, Failure> {
        Ok(SealedVault::open_in(&self.root, &self.keys, Store::MEMORY)?)
    }
}

/// The key of `space`'s item `id`.
fn item_key(space: &Space, id: &MemoryId) -> String {
    let mut parts: Vec<&[u8]> = vec![b"item"];
    parts.extend(space.segments().iter().map(String::as_bytes));
    parts.push(id.as_str().as_bytes());
    sealed_blobs::key_of(&parts)
}

/// The key of `space`'s index.
fn index_key(space: &Space) -> String {
    let mut parts: Vec<&[u8]> = vec![b"index"];
    parts.extend(space.segments().iter().map(String::as_bytes));
    sealed_blobs::key_of(&parts)
}

/// The key of `space`'s recall index (WO-048).
fn recall_index_key(space: &Space) -> String {
    let mut parts: Vec<&[u8]> = vec![b"recall-index"];
    parts.extend(space.segments().iter().map(String::as_bytes));
    sealed_blobs::key_of(&parts)
}

/// The registry's key.
fn registry_key() -> String {
    sealed_blobs::key_of(&[b"spaces"])
}

/// What a backing's caller is told of a failure.
fn told(failure: Failure) -> hatchling_memory::Error {
    match failure {
        Failure::Corrupt => hatchling_memory::Error::Corrupt,
        Failure::TooLarge { len } => hatchling_memory::Error::EncodedTooLarge {
            len,
            max: sealed_blobs::MAX_CHUNKS * hatchling_vault::MAX_SECRET_BYTES,
        },
        Failure::Vault(hatchling_vault::Error::DecryptFailed { .. }) => {
            hatchling_memory::Error::Corrupt
        }
        Failure::Vault(error) => hatchling_memory::Error::Io {
            source: std::io::Error::other(error.to_string()),
        },
    }
}

/// The set kept under `key`, empty when none is.
fn read_set<T: Ord + serde::de::DeserializeOwned>(
    vault: &SealedVault<'_>,
    key: &str,
) -> Result<BTreeSet<T>, Failure> {
    sealed_blobs::read(vault, key)?.map_or_else(
        || Ok(BTreeSet::new()),
        |bytes| serde_json::from_slice(&bytes).map_err(|_| Failure::Corrupt),
    )
}

fn write_set<T: serde::Serialize>(
    vault: &SealedVault<'_>,
    key: &str,
    set: &BTreeSet<T>,
) -> Result<(), Failure> {
    let bytes = serde_json::to_vec(set).map_err(|_| Failure::Corrupt)?;
    sealed_blobs::write(vault, key, &bytes)
}

impl SealedMemory {
    fn put_locked(&self, space: &Space, id: &MemoryId, bytes: &[u8]) -> Result<(), Failure> {
        let vault = self.vault()?;
        sealed_blobs::write(&vault, &item_key(space, id), bytes)?;
        let mut ids: BTreeSet<String> = read_set(&vault, &index_key(space))?;
        if ids.insert(id.as_str().to_owned()) {
            write_set(&vault, &index_key(space), &ids)?;
        }
        let mut spaces: BTreeSet<Space> = read_set(&vault, &registry_key())?;
        if spaces.insert(space.clone()) {
            write_set(&vault, &registry_key(), &spaces)?;
        }
        Ok(())
    }

    fn remove_locked(&self, space: &Space, id: &MemoryId) -> Result<bool, Failure> {
        let vault = self.vault()?;
        let removed = sealed_blobs::remove(&vault, &item_key(space, id))?;
        let mut ids: BTreeSet<String> = read_set(&vault, &index_key(space))?;
        if ids.remove(id.as_str()) {
            write_set(&vault, &index_key(space), &ids)?;
        }
        Ok(removed)
    }
}

/// The lock, held even if a holder panicked: what it guards is the store,
/// whose every write is whole or not at all.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Backing for SealedMemory {
    fn get(
        &self,
        space: &Space,
        id: &MemoryId,
    ) -> Result<Option<Vec<u8>>, hatchling_memory::Error> {
        let _held = lock();
        let vault = self.vault().map_err(told)?;
        sealed_blobs::read(&vault, &item_key(space, id)).map_err(told)
    }

    fn put(
        &self,
        space: &Space,
        id: &MemoryId,
        bytes: &[u8],
    ) -> Result<(), hatchling_memory::Error> {
        let _held = lock();
        self.put_locked(space, id, bytes).map_err(told)
    }

    fn remove(&self, space: &Space, id: &MemoryId) -> Result<bool, hatchling_memory::Error> {
        let _held = lock();
        self.remove_locked(space, id).map_err(told)
    }

    fn ids(&self, space: &Space) -> Result<Vec<MemoryId>, hatchling_memory::Error> {
        let _held = lock();
        let vault = self.vault().map_err(told)?;
        let ids: BTreeSet<String> = read_set(&vault, &index_key(space)).map_err(told)?;
        ids.iter()
            .map(|id| id.parse().map_err(|_| hatchling_memory::Error::Corrupt))
            .collect()
    }

    fn spaces(&self) -> Result<Vec<Space>, hatchling_memory::Error> {
        let _held = lock();
        let vault = self.vault().map_err(told)?;
        let spaces: BTreeSet<Space> = read_set(&vault, &registry_key()).map_err(told)?;
        Ok(spaces.into_iter().collect())
    }

    fn get_index(&self, space: &Space) -> Result<Option<Vec<u8>>, hatchling_memory::Error> {
        let _held = lock();
        let vault = self.vault().map_err(told)?;
        sealed_blobs::read(&vault, &recall_index_key(space)).map_err(told)
    }

    fn put_index(&self, space: &Space, bytes: &[u8]) -> Result<(), hatchling_memory::Error> {
        let _held = lock();
        let vault = self.vault().map_err(told)?;
        sealed_blobs::write(&vault, &recall_index_key(space), bytes).map_err(told)
    }
}

/// The memory store at the memory root `root`, its items sealed.
///
/// # Errors
///
/// When the root isn't the store's to open, or `keys` doesn't hold the
/// memory store.
pub fn open(
    root: &Path,
    keys: &dyn OpensStores,
) -> Result<hatchling_memory::MemoryStore, hatchling_memory::Error> {
    let backing = SealedMemory::new(root, keys).map_err(|error| hatchling_memory::Error::Io {
        source: std::io::Error::other(error.to_string()),
    })?;
    hatchling_memory::MemoryStore::with_backing(root, Box::new(backing))
}

/// The memory store at the memory root `root`, its items sealed under the
/// published keys.
///
/// # Errors
///
/// When no keys were published, or as [`open`].
pub fn open_published(
    root: &Path,
) -> Result<hatchling_memory::MemoryStore, hatchling_memory::Error> {
    let keys = crate::held_keys::published().ok_or_else(|| hatchling_memory::Error::Io {
        source: std::io::Error::other("the memory store's keys weren't handed to this process"),
    })?;
    open(root, keys)
}

/// What a migration did: items moved into the sealed store, and plaintext
/// files left where they were because they weren't an item.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Migrated {
    /// Items sealed, their plaintext removed.
    pub moved: usize,
    /// Files that weren't an item, left alone and logged.
    pub left: usize,
}

/// Moves the memory root `root`'s plaintext items (0.9 and before) into its
/// sealed store, under `keys` (WO-047).
///
/// Each item is read from its file, checked to be the item its name says,
/// written sealed, and only then its file removed: a migration cut short
/// at any point loses nothing, and run again finishes it. A file that
/// isn't an item is left where it is, and logged. Empty directories are
/// removed after.
///
/// # Errors
///
/// When the sealed store can't be opened or written, or a plaintext file
/// can't be read or removed; nothing is then lost.
pub fn migrate(root: &Path, keys: &dyn OpensStores) -> Result<Migrated, hatchling_memory::Error> {
    let files = hatchling_memory::Files::of_this_user(root);
    let sealed = SealedMemory::new(root, keys).map_err(|error| hatchling_memory::Error::Io {
        source: std::io::Error::other(error.to_string()),
    })?;
    let mut done = Migrated::default();
    for space in files.spaces()? {
        for id in files.ids(&space)? {
            let Some(bytes) = files.get(&space, &id)? else {
                continue;
            };
            let is_item = serde_json::from_slice::<hatchling_memory::MemoryItem>(&bytes)
                .is_ok_and(|item| item.id == id);
            if !is_item {
                tracing::error!(memory_id = %id, "a memory file isn't the item it's named for: left as it is");
                done.left += 1;
                continue;
            }
            sealed.put(&space, &id, &bytes)?;
            files.remove(&space, &id)?;
            done.moved += 1;
        }
    }
    for top in ["user", "project", "session"] {
        remove_empty(&root.join(top));
    }
    if done.moved > 0 || done.left > 0 {
        tracing::info!(
            moved = done.moved,
            left = done.left,
            "memory moved into its sealed store"
        );
    }
    Ok(done)
}

/// Removes `dir` and the directories under it that hold nothing,
/// best-effort: one left is harmless.
pub(crate) fn remove_empty(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            remove_empty(&entry.path());
        }
    }
    let _ = std::fs::remove_dir(dir);
}

#[cfg(test)]
mod tests;
