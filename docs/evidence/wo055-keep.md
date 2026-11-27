# WO-055 slice 3: Keep and Skip

Part of [#753](https://github.com/undergroundrap/hatchling/issues/753), [WO-055](../workorders/workorder-055-several-memories-one-card.md). Builds on slice 2 ([wo055-card.md](wo055-card.md)).

## What changed

**The ledger** (`approvals.rs`): `Approvals::decide_items` takes `keep`, the places of the items an approval keeps, on the revision the owner was shown.
- It's taken only with an approval, on a batch (two items or more), and some places, each in range and named once (`kept_places`). Anything else is `DecisionRejection::InvalidKeep`, and nothing changes.
- Each item left out has its fingerprint refused from then on, as a denial's are, on any card and from any task. The task isn't paused: the owner said yes to the rest.
- The places kept are on the request (`Request::kept`).

**The board and the worker** (`approval_board.rs`):
- `ApprovalBoard::decide_items` and `decide_as_owner` carry `keep`.
- The worker keeps only the places kept (`kept_of`).
- Approval history names a skipped item "Memory 2, skipped: …".

**The API** (`api_ops.rs`): `decide`'s `keep` goes to the board.

**The wire** (`hatchling-core`), within the shape the maintainer approved on 2026-11-27:
- `UiAction::Decide.keep: Option<Vec<u32>>`, left out when `None`;
- `DecisionRejection::InvalidKeep` (`invalid_keep`).

**The app:**
- Each item on a batch card has a Keep box, ticked to begin with.
- The approve button reads "Keep N" when some are unticked, and can't be pressed with none ticked.
- The choice belongs to the card's revision: a memory added while the card is open starts it again, with the new one ticked.
- The decision sends `keep` only when some are left out.
- The demo has a "Several memories on one card" scenario.

**Docs:** `local-api.md` (`keep` and `invalid_keep`), and the CHANGELOG.

## Tests

`approvals::tests`:
- `keeping_some_items_refuses_the_rest_and_pauses_nothing`:
  - `[2, 0]` of three keeps `[0, 2]`;
  - the skipped item is refused for another task, while a kept one isn't;
  - the task asks again at once;
  - keeping every place refuses nothing;
  - a plain request says nothing kept.
- `a_keep_that_cant_be_kept_is_refused_and_changes_nothing`: a denial, an empty keep, out of range (2 and `u32::MAX`), a repeat, a card of one, and a request without items are each `InvalidKeep`, with the request still pending and nothing refused. A stale revision is still `StaleRevision`, and the card can be decided after.

`memory_keep::tests::keeping_some_of_a_card_keeps_those_alone`: of three asks, keeping `[0, 2]` through the worker keeps those two alone. The owner is told of both, history marks "Memory 2, skipped", and the skipped words asked again by another run are refused.

`api_ops::tests::the_owners_keep_reaches_the_card_it_decides`: the owner's `decide` with `keep: [1]` reaches the ledger, and an empty `keep` is `invalid_keep`, through the real handler.

`hatchling-core`: `a_decision_keeping_some_items_has_its_wire_form` pins `keep`, its absence, a decision from before v15, and `invalid_keep`.

**The app:**
- `keeping some of a batch sends the places kept on the revision shown; keeping all sends none (WO-055)`: the action sent, `invalid_keep`'s words, and no `keep` for all.
- The e2e test `test_05c_a_batch_card_keeps_what_stays_ticked`: three items and three ticked Keep boxes with their names, "Remember them", "Keep 2" after unticking one, disabled with none, "Keep 1", then approved and gone.
  - CI runs it.
  - Its steps were also driven by hand in a browser on the built demo page. Each matched, and the card was gone after approving.

## Hand-breaks

Each one fails at least one of the tests above:

1. a keep with a denial is taken;
2. a keep on a card of one is taken;
3. an empty keep is taken;
4. a place out of range is taken;
5. a repeated place is taken;
6. a skipped item isn't refused;
7. the worker keeps every item;
8. the record doesn't say skipped;
9. the API drops the owner's keep.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- **The whole workspace's tests:** 4694 passed, 0 failed.
- **clippy** with `-D warnings`, the whole workspace, on 1.98.1 and 1.85.0: clean.
  - The first run found `Ops::handle` one line over its limit with `keep` in it, and an unneeded `std::path::` in a test.
  - The owner check moved into `Ops::decide`, whose arm is now one call. It's hand-broken separately: a non-owner deciding fails `only_the_owner_decides_and_an_unknown_confirmation_is_rejected`.
  - After that: broker lib 1951 passed, clippy clean on both, and the API's `keep` break caught again in the new form.
- `hatchling-dev audit`: all checks passed. `protocol-schema --write`: all checks passed, with the schema and the app's generated types committed.
- **Hand-breaks:** all nine caught.
- **Mutation** on the slice's change against slice 2 (`--in-diff`, `hatchling-broker` and `hatchling-core`): 35 mutants, 26 caught, 9 unviable, **none missed**.
- **The app:** `npm run check`, green.
  - Unit tests: 633 passed.
  - Live-adapter tests: 277 passed, at 100% line, branch and function coverage.
  - Operator tests: 16 passed.
