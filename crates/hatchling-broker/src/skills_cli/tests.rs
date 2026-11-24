use std::cell::RefCell;

use hatchling_core::ui::{ActionRefusal, SkillFile};

use super::*;

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_owned).collect()
}

const SIGNER: &str = "3a15e703f405b05e0b0e5943472c8f57567f778ed7ff2393f5ca614b844b1d20";
const DIGEST: &str = "1111111122222222333333334444444455555555666666667777777788888888";

fn listing(name: &str, state: SkillState) -> SkillListing {
    SkillListing {
        name: name.to_owned(),
        version: String::from("1"),
        description: String::from("Use when the owner asks for a report."),
        signer: String::from(SIGNER),
        digest: String::from(DIGEST),
        capabilities: Vec::new(),
        instructions: Some(String::from("SKILL.md")),
        program: None,
        files: vec![SkillFile {
            path: String::from("SKILL.md"),
            bytes: 2256,
        }],
        state,
    }
}

#[test]
fn the_commands_parse_and_nothing_else_does() {
    assert_eq!(parse(&words("list")), Ok(Command::List));
    assert_eq!(
        parse(&words("enable write-a-report")),
        Ok(Command::Enable {
            name: String::from("write-a-report")
        })
    );
    assert_eq!(
        parse(&words("disable write-a-report")),
        Ok(Command::Disable {
            name: String::from("write-a-report")
        })
    );
    for refused in [
        "",
        "list x",
        "enable",
        "enable a b",
        "disable",
        "show write-a-report",
        "enable write-a-report --yes",
    ] {
        assert!(parse(&words(refused)).is_err(), "{refused:?}");
    }
}

#[test]
fn a_skill_is_described_as_the_app_shows_it_in_full_when_it_will_be_enabled() {
    let skill = listing("write-a-report", SkillState::Waiting);
    let listed = describe(&skill, false);
    assert_eq!(
        listed,
        [
            "write-a-report 1: waiting for you",
            "  Use when the owner asks for a report.",
            "  signed by 3a15e703…844b1d20",
            "  digest    11111111…88888888",
            "  enabling it lets it: nothing more: it's a playbook, with no program to run",
            "  SKILL.md (2256 bytes, its playbook)",
        ]
    );
    let whole = describe(&skill, true);
    assert!(
        whole.contains(&format!("  signed by {SIGNER}")),
        "{whole:?}"
    );
    assert!(
        whole.contains(&format!("  digest    {DIGEST}")),
        "{whole:?}"
    );

    let program = SkillListing {
        capabilities: vec![String::from("fs.read"), String::from("net.egress")],
        program: Some(String::from("main")),
        files: vec![SkillFile {
            path: String::from("main"),
            bytes: 7,
        }],
        state: SkillState::Changed,
        instructions: None,
        ..skill
    };
    let said = describe(&program, false);
    assert!(
        said.iter()
            .any(|line| line.ends_with("changed since you enabled it, so it doesn't run"))
    );
    assert!(said.contains(&String::from("  enabling it lets it: fs.read, net.egress")));
    assert!(said.contains(&String::from("  main (7 bytes, its program)")));
    let silent = SkillListing {
        capabilities: Vec::new(),
        ..program
    };
    assert!(describe(&silent, false).contains(&String::from(
        "  enabling it lets it: nothing more: its program declares no capabilities"
    )));
    assert_eq!(short("abc"), "abc", "a short value is shown whole");
    // Shortened only past 16: 16 characters are shown as they are.
    assert_eq!(short("0123456789abcdef"), "0123456789abcdef");
    assert_eq!(short("0123456789abcdefg"), "01234567…9abcdefg");
    assert_eq!(short("0123456789ab"), "0123456789ab");
}

#[test]
fn a_report_says_what_happened_and_what_didnt() {
    assert_eq!(
        report(SkillsResult::Listed { skills: Vec::new() }, ""),
        Ok(vec![String::from(
            "No skills found: Hatchling has none in its skills directory that verify."
        )])
    );
    let two = report(
        SkillsResult::Listed {
            skills: vec![
                listing("a", SkillState::Enabled),
                listing("b", SkillState::Waiting),
            ],
        },
        "",
    )
    .unwrap();
    assert_eq!(two.len(), 12);
    assert!(
        report(SkillsResult::Enabled, "a")
            .is_ok_and(|said| said.concat().contains("Enabled a, as shown"))
    );
    assert!(
        report(SkillsResult::Disabled, "a").is_ok_and(|said| said.concat().contains("Disabled a"))
    );
    for (outcome, why) in [
        (SkillsResult::NotFound, "no skill called a"),
        (
            SkillsResult::Changed,
            "changed since it was shown, so nothing was enabled",
        ),
        (SkillsResult::Off, "hasn't been given a skills directory"),
        (SkillsResult::Failed, "its log says why"),
    ] {
        assert!(
            report(outcome, "a").is_err_and(|said| said.contains(why)),
            "{why}"
        );
    }
}

/// The broker's answers to `run`, in order, and every op it was asked.
struct Broker {
    answers: RefCell<Vec<SkillsResult>>,
    asked: RefCell<Vec<SkillsOp>>,
}

impl Broker {
    const fn new(answers: Vec<SkillsResult>) -> Self {
        Self {
            answers: RefCell::new(answers),
            asked: RefCell::new(Vec::new()),
        }
    }

    fn ask(&self, op: SkillsOp) -> Result<SkillsResult, String> {
        self.asked.borrow_mut().push(op);
        let mut answers = self.answers.borrow_mut();
        if answers.is_empty() {
            return Err(String::from("asked once too often"));
        }
        Ok(answers.remove(0))
    }
}

#[test]
fn enabling_shows_the_skill_asks_and_enables_the_digest_shown() {
    let listed = SkillsResult::Listed {
        skills: vec![
            listing("other", SkillState::Waiting),
            listing("write-a-report", SkillState::Waiting),
        ],
    };
    let broker = Broker::new(vec![listed.clone(), SkillsResult::Enabled]);
    let shown = RefCell::new(Vec::new());
    let command = Command::Enable {
        name: String::from("write-a-report"),
    };
    let said = run(
        &command,
        |op| broker.ask(op),
        |lines| {
            shown.borrow_mut().extend_from_slice(lines);
            Ok(true)
        },
    );
    assert!(said.is_ok_and(|said| said.concat().contains("Enabled write-a-report")));
    assert_eq!(
        *broker.asked.borrow(),
        [
            SkillsOp::List,
            SkillsOp::Enable {
                name: String::from("write-a-report"),
                digest: String::from(DIGEST),
            }
        ]
    );
    assert_eq!(
        shown.borrow().first().map(String::as_str),
        Some("write-a-report 1: waiting for you"),
        "the skill asked about is the one shown"
    );
    assert!(shown.borrow().contains(&format!("  digest    {DIGEST}")));

    // Not a yes: nothing is enabled.
    let broker = Broker::new(vec![listed.clone()]);
    let said = run(&command, |op| broker.ask(op), |_| Ok(false));
    assert_eq!(
        said,
        Err(String::from("not enabled: write-a-report is as it was"))
    );
    assert_eq!(*broker.asked.borrow(), [SkillsOp::List]);

    // Changed between the list and the yes: refused, and said.
    let broker = Broker::new(vec![listed, SkillsResult::Changed]);
    let said = run(&command, |op| broker.ask(op), |_| Ok(true));
    assert!(said.is_err_and(|why| why.contains("changed since it was shown")));
}

#[test]
fn a_skill_not_found_or_skills_off_asks_nothing_of_the_owner() {
    let command = Command::Enable {
        name: String::from("nope"),
    };
    for (answer, why) in [
        (
            SkillsResult::Listed {
                skills: vec![listing("write-a-report", SkillState::Waiting)],
            },
            "no skill called nope",
        ),
        (SkillsResult::Off, "hasn't been given a skills directory"),
    ] {
        let broker = Broker::new(vec![answer]);
        let said = run(
            &command,
            |op| broker.ask(op),
            |_| unreachable!("the owner isn't asked"),
        );
        assert!(said.is_err_and(|said| said.contains(why)), "{why}");
        assert_eq!(*broker.asked.borrow(), [SkillsOp::List]);
    }
}

#[test]
fn listing_and_disabling_ask_once_and_report() {
    let broker = Broker::new(vec![SkillsResult::Listed {
        skills: vec![listing("write-a-report", SkillState::Enabled)],
    }]);
    let said = run(&Command::List, |op| broker.ask(op), |_| unreachable!()).unwrap();
    assert_eq!(
        said.first().map(String::as_str),
        Some("write-a-report 1: enabled")
    );
    assert_eq!(*broker.asked.borrow(), [SkillsOp::List]);

    let broker = Broker::new(vec![SkillsResult::Disabled]);
    let command = Command::Disable {
        name: String::from("write-a-report"),
    };
    let said = run(&command, |op| broker.ask(op), |_| unreachable!()).unwrap();
    assert_eq!(
        said,
        [String::from("Disabled write-a-report: it doesn't run.")]
    );
    assert_eq!(
        *broker.asked.borrow(),
        [SkillsOp::Disable {
            name: String::from("write-a-report")
        }]
    );
}

#[test]
fn only_a_yes_line_enables() {
    for (typed, yes) in [
        ("yes\n", true),
        ("yes", true),
        ("  yes \r\n", true),
        ("y\n", false),
        ("YES\n", false),
        ("no\n", false),
        ("", false),
        ("yes please\n", false),
    ] {
        let mut out = Vec::new();
        let said = confirm_from(
            &[String::from("write-a-report 1: waiting for you")],
            &mut std::io::Cursor::new(typed.as_bytes()),
            &mut out,
        );
        assert_eq!(said, Ok(yes), "{typed:?}");
        let shown = String::from_utf8(out).unwrap();
        assert!(
            shown.starts_with("write-a-report 1: waiting for you\n"),
            "{shown:?}"
        );
        assert!(shown.ends_with("Type yes to enable it: "), "{shown:?}");
    }
    // A yes past the bound read isn't read as one.
    let long = format!("{}yes\n", " ".repeat(70));
    let said = confirm_from(
        &[],
        &mut std::io::Cursor::new(long.into_bytes()),
        &mut Vec::new(),
    );
    assert_eq!(said, Ok(false));
}

/// The broker on the far end of a socket pair: says hello, then answers
/// the one action with `result`, after a stray event of another request.
fn broker_answering(
    result: UiActionResult,
) -> (UnixStream, std::thread::JoinHandle<Option<UiAction>>) {
    let (client, server) = UnixStream::pair().unwrap();
    let handle = std::thread::spawn(move || {
        let read = |stream: &UnixStream| -> Option<UiRequest> {
            serde_json::from_slice(&read_frame(stream).ok()?).ok()
        };
        let send = |stream: &UnixStream, response: &UiResponse| {
            let _ = write_frame(stream, &serde_json::to_vec(response).unwrap_or_default());
        };
        let UiRequest::Hello { client, .. } = read(&server)? else {
            return None;
        };
        assert_eq!(client, "hatchling-broker skills");
        send(
            &server,
            &UiResponse::Welcome {
                schema_version: UI_SCHEMA_VERSION,
                session_protocol: hatchling_core::PROTOCOL_VERSION,
                grants: Vec::new(),
            },
        );
        let UiRequest::Action { request_id, action } = read(&server)? else {
            return None;
        };
        send(
            &server,
            &UiResponse::ActionResult {
                request_id: String::from("another"),
                result: UiActionResult::Skills {
                    outcome: SkillsResult::Failed,
                },
            },
        );
        send(&server, &UiResponse::ActionResult { request_id, result });
        Some(action)
    });
    (client, handle)
}

#[test]
fn the_exchange_says_hello_asks_manage_skills_and_takes_its_own_answer() {
    let (client, broker) = broker_answering(UiActionResult::Skills {
        outcome: SkillsResult::Disabled,
    });
    let op = SkillsOp::Disable {
        name: String::from("write-a-report"),
    };
    assert_eq!(ask(&client, op.clone()), Ok(SkillsResult::Disabled));
    assert_eq!(broker.join().unwrap(), Some(UiAction::ManageSkills { op }));

    let (client, broker) = broker_answering(UiActionResult::Refused {
        reason: ActionRefusal::Unavailable,
    });
    let refused = ask(&client, SkillsOp::List);
    assert!(refused.is_err_and(|why| why.contains("run this as Hatchling's owner")));
    let _ = broker.join();
}

#[test]
fn a_broker_that_wont_say_hello_or_hangs_up_is_said_plainly() {
    let (client, server) = UnixStream::pair().unwrap();
    let handle = std::thread::spawn(move || {
        let _ = read_frame(&server);
        let refuse = UiResponse::Incompatible {
            frontend: 0,
            backend: 1,
            message: String::from("update the app"),
        };
        let _ = write_frame(&server, &serde_json::to_vec(&refuse).unwrap_or_default());
    });
    assert_eq!(
        ask(&client, SkillsOp::List),
        Err(String::from("update the app"))
    );
    let _ = handle.join();

    let (client, server) = UnixStream::pair().unwrap();
    drop(server);
    assert!(ask(&client, SkillsOp::List).is_err());
}
