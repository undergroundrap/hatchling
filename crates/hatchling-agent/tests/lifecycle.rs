//! Component test: the full cell lifecycle through the public API only.
//!
//! The in-module unit tests may use crate internals; this suite is
//! restricted to what a real embedder could touch — [`Config`], [`Cell`],
//! and the typestate transitions — proving the pieces compose into a
//! working lifecycle.

pub(crate) mod common;

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid};

/// Build a valid config against the fixture broker's socket.
fn config_for(socket_path: &str, broker_uid: u32) -> Result<Config, Error> {
    Ok(Config {
        cell_id: "cell-component".parse()?,
        broker: hatchling_agent::BrokerAddress::Socket(socket_path.parse()?),
        broker_uid: PeerUid::from_kernel(broker_uid),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        run_subagent: false,
        run_chat: false,
        run_history: Vec::new(),
        run_builders: std::collections::BTreeMap::new(),
        vault_fd: None,
    })
}

/// The full lifecycle reaches `Ready` against a well-behaved broker.
#[test]
fn lifecycle_reaches_ready() {
    let broker = FixtureBroker::spawn("component-ready", BrokerBehavior::Welcome).unwrap();
    let uid = rustix::process::getuid().as_raw();
    let cell = Cell::new(config_for(&broker.socket_path, uid).unwrap())
        .check_preconditions()
        .unwrap()
        .connect_broker()
        .unwrap()
        .authorize()
        .unwrap()
        .handshake()
        .unwrap();
    assert_eq!(cell.config().cell_id.as_str(), "cell-component");
    // `Ready` is the only state that exposes the live connection, and the
    // peer address proves the socket is genuinely open.
    assert!(cell.broker().peer_addr().is_ok());
}

/// A rejecting broker fails the handshake fail-closed: no `Ready`, and the
/// typed reason survives.
#[test]
fn lifecycle_reject_fails_closed() {
    let broker = FixtureBroker::spawn(
        "component-reject",
        BrokerBehavior::Reject("not today".to_owned()),
    )
    .unwrap();
    let uid = rustix::process::getuid().as_raw();
    let err = Cell::new(config_for(&broker.socket_path, uid).unwrap())
        .check_preconditions()
        .unwrap()
        .connect_broker()
        .unwrap()
        .authorize()
        .unwrap()
        .handshake()
        .unwrap_err();
    assert!(matches!(err, Error::HandshakeRejected { .. }));
    if let Error::HandshakeRejected { reason } = err {
        assert_eq!(reason, "not today");
    }
}
