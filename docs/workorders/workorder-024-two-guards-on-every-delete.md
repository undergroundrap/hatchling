# WO-024: Two guards on every delete and signal

**Status:** DONE
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
  - five of the six cleanups (`Staging::drop`, the run directory's two, `RunDir::drop`, `VerifiedCode::drop`) go through `hatchling_sys::remove_dir_tree`, a path's form of `remove_tree` (no symlink followed, no mount entered, the owner checked). Each holds, or may hold, a cell's staging, whose mounts are the threat;
  - `harness.rs` `RunDirs::drop` stays on `remove_dir_all`: its work and scratch trees are handed to the cell's uid, which `remove_tree` (rightly, for a root broker) never enters, so the switch would leave them behind; it holds no mount, and it's a random, exclusively created 0700 directory;
  - the driver kills the group before it reaps the leader, while the group id can't be reused.
- A test for each guard that fails when only that guard is broken, proved by a hand mutant per guard.
- `docs/testing-destructive-code.md` and the release checklist updated where they describe these sites.

**Not in scope:**

- Any change to what is deleted, or when; only how many checks stand before it.
- Sites the 2026-10-17 review found safe (two independent guards already, or a non-recursive delete of a name this process just created).
- New cleanup features.

## Acceptance criteria

Each guard is checked on its own (its own `if`, its own function), so breaking one leaves the other standing; and each has a direct test that fails when only it is broken.

- [x] A rekey keeps the KEK file at the file tier: by its tier (`off_the_file_tier`) and by its wraps (`opens_with_the_kek_file`), each tested directly.
- [x] `bind_socket` refuses, and leaves in place, a regular file, a symlink or another user's socket at the socket path: a socket by its type and by its raw mode, and the owner, each tested directly.
- [x] `mint_ui_token` refuses, and changes nothing, when `<socket>.token` is a symlink, a directory or another user's file: a regular file by its type and by its raw mode, and the owner.
- [x] A recall or a purge deletes an expired item only when `past_its_ttl`, which reads the item's own TTL through other code, agrees with `is_expired`; each tested directly.
- [x] The five cleanups remove their directories through `remove_dir_tree`: its tests (with `remove_tree`'s) plant a bind mount and symlinks to outside data and find them untouched, and each cleanup's own test shows the bounded walk ran (a tree deeper than `MAX_DEPTH` is removed only partly, where `remove_dir_all` would take it all). `RunDirs::drop`'s exception is recorded here.
- [x] On a timeout, the driver kills the test's process group before it reaps the test: a test whose grandchild outlives it finds the grandchild killed, and the group kill ordered before the reap.
- [x] Every new guard has a hand mutant that fails its test, listed in the evidence.

## Gates

All must pass before the WO closes:

- [x] `bash scripts/check.sh` green on the pinned toolchain **and** `RUSTUP_TOOLCHAIN=1.85.0`.
- [x] Real-cell tests ran rather than skipped (`HATCHLING_REQUIRE_CELL=1`, Linux 5.11+ or WSL2).
- [x] **Run in a sandbox:** every test, hand mutant and mutation run of this WO runs in a disposable sandbox where only its work directory is writable ([Testing code that deletes](../testing-destructive-code.md)), with canary files checked throughout.
- [x] **Mutation, kill floor 1.0:** `scripts/mutant-run.sh --base <base commit> -- <each changed .rs file>`; every surviving mutant killed by a new test or proven equivalent in writing.
- [x] Docs that describe the changed behavior updated in the same change.

## Evidence

One pull request, gated in the disposable WSL2 sandbox (bubblewrap, read-only `/`, only the work directory writable; Ubuntu 26.04, Rust 1.98.1 and 1.85.0) with `HATCHLING_REQUIRE_CELL=1`, base `22a5fd05`, canary files intact after every step and throughout mutation:

- plain clippy 0 sites on both toolchains; `hatchling-dev preflight` passed; fmt clean;
- `check.sh` green on both: 2,357 tests each, 0 failed (`cargo deny` left to CI: the sandbox has no network, and `Cargo.lock` is unchanged);
- 15 hand mutants, each failing its test:
  - the socket path: the file-type check, the raw-mode check, the owner check;
  - the token file: the same three;
  - the KEK file: the tier check, the wraps check;
  - memory: `past_its_ttl`;
  - the cleanups: `Staging::drop`, `VerifiedCode::drop`, `RunDir::drop` and the run directory's back on `remove_dir_all`;
  - process groups: signalling after the leader is reaped, and reaping before the group is killed;
- mutation on the changed lines of all 12 changed files: 74 mutants, 63 caught, 11 unviable, 0 missed, 0 timeouts, 0 orphans.

The gate also caught a real interaction: an existing test signalled a group after reaping its leader, which the new guard refuses. Its cleanup now kills the orphan by its own pid, and it checks the refusal.

## Notes

The review behind this WO read every production delete and signal (2026-10-17) and judged each by the worst one mutation could do there; the rule it applies is [Testing code that deletes](../testing-destructive-code.md#1-two-independent-guards-per-escape).
