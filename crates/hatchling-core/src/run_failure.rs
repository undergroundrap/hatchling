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
    pub const ALL: [Self; 6] = [
        Self::ModelUnavailable,
        Self::ModelIncapable,
        Self::ModelSilent,
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
}
