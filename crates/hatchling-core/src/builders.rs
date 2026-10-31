//! The builder the operator names for each build kind (WO-032).
//!
//! The operator sets it (`HATCHLING_ARTIFACT_BUILDERS`), and the broker
//! tells a chat run the same list (`HATCHLING_RUN_BUILDERS`), so the
//! agent can propose a build without naming a skill it can't list. Both
//! are `kind=name@version` pairs, comma-separated. The broker is the
//! authority: it also checks each kind is registered and each builder a
//! skill id, and a chat run's proposal must name the builder listed here
//! for its kind.

use std::collections::BTreeMap;

/// The most kinds a list may name.
pub const MAX_BUILDERS: usize = 16;

/// The most bytes a kind or a builder may be.
pub const MAX_NAME_BYTES: usize = 64;

/// Parses a builder list: each kind once, each a lowercase token, and
/// each builder `name@version`.
///
/// # Errors
///
/// Why it isn't one, naming the pair.
pub fn parse(raw: &str) -> Result<BTreeMap<String, String>, String> {
    let mut builders = BTreeMap::new();
    for pair in raw
        .split(',')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
    {
        let Some((kind, builder)) = pair.split_once('=') else {
            return Err(format!("{pair:?} is not kind=name@version"));
        };
        let (kind, builder) = (kind.trim(), builder.trim());
        if !is_kind(kind) {
            return Err(format!(
                "{kind:?} is not a build kind: lowercase letters, digits and '-'"
            ));
        }
        if !is_builder(builder) {
            return Err(format!("{builder:?} is not a builder's name@version"));
        }
        if builders
            .insert(kind.to_owned(), builder.to_owned())
            .is_some()
        {
            return Err(format!("{kind:?} is named twice"));
        }
        if builders.len() > MAX_BUILDERS {
            return Err(format!("more than {MAX_BUILDERS} kinds"));
        }
    }
    Ok(builders)
}

/// Writes `builders` as [`parse`] reads it.
#[must_use]
pub fn format(builders: &BTreeMap<String, String>) -> String {
    builders
        .iter()
        .map(|(kind, builder)| format!("{kind}={builder}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether `kind` is a build kind's token.
fn is_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= MAX_NAME_BYTES
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `builder` is shaped as a skill id, `name@version`.
fn is_builder(builder: &str) -> bool {
    let part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
    };
    builder.len() <= MAX_NAME_BYTES
        && builder
            .split_once('@')
            .is_some_and(|(name, version)| part(name) && part(version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_reads_back_as_it_was_written() {
        let builders = parse(" page=page-builder@1.0.0 , document=doc@2.1.0,").unwrap();
        assert_eq!(
            builders.into_iter().collect::<Vec<_>>(),
            [
                (String::from("document"), String::from("doc@2.1.0")),
                (String::from("page"), String::from("page-builder@1.0.0")),
            ]
        );
        let written = format(&parse("page=p@1,document=d@1").unwrap());
        assert_eq!(written, "document=d@1,page=p@1");
        assert_eq!(parse(&written).unwrap().len(), 2);
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn a_malformed_list_is_refused_naming_its_pair() {
        for (raw, says) in [
            ("page", "not kind=name@version"),
            ("Page=p@1", "not a build kind"),
            ("=p@1", "not a build kind"),
            ("page=p", "not a builder"),
            ("page=p@", "not a builder"),
            ("page=@1", "not a builder"),
            ("page=p@1@2", "not a builder"),
            ("page=p q@1", "not a builder"),
            ("page=p@1,page=q@1", "named twice"),
        ] {
            let error = parse(raw).unwrap_err();
            assert!(error.contains(says), "{raw}: {error}");
        }
        let long = format!("page={}@1", "p".repeat(MAX_NAME_BYTES));
        assert!(parse(&long).unwrap_err().contains("not a builder"));
        let many = (0..=MAX_BUILDERS)
            .map(|n| format!("k{n}=b@1"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse(&many).unwrap_err().contains("more than"));
        let most = (0..MAX_BUILDERS)
            .map(|n| format!("k{n}=b@1"))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(parse(&most).unwrap().len(), MAX_BUILDERS);
    }

    #[test]
    fn a_kind_is_a_lowercase_token_and_a_builder_a_name_at_a_version() {
        for kind in ["page", "a-1", &"k".repeat(MAX_NAME_BYTES)] {
            assert!(is_kind(kind), "{kind}");
        }
        for kind in ["", "Page", "a_b", "a b", &"k".repeat(MAX_NAME_BYTES + 1)] {
            assert!(!is_kind(kind), "{kind}");
        }
        for builder in ["p@1", "page-builder@1.0.0", "a_b.c@v2-rc.1"] {
            assert!(is_builder(builder), "{builder}");
        }
        for builder in ["p", "@1", "p@", "p@1@2", "p q@1", "p@1/2"] {
            assert!(!is_builder(builder), "{builder}");
        }
    }
}
