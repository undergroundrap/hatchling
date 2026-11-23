//! `hatchling-broker skills`: the owner's skills, from a terminal (#617).
//!
//! For a headless install, or when the app isn't open. A client of the
//! local API, as `schedule` and `sign-in` are: run as the owner, without
//! `sudo`. It asks the broker's `manage_skills`, the action Settings ›
//! Skills uses, so the same checks hold: the enabling bound to the digest
//! shown, the store's bound, the rebuild lock, and the owner's connection
//! alone. It lists, enables and disables.

use std::io::{BufRead, IsTerminal as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use hatchling_core::cli::EX_USAGE;
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::ui::{
    SkillListing, SkillState, SkillsOp, SkillsResult, UI_SCHEMA_VERSION, UiAction, UiActionResult,
    UiRequest, UiResponse,
};

use crate::schedule_cli::socket;

/// How long the broker has to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The usage, for a command that doesn't parse.
pub const USAGE: &str = "\
Usage:
  hatchling-broker skills list
  hatchling-broker skills enable <name>
  hatchling-broker skills disable <name>

Run it as yourself, not with sudo. `list` shows each skill Hatchling found:
what it's for, who signed it, its digest, what enabling it lets it do, and
every file it holds. `enable` shows the skill the same way and asks you to
type yes; it's enabled as shown, and a skill that changes stops until you
enable it again. `disable` stops it at once.";

/// One parsed `skills` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// List the skills.
    List,
    /// Enable the skill `name`, once the owner says yes.
    Enable {
        /// Its name, as listed.
        name: String,
    },
    /// Disable the skill `name`.
    Disable {
        /// Its name, as listed.
        name: String,
    },
}

/// Parses the arguments after `skills`.
///
/// # Errors
///
/// When they aren't a command.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["list"] => Ok(Command::List),
        ["enable", name] => Ok(Command::Enable {
            name: (*name).to_owned(),
        }),
        ["disable", name] => Ok(Command::Disable {
            name: (*name).to_owned(),
        }),
        _ => Err(String::from("that isn't a skills command")),
    }
}

/// Says hello on `stream`, then asks the broker `op`, and its answer.
///
/// # Errors
///
/// What went wrong, in a sentence.
pub fn ask(stream: &UnixStream, op: SkillsOp) -> Result<SkillsResult, String> {
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
        client: String::from("hatchling-broker skills"),
        token: None,
    })?;
    match receive()? {
        UiResponse::Welcome { .. } => {}
        UiResponse::Incompatible { message, .. } => return Err(message),
        UiResponse::Event(_) | UiResponse::ActionResult { .. } => {
            return Err(String::from("the broker didn't say hello"));
        }
    }
    let request_id = String::from("skills-1");
    send(&UiRequest::Action {
        request_id: request_id.clone(),
        action: UiAction::ManageSkills { op },
    })?;
    loop {
        match receive()? {
            UiResponse::ActionResult {
                request_id: answered,
                result,
            } if answered == request_id => return outcome_of(result),
            // Nothing is subscribed to, but whatever else arrives isn't
            // the answer.
            UiResponse::Welcome { .. }
            | UiResponse::Incompatible { .. }
            | UiResponse::Event(_)
            | UiResponse::ActionResult { .. } => {}
        }
    }
}

/// The skills outcome in `result`, or why there's none.
fn outcome_of(result: UiActionResult) -> Result<SkillsResult, String> {
    if let UiActionResult::Skills { outcome } = result {
        Ok(outcome)
    } else if let UiActionResult::Refused { .. } = result {
        Err(String::from(
            "this connection may not change the skills: run this as Hatchling's owner",
        ))
    } else {
        Err(String::from("the broker answered with something else"))
    }
}

/// Asks the broker at the API socket `path` for `op`, on a connection of
/// its own.
fn ask_at(path: &Path, op: SkillsOp) -> Result<SkillsResult, String> {
    let stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "can't reach Hatchling's API at {} ({error}): is it running, with HATCHLING_API_SOCKET set if it's elsewhere?",
            path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(ANSWER_TIMEOUT))
        .map_err(|error| error.to_string())?;
    ask(&stream, op)
}

/// Where a skill stands, in the owner's words.
const fn state_words(state: SkillState) -> &'static str {
    match state {
        SkillState::Enabled => "enabled",
        SkillState::Waiting => "waiting for you",
        SkillState::Changed => "changed since you enabled it, so it doesn't run",
    }
}

/// A digest or fingerprint, short enough to compare by eye.
fn short(hex: &str) -> String {
    match (
        hex.get(..8),
        hex.len().checked_sub(8).and_then(|at| hex.get(at..)),
    ) {
        (Some(head), Some(tail)) if hex.len() > 16 => format!("{head}…{tail}"),
        _ => hex.to_owned(),
    }
}

/// What enabling `skill` lets it do.
fn grants(skill: &SkillListing) -> String {
    if !skill.capabilities.is_empty() {
        return skill.capabilities.join(", ");
    }
    if skill.program.is_some() {
        String::from("nothing more: its program declares no capabilities")
    } else {
        String::from("nothing more: it's a playbook, with no program to run")
    }
}

/// `skill` as the owner reads it before enabling it: `whole` shows its
/// signer and digest in full, as an enable binds to them.
#[must_use]
pub fn describe(skill: &SkillListing, whole: bool) -> Vec<String> {
    let hash = |hex: &str| if whole { hex.to_owned() } else { short(hex) };
    let mut lines = vec![
        format!(
            "{} {}: {}",
            skill.name,
            skill.version,
            state_words(skill.state)
        ),
        format!("  {}", skill.description),
        format!("  signed by {}", hash(&skill.signer)),
        format!("  digest    {}", hash(&skill.digest)),
        format!("  enabling it lets it: {}", grants(skill)),
    ];
    for file in &skill.files {
        let role = if skill.instructions.as_deref() == Some(file.path.as_str()) {
            ", its playbook"
        } else if skill.program.as_deref() == Some(file.path.as_str()) {
            ", its program"
        } else {
            ""
        };
        lines.push(format!("  {} ({} bytes{role})", file.path, file.bytes));
    }
    lines
}

/// What the broker's `outcome` means, for `name` where it's about one.
///
/// # Errors
///
/// When it means nothing changed.
pub fn report(outcome: SkillsResult, name: &str) -> Result<Vec<String>, String> {
    match outcome {
        SkillsResult::Listed { skills } if skills.is_empty() => Ok(vec![String::from(
            "No skills found: Hatchling has none in its skills directory that verify.",
        )]),
        SkillsResult::Listed { skills } => Ok(skills
            .iter()
            .flat_map(|skill| describe(skill, false))
            .collect()),
        SkillsResult::Enabled => Ok(vec![format!(
            "Enabled {name}, as shown. If it changes, it stops until you enable it again."
        )]),
        SkillsResult::Disabled => Ok(vec![format!("Disabled {name}: it doesn't run.")]),
        SkillsResult::NotFound => Err(format!("there's no skill called {name}")),
        SkillsResult::Changed => Err(format!(
            "{name} changed since it was shown, so nothing was enabled: run this again to see it as it is now"
        )),
        SkillsResult::Off => Err(String::from(
            "Hatchling has no skills: it hasn't been given a skills directory",
        )),
        SkillsResult::Failed => Err(String::from(
            "Hatchling couldn't change its skills, so nothing changed: its log says why",
        )),
    }
}

/// Carries out `command`, asking the broker with `ask` and the owner with
/// `confirm`, which is shown the skill to enable and says whether to.
fn run(
    command: &Command,
    mut ask: impl FnMut(SkillsOp) -> Result<SkillsResult, String>,
    confirm: impl FnOnce(&[String]) -> Result<bool, String>,
) -> Result<Vec<String>, String> {
    match command {
        Command::List => report(ask(SkillsOp::List)?, ""),
        Command::Disable { name } => report(ask(SkillsOp::Disable { name: name.clone() })?, name),
        Command::Enable { name } => {
            let listed = match ask(SkillsOp::List)? {
                SkillsResult::Listed { skills } => skills,
                other @ (SkillsResult::Enabled
                | SkillsResult::Disabled
                | SkillsResult::NotFound
                | SkillsResult::Changed
                | SkillsResult::Off
                | SkillsResult::Failed) => return report(other, name),
            };
            let Some(skill) = listed.iter().find(|skill| skill.name == *name) else {
                return report(SkillsResult::NotFound, name);
            };
            if !confirm(&describe(skill, true))? {
                return Err(format!("not enabled: {name} is as it was"));
            }
            let digest = skill.digest.clone();
            report(
                ask(SkillsOp::Enable {
                    name: name.clone(),
                    digest,
                })?,
                name,
            )
        }
    }
}

/// Shows the skill and asks for a yes, read as a line from `input`.
///
/// # Errors
///
/// When the answer can't be read.
pub fn confirm_from(
    shown: &[String],
    input: &mut impl BufRead,
    out: &mut impl std::io::Write,
) -> Result<bool, String> {
    for line in shown {
        let _ = writeln!(out, "{line}");
    }
    let _ = write!(out, "Enable it, as shown? Type yes to enable it: ");
    let _ = out.flush();
    let mut answer = String::new();
    std::io::Read::take(&mut *input, 64)
        .read_line(&mut answer)
        .map_err(|error| format!("can't read the answer: {error}"))?;
    Ok(answer.trim() == "yes")
}

/// Runs `skills` with `args`, the arguments after it.
#[must_use]
pub fn skills_main(program: &str, args: &[String]) -> ExitCode {
    let command = match parse(args) {
        Ok(command) => command,
        Err(why) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{program} skills: {why}\n\n{USAGE}"
            );
            return ExitCode::from(EX_USAGE);
        }
    };
    let path = socket(|key| std::env::var(key).ok());
    let confirm = |shown: &[String]| {
        let stdin = std::io::stdin();
        if !stdin.is_terminal() {
            let _ = writeln!(
                std::io::stderr().lock(),
                "(the answer is read from standard input, which isn't a terminal)"
            );
        }
        confirm_from(shown, &mut stdin.lock(), &mut std::io::stderr().lock())
    };
    match run(&command, |op| ask_at(&path, op), confirm) {
        Ok(lines) => {
            let mut out = std::io::stdout().lock();
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(std::io::stderr().lock(), "{program} skills: {why}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
