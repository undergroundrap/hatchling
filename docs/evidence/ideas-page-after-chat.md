# Evidence: ideas a chat's run suggested show on the Ideas page

What was tested, how, and what came back, for a fix to the app's Ideas
page. Every number here is read from a file in the results bundle,
which is attached to the release that carries this change.

| | |
|---|---|
| Base commit | `c374e3ac` (`docs(evidence): record file writing and the last step's answer on a real install (#459)`) |
| Tested tree | `311c83c62c2ffcc69edf4512609088a0ff44aeb2` |
| Date | 2026-11-15 (UTC) |
| Machine | Windows 11, Node, from Git Bash |
| Results bundle | `hatchling-ideas-page-after-chat-results.tar.gz`, 41655 bytes, sha256 `58256c5d5c797c0ad1907df1b3a31dadb79c2375f3994e2aa5c3f8fd047c7bd9` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it. In the
bundle, this machine's paths are replaced with `<repo>` and
`<scratchpad>`; nothing else in the logs was changed.

## What was wrong

On an install of main `19ab6890`, a chat was sent an idea's own text
("curate a library of lightweight side projects"). The agent suggested
four ideas and answered that they were on the Ideas page. The broker's
log has "an idea was suggested for the Ideas page" four times, and the
run's summary counts four suggestions done. The page showed none of
them, in any of its views, until the app was loaded again; then it
showed all four.

The page read its list when the app connected, after the owner started,
set aside or brought back an idea, and when a schedule's notice said a
run had ended. A chat's run sends no such notice.

## What changed

- `frontend/src/adapters/live/ideas.ts`: `observeRun` reads the list
  again when a run's record says it suggested an idea that was kept
  (`idea_suggest` with `done` above zero). Reads asked for while one is
  under way are answered by one more read after it.
- `frontend/src/adapters/live/adapter.ts`: each run's record is passed
  to the page, beside the two things already given it.
- The changelog.

## Tests

Two (`frontend-check.log`).

- **The page** (`live-ideas.test.mjs`): a run that used something else,
  one whose idea wasn't kept, and a record that isn't a run's summary
  read nothing; a run that suggested reads the page; three such records
  at once read it twice, not three times; and one after that reads it
  again.
- **Through the adapter** (`live-adapter.test.mjs`): an activity event
  for a chat's run that suggested an idea sends one more read of the
  list, with no schedule's notice, and the page shows what came back;
  one for a run that suggested nothing sends none.

The app's own check (`npm run check`: types, lint, 494 unit tests, and
coverage of the live adapter at 100%) passed on the tested tree.

## Breaks by hand

Seven, each built and run against the two test files, which passed
before and after (`breaks/breaks.log`, `breaks/*.log`, `ip_break.py`,
`ip-break.sh`). All seven were caught.

| Break | Caught by |
|---|---|
| The adapter doesn't pass a run's record to the page | the adapter's test |
| A run whose idea wasn't kept reads the page | the page's test |
| A run that used anything reads the page | both |
| A run that suggested reads nothing | both |
| What came during a read is forgotten | the page's test |
| Every record reads at once | the page's test |
| A read is never over | the page's test |

## What wasn't tried

- **The built change on an install.** The fault was seen on one and
  confirmed by loading the app again; the fix was tested against a
  stand-in for the broker.
- **A long replay.** How many run records the app is sent when it
  connects wasn't measured; the test's burst is three.
- **A browser.** No page was opened: the tests drive the adapter.
