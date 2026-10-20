//! Agent side of the egress protocol.
//!
//! The framing and message vocabulary live in [`hatchling_core`]; this
//! module is the agent's behavior: mint a surrogate for a destination,
//! then perform requests with it. The agent never touches the network
//! and never holds a real credential.
//! Must only be called on a [`Ready`](crate::Ready) cell — the typestate
//! enforces this.

use std::os::unix::net::UnixStream;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    EGRESS_PART_BYTES, EgressOp, EgressResult, SessionRequest, SessionResponse, decode_secret,
    encode_secret, fits_one_frame,
};

use crate::Error;

/// An outbound HTTP response, as returned to the agent.
#[derive(Debug, Clone)]
pub struct EgressResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

/// Mints a surrogate for `destination`.
///
/// The broker checks the peer's grant for the destination, then the
/// egress allowlist. The surrogate is bound to (`peer_uid`, `destination`,
/// `expiry`) and is valid for `ttl_secs` seconds (capped at 1 hour by the
/// broker).
///
/// # Errors
///
/// Returns [`Error::EgressDenied`] if the broker holds no grant or the
/// destination is not on the allowlist, [`Error::EgressFailed`] on a
/// broker-side failure, [`Error::EgressIo`] on transport failure, or
/// [`Error::EgressProtocol`] on a malformed broker reply.
pub fn mint_surrogate(
    stream: &UnixStream,
    destination: &str,
    ttl_secs: u64,
) -> Result<String, Error> {
    let op = EgressOp::MintSurrogate {
        destination: destination.to_owned(),
        ttl_secs,
    };
    match request(stream, &op)? {
        EgressResult::Surrogate { surrogate_b64 } => Ok(surrogate_b64),
        EgressResult::Denied => Err(Error::EgressDenied {
            destination: destination.to_owned(),
        }),
        EgressResult::EgressError => Err(Error::EgressFailed),
        other @ (EgressResult::Ok { .. }
        | EgressResult::Staged { .. }
        | EgressResult::Part { .. }) => Err(unexpected_reply(&op, &other)),
    }
}

/// Performs an outbound HTTP request with a previously minted surrogate.
///
/// The surrogate must have been minted for `destination`; the broker
/// validates the surrogate's signature, peer binding, destination
/// binding, and expiry before forwarding.
///
/// A body too large to go in one frame with the request is staged
/// first, in parts of [`EGRESS_PART_BYTES`]; an answer too large for one
/// frame is read after its `Ok`, a part at a time. Either way the
/// caller sees one request and one whole answer, within the egress
/// proxy's bounds.
///
/// # Errors
///
/// Returns [`Error::EgressDenied`] if the surrogate is invalid or the
/// destination is not allowed, [`Error::EgressFailed`] on a broker-side
/// failure, [`Error::EgressIo`] on transport failure, or
/// [`Error::EgressProtocol`] on a malformed broker reply.
pub fn egress_request(
    stream: &UnixStream,
    surrogate_b64: &str,
    method: &str,
    destination: &str,
    path: &str,
    headers: Vec<(String, String)>,
    body: &[u8],
) -> Result<EgressResponse, Error> {
    let mut op = EgressOp::Egress {
        method: method.to_owned(),
        destination: destination.to_owned(),
        path: path.to_owned(),
        headers,
        body_b64: encode_secret(body),
        surrogate_b64: surrogate_b64.to_owned(),
        staged: 0,
    };
    if !fits_one_frame(&SessionRequest::Egress(op.clone())) {
        stage(stream, body)?;
        if let EgressOp::Egress {
            body_b64, staged, ..
        } = &mut op
        {
            *body_b64 = String::new();
            *staged = u64::try_from(body.len()).unwrap_or(u64::MAX);
        }
    }
    match request(stream, &op)? {
        EgressResult::Ok {
            status,
            headers,
            body_b64,
            more,
        } => {
            let mut body = decode_part(&body_b64)?;
            read_held(stream, &mut body, more)?;
            Ok(EgressResponse {
                status,
                headers,
                body,
            })
        }
        EgressResult::Denied => Err(Error::EgressDenied {
            destination: destination.to_owned(),
        }),
        EgressResult::EgressError => Err(Error::EgressFailed),
        other @ (EgressResult::Surrogate { .. }
        | EgressResult::Staged { .. }
        | EgressResult::Part { .. }) => Err(unexpected_reply(&op, &other)),
    }
}

/// Sends `body` to the broker's stage in parts of [`EGRESS_PART_BYTES`],
/// for the `Egress` op that follows to send.
fn stage(stream: &UnixStream, body: &[u8]) -> Result<(), Error> {
    for part in body.chunks(EGRESS_PART_BYTES) {
        let op = EgressOp::Stage {
            part_b64: encode_secret(part),
        };
        match request(stream, &op)? {
            EgressResult::Staged { .. } => {}
            EgressResult::EgressError => return Err(Error::EgressFailed),
            other @ (EgressResult::Surrogate { .. }
            | EgressResult::Ok { .. }
            | EgressResult::Part { .. }
            | EgressResult::Denied) => return Err(unexpected_reply(&op, &other)),
        }
    }
    Ok(())
}

/// Reads the `more` bytes of an answer's body the broker holds onto
/// `body`, a part at a time. Each part must be exactly what the broker
/// said it would take off what it held, and not empty: a reply that
/// doesn't add up is a protocol violation, never a loop.
fn read_held(stream: &UnixStream, body: &mut Vec<u8>, mut more: u64) -> Result<(), Error> {
    while more > 0 {
        let op = EgressOp::Read;
        match request(stream, &op)? {
            EgressResult::Part {
                body_b64,
                more: left,
            } => {
                let part = decode_part(&body_b64)?;
                let step = more.checked_sub(left).filter(|step| *step > 0);
                if step != u64::try_from(part.len()).ok() {
                    return Err(Error::EgressProtocol {
                        reason: String::from(
                            "a body part doesn't match what the broker said it held",
                        ),
                    });
                }
                body.extend_from_slice(&part);
                more = left;
            }
            EgressResult::EgressError => return Err(Error::EgressFailed),
            other @ (EgressResult::Surrogate { .. }
            | EgressResult::Ok { .. }
            | EgressResult::Staged { .. }
            | EgressResult::Denied) => return Err(unexpected_reply(&op, &other)),
        }
    }
    Ok(())
}

/// Decodes a body, or a part of one, from the broker.
fn decode_part(body_b64: &str) -> Result<Vec<u8>, Error> {
    decode_secret(body_b64).map_err(|error| Error::EgressProtocol {
        reason: error.to_string(),
    })
}

/// Maps a frame-layer failure onto the egress vocabulary.
fn frame_error(error: hatchling_core::Error) -> Error {
    if let hatchling_core::Error::FrameIo { source } = error {
        return Error::EgressIo { source };
    }
    Error::EgressProtocol {
        reason: error.to_string(),
    }
}

/// Sends one egress op and reads the broker's reply.
///
/// The session now speaks [`SessionRequest`]/[`SessionResponse`]
/// envelopes (v3). Every op gets exactly one reply.
fn request(stream: &UnixStream, op: &EgressOp) -> Result<EgressResult, Error> {
    let request = SessionRequest::Egress(op.clone());
    let payload = serde_json::to_vec(&request).map_err(|e| Error::EgressProtocol {
        reason: format!("failed to encode egress op: {e}"),
    })?;
    write_frame(stream, &payload).map_err(frame_error)?;
    let reply = read_frame(stream).map_err(frame_error)?;
    let response: SessionResponse =
        serde_json::from_slice(&reply).map_err(|e| Error::EgressProtocol {
            reason: format!("broker reply is not a valid session response: {e}"),
        })?;
    match response {
        SessionResponse::Egress(result) => Ok(result),
        SessionResponse::Vault(_) => Err(Error::EgressProtocol {
            reason: String::from("broker gave a vault reply to an egress op"),
        }),
        SessionResponse::Channel(_) => Err(Error::EgressProtocol {
            reason: String::from("broker gave a channel reply to an egress op"),
        }),
        SessionResponse::Memory(_) => Err(Error::EgressProtocol {
            reason: String::from("broker gave a memory reply to an egress op"),
        }),
        SessionResponse::Skill(_) => Err(Error::EgressProtocol {
            reason: String::from("broker gave a skill reply to an egress op"),
        }),
        SessionResponse::Goals(_)
        | SessionResponse::Query(_)
        | SessionResponse::Schedule(_)
        | SessionResponse::Hook(_)
        | SessionResponse::Artifact(_)
        | SessionResponse::Space(_)
        | SessionResponse::Subagent(_)
        | SessionResponse::Web(_) => Err(Error::EgressProtocol {
            reason: String::from("broker gave a goals or query reply to an egress op"),
        }),
    }
}

/// Builds the protocol-violation error for a reply that does not match
/// the op that was sent.
fn unexpected_reply(op: &EgressOp, result: &EgressResult) -> Error {
    Error::EgressProtocol {
        reason: format!("broker gave an unexpected reply to egress op: {op:?} -> {result:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// Short read timeout for the fake-broker sockets. A missing frame
    /// (e.g. a mutant that skips `write_frame`) must fail the peer's
    /// read in seconds, not hang the suite.
    fn timeouts(client: &UnixStream, server: &UnixStream) {
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
    }

    /// `mint_surrogate` returns the broker's surrogate verbatim. Pins
    /// the return-value substitution mutants (`Ok(String::new())`,
    /// `Ok("xyzzy".into())`).
    #[test]
    fn mint_surrogate_returns_broker_value() {
        let (client, server) = UnixStream::pair().unwrap();
        timeouts(&client, &server);
        let expected = String::from("real-surrogate-from-broker-12345");
        let expected_clone = expected.clone();
        // Fake broker: read the op, reply with a known surrogate.
        let handle = std::thread::spawn(move || {
            let frame = read_frame(&server).unwrap();
            let request: SessionRequest = serde_json::from_slice(&frame).unwrap();
            assert!(matches!(
                request,
                SessionRequest::Egress(EgressOp::MintSurrogate { .. })
            ));
            let response = SessionResponse::Egress(EgressResult::Surrogate {
                surrogate_b64: expected_clone,
            });
            let payload = serde_json::to_vec(&response).unwrap();
            write_frame(&server, &payload).unwrap();
        });

        let got = mint_surrogate(&client, "example.com:443", 3600).unwrap();
        assert_eq!(got, expected);
        assert!(!got.is_empty());
        assert_ne!(got, "xyzzy");
        handle.join().unwrap();
    }

    /// A fake broker: answers each egress op it reads with the next of
    /// `replies`, then closes; hands back the ops it read.
    fn scripted(
        replies: Vec<EgressResult>,
    ) -> (UnixStream, std::thread::JoinHandle<Vec<EgressOp>>) {
        let (client, server) = UnixStream::pair().unwrap();
        timeouts(&client, &server);
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in replies {
                let Ok(frame) = read_frame(&server) else {
                    break;
                };
                if let Ok(SessionRequest::Egress(op)) = serde_json::from_slice(&frame) {
                    seen.push(op);
                }
                let payload = serde_json::to_vec(&SessionResponse::Egress(reply)).unwrap();
                write_frame(&server, &payload).unwrap();
            }
            seen
        });
        (client, handle)
    }

    fn ok(body: &[u8], more: u64) -> EgressResult {
        EgressResult::Ok {
            status: 200,
            headers: Vec::new(),
            body_b64: encode_secret(body),
            more,
        }
    }

    fn part(body: &[u8], more: u64) -> EgressResult {
        EgressResult::Part {
            body_b64: encode_secret(body),
            more,
        }
    }

    fn post(client: &UnixStream, body: &[u8]) -> Result<EgressResponse, Error> {
        egress_request(client, "s", "POST", "d:443", "/", Vec::new(), body)
    }

    #[test]
    fn a_small_body_goes_whole_with_its_request() {
        let (client, broker) = scripted(vec![ok(b"answer", 0)]);
        let response = post(&client, b"small").unwrap();
        assert_eq!(response.body, b"answer");
        let seen = broker.join().unwrap();
        assert!(matches!(
            seen.as_slice(),
            [EgressOp::Egress { body_b64, staged: 0, .. }] if *body_b64 == encode_secret(b"small")
        ));
    }

    #[test]
    fn egress_request_stages_a_body_too_large_for_one_frame() {
        let body: Vec<u8> = (0..EGRESS_PART_BYTES * 2 + 5)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        let staged = |held: usize| EgressResult::Staged {
            held: u64::try_from(held).unwrap(),
        };
        let (client, broker) = scripted(vec![
            staged(EGRESS_PART_BYTES),
            staged(EGRESS_PART_BYTES * 2),
            staged(body.len()),
            ok(b"answer", 0),
        ]);
        assert_eq!(post(&client, &body).unwrap().body, b"answer");
        let seen = broker.join().unwrap();
        assert!(
            seen.iter()
                .take(3)
                .all(|op| matches!(op, EgressOp::Stage { .. }))
        );
        let parts: Vec<Vec<u8>> = seen
            .iter()
            .filter_map(|op| match op {
                EgressOp::Stage { part_b64 } => decode_secret(part_b64).ok(),
                EgressOp::MintSurrogate { .. } | EgressOp::Egress { .. } | EgressOp::Read => None,
            })
            .collect();
        assert!(parts.iter().all(|part| part.len() <= EGRESS_PART_BYTES));
        assert_eq!(parts.concat(), body);
        let length = u64::try_from(body.len()).unwrap();
        assert!(matches!(
            seen.get(3),
            Some(EgressOp::Egress { body_b64, staged, .. }) if body_b64.is_empty() && *staged == length
        ));
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn egress_request_reads_a_held_answer_in_parts() {
        let (client, broker) = scripted(vec![ok(b"", 8), part(b"abc", 5), part(b"defgh", 0)]);
        assert_eq!(post(&client, b"q").unwrap().body, b"abcdefgh");
        let seen = broker.join().unwrap();
        assert!(matches!(
            seen.as_slice(),
            [EgressOp::Egress { .. }, EgressOp::Read, EgressOp::Read]
        ));
    }

    #[test]
    fn read_held_refuses_parts_that_do_not_add_up() {
        let cases = [
            // Says it took 5 off what it held, sends 3.
            vec![ok(b"", 10), part(b"abc", 5)],
            // No progress: an empty part that leaves as much as before.
            vec![ok(b"", 4), part(b"", 4)],
            // Leaves more than it held.
            vec![ok(b"", 4), part(b"", 9)],
        ];
        for replies in cases {
            let (client, broker) = scripted(replies);
            assert!(
                matches!(post(&client, b"q"), Err(Error::EgressProtocol { .. })),
                "a broker that doesn't add up is a protocol violation"
            );
            drop(client);
            broker.join().unwrap();
        }
        // A part the broker no longer holds, or a refused stage, fails the request.
        let (client, broker) = scripted(vec![ok(b"", 4), EgressResult::EgressError]);
        assert!(matches!(post(&client, b"q"), Err(Error::EgressFailed)));
        broker.join().unwrap();
        let large = vec![0_u8; EGRESS_PART_BYTES * 2];
        let (client, broker) = scripted(vec![EgressResult::EgressError]);
        assert!(matches!(post(&client, &large), Err(Error::EgressFailed)));
        broker.join().unwrap();
        let (client, broker) = scripted(vec![EgressResult::Denied]);
        assert!(matches!(
            post(&client, &large),
            Err(Error::EgressProtocol { .. })
        ));
        broker.join().unwrap();
        let (client, broker) = scripted(vec![ok(b"", 4), EgressResult::Denied]);
        assert!(matches!(
            post(&client, b"q"),
            Err(Error::EgressProtocol { .. })
        ));
        broker.join().unwrap();
    }

    #[test]
    fn read_held_appends_each_part_until_nothing_is_left() {
        let (client, broker) = scripted(vec![part(b"cd", 3), part(b"efg", 0)]);
        let mut body = b"ab".to_vec();
        read_held(&client, &mut body, 5).unwrap();
        assert_eq!(body, b"abcdefg");
        assert_eq!(broker.join().unwrap(), [EgressOp::Read, EgressOp::Read]);
        // Nothing held: nothing read, nothing sent.
        let (client, broker) = scripted(Vec::new());
        let mut body = b"ab".to_vec();
        read_held(&client, &mut body, 0).unwrap();
        assert_eq!(body, b"ab");
        drop(client);
        assert!(broker.join().unwrap().is_empty());
        // A part that doesn't add up stops it.
        let (client, broker) = scripted(vec![part(b"c", 3)]);
        assert!(matches!(
            read_held(&client, &mut Vec::new(), 5),
            Err(Error::EgressProtocol { .. })
        ));
        broker.join().unwrap();
    }

    #[test]
    fn stage_sends_a_body_in_parts_and_stops_at_a_refusal() {
        let body = vec![9_u8; EGRESS_PART_BYTES + 1];
        let (client, broker) = scripted(vec![
            EgressResult::Staged {
                held: u64::try_from(EGRESS_PART_BYTES).unwrap(),
            },
            EgressResult::Staged {
                held: u64::try_from(body.len()).unwrap(),
            },
        ]);
        stage(&client, &body).unwrap();
        let seen = broker.join().unwrap();
        let sizes: Vec<usize> = seen
            .iter()
            .filter_map(|op| match op {
                EgressOp::Stage { part_b64 } => decode_secret(part_b64).ok().map(|part| part.len()),
                EgressOp::MintSurrogate { .. } | EgressOp::Egress { .. } | EgressOp::Read => None,
            })
            .collect();
        assert_eq!(sizes, [EGRESS_PART_BYTES, 1]);
        let (client, broker) = scripted(vec![EgressResult::EgressError]);
        assert!(matches!(stage(&client, &body), Err(Error::EgressFailed)));
        assert_eq!(broker.join().unwrap().len(), 1);
    }

    #[test]
    fn decode_part_decodes_or_names_a_protocol_error() {
        assert_eq!(decode_part(&encode_secret(b"part")).unwrap(), b"part");
        assert_eq!(decode_part("").unwrap(), b"");
        assert!(matches!(
            decode_part("not base64!"),
            Err(Error::EgressProtocol { .. })
        ));
    }

    /// `mint_surrogate` maps a broker `Denied` to `EgressDenied`.
    #[test]
    fn mint_surrogate_denied() {
        let (client, server) = UnixStream::pair().unwrap();
        timeouts(&client, &server);
        let handle = std::thread::spawn(move || {
            let frame = read_frame(&server).unwrap();
            let _request: SessionRequest = serde_json::from_slice(&frame).unwrap();
            let response = SessionResponse::Egress(EgressResult::Denied);
            let payload = serde_json::to_vec(&response).unwrap();
            write_frame(&server, &payload).unwrap();
        });

        let err = mint_surrogate(&client, "example.com:443", 3600).unwrap_err();
        assert!(matches!(err, Error::EgressDenied { .. }));
        handle.join().unwrap();
    }
}
