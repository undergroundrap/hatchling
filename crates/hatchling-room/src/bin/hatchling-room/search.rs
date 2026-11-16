//! A search in the Room: the Room's own search program, run by the
//! supervisor, for the lines under a path that match a pattern.
//!
//! A search is no job. It has no name, nothing it writes is kept, and it
//! is over before the supervisor answers: its program is started, what it
//! prints is read for no longer than [`SEARCH_MILLIS`] and no further
//! than [`FOUND_BYTES`], and whatever is left of it is then killed and
//! waited for. The one thread there is waits with it, so a search is
//! short on purpose.
//!
//! The program is told everything by this file and nothing by its
//! caller's words: the pattern, the glob and the path each go as one
//! argument that can't be read as an option, and every other argument is
//! fixed. It starts with nothing of the supervisor's environment, like a
//! job, and is confined as every process in the Room is.

use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hatchling_core::room::{FOUND_BYTES, FOUND_LINE_BYTES, Found, JOB_PATH, NoSearch};
use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group};

/// The Room's search program, where its image keeps it.
pub(crate) const SEARCH_PROGRAM: &str = "/usr/bin/rg";

/// The largest file the search program looks in. A larger one is passed
/// over: it would take the search's time from every other file.
const SEARCH_FILE_BYTES: &str = "8M";

/// What one search asks for.
pub(crate) struct Wanted<'a> {
    /// What to look for.
    pub(crate) pattern: &'a str,
    /// The file or directory to look in.
    pub(crate) path: &'a str,
    /// Only files whose names match this are looked in.
    pub(crate) glob: Option<&'a str>,
    /// Whether letters match whatever their case.
    pub(crate) ignore_case: bool,
    /// Whether the pattern is the very words to find.
    pub(crate) literal: bool,
}

/// The search program's arguments for `wanted`.
///
/// Each of the caller's three words is the value of an option written
/// with `=`, or follows `--`: none can be taken for an option. Lines are
/// printed as `path:line:text`, in the order of their paths, one thread
/// looking. Every file is looked in, hidden or ignored by a project's own
/// settings or not, but a repository's own records (`.git`), files over
/// [`SEARCH_FILE_BYTES`], and files that aren't text. No link is
/// followed, and no settings file is read.
pub(crate) fn arguments(wanted: &Wanted<'_>) -> Vec<String> {
    let mut arguments: Vec<String> = [
        "--no-config",
        "--color=never",
        "--no-heading",
        "--with-filename",
        "--line-number",
        "--sort=path",
        "--hidden",
        "--no-ignore",
        "--glob=!.git",
        "--max-columns-preview",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    arguments.push(format!("--max-columns={FOUND_LINE_BYTES}"));
    arguments.push(format!("--max-filesize={SEARCH_FILE_BYTES}"));
    if wanted.ignore_case {
        arguments.push(String::from("--ignore-case"));
    }
    if wanted.literal {
        arguments.push(String::from("--fixed-strings"));
    }
    if let Some(glob) = wanted.glob {
        arguments.push(format!("--glob={glob}"));
    }
    arguments.push(format!("--regexp={}", wanted.pattern));
    arguments.push(String::from("--"));
    arguments.push(wanted.path.to_owned());
    arguments
}

/// What `program`, started with `arguments`, found within `limit`.
///
/// Whatever it printed is kept to [`FOUND_BYTES`] in whole lines. It is
/// said to have more when it was stopped, by its time or by having
/// printed more than fits. One that printed nothing and didn't end as a
/// search with nothing found does wouldn't take what it was asked.
pub(crate) fn found(
    program: &Path,
    arguments: &[String],
    limit: Duration,
) -> Result<Found, NoSearch> {
    let mut child = Command::new(program)
        .args(arguments)
        .current_dir("/")
        .env_clear()
        .env("PATH", JOB_PATH)
        .env("LANG", "C.UTF-8")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| NoSearch::Failed)?;
    let printed = child.stdout.take().map(|pipe| read_until(pipe, limit));
    // Before it is reaped its pid is its own, and names its group: what
    // is left of it, having been stopped or not, goes here.
    let _ = kill_process_group(Pid::from_child(&child), Signal::Kill);
    let status = child.wait().map_err(|_| NoSearch::Failed)?;
    let Some((bytes, stopped)) = printed else {
        return Err(NoSearch::Failed);
    };
    // A search that found something ends with 0, and one that found
    // nothing with 1.
    if !stopped && bytes.is_empty() && !matches!(status.code(), Some(0 | 1)) {
        return Err(NoSearch::Refused);
    }
    Ok(whole_lines(&bytes, stopped))
}

/// Reads `pipe` to its end, or until `limit` has passed or more than
/// [`FOUND_BYTES`] has been read: what was read, and whether the reading
/// was stopped short of the end.
fn read_until(
    mut pipe: impl std::io::Read + std::os::fd::AsFd,
    limit: Duration,
) -> (Vec<u8>, bool) {
    let until = Instant::now() + limit;
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if bytes.len() > FOUND_BYTES {
            return (bytes, true);
        }
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return (bytes, true);
        }
        let timeout = i32::try_from(left.as_millis())
            .unwrap_or(i32::MAX)
            .saturating_add(1);
        let ready = {
            let mut fds = [PollFd::new(&pipe, PollFlags::IN)];
            match poll(&mut fds, timeout) {
                Ok(ready) => ready > 0,
                Err(Errno::INTR) => false,
                Err(_) => return (bytes, true),
            }
        };
        if !ready {
            continue;
        }
        match pipe.read(&mut buffer) {
            Ok(0) => return (bytes, false),
            Ok(read) => bytes.extend_from_slice(buffer.get(..read).unwrap_or_default()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return (bytes, true),
        }
    }
}

/// The whole lines of `bytes` that fit [`FOUND_BYTES`], and whether there
/// was more: the reading was `stopped`, or something of it was left out.
fn whole_lines(bytes: &[u8], stopped: bool) -> Found {
    let fits = bytes.get(..FOUND_BYTES).unwrap_or(bytes);
    let end = fits
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    Found {
        bytes: fits.get(..end).unwrap_or_default().to_vec(),
        more: stopped || end < bytes.len(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use hatchling_core::room::SEARCH_MILLIS;

    use super::*;

    /// Writes the shell script `script` as a program at `path`, by way of
    /// a child.
    ///
    /// The file is never open for writing in this process. Were it, a
    /// test on another thread that started a program just then would
    /// carry the open file into its child for a moment, and running the
    /// script in that moment fails: the kernel won't run a file something
    /// has open for writing.
    pub(crate) fn program_at(path: &Path, script: &str) {
        use std::io::Write as _;
        let mut writer = Command::new("/bin/sh")
            .args(["-c", "umask 077; cat > \"$1\" && chmod 700 \"$1\"", "sh"])
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let text = format!("#!/bin/sh\n{script}\n");
        writer
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        assert!(
            writer.wait().unwrap().success(),
            "the stand-in wasn't written"
        );
    }

    /// A stand-in for the search program: a shell script that does what a
    /// test says, in a directory of its own.
    fn stand_in(name: &str, script: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-room-search-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("search");
        program_at(&path, script);
        path
    }

    fn tidy(program: &Path) {
        let _ = std::fs::remove_dir_all(program.parent().unwrap());
    }

    const fn wanted<'a>(pattern: &'a str, path: &'a str) -> Wanted<'a> {
        Wanted {
            pattern,
            path,
            glob: None,
            ignore_case: false,
            literal: false,
        }
    }

    /// The program is told what this file says and no more. The caller's
    /// three words each go where they can't be read as an option, however
    /// they begin.
    #[test]
    fn the_search_program_is_told_only_what_is_fixed_here() {
        assert_eq!(SEARCH_PROGRAM, "/usr/bin/rg");
        let plain = arguments(&wanted("TODO", "/workspace"));
        assert_eq!(
            plain,
            [
                "--no-config",
                "--color=never",
                "--no-heading",
                "--with-filename",
                "--line-number",
                "--sort=path",
                "--hidden",
                "--no-ignore",
                "--glob=!.git",
                "--max-columns-preview",
                "--max-columns=400",
                "--max-filesize=8M",
                "--regexp=TODO",
                "--",
                "/workspace",
            ]
        );
        let every = arguments(&Wanted {
            pattern: "--pre=/bin/sh",
            path: "--files",
            glob: Some("--pre=x"),
            ignore_case: true,
            literal: true,
        });
        let tail: Vec<&str> = every.iter().skip(12).map(String::as_str).collect();
        assert_eq!(
            tail,
            [
                "--ignore-case",
                "--fixed-strings",
                "--glob=--pre=x",
                "--regexp=--pre=/bin/sh",
                "--",
                "--files",
            ]
        );
        assert_eq!(every.get(..12), plain.get(..12));
    }

    /// What the program printed is handed back as it is, with nothing
    /// said to be left; one that found nothing hands back nothing.
    #[test]
    fn what_was_found_is_handed_back_as_it_was_printed() {
        let program = stand_in("found", "printf '/w/a:1:one\\n/w/b:2:two\\n'");
        assert_eq!(
            found(&program, &[], Duration::from_secs(5)),
            Ok(Found {
                bytes: b"/w/a:1:one\n/w/b:2:two\n".to_vec(),
                more: false,
            })
        );
        tidy(&program);
        let nothing = stand_in("nothing", "exit 1");
        assert_eq!(
            found(&nothing, &[], Duration::from_secs(5)),
            Ok(Found::default())
        );
        tidy(&nothing);
        // Found, with something it couldn't read on the way: still found.
        let partly = stand_in("partly", "printf '/w/a:1:one\\n'; exit 2");
        assert_eq!(
            found(&partly, &[], Duration::from_secs(5)),
            Ok(Found {
                bytes: b"/w/a:1:one\n".to_vec(),
                more: false,
            })
        );
        tidy(&partly);
    }

    /// The program is given the arguments, and nothing of the
    /// supervisor's environment.
    #[test]
    fn the_program_is_given_its_arguments_and_a_bare_environment() {
        let program = stand_in(
            "given",
            "printf '%s|' \"$@\"; printf '\\n'; env | sort | tr '\\n' ' '; printf '\\n'; pwd",
        );
        let given = found(
            &program,
            &[String::from("--regexp=a b"), String::from("--")],
            Duration::from_secs(5),
        )
        .unwrap();
        let text = String::from_utf8(given.bytes).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("--regexp=a b|--|"));
        let environment = lines.next().unwrap();
        assert!(
            environment.contains("LANG=C.UTF-8")
                && environment.contains(&format!("PATH={JOB_PATH}")),
            "{environment}"
        );
        assert!(
            !environment.contains("HOME=") && !environment.contains("CARGO"),
            "{environment}"
        );
        assert_eq!(lines.next(), Some("/"));
        tidy(&program);
    }

    /// A program that prints nothing and ends as no search does, and one
    /// that can't be started, hand back nothing, each said as it is.
    #[test]
    fn a_search_that_was_not_one_is_said_as_that() {
        let refused = stand_in("refused", "exit 2");
        assert_eq!(
            found(&refused, &[], Duration::from_secs(5)),
            Err(NoSearch::Refused)
        );
        tidy(&refused);
        let killed = stand_in("killed", "kill -9 $$");
        assert_eq!(
            found(&killed, &[], Duration::from_secs(5)),
            Err(NoSearch::Refused)
        );
        tidy(&killed);
        assert_eq!(
            found(
                Path::new("/no/such/hatchling-search"),
                &[],
                Duration::from_secs(5)
            ),
            Err(NoSearch::Failed)
        );
    }

    /// A search that prints more than fits is stopped, and hands back the
    /// whole lines that fit, with more said to be left. It doesn't wait
    /// for the program to finish printing.
    #[test]
    fn more_than_fits_is_cut_at_a_line_and_the_program_stopped() {
        let program = stand_in(
            "flood",
            "while :; do printf '/w/file:1:%s\\n' xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done",
        );
        let began = Instant::now();
        let flood = found(&program, &[], Duration::from_secs(10)).unwrap();
        assert!(began.elapsed() < Duration::from_secs(5), "it was stopped");
        assert!(flood.more);
        assert!(flood.check().is_ok(), "{}", flood.bytes.len());
        // As many whole lines as fit, and not one fewer.
        let line = "/w/file:1:xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n".len();
        assert_eq!(flood.bytes.len(), (FOUND_BYTES / line) * line);
        assert!(flood.bytes.ends_with(b"xxxx\n"));
        tidy(&program);
    }

    /// A search still looking when its time is up is stopped, and what it
    /// had found by then is handed back with more said to be left.
    #[test]
    fn a_search_out_of_time_is_stopped_with_what_it_had() {
        let program = stand_in("slow", "printf '/w/a:1:one\\n'; exec sleep 20");
        let began = Instant::now();
        let slow = found(&program, &[], Duration::from_millis(300)).unwrap();
        let took = began.elapsed();
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_secs(5),
            "{took:?}"
        );
        assert_eq!(
            slow,
            Found {
                bytes: b"/w/a:1:one\n".to_vec(),
                more: true,
            }
        );
        tidy(&program);
        // With nothing found in its time, nothing is handed back, and
        // more is still said to be left.
        let silent = stand_in("silent", "exec sleep 20");
        assert_eq!(
            found(&silent, &[], Duration::from_millis(200)),
            Ok(Found {
                bytes: Vec::new(),
                more: true,
            })
        );
        tidy(&silent);
        assert_eq!(SEARCH_MILLIS, 5_000);
    }

    /// Whatever the program started is ended with it: a search leaves
    /// nothing running in the Room.
    #[test]
    fn what_the_program_started_is_ended_with_it() {
        let program = stand_in(
            "group",
            "sleep 20 & echo $! > \"$0.pid\"; printf '/w/a:1:one\\n'; wait",
        );
        let stopped = found(&program, &[], Duration::from_millis(300)).unwrap();
        assert_eq!(stopped.bytes, b"/w/a:1:one\n");
        assert!(stopped.more);
        let pid = std::fs::read_to_string(program.with_extension("pid")).unwrap();
        let process = PathBuf::from(format!("/proc/{}", pid.trim()));
        let until = Instant::now() + Duration::from_secs(5);
        while process.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        let left = process.exists();
        if left {
            // Not left behind for the tests that follow.
            let _ = Command::new("kill").args(["-9", pid.trim()]).status();
        }
        assert!(!left, "what the search program started was left running");
        tidy(&program);
    }

    /// A search that printed exactly as much as fits, and ended, is whole:
    /// nothing is said to be left.
    #[test]
    fn exactly_what_fits_is_whole() {
        // 128 lines of 64 bytes are 8192.
        let program = stand_in(
            "exact",
            "i=0; while [ $i -lt 128 ]; do printf '/w/a:1:%s\\n' xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; i=$((i+1)); done",
        );
        let exact = found(&program, &[], Duration::from_secs(5)).unwrap();
        assert_eq!(exact.bytes.len(), FOUND_BYTES);
        assert!(!exact.more, "nothing was left");
        tidy(&program);
    }

    /// A pipe whose reads fail as a test says before they are let through.
    struct Faulty {
        pipe: std::os::unix::net::UnixStream,
        fails: Vec<std::io::ErrorKind>,
    }

    impl std::io::Read for Faulty {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            match self.fails.pop() {
                Some(kind) => Err(kind.into()),
                None => self.pipe.read(buffer),
            }
        }
    }

    impl std::os::fd::AsFd for Faulty {
        fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
            self.pipe.as_fd()
        }
    }

    /// A read that was only interrupted is made again, and nothing is
    /// lost. A read that fails any other way ends the reading there, at
    /// once, as stopped short.
    #[test]
    fn an_interrupted_read_is_made_again_and_a_failed_one_ends_the_reading() {
        use std::io::{ErrorKind, Write as _};
        use std::os::unix::net::UnixStream;
        let printed = |fails: Vec<ErrorKind>| {
            let (pipe, mut writer) = UnixStream::pair().unwrap();
            writer.write_all(b"/w/a:1:one\n").unwrap();
            drop(writer);
            let began = Instant::now();
            let read = read_until(Faulty { pipe, fails }, Duration::from_secs(3));
            (read, began.elapsed() < Duration::from_secs(1))
        };
        assert_eq!(
            printed(vec![ErrorKind::Interrupted, ErrorKind::Interrupted]),
            ((b"/w/a:1:one\n".to_vec(), false), true)
        );
        assert_eq!(
            printed(vec![ErrorKind::PermissionDenied; 4]),
            ((Vec::new(), true), true),
            "a failed read was made again, or wasn't taken as the end"
        );
        // One that fails every time doesn't hold the search to its limit.
        let (pipe, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"x").unwrap();
        let began = Instant::now();
        let fails = vec![ErrorKind::PermissionDenied; 100_000];
        let read = read_until(Faulty { pipe, fails }, Duration::from_secs(3));
        assert_eq!(read, (Vec::new(), true));
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "{:?}",
            began.elapsed()
        );
        drop(writer);
    }

    /// Only whole lines that fit are kept, and more is said to be left
    /// whenever anything was.
    #[test]
    fn only_whole_lines_that_fit_are_kept() {
        let kept = |bytes: &[u8], stopped: bool| {
            let found = whole_lines(bytes, stopped);
            (found.bytes.len(), found.more)
        };
        assert_eq!(kept(b"", false), (0, false));
        assert_eq!(kept(b"", true), (0, true));
        assert_eq!(kept(b"a:1:x\n", false), (6, false));
        assert_eq!(kept(b"a:1:x\n", true), (6, true));
        assert_eq!(kept(b"a:1:x\nb:2:", false), (6, true), "a line not ended");
        assert_eq!(kept(b"no line end", false), (0, true));
        let mut exact = vec![b'x'; FOUND_BYTES - 1];
        exact.push(b'\n');
        assert_eq!(
            kept(&exact, false),
            (FOUND_BYTES, false),
            "exactly what fits"
        );
        let mut over = exact.clone();
        over.extend_from_slice(b"y\n");
        assert_eq!(kept(&over, false), (FOUND_BYTES, true));
        let mut split = vec![b'x'; FOUND_BYTES - 3];
        split.extend_from_slice(b"\nabcdef\n");
        assert_eq!(
            kept(&split, false),
            (FOUND_BYTES - 2, true),
            "cut at the line before"
        );
    }
}
