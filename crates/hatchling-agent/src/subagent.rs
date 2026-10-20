//! Agent side of subagent delegation.
//!
//! The protocol and its bounds live in [`hatchling_core::subagent`]; the
//! broker decides everything that bounds a child. This module is the
//! agent's part: one op per frame, the broker's typed reply back, and
//! the run's [`Children`], so that a run ending, however it ends, kills
//! every child it left running (the orphan rule, from the parent's side;
//! the broker enforces it again when the session ends).

use std::os::unix::net::UnixStream;
use std::sync::Mutex;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{SessionRequest, SessionResponse, SubagentOp, SubagentResult};

use crate::Error;

/// Sends one subagent op and returns the broker's reply.
///
/// # Errors
///
/// [`Error::SubagentIo`] on transport failure, [`Error::SubagentProtocol`]
/// for a reply that is not a subagent reply.
pub fn request(stream: &UnixStream, op: SubagentOp) -> Result<SubagentResult, Error> {
    let payload =
        serde_json::to_vec(&SessionRequest::Subagent(op)).map_err(|e| Error::SubagentProtocol {
            reason: format!("failed to encode subagent op: {e}"),
        })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|e| Error::SubagentProtocol {
            reason: format!("broker reply is not a valid session response: {e}"),
        })?;
    match response {
        SessionResponse::Subagent(result) => Ok(result),
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
        | SessionResponse::Web(_) => Err(Error::SubagentProtocol {
            reason: String::from("broker gave another kind of reply to a subagent op"),
        }),
    }
}

fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::SubagentIo { source };
    }
    Error::SubagentProtocol {
        reason: error.to_string(),
    }
}

/// The children a run has spawned and not yet taken a report from.
#[derive(Debug, Default)]
pub struct Children {
    live: Mutex<Vec<String>>,
}

impl Children {
    /// Records a child the broker started.
    pub fn track(&self, child: &str) {
        self.with_live(|live| live.push(child.to_owned()));
    }

    /// Forgets a child whose report was taken.
    pub fn untrack(&self, child: &str) {
        self.with_live(|live| live.retain(|known| known != child));
    }

    /// The children still running, in spawn order.
    #[must_use]
    pub fn live(&self) -> Vec<String> {
        let mut snapshot = Vec::new();
        self.with_live(|live| snapshot.clone_from(live));
        snapshot
    }

    /// Kills every child still running, with `reason`: the run that
    /// spawned them is over. Returns the children it reaped.
    pub fn reap(&self, stream: &UnixStream, reason: &str) -> Vec<String> {
        let mut reaped = Vec::new();
        for child in self.live() {
            // Whatever the broker answers, the child is no longer this
            // run's: the broker reaps it at session end in any case.
            let _ = request(
                stream,
                SubagentOp::Kill {
                    child: child.clone(),
                    reason: reason.to_owned(),
                },
            );
            self.untrack(&child);
            reaped.push(child);
        }
        reaped
    }

    /// Runs `f` on the list. A list poisoned by a panic elsewhere is
    /// still the list: reaping must not be skipped because of it.
    fn with_live(&self, f: impl FnOnce(&mut Vec<String>)) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut live);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A connected pair whose reads give up after 5 s: a peer that never
    /// answers (a broken frame codec, say) fails the test, never hangs it.
    fn bounded_pair() -> (UnixStream, UnixStream) {
        let (agent, broker) = UnixStream::pair().unwrap();
        for end in [&agent, &broker] {
            end.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
        }
        (agent, broker)
    }

    #[test]
    fn children_are_tracked_until_their_report_is_taken() {
        let children = Children::default();
        assert!(children.live().is_empty());
        children.track("run-a");
        children.track("run-b");
        children.track("run-c");
        children.untrack("run-b");
        assert_eq!(children.live(), ["run-a", "run-c"]);
        children.untrack("unknown");
        assert_eq!(children.live(), ["run-a", "run-c"]);
    }

    #[test]
    fn reaping_kills_every_live_child_and_forgets_it() {
        let (agent, broker) = bounded_pair();
        let children = Children::default();
        children.track("run-a");
        children.track("run-b");
        let answering = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let frame = read_frame(&broker).unwrap();
                seen.push(serde_json::from_slice::<SessionRequest>(&frame).unwrap());
                let reply =
                    serde_json::to_vec(&SessionResponse::Subagent(SubagentResult::SubagentError))
                        .unwrap();
                write_frame(&broker, &reply).unwrap();
            }
            seen
        });
        assert_eq!(children.reap(&agent, "done"), ["run-a", "run-b"]);
        assert!(children.live().is_empty());
        let kill = |child: &str| {
            SessionRequest::Subagent(SubagentOp::Kill {
                child: child.to_owned(),
                reason: String::from("done"),
            })
        };
        assert_eq!(answering.join().unwrap(), [kill("run-a"), kill("run-b")]);
        // Nothing left: no frame is sent.
        assert!(children.reap(&agent, "again").is_empty());
    }

    #[test]
    fn a_reply_of_another_kind_is_a_protocol_error() {
        let (agent, broker) = bounded_pair();
        let answering = std::thread::spawn(move || {
            let _ = read_frame(&broker).unwrap();
            let reply = serde_json::to_vec(&SessionResponse::Space(
                hatchling_core::SpaceResult::SpaceError,
            ))
            .unwrap();
            write_frame(&broker, &reply).unwrap();
        });
        let error = request(
            &agent,
            SubagentOp::Wait {
                child: String::from("run-a"),
            },
        )
        .unwrap_err();
        assert!(
            matches!(&error, Error::SubagentProtocol { reason } if reason == "broker gave another kind of reply to a subagent op"),
            "{error:?}"
        );
        answering.join().unwrap();
        // A broken connection is an I/O error.
        let (closed, gone) = UnixStream::pair().unwrap();
        drop(gone);
        let error = request(
            &closed,
            SubagentOp::Wait {
                child: String::from("run-a"),
            },
        )
        .unwrap_err();
        assert!(matches!(error, Error::SubagentIo { .. }), "{error:?}");
    }
}
