//! A background run's seat (WO-054): what a model step is given, how what it
//! hands back is checked, and what the gate records of it.
//!
//! A seat only proposes. It's given a [`Snapshot`] of the records the step
//! may read, each under a [`Handle`] with a fingerprint and an [`Origin`],
//! and it ends with one `finish` call whose JSON is the step's output:
//! [`Finish`], holding claims of the step's own type. Code does the rest:
//!
//! - **The schema.** The output must be exactly the step's type: an unknown
//!   field, a missing one or a wrong type refuses it.
//! - **Every handle resolves.** Each claim cites where it came from, and each
//!   citation must name a record in the snapshot, its quoted words, if any,
//!   in that record's text. One that doesn't refuses the whole output: an
//!   invented id is a confused or steered model, and none of its output is
//!   applied (the threat model's rule).
//! - **One repair.** A refused output is answered once with every reason
//!   ([`Step`]); a second refusal ends the step with nothing.
//! - **No change is a result**, said as such, never as an empty list.
//! - **Outside content applies nothing.** A claim whose every source is
//!   outside content (a page, tool output, someone else's words) is kept
//!   apart ([`Checked::outside_only`]): the loop's apply step queues it, or
//!   drops it, and never applies it.
//!
//! [`GateRecord`] is what the gate keeps of a step: counts and outcomes,
//! never text.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// The longest a handle is.
pub const MAX_HANDLE_BYTES: usize = 512;

/// The most claims one output may make.
pub const MAX_CLAIMS: usize = 64;

/// The most citations one claim may make.
pub const MAX_CITATIONS: usize = 8;

/// The longest a quoted span is.
pub const MAX_QUOTE_BYTES: usize = 1024;

/// The most reasons a refusal says, so the repair turn stays small.
pub const MAX_REASONS: usize = 16;

/// The most records a seat is given.
pub const MAX_RECORDS: usize = 256;

/// The longest a record's text is.
pub const MAX_RECORD_BYTES: usize = 16_384;

/// The longest a seat's prompt or assignment is.
pub const MAX_BRIEF_BYTES: usize = 32_768;

/// Where a claim came from: a record the step was given.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Handle {
    /// A message, in a conversation: `message:<conversation>/<id>`.
    Message {
        /// The conversation.
        conversation: String,
        /// The message.
        id: String,
    },
    /// A memory item: `memory:<id>`.
    Memory(String),
    /// A goal: `goal:<id>`.
    Goal(String),
    /// A run: `run:<id>`.
    Run(String),
    /// A file: `file:<path>`.
    File(String),
}

/// Why a handle isn't one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HandleError {
    /// Over [`MAX_HANDLE_BYTES`].
    #[error("longer than {MAX_HANDLE_BYTES} bytes")]
    TooLong,
    /// Not `kind:id` with a kind this knows.
    #[error("not message:, memory:, goal:, run: or file: and an id")]
    UnknownKind,
    /// An empty id, or one with whitespace or a control character.
    #[error("an empty id, or one with a space or a control character")]
    BadId,
}

/// Whether `id` can be an id: not empty, and no whitespace or control
/// character, so a handle reads back as itself.
fn plain(id: &str) -> bool {
    !id.is_empty() && !id.chars().any(|c| c.is_whitespace() || c.is_control())
}

impl FromStr for Handle {
    type Err = HandleError;

    fn from_str(said: &str) -> Result<Self, HandleError> {
        if said.len() > MAX_HANDLE_BYTES {
            return Err(HandleError::TooLong);
        }
        let (kind, rest) = said.split_once(':').ok_or(HandleError::UnknownKind)?;
        let id = |id: &str| {
            if plain(id) {
                Ok(id.to_owned())
            } else {
                Err(HandleError::BadId)
            }
        };
        match kind {
            "message" => {
                let (conversation, message) = rest.split_once('/').ok_or(HandleError::BadId)?;
                Ok(Self::Message {
                    conversation: id(conversation)?,
                    id: id(message)?,
                })
            }
            "memory" => Ok(Self::Memory(id(rest)?)),
            "goal" => Ok(Self::Goal(id(rest)?)),
            "run" => Ok(Self::Run(id(rest)?)),
            "file" => Ok(Self::File(id(rest)?)),
            _ => Err(HandleError::UnknownKind),
        }
    }
}

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message { conversation, id } => write!(f, "message:{conversation}/{id}"),
            Self::Memory(id) => write!(f, "memory:{id}"),
            Self::Goal(id) => write!(f, "goal:{id}"),
            Self::Run(id) => write!(f, "run:{id}"),
            Self::File(path) => write!(f, "file:{path}"),
        }
    }
}

/// Whose words a record is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The owner's own words.
    Owner,
    /// The agent's.
    Agent,
    /// Outside content: a page, tool output, someone else's message.
    Outside,
}

/// One record a step may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Where it is.
    pub handle: Handle,
    /// Its fingerprint as the snapshot took it, which apply rechecks.
    pub fingerprint: String,
    /// Whose words it is.
    pub origin: Origin,
    /// What it says.
    pub text: String,
}

/// The records a step may read, taken at the run's start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    records: BTreeMap<Handle, Record>,
}

/// A snapshot with two records under one handle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("two records under {0}")]
pub struct Duplicate(pub Handle);

impl Snapshot {
    /// The snapshot of `records`.
    ///
    /// # Errors
    ///
    /// [`Duplicate`] when two share a handle.
    pub fn new(records: impl IntoIterator<Item = Record>) -> Result<Self, Duplicate> {
        let mut kept = BTreeMap::new();
        for record in records {
            let handle = record.handle.clone();
            if kept.insert(handle.clone(), record).is_some() {
                return Err(Duplicate(handle));
            }
        }
        Ok(Self { records: kept })
    }

    /// Every record, by handle.
    pub fn records(&self) -> impl Iterator<Item = &Record> {
        self.records.values()
    }

    /// The record under `handle`, if the step was given it.
    #[must_use]
    pub fn get(&self, handle: &Handle) -> Option<&Record> {
        self.records.get(handle)
    }

    /// Where `cited` came from, or why it can't be said to.
    fn origin_of(&self, cited: &Cited) -> Result<Origin, String> {
        let handle: Handle = cited
            .handle
            .parse()
            .map_err(|error| format!("{:?} isn't a handle: {error}", cited.handle))?;
        let record = self
            .get(&handle)
            .ok_or_else(|| format!("{handle} isn't among the records you were given"))?;
        if let Some(quote) = &cited.quote {
            if quote.len() > MAX_QUOTE_BYTES {
                return Err(format!(
                    "the quote from {handle} is over {MAX_QUOTE_BYTES} bytes"
                ));
            }
            if quote.trim().is_empty() || !record.text.contains(quote.as_str()) {
                return Err(format!("{handle} doesn't say what's quoted from it"));
            }
        }
        Ok(record.origin)
    }
}

/// What the broker hands a seat's cell: its prompt, its assignment, and
/// the records it may read. Nothing else reaches it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packet {
    /// The step's system prompt: the seat's rules and its loop's sections.
    pub prompt: String,
    /// What this step is to do, and the shape of its claims.
    pub assignment: String,
    /// The records it may read.
    pub records: Vec<PacketRecord>,
}

/// One record as a packet carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PacketRecord {
    /// Its handle, as text.
    pub handle: String,
    /// Its fingerprint.
    pub fingerprint: String,
    /// Whose words it is.
    pub origin: Origin,
    /// What it says.
    pub text: String,
}

/// Why a packet can't be a seat's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PacketError {
    /// Over a bound.
    #[error("over a bound: {0}")]
    TooBig(&'static str),
    /// A record's handle isn't one.
    #[error("{handle:?} isn't a handle: {error}")]
    Handle {
        /// What it said.
        handle: String,
        /// Why.
        error: HandleError,
    },
    /// Two records under one handle.
    #[error(transparent)]
    Duplicate(#[from] Duplicate),
}

impl Packet {
    /// The snapshot its records make.
    ///
    /// # Errors
    ///
    /// [`PacketError`] when it breaks a bound, a handle isn't one, or two
    /// records share one.
    pub fn snapshot(&self) -> Result<Snapshot, PacketError> {
        if self.prompt.len() > MAX_BRIEF_BYTES || self.assignment.len() > MAX_BRIEF_BYTES {
            return Err(PacketError::TooBig("the prompt or the assignment"));
        }
        if self.records.len() > MAX_RECORDS {
            return Err(PacketError::TooBig("the records"));
        }
        let mut records = Vec::with_capacity(self.records.len());
        for record in &self.records {
            if record.text.len() > MAX_RECORD_BYTES {
                return Err(PacketError::TooBig("a record's text"));
            }
            let handle = record.handle.parse().map_err(|error| PacketError::Handle {
                handle: record.handle.clone(),
                error,
            })?;
            records.push(Record {
                handle,
                fingerprint: record.fingerprint.clone(),
                origin: record.origin,
                text: record.text.clone(),
            });
        }
        Ok(Snapshot::new(records)?)
    }
}

/// A citation: a handle, and the words quoted from its record, if any.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cited {
    /// The record's handle.
    pub handle: String,
    /// Words its text holds, as they're written there.
    #[serde(default)]
    pub quote: Option<String>,
}

/// A step's own kind of claim: what it proposes, and where it came from.
pub trait Claim: DeserializeOwned {
    /// What it cites.
    fn cites(&self) -> &[Cited];
}

/// What a step's `finish` hands back: its claims, or that there's nothing
/// to change.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finish<C> {
    /// What it proposes.
    #[serde(default = "Vec::new")]
    pub claims: Vec<C>,
    /// Nothing to change: said, not left as an empty list.
    #[serde(default)]
    pub no_change: bool,
}

/// An output as checked: its claims by how they're sourced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked<C> {
    /// Claims with a source in the owner's or the agent's own words.
    pub grounded: Vec<C>,
    /// Claims whose every source is outside content: never applied.
    pub outside_only: Vec<C>,
    /// The step said there's nothing to change.
    pub no_change: bool,
}

/// Why an output was refused: every reason, as the repair turn says them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused(pub Vec<String>);

/// Checks `output`, a `finish` call's JSON, against the step's claim type
/// and `snapshot`.
///
/// # Errors
///
/// [`Refused`], with every reason, when it isn't the step's schema, breaks a
/// bound, says both or neither of claims and no change, or cites anything
/// that doesn't resolve.
pub fn check<C: Claim>(output: &str, snapshot: &Snapshot) -> Result<Checked<C>, Refused> {
    let finish: Finish<C> = serde_json::from_str(output)
        .map_err(|error| Refused(vec![format!("the output isn't the step's schema: {error}")]))?;
    let mut reasons = Vec::new();
    if finish.no_change && !finish.claims.is_empty() {
        reasons.push(String::from(
            "no_change says there's nothing, yet there are claims",
        ));
    }
    if !finish.no_change && finish.claims.is_empty() {
        reasons.push(String::from(
            "no claims: say no_change when there's nothing to change",
        ));
    }
    if finish.claims.len() > MAX_CLAIMS {
        reasons.push(format!("more than {MAX_CLAIMS} claims"));
    }
    let mut origins = Vec::with_capacity(finish.claims.len());
    for (at, claim) in finish.claims.iter().enumerate() {
        let cites = claim.cites();
        if cites.is_empty() {
            reasons.push(format!("claim {at} cites nothing"));
        }
        if cites.len() > MAX_CITATIONS {
            reasons.push(format!("claim {at} cites more than {MAX_CITATIONS}"));
        }
        let mut outside_only = true;
        for cited in cites {
            match snapshot.origin_of(cited) {
                Ok(origin) => outside_only &= origin == Origin::Outside,
                Err(reason) => reasons.push(format!("claim {at}: {reason}")),
            }
        }
        origins.push(outside_only);
    }
    if !reasons.is_empty() {
        reasons.truncate(MAX_REASONS);
        return Err(Refused(reasons));
    }
    let mut checked = Checked {
        grounded: Vec::new(),
        outside_only: Vec::new(),
        no_change: finish.no_change,
    };
    for (claim, outside_only) in finish.claims.into_iter().zip(origins) {
        if outside_only {
            checked.outside_only.push(claim);
        } else {
            checked.grounded.push(claim);
        }
    }
    Ok(checked)
}

/// Where a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Attempt {
    /// Its first output.
    #[default]
    First,
    /// Its one repair.
    Repair,
}

/// What comes of an output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict<C> {
    /// Checked: the step is done.
    Done(Checked<C>),
    /// Refused the first time: the seat is answered with these reasons,
    /// once.
    Repair(Vec<String>),
    /// Refused again: the step ends with nothing.
    Failed(Vec<String>),
}

/// One seat step: its first output, and at most one repair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Step {
    attempt: Attempt,
}

impl Step {
    /// A step that has had no output yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            attempt: Attempt::First,
        }
    }

    /// Where it is.
    #[must_use]
    pub const fn attempt(&self) -> Attempt {
        self.attempt
    }

    /// What comes of `output`, checked against `snapshot`.
    pub fn take<C: Claim>(&mut self, output: &str, snapshot: &Snapshot) -> Verdict<C> {
        match (check(output, snapshot), self.attempt) {
            (Ok(checked), _) => Verdict::Done(checked),
            (Err(Refused(reasons)), Attempt::First) => {
                self.attempt = Attempt::Repair;
                Verdict::Repair(reasons)
            }
            (Err(Refused(reasons)), Attempt::Repair) => Verdict::Failed(reasons),
        }
    }
}

/// How a step ended, as the gate keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    /// It proposed claims.
    Proposed,
    /// Nothing to change.
    NoChange,
    /// Refused twice: nothing.
    Failed,
}

/// What the gate keeps of one step: counts and outcomes, never text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateRecord {
    /// How it ended.
    pub outcome: GateOutcome,
    /// Whether it took its repair.
    pub repaired: bool,
    /// Its claims with a source in the owner's or the agent's words.
    pub grounded: usize,
    /// Its claims sourced only from outside content.
    pub outside_only: usize,
    /// The reasons its last refusal gave.
    pub refused_for: usize,
}

impl GateRecord {
    /// The record of a step that ended in `verdict`, after `attempt`.
    /// `None` while it's still owed its repair.
    #[must_use]
    pub fn of<C>(verdict: &Verdict<C>, attempt: Attempt) -> Option<Self> {
        let repaired = attempt == Attempt::Repair;
        match verdict {
            Verdict::Repair(_) => None,
            Verdict::Failed(reasons) => Some(Self {
                outcome: GateOutcome::Failed,
                repaired,
                grounded: 0,
                outside_only: 0,
                refused_for: reasons.len(),
            }),
            Verdict::Done(checked) => Some(Self {
                outcome: if checked.no_change {
                    GateOutcome::NoChange
                } else {
                    GateOutcome::Proposed
                },
                repaired,
                grounded: checked.grounded.len(),
                outside_only: checked.outside_only.len(),
                refused_for: 0,
            }),
        }
    }
}

#[cfg(test)]
#[path = "seat_tests.rs"]
mod tests;
