# WO-039: The spawner in a crate of its own

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (`spawner.rs` and what it calls), a new `crates/hatchling-spawner`, `docs/arch/module-map.md`, `docs/releasing.md`
**Issue:** [#481](https://github.com/undergroundrap/hatchling/issues/481)
**Trust boundary:** yes (the code that keeps root)

## Problem

The spawner is the one part of the broker that keeps root: it starts every cell's helper, holds each spec to its own policy, opens the room disk and runs the Room. It lives inside `hatchling-broker`, a crate of about 117,000 lines, so the code that runs as root can't be reviewed, built or audited apart from the rest.

It isn't a move. `spawner.rs` (about 4,400 lines) calls into about twenty broker modules, and through `room_disk` and `keeper` it reaches the vault, backup and the API's serving code. Taking it out means deciding where those seams go first.

## Scope

**In scope:**

- A written design first: which modules move with the spawner, which it reaches through a trait, and which it must not reach at all.
- `hatchling-spawner`, a published crate the broker depends on, holding what the design says moves.
- `hatchling-dev`'s audit holding the spawner crate away from what it must not reach.
- Adding a published crate as `docs/releasing.md` says (its first publish is the maintainer's).

**Not in scope:** any change to what the spawner may do, or to who runs as root.

## Acceptance criteria

- [ ] The design is in the work order before the code, reviewed.
- [ ] Every moved function is the same code (or the changed lines are listed with why), and the same mutants are found in it before and after.
- [ ] The broker's own code no longer contains anything the spawner runs as root.
- [ ] The audit fails a change that makes the spawner crate depend on what the design rules out.
- [ ] The full gate, the root pass and the installed Room tests pass, unchanged.

## Gates

As [TEMPLATE.md](TEMPLATE.md), plus the root pass (`HATCHLING_ROOT_PASS=1 bash scripts/check.sh`).

## Evidence

Filled in at close.

## Notes

Comes after Step 4 of 0.9. The Room's supervisor moved out of `hatchling-browser` the same way (#475), where it was a true move.
