//! Running a command in the agent's own computer, through the broker.
//!
//! The agent names a program and its arguments; the broker decides
//! everything else, and has the Room run it. The cell runs nothing: what
//! comes back is what the Room's own programs wrote, and is data.

use std::os::unix::net::UnixStream;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{RoomOp, RoomResult, SessionRequest, SessionResponse};

use crate::Error;

/// Sends `op` and reads the broker's Room reply.
///
/// # Errors
///
/// [`Error::RoomIo`] when the session fails; [`Error::RoomProtocol`] when
/// the broker's reply isn't a Room reply.
pub fn ask(stream: &UnixStream, op: RoomOp) -> Result<RoomResult, Error> {
    let request = SessionRequest::Room(op);
    let payload = serde_json::to_vec(&request).map_err(|error| Error::RoomProtocol {
        reason: format!("failed to encode the Room op: {error}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|error| Error::RoomProtocol {
            reason: format!("broker reply is not a valid session response: {error}"),
        })?;
    match response {
        SessionResponse::Room(result) => Ok(result),
        SessionResponse::Vault(_)
        | SessionResponse::Egress(_)
        | SessionResponse::Channel(_)
        | SessionResponse::Memory(_)
        | SessionResponse::Skill(_)
        | SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_)
        | SessionResponse::Browser(_)
        | SessionResponse::Suggest(_) => Err(Error::RoomProtocol {
            reason: String::from("broker gave another kind of reply to a Room op"),
        }),
    }
}

/// Maps a frame-layer failure onto the Room vocabulary: I/O stays I/O,
/// everything else is a protocol violation.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::RoomIo { source };
    }
    Error::RoomProtocol {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hatchling_core::SuggestResult;

    use super::*;

    fn pair() -> (UnixStream, UnixStream) {
        let (agent, broker) = UnixStream::pair().unwrap();
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        (agent, broker)
    }

    fn stop() -> RoomOp {
        RoomOp::Stop {
            job: String::from("j1-1"),
        }
    }

    /// The op goes as the broker reads it, and its Room reply comes back.
    #[test]
    fn an_op_asks_the_broker_and_returns_its_reply() {
        let (agent, broker) = pair();
        let answer = RoomResult::Unknown {
            job: String::from("j1-1"),
        };
        let reply = SessionResponse::Room(answer.clone());
        let serve = std::thread::spawn(move || {
            let asked: SessionRequest =
                serde_json::from_slice(&read_frame(&broker).unwrap()).unwrap();
            write_frame(&broker, &serde_json::to_vec(&reply).unwrap()).unwrap();
            asked
        });
        assert_eq!(ask(&agent, stop()).unwrap(), answer);
        assert_eq!(serve.join().unwrap(), SessionRequest::Room(stop()));
    }

    /// Another kind of reply, a reply that isn't one, and a session that
    /// is gone are each their own failure, and none is a result.
    #[test]
    fn what_is_not_a_room_reply_is_never_a_result() {
        let (agent, broker) = pair();
        let serve = std::thread::spawn(move || {
            let _ = read_frame(&broker).unwrap();
            let other = SessionResponse::Suggest(SuggestResult::Suggested);
            write_frame(&broker, &serde_json::to_vec(&other).unwrap()).unwrap();
            let _ = read_frame(&broker).unwrap();
            write_frame(&broker, b"{\"kind\":\"room\",\"result\":\"no-such\"}").unwrap();
        });
        let Err(Error::RoomProtocol { reason }) = ask(&agent, stop()) else {
            unreachable!("another kind of reply was taken")
        };
        assert_eq!(reason, "broker gave another kind of reply to a Room op");
        let Err(Error::RoomProtocol { reason }) = ask(&agent, stop()) else {
            unreachable!("a reply that isn't one was taken")
        };
        assert!(
            reason.starts_with("broker reply is not a valid session response"),
            "{reason}"
        );
        serve.join().unwrap();
        assert!(matches!(ask(&agent, stop()), Err(Error::RoomIo { .. })));
    }
}
