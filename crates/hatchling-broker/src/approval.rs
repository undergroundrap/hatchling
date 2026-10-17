//! Broker-minted rail approvals: the token primitive the confirmation
//! rail issues.
//!
//! An approval authorizes exactly one outbound channel message — the
//! channel, the account (for a credentialed channel), and the
//! exact text bytes it was minted for — once, before it expires. Only the broker mints: no session op does, so a cell can
//! present a token but never make one, and a string the broker did not
//! mint matches nothing.
//!
//! Lifecycle: **mint → present → verify → expire.**
//!
//! - *Mint* ([`ApprovalLedger::mint`]): 32 bytes from the system RNG,
//!   hex-encoded behind [`TOKEN_PREFIX`], recorded with its channel,
//!   its account, its text, and an expiry [`APPROVAL_TTL_SECS`] after minting.
//! - *Present*: the cell names the token in `ChannelOp::Send.approval`.
//! - *Verify* ([`ApprovalLedger::verify`]): the broker removes the
//!   token in the same step it looks it up. Any presentation consumes
//!   it, match or not: a token can be tried once, and trying it on the
//!   wrong message burns it.
//! - *Expire*: at or past its expiry a token verifies as
//!   [`Verdict::Expired`]; expired tokens are also dropped whenever a
//!   new one is minted, and at most [`MAX_PENDING_APPROVALS`] wait at
//!   once.
//!
//! The ledger lives in a session's channel registry: an approval is
//! good only on the session it was minted for.

use std::collections::HashMap;

/// Seconds an approval stays valid after minting.
pub const APPROVAL_TTL_SECS: u64 = 300;
/// Most approvals one session may hold unspent.
pub const MAX_PENDING_APPROVALS: usize = 64;
/// Every token starts with this, versioning the format.
pub const TOKEN_PREFIX: &str = "rail-v1-";
/// Random bytes behind the prefix.
const TOKEN_BYTES: usize = 32;

/// A broker-minted approval, as handed to whoever approved the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalToken(String);

impl ApprovalToken {
    /// The token as the cell will present it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What verifying a presented token found. The cell only ever learns
/// `Denied`; the variants are for the broker's logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Minted for exactly this channel, account, and text, unexpired,
    /// unspent.
    Approved,
    /// Not a token this ledger holds: forged, already spent, or from
    /// another session.
    Unknown,
    /// Minted here, but presented at or past its expiry.
    Expired,
    /// Minted here for a different channel, account, or text.
    Mismatch,
}

/// Minting failed.
#[derive(Debug, thiserror::Error)]
pub enum MintError {
    /// The system RNG could not be read.
    #[error("approval entropy unavailable: {source}")]
    Entropy {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The session already holds the maximum of unspent approvals.
    #[error("approval ledger full: at most {max} pending")]
    Full {
        /// The bound.
        max: usize,
    },
}

/// One pending approval.
#[derive(Debug)]
struct Pending {
    channel: String,
    account: Option<String>,
    text: String,
    expires_at_secs: u64,
}

/// A session's unspent approvals.
#[derive(Debug, Default)]
pub struct ApprovalLedger {
    pending: HashMap<String, Pending>,
}

impl ApprovalLedger {
    /// Mints a single-use approval for exactly `text` on `channel` as
    /// `account` (the account's label; `None` on a credential-less
    /// channel), valid until [`APPROVAL_TTL_SECS`] after `now_secs`.
    ///
    /// # Errors
    ///
    /// Returns [`MintError::Full`] when the session already holds
    /// [`MAX_PENDING_APPROVALS`] unexpired approvals, or
    /// [`MintError::Entropy`] if the RNG cannot be read.
    pub fn mint(
        &mut self,
        channel: &str,
        account: Option<&str>,
        text: &str,
        now_secs: u64,
    ) -> Result<ApprovalToken, MintError> {
        self.pending
            .retain(|_, pending| pending.expires_at_secs > now_secs);
        if self.pending.len() >= MAX_PENDING_APPROVALS {
            return Err(MintError::Full {
                max: MAX_PENDING_APPROVALS,
            });
        }
        let token = random_token()?;
        self.pending.insert(
            token.clone(),
            Pending {
                channel: channel.to_owned(),
                account: account.map(str::to_owned),
                text: text.to_owned(),
                expires_at_secs: now_secs.saturating_add(APPROVAL_TTL_SECS),
            },
        );
        Ok(ApprovalToken(token))
    }

    /// Verifies a presented token for `text` on `channel` as `account`,
    /// consuming it whatever the verdict.
    pub fn verify(
        &mut self,
        presented: &str,
        channel: &str,
        account: Option<&str>,
        text: &str,
        now_secs: u64,
    ) -> Verdict {
        let Some(pending) = self.pending.remove(presented) else {
            return Verdict::Unknown;
        };
        if now_secs >= pending.expires_at_secs {
            return Verdict::Expired;
        }
        if pending.channel != channel
            || pending.account.as_deref() != account
            || pending.text != text
        {
            return Verdict::Mismatch;
        }
        Verdict::Approved
    }

    /// How many approvals wait unspent (expired ones included until the
    /// next mint drops them).
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

/// [`TOKEN_PREFIX`] and [`TOKEN_BYTES`] random bytes, lowercase hex.
fn random_token() -> Result<String, MintError> {
    random_hex(TOKEN_PREFIX).map_err(|source| MintError::Entropy { source })
}

/// `prefix` and [`TOKEN_BYTES`] bytes from the system RNG, lowercase
/// hex: the broker's one minting primitive, for rail approvals and
/// account handles alike.
pub(crate) fn random_hex(prefix: &str) -> Result<String, std::io::Error> {
    use std::io::Read as _;
    let mut bytes = [0u8; TOKEN_BYTES];
    std::fs::File::open("/dev/urandom").and_then(|mut rng| rng.read_exact(&mut bytes))?;
    Ok(hex_token(prefix, &bytes))
}

/// `prefix`, then each of `bytes` as two lowercase hex digits.
pub(crate) fn hex_token(prefix: &str, bytes: &[u8]) -> String {
    let mut token = String::from(prefix);
    for b in bytes {
        token.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        token.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
    }
    token
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000;

    #[test]
    fn a_minted_token_approves_exactly_its_message_once() {
        let mut ledger = ApprovalLedger::default();
        let token = ledger
            .mint("local", None, "delete everything", NOW)
            .unwrap();
        assert_eq!(ledger.pending(), 1);
        assert_eq!(
            ledger.verify(token.as_str(), "local", None, "delete everything", NOW),
            Verdict::Approved
        );
        assert_eq!(ledger.pending(), 0, "verifying spends the token");
        assert_eq!(
            ledger.verify(token.as_str(), "local", None, "delete everything", NOW),
            Verdict::Unknown,
            "a spent token is gone"
        );
    }

    #[test]
    fn a_token_on_another_message_is_a_mismatch_and_is_spent() {
        let mut ledger = ApprovalLedger::default();
        let token = ledger
            .mint("local", None, "delete everything", NOW)
            .unwrap();
        assert_eq!(
            ledger.verify(token.as_str(), "local", None, "delete everything!", NOW),
            Verdict::Mismatch
        );
        // The wrong guess burned it: the right message no longer passes.
        assert_eq!(
            ledger.verify(token.as_str(), "local", None, "delete everything", NOW),
            Verdict::Unknown
        );
        let token = ledger
            .mint("local", None, "delete everything", NOW)
            .unwrap();
        assert_eq!(
            ledger.verify(token.as_str(), "other", None, "delete everything", NOW),
            Verdict::Mismatch,
            "the channel is bound too"
        );
        let token = ledger
            .mint("mail", Some("work"), "delete everything", NOW)
            .unwrap();
        assert_eq!(
            ledger.verify(
                token.as_str(),
                "mail",
                Some("personal"),
                "delete everything",
                NOW
            ),
            Verdict::Mismatch,
            "the account is bound too"
        );
        let token = ledger
            .mint("mail", Some("work"), "delete everything", NOW)
            .unwrap();
        assert_eq!(
            ledger.verify(token.as_str(), "mail", None, "delete everything", NOW),
            Verdict::Mismatch,
            "an account-bound approval is not an account-less one"
        );
        let token = ledger
            .mint("mail", Some("work"), "delete everything", NOW)
            .unwrap();
        assert_eq!(
            ledger.verify(
                token.as_str(),
                "mail",
                Some("work"),
                "delete everything",
                NOW
            ),
            Verdict::Approved
        );
    }

    #[test]
    fn expiry_is_exact() {
        let mut ledger = ApprovalLedger::default();
        let last_valid = NOW + APPROVAL_TTL_SECS - 1;
        let token = ledger.mint("local", None, "t", NOW).unwrap();
        assert_eq!(
            ledger.verify(token.as_str(), "local", None, "t", last_valid),
            Verdict::Approved
        );
        let token = ledger.mint("local", None, "t", NOW).unwrap();
        assert_eq!(
            ledger.verify(token.as_str(), "local", None, "t", NOW + APPROVAL_TTL_SECS),
            Verdict::Expired
        );
    }

    #[test]
    fn a_string_the_broker_did_not_mint_is_unknown() {
        let mut ledger = ApprovalLedger::default();
        let _real = ledger.mint("local", None, "t", NOW).unwrap();
        for forged in [
            "",
            "x",
            "rail-token",
            &format!("{TOKEN_PREFIX}{}", "0".repeat(64)),
        ] {
            assert_eq!(
                ledger.verify(forged, "local", None, "t", NOW),
                Verdict::Unknown,
                "{forged:?}"
            );
        }
        assert_eq!(
            ledger.pending(),
            1,
            "a forged presentation spends nothing real"
        );
    }

    #[test]
    fn the_ledger_is_bounded_and_expired_tokens_make_room() {
        let mut ledger = ApprovalLedger::default();
        for n in 0..MAX_PENDING_APPROVALS {
            ledger.mint("local", None, &n.to_string(), NOW).unwrap();
        }
        assert!(matches!(
            ledger.mint("local", None, "one more", NOW),
            Err(MintError::Full {
                max: MAX_PENDING_APPROVALS
            })
        ));
        // One second before they expire they still hold their places;
        // at expiry a mint drops them all.
        assert!(
            ledger
                .mint("local", None, "one more", NOW + APPROVAL_TTL_SECS - 1)
                .is_err()
        );
        let fresh = ledger
            .mint("local", None, "one more", NOW + APPROVAL_TTL_SECS)
            .unwrap();
        assert_eq!(ledger.pending(), 1);
        assert_eq!(
            ledger.verify(
                fresh.as_str(),
                "local",
                None,
                "one more",
                NOW + APPROVAL_TTL_SECS
            ),
            Verdict::Approved
        );
    }

    #[test]
    fn both_nibbles_of_every_byte_carry_entropy() {
        // Eight tokens: 256 high-nibble and 256 low-nibble digits. A
        // broken nibble split collapses one side to one or two digits.
        let mut ledger = ApprovalLedger::default();
        let mut high = std::collections::BTreeSet::new();
        let mut low = std::collections::BTreeSet::new();
        for _ in 0..8 {
            let token = ledger.mint("local", None, "t", NOW).unwrap();
            let hex = token
                .as_str()
                .strip_prefix(TOKEN_PREFIX)
                .unwrap()
                .to_owned();
            for (i, c) in hex.chars().enumerate() {
                if i % 2 == 0 {
                    high.insert(c);
                } else {
                    low.insert(c);
                }
            }
        }
        assert!(high.len() > 8, "high nibbles: {high:?}");
        assert!(low.len() > 8, "low nibbles: {low:?}");
    }

    #[test]
    fn hex_encoding_is_exact() {
        // Deterministic: each byte's high then low nibble, lowercase.
        // (The entropy tests above are statistical; this pins the
        // encoding itself.)
        assert_eq!(
            hex_token("p-", &[0x00, 0x0f, 0x10, 0x5a, 0xab, 0xf0, 0xff]),
            "p-000f105aabf0ff"
        );
        assert_eq!(hex_token("", &[]), "");
    }

    #[test]
    fn tokens_are_prefixed_hex_and_distinct() {
        let mut ledger = ApprovalLedger::default();
        let a = ledger.mint("local", None, "t", NOW).unwrap();
        let b = ledger.mint("local", None, "t", NOW).unwrap();
        assert_ne!(a, b);
        let hex = a.as_str().strip_prefix(TOKEN_PREFIX).unwrap();
        assert_eq!(hex.len(), 64);
        assert!(
            hex.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        assert_eq!(MAX_PENDING_APPROVALS, 64);
        assert_eq!(APPROVAL_TTL_SECS, 300);
    }
}
