//! The memory loop's claims (WO-058), and the checks they pass.
//!
//! What the loop's seat may propose about the owner's memory, checked
//! before the broker decides what's applied, what waits for the owner,
//! and what's dropped.
//!
//! Four claims, each citing the records it came from:
//! - **merge:** memories saying the same thing become one. It names the
//!   memory whose words stay, never new words, so a merge can't reword.
//! - **correct:** a memory the owner's later words contradict, and what's
//!   true now.
//! - **check:** a memory that may no longer be so, put to the owner.
//! - **keep:** something durable the owner said that no memory holds.
//!
//! [`check`] holds an output to [`seat::check`] and then to the claims'
//! own shape: every memory a claim targets must be one the seat was
//! given, and a merge must name at least two, the one kept among them.
//! A claim's words are bounded and never empty.
//!
//! What applies is the broker's rule, not this module's: this only says
//! an output is well formed and grounded in the snapshot.

use serde::Deserialize;

use crate::seat::{self, Checked, Cited, Claim, Finish, Handle, Origin, Refused, Snapshot};

/// The longest a claim's words, or a check's reason, is.
pub const MAX_WORDS_BYTES: usize = 2048;

/// The most memories one merge may name.
pub const MAX_MERGED: usize = 8;

/// What a memory kept by the loop is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The owner.
    Owner,
    /// How the owner wants the agent to work.
    Guidance,
    /// Ongoing work.
    Work,
    /// Where something lives.
    Pointer,
}

/// One of the memory loop's claims.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "claim", rename_all = "snake_case", deny_unknown_fields)]
pub enum LoopClaim {
    /// `memories` say the same thing: `keep`'s words stay, and the others
    /// are retired into it.
    Merge {
        /// The duplicates, `keep` among them.
        memories: Vec<String>,
        /// The one whose words stay.
        keep: String,
        /// Where it came from.
        cites: Vec<Cited>,
    },
    /// `memory` is contradicted by the owner's later words: `words` is
    /// what's true now.
    Correct {
        /// The memory corrected.
        memory: String,
        /// What's true now.
        words: String,
        /// Where it came from.
        cites: Vec<Cited>,
    },
    /// `memory` may no longer be so: ask the owner, for `why`.
    Check {
        /// The memory to ask about.
        memory: String,
        /// What makes it doubtful.
        why: String,
        /// Where it came from.
        cites: Vec<Cited>,
    },
    /// The owner said `words`, and no memory holds it.
    Keep {
        /// What to keep.
        words: String,
        /// What it's about.
        kind: Kind,
        /// Why, in the owner's terms: guidance and work say it.
        #[serde(default)]
        why: Option<String>,
        /// When it applies: guidance and work say it.
        #[serde(default)]
        how_to_apply: Option<String>,
        /// Where it came from.
        cites: Vec<Cited>,
    },
}

impl Claim for LoopClaim {
    fn cites(&self) -> &[Cited] {
        match self {
            Self::Merge { cites, .. }
            | Self::Correct { cites, .. }
            | Self::Check { cites, .. }
            | Self::Keep { cites, .. } => cites,
        }
    }
}

/// Whether `said` is a memory `snapshot` holds.
fn memory_in(said: &str, snapshot: &Snapshot) -> Result<Handle, String> {
    let handle: Handle = said
        .parse()
        .map_err(|error| format!("{said:?} isn't a handle: {error}"))?;
    if !matches!(handle, Handle::Memory(_)) {
        return Err(format!("{handle} isn't a memory"));
    }
    if snapshot.get(&handle).is_none() {
        return Err(format!("{handle} isn't among the records you were given"));
    }
    Ok(handle)
}

/// Whether `words` are words a claim may carry: `what` names them.
fn words_fit(words: &str, what: &str) -> Result<(), String> {
    if words.trim().is_empty() {
        return Err(format!("its {what} are empty"));
    }
    if words.len() > MAX_WORDS_BYTES {
        return Err(format!("its {what} are over {MAX_WORDS_BYTES} bytes"));
    }
    Ok(())
}

impl LoopClaim {
    /// Why its targets and words aren't what its kind needs, if they
    /// aren't.
    fn shape(&self, snapshot: &Snapshot) -> Result<(), String> {
        match self {
            Self::Merge { memories, keep, .. } => {
                if memories.len() < 2 {
                    return Err(String::from("a merge names at least two memories"));
                }
                if memories.len() > MAX_MERGED {
                    return Err(format!("a merge names at most {MAX_MERGED} memories"));
                }
                let mut seen = Vec::with_capacity(memories.len());
                for said in memories {
                    let handle = memory_in(said, snapshot)?;
                    if seen.contains(&handle) {
                        return Err(format!("{handle} is named twice"));
                    }
                    seen.push(handle);
                }
                let kept = memory_in(keep, snapshot)?;
                if !seen.contains(&kept) {
                    return Err(format!("{kept}, the one kept, isn't among its memories"));
                }
                Ok(())
            }
            Self::Correct { memory, words, .. } => {
                memory_in(memory, snapshot)?;
                words_fit(words, "words")
            }
            Self::Check { memory, why, .. } => {
                memory_in(memory, snapshot)?;
                words_fit(why, "reasons")
            }
            Self::Keep { words, .. } => words_fit(words, "words"),
        }
    }
}

/// Whether one of `cites` is the owner's own record, quoted.
#[must_use]
pub fn quotes_the_owner(cites: &[Cited], snapshot: &Snapshot) -> bool {
    cites.iter().any(|cited| {
        cited.quote.is_some()
            && cited
                .handle
                .parse::<Handle>()
                .ok()
                .and_then(|handle| snapshot.get(&handle))
                .is_some_and(|record| record.origin == Origin::Owner)
    })
}

/// Checks `output`, the memory seat's `finish` JSON, against `snapshot`:
/// [`seat::check`], then each claim's own shape.
///
/// # Errors
///
/// [`Refused`], with every reason from both, when either refuses it.
pub fn check(output: &str, snapshot: &Snapshot) -> Result<Checked<LoopClaim>, Refused> {
    let checked = seat::check::<LoopClaim>(output, snapshot);
    let mut reasons = match &checked {
        Ok(_) => Vec::new(),
        Err(Refused(reasons)) => reasons.clone(),
    };
    if let Ok(finish) = serde_json::from_str::<Finish<LoopClaim>>(output) {
        for (at, claim) in finish.claims.iter().enumerate().take(seat::MAX_CLAIMS) {
            if let Err(reason) = claim.shape(snapshot) {
                reasons.push(format!("claim {at}: {reason}"));
            }
        }
    }
    if reasons.is_empty() {
        checked
    } else {
        reasons.truncate(seat::MAX_REASONS);
        Err(Refused(reasons))
    }
}

#[cfg(test)]
#[path = "memory_loop_tests.rs"]
mod tests;
