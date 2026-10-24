//! Skill trust roots: the keys skill signatures must verify under, and how
//! they rotate without a broker restart.
//!
//! # The file
//!
//! `HATCHLING_SKILL_TRUST_ROOTS` names a file (absolute path) holding at
//! most two Ed25519 verifying keys:
//!
//! ```text
//! hatchling-trust-roots v1
//! primary <base64 key>
//! previous <base64 key>
//! end
//! ```
//!
//! The format is strict: the exact header first, then at most one
//! `primary` and one `previous` line (a `previous` needs a `primary`, and
//! they must differ), then `end`, then at most one final newline. Nothing
//! else parses. Every key must be a valid, non-weak Ed25519 point.
//!
//! - **The `end` line is the torn-write guard.** A file caught mid-write
//!   lacks it and is refused. That matters because a truncated file could
//!   otherwise still parse, with a root missing.
//! - **Zero roots** (the header, then `end`) is valid: it is the explicit
//!   "trust nothing", under which every skill is dropped.
//!
//! [`read_trust_roots_file`] reads at most [`MAX_TRUST_ROOTS_BYTES`], and
//! only from a regular file (never through a symlink) that is owned by the
//! broker's uid and not group- or world-writable. Anyone who could write
//! this file could make the broker trust their key.
//!
//! # Startup
//!
//! With skills enabled, the file must be present, readable, and valid, or
//! the broker refuses to start: a broker that cannot tell which keys it
//! trusts must not run skills. With skills disabled the file is not read.
//!
//! # Rotation: the reload state machine
//!
//! A broker thread polls the file every [`TRUST_ROOTS_POLL`]
//! ([`Skills::watch`](crate::Skills::watch)). Each poll ([`Skills::reload`](crate::Skills::reload)):
//!
//! 1. Reads the whole file and parses it, holding no lock.
//! 2. If the bytes (or the failure) are exactly what the last poll acted
//!    on: nothing, and no log. A persistently bad file logs once, not
//!    every poll.
//! 3. If the read or the parse fails (the file is missing, unreadable,
//!    malformed, torn, or has bad permissions): **the current set is
//!    kept**, nothing new is trusted, and one error is logged on entering
//!    that state. Deleting the file is not a revocation; it is
//!    indistinguishable from an accident. Revoking is writing a valid file.
//! 4. If it parses and differs: the complete new skill set is rebuilt from
//!    disk against the new roots, still holding no lock, and then swapped
//!    in with one pointer store. Skills verifiable only by a removed root
//!    are dropped, each logged. Skills newly verifiable are added.
//!
//! Guarantees:
//!
//! - **No window where an unknown key is trusted.** The set in force is
//!   always one built entirely from a file that parsed. A failed read or
//!   parse changes nothing.
//! - **Atomic.** A session takes one snapshot of the set per op (an
//!   `Arc`), so it sees the old set or the new one, never a mix. An
//!   in-flight run keeps its snapshot and finishes normally; only ops that
//!   start after the swap see the new set. The old set is freed when its
//!   last run ends.
//! - **Grace window.** During a rotation, `primary` is the new key and
//!   `previous` the old one, and skills signed by either admit. Dropping
//!   `previous`, including a compromised one, takes one valid write.
//!
//! # Updates versus drift
//!
//! A skill's identity is `name@version`. A real update ships as a new
//! version: a new id, admitted by the next reload under that skill's
//! grants, with no restart. The same id with different code bytes is not
//! an update but drift. A silent re-register is refused — the reload
//! rejects it loudly until a restart re-pins it.
//! Until then the per-run drift check refuses it at call time either way.

use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::Duration;

/// The first line of a v1 trust-roots file.
pub const TRUST_ROOTS_HEADER: &str = "hatchling-trust-roots v1";

/// Largest trust-roots file read, in bytes. Two keys fit in well under
/// 200; anything near this is not a trust-roots file.
pub const MAX_TRUST_ROOTS_BYTES: u64 = 4096;

/// How often the broker checks the trust-roots file for a rotation.
pub const TRUST_ROOTS_POLL: Duration = Duration::from_secs(2);

/// The keys skill signatures may verify under: at most a primary and,
/// during a rotation, the previous one. Built only by [`TrustRoots::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustRoots {
    primary: Option<[u8; 32]>,
    previous: Option<[u8; 32]>,
}

impl TrustRoots {
    /// Parses a v1 trust-roots file (see the module docs for the format).
    ///
    /// # Errors
    ///
    /// Returns a description of the first thing wrong with the file.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| String::from("not UTF-8"))?;
        let body = text.strip_suffix('\n').unwrap_or(text);
        let mut lines = body.split('\n');
        if lines.next() != Some(TRUST_ROOTS_HEADER) {
            return Err(format!("the first line must be {TRUST_ROOTS_HEADER:?}"));
        }
        let mut primary = None;
        let mut previous = None;
        let mut ended = false;
        for line in lines {
            if ended {
                return Err(String::from("content after the `end` line"));
            }
            if line == "end" {
                ended = true;
                continue;
            }
            let (role, key) = line
                .split_once(' ')
                .ok_or_else(|| format!("unrecognized line {line:?}"))?;
            let slot = match role {
                "primary" => &mut primary,
                "previous" => &mut previous,
                other => return Err(format!("unknown role {other:?}")),
            };
            if slot.replace(decode_key(role, key)?).is_some() {
                return Err(format!("{role} is given twice"));
            }
        }
        if !ended {
            return Err(String::from(
                "missing the `end` line: the file is incomplete (a torn write?)",
            ));
        }
        if primary.is_none() && previous.is_some() {
            return Err(String::from("a previous root needs a primary root"));
        }
        if primary.is_some() && primary == previous {
            return Err(String::from("primary and previous are the same key"));
        }
        Ok(Self { primary, previous })
    }

    /// The roots, primary first, each with its role name for logs.
    #[must_use]
    pub fn keys(&self) -> Vec<(&'static str, [u8; 32])> {
        [("primary", self.primary), ("previous", self.previous)]
            .into_iter()
            .filter_map(|(role, key)| key.map(|key| (role, key)))
            .collect()
    }

    /// Whether no root is trusted (the explicit revoke-all file).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.primary.is_none() && self.previous.is_none()
    }
}

/// Decodes one base64 root and checks it is a usable Ed25519 key.
fn decode_key(role: &str, raw: &str) -> Result<[u8; 32], String> {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, raw)
        .map_err(|_| format!("the {role} root is not base64"))?;
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("the {role} root must be 32 bytes, got {}", bytes.len()))?;
    hatchling_skill::check_trust_root(&key).map_err(|err| format!("the {role} root: {err}"))?;
    Ok(key)
}

/// Reads the whole trust-roots file, refusing anything that is not a
/// regular file owned by the broker's uid and closed to other writers.
///
/// The file is opened and then checked through the open handle, which
/// must be the same file the symlink-free lookup found, so a path swapped
/// between the two is refused rather than read.
///
/// # Errors
///
/// Returns a description of why the file cannot be trusted or read.
pub fn read_trust_roots_file(path: &Path) -> Result<Vec<u8>, String> {
    let shown = path.display();
    let link =
        std::fs::symlink_metadata(path).map_err(|err| format!("cannot stat {shown}: {err}"))?;
    if !link.file_type().is_file() {
        return Err(format!(
            "{shown} is not a regular file (symlinks are refused)"
        ));
    }
    let file = std::fs::File::open(path).map_err(|err| format!("cannot open {shown}: {err}"))?;
    let meta = file
        .metadata()
        .map_err(|err| format!("cannot stat {shown}: {err}"))?;
    if (meta.dev(), meta.ino()) != (link.dev(), link.ino()) {
        return Err(format!("{shown} changed while being opened"));
    }
    // Root's, or the broker's own: a broker that has dropped to its own
    // user (WO-017) reads root's and can't rewrite it, which is better.
    let euid = hatchling_sys::euid();
    if meta.uid() != 0 && meta.uid() != euid {
        return Err(format!(
            "{shown} must be owned by root or the broker's uid {euid}, not uid {}",
            meta.uid()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{shown} must not be group- or world-writable (mode {:o})",
            meta.mode() & 0o777
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_TRUST_ROOTS_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("cannot read {shown}: {err}"))?;
    if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_TRUST_ROOTS_BYTES) {
        return Err(format!(
            "{shown} is larger than {MAX_TRUST_ROOTS_BYTES} bytes"
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::PathBuf;

    use ed25519_dalek::SigningKey;

    use super::*;

    fn key(seed: u8) -> [u8; 32] {
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes()
    }

    fn b64(key: &[u8; 32]) -> String {
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, key)
    }

    fn file(lines: &[String]) -> Vec<u8> {
        let mut text = lines.join("\n");
        text.push('\n');
        text.into_bytes()
    }

    #[test]
    fn a_primary_and_a_previous_parse_in_order() {
        let roots = TrustRoots::parse(&file(&[
            TRUST_ROOTS_HEADER.to_owned(),
            format!("primary {}", b64(&key(1))),
            format!("previous {}", b64(&key(2))),
            String::from("end"),
        ]))
        .unwrap();
        assert_eq!(
            roots.keys(),
            vec![("primary", key(1)), ("previous", key(2))]
        );
        assert!(!roots.is_empty());
        // Order in the file does not matter; the primary is always first.
        let swapped = TrustRoots::parse(&file(&[
            TRUST_ROOTS_HEADER.to_owned(),
            format!("previous {}", b64(&key(2))),
            format!("primary {}", b64(&key(1))),
            String::from("end"),
        ]))
        .unwrap();
        assert_eq!(swapped, roots);
    }

    #[test]
    fn a_single_primary_and_the_empty_revoke_all_parse() {
        let one = TrustRoots::parse(&file(&[
            TRUST_ROOTS_HEADER.to_owned(),
            format!("primary {}", b64(&key(1))),
            String::from("end"),
        ]))
        .unwrap();
        assert_eq!(one.keys(), vec![("primary", key(1))]);
        let none = TrustRoots::parse(&file(&[TRUST_ROOTS_HEADER.to_owned(), String::from("end")]))
            .unwrap();
        assert!(none.is_empty());
        assert!(none.keys().is_empty());
        // Without the final newline too.
        let bare = format!("{TRUST_ROOTS_HEADER}\nend");
        assert!(TrustRoots::parse(bare.as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn every_malformed_file_is_refused() {
        let good = format!("primary {}", b64(&key(1)));
        let weak = {
            let mut identity = [0u8; 32];
            identity[0] = 1;
            format!("primary {}", b64(&identity))
        };
        let short = format!("primary {}", b64(&key(1)).get(..20).unwrap_or_default());
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("not utf-8", vec![0xff, 0xfe]),
            (
                "wrong header",
                file(&[
                    String::from("hatchling-trust-roots v2"),
                    String::from("end"),
                ]),
            ),
            (
                "torn: no end",
                file(&[TRUST_ROOTS_HEADER.to_owned(), good.clone()]),
            ),
            (
                "content after end",
                file(&[
                    TRUST_ROOTS_HEADER.to_owned(),
                    String::from("end"),
                    good.clone(),
                ]),
            ),
            (
                "blank line after end",
                format!("{TRUST_ROOTS_HEADER}\nend\n\n").into_bytes(),
            ),
            (
                "unknown role",
                file(&[
                    TRUST_ROOTS_HEADER.to_owned(),
                    format!("backup {}", b64(&key(1))),
                    String::from("end"),
                ]),
            ),
            (
                "line without a key",
                file(&[
                    TRUST_ROOTS_HEADER.to_owned(),
                    String::from("primary"),
                    String::from("end"),
                ]),
            ),
            (
                "primary twice",
                file(&[
                    TRUST_ROOTS_HEADER.to_owned(),
                    good.clone(),
                    format!("primary {}", b64(&key(2))),
                    String::from("end"),
                ]),
            ),
            (
                "previous alone",
                file(&[
                    TRUST_ROOTS_HEADER.to_owned(),
                    format!("previous {}", b64(&key(2))),
                    String::from("end"),
                ]),
            ),
            (
                "same key twice",
                file(&[
                    TRUST_ROOTS_HEADER.to_owned(),
                    good.clone(),
                    format!("previous {}", b64(&key(1))),
                    String::from("end"),
                ]),
            ),
            (
                "not base64",
                file(&[
                    TRUST_ROOTS_HEADER.to_owned(),
                    String::from("primary !!!"),
                    String::from("end"),
                ]),
            ),
            (
                "short key",
                file(&[TRUST_ROOTS_HEADER.to_owned(), short, String::from("end")]),
            ),
            (
                "weak key",
                file(&[TRUST_ROOTS_HEADER.to_owned(), weak, String::from("end")]),
            ),
            (
                "crlf",
                format!("{TRUST_ROOTS_HEADER}\r\n{good}\r\nend\r\n").into_bytes(),
            ),
        ];
        for (what, bytes) in cases {
            assert!(TrustRoots::parse(&bytes).is_err(), "{what} must be refused");
        }
    }

    fn roots_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-roots-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8], mode: u32) {
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_private_regular_file_is_read_whole() {
        let dir = roots_dir("read");
        let path = dir.join("roots");
        write(&path, b"contents", 0o644);
        assert_eq!(read_trust_roots_file(&path).unwrap(), b"contents");
        write(&path, b"contents", 0o600);
        assert_eq!(read_trust_roots_file(&path).unwrap(), b"contents");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The file may be root's, which a broker that isn't root reads and
    /// can't rewrite, or the broker's own; another user's is refused.
    #[test]
    fn the_file_is_roots_or_the_brokers() {
        use std::os::unix::fs::MetadataExt as _;
        let roots = Path::new("/etc/passwd");
        assert_eq!(std::fs::metadata(roots).unwrap().uid(), 0);
        let read = read_trust_roots_file(roots);
        assert!(
            read.as_ref().map_or_else(|err| !err.contains("must be owned"), |_| true),
            "root's is never refused for its owner: {read:?}"
        );
        if hatchling_sys::euid() == 0 {
            let dir = roots_dir("owner");
            let path = dir.join("roots");
            write(&path, b"x", 0o644);
            std::os::unix::fs::chown(&path, Some(4242), None).unwrap();
            let err = read_trust_roots_file(&path).unwrap_err();
            assert!(err.contains("must be owned by root or"), "{err}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn writable_by_others_missing_or_not_a_file_is_refused() {
        let dir = roots_dir("refuse");
        let path = dir.join("roots");
        for mode in [0o664, 0o646, 0o666] {
            write(&path, b"x", mode);
            let err = read_trust_roots_file(&path).unwrap_err();
            assert!(err.contains("writable"), "mode {mode:o}: {err}");
        }
        write(&path, b"x", 0o644);
        let link = dir.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(
            read_trust_roots_file(&link)
                .unwrap_err()
                .contains("not a regular file")
        );
        assert!(
            read_trust_roots_file(&dir)
                .unwrap_err()
                .contains("not a regular file")
        );
        assert!(
            read_trust_roots_file(&dir.join("missing"))
                .unwrap_err()
                .contains("cannot stat")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_size_bound_is_exact() {
        let dir = roots_dir("size");
        let path = dir.join("roots");
        let at = usize::try_from(MAX_TRUST_ROOTS_BYTES).unwrap();
        write(&path, &vec![b'x'; at], 0o600);
        assert_eq!(read_trust_roots_file(&path).unwrap().len(), at);
        write(&path, &vec![b'x'; at + 1], 0o600);
        assert!(
            read_trust_roots_file(&path)
                .unwrap_err()
                .contains("larger than")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
