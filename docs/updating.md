# Updating Hatchling

An update replaces Hatchling's programs and its app. Your settings, your model, the vault and everything the agent has stay as they are. This page is for anyone who updates an install: its owner, and a maintainer or collaborator trying a build before it's released.

Three things are true of every update:

- **Hatchling restarts**, so anything it's working on stops.
- **A vault protected by a security key or an authenticator app is locked afterwards**, as after any restart, until you unlock it.
- **It never updates by itself.** Hatchling sends nothing out on its own, so it doesn't look for new versions. They're on [the releases page](https://github.com/undergroundrap/hatchling/releases).

## Update the way you installed

| You installed with | Update with |
| --- | --- |
| Windows Setup | The new Setup: [Hatchling on Windows](windows.md#updating) |
| A release, from a Linux or WSL terminal | That release's files and `install.sh --keep-model`: [below](#from-a-release-in-a-terminal) |
| Your own build, or a build from CI | The same installer, pointed at that build: [below](#from-your-own-build-or-one-from-ci) |

Don't cross them. Windows Setup makes an account of its own in the distro and installs for it, so run on an install that a terminal made, it would set Hatchling up for a different owner.

## Before you update

1. **Read the version's [changelog](../CHANGELOG.md).** Before 1.0, a minor version may change the configuration.
2. **Make a safety copy** ([next section](#a-safety-copy)). It costs a minute, and it's the way back if the new version doesn't suit you.
3. **Have what unlocks your vault to hand**: your security key and its PIN, or your authenticator app.

## A safety copy

There are two kinds, and they bring back different things.

**A backup** is Hatchling's own: one small sealed file with the vault and what the agent has. It opens only with what opens your vault, so it's safe to keep anywhere. It doesn't hold the programs, your settings file, your model or the agent's room disk. [Back up, restore, and start over](backup-and-reset.md) has the rest.

```bash
sudo hatchling-broker backup
```

**A snapshot** is WSL's, on Windows only: the whole distro in one file, with the programs, settings, vault, room disk and models, exactly as they were. It's what puts an install back as if the update never ran. In PowerShell, with your distro's name (`wsl -l -v` lists them):

```powershell
wsl --terminate hatchling-wsl
```

```powershell
wsl --export hatchling-wsl C:\Users\<you>\hatchling-before-update.tar
```

What to know about a snapshot:

- **It's as large as what the distro holds**, models included. An install with two local models came to 18 GB, and took a minute and a half. It's one file, not a second distro, and deleting it gives the space back at once.
- **Stopping the distro locks a protected vault**, as any restart does.
- **It holds your vault.** With a security key or an authenticator app, the copy is as locked as the original. At the key file tier the key file is in it, so whoever has the snapshot can open the vault: keep it on this PC, and delete it once the update has proved itself.
- **Putting it back loses everything since.** It replaces the distro: `wsl --unregister hatchling-wsl` deletes the one that's there, for good, and `wsl --import hatchling-wsl <a folder for its disk> <the snapshot file>` brings the old one back under the same name. If it then opens as root, name your user in its `/etc/wsl.conf` (`[user]`, `default=<you>`).
- **Don't import it beside the original** under another name to compare: two installs can't run at once ([run one copy at a time](windows.md#run-one-copy-at-a-time)).

On Linux outside WSL, use your own disk or VM snapshots for the same thing.

## From a release, in a terminal

From a clone of the version you're updating to, download its release and run the installer with `--keep-model`, as the user who owns the install:

```bash
git -c advice.detachedHead=false clone --branch vX.Y.Z https://github.com/undergroundrap/hatchling && cd hatchling
```

```bash
gh release download vX.Y.Z --repo undergroundrap/hatchling --dir release
```

```bash
sudo scripts/install.sh --from release --keep-model
```

It checks every file against the release's `SHA256SUMS` before it changes anything, and a file that differs stops it there. Then it replaces the programs and the app, runs `doctor`, and restarts the services. To check first that the files were built by this project's release workflow, see [Verifying a release](releasing.md#verifying-a-release).

`--keep-model` takes no model and no `--force`: it leaves your configuration, your model and its server alone. When the new version keeps something by default that your configuration doesn't name at all, it adds that setting, copies the old file beside it (`hatchling.env.before-<date>`), and says so. A setting you set empty stays off.

**The agent's computer** (from 0.9) needs a system image, and today only Windows Setup installs it. An install updated from a terminal keeps working as it did, without the agent's computer.

## From your own build, or one from CI

This is for trying a change before it's released. Nothing here is attested: only a release is. You're trusting the commit you built, or the workflow run you chose.

**Your own build:** put the four programs and the app in one directory, as [Install Hatchling as a service](install.md#switching-models) describes under "From your own build", and pass it as `--from` with `--keep-model`. With no `SHA256SUMS` beside them, the files aren't checked.

**A build from CI:** every push to `main`, and every pull request that changes how a release is built, runs the release workflow as a dry run. It builds everything a release has, twice, compares the two builds byte for byte, and releases nothing. Its artifacts stay on the run:

| Artifact | Holds |
| --- | --- |
| `build-x86_64-unknown-linux-gnu-a` (and `aarch64`) | The programs, and `build-info-<target>.txt`, which names the commit |
| `ui-a` | The app, `hatchling-ui.tar.gz` |
| `room-image-verified` | The agent's computer's image, its hash tree and root hash |

```bash
gh run download <run id> --repo undergroundrap/hatchling -n build-x86_64-unknown-linux-gnu-a -n ui-a --dir ci-build
```

Each artifact lands in a folder of its own. Wait for the run's comparison jobs to pass before you use it, and check the commit in `build-info` is the one you mean to test. Put the programs and `hatchling-ui.tar.gz` in one directory and install from it with `--keep-model`, from a clone at that same commit, so the installer matches the programs.

## After an update

- **It says so.** The installer ends with the app's address, or with "Hatchling is up to date, and locked" and the command that unlocks it: `sudo hatchling-broker unlock`.
- **`sudo hatchling-broker doctor`** checks what a start trips on. The installer runs it before it restarts anything, and stops if it finds a problem; run it again whenever something seems off.
- **Open the app** and say something. The app's address changes with every restart, so an app left open goes offline: open it again.
- **Which build is installed?** There's no version command yet. Hash the installed program and look for it in the release's `SHA256SUMS`:

  ```bash
  sha256sum /usr/local/bin/hatchling-broker
  ```

## If it went wrong

- **It stopped before installing.** A file that differs from `SHA256SUMS`, or a problem `doctor` found, stops the update with the old programs or the old configuration still in place. Fix what it names and run it again: it carries on.
- **It installed, and something is off.** `journalctl -u hatchling` has the broker's log. Your earlier configuration is beside the current one, as `hatchling.env.before-<date>`.
- **You want the old version back.** Install the old release the same way: before 1.0 that isn't promised to work once the new version has run, because it may have moved Hatchling's data on. A snapshot is the sure way back. A backup brings back the vault and what the agent has, into whichever version is installed.

## Disk space

- **An update doesn't keep the old version.** It replaces the programs (about 23 MB) and the app in place. The agent's computer's image, where Setup installs it, is about 240 MB, and is replaced too.
- **What it leaves behind is small**: a copy of your configuration when it changed it.
- **A snapshot is the only large thing**, and it's yours to delete.
- **You don't need to compact WSL's disk after an update.** A distro's disk file grows and never shrinks by itself, but an update adds little to it, and a snapshot isn't inside it. Compacting is worth it after you've deleted many gigabytes inside a distro, such as old models or build output: [Disk usage](testing-on-wsl2.md#disk-usage) says how. It needs `wsl --shutdown`, which stops every distro, so a protected vault is locked after.

## Testing an update before you trust it

For maintainers and collaborators: what shows an update is safe is updating a real install that has something in it, and finding it all still there.

- **Never first on the install you use.** Use a disposable distro made for the test ([Testing Hatchling with an AI agent](testing-with-an-agent.md) sets one up, and its rules about keys and the app's address apply here), or take a snapshot of your own first. Only one install runs at a time, so stop yours while the test runs: its owner unlocks it again after.
- **Start from the last release**, installed as a person would install it, not from an earlier build of your own.
- **Give it something to lose.** Say a few things, let it remember one, schedule something, switch a setting, protect the vault if the change touches that.
- **Update to the candidate**, the way that install was made.
- **Check what an owner would**: the installer's own words, `doctor`, that it's locked and unlocks if protected, that the conversation, memory, schedule and setting are as they were, and that the change you're testing works in the app.
- **Record what ran**: the release you started from, the candidate's commit and the hash of each program installed, the commands, and what you didn't try. [What completes a run](testing-with-an-agent.md#what-completes-a-run) says what counts as proof.
- **Clean up**: delete the disposable distro, or the snapshot once you're sure, and stop anything you started to keep the distro awake.
