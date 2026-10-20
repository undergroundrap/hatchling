//! Component: the real accept loop, capped.
//!
//! [`serve_forever`] runs on a background thread with a cap of three;
//! raw clients hold sessions open and probe the fourth.
//!
//! No step waits on a timer. The broker frees a connection's slot
//! before it closes that connection, so a client that has read EOF after
//! ending its session knows the slot is already free. The read timeouts
//! only bound a failure.
#![allow(
    clippy::unwrap_used,
    reason = "fixture setup: a failed bind or connect must fail the test loudly"
)]

use std::io::Read as _;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use hatchling_broker::{Config, EgressConfig, VaultConfig, bind_listener, serve_forever};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, ChannelOp, ChannelResult, LogLevel, PROTOCOL_VERSION, PeerUid,
    SessionRequest, SessionResponse,
};
use hatchling_vault::MasterKey;

const CAP: usize = 3;

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

fn broker(name: &str) -> TestSocket {
    let socket = format!("/tmp/hlcap-{}-{name}.sock", std::process::id());
    let vault_root = std::env::temp_dir().join(format!(
        "hatchling-test-cap-vault-{}-{name}",
        std::process::id()
    ));
    let config = Config {
        socket: socket.parse().unwrap(),
        agent_uid: PeerUid::from_kernel(rustix::process::getuid().as_raw()),
        socket_mode: 0o600,
        max_connections: CAP,
        scheduler: hatchling_broker::SchedulerConfig::disabled(),
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
        memory: hatchling_broker::MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
        goals: hatchling_broker::GoalsConfig::disabled(),
    };
    let listener = bind_listener(&config).unwrap();
    let key = MasterKey::generate().unwrap();
    // The loop diverges; the thread lives until the test process exits.
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
    TestSocket(socket)
}

fn connect(socket: &str) -> UnixStream {
    let stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
}

fn hello(stream: &UnixStream) {
    let hello = AgentMessage::Hello {
        v: PROTOCOL_VERSION,
        cell_id: String::from("cell-cap"),
    };
    // A refused peer may already be closed: its write can fail, and
    // only what it reads back matters.
    let _ = write_frame(stream, &serde_json::to_vec(&hello).unwrap());
}

/// Connects and completes the handshake: an admitted, live session.
fn session(socket: &str) -> UnixStream {
    let stream = connect(socket);
    hello(&stream);
    let welcome: BrokerMessage = serde_json::from_slice(&read_frame(&stream).unwrap()).unwrap();
    assert!(
        matches!(welcome, BrokerMessage::Welcome { .. }),
        "{welcome:?}"
    );
    stream
}

/// Whether the broker closed `stream` without sending a byte.
fn closed_silently(stream: &UnixStream) -> bool {
    let mut stream = stream;
    let mut byte = [0u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => true,
        Err(error) => error.kind() == std::io::ErrorKind::ConnectionReset,
        Ok(_) => false,
    }
}

/// Ends a session and waits for the broker's close, which comes only
/// after its slot is free.
fn end(stream: UnixStream) {
    stream.shutdown(Shutdown::Write).unwrap();
    assert!(closed_silently(&stream), "the broker closed the session");
    drop(stream);
}

/// The session still answers ops.
fn answers(stream: &UnixStream) -> bool {
    let op = SessionRequest::Channel(ChannelOp::Receive {
        channel: String::from("local"),
        account: None,
    });
    write_frame(stream, &serde_json::to_vec(&op).unwrap()).unwrap();
    let reply: SessionResponse = serde_json::from_slice(&read_frame(stream).unwrap()).unwrap();
    reply == SessionResponse::Channel(ChannelResult::NoMessage)
}

#[test]
fn the_cap_admits_three_closes_the_fourth_and_frees_every_slot() {
    let socket = broker("cap");
    let held: Vec<UnixStream> = (0..CAP).map(|_| session(&socket)).collect();

    // The fourth, over the cap: a clean close, never a protocol frame.
    let over = connect(&socket);
    hello(&over);
    assert!(closed_silently(&over), "an over-cap peer reads nothing");

    // The admitted sessions are unaffected, and the broker still
    // accepts: another over-cap attempt is closed the same way.
    assert!(held.iter().all(answers));
    let again = connect(&socket);
    hello(&again);
    assert!(closed_silently(&again));

    // Ending one session frees exactly one slot.
    let mut held = held;
    end(held.pop().unwrap());
    let replacement = session(&socket);
    assert!(answers(&replacement));
    let over = connect(&socket);
    hello(&over);
    assert!(closed_silently(&over), "still capped at three");

    // No leak: once every session has ended, three fresh ones fit.
    end(replacement);
    for stream in held {
        end(stream);
    }
    let fresh: Vec<UnixStream> = (0..CAP).map(|_| session(&socket)).collect();
    assert!(fresh.iter().all(answers));
    for stream in fresh {
        end(stream);
    }
}
