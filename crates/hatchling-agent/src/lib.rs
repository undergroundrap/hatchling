//! `hatchling-agent`: the binary that runs inside a cell.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Every piece of agent logic lives in this library so it can be tested
//! without spawning the binary. `main.rs` is a thin edge: parse config,
//! install logging, run the cell lifecycle, and translate the outcome to a
//! process exit code.
//!
//! The shared contract (identity types, framing, messages, kernel peer
//! credentials) lives in [`hatchling_core`]; the names are re-exported
//! here so the agent's public API is unchanged by the extraction.

pub mod cell;
pub mod channel;
pub mod cli;
pub mod config;
pub mod egress;
pub mod error;
pub mod goals;
pub mod handshake;
pub mod memory;
pub mod memory_provider;
pub mod model;
pub mod research;
pub mod skill;
pub mod subagent;
#[cfg(test)]
mod test_socket;
pub mod tools;
pub mod vault;

pub use cell::{BrokerConnected, Cell, Configured, PeerAuthorized, PreconditionsChecked, Ready};
pub use channel::ChannelClient;
pub use config::Config;
pub use error::Error;
pub use hatchling_core::{
    AgentMessage, BrokerMessage, CapabilityName, CellId, LogLevel, MAX_FRAME_BYTES,
    MAX_SECRET_BYTES, PROTOCOL_VERSION, PeerCredentials, PeerPid, PeerUid, SecretName, SocketPath,
    VaultAction, VaultOp, VaultResult, peer_credentials,
};
pub use hatchling_loop::PromptKind;
pub use model::{EgressTransport, MODEL_SURROGATE_TTL_SECS, run_command};
