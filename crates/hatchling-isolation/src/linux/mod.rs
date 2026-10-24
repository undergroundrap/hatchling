//! The Linux backend: the cell the broker has always built, behind the
//! seam (, moved unchanged from the broker's `cell` module).
//!
//! [`Linux::isolate`](crate::IsolationBackend::isolate) forks a child
//! that enters a user namespace, a mount namespace, and its own network
//! and IPC namespaces ; the parent writes the id maps; the child
//! bind-mounts exactly the cell's view (all read-only but the granted
//! writable directories) into a fresh tmpfs root, `pivot_root`s, enters
//! a pid namespace, becomes the cell user, drops every capability
//!, forks the agent under a reaper, and in the agent sets
//! `NoNewPrivs`, installs the seccomp allowlist and execs the program.
//!
//! [`Linux::isolate`](crate::IsolationBackend::isolate) must be called
//! before the caller starts other threads: it forks, and forking a
//! multi-threaded process is unsound.
//!
//! The identity story: in-cell uid 0 maps to the identity's principal,
//! so `SO_PEERCRED` on the broker socket reports that principal. The
//! cell sees the broker (whose uid is outside its map) as the kernel
//! overflow uid ([`Linux::broker_in_cell`](crate::IsolationBackend::broker_in_cell)).
//!
//! All OS interaction goes through `hatchling-sys`: this module
//! contains no `unsafe`.

mod identity;
mod spawn;
mod supervise;
mod view;

pub use spawn::LinuxCell;
pub use supervise::LinuxProcess;

use crate::{CellIdentity, CellSpec, CellView, Error, IsolationBackend, SuperviseError};

/// The Linux backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Linux;

/// Who makes cells: the uid and gid recorded by [`spawns_as`].
static SPAWNS_AS: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();

/// Records that cells are made by a process running as `uid` and `gid`
/// (WO-017): the spawner, which writes their id maps, while this process,
/// which builds their specs, may run as another user. Each cell's
/// identity, and the uid its maker appears as inside it, are then worked
/// out for that process; until this is called, for this one.
///
/// # Errors
///
/// Refused when another maker is recorded already; recording the same
/// one again is fine.
pub fn spawns_as(uid: u32, gid: u32) -> std::io::Result<()> {
    if *SPAWNS_AS.get_or_init(|| (uid, gid)) == (uid, gid) {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "another maker of cells is recorded already",
        ))
    }
}

/// The uid and gid cells are made as: [`spawns_as`]'s, or this
/// process's own.
fn maker() -> (u32, u32) {
    SPAWNS_AS
        .get()
        .copied()
        .unwrap_or_else(|| (hatchling_sys::euid(), hatchling_sys::egid()))
}

impl IsolationBackend for Linux {
    type Cell = LinuxCell;
    type Process = LinuxProcess;

    fn owner(&self) -> u32 {
        maker().0
    }

    fn identity(&self, principal: Option<u32>, count: u32) -> Result<CellIdentity, Error> {
        let (uid, gid) = maker();
        identity::identity(uid, gid, principal, count)
    }

    fn subordinate_identity(&self) -> Result<CellIdentity, Error> {
        let (uid, gid) = maker();
        identity::subordinate(
            uid,
            || hatchling_sys::subid::cell_ids_for(uid, gid),
            || hatchling_sys::subid::Helpers::standard().map(drop),
        )
    }

    fn broker_in_cell(&self, identity: &CellIdentity) -> Result<u32, Error> {
        identity::broker_in_cell(maker().0, *identity)
    }

    fn view(&self, spec: &CellSpec) -> Result<CellView, Error> {
        view::view(spec)
    }

    fn isolate(&self, spec: &CellSpec) -> Result<LinuxCell, Error> {
        spawn::isolate(spec)
    }

    fn supervise(&self, child: &mut std::process::Child) -> Result<LinuxProcess, SuperviseError> {
        supervise::supervise(child)
    }
}

/// Whether this box can run a real cell, found by running one: a nested
/// user namespace (a container) or a kernel that restricts unprivileged
/// user namespaces cannot. `HATCHLING_REQUIRE_CELL=1` turns the honest
/// skip into a failure, so a mutation run never mistakes a skipped test
/// for a passing one (a mutant that breaks the cell fails here).
#[cfg(test)]
pub(crate) fn cell_available() -> bool {
    use crate::Cell as _;
    let probe = Linux.identity(None, 1).map(|identity| CellSpec {
        identity,
        program: std::path::PathBuf::from("/bin/true"),
        argv: vec![String::from(crate::PROGRAM)],
        env: Vec::new(),
        workdir: None,
        endpoint: None,
        connection: false,
        exposures: Vec::new(),
        tools: Vec::new(),
        relay: None,
        filter: crate::Filter::Program,
        limits: crate::Limits::default(),
        cgroup: None,
        scratch: None,
    });
    let works = probe
        .and_then(|spec| Linux.isolate(&spec))
        .and_then(LinuxCell::wait)
        .is_ok_and(|exit| exit.code == 0);
    let required = std::env::var("HATCHLING_REQUIRE_CELL").is_ok_and(|v| v == "1");
    assert!(
        works || !required,
        "HATCHLING_REQUIRE_CELL=1 but this box cannot build a cell: refusing to skip"
    );
    works
}

/// A program that exits with code 1 under the cell's filter: GNU
/// `false` where the distribution ships it beside another `false`.
#[cfg(test)]
pub(crate) fn false_witness() -> std::path::PathBuf {
    let gnu = std::path::Path::new("/usr/bin/gnufalse");
    if gnu.exists() {
        gnu.to_path_buf()
    } else {
        std::path::PathBuf::from("/bin/false")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_is_this_process() {
        assert_eq!(Linux.owner(), hatchling_sys::euid());
    }

    /// Recorded once, the maker's ids decide each identity, whatever this
    /// process runs as; until then they are this process's own. (In a
    /// child of its own: the record is its process's.)
    #[test]
    fn identities_are_worked_out_for_the_maker_recorded() {
        assert_eq!(maker(), (hatchling_sys::euid(), hatchling_sys::egid()));
        match hatchling_sys::fork().unwrap() {
            hatchling_sys::ForkResult::Child => {
                let held = (|| {
                    spawns_as(0, 0).ok()?;
                    spawns_as(0, 0).ok()?;
                    spawns_as(1, 1).err()?;
                    (Linux.owner() == 0).then_some(())?;
                    let cell = Linux.identity(None, 1).ok()?;
                    (cell.principal == 65534 && cell.group == 65534).then_some(())?;
                    Linux.subordinate_identity().err()?;
                    Some(())
                })();
                hatchling_sys::exit_child(i32::from(held.is_none()));
            }
            hatchling_sys::ForkResult::Parent { child } => {
                let status = child
                    .wait_deadline(std::time::Duration::from_secs(30))
                    .unwrap();
                assert_eq!(hatchling_sys::child_exit_code(status), 0);
            }
        }
    }

    #[test]
    fn the_backend_views_and_supervises_through_its_modules() {
        use crate::Supervised as _;
        let mut child = std::process::Command::new("/bin/true").spawn().unwrap();
        let process = Linux.supervise(&mut child).unwrap();
        let exit = process
            .wait_deadline(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(exit.code, 0);
        let spec = CellSpec {
            identity: Linux.identity(None, 1).unwrap(),
            program: std::path::PathBuf::from("/bin/true"),
            argv: Vec::new(),
            env: Vec::new(),
            workdir: None,
            endpoint: None,
            connection: false,
            exposures: Vec::new(),
            tools: Vec::new(),
            relay: None,
            filter: crate::Filter::Program,
            limits: crate::Limits::default(),
            cgroup: None,
            scratch: None,
        };
        assert_eq!(Linux.view(&spec).unwrap(), view::view(&spec).unwrap());
    }

    #[test]
    fn the_trait_methods_are_the_modules() {
        let euid = hatchling_sys::euid();
        assert_eq!(
            Linux.identity(None, 1).unwrap(),
            identity::identity(euid, hatchling_sys::egid(), None, 1).unwrap()
        );
        let identity = Linux.identity(None, 1).unwrap();
        assert_eq!(
            Linux.broker_in_cell(&identity).unwrap(),
            identity::broker_in_cell(euid, identity).unwrap()
        );
        // A map that leaves the broker out: the kernel's overflow uid,
        // which is never the in-cell root.
        let elsewhere = CellIdentity::direct(euid.wrapping_add(1), 1);
        let unmapped = Linux.broker_in_cell(&elsewhere).unwrap();
        assert_eq!(unmapped, identity::broker_in_cell(euid, elsewhere).unwrap());
        assert_ne!(unmapped, 0, "an unmapped broker is not the cell's root");
    }
}
