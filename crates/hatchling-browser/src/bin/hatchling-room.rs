//! The room-side foreground process controller (WO-034).
//!
//! The spawner owns admission, deadlines, and teardown. This endpoint's
//! replies and captured bytes are untrusted data.

use std::error::Error;
use std::fs::File;
use std::io::{self, Read, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};

use hatchling_core::framing::{read_frame_or_eof, write_frame};
use rustix::event::{PollFd, PollFlags, poll};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const CONNECTION: i32 = 3;
const OUTPUT_LIMIT: usize = 4 * 1024;

fn main() -> ExitCode {
    let result = hatchling_sys::net::inherited_unix_stream(CONNECTION)
        .map_err(Box::<dyn Error>::from)
        .and_then(|stream| serve(&stream));
    if result.is_ok() {
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(io::stderr(), "hatchling-room: supervisor failed");
    ExitCode::FAILURE
}

fn serve(stream: &UnixStream) -> Result<()> {
    let mut worker = None;
    let result = (|| {
        write_frame(stream, b"ready")?;
        while let Some(frame) = read_frame_or_eof(stream)? {
            let request: Vec<String> = serde_json::from_slice(&frame)?;
            if let [op, program, args @ .., nonce] = request.as_slice() {
                if op == "run" && worker.is_none() {
                    write_frame(stream, &serde_json::to_vec(&(nonce, run(program, args)?))?)?;
                    break;
                }
            }
            let reply = dispatch(&request, &mut worker)?;
            write_frame(stream, reply.as_bytes())?;
        }
        Ok(())
    })();
    if let Some(mut child) = worker {
        child.kill()?;
        child.wait()?;
    }
    result
}

fn run(program: &str, args: &[String]) -> Result<serde_json::Value> {
    if !Path::new(program).is_absolute() {
        return Err("expected an absolute room program path".into());
    }
    let Ok(mut child) = Command::new(program)
        .args(args)
        .current_dir("/workspace")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return Ok(serde_json::json!({
            "status": "spawn-failed",
            "stdout": [], "stderr": [],
            "stdout_truncated": false, "stderr_truncated": false
        }));
    };
    let result = collect(&mut child);
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn collect(child: &mut Child) -> Result<serde_json::Value> {
    let stdout = child.stdout.take().ok_or("missing stdout pipe")?;
    let stderr = child.stderr.take().ok_or("missing stderr pipe")?;
    let mut captures = [
        Capture::new(File::from(OwnedFd::from(stdout))),
        Capture::new(File::from(OwnedFd::from(stderr))),
    ];
    while captures.iter().any(|capture| capture.pipe.is_some()) {
        let mut fds: Vec<_> = captures
            .iter()
            .filter_map(|capture| capture.pipe.as_ref())
            .map(|pipe| PollFd::new(pipe, PollFlags::IN))
            .collect();
        match poll(&mut fds, -1) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
        let mut ready = fds.into_iter().map(|fd| !fd.revents().is_empty());
        let readable = captures
            .each_ref()
            .map(|capture| capture.pipe.is_some() && ready.next().unwrap_or(false));
        drop(ready);
        for (capture, readable) in captures.iter_mut().zip(readable) {
            if readable {
                capture.drain()?;
            }
        }
    }
    let [stdout, stderr] = captures;
    let status = child.wait()?;
    let status = match (status.code(), status.signal()) {
        (Some(code), _) => serde_json::json!({ "exited": code }),
        (_, Some(signal)) => serde_json::json!({ "signaled": signal }),
        _ => return Err("unrecognized child exit status".into()),
    };
    Ok(serde_json::json!({
        "status": status,
        "stdout": stdout.bytes, "stderr": stderr.bytes,
        "stdout_truncated": stdout.truncated, "stderr_truncated": stderr.truncated
    }))
}

struct Capture {
    pipe: Option<File>,
    bytes: Vec<u8>,
    truncated: bool,
}

impl Capture {
    const fn new(pipe: File) -> Self {
        Self {
            pipe: Some(pipe),
            bytes: Vec::new(),
            truncated: false,
        }
    }

    fn drain(&mut self) -> io::Result<()> {
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(());
        };
        let mut buffer = [0_u8; OUTPUT_LIMIT];
        match pipe.read(&mut buffer) {
            Ok(0) => self.pipe = None,
            Ok(count) => {
                let keep = count.min(OUTPUT_LIMIT.saturating_sub(self.bytes.len()));
                let prefix = buffer
                    .get(..keep)
                    .ok_or_else(|| io::Error::other("invalid pipe read length"))?;
                self.bytes.extend_from_slice(prefix);
                self.truncated |= keep < count;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }
}

fn dispatch(request: &[String], worker: &mut Option<Child>) -> Result<String> {
    match request {
        [op, program, args @ ..] if op == "start" => {
            if worker.is_some() {
                return Ok(String::from("busy"));
            }
            if !Path::new(program).is_absolute() {
                return Err("expected an absolute room program path".into());
            }
            // A job receives no supervisor environment or stdio. The
            // adopted control socket is close-on-exec, too.
            *worker = Some(
                Command::new(program)
                    .args(args)
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()?,
            );
            Ok(String::from("started"))
        }
        [op] if op == "poll" => {
            let Some(child) = worker else {
                return Ok(String::from("idle"));
            };
            let Some(status) = child.try_wait()? else {
                return Ok(String::from("running"));
            };
            *worker = None;
            match (status.code(), status.signal()) {
                (Some(code), _) => Ok(format!("exit:{code}")),
                (_, Some(signal)) => Ok(format!("signal:{signal}")),
                _ => Err("unrecognized child exit status".into()),
            }
        }
        [op] if op == "stop" => {
            let Some(child) = worker else {
                return Ok(String::from("idle"));
            };
            // The spawner must drain the room cgroup to stop descendants.
            child.kill()?;
            Ok(String::from("stop-requested"))
        }
        _ => Err("unrecognized supervisor request".into()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "fixture failures must fail the test")]

    use super::*;

    #[test]
    fn serve_reports_ready_and_accepts_clean_close() {
        let (peer, endpoint) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let task = std::thread::spawn(move || serve(&endpoint).is_ok());
        let ready = hatchling_core::framing::read_frame(&peer);
        drop(peer);
        assert!(task.join().unwrap());
        assert_eq!(ready.unwrap(), b"ready");
    }

    #[test]
    fn capture_distinguishes_exact_limit_excess_and_eof() {
        let mut capture = Capture::new(File::open("/dev/zero").unwrap());
        capture.drain().unwrap();
        assert_eq!(capture.bytes, vec![0; OUTPUT_LIMIT]);
        assert!(!capture.truncated);
        capture.drain().unwrap();
        assert_eq!(capture.bytes, vec![0; OUTPUT_LIMIT]);
        assert!(capture.truncated);
        let mut empty = Capture::new(File::open("/dev/null").unwrap());
        empty.drain().unwrap();
        assert!(empty.pipe.is_none());
        assert!(empty.bytes.is_empty());
    }

    #[test]
    fn dispatch_refuses_ambiguous_control_requests() {
        let mut worker = None;
        assert!(dispatch(&[], &mut worker).is_err());
        assert!(dispatch(&["poll".into(), "unexpected".into()], &mut worker).is_err());
        assert!(dispatch(&["stop".into(), "unexpected".into()], &mut worker).is_err());
        assert!(dispatch(&["start".into()], &mut worker).is_err());
        assert!(worker.is_none());
        let unknown = dispatch(&["unknown".into(), "/bin/true".into()], &mut worker);
        if let Some(mut unexpected_child) = worker {
            unexpected_child.wait().unwrap();
        }
        assert!(unknown.is_err());
    }
}
