#!/usr/bin/env bash
# The root pass (WO-015): the cell test suites, run as root, the way the
# README's quickstart runs the broker. A root broker gives its cells a uid
# of their own, hands its sockets and run directories to other users, and
# drops to users of its own: paths an unprivileged run never takes.
#
# Run by CI's "cells as root" job, and by `HATCHLING_ROOT_PASS=1 bash
# scripts/check.sh`. It builds as you, then runs each suite's test binary
# as root (sudo), from a copy the cells' user can read. It needs:
#
#   - sudo, and a kernel that builds cells (see CONTRIBUTING);
#   - the README's users, which it never makes itself:
#       for u in hatchling-cell hatchling-broker hatchling-keeper hatchling-egress; do
#         sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin "$u"
#       done
#   - optionally HATCHLING_TEST_BROWSER, a headless Chromium every user
#     can read (not under your home), for the browser cells' suites.
#
# Every cell suite must run: HATCHLING_REQUIRE_CELL=1 turns a skip into a
# failure, and HATCHLING_REQUIRE_ROOT_MOUNTS=1 does for the suites that
# mount. A test that can't apply to a root run says so, and the pass
# lists each one at the end with its reason.
set -euo pipefail
cd "$(dirname "$0")/.."

# The suites whose behavior depends on running as root: every test that
# builds a cell.
SUITES=(
    "-p hatchling-broker --test adversarial"
    "-p hatchling-broker --test artifacts"
    "-p hatchling-broker --test browser_cell"
    "-p hatchling-broker --test cell_boundary"
    "-p hatchling-broker --test harness"
    "-p hatchling-broker --test harness_cli"
    "-p hatchling-broker --test schedule_run"
    "-p hatchling-broker --test skill_cell_spawn"
    "-p hatchling-broker --test skill_envelope"
    "-p hatchling-broker --test skill_run"
    "-p hatchling-broker --test spawner"
    "-p hatchling-broker --test subagents"
    "-p hatchling-broker --test sweep"
    "-p hatchling-broker --test unknown_outcome"
    "-p hatchling-isolation --lib"
    "-p hatchling-sys --test locked_mount"
    "-p hatchling-sys --test process_groups"
    "-p hatchling-sys --test seccomp_startup"
)

USERS=(hatchling-cell hatchling-broker hatchling-keeper hatchling-egress)
missing=()
for user in "${USERS[@]}"; do
    id -u "$user" > /dev/null 2>&1 || missing+=("$user")
done
if [ ${#missing[@]} -gt 0 ]; then
    echo "error: the root pass needs the README's users; missing: ${missing[*]}"
    echo "make them with:"
    echo "  for u in ${missing[*]}; do sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin \"\$u\"; done"
    exit 1
fi

echo "==> build, as $(id -un)"
cargo build --workspace --locked --bins

# A root broker's cells run as hatchling-cell, which can't read your
# home: the test binaries and the binaries they run go where it can, laid
# out as target/debug is (each test finds its fixtures beside it).
copy=$(mktemp -d /tmp/hatchling-root-pass.XXXXXX)
cg=/sys/fs/cgroup/hatchling-root-pass-$$
cleanup() {
    sudo rm -rf -- "$copy"
    [ -d "$cg" ] && sudo rmdir "$cg/cells" "$cg/job" "$cg" 2> /dev/null || true
}
trap cleanup EXIT
mkdir "$copy/deps"
target=$(cargo metadata --format-version 1 --no-deps | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
for binary in "$target"/debug/*; do
    if [ -f "$binary" ] && [ -x "$binary" ]; then
        cp "$binary" "$copy/"
    fi
done
declare -A crate_of
for suite in "${SUITES[@]}"; do
    exe=$(cargo test --locked --all-features $suite --no-run --message-format=json \
        | grep -o '"executable":"[^"]*/deps/[^"]*"' | tail -1 | cut -d'"' -f4)
    if [ -z "$exe" ]; then
        echo "error: no test binary for: $suite"
        exit 1
    fi
    cp "$exe" "$copy/deps/"
    crate_of[$(basename "$exe")]=$(echo "$suite" | awk '{print $2}')
done
chmod -R a+rX "$copy"

# The cells' resource limits: a cgroup of the pass's own, the tests in
# one leaf and their cells' groups beside it.
sudo mkdir "$cg" "$cg/job" "$cg/cells"
echo '+pids +memory +cpu' | sudo tee "$cg/cgroup.subtree_control" > /dev/null

cell_uid=$(id -u hatchling-cell)
browser=()
if [ -n "${HATCHLING_TEST_BROWSER:-}" ]; then
    browser=("HATCHLING_TEST_BROWSER=$HATCHLING_TEST_BROWSER" "HATCHLING_REQUIRE_BROWSER=1")
fi
logs="$copy/logs"
mkdir "$logs"
failed=()
for exe in "$copy"/deps/*; do
    name=$(basename "$exe")
    crate=${crate_of[$name]}
    echo "==> $crate $name, as root"
    # As the README's root setup: cells run as hatchling-cell, the id
    # map's base, and must run (no skip).
    if sudo env HATCHLING_REQUIRE_CELL=1 HATCHLING_REQUIRE_CGROUP=1 HATCHLING_REQUIRE_ROOT_MOUNTS=1 \
        HATCHLING_TEST_CGROUP="$cg/cells" HATCHLING_CELL_UID_BASE="$cell_uid" \
        "${browser[@]}" PATH="$PATH" \
        bash -c 'echo $$ > "$1/job/cgroup.procs" && cd "crates/$2" && exec "$3"' \
        root-pass "$cg" "$crate" "$exe" 2>&1 | tee "$logs/$name.log"; then
        :
    else
        failed+=("$crate $name")
    fi
done

echo "==> skipped as root, with their reasons"
if ! grep -ahi '^skipping\|skipped:' "$logs"/*.log | sort | uniq -c; then
    echo "none"
fi

if [ ${#failed[@]} -gt 0 ]; then
    echo "root pass: failed: ${failed[*]}"
    exit 1
fi
echo "root pass: green"
