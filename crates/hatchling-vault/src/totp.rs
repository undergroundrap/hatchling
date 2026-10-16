//! The TOTP tier ( phase C): an authenticator app's one-time codes.
//!
//! Codes are RFC 6238 (HMAC-SHA-1, 6 digits, 30-second steps), what every
//! authenticator app computes. A code is accepted for the current step or
//! one either side (clock drift), never for a step at or before the last
//! one accepted: no code works twice.
//!
//! # Honest threat model
//!
//! This tier is **weaker by construction**. The seed is stored in the
//! keyring, because the vault must be able to check a code; the KEK is
//! Argon2id of that seed. Anyone holding the keyring file holds the seed,
//! can compute every code, and can rebuild the KEK. The tier is an unlock
//! ceremony that keeps the DEK off disk and needs the phone to hand, not a
//! barrier against a reader of the keyring. The hardware-key tier is.
//!
//! Enrollment takes two steps, so a mis-scanned seed cannot lock the vault:
//! [`Keyring::begin_totp`] gives the `otpauth://` URI to show once, and
//! [`Keyring::confirm_totp`] switches tier only after a code from the app
//! matches.

use argon2::{Algorithm, Argon2, Params, Version};
use hmac::{Hmac, Mac as _};
use sha1::Sha1;
use subtle::ConstantTimeEq as _;
use zeroize::{Zeroize as _, Zeroizing};

use crate::Error;
use crate::keyring::{Factor, Kek, Keyring, Tier, Unlocked, hex, random, unhex};
use crate::rotate::Rotated;

/// Seconds per code.
pub const STEP_SECONDS: u64 = 30;
/// Digits per code.
pub const DIGITS: u32 = 6;
/// Seed length: 160 bits, RFC 4226's recommendation for HMAC-SHA-1.
const SEED_LEN: usize = 20;
/// Argon2id memory, KiB (19 MiB), iterations and lanes: OWASP's baseline.
const ARGON_MEMORY_KIB: u32 = 19 * 1024;
const ARGON_ITERATIONS: u32 = 2;
const ARGON_LANES: u32 = 1;
/// How the app names the vault.
const LABEL: &str = "Hatchling:vault";

/// RFC 4226 HOTP: the `digits`-digit code for `counter`.
fn hotp(seed: &[u8], counter: u64, digits: u32) -> Option<u32> {
    let mut mac = <Hmac<Sha1>>::new_from_slice(seed).ok()?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest.last()? & 0x0f);
    let window = <[u8; 4]>::try_from(digest.get(offset..offset + 4)?).ok()?;
    Some((u32::from_be_bytes(window) & 0x7fff_ffff) % 10u32.pow(digits))
}

/// The code an authenticator app shows for `seed` at `unix_seconds`.
#[must_use]
pub fn totp_code(seed: &[u8], unix_seconds: u64) -> String {
    hotp(seed, unix_seconds / STEP_SECONDS, DIGITS)
        .map_or_else(String::new, |code| format!("{code:06}"))
}

/// RFC 4648 Base32, unpadded, as `otpauth://` URIs carry seeds.
pub(crate) fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buffer, mut bits) = (0u32, 0u32);
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.extend(
                ALPHABET
                    .get(usize::try_from((buffer >> bits) & 31).unwrap_or(0))
                    .map(|&c| char::from(c)),
            );
        }
    }
    if bits > 0 {
        out.extend(
            ALPHABET
                .get(usize::try_from((buffer << (5 - bits)) & 31).unwrap_or(0))
                .map(|&c| char::from(c)),
        );
    }
    out
}

/// A typed code: exactly six digits, surrounding spaces ignored.
fn parse_code(text: &str) -> Option<u32> {
    let text = text.trim();
    if text.len() != 6 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The step `code` is valid for, within one step of `now`'s and after
/// `after`, compared in constant time.
fn matching_step(seed: &[u8], code: u32, now: u64, after: Option<u64>) -> Option<u64> {
    let current = now / STEP_SECONDS;
    [
        current.saturating_sub(1),
        current,
        current.saturating_add(1),
    ]
    .into_iter()
    .filter(|step| after.is_none_or(|last| *step > last))
    .find(|step| {
        hotp(seed, *step, DIGITS)
            .is_some_and(|expected| bool::from(expected.to_be_bytes().ct_eq(&code.to_be_bytes())))
    })
}

/// The TOTP tier's KEK: Argon2id of the seed, salted by the vault id and
/// the wrap's own salt.
pub(crate) fn kek(vault_id: &str, seed: &[u8], salt: &[u8]) -> Result<Kek, Error> {
    let params = Params::new(ARGON_MEMORY_KIB, ARGON_ITERATIONS, ARGON_LANES, Some(32))
        .map_err(|_| Error::Kdf)?;
    let mut okm = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(
            seed,
            &[vault_id.as_bytes(), salt].concat(),
            okm.as_mut_slice(),
        )
        .map_err(|_| Error::Kdf)?;
    Ok(Kek::from_bytes(*okm))
}

/// A TOTP enrollment begun and not yet confirmed: the seed, and the URI
/// to show the user once. Nothing is stored until it is confirmed.
pub struct PendingTotp {
    seed: Zeroizing<[u8; SEED_LEN]>,
    uri: Zeroizing<String>,
}

impl PendingTotp {
    /// The `otpauth://` URI for the authenticator app (a QR code, or
    /// typed). It is the seed: show it once, store it nowhere.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }
}

impl std::fmt::Debug for PendingTotp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PendingTotp([redacted])")
    }
}

impl Keyring {
    /// Begins moving the vault to the TOTP tier: a fresh seed, and its
    /// URI. Needs the current tier's unlock. Nothing changes until
    /// [`confirm_totp`](Self::confirm_totp).
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`] or a randomness error.
    pub fn begin_totp(&self, unlocked: &Unlocked) -> Result<PendingTotp, Error> {
        self.authorize(unlocked, Tier::Totp)?;
        let seed = Zeroizing::new(random::<SEED_LEN>()?);
        let uri = Zeroizing::new(format!(
            "otpauth://totp/{LABEL}?secret={}&issuer=Hatchling&algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}",
            base32(seed.as_slice())
        ));
        Ok(PendingTotp { seed, uri })
    }

    /// Puts the vault at the TOTP tier once `code`, from the app, matches
    /// the pending seed at `now` (Unix seconds), around a fresh DEK: the
    /// rotation is committed, and the stores move to it next
    /// (`rotate.rs`). Every other wrap is dropped. A rejected code changes
    /// nothing, and may be tried again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::TierChange`], [`Error::CodeRejected`] (and nothing
    /// changes), [`Error::RotationUnfinished`], or a derivation, I/O or
    /// randomness error.
    pub fn confirm_totp(
        &mut self,
        unlocked: &Unlocked,
        pending: &PendingTotp,
        code: &str,
        now: u64,
    ) -> Result<Rotated, Error> {
        self.authorize(unlocked, Tier::Totp)?;
        let step = parse_code(code)
            .and_then(|code| matching_step(pending.seed.as_slice(), code, now, None))
            .ok_or(Error::CodeRejected)?;
        let rotation = self.begin_rotation(unlocked, Tier::Totp)?;
        let salt = random::<16>()?;
        let kek = kek(self.vault_id(), pending.seed.as_slice(), &salt)?;
        let wrap = self.wrap(
            rotation.new_key(),
            &kek,
            Factor::Totp {
                seed: hex(pending.seed.as_slice()),
                salt: hex(&salt),
                last_step: step,
            },
        )?;
        self.commit_rotation(rotation, Tier::Totp, vec![wrap])
    }

    /// Unlocks the TOTP tier with a current code from the app. The code's
    /// step is saved before the DEK is returned, so the same code never
    /// unlocks twice.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnlockFailed`] if the vault is not at this tier,
    /// or the code is wrong, stale or used; or an I/O error saving the
    /// step, in which case the vault stays locked.
    pub fn unlock_totp(&mut self, code: &str, now: u64) -> Result<Unlocked, Error> {
        if self.tier() != Tier::Totp {
            return Err(Error::UnlockFailed);
        }
        let mut wraps = self.wraps().to_vec();
        let (index, dek, step) = wraps
            .iter()
            .enumerate()
            .find_map(|(index, wrap)| {
                let Factor::Totp {
                    seed,
                    salt,
                    last_step,
                } = &wrap.factor
                else {
                    return None;
                };
                let mut seed = Zeroizing::new(unhex(seed)?);
                let step = matching_step(&seed, parse_code(code)?, now, Some(*last_step))?;
                let kek = kek(self.vault_id(), &seed, &unhex(salt)?).ok()?;
                seed.zeroize();
                Some((index, self.unwrap(wrap, &kek).ok()?, step))
            })
            .ok_or(Error::UnlockFailed)?;
        if let Some(Factor::Totp { last_step, .. }) =
            wraps.get_mut(index).map(|wrap| &mut wrap.factor)
        {
            *last_step = step;
        }
        self.store_wraps(wraps)?;
        Ok(Unlocked::new(dek, Tier::Totp, self.vault_id().to_owned()))
    }
}

/// The seed a pending enrollment shows the app, for tests acting as it.
#[cfg(test)]
pub(crate) fn seed_of(pending: &PendingTotp) -> Vec<u8> {
    pending.seed.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{settle, test_dir};
    use crate::{MasterKey, SealedVault, Secret, SecretName};

    /// RFC 4226 / RFC 6238's test seed.
    const RFC_SEED: &[u8] = b"12345678901234567890";

    #[test]
    fn the_totp_kek_is_argon2id_at_the_pinned_cost() {
        // The known answer is from an independent Argon2id, OpenSSL 3.5's
        // KDF (checked against the reference's password/somesalt example):
        // m = 19456 KiB, t = 2, p = 1, 32 bytes, the seed as the password
        // and the vault id then the wrap's salt as the salt. At any other
        // cost or salt, the answer differs.
        let expected =
            unhex("a826f6611b475973ba2cf908155b0933e8e0bf808637ac2629e09df3f60ee85b").unwrap();
        let dir = test_dir("totp-kat");
        let dek = MasterKey::generate().unwrap();
        let path = dir.join("keys").join("keyring");
        Keyring::create(
            &path,
            &dek,
            &kek("vault-kat", RFC_SEED, b"0123456789abcdef").unwrap(),
        )
        .unwrap();
        let keyring = Keyring::load(&path).unwrap();
        assert!(
            keyring
                .unlock_file(&Kek::from_bytes(<[u8; 32]>::try_from(expected).unwrap()))
                .is_ok()
        );
        assert!(
            keyring
                .unlock_file(&kek("vault-kat", RFC_SEED, b"0123456789abcdeF").unwrap())
                .is_err()
        );
    }

    #[test]
    fn hotp_and_totp_match_the_rfc_test_vectors() {
        let hotp_codes = [
            755_224, 287_082, 359_152, 969_429, 338_314, 254_676, 287_922, 162_583, 399_871,
            520_489,
        ];
        for (counter, expected) in (0u64..).zip(hotp_codes) {
            assert_eq!(
                hotp(RFC_SEED, counter, 6),
                Some(expected),
                "RFC 4226 counter {counter}"
            );
        }
        for (time, expected) in [
            (59u64, 94_287_082),
            (1_111_111_109, 7_081_804),
            (1_111_111_111, 14_050_471),
            (1_234_567_890, 89_005_924),
            (2_000_000_000, 69_279_037),
            (20_000_000_000, 65_353_130),
        ] {
            assert_eq!(
                hotp(RFC_SEED, time / STEP_SECONDS, 8),
                Some(expected),
                "RFC 6238 T={time}"
            );
        }
        assert_eq!(totp_code(RFC_SEED, 59), "287082");
        assert_eq!(
            totp_code(RFC_SEED, 1_111_111_109),
            "081804",
            "leading zeros kept"
        );
    }

    #[test]
    fn base32_matches_rfc_4648_and_codes_parse_strictly() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "MY"),
            ("fo", "MZXQ"),
            ("foo", "MZXW6"),
            ("foob", "MZXW6YQ"),
            ("fooba", "MZXW6YTB"),
            ("foobar", "MZXW6YTBOI"),
        ] {
            assert_eq!(base32(plain.as_bytes()), encoded, "{plain:?}");
        }
        assert_eq!(parse_code(" 012345 "), Some(12_345));
        for bad in ["12345", "1234567", "12a456", "", "-12345", "12 345"] {
            assert_eq!(parse_code(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_code_is_good_for_its_step_and_one_either_side_and_never_again() {
        let now = 1_000_000_000;
        let code = |at: u64| parse_code(&totp_code(RFC_SEED, at)).unwrap();
        let current = now / STEP_SECONDS;
        assert_eq!(matching_step(RFC_SEED, code(now), now, None), Some(current));
        assert_eq!(
            matching_step(RFC_SEED, code(now - 30), now, None),
            Some(current - 1)
        );
        assert_eq!(
            matching_step(RFC_SEED, code(now + 30), now, None),
            Some(current + 1)
        );
        assert_eq!(
            matching_step(RFC_SEED, code(now - 60), now, None),
            None,
            "two steps old"
        );
        assert_eq!(
            matching_step(RFC_SEED, code(now + 60), now, None),
            None,
            "two steps ahead"
        );
        assert_eq!(
            matching_step(RFC_SEED, code(now), now, Some(current)),
            None,
            "already used"
        );
        assert_eq!(
            matching_step(RFC_SEED, code(now), now, Some(current - 1)),
            Some(current)
        );
        assert_eq!(
            matching_step(RFC_SEED, code(5), 5, None),
            Some(0),
            "the first step has no step before it"
        );
    }

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

    #[test]
    fn totp_enrolls_only_after_a_matching_code_then_unlocks_once_per_code() {
        let dir = test_dir("totp-enroll");
        let (mut keyring, file_kek) = vault(&dir);
        let unlocked = keyring.unlock_file(&file_kek).unwrap();
        let now = 1_700_000_000;
        let pending = keyring.begin_totp(&unlocked).unwrap();
        let seed = seed_of(&pending);
        assert!(
            pending
                .uri()
                .starts_with("otpauth://totp/Hatchling:vault?secret=")
        );
        assert!(
            pending.uri().contains(&base32(&seed))
                && pending
                    .uri()
                    .ends_with("&issuer=Hatchling&algorithm=SHA1&digits=6&period=30")
        );
        assert_eq!(format!("{pending:?}"), "PendingTotp([redacted])");
        // A wrong code changes nothing.
        let wrong = format!(
            "{:06}",
            (parse_code(&totp_code(&seed, now)).unwrap() + 1) % 1_000_000
        );
        assert!(matches!(
            keyring.confirm_totp(&unlocked, &pending, &wrong, now),
            Err(Error::CodeRejected)
        ));
        assert_eq!(
            Keyring::load(&dir.join("keys").join("keyring"))
                .unwrap()
                .tier(),
            Tier::File
        );
        // The right one switches tier, and the file KEK is no way in after.
        let pending = keyring.begin_totp(&unlocked).unwrap();
        let seed = seed_of(&pending);
        let rotated = keyring
            .confirm_totp(&unlocked, &pending, &totp_code(&seed, now), now)
            .unwrap();
        assert!(rotated.codes.is_none(), "no codes before, none made");
        settle(&mut keyring, rotated, &dir.join("vault"));
        let mut keyring = Keyring::load(&dir.join("keys").join("keyring")).unwrap();
        assert_eq!((keyring.tier(), keyring.wraps().len()), (Tier::Totp, 1));
        assert!(matches!(
            keyring.unlock_file(&file_kek),
            Err(Error::UnlockFailed)
        ));
        // The enrollment's own code is spent; the next step's unlocks, once.
        assert!(matches!(
            keyring.unlock_totp(&totp_code(&seed, now), now),
            Err(Error::UnlockFailed)
        ));
        let later = now + STEP_SECONDS;
        let opened = keyring
            .unlock_totp(&totp_code(&seed, later), later)
            .unwrap();
        assert_eq!(opened.via(), Tier::Totp);
        let name: SecretName = "token".parse().unwrap();
        assert_eq!(
            SealedVault::open(&dir.join("vault"), opened.key())
                .unwrap()
                .fetch(&name)
                .unwrap()
                .as_bytes(),
            b"kept"
        );
        let mut reloaded = Keyring::load(&dir.join("keys").join("keyring")).unwrap();
        assert!(
            matches!(
                reloaded.unlock_totp(&totp_code(&seed, later), later),
                Err(Error::UnlockFailed)
            ),
            "replayed, even after a reload"
        );
        assert!(matches!(
            reloaded.unlock_totp("not a code", later),
            Err(Error::UnlockFailed)
        ));
        assert!(matches!(
            reloaded.unlock_totp(&totp_code(b"another seed", later + 30), later + 30),
            Err(Error::UnlockFailed)
        ));
        assert!(
            reloaded
                .unlock_totp(&totp_code(&seed, later + 30), later + 30)
                .is_ok()
        );
        // Honestly weaker: the seed is in the keyring.
        assert!(
            std::fs::read_to_string(dir.join("keys").join("keyring"))
                .unwrap()
                .contains(&hex(&seed))
        );
    }

    #[test]
    fn totp_changes_need_the_current_tier_and_its_unlock_works_nowhere_else() {
        let dir = test_dir("totp-tier");
        let (mut keyring, file_kek) = vault(&dir);
        let from_file = keyring.unlock_file(&file_kek).unwrap();
        let now = 1_700_000_000;
        let pending = keyring.begin_totp(&from_file).unwrap();
        let seed = seed_of(&pending);
        keyring
            .confirm_totp(&from_file, &pending, &totp_code(&seed, now), now)
            .unwrap();
        // The stale file unlock can neither begin nor confirm, nor go back down.
        assert!(matches!(
            keyring.begin_totp(&from_file),
            Err(Error::TierChange { .. })
        ));
        assert!(matches!(
            keyring.use_file_tier(&from_file, &file_kek),
            Err(Error::TierChange { .. })
        ));
        let mut reloaded = Keyring::load(&dir.join("keys").join("keyring")).unwrap();
        let from_code = keyring
            .unlock_totp(&totp_code(&seed, now + 30), now + 30)
            .unwrap();
        let pending = keyring.begin_totp(&from_code).unwrap();
        assert!(matches!(
            reloaded.confirm_totp(&from_file, &pending, "000000", now),
            Err(Error::TierChange { .. })
        ));
        // A file-tier vault does not unlock with a code.
        let other_dir = dir.join("other");
        std::fs::create_dir(&other_dir).unwrap();
        let (mut plain, _) = vault(&other_dir);
        assert!(matches!(
            plain.unlock_totp(&totp_code(&seed, now), now),
            Err(Error::UnlockFailed)
        ));
    }

    #[test]
    fn damaged_totp_wraps_open_nothing() {
        let dir = test_dir("totp-damage");
        let (mut keyring, file_kek) = vault(&dir);
        let unlocked = keyring.unlock_file(&file_kek).unwrap();
        let now = 1_700_000_000;
        let pending = keyring.begin_totp(&unlocked).unwrap();
        let seed = seed_of(&pending);
        keyring
            .confirm_totp(&unlocked, &pending, &totp_code(&seed, now), now)
            .unwrap();
        let path = dir.join("keys").join("keyring");
        let text = std::fs::read_to_string(&path).unwrap();
        let later = now + 90;
        for damaged in [
            text.replace(&hex(&seed), "zz"),
            text.replacen("\"salt\": \"", "\"salt\": \"zz", 1),
            text.replacen("\"kind\": \"totp\"", "\"kind\": \"file\"", 1),
        ] {
            std::fs::write(&path, &damaged).unwrap();
            let mut damaged = Keyring::load(&path).unwrap();
            assert!(matches!(
                damaged.unlock_totp(&totp_code(&seed, later), later),
                Err(Error::UnlockFailed)
            ));
        }
        std::fs::write(&path, &text).unwrap();
        assert!(
            Keyring::load(&path)
                .unwrap()
                .unlock_totp(&totp_code(&seed, later), later)
                .is_ok()
        );
    }
}
