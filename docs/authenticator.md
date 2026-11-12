# Protecting the vault with an authenticator app

Hatchling keeps your keys and sign-ins in a vault, sealed at rest. By default its key is in a file beside it, so the service comes back by itself after a restart. An authenticator app, the kind that shows a six-digit code that changes every half minute, makes Hatchling wait for you instead: after each restart it stays locked until you type a code.

**It is weaker than a security key, and this is how.** What makes the codes, the seed, is kept in the vault's keyring on the same disk. So someone who takes the disk can still open the vault. What the app gives you is that a restart doesn't open the vault by itself: the agent and its schedules don't run, and the vault's key isn't in memory, until you've typed a code. If you have a FIDO2 security key, use it instead: see [Protecting the vault with a security key](security-keys.md). [Your security](your-security.md) sets the three side by side.

**The trade-off is the same as with a key: after each restart, you unlock it.** The service starts at boot, finds the vault locked, and waits, and the agent and its schedules wait with it.

## What you need

- An authenticator app on your phone or in your password manager. Any that takes a standard `otpauth://` address works.
- A terminal you opened yourself. The seed and your codes are typed and shown there, and never pass through the app.

## Set it up

```bash
sudo hatchling-broker vault-keyring enroll-totp
```

On Windows, in PowerShell: `wsl -d hatchling-wsl -u root -- hatchling-broker vault-keyring enroll-totp`.

It works as the first thing you do to a new install. It stops Hatchling while it runs, and then:

1. **Shows the seed, once,** as an `otpauth://` address. Add it to your app. It is the seed itself: keep no other copy of it, and don't paste it anywhere else.
2. **Asks for the code your app shows.** A wrong code can be tried again, three times in all. Nothing changes in the vault until a code matches.
3. **Moves the vault to a new key,** and seals everything in it again under that key.
4. **Gives you ten backup codes.** Each works once, in place of a code from the app. Save them now, on paper or in a password manager, away from this computer: they're your way back in if you lose the phone.
5. **Starts Hatchling again and unlocks it,** asking for a code so you see how it's done.

If anything interrupts it, Ctrl-C or a closed window, Hatchling is started again within a few seconds, as it was. If it stops before a code has matched, the vault still opens by itself: run the command again when you're ready.

## After each restart

```bash
sudo hatchling-broker unlock
```

It asks for the current code from your app. A code works once: if you've just used one, wait for the app's next. A backup code works in its place.

To lock it without a restart, say before leaving the machine somewhere: `sudo hatchling-broker lock`.

## Seeing where things stand

```bash
sudo hatchling-broker vault-keyring status
```

says how the vault opens and how many backup codes are left. With an authenticator app it reads `tier TOTP (weaker: the seed is in the keyring)`. `sudo hatchling-broker doctor` says when the vault is waiting to be unlocked, and how.

## Moving up to a security key

Run `sudo hatchling-broker vault-keyring setup` when you have one. It asks for a current code first, then registers your key: [Protecting the vault with a security key](security-keys.md).
