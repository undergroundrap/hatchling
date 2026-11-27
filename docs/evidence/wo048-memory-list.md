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
- **The frontend, `live-memory.test.mjs`:**
  - a kept memory's details read in the page's words, for every kind and origin;
  - they're kept beside the list;
  - confirming reads the list again, and every answer is said.

## The gate

RESULTS
