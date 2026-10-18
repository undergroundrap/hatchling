//! Bounded child-process driver for dev-time tool invocations.
//!
//! Every `cargo`/`rustc`/`cargo-mutants` invocation in the dev harness
//! goes through this module:
//!
//! - the child is spawned in its own process group;
//! - stdout/stderr are drained on dedicated threads while it runs, so
//!   a chatty child can never fill the pipes and wedge the parent
//!   (pipe-fill deadlock);
//! - an absolute deadline is enforced: on expiry the whole process
//!   group is `SIGKILL`ed — cargo spawns rustc children, and killing
//!   only the parent would orphan them — then reaped.
//!
//! There is no unbounded `.output()`/`.status()` anywhere in the dev
//! harness. Supervision reuses the audited `hatchling-sys` primitives
//! (`ChildPid` validation, pidfd waits, group kill) rather than
//! reimplementing them.

use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use hatchling_sys::{Child, ChildPid};

use crate::Error;

/// A bounded invocation that ran to completion (did not time out).
#[derive(Debug)]
pub struct Driven {
    /// The child's exit status.
    pub status: std::process::ExitStatus,
    /// Bytes drained from stdout while the child ran.
    pub stdout: Vec<u8>,
    /// Bytes drained from stderr while the child ran.
    pub stderr: Vec<u8>,
}

/// How long `drive` waits for a drain thread's output once the child
/// is gone. Past this the drain is abandoned rather than joined: a
/// grandchild surviving the group kill and holding a pipe open would
/// otherwise block the reader past EOF forever, hanging the driver.
const DRAIN_DEADLINE: Duration = Duration::from_secs(5);

/// Which pipe a drain thread collected.
enum Drain {
    Stdout,
    Stderr,
}

/// What to run, bounded by an absolute deadline.
#[derive(Debug)]
pub struct Spec<'a> {
    /// Program to execute (e.g. `"cargo"`).
    pub program: &'a str,
    /// Arguments.
    pub args: &'a [&'a str],
    /// Working directory, if any.
    pub current_dir: Option<&'a Path>,
    /// Extra environment variables (name, value).
    pub env: &'a [(&'a str, &'a Path)],
    /// Absolute bound. Past this deadline the child and its process
    /// group are `SIGKILL`ed and [`Error::ToolTimeout`] is returned —
    /// the tool never finished, so its output cannot be trusted.
    pub timeout: Duration,
}

fn tool_name(spec: &Spec<'_>) -> String {
    format!("{} {}", spec.program, spec.args.join(" "))
}

/// Run `spec` to completion, bounded by its deadline.
///
/// On timeout the child is `SIGKILL`ed via its pidfd, the rest of its
/// process group is `SIGKILL`ed via the group id, everything is reaped,
/// and [`Error::ToolTimeout`] is returned. A non-zero exit is NOT an
/// error here — callers that need success use [`drive_output`]; the
/// mutation runners deliberately ignore the exit status and read the
/// outcome report instead.
///
/// # Errors
///
/// Returns [`Error::Io`] if the child cannot be spawned, [`Error`]
/// from `hatchling-sys` if it cannot be supervised (fail closed: the
/// just-spawned child is killed and reaped, never left unsupervised),
/// and [`Error::ToolTimeout`] when the deadline expires.
pub fn drive(spec: &Spec<'_>) -> Result<Driven, Error> {
    let mut command = Command::new(spec.program);
    command
        .args(spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Own process group: on timeout the whole group (cargo plus
        // the rustc children it spawned) is killed, not just the
        // parent — otherwise the children would be orphaned.
        .process_group(0);
    if let Some(dir) = spec.current_dir {
        command.current_dir(dir);
    }
    for (name, value) in spec.env {
        command.env(name, value);
    }
    let mut std_child = command.spawn().map_err(Error::Io)?;
    // Wrap the validated pid in pidfd supervision before anything
    // else: from here on the child is never unsupervised.
    let pid = i32::try_from(std_child.id()).map_err(|_| Error::Parse {
        path: PathBuf::from(format!("<{} pid>", spec.program)),
        reason: format!("child pid {} does not fit in i32", std_child.id()),
    })?;
    let pid = ChildPid::new(pid).ok_or_else(|| Error::Parse {
        path: PathBuf::from(format!("<{} pid>", spec.program)),
        reason: String::from("child pid failed validation"),
    })?;
    let child = match Child::new(pid) {
        Ok(child) => child,
        Err(source) => {
            // Cannot supervise: fail closed. The child is just-spawned
            // and unreaped, so signalling it by pid has no reuse
            // window — kill and reap it rather than leave it
            // unsupervised. (Mirrors `hatchling_sys::fork`'s cleanup.)
            let _ = std_child.kill();
            let _ = std_child.wait();
            return Err(Error::Parse {
                path: PathBuf::from(format!("<{} pid>", spec.program)),
                reason: format!("cannot supervise via pidfd: {source}"),
            });
        }
    };
    // Drain stdout/stderr on dedicated threads BEFORE waiting: if the
    // child ever writes more than the pipe buffers, waiting first
    // would wedge it while the parent waits for it to exit. Each
    // drain reports through the channel, so collection below is
    // deadline-bounded: if a grandchild survives the group kill and
    // holds a pipe open, the reader would block past EOF forever —
    // the cap keeps the driver fail-closed instead of hanging.
    let (drain_tx, drain_rx) = std::sync::mpsc::channel::<(Drain, Vec<u8>)>();
    let mut expected_drains = 0;
    if let Some(mut out) = std_child.stdout.take() {
        expected_drains += 1;
        let tx = drain_tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
            let _ = tx.send((Drain::Stdout, buf));
        });
    }
    if let Some(mut err) = std_child.stderr.take() {
        expected_drains += 1;
        let tx = drain_tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
            let _ = tx.send((Drain::Stderr, buf));
        });
    }
    drop(drain_tx);
    // The std handle is now redundant: drop it. (`std::process::Child`'s
    // Drop neither kills nor waits; `child` owns supervision.)
    drop(std_child);
    // Collect the drains with a deadline: once the child is gone the
    // pipes are at EOF and the drains arrive promptly. A missing
    // drain (surviving grandchild, failed group kill) is abandoned
    // after `DRAIN_DEADLINE` — the detached reader thread dies with
    // the process — instead of hanging the join.
    let collect_drains = || {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let deadline = std::time::Instant::now() + DRAIN_DEADLINE;
        for _ in 0..expected_drains {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            match drain_rx.recv_timeout(remaining) {
                Ok((Drain::Stdout, buf)) => stdout = buf,
                Ok((Drain::Stderr, buf)) => stderr = buf,
                Err(_) => break,
            }
        }
        (stdout, stderr)
    };
    let Ok(status) = child.wait_deadline_group(spec.timeout) else {
        // The bounded wait killed the child's whole process group (so no
        // rustc child outlives the driver) before reaping it, while the
        // group's id was still the child's (WO-024). A failed group kill
        // is tolerated here because drain collection is deadline-bounded
        // and cannot hang on a pipe held open by a survivor.
        let _ = collect_drains();
        return Err(Error::ToolTimeout {
            tool: tool_name(spec),
            timeout: spec.timeout,
        });
    };
    let (stdout, stderr) = collect_drains();
    Ok(Driven {
        status: std::process::ExitStatus::from_raw(status),
        stdout,
        stderr,
    })
}

/// Run `spec` and return its stdout as a `String`.
///
/// Like [`drive`], but a non-zero exit is [`Error::ToolFailed`] and
/// non-UTF-8 stdout is [`Error::Parse`]. For the `--version` probes and
/// `cargo mutants --list` discovery, where the output text is the
/// result.
///
/// # Errors
///
/// As [`drive`], plus [`Error::ToolFailed`] on non-zero exit and
/// [`Error::Parse`] on non-UTF-8 stdout.
pub fn drive_output(spec: &Spec<'_>) -> Result<String, Error> {
    let driven = drive(spec)?;
    if !driven.status.success() {
        return Err(Error::ToolFailed {
            tool: tool_name(spec),
            status: driven.status.to_string(),
        });
    }
    String::from_utf8(driven.stdout).map_err(|e| Error::Parse {
        path: PathBuf::from(format!("<{} output>", spec.program)),
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No `sleep <marker>` process from this pid may exist: the driver
    /// must leave no orphans behind. The marker distinguishes this
    /// test's sleeps from other tests' (cargo test runs in parallel).
    /// A `sleep` argument unique to this test process: the whole
    /// seconds, then this process's pid as the fraction (`31.4242`).
    /// [`no_sleep_with_marker`] scans all of `/proc`, and parallel
    /// mutation jobs run several copies of this test binary at once:
    /// with a shared marker, one copy saw another's live sleep and
    /// failed (the only failure for 96 mutants of the full pass).
    fn unique_marker(secs: u32) -> String {
        format!("{secs}.{}", std::process::id())
    }

    fn no_sleep_with_marker(marker: &str) -> bool {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return true;
        };
        for entry in entries.flatten() {
            let Ok(bytes) = std::fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            // cmdline is NUL-separated: argv[0], argv[1], ...
            let mut args = bytes.split(|b| *b == 0);
            let is_sleep = args.next().unwrap_or(&[]) == b"sleep";
            let has_marker = args.next().unwrap_or(&[]) == marker.as_bytes();
            if is_sleep && has_marker {
                return false;
            }
        }
        true
    }

    #[test]
    fn drive_runs_true_to_completion() {
        let spec = Spec {
            program: "true",
            args: &[],
            current_dir: None,
            env: &[],
            timeout: Duration::from_secs(30),
        };
        let driven = drive(&spec).unwrap();
        assert!(driven.status.success(), "true must succeed");
    }

    #[test]
    fn drive_captures_stdout() {
        let spec = Spec {
            program: "echo",
            args: &["hello"],
            current_dir: None,
            env: &[],
            timeout: Duration::from_secs(30),
        };
        let out = drive_output(&spec).unwrap();
        assert_eq!(out.trim_end(), "hello");
    }

    #[test]
    fn drive_drain_deadline_waits_for_late_grandchild_output() {
        // Mutant: `Instant::now() + DRAIN_DEADLINE` with `+` -> `-` in
        // `collect_drains`. The direct child must exit immediately
        // while a grandchild holds stdout open, sleeping a second
        // before writing. Correct code waits within the 5 s drain
        // deadline and captures "late"; the mutant computes a past
        // deadline, abandons the drain at once, and stdout comes back
        // empty. Deterministic both ways: the write lands ~1 s in
        // (well inside the deadline, far past the mutant's instant
        // abandon), and no timing assertion is needed.
        //
        // The double `sh -c` is load-bearing: dash waits for a
        // backgrounded compound list (`sh -c 'sleep 1; echo late &'`
        // blocks a full second), so the inner `sh -c` detaches the
        // sleeper as a true grandchild and the outer shell exits at
        // once.
        let spec = Spec {
            program: "sh",
            args: &["-c", "sh -c \"sleep 1; echo late\" &"],
            current_dir: None,
            env: &[],
            timeout: Duration::from_secs(30),
        };
        let driven = drive(&spec).unwrap();
        assert!(
            driven.status.success(),
            "sh must exit 0, got {:?}",
            driven.status
        );
        let stdout = String::from_utf8_lossy(&driven.stdout);
        assert!(
            stdout.contains("late"),
            "drain deadline must wait for the grandchild's late write, got {stdout:?}"
        );
    }

    #[test]
    fn drive_times_out_and_leaves_no_orphans() {
        let marker = unique_marker(31);
        let spec = Spec {
            program: "sleep",
            args: &[marker.as_str()],
            current_dir: None,
            env: &[],
            timeout: Duration::from_secs(1),
        };
        let err = drive(&spec).unwrap_err();
        assert!(
            matches!(err, Error::ToolTimeout { .. }),
            "expected ToolTimeout, got {err:?}"
        );
        assert!(
            no_sleep_with_marker(&marker),
            "timed-out sleep must not survive"
        );
    }

    #[test]
    fn drive_kills_the_whole_process_group_on_timeout() {
        // `sh -c 'sleep 32.<pid> & wait'`: the sleep is a grandchild in the
        // driver's process group. Killing only the direct child would
        // orphan it; the group kill must take it too. The grandchild
        // also inherits the pipes, so a broken group kill would leave
        // the drain threads blocked past EOF: `drive` must still
        // return `ToolTimeout` on time (bounded drain collection), and
        // this test then fails on the orphan assertion instead of
        // hanging.
        let marker = unique_marker(32);
        let script = format!("sleep {marker} & wait");
        let spec = Spec {
            program: "sh",
            args: &["-c", script.as_str()],
            current_dir: None,
            env: &[],
            timeout: Duration::from_secs(1),
        };
        let err = drive(&spec).unwrap_err();
        assert!(
            matches!(err, Error::ToolTimeout { .. }),
            "expected ToolTimeout, got {err:?}"
        );
        // Give init a moment to reap the reparented zombie, then check
        // no live sleep remains.
        std::thread::sleep(Duration::from_secs(1));
        assert!(
            no_sleep_with_marker(&marker),
            "grandchild sleep must not survive the group kill"
        );
    }

    #[test]
    fn drive_output_rejects_nonzero_exit() {
        let spec = Spec {
            program: "false",
            args: &[],
            current_dir: None,
            env: &[],
            timeout: Duration::from_secs(30),
        };
        let err = drive_output(&spec).unwrap_err();
        assert!(
            matches!(err, Error::ToolFailed { .. }),
            "expected ToolFailed, got {err:?}"
        );
    }

    /// Pins `tool_name`: the `String::new()` / `"xyzzy"` mutants fail.
    #[test]
    fn tool_name_formats_program_and_args() {
        let spec = Spec {
            program: "cargo",
            args: &["test", "--workspace"],
            current_dir: None,
            env: &[],
            timeout: Duration::from_secs(60),
        };
        assert_eq!(tool_name(&spec), "cargo test --workspace");
    }
}
