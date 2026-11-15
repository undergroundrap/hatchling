# Evidence: starting an idea opens a conversation of its own

What was tested, how, and what came back, for a change to what the
app does when the owner starts an idea from the Ideas page. Every
number here is read from a file in the results bundle, which is
attached to the release that carries this change.

| | |
|---|---|
| Base commit | `c3f464d1` (`feat(ideas): an idea that waits goes stale, or loses its source (#464)`) |
| Tested tree | `88a5eb4d8f818642bf7afc4226af6689cc12e960` |
| Date | 2026-11-15 (UTC) |
| Machine | Windows 11, Node, from Git Bash |
| Results bundle | `hatchling-idea-start-conversation-results.tar.gz`, 38108 bytes, sha256 `9866ad2de7b99d3b1a8d472c43d0a5558e3b881d62823f4fb54f84f6dbe5ba79` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it. In the
bundle, this machine's paths are replaced with `<repo>` and
`<scratchpad>`; nothing else in the logs was changed.

## What was wrong

"Let's do this" put an idea's text in the chat box of whatever
conversation was open. Sent from there, the agent took the idea up
with everything said before it: on an install, the first idea started
landed in the box of a conversation about something else.

## What changed

- `frontend/src/features/ideas/start.ts`, new: `ideaChat` says which
  chat an idea's text goes in, `freshConversation` starts a new
  conversation there, and `startIdea` does the two and then puts the
  text in the chat box.
- `frontend/src/features/ideas/ideas.ts`: the button calls `startIdea`,
  and the line under it says a new conversation is started.
- The changelog.

No Rust, no protocol change, and nothing in the app's frozen files.

## What it is, and isn't

- **Nothing is sent.** The text is put in the chat box. The owner
  reads it, changes it and sends it, as before: what the agent wrote
  never becomes the owner's message unread.
- **A new conversation is still the owner's alone to start.** It is
  started from the owner's press on the page, through the same port
  the chat's own button uses. It is not part of the command that marks
  an idea started, so nothing that drives the app by commands can
  reset what the agent is given.
- **The owner's own chat.** The conversation is started in the open
  chat when that is the owner's own, and in their chat with Hatchling
  when a connected app's chat is open. An archived chat is left alone.
- **When it can't be started, the owner is told** before they send
  anything, and the idea is put in the open conversation's box.

## Tests

Three new, in `tests/unit/idea-start.test.mjs` (`frontend-check.log`).

- **The chat**: the open chat, the owner's own when a connected app's
  is open or the open one isn't there, and none when that chat is
  archived, missing or a connected app's.
- **The conversation**: asked for once, of the chat named; a sentence
  for the owner when it couldn't be started, saying where the idea
  went; nothing asked with no way to start one or no chat.
- **The sequence**: the conversation first, then the text; the owner
  told, as a problem, when it couldn't be started, with the text put
  there all the same; the text alone in a preview and for an archived
  chat.

The app's own check (`npm run check`: types, lint, the frozen-file
check, 499 unit tests, and coverage of the live adapter at 100%) passed
on the tested tree.

## Breaks by hand

Eleven, each built and run against the new test file and the page's
own, which passed before and after (`breaks/breaks.log`,
`breaks/*.log`, `it_break.py`, `it-break.sh`). All eleven were caught.

| Break | Caught by |
|---|---|
| The open chat is used, whoever's it is | the chat's test; the sequence's test |
| The owner's own chat is always used | the chat's test; the sequence's test |
| The owner's own chat has another name | the chat's test; the sequence's test |
| An archived chat is given a conversation | the chat's test; the sequence's test |
| A connected app's chat named main is used | the chat's test |
| A conversation that started is said to have failed | the conversation's test; the sequence's test |
| The owner isn't told where the idea went | the conversation's test; the sequence's test |
| The text is put in the box before the conversation starts | the sequence's test |
| A conversation that couldn't start isn't said as a problem | the sequence's test |
| The idea is lost when the conversation can't start | the sequence's test |
| The conversation is started in the chat that is open | the sequence's test |

## What wasn't tried

- **The press itself.** The tests drive `startIdea` with a stand-in for
  the app. That the button calls it is one line, read and not run: no
  page was opened, and no install has this change.
- **Two ideas started without sending the first.** Each starts a new
  conversation, so the chat gets two notices in a row.
- **Something already typed in the box.** The idea's text is added
  after it, as before, now in the new conversation.
- **Done.** An idea the owner started isn't marked done when its chat
  finishes. The broker still can't tie a started idea to the run that
  answered it.
