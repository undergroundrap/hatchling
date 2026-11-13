# Evidence: Windows setup copies a release byte for byte on a UTF-8 console

What was tested, how, and what came back, for a change to the Windows
setup's copy of a release into WSL. Every number here is read from a
file in the results bundle, which is attached to the release that
carries this change.

| | |
|---|---|
| Base commit | `8079e410` (`feat(agent): run a program in the Room from a chat (#431)`), with the three changes before this one (the image proof's `/proc`, the mount test's wait, `doctor` and a room disk in use) and their records |
| Tested tree | `7ad035ce7968b8062cd2b918e881ab09b4d403ae` |
| Date | 2026-11-13 (UTC) |
| Windows machine | Windows 11 Home 10.0.26300, Windows PowerShell 5.1.26100.9818, WSL 2.5.10.0 |
| Gate machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox; Rust 1.85.0 (minimum) and 1.98.1 (pinned) |
| Results bundle | `hatchling-windows-setup-utf8-console-results.tar.gz`, 170593 bytes, sha256 `866a81be3825afa1b917a0a2947c96ab50665cfcdd4f03b5268e6e806313a20a` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What was wrong

It was found by the acceptance run of the Room's run tool. The setup
script, run in a console whose input code page was UTF-8 (65001),
stopped at:

    A file differs from the release's SHA256SUMS: nothing was installed

Every file of the release had reached WSL three bytes longer than it was
sent. `Copy-BundledRelease`, in `installer/windows/common.ps1`, starts
`dd` in the distro as root and writes each file to its input. Windows
PowerShell gives a child its input through a writer in the console's
input encoding, and as it makes that writer it puts the encoding's
byte-order mark ahead of everything else. UTF-8's is three bytes. On a
console whose input is code page 437 or 1252 there is no mark, and the
files arrived whole.

A console's input is UTF-8 where Windows is set to use UTF-8 for every
program, or where the console was switched to it. On such a machine
setup could not install at all. It did refuse what arrived, as it
should: nothing was installed from a file that differed.

## What changed

- `installer/windows/common.ps1`: for the length of the copy, where the
  console's input encoding has a mark, it is set to UTF-8 with none, and
  put back after. Where it can't be set, the copy goes on as before and
  the checksums are still what refuses a file that arrives changed.
- `scripts/windows-release-copy-proof.ps1`, new: makes a small release
  (a megabyte of bytes, an empty file, a short text), copies it into a
  private directory of root's in a distro with the setup's own function,
  once with the console's input set to UTF-8 and once to code page 437,
  and compares the SHA-256 of what arrived with what was sent. It also
  checks the console's input encoding is afterwards what it was. The
  installer doesn't carry it.
- The changelog says it.

## The proof, and each part taken out by hand

On the Windows machine above, on the tested tree, copying into a WSL
distro. The command is the one at the top of the script.

| Run | Exit | What it said |
|---|---|---|
| Unbroken, before | 0 | "every file arrived the same, on both code pages" |
| The copy as it was | 1 | On UTF-8, all three files "DIFFERENT"; on 437, all three the same |
| The console's input not put back | 1 | Every file the same; "the console's input encoding was left changed" |
| Unbroken, after | 0 | "every file arrived the same, on both code pages" |

The copy as it was is the fault as setup met it.

By hand, before the script was written and so not in the bundle: the
copy as it was delivered the megabyte as 1048579 bytes and the empty
file as three; and the changed copy, with the console's input set to
code page 1252, delivered every file the same.

`windows/proof-unbroken-before.log` sha256
`b180c4cc33769e0446eb4dcdadb2c2ee1e13a52788892e022a732c5969cf20e4`;
`windows/proof-break-as-it-was.log` sha256
`9ebeaaa431b9c0d66cb82378308c9347f1477b0f9dd0f9964a41ee49d8f16ebe`;
`windows/proof-break-not-put-back.log` sha256
`f9b599cb59cbdcf9de5066f1beac2118ba0ccc5d91110e1b8d86ae03d92cd041`;
`windows/proof-unbroken-after.log` sha256
`a6971462ea54e67ef3be0e5679bab985df4314e18ce0bf50179d5ca4bbaaffc2`.

## Mutation

None to run: no Rust is changed.

## The gate

No Rust is changed, and the gate was run all the same, since the tree
is. In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3781 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3781 | 0 | 0 |

`gate.log` sha256
`fd7051e6ffcdd68c7eaffbf56cb1c49c41de0398cbe34772bb5ba1468ce31940`.

Both changed PowerShell files, and `setup.ps1`, parse with no errors.

## What this change does not do

- **Run the whole setup again on a UTF-8 console.** What failed there
  was this one function, and the proof calls it as setup does. The whole
  of it is to be seen at the next install drill, run with the console's
  input set to UTF-8.
- **Cover a setup with no console at all.** There the encoding can't be
  set, the copy is as it was, and the checksums refuse a changed file.
- **Run in CI.** The release's Windows job builds the setup and doesn't
  run it. The proof is run by hand on a Windows machine with WSL.
- **Say anything of PowerShell 7.** It was not on the test machine, and
  it is not what the installer runs.
