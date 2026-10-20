//! A broker killed with a scheduled run in flight (WO-025): the next
//! broker finds the fire of unknown outcome, not lost and not run again.
//!
//! The killed broker is this test binary again, as a child: it stores a due
//! schedule, ticks the real dispatcher once, so the fire is claimed and its
//! run launched in a real cell whose program hangs (or, killed between the
//! two, the fire claimed and its launch not yet made), says so, and waits
//! to be killed with `SIGKILL`. Then the parent does what the next broker's
//! start does, and looks at what the owner sees.
//!
//! Also here: a claim whose store write fails at each of its steps, through
//! the vault's injected failures, is launched once, never twice, and never
//! on the tick its write failed.
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

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{guard_cell_available, mapped_agent_uid, run_fixture_binary};
use hatchling_broker::{
    CellLauncher, Config, EgressConfig, GoalsConfig, MemoryConfig, RunEnv, SchedulerConfig,
    SealedScheduleStore, SpaceConfig, VaultConfig, WebConfig, dispatch_once, handle_schedule_op,
};
use hatchling_core::{
    HeldFire, LogLevel, PeerUid, ScheduleGrant, ScheduleOp, ScheduleResult, ScheduleSpec,
};
use hatchling_scheduler::{
    Creator, Dispatcher, Launcher, OccurrenceState, Run, RunRequest, Store as _, Timestamp,
    validate,
};
use hatchling_vault::MasterKey;
use hatchling_vault::fault::Step;

/// In a child: the test's base directory; the child plays the killed
/// broker.
const BASE: &str = "HATCHLING_UNKNOWN_TEST_BASE";

/// In a child: where the killed broker is killed, [`LAUNCHED`] or
/// [`CLAIMED`].
const MODE: &str = "HATCHLING_UNKNOWN_TEST_MODE";

/// Killed with its fire's run launched.
const LAUNCHED: &str = "launched";

/// Killed with its fire claimed, before its launch.
const CLAIMED: &str = "claimed";

/// What the killed broker prints once its fire is claimed and running:
/// nothing the test harness prints (its `running 1 test` among them).
const READY: &str = "unknown-outcome-test: fire running";

/// The schedule's first fire, and the next an hour on.
const FIRST: i64 = 1_790_002_800;
const NEXT: i64 = FIRST + 3_600;

fn base(name: &str) -> common::TestDir {
    let base = PathBuf::from(format!("/tmp/hlun-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("tmp")).unwrap();
    common::TestDir::guard(base)
}

/// A broker config with scheduling on, in `base`.
fn config(base: &Path) -> Config {
    let vault_root = base.join("vault");
    Config {
        socket: base.join("broker.sock").to_str().unwrap().parse().unwrap(),
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
            grants: Vec::new(),
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

/// The broker's key, as each broker loads it: the same in the killed
/// broker and the next.
fn key(config: &Config) -> MasterKey {
    hatchling_broker::load_master_key(&config.vault).unwrap()
}

/// An hourly schedule whose run's program hangs, first due at [`FIRST`].
fn hourly() -> hatchling_scheduler::Schedule {
    let send = ScheduleGrant::ChannelSend {
        channel: String::from("local"),
    };
    let mut schedule = validate(
        String::from("sch-hang"),
        ScheduleSpec::Cron {
            expr: String::from("0 * * * *"),
            tz: String::from("UTC"),
        },
        String::from("hang"),
        vec![send.clone()],
        String::from("local"),
        &BTreeSet::from([send]),
        Timestamp::from_second(FIRST - 600).unwrap(),
    )
    .unwrap();
    schedule.next_due = FIRST;
    schedule
}

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

/// The killed broker: claims the fire and launches its run in a real
/// cell, then says so, and waits.
fn killed(config: &Config) {
    let key = key(config);
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    assert!(store.insert(&hourly()).unwrap());
    let env = RunEnv {
        spawner: common::spawner(),
        agent_bin: run_fixture_binary().unwrap(),
        run_root: config.scheduler.run_root(),
        timeout: Duration::from_secs(600),
    };
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, config, &key, env);
        let mut dispatcher = Dispatcher::default();
        let report =
            dispatch_once(&mut dispatcher, config, &key, None, &launcher, at(FIRST)).unwrap();
        assert_eq!(report.launched, [(String::from("sch-hang"), false)]);
        let state = store
            .load()
            .unwrap()
            .into_iter()
            .next()
            .and_then(|stored| stored.ledger.occurrence)
            .map(|fire| fire.state);
        assert_eq!(state, Some(OccurrenceState::Running));
        say_ready();
        std::thread::sleep(Duration::from_secs(600));
    });
}

/// Says it is about to launch, and never does: its broker is killed
/// first.
struct Stalling;

impl Launcher for Stalling {
    type Run = Endless;

    fn launch(&self, _request: RunRequest) -> Result<Endless, String> {
        say_ready();
        std::thread::sleep(Duration::from_secs(600));
        Err(String::from("its broker was meant to be killed by now"))
    }
}

/// Tells the parent the fire is where it should be killed.
fn say_ready() {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{READY}").unwrap();
    out.flush().unwrap();
}

/// The broker killed between its fire's claim and its launch.
fn killed_before_launch(config: &Config) {
    let key = key(config);
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    assert!(store.insert(&hourly()).unwrap());
    let mut dispatcher = Dispatcher::default();
    let _ = dispatch_once(&mut dispatcher, config, &key, None, &Stalling, at(FIRST));
}

/// Run only as a child: the killed broker, over [`BASE`].
#[test]
fn a_child() {
    let Some(base) = std::env::var_os(BASE) else {
        return;
    };
    let config = config(Path::new(&base));
    if std::env::var(MODE).as_deref() == Ok(CLAIMED) {
        killed_before_launch(&config);
    } else {
        killed(&config);
    }
}

/// Counts what it is asked to start; its runs never end.
#[derive(Default)]
struct Counting(RefCell<Vec<RunRequest>>);

struct Endless;

impl Run for Endless {
    fn poll(&mut self) -> Option<Result<String, String>> {
        None
    }
}

impl Launcher for Counting {
    type Run = Endless;

    fn launch(&self, request: RunRequest) -> Result<Endless, String> {
        self.0.borrow_mut().push(request);
        Ok(Endless)
    }
}

/// Starts the killed broker over `base`, and kills it with `SIGKILL` once
/// its fire is where `mode` says.
fn kill_once_ready(base: &Path, mode: &str) {
    let mut broker = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "a_child", "--nocapture", "--test-threads=1"])
        .env(BASE, base)
        .env(MODE, mode)
        .env("TMPDIR", base.join("tmp"))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = broker.stdout.take().unwrap();
    let (said, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = said.send(line);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut told = Vec::new();
    let running = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match heard.recv_timeout(left) {
            Ok(line) if line.contains(READY) => break true,
            Ok(line) => told.push(line),
            Err(_) => break false,
        }
    };
    broker.kill().unwrap();
    broker.wait().unwrap();
    assert!(
        running,
        "the killed broker never ran its fire; it said {told:?}"
    );
}

/// A broker killed after its fire's run launched comes back with that
/// fire of unknown outcome: held, shown to the owner, run again only when
/// the owner retries it, and never resolved by a cell.
#[test]
fn a_fire_in_flight_when_the_broker_is_killed_is_held_as_unknown() {
    if std::env::var_os(BASE).is_some() || !guard_cell_available().unwrap() {
        return;
    }
    let base = base("killed");
    let config = config(&base);
    kill_once_ready(&base, LAUNCHED);

    // The next broker's start.
    let key = key(&config);
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    let before = store.load().map(|all| {
        all.into_iter()
            .map(|stored| (stored.id, stored.ledger))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        hatchling_broker::run_launcher::hold_in_flight(&config, &key),
        ["sch-hang"],
        "the store held: {before:?}"
    );
    let fire = store
        .load()
        .unwrap()
        .into_iter()
        .next()
        .and_then(|stored| stored.ledger.occurrence)
        .unwrap();
    assert_eq!(fire.due, FIRST);
    assert_eq!(fire.state, OccurrenceState::Unknown { started: true });

    // Its next fire is held, and nothing runs.
    let launcher = Counting::default();
    let mut dispatcher = Dispatcher::default();
    let report = dispatch_once(&mut dispatcher, &config, &key, None, &launcher, at(NEXT)).unwrap();
    assert_eq!(report.held, ["sch-hang"]);
    assert!(
        launcher.0.borrow().is_empty(),
        "a fire of unknown outcome ran again"
    );

    // The owner sees it.
    let listed = handle_schedule_op(
        &config,
        &key,
        &ScheduleOp::List { after: None },
        at(NEXT),
        Creator::Owner,
    );
    let ScheduleResult::Listed { schedules, .. } = listed else {
        unreachable!("not listed: {listed:?}");
    };
    assert_eq!(
        schedules.first().and_then(|shown| shown.held),
        Some(HeldFire {
            due: FIRST,
            started: true
        })
    );

    // A cell can't resolve or retry it; the owner retries it, and it runs
    // as a new attempt.
    let retry = ScheduleOp::Retry {
        id: String::from("sch-hang"),
    };
    assert_eq!(
        handle_schedule_op(&config, &key, &retry, at(NEXT + 60), Creator::Agent),
        ScheduleResult::Denied
    );
    assert_eq!(
        handle_schedule_op(&config, &key, &retry, at(NEXT + 60), Creator::Owner),
        ScheduleResult::Retried
    );
    let report = dispatch_once(
        &mut dispatcher,
        &config,
        &key,
        None,
        &launcher,
        at(NEXT + 60),
    )
    .unwrap();
    assert_eq!(report.launched, [(String::from("sch-hang"), false)]);
    assert_eq!(launcher.0.borrow().len(), 1);
}

/// The fire the store holds for the one schedule in `config`'s store.
fn stored_fire(config: &Config, key: &MasterKey) -> Option<hatchling_scheduler::Occurrence> {
    SealedScheduleStore::new(&config.scheduler.root, key)
        .load()
        .unwrap()
        .into_iter()
        .next()
        .and_then(|stored| stored.ledger.occurrence)
}

/// A broker killed after its fire's claim committed and before its run
/// launched comes back with that fire of unknown outcome, not started:
/// held, never run again on its own.
#[test]
fn a_fire_claimed_when_the_broker_is_killed_is_held_as_not_started() {
    if std::env::var_os(BASE).is_some() {
        return;
    }
    let base = base("claimed");
    let config = config(&base);
    kill_once_ready(&base, CLAIMED);

    let key = key(&config);
    assert_eq!(
        stored_fire(&config, &key).map(|fire| fire.state),
        Some(OccurrenceState::Claimed)
    );
    assert_eq!(
        hatchling_broker::run_launcher::hold_in_flight(&config, &key),
        ["sch-hang"]
    );
    let fire = stored_fire(&config, &key).unwrap();
    assert_eq!(fire.due, FIRST);
    assert_eq!(fire.state, OccurrenceState::Unknown { started: false });
    let launcher = Counting::default();
    let mut dispatcher = Dispatcher::default();
    let report = dispatch_once(&mut dispatcher, &config, &key, None, &launcher, at(NEXT)).unwrap();
    assert_eq!(report.held, ["sch-hang"]);
    assert!(launcher.0.borrow().is_empty(), "run again on its own");
}

/// A claim whose store write fails at `step` launches nothing on that
/// tick; the next tick launches the fire it claimed, once; and no later
/// tick launches it again. Returns whether the failed claim had landed.
fn claim_failing_at(step: Step) -> bool {
    let base = base(&format!("{step:?}"));
    let config = config(&base);
    let key = key(&config);
    let store = SealedScheduleStore::new(&config.scheduler.root, &key);
    assert!(store.insert(&hourly()).unwrap());
    let launcher = Counting::default();
    let mut dispatcher = Dispatcher::default();

    hatchling_vault::fault::fail_next(step);
    let report = dispatch_once(&mut dispatcher, &config, &key, None, &launcher, at(FIRST)).unwrap();
    assert!(
        report.launched.is_empty(),
        "{step:?}: launched on its failed claim"
    );
    let landed = stored_fire(&config, &key).is_some();

    for later in [FIRST + 30, FIRST + 90, FIRST + 150] {
        dispatch_once(&mut dispatcher, &config, &key, None, &launcher, at(later)).unwrap();
    }
    let dues: Vec<i64> = launcher
        .0
        .borrow()
        .iter()
        .map(|request| request.due.as_second())
        .collect();
    assert_eq!(dues, [FIRST], "{step:?}: not launched once, as its fire");
    assert_eq!(
        stored_fire(&config, &key).map(|fire| (fire.due, fire.state)),
        Some((FIRST, OccurrenceState::Running)),
        "{step:?}"
    );
    landed
}

/// WO-025, part 3: a claim's store write failing at each of its steps
/// launches its fire once, the next tick, and never twice. Failing before
/// its rename, nothing landed; failing after it, at the directory's sync,
/// the claim is visible but may not be durable, and is written again
/// before its run launches.
#[test]
fn a_claim_failing_at_each_write_step_launches_once() {
    if std::env::var_os(BASE).is_some() {
        return;
    }
    for step in [Step::TempWrite, Step::TempSync, Step::Rename] {
        assert!(!claim_failing_at(step), "{step:?}: a claim landed");
    }
    assert!(
        claim_failing_at(Step::DirSync),
        "the claim failing at the directory's sync didn't land"
    );
}
