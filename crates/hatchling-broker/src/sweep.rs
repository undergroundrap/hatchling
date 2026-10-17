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
    tracing::info!(removed, found = found.len(), "swept the leftovers of earlier runs");
    lines.push(format!("{removed} of {} leftover(s) removed", found.len()));
    lines
}

/// The startup sweep: a serving broker, holding its lock, removes the
/// leftovers of earlier brokers. A failure is logged, never fatal.
pub fn sweep_at_start(config: &Config, redaction: Option<&RedactionConfig>) {
    let found = leftovers(&std::env::temp_dir(), run_root(config).as_deref());
    let _ = remove(&found, &state_dirs(config, redaction));
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
    let _lock = take_lock(config)?;
    let found = leftovers(&std::env::temp_dir(), run_root(config).as_deref());
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
