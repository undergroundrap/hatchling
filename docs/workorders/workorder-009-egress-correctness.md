# WO-009: Egress correctness fixes

**Status:** CLOSED
**Area:** `crates/hatchling-egress`, `crates/hatchling-broker` (serve, config, harness relay), `crates/hatchling-core` (framing)
**Issue:** [#12](https://github.com/undergroundrap/hatchling/issues/12)
**Trust boundary:** yes

## Problem

A group of smaller egress gaps:

- Two allowlist entries can map to the same vault secret name (`api.example.com` and `api-example.com`), so one host could receive the other's credential.
- A credential an upstream echoes back is returned to the cell unscrubbed.
- The harness relay buffers whole request bodies, up to about 1 GiB per run.
- The allowlist is re-resolved on every session instead of once at startup.
- Numeric host forms (`2130706433`) pass the no-IP-literal check.
- A response larger than the 64 KiB session frame (about 48 KiB of body) fails, although the proxy allows 1 MiB.

Found in the pre-launch review of the trust boundary (see [Known limitations](../arch/trust-boundary.md#known-limitations)).

## Scope

**In scope:** refusing colliding secret names at startup (or an injective name encoding); scrubbing the credential from responses; streaming or budgeting relay bodies; resolving the allowlist once and sharing it; refusing numeric host forms; making large responses work, or documenting and enforcing the real bound.

**Not in scope:** new egress features.

## Acceptance criteria

- [x] Each gap above has a test that fails before its fix and passes after.
- [x] A 512 KiB model response reaches the agent intact, or the bound is documented and enforced at the proxy.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Environment: WSL2, Ubuntu 26.04.1 LTS, kernel 6.6.87.2-microsoft-standard-WSL2; Rust 1.98.1 (pinned) and 1.85.0 (MSRV); cargo-mutants 27.1.0. Every part ran `HATCHLING_REQUIRE_CELL=1 bash scripts/check.sh` green on both toolchains, and `scripts/mutant-run.sh --base origin/main` over its changed files at kill floor 1.0. Each fix was also proved by hand mutants: the fix undone on a throwaway copy, and its test seen to fail.

- **Numeric host forms, and destinations that share a credential** (#35): `2130706433`, `0x7f000001`, `0177.0.0.1`, `127.1` and all-digit last labels are refused like IP literals; two destinations whose vault secret names collide (`api.example.com:443`, `api-example.com:443`) are refused at startup. Mutation: 205 tested, 176 caught, 0 missed.
- **An echoed credential** (#38): the proxy and the harness relay overwrite the key wherever an answer carries it, through chunked framing and across reads; compressed answers are refused, since compression would hide an echo. Gate: 2,029 passed, 0 failed, on each toolchain. Mutation: 304 tested, 0 missed but two warning-only mutants, then 151 tested on the relay after the fix, 139 caught, 0 missed, 0 timed out. Nine hand mutants, each failing its test.
- **The allowlist, resolved once** (#39): every session's proxy is built over one shared resolution; each keeps its own surrogate key. Gate: 2,031 passed, 0 failed, on each. Mutation: 227 tested, 187 caught, 0 missed, 0 timed out. Three hand mutants.
- **The relay's memory** (#40): a run's relay connections hold at most 64 MiB of request bodies between them (it was about 1 GiB); no second copy of a body; a wrapping chunk size is refused; a connection's slot comes back however its thread ends. Gate: 2,035 passed, 0 failed, on each. Mutation: 151 tested, 139 caught, 0 missed, 0 timed out. Five hand mutants.
- **Bodies larger than a frame** (this change): protocol v13 carries an egress request or answer that doesn't fit a 64 KiB frame in 32 KiB parts. The acceptance test, `a_512_kib_request_and_answer_cross_the_session_whole` (`crates/hatchling-agent/tests/local_model.rs`), sends a 512 KiB request and gets a 512 KiB answer back whole through the real broker and its egress proxy. Gate: 2,047 passed, 0 failed, on each. Mutation: 127 tested, 108 caught, 0 missed, 0 timed out, 19 unviable, over the five changed source files; the first run timed out on one mutant (`write_frame` sending nothing), fixed as the last note says. Six hand mutants, among them an agent that never stages and a broker that never holds: each fails the acceptance test.

## Notes

- **The bounds that stay.** An egress body is bounded at 1 MiB each way by the proxy (`MAX_REQUEST_BYTES`, `MAX_RESPONSE_BYTES`); a session holds one staged request and one answer at most; every message still fits one frame.
- **Re-encoded echoes.** The scrub matches the credential's exact bytes. An upstream that echoes it re-encoded (base64, hex) gets past it; what a vendor does with a request belongs to the request policy, [WO-008](workorder-008-model-request-policy.md).
- **Resolved at the first session, not at startup.** The broker resolves the allowlist when the first session needs it, and pins that for its life. A destination that doesn't resolve fails that session rather than stopping the broker. Moving it into `run_serve` wants a tested startup path, which the pre-flight's delegation rule asks for and `run_serve` doesn't have yet.
- **A test that could hang.** Mutating `write_frame` to send nothing left an adversarial test and its scripted peer both waiting on a read; both ends now time out, so that mutant fails the test in seconds.
