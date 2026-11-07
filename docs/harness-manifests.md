# Harness manifests

A harness is a coding CLI that Hatchling runs on a scoped task, in a cell. Claude Code and the Codex CLI are built in. Any other CLI that speaks one of the relay's two vendor APIs you add with a **manifest**: a file saying how to run it. You don't need a new Hatchling release ([WO-018](workorders/workorder-018-harness-manifests.md)).

A manifest only says how to run the CLI. Every protection stays the broker's, as it is for the built-in harnesses:

- **A CLI cell.** Only the run's work tree and scratch directory are writable. The cell runs only the [approved tools](approved-tools.md) and has no broker socket, no vault and nothing from the broker's environment.
- **The relay, with a surrogate key.** The CLI's only network is a relay on the cell's own loopback, which forwards to the one destination you name. The CLI holds a surrogate key that works nowhere else; the relay swaps it for your real key, which never enters the cell.
- **The request policy.** The relay answers anything that isn't the API's own method and path, or doesn't carry the surrogate, itself, and never forwards it.
- **Hatchling's patch.** Which files changed, and the patch, come from comparing the work tree with its pristine copy, never from what the CLI says. The patch goes to the artifact workspace for review.
- **Pinned.** The binary, and each companion file, must hash to its pin when the broker starts and again before every run. A changed binary is refused, never substituted.
- **The activity feed and the timeout**, as for any harness run.

Only you, the operator, define a harness. A harness gets a network and a key that spends money on your account, so the agent can't add one, change one, or pick one you haven't configured.

## Setting it up

1. Install the CLI yourself, and note its SHA-256 (`sha256sum`). Hatchling bundles none.
2. Make a directory for manifests, yours (or root's) and writable by no one else, and write the manifest there as `<name>.json`.
3. Point the broker at the directory: `HATCHLING_HARNESS_MANIFESTS=/etc/hatchling/harnesses`.
4. Store the vendor key in the vault as the secret `harness-<name>`. No vault grant may name it: the broker refuses to start if one does, since a grant would hand the key to the agent's cell.
5. Grant the harness to the agent: `HATCHLING_SKILL_GRANTS=<name>:code`. The agent calls it as the skill `<name>@1`.

As for the built-in harnesses, `HATCHLING_HARNESS_ROOTS` names the project roots a task may draw from, and `HATCHLING_ARTIFACT_WORKSPACE` must be set for the patches, outside every root.

The broker reads the manifests when it starts. A change takes effect at its next start.

## The format

```json
{
  "version": 1,
  "name": "opencode",
  "binary": { "path": "/opt/opencode/1.19.5/opencode", "sha256": "7df4e4136301f16589b0f30bf91a61380fca5f8ef22dd36570f5ebe1d4ec802c" },
  "companions": [],
  "api": "anthropic",
  "destination": "api.anthropic.com:443",
  "args": ["run", "--format", "json", "--auto", "--model", "anthropic/claude-sonnet-4-5", "{task}"],
  "env": [
    { "name": "ANTHROPIC_API_KEY", "value": "{key}" },
    { "name": "OPENCODE_CONFIG_CONTENT", "value": "{\"provider\":{\"anthropic\":{\"options\":{\"baseURL\":\"{relay}/v1\"}}},\"autoupdate\":false,\"share\":\"disabled\"}" },
    { "name": "OPENCODE_DISABLE_AUTOUPDATE", "value": "1" },
    { "name": "OPENCODE_DISABLE_MODELS_FETCH", "value": "1" },
    { "name": "OPENCODE_DISABLE_LSP_DOWNLOAD", "value": "1" },
    { "name": "OPENCODE_DISABLE_DEFAULT_PLUGINS", "value": "1" },
    { "name": "OPENCODE_DISABLE_CLAUDE_CODE", "value": "1" }
  ]
}
```

| Field | What it is |
|---|---|
| `version` | `1`, the one format there is. |
| `name` | The harness's name, as grants name it: up to 32 lower-case letters, digits and inner dashes, starting with a letter. It must be the file's name (`opencode` in `opencode.json`), and not a built-in harness's (`claude-code`, `codex-cli`). The agent calls it as `<name>@1`, and its key is the vault secret `harness-<name>`. |
| `binary` | The CLI's absolute `path` and its `sha256`. It must hash to it at the start and before every run. |
| `companions` | Optional: files the CLI needs beside it, each an absolute `path`, its `sha256`, and the `name` the cell sees it at, beside the CLI at the cell's root. Pinned and checked as the binary is. A name is one file name of letters, digits and `._-`, and not one the cell's root already holds. At most 16. |
| `api` | The API the CLI speaks: `anthropic` (the Messages API, its key in `x-api-key`) or `openai` (the Responses API, its key as an `Authorization` bearer). These are the relay's two wires. |
| `destination` | The vendor's one destination: `host:port` over HTTPS, `internal:host:port` for one that resolves inside your network, or `local:localhost:PORT` for a local endpoint. Resolved and pinned at the start. |
| `args` | The CLI's command line after its program. `{task}` goes in exactly one argument: the prompt Hatchling writes, with the task, the files and how to ask a question. `{relay}` may appear too. `{key}` may not: the key goes in the environment. At most 64. |
| `env` | The CLI's own variables, each a `name` and a `value`. One value must hold `{key}`, the run's surrogate key, and an argument or a value must hold `{relay}`, the relay's base URL (`http://127.0.0.1:8787`). `{task}` may not appear here. The cell sets `PATH`, `HOME` and `TMPDIR`, and `LANG` unless you set it, so you can't set those or any `HATCHLING_` variable. At most 64. |

Placeholders are replaced once: text the task holds is never read as a placeholder.

**Refused at the start, naming the field.** A manifest that's wrong in any way stops the broker from starting, and the error names the file and the field. That includes:
- a binary that doesn't hash to its pin;
- an API the relay doesn't speak;
- a command line with no `{task}`;
- a manifest or directory writable by anyone but the broker's user or root;
- a name that's already a harness's;
- an unknown field.

Every file in the directory must be a `<name>.json` manifest; there may be at most 32. A harness never runs half-configured.

## OpenCode

[OpenCode](https://github.com/anomalyco/opencode) is the first harness defined only by a manifest. The manifest above was tested end to end with OpenCode 1.19.5 (`opencode-linux-x64`, binary SHA-256 `7df4e413…c802c`), in a real cell, against a scripted vendor. It edited a file, ran a shell command through its tools, and answered. Its patch came back, and every request reached the vendor with the real key and none with the surrogate.

What each part does:

- **`run --format json`** is OpenCode's non-interactive mode. Hatchling takes its answer from the last `text` event it prints.
- **`--auto`** lets it edit and run commands without asking. The cell is the sandbox.
- **`--model`** names the model, as `provider/model`.
- **`OPENCODE_CONFIG_CONTENT`** points its Anthropic provider at the relay. Its `baseURL` ends in `/v1`.
- **`ANTHROPIC_API_KEY`** carries the surrogate.
- **The `OPENCODE_DISABLE_` variables** stop it reaching for anything but the relay: updates, its model catalogue, language servers and default plugins. It would fail to reach them anyway, since the relay forwards only the API.

At its start OpenCode also looks up its package registry. In a cell there's no resolver, so the lookup fails at once, as it would with no network, and the run goes on.

OpenCode also asks a small model for a title for each session, so a run makes one request more than its steps. Those go through the relay too, to the same API, and are in the run's request count.

To use OpenAI's API instead, set `"api": "openai"`, point the `openai` provider's `baseURL` at `{relay}/v1`, and carry the key in `OPENAI_API_KEY`. The relay passes only the Responses API (`POST /v1/responses`, `GET /v1/models`), so the provider must use it.
