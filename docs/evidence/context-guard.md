# Evidence: a run keeps within its model's context window (WO-051 slice 2)

What was added, and how it was tested, for WO-051's context guard.

| | |
|---|---|
| Base | `main` at `d0ba8e00` (WO-051's design, #648) |
| Date | 2026-11-24 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |

## What it is

- **Each request is measured before it's sent** (`ChatCompletionsProvider::fits`), as it would go: built, redacted, and serialized as sent. Its size is what the model's server counted for the last request it answered (`usage.prompt_tokens`), plus a token for every 3 bytes added since. Before any count, it's a token for every 3 bytes of the whole. It fits when that, plus the room kept for the answer (`room_kept`: an eighth of the window, at least 1,024 tokens), is within the window. Without a declared window, every request fits.
- **The loop asks before every turn** (`Loop::with_room`). When the request won't fit, the model is told its context is full, offered no tools, and its answer is the run's (`LoopResult::out_of_room`). If even that request won't fit, nothing is sent, and the run ends `Error::OutOfRoom`. Nothing in the history is dropped or summarized.
- **A tool's output is cut to a quarter of the window** (`Loop::with_window`), ending on a character, with a line saying it was cut and from how much.
- **The agent wires both** for every run. An answer out of room exits with its own status (`ANSWERED_OUT_OF_ROOM`, 81), which the broker takes as an answer at a limit, as it takes 80, out of steps: delivered, and offered to keep for later. A run with no room for even its last request exits 78, out of room, as a reply cut off at its limit does.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | pass |
| the whole workspace's tests | 4448 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |

New tests:
- the room kept;
- a reply's prompt tokens;
- a request measured by its bytes, at the bound;
- one measured by the server's last count;
- one measured redacted;
- no window, every request fits;
- a full context ending in an answer with no tools;
- a request that won't fit at all never sent;
- a last turn for room that calls a tool or fails;
- a model with no window;
- room not asked when not set;
- a tool's output and a failure cut at the cap, and kept whole at it;
- a cut ending on a character;
- the statuses at a limit;
- 81 taken as an answer by the launcher;
- end to end, `a_run_whose_context_fills_answers_or_sends_nothing`: the agent's binary against a scripted server that counts its first request large.
  - At 28,650 of 32,768 tokens it answers with no tools offered, and exits 81.
  - At 30,000 it sends nothing after that request, and exits 78.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The loop never asks whether a request fits | `a_full_context_ends_the_run_with_an_answer_and_no_tools`, `a_request_that_wont_fit_at_all_is_never_sent`, `a_last_turn_for_room_that_isnt_an_answer_ends_out_of_room` |
| The server's count ignored | `a_request_fits_by_what_the_server_counted_last` |
| A tool's output never cut | `a_tools_output_is_cut_to_a_quarter_of_the_window` |

**Mutation**, on the lines changed against `d0ba8e00`:

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-loop` | 22 | 16 | 0 | 0 | 6 |
| `hatchling-model` | 32 | 25 | 0 | 0 | 7 |
| `hatchling-core` | 4 | 4 | 0 | 0 | 0 |
| `hatchling-agent` (library and `local_model`) | 8 | 4 | 0 | 0 | 4 |
| `hatchling-broker` | 7 | 7 | 0 | 0 | 0 |

The first pass missed 3:
- the default `fits` with room set;
- a failure cut at exactly the cap;
- `room_kept`'s `>` as `>=`, equivalent at exactly 1,024 and since written as a `max`.

One mutant of the cut's character search hung, so the search now walks a bounded range. Each of these got a test or a rewrite, and the second pass above is clean.

## Said plainly

- **The estimate isn't a tokenizer.** None runs in the cell, and tokenizers differ by model. Three bytes a token counts English prose high, so a run stops for room early rather than late. Once a server reports usage, its count is the base.
- **A server that reports no usage** is measured by bytes throughout. Ollama's OpenAI-compatible endpoint reports `prompt_tokens`, and so do the hosted providers.
- **Not measured yet:** how the guard changes a real run's turns, time and finishing. That's slice 5's measurement, with a small local model and a large hosted one.
