# WO-001: `--help` on every binary, a quickstart CI runs, and a hosted-model guide

**Status:** DONE
**Area:** `crates/hatchling-broker`, `crates/hatchling-agent`, `crates/hatchling-dev`, `crates/hatchling-runtime`, shared CLI support in `crates/hatchling-core`, `docs/`, `README.md`, `.github/workflows/`
**Issue:** [#4](https://github.com/undergroundrap/hatchling/issues/4)
**Trust boundary:** no

Hosted-model proof depends on [WO-020](workorder-020-model-reasoning-control.md)
([#60](https://github.com/undergroundrap/hatchling/issues/60)) for the explicit
thinking-mode setting. That configuration change remains outside this WO.

## Problem

A newcomer's first command is `hatchling-broker --help`, and it doesn't print help: the broker parses its environment first, then treats `--help` as an unknown operator subcommand. The broker is configured entirely by environment variables, and the README's quickstart shows only the ones it needs.

The quickstart itself works today, run by hand, but nothing keeps it working: if a variable is renamed, the README silently breaks. And it only covers a local model. A hosted API needs its key in the vault, and that setup isn't written down.

## Scope

**In scope:**

- `--help` (and `-h`) on `hatchling-broker`, `hatchling-agent` and `hatchling-dev`: usage, subcommands (the broker's include `migrate-vault`, `rekey-vault` and `vault-keyring`), and every environment variable the binary reads, with a one-line meaning and whether it's required. Printed to stdout, exit 0, before configuration is parsed, so it works with nothing set.
- `hatchling-agent`'s help says it runs inside a cell the broker builds.
- An unknown argument prints usage to stderr and exits 64 (EX_USAGE).
- A CI job that runs the README's quickstart blocks as written (as root, which CI runners allow through `sudo`), with the stand-in model, and fails unless the agent's answer arrives.
- A guide to a hosted OpenAI-compatible API: storing its key in the vault, the egress allowlist and grants, and what the agent's requests look like at the boundary.

**Not in scope:** new configuration (a config file, flags replacing the environment variables), changes to what any variable means, running without root, and the frontend.

## Acceptance criteria

- [x] `hatchling-broker --help` lists every environment variable the broker reads; a test fails if the broker reads a variable `--help` doesn't list.
- [x] `--help` and `-h` exit 0 with usage on stdout, on every binary, with no side effects (no socket, no directory, no vault touched); tested.
- [x] An unknown argument exits 64 with usage on stderr; tested.
- [x] The CI quickstart job extracts the README's quickstart blocks rather than duplicating them, so editing the README changes what CI runs. It's green on the pinned toolchain.
- [x] Following the hosted-model guide gets an agent's answer from a hosted endpoint, with the key never present in the cell (shown by the cell's environment and its view).

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

### Part 3, the hosted-model guide

`docs/hosted-models.md` covers the vault import (a hidden prompt; the key never
enters an argument, the environment or shell history), the HTTPS destination,
allowlist and grants, and what the agent sends at the boundary, with OpenRouter
as the worked example.

- **CI, no real key:** two new jobs run the guide's path in a disposable
  namespace. `local` serves a pinned llama.cpp model that requires an API key
  through `local:localhost:18090`; `https` uses a named HTTPS stand-in with a
  disposable CA supplied only to the broker. Each refuses a missing or wrong
  key (401 on both endpoints), seals a generated key (`HVL1`, vault 700, file
  600), runs the documented first-run client, requires the scheduled agent's
  answer, and inspects the live cell's environment, argv and every read-only
  file for the key. Both passed, replaying the exact workflow YAML, with the
  host's accounts and binaries unchanged.
- **Gates on that tree** (commit `61d9d38`, rebased here unchanged in code):
  both toolchains passed 2,127 tests and 19 adversarial checks each, with
  `HATCHLING_REQUIRE_CELL=1` and the cgroup delegation required; the frontend
  check passed 397 tests with 100% live adapter coverage. No Rust changed in
  this part, so there is no mutation target.
- **A real hosted provider:** following the guide with WO-020's setting, the
  exact model `nvidia/nemotron-3-ultra-550b-a55b:free` through OpenRouter, a
  key with a $0 credit limit, and the candidate's own binaries, the scheduled
  agent answered: "I'm Hatchling, a capable personal agent running inside a
  secured cell on your machine." The key was sealed and absent from the live
  cell, cap and usage stayed $0, and cleanup passed. The first of the two
  attempts parked its task instead; see
  [WO-020's evidence](workorder-020-model-reasoning-control.md#evidence).

### Part 2, CI result

The `README quickstart` job passed on
[#59](https://github.com/undergroundrap/hatchling/pull/59), with both Rust gates.

### Part 2, README quickstart CI

The new `README quickstart` job extracts the four shell blocks in the README's
Quickstart section and runs them together as root with the stand-in model.
It pins the documented clone to the checked-out commit, verifies the cloned
tree stayed unchanged, and requires the exact agent answer rather than a
scheduling acknowledgment. Its PID namespace ends every descendant on exit;
the job retains the extracted commands, source identity and per-block logs.

Local proof used frozen commit `1303903`, tree
`556e15b2ea37085dc64ce6e8aa3ec7408e8c2e4f`:

- The complete workflow YAML parsed, and every new shell body passed `bash -n`.
  The exact root run body was replayed on pinned Rust 1.98.1 in a disposable
  mount/PID/network namespace. All four extracted README blocks passed;
  the real cell's stand-in answer arrived. Host accounts and installed
  binaries stayed unchanged, and all private replay state was removed.
- `python3 -m unittest discover -s scripts -p 'test_readme_quickstart.py' -v`
  passed **five tests**: changed README commands execute, exports persist,
  output fences and subsequent sections are excluded, malformed input fails,
  and acknowledgments/near matches cannot satisfy the answer check.
- Two source-only Python hand mutants failed their named assertions:
  bypassing `require_answer` failed
  `test_schedule_acknowledgment_and_near_matches_cannot_pass`; replacing the
  extracted commands with `true` failed
  `test_commands_change_with_the_readme_and_share_shell_state`.
  The restored suite passed all five tests; neither proof used a timeout.
- A shallow detached checkout, matching CI's checkout form, cloned through
  the pinned snapshot with identical commit and tree.
- Both 1.98.1 and 1.85.0 ran `HATCHLING_REQUIRE_CELL=1
  HATCHLING_REQUIRE_CGROUP=1 bash scripts/check.sh` with a delegated
  `HATCHLING_TEST_CGROUP`: **2,127 tests, zero failures, 19 adversarial checks**
  each, `gate: green`. Both plain clippy passes and preflight passed.
  `npm ci && npm run check` passed, including 100% live adapter coverage.
- No Rust file changed in Part 2; cargo-mutants has no changed Rust target.
  The two hand mutants above cover the new Python failure guards.

The first local gate launcher stopped before tests on its cgroup-placement
setup; it was corrected before the full gates above. After both gates and the
frontend check passed, the revised wrapper's immediate cgroup-population
assertion failed. Its event contents at that instant were not recorded, so
the transient cause is not claimed as proven. A separate bounded cleanup
audit found no remaining tasks and removed only the empty owned cgroup.
Both failed wrapper logs remain preserved; the successful test-command
results were not rewritten or rerun.

The quickstart checkbox remains open until the GitHub CI result is recorded.
Hosted-model guidance and its manual provider proof also remain pending;
this partial change references issue #4 and keeps WO-001 IN-PROGRESS.

### Part 1, retested after the security dependency repair

Configuration-free help covers the installed `hatchling` front door and all
three standalone binaries. Shared declarations supply 65 broker, nine agent
and three development settings, including cell limits and cgroups.
The source audit fails on missing settings and unsupported dynamic readers;
process tests cover stdout/stderr, exit status and unchanged filesystem state.

The help change was reconciled with main `2ba3cb7`, the rustls repair in
[#58](https://github.com/undergroundrap/hatchling/pull/58), then fully retested.
The frozen input was commit `ee857b8`, tree
`f8bda642be71a25dde967b294836e3780755af72`. Its source stayed unchanged throughout the fresh run.

- Both toolchains ran `HATCHLING_REQUIRE_CELL=1 HATCHLING_REQUIRE_CGROUP=1
  bash scripts/check.sh`, with a delegated `HATCHLING_TEST_CGROUP`: 1.98.1
  and 1.85.0 each passed **2,127 tests, zero failures**, `gate: green`,
  and **19 adversarial checks**. Both plain clippy passes, format checking
  and preflight against `2ba3cb7` passed.
- `npm ci && npm run check` passed, with 100% live adapter coverage.
- One fresh supervised run tested all **913 distinct mutants** from all
  24 changed production Rust files: **776 caught,
  137 unviable, zero missed, zero timeouts**, kill floor 1.0.
  Discovery identities, every build/test phase, compiler diagnostics and
  the unchanged source fingerprint were verified from the original outputs.
  Resource failures were not accepted as mutation evidence. No new
  equivalence exclusions were added.
- Eight source-only hand mutants failed their intended tests: four unlisted
  key/read variants and three unsupported dynamic/alias variants failed
  `broker_reads_are_listed_in_help`; replacing exit 64 with success failed
  `unknown_arguments_are_stderr_usage_with_exit_64`. Zero runner timeouts;
  the restored help suite passed all 13 tests.

Fresh run: `20261014T183157Z-f546d761`. The source hash was
`f546d761635a9c1ef608cd5d9b99223e0bd128dfdd628dd55422c769d8fc9f46`. Earlier runs and their original partial ledgers
remain preserved as historical evidence below; their results were not reused
to fill this fresh run.

```bash
base=2ba3cb70428ed6fdca43bb4285bf629d573a530a
mapfile -t files < <(git diff --name-only "$base" -- 'crates/*/src/*.rs')
scripts/mutant-run.sh --base "$base" --expected 913 -- "${files[@]}" --jobs 4
```

Quickstart CI and hosted-model guidance remain pending. This partial change
references issue #4 without closing it.

### Earlier evidence on the original base

Part 1: configuration-free help (base `1781af8`). The installed
`hatchling` front door and all three standalone binaries are covered. Shared
declarations supply 65 broker, nine agent and three development settings.
The source audit fails on missing settings and unsupported dynamic readers;
process tests cover stdout/stderr, exit status and unchanged filesystem state.

- Linux, ordinary user, with a delegated cgroup parent. Both toolchains ran
  `HATCHLING_REQUIRE_CELL=1 HATCHLING_REQUIRE_CGROUP=1 bash scripts/check.sh`
  with `HATCHLING_TEST_CGROUP` set: 1.98.1 and 1.85.0 each passed **2,127
  tests, zero failures**, `gate: green`; each passed **19 adversarial checks**.
- `npm ci && npm run check` passed, with 100% live adapter coverage.
- The supervised run below discovered **913** mutants. A machine restart interrupted it
  after 721 completed (605 caught, 116 unviable). The unchanged source was
  verified by its content hash, and a supervised recovery run selected every
  untested identity: 194 completed, including two already-tested struct-field
  cases that cargo-mutants 27.1.0 adds despite its regex filter. Verifying and
  de-duplicating the two real runs gives **913 distinct mutants: 776
  caught, 137 unviable, zero missed, zero timeouts**, kill floor 1.0.
  The interrupted ledger remains partial; no result was silently discarded.
  No mutation workers remained. No new equivalence exclusions were added.
- Eight source-only hand mutants built and failed their intended tests:
  four unlisted key/read variants and three unsupported dynamic/alias variants
  failed `broker_reads_are_listed_in_help`; replacing exit 64 with success
  failed `unknown_arguments_are_stderr_usage_with_exit_64`. Zero runner
  timeouts; the restored help suite passed all 13 tests.
- Two independent source reviews accepted the final corrections; an
  independent recovery review accepted the evidence-preserving resume method.

Quickstart CI and hosted-model guidance remain pending; this part references
issue #4 without closing it.

```bash
base=1781af8398cfbd7e39c5782d9f3dc6e475e8d3f6
mapfile -t files < <(git diff --name-only "$base" -- 'crates/*/src/*.rs')
scripts/mutant-run.sh --base "$base" --expected 913 -- "${files[@]}" --jobs 4
```

The recovery used the same files and test configuration, with `--re` set to
the anchored literal union of the untested names in the saved discovery list
and `--expected 194`. The saved per-run ledgers and a verified aggregate
retain the identity checks and original logs.

## Notes

The environment-variable list should come from the same place the broker reads them, so `--help` can't drift from the code.

The owner's scope clarification includes `hatchling`, the installed front
door added after this WO was written. Its help includes the broker and
agent inventories, and `hatchling broker --help` / `hatchling agent --help`
use their respective command lines. Work is split into help, README-run
quickstart CI, and hosted-model guidance; partial PRs reference #4 and
only the completing PR closes it.
