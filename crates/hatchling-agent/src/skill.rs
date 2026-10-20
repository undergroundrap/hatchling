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
    SkillOp, SkillResult,
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
        | SessionResponse::Web(_) => Err(Error::SkillProtocol {
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
}
