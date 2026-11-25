//! A request's upstream connection, cut from another thread (#657).
//!
//! A forward blocks its thread until the upstream answers, and a model can
//! take minutes to. When whoever asked has gone (its run stopped, or its
//! clock ran out), nobody will read the answer, and the upstream would go
//! on making it: a local model computing it, a hosted one billing for it.
//! A [`Cut`] given to the forward lets another thread end it: the
//! connection is shut down both ways, so the forward's read fails at once
//! and the upstream sees its client gone.
//!
//! The broker cuts a session's link to the egress authority the same way
//! when the session's run ends, and the authority, seeing the link gone,
//! cuts the request's upstream.

use std::net::{Shutdown, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::{Error, Result};

/// What cuts a forward's upstream connection: shared by the thread that
/// forwards and the one that cuts. Once cut, it stays cut, and a
/// connection held after is cut as it's held.
#[derive(Debug, Clone, Default)]
pub struct Cut(Arc<Mutex<Held>>);

/// A [`Cut`]'s state.
#[derive(Debug, Default)]
struct Held {
    cut: bool,
    /// The connection in flight: another handle on its socket, whose
    /// shutdown is the socket's.
    upstream: Option<Box<dyn Socket>>,
}

/// A connection a [`Cut`] can hold: a stream socket, shut down both ways
/// through any handle on it.
pub trait Socket: Send + std::fmt::Debug + 'static {
    /// Another handle on the same socket.
    ///
    /// # Errors
    ///
    /// When the handle can't be made.
    fn another(&self) -> std::io::Result<Box<dyn Socket>>;

    /// Shuts the socket down both ways. One already closed is shut all
    /// the same.
    fn shut(&self);
}

impl Socket for TcpStream {
    fn another(&self) -> std::io::Result<Box<dyn Socket>> {
        Ok(Box::new(self.try_clone()?))
    }

    fn shut(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

#[cfg(unix)]
impl Socket for UnixStream {
    fn another(&self) -> std::io::Result<Box<dyn Socket>> {
        Ok(Box::new(self.try_clone()?))
    }

    fn shut(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

impl Cut {
    /// This state, whatever a thread that panicked left: a flag and a
    /// handle are whole at every step.
    fn held(&self) -> MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Cuts the connection held, and any held after.
    pub fn cut(&self) {
        let upstream = {
            let mut held = self.held();
            held.cut = true;
            held.upstream.take()
        };
        if let Some(upstream) = upstream {
            upstream.shut();
        }
    }

    /// Whether it's been cut.
    #[must_use]
    pub fn is_cut(&self) -> bool {
        self.held().cut
    }

    /// Holds `upstream`, the connection in flight, for a cut: in place of
    /// any held before.
    ///
    /// # Errors
    ///
    /// [`Error::UpstreamFailed`] when it was cut first (the connection is
    /// shut down before this returns, so nothing is sent on it), or the
    /// connection can't be held.
    pub fn hold(&self, upstream: &impl Socket) -> Result<()> {
        let handle = upstream.another().map_err(|_| Error::UpstreamFailed)?;
        let refused = {
            let mut held = self.held();
            if held.cut {
                Some(handle)
            } else {
                held.upstream = Some(handle);
                None
            }
        };
        refused.map_or(Ok(()), |handle| {
            handle.shut();
            Err(Error::UpstreamFailed)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::net::TcpListener;

    use super::*;

    /// How long a test's read waits: a cut that didn't happen fails the
    /// test, and doesn't hang it.
    const READ_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

    /// Both ends of a loopback connection: ours, and the upstream's.
    fn connected() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let ours = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (theirs, _) = listener.accept().unwrap();
        ours.set_read_timeout(Some(READ_WAIT)).unwrap();
        theirs.set_read_timeout(Some(READ_WAIT)).unwrap();
        (ours, theirs)
    }

    /// A cut shuts the connection held both ways: the upstream sees its
    /// client gone, and a read on ours ends. It stays cut.
    #[test]
    fn a_cut_shuts_the_connection_it_holds() {
        let (mut ours, mut theirs) = connected();
        let cut = Cut::default();
        cut.hold(&ours).unwrap();
        assert!(!cut.is_cut());
        cut.cut();
        assert!(cut.is_cut());
        let mut byte = [0_u8; 1];
        assert_eq!(theirs.read(&mut byte).unwrap(), 0, "the upstream saw it");
        assert_eq!(ours.read(&mut byte).unwrap(), 0, "our read ends");
        cut.cut();
        assert!(cut.is_cut(), "it stays cut");
    }

    /// A connection held after the cut is cut as it's held, and refused.
    /// A clone shares the cut.
    #[test]
    fn a_connection_held_after_the_cut_is_cut() {
        let (ours, mut theirs) = connected();
        let cut = Cut::default();
        let other = cut.clone();
        std::thread::spawn(move || other.cut()).join().unwrap();
        assert!(matches!(cut.hold(&ours), Err(Error::UpstreamFailed)));
        let mut byte = [0_u8; 1];
        assert_eq!(theirs.read(&mut byte).unwrap(), 0);
    }

    /// A unix stream is held and cut as a TCP connection is.
    #[cfg(unix)]
    #[test]
    fn a_unix_stream_is_cut_too() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.set_read_timeout(Some(READ_WAIT)).unwrap();
        let cut = Cut::default();
        cut.hold(&ours).unwrap();
        cut.cut();
        let mut byte = [0_u8; 1];
        assert_eq!(theirs.read(&mut byte).unwrap(), 0);
    }

    /// A connection no cut came for is left as it is.
    #[test]
    fn a_connection_not_cut_stays_open() {
        let (ours, theirs) = connected();
        let cut = Cut::default();
        cut.hold(&ours).unwrap();
        drop(cut);
        theirs
            .set_read_timeout(Some(std::time::Duration::from_millis(100)))
            .unwrap();
        let mut byte = [0_u8; 1];
        let waited = (&theirs).read(&mut byte).unwrap_err().kind();
        assert!(
            matches!(
                waited,
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "{waited:?}"
        );
        drop(ours);
    }
}
