//! When each security key last opened the vault, so a spare that's sat in a
//! drawer is found out while the other key still works.
//!
//! The risk is a key that has quietly stopped working, or gone missing: a
//! spare nobody tries is only found dead on the day it's needed. The record
//! sits beside the keyring, for the keyring's owner and root, and opens
//! nothing: a wrong or missing one only changes a reminder, and failing to
//! write it never stops an unlock.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hatchling_vault::{KeyInfo, Keyring};

use crate::vault_keyring::{KeyringConfig, utc_date};

/// How long a key may go without opening the vault before it's mentioned.
const QUIET_AFTER: u64 = 90 * 24 * 60 * 60;

/// Where the record of the keyring at `keyring` is.
#[must_use]
pub fn record(keyring: &Path) -> PathBuf {
    let mut name = keyring.as_os_str().to_owned();
    name.push(".seen");
    PathBuf::from(name)
}

/// The marker that says reminders are off. Its absence says they're on.
fn off_marker(keyring: &Path) -> PathBuf {
    let mut name = keyring.as_os_str().to_owned();
    name.push(".no-reminders");
    PathBuf::from(name)
}

/// Whether reminders are on for the keyring at `keyring`.
#[must_use]
pub fn reminders_on(keyring: &Path) -> bool {
    !off_marker(keyring).exists()
}

/// Turns reminders on or off. Keys go on being noted either way, so turning
/// them back on tells the truth at once.
///
/// # Errors
///
/// Why the marker couldn't be written or removed.
pub fn set_reminders(keyring: &Path, on: bool) -> std::io::Result<()> {
    let marker = off_marker(keyring);
    if !on {
        // As root, beside a keyring its service user owns: never through
        // a link planted at the name.
        return hatchling_sys::hand_file_to(&marker, hatchling_sys::euid())
            .map_err(std::io::Error::other);
    }
    match std::fs::remove_file(marker) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Forgets everything kept for the keyring at `keyring`.
pub fn erase(keyring: &Path) {
    let _ = std::fs::remove_file(record(keyring));
    let _ = std::fs::remove_file(off_marker(keyring));
}

/// When each key was last seen, by its wrap id. A line that can't be read
/// is a key not seen.
fn read(keyring: &Path) -> BTreeMap<String, u64> {
    std::fs::read_to_string(record(keyring))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (id, at) = line.rsplit_once(' ')?;
            Some((id.to_owned(), at.parse().ok()?))
        })
        .collect()
}

/// Records that the key with wrap id `id` opened the vault at `now`.
///
/// # Errors
///
/// Why the record couldn't be written.
pub(crate) fn note(keyring: &Path, id: &str, now: u64) -> std::io::Result<()> {
    let mut seen = read(keyring);
    seen.insert(id.to_owned(), now);
    let lines: Vec<String> = seen.iter().map(|(id, at)| format!("{id} {at}")).collect();
    hatchling_sys::write_file(&record(keyring), lines.join("\n").as_bytes())
        .map_err(std::io::Error::other)
}

/// What to say of `key`, if it's gone quiet. It's quiet from when it was
/// last seen, or added if it never was. With neither on record, nothing
/// shows it has ever worked.
fn quiet(key: &KeyInfo, seen: &BTreeMap<String, u64>, now: u64) -> Option<String> {
    let last = seen.get(&key.id).copied();
    if last
        .or(key.added)
        .is_some_and(|at| now.saturating_sub(at) <= QUIET_AFTER)
    {
        return None;
    }
    let name = if key.label.is_empty() {
        format!("key {}", key.id)
    } else {
        format!("key \"{}\"", key.label)
    };
    let when = match (last.and_then(utc_date), key.added.and_then(utc_date)) {
        (Some(date), _) => format!("last opened the vault on {date}"),
        (None, Some(date)) => format!("was added on {date} and hasn't opened the vault since"),
        (None, None) => String::from("hasn't opened the vault lately"),
    };
    Some(format!(
        "Your {name} {when}. Use it for your next unlock, so you know it still works. (To stop these reminders: sudo hatchling-broker vault-keyring reminders-off)"
    ))
}

/// A line for each of `keys` that's gone quiet. A lone key opens the vault
/// every time, so only with two or more is there a spare to forget.
fn quiet_keys(keys: &[KeyInfo], seen: &BTreeMap<String, u64>, now: u64) -> Vec<String> {
    if keys.len() < 2 {
        return Vec::new();
    }
    keys.iter()
        .filter_map(|key| quiet(key, seen, now))
        .collect()
}

/// A line for each enrolled key that's gone quiet, at `now`.
#[must_use]
pub fn reminders(config: &KeyringConfig, now: u64) -> Vec<String> {
    let Some(path) = config.keyring.as_deref() else {
        return Vec::new();
    };
    if !reminders_on(path) {
        return Vec::new();
    }
    let Ok(keyring) = Keyring::load(path) else {
        return Vec::new();
    };
    quiet_keys(&keyring.hardware_keys(), &read(path), now)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;
    /// Far enough from the epoch for dates to read plainly.
    const NOW: u64 = 1_800_000_000;

    fn key(label: &str, added: Option<u64>) -> KeyInfo {
        KeyInfo {
            id: String::from("wrap-1"),
            label: label.to_owned(),
            added,
            check: hatchling_vault::KeyCheck::Pin,
        }
    }

    fn seen_at(at: u64) -> BTreeMap<String, u64> {
        BTreeMap::from([(String::from("wrap-1"), at)])
    }

    /// Reminders go off and on by the marker beside the keyring, and a
    /// link planted at the marker's name, or the record's, isn't followed:
    /// what it points at is untouched.
    #[test]
    fn the_files_beside_a_keyring_are_never_reached_through_a_link() {
        let dir = std::env::temp_dir().join(format!("hatchling-seen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let keyring = dir.join("keyring");
        assert!(reminders_on(&keyring));
        set_reminders(&keyring, false).unwrap();
        assert!(!reminders_on(&keyring));
        set_reminders(&keyring, false).unwrap();
        assert!(!reminders_on(&keyring), "off twice is off");
        set_reminders(&keyring, true).unwrap();
        assert!(reminders_on(&keyring));
        set_reminders(&keyring, true).unwrap();
        note(&keyring, "wrap-1", 7).unwrap();
        note(&keyring, "wrap-2", 8).unwrap();
        assert_eq!(read(&keyring).get("wrap-1"), Some(&7));
        erase(&keyring);

        let target = dir.join("target");
        std::fs::write(&target, b"untouched").unwrap();
        std::os::unix::fs::symlink(&target, off_marker(&keyring)).unwrap();
        std::os::unix::fs::symlink(&target, record(&keyring)).unwrap();
        assert!(set_reminders(&keyring, false).is_err());
        assert!(note(&keyring, "wrap-1", 7).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_key_goes_quiet_after_ninety_days_and_not_a_moment_before() {
        let spare = key("Nano", Some(NOW - 400 * DAY));
        let on_the_day = seen_at(NOW - 90 * DAY);
        assert_eq!(quiet(&spare, &on_the_day, NOW), None);
        let past_it = seen_at(NOW - 90 * DAY - 1);
        let said = quiet(&spare, &past_it, NOW).unwrap();
        assert!(said.starts_with("Your key \"Nano\" last opened the vault on "));
        assert!(said.contains("Use it for your next unlock, so you know it still works."));
        assert!(said.ends_with("vault-keyring reminders-off)"));
    }

    #[test]
    fn a_key_never_seen_counts_from_when_it_was_added() {
        let none = BTreeMap::new();
        assert_eq!(quiet(&key("Nano", Some(NOW - DAY)), &none, NOW), None);
        let said = quiet(&key("Nano", Some(NOW - 91 * DAY)), &none, NOW).unwrap();
        assert!(said.contains("was added on ") && said.contains("hasn't opened the vault since"));
        // Nothing on record means nothing shows it works.
        let said = quiet(&key("", None), &none, NOW).unwrap();
        assert!(said.starts_with("Your key wrap-1 hasn't opened the vault lately."));
    }

    #[test]
    fn a_lone_key_has_no_spare_to_forget() {
        let old = key("Nano", None);
        let none = BTreeMap::new();
        assert!(quiet_keys(std::slice::from_ref(&old), &none, NOW).is_empty());
        assert_eq!(quiet_keys(&[old.clone(), old], &none, NOW).len(), 2);
    }

    #[test]
    fn a_clock_set_back_mentions_nothing() {
        assert_eq!(quiet(&key("Nano", None), &seen_at(NOW + DAY), NOW), None);
    }
}
