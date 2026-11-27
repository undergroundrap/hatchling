# WO-055 slice 4: the agent knows its asks share a card

Closes [#753](https://github.com/undergroundrap/hatchling/issues/753), [WO-055](../workorders/workorder-055-several-memories-one-card.md). Builds on slices 1 to 3 ([ledger](wo055-ledger.md), [card](wo055-card.md), [keep](wo055-keep.md)).

## What changed

`crates/hatchling-agent/src/tools.rs`, `memory_propose`:
- **The description** says: "One thing per call: for two things, call it twice. What you ask for in one answer is put to them together, on one card, up to five."
- **The result of an ask that was put to the owner** says it's on one card with anything else asked in that answer, each kept only if approved.

**The broker** (`memory_keep.rs`, from slice 2) tells the agent, as an error since #751:
- when a card is full: "the card waiting for the owner already holds five memories: ask for the rest once they've answered";
- when another memory on the card already corrects the same one.

These words now have their own test.

**Left out, by the WO's rule that the session protocol is unchanged:** telling the agent how many others share its card. That would need `MemoryResult::Proposed` to carry a count.

**WO-055 closes:** each acceptance criterion is ticked against the test that shows it, and the queue row is `CLOSED`.

## Tests

- `memory_keep::tests::a_full_card_and_a_second_correction_are_told_to_the_agent`, through `propose`:
  - a correction of a kept memory is asked, and a second correction of it is refused with its words;
  - four more asks fill the card to five, and a sixth is refused with its words.
- `tools::tests::memory_propose_asks_the_owner_and_says_what_became_of_it` pins the description's sentence about the card, and the whole result of an ask (`asked` and its note).

## Hand-breaks

Each one fails its test:

1. the full card's words lose "ask for the rest once they've answered";
2. the second correction's words become "it couldn't be put to the owner";
3. the result's note loses that the asks share a card;
4. the description loses the card.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- **The whole workspace's tests:** 4695 passed, 0 failed.
- clippy with `-D warnings`, the whole workspace, on 1.98.1 and 1.85.0: clean.
- `hatchling-dev audit`: all checks passed, with WO-055 `CLOSED` and every criterion ticked.
- **Hand-breaks:** all four caught.
  - A first run passed each string to an editor that required exactly one match. The strings appear in both the code and its test, so no break was applied.
  - Rerun breaking the first occurrence, the code's.
- **Mutation:** none. This slice changes only strings, which cargo-mutants doesn't mutate. The tests above pin them and the hand-breaks show the pins hold.
