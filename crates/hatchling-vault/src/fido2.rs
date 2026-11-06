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

impl AuthenticatorError {
    /// What went wrong, in words for the person holding the key.
    #[must_use]
    pub fn said(&self) -> String {
        match self {
            Self::NoUserPresence => String::from("the key wasn't touched in time"),
            Self::UnknownCredential => String::from("this key isn't one registered here"),
            Self::NoVerification => String::from("the key couldn't check its PIN"),
            Self::Device(said) => said.clone(),
        }
    }
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

    /// Whether the key holds `credential_id`, asked without a touch or its
    /// PIN: `None` when it can't say (the default).
    fn holds(&mut self, rp_id: &str, credential_id: &[u8]) -> Option<bool> {
        let _ = (rp_id, credential_id);
        None
    }

    /// Whether a PIN is set on the key: `Some(false)` for a key that takes
    /// one and has none (a new key), `None` for one that takes none.
    fn pin_set(&self) -> Option<bool> {
        None
    }

    /// The shortest PIN the key takes, in characters.
    fn min_pin_length(&self) -> usize {
        4
    }

    /// What to call this key, as far as it says (the default: nothing).
    fn model(&self) -> Option<&'static str> {
        None
    }

    /// Sets `pin` on a key with none.
    ///
    /// # Errors
    ///
    /// Returns [`AuthenticatorError`] if the key doesn't take it.
    fn set_pin(&mut self, pin: &str) -> Result<(), AuthenticatorError> {
        let _ = pin;
        Err(AuthenticatorError::NoVerification)
    }

    /// Changes the key's PIN from `old` to `new`.
    ///
    /// # Errors
    ///
    /// Returns [`AuthenticatorError`] if the key doesn't take it.
    fn change_pin(&mut self, old: &str, new: &str) -> Result<(), AuthenticatorError> {
        let _ = (old, new);
        Err(AuthenticatorError::NoVerification)
    }
}

/// A move to the hardware-key tier, key by key.
///
/// Nothing is written until [`Keyring::commit_key_enrollment`], so a key
/// can be registered, swapped for the next on a machine with one port, and
/// the whole thing abandoned (or the power lost) with the vault as it was.
pub struct KeyEnrollment {
    rotation: crate::rotate::Rotation,
    wraps: Vec<crate::Wrap>,
    check: KeyCheck,
}

impl KeyEnrollment {
    /// How many keys are registered so far.
    #[must_use]
    pub fn keys(&self) -> usize {
        self.wraps.len()
    }

    /// Whether `device` is a key already in this enrollment, as far as it
    /// can say, without a touch.
    pub fn holds(&self, device: &mut dyn Authenticator) -> bool {
        self.wraps.iter().any(|wrap| key_holds(device, wrap))
    }

    /// The names given so far.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.wraps
            .iter()
            .map(|wrap| wrap.label().to_owned())
            .collect()
    }

    /// Names the key registered last: after its touch, as that's what
    /// shows which of two plugged in it is.
    pub fn name_last(&mut self, label: &crate::manage::Label) {
        if let Some(wrap) = self.wraps.last_mut() {
            wrap.set_label(label.as_str());
        }
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

    /// Starts a move to the hardware-key tier, each key asking its user as
    /// `check` says. Nothing is written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`] or [`Error::RotationUnfinished`], or
    /// a randomness error.
    pub fn begin_key_enrollment(
        &self,
        unlocked: &Unlocked,
        check: KeyCheck,
    ) -> Result<KeyEnrollment, Error> {
        self.authorize(unlocked, Tier::HardwareKey)?;
        Ok(KeyEnrollment {
            rotation: self.begin_rotation(unlocked, Tier::HardwareKey)?,
            wraps: Vec::new(),
            check,
        })
    }

    /// Registers `device` in `enrollment`. A key already in it is refused,
    /// before it's asked for anything: a credential is new each time, so
    /// only the key can say it's the same one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyChange`] for a key already in it,
    /// [`Error::MissingHmacSecret`], [`Error::Authenticator`], or an I/O
    /// or randomness error; `enrollment` is then as it was.
    pub fn enroll_key(
        &self,
        enrollment: &mut KeyEnrollment,
        device: &mut dyn Authenticator,
    ) -> Result<(), Error> {
        if enrollment.holds(device) {
            return Err(Error::KeyChange {
                reason: "this key is already registered",
            });
        }
        let wrap = self.hardware_wrap(enrollment.rotation.new_key(), device, enrollment.check)?;
        enrollment.wraps.push(wrap);
        Ok(())
    }

    /// Moves the vault to the hardware-key tier with the keys in
    /// `enrollment`, as [`Keyring::enroll_hardware_keys`] does. One key
    /// needs `risk` accepted and backup codes already made, as
    /// [`Keyring::enroll_single_hardware_key`] does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NeedTwoKeys`] with no key, or one without `risk`,
    /// [`Error::NeedBackupCodes`], or an I/O or randomness error; the
    /// keyring is then unchanged.
    pub fn commit_key_enrollment(
        &mut self,
        enrollment: KeyEnrollment,
        risk: Option<SingleKeyRisk>,
    ) -> Result<Rotated, Error> {
        match (enrollment.wraps.len(), risk) {
            (0, _) | (1, None) => return Err(Error::NeedTwoKeys),
            (1, Some(_)) if self.backup_codes_left() == 0 => return Err(Error::NeedBackupCodes),
            _ => {}
        }
        self.commit_rotation(enrollment.rotation, Tier::HardwareKey, enrollment.wraps)
    }

    /// Whether `device` holds one of this vault's enrolled keys, asked
    /// without a touch: `None` when it can't say.
    pub fn enrolled_on(&self, device: &mut dyn Authenticator) -> Option<bool> {
        let mut said = None;
        for wrap in self.wraps() {
            match credential_bytes(wrap).map(|id| device.holds(RP_ID, &id)) {
                Some(Some(true)) => return Some(true),
                Some(Some(false)) => said = Some(false),
                Some(None) | None => {}
            }
        }
        said
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
        let failed = |err: AuthenticatorError| Error::Authenticator { reason: err.said() };
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
    /// or the key holds none of its credentials, and
    /// [`Error::Authenticator`], saying why, if the key refuses (a wrong
    /// PIN, no touch). A refusal stops it at once: trying the vault's other
    /// credentials would ask again, and spend another of the key's PIN
    /// tries. A credential the key says it doesn't hold is passed over
    /// without asking anything.
    pub fn unlock_hardware_key(&self, device: &mut dyn Authenticator) -> Result<Unlocked, Error> {
        if self.tier() != Tier::HardwareKey {
            return Err(Error::UnlockFailed);
        }
        for wrap in self.wraps() {
            let Factor::Fido2 {
                credential,
                salt,
                verified,
            } = &wrap.factor
            else {
                continue;
            };
            let Some((credential, salt)) =
                unhex(credential).zip(unhex(salt).and_then(|salt| <[u8; 32]>::try_from(salt).ok()))
            else {
                continue;
            };
            if device.holds(RP_ID, &credential) == Some(false) {
                continue;
            }
            let mut output = match answer(device, &credential, &salt, KeyCheck::of(*verified)) {
                Ok(output) => output,
                Err(AuthenticatorError::UnknownCredential) => continue,
                Err(refused) => {
                    return Err(Error::Authenticator {
                        reason: refused.said(),
                    });
                }
            };
            let kek = kek(self.vault_id(), &credential, &output);
            output.zeroize();
            if let Ok(dek) = self.unwrap(wrap, &kek) {
                return Ok(
                    Unlocked::new(dek, Tier::HardwareKey, self.vault_id().to_owned())
                        .through(wrap.id.as_str()),
                );
            }
        }
        Err(Error::UnlockFailed)
    }
}

/// The credential of a hardware key's `wrap`, as bytes.
pub(crate) fn credential_bytes(wrap: &crate::Wrap) -> Option<Vec<u8>> {
    match &wrap.factor {
        Factor::Fido2 { credential, .. } => unhex(credential),
        Factor::File | Factor::Totp { .. } | Factor::BackupCode { .. } => None,
    }
}

/// Whether `device` says it holds `wrap`'s credential.
pub(crate) fn key_holds(device: &mut dyn Authenticator, wrap: &crate::Wrap) -> bool {
    credential_bytes(wrap).is_some_and(|id| device.holds(RP_ID, &id) == Some(true))
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

    /// A key that answers only what every [`Authenticator`] must, here to
    /// say it can't change a PIN.
    struct Bare;

    impl Authenticator for Bare {
        fn make_credential(&mut self, _: &str, _: &[u8]) -> Result<Credential, AuthenticatorError> {
            Err(AuthenticatorError::NoVerification)
        }

        fn hmac_secret(
            &mut self,
            _: &str,
            _: &[u8],
            _: &[u8; 32],
        ) -> Result<[u8; 32], AuthenticatorError> {
            Err(AuthenticatorError::NoVerification)
        }
    }

    /// A key that can't say claims nothing: not holding, not a PIN, and
    /// no way to set one.
    #[test]
    fn a_key_that_cant_say_claims_nothing() {
        let mut key = Bare;
        assert_eq!(key.holds("rp", b"id"), None);
        assert_eq!(key.pin_set(), None);
        assert_eq!(key.min_pin_length(), 4);
        assert_eq!(key.model(), None);
        assert!(key.set_pin("2468").is_err());
        assert!(key.change_pin("2468", "1357").is_err());
    }

    /// An enrollment counts the keys in it.
    #[test]
    fn an_enrollment_counts_its_keys() {
        let dir = test_dir("fido2-count");
        let (keyring, kek, _) = vault(&dir);
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let mut enrollment = keyring
            .begin_key_enrollment(&unlocked, KeyCheck::Touch)
            .unwrap();
        assert_eq!(enrollment.keys(), 0);
        let (mut one, mut two) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        keyring.enroll_key(&mut enrollment, &mut one).unwrap();
        keyring.enroll_key(&mut enrollment, &mut two).unwrap();
        assert_eq!(enrollment.keys(), 2);
        enrollment.name_last(&"second".parse().unwrap());
        assert_eq!(enrollment.names(), ["", "second"]);
    }

    /// A key that counts what it's asked, for the PIN tries a wrong PIN
    /// spends.
    struct Counting {
        key: VirtualAuthenticator,
        asked: u32,
    }

    impl Authenticator for Counting {
        fn make_credential(
            &mut self,
            rp_id: &str,
            user_id: &[u8],
        ) -> Result<Credential, AuthenticatorError> {
            self.key.make_credential(rp_id, user_id)
        }

        fn hmac_secret(
            &mut self,
            rp_id: &str,
            credential_id: &[u8],
            salt: &[u8; 32],
        ) -> Result<[u8; 32], AuthenticatorError> {
            self.key.hmac_secret(rp_id, credential_id, salt)
        }

        fn hmac_secret_verified(
            &mut self,
            rp_id: &str,
            credential_id: &[u8],
            salt: &[u8; 32],
        ) -> Result<[u8; 32], AuthenticatorError> {
            self.asked += 1;
            self.key.hmac_secret_verified(rp_id, credential_id, salt)
        }

        fn holds(&mut self, rp_id: &str, credential_id: &[u8]) -> Option<bool> {
            self.key.holds(rp_id, credential_id)
        }
    }

    /// Keys registered one at a time, as on a machine with one port:
    /// nothing changes until the commit, and an abandoned move leaves the
    /// vault as it was.
    #[test]
    fn keys_are_registered_one_at_a_time_and_nothing_changes_until_the_commit() {
        let dir = test_dir("fido2-staged");
        let (mut keyring, kek, _) = vault(&dir);
        let path = dir.join("keys").join("keyring");
        let before = std::fs::read(&path).unwrap();
        let (mut nano, mut nfc) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let label = |name: &str| name.parse::<crate::Label>().unwrap();
        let unlocked = keyring.unlock_file(&kek).unwrap();
        // Abandoned: the keys registered, and the power lost.
        let mut abandoned = keyring
            .begin_key_enrollment(&unlocked, KeyCheck::Touch)
            .unwrap();
        keyring.enroll_key(&mut abandoned, &mut nano).unwrap();
        assert_eq!(abandoned.keys(), 1);
        drop(abandoned);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(Keyring::load(&path).unwrap().unlock_file(&kek).is_ok());

        let mut enrollment = keyring
            .begin_key_enrollment(&unlocked, KeyCheck::Touch)
            .unwrap();
        keyring.enroll_key(&mut enrollment, &mut nano).unwrap();
        enrollment.name_last(&label("nano"));
        assert_eq!(std::fs::read(&path).unwrap(), before, "still nothing");
        // The same key again, as a swap that didn't happen.
        assert!(matches!(
            keyring.enroll_key(&mut enrollment, &mut nano),
            Err(Error::KeyChange { .. })
        ));
        assert_eq!(enrollment.keys(), 1);
        assert!(enrollment.holds(&mut nano));
        assert!(!enrollment.holds(&mut nfc));
        keyring.enroll_key(&mut enrollment, &mut nfc).unwrap();
        enrollment.name_last(&label("nfc"));
        assert_eq!(std::fs::read(&path).unwrap(), before, "still nothing");
        let rotated = keyring.commit_key_enrollment(enrollment, None).unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        let keyring = Keyring::load(&path).unwrap();
        assert_eq!(keyring.tier(), Tier::HardwareKey);
        let names: Vec<String> = keyring
            .hardware_keys()
            .into_iter()
            .map(|key| key.label)
            .collect();
        assert_eq!(names, ["nano", "nfc"]);
        for key in [&mut nano, &mut nfc] {
            let opened = keyring.unlock_hardware_key(key).unwrap();
            assert!(reads_the_secret(&dir, opened.key()));
        }
    }

    /// One key needs its risk accepted, and backup codes; two need neither.
    #[test]
    fn a_commit_needs_two_keys_or_one_with_its_way_back() {
        let dir = test_dir("fido2-staged-one");
        let (mut keyring, kek, _) = vault(&dir);
        let mut single = VirtualAuthenticator::new();
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let begin = |keyring: &Keyring| {
            keyring
                .begin_key_enrollment(&unlocked, KeyCheck::Touch)
                .unwrap()
        };
        let empty = begin(&keyring);
        assert!(matches!(
            keyring.commit_key_enrollment(empty, Some(SingleKeyRisk::accepted())),
            Err(Error::NeedTwoKeys)
        ));
        let mut one = begin(&keyring);
        keyring.enroll_key(&mut one, &mut single).unwrap();
        assert!(matches!(
            keyring.commit_key_enrollment(one, None),
            Err(Error::NeedTwoKeys)
        ));
        let mut one = begin(&keyring);
        keyring.enroll_key(&mut one, &mut single).unwrap();
        assert!(matches!(
            keyring.commit_key_enrollment(one, Some(SingleKeyRisk::accepted())),
            Err(Error::NeedBackupCodes)
        ));
        keyring.generate_backup_codes(&unlocked).unwrap();
        let mut one = begin(&keyring);
        keyring.enroll_key(&mut one, &mut single).unwrap();
        let rotated = keyring
            .commit_key_enrollment(one, Some(SingleKeyRisk::accepted()))
            .unwrap();
        assert!(rotated.codes.is_some(), "a new set, to show");
        settle(&mut keyring, rotated, &dir.join("vault"));
        assert_eq!(keyring.tier(), Tier::HardwareKey);
    }

    /// A wrong PIN is asked once and said: the vault's other credentials
    /// aren't tried with it, each a PIN try spent.
    #[test]
    fn a_wrong_pin_spends_one_try_and_says_why() {
        let dir = test_dir("fido2-one-try");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut first, mut second) = (
            VirtualAuthenticator::with_pin("2468"),
            VirtualAuthenticator::with_pin("1357"),
        );
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let rotated = keyring
            .enroll_hardware_keys_checked(&unlocked, &mut [&mut first, &mut second], KeyCheck::Pin)
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        // Either key, wrong: one question, whichever wrap is its own.
        let wrong = |mut key: VirtualAuthenticator| {
            key.set_typed(Some("0000"));
            let mut counting = Counting { key, asked: 0 };
            let refused = keyring.unlock_hardware_key(&mut counting);
            assert!(matches!(refused, Err(Error::Authenticator { .. })));
            assert_eq!(counting.asked, 1);
        };
        wrong(first);
        wrong(second);
        // A key that can't say what it holds is still tried, once it's asked.
        let mut stranger = Counting {
            key: VirtualAuthenticator::with_pin("2468"),
            asked: 0,
        };
        assert!(matches!(
            keyring.unlock_hardware_key(&mut stranger),
            Err(Error::UnlockFailed)
        ));
        assert_eq!(stranger.asked, 0, "not its credential: no PIN asked");
    }

    /// What a key says of the vault's credentials, without a touch.
    #[test]
    fn a_key_says_whether_its_enrolled_without_a_touch() {
        let dir = test_dir("fido2-enrolled-on");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut one, mut two, mut stranger) = (
            VirtualAuthenticator::new(),
            VirtualAuthenticator::new(),
            VirtualAuthenticator::new(),
        );
        let unlocked = keyring.unlock_file(&kek).unwrap();
        assert_eq!(
            keyring.enrolled_on(&mut stranger),
            None,
            "no keys to ask of"
        );
        let rotated = keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut one, &mut two])
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        two.set_present(false);
        assert_eq!(keyring.enrolled_on(&mut one), Some(true));
        assert_eq!(keyring.enrolled_on(&mut two), Some(true), "no touch needed");
        assert_eq!(keyring.enrolled_on(&mut stranger), Some(false));
    }

    /// The same key isn't added twice: only it can say it's the same.
    #[test]
    fn a_key_already_enrolled_isnt_added_again() {
        let dir = test_dir("fido2-add-same");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut one, mut two, mut third) = (
            VirtualAuthenticator::new(),
            VirtualAuthenticator::new(),
            VirtualAuthenticator::new(),
        );
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let rotated = keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut one, &mut two])
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        let opened = keyring.unlock_hardware_key(&mut one).unwrap();
        let name = "again".parse::<crate::Label>().unwrap();
        assert!(matches!(
            keyring.add_hardware_key(&opened, &mut two, &name),
            Err(Error::KeyChange { .. })
        ));
        assert_eq!(keyring.hardware_keys().len(), 2);
        keyring
            .add_hardware_key(&opened, &mut third, &name)
            .unwrap();
        assert_eq!(keyring.hardware_keys().len(), 3);
    }

    #[test]
    fn a_refusal_is_said_in_words() {
        assert!(
            AuthenticatorError::NoUserPresence
                .said()
                .contains("touched")
        );
        assert!(
            AuthenticatorError::UnknownCredential
                .said()
                .contains("registered")
        );
        assert!(AuthenticatorError::NoVerification.said().contains("PIN"));
        assert_eq!(
            AuthenticatorError::Device(String::from("as it is")).said(),
            "as it is"
        );
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
                Err(Error::Authenticator { .. })
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
        let mut wrap = keyring
            .hardware_wrap(&dek, &mut holder, KeyCheck::Touch)
            .unwrap();
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

    /// A key that answers for a touch only, keeping the trait's default.
    struct TouchOnly;

    impl Authenticator for TouchOnly {
        fn make_credential(
            &mut self,
            _rp_id: &str,
            _user_id: &[u8],
        ) -> Result<Credential, AuthenticatorError> {
            Err(AuthenticatorError::NoUserPresence)
        }

        fn hmac_secret(
            &mut self,
            _rp_id: &str,
            _credential_id: &[u8],
            _salt: &[u8; 32],
        ) -> Result<[u8; 32], AuthenticatorError> {
            Ok([1; 32])
        }
    }

    /// A key's credential and salt, as bytes.
    fn answer_inputs(factor: &Factor) -> (Vec<u8>, [u8; 32]) {
        let (credential, salt) = parts(factor);
        (
            unhex(&credential).unwrap(),
            <[u8; 32]>::try_from(unhex(&salt).unwrap()).unwrap(),
        )
    }

    #[test]
    fn a_key_enrolled_with_its_pin_opens_the_vault_only_with_it() {
        let dir = test_dir("fido2-pin");
        let (mut keyring, kek, _) = vault(&dir);
        let (mut primary, mut backup) = (
            VirtualAuthenticator::with_pin("2468"),
            VirtualAuthenticator::with_pin("1357"),
        );
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let rotated = keyring
            .enroll_hardware_keys_checked(
                &unlocked,
                &mut [&mut primary, &mut backup],
                KeyCheck::Pin,
            )
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        assert!(
            keyring
                .hardware_keys()
                .iter()
                .all(|key| key.check == KeyCheck::Pin)
        );
        // With its PIN it opens; with none, or a wrong one, it doesn't.
        let opened = keyring.unlock_hardware_key(&mut primary).unwrap();
        assert!(reads_the_secret(&dir, opened.key()));
        for typed in [None, Some("0000")] {
            primary.set_typed(typed);
            assert!(matches!(
                keyring.unlock_hardware_key(&mut primary),
                Err(Error::Authenticator { .. })
            ));
        }
        primary.set_typed(Some("2468"));
        // The same key's touch-only answer opens nothing.
        let wrap = keyring
            .wraps()
            .iter()
            .find(|wrap| Some(wrap.id.as_str()) == opened.opened_by())
            .unwrap()
            .clone();
        let (credential, salt) = answer_inputs(&wrap.factor);
        let touched = primary.hmac_secret(RP_ID, &credential, &salt).unwrap();
        assert!(
            keyring
                .unwrap(
                    &wrap,
                    &super::kek(keyring.vault_id(), &credential, &touched)
                )
                .is_err()
        );
        // Rotating needs each key's PIN too.
        primary.set_typed(None);
        assert!(matches!(
            keyring.rotate_in_place(&opened, None, &mut [&mut primary, &mut backup]),
            Err(Error::KeyAbsent { .. })
        ));
        primary.set_typed(Some("2468"));
        let rotated = keyring
            .rotate_in_place(&opened, None, &mut [&mut primary, &mut backup])
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        assert!(reads_the_secret(
            &dir,
            keyring.unlock_hardware_key(&mut backup).unwrap().key()
        ));
        // The flag is bound to the wrap: a keyring that drops it opens nothing.
        let path = dir.join("keys").join("keyring");
        let mut stored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for wrap in stored
            .get_mut("wraps")
            .and_then(serde_json::Value::as_array_mut)
            .unwrap()
        {
            wrap.get_mut("factor")
                .and_then(serde_json::Value::as_object_mut)
                .unwrap()
                .remove("verified");
        }
        std::fs::write(&path, serde_json::to_string(&stored).unwrap()).unwrap();
        let edited = Keyring::load(&path).unwrap();
        assert!(
            edited
                .hardware_keys()
                .iter()
                .all(|key| key.check == KeyCheck::Touch)
        );
        for key in [&mut primary, &mut backup] {
            assert!(matches!(
                edited.unlock_hardware_key(key),
                Err(Error::UnlockFailed)
            ));
        }
        // Even the PIN answer opens no edited wrap: the PIN is in the wrap's
        // associated data, so a key that answered alike either way would
        // still not open a wrap that no longer asks for it.
        let mut tried = 0;
        for (before, after) in keyring.wraps().iter().zip(edited.wraps()) {
            let (credential, salt) = answer_inputs(&after.factor);
            let Ok(answer) = primary.hmac_secret_verified(RP_ID, &credential, &salt) else {
                continue;
            };
            let kek = super::kek(keyring.vault_id(), &credential, &answer);
            assert!(keyring.unwrap(before, &kek).is_ok(), "as enrolled it opens");
            assert!(edited.unwrap(after, &kek).is_err(), "edited it doesn't");
            tried += 1;
        }
        assert_eq!(tried, 1, "the primary key's one wrap");
    }

    #[test]
    fn a_key_without_a_pin_is_never_enrolled_to_ask_for_one() {
        let dir = test_dir("fido2-no-pin");
        let (mut keyring, kek, _) = vault(&dir);
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let (mut plain, mut other) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let err = keyring
            .enroll_hardware_keys_checked(&unlocked, &mut [&mut plain, &mut other], KeyCheck::Pin)
            .unwrap_err();
        assert!(
            matches!(&err, Error::Authenticator { reason } if reason.contains("couldn't check its PIN")),
            "{err:?}"
        );
        assert_eq!(keyring.tier(), Tier::File);
        assert_eq!(
            (KeyCheck::of(true), KeyCheck::of(false)),
            (KeyCheck::Pin, KeyCheck::Touch)
        );
        // A key that can't verify its user says so, by default.
        let salt = random::<32>().unwrap();
        assert_eq!(
            TouchOnly.hmac_secret_verified(RP_ID, b"c", &salt),
            Err(AuthenticatorError::NoVerification)
        );
        assert_eq!(
            answer(&mut TouchOnly, b"c", &salt, KeyCheck::Touch),
            Ok([1; 32])
        );
        assert_eq!(
            answer(&mut TouchOnly, b"c", &salt, KeyCheck::Pin),
            Err(AuthenticatorError::NoVerification)
        );
    }
}
