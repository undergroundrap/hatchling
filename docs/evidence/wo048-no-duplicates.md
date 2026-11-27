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

RESULTS
