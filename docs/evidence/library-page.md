# Evidence: the agent's files on the Library page (WO-043, #523)

What was tested, how, and what came back, for moving the files and folders
the agent gave from a Settings list to the Library page. Found in WO-043's
acceptance on a real install.

| | |
|---|---|
| Base commit | `56a7a3a2` (`fix(broker): say a Library limit is reached only when it is (WO-043) (#524)`) |
| Tested tree | `96ac37d85bf5010cb144fd1d151610dd1ee7d405` |
| Date | 2026-11-19 (UTC) |
| Machine | Windows 11, Node 22, the app's checks run from Git Bash |

This record was added after the runs, in its own commit. It changes no code:
the tested tree is the tree of the commit before it.

## What was wrong

On a real install, with the release build of `c9451f5e`, the list lived only
in Settings, Data. That dialog redraws when its own sections change, not when
the app's state does, so:

- ticking items never showed "Download the N chosen" until the owner switched
  sections;
- while a `.zip` was made, no progress showed, and the button stayed, so a
  second click started another;
- "Clear the choice" left the boxes ticked;
- the list stayed stale after a removal, and after new items.

Meanwhile the main-menu Library page showed only the build store, and on a
live install said "Hatchling can't make files you can download yet". That
has been false since WO-041.

## What changed

- **The Library page holds them** on a live install, under "All", in the
  demo's own card (`library-card.ts`, extracted from the demo's
  `artifactCard`, which now uses it too). The cards come in grid or list,
  grouped by conversation and by reply.
  - **Each card** shows its type badge from the name's extension, its whole
    name (wrapped, never cut short), its size and when it was kept.
  - **A program, a document with macros or a page that can run script** says
    so on the card.
  - **A folder** shows its count of files and what its walk left out.
  - **Each card's menu** offers Download (`.zip` for a folder) and Remove.
- **Opening a card** shows what Hatchling knows of the item, in a dialog with a
  fixed title: its name as text, size, when it was kept, the full caution,
  its SHA-256 (a folder's labelled as its list's), Download and Remove. Never
  what it holds. The dialog closes when its item leaves the Library.
- **Choosing is the page's Select mode**: the Select button, a press on a card
  (`aria-pressed`), and the demo's selection bar with "Download as one .zip"
  (one: "Download"), Remove and Done. Choosing ends where its cards aren't
  shown. The port gains `clearChosen()`, one change for all.
- **A `.zip`'s progress is counted in place.** The adapter publishes as a
  `.zip` starts and ends, never once a file; the page's line reads the count
  itself, hidden from screen readers so it isn't read out at each file. A
  redraw no longer happens five times a second during a download, so it
  can't take a press away.
- **Settings, Data** shows a one-line summary of the Library, updated while
  it's on screen, and "Open the Library".
- Docs: CHANGELOG (Fixed, and the unreleased WO-043 entry); `local-api.md`.

## The design, reviewed

Three independent reviews, recorded in the private proposal:

1. **Of the first plan.** It found that redrawing a route page on every
   publish drops presses, and that the Settings row would go stale.
2. **Of the revised plan.** It found that keeping a cached section doesn't
   keep a press or focus, that progress publishes would recreate the
   problem, and that the demo's card cuts long names short. Answered by
   removing the per-file publishes, by stable ids for focus, and by a card
   variant that wraps names.
3. **Of the code.** Nothing blocking. It raised nine items, all fixed:
   - Remove's confirm names the item again;
   - each card's menu and buttons are named for its item;
   - the preview button has an id, so focus survives choosing;
   - Select and Done hand focus to each other, and Done draws once;
   - choosing ends where its cards aren't shown;
   - the dialog closes when its item is gone, and says when a download is
     already under way;
   - an item Hatchling can't name has no dead button or empty menu;
   - where the copies come from is said again;
   - a single file's "Downloading…" is said on its card.

## Tests

- **New:** `tests/unit/library-page.test.mjs`, 11 tests.
  - Cards, groups, the details dialog, removal (and a failing one), Select
    mode and its bar, a reply's items together, progress in place.
  - The Library as read, unread, failed (with a retry) and empty (with the way
    to the chat), and items it can't name.
  - The full and spent words, where the copies come from, choosing ending,
    and Settings' summary and link.
- **Moved:** the Settings list's tests left `file-out.test.mjs`. The chat's
  cards keep theirs.
- **Changed in the adapter's tests:** a `.zip`, of files or of a folder,
  publishes as it starts and ends, and its count goes on in place between;
  `clearChosen()` is one change.

## Hand-breaks

38 breaks, each in the change's code, each with the tests that must fail
named.

- **First run:** 37 caught. `summary-not-items` survived, a real gap: the
  test's sample Library had the same number of items as of hand-overs given,
  so swapping one for the other changed nothing. The test now gives them
  different numbers, and the break is caught.
- **Rerun of that break:** caught. **38 caught, 0 survived**, the tests green
  before and after.

| Group | Breaks |
|---|---|
| What a card shows | names unfiltered, the program word unsaid, the badge not from the name, the layout ignored, the reply heading gone |
| Opening one | the folder hash unlabelled, Download while busy, the dialog staying when its item is gone, Remove unnamed, one menu label for all, the preview id gone, busy unsaid, removal going on after a failure |
| Choosing | choosing that opens, the chosen in their own order, removed items still chosen, Done keeping the chosen, the header while choosing, Download all for one item, unpressed, "Open" while choosing, choosing not ended, ended but still chosen |
| A reply's items | offered while choosing, not named by its run |
| Progress | read out at each file, unsaid, a publish each file, `clearChosen` unpublished |
| As read | the retry reading nothing, nothing yet leading nowhere, unnamed always removable, unnamed opening, an empty menu, full forgetting its files, provenance unsaid |
| Settings | the summary of the wrong count, the link to another filter |

## The app's checks

On the tested tree, from Git Bash (so the coverage gates are real):

`npm run check` exits 0. That covers typecheck, lint (170 modules), the
design check, the actions, protocol and seam checks, the format check, 597
unit tests, the live adapter's tests (257, at 100% of lines, branches and
functions) and the operator's (16, at 100%).

## Found after: still drawn per file

On the release build ([folders-acceptance.md](folders-acceptance.md)), the page was still drawn once per file
during a `.zip`: each file read is an API call, and each call puts an Activity event on the stream, which
draws the page again. So the progress line counts without drawing, as above, but the page doesn't stay still.
A click on another card landed during one there. Drawing at most once a frame is
[#528](https://github.com/undergroundrap/hatchling/issues/528).

## Not here

A press during a `.zip` in a real browser, and the page against a real
Library, are checked on the release build in WO-043's acceptance, which
follows.
