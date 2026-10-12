//! Protocol framing: the wire format both ends speak.
//!
//! Clean-room framing, hatchling's own: every frame is a 4-byte
//! big-endian length prefix followed by that many bytes of UTF-8 JSON.
//! Frames larger than [`MAX_FRAME_BYTES`] are rejected before any
//! allocation, so a malicious or broken peer cannot make either side grow
//! its heap. The agent speaks first; anything ambiguous is fail-closed.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

use crate::Error;

/// Protocol version spoken on the wire. Bumped only with a migration
/// path; a peer speaking anything else is a protocol violation, not a
/// negotiation.
///
/// v13 carries egress bodies larger than a frame in parts
/// (`EgressOp::Stage` / `EgressResult::Staged` for a request,
/// `EgressOp::Read` / `EgressResult::Part` for an answer), with
/// `EgressOp::Egress`'s `staged` and `EgressResult::Ok`'s `more`.
/// v12 adds the read-only schedule list (`ScheduleOp::List` /
/// `ScheduleResult::Listed`), and `ScheduleSpec`'s `tz` is required
/// in the type. v11 adds the output gate's redaction result
/// (`ChannelResult::Redacted`). v10 adds spaces (`SpaceOp` / `SpaceResult`). v9 adds artifacts
/// (`ArtifactOp` / `ArtifactResult`). v8 adds hooks (`HookOp` / `HookResult`) and the run-scoped follow-up
/// op (`ScheduleOp::Followup`). v7 adds the schedule session
/// (`ScheduleOp` / `ScheduleResult`). v6 adds the goals session (`GoalsOp` / `GoalsResult`) and the
/// read-only query session (`QueryOp` / `QueryResult`). v5 adds the
/// post-handshake skill session (`SkillOp` /
/// `SkillResult`). v4 adds the post-handshake channel session
/// (`ChannelOp` / `ChannelResult`). v3 adds the post-handshake egress session
/// (`EgressOp` / `EgressResult`). v2 adds the post-handshake vault
/// session (`VaultOp` / `VaultResult`). v1 peers fail closed at the
/// version check, in both directions.
pub const PROTOCOL_VERSION: u16 = 13;

/// Largest single frame accepted, in bytes. Bounds allocation against a
/// malicious or broken peer before any heap grows.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Whether `message`, as JSON, is a payload one frame carries: at most
/// [`MAX_FRAME_BYTES`]. A message that isn't is sent another way (an
/// egress body in parts) or not at all.
pub fn fits_one_frame(message: &impl serde::Serialize) -> bool {
    serde_json::to_vec(message).is_ok_and(|payload| payload.len() <= MAX_FRAME_BYTES)
}

/// Length prefix size in bytes (a big-endian `u32`).
const LEN_PREFIX: usize = 4;

/// Writes one length-prefixed frame.
///
/// The bound is symmetric: a payload larger than [`MAX_FRAME_BYTES`],
/// which the peer's [`read_frame`] would reject as a protocol violation,
/// is refused here, before a single byte is written. The error then
/// belongs to the op that built the payload, and the session survives.
///
/// # Errors
///
/// Returns [`Error::FrameTooLarge`] for a payload above
/// [`MAX_FRAME_BYTES`] (nothing is sent), [`Error::FrameIo`] if the
/// write fails, or [`Error::FrameProtocol`] if the payload is too large
/// to length-prefix.
pub fn write_frame(stream: &UnixStream, payload: &[u8]) -> Result<(), Error> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(Error::FrameTooLarge {
            len: payload.len(),
            max: MAX_FRAME_BYTES,
        });
    }
    let len = u32::try_from(payload.len()).map_err(|_| Error::FrameProtocol {
        reason: format!("frame too large to length-prefix: {} bytes", payload.len()),
    })?;
    // `&UnixStream` implements `Write`; the local binding makes the
    // `&mut` autoref explicit.
    let mut stream = stream;
    stream
        .write_all(&len.to_be_bytes())
        .map_err(|source| Error::FrameIo { source })?;
    stream
        .write_all(payload)
        .map_err(|source| Error::FrameIo { source })?;
    Ok(())
}

/// Reads one frame, or reports a clean EOF.
///
/// Returns `Ok(None)` when the peer closed the connection before sending
/// any byte of a new frame. A truncated frame — length prefix or payload
/// cut short — is still an error, not a clean close: only a zero-byte
/// read at a frame boundary counts as EOF.
///
/// # Errors
///
/// Returns [`Error::FrameIo`] if the read fails or a frame is truncated,
/// [`Error::FrameProtocol`] if the declared length exceeds the maximum
/// (rejected before allocating).
pub fn read_frame_or_eof(stream: &UnixStream) -> Result<Option<Vec<u8>>, Error> {
    let mut stream = stream;
    let mut first = [0u8; 1];
    let n = read_uninterrupted(stream, &mut first).map_err(|source| Error::FrameIo { source })?;
    if n == 0 {
        return Ok(None);
    }
    let mut rest = [0u8; LEN_PREFIX - 1];
    stream
        .read_exact(&mut rest)
        .map_err(|source| Error::FrameIo { source })?;
    let mut len_buf = [0u8; LEN_PREFIX];
    len_buf[0] = first[0];
    len_buf[1..].copy_from_slice(&rest);
    read_frame_body(stream, len_buf).map(Some)
}

/// One `read`, retried while a signal interrupts it (`EINTR`), as
/// `read_exact` and `write_all` retry. A socket with a receive timeout
/// returns `EINTR` whenever a signal handler runs or the process is
/// stopped and continued, even under `SA_RESTART` (signal(7)): that is
/// no reason to end a session.
fn read_uninterrupted(mut reader: impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match reader.read(buf) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

/// Reads one length-prefixed frame, enforcing [`MAX_FRAME_BYTES`].
///
/// A clean close by the peer surfaces as [`Error::FrameIo`] with
/// [`ErrorKind::UnexpectedEof`](std::io::ErrorKind::UnexpectedEof): during
/// the handshake any close is premature.
///
/// # Errors
///
/// Returns [`Error::FrameIo`] if the read fails or the peer closes early,
/// [`Error::FrameProtocol`] if the declared length exceeds the maximum
/// (rejected before allocating).
pub fn read_frame(stream: &UnixStream) -> Result<Vec<u8>, Error> {
    read_frame_or_eof(stream)?.map_or_else(
        || {
            Err(Error::FrameIo {
                source: std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "peer closed the connection",
                ),
            })
        },
        Ok,
    )
}

/// Reads a frame body once the 4-byte length prefix is known.
fn read_frame_body(stream: &UnixStream, len_buf: [u8; LEN_PREFIX]) -> Result<Vec<u8>, Error> {
    let mut stream = stream;
    let len = u32::from_be_bytes(len_buf);
    let len = usize::try_from(len).map_err(|_| Error::FrameProtocol {
        reason: String::from("frame length does not fit in usize"),
    })?;
    if len > MAX_FRAME_BYTES {
        return Err(Error::FrameProtocol {
            reason: format!("frame length {len} exceeds maximum {MAX_FRAME_BYTES}"),
        });
    }
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .map_err(|source| Error::FrameIo { source })?;
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    use crate::test_socket::TestSocket;

    fn test_path(name: &str) -> TestSocket {
        TestSocket::new(format!(
            "/tmp/hatchling-test-fr-{}-{name}.sock",
            std::process::id()
        ))
    }

    fn bind_listener(name: &str) -> (UnixListener, TestSocket) {
        let path = test_path(name);
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        (listener, path)
    }

    /// Connected client/server pair with a read timeout on the server.
    /// The timeout turns a neutered `write_frame` into a fast test
    /// failure instead of a hung suite.
    fn connected_pair(name: &str) -> (UnixStream, UnixStream, TestSocket) {
        let (listener, path) = bind_listener(name);
        let client = UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        (client, server, path)
    }

    #[test]
    fn frame_round_trip() {
        let (client, server, _path) = connected_pair("frame");

        write_frame(&client, b"hello-frame").unwrap();
        assert_eq!(read_frame(&server).unwrap(), b"hello-frame");
    }

    #[test]
    fn max_size_frame_is_accepted() {
        // Boundary: a frame of exactly 64 KiB must be accepted. The
        // literal is deliberate, not MAX_FRAME_BYTES: the test pins the
        // constant's value, so it kills the `*` -> `+` mutant on the
        // constant itself as well as the `>` -> `>=` mutant on the check.
        let (client, server, _path) = connected_pair("max-frame");

        assert_eq!(MAX_FRAME_BYTES, 65_536);
        let payload = vec![0xABu8; 65_536];
        write_frame(&client, &payload).unwrap();
        assert_eq!(read_frame(&server).unwrap().len(), 65_536);
    }

    #[test]
    fn a_message_fits_one_frame_up_to_exactly_its_bound() {
        // A JSON string's payload is its text and two quotes.
        let exactly = "x".repeat(MAX_FRAME_BYTES - 2);
        assert!(fits_one_frame(&exactly));
        let over = "x".repeat(MAX_FRAME_BYTES - 1);
        assert!(!fits_one_frame(&over));
        assert!(fits_one_frame(&"small"));
    }

    #[test]
    fn an_oversize_frame_is_refused_before_any_byte_is_sent() {
        // The send-side counterpart of `max_size_frame_is_accepted`: one
        // byte over the bound is refused with a typed error, and nothing
        // reaches the wire — the next frame is the first the peer reads.
        let (client, server, _path) = connected_pair("send-oversize");

        let err = write_frame(&client, &vec![0u8; 65_537]).unwrap_err();
        assert!(
            matches!(
                err,
                Error::FrameTooLarge {
                    len: 65_537,
                    max: 65_536
                }
            ),
            "{err:?}"
        );
        assert_eq!(
            err.to_string(),
            "frame of 65537 bytes exceeds the maximum 65536; nothing was sent"
        );
        write_frame(&client, b"next").unwrap();
        assert_eq!(read_frame(&server).unwrap(), b"next");
    }

    #[test]
    fn oversize_frame_is_rejected_before_allocating() {
        let (client, mut server, _path) = connected_pair("oversize");

        // Declare a huge length, then close: the reader must refuse on
        // the length alone, without waiting for (or allocating) the body.
        let huge = u32::MAX.to_be_bytes();
        server.write_all(&huge).unwrap();
        drop(server);

        let err = read_frame(&client).unwrap_err();
        assert!(
            matches!(err, Error::FrameProtocol { .. }),
            "expected FrameProtocol, got {err:?}"
        );
    }

    #[test]
    fn clean_close_reports_none() {
        let (client, server, _path) = connected_pair("eof");

        drop(client);
        assert!(read_frame_or_eof(&server).unwrap().is_none());
    }

    #[test]
    fn clean_close_during_handshake_is_an_io_error() {
        // `read_frame` keeps the old contract: any close is premature and
        // surfaces as an I/O error, not a protocol violation.
        let (listener, path) = bind_listener("eof-strict");
        let client = UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();

        drop(client);
        let err = read_frame(&server).unwrap_err();
        assert!(
            matches!(err, Error::FrameIo { .. }),
            "expected FrameIo, got {err:?}"
        );
    }

    #[test]
    fn truncated_prefix_is_not_a_clean_close() {
        let (listener, path) = bind_listener("truncated");
        let client = UnixStream::connect(&path).unwrap();
        let (server, _) = listener.accept().unwrap();

        // One byte of the prefix, then vanish: not a frame boundary.
        let mut client = client;
        client.write_all(&[0x00]).unwrap();
        drop(client);

        let err = read_frame_or_eof(&server).unwrap_err();
        assert!(
            matches!(err, Error::FrameIo { .. }),
            "expected FrameIo, got {err:?}"
        );
    }

    /// A reader that returns `script`'s results in order, then EOF.
    struct Scripted {
        script: std::collections::VecDeque<std::io::Result<u8>>,
        reads: usize,
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            match self.script.pop_front() {
                Some(Ok(byte)) => {
                    if let Some(slot) = buf.first_mut() {
                        *slot = byte;
                    }
                    Ok(1)
                }
                Some(Err(error)) => Err(error),
                None => Ok(0),
            }
        }
    }

    fn scripted(script: Vec<std::io::Result<u8>>) -> Scripted {
        Scripted {
            script: script.into(),
            reads: 0,
        }
    }

    #[test]
    fn an_interrupted_read_is_retried_and_nothing_else_is() {
        let interrupted = || std::io::Error::from(std::io::ErrorKind::Interrupted);
        let mut reader = scripted(vec![Err(interrupted()), Err(interrupted()), Ok(7)]);
        let mut buf = [0u8; 1];
        assert_eq!(read_uninterrupted(&mut reader, &mut buf).unwrap(), 1);
        assert_eq!((buf, reader.reads), ([7], 3), "two signals, then the byte");
        let mut eof = scripted(vec![Err(interrupted())]);
        assert_eq!(
            read_uninterrupted(&mut eof, &mut buf).unwrap(),
            0,
            "then a clean EOF"
        );
        let mut timed_out = scripted(vec![
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            Ok(1),
        ]);
        let error = read_uninterrupted(&mut timed_out, &mut buf).unwrap_err();
        assert_eq!(
            (error.kind(), timed_out.reads),
            (std::io::ErrorKind::WouldBlock, 1),
            "a timeout is not retried"
        );
    }
}
