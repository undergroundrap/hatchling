# Hatchling for security reviewers

The answers a security review asks for, in one place, each linked to the full account. If something you need isn't here, [open a discussion](https://github.com/undergroundrap/hatchling/discussions). For a vulnerability, use [SECURITY.md](../../SECURITY.md) instead.

## What it is, and who it's for

Hatchling is a personal AI agent that runs on the owner's own Linux machine, or under WSL2, and keeps running. One owner per installation. There are no other users, tenants, roles or single sign-on. Access control is about keeping the agent, the code it runs and the content it reads away from the owner's secrets and accounts.

- **The trusted side:** the broker, the vault keeper and the egress authority, each a process of its own, run by the owner.
- **Everything else is untrusted:**
  - the agent;
  - the skills and coding CLIs it runs;
  - every page, file and message it reads;
  - the model's output.

  All of it runs in kernel-isolated cells: user, mount, network, IPC and pid namespaces; a seccomp allowlist; no capabilities; and a cgroup bound.

[The trust boundary](../arch/trust-boundary.md) is the full threat model: each threat, the mechanism that stops it, the test that proves it, and the known limitations.

[The bugs an always-on agent invites](bug-classes.md) takes the bug classes agents like this one commonly have, such as a local UI any website can reach, memory poisoning and keys in the agent's environment, and names the test that closes each in Hatchling.

## What leaves the machine

Hatchling sends nothing anywhere on its own: no telemetry, crash reports, update checks, analytics or license checks. When it runs a vendor's coding CLI, it turns that CLI's own telemetry off. Every connection out is one of these, and each needs the operator to configure it or the owner to allow it:

| What | Where it goes | What it carries | Who turns it on |
| --- | --- | --- | --- |
| Model requests | The model endpoint the operator configures: a hosted provider, or a model on the same machine | The conversation, the agent's tool results and anything recalled from memory. Held secrets are redacted from every request. | The operator ([Hosted models](../hosted-models.md)) |
| Web search | The search provider (Brave), through the egress authority | The agent's search queries | The operator (`HATCHLING_SEARCH`) |
| Reading a page | The site, once the owner allows it | A `GET` for the page's address, which the agent chose. No credential is sent. | The operator turns the web on (`HATCHLING_WEB`); the owner allows each site |
| The agent's browser | The sites the owner allows, and the sites of a signed-in profile they kept | What the agent types and clicks. Signed in, every act that could send something waits for the owner first. | The operator sets up the browser; the owner allows sites and keeps profiles |
| Coding CLIs | The CLI vendor's API, through a relay that swaps a surrogate for the real key | The files the agent names for the task, and the CLI's own requests | The operator configures each CLI |
| Destinations the operator grants | Each allowlisted host, through the egress proxy | What the agent sends there; the credential is attached at the boundary | The operator (`HATCHLING_EGRESS_ALLOWLIST`) |

The release security checklist's [item 7](../release-security-checklist.md#7-egress-has-one-way-out) lists every outbound path in the code, and each release checks that list again.

## What's kept, and how

- **Where:** [What Hatchling keeps on disk](../storage.md) lists every file, says which are the owner's data and which are disposable, and covers backup, restore and uninstall.
- **How it's sealed:**
  - Secrets, schedules, artifacts, spaces, the conversation and kept messages are sealed with XChaCha20-Poly1305, each store under a key of its own.
  - Those store keys are wrapped under a master key that only the vault keeper holds.
  - The master key is unlocked by a file, a keyring of FIDO2 security keys, TOTP, or backup codes, as the owner chooses.
- **Not in a cell:** a cell never holds a real credential. Credentials are attached at the network boundary, by the egress authority or the coding-CLI relay.

## Logs and audit trails

| Log | Where | What it records |
| --- | --- | --- |
| The broker's log | stderr: the journal, under systemd | What the broker does and refuses. Untrusted text is escaped, and no secret value is written. |
| The activity log | `<scheduler dir>.activity` | Every call to the local API, by connection; subagent activity |
| The vault's access log | `.access.log` in the vault directory; read it with `hatchling-broker vault-audit log` | Every read of a secret, allowed or denied, by purpose and requester |
| The output gate's audit log | `audit.log` in the redaction directory; review it with `hatchling-broker redactions list` | Each message redacted before it reached a person, and why |
| Egress decisions | The operator socket's `egress` (the last 200) | Each request out: the cell, the destination and the verdict; never a body or a header |

## Hardening checklist

1. **Run the broker as root, with the four system users the README creates**, rather than rootless. A root broker gives cells a uid of their own, and drops to a user of its own once started. The vault keeper and the egress authority each run as a separate user.
2. **Delegate a cgroup** (`HATCHLING_CELL_CGROUP`) so each cell's processes, memory and CPU are bounded as a whole.
3. **Unlock the vault with a FIDO2 key** (`vault-keyring`) instead of a key file.
4. **Keep the browser bundle current.** The browser cell runs Chromium; update it with the browser's own security releases, and pin its digest again (`browser-digest`).
5. **Keep the kernel patched.** A cell is as strong as the kernel under it.
6. **Mind other local users.** The app's host listens on `127.0.0.1:7340`, which any local user can reach. It serves only its own pages and checks every request's host and origin. On a shared machine, see the trust boundary's [Local users](../arch/trust-boundary.md#known-limitations) row.
7. **Verify what you install.** Check releases against their attestations, or rebuild them to the same bytes ([Verifying a release](../releasing.md#verifying-a-release)).

## If you suspect a compromise

1. **Stop the broker.** Every cell ends with it.
2. **Rotate whatever it held:**
   - each provider key and site credential in the vault, replaced at its source;
   - the master key, with `hatchling-broker rekey-vault`.
3. **Review what was read:**
   - `hatchling-broker vault-audit log` lists every secret read and every refusal;
   - the activity log and the operator socket's `egress` show what the agent did and where it reached.
4. **Revoke access:**
   - sign out or delete the browser's signed-in profiles, and revoke the sites the owner allowed always, both in the app's Settings;
   - remove a lost security key from the vault (`vault-keyring remove-key`).
5. **Report it** if Hatchling's own code let it happen: [SECURITY.md](../../SECURITY.md).

## Releases and the supply chain

- **Builds:**
  - every release is built by CI from a tag on `main`, twice per target, on separate runners;
  - the release goes out only if the two builds are byte-identical;
  - each file carries a build-provenance attestation and a CycloneDX SBOM.
- **crates.io:** crates are published through trusted publishing, and no job that builds holds a token.
- **Dependencies:** they're reviewed ([dependency review](../dependency-review.md)), and `cargo deny` checks them in every gate.
- **Per release:** each one runs the [release security checklist](../release-security-checklist.md).
- **Details:** see [Releasing](../releasing.md).

## Reviews and audits

- **The release security checklist** runs on every release, against everything that changed since the one before.
- **An internal audit after 0.5.0** reviewed the whole codebase in six lanes:
  1. attacks reported against other agents and sandboxes;
  2. the supply chain;
  3. isolation;
  4. the trust boundary;
  5. prompt injection;
  6. adoption.

  It found no way out of a cell, and 3 High, 11 Medium and 20 Low findings. 0.6.0 fixes all three Highs, four of the Mediums and four of the Lows, each pull request naming the attack it stops:

  | Finding | Severity | Fixed in |
  | --- | --- | --- |
  | Signed-in browsing had no confirmation rail: a page the agent read could make it act on a site you're signed in to | High | [#277](https://github.com/undergroundrap/hatchling/pull/277) |
  | The spawner checked a cell's paths by name: a link the broker made, before or after the check, could put the owner's home in a cell | High | [#292](https://github.com/undergroundrap/hatchling/pull/292), [#294](https://github.com/undergroundrap/hatchling/pull/294) |
  | The spawner passed the broker's descriptors to a cell as its stdio unchecked: a directory there opened the host's files to it | High | [#291](https://github.com/undergroundrap/hatchling/pull/291) |
  | "Allow once" approved the site, not the page shown | Medium | [#276](https://github.com/undergroundrap/hatchling/pull/276) |
  | The crates.io publish job built dependencies while holding the token | Medium | [#275](https://github.com/undergroundrap/hatchling/pull/275) |
  | A run cell's program wasn't pinned, so the broker's own program could take its vault channel | Medium | [#295](https://github.com/undergroundrap/hatchling/pull/295) |
  | Two broker threads started before its confinement, and went on unconfined | Medium | [#297](https://github.com/undergroundrap/hatchling/pull/297) |
  | One task's denials could keep the owner from being asked by any other | Low | [#284](https://github.com/undergroundrap/hatchling/pull/284) |
  | A build's card showed its inputs' hashes, not their text | Low | [#285](https://github.com/undergroundrap/hatchling/pull/285) |
  | The held-secret check missed page addresses and search queries | Low | [#283](https://github.com/undergroundrap/hatchling/pull/283) |
  | A hard link in a work tree could have had a host file's owner changed | Low | [#296](https://github.com/undergroundrap/hatchling/pull/296) |

  One reported Low, agent links hiding their address, was a false positive: the chat already shows an agent's links inert. The seven Mediums and fifteen Lows still open are tracked privately and fixed in the open like these, each one's details published with its fix.
- **No independent third-party audit has been done yet.**

## Upgrading

Before 1.0, a minor version (0.x.0) may change the configuration or the wire protocol. Each version's [changelog](../../CHANGELOG.md) entry marks every breaking change. Back up as [storage](../storage.md#back-up-and-restore) says before upgrading.
