//! A forked child that execs from a [`PreparedExec`] allocates nothing
//! between the fork and the `execve` (#704), whatever another thread of
//! the parent is doing. This binary's allocator ends a child that
//! allocates while it's told not to, so an allocation there is an exit
//! code, not a hope.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hatchling_sys::process::{ForkResult, child_exit_code, exit_child, fork};
use hatchling_sys::{PreparedExec, seccomp};

/// Set in a child once it's forked: any allocation after it ends the
/// child with [`ALLOCATED`].
static FORBIDDEN: AtomicBool = AtomicBool::new(false);

/// What a child that allocated exits with.
const ALLOCATED: i32 = 77;

struct Watched;

// SAFETY: every call goes to the system allocator, unchanged; the only
// addition is the check, which calls `_exit`, async-signal-safe.
unsafe impl GlobalAlloc for Watched {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if FORBIDDEN.load(Ordering::Relaxed) {
            // SAFETY: `_exit` ends the process at once, touching nothing.
            unsafe { libc::_exit(ALLOCATED) }
        }
        // SAFETY: as the caller's contract for `alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as the caller's contract for `dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Watched = Watched;

/// Forks, runs `child` in the child with allocation forbidden, and says
/// how the child ended.
fn in_child(child: impl FnOnce()) -> i32 {
    match fork().expect("fork") {
        ForkResult::Child => {
            FORBIDDEN.store(true, Ordering::Relaxed);
            child();
            exit_child(1)
        }
        ForkResult::Parent { child } => {
            let status = child
                .wait_deadline(Duration::from_secs(20))
                .expect("the child ended in time");
            child_exit_code(status)
        }
    }
}

/// The design's test: a prepared exec allocates nothing in the child, run
/// many times while another thread allocates in a loop, under a deadline.
#[test]
fn a_prepared_exec_allocates_nothing_after_the_fork() {
    let busy = std::sync::Arc::new(AtomicBool::new(true));
    let allocating = {
        let busy = std::sync::Arc::clone(&busy);
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            while busy.load(Ordering::Relaxed) {
                kept.push(vec![0_u8; 4096]);
                if kept.len() > 64 {
                    kept.clear();
                }
            }
        })
    };
    let truth = PreparedExec::new(Path::new("/bin/true"), &["true"], &[("A", "b")]).unwrap();
    for round in 0..50 {
        let code = in_child(|| {
            let _ = truth.exec(|| Ok(()));
        });
        assert_eq!(code, 0, "round {round}: the child allocated, or its exec failed");
    }
    busy.store(false, Ordering::Relaxed);
    allocating.join().unwrap();
}

/// A prepared filter installs allocating nothing too.
#[test]
fn a_prepared_filter_installs_allocating_nothing() {
    let filter = seccomp::prepare(&seccomp::SeccompProfile::agent().unwrap()).unwrap();
    let truth = PreparedExec::new(Path::new("/bin/true"), &["true"], &[]).unwrap();
    let code = in_child(|| {
        if hatchling_sys::set_no_new_privs().is_err() {
            exit_child(3);
        }
        let _ = truth.exec(|| filter.install());
    });
    // The agent's filter may refuse what /bin/true does: what matters is
    // that the child never allocated on its way to the exec.
    assert_ne!(code, ALLOCATED, "installing a prepared filter allocated");
}

/// The check works: building the exec in the child, as `exec` does,
/// allocates, and the child is ended for it.
#[test]
fn building_the_exec_in_the_child_is_caught() {
    let code = in_child(|| {
        let _ = hatchling_sys::exec(Path::new("/bin/true"), &["true"], &[], || Ok(()));
    });
    assert_eq!(code, ALLOCATED);
}
