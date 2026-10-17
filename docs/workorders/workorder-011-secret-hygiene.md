# WO-011: Secret hygiene in memory and logs

**Status:** DONE
**Area:** `crates/hatchling-vault`, `crates/hatchling-broker` (output gate, redactions, artifact ops, serve), `crates/hatchling-artifact`, `crates/hatchling-fido2`
**Issue:** [#14](https://github.com/undergroundrap/hatchling/issues/14)
**Trust boundary:** yes

## Problem

Some copies of secrets aren't wiped when dropped: the output gate's secret set, fetch replies, read buffers during migration, a backup-code buffer. The redaction audit log stores an unsalted SHA-256 of each redacted message, which can serve as a guessing oracle for low-entropy secrets. Concurrent keyring edits aren't locked, so a used backup code or a TOTP step could be restored. The hardware-key factor asks for a touch, not a PIN. `Debug` on a TOTP factor would print its seed. And a cell killed by seccomp dumps core (`SIGSYS`): the host's crash handler (apport, on Ubuntu) receives the cell's memory, including any secret it fetched, and running hundreds of such kills spawns enough handlers to exhaust a small machine's memory.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** wiping every copy (`Zeroizing` buffers, pre-sized reads); a keyed hash in the audit log; a lock on keyring edits; cells that never produce a host-handled core dump (for example `PR_SET_DUMPABLE` cleared after exec by the agent, and the seccomp kill path made non-dumping); an option to require the security key's PIN or user verification; no secret material in `Debug`.

**Not in scope:** hardware enclaves; memory locking.

## Acceptance criteria

- [x] Each item above has a test or a code-level check (for example, the audit log contains no unkeyed hash).
- [x] Two concurrent keyring edits can't restore a used backup code; tested.
- [x] A cell killed by seccomp produces no core dump the host's crash handler receives; a real-cell test with a pipe `core_pattern`.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Four pull requests:
- [#102](https://github.com/undergroundrap/hatchling/pull/102): no core dumps.
- [#103](https://github.com/undergroundrap/hatchling/pull/103): keyring locking, and the vault's copies.
- [#104](https://github.com/undergroundrap/hatchling/pull/104): the security key's PIN.
- [#105](https://github.com/undergroundrap/hatchling/pull/105): the broker's copies and the audit log's tags.

Each was gated on hardened WSL2 Ubuntu with `HATCHLING_REQUIRE_CELL=1`: plain clippy clean on Rust 1.98.1 and 1.85.0, and `check.sh` green on both. For each, mutation on its changed lines found 0 missed, 0 timeouts and 0 orphans.

| PR | Tests, each toolchain | Mutants | Caught | Unviable | Hand mutants failing their tests |
| --- | --- | --- | --- | --- | --- |
| #102 | 2,283 | 20 | 18 | 2 | 5 |
| #103 | 2,295 | 101 | 84 | 17 | 8 |
| #104 | 2,304 | 70 | 46 | 24 | 12 |
| #105 | 2,316 | 110 | 80 | 30 | 10 |

#102's crash-handler check also runs as root in CI.

**How.**

- **Core dumps.** Every cell is held to a core limit of one byte, soft and hard, before its user namespace. The kernel aborts a dump to a crash handler on a pipe at exactly that limit, and writes no core file under a page. The seccomp filter refuses any change to the core limit from inside; reads of every limit still pass.
- **Keyring edits.** Every save takes an exclusive lock beside the keyring, and writes only if the file is still what that keyring read (a SHA-256 compare). A failed save rolls the keyring back.
- **Wiping, in the vault.** Reads are sized once. The key file is read into a fixed buffer. The TOTP seed, the keyring's text and backup codes are `Zeroizing`.
- **Wiping, in the broker.**
  - The gates' secret sets: every form they search for.
  - The normalized forms.
  - The held-secret reads.
  - Fetch replies: a `SecretB64` on the wire, and a reply frame made at its final size.
  - Request frames: decoded only from a buffer that is wiped when dropped (`request_of` takes `&Zeroizing<Vec<u8>>`, so the type is the code-level check).
- **The audit log.** It records HMAC-SHA-256 tags under a random key, sealed under the master key in the redaction store, in place of unkeyed hashes. Overrides match by tag. Records and overrides from before keep their hashes, and those overrides still apply.
- **The PIN option.** Under `HATCHLING_REQUIRE_KEY_PIN`, keys enroll to answer from their verified-user secret (`CredRandomWithUV`). The answer must carry the user-verified flag. The setting is bound into the wrap's associated data, and a vault with a touch-only key refuses to serve.
- **`Debug`.** A TOTP factor's `Debug` redacts its seed; a `SecretB64` and a `TagKey` show neither.

**The acceptance criteria, as tested:**

- **No core for a crash handler:**
  - `a_seccomp_kill_hands_no_core_to_the_hosts_crash_handler`: a real cell under a pipe `core_pattern`, as root in CI. A control's `SIGSYS` reaches the handler; the cell's does not.
  - `core_dumps_are_held_to_one_byte_and_a_zero_hard_limit_stays`.
  - `no_cell_may_change_its_core_limit`.
- **Concurrent keyring edits:**
  - `two_edits_from_one_read_never_restore_a_spent_code`: two keyrings read at once each spend a code. The second's write would restore the first's code, so it is refused, and the spent code stays spent.
  - `one_code_opens_the_vault_once_however_many_try_it_at_once`: the same for a TOTP step.
  - `a_save_waits_for_the_edit_lock_and_never_rests_on_a_stale_read`: the lock, waited for or refused, and a write since the read refused.
  - `a_failed_save_leaves_the_keyring_as_it_was`.
- **Every item:**
  - **Wiping**, by a test allocator that scans each freed block for a canary secret in any form it takes:
    - the vault's `tests/freed_memory.rs`: migration, check, the TOTP keyring, backup-code recovery, the key file;
    - the broker's `tests/freed_memory.rs`: the gate's secret set, the held-secret read, the wire payload;
    - with capacity tests for the normalized forms and the reply frame.
  - **The audit log**: `the_log_holds_tags_and_sizes_never_the_text_or_an_unkeyed_hash`, `the_tag_key_is_made_once_sealed_and_kept`, and through the built gate, `the_gate_is_built_once_at_start_and_records_its_settings` (redacted, tagged, overridden by tag, delivered).
  - **The PIN**:
    - `a_key_enrolled_with_its_pin_opens_the_vault_only_with_it`: with the PIN or not at all; and a keyring edited to drop the flag refuses even the PIN answer;
    - `an_answer_under_the_pin_must_say_the_key_verified_its_user`;
    - `the_pin_policy_enrolls_keys_with_their_pin_and_serves_no_key_without`.
  - **`Debug`**: `a_factors_debug_never_shows_a_totp_seed`, `a_secret_payload_round_trips_as_a_plain_string_and_never_shows`, `the_tag_key_is_made_once_sealed_and_kept`.

**What it changes for an operator:**

- A redaction's `show` prints tags, where it printed hashes.
- Set `HATCHLING_REQUIRE_KEY_PIN=true` before `enroll-keys` to have every key ask for its PIN. A key enrolled before is named at unlock: remove it and add it again.
- Two keyring edits at once no longer race: the second waits up to five seconds for the lock, or is refused if the keyring changed under it.
