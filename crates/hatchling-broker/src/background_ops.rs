//! The broker's side of the background clock (WO-054): where its state is
//! kept, what holds it back, and the loops it wakes.
//!
//! The clock itself is [`hatchling_scheduler::background`]. Its state is
//! one entry in the sealed schedule store, under that store's key, apart
//! from every schedule's: a schedule op can't name it, and it holds no
//! text, only due times, outcomes and gaps.
//!
//! No loop is registered yet ([`loops`]): the first, memory reconciliation,
//! comes with the seat runner. Until then each tick records the broker's
//! uptime, so a gap shows when Hatchling was stopped or the machine slept.

use std::sync::{Mutex, PoisonError};

use hatchling_scheduler::Timestamp;
use hatchling_scheduler::background::{
    Clock, Gate, Launcher, Loop, Outcome, Report, Run, State, Store,
};
use hatchling_vault::{OpensStores, SealedVault};

use crate::schedule_ops::{read_json, write_json};
use crate::{Error, SchedulerConfig};

/// The clock's entry in the schedule store.
const BACKGROUND: &str = "background-state";

/// One update to the clock's state at a time, within this broker.
static UPDATE: Mutex<()> = Mutex::new(());

/// The background loops the clock wakes: none yet.
#[must_use]
pub const fn loops() -> Vec<Loop> {
    Vec::new()
}

/// The clock's state in the sealed schedule store at `root`.
#[derive(Debug)]
pub struct SealedBackgroundStore<'k> {
    root: &'k std::path::Path,
    key: &'k dyn OpensStores,
}

impl<'k> SealedBackgroundStore<'k> {
    /// The state in the store at `root`, sealed under `key`.
    #[must_use]
    pub const fn new(root: &'k std::path::Path, key: &'k dyn OpensStores) -> Self {
        Self { root, key }
    }

    /// The state as it's kept: the default when there is none yet.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure, or a corrupt entry.
    pub fn read(&self) -> Result<State, Error> {
        let vault = self.vault()?;
        Ok(read_json(&vault, BACKGROUND)?.unwrap_or_default())
    }

    fn vault(&self) -> Result<SealedVault<'k>, Error> {
        SealedVault::open_in(self.root, self.key, hatchling_vault::Store::SCHEDULES)
            .map_err(|source| Error::Vault { source })
    }
}

impl Store for SealedBackgroundStore<'_> {
    type Error = Error;

    fn update(&self, change: &mut dyn FnMut(&mut State)) -> Result<State, Error> {
        let _serial = UPDATE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut state: State = read_json(&vault, BACKGROUND)?.unwrap_or_default();
        change(&mut state);
        write_json(&vault, BACKGROUND, &state)?;
        Ok(state)
    }
}

/// No run: there's no loop to start yet, and [`NoLoops`] makes none. Were
/// one ever polled, it would say it failed.
#[derive(Debug)]
pub struct NoRun;

impl Run for NoRun {
    fn poll(&mut self) -> Option<Outcome> {
        Some(Outcome::Failed)
    }

    fn stop(&mut self) {}
}

/// Starts no loop: none is registered yet.
#[derive(Debug, Default)]
pub struct NoLoops;

impl Launcher for NoLoops {
    type Run = NoRun;

    fn start(&self, name: &str, _missed: bool) -> Result<NoRun, String> {
        Err(format!("no background loop named {name:?} can run yet"))
    }
}

/// What holds background work back now: a chat run in flight. The
/// owner's pause (#673) joins it when it's built.
#[must_use]
pub fn gate() -> Gate {
    Gate {
        paused: false,
        chat_active: crate::follow_ups::any_in_flight(),
    }
}

/// One tick of `clock` at `now`, against the broker's store; nothing when
/// the broker has no schedule store.
///
/// # Errors
///
/// Returns [`Error`] when the store can't be read or written.
pub fn tick_once<L: Launcher>(
    clock: &mut Clock<L::Run>,
    scheduler: &SchedulerConfig,
    key: &dyn OpensStores,
    launcher: &L,
    now: Timestamp,
) -> Result<Report, Error> {
    if !scheduler.enabled() {
        return Ok(Report::default());
    }
    let store = SealedBackgroundStore::new(&scheduler.root, key);
    clock.tick(now, gate(), &store, launcher)
}

/// Turns background runs on or off as a whole, as the owner says; the
/// state as written.
///
/// # Errors
///
/// Returns [`Error`] on a backend failure.
pub fn set_on(
    scheduler: &SchedulerConfig,
    key: &dyn OpensStores,
    on: bool,
) -> Result<State, Error> {
    SealedBackgroundStore::new(&scheduler.root, key).update(&mut |state| state.off = !on)
}

/// Turns the loop `name` on or off, as the owner says; the state as
/// written.
///
/// # Errors
///
/// Returns [`Error`] on a backend failure.
pub fn set_loop_on(
    scheduler: &SchedulerConfig,
    key: &dyn OpensStores,
    name: &str,
    on: bool,
) -> Result<State, Error> {
    SealedBackgroundStore::new(&scheduler.root, key).update(&mut |state| {
        if on {
            state.off_loops.remove(name);
        } else {
            state.off_loops.insert(name.to_owned());
        }
    })
}

#[cfg(test)]
mod tests {
    use hatchling_scheduler::background::{GAP_SECS, Gap};
    use hatchling_vault::MasterKey;

    use super::*;

    /// 2026-11-26T11:31:00Z.
    const T0: i64 = 1_795_692_660;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).unwrap()
    }

    fn config(root: &std::path::Path) -> SchedulerConfig {
        let mut config = SchedulerConfig::disabled();
        config.root = root.to_path_buf();
        config
    }

    fn root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-background-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// The clock's state is kept sealed across brokers: a second clock, as
    /// a restarted broker's, finds the first one's last tick and records
    /// the gap.
    #[test]
    fn the_state_is_kept_and_a_restart_shows_the_gap() {
        let root = root("gap");
        let key = MasterKey::generate().unwrap();
        let config = config(&root);
        let mut first = Clock::new(loops());
        let report = tick_once(&mut first, &config, &key, &NoLoops, at(T0)).unwrap();
        assert_eq!(report.gap, None);
        let later = T0 + GAP_SECS + 600;
        let mut second = Clock::new(loops());
        let report = tick_once(&mut second, &config, &key, &NoLoops, at(later)).unwrap();
        let kept = SealedBackgroundStore::new(&root, &key).read();
        let other = MasterKey::generate().unwrap();
        let sealed = SealedBackgroundStore::new(&root, &other).read();
        let _ = std::fs::remove_dir_all(&root);
        let gap = Gap {
            from: T0,
            to: later,
        };
        assert_eq!(report.gap, Some(gap));
        let kept = kept.unwrap();
        assert_eq!(kept.gaps, vec![gap]);
        assert_eq!(kept.seen, Some(later));
        assert!(sealed.is_err(), "read under another key");
    }

    /// The owner's switches are kept: off as a whole, and loop by loop.
    #[test]
    fn the_owners_switches_are_kept() {
        let root = root("switches");
        let key = MasterKey::generate().unwrap();
        let config = config(&root);
        let off = set_on(&config, &key, false).unwrap();
        let on = set_on(&config, &key, true).unwrap();
        let memory_off = set_loop_on(&config, &key, "memory", false).unwrap();
        let read = SealedBackgroundStore::new(&root, &key).read().unwrap();
        let memory_on = set_loop_on(&config, &key, "memory", true).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(off.off);
        assert!(!on.off);
        assert!(!memory_off.on("memory"));
        assert!(memory_off.on("ideas"));
        assert_eq!(read, memory_off);
        assert!(memory_on.on("memory"));
    }

    /// With no schedule store, there's no clock: nothing is written.
    #[test]
    fn no_store_no_clock() {
        let key = MasterKey::generate().unwrap();
        let mut config = config(&root("none"));
        config.root = std::path::PathBuf::new();
        let mut clock = Clock::new(loops());
        assert_eq!(
            tick_once(&mut clock, &config, &key, &NoLoops, at(T0)).unwrap(),
            Report::default()
        );
    }

    /// No loop can start yet.
    #[test]
    fn no_loop_starts_yet() {
        assert!(loops().is_empty());
        assert!(NoLoops.start("memory", false).is_err());
    }
}
