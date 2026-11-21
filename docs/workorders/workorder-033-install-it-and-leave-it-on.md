# WO-033: Install it and leave it on

**Status:** IN-PROGRESS
**Area:** `packaging/`, `scripts/install.sh`, `crates/hatchling-broker` (configuration, a `doctor` command), `.github/workflows/`, `README.md`, `docs/`
**Issue:** [#482](https://github.com/undergroundrap/hatchling/issues/482)
**Trust boundary:** yes (the broker's configuration, and what runs as root at install)

## Problem

Hatchling is an always-on agent that can't yet be installed to stay on. Getting from nothing to a first answer takes about 34 commands:
- build or download the programs, build the app with Node 22;
- create four system users;
- export 17 variables in a root shell, seal the vault, and start the broker by hand.

Nothing makes it a service. A logout, or a reboot, ends it and loses its configuration, which is only ever in that shell. Every competitor installs in one or two commands and runs as a daemon. Nobody adopts a tool with that much friction, however much safer it is.

The post-0.5.0 audit's adoption lane rated this its one critical finding (ADO-01), with:
- the app not shipped in releases (ADO-02);
- the README's root quickstart breaking on `npm` under `sudo -s` (ADO-03);
- a hosted model's key imported by writing it in plaintext first (ADO-10).

## Scope

**In scope:**

- **The release ships the app** (`hatchling-ui.tar.gz`), built reproducibly (#279).
- **A plain configuration file.**
  - `HATCHLING_AGENT_UID` takes a user's name, and the cells' map base defaults to it (#280).
  - `HATCHLING_OWNER_UID` takes a name too.
  - Everything a service needs fits in a static `/etc/hatchling/hatchling.env`.
- **Packaging files**, in `packaging/`:
  - a systemd unit for the broker (root, `Delegate=yes`, restart on failure);
  - a user unit for the app's host;
  - `sysusers.d` entries for the four users;
  - `tmpfiles.d` entries for the state directories, with their owners and modes;
  - an annotated `hatchling.env`.
- **`scripts/install.sh`:**
  - downloads a release's files and checks each against the release's `SHA256SUMS`, and against its attestation when `gh` is there; or takes a directory of files already built;
  - installs them, the units and the configuration;
  - makes the users and directories, seals the vault, and starts the services;
  - asks before overwriting a configuration that exists; prints the app's address at the end.
- **`hatchling-broker doctor`.** It checks what the broker needs before it starts, and says how to fix each thing:
  - the kernel version;
  - user namespaces (and Ubuntu's AppArmor restriction on them);
  - seccomp, Landlock, cgroup v2 and `cgroup.kill`;
  - the users, the directories' owners and modes, and the configuration.
- **A hosted model's key at a hidden prompt**, sealed straight into the vault, with no plaintext file.
- **CI installs it:**
  - on a fresh Ubuntu 24.04 runner, with systemd, from the build of the commit under test;
  - then restarts the service and gets an answer through the API;
  - the README quickstart job runs as written on a stock machine (no `PATH` pass-through, the AppArmor restriction left on).
- **Docs:**
  - `docs/install.md`;
  - the README's "try it" in three commands near the top;
  - the WSL2 page for users, apart from the contributors' test-box page.

**Not in scope:**
- deb, rpm and other distribution packages (a later WO, once this layout settles);
- macOS;
- a remote channel to reach the agent;
- automatic updates.

## Acceptance criteria

Reconciled against what shipped on 2026-11-16, after the work had gone out in 0.6.0 to 0.8.0 without this work order being closed. Each item is ticked only where the evidence proves it.

- [x] **On a fresh Ubuntu 24.04, one command gives a running broker and host that survive a reboot, and the app answers at the address printed.** CI's `service` job (every pull request, Ubuntu 24.04) installs this commit's build, laid out as a release, with `sh get-hatchling.sh --from-url file://...`, checks the printed address answers, gets an answer from the agent, restarts `hatchling.service` and gets one again. The reboot: a real install from a release build on Ubuntu 24.04.5 in WSL, rebooted (the distro's systemd started from nothing), came back with both services started at boot, `doctor` clean, the app's address answering and the agent answering ([real-install-drill.md](../evidence/real-install-drill.md)). A physical machine's reboot wasn't done. The shipped command is `curl` of `get-hatchling.sh`, run as the user, not `sudo sh install.sh` as first written.
- [x] **Every file is checked against the release's `SHA256SUMS` first, and one that differs stops it before anything is installed.** `scripts/test_get.py` (`test_a_file_that_differs_stops_it_before_anything_is_installed`, `test_a_file_not_in_sha256sums_stops_it`, the installer tarball's refusals), in the `service` job on every pull request; #351 broke each defence by hand. Not done from the scope: verifying the release's attestation (`get.sh` prints the `gh attestation verify` command); a local build with no `SHA256SUMS` isn't checked.
- [x] **The broker's whole configuration is `/etc/hatchling/hatchling.env`: no shell, no `id -u`.** The service reads it with `EnvironmentFile`, the file is plain `KEY=value`, `HATCHLING_AGENT_UID` takes a name (#280), operator commands read it and refuse it when anyone but root can write it (#290, a CI step), and `HATCHLING_OWNER_UID` takes a name too ([owner-by-name.md](../evidence/owner-by-name.md)). `HATCHLING_CELL_CGROUP` stays in the service's drop-in, beside the delegation that makes it true, which is installed only where systemd has it: it isn't a setting the owner chooses (the same record says why).
- [ ] **`hatchling-broker doctor` names each missing requirement and how to fix it, and exits non-zero when one is missing.** Partly proven: exit 1 and a fix for each problem (#287, mutation 105 caught, 0 missed); CI checks it clean, then names a stopped model, and names AppArmor's refusal in the `rootless` job. **Not yet:** the kernel version, seccomp, Landlock, the system users, and directories' owners and modes, which the scope listed ([WO-037](workorder-037-say-when-a-protection-is-off.md) and [WO-038](workorder-038-know-what-is-installed.md) take the last).
- [x] **A hosted model's key goes into the vault from a hidden prompt, and no plaintext copy is ever written (checked with a file-open trace).** `secret add` reads with echo off or from a pipe and seals at once (#289); CI pipes a key through the installer, checks the vault holds it, and finds its text nowhere under `/etc/hatchling` or `/var/lib/hatchling`. The trace, on a real install: the kernel recorded every file any process opened to write, renamed or linked while a piped stand-in key was sealed (a temporary file renamed onto the sealed entry, and a line of the activity log), and none of the distro's 37,024 files, nor the journal, held the key afterwards ([real-install-drill.md](../evidence/real-install-drill.md)). The prompt with echo off reads the same line by another route and wasn't traced.
- [x] **CI installs from the commit's own build, restarts the service and gets an answer, on every pull request that touches packaging or the broker's configuration.** The `service` job, on every pull request; the `upgrade` job also upgrades from the last release and checks nothing kept is lost (#310, #336).
- [x] **The README's first screen says what Hatchling is and how to try it in three commands.** README.md's opening and its "Try it" line (#298).

## What shipped

#281 (this work order), #282 (packaging, `install.sh`, the `service` job), #287 (`doctor`), #289 (`secret add`), #290 (operator commands take the service's settings), and without naming it: #279 (the app in releases), #280 (user names), #298 (the README), #310 and #336 (the upgrade job), #319, #340, #348, #351 (`get-hatchling.sh`), #352 (a hosted model at install), #353 (the app's host as a confined system service), #397, #423 (installs with no cgroup controllers), #439 (updating); Windows Setup in #367–#371 and #435.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- The service runs the broker as root, as the README does: it drops to `hatchling-broker` once started. The vault keeper and the egress authority run as their own users. Nothing here weakens that.
- The owner, whose sockets these are, is the user who ran the installer (`SUDO_UID`). It's written as `HATCHLING_OWNER_UID`, so the service needs no `sudo`.
- Each broker start mints a new UI token, so the host must follow the broker: the host's unit restarts when the broker's does, or reads the token afresh.
