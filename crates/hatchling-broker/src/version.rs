//! What `--version` says: the release, the commit, and how this build was
//! made, as the build script recorded them (see `build.rs`).

use crate::build_kind::Kind;

/// This build's kind, as the build script recorded it.
fn kind() -> Kind {
    Kind::from_name(env!("HATCHLING_BUILD_KIND"))
}

/// The commit the build script recorded, which it leaves empty when it
/// recorded none.
fn recorded(commit: &str) -> Option<&str> {
    Some(commit).filter(|commit| !commit.is_empty())
}

/// What `--version` prints for `program`: its name and release, then the
/// commit and how it was built, each on its own line.
#[must_use]
pub fn describe(program: &str) -> String {
    said(
        program,
        env!("CARGO_PKG_VERSION"),
        kind(),
        recorded(env!("HATCHLING_BUILD_COMMIT")),
    )
}

/// [`describe`]'s words for a build of `kind`.
fn said(program: &str, version: &str, kind: Kind, commit: Option<&str>) -> String {
    let how = match kind {
        Kind::Release => "built as a release is built, by scripts/release-artifacts.sh",
        Kind::Source => "built from source, at that commit",
        Kind::Modified => "built from source, at that commit with changes not committed",
        Kind::Package => "built from its published package, of that commit",
        Kind::PackageModified => {
            "built from its published package, of that commit with changes not committed"
        }
        Kind::Debug => "a debug build: its commit isn't recorded",
        Kind::Unrecorded => "built from source: its commit isn't recorded",
    };
    commit.map_or_else(
        || format!("{program} {version}\n{how}\n"),
        |commit| format!("{program} {version}\ncommit {commit}\n{how}\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_kind::{Checkout, KINDS, PREFIX, Seen, decide, is_commit, package_commit};

    const HEAD: &str = "5a9d270e0123456789abcdef0123456789abcdef";

    const fn seen<'a>(
        release_build: bool,
        package: Option<&'a str>,
        checkout: Option<Checkout<'a>>,
    ) -> Seen<'a> {
        Seen {
            release_profile: true,
            release_build,
            package,
            checkout,
        }
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "each test gives a checkout where there might be none"
    )]
    const fn checkout<'a>(prefix: &'a str, head: &'a str, modified: bool) -> Option<Checkout<'a>> {
        Some(Checkout {
            prefix,
            head,
            modified,
        })
    }

    #[test]
    fn a_build_says_its_release_its_commit_and_how_it_was_made() {
        assert_eq!(
            said("hatchling-broker", "0.8.0", Kind::Release, Some(HEAD)),
            format!(
                "hatchling-broker 0.8.0\ncommit {HEAD}\nbuilt as a release is built, by scripts/release-artifacts.sh\n"
            )
        );
        assert_eq!(
            said("hatchling", "0.8.0", Kind::Debug, None),
            "hatchling 0.8.0\na debug build: its commit isn't recorded\n"
        );
        // Each kind says something of its own; a change not committed is said.
        let mut words: Vec<String> = KINDS
            .iter()
            .map(|&kind| said("p", "1", kind, None))
            .collect();
        words.sort();
        words.dedup();
        assert_eq!(words.len(), KINDS.len());
        for kind in [Kind::Modified, Kind::PackageModified] {
            assert!(
                said("p", "1", kind, None).contains("changes not committed"),
                "{kind:?}"
            );
        }
        for kind in [Kind::Release, Kind::Source, Kind::Package] {
            assert!(!said("p", "1", kind, None).contains("not"), "{kind:?}");
        }
    }

    #[test]
    fn this_build_says_what_its_build_script_recorded() {
        // Tests are never built with the release profile.
        assert_eq!(kind(), Kind::Debug);
        assert_eq!(recorded(env!("HATCHLING_BUILD_COMMIT")), None);
        // A commit recorded is said; none, left empty, isn't.
        assert_eq!(recorded(HEAD), Some(HEAD));
        assert_eq!(recorded(""), None);
        assert_eq!(
            describe("hatchling-broker"),
            format!(
                "hatchling-broker {}\na debug build: its commit isn't recorded\n",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn each_kind_is_carried_by_its_name() {
        for kind in KINDS {
            assert_eq!(Kind::from_name(kind.name()), kind);
        }
        assert_eq!(Kind::from_name(""), Kind::Unrecorded);
        assert_eq!(Kind::from_name("Release"), Kind::Unrecorded);
    }

    #[test]
    fn a_commit_is_forty_or_sixty_four_lowercase_hex_digits() {
        assert!(is_commit(HEAD));
        assert!(is_commit(&"f".repeat(64)));
        for bad in [
            "",
            HEAD.get(1..).unwrap(),
            &format!("{HEAD}0"),
            &HEAD.to_uppercase(),
            &HEAD.replace('a', "g"),
            &"0".repeat(63),
        ] {
            assert!(!is_commit(bad), "{bad}");
        }
    }

    #[test]
    fn a_published_package_names_its_commit_and_whether_it_had_changes() {
        let clean = format!(
            "{{\n  \"git\": {{\n    \"sha1\": \"{HEAD}\"\n  }},\n  \"path_in_vcs\": \"crates/hatchling-broker\"\n}}"
        );
        assert_eq!(package_commit(&clean), Some((HEAD, false)));
        let dirty =
            format!("{{\n  \"git\": {{\n    \"sha1\": \"{HEAD}\",\n    \"dirty\": true\n  }}\n}}");
        assert_eq!(package_commit(&dirty), Some((HEAD, true)));
        for bad in [
            "{}",
            "{\"git\": {\"sha1\": \"xyz\"}}",
            "{\"git\": {\"sha1\": 5}}",
            "\"sha1\"",
        ] {
            assert_eq!(package_commit(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_release_profile_build_records_a_commit() {
        let debug = Seen {
            release_profile: false,
            release_build: false,
            package: None,
            checkout: checkout(PREFIX, HEAD, false),
        };
        assert_eq!(decide(&debug), Ok((Kind::Debug, None)));
        let release = Seen {
            release_build: true,
            ..debug
        };
        assert!(decide(&release).unwrap_err().contains("--release"));
    }

    #[test]
    fn a_checkout_of_this_repository_gives_its_commit() {
        assert_eq!(
            decide(&seen(false, None, checkout(PREFIX, HEAD, false))),
            Ok((Kind::Source, Some(HEAD)))
        );
        assert_eq!(
            decide(&seen(false, None, checkout(PREFIX, HEAD, true))),
            Ok((Kind::Modified, Some(HEAD)))
        );
        assert_eq!(
            decide(&seen(true, None, checkout(PREFIX, HEAD, false))),
            Ok((Kind::Release, Some(HEAD)))
        );
        // A copy in another repository, or a head that isn't a commit, records none.
        for other in [
            checkout("vendor/hatchling-broker/", HEAD, false),
            checkout("", HEAD, false),
            checkout(PREFIX, "HEAD", false),
            None,
        ] {
            assert_eq!(
                decide(&seen(false, None, other)),
                Ok((Kind::Unrecorded, None))
            );
        }
    }

    #[test]
    fn a_release_is_built_only_from_an_unmodified_checkout_of_this_repository() {
        let modified = decide(&seen(true, None, checkout(PREFIX, HEAD, true))).unwrap_err();
        assert!(modified.contains("no changes"), "{modified}");
        for other in [
            checkout("vendor/hatchling-broker/", HEAD, false),
            checkout(PREFIX, "", false),
            None,
        ] {
            let why = decide(&seen(true, None, other)).unwrap_err();
            assert!(why.contains("checkout of its repository"), "{why}");
        }
        let info = format!("{{\"git\": {{\"sha1\": \"{HEAD}\"}}}}");
        let packaged = decide(&seen(true, Some(&info), checkout(PREFIX, HEAD, false))).unwrap_err();
        assert!(packaged.contains("published package"), "{packaged}");
    }

    #[test]
    fn a_published_package_gives_its_own_commit_not_a_checkout_around_it() {
        let info = format!("{{\"git\": {{\"sha1\": \"{HEAD}\"}}}}");
        let around = checkout(PREFIX, "0123456789abcdef0123456789abcdef01234567", true);
        assert_eq!(
            decide(&seen(false, Some(&info), around)),
            Ok((Kind::Package, Some(HEAD)))
        );
        let dirty = format!("{{\"git\": {{\"sha1\": \"{HEAD}\", \"dirty\": true}}}}");
        assert_eq!(
            decide(&seen(false, Some(&dirty), None)),
            Ok((Kind::PackageModified, Some(HEAD)))
        );
        assert_eq!(
            decide(&seen(false, Some("{}"), None)),
            Ok((Kind::Unrecorded, None))
        );
    }
}
