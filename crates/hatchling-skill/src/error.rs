//! Typed errors for skill admission and verification.
//!
//! Every refusal is a typed error, never a silent pass. The broker
//! logs these loudly; the cell only ever sees the refusal, never the
//! reason's internals.

use hatchling_core::CapabilityName;
use thiserror::Error;

/// All ways a skill can be refused.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The manifest has no signature block. Unsigned skills do not run.
    #[error("skill '{name}' refused: manifest is not signed")]
    Unsigned {
        /// The skill name from the manifest.
        name: String,
    },

    /// The signature is present but does not verify. The skill was
    /// tampered with after signing, or signed by an untrusted key.
    #[error("skill '{name}' refused: signature verification failed")]
    BadSignature {
        /// The skill name from the manifest.
        name: String,
    },

    /// The manifest declares capabilities beyond the user's grants.
    /// Refused at admission — the skill may never claim these.
    #[error(
        "skill '{name}' refused: manifest declares capabilities beyond user grants: {}",
        list(.excess)
    )]
    OverPrivileged {
        /// The skill name from the manifest.
        name: String,
        /// The declared capabilities that exceed the user's grants.
        excess: Vec<CapabilityName>,
    },

    /// The skill tried to use a capability it did not declare.
    /// Denied at call time — declaration is not enough, every call
    /// re-checks the grant.
    #[error("skill '{name}' denied: capability '{capability}' was not declared in the manifest")]
    UndeclaredCapability {
        /// The skill name from the manifest.
        name: String,
        /// The capability the skill tried to use.
        capability: CapabilityName,
    },

    /// The manifest is malformed.
    #[error("skill manifest invalid: {0}")]
    InvalidManifest(String),

    /// A file of the skill isn't the file its signed manifest lists: a
    /// different size or SHA-256, or not there to read.
    #[error("skill '{name}' refused: file '{path}' isn't the file its signed manifest lists")]
    FileMismatch {
        /// The skill name from the manifest.
        name: String,
        /// The listed path.
        path: String,
    },

    /// The trust root (public key) is malformed or unknown.
    #[error("trust root invalid: {0}")]
    InvalidTrustRoot(String),

    /// The skill's code changed under its pinned identity.
    /// The code hash at load/call time doesn't match the hash pinned
    /// at admission. Refused — this is the drift rule.
    #[error("skill '{id}' refused: code drift detected (expected {expected}, found {found})")]
    IdentityDrift {
        /// The skill's registry id.
        id: String,
        /// The pinned code hash.
        expected: String,
        /// The hash of the presented code.
        found: String,
    },

    /// No skill with this id is registered.
    #[error("unknown skill: '{0}'")]
    UnknownSkill(String),

    /// A skill with this id is already registered. Re-registration is
    /// refused — explicit `unregister` followed by `register` is the
    /// only update path. Silent replacement would let a compromised or
    /// confused caller swap a skill's code without any signal.
    #[error("skill '{0}' is already registered: unregister first to replace it")]
    AlreadyRegistered(String),
}

/// Comma-separated names, for refusal messages.
fn list(names: &[CapabilityName]) -> String {
    names
        .iter()
        .map(CapabilityName::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_refusals_name_the_capabilities() {
        let cap = |raw: &str| raw.parse::<CapabilityName>().unwrap();
        let over = Error::OverPrivileged {
            name: String::from("notes"),
            excess: vec![cap("fs.write"), cap("net.egress")],
        };
        assert_eq!(
            over.to_string(),
            "skill 'notes' refused: manifest declares capabilities beyond user grants: \
             fs.write, net.egress"
        );
        let undeclared = Error::UndeclaredCapability {
            name: String::from("notes"),
            capability: cap("fs.read"),
        };
        assert_eq!(
            undeclared.to_string(),
            "skill 'notes' denied: capability 'fs.read' was not declared in the manifest"
        );
    }
}
