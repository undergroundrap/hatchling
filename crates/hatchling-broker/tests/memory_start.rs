//! Memory at the broker's start (WO-047): what `serve` runs before it
//! answers anyone. In a process of its own, as a broker is, the memory
//! store's keys are only what the start hands on, so a start that sealed
//! memory but didn't hand its keys on would leave every memory call
//! failing; the library's own tests can't see that, since a test build
//! makes a key of its own.

#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

use std::path::{Path, PathBuf};

use hatchling_broker::memory_sealed::{open_published, start};
use hatchling_broker::{
    Config, EgressConfig, GoalsConfig, MemoryAction, MemoryConfig, MemoryGrant, SchedulerConfig,
    VaultConfig,
};
use hatchling_core::{LogLevel, PeerUid};
use hatchling_memory::{MemoryPrincipal, MemoryScope, MemoryStore, RetentionPolicy, SessionId};
use hatchling_vault::MasterKey;

/// Words no other test writes: what must never be on disk.
const WORDS: &str = "the owner's saffron-striped kite";

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A config with memory under `base`, and nothing else on.
fn config(base: &Path) -> Config {
    let vault_root = base.join("vault");
    Config {
        socket: "/tmp/hatchling-test-memory-start-dummy.sock"
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
        goals: GoalsConfig::disabled(),
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

#[test]
fn a_start_seals_the_memory_kept_before_and_hands_its_keys_on() {
    let base = std::env::temp_dir().join(format!(
        "hatchling-test-memory-start-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let mut config = config(&base);
    let root = config.memory.root.clone();
    let me = MemoryPrincipal {
        peer_uid: real_uid(),
        session_id: SessionId::generate().unwrap(),
    };
    let id = "m-1".parse().unwrap();
    // What 0.9 kept: an item as a plaintext file.
    MemoryStore::open(&root)
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
    let key = MasterKey::generate().unwrap();
    // Memory off: the start hands nothing on.
    config.memory.root = PathBuf::new();
    start(&config, &key).unwrap();
    assert!(
        open_published(&root).is_err(),
        "no keys before memory starts"
    );
    // Memory on: the item is moved in, its keys handed on.
    config.memory.root.clone_from(&root);
    start(&config, &key).unwrap();
    let store = open_published(&root).unwrap();
    let item = store
        .recall(&me, &MemoryScope::User, &id, 6)
        .unwrap()
        .unwrap();
    assert_eq!(item.content, WORDS);
    assert!(!root.join("user").exists(), "the plaintext is gone");
    let disk = every_byte(&root);
    assert!(
        !disk
            .windows(WORDS.len())
            .any(|window| window == WORDS.as_bytes()),
        "no word of it on the disk"
    );
    let _ = std::fs::remove_dir_all(&base);
}
