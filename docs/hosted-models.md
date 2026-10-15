# Using a hosted model

Hatchling uses an OpenAI-compatible chat-completions API through the broker.
The model must support function calling and `tool_choice: "required"`: the
agent checks that capability before asking its task. The agent currently sends
`model`, `messages` and `tools`; it does not send `response_format`.

Follow the README's build/install and root configuration steps. Keep the broker
stopped while importing its credential. The endpoint is the API base URL, before
`/chat/completions`; allowlist entries and grants use exactly `host:port`.
Hosted destinations use HTTPS. `local:localhost:PORT` is credentialed local HTTP;
`local-keyless:` is the explicit choice for a local server needing no key.

## OpenRouter example

Use an OpenRouter key with a $0 credit limit for the free-model check. Store it in
an owner-only file outside your clone, or enter it at the hidden prompt below.
Use the exact model, rather than an automatic router:

```bash
export HATCHLING_SCHEDULER_MODEL_ENDPOINT=https://openrouter.ai/api/v1
export HATCHLING_SCHEDULER_MODEL_NAME=nvidia/nemotron-3-ultra-550b-a55b:free
export HATCHLING_EGRESS_ALLOWLIST=openrouter.ai:443
export HATCHLING_EGRESS_GRANTS=openrouter.ai:443
export HATCHLING_API_GRANTS=egress:openrouter.ai:443,channel-send:local
```

The credential name is `egress-` plus the destination, keeping ASCII letters and
digits and replacing every other character with `-`. Thus this destination needs the vault
file `egress-openrouter-ai-443`, with no filename extension.

In the configured root shell, import into a fresh vault entry without putting
the key in a command argument, environment variable or shell history:

```bash
python3 - "$HATCHLING_VAULT_DIR/egress-openrouter-ai-443" <<'PY'
import getpass
import os
import sys

# A terminal is required: never fall back to an echoed stdin prompt.
import warnings
warnings.simplefilter("error", getpass.GetPassWarning)
try:
    key = getpass.getpass("Hosted API key (hidden): ").encode()
except getpass.GetPassWarning:
    raise SystemExit("Open an interactive terminal; no key was read or stored")
if not key or b"\n" in key or b"\r" in key:
    raise SystemExit("Expected one nonempty API key")
fd = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "wb") as output:
    output.write(key)
PY
hatchling-broker migrate-vault
hatchling-broker &
python3 docs/examples/first-run.py "$HATCHLING_API_SOCKET" openrouter.ai:443
```

The import briefly creates an owner-only plaintext entry while the broker is
stopped; migration replaces it with a sealed `HVL1` envelope before startup.
Keep the vault's master key as well as its sealed files when backing it up.
For an existing credential, use a deliberate rotation/import procedure rather
than overwriting a sealed entry: this example refuses an existing filename.

The first output is a schedule acknowledgment. Success is the later `agent:`
answer. The cell asks the broker for a destination-bound surrogate; the broker
checks the grant, replaces it with the vault credential, and supplies the
Authorization header over verified TLS. The agent receives no provider key.
No vault-fetch grant is needed for model access.

For another compatible provider, replace the base URL, model, destination and
matching vault filename together. OpenRouter is an example, not a runtime
dependency. See its [model page](https://openrouter.ai/nvidia/nemotron-3-ultra-550b-a55b:free)
and [API documentation](https://openrouter.ai/docs/api/reference/overview).

## Reproducing the local proof

CI runs two checks without a provider account or operational credential:
`local` uses a pinned llama.cpp server and Qwen model with `--api-key-file`
through `local:localhost:18090`; `https` uses a named HTTPS stand-in with a
disposable CA and matching certificate. The CA is supplied only to the broker,
without changing the machine's trusted roots. Both refuse missing and wrong
keys, seal a generated credential, run the documented first-run client, and
require the scheduled agent's answer.

On a disposable Linux VM, after building the broker and agent:

```bash
python3 -m unittest discover -s scripts -p 'test_first_run.py' -v
python3 -m unittest discover -s scripts -p 'test_*hosted_model*.py' -v
proof_logs=$(mktemp -d)
proof_work=$(mktemp -d)
sudo unshare --mount --pid --fork --kill-child --mount-proc --net \
  python3 scripts/hosted_model_smoke.py "$PWD" "$proof_logs/https" "$proof_work" --mode https
sudo rm -rf -- "$proof_work"
```

For the real local model, use Python 3.12+ to run
`python3 scripts/fetch_hosted_model.py /outside-the-clone/hosted-fixtures`,
then pass `--mode local --fixtures /outside-the-clone/hosted-fixtures/paths.json`
with fresh log/work directories. The downloader checks the pinned sizes and
SHA-256 values before extracting or running the test-only fixtures. CI's
workflow contains the complete invocation; these weights are not shipped.

The model's response is held while the runner checks the identified agent's
initial exec environment, argv and every regular file in its readonly view.
The snapshot is tied to the scheduled run's broker session and successful
forwards. This proves absence in those surfaces at inspection time; it does
not claim to inspect arbitrary process memory or every instant. A missed or
incomplete scan fails. A generated credential found in a retained log also
fails; the artifact is redacted and explicitly labelled as failed.
