//! The DevTools protocol over Chromium's pipe (`--remote-debugging-pipe`):
//! JSON messages, each ended by a NUL byte, written to the browser's fd 3
//! and read from its fd 4.
//!
//! Everything read is untrusted: the browser runs page code. So one
//! message is bounded ([`MAX_MESSAGE_BYTES`]), the events kept while a
//! call waits are bounded ([`MAX_EVENTS`]), and every read and write has a
//! deadline. A browser that stops answering costs a timeout, never a hang.

use std::collections::VecDeque;
use std::os::fd::{AsFd, OwnedFd};
use std::time::Instant;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use serde_json::{Value, json};

/// The largest message read from the browser, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// The most events kept unread; past it, the oldest are dropped.
pub const MAX_EVENTS: usize = 1024;

/// How much is read from the pipe at a time.
const CHUNK: usize = 64 * 1024;

/// Why a call didn't return a result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CdpError {
    /// Nothing came in time.
    #[error("the browser didn't answer in time")]
    Timeout,
    /// The pipe closed or broke: the browser is gone.
    #[error("the browser's pipe closed")]
    Gone,
    /// A message was larger than [`MAX_MESSAGE_BYTES`], or wasn't JSON.
    /// The pipe can't be trusted to be in step after either.
    #[error("the browser sent a message that can't be read")]
    Unreadable,
    /// The browser answered the call with an error.
    #[error("the browser refused {method}: {message}")]
    Refused {
        /// The method called.
        method: String,
        /// The browser's message. Untrusted text.
        message: String,
    },
}

/// A connection to a browser over its DevTools pipe.
#[derive(Debug)]
pub struct Cdp {
    to: OwnedFd,
    from: OwnedFd,
    read: Vec<u8>,
    /// How much of `read` is known to hold no NUL. Each byte is looked at
    /// once, however small the pieces a message arrives in.
    scanned: usize,
    next_id: u64,
    events: VecDeque<Value>,
    broken: bool,
}

impl Cdp {
    /// A connection that writes to `to` (the browser's fd 3) and reads
    /// from `from` (its fd 4). Both are made non-blocking, so a wait is
    /// always one of this type's own, bounded.
    ///
    /// # Errors
    ///
    /// The `fcntl` failure, if either can't be made non-blocking.
    pub fn new(to: OwnedFd, from: OwnedFd) -> Result<Self, Errno> {
        for fd in [&to, &from] {
            let flags = rustix::fs::fcntl_getfl(fd)?;
            rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
        }
        Ok(Self {
            to,
            from,
            read: Vec::new(),
            scanned: 0,
            next_id: 0,
            events: VecDeque::new(),
            broken: false,
        })
    }

    /// Calls `method` with `params`, on the page session `session` or on
    /// the browser itself, and returns its result. Events that arrive
    /// meanwhile are kept for [`Cdp::event`].
    ///
    /// # Errors
    ///
    /// [`CdpError::Timeout`] past `deadline`; [`CdpError::Refused`] when
    /// the browser answers with an error; [`CdpError::Gone`] or
    /// [`CdpError::Unreadable`] when the pipe is no longer usable, which
    /// every later call then reports too.
    pub fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, CdpError> {
        self.next_id += 1;
        let id = self.next_id;
        let mut message = json!({"id": id, "method": method});
        if let Some(object) = message.as_object_mut() {
            object.insert(String::from("params"), params);
            if let Some(session) = session {
                object.insert(String::from("sessionId"), Value::from(session));
            }
        }
        let mut bytes = serde_json::to_vec(&message).map_err(|_| CdpError::Unreadable)?;
        bytes.push(0);
        self.write_all(&bytes, deadline)?;
        loop {
            let message = self.next_message(deadline)?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                return message.get("error").map_or_else(
                    || Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                    |error| {
                        Err(CdpError::Refused {
                            method: method.to_owned(),
                            message: error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("no message")
                                .to_owned(),
                        })
                    },
                );
            }
            if message.get("method").is_some() {
                self.keep(message);
            }
        }
    }

    /// The first event for which `wanted` holds, kept or read, by
    /// `deadline`. Kept events it passes over stay kept; events read
    /// while waiting are kept too.
    ///
    /// # Errors
    ///
    /// As [`Cdp::call`], but never [`CdpError::Refused`].
    pub fn event(
        &mut self,
        deadline: Instant,
        wanted: impl Fn(&Value) -> bool,
    ) -> Result<Value, CdpError> {
        if let Some(at) = self.events.iter().position(&wanted) {
            if let Some(event) = self.events.remove(at) {
                return Ok(event);
            }
        }
        loop {
            let message = self.next_message(deadline)?;
            if message.get("method").is_none() {
                continue;
            }
            if wanted(&message) {
                return Ok(message);
            }
            self.keep(message);
        }
    }

    /// Forgets every kept event.
    pub fn forget_events(&mut self) {
        self.events.clear();
    }

    /// Keeps an event, dropping the oldest past [`MAX_EVENTS`].
    fn keep(&mut self, event: Value) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// The next whole message, by `deadline`.
    fn next_message(&mut self, deadline: Instant) -> Result<Value, CdpError> {
        if self.broken {
            return Err(CdpError::Gone);
        }
        loop {
            let unseen = self.read.get(self.scanned..).unwrap_or_default();
            if let Some(at) = unseen.iter().position(|&byte| byte == 0) {
                let end = self.scanned + at;
                let rest = self.read.split_off(end + 1);
                let message = std::mem::replace(&mut self.read, rest);
                self.scanned = 0;
                return serde_json::from_slice(message.get(..end).unwrap_or_default())
                    .map_err(|_| self.unreadable());
            }
            self.scanned = self.read.len();
            if self.read.len() > MAX_MESSAGE_BYTES {
                return Err(self.unreadable());
            }
            self.wait(PollFlags::IN, deadline)?;
            let mut chunk = vec![0u8; CHUNK];
            match rustix::io::read(&self.from, &mut chunk) {
                Ok(0) => {
                    self.broken = true;
                    return Err(CdpError::Gone);
                }
                Ok(n) => self
                    .read
                    .extend_from_slice(chunk.get(..n).unwrap_or_default()),
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(_) => {
                    self.broken = true;
                    return Err(CdpError::Gone);
                }
            }
        }
    }

    /// Writes all of `bytes` by `deadline`.
    fn write_all(&mut self, mut bytes: &[u8], deadline: Instant) -> Result<(), CdpError> {
        if self.broken {
            return Err(CdpError::Gone);
        }
        while !bytes.is_empty() {
            self.wait(PollFlags::OUT, deadline)?;
            match rustix::io::write(&self.to, bytes) {
                Ok(n) => bytes = bytes.get(n..).unwrap_or_default(),
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(_) => {
                    self.broken = true;
                    return Err(CdpError::Gone);
                }
            }
        }
        Ok(())
    }

    /// Waits until the pipe this direction uses is ready, by `deadline`.
    fn wait(&mut self, direction: PollFlags, deadline: Instant) -> Result<(), CdpError> {
        let fd = if direction == PollFlags::IN {
            self.from.as_fd()
        } else {
            self.to.as_fd()
        };
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(CdpError::Timeout);
            }
            // At least 1 ms, so a deadline a few microseconds off still
            // polls once rather than spinning.
            let millis = i32::try_from(left.as_millis().max(1)).unwrap_or(i32::MAX);
            let mut fds = [PollFd::new(&fd, direction)];
            match poll(&mut fds, millis) {
                Ok(0) | Err(Errno::INTR) => {}
                Ok(_) => {
                    if fds.iter().any(|fd| fd.revents().intersects(direction)) {
                        return Ok(());
                    }
                    // Hung up or in error, with nothing to read.
                    self.broken = true;
                    return Err(CdpError::Gone);
                }
                Err(_) => {
                    self.broken = true;
                    return Err(CdpError::Gone);
                }
            }
        }
    }

    /// Marks the pipe unusable: past a message that can't be read, the
    /// stream can't be trusted to be in step.
    const fn unreadable(&mut self) -> CdpError {
        self.broken = true;
        CdpError::Unreadable
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::time::Duration;

    use super::*;

    /// A connection, and the browser's ends: what it reads, what it writes.
    fn connected() -> (Cdp, std::fs::File, std::fs::File) {
        let (browser_reads, to) = rustix::pipe::pipe().unwrap();
        let (from, browser_writes) = rustix::pipe::pipe().unwrap();
        (
            Cdp::new(to, from).unwrap(),
            std::fs::File::from(browser_reads),
            std::fs::File::from(browser_writes),
        )
    }

    fn method(event: &Value) -> &str {
        event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    /// The next NUL-ended message the browser was sent.
    fn sent(reads: &mut std::fs::File) -> Value {
        let mut bytes = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            reads.read_exact(&mut byte).unwrap();
            let [next] = byte;
            if next == 0 {
                return serde_json::from_slice(&bytes).unwrap();
            }
            bytes.push(next);
        }
    }

    fn send(writes: &mut std::fs::File, message: &Value) {
        let mut bytes = serde_json::to_vec(message).unwrap();
        bytes.push(0);
        writes.write_all(&bytes).unwrap();
    }

    #[test]
    fn a_call_is_answered_by_its_own_id_and_events_meanwhile_are_kept() {
        let (mut cdp, mut reads, mut writes) = connected();
        // Answers queued before the call: a stale answer, an event, a
        // second event, then the answer.
        send(&mut writes, &json!({"id": 99, "result": {"stale": true}}));
        send(
            &mut writes,
            &json!({"method": "Page.loadEventFired", "params": {}}),
        );
        send(
            &mut writes,
            &json!({"method": "Page.frameNavigated", "params": {"n": 1}}),
        );
        send(&mut writes, &json!({"id": 1, "result": {"frameId": "F"}}));
        let result = cdp
            .call(Some("S"), "Page.navigate", json!({"url": "u"}), soon())
            .unwrap();
        assert_eq!(result, json!({"frameId": "F"}));
        assert_eq!(
            sent(&mut reads),
            json!({"id": 1, "method": "Page.navigate", "params": {"url": "u"}, "sessionId": "S"})
        );
        // The second event is taken first, the other stays kept.
        let navigated = cdp
            .event(soon(), |e| method(e) == "Page.frameNavigated")
            .unwrap();
        assert_eq!(navigated.pointer("/params/n"), Some(&json!(1)));
        let loaded = cdp
            .event(soon(), |e| method(e) == "Page.loadEventFired")
            .unwrap();
        assert_eq!(method(&loaded), "Page.loadEventFired");
        // A browser-level call carries no session.
        send(&mut writes, &json!({"id": 2, "result": {}}));
        cdp.call(None, "Browser.getVersion", json!({}), soon())
            .unwrap();
        assert!(sent(&mut reads).get("sessionId").is_none());
    }

    #[test]
    fn messages_are_read_whole_and_an_unreadable_one_breaks_the_pipe() {
        let (mut cdp, _reads, mut writes) = connected();
        // Two messages in one write, read one at a time.
        writes.write_all(b"{\"a\":1}\0{\"b\":2}\0").unwrap();
        assert_eq!(cdp.next_message(soon()), Ok(json!({"a": 1})));
        assert_eq!(cdp.next_message(soon()), Ok(json!({"b": 2})));
        assert_eq!(cdp.unreadable(), CdpError::Unreadable);
        assert_eq!(cdp.next_message(soon()), Err(CdpError::Gone), "for good");
    }

    #[test]
    fn a_refusal_names_its_method_and_message() {
        let (mut cdp, _reads, mut writes) = connected();
        send(
            &mut writes,
            &json!({"id": 1, "error": {"code": -32000, "message": "no such frame"}}),
        );
        assert_eq!(
            cdp.call(None, "Page.navigate", json!({}), soon()),
            Err(CdpError::Refused {
                method: String::from("Page.navigate"),
                message: String::from("no such frame"),
            })
        );
    }

    #[test]
    fn a_silent_browser_times_out_and_a_closed_one_is_gone_for_good() {
        let (mut cdp, _reads, writes) = connected();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(200);
        assert_eq!(
            cdp.call(None, "Browser.getVersion", json!({}), deadline),
            Err(CdpError::Timeout)
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_secs(3), "bounded");
        assert_eq!(
            cdp.event(Instant::now(), |_| true),
            Err(CdpError::Timeout),
            "a deadline already past"
        );
        drop(writes);
        assert_eq!(
            cdp.call(None, "Browser.getVersion", json!({}), soon()),
            Err(CdpError::Gone)
        );
        assert_eq!(
            cdp.call(None, "Browser.getVersion", json!({}), soon()),
            Err(CdpError::Gone),
            "and stays gone"
        );
    }

    #[test]
    fn each_byte_read_is_looked_at_once_however_the_message_arrives() {
        let (mut cdp, _reads, mut writes) = connected();
        writes.write_all(br#"{"method":"#).unwrap();
        let briefly = Instant::now() + Duration::from_millis(100);
        assert_eq!(cdp.event(briefly, |_| true), Err(CdpError::Timeout));
        assert_eq!(cdp.scanned, 10, "the part read holds no end");
        writes.write_all(br#""A"}"#).unwrap();
        writes.write_all(b"\0").unwrap();
        writes.write_all(br#"{"method":"B"}"#).unwrap();
        writes.write_all(b"\0").unwrap();
        let first = cdp.event(soon(), |_| true).unwrap();
        assert_eq!(method(&first), "A");
        assert_eq!(cdp.scanned, 0, "what follows is unseen");
        let second = cdp.event(soon(), |_| true).unwrap();
        assert_eq!(method(&second), "B");
    }

    #[test]
    fn an_oversized_or_broken_message_ends_the_connection() {
        let (mut cdp, _reads, mut writes) = connected();
        let big = std::thread::spawn(move || {
            // No NUL: one message past the bound. The write fails once
            // the reader stops reading.
            let chunk = vec![b'x'; CHUNK];
            for _ in 0..(MAX_MESSAGE_BYTES / CHUNK + 2) {
                if writes.write_all(&chunk).is_err() {
                    break;
                }
            }
            writes
        });
        assert_eq!(cdp.event(soon(), |_| true), Err(CdpError::Unreadable));
        assert_eq!(cdp.event(soon(), |_| true), Err(CdpError::Gone));
        drop(cdp);
        drop(big.join().unwrap());
        let (mut cdp, _reads, mut writes) = connected();
        writes.write_all(b"{not json\0").unwrap();
        assert_eq!(cdp.event(soon(), |_| true), Err(CdpError::Unreadable));
    }

    #[test]
    fn kept_events_are_bounded_oldest_first() {
        let (mut cdp, _reads, mut writes) = connected();
        let sender = std::thread::spawn(move || {
            for n in 0..(MAX_EVENTS + 10) {
                send(&mut writes, &json!({"method": "E", "params": {"n": n}}));
            }
            send(&mut writes, &json!({"method": "Last", "params": {}}));
            writes
        });
        cdp.event(soon(), |e| method(e) == "Last").unwrap();
        let _writes = sender.join().unwrap();
        assert_eq!(cdp.events.len(), MAX_EVENTS);
        assert_eq!(
            cdp.events.front().unwrap().pointer("/params/n"),
            Some(&json!(10))
        );
        cdp.forget_events();
        assert!(cdp.events.is_empty());
    }
}
