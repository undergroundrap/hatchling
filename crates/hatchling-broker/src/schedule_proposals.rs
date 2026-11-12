//! Scheduled work the agent suggests (v19): put to the owner, and created
//! as theirs only if they approve.
//!
//! The agent can't schedule anything itself: a scheduled run acts later,
//! unattended, under grants, so what runs and when is the owner's to decide.
//! Here the agent proposes, in the vocabulary of `hatchling-broker schedule
//! add`; the broker reads the timing on the owner's clock and shows them
//! when it runs and what it may do, in its own words, and the task as the
//! agent's. Approved, the worker creates it as theirs, with the grants of
//! the session that asked, delivering to the chat.
//!
//! The agent isn't told the date, so a one-time proposal may name its day
//! in words (`today 17:00`, `tomorrow 09:00`), which the broker reads on
//! the owner's clock. A time that has already passed is never put to the
//! owner: the agent hears that, with the time it is now, and can ask again.

use hatchling_core::{ScheduleGrant, ScheduleOp, ScheduleRefusal, ScheduleResult, ScheduleSpec};
use hatchling_scheduler::{Creator, MAX_TASK_BYTES, Timestamp, Timing};
use hatchling_vault::OpensStores;

use crate::Config;
use crate::approval_board::board;
use crate::approvals::Refusal;

/// The channel a proposed schedule delivers to: the owner's chat.
pub const CHANNEL: &str = "local";

/// Puts `task`, to run as `timing` `value` says, which session task
/// `asking` proposes, to the owner, from a session configured by
/// `config`: what the agent is told.
#[must_use]
pub fn propose(
    config: &Config,
    asking: &str,
    timing: &str,
    value: &str,
    task: &str,
) -> ScheduleResult {
    let zone = hatchling_scheduler::system_zone().unwrap_or_else(|| String::from("UTC"));
    propose_at(
        config,
        asking,
        (timing, value),
        task,
        Timestamp::now(),
        &zone,
    )
}

/// [`propose`], at `now` on the clock of the time zone `zone`.
fn propose_at(
    config: &Config,
    asking: &str,
    (timing, value): (&str, &str),
    task: &str,
    now: Timestamp,
    zone: &str,
) -> ScheduleResult {
    let grants: Vec<ScheduleGrant> = crate::held_grants(config).into_iter().collect();
    let posts = grants.contains(&ScheduleGrant::ChannelSend {
        channel: String::from(CHANNEL),
    });
    if !posts || !config.channels.iter().any(|channel| channel == CHANNEL) {
        return ScheduleResult::Denied;
    }
    let task = task.trim();
    if task.is_empty()
        || task.len() > MAX_TASK_BYTES
        || task.chars().any(|c| c != '\n' && c.is_control())
    {
        return unasked("the task must be some text, at most 4 KiB, with no control characters");
    }
    let clock = hatchling_scheduler::local_time(now.as_second(), zone).unwrap_or_default();
    let value = if timing == "at" {
        dated(value, now.as_second(), zone)
    } else {
        value.to_owned()
    };
    let when = match crate::schedule_cli::when(&format!("--{timing}"), &value, zone.to_owned()) {
        Ok(when) => when,
        Err(_) if timing == "at" => {
            return unasked(&format!(
                "at takes \"YYYY-MM-DD HH:MM\", \"today HH:MM\" or \"tomorrow HH:MM\"; it is now {clock} ({zone})"
            ));
        }
        Err(reason) => return unasked(&reason.replace("--", "")),
    };
    if let Some(reason) = never_runs(&when, now, &clock) {
        return unasked(&reason);
    }
    let words = crate::schedule_cli::describe(&when);
    let now = now.as_second();
    match board(config).propose_schedule(&when, task, &grants, &words, asking, now) {
        Ok(_) => ScheduleResult::Proposed { when: words },
        Err(refusal) => unasked(match refusal {
            Refusal::Denied => "the owner said no to scheduling this",
            Refusal::Paused => "the owner's approval requests are paused for now",
            Refusal::QueueFull => "too many requests already wait for the owner",
            Refusal::Unfrozen(_) | Refusal::Entropy => {
                tracing::error!(error = %refusal, "a schedule proposal couldn't be put to the owner");
                "it couldn't be put to the owner"
            }
        }),
    }
}

/// `value` for a one-time proposal, with a day named in words made its
/// date on the clock of `zone` at `now`: `today 17:00`, `tomorrow at 9:00`.
/// Anything else is left as it is, for the timing's own reading.
fn dated(value: &str, now: i64, zone: &str) -> String {
    let asked = value.trim().to_ascii_lowercase();
    let on = |word: &str, days: i8| {
        let time = asked.strip_prefix(word)?.trim();
        let time = time.strip_prefix("at ").unwrap_or(time).trim();
        let date = hatchling_scheduler::local_date(now, zone, days)?;
        // `9:00` is read as `09:00`.
        let pad = if time.find(':') == Some(1) { "0" } else { "" };
        Some(format!("{date} {pad}{time}"))
    };
    on("today ", 0)
        .or_else(|| on("tomorrow ", 1))
        .unwrap_or_else(|| value.to_owned())
}

/// Why `when` could never run after `now`, if it couldn't: what the agent
/// is told, with the time it is now (`clock`), so it can ask again.
fn never_runs(when: &ScheduleSpec, now: Timestamp, clock: &str) -> Option<String> {
    let timing = match Timing::parse(when) {
        Ok(timing) => timing,
        Err(ScheduleRefusal::NotInFuture) => {
            return Some(String::from(
                "that time doesn't exist on the owner's clock: the clocks skip it that day",
            ));
        }
        Err(_) => return Some(String::from("that timing can't be read")),
    };
    if timing.next_after(now).is_some() {
        return None;
    }
    Some(match when {
        ScheduleSpec::Once { at, tz } => format!(
            "{} has already passed: it is now {clock} ({tz}). Ask again with a time that is still to come",
            at.replace('T', " ")
        ),
        ScheduleSpec::Cron { .. } => String::from("that timing never comes round"),
    })
}

/// Why an approved schedule wasn't created, in the owner's words.
fn why_not(created: &ScheduleResult, when: &ScheduleSpec) -> String {
    let refused = |reason: ScheduleRefusal| match reason {
        ScheduleRefusal::NotInFuture => format!(
            "its time had already passed ({})",
            crate::schedule_cli::describe(when)
        ),
        ScheduleRefusal::Full => {
            String::from("the schedule list is full. Remove one in Schedule, then ask again")
        }
        ScheduleRefusal::ExceedsGrants
        | ScheduleRefusal::ChannelNotGranted
        | ScheduleRefusal::TooManyGrants => {
            String::from("it asked for more than the chat that proposed it may do")
        }
        ScheduleRefusal::MissingTimezone
        | ScheduleRefusal::UnknownTimezone
        | ScheduleRefusal::InvalidSpec => String::from("its timing can't be read"),
        ScheduleRefusal::BadTask => String::from("its task text can't be used"),
        ScheduleRefusal::UnknownSource
        | ScheduleRefusal::MaxFollowupDepth
        | ScheduleRefusal::FollowupTaken
        | ScheduleRefusal::BadDelay => String::from("the broker refused it"),
    };
    match created {
        ScheduleResult::Refused { reason } => refused(*reason),
        ScheduleResult::Denied => String::from("scheduling is turned off on this broker"),
        ScheduleResult::ScheduleError => String::from(
            "the schedule store couldn't be written. `sudo hatchling-broker doctor` checks it",
        ),
        ScheduleResult::Created { .. }
        | ScheduleResult::Cancelled
        | ScheduleResult::NotFound
        | ScheduleResult::Listed { .. }
        | ScheduleResult::Resolved
        | ScheduleResult::Retried
        | ScheduleResult::Parked { .. }
        | ScheduleResult::Dismissed
        | ScheduleResult::Proposed { .. }
        | ScheduleResult::Unasked { .. } => String::from("the broker refused it"),
    }
}

fn unasked(reason: &str) -> ScheduleResult {
    ScheduleResult::Unasked {
        reason: reason.to_owned(),
    }
}

/// Creates the schedule the owner approved, as theirs, at `now`: what
/// they're told.
///
/// # Errors
///
/// What they're told when the broker refused it.
pub fn create(
    config: &Config,
    key: &dyn OpensStores,
    when: &ScheduleSpec,
    task: &str,
    grants: &[ScheduleGrant],
    now: Timestamp,
) -> Result<String, String> {
    let op = ScheduleOp::Create {
        when: when.clone(),
        task: task.to_owned(),
        grants: grants.to_vec(),
        channel: String::from(CHANNEL),
    };
    let created = crate::schedule_ops::handle_schedule_op(config, key, &op, now, Creator::Owner);
    if matches!(created, ScheduleResult::Created { .. }) {
        return Ok(format!(
            "Scheduled: “{task}”, {}. It's in Schedule, where you can remove it.",
            crate::schedule_cli::describe(when)
        ));
    }
    tracing::warn!(result = ?created, "an approved schedule wasn't created");
    Err(format!(
        "Approved, but it wasn't scheduled: {}.",
        why_not(&created, when)
    ))
}

/// What `grants` let a scheduled run do, in words, for the owner's card: a
/// store's grants as one phrase, where the first of them stood.
#[must_use]
pub fn grants_in_words(grants: &[ScheduleGrant]) -> String {
    let mut phrases: Vec<String> = Vec::new();
    let mut memory: Vec<(String, Vec<&str>, usize)> = Vec::new();
    let mut goals: Option<(bool, usize)> = None;
    for grant in grants {
        match grant {
            ScheduleGrant::ChannelSend { channel } if channel == CHANNEL => {
                phrases.push(String::from("post in your chat"));
            }
            ScheduleGrant::ChannelSend { channel } if channel == crate::feed_page::FEED_PAGE => {
                phrases.push(String::from("post on your Feed page"));
            }
            ScheduleGrant::ChannelSend { channel } if channel == crate::suggestions::IDEAS_PAGE => {
                phrases.push(String::from("suggest ideas on your Ideas page"));
            }
            ScheduleGrant::ChannelSend { channel } => phrases.push(format!("post on {channel}")),
            ScheduleGrant::Egress { destination } => phrases.push(format!("reach {destination}")),
            ScheduleGrant::Vault { action, name } => {
                phrases.push(format!("{action} the secret {name}"));
            }
            ScheduleGrant::Memory { scope, action } => {
                let verb = match action.as_str() {
                    "write" => "remember",
                    "recall" => "recall",
                    "remove" => "forget",
                    "propose" => "ask to remember",
                    other => other,
                };
                if let Some((_, verbs, _)) = memory.iter_mut().find(|(seen, _, _)| seen == scope) {
                    verbs.push(verb);
                } else {
                    memory.push((scope.clone(), vec![verb], phrases.len()));
                    phrases.push(String::new());
                }
            }
            ScheduleGrant::Goals { action } => {
                let keeps = !action.starts_with("query-");
                if let Some((kept, _)) = goals.as_mut() {
                    *kept |= keeps;
                } else {
                    goals = Some((keeps, phrases.len()));
                    phrases.push(String::new());
                }
            }
        }
    }
    for (scope, verbs, at) in memory {
        let about = if scope == "user" {
            String::from("things about you")
        } else {
            format!("{scope} notes")
        };
        if let Some(phrase) = phrases.get_mut(at) {
            *phrase = format!("{} {about}", in_list(&verbs));
        }
    }
    if let Some((keeps, at)) = goals {
        if let Some(phrase) = phrases.get_mut(at) {
            *phrase = String::from(if keeps {
                "keep your goals and their tracking"
            } else {
                "read your goals"
            });
        }
    }
    if phrases.is_empty() {
        return String::from("Nothing beyond answering.");
    }
    let said = phrases.join(", ");
    let mut chars = said.chars();
    chars.next().map_or_else(String::new, |first| {
        format!("{}{}.", first.to_uppercase(), chars.as_str())
    })
}

/// `items` as an English list: a, b and c.
fn in_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchling_core::ui::{Decision, FieldSource, UiEventBody};

    fn config(name: &str, grants: &[&str]) -> (Config, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-schedule-proposals-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = crate::api_auth::tests::owner_config(None);
        config.vault.root = dir.join("vault");
        config.channels = vec![String::from("local")];
        config.egress.grants = grants.iter().map(|grant| (*grant).to_owned()).collect();
        (config, dir)
    }

    #[test]
    fn a_proposal_is_put_to_the_owner_in_their_words_and_its_own() {
        let (config, dir) = config("card", &["localhost:11434"]);
        let bus = std::sync::Arc::new(crate::api_stream::Bus::starting_at(1));
        board(&config).attach(std::sync::Arc::clone(&bus));
        let result = propose(
            &config,
            "task-1",
            "daily",
            "07:30",
            "  Stretch for five minutes.  ",
        );
        let when = if let ScheduleResult::Proposed { when } = result {
            Some(when)
        } else {
            None
        }
        .unwrap();
        assert!(when.starts_with("Every day at 07:30 ("), "{when}");
        let card = bus
            .after(None)
            .into_iter()
            .find_map(|event| {
                if let UiEventBody::Confirmation(card) = event.body {
                    Some(card)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(card.summary, "Schedule this?");
        assert_eq!(card.approve.as_deref(), Some("Schedule it"));
        let fields: Vec<(&str, &str, FieldSource)> = card
            .fields
            .iter()
            .map(|field| (field.label.as_str(), field.value.as_str(), field.source))
            .collect();
        assert_eq!(fields.first().map(|f| f.0), Some("When"));
        assert_eq!(fields.first().map(|f| f.1), Some(when.as_str()));
        assert!(fields.iter().any(|f| f.0 == "What it may do"
            && f.1.contains("post in your chat")
            && f.2 == FieldSource::Broker));
        assert_eq!(
            fields.last().map(|f| (f.1, f.2)),
            Some(("Stretch for five minutes.", FieldSource::Agent))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Noon on 2026-11-11, UTC.
    fn noon() -> Timestamp {
        "2026-11-11T12:00:00Z".parse().unwrap()
    }

    fn unasked_for(result: ScheduleResult) -> String {
        if let ScheduleResult::Unasked { reason } = result {
            reason
        } else {
            format!("it was not refused: {result:?}")
        }
    }

    /// A day named in words is that day on the owner's clock, whatever the
    /// date is elsewhere; anything else is left for the timing to read.
    #[test]
    fn a_day_named_in_words_is_read_on_the_owners_clock() {
        let now = noon().as_second();
        assert_eq!(dated("tomorrow 09:00", now, "UTC"), "2026-11-12 09:00");
        assert_eq!(dated("  Tomorrow at 9:00 ", now, "UTC"), "2026-11-12 09:00");
        assert_eq!(dated("today 17:30", now, "UTC"), "2026-11-11 17:30");
        assert_eq!(dated("TODAY  at  7:05", now, "UTC"), "2026-11-11 07:05");
        let late = "2026-11-11T23:30:00Z".parse::<Timestamp>().unwrap();
        assert_eq!(
            dated("today 08:00", late.as_second(), "Asia/Tokyo"),
            "2026-11-12 08:00",
            "today is the owner's today"
        );
        for untouched in ["2026-11-20 08:00", "tomorrowish 09:00", "next tuesday"] {
            assert_eq!(dated(untouched, now, "UTC"), untouched);
        }
        assert_eq!(
            dated("tomorrow 09:00", now, "Mars/Olympus"),
            "tomorrow 09:00",
            "no clock to read it on"
        );
    }

    /// A time that can never come is never put to the owner. The agent
    /// hears why, with the time it is now, so it can ask again; a time
    /// still to come is asked, by its date.
    #[test]
    fn a_time_that_has_passed_is_never_put_to_the_owner() {
        let (config, dir) = config("passed", &["localhost:11434"]);
        let bus = std::sync::Arc::new(crate::api_stream::Bus::starting_at(1));
        board(&config).attach(std::sync::Arc::clone(&bus));
        let ask = |timing: &str, value: &str| {
            propose_at(
                &config,
                "task-1",
                (timing, value),
                "Water the plants.",
                noon(),
                "UTC",
            )
        };
        assert_eq!(
            unasked_for(ask("at", "2026-11-05 09:00")),
            "2026-11-05 09:00 has already passed: it is now 2026-11-11 12:00 (UTC). Ask again with a time that is still to come"
        );
        for passed in ["today 09:00", "2026-11-11 12:00"] {
            let reason = unasked_for(ask("at", passed));
            assert!(reason.contains("has already passed"), "{passed}: {reason}");
        }
        assert_eq!(
            unasked_for(ask("at", "next tuesday")),
            "at takes \"YYYY-MM-DD HH:MM\", \"today HH:MM\" or \"tomorrow HH:MM\"; it is now 2026-11-11 12:00 (UTC)"
        );
        assert_eq!(
            unasked_for(ask("cron", "0 0 31 2 *")),
            "that timing never comes round"
        );
        assert_eq!(unasked_for(ask("every", "90m")), "every takes 1m to 59m");
        assert!(
            bus.after(None).is_empty(),
            "a time that can never come was put to the owner"
        );

        assert_eq!(
            ask("at", "tomorrow 09:00"),
            ScheduleResult::Proposed {
                when: String::from("Once, 2026-11-12 at 09:00 (UTC)")
            }
        );
        let card = bus
            .after(None)
            .into_iter()
            .find_map(|event| {
                if let UiEventBody::Confirmation(card) = event.body {
                    Some(card)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(
            card.fields.first().map(|field| field.value.as_str()),
            Some("Once, 2026-11-12 at 09:00 (UTC)")
        );
        // It waits for the owner until its time, and no longer.
        assert_eq!(card.expires_at.as_deref(), Some("2026-11-12T09:00:00Z"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A time the clocks skip is said as that, and a timing that can't be
    /// read as that: neither is called passed.
    #[test]
    fn a_time_that_cannot_run_says_which_way() {
        let once = |at: &str| ScheduleSpec::Once {
            at: at.to_owned(),
            tz: String::from("America/New_York"),
        };
        let early = "2026-03-01T12:00:00Z".parse::<Timestamp>().unwrap();
        // The clocks go forward at 02:00 on 2026-03-08 there.
        assert_eq!(
            never_runs(&once("2026-03-08T02:30"), early, "x").as_deref(),
            Some("that time doesn't exist on the owner's clock: the clocks skip it that day")
        );
        assert_eq!(
            never_runs(&once("the 8th at half two"), early, "x").as_deref(),
            Some("that timing can't be read")
        );
        assert_eq!(never_runs(&once("2026-03-08T03:30"), early, "x"), None);
        assert_eq!(
            never_runs(&once("2026-02-28T03:30"), early, "the clock").as_deref(),
            Some(
                "2026-02-28 03:30 has already passed: it is now the clock (America/New_York). Ask again with a time that is still to come"
            )
        );
    }

    /// An approved schedule that isn't created says why, in the chat.
    #[test]
    fn a_schedule_that_was_not_created_says_why() {
        let when = ScheduleSpec::Once {
            at: String::from("2026-11-05T09:00"),
            tz: String::from("UTC"),
        };
        let refused = |reason| why_not(&ScheduleResult::Refused { reason }, &when);
        assert_eq!(
            refused(ScheduleRefusal::NotInFuture),
            "its time had already passed (Once, 2026-11-05 at 09:00 (UTC))"
        );
        assert!(refused(ScheduleRefusal::Full).contains("the schedule list is full"));
        for more in [
            ScheduleRefusal::ExceedsGrants,
            ScheduleRefusal::ChannelNotGranted,
            ScheduleRefusal::TooManyGrants,
        ] {
            assert!(refused(more).contains("more than the chat that proposed it may do"));
        }
        for unread in [
            ScheduleRefusal::MissingTimezone,
            ScheduleRefusal::UnknownTimezone,
            ScheduleRefusal::InvalidSpec,
        ] {
            assert_eq!(refused(unread), "its timing can't be read");
        }
        assert_eq!(
            refused(ScheduleRefusal::BadTask),
            "its task text can't be used"
        );
        assert_eq!(refused(ScheduleRefusal::BadDelay), "the broker refused it");
        assert_eq!(
            why_not(&ScheduleResult::Denied, &when),
            "scheduling is turned off on this broker"
        );
        assert!(why_not(&ScheduleResult::ScheduleError, &when).contains("schedule store"));
        assert_eq!(
            why_not(&ScheduleResult::Cancelled, &when),
            "the broker refused it"
        );

        // As the owner hears it, for a time that passed while it waited.
        let (mut config, dir) = config("why-not", &["localhost:11434"]);
        config.scheduler.root = dir.join("schedules");
        let key = hatchling_vault::MasterKey::generate().unwrap();
        let grants: Vec<ScheduleGrant> = crate::held_grants(&config).into_iter().collect();
        assert_eq!(
            create(&config, &key, &when, "Water the plants.", &grants, noon()),
            Err(String::from(
                "Approved, but it wasn't scheduled: its time had already passed (Once, 2026-11-05 at 09:00 (UTC))."
            ))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn approved_it_is_created_as_the_owners_and_listed() {
        let (mut config, dir) = config("create", &["localhost:11434"]);
        let schedules = dir.join("schedules");
        config.scheduler.root = schedules;
        let key = hatchling_vault::MasterKey::generate().unwrap();
        assert!(matches!(
            propose(&config, "task-1", "every", "15m", "Check the oven."),
            ScheduleResult::Proposed { .. }
        ));
        let board = board(&config);
        let id = board
            .approvals()
            .find_run("task-1", crate::approvals::State::Pending, |run| {
                matches!(run, crate::approval_board::Approved::Schedule(asked) if asked.task == "Check the oven.")
            })
            .unwrap();
        let request = board.approvals().get(&id).unwrap();
        board
            .decide(&id, request.revision, &request.digest, Decision::Approve, 1)
            .unwrap();
        assert_eq!(
            board.next_approved(std::time::Duration::ZERO),
            Some(id.clone())
        );
        let said = crate::approval_board::run_approved(
            &board,
            &id,
            &config,
            &key,
            &crate::Skills::default(),
            None,
            Timestamp::now().as_second(),
        );
        assert!(
            said.starts_with("Scheduled: “Check the oven.”, Every 15 minutes ("),
            "{said}"
        );
        let listed = crate::schedule_ops::handle_schedule_op(
            &config,
            &key,
            &ScheduleOp::List { after: None },
            Timestamp::now(),
            Creator::Owner,
        );
        let ScheduleResult::Listed { schedules, .. } = listed else {
            unreachable_listing(&listed);
            return;
        };
        let tasks: Vec<Option<&str>> = schedules.iter().map(|one| one.task.as_deref()).collect();
        assert_eq!(tasks, [Some("Check the oven.")], "shown as the owner's");
        // A store that refuses: said, and nothing claimed as done.
        let mut broken = config;
        broken.scheduler.root = std::path::PathBuf::new();
        let refused = create(
            &broken,
            &key,
            &ScheduleSpec::Cron {
                expr: String::from("*/15 * * * *"),
                tz: String::from("UTC"),
            },
            "x",
            &[],
            Timestamp::now(),
        );
        assert!(refused.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn unreachable_listing(result: &ScheduleResult) {
        assert!(
            matches!(result, ScheduleResult::Listed { .. }),
            "{result:?}"
        );
    }

    #[test]
    fn what_cant_be_scheduled_is_said_and_never_asked() {
        let (config, dir) = config("refused", &[]);
        for (timing, value, why) in [
            ("daily", "7:30", "takes a time, HH:MM"),
            ("every", "90m", "takes 1m to 59m"),
            ("at", "tomorrow", "takes \"YYYY-MM-DD HH:MM\""),
            ("cron", "* *", "five fields"),
            ("hourly", "1", "isn't a timing"),
        ] {
            let result = propose(&config, "t", timing, value, "x");
            assert!(
                matches!(&result, ScheduleResult::Unasked { reason } if reason.contains(why) && !reason.contains("--")),
                "{timing} {value}: {result:?}"
            );
        }
        for task in ["", "   ", "a\u{7}b", &"x".repeat(MAX_TASK_BYTES + 1)] {
            assert!(matches!(
                propose(&config, "t", "daily", "07:30", task),
                ScheduleResult::Unasked { .. }
            ));
        }
        let mut quiet = config;
        quiet.channels = Vec::new();
        assert_eq!(
            propose(&quiet, "t", "daily", "07:30", "x"),
            ScheduleResult::Denied
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_the_owner_refuses_is_never_asked_again() {
        let (config, dir) = config("deny", &[]);
        assert!(matches!(
            propose(&config, "task-1", "every", "15m", "Check the oven."),
            ScheduleResult::Proposed { .. }
        ));
        let board = board(&config);
        let id = board
            .approvals()
            .find_run("task-1", crate::approvals::State::Pending, |run| {
                matches!(run, crate::approval_board::Approved::Schedule(_))
            })
            .unwrap();
        let request = board.approvals().get(&id).unwrap();
        board
            .decide(&id, request.revision, &request.digest, Decision::Deny, 1)
            .unwrap();
        assert!(matches!(
            propose(&config, "task-2", "every", "15m", "Check the oven."),
            ScheduleResult::Unasked { .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grants_read_as_a_sentence() {
        assert_eq!(grants_in_words(&[]), "Nothing beyond answering.");
        let grants = [
            ScheduleGrant::ChannelSend {
                channel: String::from("local"),
            },
            ScheduleGrant::ChannelSend {
                channel: String::from("signal"),
            },
            ScheduleGrant::Egress {
                destination: String::from("localhost:11434"),
            },
            ScheduleGrant::Vault {
                action: String::from("fetch"),
                name: String::from("api-key"),
            },
            ScheduleGrant::Memory {
                scope: String::from("user"),
                action: String::from("recall"),
            },
            ScheduleGrant::Goals {
                action: String::from("query-goals"),
            },
        ];
        assert_eq!(
            grants_in_words(&grants),
            "Post in your chat, post on signal, reach localhost:11434, fetch the secret api-key, recall things about you, read your goals."
        );
        // A store's grants as one phrase, where the first stood: the
        // install's own, in full.
        let memory = |scope: &str, action: &str| ScheduleGrant::Memory {
            scope: scope.to_owned(),
            action: action.to_owned(),
        };
        let goals = |action: &str| ScheduleGrant::Goals {
            action: action.to_owned(),
        };
        let install = [
            ScheduleGrant::Egress {
                destination: String::from("localhost:11434"),
            },
            memory("project", "recall"),
            memory("user", "propose"),
            memory("user", "recall"),
            goals("query-goals"),
            goals("goal-create"),
            memory("user", "write"),
            memory("user", "remove"),
            memory("user", "context-read"),
        ];
        assert_eq!(
            grants_in_words(&install),
            "Reach localhost:11434, recall project notes, ask to remember, recall, remember, forget and context-read things about you, keep your goals and their tracking."
        );
        assert_eq!(in_list(&[]), "");
    }
}
