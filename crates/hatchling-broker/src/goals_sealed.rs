//! Goals sealed where they live (WO-047).
//!
//! The owner's goals and what they tracked are kept in [`Store::GOALS`],
//! under the vault's key, in the `sealed` directory of the goals root: a
//! copy of the disk reads none of them without what opens the vault.
//! [`hatchling_goals::GoalsStore`] and the feed's reader decide everything
//! about a record; this keeps its bytes ([`GoalsBacking`]), as values that
//! are whole or not at all ([`crate::sealed_blobs`]):
//!
//! - each goal, under the key of its user and id, and each user's index
//!   of goal ids;
//! - each user's tracking log, as segments of [`PER_SEGMENT`] entries,
//!   and its head, the number of entries the log holds.
//!
//! **Order.** A goal is written before its index names it. A tracking
//! entry's segment is written before the head counts it: a crash between
//! the two leaves an entry past the head, which no read sees and the next
//! append writes over. One lock serializes it all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hatchling_goals::{GoalId, GoalsBacking, GoalsSource};
use hatchling_vault::{OpensStores, SealedVault, Store, StoreKeys};

use crate::sealed_blobs::{self, Failure};

/// The directory the sealed store is, under the goals root.
pub const SEALED_DIR: &str = "sealed";

/// Tracking entries in one segment: at most 64 entries of at most 4 KiB
/// of text each, escaped, fit in a sealed value with room to spare.
pub const PER_SEGMENT: u64 = 64;

/// One writer at a time, across every connection's store.
static LOCK: Mutex<()> = Mutex::new(());

/// The sealed store's root for the goals root `root`.
#[must_use]
pub fn sealed_root(root: &Path) -> PathBuf {
    root.join(SEALED_DIR)
}

/// A tracking log's head: how many entries it holds.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct Head {
    count: u64,
}

/// Goals' and tracking's [`GoalsBacking`] in [`Store::GOALS`].
#[derive(Debug)]
pub struct SealedGoals {
    root: PathBuf,
    keys: StoreKeys,
    per_segment: u64,
}

impl SealedGoals {
    /// The backing of the goals root `root`, its key taken from `keys`.
    ///
    /// # Errors
    ///
    /// When `keys` doesn't hold the goals store at that root.
    pub fn new(root: &Path, keys: &dyn OpensStores) -> Result<Self, hatchling_vault::Error> {
        let root = sealed_root(root);
        let keys = keys.keys_of(&[(Store::GOALS, &root)])?;
        Ok(Self {
            root,
            keys,
            per_segment: PER_SEGMENT,
        })
    }

    fn vault(&self) -> Result<SealedVault<'_>, Failure> {
        Ok(SealedVault::open_in(&self.root, &self.keys, Store::GOALS)?)
    }
}

/// A user's part of a key: the uid, as its decimal digits.
fn user(uid: u32) -> String {
    uid.to_string()
}

/// The key of user `uid`'s goal `id`.
fn goal_key(uid: u32, id: &GoalId) -> String {
    sealed_blobs::key_of(&[b"goal", user(uid).as_bytes(), id.as_str().as_bytes()])
}

/// The key of user `uid`'s index of goal ids.
fn index_key(uid: u32) -> String {
    sealed_blobs::key_of(&[b"goals", user(uid).as_bytes()])
}

/// The key of user `uid`'s tracking head.
fn head_key(uid: u32) -> String {
    sealed_blobs::key_of(&[b"tracking", user(uid).as_bytes()])
}

/// The key of user `uid`'s tracking segment `segment`.
fn segment_key(uid: u32, segment: u64) -> String {
    sealed_blobs::key_of(&[
        b"segment",
        user(uid).as_bytes(),
        segment.to_string().as_bytes(),
    ])
}

/// What a backing's caller is told of a failure.
fn told(failure: Failure) -> hatchling_goals::Error {
    match failure {
        Failure::Corrupt
        | Failure::TooLarge { .. }
        | Failure::Vault(hatchling_vault::Error::DecryptFailed { .. }) => {
            hatchling_goals::Error::Corrupt
        }
        Failure::Vault(error) => hatchling_goals::Error::Io {
            source: std::io::Error::other(error.to_string()),
        },
    }
}

/// The lock, held even if a holder panicked: what it guards is the store,
/// whose every write is whole or not at all.
fn lock() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The entries of a segment's bytes: one per line.
fn entries_of(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut entries: Vec<Vec<u8>> = bytes
        .split(|byte| *byte == b'\n')
        .map(<[u8]>::to_vec)
        .collect();
    entries.pop();
    entries
}

/// A segment's bytes: each entry, then a newline.
fn bytes_of(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in entries {
        bytes.extend_from_slice(entry);
        bytes.push(b'\n');
    }
    bytes
}

impl SealedGoals {
    fn ids_locked(vault: &SealedVault<'_>, uid: u32) -> Result<BTreeSet<String>, Failure> {
        sealed_blobs::read(vault, &index_key(uid))?.map_or_else(
            || Ok(BTreeSet::new()),
            |bytes| serde_json::from_slice(&bytes).map_err(|_| Failure::Corrupt),
        )
    }

    fn count_locked(vault: &SealedVault<'_>, uid: u32) -> Result<u64, Failure> {
        sealed_blobs::read(vault, &head_key(uid))?.map_or(Ok(0), |bytes| {
            serde_json::from_slice::<Head>(&bytes)
                .map(|head| head.count)
                .map_err(|_| Failure::Corrupt)
        })
    }

    fn segment_locked(
        vault: &SealedVault<'_>,
        uid: u32,
        segment: u64,
    ) -> Result<Vec<Vec<u8>>, Failure> {
        Ok(sealed_blobs::read(vault, &segment_key(uid, segment))?
            .map(|bytes| entries_of(&bytes))
            .unwrap_or_default())
    }

    fn put_goal_locked(&self, uid: u32, id: &GoalId, bytes: &[u8]) -> Result<(), Failure> {
        let vault = self.vault()?;
        sealed_blobs::write(&vault, &goal_key(uid, id), bytes)?;
        let mut ids = Self::ids_locked(&vault, uid)?;
        if ids.insert(id.as_str().to_owned()) {
            let index = serde_json::to_vec(&ids).map_err(|_| Failure::Corrupt)?;
            sealed_blobs::write(&vault, &index_key(uid), &index)?;
        }
        Ok(())
    }

    fn tracking_locked(&self, uid: u32) -> Result<Vec<Vec<u8>>, Failure> {
        let vault = self.vault()?;
        let count = Self::count_locked(&vault, uid)?;
        let mut entries = Vec::new();
        for segment in 0..count.div_ceil(self.per_segment) {
            entries.extend(Self::segment_locked(&vault, uid, segment)?);
        }
        let count = usize::try_from(count).map_err(|_| Failure::Corrupt)?;
        if entries.len() < count {
            return Err(Failure::Corrupt);
        }
        entries.truncate(count);
        Ok(entries)
    }

    fn append_locked(
        &self,
        uid: u32,
        make: &mut dyn FnMut(u64) -> Result<Vec<u8>, hatchling_goals::Error>,
    ) -> Result<(), hatchling_goals::Error> {
        let vault = self.vault().map_err(told)?;
        let count = Self::count_locked(&vault, uid).map_err(told)?;
        let segment = count / self.per_segment;
        let at = usize::try_from(count % self.per_segment)
            .map_err(|_| hatchling_goals::Error::Corrupt)?;
        let mut entries = Self::segment_locked(&vault, uid, segment).map_err(told)?;
        if entries.len() < at {
            return Err(hatchling_goals::Error::Corrupt);
        }
        // An entry past the head is one a crash left: never counted, so
        // never read, and written over now.
        entries.truncate(at);
        let entry = make(count)?;
        if entry.contains(&b'\n') {
            return Err(hatchling_goals::Error::Corrupt);
        }
        entries.push(entry);
        sealed_blobs::write(&vault, &segment_key(uid, segment), &bytes_of(&entries))
            .map_err(told)?;
        let head = serde_json::to_vec(&Head { count: count + 1 })
            .map_err(|_| hatchling_goals::Error::Corrupt)?;
        sealed_blobs::write(&vault, &head_key(uid), &head).map_err(told)
    }

    /// Keeps `entries` as user `uid`'s whole tracking log, which holds
    /// none yet: every segment first, then the head, so a cut-short
    /// import counts nothing and is written over when run again.
    fn import_locked(&self, uid: u32, entries: &[Vec<u8>]) -> Result<(), Failure> {
        let vault = self.vault()?;
        if Self::count_locked(&vault, uid)? != 0 {
            return Err(Failure::Corrupt);
        }
        let per = usize::try_from(self.per_segment).map_err(|_| Failure::Corrupt)?;
        for (segment, chunk) in (0_u64..).zip(entries.chunks(per)) {
            sealed_blobs::write(&vault, &segment_key(uid, segment), &bytes_of(chunk))?;
        }
        let count = u64::try_from(entries.len()).map_err(|_| Failure::Corrupt)?;
        let head = serde_json::to_vec(&Head { count }).map_err(|_| Failure::Corrupt)?;
        sealed_blobs::write(&vault, &head_key(uid), &head)
    }
}

impl GoalsSource for SealedGoals {
    fn goal(&self, uid: u32, id: &GoalId) -> Result<Option<Vec<u8>>, hatchling_goals::Error> {
        let _held = lock();
        let vault = self.vault().map_err(told)?;
        sealed_blobs::read(&vault, &goal_key(uid, id)).map_err(told)
    }

    fn goal_ids(&self, uid: u32) -> Result<Vec<GoalId>, hatchling_goals::Error> {
        let _held = lock();
        let vault = self.vault().map_err(told)?;
        Self::ids_locked(&vault, uid)
            .map_err(told)?
            .iter()
            .map(|id| id.parse().map_err(|_| hatchling_goals::Error::Corrupt))
            .collect()
    }

    fn tracking(&self, uid: u32) -> Result<Vec<Vec<u8>>, hatchling_goals::Error> {
        let _held = lock();
        self.tracking_locked(uid).map_err(told)
    }
}

impl GoalsBacking for SealedGoals {
    fn put_goal(&self, uid: u32, id: &GoalId, bytes: &[u8]) -> Result<(), hatchling_goals::Error> {
        let _held = lock();
        self.put_goal_locked(uid, id, bytes).map_err(told)
    }

    fn append_tracking(
        &self,
        uid: u32,
        make: &mut dyn FnMut(u64) -> Result<Vec<u8>, hatchling_goals::Error>,
    ) -> Result<(), hatchling_goals::Error> {
        let _held = lock();
        self.append_locked(uid, make)
    }
}

/// What opening a sealed backing is told when the keys don't hold it.
fn unheld(error: &hatchling_vault::Error) -> hatchling_goals::Error {
    hatchling_goals::Error::Io {
        source: std::io::Error::other(error.to_string()),
    }
}

/// The goals store at the goals root `root`, its records sealed.
///
/// # Errors
///
/// When the root isn't the store's to open, or `keys` doesn't hold the
/// goals store.
pub fn open(
    root: &Path,
    keys: &dyn OpensStores,
) -> Result<hatchling_goals::GoalsStore, hatchling_goals::Error> {
    let backing = SealedGoals::new(root, keys).map_err(|error| unheld(&error))?;
    hatchling_goals::GoalsStore::with_backing(root, Box::new(backing))
}

/// What the feed reads goals and tracking from, at the goals root
/// `root`: the sealed store, read and nothing more.
///
/// # Errors
///
/// When `keys` doesn't hold the goals store.
pub fn source(
    root: &Path,
    keys: &dyn OpensStores,
) -> Result<Box<dyn GoalsSource>, hatchling_goals::Error> {
    Ok(Box::new(
        SealedGoals::new(root, keys).map_err(|error| unheld(&error))?,
    ))
}

/// A source holding no goals and no tracking: what a reader asked only
/// for memory holds, so memory reads without the goals store.
#[derive(Debug, Clone, Copy)]
pub struct NoGoals;

impl GoalsSource for NoGoals {
    fn goal(&self, _uid: u32, _id: &GoalId) -> Result<Option<Vec<u8>>, hatchling_goals::Error> {
        Ok(None)
    }

    fn goal_ids(&self, _uid: u32) -> Result<Vec<GoalId>, hatchling_goals::Error> {
        Ok(Vec::new())
    }

    fn tracking(&self, _uid: u32) -> Result<Vec<Vec<u8>>, hatchling_goals::Error> {
        Ok(Vec::new())
    }
}

/// The source `query` reads: goals' sealed store under the published
/// keys, or for a memory query, [`NoGoals`].
///
/// # Errors
///
/// As [`source_published`], for a goals or tracking query.
pub fn source_for(
    root: &Path,
    query: &hatchling_goals::FeedQuery,
) -> Result<Box<dyn GoalsSource>, hatchling_goals::Error> {
    match query {
        hatchling_goals::FeedQuery::Memory { .. } => Ok(Box::new(NoGoals)),
        hatchling_goals::FeedQuery::Goals { .. } | hatchling_goals::FeedQuery::Tracking { .. } => {
            source_published(root)
        }
    }
}

/// The published keys, or why there are none.
fn published() -> Result<&'static dyn OpensStores, hatchling_goals::Error> {
    crate::held_keys::published().ok_or_else(|| hatchling_goals::Error::Io {
        source: std::io::Error::other("the goals store's keys weren't handed to this process"),
    })
}

/// [`open`], under the published keys ([`crate::held_keys`]).
///
/// # Errors
///
/// When no keys were published, or as [`open`].
pub fn open_published(root: &Path) -> Result<hatchling_goals::GoalsStore, hatchling_goals::Error> {
    open(root, published()?)
}

/// [`source`], under the published keys ([`crate::held_keys`]).
///
/// # Errors
///
/// When no keys were published, or as [`source`].
pub fn source_published(root: &Path) -> Result<Box<dyn GoalsSource>, hatchling_goals::Error> {
    source(root, published()?)
}

/// What a migration did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Migrated {
    /// Goals sealed, their files removed.
    pub goals: usize,
    /// Tracking entries sealed, their logs removed.
    pub entries: usize,
    /// Files left where they were: a goal file that isn't the goal it's
    /// named for, or a log whose entries aren't whole and in order, or
    /// one whose sealed copy differs.
    pub left: usize,
}

/// Whether `lines` is a whole tracking log: each an entry whose sequence
/// number is its place.
fn whole_log(lines: &[Vec<u8>]) -> bool {
    (0_u64..).zip(lines).all(|(seq, line)| {
        serde_json::from_slice::<hatchling_goals::TrackingEntry>(line)
            .is_ok_and(|entry| entry.seq == seq)
    })
}

/// Moves the goals root `root`'s plaintext (0.9 and before) into its
/// sealed store, under `keys` (WO-047).
///
/// Each goal is read from its file, checked to be the goal its name says,
/// written sealed, and only then its file removed. Each user's tracking
/// log is checked whole and in order, written sealed in one import whose
/// head is written last, and only then removed. A migration cut short at
/// any point loses nothing, and run again finishes it without doubling
/// anything. What isn't a goal or a whole log is left where it is, and
/// logged. Empty directories are removed after.
///
/// # Errors
///
/// When the sealed store can't be opened or written, or a plaintext file
/// can't be read or removed; nothing is then lost.
pub fn migrate(root: &Path, keys: &dyn OpensStores) -> Result<Migrated, hatchling_goals::Error> {
    let files = hatchling_goals::Files::of_this_user(root);
    let sealed = SealedGoals::new(root, keys).map_err(|error| unheld(&error))?;
    let mut done = Migrated::default();
    for uid in files.users()? {
        for name in files.goal_names(uid)? {
            let id: Option<GoalId> = name.as_deref().and_then(|stem| stem.parse().ok());
            let Some(id) = id else {
                tracing::error!(uid, "a goal file isn't named for a goal: left as it is");
                done.left += 1;
                continue;
            };
            let Some(bytes) = files.goal(uid, &id)? else {
                continue;
            };
            let is_goal = serde_json::from_slice::<hatchling_goals::Goal>(&bytes)
                .is_ok_and(|goal| goal.id == id);
            if !is_goal {
                tracing::error!(uid, goal_id = %id, "a goal file isn't the goal it's named for: left as it is");
                done.left += 1;
                continue;
            }
            sealed.put_goal(uid, &id, &bytes)?;
            files.remove_goal(uid, &id)?;
            done.goals += 1;
        }
        let lines = files.tracking(uid)?;
        if lines.is_empty() {
            files.remove_tracking(uid)?;
            continue;
        }
        if !whole_log(&lines) {
            tracing::error!(
                uid,
                "a tracking log isn't whole and in order: left as it is"
            );
            done.left += 1;
            continue;
        }
        let kept = sealed.tracking(uid)?;
        if kept.is_empty() {
            let _held = lock();
            sealed.import_locked(uid, &lines).map_err(told)?;
        } else if kept != lines {
            tracing::error!(
                uid,
                "a tracking log differs from its sealed copy: left as it is"
            );
            done.left += 1;
            continue;
        }
        files.remove_tracking(uid)?;
        done.entries += lines.len();
    }
    for top in ["goals", "tracking"] {
        crate::memory_sealed::remove_empty(&root.join(top));
    }
    if done != Migrated::default() {
        tracing::info!(
            goals = done.goals,
            entries = done.entries,
            left = done.left,
            "goals moved into their sealed store"
        );
    }
    Ok(done)
}

#[cfg(test)]
mod tests;
