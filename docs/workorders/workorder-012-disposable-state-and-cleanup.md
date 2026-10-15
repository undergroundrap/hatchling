# WO-012: Separate disposable files from your data, and clean them up safely

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (startup, run launcher, a new operator subcommand), `crates/hatchling-isolation` (cell staging), `docs/`
**Issue:** [#18](https://github.com/undergroundrap/hatchling/issues/18)
**Trust boundary:** yes

## Problem

Hatchling keeps two kinds of files on disk, and nothing tells you which is which:

- **Your data:** the vault, schedules, memory, goals, spaces, artifacts and skills, under the `HATCHLING_*_DIR` directories. Deleting any of it loses credentials or history.
- **Disposable files:** a cell's staging directory (`$TMPDIR/hatchling-staging-<pid>-<random>/cell-<random>/`), a scheduled run's directory (`<scheduler dir>.runs/run-<token>/`) and its socket. The broker removes these when a cell or run ends normally, but a broker killed mid-run (`kill -9`, a crash, a power cut) leaves them behind, and nothing removes them later.

So the leftovers pile up after crashes, and someone freeing disk space has to guess which directories are safe to delete. A wrong guess deletes the vault. On WSL2 there's a second surprise: deleting files inside Linux doesn't shrink the virtual disk on Windows, which only grows until it's compacted.

## Scope

**In scope:**

- **A startup sweep.** Before it serves, the broker removes its own leftovers from an earlier run: staging directories whose broker pid is no longer alive, and run directories under its run root (at startup, no run is live). It logs one line per removed path, and a count.
- **`hatchling-broker clean`**, an operator subcommand that does the same sweep on demand. `--dry-run` lists what it would remove and removes nothing. It refuses while a broker is serving the configured socket.
- **Never your data.** The sweep only touches paths that match the disposable patterns above, under the staging and run roots. It never follows a symlink, never crosses a mount, and never descends into a `HATCHLING_*_DIR` state directory, even if one is configured inside a staging or run root.
- **A storage guide**, `docs/storage.md`: every directory Hatchling writes, whether it's your data or disposable, how to back up and restore your data (the vault's key or keyring included), and what to delete to uninstall.
- **A WSL2 note** in `docs/testing-on-wsl2.md`: the virtual disk doesn't shrink by itself; how to compact it, and that a build's `target/` is the usual reason it grew.

**Not in scope:** sweeping test leftovers from `cargo test` (a contributor concern, already guarded by the test suites), a cache-size limit or automatic deletion of your data (parked results are already capped at 64), compacting the WSL2 disk automatically (it needs `wsl --shutdown` and an administrator prompt), and changing where any directory lives.

## Acceptance criteria

- [ ] After a broker is killed with `SIGKILL` mid-run, the next broker start removes that run's directory and the cell's staging directory, and logs each path; tested with real cells.
- [ ] The sweep leaves a live broker's staging directories alone (its pid is alive); tested.
- [ ] `hatchling-broker clean --dry-run` lists exactly the paths `clean` would remove and changes nothing on disk; tested.
- [ ] `hatchling-broker clean` refuses, and removes nothing, while a broker serves the configured socket; tested.
- [ ] A symlink planted in a staging or run root, pointing at a state directory or any path outside the roots, is not followed: the target survives; tested.
- [ ] A state directory configured inside a staging or run root survives the sweep untouched; tested.
- [ ] `docs/storage.md` lists every directory and file the broker writes, each marked as your data or disposable, and a test fails if the broker gains a `HATCHLING_*_DIR` setting the guide doesn't list.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- The sweep deletes as the broker's user, which is root for a root broker, so it shares WO-006's concern: work on directory handles (`openat`, `unlinkat` with `AT_REMOVEDIR`, `O_NOFOLLOW`), never on re-resolved paths. If WO-006 lands first, reuse its helpers.
- A staging directory's name carries the pid of the broker that made it, which is what lets the sweep tell a dead broker's leftovers from a live one's. Pid reuse can make a dead broker's directory look live; the cost is only that it waits for a later sweep.
- Found while cleaning up a test machine after the launch: build output was most of the space, but the question users will ask is "what can I delete without losing anything?"
