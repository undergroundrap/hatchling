# WO-054 slice 3b: the seat's cell

Part of [#718](https://github.com/undergroundrap/hatchling/issues/718), [WO-054](../workorders/workorder-054-background-runs.md). Builds on slice 3a ([wo054-seat-checks.md](wo054-seat-checks.md)).

## What it is

- **What a seat is handed** (`hatchling_core::seat::Packet`): its prompt, its assignment and the records it may read, and nothing else.
  - An unknown field is refused, so nothing more can ride along.
  - Its records make the step's snapshot, within bounds on the record count, a record's text, and the prompt and assignment.
- **A seat in its cell** (`hatchling_agent::seat`): one model step with its own tool registry, holding only these three tools:
  - `seat_records` lists the records, each by handle, whose words it is, and its size, but not what it says;
  - `seat_read` reads one, saying it's data and not instructions;
  - `finish` hands back the output.

  The egress and subagent tools every other run is given aren't in it, and a tool a seat names that it wasn't given runs nothing.
- **`finish` takes any JSON object at the loop's door**, so all checking is the step's own (`check`, from slice 3a) and every refusal counts:
  - the first is answered with every reason, its one repair;
  - a second ends the step with nothing;
  - an output after the step has ended is refused.
- **What a step hands back** (`Ending`): the output that passed and whether it took its repair, or that it failed, or that it never finished.

The broker checks the output again before it applies anything (slice 3c). The cell's check only spares a seat a wasted run.

**What isn't here:** launching a seat's cell from the broker, and the loops' own claim types. They come with slice 3c and the first loop, so no plumbing waits unused.

## Tests

- **`hatchling-core`**, `a_packets_records_make_its_snapshot`:
  - a packet round-trips, and an unknown field is refused;
  - a handle that isn't one, or two records under one handle, is refused;
  - each bound holds at its limit and past it.
- **`hatchling-agent`**, `seat_tests.rs`:
  - `a_seat_is_offered_its_reads_and_finish_and_nothing_else`: the design's test, whose whole reach is `finish`, `seat_read` and `seat_records`;
  - `a_seat_reads_its_records_then_finishes`:
    - the list doesn't show what a record says;
    - a read gives the text and whose it is;
    - a record it wasn't given is refused;
    - the model sees the packet's prompt and assignment and nothing else;
  - `a_refused_output_is_answered_once_with_every_reason`;
  - `a_second_refusal_ends_the_step_with_nothing`: and a good output after that is refused;
  - `a_step_finishes_once`;
  - `a_seat_that_never_finishes_hands_back_nothing`: an answer with no finish, and running out of steps;
  - `a_tool_a_seat_wasnt_given_runs_nothing`: `memory_propose`, `egress_request`, `spawn_subagent` and `write` are each told there's no such tool.
  - `each_tool_says_what_its_for`: the words the model reads.
- **`hatchling-core`**, `the_bounds_are_what_they_say`: every bound as a number.

## The gate

On the sandbox, at the tree under test:

- `cargo fmt --check`, and clippy with `-D warnings` on 1.98.1 and 1.85.0: clean.
- The whole workspace's tests: 4591 passed, 0 failed. `hatchling-dev audit`: all checks passed.
- **Hand-breaks**, each caught by the seat's tests:
  - `finish` taking any output;
  - a second refusal answered again;
  - a step finishing more than once;
  - the list showing what a record says;
  - one record past the packet's bound.
- **Mutation** on the changed lines:
  - `hatchling-core`: 16 mutants, 15 caught, 1 unviable.
  - `hatchling-agent`: 26 mutants, 22 caught, 4 unviable.

  The first pass missed eight, each fixed:
  - The two byte bounds were written `16 * 1024` and `32 * 1024`, which a test using only the constants can't tell from `16 + 1024`. They're numbers now, pinned by `the_bounds_are_what_they_say`.
  - The three tools' descriptions, which are what the model reads, are pinned by `each_tool_says_what_its_for`.
- `Cargo.lock` gains `serde` among the agent's dev-dependencies, for the test's claim type. It's already in the workspace and reviewed.
