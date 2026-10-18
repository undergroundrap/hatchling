//! Integration tests: the model provider driving the agent loop
//! through the real broker pipeline.
//!
//! These pin 's promise: a real [`ChatCompletionsProvider`]
//! (not the fake model) drives [`Cell::run_loop`] against the fixture
//! broker, so tool calls execute through the peer-authorized session;
//! and [`Cell::run_task`] wires endpoint parsing, surrogate minting,
//! and the egress transport together, failing closed without ever
//! touching the outside network.

#![allow(
    clippy::unwrap_used,
    reason = "the scenario helper is test scaffolding: a failed setup step should fail the test"
)]

pub(crate) mod common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid, Ready, VaultAction};
use hatchling_loop::LoopBudget;
use hatchling_model::{
    ChatCompletionsProvider, ProviderConfig, Redactor, Transport, TransportError,
};
use serde_json::{Value, json};

/// A `Ready` cell against the fixture broker's socket.
fn ready_cell(broker: &FixtureBroker) -> Result<Cell<Ready>, Error> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-model".parse()?,
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
    };
    Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()
}

fn grant(
    action: VaultAction,
    name: &str,
) -> Result<hatchling_broker::VaultGrant, hatchling_core::Error> {
    Ok(hatchling_broker::VaultGrant {
        action,
        name: name.parse()?,
    })
}

/// A transport replaying a script of replies, recording every request
/// it saw so tests can assert on the wire shape the provider produced.
/// Cloning shares the underlying state, so the test keeps a probe
/// while the provider owns its handle.
#[derive(Clone)]
struct Scripted {
    inner: Arc<ScriptedInner>,
}

struct ScriptedInner {
    replies: Mutex<VecDeque<Result<Vec<u8>, TransportError>>>,
    requests: Mutex<Vec<Value>>,
}

impl Scripted {
    fn new(replies: Vec<&str>) -> Self {
        Self {
            inner: Arc::new(ScriptedInner {
                replies: Mutex::new(
                    replies
                        .into_iter()
                        .map(|reply| Ok(reply.as_bytes().to_vec()))
                        .collect(),
                ),
                requests: Mutex::new(Vec::new()),
            }),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.inner
            .requests
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

impl Transport for Scripted {
    fn post(
        &self,
        _destination: &str,
        _path: &str,
        _headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>, TransportError> {
        if let Ok(mut guard) = self.inner.requests.lock() {
            if let Ok(value) = serde_json::from_slice::<Value>(body) {
                guard.push(value);
            }
        }
        self.inner
            .replies
            .lock()
            .ok()
            .and_then(|mut guard| guard.pop_front())
            .unwrap_or_else(|| Err(TransportError::Failed(String::from("script exhausted"))))
    }
}

fn tool_call_reply(name: &str, arguments: &Value) -> String {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "script-1",
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments.to_string(),
                    }
                }]
            }
        }]
    })
    .to_string()
}

fn text_reply(text: &str) -> String {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": text,
            }
        }]
    })
    .to_string()
}

#[test]
fn provider_drives_loop_with_real_tool_through_broker() {
    let broker = FixtureBroker::spawn(
        "model-loop",
        BrokerBehavior::Vault(vec![
            grant(VaultAction::Store, "api-key").unwrap(),
            grant(VaultAction::Fetch, "api-key").unwrap(),
        ]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    cell.vault_store("api-key", b"super-secret").unwrap();

    let transport = Scripted::new(vec![
        &tool_call_reply("vault_fetch", &json!({ "name": "api-key" })),
        &text_reply("fetched it"),
    ]);
    let probe = transport.clone();
    let redactor = Redactor::default();
    let provider = ChatCompletionsProvider::new(
        ProviderConfig {
            endpoint: String::from("https://models.example.com/v1"),
            model: String::from("test-model"),
            bearer: None,
            context_window: None,
        },
        transport,
    )
    .unwrap()
    .with_redactor(redactor.clone());
    let result = cell
        .run_loop_redacted(
            hatchling_agent::PromptKind::Chat,
            &provider,
            String::from("fetch the api key"),
            LoopBudget { max_steps: 5 },
            &redactor,
        )
        .unwrap();
    assert_eq!(result.answer, "fetched it");
    assert_eq!(result.steps, 2);

    // The tool result the loop fed back on turn 2 is the fetch's success
    // from the fixture broker's vault (proving the provider's tool call
    // executed through the broker session, not a stub), with the secret
    // itself redacted before the request left.
    let requests = probe.requests();
    assert_eq!(requests.len(), 2);
    let turn_two = requests.get(1).expect("expected a second request");
    let messages = turn_two
        .get("messages")
        .and_then(Value::as_array)
        .expect("request must carry messages");
    let tool_result = messages
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .expect("turn two must contain the tool result");
    let content = tool_result
        .get("content")
        .and_then(Value::as_str)
        .expect("tool result must have string content");
    assert_eq!(content, redacted_fetch());
    assert!(
        requests
            .iter()
            .all(|request| !request.to_string().contains("super-secret")),
        "the secret must never reach the model"
    );
    // The loop itself, inside the cell, did see it.
    let history = format!("{:?}", result.history);
    assert!(history.contains("super-secret"));
}

#[test]
fn run_task_fails_closed_without_network() {
    // localhost:9 is the discard port: allowlisted (as internal:, since it
    // is this machine) and granted, so the
    // broker mints and processes the op — but no credential is
    // configured for it, so the proxy refuses at the credential swap
    // before any network I/O. No outside network is ever touched.
    let broker = FixtureBroker::spawn(
        "model-run-task",
        BrokerBehavior::Egress {
            allowlist: String::from("internal:localhost:9"),
            grants: vec![String::from("localhost:9")],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let error = cell
        .run_task(
            hatchling_agent::PromptKind::Worker,
            String::from("hi"),
            ProviderConfig {
                endpoint: String::from("https://localhost:9/v1"),
                model: String::from("test-model"),
                bearer: None,
                context_window: None,
            },
            3,
        )
        .unwrap_err();
    // The endpoint parsed, the surrogate minted, the transport posted
    // the capability probe, and the broker refused the forward: the
    // full wiring exercised, failing loudly before the task started
    //, not partway through it.
    let refused =
        matches!(&error, Error::ModelRefused(reason) if reason.starts_with("transport failed: "));
    assert!(
        refused,
        "a refused model endpoint must fail the handshake, got {error:?}"
    );
}

#[test]
fn run_task_rejects_bad_provider_config_before_minting() {
    let broker = FixtureBroker::spawn("model-bad-config", BrokerBehavior::Vault(vec![])).unwrap();
    let cell = ready_cell(&broker).unwrap();

    let http = cell.run_task(
        hatchling_agent::PromptKind::Worker,
        String::from("hi"),
        ProviderConfig {
            endpoint: String::from("http://models.example.com"),
            model: String::from("m"),
            bearer: None,
            context_window: None,
        },
        3,
    );
    assert!(
        matches!(
            http,
            Err(Error::InvalidConfig { key, .. }) if key == "HATCHLING_MODEL_ENDPOINT"
        ),
        "http endpoint must be rejected at construction"
    );

    let empty_model = cell.run_task(
        hatchling_agent::PromptKind::Worker,
        String::from("hi"),
        ProviderConfig {
            endpoint: String::from("https://models.example.com"),
            model: String::new(),
            bearer: None,
            context_window: None,
        },
        3,
    );
    assert!(
        matches!(
            empty_model,
            Err(Error::InvalidConfig { key, .. }) if key == "HATCHLING_MODEL_NAME"
        ),
        "empty model name must be rejected at construction"
    );

    let small_window = cell.run_task(
        hatchling_agent::PromptKind::Worker,
        String::from("hi"),
        ProviderConfig {
            endpoint: String::from("http://localhost:11434/v1"),
            model: String::from("m"),
            bearer: None,
            context_window: Some(4096),
        },
        3,
    );
    assert!(
        matches!(
            small_window,
            Err(Error::InvalidConfig { key, .. }) if key == "HATCHLING_MODEL_CONTEXT_WINDOW"
        ),
        "a declared window below the floor must be rejected at construction"
    );
}

/// The fetch's result as the model must see it: the fetch succeeded,
/// and the secret is redacted.
fn redacted_fetch() -> String {
    hatchling_prompt::wrap_tool_result(
        "vault_fetch",
        true,
        &json!({"name": "api-key", "secret": hatchling_model::REDACTED}),
    )
}

/// One scenario, run through a provider at `endpoint`: what the model
/// was sent, and what the loop made of its answers.
struct Run {
    requests: Vec<Value>,
    outcome: String,
}

/// Runs `script` through a fresh cell and broker against a provider at
/// `endpoint`, wired as `run_task` wires it, with `api-key` granted and
/// stored and `other` not granted.
fn run_scenario(name: &str, endpoint: &str, script: &[String]) -> Run {
    let broker = FixtureBroker::spawn(
        name,
        BrokerBehavior::Vault(vec![
            grant(VaultAction::Store, "api-key").unwrap(),
            grant(VaultAction::Fetch, "api-key").unwrap(),
        ]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    cell.vault_store("api-key", b"super-secret").unwrap();
    let transport = Scripted::new(script.iter().map(String::as_str).collect());
    let probe = transport.clone();
    let redactor = Redactor::default();
    let provider = ChatCompletionsProvider::new(
        ProviderConfig {
            endpoint: endpoint.to_owned(),
            model: String::from("same-model"),
            bearer: None,
            context_window: None,
        },
        transport,
    )
    .unwrap()
    .with_redactor(redactor.clone());
    let outcome = cell.run_loop_redacted(
        hatchling_agent::PromptKind::Chat,
        &provider,
        String::from("use the api key"),
        LoopBudget { max_steps: 5 },
        &redactor,
    );
    Run {
        requests: probe.requests().into_iter().map(sorted_tools).collect(),
        outcome: match outcome {
            Ok(result) => format!("answer {:?} after {} steps", result.answer, result.steps),
            Err(error) => format!("error {error}"),
        },
    }
}

/// `request` with its tool list in name order: the loop's registry
/// lists tools in hash order, which differs between any two registries,
/// cloud or local alike, so parity compares the set.
fn sorted_tools(mut request: Value) -> Value {
    if let Some(tools) = request.get_mut("tools").and_then(Value::as_array_mut) {
        tools.sort_by_key(|tool| tool.pointer("/function/name").map(ToString::to_string));
    }
    request
}

const CLOUD: &str = "https://models.example.com/v1";
const LOCAL: &str = "http://localhost:11434/v1";

#[test]
fn cloud_and_local_providers_redact_and_rail_identically() {
    // A granted fetch (redacted on the way to the model), an ungranted
    // one (refused by the broker's grant rail and fed back as an
    // error), then an answer.
    let script = [
        tool_call_reply("vault_fetch", &json!({ "name": "api-key" })),
        tool_call_reply("vault_fetch", &json!({ "name": "other" })),
        text_reply("done"),
    ];
    let cloud = run_scenario("parity-cloud", CLOUD, &script);
    let local = run_scenario("parity-local", LOCAL, &script);
    assert_eq!(cloud.requests, local.requests);
    assert_eq!(cloud.outcome, local.outcome);
    assert_eq!(cloud.outcome, "answer \"done\" after 3 steps");
    assert_eq!(cloud.requests.len(), 3);
    let wire = local
        .requests
        .iter()
        .map(Value::to_string)
        .collect::<String>();
    assert!(
        !wire.contains("super-secret"),
        "the secret reached the model"
    );
    let results = local
        .requests
        .last()
        .and_then(|request| request.get("messages"))
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|message| message.get("role") == Some(&json!("tool")))
                .filter_map(|message| message.get("content").and_then(Value::as_str))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(results.first().copied(), Some(redacted_fetch().as_str()));
    assert!(
        results
            .get(1)
            .is_some_and(|refusal| refusal
                .starts_with("<tool_result tool=\"vault_fetch\" status=\"error\">")),
        "the refusal must reach the model: {results:?}"
    );

    // A tool the loop does not have stops the run, the same way for both.
    let unknown = [tool_call_reply("delete_everything", &json!({}))];
    let cloud = run_scenario("parity-cloud-unknown", CLOUD, &unknown);
    let local = run_scenario("parity-local-unknown", LOCAL, &unknown);
    assert_eq!(cloud.outcome, local.outcome);
    assert_eq!(cloud.outcome, "error unknown tool: delete_everything");
    assert_eq!(cloud.requests, local.requests);
}
