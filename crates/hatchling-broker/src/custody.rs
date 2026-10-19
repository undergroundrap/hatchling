//! Channel credential custody: the broker holds secrets, the
//! cell holds handles.
//!
//! A credentialed channel authenticates to its backend as one of its
//! configured *accounts*. Each account's secret lives sealed in the
//! vault ; the broker loads it into its own memory when it
//! builds a session's channel registry, and lends it to the channel
//! backend ([`Credential`](hatchling_channel::Credential)) for the one
//! call that needs it. The cell never holds it and no reply carries it:
//! the cell names an account by an opaque handle, and the protocol has
//! no field a secret could travel in.
//!
//! - *Handles* ([`HANDLE_PREFIX`] and 32 random bytes in hex, from the
//!   same RNG path as rail approvals) are minted per session when the
//!   registry loads the accounts, and are bound to their channel. The
//!   cell learns them only from an `Accounts` reply on that session.
//! - *Resolution* is exact: a handle this session did not mint, one
//!   minted for another channel, a missing handle on a credentialed
//!   channel, or any handle on a credential-less one is a typed
//!   [`HandleRejection`], and the cell gets `UnknownAccount`.
//! - *Lifetime*: a secret is held for the session, in a
//!   [`Secret`] that zeroizes when the registry drops at session end.
//! - *Backstop*: an inbound message whose text carries any loaded
//!   secret's bytes is withheld ([`AccountBook::exposes_a_secret`]), so
//!   even a backend that echoes its credential cannot put it in front
//!   of the cell.

use std::collections::HashMap;

use hatchling_core::ChannelAccount;
use hatchling_vault::Secret;

use crate::Error;
use crate::config::ChannelAccountSpec;

/// Every account handle starts with this, versioning the format.
pub const HANDLE_PREFIX: &str = "acct-v1-";

/// One loaded account: its channel, its label, and its secret.
pub struct Account {
    channel: String,
    label: String,
    secret: Secret,
}

impl Account {
    /// The operator's label for this account.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The secret, for lending to the channel backend only.
    #[must_use]
    pub fn secret(&self) -> &[u8] {
        self.secret.as_bytes()
    }
}

/// Why a handle did not resolve. The cell only ever learns
/// `UnknownAccount`; the variants are for the broker's logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleRejection {
    /// Not a handle this session minted: forged, guessed, or from
    /// another session.
    Unknown,
    /// Minted on this session, for another channel.
    OtherChannel,
    /// The channel is credentialed and the op named no account.
    Missing,
    /// The channel is credential-less and the op named an account.
    NotCredentialed,
}

/// A session's loaded accounts, by handle.
#[derive(Default)]
pub struct AccountBook {
    by_handle: HashMap<String, Account>,
}

impl AccountBook {
    /// Takes every configured account's secret, read from the vault in
    /// the order of `specs` (`None` where nothing is stored), and mints
    /// its handle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vault`] if a secret wasn't found, and
    /// [`Error::ChannelCustody`] for an empty secret, a secret missing
    /// from `secrets`, or an unreadable RNG. The session fails closed on
    /// any of them.
    pub fn load(specs: &[ChannelAccountSpec], secrets: Vec<Option<Secret>>) -> Result<Self, Error> {
        if secrets.len() != specs.len() {
            return Err(Error::ChannelCustody {
                reason: format!(
                    "{} account secrets read for {} accounts",
                    secrets.len(),
                    specs.len()
                ),
            });
        }
        let mut book = Self::default();
        for (spec, secret) in specs.iter().zip(secrets) {
            let secret = secret.ok_or_else(|| Error::Vault {
                source: hatchling_vault::Error::NotFound {
                    name: spec.secret.clone(),
                },
            })?;
            book.insert(&spec.channel, &spec.label, secret)?;
        }
        Ok(book)
    }

    /// Adds one account under a freshly minted handle.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ChannelCustody`] for an empty secret (its bytes
    /// would match every message) or an unreadable RNG.
    pub fn insert(&mut self, channel: &str, label: &str, secret: Secret) -> Result<(), Error> {
        if secret.as_bytes().is_empty() {
            return Err(Error::ChannelCustody {
                reason: format!("account {channel}:{label} has an empty secret"),
            });
        }
        let handle =
            crate::approval::random_hex(HANDLE_PREFIX).map_err(|source| Error::ChannelCustody {
                reason: format!("account handle entropy unavailable: {source}"),
            })?;
        self.by_handle.insert(
            handle,
            Account {
                channel: channel.to_owned(),
                label: label.to_owned(),
                secret,
            },
        );
        Ok(())
    }

    /// Resolves the account an op on `channel` acts as: `None` on a
    /// credential-less channel, the handle's account on a credentialed
    /// one.
    ///
    /// # Errors
    ///
    /// Returns the [`HandleRejection`] when the op's handle (or its
    /// absence) does not fit the channel.
    pub fn resolve(
        &self,
        channel: &str,
        credentialed: bool,
        handle: Option<&str>,
    ) -> Result<Option<&Account>, HandleRejection> {
        match (credentialed, handle) {
            (false, None) => Ok(None),
            (false, Some(_)) => Err(HandleRejection::NotCredentialed),
            (true, None) => Err(HandleRejection::Missing),
            (true, Some(handle)) => match self.by_handle.get(handle) {
                None => Err(HandleRejection::Unknown),
                Some(account) if account.channel != channel => Err(HandleRejection::OtherChannel),
                Some(account) => Ok(Some(account)),
            },
        }
    }

    /// Whether any account is configured on `channel`.
    #[must_use]
    pub fn has_accounts(&self, channel: &str) -> bool {
        self.by_handle
            .values()
            .any(|account| account.channel == channel)
    }

    /// The channels that have accounts.
    #[must_use]
    pub fn channels(&self) -> Vec<&str> {
        self.by_handle
            .values()
            .map(|account| account.channel.as_str())
            .collect()
    }

    /// `channel`'s accounts as the cell sees them, in label order.
    #[must_use]
    pub fn listing(&self, channel: &str) -> Vec<ChannelAccount> {
        let mut accounts: Vec<ChannelAccount> = self
            .by_handle
            .iter()
            .filter(|(_, account)| account.channel == channel)
            .map(|(handle, account)| ChannelAccount {
                handle: handle.clone(),
                label: account.label.clone(),
            })
            .collect();
        accounts.sort_by(|a, b| a.label.cmp(&b.label));
        accounts
    }

    /// Whether `bytes` contain any loaded account's secret.
    #[must_use]
    pub fn exposes_a_secret(&self, bytes: &[u8]) -> bool {
        self.by_handle.values().any(|account| {
            let secret = account.secret();
            bytes.windows(secret.len()).any(|window| window == secret)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(bytes: &[u8]) -> Secret {
        Secret::new(bytes.to_vec()).unwrap()
    }

    fn book() -> AccountBook {
        let mut book = AccountBook::default();
        book.insert("mail", "work", secret(b"work-key")).unwrap();
        book.insert("mail", "personal", secret(b"personal-key"))
            .unwrap();
        book.insert("chat", "bot", secret(b"bot-key")).unwrap();
        book
    }

    fn handle(book: &AccountBook, channel: &str, label: &str) -> String {
        book.listing(channel)
            .into_iter()
            .find(|account| account.label == label)
            .map(|account| account.handle)
            .unwrap()
    }

    #[test]
    fn handles_are_prefixed_hex_distinct_and_carry_no_secret() {
        let book = book();
        let handles: Vec<String> = ["mail", "chat"]
            .iter()
            .flat_map(|channel| book.listing(channel))
            .map(|account| account.handle)
            .collect();
        assert_eq!(handles.len(), 3);
        for h in &handles {
            let hex = h.strip_prefix(HANDLE_PREFIX).unwrap();
            assert_eq!(hex.len(), 64, "{h}");
            assert!(
                hex.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "{h}"
            );
            assert!(!book.exposes_a_secret(h.as_bytes()), "{h}");
        }
        let unique: std::collections::BTreeSet<&String> = handles.iter().collect();
        assert_eq!(unique.len(), 3, "handles are distinct");
    }

    #[test]
    fn two_sessions_never_share_a_handle() {
        let first = book();
        let second = book();
        let work = handle(&first, "mail", "work");
        assert_ne!(work, handle(&second, "mail", "work"));
        assert_eq!(
            second.resolve("mail", true, Some(&work)).err(),
            Some(HandleRejection::Unknown),
            "a handle is good only on the session that minted it"
        );
    }

    #[test]
    fn a_handle_resolves_to_exactly_its_account() {
        let book = book();
        let work = handle(&book, "mail", "work");
        let account = book.resolve("mail", true, Some(&work)).unwrap().unwrap();
        assert_eq!(account.label(), "work");
        assert_eq!(account.secret(), b"work-key");
        let personal = handle(&book, "mail", "personal");
        let account = book
            .resolve("mail", true, Some(&personal))
            .unwrap()
            .unwrap();
        assert_eq!(account.label(), "personal");
        assert_eq!(account.secret(), b"personal-key");
    }

    #[test]
    fn every_misfit_handle_is_a_typed_rejection() {
        let book = book();
        let bot = handle(&book, "chat", "bot");
        for forged in [
            "",
            "x",
            "acct-v1-",
            &format!("{HANDLE_PREFIX}{}", "0".repeat(64)),
            "work",
            "work-key",
        ] {
            assert_eq!(
                book.resolve("mail", true, Some(forged)).err(),
                Some(HandleRejection::Unknown),
                "{forged:?}"
            );
        }
        assert_eq!(
            book.resolve("mail", true, Some(&bot)).err(),
            Some(HandleRejection::OtherChannel)
        );
        assert_eq!(
            book.resolve("mail", true, None).err(),
            Some(HandleRejection::Missing)
        );
        assert_eq!(
            book.resolve("local", false, Some(&bot)).err(),
            Some(HandleRejection::NotCredentialed)
        );
        assert!(matches!(book.resolve("local", false, None), Ok(None)));
    }

    #[test]
    fn listing_is_per_channel_and_label_ordered() {
        let book = book();
        let labels: Vec<String> = book
            .listing("mail")
            .into_iter()
            .map(|account| account.label)
            .collect();
        assert_eq!(labels, ["personal", "work"]);
        assert_eq!(book.listing("chat").len(), 1);
        assert!(book.listing("local").is_empty());
        assert!(book.has_accounts("mail"));
        assert!(!book.has_accounts("local"));
        let mut channels = book.channels();
        channels.sort_unstable();
        assert_eq!(channels, ["chat", "mail", "mail"]);
    }

    #[test]
    fn a_secret_is_found_anywhere_in_the_bytes_and_only_whole() {
        let book = book();
        assert!(book.exposes_a_secret(b"work-key"));
        assert!(book.exposes_a_secret(b"prefix work-key suffix"));
        assert!(book.exposes_a_secret(b"trailing bot-key"));
        assert!(!book.exposes_a_secret(b"work-ke"));
        assert!(!book.exposes_a_secret(b"work key"));
        assert!(!book.exposes_a_secret(b""));
        assert!(!AccountBook::default().exposes_a_secret(b"work-key"));
    }

    #[test]
    fn an_empty_secret_is_refused() {
        let mut book = AccountBook::default();
        let error = book.insert("mail", "work", secret(b"")).unwrap_err();
        assert!(
            matches!(error, Error::ChannelCustody { ref reason } if reason.contains("mail:work")),
            "{error:?}"
        );
        assert!(!book.has_accounts("mail"));
    }
}
