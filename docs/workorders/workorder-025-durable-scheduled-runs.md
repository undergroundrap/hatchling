# WO-025: Scheduled runs and their results survive a crash or a failed write

**Status:** IN PROGRESS (part 1: claims, stored results, no drops)
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

- [ ] **The three windows, each with a test written first, failing on `60e7e653`:**
  - a one-shot whose run is unfinished when the dispatcher and launcher are rebuilt over the same store is still accounted for: a claim, a result, or an unknown, not nothing;
  - a first `update` (or `claim_due`) that fails, then a completed run and a tick before the next due time, launches that occurrence once;
  - two failed deliveries and a failed park, then a tick with delivery and the store healthy, leave the output delivered or stored.
- [ ] A broker killed between the claim's commit and the launch, and after the launch, comes back with the occurrence as *outcome unknown*, not lost and not rerun; tested with a real broker process.
- [ ] A store write failing at each step (temp write, temp sync, rename, directory sync) is tested. Before the rename, nothing launches. After it, the state is reread before any further dispatch.
- [ ] A full store refuses the next claim and keeps every unresolved result; tested.
- [ ] An index entry with no file is an error on `load`; tested.
- [ ] An unknown occurrence holds its schedule until it is resolved or resumed. *Mark resolved* and *retry* work through the operator API; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **The tests for the three windows exist.** An outside review wrote them against the `MemStore` fixture in `crates/hatchling-scheduler/tests/dispatcher.rs`, with two switches added: `fail_next_update` and `fail_next_park`, each a `Cell<bool>` checked at the top of its method. They model losing the dispatcher's memory while the store survives. A real killed broker is the next criterion's job. When the ledger lands, the first test's accounting includes its claims.
- **Why one envelope and not a journal:** the invariant is "a claimed occurrence ⇔ an advanced cadence, in the same revision". One atomic replacement gives that. A journal adds framing, torn-tail recovery, replay and checkpoints, which only pay when a transaction spans independent records.
- **The write order** is the vault's existing one (`store_inner`): a temp file in the same directory, write it, fsync it, rename it over the old entry, fsync the directory. A leftover temp is ignored on recovery, never promoted because it parses.
- **Index membership stays out of the claim.** Creating a schedule writes its envelope first, then adds it to the index. The index stays authoritative for which schedules exist, and an envelope missing from it is reported, not resumed.
- **Serialization:** the store's lock (`STORE`) serializes writers within one broker. The serving lock (WO-012, `<socket>.lock`) keeps a second broker from serving the same socket, so an old dispatcher can't launch once a new one has started.
- **"Claimed", not "started":** a committed claim doesn't prove the run began. The state moves to `running` after the launch succeeds.
