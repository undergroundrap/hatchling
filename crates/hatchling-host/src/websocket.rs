//! The WebSocket frames the host speaks (RFC 6455), and no more.
//!
//! The browser's frames are untrusted bytes from any page that can reach
//! the loopback port, so this reads them strictly:
//!
//! - **One frame per message:** `FIN` set, no reserved bits (so no
//!   extensions), no continuation frames.
//! - **Masked,** as every client frame must be.
//! - **Text, close, ping and pong** only; a binary frame is refused.
//! - **Bounded:** a text frame at most [`MAX_FRAME_BYTES`], the broker's
//!   own limit; a control frame at most 125 bytes, as the RFC says.
//! - **Text is UTF-8.**
//!
//! Anything else ends the connection.

use std::io::{Read, Write};

use base64::Engine as _;
use hatchling_core::framing::MAX_FRAME_BYTES;
use sha1::{Digest as _, Sha1};

use crate::Error;

/// The GUID RFC 6455 appends to the client's key.
const ACCEPT_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The most a control frame may carry.
const MAX_CONTROL_BYTES: usize = 125;

/// The opcodes the host reads and writes.
const TEXT: u8 = 0x1;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;

/// A frame from the browser.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    /// A text message: one request, as JSON.
    Text(Vec<u8>),
    /// The browser is closing.
    Close,
    /// A ping, with its payload, to be answered.
    Ping(Vec<u8>),
    /// A pong.
    Pong,
}

/// The `Sec-WebSocket-Accept` value for the client's `key`.
#[must_use]
pub fn accept_key(key: &str) -> String {
    let mut hash = Sha1::new();
    hash.update(key.as_bytes());
    hash.update(ACCEPT_GUID.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hash.finalize())
}

/// Reads one frame from the browser.
///
/// # Errors
///
/// [`Error::Io`] when the connection fails or ends, and
/// [`Error::Frame`] for anything the module doc refuses.
pub fn read_frame(reader: &mut impl Read) -> Result<Frame, Error> {
    let mut head = [0_u8; 2];
    reader.read_exact(&mut head)?;
    let [first, second] = head;
    if first & 0x80 == 0 {
        return Err(Error::Frame("a message split over frames"));
    }
    if first & 0x70 != 0 {
        return Err(Error::Frame("a reserved bit is set"));
    }
    if second & 0x80 == 0 {
        return Err(Error::Frame("a client frame is not masked"));
    }
    let opcode = first & 0x0F;
    let length = match second & 0x7F {
        126 => {
            let mut bytes = [0_u8; 2];
            reader.read_exact(&mut bytes)?;
            u64::from(u16::from_be_bytes(bytes))
        }
        127 => {
            let mut bytes = [0_u8; 8];
            reader.read_exact(&mut bytes)?;
            u64::from_be_bytes(bytes)
        }
        short => u64::from(short),
    };
    let limit = if opcode == TEXT {
        MAX_FRAME_BYTES
    } else {
        MAX_CONTROL_BYTES
    };
    let length = usize::try_from(length)
        .ok()
        .filter(|length| *length <= limit)
        .ok_or(Error::Frame("a frame is larger than allowed"))?;
    let mut mask = [0_u8; 4];
    reader.read_exact(&mut mask)?;
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    for (byte, key) in payload.iter_mut().zip(mask.iter().cycle()) {
        *byte ^= key;
    }
    match opcode {
        TEXT => {
            std::str::from_utf8(&payload).map_err(|_| Error::Frame("text that is not UTF-8"))?;
            Ok(Frame::Text(payload))
        }
        CLOSE => Ok(Frame::Close),
        PING => Ok(Frame::Ping(payload)),
        PONG => Ok(Frame::Pong),
        _ => Err(Error::Frame("an opcode the host doesn't speak")),
    }
}

/// Writes `payload` to the browser as one text frame.
///
/// # Errors
///
/// [`Error::Io`] when the write fails.
pub fn write_text(writer: &mut impl Write, payload: &[u8]) -> Result<(), Error> {
    write_frame(writer, TEXT, payload)
}

/// Answers a ping with its payload.
///
/// # Errors
///
/// [`Error::Io`] when the write fails.
pub fn write_pong(writer: &mut impl Write, payload: &[u8]) -> Result<(), Error> {
    write_frame(writer, PONG, payload)
}

/// Tells the browser the connection is closing.
///
/// # Errors
///
/// [`Error::Io`] when the write fails.
pub fn write_close(writer: &mut impl Write) -> Result<(), Error> {
    write_frame(writer, CLOSE, &[])
}

/// One unmasked frame (a server's frames are never masked), `FIN` set.
fn write_frame(writer: &mut impl Write, opcode: u8, payload: &[u8]) -> Result<(), Error> {
    // `FIN` plus the opcode: disjoint bits, added rather than or-ed.
    let mut head = vec![0x80 + opcode];
    match (u8::try_from(payload.len()), u16::try_from(payload.len())) {
        (Ok(short), _) if short < 126 => head.push(short),
        (_, Ok(medium)) => {
            head.push(126);
            head.extend_from_slice(&medium.to_be_bytes());
        }
        _ => {
            head.push(127);
            let long = u64::try_from(payload.len())
                .map_err(|_| Error::Frame("a frame too large to send"))?;
            head.extend_from_slice(&long.to_be_bytes());
        }
    }
    writer.write_all(&head)?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client frame: `FIN` and `opcode`, masked with `mask`.
    fn client_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
        let mut frame = vec![0x80 + opcode];
        match payload.len() {
            n if n < 126 => frame.push(0x80 + u8::try_from(n).unwrap()),
            n if n <= 0xFFFF => {
                frame.push(0x80 + 126);
                frame.extend_from_slice(&u16::try_from(n).unwrap().to_be_bytes());
            }
            n => {
                frame.push(0x80 + 127);
                frame.extend_from_slice(&u64::try_from(n).unwrap().to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, k)| b ^ k));
        frame
    }

    fn read(bytes: &[u8]) -> Result<Frame, Error> {
        read_frame(&mut &bytes[..])
    }

    #[test]
    fn the_accept_key_is_rfc_6455s() {
        // RFC 6455, section 1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn a_masked_text_frame_is_read_as_its_message() {
        // RFC 6455, section 5.7: a masked "Hello".
        let hello = [
            0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
        ];
        assert_eq!(read(&hello).unwrap(), Frame::Text(b"Hello".to_vec()));
        // The 16- and 64-bit lengths, at the sizes that need them.
        for size in [126, 300, 0xFFFF, 0x1_0000, MAX_FRAME_BYTES] {
            let payload = vec![b'a'; size];
            let frame = client_frame(TEXT, &payload, [1, 2, 3, 4]);
            assert_eq!(read(&frame).unwrap(), Frame::Text(payload), "{size}");
        }
        assert_eq!(
            read(&client_frame(CLOSE, b"", [9; 4])).unwrap(),
            Frame::Close
        );
        assert_eq!(
            read(&client_frame(PING, b"hi", [9; 4])).unwrap(),
            Frame::Ping(b"hi".to_vec())
        );
        assert_eq!(read(&client_frame(PONG, b"", [9; 4])).unwrap(), Frame::Pong);
    }

    #[test]
    fn anything_but_a_whole_masked_bounded_text_or_control_frame_is_refused() {
        let refused = |bytes: Vec<u8>, why: &str| {
            assert!(
                matches!(read(&bytes), Err(Error::Frame(reason)) if reason == why),
                "{why}: {:?}",
                read(&bytes)
            );
        };
        let text = client_frame(TEXT, b"ok", [1, 2, 3, 4]);
        let mut fragment = text.clone();
        *fragment.first_mut().unwrap() = TEXT;
        refused(fragment, "a message split over frames");
        for reserved in [0x40, 0x20, 0x10] {
            let mut extended = text.clone();
            *extended.first_mut().unwrap() += reserved;
            refused(extended, "a reserved bit is set");
        }
        let mut unmasked = text;
        *unmasked.get_mut(1).unwrap() -= 0x80;
        refused(unmasked, "a client frame is not masked");
        refused(
            client_frame(TEXT, &vec![b'a'; MAX_FRAME_BYTES + 1], [1; 4]),
            "a frame is larger than allowed",
        );
        refused(
            client_frame(PING, &[0; MAX_CONTROL_BYTES + 1], [1; 4]),
            "a frame is larger than allowed",
        );
        refused(
            client_frame(0x2, b"bin", [1; 4]),
            "an opcode the host doesn't speak",
        );
        refused(
            client_frame(0x0, b"more", [1; 4]),
            "an opcode the host doesn't speak",
        );
        refused(
            client_frame(TEXT, &[0xFF, 0xFE], [1; 4]),
            "text that is not UTF-8",
        );
        // A 64-bit length past what this machine can hold is refused, not
        // allocated.
        let mut huge = vec![0x81, 0x80 + 127];
        huge.extend_from_slice(&u64::MAX.to_be_bytes());
        refused(huge, "a frame is larger than allowed");
        // A frame cut short is an I/O error, never a partial message.
        assert!(matches!(read(&[0x81, 0x85, 1]), Err(Error::Io(_))));
    }

    #[test]
    fn frames_written_to_the_browser_are_whole_unmasked_and_sized() {
        let written = |payload: &[u8]| {
            let mut out = Vec::new();
            write_text(&mut out, payload).unwrap();
            out
        };
        assert_eq!(written(b"Hi"), [0x81, 2, b'H', b'i']);
        let medium = written(&[b'x'; 200]);
        assert_eq!(medium.get(..4).unwrap(), [0x81, 126, 0, 200]);
        assert_eq!(medium.len(), 4 + 200);
        let long = written(&vec![b'x'; 0x1_0000]);
        assert_eq!(long.get(..10).unwrap(), [0x81, 127, 0, 0, 0, 0, 0, 1, 0, 0]);
        assert_eq!(written(&[b'x'; 125]).get(..2).unwrap(), [0x81, 125]);
        assert_eq!(
            written(&vec![b'x'; 0xFFFF]).get(..4).unwrap(),
            [0x81, 126, 0xFF, 0xFF]
        );
        let mut out = Vec::new();
        write_pong(&mut out, b"hi").unwrap();
        assert_eq!(out, [0x8A, 2, b'h', b'i']);
        let mut out = Vec::new();
        write_close(&mut out).unwrap();
        assert_eq!(out, [0x88, 0]);
    }
}
