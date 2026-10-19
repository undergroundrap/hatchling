# Hatchling UI / UX style guide

**Design contract · 1.12.0 · 25 September 2026**

Read this before changing any screen. This guide describes the existing Hatchling app and its approved iterations; it is not a new visual direction. The current source, actual-app screenshots, and acceptance tests accompany it. Changes to a shared pattern must update its implementation, this guide, and the affected tests together.

## 1. Keep the identity and the interaction model

Hatchling is a quiet, personal workspace. Use neutral surfaces, readable text, restrained borders, generous reading space and a small number of clear actions. Do not turn each page into a dashboard of decorated cards. Ideas and goals use open rows; documents are reading surfaces; cards are appropriate for contained files, previews, approvals and notices.

Use **Hatchling** in prose and **hatchling** in the existing product-name treatment. The only approved default companion is the supplied chick in `public/media/hatchling-mascot.png`. Preserve the full original; avatar derivatives, image samples, video posters and actual sample footage must agree. Respect a user's custom avatar. Provider icons identify services, not the assistant. Do not introduce a new mascot, stock companion, generated mockup, or screenshot as app content.

Preserve these product decisions:

- Side chats and **Connector chats** are separate. Connector chats remain at the bottom with independent collapse, unread and connection states.
- A pending task approval sits **above the composer**, in the layout, not over the input. Request identity and outcomes match Activity and Approvals history.
- Drafts, outbox items, file preparation, scroll position and approvals remain attached to the owning chat. A late response does not navigate the user away or overwrite their next draft.
- “Permission allowed” does not mean “action completed.” Missing delivery, payment or approval evidence must never be dressed up as success.
- Do not remount an active editor or the persistent profile for unrelated updates. Keep focus, selection, independent scrolling, and reduced-motion cancellation.

The uploaded mobile views are **future design references**, not approval to replace the current navigation now. Their useful patterns are noted in section 9. Never copy their account data, conversation text, research reports or browser chrome into fixtures.

## 2. Sources of truth and ownership

| Concern | Owner |
| --- | --- |
| Core palette, fonts and shared new primitives | `src/styles/tokens.css` |
| Native control defaults, shell and baseline components | `src/styles/app.css`, `workspace.css` |
| Ideas, goals, feed and library visual patterns | `src/styles/surfaces.css` |
| Comfortable / Compact / Large scale rules | `src/styles/polish.css` |
| Current chat and reading-column proportions | `src/styles/touchup.css` |
| Conversation location and navigation | `src/styles/chat-navigation.css` |
| Delivery, history recovery and connector organization | `src/styles/reliability.css` |
| Approval cards, approval history, reaction alignment | `src/styles/approvals.css` |
| Shared loading / media states | `src/styles/loading.css` |
| All popup action-menu surfaces and rows | `src/styles/menus.css`, `--menu-*` tokens |
| Embedded-preview-safe URL history | `src/app/route-location.ts` |
| Native buttons, menus, dialogs, SVGs | `src/components/ui.ts`, `dialog.ts`, `floating.ts`, `icons.ts` |
| Shared loading markup and state views | `skeleton.ts`, `loading-image.ts`, `content-state.ts` in `src/components/` |
| View-only resource operation / demo state | `src/app/content-loading.ts` |

This app has a historical CSS cascade. **Do not reorder, flatten or globally refactor it as an incidental fix.** Some early root geometry is deliberately overridden by later files, and sidebar resizing adds a view-owned width. Inspect computed styles, not just the first token definition. `index.html` is the stylesheet-order manifest; the standalone builder now reads that same order so the two previews cannot silently omit a new stylesheet.

For a local fix, edit the owning component. For a genuinely new family, add one scoped file and reuse shared tokens. Do not add another last-loaded overrides sheet for every iteration. Do not change protocol locks or the original presentation lock to hide a difference.

## 3. Color and elevation

Use semantic names, not hard-coded hex values in new feature CSS. Theme selection remains Dark, Light or System. Colors below are the current core tokens; they are not permission to recolor the app.

| Token | Dark | Light | Use |
| --- | --- | --- | --- |
| `--bg` | `#181818` | `#ffffff` | Workspace canvas |
| `--surface` | `#202020` | `#f1f2f4` | Subtle contained area |
| `--surface-raised` | `#282828` | `#e9ebee` | Higher surface / muted card |
| `--surface-control` | `#424242` | `#e4e7eb` | Neutral controls |
| `--composer-bg` | `#323232` | `#e4e7eb` | Composer / compact navigation |
| `--text` | `#f3f4f5` | `#22252a` | Primary text |
| `--muted` | `#b4b7bc` | `#5c626d` | Explanations / secondary text |
| `--border` | `#303030` | `#e0e3e7` | Surface boundaries |
| `--border-strong` | `#62666c` | `#828997` | Inputs and emphasized boundaries |
| `--focus` | `#8ecbff` | `#0064b8` | Keyboard focus |
| `--primary` | `#2496f3` | `#0071cd` | Blue system accents / send control |
| `--primary-text` | `#071523` | `#ffffff` | Text on that blue treatment |

Use `--success`, `--attention` and `--danger` only for their semantic states. A status also needs a word or icon; color is not sufficient. Editorial links, tracking and context typography have their existing tokens in `surfaces.css` and `tokens.css`. New status body text should use the higher-contrast `--muted`, not a low-opacity paragraph.

**White is the default selectable accent.** `features/settings/appearance.ts` derives `--bubble-user` / `--bubble-text` and `--theme-color` / `--theme-on-color` together; do not override only a background. `details.css` applies the selected theme pair to primary buttons (white with dark text by default), while existing system accents and the send control use the blue `--primary` pair. Do not change primary buttons to blue just because the base palette contains blue. Avatar-derived color remains opt-in.

Prefer separation through surface and border. Reserve the existing `--shadow` for elevation such as a dialog. Do not add glows, colored gradients or saturated skeletons.

## 4. Typography, spacing and geometry

Reuse `--font-ui` and `--font-mono`. There are no downloadable or bundled fonts. Do not introduce a separate heading font, CSS `zoom`, or a global transform to fit more text.

Body text in the existing shell starts at 15px times `--ui-scale`; chat text starts at 16px times that scale. Page headings generally start at 34px, section headings at 20px, labels at 13–14px and metadata at 11–12px. Preserve each established component's reading density. For **new** primitives use `--text-xs`, `--text-sm`, `--text-body`, `--text-title` (12, 14, 16, 20 base pixels, multiplied by the interface scale).

| Preference | Desktop text / spacing multiplier |
| --- | --- |
| Compact | 1 / 1 |
| Comfortable, default | 1.12 / 1.08 |
| Large | 1.25 / 1.16 |

Phone overrides already exist: at 600px and below the text scale is 1, or 1.12 for Large. At 800px and below the spacing multiplier becomes 1. Do not replace these with a whole-page scale. Changing appearance or font size is a user preference, not a way to make a screenshot pass.

New spacing follows the existing four-pixel rhythm: `--space-1/2/3/4/6/8/12` = 4/8/12/16/24/32/48 base pixels, multiplied by `--ui-space`. Prefer a small number of meaningful gaps to one-off nudges.

New radius defaults: `--radius-xs/sm/md/lg/xl` = 6/10/14/18/24px; `--radius-pill` for true pills and circles. Existing approved components retain their current radii (for example a 20px approval card). This is not an instruction to restyle them all.

At the current desktop touch-up baseline, reading width uses `--chat-width` (712 × spacing scale), assistant bubbles 556 × scale, user bubbles 482 × scale, compact cards 296 × scale. The touch-up rail is 64 × scale, context panel 334 × scale; sidebar width is also user-resizable. These are maxima in available space, not fixed widths that may overflow.

Long file names truncate in lists but remain available on open; permission scopes, errors and important instructions wrap. Never clip consent text just to match a card height.

## 5. Controls, icons and layers

Use `button()`, `iconButton()`, `menu()`, the existing dialog manager and artifact dock. Do not create fake clickable divs or put a button inside another button. Icon-only actions need accessible names and visible keyboard focus. The base focus ring is 2px with an offset; do not remove it without a replacement.

Icons are existing SVGs, centered with flex or grid. The reaction plus is **a 16 × 16 SVG inside a 32 × 32 circle**, not a text glyph with a font-baseline adjustment. Keep the circle geometrically centered. Touch hit areas may be larger than the glyph; new touch actions should use the `--control-touch` 44px minimum. The existing compact reaction row is a documented exception, not a model for every new action.

Preserve menu viewport clamping, focus return, click-outside behavior, Escape dismissing only the top layer and safe-area spacing. Hover-revealed controls must also appear on focus and remain usable without hover. Do not introduce arbitrary large z-index values. Use the established menu/dialog/dock owners.

### One action-menu style

**The bottom More menu is the visual reference for every ordinary popup action list.** Chat options, Rename / Pin / Archive / Delete, connector actions, attachments, avatar editing, Feed, Ideas, Goals, artifact actions, documents and Activity reuse `menu()` plus `src/styles/menus.css`. There are no separate sidebar, file or avatar popup themes.

Use `--menu-bg` (the composer surface), `--menu-border` (a quiet border, not the strong input border), `--menu-shadow`, the 18px `--menu-radius`, 8px padding, and a shared 10px row radius. Labels use `--text-sm`, regular weight, a 20px scaled line height, 16px SVGs and 10px icon gaps. Desktop rows use the existing More-menu spacing; coarse-pointer rows are at least 44px. Width can grow for content, is clamped to the viewport, and long labels wrap rather than overflow. Keep destructive actions readable with `--danger`, but do not turn them into solid red buttons. Disabled actions retain their disabled state and cannot be invoked.

Create group boundaries with the `separatorBefore` action option. The helper inserts a real separator; do not add a border/padding hack to the first or last row. Keyboard shortcuts are trailing `kbd` elements. A toggle must retain its check state and accessible semantics. The action-list disclosure uses native buttons and named groups; it does not give ordinary buttons incomplete ARIA menu-item behavior. The existing chat-preferences list remains a checkbox menu.

Opening with a keyboard focuses a visible item. Arrow keys, Home / End, type-to-focus, Escape and Tab must work without stealing focus from a dialog or editor. Escape closes only the menu. Clicking another menu closes the previous one. Top-layer placement and a fixed-position fallback keep menus inside the viewport, including menus opened from a document/dialog. Preserve the anchor or close the menu if it scrolls out of view. Re-anchor on content-size changes so a late or longer label cannot push the popup off-screen; detach observers when the owner closes or is removed.

**Exception:** the horizontal emoji reaction picker remains a compact pill with its centered plus. It shares neutral surface colors but is not a vertical action list. Native form selects and dialogs are also distinct controls, not new action-menu themes.

The menu ownership check applies even to historical stylesheets. Do not reintroduce `.chat-nav-row .menu-popover` colors, radii, fonts, shadows or padding in a feature file. Change the shared tokens/component and test every owner instead. Position-only rules and the documented reaction picker are permitted.

## 6. Loading is part of the component

A skeleton reserves the shape of content that is **actually unavailable**. It is not a decorative effect shown on every navigation.

| State | Required treatment |
| --- | --- |
| Initial unavailable content | Keep navigation and headings, show a named loading status and matching skeleton shape |
| Available cached content refreshing | Keep readable content; show a small updating notice, no replacement skeleton |
| Some text or images ready | Render ready parts; only missing parts stay placeholders |
| Streaming assistant text | Show received text; do not cover it with an entire fake paragraph |
| Loaded but empty | Explain the empty state and a useful action; no endless skeleton |
| Load failure | Stop animation; explain what failed and offer a scoped retry when safe |
| Slow read | Say it is taking longer; keep the workspace usable; do not invent percentages or success |
| Retry | Preserve scope, metadata, drafts and selections; do not reset the whole workspace |

Use `skeletonContent('ideas' | 'feed' | 'library' | 'document' | 'chat')` for decorative shapes, or `loadingRegion(label, kind)` for a named busy region. Library shapes support both grid and list. Every bar uses the same neutral `--skeleton-base`, stable dimensions and rounded treatment. Width variation is deterministic; no random layout shifts on re-render.

The low-key pulse is 1800ms, enabled **only** when reduced motion is not requested. Reduced motion keeps a static shape. Status labels stay opaque and readable. Do not add a second shimmer animation per page.

The image component uses actual image `load` / `error` events, handles cache hits, and reserves a frame through loading, success and error. For a known aspect ratio supply width and height. For unknown dimensions it reserves a square or the parent's thumbnail frame and contains the image; do not claim an unknown size is known. Full viewers may contain Retry; thumbnails show an error affordance and open the retry-enabled viewer instead of nesting controls.

Image watchdogs start when an eager image is requested or a lazy image reaches the viewport. After 20 seconds without a result the UI offers recovery rather than pulsing forever. Retrying reloads only that image and keeps its original URL (no cache-busting signed URLs). Detached pending images clean up their listeners, observers and timers. A timed-out image is a failed preview, not evidence that the artifact was deleted.

Fast operations are never forced to wait for a skeleton. Non-demo content-operation notices use a 120ms reveal threshold; successful operations settle immediately. Image pulse reveal also has a short delay. No minimum skeleton hold time is imposed.

### What is connected in this build

Chat skeletons are driven by the existing history phase. Feed generation tracks its actual pending command and preserves a loaded edition. Chat attachments, library images/video posters, feed images and opened image viewers use real browser image events.

Ideas, Feed first-load, and library resource-delay/error examples are explicit **local preview scenarios**. The frozen `AppState` supplies those collections synchronously, so normal navigation renders them immediately. `ContentLoading` is view state, **not a backend readiness API**. The document skeleton is a reusable component example; it does not delay or replace an already-loaded editor. A live adapter needs a reviewed readiness mapping before these surfaces can represent remote first-load status.

Use Ctrl/Cmd + Shift + D → **Loading & skeletons** to inspect the examples. They are labeled as previews, settle after six seconds or offer End preview, and do not change stored content. Failure examples have an explicit retry. They must never be enabled silently in live mode.

### Accessible loading semantics

Decorative bars have `aria-hidden="true"` and no controls. Mark the relevant content region busy, not the entire app. Keep one polite status **outside** that busy region so it is not deferred with the unfinished content. Reset busy state on both success and failure. Ordinary loading must not steal focus; only an explicit user action may reposition it. Do not announce a dozen image thumbnails individually.

Technical basis: MDN's [ARIA: aria-busy](https://developer.mozilla.org/en-US/docs/Web/Accessibility/ARIA/Reference/Attributes/aria-busy) explains deferred updates; [prefers-reduced-motion](https://developer.mozilla.org/en-US/docs/Web/CSS/Reference/At-rules/@media/prefers-reduced-motion) explains the motion preference; [HTMLImageElement.complete](https://developer.mozilla.org/en-US/docs/Web/API/HTMLImageElement/complete) notes that a broken image may also report complete. These references were consulted on 25 September 2026. Consult these with the actual component code rather than treating an animation as evidence of readiness.

## 7. Delivery, approvals and honest copy

Use these established terms consistently:

| Condition | Label / next action |
| --- | --- |
| Not dispatched, waiting for connection | **Queued** |
| Dispatch in progress | **Sending…**, softened bubble, opaque status |
| Authoritative acceptance | **Sent**; this is not a recipient read receipt |
| Definite rejection | **Not sent** → Retry / Edit / Copy / Remove |
| Delivery uncertain | **Delivery not confirmed** → Check status; no blind resend |
| Permission awaiting review | **Task approval needed**; exact request, reason and scope |
| Permission decision saving | **Saving your decision**; disable repeated actions |
| Permission response uncertain | **Decision not confirmed** → Check status, not a replay |

A task approval that carries the request's fields (WO-027) lists them in the card, above the scope: what the broker resolved first, then the agent's own words, each agent field labeled **From the agent**. Values are inert text, isolated for direction, whole (they wrap, never truncate), and every hidden or direction-control character is shown as its code point (`⟨U+202E⟩`). Agent text never becomes a button, a label or a notification.

Keep uncertainty explicit. “Dismiss” is not “Cancel the original action.” Permission approval is never silently queued while offline. A purchase must open the existing complete quote; generic Allow does not bypass purchase review. Expired requests stay in history without live approval controls.

Errors should name the failed thing, say what remains safe, and offer the next action. Prefer “This conversation couldn’t load. Try again; your draft will stay here.” to raw transport errors or a vague “Oops.” Do not insert real account names, news claims or copied report text into the synthetic examples.

## 8. Motion and accessibility review

Use `--motion-fast` (140ms), `--motion-standard` (180ms) and `--motion-ease` for new short interactions. Existing profile / panel motion has its own approved timing and cancellation; reuse it instead of changing everything to these values. Animate a changed state, not each unrelated snapshot. Never use motion or color as the only indication.

Test dark and light, normal and reduced motion, all three interface sizes, keyboard-only operation, long labels and narrow containers. The existing contrast tests validate token pairs, not every possible rendered composition. DOM/browser assertions are not a screen-reader certification; manual assistive-technology review remains a separate release activity.

## 9. Mobile direction, deliberately deferred

Keep the current responsive layout working. The supplied phone references suggest a simple bottom area switcher, an overlay chat list, a prominent page title, open editorial rows, artifact category filtering, a bottom overflow sheet and document controls with the reading surface protected. The screenshot's address bar, time, battery, back button and tab bar belong to the browser/OS, not Hatchling.

This pass does **not** replace navigation, implement a new bottom tab system or copy the mobile page proportions. When that work is requested, review safe areas, dynamic viewport height, the on-screen keyboard, drawer focus, long lists, touch actions and approval/composer stacking together. Continue checking 320px and 390px layouts now for regressions, not claiming mobile parity or an iOS Safari test.

## 10. Change process and gates

Before editing, identify the owning component and the UX state that changes. Reuse before adding. For an exception, record why the existing pattern cannot work in `DECISIONS.md`; a new token belongs in `tokens.css` and in the small machine-readable contract at `docs/DESIGN-TOKENS.json`.

Run:

```sh
npm run design:check
npm run check
npm run test:e2e
npm run test:smoke
npm run test:agent
```

Capture the **actual app** after a build in normal, loading, error, populated and cached-refresh states as applicable. Use `tests/e2e/test_loading.py` for loading captures and controlled image loading/failure fixtures. Test one ordinary chat, the plus picker, a pending approval, a failed/uncertain send and an open document after a shared-component change. Use `docs/UI-CHECKLIST.md` for review.

`design:check` checks that the guide, agent instructions, referenced tokens and stylesheet manifest agree. It rejects raw colors, ad-hoc font families and CSS zoom in new family stylesheets. Legacy CSS is explicitly grandfathered for the general raw-color scan; the shared menu-ownership rule applies to every stylesheet. This is a focused regression guard, not proof of visual consistency or a full CSS linter. Do not expand the legacy exemption to make a new feature pass.

Do not rebaseline `PROTOCOL-LOCK.json` or `FRONTEND-LOCK.json`. The original presentation audit still reports authorized UI changes. Report actual test commands, counts and environment limits. Deliver edited source plus both builds; do not substitute an illustrative image for working code.

## 11. Preview delivery and host limitations

Deliver actual source plus both compiled builds. `dist/hatchling-standalone.html` is a self-contained interactive **local preview**, not a hosted live-service URL. `dist/index.html` is the module build and must be served, for example with `npm run preview` from the supplied folder (Node is needed, dependency installation is not needed for serving the existing build).

`writeRouteHash()` preserves ordinary browser history but treats denied history access in opaque-origin iframe / attachment previews as nonfatal. The selected view still renders from local UI state. Do not change the parent window, sandbox attributes, CSP or production permissions to make a preview work. Back/forward URL history cannot be promised in a host that denies it. The preview fallback provides opening instructions when JavaScript is blocked; it is replaced on normal bootstrap. A file viewer that does not execute scripts cannot become a working app through CSS. Test the compiled document in a real script-enabled sandbox, not only in `set_content` at the top level.
