# The keeper's fill outside its lock, and a refused scan on the record (#689)

Closes [#689](https://github.com/undergroundrap/hatchling/issues/689), two gaps from an outside review, confirmed in the code. The keeper's separation was found strong.

## 1. A fill wrote to the browser cell under the keeper's lock

**The threat:**
- `answer_fill` ran under the keeper's one `Mutex<Kept>` and wrote the fill's three frames to the browser cell's channel, each bounded by `FILL_TIMEOUT` (5 s).
- A browser cell that stops reading its channel, the cell that renders untrusted pages, then stalled every keeper caller for up to 15 s a fill. That's the broker's reads, the egress authority's, the spawner's, and every cell's vault ops.

**The fix:**
- **Decided under the lock.** `decide_fill` checks the profile and the site, reads the sign-in and records the release before anything is sent, as before. It takes the browser's channel out by `Arc`.
- **Written outside it.** The frames go out under the channel's own lock, so two fills to one browser never interleave their frames, and a stalled browser holds up only its own fills.
- **A failure drops only its own channel.** A write that fails drops the channel it was written to (`Arc::ptr_eq`), not one the cell has held since, then says the channel broke.
- **Before the lock in the dispatch.** The request is answered before the keeper's lock is taken, as a `Contains` check already is.

## 2. A refused scan wasn't in the vault's access log

Every other refusal records `Outcome::Denied` in `vault_access`. A scan for a purpose that isn't a gate's only logged a warning, so a compromised broker probing scans under other purposes never showed in `vault-audit log`. It's now recorded as a denied read by the broker for that purpose, as an allowed scan is recorded as its read.

## Tests

In `keeper::tests`:
- `a_sign_in_is_filled_on_its_profiles_browser_channel_alone`, extended:
  - a browser channel whose reader never reads, its buffer full: once the fill is decided and writing, another caller takes the keeper's lock at once (under a second, not the fill's 5 s);
  - while that fill is stalled, the cell's channel is replaced: the failed fill drops its own and keeps the one held since;
  - the fill is answered that the channel broke;
  - the existing cases (a closed channel dropped, no sign-ins kept) run through the new path.
- The scan test: a scan for `egress` is refused and recorded as a denied read by the broker for `egress`.

## The gate

On the sandbox, at the tree under test:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- The whole workspace's tests: 4572 passed, 0 failed. `hatchling-dev audit`: all checks passed.
- **Hand-breaks**, each caught by `keeper::tests`:
  - the fill written under the keeper's lock again;
  - a refused scan recorded as allowed;
  - a failed fill dropping whatever channel the cell holds now.
- **Mutation** on the changed lines of `keeper.rs`: 6 mutants, 5 caught, 1 unviable, none missed.
