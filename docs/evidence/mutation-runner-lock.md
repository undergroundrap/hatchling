# Evidence: the mutation runner's lock stays its own

What was tested, how, and what came back, for #415: a program a mutation
run leaves behind held `scripts/mutant-run.sh`'s lock, so every later run
refused to start. Every number here is read from a file in the results
bundle, which is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `950dbb9f` (`fix(approvals): run only what the owner was shown (#474)`) |
| Tested tree | `4db520262a49651243592cc7ddef797fc98a20cc` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |
| Results bundle | `hatchling-mutation-lock-results.tar.gz`, 8798 bytes, sha256 `c52ce2f0bf2ce6645f2f1910e0a20d721f7783160c7a5e1f81fd2aadd129966d` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What changed

The runner takes its lock on descriptor 9, and its two background
children, the mutation command and its watchdog, inherited it. Either
child's descendants could keep the lock open after the runner had gone.
Both are now started with descriptor 9 closed (`9>&-`).

## Runs (`run.log`)

- **The new test,** `a_program_a_run_leaves_behind_doesnt_hold_its_lock`
  (`crates/hatchling-dev/tests/mutation_scripts.rs`): its mutation command
  leaves a `sleep` running, as a timed-out test's child can, and a second
  run follows. `cargo test -p hatchling-dev --test mutation_scripts --
  a_program_a_run_leaves_behind_doesnt_hold_its_lock`: 1 passed.
- **The fix taken out** (both `9>&-` removed, `broken.log`): the test
  fails, and says why: "a leftover held the lock: mutant-run: another run
  holds .../run.lock; not starting a second", the refusal #415 reported.
- `cargo test -p hatchling-dev` on 1.85.0 and 1.98.1: 222 passed, 0 failed
  on each. `cargo clippy -p hatchling-dev --all-targets --all-features --
  -D warnings`: exit 0 on each. `cargo fmt --check`: exit 0.

## What wasn't tried

- **The full gate:** the change is one script and its test; CI runs it.
- **Mutation:** there's no Rust code to mutate; the script's one change
  was broken by hand above.
