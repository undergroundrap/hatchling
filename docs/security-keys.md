# Protecting the vault with a security key

Hatchling keeps your keys and sign-ins in a vault, sealed at rest. By default its key is in a file beside it, so the service comes back by itself after a restart. A FIDO2 security key, such as a YubiKey, protects it more: the vault's key is then derived on the security key, from a secret that never leaves it. Without the key, the vault can't be opened, whatever else someone has: a copy of the disk, a backup, or the machine itself, powered off.

**The trade-off: after each restart, you unlock it.** The service starts at boot, finds the vault locked, and waits, and the agent and its schedules wait with it. You unlock it with one command, your PIN and a touch. A schedule that came due while it was locked runs once it's unlocked, marked late: once, however many times it was due.

You don't need a key: without one, nothing here applies, and Hatchling works as it does.

## What you need

- **A FIDO2 security key with `hmac-secret`:** any YubiKey 5, and most FIDO2 keys of the last several years. An older U2F-only key can't do this, and setup says so.
- **Two keys, ideally.** Either one opens the vault, so losing one doesn't lock you out. One works too: backup codes are then your only way back if you lose it.
- **One port is fine.** Setup registers one key, you swap, and it notices the next.
- **USB, not Bluetooth.** Your PIN's hash goes to whatever answers as a key, so Hatchling only talks to keys on USB: nothing in radio range can pose as one.

**Which keys have been tested.** Hatchling speaks FIDO2 itself, the open standard every key follows, so any key with `hmac-secret` should work. So far it has only been tried on these:

| Key | Firmware | Status |
| --- | --- | --- |
| YubiKey 5C NFC | 5.7.4 | Tested: setup, unlock, losing a key |
| YubiKey 5C Nano | 5.7.4 | Tested: setup, unlock, losing a key |

Any other key is untested, not unsupported. Try it: setup changes nothing until your keys are ready. Whether it works or not, please [tell us how it went](https://github.com/undergroundrap/hatchling/issues/new?template=security_key.yml), or send a pull request adding your key to this table.

## Set it up

In a terminal you opened yourself (your key's PIN is typed there, and goes only to the key):

```bash
sudo hatchling-broker vault-keyring setup
```

It looks at what's plugged in, and does the rest:

1. **Registers your keys.** Two plugged in are both taken. With one, it asks if you have a second, and says when to swap. A new key has no PIN: it asks you to choose one. Each key takes two touches, and it says when. Press Enter to keep the name it suggests, or type your own.
2. **Gives you your backup codes.** Save them now, on paper or in a password manager, away from this computer. To finish, it asks you to type part of one back.
3. **Restarts the service and unlocks it,** so you see how it's done.

Nothing in the vault changes until your keys are ready. **If anything interrupts it, a closed window or a power cut, run the same command again.** It finds where things stand and carries on.

**Choosing a PIN.** 6 to 8 digits you'll remember, not one you use anywhere else. The key locks itself after 8 wrong tries, which is why a short PIN is safe, and why you shouldn't guess. Use the same one on both keys.

**Which key goes where.** A key made to stay in a port, like a YubiKey 5C Nano, can live in the computer: the PIN protects it if the machine is stolen. Keep the other, such as a 5C NFC, on your keychain or in a drawer. Register the one that stays plugged in last: a Nano is hard to pull out.

**How to touch it.** Touch the gold part, with the pad of your finger, while the key flashes. A touch before it flashes doesn't count. On a Nano the gold is on its sides, so in a port it can be hard to see: feel for it, and press for a second.

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

After a restart of Windows or WSL, start WSL first (`wsl -d <your distro>`), run `usbipd list` for the BUSID, and attach it again to unlock (`usbipd attach --wsl --busid <busid>`). While it's attached, Windows can't use the key, for Windows Hello or passkeys in your browser: `usbipd detach --busid <busid>` gives it back. The BUSID can change, in another port or just after a restart of Windows: `usbipd list` always shows the current one.

## See who unlocked it

Every unlock, and every try that failed, is in the service's log:

```bash
sudo journalctl -u hatchling -g "unlock"
```

## Lock it now

If you're leaving a machine somewhere you don't trust, or something looks wrong:

```bash
sudo hatchling-broker lock
```

The vault's key leaves memory at once, the agent's room disk is closed, and the agent and its schedules stop until you unlock it. It says "Locked" only once the room disk is closed: one something still holds open is named instead, with `sudo hatchling-broker room-disk close` to close it. It takes a security key: a vault its key file opens would come straight back, so it says so instead.

## If your key isn't seen

It's rarely the key. Most keys flash when they get power, so:

- **No flash:** push it in fully, or try another port. A hub or adapter can be the problem: try without one.
- **It flashes, and isn't seen, on WSL:** it isn't attached. `usbipd list` should say `Attached` beside it. A new port has a new BUSID, so bind and attach that one.
- **It flashes, and isn't seen, on Linux:** setup or `unlock` says if it's a permissions problem, and which rule to add. On a virtual machine, pass the key through to it first.

**A key doesn't need a backup code just because it wasn't found.** A code is used up when you type it, so check the key first.

## After a restart

The service waits, locked: the app says so, and so does `sudo hatchling-broker doctor`. Plug in a key (in WSL, attach it), and run:

```bash
sudo hatchling-broker unlock
```

Type the key's PIN, and touch it when it flashes. Any registered key will do, and it doesn't matter if both are plugged in. A wrong PIN says how many tries are left, and Hatchling keeps waiting: run it again. With no key at hand, it asks for a backup code. If another program has the key, it waits a moment, then says so.

**A key that's gone quiet is mentioned.** If you have two keys and one hasn't opened the vault in 90 days, `unlock` and `vault-keyring status` say so. Use that key for your next unlock: a spare you never try might turn out lost or broken on the day you need it. If you'd rather not be told, for instance because you leave one key plugged in and keep the other somewhere safe:

```bash
sudo hatchling-broker vault-keyring reminders-off
```

`reminders-on` brings them back.

## Add a second key, or a replacement

```bash
sudo hatchling-broker vault-keyring setup
```

On a vault with one key, it offers to add another. To add a third, for a safe say:

```bash
sudo hatchling-broker vault-keyring add-key
```

## Lost a key

Your other key still opens the vault, or a backup code does. One command takes it from there:

```bash
sudo hatchling-broker vault-keyring lost-key
```

Pick the key from the list, and say whether it's **lost** or **stolen**:

- **Lost:** it's removed, so a finder can't use it with a guessed PIN.
- **Stolen:** the vault's key changes too, and you get new backup codes, so an old copy of your files is no use with the key. It takes a few more seconds, and your other key stays registered.

Then it offers to add a replacement. If it was your only key, the replacement comes first, and a backup code gets you in.

To remove a key you still have, say an old one you've retired: `vault-keyring remove-key`.

**Both keys lost, and no backup codes:** the vault can't be opened, by anyone. That's what protects it. You'd start fresh: your API keys, conversation, memory and schedules are sealed under it, and are gone.

## New backup codes

If you've lost them, or used some, or think someone saw them. The old set stops working:

```bash
sudo hatchling-broker vault-keyring backup-codes
```

## Change a key's PIN

```bash
sudo hatchling-broker vault-keyring change-pin
```

It changes the PIN on the key plugged in. Your vault and your other keys don't change, and what's registered keeps working. To keep the same PIN on both keys, run it for each.

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
