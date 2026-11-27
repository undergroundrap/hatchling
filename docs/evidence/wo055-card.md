# WO-055 slice 2: a task's memories gather on one card

Part of [#753](https://github.com/undergroundrap/hatchling/issues/753), [WO-055](../workorders/workorder-055-several-memories-one-card.md). Builds on slice 1's ledger ([wo055-ledger.md](wo055-ledger.md)).

## What changed

**The broker** (`approval_board.rs`):
- `propose_memory` looks for the memory card its task has waiting (`Approvals::waiting`).
  - **With none,** it puts a card of one, as a memory was always asked.
  - **With one,** the new memory joins it as a revision (`Approvals::revise`). The card is shown again, so a decision on the card as it was is refused (`StaleRevision`). The card keeps its first deadline, scopes and grant.
- **What's refused** (`CardRefusal`):
  - a sixth memory (`Full`, at `MAX_BATCH` = 5);
  - a second correction of the memory another item corrects (`CorrectsTheSame`);
  - the ledger's own reasons, as before.

  The same words again are the memory already on the card, answered as asked, with nothing shown again. A card decided or lapsed between the look and the revision starts a new card.
- **The frozen action.** One memory is frozen as before (format 2), so its digest is unchanged. Several are format 3: `{"format": 3, "scope": "user", "op": "memory.keep", "items": [...]}`, each item a memory's fingerprint. Each item is also given to the ledger as a part, so a denied card refuses each memory on it, on any card.
- **The card** (`memory_card`).
  - One memory is shown as before.
  - Several have one broker field, the operation; their `items` each carry the agent's words and the memory's fields; the summary is "Remember these N things?"; and the approve button is "Remember them".
- **Approving keeps each memory as its own** (`kept`, `kept_one`), with its own record of the one approval. Each is said; one that can't be kept doesn't hide the others.
- **Approval history records a batch whole.** The items' fields are kept in the record's fields, each named by its place ("Memory 2: What it would remember"), so `DecisionRecord`'s wire shape is unchanged.

**The wire** (`hatchling-core`):
- `ConfirmationRequest.items: Vec<ConfirmationItem>`, each a list of fields. It's empty for anything that isn't a batch, and not serialized when empty.
- UI schema v15, as the maintainer approved on 2026-11-27.

**The app:**
- `TaskApproval.items`;
- the card and the history record list the items in order, each with the same `reviewFields` view (hidden characters shown, the agent's words labelled);
- the plain-text review numbers them.

**Docs:** `local-api.md` (the confirmation's `items`, and bounded asking), `arch/protocol.md` and `arch/distribution.md` (v15), and the CHANGELOG.

## Tests

`approval_board::batch_tests`:
- `a_tasks_memories_gather_on_one_card_shown_at_each_revision`:
  - three asks are one card at revisions 1, 2 and 3, each shown, with the first's deadline;
  - one memory is a plain card;
  - three are a batch, with each item's fields, sources and words;
  - the digest is format 3, pinned;
  - approving revision 2 is `StaleRevision`, and approving revision 3 runs all three;
  - once it's decided, the next ask starts a new card.
- `the_same_words_again_are_the_memory_already_waiting`: not new, and not shown again.
- `a_card_holds_five_and_no_more`: the sixth is `Full`, and the card is unchanged.
- `one_card_corrects_a_memory_once`: a second correction of `kept-1` is `CorrectsTheSame`, while a correction of `kept-2` and a plain memory join.
- `a_denied_card_refuses_each_memory_and_cards_are_per_task`:
  - each task has its own card;
  - after a denial, an item is refused alone from another task, and on that task's waiting card;
  - a site request from a task with a waiting card is still `QueueFull`.
- `a_decided_batch_is_recorded_whole`: the record's fields name each memory's words and kind by its place.

`memory_keep::tests::a_card_of_two_keeps_each_as_its_own`: two asks through `propose`, then one approval run by the worker. Both are kept, the owner is told of both, and both records name the one approval.

`hatchling-core`: `a_batch_cards_items_have_their_wire_form` pins the items' wire form, and that a card from before v15 reads as none. The version pin is 15.

The app: `a batch card lists each memory on it, in order, each with its own fields (WO-055)` covers the items mapped, the plain-text review numbered, and a card that isn't a batch having none.

## Hand-breaks

Each one fails at least one of the tests above:

1. a card is never revised (each ask a new request);
2. a card holds more than five;
3. two memories may correct the same one;
4. the same words again are a new item;
5. only the first memory is kept;
6. a batch's items aren't recorded;
7. a card's items aren't given to the ledger.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- **The whole workspace's tests:** 4688 passed, 0 failed.
- clippy with `-D warnings`, the whole workspace, on 1.98.1 and 1.85.0: clean.
  - The first run found a wildcard match and two `panic!`s in the new tests. They were rewritten as an `if let` and a helper with `unwrap`.
- `hatchling-dev audit`: all checks passed. `hatchling-dev protocol-schema --write`: all checks passed, with the schema and the app's generated types committed.
- **Hand-breaks:** all seven caught.
  - A first run of the breaks passed its arguments to the editing step in the wrong order, so it tested unbroken code. That was caught by reading its output, and the run was repeated with the order fixed.
  - "A card is never revised" fails 7 tests. Each of the others fails 1.
- **Mutation** on the slice's change against slice 1 (`--in-diff`, `hatchling-broker` and `hatchling-core`): 29 mutants, 16 caught, 13 unviable, **none missed**.
- **The app:** `npm run check`, green.
  - Unit tests: 630 passed.
  - Live-adapter tests: 274 passed, at 100% line, branch and function coverage.
  - Operator tests: 16 passed.
