//! Whether the Windows setup made this install, and how to say things on one.
//!
//! The setup's account has no `sudo`, by design, and a person there opens
//! Hatchling from the Start menu, so what the broker says for a Linux shell
//! would send them nowhere. The marker is the setup's, written as root, and
//! only changes these words.

use std::path::Path;
use std::sync::OnceLock;

/// What the Windows setup leaves: the version it installed.
const MARKER: &str = "/etc/hatchling/windows-setup-version";

/// What's said on a Linux shell, and what's said for it on Windows: the most
/// particular first, as each replaces what the next would match.
const SAID: [(&str, &str); 5] = [
    (
        "`sudo hatchling-broker unlock`",
        "Unlock (type it in the Start menu)",
    ),
    (
        "sudo hatchling-broker unlock",
        "Unlock (type it in the Start menu)",
    ),
    (
        "The app's address, as yourself: hatchling-host url",
        "Open Hatchling from the Start menu.",
    ),
    (
        "As yourself: hatchling-host url",
        "Open Hatchling from the Start menu.",
    ),
    (
        "sudo hatchling-broker ",
        "wsl -d hatchling-wsl -u root -- hatchling-broker ",
    ),
];

static INSTALLED: OnceLock<bool> = OnceLock::new();

/// Whether the Windows setup made this install. Read once: a broker asks
/// before it gives up what it needs to read it.
pub fn installed() -> bool {
    *INSTALLED.get_or_init(|| Path::new(MARKER).exists())
}

/// `text`, as it's said on this install.
#[must_use]
pub fn here(text: &str) -> String {
    said(text, installed())
}

/// `text`, as it's said on Windows when `windows` is.
fn said(text: &str, windows: bool) -> String {
    if !windows {
        return text.to_owned();
    }
    SAID.iter().fold(text.to_owned(), |text, (linux, windows)| {
        text.replace(linux, windows)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_is_left_as_it_is() {
        let text = "Run `sudo hatchling-broker unlock`, as yourself: hatchling-host url";
        assert_eq!(said(text, false), text);
    }

    #[test]
    fn windows_is_told_where_to_click_or_what_to_type() {
        assert_eq!(
            said("the same: sudo hatchling-broker unlock", true),
            "the same: Unlock (type it in the Start menu)"
        );
        assert_eq!(
            said("Run `sudo hatchling-broker unlock`, and touch it", true),
            "Run Unlock (type it in the Start menu), and touch it"
        );
        assert_eq!(
            said(
                "Starting. The app's address, as yourself: hatchling-host url",
                true
            ),
            "Starting. Open Hatchling from the Start menu."
        );
        assert_eq!(
            said(
                "It needs its new address. As yourself: hatchling-host url",
                true
            ),
            "It needs its new address. Open Hatchling from the Start menu."
        );
        assert_eq!(
            said("See: sudo hatchling-broker doctor", true),
            "See: wsl -d hatchling-wsl -u root -- hatchling-broker doctor"
        );
    }

    #[test]
    fn a_linux_install_has_no_marker() {
        assert_eq!(installed(), Path::new(MARKER).exists());
        assert_eq!(here("hello"), "hello");
    }
}
