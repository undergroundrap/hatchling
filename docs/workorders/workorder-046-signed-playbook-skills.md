# WO-046: Signed playbook skills: manifest v2, a signer, discovery, and the first skills

**Status:** QUEUED
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
5. **The first skills.** At least the two builders as signed packages, and a few playbooks for everyday work, each signed and admitted end to end.
6. **Authoring documented.** How to write, sign and install a skill, for a person and for the agent itself.

**Decided by the maintainer, not by this work order:** whether an install ships a trust root (a key Hatchling's releases sign skills with) and a skills directory. Today the default is to trust nothing; changing it is a trust decision of its own.

**Not in scope:** skills that learn or write themselves from use; a public skill registry; credentials for skills (today a skill cell gets none, [#593](https://github.com/undergroundrap/hatchling/issues/593)).

## Acceptance criteria

- [ ] The design and threat model are written and approved by the maintainer before any code lands.
- [ ] A v2 skill whose description, instructions or program changed after signing is refused, and a test changes each.
- [ ] A skill's instructions can't widen the agent's tools, grants or approvals: a playbook that tells the agent to do what it isn't granted is refused at the grant, as any request is, and a test shows it.
- [ ] The signer makes a key and signs a package that the loader admits; no private key appears in an argument, the environment or a log, and a test checks it.
- [ ] An admitted skill is listed with its description, appears in the agent's prompt, and its instructions are read through the new tool, on a real install.
- [ ] The two builders and at least three playbooks ship as signed packages, each admitted and used on a fresh install.
- [ ] v1 manifests and their tests still pass.
- [ ] Each new guard fails its test when removed.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- First on v0.10.0's agent track, by the maintainer's decision of 2026-11-22; WO-045 (the evaluation harness) comes last, so Hatchling's model benchmarks measure the agent people will run.
