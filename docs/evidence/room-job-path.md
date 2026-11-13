# Evidence: Node is found by name in the Room

What was tested, how, and what came back, for a small change: a job's
`PATH` gains `/usr/local/bin`, where the Room's image keeps Node. Every
number here is read from a file in the results bundle, which is attached
to the release that carries this change.

| | |
|---|---|
| Base commit | `b3c8a621` (`fix(room): let the Room's own file tools run under its filter (#427)`) |
| Tested tree | `c4750f1a5c95df4ddddc9b6a6c6e8efa30d54c8b` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-room-job-path-results.tar.gz`, 180048 bytes, sha256 `f89feca95a7f70e80841f7f1278328ed1b25fbd9e906d78d9cf79fae643c2fb8` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

The Room's image keeps the programs of its distribution in `/usr/bin` and
`/bin`, and the one it adds, Node, in `/usr/local/bin`. The supervisor
gave a job the path `/usr/bin:/bin`. So a job that ran `node` was told
"not found"; only `/usr/local/bin/node`, by its whole path, started it.

## What changed

- A job's path is `/usr/local/bin:/usr/bin:/bin`. All three directories
  are in the read-only, kernel-verified image, so no job can put a
  program in any of them, and nothing in the first shadows the others
  unless the image is built so.
- The path is one constant, `hatchling_core::room::JOB_PATH`, beside the
  Room's other fixed places, so the supervisor and any proof that starts
  a program as a job does read the same one.
- The supervisor's test of a job's environment, the installed Room test
  of the same, and the wire's test of its constants say the new path.
  `docs/arch/trust-boundary.md` and the changelog say it.

## The fault, by hand

The path was put back as it was and the tests run; the unbroken tests
passed before and after.

| Part taken out | Tests that failed | How |
|---|---|---|
| `/usr/local/bin` not on a job's path | the supervisor's `a_job_is_given_its_directory_a_fixed_environment_and_its_input`; the wire's `a_streams_file_is_named_from_the_jobs_name_alone` | the job's `env` prints `PATH=/usr/bin:/bin`; the constant isn't the one pinned |

`unit-breaks.log` sha256 `f43b3b75a40fb113c69d678de04d18df61069b034526620d12787e0252266b0f`.

## In the image

In the Room's image (sha256 `2fd340c93b99e146b38b995eec585cd72a76672b8c75f30137c3884cc5e2d927`, built from the same
pinned distribution snapshot and Node version as the build script names
today), as a user, with a job's environment and no profile:

| A job's path | `node --version` |
|---|---|
| `/usr/bin:/bin` | exit 127, "No such file or directory" |
| `/usr/local/bin:/usr/bin:/bin` | exit 0, `v24.22.2` |

`root/node-by-name.log` sha256 `413ae28d49a25d0a3a6005aeccdcba287d0472b971ebe9fae0182e724ce4ea18`.

## Mutation

`cargo mutants --in-place --in-diff np-slice-browser.diff --timeout 60 --features room-supervisor -- --bin hatchling-room`,
on the lines changed in the supervisor, after an unmutated baseline.

| Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|
| 1 | 0 | 0 | 0 | 1 |

- `crates/hatchling-browser/src/bin/hatchling-room.rs:477:9: replace Room<'_>::start -> std::result::Result<Job, NotStarted> with Ok(Default::default())`: unviable, build failure.

The one line changed in the supervisor is inside `Room::start`, and the
tool's one mutant for that line is of the whole function. The hand-break
above is what tries the line.

The tool finds no mutant in the lines changed in `hatchling-core`: the
change there is a constant (`gate-mutants-core.txt`).
`mut-browser-outcomes.json` sha256 `2341a1465368ef50fc698d6e4d746d49b4834b2f69172ec5f693b6bf6c53c66a`.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3752 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3752 | 0 | 0 |

`gate.log` sha256 `106bfaa0100ba097440cd234f9f4afb68c350f147bcf22badb9037b99c2eaee3`.

An earlier tree of this change failed `clippy -D warnings` on both
toolchains: the new constant's comment began with a paragraph the lint
finds too long. The comment was shortened and everything here was run
again on the tree above.

## In a real Room

The supervisor was built static from this tree with the repository's
script (sha256 `a38e71f687d9e3b44aa33aea5e59dd6b9408ab777d648e6eecc8dc49e526ae4b`); its 17 tests and its three proofs under the
Room's profile pass. Both installed Room tests were then run as root
with it, in a real Room: 2 passed, 0 failed, and nothing was left mapped.
The test reads a job's environment in the Room and requires the new path
(`root/installed.log`, sha256 `298b0b7d78fb6dbc096877a8821a4d040ffba1d7e59049de2fe3f643f2808b9a`).

## What this change does not do

- **Make Node run in the Room.** It is found now. Under the Room's
  syscall profile it is still ended as it starts: it asks for `capget`,
  which the profile doesn't have. That is the profile's next change.
- **Check the image's build against the constant.** The release's image
  probe runs Node by its whole path. Nothing fails if a later image puts
  a program somewhere a job's path doesn't reach.
