# WO-055: Several memories, one card

**Status:** CLOSED
**Area:** `crates/hatchling-broker` (approvals, approval board, memory keep), `crates/hatchling-core` (UI protocol), `crates/hatchling-agent` (memory_propose), `frontend/` (approval card)
**Issue:** [#753](https://github.com/undergroundrap/hatchling/issues/753)
**Trust boundary:** yes (approvals: what the owner is asked, and what their decision keeps)

## Problem

At most one request per task waits for the owner (`MAX_PENDING_PER_TASK = 1`, `crates/hatchling-broker/src/approvals.rs`). That stops a task from burying the owner in cards until they approve without reading. But it also means the agent can ask to remember only **one** thing per turn. Told "remember these three things", it keeps one and loses two, and the refusal reaches the agent mid-turn.

WO-048's real-model trial (2026-11-27) showed what that costs. Gemma 4 12B split two facts into two proposals, as it should, and the second was refused both times. Its next answer, in a new conversation, recommended driving to an owner without a car and gave a distance in miles to an owner who asked for metric. Both were facts the owner had asked it to keep, lost to the limit, not to the model.

Decided by the maintainer (2026-11-27): **one batch card.** A turn's memory requests gather on one card, "Remember these?", with Keep and Skip on each item, up to five.

## Threats, and what stops each

| Threat | What stops it |
| --- | --- |
| **Flooding.** An agent, or text injected into it, asks again and again until the owner approves without reading. | Still one card per task. A card holds at most [`MAX_BATCH`] items (5), and each item added counts against the hourly limit on requests made ([`MAX_REQUESTS_PER_HOUR`]), as a new request would. |
| **Bait and switch.** An item is added while the owner reads, and their approval covers it unseen. | Each item added is a new **revision** of the request, with its own digest and the card shown again. A decision names the revision and digest it was shown, and the ledger refuses any other (`DecisionRejection::Changed`, already enforced). |
| **Smuggling through a partial yes.** The owner keeps two items and a third is kept as well, or another item than the one they ticked. | The decision names the kept items by their place on the revision it was shown, bound by that revision's digest. The broker keeps exactly those, as the frozen action holds them, and nothing else. |
| **Asking again what was skipped.** | Each item has its own fingerprint. Skipping an item refuses it for the broker's life, as a denial does now, however it's worded and from whichever task. Denying the whole card denies every item, and the task waits out the denial cooldown as before. Skipping some items while keeping others doesn't start the cooldown: the owner said yes to the task. |
| **Contradictory items.** Two items correct the same memory. | The second is refused when added, and the agent is told why. The same words asked again are the item already waiting, not a second one. |
| **A card that never ends.** Items keep the request alive past its time. | Adding an item doesn't extend the request's deadline: the card lapses when the first proposal's would have. |

Unchanged: only the owner decides; only the broker keeps; a memory proposal is checked (fit, kind, dates, `supersedes`) before it's put on a card; anything other than a memory proposal stays one request per task.

## Scope

**In scope:**
- `approvals.rs`: a pending request can be **revised** by its task with a new action (new revision, digest and fingerprints, the same scopes, grant and deadline). A request made of items keeps each item's fingerprint, each refused once denied or skipped. A decision may keep a subset of a request's items.
- `approval_board.rs` and `memory_keep.rs`: a task's memory proposals gather on its waiting memory request, up to five. The card carries its items. An approval keeps each kept item as its own memory, with its own record and the one approval.
- `hatchling-core`'s UI protocol:
  - `ConfirmationRequest` gains `items`: each item's fields, in order. A request that isn't a batch has none.
  - `UiAction::Decide` gains `keep`: the places of the items kept. Without it, an approval keeps every item. It's refused on a request without items, empty, out of range or repeated.
  - UI schema v15. The session protocol is unchanged.
- `hatchling-agent`'s `memory_propose`:
  - says that what it asks for in one answer goes to the owner together, on one card, up to five, and asks for one thing per call;
  - is told, as an error (#751), when the card is full or another memory on it already corrects the same one.

  Telling the agent how many others share its card would need the session protocol to carry the count, so it's left out: the protocol is unchanged.
- The app's approval card shows the items, each with a Keep box (ticked to begin with; unticking it is the skip), and the button says how many are kept ("Keep 2"). A card whose items change while open shows the new revision before anything can be decided, with every item ticked again.
- `docs/local-api.md`, `docs/session-protocol.md` (if the agent's result text is documented), `docs/arch/protocol.md`, the CHANGELOG.

**Not in scope:**
- Batching other kinds of request (sites, schedules, sign-ins). They stay one per task.
- The review queue for background loops (WO-054), which has its own list.
- Editing an item's words on the card. The owner skips it and says what they meant.
- Raising `MAX_PENDING_PER_TASK` or `MAX_PENDING`.

## Acceptance criteria

- [x] A chat turn that asks to remember three things puts one card to the owner, with three items. Keeping two keeps exactly those two, each as its own memory, and the third, in its words, is never asked again (the real-model trial of 2026-11-27 showed a rewording is a new memory, put to the owner again). ([card](../evidence/wo055-card.md): `a_tasks_memories_gather_on_one_card_shown_at_each_revision`; [keep](../evidence/wo055-keep.md): `keeping_some_of_a_card_keeps_those_alone`)
- [x] An item added while the card is shown makes a new revision. A decision on the old revision is refused, and the app shows the new card. ([ledger](../evidence/wo055-ledger.md): `a_waiting_card_grows_by_revision_and_never_quietly`; [card](../evidence/wo055-card.md): `StaleRevision` at revision 2. The app replaces a card by its newer revision and refuses a stale answer, as it did before.)
- [x] A sixth item, or a second correction of the same memory, is refused when added, and the agent is told why as an error, not a result (#751). The same words asked again are the item already waiting. ([card](../evidence/wo055-card.md): `a_card_holds_five_and_no_more`, `one_card_corrects_a_memory_once`, `the_same_words_again_are_the_memory_already_waiting`; [agent](../evidence/wo055-agent.md): `a_full_card_and_a_second_correction_are_told_to_the_agent`)
- [x] Denying the card refuses every item, for every task, and starts the task's cooldown. Skipping some items refuses those alone. ([ledger](../evidence/wo055-ledger.md): `a_denied_card_refuses_each_item_on_any_card`; [keep](../evidence/wo055-keep.md): `keeping_some_items_refuses_the_rest_and_pauses_nothing`)
- [x] Adding items doesn't extend the card's deadline, and each one counts against the hourly limit. ([ledger](../evidence/wo055-ledger.md): `a_waiting_card_grows_by_revision_and_never_quietly`, `each_revision_counts_against_the_hour`)
- [x] `keep` is refused on a request without items, and when empty, out of range or repeated. Nothing is kept on a refused decision. ([keep](../evidence/wo055-keep.md): `a_keep_that_cant_be_kept_is_refused_and_changes_nothing`, and through the API, `the_owners_keep_reaches_the_card_it_decides`)
- [x] A single memory proposal still shows as one card. Approving it keeps it as before. ([card](../evidence/wo055-card.md): a card of one has no items and is frozen as before; `a_proposal_is_kept_only_once_the_owner_approves` unchanged)
- [x] Each change has its evidence record under `docs/evidence/`, with hand-breaks of each defense above (below).

## Gates

All must pass before the WO closes:

- [x] `bash scripts/check.sh` green on the pinned toolchain **and** 1.85.0: CI's gate on each slice's pull request (#754, #756, #757 and slice 4's). Locally, each slice ran the workspace's tests and clippy on both toolchains.
- [x] Real-cell tests ran rather than skipped (`HATCHLING_REQUIRE_CELL=1`), on the WSL2 sandbox, for each slice.
- [x] **Mutation, kill floor 1.0** on each slice's change, with `cargo mutants --in-diff`: slice 1, 36 mutants (26 caught, 10 unviable); slice 2, 29 (16, 13); slice 3, 35 (26, 9). None missed in any. Slice 4 changes only strings, which are pinned by tests and hand-broken instead.
- [x] `cd frontend && npm ci && npm run check`: green on slices 2 and 3, with live coverage at 100%.
- [x] Wire types regenerated (`protocol-schema --write`, `npm run protocol:generate`) and committed, in slices 2 and 3.
- [x] Docs that describe the changed behavior updated in the same change: `local-api.md`, `arch/protocol.md`, `arch/distribution.md` and the CHANGELOG.

## Evidence

- Slice 1, the ledger: [wo055-ledger.md](../evidence/wo055-ledger.md).
- Slice 2, the card: [wo055-card.md](../evidence/wo055-card.md).
- Slice 3, Keep and Skip: [wo055-keep.md](../evidence/wo055-keep.md).
- Slice 4, the agent: [wo055-agent.md](../evidence/wo055-agent.md).

The rest is filled in at close.

## Notes

- **Why one card and not several:** several cards (up to three per task) was the simpler change, but it raises the flood limit and leaves the owner three decisions to make. One card keeps the limit where it is and makes the owner's choice one look.
- **Why revisions and not a fresh request:** a fresh request per item would need the old one withdrawn, and a decision racing that withdrawal could approve a request the owner never saw complete. Revisions already exist on the wire and in the app, which refuses a stale decision and shows the new card.
- **Why Skip refuses for the broker's life:** a skipped item is the owner's no. Treating it as softer than a denial would let a task re-ask it on the next turn, which is the fatigue this WO must not create.
- **The wire shape** (`ConfirmationRequest.items`, `Decide.keep`, UI schema v15) was approved by the maintainer on 2026-11-27.
- **Found by** WO-048's real-model trial, which also filed #747, #749 and #751.

### Slices

1. **The ledger** (`approvals.rs`): a task revises its waiting request (a new revision and digest, the same scopes, grant and deadline, counted against the hour); items' fingerprints, each refused once denied. No wire change.
2. **The card** (board, memory keep, UI v15, the app): a task's memory proposals gather on one request, up to five; approving keeps each item; the app lists the items. Approve keeps all of them.
3. **Keep and Skip** (the ledger's subset decision, `Decide.keep` with its rejection, the app): the owner keeps a subset, and the skipped items are refused from then on.
4. **The agent** (`memory_propose`): it's told the card it joined and when that card is full, and asks for each thing on its own.
