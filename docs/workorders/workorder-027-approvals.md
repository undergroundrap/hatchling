# WO-027: Approvals delivered to the owner

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-core` (`action`: canonical form and digest; `ui`), `crates/hatchling-broker` (`approvals`, `api_ops`, `artifact_ops`), `frontend/` (the live approvals adapter and review screen), `docs/`
**Issue:** none yet
**Trust boundary:** yes

## Problem

Some actions an agent proposes need the owner's yes: activating an artifact today, and soon a new site to reach (the search WO) or a message that isn't a reply. The broker can already mint the yes: `confirm_activation` and the channel rail's approvals exist, and no session op reaches either. But nothing asks the owner. Every such action is refused outright, the app's approve and deny answer `Unavailable`, and the protocol's confirmation events are never sent.

Other agents ask in the conversation, where whatever the agent read can argue for a yes, or they guard only the last step (sending, buying) and let everything before it through. An approval in Hatchling is the owner's decision about one exact, frozen action. It is made outside the conversation, and only the broker turns it into an effect.

## Scope

**In scope (first slice: once-only approval of an artifact activation):**

- **The frozen action** (`hatchling_core::action`):
  - The broker resolves a proposal before anyone is asked. The record holds:
    - the operation;
    - the trusted executor and its contract version;
    - the target and its preconditions (for an activation, the idea's revision);
    - every effectful argument, defaults included;
    - each input by the digest and length of bytes the broker already holds.

    Nothing in it is re-read at execution.
  - Its canonical form is RFC 8785 (JCS) for the types actions use: objects, arrays, strings, booleans and integers in ±(2⁵³−1). Keys are sorted by UTF-16 code units, strings are escaped as JCS says, and there is no Unicode normalization.
  - Its digest is `SHA-256("hatchling.action.v1\0" ‖ JCS(action))`.
  - What a cell sends is parsed strictly: unknown fields, duplicate keys (after unescaping), lone surrogates and inexact numbers are refused.
- **Three identities, kept apart:**
  - the **action digest** (the frozen effect);
  - the **request** (its id and revision: this one chance to approve it, with its owner, task, expiry and state);
  - the **denial fingerprint**: the effect without request ids, timestamps or any rationale, so a reworded request or a fresh cell can't escape a denial.

  The owner's decision binds the request id, its revision and the action digest.
- **The ledger** (`hatchling-broker`, in the broker's memory):
  - Each request moves through these states:

    ```text
    pending → approved → started → succeeded | failed | outcome_unknown
    ```

    It can instead end as denied, expired or withdrawn, before it starts.
  - The claim `approved → started` is atomic, and is made before any effect, so two clicks or two workers give one execution.
  - A request found `started` after an interruption is `outcome_unknown`, never replayed.
  - Limits:
    - 5 pending per owner and 1 per task;
    - 15 minutes to decide;
    - an identical pending proposal returns the existing request.

    At a limit, a proposal gets a typed refusal (`approval_paused`, `approval_queue_full`), not a backlog.
  - After a denial:
    - the fingerprint is blocked for the broker's life;
    - an owner-wide cooldown (10 minutes) holds further prompts;
    - the owner's app can reconsider.
- **Delivery and decision:**
  - The request goes out on the UI stream (`UiEventBody::Confirmation`) as the broker's fixed fields: the operation, the target, the executor, the request's digest and its expiry, then the payload the action will write, labeled as the agent's.
  - No agent rationale is shown in the first slice. Payload text is capped where the proposal is accepted (4 KiB, 2,000 scalar values), never truncated on screen.
  - The app approves or denies with `UiAction::Decide`, checked against the id, revision, digest, expiry and state (`DecisionRejection`). Only the owner's socket reaches it; no session op approves, denies, mints or reconsiders.
- **Execution runs the stored action.**
  - On approve, a broker worker claims the request, re-checks its preconditions (the idea is still at the approved revision), and runs the stored action. For an activation, it mints the confirmation and activates exactly the pinned request.
  - The outcome is recorded in the ledger, and told to the owner on the local channel, as the broker's.
  - A request reference given to a cell is not a capability: no session op redeems one.
- **The app's review screen** shows the fields in that fixed order:
  - inert text;
  - each field isolated for direction;
  - invisible and bidi control characters made visible;
  - no agent text in buttons, labels or notifications;
  - the screen never swapped under a pending click.

**Then (later slices):**

- Denials and pending requests that survive a restart, sealed like the schedules.
- Typed outcome events in the conversation (WO-026), so the next turn knows what happened.
- Reusable grants once a broker-owned task record exists ("this task" isn't a cell). The search WO's sites match on the exact origin (scheme, canonical ASCII host, port), and each redirect is checked before it's followed.
- The channel rail's state-changing sends, once a channel other than the local one exists. Unattended runs, whose proposals wait instead of failing.

**Not in scope:** approvals from anyone but the owner; approving through the agent's conversation (never; that is the point); the search WO's sites themselves.

## Acceptance criteria

- [ ] The canonical form matches RFC 8785 for the supported types (its key-ordering example included), and refuses floats, out-of-range integers and lone surrogates; the digest changes exactly when a field of the action changes; tested.
- [ ] A strictly parsed proposal refuses duplicate (also escaped-duplicate) keys and unknown fields; tested.
- [ ] The ledger's states move only as above; two approvals and two workers give one `started`; a restart's `started` is `outcome_unknown`; tested.
- [ ] Limits, expiry, identical-proposal reuse, the denial block and the cooldown hold; a reworded denied proposal and one from a fresh cell are still refused; tested.
- [ ] A proposed activation reaches the app as a confirmation of the broker's fields; tested through the UI socket.
- [ ] Approve activates exactly the pinned request, once. A second approve, a stale revision, a wrong digest, or an expired, denied or withdrawn request is rejected with its reason; tested.
- [ ] A change to the idea after the proposal stops the execution; tested.
- [ ] No session op approves, denies, mints, reconsiders or redeems; a cell's request naming a confirmation approves nothing; tested.
- [ ] The app's review screen and decision work against a real broker (the browser suite).

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **An outside review shaped this design.** It kept the broker executing the stored action, and separated the action's identity from the request's. It added the state machine with `outcome_unknown`, the strict parse and the integer profile, and the denial fingerprint with a cooldown. It also set the preview rules: persuasion can't be prevented, but impersonating the broker's fields, concealing the effect, and swapping the screen under a click can.
- **Why the owner's app, and not the chat.** The chat is where a cell's words appear. An approval asked for there can be argued for, or forged in an answer that looks like a prompt. The app's confirmation is a broker event the cell can't produce, and the decision travels on the owner's own socket, which the cell can't reach.
- **Why the broker runs the stored action.** If the cell ran it after the approval, the approval would bind only what the cell said it would do. Here the cell's part ends at the proposal; the outcome comes back as the broker's record, while anything a service answers stays untrusted content.
- **Restarts, for now.** A restart withdraws every pending request, which fails safe, and forgets denials. A cell can't restart the broker, and the ledger spans cells, so a fresh cell doesn't reset a denial. Denials surviving a restart are the next slice.
- **Part 2 (wiring):** each broker's board is found by its vault's root, so tests with their own vaults don't share limits. A proposal's task is the proposing session, until a broker-owned task record exists. An approval for an idea that changed after the proposal builds nothing: the worker finds the idea no longer pins the request, and tells the owner. Taking such requests back when the idea changes is a later slice. The app's review lists the fields as text; the review screen with the fixed layout is part 3.
- **Part 3 (the review screen):** the card lists the fields above the scope, in review order (the app keeps the broker's first, whatever order they arrive in), each agent field tagged. Values are `<bdi dir="auto">` with every format character, Hangul filler and variation selector shown as its code point; the override itself is never in the page. Buttons and their labels are the app's own. A decision names the revision and digest on screen, so a request that changes under a click is rejected, not approved. Still open: the browser suite against a real broker, which needs the quickstart to configure artifacts.
- **The existing activation rail stays** as the inner check: `confirm_activation` and `take_confirmation`. The worker mints the confirmation and presents it, so a cell still never holds one.
