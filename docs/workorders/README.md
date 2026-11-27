# Work orders

A work order (WO) is how core work on Hatchling is planned and closed: a short spec that says what changes, what doesn't, and the evidence that proves it done. It's a contract, so a reviewer can check the result against it, and a future reader can re-run the gates and get the same verdict.

**Who uses them:** the maintainers use WOs to drive the core: the isolation layer, the broker, the vault, egress, the protocol. You don't need one to contribute. A normal pull request is welcome for a bug fix, a test, docs, or a focused feature. It still meets [CONTRIBUTING.md](../../CONTRIBUTING.md)'s bar.

**Want to take one on?** Comment on its issue (or open one) before you start, so two people don't build the same thing. A WO you complete closes with your evidence and your name.

**Want to propose one?** Open an issue describing the problem. If it's core work, a maintainer turns it into a WO from [TEMPLATE.md](TEMPLATE.md).

## Rules

- **Numbered from WO-001, sequentially, forever.** A number is never reused or renumbered.
- **One file each:** `docs/workorders/workorder-NNN-<short-slug>.md`, from [TEMPLATE.md](TEMPLATE.md).
- **Status:** QUEUED → IN-PROGRESS → CLOSED (or DROPPED, with the reason).
- **Scope is fixed once work starts.** If the scope turns out wrong, close or drop this WO and open a new one. Don't silently widen it.
- **A closed WO keeps its evidence inline:** the exact commands, the environment, and the counts.
- **Trivial changes** (a typo, a one-line fix) don't need a WO. They still need the gates.

## Order of work

The goal is a runtime a security team can approve, for one person or a whole company. Work is ordered by what it protects, and each step is a version [milestone](https://github.com/undergroundrap/hatchling/milestones):

1. **What a hostile cell can do.** The cell is the adversary the design exists for, so these come first: WO-009 (a key an upstream echoes back reaches the cell; numeric IP forms pass the allowlist), WO-008 (data leaves through a vendor's URL fetching), WO-010 (a cell can cancel the owner's schedules; an API grant check passes vacuously), WO-005 (`socketpair` families and inherited groups), WO-004 (resource limits).
2. **What another local user, or someone with disk access, can do** (v0.3.0 shipped WO-006 and WO-007; v0.4.0 takes the rest): WO-006 (a root broker follows symlinks), WO-007 (any process with the agent's uid can open a session), WO-002 (swapped vault files), WO-003 (key rotation at the keyring tiers), WO-011 (secret copies in memory and core dumps), WO-012 (crash leftovers and safe cleanup), WO-023 (an allowlisted name that resolves to an internal address).
3. **In parallel, the first five minutes:** WO-001 (`--help`, and a quickstart CI runs) and WO-016 (`cargo install hatchling-runtime`, which also holds the crates.io names with real releases). Then seeing it work: WO-021 (the UI drives a real broker), and WO-022 (an operator API, held apart from the user's, with the command center's first screen). These come before the rest of the local-attacker work: approvals delivered to the user, and per-site prompts for the agent's web access, both need the UI.
4. **Then what a security review asks for next:** WO-024 (a second, independent guard on every delete and signal, after WO-012's test machine was wiped by a mutant); WO-014 (running without root) and WO-017 (privilege-separating the trusted side, so a bug in the broker can't reach your keys), designed together since they share the spawner; WO-025 (a scheduled run that a crash or a failed write loses or repeats, and a result dropped while reported as parked); WO-013 (the release process: signed and reproducible binaries, an SBOM, publishing from CI); and WO-015 (root cell tests in CI).
5. **What runs in a cell, by the operator's choice:** WO-018 (a coding CLI added by a manifest, OpenCode first) and WO-019 (tools approved, pinned and listed). Hatchling bundles neither: it makes adding one safe. These can start once the hostile-cell work is done.
6. **0.9, its own computer** ([milestone](https://github.com/undergroundrap/hatchling/milestones)): WO-034 (the Room) and WO-035 (the agent's tools in it), then WO-042 (keep the docs and the queue honest in CI), then step 4, WO-041 (files between you and the agent's computer), and WO-043 (folders, and the chat's files), then WO-036 (approvals bound to grant keys), WO-037 (say when a protection is off) and WO-038 (know what is installed). WO-040 (the room disk's capacity) is alongside them. WO-039 (the spawner in a crate of its own) moved to 0.10, the agent track, by the maintainer's decision, and so did WO-044 (the Room's work outliving a restart), the part of WO-034 never built.
7. **0.10, the agent track** ([milestone](https://github.com/undergroundrap/hatchling/milestones)), in the maintainer's order: WO-046 (signed playbook skills) first, since Hatchling's largest gap is what its agent can do; then WO-047 (memory and goals sealed where they live, the last two stores of what the agent knows about its owner that aren't). Then a run that can't outgrow its window, bounded by time, cost and context ([#533](https://github.com/undergroundrap/hatchling/issues/533)), with history given verbatim and sized to the window, never summarized ([#595](https://github.com/undergroundrap/hatchling/issues/595)); *Keep going*, with a record the broker makes that a scheduled run can continue from too: WO-053 ([#531](https://github.com/undergroundrap/hatchling/issues/531)), the run record (the effects it had, what it did shown on each answer, and its plan); the Room's work kept on record: WO-044 (job records that outlive a restart) with Activity keeping what each Room command, read and search was ([#599](https://github.com/undergroundrap/hatchling/issues/599)), the evidence the maintainer decides from whether a run with nobody there may have a computer; the context meter: WO-056, the context panel and the model in use ([#535](https://github.com/undergroundrap/hatchling/issues/535)); and hosted OpenAI and Anthropic keys ([#534](https://github.com/undergroundrap/hatchling/issues/534)). WO-039 is on it too.
8. **0.11, memory you can trust, and the browser and the Library ready:** WO-048 (provenance, supersession, no duplicates, bounded retrieval, then search), designed together with WO-052 (the agent's notes on earlier in the conversation, which replaces WO-026's no-summary rule by the maintainer's decision of 2026-11-25); then WO-055 (several memories asked for at once, on one card with Keep or Skip on each, from WO-048's real-model trial); then WO-054 (background runs: the clock, keeping the distro up, the seat runner and the review queue, with memory reconciliation as its first loop, on WO-048's provenance); the browser ready on install, a pinned and verified Chromium off until the owner turns it on ([#600](https://github.com/undergroundrap/hatchling/issues/600)), and its downloads and uploads through the Library and the file checks ([#601](https://github.com/undergroundrap/hatchling/issues/601)); the Library made fast, a keep with one directory sync ([#509](https://github.com/undergroundrap/hatchling/issues/509)) and a folder's list checked once per download ([#518](https://github.com/undergroundrap/hatchling/issues/518)), a kept item handed back to the agent from its card ([#608](https://github.com/undergroundrap/hatchling/issues/608)), and each item's conversation shown as itself ([#532](https://github.com/undergroundrap/hatchling/issues/532)); WO-050 (every parser of what comes from outside fuzzed in CI, [#628](https://github.com/undergroundrap/hatchling/issues/628)); and last WO-045 (the evaluation harness), to measure the finished agent, recall, the browser and files included.
9. **0.12, your voice:** WO-049, talking with the same agent: speech to text into the same conversation, the answer read aloud, local speech models fetched only when the owner turns voice on, approvals still made on the screen, and the avatar showing what the agent is doing. One feature, landing whole. The credential design for 0.14 ([#604](https://github.com/undergroundrap/hatchling/issues/604)) is written during it.
10. **0.13, the app, whole:** the live app at parity with the demo, the owner's design, for everything the broker can back without connectors or payments ([#612](https://github.com/undergroundrap/hatchling/issues/612)): side chats, schedules edited, paused and run now, goals with due dates, notifications, approval history, the agent's identity, the secure store and data controls in the app, and the document card's preview. Nothing on a live screen claims what the broker didn't say.
11. **0.14, your accounts:** the first connectors, once the maintainer has chosen how a service's credential lives from #604's design; first reading the owner's calendar ([#605](https://github.com/undergroundrap/hatchling/issues/605)), mail read and then send, with an approval of its own ([#606](https://github.com/undergroundrap/hatchling/issues/606)), and GitHub ([#607](https://github.com/undergroundrap/hatchling/issues/607)). Each is measured by WO-045 when it lands.
12. **Then integrations** for larger deployments: audit export, external secret backends, fleet configuration, and the macOS backend.

## Queue

WO-002 to WO-011 come from the pre-launch review of the trust boundary: see [Known limitations](../arch/trust-boundary.md#known-limitations). The trust-boundary ones get extra review.

| WO | Title | Trust boundary | Status |
| --- | --- | --- | --- |
| [001](workorder-001-help-and-tested-quickstart.md) | `--help` on every binary, a quickstart CI runs, and a hosted-model guide |  | DONE |
| [002](workorder-002-bind-secrets-to-names.md) | Bind sealed secrets to their names | yes | DONE |
| [003](workorder-003-keyring-key-rotation.md) | Complete key rotation for keyring vaults | yes | DONE |
| [004](workorder-004-cell-resource-limits.md) | Per-cell resource limits | yes | CLOSED |
| [005](workorder-005-seccomp-and-identity-tightening.md) | Tighten the seccomp filter and the cell's groups | yes | CLOSED |
| [006](workorder-006-symlink-safe-ownership.md) | Symlink-safe ownership and staging | yes | CLOSED |
| [007](workorder-007-bind-sessions-to-cells.md) | Bind agent sessions to the cells the broker spawned | yes | CLOSED |
| [008](workorder-008-model-request-policy.md) | A request-body policy for model vendors | yes | CLOSED |
| [009](workorder-009-egress-correctness.md) | Egress correctness fixes | yes | CLOSED |
| [010](workorder-010-schedule-ownership.md) | Schedule ownership and API grant edges | yes | CLOSED |
| [011](workorder-011-secret-hygiene.md) | Secret hygiene in memory and logs | yes | DONE |
| [012](workorder-012-disposable-state-and-cleanup.md) | Separate disposable files from your data, and clean them up safely | yes | DONE |
| [013](workorder-013-release-process.md) | A release process: signed, reproducible binaries and an SBOM | yes | DONE |
| [014](workorder-014-running-without-root.md) | Run the full broker without root | yes | DONE |
| [015](workorder-015-root-cell-tests-in-ci.md) | Run the cell test suites as root in CI | yes | DONE |
| [016](workorder-016-publish-to-crates-io.md) | Publish to crates.io, with `hatchling-runtime` as the front door | yes | CLOSED |
| [017](workorder-017-privilege-separate-the-trusted-side.md) | Privilege-separate the trusted side: a credential keeper, an egress authority, and a spawner so the broker drops root | yes | DONE |
| [018](workorder-018-harness-manifests.md) | Harness manifests: add a coding CLI by configuration, with OpenCode first | yes | DONE |
| [019](workorder-019-approved-tools.md) | Approved tools: what a cell can run, pinned and listed | yes | DONE |
| [020](workorder-020-model-reasoning-control.md) | Operator-selected model thinking mode | yes | DONE |
| [021](workorder-021-live-frontend.md) | The UI drives a real broker | yes | DONE |
| [022](workorder-022-operator-api.md) | An operator API, and the command center's Machine view | yes | DONE |
| [023](workorder-023-internal-destinations.md) | Refuse internal addresses for destinations that aren't declared internal | yes | DONE |
| [024](workorder-024-two-guards-on-every-delete.md) | Two guards on every delete and signal | yes | DONE |
| [025](workorder-025-durable-scheduled-runs.md) | Scheduled runs and their results survive a crash or a failed write | yes | DONE |
| [026](workorder-026-conversation-continuity.md) | Conversation continuity: the broker keeps the chat, a cell can't write it | yes | DONE |
| [027](workorder-027-approvals.md) | Approvals delivered to the owner: a frozen action, decided in the app, run by the broker | yes | DONE |
| [028](workorder-028-web-search-and-site-approvals.md) | Web search through the broker, and each new site approved by the owner | yes | DONE |
| [029](workorder-029-browser-cell.md) | A browser cell: a real browser the agent drives, in a cell of its own, reaching only what the broker lets through | yes | DONE |
| [030](workorder-030-signed-in-browsing.md) | Signed-in browsing: sessions sealed per site, sign-in filled by the broker, the agent never in the secret path | yes | DONE |
| [031](workorder-031-cell-bound-credentials-skip-the-broker.md) | Cell-bound credentials skip the broker: a cell's vault secrets and browser sign-ins go from the keeper to the cell | yes | DONE |
| [032](workorder-032-ideas-and-builds-from-the-chat.md) | Ideas and builds from the chat: the agent proposes, the owner decides | yes | DONE |
| [033](workorder-033-install-it-and-leave-it-on.md) | Install it and leave it on: one command, a service that survives a reboot | yes | CLOSED |
| [034](workorder-034-run-commands-in-the-agents-encrypted-room.md) | Run commands in the agent's encrypted room | yes | CLOSED |
| [035](workorder-035-the-agents-tools-in-its-own-computer.md) | The agent's tools in its own computer: run, read, search, write and edit from a chat | yes | DONE |
| [036](workorder-036-approvals-bound-to-grant-keys.md) | Approvals bound to grant keys: a tool and its target, scoped and revocable | yes | CLOSED |
| [037](workorder-037-say-when-a-protection-is-off.md) | Say when a protection is off: doctor and Your security name what's missing | yes | CLOSED |
| [038](workorder-038-know-what-is-installed.md) | Know what is installed: a version command, store modes checked, the Room's image on Linux | yes | CLOSED |
| [039](workorder-039-the-spawner-in-a-crate-of-its-own.md) | The spawner in a crate of its own, design first | yes | QUEUED |
| [040](workorder-040-room-disk-capacity.md) | The room disk's capacity, reserved and grown | yes | CLOSED |
| [041](workorder-041-files-between-you-and-the-agents-computer.md) | Files between you and the agent's computer: attach one in, hand one out | yes | CLOSED |
| [042](workorder-042-keep-the-docs-and-the-queue-honest-in-ci.md) | Keep the docs and the queue honest in CI |  | DONE |
| [043](workorder-043-folders-and-the-chats-files.md) | Folders, and the chat's files: give a project back, keep it in the chat, take what you need | yes | CLOSED |
| [044](workorder-044-the-rooms-work-outlives-a-restart.md) | The Room's work outlives a restart: kept job records, receipts, and lock drilled with work running | yes | QUEUED |
| [045](workorder-045-the-evaluation-harness.md) | The evaluation harness: how each model does as Hatchling's agent, measured and published |  | QUEUED |
| [046](workorder-046-signed-playbook-skills.md) | Signed playbook skills: manifest v2, a signer, discovery, and the first skills | yes | CLOSED |
| [047](workorder-047-memory-and-goals-sealed.md) | Memory and goals sealed where they live, like the vault's other stores | yes | CLOSED |
| [048](workorder-048-memory-provenance-and-recall.md) | Memory that knows where it came from: provenance, supersession, bounded retrieval, then search | yes | CLOSED |
| [049](workorder-049-your-voice.md) | Your voice: talk with the same agent, with speech models on your machine | yes | QUEUED |
| [050](workorder-050-fuzz-what-comes-from-outside.md) | Fuzz every parser of what comes from outside | yes | QUEUED |
| [051](workorder-051-runs-bounded-by-what-matters.md) | A chat run bounded by time, tokens and context, with Stop | yes | CLOSED |
| [052](workorder-052-the-agents-notes-on-earlier.md) | The agent's notes on earlier in the conversation | yes | QUEUED |
| [053](workorder-053-the-run-record.md) | The run record: what a run did, kept by the broker, so Keep going carries on from it | yes | IN-PROGRESS |
| [054](workorder-054-background-runs.md) | Background runs: seats that propose, code that applies, and a clock that wakes them | yes | IN-PROGRESS |
| [055](workorder-055-several-memories-one-card.md) | Several memories, one card | yes | CLOSED |
| [056](workorder-056-context-panel-and-model-in-use.md) | The context panel, and the model in use | yes | CLOSED |
| [057](workorder-057-automatic-memory.md) | Automatic memory, every memory traceable | yes | IN-PROGRESS |

## Candidates

Not yet WOs. Each gets a full file, with a scope and acceptance criteria, before work starts:

- **A uid of its own for each cell.** Cells share one uid (a root broker's, or the owner's first subordinate id). Namespaces keep them from reaching each other, but per-uid kernel accounting still crosses: a cell that grows its pipes to the per-user limit (`user_struct::pipe_bufs`) leaves the others' new pipes at the two-page minimum. Leasing each running cell an id of its own from the range would end that. The design question is reuse. The outline, from an outside review:
  - **A lease generation, not the id, names a run.** A late completion or connection keyed by uid could otherwise land on the next cell with the same id.
  - **The lease is recorded before anything uses it.** That's one file per leased id, holding the uid, gid, generation, boot id, cgroup and tree. It's written to a temp file, fsynced, linked into place, and the directory fsynced, before the trusted launcher (which writes the maps and prepares the tree) is released.
  - **Every process that can act for a lease lives in its cgroup.** That includes the launcher, the map helpers and the outer reaper, which runs as the cell's uid outside its pid namespace and outlives its pid 1. An empty cgroup then proves they're gone.
  - **Release reverses that order.** Operations are joined and the cgroup drained, then the tree and any grants to the ids are removed and fsynced. Only then is the lease file unlinked and its directory fsynced. A record that can't be cleaned stays, as a quarantined lease, with its reason shown.
  - **At startup, every lease file is swept** the same way before any id is leased.
  - **The spawner checks an identity against the broker's lease.** Merely being somewhere in `/etc/subuid` isn't enough.
- **Crash dumps sent to a socket.** Since Linux 6.16, a `core_pattern` of `@<path>` hands a crashing process's core to a collector over a Unix socket. That path doesn't honor the one-byte core limit every cell is held to (WO-011), so on such a host a crashing cell's memory would reach the collector, and could outlive the cell. Pipe and file patterns still honor the limit. The fix: refuse to start cells while `core_pattern` names a socket, unless the operator opts in, and test it on a 6.16+ kernel. Neither the WSL2 kernel (6.6) nor the CI runners' (6.8) has the socket path.
- **Tell cells apart for schedules.** A cell can list and cancel any schedule an agent session made, though never the owner's. Now that every session runs under the cell id the broker gave it (WO-007), record which cell made a schedule and hold list and cancel to that cell.
- **Store directories through a handle.** The memory and goals stores still repair a directory's mode by path after checking it. Make them use `hatchling_sys::ensure_private_dir`, as the vault and scheduled runs do since WO-006.
- **Explain failed runs.** When a scheduled run fails, the broker logs only "the run failed". The owner can read the reason in the parked result through the API since WO-025 (`list-parked`); the log still doesn't say it. Log the reason, with redaction.
- **Crash handlers on a socket.** A host whose `core_pattern` starts with `@` (a crash handler listening on a socket, Linux 6.16 and later) isn't covered yet by the one-byte core limit from WO-011. Find what reaches such a handler from a crashing cell, cover it, and test it on a kernel that has the feature.
- **macOS backend.** An implementation of the isolation trait for macOS.
- **FIDO2 on real hardware.** The FIDO2 hmac-secret vault factor is tested against a software authenticator. Validate it on physical security keys, and record which ones.
- **Sweep a harness's leftover trees through the tree helper** (after WO-014). A run whose cell kept its work tree busy past the take-back leaves it in the broker's temp directory, owned by the cell's subordinate ids. The startup sweep runs as the broker and can't remove it. The sweep should hand such trees back through `harness-tree reclaim`, then remove them as it does the rest.
- **Conversation continuity**, now [WO-026](workorder-026-conversation-continuity.md). Each chat message starts a fresh run whose only input is that message, so a follow-up ("use the second option") has nothing to refer to. The broker owns a stable session per conversation, outside any cell. The session holds the turns, the decisions and corrections, the open work and pending approvals, and references to tool results. The broker builds each turn's context from it, checkpoints each tool call and its outcome, and restores it into a fresh cell. The parts exist (`SessionRecord`, `SessionStore`, interrupted-call reconciliation, compaction) but aren't on the chat path, and none holds the turns' text yet. A first slice:
  - a conversation id of the owner's, not a cell's;
  - typed turns, appended only by the broker: the user's message as accepted, and the assistant's as cleared by the output gate (a blocked or replaced answer never comes back as context), each with a turn id so a retry doesn't append twice;
  - at launch, the last completed exchange and the new message, sent to the cell as typed messages; the cell only renders them for its provider.

  What a cell writes stays a note: it can't add a user turn, record an approval, or replace the transcript. Acceptance: cell B's request to its model carries cell A's numbered answer, as the assistant's; a cell's note saying "the user approved" approves nothing; later, a correction made two turns back survives a new cell and a compaction.
- **A budget for the whole model input.** The provider checks the declared context window when it's built, but doesn't budget requests against it: history and tool schemas grow without a bound. Budget the complete input, tool schemas included, with room for the reply. A large tool result becomes a bounded excerpt plus a handle the agent can query.
- **Tool schemas validated in full.** The loop's validator checks top-level types; nested objects and arrays are accepted on their shape alone, though tools advertise item types and bounds. Validate the subset the tools use at registration, and enforce it at the call. (A batch is already checked whole before any of it runs.)
- **Memory: bounded retrieval and provenance**, now [WO-048](workorder-048-memory-provenance-and-recall.md). Recall scans and sorts every match before the prompt's cap applies. A query should carry its budget to the store. Each item should also record where it came from (the user, or an inference), when it should be checked again, and which item it supersedes, so a user's correction outranks an earlier guess. Provenance that implies authority is the broker's to attest.
- **Evaluations of whole tasks**, now part of [WO-045](workorder-045-the-evaluation-harness.md). A small, versioned suite of realistic tasks with objective outcomes:
  - a requirement changed over several turns;
  - a large tool result;
  - injected instructions in retrieved content;
  - a tool failing halfway;
  - an external action that succeeded just before an interruption;
  - an approval expired or changed;
  - memory that conflicts with a correction.

  Track task success, recovery, unauthorized attempts, latency and tokens, in repeated trials where the model varies.
- **Approvals delivered to the user, with scoped grants**, now [WO-027](workorder-027-approvals.md). Approval requests reach the user in the client, outside the conversation with the agent, so an injected agent can't argue its way to a yes; a grant can be one-time, session-, task- or time-bounded, or standing, and later requests must match its scope exactly. The broker already mints approvals through a path no agent session reaches; this adds the delivery and the scopes (and needs the live frontend, WO-021). The UI's decide action still answers `Unavailable`. Each approval binds to the exact operation it allows: the principal and task, the service, account and resource, the action and a digest of its arguments, its expiry and how many uses. The broker checks that same binding at the moment of execution, so a changed recipient or payload needs a new approval. The binding is the broker's own record of a typed action:
  - its arguments are parsed strictly (duplicate keys after unescaping, unknown fields, and inexact numbers refused) and normalized per field;
  - the record is frozen with the tool's version and the account it acts as;
  - its digest is SHA-256 over a domain tag and the action's RFC 8785 canonical JSON.

  The user's preview is drawn from that record, and execution claims it once, atomically, and runs the stored action; arguments the cell sends again are never what runs. A mutable target is checked at the effect (a revision, or `If-Match`), not by an earlier read.
- **Taint-aware egress.** Track which processes in a cell have read the user's data; those lose any automatic allowance for network requests and fall back to approval. Stops quiet exfiltration through requests that would otherwise pass.
- **Inspection on the model path.** A pluggable hook, outside the cell, that inspects requests to and responses from the model (for example a prompt-injection classifier) and can block or flag them; the cell can't turn it off.
- **Audit export.** Stream the broker's audit and vault-access records to an operator's log pipeline (syslog, or JSON lines for a SIEM) through an audit-sink interface. Exporting adds no read path for a cell or an API caller.
- **External secret backends.** A secret-backend interface, so an operator's secret manager (HashiCorp Vault, a cloud KMS) can hold the real credentials. The egress swap is unchanged: a cell still only ever holds a surrogate.
- **Fleet configuration.** A configuration file, alongside the environment variables, that provisioning tools (Terraform, Kubernetes, Ansible) can manage across many machines; and identity for local API callers beyond a socket's uid.
