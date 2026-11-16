# Evidence: a file in from the app (WO-041, slice 2, the app's half)

What was tested, how, and what came back, for the app's side of giving the
agent a file: the main chat's + gives one, the chat shows how its sending
goes and says when it was put, a file holding a secret is followed by a
warning with Remove, and Settings lists the agent's inbox and removes a
file. The broker's side and the local API were [the first half](room-file-in.md).
This half changes only the app, the changelog and the work order.

| | |
|---|---|
| Base commit | `5298e142` (`feat(broker): send a file into the Room's inbox through the local API (WO-041) (#491)`) |
| Tested tree | `f0087d48d4f418c9f72b802f1d24a2112a4246d1` |
| Date | 2026-11-16 (UTC) |
| Machine | Windows 11, Node 22.17.0, the app's checks run from Git Bash |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## No change to the frozen protocol

`frontend/src/protocol/{codec,identity-document,models,rules}.ts` are byte
for byte as they were (`npm run seam:check`: "4 frozen files verified"). A
file goes to the broker through an optional live capability, `InboxPort`,
as one-time codes and the model's settings do, never as a `Command`: no
automation can send one. The chat's word that a file was put is a `notice`
the app writes, with the role the frozen types allow (`agent`), by the
inbox's own name for the file, which is plain ASCII. The owner's name for
it never enters a message's text: the chat shows it beside the notice,
inert.

## Before any code, and after it

The design was written first and reviewed independently before any code.
The review found the plan's first draft would have needed a frozen change
(a `user` notice), let a hostile file name (right-to-left marks,
lookalikes) rewrite the app's own sentence, overstated the secret warning
twice, left a "not confirmed" send no way to settle, and enforced one file
at a time in the wrong place. Each was changed in the design before code.

A second independent review of the code found two serious things and
seven smaller ones. Each was changed before the runs below:

- "Check status" and Settings' Remove drew the inbox's list before its
  read came back, so the list, "the record", could show a file that was
  gone, or not show one that was there;
- the work order linked this record before it was written;
- Remove was offered on a name the inbox never gives;
- reading the list, or removing a file, was refused in the words for
  giving one;
- a send that threw would have stalled every later one;
- a replay of many files read the list once for each;
- a file removed showed as still there after a reload;
- a part could be as large as the broker said, not the app's bound;
- the composer's gate had no test of its own.

## Runs

**The app's check** (`npm run check`): types, lint, the design check, the
action and protocol drift checks, the frozen files unchanged, formatting,
and the unit tests: exit 0.

**Coverage, from Git Bash** (under `cmd` the coverage globs match nothing
and the gates pass vacuously):

- The live adapter (`--test-coverage-include='dist/app/adapters/live/**'`,
  `tests/unit/live-*.test.mjs`): 204 tests, 0 failed, 100% of lines,
  branches and functions.
- The whole app (`dist/app/**`, `tests/unit/*.test.mjs`): 528 tests,
  0 failed, 91.04% of lines, 94.77% of branches and 90.12% of functions (its gate: 78, 88 and 81); the new modules (`session/inbox.js`,
  `adapters/live/inbox.js`, `features/chat/file-in.js`,
  `features/settings/live-inbox.js`) each 100%.

**Hand-breaks** (`fe_break.py`, `fe-break.sh`): 29 breaks, 0 survived; the tests passed before and after. Each break was
built, and a build that failed would have been counted on its own, never
as caught: none did.

The breaks:

- `the-size-isnt-bounded`: a file past 8 MiB sent.
- `any-chat-is-given-a-file`: a file given outside the main chat.
- `every-chat-takes-files`: every chat said to take a file.
- `every-chat-offers-a-file`: the composer's + offered in every chat.
- `a-part-out-of-step-is-taken`: a part answered for another place taken.
- `another-files-part-is-taken`: a part answered for another file taken.
- `the-broker-sizes-the-parts`: a part as large as the broker says.
- `an-end-never-sent-isnt-known`: an end never sent said not known, not "nothing put".
- `a-part-lost-isnt-known`: a part lost said not known, not "nothing put".
- `two-files-at-once`: a second file sent while the first goes.
- `a-thrown-send-stalls-the-queue`: a send that throws stalls every later one.
- `a-file-put-isnt-settled-by-the-stream`: a not-confirmed send never settled by the stream's word.
- `a-removed-file-isnt-marked`: a file removed here not shown as removed.
- `a-file-gone-from-the-list-isnt-removed`: a file the list no longer holds not shown as removed.
- `a-file-given-since-reads-as-removed`: a file given since the list was read shown as removed.
- `a-removal-takes-any-name`: a name the inbox never gives sent to be removed (its own test failed first; six after it failed waiting on the removal it let through).
- `a-removal-answers-before-the-list`: a removal answered before the list was read again.
- `the-list-takes-any-name`: the list takes a name the inbox never gives.
- `reads-pile-up`: each ask reads the list again.
- `every-part-redraws`: the app drawn again for every part.
- `no-secret-warning`: no warning after a file holding a secret.
- `the-owners-name-in-the-notice`: the owner's name in the chat's own text.
- `any-placed-name-in-the-notice`: any name in the chat's text, whatever its grammar.
- `the-name-isnt-isolated`: the owner's name shown without isolation or its hidden characters.
- `an-odd-name-offers-remove`: Remove offered on a name the inbox never gives.
- `remove-without-asking`: Remove in the chat without a confirm.
- `check-status-shows-the-old-list`: Check status shows the list before it is read again.
- `settings-lists-any-name`: Settings lists a name the inbox never gives.
- `settings-remove-without-asking`: Remove in Settings without a confirm.

## What wasn't tried

- **A real install, a real file, a real model.** The app's half is tested
  against the live adapter over a fake transport, and the broker's half
  against a real encrypted disk as root. Together, end to end, they are
  WO-041's acceptance check, "a real install moves a file each way with a
  real model", and come with the slices that move a file out.
- **A screen reader.** The count of parts is hidden from it and the row
  says its state once; no assistive technology was run.
- **The look of it in a browser.** The rows reuse the composer's existing
  status row and its styles; the app's browser tests run the preview, which
  has no inbox.
