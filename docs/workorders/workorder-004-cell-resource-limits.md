# WO-004: Per-cell resource limits

**Status:** QUEUED
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

- [ ] A fork bomb in a cell stops at its `pids.max`, and the host is unaffected; a real-cell test.
- [ ] A cell that clears its parent-death signal is still killed when its supervisor is; a real-cell test.
- [ ] Configured limits are visible in the cell's cgroup and rlimits; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
