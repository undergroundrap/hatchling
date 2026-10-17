//! Agent side of the vault protocol.
//!
//! The framing and message vocabulary live in [`hatchling_core`]; this
//! module is the agent's behavior: validate the name client-side, send
//! one op per frame, and require a well-formed [`VaultResult`] in reply.
//! Must only be called on a [`Ready`](crate::Ready) cell — the typestate
//! enforces this.

use std::os::unix::net::UnixStream;

use hatchling_core::MAX_SECRET_BYTES;
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    SecretB64, SecretName, SessionRequest, SessionResponse, VaultOp, VaultResult, decode_secret,
};

use crate::Error;

/// Stores `secret` under `name`, replacing any existing value.
///
/// The name is validated client-side (no round trip is spent on a name
/// the broker would deny); the secret is size-checked against the wire
/// limit before encoding.
///
/// # Errors
///
/// Returns [`Error::InvalidSecretName`] or [`Error::SecretTooLarge`]
/// before any I/O, [`Error::VaultDenied`] if the broker holds no grant,
/// [`Error::VaultFailed`] on a broker-side backend failure,
/// [`Error::VaultIo`] on transport failure, or [`Error::VaultProtocol`]
/// on a malformed broker reply.
pub fn store(stream: &UnixStream, name: &str, secret: &[u8]) -> Result<(), Error> {
    let name = validated_name(name)?;
    if secret.len() > MAX_SECRET_BYTES {
        return Err(Error::SecretTooLarge {
            len: secret.len(),
            max: MAX_SECRET_BYTES,
        });
    }
    let op = VaultOp::Store {
        name: name.as_str().to_owned(),
        secret_b64: SecretB64::encode(secret),
    };
    match request(stream, &op)? {
        VaultResult::Stored => Ok(()),
        VaultResult::Denied => Err(denied(&op, &name)),
        VaultResult::VaultError => Err(Error::VaultFailed),
        other @ (VaultResult::Fetched { .. } | VaultResult::NotFound | VaultResult::Removed) => {
            Err(unexpected_reply(&op, &other))
        }
    }
}

/// Fetches the secret stored under `name`.
///
/// Returns `Ok(None)` when no secret is stored under the name.
///
/// # Errors
///
/// Returns [`Error::InvalidSecretName`] before any I/O,
/// [`Error::VaultDenied`] if the broker holds no grant,
/// [`Error::VaultFailed`] on a broker-side backend failure,
/// [`Error::VaultIo`] on transport failure, or [`Error::VaultProtocol`]
/// on a malformed broker reply.
pub fn fetch(stream: &UnixStream, name: &str) -> Result<Option<Vec<u8>>, Error> {
    let name = validated_name(name)?;
    let op = VaultOp::Fetch {
        name: name.as_str().to_owned(),
    };
    match request(stream, &op)? {
        VaultResult::Fetched { secret_b64 } => {
            let bytes = decode_secret(secret_b64.as_str()).map_err(|error| Error::VaultProtocol {
                reason: error.to_string(),
            })?;
            Ok(Some(bytes))
        }
        VaultResult::NotFound => Ok(None),
        VaultResult::Denied => Err(denied(&op, &name)),
        VaultResult::VaultError => Err(Error::VaultFailed),
        other @ (VaultResult::Stored | VaultResult::Removed) => Err(unexpected_reply(&op, &other)),
    }
}

/// Removes the secret stored under `name`.
///
/// Idempotent: removing a missing secret succeeds. Whether anything was
/// actually removed is deliberately not reported — the postcondition
/// ("no secret under this name") holds either way.
///
/// # Errors
///
/// Returns [`Error::InvalidSecretName`] before any I/O,
/// [`Error::VaultDenied`] if the broker holds no grant,
/// [`Error::VaultFailed`] on a broker-side backend failure,
/// [`Error::VaultIo`] on transport failure, or [`Error::VaultProtocol`]
/// on a malformed broker reply.
pub fn remove(stream: &UnixStream, name: &str) -> Result<(), Error> {
    let name = validated_name(name)?;
    let op = VaultOp::Remove {
        name: name.as_str().to_owned(),
    };
    match request(stream, &op)? {
        VaultResult::Removed | VaultResult::NotFound => Ok(()),
        VaultResult::Denied => Err(denied(&op, &name)),
        VaultResult::VaultError => Err(Error::VaultFailed),
        other @ (VaultResult::Stored | VaultResult::Fetched { .. }) => {
            Err(unexpected_reply(&op, &other))
        }
    }
}

/// Validates the secret name client-side: no round trip is spent on a
/// name the broker would deny. (The broker re-validates regardless: the
/// agent's bytes are never trusted.)
fn validated_name(name: &str) -> Result<SecretName, Error> {
    name.parse::<SecretName>()
        .map_err(|error| Error::InvalidSecretName(error.to_string()))
}

/// Maps a frame-layer failure onto the vault vocabulary: transport
/// failures are [`VaultIo`](Error::VaultIo), everything else is a
/// protocol violation. Used for both directions of a request.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::VaultIo { source };
    }
    Error::VaultProtocol {
        reason: error.to_string(),
    }
}

/// Sends one op and reads the broker's reply.
///
/// The session speaks [`SessionRequest`]/[`SessionResponse`] envelopes
/// (v3). Every op gets exactly one reply. EOF mid-op is a transport
/// failure; a reply that is not a well-formed `SessionResponse` or that
/// carries the wrong kind is a protocol violation. Fail-closed
/// throughout.
fn request(stream: &UnixStream, op: &VaultOp) -> Result<VaultResult, Error> {
    let request = SessionRequest::Vault(op.clone());
    let payload = serde_json::to_vec(&request).map_err(|e| Error::VaultProtocol {
        reason: format!("failed to encode vault op: {e}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|e| Error::VaultProtocol {
            reason: format!("broker reply is not a valid session response: {e}"),
        })?;
    match response {
        SessionResponse::Vault(result) => Ok(result),
        SessionResponse::Egress(_) => Err(Error::VaultProtocol {
            reason: String::from("broker gave an egress reply to a vault op"),
        }),
        SessionResponse::Channel(_) => Err(Error::VaultProtocol {
            reason: String::from("broker gave a channel reply to a vault op"),
        }),
        SessionResponse::Memory(_) => Err(Error::VaultProtocol {
            reason: String::from("broker gave a memory reply to a vault op"),
        }),
        SessionResponse::Skill(_) => Err(Error::VaultProtocol {
            reason: String::from("broker gave a skill reply to a vault op"),
        }),
        SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_) => Err(Error::VaultProtocol {
            reason: String::from("broker gave a goals or query reply to a vault op"),
        }),
    }
}

/// Builds the denial error for an op the broker holds no grant for.
fn denied(op: &VaultOp, name: &SecretName) -> Error {
    Error::VaultDenied {
        action: op.action().to_string(),
        name: name.as_str().to_owned(),
    }
}

/// Builds the protocol-violation error for a reply that does not match
/// the op that was sent (e.g. a `Fetched` answer to a `Store`).
fn unexpected_reply(op: &VaultOp, result: &VaultResult) -> Error {
    Error::VaultProtocol {
        reason: format!(
            "broker gave an unexpected reply to {}: {result:?}",
            op.action(),
        ),
    }
}
