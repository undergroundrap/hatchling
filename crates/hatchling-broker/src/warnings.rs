//! The warnings the broker puts on a request (WO-036): a request that
//! carries one asks the owner every time, whatever they granted, and only
//! once.
//!
//! - **Checked before any grant.** A site held for the task, a 24-hour or
//!   always grant, and a browser's visit all cover nothing a warning is on:
//!   the owner is asked, and the card says the warning and offers *Allow
//!   once* alone.
//! - **The broker's list, and nothing else's.** [`WARNINGS`] is a constant of
//!   the broker's build. A warning is found from the request the broker
//!   resolved alone, never from a session, its words, a grant or the
//!   operator's config, so nothing an agent sends adds one or takes one away.
//! - **Which requests carry one** is decided with each kind that can. Today a
//!   site's: an address over [`LONG_ADDRESS`] characters, the easiest way to
//!   carry out what the agent read. The commands a tool asks to run get their
//!   list with the first tool that asks to run one.

use hatchling_egress::web::WebUrl;

use crate::site_grants::Kind;

/// The longest address a standing grant covers, in characters: a longer
/// one asks every time (the owner's choice, 2026-11-20).
pub const LONG_ADDRESS: usize = 1024;

/// A warning: what kind of request it's on, what the owner is told, and
/// which requests carry it.
#[derive(Debug)]
pub struct Warning {
    /// The kind of request it's on.
    pub kind: Kind,
    /// What the owner is told, on the card.
    pub says: &'static str,
    /// Whether a request of that kind carries it.
    applies: fn(&WebUrl) -> bool,
}

/// Every warning the broker puts on a request.
pub static WARNINGS: [Warning; 1] = [Warning {
    kind: Kind::WebVisit,
    says: "Its address is over 1,024 characters: a long address can carry out what the agent has read. A standing grant doesn't cover it, so you're asked each time.",
    applies: long_address,
}];

/// Whether `url` is longer than [`LONG_ADDRESS`].
fn long_address(url: &WebUrl) -> bool {
    url.to_string().chars().count() > LONG_ADDRESS
}

/// The warning on reading or visiting `url`, if it carries one: found from
/// the address alone.
#[must_use]
pub fn site_warning(url: &WebUrl) -> Option<&'static str> {
    WARNINGS
        .iter()
        .filter(|warning| warning.kind == Kind::WebVisit)
        .find(|warning| (warning.applies)(url))
        .map(|warning| warning.says)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(length: usize) -> WebUrl {
        let base = "https://a.example/";
        let url = format!("{base}{}", "x".repeat(length - base.len()));
        assert_eq!(url.len(), length);
        WebUrl::parse(&url).unwrap()
    }

    /// The list is the broker's, as built: one warning, on a site's long
    /// address. No session, grant or config is an input to it, so nothing an
    /// agent sends adds to it or takes from it.
    #[test]
    fn the_warnings_are_the_brokers_alone() {
        assert_eq!(WARNINGS.len(), 1);
        let [long] = &WARNINGS;
        assert_eq!(long.kind, Kind::WebVisit);
        assert!(
            long.says
                .starts_with("Its address is over 1,024 characters")
        );
        assert_eq!(LONG_ADDRESS, 1024);
    }

    /// An address over 1,024 characters carries the warning; one of 1,024
    /// doesn't.
    #[test]
    fn a_long_address_carries_the_warning() {
        assert_eq!(site_warning(&address(LONG_ADDRESS)), None);
        assert_eq!(
            site_warning(&address(LONG_ADDRESS + 1)),
            WARNINGS.first().map(|warning| warning.says)
        );
        assert_eq!(
            site_warning(&WebUrl::parse("https://a.example/").unwrap()),
            None
        );
    }
}
