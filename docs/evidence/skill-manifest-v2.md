# Evidence: skill manifest format 2, every file signed (WO-046 slice 1a, #613)

What changed, and how it was tested, for WO-046's first slice and
[#613](https://github.com/undergroundrap/hatchling/issues/613).

| | |
|---|---|
| Base | `main` at `a11169f7` (WO-046's design approved) |
| Date | 2026-11-22 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu), toolchains 1.98.1 and 1.85.0 |

This record was added after the change, in its own commit. It changes no code.

## The problem

A format 1 skill's signature covered its manifest's four fields, not the
program the entry point named. The program was bound only by the registry's
pin, its SHA-256 when the broker first saw that `name@version`, kept until a
restart, and nothing checked who could write the skills directory (#613). So
whoever could write that directory could change a signed skill's program,
and after a restart it ran under the original signer's identity.

## The change

**`crates/hatchling-skill`, manifest format 2:**

- The manifest holds `format: 2`, a name, a version, a description (at most
  512 bytes, one line), an optional `instructions` file (the playbook, at
  most 32 KiB), every file of the skill (`files`: path, size, SHA-256), and
  an optional `program` (a listed file and its capabilities). A skill has a
  playbook, a program, or both.
- The signed payload is the domain tag `hatchling-skill-manifest-v2\n` and
  the manifest as compact JSON, so the signature covers every file's hash.
  `canonical_bytes_are_pinned` pins the exact bytes.
- Bounds: at most 64 files; the files other than the program at most 1 MiB
  together (they are what reaches the agent's context); the program at most
  64 MiB, a bound of its own, since a compiled program is often several
  megabytes.
- Paths are plain relative paths, each listed once, and `manifest.json`
  never lists itself. Unknown and duplicate fields are refused, in the
  manifest and in each file entry.
- **Format 1 is retired:** `SignedManifest::from_json` refuses a v1 manifest
  saying v1's signature didn't cover its program and to sign the skill again
  as format 2. No v1 package existed outside the tests.
- `FileEntry::check` compares a file's bytes with its entry (size and hash);
  `Error::FileMismatch` names the skill and the file.

**`crates/hatchling-broker`, the loader (`skills.rs`):**

- Every listed file is read, refusing a symlink anywhere in its path, and
  checked against its signed entry on every load. A changed or missing file
  refuses the skill; a file the manifest doesn't list is never read.
- The skills directory and each skill's own directory must be owned by root
  or the broker and writable by no one else, the trust-roots file's rule. A
  skills directory that fails it refuses every skill; a skill directory, that
  skill alone.
- The registry still pins the program's hash: a skill signed again under the
  same `name@version` over other bytes is drift, refused until a restart.
- A playbook without a program loads, pins nothing to run, and declares no
  capability, so it can't be invoked; the skill cell refuses one with no
  program to run.

**The workspace is 0.10.0:** `hatchling-skill`'s public API changes, the
first breaking change after 0.9.0, so the version is bumped as
`docs/releasing.md` says (`Cargo.toml`, `Cargo.lock`, Windows Setup's
default). Nothing is released until the maintainer tags it.

## Tests

| Check | 1.98.1 | 1.85.0 |
|---|---|---|
| `cargo fmt --all --check` | pass | |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | pass | pass |
| `hatchling-skill` | 36 passed | 36 passed |
| `hatchling-broker --lib` skills, skill cell, invoke, run, ops, trust roots | 50 passed | 50 passed |
| Integration: `skill_rotation`, `skill_session`, `skill_fixture`, `skill_code_umask`, `skill_cell_spawn`, `skill_envelope`, `skill_run`, `harness` | all passed | |

New tests: each validation rule at and past its bound; v1 refused with its
message; every listed file checked, a changed and a missing file refused, an
unlisted file ignored; a playbook without a program loading; the skills
directory and a skill directory refused when group- or world-writable; and
the rotation test rewritten so a program changed under its signature is
dropped at load, while the same id signed again over other bytes is drift.

**Hand-breaks**, each failing its test and passing again when restored:
the per-file check removed from the loader; the mode check weakened; any
owner accepted; a v1 manifest accepted.

**Mutation**, on the lines changed against `a11169f7`
(`cargo mutants --in-place --in-diff`, each crate's own tests):

| Crate | Mutants | Caught | Missed | Timeouts | Unviable |
|---|---|---|---|---|---|
| `hatchling-skill` | 62 | 55 | 0 | 0 | 7 |
| `hatchling-broker` (`skills.rs`, `skill_cell.rs`) | 17 | 12 | 0 | 0 | 5 |

The first run missed two, each a gap in the tests: `&&` to `||` in the v1
check (no test had a manifest with both `format` and `entry_point`, or
neither), and `!=` to `==` in the directory owner check (a test can't make
a directory of another uid without root). The second is now
`owned_by_root_or`, tested with each kind of owner; the first has its test.
The unviable mutants replace a constructor or a parser's result with a
`Default` the types don't have.

`skill_cell_spawn`, which runs a 7.8 MB program in a skill cell, first
failed under a single 1 MiB bound for a whole skill, which is what led to
the program's bound of its own.
