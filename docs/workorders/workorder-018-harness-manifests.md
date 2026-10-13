# WO-018: Harness manifests: add a coding CLI by configuration, with OpenCode first

**Status:** QUEUED
**Area:** `crates/hatchling-broker` (`harness.rs`, `harness_relay.rs`, config), `docs/`
**Issue:** [#46](https://github.com/undergroundrap/hatchling/issues/46)
**Trust boundary:** yes

## Problem

A harness is an external coding CLI that Hatchling runs on a scoped task, inside a cell: today, Claude Code and the Codex CLI. Each one is a variant of `HarnessKind`, compiled into the broker. Adding a CLI, OpenCode for one, means changing Hatchling's code and waiting for a release, though what a harness needs from the broker is the same every time: its binary pinned by hash, a cell with only its work tree writable, the vendor API it speaks reached through the broker's relay with a surrogate key, and the model request policy on everything it sends.

So the broker should take a harness as configuration: a manifest the operator writes, which says how to run the CLI and which API it speaks, while every protection stays the broker's.

## Scope

**In scope:**

- **A manifest format**, one file per harness, in a directory the operator names: the harness's name; its binary's path and SHA-256, and any companion files it needs, each pinned the same way; the API it speaks (`anthropic` or `openai`, the two wires the relay has); the destination it reaches; its command line, with a placeholder for the task; and the environment variables that point it at the relay and carry its surrogate key.
- **Loading at startup:** each manifest is validated, and the directory and its files must be owned by the broker's user and writable by no one else, like the vault. An invalid manifest is refused, with a message naming the field; it never runs half-configured.
- **Running:** a manifest-defined harness runs exactly as the built-in ones do: a CLI cell, the relay with a surrogate, the request policy, the work-tree copy, the patch computed by Hatchling, the activity feed, the timeout.
- **OpenCode**, as the first harness defined only by a manifest: the manifest, documented, and tested end to end.
- **Docs:** how to write a manifest, and the OpenCode one as the worked example.

**Not in scope:**

- The agent adding, changing or choosing a harness that isn't configured. Adding one is the operator's decision.
- Installing CLIs. The operator installs one and pins it; Hatchling bundles none.
- Moving Claude Code and the Codex CLI to manifests. They stay built in: each has quirks of its own (Codex's code-mode host), and moving them is a follow-up once manifests have run in the field.
- Vendor APIs beyond the relay's two wires.

## Acceptance criteria

- [ ] A harness defined only by a manifest runs a task in a CLI cell: it reaches its vendor only through the relay, with a surrogate key, never the real one; the request policy applies to what it sends; its patch is computed by Hatchling. Tested with a scripted CLI and a scripted vendor, as the built-in harnesses are.
- [ ] OpenCode, at a version pinned in the evidence, runs a task through its manifest end to end against a scripted vendor, with `HATCHLING_REQUIRE_CELL=1`.
- [ ] A manifest is refused at startup, naming the field, when: its binary's hash doesn't match; its API isn't one the relay speaks; its command line has no task placeholder; its file or directory is writable by anyone but the broker's user; or it names a harness that already exists.
- [ ] A binary that changes after startup is refused before the next run, as for the built-in harnesses.
- [ ] The docs show how to write a manifest, with OpenCode's as the example.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Filled in at close.

## Notes

- **Why configuration, not the agent's choice.** A harness gets a network (the relay) and a key that spends money on the operator's account. A prompt-injected agent able to define one could point an arbitrary program at both. The manifest keeps that decision with the operator, and the broker keeps every check.
- **Why OpenCode first.** It's open source, widely used, and speaks the vendor APIs the relay already has, so it proves the format without new wire code.
