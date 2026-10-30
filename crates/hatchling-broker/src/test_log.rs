//! What the broker logs while a test runs `f` on this thread, as the
//! broker's own subscriber writes it.
#![allow(
    clippy::redundant_pub_crate,
    reason = "crate-visible on purpose: test modules across the crate share the capture"
)]

use std::cell::RefCell;
use std::sync::{Arc, Mutex, Once};

thread_local! {
    /// Where this thread's lines go while it captures them: nowhere else.
    static CAPTURING: RefCell<Option<Arc<Mutex<Vec<u8>>>>> = const { RefCell::new(None) };
}

/// A `tracing` writer into the capture of the thread that logs, if it
/// captures.
struct Capture;

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        CAPTURING.with(|capturing| {
            if let Some(buffer) = capturing.borrow().as_ref() {
                buffer.lock().unwrap().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The tests' global subscriber, set once for the whole process: it wants
/// every line, at every level, and writes only a capturing thread's.
///
/// `tracing` caches, per callsite, whether any subscriber wants it. A
/// subscriber of this thread's own, set while it captures, raced the
/// threads of other tests: one that first reached a callsite while none
/// wanted it had it cached as unwanted, and this test's line was dropped.
/// A global subscriber that wants everything is never raced. One the
/// broker's own start set first (a test runs it) wants what it logs.
static SUBSCRIBER: Once = Once::new();

/// A subscriber that wants every line, and writes a capturing thread's.
fn everything() -> impl tracing::Subscriber + Send + Sync {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(|| Capture)
        .finish()
}

/// Every line logged on this thread while `f` runs, at every level.
pub(crate) fn captured(f: impl FnOnce()) -> String {
    SUBSCRIBER.call_once(|| {
        let _ = tracing::subscriber::set_global_default(everything());
    });
    let buffer = Arc::new(Mutex::new(Vec::new()));
    CAPTURING.with(|capturing| *capturing.borrow_mut() = Some(Arc::clone(&buffer)));
    // This thread's own too, whichever global was set first.
    tracing::subscriber::with_default(everything(), || {
        tracing::callsite::rebuild_interest_cache();
        f();
    });
    CAPTURING.with(|capturing| *capturing.borrow_mut() = None);
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
