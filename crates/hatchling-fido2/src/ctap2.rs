//! CTAP2's commands, status codes and replies, as this host uses them
//! (CTAP 2.1, section 6).
//!
//! Replies are untrusted: [`Info`] and [`AuthData`] are parsed strictly,
//! and everything a reply claims about whom it answers for is checked by
//! [`crate::device`] before it is believed.

use crate::cbor::{CborError, Value};
use crate::ctaphid::TransportError;
use crate::pin::{CryptoError, Protocol};

/// `authenticatorMakeCredential`.
pub const MAKE_CREDENTIAL: u8 = 0x01;
/// `authenticatorGetAssertion`.
pub const GET_ASSERTION: u8 = 0x02;
/// `authenticatorGetInfo`.
pub const GET_INFO: u8 = 0x04;
/// `authenticatorClientPIN`.
pub const CLIENT_PIN: u8 = 0x06;

/// `CTAP2_OK`.
pub const OK: u8 = 0x00;
/// `CTAP2_ERR_INVALID_CREDENTIAL`.
pub const INVALID_CREDENTIAL: u8 = 0x22;
/// `CTAP2_ERR_OPERATION_DENIED`.
pub const OPERATION_DENIED: u8 = 0x27;
/// `CTAP2_ERR_KEEPALIVE_CANCEL`.
pub const KEEPALIVE_CANCEL: u8 = 0x2d;
/// `CTAP2_ERR_NO_CREDENTIALS`.
pub const NO_CREDENTIALS: u8 = 0x2e;
/// `CTAP2_ERR_USER_ACTION_TIMEOUT`.
pub const USER_ACTION_TIMEOUT: u8 = 0x2f;
/// `CTAP2_ERR_PIN_INVALID`.
pub const PIN_INVALID: u8 = 0x31;
/// `CTAP2_ERR_PIN_BLOCKED`.
pub const PIN_BLOCKED: u8 = 0x32;
/// `CTAP2_ERR_PIN_AUTH_INVALID`.
pub const PIN_AUTH_INVALID: u8 = 0x33;
/// `CTAP2_ERR_PIN_AUTH_BLOCKED`.
pub const PIN_AUTH_BLOCKED: u8 = 0x34;
/// `CTAP2_ERR_PIN_NOT_SET`.
pub const PIN_NOT_SET: u8 = 0x35;
/// `CTAP2_ERR_PIN_REQUIRED`.
pub const PIN_REQUIRED: u8 = 0x36;

/// The `up` (user present) flag in authenticator data.
pub const FLAG_UP: u8 = 0x01;
/// The `uv` (user verified) flag.
pub const FLAG_UV: u8 = 0x04;
/// The `at` (attested credential data included) flag.
pub const FLAG_AT: u8 = 0x40;
/// The `ed` (extension data included) flag.
pub const FLAG_ED: u8 = 0x80;

/// Why a CTAP2 exchange failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Ctap2Error {
    /// The transport failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The authenticator answered with a CTAP2 error status.
    #[error("the security key answered with CTAP2 status {0:#04x}")]
    Status(u8),
    /// A reply that is not what CTAP2 promises.
    #[error("the security key's reply is malformed: {0}")]
    Malformed(&'static str),
    /// A reply that is well formed but answers for something else.
    #[error("the security key's reply does not match the request: {0}")]
    Mismatch(&'static str),
    /// A key agreement or decryption failed.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    /// The reply is not canonical CBOR.
    #[error(transparent)]
    Cbor(#[from] CborError),
    /// The key needs its PIN, and none was given.
    #[error("this security key has a PIN, and none was given")]
    NoPin,
    /// The key can't verify its user: it has no PIN set.
    #[error("this security key has no PIN set, so it can't ask for one")]
    NoPinSet,
}

/// What `authenticatorGetInfo` says, as far as this host needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// The protocol versions the device speaks (`FIDO_2_0`, ...).
    pub versions: Vec<String>,
    /// The extensions it supports.
    pub extensions: Vec<String>,
    /// Whether a PIN is set (`Some(true)`), supported but not set
    /// (`Some(false)`), or not supported (`None`).
    pub client_pin: Option<bool>,
    /// Whether it issues tokens with permissions (CTAP 2.1's
    /// `getPinUvAuthTokenUsingPinWithPermissions`).
    pub pin_uv_auth_token: bool,
    /// The PIN/UV auth protocols it speaks, in its order of preference.
    pub protocols: Vec<Protocol>,
}

impl Info {
    /// The `getInfo` reply.
    ///
    /// # Errors
    ///
    /// Returns [`Ctap2Error::Malformed`] unless it is a map with a
    /// non-empty versions list of text.
    pub fn parse(reply: &Value) -> Result<Self, Ctap2Error> {
        let texts = |key| -> Result<Vec<String>, Ctap2Error> {
            reply.at(key).map_or(Ok(Vec::new()), |list| {
                list.as_array()
                    .ok_or(Ctap2Error::Malformed(
                        "getInfo: a list that is not an array",
                    ))?
                    .iter()
                    .map(|item| match item {
                        Value::Text(text) => Ok(text.clone()),
                        Value::Unsigned(_)
                        | Value::Negative(_)
                        | Value::Bytes(_)
                        | Value::Array(_)
                        | Value::Map(_)
                        | Value::Bool(_)
                        | Value::Null => Err(Ctap2Error::Malformed(
                            "getInfo: a list item that is not text",
                        )),
                    })
                    .collect()
            })
        };
        let versions = texts(1)?;
        if versions.is_empty() {
            return Err(Ctap2Error::Malformed("getInfo: no versions"));
        }
        let option = |name| {
            reply
                .at(4)
                .and_then(|options| options.field(name))
                .and_then(Value::as_bool)
        };
        let protocols = reply
            .at(6)
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|item| item.as_int().and_then(Protocol::from_number))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            versions,
            extensions: texts(2)?,
            client_pin: option("clientPin"),
            pin_uv_auth_token: option("pinUvAuthToken") == Some(true),
            protocols,
        })
    }

    /// The PIN/UV auth protocol to use: 2 where offered, else 1 (the
    /// CTAP 2.0 default when none is listed).
    #[must_use]
    pub fn protocol(&self) -> Protocol {
        if self.protocols.contains(&Protocol::Two) {
            Protocol::Two
        } else {
            Protocol::One
        }
    }

    /// Whether the device supports `hmac-secret`.
    #[must_use]
    pub fn hmac_secret(&self) -> bool {
        self.extensions.iter().any(|name| name == "hmac-secret")
    }
}

/// Authenticator data (`WebAuthn` section 6.1), parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthData {
    /// `SHA-256` of the relying-party id it answers for.
    pub rp_id_hash: [u8; 32],
    /// Its flags.
    pub flags: u8,
    /// The credential it created, if any (`at`).
    pub credential_id: Option<Vec<u8>>,
    /// That credential's public key, a COSE key.
    pub credential_key: Option<Value>,
    /// Its extension outputs, if any (`ed`).
    pub extensions: Option<Value>,
}

impl AuthData {
    /// Authenticator data from its bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Ctap2Error`] for data that is short, claims parts it
    /// lacks, or has bytes left over.
    pub fn parse(bytes: &[u8]) -> Result<Self, Ctap2Error> {
        let short = Ctap2Error::Malformed("authenticator data too short");
        let (hash, rest) = bytes.split_at_checked(32).ok_or_else(|| short.clone())?;
        let rp_id_hash = <[u8; 32]>::try_from(hash).map_err(|_| short.clone())?;
        let (&flags, rest) = rest.split_first().ok_or_else(|| short.clone())?;
        // The signature counter: not kept by this host.
        let (_, mut rest) = rest.split_at_checked(4).ok_or_else(|| short.clone())?;
        let mut credential_id = None;
        let mut credential_key = None;
        if flags & FLAG_AT != 0 {
            // AAGUID, then the id's 16-bit length and the id.
            let (_, after) = rest.split_at_checked(16).ok_or_else(|| short.clone())?;
            let (len, after) = after.split_at_checked(2).ok_or_else(|| short.clone())?;
            let len = usize::from(u16::from_be_bytes(
                <[u8; 2]>::try_from(len).map_err(|_| short.clone())?,
            ));
            let (id, after) = after.split_at_checked(len).ok_or_else(|| short.clone())?;
            if id.is_empty() {
                return Err(Ctap2Error::Malformed("an empty credential id"));
            }
            // The credential's public key: one CBOR item.
            let (key, after) = crate::cbor::decode_prefix(after)?;
            credential_id = Some(id.to_vec());
            credential_key = Some(key);
            rest = after;
        }
        let mut extensions = None;
        if flags & FLAG_ED != 0 {
            let (value, after) = crate::cbor::decode_prefix(rest)?;
            if !matches!(value, Value::Map(_)) {
                return Err(Ctap2Error::Malformed("extensions that are not a map"));
            }
            extensions = Some(value);
            rest = after;
        }
        if !rest.is_empty() {
            return Err(Ctap2Error::Malformed("bytes after the authenticator data"));
        }
        Ok(Self {
            rp_id_hash,
            flags,
            credential_id,
            credential_key,
            extensions,
        })
    }

    /// Whether the user was present.
    #[must_use]
    pub const fn user_present(&self) -> bool {
        self.flags & FLAG_UP != 0
    }

    /// Whether the key verified its user (its PIN).
    #[must_use]
    pub const fn user_verified(&self) -> bool {
        self.flags & FLAG_UV != 0
    }
}

/// What the vault hears about a failure.
#[must_use]
pub fn to_authenticator_error(error: &Ctap2Error) -> hatchling_vault::AuthenticatorError {
    use hatchling_vault::AuthenticatorError;
    match error {
        Ctap2Error::Status(NO_CREDENTIALS | INVALID_CREDENTIAL) => {
            AuthenticatorError::UnknownCredential
        }
        Ctap2Error::Status(OPERATION_DENIED | USER_ACTION_TIMEOUT | KEEPALIVE_CANCEL)
        | Ctap2Error::Transport(TransportError::Timeout) => AuthenticatorError::NoUserPresence,
        Ctap2Error::Status(PIN_INVALID) => {
            AuthenticatorError::Device(String::from("the PIN is wrong"))
        }
        Ctap2Error::Status(PIN_BLOCKED | PIN_AUTH_BLOCKED) => AuthenticatorError::Device(
            String::from("the key's PIN is blocked: unplug it and plug it in again, or reset it"),
        ),
        Ctap2Error::NoPinSet => AuthenticatorError::NoVerification,
        Ctap2Error::Status(PIN_REQUIRED | PIN_NOT_SET | PIN_AUTH_INVALID) | Ctap2Error::NoPin => {
            AuthenticatorError::Device(String::from(
                "this security key needs its PIN for that, and none was given",
            ))
        }
        other @ (Ctap2Error::Transport(_)
        | Ctap2Error::Status(_)
        | Ctap2Error::Malformed(_)
        | Ctap2Error::Mismatch(_)
        | Ctap2Error::Crypto(_)
        | Ctap2Error::Cbor(_)) => AuthenticatorError::Device(other.to_string()),
    }
}

#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "the device tests build authenticator data with these helpers"
)]
pub(crate) mod tests {
    use super::*;
    use crate::cbor::encode;
    use hatchling_vault::AuthenticatorError;

    fn info_reply(options: Vec<(Value, Value)>, protocols: Vec<Value>) -> Value {
        Value::map(vec![
            (
                Value::int(1),
                Value::Array(vec![Value::text("FIDO_2_0"), Value::text("FIDO_2_1")]),
            ),
            (
                Value::int(2),
                Value::Array(vec![Value::text("hmac-secret")]),
            ),
            (Value::int(3), Value::Bytes(vec![0; 16])),
            (Value::int(4), Value::map(options)),
            (Value::int(6), Value::Array(protocols)),
        ])
    }

    #[test]
    fn info_reads_versions_extensions_pin_and_protocols() {
        let reply = info_reply(
            vec![
                (Value::text("clientPin"), Value::Bool(true)),
                (Value::text("pinUvAuthToken"), Value::Bool(true)),
            ],
            vec![Value::int(2), Value::int(1), Value::int(9)],
        );
        let info = Info::parse(&reply).unwrap();
        assert_eq!(info.versions, ["FIDO_2_0", "FIDO_2_1"]);
        assert!(info.hmac_secret());
        assert_eq!(info.client_pin, Some(true));
        assert!(info.pin_uv_auth_token);
        assert_eq!(info.protocols, [Protocol::Two, Protocol::One]);
        assert_eq!(info.protocol(), Protocol::Two);
        let plain = Info::parse(&info_reply(vec![], vec![])).unwrap();
        assert_eq!(plain.client_pin, None);
        assert!(!plain.pin_uv_auth_token);
        assert_eq!(plain.protocol(), Protocol::One);
        let only_one = Info::parse(&info_reply(vec![], vec![Value::int(1)])).unwrap();
        assert_eq!(only_one.protocol(), Protocol::One);
    }

    #[test]
    fn malformed_info_is_refused() {
        let bad_versions = Value::map(vec![(Value::int(1), Value::Array(vec![Value::int(1)]))]);
        let no_versions = Value::map(vec![(Value::int(2), Value::Array(vec![]))]);
        let not_array = Value::map(vec![
            (Value::int(1), Value::Array(vec![Value::text("FIDO_2_0")])),
            (Value::int(2), Value::text("hmac-secret")),
        ]);
        assert_eq!(
            Info::parse(&bad_versions),
            Err(Ctap2Error::Malformed(
                "getInfo: a list item that is not text"
            ))
        );
        assert_eq!(
            Info::parse(&no_versions),
            Err(Ctap2Error::Malformed("getInfo: no versions"))
        );
        assert_eq!(
            Info::parse(&not_array),
            Err(Ctap2Error::Malformed(
                "getInfo: a list that is not an array"
            ))
        );
        let no_hmac = Value::map(vec![(
            Value::int(1),
            Value::Array(vec![Value::text("U2F_V2")]),
        )]);
        assert!(!Info::parse(&no_hmac).unwrap().hmac_secret());
    }

    /// Authenticator data with the given parts.
    pub(crate) fn auth_data(
        rp_id_hash: [u8; 32],
        flags: u8,
        credential: Option<&[u8]>,
        extensions: Option<&Value>,
    ) -> Vec<u8> {
        let mut bytes = [&rp_id_hash[..], &[flags], &[0, 0, 0, 7]].concat();
        if let Some(id) = credential {
            bytes.extend_from_slice(&[0xaa; 16]);
            bytes.extend_from_slice(&u16::try_from(id.len()).unwrap().to_be_bytes());
            bytes.extend_from_slice(id);
            bytes.extend(encode(&Value::map(vec![(Value::int(1), Value::int(2))])));
        }
        if let Some(extensions) = extensions {
            bytes.extend(encode(extensions));
        }
        bytes
    }

    #[test]
    fn authenticator_data_yields_its_parts() {
        let extensions = Value::map(vec![(Value::text("hmac-secret"), Value::Bool(true))]);
        let bytes = auth_data(
            [1; 32],
            FLAG_UP | FLAG_AT | FLAG_ED,
            Some(b"cred"),
            Some(&extensions),
        );
        let data = AuthData::parse(&bytes).unwrap();
        assert_eq!(data.rp_id_hash, [1; 32]);
        assert!(data.user_present());
        assert_eq!(data.credential_id.as_deref(), Some(&b"cred"[..]));
        assert_eq!(
            data.credential_key,
            Some(Value::map(vec![(Value::int(1), Value::int(2))]))
        );
        assert_eq!(data.extensions, Some(extensions));
        let bare = AuthData::parse(&auth_data([2; 32], 0, None, None)).unwrap();
        assert!(!bare.user_present());
        assert_eq!((bare.credential_id, bare.extensions), (None, None));
        assert_eq!(bare.credential_key, None);
    }

    #[test]
    fn authenticator_data_that_lies_about_its_parts_is_refused() {
        let ext = Value::map(vec![(Value::text("x"), Value::Null)]);
        let full = auth_data(
            [1; 32],
            FLAG_UP | FLAG_AT | FLAG_ED,
            Some(b"id"),
            Some(&ext),
        );
        // Every truncation fails: a claimed part must be there.
        for cut in 0..full.len() {
            assert!(
                AuthData::parse(full.get(..cut).unwrap()).is_err(),
                "cut at {cut}"
            );
        }
        let mut trailing = full;
        trailing.push(0);
        assert_eq!(
            AuthData::parse(&trailing),
            Err(Ctap2Error::Malformed("bytes after the authenticator data"))
        );
        assert_eq!(
            AuthData::parse(&auth_data([1; 32], FLAG_AT, Some(b""), None)),
            Err(Ctap2Error::Malformed("an empty credential id"))
        );
        assert_eq!(
            AuthData::parse(&auth_data(
                [1; 32],
                FLAG_ED,
                None,
                Some(&Value::Array(vec![]))
            )),
            Err(Ctap2Error::Malformed("extensions that are not a map"))
        );
        // Unclaimed parts are leftovers.
        assert!(AuthData::parse(&auth_data([1; 32], FLAG_UP, Some(b"id"), None)).is_err());
        assert!(AuthData::parse(&auth_data([1; 32], FLAG_UP, None, Some(&ext))).is_err());
    }

    #[test]
    fn failures_reach_the_vault_as_what_they_mean() {
        for status in [NO_CREDENTIALS, INVALID_CREDENTIAL] {
            assert_eq!(
                to_authenticator_error(&Ctap2Error::Status(status)),
                AuthenticatorError::UnknownCredential
            );
        }
        for error in [
            Ctap2Error::Status(OPERATION_DENIED),
            Ctap2Error::Status(USER_ACTION_TIMEOUT),
            Ctap2Error::Status(KEEPALIVE_CANCEL),
            Ctap2Error::Transport(TransportError::Timeout),
        ] {
            assert_eq!(
                to_authenticator_error(&error),
                AuthenticatorError::NoUserPresence
            );
        }
        let said = |error: Ctap2Error| match to_authenticator_error(&error) {
            AuthenticatorError::Device(text) => text,
            other @ (AuthenticatorError::NoUserPresence
            | AuthenticatorError::UnknownCredential
            | AuthenticatorError::NoVerification) => {
                unreachable!("{other:?}")
            }
        };
        assert_eq!(said(Ctap2Error::Status(PIN_INVALID)), "the PIN is wrong");
        assert_eq!(
            to_authenticator_error(&Ctap2Error::NoPinSet),
            AuthenticatorError::NoVerification
        );
        for status in [PIN_BLOCKED, PIN_AUTH_BLOCKED] {
            assert!(said(Ctap2Error::Status(status)).contains("blocked"));
        }
        for error in [
            Ctap2Error::Status(PIN_REQUIRED),
            Ctap2Error::Status(PIN_NOT_SET),
            Ctap2Error::Status(PIN_AUTH_INVALID),
            Ctap2Error::NoPin,
        ] {
            assert!(said(error).contains("needs its PIN"));
        }
        assert!(said(Ctap2Error::Status(0x7f)).contains("0x7f"));
        assert!(said(Ctap2Error::Mismatch("x")).contains("does not match"));
        assert!(
            said(Ctap2Error::Transport(TransportError::Io(String::from(
                "gone"
            ))))
            .contains("gone")
        );
    }
}
