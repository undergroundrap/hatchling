//! Driving a browser through the broker (WO-029).
//!
//! The agent names a page and says why, and acts on what the page shows;
//! the broker decides everything else. A site the session doesn't hold is
//! put to the owner, and the agent is told it's waiting. The browser runs
//! in a cell of its own: this cell never touches it, or the network.

use std::os::unix::net::UnixStream;

use hatchling_core::browser::PageAction;
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{BrowserOp, BrowserResult, SessionRequest, SessionResponse};

use crate::Error;

/// Asks the broker to open the page at `url`, for `reason`.
///
/// # Errors
///
/// [`Error::BrowserIo`] when the session fails; [`Error::BrowserProtocol`]
/// when the broker's reply isn't a browser reply.
pub fn open(stream: &UnixStream, url: &str, reason: &str) -> Result<BrowserResult, Error> {
    request(
        stream,
        BrowserOp::Open {
            url: url.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// The open page as it is now, scrolled a screen down or up first when
/// `scroll` says so.
///
/// # Errors
///
/// As [`open`].
pub fn read(stream: &UnixStream, scroll: Option<bool>) -> Result<BrowserResult, Error> {
    request(
        stream,
        scroll.map_or(BrowserOp::Read, |down| BrowserOp::Scroll { down }),
    )
}

/// Acts on `element` of the last page: `action`.
///
/// # Errors
///
/// As [`open`].
pub fn act(stream: &UnixStream, element: u32, action: PageAction) -> Result<BrowserResult, Error> {
    request(stream, BrowserOp::Act { element, action })
}

/// Asks the owner to allow `site`, which the browser was refused for the
/// page, for `reason`.
///
/// # Errors
///
/// As [`open`].
pub fn allow(stream: &UnixStream, site: &str, reason: &str) -> Result<BrowserResult, Error> {
    request(
        stream,
        BrowserOp::Allow {
            site: site.to_owned(),
            reason: reason.to_owned(),
        },
    )
}

/// Signs in on the page with the sign-in the owner keeps for its site:
/// the `password` field, and the `username` one if the form has it, by
/// their ids; `label` says which sign-in when there are several.
///
/// # Errors
///
/// As [`open`].
pub fn sign_in(
    stream: &UnixStream,
    username: Option<u32>,
    password: u32,
    label: Option<&str>,
) -> Result<BrowserResult, Error> {
    request(
        stream,
        BrowserOp::SignIn {
            username,
            password,
            label: label.map(str::to_owned),
        },
    )
}

/// Closes the browser.
///
/// # Errors
///
/// As [`open`].
pub fn close(stream: &UnixStream) -> Result<BrowserResult, Error> {
    request(stream, BrowserOp::Close)
}

/// Sends `op` and reads the broker's browser reply.
fn request(stream: &UnixStream, op: BrowserOp) -> Result<BrowserResult, Error> {
    let request = SessionRequest::Browser(op);
    let payload = serde_json::to_vec(&request).map_err(|error| Error::BrowserProtocol {
        reason: format!("failed to encode the browser op: {error}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|error| Error::BrowserProtocol {
            reason: format!("broker reply is not a valid session response: {error}"),
        })?;
    match response {
        SessionResponse::Browser(result) => Ok(result),
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
        | SessionResponse::Web(_) => Err(Error::BrowserProtocol {
            reason: String::from("broker gave another kind of reply to a browser op"),
        }),
    }
}

/// Maps a frame-layer failure onto the browser vocabulary: I/O stays I/O,
/// everything else is a protocol violation.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::BrowserIo { source };
    }
    Error::BrowserProtocol {
        reason: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hatchling_core::WebResult;

    use super::*;

    fn pair() -> (UnixStream, UnixStream) {
        let (agent, broker) = UnixStream::pair().unwrap();
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        }
        (agent, broker)
    }

    /// The broker, answering one op with `reply`: the op it read.
    fn broker(
        broker: UnixStream,
        reply: SessionResponse,
    ) -> std::thread::JoinHandle<SessionRequest> {
        std::thread::spawn(move || {
            let asked: SessionRequest =
                serde_json::from_slice(&read_frame(&broker).unwrap()).unwrap();
            write_frame(&broker, &serde_json::to_vec(&reply).unwrap()).unwrap();
            asked
        })
    }

    /// One of this module's calls, on a stream.
    type Call = fn(&UnixStream) -> Result<BrowserResult, Error>;

    /// Each op goes as the broker reads it, and its browser reply comes
    /// back.
    #[test]
    fn each_op_asks_the_broker_and_returns_its_reply() {
        let calls: [(Call, BrowserOp); 6] = [
            (
                |stream| open(stream, "https://a.example/", "why"),
                BrowserOp::Open {
                    url: String::from("https://a.example/"),
                    reason: String::from("why"),
                },
            ),
            (|stream| read(stream, None), BrowserOp::Read),
            (
                |stream| read(stream, Some(false)),
                BrowserOp::Scroll { down: false },
            ),
            (
                |stream| act(stream, 7, PageAction::Click),
                BrowserOp::Act {
                    element: 7,
                    action: PageAction::Click,
                },
            ),
            (
                |stream| allow(stream, "cdn.example:443", "fonts"),
                BrowserOp::Allow {
                    site: String::from("cdn.example:443"),
                    reason: String::from("fonts"),
                },
            ),
            (close, BrowserOp::Close),
        ];
        for (call, op) in calls {
            let (agent, theirs) = pair();
            let serve = broker(theirs, SessionResponse::Browser(BrowserResult::Closed));
            assert_eq!(call(&agent).unwrap(), BrowserResult::Closed);
            assert_eq!(serve.join().unwrap(), SessionRequest::Browser(op));
        }
    }

    /// Another kind of reply, or bytes that aren't one, end in a
    /// protocol error; a session that's gone is I/O.
    #[test]
    fn a_reply_that_isnt_the_browsers_is_refused() {
        let (agent, theirs) = pair();
        let serve = broker(theirs, SessionResponse::Web(WebResult::Off));
        let refused = close(&agent).unwrap_err();
        assert!(
            matches!(refused, Error::BrowserProtocol { .. }),
            "{refused}"
        );
        serve.join().unwrap();

        let (agent, theirs) = pair();
        let serve = std::thread::spawn(move || {
            let _ = read_frame(&theirs).unwrap();
            write_frame(&theirs, b"not json").unwrap();
        });
        let refused = close(&agent).unwrap_err();
        assert!(
            refused.to_string().contains("not a valid session response"),
            "{refused}"
        );
        serve.join().unwrap();

        let (agent, theirs) = pair();
        drop(theirs);
        let refused = close(&agent).unwrap_err();
        assert!(matches!(refused, Error::BrowserIo { .. }), "{refused}");
    }

    #[test]
    fn a_frame_failure_is_io_or_protocol() {
        let io = frame_error(hatchling_core::Error::FrameIo {
            source: std::io::Error::other("gone"),
        });
        assert!(matches!(io, Error::BrowserIo { .. }));
        let protocol = frame_error(hatchling_core::Error::FrameProtocol {
            reason: String::from("bad"),
        });
        assert!(matches!(protocol, Error::BrowserProtocol { .. }));
    }
}
