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

/// The keeper, started as the broker starts it, the broker's end, and the
/// egress authority's.
fn start(scratch: &Path) -> io::Result<(Child, UnixStream, UnixStream)> {
    let (ours, theirs) = UnixStream::pair()?;
    let (authoritys, keepers) = UnixStream::pair()?;
    for end in [theirs.as_fd(), keepers.as_fd()] {
        hatchling_sys::keep_across_exec(end).map_err(io::Error::other)?;
    }
    let child = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("vault-keeper")
        .envs(environment(scratch))
        .env("HATCHLING_KEEPER_FD", theirs.as_raw_fd().to_string())
        .env(
            "HATCHLING_KEEPER_AUTHORITY_FD",
            keepers.as_raw_fd().to_string(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    drop(theirs);
    drop(keepers);
    for end in [&ours, &authoritys] {
        end.set_read_timeout(Some(Duration::from_secs(20)))?;
    }
    Ok((child, ours, authoritys))
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
/// a configured credential to the egress authority and refuses it to the
/// broker, refuses any other, records each, keeps its memory closed, and
/// leaves when the broker does.
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
    let (child, stream, authoritys) = start(&dir).unwrap();
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
    // A configured credential, read for its purpose by the authority.
    let credential = r#"{"request":"read","purpose":"egress","names":["egress-localhost-18089"]}"#;
    let reply = ask(&authoritys, credential).unwrap();
    assert_eq!(reply, r#"{"reply":"read","found":[true]}"#);
    assert_eq!(read_frame(&authoritys).unwrap(), b"credential");
    // The broker asking for it is refused, and so is anything else.
    let reply = ask(&stream, credential).unwrap();
    assert!(reply.starts_with(r#"{"reply":"refused""#), "{reply}");
    for refused in [
        r#"{"request":"read","purpose":"egress","names":["unlisted"]}"#,
        r#"{"request":"read","purpose":"anything","names":["unlisted"]}"#,
    ] {
        let reply = ask(&authoritys, refused).unwrap();
        assert!(reply.starts_with(r#"{"reply":"refused""#), "{reply}");
        assert!(!reply.contains("kept"), "{reply}");
    }
    let log = std::fs::read_to_string(vault.join(".access.log")).unwrap();
    let lines =
        |name: &str| -> Vec<&str> { log.lines().filter(|line| line.contains(name)).collect() };
    let released = lines("egress-localhost-18089");
    assert_eq!(released.len(), 2, "{log}");
    assert!(
        released
            .first()
            .is_some_and(|line| line.contains("authority") && line.contains("allowed")),
        "{log}"
    );
    assert!(
        released
            .get(1)
            .is_some_and(|line| line.contains("broker") && line.contains("denied")),
        "{log}"
    );
    let refused = lines("unlisted");
    assert_eq!(refused.len(), 2, "{log}");
    assert!(refused.iter().all(|line| line.contains("denied")), "{log}");
    drop(stream);
    assert!(exits(child).unwrap().success());
    drop(authoritys);
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
    let (keeper, keys, authoritys) = Keeper::start_with(program).unwrap();
    assert!(keys.stores().all(|(store, _)| store != Store::SECRETS));
    let name = |text: &str| -> hatchling_core::SecretName { text.parse().unwrap() };
    let authority = Keeper::over(authoritys).unwrap();
    let mixed = authority
        .read("egress", &[name("egress-localhost-18089"), name("token")])
        .map(|read| read.len());
    assert!(
        matches!(mixed, Err(hatchling_broker::Error::Keeper { .. })),
        "egress read a vault grant's secret: {mixed:?}"
    );
    assert!(
        keeper
            .read("egress", &[name("egress-localhost-18089")])
            .is_err(),
        "the broker read an egress credential"
    );
    let mut read = authority
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
    let (keeper, _, _) = Keeper::start_with(program).unwrap();
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
    let (keeper, _, _) = Keeper::start_with(program).unwrap();
    assert!(keeper.scan("output-gate").unwrap().is_empty());
    assert!(keeper.scan("egress").is_err());
    drop(keeper);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The broker's start, as it starts the keeper (and the egress authority
/// with its `search`), with `env` beside the usual.
fn program(dir: &Path, env: &[(&str, &str)]) -> Command {
    let mut program = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    program
        .envs(environment(dir))
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    program
}

/// The egress authority's channel (WO-017, step 2): the keeper releases
/// search's key and the egress credentials on it, never on the broker's,
/// and nothing of the broker's on it.
#[test]
fn the_authoritys_reads_are_released_on_its_channel_alone() {
    use hatchling_broker::keeper::Keeper;
    let dir = scratch("authority-channel").unwrap();
    sealed_vault(
        &dir,
        &[
            ("search-brave", "k-brave"),
            ("egress-localhost-18089", "credential"),
        ],
    )
    .unwrap();
    let (keeper, _, authoritys) =
        Keeper::start_with(program(&dir, &[("HATCHLING_SEARCH", "brave")])).unwrap();
    let name = |text: &str| -> hatchling_core::SecretName { text.parse().unwrap() };
    let refused = format!("{:?}", keeper.read("search", &[name("search-brave")]));
    assert!(
        refused.contains("no such read"),
        "the broker read search's key"
    );
    let authority = Keeper::over(authoritys).unwrap();
    let read = authority.read("search", &[name("search-brave")]).unwrap();
    let key = read.first().and_then(Option::as_ref).unwrap();
    assert_eq!(key.as_bytes(), b"k-brave");
    let read = authority
        .read("egress", &[name("egress-localhost-18089")])
        .unwrap();
    let credential = read.first().and_then(Option::as_ref).unwrap();
    assert_eq!(credential.as_bytes(), b"credential");
    assert!(
        keeper
            .read("egress", &[name("egress-localhost-18089")])
            .is_err(),
        "the broker read an egress credential"
    );
    assert!(authority.scan("output-gate").is_err());
    // The broker's channel goes on after the authority's.
    drop(authority);
    assert!(keeper.scan("output-gate").is_ok());
    drop(keeper);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The egress authority, started as the broker starts it: it searches
/// only while search is on, and with the key it reads from the keeper.
#[test]
fn the_egress_authority_searches_with_what_the_keeper_gives_it() {
    use hatchling_broker::egress_authority::Authority;
    use hatchling_broker::keeper::Keeper;
    let dir = scratch("authority").unwrap();
    sealed_vault(&dir, &[]).unwrap();
    let searching = [("HATCHLING_SEARCH", "brave")];
    let (keeper, _, authoritys) = Keeper::start_with(program(&dir, &searching)).unwrap();
    let authority = Authority::start_with(program(&dir, &searching), &authoritys).unwrap();
    drop(authoritys);
    assert_eq!(
        authority.search("rust"),
        Err(String::from(
            "no search key is stored: keep it in the vault as search-brave"
        ))
    );
    drop(authority);
    drop(keeper);
    let (keeper, _, authoritys) = Keeper::start_with(program(&dir, &[])).unwrap();
    let authority = Authority::start_with(program(&dir, &[]), &authoritys).unwrap();
    drop(authoritys);
    assert_eq!(authority.search("rust"), Err(String::from("search is off")));
    drop(authority);
    drop(keeper);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The egress authority runs only as the broker starts it.
#[test]
fn the_egress_authority_refuses_to_run_unless_the_broker_started_it() {
    let output = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("egress-authority")
        .env_remove("HATCHLING_AUTHORITY_FD")
        .env_remove("HATCHLING_AUTHORITY_KEEPER_FD")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("only the broker starts it"), "{stderr}");
}

/// What a one-request upstream was sent.
type Upstream = std::thread::JoinHandle<io::Result<Vec<u8>>>;

/// A one-request upstream on loopback: it answers `reply`, and returns
/// what it was sent.
fn upstream(reply: &'static [u8]) -> io::Result<(u16, Upstream)> {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut seen = Vec::new();
        let mut byte = [0_u8; 1];
        while !seen.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte)?;
            seen.extend_from_slice(&byte);
        }
        let length: usize = String::from_utf8_lossy(&seen)
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|length| length.trim().parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        stream.read_exact(&mut body)?;
        seen.extend_from_slice(&body);
        stream.write_all(reply)?;
        Ok(seen)
    });
    Ok((port, handle))
}

/// A session's egress through the real keeper and authority, started as
/// the broker starts them (WO-017, step 2): the authority mints the
/// surrogate, reads the destination's credential from the keeper, puts it
/// in, and scrubs it from the answer; the broker's own read of it is
/// refused, and the access log names the authority.
#[test]
fn a_sessions_egress_goes_through_the_authority_with_its_credential() {
    use hatchling_broker::egress_authority::Authority;
    use hatchling_broker::egress_path::EgressPath as _;
    use hatchling_broker::keeper::Keeper;
    let (port, served) =
        upstream(b"HTTP/1.1 200 OK\r\nContent-Length: 22\r\n\r\necho: Bearer cred-qz-9").unwrap();
    let destination = format!("localhost:{port}");
    let secret = format!("egress-localhost-{port}");
    let dir = scratch("authority-egress").unwrap();
    sealed_vault(&dir, &[(secret.as_str(), "cred-qz-9")]).unwrap();
    let allowlist = format!("local:{destination}");
    let env = [
        ("HATCHLING_EGRESS_ALLOWLIST", allowlist.as_str()),
        ("HATCHLING_EGRESS_GRANTS", destination.as_str()),
    ];
    let (keeper, _, authoritys) = Keeper::start_with(program(&dir, &env)).unwrap();
    let authority = Authority::start_with(program(&dir, &env), &authoritys).unwrap();
    drop(authoritys);
    let name: hatchling_core::SecretName = secret.parse().unwrap();
    assert!(
        keeper.read("egress", &[name]).is_err(),
        "the broker read the credential"
    );
    let link = authority.connect().unwrap().egress().unwrap();
    let surrogate_b64 = link.mint(1000, &destination, 60).unwrap();
    let request = hatchling_egress::EgressRequest {
        method: String::from("GET"),
        destination: destination.clone(),
        path: String::from("/v1/models"),
        headers: Vec::new(),
        body: Vec::new(),
        surrogate_b64,
    };
    let answer = link.forward(1000, &request).unwrap();
    let seen = String::from_utf8(served.join().unwrap().unwrap()).unwrap();
    assert!(
        seen.contains("\r\nAuthorization: Bearer cred-qz-9\r\n"),
        "the credential wasn't put in"
    );
    assert_eq!(answer.status, 200);
    assert_eq!(answer.body.len(), 22);
    assert!(
        !String::from_utf8_lossy(&answer.body).contains("cred-qz-9"),
        "the credential came back"
    );
    drop((link, authority, keeper));
    let log = std::fs::read_to_string(dir.join("vault").join(".access.log")).unwrap();
    let reads: Vec<&str> = log.lines().filter(|line| line.contains(&secret)).collect();
    assert_eq!(reads.len(), 2, "{log}");
    assert!(
        reads
            .first()
            .is_some_and(|line| line.contains("broker") && line.contains("denied")),
        "{log}"
    );
    assert!(
        reads
            .get(1)
            .is_some_and(|line| line.contains("authority") && line.contains("allowed")),
        "{log}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The settings of a Claude Code harness under `dir`, relaying to a
/// vendor on `port`: a stand-in binary, pinned; a project root; and the
/// artifact workspace its patches go to.
fn harness_settings(dir: &Path, port: u16) -> io::Result<Vec<(&'static str, String)>> {
    for sub in ["bin", "project", "workspace"] {
        std::fs::create_dir_all(dir.join(sub))?;
    }
    let binary = dir.join("bin").join("claude");
    std::fs::write(&binary, b"#!/bin/sh\nexit 0\n")?;
    let pin = hatchling_skill::CodeHash::of(&std::fs::read(&binary)?).to_string();
    let path = |sub: &str| dir.join(sub).display().to_string();
    Ok(vec![
        (
            "HATCHLING_HARNESS_CLAUDE_CODE",
            binary.display().to_string(),
        ),
        ("HATCHLING_HARNESS_CLAUDE_CODE_SHA256", pin),
        (
            "HATCHLING_HARNESS_CLAUDE_CODE_API",
            format!("local:localhost:{port}"),
        ),
        (
            "HATCHLING_HARNESS_ROOTS",
            format!("project={}", path("project")),
        ),
        ("HATCHLING_ARTIFACT_WORKSPACE", path("workspace")),
    ])
}

/// A harness relay through the real keeper and authority (WO-017, step
/// 2): the authority finds the harness's vendor in its own configuration
/// and reads the vendor key, the cell's surrogate is swapped for it on
/// the way to the vendor, and the keeper refuses the broker's own read of
/// it. A harness with no vendor configured is refused.
#[test]
fn a_harness_relay_runs_in_the_authority_with_the_key_it_reads() {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixListener;

    use hatchling_broker::egress_authority::Authority;
    use hatchling_broker::harness::HarnessKind;
    use hatchling_broker::keeper::Keeper;
    let (port, vendor) = upstream(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").unwrap();
    let dir = scratch("authority-relay").unwrap();
    sealed_vault(&dir, &[("harness-claude-code", "sk-real-qz-3")]).unwrap();
    let settings = harness_settings(&dir, port).unwrap();
    let env: Vec<(&str, &str)> = settings
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    let (keeper, _, authoritys) = Keeper::start_with(program(&dir, &env)).unwrap();
    let authority = Authority::start_with(program(&dir, &env), &authoritys).unwrap();
    drop(authoritys);
    let name: hatchling_core::SecretName = "harness-claude-code".parse().unwrap();
    assert!(
        keeper.read("harness", &[name]).is_err(),
        "the broker read a vendor key"
    );
    let refused = authority
        .connect()
        .unwrap()
        .relay(HarnessKind::CodexCli, "sk-surrogate")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        refused,
        "the codex-cli harness has no vendor API configured"
    );
    let pending = authority
        .connect()
        .unwrap()
        .relay(HarnessKind::ClaudeCode, "sk-surrogate-qz")
        .unwrap()
        .unwrap();
    let handoff_path = dir.join("relay.sock");
    let handoff = UnixListener::bind(&handoff_path).unwrap();
    let relay = pending.start(&handoff, Duration::from_secs(30)).unwrap();
    // The cell hands its listener over; the CLI asks through it.
    let cell = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let cell_port = cell.local_addr().unwrap().port();
    let to_handoff = UnixStream::connect(&handoff_path).unwrap();
    hatchling_sys::net::send_fd(to_handoff.as_fd(), cell.as_fd()).unwrap();
    let mut cli = std::net::TcpStream::connect(("127.0.0.1", cell_port)).unwrap();
    cli.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    cli.write_all(
        b"POST /v1/messages HTTP/1.1\r\nx-api-key: sk-surrogate-qz\r\nContent-Length: 2\r\n\r\n{}",
    )
    .unwrap();
    let mut answer = String::new();
    cli.read_to_string(&mut answer).unwrap();
    assert!(answer.ends_with("ok"), "{answer}");
    let seen = String::from_utf8(vendor.join().unwrap().unwrap()).unwrap();
    assert!(seen.contains("x-api-key: sk-real-qz-3\r\n"), "{seen}");
    assert!(!seen.contains("sk-surrogate-qz"), "{seen}");
    let (count, problem) = relay.finish();
    assert_eq!((count.forwarded, count.refused, problem), (1, 0, None));
    drop((to_handoff, cell, authority, keeper));
    let log = std::fs::read_to_string(dir.join("vault").join(".access.log")).unwrap();
    let reads: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("harness-claude-code"))
        .collect();
    assert_eq!(reads.len(), 2, "{log}");
    assert!(
        reads
            .first()
            .is_some_and(|line| line.contains("broker") && line.contains("denied")),
        "{log}"
    );
    assert!(
        reads
            .get(1)
            .is_some_and(|line| line.contains("authority") && line.contains("allowed")),
        "{log}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
