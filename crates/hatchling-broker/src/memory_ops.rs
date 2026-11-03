//! Broker-side memory operations: the session handler for [`MemoryOp`].
//!
//! The broker owns the [`MemoryStore`]; the agent never touches memory
//! files. Every op is authorized against the configured grants before
//! the store is opened, and the scope is bound through the
//! [`MemoryPrincipal`] derived from kernel peer credentials — the
//! agent's scope string is a wish, the principal is the authority.
//!
//! There is deliberately no list, dump, or iterate operation: the
//! protocol cannot express enumeration (see [`MemoryOp`]), so a
//! compromised agent cannot bulk-read the store.

use std::time::{SystemTime, UNIX_EPOCH};

use hatchling_core::{MemoryOp, MemoryResult};
use hatchling_memory::{
    ContextEntry, ContextLog, MemoryId, MemoryItem, MemoryPrincipal, MemoryScope, MemoryStore,
    PAGE_BUDGET_BYTES, Page, ProjectName, RetentionPolicy, SessionId, Tag, page,
};

use crate::Error;
use crate::config::{MemoryAction, MemoryConfig};

/// Handles one memory op: authorize, bind scope, execute.
///
/// The `principal` is broker-asserted (peer uid from `SO_PEERCRED`,
/// session id minted per connection). The agent-supplied scope string
/// is parsed and bound through it; an unparseable scope is a protocol
/// violation, not a silent default.
///
/// # Errors
///
/// Returns [`Error::Memory`] if the store cannot be opened or an op
/// fails at the backend. Grant denials are not errors: they become
/// [`Denied`](MemoryResult::Denied) replies.
pub fn handle_memory_op(
    config: &MemoryConfig,
    op: &MemoryOp,
    principal: &MemoryPrincipal,
    session_id: &SessionId,
) -> Result<MemoryResult, Error> {
    let action = memory_action(op);
    let scope_str = op_scope_str(op);
    if !is_granted(config, scope_str, action) {
        return Ok(MemoryResult::Denied);
    }
    let scope = bind_scope(scope_str, principal).map_err(|reason| Error::SessionProtocol {
        reason: format!("invalid memory scope: {reason}"),
    })?;
    let store = MemoryStore::open(&config.root).map_err(|source| Error::Memory { source })?;
    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    match op {
        MemoryOp::Write {
            id,
            content,
            tags,
            retention,
            ..
        } => {
            let id = parse_id(id)?;
            let tags = parse_tags(tags)?;
            let retention = parse_retention(retention)?;
            match store.write(principal, &scope, &id, content, &tags, retention, now_secs) {
                Ok(item) => {
                    let _ = item;
                    Ok(MemoryResult::Stored)
                }
                Err(source) => refuse_oversize(source, "memory write"),
            }
        }
        MemoryOp::Recall { id, .. } => {
            let id = parse_id(id)?;
            match store.recall(principal, &scope, &id, now_secs) {
                Ok(Some(item)) => {
                    let item_json = serde_json::to_string(&item).map_err(|_| Error::Memory {
                        source: hatchling_memory::Error::Corrupt,
                    })?;
                    Ok(MemoryResult::Item { item_json })
                }
                Ok(None) => Ok(MemoryResult::NotFound),
                Err(source) => Err(Error::Memory { source }),
            }
        }
        MemoryOp::RecallByTag { tag, after, .. } => {
            let tag = parse_tag(tag)?;
            let after = after.as_deref().map(parse_id).transpose()?;
            match store.recall_by_tag(principal, &scope, &tag, now_secs) {
                Ok(items) => tag_page(items, after.as_ref()),
                Err(source) => Err(Error::Memory { source }),
            }
        }
        MemoryOp::Remove { id, .. } => {
            let id = parse_id(id)?;
            match store.remove(principal, &scope, &id) {
                Ok(removed) => Ok(MemoryResult::Removed { removed }),
                Err(source) => Err(Error::Memory { source }),
            }
        }
        MemoryOp::ContextAppend { text } => {
            let log = ContextLog::new(&store);
            match log.append(
                session_id,
                hatchling_memory::ContextKind::Note,
                text,
                now_secs,
            ) {
                Ok(_) => Ok(MemoryResult::ContextAppended),
                Err(source) => refuse_oversize(source, "context append"),
            }
        }
        MemoryOp::ContextRead { after } => {
            let after = after.as_deref().map(parse_seq).transpose()?;
            let log = ContextLog::new(&store);
            match log.read(session_id) {
                Ok(entries) => context_page(entries, after),
                Err(source) => Err(Error::Memory { source }),
            }
        }
        // A proposal is put to the owner, which takes the session's task:
        // the session itself does that ([`crate::memory_keep::propose`]).
        // Here, nothing is written.
        MemoryOp::Propose { .. } => Ok(MemoryResult::Denied),
    }
}

/// Maps a write's backend failure onto the session.
///
/// Too large for the store, or for a reply frame: refused per op with
/// the documented opaque `MemoryError` ("semantically rejected (e.g. an
/// oversize value)"), and the session survives. Every other
/// failure keeps its handling.
fn refuse_oversize(source: hatchling_memory::Error, what: &str) -> Result<MemoryResult, Error> {
    if matches!(
        source,
        hatchling_memory::Error::ContentTooLarge { .. }
            | hatchling_memory::Error::EncodedTooLarge { .. }
    ) {
        tracing::warn!(%source, "{what} refused: too large");
        return Ok(MemoryResult::MemoryError);
    }
    Err(Error::Memory { source })
}

/// One record's JSON, as a reply carries it.
fn encode<T: serde::Serialize>(record: &T) -> Result<String, hatchling_memory::Error> {
    serde_json::to_string(record).map_err(|_| hatchling_memory::Error::Corrupt)
}

/// One page of a multi-item reply, through the shared pager: records up
/// to [`PAGE_BUDGET_BYTES`], so the reply always fits a frame and the
/// oversize fallback is never the path.
fn paged(
    records: impl IntoIterator<Item = Result<(String, String), hatchling_memory::Error>>,
) -> Result<Page, Error> {
    page(records, PAGE_BUDGET_BYTES).map_err(|source| Error::Memory { source })
}

/// One page of a tag's items: they come ordered by id, the page resumes
/// past the caller's cursor, and each item's id is its cursor.
fn tag_page(items: Vec<MemoryItem>, after: Option<&MemoryId>) -> Result<MemoryResult, Error> {
    let Page { items_json, next } = paged(
        items
            .into_iter()
            .filter(|item| after.is_none_or(|after| item.id.as_str() > after.as_str()))
            .map(|item| Ok((encode(&item)?, item.id.to_string()))),
    )?;
    Ok(MemoryResult::Items { items_json, next })
}

/// One page of the context log: entries come in sequence order, the
/// page resumes past the caller's cursor, and each entry's sequence
/// number is its cursor.
fn context_page(entries: Vec<ContextEntry>, after: Option<u64>) -> Result<MemoryResult, Error> {
    let Page { items_json, next } = paged(
        entries
            .into_iter()
            .filter(|entry| after.is_none_or(|after| entry.seq > after))
            .map(|entry| Ok((encode(&entry)?, entry.seq.to_string()))),
    )?;
    Ok(MemoryResult::ContextEntries {
        entries_json: items_json,
        next,
    })
}

/// Parses a context-read cursor: a sequence number.
fn parse_seq(raw: &str) -> Result<u64, Error> {
    raw.parse::<u64>().map_err(|_| Error::SessionProtocol {
        reason: format!("invalid context cursor {raw:?}: expected a sequence number"),
    })
}

/// The action a [`MemoryOp`] performs: the unit of grant matching.
const fn memory_action(op: &MemoryOp) -> MemoryAction {
    match op {
        MemoryOp::Write { .. } => MemoryAction::Write,
        MemoryOp::Recall { .. } | MemoryOp::RecallByTag { .. } => MemoryAction::Recall,
        MemoryOp::Remove { .. } => MemoryAction::Remove,
        MemoryOp::ContextAppend { .. } => MemoryAction::ContextAppend,
        MemoryOp::ContextRead { .. } => MemoryAction::ContextRead,
        MemoryOp::Propose { .. } => MemoryAction::Propose,
    }
}

/// The scope string of an op, for grant matching.
fn op_scope_str(op: &MemoryOp) -> &str {
    match op {
        MemoryOp::Write { scope, .. }
        | MemoryOp::Recall { scope, .. }
        | MemoryOp::RecallByTag { scope, .. }
        | MemoryOp::Remove { scope, .. } => scope,
        MemoryOp::ContextAppend { .. } | MemoryOp::ContextRead { .. } => "session",
        MemoryOp::Propose { .. } => "user",
    }
}

/// Deny-by-default grant check: the op's (scope, action) must be
/// explicitly granted.
fn is_granted(config: &MemoryConfig, scope: &str, action: MemoryAction) -> bool {
    config
        .grants
        .iter()
        .any(|g| g.scope == scope && g.action == action)
}

/// Binds the agent's scope string through the broker-asserted
/// principal. `user` binds to the peer uid, `project` binds to
/// `uid-<peer uid>` (the broker asserts the identity; the agent names
/// no project), `session` binds to the broker-minted session id.
/// Anything else is refused.
pub(crate) fn bind_scope(scope: &str, principal: &MemoryPrincipal) -> Result<MemoryScope, String> {
    match scope {
        "user" => Ok(MemoryScope::User),
        "project" => {
            // The broker asserts the project identity from its own
            // config; the agent names no project. v0.1 uses the peer
            // uid as the project discriminator — a project cannot be
            // named by the agent, so cross-project reads by naming
            // are unrepresentable.
            let name: ProjectName = format!("uid-{}", principal.peer_uid)
                .parse()
                .map_err(|e: hatchling_memory::Error| e.to_string())?;
            Ok(MemoryScope::Project(name))
        }
        "session" => Ok(MemoryScope::Session),
        other => Err(format!(
            "unknown scope {other:?}, expected user, project, or session"
        )),
    }
}

/// Parses a retention string: `keep` or `ttl-secs:<n>`. There is no
/// default; the caller names it explicitly.
fn parse_retention(raw: &str) -> Result<RetentionPolicy, Error> {
    if raw == "keep" {
        return Ok(RetentionPolicy::Keep);
    }
    if let Some(secs) = raw.strip_prefix("ttl-secs:") {
        if let Ok(ttl_secs) = secs.parse::<u64>() {
            return Ok(RetentionPolicy::Ttl { ttl_secs });
        }
    }
    Err(Error::SessionProtocol {
        reason: format!("invalid retention {raw:?}: expected 'keep' or 'ttl-secs:<n>'"),
    })
}

fn parse_id(raw: &str) -> Result<MemoryId, Error> {
    raw.parse::<MemoryId>().map_err(|e| Error::SessionProtocol {
        reason: format!("invalid memory id: {e}"),
    })
}

fn parse_tag(raw: &str) -> Result<Tag, Error> {
    raw.parse::<Tag>().map_err(|e| Error::SessionProtocol {
        reason: format!("invalid memory tag: {e}"),
    })
}

fn parse_tags(raw: &[String]) -> Result<Vec<Tag>, Error> {
    raw.iter().map(|t| parse_tag(t)).collect()
}

#[cfg(test)]
mod paging_tests {
    use std::path::PathBuf;
    use std::str::FromStr as _;

    use hatchling_core::{MAX_FRAME_BYTES, MemoryOp, MemoryResult, SessionResponse};
    use hatchling_memory::{MemoryPrincipal, SessionId};

    use super::handle_memory_op;
    use crate::Error;
    use crate::config::{MemoryAction, MemoryConfig, MemoryGrant};

    fn config(name: &str) -> MemoryConfig {
        let root: PathBuf = std::env::temp_dir().join(format!(
            "hatchling-broker-paging-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let grant = |action| MemoryGrant {
            scope: String::from("user"),
            action,
        };
        MemoryConfig {
            root,
            grants: vec![
                grant(MemoryAction::Write),
                grant(MemoryAction::Recall),
                MemoryGrant {
                    scope: String::from("session"),
                    action: MemoryAction::ContextAppend,
                },
                MemoryGrant {
                    scope: String::from("session"),
                    action: MemoryAction::ContextRead,
                },
            ],
        }
    }

    fn principal() -> (MemoryPrincipal, SessionId) {
        let session = SessionId::from_str("sess-paging").unwrap();
        (
            MemoryPrincipal {
                peer_uid: 7,
                session_id: session.clone(),
            },
            session,
        )
    }

    /// Content whose item encodes just under the 56 KiB bound: quotes
    /// cost 4 bytes each once a reply carries them.
    fn near_bound() -> String {
        "\"".repeat(13_900)
    }

    fn write(config: &MemoryConfig, id: &str, content: &str) {
        let (p, s) = principal();
        let op = MemoryOp::Write {
            scope: String::from("user"),
            id: id.to_owned(),
            content: content.to_owned(),
            tags: vec![String::from("t")],
            retention: String::from("keep"),
        };
        assert_eq!(
            handle_memory_op(config, &op, &p, &s).unwrap(),
            MemoryResult::Stored,
            "{id}"
        );
    }

    /// Every page within one frame, never the oversize fallback, and the
    /// cursors chaining to `None`: returns (records, page count).
    fn walk(
        config: &MemoryConfig,
        op_for: impl Fn(Option<String>) -> MemoryOp,
    ) -> (Vec<String>, usize) {
        let (p, s) = principal();
        let mut records = Vec::new();
        let mut after = None;
        for pages in 1..=64 {
            let reply = handle_memory_op(config, &op_for(after.clone()), &p, &s).unwrap();
            let frame = serde_json::to_vec(&SessionResponse::Memory(reply.clone())).unwrap();
            assert!(
                frame.len() <= MAX_FRAME_BYTES,
                "page {pages}: {} bytes",
                frame.len()
            );
            let (json, next) = match reply {
                MemoryResult::Items { items_json, next } => (items_json, next),
                MemoryResult::ContextEntries { entries_json, next } => (entries_json, next),
                other @ (MemoryResult::Stored
                | MemoryResult::Item { .. }
                | MemoryResult::NotFound
                | MemoryResult::Removed { .. }
                | MemoryResult::ContextAppended
                | MemoryResult::Denied
                | MemoryResult::MemoryError
                | MemoryResult::Proposed
                | MemoryResult::Unasked { .. }) => {
                    unreachable!("page {pages}: not a page but {other:?}")
                }
            };
            assert!(!json.is_empty(), "a page must make progress");
            records.extend(json);
            let Some(next) = next else {
                return (records, pages);
            };
            after = Some(next);
        }
        unreachable!("the cursors never reached the end")
    }

    #[test]
    fn recall_by_tag_pages_adversarial_items_to_completion() {
        // Six items near the 56 KiB encoded bound (each alone past the
        // 48 KiB budget, so each travels alone) and four small ones:
        // together about six frames' worth.
        let config = config("tag");
        let mut ids = Vec::new();
        for n in 0..6 {
            let id = format!("big-{n}");
            write(&config, &id, &near_bound());
            ids.push(id);
        }
        for n in 0..4 {
            let id = format!("small-{n}");
            write(&config, &id, "a note");
            ids.push(id);
        }
        let (records, pages) = walk(&config, |after| MemoryOp::RecallByTag {
            scope: String::from("user"),
            tag: String::from("t"),
            after,
        });
        let got: Vec<String> = records
            .iter()
            .map(|json| {
                serde_json::from_str::<hatchling_memory::MemoryItem>(json)
                    .unwrap()
                    .id
                    .to_string()
            })
            .collect();
        ids.sort();
        assert_eq!(got, ids, "every item exactly once, in id order");
        assert!(
            pages >= 6,
            "six items past the budget need six pages, got {pages}"
        );
        let _ = std::fs::remove_dir_all(&config.root);
    }

    #[test]
    fn a_long_context_log_pages_to_completion() {
        let config = config("context");
        let (p, s) = principal();
        let big = near_bound();
        for n in 0..8 {
            let text = if n % 2 == 0 {
                big.clone()
            } else {
                format!("note {n}")
            };
            let op = MemoryOp::ContextAppend { text };
            assert_eq!(
                handle_memory_op(&config, &op, &p, &s).unwrap(),
                MemoryResult::ContextAppended
            );
        }
        let (records, pages) = walk(&config, |after| MemoryOp::ContextRead { after });
        let seqs: Vec<u64> = records
            .iter()
            .map(|json| {
                serde_json::from_str::<hatchling_memory::ContextEntry>(json)
                    .unwrap()
                    .seq
            })
            .collect();
        assert_eq!(
            seqs,
            (0..8).collect::<Vec<u64>>(),
            "every entry exactly once, in order"
        );
        assert!(
            pages >= 4,
            "four entries past the budget need four pages, got {pages}"
        );
        let _ = std::fs::remove_dir_all(&config.root);
    }

    #[test]
    fn a_small_result_is_one_page_and_the_cursor_is_exclusive() {
        let config = config("small");
        for id in ["a", "b", "c"] {
            write(&config, id, "tiny");
        }
        let (p, s) = principal();
        let op = |after: Option<&str>| MemoryOp::RecallByTag {
            scope: String::from("user"),
            tag: String::from("t"),
            after: after.map(str::to_owned),
        };
        let ids = |reply: MemoryResult| -> (Vec<String>, Option<String>) {
            let MemoryResult::Items { items_json, next } = reply else {
                unreachable!("not an items page: {reply:?}");
            };
            let ids = items_json
                .iter()
                .map(|json| {
                    serde_json::from_str::<hatchling_memory::MemoryItem>(json)
                        .unwrap()
                        .id
                        .to_string()
                })
                .collect();
            (ids, next)
        };
        let all = ids(handle_memory_op(&config, &op(None), &p, &s).unwrap());
        assert_eq!(
            all,
            (
                vec![String::from("a"), String::from("b"), String::from("c")],
                None
            )
        );
        let rest = ids(handle_memory_op(&config, &op(Some("a")), &p, &s).unwrap());
        assert_eq!(rest, (vec![String::from("b"), String::from("c")], None));
        let none = ids(handle_memory_op(&config, &op(Some("c")), &p, &s).unwrap());
        assert_eq!(none, (vec![], None));
        let ctx = MemoryOp::ContextAppend {
            text: String::from("one"),
        };
        handle_memory_op(&config, &ctx, &p, &s).unwrap();
        let after_first = MemoryOp::ContextRead {
            after: Some(String::from("0")),
        };
        assert_eq!(
            handle_memory_op(&config, &after_first, &p, &s).unwrap(),
            MemoryResult::ContextEntries {
                entries_json: vec![],
                next: None
            }
        );
        let _ = std::fs::remove_dir_all(&config.root);
    }

    #[test]
    fn a_malformed_cursor_ends_the_session() {
        let config = config("bad-cursor");
        let (p, s) = principal();
        let bad_id = MemoryOp::RecallByTag {
            scope: String::from("user"),
            tag: String::from("t"),
            after: Some(String::from("../x")),
        };
        assert!(matches!(
            handle_memory_op(&config, &bad_id, &p, &s),
            Err(Error::SessionProtocol { .. })
        ));
        let bad_seq = MemoryOp::ContextRead {
            after: Some(String::from("-1")),
        };
        assert!(matches!(
            handle_memory_op(&config, &bad_seq, &p, &s),
            Err(Error::SessionProtocol { .. })
        ));
        let _ = std::fs::remove_dir_all(&config.root);
    }
}
