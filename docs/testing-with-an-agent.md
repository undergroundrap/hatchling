# Testing Hatchling with an AI agent

An AI coding agent can install Hatchling as a new user would, open the app, click through it, ask the agent things and take screenshots. It finds what a person would trip over, and it does so on every change. This page sets that up safely with an AI coding agent and its available browser tools.

The agent works on a throwaway Linux box of its own, never on the Hatchling you use. It never sees the app's key, your model's API key or a security key's PIN.

## The rules

- **A box of its own.** Install into a separate WSL2 distro (or VM) made for testing. Your own install, its vault and its conversations stay out of reach. Delete the box when you're done.
- **The app's key stays out of the transcript.** The app's address carries a key (`#hatchling-ui=...`), and anyone holding it can talk to the agent. The agent opens the app through a one-minute page that forwards the browser to it ([below](#open-the-app-without-showing-its-key)), and never prints the address. Whatever the agent prints can end up in logs and in the model provider's records.
- **You type the secrets, in a terminal you opened.** A hosted model's key goes in with `sudo hatchling-broker secret add` at its own prompt, and a security key's PIN on the key's prompt, typed by you. Ask the agent for the command, and run it in a terminal you opened yourself, such as Windows Terminal: don't let it open one for you inside the Claude or Codex app. That app draws the terminal, so it handles everything typed there, the key included; in your own terminal, the key never passes through the agent's app. Never paste a key into the chat. From Windows, [hosted models](hosted-models.md) has a command that takes the key from the clipboard and clears it after, so nothing is typed or shown.
- **You decide what leaves the box.** Pushing, opening pull requests and releases are yours to approve. Say so in the brief.
- **Nothing destructive here.** Tests that delete or kill run in the sandbox described in [Testing destructive code](testing-destructive-code.md), not on the demo box.

## Make the test box (Windows, WSL2)

First list your distros with `wsl --list --verbose`. Choose a new name for the disposable test box; the examples use `hatchling-agent-test`. If that name already exists, establish who owns it and what it holds before using it, or choose another. Record the chosen name in the task brief. Never reuse your personal install for a reset, restore or uninstall drill.

WSL distros share their kernel, control groups and ports: [run one Hatchling install at a time](install.md). If your personal install is running, arrange a test window with its owner. Don't silently stop or restart it: a protected vault needs its owner to unlock it again.

In PowerShell:

```powershell
wsl --install Ubuntu-26.04 --name hatchling-agent-test --no-launch
```

Then, as root in it (`wsl -d hatchling-agent-test -u root`), add a user to install as, turn systemd on, and install git and curl:

```bash
useradd -m -s /bin/bash tester
echo 'tester ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/tester && chmod 440 /etc/sudoers.d/tester
printf '[boot]\nsystemd=true\n[user]\ndefault=tester\n' > /etc/wsl.conf
apt-get update -qq && apt-get install -y -qq git curl ca-certificates zstd python3
```

Passwordless `sudo` is for this box only, so the agent can install without a person at the prompt. Restart the box to turn systemd on: `wsl --terminate hatchling-agent-test`.

Install a model and Hatchling by [Install Hatchling as a service](install.md), as `tester`. With an NVIDIA GPU and its Windows driver, Ollama in WSL2 uses the GPU by itself; `ollama ps` shows how much of the model is on it.

WSL stops a box nobody is using after a while, which stops Hatchling with it. While testing, keep it up from a second terminal:

```powershell
wsl -d hatchling-agent-test -- sleep infinity
```

## Open the app without showing its key

WSL2 forwards `127.0.0.1` to Windows, so a browser on Windows reaches the app at `http://127.0.0.1:7340`. The address the installer prints carries the key. Instead of handing that to the agent, run this on the box: for one minute, it serves a page on `127.0.0.1:7399` that forwards the browser to the app, key and all.

```bash
#!/bin/bash
# open-app.sh: a one-minute page on loopback that forwards to the app.
set -eu
dir=$(mktemp -d)
trap 'rm -rf -- "$dir"' EXIT
chmod 700 "$dir"
printf '<!doctype html><meta charset=utf-8><title>Opening Hatchling</title><script>location.replace("http://127.0.0.1:7340/#hatchling-ui=%s")</script>' \
  "$(sudo cat /var/lib/hatchling/ui.sock.token)" > "$dir/index.html"
timeout 60 python3 -m http.server 7399 --bind 127.0.0.1 --directory "$dir" > /dev/null 2>&1 || true
```

The agent starts it, opens `http://127.0.0.1:7399/` in its browser within the minute, and the app opens signed in. The key reaches the browser, not the transcript. For that minute, any program on this machine could fetch the page too, so do this on your own machine only.

The broker makes a new key each time it starts. After `sudo systemctl restart hatchling`, an open app goes offline: run the script again and reopen.

## Give the agent a bounded brief

Use the browser or computer-control tools available in the agent's current session, following their instructions. If none is connected, a local Playwright browser is another option. Keep the same secret-entry and evidence rules whichever tool is used. A useful brief:

> Test Hatchling as a new user would, on the disposable WSL2 distro `hatchling-agent-test` only. Install it by docs/install.md and record the release or source revision actually installed. Never print the app's address or any key: use the open-app script from docs/testing-with-an-agent.md and open http://127.0.0.1:7399/. Check that the app consumed its key before screenshots. Click through the connected app, ask a few everyday things, and verify what actually changed. Report confusing steps, failures and unsupported demo actions with screenshots where useful. When a step needs a key or a PIN, give me the exact command and expected interaction in my own terminal; never open a terminal for secret entry or ask for a secret in chat. Preserve the first failure and diagnose it. Ask before publication, merging, changing another distro or deleting this one.

Read page text where possible; take screenshots for visual judgment. A Playwright example, when using that tool directly:

```python
from pathlib import Path
from playwright.sync_api import sync_playwright

Path("shots").mkdir(exist_ok=True)
with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()
    page.goto("http://127.0.0.1:7399/")
    page.wait_for_url("http://127.0.0.1:7340/**")
    page.wait_for_function("!location.hash.includes('hatchling-ui=')")
    page.screenshot(path="shots/01-first-open.png", full_page=True)
    browser.close()
```

The app reads the key and removes it from its address. Verify that happened before capturing evidence; don't print the address to check. The forwarding page's source, redirect history and network logs can hold the key: leave them out of agent-readable traces and reports.

## What completes a run

- **Identify what ran.** Record the disposable box, installed version and artifact hash or source revision, relevant configuration, commands and outcomes. A local build is not proof of the published installer.
- **Prove connected behavior.** Confirm the live connection. Exercise the changed flow and its locked, offline or failure state, then check the resulting backend state. Reload or restart where persistence is part of the claim. A toast or accepted approval alone is not completion; reconcile uncertain delivery before repeating an action.
- **Keep the proof honest.** Preserve the first failure, its cause and the repair's result. List skipped and unexercised paths. Use [the scorecard's evidence classes](scorecard.md): an agent's real-install pass does not become a person's pass, and demo UI does not become a working feature. Keep screenshots free of secrets and personal data.
- **Run the applicable gates.** Follow [CONTRIBUTING.md](../CONTRIBUTING.md), [testing patterns](testing-patterns.md) and the current workflows. Frontend `npm run check` alone omits the browser suites. Build and run those suites and the HTTP smoke check on Linux, as CI does: their helpers assume POSIX APIs and paths. The preview observes `navigator.onLine`; a backend jail with only loopback starts it offline. Use a credential-free frontend test environment that matches CI, keeping destructive tests in their stricter sandbox. A Windows browser can still drive the app for an installation walkthrough. An unprivileged pass does not prove the root mount or room-disk tests. Hand mutants must fail their intended assertion, not a build or fixture setup.
- **Finish the whole job.** Record the final source identity and actual completion of every stage. Stopping a Windows `wsl.exe` client or seeing an intermediate “done” marker does not prove the Linux job stopped. Inspect owned jobs before changing their checkout or cleaning their files. Preserve logs before cleanup and stop only the processes or scopes identified as this run's.

## Clean up

Stop this run's test jobs, temporary servers and `sleep infinity` keep-alive; verify the jobs ended. Keep the logs, screenshots and source identity outside the disposable box. Before removing it, list the distros again and obtain the owner's approval for the exact test-box name. `--unregister` permanently deletes that distro and everything in it; never use the personal install's name or a global WSL shutdown as cleanup. For the disposable box created above:

```powershell
wsl --unregister hatchling-agent-test
```
