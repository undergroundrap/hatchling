# WO-048 slice 8: the owner's list

Part of [WO-048](../workorders/workorder-048-memory-provenance-and-recall.md), its last slice, closing [#598](https://github.com/undergroundrap/hatchling/issues/598). The maintainer approved the wire change ("the app's memory list gains the record and a confirm operation") on 2026-11-26.

## What it is

- **Each kept memory's record, listed** (UI schema v14, `Kept`):
  - where it came from (`KeptFrom`): a chat with its id, a scheduled run, an approval from no run, or not recorded for one kept before records;
  - its kind, why and when it applies, and whether it's an absence;
  - when it was kept and last confirmed;
  - its day to check it again, and whether that day has passed (`due`);
  - the words of the memory it replaced, read from the store.

  What it replaced isn't listed itself (slice 5).
- **Confirming** (`KeptOp::Confirm`): the owner says a memory is still so. Its record's `confirmed_at` is now, and its check-again day is cleared (`MemoryStore::rerecord`, which keeps everything else and updates the recall index). Only a memory with a record is confirmed: one from before records has nothing to say it, and is `not_found`.
- **The Memory page** shows, under each memory:
  - its kind and absence as pills;
  - why, when it applies, and what it replaced;
  - where it came from and when it was last confirmed.

  When its day has come, it asks "Is this still so?" with **Still so**. Forgetting is as it was.
- **Confirming is never a Command.** It goes straight to the broker through `MemoryDetailsPort`, so nothing but the owner's own click confirms a memory. `MemoryEntry` (the locked protocol model) is unchanged: the details ride beside it.

## Tests

- **`hatchling-broker`, `memory_keep::tests`:**
  - the owner's list shows each memory's record: from a chat, from an approval, not recorded;
  - its kind, reasons and dates, and what it replaced;
  - due once its day has come;
  - confirmed, not asked about again;
  - nothing to confirm for one from before records, or none at all;
  - a day is due only after it.

  The provenance test's words now also claim a date the record doesn't keep.
- **`hatchling-memory`, `store::tests`:** rerecording changes the record alone, and an item that isn't there is said not to be.
- **`hatchling-core`, `ui::tests`:** the v14 wire is pinned: a full record, one from before records, an older broker's listing read as not recorded, the confirm op and its answer.
- **The frontend, `live-memory.test.mjs`:**
  - a kept memory's details read in the page's words, for every kind and origin;
  - they're kept beside the list;
  - confirming reads the list again, and every answer is said;
  - an older broker's listing, saying nothing of where a memory came from, reads as not recorded.

  The live adapters keep full line, branch and function coverage. A change's answer is typed so it can't be a listing, which leaves no unreachable case.

## The gate

**The gate script** (`dr-k8.sh`) on tree `8b34d9be`:
- fmt passes, clippy passes on 1.98.1 and 1.85.0, and the workspace builds;
- the workspace's tests: 4674 passed, 0 failed;
- `hatchling-dev audit`: all checks passed.

The protocol schema and the frontend's wire are regenerated at UI v14, and the frontend's check passes: 629, 273 and 16 tests.

**Six hand-breaks of `memory_keep.rs`, each caught by its tests:**
- confirming doesn't clear the day it's asked about;
- confirming doesn't say when;
- one from before records is confirmed as if it had one;
- it's due on the day itself;
- where it came from loses its chat;
- what it replaced isn't said.

**Mutation** on the slice's diff:
- broker (`memory_keep.rs`): 11 mutants, 7 caught, 4 unviable;
- memory (`store.rs`'s `rerecord`): the first run missed both of its mutants, since it was tested only through the broker. With its own test, 2 mutants, 2 caught.

None were missed in the end, and none timed out.
