# Evidence: a chat run is bounded by its time, not 25 turns (WO-051 slice 3a)

What was added, and how it was tested, for WO-051's clock by kind and the deadline told to the agent.

| | |
|---|---|
| Base | `main` at `2cee686c` (#652, after #649) |
| Date | 2026-11-24 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |

## What it is

- **A clock by kind.** A chat run, whose owner is there, gets `CHAT_RUN_TIMEOUT`, thirty minutes. A run with nobody there keeps `DEFAULT_RUN_TIMEOUT`, ten. The broker picks it as it launches the cell (`clocked`), and its own clock ends the run at it, as before.
- **The deadline told to the agent.** The run's spec carries its deadline in Unix seconds, `HATCHLING_RUN_DEADLINE`, set at launch. Inside its last 90 seconds the loop's next turn is its last (`Loop::with_deadline`): told its time is nearly up, offered no tools, and its answer is the run's, `out_of_time`.
- **Ending out of time.** An answer exits with its own status (`ANSWERED_OUT_OF_TIME`, 82). The broker takes it as an answer at a limit, as it does 80 and 81: delivered, and offered to keep for later. A last turn that isn't an answer exits `RunFailure::OutOfTime` (79), and the owner is told the agent's time ran out before it said where it got to.
- **Turns.** A chat run's cap is 200, a backstop against a loop. A run with nobody there keeps 10, and the subagent caps are unchanged.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4455 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |

New tests:
- a run clocked by its kind, and told a deadline that far away;
- the deadline in the agent's environment, or nothing without one;
- a run out of time answering with no tools, and one with time left going on;
- a last turn for time that calls a tool, fails, or doesn't fit;
- the 90 seconds;
- the statuses at a limit, including 79 and 82;
- 82 taken as an answer by the launcher;
- a chat's 200 steps;
- end to end, `a_run_near_its_deadline_answers_with_no_tools`: the agent's binary told a deadline 30 seconds away answers on its first turn, with no tools offered, and exits 82.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The loop never looks at its deadline | `a_run_out_of_time_answers_with_no_tools`, `a_last_turn_for_time_that_isnt_an_answer_ends_out_of_time` |
| A chat run given the clock of a run with nobody there | `a_run_is_clocked_by_its_kind_and_told_its_deadline` |
| The deadline never reaching the agent's environment | `a_runs_deadline_is_in_its_agents_environment` |
| The agent's last turn put an hour after its deadline | `a_run_near_its_deadline_answers_with_no_tools` |

**Mutation**, on the lines changed against `2cee686c`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-loop` | 19 | 13 | 0 | 0 | 6 |
| `hatchling-core` | 8 | 8 | 0 | 0 | 0 |
| `hatchling-agent` (library and `local_model`) | 14 | 6 | 0 | 0 | 8 |
| `hatchling-broker` | 19 | 14 | 0 | 0 | 5 |

## Said plainly

- **The deadline is the broker's.** A run told a later one, or none, is still ended at its clock. The agent's last turn is its chance to say where it got to, and nothing depends on it taking it.
- **Clocks drift by at most a second here.** The deadline is set as the cell is launched, a moment before the broker's own clock starts, so the agent's is never later than the broker's.
- **A slow model's last turn can still be cut off.** A turn started inside the 90 seconds that takes longer than what's left is ended by the broker's clock, as any run past its time is.
