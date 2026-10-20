//! HTTPS to a credentialed or public destination: rustls, over a
//! connection to the allowlist's pinned addresses, read under the local
//! client's bounds.
//!
//! - **Pinned.** The connection goes to the addresses the allowlist
//!   resolved at startup, never to a fresh lookup, and its peer is checked
//!   against them before a byte is written: DNS cannot be rebound between
//!   the allowlist check and the connection.
//! - **One root store.** The bundled Mozilla roots (`webpki-roots`), plus
//!   the operator's own CA bundle when one is given. The system CA store
//!   is never read, and nothing is decrypted in the middle: the broker is
//!   the TLS client, end to end, and every root it trusts is a file the
//!   owner can read.
//! - **Bounded.** The request is framed and the response read by
//!   [`crate::local`]'s HTTP/1.1 code, under its bounds on every line, the
//!   head and the body. A body that runs to the close must end with the
//!   server's `close_notify`; one that just stops is a truncation, and
//!   fails.

use std::io::Write as _;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::proxy::{EgressRequest, EgressResponse};
use crate::{Error, Result};

/// How long the proxy waits to connect to an HTTPS destination.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long each read or write on an HTTPS connection may block.
pub const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// The proxy's TLS client: its one root store, built once.
#[derive(Clone)]
pub struct TlsClient {
    config: Arc<ClientConfig>,
    operator_roots: usize,
}

impl std::fmt::Debug for TlsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsClient")
            .field("bundled_roots", &webpki_roots::TLS_SERVER_ROOTS.len())
            .field("operator_roots", &self.operator_roots)
            .finish_non_exhaustive()
    }
}

impl TlsClient {
    /// The bundled Mozilla roots, and nothing else.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidTrustBundle`] if the TLS configuration
    /// cannot be built (defensive: the bundled set always builds).
    pub fn bundled() -> Result<Self> {
        Self::build(Vec::new())
    }

    /// The bundled roots and every certificate in the operator's PEM
    /// `bundle`: hatchling's own CA, or a local service's.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidTrustBundle`] for a bundle that is not PEM,
    /// holds no certificate, or holds one rustls cannot use as a root.
    pub fn with_operator_bundle(bundle: &[u8]) -> Result<Self> {
        let invalid = |reason: String| Error::InvalidTrustBundle { reason };
        let certs = CertificateDer::pem_slice_iter(bundle)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| invalid(format!("not a PEM certificate bundle: {error}")))?;
        if certs.is_empty() {
            return Err(invalid(String::from("the bundle holds no certificate")));
        }
        Self::build(certs)
    }

    fn build(operator: Vec<CertificateDer<'static>>) -> Result<Self> {
        let invalid = |reason: String| Error::InvalidTrustBundle { reason };
        let mut roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let operator_roots = operator.len();
        for cert in operator {
            roots
                .add(cert)
                .map_err(|error| invalid(format!("a certificate is not a usable root: {error}")))?;
        }
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(|error| invalid(error.to_string()))?
                .with_root_certificates(roots)
                .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
            operator_roots,
        })
    }

    /// How many roots the operator's bundle added.
    #[must_use]
    pub const fn operator_roots(&self) -> usize {
        self.operator_roots
    }

    /// A client connection to `name`, under this root store.
    pub(crate) fn connection(&self, name: ServerName<'static>) -> Result<ClientConnection> {
        ClientConnection::new(Arc::clone(&self.config), name).map_err(|error| {
            tracing::warn!(%error, "egress TLS connection could not start");
            Error::UpstreamFailed
        })
    }
}

/// Forwards `request` over HTTPS to one of `pinned`, the destination's
/// pinned addresses, with `credential` as the bearer if it has one.
///
/// The request is framed before connecting, so one that cannot be sent
/// never opens a connection.
///
/// # Errors
///
/// [`Error::InvalidSurrogate`] for a request that cannot be framed safely
/// or a destination that is no server name; [`Error::DestinationDenied`]
/// for a peer that is not a pinned address; [`Error::UpstreamFailed`] when
/// no pinned address answers, the handshake or certificate check fails,
/// or the response is malformed or truncated; [`Error::ResponseTooLarge`]
/// past the body bound.
pub(crate) fn forward(
    tls: &TlsClient,
    pinned: &[SocketAddr],
    request: &EgressRequest,
    credential: Option<&[u8]>,
) -> Result<EgressResponse> {
    let bytes = crate::local::request_bytes(request, credential)?;
    send(tls, pinned, &request.destination, &bytes)
}

/// Sends the framed request `bytes` over HTTPS to one of `pinned`, the
/// addresses of `destination` (`host:port`), and reads the answer.
///
/// # Errors
///
/// As [`forward`].
pub(crate) fn send(
    tls: &TlsClient,
    pinned: &[SocketAddr],
    destination: &str,
    bytes: &[u8],
) -> Result<EgressResponse> {
    let host = destination
        .rsplit_once(':')
        .map(|(host, _)| host)
        .ok_or(Error::InvalidSurrogate)?;
    let name = ServerName::try_from(host.to_owned()).map_err(|_| Error::InvalidSurrogate)?;
    let tcp = connect(pinned)?;
    tcp.set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| tcp.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|_| Error::UpstreamFailed)?;
    let connection = ClientConnection::new(Arc::clone(&tls.config), name).map_err(|error| {
        tracing::warn!(%error, "egress TLS connection could not start");
        Error::UpstreamFailed
    })?;
    let mut stream = StreamOwned::new(connection, tcp);
    stream
        .write_all(bytes)
        .and_then(|()| stream.flush())
        .map_err(|error| {
            tracing::warn!(%error, %destination, "egress TLS request failed");
            Error::UpstreamFailed
        })?;
    crate::local::read_response(stream)
}

/// Connects to the first pinned address that answers, then checks that
/// the kernel connected to one of them before handing it back.
pub(crate) fn connect(pinned: &[SocketAddr]) -> Result<TcpStream> {
    for addr in pinned {
        let Ok(stream) = TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) else {
            continue;
        };
        let peer = stream.peer_addr().map_err(|_| Error::UpstreamFailed)?;
        if !pinned.contains(&peer) {
            tracing::warn!(peer = %peer, "egress peer is not a pinned address");
            return Err(Error::DestinationDenied);
        }
        return Ok(stream);
    }
    tracing::warn!("egress destination unreachable at its pinned addresses");
    Err(Error::UpstreamFailed)
}

#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the proxy's tests share this module's in-memory certificates"
)]
pub(crate) mod tests {
    use std::io::Read as _;
    use std::net::TcpListener;
    use std::sync::mpsc;

    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

    use super::*;
    use crate::MAX_RESPONSE_BYTES;

    /// A throwaway CA, made fresh for this test, and a server certificate
    /// it signs for `name`: no key is ever committed. Built here as DER and
    /// signed with ring, so the tests need no certificate crate.
    pub(crate) struct Pki {
        pub(crate) ca_pem: String,
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
    }

    use ring::rand::SystemRandom;
    use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};

    // The OIDs, DER-encoded: the algorithms, the subject's common name,
    // and the four extensions a CA and a server certificate need.
    const OID_ECDSA_SHA256: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
    const OID_EC_PUBLIC_KEY: &[u8] = &[0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
    const OID_P256: &[u8] = &[0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
    const OID_COMMON_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x03];
    const OID_BASIC_CONSTRAINTS: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x13];
    const OID_KEY_USAGE: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x0f];
    const OID_EXT_KEY_USAGE: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x25];
    const OID_SUBJECT_ALT_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x1d, 0x11];
    const OID_SERVER_AUTH: &[u8] = &[0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

    /// One DER element: `tag`, its definite length, `content`.
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let length = content.len();
        let mut out = vec![tag];
        if length < 0x80 {
            out.push(u8::try_from(length).unwrap());
        } else {
            let digits: Vec<u8> = length
                .to_be_bytes()
                .into_iter()
                .skip_while(|byte| *byte == 0)
                .collect();
            out.push(0x80 | u8::try_from(digits.len()).unwrap());
            out.extend(digits);
        }
        out.extend_from_slice(content);
        out
    }

    fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
        tlv(0x30, &parts.concat())
    }

    /// A name with one common name.
    fn name(common: &str) -> Vec<u8> {
        let attribute = seq(&[OID_COMMON_NAME.to_vec(), tlv(0x0c, common.as_bytes())]);
        seq(&[tlv(0x31, &attribute)])
    }

    fn extension(oid: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
        let mut parts = vec![oid.to_vec()];
        if critical {
            parts.push(vec![0x01, 0x01, 0xff]);
        }
        parts.push(tlv(0x04, value));
        seq(&parts)
    }

    /// A v3 certificate for `subject`'s public key, issued by `issuer` and
    /// signed by its key, valid from 2020 to 2120.
    fn certificate(
        serial: u8,
        issuer: &str,
        subject: &str,
        public_key: &[u8],
        extensions: &[Vec<u8>],
        signer: &EcdsaKeyPair,
    ) -> Vec<u8> {
        let algorithm = seq(&[OID_ECDSA_SHA256.to_vec()]);
        let key_info = seq(&[
            seq(&[OID_EC_PUBLIC_KEY.to_vec(), OID_P256.to_vec()]),
            tlv(0x03, &[&[0_u8][..], public_key].concat()),
        ]);
        let validity = seq(&[tlv(0x17, b"200101000000Z"), tlv(0x18, b"21200101000000Z")]);
        let tbs = seq(&[
            tlv(0xa0, &[0x02, 0x01, 0x02]),
            tlv(0x02, &[serial]),
            algorithm.clone(),
            name(issuer),
            validity,
            name(subject),
            key_info,
            tlv(0xa3, &seq(extensions)),
        ]);
        let signature = signer.sign(&SystemRandom::new(), &tbs).unwrap();
        seq(&[
            tbs,
            algorithm,
            tlv(0x03, &[&[0_u8][..], signature.as_ref()].concat()),
        ])
    }

    /// A fresh P-256 key: its PKCS#8 document and the key pair.
    fn key() -> (Vec<u8>, EcdsaKeyPair) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        (pkcs8.as_ref().to_vec(), pair)
    }

    /// `der` as one PEM certificate.
    fn pem(der: &[u8]) -> String {
        use base64::Engine as _;
        let text = base64::engine::general_purpose::STANDARD.encode(der);
        let lines: Vec<&str> = text
            .as_bytes()
            .chunks(64)
            .map(|line| std::str::from_utf8(line).unwrap())
            .collect();
        format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
            lines.join("\n")
        )
    }

    pub(crate) fn pki(name: &str) -> Pki {
        const CA: &str = "hatchling test CA";
        let (_, ca_key) = key();
        let ca = certificate(
            1,
            CA,
            CA,
            ca_key.public_key().as_ref(),
            &[
                extension(OID_BASIC_CONSTRAINTS, true, &seq(&[vec![0x01, 0x01, 0xff]])),
                extension(OID_KEY_USAGE, true, &[0x03, 0x02, 0x01, 0x06]),
            ],
            &ca_key,
        );
        let (leaf_pkcs8, leaf_key) = key();
        let leaf = certificate(
            2,
            CA,
            name,
            leaf_key.public_key().as_ref(),
            &[
                extension(OID_BASIC_CONSTRAINTS, true, &seq(&[])),
                extension(OID_KEY_USAGE, true, &[0x03, 0x02, 0x07, 0x80]),
                extension(OID_EXT_KEY_USAGE, false, &seq(&[OID_SERVER_AUTH.to_vec()])),
                extension(
                    OID_SUBJECT_ALT_NAME,
                    false,
                    &seq(&[tlv(0x82, name.as_bytes())]),
                ),
            ],
            &ca_key,
        );
        Pki {
            ca_pem: pem(&ca),
            chain: vec![CertificateDer::from(leaf)],
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_pkcs8)),
        }
    }

    /// How the test server ends its answer.
    #[derive(Clone, Copy)]
    pub(crate) enum Ending {
        /// With TLS `close_notify`.
        Notify,
        /// By dropping the connection: a truncation, to the client.
        Drop,
    }

    /// A TLS server on 127.0.0.1 with `pki`'s certificate, answering one
    /// request with `answer`. It sends back the request head it read.
    pub(crate) fn serve(
        pki: &Pki,
        answer: Vec<u8>,
        ending: Ending,
    ) -> (SocketAddr, mpsc::Receiver<String>) {
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(pki.chain.clone(), pki.key.clone_key())
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (heads, head) = mpsc::channel();
        std::thread::spawn(move || {
            let Ok((tcp, _)) = listener.accept() else {
                return;
            };
            tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut tls = StreamOwned::new(connection, tcp);
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                match tls.read(&mut byte) {
                    Ok(1) => request.extend_from_slice(&byte),
                    // A refused handshake, or the client gave up.
                    Ok(_) | Err(_) => return,
                }
            }
            let _ = heads.send(String::from_utf8_lossy(&request).into_owned());
            let _ = tls.write_all(&answer);
            match ending {
                Ending::Notify => {
                    tls.conn.send_close_notify();
                    let _ = tls.flush();
                }
                Ending::Drop => {
                    let _ = tls.flush();
                }
            }
        });
        (addr, head)
    }

    fn get(destination: &str) -> EgressRequest {
        EgressRequest {
            method: String::from("GET"),
            destination: destination.to_owned(),
            path: String::from("/v1/models"),
            headers: vec![(String::from("accept"), String::from("application/json"))],
            body: Vec::new(),
            surrogate_b64: String::new(),
        }
    }

    fn trusting(pki: &Pki) -> TlsClient {
        TlsClient::with_operator_bundle(pki.ca_pem.as_bytes()).unwrap()
    }

    #[test]
    fn a_connection_starts_its_handshake_under_the_clients_roots() {
        let tls = TlsClient::bundled().unwrap();
        let name = ServerName::try_from("vendor.example").unwrap();
        let connection = tls.connection(name).unwrap();
        assert!(connection.is_handshaking());
        assert!(connection.wants_write(), "the client speaks first");
    }

    #[test]
    fn a_request_reaches_a_server_the_operator_trusts_with_its_credential() {
        let pki = pki("localhost");
        let (addr, head) = serve(
            &pki,
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec(),
            Ending::Notify,
        );
        let destination = format!("localhost:{}", addr.port());
        let response = forward(
            &trusting(&pki),
            &[addr],
            &get(&destination),
            Some(b"secret-key"),
        )
        .unwrap();
        assert_eq!(
            (response.status, response.body.as_slice()),
            (200, b"hello".as_slice())
        );
        let head = head.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(head.starts_with("GET /v1/models HTTP/1.1\r\n"), "{head}");
        let host = format!("\r\nHost: {destination}\r\n");
        assert!(head.contains(&host), "{head}");
        assert!(
            head.contains("\r\nAuthorization: Bearer secret-key\r\n"),
            "{head}"
        );
        assert!(head.contains("\r\naccept: application/json\r\n"), "{head}");
    }

    #[test]
    fn the_bundled_roots_alone_refuse_a_private_ca() {
        let pki = pki("localhost");
        let (addr, head) = serve(&pki, b"HTTP/1.1 200 OK\r\n\r\n".to_vec(), Ending::Notify);
        let destination = format!("localhost:{}", addr.port());
        let bundled = TlsClient::bundled().unwrap();
        assert_eq!(bundled.operator_roots(), 0);
        assert!(matches!(
            forward(&bundled, &[addr], &get(&destination), None),
            Err(Error::UpstreamFailed)
        ));
        assert!(
            head.recv_timeout(Duration::from_secs(5)).is_err(),
            "no request reached it"
        );
    }

    #[test]
    fn a_certificate_for_another_name_is_refused() {
        let pki = pki("wrong.test");
        let (addr, head) = serve(&pki, b"HTTP/1.1 200 OK\r\n\r\n".to_vec(), Ending::Notify);
        let destination = format!("localhost:{}", addr.port());
        assert!(matches!(
            forward(&trusting(&pki), &[addr], &get(&destination), None),
            Err(Error::UpstreamFailed)
        ));
        assert!(
            head.recv_timeout(Duration::from_secs(5)).is_err(),
            "no request reached it"
        );
    }

    #[test]
    fn a_body_that_runs_to_the_close_needs_close_notify() {
        let pki = pki("localhost");
        let to_close = b"HTTP/1.1 200 OK\r\n\r\nall of it".to_vec();
        let (addr, _) = serve(&pki, to_close.clone(), Ending::Notify);
        let destination = format!("localhost:{}", addr.port());
        let response = forward(&trusting(&pki), &[addr], &get(&destination), None).unwrap();
        assert_eq!(response.body, b"all of it");
        let (addr, _) = serve(&pki, to_close, Ending::Drop);
        let destination = format!("localhost:{}", addr.port());
        assert!(
            matches!(
                forward(&trusting(&pki), &[addr], &get(&destination), None),
                Err(Error::UpstreamFailed)
            ),
            "a connection that just stops is a truncation"
        );
    }

    #[test]
    fn a_response_past_the_bound_is_refused() {
        let pki = pki("localhost");
        let mut answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            MAX_RESPONSE_BYTES + 1
        )
        .into_bytes();
        answer.extend(std::iter::repeat_n(b'x', MAX_RESPONSE_BYTES + 1));
        let (addr, _) = serve(&pki, answer, Ending::Notify);
        let destination = format!("localhost:{}", addr.port());
        assert!(matches!(
            forward(&trusting(&pki), &[addr], &get(&destination), None),
            Err(Error::ResponseTooLarge)
        ));
    }

    #[test]
    fn only_the_pinned_addresses_are_dialed() {
        let pki = pki("localhost");
        let (addr, head) = serve(&pki, b"HTTP/1.1 200 OK\r\n\r\n".to_vec(), Ending::Notify);
        // The destination names the live server, but its pin is a port
        // nothing listens on: the proxy never looks the name up again.
        let closed = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let destination = format!("localhost:{}", addr.port());
        assert!(matches!(
            forward(&trusting(&pki), &[closed], &get(&destination), None),
            Err(Error::UpstreamFailed)
        ));
        assert!(
            head.recv_timeout(Duration::from_millis(300)).is_err(),
            "the live server was never dialed"
        );
        assert!(matches!(connect(&[]), Err(Error::UpstreamFailed)));
        assert_eq!(CONNECT_TIMEOUT, Duration::from_secs(5));
        assert_eq!(IO_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn a_destination_that_names_no_server_is_refused_before_connecting() {
        let client = TlsClient::bundled().unwrap();
        let nowhere = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        for destination in ["no-port", "[::1]:443"] {
            assert!(
                matches!(
                    forward(&client, &[nowhere], &get(destination), None),
                    Err(Error::InvalidSurrogate)
                ),
                "{destination}"
            );
        }
    }

    #[test]
    fn the_root_store_is_the_bundled_set_plus_the_operators() {
        assert_eq!(TlsClient::build(Vec::new()).unwrap().operator_roots(), 0);
        let pki = pki("localhost");
        let ca = CertificateDer::from_pem_slice(pki.ca_pem.as_bytes()).unwrap();
        assert_eq!(TlsClient::build(vec![ca]).unwrap().operator_roots(), 1);
        assert!(matches!(
            TlsClient::build(vec![CertificateDer::from(vec![0_u8; 3])]),
            Err(Error::InvalidTrustBundle { .. })
        ));
    }

    #[test]
    fn an_operator_bundle_must_hold_usable_certificates() {
        let pki = pki("localhost");
        let client = trusting(&pki);
        assert_eq!(client.operator_roots(), 1);
        let shown = format!("{client:?}");
        assert!(shown.contains("operator_roots: 1"), "{shown}");
        let two = format!("{}{}", pki.ca_pem, self::pki("x.test").ca_pem);
        assert_eq!(
            TlsClient::with_operator_bundle(two.as_bytes())
                .unwrap()
                .operator_roots(),
            2
        );
        for (bundle, why) in [
            (&b""[..], "holds no certificate"),
            (b"not a certificate at all", "holds no certificate"),
            (
                b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n",
                "not a PEM certificate bundle",
            ),
            (
                b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
                "not a usable root",
            ),
        ] {
            let refused = TlsClient::with_operator_bundle(bundle);
            assert!(
                matches!(&refused, Err(Error::InvalidTrustBundle { reason }) if reason.contains(why)),
                "{why}: {refused:?}"
            );
        }
    }
}
