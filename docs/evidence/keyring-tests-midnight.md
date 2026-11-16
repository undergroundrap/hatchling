# Evidence: the security-key tests no longer fail across midnight

What was tested, how, and what came back, for a change to three tests in
`crates/hatchling-broker/src/vault_keyring.rs`. No code outside the tests
changes. Every number here is read from a file in the results bundle,
which is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `744ce543` (`feat(ideas): keep an unfinished chat for later (#469)`) |
| Tested tree | `0ef44cfe4e907fc68a7ad2e1db4b4c5e4a256de4` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned) |
| Results bundle | `hatchling-keyring-midnight-results.tar.gz`, 7924 bytes, sha256 `982162d0ce54099a3bd6a234994bd716538463c87e352d43c582beb83ed07bbf` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

A full gate whose tests ran across 00:00 UTC failed two tests this code
never touches the subject of:
`status_shows_every_way_in_and_each_key_and_never_a_code` and
`the_command_runs_the_key_actions_through_its_devices` (seen in
`docs/evidence/keep-for-later.md`, its first gate). Each stamps a security
key with the time it is added, then builds the line it expects with
`today()`, a second read of the clock. When midnight falls between the
two, the key says one day and the expected line the next.

A third test, `adding_a_key_names_it_confirms_authorizes_and_then_enrolls_it`,
had the same shape.

## What changed

- `today()` is gone. A line about a key is checked against the day the
  key's own record keeps (`day_added`), not the clock read again.
- The test that adds a key checks the stamp is the time it was added: no
  earlier than a clock read just before the add and no later than one
  just after. That holds across midnight, and is a closer check than the
  day it replaced.

## Runs (`run.log`)

| | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo clippy -p hatchling-broker --all-targets --all-features -- -D warnings` | exit 0 | exit 0 |
| `cargo test -p hatchling-broker --lib -- vault_keyring::` | 66 passed, 0 failed | 66 passed, 0 failed |

**Midnight, made to happen** (on 1.98.1): every key stamped a day before
the clock the tests read (`added: Some(now() - 86_400)` in
`hatchling-vault`'s keyring, for the run only).

- The tests as they were on the base: 3 failed, the two seen in the gate
  among them (`old-across-midnight.log`).
- The tests as changed: 1 failed, the one that checks a new key is
  stamped with the current time, which a key stamped a day ago isn't.
  Every check of the day a key shows passed (`new-across-midnight.log`).

## Found on the way

- **An experiment that tested the wrong thing.** The first attempt moved
  the broker module's clock a day on. That also moved its one-time codes
  and the add test's own bracket, but not the stamp, which the vault crate
  makes, so nine unrelated tests failed and proved nothing. Its logs are
  in the bundle (`old-past-midnight.log`, `new-clock-moved.log`, from tree
  `4b44616f…`, which also had a redundant clone clippy refused on 1.85.0).
  The experiment above stamps the key itself a day back instead.

## What wasn't tried

- **The full gate.** Only the broker's clippy and its keyring tests ran
  here; the change touches nothing else. CI runs the full gate.
- **Mutation.** The change is to tests alone: there is no code to mutate.
