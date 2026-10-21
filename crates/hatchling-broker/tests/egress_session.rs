//! Component: the broker's egress session against raw socket clients.
//!
//! The real [`serve_connection`](hatchling_broker::serve_connection)
//! pipeline runs on a background thread over a socketpair; the test
//! client speaks the raw egress protocol. These prove the mediation the
//! mediation under test: the peer gate fires before parsing, every op is
//! grant-checked before the proxy is touched, denials never reach the
//! network, and malformed traffic ends the session.
//!
//! Reads carry a timeout so a broker regression fails the test instead
//! of hanging the suite.

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use hatchling_broker::{Config, EgressConfig, VaultConfig, serve_connection};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, EgressOp, EgressResult, LogLevel, PeerUid, SessionRequest,
    SessionResponse, SocketPath,
};
use hatchling_vault::MasterKey;

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Spawns the real broker pipeline on a socketpair, returning the client
/// end and the vault root. The broker runs on a background thread; the
/// test drives the client end.
fn spawn_session(
    name: &str,
    agent_uid: u32,
    egress_allowlist: &str,
    egress_grants: Vec<String>,
) -> io::Result<(UnixStream, PathBuf)> {
    let vault_root = std::env::temp_dir().join(format!(
        "hatchling-test-egress-session-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&vault_root);
    let key_file = VaultConfig::default_key_file(&vault_root);
    let config = Config {
        socket: "/tmp/hatchling-test-egress-session-dummy.sock"
            .parse::<SocketPath>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
        agent_uid: PeerUid::from_kernel(agent_uid),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: hatchling_broker::SchedulerConfig::disabled(),
        log_level: LogLevel::Error,
        vault: VaultConfig {
            root: vault_root.clone(),
            key_file,
            grants: Vec::new(),
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::new(egress_allowlist),
            grants: egress_grants,
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
    };
    // The broker's single master key, borrowed by the session thread
    // for every sealed-vault open (the egress credential fetcher reads
    // through the sealed vault too).
    let key = MasterKey::generate().map_err(io::Error::other)?;
    let (client, server) = UnixStream::pair()?;
    client.set_read_timeout(Some(Duration::from_secs(5)))?;
    std::thread::spawn(move || {
        let _ = serve_connection(&server, &config, &key, &hatchling_broker::Skills::default());
    });
    Ok((client, vault_root))
}

/// Performs the client side of the handshake; asserts WELCOME.
fn handshake(client: &UnixStream) -> io::Result<()> {
    let hello = AgentMessage::Hello {
        v: hatchling_core::PROTOCOL_VERSION,
        cell_id: String::from("cell-01"),
    };
    let payload = serde_json::to_vec(&hello)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_frame(client, &payload).map_err(|e| frame_io(&e))?;
    let reply = read_frame(client).map_err(|e| frame_io(&e))?;
    let message: BrokerMessage = serde_json::from_slice(&reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    assert!(
        matches!(message, BrokerMessage::Welcome { .. }),
        "expected WELCOME, got {message:?}"
    );
    Ok(())
}

fn frame_io(error: &hatchling_core::Error) -> io::Error {
    io::Error::other(error.to_string())
}

fn send_egress_op(stream: &UnixStream, op: &EgressOp) -> io::Result<()> {
    let request = SessionRequest::Egress(op.clone());
    let payload = serde_json::to_vec(&request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_frame(stream, &payload).map_err(|e| frame_io(&e))
}

fn read_egress_result(stream: &UnixStream) -> io::Result<EgressResult> {
    let frame = read_frame(stream).map_err(|e| frame_io(&e))?;
    let response: SessionResponse = serde_json::from_slice(&frame)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match response {
        SessionResponse::Egress(result) => Ok(result),
        SessionResponse::Vault(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a vault reply to an egress op",
        )),
        SessionResponse::Channel(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a channel reply to an egress op",
        )),
        SessionResponse::Memory(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a memory reply to an egress op",
        )),
        SessionResponse::Skill(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a skill reply to an egress op",
        )),
        SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_)
        | SessionResponse::Browser(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a goals or query reply to an egress op",
        )),
    }
}

fn cleanup(root: &PathBuf) {
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn mint_surrogate_granted_destination_succeeds() {
    // localhost:443 is on the allowlist and granted: minting must
    // succeed and return a surrogate.
    let (client, root) = spawn_session(
        "mint-granted",
        real_uid(),
        "internal:localhost:443",
        vec![String::from("localhost:443")],
    )
    .unwrap();
    handshake(&client).unwrap();

    send_egress_op(
        &client,
        &EgressOp::MintSurrogate {
            destination: String::from("localhost:443"),
            ttl_secs: 3600,
        },
    )
    .unwrap();
    let result = read_egress_result(&client).unwrap();
    assert!(
        matches!(result, EgressResult::Surrogate { .. }),
        "expected Surrogate, got {result:?}"
    );

    drop(client);
    cleanup(&root);
}

#[test]
fn mint_surrogate_without_grant_is_denied() {
    // The destination is on the allowlist but the peer holds no grant:
    // the mint must be denied before the proxy is touched.
    let (client, root) = spawn_session(
        "mint-ungranted",
        real_uid(),
        "internal:localhost:443",
        Vec::new(), // No grants.
    )
    .unwrap();
    handshake(&client).unwrap();

    send_egress_op(
        &client,
        &EgressOp::MintSurrogate {
            destination: String::from("localhost:443"),
            ttl_secs: 3600,
        },
    )
    .unwrap();
    assert_eq!(read_egress_result(&client).unwrap(), EgressResult::Denied);

    drop(client);
    cleanup(&root);
}

#[test]
fn mint_surrogate_non_allowlisted_destination_is_denied() {
    // The peer holds a grant for example.com:443, but it's not on the
    // allowlist: the proxy must refuse to mint.
    let (client, root) = spawn_session(
        "mint-not-allowlisted",
        real_uid(),
        "internal:localhost:443", // Only localhost is allowlisted (internal: it is this machine).
        vec![String::from("example.com:443")], // Grant for non-allowlisted.
    )
    .unwrap();
    handshake(&client).unwrap();

    send_egress_op(
        &client,
        &EgressOp::MintSurrogate {
            destination: String::from("example.com:443"),
            ttl_secs: 3600,
        },
    )
    .unwrap();
    // The grant check passes, but the proxy's allowlist check fails.
    assert_eq!(read_egress_result(&client).unwrap(), EgressResult::Denied);

    drop(client);
    cleanup(&root);
}

#[test]
fn egress_with_invalid_surrogate_is_denied() {
    let (client, root) = spawn_session(
        "egress-bad-surrogate",
        real_uid(),
        "internal:localhost:443",
        vec![String::from("localhost:443")],
    )
    .unwrap();
    handshake(&client).unwrap();

    // Present a forged surrogate: the HMAC will not verify.
    send_egress_op(
        &client,
        &EgressOp::Egress {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: Vec::new(),
            body_b64: String::new(),
            surrogate_b64: String::from("Zm9yZ2VkLXN1cnJvZ2F0ZQ=="), // "forged-surrogate"
            staged: 0,
        },
    )
    .unwrap();
    assert_eq!(read_egress_result(&client).unwrap(), EgressResult::Denied);

    drop(client);
    cleanup(&root);
}

#[test]
fn egress_without_grant_is_denied() {
    let (client, root) = spawn_session(
        "egress-ungranted",
        real_uid(),
        "internal:localhost:443",
        Vec::new(), // No grants.
    )
    .unwrap();
    handshake(&client).unwrap();

    send_egress_op(
        &client,
        &EgressOp::Egress {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: Vec::new(),
            body_b64: String::new(),
            surrogate_b64: String::from("c3Vycm9nYXRl"),
            staged: 0,
        },
    )
    .unwrap();
    // Denied at the grant check, before surrogate validation.
    assert_eq!(read_egress_result(&client).unwrap(), EgressResult::Denied);

    drop(client);
    cleanup(&root);
}

#[test]
fn wrong_uid_gets_silent_close_before_egress() {
    // The peer gate fires before any egress byte is parsed: a peer with
    // the wrong uid gets a silent close, not a REJECT.
    let (mut client, root) = spawn_session(
        "egress-wrong-uid",
        real_uid() + 1, // Broker expects a different uid.
        "internal:localhost:443",
        vec![String::from("localhost:443")],
    )
    .unwrap();
    // The handshake itself must fail: the peer gate fires first.
    let hello = AgentMessage::Hello {
        v: hatchling_core::PROTOCOL_VERSION,
        cell_id: String::from("cell-01"),
    };
    let payload = serde_json::to_vec(&hello).unwrap();
    // The peer gate fires on accept, so the broker may close before our
    // HELLO lands. A BrokenPipe here is the expected outcome, not a test
    // failure — the session is dead either way. The assertion below
    // confirms the close is silent (EOF, not a REJECT frame).
    let _ = write_frame(&client, &payload);
    // The broker closes silently: we get EOF, not a REJECT frame.
    let mut buf = [0u8; 1];
    let n = client.read(&mut buf).unwrap_or(0);
    assert_eq!(n, 0, "expected silent close (EOF), not a REJECT");

    drop(client);
    cleanup(&root);
}
