# Back up, restore, and start over

Hatchling is yours to copy, bring back, and wipe. All of this runs on the machine, as root: no agent can reach it.

## Two separate things

Hatchling keeps two kinds of thing apart, and changing one doesn't touch the other:

- **What the agent has:** its conversation, memory, goals, schedules and what it made. `reset` clears this.
- **Who can open the vault:** your security keys, backup codes and PIN. `vault-keyring` changes this: see [security keys](security-keys.md).

So you can wipe the agent and keep using the same security key, or change your keys and lose nothing the agent knows. Only `reset everything` crosses the two.

## Back up

```bash
sudo hatchling-broker backup
```

One sealed file lands in `/var/lib/hatchling-backups`, and the newest five are kept. Name a file to put it elsewhere, such as a USB drive:

```bash
sudo hatchling-broker backup /media/usb/hatchling.backup
```

**The file opens with what opens your vault today:** a security key and its PIN, or a backup code. At the key file tier, it opens on this machine, as the key file isn't in it. To move to another machine, protect the vault with a security key or make backup codes first: `sudo hatchling-broker vault-keyring backup-codes`.

Nothing inside can be read or changed without that, so a copy on a USB drive is safe to keep.

The service stops while it's copied, and starts again after. With a security key, that means unlocking it again.

## Restore

```bash
sudo hatchling-broker restore /media/usb/hatchling.backup
```

It opens the whole backup and checks it before it touches anything. **It replaces what's there now.** A backup keeps the keys of its day: if you've changed keys since, use one of those, or a backup code from then.

The agent's room disk, its encrypted workspace, isn't in a backup. A restore tests the restored vault's key against the room disk that's there: if it opens it, the room disk stays, and it's today's, not the backup's; if it doesn't (the vault's key changed since that backup), the room disk is wiped, and restore says so.

## Start over

```bash
sudo hatchling-broker reset conversation
```

Each level asks you to confirm, and takes a backup first:

| Command | Clears | Keeps |
| --- | --- | --- |
| `reset conversation` | The conversation | Everything else |
| `reset agent` | The conversation, memory, goals, schedules and what it made, its room disk included | Your API keys, sign-ins, security keys and backup codes |
| `reset everything` | All of that, your API keys and sign-ins, and the keyring | Your settings. Security keys aren't changed |

Use `--yes` to skip the question in a script. Use `--no-backup` if you can't open the vault to back it up, which is what to do when your keys and codes are all gone.

A store is Hatchling's own directory. Run as root, as they are on an install, `reset` and `restore` first hand what's in a store to Hatchling's service user, so they clear it of everything in it, whoever made it, along with what a crash left behind. **Don't keep files you want inside a store.** A link where a store should be stops them, and they name it: look at it, and remove it yourself if it's yours to lose. A link inside a store is removed, and what it points at is left alone.

After `reset everything`, set your keys up again as on a new install. **Never reset a security key to start fresh:** it isn't needed, and it wipes the key's passkeys for everything else.

## Throwaway use

To use Hatchling like a VM you can always throw away, keep a clean backup and restore it, or reset:

```bash
sudo hatchling-broker reset agent --yes
```

On WSL, the real nuke is the distro itself, which takes everything in it:

```powershell
wsl --unregister <your distro>
```
