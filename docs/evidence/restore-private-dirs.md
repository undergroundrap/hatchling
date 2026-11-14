# Evidence: a restore leaves what it brought back readable

What was tested, how, and what came back, for a change to `restore`: it
makes a store's directories as the store makes them. Every number here
is read from a file in the results bundle, which is attached to the
release that carries this change, unless it says otherwise.

| | |
|---|---|
| Base commit | `b914a7a8` (`fix(room): answer the two calls ls -l reads a file's label with (#440)`) |
| Tested tree | `11637f996fe7143eb755aca8717f7c01b00f7d8d` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox, umask 022 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-restore-private-dirs-results.tar.gz`, 399406 bytes, sha256 `770952ce70faa420b1cb30b75a12079221d64095f211823ac4eeba8867c837ed` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

On a real install, each time the app opened, the broker logged "feed
read failed … memory store root has insecure permissions:
/var/lib/hatchling/goals/goals/985", and that read of the owner's goals
failed. That directory was mode 755. (The update drill's record
has how it was found, and two wrong turns on the way.)

A store makes its directories 700. Its reader refuses a directory of any
other mode and repairs nothing, because a store's own writer never
leaves another mode behind; a writer repairs. `restore` isn't the
store's writer. It staged each file of a backup with `create_dir_all`,
which gives a directory the mode any new one gets, 755 under the usual
umask, and moved those directories into the stores. The broker's start
then gives a store to its user, and leaves the modes of the directories
inside it: a week of starts later, they were still 755.

So after a restore, what was brought back couldn't be read until
something was written to it. On that install a backup had been made five
minutes before the time on the directories. Memory, restored the same
way, had been written to since, which repaired it. The goals hadn't.

## What changed

`restore` makes a store's directories 700, where it stages a backup's
files and where it makes a store that isn't there. Every place a backup
covers is one of the broker's own stores, whose directories are its
user's alone, so no place wants another mode.

It doesn't repair an install restored before this change. Changing any
goal in the app does, or setting the directory the broker's log names to
700. `doctor` still doesn't look at a store's directories: that is its
own change.

## Tests

Two, in the broker's `backup` tests.

- `a_restored_store_is_one_its_reader_takes`: a store with a chain of
  directories made by the store's own writer, with a file in it, is
  backed up, cleared and restored. Before and after, the store's own
  reader (`existing_chain`) takes it.
- `a_restore_makes_a_missing_store_private`: a store removed after its
  backup was taken comes back with its directories 700.

They show the fault only where a new directory isn't 700 by default,
which is so under umask 022, as here and in CI.

## Breaks by hand

Four, each one part of the change taken out or bent, the backup's tests
run on each, and each failure read (`unit-breaks.log`, `unit-*.log`).
All four were caught, and the 27 tests passed before and after.

| Break | Caught by | What it said |
|---|---|---|
| The mode a new directory gets (755) | both tests | 493 where 448 was expected: 755 for 700 |
| A mode tighter than a store's (500) | both, and `a_backup_brings_back_what_it_took` | "Permission denied" as the backup was staged: no file can be made in such a directory, so these fail before they look at a mode |
| Staged directories made as before | both tests | 493 for 448 |
| A missing store made as before | `a_restore_makes_a_missing_store_private` | 493 for 448 |

## Mutation

`cargo mutants --in-place --in-diff` on the change to the broker's
source, the broker's library tests on each (`mut.log`,
`mut-broker-outcomes.json`, sha256
`6a8aac267bd50becc95cd5bbb53127c56e75bc0bfddb9b3a8467cf9ce8b9fa35`):

| Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|
| 4 | 4 | 0 | 0 | 0 |

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3806 passed, 0 failed | exit 0: 135 binaries, 3806 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

## As root

The backup's tests were also run as root, outside the jail (`root/`).
The two new tests passed. Two others failed, with the change and without
it, the same way each time:

- `a_clear_takes_the_cells_leftovers_and_nothing_else`: "wasn't removed:
  it isn't Hatchling's";
- `a_restores_keyring_never_goes_through_a_planted_link`:
  `InsecureKeyFile`.

Neither is this change's, and why they fail as root in this sandbox
wasn't looked into. CI's own run as root is the one that counts for
them.

## What wasn't tried

- `hatchling-broker restore` itself, as root on an install: the tests
  call the same staging and moving it does, with this process's user.
  The install the fault was found on wasn't restored again.
- A umask other than 022.
