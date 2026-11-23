//! Making and signing skills, on the author's side (WO-046).
//!
//! `hatchling skill` runs these, in the author's own terminal. The broker
//! has none of this: it verifies, and never holds a signing key.
//!
//! - [`new_key`] makes an Ed25519 key in a file of its own, mode 0600,
//!   never over another, and gives back the public key for a trust-roots
//!   file and its fingerprint.
//! - [`sign`] reads the manifest the author wrote (name, version,
//!   description, playbook, program), lists every file in the skill's
//!   directory with its size and SHA-256, and writes the signed format 2
//!   manifest.
//! - [`check`] verifies a skill as the broker's loader will: the
//!   signature under a given public key, and every listed file.
//!
//! The private key is read from its file and nowhere else: never from an
//! argument or the environment, and never printed or logged.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signer as _, SigningKey};
use hatchling_core::CapabilityName;
use serde::Deserialize;
use sha2::Digest as _;
use zeroize::Zeroizing;

use crate::{
    Error, FORMAT, FileEntry, MANIFEST_FILE, Manifest, Program, SignedManifest, verify_admission,
};

/// The first line of a signing-key file.
pub const KEY_HEADER: &str = "hatchling-skill-signing-key-v1";

/// A new key's public half: what goes in a trust-roots file, and its
/// fingerprint as admission shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    /// The 32-byte public key, base64: a trust-roots file's line.
    pub base64: String,
    /// The SHA-256 of the public key, lowercase hex.
    pub fingerprint: String,
}

impl PublicKey {
    fn of(key: &SigningKey) -> Self {
        let public = key.verifying_key().to_bytes();
        Self {
            base64: BASE64.encode(public),
            fingerprint: hex(&sha2::Sha256::digest(public)),
        }
    }
}

/// What [`check`] found: the skill, who signed it, and any file in its
/// directory the manifest doesn't list (never read by the broker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The skill's `name@version`.
    pub id: String,
    /// The signer's fingerprint.
    pub signer: String,
    /// How many files the manifest lists.
    pub files: usize,
    /// Files present but not listed.
    pub unlisted: Vec<String>,
}

/// The manifest an author writes: what [`sign`] doesn't fill in. A
/// manifest signed before may be signed again, so its `format`, `files`
/// and `signature` are read and replaced.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    #[serde(default)]
    #[allow(dead_code, reason = "a signed manifest's format, replaced")]
    format: Option<u32>,
    name: String,
    version: String,
    description: String,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    #[allow(dead_code, reason = "a signed manifest's files, listed again")]
    files: Option<Vec<FileEntry>>,
    #[serde(default)]
    program: Option<Program>,
    #[serde(default)]
    #[allow(dead_code, reason = "a signed manifest's signature, made again")]
    signature: Option<String>,
}

/// Makes a signing key at `path`: 32 random bytes, written to a new file
/// readable by its owner alone, never over a file already there.
///
/// # Errors
/// Returns [`Error::Author`] if the randomness or the file can't be had.
pub fn new_key(path: &Path) -> Result<PublicKey, Error> {
    let mut secret = Zeroizing::new([0u8; 32]);
    getrandom::fill(secret.as_mut())
        .map_err(|err| Error::Author(format!("no randomness for a key: {err}")))?;
    let key = SigningKey::from_bytes(&secret);
    let text = Zeroizing::new(format!("{KEY_HEADER}\n{}\n", BASE64.encode(*secret)));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| Error::Author(format!("can't make {}: {err}", path.display())))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|err| Error::Author(format!("can't write {}: {err}", path.display())))?;
    Ok(PublicKey::of(&key))
}

/// Reads the signing key at `path`: a regular file, not a link, that only
/// its owner can read or write, holding [`KEY_HEADER`] and the key.
fn read_key(path: &Path) -> Result<SigningKey, Error> {
    let shown = path.display();
    let refuse = |why: String| Error::Author(format!("signing key {shown}: {why}"));
    let meta = std::fs::symlink_metadata(path).map_err(|err| refuse(err.to_string()))?;
    if !meta.file_type().is_file() {
        return Err(refuse(String::from(
            "not a regular file (links are refused)",
        )));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(refuse(format!(
            "others can read or write it (mode {:o}): it must be 600",
            meta.mode() & 0o777
        )));
    }
    let text =
        Zeroizing::new(std::fs::read_to_string(path).map_err(|err| refuse(err.to_string()))?);
    let mut lines = text.lines();
    if lines.next() != Some(KEY_HEADER) {
        return Err(refuse(format!("its first line isn't {KEY_HEADER}")));
    }
    let bytes = Zeroizing::new(
        BASE64
            .decode(lines.next().unwrap_or_default().trim())
            .map_err(|_| refuse(String::from("its key isn't base64")))?,
    );
    let secret: Zeroizing<[u8; 32]> = Zeroizing::new(
        bytes
            .as_slice()
            .try_into()
            .map_err(|_| refuse(String::from("its key isn't 32 bytes")))?,
    );
    if lines.next().is_some_and(|line| !line.is_empty()) {
        return Err(refuse(String::from("it holds more than a key")));
    }
    Ok(SigningKey::from_bytes(&secret))
}

/// Every file under `dir`, sorted by path, but `dir`'s own
/// [`MANIFEST_FILE`]: each a regular file, listed with its size and hash.
///
/// # Errors
/// Returns [`Error::Author`] for a link, a special file, a name that isn't
/// UTF-8, or one that can't be read.
pub fn list_files(dir: &Path) -> Result<Vec<FileEntry>, Error> {
    let mut found = Vec::new();
    walk(dir, "", &mut found)?;
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

fn walk(dir: &Path, prefix: &str, found: &mut Vec<FileEntry>) -> Result<(), Error> {
    let shown = dir.display();
    let entries = std::fs::read_dir(dir)
        .map_err(|err| Error::Author(format!("can't read {shown}: {err}")))?;
    for entry in entries {
        let entry = entry.map_err(|err| Error::Author(format!("can't read {shown}: {err}")))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| Error::Author(format!("{name:?} in {shown} isn't a UTF-8 name")))?;
        let path = format!("{prefix}{name}");
        if path == MANIFEST_FILE {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(|err| Error::Author(format!("can't read {path}: {err}")))?;
        if kind.is_dir() {
            walk(&entry.path(), &format!("{path}/"), found)?;
        } else if kind.is_file() {
            let bytes = std::fs::read(entry.path())
                .map_err(|err| Error::Author(format!("can't read {path}: {err}")))?;
            found.push(FileEntry::of(&path, &bytes));
        } else {
            return Err(Error::Author(format!(
                "{path} is a link or a special file: a skill holds regular files only"
            )));
        }
    }
    Ok(())
}

/// Signs the skill in `dir` with the key at `key`: the author's manifest,
/// every file listed, written back as the signed format 2 manifest.
/// Nothing is written unless the manifest validates.
///
/// # Errors
/// Returns why it can't be signed: the key, the manifest, or a file.
pub fn sign(dir: &Path, key: &Path) -> Result<SignedManifest, Error> {
    let key = read_key(key)?;
    let manifest_path = dir.join(MANIFEST_FILE);
    let draft_bytes = std::fs::read(&manifest_path)
        .map_err(|err| Error::Author(format!("can't read {}: {err}", manifest_path.display())))?;
    let draft: Draft = serde_json::from_slice(&draft_bytes)
        .map_err(|err| Error::InvalidManifest(format!("parse: {err}")))?;
    let manifest = Manifest {
        format: FORMAT,
        name: draft.name,
        version: draft.version,
        description: draft.description,
        instructions: draft.instructions,
        files: list_files(dir)?,
        program: draft.program,
    };
    manifest.validate()?;
    let signature = key.sign(&manifest.canonical_bytes()?);
    let signed = SignedManifest {
        manifest,
        signature: Some(BASE64.encode(signature.to_bytes())),
    };
    let json = serde_json::to_vec_pretty(&signed)
        .map_err(|err| Error::InvalidManifest(format!("serialize: {err}")))?;
    std::fs::write(&manifest_path, json)
        .map_err(|err| Error::Author(format!("can't write {}: {err}", manifest_path.display())))?;
    Ok(signed)
}

/// Checks the skill in `dir` as the broker's loader will.
///
/// Its signature under `root` (a public key, base64), and every listed
/// file against its entry. Its program's capabilities are taken as
/// granted: the grants are the owner's, at install.
///
/// # Errors
/// Returns why the broker would refuse it.
pub fn check(dir: &Path, root: &str) -> Result<Checked, Error> {
    let root: [u8; 32] = BASE64
        .decode(root.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| Error::InvalidTrustRoot(String::from("not a base64 32-byte key")))?;
    let manifest_path = dir.join(MANIFEST_FILE);
    let bytes = std::fs::read(&manifest_path)
        .map_err(|err| Error::Author(format!("can't read {}: {err}", manifest_path.display())))?;
    let signed = SignedManifest::from_json(&bytes)?;
    let declared = signed
        .manifest
        .capabilities()
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<CapabilityName>>();
    let admission = verify_admission(&signed, &root, &declared)?;
    let manifest = admission.manifest();
    for entry in &manifest.files {
        let path = dir.join(&entry.path);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| Error::FileMismatch {
            name: manifest.name.clone(),
            path: entry.path.clone(),
        })?;
        if !meta.file_type().is_file() {
            return Err(Error::FileMismatch {
                name: manifest.name.clone(),
                path: entry.path.clone(),
            });
        }
        let content = std::fs::read(&path).map_err(|_| Error::FileMismatch {
            name: manifest.name.clone(),
            path: entry.path.clone(),
        })?;
        entry.check(&manifest.name, &content)?;
    }
    let listed: BTreeSet<&str> = manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    let unlisted = list_files(dir)?
        .into_iter()
        .map(|entry| entry.path)
        .filter(|path| !listed.contains(path.as_str()))
        .collect();
    Ok(Checked {
        id: format!("{}@{}", manifest.name, manifest.version),
        signer: admission.signer_fingerprint(),
        files: manifest.files.len(),
        unlisted,
    })
}

/// Usage for `hatchling skill`.
pub const USAGE: &str = "\
Usage:
  hatchling skill key new <key-file>      make a signing key, readable by you alone
  hatchling skill sign <dir> --key <key-file>
                                          list every file in <dir> and sign its manifest
  hatchling skill check <dir> --root <public-key>
                                          verify <dir> as Hatchling will load it

A skill's directory holds manifest.json and its files. Write the manifest's
name, version, description, and its instructions (a Markdown playbook) or
program ({\"path\": ..., \"capabilities\": [...]}) or both; `sign` fills in
the rest. The key is read from its file only, never from an argument.";

/// `hatchling skill <args>`: writes what it did to `out`, a refusal to
/// `err`, and returns the exit status (0, 1 refused, 64 usage).
pub fn cli(args: &[String], out: &mut dyn std::io::Write, err: &mut dyn std::io::Write) -> u8 {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match words.as_slice() {
        ["key", "new", file] => new_key(Path::new(file)).map(|public| {
            format!(
                "Made a signing key in {file}. Keep it to yourself: it's readable by you alone.\n\
                 Its public key, for a trust-roots file: {}\n\
                 Its fingerprint, as Hatchling shows it: {}\n",
                public.base64, public.fingerprint
            )
        }),
        ["sign", dir, "--key", file] => sign(Path::new(dir), &PathBuf::from(file)).map(|signed| {
            format!(
                "Signed {}@{}: {} files listed, in {}/{MANIFEST_FILE}.\n",
                signed.manifest.name,
                signed.manifest.version,
                signed.manifest.files.len(),
                dir
            )
        }),
        ["check", dir, "--root", root] => check(Path::new(dir), root).map(|checked| {
            let mut said = format!(
                "{} verifies: signed by {}, {} files, each as listed.\n",
                checked.id, checked.signer, checked.files
            );
            for path in &checked.unlisted {
                let _ = writeln!(said, "Not listed, so never read: {path}");
            }
            said
        }),
        ["--help" | "-h" | "help"] => {
            let _ = writeln!(out, "{USAGE}");
            return 0;
        }
        _ => {
            let _ = writeln!(err, "{USAGE}");
            return 64;
        }
    };
    match result {
        Ok(said) => {
            let _ = write!(out, "{said}");
            0
        }
        Err(refusal) => {
            let _ = writeln!(err, "hatchling skill: {refusal}");
            1
        }
    }
}

/// Lowercase hex of `bytes`.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests;
