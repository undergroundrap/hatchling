//! What runs on the machine, for the operator (WO-022).
//!
//! Every cell the broker starts is listed here while it runs: its agent
//! cell, each run and subagent, each skill and harness. A [`Running`]
//! guard lists one, and removes it when dropped, so a cell is listed
//! exactly as long as the broker holds it. Only metadata is kept: the
//! id the broker gave the cell, what kind it is, when it started and its
//! limits; never its task, its input or its output.
//!
//! Every admitted API and UI connection is listed the same way, by an
//! [`Open`] guard: its id, its socket and when it was admitted, never
//! what it said.
//!
//! The broker's last [`MAX_DECISIONS`] egress decisions are kept too
//! ([`decided`]): when, which cell, the destination it named (bounded to
//! [`MAX_DESTINATION_CHARS`]) and the verdict; never a path, a header, a
//! body or an answer.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use hatchling_core::ui::operator::{
    CellKind, CellLimits, EgressDecision, EgressVerdict, OperatorCell, OperatorSession,
    SessionSocket,
};

/// How many egress decisions are kept: the most recent.
pub const MAX_DECISIONS: usize = 200;

/// The most of a destination a decision keeps, in characters: the cell
/// chose it, so it is bounded.
pub const MAX_DESTINATION_CHARS: usize = 255;

/// The cells running, by the order they started in.
static CELLS: Mutex<BTreeMap<u64, OperatorCell>> = Mutex::new(BTreeMap::new());

/// The connections open, by the order they were admitted in.
static SESSIONS: Mutex<BTreeMap<u64, OperatorSession>> = Mutex::new(BTreeMap::new());

/// The egress decisions kept, oldest first.
static DECISIONS: Mutex<VecDeque<EgressDecision>> = Mutex::new(VecDeque::new());

/// How many cells are remembered as having a model that didn't answer in
/// time: the most recent. One is forgotten when its run's failure is told.
pub const MAX_QUIET_MODELS: usize = 64;

/// The cells whose model was still answering when the broker's wait for it
/// ran out, oldest first.
static QUIET_MODELS: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Held by a test that fills the decisions to their bound, and by one that
/// needs its own decision still kept: tests share the one process.
#[cfg(test)]
pub(crate) static FLOOD: Mutex<()> = Mutex::new(());

/// The next listing's key: its place in the start order.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// The listing, even if a holder panicked: it stays a valid map.
fn cells_now() -> MutexGuard<'static, BTreeMap<u64, OperatorCell>> {
    CELLS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One cell's listing, removed when dropped.
#[derive(Debug)]
#[must_use = "a cell is listed only while its guard lives"]
pub struct Running(u64);

impl Running {
    /// Lists the cell `cell_id`, of `kind`, held to `limits`, as started
    /// now.
    pub fn start(cell_id: &str, kind: CellKind, limits: &crate::CellLimits) -> Self {
        let key = NEXT.fetch_add(1, Ordering::Relaxed);
        cells_now().insert(
            key,
            OperatorCell {
                cell_id: cell_id.to_owned(),
                kind,
                started_at: hatchling_scheduler::Timestamp::now().to_string(),
                limits: operator_limits(limits),
            },
        );
        Self(key)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        cells_now().remove(&self.0);
    }
}

/// The cells running now, oldest first.
#[must_use]
pub fn cells() -> Vec<OperatorCell> {
    cells_now().values().cloned().collect()
}

/// The sessions listing, even if a holder panicked.
fn sessions_now() -> MutexGuard<'static, BTreeMap<u64, OperatorSession>> {
    SESSIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One open connection's listing, removed when dropped.
#[derive(Debug)]
#[must_use = "a session is listed only while its guard lives"]
pub struct Open(u64);

impl Open {
    /// Lists the connection `connection`, on `socket`, as admitted now.
    pub fn start(connection: &str, socket: SessionSocket) -> Self {
        let key = NEXT.fetch_add(1, Ordering::Relaxed);
        sessions_now().insert(
            key,
            OperatorSession {
                connection: connection.to_owned(),
                socket,
                opened_at: hatchling_scheduler::Timestamp::now().to_string(),
            },
        );
        Self(key)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        sessions_now().remove(&self.0);
    }
}

/// The connections open now, oldest first.
#[must_use]
pub fn sessions() -> Vec<OperatorSession> {
    sessions_now().values().cloned().collect()
}

/// The decisions kept, even if a holder panicked.
fn decisions_now() -> MutexGuard<'static, VecDeque<EgressDecision>> {
    DECISIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps the broker's `verdict` on `cell_id`'s request to `destination`,
/// made now, dropping the oldest past [`MAX_DECISIONS`].
pub fn decided(cell_id: &str, destination: &str, verdict: EgressVerdict) {
    let decision = EgressDecision {
        at: hatchling_scheduler::Timestamp::now().to_string(),
        cell_id: cell_id.to_owned(),
        destination: destination.chars().take(MAX_DESTINATION_CHARS).collect(),
        verdict,
    };
    let mut kept = decisions_now();
    // One in at a time, so one out at most keeps the bound.
    if kept.len() >= MAX_DECISIONS {
        kept.pop_front();
    }
    kept.push_back(decision);
}

/// The egress decisions kept, oldest first.
#[must_use]
pub fn decisions() -> Vec<EgressDecision> {
    decisions_now().iter().cloned().collect()
}

/// The quiet models kept, even if a holder panicked.
fn quiet_models_now() -> MutexGuard<'static, VecDeque<String>> {
    QUIET_MODELS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Remembers that `cell_id`'s model was still answering when the broker's
/// wait for it ran out, dropping the oldest past [`MAX_QUIET_MODELS`].
///
/// The agent is told only that its request failed. This is the broker's
/// own knowledge of why, kept for the words its owner is told.
pub fn model_was_quiet(cell_id: &str) {
    let mut kept = quiet_models_now();
    if kept.iter().any(|known| known == cell_id) {
        return;
    }
    if kept.len() >= MAX_QUIET_MODELS {
        kept.pop_front();
    }
    kept.push_back(cell_id.to_owned());
}

/// Whether `cell_id`'s model was still answering when the wait for it ran
/// out, forgetting it: it is told once.
#[must_use]
pub fn took_quiet_model(cell_id: &str) -> bool {
    let mut kept = quiet_models_now();
    let before = kept.len();
    kept.retain(|known| known != cell_id);
    kept.len() != before
}

/// A cell's limits as the operator's protocol carries them.
const fn operator_limits(limits: &crate::CellLimits) -> CellLimits {
    CellLimits {
        processes: limits.processes,
        address_space: limits.address_space,
        file_size: limits.file_size,
        open_files: limits.open_files,
        memory: limits.memory,
        cpu_millis: limits.cpu_millis,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cell whose model didn't answer in time is remembered until it is
    /// asked after, once, and only the most recent are kept.
    #[test]
    fn a_quiet_model_is_remembered_until_it_is_told() {
        assert!(!took_quiet_model("quiet-none"), "never noted");
        model_was_quiet("quiet-a");
        model_was_quiet("quiet-a");
        model_was_quiet("quiet-b");
        assert!(took_quiet_model("quiet-a"));
        assert!(
            !took_quiet_model("quiet-a"),
            "told once, though noted twice"
        );
        assert!(took_quiet_model("quiet-b"), "another's stays");
        // Past the bound the oldest goes.
        model_was_quiet("quiet-first");
        for n in 0..MAX_QUIET_MODELS {
            model_was_quiet(&format!("quiet-fill-{n}"));
        }
        assert!(!took_quiet_model("quiet-first"), "the oldest was dropped");
        assert!(took_quiet_model("quiet-fill-0"));
        for n in 1..MAX_QUIET_MODELS {
            assert!(took_quiet_model(&format!("quiet-fill-{n}")), "{n}");
        }
    }

    /// The listings of `ids` only: other tests start cells too.
    fn listed(ids: &[&str]) -> Vec<(String, CellKind)> {
        cells()
            .into_iter()
            .filter(|cell| ids.contains(&cell.cell_id.as_str()))
            .map(|cell| (cell.cell_id, cell.kind))
            .collect()
    }

    #[test]
    fn a_cell_is_listed_while_its_guard_lives_in_the_order_cells_started() {
        let ids = ["machine-a", "machine-b"];
        let limits = crate::CellLimits {
            memory: Some(1 << 30),
            cpu_millis: Some(500),
            ..crate::CellLimits::default()
        };
        let first = Running::start("machine-a", CellKind::Skill, &limits);
        let second = Running::start(
            "machine-b",
            CellKind::Harness,
            &crate::CellLimits::default(),
        );
        assert_eq!(
            listed(&ids),
            [
                (String::from("machine-a"), CellKind::Skill),
                (String::from("machine-b"), CellKind::Harness)
            ]
        );
        let cell = cells()
            .into_iter()
            .find(|cell| cell.cell_id == "machine-a")
            .unwrap();
        assert_eq!(cell.limits, operator_limits(&limits));
        assert_eq!(cell.limits.memory, Some(1 << 30));
        assert_eq!(cell.limits.cpu_millis, Some(500));
        assert_eq!(cell.limits.processes, limits.processes);
        assert!(
            cell.started_at
                .parse::<hatchling_scheduler::Timestamp>()
                .is_ok()
        );
        drop(first);
        assert_eq!(
            listed(&ids),
            [(String::from("machine-b"), CellKind::Harness)]
        );
        drop(second);
        assert!(listed(&ids).is_empty());
    }

    #[test]
    fn the_listing_is_read_and_written_under_its_lock() {
        let guard = Running::start(
            "machine-locked",
            CellKind::Run,
            &crate::CellLimits::default(),
        );
        let key = guard.0;
        assert_eq!(
            cells_now().get(&key).map(|cell| cell.cell_id.as_str()),
            Some("machine-locked")
        );
        drop(guard);
        assert!(cells_now().get(&key).is_none());
    }

    #[test]
    fn a_session_is_listed_while_its_guard_lives() {
        let mine = || -> Vec<(String, SessionSocket)> {
            sessions()
                .into_iter()
                .filter(|open| open.connection.starts_with("machine-session-"))
                .map(|open| (open.connection, open.socket))
                .collect()
        };
        let api = Open::start("machine-session-api", SessionSocket::Api);
        let ui = Open::start("machine-session-ui", SessionSocket::Ui);
        assert_eq!(
            mine(),
            [
                (String::from("machine-session-api"), SessionSocket::Api),
                (String::from("machine-session-ui"), SessionSocket::Ui)
            ]
        );
        let key = ui.0;
        assert!(sessions_now().get(&key).is_some_and(|open| {
            open.opened_at
                .parse::<hatchling_scheduler::Timestamp>()
                .is_ok()
        }));
        drop(api);
        assert_eq!(
            mine(),
            [(String::from("machine-session-ui"), SessionSocket::Ui)]
        );
        drop(ui);
        assert!(mine().is_empty());
        assert!(sessions_now().get(&key).is_none());
    }

    #[test]
    fn egress_decisions_are_kept_bounded_and_newest_last() {
        let _flood = FLOOD.lock().unwrap_or_else(PoisonError::into_inner);
        // Others decide in parallel: only this cell's are looked at, and
        // each test cell's own order is what is checked.
        let mine = |cell: &str| -> Vec<(String, EgressVerdict)> {
            decisions()
                .into_iter()
                .filter(|decision| decision.cell_id == cell)
                .map(|decision| (decision.destination, decision.verdict))
                .collect()
        };
        decided("machine-egress", "localhost:18089", EgressVerdict::Allowed);
        decided("machine-egress", "evil.example:443", EgressVerdict::NoGrant);
        assert_eq!(
            mine("machine-egress"),
            [
                (String::from("localhost:18089"), EgressVerdict::Allowed),
                (String::from("evil.example:443"), EgressVerdict::NoGrant)
            ]
        );
        // A long destination is cut, on a character.
        let long = "\u{e9}".repeat(MAX_DESTINATION_CHARS + 10);
        decided("machine-egress-long", &long, EgressVerdict::NotAllowed);
        let kept = mine("machine-egress-long");
        assert_eq!(kept.len(), 1);
        assert_eq!(
            kept.first().map(|cut| cut.0.chars().count()),
            Some(MAX_DESTINATION_CHARS)
        );
        assert!(decisions_now().iter().all(|decision| {
            decision
                .at
                .parse::<hatchling_scheduler::Timestamp>()
                .is_ok()
        }));
        // Never more than the bound: the oldest go first.
        for n in 0..=MAX_DECISIONS {
            decided(
                "machine-egress-flood",
                &n.to_string(),
                EgressVerdict::Failed,
            );
        }
        assert!(decisions().len() <= MAX_DECISIONS);
        assert!(
            mine("machine-egress").is_empty(),
            "pushed out by newer ones"
        );
        let flood = mine("machine-egress-flood");
        assert_eq!(flood.last().map(|kept| kept.0.as_str()), Some("200"));
        assert!(!flood.iter().any(|kept| kept.0 == "0"));
    }

    #[test]
    fn every_limit_reaches_the_operator() {
        let limits = crate::CellLimits {
            processes: Some(1),
            address_space: Some(2),
            file_size: Some(3),
            open_files: Some(4),
            memory: Some(5),
            cpu_millis: Some(6),
        };
        assert_eq!(
            operator_limits(&limits),
            CellLimits {
                processes: Some(1),
                address_space: Some(2),
                file_size: Some(3),
                open_files: Some(4),
                memory: Some(5),
                cpu_millis: Some(6),
            }
        );
    }
}
