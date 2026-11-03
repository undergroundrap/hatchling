//! Typed errors for the agent loop.
//!
//! Two failure classes are distinguished by design: tool failures are
//! information the model can act on (they feed back as
//! `ToolResult::Err`), while loop-control failures (unknown tool,
//! invalid arguments, budget exhausted) stop the loop fail-closed.

use thiserror::Error;

/// Errors from the tool-calling loop.
#[derive(Debug, Error)]
pub enum Error {
    /// The model named a tool that is not in the registry. Nothing
    /// was executed.
    #[error("unknown tool: {0}")]
    UnknownTool(String),

    /// The model's arguments failed validation against the tool's
    /// JSON Schema. Nothing was executed.
    #[error("invalid arguments for tool {tool}: {reason}")]
    InvalidArguments {
        /// The tool that was named.
        tool: String,
        /// Why validation failed.
        reason: String,
    },

    /// The loop exceeded its step budget. Fail closed.
    #[error("loop budget exhausted after {steps} steps")]
    BudgetExhausted {
        /// How many steps ran before the bound fired.
        steps: u32,
    },

    /// The model itself failed (transport, provider error, malformed
    /// response). The loop cannot continue without a next action.
    #[error("model failed: {0}")]
    ModelFailed(String),

    /// The model's reply was cut off at its limit: out of output tokens
    /// or context window. The loop cannot continue without a whole
    /// next action.
    #[error("model failed: {0}")]
    ModelCutOff(String),

    /// A tool's execution failed. This normally feeds back to the
    /// model as `ToolResult::Err`; it surfaces as `Error` only when
    /// the failure happens outside the feedback path.
    #[error("tool {tool} failed: {reason}")]
    ToolFailed {
        /// The tool that failed.
        tool: String,
        /// Why it failed.
        reason: String,
    },

    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
