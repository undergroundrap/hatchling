# WO-025: Scheduled runs and their results survive a crash or a failed write

**Status:** DONE
**Area:** `crates/hatchling-scheduler` (dispatcher, `Store`), `crates/hatchling-broker` (`schedule_ops.rs`, `run_launcher.rs`, startup), `docs/`
**Issue:** [#118](https://github.com/undergroundrap/hatchling/issues/118)
**Trust boundary:** yes (a scheduled run acts with its creator's grants; running one twice repeats its effects)

## Problem

The scheduler keeps a running occurrence only in memory (`Dispatcher::active`). Its store holds each schedule and its next due time, and nothing else. So three failures lose or repeat work (`dispatcher.rs`, verified at `60e7e653`):

1. **A one-shot is forgotten as it starts.** `fire` launches the run, then removes the schedule at once, since it has no next occurrence. If the broker dies before the run ends, nothing records that the run was ever started.
2. **A failed save repeats an occurrence.** After the launch, `fire` saves the schedule's next due time and ignores an error (`let _ = services.store.update(..)`). Once the run finishes, the next tick sees the old due time, still due, and launches the same occurrence again.
3. **A result can be lost while reported as parked.** When delivery fails twice, the result is parked. If parking also fails, the failure is logged, the id is still reported as parked, and the output, held only in a local value, is dropped.

The store also loses results and records quietly in two more places (`schedule_ops.rs`):

- **`park` drops the oldest result when the store is full** (`MAX_PARKED`, 64). A warning is logged, but the result is gone.
- **`load` skips an index entry whose file is missing**, as if the schedule never existed. A corrupt entry is already an error; a missing one should be too.

A scheduled run acts with its creator's grants: it can send messages, call services, write files. Running one twice repeats those effects, and losing one loses work the user was promised.

## Scope

**In scope:**

- **One record per schedule holds both the occurrence and the cadence.** A schedule's sealed entry becomes an envelope: the schedule, its next due time, a revision, and its unresolved occurrences. Each occurrence has an id (schedule, definition revision, due time), an attempt id, a state, the immutable part of the schedule it runs, and its result. Advancing the cadence and claiming the occurrence happen in the same write, so one can't happen without the other.
- **A claim is committed before the launch.** A store operation `claim_due(id, expected_revision, expected_due)` reads the latest envelope under the store's lock, checks it's still due and unclaimed, adds the occurrence (state `claimed`), advances `next_due` (or marks a one-shot exhausted, not removed), and commits. The dispatcher launches only on a committed claim. A failed commit launches nothing.
- **Every later change reloads the latest envelope:** running, finished, delivered, failed. A completion never writes back the snapshot it launched from, which could undo an edit or a cancellation made in between.
- **A result is stored before it is delivered.** The finished output goes into the occurrence first. Delivery then works from the store (an outbox), and a failed delivery leaves the result where it was. A result is never only in memory.
- **A full store refuses new claims instead of dropping results.** If unresolved occurrences and parked results reach their bound, the schedule's next claim is refused, loudly, rather than deleting anything unresolved. `park`'s drop-oldest goes.
- **A missing entry is an error.** An index entry without its file is reported as a store error, not skipped.
- **Startup reconciles what was in flight.** First every surviving run is accounted for. Then each occurrence left `claimed` or `running` becomes **outcome unknown**. It is shown to the user with its schedule, due time and attempt, plus whatever the broker saw it do, and the options *view activity*, *mark resolved*, and *retry as a new attempt*. Retry goes through the normal grants. An unknown occurrence is never marked as succeeded, never described as "failed before doing anything", and never put back in the queue. That schedule's later runs are held until the user resolves it or resumes them.
- **A commit that may or may not have happened stops dispatching.** If the rename succeeded but the directory sync failed, the new state may be visible but not durable. The dispatcher stops, rereads under the lock, and resumes only once a sync succeeds. It never rolls back to a cached older envelope.

**Not in scope:**

- exactly-once external effects (a recipient that accepted a delivery just before a crash; the occurrence is *unknown*, and an idempotency key, where the recipient supports one, is that integration's work);
- an append-only journal (one envelope per schedule is enough for this invariant);
- hooks' durability (the same pattern can follow, as its own WO);
- the UI for unknown occurrences beyond what the operator API and the existing frontend views can show (the live approvals and notifications work delivers the rest).

## Acceptance criteria

- [x] **The three windows, each with a test written first, failing on `60e7e653`:**
  - a one-shot whose run is unfinished when the dispatcher and launcher are rebuilt over the same store is still accounted for: a claim, a result, or an unknown, not nothing;
  - a first `update` (or `claim_due`) that fails, then a completed run and a tick before the next due time, launches that occurrence once;
  - two failed deliveries and a failed park, then a tick with delivery and the store healthy, leave the output delivered or stored.
- [x] A broker killed between the claim's commit and the launch, and after the launch, comes back with the occurrence as *outcome unknown*, not lost and not rerun; tested with a real broker process.
- [x] A store write failing at each step (temp write, temp sync, rename, directory sync) is tested. Before the rename, nothing launches. After it, the state is reread before any further dispatch.
- [x] A full store refuses the next claim and keeps every unresolved result; tested.
- [x] An index entry with no file is an error on `load`; tested.
- [x] An unknown occurrence holds its schedule until it is resolved or resumed. *Mark resolved* and *retry* work through the operator API; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Merged in parts:
- part 1 (claims, stored results, no drops): #146;
- part 2 (unknown outcomes, the owner's operations): #147;
- part 3 (failed writes at each step, a claim that may have landed, a broker killed before the launch): #149.

Each was gated in the disposable sandbox (WSL2, uid 2001, gid 2501): fmt, clippy on 1.98.1 and 1.85.0, the preflight, `check.sh` with `HATCHLING_REQUIRE_CELL=1` on both toolchains, hand mutants, and mutation on the changed lines (0 missed, 0 timeouts in every part). The criteria:

- **The three windows** (`crates/hatchling-scheduler/tests/dispatcher.rs`, written first and failing on `60e7e653`, #146):
  - `a_one_shot_still_running_when_the_broker_dies_is_accounted_for`;
  - `a_failed_cadence_write_never_launches_an_occurrence_twice`;
  - `an_output_that_cannot_be_delivered_or_parked_is_kept_for_later`.
- **A real broker, killed** (`crates/hatchling-broker/tests/unknown_outcome.rs`). The killed broker is the test binary again, as a child, over the sealed store and the real dispatcher, killed with `SIGKILL`:
  - after the launch, its run out in a real cell (`a_fire_in_flight_when_the_broker_is_killed_is_held_as_unknown`, #147): the next broker holds the fire as `Unknown { started: true }`, holds the schedule's next fire, shows it to the owner, refuses a cell's retry, and runs it again only on the owner's;
  - between the claim's commit and the launch (`a_fire_claimed_when_the_broker_is_killed_is_held_as_not_started`): the fire is `Claimed` in the store, and comes back `Unknown { started: false }`, held, not run again.
- **A store write failing at each step** (`a_claim_failing_at_each_write_step_launches_once`): the vault's test-only `fault-injection` feature fails the claim's write at the temp write, the temp sync, the rename, and the directory sync, in turn. Nothing launches on the tick that failed. Before the rename, no claim landed; at the directory sync, it did, visible but maybe not durable. Either way the next tick launches the fire once, as the fire it claimed, and no later tick launches it again. The dispatcher remembers a failed claim as uncertain, fires no schedule while one is, and writes it again before anything else (`Dispatcher::settle`): a claim that landed is launched once that write succeeds; one that didn't is claimed afresh. The scheduler's component tests cover each branch with a store whose claim is written and then fails (`a_claim_that_landed_but_failed_launches_once_the_next_tick`, `a_landed_claim_launches_nothing_until_it_is_written_again`, `a_failed_claim_that_did_not_land_is_claimed_afresh`, `a_landed_claim_whose_schedule_is_cancelled_launches_nothing`).
- **A full store refuses and keeps** (#146): `the_sealed_store_claims_records_and_resolves_fires` fills the parked results to `MAX_PARKED`, and the next park and resolve are refused with nothing dropped; `a_result_that_waits_holds_its_schedules_next_fire` holds the next fire while a result waits.
- **A missing entry is an error** (#146): `the_sealed_store_claims_records_and_resolves_fires` and `the_sealed_stores_parts_called_directly`.
- **An unknown occurrence holds its schedule; mark resolved and retry work** (#147): `a_fire_left_in_flight_holds_its_schedule_as_unknown`, `a_fire_left_in_flight_is_held_and_a_cell_cant_release_it`, `the_owner_resolves_retries_and_dismisses`, `the_owner_lists_and_dismisses_parked_results`, through the session protocol and the local API (`resolve`, `retry`, `list-parked`, `dismiss`).

## Notes

- **The tests for the three windows exist.** An outside review wrote them against the `MemStore` fixture in `crates/hatchling-scheduler/tests/dispatcher.rs`, with two switches added: `fail_next_update` and `fail_next_park`, each a `Cell<bool>` checked at the top of its method. They model losing the dispatcher's memory while the store survives. A real killed broker is the next criterion's job. When the ledger lands, the first test's accounting includes its claims.
- **Why one envelope and not a journal:** the invariant is "a claimed occurrence ⇔ an advanced cadence, in the same revision". One atomic replacement gives that. A journal adds framing, torn-tail recovery, replay and checkpoints, which only pay when a transaction spans independent records.
- **The write order** is the vault's existing one (`store_inner`): a temp file in the same directory, write it, fsync it, rename it over the old entry, fsync the directory. A leftover temp is ignored on recovery, never promoted because it parses.
- **Index membership stays out of the claim.** Creating a schedule writes its envelope first, then adds it to the index. The index stays authoritative for which schedules exist, and an envelope missing from it is reported, not resumed.
- **Serialization:** the store's lock (`STORE`) serializes writers within one broker. The serving lock (WO-012, `<socket>.lock`) keeps a second broker from serving the same socket, so an old dispatcher can't launch once a new one has started.
- **"Claimed", not "started":** a committed claim doesn't prove the run began. The state moves to `running` after the launch succeeds.
