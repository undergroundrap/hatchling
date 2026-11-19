# Evidence: folders, on a real install, with a real model (WO-043, acceptance)

What was done, how, and what came back, when the release builds of WO-043
were installed by Windows Setup's own script into a throwaway WSL distro,
folders were given by a scripted stand-in model and by a real one, and
downloaded in the app. Two drills: the first on the build of the last
slice, and a second on the build with the two fixes the first one found.

| | First drill | Second drill |
|---|---|---|
| Build | `main` at `c9451f5e` (#516), release run 44423417395 | `main` at `ebbc1a29` (#525), release run 44455569881 |
| Date | 2026-11-19 (UTC) | 2026-11-19 (UTC) |

| | |
|---|---|
| Machine | Windows 11, WSL2 (kernel 6.6.87.2), 28 processors, a 12 GB GPU |
| Distro | `hatchling-drill`: a copy of a prepared Ubuntu 24.04 distro with `cryptsetup-bin`, its own Hatchling taken out first, so Setup installed fresh; deleted after each drill |
| Model | a scripted stand-in (`drill-model`) that runs a fixed recipe and gives its folder, then `qwen3.5:9b` on Ollama, from the maintainer's demo distro, with that distro's Hatchling stopped and locked throughout |
| Browser | the app in a Chromium-based browser pane on the same PC; each `.zip` saved at the browser's prompt by the maintainer |

## The install

- **The release files** were gathered as the release workflow gathers them
  for Setup: the four programs, the app, the installer from the same commit,
  and the Room image's five files. Their `SHA256SUMS` matched the release
  run's own, in both drills.
- **Setup** (`installer/windows/setup.ps1 -Distro hatchling-drill
  -BundledRoom -ModelEndpoint http://localhost:11434/v1 -Version 0.9.0
  -Unattended`) exited 0 both times: the vault sealed, the Library at
  `/var/lib/hatchling/library` (`0700`, the broker's user), the verified
  Room image installed, and `doctor`: "Nothing found wrong".
- The first release run's Room image check failed on its first attempt:
  its two supervisor builds ran on different GitHub runner images, with
  different `libc6-dev`. That is #517. A rerun built both on one image and
  passed.

## Folders given

| Folder | Result | Time |
|---|---|---|
| `site`: 115 files, nested, with a `.git`, and a link | kept; the link left out and counted | 0.8 s |
| one past the bound: 513 files | refused: "the folder holds more than 512 files"; nothing kept | 1.9 s |
| one holding `CON.txt` | refused: "a name in a folder is one Windows keeps for a device"; nothing kept | 2.0 s |
| at every bound: 512 files, 32 MiB, one 16 steps deep | kept | 10–11 s |
| two folders in one reply | both kept, under that reply | 1.9 s |
| `qwen3.5:9b`, first drill: a site of 120 pages | kept 95 files; the model said it had made fewer | 136 s |
| `qwen3.5:9b`, first drill, new conversation: `reef-site` | out of steps, said so; then, asked, kept 127 files | 292 s, 35 s |
| `qwen3.5:9b`, second drill: `bird-site`, 110 pages | kept 111 files first time, its answer true | 268 s |

The Room refuses `mkfifo` and binding a Unix socket (EPERM). So the agent
can make only files, folders and links there, and a pipe or socket can't be
handed over from inside it. The broker's walk counts them as `others` all
the same; that is covered by its tests.

## Limits, measured

For a 512-file, 32 MiB folder:

- **Time:** 10–11 s, of which about 8.6 s is the Library sealing the files
  one by one.
- **The broker's memory:** peaks about 33 MB above where it starts (45 MB
  against 12 MB), about the folder's size, and falls back after.
- **The vault's log:** 4 sealing lines a hand-over (`library-state` 3,
  `library-index` 1), never one per chunk.
- **The Library's disk:** about 13% above the bytes it keeps.

A full Library (64 items, 256 MiB, 4,096 files) and a conversation's 128 MiB
of reading were each reached and refused at their bound. The limits stay as
they are: none of these numbers is near what the machine can take, and each
bound was met cleanly.

### What the agent and the owner are told

The first drill found that a hand-over that would only pass a bound was told
the bound was reached: "has read 128 MiB" after 96 MiB, and "the Library is
full" with 224 of 256 MiB kept, when a smaller folder was then kept. Fixed in
#522 ([refusal-words.md](refusal-words.md)). The second drill showed all four
words on the install, each true when said:

| Case | Said | Then |
|---|---|---|
| read 96.2 MiB, a 32 MiB folder | "this is more than this conversation may still read out of the agent's computer (128 MiB in all): something smaller may still be handed over" | a 115-file folder was kept |
| read exactly 128 MiB | "this conversation has read 128 MiB out of the agent's computer, the most one may" | |
| the Library at 224.2 MiB, a 32 MiB folder | "this would take the Library past 256 MiB or 4096 files in all: something smaller may still fit" | a 115-file folder was kept |
| the Library at 256 MiB and 4,096 files | "the Library is full (64 items, 256 MiB or 4096 files in all)" | |

## Downloads, in the app

Each saved `.zip` was read back with Python's `zipfile`, and checked:

- **Its files:** every one, byte for byte, against the stand-in's recipe for
  it or against the Library's own list of the folder (`list_folder`), whose
  SHA-256 matched the folder's.
- **Its paths:** none leaves its folder, and no two names differ only by
  case.
- **Its entries:** each says an MS-DOS host, so none carries Unix modes.
- **Its mark:** each `.zip` carries `ZoneId=3`.
- **Unpacked by Windows:** with the Windows shell (`Shell.Application`, as
  Explorer's Extract All), every unpacked file carried `ZoneId=3` too,
  pointing back to its `.zip`.

| Download | How | Files |
|---|---|---|
| a folder (first drill) | the chat's card | 512 |
| a reply's two folders | "Download these 2 as one .zip" | 6 |
| three chosen, two of them named `site` (`site`, `site-2`) | chosen | 742 |
| the whole Library | "Download all as one .zip" | 4,050, 236 MB |
| `site` | its own row; the app said it holds a git repository, that 2 files can run something when opened, and the link left out | 115 |
| `reef-site` (`qwen3.5:9b`) | the chat's card | 127 |
| a folder (second drill), on the Library page | its card's menu | 512 |
| two chosen, on the Library page | Select, then "Download as one .zip" | 6 |
| `bird-site` (`qwen3.5:9b`), on the Library page | its card's menu | 111 |

### On the Library page (second drill)

The first drill found the list lived only in Settings, which didn't update:
choosing showed no button, and a `.zip` showed no progress. Fixed in #523
([library-page.md](library-page.md)). On the install:

- **The page:** the agent's files appear on the Library page in the demo's
  cards, with the Library's use and where the copies come from.
- **Choosing:** Select's choices drew at once ("1 chosen"), each card marked
  pressed, focus kept on the card.
- **During a 512-file `.zip`:** its progress counted up in place ("42 of
  512", "94 of 512"…). A click on another card landed, and opened its
  details: fixed title, size, when it was kept, its list's SHA-256.
- **Settings:** said "12 of 64 items, 224.3 MB of 256 MB", and went to "11
  of 64 items, 192.3 MB" within seconds of a removal elsewhere, while it
  was open.

## Activity

Activity kept one `file_out` line for each folder read out of the agent's
computer, by its name, its count of files, its size and how it ended:
`offered` for each folder kept, and `refused`, without a name, for the two
refused at a conversation's or the Library's bound after the Room had listed
them. A folder refused before anything was read (past a bound,
or holding a device's name) has no line, as for files coming in.

## Found, and filed

- **#517:** the Room's supervisor links the runner image's `libc.a`, so a
  release can't be rebuilt once the image changes.
- **#518:** downloads take about 190 ms a file: a folder's list is read and
  checked again for each file. The whole Library took 12.5 minutes.
- **#519:** in the first drill, after earlier answers from the stand-in that
  quoted tool results, `qwen3.5:9b` answered with a made-up `give_folder`
  result and made no call. Nothing was kept and no card was drawn, because
  cards come only from the broker; the chat's words were wrong.
- **#527:** with the PC short of sockets (another program held 1,600), one
  of the app's files failed to load, and the app stayed on "Opening
  Hatchling…" with no error. Its host also closes the connection after
  every file.
- **#528:** opening the app with a long conversation froze the page for over
  a minute. The whole page is drawn once per event the stream replays, each
  draw longer than the last. The same per-event drawing goes on during a
  `.zip`, because each file read is an API call with its own Activity event.
  So #523's progress line counts without drawing, but the page is still
  drawn per file. A click landed during one here; batching the drawing is
  #528's fix.
- **#529:** the standalone page built on Windows can't start.

## Not tried on the install

- **A folder holding a secret:** proven by the hand-over's tests against a
  real vault ([give-folder.md](give-folder.md)), not here.
- **A folder given to the agent:** outside WO-043's scope ("Not in scope");
  files into the agent's computer were proven in WO-041
  ([files-acceptance.md](files-acceptance.md)).
- **A conversation's cards across a broker restart:** proven by the broker's
  and the app's tests ([chat-cards.md](chat-cards.md)). On the install the
  broker restarted, for the model switch, before the conversations it
  showed began.
