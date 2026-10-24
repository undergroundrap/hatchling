//! The spawner as the broker starts it (WO-017, step 3): the real binary
//! as `spawner`, starting the real helpers.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use hatchling_broker::CellConfig;
use hatchling_broker::spawner::{HelperKind, Spawner};
use hatchling_isolation::Supervised as _;

#[allow(
    dead_code,
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

/// A run directory of this test's own.
fn run_dir(tag: &str) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "hatchling-spawner-test-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Stdio for a helper, and the read end of its stderr.
fn stdio() -> io::Result<([OwnedFd; 3], File)> {
    let null = |write: bool| {
        File::options()
            .read(!write)
            .write(write)
            .open("/dev/null")
            .map(OwnedFd::from)
    };
    let (read, write) = hatchling_sys::pipe().map_err(io::Error::other)?;
    Ok(([null(false)?, null(true)?, write], File::from(read)))
}

/// The spawner, started as the broker starts it, with a broker's settings
/// under `dir`: it reads its policy from them.
fn spawner(dir: &std::path::Path) -> Result<Spawner, hatchling_broker::Error> {
    spawner_with(dir, rustix::process::getuid().as_raw(), None)
}

/// [`spawner`], for agents as `agent_uid`, running `agent` if given.
fn spawner_with(
    dir: &std::path::Path,
    agent_uid: u32,
    agent: Option<&std::path::Path>,
) -> Result<Spawner, hatchling_broker::Error> {
    let path = |name: &str| dir.join(name).display().to_string();
    let mut program = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    program
        .env("HATCHLING_BROKER_SOCKET", path("broker.sock"))
        .env("HATCHLING_AGENT_UID", agent_uid.to_string())
        .env("HATCHLING_VAULT_DIR", path("vault"))
        .env("HATCHLING_LOG_LEVEL", "error")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    if let Some(agent) = agent {
        program.env("HATCHLING_AGENT_BIN", agent);
    }
    Spawner::start_with(program)
}

/// A run directory of the broker's own, as the spawner's policy knows
/// one: in the temp directory, named as one, owner-only.
fn broker_run_dir(tag: &str) -> io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!(
        "hatchling-skill-code-test-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// The real spawner starts the real skill-cell helper, which reads the
/// spec the spawner wrote for it and says why its cell can't start; a
/// spec the helper doesn't start is refused before anything runs.
#[test]
fn the_real_spawner_starts_the_real_helper_on_the_spec_it_wrote() {
    let dir = run_dir("skill").unwrap();
    let spawner = spawner(&dir).unwrap();
    let code = broker_run_dir("skill").unwrap();
    let mut config = CellConfig::skill("spawned").unwrap();
    config.agent_bin = code.join("skill");
    config.skill_code = Some(code.clone());
    let spec = serde_json::to_string(&config).unwrap();
    let (fds, mut errors) = stdio().unwrap();
    let process = spawner.helper(HelperKind::SkillCell, &spec, fds).unwrap();
    let exit = process.wait_deadline(Duration::from_secs(30)).unwrap();
    let mut said = String::new();
    errors.read_to_string(&mut said).unwrap();
    assert_ne!(exit.code, 0, "{said}");
    // It got as far as its cell's own spawn: past its spec, and past its
    // watch on the spawner, its parent (an empty program is the first
    // thing the spawn itself refuses).
    assert!(
        !said.contains("cannot watch"),
        "the helper couldn't watch its parent: {said}"
    );
    assert!(
        said.contains("cannot canonicalize agent binary"),
        "the helper stopped before its cell: {said}"
    );
    assert!(
        !dir.join("cell-spec.json").exists(),
        "the helper left its spec"
    );
    let (fds, _) = stdio().unwrap();
    let refused = spawner
        .helper(HelperKind::BrowserCell, &spec, fds)
        .unwrap_err();
    assert!(
        refused.contains("the spawner refused the spec"),
        "{refused}"
    );
    // A spec naming a path the policy doesn't give a cell: refused.
    config.skill_code = Some(PathBuf::from("/etc"));
    let outside = serde_json::to_string(&config).unwrap();
    let (fds, _) = stdio().unwrap();
    let refused = spawner
        .helper(HelperKind::SkillCell, &outside, fds)
        .unwrap_err();
    assert!(refused.contains("/etc isn't a path"), "{refused}");
    drop((process, spawner));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&code);
}

/// The real spawner starts the broker's own agent cell through the real
/// `agent-run` helper, which hands back the cell's connection: the real
/// agent completes its handshake over it, and the helper exits with it.
#[test]
fn the_real_spawner_starts_the_brokers_agent_cell_and_hands_back_its_connection()
-> Result<(), String> {
    if !common::guard_cell_available()? {
        return Ok(());
    }
    let dir = run_dir("agent").map_err(|e| format!("run dir: {e}"))?;
    let agent = common::agent_binary()?;
    let uid = common::mapped_agent_uid();
    let spawner =
        spawner_with(&dir, uid, Some(&agent)).map_err(|e| format!("start the spawner: {e}"))?;
    let socket = dir.join("broker.sock");
    let config = common::broker_config(&socket, uid)?;
    let mut cell = common::cell_config(&socket, uid)?;
    cell.connected = true;
    let (process, connection) = spawner.agent_cell(&cell)?;
    connection
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("set timeout: {e}"))?;
    let cell_id = cell.cell_id.parse().map_err(|e| format!("cell id: {e}"))?;
    let agent_cell = hatchling_broker::AgentCell {
        connection,
        cell_id,
        pid: 0,
    };
    let key = hatchling_vault::MasterKey::generate().map_err(|e| format!("key: {e}"))?;
    // The agent exits after its handshake; the session then ends at EOF.
    hatchling_broker::serve_cell_connection(
        &agent_cell,
        &config,
        &key,
        &hatchling_broker::Skills::default(),
        None,
        None,
    )
    .map_err(|e| format!("serve the cell's session: {e}"))?;
    let exit = process
        .wait_deadline(Duration::from_secs(30))
        .map_err(|e| format!("wait for the helper: {e}"))?;
    if exit.code != 0 {
        return Err(format!("the helper exited {}", exit.code));
    }
    drop(spawner);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// The spawner runs only as the broker starts it.
#[test]
fn the_spawner_refuses_to_run_unless_the_broker_started_it() {
    let output = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("spawner")
        .env_remove("HATCHLING_SPAWNER_FD")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only the broker starts it"), "{stderr}");
}
