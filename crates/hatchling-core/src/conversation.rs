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
//!
//! Past the bounds, the conversation is compacted ([`Conversation`]):
//! an exchange's answer goes, and what the owner said in it is kept,
//! verbatim, as what they said earlier ([`MAX_EARLIER`],
//! [`MAX_EARLIER_BYTES`]). A decision or a correction the owner made is
//! never paraphrased, and no model, which a page it read could steer,
//! writes a word of it.

use serde::{Deserialize, Serialize};

/// The most exchanges a chat run is given.
pub const MAX_EXCHANGES: usize = 8;

/// The most bytes, both sides of every exchange escaped as JSON, a chat
/// run is given: well within a model's context, beside the tools and the
/// task.
pub const MAX_HISTORY_BYTES: usize = 24 * 1024;

/// The most of the owner's earlier messages a chat run is given: what
/// they said in exchanges past [`recent`]'s bounds.
pub const MAX_EARLIER: usize = 32;

/// The most bytes of them, escaped as JSON: a few pages, and with the
/// exchanges, well within what a cell can be sent.
pub const MAX_EARLIER_BYTES: usize = 8 * 1024;

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
    newest(
        exchanges,
        ChatExchange::bytes,
        MAX_EXCHANGES,
        MAX_HISTORY_BYTES,
    )
}

/// The newest of what the owner `said` earlier (oldest first) that fit.
///
/// Whole messages: at most [`MAX_EARLIER`], and at most
/// [`MAX_EARLIER_BYTES`] between them, escaped as JSON. One too big to
/// fit ends it there.
#[must_use]
pub fn earlier(said: &[String]) -> Vec<String> {
    newest(said, |text| escaped(text), MAX_EARLIER, MAX_EARLIER_BYTES)
}

/// The newest of `items` (oldest first), whole, that fit: at most `most`
/// of them, and at most `bytes` of their `size` between them. One too big
/// to fit ends it there, so what's kept is always the most recent.
fn newest<T: Clone>(items: &[T], size: impl Fn(&T) -> usize, most: usize, bytes: usize) -> Vec<T> {
    let mut kept = Vec::new();
    let mut sum = 0_usize;
    for item in items.iter().rev().take(most) {
        sum = sum.saturating_add(size(item));
        if sum > bytes {
            break;
        }
        kept.push(item.clone());
    }
    kept.reverse();
    kept
}

/// The conversation a chat keeps: what the owner said earlier, and the
/// exchanges since, each within its bounds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conversation {
    /// What the owner said in exchanges no longer given whole, oldest
    /// first.
    pub earlier: Vec<String>,
    /// The exchanges a run is given, oldest first.
    pub exchanges: Vec<ChatExchange>,
}

/// What keeping one more exchange compacted away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Compacted {
    /// Exchanges no longer given whole: their answers went, and what the
    /// owner said in them is kept as earlier.
    pub answers: usize,
    /// What the owner said earlier, no longer given at all: past
    /// [`earlier`]'s bounds.
    pub said: usize,
}

impl Conversation {
    /// Keeps `exchange` as the newest. The exchanges past [`recent`]'s
    /// bounds go, and what the owner said in each (unless nothing) is
    /// kept as [`earlier`], whose own bounds keep its newest. What went.
    pub fn push(&mut self, exchange: ChatExchange) -> Compacted {
        self.exchanges.push(exchange);
        let gone = self
            .exchanges
            .len()
            .saturating_sub(recent(&self.exchanges).len());
        let said = self.exchanges.drain(..gone).map(|exchange| exchange.user);
        self.earlier.extend(said.filter(|text| !text.is_empty()));
        let before = self.earlier.len();
        self.earlier = earlier(&self.earlier);
        Compacted {
            answers: gone,
            said: before.saturating_sub(self.earlier.len()),
        }
    }
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

/// What the owner said earlier, as a chat run was given it, from its
/// JSON.
///
/// # Errors
///
/// Why it isn't: not a JSON array of strings, or past the bounds (the
/// broker never sends more, so more means it didn't come from the
/// broker).
pub fn parse_earlier(raw: &str) -> Result<Vec<String>, String> {
    if raw.len() > MAX_EARLIER_BYTES.saturating_mul(2) {
        return Err(format!("larger than {MAX_EARLIER_BYTES} bytes of messages"));
    }
    let said: Vec<String> =
        serde_json::from_str(raw).map_err(|e| format!("expected a JSON array of messages: {e}"))?;
    if said.len() > MAX_EARLIER {
        return Err(format!("more than {MAX_EARLIER} messages"));
    }
    let bytes = said
        .iter()
        .fold(0_usize, |sum, text| sum.saturating_add(escaped(text)));
    if bytes > MAX_EARLIER_BYTES {
        return Err(format!("larger than {MAX_EARLIER_BYTES} bytes of messages"));
    }
    Ok(said)
}

/// What a model is shown of what the owner `said` earlier.
///
/// One message, in the owner's role, before the exchanges, saying what it
/// is and quoting each as they sent it. Nothing when they said nothing
/// earlier.
#[must_use]
pub fn render_earlier(said: &[String]) -> Option<String> {
    if said.is_empty() {
        return None;
    }
    let mut block = String::from(EARLIER_HEADER);
    for text in said {
        block.push_str("\n\n---\n");
        block.push_str(text);
    }
    Some(block)
}

/// What heads the owner's earlier messages, as a model is shown them.
pub const EARLIER_HEADER: &str = "[EARLIER IN THIS CONVERSATION]\nHatchling's record of what the owner said earlier in this conversation, before the exchanges that follow: each message exactly as they sent it, oldest first. Your answers to them are no longer shown.";

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

    fn said(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| (*text).to_owned()).collect()
    }

    #[test]
    fn the_newest_whole_items_that_fit_are_kept() {
        let size = |n: &usize| *n;
        assert_eq!(newest(&[1, 2, 3, 4], size, 3, 100), [2, 3, 4]);
        assert_eq!(newest(&[1, 2, 3, 4], size, 9, 7), [3, 4]);
        assert_eq!(newest(&[1, 2, 3, 4], size, 9, 6), [4]);
        // One too big ends it there, though an older one would fit.
        assert_eq!(newest(&[1, 9, 2], size, 9, 8), [2]);
        assert_eq!(
            newest(&[1, 2], size, 9, 3),
            [1, 2],
            "exactly the bound fits"
        );
        assert!(newest(&[5], size, 9, 4).is_empty());
        assert!(newest::<usize>(&[], size, 9, 4).is_empty());
    }

    #[test]
    fn what_the_owner_said_earlier_is_kept_within_its_bounds() {
        let many: Vec<String> = (0..MAX_EARLIER + 5).map(|n| format!("m{n}")).collect();
        let kept = earlier(&many);
        assert_eq!(kept.len(), MAX_EARLIER);
        assert_eq!(kept.first(), many.get(5));
        assert_eq!(kept.last(), many.last());
        let exact = vec!["y".repeat(MAX_EARLIER_BYTES)];
        assert_eq!(earlier(&exact), exact, "exactly the bound fits");
        let over = said(&["old", &"y".repeat(MAX_EARLIER_BYTES), "new"]);
        assert_eq!(earlier(&over), said(&["new"]));
        // Measured as sent: control characters are six bytes each.
        let control = vec!["\u{1}".repeat(MAX_EARLIER_BYTES / 6 + 1)];
        assert!(earlier(&control).is_empty());
    }

    /// WO-026: past the bounds, an exchange's answer goes, and what the
    /// owner said in it is kept, verbatim, as earlier.
    #[test]
    fn past_the_bounds_an_exchange_leaves_what_the_owner_said() {
        let mut kept = Conversation::default();
        for n in 0..MAX_EXCHANGES + 3 {
            kept.push(exchange(&format!("q{n}"), &format!("a{n}")));
        }
        assert_eq!(kept.exchanges.len(), MAX_EXCHANGES);
        assert_eq!(kept.exchanges.first(), Some(&exchange("q3", "a3")));
        assert_eq!(kept.earlier, said(&["q0", "q1", "q2"]));
        // An empty message says nothing to keep.
        kept.push(exchange("", "a empty"));
        kept.push(exchange("q12", "a12"));
        assert_eq!(kept.earlier, said(&["q0", "q1", "q2", "q3", "q4"]));
        // One too big to give takes every exchange with it.
        let huge = "x".repeat(MAX_HISTORY_BYTES);
        kept.push(exchange("Keep it short from now on.", &huge));
        assert!(kept.exchanges.is_empty());
        assert_eq!(
            kept.earlier.last().map(String::as_str),
            Some("Keep it short from now on.")
        );
        // q0 to q4, q5 to q10, q12, and the last.
        assert_eq!(kept.earlier.len(), 13);
        // What the owner said earlier keeps its own bounds.
        for n in 0..MAX_EARLIER {
            kept.push(exchange(&format!("r{n}"), &huge));
        }
        assert_eq!(kept.earlier.len(), MAX_EARLIER);
        assert_eq!(kept.earlier.first().map(String::as_str), Some("r0"));
    }

    #[test]
    fn a_run_reads_back_only_earlier_messages_a_broker_could_have_sent() {
        let sent = earlier(&said(&["Call me Sam.", "No meetings on Fridays."]));
        let raw = serde_json::to_string(&sent).unwrap();
        assert_eq!(parse_earlier(&raw).unwrap(), sent);
        assert!(parse_earlier("[]").unwrap().is_empty());
        let control = vec!["\u{1}".repeat(MAX_EARLIER_BYTES / 6)];
        let raw = serde_json::to_string(&earlier(&control)).unwrap();
        assert_eq!(parse_earlier(&raw).unwrap(), control, "the bound as sent");
        for refused in [
            String::new(),
            String::from("{}"),
            String::from("[1]"),
            String::from(r#"[{"user":"q"}]"#),
            serde_json::to_string(&vec!["m"; MAX_EARLIER + 1]).unwrap(),
            serde_json::to_string(&["y".repeat(MAX_EARLIER_BYTES + 1)]).unwrap(),
            "x".repeat(MAX_EARLIER_BYTES * 2 + 1),
        ] {
            assert!(parse_earlier(&refused).is_err(), "{refused:.60}");
        }
        let exact = serde_json::to_string(&["y".repeat(MAX_EARLIER_BYTES)]).unwrap();
        assert!(
            parse_earlier(&exact).is_ok(),
            "exactly the bound is accepted"
        );
    }

    /// The model is told what the block is, then each message, as sent.
    #[test]
    fn the_model_is_shown_what_was_said_earlier_quoted_and_labeled() {
        assert_eq!(render_earlier(&[]), None);
        let block = render_earlier(&said(&["Call me Sam.", "Two\nlines."])).unwrap();
        assert_eq!(
            block,
            format!("{EARLIER_HEADER}\n\n---\nCall me Sam.\n\n---\nTwo\nlines.")
        );
        assert!(EARLIER_HEADER.contains("the owner said earlier"));
    }
}
