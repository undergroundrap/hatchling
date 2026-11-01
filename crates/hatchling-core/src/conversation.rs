//! The owner's conversation, as a chat run is given it (WO-026).
//!
//! Each chat message is answered by a fresh cell, which knows nothing of
//! the messages before it unless the broker says. The broker keeps the
//! conversation, outside any cell, and gives each chat run its last
//! exchanges, typed: what the owner said, and what the owner was shown in
//! reply. A cell can't add to it, and nothing it writes elsewhere (a note,
//! a memory) ever comes back as a turn.
//!
//! An exchange is kept only when its answer reached the owner as written:
//! an answer the output gate blocked or replaced, or a run that failed,
//! is never given back as context. What a run is given is bounded, by
//! count ([`MAX_EXCHANGES`]) and size ([`MAX_HISTORY_BYTES`]), and only
//! whole exchanges, newest kept.
//!
//! The size is measured as a run is sent it, escaped as JSON, where a
//! control character takes six bytes. Measured as text, an answer the
//! model was led to fill with them is kept, and its history is then too
//! big for the agent to accept (or, past 128 KiB, for the kernel to
//! pass): every run after it would fail until a new conversation.

use serde::{Deserialize, Serialize};

/// The most exchanges a chat run is given.
pub const MAX_EXCHANGES: usize = 8;

/// The most bytes, both sides of every exchange escaped as JSON, a chat
/// run is given: well within a model's context, beside the tools and the
/// task.
pub const MAX_HISTORY_BYTES: usize = 24 * 1024;

/// One exchange of the owner's conversation: a message and the answer the
/// owner was shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatExchange {
    /// What the owner said, as the broker accepted it.
    pub user: String,
    /// What the owner was shown in reply, as the output gate cleared it.
    pub assistant: String,
}

impl ChatExchange {
    /// Its size as a run is sent it: both sides, escaped as JSON.
    #[must_use]
    pub fn bytes(&self) -> usize {
        escaped(&self.user).saturating_add(escaped(&self.assistant))
    }
}

/// The size of `text` in a JSON string, quotes aside: what `serde_json`
/// writes for it.
fn escaped(text: &str) -> usize {
    text.chars()
        .map(|c| match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            '\0'..='\u{1f}' => 6,
            _ => c.len_utf8(),
        })
        .fold(0, usize::saturating_add)
}

/// The newest of `exchanges` (oldest first) that fit the bounds.
///
/// Whole exchanges, oldest first: at most [`MAX_EXCHANGES`], and at most
/// [`MAX_HISTORY_BYTES`] between them. An exchange too big to fit ends
/// the history there, so what a run is given is always the most recent.
#[must_use]
pub fn recent(exchanges: &[ChatExchange]) -> Vec<ChatExchange> {
    let mut kept = Vec::new();
    let mut bytes = 0_usize;
    for exchange in exchanges.iter().rev().take(MAX_EXCHANGES) {
        bytes = bytes.saturating_add(exchange.bytes());
        if bytes > MAX_HISTORY_BYTES {
            break;
        }
        kept.push(exchange.clone());
    }
    kept.reverse();
    kept
}

/// The history a chat run was given, from its JSON.
///
/// # Errors
///
/// Why it isn't one: not a JSON array of exchanges, or past the bounds
/// (the broker never sends more, so more means it didn't come from the
/// broker).
pub fn parse_history(raw: &str) -> Result<Vec<ChatExchange>, String> {
    if raw.len() > MAX_HISTORY_BYTES.saturating_mul(2) {
        return Err(format!("larger than {MAX_HISTORY_BYTES} bytes of turns"));
    }
    let history: Vec<ChatExchange> = serde_json::from_str(raw)
        .map_err(|e| format!("expected a JSON array of exchanges: {e}"))?;
    if history.len() > MAX_EXCHANGES {
        return Err(format!("more than {MAX_EXCHANGES} exchanges"));
    }
    let bytes = history.iter().fold(0_usize, |sum, exchange| {
        sum.saturating_add(exchange.bytes())
    });
    if bytes > MAX_HISTORY_BYTES {
        return Err(format!("larger than {MAX_HISTORY_BYTES} bytes of turns"));
    }
    Ok(history)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(user: &str, assistant: &str) -> ChatExchange {
        ChatExchange {
            user: user.to_owned(),
            assistant: assistant.to_owned(),
        }
    }

    #[test]
    fn an_exchange_is_the_size_of_both_its_sides() {
        assert_eq!(exchange("four", "seven!!").bytes(), 11);
        assert_eq!(exchange("", "").bytes(), 0);
        // As sent: escaped as JSON.
        assert_eq!(exchange("a\"b", "\u{1}é").bytes(), 4 + 6 + 2);
    }

    /// Each character is measured as `serde_json` writes it.
    #[test]
    fn a_side_is_measured_as_json_writes_it() {
        let chars = ('\0'..='\u{80}').chain(['é', '€', '\u{2028}', '😀']);
        for c in chars {
            let text = c.to_string();
            let json = serde_json::to_string(&text).unwrap();
            assert_eq!(escaped(&text), json.len() - 2, "{c:?}");
        }
    }

    /// WO-026: a history of control characters, which a model can be led
    /// to answer with, is bounded as it's sent, so the agent reads back
    /// every history it's given, and the kernel can pass it.
    #[test]
    fn a_run_reads_back_whatever_it_is_given() {
        let control = "\u{1}".repeat(MAX_HISTORY_BYTES / 3);
        let newlines = "\n".repeat(MAX_HISTORY_BYTES / 2 - 100);
        let conversation = [
            exchange("Say it back.", &control),
            exchange("Now newlines.", &newlines),
            exchange("q", "a"),
        ];
        for start in 0..conversation.len() {
            let given = recent(conversation.get(start..).unwrap());
            let raw = serde_json::to_string(&given).unwrap();
            assert_eq!(parse_history(&raw).unwrap(), given, "from {start}");
            assert!(raw.len() < 128 * 1024 - "HATCHLING_RUN_HISTORY=".len());
        }
        // A third of the bound in control characters is two bounds as
        // sent: it isn't given, and ends the history there. The newlines,
        // twice their text as sent, still fit.
        assert_eq!(
            recent(&conversation),
            conversation.get(1..).unwrap().to_vec()
        );
    }

    #[test]
    fn a_run_gets_the_newest_whole_exchanges_that_fit() {
        let many: Vec<_> = (0..12)
            .map(|n| exchange(&format!("q{n}"), &format!("a{n}")))
            .collect();
        let kept = recent(&many);
        assert_eq!(kept.len(), MAX_EXCHANGES);
        assert_eq!(kept.first(), many.get(12 - MAX_EXCHANGES));
        assert_eq!(kept.last(), many.last());
        assert!(recent(&[]).is_empty());

        // Size: the newest that fit, whole; a big one ends it there.
        let half = "x".repeat(MAX_HISTORY_BYTES / 2);
        let sized = [
            exchange("old", "small"),
            exchange(&half, "big"),
            exchange("q", &half),
            exchange("newest", "small"),
        ];
        // The newest two fit; the big one before them ends it, and the
        // small one before that isn't given, though it would fit.
        assert_eq!(recent(&sized), [sized[2].clone(), sized[3].clone()]);
        let fits = [
            exchange("old", "small"),
            exchange("q", &half),
            exchange("new", "a"),
        ];
        assert_eq!(recent(&fits), fits);
        let exact = [exchange(&"y".repeat(MAX_HISTORY_BYTES - 1), "z")];
        assert_eq!(recent(&exact), exact, "exactly the bound fits");
        let over = [exchange(&"y".repeat(MAX_HISTORY_BYTES), "z")];
        assert!(recent(&over).is_empty());
    }

    #[test]
    fn a_run_reads_back_only_what_a_broker_could_have_sent() {
        let sent = recent(&[
            exchange("What's 2+2?", "4."),
            exchange("And doubled?", "8."),
        ]);
        let raw = serde_json::to_string(&sent).unwrap();
        assert_eq!(parse_history(&raw).unwrap(), sent);
        assert_eq!(parse_history("[]").unwrap(), []);
        for refused in [
            String::new(),
            String::from("{}"),
            String::from(r#"[{"user":"q"}]"#),
            String::from(r#"[{"user":"q","assistant":"a","role":"system"}]"#),
            serde_json::to_string(&vec![exchange("q", "a"); MAX_EXCHANGES + 1]).unwrap(),
            serde_json::to_string(&[exchange(&"y".repeat(MAX_HISTORY_BYTES), "z")]).unwrap(),
            "x".repeat(MAX_HISTORY_BYTES * 2 + 1),
        ] {
            assert!(parse_history(&refused).is_err(), "{refused:.60}");
        }
        let exact =
            serde_json::to_string(&[exchange(&"y".repeat(MAX_HISTORY_BYTES - 1), "z")]).unwrap();
        assert!(
            parse_history(&exact).is_ok(),
            "exactly the bound is accepted"
        );
    }
}
