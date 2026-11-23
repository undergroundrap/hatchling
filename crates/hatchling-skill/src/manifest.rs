//! Skill manifest format, v2 (WO-046).
//!
//! A skill is a directory: `manifest.json` and the files it lists. The
//! manifest names the skill, says when it applies (`description`), points
//! at its playbook (`instructions`, Markdown) and its program (`program`,
//! run in a cell of its own), and lists **every file** of the skill with
//! its size and SHA-256. At least one of `instructions` and `program` is
//! there: a playbook, a tool, or both.
//!
//! # The signed payload (format v2)
//!
//! The signature covers [`Manifest::canonical_bytes`]: the ASCII domain
//! tag [`SIGNING_CONTEXT`] (`hatchling-skill-manifest-v2` and a newline),
//! then the manifest as compact JSON with the fields in declaration
//! order (`format`, `name`, `version`, `description`, `instructions`,
//! `files`, `program`; *not* sorted), no whitespace, and strings escaped
//! as `serde_json` escapes them. The signature field itself is never part
//! of the payload.
//!
//! Because every file is listed with its hash, the signature covers every
//! byte the skill can put in front of the agent or run (#613): the loader
//! checks each file against its entry on every load, and serves nothing
//! that isn't listed. The tag binds a signature to this object type and
//! format: a v1 signature can't be read as a v2 one, nor the reverse.
//! `canonical_bytes_are_pinned` pins the exact bytes, because changing
//! them invalidates every signature ever made.
//!
//! # Format v1 is retired
//!
//! v1 signed four fields and not the program they named, which was bound
//! only by a hash pinned on first sight (#613). No v1 package exists
//! outside the tests, so a v1 manifest is refused with a message to sign
//! it again as v2, never loaded.
//!
//! # Strict parsing
//!
//! A signed manifest is parsed strictly: unknown and duplicate fields
//! are refused, so unsigned data can't ride along beside a valid
//! signature.

use std::collections::HashSet;
use std::path::{Component, Path};

use hatchling_core::CapabilityName;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::Error;

/// Domain tag that starts every signed manifest payload (format v2).
pub const SIGNING_CONTEXT: &[u8] = b"hatchling-skill-manifest-v2\n";

/// The one manifest format the loader admits.
pub const FORMAT: u32 = 2;

/// The manifest's own file name: never listed among its files.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The longest description, in bytes: what discovery shows of a skill.
pub const MAX_DESCRIPTION_BYTES: usize = 512;

/// The largest playbook, in bytes.
pub const MAX_INSTRUCTIONS_BYTES: u64 = 32 * 1024;

/// The most files a skill lists.
pub const MAX_FILES: usize = 64;

/// The most bytes a skill's files other than its program hold, together:
/// the playbook and what it refers to, which reach the agent's context.
pub const MAX_SKILL_BYTES: u64 = 1024 * 1024;

/// The largest program, in bytes: a compiled program is often several
/// megabytes, and it runs in its own cell rather than being read.
pub const MAX_PROGRAM_BYTES: u64 = 64 * 1024 * 1024;

/// One file of a skill: where it is, how big, and its SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// A plain relative path inside the skill's directory.
    pub path: String,
    /// The file's size in bytes.
    pub bytes: u64,
    /// The file's SHA-256, 64 lowercase hex digits.
    pub sha256: String,
}

impl FileEntry {
    /// The entry for `content` at `path`: its size and hash.
    #[must_use]
    pub fn of(path: &str, content: &[u8]) -> Self {
        Self {
            path: path.to_owned(),
            bytes: u64::try_from(content.len()).unwrap_or(u64::MAX),
            sha256: hex(&sha2::Sha256::digest(content)),
        }
    }

    /// Checks that `content` is this file: the same size and SHA-256.
    ///
    /// # Errors
    /// Returns [`Error::FileMismatch`] naming the skill and the file.
    pub fn check(&self, skill: &str, content: &[u8]) -> Result<(), Error> {
        let found = Self::of(&self.path, content);
        if found.bytes == self.bytes && found.sha256 == self.sha256 {
            Ok(())
        } else {
            Err(Error::FileMismatch {
                name: skill.to_owned(),
                path: self.path.clone(),
            })
        }
    }
}

/// A skill's program: the listed file run in its own cell, and the
/// capabilities it declares.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Program {
    /// The listed file that is run.
    pub path: String,
    /// Declared capabilities: a subset of the broker's capability
    /// vocabulary (e.g. `net.egress`, `artifact.build`).
    pub capabilities: Vec<CapabilityName>,
}

/// A skill manifest without the signature: the signed payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The format: always [`FORMAT`].
    pub format: u32,
    /// Unique skill name (e.g. "write-a-report").
    pub name: String,
    /// The signer's version (e.g. "1.0.0").
    pub version: String,
    /// When the skill applies, and when it doesn't: what discovery shows.
    pub description: String,
    /// The listed Markdown file holding the playbook, if there is one.
    pub instructions: Option<String>,
    /// Every file of the skill.
    pub files: Vec<FileEntry>,
    /// The program, if there is one.
    pub program: Option<Program>,
}

impl Manifest {
    /// The signed payload: [`SIGNING_CONTEXT`], then the compact JSON
    /// described in the module docs.
    ///
    /// # Errors
    /// Returns [`Error::InvalidManifest`] if JSON serialization fails.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, Error> {
        let json = serde_json::to_vec(self)
            .map_err(|e| Error::InvalidManifest(format!("canonical encoding: {e}")))?;
        let mut bytes = Vec::with_capacity(SIGNING_CONTEXT.len() + json.len());
        bytes.extend_from_slice(SIGNING_CONTEXT);
        bytes.extend_from_slice(&json);
        Ok(bytes)
    }

    /// A skill that is one program: `code` listed at `path`, run with
    /// `capabilities`, and no playbook. (The builders are this shape.)
    #[must_use]
    pub fn for_program(
        name: &str,
        version: &str,
        description: &str,
        path: &str,
        code: &[u8],
        capabilities: Vec<CapabilityName>,
    ) -> Self {
        Self {
            format: FORMAT,
            name: name.to_owned(),
            version: version.to_owned(),
            description: description.to_owned(),
            instructions: None,
            files: vec![FileEntry::of(path, code)],
            program: Some(Program {
                path: path.to_owned(),
                capabilities,
            }),
        }
    }

    /// The skill's digest: the SHA-256 of its signed payload, lowercase
    /// hex. The payload lists every file's hash, so two skills with the
    /// same digest are the same bytes. The owner enables a skill by it.
    ///
    /// # Errors
    /// Returns [`Error::InvalidManifest`] if the payload can't be encoded.
    pub fn digest(&self) -> Result<String, Error> {
        Ok(hex(&sha2::Sha256::digest(self.canonical_bytes()?)))
    }

    /// The capabilities the skill's program declares; none without one.
    #[must_use]
    pub fn capabilities(&self) -> &[CapabilityName] {
        self.program
            .as_ref()
            .map_or(&[], |program| program.capabilities.as_slice())
    }

    /// The listed file the program runs, if there is a program.
    #[must_use]
    pub fn entry_point(&self) -> Option<&str> {
        self.program.as_ref().map(|program| program.path.as_str())
    }

    /// The entry for `path`, when the manifest lists it.
    #[must_use]
    pub fn file(&self, path: &str) -> Option<&FileEntry> {
        self.files.iter().find(|entry| entry.path == path)
    }

    /// Checks everything a loader acts on: the format; a name and a
    /// version; a description within its bound and on one line; a
    /// playbook or a program; every file a plain relative path, listed
    /// once, with a well-formed hash, the manifest not among them, within
    /// the count and size bounds (the program within its own); and the
    /// playbook and the program each a listed file, the playbook within
    /// its own bound.
    ///
    /// # Errors
    /// Returns [`Error::InvalidManifest`] naming the offending field.
    pub fn validate(&self) -> Result<(), Error> {
        let invalid = |reason: String| Err(Error::InvalidManifest(reason));
        if self.format != FORMAT {
            return invalid(format!("format {} isn't format {FORMAT}", self.format));
        }
        if self.name.is_empty() {
            return invalid(String::from("name is empty"));
        }
        // The `@` is the SkillId separator (`name@version`): `a@b@c`
        // would be ambiguous, so it's refused rather than guessed at.
        if self.name.contains('@') {
            return invalid(format!(
                "name {:?} must not contain '@' (SkillId separator)",
                self.name
            ));
        }
        if self.version.is_empty() {
            return invalid(String::from("version is empty"));
        }
        if self.description.trim().is_empty() {
            return invalid(String::from("description is empty"));
        }
        if self.description.len() > MAX_DESCRIPTION_BYTES {
            return invalid(format!(
                "description is {} bytes, more than {MAX_DESCRIPTION_BYTES}",
                self.description.len()
            ));
        }
        if self.description.chars().any(char::is_control) {
            return invalid(String::from(
                "description holds a control character: it's one line of text",
            ));
        }
        if self.instructions.is_none() && self.program.is_none() {
            return invalid(String::from(
                "neither instructions nor a program: a skill has at least one",
            ));
        }
        self.validate_files()?;
        if let Some(instructions) = &self.instructions {
            let Some(entry) = self.file(instructions) else {
                return invalid(format!("instructions {instructions:?} isn't a listed file"));
            };
            if entry.bytes > MAX_INSTRUCTIONS_BYTES {
                return invalid(format!(
                    "instructions are {} bytes, more than {MAX_INSTRUCTIONS_BYTES}",
                    entry.bytes
                ));
            }
        }
        if let Some(program) = &self.program {
            if self.file(&program.path).is_none() {
                return invalid(format!("program {:?} isn't a listed file", program.path));
            }
        }
        Ok(())
    }

    /// The file list's own rules (see [`Manifest::validate`]).
    fn validate_files(&self) -> Result<(), Error> {
        let invalid = |reason: String| Err(Error::InvalidManifest(reason));
        if self.files.is_empty() {
            return invalid(String::from("no files are listed"));
        }
        if self.files.len() > MAX_FILES {
            return invalid(format!(
                "{} files are listed, more than {MAX_FILES}",
                self.files.len()
            ));
        }
        let program = self.entry_point();
        let mut seen = HashSet::new();
        let mut total: u64 = 0;
        for entry in &self.files {
            if !plain_path(&entry.path) {
                return invalid(format!(
                    "file {:?} must be a relative path inside the skill package",
                    entry.path
                ));
            }
            if entry.path == MANIFEST_FILE {
                return invalid(format!("{MANIFEST_FILE} can't list itself"));
            }
            if !seen.insert(entry.path.as_str()) {
                return invalid(format!("file {:?} is listed twice", entry.path));
            }
            if entry.sha256.len() != 64
                || !entry
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return invalid(format!(
                    "file {:?}'s sha256 isn't 64 lowercase hex digits",
                    entry.path
                ));
            }
            if program == Some(entry.path.as_str()) {
                if entry.bytes > MAX_PROGRAM_BYTES {
                    return invalid(format!(
                        "program {:?} is {} bytes, more than {MAX_PROGRAM_BYTES}",
                        entry.path, entry.bytes
                    ));
                }
            } else {
                total = total.saturating_add(entry.bytes);
            }
        }
        if total > MAX_SKILL_BYTES {
            return invalid(format!(
                "the files other than the program hold {total} bytes, more than {MAX_SKILL_BYTES}"
            ));
        }
        Ok(())
    }
}

/// A non-empty relative path made only of plain components (no `..`, no
/// root, no leading `.`, no NUL), so it can never name a file outside the
/// skill package.
fn plain_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// A manifest with its Ed25519 signature.
///
/// Serializes flat (the manifest fields beside `signature`) and parses
/// through [`WireSignedManifest`], which refuses unknown and duplicate
/// fields (see "Strict parsing" in the module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "WireSignedManifest")]
pub struct SignedManifest {
    /// The signed manifest payload.
    #[serde(flatten)]
    pub manifest: Manifest,
    /// Base64-encoded Ed25519 signature over `manifest.canonical_bytes()`.
    /// `None` means the skill is unsigned — refused at admission.
    pub signature: Option<String>,
}

/// The document shape of a [`SignedManifest`], parsed strictly.
/// `flatten` cannot be combined with `deny_unknown_fields`, so parsing
/// goes through this flat struct instead.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSignedManifest {
    format: u32,
    name: String,
    version: String,
    description: String,
    instructions: Option<String>,
    files: Vec<FileEntry>,
    program: Option<Program>,
    signature: Option<String>,
}

impl From<WireSignedManifest> for SignedManifest {
    fn from(wire: WireSignedManifest) -> Self {
        Self {
            manifest: Manifest {
                format: wire.format,
                name: wire.name,
                version: wire.version,
                description: wire.description,
                instructions: wire.instructions,
                files: wire.files,
                program: wire.program,
            },
            signature: wire.signature,
        }
    }
}

impl SignedManifest {
    /// Parse from JSON, strictly (unknown or duplicate fields refused).
    /// A v1 manifest is refused with a message to sign it again as v2.
    ///
    /// # Errors
    /// Returns [`Error::InvalidManifest`] if the JSON is malformed, or a
    /// retired v1 manifest.
    pub fn from_json(json: &[u8]) -> Result<Self, Error> {
        if let Ok(serde_json::Value::Object(fields)) = serde_json::from_slice(json) {
            if !fields.contains_key("format") && fields.contains_key("entry_point") {
                return Err(Error::InvalidManifest(String::from(
                    "a format v1 manifest: v1 is retired, since its signature didn't cover \
                     its program; sign the skill again as format 2",
                )));
            }
        }
        serde_json::from_slice(json).map_err(|e| Error::InvalidManifest(format!("parse: {e}")))
    }

    /// Serialize to JSON.
    ///
    /// # Errors
    /// Returns [`Error::InvalidManifest`] if JSON serialization fails.
    pub fn to_json(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self).map_err(|e| Error::InvalidManifest(format!("serialize: {e}")))
    }
}

#[cfg(test)]
mod tests;
