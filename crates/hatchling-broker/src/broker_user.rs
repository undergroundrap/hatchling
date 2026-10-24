//! The broker's own user (WO-017, step 3).
//!
//! Run as root, the broker's sealed stores (builds, spaces, schedules,
//! the web stores and the output gate's records) belong to
//! `hatchling-broker`, a user of their own, so that a broker that stops
//! being root still opens them. Until it does, the root broker, and an
//! operator's command run as root, act for that user in them
//! ([`hatchling_vault::act_for`]): opened as its, and what is made there
//! given to it.

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
/// Each sealed store's root but the vault, which is the keeper's, and the
/// output gate's directory, which holds the redaction store. (The
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
/// for it to guard, handed to that user first. Not run as root, it
/// changes nothing.
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
    act_for_broker_in(&broker_dirs(config, redaction.as_ref()), uid, gid)
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
