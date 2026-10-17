//! A FIDO2 security key over CTAPHID, as the vault's [`Authenticator`].
//!
//! The device moves bytes; the vault decides. Nothing a reply claims is
//! believed unchecked:
//!
//! - **Whom it answers for:** the relying-party hash must be ours, and an
//!   assertion's credential the one asked for.
//! - **Presence:** every reply must carry the user-present flag. A tap
//!   proves someone is there, not what they approved: which key and which
//!   change stay the vault's to bind.
//! - **Freshness:** an assertion for a credential made in this session is
//!   verified against that credential's public key, over its
//!   authenticator data and our fresh random client-data hash, so a
//!   replayed reply cannot slip a stale `hmac-secret` output into an
//!   enrollment. (Other credentials' public keys are not kept: a replay
//!   there yields a wrong output, and the vault simply does not open.)
//! - **Deadlines:** an operation waiting for a touch fails closed after
//!   [`PRESENCE_TIMEOUT`]; the vault is unchanged.
//!
//! A key with a PIN is asked for it to create a credential, and to answer
//! for one enrolled to need it ([`hatchling_vault::KeyCheck::Pin`]): the
//! PIN comes from the [`PinPrompt`], goes to the key as an encrypted hash,
//! and is not kept. Such an answer must carry the user-verified flag.

use std::time::Duration;

use hatchling_vault::{Authenticator, AuthenticatorError, Credential};
use ring::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::cbor::{Value, decode, encode};
use crate::ctap2::{
    AuthData, CLIENT_PIN, Ctap2Error, GET_ASSERTION, GET_INFO, Info, MAKE_CREDENTIAL, OK,
    to_authenticator_error,
};
use crate::ctaphid::{Channel, Hid};
use crate::pin::{Protocol, cose_point, encapsulate, random};

/// How long an operation waits for a touch.
pub const PRESENCE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a step that needs no touch may take.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(5);
/// The name the vault's credentials carry on the key.
pub const USER_NAME: &str = "hatchling vault";

/// Asks the operator for a key's PIN; `None` if they give none.
pub type PinPrompt = Box<dyn FnMut() -> Option<Zeroizing<String>>>;

/// `COSEAlgorithmIdentifier` ES256: ECDSA on P-256 with SHA-256.
const ES256: i64 = -7;
/// The permissions a PIN token asks for: make credential and get
/// assertion.
const PERMISSIONS: i64 = 0x03;

/// One FIDO2 security key.
pub struct Device<H: Hid> {
    channel: Channel<H>,
    info: Info,
    pin: Option<PinPrompt>,
    presence: Duration,
    /// The credentials made in this session, and their public keys.
    made: Vec<(Vec<u8>, [u8; 65])>,
}

impl<H: Hid> Device<H> {
    /// Opens a channel to the key on `hid` and reads what it supports.
    ///
    /// # Errors
    ///
    /// Returns [`Ctap2Error`] if the key does not answer, or its info is
    /// malformed.
    pub fn open(hid: H, pin: Option<PinPrompt>) -> Result<Self, Ctap2Error> {
        Self::open_with(hid, pin, PRESENCE_TIMEOUT)
    }

    /// [`Device::open`], waiting `presence` for each touch.
    ///
    /// # Errors
    ///
    /// As [`Device::open`].
    pub fn open_with(
        hid: H,
        pin: Option<PinPrompt>,
        presence: Duration,
    ) -> Result<Self, Ctap2Error> {
        let mut channel = Channel::open(hid, random::<8>()?, STEP_TIMEOUT)?;
        let reply = command(&mut channel, GET_INFO, None, STEP_TIMEOUT)?;
        Ok(Self {
            channel,
            info: Info::parse(&reply)?,
            pin,
            presence,
            made: Vec::new(),
        })
    }

    /// What the key supports.
    #[must_use]
    pub const fn info(&self) -> &Info {
        &self.info
    }

    /// The key's current key agreement key, for `protocol`.
    fn key_agreement(&mut self, protocol: Protocol) -> Result<Value, Ctap2Error> {
        let parameters = Value::map(vec![
            (Value::int(1), Value::int(protocol.number())),
            (Value::int(2), Value::int(2)),
        ]);
        command(
            &mut self.channel,
            CLIENT_PIN,
            Some(&parameters),
            STEP_TIMEOUT,
        )?
        .at(1)
        .cloned()
        .ok_or(Ctap2Error::Malformed("no key agreement key"))
    }

    /// A PIN token for `rp_id`: the operator's PIN, sent as the first 16
    /// bytes of its `SHA-256`, encrypted.
    fn pin_token(
        &mut self,
        protocol: Protocol,
        rp_id: &str,
    ) -> Result<Zeroizing<Vec<u8>>, Ctap2Error> {
        let pin = self
            .pin
            .as_mut()
            .and_then(|ask| ask())
            .ok_or(Ctap2Error::NoPin)?;
        let peer = self.key_agreement(protocol)?;
        let (ours, shared) = encapsulate(protocol, &peer)?;
        let digest = Zeroizing::new(<[u8; 32]>::from(Sha256::digest(pin.as_bytes())));
        let pin_hash = shared.encrypt(digest.get(..16).unwrap_or(&[]))?;
        let mut parameters = vec![
            (Value::int(1), Value::int(protocol.number())),
            (Value::int(3), ours),
            (Value::int(6), Value::Bytes(pin_hash)),
        ];
        if self.info.pin_uv_auth_token {
            // getPinUvAuthTokenUsingPinWithPermissions, for this rp only.
            parameters.push((Value::int(2), Value::int(9)));
            parameters.push((Value::int(9), Value::int(PERMISSIONS)));
            parameters.push((Value::int(10), Value::text(rp_id)));
        } else {
            // getPinToken (CTAP 2.0).
            parameters.push((Value::int(2), Value::int(5)));
        }
        let reply = command(
            &mut self.channel,
            CLIENT_PIN,
            Some(&Value::map(parameters)),
            STEP_TIMEOUT,
        )?;
        let sealed = reply
            .at(2)
            .and_then(Value::as_bytes)
            .ok_or(Ctap2Error::Malformed("no PIN token"))?;
        let token = shared.decrypt(sealed)?;
        if matches!(token.len(), 16 | 32) {
            Ok(token)
        } else {
            Err(Ctap2Error::Malformed("a PIN token of the wrong length"))
        }
    }

    fn create(&mut self, rp_id: &str, user_id: &[u8]) -> Result<Credential, Ctap2Error> {
        let protocol = self.info.protocol();
        let token = if self.info.client_pin == Some(true) {
            Some(self.pin_token(protocol, rp_id)?)
        } else {
            None
        };
        let client_data_hash = random::<32>()?;
        let mut parameters = vec![
            (Value::int(1), Value::Bytes(client_data_hash.to_vec())),
            (
                Value::int(2),
                Value::map(vec![(Value::text("id"), Value::text(rp_id))]),
            ),
            (
                Value::int(3),
                Value::map(vec![
                    (Value::text("id"), Value::Bytes(user_id.to_vec())),
                    (Value::text("name"), Value::text(USER_NAME)),
                ]),
            ),
            (
                Value::int(4),
                Value::Array(vec![Value::map(vec![
                    (Value::text("alg"), Value::int(ES256)),
                    (Value::text("type"), Value::text("public-key")),
                ])]),
            ),
            (
                Value::int(6),
                Value::map(vec![(Value::text("hmac-secret"), Value::Bool(true))]),
            ),
        ];
        if let Some(token) = &token {
            parameters.push((
                Value::int(8),
                Value::Bytes(protocol.authenticate(token, &client_data_hash)),
            ));
            parameters.push((Value::int(9), Value::int(protocol.number())));
        }
        let reply = command(
            &mut self.channel,
            MAKE_CREDENTIAL,
            Some(&Value::map(parameters)),
            self.presence,
        )?;
        let data = AuthData::parse(
            reply
                .at(2)
                .and_then(Value::as_bytes)
                .ok_or(Ctap2Error::Malformed("no authenticator data"))?,
        )?;
        checked(&data, rp_id)?;
        let id = data
            .credential_id
            .ok_or(Ctap2Error::Malformed("no credential was created"))?;
        let key = data
            .credential_key
            .as_ref()
            .map(cose_point)
            .transpose()?
            .ok_or(Ctap2Error::Malformed("no credential public key"))?;
        let hmac_secret = data
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.field("hmac-secret"))
            .and_then(Value::as_bool)
            == Some(true);
        self.made.push((id.clone(), key));
        Ok(Credential { id, hmac_secret })
    }

    fn derive(
        &mut self,
        rp_id: &str,
        credential_id: &[u8],
        salt: &[u8; 32],
        verified: bool,
    ) -> Result<[u8; 32], Ctap2Error> {
        let protocol = self.info.protocol();
        // The PIN first: its token proves this request, and the key then
        // answers under its verified-user secret.
        let token = if verified {
            if self.info.client_pin != Some(true) {
                return Err(Ctap2Error::NoPinSet);
            }
            Some(self.pin_token(protocol, rp_id)?)
        } else {
            None
        };
        let peer = self.key_agreement(protocol)?;
        let (ours, shared) = encapsulate(protocol, &peer)?;
        let salt_enc = shared.encrypt(salt)?;
        let mut input = vec![
            (Value::int(1), ours),
            (Value::int(3), Value::Bytes(shared.authenticate(&salt_enc))),
            (Value::int(2), Value::Bytes(salt_enc)),
        ];
        if protocol == Protocol::Two {
            input.push((Value::int(4), Value::int(protocol.number())));
        }
        let client_data_hash = random::<32>()?;
        let mut parameters = vec![
            (Value::int(1), Value::text(rp_id)),
            (Value::int(2), Value::Bytes(client_data_hash.to_vec())),
            (
                Value::int(3),
                Value::Array(vec![Value::map(vec![
                    (Value::text("id"), Value::Bytes(credential_id.to_vec())),
                    (Value::text("type"), Value::text("public-key")),
                ])]),
            ),
            (
                Value::int(4),
                Value::map(vec![(Value::text("hmac-secret"), Value::map(input))]),
            ),
            (
                Value::int(5),
                Value::map(vec![(Value::text("up"), Value::Bool(true))]),
            ),
        ];
        if let Some(token) = &token {
            parameters.push((
                Value::int(6),
                Value::Bytes(protocol.authenticate(token, &client_data_hash)),
            ));
            parameters.push((Value::int(7), Value::int(protocol.number())));
        }
        let reply = command(
            &mut self.channel,
            GET_ASSERTION,
            Some(&Value::map(parameters)),
            self.presence,
        )?;
        if let Some(credential) = reply.at(1) {
            let id =
                credential
                    .field("id")
                    .and_then(Value::as_bytes)
                    .ok_or(Ctap2Error::Malformed(
                        "an assertion credential without an id",
                    ))?;
            if id != credential_id {
                return Err(Ctap2Error::Mismatch("it answered for another credential"));
            }
        }
        let raw = reply
            .at(2)
            .and_then(Value::as_bytes)
            .ok_or(Ctap2Error::Malformed("no authenticator data"))?;
        let data = AuthData::parse(raw)?;
        checked(&data, rp_id)?;
        if verified && !data.user_verified() {
            return Err(Ctap2Error::Mismatch("it did not confirm it verified its user"));
        }
        if let Some((_, key)) = self.made.iter().find(|(id, _)| id == credential_id) {
            let signature = reply
                .at(3)
                .and_then(Value::as_bytes)
                .ok_or(Ctap2Error::Malformed("an assertion without a signature"))?;
            UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key)
                .verify(&[raw, &client_data_hash].concat(), signature)
                .map_err(|_| Ctap2Error::Mismatch("the assertion's signature does not verify"))?;
        }
        let sealed = data
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.field("hmac-secret"))
            .and_then(Value::as_bytes)
            .ok_or(Ctap2Error::Malformed("no hmac-secret output"))?;
        let output = shared.decrypt(sealed)?;
        <[u8; 32]>::try_from(output.as_slice())
            .map_err(|_| Ctap2Error::Malformed("an hmac-secret output of the wrong length"))
    }
}

/// The reply is for `rp_id`, with someone present.
fn checked(data: &AuthData, rp_id: &str) -> Result<(), Ctap2Error> {
    let ours: [u8; 32] = Sha256::digest(rp_id.as_bytes()).into();
    if data.rp_id_hash != ours {
        return Err(Ctap2Error::Mismatch(
            "it answered for another relying party",
        ));
    }
    if !data.user_present() {
        return Err(Ctap2Error::Mismatch(
            "it did not confirm a user was present",
        ));
    }
    Ok(())
}

/// One CTAP2 command: its status must be OK, and its body (if any) one
/// CBOR map.
fn command<H: Hid>(
    channel: &mut Channel<H>,
    command: u8,
    parameters: Option<&Value>,
    timeout: Duration,
) -> Result<Value, Ctap2Error> {
    let bytes = parameters.map(encode).unwrap_or_default();
    let (status, body) = channel.cbor(command, &bytes, timeout)?;
    if status != OK {
        return Err(Ctap2Error::Status(status));
    }
    if body.is_empty() {
        return Ok(Value::Map(Vec::new()));
    }
    let reply = decode(&body)?;
    if matches!(reply, Value::Map(_)) {
        Ok(reply)
    } else {
        Err(Ctap2Error::Malformed("a reply that is not a map"))
    }
}

impl<H: Hid> Authenticator for Device<H> {
    fn make_credential(
        &mut self,
        rp_id: &str,
        user_id: &[u8],
    ) -> Result<Credential, AuthenticatorError> {
        self.create(rp_id, user_id)
            .map_err(|error| to_authenticator_error(&error))
    }

    fn hmac_secret(
        &mut self,
        rp_id: &str,
        credential_id: &[u8],
        salt: &[u8; 32],
    ) -> Result<[u8; 32], AuthenticatorError> {
        self.derive(rp_id, credential_id, salt, false)
            .map_err(|error| to_authenticator_error(&error))
    }

    fn hmac_secret_verified(
        &mut self,
        rp_id: &str,
        credential_id: &[u8],
        salt: &[u8; 32],
    ) -> Result<[u8; 32], AuthenticatorError> {
        self.derive(rp_id, credential_id, salt, true)
            .map_err(|error| to_authenticator_error(&error))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use hatchling_vault::{Kek, Keyring, MasterKey, Tier};
    use hmac::{Hmac, Mac as _};

    use super::*;
    use crate::soft::{SoftKey, State, Tamper};

    const RP: &str = "hatchling.vault";

    /// A device on `key`, waiting at most a second for a touch.
    fn open(key: SoftKey, pin: Option<&str>) -> Device<SoftKey> {
        let pin = pin.map(|pin| {
            let pin = pin.to_owned();
            let prompt: PinPrompt = Box::new(move || Some(Zeroizing::new(pin.clone())));
            prompt
        });
        Device::open_with(key, pin, Duration::from_secs(1)).unwrap()
    }

    fn expected(state: &Rc<RefCell<State>>, id: &[u8], salt: &[u8; 32]) -> [u8; 32] {
        let state = state.borrow();
        let held = state.held.iter().find(|held| held.id == id).unwrap();
        let mut mac = <Hmac<Sha256>>::new_from_slice(&held.cred_random).unwrap();
        mac.update(salt);
        mac.finalize().into_bytes().into()
    }

    #[test]
    fn a_key_makes_a_credential_and_derives_its_secret_in_both_protocols() {
        for protocols in [vec![2, 1], vec![1]] {
            let key = SoftKey::new();
            let state = key.handle();
            state.borrow_mut().protocols.clone_from(&protocols);
            let mut device = open(key, None);
            assert_eq!(device.info().versions, ["FIDO_2_0", "FIDO_2_1"]);
            let credential = device.make_credential(RP, b"vault-id").unwrap();
            assert!(credential.hmac_secret);
            assert_eq!(state.borrow().held.len(), 1);
            let salt = [4u8; 32];
            let output = device.hmac_secret(RP, &credential.id, &salt).unwrap();
            assert_eq!(
                output,
                expected(&state, &credential.id, &salt),
                "{protocols:?}"
            );
            assert_eq!(
                device.hmac_secret(RP, &credential.id, &salt).unwrap(),
                output
            );
            assert_ne!(
                device.hmac_secret(RP, &credential.id, &[5; 32]).unwrap(),
                output
            );
            // A second session reaches the same credential: no session state needed.
            let mut again = open(SoftKey::sharing(Rc::clone(&state)), None);
            assert_eq!(
                again.hmac_secret(RP, &credential.id, &salt).unwrap(),
                output
            );
        }
    }

    #[test]
    fn what_a_key_does_not_hold_or_support_is_said_plainly() {
        let key = SoftKey::new();
        let state = key.handle();
        let mut device = open(key, None);
        assert_eq!(
            device.hmac_secret(RP, b"unknown", &[0; 32]),
            Err(AuthenticatorError::UnknownCredential)
        );
        let credential = device.make_credential(RP, b"u").unwrap();
        assert_eq!(
            device.hmac_secret("other.rp", &credential.id, &[0; 32]),
            Err(AuthenticatorError::UnknownCredential)
        );
        state.borrow_mut().present = false;
        assert_eq!(
            device.make_credential(RP, b"u"),
            Err(AuthenticatorError::NoUserPresence)
        );
        assert_eq!(
            device.hmac_secret(RP, &credential.id, &[0; 32]),
            Err(AuthenticatorError::NoUserPresence)
        );
        let plain = SoftKey::new();
        plain.state.borrow_mut().hmac_secret = false;
        let mut plain = open(plain, None);
        assert!(!plain.info().hmac_secret());
        assert!(!plain.make_credential(RP, b"u").unwrap().hmac_secret);
    }

    #[test]
    fn a_key_with_a_pin_asks_for_it_only_to_create() {
        for permissions in [true, false] {
            let key = SoftKey::new();
            let state = key.handle();
            state.borrow_mut().pin = Some(String::from("2468"));
            state.borrow_mut().permissions = permissions;
            let mut device = open(key, Some("2468"));
            let credential = device.make_credential(RP, b"u").unwrap();
            assert_eq!(
                state.borrow().commands,
                [GET_INFO, CLIENT_PIN, CLIENT_PIN, MAKE_CREDENTIAL],
                "key agreement, then the token, then the credential"
            );
            // Deriving needs a touch, not the PIN: no token is asked for.
            state.borrow_mut().commands.clear();
            device.hmac_secret(RP, &credential.id, &[1; 32]).unwrap();
            assert_eq!(state.borrow().commands, [CLIENT_PIN, GET_ASSERTION]);
        }
        let key = SoftKey::new();
        key.state.borrow_mut().pin = Some(String::from("2468"));
        let wrong = key.handle();
        let mut device = open(key, Some("1357"));
        assert_eq!(
            device.make_credential(RP, b"u"),
            Err(AuthenticatorError::Device(String::from("the PIN is wrong")))
        );
        assert!(wrong.borrow().held.is_empty());
        let key = SoftKey::new();
        key.state.borrow_mut().pin = Some(String::from("2468"));
        let mut device = open(key, None);
        assert!(matches!(
            device.make_credential(RP, b"u"),
            Err(AuthenticatorError::Device(text)) if text.contains("needs its PIN")
        ));
    }

    /// The error a device with `tamper` gives when making and then using a
    /// credential, if it gives one.
    fn hostile(tamper: Tamper) -> Option<AuthenticatorError> {
        let key = SoftKey::new();
        let state = key.handle();
        let mut device = open(key, None);
        state.borrow_mut().tamper = tamper;
        let made = device.make_credential(RP, b"u");
        let credential = match made {
            Ok(credential) => credential,
            Err(error) => return Some(error),
        };
        device.hmac_secret(RP, &credential.id, &[3; 32]).err()
    }

    #[test]
    fn a_lying_key_is_caught_before_the_vault_believes_it() {
        let said = |tamper| match hostile(tamper) {
            Some(AuthenticatorError::Device(text)) => text,
            other => unreachable!("{tamper:?}: {other:?}"),
        };
        assert!(said(Tamper::OtherRp).contains("another relying party"));
        assert!(said(Tamper::NoPresenceFlag).contains("user was present"));
        assert!(said(Tamper::OtherCredential).contains("another credential"));
        assert!(said(Tamper::ShortOutput).contains("wrong length"));
        assert!(said(Tamper::BadSignature).contains("signature"));
        assert!(said(Tamper::Garbage).contains("CBOR"));
        assert_eq!(
            hostile(Tamper::Silent),
            Some(AuthenticatorError::NoUserPresence)
        );
        assert_eq!(
            hostile(Tamper::WrongChannel),
            Some(AuthenticatorError::NoUserPresence)
        );
        assert_eq!(hostile(Tamper::Honest), None);
    }

    #[test]
    fn a_replayed_assertion_cannot_pass_as_fresh() {
        let key = SoftKey::new();
        let state = key.handle();
        let mut device = open(key, None);
        let credential = device.make_credential(RP, b"u").unwrap();
        let first = device.hmac_secret(RP, &credential.id, &[1; 32]).unwrap();
        state.borrow_mut().tamper = Tamper::Replay;
        // In the session that made it, the old signature is caught.
        assert!(matches!(
            device.hmac_secret(RP, &credential.id, &[2; 32]),
            Err(AuthenticatorError::Device(text)) if text.contains("signature")
        ));
        // In a later session, a replay yields an output that is not the
        // key's: it would open nothing.
        let mut later = open(SoftKey::sharing(Rc::clone(&state)), None);
        let replayed = later.hmac_secret(RP, &credential.id, &[1; 32]).unwrap();
        assert_ne!(replayed, first);
    }

    fn keyring(name: &str) -> (std::path::PathBuf, Keyring, Kek) {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-fido2-vault-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The keyring refuses a directory others can enter.
        std::fs::set_permissions(
            &dir,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let kek = Kek::from_bytes(random::<32>().unwrap());
        let path = dir.join("keyring");
        let keyring = Keyring::create(&path, &MasterKey::generate().unwrap(), &kek).unwrap();
        (dir, keyring, kek)
    }

    #[test]
    fn real_keys_enroll_and_unlock_the_vault_through_the_transport() {
        let (dir, mut keyring, kek) = keyring("enroll");
        let (one, two) = (SoftKey::new(), SoftKey::new());
        let states = [one.handle(), two.handle()];
        let (mut one, mut two) = (open(one, None), open(two, None));
        let unlocked = keyring.unlock_file(&kek).unwrap();
        keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut one, &mut two])
            .unwrap();
        let keyring = Keyring::load(&dir.join("keyring")).unwrap();
        assert_eq!(keyring.tier(), Tier::HardwareKey);
        for state in states {
            let mut session = open(SoftKey::sharing(state), None);
            assert!(keyring.unlock_hardware_key(&mut session).is_ok());
        }
        let mut stranger = open(SoftKey::new(), None);
        assert!(keyring.unlock_hardware_key(&mut stranger).is_err());
        let impostor = SoftKey::new();
        impostor.state.borrow_mut().tamper = Tamper::Impostor;
        let mut impostor = open(impostor, None);
        assert!(
            keyring.unlock_hardware_key(&mut impostor).is_err(),
            "answering for every credential opens nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_hostile_key_leaves_the_vault_as_it_was() {
        for tamper in [
            Tamper::OtherRp,
            Tamper::NoPresenceFlag,
            Tamper::OtherCredential,
            Tamper::ShortOutput,
            Tamper::BadSignature,
            Tamper::Garbage,
            Tamper::Silent,
            Tamper::WrongChannel,
        ] {
            let (dir, mut keyring, kek) = keyring("hostile");
            let bad = SoftKey::new();
            let bad_state = bad.handle();
            let (mut good, mut bad) = (open(SoftKey::new(), None), open(bad, None));
            bad_state.borrow_mut().tamper = tamper;
            let unlocked = keyring.unlock_file(&kek).unwrap();
            assert!(
                keyring
                    .enroll_hardware_keys(&unlocked, &mut [&mut good, &mut bad])
                    .is_err(),
                "{tamper:?}"
            );
            let after = Keyring::load(&dir.join("keyring")).unwrap();
            assert_eq!(after.tier(), Tier::File, "{tamper:?}");
            assert!(after.unlock_file(&kek).is_ok(), "{tamper:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn each_step_answers_on_its_own() {
        for (protocols, protocol) in [(vec![2, 1], Protocol::Two), (vec![1], Protocol::One)] {
            let key = SoftKey::new();
            let state = key.handle();
            state.borrow_mut().pin = Some(String::from("2468"));
            state.borrow_mut().protocols = protocols;
            let prompt: PinPrompt = Box::new(|| Some(Zeroizing::new(String::from("2468"))));
            let mut device = Device::open(key, Some(prompt)).unwrap();
            assert_eq!(device.info().protocol(), protocol);
            let peer = device.key_agreement(protocol).unwrap();
            assert!(cose_point(&peer).is_ok());
            let token = device.pin_token(protocol, RP).unwrap();
            assert_eq!(token.len(), 32);
            let credential = device.create(RP, b"u").unwrap();
            let salt = [6u8; 32];
            assert_eq!(
                device.derive(RP, &credential.id, &salt, false).unwrap(),
                expected(&state, &credential.id, &salt)
            );
        }
    }

    #[test]
    fn a_reply_must_be_for_us_and_present() {
        let data = |hash: [u8; 32], flags: u8| AuthData {
            rp_id_hash: hash,
            flags,
            credential_id: None,
            credential_key: None,
            extensions: None,
        };
        let ours: [u8; 32] = Sha256::digest(RP.as_bytes()).into();
        assert!(checked(&data(ours, crate::ctap2::FLAG_UP), RP).is_ok());
        assert_eq!(
            checked(&data([0; 32], crate::ctap2::FLAG_UP), RP),
            Err(Ctap2Error::Mismatch(
                "it answered for another relying party"
            ))
        );
        assert_eq!(
            checked(&data(ours, 0), RP),
            Err(Ctap2Error::Mismatch(
                "it did not confirm a user was present"
            ))
        );
    }

    #[test]
    fn a_command_needs_ok_and_a_map() {
        let key = SoftKey::new();
        let mut device = open(key, None);
        assert_eq!(
            command(&mut device.channel, 0x55, None, STEP_TIMEOUT),
            Err(Ctap2Error::Status(0x01))
        );
        assert_eq!(USER_NAME, "hatchling vault");
        assert_eq!(ES256, -7);
        assert_eq!(PERMISSIONS, 3);
        assert!(PRESENCE_TIMEOUT > STEP_TIMEOUT);
    }

    #[test]
    fn a_key_enrolled_with_its_pin_answers_only_after_it() {
        use hatchling_vault::KeyCheck;
        let (dir, mut keyring, kek) = keyring("pin-check");
        let (one, two) = (SoftKey::new(), SoftKey::new());
        let states = [one.handle(), two.handle()];
        for state in &states {
            state.borrow_mut().pin = Some(String::from("2468"));
        }
        let (mut one, mut two) = (open(one, Some("2468")), open(two, Some("2468")));
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let rotated = keyring
            .enroll_hardware_keys_checked(&unlocked, &mut [&mut one, &mut two], KeyCheck::Pin)
            .unwrap();
        keyring.finish_rotation(&rotated.rotation).unwrap();
        let keyring = Keyring::load(&dir.join("keyring")).unwrap();
        assert_eq!(keyring.tier(), Tier::HardwareKey);
        for state in &states {
            let mut right = open(SoftKey::sharing(Rc::clone(state)), Some("2468"));
            assert!(keyring.unlock_hardware_key(&mut right).is_ok());
            for wrong in [Some("0000"), None] {
                let mut unverified = open(SoftKey::sharing(Rc::clone(state)), wrong);
                assert!(keyring.unlock_hardware_key(&mut unverified).is_err(), "{wrong:?}");
            }
        }
        // A key with no PIN set can't be asked for one.
        let (fresh_dir, mut fresh, fresh_kek) = self::keyring("pin-unset");
        let unlocked = fresh.unlock_file(&fresh_kek).unwrap();
        let (mut a, mut b) = (open(SoftKey::new(), None), open(SoftKey::new(), None));
        let err = fresh
            .enroll_hardware_keys_checked(&unlocked, &mut [&mut a, &mut b], KeyCheck::Pin)
            .unwrap_err();
        let shown = format!("{err:?}");
        assert!(shown.contains("NoVerification"), "{shown}");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&fresh_dir);
    }

    #[test]
    fn an_answer_under_the_pin_must_say_the_key_verified_its_user() {
        let key = SoftKey::new();
        let state = key.handle();
        state.borrow_mut().pin = Some(String::from("2468"));
        let mut device = open(key, Some("2468"));
        let credential = device.make_credential(RP, b"u").unwrap();
        assert!(device.hmac_secret_verified(RP, &credential.id, &[3; 32]).is_ok());
        state.borrow_mut().tamper = Tamper::NoVerifiedFlag;
        assert!(matches!(
            device.hmac_secret_verified(RP, &credential.id, &[3; 32]),
            Err(AuthenticatorError::Device(text)) if text.contains("verified its user")
        ));
        // Without the PIN, no such flag is asked for.
        assert!(device.hmac_secret(RP, &credential.id, &[3; 32]).is_ok());
    }
}
