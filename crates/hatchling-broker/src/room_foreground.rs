//! The spawner side of the private, one-command Room exchange.

use std::io::{self, Read as _, Write as _};
use std::net::Shutdown;
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const OUTPUT_LIMIT: usize = 4 * 1024;
const IO_QUANTUM: Duration = Duration::from_millis(100);

/// Termination reported by the confined Room supervisor.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoomForegroundStatus {
    /// The command returned this exit code.
    Exited(u8),
    /// The command was terminated by this Linux signal.
    Signaled(u8),
    /// The supervisor could not start the requested program.
    SpawnFailed,
}

/// Untrusted output of one installed Room command, returned after teardown.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoomForegroundResult {
    /// Termination reported by the supervisor.
    pub status: RoomForegroundStatus,
    /// The first 4 KiB of standard output, as bytes.
    pub stdout: Vec<u8>,
    /// The first 4 KiB of standard error, as bytes.
    pub stderr: Vec<u8>,
    /// Standard output exceeded its byte limit.
    pub stdout_truncated: bool,
    /// Standard error exceeded its byte limit.
    pub stderr_truncated: bool,
}

pub(super) struct Operation<'a> {
    pub(super) requester: &'a UnixStream,
    pub(super) argv: &'a [String],
}

pub(super) fn validate_command(argv: &[String]) -> Result<(), String> {
    let Some(program) = argv.first() else {
        return Err(String::from("Room foreground execution requires a program"));
    };
    if !Path::new(program).is_absolute() {
        return Err(String::from(
            "Room foreground execution requires an absolute program path",
        ));
    }
    if argv.len() > 64
        || argv.iter().map(String::len).sum::<usize>() > 8 * 1024
        || argv.iter().any(|argument| argument.contains('\0'))
    {
        return Err(String::from("invalid or oversized Room command arguments"));
    }
    Ok(())
}

fn decode_result(bytes: &[u8], nonce: &str) -> Result<RoomForegroundResult, String> {
    let (echo, result): (String, RoomForegroundResult) = serde_json::from_slice(bytes)
        .map_err(|_| String::from("the Room supervisor sent an invalid foreground result"))?;
    if echo != nonce {
        return Err(String::from(
            "the Room supervisor sent a result for a different dispatch",
        ));
    }
    let valid_output = |output: &[u8], truncated: bool| {
        output.len() <= OUTPUT_LIMIT && (!truncated || output.len() == OUTPUT_LIMIT)
    };
    let valid_status = match result.status {
        RoomForegroundStatus::Exited(_) => true,
        RoomForegroundStatus::Signaled(signal) => (1..=64).contains(&signal),
        RoomForegroundStatus::SpawnFailed => result.stdout.is_empty() && result.stderr.is_empty(),
    };
    if !valid_status
        || !valid_output(&result.stdout, result.stdout_truncated)
        || !valid_output(&result.stderr, result.stderr_truncated)
    {
        return Err(String::from(
            "the Room supervisor sent an invalid foreground result",
        ));
    }
    Ok(result)
}

// The broker never sends a second request on this connection. While the
// room works, EOF or additional input cancels the operation.
fn require_quiet(stream: &UnixStream, reason: &'static str) -> io::Result<()> {
    if hatchling_sys::net::unix_stream_quiet(stream.as_fd())? {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::ConnectionAborted, reason))
    }
}

impl Operation<'_> {
    pub(super) fn connected(&self) -> io::Result<()> {
        require_quiet(
            self.requester,
            "the broker ended the Room foreground request",
        )
    }

    fn transfer<T>(
        &self,
        deadline: Instant,
        mut attempt: impl FnMut(Duration) -> io::Result<T>,
    ) -> io::Result<T> {
        let expired =
            || io::Error::new(io::ErrorKind::TimedOut, "Room foreground deadline expired");
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(expired());
            }
            self.connected()?;
            match attempt(remaining.min(IO_QUANTUM)) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) => {}
                result => {
                    if Instant::now() >= deadline {
                        return Err(expired());
                    }
                    return result;
                }
            }
        }
    }

    pub(super) fn read(
        &self,
        mut supervisor: &UnixStream,
        deadline: Instant,
        bytes: &mut [u8],
    ) -> io::Result<usize> {
        self.transfer(deadline, |remaining| {
            supervisor.set_read_timeout(Some(remaining))?;
            supervisor.read(bytes)
        })
    }

    pub(super) fn wait_for_exit(
        &self,
        deadline: Instant,
        mut try_wait: impl FnMut() -> io::Result<Option<i32>>,
    ) -> io::Result<i32> {
        self.transfer(deadline, |remaining| {
            if let Some(status) = try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(remaining);
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        })
    }

    fn read_exact(
        &self,
        supervisor: &UnixStream,
        deadline: Instant,
        mut bytes: &mut [u8],
    ) -> io::Result<()> {
        while !bytes.is_empty() {
            let count = self.read(supervisor, deadline, bytes)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete Room foreground result",
                ));
            }
            bytes = bytes
                .get_mut(count..)
                .ok_or_else(|| io::Error::other("invalid Room read length"))?;
        }
        Ok(())
    }

    pub(super) fn exchange(
        &self,
        supervisor: &UnixStream,
        deadline: Instant,
    ) -> Result<RoomForegroundResult, String> {
        let nonce = crate::approval::random_hex("").map_err(|error| error.to_string())?;
        let run = || -> io::Result<Vec<u8>> {
            require_quiet(
                supervisor,
                "the Room supervisor spoke before dispatch or closed",
            )?;
            let request = std::iter::once("run")
                .chain(self.argv.iter().map(String::as_str))
                .chain(std::iter::once(nonce.as_str()))
                .collect::<Vec<_>>();
            let payload = serde_json::to_vec(&request)?;
            let size = u32::try_from(payload.len()).map_err(io::Error::other)?;
            let mut frame = size.to_be_bytes().to_vec();
            frame.extend(payload);
            let mut unsent = frame.as_slice();
            while !unsent.is_empty() {
                let count = self.transfer(deadline, |remaining| {
                    let mut socket = supervisor;
                    socket.set_write_timeout(Some(remaining))?;
                    socket.write(unsent)
                })?;
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "could not dispatch the Room command",
                    ));
                }
                unsent = unsent
                    .get(count..)
                    .ok_or_else(|| io::Error::other("invalid Room write length"))?;
            }
            supervisor.shutdown(Shutdown::Write)?;

            let mut size = [0_u8; 4];
            self.read_exact(supervisor, deadline, &mut size)?;
            let size = usize::try_from(u32::from_be_bytes(size)).map_err(io::Error::other)?;
            if size > hatchling_core::MAX_FRAME_BYTES {
                return Err(io::Error::other("oversized Room foreground frame"));
            }
            let mut body = vec![0_u8; size];
            self.read_exact(supervisor, deadline, &mut body)?;
            if self.read(supervisor, deadline, &mut [0])? != 0 {
                return Err(io::Error::other(
                    "extra data after the Room foreground result",
                ));
            }
            Ok(body)
        };
        let body = run().map_err(|error| error.to_string())?;
        decode_result(&body, &nonce)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchling_core::framing::{read_frame, write_frame};

    const EXITED: &[u8] =
        br#"{"status":{"exited":23},"stdout":[255],"stderr":[],"stdout_truncated":false,"stderr_truncated":false}"#;

    fn result_frame(nonce: &str, payload: &[u8]) -> Vec<u8> {
        let payload: serde_json::Value = serde_json::from_slice(payload).unwrap();
        serde_json::to_vec(&(nonce, payload)).unwrap()
    }

    #[test]
    fn validate_command_requires_bounded_absolute_argv() {
        let mut args = vec![String::from("/bin/true"); 64];
        assert!(validate_command(&args).is_ok());
        args.push(String::new());
        assert!(validate_command(&args).is_err());
        let mut long = String::from("/");
        long.push_str(&"x".repeat(8191));
        assert!(validate_command(&[long.clone()]).is_ok());
        assert!(validate_command(&[long, String::from("x")]).is_err());
        assert!(validate_command(&[String::from("/bin/true"), String::from("\0")]).is_err());
        assert!(validate_command(&[String::from("bin/true")]).is_err());
        assert!(validate_command(&[]).is_err());
    }

    #[test]
    fn decode_result_checks_both_output_bounds_and_termination() {
        let base = decode_result(&result_frame("nonce", EXITED), "nonce").unwrap();
        assert_eq!(base.status, RoomForegroundStatus::Exited(23));
        assert_eq!(base.stdout, [255]);
        let mut value: serde_json::Value = serde_json::from_slice(EXITED).unwrap();
        for stream in ["stdout", "stderr"] {
            let mut over = value.clone();
            *over.get_mut(stream).unwrap() = serde_json::json!(vec![255_u8; 4097]);
            assert!(
                decode_result(&serde_json::to_vec(&("nonce", over)).unwrap(), "nonce").is_err()
            );
            let mut wrong_marker = value.clone();
            *wrong_marker.get_mut(format!("{stream}_truncated")).unwrap() = true.into();
            assert!(
                decode_result(
                    &serde_json::to_vec(&("nonce", wrong_marker)).unwrap(),
                    "nonce"
                )
                .is_err()
            );
        }
        for status in [
            serde_json::json!({"signaled": 0}),
            serde_json::json!({"signaled": 65}),
            serde_json::json!("spawn-failed"),
            serde_json::json!({"exited": 256}),
        ] {
            *value.get_mut("status").unwrap() = status;
            assert!(
                decode_result(&serde_json::to_vec(&("nonce", &value)).unwrap(), "nonce").is_err()
            );
        }
        assert!(decode_result(b"bad", "nonce").is_err());
        assert!(decode_result(&result_frame("old", EXITED), "nonce").is_err());
        let at_cap = RoomForegroundResult {
            status: RoomForegroundStatus::Signaled(15),
            stdout: vec![255; 4096],
            stderr: vec![255; 4096],
            stdout_truncated: true,
            stderr_truncated: false,
        };
        let encoded = serde_json::to_vec(&("nonce", &at_cap)).unwrap();
        assert_eq!(decode_result(&encoded, "nonce").unwrap(), at_cap);
    }

    #[test]
    fn exchange_checks_dispatch_and_accepts_only_one_complete_result() {
        let (broker, requester) = UnixStream::pair().unwrap();
        let operation = Operation {
            requester: &requester,
            argv: &[String::from("/bin/true"), String::from("one arg")],
        };
        for (reply, expected) in [
            (EXITED, None),
            (b"invalid", Some("invalid foreground result")),
        ] {
            let (supervisor, peer) = UnixStream::pair().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            std::thread::scope(|scope| {
                let task = scope.spawn(move || {
                    let dispatch: Vec<String> =
                        serde_json::from_slice(&read_frame(&peer).unwrap()).unwrap();
                    let (nonce, request) = dispatch.split_last().unwrap();
                    assert_eq!(request, ["run", "/bin/true", "one arg"]);
                    assert_eq!(nonce.len(), 64);
                    if expected.is_none() {
                        write_frame(&peer, &result_frame(nonce, reply)).unwrap();
                    } else {
                        write_frame(&peer, reply).unwrap();
                    }
                });
                let result =
                    operation.exchange(&supervisor, Instant::now() + Duration::from_secs(2));
                task.join().unwrap();
                match expected {
                    Some(message) => assert!(result.unwrap_err().contains(message)),
                    None => assert_eq!(result.unwrap().status, RoomForegroundStatus::Exited(23)),
                }
            });
        }
        for (payload, message) in [
            ([0, 1, 0, 1].as_slice(), "oversized Room foreground frame"),
            (
                [0, 0, 0, 8, b'{'].as_slice(),
                "incomplete Room foreground result",
            ),
        ] {
            let (supervisor, mut peer) = UnixStream::pair().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            std::thread::scope(|scope| {
                let task = scope.spawn(move || {
                    read_frame(&peer).unwrap();
                    peer.write_all(payload).unwrap();
                });
                let result =
                    operation.exchange(&supervisor, Instant::now() + Duration::from_secs(2));
                task.join().unwrap();
                assert!(result.unwrap_err().contains(message));
            });
        }
        let (supervisor, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        std::thread::scope(|scope| {
            let task = scope.spawn(move || {
                read_frame(&peer).unwrap();
                write_frame(&peer, EXITED).unwrap();
                peer.write_all(b"!").unwrap();
            });
            let result = operation.exchange(&supervisor, Instant::now() + Duration::from_secs(2));
            task.join().unwrap();
            assert!(result.unwrap_err().contains("extra data"));
        });
        let (supervisor, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        std::thread::scope(|scope| {
            let task = scope.spawn(move || {
                read_frame(&peer).unwrap();
                write_frame(&peer, &result_frame("previous-dispatch", EXITED)).unwrap();
            });
            let result = operation.exchange(&supervisor, Instant::now() + Duration::from_secs(2));
            task.join().unwrap();
            assert!(result.unwrap_err().contains("different dispatch"));
        });
        let (supervisor, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"!").unwrap();
        assert!(
            operation
                .exchange(&supervisor, Instant::now() + Duration::from_secs(2))
                .unwrap_err()
                .contains("before dispatch")
        );
        assert!(operation.connected().is_ok());
        drop(broker);
        assert!(operation.connected().is_err());
    }

    #[test]
    fn transfer_deadline_and_broker_disconnect_end_a_stalled_read() {
        let (supervisor, _peer) = UnixStream::pair().unwrap();
        let (broker, requester) = UnixStream::pair().unwrap();
        let operation = Operation {
            requester: &requester,
            argv: &[],
        };
        let permanent = operation.transfer(Instant::now() + Duration::from_millis(500), |_| {
            Err::<(), _>(io::Error::from(io::ErrorKind::BrokenPipe))
        });
        assert_eq!(permanent.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        let deadline = Instant::now() + Duration::from_millis(20);
        let result = operation.transfer(deadline, |_| {
            std::thread::sleep(
                deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(1),
            );
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);

        let deadline = Instant::now() + Duration::from_millis(40);
        let result = operation.read(&supervisor, deadline, &mut [0]).unwrap_err();
        assert_eq!(result.kind(), io::ErrorKind::TimedOut);
        assert!(result.to_string().contains("deadline expired"));

        let deadline = Instant::now() + Duration::from_secs(2);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(40));
                drop(broker);
            });
            let result = operation.read(&supervisor, deadline, &mut [0]).unwrap_err();
            assert_eq!(result.kind(), io::ErrorKind::ConnectionAborted);
        });
    }

    #[test]
    fn wait_for_exit_checks_broker_after_the_supervisor_reply() {
        let (broker, requester) = UnixStream::pair().unwrap();
        let operation = Operation {
            requester: &requester,
            argv: &[],
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        assert_eq!(
            operation.wait_for_exit(deadline, || Ok(Some(23))).unwrap(),
            23
        );
        let mut broker = Some(broker);
        let refused = operation.wait_for_exit(deadline, || {
            drop(broker.take());
            Ok(None)
        });
        assert_eq!(
            refused.unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }

    #[test]
    fn require_quiet_rejects_data_and_eof() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        assert!(require_quiet(&stream, "peer spoke").is_ok());
        peer.write_all(b"x").unwrap();
        let unexpected = require_quiet(&stream, "peer spoke").unwrap_err();
        assert_eq!(unexpected.to_string(), "peer spoke");
        assert_eq!(unexpected.kind(), io::ErrorKind::ConnectionAborted);
        let mut reader = &stream;
        reader.read_exact(&mut [0]).unwrap();
        drop(peer);
        assert!(require_quiet(&stream, "peer closed").is_err());
    }

    #[test]
    fn exchange_accepts_a_result_at_the_frame_size_limit() {
        let (_broker, requester) = UnixStream::pair().unwrap();
        let operation = Operation {
            requester: &requester,
            argv: &[String::from("/bin/true")],
        };
        let (supervisor, peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        peer.set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        std::thread::scope(|scope| {
            let task = scope.spawn(move || {
                let dispatch: Vec<String> =
                    serde_json::from_slice(&read_frame(&peer).unwrap()).unwrap();
                let mut payload = result_frame(dispatch.last().unwrap(), EXITED);
                payload.resize(hatchling_core::MAX_FRAME_BYTES, b' ');
                write_frame(&peer, &payload).unwrap();
            });
            let result = operation.exchange(&supervisor, Instant::now() + Duration::from_secs(2));
            task.join().unwrap();
            assert_eq!(result.unwrap().status, RoomForegroundStatus::Exited(23));
        });
    }
}
