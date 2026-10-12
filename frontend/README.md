# Hatchling Frontend

Revision **v10 + UI touch-up + reliability UX + approvals/mascot + shared loading, style guide and consistent popup menus**, package **1.12.0**. A frontend-only personal-agent workspace with strict TypeScript, native ES modules, original icon motion, and entirely synthetic local fixtures. No live backend is implemented. Screens, consent flows and browser frames are demonstrations, not connected account activity.

Start every UI change with **[STYLE_GUIDE.md](STYLE_GUIDE.md)** and **[docs/UI-CHECKLIST.md](docs/UI-CHECKLIST.md)**. The latest iteration brings every action popup into the existing More-menu style and fixes navigation in script-enabled sandboxed previews. The preceding iteration added reusable text/media skeletons, real image loading/error/retry handling, explicit content-loading previews and a token/style guard. See **[docs/DESIGN-SYSTEM.md](docs/DESIGN-SYSTEM.md)** for scope.

The preceding iteration centered the reaction plus, replaced the default companion with the supplied mascot, and put approvals above the composer.

The existing loading/retry states, visible outbox, per-tab draft recovery, and separate bottom **Connector chats** section remain.

The updated source and both compiled previews are included. Existing behavior and text-size settings are retained.

## Run

Place this directory at **`hatchling/frontend/`**. The package identifier stays `hatchling-frontend`; no source path depends on its enclosing directory name.

```sh
cd frontend
npm ci
npm run dev
```

Requires Node.js 22.12 or newer. Open `http://localhost:5173`. The development watcher rebuilds TypeScript and copies styles/assets; refresh after editing. `npm run build` also produces `dist/hatchling-standalone.html`, an offline preview that needs no installation. `npm run preview` serves `dist/` on port 4173. No runtime npm packages or font files are shipped. The single npm development dependency is the pinned TypeScript compiler.

### Included handoff preview

This handoff intentionally includes `preview/Hatchling-clickaround-preview.html` plus two verification screenshots. The preview is generated output for click-through/reference; edit the TypeScript source and rebuild rather than editing the preview directly.

### Opening the preview

Open **`dist/hatchling-standalone.html`** directly in a browser, or use the separately supplied standalone HTML. An attachment viewer may display the file without running its scripts; use the downloaded file in that case. Script-enabled sandboxed frames now navigate without requiring URL-history permission. This is still local synthetic data, not a hosted or connected service.

For the included module build, run **`npm run preview`** and visit `http://localhost:4173`. Serving the already-built files only requires Node; `npm ci` is needed for editing/rebuilding, not for this preview command. Do not open `dist/index.html` directly as a file, because it uses ES modules. No browser security settings need to be disabled.

## Validate

```sh
npm run check
python3 -m pip install -r tests/e2e/requirements.txt
python3 -m playwright install chromium
npm run check:all
# Audit against the original v10 presentation lock; UI differences are expected.
npm run integration:check
npm run screenshots
npm run capture:motion
```

`check` runs strict typing, the project-owned AST linter (not ESLint), the shared design contract, generated-schema drift, the frozen protocol check, asset verification, formatting, build and unit tests. `check:all` adds Chromium interaction tests, static HTTP smoke tests and CLI/MCP companion integration tests. Browser tests accept `CHROMIUM_PATH`. The original product suite (`test_app.py`) can use `HATCHLING_BASE_URL=http://localhost:4173` for an origin-backed page; reliability and approval suites use the compiled standalone document. Screenshot capture uses the compiled app, never a screenshot as its UI.

Run one browser case with `python3 tests/e2e/test_app.py FrontendTests.test_160_profile_return_staggers_name_and_status_in_actual_animations`.

## Explore

Main chat has a **Chats** pill; side chats show a title and a Back-to-Main arrow. Each conversation retains its draft and scroll position. The agent portrait collapses to the conversation center. Restoring it brings the portrait, name and status back together; active work changes the status from adapter data.

Settings → General offers Compact, Comfortable (default) and Large, independently of appearance mode and accent. White remains the default accent. Matching the uploaded avatar is explicit and optional.

WhatsApp and Messenger live in the separate bottom **Connector chats** section.

Press **Ctrl/Cmd + Shift + D** for the scripted scenario chooser. The **Loading & skeletons** group shows first-load, retry, cached-refresh and reusable component examples. These previews are labeled and do not alter your content; ordinary loaded routes do not wait for an animation. Use **Task approvals → Task approval above the input** for the new approval flow, or the multiple, expired, failed-save, and unconfirmed-decision cases in the same group. The **Loading & delivery edge cases** group lets you try loading errors, offline queues, a workspace restart, slow/rejected/uncertain sends, session recovery and attachment failure. It covers live-looking activity, browser watching/takeover, exact-quote purchases, uncertain outcomes, CAPTCHA gates, channel states and mixed chat messages. All external effects remain stubbed. Markdown documents edit locally; attachments can be downloaded; generated public-share links are local snapshots, not hosted URLs.

## Agent handoff

For backend wiring, start with **docs/BACKEND-INTEGRATION-CHECKLIST.md**, then read **docs/LIVE-ADAPTER-HANDOFF.md** and **AGENTS.md**. For UI context, read **STYLE_GUIDE.md**, **docs/UI-CHECKLIST.md**, **docs/DESIGN-SYSTEM.md**, **docs/PARITY-V10.md**, and **docs/TEST-RESULTS.md**. `ProtocolAdapter` remains the frozen seam. The optional `chatSession` and `taskApprovals` UI capabilities require integration review and authoritative live receipts; it is not a change to that frozen protocol. There is no live implementation. Changes to the interface require review before implementation; selecting a future live adapter also requires the one approved composition-root change in `src/main.ts`.

The optional CLI/MCP developer companion is disabled by default and needs explicit tab enablement. See `docs/AGENT-CONTROL.md`. It is not production authorization, remote browser streaming, or a hosted agent runtime.

## Validation limitation for this delivery

During the preceding touch-up delivery, a genuinely fresh `npm ci` was attempted after removing `node_modules`; registry access failed (`EAI_AGAIN`), and the offline cache was empty. The checked-in lockfile is pinned and includes the registry-published TypeScript integrity hash. Executed checks used the already-installed matching TypeScript 5.8.3 compiler. The present style-guide/loading pass repeated `npm ci` with a short timeout; registry DNS again failed (`EAI_AGAIN`). Checks used the matching installed compiler. This is **not** a clean-install pass. Managed Chromium rejects URL navigation with `ERR_BLOCKED_BY_ADMINISTRATOR`; browser policy was not modified. The loading suite uses the compiled standalone document, with test-only seams for pending image sources and generation promises. Image success/failure also exercises actual data-image browser events. Product/reliability/approval suites use the standalone document. HTTP assets are also checked separately; this does not certify deployment-origin behavior. Remote CI was not run.

The included `.github/workflows/ci.yml` is a standalone-repository template. Once this directory sits under a parent repository, GitHub does not discover that nested workflow automatically: the integration owner must move/adapt it under the repository's root `.github/workflows/`, using working-directory `frontend` and `frontend/package-lock.json` for npm caching. Remote CI remains that owner's task.
