//! Byte soup: what an untrusted device could send, thrown at everything
//! that reads it, and failures at every step of a real exchange.
//!
//! Repeatable by design: noise from a fixed seed, so a failure here is the
//! same failure on every machine. Nothing may panic, and what's accepted
//! must be exactly what was sent.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use hatchling_vault::Authenticator as _;

use crate::cbor::{Value, decode, encode};
use crate::ctap2::{AuthData, Info};
use crate::ctaphid::{BROADCAST, Channel, Hid, INIT, REPORT, TransportError};
use crate::device::Device;
use crate::pin::cose_key;
use crate::soft::SoftKey;

const RP: &str = "soup.example";

/// Xorshift: small, and the same everywhere.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(limit).unwrap()).unwrap()
    }

    fn byte(&mut self) -> u8 {
        self.next().to_le_bytes()[0]
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.byte()).collect()
    }

    /// `sample`, with a few bytes changed, and perhaps cut short.
    fn spoiled(&mut self, sample: &[u8]) -> Vec<u8> {
        let mut bytes = sample.to_vec();
        for _ in 0..=self.below(4) {
            let at = self.below(bytes.len());
            let byte = self.byte();
            if let Some(slot) = bytes.get_mut(at) {
                *slot = byte;
            }
        }
        if self.below(4) == 0 {
            bytes.truncate(self.below(sample.len()));
        }
        bytes
    }
}

/// A `getInfo` reply, as a key writes one.
fn info() -> Vec<u8> {
    encode(&Value::map(vec![
        (
            Value::int(1),
            Value::Array(vec![Value::text("FIDO_2_0"), Value::text("FIDO_2_1")]),
        ),
        (
            Value::int(2),
            Value::Array(vec![Value::text("hmac-secret")]),
        ),
        (Value::int(3), Value::Bytes(vec![7; 16])),
        (
            Value::int(4),
            Value::map(vec![
                (Value::text("clientPin"), Value::Bool(true)),
                (Value::text("up"), Value::Bool(true)),
            ]),
        ),
        (Value::int(5), Value::int(1200)),
        (
            Value::int(6),
            Value::Array(vec![Value::int(2), Value::int(1)]),
        ),
        (Value::int(13), Value::int(4)),
    ]))
}

/// Authenticator data holding a new credential and an extension.
fn auth_data() -> Vec<u8> {
    [
        &[9u8; 32][..],
        &[0xc5],
        &[0, 0, 0, 1],
        &[3; 16],
        &[0, 16],
        &[4; 16],
        &encode(&cose_key(&[4u8; 65])),
        &encode(&Value::map(vec![(
            Value::text("hmac-secret"),
            Value::Bool(true),
        )])),
    ]
    .concat()
}

/// Whatever `decode` takes survives being written and read again, as the
/// same thing.
#[test]
fn the_decoder_takes_anything_and_means_only_what_it_was_sent() {
    let mut noise = Noise(0x9e37_79b9_7f4a_7c15);
    let samples = (info(), auth_data());
    let mut taken = 0;
    for round in 0..40_000 {
        let bytes = if round % 2 == 0 {
            let len = noise.below(48);
            noise.bytes(len)
        } else {
            let sample = if round % 4 == 1 {
                &samples.0
            } else {
                &samples.1
            };
            noise.spoiled(sample)
        };
        if let Ok(value) = decode(&bytes) {
            assert_eq!(decode(&encode(&value)).as_ref(), Ok(&value), "{bytes:02x?}");
            let _ = Info::parse(&value);
            taken += 1;
        }
    }
    assert!(
        taken > 100,
        "the soup reached the decoder's far side: {taken}"
    );
}

#[test]
fn replies_gone_wrong_are_refused_or_read_never_a_panic() {
    let mut noise = Noise(0x2545_f491_4f6c_dd1d);
    let info = info();
    let data = auth_data();
    assert!(AuthData::parse(&data).is_ok(), "the sample itself is good");
    assert!(Info::parse(&decode(&info).unwrap()).is_ok());
    for _ in 0..20_000 {
        let _ = AuthData::parse(&noise.spoiled(&data));
        if let Ok(value) = decode(&noise.spoiled(&info)) {
            let _ = Info::parse(&value);
        }
    }
}

/// A device that answers a few reports of noise, shaped enough to get
/// past the first checks, and then is gone.
struct Soup {
    noise: Noise,
    left: usize,
    nonce: [u8; 8],
}

impl Hid for Soup {
    fn send(&mut self, _: &[u8; REPORT]) -> Result<(), TransportError> {
        Ok(())
    }

    fn receive(&mut self, _: Duration) -> Result<Option<[u8; REPORT]>, TransportError> {
        if self.left == 0 {
            return Err(TransportError::Io(String::from("gone")));
        }
        self.left -= 1;
        let mut report = [0u8; REPORT];
        let bytes = self.noise.bytes(REPORT);
        report.copy_from_slice(&bytes);
        if self.noise.below(2) == 0 {
            // An answer to INIT, on the broadcast channel, with our nonce.
            report[..4].copy_from_slice(&BROADCAST);
            report[4] = INIT;
            report[5] = 0;
            report[6] = u8::try_from(8 + self.noise.below(16)).unwrap();
            report[7..15].copy_from_slice(&self.nonce);
        }
        Ok(Some(report))
    }
}

#[test]
fn a_channel_survives_any_reports() {
    let mut noise = Noise(0x1234_5678_9abc_def1);
    let nonce = crate::pin::random::<8>().unwrap();
    for _ in 0..20_000 {
        let left = noise.below(8);
        let seed = noise.next() | 1;
        let soup = Soup {
            noise: Noise(seed),
            left,
            nonce,
        };
        if let Ok(mut channel) = Channel::open(soup, nonce, Duration::from_secs(1)) {
            let _ = channel.cbor(4, &[], Duration::from_secs(1));
        }
    }
}

/// How a report goes wrong at the chosen call.
#[derive(Clone, Copy)]
enum Fault {
    /// The key is pulled out.
    Unplugged,
    /// A byte of a reply's body arrives changed.
    Flipped,
}

/// A key whose `at`th call to its transport goes wrong.
struct Faulty {
    key: SoftKey,
    calls: usize,
    at: usize,
    fault: Fault,
    happened: Rc<Cell<bool>>,
}

impl Faulty {
    fn now(&mut self) -> bool {
        self.calls += 1;
        let now = self.calls == self.at;
        if now {
            self.happened.set(true);
        }
        now
    }
}

impl Hid for Faulty {
    fn send(&mut self, report: &[u8; REPORT]) -> Result<(), TransportError> {
        if self.now() && matches!(self.fault, Fault::Unplugged) {
            return Err(TransportError::Io(String::from("unplugged")));
        }
        self.key.send(report)
    }

    fn receive(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT]>, TransportError> {
        let now = self.now();
        if now && matches!(self.fault, Fault::Unplugged) {
            return Err(TransportError::Io(String::from("unplugged")));
        }
        let mut report = self.key.receive(timeout)?;
        if now {
            if let Some(report) = &mut report {
                if let Some(byte) = report.get_mut(20 + self.at % 40) {
                    *byte ^= 0x55;
                }
            }
        }
        Ok(report)
    }
}

/// Every call of a whole unlock fails in turn. What comes back is the
/// right secret or a refusal, never a panic, and an unplugged key is never
/// a wrong secret.
#[test]
fn a_failure_at_any_step_is_a_refusal() {
    let honest = SoftKey::new();
    let state = honest.handle();
    let pin = || -> crate::device::PinPrompt {
        Box::new(|| Some(zeroize::Zeroizing::new(String::from("2468"))))
    };
    state.borrow_mut().pin = Some(String::from("2468"));
    let mut device = Device::open_with(honest, Some(pin()), Duration::from_secs(1)).unwrap();
    let made = device.make_credential(RP, b"user").unwrap();
    let salt = crate::pin::random::<32>().unwrap();
    let right = device.hmac_secret_verified(RP, &made.id, &salt).unwrap();
    drop(device);
    for fault in [Fault::Unplugged, Fault::Flipped] {
        let mut happened = 0;
        for at in 1..400 {
            let reached = Rc::new(Cell::new(false));
            let hid = Faulty {
                key: SoftKey::sharing(Rc::clone(&state)),
                calls: 0,
                at,
                fault,
                happened: Rc::clone(&reached),
            };
            let Ok(mut device) = Device::open_with(hid, Some(pin()), Duration::from_secs(1)) else {
                happened += 1;
                continue;
            };
            let got = device.hmac_secret_verified(RP, &made.id, &salt);
            if matches!(fault, Fault::Unplugged) {
                let wrong = got.as_ref().is_ok_and(|got| *got != right);
                assert!(!wrong, "call {at}");
            }
            state.borrow_mut().retries = 8;
            state.borrow_mut().wrong_in_a_row = 0;
            if !reached.get() {
                break;
            }
            happened += 1;
        }
        assert!(
            happened > 10,
            "the failures reached the exchange: {happened}"
        );
    }
}
