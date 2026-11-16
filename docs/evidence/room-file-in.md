# Evidence: a file in through the local API (WO-041, slice 2)

What was tested, how, and what came back, for the second slice of files
between the owner and the agent's computer. A client of the local API
sends a file in parts; the broker holds it whole, names it, and has the
spawner put it in the Room's inbox, as root. The owner lists the inbox
and removes a file through the spawner too. The next run that answers
the owner is told of the file in the broker's words, and Activity keeps
one line a file. The app's own attach comes in the next pull request.
Every number here is read from a file in the results bundle, which is
attached to the release that carries this change.

| | |
|---|---|
| Base commit | `d5a68385` (`feat(sys): put a file in the Room's inbox, as root (WO-041) (#490)`) |
| Tested tree | `5ed92d7fc8af14d8bf982adce3868d9203a5eb81` |
| Date | 2026-11-16 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 threads, in a sandbox; the installed Room's tests as root outside it; the app's checks on Windows, Node 22.17.0 |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned); cargo-mutants 27.1.0 |
| Room supervisor | built from the tested tree by `scripts/room-image-supervisor.sh`, sha256 `1cf11e45770ab7bb520bd9dc8b120a025a394025d7747f346f7f52cedac6e80b` |
| Results bundle | `hatchling-room-file-in-results.tar.gz`, 5877019 bytes, sha256 `255a284a60b84827c0938710588d81b49938b70707bf5d613efbe09adf7c6e4b` |

This record was added after the runs, in its own commit, with the work
order's link to it. It changes no code: the tested tree is the tree of
the commit before it.

## Before the runs

WO-041's design came from an independent threat model before slice 1. A
second independent review of this slice's diff found nothing serious:
nothing let root write, list or remove outside the inbox, and the owner's
gate held. It found ten smaller things, and each was changed before the
runs below:

- the version bump left four clients and the docs' example saying 4, and
  a v5 broker refuses them;
- the model's line was put in the owner's message, so it was kept and
  replayed as the owner's words, and lines that no longer fit one message
  waited forever;
- the line called only the file's contents untrusted, while its name,
  made of the owner's, reached the model too;
- a file that died part way, or with its connection, left no line in
  Activity;
- one stalled upload held the one-at-a-time turn until its connection
  closed;
- a failed check for a held secret was shown as a hit, which still holds
  and is now said;
- the spawner's words, errno text among them, reached the app;
- a name the spawner gave wasn't checked before the model was told it;
- the stand-in tests left four guards no test could fail.

## Runs

**Quick** (`quick.log`): `cargo fmt --check`, then on each toolchain
`cargo clippy --workspace --all-targets --all-features -- -D warnings`
and the touched crates' tests. Every step exited 0: the inbox's tests
(18), the hub's (36), the UI protocol's (49) and the broker's file in,
API, chat run, spawner, Room session and room disk tests (298).

**The installed Room, as root** (`root/final.log`): on a real encrypted
room disk, with the supervisor above.

| Test | Result |
|---|---|
| `the_spawner_prepares_layout_only_on_the_verified_room_disk` | ok |
| `the_installed_spawner_hands_the_verified_image_to_its_mount_helper` | ok |
| `an_installed_room_ends_when_its_spawner_is_killed` | ok |

What the first two now hold, beyond slice 1's:

- **With no Room live**, the inbox lists its files by number with their
  sizes, and a file is removed by its name alone. A name that walks out
  of the inbox (`../outside-the-inbox`), where its numbers are kept
  (`.numbered`), and a file that isn't there, are each refused, and the
  file outside is untouched. The number isn't given again.
- **While a Room holds the disk**, the list and the removal are done by
  its session, and the Room goes on.

**Hand-breaks** (`unit-breaks.log`, `root/final-break-*.log`): 36 breaks,
each taking out or bending one guard. Every test named for a break failed.

- **34 in the jail**: 34 broken, 0 survived; the tests passed before and after.
- **2 as root**: the inbox's list and its removal, which only root
  reaches, each made to do nothing. Each failed the tests named for it:
  the list's by an empty list, the removal's by a name outside the inbox
  that was not refused and a file the held session didn't remove.

The breaks are in `fi_break.py` in the bundle.

**Mutation**, on each crate's changed lines:
`cargo mutants --in-place --in-diff fi-slice-<crate>.diff --timeout <n> -o <out> -- --lib`
(and `--all-features` for `hatchling-core`).

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-sys` (timeout 60) | 14 | 10 | 2 | 0 | 2 |
| `hatchling-core` (timeout 60) | 8 | 6 | 0 | 0 | 2 |
| `hatchling-channel` (timeout 60) | 13 | 13 | 0 | 0 | 0 |
| `hatchling-broker` (timeout 240) | 160 | 114 | 3 | 0 | 43 |

- `mut-sys-outcomes.json` sha256 `834e9522ef6c42f2e196c23f15428813b2a77bc70b4c06b0aab1b8d9bb91beea`.
- `mut-core-outcomes.json` sha256 `88ddc115b4492ee3a156a368542435f73b42935a484c7ac8d29cb74daf8054f9`.
- `mut-channel-outcomes.json` sha256 `c39cf2da81d970d65d5ddee2fe442b4356912811ae43e7b8b6f01c7ce35dce70`.
- `mut-broker-outcomes.json` sha256 `c15f67c4f36857416799d7d50211a4a31493f3f7073786ca9106ef299cf4d924`.

The broker's run was on the tested tree. The other three were on tree
`0f48e821`, where those three crates' files are the same as in the
tested tree (`mutation-tree-diff.txt`: no difference under them).
Earlier broker runs on the way, which found the gaps the last closed,
are in the bundle as `mut-broker-first*` and `mut-broker-second*`.

Missed, and why each is left:

- `room_inbox.rs:220:5`, `list` → `Ok(vec![])`: the inbox as root; the
  jail's tests can't call it. Caught by the root break
  `the-inbox-lists-nothing`.
- `room_inbox.rs:260:5`, `remove` → `Ok(())`: the same; caught by
  `the-inbox-removes-nothing`.
- `api_ops.rs:965:9`, `SpawnedInbox::open` → `false`: in the jail there is
  no Room, so it is false already. Every caller's refusal on a closed
  Room is tested through the stand-in inbox, and a root run with a Room
  open is the only place the two differ.
- `chat_run.rs:640:9`, `<() as Notes>::take` → `vec![]`: it returns an
  empty list already: the mutant is the same code.
- `file_in.rs:89:30`, `stem_of`, `>` → `>=`: at exactly 64 bytes the cut
  keeps the whole stem, extension and all, so cutting it changes nothing.

Unviable (every one is a build failure):

- `room_inbox.rs:220:5: replace list -> Result<Vec<InboxFile>, Error> with Ok(vec![Default::default()])`: unviable, build failure
- `room_inbox.rs:225:5: replace list_as -> Result<Vec<InboxFile>, Error> with Ok(vec![Default::default()])`: unviable, build failure
- `ui.rs:333:5: replace activity_state -> ActivityState with Default::default()`: unviable, build failure
- `api.rs:144:9: replace ApiRequest::of_action -> Self with Default::default()`: unviable, build failure
- `api_ops.rs:257:9: replace Ops<'a>::handle -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure
- `api_ops.rs:356:9: replace Ops<'a>::skill -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure
- `api_ops.rs:961:9: replace <impl Inbox for SpawnedInbox<'_>>::turn -> &'static crate::file_in::Turns with Box::leak(Box::new(Default::default()))`: unviable, build failure
- `api_ops.rs:975:9: replace <impl Inbox for SpawnedInbox<'_>>::list -> Result<Vec<crate::room_disk::InboxEntry>, String> with Ok(vec![Default::default()])`: unviable, build failure
- `api_ops.rs:1008:9: replace Ops<'a>::inbox -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure
- `api_ops.rs:1027:9: replace Ops<'a>::files -> (UiActionResult, ApiOutcome) with (Default::default(), Default::default())`: unviable, build failure
- `api_ops.rs:1120:9: replace Ops<'a>::put_in -> UiActionResult with Default::default()`: unviable, build failure
- `api_ops.rs:1234:9: replace Replies<'_>::take_crossed -> Option<SubagentEvent> with Some(Default::default())`: unviable, build failure
- `api_ops.rs:1241:9: replace Replies<'_>::abandon -> Option<SubagentEvent> with Some(Default::default())`: unviable, build failure
- `api_ops.rs:1289:9: replace Audit::record -> ActivityEvent with Default::default()`: unviable, build failure
- `api_ops.rs:1303:9: replace Audit::keep -> ActivityEvent with Default::default()`: unviable, build failure
- `chat_run.rs:478:9: replace Chat<R, K>::step -> Step with Default::default()`: unviable, build failure
- `chat_run.rs:497:9: replace Chat<R, K>::step_noted -> Step with Default::default()`: unviable, build failure
- `chat_run.rs:584:9: replace Chat<R, K>::start -> Step with Default::default()`: unviable, build failure
- `chat_run.rs:753:5: replace chat_turn -> Step with Default::default()`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::new()`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::from_iter([None])`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::new(None)`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::from(None)`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::from_iter([Some((0, Default::default()))])`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::new(Some((0, Default::default())))`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::from(Some((0, Default::default())))`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::from_iter([Some((1, Default::default()))])`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::new(Some((1, Default::default())))`: unviable, build failure
- `file_in.rs:49:9: replace Turns::held -> std::sync::MutexGuard<'_, Option<(u64, Instant)>> with MutexGuard::from(Some((1, Default::default())))`: unviable, build failure
- `file_in.rs:158:5: replace crossing -> SubagentEvent with Default::default()`: unviable, build failure
- `file_in.rs:197:9: replace Turn::take -> Option<Self> with Some(Default::default())`: unviable, build failure
- `file_in.rs:325:9: replace Sending::end -> Result<Ready, FileInRefusal> with Ok(Default::default())`: unviable, build failure
- `file_in.rs:352:9: replace Sending::sent -> Result<&mut Upload, FileInRefusal> with Ok(Box::leak(Box::new(Default::default())))`: unviable, build failure
- `file_in.rs:365:9: replace Sending::take_crossed -> Option<SubagentEvent> with Some(Default::default())`: unviable, build failure
- `room_disk.rs:1205:5: replace inbox -> Result<InboxAnswer, String> with Ok(Default::default())`: unviable, build failure
- `room_disk.rs:1215:5: replace inbox_held -> Result<InboxAnswer, String> with Ok(Default::default())`: unviable, build failure
- `room_disk.rs:1222:5: replace inbox_at -> Result<InboxAnswer, String> with Ok(Default::default())`: unviable, build failure
- `room_session.rs:2118:9: replace Sessions::inbox -> InboxReply with Default::default()`: unviable, build failure
- `spawner.rs:812:9: replace Spawner::helper -> Result<SpawnedProcess, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:972:9: replace Spawner::room_inbox_list -> Result<Vec<crate::room_disk::InboxEntry>, String> with Ok(vec![Default::default()])`: unviable, build failure
- `spawner.rs:1017:9: replace Spawner::room_inbox_ask -> Result<Reply, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1036:9: replace Spawner::room_disk -> Result<Option<crate::room_disk::RoomDiskStatus>, String> with Ok(Some(Default::default()))`: unviable, build failure
- `spawner.rs:1237:9: replace Spawner::room_change -> Result<RoomChanged, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1307:9: replace Spawner::room_job_asked -> Result<RoomJobAnswer, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1390:9: replace SpawnedProcess::watching -> Result<Self, String> with Ok(Default::default())`: unviable, build failure
- `spawner.rs:1916:9: replace Reply::inboxed -> Self with Default::default()`: unviable, build failure
- `spawner.rs:2108:5: replace room_inbox_done -> Result<InboxAnswer, String> with Ok(Default::default())`: unviable, build failure

**The full gate** (`gate.log`), on each toolchain from a clean cache:

- fmt, clippy (default and all features, `-D warnings`), build, the
  workspace's tests, seed-check, protocol-schema and the docs audit:
  each exit 0.
- 135 test binaries, 4026 passed, 0 failed, on 1.85.0 and on 1.98.1.

**The app** (on the same tree's checkout, from Git Bash):
`npm run check` exit 0: types, lint, the design check, the action and
protocol drift checks, the four frozen files unchanged, formatting, and
the unit tests. The live adapter's coverage, run with the glob Git Bash
passes (`--test-coverage-include='dist/app/adapters/live/**'`): 185
tests, 100% of lines, branches and functions. The app's page of your
security matches `docs/your-security.md` word for word.

## Found on the way

- **The version bump would have locked out four clients**, the CI's
  upgrade check among them: it seeds the last release (v4) and checks the
  upgrade (v5). It now speaks whichever version the broker says.
- **A test's stand-in spawner waited forever on a mutant that never
  asked it**, so the mutant timed out where it should have failed. The
  test lets its end go first now, and the mutant is caught.
- **CI's upgrade check found one more client that said 4.** It runs this
  commit's `docs/examples/first-run.py` against the last release, a v4
  broker, before the upgrade. The examples now share one handshake
  (`hello.py`'s `handshake`): it says 5, and to a broker that says it
  speaks another version, asks again in that one. What the examples ask
  for is the same in both. That change came after the runs above, in its
  own commit, and touches only `docs/examples/` and
  `scripts/upgrade_state.py`: no code the runs tested. Its own check:
  the scripts' tests that use the examples (`test_first_run.py`,
  `test_hosted_model_smoke.py`, `test_get.py`,
  `test_readme_quickstart.py`) pass, and the handshake against stand-in
  brokers of v4, v5 and v9 is welcomed, and isn't asked again after a
  refusal.
- **A loop a mutant could make endless** (`shown_name` stepping back to a
  character's end) is gone: the name is cut by its characters as they
  are counted.

## What wasn't tried

- **The whole path through a real install and a real model**: an app or
  client sending a file to an installed broker with a Room open, and the
  agent reading it. Each part is tested on its own: the parts and their
  bounds, the put, list and removal as root on a real disk, the line in a
  run's task. Together they are the next slice's, with the app.
- **That the connection loop keeps a file it was sending as let go when
  the connection ends.** The pieces are tested (the file let go, and its
  line), not the loop that calls them on a real connection.
- **A stalled upload taken over after a minute, on a clock.** The turn's
  idle bound is tested with the times it is given, not by waiting.
