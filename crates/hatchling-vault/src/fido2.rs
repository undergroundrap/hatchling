//! The hardware-key tier ( phase B): a FIDO2 security key gates
//! the vault.
//!
//! The key is not a yes/no check in front of a key on disk. Each
//! credential's KEK is derived from the key's `hmac-secret` output, which
//! the authenticator computes on the device from a secret it never
//! releases: `HMAC-SHA-256(credential secret, salt)`, the CTAP2 extension.
//! Without that output the KEK cannot be rebuilt, and the wrapped DEK
//! cannot be opened, whatever else an attacker holds.
//!
//! The KEK is `HKDF-SHA-256(ikm = hmac-secret output, salt = vault id,
//! info = KEK_INFO || credential id)`. The `hmac-secret` salt is random
//! per credential and stored in its wrap.
//!
//! [`Authenticator`] is the contract with the device: create a credential
//! with `hmac-secret` requested, and compute `hmac-secret` for one. The USB
//! transport to real keys (CTAP2 over HID) implements it in a later work
//! order; [`crate::virtual_key`] implements it in software for tests.

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroize as _;

use crate::Error;
use crate::keyring::{Factor, Kek, Keyring, Tier, Unlocked, hex, random, unhex};
use crate::rotate::Rotated;
use crate::seal::MasterKey;

/// The relying-party id the vault's credentials are registered under.
pub const RP_ID: &str = "hatchling.vault";
/// HKDF info for a hardware-key KEK, ahead of the credential id.
const KEK_INFO: &[u8] = b"hatchling vault kek: fido2 hmac-secret v1/";

/// A credential an authenticator created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credential {
    /// The credential id the authenticator issued.
    pub id: Vec<u8>,
    /// Whether the authenticator enabled `hmac-secret` for it.
    pub hmac_secret: bool,
}

/// Why an authenticator did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticatorError {
    /// Nobody touched the key in time.
    NoUserPresence,
    /// The key does not hold that credential.
    UnknownCredential,
    /// The key could not verify its user: no PIN is set on it, or the PIN
    /// was not given or was refused.
    NoVerification,
    /// Anything else the device reported.
    Device(String),
}

/// What an enrolled security key asks of its user each time it opens the
/// vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCheck {
    /// A touch.
    Touch,
    /// Its PIN, and a touch. The key computes its answer under its
    /// verified-user secret, so a client that skips the PIN gets an answer
    /// that opens nothing.
    Pin,
}

/// The deliberate choice to protect the vault with one security key.
///
/// Losing the key loses the vault, unless a backup code is kept. Built
/// only by [`SingleKeyRisk::accepted`], so the choice is explicit at the
/// call site, and only accepted with backup codes already made.
#[derive(Debug, Clone, Copy)]
pub struct SingleKeyRisk(());

impl SingleKeyRisk {
    /// The risk, accepted: one key, with backup codes as the way back.
    #[must_use]
    pub const fn accepted() -> Self {
        Self(())
    }
}

/// A FIDO2 authenticator, as the vault needs it (CTAP2).
pub trait Authenticator {
    /// `authenticatorMakeCredential` for `rp_id` and `user_id`, with the
    /// `hmac-secret` extension requested.
    ///
    /// # Errors
    ///
    /// Returns [`AuthenticatorError`] if the device does not create one.
    fn make_credential(
        &mut self,
        rp_id: &str,
        user_id: &[u8],
    ) -> Result<Credential, AuthenticatorError>;

    /// `authenticatorGetAssertion` with the `hmac-secret` extension:
    /// `HMAC-SHA-256` of `salt` under the credential's own secret,
    /// computed on the device.
    ///
    /// # Errors
    ///
    /// Returns [`AuthenticatorError`] if the device does not answer.
    fn hmac_secret(
        &mut self,
        rp_id: &str,
        credential_id: &[u8],
        salt: &[u8; 32],
    ) -> Result<[u8; 32], AuthenticatorError>;

    /// [`Authenticator::hmac_secret`] after the key verified its user with
    /// its PIN. CTAP2 computes it under the credential's other secret
    /// (`CredRandomWithUV`), so it differs from the touch-only answer.
    ///
    /// # Errors
    ///
    /// Returns [`AuthenticatorError::NoVerification`] if the key can't
    /// verify its user (the default, for a key that never can), or as
    /// [`Authenticator::hmac_secret`].
    fn hmac_secret_verified(
        &mut self,
        rp_id: &str,
        credential_id: &[u8],
        salt: &[u8; 32],
    ) -> Result<[u8; 32], AuthenticatorError> {
        let _ = (rp_id, credential_id, salt);
        Err(AuthenticatorError::NoVerification)
    }
}

/// The key's answer for `credential_id` and `salt`, asked as `check` says.
fn answer(
    device: &mut dyn Authenticator,
    credential_id: &[u8],
    salt: &[u8; 32],
    check: KeyCheck,
) -> Result<[u8; 32], AuthenticatorError> {
    match check {
        KeyCheck::Touch => device.hmac_secret(RP_ID, credential_id, salt),
        KeyCheck::Pin => device.hmac_secret_verified(RP_ID, credential_id, salt),
    }
}

/// The KEK for one credential, from its `hmac-secret` output.
fn kek(vault_id: &str, credential_id: &[u8], output: &[u8; 32]) -> Kek {
    let info = [KEK_INFO, credential_id].concat();
    let mut okm = [0u8; 32];
    // HKDF-SHA-256 expands up to 8160 bytes; 32 always fits.
    let _ = Hkdf::<Sha256>::new(Some(vault_id.as_bytes()), output).expand(&info, &mut okm);
    let kek = Kek::from_bytes(okm);
    okm.zeroize();
    kek
}

impl KeyCheck {
    /// The check a wrap records: its key's PIN if `verified`.
    #[must_use]
    pub const fn of(verified: bool) -> Self {
        if verified { Self::Pin } else { Self::Touch }
    }
}

impl Keyring {
    /// Puts the vault at the hardware-key tier around a fresh DEK: one
    /// wrap per key, each under a KEK only that key can rebuild. Every
    /// other wrap is dropped, so no weaker way in remains, and the
    /// rotation is committed for the stores to move to the new DEK
    /// (`rotate.rs`).
    ///
    /// Needs the current tier's unlock, and two keys: a primary and a
    /// backup. A key without `hmac-secret` stops the enrollment, and the
    /// keyring is unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`], [`Error::NeedTwoKeys`],
    /// [`Error::MissingHmacSecret`], [`Error::Authenticator`],
    /// [`Error::RotationUnfinished`], or an I/O or randomness error.
    pub fn enroll_hardware_keys(
        &mut self,
        unlocked: &Unlocked,
        keys: &mut [&mut dyn Authenticator],
    ) -> Result<Rotated, Error> {
        self.enroll_hardware_keys_checked(unlocked, keys, KeyCheck::Touch)
    }

    /// [`Keyring::enroll_hardware_keys`], each key asking its user as
    /// `check` says each time it opens the vault.
    ///
    /// # Errors
    ///
    /// As [`Keyring::enroll_hardware_keys`].
    pub fn enroll_hardware_keys_checked(
        &mut self,
        unlocked: &Unlocked,
        keys: &mut [&mut dyn Authenticator],
        check: KeyCheck,
    ) -> Result<Rotated, Error> {
        self.authorize(unlocked, Tier::HardwareKey)?;
        if keys.len() < 2 {
            return Err(Error::NeedTwoKeys);
        }
        let rotation = self.begin_rotation(unlocked, Tier::HardwareKey)?;
        let mut wraps = Vec::with_capacity(keys.len());
        for device in keys.iter_mut() {
            wraps.push(self.hardware_wrap(rotation.new_key(), &mut **device, check)?);
        }
        self.commit_rotation(rotation, Tier::HardwareKey, wraps)
    }

    /// Puts the vault at the hardware-key tier with one key, knowingly,
    /// around a fresh DEK as [`Keyring::enroll_hardware_keys`] does:
    /// backup codes must already exist, since they are the only way back
    /// if the key is lost, and a new set replaces them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`], [`Error::NeedBackupCodes`],
    /// [`Error::MissingHmacSecret`], [`Error::Authenticator`],
    /// [`Error::RotationUnfinished`], or an I/O or randomness error; the
    /// keyring is then unchanged.
    pub fn enroll_single_hardware_key(
        &mut self,
        unlocked: &Unlocked,
        device: &mut dyn Authenticator,
        risk: SingleKeyRisk,
    ) -> Result<Rotated, Error> {
        self.enroll_single_hardware_key_checked(unlocked, device, risk, KeyCheck::Touch)
    }

    /// [`Keyring::enroll_single_hardware_key`], the key asking its user as
    /// `check` says each time it opens the vault.
    ///
    /// # Errors
    ///
    /// As [`Keyring::enroll_single_hardware_key`].
    pub fn enroll_single_hardware_key_checked(
        &mut self,
        unlocked: &Unlocked,
        device: &mut dyn Authenticator,
        risk: SingleKeyRisk,
        check: KeyCheck,
    ) -> Result<Rotated, Error> {
        let SingleKeyRisk(()) = risk;
        self.authorize(unlocked, Tier::HardwareKey)?;
        if self.backup_codes_left() == 0 {
            return Err(Error::NeedBackupCodes);
        }
        let rotation = self.begin_rotation(unlocked, Tier::HardwareKey)?;
        let wrap = self.hardware_wrap(rotation.new_key(), device, check)?;
        self.commit_rotation(rotation, Tier::HardwareKey, vec![wrap])
    }

    /// The KEK of the enrolled key whose wrap is `wrap`, from the first of
    /// `keys` that answers for its credential with a KEK that opens it.
    pub(crate) fn key_kek(
        &self,
        wrap: &crate::Wrap,
        keys: &mut [&mut dyn Authenticator],
    ) -> Result<Kek, Error> {
        let absent = || Error::KeyAbsent {
            key: if wrap.label().is_empty() {
                wrap.id.clone()
            } else {
                format!("{} ({})", wrap.label(), wrap.id)
            },
        };
        let Factor::Fido2 {
            credential,
            salt,
            verified,
        } = &wrap.factor
        else {
            return Err(absent());
        };
        let check = KeyCheck::of(*verified);
        let credential = unhex(credential).ok_or_else(absent)?;
        let salt = unhex(salt)
            .and_then(|salt| <[u8; 32]>::try_from(salt).ok())
            .ok_or_else(absent)?;
        keys.iter_mut()
            .find_map(|device| {
                let mut output = answer(&mut **device, &credential, &salt, check).ok()?;
                let kek = kek(self.vault_id(), &credential, &output);
                output.zeroize();
                self.unwrap(wrap, &kek).ok().map(|_| kek)
            })
            .ok_or_else(absent)
    }

    /// One credential on `key`, and `dek` wrapped under its KEK, from its
    /// answer asked as `check` says.
    pub(crate) fn hardware_wrap(
        &self,
        dek: &MasterKey,
        device: &mut dyn Authenticator,
        check: KeyCheck,
    ) -> Result<crate::Wrap, Error> {
        let failed = |err: AuthenticatorError| Error::Authenticator {
            reason: format!("{err:?}"),
        };
        let credential = device
            .make_credential(RP_ID, self.vault_id().as_bytes())
            .map_err(failed)?;
        if !credential.hmac_secret {
            return Err(Error::MissingHmacSecret);
        }
        let salt = random::<32>()?;
        let mut output = answer(device, &credential.id, &salt, check).map_err(failed)?;
        let kek = kek(self.vault_id(), &credential.id, &output);
        output.zeroize();
        self.wrap(
            dek,
            &kek,
            Factor::Fido2 {
                credential: hex(&credential.id),
                salt: hex(&salt),
                verified: check == KeyCheck::Pin,
            },
        )
    }

    /// Unlocks the hardware-key tier with `device`: the first of the vault's
    /// credentials the key holds opens it, and the unlock records which
    /// ([`Unlocked::opened_by`]).
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnlockFailed`] if the vault is not at this tier,
    /// or the key holds none of its credentials, or does not answer.
    pub fn unlock_hardware_key(&self, device: &mut dyn Authenticator) -> Result<Unlocked, Error> {
        if self.tier() != Tier::HardwareKey {
            return Err(Error::UnlockFailed);
        }
        self.wraps()
            .iter()
            .find_map(|wrap| {
                let Factor::Fido2 {
                    credential,
                    salt,
                    verified,
                } = &wrap.factor
                else {
                    return None;
                };
                let credential = unhex(credential)?;
                let salt = <[u8; 32]>::try_from(unhex(salt)?).ok()?;
                let mut output =
                    answer(device, &credential, &salt, KeyCheck::of(*verified)).ok()?;
                let kek = kek(self.vault_id(), &credential, &output);
                output.zeroize();
                Some((wrap.id.as_str(), self.unwrap(wrap, &kek).ok()?))
            })
            .map(|(id, dek)| {
                Unlocked::new(dek, Tier::HardwareKey, self.vault_id().to_owned()).through(id)
            })
            .ok_or(Error::UnlockFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{settle, test_dir};
    use crate::virtual_key::VirtualAuthenticator;
    use crate::{MasterKey, SealedVault, Secret, SecretName};

    /// A hardware wrap's credential and salt; empty for any other kind,
    /// so a wrong kind fails the test's own assertions.
    fn parts(factor: &Factor) -> (String, String) {
        match factor {
            Factor::Fido2 {
                credential, salt, ..
            } => (credential.clone(), salt.clone()),
            Factor::File | Factor::Totp { .. } | Factor::BackupCode { .. } => {
                (String::new(), String::new())
            }
        }
    }

    fn file_kek() -> Kek {
        Kek::from_bytes(random::<32>().unwrap())
    }

    /// A vault with one secret, its keyring at the file tier, and that
    /// tier's unlock.
    fn vault(dir: &std::path::Path) -> (Keyring, Kek, MasterKey) {
        let dek = MasterKey::generate().unwrap();
        let kek = file_kek();
        let name: SecretName = "token".parse().unwrap();
        SealedVault::open(&dir.join("vault"), &dek)
            .unwrap()
            .store(&name, &Secret::new(b"kept".to_vec()).unwrap())
            .unwrap();
        (
            Keyring::create(&dir.join("keys").join("keyring"), &dek, &kek).unwrap(),
            kek,
            dek,
        )
    }

    fn reads_the_secret(dir: &std::path::Path, key: &MasterKey) -> bool {
        let name: SecretName = "token".parse().unwrap();
        SealedVault::open(&dir.join("vault"), key)
            .unwrap()
            .fetch(&name)
            .is_ok_and(|secret| secret.as_bytes() == b"kept")
    }

    #[test]
    fn two_registered_keys_each_unlock_the_vault_and_nothing_else_does() {
        let dir = test_dir("fido2-two");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut primary, mut backup, mut stranger) = (
            VirtualAuthenticator::new(),
            VirtualAuthenticator::new(),
            VirtualAuthenticator::new(),
        );
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let rotated = keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut primary, &mut backup])
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        let keyring = Keyring::load(&dir.join("keys").join("keyring")).unwrap();
        assert_eq!(keyring.tier(), Tier::HardwareKey);
        assert_eq!(
            keyring.wraps().len(),
            2,
            "one wrap per key, and no file wrap left behind"
        );
        assert!(
            keyring
                .wraps()
                .iter()
                .all(|wrap| wrap.factor.tier() == Some(Tier::HardwareKey))
        );
        for key in [&mut primary, &mut backup] {
            let unlocked = keyring.unlock_hardware_key(key).unwrap();
            assert_eq!(unlocked.via(), Tier::HardwareKey);
            assert!(reads_the_secret(&dir, unlocked.key()));
        }
        assert!(
            matches!(
                keyring.unlock_hardware_key(&mut stranger),
                Err(Error::UnlockFailed)
            ),
            "an unregistered key opens nothing"
        );
        assert!(
            matches!(keyring.unlock_file(&kek), Err(Error::UnlockFailed)),
            "the old file KEK is no way in"
        );
        primary.set_present(false);
        assert!(
            matches!(
                keyring.unlock_hardware_key(&mut primary),
                Err(Error::UnlockFailed)
            ),
            "no touch, no unlock"
        );
    }

    #[test]
    fn the_vault_bytes_do_not_open_without_the_keys_answer() {
        // The boolean-gate trap: everything on disk, every KEK an attacker
        // can build without the authenticator, opens nothing.
        let dir = test_dir("fido2-gate");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut primary, mut backup) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let unlocked = keyring.unlock_file(&kek).unwrap();
        keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut primary, &mut backup])
            .unwrap();
        let text = std::fs::read_to_string(dir.join("keys").join("keyring")).unwrap();
        assert!(
            !text.contains(&hex(unlocked.key().expose())),
            "the DEK is not on disk"
        );
        for wrap in keyring.wraps() {
            let (credential, salt) = parts(&wrap.factor);
            let credential = unhex(&credential).unwrap();
            // Guesses from what the disk holds: the salt, the credential id, zeros, the old KEK.
            let salt_bytes = <[u8; 32]>::try_from(unhex(&salt).unwrap()).unwrap();
            for guess in [salt_bytes, [0u8; 32], [7u8; 32]] {
                assert!(
                    keyring
                        .unwrap(wrap, &super::kek(keyring.vault_id(), &credential, &guess))
                        .is_err()
                );
            }
            assert!(keyring.unwrap(wrap, &kek).is_err());
            // The key's real answer opens it; the KEK is bound to the vault and the credential.
            let answer = primary
                .hmac_secret(RP_ID, &credential, &salt_bytes)
                .or_else(|_| backup.hmac_secret(RP_ID, &credential, &salt_bytes))
                .unwrap();
            assert!(
                keyring
                    .unwrap(wrap, &super::kek(keyring.vault_id(), &credential, &answer))
                    .is_ok()
            );
            assert!(
                keyring
                    .unwrap(wrap, &super::kek("another-vault", &credential, &answer))
                    .is_err()
            );
            assert!(
                keyring
                    .unwrap(wrap, &super::kek(keyring.vault_id(), b"another", &answer))
                    .is_err()
            );
        }
    }

    #[test]
    fn a_key_without_hmac_secret_fails_enrollment_loudly_and_changes_nothing() {
        let dir = test_dir("fido2-missing");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut good, mut plain) = (
            VirtualAuthenticator::new(),
            VirtualAuthenticator::without_hmac_secret(),
        );
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let err = keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut good, &mut plain])
            .unwrap_err();
        assert!(matches!(err, Error::MissingHmacSecret));
        assert!(err.to_string().contains("hmac-secret"));
        let after = Keyring::load(&dir.join("keys").join("keyring")).unwrap();
        assert_eq!(
            (after.tier(), after.wraps().len()),
            (Tier::File, 1),
            "no silent fallthrough: the vault is as it was"
        );
        assert!(after.unlock_file(&kek).is_ok());
        // A key that refuses or is not touched stops the enrollment the same way.
        let mut absent = VirtualAuthenticator::new();
        absent.set_present(false);
        assert!(matches!(
            keyring.enroll_hardware_keys(&unlocked, &mut [&mut good, &mut absent]),
            Err(Error::Authenticator { .. })
        ));
        let mut forgetful = VirtualAuthenticator::forgetting_credentials();
        assert!(matches!(
            keyring.enroll_hardware_keys(&unlocked, &mut [&mut forgetful, &mut good]),
            Err(Error::Authenticator { .. })
        ));
        assert_eq!(
            Keyring::load(&dir.join("keys").join("keyring"))
                .unwrap()
                .tier(),
            Tier::File
        );
    }

    #[test]
    fn one_key_is_not_enough() {
        let dir = test_dir("fido2-one");
        let (mut keyring, kek, _) = vault(&dir);
        let mut only = VirtualAuthenticator::new();
        let unlocked = keyring.unlock_file(&kek).unwrap();
        assert!(matches!(
            keyring.enroll_hardware_keys(&unlocked, &mut [&mut only]),
            Err(Error::NeedTwoKeys)
        ));
        assert!(matches!(
            keyring.enroll_hardware_keys(&unlocked, &mut []),
            Err(Error::NeedTwoKeys)
        ));
        assert_eq!(keyring.tier(), Tier::File);
    }

    #[test]
    fn a_tier_changes_only_with_its_own_unlock_from_this_vault() {
        let dir = test_dir("fido2-tier");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut primary, mut backup) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let from_file = keyring.unlock_file(&kek).unwrap();
        // Another vault's unlock changes nothing here.
        std::fs::create_dir(dir.join("other")).unwrap();
        let (other, other_kek, _) = vault(&dir.join("other"));
        let foreign = other.unlock_file(&other_kek).unwrap();
        assert!(matches!(
            keyring.enroll_hardware_keys(&foreign, &mut [&mut primary, &mut backup]),
            Err(Error::TierChange { .. })
        ));
        let rotated = keyring
            .enroll_hardware_keys(&from_file, &mut [&mut primary, &mut backup])
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        // The downgrade trap: the file-tier unlock from before is stale now.
        let err = keyring.use_file_tier(&from_file, &file_kek()).unwrap_err();
        assert!(matches!(err, Error::TierChange { .. }), "{err}");
        assert!(matches!(
            keyring.enroll_hardware_keys(&from_file, &mut [&mut primary, &mut backup]),
            Err(Error::TierChange { .. })
        ));
        assert_eq!(keyring.tier(), Tier::HardwareKey);
        // With the hardware tier's own unlock, the change is made, and explicitly.
        let from_key = keyring.unlock_hardware_key(&mut primary).unwrap();
        let new_kek = file_kek();
        keyring.use_file_tier(&from_key, &new_kek).unwrap();
        assert_eq!(keyring.tier(), Tier::File);
        assert!(
            matches!(
                keyring.unlock_hardware_key(&mut primary),
                Err(Error::UnlockFailed)
            ),
            "the keys no longer open it"
        );
        assert!(reads_the_secret(
            &dir,
            keyring.unlock_file(&new_kek).unwrap().key()
        ));
    }

    #[test]
    fn one_hardware_wrap_is_one_credential_opening_with_that_keys_answer() {
        let dir = test_dir("fido2-wrap");
        let (keyring, kek, _) = vault(&dir);
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let mut device = VirtualAuthenticator::new();
        let wrap = keyring
            .hardware_wrap(unlocked.key(), &mut device, KeyCheck::Touch)
            .unwrap();
        let (credential, salt) = parts(&wrap.factor);
        let credential = unhex(&credential).unwrap();
        let salt = <[u8; 32]>::try_from(unhex(&salt).unwrap()).unwrap();
        let answer = device.hmac_secret(RP_ID, &credential, &salt).unwrap();
        assert!(
            keyring
                .unwrap(&wrap, &super::kek(keyring.vault_id(), &credential, &answer))
                .is_ok()
        );
        assert!(matches!(
            keyring.hardware_wrap(
                unlocked.key(),
                &mut VirtualAuthenticator::without_hmac_secret(),
                KeyCheck::Touch
            ),
            Err(Error::MissingHmacSecret)
        ));
    }

    #[test]
    fn a_damaged_wrap_stops_its_own_key_and_only_that_key() {
        let dir = test_dir("fido2-damage");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut primary, mut backup) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let unlocked = keyring.unlock_file(&kek).unwrap();
        keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut primary, &mut backup])
            .unwrap();
        let path = dir.join("keys").join("keyring");
        let text = std::fs::read_to_string(&path).unwrap();
        // Wraps are in enrollment order: the primary's first.
        let wraps = keyring.wraps().to_vec();
        let (first, second) = (wraps.first().unwrap(), wraps.get(1).unwrap());
        let (credential, salt) = parts(&first.factor);
        let variants = [
            text.replace(credential.as_str(), "zz"),
            text.replace(salt.as_str(), "zz"),
            text.replace(salt.as_str(), "00"),
            text.replacen(&format!("\"id\": \"{}\"", first.id), "\"id\": \"moved\"", 1),
        ];
        for damaged in &variants {
            std::fs::write(&path, damaged).unwrap();
            let damaged = Keyring::load(&path).unwrap();
            assert!(matches!(
                damaged.unlock_hardware_key(&mut primary),
                Err(Error::UnlockFailed)
            ));
            assert!(
                damaged.unlock_hardware_key(&mut backup).is_ok(),
                "the other key still opens it"
            );
        }
        // A wrap of another kind in a hardware-key keyring is no way in either.
        let misfiled = text.replacen("\"kind\": \"fido2\"", "\"kind\": \"file\"", 1);
        std::fs::write(&path, misfiled).unwrap();
        let misfiled = Keyring::load(&path).unwrap();
        assert!(misfiled.unlock_hardware_key(&mut primary).is_err());
        assert!(misfiled.unlock_hardware_key(&mut backup).is_ok());
        assert_eq!(second.factor.tier(), Some(Tier::HardwareKey));
    }

    #[test]
    fn a_keys_kek_comes_from_whichever_key_holds_its_credential() {
        let dir = test_dir("fido2-key-kek");
        let (keyring, _, dek) = vault(&dir);
        let (mut holder, mut other) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let mut wrap = keyring.hardware_wrap(&dek, &mut holder).unwrap();
        let kek = keyring
            .key_kek(&wrap, &mut [&mut other, &mut holder])
            .unwrap();
        assert!(keyring.unwrap(&wrap, &kek).is_ok());
        let absent = keyring.key_kek(&wrap, &mut [&mut other]).unwrap_err();
        assert!(matches!(&absent, Error::KeyAbsent { key } if key == &wrap.id));
        wrap.set_label("spare");
        let named = keyring.key_kek(&wrap, &mut []).unwrap_err();
        assert!(
            matches!(&named, Error::KeyAbsent { key } if key == &format!("spare ({})", wrap.id)),
            "{named:?}"
        );
        let file = keyring.wrap(&dek, &file_kek(), Factor::File).unwrap();
        assert!(matches!(
            keyring.key_kek(&file, &mut [&mut holder]),
            Err(Error::KeyAbsent { .. })
        ));
    }
}
