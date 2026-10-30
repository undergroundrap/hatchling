//! Component: the real broker serving loop against raw socket clients.
//!
//! The broker runs in-process via [`serve_forever`](hatchling_broker::serve_forever)
//! on a background thread; clients speak the raw handshake protocol. These
//! prove the broker's half of the trust boundary: the peer gate fires
//! before parsing, unauthorized peers see a silent close, and every other
//! refusal is fail-closed.
//!
//! Reads carry a timeout so a broker regression fails the test instead of
//! hanging the suite.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use hatchling_broker::{Config, EgressConfig, VaultConfig, bind_listener, serve_forever};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, LogLevel, PROTOCOL_VERSION, PeerUid, SocketPath,
};
use hatchling_vault::MasterKey;

fn test_socket(name: &str) -> String {
    format!("/tmp/hatchling-test-brk-{}-{name}.sock", std::process::id())
}

/// A test socket under `/tmp`, removed when the test ends, pass or fail
/// `sun_path` is short, so it is outside `TMPDIR`.
struct TestSocket(String);

impl std::ops::Deref for TestSocket {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl Drop for TestSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn test_config(name: &str, socket: &str, agent_uid: u32) -> Result<Config, String> {
    let socket: SocketPath = socket
        .parse()
        .map_err(|e| format!("bad test socket: {e}"))?;
    let vault_root = std::env::temp_dir().join(format!(
        "hatchling-test-brk-vault-{}-{name}",
        std::process::id()
    ));
    // The key file defaults to the vault root's sibling, mirroring
    // the broker's own default: never inside the store dir.
    let key_file = VaultConfig::default_key_file(&vault_root);
    Ok(Config {
        socket,
        agent_uid: PeerUid::from_kernel(agent_uid),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: hatchling_broker::SchedulerConfig::disabled(),
        log_level: LogLevel::Error,
        vault: VaultConfig {
            root: vault_root,
            key_file,
            grants: Vec::new(),
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: Vec::new(),
        },
        channels: Vec::new(),
        channel_accounts: Vec::new(),
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: hatchling_broker::MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
        goals: hatchling_broker::GoalsConfig::disabled(),
    })
}

/// Spawns the real serving loop on a background thread. The thread is
/// detached and dies with the test process; the returned guard removes the
/// socket file when the test ends.
fn spawn_broker(name: &str, agent_uid: u32) -> Result<TestSocket, String> {
    let socket = test_socket(name);
    let _ = std::fs::remove_file(&socket);
    let config = test_config(name, &socket, agent_uid)?;
    let listener = bind_listener(&config).map_err(|e| format!("bind failed: {e}"))?;
    // The broker owns the single master key; the serving loop borrows
    // it for every session's sealed vault.
    let key = MasterKey::generate().map_err(|e| format!("key generation failed: {e}"))?;
    std::thread::spawn(move || {
        serve_forever(
            &listener,
            &config,
            &key,
            &hatchling_broker::Skills::default(),
            None,
            None,
        )
    });
    Ok(TestSocket(socket))
}

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

fn connect(socket: &str) -> std::io::Result<UnixStream> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}

fn send_hello(stream: &UnixStream, version: u16, cell_id: &str) -> Result<(), String> {
    let hello = AgentMessage::Hello {
        v: version,
        cell_id: cell_id.to_owned(),
    };
    let payload = serde_json::to_vec(&hello).map_err(|e| format!("encode failed: {e}"))?;
    write_frame(stream, &payload).map_err(|e| format!("write failed: {e}"))
}

fn read_broker_message(stream: &UnixStream) -> Result<BrokerMessage, String> {
    let frame = read_frame(stream).map_err(|e| format!("read failed: {e}"))?;
    serde_json::from_slice(&frame).map_err(|e| format!("decode failed: {e}"))
}

/// Reads until EOF; asserts the peer closed without sending anything.
fn assert_silent_close(stream: &mut UnixStream, context: &str) -> std::io::Result<()> {
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf)?;
    assert_eq!(
        n, 0,
        "{context}: expected a silent close (0 bytes), got {n} bytes"
    );
    Ok(())
}

#[test]
fn authorized_peer_completes_handshake() {
    let socket = spawn_broker("welcome", real_uid()).unwrap();
    let stream = connect(&socket).unwrap();

    send_hello(&stream, PROTOCOL_VERSION, "cell-01").unwrap();
    let reply = read_broker_message(&stream).unwrap();
    assert!(
        matches!(reply, BrokerMessage::Welcome { v } if v == PROTOCOL_VERSION),
        "expected WELCOME, got {reply:?}"
    );
}

#[test]
fn unauthorized_peer_is_silently_closed() {
    // The kernel still reports our real uid; the broker expects another.
    let socket = spawn_broker("silent-close", real_uid().wrapping_add(1)).unwrap();
    let mut stream = connect(&socket).unwrap();

    // No REJECT, no bytes at all: the peer gate fires before parsing.
    assert_silent_close(&mut stream, "unauthorized peer").unwrap();
}

#[test]
fn malformed_hello_gets_reject() {
    let socket = spawn_broker("malformed", real_uid()).unwrap();
    let stream = connect(&socket).unwrap();

    // Authorized peer, complete frame, invalid bytes: REJECT is safe.
    write_frame(&stream, b"this is not json").unwrap();
    let reply = read_broker_message(&stream).unwrap();
    assert!(
        matches!(reply, BrokerMessage::Reject { .. }),
        "expected REJECT, got {reply:?}"
    );
    if let BrokerMessage::Reject { reason, .. } = reply {
        // A fixed sentence: the parser's detail stays in the broker's log.
        assert_eq!(reason, "hello is not a valid handshake message");
    }
}

#[test]
fn wrong_version_gets_reject() {
    let socket = spawn_broker("version", real_uid()).unwrap();
    let stream = connect(&socket).unwrap();

    send_hello(&stream, 999, "cell-01").unwrap();
    let reply = read_broker_message(&stream).unwrap();
    assert!(
        matches!(reply, BrokerMessage::Reject { .. }),
        "expected REJECT, got {reply:?}"
    );
    if let BrokerMessage::Reject { reason, .. } = reply {
        assert!(
            reason.contains("unsupported protocol version"),
            "unexpected reason: {reason}"
        );
    }
}

#[test]
fn oversize_frame_closes_without_answer() {
    let socket = spawn_broker("oversize", real_uid()).unwrap();
    let mut stream = connect(&socket).unwrap();

    // Declare 70_000 bytes, then stop: the broker must refuse on the
    // length alone and close without answering.
    stream.write_all(&70_000u32.to_be_bytes()).unwrap();
    assert_silent_close(&mut stream, "oversize frame").unwrap();
}

#[test]
fn post_handshake_garbage_frame_closes_session() {
    let socket = spawn_broker("post-handshake", real_uid()).unwrap();
    let mut stream = connect(&socket).unwrap();

    send_hello(&stream, PROTOCOL_VERSION, "cell-01").unwrap();
    let reply = read_broker_message(&stream).unwrap();
    assert!(matches!(reply, BrokerMessage::Welcome { .. }));

    // A frame that is not a valid vault op is a session protocol
    // violation: the session ends without answering.
    write_frame(&stream, b"{}").unwrap();
    assert_silent_close(&mut stream, "post-handshake frame").unwrap();
}

/// What a cell sends is read strictly (WO-027): a request that could be
/// read two ways, by a key given twice however it's escaped or by a number
/// that isn't a safe integer, ends the session unanswered, even where the
/// broker's own reading would ignore it. The same request read one way is
/// answered.
#[test]
fn a_request_that_could_be_read_two_ways_ends_the_session() {
    let socket = spawn_broker("strict", real_uid()).unwrap();
    let session = |frame: &str| {
        let mut stream = connect(&socket).unwrap();
        send_hello(&stream, PROTOCOL_VERSION, "cell-01").unwrap();
        let reply = read_broker_message(&stream).unwrap();
        assert!(matches!(reply, BrokerMessage::Welcome { .. }));
        write_frame(&stream, frame.as_bytes()).unwrap();
        let answered = read_frame(&stream).is_ok();
        if !answered {
            assert_silent_close(&mut stream, frame).unwrap();
        }
        answered
    };
    assert!(
        session(r#"{"kind":"vault","op":"fetch","name":"app-token"}"#),
        "read one way: answered"
    );
    for frame in [
        r#"{"kind":"vault","op":"fetch","name":"app-token","note":1.5}"#,
        r#"{"kind":"vault","op":"fetch","name":"app-token","note":9007199254740992}"#,
        // A JSON escape, written with `\x5c`, the backslash.
        "{\"kind\":\"vault\",\"op\":\"fetch\",\"name\":\"app-token\",\"note\":{\"a\":1,\"\x5cu0061\":2}}",
        "{\"kind\":\"vault\",\"\x5cu006bind\":\"egress\",\"op\":\"fetch\",\"name\":\"app-token\"}",
    ] {
        assert!(!session(frame), "answered: {frame}");
    }
}

/// A cell the broker spawned speaks on its own connection: there is no
/// peer gate (the configured agent uid here is one the kernel would never
/// report for this process), and the hello must name the cell's own id.
#[test]
fn a_cells_own_connection_serves_that_cell_alone() -> Result<(), String> {
    let socket = test_socket("cell-connection");
    let config = test_config("cell-connection", &socket, real_uid().wrapping_add(1))?;
    let key = MasterKey::generate().map_err(|e| format!("key generation failed: {e}"))?;
    let skills = hatchling_broker::Skills::default();
    for (named, welcomed) in [("cell-01", true), ("cell-02", false)] {
        let (broker_end, agent_end) =
            UnixStream::pair().map_err(|e| format!("socket pair: {e}"))?;
        agent_end
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| format!("timeout: {e}"))?;
        let cell = hatchling_broker::AgentCell {
            connection: broker_end,
            cell_id: "cell-01".parse().map_err(|e| format!("cell id: {e}"))?,
            pid: 42,
        };
        let served = std::thread::scope(|scope| {
            let served = scope.spawn(|| {
                hatchling_broker::serve_cell_connection(&cell, &config, &key, &skills, None, None)
            });
            send_hello(&agent_end, PROTOCOL_VERSION, named)?;
            let reply = read_broker_message(&agent_end)?;
            // The session ends when the agent goes.
            drop(agent_end);
            let served = served
                .join()
                .map_err(|_| String::from("the session panicked"))?;
            Ok::<_, String>((reply, served))
        })?;
        match (welcomed, served) {
            (true, (BrokerMessage::Welcome { v }, Ok(()))) if v == PROTOCOL_VERSION => {}
            (false, (BrokerMessage::Reject { reason, .. }, Err(error)))
                if reason == "the hello names cell cell-02, but this connection is cell-01's"
                    && matches!(error, hatchling_broker::Error::HandshakeProtocol { .. }) => {}
            (_, other) => return Err(format!("{named}: got {other:?}")),
        }
    }
    Ok(())
}
