//! `hatchling-model`: a real model provider for the agent loop.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! The loop (`hatchling-loop`) is LLM-agnostic: a [`Model`][loop_model]
//! is anything that turns history plus tools into the next action.
//! This crate is the first real implementation of that seam — a
//! translation layer for the OpenAI-compatible chat-completions
//! dialect, which one client speaks to both local servers (Ollama,
//! llama.cpp) and cloud endpoints.
//!
//! A local endpoint (ollama, LM Studio, vLLM, llama.cpp; ) is
//! configuration, not code: `http://localhost:port`, behind the same
//! trait, the same redaction, and the same capability handshake as a
//! cloud one. "Local" is not "trusted": secrets are redacted
//! ([`Redactor`]) before any request reaches the transport, whatever
//! the endpoint, and a model that cannot call tools is refused before
//! a task ([`ChatCompletionsProvider::verify_capabilities`]).
//!
//! The crate never opens a socket. HTTP goes through the injected
//! [`Transport`], which the host implements over the broker-mediated
//! egress path — so the cell still never touches the network, and the
//! model credential still travels as a broker-minted surrogate the
//! proxy swaps. Every ambiguous provider response fails closed as
//! [`hatchling_loop::Error::ModelFailed`].
//!
//! [loop_model]: hatchling_loop::Model

pub mod endpoint;
pub mod error;
pub mod provider;
pub mod redact;
pub mod transport;

pub use endpoint::{Endpoint, LOCAL_HOST};
pub use error::Error;
pub use provider::{
    ChatCompletionsProvider, MIN_CONTEXT_WINDOW, ProviderConfig, check_context_window,
};
pub use redact::{REDACTED, Redactor};
pub use transport::{Transport, TransportError};
