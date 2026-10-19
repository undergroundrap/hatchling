//! Activation confirmations: the rail between an idea and a
//! build.
//!
//! An idea is activated in three steps (see
//! [`hatchling_artifact::IdeaRecord`]): a session *proposes* an
//! activation and the broker pins its exact build request; the user is
//! shown that request and *confirms* it; the session *activates* it with
//! the confirmation. This module is the middle step.
//!
//! - **Only the broker mints** ([`confirm_activation`]). It is the hook the
//!   confirmation rail calls once a person has confirmed the
//!   proposal they were shown. No session op reaches it, so a cell can
//!   present a confirmation but never make one.
//! - **A confirmation is bound** to one activation and the hash of the
//!   request it pinned, and is good once, for [`CONFIRMATION_TTL_SECS`].
//!   Any presentation spends it, match or not
//!   ([`take_confirmation`]).
//! - **What is built is what was confirmed.** The activation pins the
//!   request again from the idea and builds only if its hash is the one
//!   the confirmation names; a change to the idea after the proposal
//!   withdraws it.
//!
//! Confirmations live in the broker's memory: a restart forgets them,
//! and the user confirms again.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use hatchling_vault::OpensStores;

use crate::Config;
use crate::artifact_ops::SealedArtifactStore;

/// Seconds a confirmation stays good after minting.
pub const CONFIRMATION_TTL_SECS: i64 = 300;
/// Most confirmations waiting at once.
pub const MAX_PENDING_CONFIRMATIONS: usize = 64;
/// Every confirmation starts with this.
pub const CONFIRMATION_PREFIX: &str = "confirm-";

/// What one confirmation is good for.
#[derive(Debug)]
struct Pending {
    activation: String,
    request_hash: String,
    expires_at: i64,
}

/// The confirmations waiting to be presented, by token.
static CONFIRMATIONS: Mutex<BTreeMap<String, Pending>> = Mutex::new(BTreeMap::new());

/// Why a confirmation could not be minted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmError {
    /// No idea has that activation awaiting confirmation.
    NoProposal,
    /// The most confirmations are already waiting.
    Full,
    /// The store could not be read, or there was no entropy.
    Unavailable,
}

/// Mints the confirmation for `activation`, which the user confirmed at
/// `now`, after being shown its proposal.
///
/// Broker-side only: the confirmation rail calls this; no session op
/// does.
///
/// # Errors
///
/// [`ConfirmError::NoProposal`] if no idea has `activation` awaiting
/// confirmation, [`ConfirmError::Full`] at [`MAX_PENDING_CONFIRMATIONS`],
/// or [`ConfirmError::Unavailable`].
pub fn confirm_activation(
    config: &Config,
    key: &dyn OpensStores,
    activation: &str,
    now: i64,
) -> Result<String, ConfirmError> {
    let ideas = SealedArtifactStore::new(&config.artifacts.root, key)
        .ideas()
        .map_err(|_| ConfirmError::Unavailable)?;
    let request_hash = ideas
        .iter()
        .find_map(|idea| {
            idea.pinned(activation)
                .map(|(_, proposal)| proposal.request_hash.clone())
        })
        .ok_or(ConfirmError::NoProposal)?;
    let token =
        crate::approval::random_hex(CONFIRMATION_PREFIX).map_err(|_| ConfirmError::Unavailable)?;
    admit(
        &mut CONFIRMATIONS.lock().unwrap_or_else(PoisonError::into_inner),
        token.clone(),
        Pending {
            activation: activation.to_owned(),
            request_hash,
            expires_at: now.saturating_add(CONFIRMATION_TTL_SECS),
        },
        now,
    )?;
    Ok(token)
}

/// Keeps `entry` under `token` in `pending`, after dropping every
/// confirmation expired at `now`.
///
/// # Errors
///
/// [`ConfirmError::Full`] at [`MAX_PENDING_CONFIRMATIONS`].
fn admit(
    pending: &mut BTreeMap<String, Pending>,
    token: String,
    entry: Pending,
    now: i64,
) -> Result<(), ConfirmError> {
    pending.retain(|_, waiting| waiting.expires_at > now);
    if pending.len() >= MAX_PENDING_CONFIRMATIONS {
        return Err(ConfirmError::Full);
    }
    pending.insert(token, entry);
    Ok(())
}

/// Spends `token`, and says whether it confirmed exactly `activation`
/// with `request_hash`, unexpired at `now`. A token is spent by any
/// presentation, match or not.
pub(crate) fn take_confirmation(
    token: &str,
    activation: &str,
    request_hash: &str,
    now: i64,
) -> bool {
    CONFIRMATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(token)
        .is_some_and(|pending| {
            pending.activation == activation
                && pending.request_hash == request_hash
                && now < pending.expires_at
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plant(token: &str, activation: &str, hash: &str, expires_at: i64) {
        CONFIRMATIONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                token.to_owned(),
                Pending {
                    activation: activation.to_owned(),
                    request_hash: hash.to_owned(),
                    expires_at,
                },
            );
    }

    fn waiting(expires_at: i64) -> Pending {
        Pending {
            activation: String::from("act-a"),
            request_hash: String::from("h"),
            expires_at,
        }
    }

    #[test]
    fn the_ledger_drops_what_expired_and_holds_at_most_its_bound() {
        let mut pending = BTreeMap::new();
        pending.insert(String::from("at-now"), waiting(100));
        pending.insert(String::from("after"), waiting(101));
        admit(&mut pending, String::from("new"), waiting(400), 100).unwrap();
        let kept: Vec<&str> = pending.keys().map(String::as_str).collect();
        assert_eq!(kept, ["after", "new"]);
        // Full at the bound, unexpired.
        let mut full: BTreeMap<String, Pending> = (0..MAX_PENDING_CONFIRMATIONS - 1)
            .map(|n| (format!("t{n}"), waiting(500)))
            .collect();
        admit(&mut full, String::from("last"), waiting(500), 100).unwrap();
        assert_eq!(full.len(), MAX_PENDING_CONFIRMATIONS);
        assert_eq!(
            admit(&mut full, String::from("over"), waiting(500), 100),
            Err(ConfirmError::Full)
        );
        assert!(!full.contains_key("over"));
        // Expired entries make room.
        admit(&mut full, String::from("later"), waiting(900), 500).unwrap();
        assert_eq!(full.len(), 1);
    }

    #[test]
    fn a_confirmation_is_good_once_for_exactly_its_activation_until_it_expires() {
        let id = |n: u32| format!("confirm-unit-{}-{n}", std::process::id());
        plant(&id(1), "act-a", "h", 100);
        assert!(take_confirmation(&id(1), "act-a", "h", 99));
        assert!(!take_confirmation(&id(1), "act-a", "h", 99), "spent");
        // Wrong activation, wrong hash, or expired: refused, and spent.
        for (n, activation, hash, now) in [
            (2, "act-b", "h", 99),
            (3, "act-a", "other", 99),
            (4, "act-a", "h", 100),
        ] {
            plant(&id(n), "act-a", "h", 100);
            assert!(!take_confirmation(&id(n), activation, hash, now), "{n}");
            assert!(!take_confirmation(&id(n), "act-a", "h", 99), "{n} spent");
        }
        assert!(!take_confirmation("confirm-forged", "act-a", "h", 0));
        assert_eq!(
            (
                CONFIRMATION_TTL_SECS,
                MAX_PENDING_CONFIRMATIONS,
                CONFIRMATION_PREFIX
            ),
            (300, 64, "confirm-")
        );
    }
}
