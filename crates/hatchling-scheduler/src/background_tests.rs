use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::*;

/// 2026-11-26T11:31:00Z.
const T0: i64 = 1_795_692_660;
const HOUR: i64 = 3600;

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(second).unwrap()
}

#[derive(Default)]
struct Mem {
    state: RefCell<State>,
    broken: Cell<bool>,
}

impl Store for Mem {
    type Error = String;

    fn update(&self, change: &mut dyn FnMut(&mut State)) -> Result<State, String> {
        if self.broken.get() {
            return Err(String::from("the store is down"));
        }
        let mut state = self.state.borrow().clone();
        change(&mut state);
        *self.state.borrow_mut() = state.clone();
        Ok(state)
    }
}

/// A run's controls, shared with the test.
#[derive(Default)]
struct Controls {
    outcome: Cell<Option<Outcome>>,
    stops: Cell<u32>,
}

struct Fake(Rc<Controls>);

impl Run for Fake {
    fn poll(&mut self) -> Option<Outcome> {
        self.0.outcome.get()
    }

    fn stop(&mut self) {
        self.0.stops.set(self.0.stops.get() + 1);
    }
}

/// Starts fake runs, and keeps what the store said as each started.
struct Starts {
    store: Rc<Mem>,
    started: RefCell<Vec<(String, bool)>>,
    claimed_first: RefCell<Vec<bool>>,
    controls: RefCell<Vec<Rc<Controls>>>,
    refuse: Cell<bool>,
}

impl Starts {
    fn new(store: &Rc<Mem>) -> Self {
        Self {
            store: Rc::clone(store),
            started: RefCell::new(Vec::new()),
            claimed_first: RefCell::new(Vec::new()),
            controls: RefCell::new(Vec::new()),
            refuse: Cell::new(false),
        }
    }

    fn count(&self) -> usize {
        self.started.borrow().len()
    }

    fn last(&self) -> Rc<Controls> {
        Rc::clone(self.controls.borrow().last().unwrap())
    }
}

impl Launcher for Starts {
    type Run = Fake;

    fn start(&self, name: &str, missed: bool) -> Result<Fake, String> {
        let state = self.store.state.borrow();
        let claimed = state.running.as_deref() == Some(name)
            && state.loops[name]
                .is_some_and(|due| due > state.seen.unwrap_or(0));
        self.claimed_first.borrow_mut().push(claimed);
        if self.refuse.get() {
            return Err(String::from("no cell"));
        }
        self.started.borrow_mut().push((name.to_owned(), missed));
        let controls = Rc::new(Controls::default());
        self.controls.borrow_mut().push(Rc::clone(&controls));
        Ok(Fake(controls))
    }
}

fn hourly(name: &'static str) -> Loop {
    Loop {
        name,
        cadence: Cadence::Every { secs: HOUR },
        needs_activity: false,
    }
}

fn open() -> Gate {
    Gate::default()
}

struct Rig {
    store: Rc<Mem>,
    starts: Starts,
    clock: Clock<Fake>,
}

impl Rig {
    fn new(loops: Vec<Loop>) -> Self {
        let store = Rc::new(Mem::default());
        let starts = Starts::new(&store);
        Self {
            store,
            starts,
            clock: Clock::new(loops),
        }
    }

    fn tick(&mut self, second: i64, gate: Gate) -> Report {
        self.clock
            .tick(at(second), gate, &*self.store, &self.starts)
            .unwrap()
    }

    fn state(&self) -> State {
        self.store.state.borrow().clone()
    }

    fn kept(&self, name: &str) -> LoopState {
        self.state().loops.get(name).cloned().unwrap()
    }

    fn finish(&self, outcome: Outcome) {
        self.starts.last().outcome.set(Some(outcome));
    }
}

/// The design's test: the machine sleeps through ten periods, and wakes to
/// one run, flagged as missed, with the gap recorded; its next is an hour
/// from the wake, not ten piled up.
#[test]
fn a_suspend_shows_up_as_one_catch_up_run() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    assert_eq!(rig.tick(T0, open()), Report::default());
    assert_eq!(rig.kept("memory").due, Some(T0 + HOUR));
    assert_eq!(rig.tick(T0 + 60, open()).started, None);
    // Asleep from T0 + 60 for ten hours.
    let wake = T0 + 10 * HOUR + 120;
    let report = rig.tick(wake, open());
    assert_eq!(report.started, Some((String::from("memory"), true)));
    assert_eq!(
        report.gap,
        Some(Gap {
            from: T0 + 60,
            to: wake
        })
    );
    assert_eq!(
        rig.state().gaps,
        vec![Gap {
            from: T0 + 60,
            to: wake
        }]
    );
    assert_eq!(rig.kept("memory").due, Some(wake + HOUR));
    rig.finish(Outcome::Done);
    let ended = rig.tick(wake + 60, open());
    assert_eq!(ended.ended, Some((String::from("memory"), Outcome::Done)));
    assert_eq!(ended.started, None, "nothing piled up behind it");
    for minute in 2..60 {
        assert_eq!(rig.tick(wake + minute * 60, open()), Report::default());
    }
    assert_eq!(rig.starts.count(), 1);
    assert_eq!(
        rig.tick(wake + HOUR, open()).started,
        Some((String::from("memory"), false))
    );
}

/// A run is missed once it's more than the grace late, and not before.
#[test]
fn a_run_is_missed_past_the_grace_and_not_before() {
    for (late, missed) in [
        (0, false),
        (MISSED_GRACE_SECS, false),
        (MISSED_GRACE_SECS + 1, true),
    ] {
        let mut rig = Rig::new(vec![hourly("memory")]);
        let _ = rig.tick(T0, open());
        let report = rig.tick(T0 + HOUR + late, open());
        assert_eq!(
            report.started,
            Some((String::from("memory"), missed)),
            "{late}"
        );
    }
}

/// The claim is written before the run starts: the launcher sees the loop
/// running and its due time moved on. A store that's down starts nothing.
#[test]
fn the_claim_is_written_before_the_run_starts() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    let _ = rig.tick(T0 + HOUR, open());
    assert_eq!(*rig.starts.claimed_first.borrow(), vec![true]);

    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    rig.store.broken.set(true);
    assert!(
        rig.clock
            .tick(at(T0 + HOUR), open(), &*rig.store, &rig.starts)
            .is_err()
    );
    assert_eq!(rig.starts.count(), 0);
    assert_eq!(rig.clock.running(), None);
}

/// Nothing starts while a chat run is active; once it ends, the loop due
/// starts.
#[test]
fn nothing_starts_while_a_chat_run_is_active() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    let chatting = Gate {
        chat_active: true,
        ..Gate::default()
    };
    for minute in 0..5 {
        let report = rig.tick(T0 + HOUR + minute * 60, chatting);
        assert_eq!(report.held, Some(Held::ChatActive));
        assert_eq!(report.started, None);
    }
    assert_eq!(
        rig.tick(T0 + HOUR + 300, open()).started,
        Some((String::from("memory"), true))
    );
}

/// A chat run starting tells the run in flight to stop, once. Stopped, it
/// runs again as soon as the chat is idle, not a period later.
#[test]
fn a_chat_run_stops_the_run_in_flight_once_and_it_runs_again_after() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    let _ = rig.tick(T0 + HOUR, open());
    let chatting = Gate {
        chat_active: true,
        ..Gate::default()
    };
    let report = rig.tick(T0 + HOUR + 60, chatting);
    assert_eq!(report.stopping, Some(String::from("memory")));
    assert_eq!(report.held, Some(Held::ChatActive));
    let again = rig.tick(T0 + HOUR + 120, chatting);
    assert_eq!(again.stopping, None, "told once");
    assert_eq!(rig.starts.last().stops.get(), 1);
    rig.finish(Outcome::Stopped);
    let ended = rig.tick(T0 + HOUR + 180, chatting);
    assert_eq!(
        ended.ended,
        Some((String::from("memory"), Outcome::Stopped))
    );
    assert_eq!(ended.started, None, "not while the chat runs");
    assert_eq!(
        rig.tick(T0 + HOUR + 240, open()).started,
        Some((String::from("memory"), false))
    );
}

/// The pause tells the run in flight to stop and starts nothing; on
/// resume, a loop that came due many times over runs once.
#[test]
fn the_pause_holds_every_loop_and_each_catches_up_once() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    let _ = rig.tick(T0 + HOUR, open());
    let paused = Gate {
        paused: true,
        ..Gate::default()
    };
    let report = rig.tick(T0 + HOUR + 60, paused);
    assert_eq!(report.stopping, Some(String::from("memory")));
    assert_eq!(report.held, Some(Held::Paused));
    rig.finish(Outcome::Stopped);
    let _ = rig.tick(T0 + HOUR + 120, paused);
    for minute in 3..(5 * 60) {
        let report = rig.tick(T0 + HOUR + minute * 60, paused);
        assert_eq!(report.started, None);
        assert_eq!(report.held, Some(Held::Paused));
    }
    let resumed = rig.tick(T0 + 6 * HOUR, open());
    assert_eq!(resumed.started, Some((String::from("memory"), true)));
    rig.finish(Outcome::Done);
    for minute in 1..59 {
        assert_eq!(rig.tick(T0 + 6 * HOUR + minute * 60, open()).started, None);
    }
    assert_eq!(rig.starts.count(), 2);
}

/// A loop turned off doesn't run and its cadence moves on: turned back
/// on, it waits for its next due time.
#[test]
fn a_loop_turned_off_moves_on_and_doesnt_start_when_turned_on() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    rig.store
        .state
        .borrow_mut()
        .off_loops
        .insert(String::from("memory"));
    let report = rig.tick(T0 + HOUR, open());
    assert_eq!(report.started, None);
    assert_eq!(report.held, None);
    assert_eq!(rig.kept("memory").due, Some(T0 + 2 * HOUR));
    rig.store.state.borrow_mut().off_loops.clear();
    let on = rig.tick(T0 + HOUR + 120, open());
    assert_eq!(on.started, None, "not there and then");
    assert_eq!(
        rig.tick(T0 + 2 * HOUR, open()).started,
        Some((String::from("memory"), false))
    );
}

/// Background runs turned off as a whole stop the run in flight and start
/// nothing.
#[test]
fn background_off_stops_the_run_in_flight_and_starts_nothing() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    let _ = rig.tick(T0 + HOUR, open());
    rig.store.state.borrow_mut().off = true;
    let report = rig.tick(T0 + HOUR + 60, open());
    assert_eq!(report.stopping, Some(String::from("memory")));
    assert_eq!(report.held, Some(Held::Off));
    rig.finish(Outcome::Stopped);
    let _ = rig.tick(T0 + HOUR + 120, open());
    for hour in 2..10 {
        assert_eq!(rig.tick(T0 + hour * HOUR, open()).started, None);
    }
    assert_eq!(rig.starts.count(), 1);
}

/// Two loops due: one at a time, the most overdue first, the other once
/// the first has ended.
#[test]
fn one_loop_at_a_time_the_most_overdue_first() {
    let mut rig = Rig::new(vec![
        hourly("memory"),
        Loop {
            name: "ideas",
            cadence: Cadence::Every { secs: HOUR / 2 },
            needs_activity: false,
        },
    ]);
    let _ = rig.tick(T0, open());
    let first = rig.tick(T0 + 2 * HOUR, open());
    assert_eq!(first.started, Some((String::from("ideas"), true)));
    let busy = rig.tick(T0 + 2 * HOUR + 60, open());
    assert_eq!(busy.held, Some(Held::Busy));
    assert_eq!(busy.started, None);
    rig.finish(Outcome::Done);
    let ended = rig.tick(T0 + 2 * HOUR + 120, open());
    assert_eq!(ended.ended, Some((String::from("ideas"), Outcome::Done)));
    assert_eq!(ended.started, Some((String::from("memory"), true)));
}

/// A loop that needs activity, due with none since it last started, is
/// moved on without a run; with some, it runs.
#[test]
fn a_loop_with_nothing_new_moves_on_without_a_run() {
    let mut rig = Rig::new(vec![Loop {
        name: "memory",
        cadence: Cadence::Every { secs: HOUR },
        needs_activity: true,
    }]);
    let _ = rig.tick(T0, open());
    let quiet = rig.tick(T0 + HOUR, open());
    assert_eq!(quiet.nothing_new, vec![String::from("memory")]);
    assert_eq!(quiet.started, None);
    let kept = &rig.kept("memory");
    assert_eq!(kept.outcome, Some(Outcome::NothingNew));
    assert_eq!(kept.due, Some(T0 + 2 * HOUR));
    rig.clock
        .activity(at(T0 + HOUR + 600), &*rig.store)
        .unwrap();
    let report = rig.tick(T0 + 2 * HOUR, open());
    assert_eq!(report.started, Some((String::from("memory"), false)));
    rig.finish(Outcome::Done);
    let _ = rig.tick(T0 + 2 * HOUR + 60, open());
    assert_eq!(
        rig.tick(T0 + 3 * HOUR, open()).nothing_new,
        vec![String::from("memory")],
        "the activity it ran on isn't new again"
    );
}

/// A loop that waits for activity runs once things have been quiet long
/// enough; each new activity pushes it back, and it runs once.
#[test]
fn a_loop_after_activity_waits_for_quiet_and_runs_once() {
    let quiet_secs = 600;
    let mut rig = Rig::new(vec![Loop {
        name: "goals",
        cadence: Cadence::AfterActivity { quiet_secs },
        needs_activity: false,
    }]);
    let _ = rig.tick(T0, open());
    assert_eq!(rig.kept("goals").due, None);
    assert_eq!(rig.tick(T0 + 10 * HOUR, open()).started, None);
    rig.clock.activity(at(T0), &*rig.store).unwrap();
    rig.clock.activity(at(T0 + 300), &*rig.store).unwrap();
    assert_eq!(
        rig.tick(T0 + quiet_secs, open()).started,
        None,
        "pushed back"
    );
    let report = rig.tick(T0 + 300 + quiet_secs, open());
    assert_eq!(report.started, Some((String::from("goals"), false)));
    rig.finish(Outcome::Done);
    for minute in 1..100 {
        assert_eq!(
            rig.tick(T0 + 300 + quiet_secs + minute * 60, open())
                .started,
            None
        );
    }
    assert_eq!(rig.kept("goals").due, None);
}

/// A loop an earlier broker left running is of unknown outcome: noted as
/// interrupted, and not run again before it's due.
#[test]
fn a_run_an_earlier_broker_left_is_noted_and_not_rerun_early() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    let _ = rig.tick(T0 + HOUR, open());
    // The broker stops; a new one starts with the same store.
    let mut fresh = Clock::new(vec![hourly("memory")]);
    let report = fresh
        .tick(at(T0 + HOUR + 60), open(), &*rig.store, &rig.starts)
        .unwrap();
    assert_eq!(report.interrupted, Some(String::from("memory")));
    assert_eq!(report.started, None);
    let kept = &rig.kept("memory");
    assert_eq!(kept.outcome, Some(Outcome::Interrupted));
    assert_eq!(rig.state().running, None);
    assert_eq!(rig.starts.count(), 1);
}

/// A run that can't start is recorded as failed, and nothing is left
/// claimed.
#[test]
fn a_run_that_cant_start_is_failed_and_unclaimed() {
    let mut rig = Rig::new(vec![hourly("memory")]);
    let _ = rig.tick(T0, open());
    rig.starts.refuse.set(true);
    let report = rig.tick(T0 + HOUR, open());
    assert_eq!(
        report.ended,
        Some((String::from("memory"), Outcome::Failed))
    );
    assert_eq!(report.started, None);
    assert_eq!(rig.clock.running(), None);
    let state = rig.state();
    assert_eq!(state.running, None);
    assert_eq!(state.loops.get("memory").unwrap().outcome, Some(Outcome::Failed));
    assert_eq!(state.loops.get("memory").unwrap().due, Some(T0 + 2 * HOUR));
}

/// A nightly loop is due at its local time in the machine's zone.
#[test]
fn a_nightly_loop_is_due_at_its_local_time() {
    let zone = jiff::tz::db().get("America/New_York").unwrap();
    let mut rig = Rig::new(vec![Loop {
        name: "alignment",
        cadence: Cadence::At {
            cron: CronExpr::parse("0 3 * * *").unwrap(),
            zone,
        },
        needs_activity: false,
    }]);
    // 06:31 in New York on 26 November: next 03:00 is the 27th, 08:00Z.
    let _ = rig.tick(T0, open());
    let due = rig.kept("alignment").due.unwrap();
    assert_eq!(at(due).to_string(), "2026-11-27T08:00:00Z");
}

/// Gaps shorter than the bound aren't gaps, and only the newest are kept.
#[test]
fn only_real_gaps_are_kept_and_only_the_newest() {
    let mut rig = Rig::new(Vec::new());
    let _ = rig.tick(T0, open());
    assert_eq!(rig.tick(T0 + GAP_SECS, open()).gap, None);
    let mut now = T0 + GAP_SECS;
    for _ in 0..(MAX_GAPS + 3) {
        now += GAP_SECS + 1;
        assert!(rig.tick(now, open()).gap.is_some());
    }
    let gaps = rig.state().gaps;
    assert_eq!(gaps.len(), MAX_GAPS);
    assert_eq!(gaps.last().unwrap().to, now);
}
