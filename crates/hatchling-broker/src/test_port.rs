//! A local TCP port that refuses every connection, and that no other test
//! can take while it's held.
//!
//! Binding a listener and dropping it, to learn a port nothing listens
//! on, races: the kernel gives that port to the next test that binds one,
//! and a connection meant to be refused reaches that test's server. Two
//! browser-proxy tests failed together so on main: one was answered, and
//! the other's site counted a visitor it never had. A socket bound and
//! never listening holds its port, and the kernel refuses a connection
//! to it.
#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-visible on purpose: test modules across the crate share it"
)]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};

/// A port on 127.0.0.1 that refuses connections until this drops.
pub(crate) struct RefusingPort {
    /// The bound socket. It never listens: the type is only for its
    /// address.
    _held: TcpListener,
    addr: SocketAddr,
}

impl RefusingPort {
    pub(crate) fn new() -> Self {
        use rustix::net::{AddressFamily, SocketType, bind_v4, socket};
        let bound = socket(AddressFamily::INET, SocketType::STREAM, None).unwrap();
        bind_v4(&bound, &SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let held = TcpListener::from(bound);
        let addr = held.local_addr().unwrap();
        Self { _held: held, addr }
    }

    /// Its address.
    pub(crate) const fn addr(&self) -> SocketAddr {
        self.addr
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpStream;

    use super::*;

    #[test]
    fn it_refuses_and_keeps_its_port() {
        let refusing = RefusingPort::new();
        let refused = TcpStream::connect(refusing.addr()).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::ConnectionRefused);
        // Nobody else may have it meanwhile.
        assert!(TcpListener::bind(refusing.addr()).is_err());
    }
}
