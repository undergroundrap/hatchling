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

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

use hatchling_core::ui::operator::{
    CellKind, CellLimits, OperatorCell, OperatorSession, SessionSocket,
};

/// The cells running, by the order they started in.
static CELLS: Mutex<BTreeMap<u64, OperatorCell>> = Mutex::new(BTreeMap::new());

/// The connections open, by the order they were admitted in.
static SESSIONS: Mutex<BTreeMap<u64, OperatorSession>> = Mutex::new(BTreeMap::new());

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
