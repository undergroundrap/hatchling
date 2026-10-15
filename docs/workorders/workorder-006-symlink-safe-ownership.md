# WO-006: Symlink-safe ownership and staging

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-isolation` (spawn), `crates/hatchling-sys` (ownership), `crates/hatchling-broker` (listener, ingress, device), `crates/hatchling-vault`
**Issue:** [#9](https://github.com/undergroundrap/hatchling/issues/9)
**Trust boundary:** yes

## Problem

Several privileged operations work on paths rather than handles. Handing the broker socket to a cell checks it with `lstat`, then chowns it by path, which follows symlinks. The listener sets the socket's mode by path after binding. Ingress and device endpoints read their owner from the path after binding. Vault directories are created first and chmodded after. Cell staging directories have predictable names under a shared `TMPDIR` and aren't created exclusively, so a local user can pre-create one and deny cell spawns.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** changing ownership and mode through `O_PATH | O_NOFOLLOW` handles; keeping the broker socket's directory broker-owned (traverse-only for the cell) and handing over only the socket; taking endpoint owners from the broker's own uid; creating directories with their final mode; creating staging directories exclusively, with random names, under a broker-owned directory, and checking their owner through the handle.

**Not in scope:** changes to which paths are configurable.

## Acceptance criteria

- [ ] A symlink swapped in at any of these points makes the operation fail closed, never act on the target; tested where it can be raced deterministically.
- [x] A pre-created staging directory name can't block or redirect a spawn; tested.
- [x] The broker socket's directory is never owned by the cell uid; tested as root.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
