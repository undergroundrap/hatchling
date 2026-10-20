//! End to end: subagents in real cells.
//!
//! Everything below the parent is production code: the session
//! ([`serve_connection_with_runs`]), its subagent ledger and checks, the
//! cell launcher with a per-child socket and grant-scoped session, the
//! `hatchling-broker agent-run` helper, and the real cell (namespaces,
//! id map, seccomp). The child's program is `hatchling-run-fixture`,
//! which reports what its cell can see and do; the last tests put the
//! real agent library in the parent's place, driving the spawn tools.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

#[allow(
    dead_code,
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use std::collections::BTreeSet;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::{guard_cell_available, mapped_agent_uid, run_fixture_binary};
use hatchling_broker::{
    Config, EgressConfig, GoalsConfig, MemoryConfig, RunEnv, SchedulerConfig, Skills, SpaceConfig,
    VaultConfig, VaultGrant, WebConfig, serve_connection_with_runs,
};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::subagent::{MAX_CHILD_STEPS, ORPHANED};
use hatchling_core::{
    ActivityEvent, AgentMessage, BrokerMessage, ChildFailure, ChildOutcome, ChildReport,
    ChildVerdict, LogLevel, PeerUid, ScheduleGrant, SessionRequest, SessionResponse, SubagentEvent,
    SubagentOp, SubagentRefusal, SubagentResult, VaultAction,
};
use hatchling_vault::{MasterKey, SealedVault, Secret};
use serde_json::{Value, json};

/// How long a test waits for a real child.
const RUN_DEADLINE: Duration = Duration::from_secs(60);

/// Joins `handle` within `RUN_DEADLINE`, failing the test past it: a
/// thread that never ends must not hang the suite.
fn join_within(handle: JoinHandle<()>) {
    let started = Instant::now();
    while !handle.is_finished() {
        assert!(started.elapsed() < RUN_DEADLINE, "a session never ended");
        std::thread::sleep(Duration::from_millis(20));
    }
    handle.join().unwrap();
}

/// The secret the vault holds for `app-token`.
const SECRET: &str = "sk-live-0123456789abcdef";

/// A test's base under `/tmp` (child sockets must fit `sun_path`),
/// removed when the test ends.
fn base(name: &str) -> common::TestDir {
    let base = PathBuf::from(format!("/tmp/hlsa-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    common::TestDir::guard(base)
}

fn config(base: &Path) -> Config {
    let vault_root = base.join("vault");
    Config {
        socket: "/tmp/hatchling-test-subagents-dummy/broker.sock"
            .parse()
            .unwrap(),
        agent_uid: PeerUid::from_kernel(mapped_agent_uid()),
        socket_mode: 0o600,
        max_connections: 16,
        scheduler: SchedulerConfig {
            root: base.join("schedules"),
            ..SchedulerConfig::disabled()
        },
        log_level: LogLevel::Error,
        vault: VaultConfig {
            key_file: VaultConfig::default_key_file(&vault_root),
            root: vault_root,
            grants: ["app-token", "old-token"]
                .iter()
                .map(|name| VaultGrant {
                    action: VaultAction::Fetch,
                    name: name.parse().unwrap(),
                })
                .collect(),
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: Vec::new(),
        },
        channels: vec![String::from("local")],
        channel_accounts: Vec::new(),
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: SpaceConfig::disabled(),
        web: WebConfig::off(),
        goals: GoalsConfig::disabled(),
    }
}

/// The vault, with `app-token` stored when `with_secret`.
fn seed(config: &Config, key: &MasterKey, with_secret: bool) {
    let vault = SealedVault::open(&config.vault.root, key).unwrap();
    let (name, value) = if with_secret {
        ("app-token", SECRET)
    } else {
        ("unrelated", "x")
    };
    vault
        .store(
            &name.parse().unwrap(),
            &Secret::new(value.as_bytes().to_vec()).unwrap(),
        )
        .unwrap();
}

fn env(config: &Config, timeout: Duration) -> RunEnv {
    RunEnv {
        spawner: common::spawner(),
        agent_bin: run_fixture_binary().unwrap(),
        run_root: config.scheduler.run_root(),
        timeout,
    }
}

fn vault(name: &str) -> ScheduleGrant {
    ScheduleGrant::Vault {
        action: String::from("fetch"),
        name: name.to_owned(),
    }
}

fn sha(bytes: &[u8]) -> String {
    hatchling_artifact::request::sha256_hex(bytes)
}

fn run_dirs(config: &Config) -> Vec<PathBuf> {
    std::fs::read_dir(config.scheduler.run_root())
        .map(|dir| {
            dir.filter_map(|entry| entry.ok().map(|e| e.path()))
                .collect()
        })
        .unwrap_or_default()
}

/// A parent session: the real session code over a socket pair, with its
/// children started through the test's run environment.
struct Parent {
    client: UnixStream,
    server: Option<JoinHandle<()>>,
}

impl Parent {
    fn open(config: &Config, key: MasterKey, timeout: Duration) -> Self {
        let (client, server) = UnixStream::pair().unwrap();
        client.set_read_timeout(Some(RUN_DEADLINE * 2)).unwrap();
        let session_config = config.clone();
        let runs = env(config, timeout);
        let server = std::thread::spawn(move || {
            let _ = serve_connection_with_runs(
                &server,
                &session_config,
                &key,
                &Skills::default(),
                None,
                Some(runs),
            );
        });
        let hello = AgentMessage::Hello {
            v: hatchling_core::PROTOCOL_VERSION,
            cell_id: String::from("cell-parent"),
        };
        write_frame(&client, &serde_json::to_vec(&hello).unwrap()).unwrap();
        let welcome: BrokerMessage = serde_json::from_slice(&read_frame(&client).unwrap()).unwrap();
        assert!(matches!(welcome, BrokerMessage::Welcome { .. }));
        Self {
            client,
            server: Some(server),
        }
    }

    fn ask(&self, op: SubagentOp) -> SubagentResult {
        let request = SessionRequest::Subagent(op);
        write_frame(&self.client, &serde_json::to_vec(&request).unwrap()).unwrap();
        let response: SessionResponse =
            serde_json::from_slice(&read_frame(&self.client).unwrap()).unwrap();
        let SessionResponse::Subagent(result) = response else {
            unreachable!("not a subagent reply: {response:?}");
        };
        result
    }

    fn spawn(&self, task: &str, budget: u32, capabilities: Vec<ScheduleGrant>) -> SubagentResult {
        self.ask(SubagentOp::Spawn {
            task: task.to_owned(),
            budget,
            capabilities,
        })
    }

    fn spawned(&self, task: &str, budget: u32, capabilities: Vec<ScheduleGrant>) -> String {
        let result = self.spawn(task, budget, capabilities);
        let SubagentResult::Spawned { child, .. } = result else {
            unreachable!("not spawned: {result:?}");
        };
        child
    }

    fn report(&self, op: SubagentOp) -> ChildReport {
        let result = self.ask(op);
        let SubagentResult::Report(report) = result else {
            unreachable!("no report: {result:?}");
        };
        report
    }

    fn wait(&self, child: &str) -> ChildReport {
        self.report(SubagentOp::Wait {
            child: child.to_owned(),
        })
    }

    /// Ends the session, as a parent that goes away does; how long the
    /// session took to end.
    fn close(mut self) -> Duration {
        let started = Instant::now();
        drop(self.client);
        if let Some(server) = self.server.take() {
            join_within(server);
        }
        started.elapsed()
    }
}

/// The answer of a finished child.
fn answer(report: &ChildReport) -> &str {
    let ChildOutcome::Finished { answer, .. } = &report.outcome else {
        unreachable!("not finished: {report:?}");
    };
    answer
}

/// The activity log's events, each checked against the contract: its
/// exact field set, and a typed parse.
fn events(config: &Config) -> Vec<ActivityEvent> {
    let text = std::fs::read_to_string(config.scheduler.activity_log()).unwrap();
    let common = ["event", "event_id", "ts", "run_id", "child_id"];
    text.lines()
        .map(|line| {
            let value: Value = serde_json::from_str(line).unwrap();
            let fields: &[&str] = match value.get("event").and_then(Value::as_str).unwrap() {
                "subagent_spawned" => &[
                    "parent_run_id",
                    "task_brief_hash",
                    "budget_steps",
                    "capabilities",
                    "depth",
                ],
                "subagent_progress" => &["note", "steps_spent"],
                "subagent_finished" => &["result_hash", "steps_spent", "verdict"],
                "subagent_killed" => &["reason", "partial_result_hash", "steps_spent"],
                "subagent_failed" => &["failure", "detail", "steps_spent"],
                other => unreachable!("not a contract event: {other}"),
            };
            let expected: BTreeSet<&str> = common.iter().chain(fields).copied().collect();
            let seen: BTreeSet<&str> = value
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(seen, expected, "{line}");
            serde_json::from_value(value).unwrap()
        })
        .collect()
}

#[test]
fn a_child_runs_in_its_own_cell_with_only_what_it_was_given() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("narrow");
    let mut config = config(&base);
    config.scheduler.model_reasoning = hatchling_core::ModelReasoning::Disabled;
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, false);
    let parent = Parent::open(&config, key, RUN_DEADLINE);
    let task = "vault:app-token vault:old-token subagent model-reasoning";
    let spawned = parent.spawn(task, 4, vec![vault("app-token")]);
    let SubagentResult::Spawned {
        child,
        brief_hash,
        budget,
        capabilities,
    } = spawned
    else {
        unreachable!("not spawned: {spawned:?}");
    };
    assert_eq!(
        (brief_hash.as_str(), budget),
        (sha(task.as_bytes()).as_str(), 4)
    );
    assert_eq!(capabilities, [vault("app-token")]);
    assert!(child.starts_with("run-"), "{child}");
    let report = parent.wait(&child);
    assert_eq!(
        (report.child.as_str(), report.steps_spent),
        (child.as_str(), 4)
    );
    let shown = answer(&report);
    for line in [
        // Its own cell: its id is the child's, its connection its own.
        format!("cell={child} steps=4 subagent=1"),
        String::from("fds=0,1,2,c"),
        String::from("sock=absent"),
        String::from("stdin=not-socket"),
        String::from("model-reasoning=disabled"),
        // Narrowed: what it was given, and nothing the parent kept back.
        String::from("vault:app-token=Vault(NotFound)"),
        String::from("vault:old-token=Vault(Denied)"),
        // The depth bound holds from inside the child.
        String::from("subagent=Subagent(Refused { reason: TooDeep })"),
    ] {
        assert!(
            shown.lines().any(|seen| seen == line),
            "{line:?} not in:\n{shown}"
        );
    }
    // Its report was taken: the child is gone, and so is its cell's dir.
    assert_eq!(
        parent.ask(SubagentOp::Wait { child }),
        SubagentResult::Refused {
            reason: SubagentRefusal::UnknownChild
        }
    );
    assert!(run_dirs(&config).is_empty());
    parent.close();
}

#[test]
fn siblings_cannot_reach_each_other_and_a_kill_keeps_the_childs_last_words() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("siblings");
    let config = config(&base);
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, false);
    let parent = Parent::open(&config, key, RUN_DEADLINE);
    let first = parent.spawned("hang", 2, Vec::new());
    // Where a socket would be, if children had them: they have only their
    // connections (WO-007).
    let sibling_socket = config.scheduler.run_root().join(&first).join("broker.sock");
    assert!(!sibling_socket.exists(), "a child binds no socket");
    let second = parent.spawned(
        &format!("connect:{}", sibling_socket.display()),
        2,
        Vec::new(),
    );
    let report = parent.wait(&second);
    let shown = answer(&report);
    let unreachable_line = format!("connect:{}=NotFound", sibling_socket.display());
    let second_line = format!("cell={second} steps=2 subagent=1");
    assert!(
        shown.lines().any(|line| line == unreachable_line),
        "a sibling's socket must not be reachable:\n{shown}"
    );
    assert!(shown.lines().any(|line| line == second_line));
    // The parent kills the hung child: what it wrote before hanging
    // survives the kill.
    let killed = parent.report(SubagentOp::Kill {
        child: first.clone(),
        reason: String::from("taking too long"),
    });
    let ChildOutcome::Killed {
        reason,
        partial,
        partial_hash,
    } = &killed.outcome
    else {
        unreachable!("not killed: {killed:?}");
    };
    assert_eq!(reason, "taking too long");
    let partial = partial.as_deref().unwrap();
    let first_line = format!("cell={first} steps=2 subagent=1");
    assert!(partial.lines().any(|line| line == first_line));
    assert!(partial_hash.is_some());
    assert_eq!(killed.steps_spent, 2);
    // A kill is recorded with the partial result's hash.
    let recorded = events(&config);
    assert!(recorded.iter().any(|event| matches!(
        &event.kind,
        SubagentEvent::Killed { child_id, partial_result_hash, steps_spent: 2, .. }
            if *child_id == first && partial_result_hash == partial_hash
    )));
    assert!(run_dirs(&config).is_empty());
    parent.close();
}

#[test]
fn widening_is_refused_before_anything_is_spent_and_budgets_are_never_refunded() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("ledger");
    let config = config(&base);
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, false);
    let parent = Parent::open(&config, key, RUN_DEADLINE);
    // A capability the parent does not hold: refused, no cell started.
    assert_eq!(
        parent.spawn("noconnect", MAX_CHILD_STEPS, vec![vault("not-mine")]),
        SubagentResult::Refused {
            reason: SubagentRefusal::ExceedsGrants
        }
    );
    assert!(run_dirs(&config).is_empty());
    assert_eq!(
        parent.spawn("noconnect", MAX_CHILD_STEPS + 1, Vec::new()),
        SubagentResult::Refused {
            reason: SubagentRefusal::BadBudget
        }
    );
    // Two children take the whole ledger, though each ends at once
    // having spent nothing: allocation is spend.
    for _ in 0..2 {
        let child = parent.spawned("noconnect", MAX_CHILD_STEPS, Vec::new());
        assert_eq!(parent.wait(&child).steps_spent, MAX_CHILD_STEPS);
    }
    assert_eq!(
        parent.spawn("noconnect", 1, Vec::new()),
        SubagentResult::Refused {
            reason: SubagentRefusal::LedgerSpent
        }
    );
    parent.close();
}

#[test]
fn a_child_that_times_out_or_fails_is_a_typed_failure_and_the_parent_carries_on() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("failures");
    let config = config(&base);
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, false);
    let parent = Parent::open(&config, key, Duration::from_secs(3));
    let hung = parent.spawned("hang", 3, Vec::new());
    let report = parent.wait(&hung);
    assert_eq!(
        report.outcome,
        ChildOutcome::Failed {
            failure: ChildFailure::Timeout,
            detail: String::from("the child ran past its time bound"),
        }
    );
    // A child whose report breaks the output bound failed its cell.
    let flooded = parent.spawned("flood:20000", 3, Vec::new());
    assert!(matches!(
        parent.wait(&flooded).outcome,
        ChildOutcome::Failed {
            failure: ChildFailure::CellFailed,
            ..
        }
    ));
    // The parent carries on: its next child runs and reports.
    let next = parent.spawned("envelope:still here", 3, Vec::new());
    let finished = parent.wait(&next);
    assert_eq!(finished.steps_spent, 2);
    assert!(matches!(
        &finished.outcome,
        ChildOutcome::Finished { verdict: ChildVerdict::Ok, answer, .. } if answer == "still here"
    ));
    let recorded = events(&config);
    assert!(recorded.iter().any(|event| matches!(
        &event.kind,
        SubagentEvent::Failed { child_id, failure: ChildFailure::Timeout, steps_spent: 3, .. }
            if *child_id == hung
    )));
    parent.close();
}

#[test]
fn an_answer_that_would_expose_a_secret_never_reaches_the_parent() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("withheld");
    let config = config(&base);
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, true);
    let parent = Parent::open(&config, key, RUN_DEADLINE);
    let child = parent.spawned("vault:app-token", 2, vec![vault("app-token")]);
    let report = parent.wait(&child);
    assert_eq!(answer(&report), hatchling_broker::WITHHELD);
    // The hash of what it really returned is still on the record.
    let ChildOutcome::Finished { result_hash, .. } = &report.outcome else {
        unreachable!();
    };
    assert!(events(&config).iter().any(|event| matches!(
        &event.kind,
        SubagentEvent::Finished { result_hash: recorded, .. } if recorded == result_hash
    )));
    parent.close();
}

#[test]
fn when_the_parent_goes_away_its_children_are_reaped_and_recorded() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("orphans");
    let config = config(&base);
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, false);
    let parent = Parent::open(&config, key, RUN_DEADLINE);
    let first = parent.spawned("hang", 2, Vec::new());
    let second = parent.spawned("hang", 2, Vec::new());
    // The session ends with both running, well inside their own bound.
    // (A child killed before it wrote anything leaves no partial result:
    // the siblings test covers a partial result surviving a kill.)
    assert!(parent.close() < RUN_DEADLINE);
    assert!(run_dirs(&config).is_empty(), "no child outlives its parent");
    let recorded = events(&config);
    for child in [&first, &second] {
        assert!(recorded.iter().any(|event| matches!(
            &event.kind,
            SubagentEvent::Killed { child_id, reason, .. }
                if child_id == child && reason == ORPHANED
        )));
    }
}

#[test]
fn every_lifecycle_event_matches_the_contract() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("events");
    let config = config(&base);
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, false);
    let parent = Parent::open(&config, key, RUN_DEADLINE);
    let task = "envelope:done";
    let child = parent.spawned(task, 3, vec![vault("app-token")]);
    let report = parent.wait(&child);
    parent.close();
    let recorded = events(&config);
    assert_eq!(recorded.len(), 2, "{recorded:?}");
    let run_id = recorded.first().unwrap().run_id.clone();
    assert!(recorded.iter().all(|event| event.run_id == run_id));
    let ids: BTreeSet<&str> = recorded.iter().map(|e| e.event_id.as_str()).collect();
    assert_eq!(ids.len(), 2, "event ids are unique");
    assert!(
        recorded
            .iter()
            .all(|event| event.ts.parse::<hatchling_scheduler::Timestamp>().is_ok())
    );
    let ChildOutcome::Finished { result_hash, .. } = &report.outcome else {
        unreachable!();
    };
    assert_eq!(
        recorded.iter().map(|e| e.kind.clone()).collect::<Vec<_>>(),
        [
            SubagentEvent::Spawned {
                child_id: child.clone(),
                parent_run_id: run_id,
                task_brief_hash: sha(task.as_bytes()),
                budget_steps: 3,
                capabilities: vec![String::from("vault:fetch:app-token")],
                depth: 1,
            },
            SubagentEvent::Finished {
                child_id: child,
                result_hash: result_hash.clone(),
                steps_spent: 2,
                verdict: ChildVerdict::Ok,
            },
        ]
    );
    // The log is the owner's alone.
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(config.scheduler.activity_log())
            .unwrap()
            .permissions(),
    );
    assert_eq!(mode & 0o777, 0o600);
}

/// A scripted parent model: spawn a child with `task` and `budget`, take
/// its report, then answer with what the child said.
struct Delegating {
    task: String,
    budget: u32,
}

impl hatchling_loop::Model for Delegating {
    fn next_action(
        &self,
        messages: &[hatchling_loop::Message],
        _tools: &[hatchling_loop::ToolDescription],
    ) -> Result<hatchling_loop::ModelAction, hatchling_loop::Error> {
        let results: Vec<&Value> = messages
            .iter()
            .filter_map(|message| {
                if let hatchling_loop::Message::ToolUse { results, .. } = message {
                    results.first()
                } else {
                    None
                }
            })
            .filter_map(|output| match output {
                hatchling_loop::ToolOutput::Ok(value) => Some(value),
                hatchling_loop::ToolOutput::Err(_) => None,
            })
            .collect();
        let call = |name: &str, arguments: Value| {
            hatchling_loop::ModelAction::ToolCalls(vec![hatchling_loop::ToolCall {
                name: name.to_owned(),
                arguments,
            }])
        };
        Ok(match results.as_slice() {
            [] => call(
                "spawn_subagent",
                json!({"task": self.task, "budget": self.budget}),
            ),
            [spawned] => call(
                "subagent_result",
                json!({"child": spawned.get("child").unwrap()}),
            ),
            [.., report] => hatchling_loop::ModelAction::Text(format!(
                "the child said: {}",
                report
                    .pointer("/outcome/answer")
                    .and_then(Value::as_str)
                    .unwrap_or("nothing")
            )),
        })
    }
}

/// A real agent cell's session: the agent library connects over a
/// socket path to the real session code.
fn agent_parent(
    config: &Config,
    key: MasterKey,
    base: &Path,
) -> (
    hatchling_agent::Cell<hatchling_agent::Ready>,
    JoinHandle<()>,
) {
    let socket = base.join("parent.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let session_config = config.clone();
    let runs = env(config, RUN_DEADLINE);
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = serve_connection_with_runs(
            &stream,
            &session_config,
            &key,
            &Skills::default(),
            None,
            Some(runs),
        );
    });
    let agent = hatchling_agent::Config {
        cell_id: "cell-parent".parse().unwrap(),
        broker: hatchling_agent::BrokerAddress::Socket(socket.to_string_lossy().parse().unwrap()),
        broker_uid: PeerUid::from_kernel(hatchling_sys::euid()),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        run_subagent: false,
        run_chat: false,
        run_history: Vec::new(),
    };
    let cell = hatchling_agent::Cell::new(agent)
        .check_preconditions()
        .unwrap()
        .connect_broker()
        .unwrap()
        .authorize()
        .unwrap()
        .handshake()
        .unwrap();
    (cell, server)
}

#[test]
fn a_parent_agent_delegates_through_its_tools_and_gets_the_childs_answer() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("agent");
    let config = config(&base);
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, false);
    let (cell, server) = agent_parent(&config, key, &base);
    let model = Delegating {
        task: String::from("envelope:the build is green"),
        budget: 3,
    };
    let result = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("check the build"),
            hatchling_loop::LoopBudget { max_steps: 10 },
        )
        .unwrap();
    assert_eq!(result.answer, "the child said: the build is green");
    assert_eq!(result.steps, 3);
    drop(cell);
    join_within(server);
    let recorded = events(&config);
    assert!(matches!(
        recorded.as_slice(),
        [
            ActivityEvent {
                kind: SubagentEvent::Spawned {
                    budget_steps: 3,
                    ..
                },
                ..
            },
            ActivityEvent {
                kind: SubagentEvent::Finished { steps_spent: 2, .. },
                ..
            },
        ]
    ));
}

#[test]
fn a_parent_that_spends_its_budget_on_a_child_ends_and_its_child_is_reaped() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("exhaust");
    let config = config(&base);
    let key = MasterKey::generate().unwrap();
    seed(&config, &key, false);
    let (cell, server) = agent_parent(&config, key, &base);
    // Four steps: one turn to spawn, three to the child. Nothing is left
    // for a second turn, so the parent ends there, and its child with it.
    let model = Delegating {
        task: String::from("hang"),
        budget: 3,
    };
    let started = Instant::now();
    let error = cell
        .run_loop(
            hatchling_agent::PromptKind::Chat,
            &model,
            String::from("delegate"),
            hatchling_loop::LoopBudget { max_steps: 4 },
        )
        .unwrap_err();
    assert!(
        matches!(error, hatchling_loop::Error::BudgetExhausted { steps: 1 }),
        "{error:?}"
    );
    assert!(
        started.elapsed() < RUN_DEADLINE,
        "the child was killed, not waited for"
    );
    assert!(run_dirs(&config).is_empty(), "no child outlives its parent");
    drop(cell);
    join_within(server);
    assert!(events(&config).iter().any(|event| matches!(
        &event.kind,
        SubagentEvent::Killed { reason, steps_spent: 3, .. } if reason == ORPHANED
    )));
}
