//! Keeps a credential out of what an upstream sends back.
//!
//! The proxy and the harness relay put the real credential into a
//! request at the boundary. An upstream that echoes it (an error message
//! quoting the request, a debugging endpoint) would hand it to the cell,
//! so every response is scrubbed: each verbatim occurrence of the
//! credential is overwritten with [`MASK`], byte for byte. The length is
//! kept, so `Content-Length` and chunk sizes stay right.
//!
//! Only verbatim occurrences can be found by a byte scan. So responses
//! are requested uncompressed, and a compressed one is refused (see
//! [`is_identity`]): compression would otherwise hide an echo. An
//! upstream that re-encodes what it echoes (base64, hex) is beyond a byte
//! scan; that is a matter for the request policy, not this module.

use zeroize::Zeroizing;

/// The byte a scrubbed credential is overwritten with.
pub const MASK: u8 = b'*';

/// Overwrites every occurrence of `secret` in `bytes` with [`MASK`], and
/// says whether there was one. An empty secret matches nothing.
pub fn scrub(bytes: &mut [u8], secret: &[u8]) -> bool {
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

/// Scrubs a stream that arrives in pieces. A credential split across two
/// pieces is still found: up to `secret.len() - 1` bytes are held back
/// until the next piece shows they don't start the credential.
pub struct Scrubber<'a> {
    secret: &'a [u8],
    held: Zeroizing<Vec<u8>>,
    found: bool,
}

impl<'a> Scrubber<'a> {
    /// A scrubber for `secret`.
    #[must_use]
    pub fn new(secret: &'a [u8]) -> Self {
        Self {
            secret,
            held: Zeroizing::new(Vec::new()),
            found: false,
        }
    }

    /// Takes the next piece. Returns the bytes that are now safe to pass
    /// on, scrubbed.
    pub fn push(&mut self, piece: &[u8]) -> Vec<u8> {
        self.held.extend_from_slice(piece);
        self.found |= scrub(&mut self.held, self.secret);
        let keep = self.secret.len().saturating_sub(1).min(self.held.len());
        let ready = self.held.len().saturating_sub(keep);
        self.held.drain(..ready).collect()
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
}
