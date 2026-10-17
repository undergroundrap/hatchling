# Testing code that deletes

Mutation testing changes one operator at a time and runs the tests
against the result. For most code the worst a mutant can do is fail.
For code that deletes files or signals processes, a mutant can do the
damage the code was written to prevent, to whatever the test process
can reach. This guide covers how Hatchling tests that code, and why.

## What happened

On 2026-10-17, during the WO-012 mutation run on a WSL2 test box, a
mutant of `hatchling_sys::tree::remove_tree` flipped `&&` to `||` in
the walk's `.`/`..` name filter. The walk entered `..`, climbed out of
the test's scratch directory, and deleted nearly every file the test
user owned on the distro: its home directory, its toolchains, other
checkouts, and another agent's work in progress. Files that existed
only in WSL were lost for good.

What limited it:

- **The walk's second guard held.** The mount check kept it off
  every other filesystem, so `/tmp` (tmpfs) survived.
- **The distro was hardened.** With `/mnt/c` unmounted and interop
  off ([Harden the distro](testing-on-wsl2.md#harden-the-distro)),
  nothing on the Windows drive was reachable. Without that, a mutant
  that also broke the mount check could have reached it.
- **File permissions.** Files owned by root or other users survived,
  because unlinking needs write permission on the parent directory.

There was an earlier sign. In the run before, tests that ran the real
broker sweep over the shared `TMPDIR` let a mutant delete the other
mutation workers' workspace copies. Treat any unexplained missing file
during a mutation run as a containment failure, and stop the run.

## The rules

### 1. Two independent guards per escape

One flipped operator removes one guard. Every way out of the tree
(`..`, a symlink, a mount point, another user's directory, unbounded
depth) needs two checks that don't share code, so either alone keeps
the walk inside. `remove_tree` has:

- a name check: refuses empty names, `.`, `..` and anything with `/`;
- an identity check: never enters a directory whose (device, inode)
  it is already in or above, whatever its name;
- a mount check (`statx` mount id), an owner check, and a depth limit;
- `O_NOFOLLOW` on every open, working through handles, never paths.

Each guard needs a test that fails when only that guard is broken.
Prove it with a hand mutant for each (see
[Cell evidence and hand mutants](testing-patterns.md#cell-evidence-and-hand-mutants)).
A guard that another guard always shadows looks tested but isn't: the
WO-012 gate found exactly that in the sweep's listing, and a direct
test of the listing now pins it.

### 2. Tests delete only what they made

- A test of cleanup code gives the code its own `TMPDIR` and its own
  roots. Never let it sweep the shared temp dir: under mutation, that
  directory holds the other workers' copies of the workspace.
- A child process the test starts (a broker, a helper) gets a private
  `TMPDIR` too. It inherits the parent's otherwise.
- Cleanup decides by evidence, not by name alone: the broker's sweep
  needs the staging name, a dead owner pid, contents that are only
  cells' staging, and no process holding the directory's lock. Each
  check alone keeps unrelated directories safe.

### 3. Containment, not configuration

Changing `HOME` or `TMPDIR` isn't containment: the process can still
write everything its user can. Run tests, hand mutants and mutation
runs of deletion or signalling code where the kernel stops a runaway,
even with every guard mutated away:

- **A disposable machine or distro**, with nothing on it you can't
  rebuild. On Windows, a separate WSL2 distro just for this, hardened
  as in [Testing on WSL2](testing-on-wsl2.md#harden-the-distro), with
  automount and interop off.
- **A dedicated user**, with no sudo and no group shared with anyone.
  Don't give it uid 1000; Hatchling's tests treat that uid specially.
- **A read-only view of everything but the work directory.** Use
  bubblewrap with `/` bound read-only, private tmpfs over the shared
  writable places, the network unshared, and resource limits.

The wrapper below is the one the WO-012 gate ran in. It runs as root,
checks its inputs, and then drops to the sandbox user. Cells create
their own user namespaces, which they can't do inside bubblewrap's
unprivileged mode, so bubblewrap runs as root and `setpriv` drops
every privilege inside it.

```bash
#!/bin/bash
# sbx-job: run "$@" as $U with only $W writable.
set -eu
U=sbx                   # the sandbox user; its home holds the toolchain
H=/home/$U
W=$H/work               # the only writable directory
[ "$(id -u)" = 0 ] || { echo "run as root" >&2; exit 97; }
for d in "$W" "$W/scratch" "$H/.rustup" "$H/.cargo"; do
  if [ -L "$d" ] || [ ! -d "$d" ] || [ "$(stat -c %U "$d")" != "$U" ]; then
    echo "refusing: $d" >&2; exit 97
  fi
done
G=$((1024 * 1024 * 1024))
systemd-run --scope --quiet -p MemoryMax=10G -p TasksMax=16384 -- \
prlimit --core=1 -- \
bwrap --die-with-parent --new-session \
  --unshare-pid --unshare-ipc --unshare-uts --unshare-net --unshare-cgroup-try \
  --ro-bind / / \
  --dev /dev --proc /proc \
  --size $((4 * G)) --tmpfs /tmp \
  --size $G --tmpfs /var/tmp \
  --size $G --tmpfs /dev/shm \
  --size $((64 * 1024 * 1024)) --tmpfs /run \
  --size $((16 * 1024 * 1024)) --tmpfs /mnt \
  --bind "$W/scratch" /mnt/s \
  --size $((16 * 1024 * 1024)) --tmpfs /usr/lib/wsl \
  --size $((16 * 1024 * 1024)) --tmpfs /home \
  --perms 0755 --dir "$H" \
  --ro-bind "$H/.rustup" "$H/.rustup" \
  --ro-bind "$H/.cargo" "$H/.cargo" \
  --bind "$W" "$W" \
  --bind "$W/scratch" "$H/mutants.scratch" \
  --chmod 1777 /tmp --chmod 1777 /var/tmp --chmod 1777 /dev/shm --chmod 1777 "$H" \
  --setenv HOME "$H" --setenv TMPDIR /tmp \
  --setenv CARGO_HOME "$W/cargo-home" --setenv CARGO_NET_OFFLINE true \
  --setenv RUSTUP_HOME "$H/.rustup" \
  --setenv PATH "$H/.cargo/bin:/usr/local/bin:/usr/bin:/bin" \
  --chdir "$W" \
  setpriv --reuid="$U" --regid="$U" --clear-groups --no-new-privs \
    --inh-caps=-all --bounding-set=-all -- \
  "$@"
```

Things that bite inside it:

- **Unix socket paths are at most 108 bytes.** A long `TMPDIR` makes
  tests that bind sockets fail. Keep `TMPDIR=/tmp`, and bind the
  disk-backed mutation scratch at a short path (`/mnt/s` above).
- **No network means no DNS.** The broker resolves its allowlisted
  hosts at start. Bind a fixed, root-owned hosts file over
  `/etc/hosts` with the test hostnames that resolve in real DNS.
- **`$HOME` must be writable, and big enough.** Hatchling's
  `mutant-run` always puts its scratch in `$HOME/mutants.scratch`, so
  bind the disk-backed scratch there too.
- **A full disk is the remaining shared resource.** Watch the disk from
  outside the job, and stop the user's processes past a limit.
- **`cargo deny` needs the network** for its advisory database. Run it
  outside the sandbox, or leave it to CI when the lockfile hasn't
  changed.

Before trusting the sandbox, try to escape it from inside. Write to
the toolchain, `/etc` and `/usr`, delete from the read-only root, and
reach the network, the WSL interop socket and `/mnt/wsl`. Then list
every read-write mount and every writable socket. The only writable
places should be the work directory and the private tmpfs mounts.

### 4. Look for escapes, not just damage

Plant canary files next to the work: beside the checkout, and in the
mutation scratch next to the workspace copies. Check them after every
test step and every mutant, and stop on the first one missing. In
WO-012 the escape was found by its damage; a canary finds the mutant
that tried, even when the sandbox blocked it.

### 5. Keep nothing irreplaceable where tests run

The files lost on 2026-10-17 existed only inside the test distro.
Keep work trees and evidence where they're backed up, or export the
distro regularly (`wsl --export <distro> <file>`). Export the sandbox
itself once it's built and audited, so a damaged one can be replaced
in minutes.

## Where this applies

Any change that adds or changes code that removes files or
directories, kills or signals processes, or picks what to clean up:
run its tests, its hand mutants and its mutation run in the sandbox.
Review each `kill`/`killpg` path the same way: a pid of `0` or `-1`
reaching the syscall signals a whole process group or every process
the user owns. Check the pid where the syscall is made, not only
where it was constructed.
