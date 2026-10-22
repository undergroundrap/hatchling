# What Hatchling keeps on disk

Hatchling writes two kinds of files: **your data**, which you back up and never delete by hand, and **disposable** files, which the broker makes and removes itself. If you're freeing space, delete only what this page marks disposable, or run `hatchling-broker clean`.

Every path below comes from a setting in the broker's environment (see `hatchling-broker --help`). Settings you haven't set write nothing.

## Your data

Deleting any of these loses credentials or history. Each lives where its setting points, readable by the broker's user alone.

| Setting | What is there |
| --- | --- |
| `HATCHLING_VAULT_DIR` | The vault: every secret, sealed under the store's own key. Also `.access.log`, the record of each read. |
| `HATCHLING_VAULT_KEY_FILE` | The master key (default `<vault dir>.key/key`). It wraps each sealed store's own key, so without it, or the keyring below, nothing in any store can be read. Beside it, `keyring-in-use` records that the key lives in the keyring. |
| `HATCHLING_VAULT_KEYRING` | The keyring, when the master key is wrapped by a passphrase file, TOTP or security keys. `<keyring>.lock` beside it is the edit lock, made again when needed. |
| `HATCHLING_VAULT_KEK_FILE` | The key that opens the keyring at its file tier. |
| `HATCHLING_SKILLS_DIR` | The signed skills the broker loads. |
| `HATCHLING_SKILL_TRUST_ROOTS` | The skill-signing keys the broker trusts. |
| `HATCHLING_MEMORY_DIR` | Memory items. |
| `HATCHLING_GOALS_DIR` | Goals and their tracking log. |
| `HATCHLING_SCHEDULER_DIR` | Schedules, sealed. Beside it, `<scheduler dir>.activity` is the subagent activity log. (`<scheduler dir>.runs` is disposable; see below.) |
| `HATCHLING_ARTIFACT_DIR` | Built artifacts and their build records, sealed. |
| `HATCHLING_ARTIFACT_WORKSPACE` | What the agent delivered to your workspace. |
| `HATCHLING_SPACE_DIR` | Spaces, sealed. |
| `HATCHLING_WEB_GRANTS_DIR` | The sites you always allow the agent to read, sealed (WO-028). |
| `HATCHLING_BROWSER_PROFILES_DIR` | The browser's signed-in profiles: each one's sites, and its session as cookies, sealed (WO-030). |
| `HATCHLING_BROWSER_CREDENTIALS_DIR` | The sign-ins the broker fills for signed-in profiles: each one's profile, exact origin and label, and its username and password, sealed (WO-030). |
| `HATCHLING_HARNESS_ROOTS` | Your projects, where a coding harness may work. Hatchling doesn't own them: a harness run changes files there as you asked it to. |
| `HATCHLING_REDACTION_DIR` | The output gate's audit log (`audit.log`), your overrides (`overrides`), your categories (`categories`), and `raw/`: kept redacted messages and the audit log's tag key, sealed. |

Every sealed store (the vault, schedules, artifacts, spaces and `raw/`) holds `.store-key`: the store's own key, wrapped under the master key. The store's files are sealed under it, so a store without its `.store-key` can't be read, even with the master key. During a `rekey-vault`, `.store-key.next` holds the new one; a rerun finishes the change. The broker never loads the master key. When it starts, it starts the vault keeper (`hatchling-broker vault-keeper`, a process of its own), which loads the master key, unwraps each store's key, and hands the broker the keys of its stores other than the secrets. While the broker serves, it holds only those; the secrets store's key stays with the keeper, which reads the vault for the broker and records each read in `.access.log`. The vault also holds `keeper-released`, sealed like any secret: the names of the secrets cells have been given or have stored, which the output and artifact gates scan for. No grant may name it.

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

## Back up and restore

Back up every path under **Your data** while the broker is stopped, so no store is part-written. Back up each store whole, `.store-key` included. Keep the master key, or the keyring with what opens it, apart from the stores: the stores alone are ciphertext, and the key alone opens nothing that isn't there.

To restore, put each path back where its setting points, owned by the broker's user, mode `0700` for directories and `0600` for files, and start the broker. A store from before a `rekey-vault` needs the key from before it.

## Uninstall

Stop the broker, run `hatchling-broker clean`, then delete every path set under **Your data** that Hatchling owns (not your projects under `HATCHLING_HARNESS_ROOTS`), and the binaries. Nothing else is left: Hatchling writes no files outside the paths you configure and `$TMPDIR`. (A delegated cgroup, `HATCHLING_CELL_CGROUP`, is kernel state, not files on disk; the broker removes each cell's.)

## On WSL2

Deleting files inside Linux doesn't shrink WSL2's virtual disk on Windows: it only grows. See [Testing on WSL2](testing-on-wsl2.md#disk-usage) to compact it.
