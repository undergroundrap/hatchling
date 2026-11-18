# Evidence: groups and choosing in the app (WO-043, slice 6)

What was tested, how, and what came back, for the change that downloads
a folder the agent gave as one `.zip`, groups the Library by the
conversation and the reply each item came from, and saves a reply's
items, any the owner chooses, or all, as one `.zip`.

| | |
|---|---|
| Base commit | `0373a2f5` (`feat(chat): show what the agent gave under its reply, across a restart (WO-043) (#514)`) |
| Tested tree | `5c5d186c328d1e43d6c92811ac475ee57ae4ef3b` |
| Date | 2026-11-18 (UTC) |
| Machine | Windows 11, Node 22, the app's checks from Git Bash |

This record was added after the runs, in its own commit. It changes no
code: the tested tree is the tree of the commit before it. The change is
the app's alone: no Rust, wire or frozen-contract file changed, so the
Rust gate and mutation runs are CI's on this tree, unchanged from the
base.

## Scope

WO-043 says `.zip`. Saving the same checked files as `.tar` or
`.tar.gz` was asked for too; it was split out to
[#515](https://github.com/undergroundrap/hatchling/issues/515). Its one
unmeasured property goes with it: whether Windows keeps the browser's
mark of the web on files unpacked from a tar.

## What changed

- **A folder downloads** from the chat or Settings as one `.zip`, under
  its name, with its folders as they were. The read:
  - **Its list, page by page.** Each page must be:
    - of the folder chosen;
    - from the place asked;
    - of the folder's count;
    - exactly as long as it should be.
  - **The whole list, held to the folder.** Its sizes must add up to the
    folder's. Each hash must be one a SHA-256 can be, and each file must
    be within the bound for one. The list's own SHA-256 is recomputed from
    the pages and compared with the folder's. This works because the
    Library hashes its compact JSON, and the paths are printable ASCII
    without `"` or `\`.
  - **Every path, checked by the app itself.** The rules are the broker's
    `step_refused` and `check_paths`, never taken on its word, plus
    `CONIN$`, `CONOUT$` and `CLOCK$`
    ([#511](https://github.com/undergroundrap/hatchling/issues/511)
    remains for the broker).
  - **Each file, held to its entry.** It is read by
    `begin_file_out{id, file}` and held to its entry's folder, place,
    path, size, hash and count of parts. Its parts are read in order and
    checked against the hash. Nothing is saved unless every file is
    whole.
- **The Library in Settings:**
  - grouped by conversation (the open one first, then newest), then by
    reply;
  - each reply labelled by when it gave, and what came before replies
    kept apart;
  - a reply's items downloadable together (`hatchling-<run id>.zip`);
  - a box on each item to choose any (`hatchling-chosen.zip`; one alone
    downloads as itself);
  - Download all takes the folders too.
- **The .zip writer:**
  - folders as entries of their own, once, before their first file, with
    the MS-DOS folder attribute;
  - made by MS-DOS, so it carries no mode;
  - a path given twice or a file where a folder is, never written;
  - never the values that mean "see Zip64";
  - the output a Blob of parts, never put together in the page.
- **The save says:**
  - how many files can run something when opened;
  - a git repository in it (a `.git`, or bare), which git run there
    acts on;
  - paths long enough that Windows' Explorer may not unpack them far
    down;
  - what each folder's walk left out when it was given.
- **Docs:** `docs/local-api.md` (the list hash's encoding; what the app
  does), `docs/your-security.md` and its copy in the app, and the
  changelog.

## Before the code, and after it

The design was reviewed independently before any code, and the diff
again before the runs.

From the design review, three serious findings, all taken:

- **Tar is outside the work order's scope:** split to #515.
- **The list wasn't bound to the folder chosen:** now its count, size
  and hash.
- **What a walk left out wasn't said at download:** now on the card, in
  Settings and in the save.

Moderate findings, also taken:

- **The path check's gaps:** step and path lengths, the trimmed device
  stem, the `$` devices.
- **The caution counted only top names.**
- **Memory:** the output is now a Blob of parts.

From the diff review, no serious finding. Taken:

- **A bare git repository** wasn't said.
- **One chosen item** was wrapped in a `.zip`.
- **A reply's number** shifted when another's items were removed: it is
  now labelled by time and saved by its run.
- **Zip64's own values** were allowed.
- **Too many for one `.zip`** was said as memory.
- **An id asked twice** was saved twice.
- **A folder removed here** kept its chat card's Download until the list
  was read again.
- **Choosing drew Settings twice.**
- **Times were compared by locale.**
- **Ten guards a one-line change would have broken with no test
  failing,** each now with its test.

Left: the chat view's own placement (`features/chat/chat.ts`) still has
no unit test of its own, as slice 5 said.

## Tests

- **A folder** of 113 files, nested and spanning four pages, at least
  100 as WO-043 asks. One file is three parts long and one is empty.
  - Every file comes back byte for byte, read by an independent reader of
    the `.zip`'s central directory.
  - Each folder is an entry once, marked as a folder.
  - The pages are asked from 0, 32, 64 and 96, and the files by their
    places in order.
  - Progress is counted file by file.
- **The list held to the folder.** Each of these saves nothing:
  - a page of another folder, from another place, of another count, or
    short;
  - an answer of another kind, or a refusal;
  - a list of another hash or size;
  - a hash that can't be one, or a file larger than one can be, each with
    the folder's hash and size made to agree, so only that guard can
    catch it;
  - a path Hatchling never keeps;
  - a file that starts as another's: another folder, place, path, size,
    hash or count of parts;
  - a part that isn't the file's;
  - no answer to a page or to a file's start.
- **The app's path rules**, each refused for its own reason:
  - an empty, `.` or `..` step, or a leading `/`;
  - over 255 bytes a step;
  - not plain ASCII, a control character, DEL, or each of Windows'
    forbidden characters;
  - a space at either end, or a trailing dot;
  - each device name, with an extension, with a space before the dot, and
    the `$` ones;
  - each kind of file Windows acts on;
  - 17 steps, or 1,025 bytes (16 steps and 1,024 bytes are kept);
  - case twins;
  - a file where a folder is.
- **What the save says:**
  - files that run;
  - a `.git` in any case, a bare repository, and names that only look
    like one;
  - long paths at 199 and 200 bytes;
  - what each walk left out.
- **Choosing:**
  - items in the order asked, the names at the top made distinct;
  - an id asked twice saved once;
  - one no longer listed skipped, and none listed said;
  - the chosen held to the list, and let go on removal even once listed
    again;
  - a folder from the stream downloads before the list holds it;
  - a folder removed here is said as removed at once.
- **The `.zip`:**
  - files and folders, with their times;
  - a path twice in any case, or a file where a folder is, never written;
  - 65,534 entries kept and 65,535 refused;
  - a directory ending at 0xffffffff refused, one byte short of it kept.
- **The views:**
  - Settings grouped by conversation and reply, with what came before
    replies apart;
  - a reply's Download these, only for more than one item and never for
    what came before replies;
  - the boxes, their state, their ids and one draw;
  - the chosen downloaded in the list's order;
  - one chosen downloaded as itself;
  - a folder's Download in Settings and in the chat;
  - what a walk left out on the card.
- **Coverage:** the live adapter's gate, run from bash with CI's quoting:
  100% of its lines, branches and functions (`agent-files.js` and
  `zip.js` among them).

## Breaks by hand

**56 in the app, 0 survived**, each failing the tests named for it, the
tests passing before and after. Eight of the first run's 56 didn't build
or weren't caught:

- Six broke the build, not the guard: each left a name unused or a loop
  empty. They were rewritten to compile and break only the guard, and
  were caught.
- One was a real gap. The bare-repository check took any name ending in
  `head`, and no test had one without a `/` before it. Such a test was
  added and catches it.
- One named the wrong test. The view's fixture has the open conversation
  as the newest, so it can't tell "open first" from "newest first". The
  session test catches it.

The breaks:

- **The list, held to the folder:**
  - its paths, hash, size, entry hashes or entry sizes unchecked;
  - a page of another folder, from anywhere, or of any total.
- **Each file:** of another folder, place, path or hash, or of any parts.
- **What is saved and said:**
  - the names at the top not made distinct;
  - a folder's files not under its name;
  - left out, a git repository (by `.git`, bare, `HEAD` anywhere, or
    case), long paths, or running files unsaid;
  - the long-path bound;
  - a folder's files uncounted;
  - a folder only from the list;
  - the chosen unfiltered or kept on removal;
  - many in the list's order, not the order asked;
  - an id twice;
  - a removed folder's card kept.
- **The `.zip`:**
  - no folders, or folders as files;
  - a path twice, or a file where a folder is;
  - the entry bound, the sentinel, or the directory unbounded.
- **The path rules:**
  - the `$` devices;
  - the device stem untrimmed;
  - the trailing dot;
  - case twins;
  - a file and a folder;
  - depth;
  - length;
  - the forbidden characters;
  - the reaching kinds.
- **The groups:** the open conversation not first, or none not last.
- **The views:**
  - a reply's Download these for one, or for what came before replies;
  - the box unwired or always ticked;
  - one chosen as a `.zip`;
  - the chosen out of the list's order;
  - a folder's Download missing in Settings, or of another id in the
    chat.

## The app's checks

On the tested tree, `npm run check` from Git Bash passed:

- typecheck, lint (169 modules, 0 errors), design, actions, protocol (the
  generated types match the schema), the seam check (the frozen
  contract's four files unchanged), formatting;
- 597, 257 and 16 tests passed, none failed;
- the live and operator coverage gates.

The live adapter's coverage gate, run again from bash with CI's quoting
(the Windows `cmd` run of it passes vacuously): 100% of lines, branches
and functions.

No Rust changed, so CI's gate on 1.85.0 and 1.98.1 and its other checks
are this slice's Rust evidence, read on its pull request before merging.
