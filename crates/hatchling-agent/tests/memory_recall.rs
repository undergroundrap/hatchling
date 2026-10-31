//! Round-trip test: broker write -> production provider -> loop prompt.
//!
//! This pins 's acceptance: an item written through the real
//! broker pipeline is recalled by [`BrokerMemoryProvider`] (not a
//! mock) at loop start and appears in the assembled prompt. The
//! fixture broker runs the true pipeline (peer gate, grant mediation,
//! scope binding, store on disk); the only test doubles are the
//! scripted model and the throwaway socket/store paths.

pub(crate) mod common;

use common::{BrokerBehavior, FixtureBroker, Scratch};
use hatchling_agent::memory::{self, MemoryScope};
use hatchling_agent::memory_provider::{BrokerMemoryProvider, RECALL_TAG};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid, Ready};
use hatchling_broker::{MemoryAction, MemoryGrant};
use hatchling_loop::{FakeModel, LoopBudget, MemoryProvider, Message, ModelAction};

/// A `Ready` cell against the fixture broker's socket.
fn ready_cell(broker: &FixtureBroker) -> Result<Cell<Ready>, Error> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-recall".parse()?,
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
/// creates it on first use, and the guard removes it when the test
/// ends.
fn memory_root(name: &str) -> Scratch {
    Scratch::new(std::env::temp_dir().join(format!("hlfx-recall-{}-{name}", std::process::id())))
}

/// The recalled-memory block in `history`, if the loop injected one.
fn recalled_block(history: &[Message]) -> Option<&str> {
    history.iter().find_map(|message| match message {
        Message::RecalledMemory(block) => Some(block.as_str()),
        Message::System(_)
        | Message::User(_)
        | Message::Assistant(_)
        | Message::ToolUse { .. }
        | Message::Evidence(_) => None,
    })
}

#[test]
fn broker_write_to_loop_prompt_round_trip() {
    // The production recall policy needs both scopes granted; a
    // missing grant fails the recall closed (no memory, no crash).
    let root = memory_root("roundtrip");
    let broker = FixtureBroker::spawn(
        "memory-recall-roundtrip",
        BrokerBehavior::Memory {
            root: root.to_path_buf(),
            grants: vec![
                grant("user", MemoryAction::Write),
                grant("user", MemoryAction::Recall),
                grant("project", MemoryAction::Write),
                grant("project", MemoryAction::Recall),
            ],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();

    // Two tagged items (one per scope) and one untagged decoy.
    memory::store(
        stream,
        MemoryScope::User,
        "user-fact",
        "the user prefers tea over coffee",
        &[RECALL_TAG],
        "keep",
    )
    .unwrap();
    memory::store(
        stream,
        MemoryScope::Project,
        "project-fact",
        "this project pins Rust 1.85",
        &[RECALL_TAG],
        "keep",
    )
    .unwrap();
    memory::store(
        stream,
        MemoryScope::User,
        "untagged",
        "this must not reach the prompt",
        &["errand"],
        "keep",
    )
    .unwrap();

    // The production provider, not a mock: it speaks the broker
    // protocol over the real session.
    let provider = BrokerMemoryProvider::new(stream);
    let recalled = provider.recall().expect("recall should succeed");
    assert_eq!(recalled.len(), 2, "both tagged items, got {recalled:?}");
    let contents: Vec<&str> = recalled.iter().map(|item| item.content.as_str()).collect();
    assert!(
        contents.contains(&"the user prefers tea over coffee"),
        "{contents:?}"
    );
    assert!(
        contents.contains(&"this project pins Rust 1.85"),
        "{contents:?}"
    );

    // Through the loop path: the provider is wired into `run_loop`
    // by the cell, so this exercises write -> recall -> prompt.
    let model = FakeModel::new(vec![ModelAction::Text(String::from("done"))]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("hello"),
            LoopBudget { max_steps: 5 },
        )
        .unwrap();
    assert_eq!(result.answer, "done");

    let block = recalled_block(&result.history).expect("loop should inject recalled memory");
    assert!(
        block.contains("the user prefers tea over coffee"),
        "{block}"
    );
    assert!(block.contains("this project pins Rust 1.85"), "{block}");
    assert!(
        !block.contains("this must not reach the prompt"),
        "untagged items are never recalled: {block}"
    );
}

#[test]
fn recall_denied_fails_closed_without_blocking() {
    // No recall grants at all: the provider fails closed, the loop
    // still completes, and no memory block is injected.
    let root = memory_root("denied");
    let broker = FixtureBroker::spawn(
        "memory-recall-denied",
        BrokerBehavior::Memory {
            root: root.to_path_buf(),
            grants: vec![grant("user", MemoryAction::Write)],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let stream = cell.broker();

    memory::store(
        stream,
        MemoryScope::User,
        "user-fact",
        "the user prefers tea",
        &[RECALL_TAG],
        "keep",
    )
    .unwrap();

    let provider = BrokerMemoryProvider::new(stream);
    assert!(provider.recall().is_err(), "denied recall must fail closed");

    let model = FakeModel::new(vec![ModelAction::Text(String::from("done"))]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("hello"),
            LoopBudget { max_steps: 5 },
        )
        .unwrap();
    assert_eq!(result.answer, "done");
    assert!(
        recalled_block(&result.history).is_none(),
        "no memory block on failed recall"
    );
}
