#!/usr/bin/env bash
# The single gate: run by CI and by the pre-push hook.
# If this is green locally, CI must be green.
set -euo pipefail
cd "$(dirname "$0")/.."

# The root pass (WO-015): CI's "cells as root" job, the cell suites run as
# root. It needs sudo and the README's users; see scripts/root-pass.sh.
if [ "${HATCHLING_ROOT_PASS:-}" = 1 ]; then
    echo "==> the root pass"
    exec bash scripts/root-pass.sh
fi

echo "==> cargo fmt --check"
cargo fmt --all -- --check

echo "==> cargo clippy (-D warnings)"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> cargo build"
# The end-to-end tests spawn the real binaries; `cargo test` alone does
# not build every binary they need, so build first on every gate run.
cargo build --workspace --locked

echo "==> cargo test"
cargo test --workspace --locked

echo "==> hatchling-dev seed-check"
# Binary-only audit: pins Python<->Rust hash-scheme equivalence on a
# fixed fixture (scripts/gen_seed_fixture.py). It reads no live-tree
# files, so work orders that touch seeded files cannot break it — and
# it must not run as a unit test, because under cargo-mutants the tree
# is deliberately mutated.
cargo run -p hatchling-dev --locked -- seed-check

echo "==> cargo deny check"
if ! command -v cargo-deny >/dev/null 2>&1; then
    echo "error: cargo-deny is not installed."
    echo "install it with: cargo install cargo-deny --locked"
    exit 1
fi
cargo deny check

echo "gate: green"
