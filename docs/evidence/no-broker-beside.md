# Evidence: no backup, restore, reset or lock beside a broker still serving (#508)

What was tested, how, and what came back, for refusing the vault's whole-store
commands while a broker serves beside them.

| | |
|---|---|
| Base commit | `d28093ac` (`build(release): build the Room's supervisor from the image's own snapshot (#538)`) |
| Tested tree | `65a12c75` |
| Date | 2026-11-19 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

Not an attacker: the owner's own recovery path, and the word `lock` gives.

#508 as filed: a backup copies the Library's store while a keep writes it, so a
restore brings back chunks nothing names. Its review found that `backup`,
`restore` and `reset` already stop the service before they touch a store, so on
an install the race can't happen: what is on disk is a crash's state, which the
Library's recovery already finishes or undoes.

It happens beside a broker run by hand (`sudo hatchling-broker`, the README's
quickstart), where there is no service to stop. There it is wider than filed:

- **A backup** copies every store while the broker writes it. The Library, the
  conversation, schedules, sign-in profiles and credentials each keep one thing
  across several entries, so a restore could bring back an item whose parts are
  missing (its index named, its chunks not yet listed), parts nothing names, or a
  temporary file read half written. A part removed between the listing and the
  read failed the whole backup.
- **A restore or a reset** replaced or cleared stores under the running broker.
- **`lock`** said the vault was locked while that broker still held its key in
  memory.

## What changed

- `backup`, `restore` and `reset`, once the service is stopped, take the serving
  lock the broker holds for its life (`broker.sock.lock`, WO-012), and hold it
  until the stores are copied, cleared or swapped. A broker still serving is
  refused, in words, before anything changes, and the service is started again as
  it was. None can start meanwhile. With no directory for the lock, no broker has
  ever served there.
- `lock` checks the same, after the service stops and before the room disk is
  closed: a broker still serving holds the key, so `lock` refuses ("Nothing was
  locked") and starts the service again.
- `docs/backup-and-reset.md` and the changelog say so.

The fix #508 proposed, the backup holding the Library's in-process lock, can't
work: the backup is another process. A cross-process lock on the Library's
folder alone would have left the other stores, restore, reset and lock as they
were.

## Results

| Check | Result |
|---|---|
| The new tests, and `backup`, `sweep`, `cli` | 80 passed on 1.98.1 and on 1.85.0 |
| Hand-breaks | 9 broken, 9 caught, each by the test named for it (below) |
| Targeted mutation (`--in-diff`, the broker's own tests) | 10 mutants: 8 caught, 0 missed, 0 timeouts, 2 unviable (`hold_serving` and `stopped_alone` returning a `Default` that `HeldLock` and `T` have none of) |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | 0 on 1.85.0 and on 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,215 passed, 0 failed, 0 ignored, on each toolchain |
| `seed-check`, `protocol-schema`, `audit` | 0 on each toolchain; "all checks passed" |

### Hand-breaks

| Break | Caught by |
|---|---|
| The serving lock never taken | `nothing_is_changed_beside_a_broker_still_serving`, `a_lock_beside_a_broker_still_serving_is_refused` |
| Taken to check, then let go | `nothing_is_changed_beside_a_broker_still_serving` |
| A busy lock not said in the owner's words | both |
| A lock that can't be taken passed as no broker | both |
| A missing directory tried, not skipped | `no_lock_is_held_where_no_broker_ever_served` |
| Every directory taken as never served | both |
| Refused, the service left stopped | `nothing_is_changed_beside_a_broker_still_serving` |
| `lock` going on beside a serving broker | `a_lock_beside_a_broker_still_serving_is_refused` |
| `lock` refusing, the service left stopped | `a_lock_beside_a_broker_still_serving_is_refused` |

The first mutation run missed one mutant, `run_backup` replaced with `Ok(())`:
no test ran `backup` as anyone but root, as the root-only test did for the other
three. It now does, and the mutant is caught.

## Residual

- **The commands themselves run only as root.** As an unprivileged user each is
  refused at its start, so the tests drive the guard (`hold_serving`,
  `stopped_alone`, `lock_with`), not the commands' whole run. The call sites are
  read, not run, here.
- **A broker on another socket** (its own `HATCHLING_SOCKET`) holds another lock,
  which this can't see.
- **A temporary file a killed broker left** may still be copied, and restored as
  a file nothing reads. Not checked here.
