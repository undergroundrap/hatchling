# Testing on WSL2

How to run hatchling's full test suite, including the cell-boundary
tests, and a full `cargo mutants` pass on Windows through WSL2. Every
command below was run on the reference setup; the environment it was
verified on is recorded at the end.

## Why WSL2

The cell tests exercise real kernel isolation: user, mount and pid
namespaces, id maps, tmpfs + `pivot_root`, bind remounts, seccomp.
They need a real, non-nested Linux kernel.

- **WSL2** runs a real Linux kernel in the initial user namespace
  (`/proc/self/uid_map` is `0 0 4294967295`). The cell path runs for
  real here.
- **Nested sandboxes** (most cloud VMs/containers, uid_map like
  `0 131072 ...`) make `userns_available()` return false, so every
  `cell_boundary` test **silently passes by skipping**. A green
  baseline there says nothing about the cell code.
- **Docker** blocks namespace creation unless run `--privileged`, a
  sandbox inside a sandbox. Avoid it for these tests.
- **WSL1** has no Linux kernel. Unusable.

## 1. Install the distro (PowerShell)

```powershell
wsl --install -d Ubuntu --no-launch
```

`--no-launch` skips the interactive first-run user prompt; the user is
created below instead.

## 2. System packages and a non-root user (as root)

Run the tests as a normal user: several tests branch on `euid() == 0`,
and the unprivileged path is the one most hosts run.

```bash
# wsl -d Ubuntu -u root
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq build-essential cmake pkg-config curl ca-certificates strace
useradd -m -s /bin/bash <user>
printf '[user]\ndefault=<user>\n[boot]\nsystemd=true\n' > /etc/wsl.conf
```

`build-essential` is required: rustc needs `cc` as its linker, and
`aws-lc-sys` builds C. `strace` is for diagnosis (section 6).

With systemd on (Ubuntu's WSL default), **`/tmp` is a tmpfs mounted
`nosuid,nodev`**. That is deliberate here: it is the Ubuntu 25.04+
default and it exercises the remount path's locked-flag handling.
`/etc/wsl.conf` changes take effect after `wsl --terminate Ubuntu`.

### Harden the distro

Tests here run code you didn't write line by line, and by default
every WSL2 process can read and write your Windows drive at `/mnt/c`
and launch Windows programs. Append these sections to the
`/etc/wsl.conf` written above:

```ini
[automount]
enabled=false
[interop]
enabled=false
appendWindowsPath=false
```

Apply it with `wsl --terminate Ubuntu` (from PowerShell), then verify
inside WSL: `findmnt /mnt/c` prints nothing (`/mnt/c` is empty and not
mounted), and `command -v cmd.exe` prints nothing.

Do not add `mountFsTab=false`: in testing it also disabled the
`\\wsl.localhost` share you need to move files. Hatchling's tests do
not need the Windows side at all.

Reference: verified 2026-09-23 on WSL2. The full
suite was unchanged afterwards (491 passed, 0 failed with
`HATCHLING_REQUIRE_CELL=1`).

## 3. Toolchain (as the user)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
source ~/.cargo/env
rustup toolchain install 1.98.1 --profile minimal -c rustfmt,clippy
rustup default 1.98.1
cargo install cargo-mutants --version 27.1.0 --locked
```

## 4. Verify the kernel can run the cell path

```bash
cat /proc/self/uid_map          # must be: 0 0 4294967295 (identity)
unshare -Ur id                  # must print uid=0(root)
unshare -Urmpf --mount-proc true && echo full_ns_ok
grep NoNewPrivs /proc/self/status
findmnt -no FSTYPE,OPTIONS /tmp
uname -r; ldd --version | head -1
```

If the uid_map is not identity, stop: the cell tests will skip.

## 5. Get the tree and run the baseline

Clone inside WSL, into `~` — the Linux filesystem, never the 9p
`/mnt/c` mount (slow, different mount flags). The hardened distro
(section 2) can't see `C:`, but it still has the network:

```bash
git clone https://github.com/undergroundrap/hatchling ~/hatchling
cd ~/hatchling
HATCHLING_REQUIRE_CELL=1 cargo test --workspace --locked --no-fail-fast > ~/baseline.log 2>&1; echo "exit=$?"
grep -E '^test.* FAILED$' ~/baseline.log
```

`--no-fail-fast` matters: without it cargo stops at the first failing
test binary and every later crate's tests silently never run.

`HATCHLING_REQUIRE_CELL=1` turns the cell pipeline's environment
probes from silent skips into hard failures: if the kernel cannot run
the cell path, the baseline goes red instead of green-by-skipping.
This is what caught a real bug during validation (a test read
`geteuid()` after `unshare(CLONE_NEWUSER)`, when it already returns
the overflow uid — invisible under root, EPERM as uid 1000).

**Then prove the cell tests actually ran.** Run `cell_boundary`
serially (parallel runs can mask failures) and confirm the agent was
really exec'd inside a cell:

```bash
B=target/debug/deps/$(ls -t target/debug/deps/ | grep -E '^cell_boundary-[0-9a-f]+$' | head -1)
export HATCHLING_AGENT_BIN=$PWD/target/debug/hatchling-agent
"$B" --test-threads=1
strace -f -o /tmp/cell.trace "$B" cell_agent_handshake_reports_mapped_uid --exact --test-threads=1
grep -c 'execve("/agent"' /tmp/cell.trace     # must be >= 1
```

Error lines mentioning `llvm-cov` or a missing `Cargo.toml` under
`/tmp/hatchling-dev-mut-*` in the log are expected output from
`hatchling-dev`'s negative tests, not failures.

## 6. Diagnosing cell failures with strace

Trace the failing test with `strace -f -o /tmp/x.trace ...`, then:

- **Agent killed by seccomp**: find `+++ killed by SIGSYS +++`; the
  agent pid's *last syscall line* is the blocked one. strace shows its
  "return value" as the syscall number (e.g. `access(...) = 21`).
  Take numbers from `/usr/include/x86_64-linux-gnu/asm/unistd_64.h`,
  never from memory.
- **`mount(... MS_REMOUNT|MS_BIND ...) = -1 EPERM`**: the remount is
  dropping a flag the kernel locked on the source mount (nosuid,
  nodev, noexec, atime). Compare with the source's line in
  `/proc/self/mountinfo`.
- **`unshare(CLONE_NEWUSER) = -1 EINVAL`**: called from a
  multi-threaded process (any libtest test thread). Fork first.
- **Timeouts "waiting for the cell agent to connect"**: the child died
  before `connect`; look for a non-zero exit or signal on the cell pid.

## 7. Full mutation run

One machine, one pass, cargo-mutants' own baseline (it calibrates
timeouts for this machine).

If the change touches code that removes files or signals processes,
don't run this as your own user: a mutant of that code once deleted a
test user's whole WSL home. Run it in a sandbox, as
[Testing code that deletes](testing-destructive-code.md) describes.

```bash
cd ~/hatchling
mkdir -p ~/mutants.scratch
TMPDIR=$HOME/mutants.scratch HATCHLING_AGENT_BIN=$PWD/target/debug/hatchling-agent \
  HATCHLING_REQUIRE_CELL=1 \
  cargo +1.98.1 mutants --workspace --test-workspace=true --jobs 4
cp mutants.out/outcomes.json ~/outcomes.json
```

- `HATCHLING_REQUIRE_CELL=1` keeps the no-skip rule in force for the
  whole run: a mutant that breaks the cell pipeline must fail, not
  skip its way to "caught".
- Put `TMPDIR` on the ext4 disk, not `/tmp`: `/tmp` is RAM-backed
  tmpfs, and each job copies the workspace plus a ~1.4 GB build dir.
- Do not start mutation on a red baseline: with
  `--test-workspace=true` a test that always fails marks every mutant
  "caught".
- If a run is ever split across machines, shards are **zero-based**:
  `--shard 0/4` through `--shard 3/4`. `4/4` is invalid. Every
  machine must independently pass section 5 first.
- Each run renames the previous `mutants.out` to `mutants.out.old`.
- Keep Windows awake for the run; if the host sleeps, WSL suspends.

Results are reachable from Windows at
`\\wsl$\Ubuntu\home\<user>\`.

## Disk usage

WSL2 stores the distro in a single `ext4.vhdx` that grows to the peak
usage of the largest run and never shrinks by itself. A full mutation
pass (4 jobs, each with a workspace copy plus about 1.4 GB of build
output) pushed ours from about 5 GB to 20 GB. It plateaus rather than
grows with each run, because freed space is reused inside the disk.

Don't enable sparse mode: `wsl --manage <distro> --set-sparse true`
is refused on current WSL because of data-corruption risk. Don't
override it with `--allow-unsafe`.

To reclaim the space, with nothing running in WSL: run
`wsl --shutdown`, then in an Administrator PowerShell run `diskpart`
with `select vdisk file="<path>\ext4.vhdx"`, `attach vdisk readonly`,
`compact vdisk`, `detach vdisk`. The path comes from
`(Get-ChildItem HKCU:\Software\Microsoft\Windows\CurrentVersion\Lxss | Get-ItemProperty | Where-Object DistributionName -eq Ubuntu).BasePath`.
Note `wsl --shutdown` also stops Docker Desktop. Reference result:
19.97 GB → 9.18 GB.

Point `TMPDIR` for mutation runs at the ext4 disk, as section 7 says,
and delete the scratch directory after each run.

What grew it is usually build output: each workspace's `target/`, and
any copies of it a mutation run left. Delete those first (`cargo clean`,
or the directory), then compact; one compaction after months of gates
took ours from 179.6 GB to 16.0 GB. Deleting files frees space inside
the disk only, never on Windows, until it is compacted. Hatchling's own
leftovers go with `hatchling-broker clean`; for what is safe to delete,
and what is your data, see [What Hatchling keeps on disk](storage.md).

## Driving WSL from PowerShell

Inline `wsl -- bash -c "..."` breaks on `$`, quotes and globs as they
pass through PowerShell. Write the commands to a `.sh` file, copy it
in from the Windows side
(`Copy-Item script.sh \\wsl.localhost\Ubuntu\home\<user>\`), then run
`wsl -d Ubuntu -u <user> -- bash /home/<user>/script.sh`. (After the
"Hardening the distro" step `/mnt/c` does not exist inside WSL, so
running scripts straight off the Windows drive no longer works.)
Capture exit codes inside the script (`cmd; echo "exit=$?"`), not
through a pipe.

## Reference environment

Windows 11 26200, WSL 2.5.10, Ubuntu 26.04.1 LTS, kernel
6.6.87.2-microsoft-standard-WSL2, glibc 2.43, gcc 15.2, cmake 4.2.3,
rustc 1.98.1, cargo-mutants 27.1.0, 28 logical CPUs, 15 GB visible to
WSL, uid 1000, NoNewPrivs 0, `/tmp` tmpfs `nosuid,nodev`.
