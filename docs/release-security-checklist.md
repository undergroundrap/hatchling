# Release security checklist

Run before every release, in the release pull request, and record each item's result in its description. The categories are the ones easy to get right once and easy to regress: a new listener, a new field on the wire, a new way out. So most items start from what changed since the last release:

```bash
git diff --stat vPREVIOUS..HEAD -- crates
```

An item that finds something new either shows it holds to the rule below, with the test that proves it, or the release waits for a fix. When an item can be checked by a program, it moves into `hatchling-dev` and this list says so.

**The standing rule.** A security claim is only as verified as the realest component it was tested against. Tests against a stand-in prove the logic, not the protection. Any claim that something can never happen names what it was tested against: real keeper, real cell, real install. A claim tested only against a stand-in is recorded as unverified.

## 1. Every listener has a gate

Each place the broker accepts a connection, who may connect, and what proves it. A new bind in non-test code that isn't in this table fails this item. "The owner" is who `owner.rs` works out: `HATCHLING_OWNER_UID`, or for a root broker whoever ran `sudo` (`SUDO_UID`), or else the broker's own user. Each socket is made for that user and given to it.

```bash
grep -rn "bind_socket(\|UnixListener::bind\|TcpListener::bind\|loopback_listener(\|OwnerOnlyListener::bind" crates --include=*.rs
```

| Listener | Where | Mode | Who may connect | Proof |
| --- | --- | --- | --- | --- |
| Agent socket (`HATCHLING_BROKER_SOCKET`) | `listener.rs` | 0600 | Without a cell only: the agent uid, by peer credentials. In cell mode it isn't bound. | `handshake.rs`, `a_connected_cells_agent_handshakes_over_its_own_connection` |
| A cell's connection | a socket pair from the spawn (`isolation`, `spawn.rs`) | none: no path | The cell it was made for; no other process holds an end. | `a_cell_and_its_owner_talk_over_the_cells_connection_alone`, `schedule_run.rs` |
| Local API (`HATCHLING_API_SOCKET`) | `api.rs` | 0600 | The owner, by peer credentials; never the agent's uid. | `api.rs` tests |
| UI socket (`HATCHLING_UI_SOCKET`) | `api.rs` | 0600 | The owner, with the UI token. A sign-in's password is refused here: the web app never takes a secret. | `the_ui_socket_is_served_beside_the_api_and_wants_its_token`, `the_ui_socket_refuses_a_password_and_the_api_socket_does_not` |
| Device socket (`HATCHLING_DEVICE_SOCKET`) | `device.rs` | 0600 | The owner; then a paired device's signature. | `devices.rs` |
| Ingress (`HATCHLING_CHANNEL_INGRESS`) | `ingress.rs` | 0600 | The owner, by peer credentials; refused when that's the agent's uid. | `ingress.rs` tests |
| Harness relay handoff | `harness.rs`, `network`, through `OwnerOnlyListener::bind`, in the run's directory | 0600, in a directory 0711 with a random name | The run's own spawn: whoever connects first hands in the relay listener, so the broker's user alone may. | `harness_relay.rs` tests |
| Browser relay handoff | `browser_cell.rs`, through `OwnerOnlyListener::bind`, in the cell's run directory | 0600, in a directory 0711 with a random name | The cell's own helper: whoever connects first hands in the browser's proxy listener, so the broker's user alone may. | `a_browser_cell_reaches_a_site_only_through_the_broker_and_refuses_a_bad_certificate` |
| A CLI cell's or a browser cell's relay port | inside the cell's own network namespace | none: loopback in the cell | The cell's own processes. | `a_cli_cell_hands_its_relay_listener_to_its_owner_once`, `a_browser_cell_reaches_a_site_only_through_the_broker_and_refuses_a_bad_certificate` |
| Operator socket (`HATCHLING_OPERATOR_SOCKET`) | `operator.rs` | 0600 | The owner, by peer credentials, with the operator token (`<socket>.token`, minted each start, 0600); never bound when the agent runs as the owner's uid. Answers are metadata only. | `the_socket_and_its_token_are_the_owners_and_never_the_agents_uid`, `a_hello_without_the_operator_token_is_closed_unanswered`, `a_peer_that_is_not_the_owner_gets_no_byte_read` |
| Unlock socket (`<vault>.unlock/unlock.sock`) | `unlock_relay.rs`, bound through `bind_socket`, only while a vault that needs a person waits at a start with no terminal | 0600, in a directory 0711 | Root alone, by peer credentials; only the keeper's user owns it. The app's host checks only that it's there, by name. | `only_a_socket_is_replaced_and_it_is_the_owners_alone`, `only_the_allowed_user_is_admitted_and_another_is_told`, `a_refused_peer_never_reaches_the_unlock` |
| Frontend host (`hatchling-host`, `HATCHLING_HOST_PORT`, default 7340) | `hatchling-host/src/lib.rs` | TCP, 127.0.0.1 only | Requests addressed to `127.0.0.1:<port>` or `localhost:<port>`, and WebSocket upgrades from its own pages (`Origin`), so no other site in the browser and no rebound name. It carries `/ui` and `/operator` to those sockets as the user it runs as, the owner on an install (`hatchling-host@.service` runs with `User=%i`), so there the token in the first frame is the gate. | `a_request_from_another_site_is_refused_and_reaches_no_socket`, `only_this_hosts_own_name_and_port_are_its_host_and_origin`, `the_host_binds_loopback_only`; the token: the UI and operator socket rows |

The frontend host is the only HTTP endpoint, on loopback. There are no metrics or debug endpoints. Adding one is a new row, with its gate.

## 2. Files and directories are made safely

- **No ownership or mode change by path:** `chown`, `chmod` or `set_permissions` on a path the broker didn't just open without following symlinks. Use `hatchling_sys`'s handle-based helpers (`create_private_dir`, `ensure_private_dir`, `chmod_dir`, `chmod_socket`, `hand_over_socket`, `share_dir_with_cell`, `chown_tree`), or `File::set_permissions` on the file just made, which is what the two hits in `harness.rs` and `skill_cell.rs` are. The one left: `hatchling-memory`'s `owner_dir.rs` repairs a directory's mode by path, after checking it isn't a link, in a directory only the broker's user can write, and that's a known limitation.
- **No predictable names in a shared directory:** anything the broker makes under the temp directory gets a random name and is created exclusively.
- **Deletes and signals stand behind two guards** (WO-024, [Testing code that deletes](testing-destructive-code.md)): a recursive delete goes through `hatchling_sys::remove_tree` or `remove_dir_tree`; any other delete, and any group signal, has two independent checks, each with its own test. A new `remove_dir_all`, `remove_file` of a computed path, or `kill` in non-test code fails this item unless it shows both.
  - **One file:** `hatchling_sys::remove_file_of`. It works through a handle on the directory, opened without following a link, and removes the name only if it is the kind asked for and belongs to one of the owners named.
  - **A store a service user owns, cleared by root:** `remove_dir_tree` keeps to the effective uid, and on an install no store is root's, so a `reset` or a `restore` would be refused all of them. `backup.rs` (`clear_tree`) uses `remove_dir_tree_of` for the tree's owner, once that owner is one `owners` names: Hatchling's own users (`broker_user::own_users`), the agent's uid, or a uid in the cells' range, held to 65,536 of them, since a crash can leave a run's tree a cell owns. Each is checked alone, and anything else stops the clear and stays.
- **These deletes stay as they are, each for its reason.** Each was read against the code on 2026-11-07; a new one is a new line, with a reason that's true of it. `remove_file` unlinks a name, so none of them can reach what a link points at.
  - **A confirmed erase:** `backup.rs`, `erase_keys`, and `key_seen.rs`, `erase`, which only it calls. Only `reset everything` runs them, once `erase everything` is typed or `--yes` is on root's own command line. They take what opens the vault because that's what was asked for.
  - **Fixed names, Hatchling's own files in its own directory:**
    - `key_seen.rs`, `set_reminders`: `<keyring>.no-reminders`, which `vault-keyring reminders-off` made, beside the keyring.
    - `key_setup.rs`, `recovery_kit`: `<keyring>.codes-unconfirmed`, which it made itself, once the last code is typed back.
    - `model_choice.rs`, `set` and `switch`: `model.json` and `server.json` in the scheduler's owner directory, by the app's owner, as the broker's user and never as root.
    - `unlock_relay.rs`, `master_key_for_start`: `unlock.sock` in `<vault>.unlock`, as the keeper's user, the socket it has just finished with.
  - **A socket or token at a path the operator set, checked before it goes:** `listener.rs`, `bind_socket_as`, a stale socket, and `api_auth.rs`, `clear_old_token`, an old token. Each checks the type twice and the owner, each with its own test. `remove_file_of` would refuse a configured path whose directory is a link, so a second start would fail on it.
  - **A socket this start has just bound:** `device.rs`, `open_device_endpoint`, and `ingress.rs`, `open_ingress`, each once it finds the socket would serve the agent's uid.
  - **A temp file the same function made, exclusively, after a write that failed or finished:** `hatchling-vault`'s `vault.rs`, `store_raw` and `write_dotfile`, and `keyring.rs`, `write_atomically`, and `hatchling-memory`'s `owner_dir.rs`, `write_atomic`.
  - **A cell spec in a run's own directory:** `run_launcher.rs`, `forget_spec`, for a helper that never started, and `take_spec`, by the helper, once it has checked the file is its own user's, owner-only and not a link.
  - **An item named by its validated id:** `hatchling-memory`'s `store.rs`, `recall`, `recall_by_tag` and `remove`, a file named by a `MemoryId` in its scope's directory, and `purge_expired`, regular files with an item's suffix, walked in that store, each read as an item first. `hatchling-vault`'s `vault.rs`, `remove`, a file named by a `SecretName`, as the vault's own op.
  - **`remove_dir`, which only ever takes an empty directory:** `doctor.rs`, `check_cells_in`, the probe's own randomly named stage; `run_launcher.rs`, `make_run_dir`, what it has just made, when it can't be held; `hatchling-isolation`'s `spawn.rs`, `enter_in` and `leave_in`, this process's staging root.
  - **cgroup directories:** `hatchling-sys`'s `cgroup.rs`, `finish` and `remove_descendants`, in the leaf this process made, once `cgroup.kill` has emptied it. They take directories alone, and a cgroup directory holds no files of anyone's.
  - **Dev tools, never in a release:** `hatchling-dev`'s `mutants.rs` and `mutant_cache.rs`, their own `mutants.out/outcomes.json`, and `hatchling-adversarial`'s harness fixture, which runs in a cell.

```bash
grep -rn "set_permissions\|fs::chown\|lchown\|temp_dir()" crates/*/src --include=*.rs
```

## 3. What crosses the wire stays generic

No error text the broker sends to a cell or an API client carries a host path, an OS error string, a crate or type name, or `Debug` output. The detail goes to the broker's log; the wire gets a typed result or a fixed sentence.

```bash
grep -rn "to_string()\|format!(\"{e\|format!(\"{err\|format!(\"{error" crates/hatchling-broker/src --include=*.rs
```

Check each hit that flows into a result, a reject reason or a report a session receives.

## 4. Input is checked at the boundary

- **Names that become paths or store keys** come through an allowlist type (`CellId`, `SecretName`, `GoalId`, the space and workspace names, ...), never a raw string. A new field on a session or API message that names something uses one.
- **Untrusted strings in log lines** are written with `?` (escaped), never `%`, so a newline can't forge a line. The activity and audit logs are JSON.

```bash
grep -rnE "= %|[(,] *%[a-z_]" crates/hatchling-broker/src crates/hatchling-host/src --include=*.rs
```

Check each `%` field, the shorthand `%name` too, for a value a cell or a client chose: an error that quotes a frame it couldn't parse is one (serde names the unknown field or variant). Each `?` instead is proved by a test that sends a newline and finds no forged line (`crate::test_log`).

## 5. Secrets stay secret

- Types holding a secret, key or token redact their `Debug`, and no log line prints one.
- Every new secret-bearing type is zeroized where it's dropped.
- **What leaves the machine is checked by the keeper**, against every secret it holds, a cell having had them or not (`held_check.rs`, `Keeper::contains`): what a harness is given or hands back, a web read's address, a search, a page's address, and text the agent types into a page. An address is checked as a site reads it, percent escapes undone. The broker gets one boolean for a whole text, and never a value. A check is an oracle, so each is a line in the vault's access log, and there's one for each thing sent, never one for a frame. The line says what the check was for, one word of a fixed set (`CheckFor`: `harness-check`, `web-check`, `search-check`, `typed-check`), and, when the broker made the connection for the cell, the cell whose run it was. Neither comes from a cell: a session on the agent socket names itself, so it has no cell on the line. A new way to ask one is a change to this item. Proof, against the real keeper: `crates/hatchling-broker/tests/harness_keeper.rs`, `crates/hatchling-broker/tests/leaving_keeper.rs`.
- **A hit ends the tree, there and then** (`held_check.rs`, `HeldHit`): every cell of the run is killed from wherever the hit is recorded, through its pidfd or the spawner's. A run cell, a harness's cell and a skill's are held by their tree as they start. A browser cell isn't: it ends with its session, which a killed cell ends. Nor is a builder skill's: it runs inside one op, to its own time bound. Proof, with a real keeper, the real spawner and real cells: `a_hit_kills_the_cell_at_once_and_its_vault_channel_with_it`, `a_scheduled_run_that_sends_a_key_is_stopped_and_told_once`, `a_child_that_sends_a_key_ends_the_whole_tree_at_once`, `a_hit_kills_the_brokers_long_lived_cell_and_its_vault_channel`, `a_harness_that_hands_back_a_key_ends_the_session`.
- **The sign-in fence has never run joined when it refuses.** A fill has three checks, and each refusal is proven on its own. The broker's: `sign_in` in `browser_ops.rs` asks for a sign-in of the browser's profile on the page's exact origin, or asks for nothing (`browser_ops.rs` tests, with the browser and the keeper standing in). The keeper's: `answer_fill` refuses another profile's or another site's (`the_keeper_holds_the_sign_ins_and_hands_the_broker_no_key_to_them`: a real keeper, the browser's channel standing in). The controller's: it types only on the top frame's real origin (`crates/hatchling-browser/tests/real_cell.rs`, `kept_sign_in_filled`: real Chromium in a real cell, the keeper standing in). Only the success case is end to end (`scripts/browser_end_to_end.py`: a real broker, keeper, cell, Chromium and site). So by the standing rule, a refusal by the real keeper of a real browser's second origin is unverified: an honest broker never asks the keeper for one, so that script can't reach it. A proof needs a broker made to ask, as a compromised one would: a test holding the real keeper and a real browser cell through the real spawner, which asks `Keeper::fill` for another origin and sees the site get nothing, run where Chromium is pinned.
- **The output, artifact and subagent gates look only for what a cell has had** (`held_secrets`): no cell can hold a vault secret now, and their text goes to the owner or stays local.
- Credentials never enter an agent's or a harness's cell: the egress swap and the harness relay attach them at the boundary. The one cell given one is a browser started for a signed-in profile: its cookies are restored over its connection, and the vault keeper fills a sign-in on its vault channel (WO-030, WO-031). The agent driving it never sees either: snapshots hide what was filled, and the agent names fields by id.

## 6. The cell's surface is what was reviewed

- **The seccomp allowlist:** any syscall added since the last release has a reason in the change that added it.
- **What a cell inherits:** only stdio, plus its connection or relay where it has one, and its vault channel (`HATCHLING_VAULT_FD`) when its session holds vault grants or its browser fills sign-ins. `schedule_run.rs` checks a run's open fds and stdin; the isolation layer closes every other descriptor at exec.
- **What a cell sees:** no new mount or environment variable that names a host path or a secret. A harness manifest's `env` values are the operator's, as its binary is, and reach the CLI's cell as written ([Harness manifests](harness-manifests.md)).

```bash
git diff vPREVIOUS..HEAD -- crates/hatchling-sys/src/seccomp.rs crates/hatchling-isolation crates/hatchling-broker/src/cell.rs
```

## 7. Egress has one way out

Every outbound connection is one of these, to a destination someone allowed, connected only to the addresses checked. A new outbound path is a new row in this item.

| Path | Runs in | Who allows a destination | Checks |
| --- | --- | --- | --- |
| Egress proxy | the egress authority | the operator's allowlist | an internal address refused unless the operator marked it `internal:`; credentials attached here |
| Harness relay | the egress authority | the harness's one vendor API | the surrogate swapped for the key here; anything else refused |
| Web search | the egress authority | fixed: the provider's host | resolved and pinned per search; the key only in the request's wiped buffer |
| Web fetch (`web_fetch`) | the broker | the owner, per site; the operator's refuse list | strict URL, no IP literal; an internal address refused before connecting; one bounded `GET` with no credential |
| Doctor's model check | the operator's `hatchling-broker doctor` | the operator's settings: the model endpoint | plain `http` only, never `https`; one bounded `GET <endpoint>/models` with no credential; the model ids it prints escaped; once the model is found, one bounded `GET /api/ps` for its window and how it fits, and the model's file size, below. Before it, `scripts/install.sh` loads the model: one `POST /api/generate` to the same local endpoint, with no credential |
| The model's list (`model_choice.rs`, `available`) | the broker, when the owner reads or chooses the model in the app; `use-local`, as root, before it switches | the operator's settings: the model endpoint | plain `http` only, never `https`; one bounded `GET <endpoint>/models` with no credential, 1 s to connect and 3 s to answer; only ids that are names kept |
| The model's window (`model_window.rs`, `learned`) | the broker (`run_trail.rs`), when a session that called the model ends | the operator's settings: the model endpoint | plain `http` only, never `https`; one bounded `GET /api/ps` with no credential, 1 s to connect and 2 s to answer; only a number read from the answer; never asked by a cell, nor at a path a cell names |
| The model's file size (`model_window.rs`, `vouched`) | `hatchling-broker doctor` (`doctor.rs`, `with_window`), and `hatchling-broker model-fit`, as root, from `scripts/install.sh` | the operator's settings: the model endpoint; for `model-fit`, the endpoint it's given | plain `http` only, never `https`; asked only once `GET /api/ps` says the model is loaded; one bounded `GET /api/tags` with no credential; only a number read from the answer |
| The model as it stands (`model_choice.rs`, `state`) | the broker, when the owner reads the model in the app | the operator's settings: the model endpoint in use | plain `http` only, never `https`; one bounded `GET /api/ps` with no credential, 1 s to connect and 2 s to answer; only numbers read from the answer; never asked by a cell, nor at a path a cell names |
| Release download (`scripts/get.sh`, the `curl` of each release file) | the owner's shell, before anything is installed | `--from-url`, or by default this repository's GitHub releases | `curl --proto =https --tlsv1.2`, or `file://` for a copy on this machine; every file checked against the release's `SHA256SUMS` before anything is unpacked. `SHA256SUMS` comes from the same place, so it catches a damaged download, not a replaced release: `gh attestation verify` does, and is offered, not required |
| Ubuntu's root filesystem (`installer/windows/setup.ps1`) | Windows' `wsl.exe`, as the owner, when they run the setup and its distro isn't there yet | fixed: Ubuntu, from the list of distributions Microsoft's WSL offers; the setup says so first | `wsl.exe --install Ubuntu --name $Distro --no-launch`: what's checked of the download is `wsl.exe`'s own. **Nothing is pinned here:** the setup reads its exit code, then that the distro runs systemd. It's the base the release is installed on, so it's as trusted as Microsoft's WSL and that list |
| Ollama's list at install (`scripts/get.sh`, `installer/windows/setup.ps1`) | the owner's shell; in the Windows setup, the Hatchling distro | `get.sh`: the model endpoint it was given, or the local default; `setup.ps1`: fixed, `http://localhost:11434` | one `GET /api/tags` with no credential, 5 s in `get.sh`, 3 s in `setup.ps1`; only a model's name looked for in the answer |
| Ollama's install (`installer/windows/setup.ps1`) | root, in the Hatchling distro, after the owner's yes | fixed: `ollama.com`, and the distro's package sources for `zstd` | `curl -fsSL https://ollama.com/install.sh \| sh` as root. **Unpinned:** no hash, so whatever that site serves that day runs as root in the distro. What it holds to is the distro alone: its Windows drives aren't mounted. `scripts/get.sh` never runs this: it says how, and stops |
| A model's download (`scripts/get.sh`, `installer/windows/setup.ps1`) | Ollama, asked from the owner's shell or as root in the distro, after a yes or with the setup's default model | the model's name, in Ollama's own registry | `ollama pull`: Ollama checks what it downloads against the registry's digests. No digest is pinned here, and a model is data to the model server, not code Hatchling runs |
| Browser proxy | the broker | the owner, per site of the session | `https` on 443 only; the site checked before any lookup; an internal address refused; connects only to the addresses checked; TLS end to end, so the browser checks the certificate |

Web fetch and the browser proxy run in the broker, not the egress authority, so the owner's site grants bound them only while the broker is sound: a compromised broker can reach any site through them. The install rows run before Hatchling does, under no allowlist of its own: the owner's yes and what each row says is all that stands there.

## 8. Dependencies

`cargo deny` is green (it runs in `check.sh`), and every new dependency has an entry in [dependency review](dependency-review.md).

## 9. The known limitations are current

The trust boundary's [known limitations](arch/trust-boundary.md#known-limitations) match what this release still lets through: a closed one is removed, and anything new this checklist found and didn't fix is added, with its work order.
