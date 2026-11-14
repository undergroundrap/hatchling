//! The LLM-agnostic `Model` trait and the conversation history.
//!
//! The loop never imports a provider SDK. A model is anything that
//! can look at the history and the available tools and produce the
//! next action: either tool calls or a final text answer.

use serde_json::Value;

use crate::Error;
use crate::tool::{ToolCall, ToolDescription, ToolOutput};

/// One turn of the conversation history the loop accumulates.
#[derive(Debug, Clone)]
pub enum Message {
    /// Host instructions. Always first; the model cannot override it.
    System(String),
    /// The original user request.
    User(String),
    /// The model's text output on an earlier turn.
    Assistant(String),
    /// The model's tool calls and their results, in order.
    ToolUse {
        /// The calls the model made.
        calls: Vec<ToolCall>,
        /// Their results, positionally aligned with `calls`.
        results: Vec<ToolOutput>,
    },
    /// Recalled memory items, rendered as inert data.
    ///
    /// Positioned below [`Message::System`] and [`Message::User`]
    /// and above [`Message::ToolUse`]: memory is recalled data, not
    /// live instructions. The content is labeled as recalled and
    /// must never outrank the system prompt or the user task.
    RecalledMemory(String),
    /// A gathered source, already wrapped in its
    /// untrusted-data envelope ([`hatchling_prompt::wrap_source`]).
    ///
    /// Positioned like [`Message::RecalledMemory`]: below the system
    /// prompt and the user request, never above them. A source is
    /// evidence to weigh, not instructions.
    Evidence(String),
    /// The loop's own word to the model in the middle of a run: why the
    /// answer it just gave is held back, and what to settle first
    /// ([`Loop::with_unfinished`](crate::Loop::with_unfinished)).
    ///
    /// It follows the [`Message::Assistant`] it answers. It is the
    /// harness speaking, not the person and not the system prompt.
    Note(String),
}

/// What the model wants to do next.
#[derive(Debug, Clone)]
pub enum ModelAction {
    /// Call these tools, in order.
    ToolCalls(Vec<ToolCall>),
    /// The loop is done; this is the final answer.
    Text(String),
}

/// A language model behind the loop's seam.
///
/// Synchronous in v0.1. Implementations translate between this
/// interface and whatever provider API they wrap; the loop never
/// sees provider details.
pub trait Model: Send + Sync {
    /// Produce the next action given the history and the tools the
    /// host makes available.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ModelFailed`] when the provider call fails
    /// or the response cannot be interpreted.
    fn next_action(
        &self,
        messages: &[Message],
        tools: &[ToolDescription],
    ) -> Result<ModelAction, Error>;
}

/// A single tool result rendered for the history.
#[derive(Debug, Clone)]
pub struct ToolResultView {
    /// The tool that ran.
    pub tool: String,
    /// The JSON-encoded result (or error reason).
    pub output: Value,
    /// Whether the tool reported success.
    pub ok: bool,
}

impl From<(&ToolCall, &ToolOutput)> for ToolResultView {
    fn from((call, output): (&ToolCall, &ToolOutput)) -> Self {
        let (ok, value) = match output {
            ToolOutput::Ok(v) => (true, v.clone()),
            ToolOutput::Err(reason) => (false, Value::String(reason.clone())),
        };
        Self {
            tool: call.name.clone(),
            output: value,
            ok,
        }
    }
}
