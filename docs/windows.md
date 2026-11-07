# Hatchling on Windows

The Windows setup installs Hatchling for you, with no administrator rights and no commands to type. This page explains what it put on your PC, what each item does, and what to do if you'd rather use a terminal.

Hatchling is Linux software: it needs Linux's kernel to keep your agent's work apart from your machine. So it runs in WSL, Windows' own Linux, in a distro of its own called `hatchling-wsl`. You don't need to know any Linux to use it.

## Do I need a terminal, or sudo?

**No.** If you used this setup, you never need `sudo`, and you don't need to know any Linux. The Start menu covers everyday use.

A few advanced commands, such as switching to a hosted model, are in Settings for you to copy. They go in PowerShell, which is on every Windows PC: open the Start menu, type `PowerShell`, press Enter, then paste the command and press Enter again.

You'll see `sudo` elsewhere in these docs. That's for people who installed Hatchling from a Linux or WSL terminal, which is a different way to install it. [If you'd rather use a terminal](#if-youd-rather-use-a-terminal) shows the same commands for this setup.

## What's in the Start menu

| Item | What it does | What you'll type |
|---|---|---|
| **Hatchling** | Opens the app in your browser | Nothing, or your key's PIN if the vault is locked |
| **Hatchling Tools → Protect your vault** | Registers a security key, such as a YubiKey | Your key's PIN, then you touch it |
| **Hatchling Tools → Make a backup** | Saves one sealed file with everything Hatchling knows | Your key's PIN, if you use one |
| **Hatchling Tools → Unlock** | Opens a vault a security key protects, after a restart | Your key's PIN, then you touch it |

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

Leave that window open while you use **Protect your vault** or **Unlock**. Once Hatchling is unlocked, it doesn't need the key until the next restart, so you can close it. Making this automatic is the next thing on the list. More on keys, including what to do if you lose one: [Protecting the vault with a security key](security-keys.md).

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

**Hatchling** is the app, which runs in your browser. Everything secret stays out of it. A browser is the most exposed thing on your computer: extensions, a bug in a page, or a leaked address can all reach it.

So **Hatchling Tools** opens a real console window instead, and your PIN goes straight from there to your key. Hatchling's setup never sees it, and neither does the app or the agent. It's why the app can't change your vault, even if someone got hold of its address.

## Why the address changes

Each time Hatchling starts, it makes a new address for the app, because the address is the key to your agent. A leaked old one stops working at the next restart.

You don't have to keep track of it. **Hatchling** in the Start menu gets the current one every time. If an open tab says it's offline, close it and open Hatchling from the Start menu.

## Keeping it running

WSL stops a distro about 30 seconds after its last window closes, which would stop your agent too. So the setup can add **Keep Hatchling running while I'm logged in**: one idle process, with no window, from a shortcut in your own Startup folder.

Nothing runs while you're logged out or your computer is off. If you use a security key, Hatchling is locked after each restart of Windows until you unlock it.

## Updating

Download the new setup and run it. It finds your install, leaves your settings, model and data as they are, and offers a backup first. Hatchling restarts during an update, so anything it's working on stops. If you use a security key, it's locked afterwards, and opening Hatchling asks for your key.

Hatchling never checks for updates by itself, because it sends nothing out on its own. New versions are on [the releases page](https://github.com/undergroundrap/hatchling/releases).

## If you'd rather use a terminal

The rest of these docs show commands for Linux, with `sudo`. On Windows, type them in PowerShell, like this:

| In these docs | On Windows, in PowerShell |
|---|---|
| `sudo hatchling-broker doctor` | `wsl -d hatchling-wsl -u root -- hatchling-broker doctor` |
| `hatchling-host url` | `wsl -d hatchling-wsl -u owner -- hatchling-host url` |
| `hatchling-broker sign-in add https://site name` | `wsl -d hatchling-wsl -u owner -- hatchling-broker sign-in add https://site name` |

Everything still works: this isn't a smaller version of Hatchling.

**Why there's no `sudo`.** Inside the distro, the app runs as an account called `owner`, which has no password and no `sudo`. If the app were ever compromised, it would have no way to become root. You don't lose anything, because root is reached from Windows, with `wsl -u root`, and the app can't start that.

## Run one copy at a time

WSL distros share their control groups and their network, so two copies of Hatchling in two distros can't run together. The setup checks for this, and says so if another copy is running.

## Removing it

Uninstall Hatchling from Windows' Settings, under Apps. It asks whether to delete your data as well. The answer **No** is the default, and keeps the distro and everything in it. **Yes** removes your vault, conversations and memory, and that can't be undone.

## If Windows warns you

Hatchling's setup isn't code-signed yet, so Windows may show "The publisher could not be verified", or a blue "Windows protected your PC" box. Choose **Run**, or **More info** and then **Run anyway**. You don't have to take my word for it: [each release can be checked](releasing.md#verifying-a-release).
