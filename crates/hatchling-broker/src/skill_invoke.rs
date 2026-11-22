//! Skill invocation: the broker's call-time enforcement point.
//!
//! When the agent (via the loop) invokes a skill tool, the broker:
//!
//! 1. Looks up the skill in the registry. Unknown skill → typed denial.
//! 2. Checks the capability against the admitted manifest via
//!    [`RegisteredSkill::check_call`]. Undeclared capability → typed
//!    denial + loud refusal log (the 380-class lesson: a check that
//!    exists at admission but not at use is a missing check).
//! 3. On success, the invocation proceeds to the skill's dedicated cell.
//!
//! Every denial is a typed [`SkillDenial`], never a silent pass. The
//! broker logs refusals loudly; the cell only ever sees the denial,
//! never the reason's internals.

use hatchling_core::CapabilityName;
use hatchling_skill::{SkillId, SkillRegistry};

/// A typed denial for a refused skill invocation.
///
/// The broker returns this to the caller; the skill cell never sees it.
/// Each variant is a distinct refusal reason, matched exhaustively by
/// the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillDenial {
    /// No skill with this id is registered.
    UnknownSkill {
        /// The requested skill id.
        id: String,
    },
    /// The skill is registered but the capability was not declared in
    /// its manifest. This is the call-time enforcement: declaration at
    /// admission is not enough, every call re-checks.
    UndeclaredCapability {
        /// The skill id.
        id: SkillId,
        /// The capability the skill tried to use.
        capability: CapabilityName,
    },
}

impl std::fmt::Display for SkillDenial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSkill { id } => {
                write!(f, "skill '{id}' denied: unknown skill")
            }
            Self::UndeclaredCapability { id, capability } => {
                write!(
                    f,
                    "skill '{id}' denied: capability '{capability}' was not declared"
                )
            }
        }
    }
}

/// Check a skill invocation against the registry.
///
/// This is the broker's call-time enforcement point. It verifies:
/// - The skill is registered (known id)
/// - The capability was declared in the admitted manifest
///
/// On success, returns `Ok(())` and the caller proceeds to route the
/// invocation to the skill's cell. On failure, returns the typed
/// denial and the broker logs it loudly.
///
/// This function is pure (no I/O): the registry lookup and capability
/// check are in-memory. The loud logging happens at the call site,
/// where the broker has the tracing context.
///
/// # Errors
///
/// Returns [`SkillDenial`] if the skill is unknown or the capability
/// was not declared in the skill's manifest.
pub fn check_skill_call(
    registry: &SkillRegistry,
    skill_id: &SkillId,
    capability: &CapabilityName,
) -> Result<(), SkillDenial> {
    let skill = registry
        .get(skill_id)
        .ok_or_else(|| SkillDenial::UnknownSkill {
            id: skill_id.to_string(),
        })?;

    skill
        .check_call(capability)
        .map_err(|_| SkillDenial::UndeclaredCapability {
            id: skill_id.clone(),
            capability: capability.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchling_skill::{Admission, SignedManifest};

    fn cap(raw: &str) -> CapabilityName {
        raw.parse().unwrap()
    }

    fn test_registry() -> (SkillRegistry, SkillId) {
        // Build a minimal registry with one skill declaring "fs.read".
        use ed25519_dalek::{Signer, SigningKey};
        use std::collections::HashSet;

        let secret = [0x01; 32];
        let signing = SigningKey::from_bytes(&secret);
        let verifying = signing.verifying_key().to_bytes();

        let manifest = hatchling_skill::Manifest::for_program(
            "test-skill",
            "1.0.0",
            "A test skill.",
            "main.py",
            b"code",
            vec![cap("fs.read")],
        );
        let canonical = manifest.canonical_bytes().unwrap();
        let signature = signing.sign(&canonical);
        let signed = SignedManifest {
            manifest,
            signature: Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                signature.to_bytes(),
            )),
        };
        let grants: HashSet<CapabilityName> = std::iter::once(cap("fs.read")).collect();
        let admission: Admission =
            hatchling_skill::verify_admission(&signed, &verifying, &grants).unwrap();

        let mut registry = SkillRegistry::new();
        let id = registry.register(admission, b"code").unwrap();
        (registry, id)
    }

    #[test]
    fn denials_display_their_reason() {
        assert_eq!(
            SkillDenial::UnknownSkill {
                id: String::from("x@1")
            }
            .to_string(),
            "skill 'x@1' denied: unknown skill"
        );
        assert_eq!(
            SkillDenial::UndeclaredCapability {
                id: SkillId::new("x", "1"),
                capability: cap("fs.write"),
            }
            .to_string(),
            "skill 'x@1' denied: capability 'fs.write' was not declared"
        );
    }

    #[test]
    fn declared_capability_allowed() {
        let (registry, id) = test_registry();
        check_skill_call(&registry, &id, &cap("fs.read")).unwrap();
    }

    #[test]
    fn undeclared_capability_denied() {
        let (registry, id) = test_registry();
        let denial = check_skill_call(&registry, &id, &cap("fs.write")).unwrap_err();
        assert_eq!(
            denial,
            SkillDenial::UndeclaredCapability {
                id,
                capability: cap("fs.write"),
            }
        );
    }

    #[test]
    fn unknown_skill_denied() {
        let (registry, _) = test_registry();
        let unknown = SkillId::new("nope", "1.0.0");
        let denial = check_skill_call(&registry, &unknown, &cap("fs.read")).unwrap_err();
        assert!(matches!(denial, SkillDenial::UnknownSkill { .. }));
    }
}
