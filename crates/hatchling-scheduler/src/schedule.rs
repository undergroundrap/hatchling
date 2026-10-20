//! Schedules: when they fire, what they may do, and how a create is
//! validated.
//!
//! A schedule's timing always names an IANA timezone, and its grants
//! are always a subset of what its creator holds. Both are checked
//! here, once when the schedule is created. The dispatcher checks the
//! grants again when it fires ([`effective_grants`]).

use std::collections::BTreeSet;

use hatchling_core::{FeedSpec, ScheduleGrant, ScheduleRefusal, ScheduleSpec};
use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::cron::{CronExpr, resolve};

/// Longest task text a schedule carries, in bytes.
pub const MAX_TASK_BYTES: usize = 4 * 1024;

/// Most grants one schedule may carry.
pub const MAX_GRANTS: usize = 64;

/// How long a follow-up chain may grow: a run at this depth
/// may not ask for another. A schedule's or a hook's own run is depth
/// 0, so a chain is at most this many follow-ups long.
pub const MAX_FOLLOWUP_DEPTH: u32 = 5;

/// Longest delay a follow-up may ask for, in seconds: one day.
pub const MAX_FOLLOWUP_DELAY_SECS: u64 = 24 * 60 * 60;

/// A schedule's timing, parsed and resolved.
#[derive(Debug, Clone)]
pub enum Timing {
    /// A cron expression in a timezone.
    Cron {
        /// The expression.
        expr: CronExpr,
        /// Its timezone.
        tz: TimeZone,
    },
    /// One instant.
    Once {
        /// When.
        at: Timestamp,
    },
}

impl Timing {
    /// Parses a wire spec. The timezone is required and must be a known
    /// IANA name.
    ///
    /// # Errors
    ///
    /// Returns [`ScheduleRefusal::MissingTimezone`],
    /// [`ScheduleRefusal::UnknownTimezone`],
    /// [`ScheduleRefusal::InvalidSpec`], or, for a one-shot time that
    /// does not exist locally, [`ScheduleRefusal::NotInFuture`].
    pub fn parse(spec: &ScheduleSpec) -> Result<Self, ScheduleRefusal> {
        match spec {
            ScheduleSpec::Cron { expr, tz } => Ok(Self::Cron {
                expr: CronExpr::parse(expr).map_err(|_| ScheduleRefusal::InvalidSpec)?,
                tz: timezone(tz)?,
            }),
            ScheduleSpec::Once { at, tz } => {
                let tz = timezone(tz)?;
                let local: DateTime = at.parse().map_err(|_| ScheduleRefusal::InvalidSpec)?;
                let at = resolve(&tz, local).ok_or(ScheduleRefusal::NotInFuture)?;
                Ok(Self::Once { at })
            }
        }
    }

    /// The first fire strictly after `after`, or `None` if there is
    /// none: a one-shot already past, or a cron that never fires.
    #[must_use]
    pub fn next_after(&self, after: Timestamp) -> Option<Timestamp> {
        match self {
            Self::Cron { expr, tz } => expr.next_after(after, tz),
            Self::Once { at } => (*at > after).then_some(*at),
        }
    }

    /// Whether this timing fires at most once.
    #[must_use]
    pub const fn is_once(&self) -> bool {
        matches!(self, Self::Once { .. })
    }
}

/// Looks up an IANA timezone by name. There is no default: a blank name
/// is a missing one (a request that left `tz` out decodes with it empty).
fn timezone(name: &str) -> Result<TimeZone, ScheduleRefusal> {
    let name = Some(name.trim())
        .filter(|name| !name.is_empty())
        .ok_or(ScheduleRefusal::MissingTimezone)?;
    jiff::tz::db()
        .get(name)
        .map_err(|_| ScheduleRefusal::UnknownTimezone)
}

/// Who created a schedule. The broker records it; a cell lists and
/// cancels only the agent's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Creator {
    /// The owner, through the local API, or the broker itself (a space
    /// task's run). It's also what a schedule stored before creators were
    /// recorded counts as, so a cell can't touch one it can't be shown to
    /// have made.
    #[default]
    Owner,
    /// An agent session: a cell, or a run asking for its follow-up.
    Agent,
}

impl Creator {
    /// Whether a caller that is `self` may see and cancel a schedule
    /// `made_by` created: the owner, any; the agent, only its own.
    #[must_use]
    pub fn reaches(self, made_by: Self) -> bool {
        self == Self::Owner || made_by == self
    }
}

/// One stored schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// The broker-minted id.
    pub id: String,
    /// When it fires, as created.
    pub when: ScheduleSpec,
    /// The task a run is given.
    pub task: String,
    /// The grants a run may use, as created.
    pub grants: Vec<ScheduleGrant>,
    /// Where a run's output goes.
    pub channel: String,
    /// The next fire, in Unix seconds, broker clock.
    pub next_due: i64,
    /// Its place in a follow-up chain: 0 for a schedule a session
    /// created, and one more than its parent run's for a follow-up.
    /// Broker-set; never on the wire.
    #[serde(default)]
    pub depth: u32,
    /// For a space task's one-shot run, the space it is a task
    /// of. Broker-set; never on the wire.
    #[serde(default)]
    pub space: Option<String>,
    /// For a feed, what its digest gathers: its run is generated
    /// by the broker, read-only. Broker-set at `CreateFeed`.
    #[serde(default)]
    pub feed: Option<FeedSpec>,
    /// Who created it (see [`Creator`]). Broker-set; never on the wire.
    #[serde(default)]
    pub creator: Creator,
    /// Its claimed fire, until that fire is resolved (WO-025). Broker-set;
    /// never on the wire.
    #[serde(default)]
    pub ledger: Ledger,
}

impl Schedule {
    /// The next fire as a timestamp: none once a one-shot's fire is
    /// claimed.
    #[must_use]
    pub fn due(&self) -> Option<Timestamp> {
        if self.ledger.exhausted {
            return None;
        }
        Timestamp::from_second(self.next_due).ok()
    }
}

/// What a schedule's record keeps beside its definition (WO-025): its
/// claimed fire, from the claim until the fire's result is delivered or
/// kept for the user.
///
/// A record holds one unresolved fire at most. A fire is claimed in the
/// same write that moves the cadence on, so one never happens without the
/// other, and the run is launched only once that write is committed. While
/// a fire's run is claimed or running, a later fire is skipped; while its
/// result waits to be delivered, a later fire's claim is refused. So a
/// record stays within one stored result, and nothing unresolved is ever
/// dropped to make room.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// Moves on with every write of the record: a claim made from an
    /// older read of it is refused.
    #[serde(default)]
    pub revision: u64,
    /// A one-shot whose fire has been claimed: never due again, and
    /// removed once that fire is resolved.
    #[serde(default)]
    pub exhausted: bool,
    /// The claimed fire not yet resolved, if there is one.
    #[serde(default)]
    pub occurrence: Option<Occurrence>,
}

/// One fire of a schedule, from its claim until it is resolved (WO-025).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    /// When it was due, in Unix seconds: with the schedule's id, it names
    /// the fire.
    pub due: i64,
    /// Whether it was claimed late, because the broker missed it.
    pub missed: bool,
    /// Where it has got to.
    pub state: OccurrenceState,
}

/// Where a claimed fire has got to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum OccurrenceState {
    /// Claimed, and its cadence moved on. A claim doesn't prove the run
    /// began.
    Claimed,
    /// Its run was started.
    Running,
    /// Its run ended, or never started: the result, kept until it is
    /// delivered or parked. A result is stored before it is delivered.
    Finished {
        /// What the run came to.
        outcome: Outcome,
    },
    /// Claimed or running when the broker stopped: whether its run began,
    /// ended, or acted is not known. It is never marked succeeded, never
    /// described as failed before doing anything, and never run again on
    /// its own: it holds its schedule until the owner marks it resolved
    /// or retries it as a new attempt.
    Unknown {
        /// Whether its run had been started.
        started: bool,
    },
}

/// What a fire's run came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    /// Its output, to deliver.
    Output {
        /// The output.
        text: String,
    },
    /// It failed.
    Failed {
        /// Why.
        reason: String,
    },
    /// It didn't start.
    NotStarted {
        /// Why.
        reason: String,
    },
}

/// A create, validated against the creator's grants at `now`: the
/// schedule to store, and its first fire.
///
/// Checks run in a fixed order, so each refusal names the first fault:
/// the task, the grant count, the timing, the grants, the delivery
/// channel, and finally whether it fires at all.
///
/// # Errors
///
/// Returns the [`ScheduleRefusal`] for the first fault.
pub fn validate(
    id: String,
    when: ScheduleSpec,
    task: String,
    grants: Vec<ScheduleGrant>,
    channel: String,
    held: &BTreeSet<ScheduleGrant>,
    now: Timestamp,
) -> Result<Schedule, ScheduleRefusal> {
    check_task(&task, &grants)?;
    let timing = Timing::parse(&when)?;
    check_grants(&grants, &channel, held)?;
    let first = timing.next_after(now).ok_or_else(|| {
        if timing.is_once() {
            ScheduleRefusal::NotInFuture
        } else {
            ScheduleRefusal::InvalidSpec
        }
    })?;
    Ok(Schedule {
        id,
        when,
        task,
        grants,
        channel,
        next_due: first.as_second(),
        depth: 0,
        space: None,
        feed: None,
        creator: Creator::Owner,
        ledger: Ledger::default(),
    })
}

/// The task and grant-count checks every create runs first.
pub(crate) fn check_task(task: &str, grants: &[ScheduleGrant]) -> Result<(), ScheduleRefusal> {
    if task.trim().is_empty() || task.len() > MAX_TASK_BYTES {
        return Err(ScheduleRefusal::BadTask);
    }
    if grants.len() > MAX_GRANTS {
        return Err(ScheduleRefusal::TooManyGrants);
    }
    Ok(())
}

/// Subset-closure and the delivery channel: every grant is one `held`
/// holds, and the grants include sending on `channel`.
pub(crate) fn check_grants(
    grants: &[ScheduleGrant],
    channel: &str,
    held: &BTreeSet<ScheduleGrant>,
) -> Result<(), ScheduleRefusal> {
    if grants.iter().any(|grant| !held.contains(grant)) {
        return Err(ScheduleRefusal::ExceedsGrants);
    }
    let send = ScheduleGrant::ChannelSend {
        channel: channel.to_owned(),
    };
    if !grants.contains(&send) {
        return Err(ScheduleRefusal::ChannelNotGranted);
    }
    Ok(())
}

/// What the broker knows about a run that asks for a follow-up. None of
/// it comes from the request: the broker set it when it started the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowupParent<'a> {
    /// The run's own grants, as it was started with them.
    pub grants: &'a [ScheduleGrant],
    /// The channel its output goes to; the follow-up's goes there too.
    pub channel: &'a str,
    /// The run's depth in its follow-up chain.
    pub depth: u32,
}

/// A follow-up request from `parent`, validated at `now`: a one-shot
/// schedule `delay_secs` from now, one deeper in the chain.
///
/// This is subset-closure applied to the *run's* authority: the
/// follow-up may hold only grants the run holds, so a chain can only
/// narrow. The depth bound comes first, so a run at the bound is
/// refused whatever else it asks.
///
/// # Errors
///
/// [`ScheduleRefusal::MaxFollowupDepth`] at the bound,
/// [`ScheduleRefusal::BadDelay`] for a zero or over-long delay, then
/// the same refusals as [`validate`], with the run's grants as the
/// held set.
pub fn validate_followup(
    id: String,
    parent: &FollowupParent<'_>,
    task: String,
    delay_secs: u64,
    grants: Vec<ScheduleGrant>,
    now: Timestamp,
) -> Result<Schedule, ScheduleRefusal> {
    if parent.depth >= MAX_FOLLOWUP_DEPTH {
        return Err(ScheduleRefusal::MaxFollowupDepth);
    }
    if delay_secs == 0 || delay_secs > MAX_FOLLOWUP_DELAY_SECS {
        return Err(ScheduleRefusal::BadDelay);
    }
    // Whole seconds: the stored due time is in seconds, so a fractional
    // fire time would be due a fraction before the one-shot is spent, and
    // fire twice.
    let at = i64::try_from(delay_secs)
        .ok()
        .and_then(|secs| now.as_second().checked_add(secs))
        .and_then(|secs| Timestamp::from_second(secs).ok())
        .ok_or(ScheduleRefusal::BadDelay)?;
    let when = ScheduleSpec::Once {
        at: at.to_zoned(TimeZone::UTC).datetime().to_string(),
        tz: String::from("UTC"),
    };
    let held: BTreeSet<ScheduleGrant> = parent.grants.iter().cloned().collect();
    let mut schedule = validate(
        id,
        when,
        task,
        grants,
        parent.channel.to_owned(),
        &held,
        now,
    )?;
    schedule.depth = parent.depth.saturating_add(1);
    // A follow-up is asked for by a run, an agent session.
    schedule.creator = Creator::Agent;
    Ok(schedule)
}

/// A space task's run, validated at `now`: a one-shot schedule
/// due one second from now, bound to `space`.
///
/// Subset-closure again, one level down: the run may hold only grants
/// the space holds, so a task can never reach outside its space. The
/// dispatcher then narrows them once more, at fire time, to those the
/// creator still holds.
///
/// # Errors
///
/// The same refusals as [`validate`], with the space's grants as the held
/// set.
pub fn validate_space_task(
    id: String,
    space: &str,
    task: String,
    grants: Vec<ScheduleGrant>,
    channel: String,
    space_grants: &BTreeSet<ScheduleGrant>,
    now: Timestamp,
) -> Result<Schedule, ScheduleRefusal> {
    let at = now
        .as_second()
        .checked_add(1)
        .and_then(|secs| Timestamp::from_second(secs).ok())
        .ok_or(ScheduleRefusal::BadDelay)?;
    let when = ScheduleSpec::Once {
        at: at.to_zoned(TimeZone::UTC).datetime().to_string(),
        tz: String::from("UTC"),
    };
    let mut schedule = validate(id, when, task, grants, channel, space_grants, now)?;
    schedule.space = Some(space.to_owned());
    Ok(schedule)
}

/// A run's grants at fire time: the schedule's grants the creator still
/// holds, and the ones since revoked.
///
/// This is the fire-time half of subset-closure. A grant revoked after
/// the schedule was created is not the run's to use, however the
/// schedule was stored.
#[must_use]
pub fn effective_grants(
    grants: &[ScheduleGrant],
    held: &BTreeSet<ScheduleGrant>,
) -> (Vec<ScheduleGrant>, Vec<ScheduleGrant>) {
    grants
        .iter()
        .cloned()
        .partition(|grant| held.contains(grant))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_reaches_every_schedule_and_the_agent_only_its_own() {
        assert!(Creator::Owner.reaches(Creator::Owner));
        assert!(Creator::Owner.reaches(Creator::Agent));
        assert!(Creator::Agent.reaches(Creator::Agent));
        assert!(!Creator::Agent.reaches(Creator::Owner));
        assert_eq!(Creator::default(), Creator::Owner);
    }

    #[test]
    fn a_timezone_is_named_and_known_or_refused_by_name() {
        for blank in ["", " ", "\t"] {
            assert_eq!(
                timezone(blank).unwrap_err(),
                ScheduleRefusal::MissingTimezone,
                "{blank:?}"
            );
        }
        assert_eq!(
            timezone("Nowhere/Atlantis").unwrap_err(),
            ScheduleRefusal::UnknownTimezone
        );
        assert_eq!(
            timezone(" Europe/Berlin ").unwrap().iana_name(),
            Some("Europe/Berlin")
        );
    }

    fn now() -> Timestamp {
        // 2026-09-21T14:13:20Z
        Timestamp::from_second(1_790_000_000).unwrap()
    }

    fn cron(expr: &str, tz: Option<&str>) -> ScheduleSpec {
        ScheduleSpec::Cron {
            expr: expr.to_owned(),
            tz: tz.unwrap_or_default().to_owned(),
        }
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

    fn create(
        when: ScheduleSpec,
        task: &str,
        grants: Vec<ScheduleGrant>,
    ) -> Result<Schedule, ScheduleRefusal> {
        validate(
            String::from("sch-1"),
            when,
            task.to_owned(),
            grants,
            String::from("local"),
            &held(),
            now(),
        )
    }

    #[test]
    fn a_valid_cron_schedule_is_stored_with_its_first_fire() {
        let schedule = create(
            cron("0 9 * * *", Some("Europe/Berlin")),
            "summarize the inbox",
            vec![send("local"), egress("api.example.com:443")],
        )
        .unwrap();
        assert_eq!(schedule.id, "sch-1");
        assert_eq!(schedule.channel, "local");
        // 09:00 Berlin (CEST, UTC+2) on 2026-09-22.
        assert_eq!(
            schedule.due().map(|t| t.to_string()),
            Some(String::from("2026-09-22T07:00:00Z"))
        );
    }

    #[test]
    fn a_one_shot_resolves_its_local_time() {
        let schedule = create(
            ScheduleSpec::Once {
                at: String::from("2026-10-01T09:00"),
                tz: String::from("America/New_York"),
            },
            "ping",
            vec![send("local")],
        )
        .unwrap();
        assert_eq!(
            schedule.due().map(|t| t.to_string()),
            Some(String::from("2026-10-01T13:00:00Z"))
        );
    }

    fn ok() -> Vec<ScheduleGrant> {
        vec![send("local")]
    }

    fn once(at: &str, tz: Option<&str>) -> ScheduleSpec {
        ScheduleSpec::Once {
            at: at.to_owned(),
            tz: tz.unwrap_or_default().to_owned(),
        }
    }

    #[test]
    fn one_shot_refusals_name_their_fault() {
        for (spec, refusal) in [
            (
                once("2026-01-01T09:00", Some("UTC")),
                ScheduleRefusal::NotInFuture,
            ),
            // Inside New York's spring-forward gap: no such local time.
            (
                once("2027-03-14T02:30", Some("America/New_York")),
                ScheduleRefusal::NotInFuture,
            ),
            (once("tomorrow", Some("UTC")), ScheduleRefusal::InvalidSpec),
            (
                once("2027-01-01T09:00", None),
                ScheduleRefusal::MissingTimezone,
            ),
        ] {
            assert_eq!(
                create(spec.clone(), "t", ok()).err(),
                Some(refusal),
                "{spec:?}"
            );
        }
    }

    #[test]
    fn every_refusal_names_its_fault() {
        let cases: Vec<(Result<Schedule, ScheduleRefusal>, ScheduleRefusal)> = vec![
            (
                create(cron("0 9 * * *", Some("UTC")), "", ok()),
                ScheduleRefusal::BadTask,
            ),
            (
                create(cron("0 9 * * *", Some("UTC")), "   ", ok()),
                ScheduleRefusal::BadTask,
            ),
            (
                create(
                    cron("0 9 * * *", Some("UTC")),
                    &"x".repeat(MAX_TASK_BYTES + 1),
                    ok(),
                ),
                ScheduleRefusal::BadTask,
            ),
            (
                create(
                    cron("0 9 * * *", Some("UTC")),
                    "t",
                    vec![send("local"); MAX_GRANTS + 1],
                ),
                ScheduleRefusal::TooManyGrants,
            ),
            (
                create(cron("0 9 * * *", None), "t", ok()),
                ScheduleRefusal::MissingTimezone,
            ),
            (
                create(cron("0 9 * * *", Some(" ")), "t", ok()),
                ScheduleRefusal::MissingTimezone,
            ),
            (
                create(cron("0 9 * * *", Some("Mars/Olympus")), "t", ok()),
                ScheduleRefusal::UnknownTimezone,
            ),
            (
                create(cron("0 25 * * *", Some("UTC")), "t", ok()),
                ScheduleRefusal::InvalidSpec,
            ),
            (
                create(cron("0 0 31 2 *", Some("UTC")), "t", ok()),
                ScheduleRefusal::InvalidSpec,
            ),
            (
                create(
                    cron("0 9 * * *", Some("UTC")),
                    "t",
                    vec![send("local"), egress("evil.example:443")],
                ),
                ScheduleRefusal::ExceedsGrants,
            ),
            (
                create(
                    cron("0 9 * * *", Some("UTC")),
                    "t",
                    vec![egress("api.example.com:443")],
                ),
                ScheduleRefusal::ChannelNotGranted,
            ),
        ];
        for (index, (result, refusal)) in cases.into_iter().enumerate() {
            assert_eq!(result.err(), Some(refusal), "case {index}");
        }
        // At the bounds: accepted.
        assert!(
            create(
                cron("0 9 * * *", Some("UTC")),
                &"x".repeat(MAX_TASK_BYTES),
                ok()
            )
            .is_ok()
        );
        let mut at_bound = vec![send("local"); MAX_GRANTS];
        at_bound.truncate(MAX_GRANTS);
        assert!(create(cron("0 9 * * *", Some("UTC")), "t", at_bound).is_ok());
        assert_eq!((MAX_TASK_BYTES, MAX_GRANTS), (4096, 64));
    }

    #[test]
    fn a_revoked_grant_is_not_the_run_s_to_use() {
        let grants = vec![send("local"), egress("api.example.com:443")];
        let (kept, revoked) = effective_grants(&grants, &held());
        assert_eq!(kept, grants);
        assert!(revoked.is_empty());
        let after_revocation = BTreeSet::from([send("local")]);
        let (kept, revoked) = effective_grants(&grants, &after_revocation);
        assert_eq!(kept, [send("local")]);
        assert_eq!(revoked, [egress("api.example.com:443")]);
    }

    fn followup(
        depth: u32,
        run_grants: &[ScheduleGrant],
        delay_secs: u64,
        grants: Vec<ScheduleGrant>,
    ) -> Result<Schedule, ScheduleRefusal> {
        validate_followup(
            String::from("sch-f"),
            &FollowupParent {
                grants: run_grants,
                channel: "local",
                depth,
            },
            String::from("look again"),
            delay_secs,
            grants,
            now(),
        )
    }

    #[test]
    fn a_followup_is_a_one_shot_one_deeper_on_the_run_s_channel() {
        let run = [send("local"), egress("api.example.com:443")];
        let schedule = followup(2, &run, 600, vec![send("local")]).unwrap();
        assert_eq!(schedule.id, "sch-f");
        assert_eq!(schedule.task, "look again");
        assert_eq!(schedule.channel, "local");
        assert_eq!(schedule.grants, [send("local")]);
        assert_eq!(schedule.depth, 3);
        assert_eq!(schedule.next_due, now().as_second() + 600);
        assert!(Timing::parse(&schedule.when).unwrap().is_once());
        assert_eq!(
            schedule.when,
            ScheduleSpec::Once {
                at: String::from("2026-09-21T14:23:20"),
                tz: String::from("UTC"),
            }
        );
        // A created schedule starts every chain at depth 0.
        let created = create(cron("0 9 * * *", Some("UTC")), "t", ok()).unwrap();
        assert_eq!(created.depth, 0);
    }

    #[test]
    fn a_followup_from_a_sub_second_clock_fires_once_on_a_whole_second() {
        let run = [send("local")];
        let now = Timestamp::from_second(1_790_000_000)
            .unwrap()
            .checked_add(jiff::SignedDuration::from_millis(750))
            .unwrap();
        let schedule = validate_followup(
            String::from("sch-f"),
            &FollowupParent {
                grants: &run,
                channel: "local",
                depth: 0,
            },
            String::from("t"),
            60,
            vec![send("local")],
            now,
        )
        .unwrap();
        assert_eq!(schedule.next_due, 1_790_000_060);
        let due = schedule.due().unwrap();
        let timing = Timing::parse(&schedule.when).unwrap();
        // Fired at its due second, it has no fire after: it is spent.
        assert_eq!(timing.next_after(due), None);
        assert_eq!(timing.next_after(now), Some(due));
    }

    #[test]
    fn a_followup_may_not_hold_more_than_its_run() {
        // The privilege pump: the creator holds the egress grant, but the
        // run asking for the follow-up does not, so neither may it.
        let run = [send("local")];
        assert!(held().contains(&egress("api.example.com:443")));
        assert_eq!(
            followup(
                0,
                &run,
                60,
                vec![send("local"), egress("api.example.com:443")]
            )
            .err(),
            Some(ScheduleRefusal::ExceedsGrants)
        );
        // Its output goes where the run's does, and it must be able to.
        assert_eq!(
            followup(0, &[send("local"), send("ops")], 60, vec![send("ops")]).err(),
            Some(ScheduleRefusal::ChannelNotGranted)
        );
        assert_eq!(
            followup(0, &run, 60, vec![send("local")]).map(|s| s.grants),
            Ok(vec![send("local")])
        );
    }

    #[test]
    fn the_depth_bound_ends_the_chain() {
        let run = [send("local")];
        // Depth 0 is a schedule's own run; MAX_FOLLOWUP_DEPTH - 1 is the
        // last that may ask.
        let mut depth = 0;
        while let Ok(next) = followup(depth, &run, 60, vec![send("local")]) {
            assert_eq!(next.depth, depth + 1);
            depth = next.depth;
        }
        assert_eq!(depth, MAX_FOLLOWUP_DEPTH, "the chain stops at the bound");
        // At the bound the refusal comes first, whatever else is wrong.
        assert_eq!(
            followup(MAX_FOLLOWUP_DEPTH, &run, 0, vec![egress("evil:1")]).err(),
            Some(ScheduleRefusal::MaxFollowupDepth)
        );
        assert_eq!(
            followup(u32::MAX, &run, 60, vec![send("local")]).err(),
            Some(ScheduleRefusal::MaxFollowupDepth)
        );
        assert_eq!(MAX_FOLLOWUP_DEPTH, 5);
    }

    #[test]
    fn a_followup_delay_is_bounded() {
        let run = [send("local")];
        for delay in [0, MAX_FOLLOWUP_DELAY_SECS + 1, u64::MAX] {
            assert_eq!(
                followup(0, &run, delay, vec![send("local")]).err(),
                Some(ScheduleRefusal::BadDelay),
                "{delay}"
            );
        }
        for delay in [1, MAX_FOLLOWUP_DELAY_SECS] {
            let schedule = followup(0, &run, delay, vec![send("local")]).unwrap();
            assert_eq!(
                schedule.next_due,
                now().as_second() + i64::try_from(delay).unwrap()
            );
        }
        assert_eq!(MAX_FOLLOWUP_DELAY_SECS, 86_400);
        // The task is checked as for any schedule.
        assert_eq!(
            validate_followup(
                String::from("sch-f"),
                &FollowupParent {
                    grants: &run,
                    channel: "local",
                    depth: 0,
                },
                String::new(),
                60,
                vec![send("local")],
                now(),
            )
            .err(),
            Some(ScheduleRefusal::BadTask)
        );
    }

    #[test]
    fn timings_report_their_next_fire() {
        let once = Timing::parse(&ScheduleSpec::Once {
            at: String::from("2026-09-21T14:14"),
            tz: String::from("UTC"),
        })
        .unwrap();
        assert!(once.is_once());
        let at = once.next_after(now()).unwrap();
        assert_eq!(at.to_string(), "2026-09-21T14:14:00Z");
        assert_eq!(once.next_after(at), None, "a one-shot fires once");
        let every = Timing::parse(&cron("* * * * *", Some("UTC"))).unwrap();
        assert!(!every.is_once());
        assert_eq!(
            every.next_after(now()).map(|t| t.to_string()),
            Some(String::from("2026-09-21T14:14:00Z"))
        );
    }
}
