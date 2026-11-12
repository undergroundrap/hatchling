//! A job in the Room, as the broker asks the spawner for one and is told
//! of it.
//!
//! A job is named by whoever asks for it. Asking again by a name the
//! spawner knows starts nothing: it answers with that job as it is. So a
//! caller that lost an answer asks what became of the job, and never
//! runs it twice to find out.
//!
//! What a job is said to have done is in two parts. How long it ran,
//! whether the spawner stopped it and why, and that it was lost with its
//! Room are the spawner's own, from what it saw. How its first process
//! ended and what it wrote come from the supervisor confined in the Room:
//! checked for sense, and untrusted.

use std::time::Duration;

use hatchling_core::room::{Ask, NotStarted, WORKSPACE, check_argv, check_dir};
pub use hatchling_core::room::{Captured, End, Ended, JobId, STDIN_BYTES, Stream};
use serde::{Deserialize, Serialize};

/// How long a job may run when its caller names no limit, in seconds.
pub const JOB_DEFAULT_SECS: u32 = 120;

/// The longest a job may be allowed to run, in seconds.
pub const JOB_CEILING_SECS: u32 = 600;

/// A command to run in the Room, as a job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomRun {
    /// The job's name, chosen by the caller.
    pub job: JobId,
    /// The program, named plainly or by an absolute path, and its
    /// arguments. No shell reads them.
    pub argv: Vec<String>,
    /// The directory it starts in: the workspace when none is named.
    pub dir: Option<String>,
    /// What it reads on its standard input: at most [`STDIN_BYTES`].
    #[serde(with = "hatchling_core::room::b64")]
    pub stdin: Vec<u8>,
    /// How long it may run, in seconds: [`JOB_DEFAULT_SECS`] when no
    /// limit is named, and never more than [`JOB_CEILING_SECS`]. A job
    /// still running then is stopped.
    pub limit_secs: Option<u32>,
    /// How long the caller waits for it to end, in seconds. A job still
    /// running then is answered as running, and runs on to its limit.
    pub wait_secs: u32,
}

impl RoomRun {
    /// `argv` as the job `job`, in the workspace, with no input, the
    /// ordinary limit, and a caller that waits for it to end.
    #[must_use]
    pub const fn new(job: JobId, argv: Vec<String>) -> Self {
        Self {
            job,
            argv,
            dir: None,
            stdin: Vec::new(),
            limit_secs: None,
            wait_secs: JOB_CEILING_SECS,
        }
    }
}

/// A [`RoomRun`] that may be asked of the Room: where it starts, and its
/// two waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checked {
    /// The directory it starts in.
    pub(crate) dir: String,
    /// How long it may run.
    pub(crate) limit: Duration,
    /// How long its caller waits: never longer than it may run.
    pub(crate) wait: Duration,
}

/// Whether `run` may be asked of the Room, and with what.
///
/// # Errors
///
/// Why not, in words for whoever asked.
pub(crate) fn check(run: &RoomRun) -> Result<Checked, String> {
    check_argv(&run.argv)?;
    let dir = run.dir.clone().unwrap_or_else(|| String::from(WORKSPACE));
    check_dir(&dir)?;
    if run.stdin.len() > STDIN_BYTES {
        return Err(format!(
            "a Room job's input is at most {STDIN_BYTES} bytes: put more in a file in the workspace"
        ));
    }
    let limit = run.limit_secs.unwrap_or(JOB_DEFAULT_SECS);
    if limit == 0 || limit > JOB_CEILING_SECS {
        return Err(format!(
            "a Room job may run for 1 to {JOB_CEILING_SECS} seconds"
        ));
    }
    // The longest nonce the spawner sends is 64 characters.
    let start = Ask::Start {
        nonce: "f".repeat(64),
        job: run.job.clone(),
        argv: run.argv.clone(),
        dir: dir.clone(),
        stdin: run.stdin.clone(),
    };
    if !hatchling_core::fits_one_frame(&start) {
        return Err(String::from(
            "a Room command with its input is too long to send",
        ));
    }
    Ok(Checked {
        dir,
        limit: Duration::from_secs(u64::from(limit)),
        wait: Duration::from_secs(u64::from(run.wait_secs.min(limit))),
    })
}

/// Why the supervisor didn't start a job, in words for whoever asked.
pub(crate) const fn not_started(why: NotStarted) -> &'static str {
    match why {
        NotStarted::Known => "the Room has a job by that name already",
        NotStarted::Full => "the Room is running as many jobs as it may",
        NotStarted::NoSuchDir => "the Room has no such directory",
        NotStarted::NoSuchProgram => "the Room couldn't start that program",
        NotStarted::NoSession => {
            "the Room couldn't prepare the job: its session directory may be full"
        }
    }
}

/// Why the spawner stopped a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoomJobStopped {
    /// It was still running at its limit.
    Limit,
    /// Its stop was asked for.
    Asked,
}

/// How a job is, or how it came out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "is", rename_all = "kebab-case", deny_unknown_fields)]
pub enum RoomJobState {
    /// It is running, and has been for this long.
    Running {
        /// Milliseconds since it was asked for.
        ran_ms: u64,
    },
    /// It ended.
    Ended {
        /// How its first process ended, as the supervisor says.
        how: Ended,
        /// Why the spawner stopped it, if it did.
        stopped: Option<RoomJobStopped>,
        /// Milliseconds from when it was asked for to when its end was
        /// said, by the spawner's clock.
        ran_ms: u64,
        /// What was kept of its standard output, as the supervisor says.
        stdout: Captured,
        /// What was kept of its standard error, as the supervisor says.
        stderr: Captured,
    },
    /// Its Room ended before it did, and it with it. What it wrote went
    /// with the Room's session directory; what it had written to the
    /// workspace by then is there, and may be half done.
    Lost {
        /// Why the Room ended.
        why: String,
    },
}

/// A job in the Room.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomJob {
    /// Its name.
    pub job: JobId,
    /// How it is.
    pub state: RoomJobState,
}

/// What a caller that asked for a job is answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomJobAnswer {
    /// Whether this ask started the job. `false` when the spawner knew
    /// the name already: nothing was run again.
    pub started: bool,
    /// The job, as it is.
    pub job: RoomJob,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn job(name: &str) -> JobId {
        name.parse().unwrap()
    }

    pub(crate) fn run(name: &str, argv: &[&str]) -> RoomRun {
        RoomRun::new(
            job(name),
            argv.iter().map(|part| (*part).to_owned()).collect(),
        )
    }

    #[test]
    fn a_run_starts_in_the_workspace_with_the_ordinary_limit() {
        let plain = run("j", &["python3", "-c", "print(1)"]);
        assert_eq!(
            check(&plain),
            Ok(Checked {
                dir: String::from("/workspace"),
                limit: Duration::from_secs(120),
                wait: Duration::from_secs(120),
            })
        );
        let own = RoomRun {
            dir: Some(String::from("/session")),
            limit_secs: Some(600),
            wait_secs: 5,
            ..plain.clone()
        };
        assert_eq!(
            check(&own),
            Ok(Checked {
                dir: String::from("/session"),
                limit: Duration::from_secs(600),
                wait: Duration::from_secs(5),
            })
        );
        // A caller never waits longer than the job may run.
        let short = RoomRun {
            limit_secs: Some(1),
            wait_secs: 30,
            ..plain.clone()
        };
        assert_eq!(check(&short).unwrap().wait, Duration::from_secs(1));
        let no_wait = RoomRun {
            wait_secs: 0,
            ..plain
        };
        assert_eq!(check(&no_wait).unwrap().wait, Duration::ZERO);
    }

    #[test]
    fn a_run_that_may_not_be_asked_says_why() {
        let plain = run("j", &["true"]);
        let cases = [
            (run("j", &["./script"]), "interpreter"),
            (
                RoomRun {
                    dir: Some(String::from("workspace")),
                    ..plain.clone()
                },
                "absolute",
            ),
            (
                RoomRun {
                    stdin: vec![0; STDIN_BYTES + 1],
                    ..plain.clone()
                },
                "input is at most",
            ),
            (
                RoomRun {
                    limit_secs: Some(0),
                    ..plain.clone()
                },
                "1 to 600 seconds",
            ),
            (
                RoomRun {
                    limit_secs: Some(601),
                    ..plain.clone()
                },
                "1 to 600 seconds",
            ),
        ];
        for (refused, why) in cases {
            let said = check(&refused).unwrap_err();
            assert!(said.contains(why), "{said}");
        }
        let most = RoomRun {
            stdin: vec![0; STDIN_BYTES],
            limit_secs: Some(600),
            ..plain
        };
        assert!(check(&most).is_ok());
        // Arguments that take six bytes each on the wire: within their
        // own bound, and with the most input too long for one frame.
        let escaped = RoomRun {
            argv: vec![String::from("true"), "\u{1}".repeat(7000)],
            ..most
        };
        assert!(check(&escaped).unwrap_err().contains("too long to send"));
    }

    #[test]
    fn every_reason_a_job_was_not_started_has_its_words() {
        let words = [
            NotStarted::Known,
            NotStarted::Full,
            NotStarted::NoSuchDir,
            NotStarted::NoSuchProgram,
            NotStarted::NoSession,
        ]
        .map(not_started);
        assert!(words.iter().all(|said| said.starts_with("the Room ")));
        let mut distinct = words.to_vec();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), words.len());
        assert!(not_started(NotStarted::NoSuchDir).contains("directory"));
        assert!(not_started(NotStarted::NoSuchProgram).contains("program"));
        assert!(not_started(NotStarted::Known).contains("name"));
        assert!(not_started(NotStarted::Full).contains("as many"));
        assert!(not_started(NotStarted::NoSession).contains("prepare"));
    }

    #[test]
    fn a_job_is_told_on_the_wire_as_written() {
        let ended = RoomJob {
            job: job("j"),
            state: RoomJobState::Ended {
                how: Ended::Exited(2),
                stopped: Some(RoomJobStopped::Limit),
                ran_ms: 1500,
                stdout: Captured::default(),
                stderr: Captured::default(),
            },
        };
        let wire = concat!(
            r#"{"job":"j","state":{"is":"ended","how":{"exited":2},"stopped":"limit","ran_ms":1500,"#,
            r#""stdout":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false},"#,
            r#""stderr":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false}}}"#
        );
        assert_eq!(serde_json::to_string(&ended).unwrap(), wire);
        assert_eq!(serde_json::from_str::<RoomJob>(wire).unwrap(), ended);
        let running = r#"{"job":"j","state":{"is":"running","ran_ms":7}}"#;
        assert_eq!(
            serde_json::from_str::<RoomJob>(running).unwrap().state,
            RoomJobState::Running { ran_ms: 7 }
        );
        let lost = r#"{"job":"j","state":{"is":"lost","why":"the Room session was idle"}}"#;
        assert!(matches!(
            serde_json::from_str::<RoomJob>(lost).unwrap().state,
            RoomJobState::Lost { .. }
        ));
        assert_eq!(
            serde_json::to_string(&RoomJobStopped::Asked).unwrap(),
            r#""asked""#
        );
        let request = run("j", &["true"]);
        let wire = r#"{"job":"j","argv":["true"],"dir":null,"stdin":"","limit_secs":null,"wait_secs":600}"#;
        assert_eq!(serde_json::to_string(&request).unwrap(), wire);
        assert_eq!(serde_json::from_str::<RoomRun>(wire).unwrap(), request);
        let more = r#"{"job":"j","argv":["true"],"dir":null,"stdin":"","limit_secs":null,"wait_secs":600,"shell":true}"#;
        assert!(serde_json::from_str::<RoomRun>(more).is_err());
    }
}
