//! Just enough HTTP for the host: a request's head, read strictly; the
//! built UI's files; and the WebSocket upgrade on [`UI_PATH`].
//!
//! Every request comes from a page that can reach the loopback port, so:
//!
//! - **A head is bounded** ([`MAX_HEAD_BYTES`]) and must arrive within
//!   the connection's read timeout.
//! - **Only `GET`.**
//! - **A file is named only by plain segments** (`[A-Za-z0-9._-]`, none
//!   starting with a dot), so no `..`, no hidden file and no encoded
//!   escape can name anything outside the UI's directory; and only a
//!   regular file is served, never one through a symlink.
//! - **Nothing the page sent is echoed back.**
//! - **Only requests addressed to this host:** its `Host` must be
//!   `127.0.0.1:<port>` or `localhost:<port>`, so a page that rebinds its
//!   own name to `127.0.0.1` (DNS rebinding) is refused.
//! - **Only upgrades from its own pages:** a WebSocket upgrade's `Origin`
//!   must be this host too. A browser sends it on every WebSocket, and a
//!   page can't forge it, so another site in the user's browser can't
//!   open a channel to the broker, reach its decoder before the token, or
//!   take up its connections.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::Error;

/// Where the browser opens its channel to the broker.
pub const UI_PATH: &str = "/ui";

/// Where the Machine view's channel is opened.
pub const OPERATOR_PATH: &str = "/operator";

/// Which of the broker's sockets a WebSocket is carried to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// The UI socket, on [`UI_PATH`].
    Ui,
    /// The operator socket, on [`OPERATOR_PATH`].
    Operator,
}

/// The most a request's head may be.
pub const MAX_HEAD_BYTES: usize = 8 * 1024;

/// A request's head: its method, target and headers (names in lower case).
#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    /// The method, as sent.
    pub method: String,
    /// The target, as sent.
    pub target: String,
    /// The headers, names in lower case, values trimmed.
    pub headers: Vec<(String, String)>,
}

impl Head {
    /// The value of the header `name` (lower case), if it was sent once.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let mut found = self.headers.iter().filter(|(key, _)| key == name);
        match (found.next(), found.next()) {
            (Some((_, value)), None) => Some(value),
            _ => None,
        }
    }

    /// The channel and the client's WebSocket key, when this is an
    /// upgrade to one on [`UI_PATH`] or [`OPERATOR_PATH`]: `Upgrade:
    /// websocket`, `Connection: upgrade`, version 13, and a key.
    #[must_use]
    pub fn websocket(&self) -> Option<(Channel, &str)> {
        let channel = match self.target.as_str() {
            UI_PATH => Channel::Ui,
            OPERATOR_PATH => Channel::Operator,
            _ => return None,
        };
        self.websocket_key().map(|key| (channel, key))
    }

    /// Whether this request was addressed to the host on `port`: its
    /// `Host` is `127.0.0.1:<port>` or `localhost:<port>`.
    #[must_use]
    pub fn addressed_to(&self, port: u16) -> bool {
        self.header("host").is_some_and(|host| this_host(host, port))
    }

    /// Whether this request came from a page the host on `port` served:
    /// its `Origin` is `http://127.0.0.1:<port>` or
    /// `http://localhost:<port>`. A browser sends `Origin` on every
    /// WebSocket upgrade; a request without one isn't from a page here.
    #[must_use]
    pub fn from_page_on(&self, port: u16) -> bool {
        self.header("origin").is_some_and(|origin| {
            origin
                .strip_prefix("http://")
                .is_some_and(|authority| this_host(authority, port))
        })
    }

    /// The client's WebSocket key, when this is an upgrade to one:
    /// `Upgrade: websocket`, `Connection: upgrade`, version 13, and a key.
    #[must_use]
    fn websocket_key(&self) -> Option<&str> {
        let has = |name: &str, token: &str| {
            self.header(name).is_some_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case(token))
            })
        };
        (has("upgrade", "websocket")
            && has("connection", "upgrade")
            && self.header("sec-websocket-version") == Some("13"))
        .then(|| self.header("sec-websocket-key"))
        .flatten()
        .filter(|key| !key.is_empty())
    }
}

/// Whether `authority` names the host on `port`: `127.0.0.1:<port>` or
/// `localhost:<port>`, the name in any case.
fn this_host(authority: &str, port: u16) -> bool {
    authority.rsplit_once(':').is_some_and(|(name, given)| {
        given == port.to_string() && (name == "127.0.0.1" || name.eq_ignore_ascii_case("localhost"))
    })
}

/// Reads one request's head, byte by byte up to its blank line, so
/// nothing after it is taken from the stream.
///
/// # Errors
///
/// [`Error::Io`] when the stream fails or ends first, and
/// [`Error::Request`] for a head too long or not HTTP/1.1 text.
pub fn read_head(reader: &mut impl Read) -> Result<Head, Error> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD_BYTES {
            return Err(Error::Request("the request's head is too long"));
        }
        reader.read_exact(&mut byte)?;
        head.extend_from_slice(&byte);
    }
    let text = std::str::from_utf8(&head).map_err(|_| Error::Request("the head is not text"))?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some("HTTP/1.1"), None) = (
        request.next(),
        request.next(),
        request.next(),
        request.next(),
    ) else {
        return Err(Error::Request("the request line is not HTTP/1.1"));
    };
    let mut headers = Vec::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or(Error::Request("a header has no name"))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(Head {
        method: method.to_owned(),
        target: target.to_owned(),
        headers,
    })
}

/// The file in `dir` that `target` names, when it names one by plain
/// segments; `/` names `index.html`. A query is ignored.
#[must_use]
pub fn file_for(dir: &Path, target: &str) -> Option<PathBuf> {
    let path = target.split('?').next()?.strip_prefix('/')?;
    let path = if path.is_empty() { "index.html" } else { path };
    let mut file = dir.to_path_buf();
    for segment in path.split('/') {
        let plain = !segment.is_empty()
            && !segment.starts_with('.')
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !plain {
            return None;
        }
        file.push(segment);
    }
    Some(file)
}

/// The content type for `file`, by its extension.
#[must_use]
pub fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

/// Answers `head`, a request that isn't an upgrade, from `dir`: the file
/// it names, or a bare status.
///
/// # Errors
///
/// [`Error::Io`] when the answer can't be written.
pub fn answer(writer: &mut impl Write, dir: &Path, head: &Head) -> Result<(), Error> {
    if head.method != "GET" {
        return respond(writer, 405, "Method Not Allowed", "text/plain", b"");
    }
    let found = file_for(dir, &head.target).and_then(|file| {
        let regular = std::fs::symlink_metadata(&file).is_ok_and(|meta| meta.is_file());
        regular
            .then(|| std::fs::read(&file).ok())
            .flatten()
            .map(|bytes| (content_type(&file), bytes))
    });
    match found {
        Some((kind, bytes)) => respond(writer, 200, "OK", kind, &bytes),
        None => respond(writer, 404, "Not Found", "text/plain", b""),
    }
}

/// Refuses a request not addressed to this host, or an upgrade not from
/// its own pages, with a bare `403`.
///
/// # Errors
///
/// [`Error::Io`] when the answer can't be written.
pub fn forbid(writer: &mut impl Write) -> Result<(), Error> {
    respond(writer, 403, "Forbidden", "text/plain", b"")
}

/// Accepts the WebSocket upgrade for the client's `key`.
///
/// # Errors
///
/// [`Error::Io`] when the answer can't be written.
pub fn accept_upgrade(writer: &mut impl Write, key: &str) -> Result<(), Error> {
    let accept = crate::websocket::accept_key(key);
    writer.write_all(
        format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        )
        .as_bytes(),
    )?;
    writer.flush()?;
    Ok(())
}

/// Writes a whole response and asks the browser to close.
fn respond(
    writer: &mut impl Write,
    status: u16,
    phrase: &str,
    kind: &str,
    body: &[u8],
) -> Result<(), Error> {
    writer.write_all(
        format!(
            "HTTP/1.1 {status} {phrase}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    )?;
    writer.write_all(body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(text: &str) -> Result<Head, Error> {
        read_head(&mut text.as_bytes())
    }

    const UPGRADE: &str = "GET /ui HTTP/1.1\r\nHost: 127.0.0.1:7340\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";

    #[test]
    fn a_head_is_read_to_its_blank_line_and_no_further() {
        let mut stream: &[u8] = b"GET / HTTP/1.1\r\nHost: x\r\n\r\nafter";
        let read = read_head(&mut stream).unwrap();
        assert_eq!(read.method, "GET");
        assert_eq!(read.target, "/");
        assert_eq!(read.header("host"), Some("x"));
        assert_eq!(stream, b"after");
    }

    #[test]
    fn a_head_that_is_too_long_odd_or_cut_short_is_refused() {
        let long = format!(
            "GET / HTTP/1.1\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_HEAD_BYTES)
        );
        assert!(matches!(
            head(&long),
            Err(Error::Request("the request's head is too long"))
        ));
        for odd in [
            "GET / HTTP/1.0\r\n\r\n",
            "GET /\r\n\r\n",
            "GET / HTTP/1.1 extra\r\n\r\n",
        ] {
            assert!(
                matches!(
                    head(odd),
                    Err(Error::Request("the request line is not HTTP/1.1"))
                ),
                "{odd:?}"
            );
        }
        assert!(matches!(
            head("GET / HTTP/1.1\r\nno colon\r\n\r\n"),
            Err(Error::Request("a header has no name"))
        ));
        assert!(matches!(
            read_head(&mut &b"GET / HTTP/1.1\r\n\xff\r\n\r\n"[..]),
            Err(Error::Request("the head is not text"))
        ));
        assert!(matches!(head("GET / HTTP/1.1\r\n"), Err(Error::Io(_))));
    }

    #[test]
    fn each_channel_has_its_path_and_no_other_path_is_one() {
        assert_eq!(
            head(UPGRADE).unwrap().websocket(),
            Some((Channel::Ui, "dGhlIHNhbXBsZSBub25jZQ=="))
        );
        let operator = UPGRADE.replace("GET /ui ", "GET /operator ");
        assert_eq!(
            head(&operator).unwrap().websocket(),
            Some((Channel::Operator, "dGhlIHNhbXBsZSBub25jZQ=="))
        );
        for other in ["GET /other ", "GET /ui/ ", "GET /operator/x ", "GET / "] {
            let changed = UPGRADE.replace("GET /ui ", other);
            assert_eq!(head(&changed).unwrap().websocket(), None, "{other}");
        }
        // A path but no upgrade: no channel.
        let plain = operator.replace("Upgrade: websocket", "Upgrade: h2c");
        assert_eq!(head(&plain).unwrap().websocket(), None);
    }

    #[test]
    fn only_a_full_websocket_upgrade_has_a_key() {
        assert_eq!(
            head(UPGRADE).unwrap().websocket_key(),
            Some("dGhlIHNhbXBsZSBub25jZQ==")
        );
        for (from, to) in [
            ("Upgrade: websocket", "Upgrade: h2c"),
            ("Connection: keep-alive, Upgrade", "Connection: keep-alive"),
            ("Version: 13", "Version: 8"),
            (
                "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
                "Sec-WebSocket-Key: ",
            ),
        ] {
            let changed = UPGRADE.replace(from, to);
            assert_eq!(head(&changed).unwrap().websocket_key(), None, "{to}");
        }
        // A header sent twice is no header at all.
        let twice = UPGRADE.replace("\r\n\r\n", "\r\nSec-WebSocket-Key: b3RoZXI=\r\n\r\n");
        assert_eq!(head(&twice).unwrap().websocket_key(), None);
        let mut written = Vec::new();
        accept_upgrade(&mut written, "dGhlIHNhbXBsZSBub25jZQ==").unwrap();
        let written = String::from_utf8(written).unwrap();
        assert!(written.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));
        assert!(written.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"));
        assert!(written.ends_with("\r\n\r\n"));
    }

    #[test]
    fn only_plain_segments_name_a_file_in_the_ui() {
        let dir = Path::new("/srv/ui");
        assert_eq!(file_for(dir, "/"), Some(dir.join("index.html")));
        assert_eq!(file_for(dir, "/?x=1"), Some(dir.join("index.html")));
        assert_eq!(
            file_for(dir, "/assets/app-1.js"),
            Some(dir.join("assets").join("app-1.js"))
        );
        for refused in [
            "",
            "ui",
            "/..",
            "/../etc/passwd",
            "/assets/../index.html",
            "/.env",
            "/assets/.hidden",
            "/a//b",
            "/a/",
            "/%2e%2e/x",
            "/a\\b",
            "/a b",
        ] {
            assert_eq!(file_for(dir, refused), None, "{refused:?}");
        }
    }

    #[test]
    fn files_are_served_with_their_type_and_nothing_else_is() {
        let dir = std::env::temp_dir().join(format!("hatchling-host-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), b"<p>hi</p>").unwrap();
        std::fs::write(dir.join("assets").join("app.js"), b"go()").unwrap();
        std::os::unix::fs::symlink("/etc/hostname", dir.join("linked.txt")).unwrap();
        let served = |text: &str| {
            let mut out = Vec::new();
            answer(&mut out, &dir, &head(text).unwrap()).unwrap();
            String::from_utf8(out).unwrap()
        };
        let index = served("GET / HTTP/1.1\r\n\r\n");
        assert!(index.starts_with("HTTP/1.1 200 OK\r\n"), "{index}");
        assert!(index.contains("Content-Type: text/html; charset=utf-8\r\n"));
        assert!(index.contains("Content-Length: 9\r\n"));
        assert!(index.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(index.ends_with("\r\n\r\n<p>hi</p>"));
        assert!(served("GET /assets/app.js HTTP/1.1\r\n\r\n").contains("text/javascript"));
        for missing in ["/linked.txt", "/assets", "/nope.html", "/../x"] {
            let answer = served(&format!("GET {missing} HTTP/1.1\r\n\r\n"));
            assert!(
                answer.starts_with("HTTP/1.1 404 Not Found\r\n"),
                "{missing}: {answer}"
            );
            assert!(answer.ends_with("\r\n\r\n"), "nothing echoed: {answer}");
        }
        let post = served("POST / HTTP/1.1\r\n\r\n");
        assert!(post.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_response_is_whole_and_says_to_close() {
        let mut out = Vec::new();
        respond(&mut out, 418, "I'm a teapot", "text/plain", b"tea").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "HTTP/1.1 418 I'm a teapot\r\nContent-Type: text/plain\r\nContent-Length: 3\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\ntea"
        );
    }

    #[test]
    fn each_kind_of_file_has_its_type() {
        for (name, kind) in [
            ("a.html", "text/html; charset=utf-8"),
            ("a.mjs", "text/javascript; charset=utf-8"),
            ("a.css", "text/css; charset=utf-8"),
            ("a.json", "application/json"),
            ("a.svg", "image/svg+xml"),
            ("a.png", "image/png"),
            ("a.jpeg", "image/jpeg"),
            ("a.webp", "image/webp"),
            ("a.woff2", "font/woff2"),
            ("a.ico", "image/x-icon"),
            ("a.bin", "application/octet-stream"),
            ("noext", "application/octet-stream"),
        ] {
            assert_eq!(content_type(Path::new(name)), kind, "{name}");
        }
    }
    #[test]
    fn only_this_hosts_own_name_and_port_are_its_host_and_origin() {
        let with = |host: &str, origin: Option<&str>| {
            let origin = origin.map_or(String::new(), |origin| format!("Origin: {origin}\r\n"));
            head(&format!("GET /ui HTTP/1.1\r\nHost: {host}\r\n{origin}\r\n")).unwrap()
        };
        for host in ["127.0.0.1:7340", "localhost:7340", "LocalHost:7340"] {
            assert!(with(host, None).addressed_to(7340), "{host}");
        }
        for host in [
            "127.0.0.1",
            "127.0.0.1:7341",
            "127.0.0.1:+7340",
            "127.0.0.1:07340",
            "evil.example:7340",
            "127.0.0.1.evil.example:7340",
            "localhost.:7340",
            "[::1]:7340",
            "",
        ] {
            assert!(!with(host, None).addressed_to(7340), "{host}");
        }
        let host = "127.0.0.1:7340";
        assert!(with(host, Some("http://127.0.0.1:7340")).from_page_on(7340));
        assert!(with(host, Some("http://localhost:7340")).from_page_on(7340));
        for origin in [
            "http://evil.example",
            "http://evil.example:7340",
            "https://127.0.0.1:7340",
            "http://127.0.0.1:7341",
            "http://127.0.0.1",
            "null",
            "",
        ] {
            assert!(!with(host, Some(origin)).from_page_on(7340), "{origin}");
        }
        assert!(!with(host, None).from_page_on(7340), "no Origin");
        // Sent twice, a header is no header.
        let twice =
            head("GET /ui HTTP/1.1\r\nHost: 127.0.0.1:7340\r\nHost: evil.example:7340\r\n\r\n")
                .unwrap();
        assert!(!twice.addressed_to(7340));
    }
}
