//! Integration tests: a local model endpoint, end to end.
//!
//! A scripted OpenAI-dialect server listens on loopback, as ollama or
//! llama.cpp would. The agent runs [`Cell::run_task`] exactly as in
//! production: the real broker mints the surrogate, its egress proxy
//! reaches the server over the pinned, verified loopback connection a
//! `local-keyless:` allowlist entry gives it, and the loop drives tool
//! calls through the broker session. Nothing here touches anything but
//! the loopback interface.

#![allow(
    clippy::unwrap_used,
    reason = "the scripted server and fixtures are test scaffolding: a failed setup step should fail the test"
)]

pub(crate) mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid, Ready, VaultAction};
use hatchling_model::ProviderConfig;
use hatchling_prompt::probe::PROBE_TOOL;
use serde_json::{Value, json};

/// One request the local server read.
#[derive(Debug, Clone)]
struct Seen {
    head: String,
    body: Value,
}

/// A scripted OpenAI-dialect server on `127.0.0.1`: answers each
/// connection's request with the next reply in its script, every other
/// reply chunked as a Go or Python server would send a large body, and
/// records what it read.
struct LocalServer {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    handle: Option<JoinHandle<()>>,
}

impl LocalServer {
    fn start(replies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let handle = std::thread::spawn(move || {
            for (index, reply) in replies.into_iter().enumerate() {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                let Some(request) = read_request(&mut stream) else {
                    // The teardown connection: nothing more is coming.
                    return;
                };
                record.lock().unwrap().push(request);
                let framed = if index % 2 == 1 {
                    let (first, second) = reply.split_at(reply.len() / 2);
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{first}\r\n{:x}\r\n{second}\r\n0\r\n\r\n",
                        first.len(),
                        second.len()
                    )
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{reply}",
                        reply.len()
                    )
                };
                let _ = stream.write_all(framed.as_bytes());
            }
        });
        Self {
            port,
            seen,
            handle: Some(handle),
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        // Unblock an `accept` the script never reached.
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Reads one request: the head, then a `Content-Length` body.
fn read_request(stream: &mut std::net::TcpStream) -> Option<Seen> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).ok()?;
        head.extend_from_slice(&byte);
    }
    let head = String::from_utf8(head).ok()?;
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))?
        .parse()
        .ok()?;
    let mut body = vec![0; length];
    stream.read_exact(&mut body).ok()?;
    Some(Seen {
        head,
        body: serde_json::from_slice(&body).ok()?,
    })
}

fn tool_call_reply(name: &str, arguments: &Value) -> String {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-local",
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()},
                }]
            }
        }]
    })
    .to_string()
}

fn text_reply(text: &str) -> String {
    json!({"choices": [{"message": {"role": "assistant", "content": text}}]}).to_string()
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

fn probe_reply() -> String {
    tool_call_reply(PROBE_TOOL, &json!({"ready": true}))
}

/// The real broker, with the local endpoint at `port` allowlisted as
/// keyless and granted, and `api-key` storable and fetchable.
fn local_broker(name: &str, port: u16) -> FixtureBroker {
    let grant = |action, name: &str| hatchling_broker::VaultGrant {
        action,
        name: name.parse().unwrap(),
    };
    FixtureBroker::spawn(
        name,
        BrokerBehavior::EgressWithVault {
            allowlist: format!("local-keyless:localhost:{port}"),
            grants: vec![format!("localhost:{port}")],
            vault: vec![
                grant(VaultAction::Store, "api-key"),
                grant(VaultAction::Fetch, "api-key"),
            ],
        },
    )
    .unwrap()
}

fn ready_cell(broker: &FixtureBroker) -> Cell<Ready> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-local".parse().unwrap(),
        broker_socket: broker.socket_path.parse().unwrap(),
        broker_uid: PeerUid::from_kernel(uid),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        run_subagent: false,
    };
    Cell::new(config)
        .check_preconditions()
        .unwrap()
        .connect_broker()
        .unwrap()
        .authorize()
        .unwrap()
        .handshake()
        .unwrap()
}

fn local_provider(port: u16) -> ProviderConfig {
    ProviderConfig {
        endpoint: format!("http://localhost:{port}/v1"),
        model: String::from("local-model"),
        bearer: None,
        context_window: Some(32_768),
    }
}

fn is_probe(seen: &Seen) -> bool {
    seen.body.get("tool_choice") == Some(&json!("required"))
        && seen
            .body
            .pointer("/tools/0/function/name")
            .and_then(Value::as_str)
            == Some(PROBE_TOOL)
}

/// WO-009's acceptance test: a 512 KiB request and a 512 KiB answer
/// cross the session whole, through the real broker and its egress
/// proxy, though a session frame holds 64 KiB.
#[test]
fn a_512_kib_request_and_answer_cross_the_session_whole() {
    let content = "x".repeat(512 << 10);
    let server = LocalServer::start(vec![text_reply(&content)]);
    let broker = local_broker("egress-large", server.port);
    let cell = ready_cell(&broker);
    let destination = format!("localhost:{}", server.port);
    let surrogate = cell.egress_mint_surrogate(&destination, 60).unwrap();
    let sent = json!({"messages": [{"role": "user", "content": "y".repeat(512 << 10)}]});
    let body = serde_json::to_vec(&sent).unwrap();
    assert!(body.len() > 512 << 10);
    let response = cell
        .egress_request(
            &surrogate,
            "POST",
            &destination,
            "/v1/chat/completions",
            vec![(
                String::from("Content-Type"),
                String::from("application/json"),
            )],
            &body,
        )
        .unwrap();
    assert_eq!(response.status, 200);
    let answer: Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(
        answer
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str),
        Some(content.as_str())
    );
    let seen = server.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen.first().map(|seen| &seen.body), Some(&sent));
}

#[test]
fn a_task_runs_end_to_end_against_a_local_endpoint() {
    let server = LocalServer::start(vec![
        probe_reply(),
        tool_call_reply("vault_fetch", &json!({"name": "api-key"})),
        text_reply("fetched it"),
    ]);
    let broker = local_broker("local-e2e", server.port);
    let cell = ready_cell(&broker);
    cell.vault_store("api-key", b"super-secret").unwrap();

    let result = cell
        .run_task(
            hatchling_agent::PromptKind::Worker,
            String::from("fetch the api key"),
            local_provider(server.port),
            5,
        )
        .unwrap();
    assert_eq!(result.answer, "fetched it");
    assert_eq!(result.steps, 2);

    let seen = server.seen();
    assert_eq!(seen.len(), 3, "the probe, then two model turns");
    // The handshake came first; the task's turns after it.
    assert!(is_probe(seen.first().unwrap()));
    assert!(!is_probe(seen.get(1).unwrap()));
    for request in &seen {
        assert!(
            request.head.starts_with(&format!(
                "POST /v1/chat/completions HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n",
                server.port
            )),
            "{}",
            request.head
        );
        // Keyless: no credential travels, and none was invented.
        assert!(!request.head.to_ascii_lowercase().contains("authorization"));
        assert_eq!(request.body.get("model"), Some(&json!("local-model")));
        // Local is not trusted: the secret never reaches the server.
        assert!(!request.body.to_string().contains("super-secret"));
    }
    // The second turn carried the tool definitions and the fetch's
    // result, redacted.
    let turn_two = seen.get(2).unwrap();
    let tool_message = turn_two
        .body
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| {
            messages
                .iter()
                .find(|message| message.get("role") == Some(&json!("tool")))
        })
        .unwrap();
    assert_eq!(
        tool_message.get("content").and_then(Value::as_str).unwrap(),
        redacted_fetch()
    );
    let tools = seen
        .get(1)
        .unwrap()
        .body
        .get("tools")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(tools.contains(&"vault_fetch"), "{tools:?}");
}

#[test]
fn the_budget_holds_against_a_local_endpoint() {
    let fetch = || tool_call_reply("vault_fetch", &json!({"name": "api-key"}));
    let server = LocalServer::start(vec![probe_reply(), fetch(), fetch(), fetch(), fetch()]);
    let broker = local_broker("local-budget", server.port);
    let cell = ready_cell(&broker);
    let error = cell
        .run_task(
            hatchling_agent::PromptKind::Worker,
            String::from("loop forever"),
            local_provider(server.port),
            2,
        )
        .unwrap_err();
    assert!(
        matches!(error, Error::LoopBudgetExhausted { steps: 2 }),
        "got {error:?}"
    );
    assert_eq!(
        error.to_string(),
        "agent loop failed: loop budget exhausted after 2 steps"
    );
    assert_eq!(
        server.seen().len(),
        3,
        "the probe, then exactly the budget's two turns"
    );
}

#[test]
fn a_model_that_does_not_call_tools_is_refused_before_the_task() {
    let server = LocalServer::start(vec![text_reply("I only chat.")]);
    let broker = local_broker("local-incapable", server.port);
    let cell = ready_cell(&broker);
    let error = cell
        .run_task(
            hatchling_agent::PromptKind::Worker,
            String::from("do the task"),
            local_provider(server.port),
            5,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "the model endpoint failed its capability handshake: the model cannot do what the loop needs: it answered the capability probe in text: it does not call tools"
    );
    assert!(matches!(error, Error::ModelRefused(_)));
    let seen = server.seen();
    assert_eq!(seen.len(), 1, "the task never started");
    assert!(is_probe(seen.first().unwrap()));
}

#[test]
fn the_handshake_is_rerun_every_session() {
    // The first session's model calls tools; then a model that does not
    // is swapped in behind the same endpoint. The next session catches it.
    let server = LocalServer::start(vec![
        probe_reply(),
        text_reply("first task done"),
        text_reply("swapped-in model, chat only"),
    ]);
    let broker = local_broker("local-swap", server.port);
    let cell = ready_cell(&broker);
    let first = cell
        .run_task(
            hatchling_agent::PromptKind::Worker,
            String::from("first"),
            local_provider(server.port),
            5,
        )
        .unwrap();
    assert_eq!(first.answer, "first task done");
    let second = cell
        .run_task(
            hatchling_agent::PromptKind::Worker,
            String::from("second"),
            local_provider(server.port),
            5,
        )
        .unwrap_err();
    assert!(matches!(second, Error::ModelRefused(_)), "{second:?}");
    let seen = server.seen();
    assert_eq!(seen.len(), 3);
    assert!(is_probe(seen.first().unwrap()));
    assert!(!is_probe(seen.get(1).unwrap()));
    assert!(is_probe(seen.get(2).unwrap()));
}

#[test]
fn an_unreachable_local_endpoint_fails_loudly_before_the_task() {
    // A port bound and released: nothing listens on it.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let broker = local_broker("local-unreachable", port);
    let cell = ready_cell(&broker);
    let error = cell
        .run_task(
            hatchling_agent::PromptKind::Worker,
            String::from("do the task"),
            local_provider(port),
            5,
        )
        .unwrap_err();
    assert!(
        matches!(&error, Error::ModelRefused(reason) if reason.starts_with("transport failed: ")),
        "got {error:?}"
    );
}
