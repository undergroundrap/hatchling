//! A software FIDO2 authenticator speaking CTAPHID and CTAP2 from the
//! device's side, for this crate's tests: honest by default, and hostile
//! on request ([`Tamper`]).
//!
//! Each credential has its own `CredRandom` (the `hmac-secret` key), a
//! second one for answers after its PIN is checked (`CredRandomWithUV`),
//! and its own P-256 signing key; `hmac-secret` is `HMAC-SHA-256` of the
//! salt under one of them, sent encrypted under the key agreement, as
//! CTAP2 defines it.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use hmac::{Hmac, Mac as _};
use ring::agreement::EphemeralPrivateKey;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair as _};
use sha2::{Digest as _, Sha256};

use crate::cbor::{Value, decode, encode};
use crate::ctap2::{
    CLIENT_PIN, FLAG_AT, FLAG_ED, FLAG_UP, FLAG_UV, GET_ASSERTION, GET_INFO, MAKE_CREDENTIAL,
    NO_CREDENTIALS, NOT_ALLOWED, PIN_AUTH_BLOCKED, PIN_AUTH_INVALID, PIN_BLOCKED, PIN_INVALID,
    PIN_POLICY_VIOLATION, PIN_REQUIRED, USER_ACTION_TIMEOUT,
};
use crate::ctaphid::{
    BROADCAST, CANCEL, CBOR, ERROR, Hid, INIT, KEEPALIVE, REPORT, TransportError, packets,
};
use crate::pin::{Protocol, agree, agreement_key, cose_key, random};

/// The ways a hostile key lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tamper {
    /// An honest key.
    Honest,
    /// It answers for another relying party.
    OtherRp,
    /// Its replies lack the user-present flag.
    NoPresenceFlag,
    /// Its replies under a PIN token lack the user-verified flag.
    NoVerifiedFlag,
    /// Its assertion names another credential.
    OtherCredential,
    /// It replays its previous assertion.
    Replay,
    /// It never answers a CTAP2 command.
    Silent,
    /// Its `hmac-secret` output is one block short.
    ShortOutput,
    /// Its assertion signature does not verify.
    BadSignature,
    /// Its replies are not CBOR.
    Garbage,
    /// It answers on another channel.
    WrongChannel,
    /// It answers for any credential, with a secret of its own.
    Impostor,
}

/// One credential the key holds.
pub(crate) struct Held {
    rp_id: String,
    pub(crate) id: Vec<u8>,
    pub(crate) cred_random: [u8; 32],
    /// The secret its answers come from once the PIN is checked.
    pub(crate) cred_random_uv: [u8; 32],
    key: EcdsaKeyPair,
}

/// The key's state, shared with the test that drives it.
#[allow(
    clippy::struct_excessive_bools,
    reason = "a test double's switches, each flipped on its own"
)]
pub(crate) struct State {
    pub(crate) held: Vec<Held>,
    pub(crate) pin: Option<String>,
    /// PIN tries left before the key locks itself.
    pub(crate) retries: u8,
    /// Wrong PINs since it was plugged in: three, and it wants replugging.
    pub(crate) wrong_in_a_row: u8,
    /// The shortest PIN it takes, as its info says.
    pub(crate) min_pin_length: i64,
    /// The `alg` of the keys it makes.
    pub(crate) credential_alg: i64,
    /// The answer it owes a cancelled request: sent when it's cancelled.
    pub(crate) owes: Option<([u8; 4], Vec<u8>)>,
    /// How many times it says it's busy before it answers.
    pub(crate) busy: u32,
    /// How many pre-flights it has answered.
    pub(crate) silent: u32,
    /// How many `INIT`s it has answered.
    pub(crate) inits: u8,
    /// The `INIT` capabilities it states: CTAP2 unless changed.
    pub(crate) capabilities: u8,
    pub(crate) present: bool,
    pub(crate) hmac_secret: bool,
    pub(crate) protocols: Vec<i64>,
    pub(crate) permissions: bool,
    pub(crate) tamper: Tamper,
    pub(crate) remembers: bool,
    pub(crate) commands: Vec<u8>,
    agreement: Option<EphemeralPrivateKey>,
    /// The PIN token it gave last: cleared, it has forgotten it.
    pub(crate) token: Option<Vec<u8>>,
    last_assertion: Option<Vec<u8>>,
}

/// A software key: its CTAPHID side, and its shared state.
pub(crate) struct SoftKey {
    pub(crate) state: Rc<RefCell<State>>,
    out: VecDeque<[u8; REPORT]>,
    incoming: Option<(u8, usize, Vec<u8>)>,
    channel: [u8; 4],
}

impl SoftKey {
    /// An honest key speaking both PIN/UV protocols, with no PIN set.
    pub(crate) fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(State {
                held: Vec::new(),
                pin: None,
                retries: 8,
                wrong_in_a_row: 0,
                min_pin_length: 4,
                credential_alg: -7,
                owes: None,
                busy: 0,
                silent: 0,
                inits: 0,
                capabilities: 0x05,
                present: true,
                hmac_secret: true,
                protocols: vec![2, 1],
                permissions: true,
                tamper: Tamper::Honest,
                remembers: true,
                commands: Vec::new(),
                agreement: None,
                token: None,
                last_assertion: None,
            })),
            out: VecDeque::new(),
            incoming: None,
            channel: [0x0a, 0x0b, 0x0c, 0x0d],
        }
    }

    /// The state, to inspect or change while a device owns the key.
    pub(crate) fn handle(&self) -> Rc<RefCell<State>> {
        Rc::clone(&self.state)
    }

    /// The same key again, as a new session sees it.
    pub(crate) fn sharing(state: Rc<RefCell<State>>) -> Self {
        Self {
            state,
            ..Self::new()
        }
    }

    fn answer(&mut self, channel: [u8; 4], command: u8, payload: &[u8]) {
        self.out.extend(packets(channel, command, payload));
    }

    fn message(&mut self, channel: [u8; 4], command: u8, payload: &[u8]) {
        if command == INIT && channel == BROADCAST {
            // A channel of its own each time, as a real key gives.
            let number = self.state.borrow().inits.wrapping_add(1);
            self.state.borrow_mut().inits = number;
            self.channel = [0x0a, 0x0b, 0x0c, number];
            let capabilities = self.state.borrow().capabilities;
            let reply = [payload, &self.channel, &[2, 5, 4, 0, capabilities]].concat();
            self.answer(BROADCAST, INIT, &reply);
            return;
        }
        if command == CANCEL {
            // A cancelled request is still answered: KEEPALIVE_CANCEL.
            let owed = self.state.borrow_mut().owes.take();
            if let Some((to, reply)) = owed {
                self.answer(to, CBOR, &reply);
            }
            return;
        }
        if command != CBOR || channel != self.channel {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            if state.busy > 0 {
                state.busy -= 1;
                drop(state);
                self.answer(channel, ERROR, &[0x06]);
                return;
            }
        }
        let tamper = self.state.borrow().tamper;
        if tamper == Tamper::Silent {
            self.state.borrow_mut().owes = Some((channel, vec![0x2d]));
            return;
        }
        let Some((&code, rest)) = payload.split_first() else {
            return;
        };
        self.state.borrow_mut().commands.push(code);
        let reply = self.cbor(code, rest);
        let to = if tamper == Tamper::WrongChannel {
            [9, 9, 9, 9]
        } else {
            self.channel
        };
        let no_touch = decode(rest).is_ok_and(|asked| asks_no_touch(&asked));
        if matches!(code, MAKE_CREDENTIAL | GET_ASSERTION) && !no_touch {
            // It waits for a touch: the host sees a keepalive first.
            self.answer(to, KEEPALIVE, &[2]);
        }
        self.answer(to, CBOR, &reply);
    }

    fn cbor(&self, code: u8, rest: &[u8]) -> Vec<u8> {
        let parameters = if rest.is_empty() {
            Value::Map(Vec::new())
        } else {
            match decode(rest) {
                Ok(value) => value,
                Err(_) => return vec![0x12],
            }
        };
        let answer = match code {
            GET_INFO => Ok(self.info()),
            CLIENT_PIN => self.client_pin(&parameters),
            MAKE_CREDENTIAL => self.make(&parameters),
            GET_ASSERTION => self.assert(&parameters),
            _ => Err(0x01),
        };
        match answer {
            Ok(_) if self.state.borrow().tamper == Tamper::Garbage => vec![0x00, 0xff],
            Ok(value) => [&[0x00][..], &encode(&value)].concat(),
            Err(status) => vec![status],
        }
    }

    fn info(&self) -> Value {
        let state = self.state.borrow();
        let mut options = vec![(Value::text("up"), Value::Bool(true))];
        options.push((Value::text("clientPin"), Value::Bool(state.pin.is_some())));
        if state.permissions {
            options.push((Value::text("pinUvAuthToken"), Value::Bool(true)));
        }
        let extensions = if state.hmac_secret {
            vec![Value::text("hmac-secret")]
        } else {
            Vec::new()
        };
        Value::map(vec![
            (
                Value::int(1),
                Value::Array(vec![Value::text("FIDO_2_0"), Value::text("FIDO_2_1")]),
            ),
            (Value::int(2), Value::Array(extensions)),
            (Value::int(3), Value::Bytes(vec![0x5a; 16])),
            (Value::int(4), Value::map(options)),
            (
                Value::int(6),
                Value::Array(state.protocols.iter().map(|n| Value::int(*n)).collect()),
            ),
            (Value::int(0x0d), Value::int(state.min_pin_length)),
        ])
    }

    fn client_pin(&self, parameters: &Value) -> Result<Value, u8> {
        let protocol = parameters
            .at(1)
            .and_then(Value::as_int)
            .and_then(Protocol::from_number)
            .ok_or(0x02u8)?;
        match parameters.at(2).and_then(Value::as_int) {
            Some(1) => Ok(Value::map(vec![(
                Value::int(3),
                Value::int(i64::from(self.state.borrow().retries)),
            )])),
            Some(3) => self.set_pin(protocol, parameters),
            Some(2) => {
                let (private, public) = agreement_key().map_err(|_| 0x7fu8)?;
                self.state.borrow_mut().agreement = Some(private);
                Ok(Value::map(vec![(Value::int(1), public)]))
            }
            Some(5 | 9) => {
                let mut state = self.state.borrow_mut();
                let private = state.agreement.take().ok_or(0x02u8)?;
                let shared = agree(protocol, private, parameters.at(3).ok_or(0x02u8)?)
                    .map_err(|_| 0x02u8)?;
                let sent = parameters.at(6).and_then(Value::as_bytes).ok_or(0x02u8)?;
                if state.retries == 0 {
                    return Err(PIN_BLOCKED);
                }
                if state.wrong_in_a_row >= 3 {
                    return Err(PIN_AUTH_BLOCKED);
                }
                let hash = shared.decrypt(sent).map_err(|_| PIN_INVALID)?;
                let pin = state.pin.clone().ok_or(0x35u8)?;
                let expected = Sha256::digest(pin.as_bytes());
                if hash.as_slice() != expected.get(..16).unwrap_or(&[]) {
                    state.retries = state.retries.saturating_sub(1);
                    state.wrong_in_a_row = state.wrong_in_a_row.saturating_add(1);
                    return Err(if state.retries == 0 {
                        PIN_BLOCKED
                    } else {
                        PIN_INVALID
                    });
                }
                state.retries = 8;
                state.wrong_in_a_row = 0;
                let token = random::<32>().map_err(|_| 0x7fu8)?.to_vec();
                state.token = Some(token.clone());
                let sealed = shared.encrypt(&token).map_err(|_| 0x7fu8)?;
                Ok(Value::map(vec![(Value::int(2), Value::Bytes(sealed))]))
            }
            _ => Err(0x02),
        }
    }

    /// `setPIN`: a new PIN on a key with none, checked as CTAP2 checks
    /// it, its padding included.
    fn set_pin(&self, protocol: Protocol, parameters: &Value) -> Result<Value, u8> {
        let mut state = self.state.borrow_mut();
        if state.pin.is_some() {
            return Err(NOT_ALLOWED);
        }
        let private = state.agreement.take().ok_or(0x02u8)?;
        let shared =
            agree(protocol, private, parameters.at(3).ok_or(0x02u8)?).map_err(|_| 0x02u8)?;
        let sealed = parameters.at(5).and_then(Value::as_bytes).ok_or(0x02u8)?;
        let auth = parameters.at(4).and_then(Value::as_bytes).ok_or(0x02u8)?;
        if shared.authenticate(sealed) != auth {
            return Err(PIN_AUTH_INVALID);
        }
        let padded = shared.decrypt(sealed).map_err(|_| PIN_AUTH_INVALID)?;
        if padded.len() != 64 {
            return Err(PIN_POLICY_VIOLATION);
        }
        let end = padded.iter().position(|byte| *byte == 0).unwrap_or(64);
        let pin = std::str::from_utf8(padded.get(..end).unwrap_or(&[]))
            .map_err(|_| PIN_POLICY_VIOLATION)?;
        let long_enough =
            i64::try_from(pin.chars().count()).is_ok_and(|n| n >= state.min_pin_length);
        if !long_enough || pin.len() > 63 {
            return Err(PIN_POLICY_VIOLATION);
        }
        state.pin = Some(pin.to_owned());
        Ok(Value::map(Vec::new()))
    }

    /// The PIN check a credential needs when a PIN is set.
    fn pin_checked(&self, parameters: &Value, client_data_hash: &[u8]) -> Result<(), u8> {
        let state = self.state.borrow();
        if state.pin.is_none() {
            return Ok(());
        }
        let given = parameters
            .at(8)
            .and_then(Value::as_bytes)
            .ok_or(PIN_REQUIRED)?;
        let protocol = parameters
            .at(9)
            .and_then(Value::as_int)
            .and_then(Protocol::from_number)
            .ok_or(PIN_AUTH_INVALID)?;
        token_proves(&state, protocol, client_data_hash, given)
    }

    fn make(&self, parameters: &Value) -> Result<Value, u8> {
        let tamper = self.state.borrow().tamper;
        let client_data_hash = parameters.at(1).and_then(Value::as_bytes).ok_or(0x02u8)?;
        let rp_id = match parameters.at(2).and_then(|rp| rp.field("id")) {
            Some(Value::Text(id)) => id.clone(),
            _ => return Err(0x02),
        };
        // As an older key may: wanting the names CTAP2 calls optional.
        let names = parameters.at(2).and_then(|rp| rp.field("name")).is_some()
            && parameters
                .at(3)
                .and_then(|user| user.field("displayName"))
                .is_some();
        if !names {
            return Err(0x14);
        }
        self.pin_checked(parameters, client_data_hash)?;
        if !self.state.borrow().present {
            return Err(USER_ACTION_TIMEOUT);
        }
        let asked = parameters
            .at(6)
            .and_then(|extensions| extensions.field("hmac-secret"))
            .and_then(Value::as_bool)
            == Some(true);
        let id = random::<16>().map_err(|_| 0x7fu8)?.to_vec();
        let cred_random = random::<32>().map_err(|_| 0x7fu8)?;
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
            .map_err(|_| 0x7fu8)?;
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .map_err(|_| 0x7fu8)?;
        let point = <[u8; 65]>::try_from(key.public_key().as_ref()).map_err(|_| 0x7fu8)?;
        let mut public = cose_key(&point);
        let alg = self.state.borrow().credential_alg;
        if let Value::Map(entries) = &mut public {
            for (label, value) in entries.iter_mut() {
                if label.as_int() == Some(3) {
                    *value = Value::int(alg);
                }
            }
        }
        let hmac = self.state.borrow().hmac_secret && asked;
        let mut data = [
            &rp_hash(tamper, &rp_id)[..],
            &[flags(tamper, FLAG_AT | FLAG_ED)],
            &[0, 0, 0, 1],
            &[0x5a; 16],
            &u16::try_from(id.len()).unwrap_or(0).to_be_bytes(),
            &id,
            &encode(&public),
        ]
        .concat();
        data.extend(encode(&Value::map(vec![(
            Value::text("hmac-secret"),
            Value::Bool(hmac),
        )])));
        let mut state = self.state.borrow_mut();
        // A touch used up the token's permissions, as on CTAP 2.1.
        state.token = None;
        if state.remembers {
            state.held.push(Held {
                rp_id,
                id,
                cred_random,
                cred_random_uv: random::<32>().map_err(|_| 0x7fu8)?,
                key,
            });
        }
        Ok(Value::map(vec![
            (Value::int(1), Value::text("none")),
            (Value::int(2), Value::Bytes(data)),
            (Value::int(3), Value::Map(Vec::new())),
        ]))
    }

    fn assert(&self, parameters: &Value) -> Result<Value, u8> {
        let tamper = self.state.borrow().tamper;
        if tamper == Tamper::Replay {
            if let Some(old) = self.state.borrow().last_assertion.clone() {
                return decode(&old).map_err(|_| 0x7fu8);
            }
        }
        let rp_id = match parameters.at(1) {
            Some(Value::Text(id)) => id.clone(),
            _ => return Err(0x02),
        };
        let client_data_hash = parameters.at(2).and_then(Value::as_bytes).ok_or(0x02u8)?;
        let allowed: Vec<&[u8]> = parameters
            .at(3)
            .and_then(Value::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|entry| entry.field("id").and_then(Value::as_bytes))
            .collect();
        let mut state = self.state.borrow_mut();
        let found = state
            .held
            .iter()
            .position(|held| held.rp_id == rp_id && allowed.contains(&held.id.as_slice()));
        let held = match (found, tamper) {
            (Some(held), _) => held,
            (None, Tamper::Impostor) => {
                let id = allowed.first().map(|id| id.to_vec()).unwrap_or_default();
                state.held.push(impostor(&rp_id, id)?);
                state.held.len() - 1
            }
            (None, _) => return Err(NO_CREDENTIALS),
        };
        // Pre-flight: it holds the credential, and that's all it says.
        if asks_no_touch(parameters) {
            state.silent = state.silent.saturating_add(1);
            return Ok(Value::map(Vec::new()));
        }
        if !state.present {
            return Err(USER_ACTION_TIMEOUT);
        }
        // A PIN token's proof over this request: the user is verified.
        let verified = match parameters.at(6).and_then(Value::as_bytes) {
            None => false,
            Some(given) => {
                let protocol = parameters
                    .at(7)
                    .and_then(Value::as_int)
                    .and_then(Protocol::from_number)
                    .ok_or(PIN_AUTH_INVALID)?;
                token_proves(&state, protocol, client_data_hash, given)?;
                true
            }
        };
        let sealed = sealed_output(&mut state, parameters, held, tamper, verified)?;
        let held = state.held.get(held).ok_or(NO_CREDENTIALS)?;
        let uv = if verified && tamper != Tamper::NoVerifiedFlag {
            FLAG_UV
        } else {
            0
        };
        let data = [
            &rp_hash(tamper, &rp_id)[..],
            &[flags(tamper, FLAG_ED | uv)],
            &[0, 0, 0, 2],
            &encode(&Value::map(vec![(
                Value::text("hmac-secret"),
                Value::Bytes(sealed),
            )])),
        ]
        .concat();
        let mut signature = held
            .key
            .sign(
                &SystemRandom::new(),
                &[&data[..], client_data_hash].concat(),
            )
            .map_err(|_| 0x7fu8)?
            .as_ref()
            .to_vec();
        if tamper == Tamper::BadSignature {
            if let Some(last) = signature.last_mut() {
                *last ^= 1;
            }
        }
        let id = claimed(tamper, &held.id);
        let reply = Value::map(vec![
            (
                Value::int(1),
                Value::map(vec![
                    (Value::text("id"), Value::Bytes(id)),
                    (Value::text("type"), Value::text("public-key")),
                ]),
            ),
            (Value::int(2), Value::Bytes(data)),
            (Value::int(3), Value::Bytes(signature)),
        ]);
        state.last_assertion = Some(encode(&reply));
        state.token = None;
        Ok(reply)
    }
}

/// The encrypted `hmac-secret` output for credential `held`, from the
/// extension input in `parameters`: the salt checked and decrypted under
/// the key agreement, `HMAC-SHA-256(CredRandom, salt)` encrypted back.
/// The credential an assertion names: `id`, unless tampered with.
fn claimed(tamper: Tamper, id: &[u8]) -> Vec<u8> {
    if tamper == Tamper::OtherCredential {
        vec![0xee; 16]
    } else {
        id.to_vec()
    }
}

/// Whether an assertion's options ask for no touch: a pre-flight.
fn asks_no_touch(parameters: &Value) -> bool {
    parameters
        .at(5)
        .and_then(|options| options.field("up"))
        .and_then(Value::as_bool)
        == Some(false)
}

/// Whether `given` is the proof, over `client_data_hash`, of the PIN
/// token the key gave last (and hasn't forgotten).
fn token_proves(
    state: &State,
    protocol: Protocol,
    client_data_hash: &[u8],
    given: &[u8],
) -> Result<(), u8> {
    let token = state.token.as_deref().ok_or(PIN_AUTH_INVALID)?;
    if protocol.authenticate(token, client_data_hash) == given {
        Ok(())
    } else {
        Err(PIN_AUTH_INVALID)
    }
}

fn sealed_output(
    state: &mut State,
    parameters: &Value,
    held: usize,
    tamper: Tamper,
    verified: bool,
) -> Result<Vec<u8>, u8> {
    let input = parameters
        .at(4)
        .and_then(|extensions| extensions.field("hmac-secret"))
        .ok_or(0x02u8)?;
    let protocol = input
        .at(4)
        .and_then(Value::as_int)
        .map_or(Some(Protocol::One), Protocol::from_number)
        .ok_or(0x02u8)?;
    let private = state.agreement.take().ok_or(0x02u8)?;
    let shared = agree(protocol, private, input.at(1).ok_or(0x02u8)?).map_err(|_| 0x02u8)?;
    let salt_enc = input.at(2).and_then(Value::as_bytes).ok_or(0x02u8)?;
    if input.at(3).and_then(Value::as_bytes) != Some(shared.authenticate(salt_enc).as_slice()) {
        return Err(0x02);
    }
    let salt = shared.decrypt(salt_enc).map_err(|_| 0x02u8)?;
    let held = state.held.get(held).ok_or(NO_CREDENTIALS)?;
    let secret = if verified {
        &held.cred_random_uv
    } else {
        &held.cred_random
    };
    let mut mac = <Hmac<Sha256>>::new_from_slice(secret).map_err(|_| 0x7fu8)?;
    mac.update(&salt);
    let output = mac.finalize().into_bytes();
    let plain = if tamper == Tamper::ShortOutput {
        output.get(..16).unwrap_or(&[])
    } else {
        output.as_slice()
    };
    shared.encrypt(plain).map_err(|_| 0x7fu8)
}

/// A credential an impostor claims to hold: `id`, with a secret and
/// signing key of its own.
fn impostor(rp_id: &str, id: Vec<u8>) -> Result<Held, u8> {
    let rng = SystemRandom::new();
    let pkcs8 =
        EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).map_err(|_| 0x7fu8)?;
    Ok(Held {
        rp_id: rp_id.to_owned(),
        id,
        cred_random: random::<32>().map_err(|_| 0x7fu8)?,
        cred_random_uv: random::<32>().map_err(|_| 0x7fu8)?,
        key: EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
            .map_err(|_| 0x7fu8)?,
    })
}

/// The relying-party hash a reply carries: a hostile key's is another's.
fn rp_hash(tamper: Tamper, rp_id: &str) -> [u8; 32] {
    let rp = if tamper == Tamper::OtherRp {
        "evil.example"
    } else {
        rp_id
    };
    Sha256::digest(rp.as_bytes()).into()
}

/// A reply's flags: a hostile key leaves out user presence.
const fn flags(tamper: Tamper, more: u8) -> u8 {
    if matches!(tamper, Tamper::NoPresenceFlag) {
        more
    } else {
        FLAG_UP | more
    }
}

impl Hid for SoftKey {
    fn send(&mut self, report: &[u8; REPORT]) -> Result<(), TransportError> {
        let (head, body) = report.split_at(5);
        let channel: [u8; 4] = head
            .get(..4)
            .and_then(|c| c.try_into().ok())
            .unwrap_or([0; 4]);
        let kind = head.get(4).copied().unwrap_or(0);
        if kind & 0x80 != 0 {
            let total = usize::from(u16::from_be_bytes([
                body.first().copied().unwrap_or(0),
                body.get(1).copied().unwrap_or(0),
            ]));
            let data: Vec<u8> = body
                .get(2..)
                .unwrap_or(&[])
                .iter()
                .take(total)
                .copied()
                .collect();
            self.incoming = Some((kind, total, data));
        } else if let Some((_, total, data)) = self.incoming.as_mut() {
            let want = *total - data.len();
            data.extend(body.iter().take(want));
        }
        if let Some((kind, _, data)) = self
            .incoming
            .take_if(|(_, total, data)| data.len() == *total)
        {
            self.message(channel, kind, &data);
        }
        Ok(())
    }

    fn receive(&mut self, _: Duration) -> Result<Option<[u8; REPORT]>, TransportError> {
        Ok(self.out.pop_front())
    }
}
