#!/usr/bin/env bash
# Targeted cargo-mutants run.
#
# Faster feedback than the full-workspace run; the full run (via
# hatchling-dev) still gates WO close.
#
# Usage:
#   scripts/mutate-targeted.sh [extra cargo-mutants args]
# -> mutates the default ( cell-boundary) file list.
#   scripts/mutate-targeted.sh --file <path> [--file <path>...] [extra args]
#     -> mutates exactly the given files.
#
# Never run concurrently with another cargo-mutants process against this
# workspace (see AGENTS.md "Testing").
#
# `--workspace` scopes mutation to the whole workspace; `--test-workspace`
# is the separate flag that makes the per-mutant test run build every
# workspace binary. The agent e2e tests resolve the broker binary via
# CARGO_BIN_EXE in the scratch tree, so without it the run fails with
# "hatchling-broker binary not found".
#
# `--baseline=skip`: cargo-mutants' baseline hardcodes
# `PackageSelection::Explicit(all mutated packages)` (lab.rs
# `run_baseline`), ignoring `--test-workspace` — so a targeted run's
# baseline never builds the broker package and fails before testing
# any mutant. The baseline's job (prove the unmutated tree is green)
# is already done by `bash scripts/check.sh` on the frozen tree before
# this script runs; per-mutant timeouts fall back to the 300s default,
# ample for this suite (the e2e watchdog is 30s).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_NET_OFFLINE=true
rm -f mutants.out/outcomes.json
cargo build -p hatchling-agent --offline 2>/dev/null | tail -1
# Disk-backed and outside the repo: /tmp ENOSPCs the scratch build
# (phantom "unviable" for every mutant), and a scratch dir inside the
# repo makes cargo-mutants recurse into itself. See AGENTS.md.
mkdir -p "$HOME/mutants.scratch"
if [ "$#" -eq 0 ]; then
  set -- \
    --file crates/hatchling-sys/src/error.rs \
    --file crates/hatchling-sys/src/idmap.rs \
    --file crates/hatchling-sys/src/mounts.rs \
    --file crates/hatchling-sys/src/namespaces.rs \
    --file crates/hatchling-sys/src/process.rs \
    --file crates/hatchling-sys/src/seccomp.rs \
    --file crates/hatchling-sys/src/sync_pipe.rs \
    --file crates/hatchling-broker/src/cell.rs
fi
# No debug info: on 49 broker mutants, it took 1115s to 603s, with the same
# ones caught.
CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_PROFILE_TEST_DEBUG=0 \
TMPDIR="$HOME/mutants.scratch" \
HATCHLING_AGENT_BIN="$PWD/target/debug/hatchling-agent" \
cargo mutants \
  --workspace \
  --test-workspace=true \
  --baseline=skip \
  "$@"
