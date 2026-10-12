//! Component: hooks and follow-ups through real broker
//! sessions, the real sealed store on disk, and the dispatcher firing
//! through the real local-channel hub.
//!
//! Time is fixed: every op and tick is given its `now`. Runs are the
//! test's own, finishing when the test says.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::cell::RefCell;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use hatchling_broker::{
    Config, EgressConfig, MemoryConfig, SchedulerConfig, SealedScheduleStore, VaultConfig,
    VaultGrant, dispatch_once, handle_hook_op, handle_schedule_op, scoped_config, serve_connection,
};
use hatchling_channel::{ChannelId, LocalHub, Message, SenderId};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, HookOp, HookResult, HookSource, LogLevel, PROTOCOL_VERSION,
    PeerUid, ScheduleGrant, ScheduleOp, ScheduleRefusal, ScheduleResult, ScheduleSpec,
    SessionRequest, SessionResponse, VaultAction,
};
use hatchling_scheduler::{
    Dispatcher, Launcher, MAX_FOLLOWUP_DEPTH, Run, RunRequest, Store, Timestamp,
};
use hatchling_vault::MasterKey;

/// One master key for the process: the broker has exactly one, and
/// sessions borrow it. Not `Clone`, by design, so tests share this one.
fn key() -> &'static MasterKey {
    static KEY: OnceLock<MasterKey> = OnceLock::new();
    KEY.get_or_init(|| MasterKey::generate().unwrap())
}

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A scratch root, removed when the test ends, pass or fail.
struct Base(PathBuf);

impl Drop for Base {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn base(name: &str) -> Base {
    let root = std::env::temp_dir().join(format!(
        "hatchling-test-hooks-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    Base(root)
}

/// A broker config with a scheduler store under `base`, a vault and an
/// egress grant, and (when `ingress`) the local channel's ingress.
fn config(base: &Base, ingress: bool) -> Config {
    let vault_root = base.0.join("vault");
    Config {
        socket: "/tmp/hatchling-test-hooks-dummy.sock".parse().unwrap(),
        agent_uid: PeerUid::from_kernel(real_uid()),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: SchedulerConfig {
            root: base.0.join("schedules"),
            ..SchedulerConfig::disabled()
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
        channel_ingress: ingress.then(|| "/tmp/hatchling-test-hooks-ingress.sock".parse().unwrap()),
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: MemoryConfig {
            root: PathBuf::from("/nonexistent/hatchling-memory"),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        goals: hatchling_broker::GoalsConfig::disabled(),
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

fn vault() -> ScheduleGrant {
    ScheduleGrant::Vault {
        action: String::from("fetch"),
        name: String::from("app-token"),
    }
}

fn message(channel: &str) -> HookSource {
    HookSource::ChannelMessage {
        channel: channel.to_owned(),
    }
}

fn hook(source: HookSource, grants: Vec<ScheduleGrant>) -> HookOp {
    HookOp::Create {
        source,
        task: String::from("triage what arrived"),
        grants,
        channel: String::from("local"),
    }
}

/// A real session: the broker's own pipeline on a thread, under `config`
/// and the process's key.
fn session(config: &Config) -> UnixStream {
    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let config = config.clone();
    std::thread::spawn(move || {
        let _ = serve_connection(
            &server,
            &config,
            key(),
            &hatchling_broker::Skills::default(),
        );
    });
    let hello = AgentMessage::Hello {
        v: PROTOCOL_VERSION,
        cell_id: String::from("cell-hooks"),
    };
    write_frame(&client, &serde_json::to_vec(&hello).unwrap()).unwrap();
    let welcome: BrokerMessage = serde_json::from_slice(&read_frame(&client).unwrap()).unwrap();
    assert!(
        matches!(welcome, BrokerMessage::Welcome { .. }),
        "{welcome:?}"
    );
    client
}

fn ask(stream: &UnixStream, request: &SessionRequest) -> io::Result<SessionResponse> {
    write_frame(stream, &serde_json::to_vec(request).unwrap()).map_err(io::Error::other)?;
    serde_json::from_slice(&read_frame(stream).map_err(io::Error::other)?).map_err(io::Error::other)
}

fn hook_op(stream: &UnixStream, op: HookOp) -> HookResult {
    let reply = ask(stream, &SessionRequest::Hook(op)).unwrap();
    let SessionResponse::Hook(result) = reply else {
        return HookResult::Refused {
            reason: ScheduleRefusal::InvalidSpec,
        };
    };
    result
}

fn schedule_op(stream: &UnixStream, op: ScheduleOp) -> ScheduleResult {
    let reply = ask(stream, &SessionRequest::Schedule(op)).unwrap();
    let SessionResponse::Schedule(result) = reply else {
        return ScheduleResult::ScheduleError;
    };
    result
}

fn followup(task: &str, delay_secs: u64, grants: Vec<ScheduleGrant>) -> ScheduleOp {
    ScheduleOp::Followup {
        task: task.to_owned(),
        delay_secs,
        grants,
    }
}

/// A run's own session config, as the launcher builds it.
fn run_config(config: &Config, request: &RunRequest, token: &str) -> Config {
    let socket = "/tmp/hatchling-test-hooks-run.sock".parse().unwrap();
    scoped_config(config, request, token, socket)
}

fn run_request(grants: Vec<ScheduleGrant>, depth: u32) -> RunRequest {
    RunRequest {
        schedule_id: String::from("sch-parent"),
        task: String::from("the parent's task"),
        grants,
        due: ts("2026-09-21T12:00:00Z"),
        missed: false,
        channel: String::from("local"),
        depth,
        event: None,
        space: None,
        feed: None,
    }
}

/// A unique run token per call, as the launcher mints them.
fn token(tag: &str) -> String {
    format!("run-{tag}-{}", std::process::id())
}

fn hook_id(result: &HookResult) -> String {
    match result {
        HookResult::Created { id } => id.clone(),
        other @ (HookResult::Cancelled
        | HookResult::NotFound
        | HookResult::Refused { .. }
        | HookResult::Denied
        | HookResult::HookError) => format!("not created: {other:?}"),
    }
}

fn schedule_id(result: &ScheduleResult) -> String {
    match result {
        ScheduleResult::Created { id } => id.clone(),
        other @ (ScheduleResult::Cancelled
        | ScheduleResult::NotFound
        | ScheduleResult::Listed { .. }
        | ScheduleResult::Refused { .. }
        | ScheduleResult::Denied
        | ScheduleResult::ScheduleError) => format!("not created: {other:?}"),
    }
}

#[test]
fn hooks_are_registered_and_cancelled_through_the_peer_session() {
    let base = base("lifecycle");
    let config = config(&base, true);
    let key = key();
    let client = session(&config);
    let id = hook_id(&hook_op(
        &client,
        hook(message("local"), vec![send(), egress()]),
    ));
    assert!(id.starts_with("hk-"), "{id}");
    let store = SealedScheduleStore::new(&config.scheduler.root, key);
    let stored = store.hooks().unwrap();
    assert_eq!(stored.len(), 1);
    let first = stored.first().unwrap();
    assert_eq!(first.id, id);
    assert_eq!(first.source, message("local"));
    assert_eq!(first.grants, [send(), egress()]);
    // A hook on a schedule that exists.
    let schedule = schedule_id(&schedule_op(
        &client,
        ScheduleOp::Create {
            when: ScheduleSpec::Cron {
                expr: String::from("0 * * * *"),
                tz: String::from("UTC"),
            },
            task: String::from("t"),
            grants: vec![send()],
            channel: String::from("local"),
        },
    ));
    let on_schedule = hook_id(&hook_op(
        &client,
        hook(
            HookSource::ScheduleRunFinished {
                schedule_id: schedule.clone(),
            },
            vec![send()],
        ),
    ));
    assert!(on_schedule.starts_with("hk-"), "{on_schedule}");
    assert_eq!(store.hooks().unwrap().len(), 2);
    assert_eq!(
        hook_op(&client, HookOp::Cancel { id: id.clone() }),
        HookResult::Cancelled
    );
    assert_eq!(
        hook_op(&client, HookOp::Cancel { id }),
        HookResult::NotFound
    );
    // A schedule id is not a hook id, and cancelling one leaves it be.
    assert_eq!(
        hook_op(&client, HookOp::Cancel { id: schedule }),
        HookResult::NotFound
    );
    assert_eq!(store.load().unwrap().len(), 1);
    assert_eq!(store.hooks().unwrap().len(), 1);
}

#[test]
fn a_hook_on_a_source_the_broker_cannot_authenticate_is_refused() {
    let base = base("unknown");
    let key = key();
    let refused = |reason| HookResult::Refused { reason };
    // No ingress: the local channel's messages are not authenticated.
    let no_ingress = config(&base, false);
    let client = session(&no_ingress);
    assert_eq!(
        hook_op(&client, hook(message("local"), vec![send()])),
        refused(ScheduleRefusal::UnknownSource)
    );
    let with_ingress = config(&base, true);
    let client = session(&with_ingress);
    for source in [
        // No authenticated ingress for any other channel.
        message("telegram"),
        // No such schedule.
        HookSource::ScheduleRunFinished {
            schedule_id: String::from("sch-0000"),
        },
    ] {
        assert_eq!(
            hook_op(&client, hook(source.clone(), vec![send()])),
            refused(ScheduleRefusal::UnknownSource),
            "{source}"
        );
    }
    assert!(
        SealedScheduleStore::new(&with_ingress.scheduler.root, key)
            .hooks()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_hook_whose_grants_exceed_the_creator_s_is_refused() {
    let base = base("exceeds");
    let config = config(&base, true);
    let key = key();
    let client = session(&config);
    let evil = ScheduleGrant::Egress {
        destination: String::from("evil.example:443"),
    };
    assert_eq!(
        hook_op(&client, hook(message("local"), vec![send(), evil])),
        HookResult::Refused {
            reason: ScheduleRefusal::ExceedsGrants
        }
    );
    assert_eq!(
        hook_op(&client, hook(message("local"), vec![egress()])),
        HookResult::Refused {
            reason: ScheduleRefusal::ChannelNotGranted
        }
    );
    // With scheduling off there is no registry at all.
    let off = Config {
        scheduler: SchedulerConfig::disabled(),
        ..config
    };
    assert_eq!(
        handle_hook_op(&off, key, &hook(message("local"), vec![send()])),
        HookResult::Denied
    );
    assert_eq!(
        handle_hook_op(
            &off,
            key,
            &HookOp::Cancel {
                id: String::from("hk-1")
            }
        ),
        HookResult::Denied
    );
}

#[test]
fn a_hook_frame_naming_an_unknown_source_ends_the_session() {
    let base = base("frame");
    let config = config(&base, true);
    let key = key();
    let client = session(&config);
    let frame = br#"{"kind":"hook","op":"create","source":{"source":"webhook","url":"https://x"},"task":"t","grants":[],"channel":"local"}"#;
    write_frame(&client, frame).unwrap();
    // Never acted on: no reply, the session is over.
    assert!(read_frame(&client).is_err());
    assert!(
        SealedScheduleStore::new(&config.scheduler.root, key)
            .hooks()
            .unwrap()
            .is_empty()
    );
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

    fn last(&self) -> RunRequest {
        self.requests.borrow().last().cloned().unwrap()
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

fn user_message(text: &str) -> Message {
    Message {
        sender: SenderId(String::from("claimed-sender")),
        channel: ChannelId::local(),
        text: text.to_owned(),
    }
}

#[test]
fn a_hook_fires_on_an_ingress_arrival_only_and_delivers_to_its_channel() {
    let base = base("fire");
    let config = config(&base, true);
    let key = key();
    let id = hook_id(&handle_hook_op(
        &config,
        key,
        &hook(message("local"), vec![send(), egress()]),
    ));
    let (hub, received) = hub_with_client();
    let launcher = FakeLauncher::default();
    let mut dispatcher = Dispatcher::default();
    let tick = |dispatcher: &mut Dispatcher<FakeRun>, now: &str| {
        dispatch_once(dispatcher, &config, key, Some(&hub), &launcher, ts(now)).unwrap()
    };
    // Nothing arrived: nothing fires.
    assert!(
        tick(&mut dispatcher, "2026-09-21T12:00:00Z")
            .launched
            .is_empty()
    );
    // The agent's own reply on the channel, and scheduled output delivered
    // to it, are not arrivals: they go out to the client.
    hub.deliver(&user_message("the agent replied"), "s")
        .unwrap();
    assert!(
        tick(&mut dispatcher, "2026-09-21T12:01:00Z")
            .launched
            .is_empty()
    );
    assert!(launcher.requests.borrow().is_empty());
    // A message through the ingress's queue is.
    assert_eq!(
        hub.inject(user_message("new mail")),
        hatchling_channel::IngressEvent::Queued
    );
    let report = tick(&mut dispatcher, "2026-09-21T12:02:00Z");
    assert_eq!(report.launched, [(id.clone(), false)]);
    let request = launcher.last();
    assert_eq!(request.schedule_id, id);
    assert_eq!(request.event, Some(message("local")));
    assert_eq!(request.channel, "local");
    assert_eq!(request.depth, 0);
    assert_eq!(request.grants, [send(), egress()]);
    // The hook's run output reaches the channel, from the scheduler.
    launcher.finish(0, "triaged");
    let report = tick(&mut dispatcher, "2026-09-21T12:03:00Z");
    assert_eq!(report.delivered, [id]);
    let delivered = received.lock().unwrap().clone();
    assert_eq!(delivered.last().map(|m| m.text.as_str()), Some("triaged"));
    assert_eq!(
        delivered.last().map(|m| m.sender.0.as_str()),
        Some("broker:scheduler")
    );
    // Delivering it was no arrival either.
    assert!(
        tick(&mut dispatcher, "2026-09-21T12:04:00Z")
            .launched
            .is_empty()
    );
}

#[test]
fn a_hook_fire_withholds_what_the_creator_no_longer_holds() {
    let base = base("revoke");
    let config = config(&base, true);
    let key = key();
    let id = hook_id(&handle_hook_op(
        &config,
        key,
        &hook(message("local"), vec![send(), egress()]),
    ));
    // The operator revokes the egress grant before the hook fires.
    let revoked = Config {
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: Vec::new(),
        },
        ..config
    };
    let (hub, _received) = hub_with_client();
    let launcher = FakeLauncher::default();
    let mut dispatcher = Dispatcher::default();
    let _ = hub.inject(user_message("hi"));
    let report = dispatch_once(
        &mut dispatcher,
        &revoked,
        key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T12:00:00Z"),
    )
    .unwrap();
    assert_eq!(report.launched, [(id.clone(), false)]);
    assert_eq!(report.revoked, [(id, egress())]);
    assert_eq!(launcher.last().grants, [send()]);
}

#[test]
fn without_an_ingress_an_arrival_fires_nothing() {
    let base = base("noingress");
    let with = config(&base, true);
    let key = key();
    let _ = hook_id(&handle_hook_op(
        &with,
        key,
        &hook(message("local"), vec![send()]),
    ));
    // Restarted without the ingress: the stored hook's source is no
    // longer one the broker authenticates.
    let without = config(&base, false);
    let (hub, _received) = hub_with_client();
    let _ = hub.inject(user_message("hi"));
    let launcher = FakeLauncher::default();
    let report = dispatch_once(
        &mut Dispatcher::default(),
        &without,
        key,
        Some(&hub),
        &launcher,
        ts("2026-09-21T12:00:00Z"),
    )
    .unwrap();
    assert!(report.launched.is_empty(), "{report:?}");
    assert_eq!(report.refused_events, [message("local")]);
    assert!(launcher.requests.borrow().is_empty());
}

#[test]
fn a_run_may_ask_for_one_followup_within_its_own_grants() {
    let base = base("followup");
    let config = config(&base, true);
    let key = key();
    let run = run_config(
        &config,
        &run_request(vec![send(), egress()], 1),
        &token("one"),
    );
    let client = session(&run);
    let id = schedule_id(&schedule_op(
        &client,
        followup("look again", 600, vec![send()]),
    ));
    assert!(id.starts_with("sch-"), "{id}");
    let stored = SealedScheduleStore::new(&config.scheduler.root, key)
        .load()
        .unwrap();
    assert_eq!(stored.len(), 1);
    let next = stored.first().unwrap();
    assert_eq!(next.id, id);
    assert_eq!(next.task, "look again");
    assert_eq!(next.grants, [send()]);
    assert_eq!(next.channel, "local");
    assert_eq!(next.depth, 2, "one deeper than its run");
    assert!(matches!(next.when, ScheduleSpec::Once { .. }));
    // One per run.
    assert_eq!(
        schedule_op(&client, followup("and again", 600, vec![send()])),
        ScheduleResult::Refused {
            reason: ScheduleRefusal::FollowupTaken
        }
    );
}

#[test]
fn a_followup_beyond_the_run_s_grants_is_refused() {
    let base = base("pump");
    let config = config(&base, true);
    let key = key();
    // The creator holds the vault and egress grants; this run holds only
    // channel-send. Its follow-up may not climb back up.
    let run = run_config(&config, &run_request(vec![send()], 0), &token("pump"));
    let client = session(&run);
    for grants in [vec![send(), vault()], vec![send(), egress()]] {
        assert_eq!(
            schedule_op(&client, followup("escalate", 60, grants)),
            ScheduleResult::Refused {
                reason: ScheduleRefusal::ExceedsGrants
            }
        );
    }
    assert!(
        SealedScheduleStore::new(&config.scheduler.root, key)
            .load()
            .unwrap()
            .is_empty()
    );
    // A refusal does not take the run's one follow-up.
    assert!(matches!(
        schedule_op(&client, followup("fine", 60, vec![send()])),
        ScheduleResult::Created { .. }
    ));
}

#[test]
fn a_followup_at_the_depth_bound_is_refused() {
    let base = base("depth");
    let config = config(&base, true);
    let key = key();
    let at_bound = run_config(
        &config,
        &run_request(vec![send()], MAX_FOLLOWUP_DEPTH),
        &token("bound"),
    );
    assert_eq!(
        schedule_op(&session(&at_bound), followup("once more", 60, vec![send()])),
        ScheduleResult::Refused {
            reason: ScheduleRefusal::MaxFollowupDepth
        }
    );
    assert!(
        SealedScheduleStore::new(&config.scheduler.root, key)
            .load()
            .unwrap()
            .is_empty()
    );
    let under = run_config(
        &config,
        &run_request(vec![send()], MAX_FOLLOWUP_DEPTH - 1),
        &token("under"),
    );
    assert!(matches!(
        schedule_op(&session(&under), followup("last", 60, vec![send()])),
        ScheduleResult::Created { .. }
    ));
}

#[test]
fn a_run_session_has_no_other_scheduler_surface_and_a_peer_has_no_followup() {
    let base = base("surfaces");
    let config = config(&base, true);
    let key = key();
    let run = run_config(&config, &run_request(vec![send()], 0), &token("surfaces"));
    let client = session(&run);
    assert_eq!(
        hook_op(&client, hook(message("local"), vec![send()])),
        HookResult::Denied
    );
    assert_eq!(
        hook_op(
            &client,
            HookOp::Cancel {
                id: String::from("hk-1")
            }
        ),
        HookResult::Denied
    );
    assert_eq!(
        schedule_op(
            &client,
            ScheduleOp::Create {
                when: ScheduleSpec::Cron {
                    expr: String::from("* * * * *"),
                    tz: String::from("UTC"),
                },
                task: String::from("t"),
                grants: vec![send()],
                channel: String::from("local"),
            }
        ),
        ScheduleResult::Denied
    );
    assert_eq!(
        schedule_op(
            &client,
            ScheduleOp::Cancel {
                id: String::from("sch-1")
            }
        ),
        ScheduleResult::Denied
    );
    // The peer's own session is not a run: it has no follow-up to ask for.
    let peer = session(&config);
    assert_eq!(
        schedule_op(&peer, followup("t", 60, vec![send()])),
        ScheduleResult::Denied
    );
    assert_eq!(
        handle_schedule_op(
            &config,
            key,
            &followup("t", 60, vec![send()]),
            ts("2026-09-21T12:00:00Z")
        ),
        ScheduleResult::Denied
    );
}

#[test]
fn a_followup_chain_fires_one_link_at_a_time_and_terminates() {
    let base = base("chain");
    let config = config(&base, true);
    let key = key();
    let launcher = FakeLauncher::default();
    let mut dispatcher = Dispatcher::default();
    let mut request = run_request(vec![send()], 0);
    let mut links = 0;
    loop {
        // The run asks through its own session, as the dispatcher started
        // it: its grants, channel, and depth are the request's.
        let run = run_config(&config, &request, &token(&format!("chain{links}")));
        let result = schedule_op(&session(&run), followup("next link", 60, vec![send()]));
        let ScheduleResult::Created { id } = result else {
            assert_eq!(
                result,
                ScheduleResult::Refused {
                    reason: ScheduleRefusal::MaxFollowupDepth
                }
            );
            break;
        };
        // The session stamps it from the broker's clock: fire it when due.
        let store = SealedScheduleStore::new(&config.scheduler.root, key);
        let due = store
            .load()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.next_due);
        let now = Timestamp::from_second(due.unwrap()).unwrap();
        let report = dispatch_once(&mut dispatcher, &config, key, None, &launcher, now).unwrap();
        assert_eq!(report.launched, [(id.clone(), false)]);
        assert_eq!(report.removed, [id], "a follow-up is a one-shot");
        assert_eq!(dispatcher.active(), 1, "never two links at once");
        request = launcher.last();
        assert_eq!(request.depth, links + 1);
        launcher.finish(usize::try_from(links).unwrap(), "link done");
        let _ = dispatch_once(&mut dispatcher, &config, key, None, &launcher, now).unwrap();
        links += 1;
    }
    assert_eq!(links, MAX_FOLLOWUP_DEPTH);
    assert!(
        SealedScheduleStore::new(&config.scheduler.root, key)
            .load()
            .unwrap()
            .is_empty()
    );
}
