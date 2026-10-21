//! A browser cell's only way out (WO-029, part 2): an HTTPS `CONNECT`
//! proxy, served on the cell's relay.
//!
//! A browser cell has no network but its own loopback, and on it one
//! listener, the relay, which the cell hands over and keeps no copy of.
//! Chromium's proxy is that listener, so every connection the browser
//! makes reaches the broker, here:
//!
//! - **`CONNECT` only, to `https` on its default port.** Anything else, a
//!   plain request aimed at the listener included, is refused.
//! - **A site by name,** held to the rules a URL's authority is
//!   ([`Origin::from_authority`]): no IP address, no user.
//! - **Only a site the session holds** ([`Sites`]), checked **before** its
//!   name is resolved: a name never granted never reaches a resolver, so a
//!   secret spelled into one can't leave as a lookup. Every session is
//!   strict (WO-029): what a page loads from another site is held to the
//!   same grants as where it is, and the sites it was refused are kept for
//!   the owner to see. A site revoked loses the tunnels it had open, not
//!   only new ones.
//! - **Resolved for each connection, and refused if any address it
//!   resolves to is internal** (WO-023's [`is_internal`]); the connection
//!   is then made to the addresses checked, never a fresh lookup.
//! - **TLS end to end:** the bytes are passed as they are. The broker never
//!   sees plaintext, and the browser checks the site's certificate itself.
//! - **Every connection is recorded** with its site, in the egress
//!   decisions the operator's Machine view shows, and its byte counts in
//!   the broker's log.
//!
//! What a `CONNECT` proxy can check is the site and port a tunnel goes to,
//! never the method, path or body inside it: those are the browser's, in
//! TLS. Which sites a session holds is the broker's to decide (part 3).

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs as _};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hatchling_core::ui::operator::EgressVerdict;
use hatchling_egress::allowlist::is_internal;
use hatchling_egress::web::{FetchError, Origin, resolved};

/// The port of a browser cell's relay, on its own loopback.
pub const RELAY_PORT: u16 = 8118;

/// The one port a tunnel may name: `https`'s.
pub const HTTPS_PORT: u16 = 443;

/// Longest request line or header line.
const MAX_LINE_BYTES: usize = 8192;
/// Most header lines a `CONNECT` may carry.
const MAX_HEADERS: usize = 100;
/// How long the browser has to send its request.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a connection to a site may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a tunnel may sit with nothing passing, either way.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Most bytes a tunnel passes each way: 256 MiB. Downloads are out of
/// scope; a page is far smaller.
const MAX_TUNNEL_BYTES: u64 = 268_435_456;
/// Most tunnels open at once: a browser opens several per site.
pub const MAX_LIVE: u32 = 64;
/// Most connections past [`MAX_LIVE`] told why at once; past them too, a
/// connection is dropped unanswered.
pub const MAX_BUSY: u32 = 64;
/// Most sites one session keeps as refused, for the owner to see; past
/// them a refused site is still refused, but not kept.
pub const MAX_REFUSED_SITES: usize = 64;
/// How long a connection refused for [`MAX_LIVE`] has to send its request
/// before it's answered anyway.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the accept loop looks for a stop.
const POLL: Duration = Duration::from_millis(20);
/// One piece of a tunnel's copy.
const PIECE: usize = 16_384;

/// A lookup: a destination's addresses.
type Resolve = dyn Fn(&str) -> std::io::Result<Vec<SocketAddr>> + Send + Sync;

/// Which addresses are internal.
type Internal = dyn Fn(IpAddr) -> bool + Send + Sync;

/// How a site's addresses are found, and which of them are internal.
#[derive(Clone)]
pub struct Route {
    resolve: Arc<Resolve>,
    internal: Arc<Internal>,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Route")
    }
}

impl Route {
    /// The system's resolver, and WO-023's internal addresses.
    #[must_use]
    pub fn public() -> Self {
        Self::with(
            |destination| destination.to_socket_addrs().map(Iterator::collect),
            is_internal,
        )
    }

    /// `resolve` for the addresses, `internal` for which are internal.
    #[must_use]
    pub fn with(
        resolve: impl Fn(&str) -> std::io::Result<Vec<SocketAddr>> + Send + Sync + 'static,
        internal: impl Fn(IpAddr) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            resolve: Arc::new(resolve),
            internal: Arc::new(internal),
        }
    }
}

/// The sites one browser session may reach (WO-029: every session is
/// strict).
///
/// The broker grants and revokes them; the proxy admits a
/// `CONNECT` only to a site the session holds, before its name is
/// resolved, and a site revoked loses the tunnels it had open. A site is
/// `host:port`, as [`Origin::destination`] gives it. Clones share one set.
#[derive(Debug, Clone, Default)]
pub struct Sites(Arc<Mutex<Held>>);

#[derive(Debug, Default)]
struct Held {
    granted: BTreeSet<String>,
    refused: BTreeSet<String>,
    open: Vec<Open>,
    next: u64,
}

/// An open tunnel, as revoking its site closes it.
#[derive(Debug)]
struct Open {
    id: u64,
    destination: String,
    client: TcpStream,
    site: TcpStream,
}

impl Sites {
    fn held(&self) -> MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Grants `destination`: no longer one the session was refused.
    pub fn grant(&self, destination: &str) {
        let mut held = self.held();
        held.refused.remove(destination);
        held.granted.insert(destination.to_owned());
    }

    /// Revokes `destination`, and closes every tunnel open to it, both
    /// ways: how many it closed.
    #[must_use]
    pub fn revoke(&self, destination: &str) -> usize {
        let mut held = self.held();
        held.granted.remove(destination);
        let (closing, kept): (Vec<Open>, Vec<Open>) = std::mem::take(&mut held.open)
            .into_iter()
            .partition(|open| open.destination == destination);
        held.open = kept;
        drop(held);
        for open in &closing {
            let _ = open.client.shutdown(Shutdown::Both);
            let _ = open.site.shutdown(Shutdown::Both);
        }
        closing.len()
    }

    /// Whether the session holds `destination`.
    #[must_use]
    pub fn holds(&self, destination: &str) -> bool {
        self.held().granted.contains(destination)
    }

    /// The sites the session was refused and doesn't hold since, in order:
    /// at most [`MAX_REFUSED_SITES`].
    #[must_use]
    pub fn refused(&self) -> Vec<String> {
        self.held().refused.iter().cloned().collect()
    }

    /// Whether `destination` is admitted. One that isn't is kept as
    /// refused, while there's room.
    pub(crate) fn admit(&self, destination: &str) -> bool {
        let mut held = self.held();
        if held.granted.contains(destination) {
            return true;
        }
        if held.refused.len() < MAX_REFUSED_SITES {
            held.refused.insert(destination.to_owned());
        }
        false
    }

    /// The tunnel from `client` to `site`, for `destination`, registered so
    /// a revoke can close it: `None` if the session no longer holds the
    /// site, or the tunnel can't be held to close.
    fn opened(&self, destination: &str, client: &TcpStream, site: &TcpStream) -> Option<Opened> {
        let (client, site) = (client.try_clone().ok()?, site.try_clone().ok()?);
        let id = {
            let mut held = self.held();
            if !held.granted.contains(destination) {
                return None;
            }
            let id = held.next;
            held.next = id.wrapping_add(1);
            held.open.push(Open {
                id,
                destination: destination.to_owned(),
                client,
                site,
            });
            id
        };
        Some(Opened {
            sites: self.clone(),
            id,
        })
    }
}

/// A tunnel's registration with its session's [`Sites`], removed when the
/// tunnel ends.
struct Opened {
    sites: Sites,
    id: u64,
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.sites.held().open.retain(|open| open.id != self.id);
    }
}

/// What a browser cell's proxy did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProxyCount {
    /// Tunnels opened to a site.
    pub tunnelled: u32,
    /// Requests refused, never connected.
    pub refused: u32,
}

/// The proxy serving one browser cell's relay.
#[derive(Debug)]
pub struct BrowserProxy {
    stop: Arc<AtomicBool>,
    context: Arc<Context>,
    serving: Option<JoinHandle<()>>,
}

/// What every tunnel of one cell shares.
#[derive(Debug)]
struct Context {
    cell_id: String,
    route: Route,
    sites: Sites,
    tunnelled: AtomicU32,
    refused: AtomicU32,
    live: AtomicU32,
}

/// A tunnel's place among its cell's [`MAX_LIVE`], given back when its
/// thread ends, however it ends.
struct Live(Arc<Context>);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

impl BrowserProxy {
    /// Serves `listener`, the relay of the browser cell `cell_id`, to the
    /// `sites` its session holds, until [`BrowserProxy::finish`] or
    /// `deadline`.
    #[must_use]
    pub fn start(
        listener: TcpListener,
        cell_id: &str,
        route: Route,
        sites: Sites,
        deadline: Instant,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let context = Arc::new(Context {
            cell_id: cell_id.to_owned(),
            route,
            sites,
            tunnelled: AtomicU32::new(0),
            refused: AtomicU32::new(0),
            live: AtomicU32::new(0),
        });
        let (flag, shared) = (Arc::clone(&stop), Arc::clone(&context));
        let serving = std::thread::spawn(move || serve(&listener, &shared, deadline, &flag));
        Self {
            stop,
            context,
            serving: Some(serving),
        }
    }

    /// Stops accepting, and says what was done. A tunnel still open ends
    /// on its own bounds.
    #[must_use]
    pub fn finish(mut self) -> ProxyCount {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
        self.count()
    }

    /// What was done so far.
    #[must_use]
    pub fn count(&self) -> ProxyCount {
        ProxyCount {
            tunnelled: self.context.tunnelled.load(Ordering::SeqCst),
            refused: self.context.refused.load(Ordering::SeqCst),
        }
    }
}

impl Drop for BrowserProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Serves `listener` until `stop` or `deadline`: each connection on its
/// own thread, at most [`MAX_LIVE`] tunnels at once. Past them, up to
/// [`MAX_BUSY`] more are told so; past those, a connection is dropped.
fn serve(listener: &TcpListener, context: &Arc<Context>, deadline: Instant, stop: &AtomicBool) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
        match listener.accept() {
            Ok((client, _)) => {
                let live = context.live.load(Ordering::SeqCst);
                if live >= MAX_LIVE + MAX_BUSY {
                    context.refused.fetch_add(1, Ordering::SeqCst);
                    drop(client);
                    continue;
                }
                context.live.fetch_add(1, Ordering::SeqCst);
                let busy = live >= MAX_LIVE;
                let live = Live(Arc::clone(context));
                std::thread::spawn(move || {
                    let tunnelled = if busy {
                        refuse_busy(&client);
                        false
                    } else {
                        tunnel(&client, &live.0)
                    };
                    let counter = if tunnelled {
                        &live.0.tunnelled
                    } else {
                        &live.0.refused
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

/// Answers a connection past [`MAX_LIVE`] with `503`, once its request is
/// read: a socket closed with its request unread is reset, and a reset can
/// reach the browser before the answer does.
fn refuse_busy(client: &TcpStream) {
    let set = client
        .set_read_timeout(Some(BUSY_TIMEOUT))
        .and_then(|()| client.set_write_timeout(Some(BUSY_TIMEOUT)));
    if set.is_err() {
        return;
    }
    let _ = read_connect(&mut BufReader::new(client));
    let _ = answer(client, 503, "Too Many Connections");
    let _ = client.shutdown(Shutdown::Write);
}

/// Why a request isn't tunnelled: its status, its reason, and what the
/// operator's log says of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Refusal {
    status: u16,
    reason: &'static str,
    verdict: EgressVerdict,
}

const fn refusal(status: u16, reason: &'static str, verdict: EgressVerdict) -> Refusal {
    Refusal {
        status,
        reason,
        verdict,
    }
}

/// One connection: its `CONNECT` read and checked, then tunnelled, or
/// answered here. Whether it was tunnelled.
fn tunnel(client: &TcpStream, context: &Context) -> bool {
    let set = client
        .set_nonblocking(false)
        .and_then(|()| client.set_read_timeout(Some(HEAD_TIMEOUT)))
        .and_then(|()| client.set_write_timeout(Some(HEAD_TIMEOUT)));
    if set.is_err() {
        return false;
    }
    let mut reader = BufReader::new(client);
    let (destination, opened) = match read_connect(&mut reader)
        .and_then(|authority| open(&authority, &context.route, &context.sites))
    {
        Ok(opened) => opened,
        Err((refused, named)) => {
            refuse(client, context, refused, named.as_deref());
            return false;
        }
    };
    // Revoked since it was admitted: the revoke wins.
    let Some(_open) = context.sites.opened(&destination, client, &opened) else {
        let revoked = refusal(403, "Forbidden", EgressVerdict::NoGrant);
        refuse(client, context, revoked, Some(&destination));
        return false;
    };
    crate::machine::decided(&context.cell_id, &destination, EgressVerdict::Allowed);
    // Whatever the browser sent after its request is the tunnel's first
    // bytes.
    let early = reader.buffer().to_vec();
    drop(reader);
    let (up, down) = splice(client, &opened, &early);
    tracing::info!(
        cell = %context.cell_id,
        destination = %destination,
        up,
        down,
        "browser connection"
    );
    true
}

/// Answers `client` with `refused`, and records it with the site it names,
/// if it got as far as naming one.
fn refuse(client: &TcpStream, context: &Context, refused: Refusal, named: Option<&str>) {
    if let Some(destination) = named {
        crate::machine::decided(&context.cell_id, destination, refused.verdict);
    }
    tracing::info!(
        cell = %context.cell_id,
        destination = named.unwrap_or("-"),
        status = refused.status,
        reason = refused.reason,
        "browser connection refused"
    );
    let _ = answer(client, refused.status, refused.reason);
    let _ = client.shutdown(Shutdown::Write);
}

/// A refusal, and the destination it names, if the request got as far as
/// naming a valid one.
type Refused = (Refusal, Option<String>);

/// Reads a `CONNECT` request's head, and gives its `host:port`.
fn read_connect(reader: &mut BufReader<&TcpStream>) -> Result<String, Refused> {
    let bad = |reason| (refusal(400, reason, EgressVerdict::NotAllowed), None);
    let line = read_line(reader).ok_or_else(|| bad("Bad Request"))?;
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad("Bad Request"));
    };
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(bad("Bad Request"));
    }
    for _ in 0..=MAX_HEADERS {
        match read_line(reader) {
            Some(header) if header.is_empty() => {
                if method != "CONNECT" {
                    return Err((
                        refusal(405, "Method Not Allowed", EgressVerdict::NotAllowed),
                        None,
                    ));
                }
                return Ok(target.to_owned());
            }
            Some(_) => {}
            None => return Err(bad("Bad Request")),
        }
    }
    Err((
        refusal(
            431,
            "Request Header Fields Too Large",
            EgressVerdict::NotAllowed,
        ),
        None,
    ))
}

/// One line of the head, without its line ending: `None` past
/// [`MAX_LINE_BYTES`], at a read error, or at the end before a line ends.
fn read_line(reader: &mut BufReader<&TcpStream>) -> Option<String> {
    let mut line = Vec::new();
    let limit = u64::try_from(MAX_LINE_BYTES).unwrap_or(u64::MAX) + 1;
    let read = Read::take(&mut *reader, limit)
        .read_until(b'\n', &mut line)
        .ok()?;
    if read == 0 || line.len() > MAX_LINE_BYTES || line.last() != Some(&b'\n') {
        return None;
    }
    line.pop();
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).ok()
}

/// The connection to the site `authority` names: checked, admitted if
/// `sites` holds it, then resolved, refused if internal, and made to the
/// addresses checked. The site, as `host:port`, and the connection.
fn open(authority: &str, route: &Route, sites: &Sites) -> Result<(String, TcpStream), Refused> {
    let origin = Origin::from_authority(authority)
        .map_err(|_| (refusal(400, "Bad Request", EgressVerdict::NotAllowed), None))?;
    let destination = origin.destination();
    let named =
        |status, reason, verdict| (refusal(status, reason, verdict), Some(destination.clone()));
    if origin.port() != HTTPS_PORT {
        return Err(named(403, "Forbidden", EgressVerdict::NotAllowed));
    }
    // Before the name is resolved: a lookup is itself a way out.
    if !sites.admit(&destination) {
        return Err(named(403, "Forbidden", EgressVerdict::NoGrant));
    }
    let addrs = resolved(
        &destination,
        |d| (route.resolve)(d),
        |ip| (route.internal)(ip),
    )
    .map_err(|error| match error {
        FetchError::Internal { .. } => named(403, "Forbidden", EgressVerdict::NotAllowed),
        FetchError::Unresolved
        | FetchError::Failed
        | FetchError::TooLarge
        | FetchError::NotText { .. }
        | FetchError::NoLocation
        | FetchError::BadRedirect(_) => named(502, "Bad Gateway", EgressVerdict::Failed),
    })?;
    let opened = addrs
        .iter()
        .find_map(|addr| TcpStream::connect_timeout(addr, CONNECT_TIMEOUT).ok())
        .ok_or_else(|| named(502, "Bad Gateway", EgressVerdict::Failed))?;
    Ok((destination, opened))
}

/// Answers `client` with `status` and nothing more.
fn answer(client: &TcpStream, status: u16, reason: &str) -> std::io::Result<()> {
    let mut client = client;
    write!(
        client,
        "HTTP/1.1 {status} {reason}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    )?;
    client.flush()
}

/// Opens the tunnel and passes bytes both ways until either side ends,
/// stalls past [`IDLE_TIMEOUT`] or passes [`MAX_TUNNEL_BYTES`]: what went
/// up and what came down.
fn splice(client: &TcpStream, site: &TcpStream, early: &[u8]) -> (u64, u64) {
    let ready = [client, site].into_iter().try_for_each(|stream| {
        stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
        stream.set_write_timeout(Some(IDLE_TIMEOUT))
    });
    let opened = ready.and_then(|()| {
        let mut to_client = client;
        to_client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        let mut to_site = site;
        to_site.write_all(early)
    });
    if opened.is_err() {
        return (0, 0);
    }
    let (Ok(client_back), Ok(site_back)) = (client.try_clone(), site.try_clone()) else {
        return (0, 0);
    };
    let down = std::thread::spawn(move || copy(&site_back, &client_back, MAX_TUNNEL_BYTES));
    let up = copy(client, site, MAX_TUNNEL_BYTES) + u64::try_from(early.len()).unwrap_or(u64::MAX);
    let down = down.join().unwrap_or(0);
    (up, down)
}

/// Copies `from` to `to` until the end, an error, a stall or `max` bytes
/// (a piece may carry it past); then ends both, so the other direction
/// ends too. How much it copied.
fn copy(from: &TcpStream, to: &TcpStream, max: u64) -> u64 {
    let (mut reader, mut writer) = (from, to);
    let mut piece = vec![0u8; PIECE];
    let mut total: u64 = 0;
    while total < max {
        let read = match reader.read(&mut piece) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let Some(bytes) = piece.get(..read) else {
            break;
        };
        if writer.write_all(bytes).is_err() {
            break;
        }
        total += u64::try_from(read).unwrap_or(u64::MAX);
    }
    let _ = from.shutdown(Shutdown::Both);
    let _ = to.shutdown(Shutdown::Both);
    total
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::sync::Mutex;

    use super::*;

    /// A site on loopback that echoes what it's sent, counting the
    /// connections it takes.
    fn echo_site() -> (SocketAddr, Arc<AtomicU32>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let taken = Arc::new(AtomicU32::new(0));
        let count = Arc::clone(&taken);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                count.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut piece = [0u8; 1024];
                    while let Ok(read) = stream.read(&mut piece) {
                        let Some(bytes) = piece.get(..read).filter(|b| !b.is_empty()) else {
                            return;
                        };
                        if stream.write_all(bytes).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        (addr, taken)
    }

    /// The test's route: `site.test` is the echo site, `intranet.test` an
    /// internal address, `mixed.test` both, `nowhere.test` nothing.
    /// Loopback counts as public here, so the echo site can stand in for
    /// one; `10.0.0.0/8` is internal.
    fn route(site: SocketAddr) -> Route {
        let internal: SocketAddr = (Ipv4Addr::new(10, 0, 0, 9), 443).into();
        Route::with(
            move |destination| match destination {
                "site.test:443" => Ok(vec![site]),
                "intranet.test:443" => Ok(vec![internal]),
                "mixed.test:443" => Ok(vec![site, internal]),
                _ => Err(std::io::Error::other("no such host")),
            },
            |ip| matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 10),
        )
    }

    /// The test's sites, each granted: the route's four, so its other
    /// checks are reached.
    fn granted() -> Sites {
        let sites = Sites::default();
        for site in [
            "site.test:443",
            "intranet.test:443",
            "mixed.test:443",
            "nowhere.test:443",
        ] {
            sites.grant(site);
        }
        sites
    }

    /// A proxy on its own loopback listener, for the cell `cell`, holding
    /// `sites`.
    fn proxy_holding(site: SocketAddr, cell: &str, sites: Sites) -> (BrowserProxy, SocketAddr) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        (
            BrowserProxy::start(listener, cell, route(site), sites, deadline),
            addr,
        )
    }

    /// A proxy for the cell `cell`, holding every test site.
    fn proxy(site: SocketAddr, cell: &str) -> (BrowserProxy, SocketAddr) {
        proxy_holding(site, cell, granted())
    }

    /// Sends `request` to the proxy; its answer's status line, and the
    /// stream.
    fn ask(proxy: SocketAddr, request: &[u8]) -> (String, TcpStream) {
        let mut stream = TcpStream::connect(proxy).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(request).unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => head.extend_from_slice(&byte),
                _ => break,
            }
        }
        let status = String::from_utf8_lossy(&head)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        (status, stream)
    }

    /// The egress decisions kept for `cell`, oldest first.
    fn decisions(cell: &str) -> Vec<(String, EgressVerdict)> {
        crate::machine::decisions()
            .into_iter()
            .filter(|d| d.cell_id == cell)
            .map(|d| (d.destination, d.verdict))
            .collect()
    }

    /// Waits for `proxy`'s counts to settle at `want`.
    fn settled(proxy: &BrowserProxy, want: ProxyCount) -> ProxyCount {
        let deadline = Instant::now() + Duration::from_secs(10);
        while proxy.count() != want && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        proxy.count()
    }

    /// Held by the tests that read the egress decisions: one that floods
    /// them mustn't push these out.
    static DECIDING: Mutex<()> = Mutex::new(());

    #[test]
    fn a_connect_to_a_public_site_is_tunnelled_as_it_is_and_recorded() {
        let _deciding = DECIDING.lock().unwrap_or_else(PoisonError::into_inner);
        let _flood = crate::machine::FLOOD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (site, taken) = echo_site();
        let cell = "browser-proxy-tunnel";
        let (proxy, addr) = proxy(site, cell);
        // Bytes sent with the request are the tunnel's first.
        let (status, mut stream) = ask(
            addr,
            b"CONNECT Site.Test:443 HTTP/1.1\r\nHost: site.test:443\r\n\r\nearly",
        );
        assert_eq!(status, "HTTP/1.1 200 Connection Established");
        stream.write_all(b" and more").unwrap();
        let mut echoed = [0u8; 14];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"early and more");
        drop(stream);
        assert_eq!(
            settled(
                &proxy,
                ProxyCount {
                    tunnelled: 1,
                    refused: 0
                }
            ),
            ProxyCount {
                tunnelled: 1,
                refused: 0
            }
        );
        assert_eq!(taken.load(Ordering::SeqCst), 1);
        assert_eq!(
            decisions(cell),
            [(String::from("site.test:443"), EgressVerdict::Allowed)]
        );
        assert_eq!(
            proxy.finish(),
            ProxyCount {
                tunnelled: 1,
                refused: 0
            }
        );
    }

    /// Each request a browser cell's proxy refuses, and its answer's status
    /// line: never a connection to a site.
    fn refused_requests() -> Vec<(String, &'static str)> {
        let long = format!(
            "CONNECT {}.test:443 HTTP/1.1\r\n\r\n",
            "a".repeat(MAX_LINE_BYTES)
        );
        let headers = format!(
            "CONNECT site.test:443 HTTP/1.1\r\n{}\r\n",
            "X: y\r\n".repeat(MAX_HEADERS + 1)
        );
        vec![
            // A plain request, to the listener itself or through it.
            (
                String::from("GET http://site.test/ HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 405 Method Not Allowed",
            ),
            (
                String::from("GET / HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 405 Method Not Allowed",
            ),
            // Not https's port.
            (
                String::from("CONNECT site.test:80 HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 403 Forbidden",
            ),
            // Internal, wholly or in part.
            (
                String::from("CONNECT intranet.test:443 HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 403 Forbidden",
            ),
            (
                String::from("CONNECT mixed.test:443 HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 403 Forbidden",
            ),
            // Not a site by name.
            (
                String::from("CONNECT 10.0.0.9:443 HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 400 Bad Request",
            ),
            (
                String::from("CONNECT user@site.test:443 HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 400 Bad Request",
            ),
            (
                String::from("CONNECT site.test HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 400 Bad Request",
            ),
            // Not a request.
            (
                String::from("CONNECT site.test:443 HTTP/2\r\n\r\n"),
                "HTTP/1.1 400 Bad Request",
            ),
            (
                String::from("CONNECT site.test:443\r\n\r\n"),
                "HTTP/1.1 400 Bad Request",
            ),
            (
                String::from("CONNECT  site.test:443 HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 400 Bad Request",
            ),
            (long, "HTTP/1.1 400 Bad Request"),
            (headers, "HTTP/1.1 431 Request Header Fields Too Large"),
            // A site that resolves to nothing.
            (
                String::from("CONNECT nowhere.test:443 HTTP/1.1\r\n\r\n"),
                "HTTP/1.1 502 Bad Gateway",
            ),
        ]
    }

    #[test]
    fn anything_but_a_connect_to_a_public_https_site_is_refused_unconnected() {
        let _deciding = DECIDING.lock().unwrap_or_else(PoisonError::into_inner);
        let _flood = crate::machine::FLOOD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (site, taken) = echo_site();
        let cell = "browser-proxy-refused";
        let (proxy, addr) = proxy(site, cell);
        for (request, status) in refused_requests() {
            let (answered, _) = ask(addr, request.as_bytes());
            assert_eq!(
                answered,
                status,
                "{}",
                request.lines().next().unwrap_or_default()
            );
        }
        assert_eq!(
            settled(
                &proxy,
                ProxyCount {
                    tunnelled: 0,
                    refused: 14
                }
            ),
            ProxyCount {
                tunnelled: 0,
                refused: 14
            }
        );
        assert_eq!(taken.load(Ordering::SeqCst), 0, "nothing reached the site");
        // What named a site is recorded with it; what didn't, isn't.
        let kept = decisions(cell);
        let verdict = |destination: &str| {
            kept.iter()
                .filter(|(d, _)| d == destination)
                .map(|(_, v)| *v)
                .collect::<Vec<_>>()
        };
        assert_eq!(verdict("site.test:80"), [EgressVerdict::NotAllowed]);
        assert_eq!(verdict("intranet.test:443"), [EgressVerdict::NotAllowed]);
        assert_eq!(verdict("mixed.test:443"), [EgressVerdict::NotAllowed]);
        assert_eq!(verdict("nowhere.test:443"), [EgressVerdict::Failed]);
        assert_eq!(kept.len(), 4, "{kept:?}");
        let _ = proxy.finish();
    }

    #[test]
    fn a_site_that_takes_no_connection_is_a_bad_gateway() {
        // A port nothing listens on: resolved, public, refused.
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let site = closed.local_addr().unwrap();
        drop(closed);
        let (proxy, addr) = proxy(site, "browser-proxy-closed");
        let (status, _) = ask(addr, b"CONNECT site.test:443 HTTP/1.1\r\n\r\n");
        assert_eq!(status, "HTTP/1.1 502 Bad Gateway");
        assert_eq!(
            proxy.finish(),
            ProxyCount {
                tunnelled: 0,
                refused: 1
            }
        );
    }

    #[test]
    fn at_most_max_live_tunnels_are_open_at_once() {
        let (site, _) = echo_site();
        let (proxy, addr) = proxy(site, "browser-proxy-live");
        let open: Vec<TcpStream> = (0..MAX_LIVE)
            .map(|_| {
                let (status, stream) = ask(addr, b"CONNECT site.test:443 HTTP/1.1\r\n\r\n");
                assert_eq!(status, "HTTP/1.1 200 Connection Established");
                stream
            })
            .collect();
        let (status, _) = ask(addr, b"CONNECT site.test:443 HTTP/1.1\r\n\r\n");
        assert_eq!(status, "HTTP/1.1 503 Too Many Connections");
        // The busy ones told why are bounded too: past them, a connection
        // is dropped unanswered. These send nothing, so each holds its
        // place until its request times out.
        let waiting: Vec<TcpStream> = (0..MAX_BUSY)
            .map(|_| TcpStream::connect(addr).unwrap())
            .collect();
        let deadline = Instant::now() + Duration::from_secs(10);
        while proxy.context.live.load(Ordering::SeqCst) < MAX_LIVE + MAX_BUSY
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        // Its write may meet the drop: neither it nor the read may fail
        // the test, only an answer.
        let mut dropped = TcpStream::connect(addr).unwrap();
        dropped
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let _ = dropped.write_all(b"CONNECT site.test:443 HTTP/1.1\r\n\r\n");
        let mut heard = Vec::new();
        let _ = dropped.read_to_end(&mut heard);
        assert!(heard.is_empty(), "dropped unanswered: {heard:?}");
        let mut told = String::new();
        let mut first = waiting.first().unwrap();
        first.set_read_timeout(Some(BUSY_TIMEOUT * 3)).unwrap();
        let _ = first.read_to_string(&mut told);
        assert!(
            told.starts_with("HTTP/1.1 503 Too Many Connections"),
            "{told}"
        );
        drop(waiting);
        // One closed gives its place back.
        drop(open);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut status = String::new();
        while Instant::now() < deadline {
            status = ask(addr, b"CONNECT site.test:443 HTTP/1.1\r\n\r\n").0;
            if status.ends_with("200 Connection Established") {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(status, "HTTP/1.1 200 Connection Established");
        let _ = proxy.finish();
    }

    #[test]
    fn a_site_the_session_doesnt_hold_is_refused_before_its_name_is_looked_up() {
        let _deciding = DECIDING.lock().unwrap_or_else(PoisonError::into_inner);
        let _flood = crate::machine::FLOOD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (site, taken) = echo_site();
        // A route that records every name it's asked to look up.
        let asked = Arc::new(Mutex::new(Vec::<String>::new()));
        let lookups = Arc::clone(&asked);
        let looked_up = Route::with(
            move |destination| {
                lookups.lock().unwrap().push(destination.to_owned());
                Ok(vec![site])
            },
            |_| false,
        );
        let sites = Sites::default();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let cell = "browser-proxy-strict";
        let proxy = BrowserProxy::start(
            listener,
            cell,
            looked_up,
            sites.clone(),
            Instant::now() + Duration::from_secs(120),
        );
        // A secret spelled into a name the session doesn't hold.
        let (status, _) = ask(addr, b"CONNECT secret-1234.site.test:443 HTTP/1.1\r\n\r\n");
        assert_eq!(status, "HTTP/1.1 403 Forbidden");
        assert!(asked.lock().unwrap().is_empty(), "never looked up");
        assert_eq!(taken.load(Ordering::SeqCst), 0, "never reached");
        assert_eq!(sites.refused(), ["secret-1234.site.test:443"]);
        assert_eq!(
            decisions(cell),
            [(
                String::from("secret-1234.site.test:443"),
                EgressVerdict::NoGrant
            )]
        );
        // Granted, it's tunnelled, and no longer kept as refused.
        sites.grant("secret-1234.site.test:443");
        assert!(sites.holds("secret-1234.site.test:443"));
        assert!(sites.refused().is_empty());
        let (status, _) = ask(addr, b"CONNECT secret-1234.site.test:443 HTTP/1.1\r\n\r\n");
        assert_eq!(status, "HTTP/1.1 200 Connection Established");
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            ["secret-1234.site.test:443"]
        );
        let _ = proxy.finish();
    }

    #[test]
    fn a_revoked_site_loses_its_open_tunnels() {
        let (site, _) = echo_site();
        let sites = granted();
        let (proxy, addr) = proxy_holding(site, "browser-proxy-revoked", sites.clone());
        let (status, mut stream) = ask(addr, b"CONNECT site.test:443 HTTP/1.1\r\n\r\n");
        assert_eq!(status, "HTTP/1.1 200 Connection Established");
        stream.write_all(b"ping").unwrap();
        let mut echoed = [0u8; 4];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"ping");
        assert_eq!(sites.revoke("site.test:443"), 1, "the open tunnel closed");
        // The tunnel carries nothing more: the browser's end sees it close.
        let _ = stream.write_all(b"pong");
        let mut rest = Vec::new();
        let ended = stream.read_to_end(&mut rest);
        assert!(rest.is_empty(), "{rest:?}");
        let quiet = matches!(&ended, Err(error) if matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
        assert!(!quiet, "closed, not just quiet: {ended:?}");
        let (status, _) = ask(addr, b"CONNECT site.test:443 HTTP/1.1\r\n\r\n");
        assert_eq!(status, "HTTP/1.1 403 Forbidden", "nor opens a new one");
        let _ = proxy.finish();
    }

    #[test]
    fn a_tunnel_is_held_to_close_only_while_its_site_is() {
        let sites = granted();
        let (client, _browser) = pair();
        let (site, _far) = pair();
        let first = sites.opened("site.test:443", &client, &site).unwrap();
        let second = sites.opened("site.test:443", &client, &site).unwrap();
        let other = sites.opened("mixed.test:443", &client, &site).unwrap();
        // One ended: it's no longer held, and the other still is.
        drop(first);
        assert_eq!(sites.revoke("site.test:443"), 1);
        drop((second, other));
        assert_eq!(sites.revoke("mixed.test:443"), 0, "ended, so not held");
        // Revoked between admission and its connection: never registered.
        assert!(sites.opened("site.test:443", &client, &site).is_none());
    }

    #[test]
    fn the_sites_refused_are_kept_up_to_a_bound() {
        let sites = Sites::default();
        for n in 0..=MAX_REFUSED_SITES {
            let site = format!("site-{n:03}.test:443");
            assert!(!sites.admit(&site));
        }
        let refused = sites.refused();
        assert_eq!(refused.len(), MAX_REFUSED_SITES);
        assert_eq!(
            refused.first().map(String::as_str),
            Some("site-000.test:443")
        );
        let past = format!("site-{MAX_REFUSED_SITES:03}.test:443");
        assert!(!refused.contains(&past), "past the bound, not kept");
        sites.grant("site-000.test:443");
        assert!(sites.admit("site-000.test:443"));
        assert_eq!(sites.refused().len(), MAX_REFUSED_SITES - 1);
    }

    #[test]
    fn a_stopped_proxy_takes_nothing_more() {
        let (site, _) = echo_site();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        // The test keeps the port: freed, another test could be given it.
        let kept = listener.try_clone().unwrap();
        let proxy = BrowserProxy::start(
            listener,
            "browser-proxy-stopped",
            route(site),
            granted(),
            Instant::now() + Duration::from_secs(120),
        );
        assert_eq!(proxy.finish(), ProxyCount::default());
        // A connection made now is left for whoever else holds the port:
        // the stopped proxy never takes it.
        let _late = TcpStream::connect(addr).unwrap();
        kept.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let taken = loop {
            match kept.accept() {
                Ok(_) => break true,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break false,
            }
        };
        assert!(taken, "the stopped proxy took a connection");
        // A deadline already past serves nothing either.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let late = BrowserProxy::start(
            listener,
            "browser-proxy-late",
            route(site),
            granted(),
            Instant::now(),
        );
        assert_eq!(late.finish(), ProxyCount::default());
        let _ = Route::public();
    }

    /// A connected pair of loopback streams.
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let near = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (far, _) = listener.accept().unwrap();
        far.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        near.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        (near, far)
    }

    /// Everything `stream` sends until it ends.
    fn drained(mut stream: &TcpStream) -> Vec<u8> {
        let mut all = Vec::new();
        let _ = stream.read_to_end(&mut all);
        all
    }

    #[test]
    fn a_requests_head_is_read_a_line_at_a_time_and_bounded() {
        let (mut near, far) = pair();
        near.write_all(b"CONNECT site.test:443 HTTP/1.1\r\nHost: x\r\n\r\nrest")
            .unwrap();
        let mut reader = BufReader::new(&far);
        assert_eq!(read_connect(&mut reader), Ok(String::from("site.test:443")));
        assert_eq!(reader.buffer(), b"rest", "what follows the head is kept");
        let (mut near, far) = pair();
        near.write_all(b"GET / HTTP/1.1\n\n").unwrap();
        let mut reader = BufReader::new(&far);
        assert_eq!(
            read_connect(&mut reader),
            Err((
                refusal(405, "Method Not Allowed", EgressVerdict::NotAllowed),
                None
            ))
        );
        // A line: its ending dropped, `\r` or not; too long, unended, or
        // not UTF-8, none.
        let (mut near, far) = pair();
        let mut long = vec![b'a'; MAX_LINE_BYTES];
        long.push(b'\n');
        near.write_all(b"one\r\ntwo\n\xff\n").unwrap();
        near.write_all(&long).unwrap();
        near.write_all(b"cut").unwrap();
        drop(near);
        let mut reader = BufReader::new(&far);
        assert_eq!(read_line(&mut reader), Some(String::from("one")));
        assert_eq!(read_line(&mut reader), Some(String::from("two")));
        assert_eq!(read_line(&mut reader), None, "not UTF-8");
        assert_eq!(read_line(&mut reader), None, "one byte past the bound");
        let (mut near, far) = pair();
        let mut longest = vec![b'a'; MAX_LINE_BYTES - 1];
        longest.push(b'\n');
        near.write_all(&longest).unwrap();
        near.write_all(b"cut").unwrap();
        drop(near);
        let mut reader = BufReader::new(&far);
        assert_eq!(
            read_line(&mut reader).map(|line| line.len()),
            Some(MAX_LINE_BYTES - 1),
            "exactly the bound"
        );
        assert_eq!(read_line(&mut reader), None, "ended without a line end");
    }

    #[test]
    fn a_site_is_opened_at_the_addresses_checked_or_refused_by_name() {
        let (site, taken) = echo_site();
        let route = route(site);
        let sites = granted();
        let (destination, stream) = open("Site.Test:443", &route, &sites).unwrap();
        assert_eq!(destination, "site.test:443");
        assert_eq!(
            stream.peer_addr().unwrap(),
            site,
            "pinned to the address checked"
        );
        let refused = |authority: &str| open(authority, &route, &sites).err();
        assert_eq!(
            refused("intranet.test:443"),
            Some((
                refusal(403, "Forbidden", EgressVerdict::NotAllowed),
                Some(String::from("intranet.test:443"))
            ))
        );
        assert_eq!(
            refused("site.test:8443"),
            Some((
                refusal(403, "Forbidden", EgressVerdict::NotAllowed),
                Some(String::from("site.test:8443"))
            ))
        );
        assert_eq!(
            refused("nowhere.test:443"),
            Some((
                refusal(502, "Bad Gateway", EgressVerdict::Failed),
                Some(String::from("nowhere.test:443"))
            ))
        );
        assert_eq!(
            refused("10.0.0.9:443"),
            Some((refusal(400, "Bad Request", EgressVerdict::NotAllowed), None))
        );
        drop(stream);
        // The site accepts on its own thread: wait for the one it was
        // opened to, then make sure no other followed.
        let deadline = Instant::now() + Duration::from_secs(10);
        while taken.load(Ordering::SeqCst) < 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(taken.load(Ordering::SeqCst), 1, "only the one opened");
    }

    #[test]
    fn a_refusal_is_answered_and_recorded_with_the_site_it_names() {
        let _deciding = DECIDING.lock().unwrap_or_else(PoisonError::into_inner);
        let _flood = crate::machine::FLOOD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let cell = "browser-proxy-refuse";
        let context = Context {
            cell_id: String::from(cell),
            route: Route::public(),
            sites: Sites::default(),
            tunnelled: AtomicU32::new(0),
            refused: AtomicU32::new(0),
            live: AtomicU32::new(0),
        };
        let (near, far) = pair();
        let forbidden = refusal(403, "Forbidden", EgressVerdict::NoGrant);
        refuse(&far, &context, forbidden, Some("refused.test:443"));
        drop(far);
        assert_eq!(
            drained(&near),
            b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        // One that named no site is answered, and recorded with none.
        let (near, far) = pair();
        refuse(
            &far,
            &context,
            refusal(400, "Bad Request", EgressVerdict::NotAllowed),
            None,
        );
        drop(far);
        assert_eq!(
            drained(&near),
            b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        assert_eq!(
            decisions(cell),
            [(String::from("refused.test:443"), EgressVerdict::NoGrant)]
        );
    }

    #[test]
    fn a_busy_connection_is_answered_once_its_request_is_read() {
        let (mut near, far) = pair();
        near.write_all(b"CONNECT site.test:443 HTTP/1.1\r\n\r\n")
            .unwrap();
        refuse_busy(&far);
        let mut told = String::new();
        near.read_to_string(&mut told).unwrap();
        assert_eq!(
            told,
            "HTTP/1.1 503 Too Many Connections\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            "the whole answer, then the end"
        );
    }

    #[test]
    fn an_answer_is_a_status_and_nothing_more() {
        let (near, far) = pair();
        answer(&near, 418, "Teapot").unwrap();
        drop(near);
        assert_eq!(
            drained(&far),
            b"HTTP/1.1 418 Teapot\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
    }

    #[test]
    fn a_tunnel_passes_bytes_both_ways_and_ends_at_its_bound() {
        // The splice: the 200, the early bytes first, then both ways.
        let (browser, client) = pair();
        let (proxy_side, site) = pair();
        let splicing = std::thread::spawn(move || splice(&client, &proxy_side, b"hello"));
        let mut browser_reads = browser.try_clone().unwrap();
        let mut head = [0u8; 39];
        browser_reads.read_exact(&mut head).unwrap();
        assert_eq!(&head, b"HTTP/1.1 200 Connection Established\r\n\r\n");
        let mut site_rw = site.try_clone().unwrap();
        let mut early = [0u8; 5];
        site_rw.read_exact(&mut early).unwrap();
        assert_eq!(&early, b"hello");
        site_rw.write_all(b"back").unwrap();
        let mut back = [0u8; 4];
        browser_reads.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"back");
        (&browser).write_all(b"up").unwrap();
        let mut up = [0u8; 2];
        site_rw.read_exact(&mut up).unwrap();
        drop((browser, browser_reads));
        assert_eq!(
            splicing.join().unwrap(),
            (7, 4),
            "the early bytes count as up"
        );
        // The bound: a copy stops once it has passed `max`.
        let (mut writer, from) = pair();
        let (to, reader) = pair();
        let copying = std::thread::spawn(move || copy(&from, &to, 10));
        writer.write_all(b"0123456789").unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let _ = writer.write_all(b"more!");
        assert_eq!(copying.join().unwrap(), 10);
        assert_eq!(drained(&reader), b"0123456789");
    }

    #[test]
    fn the_accept_loop_hands_each_connection_to_a_tunnel_and_stops() {
        let (site, _) = echo_site();
        let context = Arc::new(Context {
            cell_id: String::from("browser-proxy-loop"),
            route: route(site),
            sites: granted(),
            tunnelled: AtomicU32::new(0),
            refused: AtomicU32::new(0),
            live: AtomicU32::new(0),
        });
        // A tunnel, alone: a plain request is refused.
        let (mut near, far) = pair();
        near.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        assert!(!tunnel(&far, &context));
        drop(far);
        assert!(drained(&near).starts_with(b"HTTP/1.1 405"));
        // The loop, on a thread of its own so that one that never returns
        // fails here rather than hanging: stopped, it returns at once,
        // whatever the deadline; past its deadline, likewise.
        let returns = |stopped: bool, deadline: Instant| {
            let (done, finished) = std::sync::mpsc::channel();
            let context = Arc::clone(&context);
            std::thread::spawn(move || {
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
                serve(&listener, &context, deadline, &AtomicBool::new(stopped));
                let _ = done.send(());
            });
            finished.recv_timeout(Duration::from_secs(5)).is_ok()
        };
        assert!(
            returns(true, Instant::now() + Duration::from_secs(60)),
            "stopped"
        );
        assert!(returns(false, Instant::now()), "past its deadline");
        assert_eq!(context.tunnelled.load(Ordering::SeqCst), 0);
        assert_eq!(
            refusal(400, "Bad Request", EgressVerdict::NotAllowed),
            Refusal {
                status: 400,
                reason: "Bad Request",
                verdict: EgressVerdict::NotAllowed
            }
        );
    }
}
