//! Typed errors for the egress proxy.
//!
//! Every variant is fail-closed: the proxy never forwards a request it
//! cannot fully validate, and failures are opaque to the agent — the
//! agent learns that the request was denied or failed, not why.

use thiserror::Error;

/// An egress failure.
#[derive(Debug, Error)]
pub enum Error {
    /// The surrogate is malformed, has an invalid signature, or was not
    /// minted by this proxy instance.
    #[error("invalid surrogate")]
    InvalidSurrogate,
    /// The surrogate has expired.
    #[error("surrogate expired")]
    ExpiredSurrogate,
    /// The surrogate was minted for a different peer uid than the one
    /// presenting it.
    #[error("surrogate peer mismatch")]
    PeerMismatch,
    /// The surrogate was minted for a different destination than the
    /// one requested.
    #[error("surrogate destination mismatch")]
    DestinationMismatch,
    /// The destination is not on the allowlist.
    #[error("destination denied")]
    DestinationDenied,
    /// The model request policy refused the request: it would have a
    /// model vendor fetch on the cell's behalf (see [`crate::policy`]).
    #[error("refused by the model request policy: {reason}")]
    PolicyRefused {
        /// Which rule refused it.
        reason: crate::policy::Refusal,
    },
    /// The allowlist configuration is invalid.
    #[error("invalid allowlist: {reason}")]
    InvalidAllowlist {
        /// Why the allowlist entry was rejected.
        reason: String,
    },
    /// The operator's CA bundle cannot be used.
    #[error("invalid trust bundle: {reason}")]
    InvalidTrustBundle {
        /// Why the bundle was refused.
        reason: String,
    },
    /// The operating system would not provide randomness, so there is
    /// no surrogate key and the proxy cannot start.
    #[error("OS randomness unavailable: {source}")]
    Random {
        /// The underlying RNG failure.
        source: getrandom::Error,
    },
    /// The upstream request failed.
    #[error("upstream request failed")]
    UpstreamFailed,
    /// The upstream response exceeded the size bound.
    #[error("upstream response too large")]
    ResponseTooLarge,
    /// An I/O error occurred.
    #[error("egress I/O error")]
    Io {
        /// The underlying I/O error.
        #[from]
        source: std::io::Error,
    },
}
