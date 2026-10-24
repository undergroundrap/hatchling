//! The device endpoint: where a paired device syncs a space.
//!
//! The broker binds a Unix socket at `HATCHLING_DEVICE_SOCKET` and owns it
//! for its lifetime. It is kept from the cell as the channel ingress is:
//! mode `0600`, a kernel peer gate on the socket owner's uid (refused at
//! startup if that is the agent's), and config refuses a path inside the
//! directory the cell mounts. Nothing is discovered: a device reaches the
//! broker only through this socket, and gets nothing until it pairs.
//!
//! **Both ends authenticate before any state moves.** The broker opens
//! with a [`DeviceChallenge`]: a fresh nonce and its device key. The device
//! answers with a [`DeviceHello`], signed over that nonce with its own key:
//!
//! - `pair`: redeem a pairing code the agent issued
//!   ([`SpaceOp::Pair`](hatchling_core::SpaceOp::Pair)), registering its
//!   key and joining that one space. The code is single-use and
//!   short-lived; a used, expired, or unknown one is refused and logged.
//!   The signature is checked before the code is spent.
//! - `auth`: authenticate as a device already paired.
//!
//! The broker answers with its own signature over the device's nonce, which
//! the device checks against the broker key it pinned at pairing. Only
//! then does the session carry [`SpaceOp`](hatchling_core::SpaceOp)
//! frames, as [`Principal::Device`]: every one through the same admission
//! and membership checks as the agent's, for the spaces it was paired into
//! and no others. A frame that is not a hello where one is due ends the
//! connection before anything is read from the store.

use std::os::unix::net::{UnixListener, UnixStream};

use base64::Engine as _;
use ed25519_dalek::Signer as _;
use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::{
    DeviceChallenge, DeviceHello, DeviceRefusal, DeviceWelcome, MAX_FRAME_BYTES, PROTOCOL_VERSION,
    PeerCredentials, PeerUid, SocketPath, SpaceOp, SpaceResult, peer_credentials,
};
use hatchling_memory::SessionId;
use hatchling_scheduler::Timestamp;
use hatchling_space::{
    DeviceRecord, PeerPin, Principal, SessionPin, auth_transcript, broker_transcript, device_id,
    is_nonce, pair_transcript, parse_key, verifies,
};
use hatchling_vault::OpensStores;

use crate::serve::session_error;
use crate::space_ops::{EndAdmissions, SealedSpaceStore, SpaceSession, handle_space_op, pairings};
use crate::listener::bind_owner_socket;
use crate::{Config, Error};

/// The device socket's permission bits: owner only.
pub const DEVICE_SOCKET_MODE: u32 = 0o600;

/// The bound device endpoint: its socket, and the one uid it serves.
#[derive(Debug)]
pub struct DeviceEndpoint {
    listener: UnixListener,
    peer_uid: PeerUid,
}

impl DeviceEndpoint {
    /// Binds the device socket at `socket`, mode [`DEVICE_SOCKET_MODE`],
    /// for the broker's owner, who is given it and is the only peer it
    /// serves.
    ///
    /// # Errors
    ///
    /// The errors of [`bind_owner_socket`].
    pub fn bind(socket: &SocketPath) -> Result<Self, Error> {
        // Made for the broker's owner, recorded at start, never read back
        // from a path that could be swapped.
        let owner = crate::owner::owner()?;
        let listener = bind_owner_socket(socket, DEVICE_SOCKET_MODE, owner)?;
        Ok(Self {
            listener,
            peer_uid: PeerUid::from_kernel(owner),
        })
    }

    /// The only uid this endpoint serves.
    #[must_use]
    pub const fn peer_uid(&self) -> PeerUid {
        self.peer_uid
    }

    /// Serves devices one at a time, forever. A failed connection is
    /// logged and the next one accepted.
    pub fn serve(&self, config: &Config, key: &dyn OpensStores) -> ! {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) = serve_device(&stream, config, key, self.peer_uid) {
                        tracing::warn!(%error, "device connection failed");
                    }
                }
                Err(source) => tracing::error!(%source, "device accept failed"),
            }
        }
    }
}

/// The broker's device endpoint, bound, when one is configured.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when the endpoint would serve the agent's own
/// uid (the socket is unlinked again first), or the errors of
/// [`DeviceEndpoint::bind`].
pub fn open_device_endpoint(config: &Config) -> Result<Option<DeviceEndpoint>, Error> {
    let Some(socket) = &config.spaces.device_socket else {
        return Ok(None);
    };
    let endpoint = DeviceEndpoint::bind(socket)?;
    if endpoint.peer_uid() == config.agent_uid {
        let _ = std::fs::remove_file(socket.as_path());
        return Err(Error::InvalidConfig {
            key: "HATCHLING_DEVICE_SOCKET",
            reason: format!(
                "the device endpoint serves uid {}, the broker's own, which is also the agent's: \
                 the cell could pose as a device; {}",
                config.agent_uid,
                crate::cell::OWN_UID_HINT
            ),
        });
    }
    Ok(Some(endpoint))
}

/// Serves one device connection on `stream`, for a peer of `peer_uid`:
/// the challenge, the hello, and, once both ends are authenticated, its
/// space ops until it disconnects.
///
/// # Errors
///
/// [`Error::PeerUnauthorized`] for another peer (closed before a byte is
/// read); [`Error::SessionIo`] or [`Error::SessionProtocol`] when the
/// stream fails or carries something else where a hello or a space op is
/// due; the store's errors.
pub fn serve_device(
    stream: &UnixStream,
    config: &Config,
    key: &dyn OpensStores,
    peer_uid: PeerUid,
) -> Result<(), Error> {
    let creds = peer_credentials(stream)?;
    if creds.uid() != peer_uid {
        return Err(Error::PeerUnauthorized {
            actual: creds.uid(),
            expected: peer_uid,
        });
    }
    if !config.spaces.enabled() {
        return Err(Error::SessionProtocol {
            reason: String::from("the device endpoint needs the space store"),
        });
    }
    let store = SealedSpaceStore::new(&config.spaces.root, key);
    let signing = store.broker_key()?;
    let nonce = crate::approval::random_hex("").map_err(|source| Error::SessionIo { source })?;
    send(
        stream,
        &DeviceChallenge {
            v: PROTOCOL_VERSION,
            nonce: nonce.clone(),
            broker_key: base64::engine::general_purpose::STANDARD
                .encode(signing.verifying_key().as_bytes()),
        },
    )?;
    let hello: DeviceHello = serde_json::from_slice(&read_frame(stream).map_err(session_error)?)
        .map_err(|e| Error::SessionProtocol {
            reason: format!("device hello is not a hello: {e}"),
        })?;
    let now = Timestamp::now().as_second();
    let outcome = match &hello {
        DeviceHello::Pair {
            code,
            device_key,
            nonce: theirs,
            signature,
        } => pair(&store, &nonce, code, device_key, theirs, signature, now),
        DeviceHello::Auth {
            device,
            nonce: theirs,
            signature,
        } => authenticate(&store, &nonce, device, theirs, signature),
    };
    let (device, theirs, space) = match outcome {
        Ok(admitted) => admitted,
        Err(reason) => {
            tracing::warn!(?reason, "device refused before any state moved");
            return send(stream, &DeviceWelcome::Refused { reason });
        }
    };
    let signature = base64::engine::general_purpose::STANDARD.encode(
        signing
            .sign(&broker_transcript(&theirs, &device))
            .to_bytes(),
    );
    send(
        stream,
        &match space {
            Some(space) => DeviceWelcome::Paired {
                device: device.clone(),
                space,
                signature,
            },
            None => DeviceWelcome::Authenticated {
                device: device.clone(),
                signature,
            },
        },
    )?;
    serve_ops(stream, config, key, creds, device)
}

/// What a hello establishes: the device id, its nonce, and for a pairing,
/// the space it joined.
type Admitted = (String, String, Option<String>);

/// Pairs a new device: checks its key and signature first, then spends the
/// code, then registers the device and makes it a member of the space.
fn pair(
    store: &SealedSpaceStore<'_>,
    broker_nonce: &str,
    code: &str,
    device_key: &str,
    theirs: &str,
    signature: &str,
    now: i64,
) -> Result<Admitted, DeviceRefusal> {
    let key = parse_key(device_key)
        .filter(|_| is_nonce(theirs))
        .ok_or(DeviceRefusal::BadSignature)?;
    if !verifies(
        &key,
        &pair_transcript(broker_nonce, code, device_key),
        signature,
    ) {
        return Err(DeviceRefusal::BadSignature);
    }
    let space = pairings().redeem(code, now).map_err(|reason| {
        tracing::warn!(?reason, "pairing code refused");
        DeviceRefusal::BadCode
    })?;
    let failed = |error: Error| {
        tracing::error!(%error, "device store failed while pairing");
        DeviceRefusal::DeviceError
    };
    let id = device_id(&key);
    let mut record = store
        .get_device(&id)
        .map_err(failed)?
        .unwrap_or_else(|| DeviceRecord {
            id: id.clone(),
            key: device_key.to_owned(),
            spaces: Vec::new(),
            paired_at: now,
        });
    let mut joined = store
        .get(&space)
        .map_err(failed)?
        .ok_or(DeviceRefusal::BadCode)?;
    joined
        .add_member(Principal::Device { id: id.clone() })
        .map_err(|_| DeviceRefusal::Full)?;
    if !record.spaces.contains(&space) {
        record.spaces.push(space.clone());
    }
    if !store.put_device(&record).map_err(failed)? {
        return Err(DeviceRefusal::Full);
    }
    if !store.put(&joined).map_err(failed)? {
        return Err(DeviceRefusal::BadCode);
    }
    tracing::info!(device = %id, space = %space, "device paired");
    Ok((id, theirs.to_owned(), Some(space)))
}

/// Authenticates a paired device by its signature over the broker's nonce.
fn authenticate(
    store: &SealedSpaceStore<'_>,
    broker_nonce: &str,
    device: &str,
    theirs: &str,
    signature: &str,
) -> Result<Admitted, DeviceRefusal> {
    let record = store
        .get_device(device)
        .map_err(|error| {
            tracing::error!(%error, "device store failed while authenticating");
            DeviceRefusal::DeviceError
        })?
        .ok_or(DeviceRefusal::Unpaired)?;
    let key = parse_key(&record.key)
        .filter(|_| is_nonce(theirs))
        .ok_or(DeviceRefusal::BadSignature)?;
    if !verifies(&key, &auth_transcript(broker_nonce, device), signature) {
        return Err(DeviceRefusal::BadSignature);
    }
    Ok((record.id, theirs.to_owned(), None))
}

/// The authenticated device's space ops, until it disconnects.
fn serve_ops(
    stream: &UnixStream,
    config: &Config,
    key: &dyn OpensStores,
    creds: PeerCredentials,
    device: String,
) -> Result<(), Error> {
    let session = SessionId::generate().map_err(|source| Error::Memory { source })?;
    let space_session = SpaceSession {
        principal: Principal::Device { id: device },
        pin: SessionPin {
            session: session.as_str().to_owned(),
            peer: PeerPin {
                uid: creds.uid().as_u32(),
                pid: creds.pid().as_u32(),
            },
        },
    };
    let _admissions = EndAdmissions(space_session.pin.session.clone());
    while let Some(frame) = read_frame_or_eof(stream).map_err(session_error)? {
        let op: SpaceOp = serde_json::from_slice(&frame).map_err(|e| Error::SessionProtocol {
            reason: format!("device frame is not a space op: {e}"),
        })?;
        let result = handle_space_op(config, key, &space_session, &op, Timestamp::now());
        let payload = serde_json::to_vec(&result)
            .ok()
            .filter(|payload| payload.len() <= MAX_FRAME_BYTES)
            .map_or_else(|| serde_json::to_vec(&SpaceResult::SpaceError), Ok)
            .map_err(|e| Error::SessionProtocol {
                reason: format!("device reply encode failed: {e}"),
            })?;
        write_frame(stream, &payload).map_err(session_error)?;
    }
    Ok(())
}

/// Writes one frame to the device.
fn send<T: serde::Serialize>(stream: &UnixStream, message: &T) -> Result<(), Error> {
    let payload = serde_json::to_vec(message).map_err(|e| Error::SessionProtocol {
        reason: format!("device message encode failed: {e}"),
    })?;
    write_frame(stream, &payload).map_err(session_error)
}

/// The pairing codes refused so far, oldest first: what the activity feed
/// will read.
#[must_use]
pub fn pairing_refusals() -> Vec<hatchling_space::RefusedPairing> {
    pairings().refusals().to_vec()
}

#[cfg(test)]
mod tests {
    use hatchling_vault::MasterKey;

    use super::*;

    /// A device's session ends with its connection, and a frame that
    /// isn't a space op ends it with a protocol error, answering nothing.
    #[test]
    fn serve_ops_ends_at_eof_and_refuses_a_frame_that_is_not_a_space_op() {
        let config = crate::api_auth::tests::owner_config(None);
        let key = MasterKey::generate().unwrap();
        let (device, broker) = UnixStream::pair().unwrap();
        drop(device);
        let creds = peer_credentials(&broker).unwrap();
        assert!(serve_ops(&broker, &config, &key, creds, String::from("dev-eof")).is_ok());

        let (device, broker) = UnixStream::pair().unwrap();
        write_frame(&device, b"not a space op").unwrap();
        let creds = peer_credentials(&broker).unwrap();
        let ended = serve_ops(&broker, &config, &key, creds, String::from("dev-bad"));
        drop(broker);
        assert!(
            matches!(ended, Err(Error::SessionProtocol { .. })),
            "{ended:?}"
        );
        device
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        assert!(read_frame_or_eof(&device).unwrap().is_none());
    }
}
