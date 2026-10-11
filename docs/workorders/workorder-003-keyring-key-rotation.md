# WO-003: Complete key rotation for keyring vaults

**Status:** QUEUED
**Area:** `crates/hatchling-vault` (keyring, TOTP, FIDO2), `crates/hatchling-broker` (`vault_ops`, `vault_keyring`)
**Issue:** [#6](https://github.com/undergroundrap/hatchling/issues/6)
**Trust boundary:** yes

## Problem

`rekey-vault` loads the old data key through the keyring, but writes the new one to a plain key file and never updates the keyring. At the TOTP or hardware-key tier, that defeats the tier and breaks the next unlock. Raising the unlock tier re-wraps the same data key, and the file-tier key-encryption key stays on disk, so a backup taken before the upgrade still opens the vault. And a keyring vault started without `HATCHLING_VAULT_KEYRING` quietly creates a fresh plain key.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** rotating the data key inside the keyring, re-sealing every secret and re-wrapping it under the current tier's factors, resumably; rotating on every tier increase; removing the file-tier key-encryption key when leaving that tier; refusing to create a plain key when the store already holds envelopes or a keyring marker exists.

**Not in scope:** new unlock factors; changes to the factors' key derivation.

## Acceptance criteria

- [ ] `rekey-vault` with a keyring configured either rotates through the keyring or refuses, by name, and never writes a plain key; tested.
- [ ] After a tier increase, a copy of the previous key material no longer opens the vault; tested.
- [ ] A keyring vault started without its keyring refuses to serve instead of creating a key; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
