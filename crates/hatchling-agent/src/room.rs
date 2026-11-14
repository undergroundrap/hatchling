//! Running a command in the agent's own computer, through the broker.
//!
//! The agent names a program and its arguments; the broker decides
//! everything else, and has the Room run it. The cell runs nothing: what
//! comes back is what the Room's own programs wrote, and is data.

use std::collections::BTreeSet;
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, PoisonError};

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{RoomOp, RoomResult, SessionRequest, SessionResponse};

use crate::Error;

/// The programs a run started in its computer and hasn't seen end.
///
/// Kept from the Room's own replies, so the loop can tell when the model
/// answers with one still going. A program is stopped when the reply
/// ends and nothing looks at it afterwards, so an answer that says "it's
/// still running, I'll look in a moment" promises what can't happen.
#[derive(Debug, Default)]
pub struct Running {
    jobs: Mutex<BTreeSet<String>>,
}

impl Running {
    /// None running.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            jobs: Mutex::new(BTreeSet::new()),
        }
    }

    /// Notes what a Room reply says of a program.
    pub fn saw(&self, result: &RoomResult) {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        match result {
            // One that may have started all the same counts as running
            // until it is asked after.
            RoomResult::Running { job, .. } | RoomResult::Failed { job: Some(job), .. } => {
                jobs.insert(job.clone());
            }
            RoomResult::Ended { job, .. } | RoomResult::Unknown { job } => {
                jobs.remove(job);
            }
            // The Room closed, and every program in it.
            RoomResult::Lost { .. } => jobs.clear(),
            RoomResult::Refused { .. }
            | RoomResult::Page { .. }
            | RoomResult::NoPage { .. }
            | RoomResult::Failed { job: None, .. } => {}
        }
    }

    /// What a model that answers now is told: the programs still running,
    /// and what to do about them. `None` when none is.
    #[must_use]
    pub fn unfinished(&self) -> Option<String> {
        let names: Vec<String> = self
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .collect();
        let which = match names.len() {
            0 => return None,
            1 => "a program you started is",
            _ => "programs you started are",
        };
        let names = names.join(", ");
        Some(format!(
            "You answered while {which} still running: {names}. That answer is held back. \
             Nothing happens after you answer: a program still running then is stopped, and \
             nothing looks at it again. Wait for it with run_check, or stop it with run_stop, \
             and then answer with how it came out."
        ))
    }
}

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
        | SessionResponse::Suggest(_)
        | SessionResponse::Chat(_) => Err(Error::RoomProtocol {
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

    fn running(job: &str) -> RoomResult {
        RoomResult::Running {
            job: job.to_owned(),
            ran_ms: 10_000,
        }
    }

    fn ended(job: &str) -> RoomResult {
        let text = || hatchling_core::RoomText {
            head: String::new(),
            tail: String::new(),
            left_out: 0,
            total: 0,
            file: format!("/session/jobs/{job}.stdout"),
            file_cut: false,
        };
        RoomResult::Ended {
            job: job.to_owned(),
            exit: Some(0),
            signal: None,
            stopped: None,
            ran_ms: 12,
            stdout: text(),
            stderr: text(),
        }
    }

    /// A program is running from the reply that says so until one that
    /// says it ended, and the model is told which, and what to do.
    #[test]
    fn a_program_is_running_until_its_end_is_seen() {
        let seen = Running::new();
        assert_eq!(seen.unfinished(), None);
        seen.saw(&running("j-1"));
        assert_eq!(
            seen.unfinished().as_deref(),
            Some(
                "You answered while a program you started is still running: j-1. That answer \
                 is held back. Nothing happens after you answer: a program still running then \
                 is stopped, and nothing looks at it again. Wait for it with run_check, or stop \
                 it with run_stop, and then answer with how it came out."
            )
        );
        // Asked after, and still going.
        seen.saw(&running("j-1"));
        assert!(seen.unfinished().is_some());
        seen.saw(&ended("j-1"));
        assert_eq!(seen.unfinished(), None);
        assert_eq!(Running::default().unfinished(), None);
    }

    /// Each program is its own: one ending leaves the others running, and
    /// they are named in order.
    #[test]
    fn each_running_program_is_named() {
        let seen = Running::new();
        seen.saw(&running("j-2"));
        seen.saw(&running("j-1"));
        let both = seen.unfinished().unwrap();
        assert!(
            both.starts_with(
                "You answered while programs you started are still running: j-1, j-2. That"
            ),
            "{both}"
        );
        seen.saw(&ended("j-1"));
        let one = seen.unfinished().unwrap();
        assert!(
            one.starts_with("You answered while a program you started is still running: j-2. That"),
            "{one}"
        );
        // A name this run never ran: nothing is running by it.
        seen.saw(&running("j-9"));
        seen.saw(&RoomResult::Unknown {
            job: String::from("j-9"),
        });
        assert_eq!(seen.unfinished(), Some(one));
    }

    /// A program that may have started counts until it is asked after;
    /// the Room closing ends every program; and a reply about no program
    /// changes nothing.
    #[test]
    fn what_may_have_started_counts_and_a_closed_room_runs_nothing() {
        let seen = Running::new();
        let about_none = [
            RoomResult::Refused {
                reason: String::from("no"),
            },
            RoomResult::Failed {
                reason: String::from("no"),
                job: None,
            },
            RoomResult::NoPage {
                reason: String::from("no"),
            },
            RoomResult::Page {
                path: String::from("/workspace/a"),
                from: 1,
                text: String::from("a\n"),
                more: false,
                cut: false,
                size: 2,
            },
        ];
        for reply in &about_none {
            seen.saw(reply);
            assert_eq!(seen.unfinished(), None, "{reply:?}");
        }
        seen.saw(&RoomResult::Failed {
            reason: String::from("the Room didn't answer"),
            job: Some(String::from("j-3")),
        });
        let may_run = seen.unfinished().unwrap();
        assert!(may_run.contains("still running: j-3. "), "{may_run}");
        for reply in &about_none {
            seen.saw(reply);
            assert_eq!(seen.unfinished().as_ref(), Some(&may_run), "{reply:?}");
        }
        seen.saw(&running("j-4"));
        seen.saw(&RoomResult::Lost {
            job: String::from("j-4"),
            why: String::from("it was closed"),
        });
        assert_eq!(seen.unfinished(), None, "the Room took j-3 with it");
    }
}
