//! The vault keeper as the broker starts it (WO-017, step 4): the real
//! binary as `vault-keeper`, on a socket pair, with an operator's
//! environment.

use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_vault::{SealedVault, Store, StoreKeys};

/// A scratch directory of this test's own.
fn scratch(name: &str) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("hatchling-keeper-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sock"))?;
    Ok(dir)
}

/// The environment of a broker with one egress grant and artifacts on.
fn environment(scratch: &Path) -> Vec<(&'static str, String)> {
    let path = |name: &str| scratch.join(name).to_string_lossy().into_owned();
    vec![
        ("TMPDIR", path("tmp")),
        ("HATCHLING_BROKER_SOCKET", path("sock/broker.sock")),
        (
            "HATCHLING_AGENT_UID",
            rustix::process::getuid().as_raw().to_string(),
        ),
        ("HATCHLING_LOG_LEVEL", String::from("error")),
        ("HATCHLING_VAULT_DIR", path("vault")),
        ("HATCHLING_EGRESS_GRANTS", String::from("localhost:18089")),
        ("HATCHLING_ARTIFACT_DIR", path("artifacts")),
    ]
}

/// The keeper, started as the broker starts it, and the broker's end.
fn start(scratch: &Path) -> io::Result<(Child, UnixStream)> {
    let (ours, theirs) = UnixStream::pair()?;
    hatchling_sys::keep_across_exec(theirs.as_fd()).map_err(io::Error::other)?;
    let child = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("vault-keeper")
        .envs(environment(scratch))
        .env("HATCHLING_KEEPER_FD", theirs.as_raw_fd().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    drop(theirs);
    ours.set_read_timeout(Some(Duration::from_secs(20)))?;
    Ok((child, ours))
}

/// Waits for the keeper to leave, at most 10 seconds.
fn exits(mut child: Child) -> io::Result<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(io::Error::other("the keeper didn't leave"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Sends `request`, and reads the reply.
fn ask(stream: &UnixStream, request: &str) -> io::Result<String> {
    write_frame(stream, request.as_bytes()).map_err(io::Error::other)?;
    let reply = read_frame(stream).map_err(io::Error::other)?;
    String::from_utf8(reply).map_err(io::Error::other)
}

/// Seals `secrets` in the vault under `dir` the way an operator does,
/// with `migrate-vault`, which also makes the vault's key.
fn sealed_vault(dir: &Path, secrets: &[(&str, &str)]) -> io::Result<PathBuf> {
    let vault = dir.join("vault");
    std::fs::create_dir(&vault)?;
    std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o700))?;
    for (name, value) in secrets {
        std::fs::write(vault.join(name), value)?;
    }
    let migrated = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("migrate-vault")
        .envs(environment(dir))
        .env_remove("HATCHLING_AGENT_BIN")
        .output()?;
    if !migrated.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&migrated.stderr).into_owned(),
        ));
    }
    Ok(vault)
}

/// The keeper hands the broker every store's key but the secrets', reads
/// a configured credential and refuses any other, records both, keeps
/// its memory closed, and leaves when the broker does.
#[test]
fn the_keeper_keeps_the_secrets_and_releases_only_what_is_configured() {
    let dir = scratch("serve").unwrap();
    let vault = sealed_vault(
        &dir,
        &[
            ("egress-localhost-18089", "credential"),
            ("unlisted", "kept"),
        ],
    )
    .unwrap();
    let (child, stream) = start(&dir).unwrap();
    let first = read_frame(&stream).unwrap();
    let (kind, handed) = first.split_first().unwrap();
    assert_eq!(*kind, 0, "{}", String::from_utf8_lossy(handed));
    let keys = StoreKeys::from_bytes(handed).unwrap();
    assert_eq!(
        keys.stores().map(|(store, _)| store).collect::<Vec<_>>(),
        [Store::ARTIFACTS]
    );
    assert!(
        SealedVault::open(&vault, &keys).is_err(),
        "the broker can open the secrets"
    );
    // Its memory is closed to processes of the same user.
    if rustix::process::getuid().as_raw() != 0 {
        let mem = std::fs::metadata(format!("/proc/{}/mem", child.id())).unwrap();
        assert_eq!(mem.uid(), 0, "the keeper is dumpable");
    }
    // A configured credential, read for its purpose.
    let reply = ask(
        &stream,
        r#"{"request":"read","purpose":"egress","names":["egress-localhost-18089"]}"#,
    )
    .unwrap();
    assert_eq!(reply, r#"{"reply":"read","found":[true]}"#);
    assert_eq!(read_frame(&stream).unwrap(), b"credential");
    // Anything else is refused.
    for refused in [
        r#"{"request":"read","purpose":"egress","names":["unlisted"]}"#,
        r#"{"request":"read","purpose":"anything","names":["unlisted"]}"#,
    ] {
        let reply = ask(&stream, refused).unwrap();
        assert!(reply.starts_with(r#"{"reply":"refused""#), "{reply}");
        assert!(!reply.contains("kept"), "{reply}");
    }
    let log = std::fs::read_to_string(vault.join(".access.log")).unwrap();
    let lines =
        |name: &str| -> Vec<&str> { log.lines().filter(|line| line.contains(name)).collect() };
    let released = lines("egress-localhost-18089");
    assert_eq!(released.len(), 1, "{log}");
    assert!(
        released.iter().all(|line| line.contains("allowed")),
        "{log}"
    );
    let refused = lines("unlisted");
    assert_eq!(refused.len(), 2, "{log}");
    assert!(refused.iter().all(|line| line.contains("denied")), "{log}");
    drop(stream);
    assert!(exits(child).unwrap().success());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Started by anything but the broker, the keeper doesn't run.
#[test]
fn the_keeper_runs_only_as_the_broker_starts_it() {
    let dir = scratch("alone").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("vault-keeper")
        .envs(environment(&dir))
        .env_remove("HATCHLING_KEEPER_FD")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only the broker starts it"), "{stderr}");
}

/// The broker's end of the keeper: a read for a purpose the
/// configuration gives it comes back as the secret, any other is refused,
/// and a cell's vault op is answered under the configuration's grants.
#[test]
fn the_brokers_end_reads_through_the_keeper() {
    use hatchling_broker::keeper::Keeper;
    use hatchling_core::{CellId, VaultOp, VaultResult};
    let dir = scratch("client").unwrap();
    sealed_vault(
        &dir,
        &[("egress-localhost-18089", "credential"), ("token", "t")],
    )
    .unwrap();
    let mut program = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    program
        .envs(environment(&dir))
        .env("HATCHLING_VAULT_GRANTS", "fetch:token")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    let (keeper, keys) = Keeper::start_with(program).unwrap();
    assert!(keys.stores().all(|(store, _)| store != Store::SECRETS));
    let name = |text: &str| -> hatchling_core::SecretName { text.parse().unwrap() };
    let mixed = keeper
        .read("egress", &[name("egress-localhost-18089"), name("token")])
        .map(|read| read.len());
    assert!(
        matches!(mixed, Err(hatchling_broker::Error::Keeper { .. })),
        "egress read a vault grant's secret: {mixed:?}"
    );
    let mut read = keeper
        .read("egress", &[name("egress-localhost-18089")])
        .unwrap()
        .into_iter();
    assert_eq!(read.next().flatten().unwrap().as_bytes(), b"credential");
    assert!(read.next().is_none());
    let cell: CellId = "cell-keeper".parse().unwrap();
    let fetch = |secret: &str| VaultOp::Fetch {
        name: secret.to_owned(),
    };
    assert!(matches!(
        keeper.cell_op(&fetch("token"), &cell, 1000).unwrap(),
        VaultResult::Fetched { .. }
    ));
    assert_eq!(
        keeper
            .cell_op(&fetch("egress-localhost-18089"), &cell, 1000)
            .unwrap(),
        VaultResult::Denied
    );
    drop(keeper);
    // The cell's fetch was recorded: a new keeper's gates scan for it.
    let mut program = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    program
        .envs(environment(&dir))
        .env("HATCHLING_VAULT_GRANTS", "fetch:token")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    let (keeper, _) = Keeper::start_with(program).unwrap();
    let scanned: Vec<Vec<u8>> = keeper
        .scan("output-gate")
        .unwrap()
        .iter()
        .map(|secret| secret.as_bytes().to_vec())
        .collect();
    assert!(scanned.contains(&b"t".to_vec()), "{} values", scanned.len());
    assert!(scanned.contains(&b"credential".to_vec()));
    drop(keeper);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A gate scans for a held secret only once a cell has had it.
#[test]
fn a_gate_scans_only_for_what_a_cell_has_had() {
    use hatchling_broker::keeper::Keeper;
    let dir = scratch("scan").unwrap();
    sealed_vault(&dir, &[("token", "never-given")]).unwrap();
    let mut program = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    program
        .envs(environment(&dir))
        .env("HATCHLING_VAULT_GRANTS", "fetch:token")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    let (keeper, _) = Keeper::start_with(program).unwrap();
    assert!(keeper.scan("output-gate").unwrap().is_empty());
    assert!(keeper.scan("egress").is_err());
    drop(keeper);
    let _ = std::fs::remove_dir_all(&dir);
}
