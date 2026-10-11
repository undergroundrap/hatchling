//! the real harness CLIs' network, end to end in real CLI cells.
//!
//! A harness with a vendor API runs in a CLI cell whose only network is a
//! relay on its own loopback, served by the broker. These tests run it
//! against a mock of both vendors' APIs on this host's loopback:
//!
//! - **The stub harness** (`hatchling-harness-fixture`) tries everything a
//!   hostile harness could: the relay with its surrogate, with a forged
//!   key, on another path, with ambiguous framing; any other address; the
//!   files around it. Always run where cells can be built.
//! - **The real CLIs**, Claude Code and the Codex CLI, play a scripted
//!   session (an edit, a shell command, an answer) and must return a patch.
//!   They run only when `HATCHLING_TEST_CLAUDE_CODE` /
//!   `HATCHLING_TEST_CODEX_CLI` (and `HATCHLING_TEST_CODEX_HOST`, its
//!   code-mode host) name the binaries; `HATCHLING_REQUIRE_REAL_HARNESS=1`
//!   makes their absence a failure.
//!
//! Cells need user namespaces: without them this skips, and
//! `HATCHLING_REQUIRE_CELL=1` makes the skip a failure.

#[allow(
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use std::collections::{HashMap, HashSet};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use common::{TestDir, broker_config, guard_cell_available, harness_fixture_binary, test_dir};
use hatchling_broker::harness::{HARNESS_CAPABILITY, HarnessKeys, HarnessKind};
use hatchling_broker::{Config, Harnesses, SkillRuntime, Skills, VaultConfig, handle_skill_op};
use hatchling_core::subagent::{ActivityEvent, HarnessNetworkUse, SubagentEvent};
use hatchling_core::{CellId, SkillOp, SkillResult};
use hatchling_skill::CodeHash;
use serde_json::{Value, json};
use zeroize::Zeroizing;

/// The vendor keys the tests' vault holds: what the mock must see, and
/// what the cell must never.
const CLAUDE_KEY: &str = "vendor-key-for-claude";
const CODEX_KEY: &str = "vendor-key-for-codex";

/// What the mock says last.
const ANSWER: &str = "done by the mock vendor";

/// The vendor key the broker holds for `kind`.
const fn vendor_key(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::ClaudeCode => CLAUDE_KEY,
        HarnessKind::CodexCli => CODEX_KEY,
    }
}

/// The key header's value the vendor must see for `kind`.
fn presented_key(kind: HarnessKind) -> String {
    match kind {
        HarnessKind::ClaudeCode => String::from(CLAUDE_KEY),
        HarnessKind::CodexCli => format!("Bearer {CODEX_KEY}"),
    }
}

const fn no_keys(_: HarnessKind) -> Option<Zeroizing<String>> {
    None
}

/// One request the mock vendor received.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    /// The `x-api-key` header, or else `authorization`.
    key: String,
}

/// A mock of both vendors' APIs on 127.0.0.1. It plays a scripted session
/// (an edit, a shell command, an answer), chosen by how many tool results
/// the conversation already holds, and records each request.
struct MockVendor {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    serving: Option<JoinHandle<()>>,
}

impl MockVendor {
    fn start() -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("mock bind: {e}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| format!("mock nonblocking: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("mock address: {e}"))?
            .port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, flag) = (Arc::clone(&seen), Arc::clone(&stop));
        let serving = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let log = Arc::clone(&log);
                        std::thread::spawn(move || answer(&stream, &log));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Ok(Self {
            port,
            seen,
            stop,
            serving: Some(serving),
        })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

impl Drop for MockVendor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Serves one request; a broken one gets nothing.
fn answer(stream: &TcpStream, log: &Mutex<Vec<Seen>>) {
    if stream.set_nonblocking(false).is_err()
        || stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .is_err()
    {
        return;
    }
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let path = line.split(' ').nth(1).unwrap_or_default().to_owned();
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            return;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let key = headers
        .get("x-api-key")
        .or_else(|| headers.get("authorization"))
        .cloned()
        .unwrap_or_default();
    if let Ok(mut log) = log.lock() {
        log.push(Seen {
            path: path.clone(),
            key,
        });
    }
    let (kind, payload) = if path.starts_with("/v1/messages") {
        anthropic(&body)
    } else if path.starts_with("/v1/responses") {
        openai(&body)
    } else {
        (
            "application/json",
            json!({"error": "no such path"}).to_string(),
        )
    };
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = stream.flush();
}

/// Server-sent events, in order.
fn sse(events: &[(&str, Value)]) -> String {
    let mut out = String::new();
    for (name, data) in events {
        for part in ["event: ", name, "\ndata: ", &data.to_string(), "\n\n"] {
            out.push_str(part);
        }
    }
    out
}

/// Whether the request asked for a stream.
fn streams(body: &Value) -> bool {
    body.get("stream") == Some(&Value::Bool(true))
}

/// The request's `model`, echoed back.
fn model(body: &Value) -> Value {
    body.get("model").cloned().unwrap_or(Value::Null)
}

/// Anthropic's Messages API: a `Write`, then a `Bash`, then the answer.
fn anthropic(body: &Value) -> (&'static str, String) {
    let done = body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .count();
    let tooled = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some("Write"))
        });
    let usage = json!({"input_tokens": 10, "output_tokens": 1});
    if !streams(body) {
        let whole = json!({"id": "msg_mock", "type": "message", "role": "assistant", "model": model(body),
            "content": [{"type": "text", "text": ANSWER}], "stop_reason": "end_turn",
            "stop_sequence": null, "usage": usage});
        return ("application/json", whole.to_string());
    }
    let message = json!({"id": "msg_mock", "type": "message", "role": "assistant", "model": model(body),
        "content": [], "stop_reason": null, "stop_sequence": null, "usage": usage});
    let script = [
        (
            "Write",
            json!({"file_path": "notes.txt", "content": "new notes from the mock\n"}),
        ),
        (
            "Bash",
            json!({"command": "ls && git --version && rg --version | head -1 && echo shell-ran > shell.txt", "description": "list and write"}),
        ),
    ];
    let (block, stop) = match script.get(done).filter(|_| tooled) {
        Some((name, input)) => (
            vec![
                (
                    "content_block_start",
                    json!({"type": "content_block_start", "index": 0,
                    "content_block": {"type": "tool_use", "id": format!("toolu_{done}"), "name": name, "input": {}}}),
                ),
                (
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
                ),
            ],
            "tool_use",
        ),
        None => (
            vec![
                (
                    "content_block_start",
                    json!({"type": "content_block_start", "index": 0,
                    "content_block": {"type": "text", "text": ""}}),
                ),
                (
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "text_delta", "text": ANSWER}}),
                ),
            ],
            "end_turn",
        ),
    };
    let mut events = vec![(
        "message_start",
        json!({"type": "message_start", "message": message}),
    )];
    events.extend(block);
    events.extend([
        (
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        ),
        (
            "message_delta",
            json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null},
            "usage": {"output_tokens": 4}}),
        ),
        ("message_stop", json!({"type": "message_stop"})),
    ]);
    ("text/event-stream", sse(&events))
}

/// `OpenAI`'s Responses API, as the Codex CLI's code mode uses it: an
/// `apply_patch`, then an `exec_command`, then the answer.
fn openai(body: &Value) -> (&'static str, String) {
    let done = body
        .get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.ends_with("_call_output"))
        })
        .count();
    let usage = json!({"input_tokens": 10, "output_tokens": 4, "total_tokens": 14,
        "input_tokens_details": {"cached_tokens": 0}, "output_tokens_details": {"reasoning_tokens": 0}});
    let script = [
        "await tools.apply_patch(\"*** Begin Patch\\n*** Update File: notes.txt\\n@@\\n-old notes\\n+new notes from the mock\\n*** End Patch\\n\");",
        "const r = await tools.exec_command({cmd: \"ls && git --version && rg --version | head -1 && echo shell-ran > shell.txt\", tty: false}); text(JSON.stringify(r));",
    ];
    let item = script.get(done).map_or_else(
        || json!({"type": "message", "id": "msg_mock", "status": "completed", "role": "assistant",
            "content": [{"type": "output_text", "text": ANSWER, "annotations": []}]}),
        |source| json!({"type": "custom_tool_call", "id": format!("ctc_{done}"), "call_id": format!("call_{done}"),
            "namespace": "functions", "name": "exec", "input": source, "status": "completed"}),
    );
    let response = |status: &str, output: Value| {
        json!({"id": "resp_mock", "object": "response", "status": status, "model": model(body),
            "output": output, "usage": usage})
    };
    let completed = response("completed", json!([item]));
    if !streams(body) {
        return ("application/json", completed.to_string());
    }
    let events = [
        (
            "response.created",
            json!({"type": "response.created", "response": response("in_progress", json!([]))}),
        ),
        (
            "response.output_item.done",
            json!({"type": "response.output_item.done", "output_index": 0, "item": item}),
        ),
        (
            "response.completed",
            json!({"type": "response.completed", "response": completed}),
        ),
    ];
    ("text/event-stream", sse(&events))
}

/// A deployment of one harness: its binary (a copy, pinned), a project
/// root, the artifact workspace, an activity log, and the mock vendor as
/// its API.
struct Deployment {
    dir: TestDir,
    config: Config,
    vars: HashMap<&'static str, String>,
}

/// A copy of `source` at `dir/bin/name`, and its pin.
fn pin(dir: &Path, source: &Path, name: &str) -> Result<(String, String), String> {
    let copy = dir.join("bin").join(name);
    std::fs::copy(source, &copy).map_err(|e| format!("copy {}: {e}", source.display()))?;
    let bytes = std::fs::read(&copy).map_err(|e| format!("read {}: {e}", copy.display()))?;
    Ok((copy.display().to_string(), CodeHash::of(&bytes).to_string()))
}

impl Deployment {
    fn new(
        name: &str,
        kind: HarnessKind,
        binary: &Path,
        host: Option<&Path>,
        vendor: &MockVendor,
    ) -> Result<Self, String> {
        let dir = test_dir(name);
        let _ = std::fs::remove_dir_all(&*dir);
        for sub in ["project", "workspace", "sock", "bin"] {
            std::fs::create_dir_all(dir.join(sub)).map_err(|e| format!("mkdir {sub}: {e}"))?;
        }
        std::fs::write(dir.join("project/notes.txt"), "old notes\n")
            .map_err(|e| format!("notes: {e}"))?;
        let (path, sha) = pin(&dir, binary, "harness")?;
        let (path_key, hash_key, api_key) = match kind {
            HarnessKind::ClaudeCode => (
                "HATCHLING_HARNESS_CLAUDE_CODE",
                "HATCHLING_HARNESS_CLAUDE_CODE_SHA256",
                "HATCHLING_HARNESS_CLAUDE_CODE_API",
            ),
            HarnessKind::CodexCli => (
                "HATCHLING_HARNESS_CODEX_CLI",
                "HATCHLING_HARNESS_CODEX_CLI_SHA256",
                "HATCHLING_HARNESS_CODEX_CLI_API",
            ),
        };
        let mut vars = HashMap::from([
            (path_key, path),
            (hash_key, sha),
            (api_key, format!("local:localhost:{}", vendor.port)),
            (
                "HATCHLING_HARNESS_ROOTS",
                format!("project={}", dir.join("project").display()),
            ),
            ("HATCHLING_HARNESS_TIMEOUT_SECS", String::from("180")),
        ]);
        if let Some(host) = host {
            let (path, sha) = pin(&dir, host, "codex-code-mode-host")?;
            vars.insert("HATCHLING_HARNESS_CODEX_CLI_HOST", path);
            vars.insert("HATCHLING_HARNESS_CODEX_CLI_HOST_SHA256", sha);
        }
        let uid = rustix::process::getuid().as_raw();
        let mut config = broker_config(&dir.join("sock/broker.sock"), uid.wrapping_add(1))?;
        config.vault.root = dir.join("vault");
        config.vault.key_file = VaultConfig::default_key_file(&config.vault.root);
        config.artifacts.workspace = Some(dir.join("workspace"));
        config.scheduler.root = dir.join("schedules");
        let code: HashSet<_> = [HARNESS_CAPABILITY
            .parse()
            .map_err(|e| format!("capability: {e}"))?]
        .into();
        config.skills.grants = HashMap::from([(String::from(kind.name()), code)]);
        Ok(Self { dir, config, vars })
    }

    fn skills(&self) -> Result<Skills, String> {
        let harnesses = Harnesses::parse(&self.config, |key| self.vars.get(key).cloned())
            .map_err(|e| format!("harnesses: {e}"))?;
        let runtime = SkillRuntime::new(common::spawner());
        Ok(Skills::default()
            .with_runtime(runtime)
            .with_harnesses(harnesses))
    }

    fn activity(&self) -> Vec<ActivityEvent> {
        std::fs::read_to_string(self.dir.join("schedules.activity"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The network use the last run's activity event recorded.
    fn network(&self) -> Option<HarnessNetworkUse> {
        let event = self.activity().into_iter().next_back()?;
        if let SubagentEvent::HarnessRun { network, .. } = event.kind {
            network
        } else {
            None
        }
    }
}

/// Runs `kind` on `task` over the project's `notes.txt`; its report.
fn invoke(
    skills: &Skills,
    kind: HarnessKind,
    task: &str,
    keys: HarnessKeys<'_>,
) -> Result<Value, String> {
    let op = SkillOp::Invoke {
        skill: kind.skill_id().to_owned(),
        capability: "code".parse().map_err(|e| format!("capability: {e}"))?,
        input: json!({"task": task, "root": "project", "paths": ["notes.txt"]}).to_string(),
    };
    let cell = "test"
        .parse::<CellId>()
        .map_err(|e| format!("cell id: {e}"))?;
    let result = handle_skill_op(skills, &op, &cell, keys);
    let SkillResult::Output { output } = &result else {
        return Err(format!("no report: {result:?}"));
    };
    serde_json::from_str(output).map_err(|e| format!("report: {e}"))
}

/// A report's text field at `pointer`, or nothing.
fn text<'a>(report: &'a Value, pointer: &str) -> &'a str {
    report
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[test]
fn a_hostile_harness_reaches_its_vendor_only_through_the_relay_and_only_with_its_surrogate()
-> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    for (kind, path) in [
        (HarnessKind::ClaudeCode, "/v1/messages"),
        (HarnessKind::CodexCli, "/v1/responses"),
    ] {
        let key = vendor_key(kind);
        let vendor = MockVendor::start()?;
        let mut deployment = Deployment::new(
            &format!("harness-cli-hostile-{}", kind.name()),
            kind,
            &harness_fixture_binary()?,
            None,
            &vendor,
        )?;
        // Shorter than the relay's grace past the harness timeout: only
        // that grace keeps the relay serving for the whole run.
        deployment
            .vars
            .insert("HATCHLING_HARNESS_TIMEOUT_SECS", String::from("20"));
        let vault = deployment.config.vault.root.display().to_string();
        let task = [
            format!("STUB relay {path}"),
            String::from("STUB relay /v1/files"),
            format!("STUB relay-key forged-key {path}"),
            format!("STUB relay-key {key} {path}"),
            format!("STUB relay-raw {path}"),
            String::from("STUB dial 10.255.255.254:53"),
            format!("STUB dial 127.0.0.1:{}", vendor.port),
            String::from("STUB env"),
            String::from("STUB probe /proc/1"),
            String::from("STUB probe /proc/2"),
            String::from("STUB probe /proc/sys"),
            format!("STUB probe {vault}"),
            format!("STUB probe {}", deployment.dir.join("workspace").display()),
            String::from("STUB probe /dev/null"),
        ]
        .join("\n");
        let keys = |kind| Some(Zeroizing::new(String::from(vendor_key(kind))));
        let report = invoke(&deployment.skills()?, kind, &task, &keys)?;
        assert_eq!(report.get("outcome"), Some(&json!("done")), "{report}");
        let said = text(&report, "/harness_said/message");
        let says = |line: &str| {
            assert!(
                said.lines().any(|said| said == line),
                "{line} not in:\n{said}"
            );
        };
        // The surrogate passes, and the vendor sees the vendor key.
        says(&format!("relay {path}: HTTP/1.1 200 OK"));
        // Anything else is answered by the relay itself.
        says("relay /v1/files: HTTP/1.1 404 Not Found");
        says(&format!("relay-key {path}: HTTP/1.1 401 Unauthorized"));
        says("relay-raw: HTTP/1.1 400 Bad Request");
        // Even the real key, from inside, is not this run's key.
        assert_eq!(
            said.matches("HTTP/1.1 401 Unauthorized").count(),
            2,
            "{said}"
        );
        // No route but the relay: not the host's DNS, not the vendor itself.
        says("dial 10.255.255.254:53: false");
        says(&format!("dial 127.0.0.1:{}: false", vendor.port));
        // The cell holds a surrogate, never the vendor key.
        assert!(
            !said
                .lines()
                .any(|line| line.contains('=') && line.ends_with(key)),
            "the vendor key is in the cell:\n{said}"
        );
        let surrogate_var = match kind {
            HarnessKind::ClaudeCode => "ANTHROPIC_API_KEY=sk-ant-hatchling-",
            HarnessKind::CodexCli => "HATCHLING_HARNESS_KEY=sk-hatchling-",
        };
        assert!(
            said.lines().any(|line| line.starts_with(surrogate_var)),
            "{said}"
        );
        // Its own processes only; nothing of the broker's around it.
        says("probe /proc/1: true");
        says("probe /proc/sys: false");
        says(&format!("probe {vault}: false"));
        says(&format!(
            "probe {}: false",
            deployment.dir.join("workspace").display()
        ));
        says("probe /dev/null: true");
        // The vendor saw exactly the one good request, with the real key.
        let seen = vendor.seen();
        assert_eq!(seen.len(), 1, "{seen:?}");
        let first = seen
            .first()
            .map(|seen| (seen.path.as_str(), seen.key.as_str()));
        assert_eq!(first, Some((path, presented_key(kind).as_str())));
        assert_eq!(
            deployment.network(),
            Some(HarnessNetworkUse {
                destination: format!("localhost:{}", vendor.port),
                requests: 1,
                refused: 4,
            })
        );
    }
    Ok(())
}

#[test]
fn a_harness_without_its_vendor_key_is_refused_before_anything_runs() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let vendor = MockVendor::start()?;
    let deployment = Deployment::new(
        "harness-cli-no-key",
        HarnessKind::ClaudeCode,
        &harness_fixture_binary()?,
        None,
        &vendor,
    )?;
    let report = invoke(
        &deployment.skills()?,
        HarnessKind::ClaudeCode,
        "STUB relay /v1/messages",
        &no_keys,
    )?;
    assert_eq!(report.get("outcome"), Some(&json!("refused")), "{report}");
    assert!(
        text(&report, "/reason").contains("harness-claude-code"),
        "{report}"
    );
    assert!(vendor.seen().is_empty());
    Ok(())
}

/// The real CLI named by `var`, if any; its absence fails only under
/// `HATCHLING_REQUIRE_REAL_HARNESS=1`.
fn real(var: &str) -> Option<PathBuf> {
    let found = std::env::var(var)
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_file());
    let required = std::env::var("HATCHLING_REQUIRE_REAL_HARNESS").is_ok_and(|value| value == "1");
    assert!(
        found.is_some() || !required,
        "HATCHLING_REQUIRE_REAL_HARNESS=1 but {var} names no binary"
    );
    found
}

/// One real CLI's scripted session: an edit, a shell command through its
/// tools, an answer; a patch back, and every request with the vendor key.
fn a_real_cli_edits_runs_a_command_and_answers(
    kind: HarnessKind,
    binary: &Path,
    host: Option<&Path>,
) -> Result<(), String> {
    let vendor = MockVendor::start()?;
    let deployment = Deployment::new(
        &format!("harness-cli-real-{}", kind.name()),
        kind,
        binary,
        host,
        &vendor,
    )?;
    let keys = |kind| Some(Zeroizing::new(String::from(vendor_key(kind))));
    let report = invoke(
        &deployment.skills()?,
        kind,
        "Update notes.txt, then list the files.",
        &keys,
    )?;
    assert_eq!(report.get("outcome"), Some(&json!("done")), "{report}");
    assert_eq!(text(&report, "/harness_said/message"), ANSWER, "{report}");
    assert_eq!(
        report.get("files_changed"),
        Some(&json!(["notes.txt"])),
        "{report}"
    );
    assert_eq!(
        report.get("files_added"),
        Some(&json!(["shell.txt"])),
        "the shell ran through the tools: {report}"
    );
    let patch = report
        .get("patch")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("no patch: {report}"))?;
    let patched = std::fs::read_to_string(deployment.dir.join("workspace").join(patch))
        .map_err(|e| format!("patch: {e}"))?;
    assert!(patched.contains("+new notes from the mock"), "{patched}");
    assert_eq!(
        std::fs::read_to_string(deployment.dir.join("project/notes.txt"))
            .map_err(|e| format!("notes: {e}"))?,
        "old notes\n",
        "the project is untouched"
    );
    let seen = vendor.seen();
    assert!(seen.len() >= 3, "{seen:?}");
    let wanted = presented_key(kind);
    assert!(seen.iter().all(|request| request.key == wanted), "{seen:?}");
    let network = deployment
        .network()
        .ok_or_else(|| String::from("the run recorded no network use"))?;
    assert_eq!(
        usize::try_from(network.requests).map_err(|e| e.to_string())?,
        seen.len()
    );
    Ok(())
}

#[test]
fn real_claude_code_runs_in_its_cell_through_the_relay() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    real("HATCHLING_TEST_CLAUDE_CODE").map_or(Ok(()), |binary| {
        a_real_cli_edits_runs_a_command_and_answers(HarnessKind::ClaudeCode, &binary, None)
    })
}

#[test]
fn the_real_codex_cli_runs_in_its_cell_through_the_relay() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    match (
        real("HATCHLING_TEST_CODEX_CLI"),
        real("HATCHLING_TEST_CODEX_HOST"),
    ) {
        (Some(binary), host) => a_real_cli_edits_runs_a_command_and_answers(
            HarnessKind::CodexCli,
            &binary,
            host.as_deref(),
        ),
        (None, _) => Ok(()),
    }
}
