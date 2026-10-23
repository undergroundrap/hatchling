# WO-017: Privilege-separate the trusted side

**Status:** IN PROGRESS. Scope step 1, the credential keeper, is merged in five parts:
- #140, a key per store, wrapped under the master key;
- #142, the broker serving with its stores' keys alone, the master key dropped at start;
- #143, one way into the secrets store, and a broker read that can't be recorded releasing nothing;
- #144, a vault keeper process holding the master key and the secrets store's key, the broker neither;
- #145, the gates scanning only for the secrets a cell has had, which the keeper records.

Left: step 2, the egress authority, and step 3, the privileged spawner, which drops the broker from root. Until step 3, the keeper is a separate process, not yet an OS boundary.
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

- **Design, after mapping the vault's uses and an outside review (2026-10-19).**
  - **What the code holds today.**
    - One 32-byte XChaCha20-Poly1305 master key seals every store: secrets, schedules, artifacts and ideas, spaces (with the broker's device signing key), and redactions (with the audit-tag key). The stores are told apart only by associated data, with no sub-keys.
    - The broker loads the key once and passes it to about 65 sites.
    - Plaintext secrets inside the broker today:
      - the egress swap;
      - the harness vendor relay;
      - channel account custody;
      - `vault:fetch` to a cell;
      - held-secret scanning: the output and artifact gates read every granted secret on each judgement.
  - **Three claims, each with the milestone that earns it:**
    1. *Custody separation.* The broker holds neither the vault's root key nor its unlock factors. The keeper alone gives this.
    2. *Compromise containment.* A compromised broker can't get secrets past the keeper's own authorization. This needs the keeper plus the spawner split, which drops the broker to a user of its own.
    3. *Controlled credential use.* The broker can't copy a credential and use it outside the approved operation. This needs the egress authority, doing typed operations bound to an account and destination, never a generic authenticated request.

    A credential the broker received before step 2 can be copied and reused until the provider revokes it. No grant expiry recalls a copy.
  - **Keys.**
    - Each store gets its own random data key per generation, wrapped by the keeper's key. The wrap binds the vault instance, store, purpose, generation and format version, with a fresh random nonce each time.
    - The broker receives only its own stores' keys, by an allowlisted operation, never by a derive call taking a caller's label.
    - The keeper alone holds the root, the secrets store's key, and its own policy and audit keys.
    - Wrapping rather than HKDF lets a key be rewrapped, rotated or retired per store.
    - The split starts a fresh root, so a master key copied before it unlocks nothing new.
    - Migration runs in trusted code, never with the broker given the old all-store key.
    - What `redactions` holds is checked before its key is exported. A store the broker can decrypt it can also forge, so nothing sealed in one is evidence the keeper trusts.
  - **Leak scanning.** The broker stops reading the whole vault to scan. It scans against the credentials it has already been given.
    - Exported fingerprints of secret windows are rejected: overlapping windows let a known prefix reconstruct the rest, one character at a time.
    - If unreleased secrets ever need covering, it's a bounded check at the trusted outgoing boundary, with byte budgets that survive reconnects, coarse answers, and no logging of the text.
  - **The keeper's protocol assumes the admitted broker is hostile.**
    - A handle selects a record the keeper owns: the secret and version, the operation, the recipient and broker incarnation, the owner's authorization, a budget, and a deadline and epoch.
    - Issuing or widening a grant is the owner's, on a path the broker can't drive. Enrollment, recovery and factor changes are never broker operations.
    - Release follows a fixed order:
      1. validate the handle;
      2. consume it atomically;
      3. log the release durably;
      4. only then send.

      Two concurrent uses give one release. A failed log sends nothing. A partial write stays consumed.
    - Handles in the first slice die with a keeper restart, a lock, or a new broker incarnation. Deadlines use `CLOCK_BOOTTIME`.
    - Framing is bounded and the schemas strict. Unexpected file descriptors are refused. There are no arbitrary-label derivations and no arbitrary unwraps. An unsupported constraint fails rather than broadening.
  - **Launch.**
    - Peer credentials are taken when a connection is made, so a socket pair made before the broker drops its user proves nothing about the broker's final identity. The broker connects after its drop, or the channel's role comes from the trusted launch itself.
    - Executable checks (`/proc/<pid>/exe`) are diagnostics, not authorization.
    - A small launcher that never opens the vault starts the keeper and the spawner, and the broker is started fresh, so no process forks from one holding keys.
    - The broker gets a fresh anonymous session keyring: an inherited keyring survives fork, exec and user namespaces. (Cells already can't reach one: `keyctl`, `add_key` and `request_key` are outside their seccomp allowlist, and the denied-syscall catalog proves each kills.)
    - Rootless mode inverts the users: the keeper and the spawner stay as the owner, and the broker drops to a subordinate uid of its own, distinct from the cells'. Until the spawner split, a rootless keeper is a separate process of the same uid: separate memory and a smaller surface, not an OS boundary, and the docs say so.
  - **Tests that gate the containment milestone:**
    - a vault key asked for through a broker-store operation, relabeled, is refused;
    - two concurrent uses of a one-release handle give one release;
    - a failed audit write sends no byte;
    - a retry after a partial response stays consumed;
    - old handles are dead after a restart;
    - a broker that stops reading keeps the keeper's memory bounded;
    - a dummy session key planted above the launch isn't readable by the broker;
    - the broker inherits no key-bearing memory, descriptors or owner service connections;
    - the spawner refuses owner credentials and arbitrary privileged operations from an admitted broker;
    - a window-fingerprint reconstruction is shown, to keep that design out.
- **Order matters.** The keeper comes first: it forces the grant-handle abstraction the other two build on, and it's the smallest surface with the clearest threat. The egress authority is second, because it's the keeper's only plaintext consumer. The spawner is last, because it's what makes the separate users mean anything, and it's the largest change.
- **One binary can still ship all four.** `hatchling` can start the keeper, egress authority and spawner by re-running itself under a subcommand, as the broker already does for its spawner helpers. The separation is the users, the sockets and what each process holds, not the file they're loaded from. A process may start from a larger binary and still hold nothing it shouldn't.
- Each separated process gets the same treatment as the cell interface: typed requests, peer-credential admission, a refusal for anything else, and its own section in the trust boundary.
- Existing work orders this touches: WO-002 and WO-003 (vault integrity and rotation) land in the keeper; WO-008 and WO-009 (egress policy and correctness) land in the egress authority; WO-014 (no root) shares the spawner; WO-011 (secret hygiene) gets simpler, since fewer processes ever hold a secret.
