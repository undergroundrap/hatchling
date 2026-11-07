//! Unlocking the vault after a start with nobody at its terminal.
//!
//! A vault at the hardware-key tier opens only with an enrolled security
//! key (or a backup code), and one at the TOTP tier only with a current
//! code: a person has to be there. Started from a terminal, the vault
//! keeper asks on it. Started as a service, at boot, it has none: it used
//! to fail, and systemd started it again and again, so an always-on agent
//! protected by a security key never came back after a reboot.
//!
//! Now the keeper waits, locked, on a socket of its own
//! ([`socket_path`], in a directory only its user and root can reach),
//! and the operator unlocks it with `sudo hatchling-broker unlock`
//! ([`unlock_main`]), in their own terminal:
//!
//! 1. The command, as root, opens the security keys plugged in now (the
//!    keeper's user may not) and passes them to the keeper over the
//!    socket. A key plugged in, or attached to WSL, after the boot is
//!    found then.
//! 2. The keeper runs the same unlock as at a terminal
//!    ([`crate::vault_keyring::unlock_asking`]), each question it asks
//!    (the key's PIN among them) relayed to the command, which asks it in
//!    the operator's terminal, a secret one without echo, and relays the
//!    answer.
//! 3. Unlocked, the keeper carries on starting, and the command says so.
//!    Not unlocked (a wrong PIN, no touch, no key), the command says why,
//!    and the keeper waits for the next try: it doesn't exit, so systemd
//!    doesn't start it again.
//!
//! **What it holds against.** The master key never leaves the keeper:
//! the socket carries questions, answers and the keys' descriptors, and
//! the unlock happens where it always did. Only root may connect (the
//! peer's credentials are checked), as only root could start the broker
//! at a terminal. The PIN crosses a local socket between two of root's
//! processes, held in memory that's cleared after. Anyone who can't be
//! root on this machine can't unlock it, and a stolen machine, powered
//! off, opens nothing without the key.

use std::fs::File;
use std::io::{IsTerminal as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_vault::{Keyring, MasterKey, Tier};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::Error;
use crate::config::VaultConfig;
use crate::environment as env;
use crate::vault_keyring::{Devices, KeyringConfig, SecurityKeys};

/// The subcommand that unlocks a waiting vault.
pub const UNLOCK_SUBCOMMAND: &str = "unlock";

/// The socket's file, in [`unlock_dir`].
const SOCKET: &str = "unlock.sock";

/// How long one question waits for the operator's answer: long enough to
/// find a key and remember a PIN.
const ANSWER_WAIT: Duration = Duration::from_secs(300);

/// How long the operator's command waits for the keeper's next word: past
/// the keeper's own wait for an answer.
const OPERATOR_WAIT: Duration = Duration::from_secs(600);

/// The most security keys passed in one try.
const MAX_KEYS: usize = 8;

/// The longest answer relayed: past any PIN, code or backup code.
const MAX_ANSWER: usize = 256;

/// What the keeper says to the operator's command.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ToOperator {
    /// A question, to answer in the operator's terminal; a secret one
    /// (a PIN) isn't shown as it's typed.
    Ask { prompt: String, secret: bool },
    /// A line to show the operator, with no answer wanted.
    Say { message: String },
    /// The vault is open: the keeper carries on starting.
    Unlocked,
    /// This try didn't open it, and why; the keeper waits for the next.
    Failed { reason: String },
}

/// What the operator's command says to the keeper.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ToKeeper {
    /// The security keys it opened, by node: as many descriptors follow.
    Keys { nodes: Vec<String> },
    /// The answer to the last question.
    Answer { text: String },
}

/// The directory of the unlock socket: beside the vault, its keeper's
/// user's alone.
#[must_use]
pub fn unlock_dir(vault: &VaultConfig) -> PathBuf {
    vault.root.with_extension("unlock")
}

/// The unlock socket.
#[must_use]
pub fn socket_path(vault: &VaultConfig) -> PathBuf {
    unlock_dir(vault).join(SOCKET)
}

/// Whether the vault the keyring settings name needs a person to open it.
///
/// A code does at the TOTP tier, and a key or a backup code at the
/// hardware-key tier. Without a keyring, or one that doesn't load, it
/// doesn't (the unlock says why the keyring doesn't load).
#[must_use]
pub fn needs_a_person(keyring: &KeyringConfig) -> bool {
    keyring
        .keyring
        .as_deref()
        .and_then(|path| Keyring::load(path).ok())
        .is_some_and(|ring| matches!(ring.tier(), Tier::Totp | Tier::HardwareKey))
}

/// The master key, for the keeper's start: asked for at its terminal when
/// it has one, or nobody need be asked; else waited for, locked, until
/// the operator unlocks it ([`unlock_main`]).
///
/// # Errors
///
/// As [`crate::vault_keyring::unlock`] at a terminal; waiting, only when
/// the socket can't be made.
pub fn master_key_for_start(vault: &VaultConfig) -> Result<MasterKey, Error> {
    let keyring = KeyringConfig::from_env()?;
    if std::io::stdin().is_terminal() || !needs_a_person(&keyring) {
        return crate::vault_keyring::unlock(vault, &keyring);
    }
    let listener = listen(&socket_path(vault))?;
    tracing::warn!(
        socket = %socket_path(vault).display(),
        "the vault is locked: it opens with a security key or a code. Run `sudo hatchling-broker unlock` on this machine to unlock it"
    );
    let key = wait(&listener, 0, &mut |stream| {
        unlock_over(stream, vault, &keyring)
    });
    let _ = std::fs::remove_file(socket_path(vault));
    key
}

/// A fresh socket at `path`, its directory made private if it isn't
/// there, and a socket left by an earlier start replaced.
fn listen(path: &Path) -> Result<UnixListener, Error> {
    let failed = |why: String| Error::Keeper {
        reason: format!("cannot wait to be unlocked at {}: {why}", path.display()),
    };
    if let Some(dir) = path.parent() {
        if std::fs::symlink_metadata(dir).is_err() {
            hatchling_sys::create_private_dir(dir).map_err(|e| failed(e.to_string()))?;
        }
    }
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(failed(error.to_string()));
        }
        _ => {}
    }
    let listener = UnixListener::bind(path).map_err(|e| failed(e.to_string()))?;
    hatchling_sys::chmod_socket(path, 0o600).map_err(|e| failed(e.to_string()))?;
    // Past this directory, not into it: the app's host may see that the
    // socket is there, so the app can say it's locked. Only root may use it.
    if let Some(dir) = path.parent() {
        hatchling_sys::chmod_dir(dir, 0o711).map_err(|e| failed(e.to_string()))?;
    }
    Ok(listener)
}

/// Waits on `listener` until a try by a peer of uid `allowed` opens the
/// vault through `attempt`, telling each try how it went. A peer of
/// another uid is refused, and told so; a try that fails is told why.
///
/// # Errors
///
/// Only when the listener itself fails.
fn wait(
    listener: &UnixListener,
    allowed: u32,
    attempt: &mut dyn FnMut(&UnixStream) -> Result<MasterKey, Error>,
) -> Result<MasterKey, Error> {
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) => {
                return Err(Error::Keeper {
                    reason: format!("cannot wait to be unlocked: {error}"),
                });
            }
        };
        if !admitted(&stream, allowed) || stream.set_read_timeout(Some(ANSWER_WAIT)).is_err() {
            continue;
        }
        match attempt(&stream) {
            Ok(key) => {
                let _ = tell(&stream, &ToOperator::Unlocked);
                tracing::info!("the vault was unlocked");
                return Ok(key);
            }
            Err(error) => {
                tracing::warn!(%error, "an unlock didn't open the vault: it waits for the next");
                let _ = tell(
                    &stream,
                    &ToOperator::Failed {
                        reason: plainly(&error),
                    },
                );
            }
        }
    }
}

/// Whether the peer on `stream` is of uid `allowed`; one that isn't is
/// told so.
fn admitted(stream: &UnixStream, allowed: u32) -> bool {
    let peer = hatchling_sys::socket_peercred(stream.as_fd()).map(|cred| cred.uid);
    if peer.as_ref().ok() == Some(&allowed) {
        return true;
    }
    tracing::warn!(peer = ?peer.ok(), "an unlock was refused: only root may unlock the vault");
    let _ = tell(
        stream,
        &ToOperator::Failed {
            reason: String::from("only root may unlock the vault: run it with sudo"),
        },
    );
    false
}

/// One try over `stream`: the keys the operator's command passed, then
/// the vault's own unlock with them.
fn unlock_over(
    stream: &UnixStream,
    vault: &VaultConfig,
    keyring: &KeyringConfig,
) -> Result<MasterKey, Error> {
    let keys = take_keys(stream)?;
    let pin = Rc::new(stream.try_clone().map_err(|e| io(&e))?);
    let saying = stream.try_clone().map_err(|e| io(&e))?;
    let bus = hatchling_fido2::linux::Hidraw::opened(
        keys,
        Box::new(move || {
            relay(&pin, crate::vault_keyring::PIN_PROMPT, true)
                .ok()
                .filter(|pin| !pin.is_empty())
        }),
    )
    .noticing(Box::new(move |touch| {
        let message = crate::vault_keyring::touch_said(touch).to_owned();
        let _ = tell(&saying, &ToOperator::Say { message });
    }));
    unlock_relayed(stream, vault, keyring, &mut SecurityKeys::new(bus))
}

/// The vault's own unlock, as at a terminal, with `devices`, each of its
/// questions relayed over `stream`.
pub(crate) fn unlock_relayed(
    stream: &UnixStream,
    vault: &VaultConfig,
    keyring: &KeyringConfig,
    devices: &mut dyn Devices,
) -> Result<MasterKey, Error> {
    let mut ask = |prompt: &str| {
        relay(
            stream,
            prompt,
            crate::vault_keyring::asks_for_a_code(prompt),
        )
        .map(|answer| answer.to_string())
    };
    crate::vault_keyring::unlock_asking(vault, keyring, &mut ask, devices)
}

/// One try over `stream`, as [`wait`] makes it, with `devices` for the
/// keys: the keys taken, the unlock relayed, and the operator told how it
/// went.
///
/// # Errors
///
/// Why the vault didn't open, as the operator is told.
#[cfg(test)]
pub(crate) fn one_try(
    stream: &UnixStream,
    vault: &VaultConfig,
    keyring: &KeyringConfig,
    devices: &mut dyn Devices,
) -> Result<MasterKey, Error> {
    let opened = take_keys(stream).and_then(|keys| {
        assert!(keys.is_empty(), "a test passes no keys");
        unlock_relayed(stream, vault, keyring, devices)
    });
    let told = match &opened {
        Ok(_) => ToOperator::Unlocked,
        Err(error) => ToOperator::Failed {
            reason: error.to_string(),
        },
    };
    let _ = tell(stream, &told);
    opened
}

/// The security keys the operator's command passed on `stream`.
fn take_keys(stream: &UnixStream) -> Result<Vec<(String, File)>, Error> {
    let frame = Zeroizing::new(read_frame(stream).map_err(|e| framing(&e))?);
    let Ok(ToKeeper::Keys { nodes }) = serde_json::from_slice(&frame) else {
        return Err(protocol("it didn't say which security keys it opened"));
    };
    if nodes.len() > MAX_KEYS || !nodes.iter().all(|node| valid_node(node)) {
        return Err(protocol("it named security keys that aren't ones"));
    }
    nodes
        .into_iter()
        .map(|node| {
            let fd = hatchling_sys::net::recv_fd(stream.as_fd())
                .map_err(|e| protocol(&format!("a security key didn't come: {e}")))?;
            Ok((node, File::from(fd)))
        })
        .collect()
}

/// Whether `node` is a hidraw node's name.
fn valid_node(node: &str) -> bool {
    node.strip_prefix("hidraw")
        .is_some_and(|n| !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Asks `prompt` of the operator over `stream`, and returns the answer.
fn relay(stream: &UnixStream, prompt: &str, secret: bool) -> Result<Zeroizing<String>, Error> {
    tell(
        stream,
        &ToOperator::Ask {
            prompt: prompt.to_owned(),
            secret,
        },
    )?;
    let frame = Zeroizing::new(read_frame(stream).map_err(|e| framing(&e))?);
    match serde_json::from_slice::<ToKeeper>(&frame) {
        Ok(ToKeeper::Answer { text }) if text.len() <= MAX_ANSWER => Ok(Zeroizing::new(text)),
        _ => Err(protocol("its answer wasn't one")),
    }
}

fn tell(stream: &UnixStream, message: &ToOperator) -> Result<(), Error> {
    let bytes = serde_json::to_vec(message).map_err(|e| protocol(&e.to_string()))?;
    write_frame(stream, &bytes).map_err(|e| framing(&e))
}

/// Why a try failed, as the operator is told: the reason itself, without
/// the error's kind in front.
fn plainly(error: &Error) -> String {
    if matches!(
        error,
        Error::Vault {
            source: hatchling_vault::Error::RotationUnfinished
        }
    ) {
        return String::from(
            "a change to the vault's key was interrupted: run `sudo hatchling-broker vault-keyring setup` to finish it",
        );
    }
    if let Error::Usage { reason }
    | Error::Keeper { reason }
    | Error::Vault {
        source: hatchling_vault::Error::Authenticator { reason },
    } = error
    {
        reason.clone()
    } else {
        error.to_string()
    }
}

fn framing(error: &hatchling_core::Error) -> Error {
    protocol(&error.to_string())
}

fn io(error: &std::io::Error) -> Error {
    protocol(&error.to_string())
}

fn protocol(why: &str) -> Error {
    Error::Keeper {
        reason: format!("the unlock ended: {why}"),
    }
}

/// `sudo hatchling-broker unlock`: unlocks a vault waiting for it, asking
/// what it needs in this terminal.
#[must_use]
pub fn unlock_main(program: &str, vault: &VaultConfig) -> ExitCode {
    let mut stderr = std::io::stderr().lock();
    if hatchling_sys::euid() != 0 {
        let _ = writeln!(
            stderr,
            "{program} unlock: run it with sudo: only root may unlock the vault"
        );
        return ExitCode::FAILURE;
    }
    let limits = std::env::var(env::HATCHLING_CELL_CGROUP.name)
        .ok()
        .or_else(crate::doctor::service_cell_cgroup);
    if let Some(what) = crate::doctor::cell_limits_problem(limits.as_deref()) {
        let fix = crate::doctor::CELL_LIMITS_FIX;
        let _ = writeln!(
            stderr,
            "{program} unlock: not asking for your key yet: {what}. {fix}"
        );
        return ExitCode::FAILURE;
    }
    let path = socket_path(vault);
    let Ok(stream) = UnixStream::connect(&path) else {
        let _ = writeln!(
            stderr,
            "{program} unlock: Hatchling isn't waiting to be unlocked: it's unlocked already, it isn't running (sudo systemctl start hatchling), or its vault opens without a key (sudo hatchling-broker doctor)"
        );
        return ExitCode::FAILURE;
    };
    // The keeper asks for each step within its own wait; past this, it's
    // gone quiet, and the command says so rather than hang.
    let _ = stream.set_read_timeout(Some(OPERATOR_WAIT));
    let keys = crate::keeper::open_security_keys();
    let _ = writeln!(
        stderr,
        "Unlocking Hatchling's vault. {}",
        match keys.len() {
            0 =>
                "No security key is plugged in: a backup code works, or plug one in and run this again (in WSL, attach it with usbipd first).",
            _ => "It'll say when to touch your key.",
        }
    );
    drop(stderr);
    let service = service_running();
    match operate(&stream, &keys, &mut terminal_answer) {
        Ok(()) => {
            std::thread::sleep(SETTLE);
            let stayed = !service || service_running();
            let said = crate::windows_setup::here(unlocked_said(stayed));
            let _ = writeln!(std::io::stderr().lock(), "{said}");
            remind();
            if stayed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(reason) => {
            let _ = writeln!(
                std::io::stderr().lock(),
                "{program} unlock: not unlocked: {reason}. Hatchling is still waiting: run this again to try again."
            );
            ExitCode::FAILURE
        }
    }
}

/// What's still unfinished about the keys, said after an unlock.
fn remind() {
    let Ok(config) = KeyringConfig::from_env() else {
        return;
    };
    let Some(path) = config.keyring.as_deref() else {
        return;
    };
    let mut stderr = std::io::stderr().lock();
    if crate::key_setup::codes_unconfirmed(path) {
        let _ = writeln!(
            stderr,
            "Your backup codes were never confirmed saved: sudo hatchling-broker vault-keyring setup makes a fresh set."
        );
    }
    for reminder in crate::key_seen::reminders(&config, crate::vault_keyring::now()) {
        let _ = writeln!(stderr, "{reminder}");
    }
}

/// An answer to `prompt` in this terminal; a secret one isn't shown.
fn terminal_answer(prompt: &str, secret: bool) -> Result<Zeroizing<String>, String> {
    crate::vault_keyring::terminal_answer(prompt, secret).map_err(|e| e.to_string())
}

/// How long to watch a service that's just been unlocked: one that can't
/// run stops within this.
const SETTLE: Duration = Duration::from_secs(2);

/// Whether the service is running now.
fn service_running() -> bool {
    std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "hatchling"])
        .status()
        .is_ok_and(|status| status.success())
}

/// What to say once the vault's open: only what's true of the service.
const fn unlocked_said(stayed: bool) -> &'static str {
    if stayed {
        "Unlocked: Hatchling is starting. The app's address, as yourself: hatchling-host url"
    } else {
        "The vault opened, but Hatchling stopped straight after, and it's locked again. See: sudo hatchling-broker doctor, and journalctl -u hatchling"
    }
}

/// How the operator answers a question: the prompt, and whether it's a
/// secret, in; the answer out.
pub(crate) type Answer<'a> = &'a mut dyn FnMut(&str, bool) -> Result<Zeroizing<String>, String>;

/// The operator's side over `stream`: `keys` passed, then each question
/// answered by `answer`, until the keeper says how it went.
pub(crate) fn operate(
    stream: &UnixStream,
    keys: &[(String, File)],
    answer: Answer<'_>,
) -> Result<(), String> {
    let gone = |e: hatchling_core::Error| format!("Hatchling stopped answering: {e}");
    // Past a question, the likely reason is the plain one: it waited, and gave up.
    let late = |e: hatchling_core::Error| {
        tracing::debug!(error = %e, "the keeper went after a question");
        String::from(
            "Hatchling stopped waiting: it gives each question 5 minutes. Nothing has changed",
        )
    };
    let nodes: Vec<String> = keys.iter().map(|(node, _)| node.clone()).collect();
    let keys_frame = serde_json::to_vec(&ToKeeper::Keys { nodes }).map_err(|e| e.to_string())?;
    if let Err(error) = write_frame(stream, &keys_frame) {
        // A keeper that refused this command said why before it closed.
        return Err(last_word(stream).unwrap_or_else(|| gone(error)));
    }
    for (_, key) in keys {
        hatchling_sys::net::send_fd(stream.as_fd(), key.as_fd()).map_err(|e| e.to_string())?;
    }
    let mut asked = false;
    loop {
        let frame = match read_frame(stream) {
            Ok(frame) => frame,
            Err(error) if asked => return Err(late(error)),
            Err(error) => return Err(gone(error)),
        };
        match serde_json::from_slice::<ToOperator>(&frame) {
            Ok(ToOperator::Ask { prompt, secret }) => {
                let text = answer(&prompt, secret)?;
                let reply = Zeroizing::new(
                    serde_json::to_vec(&ToKeeper::Answer {
                        text: text.to_string(),
                    })
                    .map_err(|e| e.to_string())?,
                );
                write_frame(stream, &reply).map_err(late)?;
                asked = true;
            }
            Ok(ToOperator::Say { message }) => {
                let _ = writeln!(std::io::stderr(), "{message}");
            }
            Ok(ToOperator::Unlocked) => return Ok(()),
            Ok(ToOperator::Failed { reason }) => return Err(reason),
            Err(_) => {
                return Err(String::from(
                    "Hatchling said something this command doesn't read",
                ));
            }
        }
    }
}

/// What the keeper said last on `stream`, if it said why it stopped.
fn last_word(stream: &UnixStream) -> Option<String> {
    let frame = read_frame(stream).ok()?;
    match serde_json::from_slice::<ToOperator>(&frame) {
        Ok(ToOperator::Failed { reason }) => Some(reason),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
