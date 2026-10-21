//! Component: trust-root rotation without a restart ( stage 3).
//!
//! Signed packages on disk, a real trust-roots file, and the startup
//! loader ([`Skills::load`]); rotation through [`Skills::reload`], the one
//! poll the watcher repeats, and through the watcher thread itself. The
//! session test runs the real
//! [`serve_connection`](hatchling_broker::serve_connection) over a
//! socketpair and keeps one session open across the whole rotation:
//! nothing restarts, and the same connection sees the new roots.
//!
//! No skill runtime is configured here, so a call that passes every check
//! answers the opaque `skill-error`: in these tests that means "admitted".
//! Real runs across a swap are in `skill_run.rs`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer, SigningKey};
use hatchling_broker::trust_roots::TRUST_ROOTS_HEADER;
use hatchling_broker::{
    Config, EgressConfig, Error, MemoryConfig, Reload, SkillConfig, SkillSet, Skills, VaultConfig,
    serve_connection,
};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, CapabilityName, LogLevel, PeerUid, SessionRequest,
    SessionResponse, SkillOp, SkillResult,
};
use hatchling_skill::{Manifest, SignedManifest, SkillId};

type R<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

const KEY_A: [u8; 32] = [0x51; 32];
const KEY_B: [u8; 32] = [0x52; 32];

/// Admitted (checks pass; no runtime here, so the opaque error).
const ADMITTED: SkillResult = SkillResult::SkillError;

fn public(secret: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(secret).verifying_key().to_bytes()
}

fn cap() -> R<CapabilityName> {
    "text.read".parse().map_err(err)
}

fn id(raw: &str) -> R<SkillId> {
    raw.parse().map_err(err)
}

fn test_root(name: &str) -> R<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "hatchling-test-rotation-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("skills")).map_err(err)?;
    Ok(dir)
}

/// A signed package: which dir it lives in, and what it declares.
struct Package<'a> {
    dir: &'a str,
    name: &'a str,
    version: &'a str,
    signer: &'a [u8; 32],
    code: &'a [u8],
}

/// Writes `<root>/skills/<dir>`: the signed manifest and its code.
fn package(root: &Path, pkg: &Package<'_>) -> R<()> {
    let manifest = Manifest {
        name: pkg.name.to_owned(),
        version: pkg.version.to_owned(),
        capabilities: vec![cap()?],
        entry_point: String::from("main.py"),
    };
    let payload = manifest.canonical_bytes().map_err(err)?;
    let signature = SigningKey::from_bytes(pkg.signer).sign(&payload);
    let signed = SignedManifest {
        manifest,
        signature: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        )),
    };
    let dir = root.join("skills").join(pkg.dir);
    std::fs::create_dir_all(&dir).map_err(err)?;
    std::fs::write(dir.join("manifest.json"), signed.to_json().map_err(err)?).map_err(err)?;
    std::fs::write(dir.join("main.py"), pkg.code).map_err(err)
}

const fn alpha() -> Package<'static> {
    Package {
        dir: "alpha",
        name: "alpha",
        version: "1.0.0",
        signer: &KEY_A,
        code: b"a",
    }
}

const fn beta() -> Package<'static> {
    Package {
        dir: "beta",
        name: "beta",
        version: "1.0.0",
        signer: &KEY_B,
        code: b"b",
    }
}

fn roots_path(root: &Path) -> PathBuf {
    root.join("trust-roots")
}

/// Writes the roots file: `primary` first, then `previous`, owner-only.
fn write_roots(root: &Path, keys: &[[u8; 32]]) -> R<()> {
    let mut text = format!("{TRUST_ROOTS_HEADER}\n");
    for (role, key) in ["primary", "previous"].iter().zip(keys) {
        let key = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key);
        writeln!(text, "{role} {key}").map_err(err)?;
    }
    text.push_str("end\n");
    write_raw(root, text.as_bytes())
}

fn write_raw(root: &Path, bytes: &[u8]) -> R<()> {
    let path = roots_path(root);
    std::fs::write(&path, bytes).map_err(err)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(err)
}

/// Grants `text.read` to each named skill.
fn config(root: &Path, skills: &[&str]) -> R<SkillConfig> {
    let mut grants: HashMap<String, HashSet<CapabilityName>> = HashMap::new();
    for name in skills {
        grants.insert((*name).to_owned(), HashSet::from([cap()?]));
    }
    Ok(SkillConfig {
        skills_dir: root.join("skills"),
        trust_roots_file: roots_path(root),
        grants,
    })
}

fn load(root: &Path, skills: &[&str]) -> R<Skills> {
    Skills::load(&config(root, skills)?).map_err(err)
}

fn ids(set: &SkillSet) -> Vec<String> {
    let mut ids: Vec<String> = set
        .registry()
        .list()
        .iter()
        .map(ToString::to_string)
        .collect();
    ids.sort();
    ids
}

fn applied(added: &[&str], dropped: &[&str], refused_drift: &[&str]) -> R<Reload> {
    let all = |list: &[&str]| list.iter().map(|raw| id(raw)).collect::<R<Vec<_>>>();
    Ok(Reload::Applied {
        added: all(added)?,
        dropped: all(dropped)?,
        refused_drift: all(refused_drift)?,
    })
}

/// A real session over a socketpair, serving `skills`; returns the
/// handshaken client.
fn session(root: &Path, skills: Arc<Skills>) -> R<UnixStream> {
    let vault_root = root.join("vault");
    let config = Config {
        socket: "/tmp/hatchling-test-rotation-dummy.sock"
            .parse()
            .map_err(err)?,
        agent_uid: PeerUid::from_kernel(rustix::process::getuid().as_raw()),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: hatchling_broker::SchedulerConfig::disabled(),
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
        skills: SkillConfig::disabled(),
        memory: MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
        goals: hatchling_broker::GoalsConfig::disabled(),
    };
    let key = hatchling_vault::MasterKey::generate().map_err(err)?;
    let (client, server) = UnixStream::pair().map_err(err)?;
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(err)?;
    std::thread::spawn(move || {
        let _ = serve_connection(&server, &config, &key, &skills);
    });
    let hello = AgentMessage::Hello {
        v: hatchling_core::PROTOCOL_VERSION,
        cell_id: String::from("cell-01"),
    };
    write_frame(&client, &serde_json::to_vec(&hello).map_err(err)?).map_err(err)?;
    let reply: BrokerMessage =
        serde_json::from_slice(&read_frame(&client).map_err(err)?).map_err(err)?;
    if !matches!(reply, BrokerMessage::Welcome { .. }) {
        return Err(format!("expected WELCOME, got {reply:?}"));
    }
    Ok(client)
}

fn invoke(stream: &UnixStream, skill: &str) -> R<SkillResult> {
    let request = SessionRequest::Skill(SkillOp::Invoke {
        skill: skill.to_owned(),
        capability: cap()?,
        input: String::new(),
    });
    write_frame(stream, &serde_json::to_vec(&request).map_err(err)?).map_err(err)?;
    let reply: SessionResponse =
        serde_json::from_slice(&read_frame(stream).map_err(err)?).map_err(err)?;
    match reply {
        SessionResponse::Skill(result) => Ok(result),
        SessionResponse::Vault(_)
        | SessionResponse::Egress(_)
        | SessionResponse::Channel(_)
        | SessionResponse::Memory(_)
        | SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_)
        | SessionResponse::Browser(_) => Err(String::from("not a skill reply")),
    }
}

/// The whole rotation, on one live session: A only, then the grace window
/// (B primary, A previous), then B only. The skill signed by A is dropped
/// when A is retired; the skill signed by B admits when B arrives.
#[test]
fn a_rotation_takes_effect_on_a_live_session_without_a_restart() -> R<()> {
    let root = test_root("rotate")?;
    package(&root, &alpha())?;
    package(&root, &beta())?;
    write_roots(&root, &[public(&KEY_A)])?;
    let skills = Arc::new(load(&root, &["alpha", "beta"])?);
    let client = session(&root, Arc::clone(&skills))?;
    assert_eq!(invoke(&client, "alpha@1.0.0")?, ADMITTED);
    assert_eq!(invoke(&client, "beta@1.0.0")?, SkillResult::UnknownSkill);

    write_roots(&root, &[public(&KEY_B), public(&KEY_A)])?;
    assert_eq!(skills.reload(), applied(&["beta@1.0.0"], &[], &[])?);
    assert_eq!(invoke(&client, "alpha@1.0.0")?, ADMITTED);
    assert_eq!(invoke(&client, "beta@1.0.0")?, ADMITTED);

    write_roots(&root, &[public(&KEY_B)])?;
    assert_eq!(skills.reload(), applied(&[], &["alpha@1.0.0"], &[])?);
    assert_eq!(invoke(&client, "alpha@1.0.0")?, SkillResult::UnknownSkill);
    assert_eq!(invoke(&client, "beta@1.0.0")?, ADMITTED);
    // Nothing changed on disk: the next poll does nothing.
    assert_eq!(skills.reload(), Reload::Unchanged);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A bad file never changes what is trusted, and a file that stays bad is
/// reported once, not on every poll. A torn write (no `end`) is bad.
#[test]
fn a_bad_file_keeps_the_current_set_and_is_reported_once() -> R<()> {
    let root = test_root("bad-file")?;
    package(&root, &alpha())?;
    write_roots(&root, &[public(&KEY_A)])?;
    let skills = load(&root, &["alpha"])?;
    let good = std::fs::read(roots_path(&root)).map_err(err)?;

    write_raw(&root, b"not a trust-roots file\n")?;
    assert!(matches!(skills.reload(), Reload::Rejected { .. }));
    assert_eq!(skills.reload(), Reload::Unchanged);
    assert_eq!(ids(&skills.snapshot()), vec!["alpha@1.0.0"]);

    // Torn: every line of a valid file but the final `end`.
    let torn = String::from_utf8(good.clone())
        .map_err(err)?
        .replace("end\n", "");
    write_raw(&root, torn.as_bytes())?;
    let reload = skills.reload();
    assert!(
        matches!(&reload, Reload::Rejected { reason } if reason.contains("end")),
        "{reload:?}"
    );
    assert_eq!(ids(&skills.snapshot()), vec!["alpha@1.0.0"]);

    // Writable by others: refused, whatever it says.
    write_raw(&root, &good)?;
    std::fs::set_permissions(roots_path(&root), std::fs::Permissions::from_mode(0o666))
        .map_err(err)?;
    let reload = skills.reload();
    assert!(
        matches!(&reload, Reload::Rejected { reason } if reason.contains("writable")),
        "{reload:?}"
    );
    // Missing: not a revocation.
    std::fs::remove_file(roots_path(&root)).map_err(err)?;
    assert!(matches!(skills.reload(), Reload::Rejected { .. }));
    assert_eq!(skills.reload(), Reload::Unchanged);
    assert_eq!(ids(&skills.snapshot()), vec!["alpha@1.0.0"]);

    // Valid again (the same roots as before): applied, and the set holds.
    write_raw(&root, &good)?;
    assert_eq!(skills.reload(), applied(&[], &[], &[])?);
    assert_eq!(skills.reload(), Reload::Unchanged);
    assert_eq!(ids(&skills.snapshot()), vec!["alpha@1.0.0"]);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A valid file with no roots is the explicit revoke-all.
#[test]
fn a_file_with_no_roots_drops_every_skill() -> R<()> {
    let root = test_root("revoke")?;
    package(&root, &alpha())?;
    package(&root, &beta())?;
    write_roots(&root, &[public(&KEY_A), public(&KEY_B)])?;
    let skills = load(&root, &["alpha", "beta"])?;
    assert_eq!(ids(&skills.snapshot()), vec!["alpha@1.0.0", "beta@1.0.0"]);
    write_roots(&root, &[])?;
    assert_eq!(
        skills.reload(),
        applied(&[], &["alpha@1.0.0", "beta@1.0.0"], &[])?
    );
    assert!(ids(&skills.snapshot()).is_empty());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// The same `name@version` with new bytes is drift: refused at reload.
/// A new version is an update: admitted at the same reload.
#[test]
fn drift_under_a_pinned_id_is_refused_and_a_new_version_admits() -> R<()> {
    let root = test_root("drift")?;
    package(&root, &alpha())?;
    write_roots(&root, &[public(&KEY_A)])?;
    let skills = load(&root, &["alpha"])?;
    // Same id, new bytes; and a genuine update beside it.
    std::fs::write(
        root.join("skills").join("alpha").join("main.py"),
        b"changed",
    )
    .map_err(err)?;
    package(
        &root,
        &Package {
            dir: "alpha-2",
            version: "2.0.0",
            code: b"v2",
            ..alpha()
        },
    )?;
    // A roots change triggers the reload (B added as previous).
    write_roots(&root, &[public(&KEY_A), public(&KEY_B)])?;
    assert_eq!(
        skills.reload(),
        applied(&["alpha@2.0.0"], &["alpha@1.0.0"], &["alpha@1.0.0"])?
    );
    assert_eq!(ids(&skills.snapshot()), vec!["alpha@2.0.0"]);
    // Still refused at the next reload: the pin is the first admitted
    // hash, not whatever the last set held, so drift cannot slip back in.
    write_roots(&root, &[public(&KEY_A)])?;
    assert_eq!(skills.reload(), applied(&[], &[], &["alpha@1.0.0"])?);
    // The original bytes back: the pinned id matches again and admits.
    std::fs::write(root.join("skills").join("alpha").join("main.py"), b"a").map_err(err)?;
    write_roots(&root, &[public(&KEY_A), public(&KEY_B)])?;
    assert_eq!(skills.reload(), applied(&["alpha@1.0.0"], &[], &[])?);
    assert_eq!(ids(&skills.snapshot()), vec!["alpha@1.0.0", "alpha@2.0.0"]);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// The watcher thread applies a rotation on its own, with no call to
/// `reload` from anyone.
#[test]
fn the_watcher_applies_a_rotation_by_itself() -> R<()> {
    let root = test_root("watcher")?;
    package(&root, &beta())?;
    write_roots(&root, &[public(&KEY_A)])?;
    let skills = Arc::new(load(&root, &["beta"])?);
    assert!(ids(&skills.snapshot()).is_empty());
    let watcher = Arc::clone(&skills);
    std::thread::spawn(move || watcher.watch(Duration::from_millis(20)));
    write_roots(&root, &[public(&KEY_B)])?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while ids(&skills.snapshot()).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(ids(&skills.snapshot()), vec!["beta@1.0.0"]);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// Startup with skills enabled needs a present, trusted, valid file: the
/// broker refuses to start otherwise. With skills disabled nothing is read,
/// nothing is watched, and a reload is a no-op.
#[test]
fn startup_refuses_a_missing_or_bad_file_and_disabled_skills_read_nothing() -> R<()> {
    let root = test_root("startup")?;
    package(&root, &alpha())?;
    let cfg = config(&root, &["alpha"])?;
    let refused = |what: &str| {
        let result = Skills::load(&cfg);
        assert!(
            matches!(
                &result,
                Err(Error::InvalidConfig {
                    key: "HATCHLING_SKILL_TRUST_ROOTS",
                    ..
                })
            ),
            "{what}: {result:?}"
        );
    };
    refused("missing");
    write_raw(&root, b"garbage\n")?;
    refused("malformed");
    write_roots(&root, &[public(&KEY_A)])?;
    std::fs::set_permissions(roots_path(&root), std::fs::Permissions::from_mode(0o622))
        .map_err(err)?;
    refused("world-writable");

    let disabled = Skills::load(&SkillConfig::disabled()).map_err(err)?;
    assert!(ids(&disabled.snapshot()).is_empty());
    assert_eq!(disabled.reload(), Reload::Disabled);
    assert_eq!(Skills::default().reload(), Reload::Disabled);
    // The watcher returns at once when there is nothing to watch.
    disabled.watch(Duration::from_millis(1));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A snapshot taken before a swap keeps its skills; a snapshot taken after
/// sees the new set. (The real-cell version is in `skill_run.rs`.)
#[test]
fn a_snapshot_taken_before_a_swap_is_unaffected_by_it() -> R<()> {
    let root = test_root("snapshot")?;
    package(&root, &alpha())?;
    write_roots(&root, &[public(&KEY_A)])?;
    let skills = load(&root, &["alpha"])?;
    let before = skills.snapshot();
    write_roots(&root, &[])?;
    assert!(matches!(skills.reload(), Reload::Applied { .. }));
    assert_eq!(ids(&before), vec!["alpha@1.0.0"]);
    assert!(ids(&skills.snapshot()).is_empty());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
