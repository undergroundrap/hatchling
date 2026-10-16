//! The bridge: one browser's WebSocket and one connection to the broker's
//! UI socket, each message carried across unchanged.
//!
//! A text frame from the browser becomes one length-prefixed frame to the
//! broker, and each frame from the broker one text frame to the browser.
//! Pings are answered here. Whichever side ends, the other is closed. The
//! broker decides everything about what it receives; the bridge reads
//! nothing in a message.

use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use hatchling_core::framing::{read_frame_or_eof, write_frame};

use crate::Error;
use crate::websocket::{self, Frame};

/// Carries `browser`'s messages to a new connection to `ui_socket`, and
/// its answers back, until either side ends.
///
/// # Errors
///
/// [`Error::Io`] when the broker can't be reached or the browser's side
/// fails, [`Error::Frame`] for a frame the host refuses, and
/// [`Error::Broker`] when a message can't be framed to the broker.
pub fn bridge(browser: TcpStream, ui_socket: &Path) -> Result<(), Error> {
    let broker = UnixStream::connect(ui_socket)?;
    let to_browser = Mutex::new(browser.try_clone()?);
    let from_broker = broker.try_clone()?;
    let mut from_browser = browser;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while let Ok(Some(message)) = read_frame_or_eof(&from_broker) {
                if websocket::write_text(&mut *lock(&to_browser), &message).is_err() {
                    break;
                }
            }
            let mut out = lock(&to_browser);
            let _ = websocket::write_close(&mut *out);
            let _ = out.shutdown(Shutdown::Both);
        });
        let carried = carry(&mut from_browser, &broker, &to_browser);
        // The broker's side ends with the browser's, which ends the
        // thread above.
        let _ = broker.shutdown(Shutdown::Both);
        carried
    })
}

/// The browser's frames, carried to `broker` until it closes.
fn carry(
    from_browser: &mut TcpStream,
    broker: &UnixStream,
    to_browser: &Mutex<TcpStream>,
) -> Result<(), Error> {
    loop {
        match websocket::read_frame(from_browser)? {
            Frame::Text(message) => write_frame(broker, &message)?,
            Frame::Ping(payload) => websocket::write_pong(&mut *lock(to_browser), &payload)?,
            Frame::Pong => {}
            Frame::Close => return Ok(()),
        }
    }
}

/// The browser's writer. A panic while holding it leaves a stream, not a
/// broken invariant, so a poisoned lock is still taken.
fn lock(writer: &Mutex<TcpStream>) -> MutexGuard<'_, TcpStream> {
    writer.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{Ipv4Addr, TcpListener};

    use super::*;

    /// A connected pair of TCP streams on loopback: (the browser's, ours).
    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let browser = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (ours, _) = listener.accept().unwrap();
        (browser, ours)
    }

    #[test]
    fn carry_moves_text_answers_pings_and_stops_at_close() {
        let (mut browser, ours) = tcp_pair();
        let (broker, broker_peer) = UnixStream::pair().unwrap();
        // A masked "hi", a masked ping "p", a pong, then a close.
        browser
            .write_all(&[
                0x81, 0x82, 0, 0, 0, 0, b'h', b'i', 0x89, 0x81, 0, 0, 0, 0, b'p', 0x8A, 0x80, 0, 0,
                0, 0, 0x88, 0x80, 0, 0, 0, 0,
            ])
            .unwrap();
        let writer = Mutex::new(ours.try_clone().unwrap());
        assert!(lock(&writer).peer_addr().is_ok());
        let mut reader = ours;
        assert!(carry(&mut reader, &broker, &writer).is_ok());
        assert_eq!(
            hatchling_core::framing::read_frame(&broker_peer).unwrap(),
            b"hi"
        );
        let mut pong = [0_u8; 3];
        browser.read_exact(&mut pong).unwrap();
        assert_eq!(pong, [0x8A, 1, b'p']);
        // A frame the host refuses ends the carry with its reason.
        browser.write_all(&[0x82, 0x80, 0, 0, 0, 0]).unwrap();
        assert!(matches!(
            carry(&mut reader, &broker, &writer),
            Err(Error::Frame(_))
        ));
    }
}
