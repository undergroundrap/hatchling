# WO-003: Complete key rotation for keyring vaults

**Status:** DONE
**Area:** `crates/hatchling-vault` (keyring, TOTP, FIDO2), `crates/hatchling-broker` (`vault_ops`, `vault_keyring`)
**Issue:** [#6](https://github.com/undergroundrap/hatchling/issues/6)
**Trust boundary:** yes

## Problem

`rekey-vault` loads the old data key through the keyring, but writes the new one to a plain key file and never updates the keyring. At the TOTP or hardware-key tier, that defeats the tier and breaks the next unlock. Raising the unlock tier re-wraps the same data key, and the file-tier key-encryption key stays on disk, so a backup taken before the upgrade still opens the vault. And a keyring vault started without `HATCHLING_VAULT_KEYRING` quietly creates a fresh plain key. Every tier has one more gap: `rekey-vault` re-seals only the secrets store, so the schedules, artifacts, spaces and redaction records stay under the old key, and a broker given only the new one can't open them.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** rotating the data key inside the keyring, re-sealing every sealed store the broker keeps (as `migrate-vault` does) and re-wrapping it under the current tier's factors, resumably; rotating on every tier increase; removing the file-tier key-encryption key when leaving that tier; refusing to create a plain key when the store already holds envelopes or a keyring marker exists.

**Not in scope:** new unlock factors; changes to the factors' key derivation.

## Acceptance criteria

- [x] `rekey-vault` with a keyring configured either rotates through the keyring or refuses, by name, and never writes a plain key; tested.
- [x] After a tier increase, a copy of the previous key material no longer opens the vault; tested.
- [x] A keyring vault started without its keyring refuses to serve instead of creating a key; tested.
- [x] After `rekey-vault`, every sealed store opens under the new key and none under the old; tested.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Two pull requests: [#100](https://github.com/undergroundrap/hatchling/pull/100) (every store rekeyed, and no stray key) and [#101](https://github.com/undergroundrap/hatchling/pull/101) (rotation inside the keyring, and on every enrollment). Hardened WSL2 Ubuntu, `HATCHLING_REQUIRE_CELL=1`, for each: plain clippy clean on Rust 1.98.1 and 1.85.0, and `check.sh` green on both. #100: 2,262 tests each; mutation on its changed lines, 29 mutants, 22 caught, 7 unviable; nine hand mutants failed their tests. #101: 2,280 tests each; mutation on its changed lines, 85 mutants, 50 caught, 35 unviable; eleven hand mutants failed their tests. Both: 0 missed, 0 timeouts, 0 orphans.

**How.** A rotation commits the keyring first, in one atomic write: every way in at the tier wraps a fresh data key, any backup codes are replaced by a new set, and the old key is kept *retiring*, wrapped under a key derived from the new one (keyring format 3). Every store is then re-sealed (`hatchling_vault::rekey`, resumable), and the retiring key dropped. A crash after the commit leaves both keys reachable through the current tier; the broker refuses to serve, migrate or read until `rekey-vault`, run again, finishes it. `rekey-vault` with a keyring rotates in place: the file tier's KEK, the TOTP tier's seed (no code beyond the unlock), or every enrolled security key, each touched; one that doesn't answer is named, and nothing changes. `enroll-totp` and `enroll-keys` rotate the same way, and delete the file tier's KEK file. A keyring's adoption and each unlock record it beside the key file; a start with no key file that finds that record, or sealed files, refuses rather than make a key.

**The acceptance criteria, as tested:**

- **`rekey-vault` with a keyring:** `rekey_vault_rotates_inside_the_keyring_and_every_store_follows` (file and TOTP tiers), `every_enrolled_key_must_answer_for_the_rotation` (hardware keys; a missing key named, nothing written), `rotating_a_file_tier_vault_wraps_its_kek_around_a_new_key`, `a_totp_vault_rotates_with_its_unlock_alone`; through the binary, `rekey_vault_rotates_a_keyring_vault_inside_its_keyring` (no key file written, and the keyring's key opens the re-sealed file) and `rekey_vault_refuses_a_keyring_vault_and_writes_no_key` (a key file given with a keyring is refused by name, and not created).
- **A tier increase:** `after_a_tier_increase_the_previous_key_material_opens_nothing` copies the keyring and KEK file, enrolls TOTP, and the copies' key opens none of the stores, and the KEK file is gone; `two_keys_plugged_in_at_once_move_the_vault_to_the_hardware_key_tier` likewise for keys; the backup-code tests show the old set opening nothing and the new set recovering.
- **No stray key:** `a_keyring_vault_started_without_its_keyring_refuses_and_creates_no_key` (by the record, and by sealed files alone), `a_new_vault_still_gets_its_plain_key`; through the binary, `a_keyring_vault_started_without_its_keyring_refuses_to_serve` (no key minted, no socket bound).
- **Every store:** `rekey_vault_reseals_every_store_under_the_new_key_and_none_under_the_old` (all five stores), `rekey_vault_stops_before_touching_anything_when_a_store_is_stale`, and the keyring tests above with two stores each.
- **Resumable:** `an_unfinished_rotation_refuses_to_serve_until_rekey_vault_finishes_it`, `a_pending_rotation_refuses_another_and_finishes_only_with_its_own_keys`, `a_rotation_that_cannot_be_written_changes_nothing`, `finishing_a_rotation_drops_the_kek_only_off_the_file_tier`.

**What it changes for an operator:** `rekey-vault` with a keyring takes no key file. At the hardware-key tier, plug in every enrolled key first (remove a lost one with `remove-key`). Raising the tier, or rotating, prints a new set of backup codes when the vault had codes; the old set stops working. A rotation that stops part-way is finished by running `rekey-vault` again.
