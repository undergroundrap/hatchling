//! Starting the browser: a child process with the DevTools pipe on fds 3
//! and 4 and nothing else past stderr.
//!
//! The controller's own descriptors, its connection to the broker among
//! them, never reach the browser: every descriptor is marked
//! close-on-exec in the child, and only the pipe is placed across it.

use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use hatchling_sys::{Child, ForkResult};
use serde_json::json;

use crate::cdp::Cdp;

/// What [`launch`] runs: a program and its arguments, with an explicit
/// environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The program.
    pub program: PathBuf,
    /// Its arguments, after its name.
    pub args: Vec<String>,
    /// Its whole environment.
    pub env: Vec<(String, String)>,
}

/// How long [`Browser::close`] waits for the browser to exit by itself.
pub const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// The exit code of a child whose setup or exec failed.
const EXEC_FAILED: i32 = 127;

/// Why the browser couldn't be started.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// The pipe couldn't be made.
    #[error("the DevTools pipe couldn't be made: {0}")]
    Pipe(rustix::io::Errno),
    /// The process couldn't be started.
    #[error("the browser couldn't be started: {0}")]
    Start(hatchling_sys::Error),
}

/// A running browser and its DevTools pipe. Dropping it kills the
/// browser.
#[derive(Debug)]
pub struct Browser {
    /// The browser's process: always, but for a test's browser, which is
    /// a thread on the other end of the pipe.
    child: Option<Child>,
    /// The browser's DevTools connection.
    pub cdp: Cdp,
}

/// Chromium's arguments for a headless browser in a cell, driven over
/// its pipe.
///
/// Its profile is in `profile`. When `proxy` is set, every connection goes
/// to the proxy on that port of the cell's own loopback: the cell's relay,
/// which the broker holds.
///
/// - Its own sandbox is off: it can't run in a cell, which is the
///   boundary instead (WO-029).
/// - Nothing reaches the network but through the proxy: no QUIC, WebRTC
///   only through the proxy, and loopback too goes to the proxy.
/// - No background traffic of its own: no updates, no sync, no first-run
///   pages.
#[must_use]
pub fn chromium_args(profile: &Path, proxy: Option<u16>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--headless",
        "--no-sandbox",
        "--no-zygote",
        "--disable-gpu",
        "--disable-dev-shm-usage",
        "--remote-debugging-pipe",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--disable-extensions",
        "--disable-quic",
        "--force-webrtc-ip-handling-policy",
        "--webrtc-ip-handling-policy=disable_non_proxied_udp",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    args.push(format!("--user-data-dir={}", profile.display()));
    if let Some(port) = proxy {
        args.push(format!("--proxy-server=http://127.0.0.1:{port}"));
        // Chromium sends loopback around a proxy unless told not to.
        args.push(String::from("--proxy-bypass-list=<-loopback>"));
    }
    args.push(String::from("about:blank"));
    args
}

/// Starts `launch` with the DevTools pipe on fds 3 (it reads) and 4 (it
/// writes).
///
/// # Errors
///
/// [`LaunchError`] if the pipe or the process can't be made. A program
/// that can't be executed exits [`EXEC_FAILED`], which the first call on
/// its pipe sees as a closed pipe.
pub fn launch(launch: &Launch) -> Result<Browser, LaunchError> {
    let (browser_reads, to_browser) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(LaunchError::Pipe)?;
    let (from_browser, browser_writes) =
        rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC).map_err(LaunchError::Pipe)?;
    let program = launch.program.clone();
    let argv: Vec<String> = std::iter::once(program.display().to_string())
        .chain(launch.args.iter().cloned())
        .collect();
    match hatchling_sys::fork().map_err(LaunchError::Start)? {
        ForkResult::Child => {
            let placed = hatchling_sys::cloexec_fds_above_stderr().and_then(|()| {
                hatchling_sys::place_across_exec(
                    &[browser_reads.as_fd(), browser_writes.as_fd()],
                    3,
                )
            });
            if placed.is_err() {
                hatchling_sys::exit_child(EXEC_FAILED);
            }
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            let env: Vec<(&str, &str)> = launch
                .env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let _ = hatchling_sys::exec(&program, &argv, &env, || Ok(()));
            hatchling_sys::exit_child(EXEC_FAILED)
        }
        ForkResult::Parent { child } => {
            drop((browser_reads, browser_writes));
            let cdp = Cdp::new(to_browser, from_browser).map_err(LaunchError::Pipe)?;
            Ok(Browser {
                child: Some(child),
                cdp,
            })
        }
    }
}

impl Browser {
    /// Asks the browser to close, waits for it to exit by `CLOSE_GRACE`,
    /// and kills it if it hasn't: its wait status (see
    /// [`hatchling_sys::child_exit_code`]), or `None` if it had to be
    /// killed.
    pub fn close(self) -> Option<i32> {
        let Self { child, mut cdp } = self;
        let _ = cdp.call(
            None,
            "Browser.close",
            json!({}),
            Instant::now() + CLOSE_GRACE,
        );
        drop(cdp);
        child.and_then(|child| child.wait_deadline(CLOSE_GRACE).ok())
    }

    /// A browser that is only `cdp`'s far end, for a test that answers
    /// it itself.
    #[cfg(test)]
    pub(crate) const fn over(cdp: Cdp) -> Self {
        Self { child: None, cdp }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    fn shell(script: &str) -> Launch {
        Launch {
            program: PathBuf::from("/bin/bash"),
            args: vec![String::from("-c"), String::from(script)],
            env: vec![
                (String::from("PATH"), String::from("/usr/bin:/bin")),
                (
                    String::from("ANSWER"),
                    json!({"id": 1, "result": {"echo": true}}).to_string(),
                ),
            ],
        }
    }

    /// The browser's descriptors, listed from outside it: 0 to 4, and
    /// nothing of the controller's.
    #[test]
    fn the_browser_gets_stdio_and_the_pipe_and_nothing_else() {
        let dir =
            std::env::temp_dir().join(format!("hatchling-browser-fds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fds");
        // The controller holds a descriptor without close-on-exec, as it
        // holds its broker connection: it mustn't cross.
        let held = rustix::io::dup(std::io::stdout()).unwrap();
        assert_eq!(
            hatchling_sys::fd_flags(std::os::fd::AsRawFd::as_raw_fd(&held)),
            Some(0)
        );
        let script = format!("ls -1 /proc/$$/fd > {}", out.display());
        let browser = launch(&shell(&script)).unwrap();
        let code = browser
            .child
            .as_ref()
            .unwrap()
            .wait_deadline(Duration::from_secs(30))
            .unwrap();
        drop(held);
        assert_eq!(hatchling_sys::child_exit_code(code), 0);
        let mut listed = String::new();
        std::fs::File::open(&out)
            .unwrap()
            .read_to_string(&mut listed)
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let fds: Vec<&str> = listed.lines().collect();
        assert_eq!(fds, ["0", "1", "2", "3", "4"], "{listed}");
    }

    /// What the browser writes to fd 4 is what the controller reads, and
    /// what the controller writes reaches the browser's fd 3.
    #[test]
    fn the_pipe_runs_both_ways_on_three_and_four() {
        // An echo of one message, back with an id the call waits for.
        let browser = launch(&shell(
            "IFS= read -r -d '' line <&3; printf '%s\\0' \"$ANSWER\" >&4; sleep 30",
        ))
        .unwrap();
        let mut browser = browser;
        let result = browser
            .cdp
            .call(
                None,
                "Browser.getVersion",
                json!({}),
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(result, json!({"echo": true}));
        // It doesn't answer the close, so closing waits the grace and
        // then kills it.
        let started = Instant::now();
        assert_eq!(browser.close(), None);
        assert!(started.elapsed() < CLOSE_GRACE * 3, "bounded");
    }

    #[test]
    fn a_program_that_cant_run_is_a_closed_pipe() {
        let mut browser = launch(&Launch {
            program: PathBuf::from("/nonexistent/chrome"),
            args: Vec::new(),
            env: Vec::new(),
        })
        .unwrap();
        assert_eq!(
            browser.cdp.call(
                None,
                "Browser.getVersion",
                json!({}),
                Instant::now() + Duration::from_secs(10)
            ),
            Err(crate::cdp::CdpError::Gone)
        );
        assert_eq!(
            browser.close().map(hatchling_sys::child_exit_code),
            Some(EXEC_FAILED)
        );
    }

    #[test]
    fn chromium_is_held_to_the_proxy_and_its_pipe() {
        let args = chromium_args(Path::new("/scratch/profile"), Some(8080));
        for wanted in [
            "--remote-debugging-pipe",
            "--no-sandbox",
            "--disable-quic",
            "--proxy-server=http://127.0.0.1:8080",
            "--proxy-bypass-list=<-loopback>",
            "--user-data-dir=/scratch/profile",
            "--webrtc-ip-handling-policy=disable_non_proxied_udp",
        ] {
            assert!(args.iter().any(|arg| arg == wanted), "{wanted}");
        }
        assert!(
            !args
                .iter()
                .any(|arg| arg.starts_with("--remote-debugging-port"))
        );
        assert_eq!(args.last().map(String::as_str), Some("about:blank"));
        let direct = chromium_args(Path::new("/p"), None);
        assert!(!direct.iter().any(|arg| arg.starts_with("--proxy")));
    }
}
