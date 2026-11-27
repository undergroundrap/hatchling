# The agent says who made Hatchling (#764)

Closes [#764](https://github.com/undergroundrap/hatchling/issues/764), at the maintainer's request on 2026-11-27.

## What changed

The prompt pack's `identity` section (`crates/hatchling-prompt/src/lib.rs`) now says that Hatchling was created by Ocean Bennett, an independent developer, not an AI lab. It also says what that covers: the cell, the broker that carries out and checks every action, the vault, memory and the owner's approvals.

The model is separate, made by its own developer. Each run's instructions name it from Hatchling's settings (WO-056 slice 1, #759). Asked who it is or who made it, the agent answers with both:
- it's Hatchling, made by Ocean Bennett;
- it's running right now on that model.

It names the model's maker only if it can tell from the model's name, and otherwise says it doesn't know.

The text is fixed in the pack, matching `NOTICE.md`'s attribution, so nothing a run reads can change it. No tool call is needed: the model's name is given fresh in every run's instructions.

## Tests

The pack's change detector (`PINNED`, the sections' texts as literals, never derived from `SECTIONS`) pins the new text whole.

## The gate

On the sandbox, with `HATCHLING_REQUIRE_CELL=1`:
- `hatchling-prompt` 29 passed, `hatchling-loop` 70 passed, `hatchling-agent` 296 passed: none failed. That includes the agent's check that the loop's system message is the authored pack.
- clippy with `-D warnings` on `hatchling-prompt`, on 1.98.1 and 1.85.0: clean.
- `hatchling-dev audit`: all checks passed.

How models answer it is checked live in the real-model trial that closes WO-056.
