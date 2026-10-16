//! Component tests: the agent's vault API against the real broker.
//!
//! The [`Vault`](common::BrokerBehavior::Vault) fixture runs the true
//! broker pipeline ([`serve_connection`](hatchling_broker::serve_connection)):
//! kernel peer gate, grant mediation, and the session loop. These tests
//! drive it through the agent's public API only — [`Cell<Ready>`]'s
//! `vault_store` / `vault_fetch` / `vault_remove` — proving the two
//! halves compose into the mediated credential store the task is
//! about. The [`Canned`](common::BrokerBehavior::Canned),
//! [`Raw`](common::BrokerBehavior::Raw), and
//! [`Hangup`](common::BrokerBehavior::Hangup) fixtures pin the agent's
//! reply mapping for broker behaviors the real broker would never
//! produce.

pub(crate) mod common;

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::{
    Cell, Config, Error, LogLevel, MAX_SECRET_BYTES, PeerUid, Ready, VaultAction,
};
use hatchling_core::VaultResult;

fn grant(
    action: VaultAction,
    name: &str,
) -> Result<hatchling_broker::VaultGrant, hatchling_core::Error> {
    Ok(hatchling_broker::VaultGrant {
        action,
        name: name.parse()?,
    })
}

/// A `Ready` cell against the fixture broker's socket.
fn ready_cell(broker: &FixtureBroker) -> Result<Cell<Ready>, Error> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-vault".parse()?,
        broker: hatchling_agent::BrokerAddress::Socket(broker.socket_path.parse()?),
        broker_uid: PeerUid::from_kernel(uid),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        run_subagent: false,
    };
    Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()
}

#[test]
fn vault_roundtrip_through_real_broker() {
    let broker = FixtureBroker::spawn(
        "vault-roundtrip",
        BrokerBehavior::Vault(vec![
            grant(VaultAction::Store, "api-key").unwrap(),
            grant(VaultAction::Fetch, "api-key").unwrap(),
            grant(VaultAction::Remove, "api-key").unwrap(),
        ]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    cell.vault_store("api-key", b"super-secret").unwrap();
    // The broker really wrote through to the store on disk.
    assert!(
        broker
            .vault_root()
            .is_some_and(|root| root.join("api-key").is_file()),
        "expected the secret file on disk"
    );
    assert_eq!(
        cell.vault_fetch("api-key").unwrap(),
        Some(b"super-secret".to_vec())
    );

    cell.vault_remove("api-key").unwrap();
    assert_eq!(cell.vault_fetch("api-key").unwrap(), None);
}

#[test]
fn vault_store_denied_without_grant() {
    let broker = FixtureBroker::spawn(
        "vault-denied",
        BrokerBehavior::Vault(vec![grant(VaultAction::Fetch, "api-key").unwrap()]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    let err = cell.vault_store("api-key", b"super-secret").unwrap_err();
    assert!(
        matches!(err, Error::VaultDenied { .. }),
        "expected VaultDenied, got {err:?}"
    );
    if let Error::VaultDenied { action, name } = err {
        assert_eq!(action, "store");
        assert_eq!(name, "api-key");
    }
    // The denial fired before any filesystem touch.
    assert!(
        broker
            .vault_root()
            .is_some_and(|root| !root.join("api-key").exists()),
        "denied store must not create a secret file"
    );
}

#[test]
fn vault_fetch_missing_returns_none() {
    let broker = FixtureBroker::spawn(
        "vault-fetch-missing",
        BrokerBehavior::Vault(vec![grant(VaultAction::Fetch, "api-key").unwrap()]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    assert_eq!(cell.vault_fetch("api-key").unwrap(), None);
}

#[test]
fn vault_remove_missing_is_idempotent() {
    let broker = FixtureBroker::spawn(
        "vault-remove-missing",
        BrokerBehavior::Vault(vec![grant(VaultAction::Remove, "api-key").unwrap()]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    cell.vault_remove("api-key").unwrap();
}

#[test]
fn vault_invalid_name_rejected_client_side() {
    // No vault behavior needed: the name never reaches the broker.
    let broker = FixtureBroker::spawn("vault-bad-name", BrokerBehavior::Welcome).unwrap();
    let cell = ready_cell(&broker).unwrap();

    let err = cell.vault_store("../escape", b"x").unwrap_err();
    assert!(
        matches!(err, Error::InvalidSecretName(_)),
        "expected InvalidSecretName, got {err:?}"
    );
}

#[test]
fn vault_oversize_secret_rejected_client_side() {
    let broker = FixtureBroker::spawn("vault-too-big", BrokerBehavior::Welcome).unwrap();
    let cell = ready_cell(&broker).unwrap();

    let big = vec![0xABu8; MAX_SECRET_BYTES + 1];
    let err = cell.vault_store("api-key", &big).unwrap_err();
    assert!(
        matches!(err, Error::SecretTooLarge { len, max } if len == MAX_SECRET_BYTES + 1 && max == MAX_SECRET_BYTES),
        "expected SecretTooLarge, got {err:?}"
    );
    assert_eq!(
        err.to_string(),
        format!(
            "secret of {} bytes exceeds the {MAX_SECRET_BYTES}-byte wire limit",
            MAX_SECRET_BYTES + 1
        )
    );
}

#[test]
fn vault_max_size_secret_accepted_client_side() {
    // Boundary pin: a secret of exactly MAX_SECRET_BYTES must pass the
    // client-side size check and reach the broker. Kills the `>` -> `>=`
    // mutant on the `secret.len() > MAX_SECRET_BYTES` guard (the mutant
    // rejects the boundary with SecretTooLarge).
    let broker = FixtureBroker::spawn(
        "vault-max-size",
        BrokerBehavior::Vault(vec![grant(VaultAction::Store, "api-key").unwrap()]),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    let exact = vec![0xABu8; MAX_SECRET_BYTES];
    cell.vault_store("api-key", &exact).unwrap();
    assert!(
        broker
            .vault_root()
            .is_some_and(|root| root.join("api-key").is_file()),
        "max-size store must reach the broker and land on disk"
    );
}

#[test]
fn vault_broker_backend_failure_maps_to_vault_failed() {
    let broker = FixtureBroker::spawn(
        "vault-backend-failure",
        BrokerBehavior::Canned(VaultResult::VaultError),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    let err = cell.vault_store("api-key", b"x").unwrap_err();
    assert!(
        matches!(err, Error::VaultFailed),
        "expected VaultFailed, got {err:?}"
    );
}

#[test]
fn vault_mismatched_reply_is_protocol_violation() {
    // A `Fetched` answer to a `Store` op: well-formed but impossible.
    let broker = FixtureBroker::spawn(
        "vault-mismatch",
        BrokerBehavior::Canned(VaultResult::Fetched {
            secret_b64: String::from("eA=="),
        }),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    let err = cell.vault_store("api-key", b"x").unwrap_err();
    assert!(
        matches!(err, Error::VaultProtocol { .. }),
        "expected VaultProtocol, got {err:?}"
    );
}

#[test]
fn vault_garbage_reply_is_protocol_violation() {
    let broker = FixtureBroker::spawn(
        "vault-garbage",
        BrokerBehavior::Raw(b"this is not a vault result".to_vec()),
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();

    let err = cell.vault_fetch("api-key").unwrap_err();
    assert!(
        matches!(err, Error::VaultProtocol { .. }),
        "expected VaultProtocol, got {err:?}"
    );
}

#[test]
fn vault_mid_request_hangup_is_io_error() {
    let broker = FixtureBroker::spawn("vault-hangup", BrokerBehavior::Hangup).unwrap();
    let cell = ready_cell(&broker).unwrap();

    let err = cell.vault_fetch("api-key").unwrap_err();
    assert!(
        matches!(err, Error::VaultIo { .. }),
        "expected VaultIo, got {err:?}"
    );
}
