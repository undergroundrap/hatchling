# WO-045: The evaluation harness: how each model does as Hatchling's agent, measured and published

**Status:** QUEUED
**Area:** a new `bench/` (the runner, its fixtures and their stand-in models, in Python like the repository's other end-to-end drivers), `docs/evidence/models/` (published results), CI (the harness's own tests); no product code
**Issue:** [#590](https://github.com/undergroundrap/hatchling/issues/590)
**Trust boundary:** no

## Problem

Hatchling's agent is as good as the model behind it, and nothing measures that. Which model to suggest by default, whether a change to the agent's loop helps or hurts, and how a new model does here, are each judged today from a few chats.

A few chats miss what matters most. On the same real tasks, one local model said it had found things it never searched for and that it had made thirty files when it had made nineteen, while another reported its partial progress exactly. Both call the tools well. The difference is whether the agent's claims are true, and that is the difference an owner most needs to know before leaving an agent on.

The next work on the agent changes how it behaves: hosted OpenAI and Anthropic keys ([#534](https://github.com/undergroundrap/hatchling/issues/534)), runs bounded by time, cost and context rather than 25 turns ([#533](https://github.com/undergroundrap/hatchling/issues/533)), and *Keep going* ([#531](https://github.com/undergroundrap/hatchling/issues/531)). Each should be measured before and after. The scorecard's *Per-model conformance, published* and *Benchmark* rows are ❌ for the same reason.

## Scope

**In scope:**

- **A configuration is the unit.** The model, its server, its context window as set, whether it fits the machine, and thinking on or off. A changed configuration is a new basis, and scores are compared only within one.
- **Three suites**, each a set of fixtures with a pass rule a program checks:
  - *Conformance:* each tool called when it's the right one, with valid arguments; a refusal or error read and acted on; an answer given after the last call; calls per turn reported.
  - *Behaviour:* one fixture family per failure mode. Untrusted content (a file, a page, a tool's output) that carries instructions is treated as data. The answer's checkable claims are true of the disk and the Library. Every count in the agent's own report is re-derived, and a completion claim with nothing behind it fails. A task with a crisp done-state is stopped there, and a multi-part one is scored against a list the agent never sees. What the agent says it can do here matches its tools and grants. A rule given at the start still holds at the end of a long run. A decision changed partway is answered later by its current version.
  - *Tasks:* real work from the install drills: files attached and read, files and folders given, the Room's work kept across a restart, a long task that runs out of steps and reports honestly, a search, something this install can't do answered honestly, and a short non-coding job of several steps.
- **Against the real product.** Each run is a fresh throwaway install, made the way the release drills make one, driven over the local API as the app drives it. The harness never reaches into the broker.
- **Repeated, and reported as counts.** Each fixture runs N times (5 to start) and reports passes out of N, never one pass or fail.
- **Checked by programs.** Each checker reads the disk, the Library, the activity log and the answer's text. No model judges a fixture. What a program can't check isn't a fixture.
- **The harness proves its own checkers.** In CI, with scripted stand-in models: one that does each fixture right passes them all, and for each fixture one that makes exactly that fixture's mistake fails it.
- **Real models are run by hand.** No model key in CI and no scheduled spend. Local models run on the maintainer's machine. A hosted model's key is typed into the throwaway install's own vault and goes with it.
- **The first configurations:** qwen3.5:9b and gemma4 through Ollama, and a free model on OpenRouter. Direct OpenAI and Anthropic keys (after #534) and other OpenRouter models come later, each a configuration of its own.
- **Published as Hatchling's model benchmarks.** Each full run's results (JSON) and a table under `docs/evidence/models/`. Each result names the model, the provider's own id for it, its server and settings, the date, and Hatchling's commit. Failures are included, and anyone can run the same fixtures again.
- **Fixtures hold no one's data.** Every file, page and message in a fixture is made for it. A hosted run's results say which provider saw the fixtures.

**Not in scope:** changing the agent to score better (#533, #531 and the rest, each measured by this); a model as judge; comparing against other products' harnesses (the scorecard's benchmark row builds on this).

## Acceptance criteria

- [ ] The runner makes a throwaway install, runs a suite against one configuration N times, deletes the install, and writes a results file that names the configuration and the commit; a test reads one back.
- [ ] Every fixture has a stand-in that passes it and one that fails it with exactly its mistake, and CI runs both; with a checker made to pass everything, CI fails.
- [ ] The three suites hold at least: 5 conformance fixtures, one fixture for each behaviour family above, and the 7 tasks.
- [ ] Full runs of qwen3.5:9b, gemma4 and a free OpenRouter model are published under `docs/evidence/models/`, with the table, the raw results and the configuration of each.
- [ ] The scorecard's *Per-model conformance, published* row reflects the published runs.
- [ ] `docs/` says how to run the harness on a new model, and how to read a result.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates). This work order changes no Rust: its gate is the harness's own tests in CI, and the stand-ins that must fail each fixture.

## Evidence

Filled in at close.

## Notes

- Slices: (1) the runner, the result format, five conformance fixtures and the tasks, with their stand-ins; (2) the behaviour families; (3) the first real runs, published; (4) after #534, the direct OpenAI and Anthropic configurations.
- The local models need their own copy in the harness's install: a one-time download, done with the maintainer's approval.
- A free hosted model can change or be rate-limited by its provider at any time: each result records the date and the provider's model id, and the published table says so.
