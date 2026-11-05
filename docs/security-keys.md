# Protecting the vault with a security key

Hatchling keeps your keys and sign-ins in a vault, sealed at rest. By default its key is in a file beside it, so the service comes back by itself after a restart. A FIDO2 security key, such as a YubiKey, protects it more: the vault's key is then derived on the security key, from a secret that never leaves it. Without the key, the vault can't be opened, whatever else someone has: a copy of the disk, a backup, or the machine itself, powered off.

**The trade-off: after each restart, you unlock it.** The service starts at boot, finds the vault locked, and waits, and the agent and its schedules wait with it. You unlock it with one command, your PIN and a touch. A schedule that came due while it was locked runs once it's unlocked, marked late: once, however many times it was due.

You don't need a key: without one, nothing here applies, and Hatchling works as it does.

## What you need

- **A FIDO2 security key with `hmac-secret`:** any YubiKey 5, and most FIDO2 keys of the last several years. An older U2F-only key can't do this, and setup says so.
- **Two keys, ideally.** Either one opens the vault, so losing one doesn't lock you out. One works too: backup codes are then your only way back if you lose it.
- **One port is fine.** Setup registers one key, you swap, and it notices the next.
- **USB, not Bluetooth.** Your PIN's hash goes to whatever answers as a key, so Hatchling only talks to keys on USB: nothing in radio range can pose as one.

## Set it up

In a terminal you opened yourself (your key's PIN is typed there, and goes only to the key):

```bash
sudo hatchling-broker vault-keyring setup
```

It looks at what's plugged in, and does the rest:

1. **Registers your keys.** Two plugged in are both taken. With one, it asks if you have a second, and says when to swap. A new key has no PIN: it asks you to choose one. You touch each key when it flashes, and name it.
2. **Gives you your backup codes.** Save them now, on paper or in a password manager, away from this computer. To finish, it asks you to type part of one back.
3. **Restarts the service and unlocks it,** so you see how it's done.

Nothing in the vault changes until your keys are ready. **If anything interrupts it, a closed window or a power cut, run the same command again.** It finds where things stand and carries on.

**Choosing a PIN.** 6 to 8 digits you'll remember, not one you use anywhere else. The key locks itself after 8 wrong tries, which is why a short PIN is safe, and why you shouldn't guess. Use the same one on both keys.

**Which key goes where.** A key made to stay in a port, like a YubiKey 5C Nano, can live in the computer: the PIN protects it if the machine is stolen. Keep the other, such as a 5C NFC, on your keychain or in a drawer.

## In WSL

Windows has the key; WSL sees it only once it's attached, with [usbipd-win](https://github.com/dorssel/usbipd-win). In your own PowerShell:

```powershell
winget install --exact dorssel.usbipd-win
```

Then, in a PowerShell run as Administrator, find the key's BUSID, and share it once:

```powershell
usbipd list
```

```powershell
usbipd bind --busid <busid>
```

Before you run setup, leave this running in a PowerShell window. It keeps that port attached, so swapping keys just works:

```powershell
usbipd attach --wsl --busid <busid> --auto-attach
```

After a restart of Windows or WSL, attach it again to unlock (`usbipd attach --wsl --busid <busid>`). While it's attached, Windows can't use the key, for Windows Hello or passkeys in your browser: `usbipd detach --busid <busid>` gives it back. The BUSID belongs to the port: in another port, `usbipd list` shows its new one.

## After a restart

The service waits, locked: the app can't connect, and `sudo hatchling-broker doctor` says so. Plug in a key (in WSL, attach it), and run:

```bash
sudo hatchling-broker unlock
```

Type the key's PIN, and touch it when it flashes. Any registered key will do, and it doesn't matter if both are plugged in. A wrong PIN says how many tries are left, and Hatchling keeps waiting: run it again. With no key at hand, it asks for a backup code. If another program has the key, it waits a moment, then says so.

## Add a second key, or a replacement

```bash
sudo hatchling-broker vault-keyring setup
```

On a vault with one key, it offers to add another. To add a third, for a safe say:

```bash
sudo hatchling-broker vault-keyring add-key
```

## Lost a key

Your other key still opens the vault, or a backup code does. Remove the lost one, so a finder can't use it with a guessed PIN, and add a replacement as above:

```bash
sudo hatchling-broker vault-keyring remove-key
```

`vault-keyring status` lists your keys by name.

**Both keys lost, and no backup codes:** the vault can't be opened, by anyone. That's what protects it. You'd start fresh: your API keys, conversation, memory and schedules are sealed under it, and are gone.

## New backup codes

If you've lost them, or used some, or think someone saw them. The old set stops working:

```bash
sudo hatchling-broker vault-keyring backup-codes
```

## A PIN that's blocked

Three wrong PINs in a row: unplug the key and plug it back in, and try again. Eight in all, and the key locks itself: only resetting it frees it, and a reset wipes everything on it, your other accounts' passkeys included. Unlock with your other key or a backup code, remove the blocked key, reset it, and add it again.

## Start fresh, or turn it off

**Your security keys aren't changed by any of this.** Hatchling stores nothing on them that uses a slot: uninstalling it, or wiping its vault, leaves them as they were. **Never reset a key to start fresh:** it isn't needed, and it wipes the key's passkeys for everything else.

To go back to the key file, which comes back by itself after a restart (it takes an enrolled key, not a backup code):

```bash
sudo hatchling-broker vault-keyring use-file-tier
```

## How it works, and what it holds against

The key is not a yes-or-no check in front of a key on disk: the vault's key is wrapped under a key derived from the security key's `hmac-secret`, computed on the device. The unlock command talks to the vault keeper over a socket only root and the keeper's user can reach; it passes the keeper the security keys it opened, and relays the questions and your answers. The vault's key never leaves the keeper.

It stops someone with a stolen machine that's powered off, a copy of the disk, or a backup. It doesn't stop someone who is root on the machine while it's unlocked, or who changes Hatchling's programs before you type your PIN.
