# WO-005: Tighten the seccomp filter and the cell's groups

**Status:** CLOSED
**Area:** `crates/hatchling-sys` (seccomp, process, idmap), `crates/hatchling-isolation`
**Issue:** [#8](https://github.com/undergroundrap/hatchling/issues/8)
**Trust boundary:** yes

## Problem

`socket` is restricted by address family, but `socketpair` isn't, so a cell can create sockets of other families through it. A root broker's supplementary groups carry into the cell, and once `setgroups` is denied, the cell can't drop them. A root broker's id map is refused only when it would map uid 0, not when its count would cover other host accounts.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** allowing `socketpair` for `AF_UNIX` only; calling `setgroups(0, NULL)` in the child before the user namespace is created, when the broker is root; limiting a root broker's id-map count to what the cell needs.

**Not in scope:** a wider redesign of the filter profiles.

## Acceptance criteria

- [x] `socketpair` with any family but `AF_UNIX` fails in a cell; a real-cell test.
- [x] A root broker's cell has no supplementary groups (its `Groups:` line is empty); a real-cell test run as root.
- [x] The seccomp catalog and its tests are regenerated and agree.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Environment: WSL2, Ubuntu 26.04.1 LTS, kernel 6.6.87.2-microsoft-standard-WSL2; Rust 1.98.1 (pinned) and 1.85.0 (MSRV); cargo-mutants 27.1.0.

- **`socketpair`:** the harness CLI profile rules it as it rules `socket`: `AF_UNIX`, or `EAFNOSUPPORT` (`a_cli_cell_pairs_unix_sockets_only`, against the built BPF program). In a real CLI cell, `cli_rules_refuse_as_designed` now includes it: the probe's `socketpair(-1, ...)` survives with `-EAFNOSUPPORT`.
- **Groups, as root:** the cell's child calls `setgroups(0, NULL)` (`hatchling_sys::clear_groups`) before `unshare(CLONE_NEWUSER)` when the broker is root. A new probe attack reports the cell's `/proc/self/status` `Groups:` line, and the adversarial check `a_root_brokers_cell_has_no_supplementary_groups` runs it as root. Run as root, the broker process holding groups 4, 27 and 100 (`setpriv --groups 4,27,100`): the cell's line is empty. A hand mutant that skips the drop keeps all three, seen in the cell as `65534 65534 65534`, and fails the check. Unprivileged, `only_a_root_broker_drops_its_groups` drives the decision with a recording `clear`.
- **The id map:** `IdMap::validate_for_euid(0)` refuses a count other than 1, naming why.
- **The catalog:** `cli_denied_syscalls` is the table minus what the profile decides, so `socketpair` moved from allowed to ruled with no catalog edit; `only_the_cli_profile_rules_any_call` pins the ruled set `[16, 41, 53, 56, 157, 435]`.
- **Gate:** `HATCHLING_REQUIRE_CELL=1 bash scripts/check.sh`, green on 1.98.1 and 1.85.0: 2,071 passed, 0 failed, each. The adversarial suite, unprivileged: 16 checks, ok. The cell-boundary suite as root: 7 passed.
- **Mutation**, `scripts/mutant-run.sh --base origin/main`, kill floor 1.0:
  - over `seccomp.rs`, `process.rs`, `idmap.rs`, `spawn.rs` and the adversarial `lib.rs` and `probe.rs`: 317 tested, 282 caught, 2 missed, 1 timed out, 32 unviable. All three were closed and rerun:
  - the groups drop, untestable unprivileged as first written, became a decision a test drives (`spawn.rs`: 46 tested, 39 caught, 0 missed, 0 timed out);
  - `drop_capabilities` took any refusal for the end of the bounding set (an older gap): a test now sees it fail at `prctl(CAPBSET_DROP)` (`process.rs`: 82 tested, 76 caught, 0 missed);
  - `Child::terminate -> Ok(())` timed out, the stub harness's `hang` spinning forever: it now spins a minute, and the timeout test checks a hung run comes back near its bound (`--re Child::terminate`: 12 tested, 12 caught, 0 timed out, 0 orphans).
- **Hand mutants**, each failing its test: `socketpair` allowing other families; a root broker mapping many uids; the groups drop when not root.

## Notes

- **`HATCHLING_CELL_UID_COUNT`** stays, as a setting whose only value is 1 for now; a cell that needs more than one uid is WO-014's to design.
- **Root runs of the suites aren't in CI yet** ([WO-015](workorder-015-root-cell-tests-in-ci.md)). Run as root here, the adversarial suite's `mount_escape_all_writes_denied` reports a changed mount inventory, on `main` as on this branch: the inventory was recorded unprivileged. WO-015's work, not this one's.
- **Test binaries under a 0750 home** can't be reached by a cell running as `nobody`; the root runs above opened the home to 0755 for their length.
