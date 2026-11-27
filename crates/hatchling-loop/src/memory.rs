//! Memory recall for the agent loop.
//!
//! The loop recalls memory through this trait, not by touching the
//! store directly. The host implements it against the
//! peer-authorized broker session ('s ops); the loop only sees
//! recalled items, never the store.
//!
//! Fail-closed on the recall path: if [`MemoryProvider::recall`]
//! fails, the provider logs the failure to the context log and the
//! loop proceeds without memory. A recall failure never blocks the
//! live user task.

use hatchling_memory::MemoryItem;

/// Recall memory for a loop iteration.
///
/// Implemented by the host against the broker session. The loop calls
/// this once at iteration start, before the first model turn.
pub trait MemoryProvider: Send + Sync {
    /// Recall relevant memory items (session scope + broker-bound
    /// project scope).
    ///
    /// # Errors
    ///
    /// Returns an error when recall fails. The implementation SHOULD
    /// log the failure to the context log before returning; the loop
    /// will proceed without memory.
    fn recall(&self) -> Result<Vec<MemoryItem>, RecallError>;

    /// What a run is given (WO-048): an index of what's recalled and the
    /// items given whole. By default the items [`recall`](Self::recall)
    /// gives, with no index.
    ///
    /// # Errors
    ///
    /// As [`recall`](Self::recall).
    fn recall_budgeted(&self) -> Result<hatchling_memory::Recalled, RecallError> {
        self.recall().map(|items| hatchling_memory::Recalled {
            index: Vec::new(),
            items,
            left_out: 0,
        })
    }
}

/// Why a memory recall failed.
///
/// Kept opaque: the loop does not act on the reason, it only
/// proceeds without memory. The provider logs the details.
#[derive(Debug)]
pub struct RecallError(pub String);

impl std::fmt::Display for RecallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "memory recall failed: {}", self.0)
    }
}

impl std::error::Error for RecallError {}

#[cfg(test)]
mod tests {
    use super::RecallError;

    #[test]
    fn recall_error_names_the_failure() {
        assert_eq!(
            RecallError(String::from("broker unreachable")).to_string(),
            "memory recall failed: broker unreachable"
        );
    }
}
