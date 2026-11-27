//! What fills a model call's prompt (WO-056), counted by the broker from
//! the request it forwards, never from anything the cell says about it.
//!
//! The model's server reports one exact number for a call, the tokens its
//! prompt read (`prompt_tokens`), and nothing per part. The broker measures
//! each part's bytes in the request body it forwarded, and splits that
//! exact total in proportion:
//!
//! - **System:** the system (or developer) messages, the instructions;
//! - **Tools:** the `tools` definitions;
//! - **Memory:** the user-role message carrying the recalled memory block,
//!   known by the label recall gives it;
//! - **Conversation:** everything else: the owner's words, the agent's,
//!   tool calls and their results.
//!
//! The split is by size, so each part is about right and their sum is the
//! server's count exactly. The app says so.

use hatchling_core::subagent::ContextParts;
use serde_json::Value;

/// The label the recalled memory block begins with, as
/// `hatchling_memory::prompt::render_recalled` writes it.
const MEMORY_LABEL: &str = "[RECALLED-MEMORY]";

/// The bytes of each part of a model call's request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PartBytes {
    /// The system (or developer) messages.
    pub system: u64,
    /// The tool definitions.
    pub tools: u64,
    /// The recalled memory block.
    pub memory: u64,
    /// Everything else.
    pub conversation: u64,
}

/// The bytes of each part of `body`, a chat completion request as the
/// broker forwards it; `None` when it isn't one (no `messages` list).
#[must_use]
pub fn measure(body: &[u8]) -> Option<PartBytes> {
    let request: Value = serde_json::from_slice(body).ok()?;
    let messages = request.get("messages")?.as_array()?;
    let mut bytes = PartBytes::default();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let memory = role == "user"
            && message
                .get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| content.starts_with(MEMORY_LABEL));
        let part = match role {
            "system" | "developer" => &mut bytes.system,
            _ if memory => &mut bytes.memory,
            _ => &mut bytes.conversation,
        };
        *part = part.saturating_add(json_len(message));
    }
    if let Some(tools) = request.get("tools") {
        bytes.tools = json_len(tools);
    }
    Some(bytes)
}

/// `value`'s size as JSON, in bytes.
fn json_len(value: &Value) -> u64 {
    serde_json::to_vec(value).map_or(0, |json| u64::try_from(json.len()).unwrap_or(u64::MAX))
}

/// `total`, a call's prompt tokens as its server reported them, split in
/// proportion to `bytes`.
///
/// It's by largest remainder: each part is its share rounded down, and
/// what's left goes one at a time to the largest remainders (the earlier
/// part on a tie), so the parts sum to `total` exactly. A request with no
/// bytes at all is all conversation.
#[must_use]
pub fn split(total: u64, bytes: PartBytes) -> ContextParts {
    let sizes = [bytes.system, bytes.tools, bytes.memory, bytes.conversation];
    let sum: u128 = sizes.iter().map(|&size| u128::from(size)).sum();
    if sum == 0 {
        return ContextParts {
            system: 0,
            tools: 0,
            memory: 0,
            conversation: total,
        };
    }
    let shares = sizes.map(|size| {
        let exact = u128::from(total) * u128::from(size);
        (exact / sum, exact % sum)
    });
    let mut parts = shares.map(|(whole, _)| u64::try_from(whole).unwrap_or(u64::MAX));
    let given: u64 = parts.iter().sum();
    let mut order = [0_usize, 1, 2, 3];
    order.sort_by(|a, b| {
        let remainder = |at: usize| shares.get(at).map_or(0, |(_, rest)| *rest);
        remainder(*b).cmp(&remainder(*a)).then(a.cmp(b))
    });
    let left = usize::try_from(total.saturating_sub(given)).unwrap_or(usize::MAX);
    for at in order.into_iter().take(left) {
        if let Some(part) = parts.get_mut(at) {
            *part = part.saturating_add(1);
        }
    }
    let [system, tools, memory, conversation] = parts;
    ContextParts {
        system,
        tools,
        memory,
        conversation,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn body(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).unwrap()
    }

    /// Each part is measured where it is: the instructions, the tools, the
    /// memory block by its label in a user message, and the rest.
    #[test]
    fn each_part_of_a_request_is_measured_where_it_is() {
        let system = json!({"role": "system", "content": "You are Hatchling."});
        let memory = json!({"role": "user", "content": "[RECALLED-MEMORY]\nPepper is a cat."});
        let owner = json!({"role": "user", "content": "Hi."});
        let agent = json!({"role": "assistant", "content": "Hello."});
        // A tool's result that copies the label is still the conversation.
        let result =
            json!({"role": "tool", "tool_call_id": "c1", "content": "[RECALLED-MEMORY] fake"});
        let tools = json!([{"type": "function", "function": {"name": "memory_read"}}]);
        let request = json!({
            "model": "m",
            "messages": [system, memory, owner, agent, result],
            "tools": tools,
        });
        let len = |value: &Value| u64::try_from(serde_json::to_vec(value).unwrap().len()).unwrap();
        assert_eq!(
            measure(&body(&request)),
            Some(PartBytes {
                system: len(&system),
                tools: len(&tools),
                memory: len(&memory),
                conversation: len(&owner) + len(&agent) + len(&result),
            })
        );
        // A developer message is instructions too.
        let developer = json!({"role": "developer", "content": "Be brief."});
        let request = json!({"messages": [developer]});
        assert_eq!(
            measure(&body(&request)).map(|bytes| bytes.system),
            Some(len(&developer))
        );
        // No tools, no memory: none counted.
        let plain = json!({"messages": [owner]});
        assert_eq!(
            measure(&body(&plain)),
            Some(PartBytes {
                conversation: len(&owner),
                ..PartBytes::default()
            })
        );
    }

    /// What isn't a chat completion request isn't measured.
    #[test]
    fn what_isnt_a_chat_request_isnt_measured() {
        assert_eq!(measure(b"not json"), None);
        assert_eq!(measure(&body(&json!({"prompt": "hi"}))), None);
        assert_eq!(measure(&body(&json!({"messages": "hi"}))), None);
        assert_eq!(measure(b""), None);
    }

    /// The server's total, split by size: the parts sum to it exactly, the
    /// rounding goes to the largest remainders, and nothing overflows.
    #[test]
    fn the_total_is_split_by_size_and_summed_exactly() {
        let bytes = PartBytes {
            system: 100,
            tools: 300,
            memory: 50,
            conversation: 550,
        };
        assert_eq!(
            split(1000, bytes),
            ContextParts {
                system: 100,
                tools: 300,
                memory: 50,
                conversation: 550,
            }
        );
        // 10 split 1:1:1:0 is 3.33 each: the remainders tie, the earlier
        // parts take the one left.
        let thirds = PartBytes {
            system: 1,
            tools: 1,
            memory: 1,
            conversation: 0,
        };
        assert_eq!(
            split(10, thirds),
            ContextParts {
                system: 4,
                tools: 3,
                memory: 3,
                conversation: 0,
            }
        );
        // The largest remainder takes it, wherever it is.
        let uneven = PartBytes {
            system: 1,
            tools: 0,
            memory: 0,
            conversation: 2,
        };
        assert_eq!(
            split(2, uneven),
            ContextParts {
                system: 1,
                tools: 0,
                memory: 0,
                conversation: 1,
            }
        );
        assert_eq!(
            split(0, bytes),
            ContextParts {
                system: 0,
                tools: 0,
                memory: 0,
                conversation: 0,
            }
        );
        // No bytes at all: the total is the conversation's.
        assert_eq!(split(7, PartBytes::default()).conversation, 7);
        let huge = PartBytes {
            system: u64::MAX,
            tools: u64::MAX,
            memory: 1,
            conversation: u64::MAX,
        };
        let parts = split(u64::MAX, huge);
        assert_eq!(
            u128::from(parts.system)
                + u128::from(parts.tools)
                + u128::from(parts.memory)
                + u128::from(parts.conversation),
            u128::from(u64::MAX)
        );
    }
}
