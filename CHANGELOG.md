# Changelog

Every user-visible change to Hatchling, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). Before 1.0, a minor version (0.x.0) may change the wire protocol or configuration; the entry says so when it does.

Each pull request adds its line under **Unreleased**. When a version is cut, that section becomes the version's entry and its release notes. What's planned for the next versions is on the [milestones](https://github.com/undergroundrap/hatchling/milestones), in the order set in [Order of work](docs/workorders/README.md#order-of-work).

## [Unreleased]

### Added

- `hatchling_sys::subid`, the first part of running the full broker without root (WO-014). It reads the owner's subordinate uid and gid ranges (`/etc/subuid`, `/etc/subgid`) and refuses one that holds uid 0, the owner's uid or any account's. It refuses map helpers that aren't regular, setuid files of root's. It writes a cell's maps through `newuidmap` and `newgidmap`. It leaves `setgroups` allowed, so a cell can drop the owner's supplementary groups (`deny` would make it keep them). Nothing uses it yet.
- **Cells on the owner's subordinate ids** (WO-014, part 2): with `HATCHLING_CELL_IDS=subordinate` and `HATCHLING_AGENT_UID` set to the first id of the owner's `/etc/subuid` range, an unprivileged broker gives every cell (agent, scheduled run, subagent, skill) that uid and the first of the gid range. The local API, the chat ingress, the device endpoint and the operator socket then start without root, since no cell has the broker's uid. A harness refuses to start this way until its work tree can be handed over without `chown` (part 2b). The refusals that find cells sharing the broker's uid now say how to give them their own.
  - `IsolationBackend::subordinate_identity` gives a cell its own uid and gid from the owner's ranges (`subid::cell_ids`, which says how to grant a range when there's none). It refuses a root broker, an unsafe range, and helpers it can't trust.
  - The spawner writes such a cell's maps through `newuidmap` and `newgidmap`, after checking the identity against the owner's ranges again. The cell then drops the owner's supplementary groups and checks that none is left before anything else runs; if it can't, it doesn't start.
  - A real-cell test reads, from the host, each of the cell's processes: its uid and gid (all four of each), its groups (none) and its maps.

### Changed

- **Breaking, for code built on `hatchling-broker`:** its `Error` gains `SocketPathNotOurs` and `TokenPathNotOurs` (WO-024), so the workspace moves to 0.5.0.
- **Breaking, for code built on `hatchling-isolation` or `hatchling-sys`** (WO-014):
  - `CellIdentity` gains `group` (the cell's own gid) and `mapping` (`Mapping::Direct` or `Mapping::Subordinate`); `CellIdentity::direct(principal, count)` builds the identity every cell had before.
  - `hatchling_sys::share_dir_with_cell` takes the cell's gid.
  - `subid::range_for` takes the account's name as an `Option`.
- **Every delete and signal stands behind two guards** (WO-024), after a mutant of WO-012's tree removal wiped a test machine. Each check is its own, so no one changed operator, or one bug, gets past both:
  - a stale socket is unlinked only when it's a socket, by its type and its mode, and the broker's user's; another user's socket now refuses the start, naming it;
  - a UI or operator token file is replaced only when it's a regular file of the broker's user; anything else now refuses the start, naming it;
  - the vault's KEK file is dropped only off the file tier and with no way in through it;
  - an expired memory item is deleted only when its own TTL agrees;
  - a cell's staging, and a run's, a skill run's and a skill's code directories, are removed through `remove_tree`, which never enters a mount or another user's directory;
  - `hatchling-dev` kills a timed-out process group before reaping its leader, and never after.

### Fixed

- **A chat message is answered with the chat prompt.** The local chat path started its run with the background worker's prompt; a chat turn now says so, typed from the chat path to the agent (`HATCHLING_RUN_CHAT`), and gets the chat prompt.
- **A batch of tool calls is checked whole before any of it runs.** An unknown tool or bad arguments later in a batch used to stop the loop after earlier calls in it had already run, unrecorded.
- **A reply the provider cut off isn't taken as finished.** A reply with `finish_reason` `length` (the output limit) or `content_filter` is now a model failure, not a final answer.
- **`pid_alive` no longer says a dead process is alive.** A process being reaped (state `X`) still answers a signal probe, and a zombie reaped between the probe and the read of its `/proc` stat left nothing to read; both counted as alive. Now neither does, and an unreadable stat asks the probe again, so a process hidden by `hidepid` still counts. The staging sweep relies on it, and a process-group test failed on it now and then.

## [0.4.0] - 2026-10-18

### Added

- `hatchling-broker clean [--dry-run]`: removes a dead broker's leftovers, a cell's staging directory (`$TMPDIR/hatchling-staging-<pid>-…`, whose broker is gone) and a run's directory (`<scheduler dir>.runs/run-…`), as each start now does too, and logs each. `--dry-run` lists them and removes nothing. It refuses while a broker serves the socket. It never touches your data: no symlink is followed, no mount is entered, and every configured state directory is kept whole, wherever it is. [What Hatchling keeps on disk](docs/storage.md) says which files are your data and which are disposable, and how to back up, restore and uninstall (WO-012).
- `internal:host:port` in `HATCHLING_EGRESS_ALLOWLIST` (and a coding CLI's vendor API): a destination the operator means to be on their own network, allowed to resolve to a private or loopback address; otherwise an ordinary credentialed HTTPS destination (WO-023).

- The Machine view, for whoever runs the machine: `hatchling-host`, given `HATCHLING_OPERATOR_SOCKET`, also serves `machine.html` and carries its channel to the broker's operator socket, printing a second address with the operator token. It shows the broker's version and uptime, the cells running, the connections open and the recent egress decisions, refreshed every two seconds, and metadata only. CI opens it in Chromium beside the app (WO-022, part 6).
- `HATCHLING_OPERATOR_SOCKET`: a socket for the people who run the machine, apart from its users. It speaks its own frames, so a user's socket can't answer an operator's query and the operator's can't take a user's action; it has its own token in `<socket>.token`; and its one query so far, `broker`, answers with the broker's version and uptime (WO-022, part 1).
- The operator's `cells` query: every cell running, oldest first, with its id, kind (agent, run, subagent, skill or harness), start and limits, and never its task, input or output (WO-022, part 2).
- The operator's `egress` query: the broker's last 200 egress decisions, with when, which cell, the destination and a verdict from a closed set, never a request's path, headers or body, or its answer (WO-022, part 4).
- The operator's `sessions` query: every API and UI connection open, oldest first, with its id, socket and when it was admitted, never what it said (WO-022, part 3).
- The README's "Using the app": build the UI, start `hatchling-host`, open the address it prints, and chat with the agent. CI runs it as written and checks the agent's answer on the page in Chromium (WO-021, closed).
- Chat: the broker answers each message sent on the local channel, through the API or the ingress, with an agent run in a fresh cell, given the message as its task, one at a time. Its grants are the API caller's, and it runs only when they include `channel-send:local` (WO-021, part 4).
- `HATCHLING_UI_SOCKET`: a second local API socket, for the frontend host to carry a browser's channel to. It serves the same protocol as `HATCHLING_API_SOCKET`, and every `hello` on it must also carry the UI token, which the broker mints at each start into `<socket>.token`, readable by its owner alone (WO-021, part 1).
- The frontend starts live when `hatchling-host` served it: the address the host prints carries the UI token as `#hatchling-ui=<token>`, which the app takes and clears from the address before its own routing reads it, then speaks to the broker over a WebSocket to the host's `/ui`, its hello carrying the token. Opened any other way, it runs its scripted demonstration as before. The app's main chat is the broker's `local` channel on the wire (WO-021, part 3).
- The local API reports the agent's status on the stream: *working* once a message is queued for it, with the message's first line as the detail, and *idle* when its reply arrives (WO-021, part 4).
- `hatchling-host`, the frontend host: it serves the built UI on `127.0.0.1` (`HATCHLING_HOST_PORT`, default 7340; `HATCHLING_HOST_UI` names the build) and carries the browser's WebSocket to the broker's UI socket (`HATCHLING_UI_SOCKET`), frame for frame. It prints the address to open, with the UI token in the URL's fragment. Its WebSocket frames are read strictly: whole, masked, bounded by the broker's own frame limit, text, close, ping and pong only. Its files are named only by plain segments and served only when regular, never through a symlink. A new crate (WO-021, part 2).

- [Testing code that deletes](docs/testing-destructive-code.md): how a mutant of the new removal code deleted a test machine's WSL home during WO-012, and the rules since: two independent guards per escape, tests that delete only what they made, a bubblewrap sandbox for the tests and mutation runs of such code, and canary files (WO-012).

### Changed

- **One broker per socket, and its helpers end with it.** A serving broker holds a lock beside its socket (`<socket>.lock`) for its life; a second broker on the same socket refuses to start. A run's or a skill's helper process now watches the broker that started it, by pidfd, and ends, with its cell, the moment that broker does: a broker killed with `SIGKILL` used to leave its helpers and their cells running until they ended on their own (WO-012).
- **The redaction audit log confirms no guesses.** A redaction is recorded by tags, HMAC-SHA-256 under a random key the broker makes at its first start with the gate on and seals under the master key in the redaction store (`raw/audit-tag-key`), instead of unkeyed SHA-256 hashes, which let anyone holding the log test a guess at a short secret or phrase. `redactions show` prints the tags, and an override names its flagged form by tag. Records and overrides from before keep their unkeyed hashes, and those overrides still apply; `show` marks such a record. `rekey-vault` reseals the key with the rest of the store, so tags stay stable. **Breaking, `hatchling-broker`:** `Judged` and `Override` carry `tag`/`flagged_tag`, with their hash fields optional; `output_gate::build` takes the master key. (WO-011)
- **The broker wipes its copies of held secrets.** The output and artifact gates read every held secret for each message or artifact they judge; each secret, and each form they look for (hex, base64, normalized), is now wiped when the gate is done with it. A fetched secret's reply, and each request frame (a stored secret's), are made at their final size and wiped once sent or handled. A test allocator scans every block these free for a canary secret in any of its forms. **Breaking, `hatchling-core`:** `VaultOp::Store` and `VaultResult::Fetched` carry their `secret_b64` as a `SecretB64`, which is wiped when dropped and never shown by `Debug`; the wire format is unchanged. **Breaking, `hatchling-broker`:** `held_secret_values` returns `Zeroizing` values. (WO-011)
- **Security keys can be made to ask for their PIN.** With `HATCHLING_REQUIRE_KEY_PIN=true`, `vault-keyring enroll-keys` and `add-key` enroll each key to ask for its PIN, as well as a touch, each time it opens the vault, and a hardware-key vault serves only if every key does (one that asks only for a touch is named; remove it and add it again). The key enforces it: under its PIN it answers from its verified-user secret (`CredRandomWithUV`), so a client that skips the PIN derives nothing that opens the vault, and the setting is bound into the key's wrap, so it can't be edited out of the keyring. `vault-keyring status` shows what each key asks. **Breaking, `hatchling-vault`:** `Factor::Fido2` and `KeyInfo` have a field more; the `Authenticator` trait has `hmac_secret_verified` (with a default), and `AuthenticatorError` a variant more. (WO-011)
- **Keyring edits are locked, and none rests on a stale read.** Every save takes an exclusive lock beside the keyring (`<keyring>.lock`) and writes only if the file is still what that keyring read, so two processes editing at once can't write back a spent backup code or TOTP step: the second is refused with "changed since it was read", and nothing is written. A failed save leaves the keyring as it was. (WO-011)
- **The vault wipes the copies of secrets it frees.** File reads are sized once, so a growing buffer never frees a partial copy; the master key file is read into a fixed buffer; `check` and `holds_envelopes` read only a file's first four bytes; the TOTP seed, the keyring's text, and typed and shown backup codes are wiped when dropped. A test allocator scans every block the vault frees for a canary secret while it migrates, checks, unlocks and recovers. A TOTP factor's `Debug` never shows its seed. (WO-011)
- **A cell killed by seccomp hands the host no core dump.** Every cell program is held to a core limit of one byte, soft and hard, before its filter goes on: the kernel aborts a dump to a crash handler on a pipe (apport, systemd-coredump) at exactly that limit, and writes no core file under a page. The filter refuses any change to the core limit from inside (reads of every limit still pass), and raising it takes a capability no cell holds. Before, a seccomp kill handed apport the cell's memory, any secret it fetched included, and hundreds of kills could exhaust a small machine. A broker started with a hard core limit of 0 now refuses to start cells, since 0 doesn't stop a pipe handler; raise it to at least 1. (WO-011)
- **`rekey-vault` rotates a keyring vault's key inside its keyring,** and takes no key file there. Every way in at the vault's tier is wrapped around a fresh key: the file tier's KEK, the TOTP tier's seed, and every enrolled security key, each plugged in and touched (one that doesn't answer is named, and nothing changes). Backup codes are replaced by a new set, printed once, and every store is re-sealed. A crash part-way leaves the vault refusing to serve until `rekey-vault` is run again, which finishes it. Without a keyring, a key file is still needed. (WO-003)
- **Raising the tier gives the vault a new key.** `vault-keyring enroll-totp` and `enroll-keys` rotate the key as `rekey-vault` does, re-seal every store, delete the file tier's KEK file and replace the backup codes, so a copy of the keyring, KEK file or codes from before opens nothing. **Breaking, `hatchling-vault`:** `confirm_totp`, `enroll_hardware_keys` and `enroll_single_hardware_key` return a `Rotated` for the caller to finish (`rekey` each store, then `Keyring::finish_rotation`); the keyring format is 3, which older builds refuse. **Breaking, `hatchling-broker`:** `run_rekey_vault` takes an optional key file, `run_keyring` the broker's `Config`, and `enroll_totp`, `enroll_keys` and `command` the store list. (WO-003)
- **`rekey-vault` re-seals every sealed store** (the secrets, schedules, artifacts, spaces and redaction records), where before it left all but the secrets under the old key, and prints a line per store. It checks every store first, so an old envelope or a plaintext file stops it before anything is touched. **Breaking, `hatchling-broker`:** `rekey_vault` takes the store list and returns a report per store. (WO-003)
- **A keyring vault started without its keyring refuses to serve,** where before it minted a fresh plain key and served with it. Adopting a keyring, and each keyring unlock, records it beside the key file; a start without `HATCHLING_VAULT_KEYRING` that finds that record, or finds sealed files and no key, stops and says to set it. A new vault still gets its key. `hatchling-vault` has `holds_envelopes`. (WO-003)
- **Breaking, the vault's files: run `hatchling-broker migrate-vault` after upgrading.** Every sealed file (the secrets, schedules, artifacts, spaces and redaction records) is now a version-2 envelope that authenticates its store and its name, so a file renamed or moved into another store fails to open, as a tampered one does. The broker refuses to start while any store holds an old envelope, naming the files; `migrate-vault` now re-seals every store, one file at a time (re-run it if it's interrupted), where before it sealed only the secrets' plaintext. **Breaking, `hatchling-vault`:** `migrate_plaintext` is `migrate`, and it, `rekey` and `SealedVault::open_in` take a `Store`. (WO-002)
- **The egress allowlist refuses internal addresses.** A destination (other than `local:`) that resolves at startup to a loopback, private, shared, link-local, unique-local, unspecified, multicast or cloud-metadata address now fails the allowlist, naming the entry and the address; an IPv4 address mapped into IPv6 counts as the IPv4 one. If one of yours is meant to be internal, write it `internal:host:port`. (WO-023)
- **Breaking, `hatchling-broker`:** `ApiConfig` has a `ui_socket` field. The workspace version is 0.4.0.

### Fixed

- **The frontend host serves only its own pages.** It refuses a request not addressed to `127.0.0.1:<port>` or `localhost:<port>`, and a WebSocket upgrade whose `Origin` isn't one of its own pages. Before, another site open in the browser could open the UI or operator channel (it never had the token, but it reached the broker's decoder and its connection limits), and a rebound name could fetch the UI's files.
- **A security key's PIN isn't shown as it's typed,** and is read into memory that's wiped. So are the broker's UI and operator tokens.
- **What a client or cell sends can't forge a line in the broker's log.** An error quoting a frame the broker couldn't parse is logged escaped.
- A skill run's directory in the temp dir has a random name, so another user can't make it first and block the run.
- The local API takes its socket's owner from the broker's own uid, rather than reading it back from the socket's path after binding (WO-021, part 1).
- What the broker tells a cell stays generic. A coding-CLI run that fails on the broker's side (its binary unreadable or not the pinned one, a file that can't be staged, the Codex host copy, the network set-up, the work tree unreadable) now reports a fixed sentence; the host paths, the broker's process id and the OS error stay in the broker's log. A malformed hello is rejected with a fixed sentence rather than the parser's message.
- Strings a cell or an API client chose are logged escaped, so a newline in one can't forge a log line: a refused egress destination, a malformed skill id, a refused delivery target. A skill id holding a control character names no skill.
- The operator's `redactions` review shows control characters, terminal escape sequences among them, as escapes: the text it prints was chosen outside.
- Memory names read back from the store are checked as names given are.
- A coding-CLI run's directory gets a name no one can guess, so another local user can't pre-create it to block the run.
- The coding-CLI relay's policy refusal says `403 Forbidden`, not `403 Service Unavailable`.

## [0.3.0] - 2026-10-16

The local-attacker release, first part. It closes what another local user could do to a running broker: a root broker that followed symlinks when it handed a cell its socket, set modes or made directories (WO-006), and agent sessions admitted by uid alone, so any process with the agent's uid could open one (WO-007). Every agent cell, the broker's own and each scheduled run's and subagent's, now reaches the broker over a connection made for it alone, and in cell mode no agent socket is bound. The broker and the agent must both be 0.3.0: a 0.2.0 agent can't use the connection a 0.3.0 cell gives it; the one `hatchling` binary is both. The broker also refuses `nobody` as the agent uid unless told otherwise. Rust API changes are marked **Breaking** below.

### Added

- `HATCHLING_AGENT_ALLOW_NOBODY`: the broker now refuses to start with `nobody` (65534) as `HATCHLING_AGENT_UID`, since every process running as nobody could act as the agent; this setting accepts it anyway (WO-007).

### Changed

- **Breaking:** the broker's agent cell now reaches the broker over a connection made for it alone, inherited at the descriptor `HATCHLING_CONNECTION_FD` names, and in cell mode the broker binds no agent socket at all. Before, any process with the agent's uid that could reach `HATCHLING_BROKER_SOCKET` could open a full agent session, and name any cell id in its hello; now the cell's session is the only one, and runs under the cell id the broker gave it. `HATCHLING_BROKER_SOCKET` is still read, and used when the broker serves without a cell. A scheduled run's and a subagent's cell get a connection of their own the same way: the helper that spawns the cell sends the broker its end over a channel given as the helper's stdin (taken off stdin first, so the cell inherits `/dev/null`), and no run socket is bound. With no run socket to fit a unix socket address, a scheduler directory may now be any length (WO-007, parts 2 and 3).
- **Breaking, Rust API:** `hatchling-isolation`'s `CellSpec` has a `connection` field and `Cell` a `connection` method, with `CONNECTION_FD_VAR`; `hatchling-agent`'s `Config` names its broker with `broker: BrokerAddress` instead of `broker_socket`; `hatchling-broker`'s `CellConfig` has `connected`, and `serve_cell_connection`, `serve_cell_forever` and `AgentCell` are new; `hatchling-sys` adds `keep_across_exec`, `take_stdin` and `net::inherited_unix_stream`; `hatchling-broker` drops `run_launcher::RUN_SOCKET_NAME`; `hatchling-core` adds `PeerCredentials::of_spawned`.
- **Breaking, `hatchling-sys`:** `chown_ids`, which changed ownership by path, is removed. `hand_over_socket` and `share_dir_with_cell` change ownership through handles opened without following symlinks, with `dir_share` and `owner_traverses` deciding what they do; `create_private_dir` makes a directory exclusively with mode 0700, `ensure_private_dir` makes or pins one, and `chmod_socket` sets a socket's mode through a handle. The workspace version is 0.3.0.

### Fixed

- A connection to the broker that stays silent for 10 seconds before its hello is complete is closed, freeing its connection slot; before, it held the slot until the peer went away (WO-007).
- The broker sets its listener's mode, and a scheduled run's directories and the vault's, without looking a path up again. The listener's mode goes through a handle opened without following symlinks; private directories are made exclusively and set to 0700 through a handle, and an existing run root is fixed through a handle. The ingress and device endpoints take their owner from the broker's own uid rather than reading it back from the socket's path. A skill's verified-code copy gets a name no one can guess, so another local user can't pre-create it to block the skill (WO-006).
- A root broker no longer gives its socket's directory to the cell's user. That user could swap the socket for a symlink between spawns, and the broker's next handover, which went by path, would give the cell's user whatever file the link named. The directory now stays the broker's, with the cell's group allowed to traverse it and nothing more (mode 0710); a directory an earlier version handed over is taken back. The socket and the directory are each changed through a handle opened without following symlinks, and the socket must be a socket itself (WO-006).
- A cell's staging directory can no longer be pre-created by another local user to block or redirect a spawn. Each broker process makes one private staging root in the temp directory (mode 0700, a random name, created exclusively), and each cell's directory inside it gets a random name too; a name already taken, by a directory or a symlink, is skipped and never followed (WO-006).

## [0.2.0] - 2026-10-15

The hostile-cell release. It closes the work orders the pre-launch review of the trust boundary found for a cell acting against its host: credential echoes and numeric allowlist forms (WO-009), vendor-side fetching (WO-008), schedule ownership (WO-010), socket families and inherited groups (WO-005), and per-cell resource limits (WO-004). It also makes the first five minutes tested: `--help` everywhere, a quickstart CI runs, and a hosted-model guide (WO-001), with an operator thinking-mode setting for models that need it (WO-020). The session protocol is v13, so the broker and the agent must both be 0.2.0; the one `hatchling` binary is both. Rust API changes are marked **Breaking** below.

### Added

- A hosted-model guide covers vault import, HTTPS destinations and grants, with OpenRouter as an OpenAI-compatible example; the first-run client also accepts a hosted `host:port` destination (WO-001, part 3).
- CI runs the README quickstart as written with the stand-in model and checks that the agent's answer arrives, so documentation changes cannot silently break the first run (WO-001, part 2).
- Every command now supports `--help` and `-h` before startup, with generated environment-variable inventories, including cell limits and cgroups. This covers `hatchling`, `hatchling-broker`, `hatchling-agent` and `hatchling-dev`; unknown arguments print usage to stderr and exit 64 without starting work (WO-001, part 1).

### Changed

- **Breaking, hatchling-agent and hatchling-broker:** operators can select `disabled` thinking with `HATCHLING_MODEL_REASONING` (direct runs) or `HATCHLING_SCHEDULER_MODEL_REASONING` (scheduled and child runs). Unset or `provider-default` preserves requests; endpoints may refuse the extension. Rust struct literals for agent `Config`, broker `SchedulerConfig` and `ScheduledRun` must supply `model_reasoning: ModelReasoning::ProviderDefault` to retain previous behavior; older serialized runs default only when the field is absent (WO-020).
- An operator-delegated `HATCHLING_CELL_CGROUP` now gives every cell its own cgroup v2 leaf, holding the whole cell to `pids.max`, `memory.max` (half the host's memory by default, configurable with `HATCHLING_CELL_MAX_MEMORY`) and `cpu.max` (`HATCHLING_CELL_MAX_CPUS`, fractional cores, unlimited by default). The first child enters before release, and cleanup kills all tasks and removes the leaf. Unset uses the documented rlimit fallback with a startup warning; configured but unusable fails closed. `CellSpec` and `CellConfig` gain `cgroup`; `Limits` and `CellLimits` gain `memory` and `cpu_millis`; `hatchling_sys::Error` gains `Cgroup` (breaking Rust struct construction and exhaustive matches; serialized configs retain defaults). See [cell limits](README.md#cell-limits) (WO-004).
- **Breaking, `hatchling-isolation`, `hatchling-sys` and `hatchling-broker`:** `CellSpec` gains `limits: Limits`, and `CellConfig` gains `limits: CellLimits`. `hatchling_sys::exec` takes a last step to run just before `execve`, and adds `hold_to` and `Limit`.
- A root broker maps one uid for a cell. `HATCHLING_CELL_UID_COUNT` above 1 is refused at startup: a wider map would let the cell act as every host account it covered. An unprivileged broker could only ever map one (WO-005).
- **Breaking, `hatchling-broker` and `hatchling-scheduler`:** `handle_schedule_op` and `list_schedules` take the caller's `Creator`, and `Schedule` gains `creator`. The schedule store reads an entry without one as the owner's.
- The session protocol is v13: `EgressOp::Stage` and `EgressOp::Read`, `EgressResult::Staged` and `EgressResult::Part`, `staged` on `EgressOp::Egress` and `more` on `EgressResult::Ok` (see [the session protocol](docs/session-protocol.md)). The broker and the agent must speak the same version, as always; the one `hatchling` binary is both. `hatchling-core` adds `fits_one_frame` and `EGRESS_PART_BYTES`.
- **Breaking, `hatchling-broker`:** `EgressConfig::allowlist_raw: String` is now `EgressConfig::allowlist: EgressAllowlist`, which holds the operator's text and its one shared resolution: build it with `EgressAllowlist::new(text)` (or `default()` for none), and read the text with `raw()`. The workspace version is 0.2.0. `hatchling-egress` adds `Proxy::shared`, a proxy over an allowlist already resolved, and `Proxy::allowlist`.
- The AGPL license gains three additional terms under its section 7, in `NOTICE.md`: a work built on Hatchling keeps the credit "Built on Hatchling by Ocean Bennett", a modified version is marked as modified and doesn't present itself as Hatchling, and the name and logo aren't licensed. `TRADEMARKS.md` explains, in plain words, what forks can and can't call themselves. Every crate's docs and crates.io page point to the terms. The license tag stays `AGPL-3.0-or-later`.

### Fixed

- TLS uses rustls 0.23.46 or newer, fixing four upstream security advisories and preventing selection of an older affected version. The crypto provider and TLS configuration are unchanged.
- A cell is held to hard limits: by default 4,096 processes (threads included), 8,192 open files and an 8 GiB largest file, set as both its soft and its hard limits just before the program starts, so it can't lift them. Set each with `HATCHLING_CELL_MAX_PROCESSES`, `HATCHLING_CELL_MAX_OPEN_FILES`, `HATCHLING_CELL_MAX_FILE_SIZE` and `HATCHLING_CELL_MAX_ADDRESS_SPACE` (bytes, unlimited by default), or `unlimited` to leave the broker's own; a value above the broker's own hard limit is held at the broker's. See [cell limits](README.md#cell-limits) (WO-004).
- A cell no longer outlives its supervisor when its program clears its own parent-death signal. Every cell's pid 1 is now Hatchling's init: it holds the death signal against the supervisor, never runs the program's code, reaps every child that exits, and ends with the program's exit code; the program runs as pid 2. When the supervisor dies, so does the init, and the kernel kills everything left in the cell. The init holds no capability, runs under `no_new_privs`, closes what it inherited, and can't be dumped or traced by the program it shares a uid with (WO-004).
- A coding CLI's cell may make a socket pair only of Unix sockets: `socketpair` with any other family is answered "address family not supported", as `socket` already was. It had been allowed whatever its family (WO-005).
- A root broker's cell drops its supplementary groups before entering its user namespace. It had carried the broker's groups in, where it couldn't drop them, so any file a group of the broker's could reach stayed reachable from the cell (WO-005).
- A cell can no longer see or cancel the owner's schedules. Each schedule records who made it: the owner, through the local API, or the agent, through a session. A cell lists and cancels only the agent's, and its cancel of the owner's is answered as for an id that doesn't exist. A schedule stored before this change counts as the owner's (WO-010).
- A local API caller with no grants is refused every action. Sending a message, invoking a skill and cancelling a schedule needed the owner's whole set of grants, which a caller with none held whenever the owner held none (WO-010).
- A cell can no longer have a model vendor fetch on its behalf. The egress proxy and the harness relay read every model request (chat completions, responses, messages, embeddings) before it leaves, and refuse content given by URL, a tool the vendor runs (web search, web fetch, code execution, remote tool servers, and any tool type not yet reviewed), and the vendor's own search; a body that isn't JSON, or names a key twice, is refused too. A known vendor is open to its model endpoints only, not its batch, file or assistant APIs. Ordinary requests pass unchanged. A harness set to use its vendor's web search gets those requests refused with a 403 that says why. See [the model request policy](docs/arch/trust-boundary.md#the-model-request-policy) (WO-008).
- A request or an answer through the egress proxy is no longer limited to what fits one 64 KiB session frame, about 48 KiB of body. A model conversation grows every turn, so a long one stopped working there. A body that doesn't fit now moves between the agent and the broker in 32 KiB parts, up to the proxy's 1 MiB bound each way; a 512 KiB request and a 512 KiB answer cross a session whole. Each message still fits one frame, and a session holds at most one request and one answer in transit (WO-009).
- The harness relay holds at most 64 MiB of request bodies per run, across all its connections. Before, a cell could open 16 connections, each declaring a 32 MiB body, and the relay set aside each body before reading it and copied it again for the vendor: about 1 GiB of the broker's memory per run. A request past the budget is refused with a 503, the body is sent to the vendor without a second copy, a chunk size large enough to wrap the running total is refused, and a connection's place among the 16 is given back however its thread ends (WO-009).
- The broker resolves the egress allowlist once, and every session shares it. Before, each new agent session parsed the allowlist and looked up every destination's addresses again, so a destination whose DNS changed while the broker ran could be reached at addresses the broker had never checked. Now the first resolution is pinned for the broker's life; one that fails pins nothing, and the next session tries again (WO-009).
- A credential an upstream echoes back (an error quoting the request, a debugging endpoint) no longer reaches the cell. The egress proxy and the harness relay overwrite the real key with `*` wherever it appears in a response's headers or body, keeping its length; the relay does it as the answer streams, through chunked framing, so a key split across reads or chunks is still found. Responses are requested uncompressed, the cell's own `Accept-Encoding` and `TE` are dropped, and a compressed answer is refused, since compression would hide an echo (WO-009).
- Egress destinations written as numbers the system resolver reads as IPv4 addresses (`2130706433`, `0x7f000001`, `0177.0.0.1`, `127.1`), or with an all-digit last label, are refused like IP literals: they named no host the allowlist could vouch for ([WO-009](docs/workorders/workorder-009-egress-correctness.md)).
- Two different egress destinations that would share a vault secret name (`api.example.com:443` and `api-example.com:443`) are refused at startup: one would have been sent the other's credential (WO-009).
- The startup check that keeps vault grants off egress credentials now maps `public:`, `local:` and `local-keyless:` allowlist entries without their prefix, as the credential lookup does; before, a vault grant could name the credential of a prefixed entry that wasn't also an egress grant.

## [0.1.2] - 2026-10-11

Publishing, documentation and metadata. No change to the broker's or the agent's behavior, or to the wire protocol.

### Added

- The module map shows how the crates connect: `hatchling-runtime` at the front, the trusted side (broker, vault, egress, isolation) and the cell side (agent, loop, model), and which crates are published. Every crate's crates.io page links to it.
- `hatchling-dev audit` fails the gate if a crate has no row in the module map, if the map names a crate that doesn't exist, or if `hatchling-agent` depends on the trusted side, directly or through another crate.
- Releases publish to crates.io from CI when a GitHub release is published, through trusted publishing: no crates.io token is stored anywhere. The workflow refuses a release whose tag, workspace version and changelog disagree.
- `cargo semver-checks` runs on every pull request against the crates on crates.io.
- crates.io and docs.rs badges, and `cargo install hatchling-runtime` as an install option in the quickstart.
- CONTRIBUTING says when new code earns its own crate, and what each published crate costs.
- `docs/releasing.md`: how a version is cut and published, the trusted-publishing settings, and a new crate's first publish. `scripts/publish-crates.sh` publishes in dependency order for both CI and the maintainer, skips crates already on crates.io at the version (so a failed run is simply re-run), and in CI skips and names crates new to crates.io.
- `scripts/setup-trusted-publishing.sh` makes every published crate trust the release workflow through crates.io's API, with a token scoped to `trusted-publishing` only, read without echoing; it skips crates already set up.
- crates.io pages: `hatchling-runtime` gets a README written for crates.io (the logo, why Hatchling is different, install, the security model in brief, links), and every other crate's page links to it and to the repository.

### Changed

- Keywords and categories are per crate, each describing what that crate does: `hatchling-runtime` carries the headline terms, and every other crate its own (model providers, tool-calling, cron, signed skills, seccomp, FIDO2 and so on), so each reaches a different search and leads back to Hatchling.

## [0.1.1] - 2026-10-11

The first release on crates.io. No change to the broker, the agent or the wire protocol.

### Added

- `hatchling-runtime`, the crate that installs one `hatchling` command: `hatchling broker` and `hatchling agent` run the broker and the agent with their existing command lines. Started as a cell's `/agent`, it is the agent, so `HATCHLING_AGENT_BIN` can point at it too ([WO-016](docs/workorders/workorder-016-publish-to-crates-io.md)).
- The shipped crates are published to crates.io; development and test crates are not.
- CI packages and verifies every published crate on each pull request, and runs the real-cell suites for the broker's spawner helpers through `hatchling` as well as `hatchling-broker`.
- This changelog, and a milestone per planned version: v0.2.0 (what a hostile cell can do), v0.3.0 (what a local attacker can do), v0.4.0 (what a security review asks for next).

### Changed

- Every crate shares the workspace version, and depends on the others through `[workspace.dependencies]`, so a release bumps one line.
- `cargo deny` exempts our own published crates from the license allowlist by name; a third-party AGPL dependency still fails it.

## [0.1.0] - 2026-10-11

The first public release. [Release notes](https://github.com/undergroundrap/hatchling/releases/tag/v0.1.0).

### Added

- Cells: user, mount, PID, network and IPC namespaces, a seccomp-BPF allowlist, and every capability dropped, around every agent. Hatchling refuses to start if the kernel can't build one.
- A vault sealed at rest (XChaCha20-Poly1305), unlocked by key file, authenticator codes or a FIDO2 security key, with backup codes.
- An egress proxy: no network by default, an exact-match allowlist per grant, TLS for remote hosts, and a surrogate swapped for the real key at the boundary.
- Any OpenAI-compatible model; Claude Code and Codex CLI, pinned and verified, inside cells.
- Scheduling, memory, goals and a digest feed; signed skills; artifacts inspected at the boundary; shared spaces.
- A local Unix-socket JSON API with a generated JSON Schema.
- A TypeScript frontend, running on scripted data.
- Dual licensing (AGPL-3.0-or-later and a commercial license) and a contributor license agreement.

### Known limitations

Published in [the trust boundary](docs/arch/trust-boundary.md#known-limitations), each with a work order and an issue.

[Unreleased]: https://github.com/undergroundrap/hatchling/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/undergroundrap/hatchling/releases/tag/v0.4.0
[0.3.0]: https://github.com/undergroundrap/hatchling/releases/tag/v0.3.0
[0.2.0]: https://github.com/undergroundrap/hatchling/releases/tag/v0.2.0
[0.1.2]: https://github.com/undergroundrap/hatchling/releases/tag/v0.1.2
[0.1.1]: https://github.com/undergroundrap/hatchling/releases/tag/v0.1.1
[0.1.0]: https://github.com/undergroundrap/hatchling/releases/tag/v0.1.0
