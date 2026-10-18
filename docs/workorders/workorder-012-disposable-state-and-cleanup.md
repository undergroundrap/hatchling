# WO-012: Separate disposable files from your data, and clean them up safely

**Status:** DONE
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

- [x] After a broker is killed with `SIGKILL` mid-run, the next broker start removes that run's directory and the cell's staging directory, and logs each path; tested with real cells.
- [x] The sweep leaves a live broker's staging directories alone (its pid is alive); tested.
- [x] `hatchling-broker clean --dry-run` lists exactly the paths `clean` would remove and changes nothing on disk; tested.
- [x] `hatchling-broker clean` refuses, and removes nothing, while a broker serves the configured socket; tested.
- [x] A symlink planted in a staging or run root, pointing at a state directory or any path outside the roots, is not followed: the target survives; tested.
- [x] A state directory configured inside a staging or run root survives the sweep untouched; tested.
- [x] `docs/storage.md` lists every directory and file the broker writes, each marked as your data or disposable, and a test fails if the broker gains a `HATCHLING_*_DIR` setting the guide doesn't list.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

One pull request, [#106](https://github.com/undergroundrap/hatchling/pull/106), gated in a disposable WSL2 sandbox (bubblewrap, read-only `/`, only the work directory writable) with `HATCHLING_REQUIRE_CELL=1`:
- plain clippy clean on Rust 1.98.1 and 1.85.0;
- `check.sh` green on both, 2,339 tests each (`cargo deny` left to CI: the sandbox has no network, and `Cargo.lock` is unchanged);
- mutation on its changed lines: 175 mutants, 161 caught, 14 unviable, 0 missed, 0 timeouts, 0 orphans;
- canary files intact after every step and throughout mutation;
- 21 hand mutants failed their tests:
  - the tree removal: following a symlink at its top, entering a mount, ignoring a kept directory, going unbounded deep, ignoring the directories it is already in or above, accepting any name, entering another user's directory;
  - the parent watch accepting any parent; a zombie counted alive;
  - the sweep: a non-directory listed, a live broker's staging swept, state directories not kept, `clean` without the lock, a staging directory's contents unchecked, a held staging directory or run directory listed, a leftover removed without claiming it;
  - the run helper not watching its broker; a staging root not held while in use;
  - the broker serving without its lock, or without its sweep.

**A test machine was wiped while testing this.** A mutant of the first `remove_tree` walked out of its scratch directory through `..` and deleted the WSL2 test user's files; earlier gate logs that existed only there are lost. The rules since, and the sandbox, are in [Testing code that deletes](../testing-destructive-code.md).

**How.**

- **Removal through handles.** `hatchling_sys::remove_tree` removes a tree only through directory handles. Each directory is opened from its parent's handle with `O_NOFOLLOW`, and each entry is unlinked from the handle it was read from.
  - A symlink is removed, never followed.
  - A directory on another mount (by `statx`'s mount id, so a bind mount from the same filesystem too) is never entered.
  - A directory the caller keeps, by device and inode, stays whole with everything above it.
  - A directory swapped between being looked at and opened is left.
  - No directory the walk is already in or above is entered, whatever its name; no entry named empty, `.`, `..` or with `/` is taken; no other user's directory is entered.
- **The sweep.** The broker's sweep lists only entries named as a staging directory of a dead broker pid (a zombie counts as dead) or as a run directory. Each must be a directory itself, owned by the broker's user; a staging directory must hold only cells' staging; a live run or staging root is held by its process and never listed; each leftover is claimed exclusively for its removal; and every configured state directory is kept.
- **The serving lock.** A serving broker holds `<socket>.lock` (`flock`) for its life. The kernel lets it go however the broker ends; `clean` and a second broker refuse while it's held.
- **Helpers end with their broker.** Found while testing the first criterion: a broker killed with `SIGKILL` left its run and skill helpers, and their cells, running. Each helper now takes a pidfd on the broker that started it (passed its pid, and checked to be its parent once the pidfd is open). It exits when that process does, and its cell, armed with `PDEATHSIG` against the helper, goes too. It watches the broker process, not the thread that spawned it, so a run outlives the session thread that launched it, as before.

**The acceptance criteria, as tested:**

- **Killed mid-run, swept at the next start:** `the_next_start_sweeps_what_a_killed_broker_left_and_logs_each` (`tests/sweep.rs`).
  - The test binary runs itself as a broker whose run's real cell hangs, and is `SIGKILL`ed.
  - Its helper ends with it.
  - The startup sweep then removes the run's directory, with the cell's staging in it, and logs it.
  - Through the binary, `a_starting_broker_sweeps_leftovers_unless_one_already_serves` (`tests/smoke.rs`): a start sweeps and logs a planted run directory.
- **A live broker's staging stays:** `a_live_brokers_staging_and_anything_not_disposable_is_left_alone`. A running process's staging, and this process's own, are not listed; once that process ends, its staging is.
- **`clean --dry-run`:** `clean_dry_run_lists_exactly_what_clean_removes_and_changes_nothing`. Every path under the scratch tree is the same before and after the dry run, and `clean` then removes exactly the listed paths. Through the binary, the smoke test above.
- **Refused while serving:** `clean_and_a_second_broker_are_refused_while_one_serves`; through the binary, the smoke test refuses both a start and `clean` while the lock is held, and nothing is swept.
- **Symlinks aren't followed:**
  - `a_tree_goes_whole_and_its_symlinks_take_nothing_with_them`, and `a_symlinked_tree_or_root_is_refused_and_nothing_goes`;
  - the listing test: a symlink named as a leftover isn't one;
  - `a_state_directory_inside_a_leftover_or_behind_a_symlink_survives`: a symlink to the vault in a run's directory takes nothing with it.
- **A state directory inside a root survives:** `a_state_directory_inside_a_leftover_or_behind_a_symlink_survives` (memory configured inside a run's directory) and `a_kept_directory_stays_whole_with_everything_above_it`. Mounts: `a_mount_in_the_tree_is_never_entered` (a bind mount into the tree, and one at its top).
- **The storage guide:** [docs/storage.md](../storage.md); `the_storage_guide_lists_every_path_the_broker_is_told` fails if the broker gains a `_DIR`, `_FILE`, `_KEYRING`, `_SOCKET`, `_WORKSPACE` or `_ROOTS` setting the guide doesn't name.

**What it changes for an operator:**

- A broker no longer starts on a socket another broker serves.
- A start logs each leftover it removes.
- `hatchling-broker clean [--dry-run]` does it on demand.
- [What Hatchling keeps on disk](../storage.md) says what to back up and what is safe to delete.

## Notes

- The sweep deletes as the broker's user, which is root for a root broker, so it shares WO-006's concern: work on directory handles (`openat`, `unlinkat` with `AT_REMOVEDIR`, `O_NOFOLLOW`), never on re-resolved paths. If WO-006 lands first, reuse its helpers.
- A staging directory's name carries the pid of the broker that made it, which is what lets the sweep tell a dead broker's leftovers from a live one's. Pid reuse can make a dead broker's directory look live; the cost is only that it waits for a later sweep.
- Found while cleaning up a test machine after the launch: build output was most of the space, but the question users will ask is "what can I delete without losing anything?"
