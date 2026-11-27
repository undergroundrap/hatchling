//! What a recall chooses from without reading every item (WO-048, slice 6).
//!
//! Each space keeps a **recall index**: one entry per item, with what a
//! run's index line says of it and what choosing it needs (its kind,
//! times, title, terms and the size of its words). The store keeps it
//! current on every write, keep, retag and remove, and a budgeted recall
//! reads it and then only the items it gives whole.
//!
//! **Order of writes.** A put changes the index before the item and a
//! remove after it, so a write that fails halfway leaves, at worst, an
//! entry whose item is gone, which recall skips. A space with no index
//! (one from before this, or a backing that keeps none) has it built from
//! its items when it's first read.

use serde::{Deserialize, Serialize};

use crate::item::{MemoryId, MemoryItem, Tag, is_expired};
use crate::record::MemoryKind;

/// The longest a title is, in characters.
pub const MAX_TITLE_CHARS: usize = 60;

/// The most terms an entry keeps.
pub const MAX_TERMS: usize = 32;

/// Words too common to tell items apart.
const STOP_WORDS: [&str; 30] = [
    "the", "and", "for", "are", "with", "that", "this", "they", "their", "them", "was", "were",
    "has", "have", "had", "not", "but", "you", "your", "its", "from", "our", "all", "any", "can",
    "will", "just", "into", "than", "then",
];

/// What the index keeps of one item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEntry {
    /// Its id.
    pub id: MemoryId,
    /// Its tags: what recall is asked for by.
    pub tags: Vec<Tag>,
    /// What kind of thing it is, when its record says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MemoryKind>,
    /// Whether it records something known not to be so.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub absence: bool,
    /// Whether the owner approved it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approved: bool,
    /// When it was kept: its record's time, or when it was written.
    pub kept_at_secs: u64,
    /// When it last changed.
    pub updated_at_secs: u64,
    /// When it expires, if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_secs: Option<u64>,
    /// Its first words.
    pub title: String,
    /// The words it can be found by.
    pub terms: Vec<String>,
    /// The size of its words as a run is given them whole.
    pub body_bytes: usize,
}

impl RecallEntry {
    /// The entry for `item`.
    #[must_use]
    pub fn of(item: &MemoryItem) -> Self {
        let record = item.record.as_ref();
        Self {
            id: item.id.clone(),
            tags: item.tags.clone(),
            kind: record.and_then(|record| record.kind),
            absence: record.is_some_and(|record| record.absence),
            approved: item.approved_by_owner(),
            kept_at_secs: record.map_or(item.created_at_secs, |record| record.kept_at_secs),
            updated_at_secs: item.updated_at_secs,
            expires_at_secs: crate::item::expires_at(item),
            title: title_of(&item.content),
            terms: terms_of(item),
            body_bytes: crate::prompt::framed_body(item).len(),
        }
    }

    /// Whether it carries `tag` and hasn't expired at `now_secs`.
    #[must_use]
    pub fn recalled_by(&self, tag: &Tag, now_secs: u64) -> bool {
        self.tags.iter().any(|kept| kept == tag)
            && self
                .expires_at_secs
                .is_none_or(|deadline| now_secs < deadline)
    }
}

/// The first words of `content`: its first line, cut at a word near
/// [`MAX_TITLE_CHARS`] characters.
#[must_use]
pub fn title_of(content: &str) -> String {
    let line = content.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= MAX_TITLE_CHARS {
        return line.to_owned();
    }
    let cut: String = line.chars().take(MAX_TITLE_CHARS).collect();
    let words = cut
        .rfind(' ')
        .filter(|at| *at > MAX_TITLE_CHARS / 3)
        .and_then(|at| cut.get(..at))
        .unwrap_or(cut.as_str());
    format!("{}…", words.trim_end())
}

/// The words `item` can be found by: its words, why and when it applies,
/// lowercased and split on anything not a letter or digit, without the
/// commonest words, each once, the first [`MAX_TERMS`] of them.
#[must_use]
pub fn terms_of(item: &MemoryItem) -> Vec<String> {
    let record = item.record.as_ref();
    let texts = [
        Some(item.content.as_str()),
        record.and_then(|record| record.why.as_deref()),
        record.and_then(|record| record.how_to_apply.as_deref()),
    ];
    let mut terms: Vec<String> = Vec::new();
    for text in texts.into_iter().flatten() {
        for word in text.split(|c: char| !c.is_alphanumeric()) {
            let word = word.to_lowercase();
            if word.chars().count() >= 3
                && !STOP_WORDS.contains(&word.as_str())
                && !terms.contains(&word)
            {
                if terms.len() == MAX_TERMS {
                    return terms;
                }
                terms.push(word);
            }
        }
    }
    terms
}

/// What a budgeted recall gives a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recalled {
    /// The index lines' entries, in the order given.
    pub index: Vec<RecallEntry>,
    /// The items given whole, in the order given.
    pub items: Vec<MemoryItem>,
    /// How many recalled items the index had no room for.
    pub left_out: usize,
}

/// Chooses, from `entries`, what a run is given: the index within
/// `index_bytes`, and the items to give whole within `full_bytes`.
///
/// Guidance comes first, then the most recently changed, then by id. The
/// index lists as many entries as fit; the whole items are those whose
/// words fit what's left of `full_bytes`, in the same order, a large one
/// skipped for smaller ones after it.
#[must_use]
pub fn choose(entries: &[RecallEntry], index_bytes: usize, full_bytes: usize) -> Chosen {
    let mut ordered: Vec<(usize, &RecallEntry)> = entries.iter().enumerate().collect();
    ordered.sort_by(|(_, a), (_, b)| {
        let guidance = |entry: &RecallEntry| entry.kind != Some(MemoryKind::Guidance);
        guidance(a)
            .cmp(&guidance(b))
            .then_with(|| b.updated_at_secs.cmp(&a.updated_at_secs))
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    let mut index = Vec::new();
    let mut used = 0;
    for &(position, entry) in &ordered {
        let line = crate::prompt::index_line(entry).len() + 1;
        if used + line > index_bytes {
            break;
        }
        used += line;
        index.push(position);
    }
    let left_out = ordered.len() - index.len();
    let mut whole = Vec::new();
    let mut room = full_bytes;
    for &(position, entry) in &ordered {
        if entry.body_bytes <= room {
            room -= entry.body_bytes;
            whole.push(position);
        }
    }
    Chosen {
        index,
        whole,
        left_out,
    }
}

/// What [`choose`] chose, by position in the entries it was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// The entries the index lists, in order.
    pub index: Vec<usize>,
    /// The entries given whole, in order.
    pub whole: Vec<usize>,
    /// How many the index had no room for.
    pub left_out: usize,
}

/// Whether `item`, read for an entry, is still to be given at `now_secs`.
#[must_use]
pub fn still_given(item: &MemoryItem, now_secs: u64) -> bool {
    !is_expired(item, now_secs)
}

#[cfg(test)]
#[path = "recall_index_tests.rs"]
mod tests;
