# WO-047: Memory and goals sealed where they live, like the vault's other stores

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-memory` and `crates/hatchling-goals` (their stores), `crates/hatchling-vault` (two new sealed stores), `crates/hatchling-broker` (opening, migration, backup and restore, key rotation), `docs/`
**Issue:** [#597](https://github.com/undergroundrap/hatchling/issues/597)
**Trust boundary:** yes

## Problem

Ten of Hatchling's stores are sealed under the vault's key where they live: secrets, schedules, artifacts, spaces, sites, profiles, credentials, the conversation, the Library and redactions. A copy of the disk opens none of them without what opens the vault.

Memory and goals aren't among them. They hold what the agent knows about its owner, each item words the owner approved, and they are plain JSON files under directories only Hatchling's users can open. Anyone who can read the disk can read them. The trust boundary, the scorecard, *Your security* and the Windows guide each say so, and the vault's backup seals them only in the copy.

These two stores are what an owner would most expect to be sealed, and the pattern that seals the other ten is built and proven.

## Scope

**In scope:**

- Memory items and goals sealed where they live, each as a store of the vault's own, under the vault's key, by the pattern the other sealed stores use. Every read and write goes through the seal; a store that's sealed is never also written in the clear.
- What's there already moved over once, at the broker's start or by `migrate-vault`, with nothing lost and no plaintext copy left behind, and a migration interrupted at any point safe to run again.
- Backup, restore and `reset agent` carrying the sealed stores, as they do the others.
- Key rotation re-sealing both, at every keyring tier.
- While the vault is locked, neither store is read or written.
- The docs that say memory and goals aren't sealed say what's true after: the trust boundary's *At rest* row, the scorecard's limitation, *Your security* and the app's copy of it, and the Windows guide.

**Not in scope:** the other stores still not sealed where they live (feed posts, decisions, ideas, run records, coding tools' temporary files, the usage and activity logs); memory's provenance, supersession and recall (WO-048).

## Acceptance criteria

- [ ] On a fresh install and after migration, no memory item's or goal's words are on the disk in the clear: a test writes known words and searches every file under Hatchling's state for them.
- [ ] Migration from 0.9.0's plaintext stores keeps every item and goal, leaves no plaintext copy, and is safe to re-run after being interrupted at each step; tests cover each.
- [ ] A sealed item or goal changed on disk, swapped with another, or moved to another name is refused when read, as the other sealed stores refuse it.
- [ ] Backup and restore, `reset agent`, and key rotation at each keyring tier carry both stores; tests cover each.
- [ ] Recall, *Remember this*, goals and the Feed's reads of them work as before, on a real install.
- [ ] Each new guard fails its test when removed.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

- Slice 1, the handoff's capacity (#638): [#639](https://github.com/undergroundrap/hatchling/pull/639), whose description holds its gate.
- Slice 2, memory sealed: [memory-sealed.md](../evidence/memory-sealed.md).
- Slice 3, goals and tracking sealed: [goals-sealed.md](../evidence/goals-sealed.md).

The acceptance criteria are ticked at close, against these and slice 4's.

## Notes

- On v0.10.0 after WO-046, by the maintainer's decision of 2026-11-22. The rest of memory is WO-048, on v0.11.0.
