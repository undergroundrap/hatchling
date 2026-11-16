# WO-040: The room disk's capacity, reserved and grown

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (`room_disk`, the spawner's disk operations, `doctor`), `crates/hatchling-sys` (the disk's file), the operator commands, `docs/storage.md`
**Issue:** [#485](https://github.com/undergroundrap/hatchling/issues/485)
**Trust boundary:** yes (the agent's encrypted disk)

Moved out of [WO-034](workorder-034-run-commands-in-the-agents-encrypted-room.md) on 2026-11-16, by the maintainer's decision: its third acceptance item was never built, and it is work of its own.

## Problem

The agent's room disk is a sparse file of a fixed size. Nothing reserves the space it may grow into, so the host can run out underneath it, and the agent's writes then fail in ways the owner isn't told about. And nothing grows it: an owner whose agent needs more room has no way to give it more without wiping it.

## Scope

**In scope:**

- Reserving the disk's configured size on the host when it's made, so the space is the disk's.
- Growing a closed disk, grow-only, keeping every byte already on it, and recovering from an interrupted grow without loss.
- A refusal at the free-space boundary that says what's free and what was asked, at creation and at growth.
- `doctor` saying the disk's size, what it holds, and what the host has free.

**Not in scope:** shrinking a disk; growing one that's open; more than one disk.

## Acceptance criteria

- [ ] A new disk's space is reserved on the host (no sparse holes), proven as root on a real filesystem.
- [ ] A closed disk grows to a larger size, its existing files byte for byte the same after, proven by a canary written before.
- [ ] A grow interrupted at each step (before the file grows, after it grows and before the volume does, after the volume and before the filesystem does) is completed, or undone, at the next open, and loses nothing.
- [ ] Creating or growing a disk past what the host has free is refused before anything changes, with what's free and what was asked.
- [ ] An open disk can't be grown; the refusal says to close it first.
- [ ] Each guard is hand-broken and its test fails for the right reason.

## Gates

As [TEMPLATE.md](TEMPLATE.md), plus the root pass (`HATCHLING_ROOT_PASS=1 bash scripts/check.sh`).

## Evidence

Filled in at close.
