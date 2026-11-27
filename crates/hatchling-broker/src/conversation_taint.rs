//! Whether the chat's conversation read anything from outside (WO-057):
//! what decides whether a memory may be kept without the owner's card.
//!
//! The threat is memory poisoning. Words the agent read from a page, a
//! message or a file, kept as the owner's, would steer every later run. A
//! model asked whether a memory is safe can be talked out of it by the
//! words it judges, so the broker decides, from what it served: a memory
//! is kept automatically only from a conversation in which no run used
//! anything that reads from outside ([`outside`]).
//!
//! - **The conversation's, not the run's.** A run that read a page may
//!   write its words into an answer, and a later run in the same
//!   conversation, which used no tool, is given that answer. So once any
//!   run in a conversation read from outside, the conversation stays
//!   tainted until the owner starts a new one. The chat holds the flag
//!   ([`crate::chat_run::Chat`]), and its store keeps it across a restart.
//! - **Each run's, as it goes.** The chat registers each run it starts
//!   with the conversation's taint then ([`began`]). The run's session
//!   marks it the moment it asks for an outside tool, before it's served
//!   ([`read`]), however that use ends: a refused or failed use may still
//!   carry words back, and one that killed the session still counts. When
//!   the run ends, the chat takes its mark into the conversation
//!   ([`ended`]).
//! - **Unknown is tainted.** A cell the chat didn't register, or one let go
//!   to make room, is never clean.
//! - **Files sent in** taint at hand-over: the chat marks the conversation
//!   when it gives a run their notes.

use std::sync::{Mutex, PoisonError};

use hatchling_core::subagent::ToolUse;

use crate::Config;
use crate::config::{GoalsAction, MemoryAction};

/// The most runs held at once: the chat runs one at a time, so a few
/// leave room for one ending as the next starts.
pub const MAX_RUNS: usize = 8;

/// Whether a use of `tool`, in a broker configured by `config`, reads
/// anything from outside the owner's own words.
///
/// The memory the owner kept and the schedules they approved are theirs.
/// Recalled memory and goals are too, unless runs may write them: then a
/// background run that read a page could have left its words there.
#[must_use]
pub fn outside(tool: ToolUse, config: &Config) -> bool {
    match tool {
        ToolUse::WebSearch
        | ToolUse::WebPage
        | ToolUse::Browser
        | ToolUse::MessageRead
        | ToolUse::Network
        | ToolUse::Room
        | ToolUse::Skill
        | ToolUse::Subagent
        | ToolUse::Space
        | ToolUse::Artifact => true,
        ToolUse::MemoryRecall => runs_write_memory(config),
        ToolUse::GoalsRead => runs_write_goals(config),
        ToolUse::GoalsWrite
        | ToolUse::MemoryPropose
        | ToolUse::MemoryWrite
        | ToolUse::ScheduleRead
        | ToolUse::ScheduleWrite
        | ToolUse::SchedulePropose
        | ToolUse::Hook
        | ToolUse::IdeaSuggest
        | ToolUse::MessageSend
        | ToolUse::Vault => false,
    }
}

/// Whether runs may write memory: then what's recalled may be a run's.
fn runs_write_memory(config: &Config) -> bool {
    config
        .memory
        .grants
        .iter()
        .any(|grant| matches!(grant.action, MemoryAction::Write))
}

/// Whether runs may write goals or their tracking: then what's read may
/// be a run's.
fn runs_write_goals(config: &Config) -> bool {
    config.goals.grants.iter().any(|action| {
        matches!(
            action,
            GoalsAction::GoalCreate
                | GoalsAction::GoalUpdate
                | GoalsAction::GoalClose
                | GoalsAction::TrackingAppend
        )
    })
}

/// One run the chat started, by its cell, and whether it's tainted: its
/// conversation was when it started, or it read from outside since.
#[derive(Debug)]
struct Run {
    cell: String,
    tainted: bool,
}

/// The chat's runs, oldest first, at most [`MAX_RUNS`].
#[derive(Debug, Default)]
pub struct Runs(Vec<Run>);

impl Runs {
    /// None.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Registers the run in `cell`, `tainted` as its conversation was when
    /// it started: in place of one it had, the oldest let go past
    /// [`MAX_RUNS`].
    pub fn began(&mut self, cell: &str, tainted: bool) {
        self.0.retain(|run| run.cell != cell);
        if self.0.len() >= MAX_RUNS {
            self.0.remove(0);
        }
        self.0.push(Run {
            cell: cell.to_owned(),
            tainted,
        });
    }

    /// Marks the run in `cell`: it read from outside.
    pub fn read(&mut self, cell: &str) {
        if let Some(run) = self.0.iter_mut().find(|run| run.cell == cell) {
            run.tainted = true;
        }
    }

    /// Whether the run in `cell` is clean: registered, and neither its
    /// conversation nor it has read from outside.
    #[must_use]
    pub fn clean(&self, cell: &str) -> bool {
        self.0.iter().any(|run| run.cell == cell && !run.tainted)
    }

    /// Lets the run in `cell` go: whether it's tainted. One not held, never
    /// registered or let go, is.
    pub fn ended(&mut self, cell: &str) -> bool {
        self.0
            .iter()
            .position(|run| run.cell == cell)
            .is_none_or(|at| self.0.remove(at).tainted)
    }
}

static SHARED: Mutex<Runs> = Mutex::new(Runs::new());

fn shared() -> std::sync::MutexGuard<'static, Runs> {
    SHARED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`Runs::began`], of the broker's own.
pub fn began(cell: &str, tainted: bool) {
    shared().began(cell, tainted);
}

/// [`Runs::read`], of the broker's own.
pub fn read(cell: &str) {
    shared().read(cell);
}

/// [`Runs::clean`], of the broker's own.
#[must_use]
pub fn clean(cell: &str) -> bool {
    shared().clean(cell)
}

/// [`Runs::ended`], of the broker's own.
#[must_use]
pub fn ended(cell: &str) -> bool {
    shared().ended(cell)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_clean_until_it_reads_from_outside() {
        let mut runs = Runs::new();
        runs.began("run-a", false);
        assert!(runs.clean("run-a"));
        runs.read("run-a");
        assert!(!runs.clean("run-a"));
        assert!(runs.ended("run-a"), "its mark goes to the conversation");
        assert!(!runs.clean("run-a"), "and it's let go");
    }

    #[test]
    fn a_run_in_a_tainted_conversation_is_never_clean() {
        let mut runs = Runs::new();
        runs.began("run-b", true);
        assert!(!runs.clean("run-b"));
        assert!(runs.ended("run-b"));
    }

    #[test]
    fn a_clean_run_ends_clean() {
        let mut runs = Runs::new();
        runs.began("run-c", false);
        runs.read("run-other");
        assert!(runs.clean("run-c"), "another's mark is its own");
        assert!(!runs.ended("run-c"));
    }

    #[test]
    fn a_run_not_held_is_tainted() {
        let mut runs = Runs::new();
        assert!(!runs.clean("run-never"));
        assert!(runs.ended("run-never"));
        for at in 0..=MAX_RUNS {
            runs.began(&format!("run-{at}"), false);
        }
        assert!(!runs.clean("run-0"), "let go to make room");
        assert!(runs.ended("run-0"));
        assert!(runs.clean(&format!("run-{MAX_RUNS}")));
    }

    #[test]
    fn a_run_started_again_is_registered_afresh() {
        let mut runs = Runs::new();
        runs.began("run-d", false);
        runs.read("run-d");
        runs.began("run-d", false);
        assert!(runs.clean("run-d"));
        assert_eq!(runs.0.len(), 1);
    }

    #[test]
    fn what_reads_from_outside_is_named_and_the_owners_own_is_not() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.memory.grants.clear();
        config.goals.grants.clear();
        for tool in [
            ToolUse::WebSearch,
            ToolUse::WebPage,
            ToolUse::Browser,
            ToolUse::MessageRead,
            ToolUse::Network,
            ToolUse::Room,
            ToolUse::Skill,
            ToolUse::Subagent,
            ToolUse::Space,
            ToolUse::Artifact,
        ] {
            assert!(outside(tool, &config), "{tool:?}");
        }
        for tool in [
            ToolUse::MemoryRecall,
            ToolUse::GoalsRead,
            ToolUse::GoalsWrite,
            ToolUse::MemoryPropose,
            ToolUse::MemoryWrite,
            ToolUse::ScheduleRead,
            ToolUse::ScheduleWrite,
            ToolUse::SchedulePropose,
            ToolUse::Hook,
            ToolUse::IdeaSuggest,
            ToolUse::MessageSend,
            ToolUse::Vault,
        ] {
            assert!(!outside(tool, &config), "{tool:?}");
        }
    }

    #[test]
    fn what_runs_may_write_is_outside_when_read() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.memory.grants = vec![crate::config::MemoryGrant {
            scope: String::from("user"),
            action: MemoryAction::Recall,
        }];
        config.goals.grants = vec![GoalsAction::QueryGoals, GoalsAction::QueryTracking];
        assert!(!outside(ToolUse::MemoryRecall, &config));
        assert!(!outside(ToolUse::GoalsRead, &config));
        config.memory.grants.push(crate::config::MemoryGrant {
            scope: String::from("project"),
            action: MemoryAction::Write,
        });
        assert!(outside(ToolUse::MemoryRecall, &config));
        assert!(!outside(ToolUse::GoalsRead, &config));
        for action in [
            GoalsAction::GoalCreate,
            GoalsAction::GoalUpdate,
            GoalsAction::GoalClose,
            GoalsAction::TrackingAppend,
        ] {
            config.goals.grants = vec![GoalsAction::QueryGoals, action];
            assert!(outside(ToolUse::GoalsRead, &config), "{action:?}");
        }
    }
}
