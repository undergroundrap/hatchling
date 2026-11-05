//! CTAP2's PIN/UV auth protocols 1 and 2 (CTAP 2.1, section 6.5.6–7).
//!
//! Both sides agree a P-256 ECDH secret `Z` (`ring`), and derive from it:
//!
//! | | Protocol 1 | Protocol 2 |
//! | --- | --- | --- |
//! | Keys | `SHA-256(Z)`, for HMAC and AES alike | `HKDF-SHA-256(salt = 32 zero bytes, Z)`, info `"CTAP2 HMAC key"` and `"CTAP2 AES key"` |
//! | Encrypt | AES-256-CBC, IV all zeros | AES-256-CBC, a random IV sent ahead of the ciphertext |
//! | Authenticate | `HMAC-SHA-256`, first 16 bytes | `HMAC-SHA-256`, all 32 |
//!
//! Plaintexts are whole AES blocks in CTAP2 (a salt, a PIN hash, a token),
//! so there is no padding, and anything else is refused. The same
//! functions serve the host and, in tests, a software authenticator.

use cbc::cipher::block_padding::NoPadding;
use cbc::cipher::{BlockDecryptMut as _, BlockEncryptMut as _, KeyIvInit as _};
use hmac::{Hmac, Mac as _};
use ring::agreement::{ECDH_P256, EphemeralPrivateKey, UnparsedPublicKey, agree_ephemeral};
use ring::rand::{SecureRandom as _, SystemRandom};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::cbor::Value;

/// A PIN/UV auth protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Protocol 1 (CTAP 2.0).
    One,
    /// Protocol 2 (CTAP 2.1).
    Two,
}

impl Protocol {
    /// The protocol's number on the wire.
    #[must_use]
    pub const fn number(self) -> i64 {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }

    /// The protocol numbered `number`, if it is one this host speaks.
    #[must_use]
    pub const fn from_number(number: i64) -> Option<Self> {
        match number {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            _ => None,
        }
    }

    /// `HMAC-SHA-256(key, message)`, cut to this protocol's length.
    #[must_use]
    pub fn authenticate(self, key: &[u8], message: &[u8]) -> Vec<u8> {
        // HMAC takes a key of any length.
        let Ok(mut mac) = <Hmac<Sha256>>::new_from_slice(key) else {
            return Vec::new();
        };
        mac.update(message);
        let tag = mac.finalize().into_bytes().to_vec();
        match self {
            Self::One => tag.into_iter().take(16).collect(),
            Self::Two => tag,
        }
    }
}

/// Why a protocol step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// The peer's key is not a P-256 point in a COSE key.
    #[error("the key agreement key is not a P-256 COSE key")]
    BadKey,
    /// Data that is not whole AES blocks, or too short to hold an IV.
    #[error("encrypted data has the wrong length")]
    BadLength,
    /// The system's randomness or the key agreement failed.
    #[error("the key agreement failed")]
    Agreement,
}

/// The keys both sides derive from one agreement.
pub struct Shared {
    protocol: Protocol,
    hmac_key: Zeroizing<[u8; 32]>,
    aes_key: Zeroizing<[u8; 32]>,
}

impl Shared {
    /// The keys `protocol` derives from the ECDH secret `z`.
    #[must_use]
    pub fn derive(protocol: Protocol, z: &[u8]) -> Self {
        match protocol {
            Protocol::One => {
                let hmac_key = Zeroizing::new(Sha256::digest(z).into());
                Self {
                    protocol,
                    aes_key: hmac_key.clone(),
                    hmac_key,
                }
            }
            Protocol::Two => {
                let hkdf = hkdf::Hkdf::<Sha256>::new(Some(&[0u8; 32]), z);
                let mut hmac_key = Zeroizing::new([0u8; 32]);
                let mut aes_key = Zeroizing::new([0u8; 32]);
                // HKDF-SHA-256 expands up to 8160 bytes; 32 always fits.
                let _ = hkdf.expand(b"CTAP2 HMAC key", hmac_key.as_mut_slice());
                let _ = hkdf.expand(b"CTAP2 AES key", aes_key.as_mut_slice());
                Self {
                    protocol,
                    hmac_key,
                    aes_key,
                }
            }
        }
    }

    /// The protocol these keys belong to.
    #[must_use]
    pub const fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// `plaintext` (whole AES blocks) encrypted.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::BadLength`] for anything but whole blocks,
    /// or [`CryptoError::Agreement`] if no random IV can be drawn.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if plaintext.is_empty() || plaintext.len() % 16 != 0 {
            return Err(CryptoError::BadLength);
        }
        let iv = match self.protocol {
            Protocol::One => [0u8; 16],
            Protocol::Two => random::<16>()?,
        };
        let ciphertext =
            cbc::Encryptor::<aes::Aes256>::new_from_slices(self.aes_key.as_slice(), &iv)
                .map_err(|_| CryptoError::BadLength)?
                .encrypt_padded_vec_mut::<NoPadding>(plaintext);
        Ok(match self.protocol {
            Protocol::One => ciphertext,
            Protocol::Two => [&iv[..], &ciphertext].concat(),
        })
    }

    /// `ciphertext` decrypted.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::BadLength`] for anything but whole blocks
    /// (after the IV, in protocol 2).
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let (iv, body) = match self.protocol {
            Protocol::One => ([0u8; 16], ciphertext),
            Protocol::Two => {
                let (iv, body) = ciphertext
                    .split_at_checked(16)
                    .ok_or(CryptoError::BadLength)?;
                (
                    <[u8; 16]>::try_from(iv).map_err(|_| CryptoError::BadLength)?,
                    body,
                )
            }
        };
        if body.is_empty() || body.len() % 16 != 0 {
            return Err(CryptoError::BadLength);
        }
        cbc::Decryptor::<aes::Aes256>::new_from_slices(self.aes_key.as_slice(), &iv)
            .map_err(|_| CryptoError::BadLength)?
            .decrypt_padded_vec_mut::<NoPadding>(body)
            .map(Zeroizing::new)
            .map_err(|_| CryptoError::BadLength)
    }

    /// `message` authenticated under these keys.
    #[must_use]
    pub fn authenticate(&self, message: &[u8]) -> Vec<u8> {
        self.protocol
            .authenticate(self.hmac_key.as_slice(), message)
    }
}

/// `N` random bytes from the system.
///
/// # Errors
///
/// Returns [`CryptoError::Agreement`] if the system has no randomness.
pub fn random<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| CryptoError::Agreement)?;
    Ok(bytes)
}

/// A fresh P-256 key agreement key: the private half, and its public
/// half as a COSE key.
///
/// # Errors
///
/// Returns [`CryptoError::Agreement`] if no key can be made.
pub fn agreement_key() -> Result<(EphemeralPrivateKey, Value), CryptoError> {
    let private = EphemeralPrivateKey::generate(&ECDH_P256, &SystemRandom::new())
        .map_err(|_| CryptoError::Agreement)?;
    let public = private
        .compute_public_key()
        .map_err(|_| CryptoError::Agreement)?;
    let point = <[u8; 65]>::try_from(public.as_ref()).map_err(|_| CryptoError::Agreement)?;
    Ok((private, cose_key(&point)))
}

/// The shared keys from our `private` key and the peer's COSE key.
///
/// # Errors
///
/// Returns [`CryptoError::BadKey`] for a peer key that is not a P-256
/// point, or [`CryptoError::Agreement`] if the agreement fails.
pub fn agree(
    protocol: Protocol,
    private: EphemeralPrivateKey,
    peer: &Value,
) -> Result<Shared, CryptoError> {
    let point = cose_point(peer)?;
    agree_ephemeral(private, &UnparsedPublicKey::new(&ECDH_P256, point), |z| {
        Shared::derive(protocol, z)
    })
    .map_err(|_| CryptoError::Agreement)
}

/// The host side of one agreement: a fresh key of ours, agreed with the
/// authenticator's `peer` key. Returns our public COSE key (to send) and
/// the shared keys.
///
/// # Errors
///
/// As [`agreement_key`] and [`agree`].
pub fn encapsulate(protocol: Protocol, peer: &Value) -> Result<(Value, Shared), CryptoError> {
    let (private, public) = agreement_key()?;
    Ok((public, agree(protocol, private, peer)?))
}

/// An uncompressed P-256 point as a COSE key for ECDH (CTAP2's
/// `keyAgreement`): kty EC2, alg ECDH-ES+HKDF-256, crv P-256, x, y.
#[must_use]
pub fn cose_key(point: &[u8; 65]) -> Value {
    let (x, y) = point.get(1..).unwrap_or(&[]).split_at(32);
    Value::map(vec![
        (Value::int(1), Value::int(2)),
        (Value::int(3), Value::int(-25)),
        (Value::int(-1), Value::int(1)),
        (Value::int(-2), Value::Bytes(x.to_vec())),
        (Value::int(-3), Value::Bytes(y.to_vec())),
    ])
}

/// The uncompressed P-256 point in a COSE key.
///
/// # Errors
///
/// Returns [`CryptoError::BadKey`] unless the key is EC2 on P-256 with
/// 32-byte coordinates.
pub fn cose_point(key: &Value) -> Result<[u8; 65], CryptoError> {
    if key.at(1).and_then(Value::as_int) != Some(2) || key.at(-1).and_then(Value::as_int) != Some(1)
    {
        return Err(CryptoError::BadKey);
    }
    let coordinate = |label| {
        key.at(label)
            .and_then(Value::as_bytes)
            .filter(|bytes| bytes.len() == 32)
            .ok_or(CryptoError::BadKey)
    };
    let (x, y) = (coordinate(-2)?, coordinate(-3)?);
    let mut point = [0u8; 65];
    for (slot, byte) in point
        .iter_mut()
        .zip(std::iter::once(&0x04u8).chain(x).chain(y))
    {
        *slot = *byte;
    }
    Ok(point)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocols_have_their_numbers() {
        for protocol in [Protocol::One, Protocol::Two] {
            assert_eq!(Protocol::from_number(protocol.number()), Some(protocol));
        }
        assert_eq!(Protocol::One.number(), 1);
        assert_eq!(Protocol::Two.number(), 2);
        assert_eq!(Protocol::from_number(3), None);
        assert_eq!(Protocol::from_number(0), None);
    }

    #[test]
    fn protocol_one_keys_are_the_hash_of_the_secret() {
        let z = [9u8; 32];
        let shared = Shared::derive(Protocol::One, &z);
        let hash: [u8; 32] = Sha256::digest(z).into();
        assert_eq!(*shared.hmac_key, hash);
        assert_eq!(*shared.aes_key, hash);
        assert_eq!(shared.protocol(), Protocol::One);
    }

    #[test]
    fn protocol_two_keys_come_from_hkdf_with_their_labels() {
        let z = [9u8; 32];
        let shared = Shared::derive(Protocol::Two, &z);
        let hkdf = hkdf::Hkdf::<Sha256>::new(Some(&[0u8; 32]), &z);
        let (mut hmac_key, mut aes_key) = ([0u8; 32], [0u8; 32]);
        hkdf.expand(b"CTAP2 HMAC key", &mut hmac_key).unwrap();
        hkdf.expand(b"CTAP2 AES key", &mut aes_key).unwrap();
        assert_eq!(*shared.hmac_key, hmac_key);
        assert_eq!(*shared.aes_key, aes_key);
        assert_ne!(hmac_key, aes_key);
    }

    #[test]
    fn authentication_is_hmac_sha256_cut_to_the_protocol() {
        let mut mac = <Hmac<Sha256>>::new_from_slice(b"key").unwrap();
        mac.update(b"message");
        let full = mac.finalize().into_bytes().to_vec();
        assert_eq!(Protocol::Two.authenticate(b"key", b"message"), full);
        assert_eq!(
            Protocol::One.authenticate(b"key", b"message"),
            full.get(..16).unwrap()
        );
        let shared = Shared::derive(Protocol::Two, &[1; 32]);
        assert_eq!(
            shared.authenticate(b"m"),
            Protocol::Two.authenticate(shared.hmac_key.as_slice(), b"m")
        );
    }

    #[test]
    fn protocol_one_is_cbc_with_a_zero_iv_and_protocol_two_sends_its_iv() {
        let block = [0x42u8; 32];
        let one = Shared::derive(Protocol::One, &[3; 32]);
        let sealed = one.encrypt(&block).unwrap();
        assert_eq!(sealed.len(), 32);
        assert_eq!(
            one.encrypt(&block).unwrap(),
            sealed,
            "a fixed IV: deterministic"
        );
        assert_eq!(*one.decrypt(&sealed).unwrap(), block);
        let expected =
            cbc::Encryptor::<aes::Aes256>::new_from_slices(one.aes_key.as_slice(), &[0; 16])
                .unwrap()
                .encrypt_padded_vec_mut::<NoPadding>(&block);
        assert_eq!(sealed, expected);
        let two = Shared::derive(Protocol::Two, &[3; 32]);
        let sealed = two.encrypt(&block).unwrap();
        assert_eq!(sealed.len(), 48, "the IV goes ahead");
        assert_ne!(
            two.encrypt(&block).unwrap(),
            sealed,
            "a random IV each time"
        );
        assert_eq!(*two.decrypt(&sealed).unwrap(), block);
        let (iv, body) = sealed.split_at(16);
        let expected = cbc::Encryptor::<aes::Aes256>::new_from_slices(two.aes_key.as_slice(), iv)
            .unwrap()
            .encrypt_padded_vec_mut::<NoPadding>(&block);
        assert_eq!(body, expected);
    }

    #[test]
    fn partial_blocks_are_refused_both_ways() {
        for protocol in [Protocol::One, Protocol::Two] {
            let shared = Shared::derive(protocol, &[5; 32]);
            for bad in [&[][..], &[1; 15], &[1; 17]] {
                assert_eq!(shared.encrypt(bad).err(), Some(CryptoError::BadLength));
            }
        }
        let one = Shared::derive(Protocol::One, &[5; 32]);
        for bad in [&[][..], &[1; 15], &[1; 33]] {
            assert_eq!(one.decrypt(bad).err(), Some(CryptoError::BadLength));
        }
        let two = Shared::derive(Protocol::Two, &[5; 32]);
        for bad in [&[][..], &[1; 15], &[1; 16], &[1; 31]] {
            assert_eq!(two.decrypt(bad).err(), Some(CryptoError::BadLength));
        }
    }

    #[test]
    fn both_sides_of_an_agreement_derive_the_same_keys() {
        for protocol in [Protocol::One, Protocol::Two] {
            let (authenticator, authenticator_public) = agreement_key().unwrap();
            let (host_public, host) = encapsulate(protocol, &authenticator_public).unwrap();
            let device = agree(protocol, authenticator, &host_public).unwrap();
            assert_eq!(*host.hmac_key, *device.hmac_key);
            assert_eq!(*host.aes_key, *device.aes_key);
            let sealed = host.encrypt(&[7; 32]).unwrap();
            assert_eq!(*device.decrypt(&sealed).unwrap(), [7; 32]);
            assert_eq!(host.authenticate(&sealed), device.authenticate(&sealed));
        }
    }

    #[test]
    fn a_cose_key_round_trips_and_a_bad_one_is_refused() {
        let mut point = [0u8; 65];
        for (byte, value) in point.iter_mut().zip(0u8..) {
            *byte = value;
        }
        if let Some(first) = point.first_mut() {
            *first = 4;
        }
        let key = cose_key(&point);
        assert_eq!(cose_point(&key).unwrap(), point);
        assert_eq!(key.at(3).and_then(Value::as_int), Some(-25));
        let with = |label: i64, value: Value| {
            let Value::Map(mut entries) = key.clone() else {
                unreachable!()
            };
            for (k, v) in &mut entries {
                if k.as_int() == Some(label) {
                    *v = value.clone();
                }
            }
            Value::Map(entries)
        };
        for bad in [
            with(1, Value::int(1)),
            with(-1, Value::int(2)),
            with(-2, Value::Bytes(vec![1; 31])),
            with(-3, Value::Bytes(vec![1; 33])),
            with(-2, Value::int(0)),
            Value::Null,
        ] {
            assert_eq!(cose_point(&bad), Err(CryptoError::BadKey));
        }
        // A well-formed key that is not on the curve fails the agreement.
        let (private, _) = agreement_key().unwrap();
        assert_eq!(
            agree(Protocol::One, private, &cose_key(&point)).err(),
            Some(CryptoError::Agreement)
        );
    }

    #[test]
    fn random_bytes_differ() {
        assert_ne!(random::<16>().unwrap(), random::<16>().unwrap());
        assert_eq!(
            CryptoError::BadLength.to_string(),
            "encrypted data has the wrong length"
        );
    }
}
