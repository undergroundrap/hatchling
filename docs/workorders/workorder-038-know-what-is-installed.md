# WO-038: Know what is installed

**Status:** CLOSED
**Area:** `crates/hatchling-broker` (a version command, `doctor`), `scripts/install.sh`, `packaging/install-room-image.sh`, `docs/updating.md`, `docs/install.md`
**Issue:** [#480](https://github.com/undergroundrap/hatchling/issues/480)
**Trust boundary:** yes (what runs as root at install, and the Room's image)

## Problem

An update on a real install (2026-11-14) left three gaps an owner can't work around:

- **No version command.** Nothing says which release, or which build, is installed. An owner can't tell whether an update took, or which notes apply to them.
- **`doctor` doesn't check the stores' modes.** A store whose directory isn't its owner's alone is refused when it's read, and the error comes from wherever reads it first. `doctor` should say so, and say how to fix it, before anything fails.
- **Only Windows Setup installs the Room's image.** An install from a release or from source on Linux has no way to install the image the agent's computer needs, so it has no computer, and `docs/updating.md` says so.

## Scope

**In scope:**

- `hatchling-broker --version` (and `hatchling --version`), saying the release, the commit and how it was built.
- A `doctor` check of every store's directory mode and owner, with the command that fixes each.
- Installing the Room's image, verified as Windows Setup verifies it, from a release on Linux, with `scripts/install.sh`.

**Not in scope:** the Room on aarch64; updating from inside the app.

## Acceptance criteria

- [x] `--version` names the release and commit on a release build and a source build, and CI checks it matches the tag. [version-command.md](../evidence/version-command.md): real release-profile builds said each, and the release workflow checks each program's words on every build; its dry run passed on #553's four builds. The tag's own version and `GITHUB_SHA` are checked by the same step on a tag, first run at v0.9.0's.
- [x] `doctor` names each store whose directory mode or owner is wrong, and the fix; a test makes each wrong and checks it. [store-modes.md](../evidence/store-modes.md): every directory a store refuses, with the `chmod`/`chown`; owners are checked when run by hand, and left to the service's start, which hands them over, when run as root.
- [x] A Linux install from a release can install the Room's image; it's admitted only when its digests match the release's, and a tampered or missing one is refused with a reason. [room-image-on-linux.md](../evidence/room-image-on-linux.md): the installer proof's release path and CI's install job; tampered stops the install, missing installs the rest and says why.
- [x] `docs/updating.md` and `docs/install.md` say how: which build is installed (slice 1), what `doctor`'s stores line means (slice 2), and the agent's computer from a release (slice 3).

## Gates

As [TEMPLATE.md](TEMPLATE.md), plus the installer proof (`scripts/room-installer-proof.sh`) on the Linux path.

## Evidence

- Slice 1, `--version` names the release, the commit and how it was built: [version-command.md](../evidence/version-command.md)
- Slice 2, `doctor` names each store directory its store would refuse, with the fix: [store-modes.md](../evidence/store-modes.md)
- Slice 3 and the close, the agent's computer's image from a release on Linux: [room-image-on-linux.md](../evidence/room-image-on-linux.md)
