//! Process-group termination for [`hatchling_sys::process::Child`].
//!
//! `terminate_group` must `SIGKILL` the whole group — including
//! grandchildren — so a timed-out build tool cannot leave orphaned
//! descendants behind. The group is observed through `/proc`, never
//! through signals aimed at guessed pids.

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hatchling_sys::process::{Child, ChildPid};

/// PIDs currently in `pgid`, from `/proc`.
///
/// Parses field 5 (`pgrp`) of `/proc/<pid>/stat` from the last `)`,
/// so a `comm` value containing spaces or parens cannot shift the
/// fields. Unreadable entries (a pid exiting mid-scan) are skipped.
fn group_members(pgid: i32) -> Vec<i32> {
    let mut members = Vec::new();
    let Ok(proc) = std::fs::read_dir("/proc") else {
        return members;
    };
    for entry in proc.flatten() {
        let name = entry.file_name();
        let Some(pid_text) = name.to_str() else {
            continue;
        };
        let Ok(candidate) = pid_text.parse::<i32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{candidate}/stat")) else {
            continue;
        };
        // `)` is ASCII, so `paren + 1` is a char boundary — but the
        // slicing lint cannot know that; `.get` keeps it total.
        let Some(paren) = stat.rfind(')') else {
            continue;
        };
        let Some(after_paren) = stat.get(paren + 1..) else {
            continue;
        };
        let mut fields = after_paren.split_whitespace();
        // A zombie is already dead — it holds no resources and can do
        // nothing. What matters is that no live member remains.
        if fields.next().is_some_and(|state| state == "Z") {
            continue;
        }
        fields.next(); // ppid
        if fields
            .next()
            .is_some_and(|pgrp| pgrp.parse::<i32>().is_ok_and(|group| group == pgid))
        {
            members.push(candidate);
        }
    }
    members
}

/// Spins until `cond` holds or `deadline` passes. Process-group
/// membership races with fork/exec and with `SIGKILL` delivery, so
/// the test polls instead of asserting a single instant.
fn poll_until(deadline: Instant, mut cond: impl FnMut() -> bool) -> bool {
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

/// `terminate_group` on a group leader must kill the leader and its
/// grandchildren, leaving the group empty — and the direct child
/// reaps as killed by `SIGKILL` (exit code 137).
#[test]
fn terminate_group_kills_grandchildren_and_empties_the_group() {
    // `sleep 30 & wait`: the `&` forces sh to fork a grandchild, and
    // `wait` keeps the leader alive until it is killed.
    let std_child = Command::new("sh")
        .arg("-c")
        .arg("sleep 30 & wait")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn sh");
    // `process_group(0)` makes the child a group leader before the
    // parent can observe it, so pid == pgid with no race. Dropping
    // the std handle is safe: `Child` owns supervision and reaps.
    let pid = i32::try_from(std_child.id()).expect("pid fits in i32");
    drop(std_child);
    let child_pid = ChildPid::new(pid).expect("fresh child pid is valid");
    let child = Child::new(child_pid).expect("pidfd opens on our own child");

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        poll_until(deadline, || group_members(pid).len() >= 2),
        "sleep grandchild never joined the process group"
    );

    child.terminate_group().expect("terminate_group");

    // Reap the leader first: until it is reaped it lingers as a
    // zombie, still visible in /proc with its old group id.
    let status = child
        .wait_deadline(Duration::from_secs(10))
        .expect("reap the group leader");
    assert_eq!(
        hatchling_sys::child_exit_code(status),
        137, // 128 + SIGKILL: the leader died of the group SIGKILL
        "group leader should have died of SIGKILL"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        poll_until(deadline, || group_members(pid).is_empty()),
        "process group not empty after terminate_group: {:?}",
        group_members(pid)
    );
    // Drop runs kill-on-drop against an already-reaped child: quiet Ok.
    drop(child);
}

/// `terminate` (pidfd, direct child only) is the contrast case: it
/// must NOT be used for group cleanup, and this test pins the
/// difference — killing only the leader orphans the grandchild.
/// (It documents the API contract; the orphan is reaped here so the
/// test leaves no stray processes.)
#[test]
fn terminate_alone_would_orphan_the_grandchild() {
    let std_child = Command::new("sh")
        .arg("-c")
        .arg("sleep 30 & wait")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn sh");
    let pid = i32::try_from(std_child.id()).expect("pid fits in i32");
    drop(std_child);
    let child_pid = ChildPid::new(pid).expect("fresh child pid is valid");
    let child = Child::new(child_pid).expect("pidfd opens on our own child");

    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        poll_until(deadline, || group_members(pid).len() >= 2),
        "sleep grandchild never joined the process group"
    );

    // Direct-child kill only: the grandchild survives as an orphan.
    child.terminate().expect("terminate");
    let _ = child.wait_deadline(Duration::from_secs(10));
    assert!(
        !group_members(pid).is_empty(),
        "terminate() unexpectedly emptied the group — the contrast case is stale"
    );

    // Its leader reaped, the group may not be signalled any more
    // (WO-024): once it empties, its id could name another group.
    assert!(child.terminate_group().is_err());
    // Clean up the orphan the contrast case deliberately created, by
    // its own pid.
    for member in group_members(pid) {
        let _ = Command::new("kill")
            .args(["-KILL", &member.to_string()])
            .status();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    assert!(
        poll_until(deadline, || group_members(pid).is_empty()),
        "orphan grandchild survived terminate_group: {:?}",
        group_members(pid)
    );
}
