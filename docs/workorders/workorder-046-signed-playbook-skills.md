# WO-046: Signed playbook skills: manifest v2, a signer, discovery, and the first skills

**Status:** CLOSED
**Area:** `crates/hatchling-skill` (the manifest, admission, the registry), `crates/hatchling-broker` (loading, the skill ops, the agent's prompt), `crates/hatchling-core` (the session wire), `crates/hatchling-agent` (the skill tools), a signing command, `docs/`
**Issue:** [#594](https://github.com/undergroundrap/hatchling/issues/594)
**Trust boundary:** yes

## Problem

An always-on agent is judged by what it can do, and much of that comes from skills: written know-how for a kind of work, often with a script or two, that the agent follows with its own tools. Hatchling has the strongest part of skills and none of the rest.

What it has is the trust core. A skill is admitted only when an Ed25519 signature verifies against an owner-held trust root, the root isn't a weak key, the manifest is valid, and every capability it declares is granted. The registry pins the hash of the skill's real bytes and refuses one that drifts. Each call is checked again at use, and runs in a cell of its own with no broker socket and bounded input and output. No skill can be swapped, poisoned or widened without that being refused.

What it doesn't have:

- **Know-how.** A v1 manifest is a name, a version, capabilities and one program to run. It has no description and no instructions, so a skill can't be a playbook, which is the kind of skill most agents ship and most people want.
- **A way to sign.** Signing keys appear only in tests. No command makes a key or signs a package, so no person can make a skill.
- **Discovery.** The only skill operation is to invoke one by its exact `name@version`. Nothing lists what's installed, and the agent's prompt has a fixed paragraph about skills, so an installed skill is invisible to the model.
- **Content.** No skill package exists outside the tests. The two programs built to be skills, `hatchling-build-document` and `hatchling-build-page`, have never been packaged.
- **A way to turn it on.** Skills are off unless three settings are written by hand, and an install ships none of them.

The maintainer chose, on 2026-11-22, that Hatchling's skills be **signed playbooks**: know-how and code together, under signature. The signature is what makes them Hatchling's: a skill you can trust names who wrote it, and a skill no one signed, or that changed after signing, isn't followed.

## Scope

**In scope, design first:**

1. **The design and its threat model, approved by the maintainer before any code.** At least: what the agent may do because a skill said so (it acts with its own tools, grants and approvals, and a skill widens none of them); how a skill's signed instructions reach the model, and how they differ from the owner's words and from untrusted content; what a signer is trusted for and what a compromised signer can do; what an owner sees before a skill is admitted; and what a skill's optional program can reach (the v1 cell, unchanged, unless the design says otherwise).
2. **Manifest v2.** A signed description (what the skill is for, for discovery) and signed instructions (the playbook, and any reference files it names), with the v1 program optional. Every byte the agent can be given is covered by the signature and pinned by hash. v1 manifests still load.
3. **A signer.** A command that makes a signing key and signs a package, run by its author in a terminal of their own. The private key never passes through the broker, an argument or the environment.
4. **Discovery.** A skill operation that lists what's admitted, with each description; the agent's prompt says what's installed from the live registry, not a fixed paragraph; and a tool that reads an admitted skill's instructions.
5. **The first skills.** A few playbooks for everyday work, each signed and admitted end to end. The two builders moved to [#622](https://github.com/undergroundrap/hatchling/issues/622) (the maintainer, 2026-11-23): a signed program is pinned by hash, and theirs change with every release.
6. **Authoring documented.** How to write, sign and install a skill, for a person and for the agent itself.

**Decided by the maintainer, not by this work order:** whether an install ships a trust root (a key Hatchling's releases sign skills with) and a skills directory. Today the default is to trust nothing; changing it is a trust decision of its own. **Decided 2026-11-23:** an install ships the maintainer's public key and Hatchling's skills, each off until the owner enables it; the owner can take the key out, and an update keeps their trust-roots file as it is.

**Not in scope:** skills that learn or write themselves from use; a public skill registry; credentials for skills (today a skill cell gets none, [#593](https://github.com/undergroundrap/hatchling/issues/593)).

## Design (for the maintainer's approval, before any code)

### What exists, and what it gets wrong

Read from `main` at `dd3d81f1`. A v1 skill is a directory: `manifest.json` (name, version, capabilities, entry point, Ed25519 signature) and one program. Admission checks, in order, that a signature is present, that it verifies under a key in the trust-roots file (at most two, the file owned by the broker and writable by nobody else), that the manifest is valid, and that every declared capability is granted (`HATCHLING_SKILL_GRANTS`). Each call is checked again, and runs in a cell of its own: no broker socket, no credentials, bounded input and output, its stdout given to the model as untrusted data.

Two things are wrong, and the design must fix both rather than build on them:

- **The signature doesn't cover the program** ([#613](https://github.com/undergroundrap/hatchling/issues/613)). It covers the manifest's four fields. The program is pinned by its hash when the broker first sees it, until a restart, and nothing checks who can write the skills directory. A signature has to cover everything the skill can put in front of the agent or run.
- **The source says the skill cell injects credentials** ([#593](https://github.com/undergroundrap/hatchling/issues/593)). It doesn't, and nothing here adds it.

### The threat model

The new thing is that a skill's **words** reach the model and guide what it does. Who can hurt the owner through a skill, and how:

| Who | What they could try | What stops it |
|---|---|---|
| Whoever can write the skills directory | Change a skill's instructions, files or program | Every file is signed by hash and checked on every load; the directory is held to the trust-roots file's rule (owned by root or the broker, nobody else may write) |
| A signer whose key is stolen, or who turns hostile | Publish instructions that steer the agent: to read what it shouldn't, send data out, ask the owner for approvals, or waste time and tokens | A skill grants nothing (below); the owner enables each skill, sees its signer, and can disable it or drop the signer's key; every action still goes through the same grants, egress checks and approvals |
| A web page, file or tool result | Pretend to be a skill's instructions ("[SKILL] do X") | Instructions reach the model only through the broker, fenced by markers it neutralizes inside content, as recalled memory is |
| The agent, misled | Install, change or sign a skill | No session can write the skills directory, the trust roots or the owner's enablements, and the agent never holds a signing key |
| A skill's program | Reach the broker, the vault or the network beyond its grant | Unchanged from v1: its own cell, no broker socket, no credentials, bounded I/O |

**The rule that makes this safe: a skill grants nothing.** Its instructions are guidance for a task the owner asked for. They can't widen the agent's tools or grants, can't approve anything, and rank below the owner's own words. A playbook that tells the agent to do something it isn't granted is refused at the grant, exactly as the same request from a web page would be. Signing decides *whose words these are*; it never decides *what the agent may do*.

### Manifest v2

A v2 skill is a directory: `manifest.json` and the files it lists.

```json
{
  "format": 2,
  "name": "write-a-report",
  "version": "1.0.0",
  "description": "Turns notes and sources into a structured report with citations.",
  "instructions": "SKILL.md",
  "files": [
    { "path": "SKILL.md", "bytes": 4210, "sha256": "…" },
    { "path": "templates/report.md", "bytes": 912, "sha256": "…" },
    { "path": "scripts/outline.py", "bytes": 2301, "sha256": "…" }
  ],
  "program": null,
  "signature": "…"
}
```

- **Signed payload:** the domain tag `hatchling-skill-manifest-v2\n`, then the manifest's fields as compact JSON in declaration order, the signature excluded, as v1 does. A v2 signature can never be read as v1's, or the reverse.
- **Every file is listed**, with its size and SHA-256. The loader reads exactly the listed files, refuses one whose size or hash differs, refuses a link anywhere, and serves no file that isn't listed. Paths are plain relative paths, as v1's entry point is.
- **`description`:** at most 512 bytes, one or two sentences. It's the trigger: discovery shows it, so it says when the skill applies, when a similar task belongs elsewhere, and what the owner has at the end (see *Authoring*).
- **`instructions`:** the playbook, a listed Markdown file of at most 32 KiB.
- **`program`:** optional. When present, `{ "path": "…", "capabilities": ["…"] }`, run exactly as a v1 skill's entry point is, in its own cell.
- **Bounds:** at most 64 files and 1 MiB in all, per skill; at most 64 skills.
- **v1 is retired.** No v1 package exists outside the tests, and v1's signature doesn't cover its program (#613). The loader refuses v1 manifests with a message saying to re-sign as v2, and the v1 tests move to v2. (This changes the work order's acceptance line "v1 manifests still pass", which the maintainer approves with this design.)

### The signer

Signing happens where the author is, never in the broker:

- `hatchling skill key new <file>` makes an Ed25519 key, written to `<file>` with mode 0600, refusing to overwrite, and prints the public key and its fingerprint, which go in a trust-roots file.
- `hatchling skill sign <dir> --key <file>` lists every file in `<dir>` with its hash, writes the v2 manifest and signs it. The key is read from the file, never from an argument or the environment, and never printed or logged.
- `hatchling skill check <dir> --root <public key>` verifies a package as the loader will, so an author sees exactly what an install would do.

These are commands of the `hatchling` program an author runs in their own terminal. The broker has no signing code and never sees a private key. A key protected by a security key, rather than a file, is left for later.

### Admission: what the owner sees and decides

Today a skill runs if its signer's key is in the trust-roots file and its capabilities are in an environment setting. With instructions that steer the agent, that isn't enough: **the owner enables each skill**, as they approve each memory.

- The broker finds skills that verify, and lists them for the owner in the app: name, version, description, the signer's fingerprint (and the name the owner gave that key), the capabilities its program asks for, and its files.
- Enabling one is the owner's act, in the app, never a session's. It's bound to the skill's signed digest, so a changed skill (a new version, or a new file) waits for the owner again, with what changed shown.
- Disabling a skill, or removing a signer's key, takes effect at once.
- Enablements are sealed, as grants and decisions are.

### Discovery and use

- **The prompt lists what's enabled and usable in this run**, from the live registry: each skill's name and description, up to 12 KiB, fenced as data; past that, it says how many more there are. That replaces today's fixed paragraph. A playbook that needs the agent's computer is listed only in a chat, since a run with nobody there has none.
- **`skill_list`, a new tool:** every enabled skill's name and description, for when the prompt's list is full.
- **`skill_read`, a new tool:** gives the agent a skill's instructions, or one of its listed files, on demand, fenced by markers the broker neutralizes inside the text, and labelled with the skill and its signer. Only what the agent asks for enters the context.
- **`skill_invoke`** stays for a skill with a program, unchanged.
- **A skill's files in the Room:** in a chat, an enabled skill's listed files are mounted read-only at `/workspace/skills/<name>/` in the agent's computer, from the broker's verified copy, so a playbook's helper scripts run there under the Room's rules (`python3 /workspace/skills/write-a-report/scripts/outline.py`). The maintainer chose `/workspace/skills` over `/skills`, which would need a new Room image ([evidence](../evidence/skill-room-mount.md)). Nothing a skill brings runs anywhere else.
- **Activity** records each skill read and used.

### The first skills

- `hatchling-build-document` and `hatchling-build-page`, as v2 skills with a program: moved to [#622](https://github.com/undergroundrap/hatchling/issues/622).
- Three playbooks for everyday work, from the maintainer's drafts: *write-a-report* (material and sources into a report file whose every load-bearing claim traces to a source read), *tidy-a-folder* (inventory, a plan of moves, removals and what's left alone, shown and approved before anything changes, then verified), and *research-a-question* (the workspace first, then granted web sources read in full, cross-checked, answered in the chat with what couldn't be verified said plainly). Each is written against tools a run already has, and says what to do when one isn't there.
- Signed with a key the maintainer makes, and shipped with that key as a trust root (the maintainer's decision above), in the installer's `packaging/`.

### Authoring

A person writes a directory and signs it with `hatchling skill sign`. The agent can help: it drafts a skill in its computer and gives the folder to the owner, who reads it and signs it in their own terminal. The agent never signs and never installs.

**The anatomy of a playbook that works**, documented for authors (slice 4) and followed by the first skills:

1. **Name and version.** Short and stable. Any change to any file is a new version, re-signed and enabled again.
2. **The description is the trigger.** When it applies, when a similar task belongs elsewhere, and what the owner has at the end. A description that only says what the skill is gets fetched for the wrong tasks and missed for the right ones.
3. **Purpose and inputs.** One line on what a finished run looks like; what to establish before starting; what to ask the owner when an input is missing, instead of guessing.
4. **Steps.** Imperative, ordered, each checkable, naming the tools as the run knows them, and saying what to do when a step fails: retry once, skip and say so, or stop.
5. **Guardrails.** What the playbook never does, in the same voice as its steps.
6. **Stop and ask, and the report.** When to end early and go back to the owner, and what the answer must hold: what was done, where the result is, and what couldn't be verified.

Supporting files are listed in the manifest and referred to by name, never inlined: the playbook is read in the middle of someone's task, so it stays short.

**Two rules follow from the design.** A playbook grants nothing, so it never has a step that needs an approval, a grant or a tool the run may not hold: it writes the fallback instead ("if this site isn't granted, ask through the usual request and go on with what you have"). And it serves the owner's task: a playbook that tells the agent to hide its reasoning, skip a check, or present something unverified as verified is hostile, whoever signed it. No scanner is trusted to find one: what holds is that a skill grants nothing, and that the owner reads the playbook before enabling it.

### Slices

1. v2 format, the loader's checks on every file, the skills directory's ownership rule, v1 retired, and the signer commands (#613 closed).
2. Owner enablement: the app's list, enable and disable, sealed enablements, re-approval on change.
3. Discovery: the prompt from the registry, `skill_read`, the Room mount, Activity.
4. The first skills, signed, used on a fresh install; authoring documented. The builders follow in #622.

## Acceptance criteria

- [x] The design and threat model are written and approved by the maintainer before any code lands. (The design above, merged with the maintainer's approval in #614 before slice 1.)
- [x] A v2 skill whose description, instructions or program changed after signing is refused, and a test changes each. ([slice 1a](../evidence/skill-manifest-v2.md): a changed manifest field fails its signature, and a changed playbook or program fails its listed hash; [slice 4](../evidence/first-skills.md): a shipped playbook changed by one line is refused by a root broker.)
- [x] A skill's instructions can't widen the agent's tools, grants or approvals: a playbook that tells the agent to do what it isn't granted is refused at the grant, as any request is, and a test shows it. ([slice 4](../evidence/first-skills.md): `a_playbook_the_owner_enabled_widens_no_grant`; [slice 3a](../evidence/skill-discovery.md): a run's skill ops reach no harness.)
- [x] The signer makes a key and signs a package that the loader admits; no private key appears in an argument, the environment or a log, and a test checks it. ([slice 1b](../evidence/skill-signer.md); and the maintainer signed the first skills with it, [slice 4](../evidence/first-skills.md).)
- [x] An admitted skill is listed with its description, appears in the agent's prompt, and its instructions are read through the new tool, on a real install. ([slice 3a](../evidence/skill-discovery.md) through a real broker session; [slice 4](../evidence/first-skills.md): CI's installs find each shipped skill, and the browser job enables one in the app and the agent reads it. The use runs on a root broker given the installed layout, not through `install.sh` itself.)
- [x] At least three playbooks ship as signed packages, each admitted and used on a fresh install. (The two builders moved to #622, the maintainer's choice on 2026-11-23.) ([slice 4](../evidence/first-skills.md): all three admitted on CI's install; `write-a-report` used from the app, the other two shown waiting.)
- [x] v1 manifests are refused with a message to re-sign as v2 (no v1 package exists outside the tests, and v1's signature doesn't cover its program, #613); the v1 tests move to v2. ([slice 1a](../evidence/skill-manifest-v2.md).)
- [x] Every listed file is checked against its signed hash on every load, a changed or unlisted file is refused or never served, and the skills directory is refused when anyone but root or the broker can write it; tests change each. ([slice 1a](../evidence/skill-manifest-v2.md), and at each read, [slice 3a](../evidence/skill-discovery.md).)
- [x] The owner enables each skill in the app, bound to its signed digest; a changed skill waits for the owner again; a test changes one file and checks the skill is held. ([slice 2a](../evidence/skill-enablement.md), [slice 2b](../evidence/skills-settings.md).)
- [x] Each new guard fails its test when removed. (Each slice's hand-breaks: [1a](../evidence/skill-manifest-v2.md), [1b](../evidence/skill-signer.md), [2a](../evidence/skill-enablement.md), [2b](../evidence/skills-settings.md), [3a](../evidence/skill-discovery.md), [3b](../evidence/skill-room-mount.md), [4](../evidence/first-skills.md). Two breaks failed nothing, and each note says why: 3a's filter was removed, and 3b's second mount check stays.)

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- First on v0.10.0's agent track, by the maintainer's decision of 2026-11-22; WO-045 (the evaluation harness) comes last, so Hatchling's model benchmarks measure the agent people will run.
