//! The vault keyring at broker start.
//!
//! Without a keyring the broker behaves exactly as before: the key file
//! holds the master key (the DEK). With one, the DEK is wrapped per
//! unlock tier and never on disk unwrapped; the broker unlocks it at
//! start through the vault's current tier.
//!
//! The keyring settings are read here, from their own environment
//! variables, rather than added to [`VaultConfig`]: every `Config`
//! literal in the broker and its tests would otherwise change with them.
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `HATCHLING_VAULT_KEYRING` | Absolute path of the keyring. Unset: no keyring. |
//! | `HATCHLING_VAULT_KEK_FILE` | Absolute path of the file tier's KEK. Default `<vault root>.key/kek`. |
//! | `HATCHLING_REQUIRE_HARDWARE_KEY` | `true` or `1`: the broker starts only with a vault at the hardware-key tier (a fleet policy). Default `false`. |

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use hatchling_fido2::{Bus, PinPrompt};
use hatchling_vault::{
    Authenticator, BackupCodes, FileKey, Kek, KeyInfo, KeySource as _, Keyring, Label, MasterKey,
    Protection, RekeyReport, Rotation, SingleKeyRisk, Store, Tier, Unlocked, Warning,
};

use crate::environment as env;
use crate::{Error, VaultConfig};

/// The keyring settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyringConfig {
    /// The keyring path, if the vault uses one.
    pub keyring: Option<PathBuf>,
    /// The file tier's KEK, if not the default.
    pub kek_file: Option<PathBuf>,
    /// The fleet policy ( phase E): only a hardware-key vault may
    /// serve, and no tier below it may be enrolled.
    pub require_hardware_key: bool,
}

/// A strict boolean setting: `true`/`1` or `false`/`0`, nothing else.
fn flag(raw: Option<&str>, key: &'static str) -> Result<bool, Error> {
    match raw.map(str::trim) {
        None | Some("false" | "0") => Ok(false),
        Some("true" | "1") => Ok(true),
        Some(other) => Err(Error::InvalidConfig {
            key,
            reason: format!("must be true, 1, false or 0, not {other:?}"),
        }),
    }
}

/// The policy's refusal, naming what fell short.
fn policy(what: &str) -> Error {
    Error::Usage {
        reason: format!(
            "the fleet policy (HATCHLING_REQUIRE_HARDWARE_KEY) requires the hardware-key tier, and {what}"
        ),
    }
}

/// An optional absolute path setting.
fn absolute(raw: Option<String>, key: &'static str) -> Result<Option<PathBuf>, Error> {
    raw.map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(Error::InvalidConfig {
                    key,
                    reason: format!("must be an absolute path: {}", path.display()),
                })
            }
        })
        .transpose()
}

impl KeyringConfig {
    /// The settings from the environment.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] for a relative path.
    pub fn from_env() -> Result<Self, Error> {
        Self::parse(|key| std::env::var(key).ok())
    }

    /// The settings from `get`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] for a relative path.
    pub fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        Ok(Self {
            keyring: absolute(
                get(env::HATCHLING_VAULT_KEYRING.name),
                "HATCHLING_VAULT_KEYRING",
            )?,
            kek_file: absolute(
                get(env::HATCHLING_VAULT_KEK_FILE.name),
                "HATCHLING_VAULT_KEK_FILE",
            )?,
            require_hardware_key: flag(
                get(env::HATCHLING_REQUIRE_HARDWARE_KEY.name).as_deref(),
                "HATCHLING_REQUIRE_HARDWARE_KEY",
            )?,
        })
    }

    /// The file tier's KEK path: the setting, or `<vault root>.key/kek`,
    /// beside the master-key file's default.
    #[must_use]
    pub fn kek_file(&self, vault: &VaultConfig) -> PathBuf {
        self.kek_file
            .clone()
            .unwrap_or_else(|| vault.root.with_extension("key").join("kek"))
    }

    fn keyring_path(&self) -> Result<&Path, Error> {
        self.keyring.as_deref().ok_or_else(|| Error::Usage {
            reason: String::from("set HATCHLING_VAULT_KEYRING to the keyring's absolute path"),
        })
    }
}

const fn vault_error(source: hatchling_vault::Error) -> Error {
    Error::Vault { source }
}

const fn io_error(source: std::io::Error) -> Error {
    vault_error(hatchling_vault::Error::Io { source })
}

/// The file beside the key file that records that the vault's key is in
/// a keyring, so a start without `HATCHLING_VAULT_KEYRING` never mints a
/// plain key in its place.
fn keyring_marker(vault: &VaultConfig) -> PathBuf {
    vault.key_file.with_file_name("keyring-in-use")
}

/// Records, once, that `vault`'s key is in the keyring at `path`. Written
/// on adoption and on each keyring unlock, so vaults adopted before the
/// record existed get one too.
fn mark_keyring(vault: &VaultConfig, path: &Path) -> Result<(), Error> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let marker = keyring_marker(vault);
    if marker.parent().is_none_or(|dir| !dir.is_dir()) {
        return Ok(());
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&marker)
    {
        Ok(mut file) => file
            .write_all(format!("{}\n", path.display()).as_bytes())
            .map_err(io_error),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(io_error(source)),
    }
}

/// Refuses to create a plain key for a vault whose key is elsewhere: one
/// that has used a keyring, or whose store already holds sealed files.
fn refuse_new_key(vault: &VaultConfig) -> Result<(), Error> {
    let marker = keyring_marker(vault);
    let reason = if marker.try_exists().map_err(io_error)? {
        format!(
            "this vault's key is in a keyring (recorded in {}): set HATCHLING_VAULT_KEYRING to it; no plain key was created",
            marker.display()
        )
    } else if hatchling_vault::holds_envelopes(&vault.root).map_err(vault_error)? {
        format!(
            "{} holds sealed files but there is no key at {}: set HATCHLING_VAULT_KEYRING if the vault uses a keyring, or restore the key file; no plain key was created",
            vault.root.display(),
            vault.key_file.display()
        )
    } else {
        return Ok(());
    };
    Err(Error::Usage { reason })
}

/// Why a hardware-key vault did not open: neither a key nor a code.
const NO_KEY: &str = "this vault is at the hardware-key tier: plug in an enrolled security key, or give a backup code";

/// What the operator presents to unlock the vault.
#[derive(Default)]
pub struct Factors<'a> {
    /// A FIDO2 security key, for the hardware-key tier.
    pub authenticator: Option<&'a mut dyn Authenticator>,
    /// A one-time code from the authenticator app, for the TOTP tier.
    pub code: Option<String>,
    /// A backup code, to recover a hardware-key vault no key can reach.
    pub backup_code: Option<String>,
}

/// Asks for what the file and TOTP tiers need: a current code for the
/// TOTP tier, nothing for the file tier. A hardware-key vault is opened
/// through [`authorize`] (an enrolled key, or a backup code), so nothing
/// is asked here for it.
fn asked_factors(tier: Tier, ask: Ask<'_>) -> Result<Factors<'static>, Error> {
    Ok(match tier {
        Tier::File | Tier::HardwareKey => Factors::default(),
        Tier::Totp => Factors {
            code: Some(ask("Current code from your authenticator app:")?),
            ..Factors::default()
        },
    })
}

/// Unlocks `keyring` through its current tier: the KEK file at the file
/// tier, a current code at the TOTP tier, and an enrolled key or a backup
/// code at the hardware-key tier ([`authorize`]).
fn unlock_current(
    keyring: &mut Keyring,
    vault: &VaultConfig,
    config: &KeyringConfig,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<Unlocked, Error> {
    if keyring.tier() == Tier::HardwareKey {
        return authorize(keyring, ask, devices);
    }
    let factors = asked_factors(keyring.tier(), ask)?;
    open(keyring, vault, config, factors)
}

/// Unix seconds now, for one-time codes.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Asks the operator for one line: `prompt` on `output`, the answer
/// from `input`, without its line ending.
///
/// # Errors
///
/// Returns [`Error::Vault`] if `input` cannot be read.
pub fn ask(prompt: &str, input: &mut dyn BufRead, output: &mut dyn Write) -> Result<String, Error> {
    let _ = write!(output, "{prompt} ");
    let _ = output.flush();
    let mut line = String::new();
    input.read_line(&mut line).map_err(|source| Error::Vault {
        source: hatchling_vault::Error::Io { source },
    })?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// How the operator is asked for a line: the prompt in, the answer out.
pub type Ask<'a> = &'a mut dyn FnMut(&str) -> Result<String, Error>;

/// Opens `keyring` through its current tier.
fn open(
    keyring: &mut Keyring,
    vault: &VaultConfig,
    config: &KeyringConfig,
    factors: Factors<'_>,
) -> Result<Unlocked, Error> {
    match keyring.tier() {
        Tier::File => {
            let kek = Kek::from_file_key(
                &FileKey::new(config.kek_file(vault))
                    .load()
                    .map_err(vault_error)?,
            );
            keyring.unlock_file(&kek).map_err(vault_error)
        }
        Tier::Totp => {
            let code = factors.code.ok_or_else(|| Error::Usage { reason: String::from("this vault is at the TOTP tier: a current code from the authenticator app is needed") })?;
            keyring.unlock_totp(&code, now()).map_err(vault_error)
        }
        Tier::HardwareKey => match (factors.authenticator, factors.backup_code) {
            (Some(key), _) => keyring.unlock_hardware_key(key).map_err(vault_error),
            (None, Some(code)) => keyring.recover(&code).map_err(vault_error),
            (None, None) => Err(Error::Usage {
                reason: String::from(NO_KEY),
            }),
        },
    }
}

/// The master key (the DEK) for serving.
///
/// Without a keyring it comes from the key file, exactly as before; with
/// one, through the vault's tier. A TOTP-tier vault asks for a code, and a
/// hardware-key vault for a touch of an enrolled security key (or a
/// backup code), on the terminal (stderr), reading from stdin: both need
/// an interactive start.
///
/// # Errors
///
/// Returns [`Error::Usage`] if the tier needs a factor this start cannot
/// present, or [`Error::Vault`] if the key cannot be loaded or the vault
/// does not unlock.
pub fn unlock(vault: &VaultConfig, config: &KeyringConfig) -> Result<MasterKey, Error> {
    let (mut asking, mut devices) = terminal_operator();
    unlock_asking(vault, config, &mut asking, &mut devices)
}

/// The operator at this terminal: their answers to prompts, and the
/// platform's security keys, asking for a key's PIN on the same terminal.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the console moves into the returned prompt and lives exactly as long as it does"
)]
fn terminal_operator() -> (impl FnMut(&str) -> Result<String, Error>, impl Devices) {
    let console = Console::terminal();
    let devices = platform_keys(pin_prompt(&console));
    (
        move |prompt: &str| console.borrow_mut().ask(prompt),
        devices,
    )
}

/// [`unlock`], asking with `ask` for what the vault's tier needs: a
/// current code at the TOTP tier, an enrolled key from `devices` or a
/// backup code at the hardware-key tier, and nothing otherwise.
///
/// # Errors
///
/// As [`unlock`].
pub fn unlock_asking(
    vault: &VaultConfig,
    config: &KeyringConfig,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<MasterKey, Error> {
    let loaded = config
        .keyring
        .as_deref()
        .and_then(|path| Keyring::load(path).ok());
    match (loaded, config.keyring.as_deref()) {
        (Some(mut keyring), Some(path)) if keyring.tier() == Tier::HardwareKey => {
            let unlocked = authorize(&mut keyring, ask, devices)?;
            mark_keyring(vault, path)?;
            settled(&keyring)?;
            Ok(unlocked.into_key())
        }
        (loaded, _) => {
            let tier = loaded.map_or(Tier::File, |keyring| keyring.tier());
            unlock_with(vault, config, asked_factors(tier, ask)?)
        }
    }
}

/// [`unlock`], with the factors the operator presents.
///
/// # Errors
///
/// As [`unlock`].
pub fn unlock_with(
    vault: &VaultConfig,
    config: &KeyringConfig,
    factors: Factors<'_>,
) -> Result<MasterKey, Error> {
    let Some(path) = config.keyring.as_deref() else {
        if config.require_hardware_key {
            return Err(policy("this vault has no keyring"));
        }
        if !vault.key_file.try_exists().map_err(io_error)? {
            refuse_new_key(vault)?;
        }
        return FileKey::new(vault.key_file.clone())
            .load_or_create()
            .map_err(vault_error);
    };
    let mut keyring = Keyring::load(path).map_err(vault_error)?;
    if config.require_hardware_key && keyring.tier() != Tier::HardwareKey {
        return Err(policy("this vault is at a lower tier"));
    }
    let unlocked = open(&mut keyring, vault, config, factors)?;
    mark_keyring(vault, path)?;
    settled(&keyring)?;
    Ok(unlocked.into_key())
}

/// What a rotation did: each store's re-seal, and the backup codes the new
/// key brought, if the vault had codes.
pub type Rekeyed = (Vec<(&'static str, RekeyReport)>, Option<BackupCodes>);

/// Refuses a keyring whose rotation is unfinished: some stores may still
/// be sealed under the retiring key, so nothing is served, migrated or
/// read until `rekey-vault` finishes it.
const fn settled(keyring: &Keyring) -> Result<(), Error> {
    if keyring.rotation_pending() {
        return Err(vault_error(hatchling_vault::Error::RotationUnfinished));
    }
    Ok(())
}

/// Finishes a rotation the keyring has committed (WO-003): once the vault
/// has left the file tier its KEK file goes, every store moves from the
/// old key to the new, and the retiring key is dropped. Each step is safe
/// to repeat, so `rekey-vault` resumes here after a crash.
fn finish(
    keyring: &mut Keyring,
    rotation: &Rotation,
    stores: &[(Store, PathBuf)],
    kek_file: &Path,
) -> Result<Vec<(&'static str, RekeyReport)>, Error> {
    if keyring.tier() != Tier::File {
        match std::fs::remove_file(kek_file) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(io_error(err)),
            _ => {}
        }
    }
    let reports = crate::reseal_stores(stores, rotation.old(), rotation.new_key())?;
    keyring.finish_rotation(rotation).map_err(vault_error)?;
    Ok(reports)
}

/// Rotates the vault's key inside its keyring (`rekey-vault` with a
/// keyring), or finishes a rotation a crash left unfinished.
///
/// Unlocks through the current tier, then wraps a fresh key for every
/// way in at that tier: the file tier's KEK file, the TOTP tier's seed,
/// or every enrolled security key, each plugged in and touched. Backup
/// codes are replaced by a new set. Every store is then re-sealed under
/// the new key, and nothing opens under the old one.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path, under the fleet
/// policy below the hardware-key tier, or if the tier needs a factor this
/// start cannot present; or [`Error::Vault`] if the vault does not
/// unlock, a store holds stale files, a key does not answer, or a step
/// fails, in which case re-running finishes it.
pub fn rekey(
    vault: &VaultConfig,
    config: &KeyringConfig,
    stores: &[(Store, PathBuf)],
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<Rekeyed, Error> {
    let mut keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    if config.require_hardware_key && keyring.tier() != Tier::HardwareKey {
        return Err(policy("this vault is at a lower tier"));
    }
    let unlocked = unlock_current(&mut keyring, vault, config, ask, devices)?;
    let (rotation, codes) = if let Some(rotation) =
        keyring.pending_rotation(&unlocked).map_err(vault_error)?
    {
        (rotation, None)
    } else {
        crate::check_sealed_stores(stores)?;
        let file_kek = match keyring.tier() {
            Tier::File => Some(Kek::from_file_key(
                &FileKey::new(config.kek_file(vault))
                    .load()
                    .map_err(vault_error)?,
            )),
            Tier::Totp | Tier::HardwareKey => None,
        };
        if keyring.tier() == Tier::HardwareKey {
            ask("Plug in every enrolled security key, press Enter, then touch each one when it flashes:")?;
        }
        let mut keys = devices.every_key();
        let rotated = keyring
            .rotate_in_place(&unlocked, file_kek.as_ref(), &mut keys)
            .map_err(vault_error)?;
        (rotated.rotation, rotated.codes)
    };
    let reports = finish(&mut keyring, &rotation, stores, &config.kek_file(vault))?;
    Ok((reports, codes))
}

/// [`rekey`] at this terminal, with the keyring settings from the
/// environment.
///
/// # Errors
///
/// A bad keyring setting, or as [`rekey`].
pub fn run_rekey(
    vault: &VaultConfig,
    config: &KeyringConfig,
    stores: &[(Store, PathBuf)],
) -> Result<Rekeyed, Error> {
    let (mut asking, mut devices) = terminal_operator();
    rekey(vault, config, stores, &mut asking, &mut devices)
}

/// Adopts a keyring (`vault-keyring init`).
///
/// The master key in the key file becomes the DEK, wrapped at the file
/// tier under the KEK file (created if absent), and the key file is
/// removed once the keyring is written and unlocks. Nothing in the
/// vault is re-encrypted.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path, or [`Error::Vault`]
/// if the key file is absent, the keyring exists, or anything fails;
/// the key file is kept unless the keyring unlocked.
pub fn init(vault: &VaultConfig, config: &KeyringConfig) -> Result<(), Error> {
    let path = config.keyring_path()?;
    let dek = FileKey::new(vault.key_file.clone())
        .load()
        .map_err(vault_error)?;
    let kek = Kek::from_file_key(
        &FileKey::new(config.kek_file(vault))
            .load_or_create()
            .map_err(vault_error)?,
    );
    Keyring::create(path, &dek, &kek).map_err(vault_error)?;
    Keyring::load(path)
        .and_then(|keyring| keyring.unlock_file(&kek))
        .map_err(vault_error)?;
    mark_keyring(vault, path)?;
    std::fs::remove_file(&vault.key_file).map_err(io_error)
}

/// How many codes `enroll-totp` takes before it gives up.
const CONFIRM_ATTEMPTS: usize = 3;

/// Moves the vault to the TOTP tier (`vault-keyring enroll-totp`).
///
/// Unlocks through the current tier (asking for a current code first if
/// it is already TOTP), shows the `otpauth://` URI once in a prompt, and
/// switches only when a code from the app matches. A wrong code may be
/// tried again, up to three in all. The vault moves to a fresh key: every
/// store in `stores` is re-sealed under it, and any backup codes are
/// replaced by the new set returned.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path or if the current tier
/// needs a factor this build cannot present, or [`Error::Vault`] if a
/// store holds stale files, the vault does not unlock, no code matched,
/// or re-sealing fails (then `rekey-vault` finishes it).
pub fn enroll_totp(
    vault: &VaultConfig,
    config: &KeyringConfig,
    stores: &[(Store, PathBuf)],
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<Option<BackupCodes>, Error> {
    if config.require_hardware_key {
        return Err(policy("TOTP is below it"));
    }
    crate::check_sealed_stores(stores)?;
    let mut keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    let unlocked = unlock_current(&mut keyring, vault, config, ask, devices)?;
    let pending = keyring.begin_totp(&unlocked).map_err(vault_error)?;
    let mut prompt = format!(
        "Add this to your authenticator app (it is the seed: keep no other copy), then enter the code it shows:\n{}\n>",
        pending.uri()
    );
    for _ in 0..CONFIRM_ATTEMPTS {
        match keyring.confirm_totp(&unlocked, &pending, &ask(&prompt)?, now()) {
            Err(hatchling_vault::Error::CodeRejected) => {
                prompt = String::from("That code did not match. Enter the next one the app shows:");
            }
            outcome => {
                let rotated = outcome.map_err(vault_error)?;
                finish(&mut keyring, &rotated.rotation, stores, &config.kek_file(vault))?;
                return Ok(rotated.codes);
            }
        }
    }
    Err(vault_error(hatchling_vault::Error::CodeRejected))
}

/// A new set of backup codes (`vault-keyring backup-codes`), replacing any
/// earlier set, after unlocking through the current tier.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path or if the current tier
/// needs a factor this build cannot present, or [`Error::Vault`] if the
/// vault does not unlock or the codes cannot be saved.
pub fn backup_codes(
    vault: &VaultConfig,
    config: &KeyringConfig,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<BackupCodes, Error> {
    let mut keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    let unlocked = unlock_current(&mut keyring, vault, config, ask, devices)?;
    keyring
        .generate_backup_codes(&unlocked)
        .map_err(vault_error)
}

/// Where the CLI reaches security keys: [`SecurityKeys`] over
/// the platform's transport ; tests plug in software keys.
pub trait Devices {
    /// The one security key plugged in now, or `None` if none (or more
    /// than one) can be reached.
    fn plugged_in(&mut self) -> Option<&mut dyn Authenticator>;

    /// Every security key plugged in now.
    fn every_key(&mut self) -> Vec<&mut dyn Authenticator> {
        self.plugged_in().into_iter().collect()
    }

    /// Why no key was reached, in words.
    fn unreachable(&self) -> String {
        String::from(NONE_PLUGGED)
    }
}

/// What [`Devices::unreachable`] says when nothing is plugged in.
const NONE_PLUGGED: &str = "no security key is plugged in";

/// No security key can be reached.
#[derive(Debug, Default)]
pub struct NoKeys;

impl Devices for NoKeys {
    fn plugged_in(&mut self) -> Option<&mut dyn Authenticator> {
        None
    }
}

/// The security keys a [`Bus`] reaches, as the CLI's [`Devices`]
/// .
///
/// Each step looks again at what is plugged in. A step that needs one key
/// needs exactly one plugged in: two identical keys are never told apart
/// by position, so with two in, none is used. Enrolling two keys at once
/// takes both ([`Devices::every_key`]): two open devices are two keys.
pub struct SecurityKeys<B: Bus> {
    bus: B,
    keys: Vec<B::Key>,
    why: String,
}

impl<B: Bus> SecurityKeys<B> {
    /// The keys `bus` reaches.
    pub fn new(bus: B) -> Self {
        Self {
            bus,
            keys: Vec::new(),
            why: String::from(NONE_PLUGGED),
        }
    }

    /// Looks at what is plugged in now.
    fn scan(&mut self) {
        match self.bus.keys() {
            Ok(keys) => {
                self.why = match keys.len() {
                    0 => String::from(NONE_PLUGGED),
                    1 => String::new(),
                    many => {
                        format!("{many} security keys are plugged in: unplug all but one")
                    }
                };
                self.keys = keys;
            }
            Err(why) => {
                self.keys.clear();
                self.why = why;
            }
        }
    }
}

impl<B: Bus> Devices for SecurityKeys<B> {
    fn plugged_in(&mut self) -> Option<&mut dyn Authenticator> {
        self.scan();
        if self.keys.len() != 1 {
            return None;
        }
        let key: &mut dyn Authenticator = self.keys.first_mut()?;
        Some(key)
    }

    fn every_key(&mut self) -> Vec<&mut dyn Authenticator> {
        self.scan();
        self.keys
            .iter_mut()
            .map(|key| -> &mut dyn Authenticator { key })
            .collect()
    }

    fn unreachable(&self) -> String {
        self.why.clone()
    }
}

/// The platform's security keys: `hidraw` (the broker builds on Linux
/// only), asking for a key's PIN with `pin` when one needs it.
#[must_use]
pub fn platform_keys(pin: PinPrompt) -> SecurityKeys<hatchling_fido2::linux::Hidraw> {
    SecurityKeys::new(hatchling_fido2::linux::Hidraw::new(pin))
}

/// Where the operator answers: the terminal (answers from stdin, prompts
/// on stderr), shared between the prompts and the PIN prompt.
struct Console<R, W> {
    input: R,
    output: W,
}

impl Console<std::io::StdinLock<'static>, std::io::Stderr> {
    /// The terminal.
    fn terminal() -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            input: std::io::stdin().lock(),
            output: std::io::stderr(),
        }))
    }
}

impl<R: BufRead, W: Write> Console<R, W> {
    /// One answer to `prompt`.
    fn ask(&mut self, prompt: &str) -> Result<String, Error> {
        ask(prompt, &mut self.input, &mut self.output)
    }
}

/// How a key's PIN is asked for.
const PIN_PROMPT: &str =
    "This security key has a PIN. Enter it (it goes only to the key), or nothing to stop:";

/// A PIN prompt on `console`: an empty answer is no PIN.
fn pin_prompt<R: BufRead + 'static, W: Write + 'static>(
    console: &Rc<RefCell<Console<R, W>>>,
) -> PinPrompt {
    let console = Rc::clone(console);
    Box::new(move || {
        console
            .borrow_mut()
            .ask(PIN_PROMPT)
            .ok()
            .filter(|pin| !pin.is_empty())
            .map(zeroize::Zeroizing::new)
    })
}

/// Why `add-key` cannot run now.
fn no_key_to_add(why: &str) -> String {
    format!(
        "no security key can be reached ({why}), so none can be added; nothing was asked or changed"
    )
}

/// How a change to the keys is authorized when a key can be reached.
const AUTHORIZE_PROMPT: &str = "To authorize, plug in a security key that is already enrolled and press Enter, or type a backup code (it is used up):";

/// How it is authorized when none can: a backup code.
const AUTHORIZE_CODE_PROMPT: &str = "No security key can be reached. To authorize, type a backup code (it is used up), or nothing to stop:";

/// What stops a guided flow without a change.
fn stopped() -> Error {
    Error::Usage {
        reason: String::from("stopped; nothing was changed"),
    }
}

/// Unlocks `keyring` to authorize a change to its keys: the operator's
/// answer to `prompt` is a backup code, or nothing to use the security
/// key plugged in. Without a reachable key only a backup code can.
fn authorize(
    keyring: &mut Keyring,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<Unlocked, Error> {
    match authorization(ask, devices)? {
        Some(code) => keyring.recover(&code).map_err(vault_error),
        None => by_key(keyring, devices),
    }
}

/// How the operator authorizes a change to the keys, asked once: a
/// backup code (`Some`, not yet checked or spent), or `None` for the
/// security key plugged in.
fn authorization(ask: Ask<'_>, devices: &mut dyn Devices) -> Result<Option<String>, Error> {
    let reachable = devices.plugged_in().is_some();
    let answer = ask(if reachable {
        AUTHORIZE_PROMPT
    } else {
        AUTHORIZE_CODE_PROMPT
    })?;
    let code = answer.trim();
    Ok((!code.is_empty()).then(|| code.to_owned()))
}

/// Unlocks `keyring` with the security key plugged in, or stops if none
/// can be reached.
fn by_key(keyring: &Keyring, devices: &mut dyn Devices) -> Result<Unlocked, Error> {
    let key = devices.plugged_in().ok_or_else(stopped)?;
    keyring.unlock_hardware_key(key).map_err(vault_error)
}

/// A key's line: its id, its label, and when it was added.
fn key_line(key: &KeyInfo) -> String {
    let label = if key.label.is_empty() {
        "(no label)"
    } else {
        key.label.as_str()
    };
    let added = key.added.and_then(utc_date).map_or_else(
        || String::from("added before the keyring kept dates"),
        |date| format!("added {date}"),
    );
    format!("{}  {label}  {added}", key.id)
}

/// `seconds` since the epoch as a UTC date, `YYYY-MM-DD`.
fn utc_date(seconds: u64) -> Option<String> {
    let at = hatchling_scheduler::Timestamp::from_second(i64::try_from(seconds).ok()?).ok()?;
    at.to_string().get(..10).map(str::to_owned)
}

/// The enrolled keys, one per line, for a prompt.
fn key_list(keys: &[KeyInfo]) -> String {
    keys.iter()
        .map(|key| format!("  {}", key_line(key)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A label, asked for until one is valid, up to three times.
fn asked_label(ask: Ask<'_>) -> Result<Label, Error> {
    let mut prompt = String::from(
        "Name the new key, for example \"YubiKey 5C, desk\" (only you see this name):",
    );
    for _ in 0..CONFIRM_ATTEMPTS {
        match ask(&prompt)?.parse::<Label>() {
            Ok(label) => return Ok(label),
            Err(error) => prompt = format!("{error}. Try another name:"),
        }
    }
    Err(stopped())
}

/// Adds a security key (`vault-keyring add-key`) to a vault at the
/// hardware-key tier.
///
/// The operator names it, confirms, authorizes with an enrolled key or a
/// backup code, then plugs in the new key. Nothing is asked when no key can
/// be reached, so no backup code is spent for nothing.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path, below the hardware-key
/// tier, without a reachable key, or when the operator stops; or
/// [`Error::Vault`] if the vault does not unlock or the key is refused.
pub fn add_key(
    config: &KeyringConfig,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<KeyInfo, Error> {
    let mut keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    if keyring.tier() != Tier::HardwareKey {
        return Err(Error::Usage {
            reason: String::from(
                "keys are added to a vault at the hardware-key tier; this vault is below it, and that tier is enrolled with two keys at once",
            ),
        });
    }
    if devices.plugged_in().is_none() {
        return Err(Error::Usage {
            reason: no_key_to_add(&devices.unreachable()),
        });
    }
    let label = asked_label(ask)?;
    let confirm = format!(
        "Add a security key named \"{}\" to this vault? Type yes to go on:",
        label.as_str()
    );
    if ask(&confirm)?.trim() != "yes" {
        return Err(stopped());
    }
    let unlocked = authorize(&mut keyring, ask, devices)?;
    ask("Now plug in the new security key (only it: unplug any other) and press Enter:")?;
    let device = devices.plugged_in().ok_or_else(stopped)?;
    keyring
        .add_hardware_key(&unlocked, device, &label)
        .map_err(vault_error)
}

/// The enrolled key the operator names, by id: never by label.
fn chosen_key(keys: &[KeyInfo], what: &str, ask: Ask<'_>) -> Result<KeyInfo, Error> {
    if keys.is_empty() {
        return Err(Error::Usage {
            reason: String::from("this vault has no security keys"),
        });
    }
    let prompt = format!(
        "Enrolled security keys:\n{}\nType the id of the key to {what}:",
        key_list(keys)
    );
    let id = ask(&prompt)?;
    keys.iter()
        .find(|key| key.id == id.trim())
        .cloned()
        .ok_or_else(|| vault_error(hatchling_vault::Error::NoSuchKey))
}

/// Removes a security key (`vault-keyring remove-key`), never the last.
///
/// The operator chooses it by id, confirms (and accepts the risk if one
/// key would be left), then authorizes with a *different* enrolled key or
/// a backup code. Whatever can be refused is refused before anything is
/// asked to authorize, and a backup code given to authorize is spent only
/// if the key goes: the vault decides every refusal before it writes.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path or keys, for the last
/// key, for one key left without backup codes, when only the last backup
/// code is left and no other key can be reached, or when the operator stops;
/// or [`Error::Vault`] if the vault does not unlock or the removal is
/// refused (the key authorizing its own removal, among others).
pub fn remove_key(
    config: &KeyringConfig,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<KeyInfo, Error> {
    let mut keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    let keys = keyring.hardware_keys();
    let key = chosen_key(&keys, "remove", ask)?;
    let risk = match keys.len() {
        0 | 1 => {
            return Err(Error::Usage {
                reason: String::from(
                    "it is the last security key, and removing it would leave nothing to open the vault: change the vault's tier first",
                ),
            });
        }
        2 => {
            if keyring.backup_codes_left() == 0 {
                return Err(Error::Usage {
                    reason: String::from(
                        "removing it would leave one security key and no backup codes: make backup codes first",
                    ),
                });
            }
            if keyring.backup_codes_left() == 1 && devices.plugged_in().is_none() {
                return Err(Error::Usage {
                    reason: format!(
                        "only another enrolled security key can authorize this: your last backup code would leave one key and no codes, and no key can be reached ({}); nothing was asked or changed",
                        devices.unreachable()
                    ),
                });
            }
            let accept = "Removing it leaves one security key: if that key is lost, only a backup code opens the vault. Type \"one key\" to accept that:";
            if ask(accept)?.trim() != "one key" {
                return Err(stopped());
            }
            Some(SingleKeyRisk::accepted())
        }
        _ => None,
    };
    let confirm = format!(
        "Remove the security key {}? Type yes to go on:",
        key_line(&key)
    );
    if ask(&confirm)?.trim() != "yes" {
        return Err(stopped());
    }
    // A backup code is spent only if the key goes ( flag 5): the
    // vault decides every refusal before it writes either.
    let removed = if let Some(code) = authorization(ask, devices)? {
        keyring.remove_hardware_key_with_code(&code, &key.id, risk)
    } else {
        let unlocked = by_key(&keyring, devices)?;
        keyring.remove_hardware_key(&unlocked, &key.id, risk)
    };
    removed.map_err(vault_error)
}

/// Names a security key (`vault-keyring label-key`): choose it by id,
/// name it, then authorize with an enrolled key or a backup code.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path or keys, or when the
/// operator stops; or [`Error::Vault`] if the vault does not unlock.
pub fn label_key(
    config: &KeyringConfig,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<KeyInfo, Error> {
    let mut keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    let key = chosen_key(&keyring.hardware_keys(), "name", ask)?;
    let label = asked_label(ask)?;
    let unlocked = authorize(&mut keyring, ask, devices)?;
    keyring
        .label_key(&unlocked, &key.id, &label)
        .map_err(vault_error)?;
    Ok(KeyInfo {
        label: label.as_str().to_owned(),
        ..key
    })
}

/// Moves the vault to the hardware-key tier (`vault-keyring enroll-keys`).
///
/// Two keys plugged in at once (either then opens the vault), or one key
/// with backup codes as the way back and the risk accepted. Unlocks
/// through the current tier first; every other way in is dropped.
/// Whatever can be refused is refused before that.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path, at the hardware-key
/// tier already, without the keys or codes it needs, or when the operator
/// stops; or [`Error::Vault`] if the vault does not unlock or a key is
/// refused (the vault is then unchanged).
pub fn enroll_keys(
    vault: &VaultConfig,
    config: &KeyringConfig,
    stores: &[(Store, PathBuf)],
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<(usize, Option<BackupCodes>), Error> {
    crate::check_sealed_stores(stores)?;
    let mut keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    if keyring.tier() == Tier::HardwareKey {
        return Err(Error::Usage {
            reason: String::from(
                "this vault is already at the hardware-key tier: add a key with vault-keyring add-key",
            ),
        });
    }
    let how = ask(
        "Enroll two security keys (recommended: either one opens the vault), or one key with your backup codes as the way back? Type two or one:",
    )?;
    match how.trim() {
        "two" => {
            ask("Plug in both security keys and press Enter:")?;
            let found = devices.every_key().len();
            if found != 2 {
                return Err(Error::Usage {
                    reason: format!(
                        "two security keys are needed, plugged in at once; {found} can be reached ({})",
                        devices.unreachable()
                    ),
                });
            }
            let unlocked = unlock_current(&mut keyring, vault, config, ask, devices)?;
            let mut keys = devices.every_key();
            let rotated = keyring
                .enroll_hardware_keys(&unlocked, &mut keys)
                .map_err(vault_error)?;
            finish(&mut keyring, &rotated.rotation, stores, &config.kek_file(vault))?;
            Ok((2, rotated.codes))
        }
        "one" => {
            if keyring.backup_codes_left() == 0 {
                return Err(Error::Usage {
                    reason: String::from(
                        "one key needs backup codes as the way back: make them first (vault-keyring backup-codes)",
                    ),
                });
            }
            let accept = "With one key, losing it leaves only a backup code to open the vault. Type \"one key\" to accept that:";
            if ask(accept)?.trim() != "one key" {
                return Err(stopped());
            }
            ask("Plug in the security key (only it) and press Enter:")?;
            if devices.plugged_in().is_none() {
                return Err(Error::Usage {
                    reason: format!("no security key can be reached ({})", devices.unreachable()),
                });
            }
            let unlocked = unlock_current(&mut keyring, vault, config, ask, devices)?;
            let key = devices.plugged_in().ok_or_else(stopped)?;
            let rotated = keyring
                .enroll_single_hardware_key(&unlocked, key, SingleKeyRisk::accepted())
                .map_err(vault_error)?;
            finish(&mut keyring, &rotated.rotation, stores, &config.kek_file(vault))?;
            Ok((1, rotated.codes))
        }
        _ => Err(stopped()),
    }
}

/// Lowers the vault to the file tier (`vault-keyring use-file-tier`).
///
/// Its key then sits in the KEK file on this machine, and the current
/// tier's ways in stop working. Needs the current tier's own unlock: a
/// current code at the TOTP tier, an enrolled key at the hardware-key
/// tier (a backup code never lowers the tier).
///
/// # Errors
///
/// Returns [`Error::Usage`] under the fleet policy, without a keyring
/// path, at the file tier already, or when the operator stops; or
/// [`Error::Vault`] if the vault does not unlock or the change is refused.
pub fn use_file_tier(
    vault: &VaultConfig,
    config: &KeyringConfig,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<(), Error> {
    if config.require_hardware_key {
        return Err(policy("the file tier is below it"));
    }
    let mut keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    if keyring.tier() == Tier::File {
        return Err(Error::Usage {
            reason: String::from("this vault is already at the file tier"),
        });
    }
    let confirm = "Lower the vault to the file tier? Its key then sits in a file on this machine, and the current tier's ways in stop working. Type yes to go on:";
    if ask(confirm)?.trim() != "yes" {
        return Err(stopped());
    }
    let unlocked = if keyring.tier() == Tier::HardwareKey {
        lowering_key(&keyring, ask, devices)?
    } else {
        unlock_current(&mut keyring, vault, config, ask, devices)?
    };
    let kek = Kek::from_file_key(
        &FileKey::new(config.kek_file(vault))
            .load_or_create()
            .map_err(vault_error)?,
    );
    keyring.use_file_tier(&unlocked, &kek).map_err(vault_error)
}

/// The unlock that lowers a hardware-key vault: a touch of an enrolled
/// key, never a backup code (which the vault refuses for lowering, after
/// spending it). Without a reachable key it refuses, and nothing is spent.
fn lowering_key(
    keyring: &Keyring,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
) -> Result<Unlocked, Error> {
    if devices.plugged_in().is_none() {
        return Err(Error::Usage {
            reason: format!(
                "lowering the tier needs an enrolled security key (a backup code never lowers it), and none can be reached ({}); nothing was spent",
                devices.unreachable()
            ),
        });
    }
    ask("Plug in an enrolled security key (only it), press Enter, then touch it when it flashes:")?;
    let key = devices.plugged_in().ok_or_else(stopped)?;
    keyring.unlock_hardware_key(key).map_err(vault_error)
}

/// Runs one `vault-keyring` action ([`USAGE`] lists them).
///
/// Asks with `ask`, reaches security keys through `devices`, and reports
/// on `out`.
///
/// # Errors
///
/// Returns [`Error::Usage`] for a missing or unknown action, or the
/// action's own error.
pub fn command(
    vault: &VaultConfig,
    config: &KeyringConfig,
    stores: &[(Store, PathBuf)],
    action: Option<&str>,
    ask: Ask<'_>,
    devices: &mut dyn Devices,
    out: &mut dyn Write,
) -> Result<(), Error> {
    match action {
        Some("init") => {
            init(vault, config)?;
            let _ = writeln!(
                out,
                "vault-keyring: adopted at the file tier; the plain key file is removed"
            );
        }
        Some("enroll-totp") => {
            let codes = enroll_totp(vault, config, stores, ask, devices)?;
            let _ = writeln!(
                out,
                "vault-keyring: the vault is at the TOTP tier, under a new key; every store is re-sealed"
            );
            new_codes(out, codes.as_ref());
        }
        Some("enroll-keys") => {
            let (keys, codes) = enroll_keys(vault, config, stores, ask, devices)?;
            let _ = writeln!(
                out,
                "vault-keyring: the vault is at the hardware-key tier with {keys} security key(s), under a new key; every store is re-sealed. Name the keys with vault-keyring label-key"
            );
            new_codes(out, codes.as_ref());
        }
        Some("use-file-tier") => {
            use_file_tier(vault, config, ask, devices)?;
            let _ = writeln!(
                out,
                "vault-keyring: the vault is at the file tier; its key is in the KEK file on this machine"
            );
        }
        Some("backup-codes") => {
            let codes = backup_codes(vault, config, ask, devices)?;
            let _ = writeln!(
                out,
                "vault-keyring: new backup codes (any earlier set no longer works). Each works once; keep them apart from this machine:"
            );
            for code in codes.codes() {
                let _ = writeln!(out, "  {code}");
            }
        }
        Some("status") => {
            let _ = writeln!(out, "{}", report(config)?);
        }
        Some("add-key") => {
            let key = add_key(config, ask, devices)?;
            let _ = writeln!(
                out,
                "vault-keyring: added the security key {}",
                key_line(&key)
            );
        }
        Some("remove-key") => {
            let key = remove_key(config, ask, devices)?;
            let _ = writeln!(
                out,
                "vault-keyring: removed the security key {}",
                key_line(&key)
            );
        }
        Some("label-key") => {
            let key = label_key(config, ask, devices)?;
            let _ = writeln!(
                out,
                "vault-keyring: named the security key {}",
                key_line(&key)
            );
        }
        _ => {
            return Err(Error::Usage {
                reason: String::from(USAGE),
            });
        }
    }
    Ok(())
}

/// Shows the backup codes a new key brought, if the vault had codes.
pub fn new_codes(out: &mut dyn Write, codes: Option<&BackupCodes>) {
    if let Some(codes) = codes {
        let _ = writeln!(
            out,
            "vault-keyring: new backup codes for the new key (the earlier set no longer works). Each works once; keep them apart from this machine:"
        );
        for code in codes.codes() {
            let _ = writeln!(out, "  {code}");
        }
    }
}

/// The actions `vault-keyring` takes.
pub const USAGE: &str = "usage: hatchling-broker vault-keyring <init | enroll-totp | enroll-keys | use-file-tier | backup-codes | status | add-key | remove-key | label-key>";

/// Runs `hatchling-broker vault-keyring <action>`: the keyring settings
/// from the environment, answers from stdin with the prompts on stderr,
/// and the report on stdout.
///
/// # Errors
///
/// A bad keyring setting, or as [`command`].
pub fn run_keyring(broker: &crate::Config, action: Option<&str>) -> Result<(), Error> {
    let config = KeyringConfig::from_env()?;
    let stores = crate::sealed_stores(broker, crate::vault_ops::redaction_config(broker)?.as_ref());
    let (mut asking, mut devices) = terminal_operator();
    command(
        &broker.vault,
        &config,
        &stores,
        action,
        &mut asking,
        &mut devices,
        &mut std::io::stdout(),
    )
}

/// What `vault-keyring status` prints: the tier and the kinds of wrap,
/// never what they hold.
///
/// # Errors
///
/// Returns [`Error::Usage`] without a keyring path, or [`Error::Vault`]
/// if the keyring cannot be read.
pub fn status(config: &KeyringConfig) -> Result<String, Error> {
    let keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    let tier = match keyring.tier() {
        Tier::File => "file",
        Tier::Totp => "TOTP (weaker: the seed is in the keyring)",
        Tier::HardwareKey => "hardware key",
    };
    let required = if config.require_hardware_key {
        "; policy: hardware key required"
    } else {
        ""
    };
    Ok(format!(
        "vault-keyring: tier {tier}, {} backup code(s) left{required}",
        keyring.backup_codes_left()
    ))
}

/// What `vault-keyring status` prints: the [`status`] line, then
/// every way in, each security key by id, label and date, and what to act
/// on. Never what any factor holds, and never a backup code.
///
/// # Errors
///
/// As [`status`].
pub fn report(config: &KeyringConfig) -> Result<String, Error> {
    let summary = status(config)?;
    let keyring = Keyring::load(config.keyring_path()?).map_err(vault_error)?;
    let shown = keyring.protection();
    let mut lines = vec![summary, format!("  ways in: {}", ways_in(&shown))];
    lines.extend(
        shown
            .hardware_keys
            .iter()
            .map(|key| format!("  security key {}", key_line(key))),
    );
    lines.extend(
        shown
            .warnings
            .iter()
            .map(|warning| format!("  warning: {}", warning_text(*warning))),
    );
    Ok(lines.join("\n"))
}

/// Every way into the vault, in words.
fn ways_in(shown: &Protection) -> String {
    let mut ways = Vec::new();
    match shown.hardware_keys.len() {
        0 => {}
        1 => ways.push(String::from("1 security key")),
        many => ways.push(format!("{many} security keys")),
    }
    if shown.totp {
        ways.push(String::from("an authenticator app (TOTP)"));
    }
    if shown.file {
        ways.push(String::from("a key file"));
    }
    ways.push(format!("{} backup code(s) left", shown.backup_codes_left));
    ways.push(String::from("no OS keychain (none in this build)"));
    ways.join(", ")
}

/// A warning, said plainly, with what to do.
fn warning_text(warning: Warning) -> String {
    match warning {
        Warning::OneHardwareKey { backup_codes_left } => format!(
            "only one security key is enrolled; if it is lost, only a backup code opens the vault ({backup_codes_left} left). Enroll a second key: vault-keyring add-key"
        ),
        Warning::NoHardwareKey { backup_codes_left } => format!(
            "no security key is enrolled, so only a backup code opens the vault ({backup_codes_left} left). Enroll a key: vault-keyring add-key"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use hatchling_vault::{SealedVault, Secret, SecretName};

    use super::*;

    /// [`super::unlock_asking`] with no security key reachable.
    fn unlock_asking(
        vault: &VaultConfig,
        config: &KeyringConfig,
        ask: Ask<'_>,
    ) -> Result<MasterKey, Error> {
        super::unlock_asking(vault, config, ask, &mut NoKeys)
    }

    /// [`super::enroll_totp`] with no security key reachable.
    fn enroll_totp(vault: &VaultConfig, config: &KeyringConfig, ask: Ask<'_>) -> Result<(), Error> {
        super::enroll_totp(vault, config, &secrets(vault), ask, &mut NoKeys).map(drop)
    }

    /// [`super::backup_codes`] with no security key reachable.
    fn backup_codes(
        vault: &VaultConfig,
        config: &KeyringConfig,
        ask: Ask<'_>,
    ) -> Result<BackupCodes, Error> {
        super::backup_codes(vault, config, ask, &mut NoKeys)
    }

    /// Finishes a rotation as the broker does, for the secrets store, and
    /// returns the new backup codes as text.
    fn settle(
        keyring: &mut Keyring,
        vault: &VaultConfig,
        config: &KeyringConfig,
        rotated: &hatchling_vault::Rotated,
    ) -> Vec<String> {
        let codes = rotated
            .codes
            .as_ref()
            .map(|codes| codes.codes().map(str::to_owned).collect())
            .unwrap_or_default();
        finish(keyring, &rotated.rotation, &secrets(vault), &config.kek_file(vault)).unwrap();
        codes
    }

    /// The secrets store alone: the stores a keyring command re-seals here.
    fn secrets(vault: &VaultConfig) -> Vec<(Store, PathBuf)> {
        vec![(Store::SECRETS, vault.root.clone())]
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> (Scratch, VaultConfig) {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-vault-keyring-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = dir.join("vault");
        let vault = VaultConfig {
            key_file: VaultConfig::default_key_file(&root),
            root,
            grants: Vec::new(),
        };
        (Scratch(dir), vault)
    }

    fn keyring_config(scratch: &Scratch) -> KeyringConfig {
        KeyringConfig {
            keyring: Some(scratch.0.join("vault.key").join("keyring")),
            kek_file: None,
            require_hardware_key: false,
        }
    }

    #[test]
    fn settings_are_absolute_paths_or_absent() {
        assert_eq!(
            KeyringConfig::parse(|_| None).unwrap(),
            KeyringConfig::default()
        );
        let set = KeyringConfig::parse(|key| {
            key.ends_with("KEYRING")
                .then(|| format!("/k/{key}"))
                .or_else(|| key.ends_with("KEK_FILE").then(|| format!("/k/{key}")))
        })
        .unwrap();
        assert_eq!(
            set.keyring,
            Some(PathBuf::from("/k/HATCHLING_VAULT_KEYRING"))
        );
        assert_eq!(
            set.kek_file,
            Some(PathBuf::from("/k/HATCHLING_VAULT_KEK_FILE"))
        );
        for key in ["HATCHLING_VAULT_KEYRING", "HATCHLING_VAULT_KEK_FILE"] {
            let err =
                KeyringConfig::parse(|asked| (asked == key).then(|| String::from("relative")))
                    .unwrap_err();
            assert!(
                matches!(err, Error::InvalidConfig { key: found, .. } if found == key),
                "{key}"
            );
        }
        let (_scratch, vault) = scratch("defaults");
        assert_eq!(
            KeyringConfig::default().kek_file(&vault),
            vault.root.with_extension("key").join("kek")
        );
        assert_eq!(
            set.kek_file(&vault),
            PathBuf::from("/k/HATCHLING_VAULT_KEK_FILE")
        );
        assert!(KeyringConfig::from_env().is_ok());
    }

    #[test]
    fn the_settings_helpers_each_do_their_one_job() {
        assert_eq!(absolute(None, "K").unwrap(), None);
        assert_eq!(
            absolute(Some(String::from("/a")), "K").unwrap(),
            Some(PathBuf::from("/a"))
        );
        assert!(matches!(
            absolute(Some(String::from("a")), "K"),
            Err(Error::InvalidConfig { key: "K", .. })
        ));
        let set = KeyringConfig {
            keyring: Some(PathBuf::from("/r")),
            ..KeyringConfig::default()
        };
        assert_eq!(set.keyring_path().unwrap(), Path::new("/r"));
        assert!(matches!(
            KeyringConfig::default().keyring_path(),
            Err(Error::Usage { .. })
        ));
        assert!(matches!(
            vault_error(hatchling_vault::Error::UnlockFailed),
            Error::Vault {
                source: hatchling_vault::Error::UnlockFailed
            }
        ));
    }

    #[test]
    fn without_a_keyring_the_key_file_is_the_key_as_before() {
        let (_scratch, vault) = scratch("plain");
        let first = unlock(&vault, &KeyringConfig::default()).unwrap();
        let name: SecretName = "token".parse().unwrap();
        SealedVault::open(&vault.root, &first)
            .unwrap()
            .store(&name, &Secret::new(b"t".to_vec()).unwrap())
            .unwrap();
        let again = unlock(&vault, &KeyringConfig::default()).unwrap();
        assert!(
            SealedVault::open(&vault.root, &again)
                .unwrap()
                .fetch(&name)
                .is_ok()
        );
    }

    #[test]
    fn adopting_a_keyring_keeps_every_secret_and_removes_the_plain_key() {
        let (scratch, vault) = scratch("adopt");
        let config = keyring_config(&scratch);
        assert!(
            matches!(init(&vault, &config), Err(Error::Vault { .. })),
            "no key file to adopt yet"
        );
        let plain = unlock(&vault, &KeyringConfig::default()).unwrap();
        let name: SecretName = "token".parse().unwrap();
        SealedVault::open(&vault.root, &plain)
            .unwrap()
            .store(&name, &Secret::new(b"kept".to_vec()).unwrap())
            .unwrap();
        init(&vault, &config).unwrap();
        assert!(
            !vault.key_file.exists(),
            "the DEK is no longer on disk unwrapped"
        );
        let dek = unlock(&vault, &config).unwrap();
        assert_eq!(
            SealedVault::open(&vault.root, &dek)
                .unwrap()
                .fetch(&name)
                .unwrap()
                .as_bytes(),
            b"kept"
        );
        assert_eq!(
            status(&config).unwrap(),
            "vault-keyring: tier file, 0 backup code(s) left"
        );
        // Without the KEK file, the vault does not unlock, and nothing is created in its place.
        std::fs::remove_file(config.kek_file(&vault)).unwrap();
        assert!(matches!(unlock(&vault, &config), Err(Error::Vault { .. })));
        assert!(!config.kek_file(&vault).exists());
    }

    #[test]
    fn a_keyring_is_adopted_once_and_only_with_a_path() {
        let (scratch, vault) = scratch("once");
        unlock(&vault, &KeyringConfig::default()).unwrap();
        assert!(matches!(
            init(&vault, &KeyringConfig::default()),
            Err(Error::Usage { .. })
        ));
        assert!(matches!(
            status(&KeyringConfig::default()),
            Err(Error::Usage { .. })
        ));
        let config = keyring_config(&scratch);
        assert!(matches!(status(&config), Err(Error::Vault { .. })));
        init(&vault, &config).unwrap();
        // A second adoption finds the plain key gone, so it is refused; the keyring stands.
        assert!(matches!(init(&vault, &config), Err(Error::Vault { .. })));
        assert!(unlock(&vault, &config).is_ok());
    }

    #[test]
    fn a_hardware_key_vault_unlocks_with_its_key_and_says_why_it_cannot_without() {
        use hatchling_vault::virtual_key::VirtualAuthenticator;
        let (scratch, vault) = scratch("hardware");
        let config = keyring_config(&scratch);
        unlock(&vault, &KeyringConfig::default()).unwrap();
        init(&vault, &config).unwrap();
        let path = config.keyring.clone().unwrap();
        let mut keyring = Keyring::load(&path).unwrap();
        let kek = Kek::from_file_key(&FileKey::new(config.kek_file(&vault)).load().unwrap());
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let (mut primary, mut backup) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let rotated = keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut primary, &mut backup])
            .unwrap();
        settle(&mut keyring, &vault, &config, &rotated);
        assert_eq!(
            status(&config).unwrap(),
            "vault-keyring: tier hardware key, 0 backup code(s) left"
        );
        let err = unlock_asking(&vault, &config, &mut |_| Ok(String::new())).unwrap_err();
        assert!(
            matches!(&err, Error::Usage { reason } if reason.contains("stopped; nothing was changed")),
            "{err}"
        );
        let dek = unlock_with(
            &vault,
            &config,
            Factors {
                authenticator: Some(&mut backup),
                ..Factors::default()
            },
        )
        .unwrap();
        assert!(SealedVault::open(&vault.root, &dek).is_ok());
        let mut stranger = VirtualAuthenticator::new();
        assert!(matches!(
            unlock_with(
                &vault,
                &config,
                Factors {
                    authenticator: Some(&mut stranger),
                    ..Factors::default()
                }
            ),
            Err(Error::Vault { .. })
        ));
    }
    /// What an authenticator app shows: it reads the seed from the URI in
    /// a prompt, and keeps it for the codes after.
    struct App {
        seed: Option<Vec<u8>>,
        asked: Vec<String>,
        wrong_first: usize,
    }

    impl App {
        const fn new(wrong_first: usize) -> Self {
            Self {
                seed: None,
                asked: Vec::new(),
                wrong_first,
            }
        }

        /// RFC 4648 Base32 back to bytes (the app's side of the URI).
        fn unbase32(text: &str) -> Vec<u8> {
            let (mut out, mut buffer, mut bits) = (Vec::new(), 0u32, 0u32);
            for c in text.bytes() {
                let value = u32::try_from(
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"
                        .iter()
                        .position(|&x| x == c)
                        .unwrap(),
                )
                .unwrap();
                buffer = (buffer << 5) | value;
                bits += 5;
                if bits >= 8 {
                    bits -= 8;
                    out.push(u8::try_from((buffer >> bits) & 0xff).unwrap());
                }
            }
            out
        }

        fn code(&self, offset: u64) -> String {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            hatchling_vault::totp_code(self.seed.as_deref().unwrap(), now + offset)
        }

        fn answer(&mut self, prompt: &str) -> String {
            self.asked.push(prompt.to_owned());
            if let Some((_, rest)) = prompt.split_once("secret=") {
                let secret: String = rest
                    .chars()
                    .take_while(char::is_ascii_alphanumeric)
                    .collect();
                self.seed = Some(Self::unbase32(&secret));
            }
            let right = self.code(0);
            if self.wrong_first > 0 {
                self.wrong_first -= 1;
                return format!("{:06}", (right.parse::<u32>().unwrap() + 1) % 1_000_000);
            }
            right
        }
    }

    fn adopted(name: &str) -> (Scratch, VaultConfig, KeyringConfig) {
        let (scratch, vault) = scratch(name);
        let config = keyring_config(&scratch);
        let plain = unlock(&vault, &KeyringConfig::default()).unwrap();
        let secret: SecretName = "token".parse().unwrap();
        SealedVault::open(&vault.root, &plain)
            .unwrap()
            .store(&secret, &Secret::new(b"kept".to_vec()).unwrap())
            .unwrap();
        init(&vault, &config).unwrap();
        (scratch, vault, config)
    }

    fn reads_the_secret(vault: &VaultConfig, key: &MasterKey) -> bool {
        let secret: SecretName = "token".parse().unwrap();
        SealedVault::open(&vault.root, key)
            .unwrap()
            .fetch(&secret)
            .is_ok_and(|found| found.as_bytes() == b"kept")
    }

    /// Input that cannot be read.
    struct Broken;

    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("gone"))
        }
    }

    #[test]
    fn asking_writes_the_prompt_and_reads_one_line() {
        let mut output = Vec::new();
        let answer = ask(
            "Code:",
            &mut std::io::Cursor::new(b"123456\r\nnext\n".to_vec()),
            &mut output,
        )
        .unwrap();
        assert_eq!(
            (answer.as_str(), output.as_slice()),
            ("123456", b"Code: ".as_slice())
        );
        assert_eq!(
            ask("x", &mut std::io::Cursor::new(Vec::new()), &mut Vec::new()).unwrap(),
            ""
        );
        assert!(matches!(
            ask("x", &mut std::io::BufReader::new(Broken), &mut Vec::new()),
            Err(Error::Vault { .. })
        ));
    }

    #[test]
    fn totp_enrollment_shows_the_uri_once_retries_a_wrong_code_and_then_start_up_asks_for_a_code() {
        let (_scratch, vault, config) = adopted("totp");
        let mut app = App::new(1);
        enroll_totp(&vault, &config, &mut |prompt| Ok(app.answer(prompt))).unwrap();
        assert_eq!(app.asked.len(), 2, "the URI, then one retry");
        assert!(
            app.asked
                .first()
                .unwrap()
                .contains("otpauth://totp/Hatchling:vault?secret=")
        );
        assert!(
            app.asked.get(1).unwrap().contains("did not match")
                && !app.asked.get(1).unwrap().contains("otpauth"),
            "the seed is shown once"
        );
        assert!(
            status(&config)
                .unwrap()
                .starts_with("vault-keyring: tier TOTP (weaker")
        );
        // Start-up asks for a code; the next step's code opens the vault.
        let mut asked = 0;
        let dek = unlock_asking(&vault, &config, &mut |prompt| {
            asked += 1;
            assert!(prompt.contains("Current code"));
            Ok(app.code(30))
        })
        .unwrap();
        assert_eq!(asked, 1);
        assert!(reads_the_secret(&vault, &dek));
        assert!(
            matches!(
                unlock_with(&vault, &config, Factors::default()),
                Err(Error::Usage { .. })
            ),
            "no code, no start"
        );
        assert!(matches!(
            unlock_asking(&vault, &config, &mut |_| Ok(String::from("000000"))),
            Err(Error::Vault { .. })
        ));
        assert!(matches!(
            unlock_asking(&vault, &config, &mut |_| Err(Error::Usage {
                reason: String::new()
            })),
            Err(Error::Usage { .. })
        ));
    }

    #[test]
    fn re_enrolling_totp_asks_for_a_current_code_first() {
        let (_scratch, vault, config) = adopted("totp-again");
        let mut app = App::new(0);
        enroll_totp(&vault, &config, &mut |prompt| Ok(app.answer(prompt))).unwrap();
        // The enrollment spent this step's code; the next step's is current.
        let mut again = App::new(0);
        let mut first = true;
        enroll_totp(&vault, &config, &mut |prompt| {
            if first {
                first = false;
                assert!(prompt.contains("Current code"));
                return Ok(app.code(30));
            }
            Ok(again.answer(prompt))
        })
        .unwrap();
        assert_ne!(again.seed, app.seed, "a new seed");
        assert!(reads_the_secret(
            &vault,
            &unlock_asking(&vault, &config, &mut |_| Ok(again.code(30))).unwrap()
        ));
    }

    #[test]
    fn totp_enrollment_gives_up_after_three_wrong_codes_and_changes_nothing() {
        let (_scratch, vault, config) = adopted("totp-wrong");
        let mut app = App::new(3);
        assert!(matches!(
            enroll_totp(&vault, &config, &mut |prompt| Ok(app.answer(prompt))),
            Err(Error::Vault {
                source: hatchling_vault::Error::CodeRejected
            })
        ));
        assert_eq!(app.asked.len(), 3);
        assert!(
            status(&config)
                .unwrap()
                .starts_with("vault-keyring: tier file")
        );
        let mut never = |_: &str| -> Result<String, Error> {
            Err(Error::Usage {
                reason: String::from("asked"),
            })
        };
        assert!(
            reads_the_secret(&vault, &unlock_asking(&vault, &config, &mut never).unwrap()),
            "a file-tier start asks nothing"
        );
        assert!(matches!(
            enroll_totp(&vault, &KeyringConfig::default(), &mut never),
            Err(Error::Usage { .. })
        ));
        assert!(
            matches!(
                enroll_totp(&vault, &config, &mut never),
                Err(Error::Usage { .. })
            ),
            "an ask that fails stops it"
        );
    }

    #[test]
    fn a_hardware_key_vault_cannot_enroll_totp_without_its_key() {
        use hatchling_vault::virtual_key::VirtualAuthenticator;
        let (_scratch, vault, config) = adopted("totp-hardware");
        let path = config.keyring.clone().unwrap();
        let mut keyring = Keyring::load(&path).unwrap();
        let kek = Kek::from_file_key(&FileKey::new(config.kek_file(&vault)).load().unwrap());
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let (mut primary, mut backup) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let rotated = keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut primary, &mut backup])
            .unwrap();
        settle(&mut keyring, &vault, &config, &rotated);
        let err = enroll_totp(&vault, &config, &mut |_| Ok(String::new())).unwrap_err();
        assert!(
            matches!(&err, Error::Usage { reason } if reason.contains("stopped; nothing was changed")),
            "{err}"
        );
    }

    /// A vault at the hardware-key tier, with backup codes made first.
    fn hardware_with_codes(name: &str) -> (Scratch, VaultConfig, KeyringConfig, Vec<String>) {
        use hatchling_vault::virtual_key::VirtualAuthenticator;
        let (scratch, vault, config) = adopted(name);
        backup_codes(&vault, &config, &mut |_| Ok(String::new())).unwrap();
        let path = config.keyring.clone().unwrap();
        let mut keyring = Keyring::load(&path).unwrap();
        let kek = Kek::from_file_key(&FileKey::new(config.kek_file(&vault)).load().unwrap());
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let (mut primary, mut backup) = (VirtualAuthenticator::new(), VirtualAuthenticator::new());
        let rotated = keyring
            .enroll_hardware_keys(&unlocked, &mut [&mut primary, &mut backup])
            .unwrap();
        // The new key brings the codes to use.
        let codes = settle(&mut keyring, &vault, &config, &rotated);
        (scratch, vault, config, codes)
    }

    #[test]
    fn each_tier_is_asked_for_what_it_needs() {
        let mut asked = Vec::new();
        let file = asked_factors(Tier::File, &mut |prompt| {
            asked.push(prompt.to_owned());
            Ok(String::from("x"))
        })
        .unwrap();
        assert!(
            file.code.is_none()
                && file.backup_code.is_none()
                && file.authenticator.is_none()
                && asked.is_empty()
        );
        let totp = asked_factors(Tier::Totp, &mut |_| Ok(String::from("123456"))).unwrap();
        assert_eq!(
            (totp.code.as_deref(), totp.backup_code.as_deref()),
            (Some("123456"), None)
        );
        let mut hardware_asked = 0;
        let hardware = asked_factors(Tier::HardwareKey, &mut |_| {
            hardware_asked += 1;
            Ok(String::from("ABCD"))
        })
        .unwrap();
        assert!(
            hardware.code.is_none()
                && hardware.backup_code.is_none()
                && hardware.authenticator.is_none()
                && hardware_asked == 0,
            "a hardware-key vault is asked through authorize, not here"
        );
        assert!(matches!(
            asked_factors(Tier::Totp, &mut |_| Err(Error::Usage {
                reason: String::new()
            })),
            Err(Error::Usage { .. })
        ));
    }

    #[test]
    fn backup_codes_are_made_through_the_current_tier_and_counted() {
        let (_scratch, vault, config) = adopted("codes");
        assert_eq!(
            status(&config).unwrap(),
            "vault-keyring: tier file, 0 backup code(s) left"
        );
        let mut asked = 0;
        let codes = backup_codes(&vault, &config, &mut |_| {
            asked += 1;
            Ok(String::new())
        })
        .unwrap();
        assert_eq!(
            (codes.codes().count(), asked),
            (10, 0),
            "a file-tier vault asks nothing"
        );
        assert_eq!(
            status(&config).unwrap(),
            "vault-keyring: tier file, 10 backup code(s) left"
        );
        assert!(matches!(
            backup_codes(
                &vault,
                &KeyringConfig::default(),
                &mut |_| Ok(String::new())
            ),
            Err(Error::Usage { .. })
        ));
    }

    #[test]
    fn a_hardware_key_vault_starts_with_a_backup_code_which_is_then_used_up() {
        let (_scratch, vault, config, codes) = hardware_with_codes("codes-start");
        let code = codes.first().unwrap().clone();
        let mut prompts = Vec::new();
        let dek = unlock_asking(&vault, &config, &mut |prompt| {
            prompts.push(prompt.to_owned());
            Ok(code.clone())
        })
        .unwrap();
        assert!(reads_the_secret(&vault, &dek));
        assert!(prompts.first().unwrap().contains("backup code") && prompts.len() == 1);
        assert!(status(&config).unwrap().ends_with("9 backup code(s) left"));
        assert!(
            matches!(
                unlock_asking(&vault, &config, &mut |_| Ok(code.clone())),
                Err(Error::Vault { .. })
            ),
            "used up"
        );
        let err = unlock_asking(&vault, &config, &mut |_| Ok(String::from("  "))).unwrap_err();
        assert!(
            matches!(&err, Error::Usage { reason } if reason.contains("stopped; nothing was changed")),
            "no code, no start: {err}"
        );
        assert!(matches!(
            unlock_with(
                &vault,
                &config,
                Factors {
                    backup_code: Some(code),
                    ..Factors::default()
                }
            ),
            Err(Error::Vault { .. })
        ));
    }

    #[test]
    fn a_backup_code_can_renew_the_codes_but_never_lower_the_tier() {
        let (_scratch, vault, config, codes) = hardware_with_codes("codes-renew");
        let mut answers = codes.iter().cloned();
        let renewed: Vec<String> =
            backup_codes(&vault, &config, &mut |_| Ok(answers.next().unwrap()))
                .unwrap()
                .codes()
                .map(str::to_owned)
                .collect();
        assert!(
            status(&config).unwrap().ends_with("10 backup code(s) left"),
            "a new set"
        );
        assert!(
            matches!(
                unlock_asking(&vault, &config, &mut |_| Ok(codes.get(1).unwrap().clone())),
                Err(Error::Vault { .. })
            ),
            "the old set is gone"
        );
        // Recovery restores; it does not downgrade to TOTP.
        let mut fresh = renewed.iter().cloned();
        let err = enroll_totp(&vault, &config, &mut |_| Ok(fresh.next().unwrap())).unwrap_err();
        assert!(
            matches!(
                err,
                Error::Vault {
                    source: hatchling_vault::Error::TierChange { .. }
                }
            ),
            "{err}"
        );
        assert!(
            status(&config)
                .unwrap()
                .starts_with("vault-keyring: tier hardware key")
        );
    }

    #[test]
    fn the_policy_flag_is_strict() {
        let read = |value: &str| {
            KeyringConfig::parse(|key| {
                (key == "HATCHLING_REQUIRE_HARDWARE_KEY").then(|| value.to_owned())
            })
        };
        for (value, expected) in [
            ("true", true),
            ("1", true),
            (" 1 ", true),
            ("false", false),
            ("0", false),
        ] {
            assert_eq!(
                read(value).unwrap().require_hardware_key,
                expected,
                "{value:?}"
            );
        }
        for bad in ["yes", "on", "", "TRUE"] {
            let err = read(bad).unwrap_err();
            assert!(
                matches!(&err, Error::InvalidConfig { key: "HATCHLING_REQUIRE_HARDWARE_KEY", reason } if reason.contains("true, 1, false or 0")),
                "{bad:?}: {err}"
            );
        }
        assert!(!KeyringConfig::default().require_hardware_key);
    }

    #[test]
    fn the_policy_refuses_any_vault_below_the_hardware_key_tier_and_any_step_down() {
        // No keyring at all.
        let (_plain, vault) = scratch("policy-plain");
        let required = KeyringConfig {
            require_hardware_key: true,
            ..KeyringConfig::default()
        };
        let err = unlock_with(&vault, &required, Factors::default()).unwrap_err();
        assert!(
            matches!(&err, Error::Usage { reason } if reason.contains("HATCHLING_REQUIRE_HARDWARE_KEY") && reason.contains("no keyring")),
            "{err}"
        );
        assert!(
            !vault.key_file.exists(),
            "refused before a key file is made"
        );
        // A file-tier vault: refused, and TOTP is refused too.
        let (_scratch, vault, config) = adopted("policy-file");
        let config = KeyringConfig {
            require_hardware_key: true,
            ..config
        };
        let err = unlock_asking(&vault, &config, &mut |_| Ok(String::new())).unwrap_err();
        assert!(
            matches!(&err, Error::Usage { reason } if reason.contains("lower tier")),
            "{err}"
        );
        let err = enroll_totp(&vault, &config, &mut |_| Ok(String::new())).unwrap_err();
        assert!(
            matches!(&err, Error::Usage { reason } if reason.contains("TOTP is below it")),
            "{err}"
        );
        assert!(
            status(&config)
                .unwrap()
                .ends_with("; policy: hardware key required")
        );
        // A hardware-key vault starts under the policy.
        let (_scratch, vault, config, codes) = hardware_with_codes("policy-hardware");
        let config = KeyringConfig {
            require_hardware_key: true,
            ..config
        };
        let dek =
            unlock_asking(&vault, &config, &mut |_| Ok(codes.first().unwrap().clone())).unwrap();
        assert!(reads_the_secret(&vault, &dek));
    }

    #[test]
    fn the_command_runs_each_action_and_reports_it() {
        let (scratch, vault) = scratch("command");
        let config = keyring_config(&scratch);
        let mut never = |_: &str| -> Result<String, Error> { Ok(String::new()) };
        let run = |action: Option<&str>, ask: Ask<'_>| {
            let mut out = Vec::new();
            command(&vault, &config, &secrets(&vault), action, ask, &mut NoKeys, &mut out)
                .map(|()| String::from_utf8(out).unwrap())
        };
        unlock(&vault, &KeyringConfig::default()).unwrap();
        assert!(
            run(Some("init"), &mut never)
                .unwrap()
                .contains("adopted at the file tier")
        );
        assert_eq!(
            run(Some("status"), &mut never).unwrap(),
            "vault-keyring: tier file, 0 backup code(s) left\n  ways in: a key file, 0 backup code(s) left, no OS keychain (none in this build)\n"
        );
        let codes = run(Some("backup-codes"), &mut never).unwrap();
        assert_eq!(codes.lines().count(), 11, "a heading and ten codes");
        let mut app = App::new(0);
        assert!(
            run(Some("enroll-totp"), &mut |prompt| Ok(app.answer(prompt)))
                .unwrap()
                .contains("TOTP tier")
        );
        for bad in [None, Some("bogus")] {
            assert!(matches!(run(bad, &mut never), Err(Error::Usage { .. })));
        }
        assert!(
            matches!(run(Some("init"), &mut never), Err(Error::Vault { .. })),
            "errors pass through"
        );
    }

    #[test]
    fn the_binary_runs_the_command_with_its_environments_settings() {
        let (_scratch, vault) = scratch("run-keyring");
        let mut broker = crate::api_auth::tests::owner_config(None);
        broker.vault = vault;
        assert!(matches!(
            run_keyring(&broker, Some("bogus")),
            Err(Error::Usage { .. })
        ));
        assert!(matches!(
            run_keyring(&broker, None),
            Err(Error::Usage { .. })
        ));
    }

    /// Software security keys on a desk; `plugged` says which is in.
    struct Desk {
        keys: Vec<hatchling_vault::virtual_key::VirtualAuthenticator>,
        plugged: Rc<std::cell::Cell<Option<usize>>>,
    }

    impl Devices for Desk {
        fn plugged_in(&mut self) -> Option<&mut dyn Authenticator> {
            let key: &mut dyn Authenticator = self.keys.get_mut(self.plugged.get()?)?;
            Some(key)
        }
    }

    /// An operator who gives each answer in turn, then plugs in the key
    /// it names (or none), and remembers every prompt.
    struct Operator {
        steps: std::collections::VecDeque<(String, Option<usize>)>,
        asked: Vec<String>,
        plugged: Rc<std::cell::Cell<Option<usize>>>,
    }

    impl Operator {
        fn new(desk: &Desk, steps: &[(&str, Option<usize>)]) -> Self {
            Self {
                steps: steps
                    .iter()
                    .map(|(answer, plug)| ((*answer).to_owned(), *plug))
                    .collect(),
                asked: Vec::new(),
                plugged: Rc::clone(&desk.plugged),
            }
        }

        fn answer(&mut self, prompt: &str) -> String {
            self.asked.push(prompt.to_owned());
            let (answer, plug) = self.steps.pop_front().unwrap_or_default();
            self.plugged.set(plug);
            answer
        }
    }

    /// A hardware-key vault with `enrolled` software keys on a desk (key 0
    /// plugged in), one spare key after them, and ten backup codes.
    fn desk_vault(
        name: &str,
        enrolled: usize,
    ) -> (Scratch, VaultConfig, KeyringConfig, Vec<String>, Desk) {
        use hatchling_vault::virtual_key::VirtualAuthenticator;
        let (scratch, vault, config) = adopted(name);
        backup_codes(&vault, &config, &mut |_| Ok(String::new())).unwrap();
        let mut keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let kek = Kek::from_file_key(&FileKey::new(config.kek_file(&vault)).load().unwrap());
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let mut keys: Vec<VirtualAuthenticator> = (0..=enrolled)
            .map(|_| VirtualAuthenticator::new())
            .collect();
        let mut devices: Vec<&mut dyn Authenticator> = keys
            .iter_mut()
            .take(enrolled)
            .map(|key| -> &mut dyn Authenticator { key })
            .collect();
        let rotated = keyring
            .enroll_hardware_keys(&unlocked, &mut devices)
            .unwrap();
        drop(devices);
        // The new key brings the codes to use.
        let codes = settle(&mut keyring, &vault, &config, &rotated);
        let desk = Desk {
            keys,
            plugged: Rc::new(std::cell::Cell::new(Some(0))),
        };
        (scratch, vault, config, codes, desk)
    }

    fn keys_of(config: &KeyringConfig) -> Vec<KeyInfo> {
        Keyring::load(config.keyring.as_deref().unwrap())
            .unwrap()
            .hardware_keys()
    }

    fn codes_left(config: &KeyringConfig) -> usize {
        Keyring::load(config.keyring.as_deref().unwrap())
            .unwrap()
            .backup_codes_left()
    }

    fn today() -> String {
        utc_date(now()).unwrap()
    }

    fn refused(result: &Result<KeyInfo, Error>, words: &str) -> bool {
        matches!(result, Err(Error::Usage { reason }) if reason.contains(words))
    }

    #[test]
    fn dates_are_shown_as_utc_days() {
        for (seconds, date) in [
            (0, "1970-01-01"),
            (951_782_400, "2000-02-29"),
            (1_709_164_800, "2024-02-29"),
            (1_791_417_600, "2026-10-08"),
            (4_107_542_399, "2100-02-28"),
        ] {
            assert_eq!(utc_date(seconds).as_deref(), Some(date), "{seconds}");
        }
        assert_eq!(utc_date(u64::MAX), None, "beyond any calendar");
    }

    #[test]
    fn status_shows_every_way_in_and_each_key_and_never_a_code() {
        let (_scratch, _vault, config, codes, mut desk) = desk_vault("report", 2);
        let keys = keys_of(&config);
        let shown = report(&config).unwrap();
        let mut expected = vec![
            status(&config).unwrap(),
            String::from(
                "  ways in: 2 security keys, 10 backup code(s) left, no OS keychain (none in this build)",
            ),
        ];
        expected.extend(
            keys.iter()
                .map(|key| format!("  security key {}  (no label)  added {}", key.id, today())),
        );
        assert_eq!(shown, expected.join("\n"));
        assert!(
            codes.iter().all(|code| !shown.contains(code.as_str())),
            "never a backup code"
        );
        // One key left: said plainly, with what to do.
        let mut keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let by_first = keyring
            .unlock_hardware_key(desk.plugged_in().unwrap())
            .unwrap();
        keyring
            .remove_hardware_key(
                &by_first,
                &keys.last().unwrap().id,
                Some(SingleKeyRisk::accepted()),
            )
            .unwrap();
        let shown = report(&config).unwrap();
        assert!(shown.contains("ways in: 1 security key, 10 backup code(s) left"));
        assert!(shown.ends_with(
            "  warning: only one security key is enrolled; if it is lost, only a backup code opens the vault (10 left). Enroll a second key: vault-keyring add-key"
        ));
        // The other ways in, and the other warning, in words.
        let app = Protection {
            tier: Tier::Totp,
            hardware_keys: Vec::new(),
            totp: true,
            file: true,
            backup_codes_left: 3,
            warnings: Vec::new(),
        };
        assert_eq!(
            ways_in(&app),
            "an authenticator app (TOTP), a key file, 3 backup code(s) left, no OS keychain (none in this build)"
        );
        assert_eq!(
            warning_text(Warning::NoHardwareKey {
                backup_codes_left: 2
            }),
            "no security key is enrolled, so only a backup code opens the vault (2 left). Enroll a key: vault-keyring add-key"
        );
        let migrated = KeyInfo {
            id: String::from("ab12"),
            label: String::from("YubiKey"),
            added: None,
        };
        assert_eq!(
            key_line(&migrated),
            "ab12  YubiKey  added before the keyring kept dates"
        );
        assert_eq!(
            key_list(&[migrated.clone(), migrated]),
            "  ab12  YubiKey  added before the keyring kept dates\n  ab12  YubiKey  added before the keyring kept dates"
        );
    }

    #[test]
    fn adding_a_key_names_it_confirms_authorizes_and_then_enrolls_it() {
        let (_scratch, _vault, config, codes, mut desk) = desk_vault("add-key", 2);
        let mut operator = Operator::new(
            &desk,
            &[
                ("YubiKey 5C, desk", Some(0)),
                ("yes", Some(0)),
                ("", Some(0)),
                ("", Some(2)),
            ],
        );
        let added = add_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        )
        .unwrap();
        assert_eq!(
            (added.label.as_str(), added.added.and_then(utc_date)),
            ("YubiKey 5C, desk", Some(today()))
        );
        assert_eq!(keys_of(&config).last(), Some(&added));
        assert_eq!(
            codes_left(&config),
            10,
            "an enrolled key authorized it: no code spent"
        );
        assert_eq!(operator.asked.len(), 4);
        assert!(
            operator
                .asked
                .first()
                .unwrap()
                .starts_with("Name the new key")
        );
        assert!(
            operator
                .asked
                .get(1)
                .unwrap()
                .contains("named \"YubiKey 5C, desk\"")
        );
        assert_eq!(
            operator.asked.get(2).map(String::as_str),
            Some(AUTHORIZE_PROMPT)
        );
        assert!(
            operator
                .asked
                .last()
                .unwrap()
                .starts_with("Now plug in the new security key")
        );
        let keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        desk.plugged.set(Some(2));
        assert_eq!(
            keyring
                .unlock_hardware_key(desk.plugged_in().unwrap())
                .unwrap()
                .opened_by(),
            Some(added.id.as_str())
        );
        // A backup code may authorize instead.
        desk.keys
            .push(hatchling_vault::virtual_key::VirtualAuthenticator::new());
        let code = codes.first().unwrap().as_str();
        let mut operator = Operator::new(
            &desk,
            &[
                ("spare", Some(0)),
                ("yes", Some(0)),
                (code, Some(0)),
                ("", Some(3)),
            ],
        );
        add_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        )
        .unwrap();
        assert_eq!((keys_of(&config).len(), codes_left(&config)), (4, 9));
    }

    #[test]
    fn a_bad_name_is_asked_again_and_declining_spends_nothing() {
        let (_scratch, _vault, config, _, mut desk) = desk_vault("add-declined", 2);
        // A bad name is asked for again, up to three times.
        let mut operator = Operator::new(
            &desk,
            &[
                ("", Some(0)),
                ("bad\u{7}", Some(0)),
                ("ok", Some(0)),
                ("no", Some(0)),
            ],
        );
        let declined = add_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        );
        assert!(refused(&declined, "stopped; nothing was changed"));
        assert!(
            operator.asked.get(1).unwrap().starts_with(
                "that label cannot be used: a label cannot be empty. Try another name:"
            )
        );
        assert_eq!(
            operator.asked.len(),
            4,
            "declined before anything was asked to authorize"
        );
        let mut operator = Operator::new(&desk, &[("", Some(0)), ("", Some(0)), ("", Some(0))]);
        assert!(refused(
            &add_key(
                &config,
                &mut |prompt| Ok(operator.answer(prompt)),
                &mut desk
            ),
            "stopped"
        ));
        assert_eq!(operator.asked.len(), 3);
        // No answer and no key to authorize: stopped.
        let mut operator = Operator::new(&desk, &[("x", Some(0)), ("yes", None), ("", None)]);
        assert!(refused(
            &add_key(
                &config,
                &mut |prompt| Ok(operator.answer(prompt)),
                &mut desk
            ),
            "stopped"
        ));
        assert_eq!(
            operator.asked.get(2).map(String::as_str),
            Some(AUTHORIZE_CODE_PROMPT)
        );
        assert_eq!(
            (keys_of(&config).len(), codes_left(&config)),
            (2, 10),
            "nothing changed"
        );
    }

    #[test]
    fn a_key_is_never_added_where_it_cannot_be_and_nothing_is_spent_trying() {
        let (_scratch, _vault, config, _, _) = desk_vault("add-refused", 2);
        let mut asked = Vec::new();
        let result = add_key(
            &config,
            &mut |prompt| {
                asked.push(prompt.to_owned());
                Ok(String::new())
            },
            &mut NoKeys,
        );
        assert!(refused(
            &result,
            "no security key can be reached (no security key is plugged in), so none can be added"
        ));
        assert!(asked.is_empty(), "nothing asked, so no backup code spent");
        assert!(NoKeys.plugged_in().is_none());
        assert!(NoKeys.every_key().is_empty());
        // The default: the one key plugged in, or none.
        let (_desk_scratch, _desk_vault, _desk_config, _, mut desk) = desk_vault("every-key", 2);
        assert_eq!(desk.every_key().len(), 1);
        desk.plugged.set(None);
        assert!(desk.every_key().is_empty());
        assert_eq!(NoKeys.unreachable(), NONE_PLUGGED);
        let (_low_scratch, _low_vault, low) = adopted("add-low");
        let result = add_key(
            &low,
            &mut |prompt| {
                asked.push(prompt.to_owned());
                Ok(String::new())
            },
            &mut NoKeys,
        );
        assert!(refused(
            &result,
            "keys are added to a vault at the hardware-key tier"
        ));
        assert!(asked.is_empty());
    }

    #[test]
    fn removing_a_key_needs_a_different_key_or_a_code_and_never_takes_the_last() {
        let (_scratch, _vault, config, codes, mut desk) = desk_vault("remove-key", 3);
        let ids: Vec<String> = keys_of(&config).into_iter().map(|key| key.id).collect();
        let [first, second, third] = ids.as_slice() else {
            unreachable!("three keys");
        };
        // The key being removed cannot authorize it.
        let mut operator =
            Operator::new(&desk, &[(first, Some(0)), ("yes", Some(0)), ("", Some(0))]);
        let own = remove_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        );
        assert!(
            matches!(&own, Err(Error::Vault { source: hatchling_vault::Error::KeyChange { reason } }) if reason.starts_with("a key cannot authorize its own removal")),
            "{own:?}"
        );
        assert_eq!(keys_of(&config).len(), 3);
        let listed = format!("  {first}  (no label)");
        assert!(operator.asked.first().unwrap().contains(&listed));
        // Another key can.
        let mut operator =
            Operator::new(&desk, &[(first, Some(0)), ("yes", Some(1)), ("", Some(1))]);
        let removed = remove_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        )
        .unwrap();
        assert_eq!(removed.id, *first);
        // Down to one key: the risk is accepted in so many words, and a code may authorize.
        let mut operator = Operator::new(&desk, &[(second, None), ("yes", None)]);
        assert!(refused(
            &remove_key(
                &config,
                &mut |prompt| Ok(operator.answer(prompt)),
                &mut desk
            ),
            "stopped"
        ));
        assert!(
            operator
                .asked
                .get(1)
                .unwrap()
                .contains("Type \"one key\" to accept")
        );
        assert_eq!(
            operator.asked.len(),
            2,
            "stopped before anything was asked to authorize"
        );
        let code = codes.first().unwrap().as_str();
        let mut operator = Operator::new(
            &desk,
            &[
                (second, None),
                ("one key", None),
                ("yes", None),
                (code, None),
            ],
        );
        remove_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut NoKeys,
        )
        .unwrap();
        assert_eq!(
            operator.asked.last().map(String::as_str),
            Some(AUTHORIZE_CODE_PROMPT)
        );
        assert_eq!((keys_of(&config).len(), codes_left(&config)), (1, 9));
        // The last key: refused at once, before anything is asked to authorize.
        let mut operator = Operator::new(&desk, &[(third, None)]);
        let last = remove_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        );
        assert!(refused(&last, "it is the last security key"));
        assert_eq!(
            (
                operator.asked.len(),
                keys_of(&config).len(),
                codes_left(&config)
            ),
            (1, 1, 9)
        );
    }

    #[test]
    fn the_last_backup_code_is_never_spent_on_a_doomed_removal() {
        let (_scratch, _vault, config, codes, mut desk) = desk_vault("remove-last-code", 2);
        let mut keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        for code in codes.iter().take(9) {
            keyring.recover(code).unwrap();
        }
        let last = codes.last().unwrap().as_str();
        let ids: Vec<String> = keys_of(&config).into_iter().map(|key| key.id).collect();
        let [first, second] = ids.as_slice() else {
            unreachable!("two keys");
        };
        // No key to reach: only the last code could authorize, and it
        // cannot, so nothing is asked beyond the key's id.
        let mut operator = Operator::new(&desk, &[(second.as_str(), None)]);
        let up_front = remove_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut NoKeys,
        );
        assert!(
            refused(
                &up_front,
                "only another enrolled security key can authorize this"
            ),
            "{up_front:?}"
        );
        assert!(refused(&up_front, NONE_PLUGGED));
        assert_eq!(
            (
                operator.asked.len(),
                keys_of(&config).len(),
                codes_left(&config)
            ),
            (1, 2, 1)
        );
        // A key in reach, but the last code typed: the vault refuses it
        // before spending it.
        let mut operator = Operator::new(
            &desk,
            &[
                (second.as_str(), Some(0)),
                ("one key", Some(0)),
                ("yes", Some(0)),
                (last, Some(0)),
            ],
        );
        let typed = remove_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        );
        assert!(
            matches!(&typed, Err(Error::Vault { source: hatchling_vault::Error::KeyChange { reason } }) if reason.starts_with("your last backup code cannot authorize this")),
            "{typed:?}"
        );
        assert_eq!(
            operator.asked.last().map(String::as_str),
            Some(AUTHORIZE_PROMPT)
        );
        assert_eq!((keys_of(&config).len(), codes_left(&config)), (2, 1));
        // The asking and the key's unlock, on their own.
        let mut operator = Operator::new(&desk, &[("  a-code ", Some(0)), ("", None)]);
        let mut answer = |prompt: &str| -> Result<String, Error> { Ok(operator.answer(prompt)) };
        assert_eq!(
            authorization(&mut answer, &mut desk).unwrap().as_deref(),
            Some("a-code")
        );
        assert_eq!(authorization(&mut answer, &mut NoKeys).unwrap(), None);
        assert_eq!(
            operator.asked,
            [AUTHORIZE_PROMPT, AUTHORIZE_CODE_PROMPT].map(String::from)
        );
        let keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        assert!(refused_unlock(&by_key(&keyring, &mut NoKeys)));
        desk.plugged.set(Some(0));
        assert_eq!(
            by_key(&keyring, &mut desk).unwrap().opened_by(),
            Some(first.as_str())
        );
        // The other key authorizes it, and the last code is kept.
        let mut operator = Operator::new(
            &desk,
            &[
                (second.as_str(), Some(0)),
                ("one key", Some(0)),
                ("yes", Some(0)),
                ("", Some(0)),
            ],
        );
        let removed = remove_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        )
        .unwrap();
        assert_eq!(&removed.id, second);
        assert_eq!((keys_of(&config).len(), codes_left(&config)), (1, 1));
    }

    /// Whether `result` is the operator stopping (no key to unlock with).
    fn refused_unlock(result: &Result<Unlocked, Error>) -> bool {
        matches!(result, Err(Error::Usage { reason }) if reason.starts_with("stopped"))
    }

    #[test]
    fn what_cannot_be_removed_is_refused_before_authorizing() {
        let (_scratch, _vault, config, _, mut desk) = desk_vault("remove-refused", 3);
        // Keys are chosen by id: a label, or anything else, is no key.
        for typed in ["(no label)", "nope"] {
            let mut operator = Operator::new(&desk, &[(typed, None)]);
            assert!(matches!(
                remove_key(
                    &config,
                    &mut |prompt| Ok(operator.answer(prompt)),
                    &mut desk
                ),
                Err(Error::Vault {
                    source: hatchling_vault::Error::NoSuchKey
                })
            ));
        }
        // Two keys and no backup codes: refused before authorizing.
        let (_bare_scratch, _bare_vault, bare, bare_codes, mut bare_desk) =
            desk_vault("remove-no-codes", 2);
        let mut keyring = Keyring::load(bare.keyring.as_deref().unwrap()).unwrap();
        for code in &bare_codes {
            keyring.recover(code).unwrap();
        }
        let bare_first = keys_of(&bare).first().unwrap().id.clone();
        let mut operator = Operator::new(&bare_desk, &[(bare_first.as_str(), Some(0))]);
        assert!(refused(
            &remove_key(
                &bare,
                &mut |prompt| Ok(operator.answer(prompt)),
                &mut bare_desk
            ),
            "one security key and no backup codes"
        ));
        assert_eq!(operator.asked.len(), 1);
        // A vault with no keys has none to remove or name.
        let (_low_scratch, _low_vault, low) = adopted("remove-low");
        let mut operator = Operator::new(&desk, &[]);
        assert!(refused(
            &remove_key(&low, &mut |prompt| Ok(operator.answer(prompt)), &mut desk),
            "no security keys"
        ));
        assert!(refused(
            &label_key(&low, &mut |prompt| Ok(operator.answer(prompt)), &mut desk),
            "no security keys"
        ));
        assert!(refused(
            &chosen_key(&[], "name", &mut |prompt| Ok(operator.answer(prompt))),
            "no security keys"
        ));
    }

    #[test]
    fn a_key_is_named_by_id_after_authorizing() {
        let (_scratch, _vault, config, codes, mut desk) = desk_vault("label-key", 2);
        let second = keys_of(&config).last().unwrap().id.clone();
        let code = codes.first().unwrap().as_str();
        let mut operator = Operator::new(
            &desk,
            &[
                (second.as_str(), None),
                ("  Spare, drawer ", None),
                (code, None),
            ],
        );
        let named = label_key(
            &config,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        )
        .unwrap();
        assert_eq!(
            (named.id.as_str(), named.label.as_str()),
            (second.as_str(), "Spare, drawer")
        );
        assert_eq!(keys_of(&config).last(), Some(&named));
        let line = format!("security key {second}  Spare, drawer  added");
        assert!(report(&config).unwrap().contains(&line));
        let mut operator = Operator::new(&desk, &[("one", None), ("two", None), ("three", None)]);
        assert_eq!(
            asked_label(&mut |prompt| Ok(operator.answer(prompt)))
                .unwrap()
                .as_str(),
            "one"
        );
        // Authorizing with the key plugged in, and stopping with neither.
        let mut keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let mut operator = Operator::new(&desk, &[("", Some(1))]);
        let by_key = authorize(
            &mut keyring,
            &mut |prompt| Ok(operator.answer(prompt)),
            &mut desk,
        )
        .unwrap();
        assert_eq!(by_key.opened_by(), Some(second.as_str()));
        let mut operator = Operator::new(&desk, &[("", None)]);
        assert!(matches!(
            authorize(&mut keyring, &mut |prompt| Ok(operator.answer(prompt)), &mut NoKeys),
            Err(Error::Usage { reason }) if reason == "stopped; nothing was changed"
        ));
        assert!(
            matches!(stopped(), Error::Usage { reason } if reason == "stopped; nothing was changed")
        );
    }

    #[test]
    fn the_command_runs_the_key_actions_through_its_devices() {
        let (_scratch, vault, config, _, mut desk) = desk_vault("command-keys", 3);
        let first = keys_of(&config).first().unwrap().id.clone();
        let run = |action: &str, steps: &[(&str, Option<usize>)], desk: &mut Desk| {
            let mut operator = Operator::new(desk, steps);
            let mut out = Vec::new();
            command(
                &vault,
                &config,
                &secrets(&vault),
                Some(action),
                &mut |prompt| Ok(operator.answer(prompt)),
                desk,
                &mut out,
            )
            .map(|()| String::from_utf8(out).unwrap())
        };
        let added = run(
            "add-key",
            &[
                ("desk", Some(0)),
                ("yes", Some(0)),
                ("", Some(0)),
                ("", Some(3)),
            ],
            &mut desk,
        )
        .unwrap();
        assert!(
            added.starts_with("vault-keyring: added the security key ")
                && added.contains("  desk  added ")
        );
        let named = run(
            "label-key",
            &[
                (first.as_str(), Some(1)),
                ("primary", Some(1)),
                ("", Some(1)),
            ],
            &mut desk,
        )
        .unwrap();
        assert_eq!(
            named,
            format!(
                "vault-keyring: named the security key {first}  primary  added {}\n",
                today()
            )
        );
        let removed = run(
            "remove-key",
            &[(first.as_str(), Some(1)), ("yes", Some(1)), ("", Some(1))],
            &mut desk,
        )
        .unwrap();
        assert_eq!(
            removed,
            format!(
                "vault-keyring: removed the security key {first}  primary  added {}\n",
                today()
            )
        );
        let usage = run("bogus", &[], &mut desk);
        assert!(matches!(usage, Err(Error::Usage { reason }) if reason == USAGE));
        assert!(USAGE.ends_with("status | add-key | remove-key | label-key>"));
    }

    /// A software key in a tray slot: each scan of the tray sees the same
    /// key, credentials and all.
    struct Slot(Rc<RefCell<hatchling_vault::virtual_key::VirtualAuthenticator>>);

    impl Authenticator for Slot {
        fn make_credential(
            &mut self,
            rp_id: &str,
            user_id: &[u8],
        ) -> Result<hatchling_vault::Credential, hatchling_vault::AuthenticatorError> {
            self.0.borrow_mut().make_credential(rp_id, user_id)
        }

        fn hmac_secret(
            &mut self,
            rp_id: &str,
            credential_id: &[u8],
            salt: &[u8; 32],
        ) -> Result<[u8; 32], hatchling_vault::AuthenticatorError> {
            self.0.borrow_mut().hmac_secret(rp_id, credential_id, salt)
        }
    }

    /// Software keys and which of them are plugged in, as a [`Bus`].
    struct Tray {
        keys: Vec<Rc<RefCell<hatchling_vault::virtual_key::VirtualAuthenticator>>>,
        plugged: Rc<RefCell<Vec<usize>>>,
        fail: Option<String>,
    }

    impl Bus for Tray {
        type Key = Slot;

        fn keys(&mut self) -> Result<Vec<Slot>, String> {
            if let Some(why) = &self.fail {
                return Err(why.clone());
            }
            Ok(self
                .plugged
                .borrow()
                .iter()
                .filter_map(|index| self.keys.get(*index))
                .map(|key| Slot(Rc::clone(key)))
                .collect())
        }
    }

    /// `count` software keys in a tray, `plugged` of them plugged in.
    fn tray(count: usize, plugged: &[usize]) -> (SecurityKeys<Tray>, Rc<RefCell<Vec<usize>>>) {
        let plugged = Rc::new(RefCell::new(plugged.to_vec()));
        let tray = Tray {
            keys: (0..count)
                .map(|_| {
                    Rc::new(RefCell::new(
                        hatchling_vault::virtual_key::VirtualAuthenticator::new(),
                    ))
                })
                .collect(),
            plugged: Rc::clone(&plugged),
            fail: None,
        };
        (SecurityKeys::new(tray), plugged)
    }

    /// Answers in turn; every prompt is kept.
    fn answers<'a>(
        given: &'a [&'a str],
        asked: &'a mut Vec<String>,
    ) -> impl FnMut(&str) -> Result<String, Error> + 'a {
        let mut given = given.iter();
        move |prompt| {
            asked.push(prompt.to_owned());
            Ok(given
                .next()
                .map(|answer| (*answer).to_owned())
                .unwrap_or_default())
        }
    }

    fn tier_of(config: &KeyringConfig) -> Tier {
        Keyring::load(config.keyring.as_deref().unwrap())
            .unwrap()
            .tier()
    }

    #[test]
    fn security_keys_use_exactly_one_key_and_say_why_not() {
        let (mut devices, plugged) = tray(2, &[]);
        assert_eq!(devices.unreachable(), NONE_PLUGGED, "before any scan");
        assert!(devices.plugged_in().is_none());
        assert_eq!(devices.unreachable(), NONE_PLUGGED);
        *plugged.borrow_mut() = vec![1];
        assert!(devices.plugged_in().is_some());
        assert_eq!(devices.unreachable(), "");
        *plugged.borrow_mut() = vec![0, 1];
        assert!(
            devices.plugged_in().is_none(),
            "two keys are never told apart"
        );
        assert_eq!(
            devices.unreachable(),
            "2 security keys are plugged in: unplug all but one"
        );
        assert_eq!(devices.every_key().len(), 2);
        devices.bus.fail = Some(String::from(
            "/dev/hidraw3 is a security key this user may not open",
        ));
        devices.scan();
        assert!(devices.keys.is_empty());
        assert!(devices.plugged_in().is_none());
        assert!(devices.every_key().is_empty());
        assert!(devices.unreachable().contains("may not open"));
        assert_eq!(
            no_key_to_add("x"),
            "no security key can be reached (x), so none can be added; nothing was asked or changed"
        );
        let platform = platform_keys(Box::new(|| None));
        assert_eq!(platform.unreachable(), NONE_PLUGGED);
    }

    #[test]
    fn two_keys_plugged_in_at_once_move_the_vault_to_the_hardware_key_tier() {
        let (_scratch, vault, config) = adopted("enroll-two");
        let (mut devices, _) = tray(2, &[0, 1]);
        let mut asked = Vec::new();
        let kek = Kek::from_file_key(&FileKey::new(config.kek_file(&vault)).load().unwrap());
        let enrolled = enroll_keys(
            &vault,
            &config,
            &secrets(&vault),
            &mut answers(&["two", ""], &mut asked),
            &mut devices,
        )
        .unwrap();
        assert_eq!(enrolled.0, 2);
        assert!(enrolled.1.is_none(), "no codes before, none made");
        assert_eq!(asked.len(), 2, "the file tier asks nothing to unlock");
        assert_eq!(tier_of(&config), Tier::HardwareKey);
        let keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        for mut key in devices.bus.keys().unwrap() {
            assert!(keyring.unlock_hardware_key(&mut key).is_ok());
        }
        assert!(
            keyring.unlock_file(&kek).is_err() && !config.kek_file(&vault).exists(),
            "no weaker way in remains, and its KEK file is gone"
        );
    }

    #[test]
    fn one_key_needs_codes_and_the_risk_accepted() {
        let (_scratch, vault, config) = adopted("enroll-one");
        let (mut devices, _) = tray(1, &[0]);
        let mut asked = Vec::new();
        let refused_without_codes = enroll_keys(
            &vault,
            &config,
            &secrets(&vault),
            &mut answers(&["one"], &mut asked),
            &mut devices,
        );
        assert!(
            matches!(&refused_without_codes, Err(Error::Usage { reason }) if reason.contains("make them first"))
        );
        backup_codes(&vault, &config, &mut |_| Ok(String::new())).unwrap();
        let mut asked = Vec::new();
        let declined = enroll_keys(
            &vault,
            &config,
            &secrets(&vault),
            &mut answers(&["one", "no"], &mut asked),
            &mut devices,
        );
        assert!(matches!(declined, Err(Error::Usage { reason }) if reason.contains("stopped")));
        assert_eq!(tier_of(&config), Tier::File);
        let mut asked = Vec::new();
        let enrolled = enroll_keys(
            &vault,
            &config,
            &secrets(&vault),
            &mut answers(&["one", "one key", ""], &mut asked),
            &mut devices,
        )
        .unwrap();
        assert_eq!(enrolled.0, 1);
        assert_eq!(enrolled.1.map(|codes| codes.codes().count()), Some(10), "a new key, a new set");
        assert!(asked.get(1).unwrap().contains("Type \"one key\""));
        assert_eq!(tier_of(&config), Tier::HardwareKey);
        assert_eq!(keys_of(&config).len(), 1);
    }

    #[test]
    fn enrolling_keys_refuses_before_unlocking_anything() {
        let (_scratch, vault, config) = adopted("enroll-refused");
        let mut asked = Vec::new();
        let (mut one, _) = tray(2, &[0]);
        let short = enroll_keys(
            &vault,
            &config,
            &secrets(&vault),
            &mut answers(&["two", ""], &mut asked),
            &mut one,
        );
        assert!(
            matches!(&short, Err(Error::Usage { reason }) if reason.contains("two security keys are needed, plugged in at once; 1 can be reached")),
            "{short:?}"
        );
        let mut asked = Vec::new();
        let unknown = enroll_keys(
            &vault,
            &config,
            &secrets(&vault),
            &mut answers(&["three"], &mut asked),
            &mut one,
        );
        assert!(matches!(unknown, Err(Error::Usage { reason }) if reason.contains("stopped")));
        backup_codes(&vault, &config, &mut |_| Ok(String::new())).unwrap();
        let (mut none, _) = tray(1, &[]);
        let mut asked = Vec::new();
        let absent = enroll_keys(
            &vault,
            &config,
            &secrets(&vault),
            &mut answers(&["one", "one key", ""], &mut asked),
            &mut none,
        );
        assert!(
            matches!(&absent, Err(Error::Usage { reason }) if reason == "no security key can be reached (no security key is plugged in)"),
            "{absent:?}"
        );
        assert_eq!((tier_of(&config), codes_left(&config)), (Tier::File, 10));
        let (_hw_scratch, hw_vault, hw_config, _, mut desk) = desk_vault("enroll-again", 2);
        let mut asked = Vec::new();
        let again = enroll_keys(
            &hw_vault,
            &hw_config,
            &secrets(&hw_vault),
            &mut answers(&[], &mut asked),
            &mut desk,
        );
        assert!(
            matches!(&again, Err(Error::Usage { reason }) if reason.contains("already at the hardware-key tier"))
        );
        assert!(asked.is_empty());
    }

    #[test]
    fn a_key_lowers_the_vault_to_the_file_tier_and_a_backup_code_cannot() {
        let (_scratch, vault, config, codes, mut desk) = desk_vault("lower", 2);
        // A backup code never lowers the tier, so none is asked for or
        // spent: without a key the command refuses up front.
        let code = codes.first().unwrap().as_str();
        desk.plugged.set(None);
        let mut asked = Vec::new();
        let by_code = use_file_tier(
            &vault,
            &config,
            &mut answers(&["yes", code], &mut asked),
            &mut desk,
        );
        assert!(
            matches!(&by_code, Err(Error::Usage { reason })
                if reason.contains("a backup code never lowers it") && reason.contains("nothing was spent")),
            "{by_code:?}"
        );
        assert_eq!(
            (tier_of(&config), codes_left(&config)),
            (Tier::HardwareKey, 10)
        );
        assert_eq!(asked.len(), 1, "only the confirmation was asked");
        desk.plugged.set(Some(0));
        let mut asked = Vec::new();
        let declined = use_file_tier(
            &vault,
            &config,
            &mut answers(&["no"], &mut asked),
            &mut desk,
        );
        assert!(matches!(declined, Err(Error::Usage { reason }) if reason.contains("stopped")));
        // A key that does not open the vault changes nothing.
        let mut stranger = Desk {
            keys: vec![hatchling_vault::virtual_key::VirtualAuthenticator::new()],
            plugged: Rc::new(std::cell::Cell::new(Some(0))),
        };
        let mut asked = Vec::new();
        let foreign = use_file_tier(
            &vault,
            &config,
            &mut answers(&["yes", ""], &mut asked),
            &mut stranger,
        );
        assert!(matches!(foreign, Err(Error::Vault { .. })), "{foreign:?}");
        assert_eq!(tier_of(&config), Tier::HardwareKey);
        let strict = KeyringConfig {
            require_hardware_key: true,
            ..config.clone()
        };
        let mut asked = Vec::new();
        assert!(matches!(
            use_file_tier(&vault, &strict, &mut answers(&[], &mut asked), &mut desk),
            Err(Error::Usage { reason }) if reason.contains("the file tier is below it")
        ));
        assert!(asked.is_empty());
        let keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let mut asked = Vec::new();
        assert!(lowering_key(&keyring, &mut answers(&[""], &mut asked), &mut desk).is_ok());
        assert_eq!(
            tier_of(&config),
            Tier::HardwareKey,
            "an unlock alone changes nothing"
        );
        let mut asked = Vec::new();
        use_file_tier(
            &vault,
            &config,
            &mut answers(&["yes", ""], &mut asked),
            &mut desk,
        )
        .unwrap();
        assert!(
            asked
                .get(1)
                .unwrap()
                .starts_with("Plug in an enrolled security key")
        );
        assert_eq!((tier_of(&config), codes_left(&config)), (Tier::File, 10));
        let keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let kek = Kek::from_file_key(&FileKey::new(config.kek_file(&vault)).load().unwrap());
        assert!(reads_the_secret(
            &vault,
            &keyring.unlock_file(&kek).unwrap().into_key()
        ));
        assert!(
            keyring
                .unlock_hardware_key(desk.plugged_in().unwrap())
                .is_err()
        );
        let mut asked = Vec::new();
        let again = use_file_tier(&vault, &config, &mut answers(&[], &mut asked), &mut desk);
        assert!(
            matches!(again, Err(Error::Usage { reason }) if reason.contains("already at the file tier"))
        );
    }

    #[test]
    fn a_hardware_key_vault_starts_with_a_touch_and_spends_no_code() {
        let (_scratch, vault, config, _, mut desk) = desk_vault("start-touch", 2);
        let mut asked = Vec::new();
        let master =
            super::unlock_asking(&vault, &config, &mut answers(&[""], &mut asked), &mut desk)
                .unwrap();
        assert!(reads_the_secret(&vault, &master));
        assert_eq!(asked, [AUTHORIZE_PROMPT]);
        assert_eq!(codes_left(&config), 10, "a touch, not a code");
        let mut keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let mut asked = Vec::new();
        assert!(
            unlock_current(
                &mut keyring,
                &vault,
                &config,
                &mut answers(&[""], &mut asked),
                &mut desk
            )
            .is_ok()
        );
        assert!(matches!(
            open(&mut keyring, &vault, &config, Factors::default()),
            Err(Error::Usage { reason }) if reason == NO_KEY
        ));
    }

    #[test]
    fn the_command_runs_the_tier_changes() {
        let (_scratch, vault, config) = adopted("command-tiers");
        let (mut devices, _) = tray(2, &[0, 1]);
        let mut out = Vec::new();
        let mut asked = Vec::new();
        command(
            &vault,
            &config,
            &secrets(&vault),
            Some("enroll-keys"),
            &mut answers(&["two", ""], &mut asked),
            &mut devices,
            &mut out,
        )
        .unwrap();
        let mut asked = Vec::new();
        *devices.bus.plugged.borrow_mut() = vec![1];
        command(
            &vault,
            &config,
            &secrets(&vault),
            Some("use-file-tier"),
            &mut answers(&["yes", ""], &mut asked),
            &mut devices,
            &mut out,
        )
        .unwrap();
        let said = String::from_utf8(out).unwrap();
        assert_eq!(
            said,
            "vault-keyring: the vault is at the hardware-key tier with 2 security key(s), under a new key; every store is re-sealed. Name the keys with vault-keyring label-key\nvault-keyring: the vault is at the file tier; its key is in the KEK file on this machine\n"
        );
        assert!(USAGE.contains("enroll-keys | use-file-tier"));
    }

    #[test]
    fn the_console_asks_and_a_pin_is_only_what_was_typed() {
        let console = Rc::new(RefCell::new(Console {
            input: std::io::Cursor::new(b"2468\n\nanswer\n".to_vec()),
            output: Vec::new(),
        }));
        let mut pin = pin_prompt(&console);
        assert_eq!(pin().as_deref().map(String::as_str), Some("2468"));
        assert_eq!(pin(), None, "an empty answer is no PIN");
        assert_eq!(console.borrow_mut().ask("Q?").unwrap(), "answer");
        assert_eq!(pin(), None, "no input left");
        let shown = String::from_utf8(console.borrow().output.clone()).unwrap();
        assert!(shown.starts_with(PIN_PROMPT));
        assert!(shown.contains("Q? "));
        drop(Console::terminal());
        let (_asking, devices) = terminal_operator();
        assert_eq!(devices.unreachable(), NONE_PLUGGED);
    }

    #[test]
    fn a_keyring_vault_started_without_its_keyring_refuses_and_creates_no_key() {
        let (_scratch, vault, config) = adopted("no-keyring");
        let marker = keyring_marker(&vault);
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!("{}\n", config.keyring.as_deref().unwrap().display()),
            "adoption records the keyring"
        );
        let refused = unlock(&vault, &KeyringConfig::default()).unwrap_err();
        assert!(matches!(refused, Error::Usage { .. }));
        let shown = refused.to_string();
        assert!(
            shown.contains("HATCHLING_VAULT_KEYRING") && shown.contains("keyring-in-use"),
            "{shown}"
        );
        assert!(!vault.key_file.exists(), "no plain key in its place");
        // A vault adopted before the record: its keyring's next unlock writes it.
        std::fs::remove_file(&marker).unwrap();
        assert!(reads_the_secret(&vault, &unlock(&vault, &config).unwrap()));
        assert!(marker.exists());
        // Without the record, sealed files alone stop it, naming the store.
        std::fs::remove_file(&marker).unwrap();
        let refused = unlock(&vault, &KeyringConfig::default())
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains(&vault.root.display().to_string())
                && refused.contains("HATCHLING_VAULT_KEYRING"),
            "{refused}"
        );
        assert!(!vault.key_file.exists());
        // An existing record is left as it is.
        std::fs::write(&marker, b"kept\n").unwrap();
        assert!(unlock(&vault, &config).is_ok());
        assert_eq!(std::fs::read(&marker).unwrap(), b"kept\n");
    }

    #[test]
    fn a_new_vault_still_gets_its_plain_key() {
        let (_scratch, vault) = scratch("new-vault");
        // Nothing sealed yet and no keyring recorded: the key is made.
        std::fs::create_dir_all(&vault.root).unwrap();
        std::fs::set_permissions(&vault.root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(vault.root.join("plain"), b"plaintext").unwrap();
        std::fs::set_permissions(
            vault.root.join("plain"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(unlock(&vault, &KeyringConfig::default()).is_ok());
        assert!(vault.key_file.exists());
        assert!(
            !keyring_marker(&vault).exists(),
            "a plain vault records no keyring"
        );
    }

    #[test]
    fn the_keyring_record_helpers_each_do_their_one_job() {
        let (scratch, vault) = scratch("record-helpers");
        // io_error: an I/O failure, as the vault's error.
        assert!(matches!(
            io_error(std::io::Error::other("x")),
            Error::Vault {
                source: hatchling_vault::Error::Io { .. }
            }
        ));
        // refuse_new_key: nothing recorded and nothing sealed is a new vault.
        assert!(refuse_new_key(&vault).is_ok());
        // mark_keyring: nowhere to record it, nothing is written.
        let path = scratch.0.join("vault.key").join("keyring");
        mark_keyring(&vault, &path).unwrap();
        assert!(!keyring_marker(&vault).exists());
        // Beside the key file, once, naming the keyring.
        std::fs::create_dir_all(vault.key_file.parent().unwrap()).unwrap();
        mark_keyring(&vault, &path).unwrap();
        mark_keyring(&vault, Path::new("/elsewhere")).unwrap();
        let marker = keyring_marker(&vault);
        assert_eq!(marker, scratch.0.join("vault.key").join("keyring-in-use"));
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            format!(
                "{}
",
                path.display()
            )
        );
        assert_eq!(
            std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(matches!(refuse_new_key(&vault), Err(Error::Usage { .. })));
        // A record that can't be written is an error, not a skip (root
        // writes through the mode, so only as another user).
        std::fs::remove_file(&marker).unwrap();
        if rustix::process::geteuid().is_root() {
            return;
        }
        std::fs::set_permissions(
            vault.key_file.parent().unwrap(),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        let unwritable = mark_keyring(&vault, &path);
        std::fs::set_permissions(
            vault.key_file.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(
            matches!(unwritable, Err(Error::Vault { .. })),
            "{unwritable:?}"
        );
    }

    /// The secrets store and a spaces store beside it, each holding one
    /// record under `key`: the stores a rotation must carry together.
    fn two_stores(scratch: &Scratch, vault: &VaultConfig, key: &MasterKey) -> Vec<(Store, PathBuf)> {
        let spaces = scratch.0.join("spaces");
        SealedVault::open_in(&spaces, key, Store::SPACES)
            .unwrap()
            .store(&"team".parse().unwrap(), &Secret::new(b"members".to_vec()).unwrap())
            .unwrap();
        vec![(Store::SECRETS, vault.root.clone()), (Store::SPACES, spaces)]
    }

    /// Whether every store in `stores` opens under `key`.
    fn opens_all(stores: &[(Store, PathBuf)], key: &MasterKey) -> bool {
        stores.iter().all(|(store, root)| {
            let name: SecretName = (if *store == Store::SPACES { "team" } else { "token" })
                .parse()
                .unwrap();
            SealedVault::open_in(root, key, *store)
                .unwrap()
                .fetch(&name)
                .is_ok()
        })
    }

    #[test]
    fn rekey_vault_rotates_inside_the_keyring_and_every_store_follows() {
        let (scratch, vault, config) = adopted("rekey-in-keyring");
        let old = unlock(&vault, &config).unwrap();
        let stores = two_stores(&scratch, &vault, &old);
        let (reports, codes) = rekey(&vault, &config, &stores, &mut |_| Ok(String::new()), &mut NoKeys).unwrap();
        assert_eq!(
            reports
                .iter()
                .map(|(store, report)| (*store, report.rekeyed))
                .collect::<Vec<_>>(),
            [("secrets", 1), ("spaces", 1)]
        );
        assert!(codes.is_none(), "no codes before, none made");
        let new = unlock(&vault, &config).unwrap();
        assert!(opens_all(&stores, &new));
        assert!(stores.iter().all(|(store, root)| SealedVault::open_in(root, &old, *store)
            .unwrap()
            .fetch(&"token".parse().unwrap())
            .is_err()));
        assert!(!vault.key_file.exists(), "no plain key was written");
        assert!(config.kek_file(&vault).exists(), "the file tier keeps its KEK");
        // At the TOTP tier, the app's next code is all it takes.
        let mut app = App::new(0);
        enroll_totp(&vault, &config, &mut |prompt| Ok(app.answer(prompt))).unwrap();
        let sealed = std::fs::read(vault.root.join("token")).unwrap();
        rekey(&vault, &config, &secrets(&vault), &mut |_| Ok(app.code(30)), &mut NoKeys).unwrap();
        assert_ne!(std::fs::read(vault.root.join("token")).unwrap(), sealed, "re-sealed");
        // A later step's code (with the clock given) opens the new key.
        let later = now() + 90;
        let after = Keyring::load(config.keyring.as_deref().unwrap())
            .unwrap()
            .unlock_totp(
                &hatchling_vault::totp_code(app.seed.as_deref().unwrap(), later),
                later,
            )
            .unwrap();
        assert!(reads_the_secret(&vault, after.key()));
    }

    #[test]
    fn an_unfinished_rotation_refuses_to_serve_until_rekey_vault_finishes_it() {
        let (scratch, vault, config) = adopted("rekey-resume");
        let old = unlock(&vault, &config).unwrap();
        let stores = two_stores(&scratch, &vault, &old);
        // A crash right after the keyring's commit: nothing re-sealed yet.
        let mut keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let kek = Kek::from_file_key(&FileKey::new(config.kek_file(&vault)).load().unwrap());
        let unlocked = keyring.unlock_file(&kek).unwrap();
        drop(keyring.rotate_in_place(&unlocked, Some(&kek), &mut []).unwrap());
        let refused = unlock(&vault, &config).unwrap_err().to_string();
        assert!(refused.contains("rekey-vault"), "{refused}");
        assert!(matches!(
            settled(&Keyring::load(config.keyring.as_deref().unwrap()).unwrap()),
            Err(Error::Vault { .. })
        ));
        // rekey-vault resumes it: every store moves, and the vault serves again.
        let (reports, codes) = rekey(&vault, &config, &stores, &mut |_| Ok(String::new()), &mut NoKeys).unwrap();
        assert_eq!(reports.len(), 2);
        assert!(codes.is_none());
        let new = unlock(&vault, &config).unwrap();
        assert!(opens_all(&stores, &new));
        assert!(!opens_all(&stores, &old));
        // Again: a fresh rotation, which a stale store stops before anything is written.
        std::fs::write(scratch.0.join("spaces").join("stale"), b"plain").unwrap();
        std::fs::set_permissions(
            scratch.0.join("spaces").join("stale"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let keyring_bytes = std::fs::read(config.keyring.as_deref().unwrap()).unwrap();
        assert!(rekey(&vault, &config, &stores, &mut |_| Ok(String::new()), &mut NoKeys).is_err());
        assert_eq!(std::fs::read(config.keyring.as_deref().unwrap()).unwrap(), keyring_bytes);
        assert!(opens_all(&stores, &unlock(&vault, &config).unwrap()));
    }

    #[test]
    fn after_a_tier_increase_the_previous_key_material_opens_nothing() {
        let (scratch, vault, config) = adopted("tier-up");
        let old = unlock(&vault, &config).unwrap();
        let stores = two_stores(&scratch, &vault, &old);
        let keyring_path = config.keyring.clone().unwrap();
        let (keyring_copy, kek_copy) = (
            std::fs::read(&keyring_path).unwrap(),
            std::fs::read(config.kek_file(&vault)).unwrap(),
        );
        let mut app = App::new(0);
        let codes = super::enroll_totp(
            &vault,
            &config,
            &stores,
            &mut |prompt| Ok(app.answer(prompt)),
            &mut NoKeys,
        )
        .unwrap();
        assert!(codes.is_none());
        assert!(!config.kek_file(&vault).exists(), "the file tier's KEK is gone");
        // The copies from before still unwrap the old key, which opens nothing.
        let snapshot = scratch.0.join("snapshot");
        std::fs::create_dir(&snapshot).unwrap();
        std::fs::set_permissions(&snapshot, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(snapshot.join("keyring"), &keyring_copy).unwrap();
        std::fs::set_permissions(snapshot.join("keyring"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let copied = Keyring::load(&snapshot.join("keyring")).unwrap();
        let from_copy = copied
            .unlock_file(&Kek::from_bytes(kek_copy.try_into().unwrap()))
            .unwrap();
        assert!(stores.iter().all(|(store, root)| SealedVault::open_in(root, from_copy.key(), *store)
            .unwrap()
            .fetch(&"token".parse().unwrap())
            .is_err()));
        assert!(!opens_all(&stores, from_copy.key()));
        let now = unlock_asking(&vault, &config, &mut |_| Ok(app.code(30))).unwrap();
        assert!(opens_all(&stores, &now));
    }

    #[test]
    fn finishing_a_rotation_drops_the_kek_only_off_the_file_tier() {
        let (scratch, vault, config) = adopted("finish");
        let old = unlock(&vault, &config).unwrap();
        let stores = two_stores(&scratch, &vault, &old);
        let kek_file = config.kek_file(&vault);
        let mut keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let kek = Kek::from_file_key(&FileKey::new(kek_file.clone()).load().unwrap());
        let unlocked = keyring.unlock_file(&kek).unwrap();
        let rotated = keyring.rotate_in_place(&unlocked, Some(&kek), &mut []).unwrap();
        let reports = finish(&mut keyring, &rotated.rotation, &stores, &kek_file).unwrap();
        assert_eq!(reports.len(), 2);
        assert!(kek_file.exists(), "still the file tier's way in");
        assert!(!keyring.rotation_pending());
        assert!(opens_all(&stores, rotated.rotation.new_key()));
        // Run again (as after a crash): nothing left to move, nothing breaks.
        let again = finish(&mut keyring, &rotated.rotation, &stores, &kek_file).unwrap();
        assert!(again.iter().all(|(_, report)| report.rekeyed == 0));
        assert!(settled(&keyring).is_ok());
        // A KEK file that can't be removed off the file tier is an error.
        let mut app = App::new(0);
        enroll_totp(&vault, &config, &mut |prompt| Ok(app.answer(prompt))).unwrap();
        let mut keyring = Keyring::load(config.keyring.as_deref().unwrap()).unwrap();
        let stuck = scratch.0.join("stuck");
        std::fs::create_dir(&stuck).unwrap();
        std::fs::write(stuck.join("kek"), b"x").unwrap();
        let unlocked = keyring.unlock_totp(&app.code(30), now()).unwrap();
        let rotated = keyring.rotate_in_place(&unlocked, None, &mut []).unwrap();
        let secrets = secrets(&vault);
        assert!(matches!(
            finish(&mut keyring, &rotated.rotation, &secrets, &stuck),
            Err(Error::Vault { .. })
        ));
        assert!(keyring.rotation_pending(), "nothing finished");
        finish(&mut keyring, &rotated.rotation, &secrets, &scratch.0.join("gone")).unwrap();
        assert!(!keyring.rotation_pending());
    }

    #[test]
    fn new_backup_codes_are_shown_only_when_there_are_some() {
        let mut out = Vec::new();
        new_codes(&mut out, None);
        assert!(out.is_empty());
        let (_scratch, vault, config) = adopted("new-codes");
        let codes = backup_codes(&vault, &config, &mut |_| Ok(String::new())).unwrap();
        new_codes(&mut out, Some(&codes));
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.starts_with("vault-keyring: new backup codes for the new key"));
        assert_eq!(shown.lines().count(), 1 + codes.codes().count());
        assert!(codes.codes().all(|code| shown.contains(code)));
    }

    #[test]
    fn rekey_at_this_terminal_needs_nothing_at_the_file_tier() {
        let (scratch, vault, config) = adopted("run-rekey");
        let old = unlock(&vault, &config).unwrap();
        let stores = two_stores(&scratch, &vault, &old);
        let (reports, _) = run_rekey(&vault, &config, &stores).unwrap();
        assert_eq!(reports.len(), 2);
        assert!(opens_all(&stores, &unlock(&vault, &config).unwrap()));
        assert!(matches!(
            run_rekey(&vault, &KeyringConfig::default(), &stores),
            Err(Error::Usage { .. })
        ));
    }
}
