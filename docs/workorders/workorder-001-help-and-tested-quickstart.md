# WO-001: `--help` on every binary, a quickstart CI runs, and a hosted-model guide

**Status:** QUEUED
**Area:** `crates/hatchling-broker`, `crates/hatchling-agent`, `crates/hatchling-dev`, `docs/`, `README.md`, `.github/workflows/`
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

- [ ] `hatchling-broker --help` lists every environment variable the broker reads; a test fails if the broker reads a variable `--help` doesn't list.
- [ ] `--help` and `-h` exit 0 with usage on stdout, on every binary, with no side effects (no socket, no directory, no vault touched); tested.
- [ ] An unknown argument exits 64 with usage on stderr; tested.
- [ ] The CI quickstart job extracts the README's quickstart blocks rather than duplicating them, so editing the README changes what CI runs. It's green on the pinned toolchain.
- [ ] Following the hosted-model guide gets an agent's answer from a hosted endpoint, with the key never present in the cell (shown by the cell's environment and its view).

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

The environment-variable list should come from the same place the broker reads them, so `--help` can't drift from the code.
