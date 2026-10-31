//! Component tests: the agent's memory API against the real broker.
//!
//! The [`Memory`](common::BrokerBehavior::Memory) fixture runs the true
//! broker pipeline ([`serve_connection`](hatchling_broker::serve_connection)):
//! peer gate, the deny-by-default memory grant check, scope binding, and
//! the store on disk. These tests drive it only through
//! `hatchling_agent::memory`, so the agent client and the broker's
//! `memory_ops` are pinned together.

pub(crate) mod common;

use std::path::PathBuf;

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::memory::{self, MemoryScope};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid, Ready};
use hatchling_broker::{MemoryAction, MemoryGrant};

/// A `Ready` cell against the fixture broker's socket.
fn ready_cell(broker: &FixtureBroker) -> Result<Cell<Ready>, Error> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-memory".parse()?,
        broker: hatchling_agent::BrokerAddress::Socket(broker.socket_path.parse()?),
        broker_uid: PeerUid::from_kernel(uid),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        run_subagent: false,
        run_chat: false,
        run_history: Vec::new(),
        run_builders: std::collections::BTreeMap::new(),
        vault_fd: None,
    };
    Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()
}

fn grant(scope: &str, action: MemoryAction) -> MemoryGrant {
    MemoryGrant {
        scope: scope.to_owned(),
        action,
    }
}

/// A memory root unique to this test process and test; the broker
/// creates it on first use.
fn memory_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hlfx-memory-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    root
}

fn denied_as<T>(result: Result<T, Error>, action: &str, scope: &str) -> bool {
    matches!(result, Err(Error::MemoryDenied { action: a, scope: s }) if a == action && s == scope)
}

#[test]
fn store_recall_tag_and_remove_through_real_broker() {
    let root = memory_root("roundtrip");
    let broker = FixtureBroker::spawn(
        "memory-roundtrip",
        BrokerBehavior::Memory {
            root: root.clone(),
            grants: vec![
                grant("user", MemoryAction::Write),
                grant("user", MemoryAction::Recall),
                grant("user", MemoryAction::Remove),
            ],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();

    memory::store(
        stream,
        MemoryScope::User,
        "note-1",
        "remember the milk",
        &["errand"],
        "keep",
    )
    .unwrap();
    let item = memory::recall(stream, MemoryScope::User, "note-1")
        .unwrap()
        .expect("the stored item");
    assert!(item.contains("remember the milk"), "{item}");
    let tagged = memory::recall_by_tag(stream, MemoryScope::User, "errand").unwrap();
    assert_eq!(tagged.len(), 1, "{tagged:?}");
    assert!(
        tagged
            .first()
            .is_some_and(|i| i.contains("remember the milk")),
        "{tagged:?}"
    );
    assert!(memory::remove(stream, MemoryScope::User, "note-1").unwrap());
    assert!(!memory::remove(stream, MemoryScope::User, "note-1").unwrap());
    assert_eq!(
        memory::recall(stream, MemoryScope::User, "note-1").unwrap(),
        None
    );

    drop(cell);
    drop(broker);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn project_scope_and_session_context_through_real_broker() {
    let root = memory_root("project-context");
    let broker = FixtureBroker::spawn(
        "memory-project-context",
        BrokerBehavior::Memory {
            root: root.clone(),
            grants: vec![
                grant("project", MemoryAction::Write),
                grant("project", MemoryAction::Recall),
                grant("session", MemoryAction::ContextAppend),
                grant("session", MemoryAction::ContextRead),
            ],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();

    memory::store(
        stream,
        MemoryScope::Project,
        "fact",
        "alpha uses rust",
        &[],
        "ttl-secs:3600",
    )
    .unwrap();
    let fact = memory::recall(stream, MemoryScope::Project, "fact").unwrap();
    assert!(
        fact.as_deref()
            .is_some_and(|i| i.contains("alpha uses rust")),
        "{fact:?}"
    );

    memory::context_append(stream, "{\"note\":\"first\"}").unwrap();
    memory::context_append(stream, "{\"note\":\"second\"}").unwrap();
    let entries = memory::context_read(stream).unwrap();
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(
        entries.first().is_some_and(|e| e.contains("first"))
            && entries.get(1).is_some_and(|e| e.contains("second")),
        "{entries:?}"
    );

    drop(cell);
    drop(broker);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn every_memory_op_is_denied_without_a_grant() {
    let root = memory_root("denied");
    let broker = FixtureBroker::spawn(
        "memory-denied",
        BrokerBehavior::Memory {
            root: root.clone(),
            grants: Vec::new(),
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();

    assert!(denied_as(
        memory::store(stream, MemoryScope::User, "n", "c", &[], "keep"),
        "write",
        "user"
    ));
    assert!(denied_as(
        memory::recall(stream, MemoryScope::User, "n"),
        "recall",
        "user"
    ));
    assert!(denied_as(
        memory::recall_by_tag(stream, MemoryScope::User, "t"),
        "recall-by-tag",
        "user"
    ));
    assert!(denied_as(
        memory::remove(stream, MemoryScope::User, "n"),
        "remove",
        "user"
    ));
    assert!(denied_as(
        memory::context_append(stream, "{}"),
        "context-append",
        "session"
    ));
    assert!(denied_as(
        memory::context_read(stream),
        "context-read",
        "session"
    ));
    // Denied before the store is touched: the root was never created.
    let touched = root.exists();

    drop(cell);
    drop(broker);
    let _ = std::fs::remove_dir_all(&root);
    assert!(!touched, "a denied op must not touch the store");
}

#[test]
fn grants_match_both_scope_and_action() {
    let root = memory_root("scope-action");
    let broker = FixtureBroker::spawn(
        "memory-scope-action",
        BrokerBehavior::Memory {
            root: root.clone(),
            grants: vec![
                grant("user", MemoryAction::Recall),
                grant("project", MemoryAction::Write),
            ],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();

    // user:recall does not grant user:write; project:write does not
    // grant user:write.
    assert!(denied_as(
        memory::store(stream, MemoryScope::User, "n", "c", &[], "keep"),
        "write",
        "user"
    ));
    memory::store(stream, MemoryScope::Project, "n", "c", &[], "keep").unwrap();
    assert_eq!(
        memory::recall(stream, MemoryScope::User, "n").unwrap(),
        None
    );

    drop(cell);
    drop(broker);
    let _ = std::fs::remove_dir_all(&root);
}

/// A ready session against the real broker with user write and recall,
/// with bounded replies: a broker that stops answering fails the test
/// instead of hanging it.
#[allow(
    clippy::unwrap_used,
    reason = "fixture setup: a failed step must fail the calling test loudly"
)]
fn bounded_memory_session(name: &str) -> (FixtureBroker, Cell<Ready>, PathBuf) {
    let root = memory_root(name);
    let broker = FixtureBroker::spawn(
        name,
        BrokerBehavior::Memory {
            root: root.clone(),
            grants: vec![
                grant("user", MemoryAction::Write),
                grant("user", MemoryAction::Recall),
            ],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    cell.broker()
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    (broker, cell, root)
}

/// The session is still up: a small write and its recall both work.
#[allow(
    clippy::unwrap_used,
    reason = "fixture setup: a failed step must fail the calling test loudly"
)]
fn session_survives(stream: &std::os::unix::net::UnixStream) {
    memory::store(
        stream,
        MemoryScope::User,
        "after",
        "still here",
        &[],
        "keep",
    )
    .unwrap();
    assert!(
        memory::recall(stream, MemoryScope::User, "after")
            .unwrap()
            .is_some()
    );
}

/// Content of exactly `MAX_CONTENT_BYTES`: quotes, backslashes, control
/// characters, and a two-byte character, repeated.
fn mixed_worst_case() -> String {
    let unit = "\"\\\u{1}é";
    let mut content = unit.repeat(hatchling_memory::MAX_CONTENT_BYTES / unit.len());
    while content.len() < hatchling_memory::MAX_CONTENT_BYTES {
        content.push('"');
    }
    content
}

#[test]
fn content_bound_adversarial_writes_fail_per_op_and_the_session_survives() {
    // all-quote, all-control, and mixed worst-case content at
    // exactly MAX_CONTENT_BYTES. Each request would exceed the frame, so
    // the agent refuses it before sending — a typed per-op error — and
    // the same session goes on working.
    let (_broker, cell, root) = bounded_memory_session("w47-bound");
    let stream = cell.broker();
    for (name, content) in [
        (
            "all-quote",
            "\"".repeat(hatchling_memory::MAX_CONTENT_BYTES),
        ),
        (
            "all-control",
            "\u{1}".repeat(hatchling_memory::MAX_CONTENT_BYTES),
        ),
        ("mixed", mixed_worst_case()),
    ] {
        assert_eq!(content.len(), hatchling_memory::MAX_CONTENT_BYTES, "{name}");
        let result = memory::store(stream, MemoryScope::User, name, &content, &[], "keep");
        assert!(
            matches!(
                result,
                Err(Error::MemoryRequestTooLarge { len, max })
                    if len > max && max == hatchling_core::MAX_FRAME_BYTES
            ),
            "{name}: {result:?}"
        );
        session_survives(stream);
        assert!(
            memory::recall(stream, MemoryScope::User, name)
                .unwrap()
                .is_none()
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn content_that_fits_the_request_but_not_a_reply_is_refused_by_the_broker() {
    // 15 KiB of quotes: the request (2x) fits a frame, but the item in a
    // recall reply (4x) would not. The broker refuses the write per op,
    // stores nothing, and the session survives.
    let (_broker, cell, root) = bounded_memory_session("w47-broker");
    let stream = cell.broker();
    let content = "\"".repeat(15 * 1024);
    let result = memory::store(stream, MemoryScope::User, "big", &content, &[], "keep");
    assert!(matches!(result, Err(Error::MemoryFailed)), "{result:?}");
    session_survives(stream);
    assert!(
        memory::recall(stream, MemoryScope::User, "big")
            .unwrap()
            .is_none()
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn quote_heavy_content_the_store_accepts_round_trips() {
    // 13 KiB of quotes sits under the encoded bound: stored, and
    // recalled byte for byte through the real session.
    let (_broker, cell, root) = bounded_memory_session("w47-accept");
    let stream = cell.broker();
    let content = "\"".repeat(13 * 1024);
    memory::store(stream, MemoryScope::User, "quotes", &content, &[], "keep").unwrap();
    let item_json = memory::recall(stream, MemoryScope::User, "quotes")
        .unwrap()
        .expect("stored");
    let item: hatchling_memory::MemoryItem = serde_json::from_str(&item_json).unwrap();
    assert_eq!(item.content, content);
    session_survives(stream);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn recall_by_tag_returns_every_item_across_pages() {
    // items that together far exceed one frame — six near the
    // 56 KiB encoded bound, each alone past the 48 KiB page budget, and
    // three small ones — all come back through the real session, content
    // intact. Before paging this op failed per op ; an oversize
    // fallback here would surface as `MemoryFailed`, so success also
    // proves the pager never hit it.
    let (_broker, cell, root) = bounded_memory_session("w51-tag");
    let stream = cell.broker();
    let big = "\"".repeat(13_900);
    let mut expected = Vec::new();
    for n in 0..6 {
        let id = format!("big-{n}");
        memory::store(stream, MemoryScope::User, &id, &big, &["t"], "keep").unwrap();
        expected.push((id, big.clone()));
    }
    for n in 0..3 {
        let id = format!("small-{n}");
        memory::store(stream, MemoryScope::User, &id, "a note", &["t"], "keep").unwrap();
        expected.push((id, String::from("a note")));
    }
    expected.sort();
    let got: Vec<(String, String)> = memory::recall_by_tag(stream, MemoryScope::User, "t")
        .unwrap()
        .iter()
        .map(|json| {
            let item: hatchling_memory::MemoryItem = serde_json::from_str(json).unwrap();
            (item.id.to_string(), item.content)
        })
        .collect();
    assert_eq!(got, expected);
    session_survives(stream);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn context_read_returns_a_long_log_across_pages() {
    let root = memory_root("w51-context");
    let broker = FixtureBroker::spawn(
        "w51-context",
        BrokerBehavior::Memory {
            root: root.clone(),
            grants: vec![
                grant("session", MemoryAction::ContextAppend),
                grant("session", MemoryAction::ContextRead),
            ],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let big = "\"".repeat(13_900);
    let texts: Vec<String> = (0..8)
        .map(|n| {
            if n % 2 == 0 {
                big.clone()
            } else {
                format!("note {n}")
            }
        })
        .collect();
    for text in &texts {
        memory::context_append(stream, text).unwrap();
    }
    let got: Vec<String> = memory::context_read(stream)
        .unwrap()
        .iter()
        .map(|json| {
            serde_json::from_str::<hatchling_memory::ContextEntry>(json)
                .unwrap()
                .payload_json
        })
        .collect();
    assert_eq!(got, texts, "every entry, in order, content intact");
    let _ = std::fs::remove_dir_all(&root);
}
