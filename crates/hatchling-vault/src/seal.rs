//! Sealed vault: encryption at rest over the [`Vault`] store.
//!
//! [`SealedVault`] wraps [`Vault`]: every secret is stored as a
//! versioned XChaCha20-Poly1305 envelope, so the bytes on disk are
//! useless without the 256-bit [`MasterKey`]. The key is loaded
//! through a [`KeySource`] ([`FileKey`] ships now; the trait is the
//! extension point for a platform-keyring backend) and is zeroized on
//! drop via the `ZeroizeOnDrop` derive.
//!
//! Honest threat model: the real boundary stays peer authorization
//! plus filesystem permissions. Against a live file-read attacker a
//! file-held key adds nothing — [`FileKey`]'s documentation says so.
//! Encryption at rest defends the *at-rest* class: backups, disk
//! images, volume mounts that escape the broker, accidental
//! disclosure of secret files. The key path is independently
//! configurable so it can live apart from the vault.
//!
//! **Each store has its own key** (WO-017). A random key per store is
//! kept in the store's [`STORE_KEY_FILE`], wrapped under the master key
//! and bound to the store's name; every current envelope is sealed under
//! it. A process can then be given one store's key without the master key
//! or any other store's, and the master key can be rotated store by store.
//!
//! **Each envelope names what it holds.** A version-3 envelope (`HVL3`),
//! sealed under its store's key, authenticates, as associated data, a
//! domain separator, the [`Store`] it belongs to and the name it is
//! stored under. A sealed file renamed to another secret's name, or moved
//! into another store, fails to open with the same generic
//! [`Error::DecryptFailed`] as a tampered one: a cell granted one secret
//! can't be handed another by swapping files on disk. Older envelopes
//! (`HVL2`, bound the same way but sealed under the master key; `HVL1`,
//! with empty associated data) are read only by [`migrate`], which
//! re-seals them; anywhere else they are [`Error::OldEnvelope`], and
//! [`check`] refuses a store holding any.
//!
//! Fail-closed rules: a sealed vault never falls back to plaintext
//! ([`Error::NotSealed`]); authentication failure is one generic
//! error ([`Error::DecryptFailed`]) whether the key is wrong or the
//! bytes were tampered with; a missing or mispermissioned key file
//! refuses to start.
//!
//! Scrubbing discipline: key material and plaintext are zeroized after
//! use. Ciphertext is not — it is stored on disk by design, so
//! scrubbing a heap copy of it buys nothing.
//!
//! There is deliberately no `list`. [`migrate`], [`check`] and
//! [`rekey`] enumerate the store directory, but they are one-shot
//! operator actions that transform files in place and never return
//! secret material to any caller; the broker never exposes them over
//! the protocol.

use std::io::Read as _;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, AeadInPlace, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::vault::{Vault, ensure_owner_only_dir};
use crate::{Error, Secret, SecretName};
use hatchling_core::MAX_SECRET_BYTES;

/// Envelope magic: identifies a sealed envelope and its version.
/// `HVL3` = hatchling vault, envelope v3: sealed under its store's key,
/// bound to its store and name. Anything else is refused:
/// [`Error::OldEnvelope`] for `HVL2` and `HVL1`, [`Error::NotSealed`] for
/// any other bytes.
const ENVELOPE_MAGIC: &[u8; 4] = b"HVL3";

/// The version-2 magic: the same layout and binding, sealed under the
/// master key itself.
const ENVELOPE_V2_MAGIC: &[u8; 4] = b"HVL2";

/// The version-1 magic: the same layout, with empty associated data.
const ENVELOPE_V1_MAGIC: &[u8; 4] = b"HVL1";

/// The associated data's domain separator, so an envelope can never be
/// confused with another use of the same key.
const DOMAIN: &[u8] = b"hatchling vault envelope v3";

/// Version 2's domain separator.
const DOMAIN_V2: &[u8] = b"hatchling vault envelope v2";

/// The file in a store's root holding the store's key, wrapped under the
/// master key. A dotfile: no [`SecretName`] can name it.
pub const STORE_KEY_FILE: &str = ".store-key";

/// A rotation's next store key, wrapped under the new master key, until
/// every file is sealed under it ([`rekey`]).
const STORE_KEY_NEXT: &str = ".store-key.next";

/// A wrapped store key's magic: hatchling store key, v1.
const STORE_KEY_MAGIC: &[u8; 4] = b"HSK1";

/// A wrapped store key's associated data starts with this.
const STORE_KEY_DOMAIN: &[u8] = b"hatchling store key v1";

/// Which store a sealed file belongs to: part of every v2 envelope's
/// associated data, so a file moved between stores fails to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Store(&'static str);

impl Store {
    /// The broker's secrets: the vault proper.
    pub const SECRETS: Self = Self("secrets");
    /// Scheduled tasks and hooks.
    pub const SCHEDULES: Self = Self("schedules");
    /// Artifacts and their revisions.
    pub const ARTIFACTS: Self = Self("artifacts");
    /// Spaces and their members.
    pub const SPACES: Self = Self("spaces");
    /// The raw text of redaction records.
    pub const REDACTIONS: Self = Self("redactions");

    /// The store's label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.0
    }

    /// The associated data an envelope of `name` in this store carries.
    fn associated(self, name: &SecretName) -> Vec<u8> {
        self.bound(DOMAIN, name)
    }

    /// The associated data a version-2 envelope of `name` carried.
    fn associated_v2(self, name: &SecretName) -> Vec<u8> {
        self.bound(DOMAIN_V2, name)
    }

    /// `domain`, this store's label and `name`, each ended by a zero.
    fn bound(self, domain: &[u8], name: &SecretName) -> Vec<u8> {
        let mut data = Vec::with_capacity(domain.len() + self.0.len() + name.as_str().len() + 2);
        data.extend_from_slice(domain);
        data.push(0);
        data.extend_from_slice(self.0.as_bytes());
        data.push(0);
        data.extend_from_slice(name.as_str().as_bytes());
        data
    }

    /// The associated data this store's wrapped key carries: the key can
    /// only be unwrapped as this store's.
    fn key_associated(self) -> Vec<u8> {
        let mut data = Vec::with_capacity(STORE_KEY_DOMAIN.len() + self.0.len() + 1);
        data.extend_from_slice(STORE_KEY_DOMAIN);
        data.push(0);
        data.extend_from_slice(self.0.as_bytes());
        data
    }
}
/// `XChaCha20` nonce length in bytes. Fresh OS randomness per `store`;
/// the 192-bit width makes random nonces safe.
const NONCE_LEN: usize = 24;
/// Poly1305 authentication tag length in bytes.
const TAG_LEN: usize = 16;
/// Fixed envelope overhead: magic + nonce + tag. Sealed reads bound
/// the allocation at `MAX_SECRET_BYTES + ENVELOPE_OVERHEAD`.
const ENVELOPE_OVERHEAD: usize = ENVELOPE_MAGIC.len() + NONCE_LEN + TAG_LEN;
/// A wrapped store key's exact length: magic + nonce + key + tag.
const WRAPPED_STORE_KEY_LEN: usize = STORE_KEY_MAGIC.len() + NONCE_LEN + MASTER_KEY_LEN + TAG_LEN;
/// Master-key length in bytes (256 bits).
const MASTER_KEY_LEN: usize = 32;
/// Mode of the key file: owner-only.
const KEY_FILE_MODE: u32 = 0o600;

/// The 256-bit master key for the sealed vault.
///
/// Zeroized on drop via the `ZeroizeOnDrop` derive (declarative: no
/// `Drop` body for a mutant to delete), redacted in `Debug`, and
/// deliberately not `Clone` — key material is never duplicated. The
/// single owner (the broker) hands out borrows; [`SealedVault`]
/// holds `&MasterKey`, never an owned copy.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MasterKey([u8; 32]);

impl MasterKey {
    /// Generates a fresh key from the OS RNG.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Random`] if the OS provides no randomness —
    /// without it there is no key, so the sealed vault cannot start.
    pub fn generate() -> Result<Self, Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|source| Error::Random { source })?;
        Ok(Self(bytes))
    }

    /// Wraps 32 bytes of existing key material (e.g. read from a key
    /// file). The caller vouches for the source; this type's job is
    /// hygiene from here on.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key bytes, for wrapping under a KEK (`keyring.rs`). Never
    /// leaves the crate.
    pub(crate) const fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// The AEAD cipher keyed by this master key.
    fn cipher(&self) -> XChaCha20Poly1305 {
        let key: chacha20poly1305::Key = self.0.into();
        XChaCha20Poly1305::new(&key)
    }
}

/// A key the sealing functions take: a store's key for current
/// envelopes, the master key for older ones and for wrapping store keys.
trait Keyed {
    /// The AEAD cipher keyed by it.
    fn cipher(&self) -> XChaCha20Poly1305;
}

impl Keyed for MasterKey {
    fn cipher(&self) -> XChaCha20Poly1305 {
        Self::cipher(self)
    }
}

/// A store's own key (WO-017): random, wrapped under the master key in
/// the store's [`STORE_KEY_FILE`], and what every current envelope in the
/// store is sealed under. Zeroized on drop and never cloned, as the
/// master key is.
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct StoreKey([u8; 32]);

impl StoreKey {
    /// A fresh key from the OS RNG.
    fn generate() -> Result<Self, Error> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|source| Error::Random { source })?;
        Ok(Self(bytes))
    }
}

impl Keyed for StoreKey {
    fn cipher(&self) -> XChaCha20Poly1305 {
        let key: chacha20poly1305::Key = self.0.into();
        XChaCha20Poly1305::new(&key)
    }
}

impl std::fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreKey([redacted])")
    }
}

/// `key`, wrapped under `master` as `store`'s: magic || nonce ||
/// ciphertext+tag, with the store authenticated as associated data.
fn wrap_store_key(master: &MasterKey, store: Store, key: &StoreKey) -> Result<Vec<u8>, Error> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).map_err(|source| Error::Random { source })?;
    let nonce = XNonce::from(nonce_bytes);
    let mut wrapped = Vec::with_capacity(WRAPPED_STORE_KEY_LEN);
    wrapped.extend_from_slice(STORE_KEY_MAGIC);
    wrapped.extend_from_slice(nonce.as_slice());
    let body_start = wrapped.len();
    wrapped.extend_from_slice(&key.0);
    let (_, body) = wrapped.split_at_mut(body_start);
    let tag = master
        .cipher()
        .encrypt_in_place_detached(&nonce, &store.key_associated(), body)
        .map_err(|e| Error::SealFailed { source: e })?;
    wrapped.extend_from_slice(tag.as_slice());
    Ok(wrapped)
}

/// `store`'s key, unwrapped from `wrapped` under `master`.
fn unwrap_store_key(master: &MasterKey, store: Store, wrapped: &[u8]) -> Result<StoreKey, Error> {
    let malformed = || Error::StoreKeyMalformed {
        store: store.label(),
    };
    if wrapped.len() != WRAPPED_STORE_KEY_LEN
        || wrapped.get(..STORE_KEY_MAGIC.len()) != Some(STORE_KEY_MAGIC.as_slice())
    {
        return Err(malformed());
    }
    let nonce: [u8; NONCE_LEN] = wrapped
        .get(STORE_KEY_MAGIC.len()..STORE_KEY_MAGIC.len() + NONCE_LEN)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(malformed)?;
    let body = wrapped
        .get(STORE_KEY_MAGIC.len() + NONCE_LEN..)
        .ok_or_else(malformed)?;
    let plain = Zeroizing::new(
        master
            .cipher()
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: body,
                    aad: &store.key_associated(),
                },
            )
            .map_err(|_| Error::StoreKeyRefused {
                store: store.label(),
            })?,
    );
    let bytes: [u8; MASTER_KEY_LEN] = plain.as_slice().try_into().map_err(|_| malformed())?;
    Ok(StoreKey(bytes))
}

/// The wrapped key in `file` at the store's root, if there is one. One of
/// any other size is malformed.
fn read_wrapped(vault: &Vault, store: Store, file: &str) -> Result<Option<Vec<u8>>, Error> {
    match vault.read_dotfile(file, WRAPPED_STORE_KEY_LEN) {
        Err(Error::TooLarge { .. }) => Err(Error::StoreKeyMalformed {
            store: store.label(),
        }),
        other => other,
    }
}

/// Whether the store at `root` holds a current (v3) envelope: files
/// sealed under a store key.
fn holds_current(vault: &Vault, root: &Path) -> Result<bool, Error> {
    for entry in std::fs::read_dir(root).map_err(|source| Error::Io { source })? {
        if is_key_file(&entry) {
            continue;
        }
        let Some(name) = secret_entry(entry)? else {
            continue;
        };
        let (mut head, len) = vault.fetch_head(&name)?;
        let current = kind_of(head.get(..len).unwrap_or_default()) == Kind::Current;
        head.zeroize();
        if current {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `store`'s key at `root`, unwrapped under `master`; made, wrapped and
/// kept if the store has none yet.
///
/// Of two processes making one at once, one wins: the key file is
/// created only if absent, and the other reads the winner's. A store that
/// holds current envelopes but no key file is refused, never given a new
/// key that would leave them unreadable.
fn store_key(
    vault: &Vault,
    root: &Path,
    master: &MasterKey,
    store: Store,
) -> Result<StoreKey, Error> {
    if let Some(wrapped) = read_wrapped(vault, store, STORE_KEY_FILE)? {
        return unwrap_store_key(master, store, &wrapped);
    }
    if holds_current(vault, root)? {
        return Err(Error::StoreKeyMissing {
            store: store.label(),
        });
    }
    let key = StoreKey::generate()?;
    if vault.write_dotfile(STORE_KEY_FILE, &wrap_store_key(master, store, &key)?, false)? {
        return Ok(key);
    }
    let wrapped = read_wrapped(vault, store, STORE_KEY_FILE)?.ok_or_else(|| Error::StoreKeyMissing {
        store: store.label(),
    })?;
    unwrap_store_key(master, store, &wrapped)
}

/// Refuses a store with no key that holds anything but nothing: an older
/// envelope is [`Error::OldEnvelope`], anything else [`Error::NotSealed`].
/// Both mean: migrate first.
fn refuse_unmigrated(vault: &Vault, root: &Path) -> Result<(), Error> {
    for entry in std::fs::read_dir(root).map_err(|source| Error::Io { source })? {
        if is_key_file(&entry) {
            continue;
        }
        let Some(name) = secret_entry(entry)? else {
            continue;
        };
        let (mut head, len) = vault.fetch_head(&name)?;
        let kind = kind_of(head.get(..len).unwrap_or_default());
        head.zeroize();
        return Err(match kind {
            Kind::V2 | Kind::Old => Error::OldEnvelope { name },
            Kind::Current | Kind::Other => Error::NotSealed { name },
        });
    }
    Ok(())
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MasterKey([redacted])")
    }
}

/// How a sealed vault obtains its master key.
///
/// The trait is the extension point: [`FileKey`] ships now; a
/// platform-keyring backend can implement this trait later without
/// changing the envelope format or the vault API. Implementations
/// fail closed — no fallback to a weaker source.
pub trait KeySource {
    /// Loads the existing master key, or creates, persists, and
    /// returns a new one. Creation uses OS randomness.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on any load or creation failure.
    fn load_or_create(&self) -> Result<MasterKey, Error>;
}

/// Master key in a `0600` file, created with OS-random bytes if absent.
///
/// The parent directory is created `0700` if absent and validated
/// (`0700`, owned by the broker uid) if present — the same check as
/// the vault root, via the shared helper. An existing key file that
/// is not a regular file, has the wrong mode or owner, or holds
/// anything other than exactly 32 bytes fails closed.
///
/// Honest threat model: this is defense-in-depth for the *at-rest*
/// class — backups, disk images, volume mounts that escape the
/// broker, accidental disclosure of secret files. Against a live
/// file-read attacker it adds nothing over `0600` (the attacker
/// could read the key file as easily as the vault), so keep the key
/// path off the backup set — or on a separate volume — for the full
/// benefit. The real boundary stays peer authorization plus
/// filesystem permissions.
///
/// Operational hazard: the key file is load-bearing. Deleting it
/// does not delete the vault — the next start creates a *different*
/// key, and every existing secret then fails closed
/// ([`Error::DecryptFailed`]), loudly. Back the key file up with the
/// vault, or the sealed data is unrecoverable. First-run creation is
/// the explicit provisioning step; after that, the file must be
/// treated as irreplaceable state.
pub struct FileKey {
    /// The key file path. The parent directory carries the `0700`
    /// discipline; the file itself is `0600`.
    path: PathBuf,
}

impl FileKey {
    /// The key file path. Parent-directory and file discipline are
    /// enforced on load, not here.
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Loads or creates the key, requiring file and parent ownership
    /// by `owner`. Split from [`load_or_create`](KeySource::load_or_create)
    /// so tests exercise the checks hermetically; the production path
    /// always passes the real uid.
    fn load_or_create_as(&self, owner: u32) -> Result<MasterKey, Error> {
        let parent = self.path.parent().ok_or_else(|| Error::KeyLoad {
            path: self.path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "key path has no parent directory",
            ),
        })?;
        ensure_owner_only_dir(parent, owner)?;
        match std::fs::File::open(&self.path) {
            Ok(file) => Self::read_key(&self.path, &file, owner),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => self.create_key(owner),
            Err(source) => Err(Error::KeyLoad {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Reads and validates an existing key file: a regular file (never
    /// a symlink), mode `0600`, owned by `owner`, holding exactly 32
    /// bytes. Anything else fails closed.
    fn read_key(path: &Path, file: &std::fs::File, owner: u32) -> Result<MasterKey, Error> {
        // `symlink_metadata`: a symlink key file is never accepted,
        // even one pointing at a well-formed key.
        let metadata = std::fs::symlink_metadata(path).map_err(|source| Error::KeyLoad {
            path: path.to_path_buf(),
            source,
        })?;
        if !metadata.file_type().is_file()
            || metadata.mode() & 0o777 != KEY_FILE_MODE
            || metadata.uid() != owner
        {
            return Err(Error::InsecureKeyFile {
                path: path.to_path_buf(),
            });
        }
        // Bounded read: one byte past the key length, so a longer file
        // is *detected* and rejected, never silently truncated. A key
        // file with trailing junk is corrupt, not "a key plus junk".
        // Into a fixed buffer one byte past the key, wiped when dropped: a
        // growing one would free copies of the key.
        let mut bytes = Zeroizing::new([0u8; MASTER_KEY_LEN + 1]);
        let mut len = 0;
        let mut reader = file;
        while let Some(rest) = bytes.get_mut(len..).filter(|rest| !rest.is_empty()) {
            let read = reader.read(rest).map_err(|source| Error::KeyLoad {
                path: path.to_path_buf(),
                source,
            })?;
            if read == 0 {
                break;
            }
            len += read;
        }
        if len != MASTER_KEY_LEN {
            return Err(Error::InvalidKey {
                path: path.to_path_buf(),
                len,
            });
        }
        let key = bytes
            .get(..MASTER_KEY_LEN)
            .and_then(|read| <[u8; MASTER_KEY_LEN]>::try_from(read).ok())
            .ok_or_else(|| Error::InvalidKey {
                path: path.to_path_buf(),
                len,
            })?;
        Ok(MasterKey::from_bytes(key))
    }

    /// Creates the key file with fresh OS-random bytes, mode `0600`.
    /// If a concurrent creator wins the race (`AlreadyExists`), reads
    /// the key it wrote instead of failing.
    ///
    /// Creation-leg I/O failures are [`Error::Io`], not
    /// [`Error::KeyLoad`]: `KeyLoad` is the *read* leg
    /// (`load_or_create_as`, `read_key`), and the two legs must stay
    /// distinguishable — silently creating a fresh key over an
    /// unreadable one would orphan every secret sealed under the old
    /// key, so "create on any error" must be a testably different
    /// outcome from "fail the load".
    fn create_key(&self, owner: u32) -> Result<MasterKey, Error> {
        let key = MasterKey::generate()?;
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(KEY_FILE_MODE)
            .open(&self.path);
        match opened {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(&key.0)
                    .map_err(|source| Error::Io { source })?;
                file.sync_all().map_err(|source| Error::Io { source })?;
                Ok(key)
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                // Lost the creation race: the winner's key is the key.
                // `key` drops here, zeroized by the derive.
                let file = std::fs::File::open(&self.path).map_err(|source| Error::KeyLoad {
                    path: self.path.clone(),
                    source,
                })?;
                Self::read_key(&self.path, &file, owner)
            }
            Err(source) => Err(Error::Io { source }),
        }
    }
}

impl FileKey {
    /// Loads an existing key file, never creating one: a KEK
    /// that is missing must fail the unlock, not be replaced by a fresh
    /// one that opens nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyLoad`] if the file is absent or unreadable,
    /// or the [`load_or_create`](KeySource::load_or_create) checks'
    /// errors for a file that is not a sound key file.
    pub fn load(&self) -> Result<MasterKey, Error> {
        self.load_as(rustix::process::getuid().as_raw())
    }

    fn load_as(&self, owner: u32) -> Result<MasterKey, Error> {
        let file = std::fs::File::open(&self.path).map_err(|source| Error::KeyLoad {
            path: self.path.clone(),
            source,
        })?;
        Self::read_key(&self.path, &file, owner)
    }
}

impl KeySource for FileKey {
    fn load_or_create(&self) -> Result<MasterKey, Error> {
        self.load_or_create_as(rustix::process::getuid().as_raw())
    }
}

/// The sealed vault: [`Vault`] storage with every secret encrypted.
///
/// Borrows the [`MasterKey`]: the key has exactly one owner (the
/// broker), and this type never clones it. Method shapes mirror
/// [`Vault`]'s; `store` seals the envelope and `fetch` opens it.
/// `remove` and `exists` delegate unchanged — they never touch key
/// material.
pub struct SealedVault<'a> {
    vault: Vault,
    key: StoreKey,
    store: Store,
    master: std::marker::PhantomData<&'a MasterKey>,
}

impl<'a> SealedVault<'a> {
    /// Opens the sealed vault: opens the underlying store, borrowing
    /// `key` for all cryptographic operations.
    ///
    /// No migration happens here. A store that still holds plaintext
    /// must be migrated explicitly via [`migrate`];
    /// `fetch` on a non-envelope file fails closed with
    /// [`Error::NotSealed`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotADirectory`],
    /// [`Error::InsecurePermissions`], or [`Error::Io`] exactly as
    /// [`Vault::open`] does.
    pub fn open(root: &Path, key: &'a MasterKey) -> Result<Self, Error> {
        Self::open_in(root, key, Store::SECRETS)
    }

    /// Opens the sealed store `store` at `root`, as [`open`](Self::open)
    /// opens the secrets: every envelope it seals or opens is bound to
    /// `store`.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    pub fn open_in(root: &Path, key: &'a MasterKey, store: Store) -> Result<Self, Error> {
        let vault = Vault::open(root)?;
        let key = store_key(&vault, root, key, store)?;
        Ok(Self {
            vault,
            key,
            store,
            master: std::marker::PhantomData,
        })
    }

    /// Stores `secret` under `name` as a sealed envelope, atomically
    /// replacing any existing value. The secret value is never logged;
    /// overwrites are.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SealFailed`] (defensive: sealing with a valid
    /// key and a fresh nonce cannot fail) or [`Error::Io`] on
    /// filesystem failure.
    pub fn store(&self, name: &SecretName, secret: &Secret) -> Result<(), Error> {
        let envelope = seal_envelope(&self.key, self.store, name, secret.as_bytes())?;
        self.vault.store_raw(name, &envelope)?;
        tracing::info!(secret_name = name.as_str(), "stored sealed secret");
        Ok(())
    }

    /// Fetches and unseals the secret stored under `name`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if no secret is stored under `name`,
    /// [`Error::NotSealed`] if the file is not an envelope (never read
    /// as plaintext), [`Error::Malformed`] for a truncated envelope,
    /// [`Error::DecryptFailed`] for a wrong key or tampered bytes
    /// (indistinguishable by design), [`Error::TooLarge`] for a
    /// foreign-grown file, or [`Error::Io`].
    pub fn fetch(&self, name: &SecretName) -> Result<Secret, Error> {
        let envelope = self
            .vault
            .fetch_raw(name, MAX_SECRET_BYTES + ENVELOPE_OVERHEAD)?;
        let secret = open_envelope(&self.key, self.store, name, &envelope)?;
        tracing::debug!(secret_name = name.as_str(), "fetched sealed secret");
        Ok(secret)
    }

    /// Removes the secret stored under `name`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotFound`] if no secret is stored under `name`,
    /// or [`Error::Io`].
    pub fn remove(&self, name: &SecretName) -> Result<(), Error> {
        self.vault.remove(name)
    }

    /// Whether a secret is stored under `name`. Informational only —
    /// subject to TOCTOU; `fetch` is the authority.
    #[must_use]
    pub fn exists(&self, name: &SecretName) -> bool {
        self.vault.exists(name)
    }
}

/// Seals `plaintext`, stored as `name` in `store`, into a version-3
/// envelope under the store's `key`: magic || nonce || ciphertext+tag,
/// with the store and name authenticated as associated data.
///
/// The nonce is fresh OS randomness per call; encryption happens in
/// place in the envelope's tail, so no intermediate ciphertext buffer
/// exists to scrub. Plaintext is borrowed, never copied except into
/// the envelope being built.
fn seal_envelope(
    key: &impl Keyed,
    store: Store,
    name: &SecretName,
    plaintext: &[u8],
) -> Result<Vec<u8>, Error> {
    let cipher = key.cipher();
    // Fresh OS randomness per seal: XChaCha's 192-bit nonce makes
    // random nonces safe (reuse is catastrophic only for repeated
    // nonces under one key, which randomness at this width prevents).
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).map_err(|source| Error::Random { source })?;
    let nonce = XNonce::from(nonce_bytes);
    let mut envelope = Vec::with_capacity(ENVELOPE_OVERHEAD + plaintext.len());
    envelope.extend_from_slice(ENVELOPE_MAGIC);
    envelope.extend_from_slice(nonce.as_slice());
    let body_start = envelope.len();
    envelope.extend_from_slice(plaintext);
    // `body_start` was just read from the length after pushing exactly
    // magic + nonce, so the split below is infallible by construction.
    debug_assert_eq!(body_start, ENVELOPE_MAGIC.len() + NONCE_LEN);
    let (_, body) = envelope.split_at_mut(body_start);
    let tag = cipher
        .encrypt_in_place_detached(&nonce, &store.associated(name), body)
        .map_err(|e| Error::SealFailed { source: e })?;
    envelope.extend_from_slice(tag.as_slice());
    Ok(envelope)
}

/// Which kind of file `bytes` are, by their first four bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A current (v3) envelope.
    Current,
    /// A version-2 envelope.
    V2,
    /// A version-1 envelope.
    Old,
    /// Anything else: plaintext, or an unknown version.
    Other,
}

/// What `bytes` are.
fn kind_of(bytes: &[u8]) -> Kind {
    match bytes.get(..ENVELOPE_MAGIC.len()) {
        Some(magic) if magic == ENVELOPE_MAGIC => Kind::Current,
        Some(magic) if magic == ENVELOPE_V2_MAGIC => Kind::V2,
        Some(magic) if magic == ENVELOPE_V1_MAGIC => Kind::Old,
        _ => Kind::Other,
    }
}

/// Opens a sealed envelope: checks the magic, splits the nonce, and
/// authenticates + decrypts the body.
///
/// Anything without the magic is [`Error::NotSealed`] — the sealed
/// vault never guesses at plaintext. A magic-bearing but truncated
/// envelope is [`Error::Malformed`]. Authentication failure is the
/// single generic [`Error::DecryptFailed`]: wrong key, tampered
/// ciphertext, and tampered tag are indistinguishable by design.
fn open_envelope(
    key: &impl Keyed,
    store: Store,
    name: &SecretName,
    envelope: &[u8],
) -> Result<Secret, Error> {
    if matches!(kind_of(envelope), Kind::Old | Kind::V2) {
        return Err(Error::OldEnvelope { name: name.clone() });
    }
    open_versioned(
        key,
        *ENVELOPE_MAGIC,
        &store.associated(name),
        name,
        envelope,
    )
}

/// Opens an envelope carrying `magic`, authenticated with `associated`.
fn open_versioned(
    key: &impl Keyed,
    magic_wanted: [u8; 4],
    associated: &[u8],
    name: &SecretName,
    envelope: &[u8],
) -> Result<Secret, Error> {
    let not_sealed = || Error::NotSealed { name: name.clone() };
    let malformed = || Error::Malformed { name: name.clone() };
    // Magic first, with the length gate before it: a file shorter
    // than the magic cannot be an envelope at all, so it is
    // `NotSealed` (a short plaintext file), never `Malformed`.
    let Some(magic_slice) = envelope.get(..ENVELOPE_MAGIC.len()) else {
        return Err(not_sealed());
    };
    // `get(..4)` yields exactly four bytes on `Some`, so this
    // conversion is infallible; the error arm is fail-closed dead
    // code, not a panic.
    let magic = <&[u8; 4]>::try_from(magic_slice).map_err(|_| malformed())?;
    if *magic != magic_wanted {
        return Err(not_sealed());
    }
    let nonce: [u8; NONCE_LEN] = envelope
        .get(ENVELOPE_MAGIC.len()..ENVELOPE_MAGIC.len() + NONCE_LEN)
        .and_then(|slice| slice.try_into().ok())
        .ok_or_else(malformed)?;
    let ciphertext = envelope
        .get(ENVELOPE_MAGIC.len() + NONCE_LEN..)
        .ok_or_else(malformed)?;
    // The smallest valid body is the bare authentication tag: an empty
    // plaintext seals to magic + nonce + tag.
    if ciphertext.len() < TAG_LEN {
        return Err(malformed());
    }
    let nonce = XNonce::from(nonce);
    let plaintext = key
        .cipher()
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad: associated,
            },
        )
        .map_err(|_| Error::DecryptFailed { name: name.clone() })?;
    Secret::new(plaintext)
}

/// One directory entry that could hold a secret: dotfiles (crashed
/// temp files) and names outside the [`SecretName`] alphabet are not
/// secrets and come back as `None`. A non-UTF-8 file name cannot be a
/// `SecretName` (the alphabet is ASCII), so it is skipped, not failed.
fn secret_entry(entry: std::io::Result<std::fs::DirEntry>) -> Result<Option<SecretName>, Error> {
    let entry = entry.map_err(|source| Error::Io { source })?;
    let name = entry.file_name();
    let name_str = name.to_string_lossy();
    if name_str.starts_with('.') {
        return Ok(None);
    }
    Ok(name_str.parse().ok())
}

/// Whether `entry` is one of the store's key files: neither a secret nor
/// a file to skip and count.
fn is_key_file(entry: &std::io::Result<std::fs::DirEntry>) -> bool {
    entry.as_ref().is_ok_and(|entry| {
        let name = entry.file_name();
        name == STORE_KEY_FILE || name == STORE_KEY_NEXT
    })
}

/// Outcome of [`migrate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrateReport {
    /// Plaintext files sealed into current envelopes.
    pub encrypted: usize,
    /// Version-1 envelopes re-sealed as current ones.
    pub upgraded: usize,
    /// Files left untouched: current envelopes (verified), temp files,
    /// foreign names.
    pub skipped: usize,
}

/// One-shot migration of the store `store` at `root` to current envelopes.
///
/// Plaintext files are sealed, and version-1 envelopes re-sealed, each
/// bound to `store` and its name. Afterwards the store passes [`check`].
///
/// Operator action, run when adopting the sealed vault and after an
/// upgrade, not a runtime API. A version-1 envelope is opened under
/// `key` before it is re-sealed, and a current one is verified, not
/// assumed: either failing to authenticate fails the migration closed
/// (it is tampered or corrupt, not "already migrated"). A file with an
/// unknown magic is not touched and fails it too. Dotfiles and foreign
/// names are skipped and counted. Each rewrite goes through the vault's
/// atomic temp-write + rename, one file at a time, so an interrupted
/// migration leaves every file readable as either version, and running
/// it again resumes it.
///
/// Enumeration happens here and in [`check`] and [`rekey`] only. This
/// never returns secret material to its caller, and the broker never
/// exposes it over the protocol.
///
/// # Errors
///
/// Returns [`Error::Malformed`] or [`Error::DecryptFailed`] for a
/// tampered envelope, [`Error::NotSealed`] for an unknown version, and
/// I/O or permission errors from the store's root.
pub fn migrate(root: &Path, key: &MasterKey, store: Store) -> Result<MigrateReport, Error> {
    let vault = Vault::open(root)?;
    let master = key;
    let key = store_key(&vault, root, master, store)?;
    let mut report = MigrateReport {
        encrypted: 0,
        upgraded: 0,
        skipped: 0,
    };
    for entry in std::fs::read_dir(root).map_err(|source| Error::Io { source })? {
        if is_key_file(&entry) {
            continue;
        }
        let Some(name) = secret_entry(entry)? else {
            report.skipped += 1;
            continue;
        };
        // Plaintext files were written by `Vault::store`, so they fit
        // the plaintext bound; the sealed bound also covers envelopes.
        // Anything larger is foreign and fails the migration closed.
        let mut bytes = vault.fetch_raw(&name, MAX_SECRET_BYTES + ENVELOPE_OVERHEAD)?;
        let plaintext = match kind_of(&bytes) {
            Kind::Current => {
                // Verified, not assumed. The `Secret` drops zeroized.
                open_envelope(&key, store, &name, &bytes)?;
                bytes.zeroize();
                report.skipped += 1;
                continue;
            }
            Kind::V2 => {
                let opened = open_versioned(
                    master,
                    *ENVELOPE_V2_MAGIC,
                    &store.associated_v2(&name),
                    &name,
                    &bytes,
                )?;
                report.upgraded += 1;
                opened.as_bytes().to_vec()
            }
            Kind::Old => {
                let opened = open_versioned(master, *ENVELOPE_V1_MAGIC, b"", &name, &bytes)?;
                report.upgraded += 1;
                opened.as_bytes().to_vec()
            }
            Kind::Other
                if bytes
                    .get(..ENVELOPE_MAGIC.len())
                    .is_some_and(looks_like_magic) =>
            {
                // `HVL` and a digit: an envelope version this build
                // doesn't know. Never sealed over as if plaintext.
                return Err(Error::NotSealed { name });
            }
            Kind::Other => {
                report.encrypted += 1;
                std::mem::take(&mut bytes)
            }
        };
        let mut plaintext = Zeroizing::new(plaintext);
        let envelope = seal_envelope(&key, store, &name, &plaintext)?;
        plaintext.zeroize();
        bytes.zeroize();
        vault.store_raw(&name, &envelope)?;
    }
    Ok(report)
}

/// Whether `magic` has the shape of an envelope magic: `HVL` and a digit.
const fn looks_like_magic(magic: &[u8]) -> bool {
    matches!(magic, [b'H', b'V', b'L', digit] if digit.is_ascii_digit())
}

/// Whether the store at `root` holds a sealed envelope of any version.
///
/// A store that doesn't exist holds none. Nothing is decrypted, so no key
/// is needed: this is how a start that has no key tells a new vault from
/// one whose key is elsewhere.
///
/// # Errors
///
/// I/O or permission errors from the store's root.
pub fn holds_envelopes(root: &Path) -> Result<bool, Error> {
    if !root.try_exists().map_err(|source| Error::Io { source })? {
        return Ok(false);
    }
    let vault = Vault::open(root)?;
    if vault
        .read_dotfile(STORE_KEY_FILE, WRAPPED_STORE_KEY_LEN)
        .is_ok_and(|key| key.is_some())
    {
        return Ok(true);
    }
    for entry in std::fs::read_dir(root).map_err(|source| Error::Io { source })? {
        if is_key_file(&entry) {
            continue;
        }
        let Some(name) = secret_entry(entry)? else {
            continue;
        };
        let (mut head, len) = vault.fetch_head(&name)?;
        let sealed = head
            .get(..len)
            .and_then(|head| head.get(..ENVELOPE_MAGIC.len()))
            .is_some_and(looks_like_magic);
        head.zeroize();
        if sealed {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Refuses a store that holds anything but current envelopes.
///
/// Old (v1) envelopes, unknown versions and plaintext are named in the
/// error. The broker calls this for every store it keeps, before it
/// serves. Nothing is decrypted, so no key is needed.
///
/// # Errors
///
/// Returns [`Error::UnmigratedStore`] naming every such file, and I/O
/// or permission errors from the store's root.
pub fn check(root: &Path, store: Store) -> Result<(), Error> {
    let vault = Vault::open(root)?;
    let mut stale = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|source| Error::Io { source })? {
        if is_key_file(&entry) {
            continue;
        }
        let Some(name) = secret_entry(entry)? else {
            continue;
        };
        // Only the first four bytes are read: a plaintext file's secret
        // never enters memory.
        let (mut head, len) = vault.fetch_head(&name)?;
        let current = kind_of(head.get(..len).unwrap_or_default()) == Kind::Current;
        head.zeroize();
        if !current {
            stale.push(name.as_str().to_owned());
        }
    }
    if stale.is_empty() {
        return Ok(());
    }
    stale.sort();
    Err(Error::UnmigratedStore {
        store: store.label(),
        names: stale.join(", "),
    })
}

/// Outcome of [`rekey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RekeyReport {
    /// Files this run re-encrypted under the new key.
    pub rekeyed: usize,
    /// Files left untouched: temp files, foreign names, and files
    /// already under the new key (e.g. from an interrupted earlier
    /// run — see the resumability note on [`rekey`]).
    pub skipped: usize,
}

/// One-shot master-key rotation of the store `store` under `root`: a new
/// store key, wrapped under `new_key`, and every sealed file re-sealed
/// under it.
///
/// A routine rewrap of the old store key would leave a master key stolen
/// earlier able to unwrap it from a backup, so rotation replaces the store
/// key too. Three steps, each resumable:
/// 1. The next store key is kept, wrapped under `new_key`, in
///    `.store-key.next`.
/// 2. Every envelope is opened under the current store key (unwrapped
///    under `old_key`), then sealed under the next. A file that already
///    opens under the next key is done, so a run never aborts on its own
///    output; one that opens under neither aborts with
///    [`Error::DecryptFailed`] before any file is written.
/// 3. The next key becomes the store's key.
///
/// Re-running with the same keys after an interruption finishes it, and
/// after it's done reports every file skipped. A version-1 or version-2
/// envelope aborts with [`Error::OldEnvelope`] and a plaintext file with
/// [`Error::NotSealed`]: migrate first. A store with no key and no
/// envelopes has nothing to rotate.
///
/// Same one-shot operator discipline as [`migrate`]: enumeration here
/// never returns secret material to a caller, and the broker never
/// exposes it over the protocol. Note the transient memory cost: all
/// secrets are held while re-encrypting.
///
/// # Errors
///
/// As above, [`Error::StoreKeyRefused`] if the store's key opens under
/// neither key, and I/O or permission errors from the store's root.
pub fn rekey(
    root: &Path,
    old_key: &MasterKey,
    new_key: &MasterKey,
    store: Store,
) -> Result<RekeyReport, Error> {
    let vault = Vault::open(root)?;
    let Some(wrapped) = read_wrapped(&vault, store, STORE_KEY_FILE)? else {
        if holds_current(&vault, root)? {
            return Err(Error::StoreKeyMissing {
                store: store.label(),
            });
        }
        // Nothing sealed under a store key: anything else here must be
        // migrated first.
        refuse_unmigrated(&vault, root)?;
        return Ok(RekeyReport {
            rekeyed: 0,
            skipped: 0,
        });
    };
    // The store's key opens under the old master key, or, once an earlier
    // run finished, under the new one: then every file is already done.
    let (current, finished) = match unwrap_store_key(old_key, store, &wrapped) {
        Ok(key) => (key, false),
        Err(Error::StoreKeyRefused { .. }) => (unwrap_store_key(new_key, store, &wrapped)?, true),
        Err(other) => return Err(other),
    };
    let next = if finished {
        None
    } else if let Some(next) = read_wrapped(&vault, store, STORE_KEY_NEXT)? {
        Some(unwrap_store_key(new_key, store, &next)?)
    } else {
        let next = StoreKey::generate()?;
        vault.write_dotfile(
            STORE_KEY_NEXT,
            &wrap_store_key(new_key, store, &next)?,
            true,
        )?;
        Some(next)
    };
    // Phase 1: open every envelope. Any failure aborts before the first
    // write in phase 2.
    let mut opened: Vec<(SecretName, Secret)> = Vec::new();
    let mut skipped = 0;
    for entry in std::fs::read_dir(root).map_err(|source| Error::Io { source })? {
        if is_key_file(&entry) {
            continue;
        }
        let Some(name) = secret_entry(entry)? else {
            skipped += 1;
            continue;
        };
        let envelope = vault.fetch_raw(&name, MAX_SECRET_BYTES + ENVELOPE_OVERHEAD)?;
        let Some(next) = &next else {
            // Finished earlier: every file must open under the store's key.
            open_envelope(&current, store, &name, &envelope)?;
            skipped += 1;
            continue;
        };
        match open_envelope(&current, store, &name, &envelope) {
            Ok(secret) => opened.push((name, secret)),
            Err(Error::DecryptFailed { .. })
                if open_envelope(next, store, &name, &envelope).is_ok() =>
            {
                skipped += 1;
            }
            Err(other) => return Err(other),
        }
    }
    let Some(next) = next else {
        return Ok(RekeyReport {
            rekeyed: 0,
            skipped,
        });
    };
    // Phase 2: every envelope opened cleanly; re-seal under the next key
    // through the atomic pipeline. `Secret`s drop zeroized.
    let mut rekeyed = 0;
    for (name, secret) in &opened {
        let envelope = seal_envelope(&next, store, name, secret.as_bytes())?;
        vault.store_raw(name, &envelope)?;
        rekeyed += 1;
    }
    // Phase 3: the next key is the store's key.
    vault.rename_dotfile(STORE_KEY_NEXT, STORE_KEY_FILE)?;
    Ok(RekeyReport { rekeyed, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// A fresh directory unique to this test and process, so tests run
    /// in parallel without colliding. The directory itself is NOT
    /// created: `Vault::open` and `FileKey` create what they need with
    /// the `0700`/`0600` discipline, and a pre-created directory would
    /// carry the umask mode and fail validation.
    ///
    /// The guard removes the directory when the test ends, pass or
    /// fail.
    fn test_dir(name: &str) -> TestDir {
        let dir =
            std::env::temp_dir().join(format!("hatchling-seal-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        TestDir(dir)
    }

    /// A test's scratch directory, removed on drop so a failing
    /// assertion cannot leak it.
    struct TestDir(PathBuf);

    impl std::ops::Deref for TestDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl AsRef<Path> for TestDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_test_dir_is_removed_when_its_guard_drops() {
        let dir = test_dir("guard");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        std::fs::write(dir.join("nested").join("file"), b"x").unwrap();
        let path = dir.to_path_buf();
        drop(dir);
        assert!(!path.exists(), "{} was left behind", path.display());
    }

    fn test_key() -> MasterKey {
        MasterKey::generate().unwrap()
    }

    fn secret_name(name: &str) -> SecretName {
        name.parse().unwrap()
    }

    fn secret(bytes: &[u8]) -> Secret {
        Secret::new(bytes.to_vec()).unwrap()
    }

    /// The raw bytes of the secret file on disk: the sealed vault
    /// stores one file per secret named by the secret name.
    fn raw_bytes(root: &Path, name: &SecretName) -> Vec<u8> {
        std::fs::read(root.join(name.as_str())).unwrap()
    }

    /// `MasterKey` has no equality: behavioral check — a secret sealed
    /// under `a` must open under `b`.
    fn assert_same_key(a: &MasterKey, b: &MasterKey, root: &Path) {
        let va = SealedVault::open(root, a).unwrap();
        let vb = SealedVault::open(root, b).unwrap();
        let name = secret_name("equality-probe");
        va.store(&name, &secret(b"probe")).unwrap();
        let back = vb.fetch(&name).unwrap();
        assert_eq!(back.as_bytes(), b"probe");
    }

    fn real_uid() -> u32 {
        rustix::process::getuid().as_raw()
    }

    #[test]
    fn debug_is_redacted() {
        // Deleting the custom `Debug` impl (falling back to a derived
        // one that prints the bytes) must fail this test.
        let key = MasterKey::from_bytes([0xAB; 32]);
        assert_eq!(format!("{key:?}"), "MasterKey([redacted])");
    }

    #[test]
    #[allow(
        unsafe_code,
        reason = "reads the dropped key's former stack slot through a raw pointer; the frame is alive and nothing has reused the slot, mirroring secret.rs's drop-zeroize test"
    )]
    fn drop_zeroizes_key_material() {
        // The whole point of the `ZeroizeOnDrop` derive: the
        // `drop -> ()` mutant (deleting the derive) must fail this
        // test. The key is dropped *in place* at the end of the
        // block: passing it by value to `drop()` would move it
        // first, and the scrub would land on the moved-to slot
        // instead of the observed one. (Moves always leave a stale
        // copy behind — which is why the type is non-cloneable and
        // is borrowed as `&MasterKey` across the broker rather than
        // passed by value.)
        let addr: *const u8;
        {
            let key = MasterKey::from_bytes([0xAB; 32]);
            addr = key.0.as_ptr();
        }
        // SAFETY: out-of-model read (use-after-free) — the only way
        // to observe the scrub. It pins the property the type
        // exists for, and must never be imitated in production code.
        let after = unsafe { std::slice::from_raw_parts(addr, 32) };
        assert!(
            after.iter().all(|&b| b == 0),
            "key material survived drop: {after:?}"
        );
    }

    #[test]
    fn roundtrip() {
        let dir = test_dir("roundtrip");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        let name = secret_name("api-token");
        vault.store(&name, &secret(b"super-secret-value")).unwrap();
        assert!(vault.exists(&name));
        let back = vault.fetch(&name).unwrap();
        assert_eq!(back.as_bytes(), b"super-secret-value");
        vault.remove(&name).unwrap();
        assert!(!vault.exists(&name));
        let err = vault.fetch(&name).unwrap_err();
        assert!(matches!(err, Error::NotFound { .. }));
    }

    /// A version-1 envelope of `plaintext`, as earlier builds sealed it:
    /// empty associated data.
    fn seal_v1(key: &MasterKey, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).unwrap();
        let nonce = XNonce::from(nonce);
        let mut envelope = ENVELOPE_V1_MAGIC.to_vec();
        envelope.extend_from_slice(nonce.as_slice());
        envelope.extend(key.cipher().encrypt(&nonce, plaintext).unwrap());
        envelope
    }

    /// A version-2 envelope of `plaintext` as `name` in `store`: the
    /// current layout and binding, sealed under the master key itself.
    fn seal_v2(key: &MasterKey, store: Store, name: &str, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).unwrap();
        let nonce = XNonce::from(nonce);
        let mut envelope = ENVELOPE_V2_MAGIC.to_vec();
        envelope.extend_from_slice(nonce.as_slice());
        envelope.extend(
            key.cipher()
                .encrypt(
                    &nonce,
                    Payload {
                        msg: plaintext,
                        aad: &store.associated_v2(&secret_name(name)),
                    },
                )
                .unwrap(),
        );
        envelope
    }

    /// The store key the store at `root` keeps, unwrapped under `key`.
    fn kept_key(root: &Path, key: &MasterKey, store: Store) -> StoreKey {
        let wrapped = std::fs::read(root.join(STORE_KEY_FILE)).unwrap();
        unwrap_store_key(key, store, &wrapped).unwrap()
    }

    /// Writes `bytes` as the raw file for `name` in the store at `root`.
    fn plant(root: &Path, name: &str, bytes: &[u8]) {
        Vault::open(root)
            .unwrap()
            .store_raw(&secret_name(name), bytes)
            .unwrap();
    }

    #[test]
    fn a_sealed_file_renamed_to_another_secret_fails_to_open() {
        let dir = test_dir("renamed");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        vault
            .store(&secret_name("cell-granted"), &secret(b"harmless"))
            .unwrap();
        vault
            .store(&secret_name("never-in-a-cell"), &secret(b"crown jewels"))
            .unwrap();
        // An attacker with write access to the vault, and no key, swaps
        // the two files.
        let granted = raw_bytes(&dir, &secret_name("cell-granted"));
        let jewels = raw_bytes(&dir, &secret_name("never-in-a-cell"));
        plant(&dir, "cell-granted", &jewels);
        plant(&dir, "never-in-a-cell", &granted);
        for name in ["cell-granted", "never-in-a-cell"] {
            let err = vault.fetch(&secret_name(name)).unwrap_err();
            assert!(
                matches!(&err, Error::DecryptFailed { name: failed } if failed.as_str() == name),
                "the same generic error as a tampered file: {err:?}"
            );
        }
        // Put back, both open again.
        plant(&dir, "cell-granted", &granted);
        assert_eq!(
            vault
                .fetch(&secret_name("cell-granted"))
                .unwrap()
                .as_bytes(),
            b"harmless"
        );
    }

    #[test]
    fn a_record_moved_between_stores_or_names_fails_to_open() {
        let dir = test_dir("stores");
        let other = test_dir("stores-other");
        let key = test_key();
        let redactions = SealedVault::open_in(&dir, &key, Store::REDACTIONS).unwrap();
        let schedules = SealedVault::open_in(&other, &key, Store::SCHEDULES).unwrap();
        redactions
            .store(&secret_name("red-1"), &secret(b"flagged text"))
            .unwrap();
        let record = raw_bytes(&dir, &secret_name("red-1"));
        // Into another store under the same name: refused.
        plant(&other, "red-1", &record);
        assert!(matches!(
            schedules.fetch(&secret_name("red-1")),
            Err(Error::DecryptFailed { .. })
        ));
        // The same directory opened as another store: refused, at once,
        // since its key is bound to its store.
        assert!(matches!(
            SealedVault::open(&dir, &key),
            Err(Error::StoreKeyRefused { store: "secrets" })
        ));
        // Under another name in its own store: refused.
        plant(&dir, "red-2", &record);
        assert!(matches!(
            redactions.fetch(&secret_name("red-2")),
            Err(Error::DecryptFailed { .. })
        ));
        // Where it was made, it opens.
        assert_eq!(
            redactions.fetch(&secret_name("red-1")).unwrap().as_bytes(),
            b"flagged text"
        );
        assert_eq!(
            [
                Store::SECRETS,
                Store::SCHEDULES,
                Store::ARTIFACTS,
                Store::SPACES,
                Store::REDACTIONS
            ]
            .map(Store::label),
            ["secrets", "schedules", "artifacts", "spaces", "redactions"]
        );
    }

    #[test]
    fn an_envelope_opens_only_as_it_was_sealed() {
        let key = test_key();
        let name = secret_name("tok");
        let envelope = seal_envelope(&key, Store::SECRETS, &name, b"value").unwrap();
        assert_eq!(envelope.get(..4), Some(ENVELOPE_MAGIC.as_slice()));
        assert_eq!(envelope.len(), ENVELOPE_OVERHEAD + b"value".len());
        assert_eq!(
            open_envelope(&key, Store::SECRETS, &name, &envelope)
                .unwrap()
                .as_bytes(),
            b"value"
        );
        for (store, under) in [(Store::SPACES, "tok"), (Store::SECRETS, "other")] {
            assert!(matches!(
                open_envelope(&key, store, &secret_name(under), &envelope),
                Err(Error::DecryptFailed { .. })
            ));
        }
        // A version-1 envelope: refused as old, read under its own magic
        // and empty associated data, and nothing else.
        let old = seal_v1(&key, b"from 0.3");
        assert!(matches!(
            open_envelope(&key, Store::SECRETS, &name, &old),
            Err(Error::OldEnvelope { .. })
        ));
        assert_eq!(
            open_versioned(&key, *ENVELOPE_V1_MAGIC, b"", &name, &old)
                .unwrap()
                .as_bytes(),
            b"from 0.3"
        );
        assert!(matches!(
            open_versioned(&key, *ENVELOPE_MAGIC, b"", &name, &old),
            Err(Error::NotSealed { .. })
        ));
        assert!(matches!(
            open_versioned(&key, *ENVELOPE_V1_MAGIC, b"x", &name, &old),
            Err(Error::DecryptFailed { .. })
        ));
    }

    #[test]
    fn the_associated_data_names_the_domain_the_store_and_the_name() {
        assert_eq!(
            Store::SPACES.associated(&secret_name("team-7")),
            b"hatchling vault envelope v3\0spaces\0team-7".to_vec()
        );
        assert_eq!(
            Store::SPACES.associated_v2(&secret_name("team-7")),
            b"hatchling vault envelope v2\0spaces\0team-7".to_vec()
        );
        assert_eq!(
            Store::SPACES.key_associated(),
            b"hatchling store key v1\0spaces".to_vec()
        );
    }

    #[test]
    fn an_old_envelope_is_read_only_by_the_migration() {
        let dir = test_dir("old-envelope");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        plant(&dir, "legacy", &seal_v1(&key, b"from 0.3"));
        assert!(matches!(
            vault.fetch(&secret_name("legacy")),
            Err(Error::OldEnvelope { .. })
        ));
        let report = migrate(&dir, &key, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            MigrateReport {
                encrypted: 0,
                upgraded: 1,
                skipped: 0
            }
        );
        assert_eq!(
            vault.fetch(&secret_name("legacy")).unwrap().as_bytes(),
            b"from 0.3"
        );
        assert_eq!(
            raw_bytes(&dir, &secret_name("legacy")).get(..4),
            Some(ENVELOPE_MAGIC.as_slice())
        );
    }

    #[test]
    fn an_interrupted_migration_leaves_every_secret_readable_and_resumes() {
        let dir = test_dir("interrupted");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        // As if a migration stopped halfway: one file re-sealed, one old,
        // one still plaintext.
        vault.store(&secret_name("done"), &secret(b"one")).unwrap();
        plant(&dir, "old", &seal_v1(&key, b"two"));
        plant(&dir, "plain", b"three");
        assert!(check(&dir, Store::SECRETS).is_err());
        let report = migrate(&dir, &key, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            MigrateReport {
                encrypted: 1,
                upgraded: 1,
                skipped: 1
            }
        );
        for (name, value) in [
            ("done", b"one".as_slice()),
            ("old", b"two".as_slice()),
            ("plain", b"three".as_slice()),
        ] {
            assert_eq!(vault.fetch(&secret_name(name)).unwrap().as_bytes(), value);
        }
        assert!(check(&dir, Store::SECRETS).is_ok());
        // Run again: nothing left to do.
        assert_eq!(
            migrate(&dir, &key, Store::SECRETS).unwrap(),
            MigrateReport {
                encrypted: 0,
                upgraded: 0,
                skipped: 3
            }
        );
    }

    #[test]
    fn a_store_with_anything_but_current_envelopes_is_refused_by_name() {
        let dir = test_dir("check");
        let key = test_key();
        let vault = SealedVault::open_in(&dir, &key, Store::ARTIFACTS).unwrap();
        vault
            .store(&secret_name("current"), &secret(b"ok"))
            .unwrap();
        assert!(check(&dir, Store::ARTIFACTS).is_ok());
        plant(&dir, "z-old", &seal_v1(&key, b"x"));
        plant(&dir, "a-plain", b"plaintext");
        let mut future = b"HVL9".to_vec();
        future.extend_from_slice(&[0u8; 64]);
        plant(&dir, "m-future", &future);
        // Temp files and foreign names are not the store's.
        std::fs::write(dir.join(".tmp-123"), b"partial").unwrap();
        let err = check(&dir, Store::ARTIFACTS).unwrap_err();
        assert!(
            matches!(&err, Error::UnmigratedStore { store: "artifacts", names } if names == "a-plain, m-future, z-old"),
            "{err:?}"
        );
        let shown = err.to_string();
        assert!(shown.contains("migrate-vault"), "{shown}");
        // An unknown version is never sealed over as plaintext.
        let refused = migrate(&dir, &key, Store::ARTIFACTS).unwrap_err();
        assert!(matches!(refused, Error::NotSealed { .. }), "{refused:?}");
        assert_eq!(raw_bytes(&dir, &secret_name("m-future")), future);
    }

    #[test]
    fn an_old_envelope_that_does_not_authenticate_fails_the_migration() {
        let dir = test_dir("old-tampered");
        let key = test_key();
        let mut tampered = seal_v1(&key, b"secret");
        if let Some(last) = tampered.last_mut() {
            *last ^= 1;
        }
        plant(&dir, "tampered", &tampered);
        assert!(matches!(
            migrate(&dir, &key, Store::SECRETS),
            Err(Error::DecryptFailed { .. })
        ));
        assert_eq!(raw_bytes(&dir, &secret_name("tampered")), tampered);
    }

    #[test]
    fn a_store_holds_envelopes_when_any_file_is_one_of_any_version() {
        let dir = test_dir("holds");
        let key = test_key();
        assert!(!holds_envelopes(&dir.join("absent")).unwrap());
        assert!(!dir.join("absent").exists(), "looking creates nothing");
        Vault::open(&dir).unwrap();
        assert!(!holds_envelopes(&dir).unwrap());
        plant(&dir, "plain", b"plaintext");
        assert!(!holds_envelopes(&dir).unwrap());
        for (name, bytes) in [
            ("old", seal_v1(&key, b"x")),
            ("future", b"HVL9 not yet".to_vec()),
        ] {
            let alone = test_dir(&format!("holds-{name}"));
            plant(&alone, name, &bytes);
            assert!(holds_envelopes(&alone).unwrap(), "{name}");
        }
        SealedVault::open(&dir, &key)
            .unwrap()
            .store(&secret_name("current"), &secret(b"y"))
            .unwrap();
        assert!(holds_envelopes(&dir).unwrap());
    }

    #[test]
    fn only_hvl_and_a_digit_looks_like_an_envelope() {
        assert!(looks_like_magic(b"HVL1"));
        assert!(looks_like_magic(b"HVL9"));
        for other in [b"HVLx".as_slice(), b"hvl1", b"XVL1", b"HVL", b""] {
            assert!(!looks_like_magic(other), "{other:?}");
        }
        assert_eq!(kind_of(b"HVL3...."), Kind::Current);
        assert_eq!(kind_of(b"HVL2...."), Kind::V2);
        assert_eq!(kind_of(b"HVL1...."), Kind::Old);
        assert_eq!(kind_of(b"HV"), Kind::Other);
    }

    #[test]
    fn rekey_refuses_an_old_envelope_until_it_is_migrated() {
        let dir = test_dir("rekey-old");
        let old = test_key();
        let new = test_key();
        plant(&dir, "legacy", &seal_v1(&old, b"x"));
        assert!(matches!(
            rekey(&dir, &old, &new, Store::SECRETS),
            Err(Error::OldEnvelope { .. })
        ));
    }

    #[test]
    fn empty_secret_roundtrips() {
        // The smallest valid envelope: magic + nonce + bare tag.
        let dir = test_dir("empty");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        let name = secret_name("empty");
        vault.store(&name, &secret(b"")).unwrap();
        let raw = raw_bytes(&dir, &name);
        assert_eq!(raw.len(), ENVELOPE_OVERHEAD);
        let back = vault.fetch(&name).unwrap();
        assert_eq!(back.as_bytes(), b"");
    }

    #[test]
    fn on_disk_bytes_are_a_versioned_envelope_not_plaintext() {
        let dir = test_dir("envelope-shape");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        let name = secret_name("token");
        // Distinctive plaintext: a coincidental reappearance in random
        // ciphertext is not a test failure we want to chase.
        let plaintext = b"plaintext-marker-0123456789abcdef";
        vault.store(&name, &secret(plaintext)).unwrap();
        let raw = raw_bytes(&dir, &name);
        assert_eq!(
            raw.get(..ENVELOPE_MAGIC.len()),
            Some(ENVELOPE_MAGIC.as_slice())
        );
        assert_eq!(raw.len(), ENVELOPE_OVERHEAD + plaintext.len());
        assert!(
            !raw.windows(plaintext.len()).any(|w| w == plaintext),
            "plaintext appears in the stored bytes"
        );
    }

    #[test]
    fn identical_plaintexts_seal_differently() {
        // Fresh random nonce per store: two envelopes of the same
        // plaintext must differ. Pins the nonce generation — a mutant
        // replacing it with a fixed nonce fails here.
        let dir = test_dir("fresh-nonce");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        let s = secret(b"same-value");
        vault.store(&secret_name("first"), &s).unwrap();
        vault.store(&secret_name("second"), &s).unwrap();
        let a = raw_bytes(&dir, &secret_name("first"));
        let b = raw_bytes(&dir, &secret_name("second"));
        assert_ne!(a, b, "two seals of the same plaintext are identical");
    }

    /// Writes arbitrary bytes as the secret file, bypassing the seal
    /// pipeline, then reads through the sealed vault.
    fn fetch_malformed(dir: &Path, key: &MasterKey, name: &SecretName, bytes: &[u8]) -> Error {
        // Opened first, so the store has its key before the file arrives.
        let sealed = SealedVault::open(dir, key).unwrap();
        Vault::open(dir).unwrap().store_raw(name, bytes).unwrap();
        sealed.fetch(name).unwrap_err()
    }

    #[test]
    fn short_file_is_not_sealed_never_malformed() {
        // A file shorter than the magic cannot be an envelope at all:
        // `NotSealed` (a short plaintext file), never `Malformed`.
        // Pins the length gate before the magic check.
        let dir = test_dir("short-file");
        let key = test_key();
        let err = fetch_malformed(&dir, &key, &secret_name("s"), b"AB");
        assert!(matches!(err, Error::NotSealed { .. }), "got {err:?}");
    }

    #[test]
    fn empty_file_is_not_sealed() {
        let dir = test_dir("empty-file");
        let key = test_key();
        let err = fetch_malformed(&dir, &key, &secret_name("s"), b"");
        assert!(matches!(err, Error::NotSealed { .. }), "got {err:?}");
    }

    #[test]
    fn wrong_magic_is_not_sealed() {
        // Long enough to be an envelope, wrong magic: still
        // `NotSealed`, not a decryption attempt.
        let dir = test_dir("wrong-magic");
        let key = test_key();
        let mut bytes = b"XXXX".to_vec();
        bytes.extend_from_slice(&[0u8; 100]);
        let err = fetch_malformed(&dir, &key, &secret_name("s"), &bytes);
        assert!(matches!(err, Error::NotSealed { .. }), "got {err:?}");
    }

    #[test]
    fn magic_only_is_malformed() {
        let dir = test_dir("magic-only");
        let key = test_key();
        let err = fetch_malformed(&dir, &key, &secret_name("s"), ENVELOPE_MAGIC);
        assert!(matches!(err, Error::Malformed { .. }), "got {err:?}");
    }

    #[test]
    fn truncated_nonce_is_malformed() {
        let dir = test_dir("truncated-nonce");
        let key = test_key();
        let mut bytes = ENVELOPE_MAGIC.to_vec();
        bytes.extend_from_slice(&[0u8; 10]);
        let err = fetch_malformed(&dir, &key, &secret_name("s"), &bytes);
        assert!(matches!(err, Error::Malformed { .. }), "got {err:?}");
    }

    #[test]
    fn nonce_without_tag_is_malformed() {
        let dir = test_dir("nonce-no-tag");
        let key = test_key();
        let mut bytes = ENVELOPE_MAGIC.to_vec();
        bytes.extend_from_slice(&[0u8; NONCE_LEN]);
        let err = fetch_malformed(&dir, &key, &secret_name("s"), &bytes);
        assert!(matches!(err, Error::Malformed { .. }), "got {err:?}");
    }

    #[test]
    fn short_body_is_malformed() {
        // Magic + full nonce + fewer than TAG_LEN body bytes.
        let dir = test_dir("short-body");
        let key = test_key();
        let mut bytes = ENVELOPE_MAGIC.to_vec();
        bytes.extend_from_slice(&[0u8; NONCE_LEN + 5]);
        let err = fetch_malformed(&dir, &key, &secret_name("s"), &bytes);
        assert!(matches!(err, Error::Malformed { .. }), "got {err:?}");
    }

    #[test]
    fn tampered_ciphertext_is_a_generic_authentication_failure() {
        let dir = test_dir("tampered-ct");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        let name = secret_name("s");
        vault.store(&name, &secret(b"0123456789abcdef")).unwrap();
        let mut raw = raw_bytes(&dir, &name);
        let byte = raw.get_mut(ENVELOPE_MAGIC.len() + NONCE_LEN).unwrap();
        *byte ^= 0xFF;
        Vault::open(&dir).unwrap().store_raw(&name, &raw).unwrap();
        let err = vault.fetch(&name).unwrap_err();
        assert!(matches!(err, Error::DecryptFailed { .. }), "got {err:?}");
    }

    #[test]
    fn tampered_tag_is_a_generic_authentication_failure() {
        let dir = test_dir("tampered-tag");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        let name = secret_name("s");
        vault.store(&name, &secret(b"0123456789abcdef")).unwrap();
        let mut raw = raw_bytes(&dir, &name);
        let last = raw.len() - 1;
        let byte = raw.get_mut(last).unwrap();
        *byte ^= 0xFF;
        Vault::open(&dir).unwrap().store_raw(&name, &raw).unwrap();
        let err = vault.fetch(&name).unwrap_err();
        assert!(matches!(err, Error::DecryptFailed { .. }), "got {err:?}");
    }

    #[test]
    fn wrong_key_is_indistinguishable_from_tampering() {
        // The whole point of the single generic error: no oracle leaks
        // whether the key was wrong or the bytes were tampered with.
        let dir = test_dir("wrong-key");
        let key_a = test_key();
        let key_b = test_key();
        let va = SealedVault::open(&dir, &key_a).unwrap();
        let name = secret_name("s");
        va.store(&name, &secret(b"value")).unwrap();
        // The wrong master key fails where the store's key unwraps...
        let err = SealedVault::open(&dir, &key_b).unwrap_err();
        assert!(matches!(err, Error::StoreKeyRefused { .. }), "got {err:?}");
        // ...with the same error as a changed key file.
        let mut wrapped = std::fs::read(dir.join(STORE_KEY_FILE)).unwrap();
        if let Some(byte) = wrapped.last_mut() {
            *byte ^= 1;
        }
        std::fs::write(dir.join(STORE_KEY_FILE), &wrapped).unwrap();
        let err = SealedVault::open(&dir, &key_a).unwrap_err();
        assert!(matches!(err, Error::StoreKeyRefused { .. }), "got {err:?}");
    }

    #[test]
    fn oversized_envelope_is_rejected_before_allocation() {
        // A foreign-grown file must not become an unbounded allocation:
        // the sealed read bound is MAX_SECRET_BYTES + ENVELOPE_OVERHEAD.
        let dir = test_dir("oversized");
        let key = test_key();
        Vault::open(&dir).unwrap();
        let name = secret_name("big");
        let foreign = vec![0xAAu8; MAX_SECRET_BYTES + ENVELOPE_OVERHEAD + 1];
        std::fs::write(dir.join(name.as_str()), &foreign).unwrap();
        let sealed = SealedVault::open(&dir, &key).unwrap();
        let err = sealed.fetch(&name).unwrap_err();
        assert!(
            matches!(err, Error::TooLarge { len, max }
                if len == MAX_SECRET_BYTES + ENVELOPE_OVERHEAD + 1
                   && max == MAX_SECRET_BYTES + ENVELOPE_OVERHEAD),
            "got {err:?}"
        );
    }

    #[test]
    fn max_plaintext_roundtrips() {
        // Boundary: exactly MAX_SECRET_BYTES seals and opens.
        let dir = test_dir("max-plaintext");
        let key = test_key();
        let vault = SealedVault::open(&dir, &key).unwrap();
        let name = secret_name("max");
        let big = vec![0x55u8; MAX_SECRET_BYTES];
        vault.store(&name, &secret(&big)).unwrap();
        let back = vault.fetch(&name).unwrap();
        assert_eq!(back.as_bytes(), big.as_slice());
    }

    // ---- FileKey ----

    fn key_file_in(dir: &Path) -> (FileKey, PathBuf) {
        let path = dir.join("vault.key");
        (FileKey::new(path.clone()), path)
    }

    #[test]
    fn file_key_creates_with_owner_only_discipline() {
        // `FileKey` parents are single-level: the broker's key path
        // is a sibling of the vault root, whose own parent already
        // exists. (Deeper nesting is not created; it fails closed.)
        let dir = test_dir("file-key-create");
        let key_path = dir.join("vault.key");
        let source = FileKey::new(key_path.clone());
        let key = source.load_or_create_as(real_uid()).unwrap();
        // Parent created 0700, file created 0600, exactly 32 bytes.
        let parent = key_path.parent().unwrap();
        let parent_mode = std::fs::metadata(parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(parent_mode, 0o700, "parent mode {parent_mode:o}");
        let file_mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "key file mode {file_mode:o}");
        assert_eq!(std::fs::metadata(&key_path).unwrap().len(), 32);
        // Reload returns the same key (behavioral equality).
        let dir2 = test_dir("file-key-reload-root");
        let again = source.load_or_create_as(real_uid()).unwrap();
        assert_same_key(&key, &again, &dir2);
    }

    #[test]
    fn file_key_trait_load_or_create_uses_the_real_uid() {
        // The public `KeySource` path: create then reload.
        let dir = test_dir("file-key-trait");
        let (source, path) = key_file_in(&dir);
        let key = source.load_or_create().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let again = source.load_or_create().unwrap();
        let dir2 = test_dir("file-key-trait-root");
        assert_same_key(&key, &again, &dir2);
    }

    #[test]
    fn file_key_wrong_mode_fails_closed() {
        let dir = test_dir("file-key-mode");
        let (source, path) = key_file_in(&dir);
        source.load_or_create_as(real_uid()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = source.load_or_create_as(real_uid()).unwrap_err();
        assert!(matches!(err, Error::InsecureKeyFile { .. }), "got {err:?}");
    }

    #[test]
    fn file_key_wrong_owner_fails_closed() {
        // The parent directory check fires first: a key file whose
        // parent is not owned by the loading uid is never trusted.
        let dir = test_dir("file-key-owner");
        let (source, _) = key_file_in(&dir);
        source.load_or_create_as(real_uid()).unwrap();
        let err = source
            .load_or_create_as(real_uid().wrapping_add(1))
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::InsecurePermissions { .. } | Error::InsecureKeyFile { .. }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn file_key_symlink_is_rejected() {
        let dir = test_dir("file-key-symlink");
        let (source, path) = key_file_in(&dir);
        source.load_or_create_as(real_uid()).unwrap();
        let link = dir.join("vault-link.key");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        // `read_key` directly: the parent check is bypassed on purpose
        // so this pins the symlink rejection itself, not the parent.
        let file = std::fs::File::open(&link).unwrap();
        let err = FileKey::read_key(&link, &file, real_uid()).unwrap_err();
        assert!(matches!(err, Error::InsecureKeyFile { .. }), "got {err:?}");
    }

    #[test]
    fn file_key_symlink_loop_fails_closed() {
        // `File::open` on a symlink loop fails with `FilesystemLoop`,
        // not `NotFound`: only a genuinely missing key file may be
        // created. A `load_or_create_as` that created on *any* I/O
        // error would mint a fresh key here (failing as `Io` from the
        // create leg) instead of failing the load closed — silently
        // orphaning every secret sealed under the real key.
        let dir = test_dir("file-key-loop");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let link = dir.join("vault.key");
        std::os::unix::fs::symlink(&link, &link).unwrap();
        let source = FileKey::new(link);
        let err = source.load_or_create_as(real_uid()).unwrap_err();
        assert!(matches!(err, Error::KeyLoad { .. }), "got {err:?}");
    }

    #[test]
    fn file_key_trailing_slash_fails_closed() {
        // `File::open` on "vault.key/" (trailing slash over a regular
        // file) fails — here with `IsADirectory` — not `NotFound`:
        // only a genuinely missing key file may be created.
        // `create_new` on the same path likewise fails without
        // `AlreadyExists`, so a `load_or_create_as` that created on
        // *any* I/O error surfaces the create-leg `Io` here instead of
        // the load-leg `KeyLoad`, and the test tells them apart.
        let dir = test_dir("file-key-slash");
        let (source, path) = key_file_in(&dir);
        source.load_or_create_as(real_uid()).unwrap();
        let slashed = FileKey::new(PathBuf::from(format!("{}/", path.display())));
        let err = slashed.load_or_create_as(real_uid()).unwrap_err();
        assert!(matches!(err, Error::KeyLoad { .. }), "got {err:?}");
    }

    #[test]
    fn file_key_create_missing_parent_fails_closed() {
        // `create_new` with a missing parent fails with `NotFound`,
        // not `AlreadyExists`: the lost-race branch must not run. A
        // `create_key` that treated any creation error as "already
        // exists" would try opening the missing file and report the
        // read-leg `KeyLoad` instead of the creation-leg I/O failure.
        let dir = test_dir("file-key-create-noparent");
        std::fs::create_dir_all(&dir).unwrap();
        let source = FileKey::new(dir.join("no-such-subdir").join("vault.key"));
        let err = source.create_key(real_uid()).unwrap_err();
        assert!(matches!(err, Error::Io { .. }), "got {err:?}");
    }

    #[test]
    fn file_key_create_lost_race_reads_winner() {
        // Deterministic lost race: the key file already exists when
        // `create_key` runs, so `create_new` reports `AlreadyExists`
        // and the winner's key is read instead of failing. Pins the
        // `AlreadyExists` guard in the failing direction too: without
        // it the race is reported as a creation I/O failure.
        let dir = test_dir("file-key-race");
        let (source, _) = key_file_in(&dir);
        let winner = source.load_or_create_as(real_uid()).unwrap();
        let ours = source.create_key(real_uid()).unwrap();
        let dir2 = test_dir("file-key-race-root");
        assert_same_key(&winner, &ours, &dir2);
    }

    #[test]
    fn file_key_directory_is_rejected() {
        let dir = test_dir("file-key-dir");
        std::fs::create_dir_all(&dir).unwrap();
        let sub = dir.join("not-a-key");
        std::fs::create_dir(&sub).unwrap();
        let file = std::fs::File::open(&sub).unwrap();
        let err = FileKey::read_key(&sub, &file, real_uid()).unwrap_err();
        assert!(matches!(err, Error::InsecureKeyFile { .. }), "got {err:?}");
    }

    #[test]
    fn file_key_short_key_is_rejected() {
        // A partial write (e.g. crashed creation) fails closed on the
        // next load: 16 bytes is not a key.
        let dir = test_dir("file-key-short");
        let (source, path) = key_file_in(&dir);
        source.load_or_create_as(real_uid()).unwrap();
        std::fs::write(&path, [0x11u8; 16]).unwrap();
        let err = source.load_or_create_as(real_uid()).unwrap_err();
        assert!(
            matches!(err, Error::InvalidKey { len: 16, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn file_key_long_key_is_rejected_not_truncated() {
        // 33 bytes: the bounded read detects the extra byte and
        // rejects, never silently truncates to 32. Pins the
        // `saturating_add(1)` in the read limit.
        let dir = test_dir("file-key-long");
        let (source, path) = key_file_in(&dir);
        source.load_or_create_as(real_uid()).unwrap();
        std::fs::write(&path, [0x22u8; 33]).unwrap();
        let err = source.load_or_create_as(real_uid()).unwrap_err();
        assert!(
            matches!(err, Error::InvalidKey { len: 33, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn file_key_empty_key_is_rejected() {
        let dir = test_dir("file-key-empty");
        let (source, path) = key_file_in(&dir);
        source.load_or_create_as(real_uid()).unwrap();
        std::fs::write(&path, []).unwrap();
        let err = source.load_or_create_as(real_uid()).unwrap_err();
        assert!(
            matches!(err, Error::InvalidKey { len: 0, .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn file_key_without_parent_fails() {
        // The root path has no parent directory at all.
        let source = FileKey::new(PathBuf::from("/"));
        let err = source.load_or_create_as(real_uid()).unwrap_err();
        assert!(matches!(err, Error::KeyLoad { .. }), "got {err:?}");
    }

    #[test]
    fn file_key_creation_race_reads_the_winners_key() {
        // Deterministic simulation of the `create_new` race: the file
        // already exists when `create_key` runs, so the `AlreadyExists`
        // arm must read the winner's key instead of failing. Deleting
        // that arm fails this test.
        //
        // NOTE: the directory names must be unique to this test. An
        // earlier revision shared `file-key-race` / `file-key-race-root`
        // with `file_key_create_lost_race_reads_winner`; `test_dir`
        // removes the directory on entry, so parallel runs deleted the
        // other test's directory mid-flight and failed CI with
        // `NotFound` (exit 101 on the push).
        let dir = test_dir("file-key-winner-race");
        let parent = dir.join("sub");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = parent.join("vault.key");
        let winner_bytes = [0x77u8; 32];
        std::fs::write(&path, winner_bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let source = FileKey::new(path);
        let key = source.create_key(real_uid()).unwrap();
        let dir2 = test_dir("file-key-winner-race-root");
        assert_same_key(&key, &MasterKey::from_bytes(winner_bytes), &dir2);
    }

    // ---- migrate ----

    #[test]
    fn migrate_encrypts_plaintext_in_place() {
        let dir = test_dir("migrate-basic");
        let key = test_key();
        // Legacy plaintext store: two secrets via the plain `Vault`.
        let plain = Vault::open(&dir).unwrap();
        plain
            .store(&secret_name("one"), &secret(b"first-value"))
            .unwrap();
        plain
            .store(&secret_name("two"), &secret(b"second-value"))
            .unwrap();

        let report = migrate(&dir, &key, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            MigrateReport {
                encrypted: 2,
                upgraded: 0,
                skipped: 0
            }
        );
        // Bytes on disk are envelopes now, and the sealed vault reads
        // them back.
        for name in ["one", "two"] {
            let raw = raw_bytes(&dir, &secret_name(name));
            assert_eq!(
                raw.get(..ENVELOPE_MAGIC.len()),
                Some(ENVELOPE_MAGIC.as_slice())
            );
        }
        let sealed = SealedVault::open(&dir, &key).unwrap();
        assert_eq!(
            sealed.fetch(&secret_name("one")).unwrap().as_bytes(),
            b"first-value"
        );
        assert_eq!(
            sealed.fetch(&secret_name("two")).unwrap().as_bytes(),
            b"second-value"
        );
    }

    #[test]
    fn migrate_rejects_file_past_the_sealed_bound() {
        // The migration read bound is exactly
        // `MAX_SECRET_BYTES + ENVELOPE_OVERHEAD`: a larger file is
        // foreign and fails closed with that bound reported. Pins the
        // bound against `-` (a max-size secret would be rejected) and
        // `*` (the foreign file would be sealed instead of rejected).
        let dir = test_dir("migrate-oversize");
        let key = test_key();
        let plain = Vault::open(&dir).unwrap();
        plain
            .store(&secret_name("small"), &secret(b"value"))
            .unwrap();
        // Foreign bytes just past the sealed bound, without envelope
        // magic so they cannot be mistaken for an envelope.
        let big = vec![0x41u8; MAX_SECRET_BYTES + ENVELOPE_OVERHEAD + 1];
        std::fs::write(dir.join("big"), &big).unwrap();
        let err = migrate(&dir, &key, Store::SECRETS).unwrap_err();
        assert!(
            matches!(err, Error::TooLarge { max, .. } if max == MAX_SECRET_BYTES + ENVELOPE_OVERHEAD),
            "got {err:?}"
        );
    }

    #[test]
    fn migrate_is_idempotent() {
        let dir = test_dir("migrate-idempotent");
        let key = test_key();
        let plain = Vault::open(&dir).unwrap();
        plain.store(&secret_name("one"), &secret(b"value")).unwrap();
        migrate(&dir, &key, Store::SECRETS).unwrap();
        // Second run: the sealed file authenticates under the key and
        // is verified-skipped, not re-encrypted.
        let report = migrate(&dir, &key, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            MigrateReport {
                encrypted: 0,
                upgraded: 0,
                skipped: 1
            }
        );
    }

    #[test]
    fn migrate_verifies_already_sealed_files() {
        // Magic is verified, not assumed: a magic-bearing file that
        // does not authenticate under the migration key fails closed.
        let dir = test_dir("migrate-verify");
        let key_a = test_key();
        let key_b = test_key();
        let sealed = SealedVault::open(&dir, &key_a).unwrap();
        sealed.store(&secret_name("s"), &secret(b"value")).unwrap();
        let err = migrate(&dir, &key_b, Store::SECRETS).unwrap_err();
        assert!(matches!(err, Error::StoreKeyRefused { .. }), "got {err:?}");
        // A current envelope under another store key, in a store whose key
        // opens: verified, and refused.
        let foreign = seal_envelope(&StoreKey::generate().unwrap(), Store::SECRETS, &secret_name("f"), b"x").unwrap();
        plant(&dir, "f", &foreign);
        let err = migrate(&dir, &key_a, Store::SECRETS).unwrap_err();
        assert!(matches!(err, Error::DecryptFailed { .. }), "got {err:?}");
    }

    #[test]
    fn migrate_rejects_tampered_envelopes() {
        let dir = test_dir("migrate-tampered");
        let key = test_key();
        let sealed = SealedVault::open(&dir, &key).unwrap();
        let name = secret_name("s");
        sealed.store(&name, &secret(b"value")).unwrap();
        let mut raw = raw_bytes(&dir, &name);
        let byte = raw.get_mut(ENVELOPE_MAGIC.len()).unwrap();
        *byte ^= 0xFF;
        Vault::open(&dir).unwrap().store_raw(&name, &raw).unwrap();
        let err = migrate(&dir, &key, Store::SECRETS).unwrap_err();
        assert!(matches!(err, Error::DecryptFailed { .. }), "got {err:?}");
    }

    #[test]
    fn migrate_skips_dotfiles_and_foreign_names() {
        let dir = test_dir("migrate-skip");
        let key = test_key();
        let plain = Vault::open(&dir).unwrap();
        plain
            .store(&secret_name("real"), &secret(b"value"))
            .unwrap();
        // Crashed temp file and a name outside the SecretName alphabet:
        // not secrets, counted as skipped, left untouched.
        std::fs::write(dir.join(".tmp-12"), b"partial").unwrap();
        std::fs::write(dir.join("has space"), b"foreign").unwrap();
        let report = migrate(&dir, &key, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            MigrateReport {
                encrypted: 1,
                upgraded: 0,
                skipped: 2
            }
        );
        assert_eq!(std::fs::read(dir.join(".tmp-12")).unwrap(), b"partial");
        assert_eq!(std::fs::read(dir.join("has space")).unwrap(), b"foreign");
    }

    #[test]
    fn migrate_resumes_a_partial_run() {
        // One file already sealed (previous interrupted run), one still
        // plaintext: re-running completes the migration.
        let dir = test_dir("migrate-resume");
        let key = test_key();
        SealedVault::open(&dir, &key)
            .unwrap()
            .store(&secret_name("done"), &secret(b"done-value"))
            .unwrap();
        Vault::open(&dir)
            .unwrap()
            .store(&secret_name("todo"), &secret(b"todo-value"))
            .unwrap();
        let report = migrate(&dir, &key, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            MigrateReport {
                encrypted: 1,
                upgraded: 0,
                skipped: 1
            }
        );
        let sealed = SealedVault::open(&dir, &key).unwrap();
        assert_eq!(
            sealed.fetch(&secret_name("todo")).unwrap().as_bytes(),
            b"todo-value"
        );
    }

    // ---- rekey ----

    #[test]
    fn rekey_rotates_to_the_new_key() {
        let dir = test_dir("rekey-basic");
        let old = test_key();
        let new = test_key();
        let sealed = SealedVault::open(&dir, &old).unwrap();
        sealed.store(&secret_name("a"), &secret(b"alpha")).unwrap();
        sealed.store(&secret_name("b"), &secret(b"beta")).unwrap();

        let report = rekey(&dir, &old, &new, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            RekeyReport {
                rekeyed: 2,
                skipped: 0
            }
        );
        // New key opens everything; the old key opens nothing.
        let under_new = SealedVault::open(&dir, &new).unwrap();
        assert_eq!(
            under_new.fetch(&secret_name("a")).unwrap().as_bytes(),
            b"alpha"
        );
        let err = SealedVault::open(&dir, &old).unwrap_err();
        assert!(matches!(err, Error::StoreKeyRefused { .. }), "got {err:?}");
        // The store key was replaced, not rewrapped: the old one opens
        // nothing now, whoever holds it.
        assert!(!dir.join(STORE_KEY_NEXT).exists());
    }

    #[test]
    fn rekey_is_idempotent() {
        let dir = test_dir("rekey-idempotent");
        let old = test_key();
        let new = test_key();
        SealedVault::open(&dir, &old)
            .unwrap()
            .store(&secret_name("a"), &secret(b"alpha"))
            .unwrap();
        rekey(&dir, &old, &new, Store::SECRETS).unwrap();
        let report = rekey(&dir, &old, &new, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            RekeyReport {
                rekeyed: 0,
                skipped: 1
            }
        );
    }

    #[test]
    fn rekey_resumes_after_an_interrupted_phase_two() {
        // File `a` is already under the new key (phase 2 wrote it, then
        // the rotation was interrupted); file `b` is still under the
        // old. Re-running must complete the rotation, not abort on the
        // already-rekeyed file. Deleting the new-key probe fails this
        // test.
        let dir = test_dir("rekey-resume");
        let old = test_key();
        let new = test_key();
        let vault = SealedVault::open(&dir, &old).unwrap();
        vault.store(&secret_name("a"), &secret(b"alpha")).unwrap();
        vault.store(&secret_name("b"), &secret(b"beta")).unwrap();
        // Phases 1 and half of 2 ran: the next key is kept under the new
        // master key, and `a` is sealed under it.
        let next = StoreKey::generate().unwrap();
        std::fs::write(
            dir.join(STORE_KEY_NEXT),
            wrap_store_key(&new, Store::SECRETS, &next).unwrap(),
        )
        .unwrap();
        plant(&dir, "a", &seal_envelope(&next, Store::SECRETS, &secret_name("a"), b"alpha").unwrap());
        let report = rekey(&dir, &old, &new, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            RekeyReport {
                rekeyed: 1,
                skipped: 1
            }
        );
        let under_new = SealedVault::open(&dir, &new).unwrap();
        assert_eq!(
            under_new.fetch(&secret_name("a")).unwrap().as_bytes(),
            b"alpha"
        );
        assert_eq!(
            under_new.fetch(&secret_name("b")).unwrap().as_bytes(),
            b"beta"
        );
    }

    #[test]
    fn rekey_rejects_files_under_neither_key() {
        let dir = test_dir("rekey-foreign");
        let old = test_key();
        let new = test_key();
        let third = test_key();
        SealedVault::open(&dir, &third)
            .unwrap()
            .store(&secret_name("x"), &secret(b"???"))
            .unwrap();
        // The store's key opens under neither master key.
        let err = rekey(&dir, &old, &new, Store::SECRETS).unwrap_err();
        assert!(matches!(err, Error::StoreKeyRefused { .. }), "got {err:?}");
        // A file under neither store key, in a store whose key opens:
        // refused before anything is written.
        let dir = test_dir("rekey-foreign-file");
        SealedVault::open(&dir, &old)
            .unwrap()
            .store(&secret_name("a"), &secret(b"alpha"))
            .unwrap();
        plant(&dir, "x", &seal_envelope(&StoreKey::generate().unwrap(), Store::SECRETS, &secret_name("x"), b"?").unwrap());
        let before = raw_bytes(&dir, &secret_name("a"));
        let err = rekey(&dir, &old, &new, Store::SECRETS).unwrap_err();
        assert!(matches!(err, Error::DecryptFailed { .. }), "got {err:?}");
        assert_eq!(raw_bytes(&dir, &secret_name("a")), before, "nothing was re-sealed");
    }

    /// A rotation interrupted after every file was re-sealed, before the
    /// next key became the store's: re-running finishes it.
    #[test]
    fn rekey_finishes_after_an_interrupted_phase_three() {
        let dir = test_dir("rekey-phase-three");
        let old = test_key();
        let new = test_key();
        SealedVault::open(&dir, &old)
            .unwrap()
            .store(&secret_name("a"), &secret(b"alpha"))
            .unwrap();
        let next = StoreKey::generate().unwrap();
        std::fs::write(
            dir.join(STORE_KEY_NEXT),
            wrap_store_key(&new, Store::SECRETS, &next).unwrap(),
        )
        .unwrap();
        plant(&dir, "a", &seal_envelope(&next, Store::SECRETS, &secret_name("a"), b"alpha").unwrap());
        let report = rekey(&dir, &old, &new, Store::SECRETS).unwrap();
        assert_eq!(report, RekeyReport { rekeyed: 0, skipped: 1 });
        assert!(!dir.join(STORE_KEY_NEXT).exists());
        assert_eq!(
            SealedVault::open(&dir, &new).unwrap().fetch(&secret_name("a")).unwrap().as_bytes(),
            b"alpha"
        );
        // Finished, it verifies: a file the store's key can't open fails it.
        plant(&dir, "x", &seal_envelope(&StoreKey::generate().unwrap(), Store::SECRETS, &secret_name("x"), b"?").unwrap());
        assert!(matches!(
            rekey(&dir, &old, &new, Store::SECRETS),
            Err(Error::DecryptFailed { .. })
        ));
    }

    /// A store with no key has nothing sealed under one: nothing to
    /// rotate, unless its files say otherwise.
    #[test]
    fn rekey_of_a_store_without_a_key() {
        let old = test_key();
        let new = test_key();
        let empty = test_dir("rekey-empty");
        Vault::open(&empty).unwrap();
        assert_eq!(
            rekey(&empty, &old, &new, Store::SECRETS).unwrap(),
            RekeyReport { rekeyed: 0, skipped: 0 }
        );
        assert!(!empty.join(STORE_KEY_FILE).exists(), "rotating made no key");
        let v2 = test_dir("rekey-v2");
        plant(&v2, "legacy", &seal_v2(&old, Store::SECRETS, "legacy", b"x"));
        assert!(matches!(
            rekey(&v2, &old, &new, Store::SECRETS),
            Err(Error::OldEnvelope { .. })
        ));
        let lost = test_dir("rekey-lost-key");
        SealedVault::open(&lost, &old)
            .unwrap()
            .store(&secret_name("a"), &secret(b"alpha"))
            .unwrap();
        std::fs::remove_file(lost.join(STORE_KEY_FILE)).unwrap();
        assert!(matches!(
            rekey(&lost, &old, &new, Store::SECRETS),
            Err(Error::StoreKeyMissing { store: "secrets" })
        ));
    }

    /// A store's key is made once, when the store is first opened, kept
    /// wrapped and owner-only, and used from then on; of two processes
    /// opening a new store at once, both end up with the one key.
    #[test]
    fn a_stores_key_is_made_once_and_kept() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = test_dir("store-key-once");
        let key = test_key();
        SealedVault::open(&dir, &key)
            .unwrap()
            .store(&secret_name("a"), &secret(b"alpha"))
            .unwrap();
        let wrapped = std::fs::read(dir.join(STORE_KEY_FILE)).unwrap();
        assert_eq!(wrapped.len(), WRAPPED_STORE_KEY_LEN);
        assert_eq!(wrapped.get(..4), Some(STORE_KEY_MAGIC.as_slice()));
        assert_eq!(std::fs::metadata(dir.join(STORE_KEY_FILE)).unwrap().mode() & 0o777, 0o600);
        SealedVault::open(&dir, &key).unwrap();
        assert_eq!(std::fs::read(dir.join(STORE_KEY_FILE)).unwrap(), wrapped, "kept, not remade");
        assert_eq!(
            raw_bytes(&dir, &secret_name("a")).get(..4),
            Some(ENVELOPE_MAGIC.as_slice())
        );
        let opened = open_envelope(&kept_key(&dir, &key, Store::SECRETS), Store::SECRETS, &secret_name("a"), &raw_bytes(&dir, &secret_name("a")));
        assert_eq!(opened.unwrap().as_bytes(), b"alpha", "sealed under the kept key");
        // A race: every opener of a fresh store gets the same key.
        let race = test_dir("store-key-race");
        let master = std::sync::Arc::new(test_key());
        let openers: Vec<_> = (0..8)
            .map(|n| {
                let (race, master) = (race.to_path_buf(), std::sync::Arc::clone(&master));
                std::thread::spawn(move || {
                    let name = format!("s{n}").parse::<SecretName>().unwrap();
                    SealedVault::open(&race, &master).and_then(|vault| vault.store(&name, &Secret::new(vec![1]).unwrap()))
                })
            })
            .collect();
        for opener in openers {
            opener.join().unwrap().unwrap();
        }
        let vault = SealedVault::open(&race, &master).unwrap();
        for n in 0..8 {
            assert_eq!(vault.fetch(&format!("s{n}").parse().unwrap()).unwrap().as_bytes(), [1]);
        }
        let leftovers: Vec<_> = std::fs::read_dir(&*race)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// A store's key is its own: refused anywhere else, refused when it
    /// isn't a wrapped key, and never remade over files sealed under it.
    #[test]
    fn a_stores_key_is_bound_to_its_store_and_never_remade() {
        let key = test_key();
        let spaces = test_dir("store-key-spaces");
        let schedules = test_dir("store-key-schedules");
        SealedVault::open_in(&spaces, &key, Store::SPACES).unwrap();
        Vault::open(&schedules).unwrap();
        std::fs::copy(spaces.join(STORE_KEY_FILE), schedules.join(STORE_KEY_FILE)).unwrap();
        assert!(matches!(
            SealedVault::open_in(&schedules, &key, Store::SCHEDULES),
            Err(Error::StoreKeyRefused { store: "schedules" })
        ));
        for bad in [Vec::new(), vec![0u8; WRAPPED_STORE_KEY_LEN], vec![0u8; WRAPPED_STORE_KEY_LEN + 1]] {
            std::fs::write(schedules.join(STORE_KEY_FILE), &bad).unwrap();
            assert!(matches!(
                SealedVault::open_in(&schedules, &key, Store::SCHEDULES),
                Err(Error::StoreKeyMalformed { store: "schedules" })
            ), "{} bytes", bad.len());
        }
        SealedVault::open_in(&spaces, &key, Store::SPACES)
            .unwrap()
            .store(&secret_name("team"), &secret(b"t"))
            .unwrap();
        std::fs::remove_file(spaces.join(STORE_KEY_FILE)).unwrap();
        assert!(matches!(
            SealedVault::open_in(&spaces, &key, Store::SPACES),
            Err(Error::StoreKeyMissing { store: "spaces" })
        ));
        assert!(!spaces.join(STORE_KEY_FILE).exists(), "never remade over sealed files");
        assert!(holds_envelopes(&spaces).unwrap());
        let bare = test_dir("store-key-bare");
        SealedVault::open(&bare, &key).unwrap();
        assert!(holds_envelopes(&bare).unwrap(), "a store with a key has held sealed data");
    }

    /// Version-2 envelopes, sealed under the master key, migrate into the
    /// store's key, and are refused anywhere else until they do.
    #[test]
    fn version_two_envelopes_migrate_into_the_stores_key() {
        let dir = test_dir("migrate-v2");
        let key = test_key();
        plant(&dir, "a", &seal_v2(&key, Store::SECRETS, "a", b"alpha"));
        plant(&dir, "b", &seal_v1(&key, b"beta"));
        assert!(matches!(check(&dir, Store::SECRETS), Err(Error::UnmigratedStore { .. })));
        assert!(matches!(
            SealedVault::open(&dir, &key).unwrap().fetch(&secret_name("a")),
            Err(Error::OldEnvelope { .. })
        ));
        let report = migrate(&dir, &key, Store::SECRETS).unwrap();
        assert_eq!(report, MigrateReport { encrypted: 0, upgraded: 2, skipped: 0 });
        check(&dir, Store::SECRETS).unwrap();
        let vault = SealedVault::open(&dir, &key).unwrap();
        assert_eq!(vault.fetch(&secret_name("a")).unwrap().as_bytes(), b"alpha");
        assert_eq!(vault.fetch(&secret_name("b")).unwrap().as_bytes(), b"beta");
        // A version-2 envelope bound to another store, or sealed under
        // another key, doesn't migrate in.
        let other = test_dir("migrate-v2-other");
        plant(&other, "a", &seal_v2(&key, Store::SPACES, "a", b"alpha"));
        assert!(matches!(migrate(&other, &key, Store::SECRETS), Err(Error::DecryptFailed { .. })));
        let wrong = test_dir("migrate-v2-wrong");
        plant(&wrong, "a", &seal_v2(&test_key(), Store::SECRETS, "a", b"alpha"));
        assert!(matches!(migrate(&wrong, &key, Store::SECRETS), Err(Error::DecryptFailed { .. })));
    }

    #[test]
    fn rekey_rejects_plaintext() {
        // A plaintext file is a migrate-first signal, not silently
        // skipped: the sealed vault never reads it as plaintext.
        let dir = test_dir("rekey-plaintext");
        let old = test_key();
        let new = test_key();
        Vault::open(&dir)
            .unwrap()
            .store(&secret_name("p"), &secret(b"plaintext"))
            .unwrap();
        let err = rekey(&dir, &old, &new, Store::SECRETS).unwrap_err();
        assert!(matches!(err, Error::NotSealed { .. }), "got {err:?}");
    }

    #[test]
    fn rekey_rejects_file_past_the_sealed_bound() {
        // The rekey read bound is exactly
        // `MAX_SECRET_BYTES + ENVELOPE_OVERHEAD`: a larger file is
        // foreign and fails closed with that bound reported. Pins the
        // bound against `-` (a max-size envelope would be rejected)
        // and `*` (the foreign file would be probed as an envelope
        // instead of rejected).
        let dir = test_dir("rekey-oversize");
        let old = test_key();
        let new = test_key();
        let sealed = SealedVault::open(&dir, &old).unwrap();
        sealed
            .store(&secret_name("small"), &secret(b"value"))
            .unwrap();
        let big = vec![0x41u8; MAX_SECRET_BYTES + ENVELOPE_OVERHEAD + 1];
        std::fs::write(dir.join("big"), &big).unwrap();
        let err = rekey(&dir, &old, &new, Store::SECRETS).unwrap_err();
        assert!(
            matches!(err, Error::TooLarge { max, .. } if max == MAX_SECRET_BYTES + ENVELOPE_OVERHEAD),
            "got {err:?}"
        );
    }

    #[test]
    fn rekey_skips_dotfiles() {
        let dir = test_dir("rekey-dotfile");
        let old = test_key();
        let new = test_key();
        SealedVault::open(&dir, &old)
            .unwrap()
            .store(&secret_name("a"), &secret(b"alpha"))
            .unwrap();
        std::fs::write(dir.join(".tmp-7"), b"partial").unwrap();
        let report = rekey(&dir, &old, &new, Store::SECRETS).unwrap();
        assert_eq!(
            report,
            RekeyReport {
                rekeyed: 1,
                skipped: 1
            }
        );
    }

    #[test]
    fn loading_a_key_never_creates_one() {
        let dir = test_dir("load-only");
        let path = dir.join("kek");
        assert!(matches!(
            FileKey::new(path.clone()).load(),
            Err(Error::KeyLoad { .. })
        ));
        assert!(!path.exists(), "a missing key is not created by a load");
        let created = FileKey::new(path.clone()).load_or_create().unwrap();
        let loaded = FileKey::new(path.clone()).load().unwrap();
        assert_same_key(&created, &loaded, &dir.join("vault"));
        let owner = rustix::process::getuid().as_raw();
        assert!(matches!(
            FileKey::new(path).load_as(owner + 1),
            Err(Error::InsecureKeyFile { .. })
        ));
    }
}
