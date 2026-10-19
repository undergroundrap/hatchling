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

/// The keeper hands the broker every store's key but the secrets', reads
/// a configured credential and refuses any other, records both, keeps
/// its memory closed, and leaves when the broker does.
#[test]
fn the_keeper_keeps_the_secrets_and_releases_only_what_is_configured() {
    let dir = scratch("serve").unwrap();
    let vault = dir.join("vault");
    std::fs::create_dir(&vault).unwrap();
    std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o700)).unwrap();
    for (name, value) in [
        ("egress-localhost-18089", "credential"),
        ("unlisted", "kept"),
    ] {
        std::fs::write(vault.join(name), value).unwrap();
    }
    // The operator seals them, and the vault's key is made.
    let migrated = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("migrate-vault")
        .envs(environment(&dir))
        .env_remove("HATCHLING_AGENT_BIN")
        .output()
        .unwrap();
    assert!(
        migrated.status.success(),
        "{}",
        String::from_utf8_lossy(&migrated.stderr)
    );
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
