//! `hatchling-memory`: the broker-owned durable memory store.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! User memory (preferences, facts, standing context) and project memory
//! (per-project notes, decisions) that survive across sessions. The store
//! lives on the host side, behind the peer-authorized broker session —
//! the agent never touches these files; filesystem permissions are the
//! second barrier, exactly like the vault.
//!
//! Three structural guarantees, enforced by construction:
//!
//! 1. **Memory is recalled data, never instructions.** Recalled items
//!    reach the prompt below the system prompt and the live user task,
//!    labeled as recalled ([`prompt::render_recalled_block`]). Position
//!    plus labeling is the precedence mechanism — not a prose request.
//! 2. **No bulk export.** The store speaks scoped single-item reads
//!    ([`MemoryStore::recall`], [`MemoryStore::recall_by_tag`]); there
//!    is no list, dump, or iterate operation, so a compromised reader
//!    cannot exfiltrate the store item by item without already knowing
//!    every id.
//! 3. **Explicit retention.** Every item carries a [`RetentionPolicy`]
//!    at write time. "Keep forever" ([`RetentionPolicy::Keep`]) is an
//!    explicit choice; there is no `Default` impl, so silent infinite
//!    retention is unrepresentable. Expired items stop being recalled
//!    and are purged by [`MemoryStore::purge_expired`].
//!
//! Project binding is broker-asserted: the agent names a project, but
//! the broker binds the name to the authenticated peer identity
//! ([`MemoryPrincipal`]). Project A cannot read project B's memory by
//! naming it — the binding, not the name, decides visibility.
//!
//! [`session`] holds the loop's durable session record (open tasks,
//! pending confirmations, in-flight tool calls). A crashed-then-resumed
//! session reconciles rather than replays: in-flight tool calls are
//! marked interrupted, never auto-executed.
//!
//! [`context_log`] is the append-only audit trail. The agent cannot
//! rewrite it: the only mutating operation is append.
//!
//! Storage backend: flat JSON files under a `0700` root, atomic
//! temp-write + rename, `0600` item files — the vault's file discipline,
//! applied to memory items. Encryption at rest (sealed envelopes, the
//! pattern) is a named follow-up, not v0.1 scope.

pub mod compaction;
pub mod context_log;
pub mod error;
pub mod item;
pub mod owner_dir;
pub mod paging;
pub mod prompt;
pub mod session;
pub mod store;

pub use compaction::{CompactionSummary, compact};
pub use context_log::{ContextEntry, ContextKind, ContextLog, MAX_CONTEXT_PAYLOAD_BYTES};
pub use error::Error;
pub use item::{
    MAX_CONTENT_BYTES, MAX_ENCODED_ITEM_BYTES, MemoryId, MemoryItem, MemoryPrincipal, MemoryScope,
    ProjectName, RetentionPolicy, SessionId, Tag, expires_at, is_expired, reply_encoded_len,
};
pub use paging::{PAGE_BUDGET_BYTES, Page, page};
pub use prompt::render_recalled_block;
pub use session::{
    ConfirmationRecord, SessionRecord, SessionStatus, SessionStore, TaskRecord, ToolCallOutcome,
    ToolCallRecord,
};
pub use store::MemoryStore;

#[cfg(test)]
mod frame_bound_tests {
    use std::str::FromStr as _;

    use crate::item::{MAX_ENCODED_ITEM_BYTES, reply_encoded_len};
    use crate::{
        ContextKind, ContextLog, Error, MemoryId, MemoryPrincipal, MemoryScope, MemoryStore,
        RetentionPolicy, SessionId,
    };

    fn store(name: &str) -> (MemoryStore, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "hatchling-memory-frame-bound-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        (MemoryStore::open(&root).unwrap(), root)
    }

    fn principal() -> MemoryPrincipal {
        MemoryPrincipal {
            peer_uid: 7,
            session_id: SessionId::from_str("sess-bound").unwrap(),
        }
    }

    fn write(store: &MemoryStore, id: &str, content: &str) -> Result<crate::MemoryItem, Error> {
        store.write(
            &principal(),
            &MemoryScope::User,
            &MemoryId::from_str(id).unwrap(),
            content,
            &[],
            RetentionPolicy::Keep,
            1_000,
        )
    }

    fn encoded(item: &crate::MemoryItem) -> usize {
        reply_encoded_len(&serde_json::to_vec(item).unwrap()).unwrap()
    }

    #[test]
    fn reply_encoded_len_counts_the_second_encoding() {
        // `ab` → `"ab"`; a quote costs 2 more, a control character 5.
        assert_eq!(reply_encoded_len(b"ab").unwrap(), 4);
        assert_eq!(reply_encoded_len(b"a\"").unwrap(), 5);
        assert_eq!(reply_encoded_len(b"a\x01").unwrap(), 9);
        assert!(matches!(reply_encoded_len(&[0xff]), Err(Error::Corrupt)));
    }

    #[test]
    fn the_encoded_bound_is_exact() {
        // The empty item's encoded size, then quotes (4 bytes each once
        // re-encoded) and plain bytes (1 each) to land exactly on the
        // bound: accepted. One more plain byte: refused.
        let (store, root) = store("exact");
        // Measured under the same id: the id's length is part of the
        // encoded size.
        let empty = encoded(&write(&store, "edge", "").unwrap());
        let spare = MAX_ENCODED_ITEM_BYTES - empty;
        let content = format!("{}{}", "\"".repeat(spare / 4), "a".repeat(spare % 4));
        let item = write(&store, "edge", &content).unwrap();
        assert_eq!(encoded(&item), MAX_ENCODED_ITEM_BYTES);
        let over = format!("{content}a");
        let refused = write(&store, "edge", &over);
        assert!(
            matches!(
                &refused,
                Err(Error::EncodedTooLarge { len, max })
                    if *len == MAX_ENCODED_ITEM_BYTES + 1 && *max == MAX_ENCODED_ITEM_BYTES
            ),
            "{refused:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_refused_write_leaves_the_earlier_item_untouched() {
        let (store, root) = store("untouched");
        let first = write(&store, "note", "kept").unwrap();
        let hostile = "\u{1}".repeat(crate::MAX_CONTENT_BYTES);
        assert!(matches!(
            write(&store, "note", &hostile),
            Err(Error::EncodedTooLarge { .. })
        ));
        let recalled = store
            .recall(
                &principal(),
                &MemoryScope::User,
                &MemoryId::from_str("note").unwrap(),
                1_000,
            )
            .unwrap();
        assert_eq!(recalled, Some(first));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_context_bound_is_exact() {
        // An entry encoding to exactly the bound is appended; one byte
        // more is refused. Measured on the entry the append will build
        // (seq 0, and seq 1 for the refusal: the same width).
        let (store, root) = store("context-exact");
        let log = ContextLog::new(&store);
        let id = SessionId::from_str("sess-exact").unwrap();
        let empty = crate::ContextEntry {
            seq: 0,
            session_id: id.clone(),
            at_secs: 7,
            kind: ContextKind::Note,
            payload_json: String::new(),
        };
        let base = reply_encoded_len(&serde_json::to_vec(&empty).unwrap()).unwrap();
        let spare = MAX_ENCODED_ITEM_BYTES - base;
        let payload = format!("{}{}", "\"".repeat(spare / 4), "a".repeat(spare % 4));
        let entry = log.append(&id, ContextKind::Note, &payload, 7).unwrap();
        assert_eq!(
            reply_encoded_len(&serde_json::to_vec(&entry).unwrap()).unwrap(),
            MAX_ENCODED_ITEM_BYTES
        );
        let refused = log.append(&id, ContextKind::Note, &format!("{payload}a"), 7);
        assert!(
            matches!(
                &refused,
                Err(Error::EncodedTooLarge { len, max })
                    if *len == MAX_ENCODED_ITEM_BYTES + 1 && *max == MAX_ENCODED_ITEM_BYTES
            ),
            "{refused:?}"
        );
        assert_eq!(log.read(&id).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn context_entries_obey_the_same_bound() {
        let (store, root) = store("context");
        let log = ContextLog::new(&store);
        let id = SessionId::from_str("sess-ctx").unwrap();
        log.append(&id, ContextKind::Note, r#""fine""#, 1).unwrap();
        // Within the raw payload bound, but four times too big once a
        // reply carries it.
        let payload = format!("\"{}\"", "\\\"".repeat(14_000));
        assert!(payload.len() <= crate::MAX_CONTEXT_PAYLOAD_BYTES);
        assert!(matches!(
            log.append(&id, ContextKind::Note, &payload, 2),
            Err(Error::EncodedTooLarge { .. })
        ));
        assert_eq!(
            log.read(&id).unwrap().len(),
            1,
            "a refused append adds nothing"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
