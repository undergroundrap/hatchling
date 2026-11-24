//! Memory and goals at the broker's start (WO-047): what `serve` runs
//! before it answers anyone. In a process of its own, as a broker is, the
//! stores' keys are only what the start hands on, so a start that sealed
//! them but didn't hand their keys on would leave every memory and goals
//! call failing; the library's own tests can't see that, since a test
//! build makes a key of its own.

#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::path::{Path, PathBuf};

use hatchling_broker::held_keys::start;
use hatchling_broker::{
    Config, EgressConfig, GoalsConfig, MemoryAction, MemoryConfig, MemoryGrant, SchedulerConfig,
    VaultConfig, goals_sealed, memory_sealed,
};
use hatchling_core::{LogLevel, PeerUid};
use hatchling_goals::GoalsStore;
use hatchling_memory::{MemoryPrincipal, MemoryScope, MemoryStore, RetentionPolicy, SessionId};
use hatchling_vault::MasterKey;

/// Words no other test writes: what must never be on disk.
const WORDS: &str = "the owner's saffron-striped kite";

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A config with memory and goals under `base`, and nothing else on.
fn config(base: &Path) -> Config {
    let vault_root = base.join("vault");
    Config {
        socket: "/tmp/hatchling-test-sealed-start-dummy.sock"
            .parse()
            .unwrap(),
        agent_uid: PeerUid::from_kernel(real_uid()),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: SchedulerConfig::disabled(),
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
        channels: Vec::new(),
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
        web: hatchling_broker::WebConfig::off(),
        goals: GoalsConfig {
            root: base.join("goals"),
            grants: Vec::new(),
        },
    }
}

/// Every byte of every file under `dir`.
fn every_byte(dir: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            bytes.extend(every_byte(&path));
        } else {
            bytes.extend(std::fs::read(&path).unwrap());
        }
    }
    bytes
}

fn holds(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn a_start_seals_what_was_kept_before_and_hands_the_keys_on() {
    let base = std::env::temp_dir().join(format!(
        "hatchling-test-sealed-start-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let mut config = config(&base);
    let memory_root = config.memory.root.clone();
    let goals_root = config.goals.root.clone();
    let me = MemoryPrincipal {
        peer_uid: real_uid(),
        session_id: SessionId::generate().unwrap(),
    };
    let id = "m-1".parse().unwrap();
    // What 0.9 kept: an item, a goal and a tracking entry, as plaintext.
    MemoryStore::open(&memory_root)
        .unwrap()
        .write(
            &me,
            &MemoryScope::User,
            &id,
            WORDS,
            &[],
            RetentionPolicy::Keep,
            5,
        )
        .unwrap();
    let plain = GoalsStore::open(&goals_root).unwrap();
    let goal = plain.create_goal(&me, WORDS, "", 5).unwrap();
    plain
        .append_tracking(&me, &"user".parse().unwrap(), WORDS, 5)
        .unwrap();
    let key = MasterKey::generate().unwrap();
    // Neither on: the start hands nothing on.
    config.memory.root = PathBuf::new();
    config.goals.root = PathBuf::new();
    start(&config, &key).unwrap();
    assert!(
        memory_sealed::open_published(&memory_root).is_err(),
        "no keys before memory starts"
    );
    assert!(goals_sealed::open_published(&goals_root).is_err());
    // Both on: what was kept is moved in, the keys handed on.
    config.memory.root.clone_from(&memory_root);
    config.goals.root.clone_from(&goals_root);
    start(&config, &key).unwrap();
    let store = memory_sealed::open_published(&memory_root).unwrap();
    let item = store
        .recall(&me, &MemoryScope::User, &id, 6)
        .unwrap()
        .unwrap();
    assert_eq!(item.content, WORDS);
    let goals = goals_sealed::source_published(&goals_root).unwrap();
    assert_eq!(goals.goal_ids(me.peer_uid).unwrap(), [goal.id]);
    assert_eq!(goals.tracking(me.peer_uid).unwrap().len(), 1);
    // And the goals store carries on from what was moved in.
    let entry = goals_sealed::open_published(&goals_root)
        .unwrap()
        .append_tracking(&me, &"user".parse().unwrap(), "next", 6)
        .unwrap();
    assert_eq!(entry.seq, 1);
    assert!(!memory_root.join("user").exists(), "the plaintext is gone");
    assert!(!goals_root.join("goals").exists());
    assert!(!goals_root.join("tracking").exists());
    for root in [&memory_root, &goals_root] {
        assert!(
            !holds(&every_byte(root), WORDS.as_bytes()),
            "no word of it on the disk"
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}
