//! When each security key last opened the vault, so a spare that's sat in a
//! drawer is found out while the other key still works.
//!
//! The risk is a key that has quietly stopped working, or gone missing: a
//! spare nobody tries is only found dead on the day it's needed. The record
//! sits beside the keyring, for the keyring's owner and root, and opens
//! nothing: a wrong or missing one only changes a reminder, and failing to
//! write it never stops an unlock.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
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
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(record(keyring))?
        .write_all(lines.join("\n").as_bytes())
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
        "Your {name} {when}. Use it for your next unlock, so you know it still works."
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

    #[test]
    fn a_key_goes_quiet_after_ninety_days_and_not_a_moment_before() {
        let spare = key("Nano", Some(NOW - 400 * DAY));
        let on_the_day = seen_at(NOW - 90 * DAY);
        assert_eq!(quiet(&spare, &on_the_day, NOW), None);
        let past_it = seen_at(NOW - 90 * DAY - 1);
        let said = quiet(&spare, &past_it, NOW).unwrap();
        assert!(said.starts_with("Your key \"Nano\" last opened the vault on "));
        assert!(said.ends_with("Use it for your next unlock, so you know it still works."));
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
