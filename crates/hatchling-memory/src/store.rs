//! The broker-side memory store: each item kept by a [`Backing`], one JSON
//! file per item under a `0700` root ([`Files`]) or the broker's sealed
//! store (WO-047).
//!
//! Layout, bound to the broker-asserted [`MemoryPrincipal`]:
//!
//! ```text
//! <root>/user/<uid>/<id>.json
//! <root>/project/<uid>/<name>/<id>.json
//! <root>/session/<session-id>/<id>.json
//! ```
//!
//! The uid in the path is the kernel-authenticated peer, never the
//! agent's claimed identity: project A's `(uid, name)` namespace is a
//! different directory from project B's, so naming another project's
//! name cannot reach its items. Session namespaces are broker-minted
//! per connection.
//!
//! File discipline mirrors the vault: the root is created `0700` and
//! re-validated (mode + owner) on every open; item files are `0600`;
//! writes go through temp-write + fsync + rename so a crash never
//! leaves a torn item.
//!
//! There is deliberately no list, dump, or iterate operation. The
//! store answers scoped single-item reads
//! ([`MemoryStore::recall`]) and scoped tag reads
//! ([`MemoryStore::recall_by_tag`]); [`MemoryStore::purge_expired`]
//! walks the tree, but it is broker-side maintenance — like the
//! vault's one-shot migrate/rekey — never an agent-reachable op.

use std::path::{Path, PathBuf};

use crate::backing::{Backing, Files, Space};
use crate::item::{
    MAX_CONTENT_BYTES, MAX_ENCODED_ITEM_BYTES, MemoryId, MemoryItem, MemoryPrincipal, MemoryScope,
    Tag, reply_encoded_len,
};
use crate::recall_index::{RecallAsk, RecallEntry, Recalled};
use crate::{Error, KeptRecord, RetentionPolicy, is_expired, owner_dir};

/// Whether `item` has a TTL and `now_secs` is past it: [`is_expired`]'s
/// answer read from the item's own fields through other code. An expired
/// item is hidden by `is_expired` alone, but deleted only when this agrees
/// (WO-024), so one changed operator can't delete a live or kept item.
fn past_its_ttl(item: &MemoryItem, now_secs: u64) -> bool {
    match item.retention {
        RetentionPolicy::Ttl { ttl_secs } => now_secs
            .checked_sub(item.created_at_secs)
            .is_some_and(|age| age >= ttl_secs),
        RetentionPolicy::Keep => false,
    }
}

/// The broker-owned memory store.
///
/// `MemoryStore` is `Sync`: the broker shares one across its
/// per-connection threads. Every method takes `&self`; the backing
/// serializes.
#[derive(Debug)]
pub struct MemoryStore {
    /// The validated `0700` root: the broker's own records beside the
    /// items (`sessions/`, `context/`) live under it.
    root: PathBuf,
    /// The uid the root (and its subdirectories) must be owned by.
    owner: u32,
    /// Where the items are kept.
    backing: Box<dyn Backing>,
}

impl MemoryStore {
    /// Opens (creating if absent) the store rooted at `root`, its items
    /// kept as files under it ([`Files`]).
    ///
    /// The root must be a directory with mode `0700` owned by the
    /// current uid — created that way if absent, verified if present.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotADirectory`], [`Error::InsecurePermissions`],
    /// or [`Error::Io`].
    pub fn open(root: &Path) -> Result<Self, Error> {
        Self::open_as(root, rustix::process::getuid().as_raw())
    }

    /// Opens the store rooted at `root`, its items kept by `backing`: the
    /// broker's sealed store (WO-047). The root is validated as
    /// [`open`](Self::open) validates it, for the records beside the items.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    pub fn with_backing(root: &Path, backing: Box<dyn Backing>) -> Result<Self, Error> {
        let owner = rustix::process::getuid().as_raw();
        owner_dir::ensure_owner_only_dir(root, owner)?;
        Ok(Self {
            root: root.to_path_buf(),
            owner,
            backing,
        })
    }

    /// Opens the store, requiring the root to be owned by `owner`.
    /// Split out so tests exercise the ownership check hermetically.
    fn open_as(root: &Path, owner: u32) -> Result<Self, Error> {
        owner_dir::ensure_owner_only_dir(root, owner)?;
        tracing::debug!(store_root = %root.display(), "memory store opened");
        Ok(Self {
            root: root.to_path_buf(),
            owner,
            backing: Box::new(Files::new(root, owner)),
        })
    }

    /// The validated store root.
    pub(crate) fn memory_root(&self) -> &Path {
        &self.root
    }

    /// Ensures `dir` exists as an owner-only (`0700`) directory owned
    /// by the store owner. Used for broker-internal subdirectories
    /// (`sessions/`, `context/`).
    pub(crate) fn ensure_dir(&self, dir: &Path) -> Result<PathBuf, Error> {
        owner_dir::ensure_owner_only_dir(dir, self.owner)?;
        Ok(dir.to_path_buf())
    }

    /// Directory for broker-internal session records, with the same
    /// `0700` discipline as the root. Session records live beside the
    /// memory item tree: they are broker bookkeeping, not
    /// agent-readable memory.
    pub(crate) fn sessions_dir(&self) -> Result<PathBuf, Error> {
        self.ensure_dir(&self.root.join("sessions"))
    }

    /// Atomic raw write of `bytes` to `path`. Used for session
    /// records; memory items go through [`write`](Self::write).
    pub(crate) fn write_raw(dir: &Path, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        owner_dir::write_atomic(dir, path, bytes)
    }

    /// Raw read of one record file. `Ok(None)` when absent.
    pub(crate) fn read_raw<T>(path: &Path) -> Result<Option<T>, Error>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let Some(bytes) = owner_dir::read_bytes(path)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Error::Corrupt)
    }

    /// The space for `principal` + `scope`.
    ///
    /// The binding is the authority: `User` → the peer uid,
    /// `Project(name)` → `(peer uid, name)`, `Session` → the
    /// broker-minted session id. The agent's scope request only
    /// selects which binding applies.
    fn space(principal: &MemoryPrincipal, scope: &MemoryScope) -> Space {
        Space::new(match scope {
            MemoryScope::User => vec![String::from("user"), principal.peer_uid.to_string()],
            MemoryScope::Project(name) => vec![
                String::from("project"),
                principal.peer_uid.to_string(),
                String::from(name.as_str()),
            ],
            MemoryScope::Session => vec![
                String::from("session"),
                String::from(principal.session_id.as_str()),
            ],
        })
    }

    /// The directory a scope's items are files in, under [`Files`]: for
    /// the tests that look at the files themselves.
    #[cfg(test)]
    fn scope_dir(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
    ) -> Result<PathBuf, Error> {
        let space = Self::space(principal, scope);
        let segments: Vec<&str> = space.segments().iter().map(String::as_str).collect();
        owner_dir::ensure_chain(&self.root, &segments, self.owner)
    }

    /// The file of `id` in `dir`, under [`Files`].
    #[cfg(test)]
    fn item_path(dir: &Path, id: &MemoryId) -> PathBuf {
        dir.join(format!("{}.json", id.as_str()))
    }

    /// The item kept for `id` in `space`, parsed; `None` when there's
    /// none. An item that isn't the one asked for is corrupt.
    fn read_item(&self, space: &Space, id: &MemoryId) -> Result<Option<MemoryItem>, Error> {
        let Some(bytes) = self.backing.get(space, id)? else {
            return Ok(None);
        };
        let item: MemoryItem = serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
        if item.id != *id {
            return Err(Error::Corrupt);
        }
        Ok(Some(item))
    }

    /// Writes a memory item, broker-stamping the timestamps.
    ///
    /// `content` is bounded at [`MAX_CONTENT_BYTES`]; `tags` are
    /// validated. On overwrite of a live item, `created_at_secs` is
    /// preserved (TTL runs from creation); overwriting an expired or
    /// absent item starts fresh.
    ///
    /// The written item must fit a session frame when a reply carries
    /// it: its encoded size ([`reply_encoded_len`]) is bounded by
    /// [`MAX_ENCODED_ITEM_BYTES`], so every item the store accepts can
    /// be recalled. A refused write leaves any earlier item
    /// under the id untouched.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidName`] for a bad tag,
    /// [`Error::ContentTooLarge`] for oversize content,
    /// [`Error::EncodedTooLarge`] for an item too large once encoded, or
    /// [`Error::Io`].
    #[allow(clippy::too_many_arguments)]
    pub fn write(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &MemoryId,
        content: &str,
        tags: &[Tag],
        retention: RetentionPolicy,
        now_secs: u64,
    ) -> Result<MemoryItem, Error> {
        self.put_item(
            principal, scope, id, content, tags, retention, now_secs, None,
        )
    }

    /// Keeps `content` under (`scope`, `id`), as the owner approved it,
    /// with `record`, where it came from, at `record`'s keeping time.
    ///
    /// The broker's alone, after the owner's approval: a run's writes go
    /// through [`write`](Self::write), which writes no record.
    ///
    /// # Errors
    ///
    /// As [`write`](Self::write).
    pub fn keep(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &MemoryId,
        content: &str,
        tags: &[Tag],
        record: KeptRecord,
    ) -> Result<MemoryItem, Error> {
        let now_secs = record.kept_at_secs;
        self.put_item(
            principal,
            scope,
            id,
            content,
            tags,
            RetentionPolicy::Keep,
            now_secs,
            Some(record),
        )
    }

    /// Writes an item, with `record` or without one.
    #[allow(
        clippy::too_many_arguments,
        reason = "the item's fields, each its own: a struct would only rename them"
    )]
    fn put_item(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &MemoryId,
        content: &str,
        tags: &[Tag],
        retention: RetentionPolicy,
        now_secs: u64,
        record: Option<KeptRecord>,
    ) -> Result<MemoryItem, Error> {
        if content.len() > MAX_CONTENT_BYTES {
            return Err(Error::ContentTooLarge {
                len: content.len(),
                max: MAX_CONTENT_BYTES,
            });
        }
        let space = Self::space(principal, scope);
        // Preserve the original creation time across overwrites: TTL
        // is measured from creation, not from the last edit.
        let created_at_secs = self
            .read_item(&space, id)
            .ok()
            .flatten()
            .filter(|existing| !is_expired(existing, now_secs))
            .map_or(now_secs, |existing| existing.created_at_secs);
        let item = MemoryItem {
            id: id.clone(),
            scope: scope.clone(),
            content: content.to_owned(),
            tags: tags.to_vec(),
            created_at_secs,
            updated_at_secs: now_secs,
            retention,
            record,
        };
        let bytes = serde_json::to_vec(&item).map_err(|_| Error::Corrupt)?;
        // The frame invariant: measure the item exactly as a reply will
        // carry it, and refuse what could not travel in one frame.
        let encoded = reply_encoded_len(&bytes)?;
        if encoded > MAX_ENCODED_ITEM_BYTES {
            return Err(Error::EncodedTooLarge {
                len: encoded,
                max: MAX_ENCODED_ITEM_BYTES,
            });
        }
        // The index first: a put that fails after it leaves an entry recall
        // skips or one that names this item's last words.
        self.reindex(&space, id, Some(RecallEntry::of(&item)))?;
        self.backing.put(&space, id, &bytes)?;
        tracing::info!(memory_id = %id, "stored memory item");
        Ok(item)
    }

    /// Recalls one item by id within the principal-bound scope.
    ///
    /// Returns `Ok(None)` when no item is stored under the id — or
    /// when the stored item expired: expired items are removed on
    /// sight and stop being recalled, even before
    /// [`purge_expired`](Self::purge_expired) runs.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] for an unreadable item, or
    /// [`Error::Io`].
    pub fn recall(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &MemoryId,
        now_secs: u64,
    ) -> Result<Option<MemoryItem>, Error> {
        let space = Self::space(principal, scope);
        let Some(item) = self.read_item(&space, id)? else {
            return Ok(None);
        };
        if is_expired(&item, now_secs) {
            // Expired on sight: remove so it can never be recalled,
            // then report absence. Best-effort: a racing purge may
            // have taken it first, and a failure here just means the
            // next recall (or purge) filters it again.
            if past_its_ttl(&item, now_secs) && self.backing.remove(&space, id).is_ok() {
                let _ = self.reindex(&space, id, None);
            }
            tracing::debug!(memory_id = %id, "expired memory item purged on recall");
            return Ok(None);
        }
        Ok(Some(item))
    }

    /// Recalls all items carrying `tag` within the principal-bound
    /// scope, ordered by id.
    ///
    /// Scoped tag recall is not a bulk export: it cannot cross the
    /// scope boundary, and it returns only items the caller tagged.
    /// Expired items are filtered (and removed) like in
    /// [`recall`](Self::recall).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] for an unreadable item, or
    /// [`Error::Io`].
    pub fn recall_by_tag(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        tag: &Tag,
        now_secs: u64,
    ) -> Result<Vec<MemoryItem>, Error> {
        let space = Self::space(principal, scope);
        let mut items = Vec::new();
        for id in self.backing.ids(&space)? {
            let Some(item) = self.read_item(&space, &id)? else {
                continue;
            };
            if is_expired(&item, now_secs) {
                if past_its_ttl(&item, now_secs) && self.backing.remove(&space, &id).is_ok() {
                    let _ = self.reindex(&space, &id, None);
                }
                continue;
            }
            if item.tags.iter().any(|t| t == tag) {
                items.push(item);
            }
        }
        items.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Ok(items)
    }

    /// The recall index of `space`: kept, or built from its items when none
    /// is, or what's kept can't be read.
    fn index_of(&self, space: &Space) -> Result<Vec<RecallEntry>, Error> {
        if let Some(entries) = self
            .backing
            .get_index(space)?
            .and_then(|bytes| serde_json::from_slice::<Vec<RecallEntry>>(&bytes).ok())
        {
            return Ok(entries);
        }
        let mut entries = Vec::new();
        for id in self.backing.ids(space)? {
            if let Some(item) = self.read_item(space, &id)? {
                entries.push(RecallEntry::of(&item));
            }
        }
        self.put_index(space, &mut entries)?;
        Ok(entries)
    }

    /// Keeps `entries`, sorted by id, as `space`'s recall index.
    fn put_index(&self, space: &Space, entries: &mut [RecallEntry]) -> Result<(), Error> {
        entries.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        let bytes = serde_json::to_vec(entries).map_err(|_| Error::Corrupt)?;
        self.backing.put_index(space, &bytes)
    }

    /// `space`'s recall index with `id`'s entry replaced by `entry`, or
    /// removed when there's none.
    fn reindex(
        &self,
        space: &Space,
        id: &MemoryId,
        entry: Option<RecallEntry>,
    ) -> Result<(), Error> {
        let mut entries = self.index_of(space)?;
        entries.retain(|kept| kept.id != *id);
        entries.extend(entry);
        self.put_index(space, &mut entries)
    }

    /// What a run is given of the items carrying `tag` in `scopes`, at
    /// `now_secs`, as `ask` says (WO-048): the index within its index
    /// budget and the items given whole within its full budget, ranked by
    /// its words, or the one item it names. It reads each scope's recall
    /// index and then only the items it gives whole.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] for an unreadable item, or [`Error::Io`].
    pub fn recall_budgeted(
        &self,
        principal: &MemoryPrincipal,
        scopes: &[MemoryScope],
        tag: &Tag,
        now_secs: u64,
        ask: &RecallAsk<'_>,
    ) -> Result<Recalled, Error> {
        let mut entries = Vec::new();
        let mut spaces = Vec::new();
        for scope in scopes {
            let space = Self::space(principal, scope);
            for entry in self.index_of(&space)? {
                if entry.recalled_by(tag, now_secs) && ask.only.is_none_or(|id| entry.id == *id) {
                    entries.push(entry);
                    spaces.push(space.clone());
                }
            }
        }
        let chosen =
            crate::recall_index::choose(&entries, ask.query, ask.index_bytes, ask.full_bytes);
        let mut items = Vec::new();
        for &position in &chosen.whole {
            let (Some(entry), Some(space)) = (entries.get(position), spaces.get(position)) else {
                continue;
            };
            // An entry whose item is gone, or changed to no longer be
            // recalled, is skipped.
            if let Some(item) = self.read_item(space, &entry.id)? {
                if crate::recall_index::still_given(&item, now_secs)
                    && item.tags.iter().any(|kept| kept == tag)
                {
                    items.push(item);
                }
            }
        }
        let index = chosen
            .index
            .iter()
            .filter_map(|&position| entries.get(position).cloned())
            .collect();
        Ok(Recalled {
            index,
            items,
            left_out: chosen.left_out,
        })
    }

    /// Rewrites the tags of the item under `id` in the principal-bound
    /// scope to `tags`, keeping everything else as it was: its words, its
    /// times, its retention and its record. `Ok(false)` when there's none.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] for an unreadable item, or [`Error::Io`].
    pub fn retag(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &MemoryId,
        tags: &[Tag],
    ) -> Result<bool, Error> {
        let space = Self::space(principal, scope);
        let Some(mut item) = self.read_item(&space, id)? else {
            return Ok(false);
        };
        item.tags = tags.to_vec();
        let bytes = serde_json::to_vec(&item).map_err(|_| Error::Corrupt)?;
        self.reindex(&space, id, Some(RecallEntry::of(&item)))?;
        self.backing.put(&space, id, &bytes)?;
        Ok(true)
    }

    /// Removes the item under `id` in the principal-bound scope.
    /// Idempotent: removing a missing item reports `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on failure.
    pub fn remove(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &MemoryId,
    ) -> Result<bool, Error> {
        let space = Self::space(principal, scope);
        let removed = self.backing.remove(&space, id)?;
        if removed {
            self.reindex(&space, id, None)?;
        }
        if removed {
            tracing::info!(memory_id = %id, "removed memory item");
        }
        Ok(removed)
    }

    /// Removes every expired item in the store, returning the count.
    ///
    /// Broker-side maintenance, like the vault's one-shot
    /// migrate/rekey: it walks every space, but it is never exposed as
    /// an agent-reachable op. Items that cannot be read are left alone
    /// (an operator inspects those); only provably-expired items are
    /// removed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on failure.
    pub fn purge_expired(&self, now_secs: u64) -> Result<usize, Error> {
        let mut purged = 0;
        for space in self.backing.spaces()? {
            for id in self.backing.ids(&space)? {
                match self.read_item(&space, &id) {
                    Ok(Some(item)) if is_expired(&item, now_secs) => {
                        if past_its_ttl(&item, now_secs) && self.backing.remove(&space, &id)? {
                            self.reindex(&space, &id, None)?;
                            purged += 1;
                        }
                    }
                    // Live items stay; unreadable ones are an operator's
                    // problem, not the purger's.
                    Ok(_) | Err(_) => {}
                }
            }
        }
        if purged > 0 {
            tracing::info!(purged, "purged expired memory items");
        }
        Ok(purged)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::str::FromStr as _;

    use super::*;
    use crate::item::{ProjectName, SessionId};

    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-memory-store-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// A scope directory that can't be read fails a purge: it is never
    /// taken for one holding nothing.
    #[test]
    fn an_unreadable_scope_directory_is_an_error_not_empty() {
        let root = test_root("unreadable-scope");
        let store = MemoryStore::open(&root).unwrap();
        std::fs::write(root.join("user"), b"not a directory").unwrap();
        assert!(matches!(store.purge_expired(0), Err(Error::Io { .. })));
        assert!(Files::of_this_user(&root).spaces().is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn principal(uid: u32, session: &str) -> MemoryPrincipal {
        MemoryPrincipal {
            peer_uid: uid,
            session_id: SessionId::from_str(session).unwrap(),
        }
    }

    fn write(
        store: &MemoryStore,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &str,
        content: &str,
        now: u64,
    ) -> MemoryItem {
        store
            .write(
                principal,
                scope,
                &MemoryId::from_str(id).unwrap(),
                content,
                &[],
                RetentionPolicy::Keep,
                now,
            )
            .unwrap()
    }

    /// Only `keep` writes a record, and keeps it as given; a run's write
    /// over a kept item replaces it with one that has none, no longer
    /// approved by the owner.
    #[test]
    fn keep_writes_the_record_and_a_runs_write_drops_it() {
        let root = test_root("kept-record");
        let store = MemoryStore::open(&root).unwrap();
        let p = principal(7, "sess-kept");
        let id = MemoryId::from_str("kept-1").unwrap();
        let record = KeptRecord::agent_words(Some(String::from("confirm-1")), None, None, 300);
        let kept = store
            .keep(
                &p,
                &MemoryScope::User,
                &id,
                "Their cat is Biscuit.",
                &[],
                record.clone(),
            )
            .unwrap();
        assert_eq!((kept.created_at_secs, kept.updated_at_secs), (300, 300));
        assert_eq!(kept.retention, RetentionPolicy::Keep);
        let read = store
            .recall(&p, &MemoryScope::User, &id, 400)
            .unwrap()
            .unwrap();
        assert_eq!(read.record, Some(record));
        assert!(read.approved_by_owner());
        let replaced = write(
            &store,
            &p,
            &MemoryScope::User,
            "kept-1",
            "Changed by a run.",
            500,
        );
        assert_eq!(replaced.record, None);
        let read = store
            .recall(&p, &MemoryScope::User, &id, 600)
            .unwrap()
            .unwrap();
        assert!(!read.approved_by_owner());
        assert_eq!(
            read.created_at_secs, 300,
            "an overwrite keeps when it was made"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What a counted backing has done.
    #[derive(Debug, Default)]
    struct Counts {
        item_reads: std::sync::atomic::AtomicUsize,
        index_reads: std::sync::atomic::AtomicUsize,
    }

    /// A backing in memory that keeps a recall index and counts its reads.
    /// Items by their space's segments and their id.
    type Kept = std::collections::BTreeMap<(Vec<String>, String), Vec<u8>>;

    #[derive(Debug, Default)]
    struct Counted {
        items: std::sync::Mutex<Kept>,
        index: std::sync::Mutex<std::collections::BTreeMap<Vec<String>, Vec<u8>>>,
        counts: std::sync::Arc<Counts>,
    }

    impl Backing for Counted {
        fn get(&self, space: &Space, id: &MemoryId) -> Result<Option<Vec<u8>>, Error> {
            self.counts
                .item_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self
                .items
                .lock()
                .unwrap()
                .get(&(space.segments().to_vec(), id.to_string()))
                .cloned())
        }

        fn put(&self, space: &Space, id: &MemoryId, bytes: &[u8]) -> Result<(), Error> {
            self.items
                .lock()
                .unwrap()
                .insert((space.segments().to_vec(), id.to_string()), bytes.to_vec());
            Ok(())
        }

        fn remove(&self, space: &Space, id: &MemoryId) -> Result<bool, Error> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .remove(&(space.segments().to_vec(), id.to_string()))
                .is_some())
        }

        fn ids(&self, space: &Space) -> Result<Vec<MemoryId>, Error> {
            let segments = space.segments().to_vec();
            Ok(self
                .items
                .lock()
                .unwrap()
                .keys()
                .filter(|(kept, _)| *kept == segments)
                .map(|(_, id)| MemoryId::from_str(id).unwrap())
                .collect())
        }

        fn spaces(&self) -> Result<Vec<Space>, Error> {
            Ok(Vec::new())
        }

        fn get_index(&self, space: &Space) -> Result<Option<Vec<u8>>, Error> {
            self.counts
                .index_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.index.lock().unwrap().get(space.segments()).cloned())
        }

        fn put_index(&self, space: &Space, bytes: &[u8]) -> Result<(), Error> {
            self.index
                .lock()
                .unwrap()
                .insert(space.segments().to_vec(), bytes.to_vec());
            Ok(())
        }
    }

    /// A store over a counted backing, and its counts.
    fn counted(name: &str) -> (MemoryStore, std::sync::Arc<Counts>, PathBuf) {
        let root = test_root(name);
        let backing = Counted::default();
        let counts = std::sync::Arc::clone(&backing.counts);
        (
            MemoryStore::with_backing(&root, Box::new(backing)).unwrap(),
            counts,
            root,
        )
    }

    fn reads(counts: &Counts) -> (usize, usize) {
        use std::sync::atomic::Ordering::SeqCst;
        (
            counts.item_reads.swap(0, SeqCst),
            counts.index_reads.swap(0, SeqCst),
        )
    }

    /// With 500 items kept, a budgeted recall reads the index once and then
    /// only the items it gives whole: never every item.
    #[test]
    fn a_budgeted_recall_reads_only_what_it_gives() {
        let (store, counts, root) = counted("budgeted");
        let p = principal(7, "sess-budget");
        let recall = Tag::from_str("recall").unwrap();
        for n in 0..500u64 {
            let id = MemoryId::from_str(&format!("kept-{n:03}")).unwrap();
            let record = KeptRecord::agent_words(None, None, None, 1_000 + n);
            store
                .keep(
                    &p,
                    &MemoryScope::User,
                    &id,
                    &format!("Fact number {n}, kept for the owner."),
                    std::slice::from_ref(&recall),
                    record,
                )
                .unwrap();
        }
        let _ = reads(&counts);
        let recalled = store
            .recall_budgeted(
                &p,
                &[MemoryScope::User],
                &recall,
                5_000,
                &RecallAsk {
                    query: &[],
                    only: None,
                    index_bytes: 3 * 1024,
                    full_bytes: 5 * 1024,
                },
            )
            .unwrap();
        let (item_reads, index_reads) = reads(&counts);
        assert_eq!(index_reads, 1, "the index, once");
        assert_eq!(
            item_reads,
            recalled.items.len(),
            "only the items given whole"
        );
        assert!(!recalled.items.is_empty() && recalled.items.len() < 500);
        let words: usize = recalled.items.iter().map(|item| item.content.len()).sum();
        assert!(words <= 5 * 1024, "{words}");
        // The newest first, and the index counts what it had no room for.
        assert_eq!(
            recalled.items.first().map(|item| item.id.as_str()),
            Some("kept-499")
        );
        assert_eq!(
            recalled.index.first().map(|entry| entry.id.as_str()),
            Some("kept-499")
        );
        assert_eq!(recalled.index.len() + recalled.left_out, 500);
        assert!(recalled.left_out > 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What the owner asked finds a memory outside the newest ten, and a
    /// read gives one item alone (WO-048's acceptance).
    #[test]
    fn an_ask_finds_a_memory_outside_the_newest_ten() {
        let (store, _counts, root) = counted("relevance");
        let p = principal(7, "sess-relevance");
        let recall = Tag::from_str("recall").unwrap();
        let keep = |raw: &str, words: &str, at: u64| {
            store
                .keep(
                    &p,
                    &MemoryScope::User,
                    &MemoryId::from_str(raw).unwrap(),
                    words,
                    std::slice::from_ref(&recall),
                    KeptRecord::agent_words(None, None, None, at),
                )
                .unwrap();
        };
        keep("kept-dentist", "Their dentist is Dr Lee, on Mill Road.", 1);
        for n in 0..30u64 {
            keep(
                &format!("kept-{n:02}"),
                &format!("Fact {n} about their garden and the weather."),
                100 + n,
            );
        }
        let given = |query: &[String], only: Option<&MemoryId>, full: usize| -> Vec<String> {
            store
                .recall_budgeted(
                    &p,
                    &[MemoryScope::User],
                    &recall,
                    500,
                    &RecallAsk {
                        query,
                        only,
                        index_bytes: 0,
                        full_bytes: full,
                    },
                )
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.id.to_string())
                .collect()
        };
        // Room for three whole: the newest, unasked; the dentist, asked.
        let three_whole = 3 * "Fact 29 about their garden and the weather.".len();
        assert_eq!(
            given(&[], None, three_whole),
            ["kept-29", "kept-28", "kept-27"]
        );
        let asked = given(
            &crate::recall_index::query_terms("when is the dentist?"),
            None,
            three_whole,
        );
        assert_eq!(
            asked.first().map(String::as_str),
            Some("kept-dentist"),
            "{asked:?}"
        );
        // A read gives the one item it names, and nothing for an id not kept.
        let dentist = MemoryId::from_str("kept-dentist").unwrap();
        assert_eq!(given(&[], Some(&dentist), 4096), ["kept-dentist"]);
        assert!(given(&[], Some(&MemoryId::from_str("kept-none").unwrap()), 4096).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An item past its TTL found by a tag recall is removed, from the
    /// items and from the index.
    #[test]
    fn a_tag_recall_removes_what_is_past_its_ttl() {
        let (store, _counts, root) = counted("ttl");
        let p = principal(7, "sess-ttl");
        let recall = Tag::from_str("recall").unwrap();
        let id = MemoryId::from_str("brief").unwrap();
        store
            .write(
                &p,
                &MemoryScope::User,
                &id,
                "brief",
                std::slice::from_ref(&recall),
                RetentionPolicy::Ttl { ttl_secs: 10 },
                0,
            )
            .unwrap();
        assert!(
            store
                .recall_by_tag(&p, &MemoryScope::User, &recall, 100)
                .unwrap()
                .is_empty()
        );
        let space = MemoryStore::space(&p, &MemoryScope::User);
        assert!(
            store.backing.ids(&space).unwrap().is_empty(),
            "removed from the items"
        );
        let index = store
            .recall_budgeted(
                &p,
                &[MemoryScope::User],
                &recall,
                5,
                &RecallAsk {
                    query: &[],
                    only: None,
                    index_bytes: 4096,
                    full_bytes: 0,
                },
            )
            .unwrap();
        assert!(index.index.is_empty(), "and from the index");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// An item changed to no longer be recalled, without the index being
    /// told, isn't given: its own tags decide.
    #[test]
    fn an_item_retagged_behind_the_index_is_not_given() {
        let (store, _counts, root) = counted("behind");
        let p = principal(7, "sess-behind");
        let recall = Tag::from_str("recall").unwrap();
        let id = MemoryId::from_str("kept-d").unwrap();
        store
            .keep(
                &p,
                &MemoryScope::User,
                &id,
                "kept-d",
                std::slice::from_ref(&recall),
                KeptRecord::agent_words(None, None, None, 1),
            )
            .unwrap();
        let mut retagged = store
            .recall(&p, &MemoryScope::User, &id, 10)
            .unwrap()
            .unwrap();
        retagged.tags = vec![Tag::from_str("superseded").unwrap()];
        let space = MemoryStore::space(&p, &MemoryScope::User);
        store
            .backing
            .put(&space, &id, &serde_json::to_vec(&retagged).unwrap())
            .unwrap();
        let given = store
            .recall_budgeted(
                &p,
                &[MemoryScope::User],
                &recall,
                10,
                &RecallAsk {
                    query: &[],
                    only: None,
                    index_bytes: 4096,
                    full_bytes: 4096,
                },
            )
            .unwrap();
        assert!(given.items.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The index follows every change: a keep, a retag, a remove. An entry
    /// whose item went without the index being told is skipped, and an
    /// index that's missing is built from the items once, then read.
    #[test]
    fn the_recall_index_follows_every_change() {
        let (store, counts, root) = counted("follows");
        let p = principal(7, "sess-follows");
        let recall = Tag::from_str("recall").unwrap();
        let id = |raw: &str| MemoryId::from_str(raw).unwrap();
        let listed = |store: &MemoryStore| -> Vec<String> {
            store
                .recall_budgeted(
                    &p,
                    &[MemoryScope::User],
                    &recall,
                    10,
                    &RecallAsk {
                        query: &[],
                        only: None,
                        index_bytes: 4096,
                        full_bytes: 0,
                    },
                )
                .unwrap()
                .index
                .into_iter()
                .map(|entry| entry.id.to_string())
                .collect()
        };
        let given = |store: &MemoryStore| -> Vec<String> {
            store
                .recall_budgeted(
                    &p,
                    &[MemoryScope::User],
                    &recall,
                    10,
                    &RecallAsk {
                        query: &[],
                        only: None,
                        index_bytes: 4096,
                        full_bytes: 4096,
                    },
                )
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.id.to_string())
                .collect()
        };
        for raw in ["kept-a", "kept-b", "kept-c"] {
            store
                .keep(
                    &p,
                    &MemoryScope::User,
                    &id(raw),
                    raw,
                    std::slice::from_ref(&recall),
                    KeptRecord::agent_words(None, None, None, 1),
                )
                .unwrap();
        }
        assert_eq!(given(&store), ["kept-a", "kept-b", "kept-c"]);
        assert!(
            store
                .retag(
                    &p,
                    &MemoryScope::User,
                    &id("kept-b"),
                    &[Tag::from_str("superseded").unwrap()]
                )
                .unwrap()
        );
        assert!(store.remove(&p, &MemoryScope::User, &id("kept-c")).unwrap());
        assert_eq!(given(&store), ["kept-a"]);
        assert_eq!(
            listed(&store),
            ["kept-a"],
            "the index lists neither the retagged nor the removed"
        );
        // Gone without the index being told: skipped.
        let space = MemoryStore::space(&p, &MemoryScope::User);
        assert!(store.backing.remove(&space, &id("kept-a")).unwrap());
        assert!(given(&store).is_empty());
        // Missing, it's built once from the items, then read.
        store
            .keep(
                &p,
                &MemoryScope::User,
                &id("kept-d"),
                "kept-d",
                std::slice::from_ref(&recall),
                KeptRecord::agent_words(None, None, None, 1),
            )
            .unwrap();
        store.backing.put_index(&space, b"not an index").unwrap();
        let _ = reads(&counts);
        assert_eq!(given(&store), ["kept-d"]);
        let (rebuilt, _) = reads(&counts);
        assert_eq!(rebuilt, 3, "kept-b and kept-d to build it, kept-d given");
        assert_eq!(given(&store), ["kept-d"]);
        assert_eq!(reads(&counts).0, 1, "built, it's read: only kept-d");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Retagging changes the tags alone: the words, times and record stay,
    /// and an item that isn't there is said not to be.
    #[test]
    fn retag_changes_the_tags_alone() {
        let root = test_root("retag");
        let store = MemoryStore::open(&root).unwrap();
        let p = principal(7, "sess-retag");
        let id = MemoryId::from_str("kept-1").unwrap();
        let record = KeptRecord::agent_words(Some(String::from("confirm-1")), None, None, 300);
        let kept = store
            .keep(
                &p,
                &MemoryScope::User,
                &id,
                "Their cat is Biscuit.",
                &[Tag::from_str("recall").unwrap()],
                record,
            )
            .unwrap();
        let old = [Tag::from_str("superseded").unwrap()];
        assert!(store.retag(&p, &MemoryScope::User, &id, &old).unwrap());
        let read = store
            .recall(&p, &MemoryScope::User, &id, 400)
            .unwrap()
            .unwrap();
        assert_eq!(
            read,
            MemoryItem {
                tags: old.to_vec(),
                ..kept
            }
        );
        let none = MemoryId::from_str("kept-none").unwrap();
        assert!(!store.retag(&p, &MemoryScope::User, &none, &old).unwrap());
        assert_eq!(
            store.recall(&p, &MemoryScope::User, &none, 400).unwrap(),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn past_its_ttl_is_expiry_read_from_the_items_own_fields() {
        let item = |retention, created_at_secs| MemoryItem {
            id: MemoryId::from_str("fact-1").unwrap(),
            scope: MemoryScope::User,
            content: String::from("x"),
            tags: Vec::new(),
            created_at_secs,
            updated_at_secs: created_at_secs,
            retention,
            record: None,
        };
        let ttl = item(RetentionPolicy::Ttl { ttl_secs: 100 }, 1_000);
        assert!(!past_its_ttl(&ttl, 1_099));
        assert!(past_its_ttl(&ttl, 1_100), "gone exactly at its deadline");
        assert!(past_its_ttl(&ttl, 1_101));
        assert!(!past_its_ttl(&ttl, 999), "a clock before its creation");
        let kept = item(RetentionPolicy::Keep, 1_000);
        assert!(!past_its_ttl(&kept, u64::MAX));
        let zero = item(RetentionPolicy::Ttl { ttl_secs: 0 }, 1_000);
        assert!(past_its_ttl(&zero, 1_000));
        let far = item(RetentionPolicy::Ttl { ttl_secs: u64::MAX }, 1_000);
        assert!(!past_its_ttl(&far, u64::MAX - 1));
    }

    #[test]
    fn cross_session_write_and_recall() {
        // Acceptance: an item written in one session is recalled in
        // another, for the same peer and project binding.
        let root = test_root("xsession");
        let store = MemoryStore::open(&root).unwrap();
        let scope = MemoryScope::Project(ProjectName::from_str("alpha").unwrap());

        let session_a = principal(1000, "sess-a");
        let written = write(
            &store,
            &session_a,
            &scope,
            "fact-1",
            "alpha uses rust",
            1_000,
        );

        let session_b = principal(1000, "sess-b");
        let recalled = store
            .recall(
                &session_b,
                &scope,
                &MemoryId::from_str("fact-1").unwrap(),
                2_000,
            )
            .unwrap()
            .unwrap();
        assert_eq!(recalled, written);
        assert_eq!(recalled.content, "alpha uses rust");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_isolation_by_peer() {
        // Acceptance (negative): uid 2000 names project "alpha" and
        // the same id, but the binding is (peer, name) — uid 1000's
        // item is unreachable.
        let root = test_root("isolation-peer");
        let store = MemoryStore::open(&root).unwrap();
        let scope = MemoryScope::Project(ProjectName::from_str("alpha").unwrap());

        write(
            &store,
            &principal(1000, "s1"),
            &scope,
            "fact-1",
            "secret-a",
            1_000,
        );

        let other = principal(2000, "s2");
        let recalled = store
            .recall(
                &other,
                &scope,
                &MemoryId::from_str("fact-1").unwrap(),
                2_000,
            )
            .unwrap();
        assert!(
            recalled.is_none(),
            "peer 2000 read peer 1000's project memory"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_isolation_by_name() {
        // Same peer, different project name: separate namespaces.
        let root = test_root("isolation-name");
        let store = MemoryStore::open(&root).unwrap();
        let alpha = MemoryScope::Project(ProjectName::from_str("alpha").unwrap());
        let beta = MemoryScope::Project(ProjectName::from_str("beta").unwrap());
        let me = principal(1000, "s1");

        write(&store, &me, &alpha, "fact-1", "alpha secret", 1_000);

        let recalled = store
            .recall(&me, &beta, &MemoryId::from_str("fact-1").unwrap(), 2_000)
            .unwrap();
        assert!(
            recalled.is_none(),
            "project beta read project alpha's memory"
        );
        // And tag recall cannot cross the name boundary either.
        let tagged = store
            .recall_by_tag(&me, &beta, &Tag::from_str("t").unwrap(), 2_000)
            .unwrap();
        assert!(tagged.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn user_scope_is_per_peer() {
        let root = test_root("user-scope");
        let store = MemoryStore::open(&root).unwrap();
        write(
            &store,
            &principal(1000, "s1"),
            &MemoryScope::User,
            "pref",
            "likes tea",
            1_000,
        );
        let recalled = store
            .recall(
                &principal(2000, "s2"),
                &MemoryScope::User,
                &MemoryId::from_str("pref").unwrap(),
                2_000,
            )
            .unwrap();
        assert!(recalled.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_scope_is_per_broker_session() {
        // Session memory dies with the broker session: a new
        // connection gets a new session id and cannot see the old
        // one's items.
        let root = test_root("session-scope");
        let store = MemoryStore::open(&root).unwrap();
        write(
            &store,
            &principal(1000, "sess-old"),
            &MemoryScope::Session,
            "tmp",
            "scratch",
            1_000,
        );
        let recalled = store
            .recall(
                &principal(1000, "sess-new"),
                &MemoryScope::Session,
                &MemoryId::from_str("tmp").unwrap(),
                2_000,
            )
            .unwrap();
        assert!(recalled.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn expired_item_is_not_recalled_and_purged_on_sight() {
        let root = test_root("expiry");
        let store = MemoryStore::open(&root).unwrap();
        let me = principal(1000, "s1");
        let id = MemoryId::from_str("ephemeral").unwrap();
        store
            .write(
                &me,
                &MemoryScope::User,
                &id,
                "temporary",
                &[],
                RetentionPolicy::Ttl { ttl_secs: 100 },
                1_000,
            )
            .unwrap();

        // Live before the deadline...
        assert!(
            store
                .recall(&me, &MemoryScope::User, &id, 1_099)
                .unwrap()
                .is_some()
        );
        // ...gone at it, and the file is removed on sight.
        assert!(
            store
                .recall(&me, &MemoryScope::User, &id, 1_100)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.purge_expired(1_100).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn purge_expired_removes_only_expired_items() {
        // Acceptance: the expiry purge removes expired items and
        // keeps live ones (both TTL-not-yet-due and Keep).
        let root = test_root("purge");
        let store = MemoryStore::open(&root).unwrap();
        let me = principal(1000, "s1");

        for (id, retention) in [
            ("gone-1", RetentionPolicy::Ttl { ttl_secs: 10 }),
            ("gone-2", RetentionPolicy::Ttl { ttl_secs: 50 }),
            ("live-ttl", RetentionPolicy::Ttl { ttl_secs: 10_000 }),
            ("live-keep", RetentionPolicy::Keep),
        ] {
            store
                .write(
                    &me,
                    &MemoryScope::User,
                    &MemoryId::from_str(id).unwrap(),
                    "x",
                    &[],
                    retention,
                    1_000,
                )
                .unwrap();
        }
        // A second principal's expired item must be purged too: the
        // purge is broker-side maintenance, not scoped to a caller.
        store
            .write(
                &principal(2000, "s9"),
                &MemoryScope::User,
                &MemoryId::from_str("gone-3").unwrap(),
                "y",
                &[],
                RetentionPolicy::Ttl { ttl_secs: 10 },
                1_000,
            )
            .unwrap();

        assert_eq!(store.purge_expired(2_000).unwrap(), 3);
        assert!(
            store
                .recall(
                    &me,
                    &MemoryScope::User,
                    &MemoryId::from_str("live-ttl").unwrap(),
                    2_000
                )
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .recall(
                    &me,
                    &MemoryScope::User,
                    &MemoryId::from_str("live-keep").unwrap(),
                    2_000
                )
                .unwrap()
                .is_some()
        );
        // Second purge is a no-op: idempotent.
        assert_eq!(store.purge_expired(2_000).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn overwrite_preserves_created_at_for_ttl() {
        // TTL runs from creation, not from the last edit: rewriting
        // an item must not silently extend its life.
        let root = test_root("ttl-overwrite");
        let store = MemoryStore::open(&root).unwrap();
        let me = principal(1000, "s1");
        let id = MemoryId::from_str("note").unwrap();

        store
            .write(
                &me,
                &MemoryScope::User,
                &id,
                "v1",
                &[],
                RetentionPolicy::Ttl { ttl_secs: 100 },
                1_000,
            )
            .unwrap();
        let rewritten = store
            .write(
                &me,
                &MemoryScope::User,
                &id,
                "v2",
                &[],
                RetentionPolicy::Ttl { ttl_secs: 100 },
                1_050,
            )
            .unwrap();
        assert_eq!(rewritten.created_at_secs, 1_000);
        assert_eq!(rewritten.updated_at_secs, 1_050);
        // Expires 100s after creation (1_100), not after the edit.
        assert!(
            store
                .recall(&me, &MemoryScope::User, &id, 1_099)
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .recall(&me, &MemoryScope::User, &id, 1_100)
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recall_by_tag_is_scoped_and_ordered() {
        let root = test_root("tag");
        let store = MemoryStore::open(&root).unwrap();
        let me = principal(1000, "s1");
        let tag = Tag::from_str("rust").unwrap();

        for id in ["b-item", "a-item", "untagged"] {
            let tags: Vec<Tag> = if id == "untagged" {
                Vec::new()
            } else {
                vec![tag.clone()]
            };
            store
                .write(
                    &me,
                    &MemoryScope::User,
                    &MemoryId::from_str(id).unwrap(),
                    id,
                    &tags,
                    RetentionPolicy::Keep,
                    1_000,
                )
                .unwrap();
        }
        let items = store
            .recall_by_tag(&me, &MemoryScope::User, &tag, 2_000)
            .unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["a-item", "b-item"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn remove_is_idempotent() {
        let root = test_root("remove");
        let store = MemoryStore::open(&root).unwrap();
        let me = principal(1000, "s1");
        let id = MemoryId::from_str("doomed").unwrap();
        write(&store, &me, &MemoryScope::User, "doomed", "x", 1_000);
        assert!(store.remove(&me, &MemoryScope::User, &id).unwrap());
        assert!(!store.remove(&me, &MemoryScope::User, &id).unwrap());
        assert!(
            store
                .recall(&me, &MemoryScope::User, &id, 2_000)
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn content_size_bound_is_enforced() {
        let root = test_root("too-large");
        let store = MemoryStore::open(&root).unwrap();
        let me = principal(1000, "s1");
        let big = "x".repeat(MAX_CONTENT_BYTES + 1);
        let err = store
            .write(
                &me,
                &MemoryScope::User,
                &MemoryId::from_str("big").unwrap(),
                &big,
                &[],
                RetentionPolicy::Keep,
                1_000,
            )
            .unwrap_err();
        assert!(matches!(err, Error::ContentTooLarge { .. }));
        // Exactly at the bound is fine.
        let ok = "x".repeat(MAX_CONTENT_BYTES);
        store
            .write(
                &me,
                &MemoryScope::User,
                &MemoryId::from_str("ok").unwrap(),
                &ok,
                &[],
                RetentionPolicy::Keep,
                1_000,
            )
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One item's words moved under another's name read as corrupt, not
    /// as the item asked for.
    #[test]
    fn an_item_under_another_items_name_is_corrupt() {
        let root = test_root("renamed");
        let store = MemoryStore::open(&root).unwrap();
        let me = principal(1000, "s1");
        write(&store, &me, &MemoryScope::User, "other", "x", 1_000);
        let dir = store.scope_dir(&me, &MemoryScope::User).unwrap();
        std::fs::copy(dir.join("other.json"), dir.join("victim.json")).unwrap();
        let victim = MemoryId::from_str("victim").unwrap();
        assert!(matches!(
            store.recall(&me, &MemoryScope::User, &victim, 1_000),
            Err(Error::Corrupt)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_item_file_is_corrupt_not_missing() {
        // A tampered item file must surface as corruption, not
        // silently read as absent.
        let root = test_root("corrupt");
        let store = MemoryStore::open(&root).unwrap();
        let me = principal(1000, "s1");
        write(&store, &me, &MemoryScope::User, "victim", "x", 1_000);
        let dir = store.scope_dir(&me, &MemoryScope::User).unwrap();
        std::fs::write(dir.join("victim.json"), "NOT JSON").unwrap();
        let err = store
            .recall(
                &me,
                &MemoryScope::User,
                &MemoryId::from_str("victim").unwrap(),
                2_000,
            )
            .unwrap_err();
        assert!(matches!(err, Error::Corrupt));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_repairs_insecure_mode_but_rejects_wrong_owner() {
        // A pre-existing root with a group-readable mode is repaired
        // to 0700: in the same-uid threat model the mode carries no
        // signal a same-uid observer couldn't produce directly, and
        // concurrent opens require converge-not-fail semantics (see
        // `concurrent_opens_of_a_fresh_root_all_succeed`). A root
        // owned by someone else still fails closed — the UID check is
        // the real boundary.
        let root = test_root("insecure");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        MemoryStore::open(&root).unwrap();
        let mode = std::fs::symlink_metadata(&root).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);

        // Wrong owner also fails.
        let err = MemoryStore::open_as(&root, u32::MAX).unwrap_err();
        assert!(matches!(err, Error::InsecurePermissions { .. }));

        // A file (not a directory) fails too.
        let file = test_root("not-a-dir-file");
        std::fs::write(&file, "x").unwrap();
        let err = MemoryStore::open(&file).unwrap_err();
        assert!(matches!(err, Error::NotADirectory { .. }));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn no_bulk_export_surface_exists() {
        // Structural acceptance: the store's public API is exactly
        // the point operations. This test enumerates them so adding
        // a list/dump/iter method breaks the build's expectations
        // here and forces a deliberate WO-level decision.
        //
        // Allowed: open, write, recall, recall_by_tag, remove,
        // purge_expired.
        //
        // Forbidden: list, dump, export, iter, items, all, scan,
        // walk, keys.
        const FORBIDDEN: &[&str] = &[
            "list", "dump", "export", "iter", "items", "all", "scan", "walk", "keys",
        ];
        let source = include_str!("store.rs");
        for name in FORBIDDEN {
            let pattern = format!("pub fn {name}");
            assert!(
                !source.contains(&pattern),
                "bulk-export surface detected: `{pattern}`"
            );
        }
    }

    #[test]
    fn concurrent_opens_of_a_fresh_root_all_succeed() {
        // Regression: racing `open` calls on a fresh root used to
        // trip `InsecurePermissions` when the loser read the mode
        // inside the winner's create→chmod window (surfaced as a
        // flaky egress-fixture failure: two broker threads sharing a
        // pid-keyed memory root). The mode is now pinned
        // unconditionally, so all racers converge on 0700.
        for round in 0..20 {
            let root = test_root(&format!("concurrent-open-{round}"));
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let root = root.clone();
                    std::thread::spawn(move || MemoryStore::open(&root))
                })
                .collect();
            for handle in handles {
                handle.join().unwrap().unwrap();
            }
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn planted_symlink_in_scope_chain_fails_closed() {
        // A symlink at `root/user` must not redirect the store: the
        // per-component walk rejects it instead of traversing it.
        use std::os::unix::fs::symlink;
        let root = test_root("symlink-chain");
        let store = MemoryStore::open(&root).unwrap();
        let target = test_root("symlink-target");
        std::fs::create_dir_all(&target).unwrap();
        symlink(&target, root.join("user")).unwrap();

        let scope = MemoryScope::User;
        let err = store
            .write(
                &principal(1000, "sess-a"),
                &scope,
                &MemoryId::from_str("fact-1").unwrap(),
                "must not land in the target",
                &[],
                RetentionPolicy::Keep,
                1_000,
            )
            .unwrap_err();
        assert!(matches!(err, Error::NotADirectory { .. }));
        // Nothing was written through the link.
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn planted_symlink_at_uid_component_fails_closed() {
        // Same guarantee one level deeper: `root/user/<uid>` as a
        // symlink is rejected, not followed.
        use std::os::unix::fs::symlink;
        let root = test_root("symlink-uid");
        let store = MemoryStore::open(&root).unwrap();
        std::fs::create_dir_all(root.join("user")).unwrap();
        let target = test_root("symlink-uid-target");
        std::fs::create_dir_all(&target).unwrap();
        symlink(&target, root.join("user").join("1000")).unwrap();

        let scope = MemoryScope::User;
        let err = store
            .write(
                &principal(1000, "sess-a"),
                &scope,
                &MemoryId::from_str("fact-1").unwrap(),
                "must not land in the target",
                &[],
                RetentionPolicy::Keep,
                1_000,
            )
            .unwrap_err();
        assert!(matches!(err, Error::NotADirectory { .. }));
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn group_readable_component_is_repaired_to_owner_only() {
        // A pre-existing component with a permissive mode is pinned
        // back to 0700 instead of being left group-readable.
        use std::os::unix::fs::PermissionsExt as _;
        let root = test_root("mode-repair");
        let store = MemoryStore::open(&root).unwrap();
        let user_dir = root.join("user");
        std::fs::create_dir_all(&user_dir).unwrap();
        std::fs::set_permissions(&user_dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        let written = write(
            &store,
            &principal(1000, "sess-a"),
            &MemoryScope::User,
            "fact-1",
            "repaired",
            1_000,
        );
        assert_eq!(written.content, "repaired");
        let mode = std::fs::symlink_metadata(&user_dir).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn running_as_root() -> bool {
        rustix::process::geteuid().is_root()
    }

    #[test]
    fn content_bound_admits_exactly_32_kib() {
        let root = test_root("bound");
        let store = MemoryStore::open(&root).unwrap();
        let p = principal(1000, "sess-bound");
        let id = MemoryId::from_str("big").unwrap();
        let exact = "a".repeat(32 * 1024);
        let over = "a".repeat(32 * 1024 + 1);
        let fits = store.write(
            &p,
            &MemoryScope::User,
            &id,
            &exact,
            &[],
            RetentionPolicy::Keep,
            1,
        );
        let too_big = store.write(
            &p,
            &MemoryScope::User,
            &id,
            &over,
            &[],
            RetentionPolicy::Keep,
            1,
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(fits.is_ok(), "{:?}", fits.err());
        assert!(matches!(too_big, Err(Error::ContentTooLarge { .. })));
    }

    #[test]
    fn open_reports_the_real_create_error() {
        // `open` may ignore only AlreadyExists from create_dir: a parent
        // it cannot write must surface as PermissionDenied, not be
        // skipped into a later NotFound.
        use std::os::unix::fs::PermissionsExt as _;
        if running_as_root() {
            return;
        }
        let parent = test_root("ro-parent");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = MemoryStore::open(&parent.join("store"));
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&parent);
        let err = result.err();
        assert!(
            matches!(&err, Some(Error::Io { source }) if source.kind() == std::io::ErrorKind::PermissionDenied),
            "{err:?}"
        );
    }

    #[test]
    fn remove_reports_errors_other_than_not_found() {
        // A directory where the item file would be: unlink fails with
        // EISDIR, which is not "already gone".
        let root = test_root("remove-dir");
        let store = MemoryStore::open(&root).unwrap();
        let p = principal(1000, "sess-rm");
        let id = MemoryId::from_str("blocked").unwrap();
        let dir = store.scope_dir(&p, &MemoryScope::User).unwrap();
        std::fs::create_dir(MemoryStore::item_path(&dir, &id)).unwrap();
        let result = store.remove(&p, &MemoryScope::User, &id);
        let _ = std::fs::remove_dir_all(&root);
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn recall_reports_an_unreadable_item() {
        use std::os::unix::fs::PermissionsExt as _;
        if running_as_root() {
            return;
        }
        let root = test_root("unreadable");
        let store = MemoryStore::open(&root).unwrap();
        let p = principal(1000, "sess-unreadable");
        write(&store, &p, &MemoryScope::User, "secret", "x", 1);
        let id = MemoryId::from_str("secret").unwrap();
        let path = MemoryStore::item_path(&store.scope_dir(&p, &MemoryScope::User).unwrap(), &id);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = store.recall(&p, &MemoryScope::User, &id, 1);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
    }

    #[test]
    fn scope_dir_repairs_an_owner_blind_mode() {
        // 0o077 (no owner bits, every group/other bit) is as wrong as any
        // other mode and must be repaired to 0700 on the next access.
        use std::os::unix::fs::PermissionsExt as _;
        let root = test_root("repair-077");
        let store = MemoryStore::open(&root).unwrap();
        let p = principal(1000, "sess-repair");
        let dir = store.scope_dir(&p, &MemoryScope::User).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o077)).unwrap();
        let again = store.scope_dir(&p, &MemoryScope::User);
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        let _ = std::fs::remove_dir_all(&root);
        assert!(again.is_ok(), "{again:?}");
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn purge_ignores_files_that_are_not_items() {
        let root = test_root("purge-stray");
        let store = MemoryStore::open(&root).unwrap();
        let p = principal(1000, "sess-purge");
        let id = MemoryId::from_str("old").unwrap();
        store
            .write(
                &p,
                &MemoryScope::User,
                &id,
                "x",
                &[],
                RetentionPolicy::Ttl { ttl_secs: 1 },
                1,
            )
            .unwrap();
        let path = MemoryStore::item_path(&store.scope_dir(&p, &MemoryScope::User).unwrap(), &id);
        // An expired item's bytes under a non-item name.
        let stray = path.with_extension("bak");
        std::fs::copy(&path, &stray).unwrap();
        let purged = store.purge_expired(1_000).unwrap();
        let stray_kept = stray.exists();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(purged, 1);
        assert!(stray_kept, "a non-item file must never be purged");
    }

    #[test]
    fn recall_by_tag_skips_directories_named_like_items() {
        let root = test_root("tag-dir");
        let store = MemoryStore::open(&root).unwrap();
        let p = principal(1000, "sess-tag");
        let tag = Tag::from_str("t").unwrap();
        store
            .write(
                &p,
                &MemoryScope::User,
                &MemoryId::from_str("real").unwrap(),
                "x",
                std::slice::from_ref(&tag),
                RetentionPolicy::Keep,
                1,
            )
            .unwrap();
        std::fs::create_dir(
            store
                .scope_dir(&p, &MemoryScope::User)
                .unwrap()
                .join("decoy.json"),
        )
        .unwrap();
        let found = store.recall_by_tag(&p, &MemoryScope::User, &tag, 1);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.map(|items| items.len()).ok(), Some(1));
    }
}
