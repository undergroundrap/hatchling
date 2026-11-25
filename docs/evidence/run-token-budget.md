# Evidence: a run has a token budget (WO-051 slice 3b)

What was added, and how it was tested, for WO-051's token budget: the broker's hard limit and the agent's last turn before it.

| | |
|---|---|
| Base | `main` at `5fee8784` (#653, slice 3a) |
| Date | 2026-11-24 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |

## What it is

- **A budget by kind.** A chat run gets `CHAT_RUN_TOKENS`, 2,000,000 input tokens. A run with nobody there, and every subagent, gets `RUN_TOKENS`, 200,000. The broker picks it as it launches the run (`run_tokens`), and holds the run's session to it.
- **The broker's limit.** The run's trail sums the input tokens each model reply reports. Once they reach the budget, the broker refuses the run's next model call, as it refuses a call past the run's call limit: logged, decided `OverLimit`, and denied. The agent can't spend past it whatever it does.
- **The budget told to the agent.** The run's spec carries it as `HATCHLING_RUN_TOKEN_BUDGET`, a subagent's included. The model's provider counts what its server reported, and before each turn the loop asks whether the next request still fits (`Model::affords`): what was spent, plus the next request measured as the window guard measures it. When it wouldn't, that turn is the last (`Loop::with_token_budget`): told its budget is nearly spent, offered no tools, and its answer is the run's.
- **Ending out of tokens.** An answer exits with its own status (`ANSWERED_OUT_OF_TOKENS`, 83), and the broker takes it as an answer at a limit, as it does 80 to 82. A last turn that isn't an answer exits `RunFailure::OutOfTokens` (77), and the owner is told the agent's budget ran out before it said where it got to.
- **One limit, not four flags.** A loop's result says which limit its answer was given at, if any (`Limit::{Steps, Room, Time, Tokens}`), and the agent's status is read from it.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | TESTS |
| `hatchling-dev audit` | AUDIT |

New tests:
- a run's session held to its kind's budget, and a subagent's to `RUN_TOKENS`;
- the budget in the agent's environment, read or refused;
- a run's calls refused once they reach its budget, and counted as refused;
- a request afforded by what was spent, and by its own size;
- a run out of tokens answering with no tools, and a last turn for tokens that isn't an answer ending out of tokens;
- the statuses at a limit, including 77 and 83, and 83 taken as an answer by the launcher;
- end to end, `the_broker_refuses_a_call_past_the_runs_budget`: the agent's binary, told no budget, is refused its third call by the broker, which never reaches the server, and the run fails;
- end to end, `a_run_at_its_token_budget_answers_with_no_tools`: the agent's binary, its first call reported as 10,000 tokens of a 15,000 budget, answers on its next turn with no tools offered and exits 83.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The loop never asks whether the next request is afforded | `a_run_out_of_tokens_answers_with_no_tools`, `a_last_turn_for_tokens_that_isnt_an_answer_ends_out_of_tokens` |
| The provider never counts what its server reported | `a_request_affords_the_budget_by_what_was_spent` |
| The trail never sums a reply's input tokens | `a_runs_calls_are_refused_once_they_reach_its_budget` |
| The broker never refusing a call past the budget | `the_broker_refuses_a_call_past_the_runs_budget` |
| A run's session given no budget | `a_runs_session_is_held_to_its_kinds_token_budget` |

**Mutation**, on the lines changed against `5fee8784`:

MUTATION

## Said plainly

- **The budget is the broker's.** The agent's count is its own estimate, so that its last turn comes before the broker's refusal. A run that misjudges it is still refused at the budget, and fails.
- **The broker counts what the server reports.** A server that reports no usage spends nothing against the budget. The run is still bounded by its clock and its call limit.
- **It's input tokens.** They are most of what a run costs, since every turn resends the history. Output tokens aren't counted.
- **The last call can cross it.** The broker refuses a call once the run has reached its budget, not one that would cross it, so a run's last call can take it past the budget by one request's worth.
