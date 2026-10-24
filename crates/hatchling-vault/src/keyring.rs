//! The vault keyring: the data-encryption key, wrapped once per
//! way of unlocking it.
//!
//! # Keys
//!
//! - **The DEK** is the [`MasterKey`] every secret is sealed under. It is
//!   never written to disk unwrapped once a keyring exists; the sealed
//!   envelopes do not change, so adopting a keyring re-encrypts nothing.
//! - **A KEK** (key-encryption key) is derived per unlock factor. Each
//!   [`Wrap`] holds the DEK encrypted under one factor's KEK.
//! - **Wrapping** is XChaCha20-Poly1305 under the KEK, with a fresh
//!   random nonce and the vault id, the wrap id and the factor's kind as
//!   associated data: a wrap cannot be moved to another vault, renamed,
//!   or relabelled as another factor without failing authentication.
//!
//! # Tiers
//!
//! A vault sits at one [`Tier`], and holds wraps only for that tier's
//! factor (plus recovery wraps, which add no unlock path of their own
//! kind). Changing tier needs an [`Unlocked`] obtained through the
//! current tier, and drops every wrap of the old one: a weaker wrap left
//! behind would be a side door, a downgrade nobody chose. Enrolling a
//! tier also moves the vault to a new DEK (`rotate.rs`), so a copy of the
//! keyring from before opens nothing once the stores are re-sealed.
//!
//! # Format
//!
//! Format 2 gives each wrap a label and the time it was added,
//! for display only (`manage.rs`). Format 3 adds the retiring DEK of an
//! unfinished rotation (`rotate.rs`). This build reads formats 1 and 2
//! too, and writes format 3 the next time it saves. An older build
//! refuses a newer keyring rather than drop what it can't read when it
//! saves.
//!
//! # Honest threat model
//!
//! The [`Tier::File`] KEK lives in a key file, exactly the protection the
//! vault had before keyrings: useful against the at-rest class (backups,
//! images) when the KEK file is kept apart, nothing against a live reader
//! of both files. The stronger tiers are the point of the keyring.

use std::cell::Cell;
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::Error;
use crate::seal::MasterKey;
use crate::acting::{give, owner_in};
use crate::vault::ensure_owner_only_dir;

/// The keyring format this build writes.
const FORMAT: u32 = 3;
/// The keyring formats this build reads: format 1, which has no labels or
/// dates; format 2, which has no retiring DEK; and its own.
const READS: [u32; 3] = [1, 2, FORMAT];
/// Associated-data prefix for a wrap: hatchling vault keyring, v1.
pub(crate) const WRAP_DOMAIN: &[u8; 4] = b"HVK1";
/// `XChaCha20` nonce length.
pub(crate) const NONCE_LEN: usize = 24;
/// Key length, for the DEK and every KEK.
const KEY_LEN: usize = 32;
/// Mode of the keyring file: owner-only.
const KEYRING_MODE: u32 = 0o600;
/// The most a keyring file may hold; a bigger one is not a keyring.
const MAX_KEYRING_BYTES: u64 = 1 << 20;
/// How long a save waits for another process's edit of the keyring.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// A key-encryption key: 32 bytes, zeroized on drop, never cloned.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Kek([u8; KEY_LEN]);

impl Kek {
    /// A KEK from 32 bytes of key material the caller derived.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// The file tier's KEK: the 32 bytes of a key file, loaded with the
    /// same discipline as the vault's own key file.
    #[must_use]
    pub const fn from_file_key(key: &MasterKey) -> Self {
        Self(*key.expose())
    }

    pub(crate) fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(&self.0.into())
    }
}

impl std::fmt::Debug for Kek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Kek([redacted])")
    }
}

/// How strongly a vault is protected, weakest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// A KEK in a key file.
    File,
    /// An authenticator app's one-time codes (RFC 6238). Weaker by
    /// construction: the seed is stored in the keyring (`totp.rs`).
    Totp,
    /// A FIDO2 security key: the KEK needs the key's `hmac-secret`
    /// output, which only the key can compute.
    HardwareKey,
}

/// One way of unlocking the DEK, with what that factor needs to derive
/// its KEK again. Nothing here unlocks anything by itself, except the TOTP
/// seed, which its `Debug` never shows.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Factor {
    /// The file tier's KEK, held in a key file.
    File,
    /// A FIDO2 credential: its KEK is derived from the key's
    /// `hmac-secret` output for `salt` (`fido2.rs`).
    Fido2 {
        /// The credential id, hex.
        credential: String,
        /// The `hmac-secret` salt, hex: random per credential.
        salt: String,
        /// Whether the key asks for its PIN each time ([`crate::KeyCheck`]):
        /// its KEK comes from its verified-user answer. Bound into the
        /// wrap's associated data, so it can't be turned off in the file.
        #[serde(default, skip_serializing_if = "is_false")]
        verified: bool,
    },
    /// An authenticator app's seed: its KEK is Argon2id of the seed,
    /// which is stored here (`totp.rs`).
    Totp {
        /// The RFC 6238 seed, hex. Stored: this is why the tier is weaker.
        /// Wiped when dropped.
        #[serde(with = "secret_text")]
        seed: Zeroizing<String>,
        /// The Argon2id salt, hex.
        salt: String,
        /// The last time step a code was accepted for: no code is
        /// accepted twice.
        last_step: u64,
    },
    /// A backup code (`backup.rs`): it recovers the vault, whatever its
    /// tier, and is burned when used. Only its verifier is stored.
    BackupCode {
        /// HKDF of the code, to recognise it: the code itself is not here.
        verifier: String,
        /// The HKDF salt, hex.
        salt: String,
    },
}

/// Whether `value` is false: a field left out of the keyring's JSON.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// Serde for a secret text field: the same JSON string, held so it is
/// wiped when dropped.
mod secret_text {
    use serde::{Deserialize as _, Deserializer, Serializer};
    use zeroize::Zeroizing;

    pub(super) fn serialize<S: Serializer>(
        text: &Zeroizing<String>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(text)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Zeroizing<String>, D::Error> {
        String::deserialize(deserializer).map(Zeroizing::new)
    }
}

impl std::fmt::Debug for Factor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File => f.write_str("File"),
            Self::Fido2 {
                credential,
                salt,
                verified,
            } => f
                .debug_struct("Fido2")
                .field("credential", credential)
                .field("salt", salt)
                .field("verified", verified)
                .finish(),
            Self::Totp {
                salt, last_step, ..
            } => f
                .debug_struct("Totp")
                .field("seed", &"[redacted]")
                .field("salt", salt)
                .field("last_step", last_step)
                .finish(),
            Self::BackupCode { verifier, salt } => f
                .debug_struct("BackupCode")
                .field("verifier", verifier)
                .field("salt", salt)
                .finish(),
        }
    }
}

impl Factor {
    /// The tier this factor unlocks, or `None` for a backup code, which
    /// recovers the vault rather than being a tier of its own.
    #[must_use]
    pub const fn tier(&self) -> Option<Tier> {
        match self {
            Self::File => Some(Tier::File),
            Self::Fido2 { .. } => Some(Tier::HardwareKey),
            Self::Totp { .. } => Some(Tier::Totp),
            Self::BackupCode { .. } => None,
        }
    }

    /// The factor's kind, bound into its wrap's associated data.
    const fn kind(&self) -> &'static [u8] {
        match self {
            Self::File => b"file",
            Self::Fido2 {
                verified: false, ..
            } => b"fido2",
            Self::Fido2 { verified: true, .. } => b"fido2-pin",
            Self::Totp { .. } => b"totp",
            Self::BackupCode { .. } => b"backup",
        }
    }
}

/// The DEK, wrapped under one factor's KEK.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wrap {
    /// Unique within the keyring.
    pub id: String,
    /// The factor that unlocks it.
    pub factor: Factor,
    /// The user's name for it: display-only, never compared,
    /// and empty when it has none.
    #[serde(default)]
    label: String,
    /// When it was added, in Unix seconds ; `None` for a wrap
    /// from a keyring older than dates.
    #[serde(default)]
    added: Option<u64>,
    /// The nonce, hex.
    nonce: String,
    /// The wrapped DEK with its tag, hex.
    wrapped: String,
}

impl Wrap {
    /// The user's name for it: display-only, empty when it has none.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// When it was added, in Unix seconds, if the keyring knows.
    #[must_use]
    pub const fn added(&self) -> Option<u64> {
        self.added
    }

    /// The same wrap, named `label`.
    pub(crate) fn set_label(&mut self, label: &str) {
        label.clone_into(&mut self.label);
    }
}

/// The keyring as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    format: u32,
    /// Random, per vault: salts every derivation and binds every wrap.
    vault_id: String,
    tier: Tier,
    wraps: Vec<Wrap>,
    /// The DEK being retired, while a rotation is unfinished
    /// (`rotate.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retiring: Option<Retiring>,
}

/// The previous DEK during a rotation, wrapped under a key derived from
/// the new DEK (`rotate.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Retiring {
    /// The nonce, hex.
    pub(crate) nonce: String,
    /// The wrapped DEK with its tag, hex.
    pub(crate) wrapped: String,
}

/// The DEK, unwrapped, with how it was unlocked.
///
/// Only an unlock produces one, and every change to the keyring takes
/// one: holding an `Unlocked` is the proof that the current factor was
/// presented. The DEK inside is zeroized when this is dropped.
#[derive(Debug)]
pub struct Unlocked {
    key: MasterKey,
    via: Tier,
    /// The vault it came from: a proof from one vault changes no other.
    vault_id: String,
    /// Unlocked with a backup code: it may restore the vault's protection,
    /// never lower it.
    recovery: bool,
    /// The wrap id of the security key that opened it, so that a
    /// key can never authorize its own removal. `None` for every other way
    /// of unlocking.
    opened_by: Option<String>,
}

impl Unlocked {
    /// The DEK of the vault `vault_id`, unlocked through `via`. Only the
    /// unlock functions call this.
    pub(crate) const fn new(key: MasterKey, via: Tier, vault_id: String) -> Self {
        Self {
            key,
            via,
            vault_id,
            recovery: false,
            opened_by: None,
        }
    }

    /// The same unlock, made by the security key whose wrap is `wrap`.
    pub(crate) fn through(mut self, wrap: &str) -> Self {
        self.opened_by = Some(wrap.to_owned());
        self
    }

    /// The DEK of the vault `vault_id` at tier `via`, recovered with a
    /// backup code.
    pub(crate) const fn recovered(key: MasterKey, via: Tier, vault_id: String) -> Self {
        Self {
            key,
            via,
            vault_id,
            recovery: true,
            opened_by: None,
        }
    }

    /// Whether this came from a backup code.
    #[must_use]
    pub const fn is_recovery(&self) -> bool {
        self.recovery
    }

    /// The DEK.
    #[must_use]
    pub const fn key(&self) -> &MasterKey {
        &self.key
    }

    /// The tier it was unlocked through.
    #[must_use]
    pub const fn via(&self) -> Tier {
        self.via
    }

    /// The wrap id of the security key that opened it, if one did.
    #[must_use]
    pub fn opened_by(&self) -> Option<&str> {
        self.opened_by.as_deref()
    }

    /// The DEK, for its single owner (the broker) to hold.
    #[must_use]
    pub fn into_key(self) -> MasterKey {
        self.key
    }
}

/// Lower-case hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    // Sized up front: a growing buffer would free copies of what it held.
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// Bytes from lower-case hex, or `None` if it is not.
pub(crate) fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    // Sized up front: a growing buffer would free copies of what it held.
    let mut out = Vec::with_capacity(text.len() / 2);
    for at in (0..text.len()).step_by(2) {
        out.push(
            text.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())?,
        );
    }
    Some(out)
}

/// Unix seconds now, for the time a wrap is added.
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `N` random bytes from the OS.
pub(crate) fn random<const N: usize>() -> Result<[u8; N], Error> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|source| Error::Random { source })?;
    Ok(bytes)
}

/// A keyring file and what it holds.
#[derive(Debug)]
pub struct Keyring {
    path: PathBuf,
    stored: Stored,
    /// The SHA-256 of the file as this keyring last read or wrote it, or
    /// `None` before it exists. A save goes ahead only while the file still
    /// matches, under the edit lock: no edit rests on a stale read, so a
    /// spent code or step can't be written back.
    seen: Cell<Option<[u8; 32]>>,
}

impl Keyring {
    /// Creates a keyring at `path` for `dek`, at the file tier, with one
    /// wrap under `kek`. Refuses to overwrite an existing keyring.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyringExists`] if `path` exists, or an I/O or
    /// randomness error.
    pub fn create(path: &Path, dek: &MasterKey, kek: &Kek) -> Result<Self, Error> {
        Self::create_as(path, dek, kek, owner_of(path))
    }

    fn create_as(path: &Path, dek: &MasterKey, kek: &Kek, owner: u32) -> Result<Self, Error> {
        if std::fs::symlink_metadata(path).is_ok() {
            return Err(Error::KeyringExists {
                path: path.to_path_buf(),
            });
        }
        let vault_id = hex(&random::<16>()?);
        let mut keyring = Self {
            path: path.to_path_buf(),
            stored: Stored {
                format: FORMAT,
                vault_id,
                tier: Tier::File,
                wraps: Vec::new(),
                retiring: None,
            },
            seen: Cell::new(None),
        };
        let wrap = keyring.wrap(dek, kek, Factor::File)?;
        keyring.stored.wraps.push(wrap);
        keyring.save_as(owner)?;
        Ok(keyring)
    }

    /// Loads the keyring at `path`: a regular file (never a symlink),
    /// mode `0600`, owned by the broker, in a format this build reads. A
    /// format 1 keyring is written as format 2 the next time it is saved.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyringLoad`], [`Error::InsecureKeyFile`] or
    /// [`Error::KeyringFormat`].
    pub fn load(path: &Path) -> Result<Self, Error> {
        Self::load_as(path, owner_of(path))
    }

    fn load_as(path: &Path, owner: u32) -> Result<Self, Error> {
        let load = |source| Error::KeyringLoad {
            path: path.to_path_buf(),
            source,
        };
        let metadata = std::fs::symlink_metadata(path).map_err(load)?;
        if !metadata.file_type().is_file()
            || metadata.mode() & 0o777 != KEYRING_MODE
            || metadata.uid() != owner
        {
            return Err(Error::InsecureKeyFile {
                path: path.to_path_buf(),
            });
        }
        // Sized to the file, so the read never grows (and frees) a copy of
        // what it holds; wiped when dropped, as it holds a TOTP seed.
        let size = usize::try_from(metadata.len().min(MAX_KEYRING_BYTES)).unwrap_or(0);
        let mut text = Zeroizing::new(String::with_capacity(size + 1));
        std::fs::File::open(path)
            .map_err(load)?
            .take(MAX_KEYRING_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(load)?;
        let format = |detail: &str| Error::KeyringFormat {
            path: path.to_path_buf(),
            detail: detail.to_owned(),
        };
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_KEYRING_BYTES {
            return Err(format("larger than any keyring"));
        }
        let mut stored: Stored =
            serde_json::from_str(&text).map_err(|_| format("not a keyring"))?;
        if !READS.contains(&stored.format) {
            return Err(format("a keyring format this build does not read"));
        }
        stored.format = FORMAT;
        if unhex(&stored.vault_id).map(|id| id.len()) != Some(16) {
            return Err(format("no vault id"));
        }
        let seen = Cell::new(Some(digest(text.as_bytes())));
        Ok(Self {
            path: path.to_path_buf(),
            stored,
            seen,
        })
    }

    /// The tier this vault is at.
    #[must_use]
    pub const fn tier(&self) -> Tier {
        self.stored.tier
    }

    /// The wraps, for status display: what they are, never what they hold.
    #[must_use]
    pub fn wraps(&self) -> &[Wrap] {
        &self.stored.wraps
    }

    /// Unlocks the file tier with its KEK.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnlockFailed`] if the vault is not at the file
    /// tier or no wrap opens under `kek`.
    pub fn unlock_file(&self, kek: &Kek) -> Result<Unlocked, Error> {
        if self.stored.tier != Tier::File {
            return Err(Error::UnlockFailed);
        }
        self.stored
            .wraps
            .iter()
            .filter(|wrap| wrap.factor == Factor::File)
            .find_map(|wrap| self.unwrap(wrap, kek).ok())
            .map(|key| Unlocked::new(key, Tier::File, self.stored.vault_id.clone()))
            .ok_or(Error::UnlockFailed)
    }

    /// The associated data binding a wrap to this vault, its id and its
    /// factor's kind.
    fn aad(&self, id: &str, factor: &Factor) -> Vec<u8> {
        [
            WRAP_DOMAIN.as_slice(),
            self.stored.vault_id.as_bytes(),
            b"/",
            id.as_bytes(),
            b"/",
            factor.kind(),
        ]
        .concat()
    }

    /// The DEK wrapped under `kek` for `factor`, with a fresh id.
    pub(crate) fn wrap(&self, dek: &MasterKey, kek: &Kek, factor: Factor) -> Result<Wrap, Error> {
        self.wrap_as(hex(&random::<8>()?), dek, kek, factor)
    }

    /// `wrap` again, holding `dek` under `kek`: the same id, factor, name
    /// and date, with a fresh nonce.
    pub(crate) fn rewrap(&self, wrap: &Wrap, dek: &MasterKey, kek: &Kek) -> Result<Wrap, Error> {
        let mut fresh = self.wrap_as(wrap.id.clone(), dek, kek, wrap.factor.clone())?;
        fresh.label.clone_from(&wrap.label);
        fresh.added = wrap.added;
        Ok(fresh)
    }

    /// The DEK wrapped under `kek` for `factor`, as the wrap `id`.
    fn wrap_as(
        &self,
        id: String,
        dek: &MasterKey,
        kek: &Kek,
        factor: Factor,
    ) -> Result<Wrap, Error> {
        let nonce = random::<NONCE_LEN>()?;
        let aad = self.aad(&id, &factor);
        let wrapped = kek
            .cipher()
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: dek.expose(),
                    aad: &aad,
                },
            )
            .map_err(|source| Error::SealFailed { source })?;
        Ok(Wrap {
            id,
            factor,
            label: String::new(),
            added: Some(now()),
            nonce: hex(&nonce),
            wrapped: hex(&wrapped),
        })
    }

    /// The DEK from one wrap, if `kek` is that wrap's KEK and nothing
    /// about the wrap was changed.
    pub(crate) fn unwrap(&self, wrap: &Wrap, kek: &Kek) -> Result<MasterKey, Error> {
        let nonce = unhex(&wrap.nonce)
            .filter(|nonce| nonce.len() == NONCE_LEN)
            .ok_or(Error::UnlockFailed)?;
        let wrapped = unhex(&wrap.wrapped).ok_or(Error::UnlockFailed)?;
        let aad = self.aad(&wrap.id, &wrap.factor);
        let mut plain = kek
            .cipher()
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &wrapped,
                    aad: &aad,
                },
            )
            .map_err(|_| Error::UnlockFailed)?;
        let dek = <[u8; KEY_LEN]>::try_from(plain.as_slice()).map_err(|_| Error::UnlockFailed);
        plain.zeroize();
        dek.map(MasterKey::from_bytes)
    }

    /// Puts the vault at the file tier under `kek`: the DEK is wrapped
    /// again and every other wrap dropped. From the file tier this
    /// rotates the KEK; from a stronger tier it is a downgrade, made
    /// only with that tier's own unlock. Nothing in the vault is
    /// re-encrypted; that is what the DEK/KEK split buys.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`] unless `unlocked` came from this
    /// vault through its current tier, or an I/O or randomness error;
    /// the keyring is then unchanged.
    pub fn use_file_tier(&mut self, unlocked: &Unlocked, kek: &Kek) -> Result<(), Error> {
        self.authorize(unlocked, Tier::File)?;
        let wrap = self.wrap(unlocked.key(), kek, Factor::File)?;
        self.set_tier(Tier::File, vec![wrap])
    }

    /// Whether `unlocked` may move this keyring to tier `to`: it must
    /// come from this vault, through its current tier. A tier is never
    /// changed, up or down, without the current tier's own unlock; a
    /// backup code's unlock may restore the tier or raise it, never lower
    /// it.
    pub(crate) fn authorize(&self, unlocked: &Unlocked, to: Tier) -> Result<(), Error> {
        if unlocked.vault_id != self.stored.vault_id {
            return Err(Error::TierChange {
                reason: "the unlock came from another vault",
            });
        }
        if unlocked.via != self.stored.tier {
            return Err(Error::TierChange {
                reason: "changing the vault's protection needs its current tier's unlock",
            });
        }
        if unlocked.recovery && to < self.stored.tier {
            return Err(Error::TierChange {
                reason: "a backup code can restore the vault's protection, never lower it",
            });
        }
        Ok(())
    }

    /// The vault id, which salts every derivation.
    pub(crate) fn vault_id(&self) -> &str {
        &self.stored.vault_id
    }

    /// Puts the keyring at `tier` with `wraps` as that tier's only wraps,
    /// keeping the backup codes (they wrap the same DEK), then saves it.
    pub(crate) fn set_tier(&mut self, tier: Tier, mut wraps: Vec<Wrap>) -> Result<(), Error> {
        wraps.extend(
            self.stored
                .wraps
                .iter()
                .filter(|wrap| wrap.factor.tier().is_none())
                .cloned(),
        );
        let retiring = self.stored.retiring.clone();
        self.commit(tier, wraps, retiring)
    }

    /// Replaces every wrap, keeping the tier, then saves the keyring. If
    /// the save fails, nothing changed, here or on disk.
    pub(crate) fn store_wraps(&mut self, wraps: Vec<Wrap>) -> Result<(), Error> {
        let retiring = self.stored.retiring.clone();
        self.commit(self.stored.tier, wraps, retiring)
    }

    /// The retiring DEK, while a rotation is unfinished.
    pub(crate) const fn retiring(&self) -> Option<&Retiring> {
        self.stored.retiring.as_ref()
    }

    /// Puts the keyring at `tier` with exactly `wraps` and `retiring`, in
    /// one atomic write. If the write fails, the keyring, on disk and
    /// here, is unchanged.
    pub(crate) fn commit(
        &mut self,
        tier: Tier,
        wraps: Vec<Wrap>,
        retiring: Option<Retiring>,
    ) -> Result<(), Error> {
        let next = Stored {
            tier,
            wraps,
            retiring,
            ..self.stored.clone()
        };
        let current = std::mem::replace(&mut self.stored, next);
        let saved = self.save();
        if saved.is_err() {
            self.stored = current;
        }
        saved
    }

    fn save(&self) -> Result<(), Error> {
        self.save_as(owner_of(&self.path))
    }

    fn save_as(&self, owner: u32) -> Result<(), Error> {
        self.save_waiting(owner, LOCK_WAIT)
    }

    /// Writes the keyring atomically: a `0600` temp file beside it,
    /// fsynced, renamed over it, and the directory fsynced. The
    /// directory must be `0700` and owned by the broker. The write holds
    /// the keyring's edit lock (waiting up to `wait` for another's), and
    /// goes ahead only if the file is still what this keyring last saw.
    fn save_waiting(&self, owner: u32, wait: Duration) -> Result<(), Error> {
        let parent = self.path.parent().ok_or_else(|| Error::KeyringLoad {
            path: self.path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "keyring path has no parent directory",
            ),
        })?;
        ensure_owner_only_dir(parent, owner)?;
        let _lock = EditLock::take(&self.path, owner, wait)?;
        let on_disk = digest_file(&self.path).map_err(|source| Error::KeyringLoad {
            path: self.path.clone(),
            source,
        })?;
        if on_disk != self.seen.get() {
            return Err(Error::KeyringChanged {
                path: self.path.clone(),
            });
        }
        // One buffer as large as any keyring, wiped when dropped: the text
        // holds a TOTP seed, and a growing buffer would free copies of it.
        let mut text = Zeroizing::new(Vec::with_capacity(
            usize::try_from(MAX_KEYRING_BYTES).unwrap_or(0),
        ));
        serde_json::to_writer_pretty(&mut *text, &self.stored).map_err(|source| Error::Io {
            source: std::io::Error::other(source),
        })?;
        let temp = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        write_atomically(&temp, &self.path, parent, &text, owner)
            .map_err(|source| Error::Io { source })?;
        self.seen.set(Some(digest(&text)));
        Ok(())
    }
}

/// SHA-256 of a keyring file's bytes.
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// SHA-256 of the file at `path`, or `None` if there is none. Read a
/// small block at a time through a buffer wiped after: the file holds a
/// TOTP seed, and a whole copy of it would be freed unwiped.
fn digest_file(path: &Path) -> std::io::Result<Option<[u8; 32]>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let mut hasher = Sha256::new();
    let mut block = Zeroizing::new([0u8; 4096]);
    loop {
        let read = file.read(block.as_mut_slice())?;
        if read == 0 {
            break;
        }
        hasher.update(block.get(..read).unwrap_or_default());
    }
    Ok(Some(hasher.finalize().into()))
}

/// The keyring's edit lock, held while a save checks and writes it: an
/// exclusive `flock` on `<keyring>.lock`, owner-only, beside it, never
/// removed. Released when dropped.
struct EditLock(std::fs::File);

impl EditLock {
    /// The lock for the keyring at `keyring`, waiting up to `wait` while
    /// another process holds it.
    fn take(keyring: &Path, owner: u32, wait: Duration) -> Result<Self, Error> {
        use rustix::fs::{FlockOperation, Mode, OFlags};
        let path = lock_path(keyring);
        let failed = |source: std::io::Error| Error::KeyringLoad {
            path: path.clone(),
            source,
        };
        let fd = rustix::fs::open(
            &path,
            OFlags::RDWR | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(KEYRING_MODE),
        )
        .map_err(|source| failed(source.into()))?;
        let file = std::fs::File::from(fd);
        // A lock root made for the user it acts for is theirs.
        let made = file.metadata().map_err(failed)?.uid();
        if made == 0 && owner != 0 && rustix::process::geteuid().as_raw() == 0 {
            give(&file, owner).map_err(failed)?;
        }
        let metadata = file.metadata().map_err(failed)?;
        if !metadata.file_type().is_file() || metadata.uid() != owner {
            return Err(Error::InsecureKeyFile { path });
        }
        let deadline = Instant::now() + wait;
        loop {
            match rustix::fs::flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Self(file)),
                Err(rustix::io::Errno::WOULDBLOCK) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    return Err(Error::KeyringBusy {
                        path: keyring.to_path_buf(),
                    });
                }
                Err(source) => return Err(failed(source.into())),
            }
        }
    }
}

impl Drop for EditLock {
    fn drop(&mut self) {
        // Closing the file lets the lock go too; this says so first.
        let _ = rustix::fs::flock(&self.0, rustix::fs::FlockOperation::Unlock);
    }
}

/// Who the keyring at `keyring` is, to this process: the owner of its
/// directory ([`owner_in`]).
fn owner_of(keyring: &Path) -> u32 {
    keyring
        .parent()
        .map_or_else(|| rustix::process::getuid().as_raw(), owner_in)
}

/// Where the keyring at `keyring` keeps its edit lock.
fn lock_path(keyring: &Path) -> PathBuf {
    let mut name = keyring.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// `bytes` to `target` through `temp`: written `0600`, `owner`'s,
/// fsynced, renamed, and the directory fsynced so the rename is durable.
/// If anything fails once `temp` exists, it is removed.
fn write_atomically(
    temp: &Path,
    target: &Path,
    dir: &Path,
    bytes: &[u8],
    owner: u32,
) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(KEYRING_MODE)
        .open(temp)?;
    let written = give(&file, owner)
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(temp, target));
    if written.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    written?;
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::test_dir;
    use crate::{SealedVault, Secret, SecretName};

    #[test]
    fn only_a_set_flag_is_written_to_the_file() {
        assert!(is_false(&false));
        assert!(!is_false(&true));
    }

    fn fresh_kek() -> Kek {
        Kek::from_bytes(random::<KEY_LEN>().unwrap())
    }

    /// This process's own uid.
    fn own() -> u32 {
        rustix::process::getuid().as_raw()
    }

    /// Root acts for one user, in the directories it names: there a store,
    /// a key file and a keyring are that user's, and what root makes there
    /// is given to them. Anywhere else, and for anyone but root, nothing
    /// changes. (The only test in this crate that acts for a user: it
    /// can, once per process.)
    #[test]
    fn root_acts_for_one_user_in_the_directories_it_names() {
        use crate::{FileKey, KeySource as _, Vault, act_for};
        use std::os::unix::fs::MetadataExt as _;
        let dir = test_dir("acting");
        let (vault_dir, keys, elsewhere) =
            (dir.join("vault"), dir.join("vault.key"), dir.join("other"));
        assert_eq!(owner_in(&vault_dir), own());
        assert_eq!(owner_of(&keys.join("keyring")), own());
        if own() != 0 {
            assert!(act_for(4242, &[vault_dir.as_path()]).is_err());
            assert_eq!(owner_in(&vault_dir), own());
            return;
        }
        assert!(
            act_for(0, &[vault_dir.as_path()]).is_err(),
            "for root itself"
        );
        act_for(4242, &[vault_dir.as_path(), keys.as_path()]).unwrap();
        assert!(act_for(4243, &[vault_dir.as_path()]).is_err(), "once only");
        assert_eq!(owner_in(&vault_dir), 4242);
        assert_eq!(owner_in(&elsewhere), 0);
        assert_eq!(owner_of(&keys.join("keyring")), 4242);
        let uid = |path: &Path| std::fs::symlink_metadata(path).unwrap().uid();
        // A store made there is the user's, and so is what's written in it.
        let name: SecretName = "token".parse().unwrap();
        Vault::open(&vault_dir)
            .unwrap()
            .store(&name, &Secret::new(b"t".to_vec()).unwrap())
            .unwrap();
        assert_eq!(uid(&vault_dir), 4242);
        assert_eq!(uid(&vault_dir.join("token")), 4242);
        // A key made there is the user's, and loads again as theirs.
        let key = FileKey::new(keys.join("key"));
        key.load_or_create().unwrap();
        assert_eq!(uid(&keys), 4242);
        assert_eq!(uid(&keys.join("key")), 4242);
        key.load().unwrap();
        // A keyring made there is the user's, its lock too, and it loads.
        let path = keys.join("keyring");
        Keyring::create(&path, &MasterKey::generate().unwrap(), &fresh_kek()).unwrap();
        assert_eq!(uid(&path), 4242);
        assert_eq!(uid(&lock_path(&path)), 4242);
        Keyring::load(&path).unwrap();
        // Elsewhere, root's own, as before.
        Vault::open(&elsewhere).unwrap();
        assert_eq!(uid(&elsewhere), 0);
    }

    /// `MasterKey` has no equality: two keys are the same if a secret
    /// sealed under one opens under the other.
    fn same_key(a: &MasterKey, b: &MasterKey, root: &Path) -> bool {
        let name: SecretName = "probe".parse().unwrap();
        SealedVault::open(root, a)
            .unwrap()
            .store(&name, &Secret::new(b"probe".to_vec()).unwrap())
            .unwrap();
        SealedVault::open(root, b)
            .and_then(|vault| vault.fetch(&name))
            .is_ok()
    }

    #[test]
    fn a_file_tier_keyring_unlocks_the_same_dek_and_never_holds_it_in_the_clear() {
        let dir = test_dir("keyring-file");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let path = dir.join("keys").join("keyring");
        let keyring = Keyring::create(&path, &dek, &kek).unwrap();
        assert_eq!(keyring.tier(), Tier::File);
        assert_eq!(keyring.wraps().len(), 1);
        let reloaded = Keyring::load(&path).unwrap();
        let unlocked = reloaded.unlock_file(&kek).unwrap();
        assert_eq!(unlocked.via(), Tier::File);
        assert!(same_key(&dek, unlocked.key(), &dir.join("vault")));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains(&hex(dek.expose())),
            "the DEK is never on disk unwrapped"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(same_key(&dek, &unlocked.into_key(), &dir.join("vault2")));
    }

    #[test]
    fn the_keyring_building_blocks_each_do_their_one_job() {
        // random: fresh bytes of the length asked for.
        let (first_bytes, second_bytes): ([u8; 16], [u8; 16]) =
            (random().unwrap(), random().unwrap());
        assert_ne!(first_bytes, second_bytes);
        assert_eq!(random::<0>().unwrap(), [0u8; 0]);
        let dir = test_dir("keyring-blocks");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let keys = dir.join("keys");
        let owner = rustix::process::getuid().as_raw();
        let keyring = Keyring::create_as(&keys.join("keyring"), &dek, &kek, owner).unwrap();
        // wrap: a fresh id and nonce each time, opening under its KEK only.
        let (one, two) = (
            keyring.wrap(&dek, &kek, Factor::File).unwrap(),
            keyring.wrap(&dek, &kek, Factor::File).unwrap(),
        );
        assert!(one.id != two.id && one.nonce != two.nonce && one.wrapped != two.wrapped);
        assert!(same_key(
            &dek,
            &keyring.unwrap(&one, &kek).unwrap(),
            &dir.join("vault")
        ));
        assert!(keyring.unwrap(&one, &fresh_kek()).is_err());
        // create_as and save_as: only into a directory the owner holds.
        assert!(matches!(
            Keyring::create_as(&keys.join("other"), &dek, &kek, owner + 1),
            Err(Error::InsecurePermissions { .. })
        ));
        assert!(matches!(
            keyring.save_as(owner + 1),
            Err(Error::InsecurePermissions { .. })
        ));
        keyring.save_as(owner).unwrap();
        // write_atomically: 0600, and never over a temp file already there.
        let (target, temp) = (keys.join("atomic"), keys.join("atomic.tmp"));
        write_atomically(&temp, &target, &keys, b"bytes", own()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"bytes");
        assert_eq!(std::fs::metadata(&target).unwrap().mode() & 0o777, 0o600);
        assert!(!temp.exists());
        std::fs::write(&temp, b"stale").unwrap();
        assert!(write_atomically(&temp, &target, &keys, b"new", own()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"bytes");
    }

    #[test]
    fn a_wrong_kek_or_any_changed_byte_fails_one_generic_way() {
        let dir = test_dir("keyring-tamper");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let path = dir.join("keys").join("keyring");
        let keyring = Keyring::create(&path, &dek, &kek).unwrap();
        assert!(matches!(
            keyring.unlock_file(&fresh_kek()),
            Err(Error::UnlockFailed)
        ));
        let original = keyring.wraps().first().unwrap().clone();
        let tampered = [
            Wrap {
                id: String::from("0000000000000000"),
                ..original.clone()
            },
            Wrap {
                nonce: hex(&[0u8; NONCE_LEN]),
                ..original.clone()
            },
            Wrap {
                nonce: String::from("zz"),
                ..original.clone()
            },
            Wrap {
                nonce: hex(&[0u8; 12]),
                ..original.clone()
            },
            Wrap {
                wrapped: String::from("0"),
                ..original.clone()
            },
            Wrap {
                wrapped: hex(&[0u8; 48]),
                ..original.clone()
            },
        ];
        for wrap in &tampered {
            assert!(
                matches!(keyring.unwrap(wrap, &kek), Err(Error::UnlockFailed)),
                "{wrap:?}"
            );
        }
        // A wrap from another vault does not open here, even under the same KEK.
        let other = Keyring::create(&dir.join("keys").join("other"), &dek, &kek).unwrap();
        assert!(
            keyring
                .unwrap(other.wraps().first().unwrap(), &kek)
                .is_err()
        );
        assert!(keyring.unwrap(&original, &kek).is_ok());
        // A wrap that opens but holds something other than a key is refused too.
        let short = Wrap {
            id: String::from("s"),
            factor: Factor::File,
            label: String::new(),
            added: None,
            nonce: hex(&[1u8; NONCE_LEN]),
            wrapped: hex(&kek
                .cipher()
                .encrypt(
                    XNonce::from_slice(&[1u8; NONCE_LEN]),
                    Payload {
                        msg: b"short",
                        aad: &keyring.aad("s", &Factor::File),
                    },
                )
                .unwrap()),
        };
        assert!(matches!(
            keyring.unwrap(&short, &kek),
            Err(Error::UnlockFailed)
        ));
    }

    #[test]
    fn an_unlock_says_which_security_key_opened_it() {
        let proof = Unlocked::new(
            MasterKey::generate().unwrap(),
            Tier::HardwareKey,
            String::from("v"),
        );
        assert_eq!(proof.opened_by(), None);
        assert_eq!(proof.through("w1").opened_by(), Some("w1"));
        let recovered = Unlocked::recovered(
            MasterKey::generate().unwrap(),
            Tier::HardwareKey,
            String::from("v"),
        );
        assert_eq!(recovered.opened_by(), None, "a backup code is no key");
    }

    #[test]
    fn an_unlock_proof_carries_its_key_tier_and_vault() {
        let dir = test_dir("keyring-proof");
        let dek = MasterKey::generate().unwrap();
        let proof = Unlocked::new(
            MasterKey::from_bytes(*dek.expose()),
            Tier::File,
            String::from("vault-a"),
        );
        assert_eq!(
            (proof.via(), proof.vault_id.as_str()),
            (Tier::File, "vault-a")
        );
        assert!(same_key(&dek, proof.key(), &dir.join("vault")));
    }

    #[test]
    fn a_recovered_proof_says_so_and_tier_changes_keep_the_backup_codes() {
        let recovered = Unlocked::recovered(
            MasterKey::generate().unwrap(),
            Tier::Totp,
            String::from("v"),
        );
        assert!(recovered.is_recovery() && recovered.via() == Tier::Totp);
        assert!(
            !Unlocked::new(
                MasterKey::generate().unwrap(),
                Tier::Totp,
                String::from("v")
            )
            .is_recovery()
        );
        let dir = test_dir("keyring-set-tier");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let path = dir.join("keys").join("keyring");
        let mut keyring = Keyring::create(&path, &dek, &kek).unwrap();
        let backup = Wrap {
            factor: Factor::BackupCode {
                verifier: String::from("00"),
                salt: String::from("00"),
            },
            ..keyring.wrap(&dek, &kek, Factor::File).unwrap()
        };
        let file = keyring.wrap(&dek, &kek, Factor::File).unwrap();
        keyring
            .store_wraps(vec![file.clone(), backup.clone()])
            .unwrap();
        assert_eq!(
            Keyring::load(&path).unwrap().wraps(),
            [file, backup.clone()]
        );
        let next = keyring.wrap(&dek, &kek, Factor::File).unwrap();
        keyring.set_tier(Tier::Totp, vec![next.clone()]).unwrap();
        let reloaded = Keyring::load(&path).unwrap();
        assert_eq!(reloaded.tier(), Tier::Totp);
        assert_eq!(
            reloaded.wraps(),
            [next, backup],
            "the tier's wraps replaced, the backup code kept"
        );
    }

    #[test]
    fn a_keyring_is_never_overwritten_and_loads_only_when_sound() {
        let dir = test_dir("keyring-load");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let keys = dir.join("keys");
        let path = keys.join("keyring");
        Keyring::create(&path, &dek, &kek).unwrap();
        assert!(matches!(
            Keyring::create(&path, &dek, &kek),
            Err(Error::KeyringExists { .. })
        ));
        let owner = rustix::process::getuid().as_raw();
        assert!(matches!(
            Keyring::load_as(&path, owner + 1),
            Err(Error::InsecureKeyFile { .. })
        ));
        assert!(matches!(
            Keyring::load(&keys.join("missing")),
            Err(Error::KeyringLoad { .. })
        ));
        let write = |name: &str, text: &str| {
            let at = keys.join(name);
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&at)
                .unwrap();
            std::fs::write(&at, text).unwrap();
            at
        };
        let stored = std::fs::read_to_string(&path).unwrap();
        for (name, text, detail) in [
            ("junk", String::from("{"), "not a keyring"),
            (
                "future",
                stored.replace("\"format\": 3", "\"format\": 4"),
                "format this build",
            ),
            (
                "anonymous",
                stored.replace(&Keyring::load(&path).unwrap().stored.vault_id, "ab"),
                "no vault id",
            ),
            (
                "huge",
                "x".repeat(usize::try_from(MAX_KEYRING_BYTES).unwrap() + 1),
                "larger than any keyring",
            ),
            // The largest keyring there can be is read, and judged on what
            // it holds.
            (
                "largest",
                "x".repeat(usize::try_from(MAX_KEYRING_BYTES).unwrap()),
                "not a keyring",
            ),
        ] {
            let at = write(name, &text);
            let err = Keyring::load(&at).unwrap_err();
            assert!(
                matches!(&err, Error::KeyringFormat { detail: found, .. } if found.contains(detail)),
                "{name}: {err}"
            );
        }
        let loose = write("loose", &stored);
        std::fs::set_permissions(&loose, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
        assert!(matches!(
            Keyring::load(&loose),
            Err(Error::InsecureKeyFile { .. })
        ));
        let link = keys.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(matches!(
            Keyring::load(&link),
            Err(Error::InsecureKeyFile { .. })
        ));
        assert!(Keyring::load(&path).is_ok());
    }

    #[test]
    fn saving_needs_an_owner_only_directory_and_leaves_no_temp_file() {
        let dir = test_dir("keyring-save");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let keys = dir.join("keys");
        let path = keys.join("keyring");
        let mut keyring = Keyring::create(&path, &dek, &kek).unwrap();
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let next = fresh_kek();
        keyring.use_file_tier(&unlocked, &next).unwrap();
        assert!(
            matches!(
                Keyring::load(&path).unwrap().unlock_file(&kek),
                Err(Error::UnlockFailed)
            ),
            "the old KEK no longer opens it"
        );
        assert!(same_key(
            &dek,
            Keyring::load(&path)
                .unwrap()
                .unlock_file(&next)
                .unwrap()
                .key(),
            &dir.join("vault")
        ));
        assert_eq!(
            std::fs::read_dir(&keys).unwrap().count(),
            2,
            "only the keyring and its edit lock"
        );
        std::fs::set_permissions(&keys, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        assert!(matches!(
            keyring.save(),
            Err(Error::InsecurePermissions { .. })
        ));
        std::fs::set_permissions(&keys, std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .unwrap();
        // Something not a keyring where it would go: nothing is written.
        let blocked = Keyring {
            path: keys.join("blocked"),
            stored: keyring.stored.clone(),
            seen: Cell::new(None),
        };
        std::fs::create_dir(keys.join("blocked")).unwrap();
        std::fs::create_dir(keys.join("blocked").join("x")).unwrap();
        assert!(matches!(blocked.save(), Err(Error::KeyringLoad { .. })));
        // A rename that cannot happen (the target is a directory) cleans up
        // its temp file.
        let temp = keys.join("blocked.tmp-test");
        assert!(write_atomically(&temp, &keys.join("blocked"), &keys, b"{}", own()).is_err());
        assert!(!temp.exists());
        assert!(!std::fs::read_dir(&keys).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("tmp-")
        }));
        let rootless = Keyring {
            path: PathBuf::from("/"),
            stored: keyring.stored.clone(),
            seen: Cell::new(None),
        };
        assert!(matches!(rootless.save(), Err(Error::KeyringLoad { .. })));
        assert!(Keyring::create(Path::new("/"), &dek, &kek).is_err());
    }

    #[test]
    fn hex_round_trips_and_refuses_what_is_not_hex() {
        assert_eq!(hex(&[0, 15, 16, 255]), "000f10ff");
        assert_eq!(unhex("000f10ff"), Some(vec![0, 15, 16, 255]));
        assert_eq!(unhex(""), Some(Vec::new()));
        for bad in ["0", "0g", "0F", "zz", "+1"] {
            assert_eq!(unhex(bad), None, "{bad}");
        }
        assert_eq!(format!("{:?}", fresh_kek()), "Kek([redacted])");
        assert_eq!(Factor::File.tier(), Some(Tier::File));
    }

    #[test]
    fn a_commit_writes_the_tier_wraps_and_retiring_key_at_once_and_rewraps_keep_their_names() {
        let dir = test_dir("keyring-commit");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let path = dir.join("keys").join("keyring");
        let mut keyring = Keyring::create(&path, &dek, &kek).unwrap();
        assert!(keyring.retiring().is_none());
        let mut named = keyring.wrap(&dek, &kek, Factor::File).unwrap();
        named.set_label("desk");
        let other = MasterKey::generate().unwrap();
        let again = keyring.rewrap(&named, &other, &kek).unwrap();
        assert_eq!(
            (&again.id, again.label(), again.added(), &again.factor),
            (&named.id, "desk", named.added(), &named.factor)
        );
        assert_ne!(again.nonce, named.nonce);
        assert!(same_key(
            &other,
            &keyring.unwrap(&again, &kek).unwrap(),
            &dir.join("vault")
        ));
        let explicit = keyring
            .wrap_as(String::from("chosen"), &dek, &kek, Factor::File)
            .unwrap();
        assert_eq!(explicit.id, "chosen");
        let retiring = Retiring {
            nonce: String::from("00"),
            wrapped: String::from("11"),
        };
        keyring
            .commit(Tier::Totp, vec![again.clone()], Some(retiring.clone()))
            .unwrap();
        let reloaded = Keyring::load(&path).unwrap();
        assert_eq!(
            (reloaded.tier(), reloaded.wraps(), reloaded.retiring()),
            (Tier::Totp, [again].as_slice(), Some(&retiring))
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"format\": 3")
        );
        keyring.commit(Tier::File, Vec::new(), None).unwrap();
        assert!(
            !std::fs::read_to_string(&path).unwrap().contains("retiring"),
            "no retiring key, nothing written for it"
        );
    }

    #[test]
    fn a_save_waits_for_the_edit_lock_and_never_rests_on_a_stale_read() {
        let dir = test_dir("keyring-lock");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let path = dir.join("keys").join("keyring");
        let owner = rustix::process::getuid().as_raw();
        let keyring = Keyring::create(&path, &dek, &kek).unwrap();
        // The lock: owner-only, beside the keyring, and kept.
        let lock = lock_path(&path);
        assert_eq!(lock, dir.join("keys").join("keyring.lock"));
        assert_eq!(std::fs::metadata(&lock).unwrap().mode() & 0o777, 0o600);
        // Held by another: a save that won't wait writes nothing.
        let held = EditLock::take(&path, owner, Duration::ZERO).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            keyring.save_waiting(owner, Duration::ZERO),
            Err(Error::KeyringBusy { .. })
        ));
        assert!(matches!(
            EditLock::take(&path, owner, Duration::ZERO),
            Err(Error::KeyringBusy { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // A save that waits gets it once it is let go.
        let waiting = std::thread::scope(|scope| {
            let saving = scope.spawn(move || keyring.save_waiting(owner, Duration::from_secs(5)));
            std::thread::sleep(Duration::from_millis(100));
            drop(held);
            saving.join().unwrap()
        });
        assert!(waiting.is_ok());
        // Another's write since this one read: refused, nothing written.
        let other = Keyring::load(&path).unwrap();
        other.save().unwrap();
        let theirs = std::fs::read(&path).unwrap();
        std::fs::write(&path, [theirs.as_slice(), b" "].concat()).unwrap();
        assert!(matches!(other.save(), Err(Error::KeyringChanged { .. })));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            [theirs.as_slice(), b" "].concat()
        );
        // A lock that isn't the owner's, or isn't a file, is refused.
        assert!(matches!(
            EditLock::take(&path, owner + 1, Duration::ZERO),
            Err(Error::InsecureKeyFile { .. })
        ));
        std::fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere"), &lock).unwrap();
        assert!(EditLock::take(&path, owner, Duration::ZERO).is_err());
        assert!(!dir.join("elsewhere").exists(), "never followed");
        assert_eq!(digest(b"abc").len(), 32);
        assert_ne!(digest(b"abc"), digest(b"abd"));
    }

    #[test]
    fn a_failed_save_leaves_the_keyring_as_it_was() {
        let dir = test_dir("keyring-rollback");
        let dek = MasterKey::generate().unwrap();
        let kek = fresh_kek();
        let path = dir.join("keys").join("keyring");
        let mut keyring = Keyring::create(&path, &dek, &kek).unwrap();
        let before = keyring.wraps().to_vec();
        std::fs::write(&path, b"changed").unwrap();
        let extra = keyring.wrap(&dek, &kek, Factor::File).unwrap();
        assert!(matches!(
            keyring.store_wraps(vec![extra.clone()]),
            Err(Error::KeyringChanged { .. })
        ));
        assert_eq!(keyring.wraps(), before.as_slice());
        assert!(matches!(
            keyring.set_tier(Tier::Totp, vec![extra]),
            Err(Error::KeyringChanged { .. })
        ));
        assert_eq!(
            (keyring.tier(), keyring.wraps()),
            (Tier::File, before.as_slice())
        );
    }

    #[test]
    fn a_factors_debug_never_shows_a_totp_seed() {
        let seed = "3132333435363738393031323334353637383930";
        let totp = Factor::Totp {
            seed: Zeroizing::new(seed.to_owned()),
            salt: String::from("ab"),
            last_step: 7,
        };
        let shown = format!("{totp:?}");
        assert!(!shown.contains(seed));
        assert_eq!(
            shown,
            "Totp { seed: \"[redacted]\", salt: \"ab\", last_step: 7 }"
        );
        let wrap = Wrap {
            id: String::from("w1"),
            factor: totp,
            label: String::new(),
            added: None,
            nonce: String::from("00"),
            wrapped: String::from("00"),
        };
        let wrap_shown = format!("{wrap:?}");
        assert!(!wrap_shown.contains(seed));
        assert_eq!(format!("{:?}", Factor::File), "File");
        assert_eq!(
            format!(
                "{:?}",
                Factor::Fido2 {
                    credential: String::from("c"),
                    salt: String::from("s"),
                    verified: false,
                }
            ),
            "Fido2 { credential: \"c\", salt: \"s\", verified: false }"
        );
        assert_eq!(
            format!(
                "{:?}",
                Factor::BackupCode {
                    verifier: String::from("v"),
                    salt: String::from("s")
                }
            ),
            "BackupCode { verifier: \"v\", salt: \"s\" }"
        );
    }

    #[test]
    fn a_secret_text_is_plain_json_and_a_file_hashes_a_block_at_a_time() {
        // secret_text: the same JSON string either way.
        let mut out = Vec::new();
        secret_text::serialize(
            &Zeroizing::new(String::from("6162")),
            &mut serde_json::Serializer::new(&mut out),
        )
        .unwrap();
        assert_eq!(out, b"\"6162\"");
        let back =
            secret_text::deserialize(&mut serde_json::Deserializer::from_str("\"6162\"")).unwrap();
        assert_eq!(back.as_str(), "6162");
        assert!(secret_text::deserialize(&mut serde_json::Deserializer::from_str("7")).is_err());
        // digest_file: none for no file; the whole file, past one block.
        let dir = test_dir("digest-file");
        assert_eq!(digest_file(&dir.join("absent")).unwrap(), None);
        let long: Vec<u8> = (0..10_000u32)
            .map(|at| u8::try_from(at % 251).unwrap())
            .collect();
        std::fs::write(dir.join("long"), &long).unwrap();
        assert_eq!(digest_file(&dir.join("long")).unwrap(), Some(digest(&long)));
        std::fs::write(dir.join("empty"), b"").unwrap();
        assert_eq!(digest_file(&dir.join("empty")).unwrap(), Some(digest(b"")));
        assert!(digest_file(&dir).is_err(), "a directory is no file to hash");
    }
}
