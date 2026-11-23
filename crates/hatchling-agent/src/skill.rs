//! Agent side of the skill protocol.
//!
//! The framing and message vocabulary live in [`hatchling_core`]; this
//! module is the agent's behavior: bound the request before any I/O,
//! send one op per frame, and map the broker's typed reply onto typed
//! errors. Must only be called on a [`Ready`](crate::Ready) cell — the
//! typestate enforces this.
//!
//! The output this returns is untrusted. The agent hands it to the model
//! only as a tool result ([`SkillInvokeTool`](crate::tools::SkillInvokeTool)),
//! which the provider wraps in the untrusted-data envelope on every
//! request. There is no other path from a skill to the model.

use std::os::unix::net::UnixStream;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    CapabilityName, MAX_FRAME_BYTES, MAX_SKILL_INPUT_BYTES, SessionRequest, SessionResponse,
    SkillEntry, SkillOp, SkillResult,
};

use crate::Error;

/// Runs `skill` (the exact `name@version` id) on `input`, exercising
/// `capability`, and returns the skill's output.
///
/// # Errors
///
/// Returns [`Error::SkillInputTooLarge`] before any I/O for an input
/// over [`MAX_SKILL_INPUT_BYTES`] or a request that would not fit one
/// frame, [`Error::SkillUnknown`] if no skill has the id,
/// [`Error::SkillDenied`] if the skill did not declare the capability,
/// [`Error::SkillFailed`] if the skill could not run or failed,
/// [`Error::SkillIo`] on transport failure, or [`Error::SkillProtocol`]
/// on a malformed broker reply.
pub fn invoke(
    stream: &UnixStream,
    skill: &str,
    capability: &CapabilityName,
    input: &str,
) -> Result<String, Error> {
    if input.len() > MAX_SKILL_INPUT_BYTES {
        return Err(Error::SkillInputTooLarge {
            len: input.len(),
            max: MAX_SKILL_INPUT_BYTES,
        });
    }
    let request = SessionRequest::Skill(SkillOp::Invoke {
        skill: skill.to_owned(),
        capability: capability.clone(),
        input: input.to_owned(),
    });
    let payload = serde_json::to_vec(&request).map_err(|e| Error::SkillProtocol {
        reason: format!("failed to encode skill op: {e}"),
    })?;
    // A frame the broker would refuse to read ends the session; refuse
    // it here instead, so an oversized skill id costs one error, not the
    // connection.
    if payload.len() > MAX_FRAME_BYTES {
        return Err(Error::SkillInputTooLarge {
            len: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    match request_frame(stream, &payload)? {
        SkillResult::Output { output } => Ok(output),
        SkillResult::UnknownSkill => Err(Error::SkillUnknown {
            skill: skill.to_owned(),
        }),
        SkillResult::CapabilityDenied => Err(Error::SkillDenied {
            skill: skill.to_owned(),
            capability: capability.to_string(),
        }),
        SkillResult::SkillError => Err(Error::SkillFailed {
            skill: skill.to_owned(),
        }),
        other @ (SkillResult::Listed { .. }
        | SkillResult::Text { .. }
        | SkillResult::NoSuchFile
        | SkillResult::TooLarge { .. }
        | SkillResult::NotText) => Err(unexpected(&other)),
    }
}

/// A reply that answers another op.
fn unexpected(reply: &SkillResult) -> Error {
    let kind = match reply {
        SkillResult::Output { .. } => "an output",
        SkillResult::Listed { .. } => "a list",
        SkillResult::Text { .. } => "a read",
        SkillResult::UnknownSkill
        | SkillResult::CapabilityDenied
        | SkillResult::SkillError
        | SkillResult::NoSuchFile
        | SkillResult::TooLarge { .. }
        | SkillResult::NotText => "another op's answer",
    };
    Error::SkillProtocol {
        reason: format!("broker gave {kind} to this skill op"),
    }
}

/// Encodes `op` as one frame's payload.
fn payload(op: SkillOp) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(&SessionRequest::Skill(op)).map_err(|e| Error::SkillProtocol {
        reason: format!("failed to encode skill op: {e}"),
    })
}

/// The skills the owner enabled, as many as one frame carries, and how
/// many more there are.
///
/// # Errors
///
/// [`Error::SkillIo`] on transport failure, [`Error::SkillProtocol`] on a
/// malformed or mismatched reply, [`Error::SkillFailed`] if the broker
/// couldn't list them.
pub fn list(stream: &UnixStream) -> Result<(Vec<SkillEntry>, u32), Error> {
    match request_frame(stream, &payload(SkillOp::List)?)? {
        SkillResult::Listed { skills, more } => Ok((skills, more)),
        SkillResult::SkillError => Err(Error::SkillFailed {
            skill: String::from("the list"),
        }),
        other @ (SkillResult::Output { .. }
        | SkillResult::UnknownSkill
        | SkillResult::CapabilityDenied
        | SkillResult::Text { .. }
        | SkillResult::NoSuchFile
        | SkillResult::TooLarge { .. }
        | SkillResult::NotText) => Err(unexpected(&other)),
    }
}

/// Reads the enabled skill `skill`'s playbook, or the file `path` it
/// lists, fenced and labelled by the broker.
///
/// # Errors
///
/// [`Error::SkillInputTooLarge`] before any I/O for a request one frame
/// can't carry, [`Error::SkillUnknown`] if no enabled skill has the name,
/// [`Error::SkillNoSuchFile`], [`Error::SkillFileTooLarge`] and
/// [`Error::SkillNotText`] for the file, [`Error::SkillFailed`] if the
/// broker couldn't read it, [`Error::SkillIo`] on transport failure, or
/// [`Error::SkillProtocol`] on a malformed reply.
pub fn read(stream: &UnixStream, skill: &str, path: Option<&str>) -> Result<String, Error> {
    let payload = payload(SkillOp::Read {
        skill: skill.to_owned(),
        path: path.map(str::to_owned),
    })?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(Error::SkillInputTooLarge {
            len: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    let skill = skill.to_owned();
    match request_frame(stream, &payload)? {
        SkillResult::Text { text } => Ok(text),
        SkillResult::UnknownSkill => Err(Error::SkillUnknown { skill }),
        SkillResult::NoSuchFile => Err(Error::SkillNoSuchFile {
            skill,
            path: path.unwrap_or_default().to_owned(),
        }),
        SkillResult::TooLarge { bytes } => Err(Error::SkillFileTooLarge { skill, bytes }),
        SkillResult::NotText => Err(Error::SkillNotText { skill }),
        SkillResult::SkillError => Err(Error::SkillFailed { skill }),
        other @ (SkillResult::Output { .. }
        | SkillResult::CapabilityDenied
        | SkillResult::Listed { .. }) => Err(unexpected(&other)),
    }
}

/// Maps a frame-layer failure onto the skill vocabulary: I/O stays I/O,
/// everything else is a protocol violation.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::SkillIo { source };
    }
    Error::SkillProtocol {
        reason: error.to_string(),
    }
}

/// Sends one encoded op and reads the broker's reply. EOF mid-op is a
/// transport failure; a reply of the wrong kind is a protocol violation.
fn request_frame(stream: &UnixStream, payload: &[u8]) -> Result<SkillResult, Error> {
    write_frame(stream, payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|e| Error::SkillProtocol {
            reason: format!("broker reply is not a valid session response: {e}"),
        })?;
    match response {
        SessionResponse::Skill(result) => Ok(result),
        SessionResponse::Vault(_) => Err(Error::SkillProtocol {
            reason: String::from("broker gave a vault reply to a skill op"),
        }),
        SessionResponse::Egress(_) => Err(Error::SkillProtocol {
            reason: String::from("broker gave an egress reply to a skill op"),
        }),
        SessionResponse::Channel(_) => Err(Error::SkillProtocol {
            reason: String::from("broker gave a channel reply to a skill op"),
        }),
        SessionResponse::Memory(_) => Err(Error::SkillProtocol {
            reason: String::from("broker gave a memory reply to a skill op"),
        }),
        SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_)
        | SessionResponse::Browser(_)
        | SessionResponse::Suggest(_)
        | SessionResponse::Room(_)
        | SessionResponse::Chat(_) => Err(Error::SkillProtocol {
            reason: String::from("broker gave a goals or query reply to a skill op"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use hatchling_core::VaultResult;

    use super::*;

    fn cap(raw: &str) -> CapabilityName {
        raw.parse().unwrap()
    }

    /// A broker stand-in: reads one request, answers `reply`, and hands
    /// back what it read.
    fn answer_once(
        reply: SessionResponse,
    ) -> (UnixStream, std::thread::JoinHandle<Option<SessionRequest>>) {
        let (agent, broker) = UnixStream::pair().unwrap();
        // Bounded both ways: a client that wrongly skips the send (or
        // sends when it should not) fails the test instead of hanging it.
        broker
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        agent
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let handle = std::thread::spawn(move || {
            let frame = read_frame(&broker).ok()?;
            let request = serde_json::from_slice(&frame).ok();
            write_frame(&broker, &serde_json::to_vec(&reply).ok()?).ok()?;
            request
        });
        (agent, handle)
    }

    /// Asserts nothing reached the broker end.
    fn assert_silent(broker: &UnixStream) {
        broker.set_nonblocking(true).unwrap();
        let mut byte = [0u8; 1];
        let mut reader = broker;
        let read = reader.read(&mut byte);
        assert!(
            matches!(&read, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "the request must not be sent: {read:?}"
        );
    }

    #[test]
    fn output_is_returned_for_the_exact_request() {
        let (agent, broker) = answer_once(SessionResponse::Skill(SkillResult::Output {
            output: String::from("out"),
        }));
        let output = invoke(&agent, "web@1.0.0", &cap("net.egress"), "in").unwrap();
        assert_eq!(output, "out");
        assert_eq!(
            broker.join().unwrap(),
            Some(SessionRequest::Skill(SkillOp::Invoke {
                skill: String::from("web@1.0.0"),
                capability: cap("net.egress"),
                input: String::from("in"),
            }))
        );
    }

    #[test]
    fn denials_and_failures_are_typed() {
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::UnknownSkill));
        let err = invoke(&agent, "web@1.0.0", &cap("net.egress"), "").unwrap_err();
        assert!(
            matches!(&err, Error::SkillUnknown { skill } if skill == "web@1.0.0"),
            "{err:?}"
        );
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::CapabilityDenied));
        let err = invoke(&agent, "web@1.0.0", &cap("net.egress"), "").unwrap_err();
        assert!(
            matches!(&err, Error::SkillDenied { skill, capability }
                if skill == "web@1.0.0" && capability == "net.egress"),
            "{err:?}"
        );
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::SkillError));
        let err = invoke(&agent, "web@1.0.0", &cap("net.egress"), "").unwrap_err();
        assert!(
            matches!(&err, Error::SkillFailed { skill } if skill == "web@1.0.0"),
            "{err:?}"
        );
    }

    #[test]
    fn a_reply_of_another_kind_is_a_protocol_violation() {
        let (agent, _broker) = answer_once(SessionResponse::Vault(VaultResult::Denied));
        let err = invoke(&agent, "web@1.0.0", &cap("net.egress"), "").unwrap_err();
        assert!(matches!(err, Error::SkillProtocol { .. }), "{err:?}");
    }

    #[test]
    fn frame_failures_keep_their_class() {
        let io = frame_error(hatchling_core::Error::FrameIo {
            source: std::io::Error::other("gone"),
        });
        assert!(matches!(io, Error::SkillIo { .. }), "{io:?}");
        let protocol = frame_error(hatchling_core::Error::FrameProtocol {
            reason: String::from("oversize"),
        });
        assert!(
            matches!(&protocol, Error::SkillProtocol { reason } if reason.contains("oversize")),
            "{protocol:?}"
        );
    }

    /// A pair whose agent end gives up after a moment: nothing answers.
    fn unanswered_pair() -> (UnixStream, UnixStream) {
        let (agent, broker) = UnixStream::pair().unwrap();
        agent
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        (agent, broker)
    }

    #[test]
    fn input_over_the_cap_is_refused_before_any_io() {
        let (agent, broker) = unanswered_pair();
        let input = "x".repeat(MAX_SKILL_INPUT_BYTES + 1);
        let err = invoke(&agent, "web@1.0.0", &cap("net.egress"), &input).unwrap_err();
        assert!(
            matches!(err, Error::SkillInputTooLarge { len, max }
                if len == MAX_SKILL_INPUT_BYTES + 1 && max == MAX_SKILL_INPUT_BYTES),
            "{err:?}"
        );
        assert_silent(&broker);
        // Exactly at the cap is sent.
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::UnknownSkill));
        let input = "x".repeat(MAX_SKILL_INPUT_BYTES);
        let err = invoke(&agent, "web@1.0.0", &cap("net.egress"), &input).unwrap_err();
        assert!(matches!(err, Error::SkillUnknown { .. }), "{err:?}");
    }

    #[test]
    fn a_request_over_one_frame_is_refused_before_any_io() {
        // The encoded size grows one byte per ASCII id byte: size the id
        // so the request is exactly one frame, then one byte more.
        let base = serde_json::to_vec(&SessionRequest::Skill(SkillOp::Invoke {
            skill: String::new(),
            capability: cap("net.egress"),
            input: String::new(),
        }))
        .unwrap()
        .len();
        let exact = "a".repeat(MAX_FRAME_BYTES - base);
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::UnknownSkill));
        let err = invoke(&agent, &exact, &cap("net.egress"), "").unwrap_err();
        assert!(matches!(err, Error::SkillUnknown { .. }), "{err:?}");

        let (agent, broker) = unanswered_pair();
        let over = format!("{exact}a");
        let err = invoke(&agent, &over, &cap("net.egress"), "").unwrap_err();
        assert!(
            matches!(err, Error::SkillInputTooLarge { len, max }
                if len == MAX_FRAME_BYTES + 1 && max == MAX_FRAME_BYTES),
            "{err:?}"
        );
        assert_silent(&broker);
    }

    /// `request_frame`, called directly: a reply of another kind, a web
    /// reply among them, is a protocol violation.
    #[test]
    fn request_frame_refuses_a_reply_of_another_kind() {
        for reply in [
            SessionResponse::Web(hatchling_core::WebResult::Off),
            SessionResponse::Vault(VaultResult::VaultError),
        ] {
            let (agent, _broker) = answer_once(reply);
            assert!(matches!(
                request_frame(&agent, b"{}"),
                Err(Error::SkillProtocol { .. })
            ));
        }
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::SkillError));
        assert_eq!(
            request_frame(&agent, b"{}").unwrap(),
            SkillResult::SkillError
        );
    }

    /// A check on an error, for a table of cases.
    type Check = fn(&Error) -> bool;

    fn entry(name: &str) -> SkillEntry {
        SkillEntry {
            name: name.to_owned(),
            version: String::from("1"),
            description: String::from("Use when asked."),
            playbook: true,
            capabilities: Vec::new(),
            files: false,
        }
    }

    /// The list is asked for as one op, and its answer is the broker's.
    #[test]
    fn list_asks_once_and_takes_the_brokers_answer() {
        let (agent, broker) = answer_once(SessionResponse::Skill(SkillResult::Listed {
            skills: vec![entry("notes")],
            more: 2,
        }));
        assert_eq!(list(&agent).unwrap(), (vec![entry("notes")], 2));
        assert_eq!(
            broker.join().unwrap(),
            Some(SessionRequest::Skill(SkillOp::List))
        );
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::SkillError));
        assert!(matches!(list(&agent), Err(Error::SkillFailed { .. })));
        for other in [
            SkillResult::Text {
                text: String::new(),
            },
            SkillResult::UnknownSkill,
            SkillResult::NotText,
        ] {
            let (agent, _broker) = answer_once(SessionResponse::Skill(other));
            assert!(matches!(list(&agent), Err(Error::SkillProtocol { .. })));
        }
    }

    /// A read names the skill and the file, and each answer is its own
    /// error, naming the skill.
    #[test]
    fn read_names_the_file_and_maps_each_answer() {
        let (agent, broker) = answer_once(SessionResponse::Skill(SkillResult::Text {
            text: String::from("[SKILL notes@1] x [/SKILL]"),
        }));
        assert_eq!(
            read(&agent, "notes", Some("SKILL.md")).unwrap(),
            "[SKILL notes@1] x [/SKILL]"
        );
        assert_eq!(
            broker.join().unwrap(),
            Some(SessionRequest::Skill(SkillOp::Read {
                skill: String::from("notes"),
                path: Some(String::from("SKILL.md")),
            }))
        );
        let (agent, broker) = answer_once(SessionResponse::Skill(SkillResult::NoSuchFile));
        assert!(matches!(
            read(&agent, "notes", None),
            Err(Error::SkillNoSuchFile { skill, path }) if skill == "notes" && path.is_empty()
        ));
        assert_eq!(
            broker.join().unwrap(),
            Some(SessionRequest::Skill(SkillOp::Read {
                skill: String::from("notes"),
                path: None,
            })),
            "no path: its playbook"
        );
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::NoSuchFile));
        assert!(matches!(
            read(&agent, "notes", Some("x.md")),
            Err(Error::SkillNoSuchFile { path, .. }) if path == "x.md"
        ));
        let cases: [(SkillResult, Check); 6] = [
            (
                SkillResult::UnknownSkill,
                |e: &Error| matches!(e, Error::SkillUnknown { skill } if skill == "notes"),
            ),
            (
                SkillResult::TooLarge { bytes: 9 },
                |e: &Error| matches!(e, Error::SkillFileTooLarge { skill, bytes: 9 } if skill == "notes"),
            ),
            (
                SkillResult::NotText,
                |e: &Error| matches!(e, Error::SkillNotText { skill } if skill == "notes"),
            ),
            (
                SkillResult::SkillError,
                |e: &Error| matches!(e, Error::SkillFailed { skill } if skill == "notes"),
            ),
            (
                SkillResult::Output {
                    output: String::new(),
                },
                |e: &Error| matches!(e, Error::SkillProtocol { .. }),
            ),
            (
                SkillResult::Listed {
                    skills: Vec::new(),
                    more: 0,
                },
                |e: &Error| matches!(e, Error::SkillProtocol { .. }),
            ),
        ];
        for (reply, check) in cases {
            let (agent, _broker) = answer_once(SessionResponse::Skill(reply.clone()));
            let error = read(&agent, "notes", None).unwrap_err();
            assert!(check(&error), "{reply:?}: {error:?}");
        }
        // A request of exactly one frame is sent.
        let overhead = payload(SkillOp::Read {
            skill: String::new(),
            path: None,
        })
        .unwrap()
        .len();
        let exact = "n".repeat(MAX_FRAME_BYTES - overhead);
        let (agent, _broker) = answer_once(SessionResponse::Skill(SkillResult::NotText));
        assert!(matches!(
            read(&agent, &exact, None),
            Err(Error::SkillNotText { .. })
        ));
        // A request one frame can't carry is refused before any I/O.
        let (agent, broker) = UnixStream::pair().unwrap();
        let name = "n".repeat(MAX_FRAME_BYTES);
        assert!(matches!(
            read(&agent, &name, None),
            Err(Error::SkillInputTooLarge { max, .. }) if max == MAX_FRAME_BYTES
        ));
        assert_silent(&broker);
    }

    /// Each reply another op gives is named in the protocol error.
    #[test]
    fn a_mismatched_reply_says_what_it_was() {
        for (reply, kind) in [
            (
                SkillResult::Output {
                    output: String::new(),
                },
                "an output",
            ),
            (
                SkillResult::Listed {
                    skills: Vec::new(),
                    more: 0,
                },
                "a list",
            ),
            (
                SkillResult::Text {
                    text: String::new(),
                },
                "a read",
            ),
            (SkillResult::NotText, "another op's answer"),
        ] {
            let Error::SkillProtocol { reason } = unexpected(&reply) else {
                unreachable!("not a protocol error")
            };
            assert_eq!(reason, format!("broker gave {kind} to this skill op"));
        }
    }
}
