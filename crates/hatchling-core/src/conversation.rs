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

use serde::{Deserialize, Serialize};

/// The most exchanges a chat run is given.
pub const MAX_EXCHANGES: usize = 8;

/// The most bytes of text, both sides of every exchange, a chat run is
/// given: well within a model's context, beside the tools and the task.
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
    /// Its text's size, both sides.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.user.len().saturating_add(self.assistant.len())
    }
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
