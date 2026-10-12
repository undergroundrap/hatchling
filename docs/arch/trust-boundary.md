# Trust Boundary

This document describes what hatchling trusts, what it doesn't,
and where the lines are drawn. If you're new here, read this
first — it explains why the architecture looks the way it does.

## The principle

**Nothing inside a cell is trusted. Everything the owner can see
is transparent.**

Hatchling runs untrusted workloads (agent skills, external CLI
tools, generated code) in isolated cells. The cell is the unit of
distrust: code inside a cell is assumed hostile until proven
otherwise, and the isolation mechanisms don't depend on the
code behaving well.

## What's inside the boundary (trusted)

- **The broker.** The central coordinator. It spawns cells,
  routes messages, enforces grants, and serves the local API.
  The broker never executes untrusted code directly.
- **The vault.** Stores secrets (API keys, credentials).
  Egress and channel credentials never enter a cell: the broker
  refuses vault grants naming them at startup, and the egress
  proxy substitutes the real credential at the boundary. A
  `vault:fetch:<name>` grant deliberately hands that one secret
  to the agent — the grant is the authorization, and a fetch
  returns exactly the named secret, never the store.
- **The owner.** You. You can inspect everything: cell contents,
  broker state, vault metadata, audit logs. There is no seal
  against the owner.

## What's outside the boundary (untrusted)

- **Cell contents.** Skills, harness CLIs (Claude Code, Codex),
  generated code, tool outputs. All run under seccomp-BPF
  allowlists, Linux namespaces (user, mount, network, PID, IPC), and
  dropped capabilities.
- **Network responses.** Everything from the network is
  untrusted input, even from allowlisted hosts. The egress proxy
  controls what leaves; the output gate controls what reaches
  a person.
- **Tool outputs.** A tool saying "do X" is a suggestion, not
  an instruction. State-changing actions go through confirmation
  rails.

## The mechanisms

| Threat | Mechanism |
| --- | --- |
| Malicious code in a cell | seccomp-BPF syscall allowlist, namespaces; inherited descriptors closed, so a cell holds only what it was handed (an agent cell: its one peer-authorized broker session) |
| Credential exfiltration | Egress/channel credentials never enter cells (refused at startup; proxy substitutes at the boundary); fetch grants hand over only the named secret |
| Unscoped network access | Empty network namespace by default; egress allowlist per grant |
| Prompt injection via tool output | Tool output reaches the model only inside an untrusted-data envelope it can't close early; state-changing actions go through confirmation rails; everything sent to a person passes the output gate, which redacts held secrets and private keys |
| Privilege escalation | Capabilities dropped; user namespaces; with a root broker, cells run as `HATCHLING_AGENT_UID`, which must be a uid no other process uses (a dedicated system user, never `nobody`): the broker admits an agent by its uid alone. An unprivileged broker can map only its own uid, so its cells run as the owner; the local API then refuses to start, so a cell can never drive it |
| Supply-chain tampering | Pinned binaries (SHA-256); `cargo deny`; dependency review |

## What the boundary does NOT cover

- **The host kernel.** A kernel vulnerability is the host's
  problem. We document minimum kernel versions below.
- **Physical access.** If someone has your machine, no software
  boundary helps. Full-disk encryption is your friend.
- **The model's judgment.** The trust boundary constrains what
  the agent *can* do. What it *should* do is governed by grants
  and confirmation rails, not by the sandbox.

## Known limitations

Hatchling is pre-1.0. Before launch we reviewed our own trust boundary: who may connect, egress, the vault, and cell isolation. The review found no way out of a cell. The seccomp allowlist kills by default and checks the architecture, namespace flags are refused, every capability is dropped, and mounts are read-only. It did find the gaps below. Each one is a work order, open to contributors.

| Area | Limitation | Work order |
| --- | --- | --- |
| Identity | An agent session is admitted by its kernel uid alone, not tied to the cell the broker spawned. A uid shared with another process (such as `nobody`) lets that process act as the agent. Use a dedicated uid. | [WO-007](../workorders/workorder-007-bind-sessions-to-cells.md) |
| Resources | Cells have no CPU, memory or process-count limits, beyond a runtime bound. A hostile cell can exhaust the host. A CLI cell can also clear its own parent-death signal and outlive its supervisor. | [WO-004](../workorders/workorder-004-cell-resource-limits.md) |
| Seccomp | `socketpair` isn't restricted to Unix sockets. A root broker's supplementary groups carry into the cell. | [WO-005](../workorders/workorder-005-seccomp-and-identity-tightening.md) |
| Filesystem | Some ownership and permission changes go by path rather than by a no-follow handle, and cell staging directories have predictable names under a shared `TMPDIR`. A local attacker who can write there can deny cell spawns. | [WO-006](../workorders/workorder-006-symlink-safe-ownership.md) |
| Vault | A sealed secret isn't bound to its name. Someone who can write the vault on disk, without the key, can swap two sealed files. | [WO-002](../workorders/workorder-002-bind-secrets-to-names.md) |
| Vault | Key rotation isn't complete for keyring vaults: `rekey-vault` bypasses the keyring, and raising the unlock tier keeps the same data key. | [WO-003](../workorders/workorder-003-keyring-key-rotation.md) |
| Egress | An allowlisted model vendor that fetches URLs on request (image or document URLs, server-side tools) can carry data to hosts outside the allowlist. The proxy checks destinations, not request bodies. An upstream that echoes a credential back re-encoded (base64, hex) gets past the scrub, which matches its exact bytes. | [WO-008](../workorders/workorder-008-model-request-policy.md) |
| Grants | A cell can list and cancel every schedule, not only its own. | [WO-010](../workorders/workorder-010-schedule-ownership.md) |
| Hygiene | Some copies of secrets in memory aren't wiped; the redaction audit log stores unsalted hashes; concurrent keyring edits aren't locked; the hardware-key factor doesn't ask for a PIN. A cell killed by seccomp dumps core, so the host's crash handler (apport, on Ubuntu) receives that cell's memory, including any secret it fetched. | [WO-011](../workorders/workorder-011-secret-hygiene.md) |

Found something that isn't here? Report it privately: [SECURITY.md](../../SECURITY.md).

## Requirements

Isolation is mandatory, not best-effort. Without unprivileged
user namespaces and seccomp-BPF, the agent refuses to start —
there is no degraded or unsandboxed fallback.

Minimum: Linux 5.11+ (for `close_range` with `CLOSE_RANGE_CLOEXEC`)
with unprivileged user namespaces enabled and seccomp-BPF available.

## Defense in depth

No single mechanism is the security. A cell escape still faces
the vault's secret isolation, the egress proxy's allowlist, and
the output gate's redaction of what reaches a person. For maximum isolation, run inside
a VM (WSL2 on Windows, or a VM of your choice on Linux) — the
hypervisor boundary sits underneath everything else.
