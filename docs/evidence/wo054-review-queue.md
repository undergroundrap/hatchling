# WO-054 slice 4a: the review queue

Part of [#718](https://github.com/undergroundrap/hatchling/issues/718), [WO-054](../workorders/workorder-054-background-runs.md). The maintainer approved the protocol change on 2026-11-26.

## What it is

What a background loop may not apply on its own waits here for the owner (the maintainer's decisions of 2026-11-26):
- replacing or removing a preference, a boundary or a commitment;
- a fact about who the owner is;
- anything a loop's checks couldn't resolve;
- installing new guidance.

The queue never blocks a loop.

- **The protocol (UI schema v13):** `UiAction::ManageReview { op }` with `ReviewOp::List` and `ReviewOp::Decide { item_id, approve }`, answered `UiActionResult::Review { outcome: ReviewResult }`.
  - The maintainer's question named two actions, `read_review_queue` and `decide_review`. They're the house's one action with an op, as `manage_suggestions` is, and carry the same two operations.
  - The schema, `generated.ts` and `wire.ts` are regenerated. The live adapter's three exhaustive maps name it.
- **The store** (`review_queue.rs`) is sealed in the schedule store, under its key: one entry per item and an index, apart from every schedule's, so a schedule op can't name one.
- **An item:**
  - its loop and kind;
  - its target record, by handle;
  - what would change, and why;
  - the handles it cites;
  - when it was proposed, where it stands, and when it was decided.
- **Append-only:**
  - a waiting item is never removed, and a decided one is kept as decided;
  - when the queue is full (256, so its index fits one sealed entry), the oldest decided item gives way;
  - a queue of waiting items alone refuses another, and the loop goes on without it.
- **Bounds:** its change, its reason, its handles and their count. An item that keeps them but would still be escaped past one sealed entry is refused, never failed on.
- **A decision is recorded, not applied here:**
  - only a waiting item is decided, once;
  - an approved one is applied by its loop's apply step (slice 5, with the memory loop), which rechecks its target and marks it `stale` when that changed;
  - a rejected one changes nothing.

**What isn't here:** the app's Review page (4b), and the loops that propose and apply (the memory loop's work order).

## Tests

In `hatchling-broker`'s `review_queue::tests`:
- proposals wait, the newest first, sealed under the store's key;
- a waiting item is decided once, approved or rejected, and a decided one isn't decided again; an id that names none, a path's worth or one too long, isn't found;
- what breaks a bound is refused and nothing is kept, while one at each bound is kept;
- full, the oldest decided item gives way, never a waiting one; a queue of waiting items alone refuses another;
- the owner's operations are answered: `off` with no store, `failed` under another key;
- an item that JSON escapes past one sealed entry is refused, and a full index fits one.

`hatchling-core`'s tests pass with the version at 13. The frontend's check passes with its maps naming the new action: typecheck, lint, protocol, and 622, 266 and 16 tests.

## The gate

RESULTS
