//! Broker-side verification and admission.
//!
//! The broker verifies, the cell executes. Signature checks and
//! capability enforcement live on the privileged side; the cell
//! never self-attests.
//!
//! Admission gates, in order:
//! 1. **Signature present**: unsigned → refused.
//! 2. **Signature verifies** (`verify_strict`) over the domain-tagged v2
//!    payload, under the trust root: bad or foreign signature, or a weak
//!    trust root → refused.
//! 3. **Manifest valid**: the signed fields a loader acts on are sane
//!    (see [`Manifest::validate`]) → otherwise refused, signed or not.
//! 4. **Grant check**: the declared capabilities must be a subset of the
//!    user's grants. Over-privileged → refused at admission.
//!
//! Call-time enforcement is separate: every skill tool call re-checks
//! the capability against the admitted manifest's declarations.
//! A skill calling an undeclared capability is denied at call time,
//! not just at admission. (The 380-class lesson: a check that exists
//! at admission but not at use is a missing check.)

use std::collections::HashSet;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, VerifyingKey};
use hatchling_core::CapabilityName;

use crate::{Error, Manifest, SignedManifest};

/// A successfully admitted skill: the verified manifest plus the
/// capabilities the broker will enforce at call time.
///
/// Only [`verify_admission`] makes one. The fields are private, so
/// holding an `Admission` proves the manifest passed every gate, and its
/// capability set cannot be widened afterwards.
#[derive(Debug, Clone)]
pub struct Admission {
    manifest: SignedManifest,
    granted: HashSet<CapabilityName>,
    signer: [u8; 32],
}

impl Admission {
    /// The verified manifest.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest.manifest
    }

    /// The trust root whose key verified the manifest's signature: the
    /// skill's signing identity, as distinct from its name.
    #[must_use]
    pub const fn signer(&self) -> &[u8; 32] {
        &self.signer
    }

    /// The signing identity as a stable display form: the SHA-256 of the
    /// verifying key, lowercase hex. Two skills share it exactly when the
    /// same key signed both, whatever their names.
    #[must_use]
    pub fn signer_fingerprint(&self) -> String {
        use sha2::Digest as _;
        use std::fmt::Write as _;
        sha2::Sha256::digest(self.signer)
            .iter()
            .fold(String::with_capacity(64), |mut out, byte| {
                let _ = write!(out, "{byte:02x}");
                out
            })
    }

    /// The declared capabilities that call-time checks enforce.
    #[must_use]
    pub const fn capabilities(&self) -> &HashSet<CapabilityName> {
        &self.granted
    }

    /// Check a capability at call time. Returns `Ok(())` if the
    /// capability was declared in the manifest; otherwise a typed
    /// denial.
    ///
    /// # Errors
    /// Returns [`Error::UndeclaredCapability`] if the capability was
    /// not declared in the manifest.
    pub fn check_call(&self, capability: &CapabilityName) -> Result<(), Error> {
        if self.granted.contains(capability) {
            Ok(())
        } else {
            Err(Error::UndeclaredCapability {
                name: self.manifest.manifest.name.clone(),
                capability: capability.clone(),
            })
        }
    }
}

/// Checks that `root` can serve as a trust root: a valid Ed25519 point,
/// and not a weak (small-order) key, under which a signature can verify
/// for any message.
///
/// # Errors
/// Returns [`Error::InvalidTrustRoot`] saying which.
pub fn check_trust_root(root: &[u8; 32]) -> Result<(), Error> {
    let key = VerifyingKey::from_bytes(root)
        .map_err(|e| Error::InvalidTrustRoot(format!("malformed Ed25519 public key: {e}")))?;
    if key.is_weak() {
        return Err(Error::InvalidTrustRoot(String::from(
            "weak (small-order) key: any signature could verify under it",
        )));
    }
    Ok(())
}

/// Verify a signed manifest and admit it against the user's grants.
///
/// # Arguments
/// * `signed` — the parsed signed manifest
/// * `trust_root` — the Ed25519 verifying key (32 bytes) that must
///   have signed the manifest
/// * `user_grants` — the capabilities the user has granted; the
///   manifest's declarations must be a subset
///
/// # Errors
/// * `Unsigned` — no signature block
/// * `BadSignature` — signature present but invalid (or a weak key)
/// * `InvalidTrustRoot` — the trust root is not an Ed25519 point
/// * `InvalidManifest` — a signed field fails [`Manifest::validate`]
/// * `OverPrivileged` — declarations exceed user grants
pub fn verify_admission<S: std::hash::BuildHasher>(
    signed: &SignedManifest,
    trust_root: &[u8; 32],
    user_grants: &HashSet<CapabilityName, S>,
) -> Result<Admission, Error> {
    let manifest = &signed.manifest;

    // Gate 1: signature must be present.
    let sig_b64 = signed.signature.as_ref().ok_or_else(|| Error::Unsigned {
        name: manifest.name.clone(),
    })?;

    // Gate 2: signature must verify, strictly: `verify_strict` also
    // refuses weak (small-order) keys and signature points, under which
    // plain `verify` can accept a signature for any message.
    let verifying_key = VerifyingKey::from_bytes(trust_root)
        .map_err(|e| Error::InvalidTrustRoot(format!("malformed Ed25519 public key: {e}")))?;
    let sig_bytes = BASE64.decode(sig_b64).map_err(|_| Error::BadSignature {
        name: manifest.name.clone(),
    })?;
    let signature = Signature::from_slice(&sig_bytes).map_err(|_| Error::BadSignature {
        name: manifest.name.clone(),
    })?;
    let message = manifest
        .canonical_bytes()
        .map_err(|_| Error::BadSignature {
            name: manifest.name.clone(),
        })?;
    verifying_key
        .verify_strict(&message, &signature)
        .map_err(|_| Error::BadSignature {
            name: manifest.name.clone(),
        })?;

    // Gate 3: the signed fields a loader acts on must be sane.
    manifest.validate()?;

    // Gate 4: declarations must be a subset of user grants.
    let excess: Vec<CapabilityName> = manifest
        .capabilities()
        .iter()
        .filter(|cap| !user_grants.contains(*cap))
        .cloned()
        .collect();
    if !excess.is_empty() {
        return Err(Error::OverPrivileged {
            name: manifest.name.clone(),
            excess,
        });
    }

    let declared: HashSet<CapabilityName> = manifest.capabilities().iter().cloned().collect();
    Ok(Admission {
        manifest: signed.clone(),
        granted: declared,
        signer: *trust_root,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn sign_manifest(manifest: &Manifest, signing: &SigningKey) -> SignedManifest {
        let msg = manifest.canonical_bytes().unwrap();
        let sig = signing.sign(&msg);
        let sig_b64 = BASE64.encode(sig.to_bytes());
        SignedManifest {
            manifest: manifest.clone(),
            signature: Some(sig_b64),
        }
    }

    fn sample_manifest() -> Manifest {
        Manifest::for_program(
            "test-skill",
            "1.0.0",
            "A test skill.",
            "main.py",
            b"print(1)",
            vec![cap("net.egress")],
        )
    }

    fn cap(raw: &str) -> CapabilityName {
        raw.parse().unwrap()
    }

    fn net_grants() -> HashSet<CapabilityName> {
        HashSet::from([cap("net.egress")])
    }

    #[test]
    fn valid_signature_admits() {
        let (signing, verifying) = test_keypair();
        let signed = sign_manifest(&sample_manifest(), &signing);
        let admission = verify_admission(&signed, &verifying, &net_grants()).unwrap();
        assert_eq!(admission.manifest().name, "test-skill");
        assert_eq!(admission.capabilities(), &net_grants());
    }

    #[test]
    fn an_admission_records_the_key_that_signed_it() {
        use sha2::Digest as _;
        // the signing identity survives admission, so a
        // builder's provenance can name it rather than its display name.
        let (signing, verifying) = test_keypair();
        let signed = sign_manifest(&sample_manifest(), &signing);
        let admission = verify_admission(&signed, &verifying, &net_grants()).unwrap();
        assert_eq!(admission.signer(), &verifying);
        let fingerprint = admission.signer_fingerprint();
        assert_eq!(fingerprint.len(), 64);
        assert!(
            fingerprint
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        let digest = sha2::Sha256::digest(verifying);
        let expected: Vec<String> = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        let expected = expected.concat();
        assert_eq!(fingerprint, expected);
        // Same name, another key: another identity.
        let other = SigningKey::from_bytes(&[0x42; 32]);
        let other_root = other.verifying_key().to_bytes();
        let impostor = verify_admission(
            &sign_manifest(&sample_manifest(), &other),
            &other_root,
            &net_grants(),
        )
        .unwrap();
        assert_eq!(impostor.manifest().name, admission.manifest().name);
        assert_ne!(impostor.signer_fingerprint(), fingerprint);
        assert_eq!(impostor.signer(), &other_root);
    }

    #[test]
    fn unsigned_is_refused() {
        let (_, verifying) = test_keypair();
        let signed = SignedManifest {
            manifest: sample_manifest(),
            signature: None,
        };
        let err = verify_admission(&signed, &verifying, &net_grants()).unwrap_err();
        assert!(matches!(err, Error::Unsigned { .. }));
    }

    #[test]
    fn tampered_is_refused() {
        let (signing, verifying) = test_keypair();
        let mut signed = sign_manifest(&sample_manifest(), &signing);
        // Flip one byte in the manifest after signing.
        signed.manifest.version = String::from("9.9.9");
        let err = verify_admission(&signed, &verifying, &net_grants()).unwrap_err();
        assert!(matches!(err, Error::BadSignature { .. }));
    }

    #[test]
    fn wrong_key_is_refused() {
        let (signing, _) = test_keypair();
        // Different fixed key.
        let other_secret = [
            0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xab, 0xac, 0xad, 0xae,
            0xaf, 0xb0, 0xb1, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xbb, 0xbc,
            0xbd, 0xbe, 0xbf, 0xc0,
        ];
        let other_signing = SigningKey::from_bytes(&other_secret);
        let other_verifying = other_signing.verifying_key().to_bytes();
        let signed = sign_manifest(&sample_manifest(), &signing);
        let err = verify_admission(&signed, &other_verifying, &net_grants()).unwrap_err();
        assert!(matches!(err, Error::BadSignature { .. }));
    }

    #[test]
    fn signature_without_the_domain_tag_is_refused() {
        // A signature over the bare JSON (no v2 domain tag) must not
        // verify: the tag is part of what is signed.
        let (signing, verifying) = test_keypair();
        let manifest = sample_manifest();
        let bare = serde_json::to_vec(&manifest).unwrap();
        let signed = SignedManifest {
            manifest,
            signature: Some(BASE64.encode(signing.sign(&bare).to_bytes())),
        };
        let err = verify_admission(&signed, &verifying, &net_grants()).unwrap_err();
        assert!(matches!(err, Error::BadSignature { .. }));
    }

    #[test]
    fn trust_roots_are_checked_for_shape_and_strength() {
        let (_, verifying) = test_keypair();
        assert!(check_trust_root(&verifying).is_ok());
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(matches!(
            check_trust_root(&identity),
            Err(Error::InvalidTrustRoot(reason)) if reason.contains("weak")
        ));
    }

    #[test]
    fn weak_trust_root_is_refused() {
        // The identity point as the trust root, with R = identity and
        // S = 0: the verification equation holds for every message, so
        // only a strict verifier refuses it.
        let mut identity = [0u8; 32];
        identity[0] = 1;
        let mut sig = [0u8; 64];
        sig[0] = 1;
        let signed = SignedManifest {
            manifest: sample_manifest(),
            signature: Some(BASE64.encode(sig)),
        };
        let result = verify_admission(&signed, &identity, &net_grants());
        assert!(
            matches!(
                result,
                Err(Error::BadSignature { .. } | Error::InvalidTrustRoot(_))
            ),
            "a weak trust root must never admit: {result:?}"
        );
    }

    #[test]
    fn signed_but_invalid_manifest_is_refused() {
        // Signing does not make an unsafe entry point safe.
        let (signing, verifying) = test_keypair();
        let mut manifest = sample_manifest();
        manifest.program.as_mut().unwrap().path = String::from("../evil.py");
        let signed = sign_manifest(&manifest, &signing);
        let err = verify_admission(&signed, &verifying, &net_grants()).unwrap_err();
        assert!(matches!(err, Error::InvalidManifest(_)), "{err:?}");
    }

    #[test]
    fn over_privileged_is_refused_at_admission() {
        let (signing, verifying) = test_keypair();
        let mut manifest = sample_manifest();
        manifest.program.as_mut().unwrap().capabilities = vec![cap("net.egress"), cap("fs.write")];
        let signed = sign_manifest(&manifest, &signing);
        // User only granted net.egress, not fs.write.
        let err = verify_admission(&signed, &verifying, &net_grants()).unwrap_err();
        assert!(
            matches!(err, Error::OverPrivileged { .. }),
            "expected OverPrivileged, got {err:?}"
        );
        if let Error::OverPrivileged { name, excess } = err {
            assert_eq!(name, "test-skill");
            assert_eq!(excess, vec![cap("fs.write")]);
        }
    }

    #[test]
    fn undeclared_capability_denied_at_call_time() {
        let (signing, verifying) = test_keypair();
        let signed = sign_manifest(&sample_manifest(), &signing);
        let admission = verify_admission(&signed, &verifying, &net_grants()).unwrap();
        // Declared capability passes.
        assert!(admission.check_call(&cap("net.egress")).is_ok());
        // Undeclared capability is denied.
        let err = admission.check_call(&cap("fs.read")).unwrap_err();
        assert!(
            matches!(&err, Error::UndeclaredCapability { capability, .. } if *capability == cap("fs.read")),
            "{err:?}"
        );
    }
}
