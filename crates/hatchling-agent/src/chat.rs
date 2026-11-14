//! What the owner sent while a chat run works, asked of the broker.
//!
//! A chat run answers one message. What its owner sends while it works
//! waits with the broker, and the run asks for it between its steps
//! ([`more`]), so it can take it into the task it is on. The broker gives
//! it only to the chat's run in flight; what it gives is the owner's own
//! words, the run's task like the message it was started for.

use std::os::unix::net::UnixStream;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{ChatOp, ChatResult, SessionRequest, SessionResponse};

use crate::Error;

/// What the owner has sent since the run's own message, oldest first.
///
/// # Errors
///
/// [`Error::ChatIo`] when the session fails; [`Error::ChatProtocol`] when
/// the broker's reply isn't a chat reply.
pub fn more(stream: &UnixStream) -> Result<Vec<String>, Error> {
    let request = SessionRequest::Chat(ChatOp::More {});
    let payload = serde_json::to_vec(&request).map_err(|error| Error::ChatProtocol {
        reason: format!("failed to encode the chat op: {error}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|error| Error::ChatProtocol {
            reason: format!("broker reply is not a valid session response: {error}"),
        })?;
    match response {
        SessionResponse::Chat(ChatResult::More { messages }) => Ok(messages),
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
        | SessionResponse::Suggest(_)
        | SessionResponse::Room(_) => Err(Error::ChatProtocol {
            reason: String::from("broker gave another kind of reply to a chat op"),
        }),
    }
}

/// Maps a frame-layer failure onto the chat vocabulary: I/O stays I/O,
/// everything else is a protocol violation.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::ChatIo { source };
    }
    Error::ChatProtocol {
        reason: error.to_string(),
    }
}

/// What a chat run is told of messages that reach it while it works.
pub const NOTE: &str = "The owner can send more while you work. It reaches you as a further message from them, after what you have done so far. Take it into the task you are on: it may add to what they asked, change it, or answer something you needed. Answer everything they sent in one reply.";

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hatchling_core::RoomResult;

    use super::*;

    fn pair() -> (UnixStream, UnixStream) {
        let (agent, broker) = UnixStream::pair().unwrap();
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        (agent, broker)
    }

    /// The op goes as the broker reads it, and its messages come back in
    /// the order the broker gave them.
    #[test]
    fn the_op_asks_the_broker_and_returns_the_owners_messages() {
        let (agent, broker) = pair();
        let serve = std::thread::spawn(move || {
            let asked: SessionRequest =
                serde_json::from_slice(&read_frame(&broker).unwrap()).unwrap();
            let reply = SessionResponse::Chat(ChatResult::More {
                messages: vec![String::from("and make it blue"), String::from("thanks")],
            });
            write_frame(&broker, &serde_json::to_vec(&reply).unwrap()).unwrap();
            asked
        });
        assert_eq!(more(&agent).unwrap(), ["and make it blue", "thanks"]);
        assert_eq!(serve.join().unwrap(), SessionRequest::Chat(ChatOp::More {}));
    }

    /// Another kind of reply, bytes that aren't a reply, and a broker
    /// that has gone are each a failure, and never messages.
    #[test]
    fn what_is_not_a_chat_reply_is_never_messages() {
        let (agent, broker) = pair();
        let serve = std::thread::spawn(move || {
            read_frame(&broker).unwrap();
            let other = SessionResponse::Room(RoomResult::Refused {
                reason: String::from("no"),
            });
            write_frame(&broker, &serde_json::to_vec(&other).unwrap()).unwrap();
            read_frame(&broker).unwrap();
            write_frame(&broker, b"{\"kind\":\"chat\"}").unwrap();
        });
        assert!(matches!(
            more(&agent),
            Err(Error::ChatProtocol { reason }) if reason == "broker gave another kind of reply to a chat op"
        ));
        assert!(matches!(
            more(&agent),
            Err(Error::ChatProtocol { reason }) if reason.starts_with("broker reply is not a valid session response")
        ));
        serve.join().unwrap();
        assert!(matches!(more(&agent), Err(Error::ChatIo { .. })));
    }
}
