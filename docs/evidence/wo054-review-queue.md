# WO-054 slice 4: the review queue

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

## The app's list

What waits is on the live Memory page, above what's kept: under **Waiting for you**, each proposal's change, its kind, why, and when it was proposed, with **Approve** (behind a confirmation that says what approving does) and **Reject**. Under **Decided**, what was approved, rejected or went stale, with where it stands.

- **Never a Command.** Deciding goes straight to the broker through a port (`ReviewPort`), as closing a goal does. The threat it stops: the agent, or an automation driving the app on its behalf, approving its own loop's proposal. No Command decides one, so the automation catalog can't reach it, and the broker keeps `manage_review` to the owner's connection besides.
- **Read when it can have changed:** when the connection opens, and whenever the owner opens Memory, since loops propose without the app being told.
- **With scheduled work off,** nothing runs in the background to propose anything: the list is empty, not a warning on every visit. Every other answer is said in a sentence.
- **Built from what's there:** the page's own entry cards, its section titles, `pill` and `button`. No new look.

**What isn't here:** the loops that propose and apply (the memory loop's work order).

## Tests

In `hatchling-broker`'s `review_queue::tests`:
- proposals wait, the newest first, sealed under the store's key;
- a waiting item is decided once, approved or rejected, and a decided one isn't decided again; an id that names none, a path's worth or one too long, isn't found;
- what breaks a bound is refused and nothing is kept, while one at each bound is kept;
- full, the oldest decided item gives way, never a waiting one; a queue of waiting items alone refuses another;
- the owner's operations are answered: `off` with no store, `failed` under another key;
- an item that JSON escapes past one sealed entry is refused, and a full index fits one.

`hatchling-core`'s tests pass with the version at 13.

In the frontend, `live-review.test.mjs` covers the adapter whole (lines, branches and functions at 100%):
- each kind and state reads in the page's words, and a time that isn't one shows none;
- the list is read once each time the connection opens, and again as the Memory page opens, never while disconnected;
- with scheduled work off nothing waits, and every other answer is said in its place;
- deciding sends the owner's choice and reads the list again, and every answer is said.

`live-adapter.test.mjs` checks the adapter end to end: listed on connecting, decided straight with the broker, the decision read back. The frontend's check passes: typecheck, lint, design, actions, protocol, seam, format, and 627, 271 and 16 tests.

## The gate

The gate script (`dr-rq.sh`) on the slice's first commit:
- the workspace's tests, 4620 passed, with clippy on 1.98.1 and 1.85.0 and the audit;
- four hand-breaks of `review_queue.rs`, each caught by its tests: a decided item decided again, a waiting item evicted, the entry's size bound, and the queue answered with scheduled work off.

Its mutation run on `review_queue.rs` left six mutants missed. Five of them were in an id check that only duplicated the index (an id the index doesn't hold is never read), so the check was removed. The sixth, `>` for `>=`, was at the entry's size bound, now named `fits` with its limit tested on each side.

On the fixed tree: the module's tests pass, clippy passes on both toolchains, and the mutation run on `review_queue.rs` (`--in-diff`, the broker's `review_queue` tests) tested 39 mutants: 34 caught, 5 unviable, 0 missed, 0 timeouts.
