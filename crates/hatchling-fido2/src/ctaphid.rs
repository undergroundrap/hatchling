//! CTAPHID: CTAP2 messages framed in 64-byte HID reports (CTAP 2.1,
//! section 11.2).
//!
//! A message is one initialization packet (channel id, command, 16-bit
//! length, 57 bytes of data) and as many continuation packets as it needs
//! (channel id, sequence number 0..=127, 59 bytes of data). A channel id
//! comes from `INIT` on the broadcast channel, echoing the host's nonce.
//!
//! Everything the device sends is checked: packets for another channel are
//! skipped, a keepalive only waits (never past the deadline), an `ERROR`
//! packet ends the exchange, and a length, sequence number or command out
//! of place is a protocol error. The deadline is hard: past it the host
//! cancels and the exchange fails.

use std::time::{Duration, Instant};

/// A HID report's size, for every FIDO device.
pub const REPORT: usize = 64;
/// The broadcast channel, for `INIT`.
pub const BROADCAST: [u8; 4] = [0xff; 4];
/// The largest message: 57 bytes, then 128 continuations of 59.
pub const MAX_MESSAGE: usize = 57 + 128 * 59;

/// `INIT`: allocate a channel.
pub const INIT: u8 = 0x86;
/// `CBOR`: one CTAP2 command.
pub const CBOR: u8 = 0x90;
/// CTAPHID's `ERR_CHANNEL_BUSY`: another channel has the device.
const CHANNEL_BUSY: u8 = 0x06;

/// How long before asking a busy device again.
const BUSY_WAIT: Duration = Duration::from_millis(100);

/// The `INIT` capability that says a device speaks CTAP2.
const CAPABILITY_CBOR: u8 = 0x04;

/// `CANCEL`: stop the command in progress.
pub const CANCEL: u8 = 0x91;
/// `KEEPALIVE`: the device is still working, or waits for a touch.
pub const KEEPALIVE: u8 = 0xbb;
/// `ERROR`: the device refused the message.
pub const ERROR: u8 = 0xbf;

/// One connection to a FIDO device: whole reports out and in.
pub trait Hid {
    /// Sends one report.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Io`] if the device cannot be written.
    fn send(&mut self, report: &[u8; REPORT]) -> Result<(), TransportError>;

    /// The next report, or `None` if none came within `timeout`.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Io`] if the device cannot be read (it
    /// was unplugged, for one).
    fn receive(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT]>, TransportError>;
}

/// Why an exchange with the device failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The device could not be read or written.
    #[error("the security key could not be reached: {0}")]
    Io(String),
    /// No complete answer came before the deadline.
    #[error("the security key did not answer in time")]
    Timeout,
    /// The device sent something CTAPHID does not allow.
    #[error("the security key broke the CTAPHID protocol: {0}")]
    Protocol(&'static str),
    /// The device answered with a CTAPHID error code.
    #[error("the security key refused the message (CTAPHID error {0:#04x})")]
    Device(u8),
    /// The device doesn't speak CTAP2: an older U2F-only key.
    #[error("this key doesn't speak FIDO2 (CTAP2): it's an older U2F-only key")]
    NotCtap2,
}

/// The reports that carry `payload` for `command` on `channel`.
#[must_use]
pub fn packets(channel: [u8; 4], command: u8, payload: &[u8]) -> Vec<[u8; REPORT]> {
    let len = u16::try_from(payload.len())
        .unwrap_or(u16::MAX)
        .to_be_bytes();
    let (first, rest) = payload.split_at(payload.len().min(57));
    let mut init = [0u8; REPORT];
    for (slot, byte) in init
        .iter_mut()
        .zip(channel.iter().chain(&[command]).chain(&len).chain(first))
    {
        *slot = *byte;
    }
    let mut reports = vec![init];
    for (seq, chunk) in (0u8..).zip(rest.chunks(59)) {
        let mut cont = [0u8; REPORT];
        for (slot, byte) in cont
            .iter_mut()
            .zip(channel.iter().chain(&[seq]).chain(chunk))
        {
            *slot = *byte;
        }
        reports.push(cont);
    }
    reports
}

/// The parts of a report: its channel, then its command or sequence
/// byte, then the rest.
fn split(report: &[u8; REPORT]) -> ([u8; 4], u8, &[u8]) {
    let (head, body) = report.split_at(5);
    let mut channel = [0u8; 4];
    channel.copy_from_slice(head.get(..4).unwrap_or(&[0; 4]));
    (channel, head.get(4).copied().unwrap_or(0), body)
}

/// A message being reassembled.
struct Incoming {
    total: usize,
    data: Vec<u8>,
    next_seq: u8,
}

/// Sends `payload` as `command` on `channel`, and returns the answer to
/// it: the payload of the device's message with the same command.
///
/// # Errors
///
/// Returns [`TransportError`] if the device cannot be reached, refuses
/// the message, breaks the protocol, or does not finish by `deadline`.
/// Past the deadline a `CANCEL` is sent, best-effort.
pub fn exchange<H: Hid + ?Sized>(
    hid: &mut H,
    channel: [u8; 4],
    command: u8,
    payload: &[u8],
    deadline: Instant,
) -> Result<Vec<u8>, TransportError> {
    if payload.len() > MAX_MESSAGE {
        return Err(TransportError::Protocol("message too long"));
    }
    for report in packets(channel, command, payload) {
        hid.send(&report)?;
    }
    let answer = receive(hid, channel, command, deadline);
    if matches!(
        answer,
        Err(TransportError::Timeout | TransportError::Protocol(_))
    ) {
        if let Some(cancel) = packets(channel, CANCEL, &[]).first() {
            let _ = hid.send(cancel);
        }
    }
    answer
}

/// `INIT` on the broadcast channel with `nonce`: the channel id the
/// device gives, once it has said it speaks CTAP2.
fn init<H: Hid + ?Sized>(
    hid: &mut H,
    nonce: [u8; 8],
    timeout: Duration,
) -> Result<[u8; 4], TransportError> {
    let deadline = Instant::now() + timeout;
    for report in packets(BROADCAST, INIT, &nonce) {
        hid.send(&report)?;
    }
    loop {
        // Its own deadline: answers to other hosts' INITs must not keep it
        // waiting past it.
        if deadline.saturating_duration_since(Instant::now()).is_zero() {
            return Err(TransportError::Timeout);
        }
        let answer = receive(hid, BROADCAST, INIT, deadline)?;
        // An answer to another host's INIT on the broadcast channel
        // carries its nonce, not ours: wait for ours.
        if answer.get(..8) != Some(&nonce[..]) {
            continue;
        }
        let id: [u8; 4] = answer
            .get(8..12)
            .and_then(|id| id.try_into().ok())
            .ok_or(TransportError::Protocol("INIT answer too short"))?;
        if id == BROADCAST || id == [0; 4] {
            return Err(TransportError::Protocol("INIT gave a reserved channel"));
        }
        let capabilities = answer
            .get(16)
            .ok_or(TransportError::Protocol("INIT answer too short"))?;
        if capabilities & CAPABILITY_CBOR == 0 {
            return Err(TransportError::NotCtap2);
        }
        return Ok(id);
    }
}

/// Reads the device's answer to `command` on `channel`.
fn receive<H: Hid + ?Sized>(
    hid: &mut H,
    channel: [u8; 4],
    command: u8,
    deadline: Instant,
) -> Result<Vec<u8>, TransportError> {
    let mut incoming: Option<Incoming> = None;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(TransportError::Timeout);
        }
        let Some(report) = hid.receive(left)? else {
            return Err(TransportError::Timeout);
        };
        let (from, kind, body) = split(&report);
        if from != channel {
            continue;
        }
        if let Some(message) = incoming.as_mut() {
            if kind & 0x80 != 0 {
                return Err(TransportError::Protocol("a new message inside a message"));
            }
            if kind != message.next_seq {
                return Err(TransportError::Protocol("continuation out of sequence"));
            }
            message.next_seq = message.next_seq.wrapping_add(1);
            let want = message.total - message.data.len();
            message.data.extend(body.iter().take(want));
        } else {
            incoming = start(kind, body, command)?;
            if incoming.is_none() {
                continue;
            }
        }
        if let Some(message) = incoming.take_if(|message| message.data.len() == message.total) {
            return Ok(message.data);
        }
    }
}

/// The start of an answer from an initialization packet, or `None` for a
/// keepalive.
fn start(kind: u8, body: &[u8], command: u8) -> Result<Option<Incoming>, TransportError> {
    if kind & 0x80 == 0 {
        return Err(TransportError::Protocol("a continuation without a message"));
    }
    let total = usize::from(u16::from_be_bytes([
        body.first().copied().unwrap_or(0),
        body.get(1).copied().unwrap_or(0),
    ]));
    let data = body.get(2..).unwrap_or(&[]);
    match kind {
        KEEPALIVE => Ok(None),
        ERROR => Err(TransportError::Device(data.first().copied().unwrap_or(0))),
        _ if kind != command => Err(TransportError::Protocol("an answer to another command")),
        _ if total > MAX_MESSAGE => Err(TransportError::Protocol("message too long")),
        _ => Ok(Some(Incoming {
            total,
            data: data.iter().take(total).copied().collect(),
            next_seq: 0,
        })),
    }
}

/// A CTAPHID channel to one device.
pub struct Channel<H: Hid> {
    hid: H,
    id: [u8; 4],
    /// An exchange failed part way: the device may still answer it, so
    /// the next request takes a channel of its own, and that late answer
    /// goes unread on this one, never mistaken for its reply.
    stale: bool,
}

impl<H: Hid> Channel<H> {
    /// Opens a channel: `INIT` on the broadcast channel with `nonce`,
    /// waiting at most `timeout` for the device's answer echoing it.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] if the device does not answer, or
    /// answers with a malformed or reserved channel id.
    pub fn open(mut hid: H, nonce: [u8; 8], timeout: Duration) -> Result<Self, TransportError> {
        let id = init(&mut hid, nonce, timeout)?;
        Ok(Self {
            hid,
            id,
            stale: false,
        })
    }

    /// This channel's id.
    #[must_use]
    pub const fn id(&self) -> [u8; 4] {
        self.id
    }

    /// One CTAP2 command: `command` and its CBOR `parameters` out; the
    /// status byte and the CBOR after it back.
    ///
    /// # Errors
    ///
    /// As [`exchange`], and [`TransportError::Protocol`] for an empty
    /// answer.
    pub fn cbor(
        &mut self,
        command: u8,
        parameters: &[u8],
        timeout: Duration,
    ) -> Result<(u8, Vec<u8>), TransportError> {
        if self.stale {
            let nonce = crate::pin::random::<8>()
                .map_err(|_| TransportError::Protocol("no randomness for a channel"))?;
            self.id = init(&mut self.hid, nonce, timeout)?;
            self.stale = false;
        }
        let payload = [&[command][..], parameters].concat();
        let deadline = Instant::now() + timeout;
        let answer = loop {
            match exchange(&mut self.hid, self.id, CBOR, &payload, deadline) {
                // Another program is talking to the key: it's its turn,
                // not a failure.
                Err(TransportError::Device(CHANNEL_BUSY))
                    if deadline.saturating_duration_since(Instant::now()) > BUSY_WAIT =>
                {
                    std::thread::sleep(BUSY_WAIT);
                }
                answer => break answer,
            }
        };
        self.stale = matches!(
            answer,
            Err(TransportError::Timeout | TransportError::Protocol(_))
        );
        let answer = answer?;
        let (&status, rest) = answer
            .split_first()
            .ok_or(TransportError::Protocol("an empty CBOR answer"))?;
        Ok((status, rest.to_vec()))
    }
}

#[cfg(test)]
#[allow(
    clippy::redundant_pub_crate,
    reason = "other modules' tests drive these scripted reports"
)]
pub(crate) mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// A device that answers from a script, and records what it was sent.
    #[derive(Default)]
    pub(crate) struct Scripted {
        pub(crate) answers: VecDeque<Option<[u8; REPORT]>>,
        pub(crate) sent: Vec<[u8; REPORT]>,
        pub(crate) broken: bool,
    }

    impl Hid for Scripted {
        fn send(&mut self, report: &[u8; REPORT]) -> Result<(), TransportError> {
            if self.broken {
                return Err(TransportError::Io(String::from("unplugged")));
            }
            self.sent.push(*report);
            Ok(())
        }

        fn receive(&mut self, _: Duration) -> Result<Option<[u8; REPORT]>, TransportError> {
            if self.broken {
                return Err(TransportError::Io(String::from("unplugged")));
            }
            Ok(self.answers.pop_front().flatten())
        }
    }

    fn scripted(answers: Vec<[u8; REPORT]>) -> Scripted {
        Scripted {
            answers: answers.into_iter().map(Some).collect(),
            ..Scripted::default()
        }
    }

    const CID: [u8; 4] = [1, 2, 3, 4];

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn a_long_message_is_split_and_reassembled() {
        let payload: Vec<u8> = (0..200u8).collect();
        let reports = packets(CID, CBOR, &payload);
        assert_eq!(reports.len(), 4, "57 + 59 + 59 + 25");
        let first = reports.first().unwrap();
        assert_eq!(first.get(..7).unwrap(), [1, 2, 3, 4, CBOR, 0, 200]);
        assert_eq!(first.get(7..).unwrap(), payload.get(..57).unwrap());
        let second = reports.get(1).unwrap();
        assert_eq!(second.get(..5).unwrap(), [1, 2, 3, 4, 0]);
        assert_eq!(reports.get(3).unwrap().get(4), Some(&2));
        let mut hid = scripted(reports);
        assert_eq!(receive(&mut hid, CID, CBOR, soon()).unwrap(), payload);
        let empty = packets(CID, CANCEL, &[]);
        assert_eq!(empty.len(), 1);
        assert_eq!(
            empty.first().unwrap().get(..7).unwrap(),
            [1, 2, 3, 4, CANCEL, 0, 0]
        );
    }

    #[test]
    fn the_largest_message_fits_and_a_larger_one_is_refused() {
        let payload = vec![7u8; MAX_MESSAGE];
        let reports = packets(CID, CBOR, &payload);
        assert_eq!(reports.len(), 129);
        assert_eq!(reports.last().unwrap().get(4), Some(&127));
        let mut hid = scripted(reports);
        assert_eq!(
            receive(&mut hid, CID, CBOR, soon()).unwrap().len(),
            MAX_MESSAGE
        );
        let mut hid = Scripted::default();
        assert_eq!(
            exchange(&mut hid, CID, CBOR, &vec![0; MAX_MESSAGE + 1], soon()),
            Err(TransportError::Protocol("message too long"))
        );
        assert!(hid.sent.is_empty());
        // Exactly the limit goes out whole, and its answer comes back.
        let mut hid = scripted(packets(CID, CBOR, b"ok"));
        assert_eq!(
            exchange(&mut hid, CID, CBOR, &vec![0; MAX_MESSAGE], soon()).unwrap(),
            b"ok"
        );
        assert_eq!(hid.sent.len(), 129);
    }

    #[test]
    fn keepalives_and_other_channels_are_waited_through() {
        let mut answers = packets([9, 9, 9, 9], CBOR, b"not ours");
        answers.extend(packets(CID, KEEPALIVE, &[2]));
        answers.extend(packets(CID, CBOR, b"ours"));
        let mut hid = scripted(answers);
        assert_eq!(receive(&mut hid, CID, CBOR, soon()).unwrap(), b"ours");
    }

    #[test]
    fn an_error_packet_ends_the_exchange() {
        let mut hid = scripted(packets(CID, ERROR, &[0x06]));
        assert_eq!(
            receive(&mut hid, CID, CBOR, soon()),
            Err(TransportError::Device(0x06))
        );
        let mut hid = scripted(packets(CID, ERROR, &[]));
        assert_eq!(
            receive(&mut hid, CID, CBOR, soon()),
            Err(TransportError::Device(0))
        );
    }

    #[test]
    fn a_device_breaking_the_framing_is_refused() {
        let message = packets(CID, CBOR, &[5u8; 100]);
        let (first, second) = (*message.first().unwrap(), *message.get(1).unwrap());
        let mut skipped = second;
        if let Some(seq) = skipped.get_mut(4) {
            *seq = 1;
        }
        let restarted = first;
        for (answers, why) in [
            (vec![second], "a continuation without a message"),
            (vec![first, skipped], "continuation out of sequence"),
            (vec![first, restarted], "a new message inside a message"),
            (packets(CID, INIT, b"x"), "an answer to another command"),
        ] {
            let mut hid = scripted(answers);
            assert_eq!(
                receive(&mut hid, CID, CBOR, soon()),
                Err(TransportError::Protocol(why)),
                "{why}"
            );
        }
        let mut huge = [0u8; REPORT];
        huge.copy_from_slice(&first);
        if let Some(len) = huge.get_mut(5..7) {
            len.copy_from_slice(&u16::MAX.to_be_bytes());
        }
        let mut hid = scripted(vec![huge]);
        assert_eq!(
            receive(&mut hid, CID, CBOR, soon()),
            Err(TransportError::Protocol("message too long"))
        );
    }

    #[test]
    fn silence_past_the_deadline_cancels_and_fails() {
        let mut hid = Scripted {
            answers: VecDeque::from([None]),
            ..Scripted::default()
        };
        assert_eq!(
            exchange(&mut hid, CID, CBOR, b"\x04", soon()),
            Err(TransportError::Timeout)
        );
        let cancel = hid.sent.last().unwrap();
        assert_eq!(cancel.get(..7).unwrap(), [1, 2, 3, 4, CANCEL, 0, 0]);
        // A deadline already gone fails without waiting, and cancels.
        let mut hid = scripted(packets(CID, CBOR, b"late"));
        assert_eq!(
            exchange(&mut hid, CID, CBOR, b"\x04", Instant::now()),
            Err(TransportError::Timeout)
        );
        assert_eq!(hid.sent.last().unwrap().get(4), Some(&CANCEL));
        // Keepalives never extend it.
        let mut hid = Scripted {
            answers: std::iter::repeat_n(packets(CID, KEEPALIVE, &[2]).first().copied(), 3)
                .chain([None])
                .collect(),
            ..Scripted::default()
        };
        assert_eq!(
            receive(&mut hid, CID, CBOR, soon()),
            Err(TransportError::Timeout)
        );
    }

    #[test]
    fn an_unplugged_device_is_an_io_error() {
        let mut hid = Scripted {
            broken: true,
            ..Scripted::default()
        };
        assert!(matches!(
            exchange(&mut hid, CID, CBOR, b"\x04", soon()),
            Err(TransportError::Io(_))
        ));
    }

    /// The device's answer to INIT with `nonce`, giving `channel`.
    pub(crate) fn init_answer(nonce: [u8; 8], channel: [u8; 4]) -> Vec<[u8; REPORT]> {
        let payload = [&nonce[..], &channel, &[2, 5, 4, 0, 0x05]].concat();
        packets(BROADCAST, INIT, &payload)
    }

    #[test]
    fn a_channel_opens_only_on_an_answer_with_our_nonce() {
        let nonce = [8u8; 8];
        let mut answers = init_answer([7u8; 8], [5, 5, 5, 5]);
        answers.extend(init_answer(nonce, CID));
        let channel = Channel::open(scripted(answers), nonce, Duration::from_secs(5)).unwrap();
        assert_eq!(channel.id(), CID);
        let sent = channel.hid.sent.first().unwrap();
        assert_eq!(
            sent.get(..15).unwrap(),
            [0xff, 0xff, 0xff, 0xff, INIT, 0, 8, 8, 8, 8, 8, 8, 8, 8, 8]
        );
        for (channel_id, why) in [
            (BROADCAST, "INIT gave a reserved channel"),
            ([0; 4], "INIT gave a reserved channel"),
        ] {
            let opened = Channel::open(
                scripted(init_answer(nonce, channel_id)),
                nonce,
                Duration::from_secs(5),
            );
            assert_eq!(opened.err(), Some(TransportError::Protocol(why)));
        }
        // Answers that never carry our nonce end at the deadline.
        let strangers: Vec<[u8; REPORT]> = (0..1000)
            .flat_map(|_| init_answer([7u8; 8], [5, 5, 5, 5]))
            .collect();
        let started = Instant::now();
        assert_eq!(
            Channel::open(scripted(strangers), nonce, Duration::from_millis(1)).err(),
            Some(TransportError::Timeout)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        let short = packets(BROADCAST, INIT, &[&nonce[..], &[1, 2]].concat());
        assert_eq!(
            Channel::open(scripted(short), nonce, Duration::from_secs(5)).err(),
            Some(TransportError::Protocol("INIT answer too short"))
        );
        assert_eq!(
            Channel::open(Scripted::default(), nonce, Duration::from_secs(5)).err(),
            Some(TransportError::Timeout)
        );
    }

    #[test]
    fn a_cbor_command_returns_its_status_and_body() {
        let nonce = [3u8; 8];
        let mut answers = init_answer(nonce, CID);
        answers.extend(packets(CID, CBOR, &[0x00, 0xa0]));
        answers.extend(packets(CID, CBOR, &[]));
        let mut channel = Channel::open(scripted(answers), nonce, Duration::from_secs(5)).unwrap();
        assert_eq!(
            channel.cbor(0x04, &[], Duration::from_secs(5)).unwrap(),
            (0x00, vec![0xa0])
        );
        let sent = channel.hid.sent.last().unwrap();
        assert_eq!(sent.get(..8).unwrap(), [1, 2, 3, 4, CBOR, 0, 1, 0x04]);
        assert_eq!(
            channel.cbor(0x04, &[], Duration::from_secs(5)),
            Err(TransportError::Protocol("an empty CBOR answer"))
        );
    }

    #[test]
    fn errors_describe_themselves() {
        assert_eq!(
            TransportError::Device(0x06).to_string(),
            "the security key refused the message (CTAPHID error 0x06)"
        );
        assert_eq!(
            TransportError::Timeout.to_string(),
            "the security key did not answer in time"
        );
        assert_eq!(split(&[0; REPORT]).1, 0);
        assert!(start(KEEPALIVE, &[0, 1, 2], CBOR).unwrap().is_none());
    }
}
