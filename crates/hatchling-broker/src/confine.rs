//! The broker's own file access, confined (WO-017, slice 4).
//!
//! Once its start is done, the broker holds itself, with Landlock, to
//! what it keeps and what it reads: its stores and state, its run
//! directories and socket directory to read and write; the system's
//! paths, its skills, its trust roots and the browser bundle to read;
//! nothing to run, since every program a cell needs is started by the
//! spawner. Without root the broker runs as its owner: a compromised
//! broker would otherwise read the whole of the owner's home. Root's
//! broker, run as `hatchling-broker`, is held the same way.
//!
//! A kernel without Landlock (before 5.13, or with it off) leaves the
//! broker unconfined, and the broker says so.

use std::path::{Path, PathBuf};

use hatchling_sys::landlock::{Access, Confinement};

use crate::browser_session::BrowserSettings;
use crate::harness::Harnesses;
use crate::{Config, Error, RedactionConfig};

/// The system's paths a broker reads: its configuration, libraries,
/// zone data, name service and resolver files, and its own process's.
const SYSTEM_READ: [&str; 8] = ["/etc", "/usr", "/lib", "/lib64", "/proc", "/sys", "/run", "/dev/urandom"];

/// The paths the broker reads and writes: its stores and state (each
/// made first, if absent: a rule needs a path), its run directories'
/// temp directory, its socket directory, and `/dev/null`.
fn writable(config: &Config, redaction: Option<&RedactionConfig>) -> Vec<PathBuf> {
    let mut paths = crate::broker_user::broker_dirs(config, redaction);
    paths.extend(crate::broker_user::state_dirs(config));
    paths.extend(crate::broker_user::state_files(config));
    paths.push(std::env::temp_dir());
    paths.extend(config.socket.as_path().parent().map(Path::to_path_buf));
    paths.push(PathBuf::from("/dev/null"));
    paths
}

/// The paths the broker only reads, besides the system's: its skills,
/// their trust roots, the harnesses' project roots it copies a task's
/// files from, and the browser bundle it snapshots, its fonts and its
/// controller.
///
/// # Errors
///
/// As [`Harnesses::parse`] and [`BrowserSettings::parse`].
fn readable(config: &Config, get: impl Fn(&str) -> Option<String>) -> Result<Vec<PathBuf>, Error> {
    let mut paths: Vec<PathBuf> = SYSTEM_READ.iter().map(PathBuf::from).collect();
    paths.extend(
        [&config.skills.skills_dir, &config.skills.trust_roots_file]
            .into_iter()
            .filter(|path| !path.as_os_str().is_empty())
            .cloned(),
    );
    paths.extend(Harnesses::parse(config, &get)?.roots().map(Path::to_path_buf));
    if let Some(browser) = BrowserSettings::parse(&get)? {
        paths.push(browser.bundle().to_path_buf());
        paths.push(browser.controller().to_path_buf());
        paths.extend(browser.fonts().map(Path::to_path_buf));
    }
    Ok(paths)
}

/// Holds this process, for good, to the broker's paths ([`writable`],
/// [`readable`]), the state among them made first if absent. Call it once
/// its start is done, while it has one thread. A kernel without
/// Landlock leaves it as it was, said in the log. Whether it was held.
///
/// # Errors
///
/// [`Error::BrokerUser`] when a path can't be made or given, or the
/// kernel refuses the confinement it has.
pub fn confine_broker(config: &Config, redaction: Option<&RedactionConfig>) -> Result<bool, Error> {
    let failed = |reason: String| Error::BrokerUser { reason };
    let writable = writable(config, redaction);
    let readable = readable(config, |key| crate::cell::env_value(std::env::var(key)))?;
    let Some(mut rules) = Confinement::new().map_err(|e| failed(e.to_string()))? else {
        tracing::warn!(
            "this kernel has no Landlock: the broker's file access is not confined, \
             and a compromised broker reaches every file its user can"
        );
        return Ok(false);
    };
    for dir in crate::broker_user::broker_dirs(config, redaction)
        .into_iter()
        .chain(crate::broker_user::state_dirs(config))
    {
        if std::fs::symlink_metadata(&dir).is_err() {
            hatchling_sys::create_private_dir(&dir)
                .map_err(|e| failed(format!("cannot make {}: {e}", dir.display())))?;
        }
    }
    for file in crate::broker_user::state_files(config) {
        hatchling_sys::hand_file_to(&file, hatchling_sys::euid())
            .map_err(|e| failed(format!("cannot make {}: {e}", file.display())))?;
    }
    for (paths, access) in [(&writable, Access::ReadWrite), (&readable, Access::Read)] {
        for path in paths {
            rules
                .allow(path, access)
                .map_err(|e| failed(format!("cannot confine to {}: {e}", path.display())))?;
        }
    }
    rules
        .restrict_self()
        .map_err(|e| failed(format!("cannot confine itself: {e}")))?;
    tracing::info!("the broker's file access is confined to what it keeps and reads");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hatchling_sys::{ForkResult, child_exit_code, exit_child, fork};

    /// The broker writes its stores, its state, its temp and socket
    /// directories and `/dev/null`; it reads the system's paths, its
    /// skills, their trust roots and its browser bundle, and nothing else.
    #[test]
    fn the_broker_keeps_and_reads_its_own_paths_alone() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.artifacts.root = PathBuf::from("/srv/h/artifacts");
        config.memory.root = PathBuf::from("/srv/h/memory");
        config.scheduler.root = PathBuf::from("/srv/h/schedules");
        let written = writable(&config, None);
        for path in [
            "/srv/h/artifacts",
            "/srv/h/memory",
            "/srv/h/schedules",
            "/srv/h/schedules.runs",
            "/srv/h/schedules.activity",
            "/dev/null",
        ] {
            assert!(written.contains(&PathBuf::from(path)), "{path}: {written:?}");
        }
        assert!(written.contains(&std::env::temp_dir()));
        assert!(written.contains(&config.socket.as_path().parent().unwrap().to_path_buf()));
        assert!(!written.contains(&config.vault.root), "the vault is the keeper's");
        config.skills.skills_dir = PathBuf::from("/srv/h/skills");
        config.skills.trust_roots_file = PathBuf::from("/etc/hatchling/roots");
        let read = readable(&config, |_| None).unwrap();
        for path in SYSTEM_READ.iter().copied().chain(["/srv/h/skills", "/etc/hatchling/roots"]) {
            assert!(read.contains(&PathBuf::from(path)), "{path}: {read:?}");
        }
        assert!(!read.contains(&config.vault.root));
        config.skills.skills_dir = PathBuf::new();
        assert!(!readable(&config, |_| None).unwrap().contains(&PathBuf::new()));
        // A harness's project roots, which its tasks' files are copied from.
        let dir = crate::api_auth::tests::scratch("confine-roots");
        let (project, pinned) = (dir.0.join("project"), dir.0.join("harness"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&pinned, b"stub harness").unwrap();
        let sha256 = hatchling_skill::CodeHash::of(b"stub harness").to_string();
        let mut config = crate::api_auth::tests::owner_config(None);
        std::fs::create_dir_all(dir.0.join("workspace")).unwrap();
        config.artifacts.workspace = Some(dir.0.join("workspace"));
        let vars = std::collections::HashMap::from([
            ("HATCHLING_HARNESS_CLAUDE_CODE", pinned.display().to_string()),
            ("HATCHLING_HARNESS_CLAUDE_CODE_SHA256", sha256),
            ("HATCHLING_HARNESS_ROOTS", format!("project={}", project.display())),
        ]);
        let read = readable(&config, |key| vars.get(key).cloned()).unwrap();
        assert!(read.contains(&project), "{read:?}");
    }

    /// Confined, the broker writes its stores, makes its state, and can't
    /// read a file of its user's beside them, nor run a program. (In a
    /// child: a confinement is for good.) Without Landlock it says so,
    /// and is left as it was.
    #[test]
    fn a_confined_broker_reaches_its_own_paths_and_nothing_of_its_users() {
        use std::time::Duration;
        let dir = crate::api_auth::tests::scratch("confine");
        let mut config = crate::api_auth::tests::owner_config(None);
        let home = dir.0.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("secret"), b"s").unwrap();
        config.artifacts.root = home.join("state").join("artifacts");
        config.scheduler.root = home.join("state").join("schedules");
        std::fs::create_dir_all(home.join("state")).unwrap();
        // The temp directory is the broker's: the user's file lives apart
        // from it, as a home does.
        let outside = std::env::var_os("HOME").map(PathBuf::from).filter(|home| {
            !home.starts_with(std::env::temp_dir()) && home.is_dir()
        });
        let probe = outside.map(|home| {
            let probe = home.join(format!(".hatchling-confine-probe-{}", std::process::id()));
            std::fs::write(&probe, b"p").unwrap();
            probe
        });
        if hatchling_sys::landlock::landlock_abi().is_none() {
            assert!(!confine_broker(&config, None).unwrap());
            if let Some(probe) = &probe {
                std::fs::remove_file(probe).unwrap();
            }
            return;
        }
        let in_temp = std::env::temp_dir().join(format!("hatchling-confine-temp-{}", std::process::id()));
        match fork().unwrap() {
            ForkResult::Parent { child } => {
                let status = child.wait_deadline(Duration::from_secs(10)).unwrap();
                let code = child_exit_code(status);
                if let Some(probe) = &probe {
                    std::fs::remove_file(probe).unwrap();
                }
                let _ = std::fs::remove_file(&in_temp);
                assert_eq!(
                    code, 0,
                    "1: not confined; 2: its state wasn't made; 3: can't write its store; \
                     4: read its user's file; 5: ran a program; 6: read beside its state;                      7: can't write its temp directory"
                );
            }
            ForkResult::Child => {
                if !matches!(confine_broker(&config, None), Ok(true)) {
                    exit_child(1);
                }
                let state = home.join("state");
                if !state.join("schedules.runs").is_dir() || !state.join("schedules.activity").is_file() {
                    exit_child(2);
                }
                if std::fs::write(config.artifacts.root.join("build"), b"b").is_err() {
                    exit_child(3);
                }
                if probe.as_ref().is_some_and(|probe| std::fs::read(probe).is_ok()) {
                    exit_child(4);
                }
                if std::process::Command::new("/bin/true").status().is_ok() {
                    exit_child(5);
                }
                if std::fs::read(home.join("secret")).is_ok() && !home.starts_with(std::env::temp_dir()) {
                    exit_child(6);
                }
                if std::fs::write(&in_temp, b"t").is_err() {
                    exit_child(7);
                }
                exit_child(0);
            }
        }
    }
}
