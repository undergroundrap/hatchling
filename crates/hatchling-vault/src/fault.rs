//! Failures injected into the vault's atomic write, for tests only (WO-025).
//!
//! A store writes a temp file, syncs it, renames it over the entry, and
//! syncs the directory. Whatever stores through the vault has to hold up
//! when any one of those fails: before the rename, nothing changed; after
//! it, the new state is visible but may not be durable. With the
//! `fault-injection` feature, a test chooses the step at which the next
//! store on its thread fails ([`fail_next`]), or a later one does
//! ([`fail_nth`]). Without it, every step goes ahead, and no build that
//! ships carries the switch.

/// A step of the vault's atomic write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Writing the temp file.
    TempWrite,
    /// Syncing the temp file.
    TempSync,
    /// Renaming it over the entry.
    Rename,
    /// Syncing the directory, after the rename: the new state is visible,
    /// but may not be durable.
    DirSync,
}

#[cfg(feature = "fault-injection")]
std::thread_local! {
    /// The step to fail at, and how many times it goes ahead first.
    static NEXT: std::cell::Cell<Option<(Step, usize)>> = const { std::cell::Cell::new(None) };
}

/// Makes the next store on this thread fail at `step`, once.
#[cfg(feature = "fault-injection")]
pub fn fail_next(step: Step) {
    fail_nth(step, 0);
}

/// Makes the store on this thread that reaches `step` for the `skipped`th
/// time after this, counted from 0, fail there, once: those before it go
/// ahead.
#[cfg(feature = "fault-injection")]
pub fn fail_nth(step: Step, skipped: usize) {
    NEXT.with(|next| next.set(Some((step, skipped))));
}

/// Fails here when a test asked for a failure at `step`; goes ahead
/// otherwise.
///
/// # Errors
///
/// Returns [`crate::Error::Io`] at the step a test chose.
#[cfg(feature = "fault-injection")]
pub(crate) fn at(step: Step) -> Result<(), crate::Error> {
    match NEXT.with(std::cell::Cell::get) {
        Some((at, 0)) if at == step => {
            NEXT.with(|next| next.set(None));
            Err(crate::Error::Io {
                source: std::io::Error::other(format!("a failure injected at {step:?}")),
            })
        }
        Some((at, left)) if at == step => {
            NEXT.with(|next| next.set(Some((at, left - 1))));
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Every step goes ahead: no failure can be injected in this build.
#[cfg(not(feature = "fault-injection"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same signature as the injecting build's, so the write path reads alike in both"
)]
pub(crate) const fn at(_step: Step) -> Result<(), crate::Error> {
    Ok(())
}

#[cfg(all(test, feature = "fault-injection"))]
mod tests {
    use super::*;

    /// A failure is injected once, at its step alone.
    #[test]
    fn a_failure_fires_once_at_its_step() {
        fail_next(Step::Rename);
        assert!(at(Step::TempWrite).is_ok());
        assert!(at(Step::Rename).is_err());
        assert!(at(Step::Rename).is_ok(), "once");
    }

    /// A later failure lets the step go ahead as many times as it was
    /// told, then fires once, and no other step counts towards it.
    #[test]
    fn a_later_failure_fires_after_its_step_goes_ahead() {
        fail_nth(Step::DirSync, 2);
        assert!(at(Step::TempWrite).is_ok());
        assert!(at(Step::DirSync).is_ok());
        assert!(at(Step::Rename).is_ok());
        assert!(at(Step::DirSync).is_ok());
        assert!(at(Step::DirSync).is_err());
        assert!(at(Step::DirSync).is_ok(), "once");
    }
}
