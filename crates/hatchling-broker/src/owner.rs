//! The broker's owner (WO-017, step 3): the one user its owner-facing
//! sockets admit, recorded at its start.
//!
//! `HATCHLING_OWNER_UID` names it. Otherwise a broker started as root by
//! `sudo` is owned by whoever ran `sudo` (`SUDO_UID`), and any other by its
//! own user. The API, UI, operator, device and ingress sockets, and the
//! UI and operator tokens, are made for that user and given to it, and
//! the sockets admit it alone.

use std::sync::OnceLock;

use crate::Error;

/// The setting that names the owner.
const OWNER_UID: &str = crate::environment::HATCHLING_OWNER_UID.name;

/// Who ran `sudo`, as `sudo` sets it.
const SUDO_UID: &str = crate::environment::SUDO_UID.name;

/// The owner, once worked out.
static OWNER: OnceLock<u32> = OnceLock::new();

/// The owner's uid (see the module docs), worked out once.
///
/// # Errors
///
/// [`Error::InvalidConfig`] when `HATCHLING_OWNER_UID`, or a root
/// broker's `SUDO_UID`, isn't a uid.
pub fn owner() -> Result<u32, Error> {
    if let Some(owner) = OWNER.get() {
        return Ok(*owner);
    }
    let found = owner_from(
        &|key| crate::cell::env_value(std::env::var(key)),
        hatchling_sys::uid(),
        hatchling_sys::euid(),
    )?;
    Ok(*OWNER.get_or_init(|| found))
}

/// The owner as recorded, or, before it is, this process's own user.
pub(crate) fn recorded() -> u32 {
    OWNER.get().copied().unwrap_or_else(hatchling_sys::euid)
}

/// The owner `get` names, for a process whose real uid is `uid` and
/// effective uid `euid`.
///
/// # Errors
///
/// As [`owner`].
fn owner_from(get: &impl Fn(&str) -> Option<String>, uid: u32, euid: u32) -> Result<u32, Error> {
    let parsed = |key: &'static str, raw: String| {
        raw.parse::<u32>().map_err(|_| Error::InvalidConfig {
            key,
            reason: String::from("expected a uid as a base-10 u32"),
        })
    };
    if let Some(raw) = get(OWNER_UID) {
        return parsed("HATCHLING_OWNER_UID", raw);
    }
    if euid == 0 {
        if let Some(raw) = get(SUDO_UID) {
            return parsed("SUDO_UID", raw);
        }
    }
    Ok(uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner is the setting's, else a root broker's sudo user's, else
    /// this process's real user; a value that isn't a uid is refused.
    #[test]
    fn the_owner_is_named_or_ran_sudo_or_is_this_user() {
        let with = |pairs: &[(&str, &str)]| {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value.clone())
            }
        };
        let named = with(&[("HATCHLING_OWNER_UID", "1234"), ("SUDO_UID", "1000")]);
        assert_eq!(owner_from(&named, 0, 0).unwrap(), 1234);
        let sudo = with(&[("SUDO_UID", "1000")]);
        assert_eq!(owner_from(&sudo, 0, 0).unwrap(), 1000);
        assert_eq!(
            owner_from(&sudo, 1001, 1001).unwrap(),
            1001,
            "not root: no sudo user"
        );
        assert_eq!(owner_from(&with(&[]), 0, 0).unwrap(), 0);
        assert_eq!(owner_from(&with(&[]), 1001, 1001).unwrap(), 1001);
        for bad in [
            with(&[("HATCHLING_OWNER_UID", "ocean")]),
            with(&[("SUDO_UID", "-1")]),
        ] {
            assert!(matches!(
                owner_from(&bad, 0, 0),
                Err(Error::InvalidConfig { .. })
            ));
        }
        // Recorded once: what is recorded is what is read back.
        let recorded_first = recorded();
        let owner = owner().unwrap();
        assert_eq!(recorded(), owner);
        assert!(recorded_first == owner || recorded_first == hatchling_sys::euid());
    }
}
