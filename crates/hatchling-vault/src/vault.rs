//! The store: one file per secret under a `0700` root.
//!
//! Layout: `<root>/<name>` holds the raw secret bytes with mode `0600`.
//! Writes go to a `0600` temp file in the same directory (`write`,
//! `fsync`, `rename`): a crash can leave an orphaned temp file but never
//! a torn secret, and temp names are unique per call (pid + counter) so
//! an orphan can never collide with a later store. The directory itself
//! is fsynced after the rename so the rename is durable, not just the
//! file data.
//!
//! The root is created `0700` on open; an existing root with the wrong
//! mode or owner fails closed. Names are [`SecretName`]s, so joining is
//! safe by construction — no separators, no traversal.

use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Error, MAX_SECRET_BYTES, Secret, SecretName};

/// Expected mode of the vault root directory: owner-only.
const VAULT_DIR_MODE: u32 = 0o700;
/// Mode of every secret file and temp file: owner-only.
const SECRET_FILE_MODE: u32 = 0o600;

/// Makes temp file names unique within this process. Combined with the
/// pid, no two `store` calls — in this process or another — share a
/// temp name, so a crashed store's orphan can never be picked up or
/// collided with later.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates `path` as an owner-only directory if absent, then validates
/// it: a real directory (never a symlink), mode `0700`, owned by
/// `owner`. Shared by the vault root and the key-file parent directory —
/// one implementation of the check, so the two cannot drift.
pub(crate) fn ensure_owner_only_dir(path: &Path, owner: u32) -> Result<(), Error> {
    // Made 0700 at once, whatever the umask, and never chmodded by path
    // afterwards; one already there is only checked below.
    let _ = hatchling_sys::create_private_dir(path);
    // `symlink_metadata`: a symlink is never accepted, even one
    // pointing at a directory.
    let metadata = std::fs::symlink_metadata(path).map_err(|source| Error::Io { source })?;
    if !metadata.file_type().is_dir() {
        return Err(Error::NotADirectory {
            path: path.to_path_buf(),
        });
    }
    if metadata.mode() & 0o777 != VAULT_DIR_MODE || metadata.uid() != owner {
        return Err(Error::InsecurePermissions {
            path: path.to_path_buf(),
            expected_uid: owner,
        });
    }
    Ok(())
}

/// A credential store rooted at a filesystem directory.
///
/// `Vault` is `Sync`: the broker shares one across its per-connection
/// threads. Every method takes `&self`; the filesystem serializes.
#[derive(Debug)]
pub struct Vault {
    /// The validated `0700` root. Secret files live directly under it.
    root: PathBuf,
}

impl Vault {
    /// Opens (creating if absent) the vault rooted at `root`.
    ///
    /// The root must be a directory with mode `0700` owned by the
    /// current uid — created that way if absent, verified if present.
    /// Anything else fails closed: the vault will not operate on a
    /// store another user could read.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotADirectory`], [`Error::InsecurePermissions`],
    /// or [`Error::Io`].
    pub fn open(root: &Path) -> Result<Self, Error> {
        Self::open_as(root, rustix::process::getuid().as_raw())
    }

    /// Opens the vault, requiring the root to be owned by `owner`.
    ///
    /// Split out so tests can exercise the ownership check hermetically
    /// (no `setpriv`): the production path always passes the real uid.
    ///
    /// Create-first: the root is created when absent, then *always*
    /// re-statted and validated. There is no branch on the `stat` error
    /// kind — the validation below is the single gate, so a genuine
    /// creation failure surfaces there as [`Error::Io`]. This also
    /// narrows the create race: at most one creator wins, and every
    /// opener validates what is actually on disk.
    fn open_as(root: &Path, owner: u32) -> Result<Self, Error> {
        ensure_owner_only_dir(root, owner)?;
        tracing::debug!(vault_root = %root.display(), "vault opened");
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Stores `secret` under `name`, atomically replacing any existing
    /// value. The secret value is never logged; overwrites are.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on filesystem failure.
    pub fn store(&self, name: &SecretName, secret: &Secret) -> Result<(), Error> {
        self.store_raw(name, secret.as_bytes())?;
        tracing::info!(secret_name = name.as_str(), "stored secret");
        Ok(())
    }

    /// Stores raw `bytes` under `name` through the same atomic
    /// temp-write + fsync + rename pipeline as [`store`](Self::store),
    /// including temp-file cleanup on failure.
    ///
    /// `pub(crate)`: the sealed vault (`seal.rs`) stores envelopes,
    /// which exceed [`MAX_SECRET_BYTES`] by the fixed envelope
    /// overhead. The byte-level size bound lives in the envelope
    /// layer; this method enforces the pipeline, not the bound.
    pub(crate) fn store_raw(&self, name: &SecretName, bytes: &[u8]) -> Result<(), Error> {
        let target = self.path(name);
        let temp = self.temp_path(name);
        let outcome = self.store_inner(&target, &temp, bytes);
        if outcome.is_err() {
            // A failed store must not leave the temp file behind: it
            // holds secret bytes, and its name claims no secret.
            let _ = std::fs::remove_file(&temp);
        }
        outcome
    }

    /// The atomic-write pipeline: temp write, fsync, rename, directory
    /// fsync. Split out so [`store`](Self::store) owns the failure
    /// cleanup in one place.
    fn store_inner(&self, target: &Path, temp: &Path, bytes: &[u8]) -> Result<(), Error> {
        Self::write_temp(temp, bytes)?;
        std::fs::rename(temp, target).map_err(|source| Error::Io { source })?;
        // Sync the directory so the rename itself is durable.
        let dir = std::fs::File::open(&self.root).map_err(|source| Error::Io { source })?;
        dir.sync_all().map_err(|source| Error::Io { source })?;
        Ok(())
    }

    /// Fetches the secret stored under `name`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if no secret is stored under `name`,
    /// [`Error::TooLarge`] if the file somehow exceeds the secret limit
    /// (only this crate writes these files, but a foreign-grown file
    /// must not become an unbounded allocation), or [`Error::Io`].
    pub fn fetch(&self, name: &SecretName) -> Result<Secret, Error> {
        let bytes = self.read_bounded(name, MAX_SECRET_BYTES)?;
        tracing::debug!(secret_name = name.as_str(), "fetched secret");
        // `read_bounded` already enforced the limit; the constructor
        // cannot fail here, but its check is the backstop if the bound
        // above ever regresses.
        Secret::new(bytes)
    }

    /// Reads the raw bytes stored under `name`, bounding the allocation
    /// at `max`.
    ///
    /// `pub(crate)`: the sealed vault (`seal.rs`) reads envelopes,
    /// which exceed [`MAX_SECRET_BYTES`] by the fixed envelope
    /// overhead. The plaintext bound stays on
    /// [`fetch`](Self::fetch); the envelope layer owns its own bound.
    pub(crate) fn fetch_raw(&self, name: &SecretName, max: usize) -> Result<Vec<u8>, Error> {
        self.read_bounded(name, max)
    }

    /// The first four bytes stored under `name` (fewer for a shorter
    /// file), and how many: what a check needs to tell an envelope from
    /// plaintext without reading the secret itself.
    pub(crate) fn fetch_head(&self, name: &SecretName) -> Result<([u8; 4], usize), Error> {
        let mut file = self.open_entry(name)?;
        let mut head = [0u8; 4];
        let mut len = 0;
        while let Some(rest) = head.get_mut(len..).filter(|rest| !rest.is_empty()) {
            let read = file.read(rest).map_err(|source| Error::Io { source })?;
            if read == 0 {
                break;
            }
            len += read;
        }
        Ok((head, len))
    }

    /// The file stored under `name`, opened: [`Error::NotFound`] if there
    /// is none.
    fn open_entry(&self, name: &SecretName) -> Result<std::fs::File, Error> {
        std::fs::File::open(self.path(name)).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound { name: name.clone() }
            } else {
                Error::Io { source }
            }
        })
    }

    /// The bounded-read pipeline shared by [`fetch`](Self::fetch) and
    /// [`fetch_raw`](Self::fetch_raw): open with `NotFound` mapping,
    /// advisory size preflight, then a `take(max + 1)` read so growth
    /// past the bound is *detected* and rejected, never silently
    /// truncated.
    fn read_bounded(&self, name: &SecretName, max: usize) -> Result<Vec<u8>, Error> {
        let file = self.open_entry(name)?;
        // Reject an already-oversized file before allocating. Advisory
        // only: the `take` below is the real bound, so a file that grows
        // between this check and the read is still caught.
        let reported = file
            .metadata()
            .map_err(|source| Error::Io { source })?
            .len();
        let reported = usize::try_from(reported).unwrap_or(usize::MAX);
        if reported > max {
            return Err(Error::TooLarge { len: reported, max });
        }
        // The allocation itself is bounded: `take` caps the read at one
        // byte past the limit, so no matter what the filesystem does
        // mid-read, the buffer cannot grow without bound. One byte past
        // (not exactly at) the limit so growth is *detected* and rejected
        // rather than silently truncated.
        let read_limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
        // Sized to the file, so the read never grows the buffer and frees
        // a copy of what it held; a file that grew meanwhile still reads,
        // and is still refused past `max`.
        let mut bytes = Vec::with_capacity(reported + 1);
        Read::take(&file, read_limit)
            .read_to_end(&mut bytes)
            .map_err(|source| Error::Io { source })?;
        if bytes.len() > max {
            return Err(Error::TooLarge {
                len: bytes.len(),
                max,
            });
        }
        Ok(bytes)
    }

    /// Removes the secret stored under `name`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if no secret is stored under `name`,
    /// or [`Error::Io`].
    pub fn remove(&self, name: &SecretName) -> Result<(), Error> {
        std::fs::remove_file(self.path(name)).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound { name: name.clone() }
            } else {
                Error::Io { source }
            }
        })?;
        tracing::info!(secret_name = name.as_str(), "removed secret");
        Ok(())
    }

    /// Whether a secret is stored under `name`. Informational only —
    /// subject to TOCTOU; `fetch` is the authority.
    #[must_use]
    pub fn exists(&self, name: &SecretName) -> bool {
        self.path(name).is_file()
    }

    /// The filesystem path of a secret. Safe by construction:
    /// [`SecretName`] cannot contain a separator.
    fn path(&self, name: &SecretName) -> PathBuf {
        self.root.join(name.as_str())
    }

    /// A fresh temp path for one `store` call. Dot-prefixed (hidden,
    /// and unrepresentable as a [`SecretName`]); unique per call via
    /// pid + counter.
    fn temp_path(&self, name: &SecretName) -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.root.join(format!(
            ".{}.{}.{id}.tmp",
            name.as_str(),
            std::process::id()
        ))
    }

    /// Writes `bytes` to a new `0600` temp file and fsyncs it. Split
    /// out so the atomic-write sequence reads as one pipeline in
    /// [`store`](Self::store).
    fn write_temp(temp: &Path, bytes: &[u8]) -> Result<(), Error> {
        use std::io::Write as _;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(SECRET_FILE_MODE)
            .open(temp)
            .map_err(|source| Error::Io { source })?;
        file.write_all(bytes)
            .map_err(|source| Error::Io { source })?;
        file.sync_all().map_err(|source| Error::Io { source })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_SECRET_BYTES;
    use std::os::unix::fs::PermissionsExt as _;

    /// A fresh vault root unique to this test and process, so tests run
    /// in parallel without colliding.
    fn test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-vault-test-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn open_test_vault(name: &str) -> (Vault, PathBuf) {
        let root = test_root(name);
        let vault = Vault::open(&root).unwrap();
        (vault, root)
    }

    fn secret_name(name: &str) -> SecretName {
        name.parse().unwrap()
    }

    fn secret(bytes: &[u8]) -> Secret {
        Secret::new(bytes.to_vec()).unwrap()
    }

    /// Asserts no temp files linger: every store must end with exactly
    /// the secret files, never orphans.
    fn assert_no_temps(root: &Path) {
        let entries = std::fs::read_dir(root).unwrap();
        for entry in entries {
            let file_name = entry.unwrap().file_name();
            let name = file_name.to_string_lossy();
            assert!(!name.ends_with(".tmp"), "orphaned temp file: {name}");
        }
    }

    #[test]
    fn open_creates_root_with_0700() {
        let root = test_root("create-mode");
        let _vault = Vault::open(&root).unwrap();
        let mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "vault root must be owner-only");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_accepts_existing_valid_root() {
        let (vault, root) = open_test_vault("existing-ok");
        // Reopening the same root must succeed and see the same store.
        let name = secret_name("app-token");
        vault.store(&name, &secret(b"value")).unwrap();
        let reopened = Vault::open(&root).unwrap();
        assert_eq!(reopened.fetch(&name).unwrap().as_bytes(), b"value");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_rejects_wrong_mode() {
        // A root another user could read must fail closed. Pins the
        // mode comparison: `!=` against a `==` mutant.
        let root = test_root("wrong-mode");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = Vault::open(&root).unwrap_err();
        assert!(
            matches!(err, Error::InsecurePermissions { .. }),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_rejects_wrong_owner() {
        // Hermetic wrong-owner check via `open_as`: no setpriv needed.
        // Pins the uid comparison.
        let root = test_root("wrong-owner");
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bogus_uid = rustix::process::getuid().as_raw().wrapping_add(1);
        let err = Vault::open_as(&root, bogus_uid).unwrap_err();
        assert!(
            matches!(err, Error::InsecurePermissions { .. }),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_rejects_non_directory() {
        let root = test_root("not-a-dir");
        std::fs::write(&root, b"not a directory").unwrap();
        let err = Vault::open(&root).unwrap_err();
        assert!(
            matches!(err, Error::NotADirectory { .. }),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_file(&root);
    }

    #[test]
    fn roundtrip_store_fetch_remove() {
        let (vault, root) = open_test_vault("roundtrip");
        let name = secret_name("db-password");
        assert!(!vault.exists(&name));
        vault.store(&name, &secret(b"hunter2")).unwrap();
        assert!(vault.exists(&name));
        assert_eq!(vault.fetch(&name).unwrap().as_bytes(), b"hunter2");
        vault.remove(&name).unwrap();
        assert!(!vault.exists(&name));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fetch_and_remove_missing_are_not_found() {
        let (vault, root) = open_test_vault("not-found");
        let name = secret_name("ghost");
        let fetch_err = vault.fetch(&name).unwrap_err();
        assert!(
            matches!(fetch_err, Error::NotFound { name: ref n } if n == &name),
            "unexpected error: {fetch_err}"
        );
        let remove_err = vault.remove(&name).unwrap_err();
        assert!(matches!(remove_err, Error::NotFound { .. }));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn secret_files_are_0600() {
        let (vault, root) = open_test_vault("file-mode");
        let name = secret_name("api-key");
        vault.store(&name, &secret(b"value")).unwrap();
        let mode = std::fs::metadata(root.join("api-key"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "secret files must be owner-only");
        assert_no_temps(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn store_overwrites_atomically() {
        let (vault, root) = open_test_vault("overwrite");
        let name = secret_name("rotating");
        vault.store(&name, &secret(b"first")).unwrap();
        vault.store(&name, &secret(b"second")).unwrap();
        assert_eq!(vault.fetch(&name).unwrap().as_bytes(), b"second");
        assert_no_temps(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fetch_rejects_foreign_oversize_file() {
        // Defense in depth: a file that somehow exceeds the limit must
        // not become an unbounded allocation, even though only this
        // crate writes here.
        let (vault, root) = open_test_vault("oversize");
        let name = secret_name("big");
        std::fs::write(root.join("big"), vec![0u8; MAX_SECRET_BYTES + 1]).unwrap();
        let err = vault.fetch(&name).unwrap_err();
        assert!(matches!(err, Error::TooLarge { .. }), "unexpected: {err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_same_name_stores_never_tear() {
        // Unique temp names per call: racing stores serialize on the
        // rename, and the final value is always one complete write —
        // never a torn secret, never a temp leak.
        let (vault, root) = open_test_vault("concurrent");
        let name = secret_name("racy");
        std::thread::scope(|scope| {
            for thread in 0..4 {
                let vault = &vault;
                let name = &name;
                scope.spawn(move || {
                    for round in 0..10 {
                        let value = format!("thread-{thread}-round-{round}");
                        vault.store(name, &secret(value.as_bytes())).unwrap();
                    }
                });
            }
        });
        let final_value = vault.fetch(&name).unwrap();
        let text = String::from_utf8(final_value.as_bytes().to_vec()).unwrap();
        assert!(text.starts_with("thread-"), "torn secret: {text:?}");
        assert_no_temps(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_store_cleans_its_temp_file() {
        // Force the rename to fail *after* the temp file was written:
        // a directory at the target path makes rename return an error.
        // The temp file holds secret bytes and must not be left behind.
        let (vault, root) = open_test_vault("failed-store-cleanup");
        let name = secret_name("blocked");
        std::fs::create_dir(root.join("blocked")).unwrap();

        let result = vault.store(&name, &secret(b"super-secret"));
        assert!(
            result.is_err(),
            "expected the store to fail, got {result:?}"
        );
        assert_no_temps(&root);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fetch_rejects_foreign_oversize_file_before_reading() {
        // A file that did not come from `store` (which would have
        // rejected it via `Secret::new`) must not become an unbounded
        // allocation: the size preflight rejects it first.
        let (vault, root) = open_test_vault("oversize-preflight");
        let name = secret_name("grown");
        std::fs::write(root.join("grown"), vec![0xABu8; MAX_SECRET_BYTES + 1]).unwrap();

        let result = vault.fetch(&name);
        assert!(
            matches!(result, Err(Error::TooLarge { .. })),
            "expected TooLarge, got {result:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fetch_reports_the_foreign_file_size() {
        // Pins `>` on the preflight against `==`: a file of MAX+100
        // must be rejected with the file's own size. The `==` mutant
        // slips past the preflight and the post-read check reports the
        // truncated read size (MAX+1) instead.
        let (vault, root) = open_test_vault("oversize-len");
        let name = secret_name("grown-len");
        std::fs::write(root.join("grown-len"), vec![0xABu8; MAX_SECRET_BYTES + 100]).unwrap();
        let err = vault.fetch(&name).unwrap_err();
        assert!(
            matches!(err, Error::TooLarge { len, max } if len == MAX_SECRET_BYTES + 100 && max == MAX_SECRET_BYTES),
            "unexpected: {err:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fetch_accepts_foreign_file_at_exactly_max() {
        // Boundary pin: a file of exactly MAX_SECRET_BYTES passes both
        // the preflight and the post-read check. Kills `>` -> `>=` on
        // either check, and `>` -> `==` on the post-read check (which
        // would reject the boundary).
        let (vault, root) = open_test_vault("exact-max");
        let name = secret_name("exact");
        let bytes = vec![0xABu8; MAX_SECRET_BYTES];
        std::fs::write(root.join("exact"), &bytes).unwrap();
        let fetched = vault.fetch(&name).unwrap();
        assert_eq!(fetched.as_bytes(), bytes.as_slice());
        let _ = std::fs::remove_dir_all(&root);
    }
}
