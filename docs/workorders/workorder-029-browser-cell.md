# WO-029: A browser cell: a real browser the agent drives, in a cell of its own, reaching only what the broker lets through

**Status:** IN PROGRESS (part 1a, the browser cell's profile, done; part 1b: the controller)
**Area:** `crates/hatchling-browser` (new: the controller that runs in the cell), `crates/hatchling-broker` (the browser session, the proxy, site grants, the UI stream), `crates/hatchling-isolation` (the browser cell's profile), `crates/hatchling-agent` (the tools), `crates/hatchling-core` (the ops; `ui` already has the browser surfaces), `frontend/` (the live browser view)
**Issue:** none yet
**Trust boundary:** yes

## Problem

WO-028 lets the agent read a page as text, from a site the owner allows. Much of the web isn't readable that way: a page built by script, a form to fill, a flow of several steps. Someone who asks Hatchling to "check the status on the carrier's site" or "fill in this form" needs a real browser.

A real browser is the most exposed program there is. It runs the code of every page it opens. So it can't share a cell with the agent, which holds the conversation, and it can't have the network: a page's script would reach the owner's router, their printer, a cloud metadata address. And it can't be steered by what it reads: a page that says "now go to evil.example" mustn't take the agent there.

The UI already has the browser's surfaces, from WO-021: sessions, frames, the action log, and the owner taking control. Nothing in the broker drives them.

## Scope

**In scope:**

- **A browser cell.** Chromium, headless, in a cell of its own: no vault, no project files, no session of the agent's, a profile in the cell's own temporary storage that goes with it. It has a resource group (WO-004's cgroup) or it doesn't start: its memory and processes are bounded as a whole, not only per process.
  - **The operator's binary, pinned.** `HATCHLING_BROWSER` names its path and its SHA-256, which is checked before every start. Hatchling bundles no browser.
  - **A controller in the cell** (`hatchling-browser`, Hatchling's own) drives Chromium over the DevTools protocol on a pipe, never a port. It takes the broker's commands on the cell's one connection, and answers with page snapshots.
- **The browser's only way out is the broker.** The cell has no network but its own loopback, and on it one listener, the cell's relay, which the broker holds and the cell keeps no copy of. Chromium's proxy is that listener, so nothing in the cell forwards anything. The broker serves an HTTPS `CONNECT` proxy on it:
  - `CONNECT` only, to `https`, the default port unless the grant names one; any other request, such as one a page sends to the listener itself, is refused;
  - each host resolved when it's connected to, refused if any address it resolves to is internal (WO-023), and the connection pinned to the addresses checked;
  - TLS end to end between the browser and the site: the broker never sees plaintext, and the browser's certificate checks stay on;
  - every connection logged with its origin, in the egress log the Machine view shows (WO-022);
  - no UDP: WebRTC and QUIC are off.
- **Navigation needs the site's grant.** Opening a page, or a page navigating its top frame, to an origin the session doesn't hold is put to the owner (WO-027's board, WO-028's site grants: once, for the task, always, never), and doesn't load until they say yes. The agent is told it's waiting. What a page then loads from other public origins (images, scripts, data) is allowed and logged, as in any browser.
- **The agent drives through the broker.** The agent's tools (`browser_open`, `browser_act`, `browser_read`, `browser_close`) are ops on its session; the broker passes them to the browser cell. An action names an element from the last snapshot (a click, typed text, a key, a scroll, a choice). A snapshot is the page's text and its actionable elements, bounded, and reaches the model as data.
- **Typed text is checked.** Text the agent types that matches a secret the broker holds is refused before it reaches the page (the output gate's scan).
- **The owner watches and takes over.** Frames (a picture of the page) and the action log reach the app on the UI stream, as WO-021's surfaces define them. The owner can take control: the agent's actions are refused until they hand it back, and their own clicks and keys go to the browser. A handback names the frame it was given at, so the agent knows the page changed under it.
- **Bounded:** one browser session per agent session, a cap on sessions in all, a wall-clock limit, the cell's memory and process limits (WO-004), and the session ends with the agent's.
- **Docs:** the threat model, the configuration, the local API, the CHANGELOG.

**Not in scope:**

- signing in to sites: a profile kept between sessions, its cookies sealed, per project or per owner (its own WO, after this one);
- downloads and uploads (refused), extensions, printing, the clipboard;
- plain `http`, other schemes, and internal sites, even declared ones;
- a browser on macOS (the macOS backend is a candidate);
- holding back a page's own requests to other sites (see Notes).

## Acceptance criteria

- [ ] With a real Chromium in a real cell: a page's script that fetches an internal address, plain `http`, or a UDP peer gets nothing, and no packet leaves the cell except through the broker's proxy.
- [ ] Opening, or a page navigating to, an origin the session doesn't hold loads nothing: it asks the owner, and once, for the task, always and never each do exactly what they do for `web_fetch` (WO-028).
- [ ] Every connection the browser makes is in the egress log with its origin; the broker sees no plaintext, and a site with a bad certificate doesn't load.
- [ ] A binary whose SHA-256 isn't the configured one isn't started.
- [ ] Text the agent types that matches a held secret is refused, and never reaches the page.
- [ ] While the owner holds control, the agent's actions are refused; after the handback the agent is told the frame changed. Tested in a browser, in the app.
- [ ] Nothing of a session outlives it: its cell, profile and temporary files are gone when it ends, however it ends.
- [ ] Everything a page shows the agent reaches the model inside the untrusted-data envelope; a page that says it approved a site approves nothing.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates). The browser cell's tests need a Chromium: the sandbox and CI install one, and its tests run with `HATCHLING_REQUIRE_CELL=1` like every real-cell test.

## Evidence

Filled in at close.

## Parts

1. **The cell and the controller:** Chromium in a cell of its own, pinned, with no network at all; the controller over a DevTools pipe; open, snapshot, act, close, against a page the test serves through a test proxy.
2. **The proxy:** the broker's `CONNECT` proxy on the cell's connection, `https` and public only, resolved and pinned per connection, logged; UDP off.
3. **Grants and the tools:** navigation put to the owner with WO-028's grants; the agent's four tools; typed text scanned.
4. **The owner in the app:** frames, the action log, and taking and returning control, in the live frontend.

## Notes

- **What the cell lets Chromium do, measured.** A traced headless run (`--no-sandbox --no-zygote --disable-gpu --disable-dev-shm-usage`) made 90 distinct calls; 15 were outside the CLI profile. Leaving each out in a real cell in turn: seven are needed (without `sendmsg`, `recvmsg`, `epoll_wait`, `ppoll`, `fallocate` or `memfd_create` it is killed; without `mremap` a child is, and the browser hangs waiting for it); five are probes it handles as errors; two (`fstatfs`, `link`) aren't needed. That a dead child hangs the browser is why every wait the controller makes is bounded. CI's kernel audit then showed a call the trace never had: `sched_setaffinity` killed three of Chromium's processes there, and the sandbox's log showed the same, fifteen times, while the page still rendered and the test passed. Outside a cell Chromium never makes it; in a cell, where the thread priorities are refused, its processes make it instead (the likely reason: it falls back to affinity when it can't set a priority). It's answered `EPERM` like the priorities. A call the cell itself brings about, killing a child the page didn't need, is why the trace alone isn't the measure, and why the controller's tests load a real page through the proxy, which needs the processes a `data:` page doesn't.

- **Name the threat, then the mechanism.**
  - *A page's script reaching the owner's network* (the router, a printer, cloud metadata): the cell has no network, and the proxy refuses internal addresses and anything but `https`.
  - *What the agent reads steering it to a new site:* navigation needs the owner's grant, and nothing a page says grants anything.
  - *The agent typing a secret into a page:* the cell holds no secret, and typed text is scanned against the ones the broker holds.
  - *A compromised renderer reaching the rest of the machine:* the cell (namespaces, seccomp, limits) is the boundary; Chromium's own sandbox, where it can run inside the cell, is a second layer, not the one relied on.
- **Why a page's other requests aren't held back.** Almost every page loads from origins it doesn't name in its address: a CDN, fonts, an API. Asking the owner about each would make prompts nobody reads, and a site the owner granted already has whatever the agent gives it, including the power to send it on. What holding them back would stop is a compromised renderer moving one granted site's data to another public origin. The proxy logs every origin, so that's visible; a strict mode that allows only granted origins is a candidate, for those who'd trade breakage for it.
- **What a compromised renderer finds in the cell.** Chromium runs without its own sandbox, so a renderer exploit can open any file the cell can and talk to every process in it, the controller included. So the cell holds nothing worth taking:
  - in its filesystem: the browser's directory (read-only), the profile in the cell's temporary storage, its work directory, and a `/proc` of its own processes; a test lists the mounts;
  - in a process's arguments or environment: no secret and no token. The cell's connection to the broker is a descriptor it was started with;
  - in Chromium's descriptors: only 0 to 4. The controller marks every descriptor close-on-exec, puts the DevTools pipe on 3 and 4, and keeps its own connection to the broker from crossing; a test lists them.

  And `file://` is never navigated to: a site grant is for an `https` origin.
- **Bounded as a whole.** A page can fill memory with files it never maps (`memfd_create`, `fallocate`), which no per-process limit counts, and start processes up to the cell's limit. So a browser cell needs its resource group: without one configured it isn't started, rather than started with less.
- **Chromium's own seccomp layer, if it can run.** Its sandbox is two layers: namespaces, which a cell may never make, and a `seccomp` filter on its renderers, which can only narrow what the cell allows. Whether the second runs without the first is measured in part 1. If it does, the browser profile allows `seccomp` to add a filter, and nothing else: not `SECCOMP_FILTER_FLAG_NEW_LISTENER`, which would make a process the judge of another's calls. Either way the cell is the boundary.
- **Why the controller isn't trusted.** It runs beside page code, so a renderer exploit can reach it. Navigation grants are enforced where the controller asks the broker, which keeps an honest agent honest; the boundary against a compromised cell is the proxy, which the cell can't bypass, and which allows no more than any page's requests already could.
- **Signed-in profiles, later: one cell per profile.** Without its own sandbox, Chromium can't keep one site's renderer from another's, so the sign-in WO puts each signed-in profile in a cell of its own. That rebuilds site isolation at the kernel instead of in the browser. A profile is what the cell holds, not a host name: its grants include the sites its sign-in goes through (an identity provider, its scripts), and two profiles never share a cell. Signing in through a shared provider leaves that provider's session in each cell that used it, which is a reason to keep a profile's grants narrow.
- **Order.** After WO-028, whose site grants this reuses; before the browser's sign-in profiles.
