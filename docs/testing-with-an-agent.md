# Testing Hatchling with an AI agent

An AI coding agent can install Hatchling as a new user would, open the app, click through it, ask the agent things and take screenshots. It finds what a person would trip over, and it does so on every change. This page sets that up safely, with the Claude desktop app's Code tab or Codex.

The agent works on a throwaway Linux box of its own, never on the Hatchling you use. It never sees the app's key, your model's API key or a security key's PIN.

## The rules

- **A box of its own.** Install into a separate WSL2 distro (or VM) made for testing. Your own install, its vault and its conversations stay out of reach. Delete the box when you're done.
- **The app's key stays out of the transcript.** The app's address carries a key (`#hatchling-ui=...`), and anyone holding it can talk to the agent. The agent opens the app through a one-minute page that forwards the browser to it ([below](#open-the-app-without-showing-its-key)), and never prints the address. Whatever the agent prints can end up in logs and in the model provider's records.
- **You type the secrets, in a terminal you opened.** A hosted model's key goes in with `sudo hatchling-broker secret add` at its own prompt, and a security key's PIN on the key's prompt, typed by you. Ask the agent for the command, and run it in a terminal you opened yourself, such as Windows Terminal: don't let it open one for you inside the Claude or Codex app. That app draws the terminal, so it handles everything typed there, the key included; in your own terminal, the key never passes through the agent's app. Never paste a key into the chat.
- **You decide what leaves the box.** Pushing, opening pull requests and releases are yours to approve. Say so in the brief.
- **Nothing destructive here.** Tests that delete or kill run in the sandbox described in [Testing destructive code](testing-destructive-code.md), not on the demo box.

## Make the test box (Windows, WSL2)

In PowerShell:

```powershell
wsl --install Ubuntu-26.04 --name hatchling-demo --no-launch
```

Then, as root in it (`wsl -d hatchling-demo -u root`), add a user to install as, turn systemd on, and install git and curl:

```bash
useradd -m -s /bin/bash tester
echo 'tester ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/tester && chmod 440 /etc/sudoers.d/tester
printf '[boot]\nsystemd=true\n[user]\ndefault=tester\n' > /etc/wsl.conf
apt-get update -qq && apt-get install -y -qq git curl ca-certificates zstd
```

Passwordless `sudo` is for this box only, so the agent can install without a person at the prompt. Restart the box to turn systemd on: `wsl --terminate hatchling-demo`.

Install a model and Hatchling by [Install Hatchling as a service](install.md), as `tester`. With an NVIDIA GPU and its Windows driver, Ollama in WSL2 uses the GPU by itself; `ollama ps` shows how much of the model is on it.

WSL stops a box nobody is using after a while, which stops Hatchling with it. While testing, keep it up from a second terminal:

```powershell
wsl -d hatchling-demo -- sleep infinity
```

## Open the app without showing its key

WSL2 forwards `127.0.0.1` to Windows, so a browser on Windows reaches the app at `http://127.0.0.1:7340`. The address the installer prints carries the key. Instead of handing that to the agent, run this on the box: for one minute, it serves a page on `127.0.0.1:7399` that forwards the browser to the app, key and all.

```bash
#!/bin/bash
# open-app.sh: a one-minute page on loopback that forwards to the app.
set -eu
dir=$(mktemp -d)
chmod 700 "$dir"
printf '<!doctype html><meta charset=utf-8><title>Opening Hatchling</title><script>location.replace("http://127.0.0.1:7340/#hatchling-ui=%s")</script>' \
  "$(sudo cat /var/lib/hatchling/ui.sock.token)" > "$dir/index.html"
timeout 60 python3 -m http.server 7399 --bind 127.0.0.1 --directory "$dir" > /dev/null 2>&1 || true
rm -rf "$dir"
```

The agent starts it, opens `http://127.0.0.1:7399/` in its browser within the minute, and the app opens signed in. The key reaches the browser, not the transcript. For that minute, any program on this machine could fetch the page too, so do this on your own machine only.

The broker makes a new key each time it starts. After `sudo systemctl restart hatchling`, an open app goes offline: run the script again and reopen.

## With the Claude desktop app

The Code tab has a browser of its own beside the chat. It reaches the box's `127.0.0.1`, clicks, types and takes screenshots, and you watch it happen and can take over. Open a session in your clone of Hatchling and give it a brief like this:

> Test Hatchling as a new user would, on the WSL2 distro `hatchling-demo` only. Install it by docs/install.md. Never print the app's address or any key: open the app by running the open-app script from docs/testing-with-an-agent.md and going to http://127.0.0.1:7399/ in your browser. Click through every screen, ask the agent a few everyday things, and screenshot what looks wrong. Tell me how it feels to use, what confused you and what broke, with a screenshot of each. When a step needs a key or a PIN, give me the command to run in my own terminal; don't open a terminal for it, and never ask me for a key in the chat. Don't push, open pull requests or change anything outside that distro without asking me.

It can send you the screenshots, and it reads the page's text rather than guessing from pictures where it can.

## With Codex

Codex has no browser pane, so it drives one with [Playwright](https://playwright.dev). Give it the same brief, with one change: open `http://127.0.0.1:7399/` in Playwright's Chromium, and save each screenshot to a folder you name. A script it might write for the first look:

```python
from playwright.sync_api import sync_playwright

with sync_playwright() as p:
    page = p.chromium.launch().new_page()
    page.goto("http://127.0.0.1:7399/")
    page.wait_for_url("http://127.0.0.1:7340/**")
    page.screenshot(path="shots/01-first-open.png", full_page=True)
```

Once the app has loaded, its address bar no longer holds the key: the app reads it and takes it out, so screenshots are safe. The forwarding page's source and the browser's network log do hold it: neither agent should read them.

## Clean up

Stop the `sleep infinity` terminal, then delete the box and everything in it:

```powershell
wsl --unregister hatchling-demo
```
