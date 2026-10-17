//! End-to-end: the real `hatchling-agent` binary against a fixture broker.
//!
//! Each test spawns the compiled binary with a controlled environment, so
//! these prove the artifact that ships — not just the library — starts,
//! authorizes its broker, completes the handshake, and fails closed.
//!
//! The agent is fail-closed by design: it must exit, never hang. A 30s
//! watchdog kills a stuck binary and fails the test, because a hang is a
//! bug, not a slow test.

pub(crate) mod common;

use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::{BrokerBehavior, FixtureBroker, Scratch};

/// Path to the real binary under test, built by Cargo for this target.
const fn agent_binary() -> &'static str {
    env!("CARGO_BIN_EXE_hatchling-agent")
}

/// Spawn the agent with the given environment variables.
fn spawn_agent(vars: &[(&str, &str)]) -> std::io::Result<Child> {
    let mut command = Command::new(agent_binary());
    for (key, value) in vars {
        command.env(key, value);
    }
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Wait for the agent to exit, enforcing the never-hang contract.
///
/// Polls with `try_wait`; stderr is drained after exit. If the deadline
/// passes first, the child is killed and reaped, and the wait reports a
/// timeout — a hang is a bug, not a slow test.
fn wait_for_exit(child: &mut Child) -> std::io::Result<Output> {
    const TIMEOUT: Duration = Duration::from_secs(30);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            // The child has exited, so no more output is coming: drain
            // both pipes to EOF without deadlock risk.
            let mut stdout = Vec::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = std::io::Read::read_to_end(&mut pipe, &mut stdout);
            }
            let mut stderr = Vec::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = std::io::Read::read_to_end(&mut pipe, &mut stderr);
            }
            return Ok(Output {
                status,
                stdout,
                stderr,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "agent did not exit within 30s",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Environment for the agent: cell id, broker socket, broker uid, log level.
const fn agent_env<'a>(
    socket_path: &'a str,
    broker_uid: &'a str,
    log_level: &'a str,
) -> [(&'static str, &'a str); 4] {
    [
        ("HATCHLING_CELL_ID", "cell-e2e"),
        ("HATCHLING_BROKER_SOCKET", socket_path),
        ("HATCHLING_BROKER_UID", broker_uid),
        ("HATCHLING_LOG_LEVEL", log_level),
    ]
}

/// Path to the real broker binary, derived from the agent binary's
/// directory. Both are built by `cargo build --workspace`; a lone
/// `cargo test -p hatchling-agent` does not build the broker, and the
/// test says so instead of failing obscurely.
fn broker_binary() -> std::path::PathBuf {
    std::path::Path::new(agent_binary()).with_file_name("hatchling-broker")
}

/// Spawn the real broker with the given environment variables.
///
/// `HATCHLING_AGENT_BIN` is scrubbed: the broker's `main` spawns a full
/// cell when it is set, and these tests want the bare handshake server.
/// Without the scrub the test inherits the ambient environment — under
/// `cargo mutants` (which exports `HATCHLING_AGENT_BIN`) the broker
/// blocks in `spawn_cell` and never reaches `listen()`.
fn spawn_broker(vars: &[(&str, String)]) -> std::io::Result<Child> {
    // A starting broker sweeps its `TMPDIR` (WO-012): this test process's
    // own, never a shared one, unless the test gives one.
    let tmp = std::env::temp_dir().join(format!("hatchling-e2e-{}-tmp", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let mut command = Command::new(broker_binary());
    command.env("TMPDIR", &tmp);
    for (key, value) in vars {
        command.env(key, value);
    }
    command.env_remove("HATCHLING_AGENT_BIN");
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

/// Waits until the broker is accepting connections, so the test does not
/// race the broker's bind->listen window.
///
/// The socket file appears at `bind()`, but a `connect()` before
/// `listen()` gets `ECONNREFUSED` — so a bare file-exists check is not
/// enough. Under load (e.g. a mutation baseline compiling in the
/// background) the broker can lose that race and the agent's single
/// connect attempt fails. The probe connection is closed immediately;
/// the broker drops it at handshake EOF and keeps serving.
fn wait_for_listener(path: &str) {
    use std::os::unix::net::UnixStream;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(stream) = UnixStream::connect(path) {
            drop(stream);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "broker did not start listening on {path} in time"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// Stops the broker child. The broker serves forever by design, so the
/// test reaps it explicitly.
fn stop_broker(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Environment for the broker: socket, agent uid, log level, and the
/// vault root (required config; derived from the socket path so each
/// test gets an isolated store).
fn broker_env(socket_path: &str, agent_uid: &str, log_level: &str) -> Vec<(&'static str, String)> {
    vec![
        ("HATCHLING_BROKER_SOCKET", socket_path.to_owned()),
        ("HATCHLING_AGENT_UID", agent_uid.to_owned()),
        ("HATCHLING_LOG_LEVEL", log_level.to_owned()),
        ("HATCHLING_VAULT_DIR", vault_dir(socket_path)),
    ]
}

fn vault_dir(socket_path: &str) -> String {
    format!("{socket_path}-vault")
}

/// Everything a real broker run leaves on disk: its socket, its vault
/// directory, and the key file's own directory beside the vault.
/// Removed when the test ends, pass or fail.
fn real_broker_scratch(socket_path: &str) -> [Scratch; 3] {
    let vault = vault_dir(socket_path);
    let key = hatchling_broker::VaultConfig::default_key_file(std::path::Path::new(&vault));
    let key_dir = key.parent().unwrap_or(&key).to_path_buf();
    [
        Scratch::new(socket_path),
        Scratch::new(key_dir),
        Scratch::new(vault),
    ]
}

/// the scratch guard removes a file or a whole directory, both
/// when it is dropped and (for a stale one) when it is made.
#[test]
fn scratch_paths_are_removed_files_and_directories_alike() {
    let base = std::env::temp_dir().join(format!("hlfx-scratch-{}", std::process::id()));
    let (file, dir) = (base.with_extension("file"), base.with_extension("dir"));
    std::fs::write(&file, b"stale").unwrap();
    let stale = Scratch::new(&file);
    assert!(!file.exists(), "a stale file must be cleared on guard");
    drop(stale);
    let guards = [Scratch::new(&file), Scratch::new(&dir)];
    std::fs::write(&file, b"x").unwrap();
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(dir.join("nested").join("file"), b"x").unwrap();
    drop(guards);
    assert!(!file.exists(), "{} was left behind", file.display());
    assert!(!dir.exists(), "{} was left behind", dir.display());
}

/// The real agent binary against the real broker binary: full handshake,
/// exit 0, readiness logged. This is the acceptance test for the
/// broker's half of the trust boundary.
#[test]
fn e2e_real_broker_happy_path() {
    let broker_bin = broker_binary();
    assert!(
        broker_bin.is_file(),
        "hatchling-broker binary not found at {} — run `cargo build --workspace` first",
        broker_bin.display()
    );
    let socket = format!("/tmp/hatchling-e2e-real-{}-happy.sock", std::process::id());
    let _scratch = real_broker_scratch(&socket);
    let uid = rustix::process::getuid().as_raw().to_string();

    let mut broker = spawn_broker(&broker_env(&socket, &uid, "info")).unwrap();
    wait_for_listener(&socket);

    let mut agent = spawn_agent(&agent_env(&socket, &uid, "info")).unwrap();
    let output = wait_for_exit(&mut agent).unwrap();
    stop_broker(&mut broker);
    let _ = std::fs::remove_file(&socket);

    assert!(
        output.status.success(),
        "agent failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ready"),
        "missing readiness log, stdout was: {stdout}"
    );
}

/// The real broker answers a wrong agent uid with a silent close; the
/// agent fails closed on the resulting EOF.
#[test]
fn e2e_real_broker_wrong_agent_uid_fails_closed() {
    let broker_bin = broker_binary();
    assert!(
        broker_bin.is_file(),
        "hatchling-broker binary not found at {} — run `cargo build --workspace` first",
        broker_bin.display()
    );
    let socket = format!(
        "/tmp/hatchling-e2e-real-{}-wrong-uid.sock",
        std::process::id()
    );
    let _scratch = real_broker_scratch(&socket);
    let real_uid = rustix::process::getuid().as_raw();
    // Deliberately wrong: the kernel will report the real uid.
    let wrong_uid = real_uid.wrapping_add(1).to_string();
    let real_uid = real_uid.to_string();

    let mut broker = spawn_broker(&broker_env(&socket, &wrong_uid, "error")).unwrap();
    wait_for_listener(&socket);

    // The agent side is configured correctly; the broker rejects it.
    let mut agent = spawn_agent(&agent_env(&socket, &real_uid, "error")).unwrap();
    let output = wait_for_exit(&mut agent).unwrap();
    stop_broker(&mut broker);
    let _ = std::fs::remove_file(&socket);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("broker handshake I/O failed"),
        "unexpected stderr: {stderr}"
    );
}

/// Happy path: the real binary completes the handshake, logs readiness,
/// and exits 0.
#[test]
fn e2e_happy_path_exits_zero() {
    let broker = FixtureBroker::spawn("e2e-happy", BrokerBehavior::Welcome).unwrap();
    let uid = rustix::process::getuid().as_raw().to_string();
    let mut child = spawn_agent(&agent_env(&broker.socket_path, &uid, "info")).unwrap();
    let output = wait_for_exit(&mut child).unwrap();
    assert!(
        output.status.success(),
        "agent exited non-zero: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ready"),
        "missing readiness log, stdout was: {stdout}"
    );
}

/// A wrong configured broker uid fails closed: non-zero exit, and the
/// kernel-reported mismatch is named on stderr.
#[test]
fn e2e_wrong_broker_uid_fails_closed() {
    let broker = FixtureBroker::spawn("e2e-wrong-uid", BrokerBehavior::Welcome).unwrap();
    // Deliberately wrong: the kernel will report our real uid.
    let wrong_uid = rustix::process::getuid()
        .as_raw()
        .wrapping_add(1)
        .to_string();
    let mut child = spawn_agent(&agent_env(&broker.socket_path, &wrong_uid, "error")).unwrap();
    let output = wait_for_exit(&mut child).unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("broker uid mismatch"),
        "unexpected stderr: {stderr}"
    );
}

/// A rejecting broker fails the binary closed with the typed reason.
#[test]
fn e2e_broker_reject_fails_closed() {
    let broker = FixtureBroker::spawn(
        "e2e-reject",
        BrokerBehavior::Reject("untrusted cell".to_owned()),
    )
    .unwrap();
    let uid = rustix::process::getuid().as_raw().to_string();
    let mut child = spawn_agent(&agent_env(&broker.socket_path, &uid, "error")).unwrap();
    let output = wait_for_exit(&mut child).unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("broker rejected handshake: untrusted cell"),
        "unexpected stderr: {stderr}"
    );
}

/// No broker listening: fail-closed, non-zero, connection error on stderr.
#[test]
fn e2e_missing_broker_fails_closed() {
    let socket = format!(
        "/tmp/hatchling-fixture-{}-e2e-absent.sock",
        std::process::id()
    );
    let _ = std::fs::remove_file(&socket);
    let uid = rustix::process::getuid().as_raw().to_string();
    let mut child = spawn_agent(&agent_env(&socket, &uid, "error")).unwrap();
    let output = wait_for_exit(&mut child).unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to connect to broker socket"),
        "unexpected stderr: {stderr}"
    );
}
