//! The local API's event stream: what `Subscribe` replays and
//! then follows.
//!
//! A [`Bus`] holds the retained events, numbered in arrival order. Two
//! things feed it: the activity log ([`Follower`]), whose every line is
//! one activity event (a subagent's lifecycle, or an API call's audit
//! record), and the agent's replies on the local channel, pushed as chat
//! messages when they arrive.
//!
//! Sequence numbers start from the broker's start time, in microseconds,
//! so a number from a later broker is always larger than any from an
//! earlier one: a client resuming across a restart misses nothing, and
//! at worst sees the retained activity again (each activity event carries
//! its own `event_id`, which is how a client tells a repeat).

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hatchling_core::subagent::ActivityEvent;
use hatchling_core::ui::{UiEvent, UiEventBody, activity_state};

/// Most events the bus retains. Past it the oldest are dropped: a client
/// resuming from before them starts at the oldest still held.
pub const MAX_RETAINED: usize = 4096;

/// How often the follower looks for new activity.
pub const FOLLOW_POLL: Duration = Duration::from_millis(250);

/// The retained events and the next number to give.
#[derive(Debug)]
struct Retained {
    next: u64,
    events: VecDeque<UiEvent>,
}

/// The API's event stream.
#[derive(Debug)]
pub struct Bus {
    state: Mutex<Retained>,
    arrived: Condvar,
}

impl Bus {
    /// An empty bus whose first event is numbered `first`.
    #[must_use]
    pub const fn starting_at(first: u64) -> Self {
        Self {
            state: Mutex::new(Retained {
                next: first,
                events: VecDeque::new(),
            }),
            arrived: Condvar::new(),
        }
    }

    /// An empty bus numbered from now: the microseconds since the epoch.
    #[must_use]
    pub fn starting_now() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |since| {
                u64::try_from(since.as_micros()).unwrap_or(u64::MAX)
            });
        Self::starting_at(now)
    }

    fn state(&self) -> MutexGuard<'_, Retained> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds an event with `body` at `ts`, and returns its number.
    pub fn push(&self, ts: String, body: UiEventBody) -> u64 {
        let mut state = self.state();
        let seq = state.next;
        state.next = seq.saturating_add(1);
        state.events.push_back(UiEvent { seq, ts, body });
        if state.events.len() > MAX_RETAINED {
            state.events.pop_front();
        }
        drop(state);
        self.arrived.notify_all();
        seq
    }

    /// Adds `event` as activity, with its feed state.
    pub fn push_activity(&self, event: ActivityEvent) -> u64 {
        let state = activity_state(&event.kind);
        self.push(event.ts.clone(), UiEventBody::Activity { event, state })
    }

    /// The retained events numbered after `seq` (all of them for `None`).
    #[must_use]
    pub fn after(&self, seq: Option<u64>) -> Vec<UiEvent> {
        self.state()
            .events
            .iter()
            .filter(|event| seq.is_none_or(|seq| event.seq > seq))
            .cloned()
            .collect()
    }

    /// The events after `seq`, waiting up to `wait` for one to arrive
    /// when there are none yet.
    #[must_use]
    pub fn wait_after(&self, seq: Option<u64>, wait: Duration) -> Vec<UiEvent> {
        // Waits while nothing is held after the mark.
        let waiting = |state: &mut Retained| match seq {
            Some(seq) => state.events.back().is_none_or(|last| last.seq <= seq),
            None => state.events.is_empty(),
        };
        drop(self.arrived.wait_timeout_while(self.state(), wait, waiting));
        self.after(seq)
    }
}

/// Follows the activity log: each complete line read since the last
/// look becomes one event on the bus.
#[derive(Debug)]
pub struct Follower {
    log: PathBuf,
    offset: u64,
    partial: Vec<u8>,
}

impl Follower {
    /// A follower of `log` from its start.
    #[must_use]
    pub const fn new(log: PathBuf) -> Self {
        Self {
            log,
            offset: 0,
            partial: Vec::new(),
        }
    }

    /// The log followed.
    #[must_use]
    pub fn log(&self) -> &Path {
        &self.log
    }

    /// Reads what the log gained since the last look and pushes each
    /// complete line onto `bus`; returns how many events it pushed.
    ///
    /// A line still being written waits for its newline. A line that is
    /// not an activity event is skipped, and logged. A log that shrank
    /// was replaced, and is read again from its start.
    pub fn poll(&mut self, bus: &Bus) -> usize {
        let Ok(mut file) = File::open(&self.log) else {
            return 0;
        };
        let length = file.metadata().map_or(0, |meta| meta.len());
        if length < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        let mut grown = Vec::new();
        let read = file
            .seek(SeekFrom::Start(self.offset))
            .and_then(|_| file.read_to_end(&mut grown));
        let Ok(count) = read else {
            return 0;
        };
        self.offset = self
            .offset
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        self.partial.extend_from_slice(&grown);
        let mut pushed = 0;
        while let Some(end) = self.partial.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            let text = line.strip_suffix(b"\n").unwrap_or(&line);
            if text.is_empty() {
                continue;
            }
            match serde_json::from_slice::<ActivityEvent>(text) {
                Ok(event) => {
                    bus.push_activity(event);
                    pushed += 1;
                }
                Err(error) => {
                    tracing::warn!(%error, log = %self.log.display(), "activity log line is not an activity event: skipped");
                }
            }
        }
        pushed
    }

    /// Follows the log forever, looking every `poll`.
    pub fn follow(&mut self, bus: &Bus, poll: Duration) -> ! {
        loop {
            self.poll(bus);
            std::thread::sleep(poll);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use hatchling_core::subagent::SubagentEvent;
    use hatchling_core::ui::{ActivityState, ApiOutcome, ApiRequest, ChatAuthor, ChatMessage};

    use super::*;

    fn call(id: &str) -> ActivityEvent {
        ActivityEvent {
            event_id: id.to_owned(),
            ts: String::from("2026-10-07T09:00:00Z"),
            run_id: String::from("api"),
            kind: SubagentEvent::ApiCall {
                caller: String::from("uid 1000"),
                capabilities: Vec::new(),
                request: ApiRequest::Hello,
                outcome: ApiOutcome::Served,
            },
        }
    }

    fn chat(text: &str) -> UiEventBody {
        UiEventBody::ChatMessage(ChatMessage {
            chat_id: String::from("local"),
            message_id: String::from("m"),
            author: ChatAuthor::Agent,
            run: None,
            text: text.to_owned(),
        })
    }

    fn seqs(events: &[UiEvent]) -> Vec<u64> {
        events.iter().map(|event| event.seq).collect()
    }

    #[test]
    fn events_are_numbered_in_order_and_replayed_after_a_mark() {
        let bus = Bus::starting_at(100);
        assert!(bus.after(None).is_empty());
        assert_eq!(bus.push(String::from("t1"), chat("a")), 100);
        assert_eq!(bus.push_activity(call("e1")), 101);
        assert_eq!(bus.push(String::from("t3"), chat("c")), 102);
        assert_eq!(seqs(&bus.after(None)), [100, 101, 102]);
        assert_eq!(seqs(&bus.after(Some(100))), [101, 102]);
        assert!(bus.after(Some(102)).is_empty());
        let activity = bus.after(Some(100)).into_iter().next().unwrap();
        assert_eq!(activity.ts, "2026-10-07T09:00:00Z");
        assert!(matches!(
            activity.body,
            UiEventBody::Activity {
                state: ActivityState::Success,
                ..
            }
        ));
        let later = Bus::starting_now();
        assert!(
            later.push(String::new(), chat("x")) > 1_700_000_000_000_000,
            "numbered from the epoch, in microseconds"
        );
    }

    #[test]
    fn the_oldest_events_go_past_the_retention_bound() {
        let bus = Bus::starting_at(1);
        for _ in 0..=MAX_RETAINED {
            bus.push(String::new(), chat("x"));
        }
        let held = bus.after(None);
        assert_eq!(held.len(), MAX_RETAINED);
        assert_eq!(held.first().map(|event| event.seq), Some(2));
    }

    #[test]
    fn a_waiter_wakes_for_the_next_event_and_times_out_without_one() {
        let bus = Bus::starting_at(1);
        assert!(bus.wait_after(None, Duration::from_millis(20)).is_empty());
        bus.push(String::new(), chat("first"));
        assert_eq!(
            seqs(&bus.wait_after(None, Duration::from_secs(5))),
            [1],
            "a held event returns at once"
        );
        assert!(
            bus.wait_after(Some(1), Duration::from_millis(20))
                .is_empty()
        );
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| bus.wait_after(Some(1), Duration::from_secs(10)));
            std::thread::sleep(Duration::from_millis(50));
            bus.push(String::new(), chat("second"));
            assert_eq!(seqs(&waiter.join().unwrap()), [2]);
        });
    }

    #[test]
    fn the_follower_reads_whole_lines_skips_noise_and_rereads_a_replaced_log() {
        let dir = std::env::temp_dir().join(format!("hatchling-api-follow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("schedules.activity");
        let bus = Bus::starting_at(1);
        let mut follower = Follower::new(log.clone());
        assert_eq!(follower.log(), log.as_path());
        assert_eq!(follower.poll(&bus), 0, "no log yet");
        let line = |id: &str| serde_json::to_string(&call(id)).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .unwrap();
        let second = line("e2");
        let (head, tail) = second.split_at(10);
        write!(file, "{}\nnot json\n\n{head}", line("e1")).unwrap();
        assert_eq!(follower.poll(&bus), 1, "the half-written line waits");
        writeln!(file, "{tail}").unwrap();
        assert_eq!(follower.poll(&bus), 1);
        assert_eq!(follower.poll(&bus), 0, "nothing new");
        let ids: Vec<String> = bus
            .after(None)
            .into_iter()
            .filter_map(|event| {
                if let UiEventBody::Activity { event, .. } = event.body {
                    Some(event.event_id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(ids, ["e1", "e2"]);
        drop(file);
        std::fs::write(&log, format!("{}\n", line("e3"))).unwrap();
        assert_eq!(
            follower.poll(&bus),
            1,
            "a shorter log is a new one, read from its start"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn following_pushes_each_line_the_log_gains() {
        let dir =
            std::env::temp_dir().join(format!("hatchling-api-follows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("schedules.activity");
        let bus: &'static Bus = Box::leak(Box::new(Bus::starting_at(1)));
        let follower: &'static mut Follower = Box::leak(Box::new(Follower::new(log.clone())));
        // Followed for the rest of the test process, as the API follows it.
        std::thread::spawn(move || follower.follow(bus, Duration::from_millis(10)));
        std::fs::write(
            &log,
            format!("{}\n", serde_json::to_string(&call("e1")).unwrap()),
        )
        .unwrap();
        assert_eq!(seqs(&bus.wait_after(None, Duration::from_secs(10))), [1]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
