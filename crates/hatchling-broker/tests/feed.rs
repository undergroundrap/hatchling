//! Component: a feed end to end. A feed is created through the
//! schedule op, with grants the broker derives; it fires through the real
//! dispatcher; the broker generates its digest from the real goals,
//! tracking, and memory stores through the read-only query path; and the
//! dispatcher delivers it to the local channel.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use hatchling_broker::{
    Config, EgressConfig, FEED_TASK, FeedLauncher, GoalsAction, GoalsConfig, MemoryAction,
    MemoryConfig, MemoryGrant, SchedulerConfig, SealedScheduleStore, VaultConfig, dispatch_once,
    handle_schedule_op,
};
use hatchling_channel::{LocalHub, Message};
use hatchling_core::{
    FeedSpec, LogLevel, PeerUid, ScheduleGrant, ScheduleOp, ScheduleRefusal, ScheduleResult,
    ScheduleSpec,
};
use hatchling_goals::GoalsStore;
use hatchling_memory::{MemoryPrincipal, MemoryScope, MemoryStore, RetentionPolicy, SessionId};
use hatchling_scheduler::{Dispatcher, Launcher, Run, RunRequest, Store, Timestamp};
use hatchling_vault::MasterKey;

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A config with a scheduler, goals, and memory under `base`, granting
/// the goals and tracking queries and user memory recall.
fn config(base: &Path) -> Config {
    let vault_root = base.join("vault");
    Config {
        socket: "/tmp/hatchling-test-feed-dummy.sock".parse().unwrap(),
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
            grants: Vec::new(),
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: Vec::new(),
        },
        channels: vec![String::from("local")],
        channel_accounts: Vec::new(),
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: MemoryConfig {
            root: base.join("memory"),
            grants: vec![MemoryGrant {
                scope: String::from("user"),
                action: MemoryAction::Recall,
            }],
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        goals: GoalsConfig {
            root: base.join("goals"),
            grants: vec![GoalsAction::QueryGoals, GoalsAction::QueryTracking],
        },
    }
}

/// A fresh base directory for one test, removed when dropped.
struct Base(PathBuf);

impl Base {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-feedrun-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}

impl Drop for Base {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ts(rfc3339: &str) -> Timestamp {
    rfc3339.parse().unwrap()
}

fn spec() -> FeedSpec {
    FeedSpec {
        goals: Some(String::from("open")),
        tracking: true,
        tracking_source: None,
        memory_tags: vec![String::from("travel")],
        lookback_hours: 24,
        max_items: 20,
    }
}

fn create_feed(feed: FeedSpec) -> ScheduleOp {
    ScheduleOp::CreateFeed {
        when: ScheduleSpec::Cron {
            expr: String::from("0 8 * * *"),
            tz: String::from("UTC"),
        },
        channel: String::from("local"),
        feed,
    }
}

fn created(result: ScheduleResult) -> String {
    if let ScheduleResult::Created { id } = result {
        id
    } else {
        format!("not created: {result:?}")
    }
}

/// An inner launcher for every run that is not a feed: it records the
/// request and answers at once.
#[derive(Default, Clone)]
struct Cells {
    requests: Rc<RefCell<Vec<RunRequest>>>,
}

struct Done(Option<Result<String, String>>);

impl Run for Done {
    fn poll(&mut self) -> Option<Result<String, String>> {
        self.0.take()
    }
}

impl Launcher for Cells {
    type Run = Done;

    fn launch(&self, request: RunRequest) -> Result<Done, String> {
        self.requests.borrow_mut().push(request);
        Ok(Done(Some(Ok(String::from("a cell ran")))))
    }
}

/// A hub with a client attached, recording what it receives.
fn hub_with_client() -> (LocalHub, Arc<Mutex<Vec<Message>>>) {
    let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let _ = hub.attach(Box::new(move |m: &hatchling_channel::Cleared| {
        sink.lock().unwrap().push(m.message().clone());
        Ok(())
    }));
    (hub, received)
}

/// Every file under `dir`, with its bytes, sorted by path.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                out.extend(snapshot(&path));
            } else {
                out.push((path.clone(), std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    out.sort();
    out
}

/// Seeds the stores: a goal, two tracking entries, and a tagged note.
fn seed(config: &Config, at: u64) -> (String, u64) {
    let me = MemoryPrincipal {
        peer_uid: real_uid(),
        session_id: SessionId::generate().unwrap(),
    };
    let goals = GoalsStore::open(&config.goals.root).unwrap();
    let goal = goals.create_goal(&me, "Run 5k", "", at).unwrap();
    let entry = goals
        .append_tracking(&me, &"whatsapp".parse().unwrap(), "you should sell now", at)
        .unwrap();
    MemoryStore::open(&config.memory.root)
        .unwrap()
        .write(
            &me,
            &MemoryScope::User,
            &"m-1".parse().unwrap(),
            "passport expires in March",
            &["travel".parse().unwrap()],
            RetentionPolicy::Keep,
            at,
        )
        .unwrap();
    (goal.id.to_string(), entry.seq)
}

#[test]
fn a_scheduled_feed_delivers_a_digest_whose_every_item_is_labeled() {
    let base = Base::new("deliver");
    let config = config(&base.0);
    let key = MasterKey::generate().unwrap();
    let id = created(agent_schedule_op(
        &config,
        &key,
        &create_feed(spec()),
        ts("2026-09-21T07:00:00Z"),
    ));
    assert!(id.starts_with("sch-"), "{id}");
    // The feed's grants come from its spec: reads, and the channel.
    let stored = SealedScheduleStore::new(&config.scheduler.root, &key)
        .load()
        .unwrap();
    let schedule = stored.first().unwrap();
    assert_eq!(schedule.task, FEED_TASK);
    assert_eq!(schedule.feed, Some(spec()));
    assert_eq!(
        schedule.grants,
        [
            ScheduleGrant::Memory {
                scope: String::from("user"),
                action: String::from("recall"),
            },
            ScheduleGrant::Goals {
                action: String::from("query-goals"),
            },
            ScheduleGrant::Goals {
                action: String::from("query-tracking"),
            },
            ScheduleGrant::ChannelSend {
                channel: String::from("local"),
            },
        ]
    );
    // 2026-09-21T07:30:00Z: inside the window of the 08:00 digest.
    let (goal, seq) = seed(&config, 1_789_975_800);
    let goals_dir = snapshot(&config.goals.root);
    let memory_dir = snapshot(&config.memory.root);
    let (hub, received) = hub_with_client();
    let cells = Cells::default();
    let launcher = FeedLauncher::new(&config, cells.clone());
    let mut dispatcher = Dispatcher::default();
    let mut delivered = Vec::new();
    for now in ["2026-09-21T08:00:02Z", "2026-09-21T08:01:00Z"] {
        let report = dispatch_once(
            &mut dispatcher,
            &config,
            &key,
            Some(&hub),
            &launcher,
            ts(now),
        )
        .unwrap();
        delivered.extend(report.delivered);
    }
    assert_eq!(delivered, [id]);
    assert!(cells.requests.borrow().is_empty(), "no cell for a feed");
    let received = received.lock().unwrap().clone();
    assert_eq!(received.len(), 1);
    let message = received.first().unwrap();
    assert_eq!(message.sender.0, "broker:scheduler");
    assert_eq!(
        message.text,
        format!(
            "Feed for 2026-09-20T08:00:00Z to 2026-09-21T08:00:00Z (UTC)\nObservations from your own records, each with where it came from. Nothing here is a recommendation.\n\nOpen goals\n- goal \"Run 5k\" is open, last changed 2026-09-21T07:30:00Z [from goal (created by the user)] (goal {goal})\n\nTracking\n- reported 2026-09-21T07:30:00Z: \"you should sell now\" [from tracking (originally from whatsapp)] (tracking {seq})\n\nMemory tagged travel\n- noted 2026-09-21T07:30:00Z: \"passport expires in March\" [from memory (origin not recorded)] (memory m-1)"
        )
    );
    // Every item line carries a label.
    let items: Vec<&str> = message
        .text
        .lines()
        .filter(|line| line.starts_with("- "))
        .collect();
    assert_eq!(items.len(), 3);
    assert!(items.iter().all(|line| line.contains(" [from ")));
    // The feed read; it wrote nothing.
    assert_eq!(snapshot(&config.goals.root), goals_dir);
    assert_eq!(snapshot(&config.memory.root), memory_dir);
}

#[test]
fn a_feed_holds_only_reads_and_reads_only_what_it_still_may() {
    let base = Base::new("grants");
    let mut config = config(&base.0);
    let key = MasterKey::generate().unwrap();
    let now = ts("2026-09-21T07:00:00Z");
    // A spec it cannot use: no section, a bound out of range, or a source
    // without tracking.
    for bad in [
        FeedSpec {
            goals: None,
            tracking: false,
            memory_tags: Vec::new(),
            ..spec()
        },
        FeedSpec {
            lookback_hours: 0,
            ..spec()
        },
        FeedSpec {
            tracking: false,
            tracking_source: Some(String::from("user")),
            ..spec()
        },
    ] {
        assert_eq!(
            agent_schedule_op(&config, &key, &create_feed(bad.clone()), now),
            ScheduleResult::Refused {
                reason: ScheduleRefusal::BadTask
            },
            "{bad:?}"
        );
    }
    // A feed may not read what its creator may not.
    let mut lacking = crate::config(&base.0);
    lacking.memory.grants.clear();
    assert_eq!(
        agent_schedule_op(&lacking, &key, &create_feed(spec()), now),
        ScheduleResult::Refused {
            reason: ScheduleRefusal::ExceedsGrants
        }
    );
    let mut off = crate::config(&base.0);
    off.scheduler = SchedulerConfig::disabled();
    assert_eq!(
        agent_schedule_op(&off, &key, &create_feed(spec()), now),
        ScheduleResult::Denied
    );
    // A grant revoked after the feed was created is not read when it fires.
    let id = created(agent_schedule_op(&config, &key, &create_feed(spec()), now));
    seed(&config, 1_789_975_800);
    config.goals.grants = vec![GoalsAction::QueryGoals];
    let (hub, received) = hub_with_client();
    let launcher = FeedLauncher::new(&config, Cells::default());
    let mut dispatcher = Dispatcher::default();
    let mut revoked = Vec::new();
    for now in ["2026-09-21T08:00:02Z", "2026-09-21T08:01:00Z"] {
        let report = dispatch_once(
            &mut dispatcher,
            &config,
            &key,
            Some(&hub),
            &launcher,
            ts(now),
        )
        .unwrap();
        revoked.extend(report.revoked);
    }
    assert_eq!(
        revoked,
        [(
            id,
            ScheduleGrant::Goals {
                action: String::from("query-tracking")
            }
        )]
    );
    let text = received
        .lock()
        .unwrap()
        .first()
        .map(|m| m.text.clone())
        .unwrap();
    assert!(
        text.contains(
            "\n\nTracking\n- not read: this feed no longer holds the grant to read it\n\n"
        ),
        "{text}"
    );
    assert!(text.contains("[from goal (created by the user)]"), "{text}");
}

#[test]
fn an_ordinary_run_still_gets_its_cell() {
    let base = Base::new("ordinary");
    let config = config(&base.0);
    let key = MasterKey::generate().unwrap();
    let id = created(agent_schedule_op(
        &config,
        &key,
        &ScheduleOp::Create {
            when: ScheduleSpec::Cron {
                expr: String::from("0 8 * * *"),
                tz: String::from("UTC"),
            },
            task: String::from("summarize my day"),
            grants: vec![ScheduleGrant::ChannelSend {
                channel: String::from("local"),
            }],
            channel: String::from("local"),
        },
        ts("2026-09-21T07:00:00Z"),
    ));
    let (hub, received) = hub_with_client();
    let cells = Cells::default();
    let launcher = FeedLauncher::new(&config, cells.clone());
    let mut dispatcher = Dispatcher::default();
    for now in ["2026-09-21T08:00:02Z", "2026-09-21T08:01:00Z"] {
        dispatch_once(
            &mut dispatcher,
            &config,
            &key,
            Some(&hub),
            &launcher,
            ts(now),
        )
        .unwrap();
    }
    let requests = cells.requests.borrow();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests
            .first()
            .map(|r| (r.schedule_id.as_str(), r.feed.is_none())),
        Some((id.as_str(), true))
    );
    assert_eq!(
        received.lock().unwrap().first().map(|m| m.text.clone()),
        Some(String::from("a cell ran"))
    );
}

/// A schedule op as an agent session sends it: every one these tests
/// make.
fn agent_schedule_op(
    config: &Config,
    key: &MasterKey,
    op: &ScheduleOp,
    now: Timestamp,
) -> ScheduleResult {
    handle_schedule_op(config, key, op, now, hatchling_scheduler::Creator::Agent)
}
