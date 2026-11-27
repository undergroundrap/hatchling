//! Production [`MemoryProvider`] backed by the broker session.
//!
//! The loop recalls memory through [`hatchling_loop::MemoryProvider`];
//! this is the host implementation that speaks the broker's memory
//! ops. The loop never touches the store; the broker asserts the
//! principal and binds the scope.
//!
//! ## Recall policy (v0.1)
//!
//! The broker has no "list all" operation (deliberate: 's
//! no-bulk-export ratchet), so the provider cannot enumerate the
//! store. Instead, recall is **opt-in by tag**: items carrying the
//! well-known [`RECALL_TAG`] are fetched via `RecallByTag` from the
//! user scope and the broker-bound project scope.
//!
//! This is not enumeration: the provider cannot discover untagged
//! items, and `RecallByTag` cannot cross the scope boundary
//! (broker-enforced). The merged result is ordered most-recently-
//! updated first and bounded by [`MAX_RECALL_ITEMS`] and
//! [`MAX_RECALL_BYTES`].
//!
//! Fail-closed: any broker failure (transport, denial, malformed
//! reply) becomes a [`RecallError`]; the loop warns and proceeds
//! without memory. A recall failure never blocks the live user task.

use std::os::unix::net::UnixStream;

use hatchling_loop::{MemoryProvider, RecallError};
use hatchling_memory::MemoryItem;

use crate::memory::{self, MemoryScope};

/// Well-known tag: items carrying this tag are recalled into the
/// loop's prompt at iteration start. Tagging is the explicit opt-in;
/// untagged items are never recalled by the loop.
pub const RECALL_TAG: &str = "recall";

/// Maximum recalled items per iteration. Bounds prompt injection.
pub const MAX_RECALL_ITEMS: usize = 10;

/// Maximum total content bytes across recalled items. The item list is
/// truncated to fit (items are never truncated); most-recently-updated
/// first.
///
/// An item that would overflow the remaining budget is skipped, not
/// fatal: older items keep filling what is left (see
/// `bound_recalled`). One oversized item never suppresses all recall.
pub const MAX_RECALL_BYTES: usize = 8 * 1024;

/// The most a run's index of its recalled memory takes, in bytes (WO-048).
pub const INDEX_BYTES: u32 = 3 * 1024;

/// The most the items it's given whole take, in bytes (WO-048).
pub const FULL_BYTES: u32 = 5 * 1024;

/// The production memory provider: recalls tagged items through the
/// peer-authorized broker session.
pub struct BrokerMemoryProvider<'s> {
    stream: &'s UnixStream,
}

impl<'s> BrokerMemoryProvider<'s> {
    /// Build over the broker session. The stream must be the
    /// peer-authorized session connection; the broker asserts the
    /// principal and binds every scope.
    #[must_use]
    pub const fn new(stream: &'s UnixStream) -> Self {
        Self { stream }
    }

    /// Fetches tagged items from one scope and parses them. Any
    /// broker failure or malformed item fails the recall: fail-closed,
    /// never partial trust.
    fn recall_scope(&self, scope: MemoryScope) -> Result<Vec<MemoryItem>, RecallError> {
        let items_json = memory::recall_by_tag(self.stream, scope, RECALL_TAG)
            .map_err(|error| RecallError(format!("broker recall-by-tag failed: {error}")))?;
        items_json
            .iter()
            .map(|json| {
                serde_json::from_str::<MemoryItem>(json)
                    .map_err(|error| RecallError(format!("broker sent malformed item: {error}")))
            })
            .collect()
    }
}

impl MemoryProvider for BrokerMemoryProvider<'_> {
    /// Recall tagged items from the user and project scopes.
    ///
    /// Merged most-recently-updated first (id breaks ties), then
    /// bounded by [`MAX_RECALL_ITEMS`] and [`MAX_RECALL_BYTES`].
    /// The index of every recalled item within [`INDEX_BYTES`], and the
    /// items given whole within [`FULL_BYTES`], the broker reading only
    /// what it gives (v38).
    fn recall_budgeted(&self) -> Result<hatchling_memory::Recalled, RecallError> {
        memory::recall_budgeted(self.stream, RECALL_TAG, INDEX_BYTES, FULL_BYTES)
            .map_err(|error| RecallError(format!("broker budgeted recall failed: {error}")))
    }

    fn recall(&self) -> Result<Vec<MemoryItem>, RecallError> {
        let mut items = self.recall_scope(MemoryScope::User)?;
        items.extend(self.recall_scope(MemoryScope::Project)?);

        Ok(bound_recalled(items))
    }
}

/// Orders recalled items most-recently-updated first (id breaks ties)
/// and bounds them by [`MAX_RECALL_ITEMS`], then [`MAX_RECALL_BYTES`].
/// The byte budget skips items, never truncates them: an item that
/// would overflow the remaining budget is dropped, and older items
/// keep filling what is left.
fn bound_recalled(mut items: Vec<MemoryItem>) -> Vec<MemoryItem> {
    items.sort_by(|a, b| {
        b.updated_at_secs
            .cmp(&a.updated_at_secs)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    let mut bytes = 0usize;
    items.retain(|item| {
        // An oversized item does not poison the budget: skip it and
        // keep filling with older ones. The budget bounds the total,
        // never a single item.
        if bytes + item.content.len() > MAX_RECALL_BYTES {
            false
        } else {
            bytes += item.content.len();
            true
        }
    });
    // Cap what is kept, not what is considered: ten oversized newest
    // items must not starve older items that fit.
    items.truncate(MAX_RECALL_ITEMS);
    items
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use hatchling_memory::{MemoryId, MemoryItem, RetentionPolicy};

    use super::{MAX_RECALL_BYTES, MAX_RECALL_ITEMS, bound_recalled};

    fn item(id: &str, updated: u64, len: usize) -> MemoryItem {
        MemoryItem {
            id: MemoryId::from_str(id).unwrap(),
            scope: hatchling_memory::MemoryScope::User,
            content: "x".repeat(len),
            tags: Vec::new(),
            created_at_secs: 1,
            updated_at_secs: updated,
            retention: RetentionPolicy::Keep,
            record: None,
        }
    }

    fn ids(items: &[MemoryItem]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn bounds_are_ten_items_and_8_kib() {
        assert_eq!(MAX_RECALL_ITEMS, 10);
        assert_eq!(MAX_RECALL_BYTES, 8 * 1024);
    }

    #[test]
    fn newest_first_with_id_breaking_ties() {
        let out = bound_recalled(vec![
            item("old", 1, 1),
            item("b-new", 5, 1),
            item("a-new", 5, 1),
        ]);
        assert_eq!(ids(&out), vec!["a-new", "b-new", "old"]);
    }

    #[test]
    fn item_cap_keeps_the_ten_newest() {
        let items = (0..12).map(|n| item(&format!("i{n:02}"), n, 1)).collect();
        let out = bound_recalled(items);
        assert_eq!(out.len(), 10);
        assert_eq!(out.first().map(|i| i.id.as_str()), Some("i11"));
        assert_eq!(out.last().map(|i| i.id.as_str()), Some("i02"));
    }

    #[test]
    fn exactly_8_kib_is_kept_one_byte_more_ends_the_list() {
        let fits = bound_recalled(vec![item("a", 3, 4096), item("b", 2, 4096)]);
        assert_eq!(ids(&fits), vec!["a", "b"]);
        let over = bound_recalled(vec![
            item("a", 3, 4096),
            item("b", 2, 4096),
            item("c", 1, 1),
        ]);
        assert_eq!(ids(&over), vec!["a", "b"]);
    }

    #[test]
    fn an_overflowing_item_is_skipped_older_items_still_fill() {
        // Policy: an item that would overflow the remaining budget
        // is skipped, not fatal. Older items keep filling what is
        // left, so one oversized item never suppresses all recall.
        let out = bound_recalled(vec![
            item("big", 9, 9000),
            item("small", 5, 10),
            item("older", 1, 20),
        ]);
        assert_eq!(ids(&out), vec!["small", "older"]);
    }

    #[test]
    fn only_oversized_items_means_empty_recall() {
        // If every item overflows on its own, nothing is recalled —
        // the budget bounds the total, never a single item.
        let out = bound_recalled(vec![item("big", 9, 9000), item("bigger", 5, 9000)]);
        assert!(out.is_empty(), "{:?}", ids(&out));
    }

    #[test]
    fn oversized_newest_items_do_not_starve_older_ones_past_the_cap() {
        let mut items: Vec<MemoryItem> = (0..10)
            .map(|n| item(&format!("big{n}"), 100 + n, 9000))
            .collect();
        items.push(item("small", 1, 10));
        assert_eq!(ids(&bound_recalled(items)), vec!["small"]);
    }
}
