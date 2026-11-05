# Using a hosted model

Hatchling uses an OpenAI-compatible chat-completions API through the broker.
The model must support function calling and `tool_choice: "required"`: the
agent checks that capability before asking its task. The agent currently sends
`model`, `messages` and `tools`, plus `reasoning` when thinking is turned off
([model thinking mode](model-thinking.md)); it does not send `response_format`.

Follow the README's build/install and root configuration steps, or
[install it as a service](install.md). The endpoint is the API base URL, before
`/chat/completions`; allowlist entries and grants use exactly `host:port`.
Hosted destinations use HTTPS. `local:localhost:PORT` is credentialed local HTTP;
`local-keyless:` is the explicit choice for a local server needing no key.

## The quick way, on a service install

**Installing with a hosted model, and none on this machine** (no GPU needed): `sh get-hatchling.sh --hosted openrouter`, or from a clone, `sudo scripts/install.sh --from release --hosted openrouter` ([install](install.md#quick-install)). It asks for the key at the same prompt as below, in your own terminal. What follows switches an install you already have.

Installed as a service ([install](install.md)), you don't have to pick between a local model and a hosted one: set both up, and switch whenever the work calls for it. Your conversation, memory and schedules stay with Hatchling either way. One command switches to a hosted model: the model, its server, its window, thinking, and the egress that reaches it, all together. It asks for the server's key at its prompt when the vault doesn't hold one yet (a key typed, piped or pasted, never an argument), backs up the settings, and restarts the broker:

```bash
sudo hatchling-broker use-hosted openrouter
```

That's OpenRouter's free `nvidia/nemotron-3-ultra-550b-a55b:free`, with its 1,000,000-token window and thinking off. Another of its models is `--model <id>` (add `--window <tokens>` to see the context meter against it); another provider is `--endpoint https://<host>/<path> --model <id>`. A key not given stops it before anything changes.

**From Windows, with Hatchling in WSL2**, give it the key from the clipboard: open PowerShell yourself, paste this command and don't press Enter yet, copy the key from your provider's page, then press Enter.

```powershell
(Get-Clipboard -Raw).Trim() | wsl -d <distro> -- sudo hatchling-broker use-hosted openrouter; Set-Clipboard -Value ' '
```

Back to the model on your own machine:

```bash
sudo hatchling-broker use-local --model qwen3.5:9b
```

It checks the local server has the model first. The old server stays allowed and set up either way, so after the first switch you needn't run a command again: switch in the app, under **Settings → General → Model**, which asks before your words go to a hosted server. A model you chose in the app's Settings was one on the old server, so it's forgotten.

## OpenRouter example, by hand

A hosted model gives the agent far more room than a local one: this one's window is 1,000,000 tokens, against the 32,768 a local install gets. Use an OpenRouter key with a $0 credit limit for the free-model check. You
enter it at a prompt that doesn't show it, or pipe it in from a password
manager. Use the exact model, rather than an automatic router:

```bash
export HATCHLING_SCHEDULER_MODEL_ENDPOINT=https://openrouter.ai/api/v1
export HATCHLING_SCHEDULER_MODEL_NAME=nvidia/nemotron-3-ultra-550b-a55b:free
export HATCHLING_SCHEDULER_MODEL_REASONING=disabled
# The model's context window, as its provider lists it: the chat's context
# meter measures each answer against it.
export HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW=1000000
export HATCHLING_EGRESS_ALLOWLIST=openrouter.ai:443
export HATCHLING_EGRESS_GRANTS=openrouter.ai:443
export HATCHLING_API_GRANTS=egress:openrouter.ai:443,channel-send:local
```

This model needs thinking turned off: with it on, the free endpoint sometimes
answers the capability check in text, and the agent refuses to start the task.
Other models may work with the provider's default. A free endpoint can still
fail now and then; if `first-run.py` reports no answer within five minutes,
check the broker's log and run it again.

The broker reads a hosted API's key from its vault, under a name it makes from
the destination. `hatchling-broker secret add --for` makes the same name
(`egress-openrouter-ai-443` here). It reads the key at a prompt that shows
a `*` for each character, never the character itself, or from a pipe, and
seals it at once. A paste shows its stars as it lands, so you can see it went
in before you press Enter; Backspace and Ctrl-U take characters back, and
Ctrl-C cancels. It says how many characters it read (`Read 73 characters.`):
an OpenRouter key is over 70, so a count far off is a paste that went wrong,
seen without the key being shown. Line breaks at the end of a paste are dropped;
one in the middle is refused. Wait for the prompt before you paste: what's
typed before it appears is shown, as at any prompt (`sudo`'s too), since the
terminal echoes it before Hatchling can hold it back. The key is never in a
file, an argument, the environment or your shell's history:

```bash
hatchling-broker migrate-vault
hatchling-broker secret add --for openrouter.ai:443
hatchling-broker &
sudo -u "#$SUDO_UID" python3 docs/examples/first-run.py "$HATCHLING_API_SOCKET"
```

The first output is a schedule acknowledgment. Success is the later `agent:`
answer. The cell asks the broker for a destination-bound surrogate; the broker
checks the grant, replaces it with the vault credential, and supplies the
Authorization header over verified TLS. The agent receives no provider key.
No vault-fetch grant is needed for model access.

**From Windows, with Hatchling in WSL2**, let the command take the key from the clipboard, so you type nothing and nothing shows. Open PowerShell yourself (not a terminal an AI agent opens for you: see [testing with an agent](testing-with-an-agent.md)), paste this command and **don't press Enter yet**. Copy the key from your provider's page, then press Enter:

```powershell
(Get-Clipboard -Raw).Trim() | wsl -d <distro> -- sudo hatchling-broker secret add --for openrouter.ai:443; Set-Clipboard -Value ' '
```

It reads the clipboard only when it runs, trims the stray spaces and line breaks a copy can carry, hands the key to Hatchling through a pipe, and then clears the clipboard, whether or not the key went in. It should say how many characters it read, then `Sealed egress-openrouter-ai-443 in the vault.` If you use Windows' clipboard history (Win+V), delete the key's entry there too: it keeps its own copy, which a command can't clear on its own.

A broker that's running uses the key from its next request: there's no need
to stop it. To change the key, add `--replace`; without it, `secret add`
refuses a name that's already in the vault, before it asks for anything.
Keep the vault's master key as well as its sealed files (and its `.store-key`)
when backing it up.

**Installed as a service,** set the model in `/etc/hatchling/hatchling.env`
(the endpoint, the name, the allowlist entry and the grants above), then add
the key with `sudo`: the command takes the service's settings.

```bash
sudo hatchling-broker secret add --for openrouter.ai:443
```

```bash
sudo systemctl restart hatchling
```

For another compatible provider, replace the base URL, model and destination
together, and give `secret add --for` the new destination. OpenRouter is an example, not a runtime
dependency. See its [model page](https://openrouter.ai/nvidia/nemotron-3-ultra-550b-a55b:free)
and [API documentation](https://openrouter.ai/docs/api/reference/overview).

## Reproducing the local proof

CI runs two checks without a provider account or operational credential:
`local` uses a pinned llama.cpp server and Qwen model with `--api-key-file`
through `local:localhost:18090`; `https` uses a named HTTPS stand-in with a
disposable CA and matching certificate. The CA is supplied only to the broker,
without changing the machine's trusted roots. Both refuse missing and wrong
keys, seal a generated credential with `secret add`, run the documented first-run client, and
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
