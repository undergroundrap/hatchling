//! A real CLI's vendor relay: the broker's side of a harness
//! cell's only network.
//!
//! A harness cell with a network has one TCP listener, on its own loopback,
//! which its cell hands to the broker. The broker serves it here, one
//! HTTP/1.1 request per connection, and:
//!
//! - **answers locally, never forwarding,** a request that does not carry
//!   the run's surrogate key in the vendor's own header, names a method or
//!   path its API does not use, or breaks the bounds or the framing;
//! - **swaps the surrogate for the vendor key** the broker read from the
//!   vault for this run, which never enters the cell, so a harness that
//!   sends its key anywhere sends a value that works nowhere else;
//! - **forwards to the vendor's one destination**, at the addresses pinned
//!   at startup, and streams the answer back as it arrives.
//!
//! This is a carve-out from the egress confirmation rails, decided in
//! a CLI makes hundreds of calls to one API per task, so they are
//! not confirmed one by one. They go to its vendor's API host and nowhere
//! else, and each run's count is in its `harness_run` activity event.

use std::io::{BufRead as _, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hatchling_egress::scrub::{Scrubber, is_identity, scrub_text};
use hatchling_egress::{Allowlist, TlsClient};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

/// The port of a harness cell's relay, on its own loopback: each cell has
/// its own network, so one fixed port serves every run.
pub const RELAY_PORT: u16 = 8787;

/// Longest request line or header line: 16 KiB. The sizes are written
/// as literals, so no operator is left for mutation.
const MAX_LINE_BYTES: usize = 16_384;
/// Most header lines a request or an answer may carry.
const MAX_HEADERS: usize = 100;
/// Largest request body, 32 MiB: a CLI sends its whole conversation each
/// turn.
pub const MAX_REQUEST_BODY_BYTES: usize = 33_554_432;
/// Largest answer the relay passes back for one request: 64 MiB.
pub const MAX_RESPONSE_BYTES: u64 = 67_108_864;
/// How long a read or write may stall, either side: a model may think
/// for minutes before its first token.
const IO_TIMEOUT: Duration = Duration::from_secs(300);
/// How long the cell has to hand its listener over.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(60);
/// Most connections the relay serves at once.
const MAX_LIVE: u32 = 16;
/// Most request-body bytes one run's connections hold between them: 64
/// MiB, two of the largest bodies. Without it, a cell could open
/// [`MAX_LIVE`] connections, each declaring the largest body, and hold
/// half a GiB of the broker's memory.
const MAX_HELD_BYTES: usize = 67_108_864;
/// How often the accept loop looks at its deadline and stop flag.
const POLL: Duration = Duration::from_millis(20);

/// The two vendor APIs the relay speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// Anthropic's Messages API: the key in `x-api-key`.
    Anthropic,
    /// `OpenAI`'s Responses API: the key as an `Authorization` bearer.
    OpenAi,
}

impl Wire {
    /// Whether the API uses `method` on `target` (a path and its query).
    fn allows(self, method: &str, target: &str) -> bool {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        match self {
            Self::Anthropic => {
                method == "POST"
                    && matches!(path, "/v1/messages" | "/v1/messages/count_tokens")
                    && matches!(query, "" | "beta=true")
            }
            Self::OpenAi => {
                (method == "POST" && path == "/v1/responses" && query.is_empty())
                    || (method == "GET" && path == "/v1/models" && plain_query(query))
            }
        }
    }

    /// The header the API carries its key in, lower case.
    const fn key_header(self) -> &'static str {
        match self {
            Self::Anthropic => "x-api-key",
            Self::OpenAi => "authorization",
        }
    }

    /// The key header's value for `key`.
    fn key_value(self, key: &str) -> Zeroizing<String> {
        match self {
            Self::Anthropic => Zeroizing::new(key.to_owned()),
            Self::OpenAi => Zeroizing::new(format!("Bearer {key}")),
        }
    }
}

/// A query of letters, digits and `=&._-` only.
fn plain_query(query: &str) -> bool {
    query
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"=&._-".contains(&byte))
}

/// Where one harness's requests go.
#[derive(Debug, Clone)]
pub struct Vendor {
    /// The API it speaks.
    pub wire: Wire,
    /// Its one destination, `host:port`, as the allowlist lists it.
    pub destination: String,
    /// The allowlist holding that destination, pinned at startup.
    pub allowlist: Arc<Allowlist>,
    /// The egress root store.
    pub tls: TlsClient,
}

/// What a run's relay did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RelayCount {
    /// Requests forwarded to the vendor.
    pub forwarded: u32,
    /// Requests answered locally, never forwarded.
    pub refused: u32,
}

/// A relay serving one run.
#[derive(Debug)]
pub struct Relay {
    stop: Arc<AtomicBool>,
    counts: Arc<(AtomicU32, AtomicU32)>,
    serving: Option<JoinHandle<Result<(), String>>>,
}

impl Relay {
    /// Starts the relay for one run: it waits on `handoff` for the cell's
    /// listener, then serves it until [`Relay::finish`] or `deadline`.
    /// `surrogate` is what the cell holds; `key` is the vendor key.
    #[must_use]
    pub fn start(
        handoff: UnixListener,
        vendor: Vendor,
        surrogate: Zeroizing<String>,
        key: Zeroizing<String>,
        deadline: Instant,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let counts = Arc::new((AtomicU32::new(0), AtomicU32::new(0)));
        let context = Arc::new(Context {
            vendor,
            surrogate,
            key,
            counts: Arc::clone(&counts),
            live: AtomicU32::new(0),
            held: AtomicUsize::new(0),
        });
        let flag = Arc::clone(&stop);
        let serving = std::thread::spawn(move || {
            let listener = receive(
                &handoff,
                deadline.min(Instant::now() + HANDOFF_TIMEOUT),
                &flag,
            )?;
            serve(&listener, &context, deadline, &flag);
            Ok(())
        });
        Self {
            stop,
            counts,
            serving: Some(serving),
        }
    }

    /// Stops the relay and reports what it did. A connection still open
    /// ends on its own bounds.
    pub fn finish(mut self) -> (RelayCount, Option<String>) {
        self.stop.store(true, Ordering::SeqCst);
        let problem = self
            .serving
            .take()
            .and_then(|serving| serving.join().ok())
            .and_then(Result::err);
        (self.count(), problem)
    }

    /// The counts so far.
    fn count(&self) -> RelayCount {
        RelayCount {
            forwarded: self.counts.0.load(Ordering::SeqCst),
            refused: self.counts.1.load(Ordering::SeqCst),
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// What every connection of one run shares.
#[derive(Debug)]
struct Context {
    vendor: Vendor,
    surrogate: Zeroizing<String>,
    key: Zeroizing<String>,
    counts: Arc<(AtomicU32, AtomicU32)>,
    live: AtomicU32,
    /// Request-body bytes the run's connections hold now (see [`Share`]).
    held: AtomicUsize,
}

/// A connection's place among its run's [`MAX_LIVE`], given back when its
/// thread ends, however it ends.
struct Live(Arc<Context>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A connection's share of its run's [`MAX_HELD_BYTES`]: what it holds of
/// a request body, given back when it's dropped.
struct Share<'a> {
    held: &'a AtomicUsize,
    bytes: usize,
}

impl<'a> Share<'a> {
    /// A share of nothing yet, of the run's `held`.
    const fn new(held: &'a AtomicUsize) -> Self {
        Self { held, bytes: 0 }
    }

    /// Takes `more` bytes of the run's budget, or refuses, taking
    /// nothing, if the run's connections would hold more than
    /// [`MAX_HELD_BYTES`] between them.
    fn grow(&mut self, more: usize) -> Result<(), Refusal> {
        self.held
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |held| {
                held.checked_add(more)
                    .filter(|total| *total <= MAX_HELD_BYTES)
            })
            .map_err(|_| (503, "the run's requests hold too much at once"))?;
        self.bytes += more;
        Ok(())
    }
}

impl Drop for Share<'_> {
    fn drop(&mut self) {
        self.held.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

/// Waits on `handoff` for the cell's listener, until `deadline` or `stop`.
fn receive(
    handoff: &UnixListener,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<TcpListener, String> {
    handoff
        .set_nonblocking(true)
        .map_err(|error| format!("relay handoff: {error}"))?;
    loop {
        match handoff.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .and_then(|()| stream.set_read_timeout(Some(HANDOFF_TIMEOUT)))
                    .map_err(|error| format!("relay handoff: {error}"))?;
                let fd = hatchling_sys::net::recv_fd(std::os::fd::AsFd::as_fd(&stream))
                    .map_err(|error| format!("relay handoff: {error}"))?;
                let listening = hatchling_sys::net::is_listening(std::os::fd::AsFd::as_fd(&fd))
                    .map_err(|error| format!("relay handoff: {error}"))?;
                if !listening {
                    return Err(String::from("relay handoff: not a listening socket"));
                }
                return Ok(TcpListener::from(fd));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
                    return Err(String::from(
                        "relay handoff: the cell's listener never came",
                    ));
                }
                std::thread::sleep(POLL);
            }
            Err(error) => return Err(format!("relay handoff: {error}")),
        }
    }
}

/// Serves `listener` until `stop` or `deadline`: each connection on its
/// own thread, at most [`MAX_LIVE`] at once.
fn serve(listener: &TcpListener, context: &Arc<Context>, deadline: Instant, stop: &AtomicBool) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
        match listener.accept() {
            Ok((client, _)) => {
                if context.live.load(Ordering::SeqCst) >= MAX_LIVE {
                    context.counts.1.fetch_add(1, Ordering::SeqCst);
                    let _ = answer(&client, 503, "too many requests at once");
                    continue;
                }
                context.live.fetch_add(1, Ordering::SeqCst);
                let live = Live(Arc::clone(context));
                std::thread::spawn(move || {
                    let forwarded = relay_one(&client, &live.0);
                    let counter = if forwarded {
                        &live.0.counts.0
                    } else {
                        &live.0.counts.1
                    };
                    counter.fetch_add(1, Ordering::SeqCst);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(POLL);
            }
            Err(_) => return,
        }
    }
}

/// One request: checked, then forwarded, or answered here. Whether it was
/// forwarded.
fn relay_one(client: &TcpStream, context: &Context) -> bool {
    let timed = client
        .set_nonblocking(false)
        .and_then(|()| client.set_read_timeout(Some(IO_TIMEOUT)))
        .and_then(|()| client.set_write_timeout(Some(IO_TIMEOUT)));
    if timed.is_err() {
        return false;
    }
    let mut reader = BufReader::new(client);
    let request = match read_request(&mut reader) {
        Ok(request) => request,
        Err((status, reason)) => {
            let _ = answer(client, status, reason);
            return false;
        }
    };
    let vendor = &context.vendor;
    if !vendor.wire.allows(&request.method, &request.target) {
        let _ = answer(client, 404, "the relay serves the vendor's API only");
        return false;
    }
    let presented = request.header(vendor.wire.key_header()).unwrap_or_default();
    let expected = vendor.wire.key_value(&context.surrogate);
    if !bool::from(presented.as_bytes().ct_eq(expected.as_bytes())) {
        let _ = answer(client, 401, "not this run's key");
        return false;
    }
    let mut share = Share::new(&context.held);
    let body = match read_body(&mut reader, &request, &mut share) {
        Ok(body) => body,
        Err((status, reason)) => {
            let _ = answer(client, status, reason);
            return false;
        }
    };
    // The model request policy: the vendor is not asked to fetch anything
    // on the cell's behalf (see `hatchling_egress::policy`).
    if let Err(reason) = hatchling_egress::policy::check_body(&body) {
        tracing::warn!(reason, "relay request refused by the model request policy");
        let _ = answer(client, 403, reason);
        return false;
    }
    let head = upstream_head(&request, body.len(), vendor, &context.key);
    let opened = hatchling_egress::relay::open(
        &vendor.allowlist,
        &vendor.tls,
        &vendor.destination,
        IO_TIMEOUT,
    );
    let Ok(mut upstream) = opened else {
        let _ = answer(client, 502, "the vendor could not be reached");
        return false;
    };
    if upstream
        .write_all(&head)
        .and_then(|()| upstream.write_all(&body))
        .and_then(|()| upstream.flush())
        .is_err()
    {
        let _ = answer(client, 502, "the vendor could not be reached");
        return false;
    }
    // The body has gone upstream: its share of the run's budget goes back
    // before the answer streams, which can take minutes.
    drop(body);
    drop(share);
    if matches!(
        stream_back(&mut upstream, client, context.key.as_bytes()),
        Ok(true)
    ) {
        tracing::warn!("the vendor echoed its key; it was scrubbed from the answer");
    }
    true
}

/// A parsed request head.
#[derive(Debug)]
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl Request {
    /// The value of `name` (lower case), if it is present once.
    fn header(&self, name: &str) -> Option<&str> {
        let mut found = self
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str());
        let first = found.next();
        found.next().map_or(first, |_| None)
    }
}

/// A local refusal: an HTTP status and its reason.
type Refusal = (u16, &'static str);

/// Reads one line, without its line ending, within [`MAX_LINE_BYTES`].
fn read_line(reader: &mut impl std::io::BufRead) -> Result<String, Refusal> {
    let mut line = Vec::new();
    let read = reader
        .take(u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX) + 1)
        .read_until(b'\n', &mut line)
        .map_err(|_| (400, "the request could not be read"))?;
    if read == 0 || !line.ends_with(b"\r\n") {
        return Err((400, "a request line must end in CRLF within its bound"));
    }
    line.truncate(line.len() - 2);
    String::from_utf8(line).map_err(|_| (400, "a request line is not UTF-8"))
}

/// Reads and checks a request head.
fn read_request(reader: &mut impl std::io::BufRead) -> Result<Request, Refusal> {
    let line = read_line(reader)?;
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some("HTTP/1.1"), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err((400, "not an HTTP/1.1 request line"));
    };
    if !target.starts_with('/')
        || target.contains("//")
        || target.contains("..")
        || target.contains('#')
    {
        return Err((400, "the request target is not a plain path"));
    }
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err((431, "too many header lines"));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err((400, "a header line has no colon"));
        };
        if name.is_empty()
            || !name.bytes().all(is_token_byte)
            || value
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err((400, "a header line is malformed"));
        }
        headers.push((name.to_owned(), value.trim().to_owned()));
    }
    Ok(Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
    })
}

/// An HTTP token character (RFC 9110).
const fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

/// Reads a request's body, framed by `Content-Length` or chunked, never
/// both, within [`MAX_REQUEST_BODY_BYTES`] and the run's budget: `share`
/// takes each byte before it's allocated.
fn read_body(
    reader: &mut impl std::io::BufRead,
    request: &Request,
    share: &mut Share<'_>,
) -> Result<Vec<u8>, Refusal> {
    let length = request
        .headers
        .iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .count();
    let chunked = request.header("transfer-encoding");
    match (length, chunked) {
        (0, None) => Ok(Vec::new()),
        (1, None) => {
            let length: usize = request
                .header("content-length")
                .and_then(|value| value.parse().ok())
                .ok_or((400, "the content length is not a number"))?;
            if length > MAX_REQUEST_BODY_BYTES {
                return Err((413, "the request body is past its bound"));
            }
            share.grow(length)?;
            let mut body = vec![0; length];
            reader
                .read_exact(&mut body)
                .map_err(|_| (400, "the request body ended early"))?;
            Ok(body)
        }
        (0, Some(coding)) if coding.eq_ignore_ascii_case("chunked") => read_chunked(reader, share),
        _ => Err((400, "the request body's framing is ambiguous")),
    }
}

/// Reads a chunked body, within [`MAX_REQUEST_BODY_BYTES`] and `share`.
fn read_chunked(
    reader: &mut impl std::io::BufRead,
    share: &mut Share<'_>,
) -> Result<Vec<u8>, Refusal> {
    let mut body = Vec::new();
    loop {
        let line = read_line(reader)?;
        let size = line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16)
            .map_err(|_| (400, "a chunk size is not hexadecimal"))?;
        if size == 0 {
            // Trailers are read and dropped; the blank line ends the body.
            while !read_line(reader)?.is_empty() {}
            return Ok(body);
        }
        // Checked: a chunk size near the largest number must not wrap
        // around to a small one.
        let total = body
            .len()
            .checked_add(size)
            .filter(|total| *total <= MAX_REQUEST_BODY_BYTES)
            .ok_or((413, "the request body is past its bound"))?;
        share.grow(size)?;
        let start = body.len();
        body.resize(total, 0);
        let chunk = body
            .get_mut(start..)
            .ok_or((400, "a chunk could not be read"))?;
        reader
            .read_exact(chunk)
            .map_err(|_| (400, "a chunk ended early"))?;
        if !read_line(reader)?.is_empty() {
            return Err((400, "a chunk did not end in CRLF"));
        }
    }
}

/// Headers never passed on: the hop's own, the host, and either key
/// header (the relay adds its own).
const DROPPED: [&str; 14] = [
    "host",
    "authorization",
    "x-api-key",
    "connection",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
    "expect",
    // The relay asks for an uncompressed answer itself, so it can be
    // scrubbed for the vendor key.
    "accept-encoding",
];

/// The head of the request the vendor receives, for a body of `length`
/// bytes sent after it: the cell's head, with the vendor key in place of
/// the surrogate, the vendor's host, its own framing, and a closed
/// connection.
fn upstream_head(
    request: &Request,
    length: usize,
    vendor: &Vendor,
    key: &str,
) -> Zeroizing<Vec<u8>> {
    let host = vendor
        .destination
        .strip_suffix(":443")
        .unwrap_or(&vendor.destination);
    let mut head = Zeroizing::new(format!(
        "{} {} HTTP/1.1\r\nHost: {host}\r\n",
        request.method, request.target
    ));
    for (name, value) in &request.headers {
        if !DROPPED
            .iter()
            .any(|dropped| name.eq_ignore_ascii_case(dropped))
        {
            for part in [name.as_str(), ": ", value.as_str(), "\r\n"] {
                head.push_str(part);
            }
        }
    }
    let wire = vendor.wire;
    let length = length.to_string();
    for part in [
        wire.key_header(),
        ": ",
        wire.key_value(key).as_str(),
        "\r\nAccept-Encoding: identity\r\nContent-Length: ",
        length.as_str(),
        "\r\nConnection: close\r\n\r\n",
    ] {
        head.push_str(part);
    }
    Zeroizing::new(head.as_bytes().to_vec())
}

/// Passes the vendor's answer back as it arrives: its head with the
/// connection marked closed, then its body, within [`MAX_RESPONSE_BYTES`].
///
/// The vendor key is scrubbed from all of it (see
/// [`hatchling_egress::scrub`]): from each header line, and from the body
/// as it streams, through any chunked framing, so a vendor that echoes
/// the key never hands it to the cell. An answer the scrub can't read,
/// compressed or in another transfer coding, is refused with a 502.
/// Whether the key was anywhere in the answer.
fn stream_back(upstream: &mut impl Read, client: &TcpStream, key: &[u8]) -> std::io::Result<bool> {
    let mut reader = BufReader::new(upstream);
    let mut head = Vec::new();
    let mut status = read_line(&mut reader).map_err(|_| std::io::Error::other("no status line"))?;
    let mut found = scrub_text(&mut status, key);
    head.extend_from_slice(status.as_bytes());
    head.extend_from_slice(b"\r\n");
    let mut chunked = false;
    for _ in 0..=MAX_HEADERS {
        let mut line =
            read_line(&mut reader).map_err(|_| std::io::Error::other("no header line"))?;
        if line.is_empty() {
            head.extend_from_slice(b"Connection: close\r\n\r\n");
            let mut client = client;
            client.write_all(&head)?;
            let mut body = reader.take(MAX_RESPONSE_BYTES);
            let echoed = if chunked {
                stream_chunked(&mut body, client, key)?
            } else {
                stream_raw(&mut body, client, key)?
            };
            client.flush()?;
            return Ok(found || echoed);
        }
        let (name, value) = line
            .split_once(':')
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .unwrap_or_default();
        let unreadable = (name == "content-encoding" && !is_identity(&value))
            || (name == "transfer-encoding" && !value.eq_ignore_ascii_case("chunked"));
        if unreadable {
            let _ = answer(
                client,
                502,
                "the vendor's answer is encoded so it can't be checked for its key",
            );
            return Err(std::io::Error::other("an answer the scrub can't read"));
        }
        chunked |= name == "transfer-encoding";
        if !["connection", "keep-alive"].contains(&name.as_str()) {
            found |= scrub_text(&mut line, key);
            head.extend_from_slice(line.as_bytes());
            head.extend_from_slice(b"\r\n");
        }
    }
    Err(std::io::Error::other("too many header lines"))
}

/// The size of each read of an answer's body.
const PIECE: usize = 8192;

/// Passes on a body read to its end, scrubbed for `key` as it goes.
/// Whether the key was in it.
fn stream_raw(body: &mut impl Read, mut client: &TcpStream, key: &[u8]) -> std::io::Result<bool> {
    let mut scrubber = Scrubber::new(key);
    let mut piece = [0_u8; PIECE];
    loop {
        let read = match body.read(&mut piece) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        client.write_all(&scrubber.push(piece.get(..read).unwrap_or_default()))?;
    }
    let found = scrubber.found();
    client.write_all(&scrubber.finish())?;
    Ok(found)
}

/// Passes on a chunked body: decoded as it arrives, scrubbed for `key`,
/// and chunked again. A key the vendor split across chunks is put back
/// together before the scrub, as the client would put it together.
/// Trailers are dropped. Whether the key was in it.
fn stream_chunked(
    body: &mut impl Read,
    mut client: &TcpStream,
    key: &[u8],
) -> std::io::Result<bool> {
    fn chunk(client: &mut &TcpStream, data: &[u8]) -> std::io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        write!(client, "{:x}\r\n", data.len())?;
        client.write_all(data)?;
        client.write_all(b"\r\n")
    }
    let framing = |_| std::io::Error::other("the answer's chunked framing is broken");
    let mut reader = BufReader::new(body);
    let mut scrubber = Scrubber::new(key);
    let mut piece = [0_u8; PIECE];
    loop {
        let line = read_line(&mut reader).map_err(framing)?;
        let size = line.split(';').next().unwrap_or_default().trim();
        let size = u64::from_str_radix(size, 16)
            .map_err(|_| std::io::Error::other("a chunk size is not hexadecimal"))?;
        if size == 0 {
            break;
        }
        let mut data = (&mut reader).take(size);
        let mut left = size;
        while left > 0 {
            let read = match data.read(&mut piece) {
                Ok(0) => return Err(std::io::Error::other("the answer ended inside a chunk")),
                Ok(read) => read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            left = left.saturating_sub(u64::try_from(read).unwrap_or(u64::MAX));
            chunk(
                &mut client,
                &scrubber.push(piece.get(..read).unwrap_or_default()),
            )?;
        }
        if !read_line(&mut reader).map_err(framing)?.is_empty() {
            return Err(std::io::Error::other(
                "a chunk doesn't end where its size says",
            ));
        }
    }
    // Trailers could carry anything; they aren't passed on.
    for _ in 0..=MAX_HEADERS {
        if read_line(&mut reader).map_err(framing)?.is_empty() {
            let found = scrubber.found();
            chunk(&mut client, &scrubber.finish())?;
            client.write_all(b"0\r\n\r\n")?;
            return Ok(found);
        }
    }
    Err(std::io::Error::other("too many trailer lines"))
}

/// Answers locally with `status` and a JSON error naming `reason`.
fn answer(client: &TcpStream, status: u16, reason: &str) -> std::io::Result<()> {
    let body =
        serde_json::json!({"error": {"type": "hatchling_relay", "message": reason}}).to_string();
    let text = match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "Service Unavailable",
    };
    let mut client = client;
    client.write_all(
        format!(
            "HTTP/1.1 {status} {text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .as_bytes(),
    )?;
    client.flush()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::os::fd::AsFd as _;
    use std::sync::mpsc;

    use super::*;

    const SOON: Duration = Duration::from_secs(5);

    /// Both ends of a loopback TCP connection: (client, server).
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    fn request(method: &str, target: &str, headers: &[(&str, &str)]) -> Request {
        Request {
            method: method.to_owned(),
            target: target.to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    fn reader(bytes: &[u8]) -> BufReader<Cursor<Vec<u8>>> {
        BufReader::new(Cursor::new(bytes.to_vec()))
    }

    /// A plain local vendor that answers each connection with `answer`
    /// and reports every request it got, whole.
    fn mock_vendor(answer: &'static [u8]) -> (u16, mpsc::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                // Bounded: a request that never comes whole (an empty one
                // from a broken relay) is answered anyway, and fails the
                // test's assertions instead of stalling it.
                stream.set_read_timeout(Some(SOON)).unwrap();
                let mut got = BufReader::new(stream.try_clone().unwrap());
                let mut head = Vec::new();
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    if got.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if let Some(value) = line.strip_prefix("Content-Length: ") {
                        length = value.trim().parse().unwrap();
                    }
                    head.extend_from_slice(line.as_bytes());
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                got.read_exact(&mut body).unwrap();
                head.extend_from_slice(&body);
                let _ = tx.send(head);
                stream.write_all(answer).unwrap();
            }
        });
        (port, rx)
    }

    fn vendor(wire: Wire, port: u16) -> Vendor {
        let destination = format!("localhost:{port}");
        Vendor {
            wire,
            allowlist: Arc::new(Allowlist::parse(&format!("local:{destination}")).unwrap()),
            destination,
            tls: TlsClient::bundled().unwrap(),
        }
    }

    fn context(vendor: Vendor) -> Context {
        Context {
            vendor,
            surrogate: Zeroizing::new(String::from("sk-ant-hatchling-cell")),
            key: Zeroizing::new(String::from("real-vendor-key")),
            counts: Arc::new((AtomicU32::new(0), AtomicU32::new(0))),
            live: AtomicU32::new(0),
            held: AtomicUsize::new(0),
        }
    }

    /// Sends `bytes` as the client of a connection `relay_one` serves;
    /// whether it forwarded, and what the client got back.
    fn relay_once(context: &Context, bytes: &[u8]) -> (bool, String) {
        let (mut client, server) = pair();
        client.write_all(bytes).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let forwarded = relay_one(&server, context);
        drop(server);
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        (forwarded, answer)
    }

    #[test]
    fn the_bounds_are_16_kib_lines_32_mib_requests_and_64_mib_answers() {
        assert_eq!(MAX_LINE_BYTES, 16 << 10);
        assert_eq!(MAX_REQUEST_BODY_BYTES, 32 << 20);
        assert_eq!(MAX_RESPONSE_BYTES, 64 << 20);
        assert_eq!(MAX_HELD_BYTES, 64 << 20);
    }

    #[test]
    fn a_runs_connections_hold_at_most_its_budget_between_them() {
        let held = AtomicUsize::new(0);
        let mut first = Share::new(&held);
        first.grow(MAX_HELD_BYTES - 10).unwrap();
        let mut second = Share::new(&held);
        second.grow(4).unwrap();
        second.grow(6).unwrap();
        // Exactly the budget is held; past it is refused, taking nothing.
        assert_eq!(held.load(Ordering::SeqCst), MAX_HELD_BYTES);
        let refused = (503, "the run's requests hold too much at once");
        assert_eq!(second.grow(1).unwrap_err(), refused);
        assert_eq!(second.grow(usize::MAX).unwrap_err(), refused);
        assert_eq!(held.load(Ordering::SeqCst), MAX_HELD_BYTES);
        // Each share gives back what it took, and no more.
        drop(first);
        assert_eq!(held.load(Ordering::SeqCst), 10);
        drop(second);
        assert_eq!(held.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_body_the_runs_budget_cannot_hold_is_refused_before_it_is_read() {
        let held = AtomicUsize::new(MAX_HELD_BYTES - 2);
        let mut share = Share::new(&held);
        let length = |length: &'static str| request("POST", "/", &[("Content-Length", length)]);
        let refused = (503, "the run's requests hold too much at once");
        assert_eq!(
            read_body(&mut reader(b"abc"), &length("3"), &mut share).unwrap_err(),
            refused
        );
        assert_eq!(
            read_body(&mut reader(b"ab"), &length("2"), &mut share).unwrap(),
            b"ab"
        );
        assert_eq!(held.load(Ordering::SeqCst), MAX_HELD_BYTES);
        drop(share);
        assert_eq!(held.load(Ordering::SeqCst), MAX_HELD_BYTES - 2);
        // A chunked body takes its share chunk by chunk.
        let mut share = Share::new(&held);
        assert_eq!(
            read_chunked(&mut reader(b"1\r\na\r\n2\r\nbc\r\n0\r\n\r\n"), &mut share).unwrap_err(),
            refused
        );
        assert_eq!(held.load(Ordering::SeqCst), MAX_HELD_BYTES - 1);
        drop(share);
        assert_eq!(held.load(Ordering::SeqCst), MAX_HELD_BYTES - 2);
    }

    #[test]
    fn the_relay_refuses_what_the_runs_budget_cannot_hold_and_frees_what_it_forwarded() {
        let sent = b"POST /v1/messages HTTP/1.1\r\nx-api-key: sk-ant-hatchling-cell\r\n\
                     Content-Length: 2\r\n\r\n{}";
        let full = context(vendor(Wire::Anthropic, 1));
        full.held.store(MAX_HELD_BYTES, Ordering::SeqCst);
        let (forwarded, answer) = relay_once(&full, sent);
        assert!(!forwarded);
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        assert_eq!(full.held.load(Ordering::SeqCst), MAX_HELD_BYTES);
        let (port, got) = mock_vendor(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let open = context(vendor(Wire::Anthropic, port));
        let (forwarded, answer) = relay_once(&open, sent);
        assert!(forwarded, "{answer}");
        assert!(got.recv().unwrap().ends_with(b"\r\n\r\n{}"));
        assert_eq!(open.held.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_connection_gives_its_place_back_however_its_thread_ends() {
        let context = Arc::new(context(vendor(Wire::Anthropic, 1)));
        context.live.fetch_add(2, Ordering::SeqCst);
        let live = Live(Arc::clone(&context));
        drop(live);
        assert_eq!(context.live.load(Ordering::SeqCst), 1);
        let live = Live(Arc::clone(&context));
        let ended = std::thread::spawn(move || {
            let _live = live;
            std::panic::resume_unwind(Box::new("the connection's thread broke"));
        })
        .join();
        assert!(ended.is_err());
        assert_eq!(context.live.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn each_wire_names_its_own_key_header_and_value() {
        assert_eq!(Wire::Anthropic.key_header(), "x-api-key");
        assert_eq!(Wire::OpenAi.key_header(), "authorization");
        assert_eq!(Wire::Anthropic.key_value("k").as_str(), "k");
        assert_eq!(Wire::OpenAi.key_value("k").as_str(), "Bearer k");
    }

    #[test]
    fn each_wire_allows_only_its_apis_calls() {
        let anthropic = Wire::Anthropic;
        assert!(anthropic.allows("POST", "/v1/messages"));
        assert!(anthropic.allows("POST", "/v1/messages?beta=true"));
        assert!(anthropic.allows("POST", "/v1/messages/count_tokens"));
        assert!(!anthropic.allows("GET", "/v1/messages"));
        assert!(!anthropic.allows("POST", "/v1/messages?beta=false"));
        assert!(!anthropic.allows("POST", "/v1/responses"));
        let openai = Wire::OpenAi;
        assert!(openai.allows("POST", "/v1/responses"));
        assert!(!openai.allows("POST", "/v1/responses?x=1"));
        assert!(openai.allows("GET", "/v1/models"));
        assert!(openai.allows("GET", "/v1/models?client_version=0.162.0"));
        assert!(!openai.allows("GET", "/v1/models?a=%2F"));
        assert!(!openai.allows("POST", "/v1/models"));
        assert!(!openai.allows("GET", "/v1/responses"));
        assert!(!openai.allows("POST", "/v1/messages"));
    }

    #[test]
    fn a_plain_query_is_letters_digits_and_five_marks() {
        assert!(plain_query(""));
        assert!(plain_query("a=1&b=c.d_e-f"));
        for bad in ["a b", "a=%2F", "a/b", "a?b", "a+b", "\u{e9}"] {
            assert!(!plain_query(bad), "{bad}");
        }
    }

    #[test]
    fn a_header_counts_only_when_present_exactly_once() {
        let once = request("POST", "/", &[("X-Api-Key", "k"), ("a", "1")]);
        assert_eq!(once.header("x-api-key"), Some("k"));
        assert_eq!(once.header("missing"), None);
        let twice = request("POST", "/", &[("x-api-key", "k"), ("X-API-KEY", "j")]);
        assert_eq!(twice.header("x-api-key"), None);
    }

    #[test]
    fn a_line_ends_in_crlf_within_its_bound() {
        assert_eq!(read_line(&mut reader(b"abc\r\nrest")).unwrap(), "abc");
        assert_eq!(read_line(&mut reader(b"\r\n")).unwrap(), "");
        for bad in [&b""[..], b"abc\n", b"abc", b"a\xff\r\n"] {
            assert_eq!(read_line(&mut reader(bad)).unwrap_err().0, 400);
        }
        // The bound counts the line ending: MAX_LINE_BYTES - 1 bytes and
        // CRLF fit, one more does not.
        let mut fits = vec![b'a'; MAX_LINE_BYTES - 1];
        fits.extend_from_slice(b"\r\n");
        assert_eq!(
            read_line(&mut reader(&fits)).unwrap().len(),
            MAX_LINE_BYTES - 1
        );
        let mut over = vec![b'a'; MAX_LINE_BYTES];
        over.extend_from_slice(b"\r\n");
        assert_eq!(
            read_line(&mut reader(&over)).unwrap_err(),
            (400, "a request line must end in CRLF within its bound")
        );
    }

    #[test]
    fn a_token_is_letters_digits_and_rfc_9110s_marks() {
        for byte in b"aZ09!#$%&'*+-.^_`|~" {
            assert!(is_token_byte(*byte), "{}", char::from(*byte));
        }
        for byte in b" :\"(),/;<=>?@[\\]{}\t\x7f\x80" {
            assert!(!is_token_byte(*byte), "{byte:#x}");
        }
    }

    #[test]
    fn a_request_head_is_one_plain_http_11_line_and_well_formed_headers() {
        let head = read_request(&mut reader(
            b"POST /v1/messages HTTP/1.1\r\nX-Api-Key:  k \r\nTab: a\tb\r\n\r\nbody",
        ))
        .unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.target, "/v1/messages");
        assert_eq!(
            head.headers,
            vec![
                (String::from("X-Api-Key"), String::from("k")),
                (String::from("Tab"), String::from("a\tb")),
            ]
        );
        for (bad, status) in [
            (&b"POST /v1/messages HTTP/1.0\r\n\r\n"[..], 400),
            (b"POST /v1/messages\r\n\r\n", 400),
            (b"POST /v1/messages HTTP/1.1 x\r\n\r\n", 400),
            (b"POST v1/messages HTTP/1.1\r\n\r\n", 400),
            (b"POST //v1 HTTP/1.1\r\n\r\n", 400),
            (b"POST /v1/../x HTTP/1.1\r\n\r\n", 400),
            (b"POST /v1#x HTTP/1.1\r\n\r\n", 400),
            (b"POST / HTTP/1.1\r\nno colon\r\n\r\n", 400),
            (b"POST / HTTP/1.1\r\n: empty\r\n\r\n", 400),
            (b"POST / HTTP/1.1\r\nbad name: x\r\n\r\n", 400),
            (b"POST / HTTP/1.1\r\nx: a\x01b\r\n\r\n", 400),
            (b"POST / HTTP/1.1\r\nx: unended", 400),
        ] {
            assert_eq!(
                read_request(&mut reader(bad)).unwrap_err().0,
                status,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn a_request_head_holds_at_most_its_header_bound() {
        let head = |count: usize| {
            let mut bytes = b"POST / HTTP/1.1\r\n".to_vec();
            for index in 0..count {
                bytes.extend_from_slice(format!("h{index}: v\r\n").as_bytes());
            }
            bytes.extend_from_slice(b"\r\n");
            read_request(&mut reader(&bytes))
        };
        assert_eq!(head(MAX_HEADERS).unwrap().headers.len(), MAX_HEADERS);
        assert_eq!(
            head(MAX_HEADERS + 1).unwrap_err(),
            (431, "too many header lines")
        );
    }

    #[test]
    fn a_body_is_framed_one_way_within_its_bound() {
        let post = |headers: &[(&str, &str)], bytes: &[u8]| {
            read_body(
                &mut reader(bytes),
                &request("POST", "/", headers),
                &mut Share::new(&AtomicUsize::new(0)),
            )
        };
        assert_eq!(post(&[], b"ignored").unwrap(), b"");
        assert_eq!(post(&[("Content-Length", "3")], b"abcd").unwrap(), b"abc");
        assert_eq!(
            post(&[("transfer-encoding", "Chunked")], b"2\r\nab\r\n0\r\n\r\n").unwrap(),
            b"ab"
        );
        assert_eq!(post(&[("Content-Length", "x")], b"").unwrap_err().0, 400);
        assert_eq!(
            post(&[("Content-Length", "3")], b"ab").unwrap_err(),
            (400, "the request body ended early")
        );
        let over = (MAX_REQUEST_BODY_BYTES + 1).to_string();
        assert_eq!(
            post(&[("Content-Length", over.as_str())], b"").unwrap_err(),
            (413, "the request body is past its bound")
        );
        // Exactly the bound is read, not refused (it ends early here).
        let bound = MAX_REQUEST_BODY_BYTES.to_string();
        assert_eq!(
            post(&[("Content-Length", bound.as_str())], b"").unwrap_err(),
            (400, "the request body ended early")
        );
        let ambiguous = (400, "the request body's framing is ambiguous");
        for headers in [
            &[("Content-Length", "1"), ("Transfer-Encoding", "chunked")][..],
            &[("Content-Length", "1"), ("content-length", "1")],
            &[("Transfer-Encoding", "gzip")],
        ] {
            assert_eq!(post(headers, b"x").unwrap_err(), ambiguous, "{headers:?}");
        }
    }

    #[test]
    fn a_chunked_body_is_read_whole_within_its_bound() {
        let chunked =
            |bytes: &[u8]| read_chunked(&mut reader(bytes), &mut Share::new(&AtomicUsize::new(0)));
        assert_eq!(
            chunked(b"3;ext=1\r\nabc\r\nA\r\n0123456789\r\n0\r\nTrailer: x\r\n\r\n").unwrap(),
            b"abc0123456789"
        );
        assert_eq!(
            chunked(b"zz\r\n").unwrap_err(),
            (400, "a chunk size is not hexadecimal")
        );
        assert_eq!(
            chunked(b"3\r\nabcX\r\n").unwrap_err(),
            (400, "a chunk did not end in CRLF")
        );
        assert_eq!(
            chunked(b"3\r\nab").unwrap_err(),
            (400, "a chunk ended early")
        );
        // The bound is on the whole body: past it is refused before any
        // byte of the chunk is read; exactly at it, the chunk is read.
        let over = format!("1\r\na\r\n{MAX_REQUEST_BODY_BYTES:x}\r\n");
        assert_eq!(
            chunked(over.as_bytes()).unwrap_err(),
            (413, "the request body is past its bound")
        );
        let at = format!("1\r\na\r\n{:x}\r\n", MAX_REQUEST_BODY_BYTES - 1);
        assert_eq!(
            chunked(at.as_bytes()).unwrap_err(),
            (400, "a chunk ended early")
        );
        // A size so large the running total would wrap is refused too.
        let wraps = format!("1\r\na\r\n{:x}\r\n", usize::MAX);
        assert_eq!(
            chunked(wraps.as_bytes()).unwrap_err(),
            (413, "the request body is past its bound")
        );
    }

    #[test]
    fn the_vendor_gets_the_key_its_host_and_fresh_framing_and_nothing_hop_by_hop() {
        let mut headers = vec![
            ("Content-Type", "application/json"),
            ("anthropic-version", "1"),
        ];
        for dropped in DROPPED {
            headers.push((dropped, "drop me"));
        }
        headers.push(("X-API-KEY", "drop me too"));
        let head = request("POST", "/v1/messages?beta=true", &headers);
        let mut anthropic = vendor(Wire::Anthropic, 443);
        anthropic.destination = String::from("api.anthropic.com:443");
        let bytes = upstream_head(&head, 2, &anthropic, "real");
        assert_eq!(
            String::from_utf8(bytes.to_vec()).unwrap(),
            "POST /v1/messages?beta=true HTTP/1.1\r\nHost: api.anthropic.com\r\n\
             Content-Type: application/json\r\nanthropic-version: 1\r\n\
             x-api-key: real\r\nAccept-Encoding: identity\r\nContent-Length: 2\r\nConnection: close\r\n\r\n"
        );
        let openai = vendor(Wire::OpenAi, 8443);
        let bytes = upstream_head(&request("GET", "/v1/models", &[]), 0, &openai, "real");
        assert_eq!(
            String::from_utf8(bytes.to_vec()).unwrap(),
            "GET /v1/models HTTP/1.1\r\nHost: localhost:8443\r\n\
             authorization: Bearer real\r\nAccept-Encoding: identity\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn the_answer_streams_back_with_the_connection_marked_closed() {
        let (mut client, server) = pair();
        let mut upstream = Cursor::new(
            b"HTTP/1.1 200 OK\r\nConnection: keep-alive\r\nKeep-Alive: 5\r\nX-A: 1\r\n\r\nbody"
                .to_vec(),
        );
        stream_back(&mut upstream, &server, b"real-vendor-key").unwrap();
        drop(server);
        let mut got = String::new();
        client.read_to_string(&mut got).unwrap();
        assert_eq!(
            got,
            "HTTP/1.1 200 OK\r\nX-A: 1\r\nConnection: close\r\n\r\nbody"
        );
        let (_client, server) = pair();
        assert!(stream_back(&mut Cursor::new(b"".to_vec()), &server, b"k").is_err());
        assert!(
            stream_back(
                &mut Cursor::new(b"HTTP/1.1 200 OK\r\nX: 1".to_vec()),
                &server,
                b"k"
            )
            .is_err()
        );
    }

    /// A body that reads as `steps`, in order, then ends. A step longer
    /// than the reader's buffer is read in parts.
    struct Scripted(std::collections::VecDeque<std::io::Result<Vec<u8>>>);

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let Some(step) = self.0.pop_front() else {
                return Ok(0);
            };
            let bytes = step?;
            let (now, later) = bytes.split_at(bytes.len().min(buf.len()));
            buf.get_mut(..now.len()).unwrap().copy_from_slice(now);
            if !later.is_empty() {
                self.0.push_front(Ok(later.to_vec()));
            }
            Ok(now.len())
        }
    }

    fn interrupted() -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
    }

    fn failed() -> std::io::Result<Vec<u8>> {
        Err(std::io::Error::other("the vendor hung up"))
    }

    /// The answer a client receives from `stream_back`, for an upstream
    /// that sends `bytes`, with `key` as the vendor key.
    fn streamed(bytes: &[u8], key: &[u8]) -> (std::io::Result<bool>, Vec<u8>) {
        let (mut client, server) = pair();
        let result = stream_back(&mut Cursor::new(bytes.to_vec()), &server, key);
        drop(server);
        let mut got = Vec::new();
        client.read_to_end(&mut got).unwrap();
        (result, got)
    }

    /// A chunked body, decoded: what the client would put together.
    fn unchunked(mut body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let line_end = body.windows(2).position(|pair| pair == b"\r\n").unwrap();
            let (size, rest) = body.split_at(line_end);
            let size = usize::from_str_radix(std::str::from_utf8(size).unwrap(), 16).unwrap();
            let rest = rest.get(2..).unwrap();
            if size == 0 {
                assert_eq!(rest, b"\r\n", "the last chunk ends the body");
                return out;
            }
            let (data, rest) = rest.split_at(size);
            out.extend_from_slice(data);
            assert!(
                rest.starts_with(b"\r\n"),
                "each chunk ends where its size says"
            );
            body = rest.get(2..).unwrap();
        }
    }

    #[test]
    fn a_body_streams_scrubbed_and_says_whether_the_key_was_in_it() {
        let raw = |steps: Vec<std::io::Result<Vec<u8>>>| {
            let (mut client, server) = pair();
            let found = stream_raw(&mut Scripted(steps.into()), &server, b"key");
            drop(server);
            let mut got = Vec::new();
            client.read_to_end(&mut got).unwrap();
            (found.ok(), got)
        };
        // An interrupted read is retried, and a key split around it is found.
        assert_eq!(
            raw(vec![
                Ok(b"a k".to_vec()),
                interrupted(),
                Ok(b"ey b".to_vec())
            ]),
            (Some(true), b"a *** b".to_vec())
        );
        assert_eq!(
            raw(vec![Ok(b"clean".to_vec())]),
            (Some(false), b"clean".to_vec())
        );
        // A failed read ends the stream, even if the vendor sends more.
        assert_eq!(
            raw(vec![Ok(b"a".to_vec()), failed(), Ok(b"b".to_vec())]).0,
            None
        );

        let chunked = |steps: Vec<std::io::Result<Vec<u8>>>| {
            let (mut client, server) = pair();
            let found = stream_chunked(&mut Scripted(steps.into()), &server, b"key");
            drop(server);
            let mut got = Vec::new();
            client.read_to_end(&mut got).unwrap();
            (found.ok(), got)
        };
        let (found, got) = chunked(vec![
            Ok(b"3\r\na ".to_vec()),
            interrupted(),
            Ok(b"k\r\n2\r\ney\r\n0\r\n\r\n".to_vec()),
        ]);
        assert_eq!(found, Some(true));
        assert_eq!(unchunked(&got), b"a ***".to_vec());
        let (found, got) = chunked(vec![Ok(b"3\r\nabc\r\n0\r\n\r\n".to_vec())]);
        assert_eq!(found, Some(false));
        assert_eq!(unchunked(&got), b"abc".to_vec());
        assert_eq!(
            chunked(vec![
                Ok(b"3\r\na".to_vec()),
                failed(),
                Ok(b"bc\r\n0\r\n\r\n".to_vec())
            ])
            .0,
            None
        );
    }

    #[test]
    fn stream_back_says_whether_the_key_was_anywhere_in_the_answer() {
        for (answer, echoed) in [
            (
                &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"[..],
                false,
            ),
            (
                &b"HTTP/1.1 401 real-vendor-key\r\nContent-Length: 2\r\n\r\nok"[..],
                true,
            ),
            (
                &b"HTTP/1.1 200 OK\r\nX-Echo: real-vendor-key\r\nContent-Length: 2\r\n\r\nok"[..],
                true,
            ),
            (
                &b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\nreal-vendor-key"[..],
                true,
            ),
        ] {
            let (result, _) = streamed(answer, b"real-vendor-key");
            assert_eq!(
                result.unwrap(),
                echoed,
                "{}",
                String::from_utf8_lossy(answer)
            );
        }
    }

    #[test]
    fn the_vendor_key_is_scrubbed_from_the_head_and_a_plain_body() {
        let (result, got) = streamed(
            b"HTTP/1.1 401 real-vendor-key\r\nX-Echo: key=real-vendor-key\r\nContent-Length: 30\r\n\r\nreal-vendor-keyreal-vendor-key",
            b"real-vendor-key",
        );
        result.unwrap();
        assert_eq!(
            String::from_utf8(got).unwrap(),
            "HTTP/1.1 401 ***************\r\nX-Echo: key=***************\r\nContent-Length: 30\r\n\
             Connection: close\r\n\r\n******************************"
        );
    }

    #[test]
    fn a_key_split_across_chunks_is_put_together_and_scrubbed() {
        // "real-" and "vendor-key" in two chunks, a chunk extension, and a
        // trailer that is dropped.
        let (result, got) = streamed(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
              6;x=1\r\nkey: \"\r\n5\r\nreal-\r\na\r\nvendor-key\r\n2\r\n\"!\r\n0\r\nX-Trailer: real-vendor-key\r\n\r\n",
            b"real-vendor-key",
        );
        result.unwrap();
        let text = String::from_utf8_lossy(&got).into_owned();
        assert!(!text.contains("real-"), "{text}");
        assert!(!text.contains("vendor-key"), "{text}");
        assert!(!text.contains("X-Trailer"), "trailers are dropped: {text}");
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        assert_eq!(
            head,
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close"
        );
        assert_eq!(
            unchunked(body.as_bytes()),
            b"key: \"***************\"!".to_vec()
        );
        // A clean chunked body comes through whole.
        let (result, got) = streamed(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
            b"real-vendor-key",
        );
        result.unwrap();
        let text = String::from_utf8(got).unwrap();
        let (_, body) = text.split_once("\r\n\r\n").unwrap();
        assert_eq!(unchunked(body.as_bytes()), b"abcde".to_vec());
    }

    #[test]
    fn broken_chunked_framing_is_cut_off() {
        for broken in [
            &b"zz\r\nabc\r\n0\r\n\r\n"[..], // a size that isn't hex
            &b"5\r\nabc"[..],               // the answer ends inside a chunk
            &b"3\r\nabcX\r\n0\r\n\r\n"[..], // a chunk longer than its size
            &b"3\r\nabc\r\n"[..],           // no last chunk
        ] {
            let mut bytes = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            bytes.extend_from_slice(broken);
            let (result, got) = streamed(&bytes, b"real-vendor-key");
            assert!(result.is_err(), "{}", String::from_utf8_lossy(broken));
            assert!(
                !got.ends_with(b"0\r\n\r\n"),
                "a broken body is never ended cleanly"
            );
        }
    }

    #[test]
    fn an_answer_the_scrub_cannot_read_is_refused() {
        for encoding in [
            "Content-Encoding: gzip",
            "Content-Encoding: br",
            "Transfer-Encoding: gzip, chunked",
        ] {
            let (result, got) = streamed(
                format!("HTTP/1.1 200 OK\r\n{encoding}\r\n\r\nsecret").as_bytes(),
                b"real-vendor-key",
            );
            assert!(result.is_err(), "{encoding}");
            let text = String::from_utf8(got).unwrap();
            assert!(text.starts_with("HTTP/1.1 502 "), "{encoding}: {text}");
            assert!(!text.contains("secret"), "{encoding}: {text}");
        }
        let (result, _) = streamed(
            b"HTTP/1.1 200 OK\r\nContent-Encoding: identity\r\nContent-Length: 2\r\n\r\nok",
            b"real-vendor-key",
        );
        result.unwrap();
    }

    #[test]
    fn a_request_asking_the_vendor_to_fetch_is_refused_and_never_sent() {
        let (port, requests) = mock_vendor(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let context = context(vendor(Wire::Anthropic, port));
        let fetch = br#"{"model":"m","max_tokens":1,"messages":[],"tools":[{"type":"web_fetch_20250910","name":"web_fetch"}]}"#;
        let mut sent = format!(
            "POST /v1/messages HTTP/1.1\r\nx-api-key: sk-ant-hatchling-cell\r\nContent-Length: {}\r\n\r\n",
            fetch.len()
        )
        .into_bytes();
        sent.extend_from_slice(fetch);
        let (forwarded, answer) = relay_once(&context, &sent);
        assert!(!forwarded);
        assert!(answer.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{answer}");
        assert!(answer.contains("a tool the vendor runs"), "{answer}");
        assert!(
            requests.recv_timeout(SOON).is_err(),
            "nothing reached the vendor"
        );
    }

    #[test]
    fn a_vendor_that_echoes_its_key_never_hands_it_to_the_cell() {
        let (port, requests) = mock_vendor(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 34\r\n\r\n{\"bad key\":\"Bearer real-vendor-key\"}",
        );
        let context = context(vendor(Wire::OpenAi, port));
        let (forwarded, answer) = relay_once(
            &context,
            b"POST /v1/responses HTTP/1.1\r\nAuthorization: Bearer sk-ant-hatchling-cell\r\n\
              Accept-Encoding: gzip\r\nContent-Length: 2\r\n\r\n{}",
        );
        assert!(forwarded);
        assert!(!answer.contains("real-vendor-key"), "{answer}");
        assert!(
            answer.ends_with("{\"bad key\":\"Bearer ***************\"}"),
            "{answer}"
        );
        let sent = String::from_utf8(requests.recv_timeout(SOON).unwrap()).unwrap();
        assert!(sent.contains("\r\nAccept-Encoding: identity\r\n"), "{sent}");
        assert!(
            !sent.contains("gzip"),
            "the cell's encodings are dropped: {sent}"
        );
    }

    #[test]
    fn an_answer_holds_at_most_its_header_bound() {
        let answer = |count: usize| {
            let mut bytes = b"HTTP/1.1 200 OK\r\n".to_vec();
            for index in 0..count {
                bytes.extend_from_slice(format!("h{index}: v\r\n").as_bytes());
            }
            bytes.extend_from_slice(b"\r\n");
            let (_client, server) = pair();
            stream_back(&mut Cursor::new(bytes), &server, b"k")
        };
        assert!(answer(MAX_HEADERS).is_ok());
        assert!(answer(MAX_HEADERS + 1).is_err());
    }

    #[test]
    fn an_answer_is_cut_at_its_bound() {
        let (client, server) = pair();
        let reading = std::thread::spawn(move || {
            let mut client = client;
            let mut sink = Vec::new();
            client.read_to_end(&mut sink).unwrap();
            sink.len()
        });
        let mut endless = (&b"HTTP/1.1 200 OK\r\n\r\n"[..]).chain(std::io::repeat(b'x'));
        stream_back(&mut endless, &server, b"real-vendor-key").unwrap();
        drop(server);
        let sent = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".len();
        assert_eq!(
            reading.join().unwrap(),
            sent + usize::try_from(MAX_RESPONSE_BYTES).unwrap()
        );
    }

    #[test]
    fn a_local_answer_names_its_status_and_reason_in_json() {
        for (status, text) in [
            (400, "Bad Request"),
            (401, "Unauthorized"),
            (404, "Not Found"),
            (413, "Content Too Large"),
            (431, "Request Header Fields Too Large"),
            (502, "Bad Gateway"),
            (503, "Service Unavailable"),
        ] {
            let (mut client, server) = pair();
            answer(&server, status, "why").unwrap();
            drop(server);
            let mut got = String::new();
            client.read_to_string(&mut got).unwrap();
            let (head, body) = got.split_once("\r\n\r\n").unwrap();
            assert_eq!(
                head,
                format!(
                    "HTTP/1.1 {status} {text}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close",
                    body.len()
                )
            );
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(body).unwrap(),
                serde_json::json!({"error": {"type": "hatchling_relay", "message": "why"}})
            );
        }
    }

    #[test]
    fn one_request_is_forwarded_only_with_the_runs_key_on_the_vendors_api() {
        let (port, got) = mock_vendor(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        let context = context(vendor(Wire::Anthropic, port));
        let (forwarded, answer) = relay_once(
            &context,
            b"POST /v1/messages HTTP/1.1\r\nx-api-key: sk-ant-hatchling-cell\r\n\
              Content-Length: 2\r\n\r\n{}",
        );
        assert!(forwarded);
        assert_eq!(
            answer,
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
        );
        let sent = String::from_utf8(got.recv_timeout(SOON).unwrap()).unwrap();
        assert!(
            sent.contains("\r\nx-api-key: real-vendor-key\r\n"),
            "{sent}"
        );
        assert!(!sent.contains("hatchling-cell"), "{sent}");
        for (bytes, status) in [
            (
                &b"POST /v1/messages HTTP/1.1\r\nx-api-key: forged\r\n\r\n"[..],
                "401",
            ),
            (b"POST /v1/messages HTTP/1.1\r\n\r\n", "401"),
            (
                b"GET /v1/messages HTTP/1.1\r\nx-api-key: sk-ant-hatchling-cell\r\n\r\n",
                "404",
            ),
            (b"POST /v1/messages HTTP/1.0\r\n\r\n", "400"),
            (
                b"POST /v1/messages HTTP/1.1\r\nx-api-key: sk-ant-hatchling-cell\r\n\
                  Content-Length: 9\r\n\r\n{}",
                "400",
            ),
        ] {
            let (forwarded, answer) = relay_once(&context, bytes);
            assert!(!forwarded, "{answer}");
            assert!(
                answer.starts_with(&format!("HTTP/1.1 {status} ")),
                "{answer}"
            );
        }
        assert!(got.try_recv().is_err(), "a refusal reached the vendor");
    }

    #[test]
    fn a_vendor_that_cannot_be_reached_is_a_bad_gateway() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let context = context(vendor(Wire::OpenAi, port));
        let (forwarded, answer) = relay_once(
            &context,
            b"GET /v1/models HTTP/1.1\r\nAuthorization: Bearer sk-ant-hatchling-cell\r\n\r\n",
        );
        assert!(!forwarded);
        assert!(answer.starts_with("HTTP/1.1 502 "), "{answer}");
    }

    /// A relay started on a fresh handoff socket, and the cell's side.
    fn started(vendor: Vendor, deadline: Instant) -> (Relay, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-relay-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("handoff");
        let handoff = UnixListener::bind(&path).unwrap();
        let relay = Relay::start(
            handoff,
            vendor,
            Zeroizing::new(String::from("sk-ant-hatchling-cell")),
            Zeroizing::new(String::from("real-vendor-key")),
            deadline,
        );
        (relay, path)
    }

    /// Finishes `relay`, which must stop promptly: the tests' deadlines
    /// are far past [`SOON`], so a relay deaf to its stop fails here
    /// instead of hanging.
    fn finish_soon(relay: Relay) -> (RelayCount, Option<String>) {
        let started = Instant::now();
        let finished = relay.finish();
        assert!(started.elapsed() < SOON, "the relay did not stop");
        finished
    }

    /// Hands `listener` over on `path`, as a cell does.
    fn hand_over(path: &std::path::Path, listener: &TcpListener) {
        let channel = std::os::unix::net::UnixStream::connect(path).unwrap();
        hatchling_sys::net::send_fd(channel.as_fd(), listener.as_fd()).unwrap();
    }

    fn ask(port: u16, bytes: &[u8]) -> String {
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client.write_all(bytes).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut answer = String::new();
        client.read_to_string(&mut answer).unwrap();
        answer
    }

    #[test]
    fn a_relay_serves_its_cells_listener_and_counts_what_it_did() {
        let (vendor_port, got) = mock_vendor(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        let (relay, path) = started(
            vendor(Wire::Anthropic, vendor_port),
            Instant::now() + Duration::from_secs(20),
        );
        let cell = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = cell.local_addr().unwrap().port();
        hand_over(&path, &cell);
        drop(cell);
        let ok = ask(
            port,
            b"POST /v1/messages HTTP/1.1\r\nx-api-key: sk-ant-hatchling-cell\r\n\r\n",
        );
        assert!(ok.starts_with("HTTP/1.1 200 OK"), "{ok}");
        got.recv_timeout(SOON).unwrap();
        let refused = ask(port, b"POST /v1/messages HTTP/1.1\r\nx-api-key: no\r\n\r\n");
        assert!(refused.starts_with("HTTP/1.1 401 "), "{refused}");
        // The counter of a connection's thread moves after its answer.
        let until = Instant::now() + SOON;
        while relay.count()
            != (RelayCount {
                forwarded: 1,
                refused: 1,
            })
        {
            assert!(Instant::now() < until, "{:?}", relay.count());
            std::thread::sleep(POLL);
        }
        let (count, problem) = finish_soon(relay);
        assert_eq!(
            count,
            RelayCount {
                forwarded: 1,
                refused: 1
            }
        );
        assert_eq!(problem, None);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn past_its_bound_of_live_connections_the_relay_answers_503() {
        let (relay, path) = started(
            vendor(Wire::Anthropic, 1),
            Instant::now() + Duration::from_secs(20),
        );
        let cell = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = cell.local_addr().unwrap().port();
        hand_over(&path, &cell);
        drop(cell);
        // Each silent connection holds its thread at its first read.
        let mut silent = Vec::new();
        for _ in 0..MAX_LIVE {
            silent.push(TcpStream::connect(("127.0.0.1", port)).unwrap());
        }
        // Accepted in order, each counted live as it is accepted: the
        // next one is past the bound.
        let answer = ask(port, b"");
        assert!(answer.starts_with("HTTP/1.1 503 "), "{answer}");
        drop(silent);
        let until = Instant::now() + SOON;
        while relay.count().refused < MAX_LIVE + 1 {
            assert!(Instant::now() < until, "{:?}", relay.count());
            std::thread::sleep(POLL);
        }
        let (count, _) = finish_soon(relay);
        assert_eq!(
            count,
            RelayCount {
                forwarded: 0,
                refused: MAX_LIVE + 1
            }
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_handoff_that_never_comes_or_is_no_listener_is_the_runs_problem() {
        let (relay, path) = started(vendor(Wire::OpenAi, 1), Instant::now() + POLL);
        let (count, problem) = finish_soon(relay);
        assert_eq!(count, RelayCount::default());
        assert_eq!(
            problem.as_deref(),
            Some("relay handoff: the cell's listener never came")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        // A connected socket is not a listener.
        let (relay, path) = started(
            vendor(Wire::OpenAi, 1),
            Instant::now() + Duration::from_secs(20),
        );
        let (client, _server) = pair();
        let channel = std::os::unix::net::UnixStream::connect(&path).unwrap();
        hatchling_sys::net::send_fd(channel.as_fd(), client.as_fd()).unwrap();
        let (_, problem) = finish_soon(relay);
        assert_eq!(
            problem.as_deref(),
            Some("relay handoff: not a listening socket")
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_dropped_relay_stops_waiting_for_its_handoff() {
        let (relay, path) = started(
            vendor(Wire::OpenAi, 1),
            Instant::now() + Duration::from_secs(20),
        );
        drop(relay);
        // Ten of its polls later, a listener offered on the handoff anyway
        // must never be served. (A probe that only connects proves nothing:
        // a relay still waiting takes the connection for a handoff, fails
        // on it, and stops by itself.)
        std::thread::sleep(POLL * 10);
        let cell = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = cell.local_addr().unwrap().port();
        if let Ok(channel) = std::os::unix::net::UnixStream::connect(&path) {
            let _ = hatchling_sys::net::send_fd(channel.as_fd(), cell.as_fd());
        }
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"POST /v1/responses HTTP/1.1\r\n\r\n")
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut answer = Vec::new();
        let _ = client.read_to_end(&mut answer);
        assert!(
            answer.is_empty(),
            "a dropped relay served: {}",
            String::from_utf8_lossy(&answer)
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn serving_ends_at_once_when_its_listener_fails() {
        // A connected stream's fd taken as a listener: `accept` on it is
        // EINVAL, not "nothing yet", and serving ends there.
        let (stream, _server) = pair();
        let broken = TcpListener::from(std::os::fd::OwnedFd::from(stream));
        let context = Arc::new(context(vendor(Wire::OpenAi, 1)));
        bounded(move || {
            serve(
                &broken,
                &context,
                Instant::now() + Duration::from_secs(20),
                &AtomicBool::new(false),
            );
        });
    }

    #[test]
    fn a_handoff_socket_that_fails_is_the_runs_problem_at_once() {
        // A connected stream's fd taken as a listener: `accept` on it is
        // EINVAL, an error that is not "nothing yet".
        let (stream, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let broken = UnixListener::from(std::os::fd::OwnedFd::from(stream));
        let started = Instant::now();
        let failed = receive(
            &broken,
            Instant::now() + Duration::from_secs(20),
            &AtomicBool::new(false),
        );
        let reason = failed.unwrap_err();
        assert!(reason.starts_with("relay handoff: "), "{reason}");
        assert_ne!(reason, "relay handoff: the cell's listener never came");
        assert!(started.elapsed() < SOON, "it waited instead of failing");
    }

    /// Runs `wait` on a thread of its own and gives what it returned, with
    /// how long it took; a wait that does not end within [`SOON`] fails the
    /// test instead of hanging it (its thread is left behind).
    fn bounded<T: Send + 'static>(wait: impl FnOnce() -> T + Send + 'static) -> (T, Duration) {
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = tx.send(wait());
        });
        let got = rx.recv_timeout(SOON);
        assert!(got.is_ok(), "the wait did not end");
        (got.unwrap(), started.elapsed())
    }

    #[test]
    fn a_handoff_wait_ends_at_its_deadline_or_its_stop() {
        let dir = std::env::temp_dir().join(format!("hatchling-relay-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let never = "relay handoff: the cell's listener never came";
        let path = dir.join("deadline");
        let handoff = UnixListener::bind(&path).unwrap();
        let (waited, took) = bounded(move || {
            receive(
                &handoff,
                Instant::now() + Duration::from_millis(200),
                &AtomicBool::new(false),
            )
        });
        assert_eq!(waited.unwrap_err(), never);
        assert!(took >= Duration::from_millis(200));
        let handoff = UnixListener::bind(dir.join("stop")).unwrap();
        let (stopped, _) = bounded(move || {
            receive(
                &handoff,
                Instant::now() + Duration::from_secs(20),
                &AtomicBool::new(true),
            )
        });
        assert_eq!(stopped.unwrap_err(), never);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serving_ends_at_its_deadline_or_its_stop() {
        let cell = TcpListener::bind("127.0.0.1:0").unwrap();
        let context = Arc::new(context(vendor(Wire::OpenAi, 1)));
        let shared = Arc::clone(&context);
        let ((), took) = bounded(move || {
            serve(
                &cell,
                &shared,
                Instant::now() + Duration::from_millis(200),
                &AtomicBool::new(false),
            );
        });
        assert!(took >= Duration::from_millis(200));
        let cell = TcpListener::bind("127.0.0.1:0").unwrap();
        let shared = Arc::clone(&context);
        bounded(move || {
            serve(
                &cell,
                &shared,
                Instant::now() + Duration::from_secs(20),
                &AtomicBool::new(true),
            );
        });
        assert_eq!(context.counts.1.load(Ordering::SeqCst), 0);
    }
}
