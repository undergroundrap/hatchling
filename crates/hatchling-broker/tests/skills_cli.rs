//! `hatchling-broker skills` as the owner runs it (#617): the real binary,
//! against an API socket of the test's that answers as the broker does,
//! and records what it was asked.

use std::io::Write as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::ui::{
    SkillFile, SkillListing, SkillState, SkillsOp, SkillsResult, UI_SCHEMA_VERSION, UiAction,
    UiActionResult, UiRequest, UiResponse,
};

type Failed = Box<dyn std::error::Error>;

const DIGEST: &str = "1111111122222222333333334444444455555555666666667777777788888888";

fn listed() -> SkillsResult {
    SkillsResult::Listed {
        skills: vec![SkillListing {
            name: String::from("write-a-report"),
            version: String::from("1"),
            description: String::from("Use when the owner asks for a report."),
            signer: "ab".repeat(32),
            digest: String::from(DIGEST),
            capabilities: Vec::new(),
            instructions: Some(String::from("SKILL.md")),
            program: None,
            files: vec![SkillFile {
                path: String::from("SKILL.md"),
                bytes: 2256,
            }],
            state: SkillState::Waiting,
        }],
    }
}

fn place(name: &str) -> Result<PathBuf, Failed> {
    let dir = std::env::temp_dir().join(format!(
        "hatchling-skills-cli-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Answers one connection: the hello, then the action with `answer`.
/// The action asked for, if one came.
fn answer_one(stream: &UnixStream, answer: SkillsResult) -> Result<Option<SkillsOp>, Failed> {
    let request: UiRequest = serde_json::from_slice(&read_frame(stream)?)?;
    let UiRequest::Hello { .. } = request else {
        return Ok(None);
    };
    let welcome = UiResponse::Welcome {
        schema_version: UI_SCHEMA_VERSION,
        session_protocol: hatchling_core::PROTOCOL_VERSION,
        grants: Vec::new(),
    };
    write_frame(stream, &serde_json::to_vec(&welcome)?)?;
    let request: UiRequest = serde_json::from_slice(&read_frame(stream)?)?;
    let UiRequest::Action {
        request_id,
        action: UiAction::ManageSkills { op },
    } = request
    else {
        return Ok(None);
    };
    let reply = UiResponse::ActionResult {
        request_id,
        result: UiActionResult::Skills { outcome: answer },
    };
    write_frame(stream, &serde_json::to_vec(&reply)?)?;
    Ok(Some(op))
}

/// An API socket at `path` answering each connection in turn with the
/// next of `answers`; every op it was asked, once they're all answered.
fn api(
    path: &Path,
    answers: Vec<SkillsResult>,
) -> Result<std::thread::JoinHandle<Vec<SkillsOp>>, Failed> {
    let listener = UnixListener::bind(path)?;
    Ok(std::thread::spawn(move || {
        let mut asked = Vec::new();
        for answer in answers {
            let Ok((stream, _)) = listener.accept() else {
                break;
            };
            if let Ok(Some(op)) = answer_one(&stream, answer) {
                asked.push(op);
            }
        }
        asked
    }))
}

fn skills(socket: &Path, args: &[&str], typed: &str) -> Result<Output, Failed> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("skills")
        .args(args)
        .env_clear()
        .env("HATCHLING_API_SOCKET", socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(typed.as_bytes())?;
    Ok(child.wait_with_output()?)
}

#[test]
fn enable_shows_the_skill_takes_a_typed_yes_and_enables_the_digest_shown() -> Result<(), Failed> {
    let dir = place("enable")?;
    let socket = dir.join("api.sock");
    let broker = api(&socket, vec![listed(), SkillsResult::Enabled])?;
    let done = skills(&socket, &["enable", "write-a-report"], "yes\n")?;
    let said = String::from_utf8_lossy(&done.stderr);
    assert!(done.status.success(), "{said}");
    assert!(
        said.contains("which isn't a terminal"),
        "a piped answer is said to be one: {said}"
    );
    assert!(said.contains(&format!("digest    {DIGEST}")), "{said}");
    assert!(said.contains("Type yes to enable it"), "{said}");
    assert!(String::from_utf8_lossy(&done.stdout).contains("Enabled write-a-report, as shown"));
    assert_eq!(
        broker.join().map_err(|_| "the API thread panicked")?,
        [
            SkillsOp::List,
            SkillsOp::Enable {
                name: String::from("write-a-report"),
                digest: String::from(DIGEST),
            }
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn anything_but_yes_enables_nothing_and_fails() -> Result<(), Failed> {
    let dir = place("no")?;
    let socket = dir.join("api.sock");
    let broker = api(&socket, vec![listed()])?;
    let done = skills(&socket, &["enable", "write-a-report"], "no\n")?;
    assert_eq!(done.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&done.stderr).contains("not enabled: write-a-report is as it was")
    );
    assert_eq!(
        broker.join().map_err(|_| "the API thread panicked")?,
        [SkillsOp::List]
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_command_that_doesnt_parse_or_a_broker_not_there_is_said() -> Result<(), Failed> {
    let dir = place("usage")?;
    let socket = dir.join("api.sock");
    let done = skills(&socket, &["show", "write-a-report"], "")?;
    assert_eq!(done.status.code(), Some(64), "usage");
    assert!(
        String::from_utf8_lossy(&done.stderr).contains("hatchling-broker skills enable <name>")
    );
    let done = skills(&socket, &["list"], "")?;
    assert_eq!(done.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&done.stderr).contains("can't reach Hatchling's API"));
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
