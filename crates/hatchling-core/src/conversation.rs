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
//! count and size ([`Bounds`]), and only whole exchanges, newest kept.
//!
//! The bounds are sized to the model's window (#595). What the agent's
//! prompt and tools take is set aside first ([`RUN_RESERVE_TOKENS`]), and
//! the conversation is given half of what's left, at three bytes a token:
//! the other half is the run's own work, its tools' output and its answer.
//! Never less than [`Bounds::LEAST`], today's bounds, which fit the
//! smallest window Hatchling accepts and stand when no window is known;
//! never more than [`Bounds::MOST`], three times them, since each turn of
//! a run resends the whole of it, against the run's token budget and a
//! paid model's bill.
//!
//! The size is measured as a run is sent it, escaped as JSON, where a
//! control character takes six bytes. Measured as text, an answer the
//! model was led to fill with them is kept, and its history is then too
//! big for the agent to accept (or, past 128 KiB, for the kernel to
//! pass): every run after it would fail until a new conversation.
//!
//! Past the bounds, the conversation is compacted ([`Conversation`]):
//! an exchange's answer goes, and what the owner said in it is kept,
//! verbatim, as what they said earlier, within its own bounds. A decision
//! or a correction the owner made is never paraphrased, and no model,
//! which a page it read could steer, writes a word of it.

use serde::{Deserialize, Serialize};

/// The most exchanges a chat run is given with no window known: those of
/// [`Bounds::LEAST`].
pub const MAX_EXCHANGES: usize = 8;

/// The most bytes, both sides of every exchange escaped as JSON, a chat
/// run is given with no window known: well within the smallest window,
/// beside the tools and the task.
pub const MAX_HISTORY_BYTES: usize = 24 * 1024;

/// The most of the owner's earlier messages a chat run is given with no
/// window known: what they said in exchanges past the bounds.
pub const MAX_EARLIER: usize = 32;

/// The most bytes of them, escaped as JSON, with no window known.
pub const MAX_EARLIER_BYTES: usize = 8 * 1024;

/// How many bytes a token is taken to be, sizing the bounds to a window:
/// fewer than English prose takes, so the window is overestimated in
/// tokens, as the agent's context guard does.
const BYTES_PER_TOKEN: usize = 3;

/// How far [`Bounds::MOST`] goes past [`Bounds::LEAST`].
const MOST_TIMES: usize = 3;

/// The tokens set aside for the agent's own prompt and tools before the
/// conversation is sized: they took about 9,400 on every call in WO-051's
/// measurement, and this leaves them room to grow.
pub const RUN_RESERVE_TOKENS: u32 = 12_288;

/// How much of the owner's conversation a chat run is given: exchanges
/// whole by count and size, and what the owner said before them by count
/// and size, each escaped as JSON as a run is sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// The most exchanges.
    pub exchanges: usize,
    /// The most bytes of them, both sides.
    pub history_bytes: usize,
    /// The most messages the owner sent earlier.
    pub earlier: usize,
    /// The most bytes of them.
    pub earlier_bytes: usize,
}

impl Bounds {
    /// The least a run is given, and what it's given when no window is
    /// known: sized for the smallest window Hatchling accepts.
    pub const LEAST: Self = Self {
        exchanges: MAX_EXCHANGES,
        history_bytes: MAX_HISTORY_BYTES,
        earlier: MAX_EARLIER,
        earlier_bytes: MAX_EARLIER_BYTES,
    };

    /// The most a run is given, whatever its window: reached at 77,824
    /// tokens. Each turn resends it.
    pub const MOST: Self = Self {
        exchanges: MAX_EXCHANGES * MOST_TIMES,
        history_bytes: MAX_HISTORY_BYTES * MOST_TIMES,
        earlier: MAX_EARLIER * MOST_TIMES,
        earlier_bytes: MAX_EARLIER_BYTES * MOST_TIMES,
    };

    /// The bounds for a model whose window is `window` tokens: half of
    /// what [`RUN_RESERVE_TOKENS`] leaves of it, at [`BYTES_PER_TOKEN`],
    /// between [`Bounds::LEAST`] and [`Bounds::MOST`], each bound scaled
    /// from the least alike. With no window known, the least.
    #[must_use]
    pub fn for_window(window: Option<u32>) -> Self {
        let Some(window) = window else {
            return Self::LEAST;
        };
        let least = MAX_HISTORY_BYTES + MAX_EARLIER_BYTES;
        let half =
            usize::try_from(window.saturating_sub(RUN_RESERVE_TOKENS) / 2).unwrap_or(usize::MAX);
        let bytes = half
            .saturating_mul(BYTES_PER_TOKEN)
            .clamp(least, least * MOST_TIMES);
        let scaled = |bound: usize| bound.saturating_mul(bytes) / least;
        Self {
            exchanges: scaled(MAX_EXCHANGES),
            history_bytes: scaled(MAX_HISTORY_BYTES),
            earlier: scaled(MAX_EARLIER),
            earlier_bytes: scaled(MAX_EARLIER_BYTES),
        }
    }
}

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

/// The newest of `exchanges` (oldest first) that fit `bounds`.
///
/// Whole exchanges, oldest first: at most `bounds.exchanges`, and at most
/// `bounds.history_bytes` between them. An exchange too big to fit ends
/// the history there, so what a run is given is always the most recent.
#[must_use]
pub fn recent(exchanges: &[ChatExchange], bounds: &Bounds) -> Vec<ChatExchange> {
    newest(
        exchanges,
        ChatExchange::bytes,
        bounds.exchanges,
        bounds.history_bytes,
    )
}

/// The newest of what the owner `said` earlier (oldest first) that fit
/// `bounds`.
///
/// Whole messages: at most `bounds.earlier`, and at most
/// `bounds.earlier_bytes` between them, escaped as JSON. One too big to
/// fit ends it there.
#[must_use]
pub fn earlier(said: &[String], bounds: &Bounds) -> Vec<String> {
    newest(
        said,
        |text| escaped(text),
        bounds.earlier,
        bounds.earlier_bytes,
    )
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
    /// Keeps `exchange` as the newest, within `bounds`: as
    /// [`Conversation::compact`]. What went.
    pub fn push(&mut self, exchange: ChatExchange, bounds: &Bounds) -> Compacted {
        self.exchanges.push(exchange);
        self.compact(bounds)
    }

    /// Holds it to `bounds`. The exchanges past [`recent`]'s go, and what
    /// the owner said in each (unless nothing) is kept as [`earlier`],
    /// whose own bounds keep its newest. What went.
    pub fn compact(&mut self, bounds: &Bounds) -> Compacted {
        let gone = self
            .exchanges
            .len()
            .saturating_sub(recent(&self.exchanges, bounds).len());
        let said = self.exchanges.drain(..gone).map(|exchange| exchange.user);
        self.earlier.extend(said.filter(|text| !text.is_empty()));
        let before = self.earlier.len();
        self.earlier = earlier(&self.earlier, bounds);
        Compacted {
            answers: gone,
            said: before.saturating_sub(self.earlier.len()),
        }
    }

    /// It, held to `bounds`: what a run with them is given, compacted as
    /// [`Conversation::compact`] does.
    #[must_use]
    pub fn within(&self, bounds: &Bounds) -> Self {
        let mut given = self.clone();
        given.compact(bounds);
        given
    }
}

/// The history a chat run was given, from its JSON, within `bounds`:
/// those of the window the run was told.
///
/// # Errors
///
/// Why it isn't one: not a JSON array of exchanges, or past the bounds
/// (the broker never sends more, so more means it didn't come from the
/// broker).
pub fn parse_history(raw: &str, bounds: &Bounds) -> Result<Vec<ChatExchange>, String> {
    let most = bounds.history_bytes;
    if raw.len() > most.saturating_mul(2) {
        return Err(format!("larger than {most} bytes of turns"));
    }
    let history: Vec<ChatExchange> = serde_json::from_str(raw)
        .map_err(|e| format!("expected a JSON array of exchanges: {e}"))?;
    if history.len() > bounds.exchanges {
        return Err(format!("more than {} exchanges", bounds.exchanges));
    }
    let bytes = history.iter().fold(0_usize, |sum, exchange| {
        sum.saturating_add(exchange.bytes())
    });
    if bytes > most {
        return Err(format!("larger than {most} bytes of turns"));
    }
    Ok(history)
}

/// What the owner said earlier, as a chat run was given it, from its
/// JSON, within `bounds`.
///
/// # Errors
///
/// Why it isn't: not a JSON array of strings, or past the bounds (the
/// broker never sends more, so more means it didn't come from the
/// broker).
pub fn parse_earlier(raw: &str, bounds: &Bounds) -> Result<Vec<String>, String> {
    let most = bounds.earlier_bytes;
    if raw.len() > most.saturating_mul(2) {
        return Err(format!("larger than {most} bytes of messages"));
    }
    let said: Vec<String> =
        serde_json::from_str(raw).map_err(|e| format!("expected a JSON array of messages: {e}"))?;
    if said.len() > bounds.earlier {
        return Err(format!("more than {} messages", bounds.earlier));
    }
    let bytes = said
        .iter()
        .fold(0_usize, |sum, text| sum.saturating_add(escaped(text)));
    if bytes > most {
        return Err(format!("larger than {most} bytes of messages"));
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

    /// The least bounds: today's, and with no window known.
    const L: Bounds = Bounds::LEAST;

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
            let given = recent(conversation.get(start..).unwrap(), &L);
            let raw = serde_json::to_string(&given).unwrap();
            assert_eq!(parse_history(&raw, &L).unwrap(), given, "from {start}");
            assert!(raw.len() < 128 * 1024 - "HATCHLING_RUN_HISTORY=".len());
        }
        // A third of the bound in control characters is two bounds as
        // sent: it isn't given, and ends the history there. The newlines,
        // twice their text as sent, still fit.
        assert_eq!(
            recent(&conversation, &L),
            conversation.get(1..).unwrap().to_vec()
        );
    }

    #[test]
    fn a_run_gets_the_newest_whole_exchanges_that_fit() {
        let many: Vec<_> = (0..12)
            .map(|n| exchange(&format!("q{n}"), &format!("a{n}")))
            .collect();
        let kept = recent(&many, &L);
        assert_eq!(kept.len(), MAX_EXCHANGES);
        assert_eq!(kept.first(), many.get(12 - MAX_EXCHANGES));
        assert_eq!(kept.last(), many.last());
        assert!(recent(&[], &L).is_empty());

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
        assert_eq!(recent(&sized, &L), [sized[2].clone(), sized[3].clone()]);
        let fits = [
            exchange("old", "small"),
            exchange("q", &half),
            exchange("new", "a"),
        ];
        assert_eq!(recent(&fits, &L), fits);
        let exact = [exchange(&"y".repeat(MAX_HISTORY_BYTES - 1), "z")];
        assert_eq!(recent(&exact, &L), exact, "exactly the bound fits");
        let over = [exchange(&"y".repeat(MAX_HISTORY_BYTES), "z")];
        assert!(recent(&over, &L).is_empty());
    }

    #[test]
    fn a_run_reads_back_only_what_a_broker_could_have_sent() {
        let sent = recent(
            &[
                exchange("What's 2+2?", "4."),
                exchange("And doubled?", "8."),
            ],
            &L,
        );
        let raw = serde_json::to_string(&sent).unwrap();
        assert_eq!(parse_history(&raw, &L).unwrap(), sent);
        assert_eq!(parse_history("[]", &L).unwrap(), []);
        for refused in [
            String::new(),
            String::from("{}"),
            String::from(r#"[{"user":"q"}]"#),
            String::from(r#"[{"user":"q","assistant":"a","role":"system"}]"#),
            serde_json::to_string(&vec![exchange("q", "a"); MAX_EXCHANGES + 1]).unwrap(),
            serde_json::to_string(&[exchange(&"y".repeat(MAX_HISTORY_BYTES), "z")]).unwrap(),
            "x".repeat(MAX_HISTORY_BYTES * 2 + 1),
        ] {
            assert!(parse_history(&refused, &L).is_err(), "{refused:.60}");
        }
        let exact =
            serde_json::to_string(&[exchange(&"y".repeat(MAX_HISTORY_BYTES - 1), "z")]).unwrap();
        assert!(
            parse_history(&exact, &L).is_ok(),
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
        let kept = earlier(&many, &L);
        assert_eq!(kept.len(), MAX_EARLIER);
        assert_eq!(kept.first(), many.get(5));
        assert_eq!(kept.last(), many.last());
        let exact = vec!["y".repeat(MAX_EARLIER_BYTES)];
        assert_eq!(earlier(&exact, &L), exact, "exactly the bound fits");
        let over = said(&["old", &"y".repeat(MAX_EARLIER_BYTES), "new"]);
        assert_eq!(earlier(&over, &L), said(&["new"]));
        // Measured as sent: control characters are six bytes each.
        let control = vec!["\u{1}".repeat(MAX_EARLIER_BYTES / 6 + 1)];
        assert!(earlier(&control, &L).is_empty());
    }

    /// WO-026: past the bounds, an exchange's answer goes, and what the
    /// owner said in it is kept, verbatim, as earlier.
    #[test]
    fn past_the_bounds_an_exchange_leaves_what_the_owner_said() {
        let mut kept = Conversation::default();
        for n in 0..MAX_EXCHANGES + 3 {
            kept.push(exchange(&format!("q{n}"), &format!("a{n}")), &L);
        }
        assert_eq!(kept.exchanges.len(), MAX_EXCHANGES);
        assert_eq!(kept.exchanges.first(), Some(&exchange("q3", "a3")));
        assert_eq!(kept.earlier, said(&["q0", "q1", "q2"]));
        // An empty message says nothing to keep.
        kept.push(exchange("", "a empty"), &L);
        kept.push(exchange("q12", "a12"), &L);
        assert_eq!(kept.earlier, said(&["q0", "q1", "q2", "q3", "q4"]));
        // One too big to give takes every exchange with it.
        let huge = "x".repeat(MAX_HISTORY_BYTES);
        kept.push(exchange("Keep it short from now on.", &huge), &L);
        assert!(kept.exchanges.is_empty());
        assert_eq!(
            kept.earlier.last().map(String::as_str),
            Some("Keep it short from now on.")
        );
        // q0 to q4, q5 to q10, q12, and the last.
        assert_eq!(kept.earlier.len(), 13);
        // What the owner said earlier keeps its own bounds.
        for n in 0..MAX_EARLIER {
            kept.push(exchange(&format!("r{n}"), &huge), &L);
        }
        assert_eq!(kept.earlier.len(), MAX_EARLIER);
        assert_eq!(kept.earlier.first().map(String::as_str), Some("r0"));
    }

    #[test]
    fn a_run_reads_back_only_earlier_messages_a_broker_could_have_sent() {
        let sent = earlier(&said(&["Call me Sam.", "No meetings on Fridays."]), &L);
        let raw = serde_json::to_string(&sent).unwrap();
        assert_eq!(parse_earlier(&raw, &L).unwrap(), sent);
        assert!(parse_earlier("[]", &L).unwrap().is_empty());
        let control = vec!["\u{1}".repeat(MAX_EARLIER_BYTES / 6)];
        let raw = serde_json::to_string(&earlier(&control, &L)).unwrap();
        assert_eq!(
            parse_earlier(&raw, &L).unwrap(),
            control,
            "the bound as sent"
        );
        for refused in [
            String::new(),
            String::from("{}"),
            String::from("[1]"),
            String::from(r#"[{"user":"q"}]"#),
            serde_json::to_string(&vec!["m"; MAX_EARLIER + 1]).unwrap(),
            serde_json::to_string(&["y".repeat(MAX_EARLIER_BYTES + 1)]).unwrap(),
            "x".repeat(MAX_EARLIER_BYTES * 2 + 1),
        ] {
            assert!(parse_earlier(&refused, &L).is_err(), "{refused:.60}");
        }
        let exact = serde_json::to_string(&["y".repeat(MAX_EARLIER_BYTES)]).unwrap();
        assert!(
            parse_earlier(&exact, &L).is_ok(),
            "exactly the bound is accepted"
        );
    }

    /// #595: the bounds are half of what the run's reserve leaves of the
    /// window, at three bytes a token, between the least and the most,
    /// each scaled from the least alike.
    #[test]
    fn the_bounds_are_sized_to_the_window() {
        assert_eq!(Bounds::for_window(None), L, "no window known");
        // A 32,768-token window keeps today's room for the run's work.
        for small in [8192, 12_288, 16_384, 32_768, 34_133] {
            assert_eq!(Bounds::for_window(Some(small)), L, "{small}");
        }
        // 40,000 less the reserve is 27,712 tokens; half is 41,568 bytes,
        // shared as the least is.
        assert_eq!(
            Bounds::for_window(Some(40_000)),
            Bounds {
                exchanges: 10,
                history_bytes: 31_176,
                earlier: 40,
                earlier_bytes: 10_392,
            }
        );
        // The installer's window: 79,872 bytes, 2.4 times today's.
        assert_eq!(
            Bounds::for_window(Some(65_536)),
            Bounds {
                exchanges: 19,
                history_bytes: 59_904,
                earlier: 78,
                earlier_bytes: 19_968,
            }
        );
        assert_ne!(Bounds::for_window(Some(77_823)), Bounds::MOST);
        for big in [77_824, 131_072, 1_000_000, u32::MAX] {
            assert_eq!(Bounds::for_window(Some(big)), Bounds::MOST, "{big}");
        }
        assert_eq!(
            Bounds::MOST,
            Bounds {
                exchanges: 24,
                history_bytes: 72 * 1024,
                earlier: 96,
                earlier_bytes: 24 * 1024,
            }
        );
    }

    /// At the most, what a run is given still passes the kernel: the
    /// history and what was said earlier are each under the 128 KiB one
    /// environment string may be, and read back whole.
    #[test]
    fn the_most_a_run_is_given_passes_the_kernel() {
        const ARG_MAX: usize = 128 * 1024;
        let most = Bounds::MOST;
        // Control characters, six bytes each as sent, to the bound.
        let control = "\u{1}".repeat(most.history_bytes / 6 - 100);
        let small = |n: usize| exchange(&format!("q{n}"), &format!("a{n}"));
        let mut many: Vec<_> = (0..most.exchanges - 1).map(small).collect();
        many.insert(0, exchange("Say it back.", &control));
        for history in [vec![exchange("Say it back.", &control)], many] {
            let given = recent(&history, &most);
            assert_eq!(given, history, "within the most");
            let raw = serde_json::to_string(&given).unwrap();
            assert!(raw.len() < ARG_MAX - "HATCHLING_RUN_HISTORY=".len());
            assert_eq!(parse_history(&raw, &most).unwrap(), given);
        }
        let said = vec!["\u{1}".repeat(most.earlier_bytes / 6)];
        let given = earlier(&said, &most);
        assert_eq!(given, said);
        let raw = serde_json::to_string(&given).unwrap();
        assert!(raw.len() < ARG_MAX - "HATCHLING_RUN_EARLIER=".len());
        assert_eq!(parse_earlier(&raw, &most).unwrap(), said);
        // What's within the most is past the least: a run told no
        // window refuses it.
        let raw = serde_json::to_string(&recent(&[exchange("q", &control)], &most)).unwrap();
        assert!(parse_history(&raw, &L).is_err());
    }

    /// What a run is given is read back at its bounds exactly: as many
    /// messages as they allow, and raw text up to twice their bytes before
    /// it's parsed at all. Past that, it's refused unparsed.
    #[test]
    fn a_run_reads_back_at_its_bounds_exactly() {
        let most = Bounds::MOST;
        let said: Vec<String> = (0..most.earlier).map(|n| format!("m{n}")).collect();
        let raw = serde_json::to_string(&said).unwrap();
        assert_eq!(
            parse_earlier(&raw, &most).unwrap(),
            said,
            "as many as allowed"
        );
        let many: Vec<String> = (0..=most.earlier).map(|n| format!("m{n}")).collect();
        let raw = serde_json::to_string(&many).unwrap();
        assert!(
            parse_earlier(&raw, &most)
                .unwrap_err()
                .contains("more than")
        );
        // Twice the bytes is parsed (and isn't JSON); one more isn't.
        let at_and_past = |bound: usize, parse: &dyn Fn(&str) -> Result<(), String>| {
            let at = parse(&"x".repeat(bound * 2)).unwrap_err();
            assert!(at.starts_with("expected a JSON array"), "{at}");
            let past = parse(&"x".repeat(bound * 2 + 1)).unwrap_err();
            assert!(past.starts_with("larger than"), "{past}");
        };
        at_and_past(most.history_bytes, &|raw| {
            parse_history(raw, &most).map(|_| ())
        });
        at_and_past(most.earlier_bytes, &|raw| {
            parse_earlier(raw, &most).map(|_| ())
        });
    }

    /// A conversation kept within larger bounds, given within smaller
    /// ones, is compacted as a push would: the oldest answers go, and what
    /// the owner said in them is given as earlier, word for word. What was
    /// kept is left as it was.
    #[test]
    fn a_conversation_given_within_smaller_bounds_is_compacted() {
        let mut kept = Conversation::default();
        for n in 0..20 {
            let compacted = kept.push(exchange(&format!("q{n}"), &format!("a{n}")), &Bounds::MOST);
            assert_eq!(compacted, Compacted::default());
        }
        assert_eq!((kept.exchanges.len(), kept.earlier.len()), (20, 0));
        let given = kept.within(&L);
        assert_eq!(given.exchanges.len(), MAX_EXCHANGES);
        assert_eq!(given.exchanges.first(), Some(&exchange("q12", "a12")));
        let said: Vec<String> = (0..12).map(|n| format!("q{n}")).collect();
        assert_eq!(given.earlier, said);
        assert_eq!(kept.exchanges.len(), 20, "what was kept is as it was");
        assert_eq!(kept.within(&Bounds::MOST), kept);
        // Compacted in place, it says what went.
        let mut held = kept.clone();
        assert_eq!(
            held.compact(&L),
            Compacted {
                answers: 12,
                said: 0
            }
        );
        assert_eq!(held, given);
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
