# Evidence: an install on a machine with no cgroup controllers

What was tested, how, and what came back, for the change that makes an
install on a machine whose cgroups offer no controllers work and say what
it is, makes `doctor` judge a service's cgroup before the service has
started, stops the installer calling a service that keeps stopping
"running", and lets an install an authenticator app protects be upgraded.
Every number here is read from a file in the results bundle, which is
attached to the release that carries this change.

| | |
|---|---|
| Base commit | `95bf8ee6` (`feat(keyring): an authenticator app can protect a new install, and leaves a way back (#422)`) |
| Tested tree | `d76dbca406865b2f63de2eebd76a70cd8f2b9d8f` |
| Date | 2026-11-12 (UTC) |
| Machines | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), systemd 259: a sandbox for the gate, and a separate throwaway install for the drill. App: Windows 11, Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0, TypeScript 5.8.3 |
| Results bundle | `hatchling-no-cgroup-controllers-results.tar.gz`, 781622 bytes, sha256 `4fcb13d7a109b4a5315a1407d04121d825679ce989029a6daecab1e96017df9e` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

In some containers and restricted VMs the machine's cgroups offer no
`pids`, `memory` or `cpu` controller. The installer gave the broker a
cgroup for its cells there all the same. The broker's spawner refuses to
run cells it can't bound, so the broker stopped at every start.

Two things hid it. The installer's own `doctor` looked at the service's
cgroup, which isn't there before the service's first start, nor between
the starts of one that keeps stopping, and said "each cell has its own
cgroup". And each start of the broker writes the app's tokens before the
spawner refuses, so the installer's wait for them passed, and it said
"Hatchling is running".

A first-hour test on such a machine found it: installed, doctor green,
the app dead, 45 restarts, and the only explanation in the journal.

The same test found that upgrading an install an authenticator app
protects failed at the installer's wait for the app's tokens. The restart
had locked the vault, as it should, and the installer treated only a
security-key vault as one that waits for its owner.

## What changed

- The installer looks at what the machine's cgroups offer before it gives
  the broker a cgroup. Where a controller is missing it leaves the
  setting out, holds cells by process limits alone, and says so.
- `doctor` judges a cell cgroup by what can be seen without changing
  anything: one that's there by what it has; one that isn't there yet by
  the root of the tree it will be made in; a path that's no cgroup at all
  as that. Each has its own fix.
- Cells held by process limits alone are a note in `doctor`, with how to
  change it, not a plain "ok". The app is told, and its "Your security"
  page says it of an install that has it.
- After starting the broker and the app's host, the installer watches the
  broker past one restart period. One that restarted, or failed, is said
  to keep stopping.
- An install whose vault an authenticator app protects is upgraded as one
  a security key protects is: it ends locked, with the command that
  unlocks it. If the upgrade fails after the service was stopped for it,
  the service is started again and the unlock is named.

## The drill, on a real install

A throwaway machine with systemd, a fresh install each time by the
documented own-build route against a stand-in model. For the
no-controller scenarios the machine's kernel was booted with
`cgroup_disable=memory,pids,cpu`, so the root of its cgroup tree offered
`cpuset io hugetlb rdma` and nothing else; for the others it was booted
as usual. Each scenario records what the installer said, whether the
cgroup setting was installed, the service's state and restart count
twenty seconds on, the journal's lines about cell limits, and what
`doctor` says then.

The programs drilled are a build of tree
`6965888948b7f31df7afc9d2629efdacfa7c9a95`, which is the tested tree
before one test was extended and the installer was finished; no program
differs. The no-controller scenarios ran on the tested tree's installer.
The upgrade drill and the controllers-present install ran on the
installer one change of wording earlier: its message after a failed
upgrade read "started again as it was installed before, and locked",
where the tested one reads "started again, and locked". The console logs
under `drill/` are the record.

| Scenario | Result |
|---|---|
| No controllers; the code before this change | install exits 0; doctor "each cell has its own cgroup"; "Hatchling is running"; the cgroup setting installed; 4 restarts in 20 seconds, the spawner refusing each time; doctor still says nothing is wrong |
| No controllers; this change | install exits 0, saying the machine's cgroups offer no pids, memory, cpu controller and cells are held by process limits alone; no cgroup setting; the service is running with 0 restarts; doctor notes process limits alone, with how to change it |
| No controllers; this change; the owner's own setting asks for a cell cgroup | doctor reports it as a problem before anything is started, with the fix; the install stops there, exit 1 |
| No controllers; the broker before this change with this installer; the owner's setting asks for a cell cgroup | the old doctor passes it; the broker starts and stops; the installer says "the broker keeps stopping after it starts", exit 1 |
| Controllers present; this change | install exits 0; the cgroup setting installed; doctor "each cell has its own cgroup"; running with 0 restarts |
| Upgrade of an authenticator-protected install; the installer before this change | exit 1, "the broker didn't prepare the app tokens"; the vault is waiting to be unlocked, unsaid |
| The same; this installer | exit 0, "Hatchling is up to date, and locked", with the unlock command; a code unlocks it |
| The same, with the model's server stopped so doctor finds a problem | exit 1 with doctor's finding; "Hatchling is started again, and locked", with the unlock command; a code unlocks it |

In the upgrade drill the app's host was stopped before the authenticator
was enrolled. That works round a fault found in this drill, which this
change does not fix: see the end of this record.

Not drilled: systemd older than 254 (the installer's branch for it is
unchanged), and a real container; the kernel flag stands for one.

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
| 1.85.0 | 0 | 135 | 3710 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3710 | 0 | 0 |

`gate.log` sha256
`cbde48c9b3f319b3e50e9561fec591ec2754b8dfb94b117ab1ef1bb60b78e953`.

The broker's library tests were then run 15 times over on 1.98.1, every
test in parallel: 15 runs, 1360 passed and 0 failed in each. `stress.log`
sha256 `592be25a7ea7a4a57ff631a35295520086cc8a73e1d8cb3e5ac64f65fde397d3`.

The installer is a shell script with no unit tests of its own. The drill
above is its test, and CI installs with it on every change.

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

The app's logs are under `app/` in the bundle, with the machine's own
paths replaced by `<repo>`. The app's browser tests (`npm run test:e2e`)
were not run: they are left to CI.

## Mutation

Every mutant on the lines changed against the base, on 1.98.1 with debug
info off, one run per crate:

```text
cargo mutants --in-place --in-diff <the crate's diff> --timeout 60 -- --lib
```

| Crate | Mutants | Caught | Missed | Timed out | Unviable | `outcomes.json` sha256 |
|---|---|---|---|---|---|---|
| `hatchling-sys` | 11 | 9 | 0 | 0 | 2 | `bd352886319101d990a4669dffacd24f5d61d39c7e6c3558169d6e3b31657fd5` |
| `hatchling-broker` | 14 | 10 | 0 | 0 | 4 | `e241adcacae07a7368b939e05c0c4555db4671d06db939e170513854597a7ff2` |

`hatchling-core`'s part of the change is one field of the setup the app
is told. The tool finds no mutant in it (`mut-core.log`); its wire form
is pinned by a test. No mutant was excluded, so there is no equivalence
proof to give.

Unviable:

- `crates/hatchling-broker/src/api_ops.rs:101:5: replace read_setup -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/api_ops.rs:131:5: replace setup_answer -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`:
  unviable, build failure.
- `crates/hatchling-broker/src/doctor.rs:354:5: replace check_cell_limits -> Finding with Default::default()`:
  unviable, build failure.
- `crates/hatchling-broker/src/doctor.rs:360:5: replace cell_limits_found -> Finding with Default::default()`:
  unviable, build failure.
- `crates/hatchling-sys/src/cgroup.rs:89:5: replace delegation_problem -> Option<DelegationProblem> with Some(Default::default())`:
  unviable, build failure.
- `crates/hatchling-sys/src/cgroup.rs:103:5: replace delegation_problem_in -> Option<DelegationProblem> with Some(Default::default())`:
  unviable, build failure.

## Defences broken by hand

Each defence was removed by hand, one at a time, and the tests that guard
it were run and their failures read. The unbroken tests passed before and
after. The Rust ones were run in the sandbox; the app's on its built
JavaScript, from a copy of the file that was put back after.

| Defence removed | Test that failed | How it failed |
|---|---|---|
| A cgroup that isn't there yet is judged (the bug) | `a_cgroup_not_made_yet_is_judged_by_its_trees_root` | nothing was found wrong on a tree that offers no controllers |
| It's judged by its tree's root, not the slice above it | same | a slice systemd hadn't filled yet was taken for the machine |
| One that's there is judged by what it has | `a_cgroup_that_is_there_is_judged_by_what_it_has` | its own missing controller went unsaid |
| A path that's no cgroup is said to be | `a_path_outside_a_cgroup_tree_is_said_to_be` | nothing was found wrong |
| Only a cgroup filesystem counts as one | same | a temporary directory was taken for a cgroup tree |
| Doctor judges a cgroup that isn't there yet | `a_machine_with_no_controllers_is_a_problem_before_the_first_start` | "each cell has its own cgroup" |
| A machine with no controllers gets its own fix | same | it was told to reload systemd |
| Process limits alone are a note | `process_limits_alone_are_noted_as_weaker` | said as fine |
| A problem with a cgroup that's set is a problem | `a_cgroup_without_its_controllers_is_a_problem`, and the test above | "each cell has its own cgroup" |
| The app is told | `only_the_owner_reads_whats_set_up` | the setup said cells had a cgroup when they had none |
| It's on the wire only when true | `process_limits_only_is_on_the_wire_only_when_true` | an install with a cgroup said `false` |
| The app's adapter keeps it | "the broker's word, in the app's" | lost on the way |
| The app's page says it | "the page says only what is true of this install" | nothing said of an install that has it |
| The app's page says it only then | same | said of an install whose cells have a cgroup |

## What this change does not do

- **Give such a machine cgroups.** Cells there are held by process limits
  alone: nothing bounds a cell's memory or CPU as a whole, and a cell that
  ends isn't killed as one. It is said in three places; it is not fixed,
  and can't be from inside the machine.
- **Other weakenings.** A kernel with no Landlock is still said only in
  the broker's log, not by `doctor` or the app.
- **The room disk can stay mounted after a stop, on some machines.**
  Found in this drill, and older than this change. Where the machine's
  root mount doesn't propagate (WSL, and containers), a service with a
  mount namespace of its own that is started while the room disk is
  mounted keeps a copy of the mount. The app's own host is one, and the
  installer restarts it on every upgrade. After that, stopping or locking
  Hatchling leaves the room disk's mapping open ("device is still in
  use"), and key setup refuses until that service is restarted. Seen
  here: after a plain upgrade the host held the mount; after stopping
  Hatchling the mapping's open count was 1 and `room-disk close` failed;
  restarting the host released it. A reboot alone did not show it in one
  check. This change keeps the installer starting the host when it always
  did, so it makes this no likelier; the fix is a change of its own, next.
