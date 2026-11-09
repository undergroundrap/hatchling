//! The room-side foreground process-control prototype (WO-034).
//!
//! The spawner owns admission, deadlines, and teardown. This endpoint's
//! replies carry no authority. Output delivery and the production wire
//! contract are outside this prototype.

use std::error::Error;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::{Child, Command, ExitCode, Stdio};

use hatchling_core::framing::{read_frame_or_eof, write_frame};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const CONNECTION: i32 = 3;

fn main() -> ExitCode {
    let result = hatchling_sys::net::inherited_unix_stream(CONNECTION)
        .map_err(Box::<dyn Error>::from)
        .and_then(|stream| serve(&stream));
    if result.is_ok() {
        return ExitCode::SUCCESS;
    }
    let _ = writeln!(std::io::stderr(), "hatchling-room: supervisor failed");
    ExitCode::FAILURE
}

fn serve(stream: &UnixStream) -> Result<()> {
    let mut worker = None;
    let result = (|| {
        write_frame(stream, b"ready")?;
        while let Some(frame) = read_frame_or_eof(stream)? {
            let request: Vec<String> = serde_json::from_slice(&frame)?;
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
