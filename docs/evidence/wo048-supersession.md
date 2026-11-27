# WO-048 slice 5: supersession, and contradictions as proposals

Part of [#598](https://github.com/undergroundrap/hatchling/issues/598), [WO-048](../workorders/workorder-048-memory-provenance-and-recall.md). It builds on slices 1 to 4. The maintainer approved the wire change on 2026-11-26.

## What it is

- **Recall names each item by its id**, so a run can say which memory it means.
- **A proposal can name the memory it corrects** (session protocol v37, `MemoryOp::Propose`'s `supersedes`).
  - The broker accepts only a memory the owner approved and that's recalled as it is (`memory_keep::recalled_kept`).
  - An id nothing is recalled with, a run's own note, or a path is `Unasked`, and the agent is told to name one by the id it's recalled with.
- **The owner's card** quotes what it replaces, in the broker's words, and says the agent isn't given that any more once they approve. The approval freezes both the id and the words.
- **Approved:**
  - the new memory is kept with `supersedes` in its record;
  - the old one is retagged (`MemoryStore::retag`) from `recall` to `superseded`. So it's no longer recalled or listed, but its words, times and record stay, so the chain can be seen;
  - a memory from before records keeps the broker's old `approved` tag beside `superseded`.
- **What changed meanwhile:** a memory forgotten or changed between the proposal and the approval isn't touched. The new one is kept, and the owner is told nothing else changed.
- **Contradictions become proposals.** The agent's standing guidance covers two cases: what a run finds contradicts a recalled memory, or the conversation's notes (WO-052) record a decision that does. Either way it tells the owner rather than acting silently on either, and proposes the correction by that memory's id, quoting what contradicted it.

**What isn't here:**
- the owner's list showing the chain (slice 8);
- background loops proposing corrections, which go to WO-054's review queue with their loop's work order.

## Tests

- **`hatchling-memory`:**
  - `store::tests`: retagging changes the tags alone;
  - `prompt::tests`: each recalled item's header ends with its id.
- **`hatchling-broker`, `memory_keep::tests`:**
  - a correction names a memory the owner approved: an unknown id, a run's note and a path are each refused;
  - the card quotes what it replaces;
  - approved, only the correction is recalled, its record names the one it replaced, and the old one is kept, retagged, with its record as it was. The owner's list doesn't show it;
  - a memory from before records keeps its old approval tag when replaced;
  - one forgotten before the correction is approved leaves nothing else changed, and the owner is told.
- **`hatchling-broker`, `approval_board::tests`:** the approval freezes `supersedes` and the words replaced.
- **`hatchling-agent`, `tests/memory_recall.rs`:** a scripted model, against the real broker pipeline, is given an approved memory recalled with its id. Its correction by an unknown id is refused, and the one by the memory's id is put to the owner. WO-045 measures how often real models do this.
- **`hatchling-agent`, `tools::tests`:** the tool sends `supersedes` on the wire.

## The gate

RESULTS
