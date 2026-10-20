//! Component: the dispatcher against a fake clock, an in-memory store,
//! a scripted launcher, and a scripted delivery path.
//!
//! Every test is deterministic: time only moves when a test passes a
//! later `now` to `tick`, and a run finishes only when its test says so.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use hatchling_core::{HookSource, ScheduleGrant, ScheduleSpec};
use hatchling_scheduler::{
    Claim, Delivery, Dispatcher, FollowupParent, Hook, Launcher, MAX_FOLLOWUP_DEPTH,
    MISSED_GRACE_SECS, Occurrence, OccurrenceState, Parked, Run, RunRequest, Schedule, Services,
    Store, TickReport, Timestamp, validate, validate_followup, validate_space_task,
};

/// An in-memory store.
#[derive(Default)]
struct MemStore {
    schedules: RefCell<BTreeMap<String, Schedule>>,
    hooks: RefCell<Vec<Hook>>,
    parked: RefCell<Vec<Parked>>,
    /// The next write this store fails, by its role (WO-025): `cadence`
    /// for the write that moves a schedule to its next fire, `landed` for
    /// a claim that is written and then fails (its sync did), `record` for
    /// the write that moves a claimed fire on, `park` for the one that
    /// keeps an undelivered result.
    fail_next: RefCell<Option<&'static str>>,
    /// Whether the place for parked results is full: every park fails
    /// while it is.
    full: RefCell<bool>,
}

impl MemStore {
    /// Whether the next write of `role` fails, which it does once.
    fn fails(&self, role: &'static str) -> bool {
        let mut fail = self.fail_next.borrow_mut();
        if *fail == Some(role) {
            *fail = None;
            return true;
        }
        false
    }

    /// Whether the store still accounts for a fire of `id`: its
    /// schedule, or a result kept for the user.
    fn accounts_for(&self, id: &str) -> bool {
        self.schedules.borrow().contains_key(id)
            || self
                .parked
                .borrow()
                .iter()
                .any(|parked| parked.schedule_id == id)
    }

    /// Applies `change` to the latest record of `id` when it still holds
    /// the fire due at `due`, moving its revision on.
    fn change_fire(&self, id: &str, due: i64, change: impl FnOnce(&mut Schedule)) -> bool {
        let mut schedules = self.schedules.borrow_mut();
        let Some(stored) = schedules.get_mut(id) else {
            return false;
        };
        if stored.ledger.occurrence.as_ref().map(|fire| fire.due) != Some(due) {
            return false;
        }
        change(stored);
        stored.ledger.revision += 1;
        true
    }
}

impl Store for MemStore {
    type Error = String;

    fn load(&self) -> Result<Vec<Schedule>, String> {
        Ok(self.schedules.borrow().values().cloned().collect())
    }

    fn claim_due(
        &self,
        id: &str,
        revision: u64,
        due: i64,
        next_due: Option<i64>,
        missed: bool,
    ) -> Result<Claim, String> {
        if self.fails("cadence") {
            return Err(String::from("the disk is full"));
        }
        let mut schedules = self.schedules.borrow_mut();
        let Some(stored) = schedules.get_mut(id) else {
            return Ok(Claim::NotDue);
        };
        if stored.ledger.revision != revision || stored.next_due != due || stored.ledger.exhausted {
            return Ok(Claim::NotDue);
        }
        if stored.ledger.occurrence.is_some() {
            return Ok(Claim::Full);
        }
        stored.ledger.occurrence = Some(Occurrence {
            due,
            missed,
            state: OccurrenceState::Claimed,
        });
        match next_due {
            Some(next) => stored.next_due = next,
            None => stored.ledger.exhausted = true,
        }
        stored.ledger.revision += 1;
        if self.fails("landed") {
            return Err(String::from("the claim was written, but its sync failed"));
        }
        Ok(Claim::Claimed)
    }

    fn skip_due(
        &self,
        id: &str,
        revision: u64,
        due: i64,
        next_due: Option<i64>,
    ) -> Result<bool, String> {
        if self.fails("cadence") {
            return Err(String::from("the disk is full"));
        }
        let mut schedules = self.schedules.borrow_mut();
        let Some(stored) = schedules.get_mut(id) else {
            return Ok(false);
        };
        if stored.ledger.revision != revision || stored.next_due != due {
            return Ok(false);
        }
        match next_due {
            Some(next) => stored.next_due = next,
            None => stored.ledger.exhausted = true,
        }
        stored.ledger.revision += 1;
        Ok(true)
    }

    fn record(&self, id: &str, due: i64, state: OccurrenceState) -> Result<bool, String> {
        if self.fails("record") {
            return Err(String::from("the disk is full"));
        }
        Ok(self.change_fire(id, due, |stored| {
            if let Some(fire) = stored.ledger.occurrence.as_mut() {
                fire.state = state;
            }
        }))
    }

    fn resolve(&self, id: &str, due: i64, parked: Option<&Parked>) -> Result<bool, String> {
        let holds = self
            .schedules
            .borrow()
            .get(id)
            .and_then(|stored| stored.ledger.occurrence.as_ref())
            .is_some_and(|fire| fire.due == due);
        if !holds {
            return Ok(false);
        }
        if let Some(parked) = parked {
            self.park(parked)?;
        }
        let exhausted = self.change_fire(id, due, |stored| stored.ledger.occurrence = None)
            && self
                .schedules
                .borrow()
                .get(id)
                .is_some_and(|stored| stored.ledger.exhausted);
        if exhausted {
            self.schedules.borrow_mut().remove(id);
        }
        Ok(true)
    }

    fn hold_in_flight(&self) -> Result<Vec<String>, String> {
        let mut held = Vec::new();
        for stored in self.schedules.borrow_mut().values_mut() {
            if let Some(fire) = stored.ledger.occurrence.as_mut() {
                let started = match fire.state {
                    OccurrenceState::Claimed => false,
                    OccurrenceState::Running => true,
                    OccurrenceState::Finished { .. } | OccurrenceState::Unknown { .. } => continue,
                };
                fire.state = OccurrenceState::Unknown { started };
                stored.ledger.revision += 1;
                held.push(stored.id.clone());
            }
        }
        Ok(held)
    }

    fn remove(&self, id: &str) -> Result<bool, String> {
        Ok(self.schedules.borrow_mut().remove(id).is_some())
    }

    fn hooks(&self) -> Result<Vec<Hook>, String> {
        Ok(self.hooks.borrow().clone())
    }

    fn park(&self, parked: &Parked) -> Result<(), String> {
        if self.fails("park") || *self.full.borrow() {
            return Err(String::from("the disk is full"));
        }
        self.parked.borrow_mut().push(parked.clone());
        Ok(())
    }
}

/// A run whose outcome the test sets.
#[derive(Clone, Default)]
struct FakeRun(Rc<RefCell<Option<Result<String, String>>>>);

impl FakeRun {
    fn finish(&self, outcome: Result<&str, &str>) {
        *self.0.borrow_mut() = Some(outcome.map(str::to_owned).map_err(str::to_owned));
    }
}

impl Run for FakeRun {
    fn poll(&mut self) -> Option<Result<String, String>> {
        self.0.borrow_mut().take()
    }
}

/// Records every request and hands back a controllable run.
#[derive(Default)]
struct FakeLauncher {
    requests: RefCell<Vec<RunRequest>>,
    runs: RefCell<Vec<FakeRun>>,
    refuse: bool,
}

impl Launcher for FakeLauncher {
    type Run = FakeRun;

    fn launch(&self, request: RunRequest) -> Result<FakeRun, String> {
        self.requests.borrow_mut().push(request);
        if self.refuse {
            return Err(String::from("no cell available"));
        }
        let run = FakeRun::default();
        self.runs.borrow_mut().push(run.clone());
        Ok(run)
    }
}

impl FakeLauncher {
    fn run(&self, index: usize) -> FakeRun {
        self.runs.borrow().get(index).cloned().unwrap()
    }
}

/// Delivers into a log, failing the first `fail` attempts.
#[derive(Default)]
struct FakeDelivery {
    delivered: RefCell<Vec<(String, String)>>,
    attempts: RefCell<usize>,
    fail: usize,
}

impl Delivery for FakeDelivery {
    fn deliver(&self, channel: &str, text: &str) -> Result<(), String> {
        let attempt = {
            let mut attempts = self.attempts.borrow_mut();
            *attempts += 1;
            *attempts
        };
        if attempt <= self.fail {
            return Err(format!("attempt {attempt} failed"));
        }
        self.delivered
            .borrow_mut()
            .push((channel.to_owned(), text.to_owned()));
        Ok(())
    }
}

fn ts(rfc3339: &str) -> Timestamp {
    rfc3339.parse().unwrap()
}

fn send(channel: &str) -> ScheduleGrant {
    ScheduleGrant::ChannelSend {
        channel: channel.to_owned(),
    }
}

fn egress(destination: &str) -> ScheduleGrant {
    ScheduleGrant::Egress {
        destination: destination.to_owned(),
    }
}

fn held() -> BTreeSet<ScheduleGrant> {
    BTreeSet::from([send("local"), egress("api.example.com:443")])
}

/// Creates and stores a schedule, created at `now`, as the broker does.
fn schedule(store: &MemStore, id: &str, when: ScheduleSpec, now: &str) -> Schedule {
    let created = validate(
        id.to_owned(),
        when,
        String::from("summarize the inbox"),
        vec![send("local"), egress("api.example.com:443")],
        String::from("local"),
        &held(),
        ts(now),
    )
    .unwrap();
    store
        .schedules
        .borrow_mut()
        .insert(id.to_owned(), created.clone());
    created
}

fn cron(expr: &str, tz: &str) -> ScheduleSpec {
    ScheduleSpec::Cron {
        expr: expr.to_owned(),
        tz: tz.to_owned(),
    }
}

struct World {
    store: MemStore,
    launcher: FakeLauncher,
    delivery: FakeDelivery,
    held: BTreeSet<ScheduleGrant>,
    authenticated: BTreeSet<String>,
    dispatcher: Dispatcher<FakeRun>,
}

impl World {
    fn new() -> Self {
        Self {
            store: MemStore::default(),
            launcher: FakeLauncher::default(),
            delivery: FakeDelivery::default(),
            held: held(),
            authenticated: BTreeSet::from([String::from("local")]),
            dispatcher: Dispatcher::default(),
        }
    }

    fn tick(&mut self, now: &str) -> TickReport {
        self.tick_with(now, &[])
    }

    /// A tick with these events observed outside the dispatcher.
    fn tick_with(&mut self, now: &str, events: &[HookSource]) -> TickReport {
        let services = Services {
            store: &self.store,
            launcher: &self.launcher,
            delivery: &self.delivery,
            held: &self.held,
            events,
            authenticated: &self.authenticated,
        };
        self.dispatcher.tick(ts(now), &services).unwrap()
    }

    fn requests(&self) -> Vec<RunRequest> {
        self.launcher.requests.borrow().clone()
    }

    /// Stores a hook as the broker does after validating it.
    fn hook(&self, id: &str, source: HookSource, grants: Vec<ScheduleGrant>) {
        self.store.hooks.borrow_mut().push(Hook {
            id: id.to_owned(),
            source,
            task: format!("task of {id}"),
            grants,
            channel: String::from("local"),
        });
    }

    fn next_due(&self, id: &str) -> Option<String> {
        self.store
            .schedules
            .borrow()
            .get(id)
            .and_then(Schedule::due)
            .map(|t| t.to_string())
    }
}

#[test]
fn a_cron_schedule_fires_at_its_minute_and_delivers_to_its_channel() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("30 9 * * *", "Europe/Berlin"),
        "2026-09-21T12:00:00Z",
    );
    // 09:30 Berlin is 07:30Z. A minute early: nothing.
    assert_eq!(world.tick("2026-09-22T07:29:00Z"), TickReport::default());
    // At the minute: one run, on time, with the schedule's grants.
    let report = world.tick("2026-09-22T07:30:05Z");
    assert_eq!(report.launched, [(String::from("s"), false)]);
    let request = world.launcher.requests.borrow().first().cloned().unwrap();
    assert_eq!(request.task, "summarize the inbox");
    assert_eq!(
        request.grants,
        [send("local"), egress("api.example.com:443")]
    );
    assert_eq!(request.due, ts("2026-09-22T07:30:00Z"));
    assert!(!request.missed);
    assert_eq!(request.space, None, "an ordinary schedule has no space");
    assert_eq!(world.next_due("s").as_deref(), Some("2026-09-23T07:30:00Z"));
    // The run finishes; the next tick delivers its output.
    world.launcher.run(0).finish(Ok("3 new messages"));
    let report = world.tick("2026-09-22T07:31:00Z");
    assert_eq!(report.delivered, ["s"]);
    assert_eq!(
        *world.delivery.delivered.borrow(),
        [(String::from("local"), String::from("3 new messages"))]
    );
    assert_eq!(world.dispatcher.active(), 0);
}

#[test]
fn a_space_tasks_run_is_bound_to_its_space_and_holds_only_what_is_still_held() {
    let mut world = World::new();
    // The space holds both grants; the creator has since lost egress.
    let space_grants = held();
    let task = validate_space_task(
        String::from("sch-t"),
        "spc-1",
        String::from("water the plants"),
        vec![send("local"), egress("api.example.com:443")],
        String::from("local"),
        &space_grants,
        ts("2026-09-21T12:00:00Z"),
    )
    .unwrap();
    assert_eq!(task.space.as_deref(), Some("spc-1"));
    assert_eq!(task.next_due, ts("2026-09-21T12:00:01Z").as_second());
    world
        .store
        .schedules
        .borrow_mut()
        .insert(String::from("sch-t"), task);
    world.held = BTreeSet::from([send("local")]);
    let report = world.tick("2026-09-21T12:00:01Z");
    assert_eq!(report.launched, [(String::from("sch-t"), false)]);
    assert_eq!(world.next_due("sch-t"), None, "a one-shot");
    let request = world.requests().first().cloned().unwrap();
    assert_eq!(request.space.as_deref(), Some("spc-1"));
    assert_eq!(request.grants, [send("local")]);
    assert_eq!(request.task, "water the plants");
}

#[test]
fn a_space_task_holds_only_what_its_space_holds() {
    let space_grants = BTreeSet::from([send("local")]);
    let refused = validate_space_task(
        String::from("sch-t"),
        "spc-1",
        String::from("t"),
        vec![send("local"), egress("api.example.com:443")],
        String::from("local"),
        &space_grants,
        ts("2026-09-21T12:00:00Z"),
    );
    assert_eq!(
        refused.map(|schedule| schedule.id),
        Err(hatchling_core::ScheduleRefusal::ExceedsGrants)
    );
}

#[test]
fn the_dispatcher_debugs_its_run_count() {
    let mut world = World::new();
    assert_eq!(
        format!("{:?}", world.dispatcher),
        "Dispatcher { active: 0, uncertain: 0 }"
    );
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.tick("2026-09-21T13:00:00Z");
    assert_eq!(
        format!("{:?}", world.dispatcher),
        "Dispatcher { active: 1, uncertain: 0 }"
    );
}

#[test]
fn a_one_shot_fires_once_and_is_removed() {
    let mut world = World::new();
    schedule(
        &world.store,
        "once",
        ScheduleSpec::Once {
            at: String::from("2026-10-01T09:00"),
            tz: String::from("UTC"),
        },
        "2026-09-21T12:00:00Z",
    );
    let report = world.tick("2026-10-01T09:00:00Z");
    assert_eq!(report.launched, [(String::from("once"), false)]);
    // Its fire is claimed, and it is due no more; it stays, with that
    // fire, until the fire's result is resolved (WO-025).
    assert!(report.removed.is_empty());
    assert_eq!(world.next_due("once"), None);
    world.launcher.run(0).finish(Ok("done"));
    let report = world.tick("2026-10-01T09:01:00Z");
    assert_eq!(report.delivered, ["once"]);
    assert_eq!(report.removed, ["once"]);
    assert!(world.store.schedules.borrow().is_empty());
    assert_eq!(world.tick("2026-10-02T09:00:00Z"), TickReport::default());
    assert_eq!(world.launcher.requests.borrow().len(), 1);
}

#[test]
fn a_grant_revoked_before_the_fire_is_withheld_from_the_run() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    // The egress grant is revoked after creation.
    world.held = BTreeSet::from([send("local")]);
    let report = world.tick("2026-09-21T13:00:00Z");
    assert_eq!(report.launched, [(String::from("s"), false)]);
    assert_eq!(
        report.revoked,
        [(String::from("s"), egress("api.example.com:443"))]
    );
    let request = world.launcher.requests.borrow().first().cloned().unwrap();
    assert_eq!(request.grants, [send("local")], "the run cannot use it");
}

#[test]
fn a_revoked_channel_send_parks_the_output_instead_of_delivering() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.tick("2026-09-21T13:00:00Z");
    world.held = BTreeSet::from([egress("api.example.com:443")]);
    world.launcher.run(0).finish(Ok("the report"));
    let report = world.tick("2026-09-21T13:01:00Z");
    assert_eq!(report.parked, ["s"]);
    assert!(world.delivery.delivered.borrow().is_empty());
    let parked = world.store.parked.borrow().first().cloned().unwrap();
    assert_eq!(parked.text, "the report");
    assert_eq!(parked.channel, "local");
    assert_eq!(parked.due, ts("2026-09-21T13:00:00Z").as_second());
    assert!(parked.reason.contains("revoked"), "{}", parked.reason);
}

#[test]
fn an_overlapping_fire_is_skipped_so_runs_never_stack() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("* * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    assert_eq!(world.tick("2026-09-21T12:01:00Z").launched.len(), 1);
    // The run is still going for three more minutes: each fire skips.
    for minute in ["02", "03", "04"] {
        let report = world.tick(&format!("2026-09-21T12:{minute}:00Z"));
        assert_eq!(report.skipped, ["s"], "12:{minute}");
        assert!(report.launched.is_empty(), "12:{minute}");
        assert_eq!(world.dispatcher.active(), 1);
    }
    // It finishes: the next fire launches again, and delivers the first.
    world.launcher.run(0).finish(Ok("slow"));
    let report = world.tick("2026-09-21T12:05:00Z");
    assert_eq!(report.delivered, ["s"]);
    assert_eq!(report.launched, [(String::from("s"), false)]);
    assert_eq!(
        world.launcher.requests.borrow().len(),
        2,
        "never more than one at a time"
    );
}

#[test]
fn missed_fires_fire_once_flagged_then_resume_the_cadence() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    // The broker was down from before 13:00 until 17:20: four missed.
    let report = world.tick("2026-09-21T17:20:00Z");
    assert_eq!(
        report.launched,
        [(String::from("s"), true)],
        "one fire, flagged"
    );
    let request = world.launcher.requests.borrow().first().cloned().unwrap();
    assert!(request.missed);
    assert_eq!(
        request.due,
        ts("2026-09-21T13:00:00Z"),
        "the oldest missed fire"
    );
    // The cadence resumes from now, not from the missed fires.
    assert_eq!(world.next_due("s").as_deref(), Some("2026-09-21T18:00:00Z"));
    world.launcher.run(0).finish(Ok("late"));
    world.tick("2026-09-21T17:21:00Z");
    let report = world.tick("2026-09-21T18:00:10Z");
    assert_eq!(
        report.launched,
        [(String::from("s"), false)],
        "on time again"
    );
    assert_eq!(world.launcher.requests.borrow().len(), 2);
}

#[test]
fn lateness_under_the_grace_is_on_time_and_at_it_is_missed() {
    let mut world = World::new();
    schedule(
        &world.store,
        "a",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    let report = world.tick("2026-09-21T13:00:59Z");
    assert_eq!(report.launched, [(String::from("a"), false)]);
    let mut world = World::new();
    schedule(
        &world.store,
        "b",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    let report = world.tick("2026-09-21T13:01:00Z");
    assert_eq!(report.launched, [(String::from("b"), true)]);
    assert_eq!(MISSED_GRACE_SECS, 60);
}

#[test]
fn a_failed_delivery_is_retried_once_then_parked() {
    let mut world = World::new();
    world.delivery.fail = 1;
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.tick("2026-09-21T13:00:00Z");
    world.launcher.run(0).finish(Ok("first"));
    assert_eq!(
        world.tick("2026-09-21T13:01:00Z").delivered,
        ["s"],
        "the retry landed"
    );
    assert_eq!(*world.delivery.attempts.borrow(), 2);

    let mut world = World::new();
    world.delivery.fail = 2;
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.tick("2026-09-21T13:00:00Z");
    world.launcher.run(0).finish(Ok("lost?"));
    assert_eq!(world.tick("2026-09-21T13:01:00Z").parked, ["s"]);
    assert_eq!(*world.delivery.attempts.borrow(), 2, "one retry, no more");
    let parked = world.store.parked.borrow().first().cloned().unwrap();
    assert_eq!(parked.text, "lost?");
    assert!(
        parked.reason.contains("attempt 2 failed"),
        "{}",
        parked.reason
    );
}

#[test]
fn failed_and_unstarted_runs_are_parked_not_dropped() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.tick("2026-09-21T13:00:00Z");
    world.launcher.run(0).finish(Err("the model timed out"));
    assert_eq!(world.tick("2026-09-21T13:01:00Z").parked, ["s"]);
    let parked = world.store.parked.borrow().first().cloned().unwrap();
    assert_eq!(parked.text, "the model timed out");
    assert_eq!(parked.reason, "the run failed");

    let mut world = World::new();
    world.launcher.refuse = true;
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    let report = world.tick("2026-09-21T13:00:00Z");
    assert!(report.launched.is_empty());
    assert_eq!(report.parked, ["s"]);
    assert_eq!(world.dispatcher.active(), 0);
    let parked = world.store.parked.borrow().first().cloned().unwrap();
    assert_eq!(parked.reason, "the run did not start");
    assert_eq!(parked.due, ts("2026-09-21T13:00:00Z").as_second());
    // The cadence still moved on.
    assert_eq!(world.next_due("s").as_deref(), Some("2026-09-21T14:00:00Z"));
}

#[test]
fn a_schedule_that_cannot_be_evaluated_is_removed_loudly() {
    let mut world = World::new();
    let mut broken = schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    broken.when = cron("0 * * * *", "Mars/Olympus");
    world
        .store
        .schedules
        .borrow_mut()
        .insert(String::from("s"), broken);
    let report = world.tick("2026-09-21T13:00:00Z");
    assert_eq!(report.removed, ["s"]);
    assert!(report.launched.is_empty());
    assert!(world.store.schedules.borrow().is_empty());
}

#[test]
fn only_due_schedules_fire_and_a_cancel_mid_run_is_respected() {
    let mut world = World::new();
    schedule(
        &world.store,
        "hourly",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    schedule(
        &world.store,
        "daily",
        cron("0 9 * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    let report = world.tick("2026-09-21T13:00:00Z");
    assert_eq!(report.launched, [(String::from("hourly"), false)]);
    assert_eq!(
        world.next_due("daily").as_deref(),
        Some("2026-09-22T09:00:00Z")
    );
    // Cancelled while its run is active: the output is still delivered,
    // and the schedule does not come back.
    world.store.remove("hourly").unwrap();
    world.launcher.run(0).finish(Ok("last one"));
    assert_eq!(world.tick("2026-09-21T13:05:00Z").delivered, ["hourly"]);
    assert_eq!(world.tick("2026-09-21T14:00:00Z"), TickReport::default());
    assert!(world.next_due("hourly").is_none());
}

fn message(channel: &str) -> HookSource {
    HookSource::ChannelMessage {
        channel: channel.to_owned(),
    }
}

fn finished(id: &str) -> HookSource {
    HookSource::ScheduleRunFinished {
        schedule_id: id.to_owned(),
    }
}

#[test]
fn a_hook_fires_on_its_event_and_delivers_to_its_channel() {
    let mut world = World::new();
    world.hook(
        "hk-1",
        message("local"),
        vec![send("local"), egress("api.example.com:443")],
    );
    // No event, no run.
    assert_eq!(world.tick("2026-09-21T12:00:00Z"), TickReport::default());
    let report = world.tick_with("2026-09-21T12:01:00Z", &[message("local")]);
    assert_eq!(report.launched, [(String::from("hk-1"), false)]);
    let request = world.requests().first().cloned().unwrap();
    assert_eq!(
        request,
        RunRequest {
            schedule_id: String::from("hk-1"),
            task: String::from("task of hk-1"),
            grants: vec![send("local"), egress("api.example.com:443")],
            due: ts("2026-09-21T12:01:00Z"),
            missed: false,
            channel: String::from("local"),
            depth: 0,
            // The broker's own assertion of what fired it.
            event: Some(message("local")),
            space: None,
            feed: None,
            chat: false,
            history: Vec::new(),
        }
    );
    world.launcher.run(0).finish(Ok("triaged 2 messages"));
    let report = world.tick("2026-09-21T12:02:00Z");
    assert_eq!(report.delivered, ["hk-1"]);
    assert_eq!(
        *world.delivery.delivered.borrow(),
        [(String::from("local"), String::from("triaged 2 messages"))]
    );
    assert_eq!(world.dispatcher.active(), 0);
    // The hook stays registered: the next event fires it again.
    let report = world.tick_with("2026-09-21T12:03:00Z", &[message("local")]);
    assert_eq!(report.launched, [(String::from("hk-1"), false)]);
}

#[test]
fn an_event_from_an_unauthenticated_or_unknown_source_spawns_no_run() {
    let mut world = World::new();
    // Stored as if validated once; the events below must still be refused.
    world.hook("hk-tg", message("telegram"), vec![send("local")]);
    world.hook("hk-fin", finished("s"), vec![send("local")]);
    let report = world.tick_with(
        "2026-09-21T12:00:00Z",
        &[
            // No authenticated ingress for this channel.
            message("telegram"),
            // A finished run, claimed from outside: only the dispatcher
            // asserts that.
            finished("s"),
        ],
    );
    assert!(report.launched.is_empty(), "{report:?}");
    assert!(world.requests().is_empty());
    assert_eq!(report.refused_events, [message("telegram"), finished("s")]);
    // An authenticated event that no hook names fires nothing, and is
    // not a refusal.
    let report = world.tick_with("2026-09-21T12:01:00Z", &[message("local")]);
    assert_eq!(report, TickReport::default());
}

#[test]
fn a_finished_schedule_run_fires_its_hooks_and_run_output_asserts_nothing() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.hook("hk-on-s", finished("s"), vec![send("local")]);
    world.hook("hk-on-other", finished("other"), vec![send("local")]);
    world.hook("hk-on-msg", message("local"), vec![send("local")]);
    assert_eq!(
        world.tick("2026-09-21T13:00:00Z").launched,
        [(String::from("s"), false)]
    );
    // The run's output names other events: text is never an event.
    world
        .launcher
        .run(0)
        .finish(Ok("schedule-run-finished:other channel-message:local"));
    let report = world.tick("2026-09-21T13:01:00Z");
    assert_eq!(report.delivered, ["s"]);
    assert_eq!(report.launched, [(String::from("hk-on-s"), false)]);
    let fired = world.requests().last().cloned().unwrap();
    assert_eq!(fired.event, Some(finished("s")));
    // The hook's run ends; then a failed schedule run counts as finished
    // too.
    world.launcher.run(1).finish(Ok("noted"));
    world.tick("2026-09-21T13:02:00Z");
    assert_eq!(
        world.tick("2026-09-21T14:00:00Z").launched,
        [(String::from("s"), false)]
    );
    world.launcher.run(2).finish(Err("the model was down"));
    let report = world.tick("2026-09-21T14:01:00Z");
    assert_eq!(report.parked, ["s"]);
    assert_eq!(report.launched, [(String::from("hk-on-s"), false)]);
}

#[test]
fn a_hook_run_finishing_asserts_no_event() {
    let mut world = World::new();
    world.hook("hk-a", message("local"), vec![send("local")]);
    // A hook on the first hook's id, as if it were a schedule.
    world.hook("hk-b", finished("hk-a"), vec![send("local")]);
    world.tick_with("2026-09-21T12:00:00Z", &[message("local")]);
    world.launcher.run(0).finish(Ok("done"));
    let report = world.tick("2026-09-21T12:01:00Z");
    assert_eq!(report.delivered, ["hk-a"]);
    assert!(report.launched.is_empty(), "{report:?}");
}

#[test]
fn a_hook_holds_only_what_its_creator_still_holds() {
    let mut world = World::new();
    world.hook(
        "hk-1",
        message("local"),
        vec![send("local"), egress("api.example.com:443")],
    );
    world.held = BTreeSet::from([send("local")]);
    let report = world.tick_with("2026-09-21T12:00:00Z", &[message("local")]);
    assert_eq!(
        report.revoked,
        [(String::from("hk-1"), egress("api.example.com:443"))]
    );
    assert_eq!(world.requests().first().unwrap().grants, [send("local")]);
}

#[test]
fn overlapping_hook_fires_do_not_stack() {
    let mut world = World::new();
    world.hook("hk-1", message("local"), vec![send("local")]);
    // Two messages in one tick are one event.
    let report = world.tick_with(
        "2026-09-21T12:00:00Z",
        &[message("local"), message("local")],
    );
    assert_eq!(report.launched, [(String::from("hk-1"), false)]);
    assert!(report.skipped.is_empty(), "{report:?}");
    // Another while the run is active: skipped, not stacked.
    let report = world.tick_with("2026-09-21T12:01:00Z", &[message("local")]);
    assert!(report.launched.is_empty());
    assert_eq!(report.skipped, ["hk-1"]);
    assert_eq!(world.dispatcher.active(), 1);
    assert_eq!(world.requests().len(), 1);
    // Once it finishes, the next event fires it again.
    world.launcher.run(0).finish(Ok("ok"));
    let report = world.tick_with("2026-09-21T12:02:00Z", &[message("local")]);
    assert_eq!(report.delivered, ["hk-1"]);
    assert_eq!(report.launched, [(String::from("hk-1"), false)]);
}

#[test]
fn a_hook_run_that_cannot_start_is_parked() {
    let mut world = World::new();
    world.launcher.refuse = true;
    world.hook("hk-1", message("local"), vec![send("local")]);
    let report = world.tick_with("2026-09-21T12:00:00Z", &[message("local")]);
    assert_eq!(report.parked, ["hk-1"]);
    let parked = world.store.parked.borrow().first().cloned().unwrap();
    assert_eq!(parked.reason, "the run did not start");
    assert_eq!(parked.channel, "local");
    assert_eq!(parked.due, ts("2026-09-21T12:00:00Z").as_second());
}

#[test]
fn a_followup_chain_runs_one_link_at_a_time_and_ends_at_the_bound() {
    let mut world = World::new();
    let grants = vec![send("local")];
    let mut now = ts("2026-09-21T12:00:00Z");
    let mut depth = 0;
    // Each link: the run at `depth` asks for a follow-up; it is a
    // one-shot, fires once, and is removed. Never two at once.
    loop {
        let parent = FollowupParent {
            grants: &grants,
            channel: "local",
            depth,
        };
        let Ok(next) = validate_followup(
            format!("sch-{depth}"),
            &parent,
            String::from("again"),
            60,
            grants.clone(),
            now,
        ) else {
            break;
        };
        world
            .store
            .schedules
            .borrow_mut()
            .insert(next.id.clone(), next.clone());
        now = Timestamp::from_second(next.next_due).unwrap();
        let report = world.tick(&now.to_string());
        assert_eq!(report.launched, [(next.id.clone(), false)]);
        assert_eq!(world.requests().last().unwrap().depth, depth + 1);
        assert_eq!(world.dispatcher.active(), 1);
        let index = world.requests().len() - 1;
        world.launcher.run(index).finish(Ok("link"));
        let report = world.tick(&now.to_string());
        assert_eq!(report.removed, std::slice::from_ref(&next.id));
        assert_eq!(world.dispatcher.active(), 0);
        depth = next.depth;
    }
    assert_eq!(depth, MAX_FOLLOWUP_DEPTH);
    assert_eq!(world.requests().len(), 5);
    assert!(world.store.schedules.borrow().is_empty());
}

/// WO-025, window 1: a one-shot whose run hasn't finished when the
/// dispatcher is rebuilt over the same store (a broker that died) is still
/// accounted for in the store, not forgotten as it started.
#[test]
fn a_one_shot_still_running_when_the_broker_dies_is_accounted_for() {
    let mut world = World::new();
    schedule(
        &world.store,
        "once",
        ScheduleSpec::Once {
            at: String::from("2026-10-01T09:00"),
            tz: String::from("UTC"),
        },
        "2026-09-21T12:00:00Z",
    );
    assert_eq!(
        world.tick("2026-10-01T09:00:00Z").launched,
        [(String::from("once"), false)]
    );
    // The broker dies: its dispatcher and its runs are gone; the store
    // stays.
    world.dispatcher = Dispatcher::default();
    world.launcher = FakeLauncher::default();
    assert!(
        world.store.accounts_for("once"),
        "a started one-shot left no trace in the store"
    );
}

/// WO-025, window 2: a fire whose cadence write fails is launched once, not
/// again once its run has finished and the old due time is still due.
#[test]
fn a_failed_cadence_write_never_launches_an_occurrence_twice() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    *world.store.fail_next.borrow_mut() = Some("cadence");
    world.tick("2026-09-21T13:00:00Z");
    for minute in 1..4 {
        if let Some(run) = world.launcher.runs.borrow().last() {
            run.finish(Ok("done"));
        }
        world.tick(&format!("2026-09-21T13:0{minute}:00Z"));
    }
    assert_eq!(
        world.requests().len(),
        1,
        "the 13:00 occurrence ran more than once"
    );
}

/// WO-025, window 3: an output whose delivery fails twice and whose
/// parking fails too is not lost: a later tick, with delivery and the
/// store healthy, delivers it or keeps it.
#[test]
fn an_output_that_cannot_be_delivered_or_parked_is_kept_for_later() {
    let mut world = World::new();
    world.delivery.fail = 2;
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.tick("2026-09-21T13:00:00Z");
    world.launcher.run(0).finish(Ok("the summary"));
    *world.store.fail_next.borrow_mut() = Some("park");
    world.tick("2026-09-21T13:01:00Z");
    world.tick("2026-09-21T13:02:00Z");
    let delivered = world
        .delivery
        .delivered
        .borrow()
        .iter()
        .any(|(_, text)| text == "the summary");
    let kept = world
        .store
        .parked
        .borrow()
        .iter()
        .any(|parked| parked.text == "the summary");
    assert!(delivered || kept, "the output was lost");
}

/// WO-025: a result that can be neither delivered nor parked holds its
/// schedule's next fire, loudly, rather than make room by dropping it; once
/// it is delivered, the schedule fires again.
#[test]
fn a_result_that_waits_holds_its_schedules_next_fire() {
    let mut world = World::new();
    world.delivery.fail = 100;
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.tick("2026-09-21T13:00:00Z");
    world.launcher.run(0).finish(Ok("the summary"));
    *world.store.full.borrow_mut() = true;
    world.tick("2026-09-21T13:01:00Z");
    let report = world.tick("2026-09-21T14:00:00Z");
    assert_eq!(report.refused, ["s"]);
    assert!(report.launched.is_empty());
    let waiting = world
        .store
        .schedules
        .borrow()
        .get("s")
        .and_then(|stored| stored.ledger.occurrence.clone());
    assert!(
        matches!(
            waiting,
            Some(Occurrence {
                state: OccurrenceState::Finished { .. },
                ..
            })
        ),
        "{waiting:?}"
    );
    world.delivery.fail = 0;
    *world.store.full.borrow_mut() = false;
    let report = world.tick("2026-09-21T14:01:00Z");
    assert_eq!(report.delivered, ["s"]);
    assert_eq!(report.launched, [(String::from("s"), true)]);
    assert!(
        world
            .delivery
            .delivered
            .borrow()
            .iter()
            .any(|(_, text)| text == "the summary")
    );
}

/// WO-025, part 2: a fire a broker left claimed or running is of unknown
/// outcome once the next broker holds it: its schedule's later fires are
/// held, and nothing runs, until the owner resolves it.
#[test]
fn a_fire_left_in_flight_holds_its_schedule_as_unknown() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    world.tick("2026-09-21T13:00:00Z");
    // The broker dies with the run out; the next one holds the fire.
    world.dispatcher = Dispatcher::default();
    assert_eq!(world.store.hold_in_flight().unwrap(), ["s"]);
    assert_eq!(
        world.store.hold_in_flight().unwrap(),
        Vec::<String>::new(),
        "held once"
    );
    let held = world
        .store
        .schedules
        .borrow()
        .get("s")
        .and_then(|stored| stored.ledger.occurrence.clone());
    assert_eq!(
        held.map(|fire| fire.state),
        Some(OccurrenceState::Unknown { started: true })
    );
    let report = world.tick("2026-09-21T14:00:00Z");
    assert_eq!(report.held, ["s"]);
    assert!(report.launched.is_empty());
    assert_eq!(world.requests().len(), 1, "never run again on its own");
    assert_eq!(world.next_due("s").as_deref(), Some("2026-09-21T15:00:00Z"));
}

/// The fire of `id`, as the store holds it.
fn fire_of(world: &World, id: &str) -> Option<Occurrence> {
    world
        .store
        .schedules
        .borrow()
        .get(id)
        .and_then(|stored| stored.ledger.occurrence.clone())
}

/// WO-025, part 3: a claim written whose write then failed (its sync did)
/// launches nothing that tick; the next tick writes it again, and launches
/// its run once, as the fire it claimed.
#[test]
fn a_claim_that_landed_but_failed_launches_once_the_next_tick() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    *world.store.fail_next.borrow_mut() = Some("landed");
    let report = world.tick("2026-09-21T13:00:00Z");
    assert!(report.launched.is_empty());
    assert_eq!(world.requests().len(), 0);
    let report = world.tick("2026-09-21T13:00:30Z");
    assert_eq!(report.launched, [(String::from("s"), false)]);
    let requests = world.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests.first().map(|request| request.due),
        Some(ts("2026-09-21T13:00:00Z"))
    );
    assert_eq!(
        fire_of(&world, "s").map(|fire| fire.state),
        Some(OccurrenceState::Running)
    );
    world.launcher.run(0).finish(Ok("done"));
    for minute in 1..4 {
        world.tick(&format!("2026-09-21T13:0{minute}:00Z"));
    }
    assert_eq!(world.requests().len(), 1, "launched once");
    assert_eq!(world.next_due("s").as_deref(), Some("2026-09-21T14:00:00Z"));
}

/// WO-025, part 3: a claim that landed is launched only once it is
/// written again, and no schedule fires while it is uncertain: while that
/// write fails, nothing is launched.
#[test]
fn a_landed_claim_launches_nothing_until_it_is_written_again() {
    let mut world = World::new();
    for id in ["s", "t"] {
        schedule(
            &world.store,
            id,
            cron("* * * * *", "UTC"),
            "2026-09-21T12:59:30Z",
        );
    }
    *world.store.fail_next.borrow_mut() = Some("landed");
    world.tick("2026-09-21T13:00:00Z");
    *world.store.fail_next.borrow_mut() = Some("record");
    let report = world.tick("2026-09-21T13:00:10Z");
    assert!(report.launched.is_empty());
    assert_eq!(
        world.requests().len(),
        0,
        "a fire while a claim was uncertain"
    );
    assert_eq!(
        fire_of(&world, "s").map(|fire| fire.state),
        Some(OccurrenceState::Claimed)
    );
    assert_eq!(fire_of(&world, "t"), None);
    let report = world.tick("2026-09-21T13:01:05Z");
    assert_eq!(
        report.launched,
        [(String::from("s"), false), (String::from("t"), true)]
    );
    let dues: Vec<(String, Timestamp)> = world
        .requests()
        .into_iter()
        .map(|request| (request.schedule_id, request.due))
        .collect();
    assert_eq!(
        dues,
        [
            (String::from("s"), ts("2026-09-21T13:00:00Z")),
            (String::from("t"), ts("2026-09-21T13:00:00Z")),
        ],
        "the fires they claimed, not later ones"
    );
}

/// WO-025, part 3: a failed claim that didn't land is forgotten, and the
/// fire is claimed afresh the next tick, once.
#[test]
fn a_failed_claim_that_did_not_land_is_claimed_afresh() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    *world.store.fail_next.borrow_mut() = Some("cadence");
    let report = world.tick("2026-09-21T13:00:00Z");
    assert!(report.launched.is_empty());
    let report = world.tick("2026-09-21T13:00:30Z");
    assert_eq!(report.launched, [(String::from("s"), false)]);
    assert_eq!(world.requests().len(), 1);
}

/// WO-025, part 3: a landed claim whose schedule is cancelled before it
/// settles is forgotten: nothing is launched.
#[test]
fn a_landed_claim_whose_schedule_is_cancelled_launches_nothing() {
    let mut world = World::new();
    schedule(
        &world.store,
        "s",
        cron("0 * * * *", "UTC"),
        "2026-09-21T12:00:00Z",
    );
    *world.store.fail_next.borrow_mut() = Some("landed");
    world.tick("2026-09-21T13:00:00Z");
    world.store.remove("s").unwrap();
    let report = world.tick("2026-09-21T13:00:30Z");
    assert!(report.launched.is_empty());
    assert_eq!(world.requests().len(), 0);
}
