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
| Malicious code in a cell | seccomp-BPF syscall allowlist, namespaces; inherited descriptors closed, so a cell holds only what it was handed (an agent cell: its one broker session); a one-byte core limit the cell can't change, so a seccomp kill hands the host's crash handler (apport, systemd-coredump) no core and none of the cell's memory |
| Credential exfiltration | Egress/channel credentials never enter cells (refused at startup; the egress authority's proxy substitutes at the boundary, and the broker never holds them either); fetch grants hand over only the named secret, and every sealed file is bound to its store and name, so swapping files on disk can't hand a cell a different one |
| A compromised broker reading the vault | The broker never holds the master key or the secrets store's key: the vault keeper, a process of its own that isn't dumpable, holds them, and answers only the reads the configuration gives each purpose (a channel account's credential, to the broker) and cells' vault ops under their grants. The egress destinations' credentials, the harnesses' vendor keys and search's key go to the egress authority alone, a process of its own that isn't dumpable either: the keeper releases them only on the channel the broker made for the authority and closed, and answers nothing but reads there. The authority runs each session's egress proxy (its surrogate key, the swap, the scrub), reading a credential for the request that needs it; the broker checks the session's grants and relays the requests and answers. It runs each harness run's relay too, with the vendor it finds in its own configuration, so the broker names neither the key nor where it goes. The authority checks each destination against the operator's grants too, so a broker can't send it anywhere no agent may go; until the spawner split, the peer uid a request is bound to is still the broker's word. The gates scan for what the keeper chooses: the secrets a cell has been given or has stored, which it records before the cell has them, and the channel accounts' credentials, which the broker handles; never an egress credential or a vendor key, which the broker never holds. A granted secret no cell has had never leaves it. Every release is recorded before it is sent; any other read is refused and recorded as denied. Two credentials still pass through the broker, because they are meant for a cell and the cell's one connection is to the broker: a secret a cell fetches or stores under its vault grants, and a browser sign-in, from a store the broker holds, on its way to the browser cell. [WO-031](../workorders/workorder-031-cell-bound-credentials-skip-the-broker.md) moves both. Run as root, each is an OS boundary: the keeper runs as `hatchling-keeper`, the authority as `hatchling-egress`, the broker as `hatchling-broker`. Without root they're processes of the owner's user: separate memory, not separate users |
| A compromised broker reaching past its own files | Run as root, the broker gives up root once its start is done: it runs as `hatchling-broker`, with no group, no capability and no exec that could gain one. Every cell is started by the spawner, which holds each spec to its own policy (identity, limits, group, the host paths it may give), takes harness trees back to the broker's user, writes approved deliveries as the owner's, and never runs `ldd` as root on a binary another user could change. In both modes the broker then holds itself, with Landlock, to its stores, state, temp and socket directories and the system's read-only paths, with nothing to run: not the vault, not the rest of its user's home. A kernel without Landlock leaves the broker unconfined, and it says so: without root, it can then read whatever its owner can |
| Copied key material: an old keyring, KEK file or backup code | `rekey-vault` and every tier enrollment move the vault to a new data key, give every store a new key of its own wrapped under it, and re-seal each store's files; the backup codes are replaced and the file tier's KEK file deleted once the vault leaves that tier, so a copy taken before opens nothing. A keyring vault started without its keyring refuses to serve rather than make a key |
| Unscoped network access | Empty network namespace by default; egress allowlist per grant |
| A path into the operator's own network | Every destination is resolved once, at startup, and pinned, so DNS can't move it later; one that resolves to a loopback, private, shared, link-local, unique-local, multicast or cloud-metadata address fails the allowlist unless the operator wrote it `internal:` (the coding-CLI relay's vendors too) |
| Data carried out by a model vendor | The model request policy (below): a model request that asks the vendor to fetch anything is refused before it leaves |
| Prompt injection via tool output | Tool output reaches the model only inside an untrusted-data envelope it can't close early; state-changing actions go through confirmation rails; everything sent to a person passes the output gate, which redacts held secrets and private keys |
| Privilege escalation | Capabilities dropped; user namespaces; with a root broker, cells run as `HATCHLING_AGENT_UID`, which must be a uid no other process uses (a dedicated system user, never `nobody`). Every agent cell, the broker's own and each scheduled run's and subagent's, reaches it over a connection made for that cell alone, and in cell mode the broker binds no socket for agents. An unprivileged broker can map only its own uid, so its cells run as the owner; the local API then refuses to start, so a cell can never drive it |
| Supply-chain tampering | Pinned binaries (SHA-256); `cargo deny`; dependency review |

## The model request policy

A model vendor's API can fetch on the caller's behalf: an image or a document named by URL, a server-side search or fetch tool, a remote tool server. A cell allowed to reach a vendor could ask it to fetch an attacker's URL and carry data to a host the allowlist never named. So the egress proxy and the harness relay read every model request before it leaves (`crates/hatchling-egress/src/policy.rs`).

**What's read.** Every `POST` to a path ending in `/chat/completions`, `/responses`, `/messages`, `/messages/count_tokens` or `/embeddings`, on any destination; and every request the harness relay passes on. The body must be JSON, and no object in it may name a key twice: another parser could keep the other copy and read a different request.

**What's refused, anywhere in the body:**

- **Content by URL:** a block whose `type` is `url`, or an `image_url`, `file_url`, `audio_url`, `video_url` or `document_url` naming anything but a `data:` URI. Content sent inline passes.
- **A tool the vendor runs:** any tool whose `type` isn't one the caller runs itself. Allowed: no `type` (Anthropic's own tool definitions), `function`, `custom`, `local_shell`, and the `bash_`, `text_editor_`, `computer_` and `memory_` families. Anything else, including a type no vendor had shipped when the list was written, is refused until it's reviewed and listed.
- **A remote tool server or the vendor's own search:** `mcp_servers`, `web_search_options`, a `plugins` list, a stored prompt (`prompt` with an `id`, which can carry tools set on the vendor's side), or a model named with `:online`.

**What passes.** Ordinary chat, responses and messages requests, tool definitions and tool calls, inline images, thinking and caching. A tool call's own `input` isn't read: it's what a tool the caller runs was asked, not something the vendor acts on. Prompt text is never filtered.

**Known vendors.** A destination on a known model vendor's host (`api.openai.com`, `api.anthropic.com` and the others in `MODEL_VENDORS`) is open to those endpoints and `GET` of its model list only. Its batch, file and assistant APIs are refused: a batch would carry requests this policy never reads. The harness relay already holds each vendor to its own API.

**Beta headers** pass. A beta only unlocks request fields, and the fields that fetch are refused whichever betas are set.

**What it costs.** A harness configured to use its vendor's web search or fetch gets those requests refused with a 403 that says why; turn them off in the harness. A new vendor feature that fetches stays refused until it's reviewed and added here.

## Reading the web (WO-028)

An agent that can read the web can be steered by what it reads, and can carry what it knows out in the address it asks for. So the cell never reaches the web: it asks the broker for a page (`web_fetch`), and the broker decides (`crates/hatchling-broker/src/web_ops.rs`, `crates/hatchling-egress/src/web.rs`).

- **Off unless the operator turns it on** (`HATCHLING_WEB=sites`), and a host the operator refuses (`HATCHLING_WEB_REFUSE`), with every name under it, is never asked for.
- **Each site is the owner's to allow.** A site is an origin: `https`, a host and a port, never a subdomain or another port. A page on a site the session doesn't hold is put to the owner in the app, outside the conversation, with the page and the agent's reason labeled as the agent's. The owner allows it once or for the session, or refuses it for the broker's life. Nothing is fetched until they say yes, and nothing a page says can approve anything.
- **No internal address.** The URL is parsed strictly (no user name or password, no IP address, a host in ASCII), the host is resolved when it's fetched, and it's refused if any address it resolves to is internal, before a connection. The connection goes only to the addresses checked.
- **One bounded read.** A `GET` with no credential, 1 MiB at most, 60 seconds for the read with its redirects on the same site; a redirect to another site is returned, for the agent to ask for. HTML is reduced to its text, and other types than text are refused.
- **What comes back is data.** The page reaches the model inside the tool result's envelope, labeled as data, never instructions.
- **Search runs in the egress authority** (`HATCHLING_SEARCH=brave`, `crates/hatchling-broker/src/egress_authority.rs`, `crates/hatchling-egress/src/search.rs`). The provider's key is the vault secret `search-brave`: the vault keeper releases it to the egress authority for search alone, never to the broker, no grant may name it, and it travels only in the request's own wiped buffer. The broker sends the query and gets the results. The query leaves to the provider the operator chose; it's logged with its session and length, and its text only when the owner opts in (`HATCHLING_SEARCH_LOG=queries`). Results are the provider's words, given to the model as data; reading one is a fetch, which asks for its site like any other, and the owner is told which result led there.

**What a granted site can still do.** It receives whatever the agent puts in a page's address, so a session grant is a window for that site to learn what the agent read. The prompt says so, and *once* keeps the window to one page.

## What the boundary does NOT cover

- **The host kernel.** A kernel vulnerability is the host's
  problem. We document minimum kernel versions below.
- **Physical access.** If someone has your machine, no software
  boundary helps. Full-disk encryption is your friend.
- **What goes to the model vendor itself.** A model request
  carries the conversation to the vendor the operator chose, under
  the operator's account. The request policy stops the vendor
  fetching on the cell's behalf; it doesn't filter what the cell
  says to the vendor.
- **The model's judgment.** The trust boundary constrains what
  the agent *can* do. What it *should* do is governed by grants
  and confirmation rails, not by the sandbox.

## Known limitations

Hatchling is pre-1.0. Before launch we reviewed our own trust boundary: who may connect, egress, the vault, and cell isolation. The review found no way out of a cell. The seccomp allowlist kills by default and checks the architecture, namespace flags are refused, every capability is dropped, and mounts are read-only. It did find the gaps below. Each one is a work order, open to contributors.

| Area | Limitation | Work order |
| --- | --- | --- |
| Identity | Cells aren't told apart from each other when it comes to schedules: a cell can list and cancel any schedule an agent session made, though never the owner's. (Every agent session is bound to the cell the broker spawned: WO-007.) | [Candidate](../workorders/README.md#candidates) |
| Resources | A configured cgroup v2 delegation bounds each whole cell's tasks, memory and CPU; cleanup kills the group. Without `HATCHLING_CELL_CGROUP`, hard rlimits and the runtime bound remain, but aggregate memory and CPU are unbounded and `RLIMIT_NPROC` shares the real host uid's budget. Operators needing whole-cell bounds must delegate a group; unusable configured delegation fails closed. Disk quotas and network bandwidth limits remain outside scope. | [WO-004](../workorders/workorder-004-cell-resource-limits.md) |
| Filesystem | The memory and goals stores still repair a directory's mode by path after checking it, rather than through a no-follow handle; only the broker's own user can write where they live. | [Candidate](../workorders/README.md#candidates) |
| Local users | The frontend host listens on `127.0.0.1`, which every local user can reach. It serves only its own pages and refuses other sites, but against another local user the UI and operator tokens are the gate. The host prints its addresses, tokens included, to its standard output: under a service manager they land in its journal, so keep that as private as the token files. | None yet |
| Egress | An upstream that echoes a credential back re-encoded (base64, hex) gets past the scrub, which matches its exact bytes. The upstream is the service the operator gave the credential to. | None yet |
| Hygiene | The broker wipes the copies of held secrets it frees: the vault's reads, keyring and backup codes, the output and artifact gates' sets of held secrets in every form they look for, and a fetched secret's reply frame (WO-011). The agent holds a fetched secret for its run, by design, and its cell's memory goes with the cell. The redaction audit log records a redacted message by an HMAC tag under a random key sealed beside the kept texts, so the log alone confirms no guess at it; records and overrides from before the tags keep their unkeyed hashes. A security key asks for its PIN only when enrolled to (`HATCHLING_REQUIRE_KEY_PIN`); by default a touch opens the vault. A host crash handler on a socket (`core_pattern` starting `@`, Linux 6.16 and later) isn't yet covered by the one-byte core limit below. | [Candidate](../workorders/README.md#candidates) (the rest: [WO-011](../workorders/workorder-011-secret-hygiene.md), done) |

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
