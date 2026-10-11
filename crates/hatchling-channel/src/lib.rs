//! Messaging channels: the primary user interface.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! The agent loop takes a single task string; channels give it a way to
//! receive user messages and send replies. The [`Channel`] trait is the
//! abstraction; the broker owns the concrete endpoints, and the cell-side agent reaches them
//! only through the peer-authorized broker session.
//!
//! Identity is broker-asserted: a [`Message`] carries sender identity,
//! channel identity, and text, and all three are stamped by the broker.
//! The cell side never self-declares who sent a message.
//!
//! Custody is broker-side too: a channel that authenticates to
//! its backend is lent the account's [`Credential`] per call by the
//! broker, and the cell names accounts only by opaque handles.

mod credential;
mod error;
pub mod gate;
mod hub;
mod local;
mod types;

pub use credential::Credential;
pub use error::Error;
pub use gate::{
    Audit, Category, Classifier, Cleared, Event, Failure, NoAudit, NoOverrides, OutputGate,
    Overrides, Reason, Request, Verdict, normalize, notice,
};
pub use hub::{IngressEvent, LocalHub, MAX_INGRESS_TEXT_BYTES, MAX_QUEUED_MESSAGES, Sink, SinkId};
pub use local::LocalChannel;
pub use types::{Channel, ChannelId, Message, SenderId};
