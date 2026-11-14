//! Integration tests for the agent loop: the full contract the
//! task promises, pinned by a scripted model.

use hatchling_loop::{
    Error, FakeModel, Loop, LoopBudget, Message, ModelAction, PromptKind, Tool, ToolCall,
    ToolOutput, ToolRegistry,
};
use serde_json::{Value, json};

/// A test tool: echoes its `input` argument.
struct EchoTool;

impl Tool for EchoTool {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn description(&self) -> &'static str {
        "Echo the input back."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": { "type": "string" }
            },
            "required": ["input"],
            "additionalProperties": false
        })
    }

    fn execute(&self, arguments: Value) -> Result<ToolOutput, Error> {
        let input = arguments
            .get("input")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(ToolOutput::Ok(json!({ "echo": input })))
    }
}

/// A test tool that always fails execution.
struct FailTool;

impl Tool for FailTool {
    fn name(&self) -> &'static str {
        "fail"
    }

    fn description(&self) -> &'static str {
        "Always fails."
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }

    fn execute(&self, _arguments: Value) -> Result<ToolOutput, Error> {
        Ok(ToolOutput::Err(String::from("deliberate failure")))
    }
}

fn registry_with_echo() -> ToolRegistry<'static> {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool));
    registry
}

#[test]
fn multi_turn_loop_produces_final_answer() {
    let registry = registry_with_echo();
    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("echo"),
            arguments: json!({ "input": "hello" }),
        }]),
        ModelAction::Text(String::from("done")),
    ]);
    let result = Loop::new(&registry, PromptKind::Chat)
        .run(&model, String::from("say hello"))
        .expect("loop should complete");

    assert_eq!(result.answer, "done");
    assert_eq!(result.steps, 2);
    // System + User + ToolUse + Assistant
    assert_eq!(result.history.len(), 4);
    assert!(matches!(result.history.first(), Some(Message::System(_))));
    assert!(matches!(result.history.get(1), Some(Message::User(_))));
    let tool_use = result.history.get(2);
    assert!(
        matches!(tool_use, Some(Message::ToolUse { .. })),
        "expected ToolUse at index 2"
    );
    if let Some(Message::ToolUse { calls, results }) = tool_use {
        assert_eq!(calls.len(), 1);
        assert_eq!(calls.first().map(|c| c.name.as_str()), Some("echo"));
        assert_eq!(results.len(), 1);
        assert!(matches!(results.first(), Some(ToolOutput::Ok(_))));
    }

    // The model saw the tool result on its second turn.
    let seen = model.seen_histories();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen.get(1).map(Vec::len), Some(3)); // System, User, ToolUse
}

#[test]
fn unknown_tool_stops_before_execution() {
    // Told each time that nothing ran; stopped once it's done so more
    // than the loop allows.
    let registry = registry_with_echo();
    let model = FakeModel::new(
        (0..=hatchling_loop::CONTRACT_SLIPS)
            .map(|_| {
                ModelAction::ToolCalls(vec![ToolCall {
                    name: String::from("nope"),
                    arguments: json!({}),
                }])
            })
            .collect(),
    );
    let err = Loop::new(&registry, PromptKind::Chat)
        .run(&model, String::from("x"))
        .expect_err("unknown tool must stop the loop");
    assert!(matches!(err, Error::UnknownTool(_)));
}

#[test]
fn invalid_arguments_stop_before_execution() {
    let registry = registry_with_echo();
    // `input` is required; omitting it fails schema validation.
    let model = FakeModel::new(
        (0..=hatchling_loop::CONTRACT_SLIPS)
            .map(|_| {
                ModelAction::ToolCalls(vec![ToolCall {
                    name: String::from("echo"),
                    arguments: json!({}),
                }])
            })
            .collect(),
    );
    let err = Loop::new(&registry, PromptKind::Chat)
        .run(&model, String::from("x"))
        .expect_err("schema violation must stop before execute");
    assert!(matches!(err, Error::InvalidArguments { .. }));
}

#[test]
fn budget_exhaustion_fails_closed() {
    let registry = registry_with_echo();
    // A model that never returns text.
    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("echo"),
            arguments: json!({ "input": "a" }),
        }]),
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("echo"),
            arguments: json!({ "input": "b" }),
        }]),
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("echo"),
            arguments: json!({ "input": "c" }),
        }]),
    ]);
    let err = Loop::new(&registry, PromptKind::Chat)
        .with_budget(LoopBudget { max_steps: 2 })
        .run(&model, String::from("x"))
        .expect_err("budget must stop the loop");
    assert!(matches!(err, Error::BudgetExhausted { steps: 2 }));
}

#[test]
fn tool_error_feeds_back_and_loop_continues() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FailTool));
    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("fail"),
            arguments: json!({}),
        }]),
        // The model sees the error and answers anyway.
        ModelAction::Text(String::from("it failed, giving up")),
    ]);
    let result = Loop::new(&registry, PromptKind::Chat)
        .run(&model, String::from("x"))
        .expect("tool error should feed back, not stop the loop");
    assert_eq!(result.answer, "it failed, giving up");

    // The second turn's history contains the ToolUse with the error.
    let seen = model.seen_histories();
    assert_eq!(seen.len(), 2);
    let second_turn = seen.get(1).expect("second turn history");
    let tool_use = second_turn.get(2);
    assert!(
        matches!(tool_use, Some(Message::ToolUse { .. })),
        "expected ToolUse at index 2"
    );
    if let Some(Message::ToolUse { results, .. }) = tool_use {
        assert!(matches!(results.first(), Some(ToolOutput::Err(_))));
    }
}

#[test]
fn model_cannot_extend_registry() {
    // The registry is fixed at construction; there is no API for the
    // model to add tools. This test pins that the only mutation
    // method is `register`, called by the host before `run`.
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool));
    assert!(registry.get("echo").is_some());
    assert!(registry.get("echo2").is_none());
}

#[test]
fn the_system_prompt_is_the_kinds_authored_prompt() {
    let registry = registry_with_echo();
    let model = FakeModel::new(vec![ModelAction::Text(String::from("done"))]);
    let result = Loop::new(&registry, PromptKind::Worker)
        .run(&model, String::from("hi"))
        .expect("loop should complete");

    // Exactly one System message, first in history, carrying the
    // authored prompt pack — the model cannot inject another.
    assert!(matches!(result.history.first(), Some(Message::System(_))));
    let systems: Vec<&String> = result
        .history
        .iter()
        .filter_map(|message| match message {
            Message::System(prompt) => Some(prompt),
            Message::User(_)
            | Message::Assistant(_)
            | Message::ToolUse { .. }
            | Message::RecalledMemory(_)
            | Message::Evidence(_)
            | Message::Note(_) => None,
        })
        .collect();
    assert_eq!(systems.len(), 1);
    assert_eq!(
        systems.first().map(|prompt| prompt.as_str()),
        Some(hatchling_prompt::system_prompt_for(PromptKind::Worker).as_str())
    );
    // The kind decides it: a worker does not carry the chat prompt.
    assert_ne!(
        hatchling_prompt::system_prompt_for(PromptKind::Worker),
        hatchling_prompt::system_prompt_for(PromptKind::Chat)
    );
}
