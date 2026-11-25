# Evidence: a request that says it's a tool call hides nothing from the policy (#685)

| | |
|---|---|
| Base | `main` at `3b2b82e2` (#678) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0 |
| Found by | an outside model's review of `policy.rs`, confirmed in the code |

## What was wrong

The request policy walks a model request's JSON and doesn't read the `input` of an object whose `type` is `tool_use`. That skip is meant for an Anthropic tool call in an assistant message: its `input` is the arguments of the model's own earlier call, and those may name URLs. But it applied at every depth, the root included.

In the Responses API (`/v1/responses`), the conversation is the root's `input`. A cell that added `"type": "tool_use"` at the root had its whole conversation skipped, so an image or file given by URL there, such as `http://169.254.169.254/…`, was never checked. An upstream that ignores the unknown field would then fetch it for the cell, which is what the policy exists to stop.

## What it does now

The skip applies only where an Anthropic tool call can be: below the root, and only to an `input` that is an object. A root that says `tool_use`, or a `tool_use` whose `input` is an array or a string, is read like everything else.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | pass |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | pass on both |
| the whole workspace's tests | 4491 passed, 0 failed |
| `hatchling-dev audit` | all checks passed |

New and changed tests:
- `a_root_that_says_tool_use_hides_nothing`:
  - the root body above is refused as content by URL, both by `check_body` and by `check` to a vendor, a local server and a gateway;
  - so is a root whose `input` is an object, as a tool call's would be, so the root's own check is tested apart from the object check;
  - a real Anthropic tool call in an assistant message, whose arguments hold URLs, still passes.
- The walk's own test now has three cases:
  - a root `tool_use` is read;
  - a nested one with an object `input` is skipped;
  - a nested one with an array `input` is read.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| The root's skip restored (`!root` taken as true) | `a_root_that_says_tool_use_hides_nothing`, `the_policys_parts_answer_on_their_own` |
| Any `input` skipped, not just an object | `the_policys_parts_answer_on_their_own` |
| The walk starting below the root | `a_root_that_says_tool_use_hides_nothing`, `the_policys_parts_answer_on_their_own` |

**Mutation**, on the lines changed against `3b2b82e2` (`cargo mutants --in-diff`, `hatchling-egress`): 9 tested, 9 caught, 0 missed, 0 timeouts.

All of the above ran on tree `2f7e93c0`.
