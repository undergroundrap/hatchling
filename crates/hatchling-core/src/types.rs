//! Newtypes for identifiers and security-relevant values.
//!
//! Invariant: no bare `u32` or `String` crosses a module boundary. Every
//! identifier is built through a validating parser, so values inside the
//! core are valid by construction.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Error;

/// Maximum length of a [`CellId`], in bytes.
const CELL_ID_MAX_LEN: usize = 64;

/// Maximum length of a [`CapabilityName`], in bytes.
const CAPABILITY_NAME_MAX_LEN: usize = 128;

/// Maximum length of a [`SecretName`], in bytes.
const SECRET_NAME_MAX_LEN: usize = 128;

/// Unique identifier for an isolated cell.
///
/// A valid id is 1–64 ASCII alphanumeric or `-` characters. Parsing rejects
/// everything else, so a `CellId` is always safe to use in paths and logs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CellId(String);

impl CellId {
    /// Returns the identifier as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CellId {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.is_empty() {
            return Err(Error::InvalidCellId(String::from("cell id is empty")));
        }
        if raw.len() > CELL_ID_MAX_LEN {
            return Err(Error::InvalidCellId(format!(
                "cell id exceeds {CELL_ID_MAX_LEN} bytes"
            )));
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-'))
        {
            return Err(Error::InvalidCellId(format!(
                "cell id contains invalid character {bad:?}"
            )));
        }
        Ok(Self(raw.to_owned()))
    }
}

impl fmt::Display for CellId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Kernel-reported UID of a peer process.
///
/// Values of this type must only be constructed from kernel-reported peer
/// credentials (`SO_PEERCRED`), never from anything a caller sends about
/// itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerUid(u32);

impl PeerUid {
    /// Wraps a kernel-reported uid.
    #[must_use]
    pub const fn from_kernel(uid: u32) -> Self {
        Self(uid)
    }

    /// Returns the raw uid.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for PeerUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Kernel-reported PID of a peer process.
///
/// Same construction rule as [`PeerUid`]: kernel-reported only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerPid(u32);

impl PeerPid {
    /// Wraps a kernel-reported pid.
    #[must_use]
    pub const fn from_kernel(pid: u32) -> Self {
        Self(pid)
    }

    /// Returns the raw pid.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for PeerPid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Absolute path of a Unix socket the agent may connect to.
///
/// Parsing requires an absolute path. Relative paths are rejected so a
/// compromised working directory cannot redirect the agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SocketPath(PathBuf);

impl SocketPath {
    /// Returns the path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for SocketPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(f)
    }
}

impl FromStr for SocketPath {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let path = PathBuf::from(raw);
        // Absolute, and with no `..`: a socket path is checked as the
        // path it is, never as one that climbs elsewhere (#691).
        let climbs = path
            .components()
            .any(|part| part == std::path::Component::ParentDir);
        if path.is_absolute() && !climbs {
            Ok(Self(path))
        } else {
            Err(Error::InvalidSocketPath(raw.to_owned()))
        }
    }
}

/// Name of a capability an agent may request from a broker.
///
/// Valid names are 1–128 ASCII alphanumeric, `-`, `_` or `.` characters.
///
/// Serializes as the bare string and deserializes through [`FromStr`]:
/// a malformed name never decodes, so a frame or manifest carrying one
/// is refused where it enters, never carried inward as a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "String", into = "String")]
pub struct CapabilityName(String);

impl CapabilityName {
    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CapabilityName {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.is_empty() {
            return Err(Error::InvalidCapabilityName(String::from(
                "capability name is empty",
            )));
        }
        if raw.len() > CAPABILITY_NAME_MAX_LEN {
            return Err(Error::InvalidCapabilityName(format!(
                "capability name exceeds {CAPABILITY_NAME_MAX_LEN} bytes"
            )));
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_' || *c == '.'))
        {
            return Err(Error::InvalidCapabilityName(format!(
                "capability name contains invalid character {bad:?}"
            )));
        }
        Ok(Self(raw.to_owned()))
    }
}

impl TryFrom<String> for CapabilityName {
    type Error = Error;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        raw.parse()
    }
}

impl From<CapabilityName> for String {
    fn from(name: CapabilityName) -> Self {
        name.0
    }
}

impl fmt::Display for CapabilityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Name of a secret in the vault.
///
/// A valid name is 1–128 bytes of ASCII alphanumeric, `-`, or `_`. The
/// alphabet contains no path separator, so traversal (`../`), absolute
/// paths, and NUL bytes are unrepresentable: they are rejected at parse,
/// before any filesystem touch. Both ends of the protocol validate the
/// same rule — the agent before sending, the broker before acting — so a
/// hand-rolled client cannot smuggle a name past either.
///
/// Names are not secret: `Display` is allowed (unlike secret values).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretName(String);

impl SecretName {
    /// The validated name, for joining onto the vault root or logging.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SecretName {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        // Length first: the alphabet check below is byte-wise, so check
        // the byte length explicitly rather than chars.
        if raw.is_empty() || raw.len() > SECRET_NAME_MAX_LEN {
            return Err(Error::InvalidSecretName(format!(
                "name must be 1-{SECRET_NAME_MAX_LEN} bytes, got {}",
                raw.len()
            )));
        }
        let alphabet_ok = raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        if !alphabet_ok {
            return Err(Error::InvalidSecretName(format!(
                "name {raw:?} contains characters outside [A-Za-z0-9_-]"
            )));
        }
        Ok(Self(raw.to_owned()))
    }
}

impl fmt::Display for SecretName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_id_accepts_valid_ids() {
        assert_eq!("abc-123".parse::<CellId>().unwrap().as_str(), "abc-123");
    }

    #[test]
    fn cell_id_accepts_max_length() {
        // Boundary: exactly CELL_ID_MAX_LEN must be accepted. Kills the
        // `>` -> `>=` mutant on the length check.
        let max = "a".repeat(CELL_ID_MAX_LEN);
        assert_eq!(max.parse::<CellId>().unwrap().as_str(), max);
    }

    #[test]
    fn cell_id_rejects_empty() {
        assert!("".parse::<CellId>().is_err());
    }

    #[test]
    fn cell_id_rejects_too_long() {
        let long = "a".repeat(CELL_ID_MAX_LEN + 1);
        assert!(long.parse::<CellId>().is_err());
    }

    #[test]
    fn cell_id_rejects_bad_characters() {
        assert!("has space".parse::<CellId>().is_err());
        assert!("has/slash".parse::<CellId>().is_err());
        assert!("has_underscore".parse::<CellId>().is_err());
    }

    #[test]
    fn socket_path_accepts_absolute() {
        let path = "/run/hatchling/broker.sock".parse::<SocketPath>().unwrap();
        assert_eq!(path.as_path(), Path::new("/run/hatchling/broker.sock"));
    }

    #[test]
    fn socket_path_rejects_relative() {
        assert!("relative/path.sock".parse::<SocketPath>().is_err());
        // #691: nothing that climbs out of where it says it is.
        assert!("/var/lib/hatchling/x/../sock/ui.sock".parse::<SocketPath>().is_err());
        assert!("/run/../ui.sock".parse::<SocketPath>().is_err());
        assert!("/run/a..b/ui.sock".parse::<SocketPath>().is_ok());
    }

    #[test]
    fn capability_name_accepts_valid_names() {
        assert_eq!(
            "fs.read".parse::<CapabilityName>().unwrap().as_str(),
            "fs.read"
        );
    }

    #[test]
    fn capability_name_accepts_max_length() {
        // Boundary: exactly CAPABILITY_NAME_MAX_LEN must be accepted.
        // Kills the `>` -> `==` and `>` -> `>=` mutants on the length check.
        let max = "a".repeat(CAPABILITY_NAME_MAX_LEN);
        assert_eq!(max.parse::<CapabilityName>().unwrap().as_str(), max);
    }

    #[test]
    fn capability_name_rejects_too_long() {
        let long = "a".repeat(CAPABILITY_NAME_MAX_LEN + 1);
        assert!(long.parse::<CapabilityName>().is_err());
    }

    #[test]
    fn capability_name_accepts_all_separators() {
        // Kills the `||` -> `&&` mutant on the character class: each
        // separator must be accepted, not just alphanumerics and dots.
        assert!("with-dash".parse::<CapabilityName>().is_ok());
        assert!("with_underscore".parse::<CapabilityName>().is_ok());
        assert!("with.dot".parse::<CapabilityName>().is_ok());
    }

    #[test]
    fn capability_name_rejects_bad_characters() {
        assert!("has space".parse::<CapabilityName>().is_err());
        assert!("".parse::<CapabilityName>().is_err());
    }

    #[test]
    fn capability_name_serde_is_the_bare_string_and_validates() {
        let name: CapabilityName = "net.egress".parse().unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, "\"net.egress\"");
        assert_eq!(serde_json::from_str::<CapabilityName>(&json).unwrap(), name);
        assert_eq!(String::from(name), "net.egress");
        // Decoding runs the parser: every malformed shape is refused.
        let overlong = format!("\"{}\"", "a".repeat(CAPABILITY_NAME_MAX_LEN + 1));
        for bad in ["\"\"", "\"has space\"", "\"semi;colon\"", overlong.as_str()] {
            assert!(
                serde_json::from_str::<CapabilityName>(bad).is_err(),
                "{bad} must not decode"
            );
        }
    }

    #[test]
    fn peer_credentials_carry_their_values() {
        assert_eq!(PeerUid::from_kernel(1000).as_u32(), 1000);
        assert_eq!(PeerPid::from_kernel(42).as_u32(), 42);
    }

    #[test]
    fn display_shows_inner_values() {
        // Pins the Display impls: they surface in logs, so a wrong
        // rendering would mislead. Kills the `-> Ok(Default::default())`
        // mutants on each fmt.
        assert_eq!("cell-01".parse::<CellId>().unwrap().to_string(), "cell-01");
        assert_eq!(PeerUid::from_kernel(1000).to_string(), "1000");
        assert_eq!(PeerPid::from_kernel(42).to_string(), "42");
        assert_eq!(
            "/run/x.sock".parse::<SocketPath>().unwrap().to_string(),
            "/run/x.sock"
        );
        assert_eq!(
            "fs.read".parse::<CapabilityName>().unwrap().to_string(),
            "fs.read"
        );
        assert_eq!(
            "app-token".parse::<SecretName>().unwrap().to_string(),
            "app-token"
        );
    }

    #[test]
    fn secret_name_accepts_the_full_alphabet() {
        let max_len = "x".repeat(SECRET_NAME_MAX_LEN);
        for name in ["a", "app-token", "API_KEY_2", max_len.as_str(), "0-_-0"] {
            assert!(
                name.parse::<SecretName>().is_ok(),
                "name {name:?} should be accepted"
            );
        }
    }

    #[test]
    fn secret_name_as_str_returns_the_validated_name() {
        // `as_str` feeds the vault file path and the broker logs: a
        // mutant returning a constant must fail here.
        for name in ["a", "app-token", "API_KEY_2", "0-_-0"] {
            assert_eq!(name.parse::<SecretName>().unwrap().as_str(), name);
        }
    }

    #[test]
    fn secret_name_rejects_traversal_and_separators() {
        // Each of these would escape the vault root or name a hidden
        // file if it reached the filesystem. Deleting the alphabet
        // check must fail this test.
        for name in [
            "../escape",
            "..",
            "/absolute",
            "sub/dir",
            ".hidden",
            "trailing.",
            "semi;colon",
            "space name",
            "null\0byte",
            "ünïcode",
        ] {
            assert!(
                name.parse::<SecretName>().is_err(),
                "name {name:?} should be rejected"
            );
        }
    }

    #[test]
    fn secret_name_rejects_empty_and_overlong() {
        // The 128/129 boundary pins both comparisons: `is_empty` and
        // `len() > SECRET_NAME_MAX_LEN`. A `>=` mutant rejects the
        // 128-byte name; dropping the length check accepts the 129-byte
        // one.
        assert!("".parse::<SecretName>().is_err());
        assert!(
            "x".repeat(SECRET_NAME_MAX_LEN)
                .parse::<SecretName>()
                .is_ok()
        );
        assert!(
            "x".repeat(SECRET_NAME_MAX_LEN + 1)
                .parse::<SecretName>()
                .is_err()
        );
    }
}
