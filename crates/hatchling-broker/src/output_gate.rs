//! The broker's output gate: how it is configured, and the
//! built-in rules it judges by.
//!
//! The gate itself is [`hatchling_channel::OutputGate`], owned by the
//! local hub: every reply, scheduled output and artifact sent to a person
//! passes it. The broker builds it once at start ([`build`]) from the
//! operator's environment ([`RedactionConfig`]). Nothing a session sends
//! configures it, and its directory may not lie inside the broker socket
//! directory the cell mounts.
//!
//! The built-in rules ([`Rules`]) flag, in this order:
//!
//! - `secret`: a secret the broker holds, raw, hex or base64 (the artifact
//!   gate's [`SecretSet`]), or its [`normalize`]d form inside the message's,
//!   so spacing or punctuation does not hide it;
//! - `private-key`: a PEM private-key block;
//! - each operator phrase category in `<dir>/categories`, one
//!   `category: phrase` per line, by normalized containment.
//!
//! The two built-ins are on by default; `HATCHLING_REDACTION_BUILTINS`
//! names the ones to keep (`secret`, `private-key`, or `none`).
//!
//! The rules read the vault, which needs the master key, and the key has
//! one owner that only lends it. So the rules run on a worker thread in the
//! broker's own scope ([`RulesWorker::serve`]), borrowing the key, and the
//! gate holds a [`RulesHandle`] that hands it work: the key is never
//! copied or shared out.

use std::ffi::OsString;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use hatchling_artifact::SecretSet;
use hatchling_artifact::request::sha256_hex;
use hatchling_channel::gate::{DEFAULT_CLASSIFIER_TIMEOUT, FAILED_CATEGORY, MIN_BLOCK_CHARS};
use hatchling_channel::{Category, Classifier, OutputGate, Request, Verdict, normalize};
use hatchling_vault::{MAX_SECRET_BYTES, MasterKey, SealedVault, Secret, SecretName};
use zeroize::Zeroizing;

use crate::environment as env;
use crate::redactions::{FileAudit, FileOverrides, RawOutcome, Settings};
use crate::{Config, Error};

/// The built-in rules' id, as the audit log records it.
pub const RULES_ID: &str = "hatchling-rules@1";

/// The category a held secret is redacted under.
pub const SECRET_CATEGORY: &str = "secret";

/// The category a private-key block is redacted under.
pub const PRIVATE_KEY_CATEGORY: &str = "private-key";

/// The operator's phrase categories, in the redaction directory.
pub const CATEGORIES_FILE: &str = "categories";

/// Where kept raw text is sealed, in the redaction directory.
pub const RAW_DIR: &str = "raw";

/// The gate scope of the session `id`: its no-retry list and its tail.
#[must_use]
pub fn session_scope(id: &hatchling_memory::SessionId) -> String {
    format!("session:{}", id.as_str())
}

/// The gate scope scheduled output is judged in.
pub const SCHEDULER_SCOPE: &str = "broker:scheduler";

/// The longest a classifier may be given, `HATCHLING_REDACTION_TIMEOUT_MS`.
pub const MAX_CLASSIFIER_TIMEOUT: Duration = Duration::from_secs(60);

/// The output gate's settings, from the operator's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionConfig {
    /// `HATCHLING_OUTPUT_GATE`: `on` (the default) or `off`.
    pub enabled: bool,
    /// `HATCHLING_REDACTION_DIR`: the audit log, overrides and categories.
    /// Defaults to `<vault root>.redactions`.
    pub dir: PathBuf,
    /// `HATCHLING_REDACTION_KEEP_RAW`: keep each redacted message, sealed
    /// under the master key. Off by default: the log holds hashes only.
    pub keep_raw: bool,
    /// `HATCHLING_REDACTION_TIMEOUT_MS`: how long the classifier may take.
    pub timeout: Duration,
    /// `HATCHLING_REDACTION_BUILTINS`: the built-in categories kept on.
    pub builtins: Builtins,
}

/// The built-in categories the operator keeps on. Both are on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtins {
    /// `secret`: a secret the broker holds.
    pub secrets: bool,
    /// `private-key`: a PEM private-key block.
    pub private_keys: bool,
}

impl Builtins {
    /// Both built-ins: the default.
    pub const ALL: Self = Self {
        secrets: true,
        private_keys: true,
    };

    /// The names of the built-ins that are on.
    #[must_use]
    pub fn names(self) -> Vec<String> {
        [
            (self.secrets, SECRET_CATEGORY),
            (self.private_keys, PRIVATE_KEY_CATEGORY),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_owned())
        .collect()
    }
}

/// The built-ins to keep: unset or empty is both, `none` is neither, and
/// otherwise a comma-separated list of `secret` and `private-key`.
fn parse_builtins(raw: Option<&str>) -> Result<Builtins, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(Builtins::ALL);
    };
    let mut kept = Builtins {
        secrets: false,
        private_keys: false,
    };
    if raw == "none" {
        return Ok(kept);
    }
    for name in raw
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        match name {
            SECRET_CATEGORY => kept.secrets = true,
            PRIVATE_KEY_CATEGORY => kept.private_keys = true,
            other => {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_REDACTION_BUILTINS",
                    reason: format!(
                        "{other:?} is not a built-in category: name secret, private-key, or none"
                    ),
                });
            }
        }
    }
    Ok(kept)
}

impl RedactionConfig {
    /// The settings from the environment, checked against `config`.
    ///
    /// # Errors
    ///
    /// As [`RedactionConfig::parse`].
    pub fn from_env(config: &Config) -> Result<Self, Error> {
        Self::parse(config, |key| std::env::var(key).ok())
    }

    /// The settings `get` reads, checked against `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] for a switch other than `on` or
    /// `off`, a flag other than `true`, `1`, `false` or `0`, a timeout
    /// that is not a whole number of milliseconds from 1 to 60000, or a
    /// directory that is relative, overlaps the vault root, or lies inside
    /// the broker socket directory.
    pub fn parse(config: &Config, get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let enabled = match get(env::HATCHLING_OUTPUT_GATE.name)
            .as_deref()
            .map(str::trim)
        {
            None | Some("" | "on") => true,
            Some("off") => false,
            Some(other) => {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_OUTPUT_GATE",
                    reason: format!("must be on or off, not {other:?}"),
                });
            }
        };
        let keep_raw = match get(env::HATCHLING_REDACTION_KEEP_RAW.name)
            .as_deref()
            .map(str::trim)
        {
            None | Some("" | "false" | "0") => false,
            Some("true" | "1") => true,
            Some(other) => {
                return Err(Error::InvalidConfig {
                    key: "HATCHLING_REDACTION_KEEP_RAW",
                    reason: format!("must be true, 1, false or 0, not {other:?}"),
                });
            }
        };
        let timeout = parse_timeout(get(env::HATCHLING_REDACTION_TIMEOUT_MS.name).as_deref())?;
        let dir = parse_dir(config, get(env::HATCHLING_REDACTION_DIR.name).as_deref())?;
        let builtins = parse_builtins(get(env::HATCHLING_REDACTION_BUILTINS.name).as_deref())?;
        Ok(Self {
            enabled,
            dir,
            keep_raw,
            timeout,
            builtins,
        })
    }
}

/// The classifier timeout: whole milliseconds, 1 to 60000.
fn parse_timeout(raw: Option<&str>) -> Result<Duration, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(DEFAULT_CLASSIFIER_TIMEOUT);
    };
    raw.parse::<u64>()
        .ok()
        .map(Duration::from_millis)
        .filter(|timeout| !timeout.is_zero() && *timeout <= MAX_CLASSIFIER_TIMEOUT)
        .ok_or_else(|| Error::InvalidConfig {
            key: "HATCHLING_REDACTION_TIMEOUT_MS",
            reason: format!("expected whole milliseconds from 1 to 60000, got {raw:?}"),
        })
}

/// The redaction directory: absolute, apart from the vault root, and out
/// of the broker socket directory the cell mounts.
fn parse_dir(config: &Config, raw: Option<&str>) -> Result<PathBuf, Error> {
    let dir = raw
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .map_or_else(
            || {
                let mut sibling = OsString::from(config.vault.root.as_os_str());
                sibling.push(".redactions");
                PathBuf::from(sibling)
            },
            PathBuf::from,
        );
    let invalid = |reason: String| Error::InvalidConfig {
        key: "HATCHLING_REDACTION_DIR",
        reason,
    };
    if !dir.is_absolute() {
        return Err(invalid(format!(
            "must be an absolute path: {}",
            dir.display()
        )));
    }
    let vault = &config.vault.root;
    if dir.starts_with(vault) || vault.starts_with(&dir) {
        return Err(invalid(format!(
            "{} overlaps the vault root {}",
            dir.display(),
            vault.display()
        )));
    }
    if config
        .socket
        .as_path()
        .parent()
        .is_some_and(|cell_visible| dir.starts_with(cell_visible))
    {
        return Err(invalid(format!(
            "{} is inside the broker socket directory, which the cell mounts",
            dir.display()
        )));
    }
    Ok(dir)
}

/// One operator phrase category: the category, and the phrase's
/// normalized form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phrase {
    /// What it redacts under.
    pub category: Category,
    /// The phrase, [`normalize`]d.
    pub form: String,
}

/// The operator's phrase categories in `dir`, and the file's hash: none,
/// and no hash, when there is no file.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] when the file cannot be read, or a
/// line is not `category: phrase` with a valid category that is not a
/// built-in one and a phrase with letters or digits in it.
pub fn load_phrases(dir: &Path) -> Result<(Vec<Phrase>, Option<String>), Error> {
    let path = dir.join(CATEGORIES_FILE);
    let invalid = |reason: String| Error::InvalidConfig {
        key: "HATCHLING_REDACTION_DIR",
        reason: format!("{}: {reason}", path.display()),
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), None));
        }
        Err(error) => return Err(invalid(error.to_string())),
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid(String::from("is not UTF-8")))?;
    let mut phrases = Vec::new();
    for (number, line) in (1..).zip(text.lines()) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (name, phrase) = line
            .split_once(':')
            .ok_or_else(|| invalid(format!("line {number} is not `category: phrase`")))?;
        let name = name.trim();
        let category = Category::new(name)
            .filter(|_| ![SECRET_CATEGORY, PRIVATE_KEY_CATEGORY, FAILED_CATEGORY].contains(&name))
            .ok_or_else(|| {
                invalid(format!(
                    "line {number}: {name:?} is not a category of its own \
                     (lower-case letters, digits and -, not a built-in one)"
                ))
            })?;
        let form = normalize(phrase);
        if form.is_empty() {
            return Err(invalid(format!(
                "line {number}: the phrase has no letters or digits"
            )));
        }
        phrases.push(Phrase { category, form });
    }
    Ok((phrases, Some(sha256_hex(&bytes))))
}

/// The built-in rules, judging with the secrets the broker holds.
pub struct Rules<'k> {
    config: &'k Config,
    key: &'k MasterKey,
    builtins: Builtins,
    phrases: Vec<Phrase>,
}

impl std::fmt::Debug for Rules<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rules")
            .field("builtins", &self.builtins)
            .field("phrases", &self.phrases.len())
            .finish_non_exhaustive()
    }
}

impl<'k> Rules<'k> {
    /// The rules for `config`'s secrets, read under `key`, with the
    /// `builtins` that are on, and `phrases`.
    #[must_use]
    pub const fn new(
        config: &'k Config,
        key: &'k MasterKey,
        builtins: Builtins,
        phrases: Vec<Phrase>,
    ) -> Self {
        Self {
            config,
            key,
            builtins,
            phrases,
        }
    }
}

impl Classifier for Rules<'_> {
    fn id(&self) -> &'static str {
        RULES_ID
    }

    fn classify(&self, request: &Request<'_>) -> Result<Verdict, String> {
        let redact = |name: &str, flagged: Option<String>| {
            Category::new(name)
                .map(|category| Verdict::Redact { category, flagged })
                .ok_or_else(|| format!("{name:?} is not a category"))
        };
        if self.builtins.secrets {
            let secrets =
                crate::artifact_ops::held_secret_values(self.config, self.key, "output-gate")?;
            if holds_secret(request.text, &secrets) {
                // The secret is not handed back as the flagged form: no-retry
                // keeps the whole message's form, and the rule finds the
                // secret again in any message that carries it.
                return redact(SECRET_CATEGORY, None);
            }
        }
        if self.builtins.private_keys {
            if let Some(block) = private_key_block(request.text) {
                return redact(PRIVATE_KEY_CATEGORY, Some(block.to_owned()));
            }
        }
        let form = normalize(request.text);
        Ok(self
            .phrases
            .iter()
            .find(|phrase| form.contains(phrase.form.as_str()))
            .map_or(Verdict::Allow, |phrase| Verdict::Redact {
                category: phrase.category.clone(),
                flagged: Some(phrase.form.clone()),
            }))
    }
}

/// Whether `text` carries one of `secrets`: in any form the artifact gate
/// checks, or, for a secret whose normalized form is long enough to be
/// told apart from ordinary words, spaced or punctuated differently.
fn holds_secret(text: &str, secrets: &[Zeroizing<Vec<u8>>]) -> bool {
    if SecretSet::new(secrets.iter().cloned()).exposed_in(text.as_bytes()) {
        return true;
    }
    let form = normalize(text);
    secrets
        .iter()
        .filter_map(|secret| std::str::from_utf8(secret).ok())
        .map(normalized_secret)
        .any(|secret| secret.chars().count() >= MIN_BLOCK_CHARS && form.contains(secret.as_str()))
}

/// [`normalize`] of a held secret, wiped when dropped. It is made at its
/// final size, so no copy is left behind in a freed buffer: lowercasing a
/// character never more than doubles its bytes, and three times leaves
/// room to spare.
fn normalized_secret(secret: &str) -> Zeroizing<String> {
    let mut form = Zeroizing::new(String::with_capacity(secret.len() * 3));
    form.extend(
        secret
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase),
    );
    form
}

/// The first PEM private-key block in `text`: from its `-----BEGIN...
/// PRIVATE KEY...-----` line to the end of its `-----END` line, or to the
/// end of the text when it is cut short.
fn private_key_block(text: &str) -> Option<&str> {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    const DASHES: &str = "-----";
    // Bounded by the text's own BEGIN markers: each is looked at once.
    text.match_indices(BEGIN).find_map(|(start, _)| {
        let label_start = start + BEGIN.len();
        let rest = text.get(label_start..)?;
        let label = rest.get(..rest.find(DASHES)?)?;
        if !label.contains("PRIVATE KEY") || label.contains('\n') {
            return None;
        }
        let label_end = label_start + label.len();
        let end = text
            .get(label_end..)?
            .find(END)
            .and_then(|at| {
                let after = label_end + at + END.len();
                let close = text.get(after..)?.find(DASHES)?;
                Some(after + close + DASHES.len())
            })
            .unwrap_or(text.len());
        text.get(start..end)
    })
}

/// One piece of work the rules worker owes the gate.
enum Job {
    /// Judge a message.
    Judge {
        text: String,
        scope: String,
        turn: u64,
        reply: Sender<Result<Verdict, String>>,
    },
    /// Seal a redacted message, for the operator who opted in.
    Keep {
        event_id: String,
        text: String,
        reply: Sender<RawOutcome>,
    },
}

/// The gate's side of the rules worker: it queues each judgement and
/// waits at most the gate's timeout for the answer.
#[derive(Debug, Clone)]
pub struct RulesHandle {
    jobs: Sender<Job>,
    timeout: Duration,
}

/// The worker's side: the queue it serves until every handle is gone.
#[derive(Debug)]
pub struct RulesWorker {
    jobs: Receiver<Job>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the text: it may be what is being redacted.
        f.write_str(match self {
            Self::Judge { .. } => "Job::Judge",
            Self::Keep { .. } => "Job::Keep",
        })
    }
}

/// A connected handle and worker; the handle waits at most `timeout`.
#[must_use]
pub fn rules_channel(timeout: Duration) -> (RulesHandle, RulesWorker) {
    let (jobs, queue) = mpsc::channel();
    (RulesHandle { jobs, timeout }, RulesWorker { jobs: queue })
}

impl Classifier for RulesHandle {
    fn id(&self) -> &'static str {
        RULES_ID
    }

    fn classify(&self, request: &Request<'_>) -> Result<Verdict, String> {
        let (reply, answer) = mpsc::channel();
        self.jobs
            .send(Job::Judge {
                text: request.text.to_owned(),
                scope: request.scope.to_owned(),
                turn: request.turn,
                reply,
            })
            .map_err(|_| String::from("the rules worker has stopped"))?;
        answer
            .recv_timeout(self.timeout)
            .map_err(|error| format!("the rules worker did not answer: {error}"))?
    }
}

impl RulesHandle {
    /// Asks the worker to seal `text` as `event_id`'s raw record.
    #[must_use]
    pub fn keep(&self, event_id: &str, text: &str) -> RawOutcome {
        let (reply, answer) = mpsc::channel();
        let sent = self.jobs.send(Job::Keep {
            event_id: event_id.to_owned(),
            text: text.to_owned(),
            reply,
        });
        if sent.is_err() {
            return RawOutcome::Failed;
        }
        answer
            .recv_timeout(self.timeout)
            .unwrap_or(RawOutcome::Failed)
    }
}

impl RulesWorker {
    /// Serves the queue with `rules`, sealing kept text under `raw_dir`,
    /// until every handle is dropped. A panic in the rules answers as an
    /// error, so the gate redacts.
    pub fn serve(self, rules: &Rules<'_>, raw_dir: &Path) {
        for job in self.jobs {
            match job {
                Job::Judge {
                    text,
                    scope,
                    turn,
                    reply,
                } => {
                    let verdict = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        rules.classify(&Request {
                            text: &text,
                            scope: &scope,
                            turn,
                        })
                    }))
                    .unwrap_or_else(|_| Err(String::from("the rules panicked")));
                    let _ = reply.send(verdict);
                }
                Job::Keep {
                    event_id,
                    text,
                    reply,
                } => {
                    let _ = reply.send(seal_raw(raw_dir, rules.key, &event_id, &text));
                }
            }
        }
    }
}

/// Seals `text` as `event_id`'s raw record in the vault at `raw_dir`.
fn seal_raw(raw_dir: &Path, key: &MasterKey, event_id: &str, text: &str) -> RawOutcome {
    if text.len() > MAX_SECRET_BYTES {
        return RawOutcome::TooLarge;
    }
    let sealed = event_id
        .parse::<SecretName>()
        .map_err(|_| String::from("the event id is not a record name"))
        .and_then(|name| {
            let secret = Secret::new(text.as_bytes().to_vec()).map_err(|e| e.to_string())?;
            SealedVault::open_in(raw_dir, key, hatchling_vault::Store::REDACTIONS)
                .and_then(|vault| vault.store(&name, &secret))
                .map_err(|e| e.to_string())
        });
    match sealed {
        Ok(()) => RawOutcome::Kept,
        Err(error) => {
            tracing::error!(%error, event_id, "cannot keep a redacted message");
            RawOutcome::Failed
        }
    }
}

/// What [`build`] makes: the gate, and the worker the broker must serve
/// in its own scope when the gate is on.
#[derive(Debug)]
pub struct Built {
    /// The gate, for the hub.
    pub gate: OutputGate,
    /// The rules worker and the phrases it judges by, when the gate is on.
    pub worker: Option<(RulesWorker, Vec<Phrase>)>,
}

/// Builds the broker's output gate from `redaction`.
///
/// Its settings, and what they changed from, are recorded in the audit
/// log. Without an ingress there is no hub and no person to deliver to:
/// the gate is disabled and nothing is written.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] when the redaction directory is not an
/// owner-only directory and cannot be made one, or the categories file is
/// invalid, and [`Error::Redaction`] when the audit log cannot be written.
pub fn build(config: &Config, redaction: &RedactionConfig) -> Result<Built, Error> {
    if config.channel_ingress.is_none() {
        return Ok(Built {
            gate: OutputGate::disabled(),
            worker: None,
        });
    }
    crate::redactions::ensure_dir(&redaction.dir)?;
    let (phrases, categories_sha256) = if redaction.enabled {
        load_phrases(&redaction.dir)?
    } else {
        (Vec::new(), None)
    };
    let settings = Settings {
        gate: String::from(if redaction.enabled { "on" } else { "off" }),
        classifier: redaction.enabled.then(|| String::from(RULES_ID)),
        keep_raw: redaction.keep_raw,
        timeout_ms: u64::try_from(redaction.timeout.as_millis()).unwrap_or(u64::MAX),
        categories: phrases
            .iter()
            .map(|phrase| phrase.category.as_str().to_owned())
            .collect(),
        categories_sha256,
        builtins: if redaction.enabled {
            redaction.builtins.names()
        } else {
            Vec::new()
        },
    };
    let (handle, worker) = rules_channel(redaction.timeout);
    let audit = FileAudit::new(&redaction.dir, redaction.keep_raw.then(|| handle.clone()));
    audit.configured(settings)?;
    if !redaction.enabled {
        tracing::warn!(
            dir = %redaction.dir.display(),
            "HATCHLING_OUTPUT_GATE=off: replies reach people unjudged"
        );
        return Ok(Built {
            gate: OutputGate::disabled(),
            worker: None,
        });
    }
    tracing::info!(
        dir = %redaction.dir.display(),
        categories = phrases.len(),
        keep_raw = redaction.keep_raw,
        "output gate on"
    );
    Ok(Built {
        gate: OutputGate::new(
            Arc::new(handle),
            Arc::new(audit),
            Arc::new(FileOverrides::new(&redaction.dir)),
            redaction.timeout,
        ),
        worker: Some((worker, phrases)),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fmt::Write as _;

    use base64::Engine as _;
    use hatchling_channel::Message;
    use hatchling_core::VaultAction;

    use super::*;
    use crate::VaultGrant;
    use crate::api_auth::tests::{Scratch, owner_config, scratch};
    use crate::redactions::{AUDIT_FILE, Record, read_log};

    /// The secret the broker holds in [`held`].
    const HELD: &str = "nightjar-7f3a91c2";

    fn parse(config: &Config, vars: &[(&str, &str)]) -> Result<RedactionConfig, Error> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        RedactionConfig::parse(config, |key| vars.get(key).cloned())
    }

    /// A deployment whose vault holds [`HELD`], and two short secrets.
    fn held(name: &str) -> (Scratch, Config, MasterKey) {
        let dir = scratch(name);
        let mut config = owner_config(None);
        config.vault.root = dir.0.join("vault");
        let key = MasterKey::generate().unwrap();
        let vault = SealedVault::open(&config.vault.root, &key).unwrap();
        for (name, value) in [
            ("app-token", HELD),
            ("six", "ab-cd-ef"),
            ("eight", "ab-cd-ef-gh"),
        ] {
            vault
                .store(
                    &name.parse().unwrap(),
                    &Secret::new(value.as_bytes().to_vec()).unwrap(),
                )
                .unwrap();
            config.vault.grants.push(VaultGrant {
                action: VaultAction::Fetch,
                name: name.parse().unwrap(),
            });
        }
        (dir, config, key)
    }

    fn redact(name: &str, flagged: Option<&str>) -> Verdict {
        Verdict::Redact {
            category: Category::new(name).unwrap(),
            flagged: flagged.map(str::to_owned),
        }
    }

    #[test]
    fn the_gate_is_on_by_default_and_keeps_its_records_beside_the_vault() {
        let config = owner_config(None);
        let beside = PathBuf::from("/nonexistent/hatchling-api-vault.redactions");
        assert_eq!(
            RedactionConfig::from_env(&config).unwrap().dir,
            beside,
            "the test environment sets none of the gate's variables"
        );
        assert_eq!(
            parse(&config, &[]).unwrap(),
            RedactionConfig {
                enabled: true,
                dir: beside.clone(),
                keep_raw: false,
                timeout: DEFAULT_CLASSIFIER_TIMEOUT,
                builtins: Builtins::ALL,
            }
        );
        let set = [
            ("HATCHLING_OUTPUT_GATE", "off"),
            ("HATCHLING_REDACTION_KEEP_RAW", "true"),
            ("HATCHLING_REDACTION_TIMEOUT_MS", "250"),
            ("HATCHLING_REDACTION_DIR", " /srv/redactions "),
        ];
        assert_eq!(
            parse(&config, &set).unwrap(),
            RedactionConfig {
                enabled: false,
                dir: PathBuf::from("/srv/redactions"),
                keep_raw: true,
                timeout: Duration::from_millis(250),
                builtins: Builtins::ALL,
            }
        );
        for (key, value, on) in [
            ("HATCHLING_OUTPUT_GATE", " on ", true),
            ("HATCHLING_OUTPUT_GATE", "", true),
            ("HATCHLING_REDACTION_KEEP_RAW", "1", true),
            ("HATCHLING_REDACTION_KEEP_RAW", "0", false),
            ("HATCHLING_REDACTION_KEEP_RAW", "false", false),
            ("HATCHLING_REDACTION_KEEP_RAW", "", false),
        ] {
            let got = parse(&config, &[(key, value)]).unwrap();
            let flag = if key == "HATCHLING_OUTPUT_GATE" {
                got.enabled
            } else {
                got.keep_raw
            };
            assert_eq!(flag, on, "{key}={value:?}");
        }
        assert_eq!(parse_timeout(None).unwrap(), DEFAULT_CLASSIFIER_TIMEOUT);
        assert_eq!(
            parse_timeout(Some(" ")).unwrap(),
            DEFAULT_CLASSIFIER_TIMEOUT
        );
        assert_eq!(parse_timeout(Some("1")).unwrap(), Duration::from_millis(1));
        assert_eq!(
            parse_timeout(Some("60000")).unwrap(),
            MAX_CLASSIFIER_TIMEOUT
        );
        assert_eq!(MAX_CLASSIFIER_TIMEOUT, Duration::from_secs(60));
        assert_eq!(parse_dir(&config, Some("")).unwrap(), beside);
    }

    #[test]
    fn the_operator_can_turn_each_built_in_off() {
        let config = owner_config(None);
        let builtins = |raw: &str| parse(&config, &[("HATCHLING_REDACTION_BUILTINS", raw)]);
        let only = |secrets, private_keys| Builtins {
            secrets,
            private_keys,
        };
        assert_eq!(parse(&config, &[]).unwrap().builtins, Builtins::ALL);
        assert_eq!(builtins(" ").unwrap().builtins, Builtins::ALL);
        assert_eq!(builtins("none").unwrap().builtins, only(false, false));
        assert_eq!(builtins("secret").unwrap().builtins, only(true, false));
        assert_eq!(
            builtins(" private-key, ").unwrap().builtins,
            only(false, true)
        );
        assert_eq!(
            builtins("private-key,secret").unwrap().builtins,
            Builtins::ALL
        );
        let refused = builtins("secret,codename");
        assert!(
            matches!(&refused, Err(Error::InvalidConfig { key: "HATCHLING_REDACTION_BUILTINS", reason })
                if reason.contains("\"codename\" is not a built-in category")),
            "{refused:?}"
        );
        assert_eq!(
            Builtins::ALL.names(),
            [SECRET_CATEGORY, PRIVATE_KEY_CATEGORY]
        );
        assert_eq!(only(false, true).names(), [PRIVATE_KEY_CATEGORY]);
        assert!(only(false, false).names().is_empty());
        assert_eq!(parse_builtins(None).unwrap(), Builtins::ALL);

        // Off, a built-in judges nothing; the other still does.
        let (_dir, config, key) = held("gate-builtins");
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----";
        let judge = |builtins: Builtins, text: &str| {
            Rules::new(&config, &key, builtins, Vec::new())
                .classify(&Request {
                    text,
                    scope: "s",
                    turn: 0,
                })
                .unwrap()
        };
        assert_eq!(judge(only(false, true), HELD), Verdict::Allow);
        assert_eq!(
            judge(only(false, true), pem),
            redact(PRIVATE_KEY_CATEGORY, Some(pem))
        );
        assert_eq!(judge(only(true, false), pem), Verdict::Allow);
        assert_eq!(
            judge(only(true, false), HELD),
            redact(SECRET_CATEGORY, None)
        );
        // With secrets off the vault is not even opened.
        let unopened = owner_config(None);
        let request = Request {
            text: "x",
            scope: "s",
            turn: 0,
        };
        let blind = Rules::new(&unopened, &key, only(false, true), Vec::new());
        assert_eq!(blind.classify(&request).unwrap(), Verdict::Allow);
        let shown = format!("{blind:?}");
        assert!(shown.contains("secrets: false"), "{shown}");
    }

    #[test]
    fn a_bad_gate_setting_stops_the_broker() {
        let config = owner_config(None);
        for (key, value, needle) in [
            ("HATCHLING_OUTPUT_GATE", "maybe", "on or off"),
            ("HATCHLING_REDACTION_KEEP_RAW", "yes", "true, 1, false or 0"),
            ("HATCHLING_REDACTION_TIMEOUT_MS", "0", "from 1 to 60000"),
            ("HATCHLING_REDACTION_TIMEOUT_MS", "60001", "from 1 to 60000"),
            ("HATCHLING_REDACTION_TIMEOUT_MS", "2s", "from 1 to 60000"),
            ("HATCHLING_REDACTION_DIR", "relative/dir", "absolute path"),
            (
                "HATCHLING_REDACTION_DIR",
                "/nonexistent/hatchling-api-vault/in",
                "overlaps the vault root",
            ),
            (
                "HATCHLING_REDACTION_DIR",
                "/nonexistent",
                "overlaps the vault root",
            ),
            (
                "HATCHLING_REDACTION_DIR",
                "/tmp/hatchling-test-api-dummy/red",
                "socket directory",
            ),
        ] {
            let got = parse(&config, &[(key, value)]);
            assert!(
                matches!(&got, Err(Error::InvalidConfig { key: k, reason }) if *k == key && reason.contains(needle)),
                "{key}={value:?}: {got:?}"
            );
        }
        // Next to the vault is not inside it.
        assert!(parse_dir(&config, Some("/nonexistent/hatchling-api-vault-red")).is_ok());
    }

    #[test]
    fn phrase_categories_come_from_the_operators_file() {
        let dir = scratch("gate-phrases");
        assert_eq!(load_phrases(&dir.0).unwrap(), (Vec::new(), None));
        let text = "# phrases\n\ncodename: Project Nightjar\n  launch-2: the launch is on  \n";
        std::fs::write(dir.0.join(CATEGORIES_FILE), text).unwrap();
        let (phrases, hash) = load_phrases(&dir.0).unwrap();
        assert_eq!(
            phrases,
            [
                Phrase {
                    category: Category::new("codename").unwrap(),
                    form: String::from("projectnightjar"),
                },
                Phrase {
                    category: Category::new("launch-2").unwrap(),
                    form: String::from("thelaunchison"),
                },
            ]
        );
        assert_eq!(hash, Some(sha256_hex(text.as_bytes())));
        for (bad, needle) in [
            ("no colon\n", "line 1 is not `category: phrase`"),
            (
                "# c\nCodename: x\n",
                "line 2: \"Codename\" is not a category of its own",
            ),
            ("secret: x\n", "\"secret\" is not a category of its own"),
            ("private-key: x\n", "\"private-key\" is not"),
            ("classifier-failure: x\n", "\"classifier-failure\" is not"),
            ("ok: !!!\n", "line 1: the phrase has no letters or digits"),
        ] {
            std::fs::write(dir.0.join(CATEGORIES_FILE), bad).unwrap();
            let got = load_phrases(&dir.0);
            assert!(
                matches!(&got, Err(Error::InvalidConfig { key: "HATCHLING_REDACTION_DIR", reason }) if reason.contains(needle)),
                "{bad:?}: {got:?}"
            );
        }
        std::fs::write(dir.0.join(CATEGORIES_FILE), [0xff, 0xfe]).unwrap();
        let got = load_phrases(&dir.0);
        assert!(
            matches!(&got, Err(Error::InvalidConfig { reason, .. }) if reason.contains("is not UTF-8")),
            "{got:?}"
        );
        std::fs::remove_file(dir.0.join(CATEGORIES_FILE)).unwrap();
        std::fs::create_dir(dir.0.join(CATEGORIES_FILE)).unwrap();
        assert!(
            load_phrases(&dir.0).is_err(),
            "an unreadable file is refused"
        );
    }

    #[test]
    fn the_rules_flag_held_secrets_then_private_keys_then_phrases() {
        let (_dir, config, key) = held("gate-rules");
        let phrases = vec![Phrase {
            category: Category::new("codename").unwrap(),
            form: normalize("Project Nightjar"),
        }];
        let rules = Rules::new(&config, &key, Builtins::ALL, phrases);
        assert_eq!(rules.id(), RULES_ID);
        let shown = format!("{rules:?}");
        assert!(shown.contains("phrases: 1"), "{shown}");
        let judge = |text: &str| {
            rules
                .classify(&Request {
                    text,
                    scope: "s",
                    turn: 0,
                })
                .unwrap()
        };
        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----";
        let hex = HELD.bytes().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        });
        let base64 = base64::engine::general_purpose::STANDARD.encode(HELD);
        for (text, verdict) in [
            (String::from("all clear"), Verdict::Allow),
            (format!("token {HELD} here"), redact(SECRET_CATEGORY, None)),
            (
                String::from("NIGHTJAR 7F3A 91C2"),
                redact(SECRET_CATEGORY, None),
            ),
            (hex, redact(SECRET_CATEGORY, None)),
            (base64, redact(SECRET_CATEGORY, None)),
            (format!("{pem} with {HELD}"), redact(SECRET_CATEGORY, None)),
            (
                format!("key: {pem} done"),
                redact(PRIVATE_KEY_CATEGORY, Some(pem)),
            ),
            (
                format!("project nightjar {pem}"),
                redact(PRIVATE_KEY_CATEGORY, Some(pem)),
            ),
            (
                String::from("the Project-Nightjar plan"),
                redact("codename", Some("projectnightjar")),
            ),
            // A secret too short to tell from words is matched exactly only.
            (String::from("AB CD EF"), Verdict::Allow),
            (String::from("ab-cd-ef"), redact(SECRET_CATEGORY, None)),
            (String::from("ABCDEFGH"), redact(SECRET_CATEGORY, None)),
        ] {
            assert_eq!(judge(&text), verdict, "{text:?}");
        }
        // No vault, no judgement: the gate redacts on the error.
        let unopened = owner_config(None);
        let blind = Rules::new(&unopened, &key, Builtins::ALL, Vec::new());
        let request = Request {
            text: "x",
            scope: "s",
            turn: 0,
        };
        assert!(blind.classify(&request).is_err());
    }

    #[test]
    fn the_rules_look_for_every_secret_the_artifact_gate_does() {
        let (_dir, config, key) = held("gate-held");
        let values = crate::artifact_ops::held_secret_values(&config, &key, "output-gate").unwrap();
        assert_eq!(values.len(), 3);
        assert!(values.contains(&Zeroizing::new(HELD.as_bytes().to_vec())));
        let set = crate::artifact_ops::held_secrets(&config, &key).unwrap();
        assert!(set.exposed_in(HELD.as_bytes()));
        assert!(!set.exposed_in(b"nothing held"));
        assert!(
            crate::artifact_ops::held_secret_values(&owner_config(None), &key, "output-gate")
                .is_err()
        );
    }

    #[test]
    fn a_secret_is_found_raw_encoded_or_respaced() {
        let secrets = [
            HELD.as_bytes().to_vec(),
            b"ab-cd-ef".to_vec(),
            vec![0xff, 0x00, 0x41],
        ]
        .map(Zeroizing::new);
        assert!(holds_secret(HELD, &secrets));
        assert!(holds_secret("nightjar 7f3a 91c2", &secrets));
        assert!(!holds_secret("nightjar 7f3a", &secrets));
        assert!(
            !holds_secret("AB CD EF", &secrets),
            "six letters and digits are too few"
        );
        assert!(holds_secret("ab-cd-ef", &secrets));
        assert!(!holds_secret("plain text", &[]));
    }

    #[test]
    fn a_normalized_secret_is_made_at_its_final_size() {
        // Lowercasing grows these: İ to two characters, Ⱥ to a longer one.
        for secret in ["Nightjar-7F3A 91c2", "İİİİ", "ȺȺȺ", "", "--"] {
            let form = normalized_secret(secret);
            assert_eq!(*form, normalize(secret), "{secret:?}");
            assert_eq!(form.capacity(), secret.len() * 3, "{secret:?} grew");
        }
    }

    #[test]
    fn a_private_key_block_is_found_whole_or_cut_short() {
        let block = "-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----";
        let framed = format!("a {block} b");
        assert_eq!(private_key_block(&framed), Some(block));
        let cut = "-----BEGIN EC PRIVATE KEY-----\nMHcC";
        let late = format!("x {cut}");
        assert_eq!(private_key_block(&late), Some(cut));
        let open_end = "-----BEGIN PRIVATE KEY-----\nAA\n-----END PRIVATE KEY";
        assert_eq!(private_key_block(open_end), Some(open_end));
        let certificate = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----";
        assert_eq!(private_key_block(certificate), None);
        let both = format!("{certificate}\n{block}");
        assert_eq!(
            private_key_block(&both),
            Some(block),
            "a certificate before the key is passed over"
        );
        assert_eq!(
            private_key_block("-----BEGIN \nPRIVATE KEY-----"),
            None,
            "one line"
        );
        assert_eq!(
            private_key_block("-----BEGIN RSA PRIVATE KEY"),
            None,
            "no header end"
        );
        assert_eq!(private_key_block("no key at all"), None);
    }

    #[test]
    fn the_worker_judges_and_keeps_for_the_gate_until_its_handles_go() {
        let (dir, config, key) = held("gate-worker");
        let rules = Rules::new(&config, &key, Builtins::ALL, Vec::new());
        let (handle, worker) = rules_channel(Duration::from_secs(5));
        assert_eq!(handle.id(), RULES_ID);
        let raw = dir.0.join(RAW_DIR);
        let request = |text| Request {
            text,
            scope: "s",
            turn: 3,
        };
        std::thread::scope(|scope| {
            let served = scope.spawn(|| worker.serve(&rules, &raw));
            assert_eq!(handle.classify(&request("fine")).unwrap(), Verdict::Allow);
            assert_eq!(
                handle.classify(&request(HELD)).unwrap(),
                redact(SECRET_CATEGORY, None)
            );
            assert_eq!(handle.keep("red-1-2-3", "the text"), RawOutcome::Kept);
            assert_eq!(
                handle.keep("red 1", "x"),
                RawOutcome::Failed,
                "not a record name"
            );
            let most = "x".repeat(MAX_SECRET_BYTES);
            assert_eq!(handle.keep("red-1-2-4", &most), RawOutcome::Kept);
            let over = format!("{most}x");
            assert_eq!(handle.keep("red-1-2-5", &over), RawOutcome::TooLarge);
            drop(handle);
            served.join().unwrap();
        });
        let vault = SealedVault::open_in(&raw, &key, hatchling_vault::Store::REDACTIONS).unwrap();
        let kept = vault.fetch(&"red-1-2-3".parse().unwrap()).unwrap();
        assert_eq!(kept.as_bytes(), b"the text");
        assert!(!vault.exists(&"red-1-2-5".parse().unwrap()));
        assert_eq!(seal_raw(&raw, &key, "red-9", "t"), RawOutcome::Kept);
        std::fs::write(dir.0.join("file"), b"x").unwrap();
        assert_eq!(
            seal_raw(&dir.0.join("file"), &key, "red-9", "t"),
            RawOutcome::Failed
        );

        let (handle, worker) = rules_channel(Duration::from_millis(50));
        let late = handle.classify(&request("x")).unwrap_err();
        assert!(late.contains("did not answer"), "{late}");
        assert_eq!(handle.keep("red-1", "x"), RawOutcome::Failed);
        drop(worker);
        let gone = handle.classify(&request("x")).unwrap_err();
        assert!(gone.contains("stopped"), "{gone}");
        assert_eq!(handle.keep("red-1", "x"), RawOutcome::Failed);
        let shown = format!("{handle:?}");
        assert!(shown.contains("RulesHandle"), "{shown}");
    }

    #[test]
    fn the_gate_is_built_once_at_start_and_records_its_settings() {
        let (dir, mut config, key) = held("gate-build");
        let redaction = RedactionConfig {
            enabled: true,
            dir: dir.0.join("red"),
            keep_raw: true,
            timeout: Duration::from_secs(1),
            builtins: Builtins::ALL,
        };
        let built = build(&config, &redaction).unwrap();
        assert_eq!(built.gate.classifier_id(), None, "no ingress, no gate");
        assert!(built.worker.is_none());
        assert!(!redaction.dir.exists(), "and nothing written");

        config.channel_ingress = Some("/tmp/hatchling-gate-build.sock".parse().unwrap());
        crate::redactions::ensure_dir(&redaction.dir).unwrap();
        std::fs::write(
            redaction.dir.join(CATEGORIES_FILE),
            "codename: nightjar plan\n",
        )
        .unwrap();
        let built = build(&config, &redaction).unwrap();
        assert_eq!(built.gate.classifier_id(), Some(RULES_ID));
        let (worker, phrases) = built.worker.unwrap();
        assert_eq!(phrases.len(), 1);
        let rules = Rules::new(&config, &key, Builtins::ALL, phrases);
        let raw = redaction.dir.join(RAW_DIR);
        let message = Message {
            sender: hatchling_channel::SenderId(String::from("uid:1")),
            channel: hatchling_channel::ChannelId::local(),
            text: String::from("the Nightjar Plan, in full"),
        };
        let gate = built.gate;
        std::thread::scope(|scope| {
            scope.spawn(|| worker.serve(&rules, &raw));
            let cleared = gate.clear(&message, "session:a");
            assert!(
                cleared.redaction().is_some(),
                "the operator's phrase is redacted"
            );
            drop(gate);
        });
        let log = redaction.dir.join(AUDIT_FILE);
        let (records, unreadable) = read_log(&log).unwrap();
        assert_eq!(unreadable, 0);
        let [
            Record::Config {
                settings,
                changed_from: None,
                ..
            },
            Record::Redacted { event, raw },
        ] = records.as_slice()
        else {
            unreachable!("a config record, then the redaction: {records:?}");
        };
        assert_eq!(
            (
                settings.gate.as_str(),
                settings.classifier.as_deref(),
                settings.keep_raw
            ),
            ("on", Some(RULES_ID), true)
        );
        assert_eq!(settings.timeout_ms, 1000);
        assert_eq!(settings.categories, ["codename"]);
        assert!(settings.categories_sha256.is_some());
        assert_eq!(
            (event.category.as_str(), *raw),
            ("codename", RawOutcome::Kept)
        );

        let off = RedactionConfig {
            enabled: false,
            ..redaction
        };
        let built = build(&config, &off).unwrap();
        assert_eq!(built.gate.classifier_id(), None);
        assert!(built.worker.is_none());
        let (records, _) = read_log(&log).unwrap();
        let Some(Record::Config {
            settings,
            changed_from: Some(was),
            ..
        }) = records.last()
        else {
            unreachable!("turning the gate off is recorded with what it was: {records:?}");
        };
        assert_eq!(
            (settings.gate.as_str(), settings.classifier.as_deref()),
            ("off", None)
        );
        assert!(settings.categories.is_empty());
        assert_eq!(was.gate, "on");
    }

    #[test]
    fn a_gate_that_cannot_keep_its_records_stops_the_broker() {
        let (dir, mut config, _key) = held("gate-refused");
        config.channel_ingress = Some("/tmp/hatchling-gate-refused.sock".parse().unwrap());
        let redaction = RedactionConfig {
            enabled: true,
            dir: dir.0.join("red"),
            keep_raw: false,
            timeout: Duration::from_secs(1),
            builtins: Builtins::ALL,
        };
        std::fs::write(dir.0.join("file"), b"x").unwrap();
        let in_a_file = RedactionConfig {
            enabled: true,
            dir: dir.0.join("file"),
            keep_raw: false,
            timeout: Duration::from_secs(1),
            builtins: Builtins::ALL,
        };
        assert!(matches!(
            build(&config, &in_a_file),
            Err(Error::InvalidConfig {
                key: "HATCHLING_REDACTION_DIR",
                ..
            })
        ));
        crate::redactions::ensure_dir(&redaction.dir).unwrap();
        std::fs::write(
            redaction.dir.join(CATEGORIES_FILE),
            "bad line
",
        )
        .unwrap();
        assert!(
            build(&config, &redaction).is_err(),
            "a bad categories file stops the broker"
        );
        let off = RedactionConfig {
            enabled: false,
            ..redaction
        };
        std::fs::create_dir(off.dir.join(AUDIT_FILE)).unwrap();
        assert!(
            matches!(build(&config, &off), Err(Error::Redaction { .. })),
            "an unwritable log stops it"
        );
    }

    #[test]
    fn a_session_scope_is_named_for_its_session() {
        let id = hatchling_memory::SessionId::generate().unwrap();
        let named = format!("session:{}", id.as_str());
        assert_eq!(session_scope(&id), named);
        assert_eq!(SCHEDULER_SCOPE, "broker:scheduler");
    }
}
