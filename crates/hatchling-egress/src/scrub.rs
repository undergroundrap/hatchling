//! Keeps a credential out of what an upstream sends back.
//!
//! The proxy and the harness relay put the real credential into a
//! request at the boundary. An upstream that echoes it (an error message
//! quoting the request, a debugging endpoint) would hand it to the cell,
//! so every response is scrubbed: each verbatim occurrence of the
//! credential is overwritten with [`MASK`], byte for byte. The length is
//! kept, so `Content-Length` and chunk sizes stay right.
//!
//! A byte scan finds only the forms it looks for. So responses are
//! requested uncompressed, and a compressed one is refused (see
//! [`is_identity`]): compression would otherwise hide an echo.
//!
//! **The escaped forms ([`Forms`], #683).** A credential holding a byte
//! that JSON or URL-encoding may rewrite (anything but letters, digits and
//! `-._~`: a base64 token's `+`, `/` and `=`, a Basic credential's `:`)
//! is also looked for as an upstream may echo it:
//! - JSON-escaped, with `\/` for `/`;
//! - each such byte as `\u00XX`;
//! - percent-encoded.
//!
//! Each hex form is looked for in either case. A credential of letters,
//! digits, `-` and `_`, like today's model keys, has no other form.
//!
//! An upstream that re-encodes what it echoes otherwise (base64, hex) is
//! beyond a byte scan; that is a matter for the request policy, not this
//! module.

use zeroize::Zeroizing;

/// The byte a scrubbed credential is overwritten with.
pub const MASK: u8 = b'*';

/// Whether URL- or JSON-encoding may rewrite `byte`: anything but the
/// bytes URL-encoding leaves as they are.
const fn rewritten(byte: u8) -> bool {
    !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

/// `secret` with each byte `escape` rewrites written as it says, the rest
/// as they are: one of a credential's echoed forms.
fn written(secret: &[u8], escape: impl Fn(u8) -> Option<String>) -> Zeroizing<Vec<u8>> {
    let mut form = Zeroizing::new(Vec::with_capacity(secret.len().saturating_mul(6)));
    for &byte in secret {
        match escape(byte) {
            Some(escaped) => form.extend_from_slice(escaped.as_bytes()),
            None => form.push(byte),
        }
    }
    form
}

/// Every form of a credential an upstream may echo, the verbatim one
/// first: each a copy of the credential, wiped when dropped.
pub struct Forms {
    forms: Vec<Zeroizing<Vec<u8>>>,
}

impl Forms {
    /// The forms of `secret`: itself, and when it holds a byte JSON or
    /// URL-encoding may rewrite, its escaped forms too. None for an empty
    /// secret, which matches nothing.
    #[must_use]
    pub fn of(secret: &[u8]) -> Self {
        let mut forms = Vec::new();
        if secret.is_empty() {
            return Self { forms };
        }
        forms.push(Zeroizing::new(secret.to_vec()));
        if secret.iter().copied().any(rewritten) {
            // JSON's own escapes, `/` as `\/`; a control byte as `\u00XX`.
            forms.push(written(secret, |byte| match byte {
                b'"' => Some(String::from("\\\"")),
                b'\\' => Some(String::from("\\\\")),
                b'/' => Some(String::from("\\/")),
                byte if byte < 0x20 => Some(format!("\\u{byte:04x}")),
                _ => None,
            }));
            for upper in [false, true] {
                let hex = |byte: u8| {
                    if upper {
                        format!("{byte:02X}")
                    } else {
                        format!("{byte:02x}")
                    }
                };
                forms.push(written(secret, |byte| {
                    rewritten(byte).then(|| format!("\\u00{}", hex(byte)))
                }));
                forms.push(written(secret, |byte| {
                    rewritten(byte).then(|| format!("%{}", hex(byte)))
                }));
            }
        }
        let mut seen: Vec<Zeroizing<Vec<u8>>> = Vec::with_capacity(forms.len());
        for form in forms {
            if !seen.iter().any(|kept| **kept == *form) {
                seen.push(form);
            }
        }
        Self { forms: seen }
    }

    /// The longest form's length: what a stream must hold back, less one.
    #[must_use]
    pub fn longest(&self) -> usize {
        self.forms.iter().map(|form| form.len()).max().unwrap_or(0)
    }

    /// Overwrites every occurrence of every form in `bytes` with [`MASK`],
    /// and says whether there was one.
    pub fn scrub(&self, bytes: &mut [u8]) -> bool {
        let mut found = false;
        for form in &self.forms {
            found |= scrub_form(bytes, form);
        }
        found
    }
}

/// Overwrites every occurrence of `secret`, in each form an upstream may
/// echo it ([`Forms`]), in `bytes` with [`MASK`], and says whether there was
/// one. An empty secret matches nothing.
pub fn scrub(bytes: &mut [u8], secret: &[u8]) -> bool {
    Forms::of(secret).scrub(bytes)
}

/// Overwrites every verbatim occurrence of `secret` in `bytes`.
fn scrub_form(bytes: &mut [u8], secret: &[u8]) -> bool {
    if secret.is_empty() {
        return false;
    }
    let mut found = false;
    let mut start = 0;
    while let Some(at) = bytes.get(start..).and_then(|rest| {
        rest.windows(secret.len())
            .position(|window| window == secret)
    }) {
        let from = start.saturating_add(at);
        let to = from.saturating_add(secret.len());
        if let Some(hit) = bytes.get_mut(from..to) {
            hit.fill(MASK);
        }
        found = true;
        start = to;
    }
    found
}

/// [`scrub`] for text, such as a header line. The mask is ASCII, so text
/// stays text.
pub fn scrub_text(text: &mut String, secret: &[u8]) -> bool {
    let mut bytes = std::mem::take(text).into_bytes();
    let found = scrub(&mut bytes, secret);
    *text = String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
    found
}

/// Whether a `Content-Encoding` value leaves the body as it is, so a
/// scrub can see it.
#[must_use]
pub fn is_identity(content_encoding: &str) -> bool {
    content_encoding.trim().eq_ignore_ascii_case("identity")
}

/// Scrubs a stream that arrives in pieces.
///
/// A credential split across two pieces is still found, in any of its
/// [`Forms`]: up to the longest form's length less one is held back until
/// the next piece shows it doesn't start one.
pub struct Scrubber {
    forms: Forms,
    held: Zeroizing<Vec<u8>>,
    found: bool,
}

impl Scrubber {
    /// A scrubber for `secret`.
    #[must_use]
    pub fn new(secret: &[u8]) -> Self {
        Self {
            forms: Forms::of(secret),
            held: Zeroizing::new(Vec::new()),
            found: false,
        }
    }

    /// Takes the next piece. Returns the bytes that are now safe to pass
    /// on, scrubbed.
    ///
    /// Every buffer that holds unscrubbed bytes is made at the size it
    /// needs and never grown, so no copy of an echo is freed unwiped by a
    /// reallocation (#683): each is wiped when it's dropped.
    pub fn push(&mut self, piece: &[u8]) -> Vec<u8> {
        let mut joined = Zeroizing::new(Vec::with_capacity(
            self.held.len().saturating_add(piece.len()),
        ));
        joined.extend_from_slice(&self.held);
        joined.extend_from_slice(piece);
        self.found |= self.forms.scrub(&mut joined);
        let keep = self.forms.longest().saturating_sub(1).min(joined.len());
        let ready = joined.len().saturating_sub(keep);
        let (out, tail) = joined.split_at(ready);
        self.held = Zeroizing::new(tail.to_vec());
        out.to_vec()
    }

    /// Whether any piece so far held the credential.
    #[must_use]
    pub const fn found(&self) -> bool {
        self.found
    }

    /// Ends the stream: the bytes still held back, scrubbed.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        std::mem::take(&mut *self.held)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"sk-real-key-123";

    #[test]
    fn every_occurrence_is_masked_and_the_length_kept() {
        let mut bytes = b"a sk-real-key-123 b sk-real-key-123sk-real-key-123 c".to_vec();
        let length = bytes.len();
        assert!(scrub(&mut bytes, KEY));
        assert_eq!(bytes.len(), length);
        assert_eq!(
            bytes,
            b"a *************** b ****************************** c".to_vec()
        );
    }

    #[test]
    fn nothing_to_scrub_leaves_the_bytes_alone() {
        let mut bytes = b"sk-real-key-12 sk-real-key-124 SK-REAL-KEY-123".to_vec();
        let before = bytes.clone();
        assert!(!scrub(&mut bytes, KEY));
        assert_eq!(bytes, before);
        // An empty secret matches nothing, and a secret longer than the
        // bytes can't be in them.
        assert!(!scrub(&mut bytes, b""));
        assert!(!scrub(&mut b"short".to_vec(), KEY));
        assert_eq!(bytes, before);
    }

    #[test]
    fn text_is_scrubbed_and_stays_text() {
        let mut header = String::from("X-Echo: Bearer sk-real-key-123, ünïcode");
        assert!(scrub_text(&mut header, KEY));
        assert_eq!(header, "X-Echo: Bearer ***************, ünïcode");
        let mut clean = String::from("X-Other: fine");
        assert!(!scrub_text(&mut clean, KEY));
        assert_eq!(clean, "X-Other: fine");
    }

    #[test]
    fn only_identity_counts_as_uncompressed() {
        for identity in ["identity", " Identity ", "IDENTITY"] {
            assert!(is_identity(identity), "{identity}");
        }
        for compressed in ["gzip", "br", "deflate", "zstd", "gzip, identity", ""] {
            assert!(!is_identity(compressed), "{compressed}");
        }
    }

    #[test]
    fn a_credential_split_across_any_two_pieces_is_found() {
        let stream = b"before sk-real-key-123 after";
        for split in 0..=stream.len() {
            let (first, second) = stream.split_at(split);
            let mut scrubber = Scrubber::new(KEY);
            let mut out = scrubber.push(first);
            out.extend(scrubber.push(second));
            assert!(scrubber.found(), "split at {split}");
            out.extend(scrubber.finish());
            assert_eq!(
                out,
                b"before *************** after".to_vec(),
                "split at {split}"
            );
        }
    }

    #[test]
    fn a_stream_byte_by_byte_comes_out_whole_and_scrubbed() {
        let stream = b"sk-real-key-123 then sk-real-key-12 then sk-real-key-123";
        let mut scrubber = Scrubber::new(KEY);
        let mut out = Vec::new();
        for byte in stream {
            out.extend(scrubber.push(std::slice::from_ref(byte)));
            // Never more than the credential's length less one is held.
            assert!(scrubber.held.len() < KEY.len());
        }
        out.extend(scrubber.finish());
        assert_eq!(
            out,
            b"*************** then sk-real-key-12 then ***************".to_vec()
        );
    }

    #[test]
    fn a_clean_stream_passes_unchanged_and_is_not_found() {
        let mut scrubber = Scrubber::new(KEY);
        let mut out = scrubber.push(b"hello ");
        out.extend(scrubber.push(b"world"));
        assert!(!scrubber.found());
        out.extend(scrubber.finish());
        assert_eq!(out, b"hello world".to_vec());
        // With no secret, nothing is held back.
        let mut open = Scrubber::new(b"");
        assert_eq!(open.push(b"abc"), b"abc".to_vec());
        assert!(open.finish().is_empty());
    }

    /// A token holding what JSON and URL-encoding rewrite (#683).
    const TOKEN: &[u8] = b"tok+en/abc==";

    /// Each form an upstream may echo the token in, as it would write it.
    const ECHOES: [&[u8]; 6] = [
        b"tok+en/abc==",
        b"tok+en\\/abc==",
        b"tok\\u002ben\\u002fabc\\u003d\\u003d",
        b"tok%2ben%2fabc%3d%3d",
        b"tok\\u002Ben\\u002Fabc\\u003D\\u003D",
        b"tok%2Ben%2Fabc%3D%3D",
    ];

    #[test]
    fn a_credential_with_bytes_that_get_escaped_has_each_escaped_form() {
        let forms = Forms::of(TOKEN);
        let kept: Vec<&[u8]> = forms.forms.iter().map(|form| form.as_slice()).collect();
        assert_eq!(kept, ECHOES);
        assert_eq!(forms.longest(), ECHOES[2].len());
        // Letters, digits, `-` and `_` (today's model keys): itself alone,
        // scanned as before.
        let plain = Forms::of(KEY);
        assert_eq!(plain.forms.len(), 1);
        assert_eq!(plain.longest(), KEY.len());
        assert_eq!(
            Forms::of(b"a.b~c").forms.len(),
            1,
            "unreserved bytes aren't rewritten"
        );
        assert_eq!(Forms::of(b"").longest(), 0);
        // JSON's own escapes for a quote, a backslash and a control byte.
        let json = Forms::of(b"a\"b\\c\x01d");
        assert!(
            json.forms
                .iter()
                .any(|form| form.as_slice() == b"a\\\"b\\\\c\\u0001d"),
            "JSON-escaped"
        );
        // A space is no control byte: JSON leaves it as it is.
        let spaced = Forms::of(b"a b/c");
        assert_eq!(spaced.forms.get(1).map(|form| form.as_slice()), Some(&b"a b\/c"[..]));
    }

    /// Each escaped form is masked whole, its length kept.
    #[test]
    fn each_escaped_echo_is_masked() {
        for echo in ECHOES {
            let mut bytes = [b"{\"error\":\"", echo, b"\"}"].concat();
            let length = bytes.len();
            assert!(
                scrub(&mut bytes, TOKEN),
                "{}",
                String::from_utf8_lossy(echo)
            );
            assert_eq!(bytes.len(), length);
            let masked = [b"{\"error\":\"".as_slice(), &vec![MASK; echo.len()], b"\"}"].concat();
            assert_eq!(bytes, masked, "{}", String::from_utf8_lossy(echo));
            let mut text = String::from_utf8(echo.to_vec()).unwrap();
            assert!(scrub_text(&mut text, TOKEN));
            assert!(text.bytes().all(|byte| byte == MASK));
        }
    }

    /// And found split across any two pieces of a stream.
    #[test]
    fn each_escaped_echo_split_anywhere_is_found() {
        for echo in ECHOES {
            let stream = [b"before ".as_slice(), echo, b" after"].concat();
            let masked = [b"before ".as_slice(), &vec![MASK; echo.len()], b" after"].concat();
            for split in 0..=stream.len() {
                let (first, second) = stream.split_at(split);
                let mut scrubber = Scrubber::new(TOKEN);
                let mut out = scrubber.push(first);
                out.extend(scrubber.push(second));
                assert!(scrubber.found(), "split at {split}");
                out.extend(scrubber.finish());
                assert_eq!(out, masked, "split at {split}");
            }
        }
    }

    /// What's held between pieces is made at its size and never grown, so
    /// a reallocation never frees an unwiped copy of an echo (#683).
    #[test]
    fn what_is_held_is_never_grown() {
        let mut scrubber = Scrubber::new(TOKEN);
        let stream = [b"a long preamble ".as_slice(), ECHOES[2], b" and more"].concat();
        let mut out = Vec::new();
        for piece in stream.chunks(3) {
            out.extend(scrubber.push(piece));
            assert_eq!(
                scrubber.held.capacity(),
                scrubber.held.len(),
                "held exactly"
            );
            assert!(scrubber.held.len() < ECHOES[2].len());
        }
        out.extend(scrubber.finish());
        assert!(scrubber_found_in(&out));
    }

    /// Whether `out` shows a masked echo where the token's `\u` form was.
    fn scrubber_found_in(out: &[u8]) -> bool {
        out.windows(ECHOES[2].len())
            .any(|window| window.iter().all(|&byte| byte == MASK))
    }
}
