//! A virtual FIDO2 authenticator for tests and CI: the CTAP2
//! `hmac-secret` contract in software, so the hardware-key tier is tested
//! without a physical key.
//!
//! Each credential gets a random 32-byte secret (CTAP2's `CredRandom`)
//! that never leaves this value, and `hmac-secret` is
//! `HMAC-SHA-256(CredRandom, salt)`, as the extension defines it.
//!
//! Test-only by construction: the module exists in the vault's own tests
//! and behind the off-by-default `virtual-authenticator` feature, which
//! only test builds of other crates enable. A real build has no software
//! key to unlock the vault with.

use hmac::{Hmac, Mac as _};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::fido2::{Authenticator, AuthenticatorError, Credential};
use crate::keyring::random;

/// One credential the virtual key holds.
struct Held {
    rp_id: String,
    id: Vec<u8>,
    secret: Zeroizing<[u8; 32]>,
    /// The secret its answer comes from once the user is verified.
    verified: Zeroizing<[u8; 32]>,
}

/// A software FIDO2 authenticator.
pub struct VirtualAuthenticator {
    held: Vec<Held>,
    hmac_secret: bool,
    present: bool,
    remembers: bool,
    /// Its PIN, if one is set.
    pin: Option<String>,
    /// The PIN its user types when asked.
    typed: Option<String>,
}

impl VirtualAuthenticator {
    /// A key that supports `hmac-secret`, with someone touching it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            held: Vec::new(),
            hmac_secret: true,
            present: true,
            remembers: true,
            pin: None,
            typed: None,
        }
    }

    /// A key without the `hmac-secret` extension: it creates credentials
    /// but cannot derive anything from them.
    #[must_use]
    pub const fn without_hmac_secret() -> Self {
        Self {
            held: Vec::new(),
            hmac_secret: false,
            present: true,
            remembers: true,
            pin: None,
            typed: None,
        }
    }

    /// A faulty key: it creates credentials and then does not know them.
    #[must_use]
    pub const fn forgetting_credentials() -> Self {
        Self {
            held: Vec::new(),
            hmac_secret: true,
            present: true,
            remembers: false,
            pin: None,
            typed: None,
        }
    }

    /// Whether someone touches the key when it asks.
    pub const fn set_present(&mut self, present: bool) {
        self.present = present;
    }

    /// A key with `pin` set, whose user types it when asked.
    #[must_use]
    pub fn with_pin(pin: &str) -> Self {
        let mut key = Self::new();
        key.pin = Some(pin.to_owned());
        key.typed = Some(pin.to_owned());
        key
    }

    /// What its user types when asked for the PIN: `None` for nothing.
    pub fn set_typed(&mut self, typed: Option<&str>) {
        self.typed = typed.map(str::to_owned);
    }
}

impl Default for VirtualAuthenticator {
    fn default() -> Self {
        Self::new()
    }
}

impl Authenticator for VirtualAuthenticator {
    fn make_credential(
        &mut self,
        rp_id: &str,
        _user_id: &[u8],
    ) -> Result<Credential, AuthenticatorError> {
        if !self.present {
            return Err(AuthenticatorError::NoUserPresence);
        }
        let no_randomness = |_| AuthenticatorError::Device(String::from("no randomness"));
        let id = random::<16>().map_err(no_randomness)?.to_vec();
        let secret = Zeroizing::new(random::<32>().map_err(no_randomness)?);
        let verified = Zeroizing::new(random::<32>().map_err(no_randomness)?);
        if self.remembers {
            self.held.push(Held {
                rp_id: rp_id.to_owned(),
                id: id.clone(),
                secret,
                verified,
            });
        }
        Ok(Credential {
            id,
            hmac_secret: self.hmac_secret,
        })
    }

    fn hmac_secret(
        &mut self,
        rp_id: &str,
        credential_id: &[u8],
        salt: &[u8; 32],
    ) -> Result<[u8; 32], AuthenticatorError> {
        if !self.present {
            return Err(AuthenticatorError::NoUserPresence);
        }
        self.answer(rp_id, credential_id, salt, false)
    }

    fn hmac_secret_verified(
        &mut self,
        rp_id: &str,
        credential_id: &[u8],
        salt: &[u8; 32],
    ) -> Result<[u8; 32], AuthenticatorError> {
        if self.pin.is_none() || self.typed != self.pin {
            return Err(AuthenticatorError::NoVerification);
        }
        if !self.present {
            return Err(AuthenticatorError::NoUserPresence);
        }
        self.answer(rp_id, credential_id, salt, true)
    }
}

impl VirtualAuthenticator {
    /// `HMAC-SHA-256` of `salt` under the credential's secret: its
    /// verified-user one if `verified`.
    fn answer(
        &self,
        rp_id: &str,
        credential_id: &[u8],
        salt: &[u8; 32],
        verified: bool,
    ) -> Result<[u8; 32], AuthenticatorError> {
        if !self.hmac_secret {
            return Err(AuthenticatorError::Device(String::from(
                "hmac-secret is not supported",
            )));
        }
        let held = self
            .held
            .iter()
            .find(|held| held.rp_id == rp_id && held.id == credential_id)
            .ok_or(AuthenticatorError::UnknownCredential)?;
        let secret = if verified {
            &held.verified
        } else {
            &held.secret
        };
        let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_slice())
            .map_err(|_| AuthenticatorError::Device(String::from("bad key length")))?;
        mac.update(salt);
        Ok(mac.finalize().into_bytes().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_computes_hmac_secret_per_credential_and_relying_party() {
        let mut key = VirtualAuthenticator::default();
        let one = key.make_credential("rp", b"user").unwrap();
        let two = key.make_credential("rp", b"user").unwrap();
        assert!(one.hmac_secret && one.id != two.id);
        let salt = [5u8; 32];
        let a = key.hmac_secret("rp", &one.id, &salt).unwrap();
        assert_eq!(
            key.hmac_secret("rp", &one.id, &salt).unwrap(),
            a,
            "deterministic"
        );
        assert_ne!(
            key.hmac_secret("rp", &two.id, &salt).unwrap(),
            a,
            "per credential"
        );
        assert_ne!(
            key.hmac_secret("rp", &one.id, &[6u8; 32]).unwrap(),
            a,
            "per salt"
        );
        assert_eq!(
            key.hmac_secret("other", &one.id, &salt),
            Err(AuthenticatorError::UnknownCredential)
        );
        assert_eq!(
            key.hmac_secret("rp", b"nope", &salt),
            Err(AuthenticatorError::UnknownCredential)
        );
        // It is HMAC-SHA-256 under the credential's secret.
        let secret = key.held.first().unwrap().secret.clone();
        let mut mac = <Hmac<Sha256>>::new_from_slice(secret.as_slice()).unwrap();
        mac.update(&salt);
        assert_eq!(<[u8; 32]>::from(mac.finalize().into_bytes()), a);
    }

    #[test]
    fn presence_support_and_memory_are_what_the_test_set() {
        let mut key = VirtualAuthenticator::new();
        let credential = key.make_credential("rp", b"u").unwrap();
        key.set_present(false);
        assert_eq!(
            key.make_credential("rp", b"u"),
            Err(AuthenticatorError::NoUserPresence)
        );
        assert_eq!(
            key.hmac_secret("rp", &credential.id, &[0; 32]),
            Err(AuthenticatorError::NoUserPresence)
        );
        key.set_present(true);
        assert!(key.hmac_secret("rp", &credential.id, &[0; 32]).is_ok());
        let mut plain = VirtualAuthenticator::without_hmac_secret();
        let credential = plain.make_credential("rp", b"u").unwrap();
        assert!(!credential.hmac_secret);
        assert!(matches!(
            plain.hmac_secret("rp", &credential.id, &[0; 32]),
            Err(AuthenticatorError::Device(_))
        ));
        let mut forgetful = VirtualAuthenticator::forgetting_credentials();
        let credential = forgetful.make_credential("rp", b"u").unwrap();
        assert_eq!(
            forgetful.hmac_secret("rp", &credential.id, &[0; 32]),
            Err(AuthenticatorError::UnknownCredential)
        );
    }

    #[test]
    fn a_verified_answer_needs_the_pin_and_differs_from_a_touch() {
        let mut key = VirtualAuthenticator::with_pin("2468");
        let credential = key.make_credential("rp", b"u").unwrap();
        let salt = [3u8; 32];
        let verified = key
            .hmac_secret_verified("rp", &credential.id, &salt)
            .unwrap();
        let touched = key.hmac_secret("rp", &credential.id, &salt).unwrap();
        assert_ne!(verified, touched, "the verified-user secret is another");
        assert_eq!(
            key.hmac_secret_verified("rp", &credential.id, &salt)
                .unwrap(),
            verified
        );
        for typed in [None, Some("0000")] {
            key.set_typed(typed);
            assert_eq!(
                key.hmac_secret_verified("rp", &credential.id, &salt),
                Err(AuthenticatorError::NoVerification)
            );
        }
        key.set_typed(Some("2468"));
        key.set_present(false);
        assert_eq!(
            key.hmac_secret_verified("rp", &credential.id, &salt),
            Err(AuthenticatorError::NoUserPresence)
        );
        let mut plain = VirtualAuthenticator::new();
        let credential = plain.make_credential("rp", b"u").unwrap();
        assert_eq!(
            plain.hmac_secret_verified("rp", &credential.id, &salt),
            Err(AuthenticatorError::NoVerification),
            "no PIN set"
        );
    }
}
