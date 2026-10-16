//! Real cell-boundary tests: a genuine cell is spawned (user, mount,
//! and pid namespaces, minimal read-only root, seccomp) and the agent
//! inside it performs the real broker handshake over the bind-mounted
//! socket directory.
//!
//! These tests need privilege to create user namespaces with the
//! nobody mapping (or run as root) and are skipped otherwise: a
//! skipped boundary test is honest, a faked one is worse than none.
//!
//! Each test binds its own broker socket, spawns a cell running the
//! real `hatchling-agent` binary, and drives the broker side of the
//! handshake directly (peer gate, then HELLO/WELCOME) so the tests
//! assert the boundary's actual behavior, not the serving loop's.

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use hatchling_broker::{CellConfig, Error, bind_listener, spawn_cell};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{AgentMessage, BrokerMessage, LogLevel, PROTOCOL_VERSION, PeerUid};

#[allow(
    dead_code,
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use common::{
    agent_binary, broker_config, cell_config, guard_cell_available, mapped_agent_uid,
    probe_spawn_pipeline, probe_stage, probe_staging_dirs, test_dir,
};

/// the shared scratch guard removes its directory, contents
/// and all, so a test that returns early or fails leaks nothing.
#[test]
fn a_test_dir_is_removed_when_its_guard_drops() -> Result<(), String> {
    let dir = test_dir("guard");
    let nested = dir.join("nested");
    std::fs::create_dir_all(&nested).map_err(|e| format!("create: {e}"))?;
    std::fs::write(nested.join("file"), b"x").map_err(|e| format!("write: {e}"))?;
    let path = dir.to_path_buf();
    drop(dir);
    if path.exists() {
        return Err(format!("{} was left behind", path.display()));
    }
    Ok(())
}

#[test]
fn probe_spawn_pipeline_succeeds_where_guard_passes() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let stage = probe_stage();
    let _ = std::fs::remove_dir(&stage);
    // The probe must run in a forked, single-threaded child:
    // unshare(user) in the multi-threaded test process always fails
    // with EINVAL, so a direct call can never succeed where the guard
    // passes. This mirrors the guard's own fork-and-probe structure.
    let ok = match hatchling_sys::fork() {
        Ok(hatchling_sys::ForkResult::Parent { child }) => matches!(
            child.wait_deadline(Duration::from_secs(300)),
            Ok(status) if hatchling_sys::child_exit_code(status) == 0
        ),
        Ok(hatchling_sys::ForkResult::Child) => {
            let ok = probe_spawn_pipeline(&stage);
            hatchling_sys::exit_child(i32::from(!ok));
        }
        Err(e) => return Err(format!("fork probe child: {e}")),
    };
    // The child's mounts lived in its own namespace; here the staging
    // dir is a plain empty dir. Remove it so probes never accumulate
    // debris in the temp dir.
    let _ = std::fs::remove_dir(&stage);
    if !ok {
        return Err(String::from(
            "probe_spawn_pipeline must succeed where userns_available() passes",
        ));
    }
    Ok(())
}

/// The guard's probe must not accumulate debris: after it runs, no
/// *new* staging dir may remain in the temp dir.
///
/// Parallel boundary tests' in-flight probes are tolerated with a
/// grace period — their dirs drain as each guard finishes and cleans
/// up — but a dir that never drains is a leak.
#[test]
fn probe_leaves_no_staging_dir() -> Result<(), String> {
    let before: std::collections::HashSet<PathBuf> = probe_staging_dirs();
    // This call itself runs the guard's probe.
    if !guard_cell_available()? {
        return Ok(());
    }
    for _ in 0..100 {
        let after: std::collections::HashSet<PathBuf> = probe_staging_dirs();
        if after.iter().all(|p| before.contains(p)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let after: std::collections::HashSet<PathBuf> = probe_staging_dirs();
    let leaked: Vec<PathBuf> = after.difference(&before).cloned().collect();
    Err(format!("probe staging dirs leaked: {leaked:?}"))
}

/// Accept one connection with a deadline, without panicking.
fn accept_stream(
    listener: &UnixListener,
    deadline: std::time::Instant,
) -> Result<UnixStream, String> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return Err(String::from(
                        "timed out waiting for the cell agent to connect",
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("accept failed: {e}")),
        }
    }
}

/// Drives the broker side of one connection: peer gate, then the
/// HELLO/WELCOME handshake. Returns the peer uid the kernel reported.
fn broker_handshake(listener: &UnixListener, agent_uid: u32) -> Result<u32, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("set listener nonblocking: {e}"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let stream = accept_stream(listener, deadline)?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("set timeout: {e}"))?;
    let creds =
        hatchling_core::peer_credentials(&stream).map_err(|e| format!("peer creds: {e}"))?;
    let peer_uid = creds.uid().as_u32();
    if peer_uid != agent_uid {
        return Err(format!(
            "peer gate would reject: kernel reports uid {peer_uid}, broker expects {agent_uid}"
        ));
    }
    // The agent speaks first: HELLO.
    let frame = read_frame(&stream).map_err(|e| format!("read HELLO: {e}"))?;
    let hello: AgentMessage =
        serde_json::from_slice(&frame).map_err(|e| format!("parse HELLO: {e}"))?;
    let AgentMessage::Hello { v, .. } = hello;
    if v != PROTOCOL_VERSION {
        return Err(format!("agent spoke protocol v{v}"));
    }
    // Answer WELCOME.
    let welcome = BrokerMessage::Welcome {
        v: PROTOCOL_VERSION,
    };
    let bytes = serde_json::to_vec(&welcome).map_err(|e| format!("encode WELCOME: {e}"))?;
    write_frame(&stream, &bytes).map_err(|e| format!("write WELCOME: {e}"))?;
    Ok(peer_uid)
}

#[test]
fn cell_agent_handshake_reports_mapped_uid() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("handshake");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create test dir: {e}"))?;
    let socket_path = dir.join("broker.sock");
    // The nobody uid is the cell's map base when the broker runs as
    // root; unprivileged the base is the broker's own uid.
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_path, agent_uid)?;
    let listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
    let cell = cell_config(&socket_path, agent_uid)?;

    // Snapshot staging dirs before the spawn. Their names are random,
    // so other tests' cells in this process can show up in the diff
    // below; theirs drain as those cells end, and a leak never does.
    let before = cell_staging_dirs();
    let handle = spawn_cell(&cell, LogLevel::Error).map_err(|e| format!("spawn cell: {e}"))?;
    let peer_uid = broker_handshake(&listener, agent_uid)?;
    if peer_uid != agent_uid {
        return Err(format!(
            "SO_PEERCRED must report the cell's uid-map base: got {peer_uid}, want {agent_uid}"
        ));
    }
    // The agent exits 0 after a successful handshake.
    let status = handle.wait().map_err(|e| format!("wait for cell: {e}"))?;
    let code = hatchling_sys::child_exit_code(status);
    if code != 0 {
        return Err(format!("agent inside the cell should exit 0, got {code}"));
    }
    // The staging dir must be gone after wait. Drain: the reaper may lag
    // the wait by a moment, and other tests' cells end in their time.
    for _ in 0..600 {
        let after = cell_staging_dirs();
        if after.iter().all(|p| before.contains(p)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let after = cell_staging_dirs();
    let leaked: Vec<PathBuf> = after.difference(&before).cloned().collect();
    Err(format!("cell staging dirs leaked: {leaked:?}"))
}

/// the cell has its own network and IPC namespaces, and the
/// broker socket still works across them. Read from the host side while
/// the real agent waits on its handshake: the cell's `/proc/<pid>/ns`
/// links must differ from the broker's own.
#[test]
fn a_cell_has_its_own_network_and_ipc_namespaces() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("namespaces");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create test dir: {e}"))?;
    let socket_path = dir.join("broker.sock");
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_path, agent_uid)?;
    let listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;
    let cell = cell_config(&socket_path, agent_uid)?;
    let handle = spawn_cell(&cell, LogLevel::Error).map_err(|e| format!("spawn cell: {e}"))?;
    let link = |pid: &str, kind: &str| {
        std::fs::read_link(format!("/proc/{pid}/ns/{kind}"))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(|e| format!("/proc/{pid}/ns/{kind}: {e}"))
    };
    let cell_pid = handle.child_pid().to_string();
    for kind in ["net", "ipc"] {
        let ours = link("self", kind)?;
        let theirs = link(&cell_pid, kind)?;
        if ours == theirs {
            return Err(format!(
                "the cell shares the broker's {kind} namespace ({ours})"
            ));
        }
    }
    // The broker socket still reaches the agent across the new netns.
    let peer_uid = broker_handshake(&listener, agent_uid)?;
    if peer_uid != agent_uid {
        return Err(format!("SO_PEERCRED: got {peer_uid}, want {agent_uid}"));
    }
    let status = handle.wait().map_err(|e| format!("wait for cell: {e}"))?;
    let code = hatchling_sys::child_exit_code(status);
    let _ = std::fs::remove_dir_all(&dir);
    if code != 0 {
        return Err(format!("agent inside the cell should exit 0, got {code}"));
    }
    Ok(())
}

/// A connected cell's agent reaches the broker over the connection the
/// spawn made for it, and nothing else: the cell is given no socket, no
/// socket is bound, and the broker serves the session with no peer gate.
#[test]
fn a_connected_cells_agent_handshakes_over_its_own_connection() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let dir = test_dir("connected");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create test dir: {e}"))?;
    // Never bound: the cell must not need it.
    let socket_path = dir.join("broker.sock");
    let agent_uid = mapped_agent_uid();
    let config = broker_config(&socket_path, agent_uid)?;
    // Not marked connected here: the broker's own spawn connects it.
    let cell = cell_config(&socket_path, agent_uid)?;
    let agent = hatchling_broker::spawn_agent_cell(&config, cell)
        .map_err(|e| format!("spawn the agent cell: {e}"))?;
    agent
        .connection
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("set timeout: {e}"))?;
    let key = hatchling_vault::MasterKey::generate().map_err(|e| format!("key: {e}"))?;
    // The agent exits after its handshake; the session then ends at EOF.
    hatchling_broker::serve_cell_connection(
        &agent,
        &config,
        &key,
        &hatchling_broker::Skills::default(),
        None,
        None,
    )
    .map_err(|e| format!("serve the cell's session: {e}"))?;
    if socket_path.exists() {
        return Err(String::from("a socket was bound for a connected cell"));
    }
    Ok(())
}

#[test]
fn mismatched_agent_uid_is_rejected_before_handshake() -> Result<(), String> {
    use std::io::Write as _;
    // No namespaces needed: the peer gate is pure SO_PEERCRED. A plain
    // client socket peers as this process's own uid; the broker is
    // configured to expect a different one. This invokes the real
    // production gate (`serve_connection`), not a test double of its
    // first lines. The client sends garbage bytes to prove the gate
    // fires before a single byte is parsed: the gate must return
    // PeerUnauthorized, never a protocol error.
    let dir = test_dir("mismatch");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create test dir: {e}"))?;
    let socket_path = dir.join("broker.sock");
    let real_uid = hatchling_sys::euid();
    // The broker authorizes a *different* uid than the peer's: the
    // gate must reject before parsing.
    let wrong_uid = real_uid.wrapping_add(1);
    let config = broker_config(&socket_path, wrong_uid)?;
    let listener = bind_listener(&config).map_err(|e| format!("bind broker socket: {e}"))?;

    let mut client = UnixStream::connect(&socket_path).map_err(|e| format!("connect: {e}"))?;
    client
        .write_all(b"this is not a handshake frame")
        .map_err(|e| format!("write garbage: {e}"))?;
    let (stream, _) = listener.accept().map_err(|e| format!("accept: {e}"))?;
    // The key is unused on the rejection path, but the production gate
    // takes one — passing a real key keeps this the real call.
    let key = hatchling_vault::MasterKey::generate().map_err(|e| format!("generate key: {e}"))?;
    match hatchling_broker::serve::serve_connection(
        &stream,
        &config,
        &key,
        &hatchling_broker::Skills::default(),
    ) {
        Err(Error::PeerUnauthorized { actual, expected }) => {
            assert_eq!(
                actual,
                PeerUid::from_kernel(real_uid),
                "the gate must report the kernel peer uid"
            );
            assert_eq!(
                expected,
                PeerUid::from_kernel(wrong_uid),
                "the gate must report the configured uid"
            );
        }
        Err(other) => {
            return Err(format!("expected PeerUnauthorized, got: {other}"));
        }
        Ok(()) => {
            return Err(String::from(
                "peer gate should have rejected the mismatched uid",
            ));
        }
    }
    Ok(())
}

#[test]
fn cell_config_rejects_uid_base_mismatch() -> Result<(), String> {
    // Pure config-level test: no namespaces needed. The broker's
    // HATCHLING_AGENT_UID and the cell's uid-map base must agree or
    // the agent could never pass the peer gate.
    //
    // Only meaningful as root: unprivileged, the idmap validator
    // rejects any base other than the euid before the mismatch check
    // runs, so the mismatch scenario is unreachable.
    if hatchling_sys::euid() != 0 {
        return Ok(());
    }
    let dir = test_dir("config");
    let agent_bin = agent_binary()?;
    let err = CellConfig::parse_test_err(&agent_bin, "x", &dir, 1000, 1001)
        .map_err(|e| format!("parse_test_err unexpectedly succeeded: {e}"))?;
    if !matches!(err, Error::InvalidConfig { .. }) {
        return Err(format!(
            "uid base / agent uid mismatch must fail closed, got {err:?}"
        ));
    }
    Ok(())
}

/// Staging dirs from real `spawn_cell` calls in this process (in-flight
/// or leaked): every `cell-*` directory in this process's staging roots,
/// `hatchling-staging-<pid>-<random>` in the temp dir.
fn cell_staging_dirs() -> std::collections::HashSet<PathBuf> {
    let root_prefix = format!("hatchling-staging-{}-", std::process::id());
    let named = |path: &PathBuf, prefix: &str| {
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(prefix))
    };
    let entries = |dir: &std::path::Path| -> Vec<PathBuf> {
        dir.read_dir()
            .map(|entries| entries.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default()
    };
    entries(&std::env::temp_dir())
        .into_iter()
        .filter(|root| named(root, &root_prefix))
        .flat_map(|root| entries(&root))
        .filter(|dir| named(dir, "cell-"))
        .collect()
}
