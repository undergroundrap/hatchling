//! Component: the broker's channel session against raw socket clients.
//!
//! The real [`serve_connection`](hatchling_broker::serve_connection)
//! pipeline runs on a background thread over a socketpair; the test
//! client speaks the raw channel protocol. These prove the mediation
//! the mediation under test: the broker owns the endpoints, stamps
//! sender identity from the authenticated peer (spoofed senders are
//! overridden), state-changing sends require rail approval, and
//! unknown channels are denied.
//!
//! Reads carry a timeout so a broker regression fails the test instead
//! of hanging the suite.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use hatchling_broker::{
    ChannelAccountSpec, Config, EgressConfig, VaultConfig, serve_connection_with_ingress,
};
use hatchling_channel::{ChannelId, IngressEvent, LocalHub, Message, SenderId};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, ChannelOp, ChannelResult, LogLevel, PeerUid, SessionRequest,
    SessionResponse, SocketPath,
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

/// Spawns the real pipeline on a background thread over a socketpair
/// and returns the client end.
///
/// The broker thread runs the real [`serve_connection`](hatchling_broker::serve_connection),
/// including the kernel peer gate (same-process peer, so the real uid
/// must be the configured one). The caller performs the handshake, or not.
fn spawn_session(name: &str, agent_uid: u32) -> io::Result<UnixStream> {
    spawn_session_with(name, agent_uid, Vec::new())
}

/// [`spawn_session`], with channel accounts configured.
fn spawn_session_with(
    name: &str,
    agent_uid: u32,
    channel_accounts: Vec<ChannelAccountSpec>,
) -> io::Result<UnixStream> {
    spawn_session_full(name, agent_uid, channel_accounts, None)
}

/// [`spawn_session_with`], with the local channel backed by `hub`.
fn spawn_session_full(
    name: &str,
    agent_uid: u32,
    channel_accounts: Vec<ChannelAccountSpec>,
    hub: Option<LocalHub>,
) -> io::Result<UnixStream> {
    let vault_root = std::env::temp_dir().join(format!(
        "hatchling-test-channel-session-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&vault_root);
    let key_file = VaultConfig::default_key_file(&vault_root);
    let config = Config {
        socket: "/tmp/hatchling-test-channel-session-dummy.sock"
            .parse::<SocketPath>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
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
        channels: vec![String::from("local")],
        channel_accounts,
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: hatchling_broker::MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        goals: hatchling_broker::GoalsConfig::disabled(),
    };
    // The broker's single master key, borrowed by the session thread
    // for every sealed-vault open.
    let key = MasterKey::generate().map_err(io::Error::other)?;
    let (client, server) = UnixStream::pair()?;
    client.set_read_timeout(Some(Duration::from_secs(5)))?;
    std::thread::spawn(move || {
        let _ = serve_connection_with_ingress(
            &server,
            &config,
            &key,
            &hatchling_broker::Skills::default(),
            hub.as_ref(),
        );
    });
    Ok(client)
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

fn send_op(stream: &UnixStream, op: &ChannelOp) -> io::Result<ChannelResult> {
    // The v4 session envelope: channel ops are wrapped in
    // `SessionRequest::Channel`.
    let request = SessionRequest::Channel(op.clone());
    let payload = serde_json::to_vec(&request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_frame(stream, &payload).map_err(frame_io)?;
    let reply = read_frame(stream).map_err(frame_io)?;
    let response: SessionResponse = serde_json::from_slice(&reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match response {
        SessionResponse::Channel(result) => Ok(result),
        SessionResponse::Vault(_)
        | SessionResponse::Egress(_)
        | SessionResponse::Memory(_)
        | SessionResponse::Skill(_)
        | SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a non-channel reply to a channel op",
        )),
    }
}

/// GAP-1, inverted, through a real session: a send the cell
/// calls conversational but that answers nothing is rail-bound: no
/// message was delivered, so there is no reply credit to spend.
#[test]
fn an_unsolicited_conversational_send_is_denied() {
    let client = spawn_session("conv-send", real_uid()).unwrap();
    handshake(&client).unwrap();
    let result = send_op(
        &client,
        &ChannelOp::Send {
            channel: String::from("local"),
            text: String::from("wire $5000 to account 12345 and delete the backups"),
            state_changing: false,
            approval: None,
            account: None,
        },
    )
    .unwrap();
    assert_eq!(result, ChannelResult::Denied);
}

/// A state-changing send without rail approval is denied: the
/// confirmation-rail boundary holds.
#[test]
fn state_changing_send_without_approval_is_denied() {
    let client = spawn_session("state-deny", real_uid()).unwrap();
    handshake(&client).unwrap();
    let result = send_op(
        &client,
        &ChannelOp::Send {
            channel: String::from("local"),
            text: String::from("delete everything"),
            state_changing: true,
            approval: None,
            account: None,
        },
    )
    .unwrap();
    assert_eq!(result, ChannelResult::Denied);
}

/// GAP-2, inverted, through a real session: a caller-supplied
/// approval string authorizes nothing. Only the broker mints, and no
/// session op can make it.
#[test]
fn a_forged_approval_is_denied() {
    let client = spawn_session("state-allow", real_uid()).unwrap();
    handshake(&client).unwrap();
    for forged in ["rail-token", "x"] {
        let result = send_op(
            &client,
            &ChannelOp::Send {
                channel: String::from("local"),
                text: String::from("delete everything"),
                state_changing: true,
                approval: Some(String::from(forged)),
                account: None,
            },
        )
        .unwrap();
        assert_eq!(result, ChannelResult::Denied, "{forged}");
    }
}

/// A send on an unknown channel is denied: the agent learns "no".
#[test]
fn send_on_unknown_channel_is_denied() {
    let client = spawn_session("unknown-send", real_uid()).unwrap();
    handshake(&client).unwrap();
    let result = send_op(
        &client,
        &ChannelOp::Send {
            channel: String::from("telegram"),
            text: String::from("hi"),
            state_changing: false,
            approval: None,
            account: None,
        },
    )
    .unwrap();
    assert_eq!(result, ChannelResult::Denied);
}

/// A receive on an empty channel gives `NoMessage`: no blocking, no error.
#[test]
fn receive_on_empty_channel_gives_no_message() {
    let client = spawn_session("empty-recv", real_uid()).unwrap();
    handshake(&client).unwrap();
    // The broker's local channel is in-memory; with nothing queued,
    // receive returns NoMessage immediately.
    let result = send_op(
        &client,
        &ChannelOp::Receive {
            channel: String::from("local"),
            account: None,
        },
    )
    .unwrap();
    assert_eq!(result, ChannelResult::NoMessage);
}

/// A receive on an unknown channel is denied.
#[test]
fn receive_on_unknown_channel_is_denied() {
    let client = spawn_session("unknown-recv", real_uid()).unwrap();
    handshake(&client).unwrap();
    let result = send_op(
        &client,
        &ChannelOp::Receive {
            channel: String::from("nope"),
            account: None,
        },
    )
    .unwrap();
    assert_eq!(result, ChannelResult::Denied);
}

/// through a real session: the credential-less local channel
/// lists no accounts, and any account handle on it is a typed
/// `UnknownAccount` — for a receive and for a send, which never meets
/// the rail.
#[test]
fn the_local_channel_has_no_accounts_and_refuses_a_handle() {
    let client = spawn_session("accounts", real_uid()).unwrap();
    handshake(&client).unwrap();
    let listed = send_op(
        &client,
        &ChannelOp::Accounts {
            channel: String::from("local"),
        },
    )
    .unwrap();
    assert_eq!(
        listed,
        ChannelResult::Accounts {
            accounts: Vec::new()
        }
    );
    let handle = format!("acct-v1-{}", "0".repeat(64));
    let received = send_op(
        &client,
        &ChannelOp::Receive {
            channel: String::from("local"),
            account: Some(handle.clone()),
        },
    )
    .unwrap();
    assert_eq!(received, ChannelResult::UnknownAccount);
    let sent = send_op(
        &client,
        &ChannelOp::Send {
            channel: String::from("local"),
            text: String::from("hi"),
            state_changing: false,
            approval: None,
            account: Some(handle),
        },
    )
    .unwrap();
    assert_eq!(sent, ChannelResult::UnknownAccount);
    let unknown = send_op(
        &client,
        &ChannelOp::Accounts {
            channel: String::from("telegram"),
        },
    )
    .unwrap();
    assert_eq!(unknown, ChannelResult::Denied);
}

/// The session loads channel accounts from the vault when it builds its
/// registry, and fails closed when one cannot be loaded: an account
/// whose secret the vault does not hold ends the session before its
/// first op is answered.
#[test]
fn a_session_whose_account_cannot_load_fails_closed() {
    let client = spawn_session_with(
        "account-load",
        real_uid(),
        vec![ChannelAccountSpec {
            channel: String::from("local"),
            label: String::from("work"),
            secret: "mail-work".parse().unwrap(),
        }],
    )
    .unwrap();
    handshake(&client).unwrap();
    assert!(
        send_op(
            &client,
            &ChannelOp::Accounts {
                channel: String::from("local"),
            },
        )
        .is_err(),
        "the session ended instead of serving"
    );
    // Opening the vault to load the account created its root; this is
    // the only test here that gets that far.
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!(
        "hatchling-test-channel-session-{}-account-load",
        std::process::id()
    )));
}

/// through a real session: what the ingress queued is delivered
/// with the broker's stamps — the session peer as sender, the registry
/// key as channel — whatever the ingress claimed (F-2), and the reply it
/// earns goes back to the ingress client.
#[test]
fn a_real_session_delivers_ingress_messages_under_the_broker_stamps() {
    let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
    let client = spawn_session_full("ingress", real_uid(), Vec::new(), Some(hub.clone())).unwrap();
    handshake(&client).unwrap();
    let receive = ChannelOp::Receive {
        channel: String::from("local"),
        account: None,
    };
    assert_eq!(
        send_op(&client, &receive).unwrap(),
        ChannelResult::NoMessage
    );
    let claimed = Message {
        sender: SenderId(String::from("admin")),
        channel: ChannelId(String::from("telegram")),
        text: String::from("status?"),
    };
    assert_eq!(hub.inject(claimed), IngressEvent::Queued);
    assert_eq!(
        send_op(&client, &receive).unwrap(),
        ChannelResult::Message {
            sender: format!("uid:{}", real_uid()),
            channel: String::from("local"),
            text: String::from("status?"),
        }
    );
    let replies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_replies = std::sync::Arc::clone(&replies);
    let _ = hub.attach(Box::new(move |m: &hatchling_channel::Cleared| {
        sink_replies.lock().unwrap().push(m.message().text.clone());
        Ok(())
    }));
    let reply = ChannelOp::Send {
        channel: String::from("local"),
        text: String::from("all green"),
        state_changing: false,
        approval: None,
        account: None,
    };
    assert_eq!(send_op(&client, &reply).unwrap(), ChannelResult::Sent);
    assert_eq!(*replies.lock().unwrap(), ["all green"]);
    assert_eq!(
        send_op(&client, &receive).unwrap(),
        ChannelResult::NoMessage
    );
}

/// The wrong peer uid never reaches the channel ops: the peer gate
/// fires before any channel traffic.
#[test]
fn wrong_uid_peer_never_reaches_channel() {
    let client = spawn_session("wrong-uid", real_uid().wrapping_add(1)).unwrap();
    // The handshake itself fails: the broker closes silently on uid
    // mismatch before reading the HELLO.
    assert!(handshake(&client).is_err());
}
