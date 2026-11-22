# Evidence: the supervisor's files are its own, never a job's (#573)

What was tested, how, and what came back, for
[#573](https://github.com/undergroundrap/hatchling/issues/573), found by
the v0.9.0 release security checklist (item 2: files and directories are
made safely).

| | |
|---|---|
| Base | `main` at `40a004cf` |
| Tested tree | `5d658c44` |
| Date | 2026-11-22 (UTC) |
| Machine | x86_64, Ubuntu 26.04.1, kernel 6.6.87.2 (WSL2), in a sandbox |
| Toolchains | Rust 1.85.0 (minimum) and 1.98.1 (pinned), cargo-mutants 27.1.0 |

This record was added after the runs, in its own commit. It changes no code.

## The problem

Inside the Room, the supervisor and the agent's jobs are one user, so
none of this crosses a boundary; but three of the supervisor's own
guarantees were untrue:

- **A write could remove a file it didn't make.** A file is written
  beside its place, as `.hatchling-write-<pid>-<n>`, then renamed into
  it. When that failed, the name beside it was removed, even when the
  failure was its own `create_new`, refused because a job had made that
  name first: a name anyone in the Room can guess.
- **A job's files could be planted.** A job's stdin, stdout and stderr
  files in `/session/jobs` were opened with create and truncate, through
  any link there: a job could put a link where a later job's output
  would go, and the supervisor would truncate what it named.
- **One guard had no test.** A stop signals the job's process group only
  while its first process is unreaped, since by then its pid, and the
  group it named, may be another's. Only the Room's own PID namespace
  was proven.

## What changed

- A write makes its file beside the place by `create_new`, and a name
  something else made first is passed over, up to 16 names: never
  written over, never removed. Only the file it made is removed if it
  doesn't get there.
- A job's files are made with `create_new` (`O_EXCL`, which never opens
  through a link). A job's name is never given twice, so its own files
  are never there before; a file or a link a job put there first leaves
  the job without that file, and runs it all the same.
- The group kill's guard has a test of its own.

## Results

| Check | Result |
|---|---|
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings`, 1.85.0 and 1.98.1 | clean |
| The supervisor's tests (`-p hatchling-room --features room-supervisor --bin hatchling-room`), 1.85.0 and 1.98.1 | 64 passed on each |
| Mutation of the lines changed (`--in-diff`) | 6 mutants: 3 caught, 2 unviable, 1 missed, equivalent (below) |

The new tests:

- `a_name_made_first_beside_the_file_is_passed_over_and_kept`: eight
  names planted beside a file; the write is made, under a name of its
  own, and each planted file is as it was. Every name tried planted, the
  write is refused, and still none is removed.
- `what_a_job_put_where_a_later_jobs_files_go_is_never_opened`: a link
  where a job's stdout goes, and a file where its stderr goes; the job
  runs, and what the link names, and the file, are as they were.
- `a_job_whose_first_process_has_ended_signals_no_group`: a job whose
  first process is said to have ended, its group still holding another
  process; a stop signals nothing. Said not to have ended, the same stop
  ends the group.

`a_stream_is_kept_piece_by_piece` opened the same spill file twice; its
second capture now has a file of its own.

The supervisor's proof under the Room's filter (`--test room_supervisor`)
wasn't run here. The sandbox has no `/usr/bin/rg`, and there the proof
fails on `main` too, at `40a004cf`, in its search: the supervisor ends
when its search program isn't there, rather than answering. CI's runners
have `rg`, and the Room's image always does.

### Hand-breaks

Each was applied, the supervisor's tests run, then undone. 4 broken,
4 caught:

| Break | Caught by |
|---|---|
| A job's files opened with create and truncate, as before | `what_a_job_put_where_a_later_jobs_files_go_is_never_opened` |
| A name made first stops the write | `a_name_made_first_beside_the_file_is_passed_over_and_kept` |
| A name made first removed | the same |
| An ended job's group signalled | `a_job_whose_first_process_has_ended_signals_no_group` |

### The mutant missed

`beside`'s match guard, *the name is taken*, replaced with `true`: every
error tries the next name, not only a taken one. Another error, such as
a directory the Room's user can't write, is the same for every name, so
it comes back after 16 tries instead of one, with nothing made and
nothing removed. It changes how many tries, never what's left or what's
said, so it's equivalent.

## Residual

- The supervisor ending when its search program is missing (above) is
  its own issue, outside this change.
