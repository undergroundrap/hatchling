# WO-055 slice 1: the ledger revises a waiting request, and refuses each denied item

Part of [#753](https://github.com/undergroundrap/hatchling/issues/753), [WO-055](../workorders/workorder-055-several-memories-one-card.md).

## What changed

In `crates/hatchling-broker/src/approvals.rs`:

- **`Approvals::revise(id, proposal, items, now)`.** A task may revise a request it has waiting, with a new action, such as one more memory on its card. The revision gets:
  - a new revision number and digest;
  - its fingerprints and run replaced.

  It keeps the request's **scopes, grant and deadline**, whatever the new proposal asks, so it can't widen what a yes allows or ask for longer.

  It is refused (`Refusal::NotWaiting`), leaving the request as it was, when the request isn't the task's own pending one: another task's, decided, lapsed or unknown. It's also refused when:
  - an item or the effect was denied (`Denied`);
  - the hour's requests are made (`Paused`).

  Each revision counts against `MAX_REQUESTS_PER_HOUR`, as a new request would. Revising to the same action changes nothing, and isn't put to the owner again.
- **`Approvals::propose_items(proposal, items, now)`:** a request made of items, each with its own fingerprint. A proposal or revision is refused if any item was denied, and a denial refuses every item, alone or on another card, from any task. `propose` is `propose_items` with none.
- **`Refusal::NotWaiting`.** The approval board, the browser and web ops, memory keep and schedule proposals never revise, so it can't reach them. Their matches send it with the "couldn't be put to the owner" errors.

The decision that keeps part of a card is slice 3's, with its wire change. **No wire change here.**

## Tests

In `approvals::tests`:
- `a_waiting_card_grows_by_revision_and_never_quietly`:
  - the revision is 2, with a new digest, and the run replaced;
  - the scopes, grant and deadline asked for by the revision are not taken;
  - a decision on revision 1 is `StaleRevision`, and on revision 2 is approved, running the new action;
  - a card revised late still lapses at its first deadline.
- `revising_a_card_to_what_it_is_changes_nothing`: not new, still revision 1, and approvable as shown.
- `only_a_tasks_own_waiting_card_is_revised`: another task's, an unknown id, a decided request and a lapsed request are each `NotWaiting`, and the request is unchanged.
- `a_denied_card_refuses_each_item_on_any_card`: after a denial of `[a, b]`, another task's card with `b` is refused, and a revision adding `a` is refused with the card unchanged. A request without items is refused by its effect alone, as before.
- `each_revision_counts_against_the_hour`: the 20th change in the hour is `Paused`, and an hour after the first, it isn't.

## Hand-breaks

Each one fails at least one of these tests:

1. a revision keeps its revision number;
2. a revision takes the proposal's scopes;
3. a revision takes a new deadline;
4. another task's request may be revised;
5. a denial doesn't refuse the items;
6. a revision isn't counted against the hour;
7. a revision with a denied item goes through.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- `approvals::` tests: 21 passed. `hatchling-broker`'s lib tests: 1939 passed, 0 failed.
- clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- **Mutation** on the change (`--in-diff`, `approvals.rs` and the four matches): 36 mutants, 26 caught, 10 unviable, **none missed**.
