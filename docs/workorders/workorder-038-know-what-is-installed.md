# WO-038: Know what is installed

**Status:** IN-PROGRESS
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

- [ ] `--version` names the release and commit on a release build and a source build, and CI checks it matches the tag.
- [ ] `doctor` names each store whose directory mode or owner is wrong, and the fix; a test makes each wrong and checks it.
- [ ] A Linux install from a release can install the Room's image; it's admitted only when its digests match the release's, and a tampered or missing one is refused with a reason.
- [ ] `docs/updating.md` and `docs/install.md` say how.

## Gates

As [TEMPLATE.md](TEMPLATE.md), plus the installer proof (`scripts/room-installer-proof.sh`) on the Linux path.

## Evidence

- Slice 1, `--version` names the release, the commit and how it was built: [version-command.md](../evidence/version-command.md)
