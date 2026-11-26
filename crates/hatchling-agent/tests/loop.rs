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
use hatchling_core::run_tools::ToolGroup;
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
    ready_cell_offering(broker, None)
}

/// [`ready_cell`], told the tool groups `run_tools` are on.
fn ready_cell_offering(
    broker: &FixtureBroker,
    run_tools: Option<std::collections::BTreeSet<ToolGroup>>,
) -> Result<Cell<Ready>, Error> {
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
        run_earlier: Vec::new(),
        run_now: None,
        run_builders: std::collections::BTreeMap::new(),
        run_tools,
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
fn a_model_has_no_tool_that_reads_a_secret() {
    let broker = FixtureBroker::spawn(
        "loop-no-fetch",
        BrokerBehavior::Vault(vec![grant(VaultAction::Store, "api-key").unwrap()]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    cell.vault_store("api-key", b"super-secret").unwrap();
    let model = FakeModel::new(
        (0..=hatchling_loop::CONTRACT_SLIPS)
            .map(|_| {
                ModelAction::ToolCalls(vec![ToolCall {
                    name: String::from("vault_fetch"),
                    arguments: json!({ "name": "api-key" }),
                }])
            })
            .collect(),
    );
    let err = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("read the api key"),
            LoopBudget { max_steps: 10 },
        )
        .expect_err("there is no such tool");
    assert!(
        matches!(err, hatchling_loop::Error::UnknownTool(_)),
        "expected UnknownTool, got {err:?}"
    );
    let seen = format!("{:?}", model.seen_histories());
    assert!(!seen.contains("super-secret"), "{seen}");
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
        BrokerBehavior::Vault(vec![grant(VaultAction::Store, "api-key").unwrap()]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    // Told each time that nothing ran, then stopped.
    let model = FakeModel::new(
        (0..=hatchling_loop::CONTRACT_SLIPS)
            .map(|_| {
                ModelAction::ToolCalls(vec![ToolCall {
                    name: String::from("vault_fetch_privileged"),
                    arguments: json!({ "name": "api-key" }),
                }])
            })
            .collect(),
    );
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

#[test]
fn loop_offers_only_the_tool_groups_the_broker_says_are_on() {
    // Each group's tool, called with arguments it doesn't take: offered,
    // the call is malformed; not, the tool is unknown. Either way nothing
    // runs.
    let groups = [
        (ToolGroup::Skills, "skill_invoke", json!({})),
        (ToolGroup::Web, "web_fetch", json!({})),
        (ToolGroup::Search, "web_search", json!({})),
        (ToolGroup::Browser, "browser_open", json!({})),
        (ToolGroup::Goals, "goals_read", json!({"status": "closed"})),
        (ToolGroup::Memory, "memory_propose", json!({})),
        (ToolGroup::Schedule, "schedule_propose", json!({})),
        (ToolGroup::Room, "run", json!({})),
    ];
    let every = || groups.iter().map(|(group, _, _)| *group).collect();
    for (case, run_tools) in [
        (0, None),
        (1, Some(every())),
        (2, Some(std::collections::BTreeSet::new())),
        (3, Some(std::collections::BTreeSet::from([ToolGroup::Web]))),
        (
            4,
            Some([ToolGroup::Search, ToolGroup::Skills].into_iter().collect()),
        ),
    ] {
        for (group, tool, arguments) in groups.clone() {
            let offered = run_tools.as_ref().is_none_or(|on| on.contains(&group));
            let broker = FixtureBroker::spawn(
                &format!("loop-groups-{case}-{tool}"),
                BrokerBehavior::Vault(Vec::new()),
            )
            .unwrap();
            let cell = ready_cell_offering(&broker, run_tools.clone()).unwrap();
            let model = FakeModel::new(
                (0..=hatchling_loop::CONTRACT_SLIPS)
                    .map(|_| {
                        ModelAction::ToolCalls(vec![ToolCall {
                            name: String::from(tool),
                            arguments: arguments.clone(),
                        }])
                    })
                    .collect(),
            );
            let err = cell
                .run_loop(
                    hatchling_agent::PromptKind::Chat,
                    &model,
                    String::from("use a tool"),
                    LoopBudget { max_steps: 10 },
                )
                .expect_err("a bad call, made too often, stops the loop");
            if offered {
                assert!(
                    matches!(err, hatchling_loop::Error::InvalidArguments { .. }),
                    "case {case}: {tool} is offered, got {err:?}"
                );
            } else {
                assert!(
                    matches!(err, hatchling_loop::Error::UnknownTool(_)),
                    "case {case}: {tool} is not offered, got {err:?}"
                );
            }
        }
    }
}

/// With every optional group off, the model is told so in its system
/// prompt: it neither offers what's off, nor is offered the tool.
#[test]
fn loop_tells_the_model_what_is_off() {
    let broker = FixtureBroker::spawn("loop-told-off", BrokerBehavior::Vault(Vec::new())).unwrap();
    let cell = ready_cell_offering(&broker, Some(std::collections::BTreeSet::new())).unwrap();
    let model = FakeModel::new(vec![ModelAction::Text(String::from("noted"))]);
    cell.run_loop(
        hatchling_agent::PromptKind::Chat,
        &model,
        String::from("look this up online"),
        LoopBudget { max_steps: 3 },
    )
    .unwrap();
    let seen = model.seen_histories();
    let system = seen
        .first()
        .and_then(|history| history.first())
        .and_then(|message| {
            if let Message::System(text) = message {
                Some(text.clone())
            } else {
                None
            }
        })
        .unwrap_or_default();
    assert!(
        system.contains("Web search is off on this install"),
        "{system}"
    );
    assert!(system.contains("Reading web pages is off"), "{system}");
    assert!(
        system.contains("can't run programs or commands"),
        "{system}"
    );
}

/// A run offered its computer is told it has one, and its `run` goes
/// through the real broker's session and back: a broker with no Room to
/// run in says so, in words, and the loop goes on to the model's answer.
#[test]
fn loop_asks_the_broker_to_run_a_command() {
    let broker = FixtureBroker::spawn("loop-room", BrokerBehavior::Vault(Vec::new())).unwrap();
    let cell = ready_cell_offering(
        &broker,
        Some(std::collections::BTreeSet::from([ToolGroup::Room])),
    )
    .unwrap();
    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("run"),
            arguments: json!({ "command": ["true"] }),
        }]),
        ModelAction::Text(String::from("it didn't run")),
    ]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("run true"),
            LoopBudget { max_steps: 5 },
        )
        .unwrap();
    assert_eq!(result.answer, "it didn't run");
    let seen = model.seen_histories();
    let system = format!("{:?}", seen.first().and_then(|history| history.first()));
    assert!(system.contains("a computer of your own"), "{system}");
    assert!(
        !system.contains("can't run programs or commands"),
        "{system}"
    );
    let turn2 = seen.get(1).expect("model should have a second turn");
    let results = tool_results(turn2, 2).expect("expected ToolUse at index 2");
    let Some(ToolOutput::Err(told)) = results.first() else {
        unreachable!("no command runs here, got {results:?}")
    };
    assert!(
        told == "commands run only in a chat, with the owner there: this run has no computer"
            || told == "nothing was run: there is no computer on this install",
        "{told}"
    );
}

/// Its `read` goes through the real broker's session and back the same
/// way: with no Room to read in, the broker says so, in words about a
/// read, and the loop goes on to the model's answer.
#[test]
fn loop_asks_the_broker_to_read_a_file() {
    let broker = FixtureBroker::spawn("loop-read", BrokerBehavior::Vault(Vec::new())).unwrap();
    let cell = ready_cell_offering(
        &broker,
        Some(std::collections::BTreeSet::from([ToolGroup::Room])),
    )
    .unwrap();
    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("read"),
            arguments: json!({ "path": "notes.txt" }),
        }]),
        ModelAction::Text(String::from("it wasn't read")),
    ]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("read my notes"),
            LoopBudget { max_steps: 5 },
        )
        .unwrap();
    assert_eq!(result.answer, "it wasn't read");
    let seen = model.seen_histories();
    let system = format!("{:?}", seen.first().and_then(|history| history.first()));
    assert!(system.contains("`read` reads a file there"), "{system}");
    let turn2 = seen.get(1).expect("model should have a second turn");
    let results = tool_results(turn2, 2).expect("expected ToolUse at index 2");
    let Some(ToolOutput::Err(told)) = results.first() else {
        unreachable!("no file is read here, got {results:?}")
    };
    assert!(
        told == "commands run only in a chat, with the owner there: this run has no computer"
            || told == "nothing was read: there is no computer on this install",
        "{told}"
    );
}

/// The model's `search` reaches the broker as a Room op, and the broker's
/// answer comes back to the model: with no Room to search, the broker
/// says so, in words about a search, and the loop goes on to the model's
/// answer. The model is told it has the tool.
#[test]
fn loop_asks_the_broker_to_search_its_files() {
    let broker = FixtureBroker::spawn("loop-search", BrokerBehavior::Vault(Vec::new())).unwrap();
    let cell = ready_cell_offering(
        &broker,
        Some(std::collections::BTreeSet::from([ToolGroup::Room])),
    )
    .unwrap();
    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("search"),
            arguments: json!({ "pattern": "to do", "glob": "*.md" }),
        }]),
        ModelAction::Text(String::from("nothing was searched")),
    ]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("find my to-dos"),
            LoopBudget { max_steps: 5 },
        )
        .unwrap();
    assert_eq!(result.answer, "nothing was searched");
    let seen = model.seen_histories();
    let system = format!("{:?}", seen.first().and_then(|history| history.first()));
    assert!(
        system.contains("`search` finds the lines that match a pattern in its files"),
        "{system}"
    );
    let turn2 = seen.get(1).expect("model should have a second turn");
    let results = tool_results(turn2, 2).expect("expected ToolUse at index 2");
    let Some(ToolOutput::Err(told)) = results.first() else {
        unreachable!("nothing is searched here, got {results:?}")
    };
    assert!(
        told == "commands run only in a chat, with the owner there: this run has no computer"
            || told == "nothing was searched: there is no computer on this install",
        "{told}"
    );
}

/// A run's last step is an answer: the model is told it is out of steps
/// and to say where it got to, after everything else it is shown, and
/// what it says then is the run's answer, where the run used to fail
/// with nothing to show.
#[test]
fn loop_gives_its_last_step_to_an_answer() {
    let broker = FixtureBroker::spawn("loop-last-step", BrokerBehavior::Vault(Vec::new())).unwrap();
    let cell = ready_cell_offering(
        &broker,
        Some(std::collections::BTreeSet::from([ToolGroup::Room])),
    )
    .unwrap();
    let searching = || {
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("search"),
            arguments: json!({ "pattern": "to do" }),
        }])
    };
    let model = FakeModel::new(vec![
        searching(),
        searching(),
        ModelAction::Text(String::from("I ran out of steps after two searches.")),
    ]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("find my to-dos"),
            LoopBudget { max_steps: 3 },
        )
        .unwrap();
    assert_eq!(result.answer, "I ran out of steps after two searches.");
    assert_eq!(result.steps, 3);
    let seen = model.seen_histories();
    let told = |turn: usize| {
        seen.get(turn).and_then(|history| match history.last() {
            Some(Message::Note(note)) => Some(note.clone()),
            _ => None,
        })
    };
    assert_eq!(told(0), None);
    assert_eq!(told(1), None);
    let last = told(2).expect("the last step is told it is the last");
    assert!(
        last.starts_with("This is your last step")
            && last.contains("don't call a tool")
            && last.contains("what is left to do"),
        "{last}"
    );
}

/// The model's `write` and `edit` reach the broker as Room ops, and the
/// broker's answers come back to the model: with no Room to change a
/// file in, the broker says so, and the loop goes on to the model's
/// answer. The model is told it has the tools.
#[test]
fn loop_asks_the_broker_to_write_and_edit_its_files() {
    let broker = FixtureBroker::spawn("loop-write", BrokerBehavior::Vault(Vec::new())).unwrap();
    let cell = ready_cell_offering(
        &broker,
        Some(std::collections::BTreeSet::from([ToolGroup::Room])),
    )
    .unwrap();
    let model = FakeModel::new(vec![
        ModelAction::ToolCalls(vec![
            ToolCall {
                name: String::from("write"),
                arguments: json!({ "path": "notes/a.md", "text": "to do\n" }),
            },
            ToolCall {
                name: String::from("edit"),
                arguments: json!({ "path": "notes/a.md", "old": "to do", "new": "done" }),
            },
        ]),
        ModelAction::Text(String::from("nothing was changed")),
    ]);
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("note my to-dos"),
            LoopBudget { max_steps: 5 },
        )
        .unwrap();
    assert_eq!(result.answer, "nothing was changed");
    let seen = model.seen_histories();
    let system = format!("{:?}", seen.first().and_then(|history| history.first()));
    assert!(
        system.contains("`write` makes a file there, and `edit` changes part of one"),
        "{system}"
    );
    assert!(
        system.contains("isn't done until you have run it and seen what it does"),
        "{system}"
    );
    let turn2 = seen.get(1).expect("model should have a second turn");
    let results = tool_results(turn2, 2).expect("expected ToolUse at index 2");
    assert_eq!(results.len(), 2);
    for result in results {
        let ToolOutput::Err(told) = result else {
            unreachable!("nothing is changed here, got {results:?}")
        };
        assert!(
            told == "commands run only in a chat, with the owner there: this run has no computer"
                || told == "not done: there is no computer on this install",
            "{told}"
        );
    }
}

/// The model's `give_file` and `give_folder` reach the broker as Room
/// ops, offered only where the broker says files may be given: with its
/// computer and the `files` group. Where they aren't offered, a call to
/// one is no tool's.
#[test]
fn loop_asks_the_broker_to_give_a_file_or_folder_only_where_it_may() {
    let cases = ["give_file", "give_folder"].into_iter().flat_map(|tool| {
        [
            (
                tool,
                std::collections::BTreeSet::from([ToolGroup::Room, ToolGroup::Files]),
                true,
            ),
            (
                tool,
                std::collections::BTreeSet::from([ToolGroup::Room]),
                false,
            ),
            (
                tool,
                std::collections::BTreeSet::from([ToolGroup::Files]),
                false,
            ),
        ]
    });
    for (tool, groups, offered) in cases {
        let broker = FixtureBroker::spawn("loop-give", BrokerBehavior::Vault(Vec::new())).unwrap();
        let cell = ready_cell_offering(&broker, Some(groups.clone())).unwrap();
        let model = FakeModel::new(vec![
            ModelAction::ToolCalls(vec![ToolCall {
                name: String::from(tool),
                arguments: json!({ "path": "report.pdf" }),
            }]),
            ModelAction::Text(String::from("nothing was given")),
        ]);
        let result = cell.run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("give me the report"),
            LoopBudget { max_steps: 5 },
        );
        let seen = model.seen_histories();
        let system = format!("{:?}", seen.first().and_then(|history| history.first()));
        assert_eq!(
            system.contains("`give_file` gives the owner a file"),
            offered,
            "{groups:?}: {system}"
        );
        assert_eq!(result.unwrap().answer, "nothing was given");
        let turn2 = seen.get(1).expect("model should have a second turn");
        let results = tool_results(turn2, 2).expect("expected ToolUse at index 2");
        let [ToolOutput::Err(told)] = results.as_slice() else {
            unreachable!("nothing is given here, got {results:?}")
        };
        let unattended =
            "commands run only in a chat, with the owner there: this run has no computer";
        if offered {
            assert_eq!(told, unattended);
        } else {
            // A tool it wasn't given: never asked of the broker.
            assert!(
                told.contains(tool) && told != unattended,
                "{groups:?}: {told}"
            );
        }
    }
}
