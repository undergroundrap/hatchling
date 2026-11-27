# WO-048 slice 4: absences kept as absences

Part of [#598](https://github.com/undergroundrap/hatchling/issues/598), [WO-048](../workorders/workorder-048-memory-provenance-and-recall.md). It builds on slice 3. The maintainer approved the wire change on 2026-11-26, and absences were added to the scope on 2026-11-23.

## What it is

Recall used to have no way to say "this is known not to be so". Without that, the agent works a settled answer out again, and now and then asserts the opposite.

- **A proposal can record a settled negative** (session protocol v36, `MemoryOp::Propose`'s `absence`): a claim found untrue, something that doesn't exist, a thread closed. Its words say the negative. It's proposed and approved like any memory, of any kind, with the same provenance. A later slice lets it be superseded.
- **The owner's card** adds, in the broker's words, that it records something isn't so and that the agent takes it as settled. The approval freezes `absence`, so the owner's yes is to the negative.
- **Kept,** it's the item record's `absence` (slice 1's field).
- **Recalled,** its header says `KNOWN NOT TO BE SO` before its kind and dates, and the block's intro says what the mark means: take it as known, not as something missing, and don't work it out again. Recall that finds nothing has no item, so the two never read alike.
- **The agent's tool** takes `absence` (true or false), described with examples.

## Tests

- **`hatchling-memory`, `prompt::tests`:** a kept absence is recalled marked as one. Empty recall has no item, and only the intro names the mark. A kept item that isn't an absence isn't marked.
- **`hatchling-broker`:**
  - `memory_proposal::tests`: an absence is checked and carried;
  - `approval_board::tests`: the approval freezes `absence` only when it's set, and the card adds its field;
  - `serve::memory_proposal_tests`: an absence sent through a real session is kept as one.
- **`hatchling-agent`, `tools::tests`:** the tool sends `absence` on the wire.

## The gate

The gate script (`dr-k4.sh`) on tree `0286df77`, the slice on slice 3:
- fmt passes, clippy passes on 1.98.1 and 1.85.0, and the workspace builds;
- the workspace's tests: 4642 passed, 0 failed;
- `hatchling-dev audit`: all checks passed.

The protocol schema and the frontend's wire are regenerated at v36, and the frontend's check passes: 627, 271 and 16 tests.

**Six hand-breaks, each caught by its tests:**
- the check drops an absence;
- the approval doesn't freeze it;
- the card doesn't say so;
- the record doesn't keep it;
- the session drops it;
- recall doesn't mark it.

**Mutation** on the slice's own diff, with none missed and none timed out:
- broker: 8 mutants, 4 caught, 4 unviable;
- memory: 4 mutants, 4 caught;
- agent: 3 mutants, 1 caught, 2 unviable.
