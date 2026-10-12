//! `hatchling-loop`: the agent tool-calling loop.
//!
//! Part of [Hatchling](https://github.com/undergroundrap/hatchling), published so
//! [`hatchling-runtime`](https://crates.io/crates/hatchling-runtime) can be
//! installed. It carries no stability promise on its own.
//!
//! Licensed AGPL-3.0-or-later, with the additional terms in
//! [NOTICE.md](https://github.com/undergroundrap/hatchling/blob/main/NOTICE.md).
//!
//! The model proposes; the host validates and bounds. Three things
//! stay separate because they change for different reasons:
//!
//! - [`runner::Loop`] — control flow: iterate, bound, fail closed.
//! - [`model::Model`] — where the next action comes from. A trait,
//!   not a provider SDK; v0.1 ships [`fake::FakeModel`] for tests.
//! - [`tool::Tool`] — what actions exist. A fixed
//!   [`tool::ToolRegistry`] the host builds; the model only names
//!   tools and supplies arguments, validated against each tool's
//!   JSON Schema before execution.

pub mod error;
pub mod fake;
pub mod ledger;
pub mod memory;
pub mod model;
pub mod runner;
pub mod tool;

pub use error::Error;
pub use fake::FakeModel;
pub use hatchling_prompt::PromptKind;
pub use ledger::StepLedger;
pub use memory::{MemoryProvider, RecallError};
pub use model::{Message, Model, ModelAction, ToolResultView};
pub use runner::{Loop, LoopBudget, LoopResult};
pub use tool::{Tool, ToolCall, ToolDescription, ToolOutput, ToolRegistry};
