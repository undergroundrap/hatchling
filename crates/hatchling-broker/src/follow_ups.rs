//! The owner's further messages, for the chat run in flight.
//!
//! The chat answers one message at a time, and what the owner sends
//! while a run is working waits in the local hub's queue. A run used to
//! see none of it: each waiting message became a run of its own, after
//! the answer to the last. A chat run can now ask, between its steps,
//! for what the owner has sent since ([`hatchling_core::ChatOp::More`]),
//! and take it into the task it is on.
//!
//! Only the chat's run in flight is given anything, and only messages
//! sent in the conversation its own message was sent in. The chat says
//! which run that is, by its cell ([`began`]): a scheduled run, a
//! subagent, and a chat run that has ended are given nothing. What is
//! given is taken off the queue, so no run is started for it, and is
//! remembered until the run ends ([`ended`]), when the chat keeps it as
//! part of what the owner said in that exchange.
//!
//! A reply carries at most [`CHAT_MORE_BYTES`] of messages as JSON writes
//! them, so it always fits a frame: a message that doesn't fit in what is
//! left waits for the next asking, and one too long for any reply waits
//! for a run of its own.

use std::sync::{Mutex, MutexGuard, PoisonError};

use hatchling_channel::LocalHub;
use hatchling_core::CHAT_MORE_BYTES;

/// The most runs in flight remembered. The chat has one at a time, and
/// forgets it when it ends: past this, the oldest is dropped.
pub const MAX_FLIGHTS: usize = 8;

/// A chat run in flight.
struct Flight {
    /// The run's cell.
    cell: String,
    /// Where the owner's messages wait.
    hub: LocalHub,
    /// The conversation the run's own message was sent in.
    sent_in: u64,
    /// What the run was given, oldest first.
    given: Vec<String>,
}

/// The chat runs in flight, oldest first.
pub struct FollowUps {
    flights: Vec<Flight>,
}

impl Default for FollowUps {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for FollowUps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FollowUps")
            .field("flights", &self.flights.len())
            .finish()
    }
}

impl FollowUps {
    /// None in flight.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            flights: Vec::new(),
        }
    }

    /// The chat started a run in `cell` for a message sent in the
    /// conversation `sent_in`, and the owner's messages wait in `hub`.
    pub fn began(&mut self, cell: &str, hub: &LocalHub, sent_in: u64) {
        self.flights.retain(|flight| flight.cell != cell);
        if self.flights.len() >= MAX_FLIGHTS {
            self.flights.remove(0);
        }
        self.flights.push(Flight {
            cell: cell.to_owned(),
            hub: hub.clone(),
            sent_in,
            given: Vec::new(),
        });
    }

    /// What the owner has sent since the message `cell`'s run was started
    /// for, oldest first, taken off the queue. Nothing for a cell that
    /// isn't a chat run in flight.
    pub fn more(&mut self, cell: &str) -> Vec<String> {
        let Some(flight) = self.flights.iter_mut().find(|flight| flight.cell == cell) else {
            return Vec::new();
        };
        let mut room = CHAT_MORE_BYTES;
        let mut now = Vec::new();
        loop {
            let mut size = 0;
            let fits = |message: &hatchling_channel::Message, sent_in: u64| {
                size = written(&message.text);
                sent_in == flight.sent_in && size <= room
            };
            let Some(message) = flight.hub.take_if(fits) else {
                break;
            };
            room -= size;
            now.push(message.text);
        }
        flight.given.extend(now.iter().cloned());
        now
    }

    /// Whether `cell`'s run is the chat's in flight, and its own message
    /// was sent in the conversation open now: `None` when it isn't a chat
    /// run in flight.
    #[must_use]
    pub fn open_for(&self, cell: &str) -> Option<bool> {
        self.flights
            .iter()
            .find(|flight| flight.cell == cell)
            .map(|flight| flight.sent_in == flight.hub.conversation())
    }

    /// Whether any chat run is in flight.
    #[must_use]
    pub fn any(&self) -> bool {
        !self.flights.is_empty()
    }

    /// The cells of the runs in flight for the chat whose messages wait in
    /// `hub`.
    fn cells_of(&self, hub: &LocalHub) -> Vec<String> {
        self.flights
            .iter()
            .filter(|flight| flight.hub.is(hub))
            .map(|flight| flight.cell.clone())
            .collect()
    }

    /// `cell`'s run ended: everything it was given, oldest first. It is
    /// given nothing more.
    pub fn ended(&mut self, cell: &str) -> Vec<String> {
        let Some(at) = self.flights.iter().position(|flight| flight.cell == cell) else {
            return Vec::new();
        };
        self.flights.remove(at).given
    }
}

/// The bytes JSON writes `text` in, quotes and escapes and all.
fn written(text: &str) -> usize {
    serde_json::to_string(text).map_or(usize::MAX, |json| json.len())
}

/// The broker's own.
static SHARED: Mutex<FollowUps> = Mutex::new(FollowUps::new());

fn shared() -> MutexGuard<'static, FollowUps> {
    SHARED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`FollowUps::began`], of the broker's own.
pub fn began(cell: &str, hub: &LocalHub, sent_in: u64) {
    shared().began(cell, hub, sent_in);
}

/// [`FollowUps::more`], of the broker's own.
#[must_use]
pub fn more(cell: &str) -> Vec<String> {
    shared().more(cell)
}

/// [`FollowUps::open_for`], of the broker's own.
#[must_use]
pub fn open_for(cell: &str) -> Option<bool> {
    shared().open_for(cell)
}

/// Stops the chat's runs in flight, the chat whose messages wait in `hub`:
/// each one's whole tree, there and then (WO-051). Whether one was running
/// to stop.
#[must_use]
pub fn stop(hub: &LocalHub) -> bool {
    let cells = shared().cells_of(hub);
    // Each one stopped, not just up to the first.
    let stopped = cells
        .iter()
        .filter(|cell| crate::run_launcher::stop_chat_run(cell))
        .count();
    stopped > 0
}

/// [`FollowUps::any`], of the broker's own: background work waits while
/// this is so (WO-054).
#[must_use]
pub fn any_in_flight() -> bool {
    shared().any()
}

/// [`FollowUps::ended`], of the broker's own.
#[must_use]
pub fn ended(cell: &str) -> Vec<String> {
    shared().ended(cell)
}

#[cfg(test)]
mod tests {
    use hatchling_channel::{ChannelId, Message, OutputGate, SenderId};

    use super::*;

    fn hub() -> LocalHub {
        LocalHub::new(OutputGate::disabled())
    }

    fn send(hub: &LocalHub, text: &str) {
        let message = Message {
            sender: SenderId(String::from("owner")),
            channel: ChannelId::local(),
            text: text.to_owned(),
        };
        assert_eq!(
            hub.inject(message),
            hatchling_channel::IngressEvent::Queued,
            "{text}"
        );
    }

    /// A run in flight is in the open conversation until the owner starts
    /// a new one; a run that isn't in flight, or ended, is no chat run.
    #[test]
    fn a_run_is_in_the_open_conversation_until_a_new_one_starts() {
        let hub = hub();
        let mut flights = FollowUps::new();
        assert_eq!(flights.open_for("cell-a"), None);
        assert!(!flights.any());
        flights.began("cell-a", &hub, hub.conversation());
        assert!(flights.any());
        assert_eq!(flights.open_for("cell-a"), Some(true));
        assert_eq!(flights.open_for("cell-b"), None);
        hub.new_conversation();
        assert_eq!(flights.open_for("cell-a"), Some(false));
        flights.began("cell-b", &hub, hub.conversation());
        assert_eq!(flights.open_for("cell-b"), Some(true));
        let _ = flights.ended("cell-a");
        assert_eq!(flights.open_for("cell-a"), None);
        assert!(flights.any(), "cell-b is still in flight");
        let _ = flights.ended("cell-b");
        assert!(!flights.any());
    }

    /// A chat's runs in flight are its own: another hub's, or one ended,
    /// aren't among them (WO-051).
    #[test]
    fn a_chats_runs_in_flight_are_its_own() {
        let (hub, other) = (hub(), hub());
        assert!(hub.is(&hub.clone()) && !hub.is(&other));
        let mut flights = FollowUps::new();
        flights.began("cell-a", &hub, 0);
        flights.began("cell-b", &other, 0);
        flights.began("cell-c", &hub, 0);
        assert_eq!(flights.cells_of(&hub), ["cell-a", "cell-c"]);
        let _ = flights.ended("cell-a");
        assert_eq!(flights.cells_of(&hub), ["cell-c"]);
        assert_eq!(flights.cells_of(&other), ["cell-b"]);
    }

    /// A run in flight is given what the owner sent, oldest first and
    /// once, and it is off the queue; when the run ends, all it was given
    /// is handed over, and it is given no more.
    #[test]
    fn a_run_in_flight_is_given_what_the_owner_sent_since() {
        let hub = hub();
        let mut flights = FollowUps::new();
        flights.began("cell-a", &hub, 0);
        assert!(flights.more("cell-a").is_empty(), "nothing was sent");
        send(&hub, "and make it blue");
        send(&hub, "with a border");
        assert_eq!(
            flights.more("cell-a"),
            ["and make it blue", "with a border"]
        );
        assert_eq!(hub.queued(), 0, "what was given is off the queue");
        assert!(flights.more("cell-a").is_empty(), "given once");
        send(&hub, "thanks");
        assert_eq!(flights.more("cell-a"), ["thanks"]);
        assert_eq!(
            flights.ended("cell-a"),
            ["and make it blue", "with a border", "thanks"]
        );
        send(&hub, "one more thing");
        assert!(flights.more("cell-a").is_empty(), "the run has ended");
        assert_eq!(hub.queued(), 1, "it waits for a run of its own");
        assert!(flights.ended("cell-a").is_empty(), "handed over once");
    }

    /// Only the run the chat named is given anything: another cell gets
    /// nothing, and the message stays for the run it is for.
    #[test]
    fn another_cell_is_given_nothing() {
        let hub = hub();
        let mut flights = FollowUps::new();
        flights.began("cell-a", &hub, 0);
        send(&hub, "for the chat's run");
        assert!(flights.more("cell-b").is_empty());
        assert!(flights.more("").is_empty());
        assert_eq!(hub.queued(), 1);
        assert!(flights.ended("cell-b").is_empty());
        assert_eq!(flights.more("cell-a"), ["for the chat's run"]);
    }

    /// A message sent in another conversation isn't the run's: it stays
    /// first in the queue, and what is behind it stays too.
    #[test]
    fn a_message_of_another_conversation_is_left_where_it_is() {
        let hub = hub();
        let mut flights = FollowUps::new();
        flights.began("cell-a", &hub, 0);
        send(&hub, "in the run's conversation");
        hub.new_conversation();
        send(&hub, "in a new conversation");
        assert_eq!(flights.more("cell-a"), ["in the run's conversation"]);
        assert!(flights.more("cell-a").is_empty());
        assert_eq!(hub.queued(), 1);
        // A run started in the new conversation is given it.
        flights.began("cell-b", &hub, 1);
        assert_eq!(flights.more("cell-b"), ["in a new conversation"]);
    }

    /// A reply's messages fit a frame as JSON writes them: one that
    /// doesn't fit in what is left waits for the next asking, and one too
    /// long for any reply is never given.
    #[test]
    fn what_is_given_at_once_fits_a_frame() {
        let hub = hub();
        let mut flights = FollowUps::new();
        flights.began("cell-a", &hub, 0);
        // Each is 16000 bytes, and 16002 as JSON writes it.
        let long = |letter: &str| letter.repeat(16_000);
        for letter in ["a", "b", "c", "d"] {
            send(&hub, &long(letter));
        }
        let first = flights.more("cell-a");
        assert_eq!(first, [long("a"), long("b"), long("c")]);
        let reply = hatchling_core::SessionResponse::Chat(hatchling_core::ChatResult::More {
            messages: first,
        });
        let frame = serde_json::to_vec(&reply).unwrap();
        assert!(
            frame.len() <= hatchling_core::framing::MAX_FRAME_BYTES,
            "{}",
            frame.len()
        );
        assert_eq!(flights.more("cell-a"), [long("d")], "the next asking");

        // A message is measured as JSON writes it.
        assert_eq!(written("ab"), 4);
        assert_eq!(written("a\nb"), 6, "an escape is counted as written");
        // Sixteen thousand line ends are 32002 bytes as JSON: two don't
        // fit one reply, and each fits its own.
        let lines = "\n".repeat(16_000);
        send(&hub, &lines);
        send(&hub, &lines);
        assert_eq!(flights.more("cell-a"), [lines.as_str()]);
        assert_eq!(flights.more("cell-a"), [lines.as_str()]);
    }

    /// A run begun again starts with nothing given; and past the bound
    /// the oldest run in flight is forgotten.
    #[test]
    fn the_runs_in_flight_are_bounded() {
        let hub = hub();
        let mut flights = FollowUps::new();
        flights.began("cell-0", &hub, 0);
        send(&hub, "first");
        assert_eq!(flights.more("cell-0"), ["first"]);
        flights.began("cell-0", &hub, 0);
        assert!(
            flights.ended("cell-0").is_empty(),
            "begun again, given none"
        );

        flights.began("cell-0", &hub, 0);
        for n in 1..=MAX_FLIGHTS {
            flights.began(&format!("cell-{n}"), &hub, 0);
        }
        send(&hub, "second");
        assert!(flights.more("cell-0").is_empty(), "the oldest was dropped");
        assert_eq!(flights.more("cell-1"), ["second"], "the next oldest wasn't");
        assert_eq!(format!("{flights:?}"), "FollowUps { flights: 8 }");
    }

    /// The broker's own is one for the process: a cell of its own in each
    /// test keeps them apart.
    #[test]
    fn the_brokers_own_is_asked_by_cell() {
        let hub = hub();
        began("follow-ups-own-cell", &hub, 0);
        send(&hub, "to the broker's own");
        assert!(more("follow-ups-other-cell").is_empty());
        assert_eq!(more("follow-ups-own-cell"), ["to the broker's own"]);
        assert_eq!(ended("follow-ups-own-cell"), ["to the broker's own"]);
        assert!(ended("follow-ups-own-cell").is_empty());
    }
}
