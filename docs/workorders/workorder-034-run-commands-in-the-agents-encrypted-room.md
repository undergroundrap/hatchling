# WO-034: Run commands in the agent's encrypted room

**Status:** CLOSED
**Area:** `crates/hatchling-sys` (the Room's syscall profile, mounts, image admission), `crates/hatchling-broker` (the spawner's Room: `room_launch`, `room_session`, `room_job`, `room_disk`), `crates/hatchling-room` (the supervisor), `crates/hatchling-core` (`room`, the supervisor's wire), `scripts/room-*.sh`, `packaging/install-room-image.sh`, `.github/workflows/release.yml`
**Issue:** [#394](https://github.com/undergroundrap/hatchling/issues/394)
**Trust boundary:** yes

This file was written after most of the work shipped, from the tracking issue, the pull requests and their evidence records, when the queue was reconciled (2026-11-16). The scope and acceptance criteria are the issue's, unchanged.

## Problem

Step 2 of the 0.9 Room work: give the agent a room of its own that can run a pinned program against its encrypted workspace and keep files between room sessions, building on the encrypted room disk (step 1, #388–#392).

## Scope

**In scope** (as #394 states it):

- The room's own identity, syscall policy, mounts and mandatory resource limits. Room jobs, files and supervisor replies are untrusted.
- A pinned, read-only x86_64 system image, built and verified, with its digest, manifest, SBOM and provenance recorded, and verified in the installer and at mount time.
- The existing keeper and spawner for the disk's and the session's lifecycle. Room jobs drained before disk maintenance. Configured disk capacity reserved, with closed-disk, grow-only expansion verified.
- The internal room file boundary and the scoped broker command path needed to prove execution and persistence.

**Not in scope:** the agent-facing tools (run, read, search, write and edit from a chat: [WO-035](workorder-035-the-agents-tools-in-its-own-computer.md)), owner file transfers, and turning the Room on by default.

## Acceptance criteria

Each item is ticked only where the evidence below proves it.

- [x] **Install a hash-identified image in disposable Linux or WSL2; run a pinned program through the real broker, spawner, keeper and encrypted workspace; workspace bytes survive restart and session scratch is discarded.**
  - the offline install of the CI-built image (`scripts/room-installer-proof.sh`, in the release workflow's dry run);
  - workspace bytes surviving a disk close and reopen, and fresh scratch for each new Room (`check_workspace_persists` in the installed test `the_installed_spawner_hands_the_verified_image_to_its_mount_helper`, `crates/hatchling-broker/tests/room_disk.rs`, every pull request as root; `scripts/room-mount-proof.sh full`);
  - a run through the real broker on a real install, from the release image and a local model ([room-run-tool.md](../evidence/room-run-tool.md));
  - on a disposable WSL install from a release build, its verified image installed by Setup: a file given to the agent and one written in the Room kept their SHA-256 across a service restart and a reboot, and the session's scratch was gone after each ([real-install-drill.md](../evidence/real-install-drill.md)). The installed test still uses a fixture image and calls the spawner directly; the drill is the release image's proof.
- [x] **The room's identity, mount flags, syscall policy, namespaces and cgroup limits verified in a live room; hostile inputs exercised; each new guard fails its test when removed.**
  - identity (`tests/fixtures/room-admission-probe.c`, inside the installed test, #412);
  - mount flags (`scripts/room-mount-proof.sh`, `scripts/room-image-probe.sh`);
  - syscall policy (`the_kernel_enforces_the_room_profile` and the Room's other `seccomp.rs` tests in the gate; the image's 127 commands under its filter, `scripts/room-image-tools-proof.sh`);
  - cgroup limits read before the Room starts (#411);
  - PID and mount namespaces (#403);
  - hand-breaks recorded in #395, #400, #402, #404–#407, #411–#414, #416–#418 and #425.
  - on a real install, a live Room's network, IPC, PID and mount namespaces were its own, not the host's or the broker's, with no network interfaces and `ipcs` killed by the filter ([real-install-drill.md](../evidence/real-install-drill.md)).
  - the network and IPC namespaces asserted in a live Room by the admission probe, on every pull request as root; the supervisor started without either fails the installed test ([room-own-namespaces.md](../evidence/room-own-namespaces.md)).
- **Moved to [WO-040](workorder-040-room-disk-capacity.md)** (the maintainer's decision, 2026-11-16): ~~Real capacity reservation, existing workspace bytes preserved, closed-disk growth with interruption recovery, and a visible refusal at the free-space boundary.~~ It was never built. The disk is a fixed-size sparse file; nothing reserves it or grows it. Workspace bytes are preserved through layout preparation (#405) and rekey (a canary test in `room_disk.rs`). The only free-space refusal is at creation.
- **Proven in part; the rest moved to [WO-044](workorder-044-the-rooms-work-outlives-a-restart.md)** (the maintainer's decision, 2026-11-21): ~~Session expiry, visible failure receipts for interrupted work, and jobs drained before lock, wipe or rekey.~~ Proven here:
  - expiry after an hour and after fifteen idle minutes (`room_session` tests; the installed deadline modes);
  - the session ended before a close or wipe (`closing_or_wiping_the_disk_ends_the_session_first`, and the installed test);
  - admitted work holding off rekey and enrollment (#416);
  - a job lost with its Room says so (`a_command_lost_with_its_room_is_over`);
  - on a real install, closing the room disk six seconds into a running job was refused, and the job finished ([real-install-drill.md](../evidence/real-install-drill.md)).
  - **Moved to WO-044, never built or drilled:** lock with work running, on an install whose vault a security key protects (it's drilled only with stand-ins, and on installs with nothing running, #424); the job table, held in memory and lost if the spawner restarts; receipts for interrupted work that outlive the spawner.
- **Proven in part; the attestation moved to the release's own check** (the maintainer's decision, 2026-11-21): ~~Two independent image builds compared, provenance verified, a tampered or missing image rejected.~~ Proven here:
  - the release workflow builds the image twice and compares them (`room-image-verified`);
  - a tampered or missing image is refused (`scripts/room-image-probe.sh`; `room_image_admission_pins_verifies_and_releases_the_real_image`, every pull request as root).
  - a Linux install from a release installs the image, admitted only when its digests match the release's `SHA256SUMS`, which `install.sh` verifies against the release's attestation when `gh` is there ([room-image-on-linux.md](../evidence/room-image-on-linux.md), [WO-038](workorder-038-know-what-is-installed.md)).
  - **Moved to the release:** the image's attestation is made only on a tag, and no tag has been cut since the image existed. Checking it is now a step of [Cutting a release](../releasing.md#cutting-a-release): the first release with the image verifies the image and its root hash with `gh attestation verify`.

## What shipped

- **The Room cell** (#395–#418, which reference #394): the syscall filter, the image's build and verification, the supervisor, the mount plan, workspace provenance, the root bound to its verified device, encrypted directories, image admission, the offline installer bundle, cleanup retries, mandatory cgroup limits, the confined supervisor, the writable workspace, commands through the installed spawner, key maintenance held off during admitted work, the Room ending with its spawner, and the Room kept between commands.
- **Step 2 work that didn't reference #394:** #424 (the room disk closes where the root mount isn't shared), #425 (named jobs), #427–#429, #432 and #440 (the image's own programs under the filter), and the internal file boundary: #436 (a page of a file), #452 (search) and #457 (write and edit).
- **The supervisor's own crate:** #475.

## Known gaps, said where they were found

- A killed spawner leaves the image mapping, two loop devices and an empty cgroup behind; nothing sweeps them at the next start (#417; [wo-034-room-session.md](../evidence/wo-034-room-session.md)).
- Python's process pools need `/dev/shm`, which the Room doesn't have.
- The image is x86_64 only, and only Windows Setup installs it.
- A process that calls `setsid` outlives its job until the session ends (#418, #425).
- Operator key maintenance is refused for up to fifteen minutes after the last command (#418).

## Gates

As [TEMPLATE.md](TEMPLATE.md), plus the root pass and the release workflow's Room jobs.

## Evidence

Each pull request carries its own; the records are in [docs/evidence/](../evidence/): `wo-034-room-session.md`, `room-run-jobs.md`, `room-file-page.md`, `room-search-wire.md`, `room-write-wire.md`, `room-filter-*.md`, `room-image-proof-proc.md`, `room-ls-long.md`, `room-disk-*.md`, `room-job-path.md` and `room-crate.md`. Closed 2026-11-21, with the first two items proven, and of the last two what was proven said, the rest moved: capacity and growth to WO-040, which built them; lock with work running, kept job records and receipts to WO-044; the image's attestation on a real tag to the release's own check. Also: [real-install-drill.md](../evidence/real-install-drill.md) and [room-own-namespaces.md](../evidence/room-own-namespaces.md).
