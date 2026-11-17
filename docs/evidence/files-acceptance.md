# Evidence: files each way on a real install, with a real model (WO-041, acceptance)

What was done, how, and what came back, when the release build of
WO-041's last slice was installed by Windows Setup's own script into a
throwaway WSL distro, and a file went each way: first with a scripted
stand-in model, then with a real one.

| | |
|---|---|
| Build | `main` at `648975e4` (`feat(app): download a file the agent gave you, and keep the Library on (WO-041) (#497)`); its files from release run 44135412948, of the same tree (`fe86bde0`) |
| Date | 2026-11-17 (UTC) |
| Machine | Windows 11, WSL2 (kernel 6.6.87.2), 28 processors, a 12 GB GPU |
| Distro | `hatchling-drill`: a copy of a prepared Ubuntu 24.04 distro with `cryptsetup-bin`, its own Hatchling taken out first, so Setup installed fresh; deleted after |
| Model | a scripted stand-in (`drill-model`), then `qwen3.5:9b` on Ollama, from the maintainer's own demo distro, with its Hatchling stopped and locked throughout |
| Browser | the app in a Chromium-based browser pane on the same PC |

## The install

- **The release files** were gathered as the release workflow gathers
  them for Setup (the four programs, the app, the installer from the same
  commit, the Room image's five files): their `SHA256SUMS` is identical to
  the one the release run printed.
- **Setup** (`installer/windows/setup.ps1 -Distro hatchling-drill
  -BundledRoom -ModelEndpoint http://localhost:11434/v1 -Version 0.9.0
  -Unattended`) exited 0: a fresh install, its vault sealed (the Library's
  store among the stores: `migrate-vault: library: sealed 0`), the
  verified Room image installed, and `doctor`: "Nothing found wrong".
- **The Library is kept**: `HATCHLING_LIBRARY_DIR=/var/lib/hatchling/library`
  in the written configuration, the directory `0700`, owned by the
  broker's user, holding its own store key.

## A file in

- A text file was given to the agent over the local API, as the install's
  owner, with the same actions the app sends (`begin_file_in`, its parts,
  `end_file_in`). Each was answered `file_in_placed`, by the inbox's names
  `0001-notes-for-agent.txt` (52 bytes) and `0002-field-notes.txt` (104
  bytes); the chat showed each as the app's own notice, the owner's name
  beside it.
- **The stand-in** read it with `read` at
  `/workspace/inbox/0001-notes-for-agent.txt` in the Room, and answered
  with its lines.
- **`qwen3.5:9b`**, asked about `field-notes.txt`, first said it couldn't
  find it; told the file was in its inbox, it read it in the Room and
  answered "The file mentions a grey heron. It stood still for eleven
  minutes.", which is what the file says. Why it couldn't find it at
  first is below.

## A file out

- **The stand-in** wrote `out/drill-report.txt` with `write` and gave it
  with `give_file`; **`qwen3.5:9b`**, asked for "a short file called
  heron-summary.txt … three lines about grey herons, … to download",
  wrote it and gave it the same way, unprompted on the tools.
- Each was kept in the Library and the chat showed the card: "Hatchling
  gave you a file: heron-summary.txt, 297 bytes. It is kept in
  Hatchling's Library.", "From the agent's computer. Made by the agent:
  open it only if you trust what it made.", and **Download**; nothing of
  the file was shown in the page.
- **Download** read the Library's copy, and the browser asked where to save
  it; the owner accepted.

| File | Library's length and SHA-256 | The saved file's | Zone mark |
|---|---|---|---|
| `drill-report.txt` (stand-in) | 49, `74c1ccab3c190449d423f9f360969721b7e3951bbe0f93f2c22c811ce3716ced` | 49, the same | `ZoneId=3` |
| `heron-summary.txt` (`qwen3.5:9b`) | 297, `7c2353c208f455eec1268e428c926e5473214c81352c6eb67a88947ed64e99ca` | 297, the same | `ZoneId=3` |

Byte for byte, both. `ZoneId=3` is Windows' Internet zone: a download
saved from the app is marked as from the internet, so Windows warns
before it runs one.

## Activity

The activity log kept one line for each crossing, by the inbox's or the
broker's name, the size and how it ended: `file_in` `0001-notes-for-agent.txt`
52 `placed`, `file_in` `0002-field-notes.txt` 104 `placed`, and a `file_out`
for each file given, `offered`. Never what a file held.

## Found, and what it means

- **A file's note can be lost to a run that fails.** The broker tells the
  model of a file given once, in the next run's task. That run failed
  before the model saw it: it was the first after the model's server
  started, and the agent's check of the model failed while the model was
  still loading (about 15 seconds). The next run wasn’t told of the
  file, so the model looked for it by the owner's name and didn't find it.
  The file was in the inbox all along, and the model read it once pointed
  there. Filed as [#498](https://github.com/undergroundrap/hatchling/issues/498).
- **A model's first answer after its server starts can be too slow** for
  the agent's check of it, though `doctor` found the server: the same
  bug's other half.
- **After a broker restart**, a file's card is gone from the chat, as
  documented: the stream's events live in memory. The file stays in the
  Library's list in Settings.

## What wasn't tried

- **The app's own attach button**: its picker is the operating system's,
  which the browser pane can't be driven through; the file in was given
  with the same actions over the local API. The app's attach is proven by
  its own tests ([room-file-in-app](room-file-in-app.md)).
- **A file holding a secret, and a refused file, on the install**: those
  are proven by the hand-over's tests against a real vault
  ([hand-over](hand-over.md)), not here.
- **Another browser**: the zone mark was seen with this Chromium-based
  browser; other browsers mark downloads too, but weren't tried.
