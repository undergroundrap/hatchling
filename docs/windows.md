# Hatchling on Windows

Windows Setup installs Hatchling into a prepared WSL distro. Verify Setup independently before you run it. This page also describes what Setup adds to Windows and how to use it after installation.

Hatchling is Linux software: it needs Linux's kernel to keep your agent's work apart from your machine. So it runs in WSL, Windows' own Linux, in a distro of its own called `hatchling-wsl`. You don't need to know any Linux to use it.

## Verify Setup before running it

Windows Setup is unsigned in v0.9. Install the [GitHub CLI](https://cli.github.com/) independently. Download Setup from the [matching release](https://github.com/undergroundrap/hatchling/releases), then run this in PowerShell from the download directory. For other releases, use their exact filename and tag.

```powershell
gh attestation verify .\Hatchling-0.9.0-Setup.exe --repo undergroundrap/hatchling --signer-workflow undergroundrap/hatchling/.github/workflows/release.yml --source-ref refs/tags/v0.9.0 --deny-self-hosted-runners
```

**Stop if verification fails.** Run the same file only after the command succeeds. The attestation covers the Setup file and its embedded Room checksum manifest. WSL checks the image, verity tree and expected root against that manifest before it writes the Room admission record.

### Prepare WSL for an offline install

Before going offline, prepare an x86_64 Ubuntu WSL2 distro named `hatchling-wsl` with systemd running and `cryptsetup-bin` installed. If the distro is already there, use it and skip the first line.

```powershell
wsl --install Ubuntu --name hatchling-wsl --no-launch
wsl -d hatchling-wsl -u root -- apt-get update
wsl -d hatchling-wsl -u root -- apt-get install cryptsetup-bin
```

For a fresh local-model install, have Ollama running with `qwen3.5:9b` already available. An update keeps the existing model. Hosted model use needs its own network access. Setup includes the Room image and installs it without a download.

## Do I need a terminal, or sudo?

After verification and installation, the Start menu covers everyday use. You won't need `sudo` inside Hatchling's distro.

A few advanced commands, such as switching to a hosted model, are in Settings for you to copy. They go in PowerShell, which is on every Windows PC: open the Start menu, type `PowerShell`, press Enter, then paste the command and press Enter again.

You'll see `sudo` elsewhere in these docs. That's for people who installed Hatchling from a Linux or WSL terminal, which is a different way to install it. [If you'd rather use a terminal](#if-youd-rather-use-a-terminal) shows the same commands for this setup.

## Why it needs no administrator rights

A setup with administrator rights can change anything on your PC. This one can't, because it never asks for them. It works only in your own account: your apps folder, your Start menu, your Startup folder, and its own WSL distro. That also means no admin prompt to trust, and it works on a PC where you aren't an administrator.

There are two exceptions, each once, and each from Windows itself. If WSL isn't on your PC, installing it needs an administrator. So does allowing a security key with usbipd.

One tool asks for an administrator each time you run it: **Free up disk space**, because Windows lets only an administrator compact a disk file. It asks only for that step, and says what it will touch first: [Freeing disk space](#freeing-disk-space).

## What's in the Start menu

| Item | What it does | What you'll type |
|---|---|---|
| **Hatchling** | Opens the app in your browser | Nothing, or, if the vault is locked, your key's PIN or a code from your authenticator app |
| **Hatchling Tools → Protect your vault** | Registers a security key, such as a YubiKey | Your key's PIN, then you touch it |
| **Hatchling Tools → Make a backup** | Saves one sealed file with everything Hatchling knows | Your key's PIN, if you use one |
| **Hatchling Tools → Unlock** | Opens a locked vault, after a restart | Your key's PIN, then you touch it; or a code from your [authenticator app](authenticator.md) |
| **Hatchling Tools → Free up disk space** | Gives Windows back the space WSL keeps after files are deleted: [Freeing disk space](#freeing-disk-space) | `yes`, then Windows asks for an administrator |

Windows search finds these by name, not by their folder: open the Start menu and type **Unlock**, say.

Windows also adds an orange **hatchling-wsl** entry, because every WSL distro gets one. It opens a Linux terminal. You don't need it.

## Using a security key

A security key is optional, and it's the strongest way to protect your vault. Windows owns USB devices, so WSL needs one small tool, [usbipd](https://github.com/dorssel/usbipd-win), to see the key. Setting it up takes three steps, and only the first time.

1. In PowerShell, install it:

   ```powershell
   winget install --exact dorssel.usbipd-win
   ```

2. Plug your key in, then list what's connected, and note the number beside it, such as `2-10`:

   ```powershell
   usbipd list
   ```

3. In PowerShell run as administrator, allow that key, once:

   ```powershell
   usbipd bind --busid <number>
   ```

Then each time you use the key, in PowerShell:

```powershell
usbipd attach --wsl --busid <number> --auto-attach
```

Leave that window open while you use **Protect your vault** or **Unlock**: it's what lets Hatchling see the key.

Once Hatchling is unlocked, it doesn't need the key until the next restart, and it keeps working after you close that window. You only need to give the key back if you want to use it for something else: while it's attached to WSL, Windows can't use it, so a website or app on your PC that asks for your key won't find it. To give it back, press Ctrl+C in that window, then:

```powershell
usbipd detach --busid <number>
```

Making all of this automatic is the next thing on the list: the key will be attached for the unlock, and handed back straight after. More on keys, including what to do if you lose one: [Protecting the vault with a security key](security-keys.md).

## What keeps it apart from Windows

The setup seals its distro off from your PC: Windows' drives aren't mounted in it, and it can't start Windows programs. Hatchling's agent already runs inside a sandbox in that distro, so this is a second wall. If something ever got out of the sandbox, it would land in an account that can't reach your files.

It doesn't stop everything: root in the distro could mount a drive again, so it holds below root. Windows can still reach in, which is how updates arrive.

If you installed Hatchling in a WSL distro of your own instead, it has neither wall by default. To add them, put this in that distro's `/etc/wsl.conf`, then run `wsl --terminate <distro>`:

```ini
[automount]
enabled=false

[interop]
enabled=false
appendWindowsPath=false
```

## What Hatchling can't protect you from

Hatchling protects you from its own agent, which runs in a sandbox with no way to your files or keys. With a security key set up, a stolen disk won't open your vault.

It's honest to say what's left:

- **Without a security key, a stolen disk opens.** A default install keeps the vault's key in a file beside it, so it can restart by itself. **Protect your vault** is what changes that.
- **Memory and goals aren't sealed yet.** They're private to Hatchling on your PC, but they're plain files inside the distro's disk. That's being fixed.
- **Malware already running as you can reach Hatchling while it's unlocked.** On Windows, any program you run can use `wsl -u root` with no password. No local password manager can protect you from that either.

## Why they're separate

**Hatchling** is the app, which runs in your browser. Your keys, PIN and saved passwords stay out of it. A one-time code a site asks you for does go through the app, and so does what you type under Take control. A browser is the most exposed thing on your computer: extensions, a bug in a page, or a leaked address can all reach it.

So **Hatchling Tools** opens a real console window instead, and your PIN goes straight from there to your key. Hatchling's setup never sees it, and neither does the app or the agent. It's why the app can't change your vault, even if someone got hold of its address.

## Why the address changes

Each time Hatchling starts, it makes a new address for the app, because the address is the key to your agent. A leaked old one stops working at the next restart.

You don't have to keep track of it. **Hatchling** in the Start menu gets the current one every time. If an open tab says it's offline, close it and open Hatchling from the Start menu.

## Keeping it running

WSL stops a distro about 30 seconds after its last window closes, which would stop your agent too. So the setup can add **Keep Hatchling running while I'm logged in**: one idle process, with no window, from a shortcut in your own Startup folder.

Nothing runs while you're logged out or your computer is off. If you use a security key, Hatchling is locked after each restart of Windows until you unlock it.

## Updating

Download the new Setup and [verify it](#verify-setup-before-running-it) before running it. It finds your install, leaves your settings, model and data as they are, and offers a backup first. Hatchling restarts during an update, so anything it's working on stops. If you use a security key, it's locked afterwards, and opening Hatchling asks for your key.

Hatchling never checks for updates by itself, because it sends nothing out on its own. New versions are on [the releases page](https://github.com/undergroundrap/hatchling/releases).

To put the whole install back as it was if a new version doesn't suit you, take a snapshot of the distro before updating: [Updating Hatchling](updating.md#a-safety-copy) says how, and what it costs in disk space.

## Freeing disk space

WSL keeps Hatchling's distro in one file, `ext4.vhdx`, which grows as the distro uses space and never shrinks by itself. Deleting files inside the distro, such as old models or the agent's work, frees space for the distro, not for Windows. **Hatchling Tools → Free up disk space** gives Windows back what's free inside.

It says what it will do and waits for you to type `yes`. It refuses, changing nothing, while a backup, a restore, another of the broker's store commands or Hatchling's setup is running. Then it:

1. stops Hatchling and runs `hatchling-broker clean`, which removes only Hatchling's leftovers, never your data;
2. trims the distro's free space (`fstrim`). The disk file gives back only whole 1 MB blocks that nothing is in, and a block freed a file at a time stays in it until a trim;
3. stops every WSL distro, Docker Desktop's too;
4. asks Windows for an administrator, for this step alone. That window finds the distro's disk file from WSL's own record in your account, never from a path it's given. It sets WSL's service not to start, stops it, compacts that one file, and puts the service back as it was, even if something fails. Without holding the service stopped, something starts WSL again within a second, and the file can't be compacted;
5. starts Hatchling again, and says how large the file was and is.

The file stays larger than what's in use inside: it keeps every 1 MB block that holds anything, and the filesystem's own tables.

If you use a security key, Hatchling is locked afterwards, as after a restart: open Hatchling from the Start menu to unlock it.

In PowerShell, the same tool is `compact.ps1`, where Setup put Hatchling. `-DryRun` says what it would do, with the file's size and what's used inside, and changes nothing; `-Distro <name>` chooses another distro:

```powershell
powershell -ExecutionPolicy RemoteSigned -File "$env:LOCALAPPDATA\Programs\Hatchling\compact.ps1" -DryRun
```

Compacting is worth it after you've deleted many gigabytes. An update adds little to the file. To do it by hand instead, see [Disk usage](testing-on-wsl2.md#disk-usage).

## If you'd rather use a terminal

The rest of these docs show commands for Linux, with `sudo`. On Windows, type them in PowerShell, like this:

| In these docs | On Windows, in PowerShell |
|---|---|
| `sudo hatchling-broker doctor` | `wsl -d hatchling-wsl -u root -- hatchling-broker doctor` |
| `hatchling-host url` | `wsl -d hatchling-wsl -u owner -- hatchling-host url` |
| `hatchling-broker sign-in add https://site name` | `wsl -d hatchling-wsl -u owner -- hatchling-broker sign-in add https://site name` |
| `hatchling-broker skills list` | `wsl -d hatchling-wsl -u owner -- hatchling-broker skills list` |

Everything still works: this isn't a smaller version of Hatchling.

**Why there's no `sudo`.** Inside the distro, the app runs as an account called `owner`, which has no password and no `sudo`. If the app were ever compromised, it would have no way to become root. You don't lose anything, because root is reached from Windows, with `wsl -u root`, and the app can't start that.

## Run one copy at a time

WSL distros share their control groups and their network, so two copies of Hatchling in two distros can't run together. The setup checks for this, and says so if another copy is running.

One copy is also what a machine has room for. Every WSL distro draws on the one allowance of memory Windows gives WSL, and on the one GPU. A model running on your machine can need most of both, so a second copy, or any other heavy distro, takes from the first. It matters less with a hosted model, which runs elsewhere.

Another distro that isn't Hatchling can get in the way of one more thing. If a distro that runs systemd (another Ubuntu, say) starts while Hatchling is locked, it takes the cpu controller away from Hatchling's service. `sudo hatchling-broker unlock` puts it back itself before it asks for your key, and says that it did; if it can't, it stops and says what to run. Once Hatchling is unlocked and working, another distro starting changes nothing.

## Removing it

Uninstall Hatchling from Windows' Settings, under Apps. It asks whether to delete your data as well. The answer **No** is the default, and keeps the distro and everything in it. **Yes** removes your vault, conversations and memory, and that can't be undone.

## If Windows warns you

Windows may warn that it couldn't verify Setup's publisher. Complete the [independent attestation check](#verify-setup-before-running-it) before choosing **Run** or **Run anyway**.
