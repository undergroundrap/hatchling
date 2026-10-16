//! Memory item types: ids, scopes, retention, and the item itself.
//!
//! All names ([`MemoryId`], [`ProjectName`], [`Tag`], [`SessionId`])
//! share one validation rule: 1–128 bytes of ASCII alphanumeric, `-`,
//! or `_`. The alphabet contains no path separator, so traversal
//! (`../`), absolute paths, and NUL bytes are unrepresentable: they are
//! rejected at parse, before any filesystem touch. Both ends of the
//! protocol validate the same rule.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Error;

/// Maximum content bytes per memory item. Matches the vault's secret
/// bound: one rule for broker-mediated small blobs.
pub const MAX_CONTENT_BYTES: usize = 32 * 1024;

/// Maximum size of a stored item as it travels in a session reply, in
/// bytes: the item's JSON, re-encoded as a JSON string.
///
/// Every reply carries an item that way (`item_json`), so this — not the
/// raw content length — decides whether an item fits a frame. JSON
/// escaping expands content twice on that path: a plain byte costs 1, a
/// `"` or `\` costs 4 (`\"` → `\\\"`), a control character costs 7
/// (`\u0001` → `\\u0001`). At [`MAX_CONTENT_BYTES`] that is up to
/// 7 × 32 KiB = 224 KiB, far past the 64 KiB frame (`hatchling-core`'s
/// `MAX_FRAME_BYTES`), so the raw bound alone cannot hold the invariant.
/// The store measures the exact encoded size ([`reply_encoded_len`])
/// and refuses anything above 56 KiB, which leaves 8 KiB of every frame
/// for the reply envelope around one item.
pub const MAX_ENCODED_ITEM_BYTES: usize = 56 * 1024;

/// The size of `json` once re-encoded as a JSON string: how an item or
/// a context entry travels inside every session reply.
///
/// # Errors
///
/// Returns [`Error::Corrupt`] if `json` is not UTF-8.
pub fn reply_encoded_len(json: &[u8]) -> Result<usize, Error> {
    let text = std::str::from_utf8(json).map_err(|_| Error::Corrupt)?;
    serde_json::to_string(text)
        .map(|encoded| encoded.len())
        .map_err(|_| Error::Corrupt)
}

/// Maximum name length, in bytes.
const MAX_NAME_LEN: usize = 128;
/// Maximum tag length, in bytes.
const MAX_TAG_LEN: usize = 64;

/// Validates the shared name alphabet. Factored once so the four name
/// types cannot drift apart.
fn valid_name(raw: &str, max_len: usize) -> bool {
    if raw.is_empty() || raw.len() > max_len {
        return false;
    }
    raw.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

macro_rules! define_name {
    ($(#[$meta:meta])* $name:ident, $max:expr, $what:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String")]
        pub struct $name(String);

        // Read back from the store, a name is checked as one given is.
        impl TryFrom<String> for $name {
            type Error = Error;

            fn try_from(raw: String) -> Result<Self, Self::Error> {
                raw.parse()
            }
        }

        impl $name {
            /// The validated name, for joining onto the store root.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                if valid_name(raw, $max) {
                    Ok(Self(raw.to_owned()))
                } else {
                    Err(Error::InvalidName(format!(
                        concat!($what, " must be 1-{} bytes of [A-Za-z0-9_-], got {:?}"),
                        $max, raw
                    )))
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

define_name!(
    /// Identifies one memory item within its scope namespace.
    MemoryId,
    MAX_NAME_LEN,
    "memory id"
);
define_name!(
    /// Names a project. The agent supplies the name; the broker binds
    /// it to the authenticated peer identity, so the name alone never
    /// decides visibility.
    ProjectName,
    MAX_NAME_LEN,
    "project name"
);
define_name!(
    /// A retrieval tag on a memory item. Tags are scoped like items:
    /// tag recall never crosses a scope boundary.
    Tag,
    MAX_TAG_LEN,
    "tag"
);
define_name!(
    /// Broker-assigned session identity. Minted per connection by the
    /// broker; the cell side cannot choose or forge one.
    SessionId,
    MAX_NAME_LEN,
    "session id"
);

impl SessionId {
    /// Mints a fresh session id from 16 random bytes, hex-encoded.
    ///
    /// The broker calls this once per connection; the id binds the
    /// session scope and the context log for that connection's
    /// lifetime.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the system RNG cannot be read. The
    /// caller fails the session — no session without an id.
    pub fn generate() -> Result<Self, Error> {
        use std::io::Read as _;
        let mut bytes = [0u8; 16];
        // /dev/urandom is the session-id source: non-blocking, always
        // available on the broker host.
        let mut urandom =
            std::fs::File::open("/dev/urandom").map_err(|source| Error::Io { source })?;
        urandom
            .read_exact(&mut bytes)
            .map_err(|source| Error::Io { source })?;
        // Hex-encode without `format!`: the lint set denies both
        // `format!`-in-`collect` and `expect`, so build the string
        // from nibbles directly.
        let mut hex = String::with_capacity(32);
        for b in bytes {
            hex.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
            hex.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
        }
        Ok(Self(hex))
    }
}

/// The scope a memory item lives in, as requested by the agent.
///
/// The broker resolves this against the [`MemoryPrincipal`]: `User`
/// binds to the peer uid, `Project` binds `(peer uid, name)`, and
/// `Session` binds to the broker-assigned session id. The agent's
/// scope request is a wish; the binding is the authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryScope {
    /// The user's own memory: preferences, facts, standing context.
    User,
    /// Project memory, bound to (peer, name) by the broker.
    Project(ProjectName),
    /// Ephemeral session memory, bound to the broker's session id.
    Session,
}

/// Retention policy for one memory item.
///
/// There is deliberately no `Default` impl: every write names its
/// policy, so "keep forever" is always an explicit choice and silent
/// infinite retention is unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetentionPolicy {
    /// Expire `ttl_secs` after creation. The item stops being recalled
    /// once expired and is purged by
    /// [`MemoryStore::purge_expired`](crate::MemoryStore::purge_expired).
    Ttl {
        /// Seconds from `created_at_secs` until expiry.
        ttl_secs: u64,
    },
    /// Explicit keep-forever. Must be chosen deliberately at write
    /// time; never a default.
    Keep,
}

/// Identity the broker asserts for a memory operation.
///
/// Never supplied by the agent: the broker derives `peer_uid` from
/// `SO_PEERCRED` and mints `session_id` per connection. Scope
/// resolution binds every scope through this principal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryPrincipal {
    /// The authenticated peer uid, from kernel credentials.
    pub peer_uid: u32,
    /// The broker-assigned session id for this connection.
    pub session_id: SessionId,
}

/// One stored memory item.
///
/// `created_at_secs` / `updated_at_secs` are unix epoch seconds,
/// stamped by the broker (host clock), never by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryItem {
    /// Item identity within its scope namespace.
    pub id: MemoryId,
    /// The scope this item was written to.
    pub scope: MemoryScope,
    /// The remembered content. Treated as data, never instructions.
    pub content: String,
    /// Retrieval tags.
    pub tags: Vec<Tag>,
    /// Unix epoch seconds, broker-stamped at write.
    pub created_at_secs: u64,
    /// Unix epoch seconds, broker-stamped at last write.
    pub updated_at_secs: u64,
    /// The item's retention policy, fixed at write time.
    pub retention: RetentionPolicy,
}

/// The unix epoch second at which `item` expires, or `None` for
/// [`RetentionPolicy::Keep`].
///
/// Saturating: a TTL that would overflow the epoch wraps to
/// `u64::MAX`, i.e. effectively never — it cannot wrap around to an
/// already-expired past.
#[must_use]
pub const fn expires_at(item: &MemoryItem) -> Option<u64> {
    match item.retention {
        RetentionPolicy::Keep => None,
        RetentionPolicy::Ttl { ttl_secs } => Some(item.created_at_secs.saturating_add(ttl_secs)),
    }
}

/// Whether `item` is expired at `now_secs`.
///
/// Boundary: an item expires when `now_secs >= expires_at`, so a
/// zero TTL expires immediately and an item is gone exactly at its
/// deadline — never one second early, never one late.
#[must_use]
pub fn is_expired(item: &MemoryItem, now_secs: u64) -> bool {
    expires_at(item).is_some_and(|deadline| now_secs >= deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_with_retention(retention: RetentionPolicy, created: u64) -> MemoryItem {
        MemoryItem {
            id: "fact-1".parse().unwrap(),
            scope: MemoryScope::User,
            content: String::from("the user prefers tea"),
            tags: Vec::new(),
            created_at_secs: created,
            updated_at_secs: created,
            retention,
        }
    }

    #[test]
    fn names_read_back_are_checked_as_names_given_are() {
        let read: MemoryId = serde_json::from_str("\"note-1\"").unwrap();
        assert_eq!(read.as_str(), "note-1");
        for bad in ["\"../x\"", "\"a\\nb\"", "\"a\\u0000b\"", "\"\""] {
            assert!(serde_json::from_str::<MemoryId>(bad).is_err(), "{bad}");
            assert!(serde_json::from_str::<ProjectName>(bad).is_err(), "{bad}");
            assert!(serde_json::from_str::<Tag>(bad).is_err(), "{bad}");
        }
        assert_eq!(
            MemoryId::try_from(String::from("ok")).unwrap().as_str(),
            "ok"
        );
    }

    #[test]
    fn names_reject_traversal_and_separators() {
        for bad in ["", "../x", "a/b", "a b", "a.b", "x".repeat(129).as_str()] {
            assert!(
                bad.parse::<MemoryId>().is_err(),
                "MemoryId accepted {bad:?}"
            );
            assert!(
                bad.parse::<ProjectName>().is_err(),
                "ProjectName accepted {bad:?}"
            );
            assert!(
                bad.parse::<SessionId>().is_err(),
                "SessionId accepted {bad:?}"
            );
        }
        assert!("ok-name_1".parse::<MemoryId>().is_ok());
        assert!("proj".parse::<ProjectName>().is_ok());
    }

    #[test]
    fn tag_length_bound_is_independent() {
        // Tags cap at 64: a 65-byte tag fails while a 65-byte id passes.
        let long = "x".repeat(65);
        assert!(long.parse::<Tag>().is_err());
        assert!(long.parse::<MemoryId>().is_ok());
        assert!("x".repeat(64).parse::<Tag>().is_ok());
    }

    #[test]
    fn keep_never_expires() {
        let item = item_with_retention(RetentionPolicy::Keep, 1_000);
        assert_eq!(expires_at(&item), None);
        assert!(!is_expired(&item, u64::MAX));
    }

    #[test]
    fn ttl_expires_exactly_at_deadline() {
        // Boundary pins: `>=` on the comparison. `>` would keep the
        // item alive one second past its deadline; `<=` would kill it
        // one second early.
        let item = item_with_retention(RetentionPolicy::Ttl { ttl_secs: 100 }, 1_000);
        assert_eq!(expires_at(&item), Some(1_100));
        assert!(!is_expired(&item, 1_099));
        assert!(is_expired(&item, 1_100));
        assert!(is_expired(&item, 1_101));
    }

    #[test]
    fn zero_ttl_expires_immediately() {
        let item = item_with_retention(RetentionPolicy::Ttl { ttl_secs: 0 }, 1_000);
        assert!(is_expired(&item, 1_000));
    }

    #[test]
    fn ttl_overflow_saturates_instead_of_wrapping() {
        // A TTL near u64::MAX must not wrap to the past: saturation
        // keeps the item alive rather than expiring it instantly.
        let item = item_with_retention(RetentionPolicy::Ttl { ttl_secs: u64::MAX }, 1_000);
        assert_eq!(expires_at(&item), Some(u64::MAX));
        assert!(!is_expired(&item, u64::MAX - 1));
    }

    #[test]
    fn retention_policy_variants_are_exhaustive() {
        // Change-detector: adding a retention variant (especially a
        // silent-forever one) must update this test deliberately.
        let ttl = RetentionPolicy::Ttl { ttl_secs: 60 };
        let keep = RetentionPolicy::Keep;
        assert!(matches!(ttl, RetentionPolicy::Ttl { ttl_secs: 60 }));
        assert!(matches!(keep, RetentionPolicy::Keep));
    }

    #[test]
    fn content_bound_is_32_kib() {
        assert_eq!(MAX_CONTENT_BYTES, 32 * 1024);
    }

    #[test]
    fn generated_session_ids_are_random_lowercase_hex() {
        // 16 random bytes as 32 lowercase hex digits. Over 16 ids (256
        // bytes), each nibble position shows '0' about 16 times; a broken
        // nibble split (a wrong shift or mask) turns nearly every digit in
        // that position into '0' (well over 200). The 128 threshold is
        // astronomically far from both, so this never flakes.
        let mut high_zeros = 0;
        let mut low_zeros = 0;
        for _ in 0..16 {
            let id = SessionId::generate().unwrap();
            let hex = id.as_str();
            assert_eq!(hex.len(), 32, "{hex}");
            assert!(
                hex.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{hex}"
            );
            high_zeros += hex.bytes().step_by(2).filter(|&c| c == b'0').count();
            low_zeros += hex
                .bytes()
                .skip(1)
                .step_by(2)
                .filter(|&c| c == b'0')
                .count();
        }
        assert!(
            high_zeros < 128,
            "high nibbles degenerate: {high_zeros} zeros of 256"
        );
        assert!(
            low_zeros < 128,
            "low nibbles degenerate: {low_zeros} zeros of 256"
        );
    }
}
