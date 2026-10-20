//! Component: the broker's vault session against raw socket clients.
//!
//! The real [`serve_connection`](hatchling_broker::serve_connection)
//! pipeline runs on a background thread over a socketpair; the test
//! client speaks the raw vault protocol. These prove the mediation the
//! mediation under test: the peer gate fires before parsing, every op is
//! grant-checked before the vault is touched, denials never reach the
//! filesystem, and malformed traffic ends the session.
//!
//! Reads carry a timeout so a broker regression fails the test instead
//! of hanging the suite.

use std::io::{self, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use hatchling_broker::{Config, EgressConfig, VaultConfig, VaultGrant, serve_connection};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, LogLevel, PeerUid, SessionRequest, SessionResponse, SocketPath,
    VaultAction, VaultOp, VaultResult, encode_secret,
};
use hatchling_vault::MasterKey;

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Fixture error mapping: frame failures are I/O failures for the
/// test's purposes; the `?` operator just needs a single error type.
fn frame_io(error: hatchling_core::Error) -> io::Error {
    io::Error::other(error)
}

fn grant(action: VaultAction, name: &str) -> Result<VaultGrant, hatchling_core::Error> {
    Ok(VaultGrant {
        action,
        name: name.parse()?,
    })
}

/// Spawns the real pipeline on a background thread over a socketpair
/// and returns the client end plus the vault root the broker was given.
///
/// The broker thread runs the real [`serve_connection`](hatchling_broker::serve_connection),
/// including the kernel peer gate (same-process peer, so the real uid
/// must be the configured one — see `wrong_uid_peer_never_reaches_vault`
/// for the mismatch case). The caller performs the handshake, or not.
fn spawn_session(
    name: &str,
    agent_uid: u32,
    grants: Vec<VaultGrant>,
) -> io::Result<(UnixStream, PathBuf)> {
    let vault_root = std::env::temp_dir().join(format!(
        "hatchling-test-vault-session-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&vault_root);
    let key_file = VaultConfig::default_key_file(&vault_root);
    let config = Config {
        socket: "/tmp/hatchling-test-vault-session-dummy.sock"
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
            grants,
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
    };
    // The broker's single master key, borrowed by the session thread
    // for every sealed-vault open.
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
    write_frame(client, &payload).map_err(frame_io)?;
    let reply = read_frame(client).map_err(frame_io)?;
    let message: BrokerMessage = serde_json::from_slice(&reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    assert!(
        matches!(message, BrokerMessage::Welcome { .. }),
        "expected WELCOME, got {message:?}"
    );
    Ok(())
}

fn send_op(stream: &UnixStream, op: &VaultOp) -> io::Result<()> {
    // The v3 session envelope: vault ops are wrapped in
    // `SessionRequest::Vault`.
    let request = SessionRequest::Vault(op.clone());
    let payload = serde_json::to_vec(&request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_frame(stream, &payload).map_err(frame_io)
}

fn read_result(stream: &UnixStream) -> io::Result<VaultResult> {
    let frame = read_frame(stream).map_err(frame_io)?;
    let response: SessionResponse = serde_json::from_slice(&frame)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match response {
        SessionResponse::Vault(result) => Ok(result),
        SessionResponse::Egress(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave an egress reply to a vault op",
        )),
        SessionResponse::Channel(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a channel reply to a vault op",
        )),
        SessionResponse::Memory(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a memory reply to a vault op",
        )),
        SessionResponse::Skill(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a skill reply to a vault op",
        )),
        SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a goals or query reply to a vault op",
        )),
    }
}

fn store_op(name: &str, secret: &[u8]) -> VaultOp {
    VaultOp::Store {
        name: name.to_owned(),
        secret_b64: encode_secret(secret).into(),
    }
}

fn cleanup(root: &PathBuf) {
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn granted_store_fetch_remove_roundtrip() {
    let grants = vec![
        grant(VaultAction::Store, "app-token").unwrap(),
        grant(VaultAction::Fetch, "app-token").unwrap(),
        grant(VaultAction::Remove, "app-token").unwrap(),
    ];
    let (client, root) = spawn_session("roundtrip", real_uid(), grants).unwrap();
    handshake(&client).unwrap();

    send_op(&client, &store_op("app-token", b"super-secret")).unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::Stored);
    // The broker really wrote through to the store.
    assert!(root.join("app-token").is_file());

    send_op(
        &client,
        &VaultOp::Fetch {
            name: String::from("app-token"),
        },
    )
    .unwrap();
    let result = read_result(&client).unwrap();
    assert!(
        matches!(result, VaultResult::Fetched { ref secret_b64 } if secret_b64.as_str() == encode_secret(b"super-secret")),
        "unexpected result: {result:?}"
    );

    send_op(
        &client,
        &VaultOp::Remove {
            name: String::from("app-token"),
        },
    )
    .unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::Removed);
    assert!(!root.join("app-token").exists());

    send_op(
        &client,
        &VaultOp::Fetch {
            name: String::from("app-token"),
        },
    )
    .unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::NotFound);

    drop(client);
    cleanup(&root);
}

#[test]
fn stored_secret_is_sealed_on_disk_and_fetch_decrypts() {
    // the broker's vault path writes sealed envelopes, never
    // plaintext. Store through the real session pipeline, inspect the
    // raw file bytes, then prove the broker still decrypts on fetch.
    let grants = vec![
        grant(VaultAction::Store, "app-token").unwrap(),
        grant(VaultAction::Fetch, "app-token").unwrap(),
    ];
    let (client, root) = spawn_session("sealed", real_uid(), grants).unwrap();
    handshake(&client).unwrap();

    let secret = b"super-secret-value";
    send_op(&client, &store_op("app-token", secret)).unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::Stored);

    // On-disk proof: a versioned `HVL3` envelope (sealed under the
    // store's own key) — magic, 24-byte nonce, ciphertext+tag — with no
    // plaintext bytes anywhere.
    let raw = std::fs::read(root.join("app-token")).unwrap();
    assert_eq!(raw.get(..4), Some(b"HVL3".as_slice()));
    assert_eq!(raw.len(), 4 + 24 + secret.len() + 16);
    assert!(
        !raw.windows(secret.len()).any(|window| window == secret),
        "plaintext secret bytes on disk"
    );

    // The broker decrypts transparently: the agent's fetch is
    // unchanged by the at-rest sealing.
    send_op(
        &client,
        &VaultOp::Fetch {
            name: String::from("app-token"),
        },
    )
    .unwrap();
    let result = read_result(&client).unwrap();
    assert!(
        matches!(result, VaultResult::Fetched { ref secret_b64 } if secret_b64.as_str() == encode_secret(secret)),
        "unexpected result: {result:?}"
    );

    drop(client);
    cleanup(&root);
}

#[test]
fn ungranted_op_is_denied_before_touching_the_vault() {
    // Only fetch is granted: the store must be denied, and the denial
    // must happen before any filesystem touch — no file may appear.
    let grants = vec![grant(VaultAction::Fetch, "app-token").unwrap()];
    let (client, root) = spawn_session("denied", real_uid(), grants).unwrap();
    handshake(&client).unwrap();

    send_op(&client, &store_op("app-token", b"super-secret")).unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::Denied);
    assert!(
        !root.join("app-token").exists(),
        "denied store must not touch the filesystem"
    );
    // The vault is opened only for granted ops: a denied op must not
    // even create the vault root.
    assert!(!root.exists(), "denied op must not create the vault root");

    // A granted op on a missing name is NotFound, not Denied: the grant
    // check distinguishes "no permission" from "no such secret".
    send_op(
        &client,
        &VaultOp::Fetch {
            name: String::from("app-token"),
        },
    )
    .unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::NotFound);

    // Ungranted remove is denied too, and the session survives denials.
    send_op(
        &client,
        &VaultOp::Remove {
            name: String::from("app-token"),
        },
    )
    .unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::Denied);

    drop(client);
    cleanup(&root);
}

#[test]
fn invalid_name_is_denied_and_session_survives() {
    let grants = vec![
        grant(VaultAction::Store, "app-token").unwrap(),
        grant(VaultAction::Fetch, "app-token").unwrap(),
    ];
    let (client, root) = spawn_session("bad-name", real_uid(), grants).unwrap();
    handshake(&client).unwrap();

    // A traversal name can never match a grant: denied, not a protocol
    // error, and the session stays up.
    send_op(
        &client,
        &VaultOp::Fetch {
            name: String::from("../escape"),
        },
    )
    .unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::Denied);

    // Proof the session survived: a legitimate op still works.
    send_op(&client, &store_op("app-token", b"value")).unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::Stored);

    drop(client);
    cleanup(&root);
}

#[test]
fn unknown_op_tag_closes_session() {
    let grants = vec![grant(VaultAction::Fetch, "app-token").unwrap()];
    let (mut client, root) = spawn_session("unknown-op", real_uid(), grants).unwrap();
    handshake(&client).unwrap();

    // There is no list/bulk op in the protocol: claiming one is a
    // session protocol violation and ends the session silently.
    write_frame(&client, br#"{"op":"list","name":"x"}"#).unwrap();
    let mut buf = [0u8; 64];
    assert_eq!(client.read(&mut buf).unwrap(), 0);

    cleanup(&root);
}

#[test]
fn malformed_base64_store_closes_session() {
    let grants = vec![grant(VaultAction::Store, "app-token").unwrap()];
    let (mut client, root) = spawn_session("bad-b64", real_uid(), grants).unwrap();
    handshake(&client).unwrap();

    // Granted action, valid name, undecodable payload: malformed request
    // bytes end the session.
    let op = VaultOp::Store {
        name: String::from("app-token"),
        secret_b64: String::from("!!! not base64 !!!").into(),
    };
    send_op(&client, &op).unwrap();
    let mut buf = [0u8; 64];
    assert_eq!(client.read(&mut buf).unwrap(), 0);

    cleanup(&root);
}

#[test]
fn oversize_secret_gets_opaque_error_and_session_survives() {
    let grants = vec![
        grant(VaultAction::Store, "app-token").unwrap(),
        grant(VaultAction::Fetch, "app-token").unwrap(),
    ];
    let (client, root) = spawn_session("oversize", real_uid(), grants).unwrap();
    handshake(&client).unwrap();

    // Well-formed but semantically rejected: opaque VaultError, and the
    // session stays up for the next op.
    let big = vec![0xABu8; hatchling_core::MAX_SECRET_BYTES + 1];
    send_op(&client, &store_op("app-token", &big)).unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::VaultError);

    send_op(&client, &store_op("app-token", b"small")).unwrap();
    assert_eq!(read_result(&client).unwrap(), VaultResult::Stored);

    drop(client);
    cleanup(&root);
}

#[test]
fn wrong_uid_peer_never_reaches_vault() {
    // The kernel reports our real uid; the broker expects another. The
    // peer gate fires before the handshake, so the vault dir is never
    // even created.
    let (mut client, root) =
        spawn_session("wrong-uid", real_uid().wrapping_add(1), Vec::new()).unwrap();

    let mut buf = [0u8; 64];
    assert_eq!(client.read(&mut buf).unwrap(), 0);
    assert!(
        !root.exists(),
        "unauthorized peer must not cause vault creation"
    );

    cleanup(&root);
}
