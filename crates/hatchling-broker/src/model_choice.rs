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

use std::fs::{DirBuilder, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use hatchling_core::ui::ModelState;

use crate::config::SchedulerConfig;

/// The file in the owner directory that holds the choice.
pub const MODEL_FILE: &str = "model.json";

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

/// The model the owner chose, kept in `owner_dir`: none when they
/// haven't, or it doesn't read (logged).
#[must_use]
pub fn chosen(owner_dir: &Path) -> Option<String> {
    let file = match std::fs::File::open(owner_dir.join(MODEL_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::error!(error = %error, "the chosen model can't be read: the configured one is used");
            return None;
        }
    };
    let mut text = Vec::new();
    let read = file.take(MAX_FILE_BYTES + 1).read_to_end(&mut text);
    let whole = u64::try_from(text.len()).is_ok_and(|len| len <= MAX_FILE_BYTES);
    match serde_json::from_slice::<Kept>(&text) {
        Ok(kept) if read.is_ok() && whole && valid_name(&kept.name) => Some(kept.name),
        _ => {
            tracing::error!("the chosen model doesn't read: the configured one is used");
            None
        }
    }
}

/// The model runs call: the owner's choice, when scheduling keeps one,
/// else the operator's.
#[must_use]
pub fn in_use(scheduler: &SchedulerConfig) -> Option<String> {
    scheduler
        .enabled()
        .then(|| chosen(&scheduler.owner_dir()))
        .flatten()
        .or_else(|| scheduler.model_name.clone())
}

/// Why a choice wasn't kept.
#[derive(Debug)]
pub enum NotSet {
    /// Scheduling is off: there's no place to keep it.
    Off,
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
    let in_use = in_use(scheduler);
    let loaded = scheduler
        .model_endpoint
        .as_deref()
        .zip(in_use.as_deref())
        .and_then(|(endpoint, name)| crate::model_window::learned(endpoint, name));
    ModelState {
        endpoint: scheduler.model_endpoint.clone(),
        configured: scheduler.model_name.clone(),
        chosen: scheduler
            .enabled()
            .then(|| chosen(&scheduler.owner_dir()))
            .flatten(),
        available,
        window: loaded
            .and_then(|loaded| loaded.window)
            .or(scheduler.model_context_window),
        size: loaded.map(|loaded| loaded.size).filter(|size| *size > 0),
        on_cpu: loaded
            .filter(|loaded| loaded.size > 0)
            .map(|loaded| loaded.on_cpu()),
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
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
