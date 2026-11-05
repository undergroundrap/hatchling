//! The model the owner chose in the app, used over the one the operator
//! set: what every run from then on calls.
//!
//! The operator sets the model's server and a model
//! (`HATCHLING_SCHEDULER_MODEL_ENDPOINT`, `HATCHLING_SCHEDULER_MODEL_NAME`).
//! The owner may choose another model on the same server, from the app,
//! with no file to edit and no restart. Only the model's name changes:
//! the server, which is where the owner's words go, stays the operator's
//! (its grant, its allowlist, its credential).
//!
//! The choice is kept in the broker-owned owner directory
//! ([`crate::config::SchedulerConfig::owner_dir`]), written whole beside
//! and put in place, and read when each run starts. A name is checked
//! before it's kept ([`valid_name`]), and, when the server lists its
//! models, must be one of them.
//!
//! **Two servers.** The operator may set up a second model's server too
//! ([`OtherModel`]): a local one and a hosted one, say. The owner may then
//! switch between the two from the app ([`switch`]), as the work needs,
//! again with no file to edit and no restart. Which one the owner uses
//! is kept beside the model's choice ([`SERVER_FILE`]), by its endpoint.
//!
//! The threat is the owner's words going somewhere the operator didn't
//! allow. So the owner chooses between the two servers the operator set
//! up, in the root-owned settings, and no other; a switch is refused
//! unless egress grants the server, for runs and for the chat both; and
//! the app asks the owner before a switch, saying where their words will
//! go. A model chosen on one server is one of its own, so a switch
//! forgets it.

use std::fs::{DirBuilder, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use hatchling_core::ui::{ModelServer, ModelState};

use crate::config::{OtherModel, SchedulerConfig};

/// The file in the owner directory that holds the choice.
pub const MODEL_FILE: &str = "model.json";

/// The file in the owner directory that says the owner switched to the
/// other server: its endpoint. None, and the operator's first one is used.
pub const SERVER_FILE: &str = "server.json";

/// The longest name kept: far past any model's id.
pub const MAX_NAME: usize = 200;

/// The largest choice file read.
const MAX_FILE_BYTES: u64 = 1024;

/// How long the app waits to connect to the model's server for its list.
const CONNECT: Duration = Duration::from_secs(1);

/// How long it waits for the list.
const ANSWER: Duration = Duration::from_secs(3);

/// One write at a time.
static WRITES: Mutex<()> = Mutex::new(());

/// The choice, as it's kept.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Kept {
    name: String,
}

/// The server the owner switched to, as it's kept.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptServer {
    endpoint: String,
}

/// Whether `name` is one a model's id can be.
///
/// Letters, digits and `. _ : / - @ +`, at most [`MAX_NAME`] long: what a
/// server lists, and nothing that could be read as anything but a name,
/// in a log or a request.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-@+".contains(&byte))
}

/// The `host:port` egress reaches `endpoint` at: an `https://` server's,
/// or an `http://` one's on this machine. None for any other.
#[must_use]
pub fn destination(endpoint: &str) -> Option<String> {
    crate::model_switch::destination_of(endpoint, endpoint.starts_with("http://")).ok()
}

/// What `file` in `owner_dir` keeps, read whole: none when it isn't
/// there, or doesn't read (logged as `what`).
fn kept<T: serde::de::DeserializeOwned>(owner_dir: &Path, file: &str, what: &str) -> Option<T> {
    let file = match std::fs::File::open(owner_dir.join(file)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::error!(error = %error, "{what} can't be read: the configured one is used");
            return None;
        }
    };
    let mut text = Vec::new();
    let read = file.take(MAX_FILE_BYTES + 1).read_to_end(&mut text);
    let whole = u64::try_from(text.len()).is_ok_and(|len| len <= MAX_FILE_BYTES);
    match serde_json::from_slice::<T>(&text) {
        Ok(kept) if read.is_ok() && whole => Some(kept),
        _ => {
            tracing::error!("{what} doesn't read: the configured one is used");
            None
        }
    }
}

/// The model the owner chose, kept in `owner_dir`: none when they
/// haven't, or it doesn't read (logged).
#[must_use]
pub fn chosen(owner_dir: &Path) -> Option<String> {
    let kept = kept::<Kept>(owner_dir, MODEL_FILE, "the chosen model")?;
    if valid_name(&kept.name) {
        Some(kept.name)
    } else {
        tracing::error!("the chosen model doesn't read: the configured one is used");
        None
    }
}

/// The other server, when the owner switched to it: the one `other` is,
/// when `owner_dir` keeps its endpoint. A kept endpoint the operator no
/// longer sets up is none: the first server is used.
#[must_use]
pub fn switched<'a>(owner_dir: &Path, other: Option<&'a OtherModel>) -> Option<&'a OtherModel> {
    let other = other?;
    let kept = kept::<KeptServer>(owner_dir, SERVER_FILE, "the server switched to")?;
    (kept.endpoint == other.endpoint).then_some(other)
}

/// The model's server runs use, and how: the operator's first one, or the
/// other, when the owner switched to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// The API's base URL.
    pub endpoint: Option<String>,
    /// The model runs call on it: the owner's choice, else the operator's.
    pub name: Option<String>,
    /// Its thinking mode.
    pub reasoning: hatchling_core::ModelReasoning,
    /// Its window in tokens, as the operator declared it.
    pub context_window: Option<u32>,
    /// Whether it's the other server: the owner switched to it.
    pub other: bool,
}

/// The server, and model, runs use: what [`switched`] and [`chosen`]
/// keep, when scheduling keeps them, over what the operator set.
#[must_use]
pub fn server(scheduler: &SchedulerConfig) -> Server {
    let owner = scheduler.enabled().then(|| scheduler.owner_dir());
    let chosen = owner.as_deref().and_then(chosen);
    match owner
        .as_deref()
        .and_then(|dir| switched(dir, scheduler.other_model.as_ref()))
    {
        Some(other) => Server {
            endpoint: Some(other.endpoint.clone()),
            name: chosen.or_else(|| Some(other.name.clone())),
            reasoning: other.reasoning,
            context_window: other.context_window,
            other: true,
        },
        None => Server {
            endpoint: scheduler.model_endpoint.clone(),
            name: chosen.or_else(|| scheduler.model_name.clone()),
            reasoning: scheduler.model_reasoning,
            context_window: scheduler.model_context_window,
            other: false,
        },
    }
}

/// The model runs call: the owner's choice, when scheduling keeps one,
/// else the operator's, on the server in use ([`server`]).
#[must_use]
pub fn in_use(scheduler: &SchedulerConfig) -> Option<String> {
    server(scheduler).name
}

/// The model the chat names when a run fails: as [`in_use`] says, from
/// the owner directory `owner_dir` (none when scheduling keeps nothing),
/// the operator's first model `configured`, and their `other`.
#[must_use]
pub fn named(
    owner_dir: Option<&Path>,
    configured: Option<&str>,
    other: Option<&OtherModel>,
) -> Option<String> {
    let chosen = owner_dir.and_then(chosen);
    match owner_dir.and_then(|dir| switched(dir, other)) {
        Some(other) => chosen.or_else(|| Some(other.name.clone())),
        None => chosen.or_else(|| configured.map(str::to_owned)),
    }
}

/// Why a choice wasn't kept.
#[derive(Debug)]
pub enum NotSet {
    /// Scheduling is off: there's no place to keep it.
    Off,
    /// The operator set up no other server to switch to.
    NoOther,
    /// Egress doesn't grant the server: runs, or the chat, couldn't
    /// reach it.
    NotGranted,
    /// The name is none a model's can be ([`valid_name`]).
    Invalid,
    /// The server lists its models, and this isn't one of them.
    NotOnServer,
    /// It couldn't be written.
    Io(std::io::Error),
}

/// Keeps `name` as the owner's choice, or forgets it with `None`.
///
/// With none, the operator's model is used again. A choice is written
/// whole beside, then put in place. `available` is what the server lists,
/// when it does: a name must be one of them, or one with `:latest` added.
///
/// # Errors
///
/// [`NotSet`] says why not.
pub fn set(
    scheduler: &SchedulerConfig,
    name: Option<&str>,
    available: Option<&[String]>,
) -> Result<(), NotSet> {
    if !scheduler.enabled() {
        return Err(NotSet::Off);
    }
    let dir = scheduler.owner_dir();
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(name) = name else {
        return match std::fs::remove_file(dir.join(MODEL_FILE)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(NotSet::Io(error)),
            _ => Ok(()),
        };
    };
    if !valid_name(name) {
        return Err(NotSet::Invalid);
    }
    if let Some(available) = available {
        let latest = format!("{name}:latest");
        if !available.iter().any(|id| id == name || *id == latest) {
            return Err(NotSet::NotOnServer);
        }
    }
    let write = || -> std::io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let next = dir.join(format!("{MODEL_FILE}.next"));
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&next)?
            .write_all(&serde_json::to_vec(&Kept {
                name: name.to_owned(),
            })?)?;
        std::fs::rename(&next, dir.join(MODEL_FILE))
    };
    write().map_err(NotSet::Io)
}

/// Switches to the operator's other server with `to_other`, or back to
/// their first, from the next run on; `granted` says whether egress
/// grants a `host:port`, for runs and for the chat both.
///
/// The model chosen on the old server is forgotten: it was one of its
/// own. Switching to the server in use changes nothing.
///
/// # Errors
///
/// [`NotSet`] says why not: there's no other server, or egress doesn't
/// grant the one switched to.
pub fn switch(
    scheduler: &SchedulerConfig,
    to_other: bool,
    granted: &dyn Fn(&str) -> bool,
) -> Result<(), NotSet> {
    if !scheduler.enabled() {
        return Err(NotSet::Off);
    }
    let Some(other) = scheduler.other_model.as_ref() else {
        return Err(NotSet::NoOther);
    };
    let endpoint = if to_other {
        Some(other.endpoint.as_str())
    } else {
        scheduler.model_endpoint.as_deref()
    };
    let reached = endpoint
        .and_then(destination)
        .is_some_and(|destination| granted(&destination));
    if !reached {
        return Err(NotSet::NotGranted);
    }
    let dir = scheduler.owner_dir();
    let _held = WRITES.lock().unwrap_or_else(PoisonError::into_inner);
    if switched(&dir, Some(other)).is_some() == to_other {
        return Ok(());
    }
    let gone = |file: &str| match std::fs::remove_file(dir.join(file)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(NotSet::Io(error)),
        _ => Ok(()),
    };
    gone(MODEL_FILE)?;
    if !to_other {
        return gone(SERVER_FILE);
    }
    let write = || -> std::io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let next = dir.join(format!("{SERVER_FILE}.next"));
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&next)?
            .write_all(&serde_json::to_vec(&KeptServer {
                endpoint: other.endpoint.clone(),
            })?)?;
        std::fs::rename(&next, dir.join(SERVER_FILE))
    };
    write().map_err(NotSet::Io)
}

/// The models the server at the plain-HTTP `endpoint` lists, by id.
///
/// The same bounded `GET <endpoint>/models` with no credential that doctor
/// makes. None for a hosted endpoint, or a server that doesn't list them.
#[must_use]
pub fn available(endpoint: &str) -> Option<Vec<String>> {
    let (authority, path) = crate::doctor::plain_http(endpoint)?;
    let models_path = format!("/{}/models", path.trim_end_matches('/')).replace("//", "/");
    let answer = crate::doctor::ask_within(authority, &models_path, CONNECT, ANSWER).ok()??;
    let mut ids = crate::doctor::model_ids(Some(&answer))?;
    ids.retain(|id| valid_name(id));
    ids.sort();
    ids.dedup();
    Some(ids)
}

/// The model as it stands, with `available`, the server's list, when it
/// has one: what the app shows the owner.
#[must_use]
pub fn state(scheduler: &SchedulerConfig, available: Option<Vec<String>>) -> ModelState {
    let server = server(scheduler);
    let loaded = server
        .endpoint
        .as_deref()
        .zip(server.name.as_deref())
        .and_then(|(endpoint, name)| crate::model_window::learned(endpoint, name));
    let first = ModelServer {
        endpoint: scheduler.model_endpoint.clone(),
        name: scheduler.model_name.clone(),
    };
    let other = scheduler.other_model.as_ref().map(|other| ModelServer {
        endpoint: Some(other.endpoint.clone()),
        name: Some(other.name.clone()),
    });
    // The one not in use is the one the owner may switch to.
    let (in_use, switch_to) = if server.other {
        (other, Some(first))
    } else {
        (Some(first), other)
    };
    let in_use = in_use.unwrap_or_default();
    ModelState {
        endpoint: in_use.endpoint,
        configured: in_use.name,
        chosen: scheduler
            .enabled()
            .then(|| chosen(&scheduler.owner_dir()))
            .flatten(),
        available,
        window: loaded
            .and_then(|loaded| loaded.window)
            .or(server.context_window),
        size: loaded.map(|loaded| loaded.size).filter(|size| *size > 0),
        on_cpu: loaded
            .filter(|loaded| loaded.size > 0)
            .map(|loaded| loaded.on_cpu()),
        other: switch_to.filter(|_| scheduler.enabled()),
        switched: server.other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler(dir: &Path) -> SchedulerConfig {
        SchedulerConfig {
            root: dir.join("schedules"),
            model_endpoint: Some(String::from("https://models.example/v1")),
            model_name: Some(String::from("qwen3.5:9b")),
            model_context_window: Some(16384),
            ..SchedulerConfig::disabled()
        }
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-model-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// The operator's other server: a local one, beside the hosted first.
    fn local() -> OtherModel {
        OtherModel {
            endpoint: String::from("http://localhost:11434/v1"),
            name: String::from("gemma4:12b"),
            reasoning: hatchling_core::ModelReasoning::ProviderDefault,
            context_window: Some(65536),
        }
    }

    /// The owner switches between the operator's two servers, and only
    /// to one egress grants; a model chosen on one stays its own.
    #[test]
    fn the_owner_switches_between_the_operators_two_servers() {
        let dir = scratch("switch");
        let mut config = scheduler(&dir);
        config.model_reasoning = hatchling_core::ModelReasoning::Disabled;
        let granted =
            |destination: &str| matches!(destination, "localhost:11434" | "models.example:443");
        assert!(matches!(
            switch(&config, true, &granted),
            Err(NotSet::NoOther)
        ));
        config.other_model = Some(local());
        let first = Server {
            endpoint: Some(String::from("https://models.example/v1")),
            name: Some(String::from("qwen3.5:9b")),
            reasoning: hatchling_core::ModelReasoning::Disabled,
            context_window: Some(16384),
            other: false,
        };
        assert_eq!(server(&config), first);
        let state_now = state(&config, None);
        assert!(!state_now.switched);
        assert_eq!(
            state_now.other,
            Some(ModelServer {
                endpoint: Some(String::from("http://localhost:11434/v1")),
                name: Some(String::from("gemma4:12b")),
            })
        );

        // A model chosen on the first server is forgotten by a switch.
        set(&config, Some("big"), None).unwrap();
        switch(&config, true, &granted).unwrap();
        let other = Server {
            endpoint: Some(String::from("http://localhost:11434/v1")),
            name: Some(String::from("gemma4:12b")),
            reasoning: hatchling_core::ModelReasoning::ProviderDefault,
            context_window: Some(65536),
            other: true,
        };
        assert_eq!(server(&config), other);
        assert_eq!(chosen(&config.owner_dir()), None);
        let state_now = state(&config, None);
        assert!(state_now.switched);
        assert_eq!(
            (
                state_now.endpoint.as_deref(),
                state_now.configured.as_deref()
            ),
            (Some("http://localhost:11434/v1"), Some("gemma4:12b"))
        );
        assert_eq!(state_now.window, Some(65536));
        assert_eq!(
            state_now.other,
            Some(ModelServer {
                endpoint: Some(String::from("https://models.example/v1")),
                name: Some(String::from("qwen3.5:9b")),
            })
        );
        assert_eq!(
            named(
                Some(&config.owner_dir()),
                Some("qwen3.5:9b"),
                Some(&local())
            )
            .as_deref(),
            Some("gemma4:12b"),
            "the chat names the model in use"
        );

        // One chosen on the other is its own; switching to it again keeps it.
        set(&config, Some("llama3.2"), None).unwrap();
        assert_eq!(in_use(&config).as_deref(), Some("llama3.2"));
        switch(&config, true, &granted).unwrap();
        assert_eq!(in_use(&config).as_deref(), Some("llama3.2"));

        // Back to the first, as the operator set it.
        switch(&config, false, &granted).unwrap();
        assert_eq!(server(&config), first);
        assert!(!config.owner_dir().join(SERVER_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A switch reaches only a server egress grants, and one the operator
    /// sets up, and only where scheduling keeps it.
    #[test]
    fn a_switch_is_to_a_granted_server_the_operator_set_up() {
        let dir = scratch("switch-granted");
        let mut config = scheduler(&dir);
        config.other_model = Some(local());
        let first = server(&config);
        assert!(!first.other);
        let granted =
            |destination: &str| matches!(destination, "localhost:11434" | "models.example:443");

        // A server egress doesn't grant, for runs or the chat, is refused,
        // and nothing changes; so is going back to one it doesn't.
        let only_first = |destination: &str| destination == "models.example:443";
        assert!(matches!(
            switch(&config, true, &only_first),
            Err(NotSet::NotGranted)
        ));
        assert_eq!(server(&config), first);
        let only_other = |destination: &str| destination == "localhost:11434";
        switch(&config, true, &only_other).unwrap();
        assert!(matches!(
            switch(&config, false, &only_other),
            Err(NotSet::NotGranted)
        ));
        assert!(server(&config).other);

        // A switch to a server the operator no longer sets up is none.
        config.other_model = Some(OtherModel {
            endpoint: String::from("http://localhost:8080/v1"),
            ..local()
        });
        assert_eq!(server(&config), first);
        assert_eq!(
            named(
                Some(&config.owner_dir()),
                Some("qwen3.5:9b"),
                config.other_model.as_ref()
            )
            .as_deref(),
            Some("qwen3.5:9b")
        );
        config.other_model = None;
        assert_eq!(server(&config), first);
        assert!(state(&config, None).other.is_none());

        // With scheduling off, there's nowhere to keep it, and nothing to
        // switch to is shown.
        let off = SchedulerConfig {
            other_model: Some(local()),
            ..SchedulerConfig::disabled()
        };
        assert!(matches!(switch(&off, true, &granted), Err(NotSet::Off)));
        assert!(state(&off, None).other.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A kept switch that doesn't read is none: the first server is used.
    #[test]
    fn a_switch_that_doesnt_read_is_none() {
        let dir = scratch("unread-switch");
        let mut config = scheduler(&dir);
        config.other_model = Some(local());
        std::fs::create_dir_all(config.owner_dir()).unwrap();
        for text in [
            "",
            "{",
            r#"{"endpoint":"http://localhost:11434/v1","more":1}"#,
            r#"{"name":"gemma4:12b"}"#,
        ] {
            std::fs::write(config.owner_dir().join(SERVER_FILE), text).unwrap();
            assert!(!server(&config).other, "{text:?}");
        }
        let long = format!(
            r#"{{"endpoint":"http://localhost:11434/v1"}}{}"#,
            " ".repeat(1100)
        );
        std::fs::write(config.owner_dir().join(SERVER_FILE), long).unwrap();
        assert!(!server(&config).other, "too long");
        std::fs::write(
            config.owner_dir().join(SERVER_FILE),
            r#"{"endpoint":"http://localhost:11434/v1"}"#,
        )
        .unwrap();
        assert!(server(&config).other);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A name is what a model's id can be, and nothing else.
    #[test]
    fn only_a_models_name_is_one() {
        for name in [
            "qwen3.5:9b",
            "nvidia/nemotron-3-ultra-550b-a55b:free",
            "llama3.2",
            "a@b+c_d",
        ] {
            assert!(valid_name(name), "{name}");
        }
        let long = "m".repeat(MAX_NAME);
        assert!(valid_name(&long));
        for name in [
            "",
            "two words",
            "line\nbreak",
            "quote\"",
            "back\\slash",
            "café",
            "a;b",
            &format!("{long}m"),
        ] {
            assert!(!valid_name(name), "{name:?}");
        }
    }

    /// The owner's choice is kept, used over the operator's, and
    /// forgotten again; with scheduling off there's no place, and the
    /// operator's is used.
    #[test]
    fn a_choice_is_kept_used_and_forgotten() {
        let dir = scratch("kept");
        let config = scheduler(&dir);
        assert_eq!(in_use(&config).as_deref(), Some("qwen3.5:9b"));
        set(&config, Some("gemma4:12b"), None).unwrap();
        assert_eq!(chosen(&config.owner_dir()).as_deref(), Some("gemma4:12b"));
        assert_eq!(in_use(&config).as_deref(), Some("gemma4:12b"));
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(config.owner_dir().join(MODEL_FILE))
                .unwrap()
                .permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
        assert!(!config.owner_dir().join("model.json.next").exists());
        set(&config, None, None).unwrap();
        assert_eq!(in_use(&config).as_deref(), Some("qwen3.5:9b"));
        set(&config, None, None).unwrap();
        let off = SchedulerConfig {
            model_name: Some(String::from("qwen3.5:9b")),
            ..SchedulerConfig::disabled()
        };
        assert!(matches!(
            set(&off, Some("gemma4:12b"), None),
            Err(NotSet::Off)
        ));
        assert_eq!(in_use(&off).as_deref(), Some("qwen3.5:9b"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A name that isn't one, or one the server doesn't list, isn't kept;
    /// a listed name is, with or without its `latest` tag.
    #[test]
    fn a_choice_must_be_a_name_the_server_has() {
        let dir = scratch("listed");
        let config = scheduler(&dir);
        let listed = vec![String::from("gemma4:12b"), String::from("llama3.2:latest")];
        assert!(matches!(
            set(&config, Some("a b"), Some(&listed)),
            Err(NotSet::Invalid)
        ));
        assert!(matches!(
            set(&config, Some("mistral"), Some(&listed)),
            Err(NotSet::NotOnServer)
        ));
        assert_eq!(chosen(&config.owner_dir()), None);
        set(&config, Some("llama3.2"), Some(&listed)).unwrap();
        assert_eq!(chosen(&config.owner_dir()).as_deref(), Some("llama3.2"));
        set(&config, Some("gemma4:12b"), Some(&listed)).unwrap();
        assert_eq!(chosen(&config.owner_dir()).as_deref(), Some("gemma4:12b"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that doesn't read, or names no model, is no choice.
    #[test]
    fn a_choice_that_doesnt_read_is_none() {
        let dir = scratch("unread");
        let owner = dir.join("owner");
        std::fs::create_dir_all(&owner).unwrap();
        for text in [
            String::new(),
            String::from("not json"),
            String::from(r#"{"name":"two words"}"#),
            String::from(r#"{"name":"m","extra":1}"#),
            format!(r#"{{"name":"{}"}}"#, "m".repeat(2000)),
        ] {
            std::fs::write(owner.join(MODEL_FILE), &text).unwrap();
            assert_eq!(chosen(&owner), None, "{text:.40}");
        }
        std::fs::write(owner.join(MODEL_FILE), r#"{"name":"m"}"#).unwrap();
        assert_eq!(chosen(&owner).as_deref(), Some("m"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The server's list is read as doctor reads it, sorted, and only
    /// names that are names; a hosted endpoint isn't asked.
    #[test]
    fn the_servers_list_is_its_model_ids() {
        let (address, served) = crate::model_window::answering(vec![String::from(
            "HTTP/1.0 200 OK\r\n\r\n{\"data\":[{\"id\":\"qwen3.5:9b\"},{\"id\":\"bad name\"},{\"id\":\"gemma4:12b\"},{\"id\":\"qwen3.5:9b\"}]}",
        )]);
        assert_eq!(
            available(&format!("http://{address}/v1")),
            Some(vec![String::from("gemma4:12b"), String::from("qwen3.5:9b")])
        );
        let heads = served.join().unwrap();
        assert!(
            heads
                .first()
                .is_some_and(|head| head.starts_with("GET /v1/models HTTP/1.0")),
            "{heads:?}"
        );
        assert_eq!(available("https://models.example/v1"), None);
        let refusing = crate::test_port::RefusingPort::new();
        assert_eq!(available(&format!("http://{}/v1", refusing.addr())), None);
    }

    /// The state says the server, both models, the list and the window:
    /// the declared one when the server says none.
    #[test]
    fn the_state_says_what_the_owner_chooses_from() {
        let dir = scratch("state");
        let config = scheduler(&dir);
        set(&config, Some("gemma4:12b"), None).unwrap();
        let state = state(&config, Some(vec![String::from("gemma4:12b")]));
        assert_eq!(
            state,
            ModelState {
                endpoint: Some(String::from("https://models.example/v1")),
                configured: Some(String::from("qwen3.5:9b")),
                chosen: Some(String::from("gemma4:12b")),
                available: Some(vec![String::from("gemma4:12b")]),
                window: Some(16384),
                size: None,
                on_cpu: None,
                other: None,
                switched: false,
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
