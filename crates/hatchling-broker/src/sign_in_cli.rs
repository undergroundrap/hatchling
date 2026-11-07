//! `hatchling-broker sign-in`: the owner's site sign-ins, from a terminal.
//!
//! The app never takes a secret, so a sign-in is typed here. A client of
//! the local API, as `schedule` is: run as the owner, without `sudo`, and
//! the broker decides everything. It adds, lists and removes. Nothing here
//! shows a stored username or password, to anyone.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use hatchling_core::cli::EX_USAGE;
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::ui::{
    Concealed, ProfileOp, ProfileResult, SignInOp, SignInRefusal, SignInResult, SiteSignIn,
    UI_SCHEMA_VERSION, UiAction, UiActionResult, UiRequest, UiResponse,
};

use crate::schedule_cli::socket;

/// How long the broker has to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// Who this command says it is.
const CLIENT: &str = "hatchling-broker sign-in";

/// The usage, for a command that doesn't parse.
pub const USAGE: &str = "\
Usage:
  hatchling-broker sign-in list
  hatchling-broker sign-in add <https://site> <name>
  hatchling-broker sign-in remove <id>

Run it as yourself, not with sudo. `add` asks for the username, then the
password, here: neither goes through the app. The site has to be one you
kept signed in, by taking control of the browser and choosing Keep me
signed in. No command shows a stored username or password.";

/// One parsed `sign-in` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// List the sign-ins.
    List,
    /// Keep a sign-in on `origin`, called `label`.
    Add {
        /// The exact origin it's filled on.
        origin: String,
        /// What the owner calls it.
        label: String,
    },
    /// Remove the sign-in `id`.
    Remove {
        /// Its id.
        id: String,
    },
}

/// Parses the arguments after `sign-in`.
///
/// # Errors
///
/// When they aren't a command.
pub fn parse(args: &[String]) -> Result<Command, String> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["list"] => Ok(Command::List),
        ["add", origin, label] => Ok(Command::Add {
            origin: (*origin).to_owned(),
            label: (*label).to_owned(),
        }),
        ["remove", id] => Ok(Command::Remove {
            id: (*id).to_owned(),
        }),
        _ => Err(String::from("that isn't a sign-in command")),
    }
}

/// `origin`'s site as a profile lists it, `host:port`.
fn site_of(origin: &str) -> Option<String> {
    let host = origin.strip_prefix("https://")?;
    if host.is_empty() || host.contains('/') {
        return None;
    }
    Some(if host.contains(':') {
        host.to_owned()
    } else {
        format!("{host}:443")
    })
}

/// A connection to the API socket at `path`, said hello on.
fn connect(path: &Path) -> Result<UnixStream, String> {
    let stream = UnixStream::connect(path).map_err(|error| {
        format!(
            "can't reach Hatchling's API at {} ({error}): is it running, with HATCHLING_API_SOCKET set if it's elsewhere?",
            path.display()
        )
    })?;
    stream
        .set_read_timeout(Some(ANSWER_TIMEOUT))
        .map_err(|error| error.to_string())?;
    send(
        &stream,
        &UiRequest::Hello {
            schema_version: UI_SCHEMA_VERSION,
            client: String::from(CLIENT),
            token: None,
        },
    )?;
    match receive(&stream)? {
        UiResponse::Welcome { .. } => Ok(stream),
        UiResponse::Incompatible { message, .. } => Err(message),
        UiResponse::Event(_) | UiResponse::ActionResult { .. } => {
            Err(String::from("the broker didn't say hello"))
        }
    }
}

fn send(stream: &UnixStream, request: &UiRequest) -> Result<(), String> {
    let bytes = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    write_frame(stream, &bytes).map_err(|error| format!("couldn't reach the broker: {error}"))
}

fn receive(stream: &UnixStream) -> Result<UiResponse, String> {
    let bytes = read_frame(stream).map_err(|_| {
        String::from(
            "the broker closed the connection: run this as Hatchling's owner, without sudo",
        )
    })?;
    serde_json::from_slice(&bytes)
        .map_err(|_| String::from("the broker answered with something this command can't read"))
}

/// Asks the broker `action`, and its answer.
fn ask(stream: &UnixStream, action: UiAction) -> Result<UiActionResult, String> {
    let request_id = String::from("sign-in-1");
    send(
        stream,
        &UiRequest::Action {
            request_id: request_id.clone(),
            action,
        },
    )?;
    loop {
        match receive(stream)? {
            UiResponse::ActionResult {
                request_id: answered,
                result,
            } if answered == request_id => return Ok(result),
            // Nothing is subscribed to, but whatever else arrives isn't
            // the answer.
            UiResponse::Welcome { .. }
            | UiResponse::Incompatible { .. }
            | UiResponse::Event(_)
            | UiResponse::ActionResult { .. } => {}
        }
    }
}

/// The result of `op`.
fn sign_ins(stream: &UnixStream, op: SignInOp) -> Result<SignInResult, String> {
    let result = ask(stream, UiAction::ManageSignIns { op })?;
    if let UiActionResult::SignIns { outcome } = result {
        Ok(outcome)
    } else if let UiActionResult::Refused { .. } = result {
        Err(String::from("this connection may not change the sign-ins"))
    } else {
        Err(String::from("the broker answered with something else"))
    }
}

/// The profile holding `site`, or why there's none.
fn profile_of(stream: &UnixStream, site: &str) -> Result<String, String> {
    let op = ProfileOp::List;
    let UiActionResult::Profiles { outcome } = ask(stream, UiAction::ManageProfiles { op })? else {
        return Err(String::from("the broker didn't list the signed-in sites"));
    };
    let ProfileResult::Listed { profiles } = outcome else {
        return Err(String::from("the broker didn't list the signed-in sites"));
    };
    profiles
        .into_iter()
        .find(|profile| profile.sites.iter().any(|kept| kept == site))
        .map(|profile| profile.profile_id)
        .ok_or_else(|| {
            format!(
                "{site} isn't a signed-in site. Take control of the browser, sign in there, and choose Keep me signed in first"
            )
        })
}

/// What a refusal means, in a sentence.
const fn refusal(reason: SignInRefusal) -> &'static str {
    match reason {
        SignInRefusal::NoProfile => "that signed-in site is gone",
        SignInRefusal::BadOrigin => "that isn't an https site on its own",
        SignInRefusal::NotTheProfiles => "that site isn't the signed-in site's",
        SignInRefusal::BadLabel => "the name is empty, or has a line break",
        SignInRefusal::NoPassword => "the password is empty",
        SignInRefusal::TooLong => "the name, username or password is too long",
        SignInRefusal::Full => "Hatchling keeps as many sign-ins as it can: remove one first",
    }
}

/// What the broker's `outcome` means, one line each.
///
/// # Errors
///
/// When it means nothing changed.
pub fn report(outcome: SignInResult) -> Result<Vec<String>, String> {
    let line = |sign_in: &SiteSignIn| {
        format!(
            "{}  {}  {}",
            sign_in.sign_in_id, sign_in.origin, sign_in.label
        )
    };
    match outcome {
        SignInResult::Listed { sign_ins } if sign_ins.is_empty() => {
            Ok(vec![String::from("No sign-ins are kept.")])
        }
        SignInResult::Listed { sign_ins } => Ok(sign_ins.iter().map(line).collect()),
        SignInResult::Stored { sign_in } => Ok(vec![format!(
            "Kept {}. The agent can sign in with it on {}, and never sees the password.",
            sign_in.label, sign_in.origin
        )]),
        SignInResult::Deleted => Ok(vec![String::from("Removed.")]),
        SignInResult::NotFound => Err(String::from("there's no such sign-in")),
        SignInResult::Refused { reason } => Err(format!("not kept: {}", refusal(reason))),
        SignInResult::Off => Err(String::from(
            "Hatchling keeps no sign-ins: it hasn't been given a place to",
        )),
        SignInResult::Failed => Err(String::from(
            "Hatchling couldn't change its sign-ins: its log says why",
        )),
    }
}

/// The username: a line, shown as it's typed, as on any sign-in page.
fn read_username() -> Result<Concealed, String> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        let mut said = std::io::stderr().lock();
        let _ = write!(said, "Username: ");
        let _ = said.flush();
    }
    let mut line = String::new();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|error| format!("can't read the username: {error}"))?;
    Ok(Concealed::new(
        line.trim_end_matches(['\r', '\n']).to_owned(),
    ))
}

/// The password, at a prompt that doesn't show it.
fn read_password() -> Result<Concealed, String> {
    let secret = crate::secret_ops::read_secret("Password (shows as *):")
        .map_err(|error| error.to_string())?;
    let text = std::str::from_utf8(secret.as_bytes())
        .map_err(|_| String::from("the password isn't text"))?;
    Ok(Concealed::new(text.to_owned()))
}

/// Carries out `command` against the API socket at `path`.
fn run(command: &Command, path: &Path) -> Result<Vec<String>, String> {
    let stream = connect(path)?;
    let outcome = match command {
        Command::List => sign_ins(&stream, SignInOp::List)?,
        Command::Remove { id } => sign_ins(
            &stream,
            SignInOp::Delete {
                sign_in_id: id.clone(),
            },
        )?,
        Command::Add { origin, label } => {
            let site = site_of(origin).ok_or_else(|| {
                String::from("name the site as https://host, with nothing after it")
            })?;
            // Before any secret is typed: a site that can't take one
            // shouldn't be told it.
            let profile_id = profile_of(&stream, &site)?;
            let username = read_username()?;
            let password = read_password()?;
            sign_ins(
                &stream,
                SignInOp::Store {
                    profile_id,
                    origin: origin.clone(),
                    label: label.clone(),
                    username,
                    password,
                },
            )?
        }
    };
    report(outcome)
}

/// Runs `sign-in` with `args`, the arguments after it.
#[must_use]
pub fn sign_in_main(program: &str, args: &[String]) -> ExitCode {
    let command = match parse(args) {
        Ok(command) => command,
        Err(why) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{program} sign-in: {why}\n\n{USAGE}"
            );
            return ExitCode::from(EX_USAGE);
        }
    };
    let path = socket(|key| std::env::var(key).ok());
    match run(&command, &path) {
        Ok(lines) => {
            let mut out = std::io::stdout().lock();
            for line in lines {
                let _ = writeln!(out, "{line}");
            }
            ExitCode::SUCCESS
        }
        Err(why) => {
            let _ = writeln!(std::io::stderr().lock(), "{program} sign-in: {why}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn the_commands_parse_and_nothing_else_does() {
        assert_eq!(parse(&words("list")), Ok(Command::List));
        assert_eq!(
            parse(&words("add https://a.example Work")),
            Ok(Command::Add {
                origin: String::from("https://a.example"),
                label: String::from("Work"),
            })
        );
        assert_eq!(
            parse(&words("remove sign-1")),
            Ok(Command::Remove {
                id: String::from("sign-1")
            })
        );
        for refused in ["", "list x", "add https://a.example", "show sign-1", "reveal sign-1"] {
            assert!(parse(&words(refused)).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_site_is_its_profile_form() {
        assert_eq!(
            site_of("https://a.example").as_deref(),
            Some("a.example:443")
        );
        assert_eq!(
            site_of("https://a.example:8443").as_deref(),
            Some("a.example:8443")
        );
        for refused in ["http://a.example", "https://", "https://a.example/login", "a.example"] {
            assert_eq!(site_of(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn a_report_says_what_happened_and_never_more() {
        let sign_in = SiteSignIn {
            sign_in_id: String::from("sign-1"),
            profile_id: String::from("profile-1"),
            origin: String::from("https://a.example"),
            label: String::from("Work"),
            created_at: String::from("2026-11-07T00:00:00Z"),
        };
        assert_eq!(
            report(SignInResult::Listed {
                sign_ins: vec![sign_in.clone()]
            }),
            Ok(vec![String::from("sign-1  https://a.example  Work")])
        );
        assert_eq!(
            report(SignInResult::Listed {
                sign_ins: Vec::new()
            }),
            Ok(vec![String::from("No sign-ins are kept.")])
        );
        assert!(report(SignInResult::Stored { sign_in }).is_ok_and(|said| said
            .concat()
            .contains("never sees the password")));
        assert_eq!(
            report(SignInResult::Deleted),
            Ok(vec![String::from("Removed.")])
        );
        for (outcome, why) in [
            (SignInResult::NotFound, "no such sign-in"),
            (SignInResult::Off, "keeps no sign-ins"),
            (SignInResult::Failed, "its log says why"),
            (
                SignInResult::Refused {
                    reason: SignInRefusal::NoPassword,
                },
                "password is empty",
            ),
        ] {
            assert!(
                report(outcome).is_err_and(|said| said.contains(why)),
                "{why}"
            );
        }
    }

    #[test]
    fn every_refusal_is_said_plainly() {
        for (reason, why) in [
            (SignInRefusal::NoProfile, "gone"),
            (SignInRefusal::BadOrigin, "https site"),
            (SignInRefusal::NotTheProfiles, "isn't the signed-in site's"),
            (SignInRefusal::BadLabel, "name is empty"),
            (SignInRefusal::NoPassword, "password is empty"),
            (SignInRefusal::TooLong, "too long"),
            (SignInRefusal::Full, "remove one first"),
        ] {
            assert!(refusal(reason).contains(why), "{why}");
        }
    }
}
