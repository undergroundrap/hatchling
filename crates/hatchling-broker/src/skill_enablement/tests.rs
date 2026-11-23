use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use ed25519_dalek::{Signer as _, SigningKey};
use hatchling_core::ui::SkillState;
use hatchling_skill::{MANIFEST_FILE, Manifest, SignedManifest, SkillId};
use hatchling_vault::MasterKey;

use super::*;
use crate::SkillConfig;
use crate::api_auth::tests::{Scratch, owner_config, scratch};
use crate::trust_roots::TRUST_ROOTS_HEADER;

/// A time skills are enabled at in these tests.
const T: i64 = 1_790_000_000;

const SECRET: [u8; 32] = [0x2b; 32];

/// `name`, its program declaring `caps`, signed under [`SECRET`].
fn signed(name: &str, caps: &[&str]) -> SignedManifest {
    let manifest = Manifest::for_program(
        name,
        "1.0.0",
        "A test skill.",
        "main.py",
        b"print(1)",
        caps.iter().map(|cap| cap.parse().unwrap()).collect(),
    );
    let signature = SigningKey::from_bytes(&SECRET).sign(&manifest.canonical_bytes().unwrap());
    SignedManifest {
        manifest,
        signature: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        )),
    }
}

/// Puts `name` in the skills dir, declaring `caps`.
fn put(config: &Config, name: &str, caps: &[&str]) {
    let dir = config.skills.skills_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.join(MANIFEST_FILE),
        signed(name, caps).to_json().unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("main.py"), b"print(1)").unwrap();
}

/// A config with skills on in a scratch directory, `web` found in it.
fn on(name: &str) -> (Config, Scratch, MasterKey) {
    let scratch = scratch(&format!("skills-enabled-{name}"));
    let mut config = owner_config(None);
    config.skills = SkillConfig {
        skills_dir: scratch.0.join("skills"),
        trust_roots_file: scratch.0.join("roots"),
        enablements: scratch.0.join("enabled"),
    };
    std::fs::create_dir_all(&config.skills.skills_dir).unwrap();
    std::fs::set_permissions(
        &config.skills.skills_dir,
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let public = SigningKey::from_bytes(&SECRET).verifying_key().to_bytes();
    let public = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, public);
    std::fs::write(
        &config.skills.trust_roots_file,
        format!("{TRUST_ROOTS_HEADER}\nprimary {public}\nend\n"),
    )
    .unwrap();
    std::fs::set_permissions(
        &config.skills.trust_roots_file,
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    put(&config, "web", &["net.egress"]);
    (config, scratch, MasterKey::generate().unwrap())
}

fn op(config: &Config, keys: &MasterKey, skills: &Skills, op: &SkillsOp) -> SkillsResult {
    handle_skills_op(config, keys, skills, op, T)
}

fn listed(config: &Config, keys: &MasterKey, skills: &Skills) -> Vec<SkillListing> {
    match op(config, keys, skills, &SkillsOp::List) {
        SkillsResult::Listed { skills } => skills,
        other @ (SkillsResult::Enabled
        | SkillsResult::Disabled
        | SkillsResult::NotFound
        | SkillsResult::Changed
        | SkillsResult::Off
        | SkillsResult::Failed) => unreachable!("{other:?}"),
    }
}

fn runs(skills: &Skills, name: &str) -> bool {
    skills
        .snapshot()
        .registry()
        .get(&SkillId::new(name, "1.0.0"))
        .is_some()
}

fn enable(name: &str, digest: &str) -> SkillsOp {
    SkillsOp::Enable {
        name: name.to_owned(),
        digest: digest.to_owned(),
    }
}

/// With skills off, every op says so, and nothing is opened.
#[test]
fn every_op_is_off_when_skills_are() {
    let config = owner_config(None);
    let keys = MasterKey::generate().unwrap();
    let skills = Skills::load(&config.skills).unwrap();
    assert!(Enablements::of(&config, &keys).is_none());
    for each in [
        SkillsOp::List,
        enable("web", &"a".repeat(64)),
        SkillsOp::Disable {
            name: String::from("web"),
        },
    ] {
        assert_eq!(op(&config, &keys, &skills, &each), SkillsResult::Off);
    }
    // And the start hands nothing over.
    load_enabled(&config, &keys, &skills);
    assert!(skills.enabled().is_empty());
    // A directory for enablements with skills off opens nothing either.
    let mut config = config;
    config.skills.enablements = PathBuf::from("/srv/skills-enabled");
    assert!(Enablements::of(&config, &keys).is_none());
    assert_eq!(
        op(&config, &keys, &skills, &SkillsOp::List),
        SkillsResult::Off
    );
}

/// The store names its directory when debugged, and holds nothing more to
/// show: never a key.
#[test]
fn the_store_debugs_as_its_directory() {
    let (config, _scratch, keys) = on("debug");
    let store = Enablements::of(&config, &keys).unwrap();
    assert_eq!(
        format!("{store:?}"),
        format!(
            "Enablements {{ root: {:?}, .. }}",
            config.skills.enablements
        )
    );
}

/// Found, a skill waits; enabled at the digest the owner saw, it runs, kept
/// across a restart; disabled, it stops at once.
#[test]
fn enabling_runs_a_skill_and_disabling_stops_it_both_kept() {
    let (config, _scratch, keys) = on("cycle");
    let skills = Skills::load(&config.skills).unwrap();
    let seen = listed(&config, &keys, &skills);
    let web = seen.first().unwrap().clone();
    assert_eq!((web.name.as_str(), web.state), ("web", SkillState::Waiting));
    assert!(!runs(&skills, "web"));

    // Not the digest the owner saw, or no such skill: nothing enabled.
    let other = "0".repeat(64);
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &other)),
        SkillsResult::Changed
    );
    assert_eq!(
        op(&config, &keys, &skills, &enable("mail", &web.digest)),
        SkillsResult::NotFound
    );
    assert!(!runs(&skills, "web"));
    assert!(
        Enablements::of(&config, &keys)
            .unwrap()
            .enabled()
            .unwrap()
            .is_empty()
    );

    // At its digest: it runs now, and is listed enabled.
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &web.digest)),
        SkillsResult::Enabled
    );
    assert!(runs(&skills, "web"));
    assert_eq!(
        listed(&config, &keys, &skills).first().unwrap().state,
        SkillState::Enabled
    );
    // Enabling it again keeps one record.
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &web.digest)),
        SkillsResult::Enabled
    );
    let kept = Enablements::of(&config, &keys).unwrap().enabled().unwrap();
    assert_eq!(kept, Enabled::from([(String::from("web"), web.digest)]));

    // A broker started again runs it only once the store is handed over.
    let again = Skills::load(&config.skills).unwrap();
    assert!(!runs(&again, "web"));
    load_enabled(&config, &keys, &again);
    assert!(runs(&again, "web"));

    // Disabled: it stops at once, the record goes, and it waits again.
    let disable = SkillsOp::Disable {
        name: String::from("web"),
    };
    assert_eq!(
        op(&config, &keys, &skills, &disable),
        SkillsResult::Disabled
    );
    assert!(!runs(&skills, "web"));
    assert!(
        Enablements::of(&config, &keys)
            .unwrap()
            .enabled()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        listed(&config, &keys, &skills).first().unwrap().state,
        SkillState::Waiting
    );
    // Disabling it again is no error; one never found is not found.
    assert_eq!(
        op(&config, &keys, &skills, &disable),
        SkillsResult::Disabled
    );
    assert_eq!(
        op(
            &config,
            &keys,
            &skills,
            &SkillsOp::Disable {
                name: String::from("mail")
            }
        ),
        SkillsResult::NotFound
    );
}

/// A skill put in the skills dir since the broker started shows at the
/// next list, with no restart, and waits.
#[test]
fn a_skill_put_there_since_shows_at_the_next_list() {
    let (config, _scratch, keys) = on("since");
    let skills = Skills::load(&config.skills).unwrap();
    assert_eq!(listed(&config, &keys, &skills).len(), 1);
    put(&config, "mail", &["net.egress"]);
    let names: Vec<(String, SkillState)> = listed(&config, &keys, &skills)
        .into_iter()
        .map(|listing| (listing.name, listing.state))
        .collect();
    assert_eq!(
        names,
        [
            (String::from("mail"), SkillState::Waiting),
            (String::from("web"), SkillState::Waiting)
        ]
    );
}

/// A skill the signer changes after the owner enabled it stops, is listed
/// changed, and runs again only once the owner enables what it is now.
#[test]
fn a_changed_skill_waits_for_the_owner_again() {
    let (config, _scratch, keys) = on("changed");
    let skills = Skills::load(&config.skills).unwrap();
    let before = listed(&config, &keys, &skills).first().unwrap().clone();
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &before.digest)),
        SkillsResult::Enabled
    );
    // Re-signed declaring more, at the same version: the next list reads
    // it as it is, changed, and it stops.
    put(&config, "web", &["net.egress", "fs.write"]);
    let now = listed(&config, &keys, &skills).first().unwrap().clone();
    assert!(!runs(&skills, "web"));
    assert_eq!(now.state, SkillState::Changed);
    assert_ne!(now.digest, before.digest);
    assert_eq!(now.capabilities, ["net.egress", "fs.write"]);
    // The old digest no longer enables it; the new one does, and it runs.
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &before.digest)),
        SkillsResult::Changed
    );
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &now.digest)),
        SkillsResult::Enabled
    );
    assert!(runs(&skills, "web"));
    skills.refresh();
    assert!(runs(&skills, "web"));
    // Changed between the owner's list and their enable: the enable reads
    // the skill as it is, and refuses the digest they saw.
    let seen = listed(&config, &keys, &skills).first().unwrap().clone();
    put(&config, "web", &["fs.read"]);
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &seen.digest)),
        SkillsResult::Changed
    );
    assert!(!runs(&skills, "web"));
}

/// The store keeps at most [`MAX_ENABLED`], and past that refuses a new one
/// without dropping any.
#[test]
fn the_store_is_bounded() {
    let (config, _scratch, keys) = on("bounded");
    let skills = Skills::load(&config.skills).unwrap();
    let web = listed(&config, &keys, &skills).first().unwrap().clone();
    let store = Enablements::of(&config, &keys).unwrap();
    let full = Kept {
        enabled: (0..MAX_ENABLED)
            .map(|n| Record {
                name: format!("skill-{n}"),
                digest: "a".repeat(64),
                version: String::from("1"),
                signer: String::new(),
                capabilities: Vec::new(),
                enabled_at: T,
            })
            .collect(),
    };
    Enablements::write(&store.vault().unwrap(), &full).unwrap();
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &web.digest)),
        SkillsResult::Failed
    );
    assert_eq!(store.enabled().unwrap().len(), MAX_ENABLED);
    assert!(!runs(&skills, "web"));
    // One of those enabled again still fits: it replaces itself.
    let mut one_less = full;
    one_less.enabled.pop();
    Enablements::write(&store.vault().unwrap(), &one_less).unwrap();
    assert_eq!(
        op(&config, &keys, &skills, &enable("web", &web.digest)),
        SkillsResult::Enabled
    );
    assert_eq!(store.enabled().unwrap().len(), MAX_ENABLED);
}

/// A store holding what enablements can't enables nothing, and every op
/// that reads it fails.
#[test]
fn an_unsound_store_enables_nothing() {
    let (config, _scratch, keys) = on("unsound");
    let skills = Skills::load(&config.skills).unwrap();
    let web = listed(&config, &keys, &skills).first().unwrap().clone();
    let store = Enablements::of(&config, &keys).unwrap();
    let record = |name: &str, digest: &str| Record {
        name: name.to_owned(),
        digest: digest.to_owned(),
        version: String::from("1.0.0"),
        signer: String::new(),
        capabilities: Vec::new(),
        enabled_at: T,
    };
    let short: String = web.digest.chars().take(63).collect();
    let unsound = [
        vec![record("web", &web.digest), record("web", &web.digest)],
        vec![record("web", &web.digest.to_uppercase())],
        vec![record("web", &short)],
        vec![record("web", &format!("{short}g"))],
        vec![record("", &web.digest)],
        vec![record(&"w".repeat(129), &web.digest)],
        vec![Record {
            enabled_at: -1,
            ..record("web", &web.digest)
        }],
    ];
    for enabled in unsound {
        Enablements::write(&store.vault().unwrap(), &Kept { enabled }).unwrap();
        assert!(store.enabled().is_err());
        load_enabled(&config, &keys, &skills);
        assert!(!runs(&skills, "web"));
        assert_eq!(
            op(&config, &keys, &skills, &enable("web", &web.digest)),
            SkillsResult::Failed
        );
    }
    // Corrupt bytes, the same.
    let name = Enablements::name().unwrap();
    store
        .vault()
        .unwrap()
        .store(&name, &Secret::new(b"{not json".to_vec()).unwrap())
        .unwrap();
    assert!(store.enabled().is_err());
    // A name at the bound is sound.
    Enablements::write(
        &store.vault().unwrap(),
        &Kept {
            enabled: vec![record(&"w".repeat(128), &web.digest)],
        },
    )
    .unwrap();
    assert_eq!(store.enabled().unwrap().len(), 1);
}

#[test]
fn a_digest_is_lowercase_hex_sha256() {
    assert!(is_digest(&"0123456789abcdef".repeat(4)));
    for not in [
        String::new(),
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        format!("{}g", "a".repeat(63)),
        format!("{}/", "a".repeat(63)),
        format!("{}:", "a".repeat(63)),
        format!("{}`", "a".repeat(63)),
    ] {
        assert!(!is_digest(&not), "{not}");
    }
}
