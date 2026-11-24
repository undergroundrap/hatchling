//! Why a run's agent failed, by its exit status, so the broker can tell
//! the person in plain words rather than point them at a log.
//!
//! The status is the agent's word, read from the kernel: it picks one of
//! these messages and nothing else. What the agent wrote stays in the
//! broker's log.

/// A failure the person can do something about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunFailure {
    /// The model couldn't be reached, or answered with something that
    /// isn't a model's reply.
    ModelUnavailable,
    /// The model doesn't call tools: it answered the capability probe
    /// without its tool call, every time. Nothing the task asked for ran.
    ModelIncapable,
    /// The model answered, but with nothing, no text and no tool call,
    /// every time it was asked.
    ModelSilent,
    /// The model thought and then answered with nothing, every time it
    /// was asked, with its thinking turned off too where its server
    /// would take that.
    ModelThoughtOnly,
    /// The model kept calling tools it wasn't offered, or with arguments
    /// that don't fit them.
    ToolsMisused,
    /// The run used all its steps without an answer.
    OutOfSteps,
    /// The model's reply was cut off at its limit: its context window is
    /// too small, or its thinking filled it.
    OutOfRoom,
}

impl RunFailure {
    /// Every failure, in order.
    pub const ALL: [Self; 7] = [
        Self::ModelUnavailable,
        Self::ModelIncapable,
        Self::ModelSilent,
        Self::ModelThoughtOnly,
        Self::ToolsMisused,
        Self::OutOfSteps,
        Self::OutOfRoom,
    ];

    /// The agent's exit status for it: `sysexits.h` codes that don't
    /// collide with a usage error (64) or a plain failure (1).
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            // EX_UNAVAILABLE
            Self::ModelUnavailable => 69,
            // EX_NOINPUT: the probe got no tool call from it.
            Self::ModelIncapable => 66,
            // EX_DATAERR: its reply held nothing to use.
            Self::ModelSilent => 65,
            // EX_IOERR: its answer never came out of its thinking.
            Self::ModelThoughtOnly => 74,
            // EX_PROTOCOL
            Self::ToolsMisused => 76,
            // EX_TEMPFAIL
            Self::OutOfSteps => 75,
            // EX_CONFIG: the model server's window is set too small.
            Self::OutOfRoom => 78,
        }
    }

    /// The failure an agent's exit status names, if it names one.
    #[must_use]
    pub fn from_code(code: i32) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|failure| i32::from(failure.code()) == code)
    }
}

/// The status an agent exits with when it answered as its steps ran out.
///
/// That is: on its last step, told it was the last, or with an answer that
/// was held back until none were left. The answer is the run's, whole, on
/// its standard output as any answer is. It is no failure, and
/// [`RunFailure`] has no word for it: the broker takes the answer as it
/// takes any, and knows the run may have left work undone. It is the
/// agent's word, like every status here.
pub const ANSWERED_OUT_OF_STEPS: u8 = 80;

/// The status an agent exits with when it answered as its context filled.
///
/// Its next request wouldn't have fit the model's window, so it was told
/// to answer with no tools (WO-051). As with [`ANSWERED_OUT_OF_STEPS`],
/// the answer is the run's, and no failure.
pub const ANSWERED_OUT_OF_ROOM: u8 = 81;

/// Whether an agent's exit status says it answered as its steps ran out.
#[must_use]
pub fn answered_out_of_steps(code: i32) -> bool {
    code == i32::from(ANSWERED_OUT_OF_STEPS)
}

/// Whether an agent's exit status says it answered as one of its limits
/// came, its steps ([`ANSWERED_OUT_OF_STEPS`]) or its room
/// ([`ANSWERED_OUT_OF_ROOM`]): its answer may leave work undone.
#[must_use]
pub fn answered_at_a_limit(code: i32) -> bool {
    answered_out_of_steps(code) || code == i32::from(ANSWERED_OUT_OF_ROOM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_failure_has_its_own_code_and_reads_back() {
        for failure in RunFailure::ALL {
            assert_eq!(RunFailure::from_code(failure.code().into()), Some(failure));
            assert!(![0, 1, 64].contains(&failure.code()), "{failure:?}");
        }
        let codes: std::collections::BTreeSet<u8> =
            RunFailure::ALL.into_iter().map(RunFailure::code).collect();
        assert_eq!(codes.len(), RunFailure::ALL.len());
        for other in [-1, 0, 1, 64, 70, 255] {
            assert_eq!(RunFailure::from_code(other), None, "{other}");
        }
    }

    /// Answering as the steps ran out has a status of its own: no
    /// failure's, and none a run exits with for another reason.
    #[test]
    fn answering_as_the_steps_ran_out_has_a_status_of_its_own() {
        assert_eq!(ANSWERED_OUT_OF_STEPS, 80);
        assert!(answered_out_of_steps(ANSWERED_OUT_OF_STEPS.into()));
        assert_eq!(RunFailure::from_code(ANSWERED_OUT_OF_STEPS.into()), None);
        for other in [-1, 0, 1, 64, 79, 81, 255] {
            assert!(!answered_out_of_steps(other), "{other}");
        }
        for failure in RunFailure::ALL {
            assert!(!answered_out_of_steps(failure.code().into()), "{failure:?}");
        }
    }

    /// Answering as the room ran out has a status of its own too, and
    /// both are answers at a limit; nothing else is.
    #[test]
    fn answering_at_a_limit_is_either_status_and_nothing_else() {
        assert_eq!(ANSWERED_OUT_OF_ROOM, 81);
        assert_eq!(RunFailure::from_code(ANSWERED_OUT_OF_ROOM.into()), None);
        assert!(answered_at_a_limit(ANSWERED_OUT_OF_STEPS.into()));
        assert!(answered_at_a_limit(ANSWERED_OUT_OF_ROOM.into()));
        assert!(!answered_out_of_steps(ANSWERED_OUT_OF_ROOM.into()));
        for other in [-1, 0, 1, 64, 79, 82, 255] {
            assert!(!answered_at_a_limit(other), "{other}");
        }
        for failure in RunFailure::ALL {
            assert!(!answered_at_a_limit(failure.code().into()), "{failure:?}");
        }
    }
}
