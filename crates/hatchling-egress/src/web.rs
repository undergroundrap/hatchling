//! Reading the public web for the agent (WO-028): a page fetched by the
//! broker, from a site it names exactly, as text.
//!
//! - **A site is an origin:** `https`, a host and a port ([`Origin`]). It
//!   is the unit the owner grants, so a grant for one never reaches a
//!   subdomain, another port, or another scheme.
//! - **URLs are parsed strictly** ([`WebUrl::parse`]): an absolute `https`
//!   URL, no user name or password, a host written as ASCII labels (an
//!   international name in its `xn--` form, so a look-alike shows as what
//!   it is), no IP address, the default port unless one is written. The
//!   fragment is dropped: it never leaves the client.
//! - **Resolved and checked per fetch, then pinned** ([`fetch`]). The host
//!   is resolved when it's fetched, and refused if any address it resolves
//!   to is internal ([`is_internal`]), before a connection is made; the
//!   connection goes only to those addresses, and its peer is checked
//!   against them, so DNS can't be rebound between the check and the
//!   connection.
//! - **One request, bounded.** A `GET` with no credential, read under the
//!   proxy's bounds (1 MiB, no compression). A redirect is returned, never
//!   followed: where it leads is another fetch, of a site that needs its
//!   own grant.
//! - **Text only.** HTML is reduced to its visible text ([`html_text`]);
//!   plain text, JSON and XML pass as they are; anything else is refused.
//!   What comes back is the site's word: whoever shows it to a model wraps
//!   it as untrusted data.

use std::fmt;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};

use crate::https::TlsClient;
use crate::{EgressRequest, is_internal};

/// The longest URL read, in bytes.
pub const MAX_URL_BYTES: usize = 2048;

/// The most text a page gives back, in bytes; the rest is cut, and the
/// page says so ([`Page::truncated`]).
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The default `https` port, left out of an origin's name.
const HTTPS_PORT: u16 = 443;

/// The longest host name, in bytes.
const MAX_HOST_BYTES: usize = 253;

/// The longest label of a host name, in bytes.
const MAX_LABEL_BYTES: usize = 63;

/// A site: `https`, a host and a port. The unit a grant names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Origin {
    host: String,
    port: u16,
}

impl Origin {
    /// The host, lowercase ASCII labels.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    /// `host:port`, as the egress proxy names a destination.
    #[must_use]
    pub fn destination(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.port == HTTPS_PORT {
            write!(f, "https://{}", self.host)
        } else {
            write!(f, "https://{}:{}", self.host, self.port)
        }
    }
}

/// An `https` URL, parsed strictly: its origin, and the path and query it
/// asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebUrl {
    origin: Origin,
    target: String,
}

/// Why a URL is refused, before any request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UrlRefusal {
    /// Longer than [`MAX_URL_BYTES`].
    #[error("the URL is longer than {MAX_URL_BYTES} bytes")]
    TooLong,
    /// Not an absolute `https` URL.
    #[error("only absolute https URLs are read")]
    NotHttps,
    /// It names a user or a password.
    #[error("a URL with a user name or password is refused")]
    Userinfo,
    /// Its host is an IP address.
    #[error("an IP address is refused: name the site")]
    IpLiteral,
    /// Its host isn't a name of ASCII labels.
    #[error("the host isn't a valid name (write an international name in its xn-- form)")]
    BadHost,
    /// Its port isn't a number from 1 to 65535.
    #[error("the port isn't a number from 1 to 65535")]
    BadPort,
    /// It holds a space, a control character, a backslash, or a
    /// character outside ASCII.
    #[error(
        "the URL holds a space, a control character, a backslash, or a character outside ASCII"
    )]
    BadCharacter,
    /// A redirect's location that is neither an absolute URL nor an
    /// absolute path.
    #[error("a redirect to a relative location other than an absolute path is refused")]
    Relative,
}

impl WebUrl {
    /// Parses `raw` as an absolute `https` URL.
    ///
    /// # Errors
    ///
    /// The [`UrlRefusal`] that says why it isn't one this module reads.
    pub fn parse(raw: &str) -> Result<Self, UrlRefusal> {
        if raw.len() > MAX_URL_BYTES {
            return Err(UrlRefusal::TooLong);
        }
        let rest = strip_prefix_ignore_case(raw, "https://").ok_or(UrlRefusal::NotHttps)?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, target) = rest.split_at_checked(end).ok_or(UrlRefusal::BadHost)?;
        if authority.contains('@') {
            return Err(UrlRefusal::Userinfo);
        }
        let origin = parse_authority(authority)?;
        let target = target.split('#').next().unwrap_or_default();
        if target.chars().any(|c| !c.is_ascii_graphic() || c == '\\') {
            return Err(UrlRefusal::BadCharacter);
        }
        let target = if target.starts_with('/') {
            target.to_owned()
        } else {
            format!("/{target}")
        };
        Ok(Self { origin, target })
    }

    /// Its origin: the site.
    #[must_use]
    pub const fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The path and query it asks for, from `/`.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Where a redirect from this URL to `location` leads: an absolute
    /// `https` URL, one without its scheme (`//host/..`), or an absolute
    /// path on this URL's origin.
    ///
    /// # Errors
    ///
    /// [`UrlRefusal::Relative`] for any other relative location, or why
    /// the URL it names is refused.
    pub fn join(&self, location: &str) -> Result<Self, UrlRefusal> {
        if location.starts_with("//") {
            Self::parse(&format!("https:{location}"))
        } else if location.starts_with('/') {
            Self::parse(&format!("{}{location}", self.origin))
        } else if location.contains("://") {
            Self::parse(location)
        } else {
            Err(UrlRefusal::Relative)
        }
    }
}

impl fmt::Display for WebUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", self.origin, self.target)
    }
}

/// `raw` without `prefix`, matched ignoring ASCII case.
fn strip_prefix_ignore_case<'a>(raw: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, rest) = raw.split_at_checked(prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then_some(rest)
}

/// The origin an authority (`host` or `host:port`) names.
fn parse_authority(authority: &str) -> Result<Origin, UrlRefusal> {
    if authority.starts_with('[') {
        return Err(UrlRefusal::IpLiteral);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, parse_port(port)?),
        None => (authority, HTTPS_PORT),
    };
    if !host.is_ascii() {
        return Err(UrlRefusal::BadHost);
    }
    let host = host.to_ascii_lowercase();
    if host.is_empty() || host.len() > MAX_HOST_BYTES {
        return Err(UrlRefusal::BadHost);
    }
    let labels: Vec<&str> = host.split('.').collect();
    if !labels.iter().all(|label| valid_label(label)) {
        return Err(UrlRefusal::BadHost);
    }
    if labels.last().is_some_and(|last| numeric_label(last)) {
        return Err(UrlRefusal::IpLiteral);
    }
    Ok(Origin { host, port })
}

/// A port: digits only, from 1 to 65535.
fn parse_port(port: &str) -> Result<u16, UrlRefusal> {
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(UrlRefusal::BadPort);
    }
    match port.parse::<u16>() {
        Ok(0) | Err(_) => Err(UrlRefusal::BadPort),
        Ok(port) => Ok(port),
    }
}

/// A host label: 1 to 63 of `a-z`, `0-9` and `-`, not starting or ending
/// with `-`.
fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_LABEL_BYTES
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

/// Whether a host's last label makes it an IPv4 address to a resolver:
/// all digits, or `0x` and hex digits.
fn numeric_label(label: &str) -> bool {
    let hex = label
        .strip_prefix("0x")
        .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()));
    hex || label.bytes().all(|byte| byte.is_ascii_digit())
}

/// What a fetch got back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// A page, as text.
    Page(Page),
    /// A redirect, not followed: the site it leads to needs its own grant.
    Redirect {
        /// The status, 3xx.
        status: u16,
        /// Where it leads.
        to: WebUrl,
    },
}

/// A page, as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The URL it was fetched from.
    pub url: WebUrl,
    /// The HTTP status.
    pub status: u16,
    /// Whether it was HTML, reduced to its visible text.
    pub html: bool,
    /// Its text, at most [`MAX_TEXT_BYTES`].
    pub text: String,
    /// Whether the text was cut at [`MAX_TEXT_BYTES`].
    pub truncated: bool,
}

/// Why a fetch got no page.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The host doesn't resolve.
    #[error("the site's name doesn't resolve")]
    Unresolved,
    /// The host resolves to an internal address: refused before any
    /// connection.
    #[error("the site's name resolves to an internal address ({addr})")]
    Internal {
        /// The first internal address it resolved to.
        addr: IpAddr,
    },
    /// No connection, a failed handshake or certificate check, or a
    /// malformed, compressed or truncated answer.
    #[error("the site couldn't be reached, or its answer couldn't be read")]
    Failed,
    /// The answer is past the proxy's 1 MiB bound.
    #[error("the page is larger than 1 MiB")]
    TooLarge,
    /// The answer isn't text.
    #[error("a {content_type} answer isn't readable text")]
    NotText {
        /// Its content type, as the site gave it, or `none`.
        content_type: String,
    },
    /// A redirect with no location.
    #[error("a redirect with no location")]
    NoLocation,
    /// A redirect to a location that is refused.
    #[error("a redirect to a location that is refused: {0}")]
    BadRedirect(UrlRefusal),
}

/// Fetches `url` with the system's resolver, refusing a host that resolves
/// to any internal address.
///
/// # Errors
///
/// The [`FetchError`] that says why there is no page.
pub fn fetch(tls: &TlsClient, url: &WebUrl) -> Result<Fetched, FetchError> {
    fetch_with(
        tls,
        url,
        |destination| destination.to_socket_addrs().map(Iterator::collect),
        is_internal,
    )
}

/// [`fetch`], resolving with `resolve` and refusing what `internal` says
/// is internal.
pub(crate) fn fetch_with(
    tls: &TlsClient,
    url: &WebUrl,
    resolve: impl Fn(&str) -> std::io::Result<Vec<SocketAddr>>,
    internal: impl Fn(IpAddr) -> bool,
) -> Result<Fetched, FetchError> {
    let destination = url.origin.destination();
    let addrs = resolved(&destination, resolve, internal)?;
    let request = EgressRequest {
        method: String::from("GET"),
        destination,
        path: url.target.clone(),
        headers: vec![
            (
                String::from("Accept"),
                String::from("text/html, text/plain;q=0.9, application/json;q=0.8"),
            ),
            (String::from("User-Agent"), String::from("Hatchling")),
        ],
        body: Vec::new(),
        surrogate_b64: String::new(),
    };
    let response = crate::https::forward(tls, &addrs, &request, None).map_err(|error| {
        if matches!(error, crate::Error::ResponseTooLarge) {
            FetchError::TooLarge
        } else {
            FetchError::Failed
        }
    })?;
    let header = |name: &str| {
        response
            .headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    };
    if (300..400).contains(&response.status) && response.status != 304 {
        let location = header("location").ok_or(FetchError::NoLocation)?;
        let to = url.join(location).map_err(FetchError::BadRedirect)?;
        return Ok(Fetched::Redirect {
            status: response.status,
            to,
        });
    }
    let content_type = header("content-type").unwrap_or("none");
    let Some(html) = readable(content_type) else {
        return Err(FetchError::NotText {
            content_type: content_type.chars().take(80).collect(),
        });
    };
    let body = String::from_utf8_lossy(&response.body);
    let text = if html {
        html_text(&body)
    } else {
        body.into_owned()
    };
    let (text, truncated) = bounded(text, MAX_TEXT_BYTES);
    Ok(Fetched::Page(Page {
        url: url.clone(),
        status: response.status,
        html,
        text,
        truncated,
    }))
}

/// The addresses `destination` resolves to with `resolve`, refused when
/// it resolves to none, or to any that `internal` says is internal.
pub(crate) fn resolved(
    destination: &str,
    resolve: impl Fn(&str) -> std::io::Result<Vec<SocketAddr>>,
    internal: impl Fn(IpAddr) -> bool,
) -> Result<Vec<SocketAddr>, FetchError> {
    let addrs = resolve(destination).map_err(|_| FetchError::Unresolved)?;
    if addrs.is_empty() {
        return Err(FetchError::Unresolved);
    }
    if let Some(addr) = addrs.iter().map(SocketAddr::ip).find(|ip| internal(*ip)) {
        return Err(FetchError::Internal { addr });
    }
    Ok(addrs)
}

/// Whether a content type is readable: `Some(true)` for HTML, `Some(false)`
/// for text that passes as it is, `None` for anything else.
fn readable(content_type: &str) -> Option<bool> {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
        "text/html" | "application/xhtml+xml" => Some(true),
        "text/plain" | "text/markdown" | "text/csv" | "text/xml" | "application/json"
        | "application/xml" => Some(false),
        _ => None,
    }
}

/// `text` cut to at most `max` bytes at a character boundary, and whether
/// it was cut.
fn bounded(mut text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

/// Elements whose content a page never shows as text.
const HIDDEN: [&str; 7] = [
    "script", "style", "noscript", "template", "svg", "iframe", "object",
];

/// Elements that start a line of their own.
const BLOCKS: [&str; 27] = [
    "p",
    "div",
    "br",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "tr",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "ul",
    "ol",
    "table",
    "blockquote",
    "pre",
    "hr",
    "title",
    "dt",
    "dd",
];

/// The visible text of an HTML document.
///
/// Tags, comments and the content of scripts, styles and the like are
/// dropped; a block's tags start a new line, a list item a `- `;
/// character references are decoded; runs of space collapse. Not a
/// browser's rendering: enough to read.
#[must_use]
pub fn html_text(html: &str) -> String {
    // ASCII lowercasing keeps every byte offset.
    let lower = html.to_ascii_lowercase();
    let mut text = String::new();
    let mut at = 0;
    while let Some(open) = lower
        .get(at..)
        .and_then(|rest| rest.find('<'))
        .map(|found| at + found)
    {
        push_text(&mut text, html.get(at..open).unwrap_or_default());
        at = skip_markup(&lower, open, &mut text);
    }
    push_text(&mut text, html.get(at..).unwrap_or_default());
    tidy(&text)
}

/// Skips the markup at `open` (a `<`), starting a line in `text` for a
/// block; returns where the text resumes. A `<` that starts no tag is
/// text.
fn skip_markup(lower: &str, open: usize, text: &mut String) -> usize {
    let rest = lower.get(open..).unwrap_or_default();
    if rest.starts_with("<!--") {
        return rest.find("-->").map_or(lower.len(), |end| open + end + 3);
    }
    let starts_tag = rest
        .chars()
        .nth(1)
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?');
    if !starts_tag {
        text.push('<');
        return open + 1;
    }
    let Some(close) = rest.find('>') else {
        return lower.len();
    };
    let after = open + close + 1;
    let tag = rest.get(1..close).unwrap_or_default();
    let closing = tag.starts_with('/');
    let name: String = tag
        .trim_start_matches('/')
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    if !closing && !tag.ends_with('/') && HIDDEN.contains(&name.as_str()) {
        let end = format!("</{name}");
        return lower
            .get(after..)
            .and_then(|tail| tail.find(&end))
            .map(|found| after + found)
            .and_then(|from| {
                lower
                    .get(from..)
                    .and_then(|tail| tail.find('>'))
                    .map(|gt| from + gt + 1)
            })
            .unwrap_or(lower.len());
    }
    if BLOCKS.contains(&name.as_str()) {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        if name == "li" && !closing {
            text.push_str("- ");
        }
    }
    after
}

/// Appends `raw`, text between tags, with its character references
/// decoded.
fn push_text(text: &mut String, raw: &str) {
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        let (before, from) = rest.split_at_checked(amp).unwrap_or((rest, ""));
        push_plain(text, before);
        if let Some((decoded, used)) = entity(from) {
            let mut buf = [0_u8; 4];
            push_plain(text, decoded.encode_utf8(&mut buf));
            rest = from.get(used..).unwrap_or_default();
        } else {
            text.push('&');
            rest = from.get(1..).unwrap_or_default();
        }
    }
    push_plain(text, rest);
}

/// Appends `plain`, each run of whitespace one space, control characters
/// dropped.
fn push_plain(text: &mut String, plain: &str) {
    for c in plain.chars() {
        if c.is_whitespace() {
            if !text.ends_with([' ', '\n']) && !text.is_empty() {
                text.push(' ');
            }
        } else if !c.is_control() {
            text.push(c);
        }
    }
}

/// The character a reference at the start of `from` (`&name;`, `&#n;` or
/// `&#xh;`) stands for, and its length; `None` for one it doesn't know.
fn entity(from: &str) -> Option<(char, usize)> {
    let semi = from.get(..12).unwrap_or(from).find(';')?;
    let name = from.get(1..semi)?;
    let decoded = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse::<u32>().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((decoded, semi + 1))
}

/// Each line trimmed, and at most one blank line in a row.
fn tidy(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.split('\n').map(str::trim) {
        if line.is_empty() && lines.last().is_none_or(|last| last.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};
    use std::time::Duration;

    use super::*;
    use crate::https::tests::{Ending, pki, serve};

    fn url(raw: &str) -> WebUrl {
        WebUrl::parse(raw).unwrap()
    }

    /// A URL is read strictly: its origin lowercased with the default
    /// port left out of its name, its fragment dropped.
    #[test]
    fn a_url_names_its_origin_and_target() {
        let page = url("HTTPS://Docs.Example.COM/a/b?q=1#part");
        assert_eq!(page.origin().host(), "docs.example.com");
        assert_eq!(page.origin().port(), 443);
        assert_eq!(page.origin().destination(), "docs.example.com:443");
        assert_eq!(page.target(), "/a/b?q=1");
        assert_eq!(page.to_string(), "https://docs.example.com/a/b?q=1");
        let ported = url("https://example.com:8443");
        assert_eq!(ported.target(), "/");
        assert_eq!(ported.to_string(), "https://example.com:8443/");
        assert_eq!(url("https://example.com?q").target(), "/?q");
        assert_eq!(
            url("https://xn--bcher-kva.example/").origin().host(),
            "xn--bcher-kva.example"
        );
    }

    /// Origins differ by host, subdomain and port: one never stands for
    /// another.
    #[test]
    fn an_origin_is_its_host_and_port_exactly() {
        let site = url("https://example.com/a").origin().clone();
        assert_eq!(&site, url("https://example.com:443/b?c").origin());
        assert_ne!(&site, url("https://www.example.com/").origin());
        assert_ne!(&site, url("https://example.com:8443/").origin());
        assert_ne!(&site, url("https://example.co/").origin());
    }

    /// Every URL this module won't read is refused with its reason.
    #[test]
    fn a_url_is_refused_with_its_reason() {
        let cases = [
            ("http://example.com/", UrlRefusal::NotHttps),
            ("ftp://example.com/", UrlRefusal::NotHttps),
            ("example.com", UrlRefusal::NotHttps),
            ("https:example.com", UrlRefusal::NotHttps),
            ("https://user:pw@example.com/", UrlRefusal::Userinfo),
            ("https://example.com@evil.example/", UrlRefusal::Userinfo),
            ("https://1.2.3.4/", UrlRefusal::IpLiteral),
            ("https://[::1]/", UrlRefusal::IpLiteral),
            ("https://0x7f000001/", UrlRefusal::IpLiteral),
            ("https://2130706433/", UrlRefusal::IpLiteral),
            ("https://exämple.com/", UrlRefusal::BadHost),
            ("https:///path", UrlRefusal::BadHost),
            ("https://-a.example/", UrlRefusal::BadHost),
            ("https://a-.example/", UrlRefusal::BadHost),
            ("https://a..example/", UrlRefusal::BadHost),
            ("https://example.com./", UrlRefusal::BadHost),
            ("https://ex_ample.com/", UrlRefusal::BadHost),
            ("https://example.com:0/", UrlRefusal::BadPort),
            ("https://example.com:65536/", UrlRefusal::BadPort),
            ("https://example.com:+443/", UrlRefusal::BadPort),
            ("https://example.com:/", UrlRefusal::BadPort),
            ("https://example.com/a b", UrlRefusal::BadCharacter),
            ("https://example.com/a\\b", UrlRefusal::BadCharacter),
            ("https://example.com/\u{7f}", UrlRefusal::BadCharacter),
            ("https://example.com/caf\u{e9}", UrlRefusal::BadCharacter),
        ];
        for (raw, refusal) in cases {
            assert_eq!(WebUrl::parse(raw), Err(refusal), "{raw}");
        }
        let long_label = format!("https://{}.example/", "a".repeat(64));
        assert_eq!(WebUrl::parse(&long_label), Err(UrlRefusal::BadHost));
        let long_host = format!("https://{}example/", "a.".repeat(127));
        assert_eq!(WebUrl::parse(&long_host), Err(UrlRefusal::BadHost));
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        assert_eq!(WebUrl::parse(&long), Err(UrlRefusal::TooLong));
        let at_bound = format!(
            "https://example.com/{}",
            "a".repeat(MAX_URL_BYTES - "https://example.com/".len())
        );
        assert!(WebUrl::parse(&at_bound).is_ok());
        let label_at_bound = format!("https://{}.example/", "a".repeat(63));
        assert!(WebUrl::parse(&label_at_bound).is_ok());
    }

    /// A redirect's location joins its URL: absolute, scheme-relative, or
    /// an absolute path on the same origin; nothing else.
    #[test]
    fn a_redirect_location_is_absolute_or_an_absolute_path() {
        let from = url("https://example.com:8443/a/b");
        assert_eq!(
            from.join("/c?d").unwrap().to_string(),
            "https://example.com:8443/c?d"
        );
        assert_eq!(
            from.join("//other.example/x").unwrap().to_string(),
            "https://other.example/x"
        );
        assert_eq!(
            from.join("https://other.example/y").unwrap().to_string(),
            "https://other.example/y"
        );
        assert_eq!(
            from.join("http://other.example/"),
            Err(UrlRefusal::NotHttps)
        );
        assert_eq!(from.join("c"), Err(UrlRefusal::Relative));
        assert_eq!(from.join("../c"), Err(UrlRefusal::Relative));
    }

    fn resolving_to(addrs: Vec<SocketAddr>) -> impl Fn(&str) -> std::io::Result<Vec<SocketAddr>> {
        move |_| Ok(addrs.clone())
    }

    const fn addr(a: u8, b: u8, c: u8, d: u8) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), 443))
    }

    /// A host that resolves to any internal address, or to none, is
    /// refused before a connection is made.
    #[test]
    fn a_host_resolving_inside_or_nowhere_is_refused_unconnected() {
        let tls = TlsClient::bundled().unwrap();
        let page = url("https://example.com/");
        let inside = fetch_with(
            &tls,
            &page,
            resolving_to(vec![addr(93, 184, 215, 14), addr(10, 0, 0, 5)]),
            is_internal,
        );
        assert_eq!(
            inside,
            Err(FetchError::Internal {
                addr: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))
            })
        );
        let nowhere = fetch_with(&tls, &page, resolving_to(Vec::new()), is_internal);
        assert_eq!(nowhere, Err(FetchError::Unresolved));
        let failing = fetch_with(
            &tls,
            &page,
            |_| Err(std::io::Error::other("no such name")),
            is_internal,
        );
        assert_eq!(failing, Err(FetchError::Unresolved));
        // The system resolver's localhost is internal.
        let local = fetch(&tls, &url("https://localhost/"));
        assert!(
            matches!(local, Err(FetchError::Internal { addr }) if addr.is_loopback()),
            "{local:?}"
        );
    }

    /// Fetches `answer` from a test server trusted for `localhost`,
    /// returning what came back and the request head the server read.
    fn fetched(answer: &str, target: &str) -> (Result<Fetched, FetchError>, String) {
        let pki = pki("localhost");
        let (server, head) = serve(&pki, answer.as_bytes().to_vec(), Ending::Notify);
        let tls = TlsClient::with_operator_bundle(pki.ca_pem.as_bytes()).unwrap();
        let page = url(&format!("https://localhost:{}{target}", server.port()));
        let got = fetch_with(&tls, &page, resolving_to(vec![server]), |_| false);
        let head = head
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_default();
        (got, head)
    }

    /// A page is fetched with one `GET`, no credential, and comes back as
    /// its visible text.
    #[test]
    fn a_page_comes_back_as_its_text() {
        let body = "<html><head><title>T</title><script>steal()</script></head><body><p>Hello &amp; welcome</p></body></html>";
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (got, head) = fetched(&answer, "/docs?x=1");
        let Ok(Fetched::Page(page)) = got.clone() else {
            unreachable!("no page: {got:?}");
        };
        assert_eq!(page.status, 200);
        assert!(page.html);
        assert_eq!(page.text, "T\nHello & welcome");
        assert!(!page.truncated);
        assert!(page.url.to_string().ends_with("/docs?x=1"));
        assert!(head.starts_with("GET /docs?x=1 HTTP/1.1\r\n"), "{head}");
        assert!(
            !head.to_ascii_lowercase().contains("authorization"),
            "{head}"
        );
    }

    /// Plain text passes as it is; a type that isn't text is refused.
    #[test]
    fn text_passes_and_other_types_are_refused() {
        let (got, _) = fetched(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"a\": \"<b>\"}\n",
            "/",
        );
        let Ok(Fetched::Page(page)) = got.clone() else {
            unreachable!("no page: {got:?}");
        };
        assert_eq!((page.status, page.html), (404, false));
        assert_eq!(page.text, "{\"a\": \"<b>\"}\n");
        let (got, _) = fetched(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 4\r\n\r\n\u{89}PNG",
            "/",
        );
        assert_eq!(
            got,
            Err(FetchError::NotText {
                content_type: String::from("image/png")
            })
        );
        let (got, _) = fetched("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi", "/");
        assert_eq!(
            got,
            Err(FetchError::NotText {
                content_type: String::from("none")
            })
        );
    }

    /// A redirect comes back, not followed; one with no location, or to a
    /// location that is refused, is an error.
    #[test]
    fn a_redirect_is_returned_not_followed() {
        let (got, _) = fetched(
            "HTTP/1.1 302 Found\r\nLocation: https://other.example/next\r\nContent-Length: 0\r\n\r\n",
            "/",
        );
        assert_eq!(
            got,
            Ok(Fetched::Redirect {
                status: 302,
                to: url("https://other.example/next")
            })
        );
        let (got, _) = fetched("HTTP/1.1 301 Moved\r\nContent-Length: 0\r\n\r\n", "/");
        assert_eq!(got, Err(FetchError::NoLocation));
        let (got, _) = fetched(
            "HTTP/1.1 307 Temporary\r\nLocation: http://other.example/\r\nContent-Length: 0\r\n\r\n",
            "/",
        );
        assert_eq!(got, Err(FetchError::BadRedirect(UrlRefusal::NotHttps)));
        // 304 isn't a redirect: it has no page to give, and no type.
        let (got, _) = fetched(
            "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\n\r\n",
            "/",
        );
        assert!(matches!(got, Err(FetchError::NotText { .. })), "{got:?}");
        let (got, _) = fetched(
            "HTTP/1.1 399 Odd\r\nLocation: /x\r\nContent-Length: 0\r\n\r\n",
            "/",
        );
        assert!(
            matches!(got, Ok(Fetched::Redirect { status: 399, .. })),
            "{got:?}"
        );
        let (got, _) = fetched(
            "HTTP/1.1 400 Bad\r\nLocation: /x\r\nContent-Type: text/plain\r\nContent-Length: 0\r\n\r\n",
            "/",
        );
        assert!(matches!(got, Ok(Fetched::Page(_))), "{got:?}");
    }

    /// A page past the proxy's bound is refused; a server that fails the
    /// handshake is a failure.
    #[test]
    fn an_oversized_or_broken_answer_is_an_error() {
        let size = crate::MAX_RESPONSE_BYTES + 1;
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {size}\r\n\r\n{}",
            "a".repeat(size)
        );
        let (got, _) = fetched(&answer, "/");
        assert_eq!(got, Err(FetchError::TooLarge));
        let (got, _) = fetched("not http at all\r\n\r\n", "/");
        assert_eq!(got, Err(FetchError::Failed));
    }

    /// Text past [`MAX_TEXT_BYTES`] is cut at a character boundary, and
    /// says so.
    #[test]
    fn text_is_bounded_at_a_character_boundary() {
        assert_eq!(
            bounded(String::from("abc"), 3),
            (String::from("abc"), false)
        );
        assert_eq!(
            bounded(String::from("abcd"), 3),
            (String::from("abc"), true)
        );
        assert_eq!(
            bounded(String::from("a\u{e9}"), 2),
            (String::from("a"), true)
        );
        let size = MAX_TEXT_BYTES + 10;
        let answer = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {size}\r\n\r\n{}",
            "b".repeat(size)
        );
        let (got, _) = fetched(&answer, "/");
        let Ok(Fetched::Page(page)) = got.clone() else {
            unreachable!("no page: {got:?}");
        };
        assert_eq!((page.text.len(), page.truncated), (MAX_TEXT_BYTES, true));
    }

    /// HTML's visible text: scripts, styles, comments and tags gone,
    /// blocks on lines of their own, references decoded, space collapsed.
    #[test]
    fn html_reduces_to_its_visible_text() {
        assert_eq!(
            html_text("<p>One   two\n\tthree</p><p>Four</p>"),
            "One two three\nFour"
        );
        assert_eq!(
            html_text("a<SCRIPT type=x>if (a<b) x()</SCRIPT>b<style>p{}</style>c<!-- hidden -->d"),
            "abcd"
        );
        assert_eq!(html_text("<ul><li>x</li><li>y</li></ul>"), "- x\n- y");
        assert_eq!(
            html_text("1 < 2 &lt; 3 &#62; &#x41;&amp;&nbsp;z"),
            "1 < 2 < 3 > A& z"
        );
        assert_eq!(html_text("&bogus; &amp &#xZZ; &"), "&bogus; &amp &#xZZ; &");
        assert_eq!(html_text("a<br/>b<br>c"), "a\nb\nc");
        assert_eq!(html_text("<p>a</p>\n\n\n<p></p><p></p><p>b</p>"), "a\nb");
        assert_eq!(html_text("x<script/>y"), "xy");
        assert_eq!(html_text("x<script>never closed"), "x");
        assert_eq!(html_text("x<!-- never closed"), "x");
        assert_eq!(html_text("x<b unclosed"), "x");
        assert_eq!(html_text("a\u{0}b\u{7}c"), "abc");
        assert_eq!(html_text("<span>a</span><span>b</span>"), "ab");
        assert_eq!(html_text("<h1>T</h1>body<?xml x?>"), "T\nbody");
    }

    /// Only text types are readable; HTML is told apart.
    #[test]
    fn readable_types_are_text() {
        assert_eq!(readable("text/html"), Some(true));
        assert_eq!(readable("Application/XHTML+xml; charset=x"), Some(true));
        for plain in [
            "text/plain",
            "text/markdown",
            "text/csv",
            "text/xml",
            "application/json",
            "application/xml",
        ] {
            assert_eq!(readable(plain), Some(false), "{plain}");
        }
        assert_eq!(readable("application/pdf"), None);
        assert_eq!(readable(""), None);
    }

    /// The parser's parts, called directly.
    #[test]
    fn the_parsers_parts() {
        assert_eq!(strip_prefix_ignore_case("HtTpS://x", "https://"), Some("x"));
        assert_eq!(strip_prefix_ignore_case("http://x", "https://"), None);
        assert_eq!(strip_prefix_ignore_case("http", "https://"), None);
        let origin = parse_authority("Example.com:8443").unwrap();
        assert_eq!((origin.host(), origin.port()), ("example.com", 8443));
        assert_eq!(origin.to_string(), "https://example.com:8443");
        assert_eq!(parse_authority("example.com").unwrap().port(), 443);
        assert_eq!(parse_authority("[::1]:443"), Err(UrlRefusal::IpLiteral));
        assert_eq!(parse_authority("10.0.0.1"), Err(UrlRefusal::IpLiteral));
        assert_eq!(parse_authority(""), Err(UrlRefusal::BadHost));
        assert_eq!(parse_port("65535"), Ok(65535));
        assert_eq!(parse_port("1"), Ok(1));
        for bad in ["0", "65536", "", "-1", "8o"] {
            assert_eq!(parse_port(bad), Err(UrlRefusal::BadPort), "{bad}");
        }
        assert!(valid_label("a-b1"));
        assert!(valid_label(&"a".repeat(63)));
        for bad in ["", "-a", "a-", "A", "a_b", "a b"] {
            assert!(!valid_label(bad), "{bad}");
        }
        assert!(!valid_label(&"a".repeat(64)));
        assert!(numeric_label("127"));
        assert!(numeric_label("0x7F"));
        assert!(!numeric_label("com"));
        assert!(!numeric_label("0xzz"));
        assert!(!numeric_label("1a"));
    }

    /// The text extractor's parts, called directly.
    #[test]
    fn the_text_extractors_parts() {
        let mut text = String::from("a");
        assert_eq!(skip_markup("<p>b", 0, &mut text), 3);
        assert_eq!(text, "a\n");
        let mut text = String::new();
        assert_eq!(skip_markup("< b", 0, &mut text), 1);
        assert_eq!(text, "<");
        let mut text = String::from("x");
        assert_eq!(skip_markup("<li>", 0, &mut text), 4);
        assert_eq!(text, "x\n- ");
        let mut text = String::new();
        assert_eq!(skip_markup("<!-- c -->d", 0, &mut text), 10);
        assert_eq!(skip_markup("<style>p</style>d", 0, &mut text), 16);
        assert_eq!(text, "");
        let mut text = String::new();
        push_text(&mut text, "a &amp; b&lt;");
        assert_eq!(text, "a & b<");
        let mut text = String::new();
        push_plain(&mut text, "  a \t\n b\u{1}");
        assert_eq!(text, "a b");
        assert_eq!(entity("&amp;x"), Some(('&', 5)));
        assert_eq!(entity("&#65;"), Some(('A', 5)));
        assert_eq!(entity("&#x263A;"), Some(('\u{263a}', 8)));
        assert_eq!(entity("&#X41;"), Some(('A', 6)));
        assert_eq!(entity("&quot;&apos;&gt;"), Some(('"', 6)));
        assert_eq!(entity("&apos;"), Some(('\'', 6)));
        assert_eq!(entity("&gt;"), Some(('>', 4)));
        assert_eq!(entity("&nbsp;"), Some(('\u{a0}', 6)));
        assert_eq!(entity("&#xD800;"), None, "a surrogate is no character");
        assert_eq!(entity("&amp"), None);
        assert_eq!(entity("&verylongname;"), None);
        assert_eq!(tidy("\n\n a \n\n\n b \n\n"), "a\n\nb");
        assert_eq!(tidy(""), "");
    }

    /// `resolved`, called directly: the addresses, or why there are none
    /// to use.
    #[test]
    fn resolved_returns_the_addresses_or_why_not() {
        let public = addr(93, 184, 215, 14);
        assert_eq!(
            resolved("a.example:443", resolving_to(vec![public]), is_internal),
            Ok(vec![public])
        );
        assert_eq!(
            resolved("a.example:443", resolving_to(Vec::new()), is_internal),
            Err(FetchError::Unresolved)
        );
        assert_eq!(
            resolved(
                "a.example:443",
                |_| Err(std::io::Error::other("no")),
                is_internal
            ),
            Err(FetchError::Unresolved)
        );
        assert_eq!(
            resolved(
                "a.example:443",
                resolving_to(vec![public, addr(127, 0, 0, 1)]),
                is_internal
            ),
            Err(FetchError::Internal {
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST)
            })
        );
    }
}
