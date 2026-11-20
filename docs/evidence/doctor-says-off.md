# Evidence: `doctor` says when a protection is off or weaker (WO-037 slice 1a)

What was tested, how, and what came back, for WO-037's first slice: `doctor`
saying in plain words each protection the work order's inventory found off or
weaker without anything telling the owner.

| | |
|---|---|
| Base commit | `43fdf76a` (`feat(approvals): a request with a warning asks every time; close WO-036 (#548)`) |
| Tested tree | `c204cbde` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

An owner who reads `doctor` or Your security should be able to trust that
anything missing is named there. The work order's inventory (in the private
design notes, checked against the code) found protections that can be off or
weaker without either saying so. This slice is `doctor`'s words for those it
can read from the settings and the kernel. The probes (the syscall filter, the
Room's image, the Windows seal) are slice 1b. The page and the doc are slice 2.

## What changed

| Protection | Off or weaker when | `doctor` now says |
|---|---|---|
| The broker's own file access (`confine.rs`, `confine_broker`) | the kernel has no Landlock: the broker only logged it | a note: the broker's own file access isn't confined, and a compromised broker reaches every file its user can |
| The Keeper and the Broker each as their own user (`keeper.rs`, `service_user`; `broker_user.rs`) | run by hand, not as root or as the service: both run as the owner | a note: they run as your own user, kept apart from neither you nor each other; install it as a service, or run it as root |
| The output gate (`output_gate.rs`) | `HATCHLING_OUTPUT_GATE=off`, or `HATCHLING_REDACTION_BUILTINS` turning a built-in off | a note naming what isn't checked: your secrets, or private keys |
| The agent's browser (`spawn.rs`, `bounded_as_a_whole`) | `HATCHLING_BROWSER` set where cells have no cgroup: it can't start | a problem, with how to give cells a cgroup |
| Cells held by process limits alone | no cgroup | the note now also says the agent's computer (the Room) and its browser can't run |
| The vault (`check_vault`, `vault_said`) | its key file alone, a keyring at the file tier, or authenticator codes: anyone with the disk can open it | each a note saying so, with the security key that can't be opened without it (each was "fine") |
| The agent's uid (`config.rs`, `refuse_shared_agent_uid`) | `HATCHLING_AGENT_ALLOW_NOBODY` accepts nobody: any process running as nobody could act as the agent | a note, with a uid of its own as the fix |
| The owner's decisions (`api.rs`; `approval_board.rs`) | the scheduler is off: decisions aren't kept, and the approvals history is empty after a restart | a note, with `HATCHLING_SCHEDULER_DIR` as the fix |

Each is a pure function of what `doctor` read, so a test can make each
protection off and read the words.

## Results

| Check | Result |
|---|---|
| `doctor` tests | 43 passed, on 1.98.1 and on 1.85.0 |
| Hand-breaks | 12 broken, 12 caught, each by the test named for it |
| Targeted mutation (`--in-diff`) | 16 mutants: 7 caught, 0 missed, 0 timeouts, 9 unviable |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,265 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### Hand-breaks

| Break | Caught by |
|---|---|
| No Landlock said fine | `a_broker_without_landlock_is_said` |
| Run by hand said as its own users | `a_hand_run_install_says_who_runs_it` |
| The gate off, unsaid | `an_output_gate_turned_off_is_said` |
| A built-in check off, unsaid | the same |
| A browser without a cgroup said fine | `a_browser_without_a_cgroup_is_said` |
| Process limits not saying what they turn off | the same |
| Nobody as the agent, unsaid | `nobody_as_the_agent_is_said` |
| Decisions said kept with the scheduler off | `decisions_not_kept_are_said` |
| A blank setting said set | the same |
| A file-tier keyring, a stolen disk unsaid | `a_vault_a_stolen_disk_opens_is_said` |
| Authenticator codes, a stolen disk unsaid | the same |
| Its key file alone, a stolen disk unsaid | the same |

**Found by these runs:** the first mutation run missed two mutants. Both were
in `doctor_main`, which the library's tests never run: whether the browser's
setting is blank, and whether `doctor` runs as root. Both moved into functions
the tests reach (`is_set`, and `users_said` taking the uid), now tested.

## Residual, for the next slices

- **Probes** (slice 1b): the cells' syscall filter isn't installed by
  `doctor`'s probe, so a kernel without seccomp filtering reads as fine; the
  Room's image, its architecture and its cgroup aren't checked; the Windows
  distro's seal from the Windows drives isn't checked again after install.
- **The page and the doc** (slice 2), among them Your security with the setup
  unread, and the doc's claims qualified where an install can lack them.
- **The Room offered before its image is checked** is a change of behaviour,
  so it has its own issue (#549).
