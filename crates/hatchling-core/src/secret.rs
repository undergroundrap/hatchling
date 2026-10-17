//! Secret payload codec: arbitrary bytes inside framed JSON.
//!
//! The wire protocol is framed JSON, but secrets are arbitrary bytes.
//! This module is the single place that converts between the two:
//! base64, one pinned engine ([`base64::engine::general_purpose::STANDARD`]).
//! The agent encodes before sending; the broker decodes after receiving.
//! Neither end hand-rolls the conversion.
//!
//! [`MAX_SECRET_BYTES`] is the largest secret either end will carry. It
//! is sized so the base64 expansion of a maximum secret still fits
//! comfortably inside one [`MAX_FRAME_BYTES`](crate::MAX_FRAME_BYTES)
//! frame with room for the JSON envelope: 32 KiB of secret becomes
//! ~43 KiB of base64, well under the 64 KiB frame cap.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroizing;

use crate::Error;

/// Maximum size of one secret, in bytes. A secret must fit in a single
/// protocol frame; this bound keeps the base64 expansion inside
/// [`MAX_FRAME_BYTES`](crate::MAX_FRAME_BYTES).
pub const MAX_SECRET_BYTES: usize = 32 * 1024;

/// Encodes secret bytes for a protocol payload.
#[must_use]
pub fn encode(secret: &[u8]) -> String {
    STANDARD.encode(secret)
}

/// Decodes a secret payload back to bytes.
///
/// # Errors
///
/// Returns [`Error::InvalidSecretEncoding`] if the payload is not valid
/// base64. Callers treat this as a protocol violation: the peer sent
/// bytes that are not a well-formed secret.
pub fn decode(payload: &str) -> Result<Vec<u8>, Error> {
    STANDARD
        .decode(payload)
        .map_err(|e| Error::InvalidSecretEncoding(e.to_string()))
}

/// A secret payload on the wire: its bytes, base64-encoded. It is wiped
/// when dropped, and its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretB64(Zeroizing<String>);

impl SecretB64 {
    /// `secret`, encoded.
    #[must_use]
    pub fn encode(secret: &[u8]) -> Self {
        Self(Zeroizing::new(encode(secret)))
    }

    /// The bytes, decoded, wiped when dropped.
    ///
    /// # Errors
    ///
    /// As [`decode`].
    pub fn decode(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        decode(&self.0).map(Zeroizing::new)
    }

    /// The payload as it is sent.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for SecretB64 {
    fn from(payload: String) -> Self {
        Self(Zeroizing::new(payload))
    }
}

impl std::fmt::Debug for SecretB64 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretB64(..)")
    }
}

impl Serialize for SecretB64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SecretB64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_secret_bytes_fits_a_frame() {
        // The value pin and the fit argument, together: a `*` -> `+`
        // mutant on the constant breaks the pin; the inequality proves
        // the bound actually serves the frame cap.
        assert_eq!(MAX_SECRET_BYTES, 32 * 1024);
        let encoded = encode(&vec![0xABu8; MAX_SECRET_BYTES]);
        assert!(
            encoded.len() < crate::MAX_FRAME_BYTES,
            "base64 of a maximum secret must fit in one frame"
        );
    }

    #[test]
    fn codec_round_trips_arbitrary_bytes() {
        // Arbitrary bytes, including non-UTF-8: the whole point of
        // base64 over raw JSON strings.
        let raw = vec![0x00, 0xFF, 0x80, b'a', 0x1F, 0x7F];
        assert_eq!(decode(&encode(&raw)).unwrap(), raw);
    }

    #[test]
    fn decode_rejects_garbage() {
        // Pins the error mapping: undecodable input must surface as the
        // typed protocol error, not a panic or a silent empty vec.
        let err = decode("!!! not base64 !!!").unwrap_err();
        assert!(
            matches!(err, Error::InvalidSecretEncoding(_)),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_secret_payload_round_trips_as_a_plain_string_and_never_shows() {
        let payload = SecretB64::encode(b"hello");
        assert_eq!(payload.as_str(), "aGVsbG8=");
        assert_eq!(*payload.decode().unwrap(), b"hello");
        let json = serde_json::to_string(&payload).unwrap();
        assert_eq!(json, r#""aGVsbG8=""#);
        assert_eq!(serde_json::from_str::<SecretB64>(&json).unwrap(), payload);
        assert_eq!(SecretB64::from(String::from("aGVsbG8=")), payload);
        assert_eq!(format!("{payload:?}"), "SecretB64(..)");
        assert!(SecretB64::from(String::from("!!")).decode().is_err());
    }

    #[test]
    fn encode_is_standard_base64() {
        // Pins the engine choice: standard alphabet with padding.
        assert_eq!(encode(b"hello"), "aGVsbG8=");
    }
}
