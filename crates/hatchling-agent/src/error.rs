//! Typed errors for `hatchling-agent`.
//!
//! Every failure the agent can report is a variant here. Callers match
//! exhaustively: adding a variant forces each decision site to be revisited.
//! There are no stringly errors and no boxed dynamic errors.

use thiserror::Error;

use crate::{PeerUid, SocketPath};

/// All errors producible by `hatchling-agent`.
#[derive(Debug, Error)]
pub enum Error {
    /// A cell id failed validation. The enclosed string says why.
    #[error("invalid cell id: {0}")]
    InvalidCellId(String),

    /// A socket path failed validation (it must be absolute).
    #[error("invalid socket path (must be absolute): {0}")]
    InvalidSocketPath(String),

    /// A capability name failed validation. The enclosed string says why.
    #[error("invalid capability name: {0}")]
    InvalidCapabilityName(String),

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

    /// A startup precondition could not be established. The agent refuses
    /// to run rather than degrade to an unisolated state.
    #[error("startup precondition failed: {0}")]
    PreconditionFailed(String),

    /// Connecting to the broker socket failed. The agent refuses to run
    /// without its broker; there is no degraded mode and no retry.
    #[error(
        "failed to connect to broker socket {socket}: {source}; is the broker running and HATCHLING_BROKER_SOCKET correct?"
    )]
    BrokerConnect {
        /// The socket path that was attempted.
        socket: SocketPath,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The connection the cell was given can't be used. The agent refuses
    /// to run without its broker; there is no degraded mode.
    #[error("the broker connection at descriptor {fd} is unusable: {reason}")]
    BrokerConnection {
        /// The inherited descriptor (`HATCHLING_CONNECTION_FD`).
        fd: i32,
        /// Why it can't be used.
        reason: String,
    },

    /// Reading the broker's kernel peer credentials failed. Without
    /// credentials there is nothing to authorize against, so this is
    /// fail-closed.
    #[error("failed to read broker peer credentials: {source}")]
    PeerCredentials {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The kernel-reported uid of the broker socket peer does not match
    /// the configured broker uid. The agent would be talking to an
    /// impostor, so this is a hard fail-closed error. Identity comes
    /// only from the kernel, never from anything the peer claims.
    #[error("broker uid mismatch: peer uid {actual}, expected {expected}")]
    BrokerUnauthorized {
        /// The broker uid from configuration.
        expected: PeerUid,
        /// The kernel-reported uid of the connected peer.
        actual: PeerUid,
    },

    /// I/O failed during the broker handshake.
    #[error("broker handshake I/O failed: {source}")]
    HandshakeIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The broker explicitly rejected the handshake.
    #[error("broker rejected handshake: {reason}")]
    HandshakeRejected {
        /// The reason the broker gave.
        reason: String,
    },

    /// The broker sent bytes that are not a valid handshake: bad
    /// framing, undecodable payload, or wrong protocol version.
    /// Anything ambiguous is a denial, never a guess.
    #[error("broker handshake protocol violation: {reason}")]
    HandshakeProtocol {
        /// What was wrong with the peer's bytes.
        reason: String,
    },

    /// A secret name failed client-side validation. The broker would
    /// deny it; no round trip is spent. (The broker re-validates: the
    /// agent's bytes are never trusted.)
    #[error("invalid secret name: {0}")]
    InvalidSecretName(String),

    /// The secret exceeds the wire limit. Checked client-side so an
    /// oversize value never becomes a doomed frame.
    #[error("secret of {len} bytes exceeds the {max}-byte wire limit")]
    SecretTooLarge {
        /// The rejected size, in bytes.
        len: usize,
        /// The wire limit, in bytes.
        max: usize,
    },

    /// I/O failed during a vault operation.
    #[error("vault operation I/O failed: {source}")]
    VaultIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The broker's reply to a vault op was not a well-formed
    /// [`VaultResult`](crate::VaultResult): bad framing, undecodable
    /// payload, or an unexpected result for the op. Anything ambiguous
    /// is fail-closed.
    #[error("vault protocol violation: {reason}")]
    VaultProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// The broker holds no grant for this (action, name) pair. The op
    /// never reached the vault.
    #[error("vault op denied: no grant for {action} on {name:?}")]
    VaultDenied {
        /// The denied action.
        action: String,
        /// The denied secret name.
        name: String,
    },

    /// The broker reported an internal vault failure for the op.
    /// Deliberately opaque: the agent learns that the op failed, not why.
    #[error("vault backend failed")]
    VaultFailed,

    /// A memory frame violated the protocol: bad framing, undecodable
    /// payload, or an unexpected result for the op. Anything ambiguous
    /// is fail-closed.
    #[error("memory protocol violation: {reason}")]
    MemoryProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// The broker holds no grant for this (scope, action) pair. The op
    /// never reached the store.
    #[error("memory op denied: no grant for {action} on {scope:?}")]
    MemoryDenied {
        /// The denied action.
        action: String,
        /// The denied scope.
        scope: String,
    },

    /// The broker reported an internal memory failure for the op.
    /// Deliberately opaque: the agent learns that the op failed, not why.
    #[error("memory backend failed")]
    MemoryFailed,

    /// I/O failed during a memory operation.
    #[error("memory operation I/O failed: {source}")]
    MemoryIo {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The memory op's request would exceed the protocol frame bound.
    /// Refused before sending: nothing reached the broker and the
    /// session is intact.
    #[error("memory request too large: {len} bytes, max {max}")]
    MemoryRequestTooLarge {
        /// The encoded request's length.
        len: usize,
        /// The frame bound.
        max: usize,
    },

    /// A goals or query frame violated the protocol: bad framing, an
    /// undecodable payload, or a reply that does not match the op.
    /// Anything ambiguous is fail-closed.
    #[error("goals protocol violation: {reason}")]
    GoalsProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// The broker holds no grant for this goals op or query. The op
    /// never reached the store.
    #[error("goals op denied: no grant for {action}")]
    GoalsDenied {
        /// The denied action.
        action: String,
    },

    /// The broker refused the op's content: an empty or oversize
    /// field, a control character, or an update that changes nothing.
    #[error("goals op rejected: {action} content was refused")]
    GoalsRejected {
        /// The rejected action.
        action: String,
    },

    /// The user is at a goals-store bound (goals held, or tracking
    /// entries).
    #[error("goals store limit reached; remove old goals or tracking entries to free space")]
    GoalsLimitReached,

    /// The broker reported an internal goals-store failure.
    /// Deliberately opaque: the agent learns that the op failed, not why.
    #[error("goals backend failed")]
    GoalsFailed,

    /// I/O failed during a goals operation or query.
    #[error("goals operation I/O failed: {source}")]
    GoalsIo {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// I/O failed during an egress operation.
    #[error("egress operation I/O failed: {source}")]
    EgressIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The broker's reply to an egress op was not a well-formed
    /// [`EgressResult`](hatchling_core::EgressResult): bad framing,
    /// undecodable payload, or an unexpected result for the op. Anything
    /// ambiguous is fail-closed.
    #[error("egress protocol violation: {reason}")]
    EgressProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// The broker holds no grant for this destination, the destination
    /// is not on the allowlist, or the surrogate is invalid. The op
    /// never reached the network.
    #[error("egress op denied: no grant for {destination:?}")]
    EgressDenied {
        /// The denied destination.
        destination: String,
    },

    /// The broker reported an internal egress failure for the op.
    /// Deliberately opaque: the agent learns that the op failed, not why.
    #[error("egress backend failed")]
    EgressFailed,

    /// I/O failed during a channel operation.
    #[error("channel operation I/O failed: {source}")]
    ChannelIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The broker's reply to a channel op was not a well-formed
    /// [`ChannelResult`](hatchling_core::ChannelResult): bad framing,
    /// undecodable payload, or an unexpected result for the op. Anything
    /// ambiguous is fail-closed.
    #[error("channel protocol violation: {reason}")]
    ChannelProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// The broker denied the channel op: unknown channel, or a
    /// state-changing send without confirmation-rail approval. The op
    /// never reached the channel.
    #[error("channel op denied on {channel:?}")]
    ChannelDenied {
        /// The denied channel.
        channel: String,
    },

    /// The broker reported a channel backend failure for the op.
    /// Deliberately opaque: the agent learns that the op failed, not why.
    #[error("channel backend failed on {channel:?}")]
    ChannelBackend {
        /// The failed channel.
        channel: String,
    },

    /// The broker knows no such account on the channel: the handle was
    /// not one this session's broker minted for it, or a credentialed
    /// channel was used without one. The op never reached the
    /// channel.
    #[error("unknown account on channel {channel:?}")]
    ChannelUnknownAccount {
        /// The channel the op named.
        channel: String,
    },

    /// The broker's output gate redacted the message: the user
    /// saw a notice in its place. Sending it again, reworded or in parts,
    /// is redacted too.
    #[error("the output gate redacted the message on {channel:?} ({category}, event {event_id})")]
    ChannelRedacted {
        /// The channel the message was sent on.
        channel: String,
        /// The category it was redacted under.
        category: String,
        /// The redaction's event id, for the operator's review.
        event_id: String,
    },

    /// I/O failed during a web operation (WO-028).
    #[error("web operation I/O failed: {source}")]
    WebIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The broker's reply to a web op was not a well-formed
    /// [`WebResult`](hatchling_core::WebResult). Fail-closed.
    #[error("web protocol violation: {reason}")]
    WebProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// I/O failed during an idea operation (WO-032).
    #[error("idea operation I/O failed: {source}")]
    IdeaIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The broker's reply to an idea op was not a well-formed
    /// [`ArtifactResult`](hatchling_core::ArtifactResult). Fail-closed.
    #[error("idea protocol violation: {reason}")]
    IdeaProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// I/O failed during a browser operation (WO-029).
    #[error("browser operation I/O failed: {source}")]
    BrowserIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The broker's reply to a browser op was not a well-formed
    /// [`BrowserResult`](hatchling_core::BrowserResult). Fail-closed.
    #[error("browser protocol violation: {reason}")]
    BrowserProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// I/O failed during a skill operation.
    #[error("skill operation I/O failed: {source}")]
    SkillIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// The broker's reply to a skill op was not a well-formed
    /// [`SkillResult`](hatchling_core::SkillResult): bad framing,
    /// undecodable payload, or a reply of another kind. Fail-closed.
    #[error("skill protocol violation: {reason}")]
    SkillProtocol {
        /// What was wrong with the broker's bytes.
        reason: String,
    },

    /// No skill is registered under the exact id.
    #[error("skill {skill:?} is not registered")]
    SkillUnknown {
        /// The requested id.
        skill: String,
    },

    /// The skill did not declare the capability. The broker checks the
    /// declaration on every call.
    #[error("skill {skill:?} did not declare capability {capability:?}")]
    SkillDenied {
        /// The skill id.
        skill: String,
        /// The undeclared capability.
        capability: String,
    },

    /// The skill could not run, or failed. Deliberately opaque: the
    /// reason is in the broker's log, not here.
    #[error("skill {skill:?} failed")]
    SkillFailed {
        /// The skill id.
        skill: String,
    },

    /// The request is too large to send. Refused before any I/O.
    #[error("skill request is {len} bytes, over the {max}-byte limit")]
    SkillInputTooLarge {
        /// The offending size in bytes.
        len: usize,
        /// The limit it exceeds.
        max: usize,
    },

    /// Command-line usage error (unknown subcommand, missing task).
    /// The process exits non-zero; nothing ran.
    #[error("usage error: {0}")]
    Usage(String),

    /// The agent loop failed: budget exhausted, model failure, or an
    /// unrecoverable tool failure. The enclosed string is the loop's
    /// own report, preserved verbatim.
    #[error("agent loop failed: {0}")]
    LoopFailed(String),

    /// The loop used its whole step budget without an answer.
    #[error("agent loop failed: loop budget exhausted after {steps} steps")]
    LoopBudgetExhausted {
        /// The model turns it took.
        steps: u32,
    },

    /// Subagent op transport failed.
    #[error("subagent operation I/O failed: {source}")]
    SubagentIo {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The broker's reply to a subagent op was not a subagent reply.
    #[error("subagent protocol violation: {reason}")]
    SubagentProtocol {
        /// What was wrong.
        reason: String,
    },

    /// The model endpoint failed the capability handshake run before
    /// every task: unreachable, or a model that does not call
    /// tools. The task never started.
    #[error("the model endpoint failed its capability handshake: {0}")]
    ModelRefused(String),
}

/// Maps the shared contract's errors onto the agent's vocabulary.
///
/// The mapping is total and meaning-preserving: framing failures are
/// handshake failures, credential failures are credential failures, and
/// identifier validation keeps its shape. Nothing is flattened into a
/// string.
impl From<hatchling_core::Error> for Error {
    fn from(error: hatchling_core::Error) -> Self {
        match error {
            hatchling_core::Error::InvalidCellId(reason) => Self::InvalidCellId(reason),
            hatchling_core::Error::InvalidSocketPath(path) => Self::InvalidSocketPath(path),
            hatchling_core::Error::InvalidCapabilityName(reason) => {
                Self::InvalidCapabilityName(reason)
            }
            hatchling_core::Error::InvalidSecretName(reason) => Self::InvalidSecretName(reason),
            hatchling_core::Error::InvalidVaultAction(reason)
            | hatchling_core::Error::InvalidSecretEncoding(reason) => {
                Self::VaultProtocol { reason }
            }
            hatchling_core::Error::InvalidLogLevel(reason) => Self::InvalidConfig {
                key: "HATCHLING_LOG_LEVEL",
                reason,
            },
            hatchling_core::Error::FrameIo { source } => Self::HandshakeIo { source },
            hatchling_core::Error::FrameProtocol { reason } => Self::HandshakeProtocol { reason },
            hatchling_core::Error::FrameTooLarge { len, max } => Self::HandshakeProtocol {
                reason: format!("frame of {len} bytes exceeds the maximum {max}"),
            },
            hatchling_core::Error::PeerCredentials { source } => Self::PeerCredentials { source },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// each of these says what to do about it, in the audit's words.
    #[test]
    fn the_polished_messages_say_what_to_do() {
        let socket: SocketPath = "/run/hatchling/broker.sock".parse().unwrap();
        for (error, says) in [
            (
                Error::SecretTooLarge {
                    len: 70_000,
                    max: 65_536,
                },
                "secret of 70000 bytes exceeds the 65536-byte wire limit",
            ),
            (
                Error::BrokerConnect {
                    socket,
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                },
                "; is the broker running and HATCHLING_BROKER_SOCKET correct?",
            ),
            (
                Error::MissingConfig("HATCHLING_AGENT_UID"),
                "missing required configuration HATCHLING_AGENT_UID; set it in the environment",
            ),
            (
                Error::GoalsLimitReached,
                "goals store limit reached; remove old goals or tracking entries to free space",
            ),
        ] {
            assert!(error.to_string().contains(says), "{error}");
        }
    }
}
