//! Reading a web page through the broker (WO-028).
//!
//! The agent names a page and says why; the broker decides everything
//! else. A site the session doesn't hold is put to the owner, and the
//! agent is told it's waiting. The cell never touches the network.

use std::os::unix::net::UnixStream;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{SessionRequest, SessionResponse, WebOp, WebResult};

use crate::Error;

/// Asks the broker for the page at `url`, for `reason`.
///
/// # Errors
///
/// [`Error::WebIo`] when the session fails; [`Error::WebProtocol`] when
/// the broker's reply isn't a web reply.
pub fn fetch(stream: &UnixStream, url: &str, reason: &str) -> Result<WebResult, Error> {
    request(
        stream,
        WebOp::Fetch {
            url: url.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// Asks the broker to search the web for `query`.
///
/// # Errors
///
/// As [`fetch`].
pub fn search(stream: &UnixStream, query: &str) -> Result<WebResult, Error> {
    request(
        stream,
        WebOp::Search {
            query: query.to_owned(),
        },
    )
}

/// Sends `op` and reads the broker's web reply.
fn request(stream: &UnixStream, op: WebOp) -> Result<WebResult, Error> {
    let request = SessionRequest::Web(op);
    let payload = serde_json::to_vec(&request).map_err(|error| Error::WebProtocol {
        reason: format!("failed to encode the web op: {error}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|error| Error::WebProtocol {
            reason: format!("broker reply is not a valid session response: {error}"),
        })?;
    match response {
        SessionResponse::Web(result) => Ok(result),
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
        | SessionResponse::Subagent(_) => Err(Error::WebProtocol {
            reason: String::from("broker gave another kind of reply to a web op"),
        }),
    }
}

/// Maps a frame-layer failure onto the web vocabulary: I/O stays I/O,
/// everything else is a protocol violation.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::WebIo { source };
    }
    Error::WebProtocol {
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

    /// The op goes as the broker reads it, and its web reply comes back.
    #[test]
    fn a_fetch_asks_the_broker_and_returns_its_reply() {
        let (agent, broker) = pair();
        let serve = std::thread::spawn(move || {
            let asked: SessionRequest =
                serde_json::from_slice(&read_frame(&broker).unwrap()).unwrap();
            let reply = SessionResponse::Web(WebResult::Off);
            write_frame(&broker, &serde_json::to_vec(&reply).unwrap()).unwrap();
            asked
        });
        assert_eq!(
            fetch(&agent, "https://a.example/", "why").unwrap(),
            WebResult::Off
        );
        assert_eq!(
            serve.join().unwrap(),
            SessionRequest::Web(WebOp::Fetch {
                url: String::from("https://a.example/"),
                reason: String::from("why"),
            })
        );
    }

    /// A search asks the broker, and its web reply comes back.
    #[test]
    fn a_search_asks_the_broker() {
        let (agent, broker) = pair();
        let serve = std::thread::spawn(move || {
            let asked: SessionRequest =
                serde_json::from_slice(&read_frame(&broker).unwrap()).unwrap();
            let reply = SessionResponse::Web(WebResult::Off);
            write_frame(&broker, &serde_json::to_vec(&reply).unwrap()).unwrap();
            asked
        });
        assert_eq!(search(&agent, "rust").unwrap(), WebResult::Off);
        assert_eq!(
            serve.join().unwrap(),
            SessionRequest::Web(WebOp::Search {
                query: String::from("rust"),
            })
        );
        let (agent, broker) = pair();
        drop(broker);
        assert!(matches!(
            request(
                &agent,
                WebOp::Search {
                    query: String::new()
                }
            ),
            Err(Error::WebIo { .. })
        ));
    }

    /// Another kind of reply, a reply that isn't one, and a closed session
    /// are errors.
    #[test]
    fn a_wrong_or_missing_reply_is_an_error() {
        let (agent, broker) = pair();
        std::thread::spawn(move || {
            let _ = read_frame(&broker);
            let reply = SessionResponse::Skill(hatchling_core::SkillResult::SkillError);
            let _ = write_frame(&broker, &serde_json::to_vec(&reply).unwrap());
        });
        assert!(matches!(
            fetch(&agent, "https://a.example/", "why"),
            Err(Error::WebProtocol { .. })
        ));
        let (agent, broker) = pair();
        std::thread::spawn(move || {
            let _ = read_frame(&broker);
            let _ = write_frame(&broker, b"not json");
        });
        assert!(matches!(
            fetch(&agent, "https://a.example/", "why"),
            Err(Error::WebProtocol { .. })
        ));
        let (agent, broker) = pair();
        drop(broker);
        assert!(matches!(
            fetch(&agent, "https://a.example/", "why"),
            Err(Error::WebIo { .. })
        ));
        assert!(matches!(
            frame_error(hatchling_core::Error::FrameProtocol {
                reason: String::from("bad"),
            }),
            Error::WebProtocol { .. }
        ));
    }
}
