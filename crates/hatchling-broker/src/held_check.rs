//! Whether a text holds a secret Hatchling keeps, for what a harness is
//! given and what it hands back.
//!
//! What a harness is given goes to its vendor, so the check covers every
//! secret held, whether or not a cell has ever had it: API keys, vendor
//! keys, the search key, channel accounts and sign-in passwords. The broker
//! never holds most of those, so with a keeper running the keeper answers
//! ([`crate::keeper::Keeper::contains`]): the text goes in, one boolean
//! comes out, and no value, name or position ever leaves it. A broker with
//! no keeper reads them itself ([`check_here`]), as it reads everything.
//!
//! **What counts as holding one.** A value is looked for whole, never by
//! part. One of at least [`MIN_SCAN_BYTES`] is found as
//! [`hatchling_artifact::SecretSet`] finds it: raw, hex or base64 at any
//! alignment, anywhere in the text. Base64 keeps only the characters the
//! value fixes, so that form can be up to two bytes short of the value at
//! each end, which is no help in guessing one. A shorter value is too
//! short to look for inside anything without matching by chance, and that
//! would tell an asker about it piece by piece, so it matches only when
//! the whole text is exactly that value. A value stored with space around
//! it is looked for with and without it, as a vendor key is used trimmed
//! ([`crate::harness::key_text`]).

use std::collections::BTreeSet;
use std::path::Path;

use hatchling_artifact::SecretSet;
use hatchling_core::SecretName;
use hatchling_vault::{OpensStores, SealedVault};
use zeroize::Zeroizing;

use crate::Config;
use crate::browser_credentials::Credentials;
use crate::harness::MAX_WORK_BYTES;
use crate::keeper::RELEASED_RECORD;
use crate::vault_access::{Caller, Outcome, record_check};

/// The shortest value looked for inside a text. A shorter one matches only
/// a text that is exactly that value.
pub const MIN_SCAN_BYTES: usize = 8;

/// The most one check takes: past the largest patch a run can make, every
/// byte of both trees twice over with a sign before each line, and room
/// for each file's headers.
pub const MAX_CHECK_BYTES: u64 = 40 * 1024 * 1024;

/// A check too long to be a run's fails, and so stops what asked it.
const _: () = assert!(4 * MAX_WORK_BYTES < MAX_CHECK_BYTES);

/// What a check is recorded as being for.
pub const CHECK_PURPOSE: &str = "harness-check";

/// Every secret held under `config`: each one in the vault, whoever it's
/// for, and each sign-in's password. A username isn't one. `keys` must
/// open the secrets and the sign-ins: the keeper's own, or those of a
/// broker with no keeper.
///
/// Each value is wiped when dropped.
///
/// # Errors
///
/// Why they couldn't all be read.
pub(crate) fn held_values(
    config: &Config,
    keys: &dyn OpensStores,
) -> Result<Vec<Zeroizing<Vec<u8>>>, String> {
    let root = config.vault.root.as_path();
    let unread = |error: &dyn std::fmt::Display| format!("the vault can't be read: {error}");
    let names: Vec<SecretName> = crate::vault_access::stored_names(root)
        .map_err(|error| unread(&error))?
        .into_iter()
        .filter(|name| name != RELEASED_RECORD)
        .filter_map(|name| name.parse().ok())
        .collect();
    let mut values = Vec::with_capacity(names.len());
    if !names.is_empty() {
        let vault = SealedVault::open(root, keys).map_err(|error| unread(&error))?;
        for name in &names {
            match vault.fetch(name) {
                Ok(secret) => values.push(Zeroizing::new(secret.as_bytes().to_vec())),
                // Removed since it was listed: no longer held.
                Err(hatchling_vault::Error::NotFound { .. }) => {}
                Err(error) => return Err(unread(&error)),
            }
        }
    }
    if let Some(sign_ins) = Credentials::through(None, config, keys) {
        for kept in sign_ins.list()? {
            let sign_in = sign_ins.sign_in(&kept.id)?;
            values.push(Zeroizing::new(
                sign_in.password.expose().as_bytes().to_vec(),
            ));
        }
    }
    Ok(values)
}

/// Whether `text` holds one of `values`, by the rules above: one answer for
/// all of them.
pub(crate) fn holds(values: &[Zeroizing<Vec<u8>>], text: &[u8]) -> bool {
    let forms: BTreeSet<&[u8]> = values
        .iter()
        .flat_map(|value| [value.as_slice(), value.trim_ascii()])
        .filter(|form| !form.is_empty())
        .collect();
    let whole = forms.contains(text);
    let long = forms
        .into_iter()
        .filter(|form| form.len() >= MIN_SCAN_BYTES)
        .map(|form| Zeroizing::new(form.to_vec()));
    whole || SecretSet::new(long).exposed_in(text)
}

/// Records one check by `caller` of a text of `size` bytes, then gives its
/// answer. One that can't be recorded isn't given: as with a read, nothing
/// is answered off the record.
///
/// # Errors
///
/// Why the check failed, or couldn't be recorded.
pub(crate) fn on_record(
    root: &Path,
    caller: Caller,
    size: u64,
    answer: Result<bool, String>,
) -> Result<bool, String> {
    let outcome = if answer.is_ok() {
        Outcome::Allowed
    } else {
        Outcome::Failed
    };
    record_check(root, caller, size, answer.as_ref().ok().copied(), outcome)
        .map_err(|error| error.to_string())?;
    answer
}

/// Who a check is recorded as, when the broker asks it.
pub(crate) fn broker() -> Caller {
    Caller::Broker {
        purpose: CHECK_PURPOSE.to_owned(),
    }
}

/// The check, for a broker with no keeper, reading the secrets itself with
/// `keys`, and on the record as the keeper's are.
///
/// # Errors
///
/// As [`held_values`] and [`on_record`].
pub(crate) fn check_here(
    config: &Config,
    keys: &dyn OpensStores,
    text: &[u8],
) -> Result<bool, String> {
    let answer = held_values(config, keys).map(|values| holds(&values, text));
    on_record(
        &config.vault.root,
        broker(),
        u64::try_from(text.len()).unwrap_or(u64::MAX),
        answer,
    )
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    use hatchling_core::ui::Concealed;
    use hatchling_vault::{MasterKey, Secret};

    use super::*;
    use crate::browser_credentials::{SignIn, SignInAnswer, SignInRequest};

    fn values(list: &[&[u8]]) -> Vec<Zeroizing<Vec<u8>>> {
        list.iter()
            .map(|value| Zeroizing::new(value.to_vec()))
            .collect()
    }

    /// A value of the minimum length is found anywhere, and one a byte
    /// shorter only as the whole text: never inside anything, nor encoded.
    #[test]
    fn a_short_secret_matches_only_the_whole_text() {
        let long = b"exactly8";
        assert_eq!(long.len(), MIN_SCAN_BYTES);
        assert!(holds(&values(&[long]), b"say exactly8 here"));
        let short = b"seven-7";
        assert_eq!(short.len(), MIN_SCAN_BYTES - 1);
        let held = values(&[short]);
        assert!(holds(&held, short), "the whole text is the secret");
        assert!(!holds(&held, b"seven-7 "), "trailing space");
        assert!(!holds(&held, b" seven-7"), "leading space");
        assert!(!holds(&held, b"xxseven-7xx"), "inside a text");
        assert!(!holds(&held, b"seven-"), "a prefix");
        assert!(!holds(&held, hex(short).as_bytes()), "hex");
        assert!(
            !holds(&held, STANDARD_NO_PAD.encode(short).as_bytes()),
            "base64"
        );
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut hex, byte| {
            let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
            hex
        })
    }

    /// A long enough value is found raw, as hex in either case, and as
    /// either base64 at any alignment; a part of it isn't.
    #[test]
    fn a_long_secret_is_found_raw_hex_and_base64() {
        let secret = b"sk-live-0123456789";
        let held = values(&[secret]);
        assert!(holds(&held, b"KEY=sk-live-0123456789;"));
        assert!(holds(&held, format!("x{}y", hex(secret)).as_bytes()));
        assert!(holds(&held, hex(secret).to_ascii_uppercase().as_bytes()));
        for lead in ["", "a", "ab"] {
            let stream = [lead.as_bytes(), secret, b"tail"].concat();
            assert!(
                holds(&held, STANDARD_NO_PAD.encode(&stream).as_bytes()),
                "base64 after {lead:?}"
            );
            assert!(
                holds(&held, URL_SAFE_NO_PAD.encode(&stream).as_bytes()),
                "url-safe base64 after {lead:?}"
            );
        }
        assert!(!holds(&held, b"sk-live-012345678"), "all but its last byte");
        assert!(!holds(&held, b"k-live-0123456789"), "all but its first");
        assert!(!holds(&held, b"nothing like it"));
    }

    /// Stored with a newline, a key is still found as it's used: trimmed.
    /// A value of nothing but space looks for nothing, and an empty text
    /// holds nothing.
    #[test]
    fn a_value_is_looked_for_as_it_is_used() {
        let held = values(&[b"sk-live-0123456789\n"]);
        assert!(holds(&held, b"KEY=sk-live-0123456789;"));
        assert!(holds(&held, b"sk-live-0123456789\nKEY"));
        let short = values(&[b" short\n"]);
        assert!(holds(&short, b"short"));
        assert!(holds(&short, b" short\n"));
        let blank = values(&[b" \n"]);
        assert!(!holds(&blank, b""));
        assert!(!holds(&blank, b"anything"));
        assert!(holds(&blank, b" \n"), "the value itself, whole");
        assert!(!holds(&[], b""));
        assert!(!holds(&[], b"anything"));
    }

    /// With no keeper, a broker checks every secret stored and every
    /// sign-in's password, whether any cell has had them, and not the
    /// released record; a sign-in's username isn't a secret. Each check is
    /// on the record: one answer, no name, and its size.
    #[test]
    fn with_no_keeper_the_broker_checks_all_it_holds_on_the_record() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("held-check");
        config.vault.root = dir.0.join("vault");
        config.web.credentials = dir.0.join("sign-ins");
        let key = MasterKey::generate().unwrap();
        let vault = SealedVault::open(&config.vault.root, &key).unwrap();
        let store = |name: &str, value: &[u8]| {
            vault
                .store(
                    &name.parse().unwrap(),
                    &Secret::new(value.to_vec()).unwrap(),
                )
                .unwrap();
        };
        store("egress-api-example-com-443", b"egress-credential");
        store("search-brave", b"search-key-value");
        store(RELEASED_RECORD, br#"["a-released-name"]"#);
        let sign_ins = Credentials::through(None, &config, &key).unwrap();
        let kept = crate::browser_credentials::answer(
            &sign_ins,
            SignInRequest::Keep {
                profile_id: String::from("profile-1"),
                origin: String::from("https://a.example"),
                label: String::from("Work"),
                sign_in: SignIn {
                    username: Concealed::new(String::from("zoe-the-user")),
                    password: Concealed::new(String::from("sign-in-password")),
                },
                now: 1,
            },
        );
        assert!(matches!(kept, SignInAnswer::Kept { .. }), "{kept:?}");
        let held = [
            ("the credential", "egress-credential"),
            ("search's key", "search-key-value"),
            ("the password", "sign-in-password"),
        ];
        for (what, value) in held {
            let text = format!("a {value} b");
            assert_eq!(
                check_here(&config, &key, text.as_bytes()),
                Ok(true),
                "{what}"
            );
        }
        for free in ["zoe-the-user", r#"["a-released-name"]"#, "nothing"] {
            assert_eq!(
                check_here(&config, &key, free.as_bytes()),
                Ok(false),
                "{free}"
            );
        }
        let (accesses, unreadable) = crate::vault_access::read_log(&config.vault.root).unwrap();
        assert_eq!(unreadable, 0);
        assert_eq!(accesses.len(), 6);
        let first = accesses.first().unwrap();
        assert_eq!(first.caller, broker());
        assert_eq!(first.action, crate::vault_access::CHECK_ACTION);
        assert!(first.names.is_empty(), "{first:?}");
        assert_eq!(first.size, Some(21));
        assert_eq!((first.found, first.outcome), (Some(true), Outcome::Allowed));
        let last = accesses.last().unwrap();
        assert_eq!((last.found, last.outcome), (Some(false), Outcome::Allowed));
    }

    /// A vault that can't be read answers no check, on the record, and
    /// neither does a check that can't be recorded. No secrets, and no
    /// vault, is simply none held.
    #[test]
    fn a_check_that_cannot_be_made_or_recorded_answers_nothing() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("held-check-fails");
        config.vault.root = dir.0.join("vault");
        config.web.credentials = std::path::PathBuf::new();
        let key = MasterKey::generate().unwrap();
        assert_eq!(check_here(&config, &key, b"x"), Ok(false));
        SealedVault::open(&config.vault.root, &key)
            .unwrap()
            .store(
                &"token".parse().unwrap(),
                &Secret::new(b"a-held-value".to_vec()).unwrap(),
            )
            .unwrap();
        let wrong = MasterKey::generate().unwrap();
        let failed = check_here(&config, &wrong, b"x").unwrap_err();
        assert!(!failed.contains("a-held-value"), "the error shows a value");
        let (accesses, _) = crate::vault_access::read_log(&config.vault.root).unwrap();
        let [only] = accesses.as_slice() else {
            unreachable!("{accesses:?}");
        };
        assert_eq!((only.found, only.outcome), (None, Outcome::Failed));
        // Here the log itself can't be written: nothing is answered.
        std::fs::create_dir(dir.0.join("vault-log")).unwrap();
        std::fs::create_dir(
            dir.0
                .join("vault-log")
                .join(crate::vault_access::ACCESS_LOG),
        )
        .unwrap();
        assert!(on_record(&dir.0.join("vault-log"), broker(), 1, Ok(true)).is_err());
        assert_eq!(
            on_record(&dir.0.join("vault"), broker(), 1, Ok(true)),
            Ok(true)
        );
    }
}
