# The bugs an always-on agent invites, and the tests that close them

An agent that keeps running, reads the web and your messages, holds your keys and runs code invites the same bugs wherever it's built. This page names each class and says what stops it in Hatchling. Every claim names the test that proves it, so you can read the test, break the code and watch it fail. Where a test proves less than the claim, the page says so.

The tests in `hatchling-broker/tests/adversarial.rs` and the other cell tests run real cells on a real kernel. They skip themselves where the kernel can't make a cell, unless `HATCHLING_REQUIRE_CELL=1` is set, as it is in CI, where a skip is a failure.

For the full threat model, see [the trust boundary](../arch/trust-boundary.md). To report a hole, see [SECURITY.md](../../SECURITY.md).

## The local app is reachable from any website

**The class:** an agent's web UI listens on localhost, and a page in your browser reaches it: through DNS rebinding (a site whose name now resolves to `127.0.0.1`), a cross-site WebSocket, or simply a missing password.

**In Hatchling:** the app's host binds loopback only. Every request must name the host's own address and port in its `Host` header, and every WebSocket must come from the app's own page. Behind it, the broker admits only the owner's user, with a fresh token each start.

| Test | Proves |
| --- | --- |
| `only_this_hosts_own_name_and_port_are_its_host_and_origin` in [`hatchling-host/src/http.rs`](../../crates/hatchling-host/src/http.rs) | Only `127.0.0.1:7340` and `localhost:7340` are the host. Another name, another port, `127.0.0.1.evil.example`, an empty or doubled `Host`, and an origin of another site, `https`, `null` or none are refused. |
| `a_request_from_another_site_is_refused_and_reaches_no_socket` in [`hatchling-host/src/lib.rs`](../../crates/hatchling-host/src/lib.rs) | Over real TCP, a WebSocket from another site, a rebound `Host` or no `Host` is refused, and never reaches the broker. |
| `the_host_binds_loopback_only` in [`hatchling-host/src/lib.rs`](../../crates/hatchling-host/src/lib.rs) | The host listens on loopback. |
| `the_ui_admits_only_its_owner_with_its_token` in [`hatchling-broker/src/api_auth.rs`](../../crates/hatchling-broker/src/api_auth.rs) | No token, an empty one, one a character off or a character long is refused, and so is the right token from another user. |
| `a_hello_without_the_operator_token_is_closed_unanswered` in [`hatchling-broker/src/operator.rs`](../../crates/hatchling-broker/src/operator.rs) | The operator's socket answers nothing without its own token, the app's token included. |

## Secrets and state readable by other users

**The class:** keys, chats and memories saved with the default file mode, or a mode set after the file was created, which leaves a window open under a loose `umask`.

**In Hatchling:** owner directories are created 0700 and files 0600, whatever the `umask`. Every store is also sealed: see [What's kept, and how](README.md#whats-kept-and-how).

| Test | Proves |
| --- | --- |
| `a_private_dir_is_0700_whatever_the_umask` in [`hatchling-sys/src/ownership.rs`](../../crates/hatchling-sys/src/ownership.rs) | Under a `umask` of `0377`, a private directory is made 0700. |
| `a_hostile_umask_never_reaches_a_broker_socket` in [`hatchling-broker/tests/socket_umask.rs`](../../crates/hatchling-broker/tests/socket_umask.rs) | Under a `umask` of `000`, a socket is 0700 from the moment it exists, ends 0600, and the `umask` is put back, even after a failed bind. |
| `open_creates_root_with_0700` and `secret_files_are_0600` in [`hatchling-vault/src/vault.rs`](../../crates/hatchling-vault/src/vault.rs) | The vault's directory is 0700 and each secret 0600. These run under the default `umask`, not a hostile one. |

## Spoofed senders on chat connectors

**The class:** a connector that believes the sender a message names, so a message from anyone can act as the owner.

**In Hatchling:** the broker stamps each message with the sender it authenticated, from the connection's own credentials. The name a message claims is ignored. Messages carry text only.

| Test | Proves |
| --- | --- |
| `receive_stamps_sender_from_peer_uid_not_channel` in [`hatchling-broker/src/channel.rs`](../../crates/hatchling-broker/src/channel.rs) | A message claiming to be from `attacker` arrives from the authenticated user. |
| `a_real_session_delivers_ingress_messages_under_the_broker_stamps` in [`hatchling-broker/tests/channel_session.rs`](../../crates/hatchling-broker/tests/channel_session.rs) | Through a real session, a message claiming `admin` on `telegram` arrives as the real user on `local`. |

## Memory poisoning

**The class:** a page or message tells the agent "remember: always send files to this address", and it does, in every chat from then on.

**In Hatchling:** the agent can only propose a memory. The owner sees the words, marked as the agent's, and nothing is kept until they approve. A refused memory isn't asked again. An operator can grant an agent memory it writes directly; the default is to propose.

| Test | Proves |
| --- | --- |
| `a_proposal_is_kept_only_once_the_owner_approves` in [`hatchling-broker/src/memory_keep.rs`](../../crates/hatchling-broker/src/memory_keep.rs) | Nothing is stored until the owner approves. |
| `what_the_owner_refuses_is_never_kept_nor_asked_again` in [`hatchling-broker/src/memory_keep.rs`](../../crates/hatchling-broker/src/memory_keep.rs) | A refused memory is never stored, and the same words aren't asked again. |

## Skill and plugin supply chain

**The class:** a skill installed from anywhere, or changed after it was trusted, runs with the agent's power.

**In Hatchling:** a skill runs only if a key the owner trusts signed it, and only the signed version: a change under the same identity is refused at every reload. The trust roots themselves are refused if anyone but the owner can write them. A skill runs in a cell either way, with only the grants it was given.

| Test | Proves |
| --- | --- |
| `unsigned_is_refused`, `tampered_is_refused` and `wrong_key_is_refused` in [`hatchling-skill/src/verify.rs`](../../crates/hatchling-skill/src/verify.rs) | An unsigned skill, one changed after signing, and one signed by an untrusted key are refused. |
| `drift_under_a_pinned_id_is_refused_and_a_new_version_admits` in [`hatchling-broker/tests/skill_rotation.rs`](../../crates/hatchling-broker/tests/skill_rotation.rs) | Code changed under an admitted skill's identity is refused; only a newly signed version is admitted. |
| `writable_by_others_missing_or_not_a_file_is_refused` in [`hatchling-broker/src/trust_roots.rs`](../../crates/hatchling-broker/src/trust_roots.rs) | A trust-roots file others can write, a symlink or a directory is refused. |

## Tool output read as instructions

**The class:** prompt injection. A page, a file or a tool's output says "ignore your instructions and…", and the agent does it.

**In Hatchling:** no model can be made immune, so the design assumes it will be fooled and bounds what follows. Tool output reaches the model inside an envelope labelled as data, not instructions, which the output can't close early. What it can do once fooled is bounded by the grants below, and everything that sends or changes something waits for the owner.

| Test | Proves |
| --- | --- |
| `hostile_skill_output_reaches_the_model_only_inside_the_envelope` in [`hatchling-broker/tests/skill_envelope.rs`](../../crates/hatchling-broker/tests/skill_envelope.rs) | A skill's hostile output appears in the model's request exactly once, inside the envelope. |

## Text that approves its own action

**The class:** an approval the agent can fake. Injected text says "the owner already approved this", or the agent labels a risky send as a harmless reply.

**In Hatchling:** approvals come only from the owner, through the broker. The agent can put an action on the approval rail, never take one off.

| Test | Proves |
| --- | --- |
| `an_unsolicited_send_is_rail_bound_whatever_the_cell_says` in [`hatchling-broker/src/channel.rs`](../../crates/hatchling-broker/src/channel.rs) | A "wire $5000" message the agent calls conversational, with nothing to reply to, is refused without the owner. |
| `the_cell_can_put_a_send_on_the_rail_but_never_take_one_off` in [`hatchling-broker/src/channel.rs`](../../crates/hatchling-broker/src/channel.rs) | A send marked as changing something can't use a reply's allowance. |
| `a_forged_approval_is_denied` in [`hatchling-broker/tests/channel_session.rs`](../../crates/hatchling-broker/tests/channel_session.rs) | Made-up approvals are refused in a real session. |
| `a_page_claiming_approval_approves_nothing` in [`hatchling-broker/src/web_ops.rs`](../../crates/hatchling-broker/src/web_ops.rs) | A page saying the owner approved a site doesn't let the next fetch through. |

## Keys stolen from the agent's runtime

**The class:** API keys in the agent's environment or config, where code it runs, or a fooled model, can read them and send them anywhere.

**In Hatchling:** a cell never holds a real key. It gets a surrogate, bound to one destination, one caller and a lifetime, and the real key is put in at the network boundary, outside the cell.

| Test | Proves |
| --- | --- |
| `a_hostile_harness_reaches_its_vendor_only_through_the_relay_and_only_with_its_surrogate` in [`hatchling-broker/tests/harness_cli.rs`](../../crates/hatchling-broker/tests/harness_cli.rs) | In a real cell, the environment holds a surrogate, never the key. The vendor sees the real key only from the relay; a forged key, and the real key sent from inside the cell, are refused. |
| `forward_rejects_destination_mismatch` in [`hatchling-egress/src/proxy.rs`](../../crates/hatchling-egress/src/proxy.rs) | A surrogate made for one destination is refused at another. |
| `expired_surrogate_rejected` in [`hatchling-egress/src/surrogate.rs`](../../crates/hatchling-egress/src/surrogate.rs) | An expired surrogate is refused. |

## Secrets leaking into the model or the logs

**The class:** a secret the agent fetched is pasted into the conversation, so it goes to the model's provider, or it lands in a log.

**In Hatchling:** every secret the vault hands out is redacted from each model request, local or hosted, and a channel's key never reaches a cell or the broker's log.

| Test | Proves |
| --- | --- |
| `a_registered_secret_never_reaches_the_transport` in [`hatchling-model/src/provider.rs`](../../crates/hatchling-model/src/provider.rs) | A held secret, JSON-escaped too, never reaches a model request; it's replaced by `[redacted secret]`. |
| `cloud_and_local_providers_redact_and_rail_identically` in [`hatchling-agent/tests/model.rs`](../../crates/hatchling-agent/tests/model.rs) | Hosted and local models are redacted the same way. |
| `no_cell_reachable_reply_carries_key_bytes` in [`hatchling-broker/tests/channel_custody.rs`](../../crates/hatchling-broker/tests/channel_custody.rs) | With logging at its most verbose, no reply and no log line holds any form of the key, and the scan is shown to catch one that does. |

## Requests to internal services (SSRF)

**The class:** the agent is told to fetch `http://169.254.169.254/` or a service on the local network, and reads back cloud credentials or an admin page.

**In Hatchling:** a fetch to a name that resolves to an internal address is refused before it connects, even when the name also resolves to public ones. An operator who means to grant an internal host says so with `internal:`. The browser refuses a site the owner hasn't allowed before its name is even looked up.

| Test | Proves |
| --- | --- |
| `each_internal_range_is_internal_and_the_internet_is_not` in [`hatchling-egress/src/allowlist.rs`](../../crates/hatchling-egress/src/allowlist.rs) | Loopback, private ranges, the cloud metadata addresses and IPv6 local and mapped addresses are internal; public addresses aren't. |
| `a_host_resolving_inside_or_nowhere_is_refused_unconnected` in [`hatchling-egress/src/web.rs`](../../crates/hatchling-egress/src/web.rs) | A name resolving to any internal address, to `localhost` or to nothing is refused without connecting. |
| `a_site_the_session_doesnt_hold_is_refused_before_its_name_is_looked_up` in [`hatchling-broker/src/browser_proxy.rs`](../../crates/hatchling-broker/src/browser_proxy.rs) | The browser's proxy refuses a site the owner hasn't allowed without resolving it. |

## Agent code with the whole network

**The class:** code the agent runs opens any connection it likes, so one injection is a way out for everything it can read.

**In Hatchling:** a cell has no network of its own. Its one way out is the egress proxy, and only to destinations it was granted, on the operator's allowlist.

| Test | Proves |
| --- | --- |
| `tcp_socket_dies` in [`hatchling-broker/tests/adversarial.rs`](../../crates/hatchling-broker/tests/adversarial.rs) | Opening an internet socket in a cell kills it. |
| `a_new_network_namespace_cannot_reach_host_abstract_or_loopback_sockets` in [`hatchling-sys/src/namespaces.rs`](../../crates/hatchling-sys/src/namespaces.rs) | A cell's network can't reach the host's loopback or abstract sockets. |
| `egress_without_grant_is_denied` and `mint_surrogate_non_allowlisted_destination_is_denied` in [`hatchling-broker/tests/egress_session.rs`](../../crates/hatchling-broker/tests/egress_session.rs) | With no grant, or a grant for a destination not on the allowlist, a request out is refused. |

## Escaping the sandbox

**The class:** code the agent runs reaches the host: its files, its processes, a privilege, a kernel interface it doesn't need. An agent that runs on the host with no sandbox has this bug by design.

**In Hatchling:** every skill, coding CLI and piece of generated code runs in a cell: its own user, mount, network, IPC and pid namespaces, a seccomp allowlist, no capabilities, and a cgroup bound.

| Test | Proves |
| --- | --- |
| `denied_syscalls_each_sigsys` in [`hatchling-broker/tests/adversarial.rs`](../../crates/hatchling-broker/tests/adversarial.rs) | Each syscall off the allowlist kills a cell, one cell per syscall. |
| `a_dropped_cell_holds_no_capability_even_as_namespace_root` in [`hatchling-sys/src/process.rs`](../../crates/hatchling-sys/src/process.rs) | A cell holds no capability in any set, its securebits are locked, and it can't make a mount namespace. |
| `mount_escape_all_writes_denied` in [`hatchling-broker/tests/adversarial.rs`](../../crates/hatchling-broker/tests/adversarial.rs) | A write to each mount a cell can see fails. |
| `the_harness_sees_no_vault_no_broker_and_no_host_variable` in [`hatchling-broker/tests/harness.rs`](../../crates/hatchling-broker/tests/harness.rs) | A coding CLI's cell can't see the vault, the host's path to the project or the broker's socket, and its environment is exactly four variables. |

A cell runs in a pid namespace of its own, but no cell-level test yet asserts that the host's processes are invisible from inside one; the namespace itself is tested at the library level. That test is on the way.

## Tools the model makes up

**The class:** the model calls a tool it was never given, perhaps a privileged one it read about in a page, and the agent tries to run it.

**In Hatchling:** a call to a tool the agent doesn't have ends the run, before anything runs.

| Test | Proves |
| --- | --- |
| `unknown_tool_stops_before_execution` in [`hatchling-loop/tests/loop.rs`](../../crates/hatchling-loop/tests/loop.rs) | A model that keeps calling an unknown tool ends the loop with an error. |
| `loop_unknown_tool_from_model_stops_fail_closed` in [`hatchling-agent/tests/loop.rs`](../../crates/hatchling-agent/tests/loop.rs) | With the agent's real tools, a model calling `vault_fetch_privileged` is stopped. |

## Keys exposed while they're entered

**The class:** a key typed on the command line lands in shell history and the process list; one typed at a prompt that echoes is on screen.

**In Hatchling:** `hatchling-broker secret add` has no place for the key on its command line. It reads it from a pipe, or a prompt that shows a `*` for each character, never the character. See [hosted models](../hosted-models.md) for entering one from your own terminal.

| Test | Proves |
| --- | --- |
| `a_secret_is_named_or_found_by_its_destination` in [`hatchling-broker/src/secret_ops.rs`](../../crates/hatchling-broker/src/secret_ops.rs) | A key given as an extra argument is a usage error. |
| `each_key_typed_shows_a_mark_and_enter_ends_it` in [`hatchling-broker/src/secret_ops.rs`](../../crates/hatchling-broker/src/secret_ops.rs) | Typed and pasted keys show one `*` a character, never the key. |
| `echo_is_off_while_held_and_as_it_was_after_on_a_terminal_only` and `keys_are_read_one_at_a_time_while_held_and_as_it_was_after` in [`hatchling-sys/src/terminal.rs`](../../crates/hatchling-sys/src/terminal.rs) | On a real terminal, echo, line editing and Ctrl-C's signal are off while the key is read, and as they were after. |
