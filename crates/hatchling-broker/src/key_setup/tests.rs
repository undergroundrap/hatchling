use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use hatchling_vault::virtual_key::VirtualAuthenticator;

use super::*;
use crate::vault_keyring::tests::{Scratch, adopted, reads_the_secret, secrets};

/// Software keys on a bench: `plugged` says which are in now.
struct Bench {
    keys: Vec<VirtualAuthenticator>,
    plugged: Rc<RefCell<Vec<usize>>>,
}

impl Bench {
    fn of(keys: Vec<VirtualAuthenticator>, plugged: &[usize]) -> Self {
        Self {
            keys,
            plugged: Rc::new(RefCell::new(plugged.to_vec())),
        }
    }

    fn new_keys(count: usize, plugged: &[usize]) -> Self {
        Self::of(
            (0..count).map(|_| VirtualAuthenticator::new()).collect(),
            plugged,
        )
    }
}

impl Devices for Bench {
    fn plugged_in(&mut self) -> Option<&mut dyn Authenticator> {
        let plugged = self.plugged.borrow();
        let only = match plugged.as_slice() {
            [only] => *only,
            _ => return None,
        };
        drop(plugged);
        let key: &mut dyn Authenticator = self.keys.get_mut(only)?;
        Some(key)
    }

    fn every_key(&mut self) -> Vec<&mut dyn Authenticator> {
        let plugged = self.plugged.borrow().clone();
        self.keys
            .iter_mut()
            .enumerate()
            .filter(|(index, _)| plugged.contains(index))
            .map(|(_, key)| -> &mut dyn Authenticator { key })
            .collect()
    }
}

/// What a setup said, shared with the operator who reads the codes off it.
#[derive(Clone, Default)]
struct Said(Rc<RefCell<Vec<u8>>>);

impl Said {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.borrow()).into_owned()
    }
}

impl Write for Said {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The last four characters of the last code shown.
fn last_code_end(said: &Said) -> String {
    let text = said.text();
    let last = text
        .lines()
        .rfind(|line| {
            line.trim_start()
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
        .unwrap_or_default()
        .replace('-', "");
    last.chars()
        .skip(last.chars().count().saturating_sub(4))
        .collect()
}

/// One run: what's asked, what's said, and how it ended.
struct Run {
    done: Result<Done, Error>,
    asked: Vec<String>,
    said: Said,
}

/// A vault adopted at the file tier, with a keyring.
fn vault(name: &str) -> (Scratch, VaultConfig, KeyringConfig) {
    adopted(name)
}

/// Runs a setup, the operator answering `answers` in turn (`"<code>"`
/// reads the last code's end off the screen) and each beat plugging in
/// what `swaps` says next.
fn run_with(
    vault: &VaultConfig,
    config: &KeyringConfig,
    bench: &mut Bench,
    answers: &[&str],
    swaps: &[&[usize]],
) -> Run {
    run_pins(vault, config, bench, answers, swaps, &[])
}

/// [`run_with`], the secrets asked for answered with `pins` in turn, then
/// one PIN; an answer of `<plug N>` plugs in that key alone.
fn run_pins(
    vault: &VaultConfig,
    config: &KeyringConfig,
    bench: &mut Bench,
    answers: &[&str],
    swaps: &[&[usize]],
    pins: &[&str],
) -> Run {
    run_doing(run, vault, config, bench, answers, swaps, pins)
}

/// [`run_with`], for a key that's gone.
fn run_lost(
    vault: &VaultConfig,
    config: &KeyringConfig,
    bench: &mut Bench,
    answers: &[&str],
    swaps: &[&[usize]],
) -> Run {
    run_doing(lost_key, vault, config, bench, answers, swaps, &[])
}

/// [`run_pins`], running `guide`.
fn run_doing(
    guide: fn(&mut Setup<'_>) -> Result<Done, Error>,
    vault: &VaultConfig,
    config: &KeyringConfig,
    bench: &mut Bench,
    answers: &[&str],
    swaps: &[&[usize]],
    pins: &[&str],
) -> Run {
    let said = Said::default();
    let stores = secrets(vault);
    let mut answers: VecDeque<String> = answers.iter().map(|answer| (*answer).to_owned()).collect();
    let mut asked = Vec::new();
    let reads = said.clone();
    let hands = Rc::clone(&bench.plugged);
    let mut ask = |prompt: &str| {
        asked.push(prompt.to_owned());
        let answer = answers.pop_front().unwrap_or_default();
        if let Some(key) = answer.strip_prefix("<plug ") {
            let key = key.trim_end_matches('>').parse().unwrap_or(0);
            *hands.borrow_mut() = vec![key];
            return Ok(String::new());
        }
        Ok(if answer == "<code>" {
            last_code_end(&reads)
        } else {
            answer
        })
    };
    let mut pins: VecDeque<String> = pins.iter().map(|pin| (*pin).to_owned()).collect();
    let mut secret = |_: &str| {
        Ok(Zeroizing::new(
            pins.pop_front().unwrap_or_else(|| String::from("246810")),
        ))
    };
    let plugged = Rc::clone(&bench.plugged);
    let mut swaps: VecDeque<Vec<usize>> = swaps.iter().map(|swap| swap.to_vec()).collect();
    let mut wait = || {
        swaps.pop_front().is_some_and(|swap| {
            *plugged.borrow_mut() = swap;
            true
        })
    };
    let mut out = said.clone();
    let done = guide(&mut Setup {
        vault,
        config,
        stores: &stores,
        ask: &mut ask,
        secret: &mut secret,
        devices: bench,
        out: &mut out,
        wait: &mut wait,
        wsl: false,
        touches: &Cell::new(0),
    });
    Run { done, asked, said }
}

fn keyring_of(config: &KeyringConfig) -> Keyring {
    Keyring::load(config.keyring.as_deref().unwrap()).unwrap()
}

fn names(config: &KeyringConfig) -> Vec<String> {
    keyring_of(config)
        .hardware_keys()
        .into_iter()
        .map(|key| key.label)
        .collect()
}

/// Both keys in at once are both taken: nobody is told to swap anything.
#[test]
fn two_keys_plugged_in_are_registered_together() {
    let (_scratch, vault, config) = vault("setup-both");
    let mut bench = Bench::new_keys(2, &[0, 1]);
    let run = run_with(&vault, &config, &mut bench, &["", "", "<code>"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config), ["Test key", "Test key 2"]);
    assert!(
        !run.said.text().contains("unplug this one"),
        "{}",
        run.said.text()
    );
    assert!(!run.asked.iter().any(|prompt| prompt.contains("second key")));
    assert!(!codes_unconfirmed(config.keyring.as_deref().unwrap()));
    let keyring = keyring_of(&config);
    for key in &mut bench.keys {
        let opened = keyring.unlock_hardware_key(key).unwrap();
        assert!(reads_the_secret(&vault, opened.key()));
    }
}

/// With one port: register, swap, and the next is noticed arriving.
#[test]
fn with_one_port_the_second_key_is_noticed_arriving() {
    let (_scratch, vault, config) = vault("setup-swap");
    let mut bench = Bench::new_keys(2, &[0]);
    let run = run_with(
        &vault,
        &config,
        &mut bench,
        &["", "yes", "", "<code>"],
        &[&[], &[1]],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config).len(), 2);
    assert!(
        run.said
            .text()
            .contains("unplug this one and plug in the other")
    );
}

/// The same key back in is said to be, untouched, and the wait goes on.
#[test]
fn the_same_key_again_is_told_apart() {
    let (_scratch, vault, config) = vault("setup-same");
    let mut bench = Bench::new_keys(2, &[0]);
    let run = run_with(
        &vault,
        &config,
        &mut bench,
        &["", "yes", "", "<code>"],
        &[&[0], &[1]],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(run.said.text().contains("a key you've just registered"));
    assert_eq!(names(&config).len(), 2);
}

/// One key is a choice, said plainly; declining it changes nothing.
#[test]
fn one_key_takes_accepting_what_that_means() {
    let (_scratch, vault, config) = vault("setup-one");
    let mut bench = Bench::new_keys(1, &[0]);
    let before = std::fs::read(config.keyring.as_deref().unwrap()).unwrap();
    let run = run_with(&vault, &config, &mut bench, &["", "no", ""], &[]);
    assert!(matches!(run.done, Err(Error::Usage { .. })));
    assert_eq!(keyring_of(&config).tier(), Tier::File, "as it was");
    assert_ne!(
        std::fs::read(config.keyring.as_deref().unwrap()).unwrap(),
        before,
        "only the codes that say there's a way back"
    );
    let run = run_with(
        &vault,
        &config,
        &mut bench,
        &["", "no", "one key", "<code>"],
        &[],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config).len(), 1);
    assert_eq!(keyring_of(&config).tier(), Tier::HardwareKey);
}

/// A key that isn't seen, after a while, gets a word on what to check, and
/// only the once.
#[test]
fn a_key_not_seen_gets_a_hint_on_what_to_check() {
    let (_scratch, vault, config) = vault("setup-hint");
    let mut bench = Bench::new_keys(1, &[]);
    let looks = usize::try_from(HINT_AFTER).unwrap();
    let nothing: Vec<&[usize]> = vec![&[]; looks * 2];
    let run = run_with(&vault, &config, &mut bench, &[], &nothing);
    assert_eq!(run.said.text().matches(not_seen(false)).count(), 1);
    let early: Vec<&[usize]> = vec![&[]; looks - 2];
    let run = run_with(&vault, &config, &mut bench, &[], &early);
    assert!(!run.said.text().contains(not_seen(false)));
    assert!(not_seen(false).contains("another port"));
    assert!(not_seen(true).contains("usbipd list"));
}

/// With no key in, it says what to do, sees one arrive, and with none it
/// stops having changed nothing.
#[test]
fn it_waits_for_a_key_and_stops_with_nothing_changed() {
    let (_scratch, vault, config) = vault("setup-wait");
    let mut bench = Bench::new_keys(2, &[]);
    let run = run_with(&vault, &config, &mut bench, &[], &[]);
    assert!(matches!(&run.done, Err(Error::Usage { reason }) if reason.contains("no key arrived")));
    assert!(run.said.text().contains("Plug in a security key."));
    assert_eq!(keyring_of(&config).tier(), Tier::File);
    let run = run_with(
        &vault,
        &config,
        &mut bench,
        &["", "no", "one key", "<code>"],
        &[&[0]],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
}

/// A new key gets its PIN here, when every key needs one.
#[test]
fn a_new_key_is_given_a_pin() {
    let (_scratch, vault, mut config) = vault("setup-pin");
    config.require_key_pin = true;
    let mut bench = Bench::new_keys(1, &[0]);
    let run = run_with(
        &vault,
        &config,
        &mut bench,
        &["", "no", "one key", "<code>"],
        &[],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(run.said.text().contains("This key has no PIN yet."));
    let key = bench.keys.first_mut().unwrap();
    assert_eq!(key.pin_set(), Some(true));
    assert_eq!(
        keyring_of(&config).hardware_keys().first().unwrap().check,
        hatchling_vault::KeyCheck::Pin
    );
    assert!(keyring_of(&config).unlock_hardware_key(key).is_ok());
}

/// A key that can't do this is said to be, with time to swap it.
#[test]
fn a_key_too_old_is_refused_and_another_taken() {
    let (_scratch, vault, config) = vault("setup-old");
    let mut bench = Bench::of(
        vec![
            VirtualAuthenticator::without_hmac_secret(),
            VirtualAuthenticator::new(),
        ],
        &[0],
    );
    let run = run_with(
        &vault,
        &config,
        &mut bench,
        &["<plug 1>", "", "no", "one key", "<code>"],
        &[],
    );
    assert!(run.said.text().contains("doesn't support hmac-secret"));
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config).len(), 1);
    // Refused again and again, it stops, and nothing has changed.
    let (_other, vault, config) = self::vault("setup-old-stop");
    let mut bench = Bench::of(vec![VirtualAuthenticator::without_hmac_secret()], &[0]);
    let run = run_with(&vault, &config, &mut bench, &[], &[]);
    assert!(matches!(run.done, Err(Error::Usage { .. })));
    assert_eq!(keyring_of(&config).tier(), Tier::File);
}

/// A PIN only some keyboards can type isn't set.
#[test]
fn a_new_pin_is_what_any_keyboard_types() {
    let (_scratch, vault, mut config) = vault("setup-pin-keyboard");
    config.require_key_pin = true;
    let mut bench = Bench::new_keys(1, &[0]);
    let run = run_pins(
        &vault,
        &config,
        &mut bench,
        &["", "no", "one key", "<code>"],
        &[],
        &[
            "gr\u{fc}n1234",
            "gr\u{fc}n1234",
            "two words",
            "two words",
            "Good-1234",
            "Good-1234",
        ],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(run.said.text().matches("from a US keyboard").count(), 2);
    assert_eq!(bench.keys.first().unwrap().pin_set(), Some(true));
}

/// A PIN typed two ways isn't set.
#[test]
fn a_new_pin_is_typed_twice_alike() {
    let (_scratch, vault, mut config) = vault("setup-pin-twice");
    config.require_key_pin = true;
    let mut bench = Bench::new_keys(1, &[0]);
    let run = run_pins(
        &vault,
        &config,
        &mut bench,
        &["", "no", "one key", "<code>"],
        &[],
        &["111111", "222222", "246810", "246810"],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(run.said.text().contains("Those didn't match."));
    assert_eq!(bench.keys.first().unwrap().pin_set(), Some(true));
}

/// Codes not confirmed stay said to be, and the next run replaces them.
#[test]
fn codes_never_confirmed_are_replaced_on_the_next_run() {
    let (_scratch, vault, config) = vault("setup-codes");
    let path = config.keyring.clone().unwrap();
    let mut bench = Bench::new_keys(2, &[0, 1]);
    let run = run_with(&vault, &config, &mut bench, &["", "", "x", "y", "z"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(codes_unconfirmed(&path));
    assert!(run.said.text().contains("Not confirmed"));
    assert_eq!(
        run.asked.iter().filter(|p| p.contains("last four")).count(),
        3
    );

    // Run again: a fresh set, and only that's asked.
    let run = run_with(&vault, &config, &mut bench, &["<code>"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(!codes_unconfirmed(&path));
    assert!(run.said.text().contains("a fresh set"));
    // Then nothing's left.
    let run = run_with(&vault, &config, &mut bench, &[], &[]);
    assert_eq!(run.done.unwrap(), Done::AlreadySetUp);
    assert!(run.asked.is_empty());
}

/// Lost at the worst moment: keys committed, the codes never shown.
#[test]
fn a_crash_before_the_codes_are_shown_is_picked_up() {
    let (_scratch, vault, config) = vault("setup-crash");
    let path = config.keyring.clone().unwrap();
    let mut bench = Bench::new_keys(2, &[0, 1]);
    run_with(&vault, &config, &mut bench, &["", "", "<code>"], &[])
        .done
        .unwrap();
    std::fs::write(codes_marker(&path), b"").unwrap();
    let run = run_with(&vault, &config, &mut bench, &["<code>"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(!codes_unconfirmed(&path));
}

/// A power cut mid-way: the keys committed, the stores not yet moved. The
/// vault won't serve like that; the next run finishes it, and makes the
/// codes nobody saw.
#[test]
fn a_change_interrupted_mid_way_is_finished() {
    let (_scratch, vault, config) = vault("setup-interrupted");
    let path = config.keyring.clone().unwrap();
    let mut bench = Bench::new_keys(2, &[0]);
    let mut keyring = keyring_of(&config);
    let kek = hatchling_vault::Kek::from_file_key(
        &hatchling_vault::FileKey::new(config.kek_file(&vault))
            .load()
            .unwrap(),
    );
    let unlocked = keyring.unlock_file(&kek).unwrap();
    keyring.generate_backup_codes(&unlocked).unwrap();
    let mut enrollment = keyring
        .begin_key_enrollment(&unlocked, hatchling_vault::KeyCheck::Touch)
        .unwrap();
    for key in &mut bench.keys {
        keyring.enroll_key(&mut enrollment, key).unwrap();
    }
    std::fs::write(codes_marker(&path), b"").unwrap();
    keyring.commit_key_enrollment(enrollment, None).unwrap();
    assert!(keyring_of(&config).rotation_pending());

    let run = run_with(&vault, &config, &mut bench, &["", "<code>"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(run.said.text().contains("interrupted"));
    let keyring = keyring_of(&config);
    assert!(!keyring.rotation_pending());
    assert!(!codes_unconfirmed(&path));
    let opened = keyring
        .unlock_hardware_key(bench.keys.first_mut().unwrap())
        .unwrap();
    assert!(reads_the_secret(&vault, opened.key()));
}

/// A vault with one key is offered a second, on the next run.
#[test]
fn a_vault_with_one_key_is_offered_a_second() {
    let (_scratch, vault, config) = vault("setup-second");
    let mut bench = Bench::new_keys(2, &[0]);
    run_with(
        &vault,
        &config,
        &mut bench,
        &["", "no", "one key", "<code>"],
        &[],
    )
    .done
    .unwrap();
    let run = run_with(
        &vault,
        &config,
        &mut bench,
        &["yes", "keychain"],
        &[&[], &[1]],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config), ["Test key", "keychain"]);
    assert!(run.said.text().contains("Registered: keychain."));
    // Said no to: left as it is, and asked again next time.
    let (_other, vault, config) = self::vault("setup-second-no");
    let mut bench = Bench::new_keys(1, &[0]);
    run_with(
        &vault,
        &config,
        &mut bench,
        &["", "no", "one key", "<code>"],
        &[],
    )
    .done
    .unwrap();
    let run = run_with(&vault, &config, &mut bench, &["no"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config).len(), 1);
}

/// A registering's touches are counted out, and then it's a touch like any.
#[test]
fn a_registering_counts_its_touches() {
    let left = Cell::new(2);
    assert!(touch_line(Touch::Waiting, &left).starts_with("Touch 1 of 2"));
    assert_eq!(touch_line(Touch::Noticed, &left), "Got it.");
    assert!(touch_line(Touch::Waiting, &left).starts_with("Touch 2 of 2"));
    assert_eq!(left.get(), 0);
    assert!(touch_line(Touch::Waiting, &left).starts_with("Touch your key now"));
    assert_eq!(left.get(), 0);
}

/// A vault with `count` keys, the first plugged in alone, ready for one to
/// go missing.
fn with_keys(name: &str, count: usize) -> (Scratch, VaultConfig, KeyringConfig, Bench, Run) {
    let (scratch, vault, config) = vault(name);
    let plugged: Vec<usize> = (0..count).collect();
    let mut bench = Bench::new_keys(count + 1, &plugged);
    let answers: &[&str] = if count == 1 {
        &["", "no", "one key", "<code>"]
    } else {
        &["", "", "<code>"]
    };
    let run = run_with(&vault, &config, &mut bench, answers, &[]);
    assert_eq!(run.done.as_ref().ok(), Some(&Done::Protected));
    *bench.plugged.borrow_mut() = vec![0];
    (scratch, vault, config, bench, run)
}

/// A lost key goes, by its number, and another is offered. What's left
/// still opens the vault.
#[test]
fn a_lost_key_is_removed_and_a_replacement_offered() {
    let (_scratch, vault, config, mut bench, _) = with_keys("lost", 2);
    let run = run_lost(&vault, &config, &mut bench, &["2", "lost", "no"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config), ["Test key"]);
    assert!(run.said.text().contains("Removed: Test key 2."));
    assert!(!run.said.text().contains("Changing the vault's key"));
    assert!(run.asked.iter().any(|asked| asked.contains("  2. ")));
    // A replacement, in the same go.
    let (_other, vault, config, mut bench, _) = with_keys("lost-new", 2);
    let run = run_lost(
        &vault,
        &config,
        &mut bench,
        &["2", "lost", "yes", ""],
        &[&[2]],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config).len(), 2);
}

/// A stolen key also changes the vault's key: the stores are sealed anew,
/// and there are new codes, confirmed like any.
#[test]
fn a_stolen_key_also_changes_the_vaults_key() {
    let (_scratch, vault, config, mut bench, _) = with_keys("stolen", 2);
    let before: Vec<Vec<u8>> = sealed_files(&vault);
    let run = run_lost(
        &vault,
        &config,
        &mut bench,
        &["2", "stolen", "", "<code>", "no"],
        &[],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config), ["Test key"]);
    assert!(run.said.text().contains("Changing the vault's key"));
    assert!(run.said.text().contains("Your recovery kit"));
    assert_ne!(sealed_files(&vault), before, "sealed anew");
    assert!(!keyring_of(&config).rotation_pending());
}

/// Every sealed file in the vault's stores.
fn sealed_files(vault: &VaultConfig) -> Vec<Vec<u8>> {
    let mut files = Vec::new();
    for (_, dir) in secrets(vault) {
        for entry in std::fs::read_dir(dir).unwrap() {
            files.push(std::fs::read(entry.unwrap().path()).unwrap());
        }
    }
    files
}

/// An only key that's gone: a backup code gets in, its replacement comes
/// first, and the vault is never without a key.
#[test]
fn an_only_key_lost_is_replaced_first() {
    let (_scratch, vault, config, mut bench, first) = with_keys("lost-only", 1);
    let code = first.said.text();
    let code = code
        .lines()
        .rfind(|line| line.trim_start().starts_with("10."))
        .and_then(|line| line.split_whitespace().last())
        .unwrap()
        .to_owned();
    *bench.plugged.borrow_mut() = Vec::new();
    let run = run_lost(
        &vault,
        &config,
        &mut bench,
        &["1", "lost", code.as_str(), "", "no"],
        &[&[1]],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(run.said.text().contains("its replacement comes first"));
    assert_eq!(names(&config), ["Test key 2"]);
}

/// Nothing's removed on an answer that's neither, or for no key at all.
#[test]
fn a_lost_key_needs_a_clear_answer() {
    let (_scratch, vault, config, mut bench, _) = with_keys("lost-unclear", 2);
    let run = run_lost(&vault, &config, &mut bench, &["2", "x", "y", "z"], &[]);
    assert!(run.done.is_err());
    assert_eq!(names(&config).len(), 2);
    let (_other, vault, config) = self::vault("lost-none");
    let mut bench = Bench::new_keys(1, &[0]);
    let run = run_lost(&vault, &config, &mut bench, &["1"], &[]);
    assert!(matches!(run.done, Err(Error::Usage { .. })));
}

/// The guided actions are these two.
#[test]
fn the_guided_actions_are_setup_and_lost_key() {
    assert!(guided(Some("setup")));
    assert!(guided(Some("lost-key")));
    assert!(!guided(Some("status")));
    assert!(!guided(None));
}

/// Two of a model are told apart, and names already taken are passed over.
#[test]
fn a_name_already_taken_gets_a_number() {
    let taken = |names: &[&str]| {
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(unused_name("YubiKey 5", &taken(&[])), "YubiKey 5");
    assert_eq!(
        unused_name("YubiKey 5", &taken(&["YubiKey 5"])),
        "YubiKey 5 2"
    );
    assert_eq!(
        unused_name("YubiKey 5", &taken(&["YubiKey 5", "YubiKey 5 2"])),
        "YubiKey 5 3"
    );
    assert_eq!(unused_name("YubiKey 5", &taken(&["Other"])), "YubiKey 5");
}

/// A name of one's own is taken as it is, and one that isn't a name asked
/// for again.
#[test]
fn a_key_can_be_named_anything_that_is_a_name() {
    let (_scratch, vault, config) = vault("setup-names");
    let mut bench = Bench::new_keys(2, &[0, 1]);
    let run = run_with(
        &vault,
        &config,
        &mut bench,
        &["blue key", "red key", "<code>"],
        &[],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config), ["blue key", "red key"]);
}

#[test]
fn the_marker_sits_beside_the_keyring() {
    let path = Path::new("/var/lib/hatchling/keyring/keyring");
    assert_eq!(
        codes_marker(path),
        PathBuf::from("/var/lib/hatchling/keyring/keyring.codes-unconfirmed")
    );
    assert!(!codes_unconfirmed(path));
}
