# WO-054 slice 3a: the seat's checks

Part of [#718](https://github.com/undergroundrap/hatchling/issues/718), [WO-054](../workorders/workorder-054-background-runs.md).

## What it is

`hatchling_core::seat`, the part of the seat runner that needs no cell and no broker: what a seat is given, and how what it hands back is checked. Slice 3b gives a seat its cell (read tools over the snapshot and `finish`, nothing else). Slice 3c has the broker run the steps and apply.

- **The snapshot.** The records a step may read, taken at the run's start, one per handle. Each has a fingerprint, for apply's recheck, and an origin: the owner's words, the agent's, or outside content.
- **Handles:** `message:<conversation>/<id>`, `memory:<id>`, `goal:<id>`, `run:<id>` and `file:<path>`. Each reads back as itself and is bounded. An id with whitespace or a control character isn't one, so a handle can't smuggle a second line.
- **`finish`'s output** is `Finish<C>`, claims of the step's own type, or `no_change`, checked by `check`:
  - **The schema:** an unknown field, a missing one or a wrong type refuses it.
  - **Bounds:** claims, citations per claim, the quote's length, and the reasons said.
  - **`no_change` is said, not left as an empty list.** No claims without `no_change` is refused, and so is both.
  - **Every citation resolves:** its handle names a record the step was given, and its quote, if any, is in that record's text. **One that doesn't refuses the whole output**, so an invented id applies nothing. That's the threat model's rule, which the maintainer chose over the seat-runner section's earlier wording (the claim alone fails) on 2026-11-26; the design now says the same.
  - **Outside content applies nothing:** a claim whose every citation is outside content goes in `outside_only`, kept apart for the loop's apply step to queue or drop.
- **One repair:** `Step` answers the first refusal with every reason, and a second ends the step with nothing.
- **The gate's record:** how a step ended (proposed, no change, failed), whether it took its repair, and its counts. Never text.

## Tests

`seat_tests.rs`, nine tests:
- handles read back, and what isn't one is refused by why, at the length bound to the byte;
- a snapshot holds one record a handle;
- claims are split by where they came from;
- `no_change` is a result said as such;
- an output that isn't the schema is refused, an injected field included;
- a handle that doesn't resolve, a misquote or a blank quote refuses the whole output, with each reason;
- the bounds hold at and past each limit;
- a step takes one repair, then ends with nothing, and the gate's record of each ending.

## The gate

On the sandbox, at the tree under test:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- The whole workspace's tests: 4578 passed, 0 failed. `hatchling-dev audit`: all checks passed.
- **Hand-breaks**, each caught by the seat's tests:
  - a quote taken on trust;
  - outside content grounding a claim;
  - a handle that doesn't resolve let through;
  - repairs without end.
- **Mutation** on `seat.rs`: 49 mutants, 36 caught, 13 unviable, none missed.
