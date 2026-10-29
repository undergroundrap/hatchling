//! Integration tests: the agent tool-calling loop against the real broker.
//!
//! These drive [`Cell<Ready>`]'s `run_loop` through the fixture broker's
//! true pipeline (kernel peer gate, grant mediation, session loop) with a
//! scripted [`FakeModel`]. They pin the task's promise: the built-in
//! tools execute through the broker session, and denied grants / denied
//! destinations surface as tool errors fed back to the model — never as
//! loop crashes.

pub(crate) mod common;

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid, Ready, VaultAction};
use hatchling_loop::{FakeModel, LoopBudget, Message, ModelAction, ToolCall, ToolOutput};
use serde_json::json;

fn grant(
    action: VaultAction,
    name: &str,
) -> Result<hatchling_broker::VaultGrant, hatchling_core::Error> {
    Ok(hatchling_broker::VaultGrant {
        action,
        name: name.parse()?,
    })
}

/// A `Ready` cell against the fixture broker's socket.
fn ready_cell(broker: &FixtureBroker) -> Result<Cell<Ready>, Error> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-loop".parse()?,
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
        vault_fd: None,
    };
    Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()
}

/// The tool results at `messages[index]`, if that slot is a `ToolUse`.
/// Returns `None` (rather than panicking) because helpers may not
/// panic under the workspace lint policy; the `#[test]` fns assert on
/// the result.
fn tool_results(messages: &[Message], index: usize) -> Option<&Vec<ToolOutput>> {
    if let Some(Message::ToolUse { results, .. }) = messages.get(index) {
        Some(results)
    } else {
        None
    }
}

#[test]
fn loop_vault_fetch_success_and_denial_feed_back() {
    let broker = FixtureBroker::spawn(
        "loop-vault",
        BrokerBehavior::Vault(vec![
            grant(VaultAction::Store, "api-key").unwrap(),
            grant(VaultAction::Fetch, "api-key").unwrap(),
        ]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    cell.vault_store("api-key", b"super-secret").unwrap();

    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("vault_fetch"),
            arguments: json!({ "name": "api-key" }),
        }]),
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("vault_fetch"),
            arguments: json!({ "name": "other-key" }),
        }]),
        ModelAction::Text(String::from("one worked, one denied")),
    ]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("fetch two secrets"),
            LoopBudget { max_steps: 10 },
        )
        .unwrap();
    assert_eq!(result.answer, "one worked, one denied");
    assert_eq!(result.steps, 3);

    // Turn 2's history: the granted fetch succeeded.
    let seen = model.seen_histories();
    assert_eq!(seen.len(), 3);
    let turn2 = seen.get(1).expect("model should have a second turn");
    let results2 = tool_results(turn2, 2).expect("expected ToolUse at index 2");
    assert!(
        matches!(results2.first(), Some(ToolOutput::Ok(_))),
        "granted fetch must succeed, got {results2:?}"
    );
    // Turn 3's history: the denied fetch fed back as a tool error, not
    // a loop crash.
    let turn3 = seen.get(2).expect("model should have a third turn");
    let results3 = tool_results(turn3, 3).expect("expected ToolUse at index 3");
    assert!(
        matches!(results3.first(), Some(ToolOutput::Err(_))),
        "denied fetch must feed back as an error, got {results3:?}"
    );
}

#[test]
fn loop_egress_denied_destination_feeds_back() {
    let broker = FixtureBroker::spawn(
        "loop-egress",
        BrokerBehavior::Egress {
            allowlist: "example.com:443".to_string(),
            grants: vec![],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("egress_request"),
            arguments: json!({
                "surrogate": "bogus",
                "method": "GET",
                "destination": "evil.example:443",
                "path": "/",
            }),
        }]),
        ModelAction::Text(String::from("denied as expected")),
    ]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("fetch evil"),
            LoopBudget { max_steps: 10 },
        )
        .unwrap();
    assert_eq!(result.answer, "denied as expected");

    // The denied destination fed back as a tool error; the loop
    // continued to the model's final answer.
    let seen = model.seen_histories();
    assert_eq!(seen.len(), 2);
    let turn2 = seen.get(1).expect("model should have a second turn");
    let results = tool_results(turn2, 2).expect("expected ToolUse at index 2");
    assert!(
        matches!(results.first(), Some(ToolOutput::Err(_))),
        "denied destination must feed back as an error, got {results:?}"
    );
}

#[test]
fn loop_unknown_tool_from_model_stops_fail_closed() {
    // The loop-level security promise through the agent's public API:
    // an unknown tool name from the model stops before execution,
    // even when the registry is the agent's real built-in set.
    let broker = FixtureBroker::spawn(
        "loop-unknown-tool",
        BrokerBehavior::Vault(vec![grant(VaultAction::Fetch, "api-key").unwrap()]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    let model = FakeModel::new(vec![ModelAction::ToolCalls(vec![ToolCall {
        name: String::from("vault_fetch_privileged"),
        arguments: json!({ "name": "api-key" }),
    }])]);
    let err = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("do something privileged"),
            LoopBudget { max_steps: 10 },
        )
        .expect_err("unknown tool must stop the loop");
    assert!(
        matches!(err, hatchling_loop::Error::UnknownTool(_)),
        "expected UnknownTool, got {err:?}"
    );
}
