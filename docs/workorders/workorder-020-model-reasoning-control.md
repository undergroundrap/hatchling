# WO-020: Operator-selected model thinking mode

**Status:** DONE
**Area:** `crates/hatchling-model`, agent and broker configuration/run plumbing, shared help declarations, tests and docs
**Issue:** [#60](https://github.com/undergroundrap/hatchling/issues/60)
**Trust boundary:** yes (the model request/egress path)
**Milestone:** v0.2.0; blocks the remaining hosted-model proof in [WO-001](workorder-001-help-and-tested-quickstart.md)

## Problem

The production provider offers no operator control for an endpoint's thinking
mode. The exact free Nemotron Ultra endpoint sometimes returned text without
the required tool call. NVIDIA documents a matching serving limitation and a
thinking-disabled workaround. One focused request and one full private agent
run succeeded with that option. This demonstrates a working private run, not
the exact deployed upstream bug or long-term reliability.

WO-001 explicitly excludes new configuration. A hardcoded vendor/model
exception must not be hidden in its hosted-guide and CI change.

## Scope

**In scope:**

- A typed, operator-selected mode with provider-default and disabled choices.
  Unset preserves the existing request behavior. Disabled emits the documented
  `reasoning: {"enabled": false}` extension; unsupported endpoints may refuse it.
- Existing environment-based configuration and shared help declarations,
  strict validation, and consistent propagation through direct agent runs,
  scheduled runs and subagents. The child-spawn interface exposes no mode
  override; normal child runs inherit the operator-configured mode.
- The same option on capability probes and normal task requests, preserving
  all other request fields and the existing capability refusal.
- Docs, changelog and focused tests, plus the final shipping-candidate replay
  needed to unblock WO-001. Record any unavoidable public Rust API change
  explicitly and review it before implementation; prefer compatible additions.

**Not in scope:** selecting different models per child; increasing subagent
counts/depth; enabling coding harnesses for scheduled runs or subagents;
arbitrary request-body overrides; model/vendor detection; provider retries or
silent model fallback; replaying provider-specific reasoning metadata;
hardware authentication; frontend changes; paid spending or account changes;
new broker enforcement of reasoning fields in arbitrary hostile-cell requests.
This option configures the ordinary model client; the existing egress policy
does not constrain the reasoning field.

## Acceptance criteria

- [x] Unset/provider-default requests are byte/value-equivalent to the current
  behavior; no thinking parameter is silently added.
- [x] Disabled mode adds only the thinking-off option to both capability and
  task requests. Prompt, tools, required tool choice, destination, path,
  surrogate authentication, redaction and TLS policy retain their behavior.
- [x] Invalid mode values fail before work starts. Help lists the settings
  from the same declarations the readers use; the drift check covers them.
- [x] Direct, scheduled and child runs receive the same operator-selected
  mode. Mixed per-child model selection is neither exposed nor implied.
- [x] A text-only or malformed probe response is still refused, regardless
  of mode. A disabled-mode request is not itself proof of provider support.
- [x] Following the updated guide using the final candidate's ordinary
  binaries gets an actual answer from the exact approved Nemotron model,
  with protected key input, verified HTTPS, sealed vault, complete live-cell
  key-absence inspection, $0 cap/usage checks and checked cleanup.
- [x] WO-001 and this issue/spec are linked both ways; the completing PR
  closes the new issue only after its own gates and evidence are complete.

## Gates

- Both pinned Rust and MSRV gates with `HATCHLING_REQUIRE_CELL=1` and the
  required cgroup setup/flags; frontend gate per repository policy.
- Changed Rust functions called by name from tests; supervised mutation at
  kill floor 1.0 on every changed production Rust file.
- Source-only hand mutants for mode propagation, request shape and the
  capability-refusal guard, failing their intended assertions.
- Docs/changelog and any schema regeneration required by the actual change;
  no alteration of the frozen ProtocolAdapter contract without approval.
- Two independent trust-boundary reviews; record the tested source identity
  and replay any changed workflow YAML before publication.
- Separately authorized publishing, all required CI checks, and separate
  owner approval before merging.

## Evidence

Hardened WSL2 Ubuntu, Linux 6.6.87.2, `HATCHLING_REQUIRE_CELL=1`.

- **Gates:** `scripts/check.sh` green on Rust 1.98.1 and 1.85.0, 2,138 tests
  and 19 real-cell adversarial checks each, with the cgroup delegation
  required; frontend check green. After the two tests below were added,
  plain clippy is clean on both toolchains and the `hatchling-agent` (94)
  and `hatchling-broker` (450) library tests pass on both.
- **Mutation, kill floor 1.0:** 490 mutants across the 13 changed production
  files: 403 caught, 83 unviable, 0 missed, 4 timeouts. The four timed out
  under load (unrelated tests ran past 60 s with four jobs); rerun one job at
  a time with a 900 s limit, the 19 mutants of their four functions gave 14
  caught, 3 unviable and 2 missed: in both configuration readers, a
  non-UTF-8 value of any other setting read as empty instead of unset once
  the thinking-mode guard was forced true. Each reader gained a test that runs
  it in a child with a non-UTF-8 value on another setting; the rerun of
  `Config::from_env` then gave 10 caught, 3 unviable, 0 missed, 0 timeouts.
  No orphans.
- **Hand mutants:** six, each failing its intended test and passing restored:
  the request shape, the mode in direct, scheduled and child runs, the
  probe's refusal under `disabled`, and a non-UTF-8 thinking-mode value.
- **Hosted, the exact approved model:** `nvidia/nemotron-3-ultra-550b-a55b:free`
  through OpenRouter, with the candidate's own binaries and
  `HATCHLING_SCHEDULER_MODEL_REASONING=disabled`. The key had a $0 credit
  limit, was sealed in the vault, and was absent from the live cell's
  environment, argv and every read-only file. The first attempt parked its
  task; its reason wasn't recorded, and 9 of the key's 50 free daily requests
  were used, so it wasn't a rate limit. The second attempt, identical except
  that it saved the broker's debug log (checked to hold no key), delivered
  the agent's answer: "I'm Hatchling, a capable personal agent running inside
  a secured cell on your machine." Both requests returned 200, cap and usage
  stayed $0, and cleanup passed. The free endpoint isn't always reliable;
  a parked task can be scheduled again.

## Notes

NVIDIA's matching known limitation:
https://docs.nvidia.com/dynamo/dev/recipes/nemotron-3-ultra#known-limitations

OpenRouter's reasoning controls:
https://openrouter.ai/docs/guides/best-practices/reasoning-tokens
