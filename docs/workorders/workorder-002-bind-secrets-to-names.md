# WO-002: Bind sealed secrets to their names

**Status:** DONE
**Area:** `crates/hatchling-vault`, `crates/hatchling-broker` (`migrate-vault`, `rekey-vault`)
**Issue:** [#5](https://github.com/undergroundrap/hatchling/issues/5)
**Trust boundary:** yes

## Problem

A sealed secret's envelope is encrypted with empty associated data, so nothing in the ciphertext names the secret it holds. Someone who can write the vault on disk, without the key, can swap two sealed files. A cell's fetch of a secret it's granted would then return a different secret, possibly one that's never meant to enter a cell. An old envelope of a rotated secret can also be restored unnoticed. Redaction records are sealed the same way.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** binding a domain separator, the store and the secret's name as associated data in every seal and open, under a new envelope version; a migration path that re-seals existing envelopes; the same binding for redaction records.

**Not in scope:** rollback protection across versions of one secret (record it as a follow-up if wanted); key rotation (WO-003).

## Acceptance criteria

- [x] A sealed file renamed to another secret's name fails to open, with the generic error; tested.
- [x] A redaction record moved between names or stores fails to open; tested.
- [x] `migrate-vault` re-seals a store of current envelopes into the new version, resumably, and an interrupted migration leaves every secret readable; tested.
- [x] The broker refuses a store with mixed or unknown envelope versions, by name.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

One pull request. Hardened WSL2 Ubuntu, `HATCHLING_REQUIRE_CELL=1`: plain clippy clean on Rust 1.98.1 and 1.85.0; `check.sh` green on both, 2,252 tests each; mutation on the changed lines of the vault's envelope code, its errors, the broker's vault commands, its start-up and the five stores: 59 mutants, 43 caught, 16 unviable, 0 missed, 0 timeouts, 0 orphans. Ten hand mutants failed their tests: empty associated data, the name or the store left out of it, an old envelope opened as current, the store check taking old envelopes, an unknown version sealed over as plaintext, the broker serving without checking its stores, the check skipped, the spaces store not kept, and the spaces store sealed under the secrets label.

**How.** A version-2 envelope (`HVL2`) has the same layout as version 1, and authenticates as associated data a domain separator, the store's label and the secret's name, so a file renamed, or moved into another store, fails to open with the same generic error as a tampered one. Each of the broker's five sealed stores (secrets, schedules, artifacts, spaces, redaction records) opens under its own `Store`. An old envelope is refused by name everywhere but the migration. `migrate` brings one store to version 2, one file at a time: plaintext sealed, version 1 opened with its empty associated data and re-sealed, version 2 verified and skipped, an unknown version refused; `check` names every file in a store that isn't a current envelope, without the key. The broker checks every store it keeps before it binds, and `migrate-vault` migrates them all.

**The acceptance criteria, as tested:**

- **A renamed file:** `a_sealed_file_renamed_to_another_secret_fails_to_open` swaps two sealed files and gets `DecryptFailed` for both, then puts one back and opens it; `an_envelope_opens_only_as_it_was_sealed` opens an envelope only under its own store and name.
- **A redaction record moved:** `a_record_moved_between_stores_or_names_fails_to_open` moves one into the schedules store, opens its directory as the secrets store, and renames it within its own store, refused each time; `the_associated_data_names_the_domain_the_store_and_the_name` pins the bytes; `the_store_seals_under_its_own_label` (artifacts, schedules, spaces) and the output gate's own test show each store's label.
- **`migrate-vault`, resumably:** `an_old_envelope_is_read_only_by_the_migration`, `an_interrupted_migration_leaves_every_secret_readable_and_resumes` (one file current, one old, one plaintext: all readable before and after, and a second run does nothing), `an_old_envelope_that_does_not_authenticate_fails_the_migration`; in the broker, `every_sealed_store_is_checked_and_migrated_under_its_own_label` and `the_operator_commands_migrate_and_rekey_the_configured_vault`, and the `migrate-vault` smoke test through the binary.
- **Refused by name:** `a_store_with_anything_but_current_envelopes_is_refused_by_name` (old, plaintext and unknown versions, sorted), and `a_stale_sealed_store_stops_the_broker_before_bind`, through the binary: the broker exits naming the store, the file and `migrate-vault`, and never binds its socket.

**What it changes for an operator:** after upgrading, run `hatchling-broker migrate-vault` once; until then the broker stops at start, naming the files. `rekey-vault` still re-seals only the secrets store, as it did before; that gap is recorded under [WO-003](workorder-003-keyring-key-rotation.md).
