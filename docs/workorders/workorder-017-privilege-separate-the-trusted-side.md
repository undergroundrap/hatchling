# WO-017: Privilege-separate the trusted side

**Status:** IN PROGRESS. Scope step 1, the credential keeper, is merged in five parts:
- #140, a key per store, wrapped under the master key;
- #142, the broker serving with its stores' keys alone, the master key dropped at start;
- #143, one way into the secrets store, and a broker read that can't be recorded releasing nothing;
- #144, a vault keeper process holding the master key and the secrets store's key, the broker neither;
- #145, the gates scanning only for the secrets a cell has had, which the keeper records.

Step 2, the egress authority, is done (#202-#205): slices 1 and 4 (below) start it beside the keeper and move web search into it, slice 2 moves the egress swap into it, slice 3 the harness relay, and slice 5 takes the egress credentials out of the gates' scan. The broker now holds no egress credential, vendor key or search key; the keeper releases them to the authority alone. Left: step 3, the privileged spawner, which drops the broker from root. Until step 3, the keeper and the authority are separate processes, not yet OS boundaries.
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
- **Step 2's design, after mapping the broker's plaintext paths (2026-10-23).**
  - **Where the broker holds a plaintext credential today:**
    - the egress swap: one keeper read per request, put in `Authorization` and scrubbed from the answer;
    - the harness relay: the vendor's key, held for the whole run by the relay's connection threads, and scrubbed from the streamed answer;
    - web search: the Brave key, read per search;
    - channel accounts: read when a session starts. No shipped channel uses one yet: only `local`, which needs none;
    - the gates' scan: it returns the egress and channel-account credentials, because the broker handled them itself;
    - browser sign-ins (WO-030): in the broker's own `credentials` store, typed into the browser cell.
  - **What the keeper checks today:** only that a name belongs to a purpose. It checks no peer, and both run as one user. So which process may receive which purpose comes from the launch, not from peer credentials, until step 3.
  - **Slices, each landing and gated on its own:**
    1. **Launch and identity.** The keeper starts the egress authority (`hatchling-broker egress-authority`) on a socket pair of its own, and hands the broker its end of a broker-to-authority pair. The keeper releases `egress`, `harness` and `search` reads only on the authority's channel, and refuses them on the broker's: tested.
       *Landed with slice 4, changed in one way:* the broker starts the authority, not the keeper, so no process forks from one holding the master key. The broker makes the keeper-to-authority pair when it starts the keeper, hands one end to each, and closes both. The keeper serves each channel as its role, and answers only reads on the authority's. Only `search` has moved so far; `egress` and `harness` stay the broker's until their slices.
    2. **The egress swap.** The proxy runs in the authority: the surrogate key, the pinned allowlist, TLS, the swap, the scrub and the staged pages. The authority reads the egress grants from its own configuration, as the keeper does. The broker relays each session's `EgressOp` and `EgressResult` and never fetches a credential. The cell's peer uid is still the broker's word until step 3, and the docs say so.
       *Landed, in one way different:* the staged pages stay with the broker, which moves a body too large for one session frame in parts as before; the authority gets each request whole, in parts of its own wire. Each session is a connection of its own to the authority, handed over by `SCM_RIGHTS` and served on its own thread, so one session's request, or a search, never waits on another's. The authority refuses a destination no agent is granted.
    3. **The harness relay.** The broker hands the cell's relay listener to the authority (`SCM_RIGHTS`) with the run's vendor and surrogate, and the authority runs the relay with the key it alone reads.
       *Landed, in one way different:* the broker names only the harness, never its vendor: the authority finds the vendor in its own configuration, so the broker can't send the key anywhere. The broker hands over the run's handoff socket rather than the listener, so it never holds the cell's listener either. The relay is made ready, with the key read, before the cell starts, so a run with no key is refused as before.
    4. **Search.** A typed search request to the authority, which reads the key and answers with the results. *Landed with slice 1;* since slice 2, each search is a connection of its own, so searches run side by side.
    5. **The scan.** With the broker holding none of them, the egress credentials leave the gates' scan set.
       *Landed.* It closed a way round slice 2: a scan's values go to the broker, so until then a gate's scan handed it every egress credential. Vendor keys were never in the scan set.
  - **Not moved by this step:**
    - Channel accounts move with the first channel that uses one, into the authority; until then, none is configured.
    - Browser sign-ins must reach the browser cell in plaintext, so no network-side process can insert them. Their store moves to the keeper, which releases a sign-in for a fill alone, recorded. The broker still relays it to the cell, so this narrows custody but doesn't remove the broker from the path. That is its own slice, after this step.
- **Step 3's design, after mapping where the broker uses privilege (2026-10-24).**
  - **Where privilege is used today.**
    - Every cell, whichever path starts it, ends in `isolate` (`hatchling-isolation`), run by whoever spawns it: the broker for its own agent cell, a re-executed broker helper for every other (`agent-run`, `skill-cell`, `browser-cell`). The harness's work trees are given and taken back by another (`harness-tree`).
    - The steps that need privilege on the host are these:
      - writing a cell's id maps for a uid other than the spawner's own (`own` ids as root), or running the setuid `newuidmap` and `newgidmap` (`subordinate` ids);
      - dropping the host's supplementary groups;
      - handing a socket or a work tree to the cell's uid (`CAP_CHOWN`);
      - raising a hard core limit of 0;
      - reading back and removing what a cell wrote, which a root broker does by overriding file permissions.
    - Everything after `unshare(CLONE_NEWUSER)` uses capabilities inside the cell's own namespace only.
    - Today the helpers and the broker are one user, so a broker that drops root can spawn nothing.
  - **The owner's decisions (2026-10-24).**
    - **Root mode uses named system users:** `hatchling-keeper`, `hatchling-egress` and `hatchling-broker`, created once by the operator. `hatchling broker` started as root refuses to start without them, and says how to create them. The README's quickstart gains that one step.
    - **The owner is recorded at start:** `SUDO_UID` when started with sudo, the real uid when rootless, or `HATCHLING_OWNER_UID` to name it. The broker's API, UI, operator and device sockets admit that uid alone, by peer credentials, as they admit the broker's own uid today.
    - **Rootless mode, as noted under Launch:** the keeper and the spawner stay the owner, and the broker drops to a subordinate uid of its own, distinct from the cells'.
  - **The processes.**
    - **A launcher** (`hatchling broker`) never opens the vault. It reads the configuration, makes the socket pairs, and starts four processes, each fresh, by exec: the spawner, the keeper, the authority and the broker. Each starts as its user (`setgroups`, `setgid`, `setuid`, then exec, so no capability survives). Each channel's role comes from the launch, as now; peer credentials on a pair made before a drop prove nothing.
    - **The spawner** stays privileged: root in root mode, the owner rootless. It holds no store and no key. It takes one typed request, a cell spec, and validates it itself before any privileged call:
      - the identity in the configured range, never an account's;
      - every bind source under an allowed root;
      - no capability, no device, no extra mount.

      It then spawns the cell, hands the broker the cell's connection (`SCM_RIGHTS`), and stops it on request or when the broker goes. The helpers become the spawner's.
    - **The broker** runs as `hatchling-broker`, or its subordinate uid rootless, with no capability. It sends specs, and serves the connections it's handed.
    - **Files:**
      - the vault, its key file and keyring belong to `hatchling-keeper`;
      - the broker's stores belong to `hatchling-broker`;
      - the launcher hands existing state, owned by root, to the right user on the first separated start, and logs each change.
    - **The getuid and geteuid checks.** The vault, memory and goals stores check ownership against `getuid`, and the system helpers against `geteuid`. Every drop sets both.
  - **What the map turned up, to fix on the way.**
    - **The agent checks its broker's uid against the peer credentials of a socket pair the spawner made.** It matches today only because the spawner and the broker are one user. The spawner must report the broker's uid, or hand over a pair the broker made.
    - **A cell's view runs `ldd` found on `PATH`, as the spawner.** It moves to a fixed path.
    - **A root broker relies on root's file access** to read back a harness's results and remove its run directory in `own` mode. Under the spawner, the trees go back to the broker's user, as `harness-tree` already does in `subordinate` mode.
    - **(Fixed separately.)** A helper's spec travelled in its arguments, readable by any local user, and carried a run's task and chat history.
  - **Slices, each landing and gated on its own.**
    1. **The spawner, with the broker still root.** Every cell spawn moves behind the typed spec channel and its validation; tested per field.
       *In three parts.* 1a: the spawner, its connections, and every helper's start and supervision behind it, with the spec checked to be its helper's kind (#208). 1b: every field checked against the spawner's own policy. 1c: the broker's own agent cell behind it too.
       *1b landed:* the identity, the broker's uid in the cell, no endpoint, the limits, the group, and every host path a spec names (configured, or inside one of the broker's own run directories). **Left for slice 3:** the paths are checked before the spawn, but a broker that owns a run directory could change it between the check and the mount. That's harmless while the broker is root, which can do anything already. When it isn't, the spawner must open each source itself and hand the open files on. A run cell is held to the agent's identity, which `HATCHLING_CELL_UID_BASE` may set apart from a skill cell's (found by the hosted-model proof, which sets it).
       *1c landed:* the broker's own agent cell starts through the `agent-run` helper, which hands back its connection; the broker forks no cell while a spawner runs. **Also left for slice 3:** the spawner writes each spec into the run directory the broker names. Once the broker isn't root, the spawner must hold that directory to the same rule as a spec's paths, or take the spec another way.
    2. **The keeper and the authority as their users.** The vault is owned by `hatchling-keeper`, and the keeper refuses a read from any other uid; tested with real processes.
       *In three parts, after mapping what touches the vault (2026-10-24).* The keeper opened every store to unwrap its key, the vault's owner checks use the real uid, the operator's vault commands run as root, and the root quickstarts build the vault as root.
       - **2a:** the keeper opens no store of the broker's. It asks the broker for each store's key as the store keeps it, wrapped, and unwraps it without a file.
       - **2b:** the keeper runs as `hatchling-keeper`. That takes:
         - looking users up by name, and a root broker refusing to start without them;
         - the vault and its key directory handed to the keeper by handle, each change logged;
         - the operator's vault commands dropping to the keeper's user;
         - the README and the CI scripts creating the users.

         In root mode the launcher opens the security keys (`/dev/hidraw*`) before the drop and hands the keeper their descriptors, so the hardware tier keeps working without a udev rule (the owner's choice, 2026-10-24).
       *2a landed (#211).* *2b landed:* the keeper runs as `hatchling-keeper` in root mode. The broker hands it its directories by handle and opens its security keys. The operator's vault commands act for it (`hatchling_vault::act_for`, limited to the directories they name). **Not proven by a test:** an operator command that forgot to act for the keeper. The end-to-end runs don't reach a keeper-owned vault with one before the broker's first start.
       - **2c:** the authority runs as `hatchling-egress`, with its inputs (the CA bundle, the harnesses' binaries) readable by it.
         *2c landed:* the authority starts as `hatchling-egress` in root mode, and the broker waits for its word that it started.
    3. **The broker drops root.** It runs as `hatchling-broker` with an empty capability set, and the sockets admit the recorded owner. The tests read `/proc/<pid>/status` for all four processes' Uid and Cap lines, and a hand mutant that skips the drop fails one.
       *In parts, after mapping what the broker still does as root (2026-10-24). Each lands with the broker still root; the drop comes last.*
       - **3a:** a cell's identity is worked out for its maker. Today it follows the broker's own uid when the spec is built, which would change every identity after the drop. Also, `ldd` runs from a fixed path.
       - **3b:** spec files and the helpers' staging move out of the broker's run directories, so nothing root writes is placed under a name the broker could change.
       - **3c:** a harness's trees in `own` mode are given and taken back through the spawner.
       - **3d:** the spawner makes each cell's group. A broker holding the delegated group could lift its cells' limits.
       - **3e:** the owner is recorded at start. The owner-facing sockets and their token files are bound before the drop and given to the owner, and the stale-file checks accept that owner.
       - **3f:** the broker's sealed stores are handed to `hatchling-broker`. The keeper's start, which reads the stores' kept keys as root, acts for that user there, and so do the operator's commands run as root, for the keeper's user and the broker's at once.
       - **3g:** the drop itself, after the spawner starts while there is one thread, with the `/proc` tests and the README. Memory, goals, the run root, the activity log (a file beside the scheduler's store) and the serving lock go to `hatchling-broker` with it; the trust roots stay root's, read only, so the broker can't rewrite them. The owner sockets and tokens are bound and given before it. The spawner's path rule then takes the broker's uid, not its own, for the broker's run directories.
       - **3h:** a cell's bind sources are opened by the spawner, which mounts the open files. Today they are checked by path and mounted by path, so a broker that owns a run directory could change one in between. This is the TOCTOU left from slice 1.
       - **3i:** an approved workspace delivery is written by the spawner, as the owner's (the owner's choice, 2026-10-24). A root broker writes it as root, so the owner can't read it, and a broker that isn't root can't write in the owner's workspace.

       *3g landed:* run as root, the broker drops to `hatchling-broker` once its start is done, on one thread, before it serves anyone (`hatchling_sys::drop_to`: groups cleared, every uid and gid the user's, no capability, `NO_NEW_PRIVS`, each checked). Memory, goals, the run root and the activity log are handed over at start; the spawner's path rule takes the broker user's ids; the broker skips `prepare_parent` with a spawner; the sweep runs after the drop and `clean` drops after taking the lock; trust roots may be root's. The hosted-model CI reads the broker's `/proc/<pid>/status`.
       *3f landed:* run as root, the broker's sealed stores (builds, spaces, schedules, the web stores, the output gate's directory and its redaction store) are handed to `hatchling-broker` at start, and the broker, and the operator's commands, act for it there. `act_for` takes several users, each once, never two in one directory. A root broker refuses to start without the user.
       *3i landed:* with a spawner running, the broker hands an approved delivery's bytes to the spawner (`Deliver { name }`), which writes them only into the configured workspace, a directory of the owner's opened without following a link, as a new file under a workspace name (`O_EXCL | O_NOFOLLOW`), given to the owner before a byte is written.

       *3e landed:* the owner is recorded at start (`HATCHLING_OWNER_UID`, else a root broker's `SUDO_UID`, else its own uid), and the API, UI, operator, device and ingress sockets and the UI and operator tokens are made for that owner, given to it, and admit it alone. The root quickstart runs its clients as the owner.
       *3d landed:* with a spawner running, the spawner makes and ends each cell's group in the delegated one, and the policy accepts only the delegated group in a spec. **Left for 3g:** the broker still checks that it may write the delegated group (`prepare_parent`) whenever it builds a cell's settings. Once it isn't root, that check is the spawner's alone.
       *3c landed:* on the cell's own ids, a harness's trees are given and taken back by the spawner (`Trees { direct }`). Only trees in the broker's harness run directories qualify, given to the configured cell's identity or taken back to the broker's ids, each opened with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`. That opening is also how 3h will open bind sources.
       *3b landed:* each helper runs in a directory the spawner makes for it and removes when it ends. Its spec is written there, and its cell is staged there. The broker no longer names a directory for the spawner to write into: the request carries none.
       *3a landed:* the broker records the spawner's uid and gid when it starts it (`spawns_as`). Identities, subordinate ids and the uid the agent sees its maker as are all worked out for that maker. The agent's check of its maker's uid now holds by design rather than by the broker and the spawner being one user: the cell's connection is made by the spawner's helper, which is the maker.
    4. **Rootless.** The same split on subordinate ids.
    5. **The README and the module map**, and the close-out evidence.
- **Order matters.** The keeper comes first: it forces the grant-handle abstraction the other two build on, and it's the smallest surface with the clearest threat. The egress authority is second, because it's the keeper's only plaintext consumer. The spawner is last, because it's what makes the separate users mean anything, and it's the largest change.
- **One binary can still ship all four.** `hatchling` can start the keeper, egress authority and spawner by re-running itself under a subcommand, as the broker already does for its spawner helpers. The separation is the users, the sockets and what each process holds, not the file they're loaded from. A process may start from a larger binary and still hold nothing it shouldn't.
- Each separated process gets the same treatment as the cell interface: typed requests, peer-credential admission, a refusal for anything else, and its own section in the trust boundary.
- Existing work orders this touches: WO-002 and WO-003 (vault integrity and rotation) land in the keeper; WO-008 and WO-009 (egress policy and correctness) land in the egress authority; WO-014 (no root) shares the spawner; WO-011 (secret hygiene) gets simpler, since fewer processes ever hold a secret.
