//! The run record (WO-053): what a run did outside itself, as the broker
//! saw it happen, never as the agent tells it.
//!
//! Each effect is an [`Entry`]: a message sent, a file written in the
//! Room, an item handed to the owner's Library, an approval asked or used,
//! a use refused. The record also carries the run's spend. It's bounded
//! ([`MAX_ENTRIES`], [`MAX_RECORD_BYTES`]): past either bound, an effect is
//! counted by its kind in [`RunRecord::more`] instead of kept.
//!
//! The broker writes it; no session op does. Its strings came from the
//! cell, so a reader takes them as data, never as instructions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::subagent::ToolUse;

/// The most entries a record keeps.
pub const MAX_ENTRIES: usize = 64;
/// The most bytes a record's entries take, as JSON.
pub const MAX_RECORD_BYTES: usize = 16 * 1024;
/// The most bytes of a message's text an entry keeps.
pub const MAX_TEXT_BYTES: usize = 400;
/// The most bytes of a name, a path or a channel an entry keeps.
pub const MAX_NAME_BYTES: usize = 256;
/// What a string that held a secret is kept as.
pub const WITHHELD: &str = "[withheld: it held a secret]";

/// How a message the run sent ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Sent {
    /// Sent as written.
    Sent,
    /// Stopped by the output gate: nothing was sent.
    Redacted,
}

/// One effect a run had outside itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Effect {
    /// A message on a channel: its text kept only when it was sent.
    MessageSent {
        /// The broker's id for the channel.
        channel: String,
        /// How it ended.
        outcome: Sent,
        /// The text, as sent, cut to [`MAX_TEXT_BYTES`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    /// A file written in the Room.
    RoomWrote {
        /// Its path in the Room.
        path: String,
        /// Its length, in bytes.
        bytes: u64,
        /// Whether a file was there before.
        replaced: bool,
    },
    /// A file edited in the Room.
    RoomEdited {
        /// Its path in the Room.
        path: String,
        /// Its length after, in bytes.
        bytes: u64,
        /// How many places were changed.
        replaced: u32,
    },
    /// A file handed to the owner's Library.
    LibraryItem {
        /// The broker's name for it, as the owner is shown it.
        name: String,
        /// Its length, in bytes.
        bytes: u64,
    },
    /// A folder handed to the owner's Library.
    LibraryFolder {
        /// The broker's name for it, as the owner is shown it.
        name: String,
        /// How many files it holds.
        files: u32,
        /// Its files' length together, in bytes.
        bytes: u64,
    },
    /// The owner was asked to approve a use.
    ApprovalAsked {
        /// The use.
        tool: ToolUse,
    },
    /// A use went ahead on an approval the owner gave.
    ApprovalUsed {
        /// The use.
        tool: ToolUse,
    },
    /// A use was refused.
    Refused {
        /// The use.
        tool: ToolUse,
    },
}

impl Effect {
    /// Its kind, as [`RunRecord::more`] counts it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::MessageSent { .. } => "message_sent",
            Self::RoomWrote { .. } => "room_wrote",
            Self::RoomEdited { .. } => "room_edited",
            Self::LibraryItem { .. } => "library_item",
            Self::LibraryFolder { .. } => "library_folder",
            Self::ApprovalAsked { .. } => "approval_asked",
            Self::ApprovalUsed { .. } => "approval_used",
            Self::Refused { .. } => "refused",
        }
    }

    /// Its strings cut to their bounds: a name, path or channel to
    /// [`MAX_NAME_BYTES`], a message's text to [`MAX_TEXT_BYTES`].
    #[must_use]
    pub fn bounded(mut self) -> Self {
        for (text, max) in self.strings() {
            cut(text, max);
        }
        self
    }

    /// Each string it holds, with its bound.
    fn strings(&mut self) -> Vec<(&mut String, usize)> {
        match self {
            Self::MessageSent { channel, text, .. } => {
                let mut strings = vec![(channel, MAX_NAME_BYTES)];
                strings.extend(text.as_mut().map(|text| (text, MAX_TEXT_BYTES)));
                strings
            }
            Self::RoomWrote { path, .. } | Self::RoomEdited { path, .. } => {
                vec![(path, MAX_NAME_BYTES)]
            }
            Self::LibraryItem { name, .. } | Self::LibraryFolder { name, .. } => {
                vec![(name, MAX_NAME_BYTES)]
            }
            Self::ApprovalAsked { .. } | Self::ApprovalUsed { .. } | Self::Refused { .. } => {
                Vec::new()
            }
        }
    }

    /// Each string `holds` says holds a secret, replaced by [`WITHHELD`].
    /// A string is kept only once `holds` has answered that it doesn't.
    pub fn withhold(&mut self, mut holds: impl FnMut(&str) -> bool) {
        for (text, _) in self.strings() {
            if holds(text) {
                WITHHELD.clone_into(text);
            }
        }
    }
}

/// `text` cut to at most `max` bytes, at a character's edge.
fn cut(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.truncate(end);
}

/// An effect, and when it happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// When, in seconds since the Unix epoch.
    pub at: i64,
    /// What.
    pub effect: Effect,
}

/// What a run spent on its model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Spend {
    /// Model calls made.
    pub model_calls: u32,
    /// Tokens sent, over every call.
    pub input_tokens: u64,
    /// Tokens answered, over every call.
    pub output_tokens: u64,
}

/// A run's record: its effects, in order, and its spend.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RunRecord {
    /// The run's token (`run-…`).
    pub run: String,
    /// Its effects, the first kept, in the order they happened.
    pub entries: Vec<Entry>,
    /// How many more effects of each kind there were, past the bounds.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub more: BTreeMap<String, u32>,
    /// What it spent.
    pub spend: Spend,
}

impl RunRecord {
    /// An empty record for the run `run`.
    #[must_use]
    pub fn new(run: &str) -> Self {
        Self {
            run: run.to_owned(),
            ..Self::default()
        }
    }

    /// Notes `effect` at `at`, cut to its bounds: kept while the record has
    /// room, counted in [`Self::more`] once it hasn't.
    pub fn note(&mut self, at: i64, effect: Effect) {
        let entry = Entry {
            at,
            effect: effect.bounded(),
        };
        let fits = self.entries.len() < MAX_ENTRIES
            && self.bytes().saturating_add(entry_bytes(&entry)) <= MAX_RECORD_BYTES;
        if fits {
            self.entries.push(entry);
        } else {
            let count = self
                .more
                .entry(entry.effect.kind().to_owned())
                .or_default();
            *count = count.saturating_add(1);
        }
    }

    /// The bytes its entries take, as JSON.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.entries
            .iter()
            .map(entry_bytes)
            .fold(0, usize::saturating_add)
    }

    /// Whether it holds no effect at all, kept or counted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.more.is_empty()
    }

    /// Each kept string `holds` says holds a secret, replaced by
    /// [`WITHHELD`].
    pub fn withhold(&mut self, mut holds: impl FnMut(&str) -> bool) {
        for entry in &mut self.entries {
            entry.effect.withhold(&mut holds);
        }
    }
}

/// The bytes `entry` takes, as JSON: past any bound when it can't be
/// encoded, so it isn't kept.
fn entry_bytes(entry: &Entry) -> usize {
    serde_json::to_vec(entry).map_or(usize::MAX, |bytes| bytes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(text: &str) -> Effect {
        Effect::MessageSent {
            channel: String::from("local"),
            outcome: Sent::Sent,
            text: Some(text.to_owned()),
        }
    }

    fn wrote(path: &str) -> Effect {
        Effect::RoomWrote {
            path: path.to_owned(),
            bytes: 1,
            replaced: false,
        }
    }

    fn effects(record: &RunRecord) -> Vec<Effect> {
        record.entries.iter().map(|entry| entry.effect.clone()).collect()
    }

    #[test]
    fn an_effect_is_kept_in_order_with_when() {
        let mut record = RunRecord::new("run-abc");
        record.note(1, sent("hello"));
        record.note(2, wrote("notes/a.md"));
        assert_eq!(
            record.entries,
            [
                Entry { at: 1, effect: sent("hello") },
                Entry { at: 2, effect: wrote("notes/a.md") },
            ]
        );
        assert!(record.more.is_empty());
        assert!(!record.is_empty());
        assert!(RunRecord::new("run-abc").is_empty());
        assert_eq!(RunRecord::new("run-abc").run, "run-abc");
        let mut counted = RunRecord::new("run-abc");
        counted.more.insert(String::from("refused"), 1);
        assert!(!counted.is_empty());
    }

    #[test]
    fn strings_are_cut_to_their_bounds_at_a_character() {
        let long = "é".repeat(MAX_TEXT_BYTES); // two bytes each
        let mut record = RunRecord::new("run-abc");
        record.note(0, sent(&long));
        record.note(0, wrote(&"p".repeat(MAX_NAME_BYTES + 10)));
        record.note(
            0,
            Effect::MessageSent {
                channel: "c".repeat(MAX_NAME_BYTES + 1),
                outcome: Sent::Redacted,
                text: None,
            },
        );
        assert_eq!(
            effects(&record),
            [
                sent(&"é".repeat(MAX_TEXT_BYTES / 2)),
                wrote(&"p".repeat(MAX_NAME_BYTES)),
                Effect::MessageSent {
                    channel: "c".repeat(MAX_NAME_BYTES),
                    outcome: Sent::Redacted,
                    text: None,
                },
            ]
        );
        // A bound inside a character is moved back to its start; one at
        // the end cuts nothing.
        let mut odd = String::from("aé");
        cut(&mut odd, 2);
        assert_eq!(odd, "a");
        let mut exact = String::from("ab");
        cut(&mut exact, 2);
        assert_eq!(exact, "ab");
        let mut at_edge = String::from("abc");
        cut(&mut at_edge, 2);
        assert_eq!(at_edge, "ab");
    }

    #[test]
    fn past_its_entries_an_effect_is_counted_by_kind() {
        let mut record = RunRecord::new("run-abc");
        for at in 0..MAX_ENTRIES {
            record.note(i64::try_from(at).unwrap(), Effect::Refused { tool: ToolUse::Network });
        }
        assert_eq!(record.entries.len(), MAX_ENTRIES);
        assert!(record.more.is_empty());
        record.note(99, Effect::Refused { tool: ToolUse::Network });
        record.note(99, Effect::Refused { tool: ToolUse::Network });
        record.note(99, sent("past"));
        assert_eq!(record.entries.len(), MAX_ENTRIES);
        assert_eq!(
            record.more,
            BTreeMap::from([(String::from("message_sent"), 1), (String::from("refused"), 2)])
        );
        // The first kept are the first that happened.
        assert_eq!(record.entries.first().map(|entry| entry.at), Some(0));
        assert_eq!(record.entries.last().map(|entry| entry.at), Some(63));
    }

    #[test]
    fn past_its_bytes_an_effect_is_counted_by_kind() {
        let mut record = RunRecord::new("run-abc");
        let text = "x".repeat(MAX_TEXT_BYTES);
        let one = entry_bytes(&Entry { at: 0, effect: sent(&text) });
        let fit = MAX_RECORD_BYTES / one;
        assert!(fit < MAX_ENTRIES, "the byte bound is reached first");
        for _ in 0..fit {
            record.note(0, sent(&text));
        }
        assert_eq!((record.entries.len(), record.more.len()), (fit, 0));
        record.note(0, sent(&text));
        assert_eq!(record.entries.len(), fit);
        assert_eq!(record.more.get("message_sent"), Some(&1));
        assert_eq!(record.bytes(), fit * one);
        // One that still fits is kept after one that didn't.
        let small = Entry { at: 0, effect: Effect::Refused { tool: ToolUse::Vault } };
        assert!(record.bytes() + entry_bytes(&small) <= MAX_RECORD_BYTES);
        record.note(0, Effect::Refused { tool: ToolUse::Vault });
        assert_eq!(record.entries.len(), fit + 1);
    }

    #[test]
    fn a_string_that_holds_a_secret_is_withheld_and_only_it() {
        let mut record = RunRecord::new("run-abc");
        record.note(0, sent("the key is hunter2"));
        record.note(0, wrote("hunter2.txt"));
        record.note(0, Effect::LibraryItem { name: String::from("report.md"), bytes: 1 });
        record.note(
            0,
            Effect::LibraryFolder { name: String::from("hunter2-folder"), files: 1, bytes: 1 },
        );
        record.note(
            0,
            Effect::RoomEdited { path: String::from("edit.md"), bytes: 1, replaced: 1 },
        );
        record.note(0, Effect::ApprovalUsed { tool: ToolUse::MessageSend });
        let mut asked = Vec::new();
        record.withhold(|text| {
            asked.push(text.to_owned());
            text.contains("hunter2")
        });
        assert_eq!(
            asked,
            ["local", "the key is hunter2", "hunter2.txt", "report.md", "hunter2-folder", "edit.md"]
        );
        assert_eq!(
            effects(&record),
            [
                Effect::MessageSent {
                    channel: String::from("local"),
                    outcome: Sent::Sent,
                    text: Some(WITHHELD.to_owned()),
                },
                wrote(WITHHELD),
                Effect::LibraryItem { name: String::from("report.md"), bytes: 1 },
                Effect::LibraryFolder { name: WITHHELD.to_owned(), files: 1, bytes: 1 },
                Effect::RoomEdited { path: String::from("edit.md"), bytes: 1, replaced: 1 },
                Effect::ApprovalUsed { tool: ToolUse::MessageSend },
            ]
        );
    }

    #[test]
    fn it_reads_back_as_it_was_written_and_refuses_what_it_never_holds() {
        let mut record = RunRecord::new("run-abc");
        record.note(7, sent("hi"));
        record.note(8, Effect::ApprovalAsked { tool: ToolUse::MemoryPropose });
        record.note(9, Effect::ApprovalUsed { tool: ToolUse::MessageSend });
        record.spend = Spend { model_calls: 3, input_tokens: 100, output_tokens: 20 };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains(r#""kind":"message_sent""#), "{json}");
        assert_eq!(serde_json::from_str::<RunRecord>(&json).unwrap(), record);
        for unknown in [
            json.replace(r#""run":"#, r#""said":"x","run":"#),
            json.replace("message_sent", "money_moved"),
            json.replace(r#""at":7,"#, r#""at":7,"by":"agent","#),
        ] {
            assert!(serde_json::from_str::<RunRecord>(&unknown).is_err(), "{unknown}");
        }
    }

    #[test]
    fn every_kind_is_named() {
        let kinds = [
            sent("a").kind(),
            wrote("a").kind(),
            Effect::RoomEdited { path: String::new(), bytes: 0, replaced: 0 }.kind(),
            Effect::LibraryItem { name: String::new(), bytes: 0 }.kind(),
            Effect::LibraryFolder { name: String::new(), files: 0, bytes: 0 }.kind(),
            Effect::ApprovalAsked { tool: ToolUse::Vault }.kind(),
            Effect::ApprovalUsed { tool: ToolUse::Vault }.kind(),
            Effect::Refused { tool: ToolUse::Vault }.kind(),
        ];
        assert_eq!(
            kinds,
            [
                "message_sent",
                "room_wrote",
                "room_edited",
                "library_item",
                "library_folder",
                "approval_asked",
                "approval_used",
                "refused"
            ]
        );
    }
}
