//! Rotating the vault's data key (WO-003).
//!
//! A rotation replaces the DEK every sealed store is sealed under, so that
//! a copy of the earlier key material (a keyring file, a KEK file, a TOTP
//! seed, a backup code) opens nothing once it is done.
//!
//! # Order
//!
//! 1. The keyring commits first, in one atomic write: every wrap of the
//!    tier holds the new DEK, any backup codes are replaced by a new set,
//!    and the old DEK is kept *retiring*, wrapped under a key derived
//!    from the new one.
//! 2. The broker re-seals every store from the old DEK to the new
//!    ([`crate::rekey`], which is resumable).
//! 3. [`Keyring::finish_rotation`] drops the retiring DEK.
//!
//! A crash after step 1 leaves both DEKs reachable through the current
//! tier: [`Keyring::pending_rotation`] gives them back, and the broker
//! refuses to serve until the rotation is finished. A crash before it
//! changes nothing.

use chacha20poly1305::XNonce;
use chacha20poly1305::aead::{Aead, Payload};
use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq as _;
use zeroize::{Zeroize as _, Zeroizing};

use crate::Error;
use crate::backup::BackupCodes;
use crate::fido2::Authenticator;
use crate::keyring::{
    Factor, Kek, Keyring, NONCE_LEN, Retiring, Tier, Unlocked, WRAP_DOMAIN, hex, random, unhex,
};
use crate::seal::MasterKey;

/// The HKDF info for the key the retiring DEK is wrapped under.
const RETIRING_INFO: &[u8] = b"hatchling vault kek: retiring dek v1";

/// The two DEKs of one rotation: the one the stores are sealed under
/// now, and the one they move to. Both are zeroized when this is dropped.
#[derive(Debug)]
pub struct Rotation {
    old: MasterKey,
    new: MasterKey,
}

impl Rotation {
    /// The DEK the stores are moving from.
    #[must_use]
    pub const fn old(&self) -> &MasterKey {
        &self.old
    }

    /// The DEK the stores are moving to.
    #[must_use]
    pub const fn new_key(&self) -> &MasterKey {
        &self.new
    }
}

/// A rotation the keyring has committed, and the backup codes that
/// replace the old ones, if the vault had any. The codes are shown once.
#[derive(Debug)]
pub struct Rotated {
    /// The rotation, to re-seal the stores with and then finish.
    pub rotation: Rotation,
    /// The new backup codes: the old set opens only the retiring DEK.
    pub codes: Option<BackupCodes>,
}

/// A copy of `key`, for a rotation to hold.
const fn copy(key: &MasterKey) -> MasterKey {
    MasterKey::from_bytes(*key.expose())
}

/// The key the retiring DEK is wrapped under: derived from the new DEK,
/// so only an unlock of the new one reaches the old.
fn retiring_kek(vault_id: &str, new: &MasterKey) -> Kek {
    let mut okm = [0u8; 32];
    // HKDF-SHA-256 expands up to 8160 bytes; 32 always fits.
    let _ = Hkdf::<Sha256>::new(Some(vault_id.as_bytes()), new.expose())
        .expand(RETIRING_INFO, &mut okm);
    let kek = Kek::from_bytes(okm);
    okm.zeroize();
    kek
}

/// The associated data binding the retiring DEK to its vault.
fn retiring_aad(vault_id: &str) -> Vec<u8> {
    [WRAP_DOMAIN.as_slice(), vault_id.as_bytes(), b"/retiring"].concat()
}

/// `old` wrapped under the key derived from `new`.
fn seal_retiring(vault_id: &str, old: &MasterKey, new: &MasterKey) -> Result<Retiring, Error> {
    let nonce = random::<NONCE_LEN>()?;
    let wrapped = retiring_kek(vault_id, new)
        .cipher()
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: old.expose(),
                aad: &retiring_aad(vault_id),
            },
        )
        .map_err(|source| Error::SealFailed { source })?;
    Ok(Retiring {
        nonce: hex(&nonce),
        wrapped: hex(&wrapped),
    })
}

/// The retiring DEK, if `new` is the DEK it was wrapped for.
fn open_retiring(vault_id: &str, retiring: &Retiring, new: &MasterKey) -> Result<MasterKey, Error> {
    let nonce = unhex(&retiring.nonce)
        .filter(|nonce| nonce.len() == NONCE_LEN)
        .ok_or(Error::UnlockFailed)?;
    let wrapped = unhex(&retiring.wrapped).ok_or(Error::UnlockFailed)?;
    let mut plain = retiring_kek(vault_id, new)
        .cipher()
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &wrapped,
                aad: &retiring_aad(vault_id),
            },
        )
        .map_err(|_| Error::UnlockFailed)?;
    let old = <[u8; 32]>::try_from(plain.as_slice()).map_err(|_| Error::UnlockFailed);
    plain.zeroize();
    old.map(MasterKey::from_bytes)
}

impl Keyring {
    /// Whether a rotation is committed but not finished: the broker
    /// refuses to serve until it is.
    #[must_use]
    pub const fn rotation_pending(&self) -> bool {
        self.retiring().is_some()
    }

    /// The unfinished rotation, if there is one: `unlocked` holds the new
    /// DEK, and the retiring DEK comes back with it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`] unless `unlocked` came from this
    /// vault through its current tier, or [`Error::UnlockFailed`] if the
    /// retiring DEK does not open under it.
    pub fn pending_rotation(&self, unlocked: &Unlocked) -> Result<Option<Rotation>, Error> {
        self.authorize(unlocked, self.tier())?;
        self.retiring()
            .map(|retiring| {
                open_retiring(self.vault_id(), retiring, unlocked.key()).map(|old| Rotation {
                    old,
                    new: copy(unlocked.key()),
                })
            })
            .transpose()
    }

    /// A rotation from `unlocked`'s DEK to a fresh one, for a keyring
    /// moving to `to`. Nothing is written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`] as [`Keyring::authorize`] does,
    /// [`Error::RotationUnfinished`] while another rotation is, or a
    /// randomness error.
    pub(crate) fn begin_rotation(&self, unlocked: &Unlocked, to: Tier) -> Result<Rotation, Error> {
        self.authorize(unlocked, to)?;
        if self.rotation_pending() {
            return Err(Error::RotationUnfinished);
        }
        Ok(Rotation {
            old: copy(unlocked.key()),
            new: MasterKey::generate()?,
        })
    }

    /// Commits `rotation` with the keyring at `tier`: `wraps` (each
    /// holding the new DEK) are the tier's only wraps, a new set of backup
    /// codes replaces the old if there was one, and the old DEK is kept
    /// retiring. One atomic write; if it fails, nothing changed.
    pub(crate) fn commit_rotation(
        &mut self,
        rotation: Rotation,
        tier: Tier,
        mut wraps: Vec<crate::Wrap>,
    ) -> Result<Rotated, Error> {
        let codes = if self.backup_codes_left() > 0 {
            let (code_wraps, codes) = self.fresh_backup_codes(&rotation.new)?;
            wraps.extend(code_wraps);
            Some(codes)
        } else {
            None
        };
        let retiring = seal_retiring(self.vault_id(), &rotation.old, &rotation.new)?;
        self.commit(tier, wraps, Some(retiring))?;
        Ok(Rotated { rotation, codes })
    }

    /// Rotates the DEK at the vault's current tier (`rekey-vault`): every
    /// way in at this tier is wrapped again around a fresh DEK, keeping
    /// its id and name. The file tier needs its KEK (`file_kek`); the
    /// TOTP tier needs nothing more than its unlock; the hardware-key tier
    /// needs every enrolled key among `keys`, each touched. Each KEK must
    /// open the wrap it replaces, so a wrong KEK file or a missing key
    /// stops it before anything is written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`] or [`Error::RotationUnfinished`] as
    /// [`Keyring::begin_rotation`] does; [`Error::UnlockFailed`] if the
    /// KEK file is absent or wrong; [`Error::KeyAbsent`] naming the first
    /// enrolled key that did not answer; or an I/O or randomness error.
    /// The keyring is then unchanged.
    pub fn rotate_in_place(
        &mut self,
        unlocked: &Unlocked,
        file_kek: Option<&Kek>,
        keys: &mut [&mut dyn Authenticator],
    ) -> Result<Rotated, Error> {
        let tier = self.tier();
        let rotation = self.begin_rotation(unlocked, tier)?;
        let mut wraps = Vec::new();
        for wrap in self
            .wraps()
            .iter()
            .filter(|wrap| wrap.factor.tier() == Some(tier))
        {
            let derived;
            let kek = match &wrap.factor {
                Factor::File => file_kek.ok_or(Error::UnlockFailed)?,
                Factor::Totp { seed, salt, .. } => {
                    let seed = Zeroizing::new(unhex(seed).ok_or(Error::UnlockFailed)?);
                    let salt = unhex(salt).ok_or(Error::UnlockFailed)?;
                    derived = crate::totp::kek(self.vault_id(), &seed, &salt)?;
                    &derived
                }
                Factor::Fido2 { .. } => {
                    derived = self.key_kek(wrap, keys)?;
                    &derived
                }
                Factor::BackupCode { .. } => return Err(Error::UnlockFailed),
            };
            // The KEK must open the wrap it replaces.
            drop(self.unwrap(wrap, kek)?);
            wraps.push(self.rewrap(wrap, &rotation.new, kek)?);
        }
        self.commit_rotation(rotation, tier, wraps)
    }

    /// Finishes `rotation` once every store is re-sealed: the retiring
    /// DEK is dropped. Finishing a finished rotation does nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnlockFailed`] if `rotation` is not the pending
    /// one, or an I/O error; the keyring is then unchanged.
    pub fn finish_rotation(&mut self, rotation: &Rotation) -> Result<(), Error> {
        let Some(retiring) = self.retiring() else {
            return Ok(());
        };
        let old = open_retiring(self.vault_id(), retiring, &rotation.new)?;
        if !bool::from(old.expose().ct_eq(rotation.old.expose())) {
            return Err(Error::UnlockFailed);
        }
        let tier = self.tier();
        let wraps = self.wraps().to_vec();
        self.commit(tier, wraps, None)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    use super::*;
    use crate::testutil::{settle, test_dir};
    use crate::virtual_key::VirtualAuthenticator;
    use crate::{SealedVault, Secret, SecretName, totp_code};

    /// A vault with one secret, its keyring at the file tier, and the KEK.
    fn vault(dir: &Path) -> (Keyring, Kek) {
        let dek = MasterKey::generate().unwrap();
        let kek = Kek::from_bytes(random::<32>().unwrap());
        SealedVault::open(&dir.join("vault"), &dek)
            .unwrap()
            .store(
                &"token".parse::<SecretName>().unwrap(),
                &Secret::new(b"kept".to_vec()).unwrap(),
            )
            .unwrap();
        (
            Keyring::create(&dir.join("keys").join("keyring"), &dek, &kek).unwrap(),
            kek,
        )
    }

    fn reads(dir: &Path, key: &MasterKey) -> bool {
        SealedVault::open(&dir.join("vault"), key)
            .unwrap()
            .fetch(&"token".parse().unwrap())
            .is_ok_and(|secret| secret.as_bytes() == b"kept")
    }

    fn codes(keyring: &mut Keyring, unlocked: &Unlocked) -> Vec<String> {
        keyring
            .generate_backup_codes(unlocked)
            .unwrap()
            .codes()
            .map(str::to_owned)
            .collect()
    }

    /// The seed a pending TOTP enrollment shows the app.
    fn seed(pending: &crate::PendingTotp) -> Vec<u8> {
        crate::totp::seed_of(pending)
    }

    #[test]
    fn rotating_a_file_tier_vault_wraps_its_kek_around_a_new_key() {
        let dir = test_dir("rotate-file");
        let (mut keyring, kek) = vault(&dir);
        let path = dir.join("keys").join("keyring");
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let old_codes = codes(&mut keyring, &unlocked);
        let file_wrap = |ring: &Keyring| {
            ring.wraps()
                .iter()
                .find(|wrap| wrap.factor == Factor::File)
                .cloned()
                .unwrap()
        };
        let before = file_wrap(&keyring);
        // A wrong KEK file, or none, stops it before anything is written.
        let untouched = std::fs::read(&path).unwrap();
        for wrong in [Some(Kek::from_bytes([1; 32])), None] {
            assert!(matches!(
                keyring.rotate_in_place(&unlocked, wrong.as_ref(), &mut []),
                Err(Error::UnlockFailed)
            ));
        }
        assert_eq!(std::fs::read(&path).unwrap(), untouched);
        // Committed: the same way in, around a new key; the old one retiring.
        let rotated = keyring
            .rotate_in_place(&unlocked, Some(&kek), &mut [])
            .unwrap();
        let committed = Keyring::load(&path).unwrap();
        assert!(committed.rotation_pending());
        let after = file_wrap(&committed);
        assert_eq!(
            (&after.id, after.label(), after.added()),
            (&before.id, before.label(), before.added())
        );
        assert_ne!(after, before, "wrapped again");
        let reopened = committed.unlock_file(&kek).unwrap();
        assert!(!reads(&dir, reopened.key()), "the store isn't re-sealed yet");
        let pending = committed.pending_rotation(&reopened).unwrap().unwrap();
        assert!(reads(&dir, pending.old()));
        assert!(!reads(&dir, pending.new_key()));
        // Re-sealed and finished: only the new key opens the store.
        let renewed = settle(&mut keyring, rotated, &dir.join("vault"));
        let finished = Keyring::load(&path).unwrap();
        assert!(!finished.rotation_pending());
        assert!(reads(&dir, finished.unlock_file(&kek).unwrap().key()));
        assert!(!reads(&dir, unlocked.key()), "the old key opens nothing");
        // The codes are a new set: the old ones open nothing.
        assert_eq!(renewed.len(), old_codes.len());
        assert!(matches!(
            keyring.recover(old_codes.first().unwrap()),
            Err(Error::UnlockFailed)
        ));
        assert!(reads(
            &dir,
            keyring.recover(renewed.first().unwrap()).unwrap().key()
        ));
    }

    #[test]
    fn a_totp_vault_rotates_with_its_unlock_alone() {
        let dir = test_dir("rotate-totp");
        let (mut keyring, kek) = vault(&dir);
        let now = 1_700_000_000;
        let from_file = keyring.unlock_file(&kek).unwrap();
        let pending = keyring.begin_totp(&from_file).unwrap();
        let seed = seed(&pending);
        let rotated = keyring
            .confirm_totp(&from_file, &pending, &totp_code(&seed, now), now)
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        let unlocked = keyring
            .unlock_totp(&totp_code(&seed, now + 30), now + 30)
            .unwrap();
        let rotated = keyring.rotate_in_place(&unlocked, None, &mut []).unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        assert!(!reads(&dir, unlocked.key()));
        let next = keyring
            .unlock_totp(&totp_code(&seed, now + 60), now + 60)
            .unwrap();
        assert!(reads(&dir, next.key()), "the same app, the new key");
    }

    #[test]
    fn every_enrolled_key_must_answer_for_the_rotation() {
        let dir = test_dir("rotate-keys");
        let (mut keyring, kek) = vault(&dir);
        let path = dir.join("keys").join("keyring");
        let (mut primary, mut backup) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let from_file = keyring.unlock_file(&kek).unwrap();
        let rotated = keyring
            .enroll_hardware_keys(&from_file, &mut [&mut primary, &mut backup])
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        let backup_id = keyring
            .unlock_hardware_key(&mut backup)
            .unwrap()
            .opened_by()
            .unwrap()
            .to_owned();
        let unlocked = keyring.unlock_hardware_key(&mut primary).unwrap();
        let untouched = std::fs::read(&path).unwrap();
        let mut stranger = VirtualAuthenticator::new();
        let err = keyring
            .rotate_in_place(&unlocked, None, &mut [&mut primary, &mut stranger])
            .unwrap_err();
        assert!(
            matches!(&err, Error::KeyAbsent { key } if key == &backup_id),
            "{err:?}"
        );
        assert!(err.to_string().contains(&backup_id));
        assert_eq!(std::fs::read(&path).unwrap(), untouched, "nothing written");
        let rotated = keyring
            .rotate_in_place(&unlocked, None, &mut [&mut backup, &mut primary])
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        for key in [&mut primary, &mut backup] {
            assert!(reads(&dir, keyring.unlock_hardware_key(key).unwrap().key()));
        }
        assert!(!reads(&dir, unlocked.key()));
    }

    #[test]
    fn a_pending_rotation_refuses_another_and_finishes_only_with_its_own_keys() {
        let dir = test_dir("rotate-pending");
        let (mut keyring, kek) = vault(&dir);
        let unlocked = keyring.unlock_file(&kek).unwrap();
        assert!(keyring.pending_rotation(&unlocked).unwrap().is_none());
        let rotated = keyring
            .rotate_in_place(&unlocked, Some(&kek), &mut [])
            .unwrap();
        let reopened = keyring.unlock_file(&kek).unwrap();
        assert!(matches!(
            keyring.rotate_in_place(&reopened, Some(&kek), &mut []),
            Err(Error::RotationUnfinished)
        ));
        let totp = keyring.begin_totp(&reopened).unwrap();
        let now = 1_700_000_000;
        assert!(matches!(
            keyring.confirm_totp(&reopened, &totp, &totp_code(&seed(&totp), now), now),
            Err(Error::RotationUnfinished)
        ));
        // Another vault's unlock reaches nothing here.
        let other = test_dir("rotate-pending-other");
        let (other_ring, other_kek) = vault(&other);
        assert!(matches!(
            keyring.pending_rotation(&other_ring.unlock_file(&other_kek).unwrap()),
            Err(Error::TierChange { .. })
        ));
        // A rotation that isn't the pending one finishes nothing.
        let stranger = Rotation {
            old: MasterKey::generate().unwrap(),
            new: copy(rotated.rotation.new_key()),
        };
        assert!(matches!(
            keyring.finish_rotation(&stranger),
            Err(Error::UnlockFailed)
        ));
        let wrong_new = Rotation {
            old: copy(rotated.rotation.old()),
            new: MasterKey::generate().unwrap(),
        };
        assert!(matches!(
            keyring.finish_rotation(&wrong_new),
            Err(Error::UnlockFailed)
        ));
        assert!(keyring.rotation_pending());
        // The pending one, given back by the new key, finishes it; again is nothing.
        let resumed = keyring.pending_rotation(&reopened).unwrap().unwrap();
        assert!(reads(&dir, resumed.old()));
        keyring.finish_rotation(&resumed).unwrap();
        keyring.finish_rotation(&resumed).unwrap();
        assert!(!keyring.rotation_pending());
        assert!(keyring.pending_rotation(&reopened).unwrap().is_none());
    }

    #[test]
    fn a_rotation_that_cannot_be_written_changes_nothing() {
        // Root writes through the mode, so only as another user.
        if rustix::process::geteuid().is_root() {
            return;
        }
        let dir = test_dir("rotate-unwritable");
        let (mut keyring, kek) = vault(&dir);
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let wraps = keyring.wraps().to_vec();
        let keys = dir.join("keys");
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o500)).unwrap();
        let failed = keyring.rotate_in_place(&unlocked, Some(&kek), &mut []);
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(failed.is_err());
        assert!(!keyring.rotation_pending());
        assert_eq!(keyring.wraps(), wraps.as_slice());
        assert!(
            !Keyring::load(&keys.join("keyring"))
                .unwrap()
                .rotation_pending()
        );
    }

    #[test]
    fn the_retiring_key_opens_only_under_the_new_key_of_its_own_vault() {
        let (old, new) = (
            MasterKey::generate().unwrap(),
            MasterKey::generate().unwrap(),
        );
        let retiring = seal_retiring("ab", &old, &new).unwrap();
        assert_eq!(
            open_retiring("ab", &retiring, &new).unwrap().expose(),
            old.expose()
        );
        assert!(open_retiring("ab", &retiring, &old).is_err());
        assert!(open_retiring("cd", &retiring, &new).is_err(), "another vault");
        assert_eq!(retiring_aad("ab"), b"HVK1ab/retiring".to_vec());
        let damaged = Retiring {
            nonce: String::from("00"),
            ..retiring.clone()
        };
        assert!(open_retiring("ab", &damaged, &new).is_err());
        assert_ne!(
            seal_retiring("ab", &old, &new).unwrap(),
            retiring,
            "a fresh nonce each time"
        );
        assert_eq!(copy(&old).expose(), old.expose());
    }
}
