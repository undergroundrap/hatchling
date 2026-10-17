//! The broker's leftovers, and their safe removal (WO-012).
//!
//! Two kinds of files are disposable:
//!
//! - a cell's staging directory, `$TMPDIR/hatchling-staging-<pid>-<random>/`,
//!   named for the broker process that made it;
//! - a scheduled run's directory, `<scheduler dir>.runs/run-<token>/`.
//!
//! The broker removes them when a cell or run ends; one killed mid-run
//! leaves them behind. So before it serves, a broker sweeps its own
//! leftovers: staging directories whose broker is gone, and every run
//! directory (none is live before it serves). `hatchling-broker clean`
//! does the same on demand, and refuses while a broker serves.
//!
//! Never your data. Only entries named as above, owned by this user, are
//! swept, each through `hatchling_sys::remove_tree`: no symlink is
//! followed, no mount is entered, and every configured state directory
//! is kept whole, wherever it is.
//!
//! A serving broker holds a lock beside its socket (`<socket>.lock`) for
//! its life; the kernel lets it go however the broker ends.

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use hatchling_sys::{DirId, HeldLock, Removed};

use crate::output_gate::RedactionConfig;
use crate::{Config, Error};

/// What a staging directory's name starts with.
const STAGING_PREFIX: &str = "hatchling-staging-";

/// The random part of a staging directory's name: 16 bytes, as hex.
const STAGING_RANDOM_LEN: usize = 32;

/// What a run directory's name starts with.
const RUN_PREFIX: &str = "run-";

/// The random part of a run directory's name.
const RUN_RANDOM_LEN: usize = 24;

/// Whether `text` is `len` lower-case hex digits.
fn lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The broker pid a staging directory's name carries, if it is one.
fn staging_pid(name: &str) -> Option<i32> {
    let (pid, random) = name.strip_prefix(STAGING_PREFIX)?.split_once('-')?;
    if !lower_hex(random, STAGING_RANDOM_LEN) || !pid.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok().filter(|pid| *pid > 0)
}

/// Whether `name` is a run directory's.
fn is_run(name: &str) -> bool {
    name.strip_prefix(RUN_PREFIX)
        .is_some_and(|random| lower_hex(random, RUN_RANDOM_LEN))
}

/// The lock a serving broker holds, beside its socket.
#[must_use]
pub fn lock_path(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Takes the serving lock for `config`'s socket, held for as long as the
/// result is.
///
/// # Errors
///
/// Returns [`Error::BrokerBusy`] while another process holds it, and
/// [`Error::Sweep`] if it can't be taken.
pub fn take_lock(config: &Config) -> Result<HeldLock, Error> {
    let path = lock_path(config.socket.as_path());
    match hatchling_sys::try_lock(&path) {
        Ok(Some(lock)) => Ok(lock),
        Ok(None) => Err(Error::BrokerBusy { lock: path }),
        Err(source) => Err(Error::Sweep {
            reason: format!("{}: {source}", path.display()),
        }),
    }
}

/// A leftover: its root and its name there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    /// The directory it is in: the staging root or the run root.
    pub root: PathBuf,
    /// Its name there.
    pub name: OsString,
}

impl Leftover {
    /// Its path.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(&self.name)
    }
}

/// Every entry of `root` that `disposable` names, a directory itself
/// owned by this user. No root, or one that can't be read, has none.
fn entries(root: &Path, disposable: impl Fn(&str) -> bool) -> Vec<Leftover> {
    let Ok(read) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let euid = hatchling_sys::euid();
    let mut found: Vec<Leftover> = read
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(&disposable))
        .filter(|entry| {
            std::fs::symlink_metadata(entry.path())
                .is_ok_and(|meta| meta.file_type().is_dir() && meta.uid() == euid)
        })
        .map(|entry| Leftover {
            root: root.to_path_buf(),
            name: entry.file_name(),
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// The leftovers in `staging_root` (staging directories whose broker is
/// gone, never this process's own) and in `run_root` (every run
/// directory), in that order.
#[must_use]
pub fn leftovers(staging_root: &Path, run_root: Option<&Path>) -> Vec<Leftover> {
    let own = i32::try_from(std::process::id()).unwrap_or(0);
    let mut found = entries(staging_root, |name| {
        staging_pid(name).is_some_and(|pid| pid != own && !hatchling_sys::pid_alive(pid))
    });
    if let Some(run_root) = run_root {
        found.extend(entries(run_root, is_run));
    }
    found
}

/// Every state directory `config` (and `redaction`, when the output gate
/// is configured) names: the sweep keeps each whole.
#[must_use]
pub fn state_dirs(config: &Config, redaction: Option<&RedactionConfig>) -> Vec<PathBuf> {
    let mut dirs = vec![
        config.vault.root.clone(),
        config.skills.skills_dir.clone(),
        config.memory.root.clone(),
        config.goals.root.clone(),
        config.scheduler.root.clone(),
        config.artifacts.root.clone(),
        config.spaces.root.clone(),
    ];
    dirs.extend(config.artifacts.workspace.clone());
    dirs.extend(redaction.map(|redaction| redaction.dir.clone()));
    dirs.retain(|dir| !dir.as_os_str().is_empty());
    dirs
}

/// `config`'s run root, when scheduling is on.
fn run_root(config: &Config) -> Option<PathBuf> {
    config
        .scheduler
        .enabled()
        .then(|| config.scheduler.run_root())
}

/// Removes `found`, keeping every directory in `keep`; what became of
/// each, as lines, with a count last. Each is logged.
fn remove(found: &[Leftover], keep: &[PathBuf]) -> Vec<String> {
    let keep: Vec<DirId> = keep.iter().filter_map(|dir| DirId::of(dir)).collect();
    let mut lines = Vec::with_capacity(found.len() + 1);
    let mut removed = 0;
    for leftover in found {
        let path = leftover.path();
        let shown = path.display();
        match hatchling_sys::remove_tree(&leftover.root, &leftover.name, &keep) {
            Ok(Removed::Whole) => {
                removed += 1;
                tracing::info!(path = %shown, "removed a leftover");
                lines.push(format!("removed {shown}"));
            }
            Ok(Removed::Partly) => {
                removed += 1;
                tracing::warn!(
                    path = %shown,
                    "removed a leftover in part: kept a state directory, a mount, or what couldn't go"
                );
                lines.push(format!(
                    "removed {shown} in part (kept a state directory, a mount, or what couldn't go)"
                ));
            }
            Err(error) => {
                tracing::warn!(path = %shown, %error, "left a leftover");
                lines.push(format!("left {shown}: {error}"));
            }
        }
    }
    tracing::info!(
        removed,
        found = found.len(),
        "swept the leftovers of earlier runs"
    );
    lines.push(format!("{removed} of {} leftover(s) removed", found.len()));
    lines
}

/// The startup sweep: a serving broker, holding its lock, removes the
/// leftovers of earlier brokers; what became of each, as it logged them.
/// A failure is logged, never fatal.
#[must_use]
pub fn sweep_at_start(config: &Config, redaction: Option<&RedactionConfig>) -> Vec<String> {
    sweep_in(config, redaction, &std::env::temp_dir())
}

/// [`sweep_at_start`], with staging directories in `staging_root`.
fn sweep_in(
    config: &Config,
    redaction: Option<&RedactionConfig>,
    staging_root: &Path,
) -> Vec<String> {
    let found = leftovers(staging_root, run_root(config).as_deref());
    remove(&found, &state_dirs(config, redaction))
}

/// `hatchling-broker clean [--dry-run]`: the startup sweep on demand,
/// refused while a broker serves; with `dry_run`, what it would remove,
/// removing nothing.
///
/// # Errors
///
/// Returns [`Error::BrokerBusy`] while a broker serves `config`'s
/// socket, and the lock's or the redaction settings' errors.
pub fn clean(config: &Config, dry_run: bool) -> Result<String, Error> {
    clean_in(config, &std::env::temp_dir(), dry_run)
}

/// [`clean`], with staging directories in `staging_root`.
fn clean_in(config: &Config, staging_root: &Path, dry_run: bool) -> Result<String, Error> {
    let _lock = take_lock(config)?;
    let found = leftovers(staging_root, run_root(config).as_deref());
    if dry_run {
        let mut lines: Vec<String> = found
            .iter()
            .map(|leftover| format!("would remove {}", leftover.path().display()))
            .collect();
        lines.push(format!("{} leftover(s); nothing removed", found.len()));
        return Ok(lines.join("\n"));
    }
    let redaction = config
        .channel_ingress
        .as_ref()
        .map(|_| RedactionConfig::from_env(config))
        .transpose()?;
    Ok(remove(&found, &state_dirs(config, redaction.as_ref())).join("\n"))
}

/// Runs `hatchling-broker clean`, printing what it did.
///
/// # Errors
///
/// As [`clean`], and [`Error::Usage`] for another argument.
pub fn run_clean(config: &Config, args: &[String]) -> Result<(), Error> {
    use std::io::Write as _;
    let dry_run = match args {
        [] => false,
        [flag] if flag == "--dry-run" => true,
        _ => {
            return Err(Error::Usage {
                reason: String::from("usage: hatchling-broker clean [--dry-run]"),
            });
        }
    };
    let report = clean(config, dry_run)?;
    writeln!(std::io::stdout().lock(), "{report}").map_err(|e| Error::Sweep {
        reason: format!("cannot write the report: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;
    use std::process::Command;
    use std::time::Duration;

    use super::*;
    use crate::api_auth::tests::{owner_config, scratch};

    /// `len` lower-case hex digits, from the two given.
    fn hex(pair: &str, len: usize) -> String {
        pair.repeat(len / 2)
    }

    /// A staging directory's name for `pid`.
    fn staging(pid: i32, pair: &str) -> String {
        format!("hatchling-staging-{pid}-{}", hex(pair, STAGING_RANDOM_LEN))
    }

    /// A run directory's name.
    fn run(pair: &str) -> String {
        format!("run-{}", hex(pair, RUN_RANDOM_LEN))
    }

    /// A config whose socket, vault and scheduler are all in `dir`, with
    /// the staging root `dir/tmp` made.
    fn config_in(dir: &Path) -> Config {
        let mut config = owner_config(Some(&dir.join("schedules").to_string_lossy()));
        config.socket = dir.join("broker.sock").to_str().unwrap().parse().unwrap();
        config.vault.root = dir.join("vault");
        std::fs::create_dir_all(dir.join("tmp")).unwrap();
        std::fs::create_dir_all(config.scheduler.run_root()).unwrap();
        config
    }

    /// A pid no process has: a child's, once it is reaped.
    fn dead_pid() -> i32 {
        let mut child = Command::new("true").spawn().unwrap();
        child.wait().unwrap();
        i32::try_from(child.id()).unwrap()
    }

    /// Every path under `dir`, symlinks not followed, sorted.
    fn snapshot(dir: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).unwrap() {
                let path = entry.unwrap().path();
                if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                    pending.push(path.clone());
                }
                paths.push(path);
            }
        }
        paths.sort();
        paths
    }

    #[test]
    fn the_storage_guide_lists_every_path_the_broker_is_told() {
        let guide = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/storage.md"),
        )
        .unwrap();
        let paths = crate::environment::ENVIRONMENT.iter().filter(|variable| {
            [
                "_DIR",
                "_FILE",
                "_KEYRING",
                "_SOCKET",
                "_WORKSPACE",
                "_ROOTS",
            ]
            .iter()
            .any(|suffix| variable.name.ends_with(suffix))
                || variable.name == "HATCHLING_CHANNEL_INGRESS"
        });
        let mut listed = 0;
        for variable in paths {
            listed += 1;
            assert!(
                guide.contains(&format!("`{}`", variable.name)),
                "docs/storage.md must say whether {} is your data or disposable",
                variable.name
            );
        }
        assert!(listed >= 19, "{listed}");
        for disposable in [STAGING_PREFIX, RUN_PREFIX, ".lock`", "clean --dry-run"] {
            assert!(guide.contains(disposable), "{disposable}");
        }
    }

    #[test]
    fn runs_are_swept_only_where_scheduling_is_on() {
        let dir = scratch("sweep-root");
        let config = config_in(&dir.0);
        assert_eq!(run_root(&config), Some(dir.0.join("schedules.runs")));
        assert_eq!(run_root(&owner_config(None)), None);
    }

    #[test]
    fn only_a_disposable_name_is_one() {
        assert_eq!(staging_pid(&staging(123, "a1")), Some(123));
        let random = hex("a1", STAGING_RANDOM_LEN);
        for name in [
            format!("hatchling-staging-123-{}", random.get(1..).unwrap()),
            format!("hatchling-staging-123-{random}0"),
            format!("hatchling-staging-123-{}", random.to_ascii_uppercase()),
            format!("hatchling-staging-0-{random}"),
            format!("hatchling-staging-+5-{random}"),
            format!("hatchling-staging-12a-{random}"),
            format!("hatchling-staging--{random}"),
            format!("hatchling-staging-99999999999-{random}"),
            String::from("hatchling-staging-123"),
            format!("hatchling-stage-123-{random}"),
        ] {
            assert_eq!(staging_pid(&name), None, "{name}");
        }
        assert!(is_run(&run("b2")));
        for name in [
            format!("run-{}", hex("b2", 22)),
            format!("run-{}0", hex("b2", 24)),
            format!("run-{}", hex("B2", 24)),
            format!("runs{}", hex("b2", 24)),
        ] {
            assert!(!is_run(&name), "{name}");
        }
        assert!(lower_hex("0123456789abcdef", 16));
        assert!(!lower_hex("0123456789abcdeg", 16) && !lower_hex("ab", 3));
        assert_eq!(
            lock_path(Path::new("/srv/broker.sock")),
            PathBuf::from("/srv/broker.sock.lock")
        );
        let leftover = Leftover {
            root: PathBuf::from("/srv/runs"),
            name: OsString::from("run-1"),
        };
        assert_eq!(leftover.path(), PathBuf::from("/srv/runs/run-1"));
    }

    #[test]
    fn a_live_brokers_staging_and_anything_not_disposable_is_left_alone() {
        let dir = scratch("sweep-list");
        let config = config_in(&dir.0);
        let (tmp, runs) = (dir.0.join("tmp"), config.scheduler.run_root());
        let outside = dir.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
        let live = i32::try_from(sleeper.id()).unwrap();
        let own = i32::try_from(std::process::id()).unwrap();
        let dead = dead_pid();
        for pid in [dead, live, own] {
            std::fs::create_dir(tmp.join(staging(pid, "a1"))).unwrap();
        }
        // Named as a dead broker's, but a file, or a symlink: not swept.
        std::fs::write(tmp.join(staging(dead, "c3")), b"file").unwrap();
        symlink(&outside, tmp.join(staging(dead, "d4"))).unwrap();
        std::fs::create_dir(tmp.join("hatchling-staging-junk")).unwrap();
        std::fs::create_dir(runs.join(run("b2"))).unwrap();
        std::fs::create_dir(runs.join("not-a-run")).unwrap();
        symlink(&outside, runs.join(run("e5"))).unwrap();
        let found: Vec<PathBuf> = leftovers(&tmp, Some(&runs))
            .iter()
            .map(Leftover::path)
            .collect();
        assert_eq!(found, [tmp.join(staging(dead, "a1")), runs.join(run("b2"))]);
        assert_eq!(leftovers(&tmp, None).len(), 1, "no run root, no runs");
        assert!(leftovers(&dir.0.join("missing"), None).is_empty());
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
        assert_eq!(
            leftovers(&tmp, None).len(),
            2,
            "its broker gone, its staging is a leftover"
        );
    }

    #[test]
    fn clean_dry_run_lists_exactly_what_clean_removes_and_changes_nothing() {
        let dir = scratch("sweep-clean");
        let config = config_in(&dir.0);
        let (tmp, runs) = (dir.0.join("tmp"), config.scheduler.run_root());
        let outside = dir.0.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("file"), b"keep").unwrap();
        let dead = tmp.join(staging(dead_pid(), "a1"));
        let cell = dead.join(format!("cell-{}", hex("f6", 32))).join("root");
        std::fs::create_dir_all(&cell).unwrap();
        std::fs::write(cell.join("task"), b"t").unwrap();
        symlink(outside.join("file"), cell.join("link")).unwrap();
        let ran = runs.join(run("b2"));
        std::fs::create_dir_all(ran.join("tmp")).unwrap();
        std::fs::write(ran.join("tmp").join("out"), b"o").unwrap();
        // The lock file is made the first time it is taken.
        drop(take_lock(&config).unwrap());
        let before = snapshot(&dir.0);
        let listed = clean_in(&config, &tmp, true).unwrap();
        assert_eq!(
            listed,
            [
                format!("would remove {}", dead.display()),
                format!("would remove {}", ran.display()),
                String::from("2 leftover(s); nothing removed"),
            ]
            .join("\n")
        );
        assert_eq!(snapshot(&dir.0), before, "a dry run changes nothing");
        let done = clean_in(&config, &tmp, false).unwrap();
        assert_eq!(
            done,
            [
                format!("removed {}", dead.display()),
                format!("removed {}", ran.display()),
                String::from("2 of 2 leftover(s) removed"),
            ]
            .join("\n")
        );
        assert!(!dead.exists() && !ran.exists());
        assert_eq!(std::fs::read(outside.join("file")).unwrap(), b"keep");
        assert_eq!(
            clean_in(&config, &tmp, true).unwrap(),
            "0 leftover(s); nothing removed"
        );
        // `clean`, `run_clean` and `sweep_at_start` sweep the process's
        // own `TMPDIR`: `tests/sweep.rs` runs them in a child whose
        // `TMPDIR` is its own. A sweep here must never touch a shared one.
    }

    #[test]
    fn clean_and_a_second_broker_are_refused_while_one_serves() {
        let dir = scratch("sweep-busy");
        let config = config_in(&dir.0);
        let (tmp, ran) = (
            dir.0.join("tmp"),
            config.scheduler.run_root().join(run("b2")),
        );
        std::fs::create_dir(&ran).unwrap();
        let serving = take_lock(&config).unwrap();
        let lock = lock_path(config.socket.as_path());
        for dry_run in [true, false] {
            assert!(matches!(
                clean_in(&config, &tmp, dry_run),
                Err(Error::BrokerBusy { lock: held }) if held == lock
            ));
        }
        assert!(matches!(take_lock(&config), Err(Error::BrokerBusy { .. })));
        assert!(ran.exists(), "nothing removed while one serves");
        drop(serving);
        assert!(clean_in(&config, &tmp, false).is_ok());
        assert!(!ran.exists());
        // A lock that can't be taken is an error, not a pass.
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        assert!(matches!(take_lock(&config), Err(Error::Sweep { .. })));
    }

    #[test]
    fn a_state_directory_inside_a_leftover_or_behind_a_symlink_survives() {
        let dir = scratch("sweep-state");
        let mut config = config_in(&dir.0);
        let (tmp, runs) = (dir.0.join("tmp"), config.scheduler.run_root());
        // Memory configured inside a run's directory: kept, with the run
        // directory above it; the rest of the run goes.
        let ran = runs.join(run("b2"));
        let memory = ran.join("memory");
        std::fs::create_dir_all(&memory).unwrap();
        std::fs::write(memory.join("item"), b"m").unwrap();
        std::fs::write(ran.join("scratch"), b"s").unwrap();
        config.memory.root = memory.clone();
        // A symlink to the vault, planted in another run's directory.
        std::fs::create_dir_all(&config.vault.root).unwrap();
        std::fs::write(config.vault.root.join("secret"), b"v").unwrap();
        let other = runs.join(run("c3"));
        std::fs::create_dir(&other).unwrap();
        symlink(&config.vault.root, other.join("vault")).unwrap();
        let lines = sweep_in(&config, None, &tmp);
        assert_eq!(std::fs::read(memory.join("item")).unwrap(), b"m");
        assert!(!ran.join("scratch").exists());
        assert_eq!(
            std::fs::read(config.vault.root.join("secret")).unwrap(),
            b"v"
        );
        assert!(!other.exists());
        assert_eq!(
            lines,
            [
                format!(
                    "removed {} in part (kept a state directory, a mount, or what couldn't go)",
                    ran.display()
                ),
                format!("removed {}", other.display()),
                String::from("2 of 2 leftover(s) removed"),
            ]
        );
        // Every configured state directory is kept, the redaction one too.
        let redaction = RedactionConfig {
            enabled: true,
            dir: dir.0.join("red"),
            keep_raw: false,
            timeout: Duration::from_secs(1),
            builtins: crate::output_gate::Builtins::ALL,
        };
        config.artifacts.workspace = Some(dir.0.join("workspace"));
        let kept = state_dirs(&config, Some(&redaction));
        for expected in [
            &config.vault.root,
            &memory,
            &config.scheduler.root,
            &redaction.dir,
            &dir.0.join("workspace"),
        ] {
            assert!(kept.contains(expected), "{expected:?} in {kept:?}");
        }
        assert!(kept.iter().all(|dir| !dir.as_os_str().is_empty()));
        // One gone before it could be removed is left, and said so.
        let vanished = Leftover {
            root: runs,
            name: OsString::from(run("d4")),
        };
        let lines = remove(&[vanished], &[]);
        assert!(lines.first().is_some_and(|line| line.starts_with("left ")));
        assert_eq!(lines.last().unwrap(), "0 of 1 leftover(s) removed");
    }
}
