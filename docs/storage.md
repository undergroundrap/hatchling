# What Hatchling keeps on disk

Hatchling writes two kinds of files: **your data**, which you back up and never delete by hand, and **disposable** files, which the broker makes and removes itself. If you're freeing space, delete only what this page marks disposable, or run `hatchling-broker clean`.

Every path below comes from a setting in the broker's environment (see `hatchling-broker --help`). Settings you haven't set write nothing.

## Your data

Deleting any of these loses credentials or history. Each lives where its setting points, readable by one user alone. The vault, its master key and its keyring are the vault keeper's: `hatchling-keeper` when the broker runs as root, which hands them to that user at its start. The approved-tools list is the user's the broker starts as. Every other store is the broker's user's.

| Setting | What is there |
| --- | --- |
| `HATCHLING_VAULT_DIR` | The vault: every secret, sealed under the store's own key. Also `.access.log`, the record of each read. |
| `HATCHLING_VAULT_KEY_FILE` | The master key (default `<vault dir>.key/key`). It wraps each sealed store's own key, so without it, or the keyring below, nothing in any store can be read. Beside it, `keyring-in-use` records that the key lives in the keyring. |
| `HATCHLING_VAULT_KEYRING` | The keyring, when the master key is wrapped by a passphrase file, TOTP or security keys. `<keyring>.lock` beside it is the edit lock, made again when needed. |
| `HATCHLING_VAULT_KEK_FILE` | The key that opens the keyring at its file tier. |
| `<vault dir>.tools/` | The approved-tools list, `approved.json`: each program a coding CLI's cell may run, its SHA-256 and when it was approved (WO-019). Made at the first start with a harness's API set. It's the user's the broker starts as (root, for a root broker), and writable by no one else, or the broker won't start. Change it with `hatchling-broker tools`: see [Approved tools](approved-tools.md). |
| `HATCHLING_SKILLS_DIR` | The signed skills the broker loads. |
| `HATCHLING_SKILL_TRUST_ROOTS` | The skill-signing keys the broker trusts. |
| `HATCHLING_SKILL_ENABLEMENTS_DIR` | The skills you enabled, sealed (WO-046): each one's name, the digest you enabled, and its version, signer and capabilities then, at most 64. Required with skills; it can't be inside the skills dir or hold it. Lost, every skill waits for you again. |
| `HATCHLING_MEMORY_DIR` | Memory: the items the agent was approved to remember, sealed in `sealed/` (WO-047), and the agent's session records and context log, which aren't. Items kept by 0.9 and before are moved into `sealed/` at the broker's start. |
| `HATCHLING_GOALS_DIR` | Goals and their tracking log, sealed in `sealed/` (WO-047). Those kept by 0.9 and before are moved in at the broker's start. |
| `HATCHLING_SCHEDULER_DIR` | Schedules, sealed. Beside it, `<scheduler dir>.activity` is the subagent activity log, `<scheduler dir>.owner/` the broker's records of yours: `decisions`, the decisions you made on what the agent asked, each with the card as you were shown it (the newest 200); `feed`, your Feed page's posts (the newest 200); `ideas`, your Ideas page (the newest 100); and `idea-signals`, what became of each idea: a line per event (put on the page, kept, started, set aside, brought back, stale, its goal gone, answered) with when, the idea's id, the kind of source and its area, never its words (the newest 10,000); and `<scheduler dir>.usage/` the model usage the broker counted: a file a day (`YYYY-MM-DD.jsonl`, UTC), a line per model call with when, the model and the tokens read and written, never what was said. Beside the days, `limits.json` holds the usage limits you set. Not sealed; days past 40 are removed. (`<scheduler dir>.runs` is disposable; see below.) |
| `HATCHLING_ARTIFACT_DIR` | Built artifacts and their build records, sealed. |
| `HATCHLING_ARTIFACT_WORKSPACE` | What the agent delivered to your workspace. |
| `HATCHLING_SPACE_DIR` | Spaces, sealed. |
| `HATCHLING_WEB_GRANTS_DIR` | The sites you always allow the agent to read, sealed (WO-028). |
| `HATCHLING_CONVERSATION_DIR` | Your conversation with the agent, as a chat run is given it: up to 8 exchanges, each side an entry of its own, and what you said before them, sealed (WO-026). New conversation removes it. |
| `HATCHLING_LIBRARY_DIR` | The Library: what the agent hands you, sealed (WO-041, WO-043). Each item, a file or a folder of files, is kept as chunks of 32 KiB, its files back to back, with an index that holds its name, its length, its count of files, its SHA-256 (a folder's: its list's, which holds each file's path, length and SHA-256), when it was kept, and the conversation and reply it came from; a file is read back only whole, and only if it comes to its hash. At most 64 items, 256 MiB and 4096 files in all; a file at most 8 MiB, a folder at most 32 MiB and 512 files; nothing is evicted. It also keeps the open conversation's count of hand-overs (16 at most), what it has read out of the agent's computer (128 MiB at most) and whether handing over is off for it; a restart keeps all three. The packaged install sets it to `/var/lib/hatchling/library`, and an upgrade adds it once, as for the conversation: set empty, it stays off. The broker's log has one line for each item kept or removed, by its id, kind, files and bytes, never its name; its chunks are written without a line each. Keeping 32 MiB took about 8.5 s on ext4, the Library's other operations waiting meanwhile. An older broker reads an index with items as damaged. It's in every backup, so a full Library adds up to 256 MiB to each; `reset agent` and `reset everything` remove it, and `reset conversation` keeps it. |
| `HATCHLING_BROWSER_PROFILES_DIR` | The browser's signed-in profiles: each one's sites, and its session as cookies, sealed (WO-030). |
| `HATCHLING_BROWSER_CREDENTIALS_DIR` | The sign-ins filled for signed-in profiles: each one's profile, exact origin and label, and its username and password, sealed (WO-030). The vault keeper holds its key, not the broker (WO-031); run as root, it's `hatchling-keeper`'s. |
| `HATCHLING_HARNESS_ROOTS` | Your projects, where a coding harness may read the files a task names. Hatchling doesn't own them and never writes there: a run works on a copy, and its changes come back as a patch in the workspace. A root can't overlap the workspace, a store or Hatchling's own state, or hold the temporary directory: the broker won't start, and says which. |
| `HATCHLING_REDACTION_DIR` | The output gate's audit log (`audit.log`), your overrides (`overrides`), your categories (`categories`), and `raw/`: kept redacted messages and the audit log's tag key, sealed. |
| `HATCHLING_ROOM_DISK_DIR` | The agent's room disk (0.9), made at a root broker's first start: `disk.img`, an encrypted disk image of a fixed size (`HATCHLING_ROOM_DISK_SIZE`, 10 GiB by default), its space reserved on the disk below when it's made, so nothing else can fill that space under it, and which never shrinks when files inside it are deleted; `disk.json`, its record; and `mnt/`, where it's mounted while it's open. Root's alone, and only a broker run as root makes it. Its key isn't kept anywhere: the vault keeper derives it from the master key, so without that the image is ciphertext. The directory can't overlap a store, the vault or the broker's own state: the broker won't start, and says which. |

Every sealed store (the vault, schedules, artifacts, spaces, the conversation, the Library, the skills' enablements, memory's and goals' `sealed/` and `raw/`) holds `.store-key`: the store's own key, wrapped under the master key. The store's files are sealed under it, so a store without its `.store-key` can't be read, even with the master key. During a `rekey-vault`, `.store-key.next` holds the new one; a rerun finishes the change. The broker never loads the master key. When it starts, it starts the vault keeper (`hatchling-broker vault-keeper`, a process of its own), which loads the master key, unwraps each store's key, and hands the broker the keys of its stores other than the secrets and the sign-ins. While the broker serves, it holds only those; the secrets store's key and the sign-ins' stay with the keeper, which reads the vault for the broker and records each read in `.access.log`. The vault also holds `keeper-released`, sealed like any secret: the names of the secrets cells have been given or have stored, which the output and artifact gates scan for. No grant may name it.

## Disposable

The broker makes these as it runs and removes each when its cell or run ends. A broker killed mid-run (`kill -9`, a crash, a power cut) leaves them behind, and its next start removes them, logging each path. `hatchling-broker clean` does the same on demand, and `clean --dry-run` lists them without removing anything. Neither touches your data: no symlink is followed, no mount is entered, and every directory set above is kept whole, even one set inside a disposable directory.

| Where | What is there |
| --- | --- |
| `$TMPDIR/hatchling-staging-<pid>-<random>/` | A cell's staging: its root filesystem while it runs. `<pid>` is the broker's; one whose broker is still running is never swept. |
| `<scheduler dir>.runs/run-<token>/` | A scheduled run's directory, with its cell's staging inside. |
| `$TMPDIR/hatchling-harness-*`, `$TMPDIR/hatchling-skill-run-*`, `$TMPDIR/hatchling-skill-code-*` | A coding harness's or a skill's run directory. Removed when the run ends; not swept yet. |
| `HATCHLING_BROKER_SOCKET`, `HATCHLING_API_SOCKET`, `HATCHLING_UI_SOCKET`, `HATCHLING_OPERATOR_SOCKET`, `HATCHLING_CHANNEL_INGRESS`, `HATCHLING_DEVICE_SOCKET` | The broker's sockets, made at each start. A stale one is replaced. |
| `<socket>.token` | The UI or operator socket's token, minted at each start. |
| `<broker socket>.lock` | The serving lock: a broker holds it for its life, so a second broker or `clean` refuses while it serves. |

## Give the agent's room disk more room

The room disk is the size it was made at (`HATCHLING_ROOM_DISK_SIZE`, 10 GiB by default), all of it reserved on the disk below. To give the agent more, grow it, with Hatchling stopped:

```bash
sudo systemctl stop hatchling
```

```bash
sudo hatchling-broker room-disk grow 20G
```

```bash
sudo systemctl start hatchling
```

`room-disk grow` takes a size as `HATCHLING_ROOM_DISK_SIZE` does (bytes, or a whole number with `M` or `G`), up to 1024G. It grows the image to that size, reserving the new space on the disk below, and keeps everything already on it. The file system inside grows to the new size when Hatchling starts and opens the disk. A room disk made before 0.9.0's reserving has all its space reserved when it's grown.

It refuses, before anything changes: while the room disk is open (stop Hatchling first), a size no larger than the disk is now (a room disk only grows), and a size the disk below hasn't room for, saying what's free and what it needs (the new space and 1 GiB to spare). A grow cut short, by a crash or a power cut, is finished when Hatchling next starts; if the disk below has no room for it by then, it's undone, and the room disk opens at the size it was. `HATCHLING_ROOM_DISK_SIZE` is only the size a new room disk is made at: changing it doesn't grow one that's there.

`sudo hatchling-broker doctor` says the room disk's size, what it holds and what's free in it while it's open, what it takes of the disk below and what's free there, and a grow under way. With less than a tenth of the disk free, it says how to grow it.

## Back up and restore

Rekey and tier enrollment require the room disk to be closed. If Room work
is active, maintenance refuses before changing keys, the keyring or sealed
stores. Stop Hatchling (`sudo systemctl stop hatchling` on a service
install), then retry the command. It confirms the decrypted mapping has
gone before starting the change and excludes new Room admission until it
finishes. This refusal does not cancel the running job or erase its files.

Back up every path under **Your data** while the broker is stopped, so no store is part-written. Back up each store whole, `.store-key` included. Keep the master key, or the keyring with what opens it, apart from the stores: the stores alone are ciphertext, and the key alone opens nothing that isn't there.

To restore, put each path back where its setting points, mode `0700` for directories and `0600` for files, and start the broker. Run as root, the broker gives the vault, its key and its keyring back to `hatchling-keeper` itself, and everything else it keeps back to `hatchling-broker`. A store from before a `rekey-vault` needs the key from before it.

## Uninstall

Stop the broker, run `hatchling-broker clean`, then delete every path set under **Your data** that Hatchling owns (not your projects under `HATCHLING_HARNESS_ROOTS`), and the binaries. Nothing else is left: Hatchling writes no files outside the paths you configure and `$TMPDIR`. (A delegated cgroup, `HATCHLING_CELL_CGROUP`, is kernel state, not files on disk; the broker removes each cell's.)

## On WSL2

Deleting files inside Linux doesn't shrink WSL2's virtual disk on Windows: it only grows. **Hatchling Tools → Free up disk space** compacts it: [Freeing disk space](windows.md#freeing-disk-space). To do it by hand, see [Disk usage](testing-on-wsl2.md#disk-usage).
