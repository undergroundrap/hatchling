//! Component tests: the agent's egress API against the real broker.
//!
//! The [`Egress`](common::BrokerBehavior::Egress) fixture runs the true
//! broker pipeline ([`serve_connection`](hatchling_broker::serve_connection)):
//! kernel peer gate, grant mediation, and the session loop. These tests
//! drive it through the agent's public API only — [`Cell<Ready>`]'s
//! `egress_mint_surrogate` — proving the two halves compose into the
//! mediated egress this tests.

pub(crate) mod common;

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid, Ready};

/// A `Ready` cell against the fixture broker's socket.
fn ready_cell(broker: &FixtureBroker) -> Result<Cell<Ready>, Error> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-egress"
            .parse()
            .map_err(|e| Error::InvalidCellId(format!("{e:?}")))?,
        broker: hatchling_agent::BrokerAddress::Socket(
            broker
                .socket_path
                .parse()
                .map_err(|e| Error::InvalidSocketPath(format!("{e:?}")))?,
        ),
        broker_uid: PeerUid::from_kernel(uid),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        run_subagent: false,
        run_chat: false,
        run_history: Vec::new(),
        vault_fd: None,
    };
    Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()
}

/// `Cell<Ready>::egress_mint_surrogate` returns the broker-minted
/// surrogate verbatim. The `Ok(String::new())` and `Ok("xyzzy".into())`
/// mutants would return an empty or canned string instead of the
/// broker's value.
#[test]
fn egress_mint_surrogate_returns_broker_value() {
    let broker = FixtureBroker::spawn(
        "egress-mint",
        BrokerBehavior::Egress {
            allowlist: "example.com:443".to_string(),
            grants: vec!["example.com:443".to_string()],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let surrogate = cell.egress_mint_surrogate("example.com:443", 3600).unwrap();
    // The broker mints a real surrogate: non-empty, not a canned
    // value. A stub returning `Ok(String::new())` or `Ok("xyzzy")`
    // would fail these assertions.
    assert!(!surrogate.is_empty(), "surrogate must not be empty");
    assert_ne!(surrogate, "xyzzy", "surrogate must not be a canned value");
    // The surrogate must be long enough to be a real HMAC token, not
    // a stub. Minimum: base64 of 47-byte payload is 64 chars.
    assert!(
        surrogate.len() >= 64,
        "surrogate must be at least 64 chars (base64 of 47-byte payload), got {}",
        surrogate.len()
    );
}

/// `Cell<Ready>::egress_mint_surrogate` maps a broker denial to
/// `EgressDenied`. Pins the method's error propagation (the method
/// must not swallow the broker's denial and return Ok).
#[test]
fn egress_mint_surrogate_denied() {
    let broker = FixtureBroker::spawn(
        "egress-denied",
        BrokerBehavior::Egress {
            allowlist: "example.com:443".to_string(),
            grants: vec![],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    // No grant for the destination: broker denies.
    let err = cell
        .egress_mint_surrogate("example.com:443", 3600)
        .unwrap_err();
    assert!(
        matches!(err, Error::EgressDenied { .. }),
        "expected EgressDenied, got {err:?}"
    );
}
