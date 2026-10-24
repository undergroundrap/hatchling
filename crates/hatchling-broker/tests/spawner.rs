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

/// The spawner, started as the broker starts it.
fn spawner() -> Result<Spawner, hatchling_broker::Error> {
    let mut program = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    program.stdin(Stdio::null()).stdout(Stdio::null());
    Spawner::start_with(program)
}

/// The real spawner starts the real skill-cell helper, which reads the
/// spec the spawner wrote for it and says why its cell can't start; a
/// spec the helper doesn't start is refused before anything runs.
#[test]
fn the_real_spawner_starts_the_real_helper_on_the_spec_it_wrote() {
    let spawner = spawner().unwrap();
    let dir = run_dir("skill").unwrap();
    let mut config = CellConfig::skill("spawned").unwrap();
    config.skill_code = Some(PathBuf::from("/nonexistent/hatchling-skill-code"));
    let spec = serde_json::to_string(&config).unwrap();
    let (fds, mut errors) = stdio().unwrap();
    let process = spawner
        .helper(HelperKind::SkillCell, &spec, &dir, fds)
        .unwrap();
    let exit = process.wait_deadline(Duration::from_secs(30)).unwrap();
    let mut said = String::new();
    errors.read_to_string(&mut said).unwrap();
    assert_ne!(exit.code, 0, "{said}");
    assert!(said.contains("hatchling-broker skill-cell:"), "{said}");
    assert!(
        !said.contains("cell spec"),
        "the helper couldn't read its spec: {said}"
    );
    assert!(
        !dir.join("cell-spec.json").exists(),
        "the helper left its spec"
    );
    let (fds, _) = stdio().unwrap();
    let refused = spawner
        .helper(HelperKind::BrowserCell, &spec, &dir, fds)
        .unwrap_err();
    assert!(
        refused.contains("the spawner refused the spec"),
        "{refused}"
    );
    drop((process, spawner));
    let _ = std::fs::remove_dir_all(&dir);
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
