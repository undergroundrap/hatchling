# Protecting the vault with a security key

Hatchling keeps your keys and sign-ins in a vault, sealed at rest. By default its key is in a file beside it, so the service comes back by itself after a restart. A FIDO2 security key, such as a YubiKey, protects it more: the vault's key is then derived on the security key itself, from a secret that never leaves it (the `hmac-secret` extension). Without the key, the vault can't be opened, whatever else someone has: a copy of the disk, a backup, or the machine itself, powered off.

**The trade-off: after each restart, you unlock it.** The service starts at boot, finds the vault locked, and waits: the agent and its schedules wait with it. You unlock it with one command and a touch of the key. Work that came due while it was locked runs then, and says it ran late. If the machine restarts while you're away (a power cut, an update), Hatchling waits for you.

## What you need

- **A FIDO2 security key with `hmac-secret`**: any YubiKey 5, and most current FIDO2 keys.
- **Two keys, ideally.** Either one opens the vault, so losing one doesn't lock you out. With one key, the backup codes are your only way back if you lose it: keep them somewhere safe, away from the machine.
- **A PIN on the key**, so a key someone finds can't be used alone. A new YubiKey has no FIDO2 PIN: set one with Yubico Authenticator (Passkeys, Set PIN), or `ykman fido access change-pin`. To make Hatchling refuse a key without one, set `HATCHLING_REQUIRE_KEY_PIN=true`.
- **In WSL, the key attached to WSL.** Windows has the key; WSL sees it only once it's attached, with [usbipd-win](https://github.com/dorssel/usbipd-win). In your own PowerShell:

  ```powershell
  winget install --exact dorssel.usbipd-win
  ```

  Then, in a PowerShell run as Administrator, find the key's BUSID (`usbipd list`), and share it once:

  ```powershell
  usbipd bind --busid <busid>
  ```

  Each time WSL needs it (to set it up, and to unlock after a restart), from any PowerShell:

  ```powershell
  usbipd attach --wsl --busid <busid>
  ```

  While it's attached, Windows can't use the key (for Windows Hello or passkeys in your browser): `usbipd detach --busid <busid>` gives it back. `sudo hatchling-broker doctor` says whether Hatchling can see a key.

## Setting it up

Run these yourself, in a terminal you opened: the key's PIN is typed there, and goes only to the key.

1. **Name the keyring** in `/etc/hatchling/hatchling.env`: add this line.

   ```text
   HATCHLING_VAULT_KEYRING=/var/lib/hatchling/keyring/keyring
   ```

2. **Stop the service:** moving the vault to the key gives it a new key, and a running broker would keep the old one.

   ```bash
   sudo systemctl stop hatchling
   ```

3. **Adopt the keyring**, at first with the key file it has now:

   ```bash
   sudo hatchling-broker vault-keyring init
   ```

4. **Enroll your keys.** Plug in both keys (or attach both, in WSL), and follow its prompts: it asks each key's PIN, and you touch each when it flashes. With one key, it asks you to accept that risk, and gives you backup codes. Write them down: each opens the vault once.

   ```bash
   sudo hatchling-broker vault-keyring enroll-keys
   ```

5. **Start it, and unlock it:**

   ```bash
   sudo systemctl start hatchling
   ```

   ```bash
   sudo hatchling-broker unlock
   ```

`sudo hatchling-broker vault-keyring status` shows the tier and the keys, never what they hold.

## After a restart

The service starts, and the vault waits, locked: `sudo hatchling-broker doctor` says so, and the app can't connect until it's unlocked. Plug in (or attach) a key, and run:

```bash
sudo hatchling-broker unlock
```

It asks for the key's PIN, if it has one, and you touch it when it flashes. A wrong PIN, or no touch, is said, and Hatchling keeps waiting: run it again. With no key at hand, a backup code works instead: it's asked for when no key is plugged in, and each is used once.

Only root can unlock it: the command talks to the vault keeper over a socket only root and the keeper's own user can reach. The vault's key never leaves the keeper: the command passes it the security keys it opened, and relays the questions and your answers.

## Keys, later

- **Add a key:** `sudo hatchling-broker vault-keyring add-key`. An enrolled key, or a backup code, authorizes it.
- **Remove one** (lost, or replaced): `sudo hatchling-broker vault-keyring remove-key`. **Name one:** `label-key`.
- **New backup codes**, replacing the old: `sudo hatchling-broker vault-keyring backup-codes`.
- **Back to the key file**, which comes back by itself after a restart: `sudo hatchling-broker vault-keyring use-file-tier`.

A key whose PIN is blocked, after too many wrong tries, can't open the vault until it's reset, and resetting a FIDO2 key erases what was enrolled on it: remove it, reset it, and add it again, with your other key or a backup code.
