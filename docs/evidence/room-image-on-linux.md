# Evidence: the agent's computer's image installs from a release on Linux (WO-038 slice 3)

What was tested, how, and what came back, for WO-038's last slice: a Linux
install from a release installs the image the agent's computer (the Room)
boots, admitted only as the release has it.

| | |
|---|---|
| Base commit | `5698962d` (`feat(doctor): name each store directory its store would refuse, with the fix (WO-038 slice 2) (#554)`) |
| Tested tree | `b82fcac7` |
| Date | 2026-11-20 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox distro: the proof and the hand-breaks as its root, `get.sh`'s tests as an unprivileged user |
| Tools | dash, bash, shellcheck, Python 3, `veritysetup` |

This record was added after the runs, in its own commit. It changes no code.

## The threat

The image is what the agent's computer boots: whoever chooses it chooses
what the agent runs on. Before this slice only Windows Setup installed it,
from its own root-private staging, under its own attestation; a Linux
install from a release had no agent's computer at all. Installed from a
release, the image comes through a folder the owner downloaded to, and
`install.sh` runs as root. So:

- **A changed image** (a tampered download, or a mirror's): it must not be
  admitted.
- **Another account on the machine** that can write the folder, or a file in
  it, could swap the image while root checks and copies it.
- **A swap after the check:** `install.sh` checks every file first, and
  copies the image later, once `veritysetup` is installed. A release swapped
  in between, with a matching manifest, must not get in.

The release's `SHA256SUMS` is the anchor, for the image as for the
programs `install.sh` already installs as root: it adds no new root of
trust. Whether `SHA256SUMS` is the release's is the owner's check, with the
release's attestation ([Verifying a release](../releasing.md#verifying-a-release)).

## What changed

- **`get-hatchling.sh`**, on x86_64, downloads the image's three files
  (`room-system-x86_64.squashfs`, `.verity`, `.root-hash`) when the
  release's `SHA256SUMS` lists the image, and checks them as it checks the
  rest: one that differs, or isn't listed, stops it before anything is
  installed. A release without the image (before 0.9.0) installs as before.
- **`scripts/install.sh`**, given a release whose `SHA256SUMS` lists the
  image, on x86_64:
  - before installing anything, requires each image file to be in
    `SHA256SUMS`, and the folder, `SHA256SUMS` and each image file to be
    root's or the owner's, with no write for anyone else;
  - after adding `cryptsetup-bin`, checks the image as below, and installs
    it after `doctor`, as Windows Setup's path does;
  - with an image file missing, or no `veritysetup`, says so, and installs
    the rest without the agent's computer, keeping any image it had.
- **`packaging/install-room-image.sh`** has two modes, `windows` and
  `release`. In both, the manifest is copied into a root-only stage first,
  and only that copy is read after. Each image file is copied into the
  stage, the copy hashed against it, and the hash tree verified with
  `veritysetup`. A failure leaves the image and record admitted before in
  place. In `release` mode, the folder and files are checked again as
  they're copied.
- **`scripts/room-installer-proof.sh`** proves the release path beside the
  Windows one. The release workflow runs it on each change, against the
  real image it builds.
- **CI's install job** ends with a release that has an image, installed
  with `--keep-model` (below).
- **`docs/install.md` and `docs/updating.md`** say how.

## Results

| Check | Result |
|---|---|
| `dash -n` on `install.sh`, `get.sh`, `install-room-image.sh`; `bash -n` on the proof | all 0 |
| shellcheck, warnings and above | the proof clean; the three `sh` scripts' findings are each script's own style (`VAR=` assignments, one `mkdir -p -m`), the new lines included, none new in kind |
| `get.sh`'s tests, systemd running (23, five new) | all passed |
| The installer proof, as root, Windows and release paths (below) | passed, no leftovers |
| Hand-breaks | 10 broken, 10 caught |

### The installer proof

`scripts/room-installer-proof.sh`, as root, in private mount and network
namespaces, against a fixture image with a real hash tree (`veritysetup
format`; the proof verifies the tree and never mounts the image). The
release workflow runs the same proof against the real image.

The Windows path, as before: a verified image admitted with fresh inodes; a
wrong root hash, a manifest that doesn't match, and a duplicated entry
refused, with the admitted image and record untouched; an interrupted
publication left admission closed, and a retry restored it.

The release path, from a folder owned by an unprivileged uid, as
`get-hatchling.sh` leaves it:

| The release | What happened |
|---|---|
| As released | its image admitted, record and files as built |
| The image changed | refused: differs from the release's `SHA256SUMS` |
| A wrong root hash, with a manifest to match | refused: the hash tree fails verification |
| The hash tree missing | refused: missing a regular file |
| The hash tree not in `SHA256SUMS` | refused: needs one checksum |
| The folder open to all (777) | refused: nobody else's to write |
| `SHA256SUMS` writable by all | refused |
| The image owned by another uid | refused |
| A link in place of the image | refused: missing a regular file |
| The image and `SHA256SUMS` swapped once the manifest was staged | refused: the image differs from the staged manifest |
| Each of those | the image and record admitted before untouched |
| Put right | admitted again |

### Hand-breaks

| Break | Caught by |
|---|---|
| The manifest read from the release each time, not the staged copy | the proof's swap after staging |
| A folder or file others may write, taken | the proof's open folder and manifest |
| Another uid's folder or file, taken | the proof's other uid |
| The image files' own owners and modes not checked | the proof |
| A copy that differs from the manifest, taken | the proof's changed image |
| The hash tree not verified | the proof's wrong root hash |
| A link in place of an image file, followed | the proof's link |
| A manifest others may write, taken | the proof's open manifest |
| `get-hatchling.sh` downloads the image and doesn't list-check it | `test_an_image_file_not_in_sha256sums_stops_it` |
| `get-hatchling.sh` never downloads the image | `test_on_x86_64_the_agents_computer_comes_with_the_release_and_is_checked` |

**Found by these runs:**
- **An image file the manifest doesn't list.** The first hand-break run let
  `get-hatchling.sh` skip its list check of the image's files and no test
  failed: its final `sha256sum --check` still caught a changed file. It
  showed a release listing the image and not its hash tree would reach
  `install.sh`, which would refuse it only after installing the programs.
  Both now refuse it first, and a test and CI make it so.
- **A test run that skipped.** `get.sh`'s tests skip without systemd; in
  the sandbox's jail 20 of 22 skipped, and passed. They were run again
  where systemd is visible, and all ran.

## In CI, on this pull request

- **The install job's last step**, on a real runner: a release with a
  stand-in image installed with `--keep-model` admits it, record and file;
  a changed image, a folder open to all, and an image file not in
  `SHA256SUMS` each stop the install before anything is installed; an
  image file missing installs the rest, says so, and keeps the image
  admitted before.
- **The release workflow's dry run** runs the installer proof against the
  real image.

## Residual

- **`install.sh`'s wiring** (which release gets the image, and when) runs
  only in a real install: it is proven by CI's install job, not hand-broken
  here.
- **The owner's own account** can change the folder while `install.sh`
  runs, as it can the programs: it is the account that runs `sudo`.
- **An older release's image** with matching sums is admitted: the owner
  chose that release.
- **aarch64** has no image yet.
