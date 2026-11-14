# Evidence: `unlock` puts a lost controller back itself

What was tested, how, and what came back, for a change to what
`hatchling-broker unlock` does when the service's cgroup is short of a
controller cells need. Every number here is read from a file in the
results bundle, which is attached to the release that carries this
change, unless it says otherwise.

| | |
|---|---|
| Base commit | `8174448e` (`feat(chat): give a working run what its owner sends meanwhile (#449)`) |
| Tested tree | `daa5224eaa034f81f48d84d07c91c84776ee2ce6` |
| Date | 2026-11-14 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-unlock-restores-controllers-results.tar.gz`, 809753 bytes, sha256 `dd0d7c1825bc095a520daff9d866bdc641b41bb54a7495d6626992b9d506f950` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

On a real install under WSL2, locked after an update, its owner ran
`sudo hatchling-broker unlock` and was told: "not asking for your key
yet: the service's cgroup has no cpu controller, and cells won't run
without one. Run `sudo systemctl daemon-reload`, then `sudo systemctl
restart hatchling`."

Every WSL distro shares one cgroup tree. Another distro that runs
systemd had started while the install was locked, and its systemd
rewrote the shared `system.slice` to offer `memory pids`: the install's
service lost `cpu`. Done again twice on purpose, the same happened both
times. `systemctl daemon-reload` in the install's distro gave the
controller back, with nothing restarted. (None of this is in the
bundle: it was seen by hand on that install, on 2026-11-14.)

The check was right and the message was true. But the owner was at the
terminal, key in hand, and was sent away to run two commands as root
that the command in front of them, already running as root, could run
itself.

## What changed

- **`unlock`**, when the service's own cgroup is there and short of a
  controller its tree offers, has systemd apply the unit's settings
  again (`systemctl daemon-reload`), looks at the cgroup up to twenty
  times a tenth of a second apart, and, once nothing is wrong, says
  "the service's cgroup had lost its cpu controller, and has it back."
  and goes on to ask for the key.
- **If the controller doesn't come back**, or systemd couldn't be
  asked, it stops as before, with the same words and the same two
  commands.
- **Any other problem** (a machine whose cgroups offer no controller, a
  path that isn't a cgroup) is left as it was found: nothing is run.
- **A cgroup named in the command's own settings**
  (`HATCHLING_CELL_CGROUP` in its environment) is only looked at, as
  before: it isn't the service's unit's to give back.
- **`doctor`** still only looks, and changes nothing.
- `docs/install.md` and `docs/windows.md` say what `unlock` now does,
  and that every distro shares WSL's one allowance of memory and one
  GPU.

Nothing about what a cell may do changes. The unit already asks for
these controllers (`Delegate=yes`, `CPUWeight=100`); the reload has
systemd give the unit what it asks for. It is run by a command its
owner started as root, which already ran `systemctl show` to find the
cgroup.

## Tests

Seven, in the broker.

- `doctor::tests::a_cgroup_short_of_a_controller_is_given_it_back`: a
  cgroup short of a controller is put right and what came back is
  named; where systemd couldn't be asked it is as it was, and isn't
  looked at again; where it is still short, what the second look found
  is left; and for nothing wrong, a path that isn't a cgroup, or a tree
  that offers no controller, nothing is run.
- `doctor::tests::the_cgroup_is_looked_at_until_it_is_right_or_the_looks_run_out`:
  one look when it is right at once; a pause between two looks and none
  after the last; and no more looks than were said.
- `doctor::tests::a_program_is_said_to_have_ended_well_only_when_it_did`:
  `true`, `false`, and a program that isn't there.
- `doctor::tests::what_became_of_a_cgroup_is_said_as_one_of_three_things`.
- `doctor::tests::a_cgroup_only_looked_at_is_said_as_it_is_found`.
- `doctor::tests::what_needs_no_putting_right_is_left_alone`: no cgroup
  set, a path that isn't one, and the machine's own root cgroup.
- `unlock_relay::tests::unlock_says_what_became_of_the_cells_cgroup`:
  nothing of a cgroup with nothing wrong; one line, and on, when a
  controller came back; and the line to stop with when it didn't.

No test reaches a real systemd: the one that could, by having a cgroup
short of a controller, would reload the machine's units from a unit
test.

## Breaks by hand

Thirteen, each one part of the change taken out or bent, the broker's
tests of `doctor` and `unlock` run on each, and each failure read
(`unit-breaks.log`, `unit-*.log`, `uc_break.py`). All thirteen were
caught, and the 57 tests passed before and after.

| Break | Caught by |
|---|---|
| A reload that failed is taken for done | the first test: the cgroup was said to be right |
| The cgroup isn't looked at again | the first test: no second look |
| What came back isn't named | the first test |
| A tree that offers no controller is tried too | the first test: it was said to be right |
| No pause between two looks | the looks' test: "right at the third" |
| One look more than was said | the looks' test: "never right", four looks for three |
| It goes on looking once nothing is wrong | the looks' test: "right at the first look", five looks |
| A program that ran is said to have ended well, however it ended | the program's test: `false` |
| Looked at only, a problem is said as nothing | the looked-at test |
| A cgroup given its controllers back is said to have had nothing wrong | the three things' test |
| `unlock` goes on to ask for the key with the cgroup still wrong | `unlock`'s test |
| `unlock` says nothing of what it put right | `unlock`'s test |
| `unlock` stops when it has put the cgroup right | `unlock`'s test |

## Mutation

`cargo mutants --in-place --in-diff` on the change to the broker's
source, the broker's library tests on each (`mut.log`,
`mut-broker-outcomes.json`, sha256
`f44ab47ceb4c00c9a94725582321b27d60c28952f913b286ed702c87f5cdf388`):

| Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|
| 20 | 13 | 0 | 0 | 7 |

The unviable ones:

- `doctor.rs:320:5: replace cell_limits_seen -> CellLimits with Default::default()`: unviable, build failure
- `doctor.rs:342:5: replace cell_limits_restored -> CellLimits with Default::default()`: unviable, build failure
- `doctor.rs:367:5: replace became -> CellLimits with Default::default()`: unviable, build failure
- `doctor.rs:391:5: replace restored -> (Option<hatchling_sys::cgroup::DelegationProblem>, Vec<&'static str>,) with (Some(Default::default()), vec![])`: unviable, build failure
- `doctor.rs:391:5: replace restored -> (Option<hatchling_sys::cgroup::DelegationProblem>, Vec<&'static str>,) with (Some(Default::default()), vec![""])`: unviable, build failure
- `doctor.rs:391:5: replace restored -> (Option<hatchling_sys::cgroup::DelegationProblem>, Vec<&'static str>,) with (Some(Default::default()), vec!["xyzzy"])`: unviable, build failure
- `doctor.rs:413:5: replace settled -> Option<P> with Some(Default::default())`: unviable, build failure

Two places mutation makes nothing of, and no test or break reaches:
where `unlock` chooses between putting the service's cgroup right and
only looking at one named in its own settings, and where the parts are
put together with the real `systemctl` and the real pause. Both are
lines of `unlock_main` and of `cell_limits_restored` that only run as
root against a real systemd. They are what the real install is for.

## The gate

In the sandbox's jail, on the tested tree, each toolchain from a cleared
build cache (`gate.log`, `gate-1.*.log`):

| Step | 1.85.0 | 1.98.1 |
|---|---|---|
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 | exit 0 |
| the same with `--all-features` | exit 0 | exit 0 |
| `cargo build --workspace` | exit 0 | exit 0 |
| `cargo test --workspace --no-fail-fast` | exit 0: 135 binaries, 3851 passed, 0 failed | exit 0: 135 binaries, 3851 passed, 0 failed |
| `seed-check`, `protocol-schema` | exit 0 | exit 0 |

The broker's tests of `doctor` and `unlock` were run as root too, as CI
runs them: 57 passed (`root/as-root.log`).

## On a real install

Added on 2026-11-14, after the change was merged (`2993ce29`). The file
is in a second bundle,
`hatchling-unlock-and-follow-ups-real-install-results.tar.gz`, 1843 bytes, sha256
`f61850b88da1adcb419097ec5b622d9b64a35beaec5eba0d0137728a7400bd4e`.

The install the fault was found on was updated by hand to the merged
tree, which left it locked. A second WSL distro, not a Hatchling, was
then stopped and started again, and the install's service lost `cpu`,
as before: `system.slice` offered `memory pids`. Nothing was reloaded.
Its owner then ran `sudo hatchling-broker unlock` once.

| When (the install's clock) | What |
|---|---|
| 03:24:17 | the update started the service, locked |
| 03:24:44 | after the second distro started: the service has `memory pids` |
| 03:27:47 | systemd's journal: a reload asked for by `systemctl`, finished in 87 ms |
| 03:27:57 | the broker's log: "the vault was unlocked" |
| after | the service has `cpuset cpu io memory pids`; the journal has no start of it after 03:24:17 |

So the command reloaded, the controller came back, and it went on to
the key, with nothing restarted. A chat run was then made on the
install and answered, using the agent's computer twice: its cell was
given a cgroup, which a cell isn't without the cpu controller.

The owner says the terminal showed "the service's cgroup had lost its
cpu controller, and has it back." before it asked for the key. The
terminal itself wasn't read for this record: the reload ten seconds
before the unlock is what shows here that the command did it.

## What wasn't tried

- **More than once.** The command against a real systemd was run one
  time, on one install.
- **systemd older than the one the cgroup drop-in needs**, a machine
  that isn't under WSL, and a service whose cgroup lost a controller
  other than `cpu`.
- **A reload that takes more than two seconds to give the controller
  back.** `unlock` then stops with the old message, and running it
  again would find the controller there.
