# Evidence: doctor names a missing system user (WO-033)

What was tested, how, and what came back, for part of an open item of
[WO-033](../workorders/workorder-033-install-it-and-leave-it-on.md):
`hatchling-broker doctor` names each missing requirement and how to fix
it, and exits non-zero when one is missing. The scope listed the users.

| | |
|---|---|
| Base | `main` at `3a5c4445` |
| Tested tree | `aab32bd4` |
| Date | 2026-11-21 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no code.

## The problem

Run as root, the broker gives its stores, its vault keeper and its egress
authority a system user each (`hatchling-broker`, `hatchling-keeper`,
`hatchling-egress`), and won't start without them. `doctor`, run as root
or for the service, said *the Keeper and the Broker each run as a user of
their own* without looking for them: with one deleted, it said nothing
was wrong, and the service then failed to start.

## What changed

As root or for the service, `doctor` now looks each of the three up in
`/etc/passwd`. One that's missing, is root, or can't be looked up is a
problem, which makes `doctor` exit 1:

```
problem  users  there's no hatchling-keeper, the vault keeper's user: the broker won't start without it
                Each is a system user of its own. `sudo systemd-sysusers` makes them from
                /etc/sysusers.d/hatchling.conf, which the installer put there; or run the
                README's `useradd` line for each
```

Each wrong one is named, in one line. Run by hand, not as root, the users
aren't needed, and none is looked up, as before. The cells' user
(`hatchling-cell`) was already named: it's a setting, and settings that
name no account don't parse. `hatchling-room`, the Room's, isn't asked
after here: the README's quickstart doesn't make it, and the Room's own
line says when the Room can't start.

## Results

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.98.1 and 1.85.0 | clean |
| `hatchling-broker` lib tests `doctor::`, both toolchains | 60 passed on each |
| Mutation of the lines changed (`--in-diff`, the broker's lib tests) | 3 mutants: 2 caught, 1 unviable, 0 missed, 0 timeouts |

### Hand-breaks

Each was applied, `doctor`'s tests run, then undone:

| Break | Caught by |
|---|---|
| No user checked (the problem never returned) | `a_missing_system_user_is_named` |
| A user that's root taken as fine | the same |
| A lookup that failed taken as fine | the same |
| Checked only as root, not for the service run by its owner | the same |
| The keeper's user not asked after (the broker's asked twice) | the same |
| Checked when run by hand, where the users aren't needed | `a_hand_run_install_says_who_runs_it` |

6 broken, 6 caught.
