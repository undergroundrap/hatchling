<p align="center">
  <img src="assets/hatchling-logo.png" alt="Hatchling: a chick hatching from an egg" width="200">
</p>

<h1 align="center">Hatchling</h1>

<p align="center"><strong>Personal AI agents you actually own.</strong></p>

<p align="center">
  <a href="LICENSE"><img alt="License: AGPL-3.0-or-later" src="https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg"></a>
  <a href="https://github.com/undergroundrap/hatchling/actions/workflows/ci.yml"><img alt="Gate" src="https://img.shields.io/github/actions/workflow/status/undergroundrap/hatchling/ci.yml?branch=main&amp;label=gate"></a>
  <a href="https://crates.io/crates/hatchling-runtime"><img alt="crates.io: hatchling-runtime" src="https://img.shields.io/crates/v/hatchling-runtime.svg"></a>
  <a href="https://docs.rs/hatchling-runtime"><img alt="docs.rs" src="https://img.shields.io/docsrs/hatchling-runtime"></a>
  <a href="rust-toolchain.toml"><img alt="Rust 1.85+" src="https://img.shields.io/badge/rust-1.85%2B-orange.svg"></a>
  <a href="docs/arch/trust-boundary.md#requirements"><img alt="Platform: Linux 5.11+ or WSL2" src="https://img.shields.io/badge/platform-Linux%205.11%2B%20%7C%20WSL2-lightgrey.svg"></a>
  <a href="CONTRIBUTING.md"><img alt="PRs welcome" src="https://img.shields.io/badge/PRs-welcome-brightgreen.svg"></a>
</p>

An always-on agent that lives on your machine, thinks with the model you choose, works while you're away — and is built so it **cannot** see your keys, read your files, or reach the internet unless you granted exactly that.

Hatchling is open source (AGPL-3.0-or-later), written in Rust, and pre-1.0.

---

## Why Hatchling

Always-on personal agents are arriving everywhere. Almost all of them run on someone else's servers: your prompts, your files and your credentials live under their terms, and the safety model is a policy you can't inspect.

Hatchling is the other option: **the agent runs on hardware you control, and the safety model is the architecture, not a promise.**

### Isolation is built in, not bolted on

Most agent frameworks start with a powerful agent and add guardrails around it. Hatchling starts with the boundary and gives the agent only what you grant:

- **Every agent runs in a cell**: its own Linux user, mount, PID, network and IPC namespaces, a seccomp-BPF syscall allowlist, every capability dropped, and one way to talk: a session with the broker, admitted by kernel peer credentials. The agent is the process *inside* the cell; there is no unsandboxed mode. If the kernel can't build a cell, Hatchling refuses to start rather than run without one.
- **Your keys never enter the cell.** Credentials live in a local vault, sealed at rest (XChaCha20-Poly1305), unlockable by key file, authenticator codes or a FIDO2 security key, with backup codes. When the agent calls an API, the egress proxy swaps a broker-minted surrogate for the real credential at the boundary, so the agent can *use* a service without ever *holding* the key. (A `vault:fetch:<name>` grant can hand over one named secret, deliberately; nothing else can.)
- **No network by default.** A cell has an empty network namespace. It reaches the outside only through the broker's egress proxy, per grant, against an exact-match allowlist. Each session pins the addresses a destination resolves to; remote hosts are reached only over TLS, which checks the hostname before a byte is sent, and a local model port must resolve to loopback.
- **The broker checks who's calling before reading a byte.** Every connection is identified by the kernel's peer credentials, and every request is checked against that caller's grants.
- **Untrusted text stays untrusted.** Tool output reaches the model inside an untrusted-data envelope it can't close early. Everything sent to you passes an output gate that redacts held secrets and private keys, and state-changing actions go through confirmation rails.

The full model, what it deliberately doesn't cover, and the gaps our own pre-launch review found are in **[the trust boundary](docs/arch/trust-boundary.md)**. Each gap is a [work order](docs/workorders/README.md), open to contributors.

## What it can do today

| | |
| --- | --- |
| **Your model** | Any OpenAI-compatible chat-completions endpoint: Ollama, llama.cpp, LM Studio and vLLM locally, or a hosted API. Every secret a run fetches is redacted from what it sends, local endpoint or not. |
| **Coding harnesses** | Claude Code and Codex CLI, pinned to a SHA-256 and verified before they run, inside cells. |
| **Chat** | Each message you send on the local chat is answered by the agent in a fresh cell, holding your API client's grants and nothing more. It's given the conversation so far, as the broker recorded it: what you said, and what you were shown. |
| **Scheduling** | Cron or one-off tasks in explicit IANA timezones; each schedule runs with exactly the grants it was created with. |
| **Memory and goals** | Broker-owned durable memory and goal tracking, paged so no reply outgrows its frame. |
| **Feed** | A scheduled digest of your own records: what changed in your goals, tracking and tagged memory. |
| **Skills** | Signed, verified skill packages, each run in a dedicated cell scoped to its declared capabilities. |
| **Artifacts and research** | Documents and pages made by signed builder skills, inspected at the boundary before they reach you; deep-research runs that deliver a cited report. |
| **Spaces** | Shared, persistent scopes with members and devices. |
| **Browser** | A real headless Chromium the agent drives, in a cell of its own, that reaches only the sites you approve, through the broker. You watch it in the app, and can stop it, or take it over and hand it back. |
| **Local API** | A Unix-socket JSON API with a generated JSON Schema, for your own clients and integrations: **[docs/local-api.md](docs/local-api.md)**. |
| **App** | A TypeScript UI, served on loopback by `hatchling-host`: chat with the agent and follow its status. Opened on its own, it runs a scripted preview. |

**Platforms:** Linux (kernel 5.11+, unprivileged user namespaces). On Windows, use WSL2 — you get the Hyper-V boundary underneath the cells. macOS: the isolation layer is a platform-neutral trait, but there is no macOS backend yet.

## How it works

```text
  outside: your model endpoint · the APIs you grant · harness vendors
 ─────────────────────────▲────────────────────────────────▲──────────────────
                          │ real key swapped in            │ vendor key added,
                          │ TLS · pinned addresses         │ echoes scrubbed
 ┌────────────────────────┼─── broker: the trusted side ───┼─────────────────┐
 │                   ┌────┴─────────┐  scheduler     ┌─────┴─────────┐       │
 │   vault ─────────►│ egress proxy │  memory, goals │ harness relay │       │
 │   sealed at rest; │ exact-match  │  spaces        │ Claude Code,  │       │
 │   keys go only to │ allowlist    │  skills        │ Codex         │       │
 │   the proxy and   └────▲─────────┘  audit log     └─────▲─────────┘       │
 │   the relay            │                                │                 │
 └───────▲────────────────┼────────────────────────────────┼─────────────────┘
         │ local API      │ one session per cell, admitted │
         │ (Unix socket)  │ by kernel peer credentials;    │
         │                │ grants checked per request;    │
         │                │ surrogate tokens, never keys   │
         │          ┌─────┴────────────────────────────────┴─────┐
        you         │ cell: namespaces · seccomp · no caps       │
     every reply    │ empty network · read-only view             │
     passes the     │                                            │
     output gate    │ agent loop · skills · harness CLIs         │
                    └────────────────────────────────────────────┘
```

The **broker** is the trusted side: it owns the vault, the proxy, the schedules and the API, and it never executes untrusted code. Everything the model drives runs in a **cell**. Today the broker is one process running as root; [WO-017](docs/workorders/workorder-017-privilege-separate-the-trusted-side.md) splits it into a credential keeper, an egress authority and a privileged spawner, each under its own user, so a bug in a busy part of the broker can't reach your keys. Where each piece lives: **[module map](docs/arch/module-map.md)** · **[protocol](docs/arch/protocol.md)** · **[session protocol](docs/session-protocol.md)**.

## Quickstart

CI extracts and runs the shell blocks below as root with the stand-in model,
and fails unless the agent's answer arrives.

For the complete command and environment reference, run `hatchling --help`,
`hatchling broker --help` or `hatchling agent --help`. The standalone
`hatchling-broker`, `hatchling-agent` and development-only `hatchling-dev`
also support `--help` and `-h`. Help works with no configuration and
creates no sockets, directories or vault files. An invalid argument prints
usage to stderr and exits 64. The broker builds the cell in which the agent
runs; it supplies the agent's identity and configuration.

**Requirements:** Linux 5.11+ with unprivileged user namespaces and seccomp-BPF (or WSL2); Rust (the pinned 1.98.1 installs itself through `rust-toolchain.toml`; the minimum supported is 1.85); a C compiler; Python 3 for the examples; Node.js 22.12+ to build the app; root, for the reason [below](#running-without-root), or a [subordinate id range](#quickstart-without-root) for your user.

Build, install the broker, the agent and the app's host, and create four system users nothing else runs as: the one the cells run as, the vault keeper's, which alone owns the vault and its key, the egress authority's, which alone reads the credentials of the requests it makes, and the broker's own, which it runs as once it has started, and which owns what it keeps.

```bash
git clone https://github.com/undergroundrap/hatchling
cd hatchling
cargo build --release
sudo install -m 755 target/release/hatchling-broker target/release/hatchling-agent target/release/hatchling-host /usr/local/bin/
sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin hatchling-cell
sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin hatchling-keeper
sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin hatchling-egress
sudo useradd --system --user-group --no-create-home --shell /usr/sbin/nologin hatchling-broker
```

Or install from crates.io: `cargo install hatchling-runtime` builds one `hatchling` binary that is both. Copy it where the cell's user can run it (`sudo install -m 755 ~/.cargo/bin/hatchling /usr/local/bin/`), then below use `hatchling broker` for `hatchling-broker`, and set `HATCHLING_AGENT_BIN=/usr/local/bin/hatchling`. Keep the clone either way: the examples run from it, and the app's host is built there (`cargo build --release -p hatchling-host`, then install `target/release/hatchling-host` as above).

Give cells a uid of their own, never `nobody` or any uid another process uses: a process with the cells' uid can't open an agent session (each cell gets a connection of its own), but it shares the cells' file permissions and process limits. The broker refuses to start with `nobody` (65534) as the agent uid unless `HATCHLING_AGENT_ALLOW_NOBODY=true`.

The rest runs as root (`sudo -s`, still in the clone). The broker's API, app and Machine view then answer you alone: the user who ran `sudo` (`SUDO_UID`), its owner. So you run their clients as yourself, with `sudo -u "#$SUDO_UID"`, from the same shell. Configure the broker: where it keeps its state, which uid its cells get, and the one network destination a cell may reach, the model:

```bash
export HATCHLING_HOME=/var/lib/hatchling
mkdir -p -m 700 "$HATCHLING_HOME"/{sock,vault,schedules}
export HATCHLING_BROKER_SOCKET=$HATCHLING_HOME/sock/broker.sock
export HATCHLING_AGENT_UID=$(id -u hatchling-cell)   # cells run as hatchling-cell
export HATCHLING_CELL_UID_BASE=$HATCHLING_AGENT_UID
export HATCHLING_AGENT_BIN=/usr/local/bin/hatchling-agent
export HATCHLING_CELL_ID=cell-01
export HATCHLING_VAULT_DIR=$HATCHLING_HOME/vault
export HATCHLING_SCHEDULER_DIR=$HATCHLING_HOME/schedules
export HATCHLING_CHANNEL_INGRESS=$HATCHLING_HOME/ingress.sock
export HATCHLING_API_SOCKET=$HATCHLING_HOME/api.sock
export HATCHLING_UI_SOCKET=$HATCHLING_HOME/ui.sock   # for the app, below
export HATCHLING_OPERATOR_SOCKET=$HATCHLING_HOME/operator.sock   # for the Machine view, below
export HATCHLING_SCHEDULER_MODEL_ENDPOINT=http://localhost:18089/v1
export HATCHLING_SCHEDULER_MODEL_NAME=stand-in
export HATCHLING_EGRESS_ALLOWLIST=local-keyless:localhost:18089
export HATCHLING_EGRESS_GRANTS=localhost:18089
export HATCHLING_API_GRANTS=egress:localhost:18089,channel-send:local
```

Start a stand-in for a model server, seal the vault, and start the broker:

```bash
python3 docs/examples/stand-in-model.py 18089 &
hatchling-broker migrate-vault
hatchling-broker &
```

Give the agent its first task, as yourself:

```bash
sudo -u "#$SUDO_UID" python3 docs/examples/first-run.py "$HATCHLING_API_SOCKET"
```

```text
sent: Introduce yourself in one sentence.
waiting for the agent...
agent: Hello from inside a Hatchling cell. This answer came from the stand-in model; ...
```

That answer took the whole path: the broker started a fresh cell as `hatchling-cell` for your message, the agent in it checked that the model can call tools, asked it the task through the egress proxy (the cell's only way out), and replied on the local channel, back through the API to you. The run held the API's grants and nothing more.

**A real model:** run one locally with an OpenAI-compatible API (Ollama, llama.cpp, LM Studio, vLLM), and use its port and model name instead. For Ollama: `http://localhost:11434/v1`, `local-keyless:localhost:11434`, `localhost:11434`, and `egress:localhost:11434` in the API grants. For a hosted API, follow [Using a hosted model](docs/hosted-models.md): the vault holds its key, and the broker supplies it over HTTPS. For endpoints that need thinking disabled, see [model thinking mode](docs/model-thinking.md).

### Using the app

The app is Hatchling's UI: chat with the agent and see what it's doing. `hatchling-host` serves it on `127.0.0.1` and carries its channel to the broker's UI socket. The host decides nothing: the broker asks every connection for the UI token it minted at start, which the host puts in the address it prints, after the `#`, the part a browser never sends to a server.

Build the app and start the host as yourself, still in the clone and the broker's shell:

```bash
(cd frontend && npm ci && npm run build)
sudo -u "#$SUDO_UID" env HATCHLING_UI_SOCKET="$HATCHLING_UI_SOCKET"   HATCHLING_OPERATOR_SOCKET="$HATCHLING_OPERATOR_SOCKET" HATCHLING_HOST_UI="$PWD/frontend/dist" hatchling-host &
```

```text
Hatchling's UI: open http://127.0.0.1:7340/#hatchling-ui=3f9c...
Hatchling's Machine view: open http://127.0.0.1:7340/machine.html#hatchling-operator=8d21...
```

Open that address in a browser on the same machine and say something in the main chat: the agent answers there, each message in a fresh cell, holding the API's grants. On WSL2, a browser in Windows reaches it too. While the broker runs, the address is a key to your agent: don't share it. Each broker start mints a new token, so restart the host after the broker. `HATCHLING_HOST_PORT` picks another port.

The second address is the **Machine view**, for whoever runs the machine: the broker's version and uptime, the cells running, the connections open and the recent egress decisions, refreshed every two seconds. It reads the broker's operator socket, with a token of its own, and sees metadata only: never what anyone said, a task, a secret or a request's body. It can't act as a user, and the app's address can't open it.

CI runs this too: it opens the app's address in Chromium, sends a message and waits for the agent's answer on the page, then opens the Machine view and checks it shows the app's connection and the chat's egress, and none of the message.

### Running without root

The broker runs as root so that each cell can get a uid of its own. Without privilege, Linux lets a process map only its own uid into a user namespace, so by default an unprivileged broker's cells run as you: still in their own namespaces, under seccomp, with no capabilities and no network. In that mode the local API, the chat ingress, the device endpoint and the operator socket refuse to start, because a cell with your uid could otherwise drive them.

An unprivileged broker can give its cells a uid of their own the way rootless containers do, from a subordinate range granted to your user ([WO-014](docs/workorders/workorder-014-running-without-root.md)). With `HATCHLING_CELL_IDS=subordinate`, every cell runs as the first id of your `/etc/subuid` range (and the first of your `/etc/subgid` range), mapped by `newuidmap` and `newgidmap` from the `uidmap` package, with none of your supplementary groups, and the endpoints above start. `HATCHLING_AGENT_UID` can then be left out: the broker takes that first id. If you set it, it must be that id.

All cells share that one id, as a root broker's cells share theirs. Their namespaces keep them apart (none can signal, trace or reach another), but the kernel counts some quotas per host uid: pipe buffers, for one, so a cell that fills its pipes can leave the others' new pipes at their minimum size. Rootless Podman uses the same ranges, so if you run it as the same user, its containers get the same host ids as your cells: give Hatchling a user of its own. The broker refuses a range that holds root, your uid or any account's id, and map helpers that aren't root's setuid programs. A coding CLI's work tree goes to its cell, and comes back after, through a short-lived helper whose user namespace maps both you and the cell (the broker can't `chown` a file to another uid without root).

### Quickstart without root

The broker never runs as root here; setting up the machine for it does, once. CI runs these blocks too, as an unprivileged user on a stock Ubuntu runner with its user-namespace restriction left on, and fails unless the agent's answer arrives and the app answers in a browser.

Build and install as in the quickstart:

```bash
git clone https://github.com/undergroundrap/hatchling
cd hatchling
cargo build --release
sudo install -m 755 target/release/hatchling-broker target/release/hatchling-agent target/release/hatchling-host /usr/local/bin/
```

Install the map helpers (`uidmap` on Debian and Ubuntu; `shadow-utils` on Fedora, `shadow` on Arch), and give your user a subordinate range if it has none. The range must hold no account's id; 100000-165535 is the usual first one:

```bash
sudo apt-get install -y uidmap
me=$(id -un)
grep -q "^$me:" /etc/subuid || sudo usermod --add-subuids 100000-165535 "$me"
grep -q "^$me:" /etc/subgid || sudo usermod --add-subgids 100000-165535 "$me"
```

Ubuntu 23.10 and later let an unprivileged program create a user namespace only if an AppArmor profile allows it. Where that restriction is on, give the broker a profile that allows it user namespaces and confines nothing else, the kind Ubuntu ships for browsers and container tools (for the `hatchling` binary, name `/usr/local/bin/hatchling` instead):

```bash
if [ "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null)" = 1 ]; then
  sudo tee /etc/apparmor.d/hatchling-broker > /dev/null <<'EOF'
abi <abi/4.0>,
include <tunables/global>

profile hatchling-broker /usr/local/bin/hatchling-broker flags=(unconfined) {
  userns,
}
EOF
  sudo apparmor_parser -r /etc/apparmor.d/hatchling-broker
fi
```

The rest runs as you, with no `sudo`. Configure the broker as in the quickstart, with its state in your home, and no agent uid: the cells' ids come from your range.

```bash
export HATCHLING_HOME=$HOME/.local/state/hatchling
mkdir -p -m 700 "$HATCHLING_HOME"/{sock,vault,schedules}
export HATCHLING_CELL_IDS=subordinate   # cells run as the first id of your range
export HATCHLING_BROKER_SOCKET=$HATCHLING_HOME/sock/broker.sock
export HATCHLING_AGENT_BIN=/usr/local/bin/hatchling-agent
export HATCHLING_CELL_ID=cell-01
export HATCHLING_VAULT_DIR=$HATCHLING_HOME/vault
export HATCHLING_SCHEDULER_DIR=$HATCHLING_HOME/schedules
export HATCHLING_CHANNEL_INGRESS=$HATCHLING_HOME/ingress.sock
export HATCHLING_API_SOCKET=$HATCHLING_HOME/api.sock
export HATCHLING_UI_SOCKET=$HATCHLING_HOME/ui.sock
export HATCHLING_OPERATOR_SOCKET=$HATCHLING_HOME/operator.sock
export HATCHLING_SCHEDULER_MODEL_ENDPOINT=http://localhost:18089/v1
export HATCHLING_SCHEDULER_MODEL_NAME=stand-in
export HATCHLING_EGRESS_ALLOWLIST=local-keyless:localhost:18089
export HATCHLING_EGRESS_GRANTS=localhost:18089
export HATCHLING_API_GRANTS=egress:localhost:18089,channel-send:local
```

Start the stand-in model, seal the vault, start the broker, and give the agent its first task:

```bash
python3 docs/examples/stand-in-model.py 18089 &
hatchling-broker migrate-vault
hatchling-broker &
python3 docs/examples/first-run.py "$HATCHLING_API_SOCKET"
```

The answer is the quickstart's, from a cell running as the first id of your range. The app works the same way: follow [Using the app](#using-the-app) in this shell.

### Cell limits

Every cell is held to hard limits it can't lift: they're both its soft and its hard limits, and raising a hard limit takes a capability no cell holds. Set them in the broker's environment:

| Variable | Default | Limits |
| --- | --- | --- |
| `HATCHLING_CELL_MAX_PROCESSES` | 4096 | processes the cell holds at once, threads included |
| `HATCHLING_CELL_MAX_OPEN_FILES` | 8192 | files each program holds open at once |
| `HATCHLING_CELL_MAX_FILE_SIZE` | 8589934592 (8 GiB) | the largest file a program writes, in bytes |
| `HATCHLING_CELL_MAX_ADDRESS_SPACE` | unlimited | each program's address space, in bytes |
| `HATCHLING_CELL_MAX_MEMORY` | half the host's physical memory | whole-cell memory in bytes; requires a cgroup |
| `HATCHLING_CELL_MAX_CPUS` | unlimited | whole-cell CPU cores, e.g. `1.5`; requires a cgroup |

Each takes a positive number, or `unlimited`. Rlimits leave the broker's own bound when unlimited, and a value above its hard limit is held at that limit. The address space isn't a memory bound: a JavaScript runtime reserves far more address space than it uses, so a coding CLI needs it left high. CPU takes decimal digits without a sign or exponent, with up to three decimal places and at least `0.01` cores, using a 100 ms scheduling period; unlimited still shares CPU by the kernel's group weights.

#### Giving Hatchling a cgroup

Set `HATCHLING_CELL_CGROUP` to an empty cgroup v2 directory delegated to the broker. Hatchling enables its `pids`, `memory` and `cpu` controllers and creates a child group per cell. Its limits are written before the first child is placed there and released; every descendant inherits membership. The process bound counts the supervisor, init and program too. The broker stays in its own group. For helper-backed skills, harnesses and scheduled runs it retains an outer cleanup group, so cancelling or killing the helper also removes its cell groups. When a cell ends or its handle is dropped, Hatchling kills the whole group with `cgroup.kill`, waits up to five seconds for it to empty, and removes it. This mode needs Linux 5.14+ for `cgroup.kill` and access to the common ancestor's `cgroup.procs` when placing children. The group path is never mounted in a cell.

With systemd 254 or later, add these settings to the broker's service:

```ini
[Service]
Delegate=yes
DelegateSubgroup=broker
Environment=HATCHLING_CELL_CGROUP=/sys/fs/cgroup/system.slice/hatchling.service
```

Systemd starts the broker in the `broker` subgroup, leaving the delegated service directory empty. Adjust the path for your unit and slice. See [systemd's delegation documentation](https://systemd.io/CGROUP_DELEGATION/).

For a manual root-run broker, run this in a root shell (use a fresh directory):

```sh
cg=/sys/fs/cgroup/hatchling
mkdir "$cg"
echo '+pids +memory +cpu' > "$cg/cgroup.subtree_control"
mkdir "$cg/broker"
echo $$ > "$cg/broker/cgroup.procs"
export HATCHLING_CELL_CGROUP="$cg"
exec hatchling broker
```

An unprivileged broker additionally needs the directory and its `cgroup.procs` and `cgroup.subtree_control`, and the broker leaf, delegated to its user; start it from that leaf so the source and destination share a writable ancestor. See [the kernel's delegation rules](https://docs.kernel.org/admin-guide/cgroup-v2.html#delegation). Hatchling never provisions this itself.

An abrupt kill or crash of the entire broker can leave cgroup directories and helper-backed cells until their helpers terminate. Whole-broker crash recovery is outside this work; [WO-012](docs/workorders/workorder-012-disposable-state-and-cleanup.md) covers the planned cleanup of filesystem staging and run directories.

Unset `HATCHLING_CELL_CGROUP` uses the hard rlimits alone and logs one startup warning. `RLIMIT_NPROC` is per real host uid, so this fallback can share a process budget with other cells or host processes; it does not bound aggregate memory or CPU. The memory and CPU variables have no effect in this mode. With a cgroup, `pids.max` replaces the configured `RLIMIT_NPROC`; inherited host rlimits still apply. A configured directory that is missing, non-cgroup, unwritable, holds processes, lacks a required controller or lacks `cgroup.kill` makes startup fail closed.

### The browser

The agent can drive a real browser: a headless Chromium in a cell of its own, with no network but one way out, the broker's `CONNECT` proxy. A page loads only from sites you've approved: opening a site you haven't is put to you, once, for the task or always, as for a page the agent reads, and what a page then needs from other sites is refused until you allow it. Hatchling bundles no browser; you name one, pinned, in the broker's environment:

| Variable | Sets |
| --- | --- |
| `HATCHLING_BROWSER` | the browser's executable, absolute, in a directory of its own: its bundle |
| `HATCHLING_BROWSER_SHA256` | the digest of the whole bundle, every file in that directory; `hatchling-broker browser-digest <browser>` prints it |
| `HATCHLING_BROWSER_CONTROLLER` | the `hatchling-browser` controller, absolute: the cell's program, which drives the browser over its DevTools pipe |
| `HATCHLING_BROWSER_FONTS` | optional: a fonts directory shown to the cell, read-only, so pages render text |

The broker copies the bundle into a snapshot only it can write and starts the browser from that snapshot only if its digest is the pin, so a file changed in the bundle later never reaches a browser. Run as root, the broker reads the bundle as its own user, `hatchling-broker`, which can't search your home: put it where that user can read it, such as `/opt` (`sudo cp -r <bundle> /opt/`). A browser cell needs `HATCHLING_CELL_CGROUP`: the broker refuses to start without one when a browser is configured. Each browser is held to 2 GiB of memory and 512 processes as a whole, at most 4 run at once, and each ends after 30 minutes or with its session.

In the app you watch each browser: the site, the step it's on, every action in your words (never the text it types) and a picture of the page. You can stop it, which seals its sites at once. You can take it over: the agent waits, your clicks, typing and keys go to the page, and when you hand it back, the agent looks at the page again before it acts.

From here: **[the local API](docs/local-api.md)** for every action and event, and **[testing on WSL2](docs/testing-on-wsl2.md)** for a Windows setup.

## Held to a high bar

- Over 2,300 tests (1,983 Rust, 397 frontend), including real-kernel cell tests that fail rather than skip when isolation is unavailable (`HATCHLING_REQUIRE_CELL=1`).
- Mutation testing with a kill floor of 1.0: every mutant of changed code is killed by a test or proven equivalent in writing.
- `unsafe` lives in exactly one crate (`hatchling-sys`), each block justified; every other crate denies it.
- Strict clippy: pedantic and nursery lints, with `unwrap`, `expect`, `panic`, indexing and `as` casts denied in production code. Plus `cargo deny`, a written review of every dependency ([docs/dependency-review.md](docs/dependency-review.md)), and CI on the pinned toolchain and the 1.85 MSRV.
- The docs that describe the wire format are checked against the types: if they drift, the build fails.

## Contributing

PRs are welcome. Read **[CONTRIBUTING.md](CONTRIBUTING.md)** first: the bar is mutation evidence and a green gate, and it's written down so you can meet it. On your first PR, a bot asks you to sign the [CLA](CLA.md) with a one-line comment.

Core work is planned as **work orders**, specs in **[docs/workorders/](docs/workorders/)** that say exactly what done means. Fourteen are queued, most of them security hardening from the pre-launch review. Pick one up, or open an issue to propose one. Frontend changes follow `frontend/STYLE_GUIDE.md`.

**What's next:** each planned version is a [milestone](https://github.com/undergroundrap/hatchling/milestones), ordered by threat in [Order of work](docs/workorders/README.md#order-of-work); what changed is in the [changelog](CHANGELOG.md).

Everyone here follows the **[code of conduct](CODE_OF_CONDUCT.md)**. Found a security problem? Please don't open a public issue — see **[SECURITY.md](SECURITY.md)**.

## License

Hatchling is dual-licensed.

- **Open source: [AGPL-3.0-or-later](LICENSE).** Use it, study it, change it, run it. If you offer a modified Hatchling to others over a network, you share your changes under the same license.
- **Commercial license**, for organizations that need terms the AGPL doesn't give them: private modifications and closed integrations, custom builds for your environment, seat or site licensing, and support. See [oceanbennett.com/hatchling](https://oceanbennett.com/hatchling/#licensing), or contact Ocean Bennett on [LinkedIn](https://www.linkedin.com/in/oceanbennett/) or [X](https://x.com/oceanbennett).

The AGPL comes with three short additional terms, in [NOTICE.md](NOTICE.md): a work built on Hatchling keeps the credit "Built on Hatchling by Ocean Bennett", a modified version says it's modified and doesn't present itself as Hatchling, and the name and logo aren't licensed. [TRADEMARKS.md](TRADEMARKS.md) says in plain words what forks can and can't call themselves.

Contributions are accepted under the [Contributor License Agreement](CLA.md), which lets both licenses cover them. You keep your copyright, and your work stays available under the AGPL.
