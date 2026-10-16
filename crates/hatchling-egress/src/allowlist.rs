//! Destination allowlist: deny-by-default egress control.
//!
//! The allowlist is parsed once at startup from
//! `HATCHLING_EGRESS_ALLOWLIST` (comma-separated `host:port`). Every
//! entry must be explicit — no wildcards, no IP literals, no CIDR
//! ranges. An invalid entry fails the proxy closed at startup; there
//! is no "allow the valid ones and skip the bad" — a misconfigured
//! allowlist is a fail-closed proxy.
//!
//! At request time, the destination is checked against the allowlist
//! with exact string matching. DNS is resolved once at startup and the
//! IPs are pinned, eliminating DNS-rebinding between the allowlist
//! check and the connection.
//!
//! **Public sources.** An entry written `public:host:port` is
//! a public source, such as a page a research run reads. It is allowed
//! like any other destination, but the proxy only ever reads from it
//! (`GET`, no body) and never attaches a credential to it. A
//! destination is one kind or the other: listing it both ways is
//! ambiguous, and fails the allowlist closed.
//!
//! **Local model endpoints.** An entry written
//! `local:localhost:port` is a model server on this machine, such as
//! ollama or llama.cpp. "Local" is a claim to verify, not a trust
//! label: the host must be `localhost`, every address it resolves to
//! at startup must be `127.0.0.1` or `::1` (anything else, say from a
//! poisoned hosts file, fails the allowlist closed), and the proxy
//! connects only to those pinned addresses and checks each
//! connection's peer before sending a byte (see [`crate::local`]). A
//! local endpoint is spoken to in plain HTTP. Its credential is the
//! vault's, exactly as for any other destination; a server that needs
//! none is written `local-keyless:localhost:port`, so going without a
//! credential is always the operator's explicit choice, never what a
//! missing secret silently turns into.
//!
//! **Internal addresses.** Every other destination must resolve, at
//! startup, only to addresses on the public internet. One that resolves
//! to a loopback, private, shared, link-local, unique-local, unspecified,
//! multicast or cloud-metadata address (see [`is_internal`]) fails the
//! allowlist closed, naming the entry and the address: a poisoned or
//! stale record, or a typo, would otherwise give a cell a path into the
//! operator's own network. A destination that is meant to be internal
//! (a company's model gateway, split-horizon DNS) is written
//! `internal:host:port`, so going internal is always written down. It is
//! otherwise an ordinary credentialed destination, held to verified TLS.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

use crate::Error;

/// The prefix that marks a public source in the allowlist.
pub const PUBLIC_PREFIX: &str = "public:";

/// The prefix that marks a local model endpoint in the allowlist
/// loopback-pinned, plain HTTP, with the vault's credential.
pub const LOCAL_PREFIX: &str = "local:";

/// The prefix that marks a local model endpoint that takes no
/// credential: the explicit keyless marker.
pub const LOCAL_KEYLESS_PREFIX: &str = "local-keyless:";

/// The one host a local endpoint may name.
pub const LOCAL_HOST: &str = "localhost";

/// The prefix that marks a destination the operator means to be on an
/// internal network: credentialed and held to verified TLS like any
/// other, but allowed to resolve to the addresses [`is_internal`] names.
pub const INTERNAL_PREFIX: &str = "internal:";

/// Cloud metadata services outside the ranges [`is_internal`] already
/// refuses: Azure's wire server. (AWS, GCP and Oracle use 169.254.169.254,
/// a link-local address; Alibaba's 100.100.100.200 is in the shared range;
/// AWS's IPv6 `fd00:ec2::254` is unique-local.)
const METADATA: [Ipv4Addr; 1] = [Ipv4Addr::new(168, 63, 129, 16)];

/// Whether `ip` is an address a destination may reach only when the
/// operator wrote it `internal:`: loopback, private (RFC 1918), shared
/// (100.64.0.0/10), link-local, this network (0.0.0.0/8), broadcast,
/// multicast, a known metadata address, or IPv6 loopback, unspecified,
/// unique-local (fc00::/7), link-local (fe80::/10) or multicast. An
/// IPv4 address mapped into IPv6 (`::ffff:10.0.0.1`) is judged as the
/// IPv4 address it is.
#[must_use]
pub fn is_internal(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [first, second, ..] = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
                || first == 0
                || (first == 100 && (64..128).contains(&second))
                || METADATA.contains(&v4)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_internal(IpAddr::V4(v4));
            }
            let [first, ..] = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Refuses a destination that resolved to an internal address without
/// being written `internal:`, naming it and the address.
fn refuse_internal(entry: &str, resolved: &[SocketAddr]) -> Result<(), Error> {
    match resolved.iter().find(|addr| is_internal(addr.ip())) {
        Some(addr) => Err(Error::InvalidAllowlist {
            reason: format!(
                "destination {entry:?} resolves to {}, an internal address: if it is meant to be on your own network, write it {INTERNAL_PREFIX}{entry}",
                addr.ip()
            ),
        }),
        None => Ok(()),
    }
}

/// What kind of destination an allowlist entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A credentialed HTTPS destination.
    Credentialed,
    /// A credentialed HTTPS destination the operator means to be on an
    /// internal network.
    Internal,
    /// A public source: read-only, never credentialed.
    Public,
    /// A local model endpoint, credentialed unless `keyless`.
    Local {
        /// The operator's explicit marker that it takes no credential.
        keyless: bool,
    },
}

impl Kind {
    /// Splits an entry's kind prefix from its destination.
    fn of(entry: &str) -> (Self, &str) {
        [
            (LOCAL_KEYLESS_PREFIX, Self::Local { keyless: true }),
            (LOCAL_PREFIX, Self::Local { keyless: false }),
            (PUBLIC_PREFIX, Self::Public),
            (INTERNAL_PREFIX, Self::Internal),
        ]
        .into_iter()
        .find_map(|(prefix, kind)| entry.strip_prefix(prefix).map(|rest| (kind, rest)))
        .unwrap_or((Self::Credentialed, entry))
    }
}

/// A local model endpoint the allowlist admits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Local {
    /// The loopback addresses `localhost` resolved to at startup, IPv4
    /// first. The proxy connects to these and nothing else.
    addrs: Vec<SocketAddr>,
    /// Whether the operator marked it as taking no credential.
    keyless: bool,
}

impl Local {
    /// The pinned loopback addresses, IPv4 first.
    #[must_use]
    pub fn addrs(&self) -> &[SocketAddr] {
        &self.addrs
    }

    /// Whether the operator marked the endpoint as taking no credential.
    #[must_use]
    pub const fn keyless(&self) -> bool {
        self.keyless
    }
}

/// Whether `addr` is one of the two loopback addresses a local
/// endpoint may be pinned to: exactly `127.0.0.1` or `::1`.
#[must_use]
pub fn is_pinned_loopback(addr: &SocketAddr) -> bool {
    addr.ip() == IpAddr::V4(Ipv4Addr::LOCALHOST) || addr.ip() == IpAddr::V6(Ipv6Addr::LOCALHOST)
}

/// Pins a local endpoint's startup resolution: every address must be
/// loopback (a `localhost` that resolves anywhere else is an
/// impersonation, and fails closed), ordered IPv4 first.
fn pin_loopback(entry: &str, mut resolved: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, Error> {
    if let Some(addr) = resolved.iter().find(|addr| !is_pinned_loopback(addr)) {
        return Err(Error::InvalidAllowlist {
            reason: format!(
                "local endpoint {entry:?} resolves to {}, which is not loopback",
                addr.ip()
            ),
        });
    }
    resolved.sort_by_key(SocketAddr::is_ipv6);
    Ok(resolved)
}

/// A parsed egress allowlist: the exact set of permitted destinations.
#[derive(Debug, Clone)]
pub struct Allowlist {
    /// The allowed `host:port` strings, for exact matching.
    destinations: HashSet<String>,
    /// The allowed destinations that are public sources: read-only,
    /// and never given a credential.
    public: HashSet<String>,
    /// The allowed destinations that are local model endpoints, with
    /// their pinned loopback addresses.
    local: HashMap<String, Local>,
    /// Each destination's socket addresses, resolved once at startup.
    /// The proxy connects to these, never to a fresh DNS lookup.
    pinned: HashMap<String, Vec<SocketAddr>>,
}

impl Allowlist {
    /// Parses an allowlist from a comma-separated `host:port` string.
    ///
    /// Every entry must be a valid destination (see
    /// `surrogate::is_valid_destination`): explicit hostname, numeric
    /// port, no wildcards, no IP literals. Empty entries are ignored.
    ///
    /// An empty allowlist is valid: it denies everything. This is the
    /// fail-closed default — the broker serves no egress until the
    /// operator configures destinations.
    ///
    /// DNS is resolved now and the addresses pinned. A hostname that
    /// does not resolve is an error — fail closed, don't defer the
    /// failure to request time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidAllowlist`] if any entry is malformed
    /// or if a hostname fails to resolve.
    pub fn parse(raw: &str) -> Result<Self, Error> {
        Self::parse_with(raw, |entry| {
            entry.to_socket_addrs().map(Iterator::collect)
        })
    }

    /// [`parse`](Self::parse), resolving each destination with `resolve`
    /// rather than the system's resolver: the same rules, checked against
    /// whatever addresses it returns.
    ///
    /// # Errors
    ///
    /// As [`parse`](Self::parse).
    pub fn parse_with(
        raw: &str,
        resolve: impl Fn(&str) -> std::io::Result<Vec<SocketAddr>>,
    ) -> Result<Self, Error> {
        let mut destinations = HashSet::new();
        let mut public = HashSet::new();
        let mut local = HashMap::new();
        let mut kinds: HashMap<String, Kind> = HashMap::new();
        let mut pinned = HashMap::new();

        for entry in raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (kind, entry) = Kind::of(entry);
            // Reuse the surrogate destination validator: the same
            // rules apply (explicit host:port, no wildcards, no IP
            // literals).
            if !crate::surrogate::is_valid_destination(entry) {
                return Err(Error::InvalidAllowlist {
                    reason: format!("invalid destination {entry:?}"),
                });
            }
            if matches!(kind, Kind::Local { .. })
                && entry.rsplit_once(':').map(|(host, _)| host) != Some(LOCAL_HOST)
            {
                return Err(Error::InvalidAllowlist {
                    reason: format!("local endpoint {entry:?} must be {LOCAL_HOST}:port"),
                });
            }
            // Pin the DNS resolution now. If it fails, the allowlist
            // is misconfigured — fail closed at startup.
            let resolved = resolve(entry).map_err(|_| Error::InvalidAllowlist {
                reason: format!("destination {entry:?} does not resolve"),
            })?;
            if resolved.is_empty() {
                return Err(Error::InvalidAllowlist {
                    reason: format!("destination {entry:?} resolved to no addresses"),
                });
            }
            // One destination, one kind: a public source and a
            // credentialed API under the same name is ambiguous.
            if kinds.get(entry).is_some_and(|listed| *listed != kind) {
                return Err(Error::InvalidAllowlist {
                    reason: format!("destination {entry:?} is listed as two different kinds"),
                });
            }
            kinds.insert(entry.to_owned(), kind);
            destinations.insert(entry.to_owned());
            match kind {
                Kind::Public => {
                    refuse_internal(entry, &resolved)?;
                    public.insert(entry.to_owned());
                }
                Kind::Local { keyless } => {
                    let pinned = pin_loopback(entry, resolved.clone())?;
                    local.insert(
                        entry.to_owned(),
                        Local {
                            addrs: pinned,
                            keyless,
                        },
                    );
                }
                Kind::Credentialed => refuse_internal(entry, &resolved)?,
                Kind::Internal => {}
            }
            pinned.insert(entry.to_owned(), resolved);
        }

        Ok(Self {
            destinations,
            public,
            local,
            pinned,
        })
    }

    /// The local model endpoint at `destination`, if the allowlist
    /// lists it as one.
    #[must_use]
    pub fn local(&self, destination: &str) -> Option<&Local> {
        self.local.get(destination)
    }

    /// Returns `true` if the destination is an allowed public source:
    /// read-only, and never given a credential.
    #[must_use]
    pub fn is_public(&self, destination: &str) -> bool {
        self.public.contains(destination)
    }

    /// Returns `true` if the destination is on the allowlist (exact
    /// string match).
    #[must_use]
    pub fn allows(&self, destination: &str) -> bool {
        self.destinations.contains(destination)
    }

    /// The socket addresses an allowed destination was pinned to at
    /// startup, or `None` if the destination is not on the allowlist. No
    /// lookup happens here: these are the only addresses the proxy will
    /// connect to for it.
    #[must_use]
    pub fn addrs_for(&self, destination: &str) -> Option<&[SocketAddr]> {
        self.pinned.get(destination).map(Vec::as_slice)
    }

    /// The number of allowed destinations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.destinations.len()
    }

    /// Returns `true` if the allowlist has no destinations. (Cannot
    /// happen via [`parse`](Self::parse), which rejects empty
    /// allowlists — this is for completeness.)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.destinations.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A public address, from a documentation range (TEST-NET-3).
    const PUBLIC: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 5);

    /// Parses `raw`, resolving `localhost` to both loopback addresses,
    /// `gw.corp` to `internal`, and any other name to [`PUBLIC`].
    fn resolving(raw: &str, internal: &[IpAddr]) -> Result<Allowlist, Error> {
        let internal = internal.to_vec();
        Allowlist::parse_with(raw, move |entry| {
            let (host, port) = entry.rsplit_once(':').unwrap();
            let port: u16 = port.parse().unwrap();
            let ips = match host {
                "localhost" => vec![IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST)],
                "gw.corp" => internal.clone(),
                _ => vec![IpAddr::V4(PUBLIC)],
            };
            Ok(ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect())
        })
    }

    fn fake(raw: &str) -> Result<Allowlist, Error> {
        resolving(raw, &[])
    }

    #[test]
    fn parses_valid_allowlist() {
        // Use localhost, which always resolves: written internal, as it is.
        let allowlist = Allowlist::parse("internal:localhost:443,internal:localhost:80").unwrap();
        assert_eq!(allowlist.len(), 2);
        assert!(!allowlist.is_empty());
        assert!(allowlist.allows("localhost:443"));
        assert!(allowlist.allows("localhost:80"));
        assert!(!allowlist.allows("localhost:8080"));
        assert!(!allowlist.allows("example.com:443"));
    }

    #[test]
    fn rejects_wildcards() {
        assert!(Allowlist::parse("*.example.com:443").is_err());
    }

    #[test]
    fn rejects_ip_literals() {
        assert!(Allowlist::parse("192.168.1.1:443").is_err());
        assert!(Allowlist::parse("[::1]:443").is_err());
    }

    #[test]
    fn rejects_malformed_entries() {
        // Note: "" and "   " and ",,," are NOT in this list — an empty
        // allowlist is valid (deny everything). See
        // `ignores_empty_entries`.
        for bad in [
            "not-a-destination",
            "host:abc",
            "host:0",
            "https://example.com:443",
            "example.com:443/path",
        ] {
            assert!(Allowlist::parse(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        // An empty allowlist is valid: it denies everything. This is
        // the fail-closed default.
        for empty in ["", "   ", ",,,"] {
            let allowlist = Allowlist::parse(empty).unwrap();
            assert!(allowlist.is_empty());
            assert!(!allowlist.allows("localhost:443"));
        }
    }

    #[test]
    fn ignores_empty_entries() {
        let allowlist = Allowlist::parse("internal:localhost:443,, internal:localhost:80 ,").unwrap();
        assert_eq!(allowlist.len(), 2);
    }

    #[test]
    fn public_sources_are_allowed_and_marked() {
        let allowlist = fake("public:docs.example:443, api.example:443").unwrap();
        assert_eq!(allowlist.len(), 2);
        assert!(allowlist.allows("docs.example:443"));
        assert!(allowlist.allows("api.example:443"));
        assert!(allowlist.is_public("docs.example:443"));
        assert!(!allowlist.is_public("api.example:443"));
        assert!(!allowlist.is_public("example.com:443"));
        // The same kind twice is one destination.
        let twice = fake("public:docs.example:443,public:docs.example:443").unwrap();
        assert_eq!(twice.len(), 1);
        assert!(twice.is_public("docs.example:443"));
        let plain = fake("api.example:443,api.example:443").unwrap();
        assert!(!plain.is_public("api.example:443"));
        assert_eq!(PUBLIC_PREFIX, "public:");
    }

    #[test]
    fn a_destination_listed_both_ways_or_a_bad_public_entry_fails_closed() {
        for bad in [
            "public:docs.example:443,docs.example:443",
            "docs.example:443,public:docs.example:443",
            "public:*.example.com:443",
            "public:192.168.1.1:443",
            "public:",
            "public: docs.example:443",
        ] {
            assert!(fake(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn a_local_endpoint_is_pinned_to_loopback_and_marked() {
        let allowlist =
            Allowlist::parse("local:localhost:11434, local-keyless:localhost:8080, internal:localhost:443")
                .unwrap();
        assert_eq!(allowlist.len(), 3);
        assert!(allowlist.allows("localhost:11434"));
        assert!(allowlist.allows("localhost:8080"));
        let keyed = allowlist.local("localhost:11434").unwrap();
        assert!(!keyed.keyless());
        let keyless = allowlist.local("localhost:8080").unwrap();
        assert!(keyless.keyless());
        for local in [keyed, keyless] {
            assert!(!local.addrs().is_empty());
            assert!(local.addrs().iter().all(is_pinned_loopback));
        }
        assert!(keyed.addrs().iter().all(|addr| addr.port() == 11434));
        // Neither an ordinary destination nor a public source is local,
        // and a local endpoint is not a public source.
        assert!(allowlist.local("localhost:443").is_none());
        assert!(allowlist.local("example.com:443").is_none());
        assert!(!allowlist.is_public("localhost:11434"));
        assert_eq!(
            (LOCAL_PREFIX, LOCAL_KEYLESS_PREFIX, LOCAL_HOST),
            ("local:", "local-keyless:", "localhost")
        );
    }

    #[test]
    fn a_local_endpoint_must_name_localhost_and_be_one_kind() {
        for bad in [
            "local:example.com:11434",
            "local:localhost.example.com:11434",
            "local:127.0.0.1:11434",
            "local:[::1]:11434",
            "local-keyless:example.com:8080",
            "local:localhost",
            "local:",
            "local:localhost:11434,localhost:11434",
            "local:localhost:11434,local-keyless:localhost:11434",
            "public:localhost:11434,local:localhost:11434",
            "internal:localhost:11434,local:localhost:11434",
        ] {
            assert!(Allowlist::parse(bad).is_err(), "should reject {bad:?}");
        }
        // The same kind twice is one destination.
        let twice = Allowlist::parse("local:localhost:11434,local:localhost:11434").unwrap();
        assert_eq!(twice.len(), 1);
    }

    #[test]
    fn only_the_two_loopback_addresses_are_pinnable() {
        let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
        assert!(is_pinned_loopback(&addr("127.0.0.1:11434")));
        assert!(is_pinned_loopback(&addr("[::1]:11434")));
        for other in [
            "127.0.0.2:11434",
            "10.0.0.5:11434",
            "0.0.0.0:11434",
            "[::]:11434",
            "[::ffff:127.0.0.1]:11434",
            "93.184.216.34:11434",
        ] {
            assert!(!is_pinned_loopback(&addr(other)), "{other} is not pinnable");
        }
    }

    #[test]
    fn a_localhost_that_resolves_off_the_machine_fails_closed() {
        // A poisoned hosts file or a rebinding resolver: one address of
        // the resolution is remote, and the entry is refused outright.
        let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
        let poisoned = vec![addr("127.0.0.1:11434"), addr("10.0.0.5:11434")];
        let error = pin_loopback("localhost:11434", poisoned).unwrap_err();
        assert!(
            matches!(&error, Error::InvalidAllowlist { reason } if reason == "local endpoint \"localhost:11434\" resolves to 10.0.0.5, which is not loopback"),
            "got {error:?}"
        );
        // A clean resolution is kept, IPv4 first.
        let pinned = pin_loopback(
            "localhost:11434",
            vec![addr("[::1]:11434"), addr("127.0.0.1:11434")],
        )
        .unwrap();
        assert_eq!(pinned, vec![addr("127.0.0.1:11434"), addr("[::1]:11434")]);
    }

    #[test]
    fn addrs_for_returns_pinned_addresses() {
        let allowlist = Allowlist::parse("internal:localhost:443").unwrap();
        let addrs = allowlist.addrs_for("localhost:443").unwrap();
        assert!(!addrs.is_empty());
        assert!(
            addrs
                .iter()
                .all(|addr| addr.port() == 443 && addr.ip().is_loopback())
        );
        assert!(allowlist.addrs_for("localhost:80").is_none());
        assert!(allowlist.addrs_for("example.com:443").is_none());
    }

    #[test]
    fn each_internal_range_is_internal_and_the_internet_is_not() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        for internal in [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "169.254.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "100.100.100.200",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "239.255.255.250",
            "168.63.129.16",
            "::1",
            "::",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "febf::1",
            "ff02::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
        ] {
            assert!(is_internal(ip(internal)), "{internal} is internal");
        }
        for public in [
            "8.8.8.8",
            "1.1.1.1",
            "203.0.113.5",
            "172.15.255.255",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "169.253.255.255",
            "168.63.129.17",
            "2606:4700:4700::1111",
            "fec0::1",
            "::ffff:8.8.8.8",
        ] {
            assert!(!is_internal(ip(public)), "{public} is public");
        }
    }

    #[test]
    fn a_destination_resolving_inside_fails_unless_written_internal() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        for inside in ["10.1.2.3", "169.254.169.254", "fd00::7", "::ffff:192.168.0.9"] {
            for raw in ["gw.corp:443", "public:gw.corp:443"] {
                let error = resolving(raw, &[ip(inside)]).unwrap_err();
                let Error::InvalidAllowlist { reason } = &error else {
                    unreachable!("{error:?}");
                };
                assert!(
                    reason.contains("\"gw.corp:443\"") && reason.contains(&ip(inside).to_string()),
                    "names the entry and the address: {reason}"
                );
                assert!(reason.contains("internal:gw.corp:443"), "says how to: {reason}");
            }
            // Written internal: accepted, pinned, an ordinary credentialed
            // destination (not public, not local).
            let allowlist = resolving("internal:gw.corp:443", &[ip(inside)]).unwrap();
            assert!(allowlist.allows("gw.corp:443"));
            assert_eq!(
                allowlist.addrs_for("gw.corp:443"),
                Some(&[SocketAddr::new(ip(inside), 443)][..])
            );
            assert!(!allowlist.is_public("gw.corp:443"));
            assert!(allowlist.local("gw.corp:443").is_none());
        }
        // One internal address among public ones is enough to refuse.
        let mixed = [IpAddr::V4(PUBLIC), ip("10.0.0.1")];
        assert!(resolving("gw.corp:443", &mixed).is_err());
        assert!(resolving("internal:gw.corp:443", &mixed).is_ok());
        // An internal destination that resolves publicly is still allowed.
        assert!(fake("internal:api.example:443").is_ok());
        assert_eq!(INTERNAL_PREFIX, "internal:");
    }

    #[test]
    fn an_internal_destination_is_one_kind() {
        for bad in [
            "internal:gw.corp:443,gw.corp:443",
            "gw.corp:443,internal:gw.corp:443",
            "internal:gw.corp:443,public:gw.corp:443",
            "internal:*.corp:443",
            "internal:10.0.0.1:443",
            "internal:",
        ] {
            assert!(
                resolving(bad, &[IpAddr::V4(PUBLIC)]).is_err(),
                "should reject {bad:?}"
            );
        }
        let twice = resolving("internal:gw.corp:443,internal:gw.corp:443", &[IpAddr::V4(PUBLIC)]).unwrap();
        assert_eq!(twice.len(), 1);
    }

    #[test]
    fn a_name_that_does_not_resolve_fails_closed() {
        let failing = Allowlist::parse_with("api.example:443", |_| {
            Err(std::io::Error::other("no such host"))
        });
        assert!(matches!(failing, Err(Error::InvalidAllowlist { reason }) if reason.contains("does not resolve")));
        let empty = Allowlist::parse_with("api.example:443", |_| Ok(Vec::new()));
        assert!(matches!(empty, Err(Error::InvalidAllowlist { reason }) if reason.contains("no addresses")));
    }
}
