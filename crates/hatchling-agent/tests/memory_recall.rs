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
        run_earlier: Vec::new(),
        run_now: None,
        run_records: Vec::new(),
        run_builders: std::collections::BTreeMap::new(),
        run_tools: None,
        run_deadline: None,
        run_token_budget: None,
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
        | Message::Evidence(_)
        | Message::Note(_) => None,
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

/// A run that finds a recalled memory contradicted proposes the correction
/// by that memory's id (WO-048). The recalled block names each item's id;
/// a proposal that supersedes one the owner approved is put to them, and
/// one naming an id nothing is recalled with isn't. A scripted model stands
/// in for the model: WO-045 measures how often real ones do this.
#[test]
fn a_contradicted_memory_is_corrected_by_the_id_it_is_recalled_with() {
    let root = memory_root("contradiction");
    let broker = FixtureBroker::spawn(
        "memory-contradiction",
        BrokerBehavior::Memory {
            root: root.to_path_buf(),
            grants: vec![
                grant("user", MemoryAction::Recall),
                grant("project", MemoryAction::Recall),
                grant("user", MemoryAction::Propose),
            ],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    // A memory the owner approved, kept as the broker keeps one.
    let principal = hatchling_memory::MemoryPrincipal {
        peer_uid: rustix::process::getuid().as_raw(),
        session_id: hatchling_memory::SessionId::generate().unwrap(),
    };
    hatchling_broker::memory_sealed::open_published(&root.to_path_buf())
        .unwrap()
        .keep(
            &principal,
            &hatchling_memory::MemoryScope::User,
            &"kept-plan".parse().unwrap(),
            "The plan is in docs/plan.md.",
            &[RECALL_TAG.parse().unwrap()],
            hatchling_memory::KeptRecord {
                kind: Some(hatchling_memory::MemoryKind::Pointer),
                ..hatchling_memory::KeptRecord::agent_words(Some(String::from("confirm-1")), None, None, 1)
            },
        )
        .unwrap();
    let correction = |supersedes: &str| hatchling_loop::ToolCall {
        name: String::from("memory_propose"),
        arguments: serde_json::json!({
            "content": "The plan is in notes/plan.md.",
            "kind": "pointer",
            "supersedes": supersedes,
        }),
    };
    // The file the memory points to isn't there; the run proposes the
    // correction, first by an id nothing is recalled with, then by its own.
    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![correction("kept-none")]),
        ModelAction::ToolCalls(vec![correction("kept-plan")]),
        ModelAction::Text(String::from("The plan moved to notes/plan.md; I asked you to update what I remember.")),
    ]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("open the plan"),
            LoopBudget { max_steps: 6 },
        )
        .unwrap();
    let block = recalled_block(&result.history).expect("the memory is recalled");
    assert!(block.contains("; id kept-plan):\nThe plan is in docs/plan.md.\n"), "{block}");
    let said: Vec<String> = result
        .history
        .iter()
        .filter_map(|message| match message {
            Message::ToolUse { results, .. } => Some(format!("{results:?}")),
            Message::System(_)
            | Message::User(_)
            | Message::Assistant(_)
            | Message::RecalledMemory(_)
            | Message::Evidence(_)
            | Message::Note(_) => None,
        })
        .collect();
    assert_eq!(said.len(), 2, "{said:?}");
    let first = said.first().unwrap();
    assert!(first.contains("\"asked\": Bool(false)"), "{said:?}");
    assert!(first.contains("no memory the owner approved is recalled with the id kept-none"), "{said:?}");
    assert!(said.last().unwrap().contains("\"asked\": Bool(true)"), "put to the owner: {said:?}");
}

