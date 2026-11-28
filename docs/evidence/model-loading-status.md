# #775: Thinking, the tokens so far, and a local model loading

[#775](https://github.com/undergroundrap/hatchling/issues/775). Found in the WO-057 real-model trial: after a restart, gemma4:12b took 1 min 44 s to answer the first message while Ollama loaded it onto the GPU. The chat said only "Asking the model", which looked the same as a hang.

## What changed

- **`agent_step` (UI schema v19)** gains two optional fields:
  - **`tokens`:** what the run's model has written so far, as its server reported it. It's told again after each reply.
  - **`loading`:** the model's name, while the run waits for a model that its server on this machine says isn't loaded.
- **Whether it's loaded** comes from the model server's own list (`GET /api/ps`). The broker asks it as a chat run's model step starts, through the same bounded plain-HTTP read it already uses, waiting at most a second (`model_window::is_loaded`).
  - "Not loaded" is said only when the server listed what it has loaded and the model isn't among them.
  - A hosted server, one that isn't Ollama, or no answer in time says nothing: never a guess.
- **The status line:**
  - "Thinking" replaces "Asking the model".
  - "Loading gemma4:12b" shows while the model loads.
  - After the elapsed time comes what the model has written so far: "Thinking · 1 min 56 s · 2.1K tokens".
  - Status titles stay within the three words the app's rule allows (`validThreadCopy`). The tokens come through the usage port (`written`), not the thread's locked shape.

## Tests

| Rule | Test |
| --- | --- |
| Loading is said only on the server's "not listed"; each reply tells the tokens and that loading ended; a run that isn't the owner's tells nothing | `run_trail::a_chat_run_tells_the_model_loading_and_its_tokens_so_far` |
| Loaded only from a list of loaded models, by name or `latest`; HTTPS, other answers, and a refusing port can't tell | `model_window::a_model_is_loaded_only_when_the_server_lists_it` |
| The wire: `tokens` and `loading`, and a step from before that has neither | `ui::a_step_says_its_tokens_and_the_model_being_loaded` |
| The status words, and the tokens kept for the main chat until its answer | `live-adapter.test.mjs`, `live-usage.test.mjs` |

## Hand-breaks

| Break | Failed |
| --- | --- |
| Loading never said | 1 |
| The tokens not told after a reply | 1 |
| No tokens ever | 1 |
| Every model is loaded | 1 |
| The stream drops the tokens | 1 |

## Gates

In the WSL2 sandbox, with `HATCHLING_REQUIRE_CELL=1`:

- **Workspace tests:** 4749 passed.
- **Clippy, `-D warnings`:** clean on 1.98.1 and 1.85.0. fmt is clean, and `hatchling-dev audit` passes.
- **Mutation, `--in-diff`:** the first pass caught 21 of 22. The miss was a guard that a reply follows the model's step, which is always true; it's gone, and the rerun caught 20 of 20.
- **Frontend:** `npm run check` green (645, 285 and 16 tests). Wire types regenerated.
