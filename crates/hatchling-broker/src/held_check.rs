//! Whether a text holds a secret Hatchling keeps.
//!
//! It's asked of what a harness is given and hands back, and of what the
//! agent sends off the machine: a web read's address, a search, a page's
//! address, and text typed into a page.
//!
//! All of that goes to someone else, so the check covers every secret held,
//! whether or not a cell has ever had it: API keys, vendor keys, the search
//! key, channel accounts and sign-in passwords. The broker never holds most
//! of those, so with a keeper running the keeper answers
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
//!
//! **An address is checked as a site reads it** ([`as_read`]): percent
//! escapes hide a value from a plain look, and the site undoes them. So
//! the text is the address as written and as decoded once, in one check.
//!
//! **A hit ends the run** ([`HeldHit`]). A refusal tells the agent that what
//! it sent matched, so a tricked agent could find a weak password by trying
//! it, one refusal at a time. So when the agent's own text is found to hold
//! one, its run is over: every cell in its tree is killed there and then,
//! which takes each cell's channel to the keeper with it, the broker ends
//! the session and refuses everything after it, however the cell behaves,
//! and the owner is told once, in Hatchling's own words: what kind of thing
//! was stopped, never which secret. Each check is in the vault's access log,
//! a hit as found.
//!
//! What a harness hands back counts too. The files it's given are checked
//! clean first, so a secret in its patch, its message, or a file's name is
//! one the run put together, and a brief like "join these two parts" would
//! otherwise be a guess the agent reads the answer to. A project file
//! holding a secret isn't the agent's guess: a harness refuses that, and
//! the run goes on. Neither is a check that couldn't be answered: nothing
//! is delivered, and nothing was found.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use hatchling_artifact::SecretSet;
use hatchling_core::SecretName;
use hatchling_isolation::Supervised;
use hatchling_scheduler::SentTo;
use hatchling_vault::{OpensStores, SealedVault};
use zeroize::Zeroizing;

use crate::Config;
use crate::browser_credentials::Credentials;
use crate::harness::MAX_WORK_BYTES;
use crate::keeper::{Keeper, RELEASED_RECORD};
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

/// Whether a run's tree has ended on a hit, on what, and what a hit kills:
/// one state, shared by a run's session, its children's sessions and what
/// watches them. The first hit stays.
///
/// A hit kills every cell the tree holds there and then, from whatever
/// thread found it: a run's cell can hold a channel to the keeper that no
/// session stands in front of, so ending the session isn't enough, and the
/// next look at the run may be a minute off. A cell held after the hit dies
/// as it's held, so one that started in the same moment can't slip through.
///
/// A kill can't reach anything but that cell. Only a [`Weak`] is kept, so
/// nothing is killed once its run lets go of its helper; and what's killed
/// is that helper through its own pidfd, or through the spawner, which
/// holds one on the same connection, so a pid reused since can't be named.
/// The cell goes with its helper: it's bound to it by a parent-death
/// signal, and everything else in it dies with its pid namespace's init.
#[derive(Debug, Clone, Default)]
pub struct HeldHit(Arc<Mutex<Tree>>);

/// A cell in a tree: its helper, as its run shares it.
type Held = Weak<dyn Supervised + Send + Sync>;

/// A [`HeldHit`]'s state.
#[derive(Debug, Default)]
struct Tree {
    /// Where the first hit was on its way to.
    to: Option<SentTo>,
    /// The tree's cells, while their runs last and until a hit.
    cells: Vec<Held>,
}

/// Kills `cell`, if its run still has it.
fn kill(cell: &Held) {
    if let Some(cell) = cell.upgrade() {
        if let Err(error) = cell.terminate() {
            // The next look at the run kills it again, and a helper whose
            // spawner has gone is ended by its own watch on it.
            tracing::error!(?error, "a stopped run's cell couldn't be killed");
        }
    }
}

impl HeldHit {
    /// This state, whatever a thread that panicked left: a flag and a list
    /// are whole at every step.
    fn tree(&self) -> MutexGuard<'_, Tree> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ends the run: the agent's own text, on its way to `to`, held a
    /// secret. Every cell held is killed before this returns.
    pub fn record(&self, to: SentTo) {
        let cells = {
            let mut tree = self.tree();
            tree.to.get_or_insert(to);
            std::mem::take(&mut tree.cells)
        };
        cells.iter().for_each(kill);
    }

    /// Where the hit was on its way to, once there's been one.
    #[must_use]
    pub fn seen(&self) -> Option<SentTo> {
        self.tree().to
    }

    /// Holds `cell`, a cell of this tree, for a hit to kill: at once if
    /// there's been one.
    pub(crate) fn hold<P: Supervised + Send + Sync + 'static>(&self, cell: &Arc<P>) {
        let weak = Arc::downgrade(cell);
        let cell: Held = weak;
        let mut tree = self.tree();
        if tree.to.is_none() {
            // A tree's runs come and go: what's left is what's alive.
            tree.cells.retain(|held| held.strong_count() > 0);
            tree.cells.push(cell);
        } else {
            drop(tree);
            kill(&cell);
        }
    }
}

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

/// Whether `text` holds a secret: asked of `keeper`, or with none, of the
/// broker itself ([`check_here`]).
///
/// # Errors
///
/// Why nothing is known: the keeper refused it, can't be reached, or the
/// check couldn't be made or recorded. A caller treats that as holding one.
pub(crate) fn check(
    keeper: Option<&Keeper>,
    config: &Config,
    keys: &dyn OpensStores,
    text: &[u8],
) -> Result<bool, String> {
    keeper.map_or_else(
        || check_here(config, keys, text),
        |keeper| keeper.contains(text).map_err(|error| error.to_string()),
    )
}

/// The address `url` as the text to check: as written, then as a site reads
/// it, each on a line of its own. Decoded once, as a site does: `%` escapes
/// everywhere, then with `+` as a space too, as a query is. A form that adds
/// nothing is left out.
pub(crate) fn as_read(url: &str) -> Vec<u8> {
    let written = url.as_bytes();
    let plain = decoded(written, false);
    let query = decoded(written, true);
    let mut text = written.to_vec();
    if plain != written {
        text.push(b'\n');
        text.extend_from_slice(&plain);
    }
    if query != plain {
        text.push(b'\n');
        text.extend_from_slice(&query);
    }
    text
}

/// `text` with its `%` escapes undone, and `+` as a space if `plus`. A `%`
/// that doesn't start an escape stands for itself.
fn decoded(text: &[u8], plus: bool) -> Vec<u8> {
    let digit = |byte: Option<&u8>| byte.and_then(|byte| char::from(*byte).to_digit(16));
    let mut plain = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some((&byte, after)) = rest.split_first() {
        let escaped = (byte == b'%')
            .then(|| Some((digit(after.first())? << 4) | digit(after.get(1))?))
            .flatten()
            .and_then(|value| u8::try_from(value).ok());
        if let Some(value) = escaped {
            plain.push(value);
            rest = after.get(2..).unwrap_or_default();
        } else {
            plain.push(if plus && byte == b'+' { b' ' } else { byte });
            rest = after;
        }
    }
    plain
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
    use crate::cell::Process;

    fn values(list: &[&[u8]]) -> Vec<Zeroizing<Vec<u8>>> {
        list.iter()
            .map(|value| Zeroizing::new(value.to_vec()))
            .collect()
    }

    /// A hit stays, the first one, and every clone in a run's tree sees it.
    #[test]
    #[allow(
        clippy::redundant_clone,
        reason = "clippy 1.85 takes the clone for unused: both ends are, here"
    )]
    fn a_hit_is_seen_across_a_tree_and_the_first_stays() {
        let hit = HeldHit::default();
        let child = hit.clone();
        assert_eq!(hit.seen(), None);
        child.record(SentTo::Search);
        hit.record(SentTo::Page);
        assert_eq!(hit.seen(), Some(SentTo::Search));
        assert_eq!(child.seen(), Some(SentTo::Search));
        assert_eq!(HeldHit::default().seen(), None, "another run's is its own");
    }

    /// `/bin/sh -c script`, supervised as a helper is.
    fn helper(script: &str) -> Arc<Process> {
        let mut started = std::process::Command::new("/bin/sh")
            .args(["-c", script])
            .spawn()
            .unwrap();
        let process = hatchling_isolation::IsolationBackend::supervise(
            &hatchling_isolation::platform(),
            &mut started,
        )
        .unwrap();
        Arc::new(Process::Local(process))
    }

    /// A helper that lives until it's killed, as a cell's does.
    fn lingering() -> Arc<Process> {
        helper("exec sleep 60")
    }

    /// Dead within five seconds, and reaped here. It can only die of a
    /// kill: nothing else ends it for a minute.
    fn dies(cell: &Process) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(5) {
            if cell.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }

    /// A hit kills every cell of the tree there and then, with nothing
    /// looking at the runs, and only that tree's.
    #[test]
    fn a_hit_kills_each_cell_it_holds_at_once() {
        let (hit, other) = (HeldHit::default(), HeldHit::default());
        let (parent, child, elsewhere) = (lingering(), lingering(), lingering());
        hit.hold(&parent);
        hit.hold(&child);
        other.hold(&elsewhere);
        assert_eq!(parent.try_wait().unwrap(), None, "no hit: it runs on");
        hit.record(SentTo::Search);
        assert!(dies(&parent), "the cell that was running");
        assert!(dies(&child), "the rest of its tree");
        assert_eq!(elsewhere.try_wait().unwrap(), None, "another run's");
    }

    /// A cell started as the hit lands can't slip through: held after it,
    /// it dies as it's held.
    #[test]
    fn a_cell_held_after_a_hit_dies_as_it_is_held() {
        let hit = HeldHit::default();
        hit.record(SentTo::Page);
        let late = lingering();
        hit.hold(&late);
        assert!(dies(&late));
    }

    /// A kill reaches only what its run still has: a run that let go of its
    /// cell leaves nothing for a hit to find, and a cell already reaped is
    /// answered by the kernel through its pidfd, whatever its pid names
    /// now. Neither stops the cells after it.
    #[test]
    fn a_hit_reaches_nothing_that_has_ended() {
        let hit = HeldHit::default();
        let gone = lingering();
        hit.hold(&gone);
        drop(gone);
        let reaped = helper("exit 0");
        reaped
            .wait_deadline(std::time::Duration::from_secs(5))
            .unwrap();
        hit.hold(&reaped);
        let last = lingering();
        hit.hold(&last);
        assert_eq!(hit.tree().cells.len(), 2, "what has gone isn't kept");
        hit.record(SentTo::Website);
        assert!(reaped.terminate().is_ok(), "a reaped cell is no error");
        assert!(dies(&last));
    }

    /// An address is the text written, then what a site reads from it. A
    /// `%` that starts no escape stands, and a form that's the same as
    /// another isn't sent twice.
    #[test]
    fn an_address_is_checked_as_written_and_as_read() {
        let lines = |url: &str| {
            String::from_utf8_lossy(&as_read(url))
                .lines()
                .map(str::to_owned)
                .collect::<Vec<String>>()
        };
        assert_eq!(
            lines("https://a.example/plain"),
            ["https://a.example/plain"]
        );
        assert_eq!(
            lines("https://a.example/%73k%2dA"),
            ["https://a.example/%73k%2dA", "https://a.example/sk-A"]
        );
        assert_eq!(
            lines("https://a.example/a+b%2Bc"),
            [
                "https://a.example/a+b%2Bc",
                "https://a.example/a+b+c",
                "https://a.example/a b+c"
            ]
        );
        assert_eq!(
            lines("https://a.example/a+b"),
            ["https://a.example/a+b", "https://a.example/a b"]
        );
        for stands in ["100%", "%", "%4", "%zz", "%g1"] {
            assert_eq!(lines(stands), [stands], "{stands}");
        }
        assert_eq!(as_read("%41%ff"), b"%41%ff\nA\xff");
        // Decoded once: what's escaped twice reads as escaped once.
        assert_eq!(lines("%2541"), ["%2541", "%41"]);
        // What the whole check finds in it.
        let held = values(&[b"correct horse", b"sk-live-0123456789"]);
        assert!(holds(
            &held,
            &as_read("https://a.example/?k=%73k-live-0123456789")
        ));
        assert!(holds(&held, &as_read("https://a.example/?p=correct+horse")));
        assert!(holds(
            &held,
            &as_read("https://a.example/?p=correct%20horse")
        ));
        assert!(!holds(&held, &as_read("https://a.example/?k=sk-live")));
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
