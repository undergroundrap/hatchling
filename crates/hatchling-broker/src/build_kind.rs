//! How a build of the broker was made, and from which commit: decided once,
//! by the build script (`build.rs`, which includes this file), from what it
//! sees. [`crate::version`] says it, and this file's tests run in the lib.
//!
//! The build script passes the decision to the compiler as two variables,
//! [`KIND_VAR`] and [`COMMIT_VAR`].

/// The variable naming the build's kind, as [`Kind::name`] writes it.
pub(crate) const KIND_VAR: &str = "HATCHLING_BUILD_KIND";
/// The variable holding the build's commit, empty when it isn't recorded.
pub(crate) const COMMIT_VAR: &str = "HATCHLING_BUILD_COMMIT";
/// Set to `release` by `scripts/release-artifacts.sh`: a build made as a
/// release is made, which a modified tree fails.
pub(crate) const RELEASE_VAR: &str = "HATCHLING_BUILD";
/// Where the broker sits in this repository, as `git rev-parse
/// --show-prefix` names it: a copy in another repository never takes that
/// repository's commit.
pub(crate) const PREFIX: &str = "crates/hatchling-broker/";

/// What the build script saw.
pub(crate) struct Seen<'a> {
    /// Built with the release profile. Any other build (tests, checks,
    /// mutation runs) records no commit, and costs nothing to rebuild.
    pub(crate) release_profile: bool,
    /// [`RELEASE_VAR`] is `release`.
    pub(crate) release_build: bool,
    /// The published package's `.cargo_vcs_info.json`, when built from one.
    pub(crate) package: Option<&'a str>,
    /// The git checkout it was built in, when there's one.
    pub(crate) checkout: Option<Checkout<'a>>,
}

/// A git checkout, as the build script read it.
pub(crate) struct Checkout<'a> {
    /// The package's directory within it.
    pub(crate) prefix: &'a str,
    /// Its commit.
    pub(crate) head: &'a str,
    /// Whether its tracked files differ from the commit.
    pub(crate) modified: bool,
}

/// How a build was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// By `scripts/release-artifacts.sh`, from an unmodified checkout.
    Release,
    /// With the release profile, from an unmodified checkout.
    Source,
    /// With the release profile, from a checkout with changes.
    Modified,
    /// From the published package of a commit.
    Package,
    /// From a published package made from a tree with changes.
    PackageModified,
    /// Not with the release profile: the commit isn't recorded.
    Debug,
    /// With the release profile, where no commit could be read.
    Unrecorded,
}

/// Every kind, in the order [`Kind::name`] is tested.
pub(crate) const KINDS: [Kind; 7] = [
    Kind::Release,
    Kind::Source,
    Kind::Modified,
    Kind::Package,
    Kind::PackageModified,
    Kind::Debug,
    Kind::Unrecorded,
];

impl Kind {
    /// The kind's name, as [`KIND_VAR`] carries it.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Source => "source",
            Self::Modified => "modified",
            Self::Package => "package",
            Self::PackageModified => "package-modified",
            Self::Debug => "debug",
            Self::Unrecorded => "unrecorded",
        }
    }

    /// The kind [`Kind::name`] names; any other name is [`Kind::Unrecorded`].
    pub(crate) fn from_name(name: &str) -> Self {
        KINDS
            .into_iter()
            .find(|kind| kind.name() == name)
            .unwrap_or(Self::Unrecorded)
    }
}

/// A commit's id: 40 hex digits, or 64 in a SHA-256 repository.
pub(crate) fn is_commit(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The commit a published package's `.cargo_vcs_info.json` names, and
/// whether it says the tree had changes.
pub(crate) fn package_commit(info: &str) -> Option<(&str, bool)> {
    let sha1 = info.split_once("\"sha1\"")?.1.split('"').nth(1)?;
    let dense: String = info.split_whitespace().collect();
    is_commit(sha1).then_some((sha1, dense.contains("\"dirty\":true")))
}

/// The build's kind and commit, or why it can't be built: a release built
/// any way but from an unmodified checkout of this repository, with the
/// release profile.
pub(crate) fn decide<'a>(seen: &Seen<'a>) -> Result<(Kind, Option<&'a str>), &'static str> {
    if !seen.release_profile {
        return if seen.release_build {
            Err("a release is built with the release profile (--release)")
        } else {
            Ok((Kind::Debug, None))
        };
    }
    if let Some(info) = seen.package {
        if seen.release_build {
            return Err("a release is built from a checkout, not a published package");
        }
        return Ok(match package_commit(info) {
            Some((commit, false)) => (Kind::Package, Some(commit)),
            Some((commit, true)) => (Kind::PackageModified, Some(commit)),
            None => (Kind::Unrecorded, None),
        });
    }
    match &seen.checkout {
        Some(checkout) if checkout.prefix == PREFIX && is_commit(checkout.head) => {
            match (checkout.modified, seen.release_build) {
                (true, true) => Err("a release is built from a checkout with no changes"),
                (true, false) => Ok((Kind::Modified, Some(checkout.head))),
                (false, true) => Ok((Kind::Release, Some(checkout.head))),
                (false, false) => Ok((Kind::Source, Some(checkout.head))),
            }
        }
        _ if seen.release_build => Err("a release is built from a checkout of its repository"),
        _ => Ok((Kind::Unrecorded, None)),
    }
}
