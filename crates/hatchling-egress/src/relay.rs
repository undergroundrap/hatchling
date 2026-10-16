//! A streaming connection to a harness's vendor.
//!
//! The broker relays a real CLI's HTTP to its vendor's API byte for byte:
//! answers stream (server-sent events) and can run for minutes, so they are
//! never buffered whole the way [`crate::Proxy`] buffers a response.
//! [`open`] connects to one allowlisted destination, at the addresses
//! pinned for it at startup, never a fresh lookup:
//!
//! - an HTTPS destination over TLS, with the egress root store and the
//!   destination's own server name;
//! - a local endpoint (`local:localhost:PORT`) in plain TCP, to a pinned
//!   loopback address, the peer checked after connecting.
//!
//! A public source is refused: it never takes a credential. Framing, the
//! credential and the bounds are the relay's.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use rustls::StreamOwned;
use rustls::client::ClientConnection;
use rustls::pki_types::ServerName;

use crate::{Allowlist, Error, TlsClient};

/// A connection to a vendor, open for one request and its answer.
#[derive(Debug)]
pub enum Upstream {
    /// To an HTTPS destination.
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
    /// To a pinned local endpoint.
    Plain(TcpStream),
}

impl Read for Upstream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Tls(stream) => stream.read(buf),
            Self::Plain(stream) => stream.read(buf),
        }
    }
}

impl Write for Upstream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tls(stream) => stream.write(buf),
            Self::Plain(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tls(stream) => stream.flush(),
            Self::Plain(stream) => stream.flush(),
        }
    }
}

/// Opens a connection to `destination`, which `allowlist` must list as an
/// HTTPS destination or a local endpoint; each read and write is bounded
/// by `io_timeout`.
///
/// # Errors
///
/// [`Error::DestinationDenied`] for a destination the allowlist does not
/// list, a public source, or a peer that is not a pinned address;
/// [`Error::InvalidSurrogate`] for a destination that is no server name;
/// [`Error::UpstreamFailed`] when no pinned address answers or the TLS
/// connection cannot start.
pub fn open(
    allowlist: &Allowlist,
    tls: &TlsClient,
    destination: &str,
    io_timeout: Duration,
) -> Result<Upstream, Error> {
    if !allowlist.allows(destination) || allowlist.is_public(destination) {
        return Err(Error::DestinationDenied);
    }
    let timed = |tcp: TcpStream| -> Result<TcpStream, Error> {
        tcp.set_read_timeout(Some(io_timeout))
            .and_then(|()| tcp.set_write_timeout(Some(io_timeout)))
            .map_err(|_| Error::UpstreamFailed)?;
        Ok(tcp)
    };
    if let Some(local) = allowlist.local(destination) {
        return crate::local::connect(local.addrs())
            .and_then(timed)
            .map(Upstream::Plain);
    }
    let pinned = allowlist
        .addrs_for(destination)
        .ok_or(Error::DestinationDenied)?;
    let host = destination
        .rsplit_once(':')
        .map(|(host, _)| host)
        .ok_or(Error::InvalidSurrogate)?;
    let name = ServerName::try_from(host.to_owned()).map_err(|_| Error::InvalidSurrogate)?;
    let tcp = crate::https::connect(pinned).and_then(timed)?;
    let connection = tls.connection(name)?;
    Ok(Upstream::Tls(Box::new(StreamOwned::new(connection, tcp))))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;
    use crate::https::tests::{Ending, pki, serve};

    const SOON: Duration = Duration::from_secs(5);

    fn ask(upstream: &mut Upstream, request: &[u8]) -> Vec<u8> {
        upstream.write_all(request).unwrap();
        upstream.flush().unwrap();
        // Bounded: a reader that never reports the end must fail the test,
        // not spin it.
        let mut answer = Vec::new();
        let _ = upstream.take(64 * 1024).read_to_end(&mut answer);
        answer
    }

    #[test]
    fn a_local_endpoint_carries_a_request_and_its_answer_both_ways() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut got = [0_u8; 4];
            stream.read_exact(&mut got).unwrap();
            stream.write_all(b"pong").unwrap();
            got
        });
        let destination = format!("localhost:{port}");
        let allowlist = Allowlist::parse(&format!("local:{destination}")).unwrap();
        let mut upstream = open(
            &allowlist,
            &TlsClient::bundled().unwrap(),
            &destination,
            SOON,
        )
        .unwrap();
        assert!(matches!(upstream, Upstream::Plain(_)));
        assert_eq!(ask(&mut upstream, b"ping"), b"pong");
        assert_eq!(&server.join().unwrap(), b"ping");
    }

    #[test]
    fn an_https_destination_speaks_tls_to_its_own_name() {
        let vendor = pki("localhost");
        let (addr, head) = serve(
            &vendor,
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
            Ending::Notify,
        );
        // On this machine, so written internal: (WO-023), and still held to
        // verified TLS.
        let destination = format!("localhost:{}", addr.port());
        let allowlist = Allowlist::parse(&format!("internal:{destination}")).unwrap();
        let tls = TlsClient::with_operator_bundle(vendor.ca_pem.as_bytes()).unwrap();
        let mut upstream = open(&allowlist, &tls, &destination, SOON).unwrap();
        assert!(matches!(upstream, Upstream::Tls(_)));
        let answer = ask(&mut upstream, b"POST /v1/messages HTTP/1.1\r\n\r\n");
        assert!(
            answer.ends_with(b"\r\n\r\nok"),
            "{}",
            String::from_utf8_lossy(&answer)
        );
        assert!(
            head.recv_timeout(SOON)
                .unwrap()
                .starts_with("POST /v1/messages")
        );
        // Another name's certificate is refused at the handshake.
        let stranger = pki("vendor.example");
        let (addr, _) = serve(&stranger, Vec::new(), Ending::Drop);
        let destination = format!("localhost:{}", addr.port());
        let allowlist = Allowlist::parse(&format!("internal:{destination}")).unwrap();
        let tls = TlsClient::with_operator_bundle(stranger.ca_pem.as_bytes()).unwrap();
        let mut upstream = open(&allowlist, &tls, &destination, SOON).unwrap();
        assert!(
            upstream
                .write_all(b"GET / HTTP/1.1\r\n\r\n")
                .and_then(|()| upstream.flush())
                .is_err()
        );
    }

    #[test]
    fn only_a_listed_credentialed_destination_is_ever_opened() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let tls = TlsClient::bundled().unwrap();
        let listed = format!("localhost:{port}");
        // A public source can't be on this machine: it resolves, here, to a
        // documentation address.
        let public = Allowlist::parse_with("public:docs.example:443", |_| {
            Ok(vec!["203.0.113.5:443".parse().unwrap()])
        })
        .unwrap();
        for (allowlist, destination) in [(&public, "docs.example:443"), (&public, "localhost:1")] {
            assert!(matches!(
                open(allowlist, &tls, destination, SOON),
                Err(Error::DestinationDenied)
            ));
        }
        // A destination nothing listens on answers no connection.
        drop(listener);
        let local = Allowlist::parse(&format!("local:{listed}")).unwrap();
        assert!(matches!(
            open(&local, &tls, &listed, SOON),
            Err(Error::UpstreamFailed)
        ));
    }
}
