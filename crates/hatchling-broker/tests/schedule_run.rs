//! End to end (-2): scheduled runs in real cells.
//!
//! Everything below the dispatcher is production code:
//!
//! - the cell launcher and its per-run socket and grant-scoped session;
//! - the `hatchling-broker agent-run` helper (this crate's own binary);
//! - the real cell (namespaces, id map, seccomp);
//! - the sealed schedule store;
//! - delivery through the local-channel hub.
//!
//! The one stand-in is the cell's program: `hatchling-run-fixture` in
//! place of the agent, since a scheduled agent turn needs a model. It
//! runs probes through the run's socket and prints what it saw.
//!
//! Dispatcher time is fixed per test (every tick is given its `now`), so
//! a schedule fires exactly when a test says. The only waiting is on the
//! real processes, polled with a bounded deadline.
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

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{guard_cell_available, mapped_agent_uid, run_fixture_binary};
use hatchling_broker::{
    CellLauncher, Config, EgressConfig, GoalsConfig, MemoryConfig, RunEnv, SchedulerConfig,
    SealedScheduleStore, SpaceConfig, SpaceSession, VaultConfig, VaultGrant, dispatch_once,
    handle_schedule_op, handle_space_op,
};
use hatchling_channel::{LocalHub, Message};
use hatchling_core::{
    LogLevel, PeerUid, ScheduleGrant, ScheduleOp, ScheduleResult, ScheduleSpec, SpaceOp,
    SpaceResult, VaultAction,
};
use hatchling_scheduler::{Dispatcher, Launcher, Run, RunRequest, Store, TickReport, Timestamp};
use hatchling_space::{PeerPin, Principal, SessionPin};
use hatchling_vault::MasterKey;

/// How long a test waits for a real run to finish.
const RUN_DEADLINE: Duration = Duration::from_secs(60);

/// Under `/tmp`, not `$TMPDIR`: the run sockets live beneath this, and a
/// unix socket path is capped at 107 bytes, while `$TMPDIR` can be deep.
/// A run test's base under `/tmp` (its run sockets must fit `sun_path`),
/// removed when the test ends, pass or fail.
fn base(name: &str) -> common::TestDir {
    let base = PathBuf::from(format!("/tmp/hlsr-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    common::TestDir::guard(base)
}

/// Releases an accept loop if broken cleanup leaves the run socket behind.
/// Armed before assertions, so a panic cannot pin the scoped test thread.
struct DrainPendingAccept(PathBuf);

impl Drop for DrainPendingAccept {
    fn drop(&mut self) {
        let _ = std::os::unix::net::UnixStream::connect(&self.0);
    }
}

fn config(base: &Path, vault_grants: &[&str]) -> Config {
    let vault_root = base.join("vault");
    Config {
        socket: "/tmp/hatchling-test-schedrun-dummy/broker.sock"
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
            grants: vault_grants
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
        goals: GoalsConfig::disabled(),
    }
}

fn env(config: &Config, timeout: Duration) -> RunEnv {
    RunEnv {
        spawner: common::spawner(),
        agent_bin: run_fixture_binary().unwrap(),
        run_root: config.scheduler.run_root(),
        timeout,
    }
}

fn ts(rfc3339: &str) -> Timestamp {
    rfc3339.parse().unwrap()
}

fn vault(name: &str) -> ScheduleGrant {
    ScheduleGrant::Vault {
        action: String::from("fetch"),
        name: name.to_owned(),
    }
}

fn send() -> ScheduleGrant {
    ScheduleGrant::ChannelSend {
        channel: String::from("local"),
    }
}

/// Creates a schedule firing every minute, at `now`.
fn every_minute(
    config: &Config,
    key: &MasterKey,
    task: &str,
    grants: Vec<ScheduleGrant>,
    now: &str,
) -> String {
    let op = ScheduleOp::Create {
        when: ScheduleSpec::Cron {
            expr: String::from("* * * * *"),
            tz: String::from("UTC"),
        },
        task: task.to_owned(),
        grants,
        channel: String::from("local"),
    };
    match agent_schedule_op(config, key, &op, ts(now)) {
        ScheduleResult::Created { id } => id,
        other @ (ScheduleResult::Cancelled
        | ScheduleResult::NotFound
        | ScheduleResult::Listed { .. }
        | ScheduleResult::Refused { .. }
        | ScheduleResult::Denied
        | ScheduleResult::ScheduleError) => format!("not created: {other:?}"),
    }
}

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

/// Ticks at `now` until `done` holds for the combined reports, or the
/// deadline passes. `now` stays fixed, so nothing fires twice.
fn tick_until(
    mut tick: impl FnMut() -> TickReport,
    done: impl Fn(&TickReport) -> bool,
) -> TickReport {
    let deadline = Instant::now() + RUN_DEADLINE;
    let mut all = TickReport::default();
    while Instant::now() < deadline {
        let report = tick();
        all.launched.extend(report.launched);
        all.skipped.extend(report.skipped);
        all.delivered.extend(report.delivered);
        all.parked.extend(report.parked);
        all.revoked.extend(report.revoked);
        all.removed.extend(report.removed);
        if done(&all) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    all
}

fn run_dirs(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_scheduled_run_executes_in_its_own_cell_with_exactly_its_grants() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("grants");
    let created_under = config(&base, &["app-token", "old-token"]);
    let key = MasterKey::generate().unwrap();
    let id = every_minute(
        &created_under,
        &key,
        "vault:app-token vault:old-token channel:local schedule",
        vec![vault("app-token"), vault("old-token"), send()],
        "2026-09-21T12:00:30Z",
    );
    // The operator revokes `old-token` before the fire.
    let fire_under = config(&base, &["app-token"]);
    let (hub, received) = hub_with_client();
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &fire_under, &key, env(&fire_under, RUN_DEADLINE));
        let mut dispatcher = Dispatcher::default();
        let now = ts("2026-09-21T12:01:01Z");
        let all = tick_until(
            || {
                dispatch_once(
                    &mut dispatcher,
                    &fire_under,
                    &key,
                    Some(&hub),
                    &launcher,
                    now,
                )
                .unwrap()
            },
            |all| !all.delivered.is_empty() || !all.parked.is_empty(),
        );
        assert_eq!(all.launched, [(id.clone(), false)]);
        assert_eq!(all.revoked, [(id.clone(), vault("old-token"))]);
        assert_eq!(all.delivered, std::slice::from_ref(&id), "{all:?}");
    });
    let received = received.lock().unwrap().clone();
    let message = received.first().unwrap();
    assert_eq!(message.sender.0, "broker:scheduler");
    let report = &message.text;
    for line in [
        "missed=0 due=1789992060",
        "fds=0,1,2",
        "sock=visible",
        "vault:app-token=Vault(NotFound)",
        "vault:old-token=Vault(Denied)",
        "channel:local=Channel(NoMessage)",
        "schedule=Schedule(Denied)",
    ] {
        assert!(
            report.lines().any(|seen| seen == line),
            "{line:?} not in:\n{report}"
        );
    }
    assert!(
        run_dirs(&fire_under.scheduler.run_root()).is_empty(),
        "the run dir is gone"
    );
}

#[test]
fn a_missed_fire_runs_once_with_the_flag_visible_to_the_run() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("missed");
    let config = config(&base, &[]);
    let key = MasterKey::generate().unwrap();
    let id = every_minute(
        &config,
        &key,
        "report",
        vec![send()],
        "2026-09-21T12:00:30Z",
    );
    let (hub, received) = hub_with_client();
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, RUN_DEADLINE));
        let mut dispatcher = Dispatcher::default();
        // Down for five hours: one run, flagged.
        let now = ts("2026-09-21T17:00:40Z");
        let all = tick_until(
            || dispatch_once(&mut dispatcher, &config, &key, Some(&hub), &launcher, now).unwrap(),
            |all| !all.delivered.is_empty() || !all.parked.is_empty(),
        );
        assert_eq!(all.launched, [(id.clone(), true)]);
        assert_eq!(all.delivered, [id]);
    });
    let received = received.lock().unwrap().clone();
    let text = &received.first().unwrap().text;
    assert!(
        text.lines().any(|line| line == "missed=1 due=1789992060"),
        "the broker's flag and the oldest missed fire:\n{text}"
    );
}

#[test]
fn a_hung_run_blocks_the_next_fire_until_the_timeout_reaps_it() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("hang");
    let config = config(&base, &[]);
    let key = MasterKey::generate().unwrap();
    let id = every_minute(&config, &key, "hang", vec![send()], "2026-09-21T12:00:30Z");
    let (hub, _received) = hub_with_client();
    let timeout = Duration::from_secs(3);
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, timeout));
        let mut dispatcher = Dispatcher::default();
        let first = dispatch_once(
            &mut dispatcher,
            &config,
            &key,
            Some(&hub),
            &launcher,
            ts("2026-09-21T12:01:01Z"),
        )
        .unwrap();
        assert_eq!(first.launched, [(id.clone(), false)]);
        // While it hangs: its socket and directory are owner-only.
        let dirs = run_dirs(&config.scheduler.run_root());
        assert_eq!(dirs.len(), 1);
        let dir = dirs.first().unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(dir), 0o700);
        assert_eq!(mode(&dir.join("broker.sock")), 0o600);
        // The next fire comes while it still runs: skipped, not stacked.
        let next = dispatch_once(
            &mut dispatcher,
            &config,
            &key,
            Some(&hub),
            &launcher,
            ts("2026-09-21T12:02:01Z"),
        )
        .unwrap();
        assert_eq!(next.skipped, std::slice::from_ref(&id));
        assert!(next.launched.is_empty());
        // The wall-clock timeout reaps it, and the run is parked.
        let now = ts("2026-09-21T12:02:02Z");
        let all = tick_until(
            || dispatch_once(&mut dispatcher, &config, &key, Some(&hub), &launcher, now).unwrap(),
            |all| !all.parked.is_empty(),
        );
        assert_eq!(all.parked, std::slice::from_ref(&id));
        assert_eq!(dispatcher.active(), 0);
    });
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    let parked = store.parked().unwrap();
    let first = parked.first().unwrap();
    assert!(first.text.contains("timed out after 3 s"), "{}", first.text);
    assert!(
        run_dirs(&config.scheduler.run_root()).is_empty(),
        "the hung run's dir is gone"
    );
}

/// Runs one request through the launcher and waits for its outcome.
fn run_to_end(launcher: &CellLauncher<'_, '_>, task: &str) -> Result<String, String> {
    let mut run = launcher
        .launch(RunRequest {
            schedule_id: String::from("s"),
            task: task.to_owned(),
            grants: vec![send()],
            due: ts("2026-09-21T12:01:00Z"),
            missed: false,
            channel: String::from("local"),
            depth: 0,
            event: None,
            space: None,
            feed: None,
        })
        .unwrap();
    let deadline = Instant::now() + RUN_DEADLINE;
    loop {
        if let Some(outcome) = run.poll() {
            return outcome;
        }
        assert!(Instant::now() < deadline, "the run never finished");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_run_that_never_connects_still_ends_and_cleans_up() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("noconnect");
    let config = config(&base, &[]);
    let key = MasterKey::generate().unwrap();
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, RUN_DEADLINE));
        // Its session was left waiting to accept; ending the run ends it.
        assert_eq!(run_to_end(&launcher, "noconnect"), Ok(String::new()));
    });
    assert!(run_dirs(&config.scheduler.run_root()).is_empty());
}

#[test]
fn run_output_is_bounded_exactly() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("flood");
    let config = config(&base, &[]);
    let key = MasterKey::generate().unwrap();
    let bound = hatchling_broker::run_launcher::MAX_RUN_OUTPUT_BYTES;
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, RUN_DEADLINE));
        assert_eq!(
            run_to_end(&launcher, &format!("flood:{bound}")).map(|out| out.len()),
            Ok(bound),
            "exactly the bound is kept whole"
        );
        let over = run_to_end(&launcher, &format!("flood:{}", bound + 1)).unwrap_err();
        assert!(over.contains("exceeds"), "{over}");
    });
}

#[test]
fn one_run_cannot_reach_another_run_s_socket() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("isolation");
    let config = config(&base, &[]);
    let key = MasterKey::generate().unwrap();
    let request = |task: &str| RunRequest {
        schedule_id: String::from("s"),
        task: task.to_owned(),
        grants: vec![send()],
        due: ts("2026-09-21T12:01:00Z"),
        missed: false,
        channel: String::from("local"),
        depth: 0,
        event: None,
        space: None,
        feed: None,
    };
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, RUN_DEADLINE));
        let mut first = launcher.launch(request("hang")).unwrap();
        let dirs = run_dirs(&config.scheduler.run_root());
        let other = dirs.first().unwrap().join("broker.sock");
        let task = format!("connect:{} connect:/sock/broker.sock", other.display());
        let mut second = launcher.launch(request(&task)).unwrap();
        let deadline = Instant::now() + RUN_DEADLINE;
        let outcome = loop {
            if let Some(outcome) = second.poll() {
                break outcome;
            }
            assert!(Instant::now() < deadline, "the second run never finished");
            std::thread::sleep(Duration::from_millis(50));
        };
        let text = outcome.unwrap();
        assert!(
            text.lines()
                .any(|line| line == format!("connect:{}=NotFound", other.display())),
            "the first run's socket is not in the second run's cell:\n{text}"
        );
        assert!(
            text.lines()
                .any(|line| line == "connect:/sock/broker.sock=reached"),
            "its own socket is:\n{text}"
        );
        let shown = format!("{launcher:?}");
        assert!(shown.starts_with("CellLauncher { env: RunEnv {"), "{shown}");
        let shown = format!("{first:?}");
        assert!(shown.starts_with("CellRun { run_dir: "), "{shown}");
        assert!(shown.contains("done: false"), "{shown}");
        drop(first.poll());
        drop(first);
    });
    assert!(
        run_dirs(&config.scheduler.run_root()).is_empty(),
        "both run dirs are gone"
    );
}

///, in a real cell: the run asks through its own socket. Its
/// follow-up may hold only what the run holds, it gets one, and it can
/// register no hook. The follow-up then fires through the same launcher,
/// one deeper.
#[test]
fn a_run_asks_for_its_followup_through_its_own_socket_within_its_own_grants() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("followup");
    // The creator holds `app-token`; the schedule gives its run only
    // channel-send.
    let config = config(&base, &["app-token"]);
    let key = MasterKey::generate().unwrap();
    let id = every_minute(
        &config,
        &key,
        "followup-pump followup:600 followup:60 hook",
        vec![send()],
        "2026-09-21T12:00:30Z",
    );
    let (hub, received) = hub_with_client();
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, RUN_DEADLINE));
        let mut dispatcher = Dispatcher::default();
        let now = ts("2026-09-21T12:01:01Z");
        let all = tick_until(
            || dispatch_once(&mut dispatcher, &config, &key, Some(&hub), &launcher, now).unwrap(),
            |all| !all.delivered.is_empty() || !all.parked.is_empty(),
        );
        assert_eq!(all.delivered, std::slice::from_ref(&id), "{all:?}");
    });
    let first = received
        .lock()
        .unwrap()
        .first()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    for line in [
        "depth=0 event=",
        "followup-pump=Schedule(Refused { reason: ExceedsGrants })",
        "followup:60=Schedule(Refused { reason: FollowupTaken })",
        "hook=Hook(Denied)",
    ] {
        assert!(
            first.lines().any(|seen| seen == line),
            "{line:?} not in:\n{first}"
        );
    }
    assert!(
        first
            .lines()
            .any(|seen| seen.starts_with("followup:600=Schedule(Created { id: \"sch-")),
        "{first}"
    );
    // Stored as a one-shot, one deeper, with only the run's grant.
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    let next = store
        .load()
        .unwrap()
        .into_iter()
        .find(|schedule| schedule.id != id)
        .unwrap();
    assert_eq!(next.task, "next");
    assert_eq!(next.depth, 1);
    assert_eq!(next.grants, [send()]);
    assert!(matches!(next.when, ScheduleSpec::Once { .. }));
    // Its parent is cancelled; the follow-up fires on its own.
    assert_eq!(
        agent_schedule_op(
            &config,
            &key,
            &ScheduleOp::Cancel { id },
            ts("2026-09-21T12:02:00Z")
        ),
        ScheduleResult::Cancelled
    );
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, RUN_DEADLINE));
        let mut dispatcher = Dispatcher::default();
        let due = Timestamp::from_second(next.next_due).unwrap();
        let all = tick_until(
            || dispatch_once(&mut dispatcher, &config, &key, Some(&hub), &launcher, due).unwrap(),
            |all| !all.delivered.is_empty() || !all.parked.is_empty(),
        );
        assert_eq!(all.launched, [(next.id.clone(), false)]);
        assert_eq!(all.removed, std::slice::from_ref(&next.id), "a one-shot");
        assert_eq!(all.delivered, std::slice::from_ref(&next.id), "{all:?}");
    });
    let second = received
        .lock()
        .unwrap()
        .get(1)
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(
        second.lines().any(|seen| seen == "depth=1 event="),
        "the broker's depth, in the cell:\n{second}"
    );
    assert!(store.load().unwrap().is_empty());
    assert!(run_dirs(&config.scheduler.run_root()).is_empty());
}

///, in a real cell: a space task's run session is that task run,
/// bound by the broker to its one space. Through its own socket it can
/// join and read its space; another space, creating spaces, and grants
/// outside its own are all refused. An ordinary run has no spaces at all.
#[test]
fn a_space_tasks_run_uses_its_own_space_and_nothing_outside_it() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("space");
    let mut config = config(&base, &["app-token"]);
    config.spaces = SpaceConfig {
        root: base.join("spaces"),
        bound: None,
        device_socket: None,
    };
    let key = MasterKey::generate().unwrap();
    let owner = SpaceSession {
        principal: Principal::Agent {
            uid: mapped_agent_uid(),
        },
        pin: SessionPin {
            session: String::from("space-run-owner"),
            peer: PeerPin {
                uid: mapped_agent_uid(),
                pid: 1,
            },
        },
    };
    let now = Timestamp::now();
    let ask = |op: SpaceOp| handle_space_op(&config, &key, &owner, &op, now);
    let space_with = |value: &str| {
        let SpaceResult::Created { space } = ask(SpaceOp::Create {
            name: String::from("room"),
            grants: vec![send()],
        }) else {
            return String::from("not created");
        };
        let SpaceResult::Admitted { ticket, .. } = ask(SpaceOp::Join {
            space: space.clone(),
        }) else {
            return String::from("not admitted");
        };
        let stored = ask(SpaceOp::Put {
            space: space.clone(),
            ticket,
            key: String::from("plan"),
            value: value.to_owned(),
        });
        assert_eq!(stored, SpaceResult::Stored);
        space
    };
    let mine = space_with("water at six");
    let theirs = space_with("not for this run");
    let task = format!("space:{mine}:plan space:{theirs}:plan space-create vault:app-token");
    let request = |space: Option<String>| RunRequest {
        schedule_id: String::from("sch-space"),
        task: task.clone(),
        grants: vec![send()],
        due: now,
        missed: false,
        channel: String::from("local"),
        depth: 0,
        event: None,
        space,
        feed: None,
    };
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, RUN_DEADLINE));
        let finish = |request: RunRequest| {
            let mut run = launcher.launch(request).unwrap();
            let deadline = Instant::now() + RUN_DEADLINE;
            loop {
                if let Some(outcome) = run.poll() {
                    return outcome.unwrap();
                }
                assert!(Instant::now() < deadline, "the run never finished");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let bound = finish(request(Some(mine.clone())));
        for expected in [
            format!(r#"space:{mine}:plan=Space(Value {{ value: "water at six" }})"#),
            format!("space:{theirs}:plan=Space(Refused {{ reason: NoSuchSpace }})"),
            String::from("space-create=Space(Refused { reason: NotAllowed })"),
            String::from("vault:app-token=Vault(Denied)"),
        ] {
            assert!(
                bound.lines().any(|line| line == expected),
                "missing {expected:?} in:\n{bound}"
            );
        }
        let plain = finish(request(None));
        for expected in [
            format!("space:{mine}:plan=Space(Denied)"),
            format!("space:{theirs}:plan=Space(Denied)"),
        ] {
            assert!(
                plain.lines().any(|line| line == expected),
                "missing {expected:?} in:\n{plain}"
            );
        }
    });
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

#[test]
fn dropping_a_running_cell_cancels_it_and_cleans_its_resources() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let base = base("cancel");
    let config = config(&base, &[]);
    let key = MasterKey::generate().unwrap();
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, &config, &key, env(&config, RUN_DEADLINE));
        let run = launcher
            .launch(RunRequest {
                schedule_id: String::from("cancel"),
                task: String::from("hang"),
                grants: vec![send()],
                due: ts("2026-09-21T12:01:00Z"),
                missed: false,
                channel: String::from("local"),
                depth: 0,
                event: None,
                space: None,
                feed: None,
            })
            .unwrap();
        let run_dir = run_dirs(&config.scheduler.run_root())
            .into_iter()
            .next()
            .unwrap();
        let accept_rescue = DrainPendingAccept(run_dir.join("broker.sock"));
        // Wait until the helper has actually constructed a cell before
        // cancellation; cancelling only the launch would miss its cleanup.
        if let Some(parent) = std::env::var_os("HATCHLING_CELL_CGROUP") {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let nested = std::fs::read_dir(&parent)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .any(|entry| {
                        std::fs::read_dir(entry.path())
                            .unwrap()
                            .filter_map(Result::ok)
                            .any(|entry| {
                                entry.file_type().is_ok_and(|kind| kind.is_dir())
                                    && std::fs::read_to_string(entry.path().join("pids.current"))
                                        .ok()
                                        .and_then(|value| value.trim().parse::<u64>().ok())
                                        .is_some_and(|count| count >= 3)
                            })
                    });
                if nested {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "helper never created its cell cgroup"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        drop(run);
        // Correct cancellation already removed the socket. Rescue only a
        // broken destructor, after dropping the run, before scope joins.
        drop(accept_rescue);
    });
    assert!(run_dirs(&config.scheduler.run_root()).is_empty());
}

#[test]
fn cgroups_are_removed_after_scheduled_timeout_and_cancellation() -> Result<(), String> {
    common::cgroup_lifecycle_cases(
        "scheduled",
        &[
            "a_hung_run_blocks_the_next_fire_until_the_timeout_reaps_it",
            "dropping_a_running_cell_cancels_it_and_cleans_its_resources",
        ],
    )
}
