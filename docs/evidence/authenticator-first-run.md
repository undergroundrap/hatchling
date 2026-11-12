# Evidence: an authenticator app as the first thing done to an install

What was tested, how, and what came back, for the change that makes
`vault-keyring enroll-totp` work as the first keyring command on a
service install, gives a first enrollment its backup codes, and names the
authenticator path where a person without a security key would look for
it. Every number here is read from a file in the results bundle, which is
attached to the release that carries this change.

| | |
|---|---|
| Base commit | `348a2dcc` (`fix(keyring): a first vault-keyring setup that fails no longer takes Hatchling down (#421)`) |
| Tested tree | `ef71bbce20cb89ef9afb78397fab7aad6eaf50f3` |
| Date | 2026-11-12 (UTC) |
| Machines | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2): a sandbox for the gate, and a separate throwaway install for the drill. App: Windows 11, Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0, TypeScript 5.8.3 |
| Results bundle | `hatchling-authenticator-first-run-results.tar.gz`, 1123800 bytes, sha256 `09869ca2327b7e0a9c5b7d7b13416a02dec5906bd16ba1eb585b4a7b7258485a` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

A first-hour test of a fresh install, by someone with no security key,
found the authenticator path only through `--help`. No page named
`enroll-totp`, the app's checklist offered only a security key, and as
the first keyring command on a fresh install the enrollment stopped at a
missing setting. It worked only after the security-key setup had been
run and abandoned, with the service stopped and started by hand around
it.

The drill for this change then found a worse thing. Enrolled as the
first thing done to a vault, an authenticator left it with **no backup
codes**: `vault-keyring status` read `tier TOTP …, 0 backup code(s)
left`. The move to a new key replaces a vault's backup codes, and a vault
that had none got none. A vault that asks for a code after every restart
then had one way in, the phone.

## What changed

- On a service install, `enroll-totp` is the service's own, as the guided
  setup is: it runs with exactly the service's settings, stops the
  service under the same guard, makes the keyring where there's none,
  enrolls, shows the backup codes, starts the service and unlocks it with
  a code. Where there's no service it is the plain command it was.
- A first enrollment gives backup codes. A vault with none is given a set
  before the move to the new key, which replaces it with the set the
  owner is shown.
- The setup's wait for a security key says, with its first hint, how to
  use an app instead. `vault-keyring status` with no keyring, and the
  commands that need one, name both ways. `unlock` with no key plugged in
  names what else opens the vault, not only a key.
- A page of its own, `docs/authenticator.md`, linked from the install
  guide, the security-key page and "Your security". The app's checklist
  and Settings offer the command beside the security key's, with what it
  doesn't protect against.

## The drill, on a real install

A throwaway machine with systemd, installed by the documented own-build
route against a stand-in model. Each scenario starts from a copy of the
fresh install's settings and state. The command runs at a terminal of its
own, and a script answers its prompts as an owner with an authenticator
app would: it computes codes from the seed it is shown, and never gives
the same code twice. The seed and the backup codes are masked in the
record.

The drill ran on the tested tree. `drill/console-totp-before.log` and
`drill/console-totp-after.log` are the record; sha256
`af8af490edc7a5cf99c7d61c0e72637c21430c312225d49e8f021875f1eebac3` and
`683744a9a70f3830e0d02093620d3d083cdc238c116d78257630a28ce3b096a7`.

| Scenario, from a fresh install | Result |
|---|---|
| `enroll-totp` with sudo, on the code before this work | exit 64, a usage error and 144 lines of manual; nothing changed |
| `enroll-totp` with sudo | exit 0; 10 backup codes shown; the service restarted and unlocked with a code; status `tier TOTP …, 10 backup code(s) left` |
| The same as root with no sudo | the same |
| The same from a root shell holding the service's settings | the same |
| Then a restart, and `unlock` | after the restart the vault waits to be unlocked; `unlock` with the app's next code exits 0 and the service runs |
| Three wrong codes | exit 1, "that code did not match"; the service is running; the vault still opens by itself (file tier) |
| Ctrl-C at the code prompt | the service is running again; the vault still opens by itself |

In an earlier run of the drill the script gave `unlock` the code it had
just used to finish the enrollment, and the vault refused it. That is the
vault's doing, not a fault: a code opens once.

Not drilled: a real phone app (the codes are computed to the same
standard), and Windows, where the command is run through `wsl`.

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
| 1.85.0 | 0 | 135 | 3704 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3704 | 0 | 0 |

`gate.log` sha256
`6831f9cd8e7ea78a7e48cf251161d64ceac6b287aac4e90c106fb0a98a36a989`.

The broker's library tests were then run 15 times over on 1.98.1, every
test in parallel: 15 runs, 1358 passed and 0 failed in each. `stress.log`
sha256 `32daeed1e07b017e234b5991b69af7686f11ab32e8ad10251f50d48451380119`.

## The app

Run on the tested tree, from the `frontend` directory, with the test
commands given to node directly from a POSIX shell so their quoted
coverage patterns are read as written.

| Check | Result |
|---|---|
| `tsc --noEmit` | exit 0 |
| `lint`, `design:check`, `actions:check`, `protocol:check`, `seam:check`, `format:check` | each exit 0 |
| Unit tests, with the 78 / 88 / 81 coverage floor | 490 passed, 0 failed |
| Live adapter tests, with the 100% line, branch and function floor | 176 passed, 0 failed; 100 / 100 / 100 |
| Operator adapter tests, with the 100% floor | 16 passed, 0 failed; 100 / 100 / 100 |

The app's copy of "Your security" is held to the doc word for word by a
test, and was changed with it. The app's logs are under `app/` in the
bundle, with the machine's own paths replaced by `<repo>`. The app's
browser tests (`npm run test:e2e`) were not run: they are left to CI.

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
| `hatchling-broker`, library tests | 22 | 19 | 0 | 0 | 3 | `97d377e8fe0a291af8c2fdb07b32b39c859a00eab2db0d1fbfebc93b5498f687` |
| `hatchling-broker`, the keyring command's entry | 1 | 1 | 0 | 0 | 0 | `5019e23c8a19c5d33a16ec1ee2c90c4b2904449a37d3f1a669ba31e528030497` |

No mutant was excluded, so there is no equivalence proof to give.

Unviable:

- `crates/hatchling-broker/src/key_setup.rs:143:5: replace run -> Result<Done, Error> with Ok(Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/service_settings.rs:142:5: replace plan -> Plan with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/vault_keyring.rs:770:5: replace enroll_totp -> Result<Option<BackupCodes>, Error> with Ok(Some(Default::default()))`:
  unviable, build failure.

## Defences broken by hand

Each defence was removed by hand, one at a time, and the tests that guard
it were run and their failures read. The unbroken tests passed before and
after. The broker's were run in its sandbox; the app's on its built
JavaScript, from a copy of the file that was put back after.

| Defence removed | Test that failed | How it failed |
|---|---|---|
| The enrollment is the service's own on a service install | `the_authenticator_enrollment_is_the_services_own_on_a_service_install`, `the_authenticator_enrollment_takes_the_services_settings_too` | it was the plain command; it ran with no keyring among its settings |
| It is the plain command where there's no service | the first of those | it took a service's settings with no service |
| Only `vault-keyring`'s `enroll-totp` is it | same | another command's was taken for it |
| The guided setups are still the service's own | that test, and the guided setup's three | the setup ran as it did before the last fix |
| A keyring is made where there's none | `a_keyring_is_made_where_there_is_none_and_left_where_there_is_one` | none was made |
| A keyring that's there is left | same | a second adoption failed on the plain key being gone |
| A first enrollment gives backup codes (the drill's finding) | `a_first_totp_enrollment_gives_backup_codes`, `after_a_tier_increase_the_previous_key_material_opens_nothing` | no codes were shown |
| Someone with no key is told the other way | `a_key_not_seen_gets_a_hint_on_what_to_check` | said 0 times where once was due |
| `status` names both ways | `status_without_a_keyring_says_so_and_what_to_run` | the authenticator line was missing |
| A command that needs a keyring names both ways | `a_command_that_needs_a_keyring_names_the_next_step` | it named only the setup |
| `unlock` names what else opens the vault | `with_no_key_plugged_in_unlock_names_what_else_opens_the_vault` | it spoke only of a security key |
| The enrollment takes the service's settings whatever the shell holds | `the_authenticator_enrollment_takes_the_services_settings_too` | it ran with the shell's |
| The app's checklist offers the authenticator | "a new install has what it needs left to do, in order", "with no key, the vault is left to do until the owner says they have none" | one command where two were due |

## What this change does not do

- **Backup codes nobody was shown.** An enrollment that stops after its
  first step and before a code matches (three wrong codes, Ctrl-C) leaves
  a set of backup codes that nobody saw, counted by `status` at the file
  tier, where nothing asks for one. The next enrollment replaces them
  with a set that is shown. The security-key setup already did the same.
- **Windows' Start menu.** "Protect your vault" there is still the
  security-key setup. The authenticator command is in Settings, and in
  the page, for PowerShell.
- **A used code's message.** `unlock` given a code that was already used
  says the vault did not unlock, not that the code was used.
- **The other usage errors.** `init` on an install with no keyring still
  stops, naming both ways on.
