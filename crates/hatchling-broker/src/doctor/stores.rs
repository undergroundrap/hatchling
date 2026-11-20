//! Whether the stores' directories are as their readers take them
//! (WO-038): what `doctor` says before a read is refused.
//!
//! A store's reader refuses a directory that isn't a directory itself,
//! isn't its owner's, or has any mode but 700, and repairs nothing: the
//! memory and goals stores (`hatchling_memory`'s chain, from the root down
//! to each user's records) and every store the vault crate opens (its
//! root, its key file's directory, its keyring's). The refusal comes from
//! whatever reads first: a feed that won't load, an unlock that fails.
//! Here they are named at once, with the commands that put them right.
//!
//! Directories a store makes private again itself (with
//! `hatchling_sys::ensure_private_dir`) aren't checked: nothing refuses
//! them. Nothing is followed through a link, and nothing is changed.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::Finding;

/// The one mode a store's directory has: its owner's alone.
const STORE_DIR_MODE: u32 = 0o700;

/// How far beneath a store's root its directories are checked. The
/// memory and goals stores keep their records two below it.
const MAX_DEPTH: usize = 8;

/// How many directories are checked, all the stores together.
const MAX_DIRS: usize = 10_000;

/// How many of the wrong directories the report names.
const SHOWN: usize = 5;

/// A store's directory to check: with every directory beneath it
/// (`deep`), or itself alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoreDir {
    /// Where it is.
    pub(super) path: PathBuf,
    /// Whether its reader takes each directory beneath it as it takes
    /// the root.
    pub(super) deep: bool,
}

/// The directories of `config`'s stores that their readers refuse when
/// wrong, each once, the deep ones first. `keyring` is the vault's
/// keyring, when there is one, and `lookup` reads the output gate's
/// settings.
pub(super) fn store_dirs(
    config: &crate::Config,
    keyring: Option<&Path>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Vec<StoreDir> {
    let deep = [&config.memory.root, &config.goals.root]
        .into_iter()
        .filter(|root| !root.as_os_str().is_empty())
        .map(|root| (root.clone(), true));
    let redaction = config
        .channel_ingress
        .as_ref()
        .and_then(|_| crate::RedactionConfig::parse(config, lookup).ok());
    let sealed = crate::vault_ops::sealed_stores(config, redaction.as_ref())
        .into_iter()
        .map(|(_, root)| root);
    let keys = [Some(config.vault.key_file.as_path()), keyring]
        .into_iter()
        .flatten()
        .filter_map(Path::parent)
        .map(Path::to_path_buf);
    let mut dirs: Vec<StoreDir> = Vec::new();
    for (path, deep) in deep.chain(sealed.chain(keys).map(|root| (root, false))) {
        if !dirs.iter().any(|known| known.path == path) {
            dirs.push(StoreDir { path, deep });
        }
    }
    dirs
}

/// What a store's reader refuses of one of its directories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wrong {
    /// A link, or anything else that isn't a directory itself.
    NotADirectory,
    /// Another user's: its owner's uid.
    Owner(u32),
    /// Any mode but 700: its mode.
    Mode(u32),
}

/// Whose a store's directories must be, to `doctor` run by `euid`: run
/// as root, anyone's, since the service's start hands each store to its
/// user; run by hand, this user's, whose every store is.
pub(super) const fn owner(euid: u32) -> Option<u32> {
    if euid == 0 { None } else { Some(euid) }
}

/// What's wrong with a directory entry, `dir` or not, owned by `uid`, of
/// `mode`, to a reader that takes only `owner`'s. With no `owner`, its
/// owner isn't checked: the service's start hands each store to its user.
pub(super) fn wrong(dir: bool, uid: u32, mode: u32, owner: Option<u32>) -> Vec<Wrong> {
    if !dir {
        return vec![Wrong::NotADirectory];
    }
    let mut found = Vec::new();
    if owner.is_some_and(|owner| owner != uid) {
        found.push(Wrong::Owner(uid));
    }
    if mode & 0o777 != STORE_DIR_MODE {
        found.push(Wrong::Mode(mode & 0o777));
    }
    found
}

/// What the check saw.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Seen {
    /// Each wrong directory, with what's wrong.
    pub(super) wrong: Vec<(PathBuf, Wrong)>,
    /// How many directories were checked.
    pub(super) checked: usize,
    /// Whether some weren't, past [`MAX_DIRS`] or [`MAX_DEPTH`].
    pub(super) cut: bool,
}

/// Checks `dirs`, each that exists: a store not made yet is made right
/// when the broker starts.
pub(super) fn check(dirs: &[StoreDir], owner: Option<u32>) -> Seen {
    let mut seen = Seen::default();
    for dir in dirs {
        visit(&dir.path, dir.deep, 0, owner, &mut seen);
    }
    seen
}

/// Checks `path`, and when `deep`, each directory beneath it, `depth`
/// below its store's root.
fn visit(path: &Path, deep: bool, depth: usize, owner: Option<u32>, seen: &mut Seen) {
    if seen.checked >= MAX_DIRS {
        seen.cut = true;
        return;
    }
    // Never through a link: a link where a directory belongs is wrong
    // itself, whatever it points at.
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    seen.checked = seen.checked.saturating_add(1);
    let dir = meta.file_type().is_dir();
    for found in wrong(dir, meta.uid(), meta.mode(), owner) {
        seen.wrong.push((path.to_path_buf(), found));
    }
    if !deep || !dir {
        return;
    }
    if depth >= MAX_DEPTH {
        seen.cut = true;
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    // The directories beneath, and links to one: a reader walking down
    // takes a link where it expects a directory as wrong.
    let mut below: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || (kind.is_symlink() && entry.path().is_dir()))
        })
        .map(|entry| entry.path())
        .collect();
    below.sort();
    for path in below {
        visit(&path, true, depth.saturating_add(1), owner, seen);
    }
}

/// What `doctor` says of what it `seen`: each wrong directory, and the
/// commands that put each right, run with `sudo` when `as_root`.
pub(super) fn said(seen: &Seen, as_root: bool) -> Finding {
    let all = if seen.cut { " (the first ones)" } else { "" };
    if seen.wrong.is_empty() {
        return Finding::Fine(match seen.checked {
            0 => String::from("none made yet: the broker makes each as it should be"),
            1 => format!("1 directory checked{all}: its owner's alone, mode 700"),
            many => format!("{many} directories checked{all}: each its owner's alone, mode 700"),
        });
    }
    let named: Vec<String> = seen
        .wrong
        .iter()
        .take(SHOWN)
        .map(|(path, wrong)| {
            let how = match wrong {
                Wrong::NotADirectory => String::from("a link or a file, not a directory"),
                Wrong::Owner(uid) => format!("owned by uid {uid}"),
                Wrong::Mode(mode) => format!("mode {mode:o}"),
            };
            format!("{} ({how})", path.display())
        })
        .collect();
    let more = seen.wrong.len().saturating_sub(SHOWN);
    let what = format!(
        "a store refuses {} of its directories, so what reads it fails: {}{}",
        seen.wrong.len(),
        named.join(", "),
        if more == 0 {
            String::new()
        } else {
            format!(", and {more} more")
        }
    );
    Finding::problem(what, fix(&seen.wrong, as_root))
}

/// The commands that put `wrong` right: a link or file moved aside, the
/// owner given back, then the mode set (the owner first: only it, or root,
/// may change the mode).
fn fix(wrong: &[(PathBuf, Wrong)], as_root: bool) -> String {
    let sudo = if as_root { "sudo " } else { "" };
    let paths = |pick: fn(&Wrong) -> bool| -> Vec<String> {
        let mut paths: Vec<String> = wrong
            .iter()
            .filter(|(_, wrong)| pick(wrong))
            .map(|(path, _)| quoted(path))
            .collect();
        paths.dedup();
        paths
    };
    let mut steps = Vec::new();
    let moved = paths(|wrong| matches!(wrong, Wrong::NotADirectory));
    if !moved.is_empty() {
        steps.push(format!(
            "move aside {} (a store's directory is a directory, never a link: Hatchling makes it again as it starts)",
            moved.join(" ")
        ));
    }
    let owned = paths(|wrong| matches!(wrong, Wrong::Owner(_)));
    if !owned.is_empty() {
        steps.push(format!("`sudo chown \"$(id -u)\" {}`", owned.join(" ")));
    }
    let modes = paths(|wrong| matches!(wrong, Wrong::Mode(_)));
    if !modes.is_empty() {
        steps.push(format!("`{sudo}chmod 700 {}`", modes.join(" ")));
    }
    format!("{}, then run doctor again.", steps.join(", then "))
}

/// `path` as a shell word: as it is when it's only letters, digits and
/// `/._-+:@,`, else in single quotes.
fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    let plain = !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-+:@,".contains(&byte));
    if plain {
        text
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn me() -> u32 {
        hatchling_sys::euid()
    }

    /// What a problem says and its fix; anything else, said as it is,
    /// with no fix.
    fn problem(finding: Finding) -> (String, String) {
        match finding {
            Finding::Problem { what, fix } => (what, fix),
            other @ (Finding::Fine(_) | Finding::Unchecked(_) | Finding::Note { .. }) => {
                (format!("{other:?}"), String::new())
            }
        }
    }

    /// A scratch directory, read as a temporary one is.
    struct Base<'a>(&'a Path);

    impl Base<'_> {
        const fn path(&self) -> &Path {
            self.0
        }
    }

    fn private(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn set_mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn deep(path: &Path) -> StoreDir {
        StoreDir {
            path: path.to_path_buf(),
            deep: true,
        }
    }

    /// A directory is right only as a directory itself, its owner's, and
    /// mode 700; the owner is checked only when one is asked for.
    #[test]
    fn a_store_directory_is_a_directory_its_owners_alone() {
        assert_eq!(wrong(true, 7, 0o40700, Some(7)), []);
        assert_eq!(wrong(true, 7, 0o40700, None), []);
        assert_eq!(wrong(false, 7, 0o120_777, Some(7)), [Wrong::NotADirectory]);
        assert_eq!(wrong(false, 7, 0o100_700, None), [Wrong::NotADirectory]);
        assert_eq!(wrong(true, 0, 0o40700, Some(7)), [Wrong::Owner(0)]);
        assert_eq!(wrong(true, 0, 0o40700, None), []);
        // Open to the group or others, or closed to its owner: any mode but 700.
        // (The bits above the permissions, as setgid, the readers don't read.)
        assert_eq!(wrong(true, 7, 0o42700, Some(7)), []);
        for mode in [0o755, 0o750, 0o701, 0o710, 0o500, 0o600, 0o777] {
            assert_eq!(
                wrong(true, 7, mode, Some(7)),
                [Wrong::Mode(mode & 0o777)],
                "{mode:o}"
            );
        }
        assert_eq!(
            wrong(true, 0, 0o755, Some(7)),
            [Wrong::Owner(0), Wrong::Mode(0o755)]
        );
    }

    /// Run as root, the owner is left to the service's start; run by hand,
    /// it is this user.
    #[test]
    fn the_owner_is_checked_only_run_by_hand() {
        assert_eq!(owner(0), None);
        assert_eq!(owner(985), Some(985));
    }

    /// Every directory beneath a deep store is checked, as its reader
    /// walks down to each user's records; a link where one belongs is
    /// wrong, and nothing is followed through it.
    #[test]
    fn each_directory_in_a_store_is_checked_and_no_link_followed() {
        let base = crate::api_auth::tests::scratch("stores-walk");
        let base = Base(&base.0);
        let root = base.path().join("goals");
        private(&root.join("goals/985"));
        private(&root.join("tracking/985"));
        for dir in ["", "goals", "tracking"] {
            set_mode(&root.join(dir), 0o700);
        }
        std::fs::write(root.join("goals/985/a.json"), b"{}").unwrap();
        // A file, and a link to one, aren't directories the walk goes down:
        // what a store does with its files is its own.
        std::os::unix::fs::symlink(root.join("goals/985/a.json"), root.join("goals/985/b.json"))
            .unwrap();
        let seen = check(&[deep(&root)], Some(me()));
        assert_eq!(seen.wrong, []);
        assert_eq!((seen.checked, seen.cut), (5, false));

        // The drill's store (2026-11-14): one user's directory at 755.
        set_mode(&root.join("goals/985"), 0o755);
        // A link where a directory belongs, to one open to all.
        let open = base.path().join("open");
        std::fs::create_dir(&open).unwrap();
        set_mode(&open, 0o777);
        std::os::unix::fs::symlink(&open, root.join("tracking/986")).unwrap();
        let seen = check(&[deep(&root)], Some(me()));
        assert_eq!(
            seen.wrong,
            [
                (root.join("goals/985"), Wrong::Mode(0o755)),
                (root.join("tracking/986"), Wrong::NotADirectory),
            ]
        );
        assert_eq!(seen.checked, 6, "the link's target isn't walked");
    }

    /// A store checked at its root alone has nothing beneath it checked,
    /// and one not made yet isn't wrong.
    #[test]
    fn a_root_is_checked_alone_and_an_unmade_store_is_fine() {
        let base = crate::api_auth::tests::scratch("stores-root");
        let base = Base(&base.0);
        let root = base.path().join("artifacts");
        private(&root.join("inner"));
        set_mode(&root, 0o700);
        set_mode(&root.join("inner"), 0o755);
        let alone = StoreDir {
            path: root.clone(),
            deep: false,
        };
        let unmade = deep(&base.path().join("never"));
        let seen = check(&[alone, unmade], Some(me()));
        assert_eq!((seen.wrong, seen.checked), (vec![], 1));
        set_mode(&root, 0o750);
        let seen = check(&[deep(&root)], None);
        assert_eq!(
            seen.wrong,
            [
                (root.clone(), Wrong::Mode(0o750)),
                (root.join("inner"), Wrong::Mode(0o755)),
            ]
        );
    }

    /// The walk stops at its depth, and says it did.
    #[test]
    fn a_walk_past_its_depth_says_it_stopped() {
        let base = crate::api_auth::tests::scratch("stores-depth");
        let base = Base(&base.0);
        let mut path = base.path().join("memory");
        for level in 0..=MAX_DEPTH {
            path = path.join(format!("d{level}"));
        }
        private(&path);
        let mut above = path.as_path();
        while above != base.path() {
            set_mode(above, 0o700);
            above = above.parent().unwrap();
        }
        let seen = check(&[deep(&base.path().join("memory"))], Some(me()));
        assert_eq!(seen.wrong, []);
        assert!(seen.cut);
        assert_eq!(seen.checked, MAX_DEPTH + 1);
    }

    /// Each wrong directory is named with what's wrong, and the fix gives
    /// the commands for each, the owner before the mode, with `sudo` when
    /// doctor runs as root.
    #[test]
    fn each_wrong_directory_is_named_with_its_fix() {
        let seen = Seen {
            wrong: vec![
                (
                    PathBuf::from("/var/lib/hatchling/goals/goals/985"),
                    Wrong::Mode(0o755),
                ),
                (PathBuf::from("/srv/my stores/memory"), Wrong::Owner(0)),
                (PathBuf::from("/srv/my stores/memory"), Wrong::Mode(0o750)),
                (
                    PathBuf::from("/var/lib/hatchling/goals/tracking/986"),
                    Wrong::NotADirectory,
                ),
            ],
            checked: 9,
            cut: false,
        };
        let (what, fix) = problem(said(&seen, true));
        assert_eq!(
            what,
            "a store refuses 4 of its directories, so what reads it fails: \
             /var/lib/hatchling/goals/goals/985 (mode 755), /srv/my stores/memory (owned by uid 0), \
             /srv/my stores/memory (mode 750), /var/lib/hatchling/goals/tracking/986 (a link or a file, not a directory)"
        );
        assert_eq!(
            fix,
            "move aside /var/lib/hatchling/goals/tracking/986 (a store's directory is a directory, never a link: \
             Hatchling makes it again as it starts), \
             then `sudo chown \"$(id -u)\" '/srv/my stores/memory'`, \
             then `sudo chmod 700 /var/lib/hatchling/goals/goals/985 '/srv/my stores/memory'`, \
             then run doctor again."
        );
        let (_, fix) = problem(said(&seen, false));
        assert!(fix.contains("`chmod 700 /var/lib"), "{fix}");
        assert!(fix.contains("`sudo chown"), "{fix}");
    }

    /// Past five, the rest are counted; a clean check says how many it
    /// checked, and whether it stopped short.
    #[test]
    fn many_are_counted_and_a_clean_check_says_what_it_saw() {
        let wrong: Vec<(PathBuf, Wrong)> = (0..7)
            .map(|n| (PathBuf::from(format!("/s/{n}")), Wrong::Mode(0o755)))
            .collect();
        let seen = Seen {
            wrong,
            checked: 7,
            cut: false,
        };
        let (what, fix) = problem(said(&seen, true));
        assert!(what.ends_with("/s/4 (mode 755), and 2 more"), "{what}");
        assert!(
            fix.contains("chmod 700 /s/0 /s/1 /s/2 /s/3 /s/4 /s/5 /s/6`"),
            "{fix}"
        );
        let fine = |checked, cut| {
            said(
                &Seen {
                    wrong: vec![],
                    checked,
                    cut,
                },
                true,
            )
        };
        assert_eq!(
            fine(0, false),
            Finding::Fine(String::from(
                "none made yet: the broker makes each as it should be"
            ))
        );
        assert_eq!(
            fine(1, false),
            Finding::Fine(String::from(
                "1 directory checked: its owner's alone, mode 700"
            ))
        );
        assert_eq!(
            fine(12, true),
            Finding::Fine(String::from(
                "12 directories checked (the first ones): each its owner's alone, mode 700"
            ))
        );
    }

    /// The stores checked: memory's and goals' with all beneath them, then
    /// each sealed store's root, the key file's directory and the keyring's,
    /// each once.
    #[test]
    fn the_stores_checked_are_the_ones_whose_readers_refuse() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.memory.root = PathBuf::from("/srv/h/memory");
        config.goals.root = PathBuf::from("/srv/h/goals");
        config.artifacts.root = PathBuf::from("/srv/h/artifacts");
        let keyring = PathBuf::from("/srv/h/keyring/keyring");
        let dirs = store_dirs(&config, Some(&keyring), &|_| None);
        let deep: Vec<&Path> = dirs
            .iter()
            .filter(|dir| dir.deep)
            .map(|dir| dir.path.as_path())
            .collect();
        assert_eq!(
            deep,
            [Path::new("/srv/h/memory"), Path::new("/srv/h/goals")]
        );
        let roots: Vec<&Path> = dirs
            .iter()
            .filter(|dir| !dir.deep)
            .map(|dir| dir.path.as_path())
            .collect();
        assert!(roots.contains(&config.vault.root.as_path()), "{roots:?}");
        assert!(roots.contains(&Path::new("/srv/h/artifacts")), "{roots:?}");
        assert!(
            roots.contains(&config.vault.key_file.parent().unwrap()),
            "{roots:?}"
        );
        assert!(roots.contains(&Path::new("/srv/h/keyring")), "{roots:?}");
        let unique: std::collections::HashSet<&Path> =
            dirs.iter().map(|dir| dir.path.as_path()).collect();
        assert_eq!(unique.len(), dirs.len());
        // The same keyring directory as another store's is named once.
        let twice = store_dirs(&config, Some(&config.artifacts.root.join("ring")), &|_| {
            None
        });
        assert_eq!(twice.len(), dirs.len() - 1);
    }
}
