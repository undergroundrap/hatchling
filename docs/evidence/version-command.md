# Evidence: `--version` names the release, the commit and how it was built (WO-038 slice 1)

What was tested, how, and what came back, for WO-038's first slice: a version
command that says which build is installed.

| | |
|---|---|
| Base commit | `5a9d270e` (`feat(doctor): probe the syscall filter, the Room's image and the Windows seal (#551)`) |
| Tested tree | `d5ddf933` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox, as an unprivileged user |
| Toolchains | Rust 1.98.1 and 1.85.0 (rustup) |

This record was added after the runs, in its own commit. It changes no code.

## The threat

An owner who can't tell what's installed can't tell whether an update took,
which release's notes and advisories apply, or whether the program in
`/usr/local/bin` is the one they meant to run. A version string can mislead
in its own ways:
- **A modified build saying it's a commit's.** A build from a checkout with
  changes would name a commit it doesn't match.
- **Another repository's commit.** A copy of the crate inside another git
  repository would name that repository's commit.
- **A release built from changed sources.** A release build from an edited
  checkout would ship programs that say they're the tag's, and aren't.

`--version` is what the program says of itself. It doesn't prove the program
is the release's; `SHA256SUMS` and the release's attestation do, and
`docs/updating.md` says so.

## What changed

- **`hatchling-broker --version` and `hatchling --version`** (and `-V`) print
  the program and its release, then the commit and how it was built:

  ```text
  hatchling-broker 0.9.0
  commit <the commit>
  built as a release is built, by scripts/release-artifacts.sh
  ```

  They read no settings and start nothing.
- **`crates/hatchling-broker/build.rs` decides once,** with
  `src/build_kind.rs`, which it includes and the lib tests:
  - not the release profile: a debug build, commit not recorded. No git runs,
    and nothing reruns for a commit, so tests, checks and mutation runs cost
    nothing more;
  - a published package's `.cargo_vcs_info.json`: its commit, and whether it
    had changes;
  - a checkout whose prefix is `crates/hatchling-broker/`: its commit, and
    whether its tracked files differ (`git status`, which takes no lock);
  - `HATCHLING_BUILD=release`, which `scripts/release-artifacts.sh` sets: a
    release build. It fails on a checkout with changes, outside a checkout of
    this repository, from a package, or without `--release`.
- **Rerun triggers**, with the release profile only: the git HEAD, its ref,
  `packed-refs`, the index, `crates/`, `Cargo.toml` and `Cargo.lock`. An edit
  after a build reruns the decision, so it never says "unmodified" of a tree
  that's changed since.
- **The release workflow** checks, on each target's two builds, that each
  program says exactly the workspace's version, the commit built and a
  release build, and on a tag, the tag's version and `GITHUB_SHA`.

## Results

| Check | Result |
|---|---|
| The version tests (the decision, the words, the flag) | 27 passed, on 1.98.1 and on 1.85.0 |
| The broker's smoke tests, `--version` with no settings among them | 14 passed, on both |
| `hatchling`'s command-line tests | 6 passed, on both |
| Release-profile builds, real (below) | each says what it should |
| Hand-breaks | 16 broken, 16 caught, each by the test named for it |
| Targeted mutation (`--in-diff`) | broker: 36 mutants, 31 caught, 0 missed, 0 timeouts, 5 unviable; `hatchling`: 1 mutant, caught |
| `cargo fmt --check` | 0 |
| clippy `-D warnings`, default and all features | clean on 1.85.0 and 1.98.1 |
| `cargo test --workspace`, `HATCHLING_REQUIRE_CELL=1`, `HATCHLING_REQUIRE_CGROUP=1` | 135 binaries, 4,280 passed, 0 failed, on 1.85.0 and on 1.98.1 |
| `seed-check`, `protocol-schema`, `audit` | all passed on both |

### Release-profile builds

`cargo build --release -p hatchling-runtime -p hatchling-broker --bins`, in
the sandbox's checkout at the tested tree (committed there as `2e00b2b9`):

| Build | `hatchling-broker --version` said |
|---|---|
| From the clean checkout | `commit 2e00b2b9…`, `built from source, at that commit` |
| Again, nothing changed | no crate recompiled: the build script didn't rerun |
| After a line added to a file no broker crate compiles | `built from source, at that commit with changes not committed` |
| `HATCHLING_BUILD=release`, the line still added | the build failed: `hatchling-broker: a release is built from a checkout with no changes` |
| `HATCHLING_BUILD=release`, the line removed | `commit 2e00b2b9…`, `built as a release is built, by scripts/release-artifacts.sh` |

`hatchling --version` said the same of itself each time.

### Hand-breaks

| Break | Caught by |
|---|---|
| A release built from a tree with changes | `a_release_is_built_only_from_an_unmodified_checkout_of_this_repository` |
| A copy in another repository takes its commit | `a_checkout_of_this_repository_gives_its_commit`, and the release test |
| Anything taken as a commit | the same two |
| A debug build records a commit | `only_a_release_profile_build_records_a_commit` |
| A package's own commit lost to a checkout around it | `a_published_package_gives_its_own_commit_not_a_checkout_around_it`, and the release test |
| A package from a tree with changes said unmodified | `a_published_package_names_its_commit_and_whether_it_had_changes`, and the package test |
| Upper case taken as a commit | `a_commit_is_forty_or_sixty_four_lowercase_hex_digits` |
| Any length taken as a commit | the same |
| A kind's name read back as another | `each_kind_is_carried_by_its_name` |
| A build with changes says the same as one without | `a_build_says_its_release_its_commit_and_how_it_was_made` |
| The commit left out of what's said | the same |
| A commit recorded, never said | `this_build_says_what_its_build_script_recorded` |
| What the build script recorded, ignored | the same |
| `-v`, or `--version` with more after it, taken for `--version` | `version_is_asked_for_alone` |
| The broker never answers `--version` | `version_says_the_release_and_how_it_was_built_with_no_settings` (the broker's binary) |
| `hatchling --version` says only its version | `help_and_version_exit_zero_on_stdout` (`hatchling`'s binary) |

**Found by these runs:** the first mutation run missed one mutant, the commit
always `None`. A test build records none, so no test could tell. The check
that an empty record means none is now a function the tests call with a real
commit (`recorded`).

**Unviable mutants:** the five replace a value with `Default::default()`,
which `Kind` doesn't have.

## Residual

- **`build.rs` itself** runs only at build time, so no test runs it. Its
  decision is the tested `decide`; what it reads (git, the package's record,
  the release variable) is proven by the release-profile builds above, and,
  for each release, by the release workflow's check.
- **A published package's record** is tested as text: no build here was made
  from a package, which needs the registry.
- **Untracked files** don't make a build "modified": only tracked files are
  compared. A new source file is compiled only once a tracked file names it,
  and that file then differs.
- **`--version` says what the program says of itself.** It's no proof the
  program is the release's: `SHA256SUMS` and the attestation are, as
  `docs/updating.md` says.
- **Slices 2 and 3** (`doctor` and the stores' modes; the Room's image on
  Linux) are still to come.
