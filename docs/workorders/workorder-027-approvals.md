# WO-027: Approvals delivered to the owner

**Status:** DONE (2026-10-31)
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
    - an owner-wide cooldown (10 minutes) holds further prompts.
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

- Denials and pending requests that survive a restart, sealed like the schedules, and the owner's app reconsidering a denial. Moved here at close: while denials live in the broker's memory, a restart lifts every one, and a way to lift one alone matters once they persist.
- Agent tools for ideas and builds, now [WO-032](workorder-032-ideas-and-builds-from-the-chat.md). The agent's model loop has no tool that captures an idea or proposes a build, and a run's session has artifacts off, so only a session that sends the op itself reaches an approval today.
- Typed outcome events in the conversation (WO-026), so the next turn knows what happened.
- Reusable grants once a broker-owned task record exists ("this task" isn't a cell). The search WO's sites match on the exact origin (scheme, canonical ASCII host, port), and each redirect is checked before it's followed.
- The channel rail's state-changing sends, once a channel other than the local one exists. Unattended runs, whose proposals wait instead of failing.

**Not in scope:** approvals from anyone but the owner; approving through the agent's conversation (never; that is the point); the search WO's sites themselves.

## Acceptance criteria

- [x] The canonical form matches RFC 8785 for the supported types (its key-ordering example included), and refuses floats, out-of-range integers and lone surrogates; the digest changes exactly when a field of the action changes; tested.
- [x] A strictly parsed proposal refuses duplicate (also escaped-duplicate) keys and unknown fields; tested.
- [x] The ledger's states move only as above; two approvals and two workers give one `started`; a run interrupted while `started` is `outcome_unknown`, never run again, and nothing approved before a restart runs after it; tested.
- [x] Limits, expiry, identical-proposal reuse, the denial block and the cooldown hold; a reworded denied proposal and one from a fresh cell are still refused; tested.
- [x] A proposed activation reaches the app as a confirmation of the broker's fields; tested through the UI socket.
- [x] Approve activates exactly the pinned request, once. A second approve, a stale revision, a wrong digest, or an expired, denied or withdrawn request is rejected with its reason; tested.
- [x] A change to the idea after the proposal stops the execution; tested.
- [x] No session op approves, denies, mints, reconsiders or redeems; a cell's request naming a confirmation approves nothing; tested.
- [x] The app's review screen and decision work against a real broker (the browser suite).

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Built in #135 (the frozen action and the ledger), #136 (the board, the worker and the app's stream) and #137 (the review card).

An audit at close mapped each criterion to the tests behind it, and read the code beside them.
- It found real bugs, each fixed with a test that failed first:
  - a denial's fingerprint held the agent's description and the idea's id, so a reworded request or a fresh idea escaped it after the cooldown (#251);
  - the same build asked for again got a new request, refused as a full queue, and a refused proposal re-pinned the idea, so the owner's approval built nothing (#251);
  - the owner was never told what became of an approved build unless they'd sent the app a message, and then it read as the agent's words (#255).
- It found criteria proven only in part. #252, #253, #254 and #255 add those tests, and the strict reading the scope asked for.

Each change was gated in the sandbox on real cells, with a delegated cgroup:
- fmt and clippy on 1.98.1 and 1.85.0;
- preflight;
- the tests with `REQUIRE_CELL=1`;
- a hand mutant for each claim, each read for failing at its intended check;
- then CI.

- **The canonical form and the digest** (`crates/hatchling-core/src/action.rs`):
  - RFC 8785's key-ordering example, by order (`keys_sort_by_utf16_code_units_as_rfc_8785_sorts_them`), and written out whole, from bytes written by hand from the RFC (`the_rfc_8785_sorting_example_is_written_exactly`);
  - its string escaping and no normalization, and the refusal of floats, integers past ±(2⁵³−1), null and lone surrogates;
  - the digest changes exactly when a field does (`the_digest_changes_exactly_when_the_action_does`);
  - an activation's real record has the digest computed for it outside this code, from RFC 8785 and the record as written (`an_activations_record_has_the_digest_computed_from_its_form`, `crates/hatchling-broker/src/approval_board.rs`). Each input is in it by its hash and its length, from the bytes the broker holds (`each_input_is_bound_and_shown_by_its_length_and_its_hash`).
- **A strict reading** (#252, #253):
  - a key given twice in one object, however it's escaped, a number that isn't a safe integer, and a lone surrogate are refused (`crates/hatchling-core/src/strict_json.rs`). A session sent one ends unanswered (`a_request_that_could_be_read_two_ways_ends_the_session`, `crates/hatchling-broker/tests/handshake.rs`), and the vault keeper reads a cell's channel the same way;
  - a field a request doesn't name is refused (`a_session_request_with_a_field_it_doesnt_name_is_refused`, `crates/hatchling-core/src/messages.rs`), in every object a request holds (`every_object_a_session_request_holds_refuses_an_unknown_field`, `crates/hatchling-dev/src/checks/protocol_schema.rs`).
- **The ledger's states** (`crates/hatchling-broker/src/approvals.rs`):
  - `a_request_moves_from_pending_to_approved_to_started_to_its_end`;
  - two approvals and two workers racing give one run (`two_approvals_and_two_workers_give_one_run`);
  - a worker that dies holding the claim leaves the request `outcome_unknown`, never run again (`a_claim_dropped_unrecorded_is_outcome_unknown_and_never_runs_again`);
  - withdrawn and expired requests neither decide nor run.

  **Changed at close:** this criterion said a restart's `started` is `outcome_unknown`. The ledger is in the broker's memory, as the Notes say, so a restart forgets it: nothing approved before a restart runs after it, and a decision on a request from before one is `unknown`. A worker dying mid-run is the interruption that can leave a request `started`, and that is what's tested.
- **Limits, expiry, reuse, the denial and the cooldown:**
  - `asking_is_bounded_and_an_identical_proposal_gets_the_waiting_request`, `an_undecided_request_lapses_and_can_no_longer_be_approved`, `a_denial_sticks_and_pauses_every_new_request` and `the_ledger_lapses_what_is_due_and_forgets_the_oldest_decided` (`approvals.rs`);
  - a reworded denied build, and one from a fresh idea in another task, are refused (`a_denied_build_is_refused_reworded_or_from_a_fresh_idea`, `approval_board.rs`);
  - the same build asked for again is the request the owner is deciding, and a refused proposal leaves it standing (`asking_again_is_the_waiting_request_and_a_refused_proposal_leaves_it`, `crates/hatchling-broker/tests/artifacts.rs`).
- **A confirmation of the broker's fields, through the UI socket:** `a_proposed_activation_is_asked_and_decided_through_the_ui_socket` (`crates/hatchling-broker/src/api.rs`, #254). The app's client says hello with the UI token, subscribes, and reads the confirmation: the broker's fields first, the agent's words last, as the agent's. A hand mutant that doesn't attach the board to the API's stream fails it with "the app was never asked".
- **Approve runs exactly the pinned request, once; every other decision is rejected with its reason:**
  - `the_owners_approval_in_the_app_builds_exactly_what_was_proposed` (`artifacts.rs`, real builder cells): built from exactly the proposal's inputs, once; a second approve is `already_decided`, and nothing more runs;
  - a stale revision and a wrong digest are `stale_revision`, and an unknown request is `unknown` (`a_decision_names_the_request_revision_and_digest_it_was_shown`); an expired one is `expired`, and a withdrawn one `already_decided`;
  - a denied one, approved after in each way, is `already_decided`, with nothing on the stream and nothing queued (`a_denied_request_cant_be_decided_again`, #255).
- **A change to the idea stops it:** `an_approval_for_an_idea_that_changed_builds_nothing` (`artifacts.rs`).
- **No session op approves, denies, mints, reconsiders or redeems:**
  - by construction: a decision is `UiAction::Decide`, on the owner's UI socket alone. The session protocol's 14 families hold no op that decides or mints, and refuse any op or field they don't name (#253);
  - a session's `activate` naming the request's id is refused `not_confirmed` (`the_owners_approval_in_the_app_builds_exactly_what_was_proposed`), and so is a forged confirmation (`the_workers_steps_run_only_what_was_approved_and_tell_the_owner`);
  - in the browser suite, the agent's session tries to redeem the request it was shown, and is refused.
- **The review screen against a real broker** (`scripts/approval_end_to_end.py`, in CI's browser job, #255). It runs a broker with a build store, an agent session proposing a build, and the app, driven as the owner would. The build's description claims to be the broker's and carries a right-to-left override. The card shows:
  - the broker's six fields first, in order, and the agent's words last, tagged "From the agent";
  - the override as `⟨U+202E⟩`, and never in the page;
  - the input by its length and hash;
  - "Allow once" and "Deny" alone.

  Allowed once, the card goes, and the broker's outcome is shown in the chat as Hatchling's notice, never the agent's words. Its first runs found the third bug above.

## Notes

- **An outside review shaped this design.** It kept the broker executing the stored action, and separated the action's identity from the request's. It added the state machine with `outcome_unknown`, the strict parse and the integer profile, and the denial fingerprint with a cooldown. It also set the preview rules: persuasion can't be prevented, but impersonating the broker's fields, concealing the effect, and swapping the screen under a click can.
- **Why the owner's app, and not the chat.** The chat is where a cell's words appear. An approval asked for there can be argued for, or forged in an answer that looks like a prompt. The app's confirmation is a broker event the cell can't produce, and the decision travels on the owner's own socket, which the cell can't reach.
- **Why the broker runs the stored action.** If the cell ran it after the approval, the approval would bind only what the cell said it would do. Here the cell's part ends at the proposal; the outcome comes back as the broker's record, while anything a service answers stays untrusted content.
- **Restarts, for now.** A restart withdraws every pending request, which fails safe, and forgets denials. A cell can't restart the broker, and the ledger spans cells, so a fresh cell doesn't reset a denial. Denials surviving a restart are the next slice.
- **Part 2 (wiring):** each broker's board is found by its vault's root, so tests with their own vaults don't share limits. A proposal's task is the proposing session, until a broker-owned task record exists. An approval for an idea that changed after the proposal builds nothing: the worker finds the idea no longer pins the request, and tells the owner. Taking such requests back when the idea changes is a later slice. The app's review lists the fields as text; the review screen with the fixed layout is part 3.
- **Part 3 (the review screen):** the card lists the fields above the scope, in review order (the app keeps the broker's first, whatever order they arrive in), each agent field tagged. Values are `<bdi dir="auto">` with every format character, Hangul filler and variation selector shown as its code point; the override itself is never in the page. Buttons and their labels are the app's own. A decision names the revision and digest on screen, so a request that changes under a click is rejected, not approved. The browser suite against a real broker came at close (#255): a broker with a build store and no cell takes the test's session as the agent's, since the agent has no tool that proposes a build.
- **The existing activation rail stays** as the inner check: `confirm_activation` and `take_confirmation`. The worker mints the confirmation and presents it, so a cell still never holds one.
