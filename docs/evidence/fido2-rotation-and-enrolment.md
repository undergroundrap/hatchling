# Evidence: rotation asks only a key's holder and says a wrong PIN; one key twice is refused (#687)

| | |
|---|---|
| Base | `main` at `76ebdf59` (#693) |
| Date | 2026-11-25 (UTC) |
| Tested in | the maintainer's sandbox (WSL2, Ubuntu, a real kernel, `HATCHLING_REQUIRE_CELL=1`), toolchains 1.98.1 and 1.85.0, with the virtual authenticator |
| Found by | an outside model's review of `fido2.rs`, confirmed in the code. The key derivation and the unlock path were found sound |

## What was wrong

**1. Rotation asked keys that held nothing, and hid a wrong PIN.** In-place rotation finds each hardware wrap's KEK with `key_kek`. Unlike unlocking, it:
- asked every key given, so a key that didn't hold the wrap prompted its owner for its PIN;
- turned any refusal into `KeyAbsent`, so a wrong PIN looked like a missing key and the owner tried again.

Each wrong PIN spends one of a key's tries. After 8 the key's PIN is blocked, and the vault's wrap on it opens nothing.

**2. The two-key enrolment counted handles, not keys.** `enroll_hardware_keys_checked` refused fewer than two keys, but not one key given twice. If the HID layer opened a key twice, it was enrolled twice. The owner believed they had two keys and had one, and the vault got around `SingleKeyRisk`, which requires backup codes for a one-key vault. The staged enrolment (`enroll_key`) already refused it.

## What it does now

1. `key_kek` does what `unlock_hardware_key` does:
   - a key that says it doesn't hold the credential isn't asked;
   - a key that answers it doesn't know the credential is passed over;
   - any other refusal stops it, as `Error::Authenticator` saying what the key said.
2. The batch enrolment refuses a key that already holds a wrap made in the same enrolment, before it's asked for anything, as `enroll_key` does. The keyring is unchanged.

## Tests

| Check | Result |
|---|---|
| `cargo fmt --all --check` | GATE |
| clippy `-D warnings`, 1.98.1 and 1.85.0 | GATE |
| the whole workspace's tests | GATE |
| `hatchling-dev audit` | GATE |

New tests, with a test-only `Handle` (a shared handle to one virtual key, counting the times it's asked for an answer):
- `a_keys_kek_asks_only_its_holder_and_says_a_wrong_pin`:
  - two PIN-checked keys are enrolled;
  - asked for the second's wrap, the first key is never asked;
  - with a wrong PIN typed on the second, the refusal is `Error::Authenticator`, and a key after it is never asked.
- `the_same_key_twice_is_refused_by_the_batch_enrolment`:
  - two handles to one key are refused with `KeyChange`;
  - the second handle is never asked;
  - the keyring file is byte for byte as it was and the tier unchanged;
  - with a second key, the enrolment then succeeds.

**Hand-breaks**, each failing its tests and passing again when restored:

| Break | Caught by |
|---|---|
| A key that says it doesn't hold the credential is asked anyway | GATE |
| A refusal passed over, not said | GATE |
| The batch enrolment's same-key check removed | GATE |

**Mutation**, on the lines changed against `76ebdf59` (`cargo mutants --in-diff`, `hatchling-vault`):

GATE
