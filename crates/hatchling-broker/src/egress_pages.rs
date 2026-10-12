//! Egress bodies larger than a session frame, a part at a time (v13).
//!
//! Every session message fits one frame
//! ([`MAX_FRAME_BYTES`](hatchling_core::MAX_FRAME_BYTES), 64 KiB), so
//! neither side can make the other hold more for one message. An egress
//! body can be larger: a model request carries the whole conversation,
//! and its answer can run long. A body that doesn't fit travels in parts
//! of [`EGRESS_PART_BYTES`]:
//!
//! - **A request:** the agent sends `Stage` ops, each adding a part to
//!   the session's stage, then the `Egress` op, whose `staged` says how
//!   many bytes the body begins with.
//! - **An answer:** when the whole `Ok` reply wouldn't fit a frame, the
//!   broker holds the body and replies with the status, the headers and
//!   `more`, the bytes it holds; the agent reads them with `Read` ops.
//!
//! A session holds at most one staged request, within
//! [`MAX_REQUEST_BYTES`], and one answer, which the proxy has already
//! bounded by [`MAX_RESPONSE_BYTES`](hatchling_egress::MAX_RESPONSE_BYTES):
//! paging is a bound per session, not a way around one.

use hatchling_core::EGRESS_PART_BYTES;
use hatchling_egress::MAX_REQUEST_BYTES;

/// One session's egress bodies in transit: a request being staged, and
/// an answer being read.
#[derive(Debug, Default)]
pub struct Pages {
    staged: Vec<u8>,
    answer: Vec<u8>,
    read: usize,
}

impl Pages {
    /// Adds `part` to the stage: how many bytes it holds now, or `None`,
    /// emptying it, if it would pass [`MAX_REQUEST_BYTES`].
    pub fn stage(&mut self, part: &[u8]) -> Option<u64> {
        if self.staged.len().saturating_add(part.len()) > MAX_REQUEST_BYTES {
            self.staged = Vec::new();
            return None;
        }
        self.staged.extend_from_slice(part);
        u64::try_from(self.staged.len()).ok()
    }

    /// A request's body: the `staged` bytes its `Egress` op says it
    /// begins with, then `rest`. `None` if the stage holds a different
    /// count. The stage is emptied either way.
    pub fn body(&mut self, staged: u64, rest: &[u8]) -> Option<Vec<u8>> {
        let mut body = std::mem::take(&mut self.staged);
        if u64::try_from(body.len()).ok()? != staged {
            return None;
        }
        body.extend_from_slice(rest);
        Some(body)
    }

    /// Holds an answer's body to be read in parts, in place of any held
    /// before: how many bytes it holds.
    pub fn hold(&mut self, body: Vec<u8>) -> u64 {
        self.answer = body;
        self.read = 0;
        u64::try_from(self.answer.len()).unwrap_or(u64::MAX)
    }

    /// The next part of the held answer, of at most
    /// [`EGRESS_PART_BYTES`], and how many bytes are left after it; `None`
    /// if nothing is held. The last part lets the answer go.
    pub fn next_part(&mut self) -> Option<(Vec<u8>, u64)> {
        let left = self
            .answer
            .get(self.read..)
            .filter(|left| !left.is_empty())?;
        let part = left.get(..EGRESS_PART_BYTES.min(left.len()))?.to_vec();
        self.read += part.len();
        let more = self.answer.len() - self.read;
        if more == 0 {
            self.answer = Vec::new();
            self.read = 0;
        }
        Some((part, u64::try_from(more).unwrap_or(u64::MAX)))
    }

    /// Lets a held answer go: a new request's answer replaces it.
    pub fn drop_answer(&mut self) {
        self.answer = Vec::new();
        self.read = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_body_is_staged_within_its_bound_then_sent_once() {
        let mut pages = Pages::default();
        assert_eq!(pages.stage(b"abc"), Some(3));
        assert_eq!(pages.stage(b"de"), Some(5));
        assert_eq!(pages.body(5, b"fg"), Some(b"abcdefg".to_vec()));
        // Sent: the stage is empty again.
        assert_eq!(pages.body(0, b"h"), Some(b"h".to_vec()));
        // A count the stage doesn't hold is refused, and empties it.
        assert_eq!(pages.stage(b"abc"), Some(3));
        assert_eq!(pages.body(2, b""), None);
        assert_eq!(pages.body(0, b""), Some(Vec::new()));
        // Exactly the bound is staged; one byte more empties the stage.
        let bound = vec![7_u8; MAX_REQUEST_BYTES];
        assert_eq!(pages.stage(&bound), u64::try_from(MAX_REQUEST_BYTES).ok());
        assert_eq!(pages.stage(b"x"), None);
        assert_eq!(pages.body(0, b"y"), Some(b"y".to_vec()));
    }

    #[test]
    fn a_held_answer_is_read_in_parts_until_it_ends() {
        let mut pages = Pages::default();
        assert_eq!(pages.next_part(), None);
        let answer: Vec<u8> = (0..EGRESS_PART_BYTES * 2 + 5)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect();
        assert_eq!(
            pages.hold(answer.clone()),
            u64::try_from(answer.len()).unwrap()
        );
        let mut read = Vec::new();
        let mut lefts = Vec::new();
        while let Some((part, left)) = pages.next_part() {
            assert!(part.len() <= EGRESS_PART_BYTES);
            read.extend_from_slice(&part);
            lefts.push(left);
        }
        assert_eq!(read, answer);
        assert_eq!(lefts, [u64::try_from(EGRESS_PART_BYTES + 5).unwrap(), 5, 0]);
        // Read to its end, it's gone.
        assert_eq!(pages.next_part(), None);
        // A new answer replaces one half read; dropping lets it go.
        pages.hold(b"first".to_vec());
        pages.hold(b"second".to_vec());
        assert_eq!(pages.next_part(), Some((b"second".to_vec(), 0)));
        pages.hold(answer);
        assert!(pages.next_part().is_some());
        pages.drop_answer();
        assert_eq!(pages.next_part(), None);
    }
}
