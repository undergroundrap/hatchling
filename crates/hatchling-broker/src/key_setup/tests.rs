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
    guide: impl FnOnce(&mut Setup<'_>) -> Result<Done, Error>,
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
            return Ok(Zeroizing::default());
        }
        Ok(Zeroizing::new(if answer == "<code>" {
            last_code_end(&reads)
        } else {
            answer
        }))
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
    let run = run_doing(
        |setup| {
            let room = crate::room_disk::here()?;
            let maintenance = crate::room_disk::maintenance(&room)?;
            let path = config.keyring.as_deref().unwrap();
            finish_interrupted(setup, path, &maintenance)?;
            protect(setup, keyring_of(&config), path, &maintenance)
        },
        &vault,
        &config,
        &mut bench,
        &["", "", "<code>"],
        &[],
        &[],
    );
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
    // With it, the way on for someone with no key at all.
    assert_eq!(run.said.text().matches(NO_KEY).count(), 1);
    assert!(
        NO_KEY.contains("Ctrl-C")
            && NO_KEY.ends_with("sudo hatchling-broker vault-keyring enroll-totp"),
        "{NO_KEY}"
    );
    let early: Vec<&[usize]> = vec![&[]; looks - 2];
    let run = run_with(&vault, &config, &mut bench, &[], &early);
    assert!(!run.said.text().contains(not_seen(false)));
    assert!(!run.said.text().contains(NO_KEY));
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

/// Lost just after the keyring was made: the plain key file is left, on a
/// vault that no longer needs it, and the next run takes it away.
#[test]
fn a_key_file_a_crash_left_goes_on_the_next_run() {
    let (_scratch, vault, config) = vault("setup-left");
    let mut bench = Bench::new_keys(2, &[0, 1]);
    run_with(&vault, &config, &mut bench, &["", "", "<code>"], &[])
        .done
        .unwrap();
    std::fs::write(&vault.key_file, [7u8; 32]).unwrap();
    let run = run_with(&vault, &config, &mut bench, &[], &[]);
    assert_eq!(run.done.unwrap(), Done::AlreadySetUp);
    assert!(!vault.key_file.exists(), "it stays on the disk");
}

/// Lost inside the first step: the keyring made, the plain key file not yet
/// gone. The next run protects the vault and takes the file away with it.
#[test]
fn a_key_file_left_inside_the_first_step_goes_as_the_vault_is_protected() {
    let (_scratch, vault, config) = vault("setup-left-early");
    std::fs::write(&vault.key_file, [7u8; 32]).unwrap();
    assert_eq!(keyring_of(&config).tier(), Tier::File);
    let mut bench = Bench::new_keys(2, &[0, 1]);
    let run = run_with(&vault, &config, &mut bench, &["", "", "<code>"], &[]);
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(
        !vault.key_file.exists(),
        "it stays beside a protected vault"
    );
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

/// After a stolen key, the agent's room disk is offered a reset: yes resets
/// it; no keeps it and says plainly what's left open. With no room disk,
/// there's nothing to ask.
#[test]
fn a_stolen_key_offers_to_reset_the_room_disk() {
    for (answer, kept) in [("yes", false), ("no", true)] {
        let (_scratch, vault, config, mut bench, _) =
            with_keys(&format!("stolen-room-{answer}"), 2);
        let room = crate::room_disk::tests::Bench::new(&format!("stolen-room-{answer}"));
        room.run(crate::room_disk::RoomDiskOp::Create).unwrap();
        let master = keyring_of(&config)
            .unlock_hardware_key(bench.keys.first_mut().unwrap())
            .unwrap()
            .into_key();
        let key = hatchling_vault::RoomRootKey::of(&master).image_key(&room.image_id());
        room.slots(&[key.expose().as_slice()]);
        let run = run_doing(
            |setup| lost_key_with(setup, &room.disk),
            &vault,
            &config,
            &mut bench,
            &["2", "stolen", "", "<code>", answer, "no"],
            &[],
            &[],
        );
        assert!(
            run.asked
                .iter()
                .any(|asked| asked.contains("Reset the agent's room disk?")),
            "a stolen key wasn't offered a room disk reset: {:?}",
            run.done.as_ref().err()
        );
        assert_eq!(run.done.unwrap(), Done::Protected);
        let said = run.said.text();
        assert_eq!(
            room.disk.image().exists(),
            kept,
            "the room disk's reset didn't follow the answer"
        );
        if kept {
            assert!(
                said.contains("an old copy of the room disk could still read it"),
                "a room disk kept after a stolen key didn't say what's left open: {said}"
            );
        } else {
            assert!(
                said.contains("Reset: the agent's room disk is gone"),
                "{said}"
            );
        }
    }
    // As run, from this process's settings: no room disk is here, so
    // there's nothing to ask.
    let (_scratch, vault, config, mut bench, _) = with_keys("stolen-no-room", 2);
    let run = run_doing(
        |setup| {
            let room = crate::room_disk::here().unwrap();
            assert!(!room.image().exists(), "a room disk is here");
            lost_key(setup)
        },
        &vault,
        &config,
        &mut bench,
        &["2", "stolen", "", "<code>", "no"],
        &[],
        &[],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(
        !run.asked.iter().any(|asked| asked.contains("room disk?")),
        "a reset was offered with no room disk"
    );
}

/// The question says what a stolen key changes, and doesn't say an old
/// copy of everything is no use: it asks about the room disk after.
#[test]
fn whether_a_key_is_stolen_is_asked_plainly() {
    let (_scratch, vault, config) = vault("stolen-asked");
    let mut bench = Bench::new_keys(1, &[0]);
    let run = run_doing(
        |setup| {
            might_be_stolen(setup).map(|stolen| {
                if stolen {
                    Done::Protected
                } else {
                    Done::AlreadySetUp
                }
            })
        },
        &vault,
        &config,
        &mut bench,
        &["stolen"],
        &[],
        &[],
    );
    assert_eq!(run.done.unwrap(), Done::Protected);
    assert!(
        run.asked.iter().any(|asked| asked.contains(
            "so an old copy of the vault is no use with it, and asks about the agent's room disk"
        )),
        "the question still says more is safe than is"
    );
}

/// A room disk that can't be looked at isn't offered a reset, and what's
/// left open is said all the same.
#[test]
fn a_room_disk_that_cant_be_looked_at_is_said_plainly() {
    let (_scratch, vault, config) = vault("room-unchecked");
    let mut bench = Bench::new_keys(1, &[0]);
    let room = crate::room_disk::tests::Bench::new("room-unchecked");
    room.run(crate::room_disk::RoomDiskOp::Create).unwrap();
    std::fs::set_permissions(
        room.disk.dir(),
        std::os::unix::fs::PermissionsExt::from_mode(0o777),
    )
    .unwrap();
    let run = run_doing(
        |setup| offer_room_reset(setup, &room.disk).map(|()| Done::AlreadySetUp),
        &vault,
        &config,
        &mut bench,
        &[],
        &[],
        &[],
    );
    assert_eq!(run.done.unwrap(), Done::AlreadySetUp);
    let said = run.said.text();
    assert!(
        said.contains("couldn't be looked at") && said.contains("could still read it"),
        "a room disk that couldn't be looked at wasn't said plainly: {said}"
    );
    assert!(
        run.asked.is_empty(),
        "a reset was offered for a room disk that couldn't be looked at"
    );
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

/// [`change_pin`] on `bench`, the secrets answered with `pins` in turn.
fn changing(name: &str, bench: &mut Bench, pins: &[&str]) -> (Result<(), Error>, Said) {
    let (_scratch, vault, config) = vault(name);
    let stores = secrets(&vault);
    let mut pins: VecDeque<String> = pins.iter().map(|pin| (*pin).to_owned()).collect();
    let mut ask = |_: &str| Ok(Zeroizing::default());
    let mut secret = |_: &str| Ok(Zeroizing::new(pins.pop_front().unwrap_or_default()));
    let mut wait = || false;
    let said = Said::default();
    let mut out = said.clone();
    let done = change_pin(&mut Setup {
        vault: &vault,
        config: &config,
        stores: &stores,
        ask: &mut ask,
        secret: &mut secret,
        devices: bench,
        out: &mut out,
        wait: &mut wait,
        wsl: false,
        touches: &Cell::new(0),
    });
    (done, said)
}

/// A PIN changes on the key plugged in, after whatever went wrong first.
#[test]
fn a_pin_is_changed_after_its_mistakes() {
    let mut bench = Bench::of(vec![VirtualAuthenticator::with_pin("246810")], &[0]);
    let (done, said) = changing(
        "change-pin-mistakes",
        &mut bench,
        &[
            "246810",
            "135790",
            "135791", // new PINs differ
            "246810",
            "gr\u{fc}n123",
            "gr\u{fc}n123", // not on a keyboard
            "246810",
            "135790",
            "135790",
        ],
    );
    done.unwrap();
    assert!(said.text().contains("Those didn't match."));
    assert!(said.text().contains("US keyboard"));
    assert!(said.text().contains("Your new PIN opens this key"));
    assert!(
        bench
            .keys
            .first_mut()
            .unwrap()
            .change_pin("135790", "x")
            .is_ok()
    );
}

/// A wrong current PIN is said, and three in a row stops.
#[test]
fn a_wrong_current_pin_stops_after_three() {
    let mut bench = Bench::of(vec![VirtualAuthenticator::with_pin("246810")], &[0]);
    let wrong: Vec<&str> = ["000000", "135790", "135790"].repeat(3);
    let (done, said) = changing("change-pin-wrong", &mut bench, &wrong);
    assert!(done.is_err());
    assert_eq!(said.text().matches("the PIN is wrong").count(), 3);
    assert!(
        bench
            .keys
            .first_mut()
            .unwrap()
            .change_pin("246810", "x")
            .is_ok()
    );
}

/// With no key in, it says to plug one in.
#[test]
fn a_pin_needs_its_key() {
    let mut bench = Bench::new_keys(1, &[]);
    let (done, _) = changing("change-pin-none", &mut bench, &[]);
    assert!(matches!(done, Err(Error::Usage { reason }) if reason.contains("plug in the key")));
}

/// The guided actions are these two.
#[test]
fn the_guided_actions_are_setup_and_lost_key() {
    assert!(guided(Some("setup")));
    assert!(guided(Some("lost-key")));
    assert!(!guided(Some("status")));
    assert!(!guided(None));
}

#[test]
fn a_guided_command_is_vault_keyring_with_a_guided_action() {
    let args =
        |list: &[&str]| -> Vec<String> { list.iter().map(|arg| (*arg).to_owned()).collect() };
    assert!(is_guided(&args(&["vault-keyring", "setup"])));
    assert!(is_guided(&args(&["vault-keyring", "lost-key"])));
    assert!(!is_guided(&args(&["vault-keyring", "status"])));
    assert!(!is_guided(&args(&["vault-keyring"])));
    assert!(!is_guided(&args(&["setup"])));
    assert!(!is_guided(&args(&["secret", "setup"])));
    assert!(!is_guided(&[]));
}

/// The authenticator enrollment is the service's own where there's a
/// service: no doc named it, and as the first keyring command on a fresh
/// install it stopped at a missing setting. Where there's no service it's
/// the plain command it was.
#[test]
fn the_authenticator_enrollment_is_the_services_own_on_a_service_install() {
    let args =
        |list: &[&str]| -> Vec<String> { list.iter().map(|arg| (*arg).to_owned()).collect() };
    assert!(totp_for_service(Some("enroll-totp"), true));
    assert!(!totp_for_service(Some("enroll-totp"), false));
    assert!(!totp_for_service(Some("enroll-keys"), true));
    assert!(!totp_for_service(None, true));

    let totp = args(&["vault-keyring", "enroll-totp"]);
    assert!(for_service(&totp, true));
    assert!(!for_service(&totp, false));
    // The guided ones are, with a service or without (they say so).
    for guided in ["setup", "lost-key"] {
        assert!(for_service(&args(&["vault-keyring", guided]), true));
        assert!(for_service(&args(&["vault-keyring", guided]), false));
    }
    // Nothing else is.
    assert!(!for_service(&args(&["vault-keyring", "status"]), true));
    assert!(!for_service(&args(&["secret", "enroll-totp"]), true));
    assert!(!for_service(&args(&["enroll-totp"]), true));
    assert!(!for_service(&[], true));
}

fn settings_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// A first setup puts the keyring beside the vault, and asks a PIN of
/// every key; a service with a keyring gains nothing; one with no vault
/// has nowhere to put one.
#[test]
fn a_first_setup_adds_a_keyring_beside_the_vault() {
    let fresh = settings_of(&[("HATCHLING_VAULT_DIR", "/var/lib/hatchling/vault")]);
    assert_eq!(
        first_settings(&fresh).unwrap(),
        [
            (
                String::from("HATCHLING_VAULT_KEYRING"),
                String::from("/var/lib/hatchling/keyring/keyring")
            ),
            (
                String::from("HATCHLING_REQUIRE_KEY_PIN"),
                String::from("true")
            ),
        ]
    );
    let keyed = settings_of(&[
        ("HATCHLING_VAULT_DIR", "/var/lib/hatchling/vault"),
        ("HATCHLING_VAULT_KEYRING", "/elsewhere/keyring"),
    ]);
    assert_eq!(first_settings(&keyed).unwrap(), []);
    for nowhere in [
        settings_of(&[]),
        settings_of(&[("HATCHLING_VAULT_DIR", "/")]),
    ] {
        assert_eq!(
            first_settings(&nowhere).unwrap_err(),
            "the settings name no vault"
        );
    }
}

/// A service on a bench: its settings in a file of this user's, and its
/// stops and starts counted.
struct Stand {
    _scratch: Scratch,
    settings: PathBuf,
    /// Where a keyring the settings name would be.
    keyring: PathBuf,
    stops: Cell<u32>,
    starts: Cell<u32>,
    /// Whether it stops, and whether it starts.
    stoppable: Cell<bool>,
    startable: Cell<bool>,
}

/// The settings of a fresh install, as the stand's file holds them.
fn fresh_text(vault: &Path) -> String {
    format!(
        "# the service's\nHATCHLING_VAULT_DIR={}\nHATCHLING_LOG_LEVEL=info\n",
        vault.display()
    )
}

impl Stand {
    fn new(name: &str) -> Self {
        let (scratch, vault, _) = vault(&format!("stand-{name}"));
        let root = vault.root.parent().unwrap().to_path_buf();
        let settings = root.join("hatchling.env");
        std::fs::write(&settings, fresh_text(&vault.root)).unwrap();
        Self {
            _scratch: scratch,
            settings,
            keyring: root.join("keyring").join("keyring"),
            stops: Cell::new(0),
            starts: Cell::new(0),
            stoppable: Cell::new(true),
            startable: Cell::new(true),
        }
    }

    fn vault_dir(&self) -> PathBuf {
        self.settings.parent().unwrap().join("vault")
    }

    fn text(&self) -> String {
        std::fs::read_to_string(&self.settings).unwrap()
    }

    /// The files beside the settings: the settings, and any backup.
    fn beside(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.settings.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("hatchling.env"))
            .collect();
        names.sort();
        names
    }

    /// Runs `act` on this service.
    fn with<T>(&self, act: impl FnOnce(&Service<'_>) -> T) -> T {
        act(&Service {
            settings: &self.settings,
            owner: hatchling_sys::euid(),
            stop: &|| {
                self.stops.set(self.stops.get() + 1);
                if self.stoppable.get() {
                    Ok(())
                } else {
                    Err(String::from("it won't stop"))
                }
            },
            start: &|| {
                self.starts.set(self.starts.get() + 1);
                self.startable.get()
            },
        })
    }

    /// Runs a setup around this service: `guide` makes the keyring, or
    /// doesn't, and ends as it says.
    fn setup(&self, guide: impl FnOnce() -> Result<u32, Error>) -> Result<u32, Error> {
        self.with(|service| around(service, Some(&self.keyring), guide))
    }
}

fn stopped_with(reason: &str) -> Error {
    Error::Usage {
        reason: reason.to_owned(),
    }
}

/// The reason a setup gave for stopping; nothing, if it didn't stop that
/// way.
fn reason_of(done: &Result<u32, Error>) -> String {
    match done {
        Err(Error::Usage { reason }) => reason.clone(),
        Ok(_) | Err(_) => String::new(),
    }
}

/// The settings name the keyring, and a PIN of every key, as a first
/// setup leaves them.
fn names_the_keyring(stand: &Stand) -> bool {
    let settings = crate::doctor::parse_settings(&stand.text());
    settings.get("HATCHLING_VAULT_KEYRING").map(String::as_str)
        == Some(&*stand.keyring.to_string_lossy())
        && settings
            .get("HATCHLING_REQUIRE_KEY_PIN")
            .map(String::as_str)
            == Some("true")
}

/// A setup is root's, on a service install, and looking changes nothing.
#[test]
fn a_setup_is_ready_only_as_root_on_a_service_install() {
    let stand = Stand::new("ready");
    let before = stand.text();
    stand.with(|service| {
        assert_eq!(ready(0, service), Ok(()));
        assert_eq!(ready(1000, service).unwrap_err(), "run it with sudo");
    });
    std::fs::remove_file(&stand.settings).unwrap();
    stand.with(|service| {
        let refused = ready(0, service).unwrap_err();
        assert!(
            refused.contains("isn't there: setup is for a service install")
                && refused.contains(&*stand.settings.to_string_lossy()),
            "{refused}"
        );
        assert_eq!(ready(1000, service).unwrap_err(), "run it with sudo");
    });
    assert_eq!((stand.stops.get(), stand.starts.get()), (0, 0));
    std::fs::write(&stand.settings, &before).unwrap();
    assert_eq!(stand.beside(), ["hatchling.env"]);
}

/// The first setup on a fresh install: the service is stopped, its
/// settings gain the keyring, the setup makes it, and the service is
/// started on settings that name a keyring that's there.
#[test]
fn a_first_setup_gives_the_service_its_keyring() {
    let stand = Stand::new("first");
    let done = stand.setup(|| {
        // The setup runs with the service stopped, and its settings ready.
        assert_eq!((stand.stops.get(), stand.starts.get()), (1, 0));
        assert!(names_the_keyring(&stand), "{}", stand.text());
        std::fs::create_dir_all(stand.keyring.parent().unwrap()).unwrap();
        std::fs::write(&stand.keyring, b"made").unwrap();
        Ok(7)
    });
    assert_eq!(done.unwrap(), 7);
    assert_eq!((stand.stops.get(), stand.starts.get()), (1, 1));
    assert!(names_the_keyring(&stand), "{}", stand.text());
    // What was there is kept, and a copy of the settings as they were.
    assert!(stand.text().starts_with(&fresh_text(&stand.vault_dir())));
    let beside = stand.beside();
    assert_eq!(beside.len(), 2, "{beside:?}");
    let kept = beside.last().unwrap();
    let backup = stand.settings.parent().unwrap().join(kept);
    assert!(kept.starts_with("hatchling.env.before-"), "{beside:?}");
    assert_eq!(
        std::fs::read_to_string(backup).unwrap(),
        fresh_text(&stand.vault_dir())
    );

    // Run again, the settings have their keyring: nothing is added.
    let text = stand.text();
    assert_eq!(stand.setup(|| Ok(8)).unwrap(), 8);
    assert_eq!(stand.text(), text);
    assert_eq!(stand.beside().len(), 2);
}

/// The first run of `vault-keyring setup` on a fresh install stopped with
/// an error after it had put a keyring in the service's settings, and the
/// service was started on a keyring that was never made: the keeper
/// stopped at every start, and Hatchling was down until someone ran the
/// setup again. A setup that stops before it makes a keyring leaves the
/// settings as they were, and the service starts as it did before.
#[test]
fn a_setup_that_makes_no_keyring_leaves_the_settings_as_they_were() {
    let stand = Stand::new("stopped");
    let before = stand.text();
    let done = stand.setup(|| {
        assert!(names_the_keyring(&stand), "{}", stand.text());
        Err(stopped_with("the guide stopped"))
    });
    assert!(
        matches!(&done, Err(Error::Usage { reason }) if reason == "the guide stopped"),
        "{done:?}"
    );
    assert_eq!(
        stand.text(),
        before,
        "the settings name a keyring never made"
    );
    assert_eq!((stand.stops.get(), stand.starts.get()), (1, 1));

    // One that stops after the keyring is made keeps them: the vault's key
    // is in the keyring now, and the service needs its name.
    let stand = Stand::new("stopped-late");
    let done = stand.setup(|| {
        std::fs::create_dir_all(stand.keyring.parent().unwrap()).unwrap();
        std::fs::write(&stand.keyring, b"made").unwrap();
        Err(stopped_with("no key arrived"))
    });
    assert!(
        matches!(&done, Err(Error::Usage { reason }) if reason == "no key arrived"),
        "{done:?}"
    );
    assert!(names_the_keyring(&stand), "{}", stand.text());
    assert_eq!((stand.stops.get(), stand.starts.get()), (1, 1));

    // Something at the keyring's name that isn't a keyring is none made.
    let stand = Stand::new("stopped-not-a-file");
    let before = stand.text();
    let done = stand.setup(|| {
        std::fs::create_dir_all(&stand.keyring).unwrap();
        Err(stopped_with("stopped"))
    });
    assert!(done.is_err());
    assert_eq!(stand.text(), before);

    // A service that had its keyring before the setup keeps it named,
    // whatever the setup does: it added nothing, so takes nothing back.
    let stand = Stand::new("stopped-keyed");
    let keyed = format!(
        "{}HATCHLING_VAULT_KEYRING={}\n",
        fresh_text(&stand.vault_dir()),
        stand.keyring.display()
    );
    std::fs::write(&stand.settings, &keyed).unwrap();
    let done = stand.setup(|| Err(stopped_with("stopped")));
    assert!(done.is_err());
    assert_eq!(stand.text(), keyed);
    assert_eq!(stand.beside(), ["hatchling.env"]);
}

/// With no keyring named to look for, a setup that stopped made none.
#[test]
fn a_setup_with_no_keyring_to_look_for_takes_its_settings_back() {
    let stand = Stand::new("no-name");
    let before = stand.text();
    let done: Result<u32, Error> =
        stand.with(|service| around(service, None, || Err(stopped_with("stopped"))));
    assert!(done.is_err());
    assert_eq!(stand.text(), before);
}

/// A service that won't stop is left alone: nothing is changed, the setup
/// doesn't run, and it isn't started. One that doesn't start again is
/// said, with the command that starts it.
#[test]
fn a_service_that_does_not_stop_or_start_is_said() {
    let stand = Stand::new("stuck");
    let before = stand.text();
    stand.stoppable.set(false);
    let ran = Cell::new(false);
    let done = stand.setup(|| {
        ran.set(true);
        Ok(0)
    });
    assert!(!ran.get(), "the setup ran with the service going");
    assert_eq!(reason_of(&done), "it won't stop");
    assert_eq!(stand.text(), before);
    assert_eq!((stand.stops.get(), stand.starts.get()), (1, 0));

    let stand = Stand::new("unstarted");
    stand.startable.set(false);
    let done = stand.setup(|| Ok(1));
    assert!(
        matches!(&done, Err(Error::Usage { reason }) if reason == "the service didn't start: sudo systemctl start hatchling"),
        "{done:?}"
    );
    // Why the setup stopped comes first, when it did.
    let stand = Stand::new("unstarted-stopped");
    stand.startable.set(false);
    let done = stand.setup(|| Err(stopped_with("the guide stopped")));
    assert!(
        matches!(&done, Err(Error::Usage { reason }) if reason == "the guide stopped"),
        "{done:?}"
    );
    assert_eq!(stand.starts.get(), 1);
}

/// Settings the setup can't trust or can't use stop it before the guide
/// runs, and the service, stopped for it, is started again.
#[test]
fn settings_a_setup_cannot_use_stop_it_and_the_service_goes_back() {
    let stand = Stand::new("untrusted");
    mode_of(&stand.settings, 0o666);
    let ran = Cell::new(false);
    let done = stand.setup(|| {
        ran.set(true);
        Ok(0)
    });
    assert!(!ran.get(), "the setup ran on settings anyone can change");
    let reason = reason_of(&done);
    assert!(
        reason.contains("it isn't read") && reason.contains("chmod 600"),
        "{done:?}"
    );
    assert_eq!((stand.stops.get(), stand.starts.get()), (1, 1));

    let stand = Stand::new("no-vault");
    std::fs::write(&stand.settings, "HATCHLING_LOG_LEVEL=info\n").unwrap();
    let done = stand.setup(|| {
        ran.set(true);
        Ok(0)
    });
    assert!(!ran.get(), "the setup ran with no vault");
    assert_eq!(reason_of(&done), "the settings name no vault");
    assert_eq!(stand.text(), "HATCHLING_LOG_LEVEL=info\n");
    assert_eq!((stand.stops.get(), stand.starts.get()), (1, 1));
}

fn mode_of(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Settings that can't be put back are said with why the setup stopped,
/// and what to do: they still name a keyring that isn't there.
#[test]
fn settings_that_cannot_be_put_back_are_said() {
    let stand = Stand::new("stuck-settings");
    let dir = stand.settings.parent().unwrap().to_path_buf();
    let done = stand.setup(|| {
        // Nothing can be written beside the settings now.
        std::fs::create_dir(dir.join("hatchling.env.next")).unwrap();
        Err(stopped_with("the guide stopped"))
    });
    let reason = reason_of(&done);
    assert!(
        reason.starts_with("the guide stopped. And ")
            && reason.contains("couldn't be put back as it was")
            && reason.ends_with("run this again, and it carries on from here"),
        "{reason}"
    );
    assert_eq!(stand.starts.get(), 1);

    // Whatever kind of failure stopped it, it's said first, as it reads.
    let stand = Stand::new("stuck-settings-other");
    let dir = stand.settings.parent().unwrap().to_path_buf();
    let done = stand.setup(|| {
        std::fs::create_dir(dir.join("hatchling.env.next")).unwrap();
        Err(Error::Keeper {
            reason: String::from("no keeper"),
        })
    });
    let reason = reason_of(&done);
    let keeper = Error::Keeper {
        reason: String::from("no keeper"),
    }
    .to_string();
    assert!(reason.starts_with(&format!("{keeper}. And ")), "{reason}");
}

/// The whole of `vault-keyring setup` on a fresh install, stopped by the
/// guide itself: with no keyring among its settings it stops at once, and
/// the service is started again on the settings it had.
#[test]
fn the_real_setup_stopped_at_its_first_step_changes_nothing() {
    let stand = Stand::new("real");
    let before = stand.text();
    let (_scratch, vault, _) = vault("stand-real-vault");
    let stores = secrets(&vault);
    let unnamed = KeyringConfig::default();
    let done = stand.with(|service| run_setup(service, &vault, &unnamed, &stores, Some("setup")));
    assert!(matches!(done, Err(Error::Usage { .. })), "{done:?}");
    assert_eq!(stand.text(), before);
    assert_eq!((stand.stops.get(), stand.starts.get()), (1, 1));
}

/// The whole of `vault-keyring enroll-totp` on a fresh service install,
/// stopped at its first step: with no keyring among its settings it
/// stops at once, the settings are as they were, and the service is
/// started again.
#[test]
fn the_real_enrollment_stopped_at_its_first_step_changes_nothing() {
    let stand = Stand::new("real-totp");
    let before = stand.text();
    let (_scratch, vault, _) = vault("stand-real-totp-vault");
    let stores = secrets(&vault);
    let unnamed = KeyringConfig::default();
    let done = stand.with(|service| run_totp(service, &vault, &unnamed, &stores));
    assert!(matches!(done, Err(Error::Usage { .. })), "{done:?}");
    assert_eq!(stand.text(), before);
    assert_eq!((stand.stops.get(), stand.starts.get()), (1, 1));
}

/// At the terminal, an empty answer to the PIN prompt is no PIN, as one
/// that can't be read is; and of the two guided actions, only `lost-key`
/// is the guide for a key that's gone.
#[test]
fn the_terminals_pin_and_guide_are_read_as_given() {
    let typed = |text: &str| typed_pin(Ok(Zeroizing::new(text.to_owned())));
    assert_eq!(
        typed("246810").as_deref().map(String::as_str),
        Some("246810")
    );
    assert_eq!(typed(""), None);
    assert_eq!(typed_pin(Err(stopped_with("no terminal"))), None);
    assert!(is_lost_key(Some("lost-key")));
    assert!(!is_lost_key(Some("setup")));
    assert!(!is_lost_key(None));
}

/// `systemctl` is asked, and its answer is the answer.
#[test]
fn the_service_is_stopped_and_started_by_systemctl() {
    assert!(systemctl(Path::new("/bin/true"), "start", "hatchling"));
    assert!(!systemctl(Path::new("/bin/false"), "start", "hatchling"));
    assert!(!systemctl(
        Path::new("/nonexistent/systemctl"),
        "stop",
        "hatchling"
    ));
    assert!(start_after_setup(Path::new("/bin/true")));
    assert!(!start_after_setup(Path::new("/bin/false")));
}

/// A place for a setup's lock.
fn lock_at(name: &str) -> (Scratch, PathBuf) {
    let (scratch, vault, _) = vault(&format!("stand-lock-{name}"));
    let lock = vault.root.parent().unwrap().join("setup.lock");
    (scratch, lock)
}

/// Ctrl-C while a setup waited for a key left Hatchling stopped: the
/// setup had stopped it, and was gone before it could start it again. A
/// setup holds a lock while the service is stopped, and a unit of the
/// system's waits on that lock to start the service: the lock is let go
/// when the setup's process ends, whatever ended it.
#[test]
fn a_setup_holds_a_lock_that_a_guard_waits_on() {
    let (_scratch, lock) = lock_at("guard");
    assert_eq!(
        guard_arguments(&lock),
        [
            "--quiet",
            "--collect",
            "--unit",
            "hatchling-setup-guard",
            "flock",
            &*lock.to_string_lossy(),
            "systemctl",
            "start",
            "hatchling",
        ]
    );
    let held = guard(&lock, Path::new("/bin/true")).unwrap();
    assert!(held.armed, "the guard was set, and isn't said to be");
    // One setup at a time: a second finds the lock held, and says so.
    let second = guard(&lock, Path::new("/bin/true")).unwrap_err();
    assert!(second.starts_with("another setup is running"), "{second}");
    drop(held);
    // Let go, it's free again; a guard that can't be set is known.
    for unset in ["/bin/false", "/nonexistent/systemd-run"] {
        let held = guard(&lock, Path::new(unset)).unwrap();
        assert!(!held.armed, "{unset} set no guard");
    }
    // A lock that can't be taken at all is said.
    let nowhere = guard(
        Path::new("/nonexistent/dir/setup.lock"),
        Path::new("/bin/true"),
    );
    assert!(
        nowhere
            .unwrap_err()
            .starts_with("can't take the setup's lock: "),
    );
}

/// The service is stopped under the guard, and a setup with no guard is
/// told what an interruption would leave. A service that doesn't stop
/// keeps no lock.
#[test]
fn the_service_is_stopped_for_a_setup_under_its_guard() {
    let (_scratch, lock) = lock_at("stop");
    let yes = Path::new("/bin/true");
    let no = Path::new("/bin/false");
    let mut said = Vec::new();
    let held = stop_for_setup(&lock, yes, yes, &mut said).unwrap();
    assert!(held.armed);
    assert!(said.is_empty(), "{}", String::from_utf8_lossy(&said));
    drop(held);

    let held = stop_for_setup(&lock, no, yes, &mut said).unwrap();
    assert!(!held.armed);
    assert_eq!(
        String::from_utf8_lossy(&said),
        "If this is interrupted, Hatchling stays stopped until you start it: sudo systemctl start hatchling\n"
    );
    drop(held);

    let refused = stop_for_setup(&lock, yes, no, &mut Vec::new()).unwrap_err();
    assert_eq!(
        refused,
        "can't stop the service: sudo systemctl stop hatchling"
    );
    assert!(
        guard(&lock, yes).is_ok(),
        "a service that didn't stop kept the lock"
    );
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

/// A stopped keeper's socket goes, and only that: a file or a link at its
/// name stays, with what the link points at, as does a stranger's socket.
#[test]
fn only_a_keepers_socket_is_forgotten() {
    let dir = std::env::temp_dir().join(format!("hatchling-socket-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let me = [hatchling_sys::euid()];
    let stranger = [me[0].wrapping_add(1)];
    let socket = dir.join("unlock.sock");
    forget_socket(&socket, &me);
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    forget_socket(&socket, &stranger);
    assert!(socket.symlink_metadata().is_ok(), "a stranger's socket");
    forget_socket(&socket, &me);
    assert!(socket.symlink_metadata().is_err(), "the keeper's socket");
    drop(listener);
    let target = dir.join("target");
    std::fs::write(&target, b"untouched").unwrap();
    std::os::unix::fs::symlink(&target, &socket).unwrap();
    forget_socket(&socket, &me);
    assert!(socket.symlink_metadata().is_ok(), "the link went");
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    std::fs::remove_file(&socket).unwrap();
    std::fs::write(&socket, b"a file").unwrap();
    forget_socket(&socket, &me);
    assert_eq!(std::fs::read(&socket).unwrap(), b"a file");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The marker that backup codes aren't confirmed yet is made as root,
/// beside a keyring its service user owns: a link planted at its name
/// isn't followed, and what it points at stays as it was.
#[test]
fn the_codes_marker_is_never_made_through_a_link() {
    let dir = std::env::temp_dir().join(format!("hatchling-codes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    let keyring = dir.join("keyring");
    assert!(!codes_unconfirmed(&keyring));
    mark_codes(&keyring).unwrap();
    assert!(codes_unconfirmed(&keyring));
    mark_codes(&keyring).unwrap();
    std::fs::remove_file(codes_marker(&keyring)).unwrap();
    let target = dir.join("target");
    std::fs::write(&target, b"untouched").unwrap();
    std::os::unix::fs::symlink(&target, codes_marker(&keyring)).unwrap();
    assert!(mark_codes(&keyring).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
    std::fs::remove_dir_all(&dir).unwrap();
}
