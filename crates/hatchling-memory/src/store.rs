//! The broker-side memory store: one JSON file per item under a `0700` root.
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

use crate::item::{
    MAX_CONTENT_BYTES, MAX_ENCODED_ITEM_BYTES, MemoryId, MemoryItem, MemoryPrincipal, MemoryScope,
    Tag, reply_encoded_len,
};
use crate::{Error, RetentionPolicy, is_expired, owner_dir};

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

/// Item file suffix. Only files with this suffix are read; everything
/// else in the tree is ignored by scans.
const ITEM_SUFFIX: &str = ".json";

/// The broker-owned memory store.
///
/// `MemoryStore` is `Sync`: the broker shares one across its
/// per-connection threads. Every method takes `&self`; the filesystem
/// serializes.
#[derive(Debug)]
pub struct MemoryStore {
    /// The validated `0700` root.
    root: PathBuf,
    /// The uid the root (and its subdirectories) must be owned by.
    owner: u32,
}

impl MemoryStore {
    /// Opens (creating if absent) the store rooted at `root`.
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

    /// Opens the store, requiring the root to be owned by `owner`.
    /// Split out so tests exercise the ownership check hermetically.
    fn open_as(root: &Path, owner: u32) -> Result<Self, Error> {
        owner_dir::ensure_owner_only_dir(root, owner)?;
        tracing::debug!(store_root = %root.display(), "memory store opened");
        Ok(Self {
            root: root.to_path_buf(),
            owner,
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
        Self::write_atomic(dir, path, bytes)
    }

    /// Raw read of one record file. `Ok(None)` when absent.
    pub(crate) fn read_raw<T>(path: &Path) -> Result<Option<T>, Error>
    where
        T: for<'de> serde::Deserialize<'de>,
    {
        let Some(bytes) = Self::read_bytes(path)? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| Error::Corrupt)
    }

    /// The scope directory for `principal` + `scope`, creating it
    /// (and parents) as `0700` when absent.
    ///
    /// The binding is the authority: `User` → the peer uid,
    /// `Project(name)` → `(peer uid, name)`, `Session` → the
    /// broker-minted session id. The agent's scope request only
    /// selects which binding applies.
    ///
    /// Every level is created and validated individually: a real
    /// directory (never a symlink), mode `0700`, owned by the broker
    /// uid. The walk deliberately avoids `create_dir_all`: that would
    /// follow a planted symlink and a chmod sweep would then chmod
    /// *through* it. Each component is `symlink_metadata`-checked
    /// before anything beneath it is touched, so a symlink anywhere
    /// in the chain fails closed instead of redirecting the store.
    /// The walk itself is [`owner_dir::ensure_chain`], shared with the
    /// stores that live beside this one.
    fn scope_dir(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
    ) -> Result<PathBuf, Error> {
        let segments: Vec<String> = match scope {
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
        };
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        owner_dir::ensure_chain(&self.root, &segments, self.owner)
    }

    /// The item file for one id. The id's alphabet has no separator,
    /// so joining is safe by construction.
    fn item_path(scope_dir: &Path, id: &MemoryId) -> PathBuf {
        scope_dir.join(format!("{}{ITEM_SUFFIX}", id.as_str()))
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
        if content.len() > MAX_CONTENT_BYTES {
            return Err(Error::ContentTooLarge {
                len: content.len(),
                max: MAX_CONTENT_BYTES,
            });
        }
        let dir = self.scope_dir(principal, scope)?;
        let path = Self::item_path(&dir, id);
        // Preserve the original creation time across overwrites: TTL
        // is measured from creation, not from the last edit.
        let created_at_secs = Self::read_item(&path)
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
        Self::write_atomic(&dir, &path, &bytes)?;
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
    /// Returns [`Error::Corrupt`] for an unreadable item file, or
    /// [`Error::Io`].
    pub fn recall(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &MemoryId,
        now_secs: u64,
    ) -> Result<Option<MemoryItem>, Error> {
        let dir = self.scope_dir(principal, scope)?;
        let path = Self::item_path(&dir, id);
        let Some(item) = Self::read_item(&path)? else {
            return Ok(None);
        };
        if is_expired(&item, now_secs) {
            // Expired on sight: remove so it can never be recalled,
            // then report absence. Best-effort: a racing purge may
            // have taken it first, and a failure here just means the
            // next recall (or purge) filters it again.
            if past_its_ttl(&item, now_secs) {
                let _ = std::fs::remove_file(&path);
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
    /// Returns [`Error::Corrupt`] for an unreadable item file, or
    /// [`Error::Io`].
    pub fn recall_by_tag(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        tag: &Tag,
        now_secs: u64,
    ) -> Result<Vec<MemoryItem>, Error> {
        let dir = self.scope_dir(principal, scope)?;
        let mut items = Vec::new();
        for path in Self::item_files(&dir)? {
            let Some(item) = Self::read_item(&path)? else {
                continue;
            };
            if is_expired(&item, now_secs) {
                if past_its_ttl(&item, now_secs) {
                    let _ = std::fs::remove_file(&path);
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

    /// Removes the item under `id` in the principal-bound scope.
    /// Idempotent: removing a missing item reports `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on filesystem failure.
    pub fn remove(
        &self,
        principal: &MemoryPrincipal,
        scope: &MemoryScope,
        id: &MemoryId,
    ) -> Result<bool, Error> {
        let dir = self.scope_dir(principal, scope)?;
        let path = Self::item_path(&dir, id);
        match std::fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(memory_id = %id, "removed memory item");
                Ok(true)
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(Error::Io { source }),
        }
    }

    /// Removes every expired item in the store, returning the count.
    ///
    /// Broker-side maintenance, like the vault's one-shot
    /// migrate/rekey: it walks the tree, but it is never exposed as
    /// an agent-reachable op. Files that cannot be parsed are left
    /// alone (an operator inspects those); only provably-expired
    /// items are removed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on filesystem failure.
    pub fn purge_expired(&self, now_secs: u64) -> Result<usize, Error> {
        let mut purged = 0;
        // Fixed-depth walk: <root>/{user,project,session}/.../*.json.
        // Only directories are descended; only `*.json` files are
        // considered. Anything else is left untouched.
        let mut dirs = vec![self.root.clone()];
        while let Some(dir) = dirs.pop() {
            let entries = std::fs::read_dir(&dir).map_err(|source| Error::Io { source })?;
            for entry in entries {
                let entry = entry.map_err(|source| Error::Io { source })?;
                let path = entry.path();
                let file_type = entry.file_type().map_err(|source| Error::Io { source })?;
                if file_type.is_dir() {
                    dirs.push(path);
                } else if file_type.is_file()
                    && path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().ends_with(ITEM_SUFFIX))
                {
                    match Self::read_item(&path) {
                        Ok(Some(item)) if is_expired(&item, now_secs) => {
                            if past_its_ttl(&item, now_secs) {
                                std::fs::remove_file(&path)
                                    .map_err(|source| Error::Io { source })?;
                                purged += 1;
                            }
                        }
                        // Live items stay; unparseable files are an
                        // operator's problem, not the purger's.
                        Ok(_) | Err(_) => {}
                    }
                }
            }
        }
        if purged > 0 {
            tracing::info!(purged, "purged expired memory items");
        }
        Ok(purged)
    }

    /// Reads and parses one item file. `Ok(None)` when absent.
    fn read_item(path: &Path) -> Result<Option<MemoryItem>, Error> {
        Self::read_raw(path)
    }

    /// Raw bytes of one record file. `Ok(None)` when absent.
    fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, Error> {
        owner_dir::read_bytes(path)
    }

    /// The `*.json` files directly under `dir`, for scoped scans.
    fn item_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
        owner_dir::record_files(dir, ITEM_SUFFIX)
    }

    /// Atomic write: temp file (`0600`, unique name), fsync, rename,
    /// directory fsync. A crash leaves an orphaned temp file at worst
    /// — never a torn item.
    fn write_atomic(dir: &Path, target: &Path, bytes: &[u8]) -> Result<(), Error> {
        owner_dir::write_atomic(dir, target, bytes)
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
