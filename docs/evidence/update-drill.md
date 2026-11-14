# Evidence: an install updated to main, and used with a hosted model

What was done, how, and what came back, for the page on updating
(`docs/updating.md`): a real install was snapshotted, updated to main's
build, unlocked by its owner, and asked eleven things about files in the
agent's computer with a hosted model. Every number here is read from a
file in the results bundle, which is attached to the release that carries
this change, unless it says otherwise.

| | |
|---|---|
| Installed build | main `90b2f0c2` (`feat(agent): read a file in the Room from a chat (#438)`), tree `692f02832edbde074cf389812489ae9f4d1ecad9`, from that commit's dry run of the release workflow (run 43526090955): artifacts `build-x86_64-unknown-linux-gnu-a`, `ui-a`, `room-image-verified` |
| Programs installed | `hatchling-broker` sha256 `5df18ef7…7ab68`, `hatchling-agent` `6b1b57a6…9c5f`, `hatchling-host` `620e5808…62be`, `hatchling-browser` `f57acfdd…9262` (`installed-files-SHA256SUMS` has each in full) |
| The install | x86_64, Ubuntu 26.04 under WSL2, kernel 6.6.87.2; made by the Linux installer from a release on 2026-11-02, its programs last replaced on 2026-11-06; vault protected by a security key; a local model (qwen3.5:9b, Ollama) in use, and a hosted one set up as the other (nvidia/nemotron-3-ultra-550b-a55b:free at openrouter.ai) |
| Date | 2026-11-13 and 14 (UTC) |
| Results bundle | `hatchling-update-drill-results.tar.gz`, 9892 bytes, sha256 `fee1774070b593bde7c0a141bb1b5c7e1747de194a85a60ec3f3968fb4510fb3` |

This is one install, on one machine, updated once. It isn't a test suite,
and it changes no code.

## The snapshot

`wsl --terminate`, then `wsl --export` of the distro to a file: exit 0 in
89 seconds, 18.1 GB for a distro holding 19 GB, two local models among
it. `wsl` noted five sockets it can't archive; the service makes them
again at its start. Putting the snapshot back with `wsl --import` was not
run.

## The update

The files were laid out as the release workflow lays out Windows Setup's,
with a `SHA256SUMS` of them, and installed with the installer's own update
(`upd.sh` in the bundle is the whole of it):

```
env SUDO_UID=<the owner's uid> sh installer/scripts/install.sh --from <dir> --keep-model --windows-room-bundle
```

`--windows-room-bundle` is what installs the image the agent's computer
needs, and it is what Windows Setup passes. Windows Setup itself wasn't
used: it installs for an account of its own, and this install's owner is
another.

`update.log` has what the installer said. It checked every file, replaced
the four programs and the app, made the `hatchling-room` user, kept the
configuration, ran `doctor` ("Nothing found wrong"), installed the Room
image, restarted, and ended "Hatchling is up to date, and locked". After:

- the four programs' hashes are the build's;
- the configuration has the names it had (the installer added none, and
  made no copy of it);
- the keyring is as it was: hardware key, ten backup codes left;
- both models are as they were, and `doctor` named both.

The owner unlocked it with their key. The broker then made the room disk
and opened it (`the room disk is open made=true`). In the app, the
conversation from before the update was there, marked "Kept from before
the broker restarted", and the feed's earlier notes were there.

## With a hosted model

The model was switched in the app (Settings, General, Model), which asked
before sending anything to its server, and restarted nothing. Each task
was typed into the chat in a new conversation. `chat-hosted.txt` is the
chat as the app showed it; `hosted-broker-log.txt` is the broker's lines
for the same minutes. Each call to the model took between 10 and 80
seconds.

| Task | Answer | What the agent used |
|---|---|---|
| A small file | Right | `read` |
| One line by its number (437) | Right | `read`, from 437, one line |
| A range of lines (201 to 205) | Right | a command (`sed`) |
| A file that isn't there | Right: said it isn't, and listed what is | `ls -la` twice, then `find` |
| A file with an instruction planted in it | Right: called it an injection attempt, and did not act on it | `ls -la`, `find`, then `read` |
| A file that isn't text | **Wrong**: "The /workspace directory is empty" | `file`, then `ls -la` |
| A line too long for a page | **No answer**: the model's reply never came | `read`, one line |
| A file outside the workspace (`/etc/os-release`) | Right | a command (`cat`) |
| A directory the Room's user may not open | Right that it can't be read; wrong about why | `read`, refused |
| A directory | **Wrong**: "exists but is empty" | `ls -la` |
| A command's saved output (line 1500 of 3000) | Right | `seq`, then `read` from 1500 |

Eight of eleven were answered rightly. The `read` tool was used in six,
and did what it should in each: a page, a line by its number, a refusal
for a directory that can't be opened. The workspace was as it was after
the planted instruction was read. The two wrong answers and the one with
none are the first two findings below, and neither is the read tool's.

After the run the model was switched back in the app, and the test's five
files were removed.

## What it found

None of these is this change's. Each is to be its own.

1. **`ls -la` is killed in the agent's computer, and prints nothing.**
   `room-filter-kills.txt` has the kernel's eleven audit lines: `ls`, as
   the Room's user, killed by the Room's syscall filter on syscall 192,
   `lgetxattr`. `ls -l` asks the kernel which attributes each entry has,
   which the filter allows; where a directory can't say (the Room's root
   and `/tmp` answer "Operation not supported", in `room-xattr-look.txt`)
   it asks for the security label by name, which the filter doesn't
   allow. With `-a`, `..` of `/workspace` is the Room's root. The agent is
   told the command ran and printed nothing, and three times here it
   took that for an empty directory, or said it might be one. The task
   that set the files up made them, and then ran out of its ten steps
   with `ls` killed in it more than once.
2. **A model's reply that never comes ends as a fault in Hatchling's own
   parts.** After the over-long line was read, no reply came for 334
   seconds. The broker then logged "the egress authority can't be
   reached … Resource temporarily unavailable", and the chat said to
   check the model is running. The real cause, a model that took too
   long, isn't said anywhere. One run of the earlier acceptance, with a
   local model, also never got its answer.
3. **A goals store made by an older version stops the feed's read of
   it.** Once, as the app opened after the update, the broker logged
   "feed read failed … memory store root has insecure permissions:
   /var/lib/hatchling/goals/goals". The directory was mode 755, inside
   one of mode 700, as an earlier version made it. `doctor` had found
   nothing wrong. Set to 700 by hand, the error didn't come back when the
   feed was opened again. Neither the installer nor the broker tightens
   it, and `doctor` doesn't look.
4. **There's no version command.** `hatchling-broker --version` prints
   the usage. The installed build is known here only by its hash.
5. **Only Windows Setup installs the Room's image.** The Linux installer
   takes it only with `--windows-room-bundle`, from a directory laid out
   as Setup's. An install made from a terminal has no agent's computer.
6. **Starting a new conversation removes the kept one's turns from the
   vault.** That is what it says it does ("The agent isn't given anything
   said before this"); it is written here because the five turns kept
   through the update were removed by this test's first new
   conversation.

## What wasn't tried

- Putting a snapshot back.
- An update by Windows Setup, or of an install Windows Setup made.
- An update from a build of one's own, with no `SHA256SUMS`.
- Installing the last release in a new distro, giving it something to
  lose, and updating that: the routine the page describes. The two halves
  were each run, on different installs: a new install of a pull request's
  build (the read tool's record), and this update of an older one.
- aarch64.
