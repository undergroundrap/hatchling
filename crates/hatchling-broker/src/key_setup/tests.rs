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

impl std::io::Write for Said {
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
        .filter(|line| line.trim_start().chars().next().is_some_and(|c| c.is_ascii_digit()))
        .next_back()
        .unwrap_or_default()
        .replace('-', "");
    last.chars().skip(last.chars().count().saturating_sub(4)).collect()
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
    let done = run(&mut Setup {
        vault,
        config,
        stores: &stores,
        ask: &mut ask,
        secret: &mut secret,
        devices: bench,
        out: &mut out,
        wait: &mut wait,
        wsl: false,
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
    let run = run_with(&vault, &config, &mut bench, &["1", "2", "<code>"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config), ["Nano, in this computer", "Keychain key"]);
    assert!(!run.said.text().contains("unplug this one"), "{}", run.said.text());
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
        &["1", "yes", "2", "<code>"],
        &[&[], &[1]],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config).len(), 2);
    assert!(run.said.text().contains("unplug this one and plug in the other"));
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
        &["1", "yes", "2", "<code>"],
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
    let run = run_with(&vault, &config, &mut bench, &["1", "no", ""], &[]);
    assert!(matches!(run.done, Err(Error::Usage { .. })));
    assert_eq!(keyring_of(&config).tier(), Tier::File, "as it was");
    assert_ne!(
        std::fs::read(config.keyring.as_deref().unwrap()).unwrap(),
        before,
        "only the codes that say there's a way back"
    );
    let run = run_with(&vault, &config, &mut bench, &["1", "no", "one key", "<code>"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config).len(), 1);
    assert_eq!(keyring_of(&config).tier(), Tier::HardwareKey);
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
        &["1", "no", "one key", "<code>"],
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
    let run = run_with(&vault, &config, &mut bench, &["2", "no", "one key", "<code>"], &[]);
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
        &["<plug 1>", "1", "no", "one key", "<code>"],
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
        &["2", "no", "one key", "<code>"],
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
    let run = run_with(&vault, &config, &mut bench, &["1", "2", "x", "y", "z"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(codes_unconfirmed(&path));
    assert!(run.said.text().contains("Not confirmed"));
    assert_eq!(run.asked.iter().filter(|p| p.contains("last four")).count(), 3);

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
    run_with(&vault, &config, &mut bench, &["1", "2", "<code>"], &[])
        .done
        .unwrap();
    std::fs::write(codes_marker(&path), b"").unwrap();
    let run = run_with(&vault, &config, &mut bench, &["<code>"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(!codes_unconfirmed(&path));
}

/// A vault with one key is offered a second, on the next run.
#[test]
fn a_vault_with_one_key_is_offered_a_second() {
    let (_scratch, vault, config) = vault("setup-second");
    let mut bench = Bench::new_keys(2, &[0]);
    run_with(&vault, &config, &mut bench, &["1", "no", "one key", "<code>"], &[])
        .done
        .unwrap();
    let run = run_with(&vault, &config, &mut bench, &["yes", "keychain"], &[&[], &[1]]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config), ["Nano, in this computer", "keychain"]);
    assert!(run.said.text().contains("Registered: keychain."));
    // Said no to: left as it is, and asked again next time.
    let (_other, vault, config) = self::vault("setup-second-no");
    let mut bench = Bench::new_keys(1, &[0]);
    run_with(&vault, &config, &mut bench, &["1", "no", "one key", "<code>"], &[])
        .done
        .unwrap();
    let run = run_with(&vault, &config, &mut bench, &["no"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert_eq!(names(&config).len(), 1);
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
        &["blue key", "3", "red key", "<code>"],
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
