//! Subagent delegation: the protocol for bounded child agents,
//! and the activity events their lifecycle emits.
//!
//! A subagent is not a second agent with ambient authority. It is a
//! scoped execution context the broker starts for a parent session:
//!
//! - **its own cell**: a broker-spawned run cell with its own socket,
//!   never the parent's cell and never a bare process;
//! - **narrowed grants**: what the parent asks to delegate, which must be
//!   a subset of what the parent itself holds (narrowing only);
//! - **budget drawn from the parent**: steps allocated to a child are
//!   spent at allocation, used or not (no refunds, so no laundering
//!   budget through serial children);
//! - **a report back, not a stream**: the parent sees the child's typed
//!   report, never its tool calls.
//!
//! # Raising the depth (threat note)
//!
//! [`MAX_DEPTH`] is 1: a child cannot spawn a child. The bound is
//! structural (a child's session refuses every spawn), not advisory. It
//! must not be raised without answering, for every level:
//!
//! - **Transitive narrowing.** A grandchild's grants must be a subset of
//!   its parent's *delegated* grants, not of the root's: each level
//!   checks against the session it is spawned from, so a child cannot
//!   hand its own child something it was never given.
//! - **Transitive budget monotonicity.** A child's allocation must be
//!   drawn from its own ledger, which is itself a spent allocation of its
//!   parent's. No level may refund upward, and the root's total spend
//!   bounds the whole tree.
//! - **The orphan rule, recursively.** A child's death must reap its
//!   children before its own report is final, so no descendant outlives
//!   the root's run, however the chain ends (finish, kill, timeout).
//! - **Accountability.** Every level's brief hash is logged with its
//!   result hash, so data smuggled down a chain and back up leaves a
//!   trail at each hop, not only at the root.
//!
//! If raising the constant does not require answering these, the bound
//! was theater.

use serde::{Deserialize, Serialize};

use crate::messages::ScheduleGrant;
use crate::ui::{ApiOutcome, ApiRequest};

/// The deepest a subagent may sit: 1, so a child cannot spawn a child.
pub const MAX_DEPTH: u8 = 1;

/// The longest brief a child may be given, in bytes.
pub const MAX_BRIEF_BYTES: usize = 4096;

/// The most steps one child may be allocated.
pub const MAX_CHILD_STEPS: u32 = 32;

/// The most steps all of one parent session's children may be allocated,
/// together. Allocation is spend: this only ever fills.
pub const MAX_LEDGER_STEPS: u32 = 64;

/// The most children one parent session may spawn.
pub const MAX_CHILDREN: u32 = 8;

/// The longest reason a parent may give for a kill, in bytes.
pub const MAX_REASON_BYTES: usize = 256;

/// The reason recorded for a child killed because its parent's run
/// ended: the orphan rule.
pub const ORPHANED: &str = "the parent's run ended";

/// A subagent operation, from a parent session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "op", deny_unknown_fields)]
pub enum SubagentOp {
    /// Start a child on `task`, with `budget` steps drawn from the
    /// parent's and `capabilities` narrowed from the parent's grants.
    #[serde(rename = "spawn")]
    Spawn {
        /// The brief. Untrusted text: it enters the child below its
        /// system prompt, as the child's task.
        task: String,
        /// Steps allocated to the child, spent at allocation.
        budget: u32,
        /// What the child may use: a subset of the parent's grants.
        capabilities: Vec<ScheduleGrant>,
    },
    /// Wait for a child to end, and take its report.
    #[serde(rename = "wait")]
    Wait {
        /// The child id `Spawn` returned.
        child: String,
    },
    /// Kill a running child.
    #[serde(rename = "kill")]
    Kill {
        /// The child id `Spawn` returned.
        child: String,
        /// Why, for the activity record.
        reason: String,
    },
}

/// The broker's reply to one [`SubagentOp`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "result")]
pub enum SubagentResult {
    /// The child was admitted and started.
    #[serde(rename = "spawned")]
    Spawned {
        /// The child's id: its run cell's.
        child: String,
        /// SHA-256 of the brief, logged with the child's result.
        brief_hash: String,
        /// The steps allocated, now spent from the parent's ledger.
        budget: u32,
        /// The grants the child was actually given.
        capabilities: Vec<ScheduleGrant>,
    },
    /// The child ended: its report.
    #[serde(rename = "report")]
    Report(ChildReport),
    /// The op was refused, for a reason in the request itself.
    #[serde(rename = "refused")]
    Refused {
        /// Why.
        reason: SubagentRefusal,
    },
    /// The broker could not complete the op. Opaque by design.
    #[serde(rename = "subagent-error")]
    SubagentError,
}

/// Why a subagent op was refused.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SubagentRefusal {
    /// This session is itself a subagent: depth is bounded at
    /// [`MAX_DEPTH`].
    #[serde(rename = "too-deep")]
    TooDeep,
    /// A requested capability is not one the parent holds.
    #[serde(rename = "exceeds-grants")]
    ExceedsGrants,
    /// The budget is zero or over [`MAX_CHILD_STEPS`].
    #[serde(rename = "bad-budget")]
    BadBudget,
    /// The brief is empty or over [`MAX_BRIEF_BYTES`], or the kill
    /// reason is over [`MAX_REASON_BYTES`].
    #[serde(rename = "bad-task")]
    BadTask,
    /// The parent's ledger cannot cover the budget, or it has spawned
    /// [`MAX_CHILDREN`] already. Nothing is ever refunded.
    #[serde(rename = "ledger-spent")]
    LedgerSpent,
    /// This broker cannot start children (no run environment).
    #[serde(rename = "unavailable")]
    Unavailable,
    /// No child of this session has that id, or its report was taken.
    #[serde(rename = "unknown-child")]
    UnknownChild,
}

/// A child's report to its parent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ChildReport {
    /// The child.
    pub child: String,
    /// The steps it spent, at most its budget.
    pub steps_spent: u32,
    /// How it ended.
    pub outcome: ChildOutcome,
}

/// How a child ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "outcome")]
pub enum ChildOutcome {
    /// It returned normally.
    #[serde(rename = "finished")]
    Finished {
        /// Whether it completed its brief.
        verdict: ChildVerdict,
        /// Its answer, withheld if it would expose a secret.
        answer: String,
        /// SHA-256 of the answer as the child returned it.
        result_hash: String,
    },
    /// Its parent killed it.
    #[serde(rename = "killed")]
    Killed {
        /// The parent's reason.
        reason: String,
        /// What it had produced when it died, if anything.
        partial: Option<String>,
        /// SHA-256 of that partial result.
        partial_hash: Option<String>,
    },
    /// It died, or timed out, on its own.
    #[serde(rename = "failed")]
    Failed {
        /// The typed failure.
        failure: ChildFailure,
        /// Detail, with nothing secret in it.
        detail: String,
    },
}

/// Whether a child completed its brief.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChildVerdict {
    /// It did.
    Ok,
    /// It returned, but not everything the brief asked for.
    Partial,
}

/// Why a child failed on its own.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChildFailure {
    /// It ran past the run time bound.
    Timeout,
    /// Its cell failed, or it ended without a report.
    CellFailed,
    /// It used up its step budget without finishing.
    BudgetExhausted,
}

/// What a child agent prints as its run's output when it runs as a
/// subagent: its own account of how it ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "status")]
pub enum ChildEnvelope {
    /// The loop finished with an answer.
    #[serde(rename = "done")]
    Done {
        /// Whether the brief was completed.
        verdict: ChildVerdict,
        /// The final answer.
        answer: String,
        /// Steps the loop took.
        steps: u32,
    },
    /// The loop ended without an answer.
    #[serde(rename = "failed")]
    Failed {
        /// The typed failure.
        failure: ChildFailure,
        /// Steps the loop took.
        steps: u32,
    },
}

/// One activity event (the activity-monitor contract, v0.1: the subagent
/// lifecycle, and from the local API's calls). Field names and
/// value strings are the contract's.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ActivityEvent {
    /// Unique per event.
    pub event_id: String,
    /// When, RFC 3339.
    pub ts: String,
    /// The parent run that owns the activity thread.
    pub run_id: String,
    /// What happened.
    #[serde(flatten)]
    pub kind: SubagentEvent,
}

/// The activity events: the subagent lifecycle, and the local API's
/// calls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "event")]
pub enum SubagentEvent {
    /// The broker admitted a child.
    #[serde(rename = "subagent_spawned")]
    Spawned {
        /// The child's id: its lane.
        child_id: String,
        /// The parent run.
        parent_run_id: String,
        /// SHA-256 of the exact brief.
        task_brief_hash: String,
        /// Steps allocated, spent at allocation.
        budget_steps: u64,
        /// The narrowed grants actually given.
        capabilities: Vec<String>,
        /// The child's depth.
        depth: u8,
    },
    /// An observable milestone of a child. Best-effort: v0.1 emits none.
    #[serde(rename = "subagent_progress")]
    Progress {
        /// The child.
        child_id: String,
        /// The milestone, already redacted.
        note: String,
        /// Cumulative.
        steps_spent: u64,
    },
    /// A child returned normally.
    #[serde(rename = "subagent_finished")]
    Finished {
        /// The child.
        child_id: String,
        /// SHA-256 of the result, logged next to the brief hash.
        result_hash: String,
        /// Final, at most the budget.
        steps_spent: u64,
        /// Whether it completed its brief.
        verdict: ChildVerdict,
    },
    /// The parent killed a child.
    #[serde(rename = "subagent_killed")]
    Killed {
        /// The child.
        child_id: String,
        /// The parent's reason.
        reason: String,
        /// SHA-256 of the child's partial result, when one exists.
        partial_result_hash: Option<String>,
        /// Steps spent.
        steps_spent: u64,
    },
    /// A child died or timed out on its own.
    #[serde(rename = "subagent_failed")]
    Failed {
        /// The child.
        child_id: String,
        /// The typed failure.
        failure: ChildFailure,
        /// Detail, redacted.
        detail: String,
        /// Steps spent.
        steps_spent: u64,
    },
    /// One call on the local API: who called, with which
    /// grants, what kind of request, and whether the API served it. The
    /// request's content is never recorded.
    #[serde(rename = "api_call")]
    ApiCall {
        /// The caller, as its provider names it.
        caller: String,
        /// The caller's grants, as labels.
        capabilities: Vec<String>,
        /// What kind of request it was.
        request: ApiRequest,
        /// Whether the API served it or refused it.
        outcome: ApiOutcome,
    },
    /// One run of an external coding harness: which harness,
    /// pinned to the exact binary that ran, the brief it was given (by
    /// hash, never its text), how the run ended, and how many files it
    /// changed.
    #[serde(rename = "harness_run")]
    HarnessRun {
        /// The harness skill's name, e.g. `claude-code`.
        harness: String,
        /// SHA-256 of the harness binary that ran: its pinned identity.
        binary_sha256: String,
        /// SHA-256 of the exact brief.
        brief_sha256: String,
        /// How the run ended.
        outcome: HarnessOutcome,
        /// Files the run changed, added or removed.
        files_changed: u32,
        /// What a real CLI sent through its relay: absent for a
        /// harness that ran offline.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<HarnessNetworkUse>,
    },
}

/// What a real CLI's run sent through its relay. Its requests
/// are not confirmed one by one: they go to its vendor's API and nowhere
/// else, and are counted here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HarnessNetworkUse {
    /// The vendor destination, `host:port`.
    pub destination: String,
    /// Requests the relay forwarded to it.
    pub requests: u32,
    /// Requests the relay answered itself and never forwarded.
    pub refused: u32,
}

/// How a harness run ended.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum HarnessOutcome {
    /// The harness finished its task.
    Done,
    /// The harness stopped to ask a question; its answer comes in the
    /// next brief.
    NeedsClarification,
    /// The harness exited with a failure, or wrote more than it may.
    Failed,
    /// The harness ran out of time and was killed.
    TimedOut,
    /// Hatchling refused the run before it started: a bad brief, or a
    /// harness binary that is not the pinned one.
    Refused,
}

/// A grant's label in an activity event: its kind and fields, joined
/// with `:`.
#[must_use]
pub fn grant_label(grant: &ScheduleGrant) -> String {
    match grant {
        ScheduleGrant::Vault { action, name } => format!("vault:{action}:{name}"),
        ScheduleGrant::Egress { destination } => format!("egress:{destination}"),
        ScheduleGrant::Memory { scope, action } => format!("memory:{scope}:{action}"),
        ScheduleGrant::Goals { action } => format!("goals:{action}"),
        ScheduleGrant::ChannelSend { channel } => format!("channel-send:{channel}"),
    }
}

/// The grant a [`grant_label`] names, if it names one: the form a
/// model writes capabilities in when it delegates.
#[must_use]
pub fn parse_grant_label(label: &str) -> Option<ScheduleGrant> {
    let (kind, rest) = label.split_once(':')?;
    let field = |text: &str| (!text.is_empty()).then(|| text.to_owned());
    let pair = |text: &str| {
        let (first, second) = text.split_once(':')?;
        Some((field(first)?, field(second)?))
    };
    match kind {
        "vault" => pair(rest).map(|(action, name)| ScheduleGrant::Vault { action, name }),
        "egress" => field(rest).map(|destination| ScheduleGrant::Egress { destination }),
        "memory" => pair(rest).map(|(scope, action)| ScheduleGrant::Memory { scope, action }),
        "goals" => field(rest).map(|action| ScheduleGrant::Goals { action }),
        "channel-send" => field(rest).map(|channel| ScheduleGrant::ChannelSend { channel }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn wire<T: Serialize>(value: &T) -> Value {
        serde_json::to_value(value).unwrap()
    }

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        serde_json::from_value(wire(value)).unwrap()
    }

    fn egress() -> ScheduleGrant {
        ScheduleGrant::Egress {
            destination: String::from("api.example.com:443"),
        }
    }

    #[test]
    fn the_bounds_are_pinned() {
        assert_eq!((MAX_DEPTH, MAX_BRIEF_BYTES, MAX_CHILD_STEPS), (1, 4096, 32));
        assert_eq!(
            (MAX_LEDGER_STEPS, MAX_CHILDREN, MAX_REASON_BYTES),
            (64, 8, 256)
        );
    }

    #[test]
    fn subagent_op_wire_forms_are_pinned() {
        let spawn = SubagentOp::Spawn {
            task: String::from("summarize"),
            budget: 4,
            capabilities: vec![egress()],
        };
        assert_eq!(
            wire(&spawn),
            json!({"op": "spawn", "task": "summarize", "budget": 4,
                   "capabilities": [{"grant": "egress", "destination": "api.example.com:443"}]})
        );
        let wait = SubagentOp::Wait {
            child: String::from("run-1"),
        };
        assert_eq!(wire(&wait), json!({"op": "wait", "child": "run-1"}));
        let kill = SubagentOp::Kill {
            child: String::from("run-1"),
            reason: String::from("enough"),
        };
        assert_eq!(
            wire(&kill),
            json!({"op": "kill", "child": "run-1", "reason": "enough"})
        );
        for op in [spawn, wait, kill] {
            assert_eq!(round_trip(&op), op);
        }
    }

    #[test]
    fn subagent_result_wire_forms_are_pinned() {
        let spawned = SubagentResult::Spawned {
            child: String::from("run-1"),
            brief_hash: String::from("ab"),
            budget: 4,
            capabilities: vec![egress()],
        };
        assert_eq!(
            wire(&spawned),
            json!({"result": "spawned", "child": "run-1", "brief_hash": "ab", "budget": 4,
                   "capabilities": [{"grant": "egress", "destination": "api.example.com:443"}]})
        );
        let finished = SubagentResult::Report(ChildReport {
            child: String::from("run-1"),
            steps_spent: 2,
            outcome: ChildOutcome::Finished {
                verdict: ChildVerdict::Ok,
                answer: String::from("done"),
                result_hash: String::from("cd"),
            },
        });
        assert_eq!(
            wire(&finished),
            json!({"result": "report", "child": "run-1", "steps_spent": 2,
                   "outcome": {"outcome": "finished", "verdict": "ok", "answer": "done", "result_hash": "cd"}})
        );
        let killed = ChildOutcome::Killed {
            reason: String::from("enough"),
            partial: None,
            partial_hash: None,
        };
        assert_eq!(
            wire(&killed),
            json!({"outcome": "killed", "reason": "enough", "partial": null, "partial_hash": null})
        );
        let failed = ChildOutcome::Failed {
            failure: ChildFailure::BudgetExhausted,
            detail: String::from("d"),
        };
        assert_eq!(
            wire(&failed),
            json!({"outcome": "failed", "failure": "budget_exhausted", "detail": "d"})
        );
        let refused = SubagentResult::Refused {
            reason: SubagentRefusal::TooDeep,
        };
        assert_eq!(
            wire(&refused),
            json!({"result": "refused", "reason": "too-deep"})
        );
        assert_eq!(
            wire(&SubagentResult::SubagentError),
            json!({"result": "subagent-error"})
        );
        for result in [spawned, finished, refused, SubagentResult::SubagentError] {
            assert_eq!(round_trip(&result), result);
        }
        let reasons = [
            (SubagentRefusal::TooDeep, "too-deep"),
            (SubagentRefusal::ExceedsGrants, "exceeds-grants"),
            (SubagentRefusal::BadBudget, "bad-budget"),
            (SubagentRefusal::BadTask, "bad-task"),
            (SubagentRefusal::LedgerSpent, "ledger-spent"),
            (SubagentRefusal::Unavailable, "unavailable"),
            (SubagentRefusal::UnknownChild, "unknown-child"),
        ];
        for (reason, name) in reasons {
            assert_eq!(wire(&reason), json!(name));
        }
        for (failure, name) in [
            (ChildFailure::Timeout, "timeout"),
            (ChildFailure::CellFailed, "cell_failed"),
            (ChildFailure::BudgetExhausted, "budget_exhausted"),
        ] {
            assert_eq!(wire(&failure), json!(name));
        }
        assert_eq!(wire(&ChildVerdict::Partial), json!("partial"));
    }

    #[test]
    fn a_child_envelope_wire_form_is_pinned() {
        let done = ChildEnvelope::Done {
            verdict: ChildVerdict::Ok,
            answer: String::from("a"),
            steps: 3,
        };
        assert_eq!(
            wire(&done),
            json!({"status": "done", "verdict": "ok", "answer": "a", "steps": 3})
        );
        let failed = ChildEnvelope::Failed {
            failure: ChildFailure::BudgetExhausted,
            steps: 5,
        };
        assert_eq!(
            wire(&failed),
            json!({"status": "failed", "failure": "budget_exhausted", "steps": 5})
        );
        assert_eq!(round_trip(&done), done);
    }

    fn event(kind: SubagentEvent) -> ActivityEvent {
        ActivityEvent {
            event_id: String::from("ev-1"),
            ts: String::from("2026-10-03T12:00:00Z"),
            run_id: String::from("parent"),
            kind,
        }
    }

    #[test]
    fn each_lifecycle_event_has_exactly_the_contracts_fields() {
        let cases = [
            (
                event(SubagentEvent::Spawned {
                    child_id: String::from("run-1"),
                    parent_run_id: String::from("parent"),
                    task_brief_hash: String::from("ab"),
                    budget_steps: 4,
                    capabilities: vec![String::from("egress:api.example.com:443")],
                    depth: 1,
                }),
                json!({"event": "subagent_spawned", "event_id": "ev-1", "ts": "2026-10-03T12:00:00Z",
                       "run_id": "parent", "child_id": "run-1", "parent_run_id": "parent",
                       "task_brief_hash": "ab", "budget_steps": 4,
                       "capabilities": ["egress:api.example.com:443"], "depth": 1}),
            ),
            (
                event(SubagentEvent::Progress {
                    child_id: String::from("run-1"),
                    note: String::from("n"),
                    steps_spent: 1,
                }),
                json!({"event": "subagent_progress", "event_id": "ev-1", "ts": "2026-10-03T12:00:00Z",
                       "run_id": "parent", "child_id": "run-1", "note": "n", "steps_spent": 1}),
            ),
            (
                event(SubagentEvent::Finished {
                    child_id: String::from("run-1"),
                    result_hash: String::from("cd"),
                    steps_spent: 2,
                    verdict: ChildVerdict::Ok,
                }),
                json!({"event": "subagent_finished", "event_id": "ev-1", "ts": "2026-10-03T12:00:00Z",
                       "run_id": "parent", "child_id": "run-1", "result_hash": "cd",
                       "steps_spent": 2, "verdict": "ok"}),
            ),
            (
                event(SubagentEvent::Killed {
                    child_id: String::from("run-1"),
                    reason: String::from("enough"),
                    partial_result_hash: None,
                    steps_spent: 4,
                }),
                json!({"event": "subagent_killed", "event_id": "ev-1", "ts": "2026-10-03T12:00:00Z",
                       "run_id": "parent", "child_id": "run-1", "reason": "enough",
                       "partial_result_hash": null, "steps_spent": 4}),
            ),
            (
                event(SubagentEvent::Failed {
                    child_id: String::from("run-1"),
                    failure: ChildFailure::Timeout,
                    detail: String::from("d"),
                    steps_spent: 4,
                }),
                json!({"event": "subagent_failed", "event_id": "ev-1", "ts": "2026-10-03T12:00:00Z",
                       "run_id": "parent", "child_id": "run-1", "failure": "timeout",
                       "detail": "d", "steps_spent": 4}),
            ),
        ];
        for (event, expected) in cases {
            assert_eq!(wire(&event), expected);
            assert_eq!(round_trip(&event), event);
        }
    }

    #[test]
    fn a_grant_is_labeled_by_its_kind_and_fields() {
        let labels = [
            (
                ScheduleGrant::Vault {
                    action: String::from("fetch"),
                    name: String::from("k"),
                },
                "vault:fetch:k",
            ),
            (egress(), "egress:api.example.com:443"),
            (
                ScheduleGrant::Memory {
                    scope: String::from("user"),
                    action: String::from("recall"),
                },
                "memory:user:recall",
            ),
            (
                ScheduleGrant::Goals {
                    action: String::from("query-goals"),
                },
                "goals:query-goals",
            ),
            (
                ScheduleGrant::ChannelSend {
                    channel: String::from("local"),
                },
                "channel-send:local",
            ),
        ];
        for (grant, label) in labels {
            assert_eq!(grant_label(&grant), label);
            assert_eq!(parse_grant_label(label), Some(grant));
        }
        for bad in [
            "",
            "vault",
            "vault:",
            "vault:fetch",
            "vault:fetch:",
            "vault::k",
            "egress:",
            "memory:user",
            "memory::recall",
            "goals:",
            "channel-send:",
            "shell:rm",
        ] {
            assert_eq!(parse_grant_label(bad), None, "{bad:?}");
        }
    }
}
