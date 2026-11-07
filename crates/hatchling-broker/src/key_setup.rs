//! `vault-keyring setup`: one guided command that protects the vault with
//! security keys, whatever the operator has to hand.
//!
//! It adapts rather than asks: it looks at what's plugged in. Two keys in
//! at once are both registered; with one port, it registers the first and
//! says to swap, and sees the next arrive. A key that's new gets its PIN
//! here. Nothing changes in the vault until every key is ready
//! ([`hatchling_vault::KeyEnrollment`]): a power cut before that leaves it
//! as it was.
//!
//! **Run again, it carries on.** It reads where things stand, and offers
//! what's left: a change to the vault's key that a power cut interrupted
//! is finished; a vault with one key is offered a second; backup codes
//! never confirmed saved ([`codes_unconfirmed`]) are replaced, and
//! confirmed. So there's one command to remember, whatever happened.
//!
//! **Backup codes count once you've shown you have them.** The marker
//! beside the keyring is written before the codes exist, so a crash
//! between making them and showing them still counts as unconfirmed, and
//! it goes when part of a code is typed back. `doctor` and `unlock` say so
//! until then.
//!
//! The PIN of a key is typed here, in the operator's own terminal, and
//! goes only to the key.

use std::cell::Cell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::Duration;

use hatchling_fido2::ctaphid::Touch;
use hatchling_vault::{
    Authenticator, BackupCodes, Keyring, Label, SingleKeyRisk, Store, Tier, Unlocked,
};
use zeroize::Zeroizing;

use crate::Error;
use crate::config::VaultConfig;
use crate::vault_keyring::{self as ring, Ask, Devices, KeyringConfig};

/// The action.
pub const SETUP_ACTION: &str = "setup";
/// The `vault-keyring` action for a key that's gone.
pub const LOST_ACTION: &str = "lost-key";

/// The `vault-keyring` action that changes a key's PIN.
pub const CHANGE_PIN_ACTION: &str = "change-pin";

/// Whether `action` is one of the guided ones, run in the operator's
/// terminal with the service stopped.
#[must_use]
pub fn guided(action: Option<&str>) -> bool {
    matches!(action, Some(SETUP_ACTION | LOST_ACTION))
}

/// How often it looks for a key.
const LOOK_EVERY: Duration = Duration::from_millis(500);

/// How long it looks before it asks again: five minutes.
const LOOKS: u32 = 600;
/// How many looks with nothing there before saying what to check.
const HINT_AFTER: u32 = 30;

/// How many wrong tries before a step stops.
const TRIES: u32 = 3;

/// How a secret answer is asked: nothing shown as it's typed.
pub type Secret<'a> = &'a mut dyn FnMut(&str) -> Result<Zeroizing<String>, Error>;

/// All a setup reaches outside the keyring.
pub struct Setup<'a> {
    /// The vault.
    pub vault: &'a VaultConfig,
    /// Its keyring's settings.
    pub config: &'a KeyringConfig,
    /// Every sealed store, to move to the vault's new key.
    pub stores: &'a [(Store, PathBuf)],
    /// A plain question.
    pub ask: Ask<'a>,
    /// A secret one.
    pub secret: Secret<'a>,
    /// The keys.
    pub devices: &'a mut dyn Devices,
    /// What it says.
    pub out: &'a mut dyn Write,
    /// A beat between looks for a key; false when it should stop looking.
    pub wait: &'a mut dyn FnMut() -> bool,
    /// Whether this is WSL, where a key has to be attached to be seen.
    pub wsl: bool,
    /// How many touches a key has left to give in this registering.
    pub touches: &'a Cell<u32>,
}

impl Setup<'_> {
    fn say(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }
}

/// Where a setup got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Done {
    /// The vault is at the hardware-key tier, and all's as it should be.
    Protected,
    /// It was already: nothing was left to do.
    AlreadySetUp,
}

/// The marker beside `keyring`: backup codes were made, and not yet
/// confirmed saved.
pub(crate) fn codes_marker(keyring: &Path) -> PathBuf {
    let mut name = keyring.as_os_str().to_owned();
    name.push(".codes-unconfirmed");
    PathBuf::from(name)
}

/// Whether the backup codes of the keyring at `keyring` were never
/// confirmed saved.
#[must_use]
pub fn codes_unconfirmed(keyring: &Path) -> bool {
    codes_marker(keyring).exists()
}

/// What to say of a key that can't do this.
const TOO_OLD: &str = "This key can't protect the vault: it doesn't support hmac-secret. Most FIDO2 keys from the last several years do; an older U2F-only key doesn't. Plug in another.";

/// How a swap is said.
const SWAP: &str = "Now your second key. If you only have one port, unplug this one and plug in the other. It's noticed when it arrives.";

/// Runs the setup: what's left of it, from where things stand.
///
/// # Errors
///
/// Why it stopped; run again, it carries on from there.
pub fn run(setup: &mut Setup<'_>) -> Result<Done, Error> {
    let path = setup.config.keyring_path()?.to_path_buf();
    if !path.exists() {
        ring::init(setup.vault, setup.config)?;
    }
    finish_interrupted(setup, &path)?;
    let keyring = Keyring::load(&path).map_err(ring::vault_error)?;
    if keyring.tier() == Tier::HardwareKey {
        carry_on(setup, keyring, &path)
    } else {
        protect(setup, keyring, &path)
    }
}

/// Finishes a change to the vault's key that was cut off, if one was.
fn finish_interrupted(setup: &mut Setup<'_>, path: &Path) -> Result<(), Error> {
    if !Keyring::load(path)
        .map_err(ring::vault_error)?
        .rotation_pending()
    {
        return Ok(());
    }
    setup.say("A change to the vault's key was interrupted. Finishing it now.");
    let (_, codes) = ring::rekey(
        setup.vault,
        setup.config,
        setup.stores,
        setup.ask,
        setup.devices,
    )?;
    if let Some(codes) = codes {
        recovery_kit(setup, path, &codes)?;
    }
    Ok(())
}

/// `vault-keyring change-pin`: a new PIN on the key plugged in. The vault
/// doesn't change, and nor does what's registered: a PIN guards a key, and
/// isn't part of what it computes.
///
/// # Errors
///
/// Why not; a wrong current PIN is said and asked for again.
pub fn change_pin(setup: &mut Setup<'_>) -> Result<(), Error> {
    let min = {
        let mut keys = setup.devices.every_key();
        let Some(key) = keys.first_mut() else {
            drop(keys);
            return Err(Error::Usage {
                reason: format!(
                    "plug in the key to change ({})",
                    setup.devices.unreachable()
                ),
            });
        };
        key.min_pin_length()
    };
    setup.say("This changes the PIN on the key plugged in. To keep the same PIN on another key, run this again with that one.");
    for _ in 0..TRIES {
        let old = (setup.secret)("Current PIN:")?;
        let new = (setup.secret)(&format!("New PIN, at least {min} characters:"))?;
        let again = (setup.secret)("The same new PIN again:")?;
        if new != again {
            setup.say("Those didn't match. Try again.");
            continue;
        }
        if !keyboard_only(&new) {
            setup.say(KEYBOARD_ONLY);
            continue;
        }
        let changed = {
            let mut keys = setup.devices.every_key();
            let Some(key) = keys.first_mut() else {
                drop(keys);
                return Err(ring::stopped());
            };
            key.change_pin(&old, &new)
        };
        match changed {
            Ok(()) => {
                setup.say("Done. Your new PIN opens this key, and nothing else has changed.");
                return Ok(());
            }
            Err(refused) => setup.say(&refused.said()),
        }
    }
    Err(ring::stopped())
}

/// [`change_pin`], at this terminal.
///
/// # Errors
///
/// As [`change_pin`].
pub fn run_change_pin(
    broker: &crate::Config,
    config: &KeyringConfig,
    stores: &[(Store, PathBuf)],
) -> Result<(), Error> {
    let mut asking =
        |prompt: &str| ring::terminal_answer(prompt, false).map(|answer| answer.to_string());
    let mut secret = |prompt: &str| ring::terminal_answer(prompt, true);
    let mut devices = ring::platform_keys(Box::new(|| None));
    let mut wait = || false;
    let mut out = std::io::stderr();
    let touches = Cell::new(0);
    change_pin(&mut Setup {
        vault: &broker.vault,
        config,
        stores,
        ask: &mut asking,
        secret: &mut secret,
        devices: &mut devices,
        out: &mut out,
        wait: &mut wait,
        wsl: false,
        touches: &touches,
    })
}

/// Whether a key that's gone might be in someone's hands.
fn might_be_stolen(setup: &mut Setup<'_>) -> Result<bool, Error> {
    for _ in 0..TRIES {
        let answer = (setup.ask)(
            "Is it lost, or could someone have it? Type lost or stolen. Stolen also changes the vault's key and your backup codes, so an old copy of your files is no use with it:",
        )?;
        match answer.trim().to_ascii_lowercase().as_str() {
            "lost" => return Ok(false),
            "stolen" => return Ok(true),
            _ => {}
        }
    }
    Err(ring::stopped())
}

/// `vault-keyring lost-key`: a key that's gone, out of the vault.
///
/// A key that might be in someone's hands also changes the vault's key, as
/// removing it alone leaves an old copy of the keyring that it, its PIN and
/// that copy could open. An only key's replacement comes first, as the
/// vault never has none: a backup code gets in.
///
/// # Errors
///
/// Why it stopped: nothing is removed until it's been authorized.
pub fn lost_key(setup: &mut Setup<'_>) -> Result<Done, Error> {
    let path = setup.config.keyring_path()?.to_path_buf();
    finish_interrupted(setup, &path)?;
    let mut keyring = Keyring::load(&path).map_err(ring::vault_error)?;
    let keys = keyring.hardware_keys();
    if keys.is_empty() {
        return Err(Error::Usage {
            reason: String::from("this vault has no security keys, so there's none to lose"),
        });
    }
    let lost = ring::chosen_key(&keys, "remove", setup.ask)?;
    let stolen = might_be_stolen(setup)?;
    if keys.len() == 1 {
        setup.say("That's your only key, so its replacement comes first.");
        let unlocked = ring::opening(&mut keyring, setup.ask, setup.devices)?;
        add_second(setup, &mut keyring, &unlocked)?;
    }
    // The key is gone either way: one left is what's left, not a choice.
    let risk = (keyring.hardware_keys().len() == 2).then(SingleKeyRisk::accepted);
    ring::remove_authorized(&mut keyring, &lost, risk, setup.ask, setup.devices)?;
    setup.say(&format!(
        "Removed: {}. It can't open the vault now.",
        lost.label
    ));
    if stolen {
        setup.say("Changing the vault's key, so nothing the stolen key could open still works.");
        let (_, codes) = ring::rekey(
            setup.vault,
            setup.config,
            setup.stores,
            setup.ask,
            setup.devices,
        )?;
        if let Some(codes) = codes {
            recovery_kit(setup, &path, &codes)?;
        }
    }
    let keyring = Keyring::load(&path).map_err(ring::vault_error)?;
    carry_on(setup, keyring, &path)
}

/// Moves a vault below the hardware-key tier to its keys.
fn protect(setup: &mut Setup<'_>, mut keyring: Keyring, path: &Path) -> Result<Done, Error> {
    setup.say(
        "This protects Hatchling's vault with security keys: nothing opens it without one.\n\
         After each restart, you unlock it with a key and its PIN.\n\
         Two keys are best: either one opens the vault. Nothing changes until they're ready.\n\
         Have two? Start with the one you'll carry, like a 5C NFC. One that stays plugged in, like a Nano, goes last.",
    );
    if setup.wsl {
        setup.say(
            "In WSL, keep your key attached while you swap keys: in PowerShell, leave this running:\n  usbipd attach --wsl --busid <id> --auto-attach",
        );
    }
    let unlocked = ring::unlock_current(
        &mut keyring,
        setup.vault,
        setup.config,
        setup.ask,
        setup.devices,
    )?;
    if keyring.backup_codes_left() == 0 {
        // They're replaced when the keys are committed: these only say
        // there's a way back, as a single key needs.
        keyring
            .generate_backup_codes(&unlocked)
            .map_err(ring::vault_error)?;
    }
    let mut enrollment = keyring
        .begin_key_enrollment(&unlocked, setup.config.key_check())
        .map_err(ring::vault_error)?;
    register_next(setup, &keyring, &mut enrollment)?;
    let in_already = another_key_is_in(setup, &enrollment);
    let risk = if in_already || wants_a_second_key(setup)? {
        if !in_already {
            setup.say(SWAP);
        }
        register_next(setup, &keyring, &mut enrollment)?;
        None
    } else {
        Some(SingleKeyRisk::accepted())
    };
    mark_codes(path)?;
    let rotated = keyring
        .commit_key_enrollment(enrollment, risk)
        .map_err(ring::vault_error)?;
    ring::finish(
        &mut keyring,
        &rotated.rotation,
        setup.stores,
        &setup.config.kek_file(setup.vault),
    )?;
    if let Some(codes) = &rotated.codes {
        recovery_kit(setup, path, codes)?;
    }
    Ok(Done::Protected)
}

/// Whether a key that isn't in `enrollment` is plugged in now.
fn another_key_is_in(setup: &mut Setup<'_>, enrollment: &hatchling_vault::KeyEnrollment) -> bool {
    setup
        .devices
        .every_key()
        .iter_mut()
        .any(|key| !enrollment.holds(&mut **key))
}

/// Whether the operator has a second key to hand: going on with one takes
/// accepting what that means.
fn wants_a_second_key(setup: &mut Setup<'_>) -> Result<bool, Error> {
    let answer = (setup.ask)("Do you have a second key with you? Type yes or no:")?;
    if answer.trim().eq_ignore_ascii_case("yes") {
        return Ok(true);
    }
    let accept = (setup.ask)(
        "With one key, losing it leaves only a backup code to open the vault. You can add a second later: run this again. Type \"one key\" to go on with one, or nothing to stop (nothing has changed):",
    )?;
    if accept.trim() == "one key" {
        Ok(false)
    } else {
        Err(ring::stopped())
    }
}

/// Registers the next key plugged in that isn't in `enrollment`, watching
/// for one to arrive.
fn register_next(
    setup: &mut Setup<'_>,
    keyring: &Keyring,
    enrollment: &mut hatchling_vault::KeyEnrollment,
) -> Result<(), Error> {
    let mut said = None;
    let mut looks = 0;
    let mut refused = 0;
    loop {
        // One look: a key that isn't registered yet is taken from it.
        let (new, any) = {
            let mut keys = setup.devices.every_key();
            let any = !keys.is_empty();
            let new = keys
                .iter_mut()
                .position(|key| !enrollment.holds(&mut **key));
            (new, any)
        };
        if let Some(new) = new {
            match register(setup, keyring, enrollment, new) {
                Ok(()) => return Ok(()),
                Err(Refused::Stop(error)) => return Err(error),
                Err(Refused::Again(why)) => {
                    refused += 1;
                    setup.say(&why);
                    if refused >= TRIES {
                        return Err(ring::stopped());
                    }
                    // Time to swap the key, or get ready to touch it.
                    (setup.ask)("Press Enter to try again:")?;
                }
            }
            continue;
        }
        let plain = if any {
            "That's a key you've just registered. Plug in your other one."
        } else if setup.wsl {
            "Plug in a security key. In WSL, attach it too: usbipd attach --wsl --busid <id>"
        } else {
            "Plug in a security key."
        };
        if said != Some(plain) {
            setup.say(plain);
            said = Some(plain);
        }
        looks += 1;
        if looks == HINT_AFTER && !any {
            setup.say(not_seen(setup.wsl));
        }
        if looks >= LOOKS || !(setup.wait)() {
            return Err(Error::Usage {
                reason: format!(
                    "no key arrived ({}); run this again when you have it: nothing has changed",
                    setup.devices.unreachable()
                ),
            });
        }
    }
}

/// What registering a key asks of its owner.
const TWO_TOUCHES: &str = "Registering a key takes its PIN, then two touches.";

/// What to say of `touch`, with `left` of a registering's touches to come.
fn touch_line(touch: Touch, left: &Cell<u32>) -> String {
    match (touch, left.get()) {
        (Touch::Waiting, 0) | (Touch::Noticed, _) => ring::touch_said(touch).to_owned(),
        (Touch::Waiting, count) => {
            left.set(count - 1);
            format!(
                "Touch {} of 2: touch your key now, while it flashes...",
                3u32.saturating_sub(count)
            )
        }
    }
}

/// What to check when a key that's plugged in isn't seen.
const fn not_seen(wsl: bool) -> &'static str {
    if wsl {
        "Still nothing. In WSL a key shows up only once it's attached: in PowerShell, `usbipd list` should say Attached beside it. A new port has a new BUSID. If it isn't listed at all, try another port."
    } else {
        "Still nothing. Most keys flash when they get power. If yours didn't, push it in fully, or try another port, without a hub or adapter. It's rarely the key."
    }
}

/// Why one key wasn't registered.
enum Refused {
    /// Say this, and look again.
    Again(String),
    /// It can't go on.
    Stop(Error),
}

/// Registers the `index`th key plugged in: its PIN, then its touch, then
/// its name, since the touch is what shows which key this is.
fn register(
    setup: &mut Setup<'_>,
    keyring: &Keyring,
    enrollment: &mut hatchling_vault::KeyEnrollment,
    index: usize,
) -> Result<(), Refused> {
    let needs_pin = {
        let mut keys = setup.devices.every_key();
        let key = keys
            .get_mut(index)
            .ok_or_else(|| Refused::Again(String::from("That key went away.")))?;
        (key.pin_set(), key.min_pin_length())
    };
    match needs_pin {
        (Some(true), _) => {}
        (None, _) if !setup.config.require_key_pin => {}
        (None, _) => return Err(Refused::Again(String::from(TOO_OLD))),
        (Some(false), min) => set_pin(setup, index, min)?,
    }
    setup.say(TWO_TOUCHES);
    setup.touches.set(2);
    let (done, model) = {
        let mut keys = setup.devices.every_key();
        let key = keys
            .get_mut(index)
            .ok_or_else(|| Refused::Again(String::from("That key went away.")))?;
        (keyring.enroll_key(enrollment, &mut **key), key.model())
    };
    setup.touches.set(0);
    match done {
        Ok(()) => {}
        Err(hatchling_vault::Error::MissingHmacSecret) => {
            return Err(Refused::Again(String::from(TOO_OLD)));
        }
        Err(hatchling_vault::Error::Authenticator { reason }) => {
            return Err(Refused::Again(reason));
        }
        Err(hatchling_vault::Error::KeyChange { reason }) => {
            return Err(Refused::Again(reason.to_owned()));
        }
        Err(other) => return Err(Refused::Stop(ring::vault_error(other))),
    }
    let taken: Vec<String> = keyring
        .wraps()
        .iter()
        .map(|wrap| wrap.label().to_owned())
        .chain(enrollment.names())
        .collect();
    let suggested = unused_name(model.unwrap_or("Security key"), &taken);
    let label = name_of(setup, &suggested).map_err(Refused::Stop)?;
    enrollment.name_last(&label);
    setup.say(&format!("Registered: {}.", label.as_str()));
    Ok(())
}

/// Sets a PIN on the `index`th key, a new one with none.
fn set_pin(setup: &mut Setup<'_>, index: usize, min: usize) -> Result<(), Refused> {
    setup.say(&format!(
        "This key has no PIN yet. Choose one: at least {min} characters; 6 to 8 digits is plenty, as the key locks itself after 8 wrong tries. Don't use one you use elsewhere. It goes only to the key: Hatchling never keeps it."
    ));
    for _ in 0..TRIES {
        let pin = (setup.secret)("New PIN:").map_err(Refused::Stop)?;
        let again = (setup.secret)("The same PIN again:").map_err(Refused::Stop)?;
        if pin != again {
            setup.say("Those didn't match. Try again.");
            continue;
        }
        if !keyboard_only(&pin) {
            setup.say(KEYBOARD_ONLY);
            continue;
        }
        let set = {
            let mut keys = setup.devices.every_key();
            let key = keys
                .get_mut(index)
                .ok_or_else(|| Refused::Again(String::from("That key went away.")))?;
            key.set_pin(&pin)
        };
        match set {
            Ok(()) => return Ok(()),
            Err(refused) => setup.say(&refused.said()),
        }
    }
    Err(Refused::Stop(ring::stopped()))
}

/// What's said of a PIN only some keyboards can type.
const KEYBOARD_ONLY: &str = "Use letters, numbers and symbols from a US keyboard: other characters can be typed differently on another computer, and the key wouldn't recognize them.";

/// Whether a PIN can be typed the same on any machine: a key compares
/// bytes, and an accented letter isn't always the same ones.
fn keyboard_only(pin: &str) -> bool {
    pin.chars().all(|c| c.is_ascii_graphic())
}

/// `model`, or with a number if a key has that name: what tells two alike
/// apart later, and says which to remove if one is lost.
fn unused_name(model: &str, taken: &[String]) -> String {
    std::iter::once(model.to_owned())
        .chain((2..=taken.len() + 1).map(|number| format!("{model} {number}")))
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| model.to_owned())
}

/// The name of a key just registered: `suggested`, unless its owner has
/// one of their own.
fn name_of(setup: &mut Setup<'_>, suggested: &str) -> Result<Label, Error> {
    let answer = (setup.ask)(&format!(
        "Name this key, or press Enter to call it \"{suggested}\":"
    ))?;
    let name = if answer.trim().is_empty() {
        suggested
    } else {
        answer.as_str()
    };
    match name.parse::<Label>() {
        Ok(label) => Ok(label),
        Err(_) => ring::asked_label(setup.ask),
    }
}

/// Writes the marker, whose absence says the codes are confirmed.
fn mark_codes(path: &Path) -> Result<(), Error> {
    // As root, beside a keyring its service user owns: never through a
    // link planted at the name.
    hatchling_sys::hand_file_to(&codes_marker(path), hatchling_sys::euid())
        .map_err(|error| ring::io_error(std::io::Error::other(error)))
}

/// Shows `codes`, and has part of one typed back. Until it is, the marker
/// stays, and `doctor` and `unlock` go on saying so.
fn recovery_kit(setup: &mut Setup<'_>, path: &Path, codes: &BackupCodes) -> Result<(), Error> {
    mark_codes(path)?;
    let codes: Vec<&str> = codes.codes().collect();
    setup.say(
        "\nYour recovery kit. These backup codes are your way back in if you lose your keys. Each works once.\n\
         Save them now, away from this computer: on paper, or in a password manager.",
    );
    for (number, code) in codes.iter().enumerate() {
        setup.say(&format!("  {:2}. {code}", number + 1));
    }
    let number = codes.len();
    let Some(last) = codes.last() else {
        return Ok(());
    };
    let wanted: String = last
        .chars()
        .rev()
        .filter(|character| *character != '-')
        .take(4)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    for _ in 0..TRIES {
        let typed = (setup.ask)(&format!(
            "To check you've saved them, type the last four characters of code {number}:"
        ))?;
        if typed.trim().eq_ignore_ascii_case(&wanted) {
            std::fs::remove_file(codes_marker(path)).map_err(ring::io_error)?;
            setup.say("Saved. Keep them safe.");
            return Ok(());
        }
        setup.say("That doesn't match.");
    }
    setup.say(
        "Not confirmed: Hatchling will go on reminding you. Run this again to get a fresh set.",
    );
    Ok(())
}

/// What's left of a setup on a vault at the hardware-key tier.
fn carry_on(setup: &mut Setup<'_>, mut keyring: Keyring, path: &Path) -> Result<Done, Error> {
    let one_key = keyring.hardware_keys().len() == 1;
    let codes = codes_unconfirmed(path) || keyring.backup_codes_left() == 0;
    if !one_key && !codes {
        setup.say(&format!(
            "Already set up: {} security keys, and {} backup codes left.",
            keyring.hardware_keys().len(),
            keyring.backup_codes_left()
        ));
        return Ok(Done::AlreadySetUp);
    }
    let another = one_key && wants_to_add_a_key(setup)?;
    if !another && !codes {
        return Ok(Done::Protected);
    }
    let unlocked = ring::opening(&mut keyring, setup.ask, setup.devices)?;
    if another {
        add_second(setup, &mut keyring, &unlocked)?;
    }
    if codes {
        setup.say("Your backup codes were never confirmed saved: here's a fresh set.");
        mark_codes(path)?;
        let fresh = keyring
            .generate_backup_codes(&unlocked)
            .map_err(ring::vault_error)?;
        recovery_kit(setup, path, &fresh)?;
    }
    Ok(Done::Protected)
}

fn wants_to_add_a_key(setup: &mut Setup<'_>) -> Result<bool, Error> {
    let answer = (setup.ask)(
        "This vault has one key. If you lose it, only a backup code opens it. Add a second now? Type yes or no:",
    )?;
    Ok(answer.trim().eq_ignore_ascii_case("yes"))
}

/// Adds a second key to a vault with one.
fn add_second(
    setup: &mut Setup<'_>,
    keyring: &mut Keyring,
    unlocked: &Unlocked,
) -> Result<(), Error> {
    setup.say(SWAP);
    let mut said = false;
    for _ in 0..LOOKS {
        let new = {
            let mut keys = setup.devices.every_key();
            keys.iter_mut()
                .position(|key| keyring.enrolled_on(&mut **key) != Some(true))
        };
        if let Some(index) = new {
            let (needs_pin, model) = {
                let mut keys = setup.devices.every_key();
                let key = keys.get_mut(index);
                let model = key.as_ref().and_then(|key| key.model());
                (key.map(|key| (key.pin_set(), key.min_pin_length())), model)
            };
            if let Some((Some(false), min)) = needs_pin {
                set_pin(setup, index, min).map_err(|refused| match refused {
                    Refused::Again(why) => Error::Usage { reason: why },
                    Refused::Stop(error) => error,
                })?;
            }
            let taken: Vec<String> = keyring
                .wraps()
                .iter()
                .map(|wrap| wrap.label().to_owned())
                .collect();
            let suggested = unused_name(model.unwrap_or("Security key"), &taken);
            let label = name_of(setup, &suggested)?;
            let check = setup.config.key_check();
            setup.say(TWO_TOUCHES);
            setup.touches.set(2);
            let mut keys = setup.devices.every_key();
            let key = keys.get_mut(index).ok_or_else(ring::stopped)?;
            let key: &mut dyn Authenticator = &mut **key;
            keyring
                .add_hardware_key_checked(unlocked, key, &label, check)
                .map_err(ring::vault_error)?;
            drop(keys);
            setup.touches.set(0);
            setup.say(&format!("Registered: {}.", label.as_str()));
            return Ok(());
        }
        if !said {
            setup.say("Waiting for your other key.");
            said = true;
        }
        if !(setup.wait)() {
            break;
        }
    }
    Err(Error::Usage {
        reason: String::from(
            "no second key arrived; run this again when you have it: the vault is as it was",
        ),
    })
}

/// What `vault-keyring setup` does before the service's settings are taken
/// on.
///
/// It adds to them, when they have no keyring: its place, and a PIN
/// required of every key. And it stops the service, as moving the vault
/// gives it a new key a running broker wouldn't have.
///
/// # Errors
///
/// Why not, in a sentence.
pub fn prepare() -> Result<(), String> {
    use crate::environment as env;
    if hatchling_sys::euid() != 0 {
        return Err(String::from("run it with sudo"));
    }
    let path = Path::new(crate::doctor::SERVICE_SETTINGS);
    if !path.exists() {
        return Err(format!(
            "{} isn't there: setup is for a service install (docs/install.md)",
            path.display()
        ));
    }
    let text = crate::service_settings::read_trusted_text(path, 0)
        .map_err(|unread| format!("{}\n{}", unread.what, unread.fix))?;
    let settings = crate::doctor::parse_settings(&text);
    if !settings.contains_key(env::HATCHLING_VAULT_KEYRING.name) {
        let Some(root) = settings
            .get(env::HATCHLING_VAULT_DIR.name)
            .and_then(|vault| Path::new(vault).parent())
        else {
            return Err(String::from("the settings name no vault"));
        };
        let keyring = root.join("keyring").join("keyring");
        let changes = [
            (
                env::HATCHLING_VAULT_KEYRING.name,
                Some(keyring.display().to_string()),
            ),
            (
                env::HATCHLING_REQUIRE_KEY_PIN.name,
                Some(String::from("true")),
            ),
        ];
        crate::model_switch::rewrite(path, &text, &changes)?;
    }
    let stopped = Command::new("systemctl")
        .args(["stop", "hatchling"])
        .status()
        .map_err(|err| format!("can't stop the service: {err}"))?;
    if stopped.success() {
        Ok(())
    } else {
        Err(String::from(
            "can't stop the service: sudo systemctl stop hatchling",
        ))
    }
}

/// `vault-keyring setup`, in this terminal: the setup, then the service
/// started and unlocked.
///
/// # Errors
///
/// Why it stopped.
pub fn run_setup(
    broker: &crate::Config,
    config: &KeyringConfig,
    stores: &[(Store, PathBuf)],
    action: Option<&str>,
) -> Result<(), Error> {
    let mut asking =
        |prompt: &str| ring::terminal_answer(prompt, false).map(|answer| answer.to_string());
    let mut secret = |prompt: &str| ring::terminal_answer(prompt, true);
    let touches = Rc::new(Cell::new(0));
    let counted = Rc::clone(&touches);
    let mut devices = ring::platform_keys_noticing(
        Box::new(|| {
            ring::terminal_answer(ring::PIN_PROMPT, true)
                .ok()
                .filter(|pin| !pin.is_empty())
        }),
        Box::new(move |touch| {
            let _ = writeln!(std::io::stderr(), "{}", touch_line(touch, &counted));
        }),
    );
    let mut wait = || {
        std::thread::sleep(LOOK_EVERY);
        true
    };
    let mut out = std::io::stderr();
    let wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"));
    let guide = if action == Some(LOST_ACTION) {
        lost_key
    } else {
        run
    };
    let done = guide(&mut Setup {
        vault: &broker.vault,
        config,
        stores,
        ask: &mut asking,
        secret: &mut secret,
        devices: &mut devices,
        out: &mut out,
        wait: &mut wait,
        wsl,
        touches: &touches,
    });
    // Let go of the keys: each stays locked while it's held, and the unlock
    // below needs them.
    drop(devices);
    // A stopped keeper leaves its socket: gone now, it's only there again
    // once the new one is really waiting.
    let socket = crate::unlock_relay::socket_path(&broker.vault);
    let _ = std::fs::remove_file(&socket);
    // Whatever happened, the service goes back: an interrupted setup left
    // the vault as it was, or it says so.
    let started = Command::new("systemctl")
        .args(["start", "hatchling"])
        .status();
    done?;
    if !started.is_ok_and(|status| status.success()) {
        return Err(Error::Usage {
            reason: String::from("the service didn't start: sudo systemctl start hatchling"),
        });
    }
    // It's locked now, as after every restart: unlocking here shows the
    // key works, and shows how.
    for _ in 0..40 {
        if socket.exists() {
            break;
        }
        std::thread::sleep(LOOK_EVERY);
    }
    let _ = writeln!(
        std::io::stderr(),
        "\nAll set. Hatchling is locked, as it is after every restart. Unlock it now:"
    );
    let _ = crate::unlock_relay::unlock_main("hatchling-broker", &broker.vault);
    let _ = writeln!(
        std::io::stderr(),
        "{}",
        crate::windows_setup::here("After each restart, the same: sudo hatchling-broker unlock")
    );
    Ok(())
}

#[cfg(test)]
mod tests;
