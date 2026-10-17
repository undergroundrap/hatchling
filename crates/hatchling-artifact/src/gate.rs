//! The share-review safety gate.
//!
//! Before any artifact bytes leave, the broker judges the **final bytes**
//! (never the builder's summary) against:
//!
//! 1. **Every secret the broker holds for the session.** A secret is
//!    found raw, as hex, or as base64 at any alignment, so a builder
//!    that encodes a secret before embedding it is still caught. Other
//!    transforms are the review policy's to catch.
//! 2. **The configured review policy:** a size bound and denied
//!    patterns.
//!
//! The gate is fail-closed by construction. If the secrets could not be
//! gathered, or no policy is configured, the verdict is
//! [`Verdict::Error`], and delivery treats an error exactly as a block.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// The secrets to look for, in every form the gate checks. Each form is
/// wiped when the set is dropped.
#[derive(Clone, Default)]
pub struct SecretSet {
    needles: Vec<Zeroizing<Vec<u8>>>,
}

impl std::fmt::Debug for SecretSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the needles: they are the secrets.
        f.debug_struct("SecretSet")
            .field("forms", &self.needles.len())
            .finish()
    }
}

impl SecretSet {
    /// The forms of `secrets` the gate looks for. An empty secret is
    /// skipped: it matches everything and protects nothing. Each secret
    /// and each form is wiped when the set is dropped, and each is made at
    /// its final size, so no copy is left behind in a freed buffer.
    #[must_use]
    pub fn new<I>(secrets: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Zeroizing<Vec<u8>>>,
    {
        let mut needles = Vec::new();
        for secret in secrets {
            let secret: Zeroizing<Vec<u8>> = secret.into();
            if secret.is_empty() {
                continue;
            }
            let hex = Zeroizing::new(crate::request::hex(&secret));
            needles.push(Zeroizing::new(hex.to_ascii_uppercase().into_bytes()));
            needles.push(Zeroizing::new(hex.as_bytes().to_vec()));
            // Base64 of a stream containing the secret contains the base64
            // of the secret's whole 3-byte groups, at one of three
            // alignments. Only fully determined characters are kept.
            for skip in 0..3 {
                let groups = secret.get(skip..).unwrap_or_default();
                let whole = groups.len() / 3 * 3;
                let aligned = groups.get(..whole).unwrap_or_default();
                if aligned.is_empty() {
                    continue;
                }
                needles.push(Zeroizing::new(STANDARD_NO_PAD.encode(aligned).into_bytes()));
                needles.push(Zeroizing::new(URL_SAFE_NO_PAD.encode(aligned).into_bytes()));
            }
            needles.push(secret);
        }
        Self { needles }
    }

    /// Whether `bytes` contains any held secret, in any checked form.
    #[must_use]
    pub fn exposed_in(&self, bytes: &[u8]) -> bool {
        self.needles.iter().any(|needle| {
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_slice())
        })
    }
}

/// The operator's review policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPolicy {
    /// The largest artifact that may leave, in bytes.
    pub max_bytes: usize,
    /// Substrings an artifact may not contain.
    pub deny: Vec<String>,
}

/// The gate's verdict on one artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict")]
pub enum Verdict {
    /// The bytes may leave.
    #[serde(rename = "allow")]
    Allow,
    /// The bytes may not leave.
    #[serde(rename = "block")]
    Block {
        /// Why. Never quotes the bytes or a secret.
        reason: String,
    },
    /// The gate could not judge, so the bytes may not leave either.
    #[serde(rename = "error")]
    Error {
        /// Why.
        reason: String,
    },
}

/// Judges an artifact's final bytes.
pub trait Reviewer {
    /// The verdict on `bytes`.
    fn review(&self, bytes: &[u8]) -> Verdict;
}

/// The share-review gate: held secrets, then the policy.
#[derive(Debug, Clone)]
pub struct ShareReview {
    secrets: Result<SecretSet, String>,
    policy: Option<ReviewPolicy>,
}

impl ShareReview {
    /// A gate over `secrets` (or the reason they could not be gathered)
    /// and `policy` (if one is configured).
    #[must_use]
    pub const fn new(secrets: Result<SecretSet, String>, policy: Option<ReviewPolicy>) -> Self {
        Self { secrets, policy }
    }
}

impl Reviewer for ShareReview {
    fn review(&self, bytes: &[u8]) -> Verdict {
        let secrets = match &self.secrets {
            Ok(secrets) => secrets,
            Err(reason) => {
                return Verdict::Error {
                    reason: format!("the held secrets could not be gathered: {reason}"),
                };
            }
        };
        let Some(policy) = &self.policy else {
            return Verdict::Error {
                reason: String::from("no review policy is configured"),
            };
        };
        if secrets.exposed_in(bytes) {
            return Verdict::Block {
                reason: String::from("the artifact contains a secret the broker holds"),
            };
        }
        if bytes.len() > policy.max_bytes {
            return Verdict::Block {
                reason: format!("the artifact is over {} bytes", policy.max_bytes),
            };
        }
        if policy
            .deny
            .iter()
            .filter(|pattern| !pattern.is_empty())
            .any(|pattern| {
                bytes
                    .windows(pattern.len())
                    .any(|w| w == pattern.as_bytes())
            })
        {
            return Verdict::Block {
                reason: String::from("the artifact matches a denied pattern"),
            };
        }
        Verdict::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE};

    const SECRET: &[u8] = b"sk-live-0123456789abcdef";

    fn policy() -> ReviewPolicy {
        ReviewPolicy {
            max_bytes: 1024,
            deny: vec![String::from("CONFIDENTIAL")],
        }
    }

    fn gate() -> ShareReview {
        ShareReview::new(Ok(SecretSet::new([SECRET.to_vec()])), Some(policy()))
    }

    fn blocked(bytes: &[u8]) -> bool {
        matches!(gate().review(bytes), Verdict::Block { .. })
    }

    #[test]
    fn clean_bytes_pass() {
        assert_eq!(
            gate().review(b"a perfectly ordinary document"),
            Verdict::Allow
        );
        assert_eq!(gate().review(b""), Verdict::Allow);
    }

    #[test]
    fn a_held_secret_is_found_raw_anywhere() {
        let mut bytes = b"intro ".to_vec();
        bytes.extend_from_slice(SECRET);
        bytes.extend_from_slice(b" outro");
        assert!(blocked(&bytes));
        assert!(blocked(SECRET));
        // One byte short of the secret is not the secret.
        assert!(!blocked(SECRET.get(1..).unwrap()));
    }

    #[test]
    fn a_held_secret_is_found_hex_encoded() {
        let hex = crate::request::hex(SECRET);
        let embedded = format!("key={hex};");
        assert!(blocked(embedded.as_bytes()));
        assert!(blocked(hex.to_ascii_uppercase().as_bytes()));
    }

    #[test]
    fn a_held_secret_is_found_base64_encoded_at_every_alignment() {
        for prefix in ["", "a", "ab", "abc"] {
            let mut stream = prefix.as_bytes().to_vec();
            stream.extend_from_slice(SECRET);
            stream.extend_from_slice(b"!");
            for encoded in [STANDARD.encode(&stream), URL_SAFE.encode(&stream)] {
                assert!(blocked(encoded.as_bytes()), "prefix {prefix:?}: {encoded}");
            }
        }
    }

    #[test]
    fn a_secret_too_short_for_a_base64_group_is_still_found_raw_and_hex() {
        let short = SecretSet::new([b"ab".to_vec()]);
        assert!(short.exposed_in(b"xxabxx"));
        assert!(short.exposed_in(b"6162"));
        assert!(!short.exposed_in(b"a b"));
    }

    #[test]
    fn empty_secrets_are_ignored_and_the_set_is_not_debug_printed() {
        let set = SecretSet::new([Vec::new()]);
        assert!(!set.exposed_in(b"anything"));
        assert!(!SecretSet::default().exposed_in(b"anything"));
        let shown = format!("{:?}", SecretSet::new([SECRET.to_vec()]));
        assert!(!shown.contains("sk-live"), "{shown}");
        assert!(shown.starts_with("SecretSet { forms: "), "{shown}");
    }

    #[test]
    fn the_policy_bounds_size_and_denies_patterns() {
        assert_eq!(gate().review(&vec![b'x'; 1024]), Verdict::Allow);
        assert!(blocked(&vec![b'x'; 1025]));
        assert!(blocked(b"this is CONFIDENTIAL material"));
        // An empty pattern denies nothing.
        let lax = ShareReview::new(
            Ok(SecretSet::default()),
            Some(ReviewPolicy {
                max_bytes: 10,
                deny: vec![String::new()],
            }),
        );
        assert_eq!(lax.review(b"fine"), Verdict::Allow);
    }

    #[test]
    fn the_gate_fails_closed() {
        let unjudged = ShareReview::new(Err(String::from("vault unreadable")), Some(policy()));
        assert!(
            matches!(unjudged.review(b"clean"), Verdict::Error { reason } if reason.contains("vault unreadable"))
        );
        let no_policy = ShareReview::new(Ok(SecretSet::default()), None);
        assert_eq!(
            no_policy.review(b"clean"),
            Verdict::Error {
                reason: String::from("no review policy is configured")
            }
        );
        // A secret is a block even when the policy would pass it.
        assert!(
            matches!(gate().review(SECRET), Verdict::Block { reason } if reason.contains("secret"))
        );
    }

    #[test]
    fn verdicts_round_trip_as_stored() {
        for verdict in [
            Verdict::Allow,
            Verdict::Block {
                reason: String::from("r"),
            },
            Verdict::Error {
                reason: String::from("e"),
            },
        ] {
            let json = serde_json::to_string(&verdict).unwrap();
            assert_eq!(serde_json::from_str::<Verdict>(&json).unwrap(), verdict);
        }
        assert_eq!(
            serde_json::to_string(&Verdict::Allow).unwrap(),
            r#"{"verdict":"allow"}"#
        );
    }
}
