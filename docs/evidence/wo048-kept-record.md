# WO-048 slice 1: the kept record

Part of [#598](https://github.com/undergroundrap/hatchling/issues/598), [WO-048](../workorders/workorder-048-memory-provenance-and-recall.md). Closes [#736](https://github.com/undergroundrap/hatchling/issues/736). The maintainer approved the wire changes on 2026-11-26.

## What it is

A kept item now has a **record** (`hatchling_memory::KeptRecord`), written by the broker alone when the owner approves keeping the item:
- whose words they are (`words`: the agent's, approved as shown);
- the approval that kept it, the run that proposed it, and that run's chat, as the broker knows them;
- when it was kept, and when the owner last confirmed it (when it was kept, until they confirm it again).

The record holds every field the work order's later slices fill: `kind`, `why`, `how_to_apply`, `check_by`, `absence` and `supersedes`. Those slices add behavior, not a new format.

- **Only the broker writes one.**
  - `MemoryStore::keep` is the only path that writes a record. The broker calls it after the owner's approval (`memory_keep::keep`).
  - A run's `MemoryOp::Write` goes through `MemoryStore::write`, which writes none. Writing over a kept item through it replaces the item with one that has no record, so it's no longer approved by the owner.
  - The proposal carries nothing of where it came from: the session fills that in (`serve.rs`, `ProposedIn`) from the cell the broker started (`run_cell`) and, for a chat run, its follow-up scope's channel. Words that claim another origin are kept as words.
- **Approved by the owner** means the item has a record, or, for an item kept before records, the broker's old `approved` tag. A run's write carrying that tag is now `Denied` whatever its grants (#736). Before, on an install granting `memory:user:write`, a run could mark its own item approved. No shipped install grants that.
- **A run is given each item with its dates.** The recalled block says, per item, either:
  - when it was kept and last confirmed, and that the owner approved it;
  - for an item from before records, when it was kept, that the owner approved it, and that where it came from isn't recorded;
  - for an item a run wrote, that it wasn't approved by the owner.

  The block also says each item was true as of then, so the agent weighs an old one against what it finds now.
- **A run isn't given where an item came from.** A recalled item reaches a cell (`Recall`, `RecallByTag` and the goals feed's memory query) with its record, but without the ids of the approval, the run and the chat. A run token names a run, so least disclosure keeps them out of later runs. The owner's list shows them (slice 8).
- **Items from 0.10 load unchanged.** The field is optional and omitted when absent, so an item without one reads and writes byte for byte as before, and a 0.10 reader ignores it.
- **The session protocol stays at v34.** The record is an added optional field inside `item_json`. `docs/session-protocol.md` says so under v18.

**What isn't here:** the record's later fields' behavior (slices 2 to 7), and the owner's list showing provenance (slice 8).

## Tests

- **`hatchling-memory`:**
  - `record::tests`: dates are calendar dates at the epoch, leap days and century edges; a record reads back and omits what it lacks; a shared record keeps its dates without the ids.
  - `item::tests`: a 0.10 item reads as not recorded, is approved only with the old tag, and writes back byte for byte; a shared item drops only the ids.
  - `store::tests`: `keep` writes the record as given; a run's write over it drops it and keeps when the item was made.
  - `prompt::tests`: each item says when it was kept and on whose word, in all three cases.
- **`hatchling-broker`:**
  - `memory_keep::tests`: a kept item's record names the approval that kept it, the run and chat the broker saw propose it, and when, whatever the words claim. The approve test now keeps the `recall` tag alone.
  - `memory_ops::paging_tests`: a granted write tagged `approved` is denied and writes nothing; a kept item recalled by id or by tag comes back with its record and no ids.
  - `serve::memory_proposal_tests`: through a real session, approved and kept, a chat run's proposal names its run and chat; a scheduled run's names no chat; a session that isn't a run's names no run.
- **`hatchling-goals`:** `feed::tests`: a kept item reaches the feed without where it came from.

## The gate

The gate script (`dr-k1.sh`) on tree `7b5c597f`, the slice's code and docs:
- fmt passes, clippy passes on 1.98.1 and 1.85.0, and the workspace builds;
- the workspace's tests: 4631 passed, 0 failed;
- `hatchling-dev audit`: all checks passed.

**Eight hand-breaks, each caught by its tests:**
- a run's write may tag an item approved;
- an item recalled by id carries where it came from;
- an item recalled by tag carries where it came from;
- the kept record names no approval;
- the session names no run;
- the goals feed hands a run where an item came from;
- an item with no record reads as approved;
- recall gives no dates.

**Mutation** (`--in-diff`, per crate):
- `hatchling-memory`: 85 mutants, 79 caught, 6 unviable;
- `hatchling-broker` (`memory_ops.rs`, `memory_keep.rs`, `serve.rs`, `approval_board.rs`, its `memory` tests): 12 mutants, 8 caught, 4 unviable;
- `hatchling-goals` (`feed.rs`): 2 mutants, 1 caught, 1 unviable.

None missed, and none timed out.
