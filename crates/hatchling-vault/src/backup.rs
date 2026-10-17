//! Backup codes ( phase D): the way back when the usual factor is
//! lost.
//!
//! Each code is 120 random bits, shown once as six groups of four Base32
//! characters. At rest the keyring holds, per code, a verifier
//! (`HKDF-SHA-256` of the code, to recognise it) and a wrap of the DEK under
//! a KEK derived from the code with different HKDF info; never the code.
//! A code recovers the vault once: its wrap is deleted, and the keyring
//! saved, before the DEK is returned.
//!
//! A recovered unlock may restore the vault's protection (re-enroll new
//! keys) or raise it, never lower it: a stolen code cannot downgrade the
//! vault. Codes survive tier changes, since they wrap the same DEK.
//! Generating a new set replaces the old one.

use hkdf::Hkdf;
use sha2::Sha256;
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::Error;
use crate::keyring::{Factor, Kek, Keyring, Unlocked, hex, random, unhex};
use crate::totp::base32;

/// How many codes a set holds.
pub const CODE_COUNT: usize = 10;
/// Random bytes per code: 120 bits, 24 Base32 characters.
const CODE_BYTES: usize = 15;
/// HKDF info for a code's verifier, and for its KEK.
const VERIFY_INFO: &[u8] = b"hatchling vault backup code: verifier v1";
const KEK_INFO: &[u8] = b"hatchling vault backup code: kek v1";
/// The Base32 alphabet, for reading codes back.
const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// A new set of backup codes, to show once.
pub struct BackupCodes(Vec<Zeroizing<String>>);

impl BackupCodes {
    /// The codes, to show the user once and store nowhere.
    pub fn codes(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|code| code.as_str())
    }
}

impl std::fmt::Debug for BackupCodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BackupCodes([{} redacted])", self.0.len())
    }
}

/// A code as shown: Base32, in groups of four. Every buffer is sized up
/// front and the plain text wiped, so no copy of the code is freed.
fn shown(bytes: &[u8]) -> String {
    let text = Zeroizing::new(base32(bytes));
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for (at, chunk) in text.as_bytes().chunks(4).enumerate() {
        if at > 0 {
            out.push('-');
        }
        out.extend(chunk.iter().copied().map(char::from));
    }
    out
}

/// A typed code back to its bytes: case, spaces and dashes ignored.
fn read(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    // Sized up front and wiped: the typed code, cleaned.
    let mut clean = Zeroizing::new(Vec::with_capacity(text.len()));
    clean.extend(
        text.bytes()
            .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'-')
            .map(|byte| byte.to_ascii_uppercase()),
    );
    if clean.len() != 24 {
        return None;
    }
    let mut out = Zeroizing::new(Vec::with_capacity(CODE_BYTES));
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in clean.iter() {
        let value = u32::try_from(ALPHABET.iter().position(|&letter| letter == byte)?).ok()?;
        // The shift empties the low five bits and a value is under 32:
        // adding is or-ing, written so its mutants are not equivalent.
        buffer = (buffer << 5) + value;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// `HKDF-SHA-256(ikm = code, salt = vault id || wrap salt, info)`.
fn derive(vault_id: &str, salt: &[u8], code: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut okm = Zeroizing::new([0u8; 32]);
    // HKDF-SHA-256 expands up to 8160 bytes; 32 always fits.
    let _ = Hkdf::<Sha256>::new(Some(&[vault_id.as_bytes(), salt].concat()), code)
        .expand(info, okm.as_mut_slice());
    okm
}

impl Keyring {
    /// A new set of backup codes, replacing any earlier set. Needs the
    /// current tier's unlock (or a recovered one).
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`], or an I/O or randomness error; the
    /// earlier set then still stands.
    pub fn generate_backup_codes(&mut self, unlocked: &Unlocked) -> Result<BackupCodes, Error> {
        self.authorize(unlocked, self.tier())?;
        let mut wraps: Vec<_> = self
            .wraps()
            .iter()
            .filter(|wrap| wrap.factor.tier().is_some())
            .cloned()
            .collect();
        let (code_wraps, codes) = self.fresh_backup_codes(unlocked.key())?;
        wraps.extend(code_wraps);
        self.store_wraps(wraps)?;
        Ok(codes)
    }

    /// A new set of backup codes for `dek`: their wraps, and the codes to
    /// show once. Nothing is stored.
    pub(crate) fn fresh_backup_codes(
        &self,
        dek: &crate::MasterKey,
    ) -> Result<(Vec<crate::Wrap>, BackupCodes), Error> {
        let mut wraps = Vec::with_capacity(CODE_COUNT);
        let mut codes = Vec::with_capacity(CODE_COUNT);
        for _ in 0..CODE_COUNT {
            let code = Zeroizing::new(random::<CODE_BYTES>()?);
            let salt = random::<16>()?;
            let verifier = derive(self.vault_id(), &salt, code.as_slice(), VERIFY_INFO);
            let kek = Kek::from_bytes(*derive(self.vault_id(), &salt, code.as_slice(), KEK_INFO));
            wraps.push(self.wrap(
                dek,
                &kek,
                Factor::BackupCode {
                    verifier: hex(verifier.as_slice()),
                    salt: hex(&salt),
                },
            )?);
            codes.push(Zeroizing::new(shown(code.as_slice())));
        }
        Ok((wraps, BackupCodes(codes)))
    }

    /// How many unused backup codes the vault has.
    #[must_use]
    pub fn backup_codes_left(&self) -> usize {
        self.wraps()
            .iter()
            .filter(|wrap| wrap.factor.tier().is_none())
            .count()
    }

    /// Recovers the vault with a backup code, burning it: its wrap is
    /// deleted and the keyring saved before the DEK is returned.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnlockFailed`] if the code is not one of the
    /// vault's unused codes, or an I/O error saving the burn, in which
    /// case the vault stays locked.
    pub fn recover(&mut self, code: &str) -> Result<Unlocked, Error> {
        let (index, unlocked) = self.verify_code(code)?;
        let mut wraps = self.wraps().to_vec();
        wraps.remove(index);
        self.store_wraps(wraps)?;
        Ok(unlocked)
    }

    /// Checks the backup code `code` without spending it: the index of
    /// its wrap in [`Keyring::wraps`], and the unlock it gives. The caller
    /// spends it by dropping that wrap in its own write.
    pub(crate) fn verify_code(&self, code: &str) -> Result<(usize, Unlocked), Error> {
        let code = read(code).ok_or(Error::UnlockFailed)?;
        let (index, dek) = self
            .wraps()
            .iter()
            .enumerate()
            .find_map(|(index, wrap)| {
                let Factor::BackupCode { verifier, salt } = &wrap.factor else {
                    return None;
                };
                let salt = unhex(salt)?;
                let expected = unhex(verifier)?;
                if !bool::from(
                    derive(self.vault_id(), &salt, &code, VERIFY_INFO).ct_eq(expected.as_slice()),
                ) {
                    return None;
                }
                let kek = Kek::from_bytes(*derive(self.vault_id(), &salt, &code, KEK_INFO));
                Some((index, self.unwrap(wrap, &kek).ok()?))
            })
            .ok_or(Error::UnlockFailed)?;
        Ok((
            index,
            Unlocked::recovered(dek, self.tier(), self.vault_id().to_owned()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::Tier;
    use crate::testutil::{settle, test_dir};
    use crate::totp::totp_code;
    use crate::virtual_key::VirtualAuthenticator;
    use crate::{MasterKey, SealedVault, Secret, SecretName, SingleKeyRisk};

    fn vault(dir: &std::path::Path) -> (Keyring, Kek) {
        let dek = MasterKey::generate().unwrap();
        let kek = Kek::from_bytes(random::<32>().unwrap());
        let name: SecretName = "token".parse().unwrap();
        SealedVault::open(&dir.join("vault"), &dek)
            .unwrap()
            .store(&name, &Secret::new(b"kept".to_vec()).unwrap())
            .unwrap();
        (
            Keyring::create(&dir.join("keys").join("keyring"), &dek, &kek).unwrap(),
            kek,
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
    fn codes_are_grouped_for_reading_and_each_derivation_is_its_own() {
        assert_eq!(shown(b"foobar"), "MZXW-6YTB-OI");
        assert_eq!(shown(&[]), "");
        let base = derive("v", b"s", b"code", VERIFY_INFO);
        assert_eq!(*base, *derive("v", b"s", b"code", VERIFY_INFO));
        for other in [
            derive("w", b"s", b"code", VERIFY_INFO),
            derive("v", b"t", b"code", VERIFY_INFO),
            derive("v", b"s", b"cod", VERIFY_INFO),
            derive("v", b"s", b"code", KEK_INFO),
        ] {
            assert_ne!(*base, *other);
        }
    }

    #[test]
    fn codes_are_shown_once_and_only_their_hashes_are_kept() {
        let dir = test_dir("backup-codes");
        let (mut keyring, kek) = vault(&dir);
        let codes = keyring
            .generate_backup_codes(&keyring.unlock_file(&kek).unwrap())
            .unwrap();
        let shown: Vec<String> = codes.codes().map(str::to_owned).collect();
        assert_eq!(shown.len(), CODE_COUNT);
        assert_eq!(format!("{codes:?}"), "BackupCodes([10 redacted])");
        for code in &shown {
            assert_eq!(code.len(), 29, "six groups of four: {code}");
            assert_eq!(code.split('-').count(), 6);
            assert!(
                code.bytes()
                    .all(|byte| byte == b'-' || ALPHABET.contains(&byte))
            );
        }
        assert_eq!(
            shown
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            CODE_COUNT,
            "all different"
        );
        let text = std::fs::read_to_string(dir.join("keys").join("keyring")).unwrap();
        for code in &shown {
            assert!(
                !text.contains(code.as_str()) && !text.contains(&hex(&read(code).unwrap())),
                "the code is not at rest"
            );
        }
        assert_eq!(keyring.backup_codes_left(), CODE_COUNT);
        assert_eq!(keyring.tier(), Tier::File, "codes are not a tier");
    }

    #[test]
    fn a_code_recovers_once_however_it_is_typed() {
        let dir = test_dir("backup-recover");
        let (mut keyring, kek) = vault(&dir);
        let codes = keyring
            .generate_backup_codes(&keyring.unlock_file(&kek).unwrap())
            .unwrap();
        let shown: Vec<String> = codes.codes().map(str::to_owned).collect();
        let first = shown.first().unwrap();
        let typed = format!(" {} ", first.replace('-', " ").to_lowercase());
        let recovered = keyring.recover(&typed).unwrap();
        assert!(recovered.is_recovery());
        assert_eq!(recovered.via(), Tier::File);
        assert!(reads_the_secret(&dir, recovered.key()));
        assert_eq!(keyring.backup_codes_left(), CODE_COUNT - 1);
        let mut reloaded = Keyring::load(&dir.join("keys").join("keyring")).unwrap();
        assert!(
            matches!(reloaded.recover(first), Err(Error::UnlockFailed)),
            "burned, even after a reload"
        );
        for bad in [
            "",
            "AAAA-AAAA",
            "AAAA-AAAA-AAAA-AAAA-AAAA-AAA1",
            "AAAA-AAAA-AAAA-AAAA-AAAA-AAAA",
        ] {
            assert!(
                matches!(reloaded.recover(bad), Err(Error::UnlockFailed)),
                "{bad:?}"
            );
        }
        assert_eq!(
            reloaded.backup_codes_left(),
            CODE_COUNT - 1,
            "a wrong code burns nothing"
        );
        assert!(reloaded.recover(shown.get(1).unwrap()).is_ok());
        assert!(!keyring.unlock_file(&kek).unwrap().is_recovery());
        // Checking a code spends nothing: it names its wrap, and the
        // unlock it gives, for the caller to spend in its own write.
        let third = shown.get(2).unwrap();
        let held = reloaded.wraps().to_vec();
        let (index, checked) = reloaded.verify_code(third).unwrap();
        assert!(checked.is_recovery() && reads_the_secret(&dir, checked.key()));
        assert_eq!(reloaded.wraps(), held.as_slice());
        let wrap = reloaded.wraps().get(index).unwrap();
        assert!(wrap.factor.tier().is_none(), "a backup code's wrap");
        assert!(matches!(
            reloaded.verify_code(first),
            Err(Error::UnlockFailed)
        ));
        let spent: Vec<_> = held
            .iter()
            .filter(|other| other.id != wrap.id)
            .cloned()
            .collect();
        reloaded.recover(third).unwrap();
        assert_eq!(
            reloaded.wraps(),
            spent.as_slice(),
            "recover spends exactly that wrap"
        );
    }

    #[test]
    fn a_new_set_replaces_the_old_and_a_tier_change_brings_a_new_set() {
        let dir = test_dir("backup-survive");
        let (mut keyring, kek) = vault(&dir);
        let from_file = keyring.unlock_file(&kek).unwrap();
        let old: Vec<String> = keyring
            .generate_backup_codes(&from_file)
            .unwrap()
            .codes()
            .map(str::to_owned)
            .collect();
        let fresh: Vec<String> = keyring
            .generate_backup_codes(&from_file)
            .unwrap()
            .codes()
            .map(str::to_owned)
            .collect();
        assert!(
            matches!(
                keyring.recover(old.first().unwrap()),
                Err(Error::UnlockFailed)
            ),
            "the old set is gone"
        );
        assert_eq!(keyring.backup_codes_left(), CODE_COUNT);
        // Move to TOTP: the codes still recover, at the new tier.
        let now = 1_700_000_000;
        let pending = keyring.begin_totp(&from_file).unwrap();
        let seed = pending
            .uri()
            .split("secret=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap()
            .to_owned();
        let seed = read_base32(&seed);
        let rotated = keyring
            .confirm_totp(&from_file, &pending, &totp_code(&seed, now), now)
            .unwrap();
        let renewed = settle(&mut keyring, rotated, &dir.join("vault"));
        assert_eq!(
            (keyring.backup_codes_left(), renewed.len()),
            (CODE_COUNT, CODE_COUNT)
        );
        assert!(
            matches!(
                keyring.recover(fresh.first().unwrap()),
                Err(Error::UnlockFailed)
            ),
            "the set from before the new key is gone"
        );
        let recovered = keyring.recover(renewed.first().unwrap()).unwrap();
        assert!(reads_the_secret(&dir, recovered.key()));
        assert_eq!(
            (recovered.via(), recovered.is_recovery()),
            (Tier::Totp, true)
        );
        // A recovered unlock cannot lower the tier, but can restore it.
        assert!(matches!(
            keyring.use_file_tier(&recovered, &kek),
            Err(Error::TierChange { .. })
        ));
        let again = keyring.begin_totp(&recovered).unwrap();
        let seed = read_base32(
            again
                .uri()
                .split("secret=")
                .nth(1)
                .unwrap()
                .split('&')
                .next()
                .unwrap(),
        );
        let rotated = keyring
            .confirm_totp(&recovered, &again, &totp_code(&seed, now + 60), now + 60)
            .unwrap();
        settle(&mut keyring, rotated, &dir.join("vault"));
        assert_eq!(keyring.tier(), Tier::Totp);
        // A wrong-tier proof cannot make codes either.
        assert!(matches!(
            keyring.generate_backup_codes(&from_file),
            Err(Error::TierChange { .. })
        ));
    }

    /// Base32 of any length back to bytes (the authenticator app's side).
    fn read_base32(text: &str) -> Vec<u8> {
        let (mut out, mut buffer, mut bits) = (Vec::new(), 0u32, 0u32);
        for byte in text.bytes() {
            buffer = (buffer << 5)
                | u32::try_from(ALPHABET.iter().position(|&letter| letter == byte).unwrap())
                    .unwrap();
            bits += 5;
            if bits >= 8 {
                bits -= 8;
                out.push(u8::try_from((buffer >> bits) & 0xff).unwrap());
            }
        }
        out
    }

    #[test]
    fn the_recovery_drill_lost_keys_backup_code_new_keys() {
        let dir = test_dir("backup-drill");
        let (mut keyring, kek) = vault(&dir);
        let from_file = keyring.unlock_file(&kek).unwrap();
        let codes: Vec<String> = keyring
            .generate_backup_codes(&from_file)
            .unwrap()
            .codes()
            .map(str::to_owned)
            .collect();
        let (mut primary, mut backup) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let rotated = keyring
            .enroll_hardware_keys(&from_file, &mut [&mut primary, &mut backup])
            .unwrap();
        let renewed = settle(&mut keyring, rotated, &dir.join("vault"));
        assert_eq!(
            keyring.backup_codes_left(),
            CODE_COUNT,
            "a new key, a new set of codes"
        );
        assert!(renewed.iter().all(|code| !codes.contains(code)));
        // Both keys lost.
        drop((primary, backup));
        let path = dir.join("keys").join("keyring");
        let mut keyring = Keyring::load(&path).unwrap();
        assert!(
            matches!(
                keyring.recover(codes.get(3).unwrap()),
                Err(Error::UnlockFailed)
            ),
            "the set from before the new key opens nothing"
        );
        let recovered = keyring.recover(renewed.get(3).unwrap()).unwrap();
        assert!(
            reads_the_secret(&dir, recovered.key()),
            "the secrets are back"
        );
        assert!(
            matches!(
                keyring.use_file_tier(&recovered, &kek),
                Err(Error::TierChange { .. })
            ),
            "a code cannot downgrade"
        );
        let (mut new_primary, mut new_backup) =
            (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        assert_eq!(
            keyring.backup_codes_left(),
            CODE_COUNT - 1,
            "the code used is burned"
        );
        let rotated = keyring
            .enroll_hardware_keys(&recovered, &mut [&mut new_primary, &mut new_backup])
            .unwrap();
        let again = settle(&mut keyring, rotated, &dir.join("vault"));
        let keyring = Keyring::load(&path).unwrap();
        assert_eq!(keyring.tier(), Tier::HardwareKey);
        assert!(reads_the_secret(
            &dir,
            keyring.unlock_hardware_key(&mut new_backup).unwrap().key()
        ));
        assert_eq!(
            (keyring.backup_codes_left(), again.len()),
            (CODE_COUNT, CODE_COUNT),
            "the new keys come with a whole new set"
        );
    }

    #[test]
    fn a_single_key_needs_backup_codes_first_and_a_deliberate_choice() {
        let dir = test_dir("backup-single");
        let (mut keyring, kek) = vault(&dir);
        let from_file = keyring.unlock_file(&kek).unwrap();
        let mut only = VirtualAuthenticator::new();
        assert!(matches!(
            keyring.enroll_single_hardware_key(&from_file, &mut only, SingleKeyRisk::accepted()),
            Err(Error::NeedBackupCodes)
        ));
        assert!(
            matches!(
                keyring.enroll_hardware_keys(&from_file, &mut [&mut only]),
                Err(Error::NeedTwoKeys)
            ),
            "the two-key path never takes one"
        );
        let codes: Vec<String> = keyring
            .generate_backup_codes(&from_file)
            .unwrap()
            .codes()
            .map(str::to_owned)
            .collect();
        let rotated = keyring
            .enroll_single_hardware_key(&from_file, &mut only, SingleKeyRisk::accepted())
            .unwrap();
        let renewed = settle(&mut keyring, rotated, &dir.join("vault"));
        assert_eq!(
            (keyring.tier(), keyring.wraps().len()),
            (Tier::HardwareKey, 1 + CODE_COUNT)
        );
        assert!(reads_the_secret(
            &dir,
            keyring.unlock_hardware_key(&mut only).unwrap().key()
        ));
        assert!(matches!(
            keyring.recover(codes.first().unwrap()),
            Err(Error::UnlockFailed)
        ));
        assert!(
            keyring.recover(renewed.first().unwrap()).is_ok(),
            "and the new codes are the way back"
        );
        let other = test_dir("backup-single-foreign");
        let (foreign, foreign_kek) = vault(&other);
        assert!(matches!(
            keyring.enroll_single_hardware_key(
                &foreign.unlock_file(&foreign_kek).unwrap(),
                &mut only,
                SingleKeyRisk::accepted()
            ),
            Err(Error::TierChange { .. })
        ));
    }

    #[test]
    fn a_fresh_set_wraps_the_key_given_and_stores_nothing() {
        let dir = test_dir("backup-fresh");
        let (keyring, _) = vault(&dir);
        let path = dir.join("keys").join("keyring");
        let before = std::fs::read(&path).unwrap();
        let other = MasterKey::generate().unwrap();
        let (wraps, codes) = keyring.fresh_backup_codes(&other).unwrap();
        assert_eq!(
            (wraps.len(), codes.codes().count()),
            (CODE_COUNT, CODE_COUNT)
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "nothing stored");
        assert!(
            wraps
                .iter()
                .all(|wrap| matches!(wrap.factor, Factor::BackupCode { .. }))
        );
    }

    #[test]
    fn two_edits_from_one_read_never_restore_a_spent_code() {
        let dir = test_dir("backup-race");
        let (mut keyring, kek) = vault(&dir);
        let codes: Vec<String> = keyring
            .generate_backup_codes(&keyring.unlock_file(&kek).unwrap())
            .unwrap()
            .codes()
            .map(str::to_owned)
            .collect();
        let path = dir.join("keys").join("keyring");
        // Two processes read the same keyring, each holding a code.
        let mut first = Keyring::load(&path).unwrap();
        let mut second = Keyring::load(&path).unwrap();
        assert!(first.recover(codes.first().unwrap()).is_ok());
        // The second's burn would write back the first's code: refused,
        // and the vault stays locked to it.
        assert!(matches!(
            second.recover(codes.get(1).unwrap()),
            Err(Error::KeyringChanged { .. })
        ));
        let mut fresh = Keyring::load(&path).unwrap();
        assert!(matches!(
            fresh.recover(codes.first().unwrap()),
            Err(Error::UnlockFailed)
        ));
        assert_eq!(fresh.backup_codes_left(), CODE_COUNT - 1);
        assert!(fresh.recover(codes.get(1).unwrap()).is_ok(), "read again, it works");
        assert_eq!(fresh.backup_codes_left(), CODE_COUNT - 2);
    }
}
