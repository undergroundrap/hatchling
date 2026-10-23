//! The path a session's egress takes (WO-017, step 2): [`EgressPath`]
//! mints the cell's surrogates and forwards its requests with the
//! destination's credential put in.
//!
//! A broker serving with an egress authority gives every session its link
//! to the authority ([`crate::egress_authority::EgressLink`]): the
//! authority holds the session's surrogate key and reads each credential
//! from the vault keeper, and the broker holds neither. In-process, with
//! no authority running (as with no keeper), a session's path is a proxy
//! of the broker's own ([`hatchling_egress::Proxy`]).

use hatchling_egress::{EgressRequest, EgressResponse, Proxy};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// What kind of refusal a request met: what the operator is shown, and
/// whether the agent is told it was denied or that it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefusalKind {
    /// The destination isn't on the allowlist.
    NotAllowed,
    /// The model request policy refused the request.
    Policy,
    /// The surrogate isn't valid, has expired, or isn't the peer's or the
    /// destination's.
    BadSurrogate,
    /// Anything else: the request was allowed, and failed.
    Failed,
}

/// A refused or failed egress request, and why, for the broker's log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refusal {
    /// What kind.
    pub kind: RefusalKind,
    /// Why, in words for the operator's log, never the agent's.
    pub reason: String,
}

impl Refusal {
    /// The refusal a proxy's `error` is.
    #[must_use]
    pub fn of(error: &hatchling_egress::Error) -> Self {
        use hatchling_egress::Error as Egress;
        let kind = match error {
            Egress::DestinationDenied => RefusalKind::NotAllowed,
            Egress::PolicyRefused { .. } => RefusalKind::Policy,
            Egress::InvalidSurrogate
            | Egress::ExpiredSurrogate
            | Egress::PeerMismatch
            | Egress::DestinationMismatch => RefusalKind::BadSurrogate,
            Egress::UpstreamFailed
            | Egress::ResponseTooLarge
            | Egress::Io { .. }
            | Egress::InvalidAllowlist { .. }
            | Egress::InvalidTrustBundle { .. }
            | Egress::Random { .. } => RefusalKind::Failed,
        };
        Self {
            kind,
            reason: error.to_string(),
        }
    }

    /// A request that failed for `reason`.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            kind: RefusalKind::Failed,
            reason: reason.into(),
        }
    }
}

/// Where a session's egress goes: the authority, or a proxy of the
/// broker's own.
pub trait EgressPath {
    /// Mints a surrogate for `peer_uid` to reach `destination`, living
    /// `ttl_secs`.
    ///
    /// # Errors
    ///
    /// Why it wasn't minted.
    fn mint(&self, peer_uid: u32, destination: &str, ttl_secs: u64) -> Result<String, Refusal>;

    /// Forwards `request`, presented by `peer_uid`, with its credential
    /// put in, and returns the bounded answer.
    ///
    /// # Errors
    ///
    /// Why it was refused, or failed.
    fn forward(&self, peer_uid: u32, request: &EgressRequest) -> Result<EgressResponse, Refusal>;
}

impl<F> EgressPath for Proxy<F>
where
    F: Fn(&str) -> Option<Zeroizing<Vec<u8>>>,
{
    fn mint(&self, peer_uid: u32, destination: &str, ttl_secs: u64) -> Result<String, Refusal> {
        self.mint_surrogate(peer_uid, destination, ttl_secs)
            .map_err(|error| Refusal::of(&error))
    }

    fn forward(&self, peer_uid: u32, request: &EgressRequest) -> Result<EgressResponse, Refusal> {
        Self::forward(self, peer_uid, request).map_err(|error| Refusal::of(&error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_proxy_error_is_its_refusal() {
        use hatchling_egress::Error as Egress;
        for (error, kind) in [
            (Egress::DestinationDenied, RefusalKind::NotAllowed),
            (
                Egress::PolicyRefused {
                    reason: "a model request may not use a tool the vendor runs",
                },
                RefusalKind::Policy,
            ),
            (Egress::InvalidSurrogate, RefusalKind::BadSurrogate),
            (Egress::ExpiredSurrogate, RefusalKind::BadSurrogate),
            (Egress::PeerMismatch, RefusalKind::BadSurrogate),
            (Egress::DestinationMismatch, RefusalKind::BadSurrogate),
            (Egress::UpstreamFailed, RefusalKind::Failed),
            (Egress::ResponseTooLarge, RefusalKind::Failed),
            (
                Egress::Io {
                    source: std::io::Error::other("down"),
                },
                RefusalKind::Failed,
            ),
            (
                Egress::InvalidAllowlist {
                    reason: String::new(),
                },
                RefusalKind::Failed,
            ),
        ] {
            let refusal = Refusal::of(&error);
            assert_eq!(refusal.kind, kind);
            assert_eq!(refusal.reason, error.to_string());
        }
        assert_eq!(
            Refusal::failed("gone"),
            Refusal {
                kind: RefusalKind::Failed,
                reason: String::from("gone")
            }
        );
    }

    /// A proxy is a path: its mint and forward, refused as their errors
    /// are.
    #[test]
    fn a_proxy_is_a_path() {
        let proxy = Proxy::new("internal:localhost:443", |_: &str| None).unwrap();
        let path: &dyn EgressPath = &proxy;
        assert_eq!(
            path.mint(1000, "elsewhere.example:443", 60),
            Err(Refusal::of(&hatchling_egress::Error::DestinationDenied))
        );
        let surrogate = path.mint(1000, "localhost:443", 60).unwrap();
        let request = EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: Vec::new(),
            body: Vec::new(),
            surrogate_b64: surrogate,
        };
        assert_eq!(
            path.forward(1001, &request).map(|answer| answer.status),
            Err(Refusal::of(&hatchling_egress::Error::PeerMismatch))
        );
    }
}
