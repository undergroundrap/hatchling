//! The browser's bundle, pinned whole and run from a verified snapshot
//! (WO-029).
//!
//! `HATCHLING_BROWSER` names the browser, and its directory is its bundle:
//! the executable, its libraries, its `.pak` files, its ICU data.
//! `HATCHLING_BROWSER_SHA256` pins all of it: the SHA-256 of the bundle's
//! [`Manifest`], one line per directory and per file, each file by
//! whether it's executable, its size and its SHA-256.
//!
//! - **A snapshot, checked as it's made.** The broker copies the bundle
//!   through handles that follow no symlink ([`hatchling_sys::copy_tree`]),
//!   hashing each file's bytes as they're written, and keeps the copy only
//!   if its manifest is the pinned one. A file changed after the check
//!   never reaches a browser, because the browser runs the copy; one that
//!   grows while it's copied stops at the bundle's bound.
//! - **Made once.** The first browser makes it, and the broker keeps it,
//!   read-only, for its life; every browser cell binds it, never the
//!   operator's directory. It's a staging directory: a broker that's gone
//!   leaves it for the next one's sweep.
//! - **What it pins.** A verified bundle on a trusted system runtime: the
//!   loader and the system libraries the browser links are the host's.
//!
//! `hatchling-broker browser-digest <browser>` prints a bundle's digest,
//! for the setting.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hatchling_sys::{TreeEntry, TreeLimits};
use sha2::{Digest as _, Sha256};

/// How much of a bundle is copied: a headless Chromium's is about 300
/// entries and 250 MB.
pub const BUNDLE_LIMITS: TreeLimits = TreeLimits {
    entries: 4096,
    bytes: 1024 * 1024 * 1024,
    depth: 8,
};

/// A bundle's manifest: a line per entry, in the order of their paths.
///
/// - a directory: `d <path>`
/// - a file: `f <x|-> <size> <sha256> <path>`, `x` if it's executable
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Manifest {
    lines: BTreeMap<String, String>,
}

impl Manifest {
    /// The manifest, as its digest hashes it.
    #[must_use]
    pub fn text(&self) -> String {
        self.lines.values().map(String::as_str).collect()
    }

    /// Its digest: the SHA-256 of its text, as 64 lowercase hex digits.
    #[must_use]
    pub fn digest(&self) -> String {
        hex(&Sha256::digest(self.text().as_bytes()))
    }

    /// The manifest of the tree at `source`, copied into `target` when one
    /// is given: each file as the bytes written are.
    ///
    /// # Errors
    ///
    /// A sentence for the operator: the tree can't be walked or copied,
    /// or it holds something a bundle can't (see
    /// [`hatchling_sys::copy_tree`]).
    pub fn of(source: &Path, target: Option<&Path>) -> Result<Self, String> {
        let mut lines = BTreeMap::new();
        let mut file: Option<(String, bool, Sha256)> = None;
        hatchling_sys::copy_tree(source, target, BUNDLE_LIMITS, &mut |entry| match entry {
            TreeEntry::Dir { path } => {
                lines.insert(path.to_owned(), format!("d {path}\n"));
            }
            TreeEntry::File { path, executable } => {
                file = Some((path.to_owned(), executable, Sha256::new()));
            }
            TreeEntry::Bytes(bytes) => {
                if let Some((_, _, hasher)) = file.as_mut() {
                    hasher.update(bytes);
                }
            }
            TreeEntry::End { size } => {
                if let Some((path, executable, hasher)) = file.take() {
                    let kind = if executable { "x" } else { "-" };
                    let hash = hex(&hasher.finalize());
                    lines.insert(path.clone(), format!("f {kind} {size} {hash} {path}\n"));
                }
            }
        })
        .map_err(|error| format!("the browser bundle at {}: {error}", source.display()))?;
        Ok(Self { lines })
    }
}

/// The digest of the bundle at `bundle`, a browser's directory.
///
/// # Errors
///
/// As [`Manifest::of`].
pub fn digest(bundle: &Path) -> Result<String, String> {
    Manifest::of(bundle, None).map(|manifest| manifest.digest())
}

/// The operator's subcommand that prints a bundle's digest.
pub const BROWSER_DIGEST_SUBCOMMAND: &str = "browser-digest";

/// `hatchling-broker browser-digest <browser>`: prints the digest of the
/// bundle of `browser`, the path `HATCHLING_BROWSER` names, for
/// `HATCHLING_BROWSER_SHA256`. Reads no configuration.
#[must_use]
pub fn browser_digest_main(browser: Option<&str>) -> ExitCode {
    let digested = browser
        .ok_or_else(|| String::from("browser-digest requires the browser's path"))
        .and_then(|browser| {
            Path::new(browser)
                .parent()
                .filter(|bundle| !bundle.as_os_str().is_empty())
                .ok_or_else(|| format!("{browser} names no directory"))
                .and_then(digest)
        });
    match digested {
        Ok(digest) => {
            let _ = writeln!(std::io::stdout(), "{digest}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            let _ = writeln!(
                std::io::stderr(),
                "hatchling-broker browser-digest: {reason}"
            );
            ExitCode::FAILURE
        }
    }
}

/// A verified copy of a bundle, held read-only for as long as it's kept,
/// and removed when it's dropped.
#[derive(Debug)]
pub struct Snapshot {
    staging: PathBuf,
    bundle: PathBuf,
}

impl Snapshot {
    /// Copies the bundle at `source` into a fresh staging directory, and
    /// keeps the copy if its digest is `pin`.
    ///
    /// # Errors
    ///
    /// A sentence for the operator: the copy couldn't be made, or its
    /// digest isn't the pinned one. Nothing is kept.
    pub fn take(source: &Path, pin: &str) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        let random = crate::approval::random_hex("")
            .map_err(|error| format!("cannot name the browser's snapshot: {error}"))?;
        let staging = std::env::temp_dir().join(format!(
            "hatchling-staging-{}-{}",
            std::process::id(),
            random.chars().take(32).collect::<String>()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .map_err(|error| format!("cannot make the browser's snapshot: {error}"))?;
        // Held from here: a failure below removes what was copied.
        let snapshot = Self {
            bundle: staging.join("bundle"),
            staging,
        };
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&snapshot.bundle)
            .map_err(|error| format!("cannot make the browser's snapshot: {error}"))?;
        let digest = Manifest::of(source, Some(&snapshot.bundle))?.digest();
        if digest != pin {
            return Err(format!(
                "the browser bundle's digest is {digest}, not the pinned {pin}: it is never started"
            ));
        }
        Ok(snapshot)
    }

    /// The copy: a browser's directory, as the cell binds it.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.bundle
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        // Through `remove_tree`: nothing outside the snapshot goes.
        let _ = hatchling_sys::remove_dir_tree(&self.staging);
    }
}

/// `bytes` as lowercase hex.
fn hex(bytes: &[u8]) -> String {
    crate::approval::hex_token("", bytes)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    /// A fresh directory for `name`, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("hatchling-bundle-{}-{name}", std::process::id()));
            let _ = hatchling_sys::remove_dir_tree(&dir);
            std::fs::create_dir_all(dir.join("bundle/locales")).unwrap();
            std::fs::write(dir.join("bundle/chrome"), b"browser").unwrap();
            std::fs::set_permissions(
                dir.join("bundle/chrome"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            std::fs::write(dir.join("bundle/locales/en.pak"), b"en").unwrap();
            Self(dir)
        }

        fn bundle(&self) -> PathBuf {
            self.0.join("bundle")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = hatchling_sys::remove_dir_tree(&self.0);
        }
    }

    fn sha256(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    #[test]
    fn a_manifest_names_every_entry_in_order_and_its_digest_hashes_it() {
        let scratch = Scratch::new("manifest");
        let manifest = Manifest::of(&scratch.bundle(), None).unwrap();
        let expected = format!(
            "f x 7 {} chrome\nd locales\nf - 2 {} locales/en.pak\n",
            sha256(b"browser"),
            sha256(b"en")
        );
        assert_eq!(manifest.text(), expected);
        assert_eq!(manifest.digest(), sha256(expected.as_bytes()));
        assert_eq!(digest(&scratch.bundle()).unwrap(), manifest.digest());
        assert_eq!(manifest.digest().len(), 64);
    }

    #[test]
    fn any_change_to_any_file_is_another_digest() {
        let scratch = Scratch::new("changes");
        let pinned = digest(&scratch.bundle()).unwrap();
        let pak = scratch.bundle().join("locales/en.pak");
        let chrome = scratch.bundle().join("chrome");
        let mut seen = std::collections::BTreeSet::from([pinned.clone()]);
        // A resource's bytes, its executable bit, a new file, a new
        // directory: each is a digest of its own.
        std::fs::write(&pak, b"EN").unwrap();
        assert!(seen.insert(digest(&scratch.bundle()).unwrap()), "bytes");
        std::fs::write(&pak, b"en").unwrap();
        assert_eq!(digest(&scratch.bundle()).unwrap(), pinned, "restored");
        std::fs::set_permissions(&chrome, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(seen.insert(digest(&scratch.bundle()).unwrap()), "mode");
        std::fs::set_permissions(&chrome, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(scratch.bundle().join("icudtl.dat"), b"").unwrap();
        assert!(seen.insert(digest(&scratch.bundle()).unwrap()), "a file");
        std::fs::remove_file(scratch.bundle().join("icudtl.dat")).unwrap();
        std::fs::create_dir(scratch.bundle().join("swiftshader")).unwrap();
        assert!(
            seen.insert(digest(&scratch.bundle()).unwrap()),
            "a directory"
        );
        // The executable's own bytes, and a library's.
        std::fs::write(&chrome, b"browsex").unwrap();
        assert!(
            seen.insert(digest(&scratch.bundle()).unwrap()),
            "the executable"
        );
        std::fs::write(&chrome, b"browser").unwrap();
        let library = scratch.bundle().join("libEGL.so");
        std::fs::write(&library, b"egl").unwrap();
        assert!(
            seen.insert(digest(&scratch.bundle()).unwrap()),
            "a library added"
        );
        std::fs::write(&library, b"EGL").unwrap();
        assert!(
            seen.insert(digest(&scratch.bundle()).unwrap()),
            "a library's bytes"
        );
    }

    #[test]
    fn a_snapshot_is_kept_only_when_its_copy_is_the_pinned_bundle() {
        let scratch = Scratch::new("snapshot");
        let pinned = digest(&scratch.bundle()).unwrap();
        let snapshot = Snapshot::take(&scratch.bundle(), &pinned).unwrap();
        assert_eq!(
            std::fs::read(snapshot.dir().join("chrome")).unwrap(),
            b"browser"
        );
        assert_eq!(
            digest(snapshot.dir()).unwrap(),
            pinned,
            "the copy is the bundle"
        );
        let mode = std::fs::metadata(snapshot.dir().join("locales/en.pak"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o444, "read-only");
        // In a staging directory the sweep knows, named for this broker.
        let staging = snapshot.dir().parent().unwrap().to_owned();
        let name = staging.file_name().unwrap().to_str().unwrap().to_owned();
        assert!(
            name.starts_with(&format!("hatchling-staging-{}-", std::process::id())),
            "{name}"
        );
        drop(snapshot);
        assert!(!staging.exists(), "removed when dropped");

        // Another pin: nothing is kept.
        let refused = Snapshot::take(&scratch.bundle(), &"0".repeat(64)).unwrap_err();
        assert!(refused.contains("not the pinned"), "{refused}");
        assert!(refused.contains(&pinned), "{refused}");
    }

    #[test]
    fn the_operator_gets_a_bundles_digest_by_its_browser() {
        let scratch = Scratch::new("cli");
        let browser = scratch.bundle().join("chrome");
        assert_eq!(
            browser_digest_main(Some(browser.to_str().unwrap())),
            ExitCode::SUCCESS
        );
        for refused in [None, Some("chrome"), Some("/nonexistent/chrome")] {
            assert_eq!(
                browser_digest_main(refused),
                ExitCode::FAILURE,
                "{refused:?}"
            );
        }
    }

    #[test]
    fn a_bundle_that_cant_be_walked_is_named() {
        let scratch = Scratch::new("refused");
        std::os::unix::fs::symlink("/etc", scratch.bundle().join("etc")).unwrap();
        let refused = digest(&scratch.bundle()).unwrap_err();
        assert!(refused.contains("the browser bundle at"), "{refused}");
        assert!(refused.contains("symlink"), "{refused}");
        let refused = Snapshot::take(&scratch.bundle(), &"0".repeat(64)).unwrap_err();
        assert!(refused.contains("symlink"), "{refused}");
    }
}
