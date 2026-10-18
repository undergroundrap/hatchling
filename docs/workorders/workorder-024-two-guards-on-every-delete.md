# WO-024: Two guards on every delete and signal

**Status:** IN-PROGRESS
**Area:** `crates/hatchling-broker` (`vault_keyring.rs`, `listener.rs`, `api_auth.rs`, `run_launcher.rs`, `skill_run.rs`, `skill_cell.rs`, `harness.rs`), `crates/hatchling-memory` (`store.rs`), `crates/hatchling-isolation` (`linux/spawn.rs`), `crates/hatchling-sys` (`process.rs`), `crates/hatchling-dev` (`checks/driver.rs`)
**Issue:** [#111](https://github.com/undergroundrap/hatchling/issues/111)
**Trust boundary:** yes

## Problem

During WO-012's mutation run, a mutant of the new tree removal flipped `&&` to `||` in its `.`/`..` filter and deleted nearly everything the test user owned on the test machine ([Testing code that deletes](../testing-destructive-code.md)). Mutation testing makes exactly that kind of change on purpose, so any code that deletes or signals behind a single check is one changed operator away from doing damage, in a test run or, after a real bug, in production.

A review of every production delete and signal after that (2026-10-17) found no other recursive delete or signal that can reach outside Hatchling's own data with one mutation. It did find these, each one mutation from deleting something it shouldn't, or relying on luck rather than a check:

- **The vault's key file.** `vault_keyring.rs` `finish()` deletes the KEK file after a rekey when the keyring isn't at the file tier. That `!=` is the only check: flipped, a rekey at the file tier deletes the key the vault was just re-wrapped under, and the vault then opens only with backup codes.
- **The broker's socket.** `listener.rs` `bind_socket()` unlinks what's at the socket path when it's a socket. Inverted, it unlinks whatever single entry is there.
- **The UI and operator token files.** `api_auth.rs` `mint_ui_token()` removes `<socket>.token` with no check at all on what it is or who owns it.
- **Memory items.** `hatchling-memory`'s `purge_expired()`, `recall()` and `recall_by_tag()` delete an item when `is_expired` says so, and nothing else. With `is_expired` returning `true`, a recall deletes every item in the caller's scope, and a purge every item under the root.
- **Six cleanups** use plain `std::fs::remove_dir_all`: `Staging::drop` (`hatchling-isolation`), `run_launcher.rs` (the launch cleanup and `teardown`), `skill_run.rs` `RunDir::drop`, `skill_cell.rs` `VerifiedCode::drop` and `harness.rs` `RunDirs::drop`. Each path is one the broker made exclusively under a random name, so today they're safe; but `remove_dir_all` crosses mounts and checks no owner, and only the cell's mount namespace keeps its mounts out of the broker's view.
- **A process group after its leader is gone.** `hatchling-dev`'s driver (`checks/driver.rs`) kills a timed-out test's process group *after* reaping the test. Once every member has exited, the kernel may reuse that group id, and the kill reaches an unrelated group.

## Scope

**In scope:**

- Each site above gets a second guard, independent of the first (no shared code, so one mutation can't remove both):
  - the KEK file is deleted only when the keyring as read back from disk isn't at the file tier, as well as the in-memory check;
  - the socket path is unlinked only when it's a socket owned by the broker's user;
  - a token file is replaced only when it's a regular file owned by the broker's user (anything else refuses the start, naming the path);
  - a memory item is deleted only when its own `expires_at` is set and past, checked where the delete is made, as well as `is_expired`;
  - the six cleanups go through `hatchling_sys::remove_tree` (no symlink followed, no mount entered, the owner checked);
  - the driver kills the group before it reaps the leader, while the group id can't be reused.
- A test for each guard that fails when only that guard is broken, proved by a hand mutant per guard.
- `docs/testing-destructive-code.md` and the release checklist updated where they describe these sites.

**Not in scope:**

- Any change to what is deleted, or when; only how many checks stand before it.
- Sites the 2026-10-17 review found safe (two independent guards already, or a non-recursive delete of a name this process just created).
- New cleanup features.

## Acceptance criteria

- [ ] A rekey at the file tier keeps the KEK file, with the in-memory tier check broken, and with the on-disk check broken.
- [ ] `bind_socket` leaves a regular file, a symlink and another user's socket at the socket path in place and refuses, with either check broken.
- [ ] `mint_ui_token` refuses, and changes nothing, when `<socket>.token` is a symlink, a directory or another user's file.
- [ ] A recall or a purge deletes no item that hasn't expired, with `is_expired` broken, and with the `expires_at` check broken.
- [ ] Each of the six cleanups removes its directory through `remove_tree`; a test plants a bind mount and a symlink to outside data in one and finds both untouched after cleanup.
- [ ] On a timeout, the driver kills the test's process group before it reaps the test: a test whose grandchild outlives it finds the grandchild killed, and the group kill ordered before the reap.
- [ ] Every new guard has a hand mutant that fails its test, listed in the evidence.

## Gates

All must pass before the WO closes:

- [ ] `bash scripts/check.sh` green on the pinned toolchain **and** `RUSTUP_TOOLCHAIN=1.85.0`.
- [ ] Real-cell tests ran rather than skipped (`HATCHLING_REQUIRE_CELL=1`, Linux 5.11+ or WSL2).
- [ ] **Run in a sandbox:** every test, hand mutant and mutation run of this WO runs in a disposable sandbox where only its work directory is writable ([Testing code that deletes](../testing-destructive-code.md)), with canary files checked throughout.
- [ ] **Mutation, kill floor 1.0:** `scripts/mutant-run.sh --base <base commit> -- <each changed .rs file>`; every surviving mutant killed by a new test or proven equivalent in writing.
- [ ] Docs that describe the changed behavior updated in the same change.

## Evidence

Filled in at close.

## Notes

The review behind this WO read every production delete and signal (2026-10-17) and judged each by the worst one mutation could do there; the rule it applies is [Testing code that deletes](../testing-destructive-code.md#1-two-independent-guards-per-escape).
