# WO-017: Privilege-separate the trusted side

**Status:** QUEUED
**Area:** `crates/hatchling-broker`, `crates/hatchling-vault`, `crates/hatchling-egress`, `crates/hatchling-isolation`, `crates/hatchling-sys`, `crates/hatchling-runtime`, a new crate per separated process if its dependencies warrant one, `docs/arch/`
**Issue:** [#33](https://github.com/undergroundrap/hatchling/issues/33)
**Trust boundary:** yes

## Problem

Everything on the trusted side runs in one process, as root: the broker's scheduler, spaces, skills, artifacts and local API, the vault that holds every credential, and the egress proxy that swaps surrogates for real keys. A logic bug anywhere in that process runs with the same access as the code holding your keys. A bug of exactly that kind is already queued: WO-010's grant check passes vacuously when the owner holds no schedule grants.

The cell keeps an untrusted agent away from the keys. Nothing keeps a buggy broker component away from them.

The standard answer is privilege separation, OpenSSH's pattern for over twenty years. Split the trusted side into small processes, each with its own OS user and one job, so a bug in a large, busy component can't reach what that component doesn't hold.

Separate OS users only protect anything if the processes above them aren't root. Root can read any process's memory (`/proc/<pid>/mem`, `ptrace`) and any file. So the split is only real once the broker stops running as root. That makes the spawner, the third piece, the one this WO depends on.

## Scope

**In scope**, in this order (each step lands and is gated on its own):

1. **The credential keeper.**
   - A small process, under its own OS user, that owns the vault: its files, its unlock factors (key file, keyring, TOTP, FIDO2) and the decrypted data key.
   - Nothing else can read the vault directory.
   - It answers over a Unix socket admitted by kernel peer credentials.
   - The broker holds **grant handles**: opaque, scoped references to "use credential X for destination Y". It never holds a plaintext credential, so a broker bug can't leak what the broker doesn't have.
   - The vault's operator commands (`migrate-vault`, `rekey-vault`, `vault-keyring`, `vault-audit`) run against the keeper.
   - A `vault:fetch:<name>` grant, the one deliberate path that hands a named secret into a cell, goes through the keeper too, and is logged there.
2. **The egress authority.**
   - The only process that ever receives a plaintext credential, and only at the moment it inserts one into an outgoing request whose destination and grant it has checked.
   - It's the cell's only way out: the allowlist, the pinned addresses, TLS, the surrogate swap, and the request checks queued in WO-008 and WO-009 live here, not in the broker.
   - It runs under its own OS user. It asks the keeper for a credential by grant handle, and the keeper releases plaintext to this process alone, verified by its peer credentials.
3. **The privileged spawner, and the broker drops root.**
   - The operations that truly need privilege move into a small spawner process: creating a cell's namespaces, writing its uid and gid maps, mounting its view, handing it its socket, and starting it as its own uid.
   - The spawner takes a narrow, typed request (a cell spec it validates itself) and nothing else.
   - The broker, the keeper and the egress authority each run as their own unprivileged user, with no capabilities.
   - This step shares its mechanism with WO-014 (cells on a distinct uid without root), and the two are designed together: under WO-014 the spawner may need no root at all.

**Not in scope:**
- delivering approvals to the user outside the agent's conversation, and longer-lived grant scopes (a candidate);
- taint tracking for egress (a candidate);
- splitting the broker's other components (scheduler, spaces, skills) from each other;
- a different socket protocol between the agent and the broker;
- macOS.

## Acceptance criteria

- [ ] The broker process never holds a plaintext credential: a test drives every credential-using path (egress swap, harness relay, channel accounts, `vault:fetch`) and finds no credential bytes in the broker's memory or messages; shown with a canary secret.
- [ ] Only the egress authority receives plaintext credentials from the keeper, and only for a destination its grant covers. The keeper refuses the same request from the broker and from a cell, by peer credentials; tested with real processes.
- [ ] The vault directory and unlock factors are readable only by the keeper's user; a test as the broker's user is refused.
- [ ] After startup, the broker, the keeper and the egress authority run as distinct, non-root users with an empty capability set (`/proc/<pid>/status`); tested.
- [ ] The spawner rejects a malformed or out-of-policy cell spec (wrong uid range, a mount outside the allowed set, an extra capability) before any privileged call; tested per field.
- [ ] A hand mutant that gives the broker the keeper's socket permissions, or skips the broker's drop of root, fails a test.
- [ ] The README's quickstart works unchanged for the user, and `hatchling broker` starts all four processes.
- [ ] `docs/arch/trust-boundary.md` and the module map show the four processes, what each holds, and who may talk to whom.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **Order matters.** The keeper comes first: it forces the grant-handle abstraction the other two build on, and it's the smallest surface with the clearest threat. The egress authority is second, because it's the keeper's only plaintext consumer. The spawner is last, because it's what makes the separate users mean anything, and it's the largest change.
- **One binary can still ship all four.** `hatchling` can start the keeper, egress authority and spawner by re-running itself under a subcommand, as the broker already does for its spawner helpers. The separation is the users, the sockets and what each process holds, not the file they're loaded from. A process may start from a larger binary and still hold nothing it shouldn't.
- Each separated process gets the same treatment as the cell interface: typed requests, peer-credential admission, a refusal for anything else, and its own section in the trust boundary.
- Existing work orders this touches: WO-002 and WO-003 (vault integrity and rotation) land in the keeper; WO-008 and WO-009 (egress policy and correctness) land in the egress authority; WO-014 (no root) shares the spawner; WO-011 (secret hygiene) gets simpler, since fewer processes ever hold a secret.
