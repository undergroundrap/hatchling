//! What the broker logs while a test runs `f` on this thread, as the
//! broker's own subscriber writes it.
#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-visible on purpose: test modules across the crate share the capture"
)]

use std::sync::{Arc, Mutex};

/// A `tracing` writer into a shared buffer.
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every line logged on this thread while `f` runs, at every level.
pub(crate) fn captured(f: impl FnOnce()) -> String {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(Mutex::new(Capture(Arc::clone(&buffer))))
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        // `tracing` caches, per callsite, whether any subscriber wants
        // it; one cached while no subscriber did (by a test on another
        // thread) would drop this test's lines. Rebuilt with ours in place.
        tracing::callsite::rebuild_interest_cache();
        f();
    });
    let logged = buffer.lock().unwrap().clone();
    String::from_utf8(logged).unwrap()
}

/// Asserts that `logged` holds `marker`, the text a client chose, but
/// never at the start of a line: a newline it sent was escaped, so it
/// forged no line of its own.
pub(crate) fn assert_no_forged_line(logged: &str, marker: &str) {
    assert!(
        logged.contains(marker),
        "the client's text was logged: {logged}"
    );
    assert!(
        !logged.lines().any(|line| line.starts_with(marker)),
        "a client forged a log line: {logged}"
    );
}
