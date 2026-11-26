//! Typed errors for the shared contract.
//!
//! These are the failures the protocol itself can produce: malformed
//! identifiers, framing violations, and missing kernel credentials. Each
//! binary maps them into its own richer [`Error`](crate::Error)-style
//! type at its edge; the contract never invents binary-specific meaning.

use thiserror::Error;

/// All errors producible by `hatchling-core`.
#[derive(Debug, Error)]
pub enum Error {
    /// A cell id failed validation. The enclosed string says why.
    #[error("invalid cell id: {0}")]
    InvalidCellId(String),

    /// A socket path failed validation (it must be absolute, with no `..`).
    #[error("invalid socket path (must be absolute, with no ..): {0}")]
    InvalidSocketPath(String),

    /// A capability name failed validation. The enclosed string says why.
    #[error("invalid capability name: {0}")]
    InvalidCapabilityName(String),

    /// A secret name failed validation. The enclosed string says why.
    #[error("invalid secret name: {0}")]
    InvalidSecretName(String),

    /// A vault action failed to parse. The enclosed string says why.
    #[error("invalid vault action: {0}")]
    InvalidVaultAction(String),

    /// A secret payload was not valid base64. The enclosed string says why.
    #[error("invalid secret encoding: {0}")]
    InvalidSecretEncoding(String),

    /// A log level failed to parse. The enclosed string says why.
    #[error("invalid log level: {0}")]
    InvalidLogLevel(String),

    /// I/O failed while reading or writing a protocol frame.
    #[error("frame I/O failed: {source}")]
    FrameIo {
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// A frame violated the protocol: oversize length, bad encoding, or
    /// a message that is not part of the handshake.
    #[error("frame protocol violation: {reason}")]
    FrameProtocol {
        /// What was wrong with the frame.
        reason: String,
    },

    /// A frame to be sent exceeds [`MAX_FRAME_BYTES`](crate::MAX_FRAME_BYTES).
    /// Refused before any byte is written: the stream is untouched and
    /// the session stays usable, so the caller can fail just this op.
    #[error("frame of {len} bytes exceeds the maximum {max}; nothing was sent")]
    FrameTooLarge {
        /// The payload's length.
        len: usize,
        /// The bound.
        max: usize,
    },

    /// Reading the peer's kernel credentials failed. Without credentials
    /// there is nothing to authorize against, so the caller must fail
    /// closed.
    #[error("failed to read peer credentials: {source}")]
    PeerCredentials {
        /// The underlying I/O failure.
        source: std::io::Error,
    },
}
