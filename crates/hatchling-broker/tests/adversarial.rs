//! cell adversarial suite: prove the cell holds.
//!
//! Every test here spawns real production cells — [`spawn_cell`] with
//! the real mount plan, id maps, and seccomp filter — and attacks
//! them with the [`hatchling_adversarial`] probe. Nothing is mocked:
//! the probe runs as PID 1 inside the cell's user, mount, and pid
//! namespaces, and the driver observes what the kernel and the broker
//! actually do.
//!
//! ## The two verdict styles
//!
//! *Report attacks* survive and write one JSON [`Report`] to stdout.
//! The driver captures it (see [`spawn_attack`]) and asserts the
//! attack-specific property plus the interior view (`uid == 0`,
//! `gid == 0`, `pid == 1` — the probe seeing itself as root/PID 1 is
//! the userns/pidns working).
//!
//! *Signal-verdict attacks* attempt a denied raw syscall and must die
//! with `SIGSYS` (`SECCOMP_RET_KILL_PROCESS`). The driver asserts the
//! exit code is `128 + SIGSYS` **and** that the probe's `READY` marker
//! reached the pipe — without the marker, a `SIGSYS` during probe
//! startup (a broken allowlist, not a working filter) would be
//! indistinguishable from a kill on the attack itself. Any report
//! from a signal-verdict attack is a survival: a hole.
//!
//! ## The report channel
//!
//! The production spawn path execs the probe with the test process's
//! file descriptors, so the driver briefly points fd 1 at a
//! per-attack pipe around [`spawn_cell`] (serialized by a
//! process-wide mutex — fds are process-global), then restores it. A
//! drain thread consumes the pipe concurrently so a full buffer can
//! never wedge the probe; EOF arrives when the cell's last writer
//! exits. No production code was changed for this: the channel uses
//! only fd inheritance, which the spawn path already provides.
//!
//! Because fd 1 is the channel, nothing else in this process may write
//! to stdout while it is redirected. libtest does (`test x ... ok`,
//! the 60 s slow-test notice) from its own threads, and on a real
//! kernel those lines landed in probe pipes. So this target is
//! `harness = false`: [`main`] runs the checks sequentially and
//! reports on stderr only. It stops at the first failed check: every
//! check spawns real cells with 30-60 s failure deadlines, so one broken
//! pipeline would otherwise cost that eleven times over (under mutation,
//! a timeout instead of a kill); filter by name to run a single check.
//! It honours libtest's selection arguments
//! (substring filters, `--exact`, `--skip`, `--list`), so
//! `cargo test -- <filter>` behaves as it does for every other binary.
//!
//! ## The uid remap, proved empirically
//!
//! The probe can only see its interior uid (0). The exterior view —
//! the host uid the kernel reports — is observed by the broker's
//! peer gate: [`session_uniform_deny`] drives a real session through
//! the real [`serve_connection`](hatchling_broker::serve_connection),
//! whose `SO_PEERCRED` check passes only if the cell's user maps to
//! the expected host uid. A successful handshake *is* the remap
//! proof; a broken map fails closed at the gate.
//!
//! ## Living inventories
//!
//! The socket sweep, mount-escape writes, setuid scan, and negative
//! sweep pin the cell's observed shape in committed JSON under
//! `tests/inventory/`. The driver fails loudly on drift and writes
//! nothing on its own; regeneration is opt-in
//! (`HATCHLING_ADVERSARIAL_REGEN=1`) and must follow human review —
//! the inventory is a security assertion, not a snapshot.
//!
//! ## Environment
//!
//! Cells need unprivileged user namespaces. When the kernel forbids
//! them the suite skips loudly; `HATCHLING_REQUIRE_CELL=1` turns the
//! skip into a failure for CI.

#[allow(
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::{
    broker_config, cell_config_for, guard_cell_available, mapped_agent_uid, probe_binary, test_dir,
};
use hatchling_adversarial::{
    Attack, HeldLimit, Outcome, Report, all_denied, check_inventory, cli_denied_syscalls,
    denied_syscalls, is_refused_write, parse_report, responses_uniform,
};
use hatchling_broker::{Config, bind_listener, serve_connection, spawn_cell};
use hatchling_core::LogLevel;
use hatchling_isolation::{Cell as _, CellSpec, Filter, IsolationBackend as _, Limits};
use hatchling_vault::MasterKey;

/// `128 + SIGSYS`: what [`spawn_cell`]'s reaper reports when the
/// seccomp filter kills the probe (`SECCOMP_RET_KILL_PROCESS`).
const SIGSYS_EXIT_CODE: i32 = 128 + 31;
/// A process killed by `SIGILL`, in the same encoding.
const SIGILL_EXIT_CODE: i32 = 128 + 4;

/// How long the driver waits for one probe cell. Every probe exits in
/// milliseconds (the slowest, the peer decoy, reads for at most 5 s);
/// the bound turns a cell that never finishes (a supervision bug, or a
/// mutant of one) into a failed check instead of an hour-long hang.
const CELL_WATCHDOG: Duration = Duration::from_secs(30);

/// What a spawned attack produced.
struct AttackResult {
    slug: String,
    /// `CellHandle::wait`'s status decoded by
    /// [`hatchling_sys::child_exit_code`]: the probe's exit code, or
    /// `128 + signal` when it died from a signal.
    exit_code: i32,
    /// Everything the probe wrote to fd 1, captured via a per-attack
    /// pipe (see the module docs).
    stdout: Vec<u8>,
}

/// Serializes the stdout-redirect window: file descriptors are
/// process-global, so two threads must never redirect fd 1 at once.
/// The window covers only pipe creation, the `dup2` pair, and
/// [`spawn_cell`] — the child inherits the pipe before the lock
/// releases; the (potentially long) wait happens outside it.
static STDOUT_REDIRECT_LOCK: Mutex<()> = Mutex::new(());

/// Spawn one probe cell for `attack`, capturing everything it writes
/// to stdout.
///
/// `socket_dir` is the host directory bind-mounted as `/sock`;
/// `agent_uid` is the host uid the cell's user maps to (what the
/// broker's peer gate will see).
fn spawn_attack(attack: Attack, socket_dir: &Path, agent_uid: u32) -> Result<AttackResult, String> {
    let probe = probe_binary()?;
    capture(
        attack,
        || {
            let config = cell_config_for(
                &probe,
                &attack.cell_id(),
                &socket_dir.join("broker.sock"),
                agent_uid,
            )?;
            spawn_cell(&config, LogLevel::Error)
                .map_err(|e| format!("spawn_cell for {}: {e}", attack.slug()))
        },
        // `wait` returns the raw `waitpid` status; decode it (a SIGSYS
        // kill of the agent arrives as the reaper's exit code 159 =
        // 0x9f00 raw).
        |handle| {
            handle
                .wait()
                .map(hatchling_sys::child_exit_code)
                .map_err(|e| e.to_string())
        },
    )
}

/// Spawn one probe cell for `attack` under the harness CLI filter
///, through the isolation backend: no broker socket, no relay
/// (the seccomp attacks need neither), the CLI cell's own `/proc` and
/// devices. Captures stdout as [`spawn_attack`] does.
fn spawn_cli_attack(attack: Attack) -> Result<AttackResult, String> {
    spawn_cli(attack, &cli_spec(attack)?)
}

/// Spawn one probe cell for `attack` under `spec`, as
/// [`spawn_cli_attack`] does.
fn spawn_cli(attack: Attack, spec: &CellSpec) -> Result<AttackResult, String> {
    let platform = hatchling_isolation::platform();
    capture(
        attack,
        || {
            platform
                .isolate(spec)
                .map_err(|e| format!("CLI cell for {}: {e}", attack.slug()))
        },
        |cell| cell.wait().map(|exit| exit.code).map_err(|e| e.to_string()),
    )
}

/// The cell spec a CLI-filter probe runs `attack` under: no broker
/// socket, no relay, the CLI cell's own `/proc` and devices.
fn cli_spec(attack: Attack) -> Result<CellSpec, String> {
    let platform = hatchling_isolation::platform();
    Ok(CellSpec {
        identity: platform
            .identity(None, 1)
            .map_err(|e| format!("cell identity: {e}"))?,
        program: probe_binary()?,
        argv: vec![String::from(hatchling_isolation::PROGRAM)],
        env: vec![(String::from("HATCHLING_CELL_ID"), attack.cell_id())],
        workdir: None,
        endpoint: None,
        exposures: Vec::new(),
        tools: Vec::new(),
        relay: None,
        filter: Filter::Cli,
        limits: Limits::default(),
    })
}

/// A cell that clears its parent-death signal still dies with its
/// supervisor: the cell's pid 1 is Hatchling's init, which keeps its own
/// signal, and the kernel kills a pid namespace whose init dies. The
/// probe clears its signal, says `CLEARED`, and spins for a minute; the
/// cell's reaper is killed, as the broker's death would kill it; every
/// process holding the probe's stdout must be gone within seconds.
fn a_cell_that_clears_its_death_signal_still_dies_with_its_supervisor() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let spec = cli_spec(Attack::ClearDeathSignal)?;
    let lock = STDOUT_REDIRECT_LOCK
        .lock()
        .map_err(|e| format!("stdout redirect lock poisoned: {e}"))?;
    let (read_fd, write_fd) = rustix::pipe::pipe().map_err(|e| format!("pipe: {e}"))?;
    let saved = rustix::io::dup(rustix::stdio::stdout()).map_err(|e| format!("dup stdout: {e}"))?;
    rustix::stdio::dup2_stdout(&write_fd).map_err(|e| format!("dup2 pipe -> stdout: {e}"))?;
    let started = hatchling_isolation::platform().isolate(&spec);
    let restore = rustix::stdio::dup2_stdout(&saved);
    drop(saved);
    drop(write_fd);
    drop(lock);
    restore.map_err(|e| format!("restore stdout: {e}"))?;
    let cell = started.map_err(|e| format!("CLI cell: {e}"))?;
    let (cleared_tx, cleared_rx) = std::sync::mpsc::channel();
    let (ended_tx, ended_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(read_fd);
        let mut seen = Vec::new();
        let mut buf = [0_u8; 256];
        while let Ok(read) = file.read(&mut buf) {
            if read == 0 {
                break;
            }
            seen.extend_from_slice(buf.get(..read).unwrap_or_default());
            if seen.starts_with(b"CLEARED\n") {
                let _ = cleared_tx.send(());
            }
        }
        let _ = ended_tx.send(seen);
    });
    cleared_rx
        .recv_timeout(CELL_WATCHDOG)
        .map_err(|_| String::from("the probe never said it cleared its death signal"))?;
    let reaper = rustix::process::Pid::from_raw(cell.pid())
        .ok_or_else(|| String::from("the cell has no reaper pid"))?;
    rustix::process::kill_process(reaper, rustix::process::Signal::Kill)
        .map_err(|e| format!("kill the reaper: {e}"))?;
    let killed = Instant::now();
    let ended = ended_rx.recv_timeout(Duration::from_secs(15));
    let _ = cell.wait();
    match ended {
        // Killed: the probe said CLEARED and nothing after. A probe that
        // ended on its own would have reported outliving its supervisor.
        Ok(seen) if seen == b"CLEARED\n" => Ok(()),
        Ok(seen) => Err(format!(
            "the probe ended on its own rather than with its cell: {:?}",
            String::from_utf8_lossy(&seen)
        )),
        Err(_) => Err(format!(
            "the cell outlived its supervisor: still running {:?} after its reaper was killed",
            killed.elapsed()
        )),
    }
}

/// Runs `start` with fd 1 redirected into a fresh pipe, then waits for
/// what it started with `wait`: the probe's exit code (`128 + signal`
/// for a kill) and everything it wrote to fd 1.
fn capture<H: Send + 'static>(
    attack: Attack,
    start: impl FnOnce() -> Result<H, String>,
    wait: fn(H) -> Result<i32, String>,
) -> Result<AttackResult, String> {
    let lock = STDOUT_REDIRECT_LOCK
        .lock()
        .map_err(|e| format!("stdout redirect lock poisoned: {e}"))?;
    // Not CLOEXEC: the probe must inherit fd 1 through fork+exec.
    let (read_fd, write_fd) =
        rustix::pipe::pipe().map_err(|e| format!("pipe for {}: {e}", attack.slug()))?;
    // Drain concurrently: a full pipe would wedge the probe, and the
    // report must survive even if the wait below takes a while.
    let drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut file = std::fs::File::from(read_fd);
        let _ = file.read_to_end(&mut buf);
        buf
    });
    let saved = rustix::io::dup(rustix::stdio::stdout()).map_err(|e| format!("dup stdout: {e}"))?;
    rustix::stdio::dup2_stdout(&write_fd).map_err(|e| format!("dup2 pipe -> stdout: {e}"))?;
    let started = start();
    // Restore while still holding the lock: no other thread may be
    // mid-redirect. Then drop the driver's write end — the cell's
    // inherited copies are the only writers left, so EOF follows the
    // cell's exit.
    let restore = rustix::stdio::dup2_stdout(&saved);
    drop(saved);
    drop(write_fd);
    drop(lock);
    restore.map_err(|e| format!("restore stdout: {e}"))?;
    let handle = started?;
    // Bounded: `wait` allows the production hour. A probe cell finishes
    // in milliseconds, so past `CELL_WATCHDOG` the cell is abandoned and
    // the check fails. The abandoned cell armed PDEATHSIG against this
    // (runner) thread and dies with the process, which the runner ends
    // right after (it stops at the first failed check).
    let slug = attack.slug();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(wait(handle));
    });
    let exit_code = match rx.recv_timeout(CELL_WATCHDOG) {
        Ok(waited) => waited.map_err(|e| format!("cell wait for {slug}: {e}"))?,
        Err(_) => {
            return Err(format!(
                "cell for {slug} still running after {}s: abandoned",
                CELL_WATCHDOG.as_secs()
            ));
        }
    };
    let stdout = drain
        .join()
        .map_err(|_| format!("stdout drain panicked for {}", attack.slug()))?;
    Ok(AttackResult {
        slug: attack.slug(),
        exit_code,
        stdout,
    })
}

/// Parse and validate a report attack's [`Report`]: the probe exited
/// 0, named the attack it ran, saw the cell's interior view
/// (`uid == 0`, `gid == 0`, `pid == 2`: the program under the cell's
/// init), and completed the attack.
/// Returns the outcome for attack-specific assertions.
fn report_of(result: &AttackResult) -> Result<Report, String> {
    if result.exit_code != 0 {
        return Err(format!(
            "probe for {} exited with code {} (expected 0); stdout: {:?}",
            result.slug,
            result.exit_code,
            String::from_utf8_lossy(&result.stdout),
        ));
    }
    let text =
        std::str::from_utf8(&result.stdout).map_err(|e| format!("probe stdout not UTF-8: {e}"))?;
    let report = parse_report(text.trim_end()).map_err(|e| format!("{}: {e}", result.slug))?;
    if report.attack != result.slug {
        return Err(format!(
            "probe reported attack {:?}, driver ran {:?}",
            report.attack, result.slug
        ));
    }
    if report.uid != 0 || report.gid != 0 || report.pid != 2 {
        return Err(format!(
            "probe saw uid={} gid={} pid={} — not the cell interior (0/0/2); \
             the probe did not run inside the cell",
            report.uid, report.gid, report.pid
        ));
    }
    if let Some(error) = report.error {
        return Err(format!("probe could not complete {}: {error}", result.slug));
    }
    Ok(report)
}

/// Assert a signal-verdict attack died with `SIGSYS` on the attack
/// itself: the exit code proves the kill, the `READY` marker proves
/// the kill happened after the probe started the attack (not during
/// startup). Any stdout beyond the marker — a survival report — is a
/// hole and fails.
fn assert_sigsys(result: &AttackResult) -> Result<(), String> {
    let expected_marker = format!("READY {}\n", result.slug);
    let stdout = String::from_utf8_lossy(&result.stdout);
    if result.exit_code != SIGSYS_EXIT_CODE {
        return Err(format!(
            "{}: exit code {} — expected {SIGSYS_EXIT_CODE} (SIGSYS); stdout: {stdout:?}",
            result.slug, result.exit_code
        ));
    }
    if stdout.as_ref() != expected_marker {
        return Err(format!(
            "{}: SIGSYS but stdout was {stdout:?} — expected exactly {expected_marker:?}; \
             the filter killed the probe before the attack ran (broken allowlist, not a working filter), \
             or the probe survived and reported",
            result.slug
        ));
    }
    Ok(())
}

/// [`assert_sigsys`] for a catalog entry, with the one syscall the kernel
/// exempts from seccomp.
///
/// `uretprobe` (Linux 6.11+) exists for the kernel's own uretprobe
/// trampolines, and newer kernels pass it through seccomp unfiltered so that
/// probes work in sandboxed processes. Called from anywhere but a trampoline,
/// the kernel kills the caller with `SIGILL`. So on such a kernel the probe
/// dies of `SIGILL` instead of `SIGSYS`: still dead on the attack itself (the
/// `READY` marker and nothing after it), with nothing gained. Older kernels
/// filter it like any other number, and the probe dies of `SIGSYS`.
fn assert_denied_kills(name: &str, result: &AttackResult) -> Result<(), String> {
    if name == "uretprobe" && result.exit_code == SIGILL_EXIT_CODE {
        let expected_marker = format!("READY {}\n", result.slug);
        let stdout = String::from_utf8_lossy(&result.stdout);
        if stdout.as_ref() == expected_marker {
            return Ok(());
        }
        return Err(format!(
            "{}: SIGILL but stdout was {stdout:?}, expected exactly {expected_marker:?}",
            result.slug
        ));
    }
    assert_sigsys(result)
}

/// The `uretprobe` exception is exact: `SIGILL` passes for `uretprobe` alone,
/// and only with the bare `READY` marker; any other syscall still needs
/// `SIGSYS`, and a probe that reported after the call fails. Needs no cell.
fn only_uretprobe_may_die_of_sigill() -> Result<(), String> {
    let result = |slug: &str, exit_code: i32, stdout: &str| AttackResult {
        slug: slug.to_owned(),
        exit_code,
        stdout: stdout.as_bytes().to_vec(),
    };
    assert_denied_kills(
        "uretprobe",
        &result("seccomp-335", SIGILL_EXIT_CODE, "READY seccomp-335\n"),
    )?;
    let other = result("seccomp-425", SIGILL_EXIT_CODE, "READY seccomp-425\n");
    if assert_denied_kills("io_uring_setup", &other).is_ok() {
        return Err(String::from(
            "SIGILL must not pass for any syscall but uretprobe",
        ));
    }
    let reported = result(
        "seccomp-335",
        SIGILL_EXIT_CODE,
        "READY seccomp-335\nSURVIVED\n",
    );
    if assert_denied_kills("uretprobe", &reported).is_ok() {
        return Err(String::from(
            "a probe that reported after uretprobe must fail",
        ));
    }
    assert_denied_kills(
        "uretprobe",
        &result("seccomp-335", SIGSYS_EXIT_CODE, "READY seccomp-335\n"),
    )
    .map_err(|e| format!("SIGSYS must still pass for uretprobe where it is filtered: {e}"))
}

/// Compare the observed outcome against the committed living
/// inventory, or regenerate it under `HATCHLING_ADVERSARIAL_REGEN=1`.
///
/// Regeneration overwrites the committed file and passes; the change
/// is then reviewed as a normal diff before commit. The inventory is
/// a security assertion — never regenerate blindly.
fn check_or_regen(attack: &str, actual: &serde_json::Value) -> Result<(), String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("inventory")
        .join(format!("{attack}.json"));
    if std::env::var("HATCHLING_ADVERSARIAL_REGEN").as_deref() == Ok("1") {
        let json = serde_json::to_string_pretty(actual)
            .map_err(|e| format!("serialize {attack} inventory: {e}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("create inventory dir: {e}"))?;
        }
        std::fs::write(&path, format!("{json}\n"))
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        return Ok(());
    }
    let expected_json = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "read committed inventory {}: {e} — run once with \
             HATCHLING_ADVERSARIAL_REGEN=1 after review to create it",
            path.display()
        )
    })?;
    let expected: serde_json::Value = serde_json::from_str(&expected_json)
        .map_err(|e| format!("parse committed inventory {}: {e}", path.display()))?;
    if attack == "mount-escape" || attack == "setuid-scan" {
        return check_inventory(
            attack,
            &fold_merged_usr(actual),
            &fold_merged_usr(&expected),
        );
    }
    check_inventory(attack, actual, &expected)
}

/// A path inventory (mount-escape, setuid-scan) with merged-`/usr` spellings
/// folded together.
///
/// A cell's library view is what the host's `ldd` reports, and merged-`/usr`
/// hosts spell the same file two ways: Ubuntu 24.04 says
/// `/lib/x86_64-linux-gnu/libc.so.6`, 26.04 `/usr/lib/x86_64-linux-gnu/libc.so.6`.
/// So one host's cell has `/lib` and no `/usr`, and the other's the reverse.
/// Which root the cell shows is the host's choice, not the boundary's: a path
/// under `/usr/lib/`, or a probe directly in `/usr`, is read as its `/lib`
/// spelling, and the entries are compared as a sorted multiset. Each entry
/// is still checked on its own, unfolded (a write refused, no setuid bit).
fn fold_merged_usr(writes: &serde_json::Value) -> serde_json::Value {
    let mut folded: Vec<serde_json::Value> = writes
        .as_array()
        .map(|all| {
            all.iter()
                .map(|write| {
                    let lib = write
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .and_then(|path| {
                            path.strip_prefix("/usr/lib/")
                                .or_else(|| {
                                    path.strip_prefix("/usr/")
                                        .filter(|rest| !rest.contains('/'))
                                })
                                .map(|rest| format!("/lib/{rest}"))
                        });
                    let mut write = write.clone();
                    if let (Some(lib), Some(object)) = (lib, write.as_object_mut()) {
                        object.insert(String::from("path"), serde_json::Value::String(lib));
                    }
                    write
                })
                .collect()
        })
        .unwrap_or_default();
    folded.sort_by_cached_key(ToString::to_string);
    serde_json::Value::Array(folded)
}

/// The fold is exact: each committed inventory and the one an Ubuntu 24.04
/// host produced for the same cell (`tests/inventory/<name>.lib-layout.json`,
/// its `/lib` spellings) fold to the same document, and one changed outcome
/// (a write that succeeds, a setuid bit) still shows. Needs no cell.
fn inventories_fold_only_the_lib_spelling() -> Result<(), String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("inventory");
    let read = |name: &str| -> Result<serde_json::Value, String> {
        let text =
            std::fs::read_to_string(dir.join(name)).map_err(|e| format!("read {name}: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("parse {name}: {e}"))
    };
    for (inventory, field, value) in [
        ("mount-escape", "errno", serde_json::Value::from(0)),
        ("setuid-scan", "setuid", serde_json::Value::from(true)),
    ] {
        let committed = read(&format!("{inventory}.json"))?;
        let other_host = read(&format!("{inventory}.lib-layout.json"))?;
        if committed == other_host {
            return Err(format!(
                "{inventory}: the two hosts' inventories should differ before folding"
            ));
        }
        if fold_merged_usr(&committed) != fold_merged_usr(&other_host) {
            return Err(format!(
                "{inventory}: the same cell on another merged-/usr host must fold to the committed inventory"
            ));
        }
        let mut changed = other_host;
        if let Some(first) = changed
            .as_array_mut()
            .and_then(|all| all.first_mut())
            .and_then(serde_json::Value::as_object_mut)
        {
            first.insert(String::from(field), value);
        }
        if fold_merged_usr(&committed) == fold_merged_usr(&changed) {
            return Err(format!(
                "{inventory}: a changed outcome must still change the folded inventory"
            ));
        }
    }
    Ok(())
}

/// Spawn one probe cell for a report attack against a bound-but-unserved
/// broker socket, and return its validated outcome.
///
/// The socket is bound so `connect` succeeds (backlog); nothing
/// accepts — these attacks never complete a handshake.
fn run_report_attack(attack: Attack) -> Result<Option<Outcome>, String> {
    if !guard_cell_available()? {
        return Ok(None);
    }
    let dir = test_dir(&format!("adversarial-{}", attack.slug()));
    let socket_dir = dir.join("sock");
    std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
    let _listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
    let result = spawn_attack(attack, &socket_dir, agent_uid)?;
    report_of(&result).map(|report| Some(report.outcome))
}

/// Spawn one probe cell against a live production broker: a server
/// thread accepts a single connection and runs the real
/// [`serve_connection`](hatchling_broker::serve_connection) — kernel
/// peer gate, handshake, session — so the probe observes production
/// behavior, not a test reimplementation.
///
/// The accept is non-blocking with a deadline: a probe that never
/// connects fails the test instead of hanging it. `serve_connection`
/// errors are expected (REJECT, peer-gate close) and left to the
/// probe to observe; only infrastructure failures fail here.
fn run_served_attack(
    attack: Attack,
    listener: &std::os::unix::net::UnixListener,
    config: &Config,
    key: &MasterKey,
    socket_dir: &Path,
    agent_uid: u32,
) -> Result<Report, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("nonblocking listener: {e}"))?;
    let slug = attack.slug();
    // Set once the probe cell has exited: a probe that died without
    // connecting ends the wait now, not at the 60 s deadline (under
    // mutation that wait was most of a mutant's time budget).
    let probe_done = AtomicBool::new(false);
    let (server_done, attack_result) = std::thread::scope(|scope| {
        let server = scope.spawn(|| -> Result<(), String> {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = serve_connection(
                            &stream,
                            config,
                            key,
                            &hatchling_broker::Skills::default(),
                        );
                        return Ok(());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if probe_done.load(Ordering::Acquire) {
                            // One last accept: the probe may have
                            // connected between the empty accept above
                            // and its exit; its connection is still in
                            // the backlog.
                            if let Ok((stream, _)) = listener.accept() {
                                let _ = serve_connection(
                                    &stream,
                                    config,
                                    key,
                                    &hatchling_broker::Skills::default(),
                                );
                                return Ok(());
                            }
                            return Err(format!("probe for {slug} exited without connecting"));
                        }
                        if Instant::now() >= deadline {
                            return Err(format!("probe for {slug} did not connect within 60s"));
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => return Err(format!("accept: {e}")),
                }
            }
        });
        let attack_result = spawn_attack(attack, socket_dir, agent_uid);
        probe_done.store(true, Ordering::Release);
        let server_done = server
            .join()
            .map_err(|_| String::from("broker server thread panicked"));
        (server_done, attack_result)
    });
    // A failed spawn leaves the server waiting out its deadline; its
    // "did not connect" error would hide the cause, so report the spawn
    // failure first.
    let attack_result = attack_result?;
    server_done??;
    report_of(&attack_result)
}

/// Socket reachability: only the broker socket may accept connections.
/// Everything else in the dictionary — other `/sock` names, library
/// paths that must not be sockets, abstract names — must refuse.
fn socket_sweep_reaches_only_the_broker() -> Result<(), String> {
    let Some(outcome) = run_report_attack(Attack::SocketSweep)? else {
        return Ok(());
    };
    let Outcome::SocketSweep { attempts } = outcome else {
        return Err(format!("expected SocketSweep outcome, got {outcome:?}"));
    };
    let reachable: Vec<&str> = attempts
        .iter()
        .filter(|attempt| attempt.connect == "ok")
        .map(|attempt| attempt.socket.as_str())
        .collect();
    if reachable != ["/sock/broker.sock"] {
        return Err(format!(
            "unexpected socket reachability: {reachable:?} — only /sock/broker.sock may accept"
        ));
    }
    let actual = serde_json::to_value(&attempts).map_err(|e| format!("serialize attempts: {e}"))?;
    check_or_regen("socket-sweep", &actual)?;
    Ok(())
}

/// Mount escape: every write to every visible mount must fail at the
/// syscall level with `EPERM`, `EACCES`, or `EROFS`; the only other
/// accepted refusals are the entry-pinned ones in
/// [`is_refused_write`] (absent path, the running `/agent` image, a
/// socket). Success (0) or any other errno is a hole or a bug.
fn mount_escape_all_writes_denied() -> Result<(), String> {
    let Some(outcome) = run_report_attack(Attack::MountEscape)? else {
        return Ok(());
    };
    let Outcome::MountEscape { writes } = outcome else {
        return Err(format!("expected MountEscape outcome, got {outcome:?}"));
    };
    let mut bad = Vec::new();
    for write in &writes {
        if !is_refused_write(&write.path, &write.op, write.errno) {
            bad.push(format!(
                "{} via {}: errno {} (expected EPERM/EACCES/EROFS, or a pinned \
                 non-mount refusal: see `is_refused_write`)",
                write.path, write.op, write.errno
            ));
        }
    }
    if !bad.is_empty() {
        return Err(format!(
            "writes not denied at the syscall level:\n{}",
            bad.join("\n")
        ));
    }
    let actual = serde_json::to_value(&writes).map_err(|e| format!("serialize writes: {e}"))?;
    check_or_regen("mount-escape", &actual)?;
    Ok(())
}

/// The denied-syscall catalog, proven one cell at a time: every
/// syscall the allowlist does not permit must kill the probe with
/// `SIGSYS` when attempted raw. The catalog is computed as the
/// kernel-header table minus the allowlist, so it tracks the
/// profile 1:1 by construction — this test proves the kernel
/// agrees, one probe cell per syscall.
///
/// Cells run serially: every probe's report travels through the
/// test process's stdout, which is a process-global redirect — the
/// [`spawn_attack`] lock serializes them. (One cell per denied number;
/// minutes, not seconds. This is a security validation suite, not a
/// unit test.)
///
/// Fails fast on the first number that does not `SIGSYS`: a broken
/// filter lets `-1`-argument calls really run, and a few of them are
/// not inert (`pause` blocks for the cell's hour-long runtime,
/// `msgget` leaves an IPC queue behind — see
/// `hatchling_sys::process::raw_syscall_inert`). In number order the
/// first survivor of a filter broken outright is `open` (nr 2), so
/// none of those is reached.
fn denied_syscalls_each_sigsys() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let catalog = denied_syscalls().map_err(|e| format!("denied syscall catalog: {e}"))?;
    if catalog.is_empty() {
        return Err(String::from(
            "denied catalog is empty — the syscall table or the allowlist is broken",
        ));
    }
    for entry in &catalog {
        let attack = Attack::Seccomp(entry.nr);
        let dir = test_dir(&format!("adversarial-seccomp-{}", entry.nr));
        let socket_dir = dir.join("sock");
        let outcome: Result<(), String> = (|| {
            std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
            // `spawn_cell` inspects the broker socket before it forks:
            // bind one (never served: the probe dies before connecting).
            let agent_uid = mapped_agent_uid();
            let config = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
            let _listener =
                bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
            let result = spawn_attack(attack, &socket_dir, agent_uid)?;
            assert_denied_kills(entry.name, &result)
        })();
        if let Err(e) = outcome {
            return Err(format!(
                "denied syscall {} (nr {}) did not SIGSYS (stopping at the first survivor): {e}",
                entry.name, entry.nr
            ));
        }
    }
    Ok(())
}

/// The harness CLI profile under the live kernel: every syscall
/// it neither allows nor decides by a rule kills the probe in a CLI cell.
/// In number order, stopping at the first survivor, for the reason
/// [`denied_syscalls_each_sigsys`] gives.
fn cli_denied_syscalls_each_sigsys() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let catalog = cli_denied_syscalls().map_err(|e| format!("CLI syscall catalog: {e}"))?;
    if catalog.is_empty() {
        return Err(String::from(
            "CLI catalog is empty — the syscall table or the CLI profile is broken",
        ));
    }
    for entry in &catalog {
        let outcome = spawn_cli_attack(Attack::Seccomp(entry.nr))
            .and_then(|result| assert_denied_kills(entry.name, &result));
        if let Err(e) = outcome {
            return Err(format!(
                "CLI-denied syscall {} (nr {}) did not SIGSYS (stopping at the first survivor): {e}",
                entry.name, entry.nr
            ));
        }
    }
    Ok(())
}

/// The CLI profile's argument rules under the live kernel, with the
/// probe's `-1` arguments: a `clone` asking for namespaces is killed;
/// another socket family, any `clone3`, another `prctl` option and
/// another `ioctl` request are each answered with their errno, and the
/// probe lives to report exactly that.
fn cli_rules_refuse_as_designed() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let number = |name: &str| {
        hatchling_sys::seccomp::syscall_number(name).ok_or_else(|| format!("no {name} here"))
    };
    let clone = spawn_cli_attack(Attack::Seccomp(number("clone")?))?;
    assert_sigsys(&clone).map_err(|e| format!("clone with namespace flags: {e}"))?;
    // EAFNOSUPPORT (a socket, a socket pair), ENOSYS, EPERM, ENOTTY.
    for (name, errno) in [
        ("socket", 97),
        ("socketpair", 97),
        ("clone3", 38),
        ("prctl", 1),
        ("ioctl", 25),
    ] {
        let nr = number(name)?;
        let result = spawn_cli_attack(Attack::Seccomp(nr))?;
        let marker = format!("READY {}\n", result.slug);
        let report = result
            .stdout
            .strip_prefix(marker.as_bytes())
            .ok_or_else(|| {
                format!(
                    "{name}: no READY marker; exit {}, stdout {:?}",
                    result.exit_code,
                    String::from_utf8_lossy(&result.stdout)
                )
            })
            .and_then(|rest| {
                report_of(&AttackResult {
                    slug: result.slug.clone(),
                    exit_code: result.exit_code,
                    stdout: rest.to_vec(),
                })
            })?;
        let expected = Outcome::SyscallSurvived {
            nr,
            ret: -i64::from(errno),
        };
        if report.outcome != expected {
            return Err(format!(
                "{name}: {:?}, expected {expected:?}",
                report.outcome
            ));
        }
    }
    Ok(())
}

/// The limits [`a_cell_is_held_to_its_limits_and_cannot_lift_them`]
/// gives its cell.
const HELD: Limits = Limits {
    processes: Some(512),
    address_space: Some(1 << 30),
    file_size: Some(1 << 30),
    open_files: Some(256),
};

/// A cell is held to the limits its spec gives it, each both soft and
/// hard, and can lift none of them: raising a hard limit takes
/// `CAP_SYS_RESOURCE` in the host's user namespace, which no cell holds.
fn a_cell_is_held_to_its_limits_and_cannot_lift_them() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let mut spec = cli_spec(Attack::RaiseLimits)?;
    spec.limits = HELD;
    let report = report_of(&spawn_cli(Attack::RaiseLimits, &spec)?)?;
    let held = |name: &str, value: Option<u64>| HeldLimit {
        name: name.to_owned(),
        soft: value,
        hard: value,
        raise_errno: 1,
    };
    let expected = Outcome::Limits {
        limits: vec![
            held("processes", HELD.processes),
            held("address-space", HELD.address_space),
            held("file-size", HELD.file_size),
            held("open-files", HELD.open_files),
        ],
    };
    if report.outcome != expected {
        return Err(format!(
            "the cell's limits: {:?}, expected {expected:?}",
            report.outcome
        ));
    }
    Ok(())
}

/// A root broker's cell has no supplementary groups: it drops them
/// before entering its user namespace, where it couldn't. Root only: an
/// unprivileged broker has none it may drop.
fn a_root_brokers_cell_has_no_supplementary_groups() -> Result<(), String> {
    if hatchling_sys::euid() != 0 || !guard_cell_available()? {
        return Ok(());
    }
    let report = report_of(&spawn_cli_attack(Attack::Groups)?)?;
    let none = Outcome::Groups {
        groups: String::new(),
    };
    if report.outcome != none {
        return Err(format!(
            "a root broker's cell kept supplementary groups: {:?}",
            report.outcome
        ));
    }
    Ok(())
}

/// Privilege escalation via a nested user namespace: `unshare(2)`
/// with `CLONE_NEWUSER` inside the cell must die with `SIGSYS` — the
/// cell can never gain a fresh namespace with full capabilities.
fn privesc_unshare_dies() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("adversarial-privesc-unshare");
    let socket_dir = dir.join("sock");
    std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
    let _listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
    let attack = Attack::PrivescUnshare;
    let result = spawn_attack(attack, &socket_dir, agent_uid)?;
    assert_sigsys(&result)
}

/// `bind(2)` on a fresh `AF_UNIX` socket must die with `SIGSYS`:
/// the filter constrains the cell to connecting, never binding.
fn net_bind_dies() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("adversarial-net-bind");
    let socket_dir = dir.join("sock");
    std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
    let _listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
    let attack = Attack::NetBind;
    let result = spawn_attack(attack, &socket_dir, agent_uid)?;
    assert_sigsys(&result)
}

/// `socket(AF_INET, ...)` must die with `SIGSYS`: the filter
/// constrains the family argument to `AF_UNIX`, so the cell cannot
/// even create a TCP socket, let alone connect one.
fn tcp_socket_dies() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("adversarial-tcp-socket");
    let socket_dir = dir.join("sock");
    std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
    let _listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
    let attack = Attack::TcpSocket;
    let result = spawn_attack(attack, &socket_dir, agent_uid)?;
    assert_sigsys(&result)
}

/// No setuid/setgid binaries exist anywhere the cell can see: there
/// is no privilege to gain even in theory. (The `setuid`/`setgid`
/// syscalls themselves are in the denied catalog, proven `SIGSYS`
/// by [`denied_syscalls_each_sigsys`].)
fn setuid_scan_finds_no_privileged_files() -> Result<(), String> {
    let Some(outcome) = run_report_attack(Attack::SetuidScan)? else {
        return Ok(());
    };
    let Outcome::SetuidScan { files } = outcome else {
        return Err(format!("expected SetuidScan outcome, got {outcome:?}"));
    };
    let privileged: Vec<&str> = files
        .iter()
        .filter(|file| file.setuid || file.setgid)
        .map(|file| file.path.as_str())
        .collect();
    if !privileged.is_empty() {
        return Err(format!(
            "setuid/setgid files visible in the cell: {privileged:?}"
        ));
    }
    let actual = serde_json::to_value(&files).map_err(|e| format!("serialize files: {e}"))?;
    check_or_regen("setuid-scan", &actual)?;
    Ok(())
}

/// Wrong-peer decoy: a socket whose broker expects a different uid
/// must close the connection with zero bytes — no rejection oracle,
/// no banner, nothing to fingerprint. The driver serves the decoy
/// with the real [`serve_connection`](hatchling_broker::serve_connection),
/// so the silent close observed is the production peer gate's.
fn peer_decoy_closes_silently() -> Result<(), String> {
    let attack = Attack::PeerDecoy;
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("adversarial-peer-decoy");
    let socket_dir = dir.join("sock");
    std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
    let agent_uid = mapped_agent_uid();
    // The decoy broker expects a uid the probe can never have.
    let decoy_uid = agent_uid.wrapping_add(1);
    let decoy_config = broker_config(&socket_dir.join("decoy.sock"), decoy_uid)?;
    let listener = bind_listener(&decoy_config).map_err(|e| format!("bind decoy socket: {e}"))?;
    // `spawn_cell` requires the cell's own broker socket: bind it
    // (never served) so the cell starts and reaches the decoy beside it.
    let broker = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
    let _broker_listener =
        bind_listener(&broker).map_err(|e| format!("bind broker socket: {e}"))?;
    let key = MasterKey::generate().map_err(|e| format!("master key: {e}"))?;
    let report = run_served_attack(
        attack,
        &listener,
        &decoy_config,
        &key,
        &socket_dir,
        agent_uid,
    )?;
    let Outcome::PeerDecoy {
        connect_errno,
        bytes_read,
        eof,
    } = report.outcome
    else {
        return Err(format!(
            "expected PeerDecoy outcome, got {:?}",
            report.outcome
        ));
    };
    if connect_errno.is_some() {
        return Err(format!(
            "decoy connect failed with errno {connect_errno:?} — the peer gate must accept \
             the connection at the socket level and close it itself, not refuse the connect"
        ));
    }
    if bytes_read != 0 || !eof {
        return Err(format!(
            "decoy peer saw {bytes_read} bytes, eof={eof} — the gate must answer zero bytes"
        ));
    }
    Ok(())
}

/// Authorized-peer garbage: a complete frame carrying invalid
/// handshake bytes must get the designed `REJECT` — not a crash, not
/// a hang, not a different shape. Served by the real
/// [`serve_connection`](hatchling_broker::serve_connection).
fn peer_garbage_gets_reject() -> Result<(), String> {
    let attack = Attack::PeerGarbage;
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("adversarial-peer-garbage");
    let socket_dir = dir.join("sock");
    std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
    let listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
    let key = MasterKey::generate().map_err(|e| format!("master key: {e}"))?;
    let report = run_served_attack(attack, &listener, &config, &key, &socket_dir, agent_uid)?;
    let Outcome::PeerGarbage { response } = report.outcome else {
        return Err(format!(
            "expected PeerGarbage outcome, got {:?}",
            report.outcome
        ));
    };
    if response != "reject" {
        return Err(format!(
            "garbage handshake got {response:?} — expected the designed \"reject\""
        ));
    }
    Ok(())
}

/// Unknown routes: every ungranted vault and egress op must answer
/// the uniform `Denied` — identical within each kind, no reason
/// strings, nothing that distinguishes one ungranted route from
/// another. (The `kind` tag echoes the request the client itself
/// sent; it is the protocol's routing, not a fingerprint.)
///
/// The handshake succeeding through the real peer gate is also the
/// empirical uid-remap proof: `serve_connection` authorizes the
/// kernel peer uid against the mapped uid before reading a byte.
fn session_uniform_deny() -> Result<(), String> {
    let attack = Attack::SessionUniform;
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("adversarial-session-uniform");
    let socket_dir = dir.join("sock");
    std::fs::create_dir_all(&socket_dir).map_err(|e| format!("socket dir: {e}"))?;
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_dir.join("broker.sock"), agent_uid)?;
    let listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
    let key = MasterKey::generate().map_err(|e| format!("master key: {e}"))?;
    let report = run_served_attack(attack, &listener, &config, &key, &socket_dir, agent_uid)?;
    let Outcome::SessionUniform { responses } = report.outcome else {
        return Err(format!(
            "expected SessionUniform outcome, got {:?}",
            report.outcome
        ));
    };
    if responses.len() != 4 {
        return Err(format!(
            "expected 4 session responses, got {}",
            responses.len()
        ));
    }
    let values: Vec<serde_json::Value> = responses
        .iter()
        .map(|response| {
            serde_json::from_str(response).map_err(|e| format!("response not JSON: {e}"))
        })
        .collect::<Result<_, _>>()?;
    if !all_denied(&values) {
        return Err(format!("not every response was Denied: {values:?}"));
    }
    let vault_count = values
        .iter()
        .filter(|value| value.get("kind").and_then(|kind| kind.as_str()) == Some("vault"))
        .count();
    let egress_count = values
        .iter()
        .filter(|value| value.get("kind").and_then(|kind| kind.as_str()) == Some("egress"))
        .count();
    if vault_count != 2 || egress_count != 2 {
        return Err(format!(
            "expected 2 vault + 2 egress responses, got {vault_count} + {egress_count}: {values:?}"
        ));
    }
    // Per-kind uniformity: vault denies must match vault denies,
    // egress match egress. The only cross-kind difference is the
    // `"kind"` routing tag.
    if !responses_uniform(&values) {
        return Err(format!(
            "denials differ within a kind — the broker fingerprints routes: {values:?}"
        ));
    }
    Ok(())
}

/// Negative sweep: no cron, at, systemd user units, writable `PATH`
/// entries, TCP listeners, FIFOs, or unexpected abstract sockets may
/// exist in the cell. Every check must come back absent.
fn negative_sweep_finds_no_residue() -> Result<(), String> {
    let Some(outcome) = run_report_attack(Attack::NegativeSweep)? else {
        return Ok(());
    };
    let Outcome::NegativeSweep { checks } = outcome else {
        return Err(format!("expected NegativeSweep outcome, got {outcome:?}"));
    };
    let present: Vec<&str> = checks
        .iter()
        .filter(|check| check.exists)
        .map(|check| check.path.as_str())
        .collect();
    if !present.is_empty() {
        return Err(format!("unexpected residue in the cell: {present:?}"));
    }
    let actual = serde_json::to_value(&checks).map_err(|e| format!("serialize checks: {e}"))?;
    check_or_regen("negative-sweep", &actual)?;
    Ok(())
}

/// One suite entry: its name and body.
type Check = (&'static str, fn() -> Result<(), String>);

/// Every check, in run order.
const CHECKS: &[Check] = &[
    (
        "socket_sweep_reaches_only_the_broker",
        socket_sweep_reaches_only_the_broker,
    ),
    (
        "mount_escape_all_writes_denied",
        mount_escape_all_writes_denied,
    ),
    (
        "inventories_fold_only_the_lib_spelling",
        inventories_fold_only_the_lib_spelling,
    ),
    ("denied_syscalls_each_sigsys", denied_syscalls_each_sigsys),
    (
        "only_uretprobe_may_die_of_sigill",
        only_uretprobe_may_die_of_sigill,
    ),
    (
        "cli_denied_syscalls_each_sigsys",
        cli_denied_syscalls_each_sigsys,
    ),
    ("cli_rules_refuse_as_designed", cli_rules_refuse_as_designed),
    (
        "a_root_brokers_cell_has_no_supplementary_groups",
        a_root_brokers_cell_has_no_supplementary_groups,
    ),
    (
        "a_cell_is_held_to_its_limits_and_cannot_lift_them",
        a_cell_is_held_to_its_limits_and_cannot_lift_them,
    ),
    (
        "a_cell_that_clears_its_death_signal_still_dies_with_its_supervisor",
        a_cell_that_clears_its_death_signal_still_dies_with_its_supervisor,
    ),
    ("privesc_unshare_dies", privesc_unshare_dies),
    ("net_bind_dies", net_bind_dies),
    ("tcp_socket_dies", tcp_socket_dies),
    (
        "setuid_scan_finds_no_privileged_files",
        setuid_scan_finds_no_privileged_files,
    ),
    ("peer_decoy_closes_silently", peer_decoy_closes_silently),
    ("peer_garbage_gets_reject", peer_garbage_gets_reject),
    ("session_uniform_deny", session_uniform_deny),
    (
        "negative_sweep_finds_no_residue",
        negative_sweep_finds_no_residue,
    ),
];

/// The libtest command-line subset this runner honours, so
/// `cargo test -- <args>` selects checks as it selects tests in every
/// other binary: substring filters, `--exact`, `--skip <filter>`, and
/// `--list`. Other flags are ignored, including the separate value of
/// the libtest flags that take one (a value must never become a filter).
#[derive(Default)]
struct Args {
    filters: Vec<String>,
    skips: Vec<String>,
    exact: bool,
    list: bool,
}

impl Args {
    fn parse(mut args: impl Iterator<Item = String>) -> Self {
        let mut parsed = Self::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--exact" => parsed.exact = true,
                "--list" => parsed.list = true,
                "--skip" => parsed.skips.extend(args.next()),
                "--test-threads" | "--color" | "--format" | "--logfile" | "--shuffle-seed"
                | "-Z" => {
                    let _ = args.next();
                }
                flag if flag.starts_with('-') => {
                    if let Some(skip) = flag.strip_prefix("--skip=") {
                        parsed.skips.push(skip.to_owned());
                    }
                }
                filter => parsed.filters.push(filter.to_owned()),
            }
        }
        parsed
    }

    fn matches(&self, name: &str, pattern: &str) -> bool {
        if self.exact {
            name == pattern
        } else {
            name.contains(pattern)
        }
    }

    fn selects(&self, name: &str) -> bool {
        (self.filters.is_empty() || self.filters.iter().any(|f| self.matches(name, f)))
            && !self.skips.iter().any(|s| self.matches(name, s))
    }
}

/// Sequential runner (see "The report channel" in the module docs).
/// Writes only to stderr: stdout is the probes' report channel.
fn main() -> ExitCode {
    let args = Args::parse(std::env::args().skip(1));
    let selected: Vec<&Check> = CHECKS
        .iter()
        .filter(|(name, _)| args.selects(name))
        .collect();
    if args.list {
        // No cell runs under --list, so stdout is free for the listing.
        let mut out = std::io::stdout();
        for (name, _) in &selected {
            let _ = writeln!(out, "{name}: test");
        }
        return ExitCode::SUCCESS;
    }
    let mut err = std::io::stderr();
    let total = selected.len();
    let _ = writeln!(err, "\nrunning {total} adversarial checks");
    let mut failed = Vec::new();
    for (index, entry) in selected.iter().enumerate() {
        let (name, check) = **entry;
        let start = Instant::now();
        let result = check();
        let secs = start.elapsed().as_secs_f64();
        match result {
            Ok(()) => {
                let _ = writeln!(err, "test {name} ... ok ({secs:.1}s)");
            }
            Err(e) => {
                let _ = writeln!(err, "test {name} ... FAILED ({secs:.1}s)\n  {e}");
                failed.push(name);
                let not_run = total - index - 1;
                if not_run > 0 {
                    let _ = writeln!(
                        err,
                        "  stopping at the first failure: {not_run} checks not run \
                         (filter by name to run one)"
                    );
                }
                break;
            }
        }
    }
    if failed.is_empty() {
        let _ = writeln!(err, "\nadversarial result: ok. {total} checks");
        ExitCode::SUCCESS
    } else {
        let _ = writeln!(
            err,
            "\nadversarial result: FAILED. {} failed: {failed:?}",
            failed.len()
        );
        ExitCode::FAILURE
    }
}
