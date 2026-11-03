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

use hatchling_core::{ScheduleGrant, ScheduleOp, ScheduleResult, ScheduleSpec};
use hatchling_scheduler::{Creator, MAX_TASK_BYTES, Timestamp};
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
    let zone = hatchling_scheduler::system_zone().unwrap_or_else(|| String::from("UTC"));
    let when = match crate::schedule_cli::when(&format!("--{timing}"), value, zone) {
        Ok(when) => when,
        Err(reason) => return unasked(&reason.replace("--", "")),
    };
    let words = crate::schedule_cli::describe(&when);
    let now = Timestamp::now().as_second();
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
    Err(String::from(
        "Approved, but it wasn't scheduled: the broker refused it. Its log says why.",
    ))
}

/// What `grants` let a scheduled run do, in words, for the owner's card.
#[must_use]
pub fn grants_in_words(grants: &[ScheduleGrant]) -> String {
    let words: Vec<String> = grants
        .iter()
        .map(|grant| match grant {
            ScheduleGrant::ChannelSend { channel } if channel == CHANNEL => {
                String::from("post in your chat")
            }
            ScheduleGrant::ChannelSend { channel } => format!("post on {channel}"),
            ScheduleGrant::Egress { destination } => format!("reach {destination}"),
            ScheduleGrant::Vault { action, name } => format!("{action} the secret {name}"),
            ScheduleGrant::Memory { scope, action } => format!("{action} {scope} memory"),
            ScheduleGrant::Goals { action } => action.replace('-', " "),
        })
        .collect();
    if words.is_empty() {
        String::from("Nothing beyond answering.")
    } else {
        let mut said = words.join(", ");
        if let Some(first) = said.get(..1) {
            said = first.to_uppercase() + said.get(1..).unwrap_or_default();
        }
        format!("{said}.")
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
            && f.1.contains("reach localhost:11434")
            && f.2 == FieldSource::Broker));
        assert_eq!(
            fields.last().map(|f| (f.1, f.2)),
            Some(("Stretch for five minutes.", FieldSource::Agent))
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
        let mut broken = config.clone();
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
        let mut quiet = config.clone();
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
            "Post in your chat, post on signal, reach localhost:11434, fetch the secret api-key, recall user memory, query goals."
        );
    }
}
