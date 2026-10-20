//! `hatchling-egress`: the egress proxy.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! The only component that touches the network on the agent's behalf.
//! The agent never sees the network and never holds a real credential:
//! it presents a surrogate token ([`surrogate`]), the proxy validates
//! the surrogate and the destination against the [`allowlist`], swaps
//! the surrogate for the real credential, forwards the request, and
//! returns the bounded response.
//!
//! The proxy is a library, not a daemon. Only the broker drives it,
//! over the peer-authorized session (/). Surrogates are
//! minted by the broker for kernel-authorized peers with an explicit
//! grant; the HMAC key is generated fresh at startup and never leaves
//! the broker's memory.
//!
//! Design answers the surrogate-swap flaw class: a flawed egress path
//! swaps bearer tokens correctly, but trusts surrogates minted by an
//! unauthorized credential broker. Hatchling binds
//! surrogates to (`peer_uid`, `destination`, `expiry`) with an HMAC the
//! agent cannot forge, and the peer gate fires before any request byte
//! is parsed.

pub mod allowlist;
pub mod error;
pub mod https;
pub mod local;
pub mod policy;
pub mod proxy;
pub mod relay;
pub mod scrub;
pub mod search;
pub mod surrogate;
pub mod web;

pub use allowlist::{
    Allowlist, INTERNAL_PREFIX, LOCAL_HOST, LOCAL_KEYLESS_PREFIX, LOCAL_PREFIX, Local,
    PUBLIC_PREFIX, is_internal, is_pinned_loopback,
};
pub use error::Error;
pub use https::TlsClient;
pub use proxy::{EgressRequest, EgressResponse, MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, Proxy};
pub use surrogate::{SurrogateKey, ValidatedSurrogate, mint_surrogate, validate_surrogate};

/// Convenience alias: the proxy's fallible return type.
pub type Result<T> = std::result::Result<T, Error>;
