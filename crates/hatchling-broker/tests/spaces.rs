//! Component: spaces through real broker sessions and the real
//! sealed store on disk. Membership is the session's broker-asserted
//! principal; every op on a space presents an admission that is checked
//! factor by factor; tasks run as one-shot schedules bound to their space.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use hatchling_broker::{
    Config, EgressConfig, GoalsConfig, MemoryConfig, SchedulerConfig, SealedScheduleStore,
    SealedSpaceStore, SpaceConfig, SpaceSession, VaultConfig, VaultGrant, handle_space_op,
    serve_connection,
};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, LogLevel, PROTOCOL_VERSION, PeerUid, ScheduleGrant,
    SessionRequest, SessionResponse, SpaceOp, SpaceRefusal, SpaceResult, VaultAction,
};
use hatchling_scheduler::{Store, Timestamp};
use hatchling_space::{ADMISSION_TTL_SECS, PeerPin, Principal, SessionPin};
use hatchling_vault::MasterKey;

/// One master key for the process, as the broker has one.
fn key() -> &'static MasterKey {
    static KEY: OnceLock<MasterKey> = OnceLock::new();
    KEY.get_or_init(|| MasterKey::generate().unwrap())
}

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Removes a test's base directory when dropped.
struct Base(PathBuf);

impl Drop for Base {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A broker config with a space store and a schedule store under a fresh
/// base, and grants of three kinds.
fn config(name: &str) -> (Base, Config) {
    let base = std::env::temp_dir().join(format!(
        "hatchling-test-space-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let vault_root = base.join("vault");
    let config = Config {
        socket: "/tmp/hatchling-test-space-dummy.sock".parse().unwrap(),
        agent_uid: PeerUid::from_kernel(real_uid()),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: SchedulerConfig {
            root: base.join("schedules"),
            ..SchedulerConfig::disabled()
        },
        log_level: LogLevel::Error,
        vault: VaultConfig {
            key_file: VaultConfig::default_key_file(&vault_root),
            root: vault_root,
            grants: vec![VaultGrant {
                action: VaultAction::Fetch,
                name: "app-token".parse().unwrap(),
            }],
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: vec![String::from("api.example.com:443")],
        },
        channels: vec![String::from("local")],
        channel_accounts: Vec::new(),
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: SpaceConfig {
            root: base.join("spaces"),
            bound: None,
            device_socket: None,
        },
        goals: GoalsConfig::disabled(),
    };
    (Base(base), config)
}

fn send() -> ScheduleGrant {
    ScheduleGrant::ChannelSend {
        channel: String::from("local"),
    }
}

fn vault() -> ScheduleGrant {
    ScheduleGrant::Vault {
        action: String::from("fetch"),
        name: String::from("app-token"),
    }
}

fn egress() -> ScheduleGrant {
    ScheduleGrant::Egress {
        destination: String::from("api.example.com:443"),
    }
}

/// The test clock: the wall clock when the process first asks. The
/// admission table is the broker's, process-wide, and real sessions sweep
/// it on the wall clock, so the handler's ops share that timeline.
fn now_secs() -> i64 {
    static NOW: OnceLock<i64> = OnceLock::new();
    *NOW.get_or_init(|| Timestamp::now().as_second())
}

fn now() -> Timestamp {
    Timestamp::from_second(now_secs()).unwrap()
}

/// A session context as the broker builds one, for a principal, a session
/// id, and a peer.
fn session(principal: Principal, id: &str, pid: u32) -> SpaceSession {
    SpaceSession {
        principal,
        pin: SessionPin {
            session: id.to_owned(),
            peer: PeerPin { uid: 1000, pid },
        },
    }
}

const fn agent(uid: u32) -> Principal {
    Principal::Agent { uid }
}

/// One op at `at`, through the handler, as a session would ask it.
#[allow(
    clippy::needless_pass_by_value,
    reason = "callers build the op inline; taking it owned keeps them terse"
)]
fn op_at(config: &Config, who: &SpaceSession, op: SpaceOp, at: Timestamp) -> SpaceResult {
    handle_space_op(config, key(), who, &op, at)
}

fn op(config: &Config, who: &SpaceSession, op: SpaceOp) -> SpaceResult {
    op_at(config, who, op, now())
}

const fn refused(reason: SpaceRefusal) -> SpaceResult {
    SpaceResult::Refused { reason }
}

/// The space id from a `Created` reply, or what came back instead.
fn created(result: SpaceResult) -> String {
    if let SpaceResult::Created { space } = result {
        space
    } else {
        format!("not created: {result:?}")
    }
}

/// The ticket from an `Admitted` reply, or what came back instead.
fn admitted(result: SpaceResult) -> String {
    if let SpaceResult::Admitted { ticket, .. } = result {
        ticket
    } else {
        format!("not admitted: {result:?}")
    }
}

fn create(config: &Config, who: &SpaceSession, grants: Vec<ScheduleGrant>) -> String {
    created(op(
        config,
        who,
        SpaceOp::Create {
            name: String::from("Garden"),
            grants,
        },
    ))
}

fn join(config: &Config, who: &SpaceSession, space: &str) -> String {
    admitted(op(
        config,
        who,
        SpaceOp::Join {
            space: space.to_owned(),
        },
    ))
}

fn get(space: &str, ticket: &str, key: &str) -> SpaceOp {
    SpaceOp::Get {
        space: space.to_owned(),
        ticket: ticket.to_owned(),
        key: key.to_owned(),
    }
}

fn put(space: &str, ticket: &str, key: &str, value: &str) -> SpaceOp {
    SpaceOp::Put {
        space: space.to_owned(),
        ticket: ticket.to_owned(),
        key: key.to_owned(),
        value: value.to_owned(),
    }
}

/// A real session: the broker's own pipeline on a thread.
fn real_session(config: &Config) -> UnixStream {
    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let config = config.clone();
    std::thread::spawn(move || {
        let _ = serve_connection(
            &server,
            &config,
            key(),
            &hatchling_broker::Skills::default(),
        );
    });
    let hello = AgentMessage::Hello {
        v: PROTOCOL_VERSION,
        cell_id: String::from("cell-space"),
    };
    write_frame(&client, &serde_json::to_vec(&hello).unwrap()).unwrap();
    let welcome: BrokerMessage = serde_json::from_slice(&read_frame(&client).unwrap()).unwrap();
    assert!(
        matches!(welcome, BrokerMessage::Welcome { .. }),
        "{welcome:?}"
    );
    client
}

fn ask(stream: &UnixStream, op: SpaceOp) -> io::Result<SpaceResult> {
    write_frame(
        stream,
        &serde_json::to_vec(&SessionRequest::Space(op)).unwrap(),
    )
    .map_err(io::Error::other)?;
    let reply: SessionResponse =
        serde_json::from_slice(&read_frame(stream).map_err(io::Error::other)?)
            .map_err(io::Error::other)?;
    if let SessionResponse::Space(result) = reply {
        Ok(result)
    } else {
        Err(io::Error::other(format!("not a space reply: {reply:?}")))
    }
}

/// Every file under `dir`, recursively.
fn files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .flat_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                files(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}

#[test]
fn a_space_is_created_joined_and_used_through_a_real_session() {
    let (_base, config) = config("roundtrip");
    let client = real_session(&config);
    let space = created(
        ask(
            &client,
            SpaceOp::Create {
                name: String::from("Garden plot 3"),
                grants: vec![send()],
            },
        )
        .unwrap(),
    );
    // Broker-minted: the display name is not the identity.
    assert!(space.starts_with("spc-") && space.len() == 68, "{space}");
    let joined = ask(
        &client,
        SpaceOp::Join {
            space: space.clone(),
        },
    )
    .unwrap();
    let SpaceResult::Admitted {
        space: admitted_to,
        ticket,
        expires_at,
    } = joined
    else {
        panic_free_fail(&format!("{joined:?}"));
        return;
    };
    assert_eq!(admitted_to, space);
    assert!(ticket.starts_with("adm-") && ticket.len() == 68, "{ticket}");
    let wall = Timestamp::now().as_second();
    assert!(
        (wall..=wall + ADMISSION_TTL_SECS).contains(&expires_at),
        "{expires_at}"
    );
    let value = "tomatoes on the left, basil on the right";
    assert_eq!(
        ask(&client, put(&space, &ticket, "layout", value)).unwrap(),
        SpaceResult::Stored
    );
    assert_eq!(
        ask(&client, get(&space, &ticket, "layout")).unwrap(),
        SpaceResult::Value {
            value: value.to_owned()
        }
    );
    // Sealed at rest: no stored byte carries the item or the name.
    let stored = files(&config.spaces.root);
    assert!(!stored.is_empty());
    for path in &stored {
        let bytes = String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
        assert!(
            !bytes.contains("basil") && !bytes.contains("Garden"),
            "{path:?}"
        );
    }
    assert_eq!(
        ask(
            &client,
            SpaceOp::Remove {
                space: space.clone(),
                ticket: ticket.clone(),
                key: String::from("layout"),
            },
        )
        .unwrap(),
        SpaceResult::Removed
    );
    assert_eq!(
        ask(&client, get(&space, &ticket, "layout")).unwrap(),
        refused(SpaceRefusal::NoSuchItem)
    );
    // Removed means gone from the store, not just unlisted.
    let store = SealedSpaceStore::new(&config.spaces.root, key());
    assert_eq!(store.get_item(&space, "layout").unwrap(), None);
    // Removing what is not stored is not an error.
    assert!(store.remove_item(&space, "layout").is_ok());
}

/// Fails the calling test with `what`, without a `panic!`.
fn panic_free_fail(what: &str) {
    assert!(what.is_empty(), "{what}");
}

#[test]
fn another_caller_naming_a_space_gets_nothing() {
    let (_base, config) = config("cross");
    let owner = session(agent(1000), "cross-a", 10);
    let other = session(agent(1001), "cross-b", 20);
    let space = create(&config, &owner, vec![send()]);
    let ticket = join(&config, &owner, &space);
    assert!(ticket.starts_with("adm-"), "{ticket}");
    assert_eq!(
        op(&config, &owner, put(&space, &ticket, "plan", "secret plan")),
        SpaceResult::Stored
    );
    // Another caller cannot join by naming it: the same reply as for a
    // space that does not exist.
    for name in [space.as_str(), "spc-nonexistent"] {
        assert_eq!(
            op(
                &config,
                &other,
                SpaceOp::Join {
                    space: name.to_owned()
                }
            ),
            refused(SpaceRefusal::NoSuchSpace),
            "{name}"
        );
    }
    // Nor read with the owner's ticket: it was minted in another session.
    assert_eq!(
        op(&config, &other, get(&space, &ticket, "plan")),
        refused(SpaceRefusal::StaleAdmission)
    );
    // The owner's ticket for one space does not open another
    // space of the owner's, named instead.
    let second = create(&config, &owner, vec![send()]);
    assert_eq!(
        op(&config, &owner, get(&second, &ticket, "plan")),
        refused(SpaceRefusal::NotAdmitted)
    );
    // A task run bound to another space is not a member of this one, and
    // may not create spaces or tasks of its own.
    let run = session(
        Principal::TaskRun {
            space: second.clone(),
        },
        "cross-run",
        30,
    );
    assert_eq!(
        op(
            &config,
            &run,
            SpaceOp::Join {
                space: space.clone()
            }
        ),
        refused(SpaceRefusal::NoSuchSpace)
    );
    assert_eq!(
        op(
            &config,
            &run,
            SpaceOp::Create {
                name: String::from("escape"),
                grants: Vec::new(),
            }
        ),
        refused(SpaceRefusal::NotAllowed)
    );
    let run_ticket = join(&config, &run, &second);
    for task_op in [
        SpaceOp::AddTask {
            space: second.clone(),
            ticket: run_ticket.clone(),
            task: String::from("more"),
            grants: vec![send()],
            channel: String::from("local"),
        },
        SpaceOp::RunTask {
            space: second.clone(),
            ticket: run_ticket.clone(),
            task: String::from("tsk-any"),
        },
    ] {
        assert_eq!(
            op(&config, &run, task_op),
            refused(SpaceRefusal::NotAllowed)
        );
    }
    // Its own space's items it can use.
    assert_eq!(
        op(&config, &run, put(&second, &run_ticket, "log", "watered")),
        SpaceResult::Stored
    );
    // The owner's data is untouched by all of it.
    assert_eq!(
        op(&config, &owner, get(&space, &ticket, "plan")),
        SpaceResult::Value {
            value: String::from("secret plan")
        }
    );
}

#[test]
fn a_replayed_ticket_is_stale_and_a_ticket_dies_with_its_session() {
    let (_base, config) = config("replay");
    let first = real_session(&config);
    let space = created(
        ask(
            &first,
            SpaceOp::Create {
                name: String::from("Shared"),
                grants: Vec::new(),
            },
        )
        .unwrap(),
    );
    let ticket = admitted(
        ask(
            &first,
            SpaceOp::Join {
                space: space.clone(),
            },
        )
        .unwrap(),
    );
    assert_eq!(
        ask(&first, put(&space, &ticket, "note", "hello")).unwrap(),
        SpaceResult::Stored
    );
    // The same peer, a second live session: the first session's ticket is
    // a replayed credential here.
    let second = real_session(&config);
    assert_eq!(
        ask(&second, get(&space, &ticket, "note")).unwrap(),
        refused(SpaceRefusal::StaleAdmission)
    );
    // Joining in its own session admits it.
    let own = admitted(
        ask(
            &second,
            SpaceOp::Join {
                space: space.clone(),
            },
        )
        .unwrap(),
    );
    assert_ne!(own, ticket);
    assert_eq!(
        ask(&second, get(&space, &own, "note")).unwrap(),
        SpaceResult::Value {
            value: String::from("hello")
        }
    );
    // The first session ends: its ticket is gone for good.
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reply = ask(&second, get(&space, &ticket, "note")).unwrap();
        if reply == refused(SpaceRefusal::NotAdmitted) {
            break;
        }
        assert_eq!(reply, refused(SpaceRefusal::StaleAdmission));
        assert!(
            Instant::now() < deadline,
            "the ended session's ticket lived on"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_changed_peer_or_an_expired_admission_must_join_again() {
    let (_base, config) = config("factors");
    let me = session(agent(1000), "factors-a", 10);
    let space = create(&config, &me, Vec::new());
    let ticket = join(&config, &me, &space);
    // The same session id, a different peer: not the admitted peer.
    let moved = session(agent(1000), "factors-a", 11);
    assert_eq!(
        op(&config, &moved, get(&space, &ticket, "x")),
        refused(SpaceRefusal::PeerChanged)
    );
    // Expired, at the bound.
    let later = Timestamp::from_second(now_secs() + ADMISSION_TTL_SECS).unwrap();
    assert_eq!(
        op_at(&config, &me, put(&space, &ticket, "x", "y"), later),
        refused(SpaceRefusal::Expired)
    );
    // A second before it, still good.
    let before = Timestamp::from_second(now_secs() + ADMISSION_TTL_SECS - 1).unwrap();
    assert_eq!(
        op_at(&config, &me, put(&space, &ticket, "x", "y"), before),
        SpaceResult::Stored
    );
    // Re-authenticating: a fresh join, by the new peer, admits it.
    let fresh = join(&config, &moved, &space);
    assert_eq!(
        op(&config, &moved, get(&space, &fresh, "x")),
        SpaceResult::Value {
            value: String::from("y")
        }
    );
    // A forged ticket is no admission at all.
    assert_eq!(
        op(&config, &me, get(&space, "adm-forged", "x")),
        refused(SpaceRefusal::NotAdmitted)
    );
}

#[test]
fn bounds_and_bad_input_are_refused_and_nothing_enumerates() {
    let (_base, config) = config("bounds");
    let me = session(agent(1000), "bounds-a", 10);
    assert_eq!(
        op(
            &config,
            &me,
            SpaceOp::Create {
                name: String::from(" "),
                grants: Vec::new(),
            }
        ),
        refused(SpaceRefusal::BadName)
    );
    // A grant the creator does not hold.
    assert_eq!(
        op(
            &config,
            &me,
            SpaceOp::Create {
                name: String::from("x"),
                grants: vec![ScheduleGrant::Vault {
                    action: String::from("fetch"),
                    name: String::from("not-granted"),
                }],
            }
        ),
        refused(SpaceRefusal::ExceedsGrants)
    );
    let space = create(&config, &me, Vec::new());
    let ticket = join(&config, &me, &space);
    // No wildcard: a key is a token, never a pattern.
    for key in ["*", "", "../plan", "Plan", "a.b"] {
        assert_eq!(
            op(&config, &me, put(&space, &ticket, key, "v")),
            refused(SpaceRefusal::BadKey),
            "{key:?}"
        );
        assert_eq!(
            op(&config, &me, get(&space, &ticket, key)),
            refused(SpaceRefusal::BadKey),
            "{key:?}"
        );
    }
    assert_eq!(
        op(&config, &me, put(&space, &ticket, "big", &"v".repeat(8193))),
        refused(SpaceRefusal::BadValue)
    );
    assert_eq!(
        op(&config, &me, get(&space, &ticket, "never-put")),
        refused(SpaceRefusal::NoSuchItem)
    );
    assert_eq!(
        op(
            &config,
            &me,
            SpaceOp::Remove {
                space: space.clone(),
                ticket: ticket.clone(),
                key: String::from("never-put"),
            }
        ),
        refused(SpaceRefusal::NoSuchItem)
    );
    for n in 0..hatchling_space::MAX_ITEMS {
        assert_eq!(
            op(&config, &me, put(&space, &ticket, &format!("k{n}"), "v")),
            SpaceResult::Stored,
            "{n}"
        );
    }
    assert_eq!(
        op(&config, &me, put(&space, &ticket, "one-more", "v")),
        refused(SpaceRefusal::Full)
    );
    // Overwriting an existing item is no new item.
    assert_eq!(
        op(&config, &me, put(&space, &ticket, "k0", "w")),
        SpaceResult::Stored
    );
    // The protocol has no list, dump, or export op: a session that asks
    // for one is ended, not answered.
    let client = real_session(&config);
    let frame = br#"{"kind":"space","op":"list"}"#;
    write_frame(&client, frame).unwrap();
    assert!(read_frame(&client).is_err(), "no reply to a list op");
}

#[test]
fn spaces_are_denied_without_a_store_and_admissions_per_session_are_bounded() {
    let (_base, mut broker) = config("off");
    let me = session(agent(1000), "off-a", 10);
    let space = create(&broker, &me, Vec::new());
    for _ in 0..hatchling_space::MAX_ADMISSIONS_PER_SESSION {
        assert!(join(&broker, &me, &space).starts_with("adm-"));
    }
    assert_eq!(
        op(
            &broker,
            &me,
            SpaceOp::Join {
                space: space.clone()
            }
        ),
        refused(SpaceRefusal::Full)
    );
    broker.spaces = SpaceConfig::disabled();
    assert_eq!(
        op(
            &broker,
            &me,
            SpaceOp::Join {
                space: space.clone()
            }
        ),
        SpaceResult::Denied
    );
    // A run's own session has no spaces unless it is that space's task.
    let run = hatchling_broker::scoped_config(
        &config("off-run").1,
        &hatchling_scheduler::RunRequest {
            schedule_id: String::from("sch-1"),
            task: String::from("t"),
            grants: Vec::new(),
            due: now(),
            missed: false,
            channel: String::from("local"),
            depth: 0,
            event: None,
            space: None,
            feed: None,
            chat: false,
            history: Vec::new(),
        },
        "run-x",
        "/tmp/hatchling-test-space-run.sock".parse().unwrap(),
    );
    assert_eq!(op(&run, &me, SpaceOp::Join { space }), SpaceResult::Denied);
}

#[test]
fn a_task_holds_only_its_spaces_grants_and_queues_a_run_bound_to_it() {
    let (_base, mut config) = config("tasks");
    let me = session(agent(1000), "tasks-a", 10);
    // The creator holds send, vault, and egress; the space only two.
    let space = create(&config, &me, vec![send(), vault()]);
    let ticket = join(&config, &me, &space);
    let add = |grants: Vec<ScheduleGrant>| SpaceOp::AddTask {
        space: space.clone(),
        ticket: ticket.clone(),
        task: String::from("water the plants"),
        grants,
        channel: String::from("local"),
    };
    // Held by the creator but not by the space: refused.
    assert_eq!(
        op(&config, &me, add(vec![send(), egress()])),
        refused(SpaceRefusal::ExceedsGrants)
    );
    let task = if let SpaceResult::TaskAdded { task } = op(&config, &me, add(vec![send()])) {
        task
    } else {
        String::from("not added")
    };
    assert!(task.starts_with("tsk-"), "{task}");
    let run = |task: &str| SpaceOp::RunTask {
        space: space.clone(),
        ticket: ticket.clone(),
        task: task.to_owned(),
    };
    let queued = op(&config, &me, run(&task));
    let SpaceResult::TaskQueued { schedule } = queued else {
        panic_free_fail(&format!("{queued:?}"));
        return;
    };
    // A one-shot, a second out, bound to the space, with the task's grants.
    let stored = SealedScheduleStore::new(&config.scheduler.root, key())
        .load()
        .unwrap();
    let queued = stored.iter().find(|s| s.id == schedule).unwrap();
    assert_eq!(queued.space.as_deref(), Some(space.as_str()));
    assert_eq!(queued.grants, [send()]);
    assert_eq!(queued.next_due, now_secs() + 1);
    assert_eq!(queued.task, "water the plants");
    // Unknown tasks, and another space's task, do not run here.
    assert_eq!(
        op(&config, &me, run("tsk-nonexistent")),
        refused(SpaceRefusal::NoSuchTask)
    );
    let other = create(&config, &me, vec![send()]);
    let other_ticket = join(&config, &me, &other);
    assert_eq!(
        op(
            &config,
            &me,
            SpaceOp::RunTask {
                space: other,
                ticket: other_ticket,
                task: task.clone(),
            }
        ),
        refused(SpaceRefusal::NoSuchTask)
    );
    // No scheduler, no runs.
    config.scheduler = SchedulerConfig::disabled();
    assert_eq!(
        op(&config, &me, run(&task)),
        refused(SpaceRefusal::SchedulerOff)
    );
    // The store holds the task apart, sealed.
    assert!(
        SealedSpaceStore::new(&config.spaces.root, key())
            .get_task(&task)
            .unwrap()
            .is_some()
    );
}
