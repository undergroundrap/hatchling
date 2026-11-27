# WO-048 slice 2: no duplicates

Part of [#598](https://github.com/undergroundrap/hatchling/issues/598), [WO-048](../workorders/workorder-048-memory-provenance-and-recall.md). Closes [#596](https://github.com/undergroundrap/hatchling/issues/596). It builds on slice 1's record ([wo048-kept-record.md](wo048-kept-record.md)).

## What it is

- **What the owner already approved isn't asked again.** Before a proposal is put to the owner, `memory_keep::propose` reads what's kept in their user scope. If they already approved the same words, it's `Unasked`, and the agent is told it's already kept and given in every run.
  - *The same words* means equal once whitespace is collapsed: "Their  cat\nis Biscuit." repeats "Their cat is Biscuit.". A correction in different words is asked, and it's the supersession slice's to link.
  - What a run wrote itself doesn't count: the owner never approved it, so the same words are still theirs to approve.
  - A store that can't be read asks anyway. Keeping checks again.
- **Nothing is kept twice.** `memory_keep::keep` makes the same check before it writes. An approval that finds the words kept meanwhile keeps nothing new, and the owner is told "Already remembered", with the words, and that they're kept once.
- **No wire change:** the reply is the existing `Unasked { reason }`.

## Tests

In `hatchling-broker`'s `memory_keep::tests`:
- what's already kept, in the same words however spaced, is neither put to the owner again nor kept twice. Different words are asked, and the same words a run wrote itself are asked too;
- an approval of words kept meanwhile keeps one copy and says so.

## The gate

The gate script (`dr-k2.sh`) on tree `b010b260`, the slice's code and docs on slice 1:
- fmt passes, clippy passes on 1.98.1 and 1.85.0, and the workspace builds;
- the workspace's tests: 4633 passed, 0 failed;
- `hatchling-dev audit`: all checks passed.

**Four hand-breaks of `memory_keep.rs`, each caught by its tests:**
- a proposal of what's kept is asked again;
- an approval keeps a second copy;
- what a run wrote counts as approved;
- spacing makes the same words different.

**Mutation** on the slice's own diff (`memory_keep.rs`, `approval_board.rs`, the broker's `memory` tests): 9 mutants, 7 caught, 2 unviable, none missed, and none timed out.
