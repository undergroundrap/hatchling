//! Surrogate tokens: broker-minted, peer-bound, destination-bound.
//!
//! A surrogate is an HMAC-authenticated token that authorizes one peer
//! to make requests to one destination until an expiry time. The agent
//! presents the surrogate; the proxy validates the HMAC, checks the
//! peer uid matches the presenter, checks the destination matches the
//! request, and checks the expiry — all before touching the network.
//!
//! The wire format is base64-encoded:
//! ```text
//! version: u8 (1)
//! peer_uid: u32 (big-endian)
//! expiry: u64 (big-endian, unix timestamp)
//! dest_len: u16 (big-endian)
//! destination: dest_len bytes (UTF-8, "host:port")
//! hmac: 32 bytes (HMAC-SHA256 over all preceding bytes)
//! ```
//!
//! Surrogates are opaque to the agent: it cannot mint, forge, or extend
//! them. Only the broker (which holds the HMAC key) mints surrogates,
//! and only for kernel-authorized peers with an explicit grant.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::Error;

type HmacSha256 = Hmac<Sha256>;

/// Surrogate format version. Bump if the format changes.
const SURROGATE_VERSION: u8 = 1;

/// Length of the HMAC-SHA256 tag in bytes.
const HMAC_LEN: usize = 32;

/// Maximum destination string length in bytes. Destinations are
/// `host:port`; this bound keeps surrogates small and parsing simple.
const MAX_DESTINATION_LEN: usize = 253 + 1 + 5; // max hostname + ':' + max port

/// Minimum payload length in bytes: version (1) + `peer_uid` (4) +
/// expiry (8) + `dest_len` (2) + HMAC (32). A shorter payload cannot
/// be a valid surrogate.
const MIN_PAYLOAD_LEN: usize = 1 + 4 + 8 + 2 + HMAC_LEN;

/// The HMAC key. Generated once per proxy instance, zeroized on drop.
#[derive(Debug)]
pub struct SurrogateKey(Zeroizing<[u8; 32]>);

impl SurrogateKey {
    /// Generates a fresh random key from the OS CSPRNG, filled in place
    /// so no unzeroized copy of it is left behind.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Random`] if the OS will not provide randomness.
    /// A surrogate key must be unpredictable, so there is no fallback to
    /// weaker entropy.
    pub fn generate() -> Result<Self, Error> {
        let mut key = Zeroizing::new([0u8; 32]);
        getrandom::fill(key.as_mut()).map_err(|source| Error::Random { source })?;
        Ok(Self(key))
    }
}

/// A validated surrogate: the peer uid, destination, and expiry it was
/// minted for. Returned by [`validate_surrogate`]; the proxy checks
/// these against the presenting peer and the requested destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSurrogate {
    /// The uid this surrogate was minted for.
    pub peer_uid: u32,
    /// The destination (`host:port`) this surrogate authorizes.
    pub destination: String,
    /// Unix timestamp after which the surrogate is invalid.
    pub expiry: u64,
}

/// Mints a surrogate for the given peer uid, destination, and TTL.
///
/// The destination must be `host:port` format and within length bounds.
/// The TTL is in seconds from now.
///
/// # Errors
///
/// Returns [`Error::InvalidSurrogate`] if the destination is malformed
/// or too long.
pub fn mint_surrogate(
    key: &SurrogateKey,
    peer_uid: u32,
    destination: &str,
    ttl_secs: u64,
) -> Result<String, Error> {
    // Fail fast on empty or over-long destinations. The `is_empty`
    // check is separate from the length check (not `||`) so each
    // bound is pinned independently by tests.
    if destination.is_empty() {
        return Err(Error::InvalidSurrogate);
    }
    if destination.len() > MAX_DESTINATION_LEN {
        return Err(Error::InvalidSurrogate);
    }
    // Destination must be host:port — no scheme, no path, no userinfo.
    // This is validated again at request time; defense in depth.
    if !is_valid_destination(destination) {
        return Err(Error::InvalidSurrogate);
    }

    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(ttl_secs, |d| d.as_secs().saturating_add(ttl_secs));

    let mut payload = Vec::with_capacity(1 + 4 + 8 + 2 + destination.len());
    payload.push(SURROGATE_VERSION);
    payload.extend_from_slice(&peer_uid.to_be_bytes());
    payload.extend_from_slice(&expiry.to_be_bytes());
    let dest_bytes = destination.as_bytes();
    let dest_len = u16::try_from(dest_bytes.len()).map_err(|_| Error::InvalidSurrogate)?;
    payload.extend_from_slice(&dest_len.to_be_bytes());
    payload.extend_from_slice(dest_bytes);

    let mut mac =
        HmacSha256::new_from_slice(key.0.as_slice()).map_err(|_| Error::InvalidSurrogate)?;
    mac.update(&payload);
    let tag = mac.finalize().into_bytes();
    payload.extend_from_slice(&tag);

    Ok(BASE64.encode(&payload))
}

/// Validates a surrogate, returning the bound peer uid, destination,
/// and expiry.
///
/// Checks: base64 decoding, version, HMAC signature, expiry, and
/// destination format. Does NOT check that the peer uid matches the
/// presenter or that the destination matches the request — the caller
/// does that.
///
/// # Errors
///
/// Returns [`Error::InvalidSurrogate`] for malformed or forged
/// surrogates, [`Error::ExpiredSurrogate`] for expired ones.
pub fn validate_surrogate(
    key: &SurrogateKey,
    surrogate_b64: &str,
) -> Result<ValidatedSurrogate, Error> {
    let payload = BASE64
        .decode(surrogate_b64)
        .map_err(|_| Error::InvalidSurrogate)?;

    // Minimum: version(1) + uid(4) + expiry(8) + dest_len(2) + hmac(32).
    // A payload of exactly MIN_PAYLOAD_LEN has a 15-byte body, so
    // dest_len must be 0 — and the empty destination then fails
    // `is_valid_destination`, rejecting the surrogate with
    // `InvalidSurrogate` either way. `<` vs `<=` is therefore
    // behaviorally identical here: the cargo-mutants `<` -> `<=`
    // mutant is provably equivalent, excluded in
    // `.cargo/mutants.toml` with this proof, and its rejection path
    // is pinned by `validate_rejects_exact_min_payload`.
    if payload.len() < MIN_PAYLOAD_LEN {
        return Err(Error::InvalidSurrogate);
    }

    let (body, tag) = payload.split_at(payload.len() - HMAC_LEN);

    let mut mac =
        HmacSha256::new_from_slice(key.0.as_slice()).map_err(|_| Error::InvalidSurrogate)?;
    mac.update(body);
    mac.verify_slice(tag).map_err(|_| Error::InvalidSurrogate)?;

    // HMAC verified: the body is authentic. Parse it without panicking
    // on malformed lengths — every access is bounds-checked.
    let version = body.first().ok_or(Error::InvalidSurrogate)?;
    if *version != SURROGATE_VERSION {
        return Err(Error::InvalidSurrogate);
    }
    let peer_uid = u32::from_be_bytes(
        body.get(1..5)
            .ok_or(Error::InvalidSurrogate)?
            .try_into()
            .map_err(|_| Error::InvalidSurrogate)?,
    );
    let expiry = u64::from_be_bytes(
        body.get(5..13)
            .ok_or(Error::InvalidSurrogate)?
            .try_into()
            .map_err(|_| Error::InvalidSurrogate)?,
    );
    let dest_len = usize::from(u16::from_be_bytes(
        body.get(13..15)
            .ok_or(Error::InvalidSurrogate)?
            .try_into()
            .map_err(|_| Error::InvalidSurrogate)?,
    ));

    // Each bound is checked separately (not `||`) so the
    // `dest_len` limit and the length-mismatch check are pinned
    // independently by tests.
    if dest_len > MAX_DESTINATION_LEN {
        return Err(Error::InvalidSurrogate);
    }
    if body.len() != 15 + dest_len {
        return Err(Error::InvalidSurrogate);
    }
    let destination =
        std::str::from_utf8(body.get(15..15 + dest_len).ok_or(Error::InvalidSurrogate)?)
            .map_err(|_| Error::InvalidSurrogate)?;
    if !is_valid_destination(destination) {
        return Err(Error::InvalidSurrogate);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if now > expiry {
        return Err(Error::ExpiredSurrogate);
    }

    Ok(ValidatedSurrogate {
        peer_uid,
        destination: destination.to_owned(),
        expiry,
    })
}

/// Checks that a destination is `host:port` with no scheme, path,
/// userinfo, or wildcard. Host must not be an IP literal (those bypass
/// DNS-based allowlisting; v0.1 requires hostnames).
pub(crate) fn is_valid_destination(dest: &str) -> bool {
    let Some((host, port)) = dest.rsplit_once(':') else {
        return false;
    };
    // Each emptiness check is separate (not `||`) so the host and
    // port bounds are pinned independently by tests.
    if host.is_empty() {
        return false;
    }
    if port.is_empty() {
        return false;
    }
    // No scheme, path, query, fragment, or userinfo.
    if dest.contains(['/', '?', '#', '@']) {
        return false;
    }
    // No wildcards.
    if host.contains('*') {
        return false;
    }
    // No IP literals (v4 or v6). v0.1 requires DNS hostnames so the
    // allowlist is meaningful.
    if host.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    // Port must be numeric and in range.
    let port_ok = match port.parse::<u16>() {
        Ok(0) | Err(_) => false,
        Ok(_) => true,
    };
    if !port_ok {
        return false;
    }
    // Hostname: dot-separated labels, each 1-63 chars, letters/digits/
    // hyphens, no leading or trailing hyphen per label. This rejects
    // "bad-.com" (label "bad-" ends with hyphen) as well as "-bad.com".
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let labels_ok = host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    });
    labels_ok && !is_numeric_host(host)
}

/// A host the system resolver reads as an IPv4 address, not a name, so
/// it would bypass DNS-based allowlisting like an IP literal: any form
/// `inet_aton` accepts (one to four dot-separated parts, each decimal,
/// octal with a leading `0`, or hex with `0x`: `2130706433`,
/// `0x7f000001`, `0177.0.0.1`, `127.1`), and any host whose last label
/// is all digits, which no real domain has. Called on well-formed labels
/// only, so no part is empty.
fn is_numeric_host(host: &str) -> bool {
    let all_digits = |part: &str| part.chars().all(|c| c.is_ascii_digit());
    let numeric_part = |part: &str| {
        part.strip_prefix("0x")
            .or_else(|| part.strip_prefix("0X"))
            .map_or_else(
                || all_digits(part),
                |hex| hex.chars().all(|c| c.is_ascii_hexdigit()),
            )
    };
    host.rsplit('.').next().is_some_and(all_digits)
        || (host.split('.').count() <= 4 && host.split('.').all(numeric_part))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MIN_PAYLOAD_LEN` is 1 + 4 + 8 + 2 + 32 = 47 bytes. Pins the
    /// constant against arithmetic mutants (`+` → `-`/`*`).
    #[test]
    fn min_payload_len_is_47() {
        assert_eq!(MIN_PAYLOAD_LEN, 47);
    }

    /// The key is the OS CSPRNG's: never all zero, and a different key
    /// on every call. (This can't prove unpredictability; it pins that
    /// the bytes are filled and fresh.)
    #[test]
    fn each_generated_key_is_fresh_randomness() {
        let keys: Vec<[u8; 32]> = (0..8)
            .map(|_| *SurrogateKey::generate().unwrap().0)
            .collect();
        for (i, key) in keys.iter().enumerate() {
            assert_ne!(key, &[0u8; 32], "key {i} is all zero");
            for other in keys.iter().skip(i + 1) {
                assert_ne!(key, other, "key {i} repeated");
            }
        }
    }

    #[test]
    fn mint_and_validate_round_trip() {
        let key = SurrogateKey::generate().unwrap();
        let surrogate = mint_surrogate(&key, 1000, "api.github.com:443", 3600).unwrap();
        let validated = validate_surrogate(&key, &surrogate).unwrap();
        assert_eq!(validated.peer_uid, 1000);
        assert_eq!(validated.destination, "api.github.com:443");
    }

    #[test]
    fn forged_surrogate_rejected() {
        let key = SurrogateKey::generate().unwrap();
        let other_key = SurrogateKey::generate().unwrap();
        let surrogate = mint_surrogate(&key, 1000, "api.github.com:443", 3600).unwrap();
        // Signed with a different key: HMAC verification fails.
        assert!(matches!(
            validate_surrogate(&other_key, &surrogate),
            Err(Error::InvalidSurrogate)
        ));
    }

    #[test]
    fn tampered_surrogate_rejected() {
        let key = SurrogateKey::generate().unwrap();
        let surrogate = mint_surrogate(&key, 1000, "api.github.com:443", 3600).unwrap();
        let mut payload = BASE64.decode(&surrogate).unwrap();
        // Flip a bit in the peer_uid field.
        if let Some(byte) = payload.get_mut(2) {
            *byte ^= 0x01;
        }
        let tampered = BASE64.encode(&payload);
        assert!(matches!(
            validate_surrogate(&key, &tampered),
            Err(Error::InvalidSurrogate)
        ));
    }

    #[test]
    fn expired_surrogate_rejected() {
        let key = SurrogateKey::generate().unwrap();
        // TTL of 0: expires immediately (or within the same second).
        let surrogate = mint_surrogate(&key, 1000, "api.github.com:443", 0).unwrap();
        // Sleep to ensure expiry. In practice the expiry check is
        // `now > expiry`, so a 0-TTL surrogate minted at second N is
        // invalid at second N+1. We sleep 1.1s to cross the boundary.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(matches!(
            validate_surrogate(&key, &surrogate),
            Err(Error::ExpiredSurrogate)
        ));
    }

    #[test]
    fn invalid_destinations_rejected_at_mint() {
        let key = SurrogateKey::generate().unwrap();
        for bad in [
            "",
            "not-a-destination",
            "host:abc",
            "host:0",
            "host:99999",
            "*.example.com:443",
            "192.168.1.1:443",
            "::1:443",
            "https://example.com:443",
            "example.com:443/path",
            "user@example.com:443",
            "example.com:443?query",
            "-bad.com:443",
            "bad-.com:443",
        ] {
            assert!(
                mint_surrogate(&key, 1000, bad, 3600).is_err(),
                "should reject bad destination"
            );
        }
    }

    #[test]
    fn numeric_host_forms_are_refused_like_ip_literals() {
        // Each of these resolves to an IPv4 address through the system
        // resolver (inet_aton), so it would reach an address no name
        // in the allowlist names.
        for numeric in [
            "2130706433:443",       // 127.0.0.1 as one decimal number
            "0x7f000001:443",       // hex
            "0X7F000001:443",       // hex, upper-case prefix
            "0177.0.0.1:443",       // octal first part
            "127.1:443",            // two parts
            "127.0.1:443",          // three parts
            "0x7f.0x1:443",         // two hex parts: the last label isn't all digits
            "0x7f.0x0.0x0.0x1:443", // four hex parts
            "0x:443",               // inet_aton reads a bare "0x" as 0
            "0:443",
            "example.123:443", // an all-digit last label: no real domain has one
        ] {
            assert!(!is_valid_destination(numeric), "{numeric} must be refused");
            let host = numeric.rsplit_once(':').map_or(numeric, |(host, _)| host);
            assert!(is_numeric_host(host), "{host} is numeric");
        }
        // Names that only look numeric are still names.
        for name in [
            "123.example.com:443",
            "3com.com:443",
            "0x7f.example:443",
            "1password.com:443",
            "0x1.0x2.0x3.0x4.0x5:443", // five parts: inet_aton reads no five-part address
        ] {
            assert!(is_valid_destination(name), "{name} must be accepted");
            let host = name.rsplit_once(':').map_or(name, |(host, _)| host);
            assert!(!is_numeric_host(host), "{host} is a name");
        }
    }

    #[test]
    fn valid_destinations_accepted_at_mint() {
        let key = SurrogateKey::generate().unwrap();
        for good in [
            "api.github.com:443",
            "example.com:80",
            "sub.domain.example.org:8443",
            "a-b.c-d.example:1234",
        ] {
            assert!(
                mint_surrogate(&key, 1000, good, 3600).is_ok(),
                "should accept good destination"
            );
        }
    }

    #[test]
    fn malformed_surrogate_bytes_rejected() {
        let key = SurrogateKey::generate().unwrap();
        for bad in ["", "not-base64!!!", "e30=", "AQ=="] {
            assert!(
                validate_surrogate(&key, bad).is_err(),
                "should reject malformed surrogate"
            );
        }
    }

    /// The maximum destination length is exactly 253 (max hostname) +
    /// 1 (':') + 5 (max port digits) = 259 bytes. A destination of
    /// exactly 259 bytes must be accepted; 260 must be rejected. This
    /// pins the `MAX_DESTINATION_LEN` arithmetic against `+` → `-`/`*`
    /// mutants.
    #[test]
    fn max_destination_length_boundary() {
        let key = SurrogateKey::generate().unwrap();
        // 253-char hostname (4 labels of 63 + dots = 63*4+3 = 255, so
        // use a single 63-char label repeated: 63 + 1 + 63 + 1 + 63 + 1
        // + 60 = 252... simpler: one long label won't work (max 63 per
        // label). Build 253 chars as 63.63.63.61 (63+1+63+1+63+1+61).
        let host_253 = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61)
        );
        assert_eq!(host_253.len(), 253);
        // 253 + ':' + 5-digit port = 259 = MAX_DESTINATION_LEN.
        let dest_259 = format!("{host_253}:12345");
        assert_eq!(dest_259.len(), 259);
        assert!(
            mint_surrogate(&key, 1000, &dest_259, 3600).is_ok(),
            "259-byte destination must be accepted"
        );
        // 260 bytes: one more char in the port (invalid port) or host.
        // Use a 253-char host with a 6-char port suffix that keeps the
        // host valid but exceeds the length bound.
        let dest_260 = format!("{host_253}:123456");
        assert_eq!(dest_260.len(), 260);
        assert!(
            mint_surrogate(&key, 1000, &dest_260, 3600).is_err(),
            "260-byte destination must be rejected"
        );
    }

    /// `mint_surrogate` rejects empty destinations and destinations
    /// longer than `MAX_DESTINATION_LEN`. The `>` vs `==`/`>=` and
    /// `||` vs `&&` mutants change which inputs are rejected.
    #[test]
    fn mint_rejects_empty_and_too_long() {
        let key = SurrogateKey::generate().unwrap();
        // Empty: `is_empty()` is true, `len() > MAX` is false. With
        // `||` this is rejected; with `&&` it would proceed (and then
        // fail in `is_valid_destination`, but the early return is the
        // pinned behavior).
        assert!(mint_surrogate(&key, 1000, "", 3600).is_err());
        // Too long: 300 'a's (no valid host:port structure, but the
        // length check fires first).
        let too_long = "a".repeat(300);
        assert!(mint_surrogate(&key, 1000, &too_long, 3600).is_err());
    }

    /// `validate_surrogate` rejects payloads shorter than the minimum
    /// (1 + 4 + 8 + 2 + 32 = 47 bytes). A 46-byte payload must be
    /// rejected; this pins the `<` and the `+` arithmetic.
    #[test]
    fn validate_rejects_short_payload() {
        let key = SurrogateKey::generate().unwrap();
        // 46 zero bytes: one short of the 47-byte minimum.
        let short = BASE64.encode(vec![0u8; 46]);
        assert!(matches!(
            validate_surrogate(&key, &short),
            Err(Error::InvalidSurrogate)
        ));
    }

    /// A payload of exactly `MIN_PAYLOAD_LEN` (47 bytes) is still
    /// rejected — just at the destination gate, not the length gate.
    /// With a 15-byte body, `dest_len` must be 0, and the empty
    /// destination fails `is_valid_destination`. Hand-crafted with a
    /// valid HMAC so the length check is the only thing not firing.
    /// This pins the equivalence proof for the `<` -> `<=` mutant at
    /// the length check (excluded in `.cargo/mutants.toml`): both
    /// formulations reject this payload with `InvalidSurrogate`.
    #[test]
    fn validate_rejects_exact_min_payload() {
        use hmac::Mac;
        let key = SurrogateKey::generate().unwrap();
        // 15-byte body: version + uid + expiry + dest_len(0).
        let mut body = Vec::new();
        body.push(SURROGATE_VERSION);
        body.extend_from_slice(&1000u32.to_be_bytes());
        body.extend_from_slice(&u64::MAX.to_be_bytes()); // expiry far future
        body.extend_from_slice(&0u16.to_be_bytes()); // dest_len = 0
        assert_eq!(body.len(), 15);
        let mut mac = HmacSha256::new_from_slice(key.0.as_slice()).unwrap();
        mac.update(&body);
        let tag = mac.finalize().into_bytes();
        let mut payload = body;
        payload.extend_from_slice(&tag);
        assert_eq!(payload.len(), MIN_PAYLOAD_LEN);
        let surrogate = BASE64.encode(&payload);
        // Rejected: empty destination, not short payload.
        assert!(matches!(
            validate_surrogate(&key, &surrogate),
            Err(Error::InvalidSurrogate)
        ));
    }

    /// `validate_surrogate` rejects a surrogate whose declared
    /// destination length exceeds `MAX_DESTINATION_LEN`, and one whose
    /// body length does not match `15 + dest_len`. Crafted by hand
    /// (mint refuses invalid destinations).
    #[test]
    fn validate_rejects_bad_dest_len() {
        use hmac::Mac;
        let key = SurrogateKey::generate().unwrap();
        // Build a body with dest_len = MAX_DESTINATION_LEN + 1.
        let dest_len = u16::try_from(MAX_DESTINATION_LEN + 1).unwrap();
        let mut body = Vec::new();
        body.push(SURROGATE_VERSION);
        body.extend_from_slice(&1000u32.to_be_bytes());
        // Expiry far in the future.
        body.extend_from_slice(&u64::MAX.to_be_bytes());
        body.extend_from_slice(&dest_len.to_be_bytes());
        // Destination bytes (must match dest_len for the length check
        // to be the thing that fires; use valid chars).
        body.extend(vec![b'a'; usize::from(dest_len)]);
        let mut mac = HmacSha256::new_from_slice(key.0.as_slice()).unwrap();
        mac.update(&body);
        let tag = mac.finalize().into_bytes();
        let mut payload = body;
        payload.extend_from_slice(&tag);
        let surrogate = BASE64.encode(&payload);
        // `dest_len > MAX_DESTINATION_LEN` fires.
        assert!(matches!(
            validate_surrogate(&key, &surrogate),
            Err(Error::InvalidSurrogate)
        ));
    }

    /// `validate_surrogate` rejects when `body.len() != 15 + dest_len`.
    /// Here `dest_len` says 10 but the body holds 5 destination bytes.
    #[test]
    fn validate_rejects_dest_len_mismatch() {
        use hmac::Mac;
        let key = SurrogateKey::generate().unwrap();
        let mut body = Vec::new();
        body.push(SURROGATE_VERSION);
        body.extend_from_slice(&1000u32.to_be_bytes());
        body.extend_from_slice(&u64::MAX.to_be_bytes());
        body.extend_from_slice(&10u16.to_be_bytes()); // claims 10
        body.extend(b"abcde"); // only 5
        let mut mac = HmacSha256::new_from_slice(key.0.as_slice()).unwrap();
        mac.update(&body);
        let tag = mac.finalize().into_bytes();
        let mut payload = body;
        payload.extend_from_slice(&tag);
        let surrogate = BASE64.encode(&payload);
        assert!(matches!(
            validate_surrogate(&key, &surrogate),
            Err(Error::InvalidSurrogate)
        ));
    }

    /// The expiry check is `now > expiry` (strictly greater). A
    /// surrogate expiring exactly now is still valid; one second past
    /// is not. This pins `>` against `>=`.
    #[test]
    fn expiry_boundary_is_strict() {
        use hmac::Mac;
        let key = SurrogateKey::generate().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // Mint manually with expiry == now: `now > expiry` is false,
        // so it must validate. (With `>=` it would be rejected.)
        let mut body = Vec::new();
        body.push(SURROGATE_VERSION);
        body.extend_from_slice(&1000u32.to_be_bytes());
        body.extend_from_slice(&now.to_be_bytes());
        let dest = b"example.com:443";
        let dest_len = u16::try_from(dest.len()).unwrap();
        body.extend_from_slice(&dest_len.to_be_bytes());
        body.extend_from_slice(dest);
        let mut mac = HmacSha256::new_from_slice(key.0.as_slice()).unwrap();
        mac.update(&body);
        let tag = mac.finalize().into_bytes();
        let mut payload = body;
        payload.extend_from_slice(&tag);
        let surrogate = BASE64.encode(&payload);
        assert!(
            validate_surrogate(&key, &surrogate).is_ok(),
            "surrogate expiring exactly now must still be valid"
        );
    }

    /// `is_valid_destination` rejects empty host or empty port. The
    /// `||` vs `&&` mutants would accept `:443` or `example.com:`.
    #[test]
    fn destination_empty_host_or_port_rejected() {
        assert!(!is_valid_destination(":443"));
        assert!(!is_valid_destination("example.com:"));
        assert!(!is_valid_destination(":"));
    }

    /// `is_valid_destination` rejects hostnames longer than 253 chars.
    /// A 253-char hostname is accepted; 254 is not. Pins `>` vs
    /// `==`/`>=` and the `||` vs `&&` on the emptiness check.
    #[test]
    fn destination_host_length_boundary() {
        // 253-char valid hostname (labels ≤ 63).
        let host_253 = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61)
        );
        assert_eq!(host_253.len(), 253);
        let dest_253 = format!("{host_253}:443");
        assert!(is_valid_destination(&dest_253));
        // 254 chars: add one more char to the last label (62 chars,
        // still ≤ 63 per label, but total > 253).
        let host_254 = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(62)
        );
        assert_eq!(host_254.len(), 254);
        let dest_254 = format!("{host_254}:443");
        assert!(!is_valid_destination(&dest_254));
    }

    /// `validate_surrogate` accepts a surrogate with `dest_len` exactly
    /// equal to `MAX_DESTINATION_LEN` (259). The `>` vs `==`/`>=`
    /// mutants on the `dest_len > MAX_DESTINATION_LEN` check would
    /// reject it. Uses the 259-byte destination from
    /// `max_destination_length_boundary`.
    #[test]
    fn validate_accepts_max_dest_len() {
        let key = SurrogateKey::generate().unwrap();
        let host_253 = format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61)
        );
        let dest_259 = format!("{host_253}:12345");
        assert_eq!(dest_259.len(), MAX_DESTINATION_LEN);
        // Mint via the real path (259 bytes is valid).
        let surrogate = mint_surrogate(&key, 1000, &dest_259, 3600).unwrap();
        // Validate: `dest_len > MAX` is false (259 > 259 is false), so
        // it must pass. With `==` or `>=`, it would be rejected.
        let validated = validate_surrogate(&key, &surrogate).unwrap();
        assert_eq!(validated.destination, dest_259);
    }
}
