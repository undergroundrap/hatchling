# Hatchling session protocol

**Status:** v33 (current). This doc is the human-readable reference
for the broker session wire protocol. The Rust types live in
`crates/hatchling-core/src/messages.rs` (op families). The
authoritative version history is the doc comment on
`PROTOCOL_VERSION` in `crates/hatchling-core/src/framing.rs` —
if this doc and that comment disagree, the comment wins.

**Versioning:** the protocol is versioned by op-family additions.
Versions must match exactly: a peer speaking any other version is
a protocol violation, not a negotiation. Note: v12 made
`ScheduleSpec`'s `tz` required, so a bump can tighten a
previously-loose field. Peers reject unknown variants loudly —
no silent misinterpretation.

## Transport

Sessions run over the broker's framed transport (`MAX_FRAME_BYTES`
= 64 KiB per frame, length-prefixed). Every request is a
`SessionRequest`, every response a `SessionResponse`. All types are
serde-serializable; the wire format is JSON.

Every message fits one frame. An egress body that wouldn't (a long
model conversation, or a long answer) moves in parts instead: see
v13.

A request is read strictly (WO-027). A frame that could be read two ways
ends the session unanswered, as any malformed request does:
- a key given twice in one object, compared once unescaped, so a key
  spelled with a JSON escape is the same key spelled plainly;
- a number that isn't an integer within ±(2^53 - 1);
- a lone surrogate.

A request that names a field its op doesn't have ends the session the same
way, at any depth: an op, a build input, a schedule's time or a grant. The
vault keeper reads a cell's vault channel the same way.

**A held secret ends the session.** When the agent's own text is found to
hold a secret the broker keeps (a page's address, a search, text typed into
a page, a harness's brief), or a harness it started hands one back (in its
patch, its message, its question or a file's name), the broker kills the
run's cells, its session's and its children's, with any harness or skill cell
they started, and each cell's vault channel goes with it. A browser cell
ends with its session, and a builder skill's cell ends with its op, to its
own time bound. That op is answered with its usual refusal, if
its cell is still there to read it, and the broker closes the connection: no
client has to obey anything, and no new op or result is involved. The next op
finds the session gone. The owner is told by the broker, once. A check that
can't be answered is refused and ends nothing, and so does a project's file
holding a secret. An owner invoking a harness themselves is only refused.

## Op families

| Family | Request | Response | Since |
|---|---|---|---|
| Vault | `VaultOp` | `VaultResult` | v2; denied here wherever a vault keeper runs, as on every install: a cell's vault ops go on its own vault channel |
| Egress | `EgressOp` | `EgressResult` | v3 |
| Channel | `ChannelOp` | `ChannelResult` | v4 |
| Memory | `MemoryOp` | `MemoryResult` | v1; `Propose` v18 |
| Skill | `SkillOp` | `SkillResult` | v5 |
| Goals | `GoalsOp` | `GoalsResult` | v6 |
| Query | `QueryOp` | `QueryResult` | v6 |
| Schedule | `ScheduleOp` | `ScheduleResult` | v7; `Propose` v19 |
| Hook | `HookOp` | `HookResult` | v8 |
| Artifact | `ArtifactOp` | `ArtifactResult` | v9 |
| Space | `SpaceOp` | `SpaceResult` | v10 |
| Subagent | `SubagentOp` | `SubagentResult` | v10 |
| Web | `WebOp` | `WebResult` | v14 |
| Browser | `BrowserOp` | `BrowserResult` | v15; `SignIn` v16; `Code` v17 |
| Suggest | `SuggestOp` | `SuggestResult` | v20; `needs`, `from` v27 |
| Room | `RoomOp` | `RoomResult` | v21; `Read` v22; `Search` v25; `Write`, `Edit` v26 |
| Chat | `ChatOp` | `ChatResult` | v24 |

## v9 — Artifact ops

The artifact family covers builds, delivery, and the idea lifecycle.

**`ArtifactOp` variants:** `Build`, `Deliver`, `Describe`,
`CaptureIdea`, `ListIdeas`, `NoteIdea`, `ProposeActivation`,
`Activate`, `RefineIdea`, `BuildIdea`.

- `build_kind: String` — names the builder kind (e.g.
  `"document"`, `"page"`). Carried on `Build`, `BuildIdea`, and
  `ProposeActivation`.

**`ArtifactResult` variants:** `Built`, `BuildFailed`,
`Delivered`, `Refused`, `Described`, `UnknownBuild`,
`IdeaSaved`, `Ideas`, `ActivationProposed`, `IdeaRefused`,
`Denied`, `ArtifactError`.

**Typed failures** (not strings):

- **`BuildFailure`** (7): `BadRequest`, `UnknownBuilder`,
  `CapabilityDenied`, `BuilderFailed`, `BadOutput`, `Full`,
  `UnknownKind`.
- **`DeliveryRefusal`** (7): `UnknownBuild`, `Incomplete`,
  `HashMismatch`, `Blocked`, `GateError`, `BadTarget`,
  `DeliveryFailed`.

Every failure is typed so callers can match on it. A build that
fails for a reason not in `BuildFailure` is a protocol gap, not a
string to parse.

## v10 — Space and subagent ops

**Space** covers the shared-state primitive: key-value
(`Put`/`Get`/`Remove`), task lists (`AddTask`/`RunTask`), sync
(`SyncPull`/`SyncPush`/`Conflicts`), media (`PutMedia`/
`GetMedia`), and pairing (`Pair`, `Create`, `Join`).

**Subagent** covers bounded child-agent delegation:

**`SubagentOp`:** `Spawn { task, budget, capabilities }`,
`Wait { child }`, `Kill { child, reason }`.

**`SubagentResult`:** `Spawned { child, ... }`,
`Report { ... }` (the typed result envelope),
`Refused` (typed refusal — e.g. capability widening, depth bound),
`SubagentError`.

Rules enforced at the protocol layer:

- Capabilities are a subset of the parent's grants; widening is
  refused before a cell exists.
- Budget is allocated as spend (no refunds); the broker keeps a
  monotonic ledger (max 64 steps/session, 8 children, 32
  steps/child).
- Depth is structural (max 1 — a child cannot spawn).
- A child's death/timeout surfaces as a typed result, never kills
  the parent.

Full subagent lifecycle semantics are in
`docs/activity-monitor-event-contract.md`.

## v11 — Redaction results

`ChannelResult::Redacted`: the output gate's verdict surfaced as
a typed result. When the gate redacts a channel message, the
result carries the verdict instead of the content.

## v12 — Schedule list

`ScheduleOp::List { after }` / `ScheduleResult::Listed`: the
read-only schedule listing. `ScheduleSpec.tz` is required in
the type — a schedule without a timezone is refused, not
defaulted.

## v13 — Egress bodies in parts

A message must fit one frame, so an egress body larger than about
48 KiB (once base64-encoded) moved neither way. v13 carries it in parts
of `EGRESS_PART_BYTES` (32 KiB), within the egress proxy's bounds
(1 MiB each way):

- **A request:** `EgressOp::Stage { part_b64 }` adds a part to the
  session's stage; the broker answers `EgressResult::Staged { held }`.
  Then `EgressOp::Egress` names how many bytes the body begins with in
  `staged`, and carries the rest, if any, in `body_b64`. The broker
  refuses the request (`EgressError`) if its stage holds a different
  count, and empties the stage either way.
- **An answer:** when the whole `EgressResult::Ok` wouldn't fit a
  frame, the broker holds the body and answers with the status, the
  headers, an empty `body_b64` and `more`, the bytes it holds.
  `EgressOp::Read` returns `EgressResult::Part { body_b64, more }`, the
  next part and what's left after it, until `more` is 0. A new request
  lets a held answer go.

A body that fits goes whole, as before, with `staged` and `more` at 0.
A session holds one staged request and one answer at most.

## v14 — Web

A page read through the broker, from a site the owner allows (WO-028).

- **`WebOp::Fetch { url, reason }`:** the page's absolute `https` URL, and
  why the agent wants it (at most 500 characters), shown to the owner as
  the agent's words.
- **`WebOp::Search { query }`:** a web search through the provider the
  operator chose (at most 400 characters); the broker holds the key.
- **`WebResult`:**
  - `Page { url, status, text, truncated }`: the page as text, the site's
    words;
  - `Results { results }`: a search's results, at most 10, each a
    `WebHit { title, url, snippet }`, the provider's words;
  - `Redirect { to }`: a redirect to another site, not followed;
  - `Waiting { site, request }`: the owner has been asked and hasn't
    decided; fetch again later;
  - `Denied { site }`, `Expired { site }`: the owner said no, or didn't
    decide in time;
  - `Unasked { reason }`: the site couldn't be put to the owner
    (`ApprovalRefusal`);
  - `Refused { reason }`: the URL, the operator's refusal of the site, or
    the read itself;
  - `Off`: the operator hasn't turned this on (pages, or search).

## v15 — Browser

A real browser the agent drives, in a cell of its own, through the broker
(WO-029). Every session is strict: the browser reaches only the sites the
session holds.

- **`BrowserOp::Open { url, reason }`:** open the page, starting the
  session's browser if there is none. Its site is allowed as a
  `WebOp::Fetch`'s is, and a site allowed for one is allowed for the
  other. A site allowed once is held for that visit: when the browser opens
  another site, it's revoked, and its connections closed.
- **`BrowserOp::Read`:** the page as it is now.
- **`BrowserOp::Act { element, action }`:** act on an element of the last
  snapshot (`PageAction`: click, type, a key, a choice). Text to type that
  holds a secret the broker keeps is refused, and never reaches the page.
- **`BrowserOp::Allow { site, reason }`:** ask the owner to allow a site the
  browser was refused, one of the last page's `refused`, which the page
  needs. It's put to the owner as a page's site is, and a site allowed once
  is held for the visit.
- **`BrowserOp::Scroll { down }`**, **`BrowserOp::Close`**: scroll, or close
  the browser and end its cell.
- **`BrowserResult`:**
  - `Page { snapshot, refused }`: the page's text and the elements it can
    be acted on through (`PageSnapshot`), the page's words; and the sites
    the session doesn't hold that the browser was refused;
  - `Waiting`, `Denied`, `Expired`, `Unasked`: as for `WebResult`;
  - `Failed { failure }`: the browser couldn't do it (`BrowserFailure`);
  - `Refused { reason }`: refused before the browser: the URL, the
    operator's refusal of the site, typed text holding a secret, or a
    browser that couldn't start;
  - `Allowed { site }`: the site is allowed; open the page again to load
    what it needed;
  - `Closed`: the browser is closed;
  - `Stopped`: the owner stopped the browser, from the app; it stays
    stopped for the session;
  - `OwnerDriving`: the owner is driving the browser, or taking it, from
    the app; nothing was done;
  - `HandedBack { snapshot, refused }`: the owner drove the browser and
    handed it back; what was asked wasn't done, and this is the page now;
  - `Off`: the operator hasn't set up a browser.

## v16 — The browser's sign-in

- **`BrowserOp::SignIn { username, password, label }`** (WO-030): sign in on
  the page with a sign-in the owner keeps for its site, naming the fields
  by their ids from the last snapshot. Only in a browser running a
  signed-in profile, and only on the exact origin the sign-in was kept for.
  The vault keeper sends it to the browser's controller, which types it,
  and the broker never holds it; the agent learns only the page after, which shows
  none of it, or why it wasn't done: no sign-in for this site, which one
  (by `label`) when there are several, or `Failed { failure: OtherOrigin }`
  when the page isn't on the sign-in's origin. A username goes only into a
  text or email field, a password only into a password field.

## v17 — The browser's one-time code

- **`BrowserOp::Code { element }`** (WO-030): the page asks for a one-time
  code. In a browser running a signed-in profile, the owner is asked in the
  app, on the page's origin; once they give it, the broker types it into
  the field named, through the same exact-origin fill as a sign-in, and
  forgets it. The agent never sees it. Asked again while the owner hasn't
  answered, the op waits for them a while.
- **`BrowserResult`** gains `CodeWaiting { origin }` (asked, not answered
  yet: ask again shortly), `CodeDeclined` and `CodeExpired`. Filled, the
  reply is the page after, the code hidden.

## v18 — The agent's memory proposal

- **`MemoryOp::Propose { content }`**: the agent asks the owner to keep
  something about them in their memory. It's put to them in the app, the
  content shown as the agent's words; nothing is written unless they
  approve. Then the broker keeps it in the user scope, tagged `recall`, so
  later runs are given it. The agent never writes there itself: proposing
  needs the grant `user:propose`, which isn't `user:write`.
- **`MemoryResult`** gains `Proposed` (asked) and `Unasked { reason }`
  (not asked: the owner said no to the same proposal, or their requests
  are paused or full).

## v19 — The agent's schedule proposal

- **`ScheduleOp::Propose { timing, value, task }`**: the agent asks the
  owner to schedule work, in the vocabulary of `hatchling-broker schedule
  add` (`at`, `every`, `daily`, `weekdays`, `cron`), on the owner's clock.
  It's put to them in the app: when it runs and what it may do in the
  broker's words, the task as the agent's. Nothing is scheduled unless they
  approve. Then the broker creates it as theirs, with the asking session's
  grants, delivering to the chat. Proposing needs `channel-send:local`.
- **`ScheduleResult`** gains `Proposed { when }` (asked, the timing in
  words) and `Unasked { reason }` (not asked: the timing doesn't read, the
  owner said no to the same proposal, or their requests are paused or
  full).

## v20 — The agent's suggestions

- **`SessionRequest::Suggest(SuggestOp::Suggest { title, summary, why, area })`**:
  the agent suggests something Hatchling could do for the owner, for their
  Ideas page. `area` is one of `career`, `devices`, `creation`, `family`,
  `finance`, `health`, `learning`, `productivity`, `relationships`,
  `shopping`. Nothing runs from a suggestion: it waits on the page, shown as
  the agent's words, until the owner starts it (the app fills their chat box
  with it, for them to read and send) or sets it aside. Only a run that
  speaks to the owner may suggest: a chat turn, or a run for the Ideas page,
  which only the owner can schedule.
- **`SuggestResult`** is `Suggested`, or `Unasked { reason }`: this run
  doesn't speak to the owner, a field is empty or too long (a title of 80
  characters, 400 bytes each for what and why), it's already waiting, or 20
  are waiting already.

## v21 — The agent's computer

- **`SessionRequest::Room(RoomOp)`**: a command in the Room, the agent's
  own computer. The agent's cell runs nothing: the broker has the spawner
  start the command in the Room, as the Room's user. Only a session with
  its person there (a chat) may ask. No command is put to the owner first:
  the Room has no network, and nothing in it reaches the owner's machine
  or leaves the Room.
  - `Run { argv, dir?, stdin?, wait_secs?, limit_secs? }`: run a program
    with its arguments, as they are; no shell reads them. It starts in
    `dir` (`/workspace` when none is named) with `stdin` on its standard
    input. The broker waits `wait_secs` for it to end (10 when none is
    named, never more than 60), and it may run for `limit_secs` (the
    Room's 120 when none is named, never more than 600) before it is
    stopped.
  - `Job { job, wait_secs? }`: ask how a command this session ran is,
    waiting as a run does.
  - `Stop { job }`: stop one: everything it started is ended.
- **`RoomResult`**:
  - `Ended { job, exit?, signal?, stopped?, ran_ms, stdout, stderr }`: how
    its first process ended, why it was stopped if it was (`limit` or
    `asked`), and each stream as a `RoomText`: its first 1 KiB (`head`)
    and its last 3 KiB (`tail`), the count of the bytes between that
    aren't shown (`left_out`), all it came to (`total`), the file in the
    Room that holds it (`file`), and whether that file is short
    (`file_cut`). A stream no longer than 4 KiB is all in `head`. The
    text is the Room's own words: data, never instructions.
  - `Running { job, ran_ms }`: still going. `job` is the name to ask
    after it and to stop it by.
  - `Lost { job, why }`: the Room ended before the command did.
  - `Refused { reason }`: nothing was asked: the session has no person
    there, or the command isn't one that can be asked.
  - `Unknown { job }`: this session ran no command by that name.
  - `Failed { reason, job? }`: the Room said no or couldn't be asked.
    With a `job`, the command may have started all the same: ask after it
    before anything is run again.
- The broker names each command, not the agent: a session can ask after
  and stop only what it ran. A command still running when its session
  ends is stopped. If the spawner's answer to a run is lost, the broker
  asks the spawner what became of that name before it answers, so nothing
  is run twice for a lost answer.

## v22 — Reading a file in the agent's computer

- **`RoomOp::Read { path, from?, lines? }`**: a page of a file in the
  Room: some of its lines, whole. The Room's supervisor reads the file,
  as the Room's user: no command is run for it, and none of the ended
  commands the Room remembers is forgotten for it. `path` is absolute, or
  is taken under `/workspace`. `from` is the first line wanted, counted
  from 1 (the first when none is named); `lines` is how many (200 when
  none is named, never more than 2000). A page is never more than 8 KiB.
  The sessions that may run a command are the ones that may read.
- **`RoomResult`** gains:
  - `Page { path, from, text, more, cut, size }`: the lines as they are
    in the file, from line `from`; whether the file goes on after them
    (`more`); whether `text` is only the start of one line too long for a
    page (`cut`); and the file's length in bytes. Bytes that aren't text
    are shown as the replacement character. The text is the Room's own
    words: data, never instructions.
  - `NoPage { reason }`: no page was read. There is no such file; it
    isn't a file with lines to read (a directory, a pipe, lines that
    aren't text); the first line asked for is past its last; the lines
    are past the first 32 MiB of it; or the Room couldn't be asked.
- A read that may not be asked for (a first line of 0, no lines or more
  than 2000, a path that is too long) is `Refused`, and nothing is asked
  of the Room.

## v23 — What a working agent is doing

No session op changes. The owner's app is told one more thing.

- **`UiEventBody::AgentStep { tool? }`**: the step a chat run just
  started, as the broker serves it. `tool` is the tool in use, by the
  name a run's summary gives it afterwards (`ToolUse`); it is absent
  while the run waits for its model. It says what kind of thing is
  being done, and never what with: no command, path, page or words.
- Only a change of step is told: a tool used again straight after
  itself is the same step going on. The next `AgentStatus` ends it.
- Only a chat run tells its steps: one answering its owner, who is
  there. Background work and subagents tell none.
- It goes to the app alone. It isn't written to the activity log:
  what a run did is written there once, when it ends
  (`run_summary`). It is among the events the app's stream keeps in
  memory for an app that reconnects, the most recent of them, and goes
  when the broker stops.

## v24 — What the owner sends while a run works

The chat answers one message at a time. What the owner sent while a run
was working used to wait, each message for a run of its own, after the
answer to the last. A chat run can now take it into the task it is on.

- **`ChatOp::More`**: what the owner has sent since the message the run
  was started for, in the same conversation. It has no fields.
- **`ChatResult::More { messages }`**: those messages, oldest first,
  each as the owner wrote it. Empty when they sent none.
- **Who is given anything.** Only the chat's run in flight: the chat
  names it to the broker by its cell when it starts it, and forgets it
  when it ends. Any other session (a scheduled run, a subagent, a chat
  run that has ended) is answered with no messages, and learns no more
  than that. No grant is asked for it: the run already holds the
  owner's message as its task, and these are more of the same.
- **What being given means.** The messages are taken off the queue, so
  no run is started for them, and they aren't given again. When the run
  ends and its answer is shown, the chat keeps them with the run's own
  message as what the owner said in that exchange. A run that fails
  takes what it was given with it, as it does its own message.
- **Which messages.** Only ones sent in the conversation the run's own
  message was sent in. One sent after the owner started a new
  conversation stays where it is, and so does everything behind it.
- **How much at once.** A reply carries at most 48 KiB of messages, as
  JSON writes them (`CHAT_MORE_BYTES`), so it always fits a frame. A
  message that doesn't fit in what is left waits for the next asking.
- **When a run asks.** Before each turn of its model, and only then: a
  message that arrives while the model writes its answer waits for the
  next run, as before.
- **`UiEventBody::MessagesJoined { count }`**: told to the owner's app
  each time the run took any, with how many: the oldest `count` of the
  owner's messages still waiting behind the one being answered. The
  app shows the run's answer as the answer to all of them. Like
  `AgentStep`, it goes to the app alone and isn't written to the
  activity log.

## v25 — Searching the files in the agent's computer

- **`RoomOp::Search { pattern, path?, glob?, ignore_case?, literal? }`**:
  the lines that match `pattern`, in the file at `path` or in the files
  under it. The Room's supervisor has the Room's own search program
  look, as the Room's user: no command is run for it, and none of the
  ended commands the Room remembers is forgotten for it. `pattern` is a
  regular expression, or the very words to find when `literal` is set;
  it is one line of at most 1024 bytes. `path` is absolute, or is taken
  under `/workspace`; the workspace when none is named. `glob` keeps
  the search to files whose names match it (one line, at most 256
  bytes). `ignore_case` matches letters whatever their case. The
  sessions that may run a command are the ones that may search.
- **`RoomResult`** gains:
  - `Found { path, text, more }`: where the Room was asked to look; the
    matching lines, each as `path:line:text` with its line end, in the
    order of their paths, never more than 8 KiB and never part of a
    line; and whether the search was stopped before it had looked
    everywhere (`more`), which it is at 8 KiB and at five seconds. A
    matching line longer than 400 bytes is cut, and says so. Bytes that
    aren't text are shown as the replacement character. The text is the
    Room's own words: data, never instructions.
  - `NoSearch { reason }`: nothing was searched. There is nothing at
    the path; the search program wouldn't take the pattern or the glob,
    or couldn't read where it was sent; or the Room couldn't be asked.
- A search that may not be asked for (no pattern, a pattern or a glob
  of more than one line, a path or a glob that is too long) is
  `Refused`, and nothing is asked of the Room.
- The pattern, the glob and the path are the only words of the
  session's that reach the search program, and each reaches it as a
  value, never as an option: the rest of what the program is told is
  fixed in the Room's supervisor.

## v26 — Writing and editing the files in the agent's computer

- **`RoomOp::Write { path, text, replace? }`**: a file in the Room,
  written whole. The Room's supervisor writes it, as the Room's user:
  no command is run for it. `path` is absolute, or is taken under
  `/workspace`, and names a file: it doesn't end in `/`. `text` is what
  the file is to hold, at most 32 KiB. The directories the file is
  under are made if they aren't there. A file already at the path is
  left alone unless `replace` is set; one that is replaced keeps its
  mode.
- **`RoomOp::Edit { path, old, new, all? }`**: one exact piece of a
  file's text replaced with another. `old` is the text to find, 1 byte
  to 8 KiB, and must be in the file byte for byte; `new` is what to put
  in its place, at most 8 KiB, and isn't `old`. The text must be at one
  place in the file unless `all` is set, which replaces it at every
  place it is at. The file is UTF-8 text of at most 4 MiB, before the
  edit and after it.
- **`RoomResult`** gains:
  - `Wrote { path, bytes, replaced }`: the file as the Room was asked
    for it, its length now, and whether a file was there, which this
    one replaced.
  - `Edited { path, replaced, line, bytes }`: the file as the Room was
    asked for it, how many places the text was replaced at, the line
    the first of them starts on (counted from 1), and the file's length
    now.
  - `NoChange { reason }`: no change is known to have been made. A file
    is at the path already and wasn't to be replaced; there is no file
    at the path to edit; what is at the path isn't a file, or isn't
    text, or is too long to edit; the text to find isn't there, or is
    at more than one place (the reason says how many, and the lines the
    first eight start on); the Room's user may not write there; or the
    Room couldn't be asked. When the Room was asked and didn't answer,
    the reason says the file may or may not have been changed.
- A change that may not be asked for (a path that is too long or names
  a directory, text over its limit, an edit with nothing to find or
  with `new` the same as `old`) is `Refused`, and nothing is asked of
  the Room.
- A file is never changed where it lies: what it is to hold is written
  beside it and given its name in one step. A link at the path is
  never followed.
- A change is no command: it takes no job name. The sessions that may
  run a command are the ones that may write and edit.

## v27 — An idea says what it needs and where it comes from

- **`SuggestOp::Suggest`** gains two fields, both required:
  - `needs`: the tool groups doing the idea would use, each once, from
    `web`, `search`, `browser`, `skills`, `goals`, `memory`, `schedule`,
    `suggest`, `room`; an empty list when it needs none.
  - `from`: where the idea comes from, as `SuggestedFrom`. The one source
    there is yet is `{ "goal": "<id>" }`: one of the owner's open goals, by
    its id. What the agent remembers of the owner is no source: the broker
    has nothing to look a remembered thing up by.
- The broker keeps an idea only if it can check both. Each group in
  `needs` is one this install has, whoever asks: the idea is started by
  the owner in a chat of their own, so it is held to the install's
  settings and to no run's grants. And the goal in `from` is found, open,
  read under the asking run's own grant to read the owner's goals: a run
  without that grant can't suggest from a goal.
- **`SuggestResult::Unasked { reason }`** gains reasons: the install lacks
  something the idea needs (the reason names each thing), a need is named
  twice, the goal isn't there, is closed, may not be read by this run or
  couldn't be read, and the run has put three ideas on the page already.
  Three is what one run may add, whatever became of them since.
- The kept idea (`Suggestion`, in the app's protocol) carries `needs` as
  the agent gave them, `source` as the broker found it (the goal's id and
  its title then, never the agent's words for it) and `run`, the
  suggesting run as the owner's Activity names it. One kept before v27
  has none of the three. The page is listed as it is kept, the newest
  first: an idea with a source is before any kept before v27.
- The broker checks what an idea says it needs, not what doing it will
  turn out to take.

## v28 — An idea that waits doesn't wait for ever

No session op changes. The version is raised for the owner's app, which
reads a kept idea's state.

- **`SuggestionState`** (the app's protocol) gains two states, both the
  broker's findings and never the agent's:
  - `stale`: the idea waited more than fourteen days, from when it was
    suggested or last brought back.
  - `source_gone`: the goal it came from is closed or no longer there.
- Both are found among ideas still `suggested`, when the owner's app
  lists the page; an idea the owner started or set aside isn't judged.
  A stale idea is also found when a run suggests. A lost source is
  found only for the owner, whose goals the broker reads as their own
  goals page does: a run may not ask after all of them. With the goals
  unreadable, no idea is retired for its source.
- What is found is written back, so a retired idea no longer counts
  among those waiting. Sixty may wait (twenty before v28). A run still
  adds at most three.
- `Restore` brings a stale idea back as `suggested`, waiting from then:
  `suggested_at` is the time it was suggested or last brought back. An
  idea whose source is gone is answered as it stands while its goal is
  closed, and comes back once the goal is open again.
- The agent has no op that retires, sets aside or removes an idea.

## v29 — A started idea says when it was answered

No session op changes. The version is raised for the owner's app, which
names an idea as it starts a conversation and reads a kept idea's state.

- **`UiAction::NewConversation`** (the app's protocol) gains an optional
  `idea`: the id of the idea on the owner's Ideas page the conversation
  is for. The broker keeps the conversation as that idea's only when the
  idea is on the page and `started`; for any other id the conversation
  starts all the same, for nothing. Which conversation is an idea's is
  kept in memory alone, until the next conversation starts: after a
  broker restart no conversation is any idea's.
- **`SuggestionState`** gains `answered`, the broker's finding and never
  the agent's: a run answered the owner in the conversation they started
  for the idea. It is found when the answer was shown to them, in that
  conversation while it was still the open one, by a run that had steps
  to spare, and the idea was still `started`. The broker knows an answer
  was shown, not that the idea was carried out, and not that the message
  answered was the idea's: any answer in the idea's conversation marks
  it.
- **An agent's exit status** says when its steps ran out as it answered:
  `80` (`run_failure::ANSWERED_OUT_OF_STEPS`), when it answered on its
  last step, told it was the last, or with an answer held back until no
  step was left. Its answer is on its standard output as any answer is,
  and the broker delivers it as it delivers any. A subagent never exits
  with it. It is the agent's word, read from the kernel like every exit
  status: the broker uses it only to leave an idea `started`.
- `Start`, `Dismiss` and `Restore` act on an answered idea as on any
  other. The agent has no op that marks an idea answered.

## v30 — Keep an unfinished chat for later

No session op changes. The version is raised for the owner's app,
which reads a chat answer's ending and keeps an ask for later.

- **`ChatMessage`** (the app's protocol) gains `out_of_steps`, written
  only when true: the run that wrote the answer ran out of steps as it
  answered, by its agent's exit status (`ANSWERED_OUT_OF_STEPS`, v29).
  Said only of an answer in the owner's own chat; a scheduled run's
  answer never says it.
- When such an answer was shown to the owner, in the conversation still
  open, the broker **offers** what they said in that exchange, trimmed
  and at most 400 bytes, for them to keep. The offer is held in memory
  alone, by the local channel's hub, for the conversation open: a new
  conversation, or a broker restart, withdraws it. At most eight are
  held, the oldest going first.
- **`SuggestionOp::Keep { run }`** keeps the offer for `run` as a card
  on the Ideas page, and withdraws it. Owner-only, as every
  `manage_suggestions` op is. `not_found` when nothing is offered for
  that run.
- The card is the owner's own words and the broker's: its title and
  why are the broker's fixed text, what it would do is what the owner
  asked, and its source is **`SuggestionSource::Chat`**. No model
  writes it, and the agent has no op that keeps one. It waits as any
  card does and goes stale after fourteen days; it has no goal to
  lose. The cap on waiting ideas is the agent's: what the owner keeps
  isn't refused for it, and the page still holds at most 100.

## v31 — Where a kept chat's run said it stopped

No session op changes. The version is raised for the owner's app,
which shows a card kept from a chat with its run's own account.

- **`Suggestion`** (the app's protocol) gains `run_said`, written only
  when there is one: of a card the owner kept from a chat, the last
  answer of the run that ran out of steps, as the agent wrote it,
  trimmed and at most 400 bytes as the owner's words are. The broker
  holds it with the offer, from the answer the owner was shown, and
  keeps it on the card when they keep it. No new model call makes it.
- It is the agent's claim, kept apart from the owner's words and never
  checked: a card can't tell a true account from a false one. The app
  says whose words it is. No run is given it: a run started from the
  card is given the owner's words, and finds for itself where things
  stand.
- A run that answered with nothing leaves a card without it, and so is
  every card a run suggested.

## v32 — Giving the owner a file

- **`RoomOp::HandOver { path }`**: a file in the agent's computer given
  to the owner (WO-041). `path` is under `/workspace`, absolute or taken
  under it, and names a file: not a directory, a link at any step, a
  pipe, or anything on another file system, the inbox's mount among
  them. The Room's supervisor reads it, as the Room's user, a piece at a
  time; the spawner puts it together, at most 8 MiB, and holds it to
  being one file from its first piece to its last.
- Only a chat's own run gives a file, in the conversation open now: a
  session with nobody there, one the broker didn't start as a run, one
  the chat didn't start, and one started before the owner's new
  conversation are each refused before anything is read. It is offered
  to the model (the `files` tool group, `give_file`) only in a chat with
  its computer, where the operator gave the broker a Library
  (`HATCHLING_LIBRARY_DIR`).
- **The broker's name, never the agent's**: the file's last component in
  ASCII letters, digits, `.`, `_` and `-`, at most 64 bytes, no leading
  dot; refused when what is before its first dot is a name Windows keeps
  for a device (`CON`, `PRN`, `AUX`, `NUL`, `COM0` to `COM9`, `LPT0` to
  `LPT9`), or it ends in a dot.
- **Checked, the file and its name**, for the secrets a gate scans for
  and the keeper's whole set (the vault's access log has a
  `file-out-gate` scan and a `file-out-check` line). A secret found
  withholds it, never named, and turns giving files off for the
  conversation until the owner starts a new one; the run goes on. A
  check that couldn't be made gives nothing and turns nothing off.
- **`RoomResult`** gains:
  - `HandedOver { name, bytes }`: kept in the Library, and offered to
    the owner by that name.
  - `NotHandedOver { reason }`: nothing was given, and why, in words for
    the agent: refused, withheld, not checked, too large, or a Library or
    a conversation that holds as many as it may.

## v33 — Giving the owner a folder

- **`RoomOp::HandOverFolder { path }`**: a folder in the agent's
  computer given to the owner as one item (WO-043), offered to the model
  as `give_folder` where `give_file` is. `path` is `/workspace` itself
  or a folder under it, absolute or taken under it, with no empty, `.`
  or `..` step. Refused before anything is read as a file is, and when
  the folder's own name can't be one.
- **Walked first**, by the Room's listings, a folder at a time, before
  anything is spent or read: at most 256 folders, its own among them,
  512 files, 8 MiB a file, 32 MiB in all, 16 steps deep, and each path,
  with the folder's own, at most 1024 bytes. The walk and the reads have
  5 minutes, checked before each ask of the Room, so the last ask can run
  past them by its own time. A walk refused spends nothing, as a Room op
  refused doesn't. `inbox` at the workspace, and a folder given in it,
  are never walked. Links, other mounts (the workspace's inbox, by its name
  too) and devices, pipes and sockets are left out and counted, never
  opened. A name that isn't printable ASCII without `< > : " \ | ? *`,
  starts or ends with a space, ends with a dot, is a name Windows keeps
  for a device (those above, and the console's `CONIN$` and `CONOUT$`, and
  `CLOCK$`) or acts on when its folder is shown, at any depth, or is
  another's in its folder but for case, refuses the folder; so does a
  folder that isn't the one its parent listed. The words never name the
  path.
- **Spent as walked**, a hand-over and its listed bytes, before any file
  is read; then each file is read as a file is, and held at once to its
  listed length: one that isn't refuses the folder, spent.
- **Checked as one**: its name alone, then its paths and every file's
  bytes back to back in one text, through both checks (a scan and a
  check line each). A secret anywhere withholds all of it, as a file's,
  and turns giving off for the conversation.
- **`RoomResult`** gains `FolderHandedOver { name, files, bytes,
  left_out }`, with `left_out` (`LeftOut`) counting `links`, `mounts`
  and `others`; a refusal is `NotHandedOver { reason }`.

## Device handshake

`DeviceHello` / `DeviceWelcome` / `DeviceRefusal` — the pairing
handshake for new devices. Typed refusal, not a string.

## For the frontend

The frontend's generated types must cover every variant listed
here; the drift test fails if the schema and these types
diverge. When a new op family lands, this doc is updated in the
same commit.
