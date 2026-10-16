//! Key management: what protects the vault, and adding,
//! labelling and removing its security keys.
//!
//! - **Labels are display-only.** A key is named by its wrap id in every
//!   decision. Its label is the user's own words, shown and never
//!   compared: two keys may share one, and a label may look like an id.
//! - **A key never authorizes its own removal.** An unlock records which
//!   key opened it ([`Unlocked::opened_by`]). Removing that key needs
//!   another enrolled key, or a backup code.
//! - **Removal never changes the tier.** The last key cannot be removed:
//!   the vault's tier is changed first, with its own unlock. Going down to
//!   one key needs backup codes and the explicit [`SingleKeyRisk`], as
//!   enrolling one key does.

use std::str::FromStr;

use crate::Error;
use crate::fido2::{Authenticator, SingleKeyRisk};
use crate::keyring::{Factor, Keyring, Tier, Unlocked, Wrap};

/// The longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 64;

/// A security key's label: the user's name for it, such as
/// `YubiKey 5C, desk`. Trimmed, 1 to [`MAX_LABEL_CHARS`] characters, with no control
/// characters. Display-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label(String);

impl Label {
    /// The label's text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Label {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self, Error> {
        let label = raw.trim();
        if label.is_empty() {
            return Err(Error::InvalidLabel {
                reason: "a label cannot be empty",
            });
        }
        if label.chars().count() > MAX_LABEL_CHARS {
            return Err(Error::InvalidLabel {
                reason: "a label is at most 64 characters",
            });
        }
        if label.chars().any(char::is_control) {
            return Err(Error::InvalidLabel {
                reason: "a label cannot contain control characters",
            });
        }
        Ok(Self(label.to_owned()))
    }
}

/// One enrolled security key, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    /// Its wrap id: what names the key in every decision.
    pub id: String,
    /// The user's name for it; empty when it has none.
    pub label: String,
    /// When it was added, in Unix seconds; `None` for a key enrolled
    /// before keyrings kept dates.
    pub added: Option<u64>,
}

impl KeyInfo {
    fn of(wrap: &Wrap) -> Self {
        Self {
            id: wrap.id.clone(),
            label: wrap.label().to_owned(),
            added: wrap.added(),
        }
    }
}

/// Something about the vault's protection the user should act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    /// One security key left: losing it leaves only the backup codes.
    OneHardwareKey {
        /// The backup codes left: the only way back without the key.
        backup_codes_left: usize,
    },
    /// A hardware-key vault with no security key: only a backup code
    /// opens it.
    NoHardwareKey {
        /// The backup codes left.
        backup_codes_left: usize,
    },
}

/// What protects the vault: its tier, every way in, and what to act on.
/// Never what any factor holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protection {
    /// The vault's tier.
    pub tier: Tier,
    /// The enrolled security keys, in the order they were added.
    pub hardware_keys: Vec<KeyInfo>,
    /// Whether an authenticator app (TOTP) unlocks it.
    pub totp: bool,
    /// Whether a key file unlocks it.
    pub file: bool,
    /// The unused backup codes.
    pub backup_codes_left: usize,
    /// What the user should act on.
    pub warnings: Vec<Warning>,
}

const fn is_hardware_key(wrap: &Wrap) -> bool {
    matches!(wrap.factor, Factor::Fido2 { .. })
}

impl Keyring {
    /// The enrolled security keys, in the order they were added.
    #[must_use]
    pub fn hardware_keys(&self) -> Vec<KeyInfo> {
        self.wraps()
            .iter()
            .filter(|wrap| is_hardware_key(wrap))
            .map(KeyInfo::of)
            .collect()
    }

    /// What protects the vault, and what the user should act on: one
    /// security key left, or none at a hardware-key vault.
    #[must_use]
    pub fn protection(&self) -> Protection {
        let hardware_keys = self.hardware_keys();
        let backup_codes_left = self.backup_codes_left();
        let unlocks = |tier: Tier| {
            self.wraps()
                .iter()
                .any(|wrap| wrap.factor.tier() == Some(tier))
        };
        let warnings = match (self.tier(), hardware_keys.len()) {
            (Tier::HardwareKey, 0) => vec![Warning::NoHardwareKey { backup_codes_left }],
            (Tier::HardwareKey, 1) => vec![Warning::OneHardwareKey { backup_codes_left }],
            _ => Vec::new(),
        };
        Protection {
            tier: self.tier(),
            hardware_keys,
            totp: unlocks(Tier::Totp),
            file: unlocks(Tier::File),
            backup_codes_left,
            warnings,
        }
    }

    /// Adds the security key `device` to a vault at the hardware-key
    /// tier, named `label`. Needs that tier's unlock: another enrolled key,
    /// or a backup code (which may restore the vault's protection).
    ///
    /// # Errors
    ///
    /// Returns [`Error::KeyChange`] if the vault is not at the hardware-key
    /// tier or the key's credential is already enrolled,
    /// [`Error::TierChange`] if `unlocked` is not this vault's current-tier
    /// unlock, or [`Error::MissingHmacSecret`], [`Error::Authenticator`],
    /// or an I/O or randomness error; the keyring is then unchanged.
    pub fn add_hardware_key(
        &mut self,
        unlocked: &Unlocked,
        device: &mut dyn Authenticator,
        label: &Label,
    ) -> Result<KeyInfo, Error> {
        if self.tier() != Tier::HardwareKey {
            return Err(Error::KeyChange {
                reason: "keys are added to a vault at the hardware-key tier; enroll that tier first, with two keys or one and backup codes",
            });
        }
        self.authorize(unlocked, Tier::HardwareKey)?;
        let mut wrap = self.hardware_wrap(unlocked.key(), device)?;
        let credential = credential_of(&wrap);
        if self
            .wraps()
            .iter()
            .any(|enrolled| is_hardware_key(enrolled) && credential_of(enrolled) == credential)
        {
            return Err(Error::KeyChange {
                reason: "that credential is already enrolled",
            });
        }
        wrap.set_label(label.as_str());
        let added = KeyInfo::of(&wrap);
        let mut wraps = self.wraps().to_vec();
        wraps.push(wrap);
        self.store_wraps(wraps)?;
        Ok(added)
    }

    /// Removes the security key whose id is `id`, with an unlock that did
    /// not come from that key: another enrolled key, or a backup code.
    /// The last key is never removed. Leaving one key needs backup codes
    /// and the explicit `risk`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoSuchKey`] if no enrolled key has that id,
    /// [`Error::KeyChange`] if `unlocked` came from that key, if it is the
    /// last key, or if one key would be left without `risk`,
    /// [`Error::NeedBackupCodes`] if one key would be left with no backup
    /// codes, [`Error::TierChange`] if `unlocked` is not this vault's
    /// current-tier unlock, or an I/O error; the keyring is then unchanged.
    pub fn remove_hardware_key(
        &mut self,
        unlocked: &Unlocked,
        id: &str,
        risk: Option<SingleKeyRisk>,
    ) -> Result<KeyInfo, Error> {
        let (removed, wraps) = self.removal(unlocked, id, risk, None)?;
        self.store_wraps(wraps)?;
        Ok(removed)
    }

    /// Removes the security key whose id is `id`, authorized by the
    /// backup code `code`, which is spent only if the key goes. Every
    /// refusal is decided against the keyring as it would be after the
    /// spend, before anything is written, and the code and the key then
    /// leave in one write: a refused removal never costs a code (
    /// flag 5).
    ///
    /// # Errors
    ///
    /// As [`Keyring::remove_hardware_key`], and [`Error::UnlockFailed`] if
    /// `code` is not one of the vault's unused codes. [`Error::KeyChange`]
    /// also when `code` is the last one and one key would be left: the
    /// code and the keyring are then kept.
    pub fn remove_hardware_key_with_code(
        &mut self,
        code: &str,
        id: &str,
        risk: Option<SingleKeyRisk>,
    ) -> Result<KeyInfo, Error> {
        let (index, unlocked) = self.verify_code(code)?;
        let (removed, wraps) = self.removal(&unlocked, id, risk, Some(index))?;
        self.store_wraps(wraps)?;
        Ok(removed)
    }

    /// Decides a removal without writing anything: the key removed, and
    /// the wraps to store. `spent` is the index of a backup code's wrap
    /// that authorized it, which leaves with the key; every count is taken
    /// as it will be after that.
    fn removal(
        &self,
        unlocked: &Unlocked,
        id: &str,
        risk: Option<SingleKeyRisk>,
        spent: Option<usize>,
    ) -> Result<(KeyInfo, Vec<Wrap>), Error> {
        self.authorize(unlocked, Tier::HardwareKey)?;
        let keys = self.hardware_keys();
        let removed = keys
            .iter()
            .find(|key| key.id == id)
            .cloned()
            .ok_or(Error::NoSuchKey)?;
        if unlocked.opened_by() == Some(id) {
            return Err(Error::KeyChange {
                reason: "a key cannot authorize its own removal: unlock with another enrolled key or a backup code",
            });
        }
        match keys.len() {
            0 | 1 => {
                return Err(Error::KeyChange {
                    reason: "the last security key cannot be removed: change the vault's tier first, with its own unlock",
                });
            }
            2 => {
                let codes_left = self
                    .backup_codes_left()
                    .saturating_sub(usize::from(spent.is_some()));
                if codes_left == 0 {
                    return Err(if spent.is_some() {
                        Error::KeyChange {
                            reason: "your last backup code cannot authorize this: spending it would leave one security key and no backup codes. Authorize with another enrolled key instead; the code was not spent",
                        }
                    } else {
                        Error::NeedBackupCodes
                    });
                }
                if risk.is_none() {
                    return Err(Error::KeyChange {
                        reason: "removing it leaves one security key: accept that risk explicitly",
                    });
                }
            }
            _ => {}
        }
        let wraps = self
            .wraps()
            .iter()
            .enumerate()
            .filter(|(index, wrap)| wrap.id != id && Some(*index) != spent)
            .map(|(_, wrap)| wrap.clone())
            .collect();
        Ok((removed, wraps))
    }

    /// Names the security key whose id is `id` `label`. Needs the
    /// current tier's unlock: every change to the keyring takes one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NoSuchKey`] if no enrolled key has that id,
    /// [`Error::TierChange`] if `unlocked` is not this vault's
    /// current-tier unlock, or an I/O error.
    pub fn label_key(&mut self, unlocked: &Unlocked, id: &str, label: &Label) -> Result<(), Error> {
        self.authorize(unlocked, self.tier())?;
        let mut wraps = self.wraps().to_vec();
        wraps
            .iter_mut()
            .find(|wrap| wrap.id == id && is_hardware_key(wrap))
            .ok_or(Error::NoSuchKey)?
            .set_label(label.as_str());
        self.store_wraps(wraps)
    }
}

/// A hardware wrap's credential id; empty for any other kind.
fn credential_of(wrap: &Wrap) -> String {
    match &wrap.factor {
        Factor::Fido2 { credential, .. } => credential.clone(),
        Factor::File | Factor::Totp { .. } | Factor::BackupCode { .. } => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    use hmac::{Hmac, Mac as _};
    use sha2::Sha256;

    use super::*;
    use crate::fido2::{AuthenticatorError, Credential};
    use crate::keyring::{Kek, now};
    use crate::seal::MasterKey;
    use crate::testutil::{settle, test_dir};
    use crate::virtual_key::VirtualAuthenticator;

    /// A genuine keyring, written by the unchanged base code
    /// (format 1): the hardware-key tier, keys A and B (deterministic, see
    /// [`FixedKey`]), and ten backup codes.
    const WO055_KEYRING: &str = include_str!("../tests/fixtures/wo055-keyring-v1.json");
    const WO055_KEY_A: &str = "f076cdaa8d13c13c";
    const WO055_KEY_B: &str = "8e872d671774ff20";
    /// One of the fixture's backup codes.
    const WO055_CODE: &str = "YGWA-ABEA-PHBR-G7DL-TBNV-UKDF";
    /// The fixture's DEK.
    const WO055_DEK: [u8; 32] = [0x5a; 32];

    /// The deterministic security key the fixture was made with: one
    /// credential, `hmac-secret` is `HMAC-SHA-256(secret, salt)`. It
    /// issues the same credential every time, as a broken key might.
    struct FixedKey {
        id: [u8; 16],
        secret: [u8; 32],
    }

    impl FixedKey {
        const fn a() -> Self {
            Self {
                id: [0xa1; 16],
                secret: [0x11; 32],
            }
        }

        const fn b() -> Self {
            Self {
                id: [0xb2; 16],
                secret: [0x22; 32],
            }
        }
    }

    impl Authenticator for FixedKey {
        fn make_credential(
            &mut self,
            _rp_id: &str,
            _user_id: &[u8],
        ) -> Result<Credential, AuthenticatorError> {
            Ok(Credential {
                id: self.id.to_vec(),
                hmac_secret: true,
            })
        }

        fn hmac_secret(
            &mut self,
            _rp_id: &str,
            credential_id: &[u8],
            salt: &[u8; 32],
        ) -> Result<[u8; 32], AuthenticatorError> {
            if credential_id != self.id {
                return Err(AuthenticatorError::UnknownCredential);
            }
            let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).unwrap();
            mac.update(salt);
            Ok(mac.finalize().into_bytes().into())
        }
    }

    /// `text` as the keyring `dir/keys/keyring`, owner-only.
    fn place(dir: &Path, text: &str) -> PathBuf {
        let keys = dir.join("keys");
        std::fs::create_dir_all(&keys).unwrap();
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = keys.join("keyring");
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path
    }

    /// A keyring of one `tier` holding `wraps` (JSON objects, joined).
    fn stored(tier: &str, wraps: &[&str]) -> String {
        [
            r#"{"format": 2, "vault_id": ""#,
            &"ab".repeat(16),
            r#"", "tier": ""#,
            tier,
            r#"", "wraps": ["#,
            &wraps.join(","),
            "]}",
        ]
        .concat()
    }

    const TOTP_WRAP: &str = r#"{"id": "t1", "factor": {"kind": "totp", "seed": "00", "salt": "00", "last_step": 0}, "nonce": "00", "wrapped": "00"}"#;
    const CODE_WRAP: &str = r#"{"id": "c1", "factor": {"kind": "backup_code", "verifier": "00", "salt": "00"}, "nonce": "00", "wrapped": "00"}"#;

    /// A vault at the hardware-key tier with `count` software keys, and
    /// ten backup codes made first if `codes`.
    fn hardware_vault(
        dir: &Path,
        count: usize,
        codes: bool,
    ) -> (Keyring, Vec<VirtualAuthenticator>, Vec<String>) {
        let dek = MasterKey::generate().unwrap();
        let kek = Kek::from_bytes([7; 32]);
        let mut keyring = Keyring::create(&dir.join("keys").join("keyring"), &dek, &kek).unwrap();
        let unlocked = keyring.unlock_file(&kek).unwrap();
        if codes {
            keyring.generate_backup_codes(&unlocked).unwrap();
        }
        let mut keys: Vec<VirtualAuthenticator> =
            (0..count).map(|_| VirtualAuthenticator::new()).collect();
        let mut devices: Vec<&mut dyn Authenticator> = keys
            .iter_mut()
            .map(|key| -> &mut dyn Authenticator { key })
            .collect();
        let rotated = keyring
            .enroll_hardware_keys(&unlocked, &mut devices)
            .unwrap();
        // The enrollment's new key brings the codes to use.
        let made = settle(&mut keyring, rotated, &dir.join("vault"));
        (keyring, keys, made)
    }

    /// An unlock from another vault.
    fn foreign(dir: &Path) -> Unlocked {
        let kek = Kek::from_bytes([9; 32]);
        Keyring::create(
            &dir.join("other").join("keyring"),
            &MasterKey::generate().unwrap(),
            &kek,
        )
        .unwrap()
        .unlock_file(&kek)
        .unwrap()
    }

    fn ids(keyring: &Keyring) -> Vec<String> {
        keyring
            .hardware_keys()
            .into_iter()
            .map(|key| key.id)
            .collect()
    }

    fn refused_because(result: &Result<KeyInfo, Error>, start: &str) -> bool {
        matches!(result, Err(Error::KeyChange { reason }) if reason.starts_with(start))
    }

    #[test]
    fn a_wo055_keyring_loads_unchanged_and_is_written_as_format_2_when_next_saved() {
        let dir = test_dir("manage-wo055");
        let path = place(&dir, WO055_KEYRING);
        let mut keyring = Keyring::load(&path).unwrap();
        assert_eq!(keyring.tier(), Tier::HardwareKey);
        let unlabelled = |id: &str| KeyInfo {
            id: id.to_owned(),
            label: String::new(),
            added: None,
        };
        assert_eq!(
            keyring.hardware_keys(),
            [unlabelled(WO055_KEY_A), unlabelled(WO055_KEY_B)]
        );
        assert_eq!(keyring.backup_codes_left(), 10);
        assert!(
            keyring
                .wraps()
                .iter()
                .all(|wrap| wrap.label().is_empty() && wrap.added().is_none())
        );
        let original: Vec<(String, Factor)> = keyring
            .wraps()
            .iter()
            .map(|wrap| (wrap.id.clone(), wrap.factor.clone()))
            .collect();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            WO055_KEYRING,
            "loading writes nothing"
        );
        // Each key opens it, and the unlock says which.
        let by_a = keyring.unlock_hardware_key(&mut FixedKey::a()).unwrap();
        assert_eq!(
            (by_a.key().expose(), by_a.opened_by()),
            (&WO055_DEK, Some(WO055_KEY_A))
        );
        let by_b = keyring.unlock_hardware_key(&mut FixedKey::b()).unwrap();
        assert_eq!(by_b.opened_by(), Some(WO055_KEY_B));
        // The next save writes the current format, with every wrap as it was.
        keyring
            .label_key(&by_a, WO055_KEY_B, &"YubiKey 5C, spare".parse().unwrap())
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"format\": 3") && text.contains("\"label\": \"YubiKey 5C, spare\"")
        );
        let mut migrated = Keyring::load(&path).unwrap();
        let kept: Vec<(String, Factor)> = migrated
            .wraps()
            .iter()
            .map(|wrap| (wrap.id.clone(), wrap.factor.clone()))
            .collect();
        assert_eq!(kept, original);
        let spare = KeyInfo {
            label: String::from("YubiKey 5C, spare"),
            ..unlabelled(WO055_KEY_B)
        };
        assert_eq!(migrated.hardware_keys(), [unlabelled(WO055_KEY_A), spare]);
        for mut key in [FixedKey::a(), FixedKey::b()] {
            assert_eq!(
                migrated
                    .unlock_hardware_key(&mut key)
                    .unwrap()
                    .key()
                    .expose(),
                &WO055_DEK
            );
        }
        assert_eq!(
            migrated.recover(WO055_CODE).unwrap().key().expose(),
            &WO055_DEK
        );
        assert_eq!(migrated.backup_codes_left(), 9);
    }

    #[test]
    fn the_vault_always_shows_what_protects_it() {
        let dir = test_dir("manage-protection");
        let kek = Kek::from_bytes([3; 32]);
        let mut keyring = Keyring::create(
            &dir.join("keys").join("keyring"),
            &MasterKey::generate().unwrap(),
            &kek,
        )
        .unwrap();
        assert_eq!(
            keyring.protection(),
            Protection {
                tier: Tier::File,
                hardware_keys: Vec::new(),
                totp: false,
                file: true,
                backup_codes_left: 0,
                warnings: Vec::new(),
            }
        );
        let unlocked = keyring.unlock_file(&kek).unwrap();
        assert_eq!(unlocked.opened_by(), None, "no security key opened it");
        keyring.generate_backup_codes(&unlocked).unwrap();
        let (mut first, mut second) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let before = now();
        keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut first, &mut second])
            .unwrap();
        let after = now();
        let shown = keyring.protection();
        assert_eq!(
            (
                shown.tier,
                shown.totp,
                shown.file,
                shown.backup_codes_left,
                shown.warnings.as_slice()
            ),
            (Tier::HardwareKey, false, false, 10, [].as_slice())
        );
        let enrolled: Vec<KeyInfo> = keyring
            .wraps()
            .iter()
            .filter(|wrap| is_hardware_key(wrap))
            .map(KeyInfo::of)
            .collect();
        assert_eq!(shown.hardware_keys, enrolled);
        assert_eq!(enrolled.len(), 2);
        assert!(enrolled.iter().all(|key| key.label.is_empty()
            && key.added.is_some_and(|at| (before..=after).contains(&at))));
        // One key left is said plainly, with the codes that remain.
        let by_first = keyring.unlock_hardware_key(&mut first).unwrap();
        let second_id = enrolled.last().unwrap().id.clone();
        keyring
            .remove_hardware_key(&by_first, &second_id, Some(SingleKeyRisk::accepted()))
            .unwrap();
        assert_eq!(
            keyring.protection().warnings,
            [Warning::OneHardwareKey {
                backup_codes_left: 10
            }]
        );
        // A hardware-key vault with no key (nothing here makes one) says so.
        let bare = Keyring::load(&place(
            &dir.join("bare"),
            &stored("hardware_key", &[CODE_WRAP]),
        ))
        .unwrap();
        assert_eq!(
            bare.protection().warnings,
            [Warning::NoHardwareKey {
                backup_codes_left: 1
            }]
        );
        // An authenticator app is shown, with no warning.
        let app = Keyring::load(&place(&dir.join("app"), &stored("totp", &[TOTP_WRAP]))).unwrap();
        assert_eq!(
            app.protection(),
            Protection {
                tier: Tier::Totp,
                hardware_keys: Vec::new(),
                totp: true,
                file: false,
                backup_codes_left: 0,
                warnings: Vec::new(),
            }
        );
    }

    #[test]
    fn a_key_is_added_to_a_hardware_key_vault_with_its_unlock_label_and_date() {
        let dir = test_dir("manage-add");
        let (mut keyring, mut keys, codes) = hardware_vault(&dir, 2, true);
        let desk: Label = "YubiKey 5C, desk".parse().unwrap();
        let by_first = keyring
            .unlock_hardware_key(keys.first_mut().unwrap())
            .unwrap();
        let mut third = VirtualAuthenticator::new();
        let before = now();
        let added = keyring
            .add_hardware_key(&by_first, &mut third, &desk)
            .unwrap();
        assert_eq!(added.label, "YubiKey 5C, desk");
        assert!(added.added.is_some_and(|at| at >= before && at <= now()));
        assert_eq!(keyring.hardware_keys().last(), Some(&added));
        assert_eq!(
            keyring.unlock_hardware_key(&mut third).unwrap().opened_by(),
            Some(added.id.as_str())
        );
        assert_eq!(
            Keyring::load(&dir.join("keys").join("keyring"))
                .unwrap()
                .hardware_keys(),
            keyring.hardware_keys(),
            "saved"
        );
        // The recovery drill: a key lost, a backup code, a new key.
        let recovered = keyring.recover(codes.first().unwrap()).unwrap();
        let mut replacement = VirtualAuthenticator::new();
        keyring
            .add_hardware_key(
                &recovered,
                &mut replacement,
                &"replacement".parse().unwrap(),
            )
            .unwrap();
        assert!(keyring.unlock_hardware_key(&mut replacement).is_ok());
        // A key without hmac-secret, or a credential already enrolled, adds nothing.
        let mut fixed = FixedKey::a();
        keyring
            .add_hardware_key(&by_first, &mut fixed, &desk)
            .unwrap();
        let held = keyring.wraps().to_vec();
        assert!(matches!(
            keyring.add_hardware_key(
                &by_first,
                &mut VirtualAuthenticator::without_hmac_secret(),
                &desk
            ),
            Err(Error::MissingHmacSecret)
        ));
        assert!(refused_because(
            &keyring.add_hardware_key(&by_first, &mut fixed, &desk),
            "that credential is already enrolled"
        ));
        // Another vault's unlock adds nothing.
        assert!(matches!(
            keyring.add_hardware_key(&foreign(&dir), &mut VirtualAuthenticator::new(), &desk),
            Err(Error::TierChange { .. })
        ));
        assert_eq!(keyring.wraps(), held.as_slice());
        // Below the hardware-key tier a key is not added: the tier is enrolled.
        let kek = Kek::from_bytes([5; 32]);
        let mut low = Keyring::create(
            &dir.join("low").join("keyring"),
            &MasterKey::generate().unwrap(),
            &kek,
        )
        .unwrap();
        let low_unlock = low.unlock_file(&kek).unwrap();
        assert!(refused_because(
            &low.add_hardware_key(&low_unlock, &mut VirtualAuthenticator::new(), &desk),
            "keys are added to a vault at the hardware-key tier"
        ));
        assert_eq!((low.tier(), low.hardware_keys().len()), (Tier::File, 0));
    }

    #[test]
    fn a_key_never_authorizes_its_own_removal() {
        let dir = test_dir("manage-own-removal");
        let (mut keyring, mut keys, codes) = hardware_vault(&dir, 3, true);
        let enrolled = ids(&keyring);
        let ([first, second, third], [k1, k2, k3]) = (enrolled.as_slice(), keys.as_mut_slice())
        else {
            unreachable!("three keys");
        };
        let by_first = keyring.unlock_hardware_key(k1).unwrap();
        let held = keyring.wraps().to_vec();
        assert!(refused_because(
            &keyring.remove_hardware_key(&by_first, first, Some(SingleKeyRisk::accepted())),
            "a key cannot authorize its own removal"
        ));
        assert_eq!(keyring.wraps(), held.as_slice());
        // Another key may remove it, and it then opens nothing.
        let by_second = keyring.unlock_hardware_key(k2).unwrap();
        assert_eq!(
            keyring
                .remove_hardware_key(&by_second, first, None)
                .unwrap()
                .id,
            *first
        );
        assert!(matches!(
            keyring.unlock_hardware_key(k1),
            Err(Error::UnlockFailed)
        ));
        assert!(keyring.unlock_hardware_key(k3).is_ok());
        // So may a backup code, which no key made.
        let recovered = keyring.recover(codes.first().unwrap()).unwrap();
        assert_eq!(recovered.opened_by(), None);
        keyring
            .remove_hardware_key(&recovered, second, Some(SingleKeyRisk::accepted()))
            .unwrap();
        assert_eq!(ids(&keyring), std::slice::from_ref(third));
        assert_eq!(
            ids(&Keyring::load(&dir.join("keys").join("keyring")).unwrap()),
            std::slice::from_ref(third),
            "saved"
        );
    }

    #[test]
    fn removal_never_lowers_the_protection_or_leaves_one_key_unknowingly() {
        let dir = test_dir("manage-last-key");
        let (mut keyring, mut keys, codes) = hardware_vault(&dir, 2, true);
        let enrolled = ids(&keyring);
        let ([first, second], [k1, _]) = (enrolled.as_slice(), keys.as_mut_slice()) else {
            unreachable!("two keys");
        };
        let by_first = keyring.unlock_hardware_key(k1).unwrap();
        let held = keyring.wraps().to_vec();
        assert!(refused_because(
            &keyring.remove_hardware_key(&by_first, second, None),
            "removing it leaves one security key"
        ));
        assert_eq!(keyring.wraps(), held.as_slice());
        keyring
            .remove_hardware_key(&by_first, second, Some(SingleKeyRisk::accepted()))
            .unwrap();
        assert_eq!(keyring.tier(), Tier::HardwareKey);
        // The last key: never, whoever asks.
        let recovered = keyring.recover(codes.first().unwrap()).unwrap();
        let held = keyring.wraps().to_vec();
        assert!(refused_because(
            &keyring.remove_hardware_key(&recovered, first, Some(SingleKeyRisk::accepted())),
            "the last security key cannot be removed"
        ));
        assert_eq!(
            (keyring.wraps(), keyring.tier()),
            (held.as_slice(), Tier::HardwareKey)
        );
        assert!(keyring.unlock_hardware_key(k1).is_ok());
        // Keys are named by id, and only keys.
        let code = keyring
            .wraps()
            .iter()
            .find(|wrap| wrap.factor.tier().is_none())
            .unwrap()
            .id
            .clone();
        for id in ["", "nope", code.as_str()] {
            assert!(matches!(
                keyring.remove_hardware_key(&recovered, id, Some(SingleKeyRisk::accepted())),
                Err(Error::NoSuchKey)
            ));
        }
        // Another vault's unlock removes nothing.
        assert!(matches!(
            keyring.remove_hardware_key(&foreign(&dir), first, Some(SingleKeyRisk::accepted())),
            Err(Error::TierChange { .. })
        ));
        // One key with no backup codes: never.
        let bare_dir = dir.join("bare");
        std::fs::create_dir_all(&bare_dir).unwrap();
        let (mut bare, mut bare_keys, _) = hardware_vault(&bare_dir, 2, false);
        let bare_ids = ids(&bare);
        let by_key = bare
            .unlock_hardware_key(bare_keys.first_mut().unwrap())
            .unwrap();
        let held = bare.wraps().to_vec();
        assert!(matches!(
            bare.remove_hardware_key(
                &by_key,
                bare_ids.last().unwrap(),
                Some(SingleKeyRisk::accepted())
            ),
            Err(Error::NeedBackupCodes)
        ));
        assert_eq!(bare.wraps(), held.as_slice());
    }

    #[test]
    fn a_backup_code_is_spent_only_by_a_removal_that_happens() {
        let dir = test_dir("manage-code-removal");
        let path = dir.join("keys").join("keyring");
        let (mut keyring, mut keys, codes) = hardware_vault(&dir, 3, true);
        let enrolled = ids(&keyring);
        let ([first, second, third], [k1, _, _]) = (enrolled.as_slice(), keys.as_mut_slice())
        else {
            unreachable!("three keys");
        };
        let code = |n: usize| codes.get(n).unwrap().as_str();
        // A removal that happens spends its code, with the key, in one write.
        let removed = keyring
            .remove_hardware_key_with_code(code(0), third, None)
            .unwrap();
        assert_eq!(&removed.id, third);
        assert_eq!(ids(&keyring), [first.clone(), second.clone()]);
        assert_eq!(keyring.backup_codes_left(), 9);
        let on_disk = Keyring::load(&path).unwrap();
        assert_eq!(on_disk.wraps(), keyring.wraps());
        assert!(matches!(keyring.recover(code(0)), Err(Error::UnlockFailed)));
        // Every refusal keeps the code, on disk too.
        let held = keyring.wraps().to_vec();
        let refusals = [
            keyring.remove_hardware_key_with_code(code(1), second, None),
            keyring.remove_hardware_key_with_code(code(1), "nope", Some(SingleKeyRisk::accepted())),
            keyring.remove_hardware_key_with_code("AAAA-AAAA-AAAA-AAAA-AAAA-AAAA", second, None),
        ];
        assert!(refused_because(
            refusals.first().unwrap(),
            "removing it leaves one security key"
        ));
        assert!(matches!(refusals.get(1).unwrap(), Err(Error::NoSuchKey)));
        assert!(matches!(refusals.get(2).unwrap(), Err(Error::UnlockFailed)));
        assert_eq!(keyring.wraps(), held.as_slice());
        assert_eq!(Keyring::load(&path).unwrap().wraps(), held.as_slice());
        // Down to the last code: it cannot authorize leaving one key with
        // none, and it is still there afterwards.
        for n in 1..9 {
            keyring.recover(code(n)).unwrap();
        }
        assert_eq!(keyring.backup_codes_left(), 1);
        let held = keyring.wraps().to_vec();
        assert!(refused_because(
            &keyring.remove_hardware_key_with_code(
                code(9),
                second,
                Some(SingleKeyRisk::accepted())
            ),
            "your last backup code cannot authorize this"
        ));
        assert_eq!(keyring.wraps(), held.as_slice());
        assert_eq!(Keyring::load(&path).unwrap().wraps(), held.as_slice());
        // Deciding writes nothing; another key may still remove it, and
        // the last code is kept.
        let by_first = keyring.unlock_hardware_key(k1).unwrap();
        let (decided, wraps) = keyring
            .removal(&by_first, second, Some(SingleKeyRisk::accepted()), None)
            .unwrap();
        assert_eq!(&decided.id, second);
        assert_eq!(wraps.len(), held.len() - 1);
        assert_eq!(keyring.wraps(), held.as_slice());
        keyring
            .remove_hardware_key(&by_first, second, Some(SingleKeyRisk::accepted()))
            .unwrap();
        assert_eq!(keyring.wraps(), wraps.as_slice());
        assert_eq!(keyring.backup_codes_left(), 1);
        assert!(keyring.recover(code(9)).is_ok());
    }

    #[test]
    fn labels_are_shown_and_never_used_to_decide_anything() {
        let dir = test_dir("manage-labels");
        let (mut keyring, mut keys, _) = hardware_vault(&dir, 3, true);
        let enrolled = ids(&keyring);
        let ([a, b, c], [_, _, kc]) = (enrolled.as_slice(), keys.as_mut_slice()) else {
            unreachable!("three keys");
        };
        let by_c = keyring.unlock_hardware_key(kc).unwrap();
        let same: Label = "YubiKey".parse().unwrap();
        keyring.label_key(&by_c, a, &same).unwrap();
        keyring.label_key(&by_c, b, &same).unwrap();
        // A label that reads as another key's id is still only a label.
        keyring.label_key(&by_c, c, &a.parse().unwrap()).unwrap();
        let labels: Vec<String> = keyring
            .hardware_keys()
            .into_iter()
            .map(|key| key.label)
            .collect();
        assert_eq!(labels, ["YubiKey", "YubiKey", a.as_str()]);
        assert_eq!(
            Keyring::load(&dir.join("keys").join("keyring"))
                .unwrap()
                .hardware_keys(),
            keyring.hardware_keys(),
            "saved"
        );
        // A label names nothing.
        for name in ["YubiKey", "yubikey"] {
            assert!(matches!(
                keyring.remove_hardware_key(&by_c, name, None),
                Err(Error::NoSuchKey)
            ));
            assert!(matches!(
                keyring.label_key(&by_c, name, &same),
                Err(Error::NoSuchKey)
            ));
        }
        // c, labelled as a, may remove a: the label is not the key that opened it...
        assert_eq!(keyring.remove_hardware_key(&by_c, a, None).unwrap().id, *a);
        // ...and may not remove itself, whatever its label says.
        assert!(refused_because(
            &keyring.remove_hardware_key(&by_c, c, Some(SingleKeyRisk::accepted())),
            "a key cannot authorize its own removal"
        ));
        // Labels are for keys only, and need this vault's unlock.
        let code = keyring
            .wraps()
            .iter()
            .find(|wrap| wrap.factor.tier().is_none())
            .unwrap()
            .id
            .clone();
        assert!(matches!(
            keyring.label_key(&by_c, &code, &same),
            Err(Error::NoSuchKey)
        ));
        assert!(matches!(
            keyring.label_key(&foreign(&dir), b, &same),
            Err(Error::TierChange { .. })
        ));
        let mut wrap = keyring.wraps().first().unwrap().clone();
        wrap.set_label("renamed");
        assert_eq!(
            (wrap.label(), credential_of(&wrap).is_empty()),
            ("renamed", false)
        );
        assert_eq!(
            credential_of(&keyring.wraps().last().unwrap().clone()),
            "",
            "a code has no credential"
        );
    }

    #[test]
    fn a_label_is_trimmed_bounded_and_printable() {
        assert_eq!(MAX_LABEL_CHARS, 64);
        assert_eq!(
            "  desk key  ".parse::<Label>().unwrap().as_str(),
            "desk key"
        );
        let wide = "\u{e9}".repeat(MAX_LABEL_CHARS);
        assert_eq!(
            wide.parse::<Label>().unwrap().as_str(),
            wide,
            "characters, not bytes"
        );
        for (raw, reason) in [
            (String::new(), "a label cannot be empty"),
            (String::from(" \t "), "a label cannot be empty"),
            (
                "x".repeat(MAX_LABEL_CHARS + 1),
                "a label is at most 64 characters",
            ),
            (
                String::from("desk\nkey"),
                "a label cannot contain control characters",
            ),
            (
                String::from("desk\u{7}"),
                "a label cannot contain control characters",
            ),
        ] {
            assert!(
                matches!(raw.parse::<Label>(), Err(Error::InvalidLabel { reason: found }) if found == reason),
                "{raw:?}"
            );
        }
    }
}
