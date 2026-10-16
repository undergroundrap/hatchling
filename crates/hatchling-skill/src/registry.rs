//! The skill registry: admission + identity pinning.
//!
//! The broker (or a broker-mediated registry) holds admitted skills.
//! Registration pins the skill's code hash at admission; every load
//! and every call re-verifies the code against the pinned hash.
//! Drift — code changed under a pinned identity — is refused.
//!
//! Never trust a worker's self-declaration.
//! The broker validates the skill's claimed identity against the
//! pinned identity. The signature covers the manifest; the
//! registry binds the manifest to the code it admits.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use hatchling_core::CapabilityName;
use sha2::{Digest, Sha256};

use crate::{Admission, Error};

/// The SHA-256 hash of a skill's code, pinned at admission.
///
/// This is the skill's identity for drift detection. It is computed
/// by the broker from the actual code bytes at registration, never
/// taken from the skill's self-declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CodeHash([u8; 32]);

impl CodeHash {
    /// Compute the hash of skill code bytes.
    #[must_use]
    pub fn of(code: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(code);
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        Self(bytes)
    }

    /// The raw hash bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for CodeHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// A unique identifier for a registered skill.
///
/// Derived from the manifest name + version at registration. The
/// broker uses this to look up skills; the code hash pins the
/// identity against drift.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkillId(String);

impl SkillId {
    /// Build from manifest name and version.
    #[must_use]
    pub fn new(name: &str, version: &str) -> Self {
        Self(format!("{name}@{version}"))
    }

    /// The string form (`name@version`).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SkillId {
    type Err = Error;

    /// Parses the exact `name@version` form, as a caller names a skill.
    ///
    /// The split is at the first `@`: admission refuses `@` in names,
    /// so that is the only decomposition a registered id can have, and
    /// the parsed id is byte-identical to the input. A bare name, an
    /// empty name or version, or one holding a control character names no
    /// skill.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownSkill`] for anything not of that form.
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.split_once('@') {
            Some((name, version))
                if !name.is_empty()
                    && !version.is_empty()
                    && !raw.chars().any(char::is_control) =>
            {
                Ok(Self::new(name, version))
            }
            _ => Err(Error::UnknownSkill(raw.to_owned())),
        }
    }
}

impl fmt::Display for SkillId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A skill admitted to the registry: the verified admission plus the
/// pinned code hash.
///
/// Only the registry constructs one, so holding a `RegisteredSkill`
/// proves the code matched the pinned hash at registration.
#[derive(Debug, Clone)]
pub struct RegisteredSkill {
    id: SkillId,
    admission: Admission,
    code_hash: CodeHash,
}

impl RegisteredSkill {
    /// The skill's registry identifier.
    #[must_use]
    pub const fn id(&self) -> &SkillId {
        &self.id
    }

    /// The verified admission (manifest + granted capabilities).
    #[must_use]
    pub const fn admission(&self) -> &Admission {
        &self.admission
    }

    /// The pinned code hash.
    #[must_use]
    pub const fn code_hash(&self) -> &CodeHash {
        &self.code_hash
    }

    /// Check a capability call against the admitted manifest.
    ///
    /// This is the broker's call-time enforcement: every skill tool
    /// call re-checks the capability against the declarations.
    /// Returns `Ok(())` for declared capabilities, `Err` for
    /// undeclared ones.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UndeclaredCapability`] if the capability was
    /// not declared in the manifest.
    pub fn check_call(&self, capability: &CapabilityName) -> Result<(), Error> {
        self.admission.check_call(capability)
    }
}

/// The broker's registry of admitted skills.
///
/// Registration is the trust boundary: the broker takes an `Admission`
/// (already verified by ) and the skill's code bytes, pins the
/// code hash, and only then lists the skill. Every lookup can re-verify
/// the code against the pinned hash to detect drift.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: HashMap<SkillId, RegisteredSkill>,
}

impl SkillRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an admitted skill with its code bytes.
    ///
    /// The code hash is computed from the actual bytes and pinned.
    /// If a skill with the same id is already registered, registration
    /// is refused — even if the code hash matches. Silent re-registration
    /// would let a caller swap a skill's code without any signal; the
    /// explicit `unregister` + `register` sequence is the only update
    /// path (both are auditable broker operations).
    ///
    /// # Errors
    ///
    /// Returns [`Error::AlreadyRegistered`] if the id is already
    /// registered.
    pub fn register(&mut self, admission: Admission, code: &[u8]) -> Result<SkillId, Error> {
        let manifest = admission.manifest();
        let id = SkillId::new(&manifest.name, &manifest.version);
        let code_hash = CodeHash::of(code);

        if self.skills.contains_key(&id) {
            return Err(Error::AlreadyRegistered(id.to_string()));
        }

        self.skills.insert(
            id.clone(),
            RegisteredSkill {
                id: id.clone(),
                admission,
                code_hash,
            },
        );
        Ok(id)
    }

    /// Remove a skill from the registry by id.
    ///
    /// This is the explicit half of the update path: `unregister` then
    /// `register` replaces a skill's code with a clear audit trail.
    /// Returns `true` if a skill was removed, `false` if the id was
    /// not registered.
    pub fn unregister(&mut self, id: &SkillId) -> bool {
        self.skills.remove(id).is_some()
    }

    /// Look up a registered skill by id.
    #[must_use]
    pub fn get(&self, id: &SkillId) -> Option<&RegisteredSkill> {
        self.skills.get(id)
    }

    /// Verify code bytes against the pinned hash for a registered skill.
    ///
    /// Call this at load time and at call time: if the code on disk
    /// (or presented by the cell) doesn't match the pinned hash,
    /// the skill has drifted and must be refused.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownSkill`] if the id is not registered.
    /// Returns [`Error::IdentityDrift`] if the hash doesn't match.
    pub fn verify_code(&self, id: &SkillId, code: &[u8]) -> Result<(), Error> {
        let skill = self
            .skills
            .get(id)
            .ok_or_else(|| Error::UnknownSkill(id.to_string()))?;
        let found = CodeHash::of(code);
        if skill.code_hash != found {
            return Err(Error::IdentityDrift {
                id: id.to_string(),
                expected: skill.code_hash.to_string(),
                found: found.to_string(),
            });
        }
        Ok(())
    }

    /// List all registered skill ids.
    #[must_use]
    pub fn list(&self) -> Vec<SkillId> {
        self.skills.keys().cloned().collect()
    }

    /// The number of registered skills.
    #[must_use]
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SignedManifest;
    use ed25519_dalek::{Signer, SigningKey};

    fn test_keypair() -> (SigningKey, [u8; 32]) {
        // Fixed test key: 32 bytes, deterministic for tests.
        let secret = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ];
        let signing = SigningKey::from_bytes(&secret);
        let verifying = signing.verifying_key().to_bytes();
        (signing, verifying)
    }

    fn signed_manifest(signing: &SigningKey, name: &str) -> SignedManifest {
        use crate::{Manifest, SignedManifest};
        let manifest = Manifest {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            capabilities: vec!["fs.read".parse().unwrap()],
            entry_point: "main.py".to_string(),
        };
        let canonical = manifest.canonical_bytes().unwrap();
        let signature = signing.sign(&canonical);
        SignedManifest {
            manifest,
            signature: Some(base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                signature.to_bytes(),
            )),
        }
    }

    fn admit(name: &str) -> Admission {
        let (signing, verify_bytes) = test_keypair();
        let signed = signed_manifest(&signing, name);
        let grants: std::collections::HashSet<CapabilityName> =
            std::iter::once("fs.read".parse().unwrap()).collect();
        crate::verify_admission(&signed, &verify_bytes, &grants).expect("admit")
    }

    #[test]
    fn registry_counts_and_lists_every_skill() {
        let mut registry = SkillRegistry::new();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
        assert!(registry.list().is_empty());
        registry
            .register(admit("alpha"), b"a")
            .expect("register alpha");
        registry
            .register(admit("beta"), b"b")
            .expect("register beta");
        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());
        let mut ids: Vec<String> = registry.list().iter().map(ToString::to_string).collect();
        ids.sort();
        assert_eq!(ids, vec!["alpha@1.0.0", "beta@1.0.0"]);
    }

    #[test]
    fn ids_and_hashes_display_canonically() {
        assert_eq!(SkillId::new("web", "1.0.0").to_string(), "web@1.0.0");
        // SHA-256 of the empty string, as lowercase hex.
        assert_eq!(
            CodeHash::of(b"").to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn register_and_lookup() {
        let admission = admit("test-skill");
        let mut registry = SkillRegistry::new();
        let code = b"print('hello')";
        let id = registry.register(admission, code).expect("register");
        assert_eq!(id.as_str(), "test-skill@1.0.0");
        let skill = registry.get(&id).expect("lookup");
        assert_eq!(skill.code_hash(), &CodeHash::of(code));
    }

    #[test]
    fn verify_code_detects_drift() {
        let admission = admit("drift-skill");
        let mut registry = SkillRegistry::new();
        let id = registry.register(admission, b"v1 code").expect("register");
        // Same code: ok.
        registry.verify_code(&id, b"v1 code").expect("same code");
        // Changed code: drift refused.
        let err = registry.verify_code(&id, b"v2 code").expect_err("drift");
        assert!(matches!(err, Error::IdentityDrift { .. }));
    }

    #[test]
    fn register_same_id_is_refused_even_with_same_code() {
        let admission = admit("dup-skill");
        let mut registry = SkillRegistry::new();
        registry
            .register(admission.clone(), b"code a")
            .expect("first");
        // Re-registration is refused even with identical code: silent
        // replacement is never allowed.
        let err = registry
            .register(admission.clone(), b"code a")
            .expect_err("re-register with same code refused");
        assert!(matches!(err, Error::AlreadyRegistered(_)));
        // Different code under same id: also refused.
        let err = registry
            .register(admission, b"code b")
            .expect_err("re-register with different code refused");
        assert!(matches!(err, Error::AlreadyRegistered(_)));
    }

    #[test]
    fn unregister_then_register_replaces_the_skill() {
        let admission = admit("update-skill");
        let mut registry = SkillRegistry::new();
        let id = registry
            .register(admission.clone(), b"code a")
            .expect("first");
        // Explicit unregister is the update path.
        assert!(registry.unregister(&id));
        assert!(!registry.unregister(&id));
        // Re-register with new code succeeds.
        let id2 = registry
            .register(admission, b"code b")
            .expect("re-register after unregister");
        assert_eq!(id, id2);
        assert!(registry.verify_code(&id2, b"code b").is_ok());
    }

    #[test]
    fn check_call_enforces_declared_capabilities() {
        let admission = admit("call-skill");
        let mut registry = SkillRegistry::new();
        let id = registry.register(admission, b"code").expect("register");
        let skill = registry.get(&id).expect("lookup");
        skill
            .check_call(&"fs.read".parse().unwrap())
            .expect("declared");
        let err = skill
            .check_call(&"fs.write".parse().unwrap())
            .expect_err("undeclared");
        assert!(matches!(err, Error::UndeclaredCapability { .. }));
    }

    #[test]
    fn skill_ids_parse_exactly_and_round_trip() {
        let id: SkillId = "web@1.0.0".parse().unwrap();
        assert_eq!(id, SkillId::new("web", "1.0.0"));
        // The split is at the first `@`; the rest is the version, and
        // the parsed id is the input byte for byte.
        let id: SkillId = "a@1@2".parse().unwrap();
        assert_eq!(id, SkillId::new("a", "1@2"));
        assert_eq!(id.as_str(), "a@1@2");
        for raw in [
            "web",
            "",
            "@",
            "@1.0.0",
            "web@",
            "web\n@1",
            "web@1\r",
            "w\u{1b}[2J@1",
        ] {
            assert!(
                matches!(raw.parse::<SkillId>(), Err(Error::UnknownSkill(ref s)) if s == raw),
                "{raw:?} must name no skill"
            );
        }
    }

    #[test]
    fn verify_code_unknown_skill() {
        let registry = SkillRegistry::new();
        let id = SkillId::new("nope", "1.0.0");
        let err = registry.verify_code(&id, b"code").expect_err("unknown");
        assert!(matches!(err, Error::UnknownSkill(_)));
    }
}
