//! `use-hosted` and `use-local`: the model, its server, its window and the
//! egress that reaches it, switched together in one command.
//!
//! Done by hand, a switch is seven settings in the service's file
//! ([`crate::doctor::SERVICE_SETTINGS`]) that must agree: the endpoint,
//! the model, thinking, the window, the allowlist, the egress grants and
//! the app's grants. One typo and the agent can't reach its model. This
//! command makes them agree:
//!
//! 1. It checks what it's given: the model's name, the endpoint, and, for
//!    a local server, that the server has the model.
//! 2. For a hosted one, it makes sure the vault holds the server's key,
//!    asking for it at `secret add`'s prompt when it doesn't (a key typed
//!    or piped, never an argument). A key not given stops it: nothing
//!    else changes.
//! 3. It backs the file up, beside it, and writes it whole, the model's
//!    and egress's lines changed and every other line as it was. The old
//!    server stays allowed, so switching back is this command again.
//! 4. It forgets a model the owner chose in the app ([`crate::model_choice`]),
//!    which was one on the old server, and restarts the broker.
//!
//! It runs as root, before the service's settings are taken on, since it
//! writes the file they're read from and restarts the service.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::ExitCode;

use hatchling_core::cli::EX_USAGE;

use crate::environment as env;

/// The command to use a hosted model.
pub const USE_HOSTED: &str = "use-hosted";

/// The command to use a local model.
pub const USE_LOCAL: &str = "use-local";

/// `OpenRouter`: its API, and the free model with a million-token window
/// that calls tools well with thinking off (docs/hosted-models.md).
const OPENROUTER: &str = "https://openrouter.ai/api/v1";
const OPENROUTER_MODEL: &str = "nvidia/nemotron-3-ultra-550b-a55b:free";
const OPENROUTER_WINDOW: u32 = 1_000_000;

/// Ollama's API on this machine, a local model's server by default.
const OLLAMA: &str = "http://localhost:11434/v1";

/// How the commands are used.
const USAGE: &str = "usage: sudo hatchling-broker use-hosted openrouter [--model <id>] [--window <tokens>] [--thinking off|on] [--no-restart]\n       sudo hatchling-broker use-hosted --endpoint https://<host>[:<port>]/<path> --model <id> [--window <tokens>] [--thinking off|on] [--no-restart]\n       sudo hatchling-broker use-local --model <name> [--endpoint http://localhost:<port>/<path>] [--window <tokens>] [--thinking off|on] [--no-restart]";

/// What to switch to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The API's base URL.
    pub endpoint: String,
    /// Its `host:port`: what egress allows and grants.
    pub destination: String,
    /// The model's id on it.
    pub model: String,
    /// Its window in tokens, when known; none is learned (Ollama says
    /// it) or unknown.
    pub window: Option<u32>,
    /// Thinking off (`disabled`), or the provider's default (`None`).
    pub thinking_off: bool,
    /// Whether it's a server on this machine, reached in plain HTTP.
    pub local: bool,
    /// Whether to restart the broker after.
    pub restart: bool,
}

/// What's asked for, read from `args` (after the command), or what's
/// wrong with it.
///
/// # Errors
///
/// What's wrong, in a sentence, for a usage error.
pub fn target(command: &str, args: &[&str]) -> Result<Target, String> {
    let (preset, rest) = match (command, args) {
        (USE_HOSTED, ["openrouter", rest @ ..]) => (Some("openrouter"), rest),
        (USE_HOSTED | USE_LOCAL, rest) => (None, rest),
        _ => return Err(String::from("not a model switch")),
    };
    let (mut endpoint, mut model, mut window, mut thinking, mut restart) =
        (None, None, None, None, true);
    let mut words = rest.iter();
    while let Some(flag) = words.next() {
        let mut value = || {
            words
                .next()
                .map(|value| (*value).to_owned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match *flag {
            "--endpoint" => endpoint = Some(value()?),
            "--model" => model = Some(value()?),
            "--window" => window = Some(value()?),
            "--thinking" => thinking = Some(value()?),
            "--no-restart" => restart = false,
            other => return Err(format!("{other} isn't an option of {command}")),
        }
    }
    let local = command == USE_LOCAL;
    let endpoint = match (preset, endpoint) {
        (Some(_), Some(_)) => return Err(String::from("openrouter names its endpoint already")),
        (Some(_), None) => String::from(OPENROUTER),
        (None, Some(endpoint)) => endpoint,
        (None, None) if local => String::from(OLLAMA),
        (None, None) => return Err(String::from("say which: openrouter, or --endpoint")),
    };
    let destination = destination_of(&endpoint, local)?;
    let model = match (preset, model) {
        (_, Some(model)) => model,
        (Some(_), None) => String::from(OPENROUTER_MODEL),
        (None, None) => return Err(String::from("--model names the model")),
    };
    if !crate::model_choice::valid_name(&model) {
        return Err(format!("{model:?} can't be a model's id"));
    }
    let preset_model = preset.is_some() && model == OPENROUTER_MODEL;
    let window = match window {
        Some(window) => Some(
            window
                .parse::<u32>()
                .ok()
                .filter(|window| *window >= crate::config::MIN_CONTEXT_WINDOW)
                .ok_or_else(|| {
                    format!(
                        "--window is a number of tokens, at least {}",
                        crate::config::MIN_CONTEXT_WINDOW
                    )
                })?,
        ),
        None if preset_model => Some(OPENROUTER_WINDOW),
        None => None,
    };
    let thinking_off = match thinking.as_deref() {
        Some("off") => true,
        Some("on") => false,
        Some(other) => return Err(format!("--thinking is off or on, not {other:?}")),
        None => preset_model,
    };
    Ok(Target {
        endpoint,
        destination,
        model,
        window,
        thinking_off,
        local,
        restart,
    })
}

/// The `host:port` of `endpoint`: `https` for a hosted server (port 443
/// unless it says), plain `http` on this machine for a local one. Its
/// characters are a URL's plain ones, so it's written as it is.
fn destination_of(endpoint: &str, local: bool) -> Result<String, String> {
    let plain = endpoint
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-._~:/%".contains(&byte));
    if !plain {
        return Err(format!("{endpoint:?} has a character an endpoint doesn't"));
    }
    let (scheme, default_port) = if local {
        ("http://", None)
    } else {
        ("https://", Some("443"))
    };
    let rest = endpoint.strip_prefix(scheme).ok_or_else(|| {
        if local {
            format!("{endpoint} isn't http://localhost:<port>/...: a local model's server is on this machine")
        } else {
            format!("{endpoint} isn't https://...: a hosted model is reached over TLS")
        }
    })?;
    let authority = rest
        .split_once('/')
        .map_or(rest, |(authority, _)| authority);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, default_port),
    };
    let port = port
        .and_then(|port| port.parse::<u16>().ok())
        .filter(|port| *port != 0);
    let Some(port) = port.filter(|_| !host.is_empty()) else {
        return Err(format!("{endpoint} has no host and port"));
    };
    if local && !matches!(host, "localhost" | "127.0.0.1") {
        return Err(format!(
            "{endpoint} isn't on this machine: use-local is for localhost; a server elsewhere is use-hosted"
        ));
    }
    Ok(format!("{host}:{port}"))
}

/// `list` (comma-separated), with `item` added at its end unless an item
/// is already `same` as it.
fn with_item(list: Option<&String>, item: &str, same: impl Fn(&str) -> bool) -> String {
    let mut items: Vec<&str> = list
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default();
    if !items.iter().any(|existing| same(existing)) {
        items.push(item);
    }
    items.join(",")
}

/// An allowlist entry's `host:port`, its qualifier taken off.
fn allowed(entry: &str) -> &str {
    ["public:", "local:", "local-keyless:", "internal:"]
        .iter()
        .find_map(|qualifier| entry.strip_prefix(qualifier))
        .unwrap_or(entry)
}

/// The settings to change, from `settings`, to switch to `target`: each
/// key's new value, or `None` to take its line out.
#[must_use]
pub fn changes(
    settings: &BTreeMap<String, String>,
    target: &Target,
) -> Vec<(&'static str, Option<String>)> {
    let destination = target.destination.as_str();
    let entry = if target.local {
        format!("local-keyless:{destination}")
    } else {
        destination.to_owned()
    };
    let mut changes = vec![
        (
            env::HATCHLING_SCHEDULER_MODEL_ENDPOINT.name,
            Some(target.endpoint.clone()),
        ),
        (
            env::HATCHLING_SCHEDULER_MODEL_NAME.name,
            Some(target.model.clone()),
        ),
        (
            env::HATCHLING_SCHEDULER_MODEL_REASONING.name,
            target.thinking_off.then(|| String::from("disabled")),
        ),
        (
            env::HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW.name,
            target.window.map(|window| window.to_string()),
        ),
        (
            env::HATCHLING_EGRESS_ALLOWLIST.name,
            Some(with_item(
                settings.get(env::HATCHLING_EGRESS_ALLOWLIST.name),
                &entry,
                |existing| allowed(existing) == destination,
            )),
        ),
        (
            env::HATCHLING_EGRESS_GRANTS.name,
            Some(with_item(
                settings.get(env::HATCHLING_EGRESS_GRANTS.name),
                destination,
                |existing| existing == destination,
            )),
        ),
    ];
    // The app's chat calls the model with the app's grants: where it has
    // some, the new server is among them.
    if let Some(grants) = settings.get(env::HATCHLING_API_GRANTS.name) {
        let grant = format!("egress:{destination}");
        changes.push((
            env::HATCHLING_API_GRANTS.name,
            Some(with_item(Some(grants), &grant, |existing| {
                existing == grant
            })),
        ));
    }
    changes
}

/// `text`, a settings file, with `changes` made.
///
/// A changed key's first line holds its new value (later ones go), a key
/// set to `None` loses its lines, and a new key goes at the end. Every
/// other line, comments included, is as it was.
#[must_use]
pub fn edited(text: &str, changes: &[(&str, Option<String>)]) -> String {
    let key_of = |line: &str| -> Option<String> {
        let line = line.trim_start();
        if line.starts_with(['#', ';']) {
            return None;
        }
        line.split_once('=').map(|(key, _)| key.trim().to_owned())
    };
    let mut written = std::collections::BTreeSet::new();
    let mut out = String::new();
    for line in text.lines() {
        let change = key_of(line).and_then(|key| {
            changes
                .iter()
                .find(|(changed, _)| *changed == key)
                .map(|(changed, value)| (*changed, value))
        });
        match change {
            None => {
                out.push_str(line);
                out.push('\n');
            }
            Some((key, Some(value))) if written.insert(key) => {
                let _ = writeln!(out, "{key}={value}");
            }
            Some(_) => {}
        }
    }
    for (key, value) in changes {
        if let Some(value) = value {
            if !written.contains(key) {
                let _ = writeln!(out, "{key}={value}");
            }
        }
    }
    out
}

/// Runs `use-hosted` or `use-local` with `args` (the command first).
#[must_use]
pub fn switch_main(program: &str, args: &[String]) -> ExitCode {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let (command, rest) = words
        .split_first()
        .map_or(("", &[][..]), |(command, rest)| (*command, rest));
    let mut err = std::io::stderr().lock();
    let target = match target(command, rest) {
        Ok(target) => target,
        Err(why) => {
            let _ = writeln!(err, "{program} {command}: {why}\n{USAGE}");
            return ExitCode::from(EX_USAGE);
        }
    };
    drop(err);
    match switch(&target, Path::new(crate::doctor::SERVICE_SETTINGS)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            let _ = writeln!(std::io::stderr().lock(), "{program} {command}: {why}");
            ExitCode::FAILURE
        }
    }
}

/// Switches the service whose settings are at `path` to `target`, as
/// root: the file must be root's, the key is asked for by this program's
/// `secret add`, and the broker is restarted by `systemctl`.
fn switch(target: &Target, path: &Path) -> Result<(), String> {
    if hatchling_sys::euid() != 0 {
        return Err(String::from(
            "it changes the service's settings and restarts it: run it with sudo",
        ));
    }
    let program = std::env::current_exe()
        .map_err(|err| format!("can't find this program to ask for the key: {err}"))?;
    let steps = Steps {
        owner: 0,
        ask_key: &|destination| ask_key(&program, destination),
        restart: &|| restart(Path::new("systemctl")),
    };
    switch_with(target, path, &steps, &mut std::io::stdout().lock())
}

/// What a switch does outside the settings file, so each can be told
/// apart: the file's owner, asking for a key, restarting the broker.
struct Steps<'a> {
    /// Whose the settings file must be.
    owner: u32,
    /// Makes sure the vault holds the key for a `host:port`.
    ask_key: &'a dyn Fn(&str) -> Result<(), String>,
    /// Restarts the broker.
    restart: &'a dyn Fn() -> Result<(), String>,
}

/// Switches the settings at `path` to `target`, with `steps`, saying what
/// it did on `out`.
fn switch_with(
    target: &Target,
    path: &Path,
    steps: &Steps<'_>,
    out: &mut dyn std::io::Write,
) -> Result<(), String> {
    let text = crate::service_settings::read_trusted_text(path, steps.owner)
        .map_err(|unread| format!("{}\n{}", unread.what, unread.fix))?;
    let settings = crate::doctor::parse_settings(&text);
    if target.local {
        has_model(target)?;
    } else {
        let _ = writeln!(out, "The key for {}:", target.destination);
        (steps.ask_key)(&target.destination)?;
    }
    let backup = rewrite(path, &text, &changes(&settings, target))?;
    let _ = writeln!(
        out,
        "Hatchling uses {} at {}{}{}. The old settings are in {}.",
        target.model,
        target.endpoint,
        target
            .window
            .map_or_else(String::new, |window| format!(", with a {window}-token window")),
        if target.thinking_off { ", thinking off" } else { "" },
        backup.display()
    );
    if let Some(root) = settings.get(env::HATCHLING_SCHEDULER_DIR.name) {
        let choice =
            crate::config::owner_dir_of(Path::new(root)).join(crate::model_choice::MODEL_FILE);
        if std::fs::remove_file(&choice).is_ok() {
            let _ = writeln!(
                out,
                "The model chosen in the app was one on the old server: it's forgotten."
            );
        }
    }
    if !target.restart {
        let _ = writeln!(
            out,
            "Restart the broker to use it: `sudo systemctl restart hatchling`."
        );
        return Ok(());
    }
    (steps.restart)()?;
    let _ = writeln!(out, "Restarted the broker. Send a message in the app to check it.");
    Ok(())
}

/// Whether the local server `target` names has its model: before
/// anything changes, a model it hasn't is refused.
fn has_model(target: &Target) -> Result<(), String> {
    let models = crate::model_choice::available(&target.endpoint).ok_or_else(|| {
        format!(
            "nothing at {} lists its models: start the model's server (for Ollama, `sudo systemctl start ollama`); nothing was changed",
            target.endpoint
        )
    })?;
    let latest = format!("{}:latest", target.model);
    if models.iter().any(|id| *id == target.model || *id == latest) {
        return Ok(());
    }
    Err(format!(
        "the server at {} has no model {}: fetch it first (for Ollama, `ollama pull {}`); nothing was changed",
        target.endpoint, target.model, target.model
    ))
}

/// Makes sure the vault holds the key for `destination`, by `program`'s
/// `secret add --for <destination> --if-missing`: asked for at its prompt,
/// or from a pipe, when it doesn't.
fn ask_key(program: &Path, destination: &str) -> Result<(), String> {
    let given = std::process::Command::new(program)
        .args(["secret", "add", "--for", destination, "--if-missing"])
        .status()
        .map_err(|err| format!("can't ask for the key: {err}"))?;
    if given.success() {
        Ok(())
    } else {
        Err(String::from("the key wasn't stored, so nothing was changed"))
    }
}

/// Restarts the broker's service with `systemctl`, the program at that
/// name or path.
fn restart(systemctl: &Path) -> Result<(), String> {
    let restarted = std::process::Command::new(systemctl)
        .args(["restart", "hatchling.service"])
        .status()
        .map_err(|err| {
            format!("can't restart the broker: {err}; `sudo systemctl restart hatchling` does")
        })?;
    if restarted.success() {
        Ok(())
    } else {
        Err(String::from(
            "the broker didn't restart: see `journalctl -u hatchling`, and `sudo hatchling-broker doctor`",
        ))
    }
}

/// Writes `text`, the settings at `path`, with `changes` made: the old
/// file backed up beside it first, the new one written whole beside and
/// put in place. Answers where the backup is.
fn rewrite(
    path: &Path,
    text: &str,
    changes: &[(&str, Option<String>)],
) -> Result<std::path::PathBuf, String> {
    let backup = path.with_extension(format!(
        "env.before-{}",
        hatchling_scheduler::Timestamp::now().as_second()
    ));
    std::fs::copy(path, &backup)
        .map_err(|err| format!("can't back up {}: {err}", path.display()))?;
    let next = path.with_extension("env.next");
    let write = || -> std::io::Result<()> {
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&next)?
            .write_all(edited(text, changes).as_bytes())?;
        std::fs::rename(&next, path)
    };
    write().map_err(|err| format!("can't write {}: {err}; it's as it was", path.display()))?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> String {
        String::from(text)
    }

    #[test]
    fn openrouter_is_its_free_million_token_model_with_thinking_off() {
        let openrouter = target(USE_HOSTED, &["openrouter"]).unwrap();
        assert_eq!(
            openrouter,
            Target {
                endpoint: s(OPENROUTER),
                destination: s("openrouter.ai:443"),
                model: s(OPENROUTER_MODEL),
                window: Some(1_000_000),
                thinking_off: true,
                local: false,
                restart: true,
            }
        );
        // Another of its models: its window and thinking are as said.
        let other = target(
            USE_HOSTED,
            &["openrouter", "--model", "google/gemma-4-31b-it:free"],
        )
        .unwrap();
        assert_eq!((other.window, other.thinking_off), (None, false));
        let said = target(
            USE_HOSTED,
            &[
                "openrouter",
                "--model",
                "m",
                "--window",
                "262144",
                "--thinking",
                "off",
                "--no-restart",
            ],
        )
        .unwrap();
        assert_eq!(
            (said.window, said.thinking_off, said.restart),
            (Some(262_144), true, false)
        );
    }

    #[test]
    fn a_hosted_endpoint_is_https_and_a_local_one_on_this_machine() {
        let hosted = target(
            USE_HOSTED,
            &["--endpoint", "https://api.example.com/v1", "--model", "m"],
        )
        .unwrap();
        assert_eq!(hosted.destination, "api.example.com:443");
        let port = target(
            USE_HOSTED,
            &[
                "--endpoint",
                "https://api.example.com:8443/v1",
                "--model",
                "m",
            ],
        )
        .unwrap();
        assert_eq!(port.destination, "api.example.com:8443");
        let local = target(USE_LOCAL, &["--model", "qwen3.5:9b"]).unwrap();
        assert_eq!(
            (
                local.endpoint.as_str(),
                local.destination.as_str(),
                local.local,
                local.window,
                local.thinking_off
            ),
            (OLLAMA, "localhost:11434", true, None, false)
        );
        for (command, args) in [
            (
                USE_HOSTED,
                &["--endpoint", "http://api.example.com/v1", "--model", "m"][..],
            ),
            (
                USE_HOSTED,
                &[
                    "--endpoint",
                    "https://user@api.example.com/v1",
                    "--model",
                    "m",
                ],
            ),
            (
                USE_HOSTED,
                &["--endpoint", "https://api.example.com:0/v1", "--model", "m"],
            ),
            (
                USE_HOSTED,
                &["--endpoint", "https://:443/v1", "--model", "m"],
            ),
            (
                USE_HOSTED,
                &[
                    "--endpoint",
                    "https://api.example.com/v1?x=1",
                    "--model",
                    "m",
                ],
            ),
            (
                USE_LOCAL,
                &["--endpoint", "http://192.168.1.5:11434/v1", "--model", "m"],
            ),
            (
                USE_LOCAL,
                &["--endpoint", "https://localhost:11434/v1", "--model", "m"],
            ),
            (
                USE_LOCAL,
                &["--endpoint", "http://localhost/v1", "--model", "m"],
            ),
        ] {
            assert!(target(command, args).is_err(), "{command} {args:?}");
        }
    }

    #[test]
    fn what_isnt_asked_for_properly_is_refused() {
        for (command, args) in [
            (USE_HOSTED, &[][..]),
            (USE_HOSTED, &["--model", "m"]),
            (
                USE_HOSTED,
                &["openrouter", "--endpoint", "https://x.example/v1"],
            ),
            (USE_HOSTED, &["openrouter", "--model"]),
            (USE_HOSTED, &["openrouter", "--model", "two words"]),
            (USE_HOSTED, &["openrouter", "--window", "4096"]),
            (USE_HOSTED, &["openrouter", "--window", "many"]),
            (USE_HOSTED, &["openrouter", "--thinking", "maybe"]),
            (USE_HOSTED, &["openrouter", "--fast"]),
            (USE_HOSTED, &["anthropic"]),
            (USE_LOCAL, &[]),
            ("use-other", &["openrouter"]),
        ] {
            assert!(target(command, args).is_err(), "{command} {args:?}");
        }
    }

    fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (s(key), s(value)))
            .collect()
    }

    #[test]
    fn the_changes_make_the_seven_settings_agree_and_keep_the_old_server() {
        let local = settings(&[
            (
                "HATCHLING_EGRESS_ALLOWLIST",
                "local-keyless:localhost:11434",
            ),
            ("HATCHLING_EGRESS_GRANTS", "localhost:11434"),
            (
                "HATCHLING_API_GRANTS",
                "egress:localhost:11434,channel-send:local",
            ),
        ]);
        let hosted = changes(&local, &target(USE_HOSTED, &["openrouter"]).unwrap());
        assert_eq!(
            hosted,
            vec![
                ("HATCHLING_SCHEDULER_MODEL_ENDPOINT", Some(s(OPENROUTER))),
                ("HATCHLING_SCHEDULER_MODEL_NAME", Some(s(OPENROUTER_MODEL))),
                ("HATCHLING_SCHEDULER_MODEL_REASONING", Some(s("disabled"))),
                (
                    "HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW",
                    Some(s("1000000"))
                ),
                (
                    "HATCHLING_EGRESS_ALLOWLIST",
                    Some(s("local-keyless:localhost:11434,openrouter.ai:443"))
                ),
                (
                    "HATCHLING_EGRESS_GRANTS",
                    Some(s("localhost:11434,openrouter.ai:443"))
                ),
                (
                    "HATCHLING_API_GRANTS",
                    Some(s(
                        "egress:localhost:11434,channel-send:local,egress:openrouter.ai:443"
                    ))
                ),
            ]
        );
        // Back: the local server's entries are there, so nothing is added;
        // thinking and the window go back to the server's.
        let switched = settings(&[
            (
                "HATCHLING_EGRESS_ALLOWLIST",
                "local-keyless:localhost:11434,openrouter.ai:443",
            ),
            (
                "HATCHLING_EGRESS_GRANTS",
                "localhost:11434,openrouter.ai:443",
            ),
        ]);
        let back = changes(
            &switched,
            &target(USE_LOCAL, &["--model", "qwen3.5:9b"]).unwrap(),
        );
        assert_eq!(
            back,
            vec![
                ("HATCHLING_SCHEDULER_MODEL_ENDPOINT", Some(s(OLLAMA))),
                ("HATCHLING_SCHEDULER_MODEL_NAME", Some(s("qwen3.5:9b"))),
                ("HATCHLING_SCHEDULER_MODEL_REASONING", None),
                ("HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW", None),
                (
                    "HATCHLING_EGRESS_ALLOWLIST",
                    Some(s("local-keyless:localhost:11434,openrouter.ai:443"))
                ),
                (
                    "HATCHLING_EGRESS_GRANTS",
                    Some(s("localhost:11434,openrouter.ai:443"))
                ),
            ],
            "no app grants set, none added"
        );
        // An entry for the server with another qualifier counts as it.
        let public = changes(
            &settings(&[("HATCHLING_EGRESS_ALLOWLIST", "public:openrouter.ai:443")]),
            &target(USE_HOSTED, &["openrouter"]).unwrap(),
        );
        assert!(public.contains(&(
            "HATCHLING_EGRESS_ALLOWLIST",
            Some(s("public:openrouter.ai:443"))
        )));
        assert!(public.contains(&("HATCHLING_EGRESS_GRANTS", Some(s("openrouter.ai:443")))));
    }

    #[test]
    fn the_file_keeps_every_other_line_and_takes_each_change_once() {
        let text = "# Hatchling\nHATCHLING_SCHEDULER_MODEL_NAME=qwen3.5:9b\n\n; old\nHATCHLING_OTHER=kept\nHATCHLING_SCHEDULER_MODEL_NAME=again\nHATCHLING_SCHEDULER_MODEL_REASONING=disabled\n# HATCHLING_SCHEDULER_MODEL_NAME=commented\n";
        let changed = edited(
            text,
            &[
                ("HATCHLING_SCHEDULER_MODEL_NAME", Some(s("gemma4:12b"))),
                ("HATCHLING_SCHEDULER_MODEL_REASONING", None),
                ("HATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW", Some(s("32768"))),
            ],
        );
        assert_eq!(
            changed,
            "# Hatchling\nHATCHLING_SCHEDULER_MODEL_NAME=gemma4:12b\n\n; old\nHATCHLING_OTHER=kept\n# HATCHLING_SCHEDULER_MODEL_NAME=commented\nHATCHLING_SCHEDULER_MODEL_CONTEXT_WINDOW=32768\n"
        );
        assert_eq!(edited("A=1", &[]), "A=1\n", "a last line gets its newline");
        assert_eq!(edited("", &[("A", None)]), "");
    }

    #[test]
    fn an_allowlist_entry_is_its_host_and_port() {
        for (entry, bare) in [
            ("openrouter.ai:443", "openrouter.ai:443"),
            ("public:openrouter.ai:443", "openrouter.ai:443"),
            ("local:localhost:11434", "localhost:11434"),
            ("local-keyless:localhost:11434", "localhost:11434"),
            ("internal:db.lan:5432", "db.lan:5432"),
        ] {
            assert_eq!(allowed(entry), bare);
        }
        assert_eq!(with_item(None, "a:1", |item| item == "a:1"), "a:1");
        assert_eq!(
            with_item(Some(&s(" a:1 , ,b:2")), "a:1", |item| item == "a:1"),
            "a:1,b:2"
        );
    }

    /// A settings file in a fresh directory, as the service's would be,
    /// but this user's, with a scheduler root whose owner directory holds
    /// a model chosen in the app.
    fn service(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!(
            "hatchling-switch-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("schedules.owner")).unwrap();
        std::fs::write(dir.join("schedules.owner").join(crate::model_choice::MODEL_FILE), "{\"name\":\"gemma4:12b\"}").unwrap();
        let path = dir.join("hatchling.env");
        std::fs::write(
            &path,
            format!(
                "# Hatchling\nHATCHLING_SCHEDULER_DIR={}\nHATCHLING_SCHEDULER_MODEL_ENDPOINT=http://localhost:11434/v1\nHATCHLING_SCHEDULER_MODEL_NAME=qwen3.5:9b\nHATCHLING_EGRESS_ALLOWLIST=local-keyless:localhost:11434\nHATCHLING_EGRESS_GRANTS=localhost:11434\n",
                dir.join("schedules").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        (dir, path)
    }

    /// The backups beside `path`.
    fn backups(path: &Path) -> usize {
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("hatchling.env.before-"))
            })
            .count()
    }

    #[test]
    fn a_switch_asks_for_the_key_writes_the_settings_and_restarts() {
        let (dir, path) = service("hosted");
        let asked = std::cell::RefCell::new(Vec::new());
        let restarted = std::cell::Cell::new(0_u32);
        let steps = Steps {
            owner: hatchling_sys::euid(),
            ask_key: &|destination| {
                asked.borrow_mut().push(destination.to_owned());
                Ok(())
            },
            restart: &|| {
                restarted.set(restarted.get().saturating_add(1));
                Ok(())
            },
        };
        let mut said = Vec::new();
        let openrouter = target(USE_HOSTED, &["openrouter"]).unwrap();
        switch_with(&openrouter, &path, &steps, &mut said).unwrap();
        let said = String::from_utf8(said).unwrap();
        assert_eq!(*asked.borrow(), ["openrouter.ai:443"]);
        assert_eq!(restarted.get(), 1);
        let settings = crate::doctor::parse_settings(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(
            settings.get("HATCHLING_SCHEDULER_MODEL_NAME").map(String::as_str),
            Some(OPENROUTER_MODEL)
        );
        assert_eq!(
            settings.get("HATCHLING_EGRESS_GRANTS").map(String::as_str),
            Some("localhost:11434,openrouter.ai:443")
        );
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# Hatchling\n"));
        assert_eq!(backups(&path), 1);
        assert!(!dir.join("schedules.owner").join(crate::model_choice::MODEL_FILE).exists());
        assert!(said.starts_with("The key for openrouter.ai:443:\nHatchling uses nvidia/nemotron-3-ultra-550b-a55b:free at https://openrouter.ai/api/v1, with a 1000000-token window, thinking off."), "{said}");
        assert!(said.contains("it's forgotten"), "{said}");
        assert!(said.ends_with("Restarted the broker. Send a message in the app to check it.\n"), "{said}");
        // Not restarted when asked not to, and said so.
        let mut said = Vec::new();
        let unrestarted = target(USE_HOSTED, &["openrouter", "--no-restart"]).unwrap();
        switch_with(&unrestarted, &path, &steps, &mut said).unwrap();
        assert_eq!(restarted.get(), 1);
        assert!(String::from_utf8(said).unwrap().ends_with("`sudo systemctl restart hatchling`.\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_key_not_given_or_a_model_not_there_changes_nothing() {
        let (dir, path) = service("refused");
        let before = std::fs::read_to_string(&path).unwrap();
        let steps = Steps {
            owner: hatchling_sys::euid(),
            ask_key: &|_| Err(String::from("the key wasn't stored, so nothing was changed")),
            restart: &|| Err(String::from("restarted")),
        };
        let openrouter = target(USE_HOSTED, &["openrouter"]).unwrap();
        let refused = switch_with(&openrouter, &path, &steps, &mut Vec::new()).unwrap_err();
        assert!(refused.contains("nothing was changed"), "{refused}");
        // A local server without the model, or with none answering.
        let (address, served) = crate::model_window::answering(vec![String::from(
            "HTTP/1.0 200 OK\r\n\r\n{\"data\":[{\"id\":\"qwen3.5:9b\"}]}",
        )]);
        let mut missing = target(USE_LOCAL, &["--model", "gemma4:12b"]).unwrap();
        missing.endpoint = format!("http://{address}/v1");
        let refused = switch_with(&missing, &path, &steps, &mut Vec::new()).unwrap_err();
        served.join().unwrap();
        assert!(refused.contains("ollama pull gemma4:12b"), "{refused}");
        let refusing = crate::test_port::RefusingPort::new();
        missing.endpoint = format!("http://{}/v1", refusing.addr());
        let refused = switch_with(&missing, &path, &steps, &mut Vec::new()).unwrap_err();
        assert!(refused.starts_with("nothing at "), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(backups(&path), 0);
        assert!(dir.join("schedules.owner").join(crate::model_choice::MODEL_FILE).exists());
        // A file someone else owns isn't read; a restart that fails says so.
        let other = Steps {
            owner: hatchling_sys::euid().wrapping_add(1),
            ..steps
        };
        assert!(switch_with(&openrouter, &path, &other, &mut Vec::new()).is_err());
        let ok_key = Steps {
            ask_key: &|_| Ok(()),
            ..steps
        };
        assert_eq!(
            switch_with(&openrouter, &path, &ok_key, &mut Vec::new()).unwrap_err(),
            "restarted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_local_model_on_its_server_is_switched_to() {
        let (dir, path) = service("local");
        let (address, served) = crate::model_window::answering(vec![String::from(
            "HTTP/1.0 200 OK\r\n\r\n{\"data\":[{\"id\":\"qwen3.5:9b\"},{\"id\":\"llama3.2:latest\"}]}",
        )]);
        let steps = Steps {
            owner: hatchling_sys::euid(),
            ask_key: &|_| Err(String::from("a local model needs no key")),
            restart: &|| Ok(()),
        };
        let mut local = target(USE_LOCAL, &["--model", "llama3.2"]).unwrap();
        local.endpoint = format!("http://{address}/v1");
        switch_with(&local, &path, &steps, &mut Vec::new()).unwrap();
        served.join().unwrap();
        let settings = crate::doctor::parse_settings(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(settings.get("HATCHLING_SCHEDULER_MODEL_NAME").map(String::as_str), Some("llama3.2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_and_the_restart_are_the_programs_answers() {
        let yes = Path::new("/bin/true");
        let no = Path::new("/bin/false");
        assert_eq!(ask_key(yes, "openrouter.ai:443"), Ok(()));
        assert!(ask_key(no, "openrouter.ai:443").unwrap_err().contains("nothing was changed"));
        assert!(ask_key(Path::new("/nonexistent/hatchling"), "x:1").unwrap_err().starts_with("can't ask"));
        assert_eq!(restart(yes), Ok(()));
        assert!(restart(no).unwrap_err().starts_with("the broker didn't restart"));
        assert!(restart(Path::new("/nonexistent/systemctl")).unwrap_err().starts_with("can't restart"));
    }

    #[test]
    fn without_root_nothing_is_read() {
        if hatchling_sys::euid() == 0 {
            return;
        }
        let openrouter = target(USE_HOSTED, &["openrouter"]).unwrap();
        let refused = switch(&openrouter, Path::new("/nonexistent/hatchling.env")).unwrap_err();
        assert!(refused.ends_with("run it with sudo"), "{refused}");
    }
}
