//! The broker's own user (WO-017, step 3).
//!
//! Run as root, the broker becomes `hatchling-broker`, a user of its own,
//! once everything that needs root is done ([`drop_to_broker`]). What it
//! keeps is that user's: its sealed stores (builds, spaces, schedules,
//! the web stores and the output gate's records), memory, goals, the
//! scheduler's run directories and its activity log. Until it drops, the
//! root broker, and an operator's command run as root, act for that user
//! in the sealed stores ([`hatchling_vault::act_for`]): opened as its,
//! and what is made there given to it.

use std::path::{Path, PathBuf};

use crate::{Config, Error, RedactionConfig};

/// The broker's own user, run as root.
pub const BROKER_USER: &str = "hatchling-broker";

/// The broker's user, its uid and gid, when this process runs as root;
/// none otherwise, when its stores are this process's user's.
///
/// # Errors
///
/// [`Error::BrokerUser`] when this process runs as root and there's no
/// such user (saying how to make one), or it is root itself.
pub fn broker_user() -> Result<Option<(u32, u32)>, Error> {
    crate::keeper::service_user(BROKER_USER, "its stores")
        .map_err(|reason| Error::BrokerUser { reason })
}

/// The directories the broker's user owns.
///
/// Each sealed store's root but the vault and the sign-ins, which are the
/// keeper's, and the output gate's directory, which holds the redaction store. (The
/// configuration keeps the stores apart, so none is named twice.)
#[must_use]
pub fn broker_dirs(config: &Config, redaction: Option<&RedactionConfig>) -> Vec<PathBuf> {
    redaction
        .map(|redaction| redaction.dir.clone())
        .into_iter()
        .chain(
            crate::vault_ops::broker_stores(config, redaction)
                .into_iter()
                .map(|(_, root)| root),
        )
        .collect()
}

/// Lets this process, run as root, act for the broker's user in its
/// directories.
///
/// They are [`broker_dirs`], the output gate's when there is an ingress
/// for it to guard, handed to that user first, with the rest of its
/// state ([`state_dirs`], [`state_files`]). Not run as root, it changes
/// nothing.
///
/// # Errors
///
/// As [`broker_user`] and [`RedactionConfig::from_env`], and
/// [`Error::BrokerUser`] when the directories can't be handed over or
/// the acting is refused.
pub fn act_for_broker(config: &Config) -> Result<(), Error> {
    let Some((uid, gid)) = broker_user()? else {
        return Ok(());
    };
    let redaction = config
        .channel_ingress
        .as_ref()
        .map(|_| RedactionConfig::from_env(config))
        .transpose()?;
    act_for_broker_in(&broker_dirs(config, redaction.as_ref()), uid, gid)?;
    hand_state(config, uid, gid)
}

/// The broker's state that isn't a sealed store, and is a directory:
/// memory's and goals' roots, and the scheduler's run directories, each
/// when it is on.
#[must_use]
pub fn state_dirs(config: &Config) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [&config.memory.root, &config.goals.root]
        .into_iter()
        .filter(|root| !root.as_os_str().is_empty())
        .cloned()
        .collect();
    if config.scheduler.enabled() {
        dirs.push(config.scheduler.run_root());
    }
    dirs
}

/// The broker's state that is a file: the scheduler's activity log, when
/// it is on. It sits beside the store, where the broker's user may not
/// make it.
#[must_use]
pub fn state_files(config: &Config) -> Vec<PathBuf> {
    config
        .scheduler
        .enabled()
        .then(|| config.scheduler.activity_log())
        .into_iter()
        .collect()
}

/// Hands the broker's state to its user, `uid` and `gid`: its
/// directories as its stores are, and each of its files made if absent
/// (empty, 0600), never through a link, and given to the user.
///
/// # Errors
///
/// [`Error::BrokerUser`] for one that can't be handed over.
fn hand_state(config: &Config, uid: u32, gid: u32) -> Result<(), Error> {
    let failed = |reason: String| Error::BrokerUser { reason };
    crate::keeper::hand_to(BROKER_USER, &state_dirs(config), uid, gid).map_err(failed)?;
    for file in state_files(config) {
        hatchling_sys::hand_file_to(&file, uid).map_err(|e| {
            failed(format!(
                "cannot hand {} to {BROKER_USER}: {e}",
                file.display()
            ))
        })?;
    }
    Ok(())
}

/// Run as root, makes this process the broker's user, for good
/// ([`hatchling_sys::drop_to`]), and says so. Not run as root, it changes
/// nothing. Whether it dropped.
///
/// Call it once everything that needs root is done, while the process
/// has one thread.
///
/// # Errors
///
/// As [`broker_user`], and [`Error::BrokerUser`] when the drop fails.
pub fn drop_to_broker() -> Result<bool, Error> {
    let Some((uid, gid)) = broker_user()? else {
        return Ok(false);
    };
    hatchling_sys::drop_to(uid, gid).map_err(|e| Error::BrokerUser {
        reason: format!("cannot become {BROKER_USER}: {e}"),
    })?;
    tracing::info!(
        euid = hatchling_sys::euid(),
        user = BROKER_USER,
        "the broker runs as its own user"
    );
    Ok(true)
}

/// [`act_for_broker`] in `dirs`, for `uid` and `gid`.
///
/// # Errors
///
/// As [`act_for_broker`].
fn act_for_broker_in(dirs: &[PathBuf], uid: u32, gid: u32) -> Result<(), Error> {
    let failed = |reason: String| Error::BrokerUser { reason };
    crate::keeper::hand_to(BROKER_USER, dirs, uid, gid).map_err(failed)?;
    let dirs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    hatchling_vault::act_for(uid, &dirs)
        .map_err(|e| failed(format!("cannot act for {BROKER_USER}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The broker's directories: each store that is on, but the vault,
    /// and the output gate's directory with its redaction store in it.
    #[test]
    fn the_brokers_directories_are_its_stores_and_the_gates() {
        let mut config = crate::api_auth::tests::owner_config(None);
        assert!(broker_dirs(&config, None).is_empty(), "no store is on");
        config.artifacts.root = PathBuf::from("/srv/h/artifacts");
        config.spaces.root = PathBuf::from("/srv/h/spaces");
        config.web.grants = PathBuf::from("/srv/h/sites");
        let redaction = RedactionConfig::parse(&config, |key| {
            (key == "HATCHLING_REDACTION_DIR").then(|| String::from("/srv/h/gate"))
        })
        .unwrap();
        assert_eq!(
            broker_dirs(&config, Some(&redaction)),
            [
                PathBuf::from("/srv/h/gate"),
                PathBuf::from("/srv/h/artifacts"),
                PathBuf::from("/srv/h/spaces"),
                PathBuf::from("/srv/h/sites"),
                PathBuf::from("/srv/h/gate").join(crate::output_gate::RAW_DIR),
            ]
        );
        assert!(
            !broker_dirs(&config, Some(&redaction)).contains(&config.vault.root),
            "the vault is the keeper's"
        );
        config.web.credentials = PathBuf::from("/srv/h/sign-ins");
        assert!(
            !broker_dirs(&config, Some(&redaction)).contains(&config.web.credentials),
            "the sign-ins are the keeper's"
        );
    }

    /// The broker's other state: memory's, goals' and the scheduler's run
    /// directories, and its activity log, each when it is on.
    #[test]
    fn the_brokers_state_is_memory_goals_and_its_runs() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.scheduler.root = PathBuf::new();
        assert!(state_dirs(&config).is_empty() && state_files(&config).is_empty());
        config.memory.root = PathBuf::from("/srv/h/memory");
        config.goals.root = PathBuf::from("/srv/h/goals");
        config.scheduler.root = PathBuf::from("/srv/h/schedules");
        assert_eq!(
            state_dirs(&config),
            [
                PathBuf::from("/srv/h/memory"),
                PathBuf::from("/srv/h/goals"),
                PathBuf::from("/srv/h/schedules.runs"),
            ]
        );
        assert_eq!(
            state_files(&config),
            [PathBuf::from("/srv/h/schedules.activity")]
        );
    }

    /// The broker's state is handed to its user: its directories made if
    /// absent, its activity log made empty if absent, kept if present,
    /// never through a link. Given to another user only as root.
    #[test]
    fn the_brokers_state_is_handed_to_its_user() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let base =
            std::env::temp_dir().join(format!("hatchling-broker-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o711)).unwrap();
        let mut config = crate::api_auth::tests::owner_config(None);
        config.memory.root = base.join("memory");
        config.goals.root = PathBuf::new();
        config.scheduler.root = base.join("schedules");
        let me = (hatchling_sys::euid(), hatchling_sys::egid());
        hand_state(&config, me.0, me.1).unwrap();
        let meta = |path: &Path| std::fs::symlink_metadata(path).unwrap();
        assert!(meta(&base.join("memory")).is_dir());
        assert!(meta(&base.join("schedules.runs")).is_dir());
        let log = base.join("schedules.activity");
        assert_eq!((meta(&log).len(), meta(&log).mode() & 0o777), (0, 0o600));
        std::fs::write(&log, b"kept").unwrap();
        hand_state(&config, me.0, me.1).unwrap();
        assert_eq!(std::fs::read(&log).unwrap(), b"kept");
        // Never through a link.
        std::fs::remove_file(&log).unwrap();
        std::os::unix::fs::symlink(base.join("elsewhere"), &log).unwrap();
        assert!(hand_state(&config, me.0, me.1).is_err());
        assert!(!base.join("elsewhere").exists());
        std::fs::remove_file(&log).unwrap();
        if me.0 == 0 {
            hand_state(&config, 4343, 4343).unwrap();
            for handed in ["memory", "schedules.runs", "schedules.activity"] {
                assert_eq!(meta(&base.join(handed)).uid(), 4343, "{handed}");
            }
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Not run as root, there's no one to drop to, and nothing changes.
    #[test]
    fn not_as_root_the_broker_drops_to_no_one() {
        if hatchling_sys::euid() != 0 {
            let before = hatchling_sys::euid();
            assert!(!drop_to_broker().unwrap());
            assert_eq!(hatchling_sys::euid(), before);
        }
    }

    /// The broker's user is looked for only as root, where its absence
    /// stops the broker, saying how to make it; not as root, its stores
    /// are this process's user's, and nothing acts for anyone.
    #[test]
    fn the_brokers_user_is_root_modes_alone() {
        let config = crate::api_auth::tests::owner_config(None);
        if hatchling_sys::euid() != 0 {
            assert_eq!(broker_user().unwrap(), None);
            act_for_broker(&config).unwrap();
            return;
        }
        match hatchling_sys::subid::local_account(BROKER_USER).unwrap() {
            None => {
                let refused = broker_user().unwrap_err().to_string();
                assert!(refused.contains("useradd --system"), "{refused}");
                assert!(refused.contains(BROKER_USER), "{refused}");
                assert!(act_for_broker(&config).is_err());
            }
            Some(ids) => assert_eq!(broker_user().unwrap(), Some(ids)),
        }
    }

    /// Run as root, a store handed to the broker's user is that user's,
    /// with what is in it; root then opens it as that user's, and what it
    /// makes there is given to that user. Not as root, nothing can act.
    #[test]
    fn a_store_handed_to_the_brokers_user_is_opened_as_its() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let base =
            std::env::temp_dir().join(format!("hatchling-broker-user-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o711)).unwrap();
        let store = base.join("artifacts");
        let (user, group) = (4343, 4343);
        if hatchling_sys::euid() != 0 {
            assert!(act_for_broker_in(std::slice::from_ref(&store), user, group).is_err());
            std::fs::remove_dir_all(&base).unwrap();
            return;
        }
        std::fs::create_dir(&store).unwrap();
        // A store's root, as a store makes it: its owner's alone.
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(store.join("left"), b"x").unwrap();
        act_for_broker_in(std::slice::from_ref(&store), user, group).unwrap();
        let uid = |path: &Path| std::fs::symlink_metadata(path).unwrap().uid();
        assert_eq!((uid(&store), uid(&store.join("left"))), (user, user));
        let name: hatchling_core::SecretName = "build".parse().unwrap();
        hatchling_vault::Vault::open(&store)
            .unwrap()
            .store(&name, &hatchling_vault::Secret::new(b"b".to_vec()).unwrap())
            .unwrap();
        assert_eq!(uid(&store.join("build")), user);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
