# Evidence: the Room's programs run under its syscall profile

What was tested, how, and what came back, for the second change to the
Room's syscall profile: twelve more calls its image's programs make are
allowed in the Room alone, and six more are answered with an error. With
the first change that is twenty allowed and seven answered. Every number
here is read from a file in the results bundle, which is attached to the
release that carries this change.

| | |
|---|---|
| Base commit | `27eb0819` (`fix(room): find Node by name in a job (#428)`) |
| Tested tree | `f59f0d8569aec1b7c2748b9380605d0ef5927311` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Room image | sha256 `2fd340c93b99e146b38b995eec585cd72a76672b8c75f30137c3884cc5e2d927`, distribution snapshot `20261107T000000Z`, Node 24.22.2: the snapshot and Node the build script names today |
| Results bundle | `hatchling-room-filter-second-layer-results.tar.gz`, 214122 bytes, sha256 `e45cb5e083b5b25914c9c5740495a3980de46a49c3ea6b91879af7df046fe8ef` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

The first change made the image's file tools run at their plain use, and
was found from a test machine's tools. Run in the image itself, under the
profile, 127 everyday commands of the image's own programs were tried.
28 didn't end well (`scoping/scoping-image-run-before.txt`): `cp -a` and
`cp -p`, `mv` between the workspace and the session, `chmod -R`, `chown`,
`sed -i`, `tar` unpacking a directory inside another, `timeout`, `nice`,
`sync`, `make -j`, `wait` in a `sh` script, Python copying a file,
`git add` and every Git command after it, and Node, ended as it started.
One of the 28, Python's process pool, fails for a reason that isn't the
profile.

## What changed

In `crates/hatchling-sys/src/seccomp.rs`, for the Room's profile only.
The source says of each call what it can and can't do and which program
asked.

**Twelve more calls are allowed.**

| Call | What it is | Asked by |
|---|---|---|
| `flistxattr` | `listxattr`, of a file already open: names, no values | `cp -a`, `cp -p`, `mv` between file systems |
| `sendfile` | copies bytes between two open files, as `copy_file_range` does | Python's `shutil`, Node's `fs.copyFile` |
| `fchownat` | `fchown`, by name from an open directory | `cp -a`, `chown`, `mv` of a directory between file systems |
| `fchmodat2` | `fchmodat`, told not to follow a link | `chmod -R`, `tar` unpacking nested directories |
| `inotify_init` | `inotify_init1`, with no flags | `tail -f` |
| `rt_sigsuspend` | waits for a signal | `wait` in `sh`, `timeout` |
| `timer_create`, `timer_settime`, `timer_delete` | a timer that signals the process that made it | `timeout` |
| `getpriority` | reads how a process is scheduled | `nice` |
| `setpriority` | asks to be scheduled after others; can't ask for sooner | `nice` |
| `capget` | reads a process's capabilities | Node, as it starts |

`setpriority` reaches every process of the Room's user, the Room's
supervisor among them. It can only lower: a job can make the supervisor
wait its turn, as it could by keeping the processor busy.

**Six more calls are answered with an error, and not run.**

| Call | Answer | What the program does |
|---|---|---|
| `fgetxattr` | `ENOTSUP` | `sed -i` goes on without the access list it asked for |
| `link`, `linkat` | `EPERM` | Git renames the file it would have linked; `ln` and Perl say "Operation not permitted" |
| `sync` | `ENOSYS` | `sync` takes no notice |
| `mknodat` | `EPERM` | `make -j` says so once and goes on with an unnamed pipe; `mkfifo` says "Operation not permitted" |
| `io_uring_setup` | `ENOSYS` | Node goes on without `io_uring` |

So there are still no hard links in the Room, no extended attribute is
set or its value read, no named pipe or device file is made, there is no
`io_uring`, and `sync` writes out nothing but what `fsync` of a job's own
file does. The rule for an answer, written with the first change, now
says that a program handles an answer by going on without or by saying
plainly what it was refused.

**Kept, and said.** `openat2`, allowed by the first change, is not asked
for by any program in the image today: a newer `tar` than the image's
asks for it as it unpacks, which is how the test machine's tools came to
show it. It can only refuse what `openat` would open. It stays allowed,
and the source and `docs/arch/trust-boundary.md` say the image doesn't
ask for it yet.

The agent, harness, CLI and browser profiles are unchanged.
`docs/arch/trust-boundary.md` and the changelog say the new behaviour.

## The proof in the image

`crates/hatchling-sys/tests/room_image_tools.rs` runs 127 commands of the
image's programs, and `scripts/room-image-tools-proof.sh` gives it the
image as a Room has it: opened through the kernel's verity check and
mounted read-only, the Room's three devices, a workspace on the
machine's disk and a session in memory, neither one's files runnable,
and a user the image's files don't name. Each command is started as the
Room's launcher starts its supervisor: into the image, root given up, no
way back to more, and the profile installed as the program starts. A
command gets a job's own environment, from the constant the supervisor
uses.

124 commands must end well. Three must be refused, and say so: a hard
link by `ln` (which asks `linkat`), one by Perl (which asks `link`, as
Git does), and `mkfifo`.

On the candidate: **127 of 127 ended as they must**
(`root/image-proof.log`, sha256
`258ee8ea9e8e1451dac8d27a05787dc2f04d38341efac792234c76599abeeca6`).

The release workflow now builds this test and runs the script on the
image it builds, in the job that already mounts and probes that image.
That job runs on every pull request that touches the profile.

## Each part taken out by hand, twice

43 breaks, each run against the profile's unit tests and then against
the proof in the image. The unbroken tests and the unbroken proof passed
before and after. In the unit tests all 43 were caught, each by the
tests named for it. In the image the proof failed for 34 and passed for
9, and the 9 are the ones it can't see, listed under the tables. Each
failure was read.

The unit tests, by the names used below: *lists* pins the two lists and
each name's number; *verdicts* asks the built profile about every call;
*whole* compares everything the Room allows with the CLI profile's;
*others* asks the other profiles; *host tools* runs the test machine's
tools under the profile; *browser* is the browser profile's own test.

**A call not allowed.**

| Break | Unit tests that fail | The image proof |
|---|---|---|
| `not-allowed-fadvise64` | host tools, verdicts, whole | 37 fail: `fill` killed; `cat` killed; `cp` killed; and 34 more |
| `not-allowed-fchdir` | host tools, verdicts, whole | 8 fail: `fill` killed; `mkdir` killed; `find` killed; and 5 more |
| `not-allowed-creat` | host tools, verdicts, whole | 6 fail: `tar -c` killed; `tar -t` exit 2; `tar -x` exit 2; and 3 more |
| `not-allowed-fstatfs` | host tools, verdicts, whole | 10 fail: `du` killed; `find` killed; `mv, a directory to the session` killed; and 7 more |
| `not-allowed-symlinkat` | host tools, verdicts, whole | 3 fail: `fill` killed; `ln -s` killed; `readlink` exit 1 |
| `not-allowed-dup3` | host tools, verdicts, whole | 2 fail: `uniq` killed; `shuf` killed |
| `not-allowed-openat2` | host tools, verdicts, whole | passes |
| `not-allowed-llistxattr` | host tools, verdicts, whole | 5 fail: `ls` killed; `cp -a` killed; `mv, a directory to the session` killed; and 2 more |
| `not-allowed-flistxattr` | verdicts, whole | 5 fail: `cp -a` killed; `cp -p` killed; `mv, to the session` killed; and 2 more |
| `not-allowed-sendfile` | verdicts, whole | 3 fail: `python3, files and processes` killed; `python3 -m venv` killed; `node, files and processes` killed |
| `not-allowed-fchownat` | verdicts, whole | 3 fail: `cp -a` killed; `mv, a directory to the session` killed; `chown` killed |
| `not-allowed-fchmodat2` | verdicts, whole | 4 fail: `tar -x` killed; `chmod -R` killed; `tar -xz, to the session` killed; and 1 more |
| `not-allowed-inotify_init` | verdicts, whole | 1 fail: `tail -f` exit 1 |
| `not-allowed-rt_sigsuspend` | verdicts, whole | 3 fail: `tail -f` exit 1; `timeout` killed; `sh, a script` killed |
| `not-allowed-timer_create` | verdicts, whole | 2 fail: `tail -f` exit 1; `timeout` killed |
| `not-allowed-timer_settime` | verdicts, whole | 2 fail: `tail -f` exit 1; `timeout` killed |
| `not-allowed-timer_delete` | verdicts, whole | passes |
| `not-allowed-getpriority` | verdicts, whole | 1 fail: `nice` killed |
| `not-allowed-setpriority` | verdicts, whole | 1 fail: `nice` killed |
| `not-allowed-capget` | verdicts, whole | 3 fail: `node --version` killed; `node, files and processes` killed; `node, watching` killed |

**A call not answered: as it was before, outside the profile.**

| Break | Unit tests that fail | The image proof |
|---|---|---|
| `not-answered-fsetxattr` | host tools, verdicts | 2 fail: `install` killed; `sed -i` killed |
| `not-answered-fgetxattr` | verdicts | 1 fail: `sed -i` killed |
| `not-answered-link` | verdicts | 7 fail: `perl, a hard link` killed; `git add` killed; `git commit` exit 128; and 4 more |
| `not-answered-linkat` | verdicts | 1 fail: `ln, a hard link` killed |
| `not-answered-sync` | verdicts | 1 fail: `sync` killed |
| `not-answered-mknodat` | verdicts | 2 fail: `mkfifo` killed; `make -j` killed |
| `not-answered-io_uring_setup` | verdicts | 2 fail: `node, files and processes` killed; `node, watching` killed |

**A call allowed where it is answered.**

| Break | Unit tests that fail | The image proof |
|---|---|---|
| `granted-fsetxattr` | verdicts, whole | passes |
| `granted-fgetxattr` | verdicts, whole | passes |
| `granted-link` | verdicts, whole | 1 fail: `perl, a hard link` exit 0 |
| `granted-linkat` | verdicts, whole | 1 fail: `ln, a hard link` exit 0 |
| `granted-sync` | verdicts, whole | passes |
| `granted-mknodat` | verdicts, whole | 1 fail: `mkfifo` exit 0 |
| `granted-io_uring_setup` | verdicts, whole | 2 fail: `node, files and processes` killed; `node, watching` killed |

**A call answered with another error.**

| Break | Unit tests that fail | The image proof |
|---|---|---|
| `other-error-fsetxattr-EPERM` | host tools, verdicts | 1 fail: `install` exit 1 |
| `other-error-fgetxattr-EPERM` | verdicts | passes |
| `other-error-link-ENOSYS` | verdicts | 1 fail: `perl, a hard link` exit 38 |
| `other-error-linkat-ENOSYS` | verdicts | 1 fail: `ln, a hard link` exit 1 |
| `other-error-sync-EPERM` | verdicts | passes |
| `other-error-mknodat-ENOSYS` | verdicts | 1 fail: `mkfifo` exit 1 |
| `other-error-io_uring_setup-EPERM` | verdicts | passes |

**The rest.**

| Break | Unit tests that fail | The image proof |
|---|---|---|
| `grants-leak-to-the-cli-profile` | browser, others, whole | passes |
| `a-name-has-the-wrong-number` | lists, others, whole | 4 fail: `tar -x` killed; `chmod -R` killed; `tar -xz, to the session` killed; and 1 more |

**The nine the image proof passes with**, and why the unit tests are what
catch them:

- `not-allowed-openat2`: no program in the image asks for it (above).
- `not-allowed-timer_delete`: no command run removes a timer; it is
  allowed as the third of its set.
- `granted-fsetxattr`, `granted-fgetxattr`, `granted-sync`: a program
  given what it asked for goes on, as it does when answered.
- `other-error-fgetxattr-EPERM`, `other-error-sync-EPERM`,
  `other-error-io_uring_setup-EPERM`: these programs go on from either
  error. The error chosen is the one that says what is so.
- `grants-leak-to-the-cli-profile`: the Room's profile is unchanged by
  it.

Three rows say why an answer is the one it is. `fsetxattr` answered
`EPERM` makes `install` fail. `link` answered `ENOSYS` still lets Git go
on, but Perl then says the call doesn't exist, which isn't so.
`io_uring_setup` allowed, with the rest of `io_uring` still outside the
profile, ends Node: answering it is what lets Node run.

`unit-breaks.log` sha256
`1509a7bda9c771fd7ffb1bbcbcc1047e2d75ffad575c60b00e2098a878e8ed20`;
`root/image-breaks.out` sha256
`8ab5104be20b3a33160e9b7e905322041e5de9b7c632d7aff2629e80fa97e2d0`. Each
break's own output from the proof is in `root/image-breaks/`.

### What the first run of the breaks found

The breaks were first run against an earlier tree of this change, and
three things were changed for what they showed:

- With `link` allowed, the proof passed: its one hard link was `ln`'s,
  which asks `linkat`. A command that asks `link` and must be refused
  was added (Perl's).
- With `openat2` not allowed, the proof passed. That is how it was found
  that the image doesn't ask for it.
- Three verity mappings were left open. A break that ends `timeout`
  leaves the `tail -f` it started running, and that held the image's
  mount. The script now ends whatever still has the image as its root
  before it unmounts, and a run of all 43 breaks leaves nothing mapped
  and no process.

## Mutation

The tool finds no mutant in the lines this change changes in
`crates/hatchling-sys` (`gate-mutants-sys.txt`, `mut-sys.log`): the
change is the two lists, a table of numbers, and tests. The hand-breaks
are what tries them.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3754 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3754 | 0 | 0 |

`gate.log` sha256
`99ed8d8ac00aec92f423f72b0bb3efd586c876b01d6b6ced7b184fa4a0f195a9`.

In the gate the image proof has no image, says it is skipped, and passes:
it is run as root, outside the gate, with one.

## The library's tests many times over

The `hatchling-sys` library tests were run 25 times over on 1.98.1, every
test in parallel: 294 passed and 0 failed in each (`stress.log`, sha256
`731d6762adcfcddc4ff443b21dd79a31efdfa8a1218f2ce7d5ea52626634b7eb`).

On an earlier tree of this change
(`07b6e1f9e99a8fcc3017b4bb1d84194c2580b111`, which differs in the proof's
files, in comments and in the pages), one run in 25 failed one test:
`net::tests::unix_stream_quiet_distinguishes_empty_input_data_and_eof`.
It closes one end of a socket pair and expects the other end to show the
close at once. A child forked by another test at that moment holds a
copy of the closed end until it starts its own program, so the close has
not yet arrived. Nothing this change touches is in that test or in what
it calls, and it is not changed here. The run is kept in the bundle
(`stress-first.log`, and `stress-first-failed-10.log`, sha256
`287ac739519d6b8dc7076e80a9da2cbb2d1ae0a1b50f5c01fd4e7b3f63ddbcf5`). It
is to be fixed in a change of its own.

## As root, and in a real Room

The profile's unit tests were run as root, outside the sandbox, as CI
also runs them: 29 passed, 0 failed (`root/tools-as-root.log`, sha256
`ef91e4b3ef4008a35e829040bc4eb9bbc68afd226914e91aae7c03f3a139cc34`).

The supervisor was built static from this tree with the repository's
script (sha256
`01f3679cd7427182cb3b8865b42f7090dd2d7fec79d6c3acd11cc04db943fc4b`); its
17 tests and its three proofs under the Room's profile pass. Both
installed Room tests were then run as root with it, under the Room's
cgroup and this profile: 2 passed, 0 failed, and nothing was left mapped
(`root/installed.log`, sha256
`b2cd7cabef5021c395f48daad35773147ff9f98772bf38ab69cc99cbd9c7e7e5`).
Those tests run fixture programs in a Room, not the image's programs.

## What this change does not do

- **Run the image's programs in a Room the launcher made.** The proof
  does what the launcher does, by hand, around the real image and the
  real profile. It has no Room cgroup, no supervisor between the profile
  and the command, and the machine's `/proc`.
- **Prove the image this record names is the one a release ships.** It
  was built earlier from the same pinned snapshot and Node version. The
  release workflow runs the proof on the image it builds.
- **Make Python's process pools work.** They need `/dev/shm`, and the
  Room has none. That is the Room's mounts, and is left until something
  needs pools.
- **Cover what wasn't run.** 127 commands are everyday work, not every
  option of every program. A program that asks for something else is
  still ended, and that is how the next one will be found.
- **Give the agent a tool.** Nothing in the app or the agent reaches the
  Room yet.
