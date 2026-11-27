# WO-055: Several memories, one card

**Status:** IN-PROGRESS
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
  - says when a proposal joined a card already waiting ("on the card with 2 others"), and when the card is full;
  - says to ask for each thing on its own, since they gather on one card.
- The app's approval card shows the items, each with Keep (on by default) and Skip, and the button says how many are kept ("Keep 2"). A card whose items change while open shows the new revision before anything can be decided.
- `docs/local-api.md`, `docs/session-protocol.md` (if the agent's result text is documented), `docs/arch/protocol.md`, the CHANGELOG.

**Not in scope:**
- Batching other kinds of request (sites, schedules, sign-ins). They stay one per task.
- The review queue for background loops (WO-054), which has its own list.
- Editing an item's words on the card. The owner skips it and says what they meant.
- Raising `MAX_PENDING_PER_TASK` or `MAX_PENDING`.

## Acceptance criteria

- [ ] A chat turn that asks to remember three things puts one card to the owner, with three items. Keeping two keeps exactly those two, each as its own memory, and the third is never asked again.
- [ ] An item added while the card is shown makes a new revision. A decision on the old revision is refused, and the app shows the new card.
- [ ] A sixth item, or a second correction of the same memory, is refused when added, and the agent is told why as an error, not a result (#751). The same words asked again are the item already waiting.
- [ ] Denying the card refuses every item, for every task, and starts the task's cooldown. Skipping some items refuses those alone.
- [ ] Adding items doesn't extend the card's deadline, and each one counts against the hourly limit.
- [ ] `keep` is refused on a request without items, and when empty, out of range or repeated. Nothing is kept on a refused decision.
- [ ] A single memory proposal still shows as one card. Approving it keeps it as before.
- [ ] Each change has its evidence record under `docs/evidence/`, with hand-breaks of each defense above.

## Gates

All must pass before the WO closes:

- [ ] `bash scripts/check.sh` green on the pinned toolchain **and** 1.85.0.
- [ ] Real-cell tests ran rather than skipped (`HATCHLING_REQUIRE_CELL=1`).
- [ ] **Mutation, kill floor 1.0** on each changed `.rs` file.
- [ ] `cd frontend && npm ci && npm run check`.
- [ ] Wire types regenerated (`protocol-schema --write`, `npm run protocol:generate`) and committed.
- [ ] Docs that describe the changed behavior updated in the same change.

## Evidence

- Slice 1, the ledger: [wo055-ledger.md](../evidence/wo055-ledger.md).
- Slice 2, the card: [wo055-card.md](../evidence/wo055-card.md).

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
