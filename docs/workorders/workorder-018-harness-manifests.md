# WO-018: Harness manifests: add a coding CLI by configuration, with OpenCode first

**Status:** DONE
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

- [x] A harness defined only by a manifest runs a task in a CLI cell: it reaches its vendor only through the relay, with a surrogate key, never the real one; the request policy applies to what it sends; its patch is computed by Hatchling. Tested with a scripted CLI and a scripted vendor, as the built-in harnesses are.
- [x] OpenCode, at a version pinned in the evidence, runs a task through its manifest end to end against a scripted vendor, with `HATCHLING_REQUIRE_CELL=1`.
- [x] A manifest is refused at startup, naming the field, when: its binary's hash doesn't match; its API isn't one the relay speaks; its command line has no task placeholder; its file or directory is writable by anyone but the broker's user; or it names a harness that already exists.
- [x] A binary that changes after startup is refused before the next run, as for the built-in harnesses.
- [x] The docs show how to write a manifest, with OpenCode's as the example.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates).

## Evidence

Tests in `crates/hatchling-broker` and `crates/hatchling-sys`. The real-cell ones ran in the sandbox with `HATCHLING_REQUIRE_CELL=1`, on 1.98.1 and 1.85.0.

1. **A manifest's harness runs as a built-in one does.** `harness_cli::a_harness_defined_by_a_manifest_runs_as_a_built_in_one`, in a real CLI cell. The stub harness is defined only by a manifest, with Anthropic's API and the scripted vendor as its destination.
   - Its request with the surrogate is forwarded, and the vendor sees the vault's key.
   - Another path is answered `404` by the relay itself, and the real key from inside is answered `401`.
   - Dialling the vendor directly fails.
   - Its environment holds the surrogate and the relay's URL, never the key.
   - Hatchling computes its patch, and the run is in the activity feed under its name, with the relay's counts (one forwarded, two refused).
   - The unit level is covered by `harness::tests::a_manifests_harness_is_configured_granted_and_named_as_a_built_in_one` and `a_manifests_harness_runs_with_its_own_key_companions_and_grant`: its own vault secret, key shape and environment, companions copied from verified bytes, its grant, and a vault grant naming its key refused.
2. **OpenCode, end to end.** `harness_cli::real_opencode_runs_by_its_manifest_in_its_cell_through_the_relay` ran OpenCode 1.19.5, defined by the manifest `docs/harness-manifests.md` shows:
   - **Binary:** `opencode-linux-x64.tar.gz` from the project's GitHub release, SHA-256 `588310a336fa3b9df36b3f0225b26f679d453efaad0cf3d66c3651ef3ec1647a` as GitHub publishes it; the binary is `7df4e4136301f16589b0f30bf91a61380fca5f8ef22dd36570f5ebe1d4ec802c`.
   - **The session:** in a real cell against the scripted vendor, it read and rewrote `notes.txt`, ran a shell command through the approved tools, and answered.
   - **What came back:** the patch, the project untouched, and every request at the vendor with the real key.
3. **Refused at startup, naming the field.** `harness_manifest::tests::a_wrong_field_refuses_the_start_naming_it` covers each field:
   - a binary that hashes to something else (`binary.sha256`);
   - an API the relay doesn't speak (`api`);
   - no `{task}` (`args`);
   - a name that's a built-in harness's (`name`), among the rest.

   `only_its_user_or_root_may_write_a_manifest` refuses a manifest or a directory writable by group or others (`owner`). `a_directory_holds_manifests_and_nothing_else` refuses a stray file, a link, and an oversized manifest. A manifest's name is its file's, so two can't share one.
4. **A binary changed after startup.** The same `harness_cli` test rewrites the pinned binary after the start: the next run is refused, `never substituted`, and nothing reaches the vendor. A companion changed after the start is refused the same way (`a_manifests_harness_runs_with_its_own_key_companions_and_grant`).
5. **The docs.** [`docs/harness-manifests.md`](../harness-manifests.md): the format, each field, what's refused, and OpenCode's manifest as the worked example.

## Notes

- **What OpenCode needed of the cell.** In a real cell the CLI profile's filter killed OpenCode three times, each call named from the kernel's audit log (`type=1326`). Each was changed only after it was seen killing OpenCode, as the browser profile's calls were:
  - **`sendmmsg` (307), from Bun's thread pool.** glibc's resolver sends a name's two queries with it, and OpenCode looks up `registry.npmjs.org` at start. The profile now answers it with `ENOSYS`, as it does `clone3`. That's a refusal, not an allowance: the lookup fails as it would with no network, the cell having no resolver to reach, and the CLI goes on. Checked by `seccomp::tests` and, on the live kernel, `adversarial::cli_rules_refuse_as_designed`.
  - **`epoll_wait` (232), from its file watcher.** It's `epoll_pwait` with no signal mask, which the cell has.
  - **`pwritev2` (328).** It's the write that `preadv2` (which Claude Code reads with) pairs with, on a descriptor the cell holds, as `pwrite64` and `writev` are.

  Both are allowed now. With the three changes, OpenCode runs the whole session in its cell, and the kernel logs no more kills.
- **The answer, from OpenCode's JSON.** `run --format json` prints events, one per line. The harness's final message is now also the last `text` event's text, beside Claude Code's `result` and the Codex CLI's last agent message.
- **Whose a manifest is.** The broker's user's or root's, and writable by no one else. Root's, because a root broker's vault keeper and egress authority run as their own users and read the manifests too. Root can change anything those processes do anyway.
- **A manifest's key.** The vault secret `harness-<name>`, the built-in harnesses' rule. A vault grant naming it refuses the start, as for the built-in harnesses: it would hand the key to the agent's cell.
- **Why configuration, not the agent's choice.** A harness gets a network (the relay) and a key that spends money on the operator's account. A prompt-injected agent able to define one could point an arbitrary program at both. The manifest keeps that decision with the operator, and the broker keeps every check.
- **Why OpenCode first.** It's open source, widely used, and speaks the vendor APIs the relay already has, so it proves the format without new wire code.
