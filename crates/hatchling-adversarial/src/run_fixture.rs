//! `hatchling-run-fixture`: a test program for scheduled runs (-2).
//!
//! The broker's run launcher starts it in place of the agent, inside a
//! real run cell, as `run <task>`. It reports what the run can see and do
//! through its own connection, one line per fact, which the dispatcher
//! then delivers like any run's output:
//!
//! - `missed=<0|1> due=<secs>`: the broker-asserted run flags.
//! - `depth=<n> event=<source>`: the run's follow-up depth and, for a
//!   hook run, the event that fired it, as the broker set them.
//! - `fds=<list>`: the fds open at start: only 0, 1 and 2, and the run's
//!   connection, shown as `c`.
//! - `sock=<visible|absent>`: whether `/sock` (where a broker socket
//!   would be mounted) is there.
//! - `stdin=<socket|not-socket>`: whether stdin is a socket (it must not
//!   be: the helper's channel to the broker stays the helper's).
//! - `cell=<id> steps=<n> subagent=<0|1>`: the run's cell id and, for a
//!   subagent, the step budget and mode the broker set.
//!
//! Each word of the task is then one probe, run in order:
//!
//! - `vault:<name>`: fetch `name` through the session, printing the
//!   broker's reply.
//! - `channel:<id>`: receive on `id`.
//! - `schedule`: try to create a schedule.
//! - `followup:<secs>`: ask for this run's follow-up, within its grants.
//! - `followup-pump`: ask for a follow-up holding a vault grant the run
//!   was not given.
//! - `hook`: try to register a hook.
//! - `space:<id>:<key>`: join the space `id`, then read its item `key`
//!   with that admission, printing the read's reply, or the join's if it
//!   was refused.
//! - `space-create`: try to create a space.
//! - `connect:<path>`: try to connect to a unix socket at `path`.
//! - `subagent`: try to spawn a child of its own.
//! - `model-reasoning`: report the thinking-mode setting in the cell environment.
//! - `hang`: spin forever (the cell's seccomp profile has no `nanosleep`).
//!
//! Two tasks replace the report entirely:
//!
//! - `noconnect`: exit 0 at once, never connecting to the broker.
//! - `flood:<n>`: write exactly `n` bytes of output, then exit 0.
//! - `envelope:<answer>`: answer as a child agent would: print
//!   its typed envelope, finished with `answer` in 2 steps, and exit 0.
//!
//! Dev-only, like the probe: never linked into the runtime.

use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, ChannelOp, ChildEnvelope, ChildVerdict, HookOp, HookSource, PROTOCOL_VERSION,
    ScheduleGrant, ScheduleOp, ScheduleSpec, SessionRequest, SessionResponse, SpaceOp, SpaceResult,
    SubagentOp, VaultOp, VaultResult,
};

fn main() -> ExitCode {
    let task = std::env::args().skip(2).collect::<Vec<_>>().join(" ");
    let mut out = std::io::stdout().lock();
    if task == "noconnect" {
        return ExitCode::SUCCESS;
    }
    // A chat turn's view of the conversation (WO-026): what it was given,
    // as given, without connecting.
    if task.starts_with("history") {
        let env = |key: &str| std::env::var(key).unwrap_or_default();
        let line = format!(
            "chat={} history={}",
            env("HATCHLING_RUN_CHAT"),
            env("HATCHLING_RUN_HISTORY")
        );
        return if writeln!(out, "{line}").and_then(|()| out.flush()).is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        };
    }
    if let Some(answer) = task.strip_prefix("envelope:") {
        let envelope = ChildEnvelope::Done {
            verdict: ChildVerdict::Ok,
            answer: answer.to_owned(),
            steps: 2,
        };
        let Ok(json) = serde_json::to_string(&envelope) else {
            return ExitCode::from(2);
        };
        return if writeln!(out, "{json}").and_then(|()| out.flush()).is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        };
    }
    if let Some(n) = task
        .strip_prefix("flood:")
        .and_then(|n| n.parse::<usize>().ok())
    {
        return if out
            .write_all(&vec![b'y'; n])
            .and_then(|()| out.flush())
            .is_ok()
        {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(2)
        };
    }
    let env = |key: &str| std::env::var(key).unwrap_or_default();
    let connection = env("HATCHLING_CONNECTION_FD");
    let fds: Vec<String> = hatchling_sys::open_fds(1024)
        .iter()
        .map(|fd| {
            if fd.to_string() == connection {
                String::from("c")
            } else {
                fd.to_string()
            }
        })
        .collect();
    let sock = if std::path::Path::new("/sock").exists() {
        "visible"
    } else {
        "absent"
    };
    let mut lines = vec![
        format!(
            "missed={} due={}",
            env("HATCHLING_RUN_MISSED"),
            env("HATCHLING_RUN_DUE")
        ),
        format!(
            "depth={} event={}",
            env("HATCHLING_RUN_DEPTH"),
            env("HATCHLING_RUN_EVENT")
        ),
        format!("fds={}", fds.join(",")),
        format!("sock={sock}"),
        format!("stdin={}", socket_kind(std::io::stdin().as_fd())),
        format!(
            "cell={} steps={} subagent={}",
            env("HATCHLING_CELL_ID"),
            env("HATCHLING_MODEL_MAX_STEPS"),
            env("HATCHLING_RUN_SUBAGENT")
        ),
    ];
    let session = connect(
        &connection,
        &env("HATCHLING_BROKER_SOCKET"),
        &env("HATCHLING_CELL_ID"),
    );
    for probe in task.split_whitespace() {
        if probe == "hang" {
            let _ = writeln!(out, "{}", lines.join("\n"));
            let _ = out.flush();
            loop {
                std::hint::spin_loop();
            }
        }
        lines.push(format!("{probe}={}", run_probe(session.as_ref(), probe)));
    }
    if writeln!(out, "{}", lines.join("\n"))
        .and_then(|()| out.flush())
        .is_err()
    {
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

/// Whether `fd` is a socket: a socket answers `SO_ACCEPTCONN`, anything
/// else refuses it.
fn socket_kind(fd: std::os::fd::BorrowedFd<'_>) -> &'static str {
    if hatchling_sys::net::is_listening(fd).is_ok() {
        "socket"
    } else {
        "not-socket"
    }
}

/// Takes the run's connection (its inherited descriptor, `connection`, or
/// else the socket at `socket`) and completes the handshake, or says why
/// not.
fn connect(connection: &str, socket: &str, cell_id: &str) -> Result<UnixStream, String> {
    let stream = match connection.parse::<i32>() {
        Ok(fd) => hatchling_sys::net::inherited_unix_stream(fd)
            .map_err(|err| format!("connection: {err}"))?,
        Err(_) => UnixStream::connect(socket).map_err(|err| format!("connect: {err}"))?,
    };
    let hello = AgentMessage::Hello {
        v: PROTOCOL_VERSION,
        cell_id: cell_id.to_owned(),
    };
    let bytes = serde_json::to_vec(&hello).map_err(|err| err.to_string())?;
    write_frame(&stream, &bytes).map_err(|err| format!("hello: {err}"))?;
    read_frame(&stream).map_err(|err| format!("welcome: {err}"))?;
    Ok(stream)
}

fn run_probe(session: Result<&UnixStream, &String>, probe: &str) -> String {
    if probe == "model-reasoning" {
        return std::env::var("HATCHLING_MODEL_REASONING").unwrap_or_default();
    }
    if let Some(path) = probe.strip_prefix("connect:") {
        return match UnixStream::connect(path) {
            Ok(_) => String::from("reached"),
            Err(err) => format!("{:?}", err.kind()),
        };
    }
    if let Some(target) = probe.strip_prefix("space:") {
        let Ok(stream) = session else {
            return String::from("no-session");
        };
        return space_probe(stream, target).unwrap_or_else(|err| format!("error: {err}"));
    }
    let request = if let Some(name) = probe.strip_prefix("vault:") {
        SessionRequest::Vault(VaultOp::Fetch {
            name: name.to_owned(),
        })
    } else if let Some(channel) = probe.strip_prefix("channel:") {
        SessionRequest::Channel(ChannelOp::Receive {
            channel: channel.to_owned(),
            account: None,
        })
    } else if probe == "schedule" {
        SessionRequest::Schedule(ScheduleOp::Create {
            when: ScheduleSpec::Cron {
                expr: String::from("* * * * *"),
                tz: String::from("UTC"),
            },
            task: String::from("escalate"),
            grants: vec![ScheduleGrant::ChannelSend {
                channel: String::from("local"),
            }],
            channel: String::from("local"),
        })
    } else if let Some(secs) = probe.strip_prefix("followup:") {
        let Ok(delay_secs) = secs.parse() else {
            return String::from("bad-delay");
        };
        SessionRequest::Schedule(ScheduleOp::Followup {
            task: String::from("next"),
            delay_secs,
            grants: vec![local_send()],
        })
    } else if probe == "followup-pump" {
        SessionRequest::Schedule(ScheduleOp::Followup {
            task: String::from("escalate"),
            delay_secs: 60,
            grants: vec![
                local_send(),
                ScheduleGrant::Vault {
                    action: String::from("fetch"),
                    name: String::from("app-token"),
                },
            ],
        })
    } else if probe == "subagent" {
        SessionRequest::Subagent(SubagentOp::Spawn {
            task: String::from("a grandchild"),
            budget: 1,
            capabilities: Vec::new(),
        })
    } else if probe == "space-create" {
        SessionRequest::Space(SpaceOp::Create {
            name: String::from("escape"),
            grants: Vec::new(),
        })
    } else if probe == "hook" {
        SessionRequest::Hook(HookOp::Create {
            source: HookSource::ChannelMessage {
                channel: String::from("local"),
            },
            task: String::from("escalate"),
            grants: vec![local_send()],
            channel: String::from("local"),
        })
    } else {
        return String::from("unknown-probe");
    };
    let Ok(stream) = session else {
        return String::from("no-session");
    };
    ask(stream, &request).unwrap_or_else(|err| format!("error: {err}"))
}

fn local_send() -> ScheduleGrant {
    ScheduleGrant::ChannelSend {
        channel: String::from("local"),
    }
}

/// Sends one request and reads its reply.
fn exchange(stream: &UnixStream, request: &SessionRequest) -> Result<SessionResponse, String> {
    let bytes = serde_json::to_vec(request).map_err(|err| err.to_string())?;
    write_frame(stream, &bytes).map_err(|err| err.to_string())?;
    serde_json::from_slice(&read_frame(stream).map_err(|err| err.to_string())?)
        .map_err(|err| err.to_string())
}

/// Sends one request, answering with the reply's variant as the broker
/// named it (`Vault(NotFound)`, `Schedule(Denied)`, …).
fn ask(stream: &UnixStream, request: &SessionRequest) -> Result<String, String> {
    exchange(stream, request).map(|reply| shown(&reply))
}

/// `reply` as the broker named it. A fetched secret is shown as it came,
/// in base64, as a child that leaks it would: its `Debug` hides it.
fn shown(reply: &SessionResponse) -> String {
    if let SessionResponse::Vault(VaultResult::Fetched { secret_b64 }) = reply {
        return format!("Vault(Fetched {{ secret_b64: {:?} }})", secret_b64.as_str());
    }
    format!("{reply:?}")
}

/// Joins the space in `target` (`<id>:<key>`), then reads the item with
/// that admission. Answers the read's reply, or the join's if refused.
fn space_probe(stream: &UnixStream, target: &str) -> Result<String, String> {
    let (space, key) = target.split_once(':').unwrap_or((target, ""));
    let joined = exchange(
        stream,
        &SessionRequest::Space(SpaceOp::Join {
            space: space.to_owned(),
        }),
    )?;
    let SessionResponse::Space(SpaceResult::Admitted { ticket, .. }) = joined else {
        return Ok(format!("{joined:?}"));
    };
    ask(
        stream,
        &SessionRequest::Space(SpaceOp::Get {
            space: space.to_owned(),
            ticket,
            key: key.to_owned(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use hatchling_core::ScheduleResult;

    use super::*;

    #[test]
    fn a_fetched_secret_is_answered_as_it_came() {
        let leaked = format!(
            "Vault(Fetched {{ secret_b64: {:?} }})",
            hatchling_core::encode_secret(b"s3cret")
        );
        let (ours, peer) = UnixStream::pair().unwrap();
        for end in [&ours, &peer] {
            end.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
        }
        let answering = std::thread::spawn(move || {
            let _asked = read_frame(&peer).unwrap();
            let reply = SessionResponse::Vault(VaultResult::Fetched {
                secret_b64: hatchling_core::SecretB64::encode(b"s3cret"),
            });
            write_frame(&peer, &serde_json::to_vec(&reply).unwrap()).unwrap();
        });
        let request = SessionRequest::Vault(VaultOp::Fetch {
            name: String::from("app-token"),
        });
        assert_eq!(ask(&ours, &request).unwrap(), leaked);
        answering.join().unwrap();
        assert_eq!(
            shown(&SessionResponse::Vault(VaultResult::NotFound)),
            "Vault(NotFound)"
        );
    }

    #[test]
    fn only_a_socket_reads_as_one() {
        let (socket, _other) = UnixStream::pair().unwrap();
        assert_eq!(socket_kind(socket.as_fd()), "socket");
        let file = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(socket_kind(file.as_fd()), "not-socket");
    }

    #[test]
    fn the_schedule_probe_asks_for_a_create_with_its_timezone() {
        let (ours, peer) = UnixStream::pair().unwrap();
        // Both ends time out: a probe that never writes its frame must
        // fail this test, not leave both sides waiting on a read.
        for end in [&ours, &peer] {
            end.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
        }
        let answering = std::thread::spawn(move || {
            let asked: SessionRequest =
                serde_json::from_slice(&read_frame(&peer).unwrap()).unwrap();
            let reply =
                serde_json::to_vec(&SessionResponse::Schedule(ScheduleResult::Denied)).unwrap();
            write_frame(&peer, &reply).unwrap();
            asked
        });
        let said = run_probe(Ok(&ours), "schedule");
        // Closed first, so a probe that never asks ends the peer's read.
        drop(ours);
        let asked = answering.join().unwrap();
        let SessionRequest::Schedule(ScheduleOp::Create { when, .. }) = asked else {
            unreachable!("not a schedule create: {asked:?}");
        };
        assert_eq!(
            when,
            ScheduleSpec::Cron {
                expr: String::from("* * * * *"),
                tz: String::from("UTC"),
            }
        );
        assert_eq!(said, "Schedule(Denied)");
        let none = String::from("no session");
        assert_eq!(run_probe(Err(&none), "schedule"), "no-session");
        assert_eq!(run_probe(Err(&none), "followup:soon"), "bad-delay");
        assert_eq!(run_probe(Err(&none), "nothing"), "unknown-probe");
    }

    #[test]
    fn the_model_reasoning_probe_reports_the_actual_environment() {
        assert_eq!(
            run_probe(Err(&String::from("no session")), "model-reasoning"),
            std::env::var("HATCHLING_MODEL_REASONING").unwrap_or_default()
        );
    }
}
