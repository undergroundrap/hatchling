# WO-056 slice 1: each run is told which model runs it

Part of [#535](https://github.com/undergroundrap/hatchling/issues/535), [WO-056](../workorders/workorder-056-context-panel-and-model-in-use.md).

## What changed

- **`hatchling-prompt`: `model_note(name, endpoint)`.** In the pack's words, it says:
  - the model's name;
  - where it runs: on this machine ("the conversation stays here"), or hosted at a named host ("this conversation is sent to <host>");
  - that asked which model it is, the agent says so as Hatchling, and never names a model from its training as itself.

  `name` and the host pass through `label_value`, so neither can close the note or start another. The host is the endpoint's host alone, without scheme, credentials, port or path (`endpoint_host`). It counts as this machine for `localhost`, `127.0.0.1`, `::1` and `*.localhost`.
- **`hatchling-loop`: `Loop::with_model(name, endpoint)`,** beside `with_now`, adds the note at the end of the system prompt. Without both a name and an endpoint, it adds nothing.
- **`hatchling-agent`:** the run loop is given the settings the broker set for the cell (`HATCHLING_MODEL_NAME`, `HATCHLING_MODEL_ENDPOINT`). That's the broker's word, never the model's.

No wire change. The session protocol is unchanged: the settings were already given to every cell.

## Tests

- `hatchling-prompt`:
  - `the_model_note_names_the_model_and_where_its_words_go`: the whole note for a local model; four local endpoints; a hosted one naming its host twice; a forged name and host that stay on one line.
  - `an_endpoints_host_is_its_host_alone`: five endpoints and their hosts.
- `hatchling-loop`: `the_model_in_use_ends_the_system_prompt`: the note ends the system prompt exactly. With a name alone, an endpoint alone, or neither, the prompt is as it was.
- `hatchling-agent`, the seam: `a_chat_turns_loop_gives_the_model_its_conversation` now sets the cell's model settings and checks that the system message the model saw carries the note.

## Hand-breaks

Each one fails its test:

1. the cell never gives the loop its model settings (the seam test);
2. every host counts as this machine;
3. the name isn't cleaned;
4. a name alone is enough.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- **The whole workspace's tests:** 4697 passed, 0 failed.
- clippy with `-D warnings`, the whole workspace, on 1.98.1 and 1.85.0: clean.
- `hatchling-dev audit`: all checks passed.
- **Hand-breaks:** all four caught.
- **Mutation** on the change (`--in-diff`, `hatchling-prompt`, `hatchling-loop`, `hatchling-agent`): 10 mutants, 8 caught, 2 unviable, **none missed**.

**On the way:**
- The first build found that `hatchling-prompt` is a dev-only dependency of the agent, which reaches the prompt pack only through the loop. That's why the note goes through `Loop::with_model`, as the run's date does.
- It also found a local variable shadowing the run's model.
- clippy asked for a shorter first doc paragraph and a `map_or_else`.
