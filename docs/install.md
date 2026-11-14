# Install Hatchling as a service

This sets Hatchling up as a service that starts at boot, from a release, in a few commands:
- the broker, run as root;
- the app's host, run as you.

To try it by hand first, the [README's quickstart](../README.md#quickstart) runs the same pieces in a shell. To have an AI agent install it and try the app for you, see [Testing Hatchling with an AI agent](testing-with-an-agent.md).

## What you need

- **Linux with systemd**, kernel 5.11 or later (or WSL2 with systemd on), on x86_64 or aarch64, with glibc 2.34 or later. **Tested on Ubuntu** 24.04 and 26.04, and 26.04 under WSL2. Debian 12, Fedora and RHEL 9 and their successors meet these requirements, and should work, but haven't been tested.
- **A model it can reach:** an OpenAI-compatible server on this machine, or a hosted one. [Ollama](https://ollama.com) is the simplest local one: install it, then `ollama pull qwen3.5:9b`. With no GPU, or no wish to run a model here, install with a hosted model instead (`--hosted openrouter`, an OpenRouter key; a free one works). The model must be good at calling tools; see [Choosing a model](#choosing-a-model) and [Using a hosted model](hosted-models.md).
- **The GitHub CLI**, to download and verify the release (or download its files from the release page).

## Quick install

From 0.8.0, a release installs with one script and `curl`, with no clone and no GitHub CLI. Download it, read it (it's short), and run it as yourself, not with `sudo`:

```bash
curl -fsSLO https://github.com/undergroundrap/hatchling/releases/latest/download/get-hatchling.sh
```

```bash
sh get-hatchling.sh
```

It checks this machine can run Hatchling, downloads the programs, the app and the installer of the release it came with, and checks every file against that release's `SHA256SUMS`: a file that differs, or isn't listed, stops it before anything is installed. Then it makes sure Ollama is here with the model (`qwen3.5:9b`, unless `--model` names another), and offers to download the model if it isn't. Without Ollama it says how to install it and stops: it doesn't install other people's software for you. Last, it runs the release's own installer, below, with `sudo`, which asks for your password once.

It doesn't pipe anything into a shell, and it's in the release's `SHA256SUMS` and attested like every other file: `gh attestation verify get-hatchling.sh --repo undergroundrap/hatchling` checks it was built by this project's release workflow. `sh get-hatchling.sh --help` lists its options: another model, a model server other than Ollama, `--yes` to download the model without asking.

**No GPU, or no Ollama?** Install with a hosted model instead, and nothing runs a model on this machine: `sh get-hatchling.sh --hosted openrouter`. The installer asks for your OpenRouter key at its own prompt, in the terminal you ran it in (paste it and press Enter; it shows a `*` for each character), seals it in the vault, and uses OpenRouter's free 1,000,000-token model unless `--model` names another of its models. A key not given stops it before anything starts; run it again to give one.

Either way, the other kind is one command after (`use-hosted` or `use-local`, [below](#switching-models)), and then a switch in the app.

## Windows

[Hatchling on Windows](windows.md) shows how to verify the unsigned Setup and prepare WSL for an offline install. The `sudo` commands on this page are for installing from a Linux or WSL terminal instead.

## Keep it running on Windows

WSL stops a distro about 30 seconds after the last terminal into it closes, and Hatchling stops with it: the app goes offline, and nothing scheduled runs. Turning on systemd or `loginctl enable-linger` doesn't change this.

To keep it running while you're logged in, run this once, in PowerShell, **not** as administrator. Put your distro's name in place of `Ubuntu` (`wsl -l -v` lists them); `get-hatchling.sh` prints this with the name filled in:

```powershell
$p="$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\Hatchling.lnk"; $s=(New-Object -ComObject WScript.Shell).CreateShortcut($p); $s.TargetPath="$env:WINDIR\System32\conhost.exe"; $s.Arguments="--headless wsl.exe -d Ubuntu --exec sleep infinity"; $s.Save(); Start-Process $p
```

It adds a shortcut to your own Startup folder, which holds one idle process in WSL, with no window, from each time you log in. It needs no administrator rights, and nothing shows on screen. To stop it, delete the shortcut (below) and run `wsl --terminate Ubuntu`:

```powershell
Remove-Item "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\Hatchling.lnk"
```

Nothing runs while you're logged out or the computer is off. If a security key protects your vault, a restart of Windows leaves it locked until you run `sudo hatchling-broker unlock`.

**Run Hatchling in one WSL distro at a time.** Distros share their control groups and their network, so a second one fails to start with `Failed to spawn executor: Device or resource busy`, and they'd want the same port. If `unlock` says the service has no cpu controller, its own fix (`sudo systemctl daemon-reload`, then `sudo systemctl restart hatchling`) clears it.

Any other WSL distro that runs systemd can cause that message, not only a second Hatchling. If one starts while Hatchling is locked (after a restart or an update, before you unlock), its systemd takes the cpu controller away from Hatchling's service, and `unlock` stops before it asks for your key. `sudo systemctl daemon-reload` in Hatchling's distro puts the controller back; then unlock. Once Hatchling is unlocked and working it uses that controller, and another distro starting doesn't take it.

## Install

From a clone of the version you install, download its release into `release/` and install from it. `scripts/install.sh` checks every file against the release's `SHA256SUMS` before installing anything.

```bash
git -c advice.detachedHead=false clone --branch v0.8.0 https://github.com/undergroundrap/hatchling && cd hatchling
```

```bash
gh release download v0.8.0 --repo undergroundrap/hatchling --dir release
```

```bash
sudo scripts/install.sh --from release --model-endpoint http://localhost:11434/v1 --model qwen3.5:9b
```

Or, with a hosted model and none on this machine, `sudo scripts/install.sh --from release --hosted openrouter`: it asks for the key at its own prompt.

Before it starts anything, it runs `hatchling-broker doctor`, which checks the settings, that this machine can make the agent's cells, and that the model's server answers with the model named. If one isn't so, it stops and says what to do. Run it again after: it keeps your configuration.

It ends with the app's address. Open it in a browser on this machine, and say something. **The address is a key to your agent: don't share it.** Before you install, you can also check that the release's files were built by this project's release workflow: [Verifying a release](releasing.md#verifying-a-release).

## Choosing a model

**Local, hosted, or both: you don't have to choose.** How much your agent can do depends most on its model. A model on your own machine keeps everything there; a hosted model is bigger, and remembers far more of a conversation. A local model with a 32,768-token window still does real work: reads your goals, keeps your Feed, answers from what it's told. For the most out of the agent, use a strong local setup (a GPU with 12 GB or more, and the window below), or a [hosted model](hosted-models.md) with a large window: OpenRouter's free `nvidia/nemotron-3-ultra-550b-a55b:free` has 1,000,000 tokens.

Many people set up both and switch as the work needs: the hosted model for a long research task, the local one for anything private, or when the provider is down or you're offline. Hatchling uses one model at a time. Set the second up once with one command (`use-hosted` or `use-local`, [below](#switching-models)), and from then on switch in the app, under **Settings → General → Model**, with no command at all. Set both up once and both stay ready: the key stays in the vault, and Ollama keeps its models and the window the installer gave it. Your conversation, memory, goals and schedules are Hatchling's, not the model's, so they come with you. Local models keep getting smaller and better, so a choice you make today isn't for good.

Hatchling's agent works by calling tools, so the model must call them well: only when a task needs one, with arguments that fit. A model that can't is the commonest reason an answer fails. Hatchling stops such a model before anything it asked for runs, and the chat says so.

For a local model on one GPU, tried with Hatchling's own tool set (2026-11, Ollama, an RTX 4070 with 12 GB):

| Model | Download | Fits | Tool calls |
| --- | --- | --- | --- |
| `qwen3.5:9b` | 6.6 GB | 8 GB GPU or more | Right every time; the default here. |
| `gemma4:12b` | 8 GB | 12 GB GPU or more | Right every time. |
| `llama3.2` (3B) | 2 GB | almost anything | **Not recommended.** It called tools a plain question didn't need, a web search for "ideas for a relaxing Sunday", in a third of the tries. |

A model bigger than the GPU's memory runs partly on the CPU, much more slowly: `ollama ps` shows the split. Ollama uses an NVIDIA or AMD GPU by itself when its driver is installed; under WSL2, install the Windows driver, not one inside Linux.

**Give the model room.** The agent's instructions and tools take about 5,500 tokens before you say anything, and both models think before they answer, which makes better answers and uses more: one chat answer took about 6,000. Ollama's default window on many GPUs is 4,096 tokens, so a thinking model is cut off partway, and the chat says the model ran out of room.

So the installer sizes a local Ollama service's window to your machine: **65,536 tokens** when the model fits on the GPU with it, else **32,768**. It loads the model at 65,536 and asks Ollama how it fits (`hatchling-broker model-fit`), and steps down if any of it would run on the CPU. A window of 65,536 or more is kept, and so is one of 32,768 or more that you set yourself. It does it in a file of its own, `/etc/systemd/system/ollama.service.d/zz-hatchling-context.conf`, and says so; removing that file and restarting Ollama undoes it. `--ollama-context <tokens>` asks for a size instead, and `--ollama-context 0` leaves Ollama as it is. What the window costs, measured on an RTX 4070 with 12 GB (2026-11):

| Window | `qwen3.5:9b` takes | On the GPU |
| --- | --- | --- |
| 16,384 | 6.1 GB | all of it |
| 32,768 | 6.6 GB | all of it |
| 65,536 | 7.7 GB | all of it |
| 131,072 | 10.7 GB | 9.8 GB; the rest runs on the CPU, more slowly |

Its first answer took 11 to 14 seconds at each size that fit. The installer loads the model once, so doctor can see how it fits on your machine, and says so: "all on the GPU (6.6 GB)", or how much runs on the CPU and what helps. It says how much of the computer's memory is free, too. Later, when something else takes the GPU's memory or the computer runs low, the chat says so under the context meter, after the answer it slowed. A model bigger than the GPU's memory still runs, with the part that doesn't fit on the CPU; `ollama ps` shows the split, and the window in its `CONTEXT` column, once the model has loaded. To set the window by hand, or with Ollama not run by the installer:

```bash
sudo mkdir -p /etc/systemd/system/ollama.service.d
printf '[Service]\nEnvironment=OLLAMA_CONTEXT_LENGTH=32768\n' | sudo tee /etc/systemd/system/ollama.service.d/zz-hatchling-context.conf
sudo systemctl daemon-reload && sudo systemctl restart ollama
```

Hatchling reads the window from Ollama itself, as `ollama ps` does, and the chat's context meter shows how full each answer got it: "Context: 5.6K of 32K tokens (17%)". Once the model has loaded, `sudo hatchling-broker doctor` shows the window too: it says when it's too small to answer in, and when it's under 32,768. With a model server that doesn't say its window, tell Hatchling: add `HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW=32768` to `/etc/hatchling/hatchling.env`, and restart: `sudo systemctl restart hatchling` (without it, the meter shows tokens alone). It's at least 8,192, and each run is told it too.

### Switching models

To use a hosted model instead, with a far larger window, run `sudo hatchling-broker use-hosted openrouter` ([hosted models](hosted-models.md)), and `sudo hatchling-broker use-local --model qwen3.5:9b` to come back. The server you switch from stays set up, so once both are, switch between them in the app: **Settings → General → Model** says where the model in use answers ("on this machine: your words stay here", or the provider's host), and **Switch to …** offers the other. It asks first, naming where your words will go, and Hatchling uses it from your next message. The app switches only between the two servers you set up with `sudo`, never to one of its own choosing. To switch local models, pull the new one (`ollama pull <name>`), then choose it in the app: **Settings → General → Model** lists the models on your server (**Refresh** after a pull), and Hatchling uses the one you choose from your next message, with no restart. It shows how the model in use fits, too. **Back to …** returns to the model you installed with, which `HATCHLING_SCHEDULER_MODEL_NAME` in `/etc/hatchling/hatchling.env` still sets. `ollama rm <name>` removes one you no longer use.

**From your own build** instead of a release:
1. Build the programs: `cargo build --release`.
2. Build the app: `cd frontend && npm ci && npm run build`.
3. Put `hatchling-broker`, `hatchling-agent`, `hatchling-host` and `hatchling-browser` (from `target/release`) and the app's `dist`, renamed `ui`, in one directory.
4. Pass that directory as `--from`. With no `SHA256SUMS` beside them, the files aren't checked.

## What it sets up

| What | Where |
| --- | --- |
| The programs | `/usr/local/bin/hatchling-broker`, `-agent`, `-host`, `-browser` |
| The app | `/usr/local/share/hatchling/ui` |
| The configuration, the whole of it | `/etc/hatchling/hatchling.env`: every setting is in `hatchling-broker --help` |
| The broker's service, run as root, dropping to `hatchling-broker` once started | `hatchling.service` |
| The app's host, a system service run as you, confined by systemd (no privileges, the system read-only) | `hatchling-host@<you>.service` |
| Four system users: the cells', the broker's, the vault keeper's, the egress authority's | `/etc/sysusers.d/hatchling.conf` |
| Its state: the vault, schedules, sockets | `/var/lib/hatchling` ([what's kept](storage.md)) |

With systemd 254 or later, the broker gets a cgroup of its own to give each cell, so each cell's processes, memory and CPU are bounded as a whole.

## Using it

- **Your first visit.** The chat offers a checklist until you close it: choose your model, protect your vault, make a backup. You can chat straight away. Each step gives the command to copy into a terminal you opened yourself, and ticks itself when you come back. The app reads what's set up from the broker, as counts and a date: a PIN, a key or a backup code never passes through it. Find it again in Settings, under General.
- **The app's address again**, as yourself: `hatchling-host url`. Each start of the broker makes a new one, so after a restart an open app goes offline; open the new address.

- **A site's sign-in**, as yourself (not `sudo`): `hatchling-broker sign-in add https://site name` asks for the username and password at your terminal, so they never go through the app. The site has to be one you've kept signed in. `sign-in list` and `sign-in remove <id>` do what they say, and nothing shows a stored password.
- **Scheduled work from a shell**, as yourself (not `sudo`): `hatchling-broker schedule add "Check the disk is under 90% full and tell me if not" --every 15m`, then `hatchling-broker schedule list` and `hatchling-broker schedule remove <id>`. The timing is one of `--at "2026-11-04 08:00"`, `--every 5m`, `--daily 07:30`, `--weekdays 08:00` or `--cron "0 8 * * 1-5"`, in this machine's time zone unless `--tz` names another. It's the same as the app's schedule form: the work runs as a chat message's would, and its results arrive in the chat.
- **Something not working?** `sudo hatchling-broker doctor` checks what a start trips on, and says how to fix each problem it finds.
- **In a container, or a VM that keeps cgroups to itself?** Hatchling gives each of the agent's cells a cgroup of its own, which bounds the cell's memory and CPU as a whole. That needs the machine's cgroups to offer the `pids`, `memory` and `cpu` controllers (`cat /sys/fs/cgroup/cgroup.controllers`). Where they don't, the installer holds cells by process limits alone and says so as it runs. It works, and it's weaker: `doctor` notes it, and the app's **Your security** page says it of that install. The README's [Cell limits](../README.md#cell-limits) has the detail.
- **Protect the vault with a security key**, such as a YubiKey, so nothing opens it without the key: [Protecting the vault with a security key](security-keys.md). After each restart, you unlock it: `sudo hatchling-broker unlock`.
- **Or with an authenticator app**, if you have no security key: Hatchling then waits for a code after each restart, though a stolen disk still opens the vault. [Protecting the vault with an authenticator app](authenticator.md).
- **Change a setting:** edit `/etc/hatchling/hatchling.env`, then `sudo systemctl restart hatchling`.
- **The broker's other commands** (`hatchling-broker --help` lists them) take the service's settings when you run them with `sudo` from a shell that has none, so `sudo hatchling-broker tools list` just works. They read the file only while it's root's and nobody else can write it, as the installer leaves it: it says what the broker runs. `sudo hatchling-broker vault-keyring setup` always uses the service's settings, whatever your shell holds: it's the service it sets up.
- **The logs:** `journalctl -u hatchling` for the broker, `journalctl -u "hatchling-host@$USER"` for the app's host.
- **Stop it:** `sudo systemctl stop hatchling`. **Start it again:** `sudo systemctl start hatchling`.
- **Update:** clone the new version, download its release, and run `scripts/install.sh --from <dir> --keep-model`. It keeps your configuration and your model. On Windows, run the new setup: it finds your install and offers a backup first. When the new version keeps something by default that your configuration doesn't name at all, such as your conversation, it adds that setting, backs the old file up beside it (`hatchling.env.before-<date>`), and says so. A setting you set empty stays off. Read the version's [changelog](../CHANGELOG.md) first: before 1.0, a minor version may change the configuration. [Updating Hatchling](updating.md) has the whole of it: a safety copy first, a build of your own, what to check after, and disk space.

## Uninstall

1. Stop and remove the services:

   ```bash
   sudo systemctl disable --now hatchling
   ```

   ```bash
   sudo systemctl disable --now "hatchling-host@$USER"
   ```

2. Remove what was installed:

   ```bash
   sudo rm -rf /usr/local/bin/hatchling-broker /usr/local/bin/hatchling-agent /usr/local/bin/hatchling-host /usr/local/bin/hatchling-browser /usr/local/share/hatchling /etc/systemd/system/hatchling.service /etc/systemd/system/hatchling.service.d /etc/systemd/system/hatchling-host@.service
   ```

3. **Your data is still there**, in `/var/lib/hatchling` and `/etc/hatchling`. [What Hatchling keeps on disk](storage.md#uninstall) says what each part is, how to back it up, and how to remove it with the users.
