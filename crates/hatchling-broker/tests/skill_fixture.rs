//! Malicious skill fixture: exfiltration via undeclared capability,
//! contained end-to-end.
//!
//! The fixture proves the enforcement chain:
//!
//! 1. A skill with a valid signature and benign declarations (`fs.read`)
//!    is admitted — admission checks the manifest, not the code's
//!    behavior.
//! 2. The skill's code attempts exfiltration via an undeclared
//!    capability (`net.egress`).
//! 3. The broker's call-time enforcement (`check_skill_call`) denies
//!    the invocation with a typed [`SkillDenial::UndeclaredCapability`].
//! 4. The exfiltration is contained: the denial happens before any
//!    skill code runs with the undeclared capability.
//!
//! This is the 380-class lesson as a test: a check that exists at
//! admission but not at use is a missing check. The fixture proves the
//! use-time check exists and fires.

#![allow(clippy::panic)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]
#![allow(clippy::match_wildcard_for_single_variants)]

use std::collections::HashSet;

use hatchling_broker::{SkillDenial, check_skill_call};
use hatchling_core::CapabilityName;
use hatchling_skill::{SkillRegistry, verify_admission};

fn cap(raw: &str) -> CapabilityName {
    raw.parse().unwrap()
}

/// Build a registry with a "malicious" skill: valid signature,
/// declares only `fs.read`, but the test will attempt `net.egress`
/// (the exfiltration capability).
fn malicious_skill_registry() -> (SkillRegistry, hatchling_skill::SkillId) {
    use ed25519_dalek::{Signer, SigningKey};

    // Fixed test key.
    let secret = [0x42; 32];
    let signing = SigningKey::from_bytes(&secret);
    let verifying = signing.verifying_key().to_bytes();

    // The manifest declares ONLY fs.read — benign on its face.
    // The signature is valid; admission will succeed.
    let manifest = hatchling_skill::Manifest::for_program(
        "evil-exfil",
        "1.0.0",
        "A test skill.",
        "main.py",
        b"# evil code tries net.egress",
        vec![cap("fs.read")],
    );
    let canonical = manifest.canonical_bytes().unwrap();
    let signature = signing.sign(&canonical);
    let signed = hatchling_skill::SignedManifest {
        manifest,
        signature: Some(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            signature.to_bytes(),
        )),
    };

    // User grants include net.egress (the skill *could* have declared it,
    // but didn't — the attack is using an undeclared capability).
    let grants: HashSet<CapabilityName> = [cap("fs.read"), cap("net.egress")].into_iter().collect();

    let admission = verify_admission(&signed, &verifying, &grants).expect("admission");

    let mut registry = SkillRegistry::new();
    // The "code" — in a real attack this would be Python that tries
    // to open a socket. Here the code bytes are just pinned; the
    // exfiltration attempt is modeled as the capability check.
    let id = registry
        .register(admission, b"# evil code tries net.egress")
        .expect("register");

    (registry, id)
}

/// The skill is admitted (valid signature, declarations within grants).
#[test]
fn malicious_skill_is_admitted() {
    let (registry, id) = malicious_skill_registry();
    // Admission succeeded — the skill is in the registry.
    assert!(registry.get(&id).is_some());
    // It declared fs.read.
    let skill = registry.get(&id).unwrap();
    skill.check_call(&cap("fs.read")).expect("declared");
}

/// The exfiltration attempt via undeclared capability is denied at
/// call time with a typed denial.
#[test]
fn exfiltration_via_undeclared_capability_is_denied() {
    let (registry, id) = malicious_skill_registry();

    // The skill attempts to use net.egress — NOT declared in its manifest.
    // This is the exfiltration attempt.
    let denial = check_skill_call(&registry, &id, &cap("net.egress"))
        .expect_err("exfiltration must be denied");

    // The denial is typed: UndeclaredCapability, not a generic error.
    match denial {
        SkillDenial::UndeclaredCapability {
            id: denied_id,
            capability,
        } => {
            assert_eq!(denied_id, id);
            assert_eq!(capability, cap("net.egress"));
        }
        other => panic!("wrong denial type: {other:?}"),
    }
}

/// The denial happens before any skill code runs: the check is on the
/// broker side, not in the skill cell. A skill that is denied never
/// gets its cell spawned with the undeclared capability.
#[test]
fn denied_skill_never_reaches_cell_spawn() {
    let (registry, id) = malicious_skill_registry();

    // The enforcement point is check_skill_call. If it returns Err,
    // the broker must not proceed to spawn_skill_cell or route the
    // invocation. This test documents the contract: Err → no spawn.
    let result = check_skill_call(&registry, &id, &cap("net.egress"));
    assert!(result.is_err(), "broker must not spawn on denial");
    // In the real broker path (serve.rs), this Err leads to a loud
    // tracing::error! and the invocation is dropped. The skill cell
    // is never spawned for the denied capability.
}

/// A benign skill using its declared capability is allowed.
#[test]
fn benign_capability_use_is_allowed() {
    let (registry, id) = malicious_skill_registry();
    // fs.read was declared — this is allowed.
    check_skill_call(&registry, &id, &cap("fs.read")).expect("declared capability must be allowed");
}
