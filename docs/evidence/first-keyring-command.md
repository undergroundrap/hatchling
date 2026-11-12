# Evidence: the first keyring command on a fresh install

What was tested, how, and what came back, for the fix that stops a first
`vault-keyring setup` from taking Hatchling down, makes `vault-keyring
status` answer on an install with no keyring, and starts Hatchling again
when a setup is interrupted. Every number here is read from a file in the
results bundle, which is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `ac1bf238` (`fix(schedule): refuse a reminder for a time that has passed, before it is asked (#420)`) |
| Tested tree | `73bc4c33327d8addc351a1d8390b4b3afb236887` |
| Date | 2026-11-12 (UTC) |
| Machines | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2): a sandbox for the gate, and a separate throwaway install for the drill |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-first-keyring-command-results.tar.gz`, 2922006 bytes, sha256 `f7bcf0a7400c9bac2791e72c07954be662fd1fab56a2459df9352e7b37e5bfcc` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

`vault-keyring setup` is the service's own setup: it adds a keyring to the
service's settings and restarts the service. It did the first of those
before it had taken the service's settings on, and it took them on only
from a shell that held none of Hatchling's settings. Run from a shell that
held any, it changed the service's settings file, stopped the service, and
then ran with the shell's settings. With no keyring among them it stopped
with a usage error and the whole manual, and started the service again on
settings that named a keyring that was never made. The keeper then failed
at every start.

A first-hour test on a machine whose `sudo` could not elevate hit exactly
this, working as root from a shell that held the service's settings. The
drill below reproduces its receipts on the code before this change: exit
64, "set HATCHLING_VAULT_KEYRING to the keyring's absolute path", about
16 KB of manual, the keyring line written, and the journal's `keyring …
I/O failed: No such file or directory` at each restart.

Two more things came out of the same test and the drill:

- `vault-keyring status` and `init`, as the first keyring command on a
  fresh install, failed with the same usage error and manual. An install
  with no keyring is a state, not a mistake.
- Ctrl-C while the setup waited for a key left Hatchling stopped. The
  setup had stopped the service and was gone before it could start it.

## What changed

- The guided setup always runs with exactly the service's settings,
  whatever the shell holds, and with what a first setup adds to them.
- Nothing is changed before the setup itself starts. It then stops the
  service, adds the keyring to the settings, and runs. If it stops before
  a keyring file is made, the settings are put back as they were. The
  service is started again whatever happened.
- While the service is stopped the setup holds a lock, and a unit of the
  system's waits on that lock to start the service. The lock is let go
  when the setup's process ends, however it ends. Only one setup runs at
  a time.
- `status` with no keyring says so and names the setup. Commands that
  need a keyring name the setup too. `doctor` says when the settings name
  a keyring that was never made.
- A usage error is followed by one line that points at `--help`.

## The drill, on a real install

A throwaway machine with systemd, installed by the documented own-build
route (`scripts/install.sh --from <dir>`, run with `sudo` by the user who
owns Hatchling) against a stand-in model. Each scenario starts from a
copy of the fresh install's settings and state, runs one command with no
security key present, waits two restart periods, and records the
command's exit and words, the service's state and restart count, the
settings file's hash, and the keeper's failures in the journal.

The commands were run three ways: with `sudo` as the owner; as root with
no `sudo` (the owner named by the `SUDO_` variables); and as root from a
shell that had taken on the service's settings from its file.

The drill ran on tree `1904293753776844dc6e38d7645c7087ad1a10d7`, which
is this change on the commit before the base. The tested tree differs
from it by the base commit, which touches none of this code, and by two
small helpers lifted out of the setup's terminal wiring for the mutation
run. `drill/console-before.log` and `drill/console-after.log` are the
record; sha256 `d763e06446c6fa3bd2a2b2700a6a80bb17487dd5031db2c4ddc24c846cd0778d`
and `7975b48bdf7a1d9af98651b2c71227bde5840a5c55cf153dda0cdb8bc6f52136`.

| Scenario, from a fresh install | Before this change | With it |
|---|---|---|
| `status`, with sudo and as root | exit 64, 143 lines (15973 bytes) | exit 0, 3 lines: no keyring, and the command that protects the vault |
| `status`, from a shell with the service's settings | not run | exit 0 |
| `init`, with sudo and as root | exit 64, 143 lines | exit 64, 3 lines that name the setup; nothing changed |
| `setup`, from a shell with the service's settings | exit 64, 142 lines; settings changed; the service restarting, 6 keeper failures in 13 seconds | the guide runs; with no key it stops after its wait; the service is running, 0 keeper failures |
| `setup`, from a shell with one setting of Hatchling's | exit 1; settings changed; the service left stopped | as above |
| `setup`, with sudo, and as root from a clean shell | not run in this record; in an earlier run on the same build, as root from a clean shell, it reached the guide and left the service running | as above |
| `setup` that fails after the service is stopped and before the keyring is made (its wrapping key's place taken by a directory) | not run | exit 1 with the reason; the settings file's hash is as before; the service is running, 0 keeper failures |
| `setup` sent an interrupt while it waits for a key | not run; this build, before the guard was added, left the service stopped | the service is running; the guard unit is gone |
| `setup` killed outright while it waits for a key | not run | the service is running; the guard unit is gone |

A setup that reaches the guide has by then made the keyring at the file
tier, so its settings keep the keyring whether or not a key arrives. That
is the first step of the setup, not a leftover: the vault still opens by
itself, and running the setup again carries on.

Not drilled: a real security key (the guide's own steps are covered by
its unit tests, with software keys), and the short moment between the
settings being written and the keyring file being made. A setup killed in
that moment leaves settings that name a keyring not yet made; `doctor`
and `status` now say so, and running the setup again finishes it.

## Full gate

Run as an unprivileged user with cgroup delegation,
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, once per
toolchain from a clean build:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked --no-fail-fast
cargo run -q -p hatchling-dev --locked -- seed-check
cargo run -q -p hatchling-dev --locked -- protocol-schema
```

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3697 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3697 | 0 | 0 |

`gate.log` sha256
`42195cc012f2091c3ae93c5cafc3ce441e1d6f7b7829d16d8f84d922edfae07c`.

The broker's library tests were then run 15 times over on 1.98.1, every
test in parallel: 15 runs, 1351 passed and 0 failed in each. `stress.log`
sha256 `ebff18d5fa932554efbc528af947dd1f370ef37388c73098c8c557463f95891c`.

The app is not touched by this change, and its checks were not run.

## Mutation

Every mutant on the lines changed against the base, on 1.98.1 with debug
info off:

```text
cargo mutants --in-place --in-diff <hatchling-broker's diff> --timeout 60 \
    --exclude-re run_keyring -- --lib
cargo mutants --in-place --in-diff <hatchling-broker's diff> --timeout 300 \
    --re run_keyring -- --test smoke
```

The keyring command's own entry (`run_keyring`) is reached only by
running the program, which the smoke tests do, so its one mutant has a
run of its own against them.

| Run | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-broker`, library tests | 75 | 62 | 0 | 0 | 13 | `3cbe29af9e5c19f259f584b66fa7eb09c04e09f6912a23002ae6b36c1a950ea0` |
| `hatchling-broker`, the keyring command's entry | 1 | 1 | 0 | 0 | 0 | `ca7f97a4131439bd7aeb0059a3289adaa6ffd4f2fa121391706eb27e1b728f17` |

No mutant was excluded, so there is no equivalence proof to give.

Unviable:

- `crates/hatchling-broker/src/doctor.rs:323:5: replace check_vault -> Finding with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/doctor.rs:368:5: replace unmade_keyring -> Finding with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/key_setup.rs:916:5: replace installed -> T with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/key_setup.rs:972:5: replace guard -> Result<Guard, String> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/key_setup.rs:996:5: replace stop_for_setup -> Result<Guard, String> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/key_setup.rs:1088:5: replace around -> Result<T, Error> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/key_setup.rs:1122:5: replace typed_pin -> Option<Zeroizing<String>> with Some(Zeroizing::new())`:
  unviable, build failure.
- `crates/hatchling-broker/src/key_setup.rs:1122:5: replace typed_pin -> Option<Zeroizing<String>> with Some(Zeroizing::from_iter([String::new()]))`:
  unviable, build failure.
- `crates/hatchling-broker/src/key_setup.rs:1122:5: replace typed_pin -> Option<Zeroizing<String>> with Some(Zeroizing::from_iter(["xyzzy".into()]))`:
  unviable, build failure.
- `crates/hatchling-broker/src/key_setup.rs:1122:5: replace typed_pin -> Option<Zeroizing<String>> with Some(Zeroizing::from("xyzzy".into()))`:
  unviable, build failure.
- `crates/hatchling-broker/src/service_settings.rs:142:5: replace plan -> Plan with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/service_settings.rs:181:5: replace rerun_if_wanted -> Option<Unread> with Some(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/vault_keyring.rs:145:9: replace KeyringConfig::keyring_path -> Result<&Path, Error> with Ok(Box::leak(Box::new(Default::default())))`:
  unviable, build failure.

What the mutation run cannot reach is the wiring that names the real
service: the settings file's path, `systemctl`, `systemd-run`, and the
exec that runs the command again. Those lines hold no operator for the
tool to change. The drill is their test.

## Defences broken by hand

Each defence was removed by hand, one at a time, and the tests that guard
it were run and their failures read. The unbroken tests passed before and
after.

| Defence removed | Test that failed | How it failed |
|---|---|---|
| The setup takes the service's settings whatever the shell holds (the bug) | `the_guided_setup_takes_the_services_settings_whatever_the_shell_holds` and the two beside it | from a shell with settings it stayed as it was; from a bare one it ran without the keyring |
| The shell's own settings are dropped | the first of those | nothing dropped where one was due |
| The setup is given what a first setup adds | the first two | the settings came without the keyring |
| The setup stays once it has the service's settings | `the_guided_setup_runs_again_until_it_has_exactly_the_services_settings` | it was to run again, for ever |
| Only `vault-keyring` with a guided action is guided | `a_guided_command_is_vault_keyring_with_a_guided_action` | another command's `setup` was taken for it |
| The keyring goes beside the vault | `a_first_setup_adds_a_keyring_beside_the_vault` | the keyring's directory was named as the keyring |
| A setup is root's | `a_setup_is_ready_only_as_root_on_a_service_install` | another user was let on |
| A setup needs a service install | same | it went on with no settings file |
| Settings that name a keyring never made are put back (the bug's other half) | `a_setup_that_makes_no_keyring_leaves_the_settings_as_they_were` | the settings still named the keyring |
| Settings are kept once the keyring is made | same | they were put back over a keyring that was there |
| Something that isn't a file is not a keyring made | same | a directory at the keyring's name kept the settings |
| The service is started after a failure | that test, `settings_a_setup_cannot_use_stop_it_and_the_service_goes_back`, `the_real_setup_stopped_at_its_first_step_changes_nothing` | one stop and no start |
| The service is stopped first | `a_first_setup_gives_the_service_its_keyring`, `a_service_that_does_not_stop_or_start_is_said` | the guide ran with no stop |
| A service that won't stop stops the setup | `a_service_that_does_not_stop_or_start_is_said` | the guide ran |
| A service that didn't start is said | same | the setup ended as done |
| The settings are read only if nobody else can change them | `settings_a_setup_cannot_use_stop_it_and_the_service_goes_back` | the setup ran on a file anyone could write |
| The settings are given their keyring before the guide runs | `a_first_setup_gives_the_service_its_keyring` | the guide ran on settings with no keyring |
| `systemctl`'s refusal is heard | `the_service_is_stopped_and_started_by_systemctl` | a refusal was taken for a yes |
| A guard that wasn't set is known | `a_setup_holds_a_lock_that_a_guard_waits_on` | it was said to be set |
| The guard starts the service | same | its command ended at the lock |
| One setup at a time | same | a second took the lock |
| A setup with no guard is told | `the_service_is_stopped_for_a_setup_under_its_guard` | nothing was said |
| A service that didn't stop is said | same | it was taken for stopped |
| `status` with no keyring answers | `status_without_a_keyring_says_so_and_what_to_run` | a usage error |
| `status` with a keyring never made answers | same | an error from loading it |
| A command that needs a keyring names the setup | `a_command_that_needs_a_keyring_names_the_next_step` | it named a setting |
| `doctor` knows a keyring never made | `the_vault_says_how_it_opens_after_a_restart` | "doesn't load" |
| A usage error points at the help | `a_usage_error_points_at_the_help` | the whole manual |
| Putting the settings back writes them | `a_setup_that_makes_no_keyring_leaves_the_settings_as_they_were` | the settings still named the keyring |

## What this change does not do

- **The authenticator path.** `enroll-totp` and `init` on an install with
  no keyring still stop, now naming the setup. Making the authenticator
  path work as a first command, with its own page and a place in the
  app's checklist, is the next change.
- **The wait for a key.** With no key the setup still waits five minutes
  before it stops. It can be stopped sooner with Ctrl-C, and Hatchling is
  started again.
- **Other usage errors' wording.** A setup that stops because no key came
  is still reported as a usage error.
- **Hosts without systemd-run or flock.** The setup still runs there, and
  says that an interruption would leave Hatchling stopped.
