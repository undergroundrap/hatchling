//! Typed errors for `hatchling-broker`.
//!
//! Every failure the broker can report is a variant here. Callers match
//! exhaustively: adding a variant forces each decision site to be
//! revisited. There are no stringly errors and no boxed dynamic errors.

use thiserror::Error;

use hatchling_core::{PeerUid, SocketPath};

/// All errors producible by `hatchling-broker`.
#[derive(Debug, Error)]
pub enum Error {
    /// A required configuration value was missing from the environment.
    #[error("missing required configuration {0}; set it in the environment")]
    MissingConfig(&'static str),

    /// A configuration value was present but failed to parse.
    #[error("invalid configuration value for {key}: {reason}")]
    InvalidConfig {
        /// The configuration key that held the bad value.
        key: &'static str,
        /// Why the value was rejected.
        reason: String,
    },

    /// A broker serves this socket: it holds the lock beside it.
    #[error(
        "a broker is serving this socket (it holds {}): stop it first",
        lock.display()
    )]
    BrokerBusy {
        /// The lock the serving broker holds.
        lock: std::path::PathBuf,
    },

    /// Sweeping the broker's leftovers failed.
    #[error("sweep failed: {reason}")]
    Sweep {
        /// What failed.
        reason: String,
    },

    /// The broker was invoked with an unknown subcommand or a
    /// subcommand missing its arguments. CLI misuse fails before
    /// anything runs; usage goes to stderr at the call site.
    #[error("broker usage error: {reason}")]
    Usage {
        /// What was wrong with the invocation.
        reason: String,
    },

    /// Loading a skill failed: invalid manifest, bad signature, or
    /// capability outside the grants.
    #[error("skill load failed for {skill}: {reason}")]
    SkillLoad {
        /// The skill that failed to load.
        skill: String,
        /// Why the load was refused.
        reason: String,
    },

    /// Setting up the listen socket failed: stale-socket removal, bind,
    /// or permission setting.
    #[error("broker socket setup failed: {source}")]
    SocketSetup {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The configured socket path exists but is not a socket. The broker
    /// refuses to unlink it — removing a non-socket file it does not own
    /// could destroy someone else's data — and fails closed instead.
    #[error(
        "broker socket path {path} exists and is not a socket; remove it or set HATCHLING_BROKER_SOCKET to another path"
    )]
    SocketPathNotSocket {
        /// The offending path.
        path: SocketPath,
    },

    /// The configured socket path is a socket another user owns. The
    /// broker never unlinks it: it isn't the broker's to remove.
    #[error(
        "broker socket path {path} is a socket another user (uid {owner}) owns; stop what serves it, or set HATCHLING_BROKER_SOCKET to another path"
    )]
    SocketPathNotOurs {
        /// The offending path.
        path: SocketPath,
        /// Its owner's uid.
        owner: u32,
    },

    /// Where a socket's token goes is something other than a regular file
    /// of the broker's user. The broker never removes it.
    #[error(
        "the token file {} is not a regular file of this user's; remove it yourself if it's stale",
        path.display()
    )]
    TokenPathNotOurs {
        /// The offending path.
        path: std::path::PathBuf,
    },

    /// Accepting a connection failed.
    #[error("failed to accept a broker connection: {source}")]
    Accept {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The kernel-reported uid of the connecting peer does not match the
    /// configured agent uid. The connection is closed silently — no
    /// `REJECT` goes back: the peer gate fires before parsing, and the
    /// why stays in the broker's logs, not on the wire. Silence before
    /// parsing denies reconnaissance. Identity comes only from the
    /// kernel, never from anything the peer claims.
    #[error("peer uid mismatch: peer uid {actual}, expected agent uid {expected}")]
    PeerUnauthorized {
        /// The kernel-reported uid of the connecting peer.
        actual: PeerUid,
        /// The configured agent uid.
        expected: PeerUid,
    },

    /// I/O failed during the handshake.
    #[error("broker handshake I/O failed: {source}")]
    HandshakeIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The peer sent bytes that are not a valid handshake: bad framing,
    /// undecodable payload, or wrong protocol version. Anything ambiguous
    /// is a denial, never a guess.
    #[error("broker handshake protocol violation: {reason}")]
    HandshakeProtocol {
        /// What was wrong with the peer's bytes.
        reason: String,
    },

    /// The cell id map was invalid for this broker's privilege level, or
    /// writing the maps for the cell child failed. The cell is never
    /// spawned with a map the kernel would reject or the policy
    /// forbids.
    #[error("cell id map rejected: {source}")]
    CellMap {
        /// The isolation backend's refusal.
        source: hatchling_isolation::Error,
    },

    /// Building the cell's mount plan failed: a bind source outside the
    /// allowlist, an unsafe target, or a non-absolute new root. The
    /// cell is never spawned with an unvalidated mount surface.
    #[error("cell mount plan rejected: {source}")]
    CellMountPlan {
        /// The isolation backend's refusal.
        source: hatchling_isolation::Error,
    },

    /// A cell spawn step failed: staging setup, the fork handshake, or
    /// a child-reported setup failure. The child exits without running
    /// anything; the reason is in the message.
    #[error("cell spawn failed: {reason}")]
    CellSpawn {
        /// What went wrong.
        reason: String,
    },

    /// Forking the cell child failed.
    #[error("cell fork failed: {source}")]
    CellFork {
        /// The isolation backend's failure.
        source: hatchling_isolation::Error,
    },

    /// Waiting for the cell child failed.
    #[error("cell wait failed: {source}")]
    CellWait {
        /// The isolation backend's failure.
        source: hatchling_isolation::Error,
    },

    /// I/O failed during the post-handshake vault session. Distinct from
    /// [`HandshakeIo`](Error::HandshakeIo): the phase matters for the
    /// audit trail.
    #[error("broker session I/O failed: {source}")]
    SessionIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The peer sent a post-handshake frame that is not a valid vault
    /// op: bad framing, undecodable payload, or malformed secret
    /// encoding. The session ends; unknown traffic is never acted on.
    #[error("broker session protocol violation: {reason}")]
    SessionProtocol {
        /// What was wrong with the peer's bytes.
        reason: String,
    },

    /// The vault backend failed: the root could not be opened, or a
    /// granted op hit a filesystem failure. The op's reply is the opaque
    /// `VaultError`; the session itself survives backend failures.
    #[error("vault backend failed: {source}")]
    Vault {
        /// The underlying vault failure.
        source: hatchling_vault::Error,
    },

    /// The memory backend failed: the store could not be opened, or a
    /// granted op hit a filesystem failure. The op's reply is the opaque
    /// `MemoryError`; the session itself survives backend failures.
    #[error("memory backend failed: {source}")]
    Memory {
        /// The underlying memory failure.
        source: hatchling_memory::Error,
    },

    /// The egress proxy failed: the allowlist was invalid, or a granted
    /// op hit a proxy failure. The op's reply is the opaque
    /// `EgressError` or `Denied`; the session itself survives proxy
    /// failures.
    #[error("egress proxy failed: {source}")]
    Egress {
        /// The underlying egress failure.
        source: hatchling_egress::Error,
    },

    /// The schedule store is corrupt or an entry is over its bound
    /// . Schedule ops answer the opaque `ScheduleError`.
    #[error("schedule store failed: {reason}")]
    ScheduleStore {
        /// What failed.
        reason: String,
    },

    /// The artifact build store is corrupt or an entry is over its bound
    /// . Artifact ops answer the opaque `ArtifactError`.
    #[error("artifact store failed: {reason}")]
    ArtifactStore {
        /// What failed.
        reason: String,
    },

    /// The space store is corrupt or an entry is over its bound
    /// . Space ops answer the opaque `SpaceError`.
    #[error("space store failed: {reason}")]
    SpaceStore {
        /// What failed.
        reason: String,
    },

    /// A channel account could not be taken into custody: its
    /// secret is empty, or the RNG for its handle is unavailable. The
    /// session fails closed. The reason names the account, never its
    /// secret.
    #[error("channel account custody failed: {reason}")]
    ChannelCustody {
        /// What failed, by account name.
        reason: String,
    },

    /// The vault's access log could not be read or written.
    #[error("vault access log: {reason}")]
    VaultAccessLog {
        /// What failed.
        reason: String,
    },

    /// The output gate's records could not be read or written, or
    /// an operator action on them was refused.
    #[error("output gate records: {reason}")]
    Redaction {
        /// What failed.
        reason: String,
    },
}

/// Maps the shared contract's errors onto the broker's vocabulary.
///
/// Total and meaning-preserving: framing failures are handshake
/// failures, credential failures surface as I/O failures on the
/// credential read (the broker has no separate credential variant —
/// without credentials there is nothing to authorize against, and the
/// connection simply cannot proceed). Grant-config parse failures map to
/// the grant key: grant parsing is the only place those variants can
/// arise in the broker.
impl From<hatchling_core::Error> for Error {
    fn from(error: hatchling_core::Error) -> Self {
        match error {
            hatchling_core::Error::InvalidSocketPath(path) => Self::InvalidConfig {
                key: "HATCHLING_BROKER_SOCKET",
                reason: format!("socket path is not absolute: {path}"),
            },
            hatchling_core::Error::InvalidLogLevel(reason) => Self::InvalidConfig {
                key: "HATCHLING_LOG_LEVEL",
                reason,
            },
            hatchling_core::Error::InvalidSecretName(reason)
            | hatchling_core::Error::InvalidVaultAction(reason) => Self::InvalidConfig {
                key: "HATCHLING_VAULT_GRANTS",
                reason,
            },
            hatchling_core::Error::InvalidCellId(reason)
            | hatchling_core::Error::InvalidCapabilityName(reason)
            | hatchling_core::Error::FrameProtocol { reason } => Self::HandshakeProtocol { reason },
            hatchling_core::Error::FrameTooLarge { len, max } => Self::HandshakeProtocol {
                reason: format!("frame of {len} bytes exceeds the maximum {max}"),
            },
            hatchling_core::Error::InvalidSecretEncoding(reason) => {
                Self::SessionProtocol { reason }
            }
            hatchling_core::Error::FrameIo { source }
            | hatchling_core::Error::PeerCredentials { source } => Self::HandshakeIo { source },
        }
    }
}

/// Maps the isolation seam's errors onto the broker's cell variants
/// each step keeps the variant, and the message, it had when
/// the broker built its cells itself.
impl From<hatchling_isolation::Error> for Error {
    fn from(error: hatchling_isolation::Error) -> Self {
        match error {
            hatchling_isolation::Error::Spawn { reason } => Self::CellSpawn { reason },
            source @ hatchling_isolation::Error::Identity(_) => Self::CellMap { source },
            source @ hatchling_isolation::Error::View(_) => Self::CellMountPlan { source },
            source @ hatchling_isolation::Error::Start(_) => Self::CellFork { source },
            source @ hatchling_isolation::Error::Wait(_) => Self::CellWait { source },
        }
    }
}

#[cfg(test)]
mod tests {
    use hatchling_isolation::PlatformError;

    use super::*;

    fn platform(message: &str) -> PlatformError {
        PlatformError::new(std::io::Error::other(message.to_owned()))
    }

    /// each of these says what to do about it, in the audit's words.
    #[test]
    fn the_polished_messages_say_what_to_do() {
        let path: SocketPath = "/run/hatchling/broker.sock".parse().unwrap();
        assert_eq!(
            Error::SocketPathNotSocket { path }.to_string(),
            "broker socket path /run/hatchling/broker.sock exists and is not a socket; remove it or set HATCHLING_BROKER_SOCKET to another path"
        );
        assert_eq!(
            Error::MissingConfig("HATCHLING_VAULT_DIR").to_string(),
            "missing required configuration HATCHLING_VAULT_DIR; set it in the environment"
        );
    }

    #[test]
    fn each_isolation_step_keeps_its_cell_variant_and_message() {
        let cases = [
            (
                hatchling_isolation::Error::Identity(platform("bad map")),
                "cell id map rejected: bad map",
            ),
            (
                hatchling_isolation::Error::View(platform("bad bind")),
                "cell mount plan rejected: bad bind",
            ),
            (
                hatchling_isolation::Error::Start(platform("no fork")),
                "cell fork failed: no fork",
            ),
            (
                hatchling_isolation::Error::Wait(platform("no wait")),
                "cell wait failed: no wait",
            ),
            (
                hatchling_isolation::Error::Spawn {
                    reason: String::from("no staging"),
                },
                "cell spawn failed: no staging",
            ),
        ];
        for (error, message) in cases {
            let error = Error::from(error);
            assert_eq!(error.to_string(), message);
            let variant_matches = match message.split(':').next() {
                Some("cell id map rejected") => matches!(error, Error::CellMap { .. }),
                Some("cell mount plan rejected") => matches!(error, Error::CellMountPlan { .. }),
                Some("cell fork failed") => matches!(error, Error::CellFork { .. }),
                Some("cell wait failed") => matches!(error, Error::CellWait { .. }),
                _ => matches!(error, Error::CellSpawn { .. }),
            };
            assert!(variant_matches, "{message}");
        }
    }
}
