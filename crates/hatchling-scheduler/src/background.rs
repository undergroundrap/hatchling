//! The background clock (WO-054): when Hatchling's own background loops
//! run, apart from the owner's schedules.
//!
//! Each loop has a [`Cadence`]. The clock keeps its rules the way the
//! [`Dispatcher`](crate::Dispatcher) keeps a schedule's:
//!
//! - **Claim, then start.** The store write that moves a loop's next due
//!   time, and says it's running, happens before the run starts.
//! - **Catch up, never pile up.** A loop that came due while Hatchling was
//!   down, or the machine slept, runs once when it's next up, flagged as
//!   missed, and its next due time is reckoned from then: never once per
//!   period it missed. Due times are wall-clock instants, so a suspend shows
//!   up as a late start.
//! - **One at a time, chat first.** No loop starts while any other runs,
//!   or while a chat run is active. A run in flight is told to stop when a
//!   chat run starts, or the owner pauses; it stops at its next step.
//! - **Off as a whole, or loop by loop.** A loop that's off doesn't run,
//!   and its cadence moves on, so turning it back on doesn't start it there
//!   and then. The pause holds every loop where it is, so each catches up
//!   once on resume.
//!
//! The clock also notes when the broker was last up: a tick that finds the
//! last one long gone records the gap ([`Gap`]), so the owner can be told
//! Hatchling was stopped while background work came due.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::CronExpr;
use crate::dispatcher::MISSED_GRACE_SECS;

/// A tick that finds the last one more than this long ago records a gap:
/// three of the broker's minute ticks.
pub const GAP_SECS: i64 = 180;

/// The most gaps kept, the newest last.
pub const MAX_GAPS: usize = 16;

/// When a loop is due.
#[derive(Debug, Clone)]
pub enum Cadence {
    /// Every `secs`, reckoned from its last start.
    Every {
        /// The period, in seconds.
        secs: i64,
    },
    /// At each time `cron` gives in `zone`: nightly or daily, in the
    /// machine's own local time.
    At {
        /// When.
        cron: CronExpr,
        /// The timezone it's read in.
        zone: TimeZone,
    },
    /// Once, `quiet_secs` after the latest activity: each new activity
    /// pushes it back.
    AfterActivity {
        /// How long things have been quiet before it runs.
        quiet_secs: i64,
    },
}

impl Cadence {
    /// The first due time after `now`; `None` for one that waits for
    /// activity.
    fn next_after(&self, now: Timestamp) -> Option<i64> {
        match self {
            Self::Every { secs } => Some(now.as_second().saturating_add(*secs)),
            Self::At { cron, zone } => cron.next_after(now, zone).map(|at| at.as_second()),
            Self::AfterActivity { .. } => None,
        }
    }
}

/// One background loop, as the broker registers it.
#[derive(Debug, Clone)]
pub struct Loop {
    /// Its name: what its state is kept under, and how the owner turns it
    /// off.
    pub name: &'static str,
    /// When it's due.
    pub cadence: Cadence,
    /// Whether it runs only when there's been activity since it last
    /// started. When there's been none, it's due and doesn't run: that's
    /// its evidence probe, and it spends nothing.
    pub needs_activity: bool,
}

/// How a background run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It finished, with changes or with an honest nothing to do.
    Done,
    /// It was due with nothing new since it last ran, so it didn't start.
    NothingNew,
    /// It failed, or couldn't start.
    Failed,
    /// It was stopped: a chat run started, or the owner paused.
    Stopped,
    /// An earlier broker left it running: how it ended is unknown.
    Interrupted,
}

/// What the clock keeps of one loop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopState {
    /// When it's next due (Unix seconds); `None` while it waits for
    /// activity.
    pub due: Option<i64>,
    /// When it last started.
    pub started: Option<i64>,
    /// When it last ended, or was found with nothing new.
    pub ended: Option<i64>,
    /// How it last ended.
    pub outcome: Option<Outcome>,
    /// When there was last activity it runs on.
    pub activity: Option<i64>,
}

/// A stretch the broker wasn't ticking: stopped, or the machine asleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gap {
    /// The last tick before it (Unix seconds).
    pub from: i64,
    /// The first tick after it.
    pub to: i64,
}

/// The clock's durable state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Background runs are off as a whole.
    #[serde(default)]
    pub off: bool,
    /// The loops turned off one by one.
    #[serde(default)]
    pub off_loops: BTreeSet<String>,
    /// Each loop's state, by name.
    #[serde(default)]
    pub loops: BTreeMap<String, LoopState>,
    /// The loop claimed and running, if any.
    #[serde(default)]
    pub running: Option<String>,
    /// The last tick (Unix seconds).
    #[serde(default)]
    pub seen: Option<i64>,
    /// The gaps in the broker's ticking, the newest last.
    #[serde(default)]
    pub gaps: Vec<Gap>,
}

impl State {
    /// Whether `name` may run: background runs on, and it on too.
    #[must_use]
    pub fn on(&self, name: &str) -> bool {
        !self.off && !self.off_loops.contains(name)
    }
}

/// Where the clock's state is kept.
pub trait Store {
    /// A backend failure.
    type Error: std::fmt::Display;

    /// Reads the state, applies `change` to it, and writes it back, as
    /// one step no other update interleaves with; the state as written.
    ///
    /// # Errors
    ///
    /// Returns the backend's error: then nothing was written.
    fn update(&self, change: &mut dyn FnMut(&mut State)) -> Result<State, Self::Error>;
}

/// A background run in flight.
pub trait Run {
    /// `None` while it runs; how it ended, once it has.
    fn poll(&mut self) -> Option<Outcome>;

    /// Asks it to stop at its next step.
    fn stop(&mut self);
}

/// Starts background runs.
pub trait Launcher {
    /// The run handle.
    type Run: Run;

    /// Starts `name`'s run; `missed` when it came due while Hatchling was
    /// down.
    ///
    /// # Errors
    ///
    /// Returns why it couldn't start.
    fn start(&self, name: &str, missed: bool) -> Result<Self::Run, String>;
}

/// What, outside the clock, holds background work back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Gate {
    /// The owner paused everything that runs without them.
    pub paused: bool,
    /// A chat run is active.
    pub chat_active: bool,
}

/// Why no loop started on a tick, though one may have been due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// The owner paused.
    Paused,
    /// The owner turned the running loop off, or background runs.
    Off,
    /// A chat run is active.
    ChatActive,
    /// Another loop is running.
    Busy,
}

/// What one tick did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// The loop started, and whether it came due while Hatchling was down.
    pub started: Option<(String, bool)>,
    /// A run that ended, and how.
    pub ended: Option<(String, Outcome)>,
    /// A run told to stop on this tick.
    pub stopping: Option<String>,
    /// Loops due with nothing new, moved on without a run.
    pub nothing_new: Vec<String>,
    /// Why nothing could start, when something held it.
    pub held: Option<Held>,
    /// A gap this tick found before it.
    pub gap: Option<Gap>,
    /// A loop an earlier broker left running.
    pub interrupted: Option<String>,
}

/// The run in flight.
struct Flight<R> {
    name: String,
    run: R,
    stopping: bool,
}

/// The background clock: the loops it wakes, and the one in flight.
pub struct Clock<R> {
    loops: Vec<Loop>,
    flight: Option<Flight<R>>,
}

impl<R> std::fmt::Debug for Clock<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Clock")
            .field("loops", &self.loops)
            .field("running", &self.flight.as_ref().map(|flight| &flight.name))
            .finish()
    }
}

impl<R: Run> Clock<R> {
    /// The clock for `loops`, none in flight.
    #[must_use]
    pub const fn new(loops: Vec<Loop>) -> Self {
        Self {
            loops,
            flight: None,
        }
    }

    /// The loop in flight, if any.
    #[must_use]
    pub fn running(&self) -> Option<&str> {
        self.flight.as_ref().map(|flight| flight.name.as_str())
    }

    /// There was activity at `now` that loops run on: each loop that
    /// waits for activity is due once things have been quiet long enough,
    /// and each that needs it has something new.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub fn activity<S: Store>(&self, now: Timestamp, store: &S) -> Result<(), S::Error> {
        let at = now.as_second();
        store.update(&mut |state| {
            for each in &self.loops {
                let kept = state.loops.entry(each.name.to_owned()).or_default();
                kept.activity = Some(at);
                if let Cadence::AfterActivity { quiet_secs } = each.cadence {
                    kept.due = Some(at.saturating_add(quiet_secs));
                }
            }
        })?;
        Ok(())
    }

    /// One tick at `now`: notes a gap, ends or stops the run in flight, or
    /// starts the loop most overdue.
    ///
    /// # Errors
    ///
    /// Returns the store's error. A claim that failed starts nothing.
    pub fn tick<S, L>(
        &mut self,
        now: Timestamp,
        gate: Gate,
        store: &S,
        launcher: &L,
    ) -> Result<Report, S::Error>
    where
        S: Store,
        L: Launcher<Run = R>,
    {
        let at = now.as_second();
        let mut report = Report::default();
        let in_flight = self.flight.is_some();
        let state = store.update(&mut |state| {
            report.gap = None;
            report.interrupted = None;
            if let Some(seen) = state
                .seen
                .filter(|seen| at.saturating_sub(*seen) > GAP_SECS)
            {
                let gap = Gap { from: seen, to: at };
                state.gaps.push(gap);
                if state.gaps.len() > MAX_GAPS {
                    let over = state.gaps.len() - MAX_GAPS;
                    state.gaps.drain(..over);
                }
                report.gap = Some(gap);
            }
            state.seen = Some(at);
            if let Some(name) = state.running.take_if(|_| !in_flight) {
                let kept = state.loops.entry(name.clone()).or_default();
                kept.ended = Some(at);
                kept.outcome = Some(Outcome::Interrupted);
                report.interrupted = Some(name);
            }
            for each in &self.loops {
                let on = state.on(each.name);
                let kept = state.loops.entry(each.name.to_owned()).or_default();
                if kept.due.is_none() {
                    kept.due = each.cadence.next_after(now);
                } else if !on && kept.due.is_some_and(|due| due <= at) {
                    // Off, its cadence moves on: turned back on, it doesn't
                    // start there and then.
                    kept.due = each.cadence.next_after(now);
                }
            }
        })?;
        if let Some(flight) = &mut self.flight {
            if let Some(outcome) = flight.run.poll() {
                let name = flight.name.clone();
                self.flight = None;
                store.update(&mut |state| {
                    if state.running.as_deref() == Some(name.as_str()) {
                        state.running = None;
                    }
                    let kept = state.loops.entry(name.clone()).or_default();
                    kept.ended = Some(at);
                    kept.outcome = Some(outcome);
                    if outcome == Outcome::Stopped {
                        // It didn't finish: it runs again as soon as
                        // nothing holds it, not a period later.
                        kept.due = Some(at);
                    }
                })?;
                report.ended = Some((name, outcome));
            } else {
                let hold = if gate.paused {
                    Some(Held::Paused)
                } else if !state.on(&flight.name) {
                    Some(Held::Off)
                } else if gate.chat_active {
                    Some(Held::ChatActive)
                } else {
                    None
                };
                if hold.is_some() && !flight.stopping {
                    flight.run.stop();
                    flight.stopping = true;
                    report.stopping = Some(flight.name.clone());
                }
                report.held = Some(hold.unwrap_or(Held::Busy));
                return Ok(report);
            }
        }
        if gate.paused {
            report.held = Some(Held::Paused);
            return Ok(report);
        }
        if gate.chat_active {
            report.held = Some(Held::ChatActive);
            return Ok(report);
        }
        self.start_due(now, store, launcher, &mut report)?;
        Ok(report)
    }

    /// Starts the loop most overdue that has something to do, moving on
    /// each due with nothing new.
    fn start_due<S, L>(
        &mut self,
        now: Timestamp,
        store: &S,
        launcher: &L,
        report: &mut Report,
    ) -> Result<(), S::Error>
    where
        S: Store,
        L: Launcher<Run = R>,
    {
        let at = now.as_second();
        let mut claimed: Option<(String, bool)> = None;
        let mut quiet = Vec::new();
        store.update(&mut |state| {
            claimed = None;
            quiet.clear();
            if state.running.is_some() {
                return;
            }
            let mut due: Vec<(i64, &Loop)> = self
                .loops
                .iter()
                .filter(|each| state.on(each.name))
                .filter_map(|each| {
                    let when = state.loops.get(each.name)?.due?;
                    (when <= at).then_some((when, each))
                })
                .collect();
            due.sort_by_key(|(when, each)| (*when, each.name));
            for (when, each) in due {
                let kept = state.loops.entry(each.name.to_owned()).or_default();
                let next = each.cadence.next_after(now);
                if each.needs_activity
                    && kept.activity.is_none_or(|seen| Some(seen) <= kept.started)
                {
                    kept.due = next;
                    kept.ended = Some(at);
                    kept.outcome = Some(Outcome::NothingNew);
                    quiet.push(each.name.to_owned());
                    continue;
                }
                kept.due = next;
                kept.started = Some(at);
                state.running = Some(each.name.to_owned());
                claimed = Some((each.name.to_owned(), when + MISSED_GRACE_SECS < at));
                break;
            }
        })?;
        report.nothing_new = quiet;
        let Some((name, missed)) = claimed else {
            return Ok(());
        };
        match launcher.start(&name, missed) {
            Ok(run) => {
                self.flight = Some(Flight {
                    name: name.clone(),
                    run,
                    stopping: false,
                });
                report.started = Some((name, missed));
            }
            Err(reason) => {
                tracing::warn!(loop_name = %name, %reason, "a background run could not start");
                store.update(&mut |state| {
                    state.running = None;
                    let kept = state.loops.entry(name.clone()).or_default();
                    kept.ended = Some(at);
                    kept.outcome = Some(Outcome::Failed);
                })?;
                report.ended = Some((name, Outcome::Failed));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "background_tests.rs"]
mod tests;
