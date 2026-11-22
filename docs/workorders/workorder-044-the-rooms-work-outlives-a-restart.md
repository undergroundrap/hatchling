# WO-044: The Room's work outlives a restart: kept job records, receipts, and lock drilled with work running

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (the spawner's Room: `room_job`, `room_session`, `room_launch`; `lock`), `crates/hatchling-room` (the supervisor), `crates/hatchling-core` (`room`, the supervisor's wire)
**Issue:** [#568](https://github.com/undergroundrap/hatchling/issues/568)
**Trust boundary:** yes

## Problem

The agent runs programs in its Room as named jobs ([WO-034](workorder-034-run-commands-in-the-agents-encrypted-room.md), [WO-035](workorder-035-the-agents-tools-in-its-own-computer.md)). The spawner keeps the table of those jobs in memory. If the spawner restarts, the table is gone. A job it was running ends with its Room, and nothing afterwards says what it was or how it ended. The agent, asking after it by name, is told it doesn't exist, and the owner's Activity shows nothing either.

The Room's lifecycle is proven for the steps that touch running work: closing or wiping the disk ends the session first, admitted work holds off rekey and enrollment, and closing the room disk under a running job is refused. But `lock` has been drilled only with stand-ins, and on installs with nothing running. A real lock needs a vault a security key protects, and none of the drills had one with work in the Room.

These were WO-034's last open items. They moved here, unbuilt, when WO-034 closed (the maintainer's decision, 2026-11-21).

## Scope

**In scope:**

- A record of each Room job the spawner admits, kept where a spawner restart finds it: its name, what it ran, when, and how it ended. A job whose Room ended under it ends as *lost*, with why.
- A receipt for interrupted work: asked after by name after a restart, a lost job says it was lost and why; the owner's Activity says so too.
- `lock` with work running in the Room, on an install whose vault a security key protects: it holds off or ends that work as the design says, says which, and locks only once the room disk is closed.

**Not in scope:** resuming a lost job. A program the Room was running isn't restarted: it's reported.

## Acceptance criteria

- [ ] A job running when the spawner is killed is reported as lost, with why, when asked after by name once it's back, and in the owner's Activity; a test kills the spawner mid-job and checks both.
- [ ] Job records survive a spawner restart and a reboot, and are bounded (old ones are let go, with a limit a test checks).
- [ ] On a real install whose vault a security key protects, `lock` with a job running in the Room: what happens to the job, what `lock` says, and that the room disk is closed before it says *Locked*, recorded in evidence.
- [ ] Each new guard fails its test when removed.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- WO-034's proofs this builds on: [real-install-drill.md](../evidence/real-install-drill.md) (the room disk can't be closed under a running job; the workspace survives restarts), and its listed tests for session expiry and the session ended before a close or wipe.
