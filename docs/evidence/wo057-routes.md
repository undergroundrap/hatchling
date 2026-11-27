# WO-057 slice 2: a memory that waits on the owner's Review

Part of [#767](https://github.com/undergroundrap/hatchling/issues/767), [WO-057](../workorders/workorder-057-automatic-memory.md). Slice 1 is [wo057-taint.md](wo057-taint.md).

## What changed

- **Where a memory goes, in automatic memory:**

  | The conversation | Before (slice 1) | Now |
  | --- | --- | --- |
  | A chat's, clean, under the day's cap | kept at once | kept at once |
  | A chat's, clean, past the cap | a card | a card |
  | A chat's, having read from outside | a card | **waits on the Review page**, and the chat says so |
  | A background or scheduled run's, or a run the chat didn't start | a card | **waits on the Review page**, without a word in the chat |
  | Any, in Ask me | a card | a card |

  Words the owner denied or skipped never wait or are kept.
- **The review queue's `remember` kind** (UI schema v17):
  - Each item says who asked (`chat` or `run`), the words, why it waits (with the agent's own reason after it), what it would correct (`memory:<id>`), and the run and chat it came from.
  - The whole checked proposal (kind, reasons, dates, absence, what it corrects) is sealed beside the item (`review_queue::propose_with`, `detail`).
  - The same words waiting already are asked once.
- **The decision is applied as it's made** (`memory_keep::reviewed`), not later by a loop:
  - **Approved:** kept at once on the owner's word, the record's approval the item's id, and "Remembered: …" in the chat. A correction retires what it corrects.
  - **Rejected:** its words are refused from then on, as a denied card's are (`Approvals::deny`).
  - **An item whose proposal can't be read:** the owner is told it wasn't kept.
- **The agent** is answered `Proposed`, as for a card: no session protocol change. Its note now says the owner was asked "on a card … or on their Memory page".
- **The app's Review page** names the kind "Something to remember about you". An approved one reads "You approved it, so it's remembered", and deciding says "Approved. It's remembered."

## Tests

| Rule | Test |
| --- | --- |
| After reading from outside, it waits once, with who, why and where from, the chat told, no card, nothing kept | `one_asked_after_reading_from_outside_waits_on_the_review_page_once` |
| Approved, kept on the owner's word with every field | `approved_on_the_review_page_it_is_kept_on_the_owners_word` |
| Rejected, refused from then on, clean or not | `rejected_on_the_review_page_its_words_are_refused_from_then_on` |
| A background run's waits, silently | `a_background_runs_memory_waits_without_a_word_in_the_chat` |
| Ask me is a card | `in_ask_me_or_with_no_review_page_it_is_a_card` |
| A correction waits naming its target, and approved retires it | `a_correction_waits_naming_what_it_corrects_and_approved_retires_it` |
| A lost proposal is said, not kept | `approved_without_its_proposal_the_owner_is_told_it_wasnt_kept` |
| Several wait alone, each kept as asked; another kind in the same words isn't the same | `each_memory_waits_alone_and_is_kept_as_it_was_asked` |
| **End to end, a real session:** a web search first, then the memory, waits; so does a run the chat didn't start | `a_clean_chat_run_keeps_at_once_and_one_that_read_from_outside_asks` |
| The page's words | `live-review.test.mjs` |

## Hand-breaks

| Break | Failed |
| --- | --- |
| Nothing waits on the Review page | 6 |
| Ask me queues | 2 |
| Denied words wait again | 2 |
| The same words wait twice | 1 |
| The chat isn't told it waits | 1 |
| A rejection refuses nothing | 1 |
| An approval keeps nothing | 2 |
| A decision isn't applied | 4 |
| The detail isn't sealed | 3 |
| The session gives no key | 1 |
| The page names the kind otherwise | 1 (frontend) |
| An approved memory reads as applied later | 1 (frontend) |
| Deciding one says it's applied later | 1 (frontend) |

## Gates

In the WSL2 sandbox, with `HATCHLING_REQUIRE_CELL=1`:

- **The workspace's tests:**
  - 4735 passed. The 3 that failed were the audit's own tests, which caught a doc still saying UI v16.
  - With it fixed, `hatchling-dev`'s 236 pass, and the memory and review tests, with the last one added, 40.
- **Clippy, `-D warnings`, all targets and features:** clean on 1.98.1 and 1.85.0. `cargo fmt --check` is clean.
- **`hatchling-dev audit`:** all checks passed.
- **Mutation, `cargo mutants --in-diff`, kill floor 1.0:** 28 mutants, 22 caught, 6 unviable, none missed.
  - A first pass left 3: two in the check for the same words already waiting, and the detail's entry name.
  - `each_memory_waits_alone_and_is_kept_as_it_was_asked` catches all three.
- **Frontend:** `npm run check` green (640, 280 and 16 tests). The wire types were regenerated: UI schema 17, and `ReviewKind` gains `remember`.

## Not in this slice

- The app's setting (Automatic or Ask me), the Forget link on the chat's notice, and the Memory page's "Kept automatically" label and filter are slice 3, at UI v18.
