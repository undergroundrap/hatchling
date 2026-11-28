//! The memory loop (WO-058): what its seat is given of the owner's memory
//! and conversation, and the broker's rule for each claim it hands back.
//!
//! The seat only proposes ([`hatchling_core::memory_loop`]). Here:
//! - **The snapshot.** Each memory the agent recalls, under
//!   `memory:<scope>/<id>`, with a [`fingerprint`] apply rechecks: the
//!   owner's words when they approved it, the agent's when it was kept
//!   automatically. Then the conversation the loop hasn't read yet, each
//!   exchange as the owner's message and the agent's answer.
//! - **The rule** ([`judge`]), for each claim the seat check passed:
//!   - a claim whose every source is outside content is dropped;
//!   - a merge of memories of different kinds is dropped; one of a kind
//!     applies, keeping one duplicate's own words;
//!   - a correct or a keep must quote the owner's own message, and its
//!     words pass the check the agent's own proposals do, hold no secret
//!     the vault holds, and aren't words the owner said no to; otherwise
//!     it's dropped;
//!   - a correct of a memory the owner approved, about them or how they
//!     want the agent to work, waits for them;
//!   - a keep from a conversation that read from outside waits, as does
//!     one past the day's automatic keeps;
//!   - a check applies: it only sets the day to ask.
//!
//! Nothing here writes. Apply is slice 2's.

use std::collections::BTreeMap;

use hatchling_core::conversation::ChatExchange;
use hatchling_core::memory_loop::{Kind, LoopClaim, quotes_the_owner};
use hatchling_core::seat::{
    Checked, Handle, MAX_RECORD_BYTES, MAX_RECORDS, Origin, Record, Snapshot,
};
use hatchling_memory::{MemoryItem, MemoryKind};
use sha2::{Digest as _, Sha256};

use crate::memory_proposal::{Asked, Replaced, Unfit};

/// The loop's name, on the background clock and in the owner's settings.
pub const LOOP: &str = "memory";

/// How long after the owner's last chat the loop runs: an hour.
pub const QUIET_SECS: i64 = 3600;

/// The most memories a snapshot holds: the rest of its records are the
/// conversation's.
pub const MAX_MEMORIES: usize = 192;

/// The most a snapshot's records come to, as the seat's cell is handed them.
///
/// It's under the 128 KiB Linux allows one environment string, and about
/// what a small model's window holds. Memories take at most half.
pub const MAX_SNAPSHOT_BYTES: usize = 98_304;

/// What `record` costs of [`MAX_SNAPSHOT_BYTES`]: its fields as JSON
/// writes them, and their keys.
fn cost(record: &Record) -> usize {
    let text = serde_json::to_string(&record.text).map_or(usize::MAX, |json| json.len());
    text.saturating_add(record.handle.to_string().len())
        .saturating_add(record.fingerprint.len())
        .saturating_add(64)
}

/// A memory's fingerprint: SHA-256 over its words, its tags and its
/// record, as hex. Anything the owner or the broker changes about it
/// changes this.
#[must_use]
pub fn fingerprint(item: &MemoryItem) -> String {
    let body = serde_json::to_vec(&(&item.content, &item.tags, &item.record)).unwrap_or_default();
    crate::approval::hex_token("", &Sha256::digest(body))
}

/// A message's fingerprint: SHA-256 over its words.
fn said_fingerprint(text: &str) -> String {
    crate::approval::hex_token("", &Sha256::digest(text.as_bytes()))
}

/// `text` within a record's bound, cut at a character's edge.
fn bounded(text: &str) -> String {
    text.chars()
        .scan(0_usize, |used, c| {
            *used = used.saturating_add(c.len_utf8());
            (*used <= MAX_RECORD_BYTES).then_some(c)
        })
        .collect()
}

/// What a snapshot was made of: the memories by handle, for the rule
/// and for apply, and the last exchange it read.
#[derive(Debug, Clone, Default)]
pub struct Given {
    /// Each memory, by its handle, with its listed id.
    pub memories: BTreeMap<Handle, (String, MemoryItem)>,
    /// The slot of the last exchange read, if any was.
    pub read_to: Option<u64>,
}

/// The snapshot of `memories` and of `exchanges` in `conversation`.
///
/// Each memory comes with its listed id, `<scope>/<id>`, and each exchange
/// with its slot, the oldest first, all after the last the loop read.
/// Within the seat's bounds: at most [`MAX_MEMORIES`] memories, the
/// newest kept, within half of [`MAX_SNAPSHOT_BYTES`], and as many of the
/// oldest exchanges as the rest allows, so those left are read next time.
#[must_use]
pub fn snapshot(
    memories: &[(String, MemoryItem)],
    conversation: &str,
    exchanges: &[(u64, ChatExchange)],
) -> (Snapshot, Given) {
    let mut newest: Vec<&(String, MemoryItem)> = memories.iter().collect();
    newest.sort_by(|a, b| {
        b.1.updated_at_secs
            .cmp(&a.1.updated_at_secs)
            .then_with(|| a.0.cmp(&b.0))
    });
    let mut records = Vec::new();
    let mut given = Given::default();
    let mut spent = 0_usize;
    for (id, item) in newest.into_iter().take(MAX_MEMORIES) {
        let Ok(handle) = format!("memory:{id}").parse::<Handle>() else {
            continue;
        };
        if given.memories.contains_key(&handle) {
            continue;
        }
        let record = Record {
            handle: handle.clone(),
            fingerprint: fingerprint(item),
            origin: if item.approved_by_owner() {
                Origin::Owner
            } else {
                Origin::Agent
            },
            text: bounded(&item.content),
        };
        let costs = spent.saturating_add(cost(&record));
        if costs > MAX_SNAPSHOT_BYTES / 2 {
            break;
        }
        spent = costs;
        records.push(record);
        given.memories.insert(handle, (id.clone(), item.clone()));
    }
    for (slot, exchange) in exchanges {
        if records.len() + 2 > MAX_RECORDS {
            break;
        }
        let said = [
            ("owner", Origin::Owner, &exchange.user),
            ("agent", Origin::Agent, &exchange.assistant),
        ];
        let handles: Vec<_> = said
            .iter()
            .map(|(side, _, _)| format!("message:{conversation}/{slot}-{side}").parse::<Handle>())
            .collect();
        if handles.iter().any(Result::is_err) {
            break;
        }
        let pair: Vec<Record> = said
            .into_iter()
            .zip(handles.into_iter().flatten())
            .map(|((_, origin, text), handle)| Record {
                handle,
                fingerprint: said_fingerprint(text),
                origin,
                text: bounded(text),
            })
            .collect();
        let costs = pair
            .iter()
            .fold(spent, |sum, record| sum.saturating_add(cost(record)));
        if costs > MAX_SNAPSHOT_BYTES {
            break;
        }
        spent = costs;
        records.extend(pair);
        given.read_to = Some(*slot);
    }
    // Every handle is unique by construction: memories by their listed id,
    // messages by slot and side.
    let snapshot = Snapshot::new(records).unwrap_or_default();
    (snapshot, given)
}

/// What's true of the run that the rule weighs, beyond the snapshot.
pub struct Facts<'a> {
    /// What the snapshot was made of.
    pub given: &'a Given,
    /// The conversation read from outside.
    pub tainted: bool,
    /// How many more may be kept automatically today.
    pub keeps_left: usize,
    /// Today, in Unix seconds, for the proposal check.
    pub now: u64,
    /// Whether words hold a secret the vault holds.
    pub holds_secret: &'a dyn Fn(&str) -> bool,
    /// Whether the owner said no to keeping these words.
    pub refused: &'a dyn Fn(&crate::memory_proposal::Checked) -> bool,
}

/// Why a claim waits for the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waits {
    /// It corrects what the owner approved about them, or how they want
    /// the agent to work.
    OwnersOwn,
    /// It keeps something from a conversation that read from outside.
    ReadFromOutside,
    /// The day's automatic keeps are spent.
    DayFull,
}

/// Why a claim is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dropped {
    /// Its every source is outside content.
    OutsideOnly,
    /// It doesn't quote the owner's own message.
    NotTheOwners,
    /// It merges memories of different kinds.
    DifferentKinds,
    /// Its words don't pass the proposal check.
    Unfit,
    /// Its words hold a secret the vault holds.
    Secret,
    /// The owner said no to these words.
    Refused,
    /// A memory already says it.
    AlreadyKept,
    /// It names a memory the snapshot wasn't made of.
    Unknown,
}

/// What the rule makes of one claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Applied without asking.
    Apply,
    /// Waits for the owner.
    Wait(Waits),
    /// Never applied.
    Drop(Dropped),
}

/// One claim, as the rule judged it: for a correct or a keep, the
/// proposal apply keeps, or puts to the owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    /// The claim.
    pub claim: LoopClaim,
    /// What's made of it.
    pub verdict: Verdict,
    /// What a correct or a keep would keep.
    pub proposal: Option<crate::memory_proposal::Checked>,
}

/// What `kind` is called where the proposal check reads it.
const fn kind_said(kind: Option<MemoryKind>) -> &'static str {
    match kind {
        // A memory kept before kinds were is the owner's: the strictest.
        Some(MemoryKind::Owner) | None => "owner",
        Some(MemoryKind::Guidance) => "guidance",
        Some(MemoryKind::Work) => "work",
        Some(MemoryKind::Pointer) => "pointer",
    }
}

/// What a keep's kind is called where the proposal check reads it.
const fn keep_said(kind: Kind) -> &'static str {
    match kind {
        Kind::Owner => "owner",
        Kind::Guidance => "guidance",
        Kind::Work => "work",
        Kind::Pointer => "pointer",
    }
}

/// The memory `said` names, if the snapshot was made of it.
fn memory<'a>(said: &str, facts: &Facts<'a>) -> Option<&'a (String, MemoryItem)> {
    facts.given.memories.get(&said.parse::<Handle>().ok()?)
}

/// `asked`, through the proposal check and the owner's refusals.
fn proposal_of(
    asked: &Asked<'_>,
    facts: &Facts<'_>,
) -> Result<crate::memory_proposal::Checked, Dropped> {
    let proposal =
        crate::memory_proposal::check(asked, facts.now).map_err(|unfit| match unfit {
            Unfit::Words | Unfit::Said(_) => Dropped::Unfit,
        })?;
    if (facts.holds_secret)(&proposal.content) {
        return Err(Dropped::Secret);
    }
    if (facts.refused)(&proposal) {
        return Err(Dropped::Refused);
    }
    Ok(proposal)
}

/// What the rule makes of a claim: its verdict, and for a correct or a
/// keep, what it would keep.
type Ruling = (Verdict, Option<crate::memory_proposal::Checked>);

/// The rule for a merge of `memories`: one kind, or dropped.
fn judge_merge(memories: &[String], facts: &Facts<'_>) -> Ruling {
    let mut kinds = Vec::with_capacity(memories.len());
    for said in memories {
        let Some((_, item)) = memory(said, facts) else {
            return (Verdict::Drop(Dropped::Unknown), None);
        };
        kinds.push(item.record.as_ref().and_then(|record| record.kind));
    }
    if kinds.iter().any(|kind| Some(kind) != kinds.first()) {
        return (Verdict::Drop(Dropped::DifferentKinds), None);
    }
    (Verdict::Apply, None)
}

/// The rule for a correct of `said` to `words`: the owner's own words, and
/// what they approved about them, or how they want it to work, waits.
fn judge_correct(
    said: &str,
    words: &str,
    cites: &[hatchling_core::seat::Cited],
    snapshot: &Snapshot,
    facts: &Facts<'_>,
) -> Ruling {
    let Some((id, old)) = memory(said, facts) else {
        return (Verdict::Drop(Dropped::Unknown), None);
    };
    if !quotes_the_owner(cites, snapshot) {
        return (Verdict::Drop(Dropped::NotTheOwners), None);
    }
    let record = old.record.as_ref();
    let kind = record.and_then(|record| record.kind);
    let asked = Asked {
        content: words,
        kind: Some(kind_said(kind)),
        why: record.and_then(|record| record.why.as_deref()),
        how_to_apply: record.and_then(|record| record.how_to_apply.as_deref()),
        check_by: None,
        absence: record.is_some_and(|record| record.absence),
        supersedes: None,
    };
    let mut proposal = match proposal_of(&asked, facts) {
        Ok(proposal) => proposal,
        Err(dropped) => return (Verdict::Drop(dropped), None),
    };
    proposal.supersedes = Some(Replaced {
        id: id.clone(),
        content: old.content.clone(),
    });
    let owners_own = old.approved_by_owner()
        && matches!(kind, None | Some(MemoryKind::Owner | MemoryKind::Guidance));
    let verdict = if owners_own {
        Verdict::Wait(Waits::OwnersOwn)
    } else {
        Verdict::Apply
    };
    (verdict, Some(proposal))
}

/// The rule for a keep of `asked`, citing `cites`: the owner's own words,
/// from a clean conversation, inside the day's.
fn judge_keep(
    asked: &Asked<'_>,
    cites: &[hatchling_core::seat::Cited],
    snapshot: &Snapshot,
    facts: &Facts<'_>,
    keeps_left: &mut usize,
) -> Ruling {
    if !quotes_the_owner(cites, snapshot) {
        return (Verdict::Drop(Dropped::NotTheOwners), None);
    }
    let proposal = match proposal_of(asked, facts) {
        Ok(proposal) => proposal,
        Err(dropped) => return (Verdict::Drop(dropped), None),
    };
    if facts
        .given
        .memories
        .values()
        .any(|(_, item)| item.content.trim() == proposal.content)
    {
        return (Verdict::Drop(Dropped::AlreadyKept), None);
    }
    let verdict = if facts.tainted {
        Verdict::Wait(Waits::ReadFromOutside)
    } else if *keeps_left == 0 {
        Verdict::Wait(Waits::DayFull)
    } else {
        *keeps_left -= 1;
        Verdict::Apply
    };
    (verdict, Some(proposal))
}

/// The rule for one grounded claim.
fn judge_one(
    claim: &LoopClaim,
    snapshot: &Snapshot,
    facts: &Facts<'_>,
    keeps_left: &mut usize,
) -> Ruling {
    match claim {
        LoopClaim::Merge { memories, .. } => judge_merge(memories, facts),
        LoopClaim::Check { memory: said, .. } => {
            if memory(said, facts).is_none() {
                return (Verdict::Drop(Dropped::Unknown), None);
            }
            (Verdict::Apply, None)
        }
        LoopClaim::Correct {
            memory: said,
            words,
            cites,
        } => judge_correct(said, words, cites, snapshot, facts),
        LoopClaim::Keep {
            words,
            kind,
            why,
            how_to_apply,
            cites,
        } => {
            let asked = Asked {
                content: words,
                kind: Some(keep_said(*kind)),
                why: why.as_deref(),
                how_to_apply: how_to_apply.as_deref(),
                check_by: None,
                absence: false,
                supersedes: None,
            };
            judge_keep(&asked, cites, snapshot, facts, keeps_left)
        }
    }
}

/// The rule for each claim in `checked`, in the order apply takes them:
/// merges, corrections, keeps, then checks; those outside only last.
#[must_use]
pub fn judge(checked: Checked<LoopClaim>, snapshot: &Snapshot, facts: &Facts<'_>) -> Vec<Judged> {
    let order = |claim: &LoopClaim| match claim {
        LoopClaim::Merge { .. } => 0,
        LoopClaim::Correct { .. } => 1,
        LoopClaim::Keep { .. } => 2,
        LoopClaim::Check { .. } => 3,
    };
    let mut grounded = checked.grounded;
    grounded.sort_by_key(order);
    let mut keeps_left = facts.keeps_left;
    let mut judged: Vec<Judged> = grounded
        .into_iter()
        .map(|claim| {
            let (verdict, proposal) = judge_one(&claim, snapshot, facts, &mut keeps_left);
            Judged {
                claim,
                verdict,
                proposal,
            }
        })
        .collect();
    judged.extend(checked.outside_only.into_iter().map(|claim| Judged {
        claim,
        verdict: Verdict::Drop(Dropped::OutsideOnly),
        proposal: None,
    }));
    judged
}

/// What a run's claims came to, as the gate records it: counts, never
/// text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Claims that would apply.
    pub apply: usize,
    /// Claims that would wait for the owner.
    pub wait: usize,
    /// Claims dropped.
    pub dropped: usize,
}

impl Tally {
    /// The tally of `judged`.
    #[must_use]
    pub fn of(judged: &[Judged]) -> Self {
        let mut tally = Self::default();
        for each in judged {
            match each.verdict {
                Verdict::Apply => tally.apply += 1,
                Verdict::Wait(_) => tally.wait += 1,
                Verdict::Drop(_) => tally.dropped += 1,
            }
        }
        tally
    }
}

#[cfg(test)]
#[path = "memory_loop_tests.rs"]
mod tests;
