# Install Hatchling as a service

This sets Hatchling up as a service that starts at boot, from a release, in a few commands:
- the broker, run as root;
- the app's host, run as you.

To try it by hand first, the [README's quickstart](../README.md#quickstart) runs the same pieces in a shell.

## What you need

- **Linux with systemd**, kernel 5.11 or later (or WSL2 with systemd on), on x86_64 or aarch64, with glibc 2.34 or later: Ubuntu 22.04, Debian 12, Fedora, RHEL 9 and their successors.
- **A model it can reach:** an OpenAI-compatible server on this machine. [Ollama](https://ollama.com) is the simplest: install it, then `ollama pull llama3.2`. A hosted API comes after; see [Using a hosted model](hosted-models.md).
- **The GitHub CLI**, to download and verify the release (or download its files from the release page).

## Install

From a clone of the version you install, download its release into `release/` and install from it. `scripts/install.sh` checks every file against the release's `SHA256SUMS` before installing anything.

```bash
git clone --branch v0.6.0 https://github.com/undergroundrap/hatchling && cd hatchling
```

```bash
gh release download v0.6.0 --repo undergroundrap/hatchling --dir release
```

```bash
sudo scripts/install.sh --from release --model-endpoint http://localhost:11434/v1 --model llama3.2
```

Before it starts anything, it runs `hatchling-broker doctor`, which checks the settings, that this machine can make the agent's cells, and that the model's server answers with the model named. If one isn't so, it stops and says what to do. Run it again after: it keeps your configuration.

It ends with the app's address. Open it in a browser on this machine, and say something. **The address is a key to your agent: don't share it.** Before you install, you can also check that the release's files were built by this project's release workflow: [Verifying a release](releasing.md#verifying-a-release).

**From your own build** instead of a release:
1. Build the programs: `cargo build --release`.
2. Build the app: `cd frontend && npm ci && npm run build`.
3. Put `hatchling-broker`, `hatchling-agent`, `hatchling-host` and `hatchling-browser` (from `target/release`) and the app's `dist`, renamed `ui`, in one directory.
4. Pass that directory as `--from`. With no `SHA256SUMS` beside them, the files aren't checked.

## What it sets up

| What | Where |
| --- | --- |
| The programs | `/usr/local/bin/hatchling-broker`, `-agent`, `-host`, `-browser` |
| The app | `/usr/local/share/hatchling/ui` |
| The configuration, the whole of it | `/etc/hatchling/hatchling.env`: every setting is in `hatchling-broker --help` |
| The broker's service, run as root, dropping to `hatchling-broker` once started | `hatchling.service` |
| The app's host, as your own user service, running without a login (lingering) | `hatchling-host.service` |
| Four system users: the cells', the broker's, the vault keeper's, the egress authority's | `/etc/sysusers.d/hatchling.conf` |
| Its state: the vault, schedules, sockets | `/var/lib/hatchling` ([what's kept](storage.md)) |

With systemd 254 or later, the broker gets a cgroup of its own to give each cell, so each cell's processes, memory and CPU are bounded as a whole.

## Using it

- **The app's address again**, as yourself: `hatchling-host url`. Each start of the broker makes a new one, so after a restart an open app goes offline; open the new address.

- **Something not working?** `sudo hatchling-broker doctor` checks what a start trips on, and says how to fix each problem it finds.
- **Change a setting:** edit `/etc/hatchling/hatchling.env`, then `sudo systemctl restart hatchling`.
- **The broker's other commands** (`hatchling-broker --help` lists them) take the service's settings when you run them with `sudo` from a shell that has none, so `sudo hatchling-broker tools list` just works. They read the file only while it's root's and nobody else can write it, as the installer leaves it: it says what the broker runs.
- **The logs:** `journalctl -u hatchling` for the broker, `journalctl --user -u hatchling-host` for the app's host.
- **Stop it:** `sudo systemctl stop hatchling`. **Start it again:** `sudo systemctl start hatchling`.
- **Update:** clone the new version, download its release, and run `scripts/install.sh` again. It keeps your configuration, unless you pass `--force`. Read the version's [changelog](../CHANGELOG.md) first: before 1.0, a minor version may change the configuration.

## Uninstall

1. Stop and remove the services:

   ```bash
   sudo systemctl disable --now hatchling
   ```

   ```bash
   systemctl --user disable --now hatchling-host
   ```

2. Remove what was installed:

   ```bash
   sudo rm -rf /usr/local/bin/hatchling-broker /usr/local/bin/hatchling-agent /usr/local/bin/hatchling-host /usr/local/bin/hatchling-browser /usr/local/share/hatchling /etc/systemd/system/hatchling.service /etc/systemd/system/hatchling.service.d /etc/systemd/user/hatchling-host.service
   ```

3. **Your data is still there**, in `/var/lib/hatchling` and `/etc/hatchling`. [What Hatchling keeps on disk](storage.md#uninstall) says what each part is, how to back it up, and how to remove it with the users.
