# Evidence: `doctor` names each store directory its store would refuse (WO-038 slice 2)

What was tested, how, and what came back, for WO-038's second slice: a
`doctor` check of the stores' directories.

| | |
|---|---|
| Base commit | `33e89676` (`feat(broker): --version names the release, the commit and how it was built (#553)`) |
| Tested tree | `5f9e15b4` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

A store keeps the owner's data in directories that are its user's alone. A
directory open to others would let another account on the machine read or
change what the store keeps, so each store refuses one: a directory of any
mode but 700, a link where a directory belongs, or another user's. The store
repairs nothing as it reads, and the refusal comes from whatever reads first.

An update on a real install (2026-11-14) met one: a goals store's directory
made by an older version at 755. The feed failed each time the app opened,
saying only that a store had insecure permissions, and `doctor` saw nothing.

## What changed

- **`doctor` has a `stores` line.** It checks the directories whose readers
  refuse, never repair:
  - every directory of the memory and goals stores, from the root down, as
    their readers walk down to each user's records;
  - the root of each store the vault crate opens: the vault, each sealed
    store, the key file's directory and the keyring's.

  Directories a store makes private again itself aren't checked: nothing
  refuses them.
- **What's wrong with one:**
  - a link, or anything else, where a directory belongs;
  - any mode but 700;
  - run by hand, another user's.

  Run as root (as `sudo hatchling-broker doctor` and the installer run it),
  owners aren't checked: the service's start hands each of these stores to
  its user, so an owner left by an older version isn't a problem.
- **What it says:** each wrong directory and what's wrong with it, five
  named and the rest counted, with the commands that put each right: a link
  moved aside, `chown` before `chmod`, with `sudo` when it runs as root.
- **How:** nothing is followed through a link, nothing is changed, a store
  not made yet is fine, and the walk stops at 8 levels or 10,000
  directories, and says when it did.
- **`docs/updating.md`** says what the line means when an update stops on
  it.

## Results

| Check | Result |
|---|---|
| `doctor`'s tests, the new ones among them | 56 passed, on 1.98.1 and on 1.85.0 |
| The broker's smoke tests | 14 passed, on both |
| `doctor` itself, on stores made each way (below) | each said what it should |
| Hand-breaks | 14 broken, 14 caught, each by the test named for it |
| Targeted mutation (`--in-diff`) | 42 mutants, 37 caught, 0 missed, 0 timeouts, 5 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,289 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### `doctor` on real stores

The built `hatchling-broker doctor --env <file>`, run by hand, with a vault,
a memory store and a goals store in a fresh directory:

| The stores | The `stores` line said |
|---|---|
| None made yet | `ok`, none made yet: the broker makes each as it should be |
| Each directory made 700 | `ok`, 7 directories checked: each its owner's alone, mode 700 |
| A user's goals directory at 755 (the update's), the vault's root at 750, a link where a user's memory directory belongs | `PROBLEM`, naming all three and what's wrong with each; the fix moves the link aside, then `chmod 700` the two |
| Put right | `ok`, 7 directories checked |

### Hand-breaks

| Break | Caught by |
|---|---|
| A mode closed to others but not 700 (500, 600) taken as right | `a_store_directory_is_a_directory_its_owners_alone` |
| Another user's directory taken as right | the same |
| A link or file where a directory belongs taken as right | the same, and `each_directory_in_a_store_is_checked_and_no_link_followed` |
| Run by hand, the owner left unchecked | `the_owner_is_checked_only_run_by_hand` |
| Run as root, the owner checked against root | the same |
| A link followed, its target checked instead | `each_directory_in_a_store_is_checked_and_no_link_followed` |
| Only a store's root checked (the update's directory missed) | the same, three more of the store tests, and `doctor`'s `the_stores_are_checked_as_their_readers_take_them` |
| A link beneath a store not looked at | `each_directory_in_a_store_is_checked_and_no_link_followed` |
| The walk without its bound | `a_walk_past_its_depth_says_it_stopped` |
| The fix, run as root, without `sudo` | `each_wrong_directory_is_named_with_its_fix` |
| The owner's fix left out | the same |
| The wrong ones past five not counted | `many_are_counted_and_a_clean_check_says_what_it_saw` |
| The keyring's directory not checked | `the_stores_checked_are_the_ones_whose_readers_refuse` |
| The goals store not walked | the same, and `doctor`'s test |

**Found by these runs:**
- **A link to a file.** The first mutation run, on the same change over
  `f47f88f1` (tree `9731f2d7`), missed one mutant: with `&&` as `||`, the
  walk took a link to a file for a directory, and said it was wrong. No test
  had one. The walk test now has a link to a file in a store, which isn't a
  directory problem.
- **A wrong expectation.** The first hand-break run listed `doctor`'s test
  as one that must catch "run by hand, the owner left unchecked". It can't:
  it runs as the directories' owner. The test named for the rule did catch
  it, and the expectation was corrected.

**Unviable mutants:** the five replace a value with `Default::default()`,
which `Finding`, `StoreDir` and `Wrong` don't have.

## Residual

- **Run as root, nothing was run.** The tests run as an unprivileged user.
  Root's one difference, owners unchecked and `sudo` in the fix, is a
  tested function of the user that runs it (`owner`, and `said`'s `as_root`).
- **Only the stores whose readers refuse are checked.** A directory a store
  makes private again itself is left to it.
- **Owners, run as root, are left to the service's start.** A store whose
  start can't hand it over (a directory its user can't reach) fails that
  start, saying why, as before.
- **Slice 3** (the Room's image on Linux) is still to come.
