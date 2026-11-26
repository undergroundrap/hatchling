//! The owner's Review page (WO-054): what a background loop may not apply
//! itself, kept for the owner to approve or reject when they want.
//!
//! What lands here:
//! - replacing or removing one of the owner's preferences, boundaries or
//!   commitments;
//! - a fact about who the owner is;
//! - anything a loop's checks couldn't resolve;
//! - installing new guidance.
//!
//! The queue never blocks a loop: [`propose`] queues and returns.
//!
//! **Append-only.** A waiting item is never removed, and a decided one is
//! kept as it was decided. Only when the queue is full does the oldest
//! decided item give way. A queue of waiting items alone refuses a new one,
//! and the loop goes on without it.
//!
//! **A decision is recorded, not applied here.** An approved item is
//! applied by its loop's own apply step, which rechecks its target's
//! fingerprint and marks it stale when the target changed. A rejected one
//! changes nothing.
//!
//! The items are sealed in the schedule store, under its key, one entry
//! each and an index, apart from every schedule's: a schedule op can't
//! name one.

use std::sync::{Mutex, PoisonError};

use hatchling_core::ui::{ReviewItem, ReviewKind, ReviewOp, ReviewResult, ReviewState};
use hatchling_scheduler::Timestamp;
use hatchling_vault::{OpensStores, SealedVault};

use crate::schedule_ops::{read_json, write_json};
use crate::{Config, Error, SchedulerConfig};

/// The index of the items, oldest first.
const INDEX: &str = "review-index";

/// The most items kept.
pub const MAX_ITEMS: usize = 512;

/// The longest a change is said.
pub const MAX_CHANGE_BYTES: usize = 2048;

/// The longest its reason is.
pub const MAX_WHY_BYTES: usize = 1024;

/// The most handles an item cites.
pub const MAX_EVIDENCE: usize = 8;

/// The longest a handle or a loop's name is.
const MAX_NAME_BYTES: usize = 512;

/// One change to the items at a time, within this broker.
static STORE: Mutex<()> = Mutex::new(());

/// What a loop proposes.
#[derive(Debug, Clone, Copy)]
pub struct Proposal<'a> {
    /// The loop, such as `memory`.
    pub proposed_by: &'a str,
    /// What kind of change.
    pub kind: ReviewKind,
    /// The record it would change, by its handle.
    pub target: Option<&'a str>,
    /// What would change.
    pub change: &'a str,
    /// Why.
    pub why: &'a str,
    /// The handles it cites.
    pub evidence: &'a [String],
}

/// What came of a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proposed {
    /// It waits for the owner, under this id.
    Queued(String),
    /// The queue holds waiting items alone, as many as it keeps.
    Full,
    /// It breaks a bound.
    Refused(&'static str),
}

/// The entry an item is kept in.
fn entry(id: &str) -> String {
    format!("review-{id}")
}

/// Whether `id` can name an item: what [`propose`] makes.
fn is_item_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-' || byte.is_ascii_lowercase())
}

/// Why `proposal` can't be kept, if it can't.
fn refusal(proposal: &Proposal<'_>) -> Option<&'static str> {
    let name = |text: &str| !text.is_empty() && text.len() <= MAX_NAME_BYTES;
    if !name(proposal.proposed_by) {
        return Some("the loop's name is empty or too long");
    }
    if proposal.change.trim().is_empty() || proposal.change.len() > MAX_CHANGE_BYTES {
        return Some("the change is empty or too long");
    }
    if proposal.why.len() > MAX_WHY_BYTES {
        return Some("the reason is too long");
    }
    if proposal.evidence.is_empty() || proposal.evidence.len() > MAX_EVIDENCE {
        return Some("it cites no handle, or too many");
    }
    if !proposal.evidence.iter().all(|handle| name(handle)) || !proposal.target.is_none_or(name) {
        return Some("a handle is empty or too long");
    }
    None
}

/// The schedule store's vault, where the items are kept.
fn vault<'k>(
    scheduler: &'k SchedulerConfig,
    key: &'k dyn OpensStores,
) -> Result<SealedVault<'k>, Error> {
    SealedVault::open_in(&scheduler.root, key, hatchling_vault::Store::SCHEDULES)
        .map_err(|source| Error::Vault { source })
}

/// Queues `proposal` for the owner at `now`.
///
/// # Errors
///
/// [`Error`] on a backend failure: then nothing is queued.
pub fn propose(
    scheduler: &SchedulerConfig,
    key: &dyn OpensStores,
    proposal: &Proposal<'_>,
    now: Timestamp,
) -> Result<Proposed, Error> {
    if let Some(why) = refusal(proposal) {
        return Ok(Proposed::Refused(why));
    }
    let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
    let vault = vault(scheduler, key)?;
    let mut index: Vec<String> = read_json(&vault, INDEX)?.unwrap_or_default();
    if index.len() >= MAX_ITEMS {
        // The oldest decided item gives way; a waiting one never does.
        let mut gone = None;
        for (at, id) in index.iter().enumerate() {
            let kept: Option<ReviewItem> = read_json(&vault, &entry(id))?;
            if kept.is_none_or(|item| item.state != ReviewState::Waiting) {
                gone = Some(at);
                break;
            }
        }
        let Some(at) = gone else {
            return Ok(Proposed::Full);
        };
        let id = index.remove(at);
        if let Ok(name) = entry(&id).parse() {
            let _ = vault.remove(&name);
        }
    }
    let id = crate::approval::random_hex("review-")
        .map_err(|source| Error::Io { source })?
        .trim_start_matches("review-")
        .to_owned();
    let item = ReviewItem {
        item_id: id.clone(),
        proposed_by: proposal.proposed_by.to_owned(),
        kind: proposal.kind,
        target: proposal.target.map(str::to_owned),
        change: proposal.change.to_owned(),
        why: proposal.why.to_owned(),
        evidence: proposal.evidence.to_vec(),
        proposed_at: now.to_string(),
        state: ReviewState::Waiting,
        decided_at: None,
    };
    write_json(&vault, &entry(&id), &item)?;
    index.push(id.clone());
    write_json(&vault, INDEX, &index)?;
    Ok(Proposed::Queued(id))
}

/// The items, the newest first.
///
/// # Errors
///
/// [`Error`] on a backend failure.
pub fn list(scheduler: &SchedulerConfig, key: &dyn OpensStores) -> Result<Vec<ReviewItem>, Error> {
    let vault = vault(scheduler, key)?;
    let index: Vec<String> = read_json(&vault, INDEX)?.unwrap_or_default();
    let mut items = Vec::with_capacity(index.len());
    for id in index.iter().rev() {
        if let Some(item) = read_json::<ReviewItem>(&vault, &entry(id))? {
            items.push(item);
        }
    }
    Ok(items)
}

/// Approves the waiting item `id`, or rejects it, at `now`.
///
/// # Errors
///
/// [`Error`] on a backend failure: then nothing is decided.
pub fn decide(
    scheduler: &SchedulerConfig,
    key: &dyn OpensStores,
    id: &str,
    approve: bool,
    now: Timestamp,
) -> Result<ReviewResult, Error> {
    if !is_item_id(id) {
        return Ok(ReviewResult::NotFound);
    }
    let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
    let vault = vault(scheduler, key)?;
    let index: Vec<String> = read_json(&vault, INDEX)?.unwrap_or_default();
    if !index.iter().any(|kept| kept == id) {
        return Ok(ReviewResult::NotFound);
    }
    let Some(mut item) = read_json::<ReviewItem>(&vault, &entry(id))? else {
        return Ok(ReviewResult::NotFound);
    };
    if item.state != ReviewState::Waiting {
        return Ok(ReviewResult::NotWaiting { state: item.state });
    }
    item.state = if approve {
        ReviewState::Approved
    } else {
        ReviewState::Rejected
    };
    item.decided_at = Some(now.to_string());
    write_json(&vault, &entry(id), &item)?;
    Ok(ReviewResult::Decided {
        item: Box::new(item),
    })
}

/// Answers the owner's `op` on their Review page.
#[must_use]
pub fn handle(config: &Config, key: &dyn OpensStores, op: &ReviewOp) -> ReviewResult {
    if !config.scheduler.enabled() {
        return ReviewResult::Off;
    }
    let done = match op {
        ReviewOp::List => list(&config.scheduler, key).map(|items| ReviewResult::Listed { items }),
        ReviewOp::Decide { item_id, approve } => {
            decide(&config.scheduler, key, item_id, *approve, Timestamp::now())
        }
    };
    done.unwrap_or_else(|error| {
        tracing::warn!(%error, "the Review page couldn't be read or changed");
        ReviewResult::Failed
    })
}

#[cfg(test)]
#[path = "review_queue_tests.rs"]
mod tests;
