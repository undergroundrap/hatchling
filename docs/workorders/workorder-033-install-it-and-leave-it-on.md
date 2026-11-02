# WO-033: Install it and leave it on

**Status:** IN-PROGRESS
**Area:** `packaging/`, `scripts/install.sh`, `crates/hatchling-broker` (configuration, a `doctor` command), `.github/workflows/`, `README.md`, `docs/`
**Issue:** none yet
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

- [ ] On a fresh Ubuntu 24.04, `curl` of `install.sh` and one `sudo sh install.sh` give a running broker and host that survive a reboot. The app answers at the address the script printed.
- [ ] Every file `install.sh` installs is checked against the release's `SHA256SUMS` first, and a file that differs stops it before anything is installed.
- [ ] The broker's whole configuration is `/etc/hatchling/hatchling.env`: no shell, no `id -u`.
- [ ] `hatchling-broker doctor` names each missing requirement and how to fix it, and exits non-zero when one is missing.
- [ ] A hosted model's key is put in the vault from a hidden prompt, and no plaintext copy is ever written (checked with a file-open trace).
- [ ] CI installs from the commit's own build, restarts the service, and gets an answer, on every pull request that touches packaging or the broker's configuration.
- [ ] The README's first screen says what Hatchling is and how to try it in three commands.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- The service runs the broker as root, as the README does: it drops to `hatchling-broker` once started. The vault keeper and the egress authority run as their own users. Nothing here weakens that.
- The owner, whose sockets these are, is the user who ran the installer (`SUDO_UID`). It's written as `HATCHLING_OWNER_UID`, so the service needs no `sudo`.
- Each broker start mints a new UI token, so the host must follow the broker: the host's unit restarts when the broker's does, or reads the token afresh.
