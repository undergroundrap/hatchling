//! Component: the broker's skill session against raw socket clients.
//!
//! The real [`serve_connection`](hatchling_broker::serve_connection)
//! pipeline runs on a background thread over a socketpair, with skills
//! loaded by the same [`Skills::load`] the broker runs at startup from a
//! real skills dir of signed packages. The test client speaks the raw
//! skill protocol. These prove the session path: exact-id lookup,
//! the call-time capability check, typed denials that leave the session
//! running, and a malformed capability refused at the wire.
//!
//! Reads carry a timeout so a broker regression fails the test instead
//! of hanging the suite.

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use hatchling_broker::{Config, EgressConfig, SkillConfig, Skills, VaultConfig, serve_connection};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, CapabilityName, LogLevel, PeerUid, SessionRequest,
    SessionResponse, SkillOp, SkillResult, SocketPath,
};
use hatchling_skill::{Manifest, SignedManifest};
use hatchling_vault::MasterKey;

const SIGNING_SECRET: [u8; 32] = [0x44; 32];

fn real_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

fn frame_io(error: hatchling_core::Error) -> io::Error {
    io::Error::other(error)
}

fn cap(raw: &str) -> io::Result<CapabilityName> {
    raw.parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn test_root(name: &str) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "hatchling-test-skill-session-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn signed(name: &str, capabilities: &[&str]) -> io::Result<SignedManifest> {
    let manifest = Manifest {
        name: name.to_owned(),
        version: String::from("1.0.0"),
        capabilities: capabilities
            .iter()
            .map(|raw| cap(raw))
            .collect::<io::Result<_>>()?,
        entry_point: String::from("main.py"),
    };
    let payload = manifest.canonical_bytes().map_err(io::Error::other)?;
    let signature = SigningKey::from_bytes(&SIGNING_SECRET).sign(&payload);
    Ok(SignedManifest {
        manifest,
        signature: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        )),
    })
}

fn write_package(skills_dir: &Path, dir: &str, manifest: &SignedManifest) -> io::Result<()> {
    let package = skills_dir.join(dir);
    std::fs::create_dir_all(&package)?;
    let json = manifest.to_json().map_err(io::Error::other)?;
    std::fs::write(package.join("manifest.json"), json)?;
    std::fs::write(package.join("main.py"), b"print('hi')")
}

/// A skill config for `skills_dir`, granting each skill the listed
/// capabilities, under the fixed test trust root.
fn skill_config(skills_dir: &Path, grants: &[(&str, &[&str])]) -> io::Result<SkillConfig> {
    let mut granted = HashMap::new();
    for (skill, caps) in grants {
        let caps = caps
            .iter()
            .map(|raw| cap(raw))
            .collect::<io::Result<HashSet<_>>>()?;
        granted.insert((*skill).to_owned(), caps);
    }
    // The trust-roots file sits in the skills dir; the loader skips
    // anything that is not a directory.
    let trust_roots_file = skills_dir.join("trust-roots");
    let key = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        SigningKey::from_bytes(&SIGNING_SECRET)
            .verifying_key()
            .to_bytes(),
    );
    std::fs::write(
        &trust_roots_file,
        format!("hatchling-trust-roots v1\nprimary {key}\nend\n"),
    )?;
    std::fs::set_permissions(
        &trust_roots_file,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
    )?;
    Ok(SkillConfig {
        skills_dir: skills_dir.to_path_buf(),
        trust_roots_file,
        grants: granted,
    })
}

/// Loads `skills` exactly as broker startup does, then runs the real
/// pipeline on a background thread and returns the handshaken client.
fn spawn_session(root: &Path, skills: SkillConfig) -> io::Result<UnixStream> {
    let vault_root = root.join("vault");
    let config = Config {
        socket: "/tmp/hatchling-test-skill-session-dummy.sock"
            .parse::<SocketPath>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
        agent_uid: PeerUid::from_kernel(real_uid()),
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
        skills,
        memory: hatchling_broker::MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: hatchling_broker::ArtifactConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
        goals: hatchling_broker::GoalsConfig::disabled(),
    };
    let loaded = Skills::load(&config.skills).map_err(io::Error::other)?;
    let key = MasterKey::generate().map_err(io::Error::other)?;
    let (client, server) = UnixStream::pair()?;
    client.set_read_timeout(Some(Duration::from_secs(5)))?;
    std::thread::spawn(move || {
        let _ = serve_connection(&server, &config, &key, &loaded);
    });
    let hello = AgentMessage::Hello {
        v: hatchling_core::PROTOCOL_VERSION,
        cell_id: String::from("cell-01"),
    };
    let payload = serde_json::to_vec(&hello)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_frame(&client, &payload).map_err(frame_io)?;
    let reply = read_frame(&client).map_err(frame_io)?;
    let message: BrokerMessage = serde_json::from_slice(&reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    assert!(
        matches!(message, BrokerMessage::Welcome { .. }),
        "expected WELCOME, got {message:?}"
    );
    Ok(client)
}

fn invoke(stream: &UnixStream, skill: &str, capability: &str) -> io::Result<SkillResult> {
    let request = SessionRequest::Skill(SkillOp::Invoke {
        skill: skill.to_owned(),
        capability: cap(capability)?,
        input: String::from("input"),
    });
    let payload = serde_json::to_vec(&request)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_frame(stream, &payload).map_err(frame_io)?;
    let reply = read_frame(stream).map_err(frame_io)?;
    let response: SessionResponse = serde_json::from_slice(&reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match response {
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
        | SessionResponse::Browser(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "broker gave a non-skill reply to a skill op",
        )),
    }
}

/// Unknown skills and undeclared capabilities are typed denials, each
/// distinct, and the session survives every one of them. The id is
/// exact: a bare name or another version names no skill.
#[test]
fn denials_are_typed_and_the_session_continues() {
    let root = test_root("denials").unwrap();
    let skills_dir = root.join("skills");
    write_package(
        &skills_dir,
        "echo",
        &signed("echo", &["text.read"]).unwrap(),
    )
    .unwrap();
    let client = spawn_session(
        &root,
        skill_config(&skills_dir, &[("echo", &["text.read", "net.egress"])]).unwrap(),
    )
    .unwrap();

    for skill in ["nope@1.0.0", "echo", "echo@2.0.0", "echo@", "@1.0.0", ""] {
        assert_eq!(
            invoke(&client, skill, "text.read").unwrap(),
            SkillResult::UnknownSkill,
            "{skill:?}"
        );
    }
    // Granted to the skill but not declared in its manifest: the call
    // is checked against the admitted declarations, not the grants.
    assert_eq!(
        invoke(&client, "echo@1.0.0", "net.egress").unwrap(),
        SkillResult::CapabilityDenied
    );
    assert_eq!(
        invoke(&client, "echo@1.0.0", "fs.write").unwrap(),
        SkillResult::CapabilityDenied
    );
    // Declared: every check passes. These skills are loaded without a
    // runtime, so the call fails closed with the opaque error; still a
    // reply, still the same session. (Real runs: tests/skill_run.rs.)
    assert_eq!(
        invoke(&client, "echo@1.0.0", "text.read").unwrap(),
        SkillResult::SkillError
    );
    assert_eq!(
        invoke(&client, "nope@1.0.0", "text.read").unwrap(),
        SkillResult::UnknownSkill
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A capability name the parser refuses never becomes a request: the
/// frame is a protocol violation and the broker ends the session.
#[test]
fn a_malformed_capability_on_the_wire_ends_the_session() {
    let root = test_root("malformed-capability").unwrap();
    let skills_dir = root.join("skills");
    write_package(
        &skills_dir,
        "echo",
        &signed("echo", &["text.read"]).unwrap(),
    )
    .unwrap();
    let client = spawn_session(
        &root,
        skill_config(&skills_dir, &[("echo", &["text.read"])]).unwrap(),
    )
    .unwrap();
    let frame = br#"{"kind":"skill","op":"invoke","skill":"echo@1.0.0","capability":"text read","input":""}"#;
    write_frame(&client, frame).unwrap();
    // No reply: the broker closed the session (EOF), it did not answer.
    assert!(
        read_frame(&client).is_err(),
        "broker must not answer a malformed op"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Startup loading refuses a broken skill and serves the rest: the
/// tampered package names no skill, the good one passes every check.
#[test]
fn a_refused_skill_is_unknown_and_the_rest_still_serve() {
    let root = test_root("refused").unwrap();
    let skills_dir = root.join("skills");
    let mut tampered = signed("tampered", &["text.read"]).unwrap();
    tampered.manifest.version = String::from("1.0.1");
    write_package(&skills_dir, "tampered", &tampered).unwrap();
    write_package(
        &skills_dir,
        "good",
        &signed("good", &["text.read"]).unwrap(),
    )
    .unwrap();
    let client = spawn_session(
        &root,
        skill_config(
            &skills_dir,
            &[("tampered", &["text.read"]), ("good", &["text.read"])],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        invoke(&client, "tampered@1.0.1", "text.read").unwrap(),
        SkillResult::UnknownSkill
    );
    assert_eq!(
        invoke(&client, "good@1.0.0", "text.read").unwrap(),
        SkillResult::SkillError
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// With skills disabled every skill op names an unknown skill.
#[test]
fn with_skills_disabled_every_skill_is_unknown() {
    let root = test_root("disabled").unwrap();
    let client = spawn_session(&root, SkillConfig::disabled()).unwrap();
    assert_eq!(
        invoke(&client, "echo@1.0.0", "text.read").unwrap(),
        SkillResult::UnknownSkill
    );
    let _ = std::fs::remove_dir_all(&root);
}
