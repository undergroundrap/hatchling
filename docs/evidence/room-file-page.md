# Evidence: the Room hands back a page of a file

What was tested, how, and what came back, for a change to the Room's
wire, its supervisor and the spawner: the spawner can ask the supervisor
for a page of a file. Every number here is read from a file in the
results bundle, which is attached to the release that carries this
change.

| | |
|---|---|
| Base commit | `f9fac7e7` (`fix(installer): copy a release into WSL byte for byte on a UTF-8 console (#435)`). The runs were made before that commit was on `main`, on a base with the same tree |
| Tested tree | `e77e4cb06ecb6149721fd9b26c9c2fd78f6e8e49` |
| Date | 2026-11-13 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), 28 processors, in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |
| Results bundle | `hatchling-room-file-page-results.tar.gz`, 2097299 bytes, sha256 `93fee5e3447629f3f42591a1ae583f8932424a3e539ee01ecabc4652268db42b` |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it.

## What this is for

The agent can run a program in the Room. To read a file it has to run
one too (`sed`, `head`), and is shown the first 1 KiB and the last 3 KiB
of what that prints. The Room's supervisor remembers only 16 ended jobs,
so every file read that way pushes an earlier command's kept output out.

This change is the part underneath a tool that reads files: the spawner
can ask the supervisor for a page of a file, and no job is started for
it. No agent session is given it yet. The tool is the next change.

## What changed

- `crates/hatchling-core/src/room.rs`, the wire: `Ask::Page` (a path, a
  first line counted from 1, and how many lines), `Say::Page` and
  `Say::NoPage`, with the bounds: 8 KiB a page, at most 2000 lines asked
  for, 32 MiB looked through, a path of at most 1024 bytes.
  `check_page` is what may be asked; `Page::check` is what the spawner
  holds an answer to.
- `crates/hatchling-browser/src/bin/hatchling-room.rs`, the supervisor
  in the Room: it reads the file itself, as the Room's user. It opens it
  without waiting and reads only a file, so a pipe or a device can't
  hold its one thread. It hands back whole lines; a first line longer
  than a page comes back cut, and said to be; lines with a NUL byte
  aren't handed back; a line past the last is said to be, with the
  file's last line.
- `crates/hatchling-broker/src/room_session.rs`, the spawner's session
  thread: a page is asked of the supervisor in its turn, one thing at a
  time. A page that is longer than a page, or said to be cut where it
  can't be, ends the session, as a wrong end of a job does. A page
  starts a session when there is none; it isn't asked for once whoever
  wanted it has gone; one not handed back when a session ends is
  answered once the Room is down.
- `crates/hatchling-broker/src/spawner.rs`: the request and the reply,
  and `Spawner::room_page` for the broker.
- The trust boundary page and the changelog say it.

Nothing in the spawner or the broker reads the file's bytes: they are
carried as they came.

## The tests

- The wire: what may be asked, what a page may be, and each message as
  it is written, with nothing unknown taken (`hatchling-core`, 14 tests
  of the Room's wire).
- The supervisor's reader, given text a few bytes at a time and all at
  once: whole lines from the one asked for, a last line with no line
  end, empty lines and an empty file, a page that stops at the last
  whole line that fits, a first line too long, how far it looks, bytes
  that aren't text, and a read that fails.
- The supervisor, served over a socket as the spawner reaches it, on
  real files: a page, a line past the last, no such file, a directory,
  a pipe nobody writes to, a link that loops, a file its user may not
  read, a file that isn't text, and a page while a job runs.
- The session thread, with a stand-in for the supervisor: a page in its
  turn, a page that can't be one, answers out of turn, a caller that has
  gone before the launch and before its turn, a session that ends with
  pages owed, and a session that can't be cleared.
- The spawner's request path with stand-in sessions.
- As root, with a real Room: the last two sections.

## Each part taken out by hand

27 breaks. The supervisor's tests (24) and the session's and spawner's
(59) passed unbroken before and after. All 27 were caught by the test
named for each, and each failure was read.

In the supervisor (`tests::` in `hatchling-room`):

| Break | Test that fails |
|---|---|
| The file is opened waiting | `the_supervisor_hands_back_a_page_of_a_file_and_says_why_not`: no answer comes for the pipe |
| What isn't a file is read | the same |
| The file's length isn't said | the same |
| A file that isn't there is only a failure | the same |
| Bytes no text has are handed back | `a_page_with_a_byte_no_text_has_is_not_one` |
| A page is looked for without end | `a_page_is_looked_for_only_so_far_into_a_file` |
| A line not whole by the end of the look is handed back | the same |
| A cut line isn't said to be cut | `a_first_line_too_long_for_a_page_is_cut_and_said_to_be` |
| A line that doesn't fit is cut wherever it is | `a_page_stops_at_the_last_whole_line_that_fits` |
| More lines than were asked for | `a_page_is_whole_lines_from_the_one_asked_for` |
| A line past the last is an empty page | the same |
| A last line with no line end isn't counted | the same |
| A read that fails ends the file | `a_read_that_fails_is_not_the_end_of_the_file` |

In the spawner (`room_session::tests::`, and `spawner::tests::` for the
last two):

| Break | Test that fails |
|---|---|
| A page that can't be one is handed on | `a_page_that_cannot_be_one_ends_the_session` |
| A page is read for a caller that has gone | `nothing_is_read_for_a_caller_that_has_gone` |
| A Room is launched for a reader that has gone | `nothing_is_launched_for_a_reader_that_has_gone` |
| A page to ask for is nothing in hand | the same |
| A launch that fails leaves a reader waiting | the same |
| A launch that fails leaves a page not yet taken in waiting | `a_launch_that_fails_at_once_answers_every_waiting_job` |
| A page is answered before the Room is down | `a_page_not_handed_back_is_answered_once_the_room_is_down` |
| A page waiting its turn is dropped with the session | the same |
| A held session pays the page it owes | `a_session_that_cannot_be_cleared_stays_held_and_says_so` |
| A held session refuses a page in other words | the same |
| A page that may not be asked for starts a session | `a_page_starts_a_session_and_the_next_uses_it` |
| No file is said in a failure's words | `a_page_is_asked_of_the_supervisor_in_its_turn_and_handed_back` |
| An invalid page is sent to the spawner | `a_page_that_may_not_be_asked_for_never_reaches_the_spawner` |
| The spawner asks for another line | `a_page_reaches_the_session_and_starts_it` |

`unit-breaks.log` sha256
`07efffedf4e3b5c003973ad4f647d97ecf3c2469e81f96f9e4c570d9d79b1792`.

## Mutation

`cargo mutants --in-place --in-diff <the crate's lines> --timeout <60 or 90>`,
each after an unmutated baseline, with only the mutated crate's tests:
`-- --lib` for `hatchling-core` (60) and `hatchling-broker` (90), and
`--features room-supervisor -- --bin hatchling-room` for the supervisor
(60).

| Crate | Mutants | Caught | Missed | Timed out | Unviable |
|---|---|---|---|---|---|
| `hatchling-core` | 33 | 32 | 0 | 0 | 1 |
| `hatchling-browser` (the supervisor) | 31 | 28 | 0 | 0 | 3 |
| `hatchling-broker` | 44 | 37 | 0 | 0 | 7 |

Unviable, one line each:

- `crates/hatchling-core/src/room.rs:60:33: replace * with /`: unviable, build failure.
- `crates/hatchling-browser/src/bin/hatchling-room.rs:449:9: replace Room<'_>::wait -> Result<(bool, Vec<Watch>)> with Ok((true, vec![Default::default()]))`: unviable, build failure.
- `crates/hatchling-browser/src/bin/hatchling-room.rs:449:9: replace Room<'_>::wait -> Result<(bool, Vec<Watch>)> with Ok((false, vec![Default::default()]))`: unviable, build failure.
- `crates/hatchling-browser/src/bin/hatchling-room.rs:545:9: replace Room<'_>::answer -> Say with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:1482:9: replace Sessions::run -> JobReply with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:1506:9: replace Sessions::page -> PageReply with Default::default()`: unviable, build failure.
- `crates/hatchling-broker/src/room_session.rs:1527:9: replace Sessions::ask_or_start -> Result<T, String> with Ok(Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/spawner.rs:789:9: replace Spawner::helper -> Result<SpawnedProcess, String> with Ok(Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/spawner.rs:900:9: replace Spawner::room_disk -> Result<Option<crate::room_disk::RoomDiskStatus>, String> with Ok(Some(Default::default()))`: unviable, build failure.
- `crates/hatchling-broker/src/spawner.rs:1074:9: replace Spawner::room_job_asked -> Result<RoomJobAnswer, String> with Ok(Default::default())`: unviable, build failure.
- `crates/hatchling-broker/src/spawner.rs:1149:9: replace SpawnedProcess::watching -> Result<Self, String> with Ok(Default::default())`: unviable, build failure.

A first run, of an earlier tree and not in the bundle, missed four
mutants in the supervisor, all on the one line that put together the
flags the file is opened with. Three put `^` where `|` was, between flags that share no
bit, which changes nothing. The fourth left out two of the flags (no
controlling terminal, and closed across an exec) and no test saw it.
The line is now written without those operators, so it has no such
mutants; the two flags still have no test, and are named below.

`mut-core-outcomes.json` sha256
`53b1d287444d5eedfaf693aee7434f693fb965738585bd30698ff1e870a0759d`;
`mut-browser-outcomes.json` sha256
`29145c5332947c1e6ec816a15d47597be1b8995df61e14ae2ea27c3bba7761fb`;
`mut-broker-outcomes.json` sha256
`53da5ce0f23294f7b78b07dbf1e988836b265690c85ca4a6f427c472f5b34824`.

## The gate

In the sandbox, without root, on each toolchain from a cleared build
cache: `cargo fmt --check`, `cargo clippy -D warnings` (and with all
features), `cargo build`, `cargo test --workspace --no-fail-fast` with
`HATCHLING_REQUIRE_CELL=1` and `HATCHLING_REQUIRE_CGROUP=1`, the seed
check and the protocol schema check.

| Toolchain | Every step's exit | Test binaries | Passed | Failed | Ignored |
|---|---|---|---|---|---|
| 1.85.0 | 0 | 135 | 3794 | 0 | 0 |
| 1.98.1 | 0 | 135 | 3794 | 0 | 0 |

`gate.log` sha256
`8a13cf1bb1b945ce6021e0a3a0fd7cec0ad90776b0ec30e40c4e78151bd74622`.

### A gate before this one that failed

The gate of the tree before this one (`cb6c738d76cca1765c1b1a072fd8901249ad2422`)
passed on 1.85.0 and failed one test on 1.98.1:
`spawner::tests::the_spawner_refuses_a_spec_its_helper_does_not_start`,
which this change doesn't touch. It drops one end of a socket and
expects the other to see it gone at once. Tests run side by side in one
process, and a child that another test has forked holds a copy of that
end for a moment, so it isn't gone yet. A test added by this change did
the same thing; it now shuts the socket before dropping it, which is
the one difference between that tree and the tested one. The older test
is left as it is here, and is to be changed on its own.

`earlier/gate-earlier-tree.log` sha256
`3c39a594246e783420b01b8d222e0be02d7bb9dbb2684ded04e86d657e019eea`;
`earlier/gate-earlier-tree-1.98.1-test.log` sha256
`7546c336eb96c1dc31cd8049f5b27a69b3f50f8a3de83c334ed2d648992389cc`.

A second test this change doesn't touch failed in one of the 27 runs
with a part taken out, beside the test named for that break:
`room_session::tests::a_job_that_does_not_end_when_stopped_ends_the_session`
("its grace wasn't waited out"). It starts its clock a moment after the
session starts the wait it measures. It passed in the other runs, and
is to be changed on its own with the first.

## Under the Room's filter

The supervisor built static, as the Room's image carries it, with the
repository's own script (sha256
`c00b9832b0f441571409fc4e4604dd4a5e898b525ac9d2daf5d625ce8f9eb280`), and
run under the Room's syscall filter outside a Room. Its own 24 tests
pass in that build, and the three proofs under the filter pass. The
first now asks for pages too: a file a job wrote, a line past its last,
no such file, a directory, and a pipe nobody writes to, which is not
waited for.

`root/super.log` sha256
`fd078b2cb7e746cf3272722afc8a287e04c9d58fd21d013f41bbf41171b990a9`;
`root/super-proof.log` sha256
`d2c059948460fd55b41e625aa9ce9191b949a10624641b56355456f665a07bfd`.

## With a real Room, as root

Both installed Room tests, run as root outside the sandbox's jail, with
that supervisor in a verified image.
`the_installed_spawner_hands_the_verified_image_to_its_mount_helper`
now asks its spawner for pages: a file a job wrote in the workspace,
the kernel's own account of the supervisor (`/proc/self/status`), a
line past the last, no such file, a directory and a path that isn't
absolute; that the Room the page was read in is the one the jobs ran
in; and, after the disk is closed and opened again, that a page starts
a Room and the job after it runs in that one.

| Run | Result |
|---|---|
| Unbroken, before | 2 passed, 0 failed |
| No file is said in a failure's words | 1 failed: asked for a file that isn't there, the spawner said "the Room couldn't read that file" |
| Unbroken, after | 2 passed, 0 failed |

Nothing was left mapped after any run.
`root/installed-unbroken-before.log` sha256
`11dbd27cd3a89dda29553fb4ec3a5b497730b380a114ca608c81e11c1170c917`;
`root/installed-break.log` sha256
`600cb81390ae13ba04a016cf5360a63f175460fc173772475c41748006bc91db`;
`root/installed-unbroken-after.log` sha256
`2672aa3f311cd685f5e8ee4bc17a1bbbfb2fb331a8510ea7e675a107ad43e07d`.

## What this change does not do

- **Give an agent the page.** Only the spawner's own caller can ask for
  one. The tool, who may use it and what the Activity keeps of it are
  the next change.
- **Prove two of the flags the file is opened with.** It is opened so
  that it can't become the supervisor's controlling terminal, and so
  that it is closed across an exec. Neither has a test: the Room has no
  terminals, and the file is closed before the supervisor starts
  anything. The flag that matters, not waiting, has one.
- **Read a real file of 32 MiB.** How far the reader looks is a number
  it is given, and its tests give it small ones. That the supervisor
  gives it 32 MiB is the constant's own test and the one line that
  passes it.
- **Read while it reads.** The supervisor has one thread. While it
  looks through a large file for a page, it reads no job's output and
  answers nothing else; a job writing then waits on its pipe. The look
  is bounded, and the spawner ends a session whose supervisor doesn't
  answer in ten seconds.
- **Read a line longer than a page past its start.** Such a line comes
  back as its first 8 KiB, said to be cut. The rest of it is read with
  a program.
- **Keep the file from changing.** A page is what the file held when it
  was read. Two pages of a file that changed between them may not join.
- **Show, in a real Room, a file its user may not read.** The
  supervisor's own tests do, without root. The installed test's image
  has no such file to ask for.
