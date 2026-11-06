# Hatchling on Windows

The Windows setup installs Hatchling for you, with no administrator rights and no commands to type. This page explains what it put on your PC, what each item does, and what to do if you'd rather use a terminal.

Hatchling is Linux software: it needs Linux's kernel to keep your agent's work apart from your machine. So it runs in WSL, Windows' own Linux, in a distro of its own called `hatchling-wsl`. You don't need to know any Linux to use it.

## What's in the Start menu

| Item | What it does | What you'll type |
|---|---|---|
| **Hatchling** | Opens the app in your browser | Nothing, or your key's PIN if the vault is locked |
| **Hatchling Tools → Protect your vault** | Registers a security key, such as a YubiKey | Your key's PIN, then you touch it |
| **Hatchling Tools → Make a backup** | Saves one sealed file with everything Hatchling knows | Your key's PIN, if you use one |
| **Hatchling Tools → Unlock** | Opens a vault a security key protects, after a restart | Your key's PIN, then you touch it |

Windows also adds an orange **hatchling-wsl** entry, because every WSL distro gets one. It opens a Linux terminal. You don't need it.

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

Everything still works: this isn't a smaller version of Hatchling.

**Why there's no `sudo`.** Inside the distro, the app runs as an account called `owner`, which has no password and no `sudo`. If the app were ever compromised, it would have no way to become root. You don't lose anything, because root is reached from Windows, with `wsl -u root`, and the app can't start that.

## Run one copy at a time

WSL distros share their control groups and their network, so two copies of Hatchling in two distros can't run together. The setup checks for this, and says so if another copy is running.

## Removing it

Uninstall Hatchling from Windows' Settings, under Apps. It asks whether to delete your data as well. The answer **No** is the default, and keeps the distro and everything in it. **Yes** removes your vault, conversations and memory, and that can't be undone.

## If Windows warns you

Hatchling's setup isn't code-signed yet, so Windows may show "The publisher could not be verified", or a blue "Windows protected your PC" box. Choose **Run**, or **More info** and then **Run anyway**. You don't have to take my word for it: [each release can be checked](releasing.md#verifying-a-release).
