//! The loop's warning when recall fails, in a test binary of its own.
//!
//! A warning's callsite caches whether anything wants it, for every thread.
//! In a binary with other tests, one on another thread can store "nothing
//! does" just after this test's subscriber asked, and the warning is dropped.
//! Alone here, nothing else touches that callsite.

use std::sync::{Arc, Mutex};

use hatchling_loop::{
    FakeModel, Loop, MemoryProvider, ModelAction, PromptKind, RecallError, ToolRegistry,
};
use hatchling_memory::MemoryItem;

#[derive(Clone, Default)]
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

struct Failing;

impl MemoryProvider for Failing {
    fn recall(&self) -> Result<Vec<MemoryItem>, RecallError> {
        Err(RecallError(String::from("broker unreachable")))
    }
}

/// The loop owns a warning on a failed recall, so it shows whatever the
/// provider logged, and the loop still completes.
#[test]
fn recall_failure_warns_and_loop_completes() {
    let capture = Capture::default();
    let buffer = capture.0.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Mutex::new(capture))
        .finish();
    let registry = ToolRegistry::new();
    let agent_loop = Loop::new(&registry, PromptKind::Chat).with_memory_provider(Box::new(Failing));
    let model = FakeModel::new(vec![ModelAction::Text(String::from("done"))]);
    tracing::subscriber::with_default(subscriber, || {
        let result = agent_loop.run(&model, String::from("hello")).unwrap();
        assert_eq!(result.answer, "done");
    });
    let logged = String::from_utf8(buffer.lock().unwrap().clone()).expect("utf8 logs");
    assert!(logged.contains("memory recall failed"), "{logged}");
    assert!(logged.contains("proceeding without memory"), "{logged}");
}
