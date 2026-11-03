//! `hatchling-broker schedule`: the owner's scheduled work from a shell.
//!
//! A client of the local API, as the app is. Run as the owner, without
//! `sudo`: the API socket admits the owner by the kernel's credentials,
//! and the broker decides everything. A schedule added here gets the
//! connection's grants, the ones a chat message's run has, and delivers to
//! the local chat, exactly as one added in the app.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use hatchling_core::cli::EX_USAGE;
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::ui::{UI_SCHEMA_VERSION, UiAction, UiActionResult, UiRequest, UiResponse};
use hatchling_core::{ScheduleGrant, ScheduleOp, ScheduleRefusal, ScheduleResult, ScheduleSpec};

/// Where `scripts/install.sh` has the broker put its API socket.
pub const INSTALLED_API_SOCKET: &str = "/var/lib/hatchling/api.sock";

/// How long the broker has to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The usage, for a command that doesn't parse.
pub const USAGE: &str = "\
Usage:
  hatchling-broker schedule list
  hatchling-broker schedule add <task> <when> [--tz <zone>]
  hatchling-broker schedule remove <id>

<when> is one of:
  --at \"YYYY-MM-DD HH:MM\"   once
  --every <minutes>m        every 1 to 59 minutes
  --daily HH:MM             every day
  --weekdays HH:MM          Monday to Friday
  --cron \"<expr>\"           minute hour day-of-month month day-of-week

Run it as yourself, not with sudo. Times are in this machine's time zone
unless --tz names another. The work runs as a chat message's would, and its
results arrive in the chat.";

/// One parsed `schedule` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// List the schedules.
    List,
    /// Add `task` at `when`.
    Add {
        /// What the run is asked to do.
        task: String,
        /// When.
        when: ScheduleSpec,
    },
    /// Remove the schedule `id`.
    Remove {
        /// Its id.
        id: String,
    },
}

/// Parses the arguments after `schedule`, with `zone` the time zone to use
/// when `--tz` names none.
///
/// # Errors
///
/// What's wrong, in a sentence.
pub fn parse(args: &[String], zone: Option<String>) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["list"] => Ok(Command::List),
        ["remove", id] => Ok(Command::Remove {
            id: (*id).to_owned(),
        }),
        ["add", task, flag, value, rest @ ..] => {
            let tz = match rest {
                [] => zone.ok_or_else(|| {
                    String::from(
                        "this machine names no time zone: add --tz, such as --tz Europe/Berlin",
                    )
                })?,
                ["--tz", tz] => (*tz).to_owned(),
                _ => return Err(String::from("after the timing, only --tz <zone>")),
            };
            Ok(Command::Add {
                task: (*task).to_owned(),
                when: when(flag, value, tz)?,
            })
        }
        _ => Err(String::from("not a schedule command")),
    }
}

/// `HH:MM` as hour and minute.
fn clock(value: &str) -> Option<(u8, u8)> {
    let (hour, minute) = value.split_once(':')?;
    let digits = |text: &str| text.len() == 2 && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(hour) || !digits(minute) {
        return None;
    }
    let (hour, minute) = (hour.parse().ok()?, minute.parse().ok()?);
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// The timing `flag value` names, in `tz`.
fn when(flag: &str, value: &str, tz: String) -> Result<ScheduleSpec, String> {
    let cron = |expr: String| ScheduleSpec::Cron {
        expr,
        tz: tz.clone(),
    };
    match flag {
        "--at" => {
            let (date, time) = value
                .split_once(' ')
                .filter(|(date, time)| date.len() == 10 && clock(time).is_some())
                .ok_or_else(|| String::from("--at takes \"YYYY-MM-DD HH:MM\""))?;
            Ok(ScheduleSpec::Once {
                at: format!("{date}T{time}"),
                tz,
            })
        }
        "--every" => {
            let minutes: u8 = value
                .strip_suffix('m')
                .and_then(|count| count.parse().ok())
                .filter(|count| (1..60).contains(count))
                .ok_or_else(|| String::from("--every takes 1m to 59m"))?;
            Ok(cron(if minutes == 1 {
                String::from("* * * * *")
            } else {
                format!("*/{minutes} * * * *")
            }))
        }
        "--daily" | "--weekdays" => {
            let (hour, minute) =
                clock(value).ok_or_else(|| format!("{flag} takes a time, HH:MM"))?;
            let days = if flag == "--daily" { "*" } else { "1-5" };
            Ok(cron(format!("{minute} {hour} * * {days}")))
        }
        "--cron" => {
            if value.split_whitespace().count() != 5 {
                return Err(String::from(
                    "--cron takes five fields: minute hour day-of-month month day-of-week",
                ));
            }
            Ok(cron(value.split_whitespace().collect::<Vec<_>>().join(" ")))
        }
        _ => Err(format!(
            "{flag} isn't a timing: use --at, --every, --daily, --weekdays or --cron"
        )),
    }
}

/// Says hello on `stream`, then asks the broker `op`: the welcome's grants
/// and the answer. `op` is given the grants when it's a create.
///
/// # Errors
///
/// What went wrong, in a sentence.
pub fn ask(
    stream: &UnixStream,
    op: impl FnOnce(&[ScheduleGrant]) -> ScheduleOp,
) -> Result<ScheduleResult, String> {
    let send = |request: &UiRequest| {
        let bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        write_frame(stream, &bytes).map_err(|error| format!("couldn't reach the broker: {error}"))
    };
    let receive = || -> Result<UiResponse, String> {
        let bytes = read_frame(stream).map_err(|_| {
            String::from(
                "the broker closed the connection: run this as Hatchling's owner, without sudo",
            )
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|_| String::from("the broker answered with something this command can't read"))
    };
    send(&UiRequest::Hello {
        schema_version: UI_SCHEMA_VERSION,
        client: String::from("hatchling-broker schedule"),
        token: None,
    })?;
    let grants = match receive()? {
        UiResponse::Welcome { grants, .. } => grants,
        UiResponse::Incompatible { message, .. } => return Err(message),
        UiResponse::Event(_) | UiResponse::ActionResult { .. } => {
            return Err(String::from("the broker didn't say hello"));
        }
    };
    let request_id = String::from("schedule-1");
    send(&UiRequest::Action {
        request_id: request_id.clone(),
        action: UiAction::ManageSchedule { op: op(&grants) },
    })?;
    loop {
        match receive()? {
            UiResponse::ActionResult {
                request_id: answered,
                result,
            } if answered == request_id => {
                return if let UiActionResult::Schedule { outcome } = result {
                    Ok(outcome)
                } else if let UiActionResult::Refused { .. } = result {
                    Err(String::from("this connection may not change schedules"))
                } else {
                    Err(String::from("the broker answered with something else"))
                };
            }
            // Nothing is subscribed to, but whatever else arrives isn't
            // the answer.
            UiResponse::Welcome { .. }
            | UiResponse::Incompatible { .. }
            | UiResponse::Event(_)
            | UiResponse::ActionResult { .. } => {}
        }
    }
}

/// Why the broker refused, in words.
const fn refusal(reason: ScheduleRefusal) -> &'static str {
    match reason {
        ScheduleRefusal::ExceedsGrants | ScheduleRefusal::TooManyGrants => {
            "this connection may not give a schedule those permissions"
        }
        ScheduleRefusal::MissingTimezone => "the schedule needs a time zone: add --tz",
        ScheduleRefusal::UnknownTimezone => "the broker doesn't know that time zone",
        ScheduleRefusal::InvalidSpec => "that timing never happens",
        ScheduleRefusal::NotInFuture => {
            "that time has passed, or doesn't exist there (a daylight-saving change)"
        }
        ScheduleRefusal::ChannelNotGranted => "this connection may not post in the chat",
        ScheduleRefusal::BadTask => "say what should happen, in fewer words",
        ScheduleRefusal::Full => "Hatchling keeps as many schedules as it can: remove one first",
        ScheduleRefusal::UnknownSource
        | ScheduleRefusal::MaxFollowupDepth
        | ScheduleRefusal::FollowupTaken
        | ScheduleRefusal::BadDelay => "the broker refused it",
    }
}

/// How much of a schedule's id a listing shows, and enough to name it.
const SHORT_ID: usize = 12;

/// `id` as a listing shows it.
fn short(id: &str) -> String {
    id.chars().take(SHORT_ID).collect()
}

/// The day names a cron day of the week stands for, Sunday first.
const DAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `when` in words: the timings this command and the app make read back,
/// any other cron as itself.
fn describe(when: &ScheduleSpec) -> String {
    let (expr, tz) = match when {
        ScheduleSpec::Once { at, tz } => {
            return format!("Once, {} ({tz})", at.replace('T', " at "));
        }
        ScheduleSpec::Cron { expr, tz } => (expr, tz),
    };
    let fields: Vec<&str> = expr.split(' ').collect();
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let words = match fields.as_slice() {
        ["*", "*", "*", "*", "*"] => Some(String::from("Every minute")),
        [minute, "*", "*", "*", "*"] => minute
            .strip_prefix("*/")
            .filter(|step| digits(step))
            .map(|step| format!("Every {step} minutes")),
        [minute, hour, "*", "*", day] if digits(minute) && digits(hour) => {
            let time = format!("{hour:0>2}:{minute:0>2}");
            match *day {
                "*" => Some(format!("Every day at {time}")),
                "1-5" => Some(format!("Weekdays at {time}")),
                day => day
                    .parse::<usize>()
                    .ok()
                    .and_then(|day| DAY_NAMES.get(day))
                    .map(|name| format!("Every {name} at {time}")),
            }
        }
        _ => None,
    };
    format!("{} ({tz})", words.unwrap_or_else(|| format!("cron {expr}")))
}

/// The one schedule in `schedules` whose id starts with `prefix`.
///
/// # Errors
///
/// None does, or more than one: in words.
fn resolve(prefix: &str, schedules: &[hatchling_core::ScheduleSummary]) -> Result<String, String> {
    let mut matches = schedules
        .iter()
        .filter(|schedule| schedule.id.starts_with(prefix));
    match (matches.next(), matches.next()) {
        (Some(only), None) => Ok(only.id.clone()),
        (None, _) => Err(format!("there's no schedule {prefix}")),
        (Some(_), Some(_)) => Err(format!(
            "{prefix} is the start of more than one schedule's id: give more of it"
        )),
    }
}

/// What `outcome` means for `command`, as lines for the person, or why it
/// failed.
///
/// # Errors
///
/// The broker's refusal, or an answer that isn't the command's, in words.
pub fn report(command: &Command, outcome: ScheduleResult) -> Result<Vec<String>, String> {
    match (command, outcome) {
        (Command::Add { .. }, ScheduleResult::Created { id }) => Ok(vec![format!(
            "Scheduled: {id}. Its results arrive in the chat."
        )]),
        (Command::Remove { id }, ScheduleResult::Cancelled) => {
            Ok(vec![format!("Removed {}: it won't run again.", short(id))])
        }
        (Command::Remove { id }, ScheduleResult::NotFound) => {
            Err(format!("there's no schedule {}", short(id)))
        }
        (Command::List, ScheduleResult::Listed { schedules, .. }) if schedules.is_empty() => {
            Ok(vec![String::from("No scheduled work.")])
        }
        (Command::List, ScheduleResult::Listed { schedules, .. }) => Ok(schedules
            .iter()
            .map(|schedule| {
                let tz = match &schedule.when {
                    ScheduleSpec::Cron { tz, .. } | ScheduleSpec::Once { tz, .. } => tz,
                };
                let next = if schedule.held.is_some() {
                    String::from("held: a run was cut short when Hatchling stopped")
                } else {
                    hatchling_scheduler::local_time(schedule.next_due, tz)
                        .map_or_else(|| String::from("not scheduled"), |at| format!("next {at}"))
                };
                let task = schedule
                    .task
                    .as_deref()
                    .unwrap_or("(its task isn't shown: you didn't write it here)");
                format!(
                    "{}  {}  {next}  {task}",
                    short(&schedule.id),
                    describe(&schedule.when)
                )
            })
            .collect()),
        (_, ScheduleResult::Refused { reason }) => Err(String::from(refusal(reason))),
        (_, ScheduleResult::Denied) => Err(String::from(
            "Hatchling has no scheduler: the operator has not turned scheduled work on",
        )),
        (_, ScheduleResult::ScheduleError) => Err(String::from(
            "the broker couldn't change its schedules: `sudo hatchling-broker doctor` checks the install",
        )),
        (_, _) => Err(String::from("the broker answered with something else")),
    }
}

/// The API socket: `HATCHLING_API_SOCKET`, or the installed broker's.
fn socket(get: impl Fn(&str) -> Option<String>) -> PathBuf {
    get("HATCHLING_API_SOCKET").map_or_else(|| PathBuf::from(INSTALLED_API_SOCKET), PathBuf::from)
}

/// Runs `schedule` with `args`, the arguments after it.
#[must_use]
pub fn schedule_main(program: &str, args: &[String]) -> ExitCode {
    let command = match parse(args, hatchling_scheduler::system_zone()) {
        Ok(command) => command,
        Err(why) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{program} schedule: {why}\n\n{USAGE}"
            );
            return ExitCode::from(EX_USAGE);
        }
    };
    match run(&command, &socket(|key| std::env::var(key).ok())) {
        Ok(lines) => {
            let mut out = std::io::stdout().lock();
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(std::io::stderr().lock(), "{program} schedule: {why}");
            ExitCode::FAILURE
        }
    }
}

/// Asks the broker at the API socket `path` for `op`, made from the
/// connection's grants.
fn ask_at(
    path: &Path,
    op: impl FnOnce(&[ScheduleGrant]) -> ScheduleOp,
) -> Result<ScheduleResult, String> {
    let stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "can't reach Hatchling's API at {} ({error}): is it running, with HATCHLING_API_SOCKET set?",
            path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(ANSWER_TIMEOUT))
        .map_err(|error| error.to_string())?;
    ask(&stream, op)
}

/// Every schedule the broker at `path` lists, every page of it; or its
/// answer when that isn't a listing.
fn list_all(path: &Path) -> Result<ScheduleResult, String> {
    let mut all = Vec::new();
    let mut after = None;
    loop {
        let page = ask_at(path, |_| ScheduleOp::List {
            after: after.clone(),
        })?;
        let ScheduleResult::Listed { schedules, next } = page else {
            return Ok(page);
        };
        all.extend(schedules);
        if next.is_none() {
            return Ok(ScheduleResult::Listed {
                schedules: all,
                next: None,
            });
        }
        after = next;
    }
}

/// Connects to the API socket at `path` and carries out `command`.
fn run(command: &Command, path: &Path) -> Result<Vec<String>, String> {
    match command {
        Command::List => report(command, list_all(path)?),
        Command::Add { task, when } => report(
            command,
            ask_at(path, |grants| ScheduleOp::Create {
                when: when.clone(),
                task: task.clone(),
                grants: grants.to_vec(),
                channel: String::from("local"),
            })?,
        ),
        Command::Remove { id } => {
            // The listing's short id, or any start of one, names it.
            let listed = list_all(path)?;
            let ScheduleResult::Listed { schedules, .. } = listed else {
                return report(command, listed);
            };
            let id = resolve(id, &schedules)?;
            report(
                &Command::Remove { id: id.clone() },
                ask_at(path, |_| ScheduleOp::Cancel { id })?,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    const ZONE: &str = "America/New_York";

    fn zone() -> String {
        String::from(ZONE)
    }

    fn cron(expr: &str) -> ScheduleSpec {
        ScheduleSpec::Cron {
            expr: expr.to_owned(),
            tz: String::from(ZONE),
        }
    }

    fn added(words: &[&str]) -> Result<ScheduleSpec, String> {
        match parse(&args(words), Some(zone()))? {
            Command::Add { when, .. } => Ok(when),
            other @ (Command::List | Command::Remove { .. }) => Err(format!("{other:?}")),
        }
    }

    #[test]
    fn each_timing_is_the_brokers_spec_in_the_machines_zone() {
        assert_eq!(
            added(&["add", "t", "--every", "5m"]),
            Ok(cron("*/5 * * * *"))
        );
        assert_eq!(added(&["add", "t", "--every", "1m"]), Ok(cron("* * * * *")));
        assert_eq!(
            added(&["add", "t", "--daily", "07:30"]),
            Ok(cron("30 7 * * *"))
        );
        assert_eq!(
            added(&["add", "t", "--weekdays", "08:00"]),
            Ok(cron("0 8 * * 1-5"))
        );
        assert_eq!(
            added(&["add", "t", "--cron", " 0  8 * * 1-5 "]),
            Ok(cron("0 8 * * 1-5"))
        );
        assert_eq!(
            added(&["add", "t", "--at", "2026-11-04 08:00"]),
            Ok(ScheduleSpec::Once {
                at: String::from("2026-11-04T08:00"),
                tz: String::from(ZONE),
            })
        );
        assert_eq!(
            added(&["add", "t", "--daily", "07:30", "--tz", "Asia/Kolkata"]),
            Ok(ScheduleSpec::Cron {
                expr: String::from("30 7 * * *"),
                tz: String::from("Asia/Kolkata"),
            })
        );
        assert_eq!(
            parse(&args(&["add", "Stretch", "--daily", "08:00"]), Some(zone())),
            Ok(Command::Add {
                task: String::from("Stretch"),
                when: cron("0 8 * * *"),
            })
        );
        assert_eq!(parse(&args(&["list"]), None), Ok(Command::List));
        assert_eq!(
            parse(&args(&["remove", "sch-1"]), None),
            Ok(Command::Remove {
                id: String::from("sch-1")
            })
        );
    }

    #[test]
    fn a_timing_that_isnt_one_is_refused_before_the_broker_is_asked() {
        for words in [
            &["add", "t", "--every", "0m"][..],
            &["add", "t", "--every", "60m"],
            &["add", "t", "--every", "5"],
            &["add", "t", "--every", "xm"],
            &["add", "t", "--daily", "8:00"],
            &["add", "t", "--daily", "24:00"],
            &["add", "t", "--daily", "08:60"],
            &["add", "t", "--weekdays", "0800"],
            &["add", "t", "--daily", "aa:bb"],
            &["add", "t", "--at", "2026-11-04"],
            &["add", "t", "--at", "2026-11-4 08:00"],
            &["add", "t", "--at", "2026-11-04 8:00"],
            &["add", "t", "--cron", "0 8 * *"],
            &["add", "t", "--hourly", "08:00"],
            &["add", "t", "--daily", "08:00", "--zone", "UTC"],
            &["add", "t", "--daily", "08:00", "--tz"],
            &["add", "t", "--daily"],
            &["add", "t"],
            &["list", "all"],
            &["remove"],
            &[],
        ] {
            assert!(parse(&args(words), Some(zone())).is_err(), "{words:?}");
        }
        // No zone here, and none named: say how to name one.
        let why = parse(&args(&["add", "t", "--daily", "08:00"]), None).unwrap_err();
        assert!(why.contains("--tz"), "{why}");
    }

    fn summary(task: Option<&str>, held: bool) -> hatchling_core::ScheduleSummary {
        hatchling_core::ScheduleSummary {
            id: String::from("sch-1"),
            kind: hatchling_core::ScheduleKind::Task,
            when: cron("0 8 * * 1-5"),
            channel: String::from("local"),
            grants: Vec::new(),
            next_due: 1_790_000_000,
            held: held.then_some(hatchling_core::HeldFire {
                due: 1,
                started: true,
            }),
            task: task.map(str::to_owned),
        }
    }

    #[test]
    fn each_answer_is_a_line_or_a_reason() {
        let add = Command::Add {
            task: String::from("t"),
            when: cron("0 8 * * *"),
        };
        let remove = Command::Remove {
            id: String::from("sch-1"),
        };
        assert_eq!(
            report(
                &add,
                ScheduleResult::Created {
                    id: String::from("sch-9")
                }
            ),
            Ok(vec![String::from(
                "Scheduled: sch-9. Its results arrive in the chat."
            )])
        );
        assert_eq!(
            report(&remove, ScheduleResult::Cancelled),
            Ok(vec![String::from("Removed sch-1: it won't run again.")])
        );
        assert!(report(&remove, ScheduleResult::NotFound).is_err());
        let listed = |schedules| ScheduleResult::Listed {
            schedules,
            next: None,
        };
        assert_eq!(
            report(&Command::List, listed(Vec::new())),
            Ok(vec![String::from("No scheduled work.")])
        );
        // 1_790_000_000 is 2026-09-21 14:13 UTC: 10:13 in New York.
        assert_eq!(
            report(
                &Command::List,
                listed(vec![summary(Some("Stretch"), false)])
            ),
            Ok(vec![String::from(
                "sch-1  Weekdays at 08:00 (America/New_York)  next 2026-09-21 10:13  Stretch"
            )])
        );
        let hidden = report(&Command::List, listed(vec![summary(None, true)])).unwrap();
        assert!(
            hidden
                .first()
                .is_some_and(|line| line.contains("held:") && line.contains("isn't shown")),
            "{hidden:?}"
        );
        for reason in [
            ScheduleRefusal::ExceedsGrants,
            ScheduleRefusal::MissingTimezone,
            ScheduleRefusal::UnknownTimezone,
            ScheduleRefusal::InvalidSpec,
            ScheduleRefusal::NotInFuture,
            ScheduleRefusal::ChannelNotGranted,
            ScheduleRefusal::BadTask,
            ScheduleRefusal::TooManyGrants,
            ScheduleRefusal::Full,
            ScheduleRefusal::UnknownSource,
        ] {
            assert!(report(&add, ScheduleResult::Refused { reason }).is_err());
        }
        assert!(
            report(&add, ScheduleResult::Denied)
                .unwrap_err()
                .contains("no scheduler")
        );
        assert!(
            report(&add, ScheduleResult::ScheduleError)
                .unwrap_err()
                .contains("doctor")
        );
        assert!(
            report(&add, ScheduleResult::Cancelled).is_err(),
            "not the command's answer"
        );
        assert_eq!(
            describe(&ScheduleSpec::Once {
                at: String::from("2026-11-04T08:00"),
                tz: String::from(ZONE),
            }),
            "Once, 2026-11-04 at 08:00 (America/New_York)"
        );
        for (expr, words) in [
            ("* * * * *", "Every minute"),
            ("*/15 * * * *", "Every 15 minutes"),
            ("0 8 * * *", "Every day at 08:00"),
            ("30 7 * * 1-5", "Weekdays at 07:30"),
            ("5 23 * * 0", "Every Sunday at 23:05"),
            ("0 0 * * 6", "Every Saturday at 00:00"),
            ("0 8 * * 7", "cron 0 8 * * 7"),
            ("0 8 * * 1,3", "cron 0 8 * * 1,3"),
            ("*/x * * * *", "cron */x * * * *"),
            ("5 * * * *", "cron 5 * * * *"),
            ("0 8 1 * *", "cron 0 8 1 * *"),
            ("a 8 * * *", "cron a 8 * * *"),
            ("0 8 * *", "cron 0 8 * *"),
        ] {
            assert_eq!(describe(&cron(expr)), format!("{words} ({ZONE})"), "{expr}");
        }
    }

    #[test]
    fn a_schedule_is_named_by_any_start_of_its_id() {
        let listed = |ids: &[&str]| -> Vec<hatchling_core::ScheduleSummary> {
            ids.iter()
                .map(|id| hatchling_core::ScheduleSummary {
                    id: (*id).to_owned(),
                    ..summary(None, false)
                })
                .collect()
        };
        let both = listed(&["sch-82d3c6215ca6", "sch-d92259e73524"]);
        assert_eq!(
            resolve("sch-82d3", &both),
            Ok(String::from("sch-82d3c6215ca6"))
        );
        assert_eq!(
            resolve("sch-d92259e73524", &both),
            Ok(String::from("sch-d92259e73524"))
        );
        assert!(
            resolve("sch-", &both)
                .unwrap_err()
                .contains("more than one")
        );
        assert!(resolve("sch-0", &both).unwrap_err().contains("no schedule"));
        assert!(resolve("sch-1", &[]).is_err());
        assert_eq!(short("sch-82d3c6215ca6e9d23352"), "sch-82d3c621");
        assert_eq!(short("sch-1"), "sch-1");
    }

    #[test]
    fn the_socket_is_the_settings_or_the_installed_brokers() {
        assert_eq!(socket(|_| None), PathBuf::from(INSTALLED_API_SOCKET));
        assert_eq!(
            socket(|key| (key == "HATCHLING_API_SOCKET").then(|| String::from("/run/x.sock"))),
            PathBuf::from("/run/x.sock")
        );
        let unreachable = run(&Command::List, Path::new("/nonexistent/hatchling/api.sock"));
        assert!(unreachable.unwrap_err().contains("can't reach"));
    }
}
