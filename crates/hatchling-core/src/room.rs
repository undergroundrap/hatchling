//! The Room's wire: what the spawner asks the supervisor confined in the
//! Room, and what the supervisor says back (0.9).
//!
//! A command in the Room is a job. Whoever asks for one names it
//! ([`JobId`]), so a caller that lost an answer can ask what became of the
//! job by its name, and never has to run it again to find out.
//!
//! The spawner asks one thing at a time ([`Ask`]) and the supervisor
//! answers it ([`Say`]), echoing the ask's nonce. One thing is said
//! unasked: that a job has ended ([`Say::Ended`]). What a job wrote is kept
//! as a head and a tail of each stream ([`PIECE_BYTES`] each), with the
//! count of what fell between them, and the whole of it, up to
//! [`SPILL_BYTES`], in a file in the Room's session directory
//! ([`spill_path`]). The pieces are asked for one at a time
//! ([`Ask::Piece`]), so that every frame stays small. The supervisor
//! remembers only so many ended jobs: the oldest goes, with its files,
//! when another ends.
//!
//! The supervisor runs in the Room, as the Room's user: everything it says
//! is untrusted. [`Captured::check`] and [`Ended::check`] are what the
//! spawner holds its words to; unknown fields never decode.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The longest a job's name may be, in bytes.
pub const JOB_ID_MAX_LEN: usize = 64;

/// How much of the start of a stream, and of its end, is kept in memory
/// and handed back with a job's result.
pub const PIECE_BYTES: usize = 16 * 1024;

/// How much of a stream is written to its file in the session directory.
pub const SPILL_BYTES: u64 = 8 * 1024 * 1024;

/// The most a job may be given on its standard input.
pub const STDIN_BYTES: usize = 32 * 1024;

/// The most arguments a command may have, its program among them.
pub const ARGV_MAX: usize = 64;

/// The most bytes a command's arguments may come to.
pub const ARGV_BYTES: usize = 8 * 1024;

/// The longest a job's working directory may be, in bytes.
pub const DIR_BYTES: usize = 1024;

/// How many jobs run in the Room at once.
pub const RUNNING_MAX: usize = 4;

/// Where a job starts when its caller names no directory.
pub const WORKSPACE: &str = "/workspace";

/// The Room's session directory: scratch that goes with the session.
pub const SESSION_DIR: &str = "/session";

/// Where the supervisor keeps what jobs wrote, in the Room.
pub const JOBS_DIR: &str = "/session/jobs";

/// A job's `PATH`: where its programs are looked for.
///
/// The image keeps what it adds to its distribution (Node) in
/// `/usr/local/bin`, and the distribution's own programs in the other
/// two. All three are in the read-only image: no job can put a program in
/// any of them.
pub const JOB_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// The name of a job, chosen by whoever asks for it.
///
/// 1 to [`JOB_ID_MAX_LEN`] ASCII letters, digits, `-`, `_` or `.`, and not
/// starting with a `.`: it is a file name in the session directory, and
/// can't be `.`, `..`, hidden, or a path.
///
/// Serializes as the bare string and deserializes through [`FromStr`], so
/// a name that isn't one never decodes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct JobId(String);

impl JobId {
    /// The name, as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a job's name isn't one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadJobId(&'static str);

impl fmt::Display for BadJobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for BadJobId {}

impl FromStr for JobId {
    type Err = BadJobId;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.is_empty() {
            return Err(BadJobId("a job's name is empty"));
        }
        if raw.len() > JOB_ID_MAX_LEN {
            return Err(BadJobId("a job's name is longer than 64 bytes"));
        }
        if raw.starts_with('.') {
            return Err(BadJobId("a job's name starts with a dot"));
        }
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.');
        if !raw.chars().all(plain) {
            return Err(BadJobId(
                "a job's name has something other than letters, digits, '-', '_' and '.'",
            ));
        }
        Ok(Self(raw.to_owned()))
    }
}

impl TryFrom<String> for JobId {
    type Error = BadJobId;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        raw.parse()
    }
}

impl From<JobId> for String {
    fn from(job: JobId) -> Self {
        job.0
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One of a job's two output streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl Stream {
    /// Both, in order.
    pub const BOTH: [Self; 2] = [Self::Stdout, Self::Stderr];

    /// Its name, in the wire's words and in its file's name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Which kept piece of a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum End {
    /// Its first [`PIECE_BYTES`].
    Head,
    /// Its last [`PIECE_BYTES`], of what came after the head.
    Tail,
}

/// The file, in the Room, that holds what `job` wrote to `stream`. The
/// name is made here from the job's name alone: nothing the supervisor
/// says chooses it.
#[must_use]
pub fn spill_path(job: &JobId, stream: Stream) -> String {
    format!("{JOBS_DIR}/{job}.{}", stream.as_str())
}

/// Whether `argv` is a command the Room may be asked to run: a program
/// named plainly (found on the Room's fixed path) or by an absolute path,
/// and arguments within [`ARGV_MAX`] and [`ARGV_BYTES`].
///
/// # Errors
///
/// Why it isn't, in words for whoever asked.
pub fn check_argv(argv: &[String]) -> Result<(), &'static str> {
    let Some(program) = argv.first() else {
        return Err("a Room command needs a program");
    };
    if program.is_empty() {
        return Err("a Room command needs a program");
    }
    if program.contains('/') && !program.starts_with('/') {
        return Err(
            "a Room program is named plainly or by an absolute path: files in the workspace don't run, so a script is run by naming its interpreter",
        );
    }
    if argv.len() > ARGV_MAX
        || argv.iter().map(String::len).sum::<usize>() > ARGV_BYTES
        || argv.iter().any(|argument| argument.contains('\0'))
    {
        return Err("a Room command's arguments are invalid or too long");
    }
    Ok(())
}

/// Whether `dir` may be asked for as a job's working directory: an
/// absolute path of at most [`DIR_BYTES`]. The Room decides whether it
/// is there.
///
/// # Errors
///
/// Why it isn't.
pub fn check_dir(dir: &str) -> Result<(), &'static str> {
    if !dir.starts_with('/') {
        return Err("a Room job's directory is an absolute path");
    }
    if dir.len() > DIR_BYTES || dir.contains('\0') {
        return Err("a Room job's directory is invalid or too long");
    }
    Ok(())
}

/// What the spawner asks the supervisor. Each carries a nonce the answer
/// must echo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Ask {
    /// Start `job`: `argv`, in `dir`, with `stdin` on its standard input.
    Start {
        /// Echoed by the answer.
        nonce: String,
        /// The job's name.
        job: JobId,
        /// The program and its arguments.
        argv: Vec<String>,
        /// The directory it starts in.
        dir: String,
        /// What it reads on its standard input.
        #[serde(with = "b64")]
        stdin: Vec<u8>,
    },
    /// Stop `job`: every process in its group is killed. Its end is said
    /// as any job's is.
    Stop {
        /// Echoed by the answer.
        nonce: String,
        /// The job's name.
        job: JobId,
    },
    /// Hand back a kept piece of what an ended `job` wrote.
    Piece {
        /// Echoed by the answer.
        nonce: String,
        /// The job's name.
        job: JobId,
        /// Which stream.
        stream: Stream,
        /// Which piece of it.
        end: End,
    },
}

impl Ask {
    /// The nonce the answer must echo.
    #[must_use]
    pub fn nonce(&self) -> &str {
        match self {
            Self::Start { nonce, .. } | Self::Stop { nonce, .. } | Self::Piece { nonce, .. } => {
                nonce
            }
        }
    }
}

/// Why the supervisor didn't start a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotStarted {
    /// It has a job by that name already.
    Known,
    /// It is running as many jobs as it may.
    Full,
    /// The directory isn't there, or isn't one the Room's user may enter.
    NoSuchDir,
    /// The program couldn't be started.
    NoSuchProgram,
    /// It couldn't prepare the job: what it was given to read couldn't
    /// be put in the session directory, or its first process couldn't
    /// be watched.
    NoSession,
}

/// What the supervisor says. All but [`Say::Ended`] answer an [`Ask`],
/// and echo its nonce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "say", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Say {
    /// The job was started.
    Started {
        /// The ask's nonce.
        nonce: String,
    },
    /// The job wasn't started.
    NotStarted {
        /// The ask's nonce.
        nonce: String,
        /// Why not.
        why: NotStarted,
    },
    /// The job's processes were sent the kill; its end follows.
    Stopping {
        /// The ask's nonce.
        nonce: String,
    },
    /// There's no such job, or it isn't in the state the ask needs: a
    /// stop of one that has ended, a piece of one still running.
    NoJob {
        /// The ask's nonce.
        nonce: String,
    },
    /// A kept piece of what a job wrote.
    Piece {
        /// The ask's nonce.
        nonce: String,
        /// The piece.
        #[serde(with = "b64")]
        bytes: Vec<u8>,
    },
    /// A job has ended. Said once for each job that was started, unasked.
    Ended {
        /// The job's name.
        job: JobId,
        /// How its first process ended.
        how: Ended,
        /// What it wrote to standard output.
        stdout: Captured,
        /// What it wrote to standard error.
        stderr: Captured,
    },
}

impl Say {
    /// The nonce this echoes, when it answers an ask.
    #[must_use]
    pub fn nonce(&self) -> Option<&str> {
        match self {
            Self::Started { nonce }
            | Self::NotStarted { nonce, .. }
            | Self::Stopping { nonce }
            | Self::NoJob { nonce }
            | Self::Piece { nonce, .. } => Some(nonce),
            Self::Ended { .. } => None,
        }
    }
}

/// How a job's first process ended, as the supervisor says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ended {
    /// It returned this exit code.
    Exited(u8),
    /// It was ended by this signal.
    Signaled(u8),
}

impl Ended {
    /// Whether this is an end a process can have: a signal is 1 to 64.
    ///
    /// # Errors
    ///
    /// What is wrong with it.
    pub const fn check(self) -> Result<(), &'static str> {
        match self {
            Self::Exited(_) | Self::Signaled(1..=64) => Ok(()),
            Self::Signaled(_) => Err("a job ended by a signal that isn't one"),
        }
    }
}

/// How much of a stream was kept, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Captured {
    /// Bytes kept from its start.
    pub head: u32,
    /// Bytes kept from its end.
    pub tail: u32,
    /// Bytes between the two that weren't kept in memory.
    pub dropped: u64,
    /// Bytes of it written to its file.
    pub spilled: u64,
    /// Whether its file holds less than all of it.
    pub spill_cut: bool,
}

impl Captured {
    /// All the stream came to, in bytes.
    #[must_use]
    pub fn total(&self) -> u64 {
        u64::from(self.head)
            .saturating_add(self.dropped)
            .saturating_add(u64::from(self.tail))
    }

    /// Whether these counts can be true together: neither piece is over
    /// [`PIECE_BYTES`]; there's a tail only past a full head, and bytes
    /// dropped only between two full pieces; the file holds no more than
    /// [`SPILL_BYTES`] nor than the stream, and all of it unless it's
    /// said to be cut.
    ///
    /// # Errors
    ///
    /// What is wrong with them.
    pub fn check(&self) -> Result<(), &'static str> {
        let piece = u32::try_from(PIECE_BYTES).unwrap_or(u32::MAX);
        if self.head > piece || self.tail > piece {
            return Err("a kept piece of a job's output is too long");
        }
        if self.tail > 0 && self.head < piece {
            return Err("a job's output has a tail without a full head");
        }
        if self.dropped > 0 && self.tail < piece {
            return Err("a job's output dropped bytes without a full tail");
        }
        let total = self.total();
        if self.spilled > SPILL_BYTES || self.spilled > total {
            return Err("a job's output file holds more than it can");
        }
        if self.spill_cut == (self.spilled == total) {
            return Err("a job's output file isn't cut as it's said to be");
        }
        Ok(())
    }
}

/// Bytes on the wire as base64 text, for `#[serde(with = ...)]`: a frame
/// is JSON, and a list of numbers is four times the bytes it stands for.
pub mod b64 {
    use serde::{Deserialize as _, Deserializer, Serializer, de};

    /// Writes `bytes` as base64 text.
    ///
    /// # Errors
    ///
    /// The serializer's own.
    pub fn serialize<S: Serializer>(bytes: &[u8], to: S) -> Result<S::Ok, S::Error> {
        to.serialize_str(&crate::secret::encode(bytes))
    }

    /// Reads bytes written as base64 text.
    ///
    /// # Errors
    ///
    /// When what is there isn't text, or isn't base64.
    pub fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(from)?;
        crate::secret::decode(&text).map_err(|_| de::Error::custom("bytes that aren't base64"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(name: &str) -> JobId {
        name.parse().unwrap()
    }

    #[test]
    fn the_bounds_are_as_they_are_written() {
        assert_eq!(JOB_ID_MAX_LEN, 64);
        assert_eq!(PIECE_BYTES, 16_384);
        assert_eq!(SPILL_BYTES, 8_388_608);
        assert_eq!(STDIN_BYTES, 32_768);
        assert_eq!(ARGV_MAX, 64);
        assert_eq!(ARGV_BYTES, 8_192);
        assert_eq!(DIR_BYTES, 1_024);
        assert_eq!(RUNNING_MAX, 4);
    }

    #[test]
    fn a_jobs_name_is_a_plain_file_name() {
        for good in ["a", "build-1", "run_2.log", "A.b-c_d", &"x".repeat(64)] {
            assert_eq!(job(good).as_str(), good);
            assert_eq!(job(good).to_string(), good);
        }
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "a/b",
            "a b",
            "a\0",
            "caf\u{e9}",
            &"x".repeat(65),
        ] {
            assert!(bad.parse::<JobId>().is_err(), "{bad:?} was taken");
        }
        // Each refusal says which rule: a caller is told what to change.
        assert_eq!(
            "".parse::<JobId>().unwrap_err().to_string(),
            "a job's name is empty"
        );
        assert!(
            "x".repeat(65)
                .parse::<JobId>()
                .unwrap_err()
                .to_string()
                .contains("longer")
        );
        assert!(
            ".x".parse::<JobId>()
                .unwrap_err()
                .to_string()
                .contains("dot")
        );
        assert!(
            "a/b"
                .parse::<JobId>()
                .unwrap_err()
                .to_string()
                .contains("letters")
        );
    }

    #[test]
    fn a_name_that_is_not_one_never_decodes() {
        assert_eq!(serde_json::to_string(&job("j-1")).unwrap(), r#""j-1""#);
        assert_eq!(
            serde_json::from_str::<JobId>(r#""j-1""#).unwrap(),
            job("j-1")
        );
        assert!(serde_json::from_str::<JobId>(r#""../x""#).is_err());
        assert_eq!(String::from(job("j-1")), "j-1");
    }

    #[test]
    fn a_streams_file_is_named_from_the_jobs_name_alone() {
        assert_eq!(
            spill_path(&job("build-1"), Stream::Stdout),
            "/session/jobs/build-1.stdout"
        );
        assert_eq!(
            spill_path(&job("build-1"), Stream::Stderr),
            "/session/jobs/build-1.stderr"
        );
        assert_eq!(Stream::BOTH, [Stream::Stdout, Stream::Stderr]);
        assert_eq!(JOBS_DIR, [SESSION_DIR, "jobs"].join("/"));
        assert_eq!(JOB_PATH, "/usr/local/bin:/usr/bin:/bin");
    }

    #[test]
    fn a_command_is_a_plain_or_absolute_program_and_bounded_arguments() {
        let argv = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>()
        };
        assert!(check_argv(&argv(&["python3", "-c", "print(1)"])).is_ok());
        assert!(check_argv(&argv(&["/usr/bin/python3"])).is_ok());
        assert!(check_argv(&[]).is_err());
        assert!(check_argv(&argv(&[""])).is_err());
        for relative in ["./run.sh", "bin/tool", "../x"] {
            assert!(
                check_argv(&argv(&[relative]))
                    .unwrap_err()
                    .contains("interpreter"),
                "{relative} was taken"
            );
        }
        let mut many = vec![String::from("/bin/true"); ARGV_MAX];
        assert!(check_argv(&many).is_ok());
        many.push(String::new());
        assert!(check_argv(&many).is_err());
        let long = format!("/{}", "x".repeat(ARGV_BYTES - 1));
        assert!(check_argv(std::slice::from_ref(&long)).is_ok());
        assert!(check_argv(&[long, String::from("x")]).is_err());
        assert!(check_argv(&argv(&["/bin/true", "\0"])).is_err());
    }

    #[test]
    fn a_directory_is_absolute_and_bounded() {
        assert!(check_dir("/workspace").is_ok());
        assert!(check_dir(WORKSPACE).is_ok());
        assert!(check_dir("workspace").is_err());
        assert!(check_dir("").is_err());
        assert!(check_dir("/a\0").is_err());
        let longest = format!("/{}", "d".repeat(DIR_BYTES - 1));
        assert!(check_dir(&longest).is_ok());
        let over = format!("{longest}d");
        assert!(check_dir(&over).is_err());
    }

    #[test]
    fn the_wire_is_as_written_and_takes_nothing_unknown() {
        let start = Ask::Start {
            nonce: String::from("n1"),
            job: job("j"),
            argv: vec![String::from("/bin/true")],
            dir: String::from("/workspace"),
            stdin: b"hi\xff".to_vec(),
        };
        let wire = r#"{"ask":"start","nonce":"n1","job":"j","argv":["/bin/true"],"dir":"/workspace","stdin":"aGn/"}"#;
        assert_eq!(serde_json::to_string(&start).unwrap(), wire);
        assert_eq!(serde_json::from_str::<Ask>(wire).unwrap(), start);
        assert_eq!(start.nonce(), "n1");
        let asks = [
            (
                Ask::Stop {
                    nonce: String::from("n2"),
                    job: job("j"),
                },
                r#"{"ask":"stop","nonce":"n2","job":"j"}"#,
                "n2",
            ),
            (
                Ask::Piece {
                    nonce: String::from("n3"),
                    job: job("j"),
                    stream: Stream::Stderr,
                    end: End::Tail,
                },
                r#"{"ask":"piece","nonce":"n3","job":"j","stream":"stderr","end":"tail"}"#,
                "n3",
            ),
        ];
        for (ask, wire, nonce) in asks {
            assert_eq!(serde_json::to_string(&ask).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Ask>(wire).unwrap(), ask);
            assert_eq!(ask.nonce(), nonce);
        }
        for refused in [
            r#"{"ask":"stop","nonce":"n","job":"j","extra":1}"#,
            r#"{"ask":"stop","nonce":"n","job":"../j"}"#,
            r#"{"ask":"run","nonce":"n","job":"j"}"#,
            r#"{"ask":"start","nonce":"n","job":"j","argv":[],"dir":"/","stdin":"not base64!"}"#,
            r#"{"ask":"piece","nonce":"n","job":"j","stream":"stdin","end":"head"}"#,
        ] {
            assert!(
                serde_json::from_str::<Ask>(refused).is_err(),
                "taken: {refused}"
            );
        }
    }

    #[test]
    fn what_the_supervisor_says_is_as_written_and_takes_nothing_unknown() {
        let nonce = || String::from("n");
        let said = [
            (
                Say::Started { nonce: nonce() },
                r#"{"say":"started","nonce":"n"}"#,
            ),
            (
                Say::NotStarted {
                    nonce: nonce(),
                    why: NotStarted::NoSuchProgram,
                },
                r#"{"say":"not-started","nonce":"n","why":"no-such-program"}"#,
            ),
            (
                Say::Stopping { nonce: nonce() },
                r#"{"say":"stopping","nonce":"n"}"#,
            ),
            (
                Say::NoJob { nonce: nonce() },
                r#"{"say":"no-job","nonce":"n"}"#,
            ),
            (
                Say::Piece {
                    nonce: nonce(),
                    bytes: vec![0, 255],
                },
                r#"{"say":"piece","nonce":"n","bytes":"AP8="}"#,
            ),
        ];
        for (say, wire) in said {
            assert_eq!(serde_json::to_string(&say).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Say>(wire).unwrap(), say);
            assert_eq!(say.nonce(), Some("n"));
        }
        let ended = Say::Ended {
            job: job("j"),
            how: Ended::Exited(3),
            stdout: Captured {
                head: 5,
                ..Captured::default()
            },
            stderr: Captured::default(),
        };
        let wire = concat!(
            r#"{"say":"ended","job":"j","how":{"exited":3},"#,
            r#""stdout":{"head":5,"tail":0,"dropped":0,"spilled":0,"spill_cut":false},"#,
            r#""stderr":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false}}"#
        );
        assert_eq!(serde_json::to_string(&ended).unwrap(), wire);
        assert_eq!(serde_json::from_str::<Say>(wire).unwrap(), ended);
        assert_eq!(ended.nonce(), None);
        for why in [
            (NotStarted::Known, r#""known""#),
            (NotStarted::Full, r#""full""#),
            (NotStarted::NoSuchDir, r#""no-such-dir""#),
            (NotStarted::NoSession, r#""no-session""#),
        ] {
            assert_eq!(serde_json::to_string(&why.0).unwrap(), why.1);
        }
        for refused in [
            r#"{"say":"started","nonce":"n","job":"j"}"#,
            r#"{"say":"ended","job":"j","how":{"exited":256},"stdout":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false},"stderr":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false}}"#,
            r#"{"say":"ended","job":"j","how":{"signaled":9},"stdout":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false,"more":1},"stderr":{"head":0,"tail":0,"dropped":0,"spilled":0,"spill_cut":false}}"#,
            r#"{"say":"piece","nonce":"n","bytes":[1,2]}"#,
            r#"{"say":"done","nonce":"n"}"#,
        ] {
            assert!(
                serde_json::from_str::<Say>(refused).is_err(),
                "taken: {refused}"
            );
        }
    }

    #[test]
    fn an_end_is_an_exit_code_or_a_real_signal() {
        assert!(Ended::Exited(0).check().is_ok());
        assert!(Ended::Exited(255).check().is_ok());
        assert!(Ended::Signaled(1).check().is_ok());
        assert!(Ended::Signaled(64).check().is_ok());
        assert!(Ended::Signaled(0).check().is_err());
        assert!(Ended::Signaled(65).check().is_err());
    }

    #[test]
    fn kept_counts_must_be_true_together() {
        let piece = u32::try_from(PIECE_BYTES).unwrap();
        let whole = |head: u32, tail: u32, dropped: u64| {
            let total = u64::from(head) + u64::from(tail) + dropped;
            Captured {
                head,
                tail,
                dropped,
                spilled: total,
                spill_cut: false,
            }
        };
        assert!(Captured::default().check().is_ok());
        assert!(whole(1, 0, 0).check().is_ok());
        assert!(whole(piece, 0, 0).check().is_ok());
        assert!(whole(piece, 1, 0).check().is_ok());
        assert!(whole(piece, piece, 0).check().is_ok());
        assert!(whole(piece, piece, 1).check().is_ok());
        assert_eq!(whole(piece, piece, 7).total(), u64::from(piece) * 2 + 7);
        // A piece over its length.
        assert!(whole(piece + 1, 0, 0).check().is_err());
        assert!(whole(piece, piece + 1, 0).check().is_err());
        // A tail without a full head, and a gap without a full tail.
        assert!(whole(piece - 1, 1, 0).check().is_err());
        assert!(whole(piece, piece - 1, 1).check().is_err());
        // The file: no more than its cap, no more than the stream, and
        // cut exactly when it's short.
        let big = whole(piece, piece, SPILL_BYTES);
        assert!(big.check().is_err(), "a file over its cap");
        let cut = Captured {
            spilled: SPILL_BYTES,
            spill_cut: true,
            ..big
        };
        assert!(cut.check().is_ok());
        assert!(
            Captured {
                spilled: SPILL_BYTES + 1,
                ..cut
            }
            .check()
            .is_err()
        );
        let small = whole(3, 0, 0);
        assert!(
            Captured {
                spilled: 4,
                ..small
            }
            .check()
            .is_err(),
            "a file longer than its stream"
        );
        assert!(
            Captured {
                spilled: 2,
                ..small
            }
            .check()
            .is_err(),
            "short, and not said to be cut"
        );
        assert!(
            Captured {
                spilled: 2,
                spill_cut: true,
                ..small
            }
            .check()
            .is_ok()
        );
        assert!(
            Captured {
                spill_cut: true,
                ..small
            }
            .check()
            .is_err(),
            "whole, and said to be cut"
        );
    }

    #[test]
    fn a_start_with_the_most_it_may_carry_fits_one_frame() {
        // The longest plain command, directory and input: what a caller
        // may ask for at most always fits.
        let start = Ask::Start {
            nonce: "f".repeat(64),
            job: job(&"j".repeat(JOB_ID_MAX_LEN)),
            argv: vec![format!("/{}", "a".repeat(ARGV_BYTES - 1))],
            dir: format!("/{}", "d".repeat(DIR_BYTES - 1)),
            stdin: vec![0xff; STDIN_BYTES],
        };
        assert!(crate::fits_one_frame(&start));
        let piece = Say::Piece {
            nonce: "f".repeat(64),
            bytes: vec![0xff; PIECE_BYTES],
        };
        assert!(crate::fits_one_frame(&piece));
    }
}
