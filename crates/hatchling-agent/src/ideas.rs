//! Ideas, and builds proposed from them, through the broker (WO-032).
//!
//! A chat turn may capture, list, refine and note ideas, and propose a
//! build of one for the owner to approve. The broker keeps the ideas,
//! puts each proposal to the owner in the app, and builds only what the
//! owner allows: nothing here builds or delivers anything.

use std::os::unix::net::UnixStream;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{ArtifactOp, ArtifactResult, SessionRequest, SessionResponse};

use crate::Error;

/// Sends `op` and reads the broker's artifact reply.
///
/// # Errors
///
/// [`Error::IdeaIo`] when the session fails; [`Error::IdeaProtocol`] when
/// the broker's reply isn't an artifact reply.
pub fn request(stream: &UnixStream, op: ArtifactOp) -> Result<ArtifactResult, Error> {
    let request = SessionRequest::Artifact(op);
    let payload = serde_json::to_vec(&request).map_err(|error| Error::IdeaProtocol {
        reason: format!("failed to encode the idea op: {error}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|error| Error::IdeaProtocol {
            reason: format!("broker reply is not a valid session response: {error}"),
        })?;
    match response {
        SessionResponse::Artifact(result) => Ok(result),
        SessionResponse::Vault(_)
        | SessionResponse::Egress(_)
        | SessionResponse::Channel(_)
        | SessionResponse::Memory(_)
        | SessionResponse::Skill(_)
        | SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Web(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Browser(_) => Err(Error::IdeaProtocol {
            reason: String::from("broker gave another kind of reply to an idea op"),
        }),
    }
}

/// Maps a frame-layer failure onto the idea vocabulary: I/O stays I/O,
/// everything else is a protocol violation.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::IdeaIo { source };
    }
    Error::IdeaProtocol {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn pair() -> (UnixStream, UnixStream) {
        let (agent, broker) = UnixStream::pair().unwrap();
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        (agent, broker)
    }

    /// The op goes as the broker reads it, and its reply comes back.
    #[test]
    fn an_op_asks_the_broker_and_returns_its_reply() {
        let (agent, broker) = pair();
        let serve = std::thread::spawn(move || {
            let asked: SessionRequest =
                serde_json::from_slice(&read_frame(&broker).unwrap()).unwrap();
            let reply = SessionResponse::Artifact(ArtifactResult::Denied);
            write_frame(&broker, &serde_json::to_vec(&reply).unwrap()).unwrap();
            asked
        });
        let op = ArtifactOp::ListIdeas { after: None };
        assert_eq!(request(&agent, op.clone()).unwrap(), ArtifactResult::Denied);
        assert_eq!(serve.join().unwrap(), SessionRequest::Artifact(op));
    }

    /// Another kind of reply, a reply that isn't one, and a closed session
    /// are errors.
    #[test]
    fn a_wrong_or_missing_reply_is_an_error() {
        let op = || ArtifactOp::ListIdeas { after: None };
        let (agent, broker) = pair();
        std::thread::spawn(move || {
            let _ = read_frame(&broker);
            let reply = SessionResponse::Skill(hatchling_core::SkillResult::SkillError);
            let _ = write_frame(&broker, &serde_json::to_vec(&reply).unwrap());
        });
        assert!(matches!(
            request(&agent, op()),
            Err(Error::IdeaProtocol { .. })
        ));
        let (agent, broker) = pair();
        std::thread::spawn(move || {
            let _ = read_frame(&broker);
            let _ = write_frame(&broker, b"not json");
        });
        assert!(matches!(
            request(&agent, op()),
            Err(Error::IdeaProtocol { .. })
        ));
        let (agent, broker) = pair();
        drop(broker);
        assert!(matches!(request(&agent, op()), Err(Error::IdeaIo { .. })));
        assert!(matches!(
            frame_error(hatchling_core::Error::FrameProtocol {
                reason: String::from("bad"),
            }),
            Error::IdeaProtocol { .. }
        ));
    }
}
