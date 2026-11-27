//! The run record's journal (WO-053).
//!
//! Each effect a run has outside itself is noted by the broker as the
//! session serves it, never by the agent, and handed on when the session
//! ends, for the chat to keep with the exchange the run answered.
//!
//! The session loop notes each request's answer ([`Journal::note`]). What
//! the journal needs of the request is taken before the request is handled
//! ([`asked`]), since handling it uses it up, the way the run trail takes
//! its tool. A record is checked against the vault before it's held: a
//! string that holds a secret is kept as withheld, and one that can't be
//! checked is withheld too ([`publish`]).

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use hatchling_core::messages::{
    ChannelOp, ChannelResult, RoomResult, SessionRequest, SessionResponse,
};
use hatchling_core::run_record::{Effect, RunRecord, Sent, Spend};
use hatchling_core::subagent::ToolUse;

use crate::run_trail::{Ending, ending_of};

/// The most records held for the chat to take.
///
/// A chat run's is taken as its exchange is kept, moments after its session
/// ends; one never taken (a run whose answer wasn't kept) is let go once
/// this many came after.
pub const MAX_HELD: usize = 16;

/// The records of sessions that ended, oldest first, until the chat takes
/// them.
static HELD: Mutex<Held> = Mutex::new(Held(VecDeque::new()));

/// Records held for the chat, oldest first, at most [`MAX_HELD`].
#[derive(Debug, Default)]
struct Held(VecDeque<RunRecord>);

impl Held {
    /// Holds `record`, in place of one its run had: the oldest let go to
    /// make room.
    fn hold(&mut self, record: RunRecord) {
        self.0.retain(|kept| kept.run != record.run);
        // One comes in at a time, so one going is room enough.
        if self.0.len() >= MAX_HELD {
            self.0.pop_front();
        }
        self.0.push_back(record);
    }

    /// The record of the run `run`, taken: none if it isn't held.
    fn take(&mut self, run: &str) -> Option<RunRecord> {
        let at = self.0.iter().position(|kept| kept.run == run)?;
        self.0.remove(at)
    }
}

/// What the journal needs of a request once it's answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// A message to send: where, what, and whether the owner's approval
    /// came with it.
    Send {
        /// The channel.
        channel: String,
        /// The text.
        text: String,
        /// Whether a rail approval was presented.
        approved: bool,
    },
    /// Anything else: its answer says all the journal keeps.
    Other,
}

/// What of `request` the journal will need once it's answered.
#[must_use]
pub fn asked(request: &SessionRequest) -> Asked {
    if let SessionRequest::Channel(ChannelOp::Send {
        channel,
        text,
        approval,
        ..
    }) = request
    {
        Asked::Send {
            channel: channel.clone(),
            text: text.clone(),
            approved: approval.is_some(),
        }
    } else {
        Asked::Other
    }
}

/// The effects a request, `asked` of it and used as `tool`, had once
/// answered with `response`.
fn effects(asked: Asked, tool: Option<ToolUse>, response: &SessionResponse) -> Vec<Effect> {
    if let (
        Asked::Send {
            channel,
            text,
            approved,
        },
        SessionResponse::Channel(result),
    ) = (asked, response)
    {
        match result {
            ChannelResult::Sent => {
                let mut sent = vec![Effect::MessageSent {
                    channel,
                    outcome: Sent::Sent,
                    text: Some(text),
                }];
                if approved {
                    sent.push(Effect::ApprovalUsed {
                        tool: ToolUse::MessageSend,
                    });
                }
                return sent;
            }
            ChannelResult::Redacted { .. } => {
                return vec![Effect::MessageSent {
                    channel,
                    outcome: Sent::Redacted,
                    text: None,
                }];
            }
            ChannelResult::Message { .. }
            | ChannelResult::NoMessage
            | ChannelResult::Denied
            | ChannelResult::ChannelError
            | ChannelResult::Accounts { .. }
            | ChannelResult::UnknownAccount => {}
        }
    }
    if let SessionResponse::Room(result) = response {
        if let Some(effect) = room_effect(result) {
            return vec![effect];
        }
    }
    match (tool, ending_of(response)) {
        (Some(tool), Ending::Asked) => vec![Effect::ApprovalAsked { tool }],
        (Some(tool), Ending::Refused) => vec![Effect::Refused { tool }],
        (None, _) | (Some(_), Ending::Done | Ending::Failed) => Vec::new(),
    }
}

/// The effect a Room answer says happened outside the run: a file written
/// or edited in the Room, or handed to the owner's Library.
fn room_effect(result: &RoomResult) -> Option<Effect> {
    if let RoomResult::Wrote {
        path,
        bytes,
        replaced,
    } = result
    {
        return Some(Effect::RoomWrote {
            path: path.clone(),
            bytes: *bytes,
            replaced: *replaced,
        });
    }
    if let RoomResult::Edited {
        path,
        replaced,
        bytes,
        ..
    } = result
    {
        return Some(Effect::RoomEdited {
            path: path.clone(),
            bytes: *bytes,
            replaced: *replaced,
        });
    }
    if let RoomResult::HandedOver { name, bytes } = result {
        return Some(Effect::LibraryItem {
            name: name.clone(),
            bytes: *bytes,
        });
    }
    if let RoomResult::FolderHandedOver {
        name, files, bytes, ..
    } = result
    {
        return Some(Effect::LibraryFolder {
            name: name.clone(),
            files: *files,
            bytes: *bytes,
        });
    }
    None
}

/// A session's journal: the record of its run, as it's served.
#[derive(Debug)]
pub struct Journal {
    record: RunRecord,
}

impl Journal {
    /// A journal for the run `run`.
    #[must_use]
    pub fn new(run: &str) -> Self {
        Self {
            record: RunRecord::new(run),
        }
    }

    /// Notes what a request, `asked` of it and used as `tool`, did once
    /// answered with `response`, at `at`.
    pub fn note(
        &mut self,
        asked: Asked,
        tool: Option<ToolUse>,
        response: &SessionResponse,
        at: i64,
    ) {
        for effect in effects(asked, tool, response) {
            self.record.note(at, effect);
        }
    }

    /// The record, with what the run spent.
    #[must_use]
    pub fn finish(mut self, spend: Spend) -> RunRecord {
        self.record.spend = spend;
        self.record
    }
}

/// `record`, each string `holds` says holds a secret, or can't say,
/// replaced by the withheld mark. The whole record is checked once first;
/// only when that finds one, or can't say, is each string checked, so that
/// it alone is withheld.
fn checked(mut record: RunRecord, mut holds: impl FnMut(&str) -> Option<bool>) -> RunRecord {
    let texts = record.texts();
    if !texts.is_empty() && holds(&texts) != Some(false) {
        record.withhold(|text| holds(text) != Some(false));
    }
    record
}

/// Holds `record` for the chat to take, [`checked`] with `holds` first.
pub fn publish(record: RunRecord, holds: impl FnMut(&str) -> Option<bool>) {
    let record = checked(record, holds);
    HELD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .hold(record);
}

/// The record of the run `run`, taken: none if it wasn't held.
#[must_use]
pub fn take(run: &str) -> Option<RunRecord> {
    HELD.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take(run)
}

#[cfg(test)]
mod tests {
    use hatchling_core::messages::{LeftOut, MemoryOp, MemoryResult, WebResult};
    use hatchling_core::run_record::WITHHELD;

    use super::*;

    const RUN: &str = "run-0123456789abcdef01234567";

    fn send(text: &str, approval: Option<&str>) -> SessionRequest {
        SessionRequest::Channel(ChannelOp::Send {
            channel: String::from("local"),
            text: text.to_owned(),
            state_changing: false,
            approval: approval.map(str::to_owned),
            account: None,
        })
    }

    /// The effects noted for a request `asked` of, used as `tool`, answered
    /// with `response`: each at the time it was noted.
    fn noted(asked: Asked, tool: Option<ToolUse>, response: &SessionResponse) -> Vec<Effect> {
        let mut journal = Journal::new(RUN);
        journal.note(asked, tool, response, 7);
        let record = journal.finish(Spend::default());
        assert!(record.entries.iter().all(|entry| entry.at == 7));
        record
            .entries
            .into_iter()
            .map(|entry| entry.effect)
            .collect()
    }

    /// As [`noted`], for `request` itself, its tool as the trail names it.
    fn noted_for(request: &SessionRequest, response: &SessionResponse) -> Vec<Effect> {
        noted(asked(request), crate::run_trail::tool_of(request), response)
    }

    fn message(outcome: Sent, text: Option<&str>) -> Effect {
        Effect::MessageSent {
            channel: String::from("local"),
            outcome,
            text: text.map(str::to_owned),
        }
    }

    #[test]
    fn a_send_is_asked_with_its_channel_text_and_approval() {
        assert_eq!(
            asked(&send("hi", Some("rail-v1-x"))),
            Asked::Send {
                channel: String::from("local"),
                text: String::from("hi"),
                approved: true,
            }
        );
        assert_eq!(
            asked(&send("hi", None)),
            Asked::Send {
                channel: String::from("local"),
                text: String::from("hi"),
                approved: false,
            }
        );
        let receive = SessionRequest::Channel(ChannelOp::Receive {
            channel: String::from("local"),
            account: None,
        });
        assert_eq!(asked(&receive), Asked::Other);
    }

    #[test]
    fn a_message_is_noted_as_it_ended() {
        let sent = SessionResponse::Channel(ChannelResult::Sent);
        assert_eq!(
            noted_for(&send("hello", None), &sent),
            [message(Sent::Sent, Some("hello"))]
        );
        assert_eq!(
            noted_for(&send("hello", Some("rail-v1-x")), &sent),
            [
                message(Sent::Sent, Some("hello")),
                Effect::ApprovalUsed {
                    tool: ToolUse::MessageSend
                },
            ]
        );
        let redacted = SessionResponse::Channel(ChannelResult::Redacted {
            category: String::from("secret"),
            event_id: String::from("e1"),
        });
        assert_eq!(
            noted_for(&send("hunter2", None), &redacted),
            [message(Sent::Redacted, None)]
        );
        assert_eq!(
            noted_for(
                &send("hello", None),
                &SessionResponse::Channel(ChannelResult::Denied)
            ),
            [Effect::Refused {
                tool: ToolUse::MessageSend
            }]
        );
        for nothing in [
            ChannelResult::ChannelError,
            ChannelResult::NoMessage,
            ChannelResult::UnknownAccount,
        ] {
            assert_eq!(
                noted_for(&send("hello", None), &SessionResponse::Channel(nothing)),
                []
            );
        }
        // A message is noted only from a send: a send's answer without the
        // send asked of it says nothing was sent.
        assert_eq!(noted(Asked::Other, Some(ToolUse::MessageSend), &sent), []);
    }

    #[test]
    fn a_rooms_writes_and_hand_overs_are_noted() {
        let cases = [
            (
                RoomResult::Wrote {
                    path: String::from("a.md"),
                    bytes: 3,
                    replaced: true,
                },
                Effect::RoomWrote {
                    path: String::from("a.md"),
                    bytes: 3,
                    replaced: true,
                },
            ),
            (
                RoomResult::Edited {
                    path: String::from("b.md"),
                    replaced: 2,
                    line: 9,
                    bytes: 5,
                },
                Effect::RoomEdited {
                    path: String::from("b.md"),
                    bytes: 5,
                    replaced: 2,
                },
            ),
            (
                RoomResult::HandedOver {
                    name: String::from("c.pdf"),
                    bytes: 8,
                },
                Effect::LibraryItem {
                    name: String::from("c.pdf"),
                    bytes: 8,
                },
            ),
            (
                RoomResult::FolderHandedOver {
                    name: String::from("d"),
                    files: 4,
                    bytes: 16,
                    left_out: LeftOut::default(),
                },
                Effect::LibraryFolder {
                    name: String::from("d"),
                    files: 4,
                    bytes: 16,
                },
            ),
        ];
        for (result, effect) in cases {
            assert_eq!(
                noted(
                    Asked::Other,
                    Some(ToolUse::Room),
                    &SessionResponse::Room(result)
                ),
                [effect]
            );
        }
        let refused = SessionResponse::Room(RoomResult::Refused {
            reason: String::from("no"),
        });
        assert_eq!(
            noted(Asked::Other, Some(ToolUse::Room), &refused),
            [Effect::Refused {
                tool: ToolUse::Room
            }]
        );
    }

    #[test]
    fn an_approval_asked_and_a_refusal_are_noted_by_the_tool() {
        let propose = SessionRequest::Memory(MemoryOp::Propose {
            content: String::from("x"),
            memory_kind: None,
            why: None,
            how_to_apply: None,
            check_by: None,
            absence: false,
            supersedes: None,
        });
        assert_eq!(
            noted_for(&propose, &SessionResponse::Memory(MemoryResult::Proposed)),
            [Effect::ApprovalAsked {
                tool: ToolUse::MemoryPropose
            }]
        );
        assert_eq!(
            noted_for(&propose, &SessionResponse::Memory(MemoryResult::Denied)),
            [Effect::Refused {
                tool: ToolUse::MemoryPropose
            }]
        );
        for nothing in [MemoryResult::MemoryError, MemoryResult::Stored] {
            assert_eq!(noted_for(&propose, &SessionResponse::Memory(nothing)), []);
        }
        // A use the trail names no tool for (a model call, the chat's own
        // asking) has no entry, refused or not.
        let denied = SessionResponse::Web(WebResult::Denied {
            site: String::from("example.com"),
        });
        assert_eq!(noted(Asked::Other, None, &denied), []);
        assert_eq!(
            noted(Asked::Other, Some(ToolUse::WebPage), &denied),
            [Effect::Refused {
                tool: ToolUse::WebPage
            }]
        );
    }

    #[test]
    fn a_finished_record_carries_its_run_and_spend() {
        let spend = Spend {
            model_calls: 2,
            input_tokens: 30,
            output_tokens: 4,
        };
        let record = Journal::new(RUN).finish(spend);
        assert_eq!((record.run.as_str(), record.spend), (RUN, spend));
        assert!(record.is_empty());
    }

    fn record(run: &str, names: &[&str]) -> RunRecord {
        let mut record = RunRecord::new(run);
        for name in names {
            record.note(
                0,
                Effect::LibraryItem {
                    name: (*name).to_owned(),
                    bytes: 0,
                },
            );
        }
        record
    }

    #[test]
    fn a_record_is_checked_whole_then_each_string_only_on_a_hit() {
        let mut asked = Vec::new();
        let kept = checked(record(RUN, &["a.md", "b.md"]), |text| {
            asked.push(text.to_owned());
            Some(false)
        });
        assert_eq!(asked, ["a.md\nb.md"]);
        assert_eq!(kept, record(RUN, &["a.md", "b.md"]));

        let mut asked = Vec::new();
        let kept = checked(record(RUN, &["a.md", "hunter2.md"]), |text| {
            asked.push(text.to_owned());
            Some(text.contains("hunter2"))
        });
        assert_eq!(asked, ["a.md\nhunter2.md", "a.md", "hunter2.md"]);
        assert_eq!(kept, record(RUN, &["a.md", WITHHELD]));
    }

    #[test]
    fn a_string_that_cant_be_checked_is_withheld() {
        let kept = checked(record(RUN, &["a.md", "b.md"]), |text| {
            (!text.contains('\n')).then_some(text == "a.md")
        });
        // The whole couldn't be checked; "a.md" held one, and "b.md" was
        // checked and held none.
        assert_eq!(kept, record(RUN, &[WITHHELD, "b.md"]));
        let kept = checked(record(RUN, &["a.md"]), |_| None);
        assert_eq!(kept, record(RUN, &[WITHHELD]));
        // One with nothing to check isn't asked.
        let mut asked = 0;
        let kept = checked(RunRecord::new(RUN), |_| {
            asked += 1;
            None
        });
        assert_eq!((asked, kept), (0, RunRecord::new(RUN)));
    }

    #[test]
    fn a_record_is_taken_once_and_the_oldest_are_let_go() {
        let run = |at: usize| format!("run-bbbbbbbbbbbbbbbbbbbbbb{at:02}");
        let mut held = Held::default();
        for at in 0..=MAX_HELD {
            held.hold(RunRecord::new(&run(at)));
        }
        assert_eq!(held.0.len(), MAX_HELD);
        // The first is gone: as many came after it as are held.
        assert_eq!(held.take(&run(0)), None);
        assert_eq!(held.take(&run(1)).map(|kept| kept.run), Some(run(1)));
        assert_eq!(held.take(&run(1)), None, "taken once");
        // A run's record held again replaces its last, and frees no other.
        let again = record(&run(2), &["again.md"]);
        held.hold(again.clone());
        assert_eq!(held.0.len(), MAX_HELD - 1);
        assert_eq!(held.take(&run(2)), Some(again));
        for at in 3..=MAX_HELD {
            assert!(held.take(&run(at)).is_some(), "{at}");
        }
        assert!(held.0.is_empty());
    }

    #[test]
    fn a_published_record_is_checked_then_taken_by_its_run() {
        let run = "run-cccccccccccccccccccccc01";
        publish(record(run, &["hunter2.md"]), |text| {
            Some(text.contains("hunter2"))
        });
        assert_eq!(take(run), Some(record(run, &[WITHHELD])));
        assert_eq!(take(run), None);
    }
}
