//! Component: schedules through a real broker session, the
//! real sealed store on disk, and the dispatcher delivering through the
//! real local-channel hub.
//!
//! Time is fixed: every op and tick is given its `now`. Runs are the
//! test's own, finishing when the test says.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hatchling_broker::{
    Config, EgressConfig, GoalsAction, GoalsConfig, MemoryAction, MemoryConfig, MemoryGrant,
    SchedulerConfig, SealedScheduleStore, VaultConfig, VaultGrant, dispatch_once,
    handle_schedule_op, held_grants, serve_connection,
};
use hatchling_channel::{LocalHub, Message};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, LogLevel, PROTOCOL_VERSION, PeerUid, ScheduleGrant, ScheduleKind,
    ScheduleOp, ScheduleRefusal, ScheduleResult, ScheduleSpec, SessionRequest, SessionResponse,
    VaultAction,
};
use hatchling_scheduler::{Dispatcher, Launcher, Run, RunRequest, Store, Timestamp};
use hatchling_vault::MasterKey;

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hatchling-test-schedule-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    // The store's own root is created owner-only on first use; its
    // parent, like the vault's, must already exist.
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A broker config with a scheduler store at `name` (or none), and a
/// grant of every kind.
fn config(name: &str, scheduler: bool) -> Config {
    let base = root(name);
    let vault_root = base.join("vault");
    Config {
        socket: "/tmp/hatchling-test-schedule-dummy.sock".parse().unwrap(),
        agent_uid: PeerUid::from_kernel(real_uid()),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: if scheduler {
            SchedulerConfig {
                root: base.join("schedules"),
                ..SchedulerConfig::disabled()
            }
        } else {
            SchedulerConfig::disabled()
        },
        log_level: LogLevel::Error,
        vault: VaultConfig {
            key_file: VaultConfig::default_key_file(&vault_root),
            root: vault_root,
            grants: vec![VaultGrant {
                action: VaultAction::Fetch,
                name: "app-token".parse().unwrap(),
            }],
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: vec![String::from("api.example.com:443")],
        },
        channels: vec![String::from("local")],
        channel_accounts: Vec::new(),
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: MemoryConfig {
            root: PathBuf::from("/nonexistent/hatchling-memory"),
            grants: vec![MemoryGrant {
                scope: String::from("user"),
                action: MemoryAction::Recall,
            }],
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
        goals: GoalsConfig {
            root: PathBuf::from("/nonexistent/hatchling-goals"),
            grants: vec![GoalsAction::GoalCreate],
        },
    }
}

fn cleanup(config: &Config) {
    if let Some(base) = config.vault.root.parent() {
        let _ = std::fs::remove_dir_all(base);
    }
}

fn ts(rfc3339: &str) -> Timestamp {
    rfc3339.parse().unwrap()
}

fn send() -> ScheduleGrant {
    ScheduleGrant::ChannelSend {
        channel: String::from("local"),
    }
}

fn egress() -> ScheduleGrant {
    ScheduleGrant::Egress {
        destination: String::from("api.example.com:443"),
    }
}

fn hourly(tz: Option<&str>) -> ScheduleSpec {
    ScheduleSpec::Cron {
        expr: String::from("0 * * * *"),
        tz: tz.unwrap_or_default().to_owned(),
    }
}

fn create(when: ScheduleSpec, grants: Vec<ScheduleGrant>) -> ScheduleOp {
    ScheduleOp::Create {
        when,
        task: String::from("check the build and report"),
        grants,
        channel: String::from("local"),
    }
}

/// A real session: the broker's own pipeline on a thread.
fn session(config: &Config) -> UnixStream {
    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let config = config.clone();
    std::thread::spawn(move || {
        let key = MasterKey::generate().unwrap();
        let _ = serve_connection(&server, &config, &key, &hatchling_broker::Skills::default());
    });
    let hello = AgentMessage::Hello {
        v: PROTOCOL_VERSION,
        cell_id: String::from("cell-schedule"),
    };
    write_frame(&client, &serde_json::to_vec(&hello).unwrap()).unwrap();
    let welcome: BrokerMessage = serde_json::from_slice(&read_frame(&client).unwrap()).unwrap();
    assert!(
        matches!(welcome, BrokerMessage::Welcome { .. }),
        "{welcome:?}"
    );
    client
}

fn op(stream: &UnixStream, op: ScheduleOp) -> io::Result<ScheduleResult> {
    let request = SessionRequest::Schedule(op);
    write_frame(stream, &serde_json::to_vec(&request).unwrap()).map_err(io::Error::other)?;
    let reply: SessionResponse =
        serde_json::from_slice(&read_frame(stream).map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
    match reply {
        SessionResponse::Schedule(result) => Ok(result),
        other @ (SessionResponse::Vault(_)
        | SessionResponse::Egress(_)
        | SessionResponse::Channel(_)
        | SessionResponse::Memory(_)
        | SessionResponse::Skill(_)
        | SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_)
        | SessionResponse::Browser(_)) => {
            Err(io::Error::other(format!("not a schedule reply: {other:?}")))
        }
    }
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                files(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

#[test]
fn a_real_session_creates_and_cancels_but_never_reads_back() {
    let config = config("session", true);
    let client = session(&config);
    let id = created_id(op(&client, create(hourly(Some("UTC")), vec![send(), egress()])).unwrap());
    assert!(id.starts_with("sch-") && id.len() == 68, "{id}");
    // Sealed at rest: no stored byte carries the task text.
    let stored = files(&config.scheduler.root);
    assert!(!stored.is_empty());
    for path in &stored {
        let bytes = std::fs::read(path).unwrap();
        assert!(
            !bytes
                .windows(b"check the build".len())
                .any(|w| w == b"check the build"),
            "plaintext in {}",
            path.display()
        );
    }
    assert_eq!(
        op(&client, ScheduleOp::Cancel { id: id.clone() }).unwrap(),
        ScheduleResult::Cancelled
    );
    assert_eq!(
        op(&client, ScheduleOp::Cancel { id }).unwrap(),
        ScheduleResult::NotFound
    );
    for forged in ["sch-deadbeef", "../index", ""] {
        assert_eq!(
            op(
                &client,
                ScheduleOp::Cancel {
                    id: String::from(forged)
                }
            )
            .unwrap(),
            ScheduleResult::NotFound,
            "{forged:?}"
        );
    }
    cleanup(&config);
}

#[test]
fn creation_is_refused_for_each_fault() {
    let config = config("refused", true);
    let client = session(&config);
    let evil = ScheduleGrant::Egress {
        destination: String::from("evil.example:443"),
    };
    for (request, refusal) in [
        (
            create(hourly(Some("UTC")), vec![send(), evil]),
            ScheduleRefusal::ExceedsGrants,
        ),
        (
            create(hourly(None), vec![send()]),
            ScheduleRefusal::MissingTimezone,
        ),
        (
            create(hourly(Some("Mars/Olympus")), vec![send()]),
            ScheduleRefusal::UnknownTimezone,
        ),
        (
            create(
                ScheduleSpec::Cron {
                    expr: String::from("99 * * * *"),
                    tz: String::from("UTC"),
                },
                vec![send()],
            ),
            ScheduleRefusal::InvalidSpec,
        ),
        (
            create(
                ScheduleSpec::Once {
                    at: String::from("2020-01-01T00:00"),
                    tz: String::from("UTC"),
                },
                vec![send()],
            ),
            ScheduleRefusal::NotInFuture,
        ),
        (
            create(hourly(Some("UTC")), vec![egress()]),
            ScheduleRefusal::ChannelNotGranted,
        ),
    ] {
        assert_eq!(
            op(&client, request.clone()).unwrap(),
            ScheduleResult::Refused { reason: refusal },
            "{request:?}"
        );
    }
    assert!(
        files(&config.scheduler.root).is_empty(),
        "nothing refused was stored"
    );
    cleanup(&config);
}

#[test]
fn scheduling_is_denied_without_a_store() {
    let config = config("denied", false);
    let client = session(&config);
    assert_eq!(
        op(&client, create(hourly(Some("UTC")), vec![send()])).unwrap(),
        ScheduleResult::Denied
    );
    assert_eq!(
        op(
            &client,
            ScheduleOp::Cancel {
                id: String::from("sch-1")
            }
        )
        .unwrap(),
        ScheduleResult::Denied
    );
    cleanup(&config);
}

#[test]
fn the_creator_holds_exactly_the_broker_s_grants() {
    let config = config("held", true);
    assert_eq!(
        held_grants(&config),
        BTreeSet::from([
            ScheduleGrant::Vault {
                action: String::from("fetch"),
                name: String::from("app-token"),
            },
            egress(),
            ScheduleGrant::Memory {
                scope: String::from("user"),
                action: String::from("recall"),
            },
            ScheduleGrant::Goals {
                action: String::from("goal-create"),
            },
            send(),
        ])
    );
    cleanup(&config);
}

#[test]
fn the_store_holds_at_most_64_schedules() {
    let config = config("full", true);
    let key = MasterKey::generate().unwrap();
    let now = ts("2026-09-21T12:00:00Z");
    for n in 0..hatchling_broker::schedule_ops::MAX_SCHEDULES {
        assert!(
            matches!(
                agent_schedule_op(
                    &config,
                    &key,
                    &create(hourly(Some("UTC")), vec![send()]),
                    now
                ),
                ScheduleResult::Created { .. }
            ),
            "schedule {n}"
        );
    }
    assert_eq!(
        agent_schedule_op(
            &config,
            &key,
            &create(hourly(Some("UTC")), vec![send()]),
            now
        ),
        ScheduleResult::Refused {
            reason: ScheduleRefusal::Full
        }
    );
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    assert_eq!(store.load().unwrap().len(), 64);
    cleanup(&config);
}

/// A run whose outcome the test sets.
#[derive(Clone, Default)]
struct FakeRun(Rc<RefCell<Option<Result<String, String>>>>);

impl Run for FakeRun {
    fn poll(&mut self) -> Option<Result<String, String>> {
        self.0.borrow_mut().take()
    }
}

#[derive(Default)]
struct FakeLauncher {
    requests: RefCell<Vec<RunRequest>>,
    runs: RefCell<Vec<FakeRun>>,
}

impl Launcher for FakeLauncher {
    type Run = FakeRun;

    fn launch(&self, request: RunRequest) -> Result<FakeRun, String> {
        self.requests.borrow_mut().push(request);
        let run = FakeRun::default();
        self.runs.borrow_mut().push(run.clone());
        Ok(run)
    }
}

impl FakeLauncher {
    fn finish(&self, index: usize, output: &str) {
        let run = self.runs.borrow().get(index).cloned().unwrap();
        *run.0.borrow_mut() = Some(Ok(output.to_owned()));
    }
}

/// A hub with a client attached, recording what it receives.
fn hub_with_client() -> (LocalHub, Arc<Mutex<Vec<Message>>>) {
    let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let _ = hub.attach(Box::new(move |m: &hatchling_channel::Cleared| {
        sink.lock().unwrap().push(m.message().clone());
        Ok(())
    }));
    (hub, received)
}

fn created(config: &Config, key: &MasterKey, grants: Vec<ScheduleGrant>, now: &str) -> String {
    created_id(agent_schedule_op(
        config,
        key,
        &create(hourly(Some("UTC")), grants),
        ts(now),
    ))
}

#[test]
fn a_list_pages_through_every_schedule_in_id_order_without_its_task() {
    let config = config("list", true);
    let key = MasterKey::generate().unwrap();
    let now = ts("2026-10-10T08:00:00Z");
    let mut ids: Vec<String> = (0..3)
        .map(|_| {
            created_id(agent_schedule_op(
                &config,
                &key,
                &create(hourly(Some("UTC")), vec![send()]),
                now,
            ))
        })
        .collect();
    ids.sort();
    let list = |after: Option<&str>| {
        agent_schedule_op(
            &config,
            &key,
            &ScheduleOp::List {
                after: after.map(str::to_owned),
            },
            now,
        )
    };
    let ScheduleResult::Listed { schedules, next } = list(None) else {
        unreachable!("not listed");
    };
    assert_eq!(next, None, "three schedules fit one page");
    assert_eq!(
        schedules
            .iter()
            .map(|shown| shown.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    for shown in &schedules {
        assert_eq!(shown.kind, ScheduleKind::Task);
        assert_eq!(shown.when, hourly(Some("UTC")));
        assert_eq!(shown.channel, "local");
        assert_eq!(shown.grants, [send()]);
        assert_eq!(shown.next_due, ts("2026-10-10T09:00:00Z").as_second());
    }
    let shown = serde_json::to_string(&schedules).unwrap();
    assert!(!shown.contains("check the build"), "no task text: {shown}");
    // Resuming after a schedule lists only those past it.
    let ScheduleResult::Listed { schedules, .. } = list(ids.first().map(String::as_str)) else {
        unreachable!("not listed");
    };
    assert_eq!(
        schedules
            .into_iter()
            .map(|shown| shown.id)
            .collect::<Vec<_>>(),
        ids.get(1..).unwrap()
    );
    let ScheduleResult::Listed { schedules, .. } = list(ids.last().map(String::as_str)) else {
        unreachable!("not listed");
    };
    assert!(schedules.is_empty());
    cleanup(&config);
    // No scheduler, no list.
    let off = self::config("list-off", false);
    assert_eq!(
        agent_schedule_op(&off, &key, &ScheduleOp::List { after: None }, now),
        ScheduleResult::Denied
    );
    cleanup(&off);
}

#[test]
fn a_missing_timezone_is_still_refused_by_name() {
    let config = config("tz", true);
    let key = MasterKey::generate().unwrap();
    for when in [
        r#"{"every":"cron","expr":"0 * * * *"}"#,
        r#"{"every":"cron","expr":"0 * * * *","tz":null}"#,
        r#"{"every":"once","at":"2026-12-01T09:00"}"#,
        r#"{"every":"once","at":"2026-12-01T09:00","tz":"  "}"#,
    ] {
        let when: ScheduleSpec = serde_json::from_str(when).unwrap();
        assert_eq!(
            agent_schedule_op(
                &config,
                &key,
                &create(when, vec![send()]),
                ts("2026-10-10T08:00:00Z")
            ),
            ScheduleResult::Refused {
                reason: ScheduleRefusal::MissingTimezone
            }
        );
    }
    cleanup(&config);
}

/// The created id, or a description of what came back instead (which
/// the caller's assertions then reject).
fn created_id(result: ScheduleResult) -> String {
    match result {
        ScheduleResult::Created { id } => id,
        other @ (ScheduleResult::Cancelled
        | ScheduleResult::NotFound
        | ScheduleResult::Listed { .. }
        | ScheduleResult::Refused { .. }
        | ScheduleResult::Denied
        | ScheduleResult::ScheduleError
        | ScheduleResult::Resolved
        | ScheduleResult::Retried
        | ScheduleResult::Parked { .. }
        | ScheduleResult::Dismissed) => format!("not created: {other:?}"),
    }
}

#[test]
fn a_due_schedule_fires_and_its_output_reaches_the_local_channel() {
    let config = config("deliver", true);
    let key = MasterKey::generate().unwrap();
    let id = created(
        &config,
        &key,
        vec![send(), egress()],
        "2026-09-21T12:10:00Z",
    );
    let (hub, received) = hub_with_client();
    let launcher = FakeLauncher::default();
    let mut dispatcher = Dispatcher::default();
    // Not yet due.
    let early = dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T12:59:59Z"),
    )
    .unwrap();
    assert!(early.launched.is_empty());
    // Due at 13:00.
    let report = dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T13:00:02Z"),
    )
    .unwrap();
    assert_eq!(report.launched, [(id.clone(), false)]);
    let request = launcher.requests.borrow().first().cloned().unwrap();
    assert_eq!(request.task, "check the build and report");
    assert_eq!(request.grants, [send(), egress()]);
    launcher.finish(0, "build is green");
    let report = dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T13:01:00Z"),
    )
    .unwrap();
    assert_eq!(report.delivered, [id]);
    let received = received.lock().unwrap().clone();
    assert_eq!(received.len(), 1);
    let message = received.first().unwrap();
    assert_eq!(message.text, "build is green");
    assert_eq!(message.sender.0, "broker:scheduler", "the broker's stamp");
    assert_eq!(message.channel.0, "local");
    cleanup(&config);
}

#[test]
fn a_restarted_broker_fires_a_missed_schedule_once_from_the_sealed_store() {
    let config = config("restart", true);
    let key = MasterKey::generate().unwrap();
    let id = created(&config, &key, vec![send()], "2026-09-21T12:10:00Z");
    // A fresh dispatcher after downtime, reading only the store.
    let (hub, _received) = hub_with_client();
    let launcher = FakeLauncher::default();
    let mut dispatcher = Dispatcher::default();
    let report = dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T18:30:00Z"),
    )
    .unwrap();
    assert_eq!(report.launched, [(id, true)], "once, flagged missed");
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    let stored = store.load().unwrap();
    assert_eq!(
        stored
            .first()
            .and_then(hatchling_scheduler::Schedule::due)
            .map(|t| t.to_string()),
        Some(String::from("2026-09-21T19:00:00Z")),
        "the cadence resumes, persisted"
    );
    cleanup(&config);
}

#[test]
fn a_grant_revoked_in_the_config_is_withheld_at_fire_time() {
    let mut config = config("revoke", true);
    let key = MasterKey::generate().unwrap();
    created(
        &config,
        &key,
        vec![send(), egress()],
        "2026-09-21T12:10:00Z",
    );
    // The operator revokes the egress grant before the fire.
    config.egress.grants.clear();
    let (hub, _received) = hub_with_client();
    let launcher = FakeLauncher::default();
    let mut dispatcher = Dispatcher::default();
    let report = dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T13:00:00Z"),
    )
    .unwrap();
    assert_eq!(report.revoked.len(), 1);
    let request = launcher.requests.borrow().first().cloned().unwrap();
    assert_eq!(
        request.grants,
        [send()],
        "the run cannot use the revoked grant"
    );
    cleanup(&config);
}

#[test]
fn undeliverable_output_is_parked_in_the_sealed_store() {
    let config = config("park", true);
    let key = MasterKey::generate().unwrap();
    let id = created(&config, &key, vec![send()], "2026-09-21T12:10:00Z");
    // A hub with no client: every delivery fails.
    let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
    let launcher = FakeLauncher::default();
    let mut dispatcher = Dispatcher::default();
    dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T13:00:00Z"),
    )
    .unwrap();
    launcher.finish(0, "nobody to tell");
    let report = dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T13:01:00Z"),
    )
    .unwrap();
    assert_eq!(report.parked, std::slice::from_ref(&id));
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    let parked = store.parked().unwrap();
    assert_eq!(parked.len(), 1);
    let first = parked.first().unwrap();
    assert_eq!(first.schedule_id, id);
    assert_eq!(first.text, "nobody to tell");
    assert!(
        first.reason.contains("no local ingress client"),
        "{}",
        first.reason
    );
    // No ingress at all: the same.
    let config2 = self::config("park-none", true);
    let id2 = created(&config2, &key, vec![send()], "2026-09-21T12:10:00Z");
    let mut dispatcher = Dispatcher::default();
    let launcher = FakeLauncher::default();
    dispatch_once(
        &mut dispatcher,
        &config2,
        &key,
        None,
        &launcher,
        ts("2026-09-21T13:00:00Z"),
    )
    .unwrap();
    launcher.finish(0, "x");
    let report = dispatch_once(
        &mut dispatcher,
        &config2,
        &key,
        None,
        &launcher,
        ts("2026-09-21T13:01:00Z"),
    )
    .unwrap();
    assert_eq!(report.parked, [id2]);
    cleanup(&config);
    cleanup(&config2);
}

#[test]
fn a_disabled_scheduler_dispatches_nothing() {
    let config = config("off", false);
    let key = MasterKey::generate().unwrap();
    let launcher = FakeLauncher::default();
    let mut dispatcher = Dispatcher::default();
    let report = dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        None,
        &launcher,
        ts("2026-09-21T13:00:00Z"),
    )
    .unwrap();
    assert_eq!(report, hatchling_scheduler::TickReport::default());
    cleanup(&config);
}

/// A schedule op as an agent session sends it: every one these tests
/// make.
fn agent_schedule_op(
    config: &Config,
    key: &MasterKey,
    op: &ScheduleOp,
    now: Timestamp,
) -> ScheduleResult {
    handle_schedule_op(config, key, op, now, hatchling_scheduler::Creator::Agent)
}
