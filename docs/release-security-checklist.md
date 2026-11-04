# Release security checklist

Run before every release, in the release pull request, and record each item's result in its description. The categories are the ones easy to get right once and easy to regress: a new listener, a new field on the wire, a new way out. So most items start from what changed since the last release:

```bash
git diff --stat vPREVIOUS..HEAD -- crates
```

An item that finds something new either shows it holds to the rule below, with the test that proves it, or the release waits for a fix. When an item can be checked by a program, it moves into `hatchling-dev` and this list says so.

## 1. Every listener has a gate

Each place the broker accepts a connection, who may connect, and what proves it. A new bind in non-test code that isn't in this table fails this item.

```bash
grep -rn "bind_socket(\|UnixListener::bind\|TcpListener::bind\|loopback_listener(" crates --include=*.rs
```

| Listener | Where | Mode | Who may connect | Proof |
| --- | --- | --- | --- | --- |
| Agent socket (`HATCHLING_BROKER_SOCKET`) | `listener.rs` | 0600 | Without a cell only: the agent uid, by peer credentials. In cell mode it isn't bound. | `handshake.rs`, `a_connected_cells_agent_handshakes_over_its_own_connection` |
| A cell's connection | a socket pair from the spawn (`isolation`, `spawn.rs`) | none: no path | The cell it was made for; no other process holds an end. | `a_cell_and_its_owner_talk_over_the_cells_connection_alone`, `schedule_run.rs` |
| Local API (`HATCHLING_API_SOCKET`) | `api.rs` | 0600 | The broker's own uid, by peer credentials; never the agent's. | `api.rs` tests |
| UI socket (`HATCHLING_UI_SOCKET`) | `api.rs` | 0600 | The broker's own uid, with the UI token. | `the_ui_socket_is_served_beside_the_api_and_wants_its_token` |
| Device socket (`HATCHLING_DEVICE_SOCKET`) | `device.rs` | 0600 | The broker's own uid; then a paired device's signature. | `devices.rs` |
| Ingress (`HATCHLING_CHANNEL_INGRESS`) | `ingress.rs` | 0600 | The broker's own uid, by peer credentials. | `ingress.rs` tests |
| Harness relay handoff | `harness.rs`, in the run's directory | directory 0700 | The run's own spawn. | `harness_relay.rs` tests |
| A CLI cell's relay port | inside the cell's own network namespace | none: loopback in the cell | The cell's own processes. | `a_cli_cell_hands_its_relay_listener_to_its_owner_once` |
| Operator socket (`HATCHLING_OPERATOR_SOCKET`) | `operator.rs` | 0600 | The broker's own uid, by peer credentials, with the operator token (`<socket>.token`, minted each start, 0600); never bound when the agent runs as the broker's uid. Answers are metadata only. | `the_socket_and_its_token_are_the_owners_and_never_the_agents_uid`, `a_hello_without_the_operator_token_is_closed_unanswered`, `a_peer_that_is_not_the_owner_gets_no_byte_read` |
| Frontend host (`hatchling-host`, `HATCHLING_HOST_PORT`, default 7340) | `hatchling-host/src/lib.rs` | TCP, 127.0.0.1 only | Requests addressed to `127.0.0.1:<port>` or `localhost:<port>`, and WebSocket upgrades from its own pages (`Origin`), so no other site in the browser and no rebound name. It carries `/ui` and `/operator` to those sockets as the broker's uid, so there the token in the first frame is the gate. | `a_request_from_another_site_is_refused_and_reaches_no_socket`, `only_this_hosts_own_name_and_port_are_its_host_and_origin`, `the_host_binds_loopback_only`; the token: the UI and operator socket rows |

The frontend host is the only HTTP endpoint, on loopback. There are no metrics or debug endpoints. Adding one is a new row, with its gate.

## 2. Files and directories are made safely

- **No ownership or mode change by path:** `chown`, `chmod` or `set_permissions` on a path the broker didn't just open without following symlinks. Use `hatchling_sys`'s handle-based helpers (`create_private_dir`, `ensure_private_dir`, `chmod_socket`, `hand_over_socket`, `share_dir_with_cell`).
- **No predictable names in a shared directory:** anything the broker makes under the temp directory gets a random name and is created exclusively.
- **Deletes and signals stand behind two guards** (WO-024, [Testing code that deletes](testing-destructive-code.md)): a recursive delete goes through `hatchling_sys::remove_tree` or `remove_dir_tree`; any other delete, and any group signal, has two independent checks, each with its own test. A new `remove_dir_all`, `remove_file` of a computed path, or `kill` in non-test code fails this item unless it shows both.

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
| Doctor's model check | the operator's `hatchling-broker doctor` | the operator's settings: the model endpoint | plain `http` only, never `https`; one bounded `GET <endpoint>/models` with no credential; the model ids it prints escaped; once the model is found, one bounded `GET /api/ps` for its window |
| The model's window (`model_window.rs`) | the broker, when a session that called the model ends | the operator's settings: the model endpoint | plain `http` only, never `https`; one bounded `GET /api/ps` with no credential, 1 s to connect and 2 s to answer; only a number read from the answer; never asked by a cell, nor at a path a cell names |
| Browser proxy | the broker | the owner, per site of the session | `https` on 443 only; the site checked before any lookup; an internal address refused; connects only to the addresses checked; TLS end to end, so the browser checks the certificate |

The last two run in the broker, not the egress authority, so the owner's site grants bound them only while the broker is sound: a compromised broker can reach any site through them.

## 8. Dependencies

`cargo deny` is green (it runs in `check.sh`), and every new dependency has an entry in [dependency review](dependency-review.md).

## 9. The known limitations are current

The trust boundary's [known limitations](arch/trust-boundary.md#known-limitations) match what this release still lets through: a closed one is removed, and anything new this checklist found and didn't fix is added, with its work order.
