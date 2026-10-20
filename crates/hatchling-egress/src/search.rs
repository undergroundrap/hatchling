//! A web search the broker runs (WO-028): Brave Search's web API, asked by
//! the broker with a key the cell never holds.
//!
//! - **The provider's host is resolved and checked per search**, as a
//!   page's is ([`crate::web`]): refused if it resolves to an internal
//!   address, and the connection pinned to what was checked.
//! - **The key travels only in the request's own wiped buffer:** it is
//!   added to the framed request, which is zeroized when dropped, and never
//!   held in a header list.
//! - **Bounded:** a query of at most [`MAX_QUERY_CHARS`]; at most
//!   [`MAX_RESULTS`] results, each title and snippet reduced to its text
//!   and cut short; the answer under the proxy's 1 MiB. A result whose URL
//!   isn't one [`WebUrl::parse`] reads is dropped.
//!
//! What comes back is the provider's word about other sites: whoever shows
//! it to a model wraps it as untrusted data.

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};

use serde_json::Value;
use zeroize::Zeroizing;

use crate::https::TlsClient;
use crate::web::{FetchError, Origin, WebUrl, html_text};
use crate::{EgressRequest, is_internal};

/// Brave Search's API host.
pub const BRAVE_HOST: &str = "api.search.brave.com";

/// The longest query, in characters.
pub const MAX_QUERY_CHARS: usize = 400;

/// The most results a search gives back.
pub const MAX_RESULTS: usize = 10;

/// The longest title a result keeps, in characters.
pub const MAX_TITLE_CHARS: usize = 300;

/// The longest snippet a result keeps, in characters.
pub const MAX_SNIPPET_CHARS: usize = 600;

/// The header Brave Search reads its key from.
const KEY_HEADER: &str = "X-Subscription-Token";

/// One result: the provider's title and snippet, as text, and the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The page's title.
    pub title: String,
    /// The page.
    pub url: WebUrl,
    /// What the provider says of it.
    pub snippet: String,
}

/// Why a search got no results.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    /// The query is empty, or only space.
    #[error("the query is empty")]
    Empty,
    /// The query is longer than [`MAX_QUERY_CHARS`].
    #[error("the query is longer than {MAX_QUERY_CHARS} characters")]
    TooLong,
    /// The key can't travel in a header.
    #[error("the search key can't be sent")]
    BadKey,
    /// The provider couldn't be reached, or its answer read.
    #[error("the search provider: {0}")]
    Unreachable(FetchError),
    /// The provider answered with a status other than 200.
    #[error("the search provider refused the search (HTTP {status})")]
    Status {
        /// The status.
        status: u16,
    },
    /// The answer isn't the provider's JSON.
    #[error("the search provider's answer couldn't be read")]
    Malformed,
}

/// Searches Brave Search for `query` with `key`, resolving its host with
/// the system's resolver.
///
/// # Errors
///
/// The [`SearchError`] that says why there are no results.
pub fn brave(tls: &TlsClient, key: &[u8], query: &str) -> Result<Vec<Hit>, SearchError> {
    let origin = WebUrl::parse(&format!("https://{BRAVE_HOST}/"))
        .map_err(|_| SearchError::Malformed)?
        .origin()
        .clone();
    brave_at(
        tls,
        &origin,
        key,
        query,
        |destination| destination.to_socket_addrs().map(Iterator::collect),
        is_internal,
    )
}

/// [`brave`], at `origin`, resolving with `resolve` and refusing what
/// `internal` says is internal.
pub(crate) fn brave_at(
    tls: &TlsClient,
    origin: &Origin,
    key: &[u8],
    query: &str,
    resolve: impl Fn(&str) -> std::io::Result<Vec<SocketAddr>>,
    internal: impl Fn(IpAddr) -> bool,
) -> Result<Vec<Hit>, SearchError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(SearchError::Empty);
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(SearchError::TooLong);
    }
    let key = std::str::from_utf8(key)
        .ok()
        .filter(|key| !key.is_empty() && crate::local::is_field_value(key))
        .ok_or(SearchError::BadKey)?;
    let destination = origin.destination();
    let addrs =
        crate::web::resolved(&destination, resolve, internal).map_err(SearchError::Unreachable)?;
    let request = EgressRequest {
        method: String::from("GET"),
        destination: destination.clone(),
        path: format!(
            "/res/v1/web/search?q={}&count={MAX_RESULTS}",
            percent_encode(query)
        ),
        headers: vec![(String::from("Accept"), String::from("application/json"))],
        body: Vec::new(),
        surrogate_b64: String::new(),
    };
    let bytes = with_key(
        &crate::local::request_bytes(&request, None).map_err(|_| SearchError::Malformed)?,
        key,
    );
    let response = crate::https::send(tls, &addrs, &destination, &bytes).map_err(|error| {
        SearchError::Unreachable(if matches!(error, crate::Error::ResponseTooLarge) {
            FetchError::TooLarge
        } else {
            FetchError::Failed
        })
    })?;
    if response.status != 200 {
        return Err(SearchError::Status {
            status: response.status,
        });
    }
    hits(&response.body)
}

/// The framed request `framed`, ending in its blank line, with the key's
/// header added before it: in a buffer zeroized when dropped.
fn with_key(framed: &[u8], key: &str) -> Zeroizing<Vec<u8>> {
    let head = framed.strip_suffix(b"\r\n").unwrap_or(framed);
    let mut bytes = Zeroizing::new(Vec::with_capacity(framed.len() + key.len() + 32));
    bytes.extend_from_slice(head);
    bytes.extend_from_slice(KEY_HEADER.as_bytes());
    bytes.extend_from_slice(b": ");
    bytes.extend_from_slice(key.as_bytes());
    bytes.extend_from_slice(b"\r\n\r\n");
    bytes
}

/// `query` percent-encoded for a URL's query: unreserved characters as
/// they are, every other byte as `%XX`.
fn percent_encode(query: &str) -> String {
    let mut encoded = String::new();
    for byte in query.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// The results in Brave Search's answer `body`: `web.results`, each with a
/// `title`, a `url` and a `description`.
fn hits(body: &[u8]) -> Result<Vec<Hit>, SearchError> {
    let answer: Value = serde_json::from_slice(body).map_err(|_| SearchError::Malformed)?;
    let Some(results) = answer
        .get("web")
        .and_then(|web| web.get("results"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    Ok(results
        .iter()
        .filter_map(|result| {
            let text = |key: &str, max: usize| {
                let raw = result.get(key).and_then(Value::as_str).unwrap_or_default();
                html_text(raw).chars().take(max).collect::<String>()
            };
            let url = result
                .get("url")
                .and_then(Value::as_str)
                .and_then(|url| WebUrl::parse(url).ok())?;
            Some(Hit {
                title: text("title", MAX_TITLE_CHARS),
                url,
                snippet: text("description", MAX_SNIPPET_CHARS),
            })
        })
        .take(MAX_RESULTS)
        .collect())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::https::tests::{Ending, pki, serve};

    /// Searches a test server trusted for `localhost` that answers
    /// `answer`: the results, and the request head the server read.
    fn searched(answer: &str, key: &[u8], query: &str) -> (Result<Vec<Hit>, SearchError>, String) {
        let pki = pki("localhost");
        let (server, head) = serve(&pki, answer.as_bytes().to_vec(), Ending::Notify);
        let tls = TlsClient::with_operator_bundle(pki.ca_pem.as_bytes()).unwrap();
        let origin = WebUrl::parse(&format!("https://localhost:{}/", server.port()))
            .unwrap()
            .origin()
            .clone();
        let got = brave_at(
            &tls,
            &origin,
            key,
            query,
            move |_| Ok(vec![server]),
            |_| false,
        );
        let head = head
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_default();
        (got, head)
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    /// The query goes encoded, with the key in its header; the results
    /// come back as text, a result with a URL that isn't read dropped.
    #[test]
    fn a_search_sends_the_key_and_reads_the_results() {
        let body = serde_json::json!({ "web": { "results": [
            {
                "title": "Rust <strong>1.98</strong> &amp; more",
                "url": "https://blog.example/rust",
                "description": "What's new &#x27;here&#x27;",
            },
            { "title": "Plain", "url": "http://insecure.example/", "description": "dropped" },
            { "title": "No description", "url": "https://docs.example/" },
        ] } })
        .to_string();
        let (got, head) = searched(&ok(&body), b"k-123", "  rust 1.98 & más ");
        assert_eq!(
            got.unwrap(),
            [
                Hit {
                    title: String::from("Rust 1.98 & more"),
                    url: WebUrl::parse("https://blog.example/rust").unwrap(),
                    snippet: String::from("What's new 'here'"),
                },
                Hit {
                    title: String::from("No description"),
                    url: WebUrl::parse("https://docs.example/").unwrap(),
                    snippet: String::new(),
                },
            ]
        );
        assert!(
            head.starts_with(
                "GET /res/v1/web/search?q=rust%201.98%20%26%20m%C3%A1s&count=10 HTTP/1.1\r\n"
            ),
            "{head}"
        );
        assert!(
            head.contains("\r\nX-Subscription-Token: k-123\r\n"),
            "{head}"
        );
        assert!(head.contains("\r\nAccept: application/json\r\n"), "{head}");
        assert!(
            !head.to_ascii_lowercase().contains("authorization"),
            "{head}"
        );
    }

    /// No `web` in the answer is no results; a status other than 200, an
    /// answer that isn't JSON, and a broken server are errors.
    #[test]
    fn an_answer_without_results_or_a_failed_one() {
        let empty = serde_json::json!({ "query": {} }).to_string();
        let (got, _) = searched(&ok(&empty), b"k", "q");
        assert_eq!(got, Ok(Vec::new()));
        let (got, _) = searched(
            "HTTP/1.1 429 Too Many\r\nContent-Length: 0\r\n\r\n",
            b"k",
            "q",
        );
        assert_eq!(got, Err(SearchError::Status { status: 429 }));
        let (got, _) = searched(&ok("not json"), b"k", "q");
        assert_eq!(got, Err(SearchError::Malformed));
        let (got, _) = searched("garbage\r\n\r\n", b"k", "q");
        assert_eq!(got, Err(SearchError::Unreachable(FetchError::Failed)));
        let size = crate::MAX_RESPONSE_BYTES + 1;
        let big = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\n\r\n{}",
            "a".repeat(size)
        );
        let (got, _) = searched(&big, b"k", "q");
        assert_eq!(got, Err(SearchError::Unreachable(FetchError::TooLarge)));
    }

    /// A query that's empty or too long, and a key that can't travel in a
    /// header, are refused before any request; so is a provider host that
    /// resolves inside.
    #[test]
    fn refusals_come_before_the_request() {
        let tls = TlsClient::bundled().unwrap();
        let origin = WebUrl::parse("https://search.example/")
            .unwrap()
            .origin()
            .clone();
        let never = |_: &str| -> std::io::Result<Vec<SocketAddr>> {
            Err(std::io::Error::other("not resolved"))
        };
        let search =
            |key: &[u8], query: &str| brave_at(&tls, &origin, key, query, never, |_| false);
        assert_eq!(search(b"k", "   "), Err(SearchError::Empty));
        assert_eq!(
            search(b"k", &"q".repeat(MAX_QUERY_CHARS + 1)),
            Err(SearchError::TooLong)
        );
        assert_eq!(
            search(b"k", &"q".repeat(MAX_QUERY_CHARS)),
            Err(SearchError::Unreachable(FetchError::Unresolved)),
            "a query at the bound is sent"
        );
        for bad in [&b""[..], b"a\r\nInjected: 1", b"\xff"] {
            assert_eq!(search(bad, "q"), Err(SearchError::BadKey));
        }
        let inside = brave_at(
            &tls,
            &origin,
            b"k",
            "q",
            |_| Ok(vec!["10.0.0.9:443".parse().unwrap()]),
            is_internal,
        );
        assert_eq!(
            inside,
            Err(SearchError::Unreachable(FetchError::Internal {
                addr: "10.0.0.9".parse().unwrap()
            }))
        );
    }

    /// At most [`MAX_RESULTS`] results, each title and snippet cut short.
    #[test]
    fn results_are_bounded() {
        let long = "t".repeat(MAX_SNIPPET_CHARS + 50);
        let results: Vec<Value> = (0..MAX_RESULTS + 3)
            .map(|n| {
                serde_json::json!({
                    "title": long,
                    "url": format!("https://r{n}.example/"),
                    "description": long,
                })
            })
            .collect();
        let body = serde_json::json!({ "web": { "results": results } }).to_string();
        let hits = hits(body.as_bytes()).unwrap();
        assert_eq!(hits.len(), MAX_RESULTS);
        let first = hits.first().unwrap();
        assert_eq!(first.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(first.snippet.chars().count(), MAX_SNIPPET_CHARS);
    }

    /// The parts, called directly.
    #[test]
    fn the_parts() {
        assert_eq!(percent_encode("a-b._~Z9"), "a-b._~Z9");
        assert_eq!(percent_encode("a b/é?&="), "a%20b%2F%C3%A9%3F%26%3D");
        let framed = b"GET / HTTP/1.1\r\nHost: h\r\n\r\n";
        assert_eq!(
            with_key(framed, "k").as_slice(),
            b"GET / HTTP/1.1\r\nHost: h\r\nX-Subscription-Token: k\r\n\r\n"
        );
        assert_eq!(hits(b"[]"), Ok(Vec::new()));
        // The real provider's origin is a name, not refused by the parser.
        assert!(WebUrl::parse(&format!("https://{BRAVE_HOST}/")).is_ok());
        let tls = TlsClient::bundled().unwrap();
        assert_eq!(brave(&tls, b"k", ""), Err(SearchError::Empty));
    }
}
