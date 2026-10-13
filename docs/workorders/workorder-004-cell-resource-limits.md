# WO-004: Per-cell resource limits

**Status:** CLOSED
**Area:** `crates/hatchling-isolation`, `crates/hatchling-sys`, `crates/hatchling-broker` (cell, run launcher, harness)
**Issue:** [#7](https://github.com/undergroundrap/hatchling/issues/7)
**Trust boundary:** yes

## Problem

Cells have no CPU, memory or process-count limits, beyond a runtime bound. A hostile cell can fork-bomb or allocate until the host's OOM killer runs, or fill a writable bind. A CLI cell can also clear its own parent-death signal (`prctl` allows any argument), so if its supervisor is killed from outside, the cell outlives it.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** a cgroup v2 leaf per cell with `pids.max`, `memory.max` and `cpu.max` from configuration, killed with `cgroup.kill` when the cell ends; rlimits (`RLIMIT_NPROC`, `RLIMIT_AS`, `RLIMIT_FSIZE`, `RLIMIT_NOFILE`) set in the cell before exec; a documented fallback when cgroup delegation isn't available.

**Not in scope:** disk quotas; per-cell network bandwidth.

## Acceptance criteria

- [x] A fork bomb in a cell stops at its `pids.max`, and the host is unaffected; a real-cell test.
- [x] A cell that clears its parent-death signal is still killed when its supervisor is; a real-cell test.
- [x] Configured limits are visible in the cell's cgroup and rlimits; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Closed by Ocean Bennett after local verification on 2026-10-13 (the last mutation pass ended 2026-10-14 UTC). GitHub CI remains pending publication.

Environment: WSL2, Ubuntu 26.04.1 LTS, Linux 6.6.87.2-microsoft-standard-WSL2; Rust 1.98.1 (pinned), 1.85.0 (MSRV), cargo-mutants 27.1.0. The ordinary broker/test user ran inside an operator-provisioned cgroup v2 delegation. `HATCHLING_TEST_CGROUP` named its empty cells parent; `HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1` made missing cell/cgroup support a failure. WSL automount and Windows interop stayed disabled.

- **Supervisor death:** [#52](https://github.com/undergroundrap/hatchling/pull/52), merged as `a6fb136`, made Hatchling's init pid 1. `a_cell_that_clears_its_death_signal_still_dies_with_its_supervisor` clears the program's signal, kills its supervisor, and checks that the cell disappears. It passes again in this part's full gates.
- **Hard rlimits:** [#53](https://github.com/undergroundrap/hatchling/pull/53), merged as `ac117ea`, sets soft and hard limits before exec. `a_cell_is_held_to_its_limits_and_cannot_lift_them` checks the configured values and `EPERM` on an attempted lift. Grouped cells use `pids.max` instead of applying a new `RLIMIT_NPROC`; the inherited host limit remains. The other hard rlimits still apply.
- **Aggregate limits:** `a_cell_fork_bomb_hits_its_cgroup_and_leaves_the_host_usable` launches a real CLI cell with `pids.max=64`, `memory.max=134217728` and `cpu.max="150000 100000"`. The probe creates 61 children and receives `EAGAIN`; `pids.events` records a limit hit. The cell stays at 64 tasks before and after a successful host `/bin/true` fork. The operator sees the configured values, the owner's membership stays unchanged, and the leaf disappears when the cell ends. The cell cannot see the host cgroup mount.
- **Cleanup:** direct controller tests kill all tasks and remove owned nested groups, including descendants whose helper never ran its destructor. Fresh-process real-cell replays cover skill success, timeout and output overflow, plus scheduled timeout and cancellation. Broker-owned outer groups retain cleanup ownership when a helper is killed. Each replay checks that its private parent has no child directories afterward.
- **Malformed settings:** a pure environment adapter distinguishes a missing variable from a present non-UTF-8 value across startup and all cell constructors. Synthetic errors and fresh-process `CellConfig::from_env` tests reject malformed cgroup, memory and CPU settings with the exact configuration key. Valid values and missing-value defaults remain unchanged; tests do not modify the parent process environment.

The following gate commands passed. Both plain clippy runs had zero warning/error sites. Both full gates report **2,100 libtest tests passed, zero failed**, plus **19 adversarial checks, ok**. The final tested code commit was `7c12d804bc04eed82683138c9845653ab7b2fbd0`; closing this work order changes only Markdown.

```bash
cargo fmt --all
cargo run -q -p hatchling-dev --locked -- preflight --base ac117ea
for tc in 1.98.1 1.85.0; do
  RUSTUP_TOOLCHAIN=$tc cargo clippy -q --workspace --all-targets --all-features --locked
  HATCHLING_REQUIRE_CELL=1 HATCHLING_REQUIRE_CGROUP=1 RUSTUP_TOOLCHAIN=$tc bash scripts/check.sh
done
HATCHLING_TEST_SPAWNER="$PWD/target/debug/hatchling" cargo test -q -p hatchling-broker --locked \
  --test skill_run --test schedule_run --test subagents --test harness --test harness_cli --test skill_cell_spawn
```

The runtime-spawner replay passed **48 tests, zero failed**. It and the frontend parity run preceded the malformed-environment correction; that correction was covered by the final dual-toolchain gates above.

**Mutation, base `ac117ea`:** every changed production `.rs` file was selected. The cgroup-module pass tested 37 candidates (32 caught, 5 unviable); the disjoint remaining-file pass tested 456 (381 caught, 71 unviable, 3 missed, 1 timeout). The three survivors changed CPU-parser `||` operators to `&&`; the tests now exercise unsigned-decimal sign errors in both components. Deleting `CellRun::drop` exposed a test peer waiting forever. A direct stop/removal test and a rescue guard make that broken cleanup fail by assertion without changing normal cancellation before connection.

The focused rerun tested 135 candidates: all of `cell.rs` and `serve.rs`, `CellRun::drop`, and one additional `scoped_config` field-deletion candidate selected by the regex. It reports **111 caught, 24 unviable, zero missed, zero timeouts**. It supersedes 132 earlier candidates; 361 outcomes from unchanged production functions remain valid. A fresh full discovery list exactly matches the combined outcome multiset: **496 unique candidates, 420 caught, 76 unviable, zero missed, zero timeouts; kill floor 1.0**. All 100 unviable attempts across the three passes have Rust compiler diagnostics, rather than resource failures. The final process audit found **zero orphans**. Original failures, reruns, logs, diffs and ledgers are retained as local proof.

```bash
export TMPDIR="$HOME/mutants.scratch"
mapfile -t files < <(git diff --name-only ac117ea -- '*.rs' | sed '/\/tests\//d; /\/src\/tests.rs$/d')
remaining=()
for file in "${files[@]}"; do
  if [ "$file" != crates/hatchling-sys/src/cgroup.rs ]; then remaining+=("$file"); fi
done
bash scripts/mutant-run.sh --base ac117ea --expected 37 -- crates/hatchling-sys/src/cgroup.rs --jobs 2
bash scripts/mutant-run.sh --base ac117ea --expected 456 -- "${remaining[@]}" --jobs 2
# After the fixes, rerun the affected scope:
bash scripts/mutant-run.sh --base ac117ea --expected 135 -- \
  --file crates/hatchling-broker/src/cell.rs --file crates/hatchling-broker/src/serve.rs \
  --file crates/hatchling-broker/src/run_launcher.rs \
  --re 'crates/hatchling-broker/src/(cell|serve)\.rs|<impl Drop for CellRun' --jobs 2
```

The `--expected 456` command records the original pre-fix discovery; the final complete source discovers 496 candidates overall. The initial production code and retained functions were identical to the final code outside the rerun scope.

**Hand mutants:** source-only copies were made with `git ls-files -z | xargs -0 cp --parents -t "$M"`, each with its own Cargo target. Omit placement, omit `pids.max`, omit group kill/removal, and omit broker cleanup ownership: all four fail their intended tests, zero timeouts. A fifth mutant deleting `CellRun::drop` fails the stop/removal unit and standalone real-cell cancellation in both fallback and grouped modes by assertion, zero timeouts. Both scratch checkouts were removed.

**Frontend/CI parity:** in `frontend/`, `npm ci`, `npm run check`, `npm run test:e2e`, `npm run test:smoke`, `npm run test:agent`, and `npm run screenshots` all pass. Live-adapter coverage stays at 100%; frontend and wire types did not change. The workflow parses with `yaml.safe_load`. Each gate and runtime-helper step moves its own shell into the delegated runner group through a privileged writer, then tests as the ordinary user. Independent read-only code review: **ACCEPT**.

`HATCHLING_CELL_CGROUP` is opt-in. Unset logs one warning and retains the documented rlimit fallback; an unusable configured delegation fails closed. Linux 5.14+ is required for grouped cleanup through `cgroup.kill`. Deletion is limited to subtrees created by Hatchling. Sudden death of the entire broker can leave populated groups until its helpers terminate; whole-broker/cgroup crash recovery is outside this work order. Disk quotas and network bandwidth limits remain outside scope.
