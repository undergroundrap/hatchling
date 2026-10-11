# WO-002: Bind sealed secrets to their names

**Status:** QUEUED
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

- [ ] A sealed file renamed to another secret's name fails to open, with the generic error; tested.
- [ ] A redaction record moved between names or stores fails to open; tested.
- [ ] `migrate-vault` re-seals a store of current envelopes into the new version, resumably, and an interrupted migration leaves every secret readable; tested.
- [ ] The broker refuses a store with mixed or unknown envelope versions, by name.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
