# WO-011: Secret hygiene in memory and logs

**Status:** QUEUED
**Area:** `crates/hatchling-vault`, `crates/hatchling-broker` (output gate, redactions, artifact ops, serve), `crates/hatchling-artifact`, `crates/hatchling-fido2`
**Issue:** none yet
**Trust boundary:** yes

## Problem

Some copies of secrets aren't wiped when dropped: the output gate's secret set, fetch replies, read buffers during migration, a backup-code buffer. The redaction audit log stores an unsalted SHA-256 of each redacted message, which can serve as a guessing oracle for low-entropy secrets. Concurrent keyring edits aren't locked, so a used backup code or a TOTP step could be restored. The hardware-key factor asks for a touch, not a PIN. `Debug` on a TOTP factor would print its seed. And a cell killed by seccomp dumps core (`SIGSYS`): the host's crash handler (apport, on Ubuntu) receives the cell's memory, including any secret it fetched, and running hundreds of such kills spawns enough handlers to exhaust a small machine's memory.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** wiping every copy (`Zeroizing` buffers, pre-sized reads); a keyed hash in the audit log; a lock on keyring edits; cells that never produce a host-handled core dump (for example `PR_SET_DUMPABLE` cleared after exec by the agent, and the seccomp kill path made non-dumping); an option to require the security key's PIN or user verification; no secret material in `Debug`.

**Not in scope:** hardware enclaves; memory locking.

## Acceptance criteria

- [ ] Each item above has a test or a code-level check (for example, the audit log contains no unkeyed hash).
- [ ] Two concurrent keyring edits can't restore a used backup code; tested.
- [ ] A cell killed by seccomp produces no core dump the host's crash handler receives; a real-cell test with a pipe `core_pattern`.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.
