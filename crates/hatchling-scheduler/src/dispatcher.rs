//! The dispatcher: fires due schedules, delivers what their runs
//! produce, and parks what cannot be delivered.
//!
//! It is a plain state machine driven by [`Dispatcher::tick`], with the
//! clock, the store, the run launcher, and the delivery path all passed
//! in. The broker ticks it with the real ones; tests tick it with fakes
//! and a fake clock.
//!
//! One tick:
//!
//! 1. **Finished runs are stored, then delivered** (WO-025). Each run
//!    that has finished since the last tick has its result stored in its
//!    schedule's record first; one the store can't take yet stays in
//!    memory and is tried again. Then every stored result is delivered
//!    from the store to its schedule's channel, if the creator still
//!    holds `channel-send` on it. A failed delivery is logged and retried
//!    once. If the retry fails too, the output is parked for the user. A
//!    result is resolved only once it is delivered or kept: one that can
//!    be neither stays stored, for a later tick, and holds its schedule's
//!    next fire. Nothing is dropped. Delivery is at least once: a result
//!    delivered whose resolution then fails is delivered again.
//! 2. **Due schedules fire.** For each schedule whose `next_due` has
//!    come:
//!    - **Claimed before it starts.** The fire is claimed in the same
//!      store write that moves the cadence on (or marks a one-shot
//!      exhausted), and its run is launched only once that write is
//!      committed. A claim that fails launches nothing; one read from a
//!      record that has changed since is refused.
//!    - **Overlap: skip.** If its previous run is still claimed or
//!      active, this fire is skipped. At most one run per schedule exists
//!      at any time, so a run slower than its interval cannot stack.
//!    - **Missed fires: once, flagged.** If the fire is more than
//!      [`MISSED_GRACE_SECS`] late (the broker was down), it fires once,
//!      with `missed: true` in the run request, and not once per missed
//!      occurrence. The flag is the broker's, not the agent's.
//!    - **Grants re-checked.** The run gets the schedule's grants that
//!      the creator still holds. A grant revoked since creation is not
//!      the run's to use.
//!
//!    Either way, the schedule then moves to its next fire after `now`,
//!    resuming its cadence. A one-shot is removed once its fire is
//!    resolved. A follow-up is a one-shot like any other, so a chain runs
//!    one link at a time.
//! 3. **Events fire hooks.** An event is a [`HookSource`] the
//!    broker asserts:
//!    - **From outside the dispatcher,** only a channel message that
//!      arrived through an authenticated ingress, on a channel in
//!      [`Services::authenticated`]. Any other event is refused before
//!      a run spawns: it is not one the broker can vouch for.
//!    - **From the dispatcher itself:** a schedule's run finished
//!      (step 1). Nothing else can assert that, and a run's output is
//!      never parsed for events.
//!
//!    Each hook on the event fires through the same path as a schedule:
//!    at most one run per hook (a fire while it runs is skipped), and
//!    the hook's grants the creator still holds.

use std::collections::{BTreeSet, HashMap};

use hatchling_core::{FeedSpec, HookSource, ScheduleGrant};
use jiff::Timestamp;

use crate::hook::Hook;
use crate::schedule::{Occurrence, OccurrenceState, Outcome, Schedule, Timing, effective_grants};

/// How late a fire may be and still count as on time, in seconds. The
/// broker ticks well inside this, so only a broker that was down (or
/// stalled) produces a missed fire.
pub const MISSED_GRACE_SECS: i64 = 60;

/// One scheduled or hook run, as the launcher is asked to start it.
/// Every field is the broker's: none comes from a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    /// The schedule or hook it is for.
    pub schedule_id: String,
    /// The task text.
    pub task: String,
    /// The grants the run may use: the schedule's or hook's, minus any
    /// revoked.
    pub grants: Vec<ScheduleGrant>,
    /// When this fire was due.
    pub due: Timestamp,
    /// Whether the fire is late because the broker missed it.
    pub missed: bool,
    /// Where the run's output goes.
    pub channel: String,
    /// The run's depth in its follow-up chain: 0 unless it is a
    /// follow-up.
    pub depth: u32,
    /// For a hook run, the event that fired it.
    pub event: Option<HookSource>,
    /// For a space task's run, the space it is a task of: its
    /// session may use that space and no other.
    pub space: Option<String>,
    /// For a feed's run, what its digest gathers: the broker
    /// generates it, read-only, instead of starting a cell.
    pub feed: Option<FeedSpec>,
    /// Whether it answers the user's message on the local channel: its
    /// agent then speaks with the chat prompt, not the worker's.
    pub chat: bool,
    /// For a chat run, the conversation's last exchanges, oldest first
    /// (WO-026): only the broker's record of what the owner said and was
    /// shown. Empty for every other run.
    pub history: Vec<hatchling_core::conversation::ChatExchange>,
}

/// A result that could not be delivered, kept for the user.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Parked {
    /// The schedule it came from.
    pub schedule_id: String,
    /// The channel it was for.
    pub channel: String,
    /// The run's output, or what went wrong.
    pub text: String,
    /// When the fire was due, in Unix seconds.
    pub due: i64,
    /// Why it was parked.
    pub reason: String,
}

/// What a claim on a due fire came to (WO-025).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// Committed: the fire is claimed and the cadence moved on, in one
    /// write. Only now may its run be launched.
    Claimed,
    /// Not due as the store has it: moved on, cancelled, or edited since
    /// it was read. Nothing was written.
    NotDue,
    /// Its last fire's result still waits to be delivered or kept for the
    /// user: no new fire is claimed until it is, and nothing is dropped
    /// to make room.
    Full,
}

/// Where schedules live. Broker-private: only the broker implements it.
///
/// Every change reads the schedule's latest record under the store's own
/// serialization: a change never writes back a copy the dispatcher read
/// earlier, which could undo an edit or a cancel made in between.
pub trait Store {
    /// A backend failure.
    type Error: std::fmt::Display;

    /// Every stored schedule.
    ///
    /// # Errors
    ///
    /// Returns the backend's error, a schedule the index names whose
    /// record is missing among them.
    fn load(&self) -> Result<Vec<Schedule>, Self::Error>;

    /// Claims the fire of `id` due at `due`, when its record is still at
    /// `revision` with that fire due and no fire unresolved: records it
    /// claimed (with `missed`), and moves the cadence to `next_due`, or
    /// marks a one-shot exhausted when there is none, in one write.
    ///
    /// # Errors
    ///
    /// Returns the backend's error: then nothing is claimed, and nothing
    /// may be launched.
    fn claim_due(
        &self,
        id: &str,
        revision: u64,
        due: i64,
        next_due: Option<i64>,
        missed: bool,
    ) -> Result<Claim, Self::Error>;

    /// Moves the cadence of `id` past the fire due at `due` without
    /// claiming it (its previous fire is still running), when its record
    /// is still at `revision`. Returns whether it moved.
    ///
    /// # Errors
    ///
    /// Returns the backend's error.
    fn skip_due(
        &self,
        id: &str,
        revision: u64,
        due: i64,
        next_due: Option<i64>,
    ) -> Result<bool, Self::Error>;

    /// Moves the claimed fire of `id` due at `due` to `state`. Returns
    /// whether that fire is still the schedule's (a cancel wins).
    ///
    /// # Errors
    ///
    /// Returns the backend's error.
    fn record(&self, id: &str, due: i64, state: OccurrenceState) -> Result<bool, Self::Error>;

    /// Resolves the claimed fire of `id` due at `due`: delivered, or,
    /// with `parked`, kept for the user first. An exhausted one-shot goes
    /// with its fire. Returns whether that fire was still the schedule's.
    ///
    /// # Errors
    ///
    /// Returns the backend's error, among them a full place for parked
    /// results: then the fire stays as it was, and its result with it.
    fn resolve(&self, id: &str, due: i64, parked: Option<&Parked>) -> Result<bool, Self::Error>;

    /// Removes a schedule. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Returns the backend's error.
    fn remove(&self, id: &str) -> Result<bool, Self::Error>;

    /// Every stored hook.
    ///
    /// # Errors
    ///
    /// Returns the backend's error.
    fn hooks(&self) -> Result<Vec<Hook>, Self::Error>;

    /// Keeps an undeliverable hook result for the user.
    ///
    /// # Errors
    ///
    /// Returns the backend's error, among them a full place for parked
    /// results: nothing kept is dropped to make room.
    fn park(&self, parked: &Parked) -> Result<(), Self::Error>;
}

/// A running scheduled run.
pub trait Run {
    /// `None` while it runs; its output, or why it failed, once done.
    fn poll(&mut self) -> Option<Result<String, String>>;
}

/// Starts scheduled runs: the bounded agent runs the broker spawns.
pub trait Launcher {
    /// The run handle.
    type Run: Run;

    /// Starts one run.
    ///
    /// # Errors
    ///
    /// Returns why it could not start.
    fn launch(&self, request: RunRequest) -> Result<Self::Run, String>;
}

/// Delivers a run's output to a channel.
pub trait Delivery {
    /// Delivers `text` on `channel`.
    ///
    /// # Errors
    ///
    /// Returns why it could not.
    fn deliver(&self, channel: &str, text: &str) -> Result<(), String>;

    /// Delivers `text` on `channel`, as [`deliver`](Self::deliver) does,
    /// and says whether the person was shown `text` itself: `false` when
    /// something else reached them in its place (an output gate's
    /// notice), or when the delivery path can't tell. A chat keeps an
    /// exchange as context only on `true` (WO-026).
    ///
    /// # Errors
    ///
    /// As [`deliver`](Self::deliver).
    fn deliver_verbatim(&self, channel: &str, text: &str) -> Result<bool, String> {
        self.deliver(channel, text).map(|()| false)
    }
}

/// What one tick did, for the broker's logs and for tests.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// Runs started: schedule id, and whether the fire was missed.
    pub launched: Vec<(String, bool)>,
    /// Fires skipped because the previous run was still active.
    pub skipped: Vec<String>,
    /// Outputs delivered, by schedule id.
    pub delivered: Vec<String>,
    /// Results parked, by schedule id.
    pub parked: Vec<String>,
    /// Grants withheld from a run because they were revoked.
    pub revoked: Vec<(String, ScheduleGrant)>,
    /// Schedules removed: one-shots done, or no longer evaluable.
    pub removed: Vec<String>,
    /// Events refused: not from a source the broker authenticates.
    pub refused_events: Vec<HookSource>,
    /// Claims refused: the schedule's last result still waits (WO-025).
    pub refused: Vec<String>,
}

/// What a run in flight was started for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Schedule,
    Hook,
}

/// A run in flight, or one that ended and whose result isn't stored yet.
struct Active<R> {
    /// The run, while it runs; none once it didn't start.
    run: Option<R>,
    channel: String,
    due: Timestamp,
    kind: Kind,
    /// What it came to, once it ended: a schedule run's result stays here
    /// until the store has it.
    ended: Option<Outcome>,
}

/// One run to start: what [`Dispatcher::start`] needs, from a schedule
/// or a hook.
struct Start<'a> {
    id: &'a str,
    task: &'a str,
    grants: &'a [ScheduleGrant],
    channel: &'a str,
    due: Timestamp,
    missed: bool,
    depth: u32,
    event: Option<HookSource>,
    space: Option<&'a str>,
    feed: Option<&'a FeedSpec>,
    kind: Kind,
}

/// The dispatcher's state: the runs in flight, one per schedule at most.
pub struct Dispatcher<R> {
    active: HashMap<String, Active<R>>,
}

impl<R> Default for Dispatcher<R> {
    fn default() -> Self {
        Self {
            active: HashMap::new(),
        }
    }
}

impl<R> std::fmt::Debug for Dispatcher<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("active", &self.active.len())
            .finish()
    }
}

/// The services one tick uses.
pub struct Services<'a, S, L, D> {
    /// The schedule store.
    pub store: &'a S,
    /// The run launcher.
    pub launcher: &'a L,
    /// The delivery path.
    pub delivery: &'a D,
    /// What the creator holds right now.
    pub held: &'a BTreeSet<ScheduleGrant>,
    /// Events the broker observed outside the dispatcher since the last
    /// tick: messages that arrived through its authenticated ingress.
    pub events: &'a [HookSource],
    /// The channels whose messages the broker authenticates.
    pub authenticated: &'a BTreeSet<String>,
}

impl<R: Run> Dispatcher<R> {
    /// How many runs are in flight.
    #[must_use]
    pub fn active(&self) -> usize {
        self.active.len()
    }

    /// Runs one tick at `now`. See the module docs for what it does.
    ///
    /// # Errors
    ///
    /// Returns the store's error if the schedules cannot be loaded.
    /// Failures on a single schedule are logged and reported, and do not
    /// stop the tick.
    pub fn tick<S, L, D>(
        &mut self,
        now: Timestamp,
        services: &Services<'_, S, L, D>,
    ) -> Result<TickReport, S::Error>
    where
        S: Store,
        L: Launcher<Run = R>,
        D: Delivery,
    {
        let mut report = TickReport::default();
        let finished = self.store_finished(services, &mut report);
        // What finished is delivered before anything fires, so a schedule
        // whose last fire just ended can claim its next one.
        deliver_stored(services, &mut report)?;
        for schedule in services.store.load()? {
            let Some(due) = schedule.due() else {
                continue;
            };
            if due > now {
                continue;
            }
            self.fire(&schedule, due, now, services, &mut report);
        }
        deliver_stored(services, &mut report)?;
        let mut events: Vec<HookSource> = Vec::new();
        for event in services.events {
            if !authenticated(event, services.authenticated) {
                tracing::warn!(%event, "event from a source the broker cannot vouch for: refused");
                report.refused_events.push(event.clone());
            } else if !events.contains(event) {
                events.push(event.clone());
            }
        }
        events.extend(finished);
        if !events.is_empty() {
            let hooks = services.store.hooks()?;
            for event in &events {
                for hook in hooks.iter().filter(|hook| hook.source == *event) {
                    self.start(
                        &Start {
                            id: &hook.id,
                            task: &hook.task,
                            grants: &hook.grants,
                            channel: &hook.channel,
                            due: now,
                            missed: false,
                            depth: 0,
                            event: Some(event.clone()),
                            space: None,
                            feed: None,
                            kind: Kind::Hook,
                        },
                        services,
                        &mut report,
                    );
                }
            }
        }
        Ok(report)
    }

    /// Takes what ended, stores it ([`store_ended`](Self::store_ended)),
    /// and returns the events that asserts: one
    /// [`HookSource::ScheduleRunFinished`] per schedule run that ended.
    fn store_finished<S: Store, L, D: Delivery>(
        &mut self,
        services: &Services<'_, S, L, D>,
        report: &mut TickReport,
    ) -> Vec<HookSource> {
        let mut events = Vec::new();
        for (id, active) in &mut self.active {
            if active.ended.is_some() {
                continue;
            }
            let Some(outcome) = active.run.as_mut().and_then(Run::poll) else {
                continue;
            };
            active.ended = Some(match outcome {
                Ok(text) => Outcome::Output { text },
                Err(reason) => Outcome::Failed { reason },
            });
            if active.kind == Kind::Schedule {
                events.push(HookSource::ScheduleRunFinished {
                    schedule_id: id.clone(),
                });
            }
        }
        self.store_ended(services, report);
        events
    }

    /// Stores each result that ended and isn't stored yet. A schedule
    /// run's result is stored before anything else is done with it
    /// (WO-025): it stays in memory, and is tried again next tick, until
    /// the store has it. A hook run's result is delivered, or parked, at
    /// once.
    fn store_ended<S: Store, L, D: Delivery>(
        &mut self,
        services: &Services<'_, S, L, D>,
        report: &mut TickReport,
    ) {
        let ended: Vec<String> = self
            .active
            .iter()
            .filter(|(_, active)| active.ended.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in ended {
            let Some(active) = self.active.get(&id) else {
                continue;
            };
            let Some(outcome) = active.ended.clone() else {
                continue;
            };
            if active.kind == Kind::Hook {
                if let Some(active) = self.active.remove(&id) {
                    deliver_now(&id, &active, &outcome, services, report);
                }
                continue;
            }
            let stored = services.store.record(
                &id,
                active.due.as_second(),
                OccurrenceState::Finished {
                    outcome: outcome.clone(),
                },
            );
            match stored {
                Ok(true) => {
                    self.active.remove(&id);
                }
                Ok(false) => {
                    // Cancelled while it ran: there is no record to keep
                    // the result in, so it is delivered or parked now.
                    if let Some(active) = self.active.remove(&id) {
                        deliver_now(&id, &active, &outcome, services, report);
                    }
                }
                Err(error) => {
                    tracing::error!(schedule = %id, %error, "a finished run's result not stored yet: kept, and tried again");
                }
            }
        }
    }

    fn fire<S: Store, L: Launcher<Run = R>, D: Delivery>(
        &mut self,
        schedule: &Schedule,
        due: Timestamp,
        now: Timestamp,
        services: &Services<'_, S, L, D>,
        report: &mut TickReport,
    ) {
        let id = schedule.id.clone();
        let Ok(timing) = Timing::parse(&schedule.when) else {
            // Its timezone left the system database, say. It can never
            // be evaluated again: remove it, loudly.
            tracing::error!(schedule = %id, "schedule no longer evaluable: removed");
            let _ = services.store.remove(&id);
            report.removed.push(id);
            return;
        };
        // Resume the cadence from now: never once per missed occurrence.
        // A one-shot has no fire after its own, and is exhausted.
        let next = timing.next_after(now).map(Timestamp::as_second);
        let revision = schedule.ledger.revision;
        let running = matches!(
            schedule.ledger.occurrence,
            Some(Occurrence {
                state: OccurrenceState::Claimed | OccurrenceState::Running,
                ..
            })
        );
        if running || self.active.contains_key(&id) {
            tracing::warn!(schedule = %id, "previous run still active: fire skipped");
            if let Err(error) = services
                .store
                .skip_due(&id, revision, due.as_second(), next)
            {
                tracing::error!(schedule = %id, %error, "a skipped fire's cadence not moved on");
            }
            report.skipped.push(id);
            return;
        }
        let missed = now.as_second() >= due.as_second().saturating_add(MISSED_GRACE_SECS);
        match services
            .store
            .claim_due(&id, revision, due.as_second(), next, missed)
        {
            Ok(Claim::Claimed) => self.start(
                &Start {
                    id: &id,
                    task: &schedule.task,
                    grants: &schedule.grants,
                    channel: &schedule.channel,
                    due,
                    missed,
                    depth: schedule.depth,
                    event: None,
                    space: schedule.space.as_deref(),
                    feed: schedule.feed.as_ref(),
                    kind: Kind::Schedule,
                },
                services,
                report,
            ),
            Ok(Claim::NotDue) => {}
            Ok(Claim::Full) => {
                tracing::error!(schedule = %id, "its last result still waits to be delivered or kept: this fire is refused");
                report.refused.push(id);
            }
            Err(error) => {
                tracing::error!(schedule = %id, %error, "the fire could not be claimed: nothing launched");
            }
        }
    }

    /// Starts one run of a schedule or hook: skipped while its previous
    /// run is active, with the grants the creator still holds. A schedule
    /// run starts only on a committed claim; one that can't start is
    /// recorded as such, and parked from the store.
    fn start<S: Store, L: Launcher<Run = R>, D: Delivery>(
        &mut self,
        start: &Start<'_>,
        services: &Services<'_, S, L, D>,
        report: &mut TickReport,
    ) {
        let id = start.id.to_owned();
        if self.active.contains_key(&id) {
            tracing::warn!(schedule = %id, "previous run still active: fire skipped");
            report.skipped.push(id);
            return;
        }
        let (grants, revoked) = effective_grants(start.grants, services.held);
        for grant in revoked {
            tracing::warn!(schedule = %id, ?grant, "revoked grant withheld from the run");
            report.revoked.push((id.clone(), grant));
        }
        let request = RunRequest {
            schedule_id: id.clone(),
            task: start.task.to_owned(),
            grants,
            due: start.due,
            missed: start.missed,
            channel: start.channel.to_owned(),
            depth: start.depth,
            event: start.event.clone(),
            space: start.space.map(str::to_owned),
            feed: start.feed.cloned(),
            chat: false,
            history: Vec::new(),
        };
        let launched = services.launcher.launch(request);
        let (run, ended) = match launched {
            Ok(run) => {
                if start.kind == Kind::Schedule {
                    let running =
                        services
                            .store
                            .record(&id, start.due.as_second(), OccurrenceState::Running);
                    if let Err(error) = running {
                        tracing::error!(schedule = %id, %error, "a started run not recorded as running");
                    }
                }
                report.launched.push((id.clone(), start.missed));
                (Some(run), None)
            }
            Err(failure) => {
                tracing::error!(schedule = %id, %failure, "scheduled run did not start");
                (None, Some(Outcome::NotStarted { reason: failure }))
            }
        };
        self.active.insert(
            id,
            Active {
                run,
                channel: start.channel.to_owned(),
                due: start.due,
                kind: start.kind,
                ended,
            },
        );
        // A run that didn't start is stored, or for a hook parked, now.
        if self.active.values().any(|active| active.run.is_none()) {
            self.store_ended(services, report);
        }
    }
}

/// Delivers every result the store holds that is still waiting (WO-025):
/// to its channel, when the creator still holds `channel-send` on it, or
/// parked for the user. A result is resolved only once it is delivered or
/// kept; one that can't be either stays where it is, for a later tick.
///
/// A result delivered whose resolution then fails is delivered again next
/// tick: delivery is at least once.
fn deliver_stored<S: Store, L, D: Delivery>(
    services: &Services<'_, S, L, D>,
    report: &mut TickReport,
) -> Result<(), S::Error> {
    for schedule in services.store.load()? {
        let Some(Occurrence {
            due,
            state: OccurrenceState::Finished { outcome },
            ..
        }) = &schedule.ledger.occurrence
        else {
            continue;
        };
        let id = &schedule.id;
        let parked = |text: &str, reason: String| Parked {
            schedule_id: id.clone(),
            channel: schedule.channel.clone(),
            text: text.to_owned(),
            due: *due,
            reason,
        };
        let undelivered = undelivered(outcome, &schedule.channel, services, &parked);
        match services.store.resolve(id, *due, undelivered.as_ref()) {
            Ok(true) => {
                match undelivered {
                    None => report.delivered.push(id.clone()),
                    Some(parked) => {
                        tracing::warn!(schedule = %id, reason = %parked.reason, "scheduled result parked");
                        report.parked.push(id.clone());
                    }
                }
                if schedule.ledger.exhausted {
                    report.removed.push(id.clone());
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::error!(schedule = %id, %error, "a result neither delivered nor kept yet: it stays stored");
            }
        }
    }
    Ok(())
}

/// What becomes of `outcome`: `None` once delivered on `channel`, or the
/// record to park it under.
fn undelivered<S, L, D: Delivery>(
    outcome: &Outcome,
    channel: &str,
    services: &Services<'_, S, L, D>,
    parked: &impl Fn(&str, String) -> Parked,
) -> Option<Parked> {
    match outcome {
        Outcome::Failed { reason } => Some(parked(reason, String::from("the run failed"))),
        Outcome::NotStarted { reason } => {
            Some(parked(reason, String::from("the run did not start")))
        }
        Outcome::Output { text } => {
            let send = ScheduleGrant::ChannelSend {
                channel: channel.to_owned(),
            };
            if services.held.contains(&send) {
                deliver_with_retry(services.delivery, channel, text)
                    .err()
                    .map(|error| parked(text, format!("delivery failed twice: {error}")))
            } else {
                Some(parked(
                    text,
                    String::from("channel-send was revoked before delivery"),
                ))
            }
        }
    }
}

/// Delivers, or parks, a result that has no record to wait in: a hook's,
/// or a schedule's cancelled while its run ran.
fn deliver_now<R, S: Store, L, D: Delivery>(
    id: &str,
    active: &Active<R>,
    outcome: &Outcome,
    services: &Services<'_, S, L, D>,
    report: &mut TickReport,
) {
    let parked = |text: &str, reason: String| Parked {
        schedule_id: id.to_owned(),
        channel: active.channel.clone(),
        text: text.to_owned(),
        due: active.due.as_second(),
        reason,
    };
    match undelivered(outcome, &active.channel, services, &parked) {
        None => report.delivered.push(id.to_owned()),
        Some(parked) => {
            tracing::warn!(schedule = %id, reason = %parked.reason, "scheduled result parked");
            if let Err(error) = services.store.park(&parked) {
                tracing::error!(schedule = %id, %error, "parking a result failed");
            }
            report.parked.push(id.to_owned());
        }
    }
}

/// Whether an event observed outside the dispatcher is one the broker
/// can vouch for: a message on a channel it authenticates. A finished
/// run is never one: only the dispatcher asserts that, from its own
/// state.
fn authenticated(event: &HookSource, channels: &BTreeSet<String>) -> bool {
    match event {
        HookSource::ChannelMessage { channel } => channels.contains(channel),
        HookSource::ScheduleRunFinished { .. } => false,
    }
}

/// Delivers, retrying once after a logged failure.
fn deliver_with_retry<D: Delivery>(delivery: &D, channel: &str, text: &str) -> Result<(), String> {
    match delivery.deliver(channel, text) {
        Ok(()) => Ok(()),
        Err(first) => {
            tracing::warn!(channel, error = %first, "scheduled delivery failed; retrying once");
            delivery.deliver(channel, text)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// A store that holds nothing and keeps what is parked.
    #[derive(Default)]
    struct Nothing(RefCell<Vec<Parked>>);

    impl Store for Nothing {
        type Error = String;

        fn load(&self) -> Result<Vec<Schedule>, String> {
            Ok(Vec::new())
        }

        fn claim_due(
            &self,
            _id: &str,
            _revision: u64,
            _due: i64,
            _next_due: Option<i64>,
            _missed: bool,
        ) -> Result<Claim, String> {
            Ok(Claim::NotDue)
        }

        fn skip_due(
            &self,
            _id: &str,
            _revision: u64,
            _due: i64,
            _next_due: Option<i64>,
        ) -> Result<bool, String> {
            Ok(false)
        }

        fn record(&self, _id: &str, _due: i64, _state: OccurrenceState) -> Result<bool, String> {
            Ok(true)
        }

        fn resolve(&self, _id: &str, _due: i64, _parked: Option<&Parked>) -> Result<bool, String> {
            Ok(true)
        }

        fn remove(&self, _id: &str) -> Result<bool, String> {
            Ok(true)
        }

        fn hooks(&self) -> Result<Vec<Hook>, String> {
            Ok(Vec::new())
        }

        fn park(&self, parked: &Parked) -> Result<(), String> {
            self.0.borrow_mut().push(parked.clone());
            Ok(())
        }
    }

    /// A run that never ends.
    struct Running;

    impl Run for Running {
        fn poll(&mut self) -> Option<Result<String, String>> {
            None
        }
    }

    /// A launcher that keeps what it was asked to start.
    #[derive(Default)]
    struct Asked(RefCell<Vec<RunRequest>>);

    impl Launcher for Asked {
        type Run = Running;

        fn launch(&self, request: RunRequest) -> Result<Running, String> {
            self.0.borrow_mut().push(request);
            Ok(Running)
        }
    }

    struct Nowhere;

    impl Delivery for Nowhere {
        fn deliver(&self, _channel: &str, _text: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// Delivers everything, and says nothing more.
    struct Plain;

    impl Delivery for Plain {
        fn deliver(&self, _channel: &str, _text: &str) -> Result<(), String> {
            Ok(())
        }
    }

    /// Refuses everything.
    struct Closed;

    impl Delivery for Closed {
        fn deliver(&self, _channel: &str, _text: &str) -> Result<(), String> {
            Err(String::from("closed"))
        }
    }

    /// WO-026: a delivery path that can't tell what the person was shown
    /// never claims they saw the text itself.
    #[test]
    fn a_delivery_that_cant_tell_never_says_the_text_was_shown() {
        assert_eq!(Plain.deliver_verbatim("local", "hi"), Ok(false));
        assert_eq!(
            Closed.deliver_verbatim("local", "hi"),
            Err(String::from("closed"))
        );
    }

    #[test]
    fn a_started_run_is_asked_for_as_scheduled_work_never_a_chat_turn() {
        let (store, launcher) = (Nothing::default(), Asked::default());
        let (held, authenticated) = (BTreeSet::new(), BTreeSet::new());
        let services = Services {
            store: &store,
            launcher: &launcher,
            delivery: &Nowhere,
            held: &held,
            events: &[],
            authenticated: &authenticated,
        };
        let mut dispatcher = Dispatcher::<Running>::default();
        let mut report = TickReport::default();
        let due = Timestamp::from_second(1_790_000_000).unwrap();
        let start = Start {
            id: "sch-1",
            task: "tidy the inbox",
            grants: &[],
            channel: "local",
            due,
            missed: true,
            depth: 2,
            event: None,
            space: Some("garden"),
            feed: None,
            kind: Kind::Schedule,
        };
        dispatcher.start(&start, &services, &mut report);
        let asked = launcher.0.borrow();
        let request = asked.first().unwrap();
        assert_eq!(
            (
                request.schedule_id.as_str(),
                request.task.as_str(),
                request.channel.as_str()
            ),
            ("sch-1", "tidy the inbox", "local")
        );
        assert_eq!((request.due, request.missed, request.depth), (due, true, 2));
        assert_eq!(request.space.as_deref(), Some("garden"));
        assert!(
            !request.chat,
            "scheduled work speaks with the worker's prompt"
        );
        assert_eq!(report.launched, [(String::from("sch-1"), true)]);
        assert_eq!(dispatcher.active(), 1);
        drop(asked);
        // A second start while it runs is skipped, not launched again.
        dispatcher.start(&start, &services, &mut report);
        assert_eq!(launcher.0.borrow().len(), 1);
        assert_eq!(report.skipped, [String::from("sch-1")]);
    }
}
