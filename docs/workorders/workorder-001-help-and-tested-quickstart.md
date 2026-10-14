# WO-001: `--help` on every binary, a quickstart CI runs, and a hosted-model guide

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-broker`, `crates/hatchling-agent`, `crates/hatchling-dev`, `crates/hatchling-runtime`, shared CLI support in `crates/hatchling-core`, `docs/`, `README.md`, `.github/workflows/`
**Issue:** [#4](https://github.com/undergroundrap/hatchling/issues/4)
**Trust boundary:** no

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
- [ ] The CI quickstart job extracts the README's quickstart blocks rather than duplicating them, so editing the README changes what CI runs. It's green on the pinned toolchain.
- [ ] Following the hosted-model guide gets an agent's answer from a hosted endpoint, with the key never present in the cell (shown by the cell's environment and its view).

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

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
