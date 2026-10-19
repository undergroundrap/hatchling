//! Plain HTTP to a local model endpoint, over a connection
//! the proxy has verified is loopback.
//!
//! "Local" is a claim, not a fact. The allowlist pins a local
//! endpoint's `localhost` to the loopback addresses it resolved to at
//! startup (see [`crate::allowlist`]); this module connects only to
//! those pinned addresses, never to a fresh lookup, and then checks
//! the connection itself: connect, then verify, with the peer the
//! kernel reports (`getpeername`) required to be a pinned loopback
//! address before a single request byte is written. Resolving and then
//! connecting would leave a window between the check and the use.
//!
//! The client is deliberately small: one HTTP/1.1 request per
//! connection (`Connection: close`), `GET` or `POST` only, and a
//! response read under a bound on every line, the head, and the body,
//! whether the body is length-delimited, chunked, or runs to the close.
//! Anything it does not understand fails closed.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use zeroize::Zeroizing;

use crate::allowlist::is_pinned_loopback;
use crate::proxy::{EgressRequest, EgressResponse, MAX_RESPONSE_BYTES};
use crate::{Error, Result};

/// How long the proxy waits to connect to a local endpoint.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the proxy waits on a local endpoint's reads and writes.
/// Longer than the cloud path's: a model on a desktop CPU may think
/// for minutes. Still a bound, so the session is never held forever.
pub const IO_TIMEOUT: Duration = Duration::from_secs(300);

/// The longest status, header, or chunk-size line accepted.
pub const MAX_LINE_BYTES: usize = 8 * 1024;

/// The largest response head (status line and headers) accepted.
pub const MAX_HEAD_BYTES: usize = 32 * 1024;

/// Request headers the proxy writes itself: the agent may not frame
/// the request.
const FRAMING_HEADERS: [&str; 3] = ["content-length", "transfer-encoding", "connection"];

/// Request headers that would let the response be compressed. The proxy
/// drops them and asks for `identity` itself, so the response can be
/// scrubbed (see [`crate::scrub`]).
const UNCOMPRESSED_ONLY: [&str; 2] = ["accept-encoding", "te"];

/// Forwards `request` to the local endpoint pinned at `pinned`, with
/// `credential` as the bearer if it has one.
///
/// The request is built before connecting, so a request that cannot
/// be sent never opens a connection.
///
/// # Errors
///
/// [`Error::DestinationDenied`] for a method other than `GET` or
/// `POST`, or a peer that is not a pinned loopback address;
/// [`Error::InvalidSurrogate`] for a request that cannot be framed
/// safely; [`Error::UpstreamFailed`] when no pinned address answers or
/// the response is malformed; [`Error::ResponseTooLarge`] past the
/// body bound.
pub(crate) fn forward(
    pinned: &[SocketAddr],
    request: &EgressRequest,
    credential: Option<&[u8]>,
) -> Result<EgressResponse> {
    if request.method != "GET" && request.method != "POST" {
        tracing::warn!(method = %request.method, "local model endpoint method denied");
        return Err(Error::DestinationDenied);
    }
    let bytes = request_bytes(request, credential)?;
    let mut stream = connect(pinned)?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|_| Error::UpstreamFailed)?;
    stream
        .write_all(&bytes)
        .map_err(|_| Error::UpstreamFailed)?;
    read_response(stream)
}

/// Connects to the first pinned address that answers, then verifies
/// the connection's actual peer before handing it back.
pub(crate) fn connect(pinned: &[SocketAddr]) -> Result<TcpStream> {
    for addr in pinned {
        let Ok(stream) = TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) else {
            continue;
        };
        // Connect, then verify: the peer the kernel connected to, not
        // the address the proxy meant to reach.
        let peer = stream.peer_addr().map_err(|_| Error::UpstreamFailed)?;
        verify_peer(&peer, pinned)?;
        return Ok(stream);
    }
    tracing::warn!("local model endpoint unreachable");
    Err(Error::UpstreamFailed)
}

/// A connection's peer must be loopback, and one of the addresses the
/// endpoint was pinned to.
fn verify_peer(peer: &SocketAddr, pinned: &[SocketAddr]) -> Result<()> {
    if is_pinned_loopback(peer)
        && pinned
            .iter()
            .any(|addr| addr.ip() == peer.ip() && addr.port() == peer.port())
    {
        Ok(())
    } else {
        tracing::warn!(peer = %peer, "local model endpoint peer is not a pinned loopback address");
        Err(Error::DestinationDenied)
    }
}

/// Frames the request: the request line, the proxy's own `Host`,
/// `Connection` and `Content-Length`, the credential if any, the
/// agent's headers, and the body.
///
/// The credential is written once, into a buffer allocated at the
/// request's exact size and wiped when dropped: no copy of it is left in
/// freed memory, by a reallocation or otherwise.
pub(crate) fn request_bytes(
    request: &EgressRequest,
    credential: Option<&[u8]>,
) -> Result<Zeroizing<Vec<u8>>> {
    if request
        .path
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(Error::InvalidSurrogate);
    }
    let mut head = String::new();
    let _ = write!(
        head,
        "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept-Encoding: identity\r\nContent-Length: {}\r\n",
        request.method,
        request.path,
        request.destination,
        request.body.len()
    );
    if let Some(credential) = credential {
        if !std::str::from_utf8(credential).is_ok_and(is_field_value) {
            tracing::warn!("egress credential cannot travel in a header");
            return Err(Error::UpstreamFailed);
        }
    }
    let mut tail = String::new();
    for (name, value) in &request.headers {
        if UNCOMPRESSED_ONLY
            .iter()
            .any(|chosen| name.eq_ignore_ascii_case(chosen))
        {
            continue;
        }
        if !is_token(name)
            || !is_field_value(value)
            || FRAMING_HEADERS
                .iter()
                .any(|framing| name.eq_ignore_ascii_case(framing))
        {
            return Err(Error::InvalidSurrogate);
        }
        let _ = write!(tail, "{name}: {value}\r\n");
    }
    tail.push_str("\r\n");
    let credential = credential.map(|credential| [AUTHORIZATION, credential, CRLF]);
    let size = head.len()
        + credential.map_or(0, |line| line.iter().map(|part| part.len()).sum())
        + tail.len()
        + request.body.len();
    let mut bytes = Zeroizing::new(Vec::with_capacity(size));
    bytes.extend_from_slice(head.as_bytes());
    for part in credential.iter().flatten() {
        bytes.extend_from_slice(part);
    }
    bytes.extend_from_slice(tail.as_bytes());
    bytes.extend_from_slice(&request.body);
    Ok(bytes)
}

/// The credential's header, up to the credential itself.
const AUTHORIZATION: &[u8] = b"Authorization: Bearer ";

/// A header line's end.
const CRLF: &[u8] = b"\r\n";

/// An HTTP token (a header name): non-empty, visible, no separators.
fn is_token(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// A header value: no control character but tab, so no line break can
/// smuggle in a header or a second request.
fn is_field_value(value: &str) -> bool {
    !value
        .bytes()
        .any(|byte| byte.is_ascii_control() && byte != b'\t')
}

/// How a response's body is delimited.
#[derive(Debug, PartialEq, Eq)]
enum Framing {
    Length(usize),
    Chunked,
    ToClose,
}

/// Reads one response under its bounds.
pub(crate) fn read_response(reader: impl Read) -> Result<EgressResponse> {
    let mut reader = BufReader::new(reader);
    let status_line = read_line(&mut reader)?;
    let status = parse_status(&status_line)?;
    let mut headers = Vec::new();
    let mut head_bytes = status_line.len();
    loop {
        let line = read_line(&mut reader)?;
        if line.is_empty() {
            break;
        }
        head_bytes += line.len();
        if head_bytes > MAX_HEAD_BYTES {
            return Err(Error::UpstreamFailed);
        }
        headers.push(parse_header(&line)?);
    }
    // A compressed body would hide an echoed credential from the scrub.
    if headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("content-encoding") && !crate::scrub::is_identity(value)
    }) {
        tracing::warn!("the upstream sent a compressed response, which can't be scrubbed");
        return Err(Error::UpstreamFailed);
    }
    let body = match framing(&headers)? {
        Framing::Length(length) => read_length(&mut reader, length)?,
        Framing::Chunked => read_chunked(&mut reader)?,
        Framing::ToClose => read_to_close(&mut reader)?,
    };
    Ok(EgressResponse {
        status,
        headers,
        body,
    })
}

/// Reads one CRLF-terminated line of at most [`MAX_LINE_BYTES`],
/// without its terminator. A longer line, a bare LF, or the stream
/// ending first is malformed.
fn read_line(reader: &mut impl BufRead) -> Result<String> {
    let mut line = Vec::new();
    let limit = u64::try_from(MAX_LINE_BYTES + 2).map_err(|_| Error::UpstreamFailed)?;
    reader
        .take(limit)
        .read_until(b'\n', &mut line)
        .map_err(|_| Error::UpstreamFailed)?;
    let line = line.strip_suffix(b"\r\n").ok_or(Error::UpstreamFailed)?;
    String::from_utf8(line.to_vec()).map_err(|_| Error::UpstreamFailed)
}

/// `HTTP/1.x NNN reason` → `NNN`.
fn parse_status(line: &str) -> Result<u16> {
    let mut parts = line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.")
        || code.len() != 3
        || !code.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(Error::UpstreamFailed);
    }
    code.parse().map_err(|_| Error::UpstreamFailed)
}

/// `Name: value` → (`Name`, `value`), the value trimmed.
fn parse_header(line: &str) -> Result<(String, String)> {
    let (name, value) = line.split_once(':').ok_or(Error::UpstreamFailed)?;
    if !is_token(name) {
        return Err(Error::UpstreamFailed);
    }
    Ok((name.to_owned(), value.trim().to_owned()))
}

/// The body's framing. Chunked is the only transfer coding accepted,
/// and a response that frames itself two ways, or gives two lengths,
/// is refused rather than guessed at.
fn framing(headers: &[(String, String)]) -> Result<Framing> {
    let named = |wanted: &str| {
        headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>()
    };
    let codings = named("transfer-encoding");
    let lengths = named("content-length");
    match (codings.as_slice(), lengths.as_slice()) {
        ([], []) => Ok(Framing::ToClose),
        ([], [length]) => {
            if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Error::UpstreamFailed);
            }
            length
                .parse()
                .map(Framing::Length)
                .map_err(|_| Error::UpstreamFailed)
        }
        ([coding], []) if coding.eq_ignore_ascii_case("chunked") => Ok(Framing::Chunked),
        _ => Err(Error::UpstreamFailed),
    }
}

/// A length-delimited body, refused before reading if over the bound.
fn read_length(reader: &mut impl Read, length: usize) -> Result<Vec<u8>> {
    if length > MAX_RESPONSE_BYTES {
        return Err(Error::ResponseTooLarge);
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| Error::UpstreamFailed)?;
    Ok(body)
}

/// A chunked body: size lines in hex, each chunk followed by CRLF, a
/// zero-size chunk, then trailer lines up to an empty one.
fn read_chunked(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let line = read_line(reader)?;
        let size_text = line.split(';').next().unwrap_or_default().trim();
        if size_text.is_empty() || !size_text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::UpstreamFailed);
        }
        let size = usize::from_str_radix(size_text, 16).map_err(|_| Error::UpstreamFailed)?;
        if size == 0 {
            break;
        }
        if body
            .len()
            .checked_add(size)
            .is_none_or(|total| total > MAX_RESPONSE_BYTES)
        {
            return Err(Error::ResponseTooLarge);
        }
        let start = body.len();
        body.resize(start + size, 0);
        reader
            .read_exact(body.get_mut(start..).ok_or(Error::UpstreamFailed)?)
            .map_err(|_| Error::UpstreamFailed)?;
        if !read_line(reader)?.is_empty() {
            return Err(Error::UpstreamFailed);
        }
    }
    let mut trailer_bytes = 0;
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            return Ok(body);
        }
        trailer_bytes += line.len();
        if trailer_bytes > MAX_HEAD_BYTES {
            return Err(Error::UpstreamFailed);
        }
    }
}

/// A body that runs until the server closes the connection.
fn read_to_close(reader: &mut impl Read) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    let limit = u64::try_from(MAX_RESPONSE_BYTES + 1).map_err(|_| Error::UpstreamFailed)?;
    reader
        .take(limit)
        .read_to_end(&mut body)
        .map_err(|_| Error::UpstreamFailed)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(Error::ResponseTooLarge);
    }
    Ok(body)
}

#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the proxy's tests share this module's local-server helpers"
)]
pub(crate) mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    fn request(method: &str, headers: &[(&str, &str)], body: &[u8]) -> EgressRequest {
        EgressRequest {
            method: method.to_owned(),
            destination: String::from("localhost:11434"),
            path: String::from("/v1/chat/completions"),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.to_vec(),
            surrogate_b64: String::new(),
        }
    }

    fn response(raw: &[u8]) -> Result<EgressResponse> {
        read_response(raw)
    }

    fn body_of(raw: &[u8]) -> Vec<u8> {
        response(raw).unwrap().body
    }

    fn upstream_failed(raw: &[u8]) -> bool {
        matches!(response(raw), Err(Error::UpstreamFailed))
    }

    fn too_large(raw: &[u8]) -> bool {
        matches!(response(raw), Err(Error::ResponseTooLarge))
    }

    #[test]
    fn a_peer_must_be_a_pinned_loopback_address() {
        let pinned = [addr("127.0.0.1:11434"), addr("[::1]:11434")];
        assert!(verify_peer(&addr("127.0.0.1:11434"), &pinned).is_ok());
        assert!(verify_peer(&addr("[::1]:11434"), &pinned).is_ok());
        for peer in [
            "127.0.0.1:11435",
            "[::1]:80",
            "127.0.0.2:11434",
            "10.0.0.5:11434",
        ] {
            assert!(
                matches!(
                    verify_peer(&addr(peer), &pinned),
                    Err(Error::DestinationDenied)
                ),
                "{peer} must be refused"
            );
        }
        // Only the IPv4 address pinned: the IPv6 loopback is not it.
        assert!(verify_peer(&addr("[::1]:11434"), &[addr("127.0.0.1:11434")]).is_err());
        // A remote address is refused even were it somehow pinned.
        let remote = addr("10.0.0.5:11434");
        assert!(matches!(
            verify_peer(&remote, &[remote]),
            Err(Error::DestinationDenied)
        ));
    }

    #[test]
    fn a_request_is_framed_by_the_proxy_alone() {
        let bytes = request_bytes(
            &request("POST", &[("content-type", "application/json")], b"{}"),
            Some(b"local-key"),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(bytes.to_vec()).unwrap(),
            "POST /v1/chat/completions HTTP/1.1\r\nHost: localhost:11434\r\nConnection: close\r\nAccept-Encoding: identity\r\nContent-Length: 2\r\nAuthorization: Bearer local-key\r\ncontent-type: application/json\r\n\r\n{}"
        );
        // Allocated once, at its exact size: never grown, so no copy of the
        // credential was left behind in a buffer it outgrew.
        assert_eq!(bytes.capacity(), bytes.len());
        let keyless = request_bytes(&request("GET", &[], b""), None).unwrap();
        assert_eq!(keyless.capacity(), keyless.len());
        assert_eq!(
            String::from_utf8(keyless.to_vec()).unwrap(),
            "GET /v1/chat/completions HTTP/1.1\r\nHost: localhost:11434\r\nConnection: close\r\nAccept-Encoding: identity\r\nContent-Length: 0\r\n\r\n"
        );
    }

    #[test]
    fn nothing_can_smuggle_a_line_into_the_request() {
        for path in ["/v1 x", "/v1\r\nX: y", "/v1\n", "/v1\0", "/v1\t"] {
            let mut bad = request("POST", &[], b"");
            bad.path = path.to_owned();
            assert!(
                matches!(request_bytes(&bad, None), Err(Error::InvalidSurrogate)),
                "path {path:?}"
            );
        }
        for (name, value) in [
            ("", "v"),
            ("a b", "v"),
            ("a:b", "v"),
            ("a(b", "v"),
            ("a\"b", "v"),
            ("a/b", "v"),
            ("x", "v\r\nInjected: 1"),
            ("x", "v\n"),
            ("x", "v\r"),
            ("x", "\0"),
            ("Content-Length", "5"),
            ("TRANSFER-ENCODING", "chunked"),
            ("connection", "keep-alive"),
        ] {
            assert!(
                matches!(
                    request_bytes(&request("POST", &[(name, value)], b""), None),
                    Err(Error::InvalidSurrogate)
                ),
                "header {name:?}: {value:?}"
            );
        }
        // Every token character is a header name; a tab may sit in a value.
        let name = "Az09!#$%&'*+-.^_`|~";
        assert!(request_bytes(&request("POST", &[(name, "a\tb c")], b""), None).is_ok());
        for credential in [&b"key\r\nX: y"[..], b"key\n", b"\xff\xfe"] {
            assert!(
                matches!(
                    request_bytes(&request("POST", &[], b""), Some(credential)),
                    Err(Error::UpstreamFailed)
                ),
                "credential {credential:?}"
            );
        }
    }

    #[test]
    fn a_response_is_read_by_its_framing() {
        let length = response(b"HTTP/1.1 200 OK\r\nContent-Type:  application/json \r\nContent-Length: 5\r\n\r\nhello-and-more").unwrap();
        assert_eq!(length.status, 200);
        assert_eq!(
            length.headers,
            vec![
                (
                    String::from("Content-Type"),
                    String::from("application/json")
                ),
                (String::from("Content-Length"), String::from("5")),
            ]
        );
        assert_eq!(length.body, b"hello");
        assert_eq!(
            body_of(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n5;ext=1\r\nhello\r\n1\r\n!\r\n0\r\nX-Trailer: t\r\n\r\n"),
            b"hello!"
        );
        assert_eq!(
            body_of(b"HTTP/1.0 200 OK\r\n\r\nto the close"),
            b"to the close"
        );
        assert_eq!(body_of(b"HTTP/1.1 204\r\n\r\n"), b"");
        let status =
            response(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert_eq!(status.status, 429);
    }

    #[test]
    fn a_malformed_response_fails_closed() {
        for raw in [
            &b""[..],
            b"HTTP/2 200 OK\r\n\r\n",
            b"HTTP/1.1 20 OK\r\n\r\n",
            b"HTTP/1.1 2000 OK\r\n\r\n",
            b"HTTP/1.1 2x0 OK\r\n\r\n",
            b"HTTP/1.1\r\n\r\n",
            b"HTTP/1.1 200 OK\n\n",
            b"HTTP/1.1 200 OK\r\n",
            b"HTTP/1.1 200 OK\r\nno colon\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nbad name: v\r\n\r\n",
            b"HTTP/1.1 200 OK\r\n\xff: v\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nhi",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\nhi",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\nhi",
            // A coding other than chunked is refused even when its body
            // would read as chunks.
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\n\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\nhi",
            b"HTTP/1.1 200 OK\r\nContent-Length: \r\n\r\nhi",
            b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999999\r\n\r\nhi",
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhi",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\nhi\r\n0\r\n\r\n",
            // A sign the integer parser would take is not a hex digit.
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n+2\r\nhi\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\r\nhi\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhiX\r\n0\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhi",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n",
        ] {
            assert!(upstream_failed(raw), "{:?}", String::from_utf8_lossy(raw));
        }
    }

    /// A status line, then headers padded to exactly `head` bytes of
    /// head (status line included), then a short body.
    fn head_of(head: usize) -> Vec<u8> {
        let status = "HTTP/1.1 200 OK";
        let mut raw = format!("{status}\r\n");
        let mut left = head - status.len();
        while left > 0 {
            let line = left.min(8000);
            let _ = write!(raw, "x-pad:{}\r\n", "a".repeat(line - 6));
            left -= line;
        }
        raw.push_str("Content-Length: 0\r\n\r\n");
        raw.into_bytes()
    }

    #[test]
    fn the_head_is_bounded_at_its_exact_limit() {
        let length_line = "Content-Length: 0".len();
        assert!(response(&head_of(MAX_HEAD_BYTES - length_line)).is_ok());
        assert!(upstream_failed(&head_of(MAX_HEAD_BYTES - length_line + 1)));
        assert_eq!((MAX_HEAD_BYTES, MAX_LINE_BYTES), (32 * 1024, 8 * 1024));
    }

    #[test]
    fn a_line_is_bounded_at_its_exact_limit() {
        let line = |value_len: usize| {
            format!(
                "HTTP/1.1 200 OK\r\nx:{}\r\nContent-Length: 0\r\n\r\n",
                "a".repeat(value_len)
            )
            .into_bytes()
        };
        assert!(response(&line(MAX_LINE_BYTES - 2)).is_ok());
        assert!(upstream_failed(&line(MAX_LINE_BYTES - 1)));
    }

    #[test]
    fn a_length_body_is_bounded_at_its_exact_limit() {
        let mut exact =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {MAX_RESPONSE_BYTES}\r\n\r\n").into_bytes();
        exact.resize(exact.len() + MAX_RESPONSE_BYTES, b'a');
        assert_eq!(body_of(&exact).len(), MAX_RESPONSE_BYTES);
        // One over is refused before a byte of it is read.
        let over = MAX_RESPONSE_BYTES + 1;
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {over}\r\n\r\n");
        assert!(too_large(head.as_bytes()));
    }

    #[test]
    fn a_chunked_body_is_bounded_at_its_exact_limit() {
        let chunked = |sizes: &[usize]| {
            let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            for size in sizes {
                raw.extend_from_slice(format!("{size:x}\r\n").as_bytes());
                raw.resize(raw.len() + size, b'a');
                raw.extend_from_slice(b"\r\n");
            }
            raw.extend_from_slice(b"0\r\n\r\n");
            raw
        };
        let half = MAX_RESPONSE_BYTES / 2;
        assert_eq!(body_of(&chunked(&[half, half])).len(), MAX_RESPONSE_BYTES);
        assert!(too_large(&chunked(&[half, half + 1])));
        // A size that would overflow is too large, not a panic.
        assert!(too_large(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\nffffffffffffffff\r\n"
        ));
    }

    #[test]
    fn chunked_trailers_are_bounded_at_their_exact_limit() {
        let trailers = |total: usize| {
            let mut raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n".to_vec();
            let mut left = total;
            while left > 0 {
                let line = left.min(8000);
                raw.extend_from_slice(format!("t:{}\r\n", "a".repeat(line - 2)).as_bytes());
                left -= line;
            }
            raw.extend_from_slice(b"\r\n");
            raw
        };
        assert!(response(&trailers(MAX_HEAD_BYTES)).is_ok());
        assert!(upstream_failed(&trailers(MAX_HEAD_BYTES + 1)));
    }

    #[test]
    fn a_body_to_the_close_is_bounded_at_its_exact_limit() {
        let to_close = |len: usize| {
            let mut raw = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
            raw.resize(raw.len() + len, b'a');
            raw
        };
        assert_eq!(
            body_of(&to_close(MAX_RESPONSE_BYTES)).len(),
            MAX_RESPONSE_BYTES
        );
        assert!(too_large(&to_close(MAX_RESPONSE_BYTES + 1)));
    }

    #[test]
    fn framing_is_read_from_the_headers() {
        let headers = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect()
        };
        assert_eq!(framing(&headers(&[])).unwrap(), Framing::ToClose);
        assert_eq!(
            framing(&headers(&[("content-LENGTH", "12")])).unwrap(),
            Framing::Length(12)
        );
        assert_eq!(
            framing(&headers(&[("Transfer-Encoding", "CHUNKED")])).unwrap(),
            Framing::Chunked
        );
        assert_eq!(
            framing(&headers(&[("x", "1"), ("content-length", "0")])).unwrap(),
            Framing::Length(0)
        );
    }

    /// A one-shot local server: accepts one connection, reads one
    /// request (head and `Content-Length` body), answers `reply`, and
    /// hands back what it read.
    pub(crate) fn serve_once(reply: &'static [u8]) -> (SocketAddr, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let bound = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut seen = Vec::new();
            let mut byte = [0_u8; 1];
            while !seen.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                seen.extend_from_slice(&byte);
            }
            let head = String::from_utf8_lossy(&seen).into_owned();
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            seen.extend_from_slice(&body);
            stream.write_all(reply).unwrap();
            seen
        });
        (bound, handle)
    }

    /// An address nothing listens on: a port bound, then released.
    pub(crate) fn dead_addr() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    }

    #[test]
    fn forward_speaks_to_the_first_pinned_address_that_answers() {
        let (live, server) = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let reply = forward(
            &[dead_addr(), live],
            &request(
                "POST",
                &[("content-type", "application/json")],
                b"{\"a\":1}",
            ),
            Some(b"k"),
        )
        .unwrap();
        assert_eq!((reply.status, reply.body.as_slice()), (200, &b"ok"[..]));
        assert_eq!(
            String::from_utf8(server.join().unwrap()).unwrap(),
            "POST /v1/chat/completions HTTP/1.1\r\nHost: localhost:11434\r\nConnection: close\r\nAccept-Encoding: identity\r\nContent-Length: 7\r\nAuthorization: Bearer k\r\ncontent-type: application/json\r\n\r\n{\"a\":1}"
        );
    }

    #[test]
    fn forward_refuses_before_connecting_and_fails_when_nothing_answers() {
        let dead = [dead_addr()];
        assert!(matches!(
            forward(&dead, &request("POST", &[], b""), None),
            Err(Error::UpstreamFailed)
        ));
        assert!(matches!(
            forward(&[], &request("GET", &[], b""), None),
            Err(Error::UpstreamFailed)
        ));
        // A method other than GET or POST, or a request that cannot be
        // framed, is refused without connecting: against a dead address,
        // connecting would have failed upstream instead.
        for method in ["PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"] {
            assert!(
                matches!(
                    forward(&dead, &request(method, &[], b""), None),
                    Err(Error::DestinationDenied)
                ),
                "{method}"
            );
        }
        assert!(matches!(
            forward(&dead, &request("POST", &[("x", "a\nb")], b""), None),
            Err(Error::InvalidSurrogate)
        ));
        assert_eq!(
            (CONNECT_TIMEOUT, IO_TIMEOUT),
            (Duration::from_secs(5), Duration::from_secs(300))
        );
    }
}
