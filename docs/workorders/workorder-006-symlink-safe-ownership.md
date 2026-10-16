# WO-006: Symlink-safe ownership and staging

**Status:** CLOSED
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

- [x] A symlink swapped in at any of these points makes the operation fail closed, never act on the target; tested where it can be raced deterministically.
- [x] A pre-created staging directory name can't block or redirect a spawn; tested.
- [x] The broker socket's directory is never owned by the cell uid; tested as root.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Hardened WSL2 Ubuntu, Linux 6.6.87.2, `HATCHLING_REQUIRE_CELL=1`. Three pull requests, each gated on its own.

- **Part 1, staging ([#66](https://github.com/undergroundrap/hatchling/pull/66)):** each broker process makes one private staging root (mode 0700, a 128-bit random name, created exclusively), and each cell's directory inside it gets a random name too; a taken name, a directory or a symlink, is skipped and never followed.
  - `check.sh` green on Rust 1.98.1 and 1.85.0, 2,145 tests each; no staging roots left behind.
  - Mutation on the two changed files: 68 mutants, 58 caught, 10 unviable, 0 missed, 0 timeouts. The first run's six misses (the open's flags, and a redundant `Drop`) were fixed by testing the open directly and removing the `Drop`.
  - Three hand mutants failed their tests: using a planted name, building outside the private root, following a planted symlink.
- **Part 2, the socket handover ([#67](https://github.com/undergroundrap/hatchling/pull/67)):** the socket's directory stays the broker's (a root broker sets it `root:<cell>` 0710, takes back one an earlier version handed over, and refuses anyone else's), and the socket is chowned through an `O_PATH | O_NOFOLLOW` handle. The path-based `chown_ids` is gone.
  - `check.sh` green on both toolchains, 2,144 tests each.
  - Mutation on the two changed files: 75 mutants, 62 caught, 13 unviable, 0 missed, 0 timeouts.
  - Hand mutants failed their tests: following a symlinked socket, skipping the socket check, and, as root, giving the directory to the cell.
  - As root, the new test `as_root_the_socket_is_the_cells_and_its_directory_stays_the_brokers` passes.
- **Part 3, modes and owners ([#68](https://github.com/undergroundrap/hatchling/pull/68)):** the listener's mode is set through a no-follow handle; the ingress and device endpoints take their owner from the broker's own uid; the vault's and a run's directories are made exclusively at 0700 and set through a handle, and an existing run root is pinned; a skill's verified-code copy gets a random name.
  - Plain clippy clean and `check.sh` green on both toolchains, 2,148 tests each.
  - Mutation on the seven changed files: 190 mutants, 157 caught, 33 unviable, 0 missed, 0 timeouts, 0 orphans.
  - Three hand mutants failed their tests: opening the socket without `O_NOFOLLOW`, dropping the `fchmod` that restores 0700 under a strict umask, and the vault using a plain `create_dir`.
- **As root**, with a delegated test cgroup, after part 3: 32 of 34 test binaries pass. The two that fail, `mount_escape_all_writes_denied` and the `subagents` suite, fail on `main` too and belong to [WO-015](workorder-015-root-cell-tests-in-ci.md).

**Left over, out of this scope:** the memory and goals stores still repair a directory's mode by path after checking it. Only the broker's own user can write where they live. It's listed as a candidate in the [queue](README.md#candidates) and in the trust boundary's known limitations.
