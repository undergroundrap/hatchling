//! The sweep as the broker runs it (WO-012): a broker killed mid-run
//! leaves its run's directory, with the run's cell staging in it, and the
//! next start sweeps them and logs each; `hatchling-broker clean` does the
//! same on demand.
//!
//! Every sweep here runs in a child process, this test binary again, with
//! a `TMPDIR` of its own: a sweep lists its `TMPDIR`, and under mutation a
//! broken filter would empty a shared one (other tests' and other
//! workers' directories in it). The killed broker is such a child too: it
//! launches a run whose real cell hangs, says where the run's directory
//! is, and waits to be killed with `SIGKILL`.
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

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{guard_cell_available, mapped_agent_uid, run_fixture_binary};
use hatchling_broker::{
    CellLauncher, Config, EgressConfig, GoalsConfig, MemoryConfig, RunEnv, SchedulerConfig,
    SpaceConfig, VaultConfig,
};
use hatchling_core::{LogLevel, PeerUid};
use hatchling_scheduler::{Launcher as _, RunRequest, Timestamp};
use hatchling_vault::MasterKey;

/// In a child: what it plays, `killed` (a broker killed mid-run),
/// `start` (the next broker's start) or `clean` (the operator's command).
const MODE: &str = "HATCHLING_SWEEP_TEST_MODE";

/// In a child: the test's base directory.
const BASE: &str = "HATCHLING_SWEEP_TEST_BASE";

/// In a `clean` child: its arguments, space-separated.
const ARGS: &str = "HATCHLING_SWEEP_TEST_ARGS";

/// What the killed broker prints once its run's cell staging is there.
const READY: &str = "run-dir ";

/// The test's base under `/tmp`, short as run paths like them.
fn base(name: &str) -> common::TestDir {
    let base = PathBuf::from(format!("/tmp/hlsw-{}-{name}", std::process::id()));
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
        goals: GoalsConfig::disabled(),
    }
}

/// This test binary as a `mode` child over `base`, with `base/tmp` its
/// `TMPDIR`.
fn child(mode: &str, base: &Path, args: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "a_child", "--nocapture", "--test-threads=1"])
        .env(MODE, mode)
        .env(BASE, base)
        .env(ARGS, args)
        .env("TMPDIR", base.join("tmp"))
        .stdout(Stdio::piped());
    command
}

/// Runs a `mode` child to its end, within a minute; what it said.
fn said_by(mode: &str, base: &Path, args: &str) -> String {
    let mut running = child(mode, base, args).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while running.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            let _ = running.kill();
            let _ = running.wait();
            unreachable!("the {mode} child never ended");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut out = String::new();
    running.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    out
}

/// One line on stdout, at once: the parent reads the child's stdout.
fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").unwrap();
    out.flush().unwrap();
}

/// The run's cell staging in `run_dir`, once it is there.
fn staging_in(run_dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(run_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("hatchling-staging-"))
        })
}

/// The pid a staging directory's name carries.
fn staging_pid(staging: &Path) -> i32 {
    let name = staging.file_name().unwrap().to_str().unwrap();
    let rest = name.strip_prefix("hatchling-staging-").unwrap();
    rest.split('-').next().unwrap().parse().unwrap()
}

/// What `/proc` says of `pid`: its command line, and the status lines
/// naming its state and parent.
fn who(pid: i32) -> String {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', " "))
        .unwrap_or_default();
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let lines: Vec<&str> = status
        .lines()
        .filter(|line| {
            ["State:", "PPid:", "NSpid:"]
                .iter()
                .any(|key| line.starts_with(key))
        })
        .collect();
    format!("pid {pid} `{cmdline}` {lines:?}")
}

/// A pid no process has: a child's, once it is reaped.
fn dead_pid() -> i32 {
    let mut child = Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    i32::try_from(child.id()).unwrap()
}

/// The killed broker: its run's real cell hangs; it prints the run's
/// directory once the cell's staging is in it, and waits.
fn killed(config: &Config) {
    let key = MasterKey::generate().unwrap();
    let env = RunEnv {
        spawner: common::spawner(),
        agent_bin: run_fixture_binary().unwrap(),
        run_root: config.scheduler.run_root(),
        timeout: Duration::from_secs(600),
    };
    std::thread::scope(|scope| {
        let launcher = CellLauncher::new(scope, config, &key, env);
        let _run = launcher
            .launch(RunRequest {
                schedule_id: String::from("s"),
                task: String::from("hang"),
                grants: Vec::new(),
                due: Timestamp::now(),
                missed: false,
                channel: String::from("local"),
                depth: 0,
                event: None,
                space: None,
                feed: None,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let run_dir = loop {
            let ready = std::fs::read_dir(config.scheduler.run_root())
                .ok()
                .and_then(|mut entries| entries.next())
                .and_then(Result::ok)
                .map(|entry| entry.path())
                .filter(|dir| staging_in(dir).is_some());
            if let Some(dir) = ready {
                break dir;
            }
            assert!(Instant::now() < deadline, "the run's cell never staged");
            std::thread::sleep(Duration::from_millis(50));
        };
        say(&format!("{READY}{}", run_dir.display()));
        std::thread::sleep(Duration::from_secs(600));
    });
}

/// Run only as a child: what [`MODE`] names, over [`BASE`].
#[test]
fn a_child() {
    let (Some(mode), Some(base)) = (std::env::var_os(MODE), std::env::var_os(BASE)) else {
        return;
    };
    let config = config(Path::new(&base));
    match mode.to_str() {
        Some("killed") => killed(&config),
        Some("start") => {
            for line in hatchling_broker::sweep::sweep_at_start(&config, None) {
                say(&line);
            }
        }
        Some("clean") => {
            let args: Vec<String> = std::env::var(ARGS)
                .unwrap_or_default()
                .split_whitespace()
                .map(String::from)
                .collect();
            match hatchling_broker::sweep::run_clean(&config, &args) {
                Ok(()) => say("clean: done"),
                Err(error) => say(&format!("clean: {error}")),
            }
        }
        other => unreachable!("no child plays {other:?}"),
    }
}

#[test]
fn the_next_start_sweeps_what_a_killed_broker_left_and_logs_each() {
    if std::env::var_os(MODE).is_some() || !guard_cell_available().unwrap() {
        return;
    }
    let base = base("killed");
    let config = config(&base);
    let mut broker = child("killed", &base, "").spawn().unwrap();
    let stdout = broker.stdout.take().unwrap();
    let (said, heard) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = said.send(line);
        }
    });
    // Everything the child says, until it says where its run is.
    let mut told = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(90);
    let run_dir = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match heard.recv_timeout(left) {
            Ok(line) => match line.strip_prefix(READY) {
                Some(dir) => break Some(PathBuf::from(dir)),
                None => told.push(line),
            },
            Err(_) => break None,
        }
    };
    // Killed mid-run, however the run went: nothing cleans up after it.
    broker.kill().unwrap();
    broker.wait().unwrap();
    let Some(run_dir) = run_dir else {
        unreachable!(
            "the broker's run never staged its cell; it said {told:?}; under its run root: {:?}",
            std::fs::read_dir(config.scheduler.run_root()).map(|entries| entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect::<Vec<_>>())
        );
    };
    let staging = staging_in(&run_dir).expect("the run's cell staging is left behind");
    // Its helper and cell die with it.
    let helper = staging_pid(&staging);
    let deadline = Instant::now() + Duration::from_secs(30);
    while hatchling_sys::pid_alive(helper) {
        if Instant::now() >= deadline {
            let seen = who(helper);
            // Leave no orphan behind: its cell dies with it.
            if let Some(pid) = rustix::process::Pid::from_raw(helper) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::Kill);
            }
            unreachable!("the run's helper outlived its broker: {seen}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(run_dir.exists() && staging.exists());
    let said = said_by("start", &base, "");
    assert!(
        said.lines()
            .any(|line| line == format!("removed {}", run_dir.display())),
        "{said}"
    );
    assert!(!run_dir.exists() && !staging.exists());
    assert!(config.scheduler.run_root().exists(), "the run root stays");
}

#[test]
fn clean_lists_then_removes_a_dead_brokers_leftovers_and_nothing_else() {
    if std::env::var_os(MODE).is_some() {
        return;
    }
    let base = base("clean");
    let config = config(&base);
    let tmp = base.join("tmp");
    let random = "a1".repeat(16);
    let dead = tmp.join(format!("hatchling-staging-{}-{random}", dead_pid()));
    std::fs::create_dir_all(dead.join(format!("cell-{random}")).join("root")).unwrap();
    let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
    let live = tmp.join(format!("hatchling-staging-{}-{random}", sleeper.id()));
    std::fs::create_dir_all(&live).unwrap();
    std::fs::create_dir_all(tmp.join("someone-elses")).unwrap();
    let ran = config.scheduler.run_root().join(format!("run-{}", "b2".repeat(12)));
    std::fs::create_dir_all(ran.join("tmp")).unwrap();

    let listed = said_by("clean", &base, "--dry-run");
    for leftover in [&dead, &ran] {
        assert!(
            listed.contains(&format!("would remove {}", leftover.display())),
            "{listed}"
        );
        assert!(leftover.exists(), "a dry run removes nothing");
    }
    assert!(!listed.contains(&live.display().to_string()), "{listed}");
    let refused = said_by("clean", &base, "--force");
    assert!(refused.contains("clean: broker usage error"), "{refused}");
    let cleaned = said_by("clean", &base, "");
    assert!(cleaned.contains("clean: done"), "{cleaned}");
    assert!(!dead.exists() && !ran.exists());
    assert!(live.exists(), "a live broker's staging stays");
    assert!(tmp.join("someone-elses").exists());
    sleeper.kill().unwrap();
    sleeper.wait().unwrap();
}
