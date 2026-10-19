//! The output gate's records: the audit log, the operator's
//! overrides, and `hatchling-broker redactions`.
//!
//! Everything lives in the owner-only redaction directory:
//!
//! - `audit.log`: one JSON [`Record`] per line, mode `0600`. A redaction is
//!   recorded by tags (HMAC-SHA-256, under the [`TagKey`], of the message
//!   and of its normalized flagged form) and size, never its text. Without
//!   the key, a tag confirms no guess at what was redacted. Each broker
//!   start records the gate's settings and, when they changed, what they
//!   were. Records from before the tags carry unkeyed SHA-256 hashes.
//! - `overrides`: one JSON [`Override`] per line, appended by
//!   `redactions override`: a verdict the operator marked wrong, for its
//!   scope or every scope. The gate reads it on every flagged message.
//! - `raw/`: when the operator opted in, each redacted message, sealed
//!   under the master key, readable with `redactions raw`; and, always, the
//!   [`TagKey`], sealed the same way.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hatchling_artifact::request::sha256_hex;
use hatchling_channel::gate::FAILED_CATEGORY;
use hatchling_channel::{Audit, Category, Event, Failure, Overrides, Reason};
use hatchling_vault::OpensStores;
use hatchling_vault::{MasterKey, SealedVault, Secret, SecretName};
use hmac::{Hmac, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::Error;
use crate::output_gate::{RAW_DIR, RedactionConfig, RulesHandle, SECRET_CATEGORY};

/// The audit log, in the redaction directory.
pub const AUDIT_FILE: &str = "audit.log";

/// The operator's overrides, in the redaction directory.
pub const OVERRIDES_FILE: &str = "overrides";

/// The scope an override for every scope names.
pub const ANY_SCOPE: &str = "*";

/// The [`TagKey`]'s name in the redaction store. No event id has it.
pub const TAG_KEY_NAME: &str = "audit-tag-key";

/// The key the audit log's tags are made under: random, sealed under the
/// master key in the redaction store, wiped when dropped. The log alone
/// confirms no guess at a redacted message, however short.
pub struct TagKey(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for TagKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TagKey(..)")
    }
}

impl TagKey {
    /// The key in the redaction store at `raw_dir`, made and sealed there
    /// under `key` the first time.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Vault`] when the store can't be opened, read or
    /// written, and [`Error::Redaction`] for a stored key of the wrong size
    /// or no randomness to make one.
    pub fn load_or_create(raw_dir: &Path, key: &dyn OpensStores) -> Result<Self, Error> {
        let vault = SealedVault::open_in(raw_dir, key, hatchling_vault::Store::REDACTIONS)
            .map_err(|source| Error::Vault { source })?;
        let name: SecretName = TAG_KEY_NAME.parse().map_err(|_| Error::Redaction {
            reason: String::from("the audit-tag key's name is not a record name"),
        })?;
        let mut bytes = Zeroizing::new([0u8; 32]);
        match vault.fetch(&name) {
            Ok(stored) => {
                if stored.as_bytes().len() != bytes.len() {
                    return Err(Error::Redaction {
                        reason: String::from("the stored audit-tag key is not 32 bytes"),
                    });
                }
                bytes.copy_from_slice(stored.as_bytes());
            }
            Err(hatchling_vault::Error::NotFound { .. }) => {
                use std::io::Read as _;
                std::fs::File::open("/dev/urandom")
                    .and_then(|mut rng| rng.read_exact(bytes.as_mut_slice()))
                    .map_err(|e| Error::Redaction {
                        reason: format!("no randomness for the audit-tag key: {e}"),
                    })?;
                let secret =
                    Secret::new(bytes.to_vec()).map_err(|source| Error::Vault { source })?;
                vault
                    .store(&name, &secret)
                    .map_err(|source| Error::Vault { source })?;
            }
            Err(source) => return Err(Error::Vault { source }),
        }
        Ok(Self(bytes))
    }

    /// The tag of `bytes`: HMAC-SHA-256 under this key, lower-case hex.
    #[must_use]
    pub fn tag(&self, bytes: &[u8]) -> String {
        // HMAC takes a key of any length; a 32-byte one is never refused.
        <Hmac<Sha256>>::new_from_slice(self.0.as_slice()).map_or_else(
            |_| String::new(),
            |mut mac| {
                mac.update(bytes);
                crate::approval::hex_token("", &mac.finalize().into_bytes())
            },
        )
    }
}

/// The gate's settings, as a config record shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// `on` or `off`.
    pub gate: String,
    /// The classifier's id, when the gate is on.
    pub classifier: Option<String>,
    /// Whether redacted text is kept, sealed.
    pub keep_raw: bool,
    /// The classifier timeout, in milliseconds.
    pub timeout_ms: u64,
    /// The operator's phrase categories, by name.
    pub categories: Vec<String>,
    /// The categories file's SHA-256, when there is one.
    pub categories_sha256: Option<String>,
    /// The built-in categories that are on.
    pub builtins: Vec<String>,
}

/// What became of a redacted message's raw text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RawOutcome {
    /// Not kept: the operator did not opt in.
    Off,
    /// Kept, sealed.
    Kept,
    /// Too large to keep.
    TooLarge,
    /// Keeping it failed.
    Failed,
}

/// One redaction or override delivery, by hashes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Judged {
    /// The event id.
    pub event_id: String,
    /// When, in Unix seconds.
    pub at: u64,
    /// The scope (session) it happened in.
    pub scope: String,
    /// The sender stamp.
    pub sender: String,
    /// The classifier's id.
    pub classifier: String,
    /// The category.
    pub category: String,
    /// Why: `flagged`, `no-retry`, or `classifier-error`, `-panic` or
    /// `-timeout`.
    pub reason: String,
    /// The message's tag, under the [`TagKey`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// The message's unkeyed SHA-256, in a record from before the tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The message's size in bytes.
    pub bytes: usize,
    /// The tag of its normalized flagged form, when there is one: what an
    /// override names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged_tag: Option<String>,
    /// The flagged form's unkeyed SHA-256, in a record from before the
    /// tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged_sha256: Option<String>,
}

impl Judged {
    /// What an override of this redaction names: its flagged form's tag,
    /// or, from before the tags, its unkeyed hash.
    fn flagged(&self) -> Option<Flagged> {
        self.flagged_tag
            .clone()
            .map(Flagged::Tag)
            .or_else(|| self.flagged_sha256.clone().map(Flagged::Sha256))
    }
}

/// How an override names a flagged form.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Flagged {
    /// By its tag.
    Tag(String),
    /// By its unkeyed SHA-256, from before the tags.
    Sha256(String),
}

/// An operator override: `category` with this flagged form is let through
/// in `scope` (or every scope, [`ANY_SCOPE`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Override {
    /// The redaction it answers.
    pub event_id: String,
    /// When, in Unix seconds.
    pub at: u64,
    /// The scope it holds in.
    pub scope: String,
    /// The category it lifts.
    pub category: String,
    /// The flagged form's tag, under the [`TagKey`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged_tag: Option<String>,
    /// The flagged form's unkeyed SHA-256, in an override from before the
    /// tags. It is still honoured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flagged_sha256: Option<String>,
    /// Who overrode it: the operator's uid.
    pub by: u32,
}

impl Override {
    /// Whether this names `flagged`, whose tag is `tag` when there is a key.
    fn names(&self, tag: Option<&str>, flagged: &str) -> bool {
        tag.is_some_and(|tag| self.flagged_tag.as_deref() == Some(tag))
            || self
                .flagged_sha256
                .as_deref()
                .is_some_and(|hash| hash == sha256_hex(flagged.as_bytes()))
    }

    /// Whether this names the same flagged form as `other`.
    fn same_form(&self, other: &Self) -> bool {
        (self.flagged_tag.is_some() && self.flagged_tag == other.flagged_tag)
            || (self.flagged_sha256.is_some() && self.flagged_sha256 == other.flagged_sha256)
    }
}

/// One line of the audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Record {
    /// The broker started with these settings.
    Config {
        /// When, in Unix seconds.
        at: u64,
        /// Who started the broker with them: the operator's uid.
        by: u32,
        /// The settings.
        settings: Settings,
        /// The previous settings, when these differ from them.
        changed_from: Option<Settings>,
    },
    /// A message was redacted.
    Redacted {
        /// What was redacted, by hashes.
        #[serde(flatten)]
        event: Judged,
        /// What became of its text.
        raw: RawOutcome,
    },
    /// A flagged message was delivered on an operator override.
    Overridden {
        /// What was delivered, by hashes.
        #[serde(flatten)]
        event: Judged,
    },
    /// The operator overrode a redaction.
    Override(Override),
    /// The operator read a kept message.
    RawRead {
        /// The event read.
        event_id: String,
        /// When, in Unix seconds.
        at: u64,
        /// Who read it: the operator's uid.
        by: u32,
    },
}

/// Unix seconds now.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A redaction reason as the log spells it.
#[must_use]
pub const fn reason_name(reason: Reason) -> &'static str {
    match reason {
        Reason::Flagged => "flagged",
        Reason::NoRetry => "no-retry",
        Reason::ClassifierFailed(Failure::Error) => "classifier-error",
        Reason::ClassifierFailed(Failure::Panic) => "classifier-panic",
        Reason::ClassifierFailed(Failure::Timeout) => "classifier-timeout",
    }
}

/// Makes `dir` an owner-only directory if it is not one: a real directory,
/// mode `0700`, owned by this user. The vault's own check, so the two
/// cannot drift.
///
/// # Errors
///
/// Returns [`Error::InvalidConfig`] when it is not one and cannot be made
/// one.
pub fn ensure_dir(dir: &Path) -> Result<(), Error> {
    hatchling_vault::Vault::open(dir)
        .map(|_| ())
        .map_err(|source| Error::InvalidConfig {
            key: "HATCHLING_REDACTION_DIR",
            reason: format!("{}: {source}", dir.display()),
        })
}

/// Appends `line` to `path`, creating it owner-only.
fn append(path: &Path, line: &str) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| writeln!(file, "{line}"))
}

/// Every record in the log at `path` that parses, and how many lines did
/// not. No log is no records.
///
/// # Errors
///
/// Returns [`Error::Redaction`] when the log exists but cannot be read.
pub fn read_log(path: &Path) -> Result<(Vec<Record>, usize), Error> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(Error::Redaction {
                reason: format!("{}: {error}", path.display()),
            });
        }
    };
    let mut records = Vec::new();
    let mut unreadable = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(record) => records.push(record),
            Err(_) => unreadable += 1,
        }
    }
    Ok((records, unreadable))
}

/// The gate's audit log: tags, sizes and categories, never text.
#[derive(Debug)]
pub struct FileAudit {
    log: PathBuf,
    keeper: Option<RulesHandle>,
    tags: Option<Arc<TagKey>>,
}

impl FileAudit {
    /// The audit log in `dir`; `keeper` seals each redacted message when
    /// the operator opted in. It records no tag until given a key
    /// ([`FileAudit::with_tags`]).
    #[must_use]
    pub fn new(dir: &Path, keeper: Option<RulesHandle>) -> Self {
        Self {
            log: dir.join(AUDIT_FILE),
            keeper,
            tags: None,
        }
    }

    /// This log, tagging what it records under `tags`.
    #[must_use]
    pub fn with_tags(self, tags: Arc<TagKey>) -> Self {
        Self {
            tags: Some(tags),
            ..self
        }
    }

    /// Records the gate's `settings` at start, with the last recorded
    /// settings when these differ from them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Redaction`] when the log cannot be read or written.
    pub fn configured(&self, settings: Settings) -> Result<(), Error> {
        let (records, _) = read_log(&self.log)?;
        let last = records.into_iter().rev().find_map(|record| match record {
            Record::Config { settings, .. } => Some(settings),
            Record::Redacted { .. }
            | Record::Overridden { .. }
            | Record::Override(_)
            | Record::RawRead { .. } => None,
        });
        let changed_from = last.filter(|last| *last != settings);
        if let Some(previous) = &changed_from {
            tracing::warn!(?previous, current = ?settings, "the output gate's settings changed");
        }
        self.write(&Record::Config {
            at: now(),
            by: hatchling_sys::euid(),
            settings,
            changed_from,
        })
    }

    fn write(&self, record: &Record) -> Result<(), Error> {
        let line = serde_json::to_string(record).map_err(|e| Error::Redaction {
            reason: format!("cannot encode an audit record: {e}"),
        })?;
        append(&self.log, &line).map_err(|e| Error::Redaction {
            reason: format!("{}: {e}", self.log.display()),
        })
    }

    /// Writes `record`; a failure is logged, since the gate has already
    /// decided.
    fn record(&self, record: &Record) {
        if let Err(error) = self.write(record) {
            tracing::error!(%error, "cannot record an output gate event");
        }
    }
}

/// `event` by its tags under `tags`, when there is a key.
fn judged(event: &Event<'_>, tags: Option<&TagKey>) -> Judged {
    Judged {
        event_id: event.event_id.to_owned(),
        at: event.at,
        scope: event.scope.to_owned(),
        sender: event.sender.to_owned(),
        classifier: event.classifier.to_owned(),
        category: event.category.as_str().to_owned(),
        reason: String::from(reason_name(event.reason)),
        tag: tags.map(|key| key.tag(event.text.as_bytes())),
        sha256: None,
        bytes: event.text.len(),
        flagged_tag: event
            .flagged
            .and_then(|form| tags.map(|key| key.tag(form.as_bytes()))),
        flagged_sha256: None,
    }
}

impl Audit for FileAudit {
    fn redacted(&self, event: &Event<'_>) {
        let raw = self.keeper.as_ref().map_or(RawOutcome::Off, |keeper| {
            keeper.keep(event.event_id, event.text)
        });
        tracing::warn!(
            event_id = event.event_id,
            category = event.category.as_str(),
            reason = reason_name(event.reason),
            "the output gate redacted a message"
        );
        self.record(&Record::Redacted {
            event: judged(event, self.tags.as_deref()),
            raw,
        });
    }

    fn overridden(&self, event: &Event<'_>) {
        tracing::info!(
            event_id = event.event_id,
            category = event.category.as_str(),
            "a flagged message was delivered on an operator override"
        );
        self.record(&Record::Overridden {
            event: judged(event, self.tags.as_deref()),
        });
    }
}

/// The operator's overrides file, read afresh for every flagged message.
#[derive(Debug)]
pub struct FileOverrides {
    path: PathBuf,
    tags: Option<Arc<TagKey>>,
}

impl FileOverrides {
    /// The overrides file in `dir`. Until given the key
    /// ([`FileOverrides::with_tags`]) only overrides from before the tags
    /// apply.
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join(OVERRIDES_FILE),
            tags: None,
        }
    }

    /// These overrides, matching tags made under `tags`.
    #[must_use]
    pub fn with_tags(self, tags: Arc<TagKey>) -> Self {
        Self {
            tags: Some(tags),
            ..self
        }
    }

    /// Every override that parses. No file is no overrides.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Redaction`] for a file that exists but cannot be
    /// read: the gate takes it as no overrides (it stays shut), and the
    /// operator's commands report it.
    fn entries(&self) -> Result<Vec<Override>, Error> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(Error::Redaction {
                reason: format!("{}: {error}", self.path.display()),
            }),
        }
    }
}

impl Overrides for FileOverrides {
    fn allows(&self, scope: &str, category: &Category, flagged: &str) -> bool {
        let tag = self.tags.as_ref().map(|key| key.tag(flagged.as_bytes()));
        let entries = self.entries().unwrap_or_else(|error| {
            tracing::error!(%error, "cannot read the output gate's overrides: none apply");
            Vec::new()
        });
        entries.iter().any(|entry| {
            entry.category == category.as_str()
                && (entry.scope == scope || entry.scope == ANY_SCOPE)
                && entry.names(tag.as_deref(), flagged)
        })
    }
}

/// The actions `redactions` takes.
pub const USAGE: &str = "usage: hatchling-broker redactions <list | show <event-id> | stats | \
                         override <event-id> [--any-scope] | categories | raw <event-id>>";

/// Runs `hatchling-broker redactions <action>`, printing to stdout. The
/// master key is loaded only for `raw`.
///
/// # Errors
///
/// As [`command`].
pub fn run_redactions(
    config: &crate::Config,
    args: impl IntoIterator<Item = String>,
) -> Result<(), Error> {
    let redaction = RedactionConfig::from_env(config)?;
    let args: Vec<String> = args.into_iter().collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let text = command(&redaction, &args, || crate::load_master_key(&config.vault))?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{text}").map_err(|e| Error::Redaction {
        reason: format!("cannot write the report: {e}"),
    })
}

/// Every operator subcommand, for the broker's usage line.
pub const SUBCOMMANDS: &str = "usage: hatchling-broker [migrate-vault | rekey-vault [<new-key-file>] | \
     vault-keyring <init | enroll-totp | backup-codes | status | add-key | remove-key | label-key> | \
     redactions <list | show | stats | override | categories | raw> | vault-audit [log [count]] | \
     clean [--dry-run]]";

/// Runs the record-reading operator command `name` (`redactions` or
/// `vault-audit`) with `args`, printing to stdout. Any other name
/// is an unknown subcommand, answered with [`SUBCOMMANDS`].
///
/// # Errors
///
/// Returns [`Error::Usage`] for another name, and each command's errors.
pub fn run_operator(
    name: &str,
    config: &crate::Config,
    args: impl IntoIterator<Item = String>,
) -> Result<(), Error> {
    match name {
        "redactions" => run_redactions(config, args),
        "vault-audit" => {
            let args: Vec<String> = args.into_iter().collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let text = crate::vault_access::command(config, &args)?;
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{text}").map_err(|e| Error::VaultAccessLog {
                reason: format!("cannot write the report: {e}"),
            })
        }
        other => Err(Error::Usage {
            reason: format!("unknown subcommand {other:?}; {SUBCOMMANDS}"),
        }),
    }
}

/// `redactions` with `args` against `redaction`'s directory: the report,
/// or the kept text for `raw`. `key` is called only for `raw`.
///
/// # Errors
///
/// Returns [`Error::Usage`] for an unknown action or a missing event id,
/// [`Error::Redaction`] for an event that is not in the log, an override
/// the gate cannot honour, or a log that cannot be read or written, and
/// `key`'s error.
pub fn command(
    redaction: &RedactionConfig,
    args: &[&str],
    key: impl FnOnce() -> Result<MasterKey, Error>,
) -> Result<String, Error> {
    let log = redaction.dir.join(AUDIT_FILE);
    let usage = || Error::Usage {
        reason: String::from(USAGE),
    };
    match args {
        ["list"] => Ok(list(&read_log(&log)?)),
        ["show", id] => show(&read_log(&log)?.0, &FileOverrides::new(&redaction.dir), id),
        ["stats"] => Ok(stats(&read_log(&log)?)),
        ["override", id, rest @ ..] => {
            let any = match rest {
                [] => false,
                ["--any-scope"] => true,
                _ => return Err(usage()),
            };
            add_override(redaction, &read_log(&log)?.0, id, any)
        }
        ["categories"] => categories(redaction),
        ["raw", id] => raw(redaction, &read_log(&log)?.0, id, key),
        _ => Err(usage()),
    }
}

/// An event's time, as a UTC timestamp.
fn when(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|at| hatchling_scheduler::Timestamp::from_second(at).ok())
        .map_or_else(|| at.to_string(), |at| at.to_string())
}

/// The redactions and override deliveries, oldest first.
fn list((records, unreadable): &(Vec<Record>, usize)) -> String {
    let mut lines: Vec<String> = records
        .iter()
        .filter_map(|record| match record {
            Record::Redacted { event, .. } => Some(("redacted", event)),
            Record::Overridden { event } => Some(("delivered on override", event)),
            Record::Config { .. } | Record::Override(_) | Record::RawRead { .. } => None,
        })
        .map(|(kind, event)| {
            format!(
                "{}  {}  {kind}  {} ({})  {}  {} bytes",
                event.event_id,
                when(event.at),
                printable(&event.category),
                printable(&event.reason),
                printable(&event.scope),
                event.bytes
            )
        })
        .collect();
    if lines.is_empty() {
        lines.push(String::from("no redactions"));
    }
    if *unreadable > 0 {
        lines.push(format!(
            "warning: {unreadable} unreadable line(s) in the audit log"
        ));
    }
    lines.join("\n")
}

/// The redaction `id`, if the log has it.
fn find<'r>(records: &'r [Record], id: &str) -> Option<(&'r Judged, RawOutcome)> {
    records.iter().find_map(|record| match record {
        Record::Redacted { event, raw } if event.event_id == id => Some((event, *raw)),
        Record::Redacted { .. }
        | Record::Overridden { .. }
        | Record::Config { .. }
        | Record::Override(_)
        | Record::RawRead { .. } => None,
    })
}

fn unknown(id: &str) -> Error {
    Error::Redaction {
        reason: format!("no redaction {id:?} in the audit log"),
    }
}

/// Every recorded field of redaction `id`, and whether it is overridden.
fn show(records: &[Record], overrides: &FileOverrides, id: &str) -> Result<String, Error> {
    let (event, raw) = find(records, id).ok_or_else(|| unknown(id))?;
    let overridden: Vec<String> = overrides
        .entries()?
        .into_iter()
        .filter(|entry| entry.event_id == event.event_id)
        .map(|entry| entry.scope)
        .collect();
    let raw = match raw {
        RawOutcome::Off => "not kept (HATCHLING_REDACTION_KEEP_RAW is off)",
        RawOutcome::Kept => "kept, sealed: redactions raw shows it",
        RawOutcome::TooLarge => "not kept: too large",
        RawOutcome::Failed => "not kept: keeping it failed",
    };
    Ok([
        format!("event {}", event.event_id),
        format!("  at          {}", when(event.at)),
        format!("  scope       {}", printable(&event.scope)),
        format!("  sender      {}", printable(&event.sender)),
        format!("  classifier  {}", printable(&event.classifier)),
        format!("  category    {}", printable(&event.category)),
        format!("  reason      {}", printable(&event.reason)),
        format!("  bytes       {}", event.bytes),
        format!(
            "  tag         {}",
            shown_tag(event.tag.as_ref(), event.sha256.as_ref())
        ),
        format!(
            "  flagged     {}",
            if event.flagged_tag.is_none() && event.flagged_sha256.is_none() {
                String::from("(the whole message)")
            } else {
                shown_tag(event.flagged_tag.as_ref(), event.flagged_sha256.as_ref())
            }
        ),
        format!("  text        {raw}"),
        format!(
            "  overridden  {}",
            if overridden.is_empty() {
                String::from("no")
            } else {
                overridden.join(", ")
            }
        ),
    ]
    .join("\n"))
}

/// A recorded tag, or the unkeyed hash a record from before the tags
/// holds, marked as such.
fn shown_tag(tag: Option<&String>, sha256: Option<&String>) -> String {
    match (tag, sha256) {
        (Some(tag), _) => tag.clone(),
        (None, Some(hash)) => format!("sha256 {hash} (unkeyed, recorded before the tags)"),
        (None, None) => String::from("(none)"),
    }
}

/// Counts by category and by reason, the override deliveries, and the
/// failures that hid nothing but shut the gate.
fn stats((records, unreadable): &(Vec<Record>, usize)) -> String {
    use std::collections::BTreeMap;
    // Per category: redactions, then overrides.
    let mut categories: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    let (mut redacted, mut delivered, mut overrides) = (0_usize, 0_usize, 0_usize);
    for record in records {
        match record {
            Record::Redacted { event, .. } => {
                redacted += 1;
                categories.entry(event.category.as_str()).or_default().0 += 1;
                *reasons.entry(event.reason.as_str()).or_default() += 1;
            }
            Record::Overridden { .. } => delivered += 1,
            Record::Override(entry) => {
                overrides += 1;
                categories.entry(entry.category.as_str()).or_default().1 += 1;
            }
            Record::Config { .. } | Record::RawRead { .. } => {}
        }
    }
    let mut lines = vec![format!(
        "{redacted} redacted, {delivered} delivered on override, {overrides} override(s)"
    )];
    lines.extend(categories.iter().map(|(name, (count, over))| {
        format!(
            "  category {name}: {count} redacted, {over} override(s){}",
            override_rate(*count, *over)
        )
    }));
    lines.extend(
        reasons
            .iter()
            .map(|(name, count)| format!("  reason {name}: {count}")),
    );
    if *unreadable > 0 {
        lines.push(format!(
            "warning: {unreadable} unreadable line(s) in the audit log"
        ));
    }
    lines.join("\n")
}

/// A category's override rate: a category the operator overrides half the
/// time or more is misconfigured, and the report says so.
fn override_rate(redacted: usize, overrides: usize) -> String {
    if redacted == 0 {
        return String::new();
    }
    let percent = overrides * 100 / redacted;
    // `redacted` is not zero here, so no overrides never makes half.
    if overrides * 2 >= redacted {
        format!(" ({percent}% overridden: check this category)")
    } else {
        format!(" ({percent}% overridden)")
    }
}

/// Overrides redaction `id` in its scope, or every scope when `any`.
fn add_override(
    redaction: &RedactionConfig,
    records: &[Record],
    id: &str,
    any: bool,
) -> Result<String, Error> {
    let (event, _) = find(records, id).ok_or_else(|| unknown(id))?;
    let refuse = |reason: &str| Error::Redaction {
        reason: format!("{id} cannot be overridden: {reason}"),
    };
    if event.category == SECRET_CATEGORY {
        return Err(refuse("it carried a secret the broker holds"));
    }
    if event.category == FAILED_CATEGORY {
        return Err(refuse(
            "the classifier failed; nothing was judged to override",
        ));
    }
    let flagged = event
        .flagged()
        .ok_or_else(|| refuse("it has no flagged form"))?;
    let (flagged_tag, flagged_sha256) = match flagged {
        Flagged::Tag(tag) => (Some(tag), None),
        Flagged::Sha256(hash) => (None, Some(hash)),
    };
    let entry = Override {
        event_id: event.event_id.clone(),
        at: now(),
        scope: if any {
            String::from(ANY_SCOPE)
        } else {
            event.scope.clone()
        },
        category: event.category.clone(),
        flagged_tag,
        flagged_sha256,
        by: hatchling_sys::euid(),
    };
    let overrides = FileOverrides::new(&redaction.dir);
    if overrides.entries()?.iter().any(|held| {
        held.category == entry.category && held.same_form(&entry) && held.scope == entry.scope
    }) {
        return Ok(format!("{id} is already overridden in {}", entry.scope));
    }
    let line = serde_json::to_string(&entry).map_err(|e| Error::Redaction {
        reason: format!("cannot encode an override: {e}"),
    })?;
    ensure_dir(&redaction.dir)?;
    append(&overrides.path, &line).map_err(|e| Error::Redaction {
        reason: format!("{}: {e}", overrides.path.display()),
    })?;
    let scope = entry.scope.clone();
    FileAudit::new(&redaction.dir, None).write(&Record::Override(entry))?;
    Ok(format!(
        "overrode {id}: {} with that flagged form is delivered in {scope} from now on",
        event.category
    ))
}

/// The categories the gate judges by.
fn categories(redaction: &RedactionConfig) -> Result<String, Error> {
    let (phrases, sha256) = crate::output_gate::load_phrases(&redaction.dir)?;
    let on = |on: bool| if on { "on" } else { "off" };
    let mut lines = vec![
        format!(
            "{SECRET_CATEGORY} (built in, {}: a secret the broker holds)",
            on(redaction.builtins.secrets)
        ),
        format!(
            "{} (built in, {}: a PEM private-key block)",
            crate::output_gate::PRIVATE_KEY_CATEGORY,
            on(redaction.builtins.private_keys)
        ),
        format!("{FAILED_CATEGORY} (built in, always on: the classifier failed)"),
    ];
    let mut counts = std::collections::BTreeMap::<&str, usize>::new();
    for phrase in &phrases {
        *counts.entry(phrase.category.as_str()).or_default() += 1;
    }
    lines.extend(
        counts
            .iter()
            .map(|(name, count)| format!("{name} ({count} phrase(s))")),
    );
    lines.push(sha256.map_or_else(
        || String::from("no categories file"),
        |sha256| format!("categories file sha256 {sha256}"),
    ));
    Ok(lines.join("\n"))
}

/// Kept redaction `id`'s text, unsealed under `key`; the read is recorded.
fn raw(
    redaction: &RedactionConfig,
    records: &[Record],
    id: &str,
    key: impl FnOnce() -> Result<MasterKey, Error>,
) -> Result<String, Error> {
    let (event, raw) = find(records, id).ok_or_else(|| unknown(id))?;
    if event.event_id == TAG_KEY_NAME {
        return Err(Error::Redaction {
            reason: format!("{id} is the audit-tag key, not a kept text"),
        });
    }
    if raw != RawOutcome::Kept {
        return Err(Error::Redaction {
            reason: format!("{id}'s text was not kept"),
        });
    }
    let name: SecretName = event.event_id.parse().map_err(|_| Error::Redaction {
        reason: format!("{id} is not a record name"),
    })?;
    let key = key()?;
    let secret = SealedVault::open_in(
        &redaction.dir.join(RAW_DIR),
        &key,
        hatchling_vault::Store::REDACTIONS,
    )
    .and_then(|vault| vault.fetch(&name))
    .map_err(|source| Error::Vault { source })?;
    let text = String::from_utf8(secret.as_bytes().to_vec()).map_err(|_| Error::Redaction {
        reason: format!("{id}'s kept text is not UTF-8"),
    })?;
    FileAudit::new(&redaction.dir, None).write(&Record::RawRead {
        event_id: event.event_id.clone(),
        at: now(),
        by: hatchling_sys::euid(),
    })?;
    Ok(escaped(&text, &['\n', '\t']))
}

/// `text` for one line of the operator's terminal: every control
/// character shown escaped ([`escaped`]).
fn printable(text: &str) -> String {
    escaped(text, &[])
}

/// `text` for the operator's terminal: its control characters, terminal
/// escape sequences among them, shown as escapes and never acted on;
/// those in `keep` are left as they are. Someone outside chose this text.
fn escaped(text: &str, keep: &[char]) -> String {
    text.chars()
        .fold(String::with_capacity(text.len()), |mut out, c| {
            if c.is_control() && !keep.contains(&c) {
                out.extend(c.escape_default());
            } else {
                out.push(c);
            }
            out
        })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    use super::*;
    use crate::api_auth::tests::{Scratch, owner_config, scratch};

    #[test]
    fn text_outside_chose_is_shown_escaped_on_the_operators_terminal() {
        let hostile = "ok\u{1b}]0;owned\u{7}\nline\tend\r";
        assert_eq!(
            printable(hostile),
            "ok\\u{1b}]0;owned\\u{7}\\nline\\tend\\r"
        );
        assert_eq!(
            escaped(hostile, &['\n', '\t']),
            "ok\\u{1b}]0;owned\\u{7}\nline\tend\\r"
        );
        assert_eq!(printable("plain words"), "plain words");
    }

    fn settings(gate: &str) -> Settings {
        Settings {
            gate: gate.to_owned(),
            classifier: Some(String::from("c@1")),
            keep_raw: false,
            timeout_ms: 2000,
            categories: vec![String::from("codename")],
            categories_sha256: None,
            builtins: vec![String::from("secret")],
        }
    }

    /// The gate's settings over a fresh redaction directory.
    fn deployment(name: &str) -> (Scratch, RedactionConfig) {
        let dir = scratch(name);
        let redaction = RedactionConfig {
            enabled: true,
            dir: dir.0.join("red"),
            keep_raw: false,
            timeout: Duration::from_secs(1),
            builtins: crate::output_gate::Builtins::ALL,
        };
        ensure_dir(&redaction.dir).unwrap();
        (dir, redaction)
    }

    const fn event<'a>(id: &'a str, category: &'a Category, flagged: Option<&'a str>) -> Event<'a> {
        Event {
            event_id: id,
            at: 1_790_000_000,
            scope: "session:a",
            sender: "uid:1",
            classifier: "c@1",
            category,
            reason: Reason::Flagged,
            text: "the nightjar plan",
            flagged,
        }
    }

    /// A tag key for `redaction`, sealed under a fresh master key.
    fn tag_key(redaction: &RedactionConfig) -> Arc<TagKey> {
        let key = MasterKey::generate().unwrap();
        Arc::new(TagKey::load_or_create(&redaction.dir.join(RAW_DIR), &key).unwrap())
    }

    /// One redaction of each kind, through the audit, tagged under the key
    /// it returns.
    fn redactions(redaction: &RedactionConfig) -> Arc<TagKey> {
        let tags = tag_key(redaction);
        let audit = FileAudit::new(&redaction.dir, None).with_tags(Arc::clone(&tags));
        audit.configured(settings("on")).unwrap();
        let codename = Category::new("codename").unwrap();
        let secret = Category::new(SECRET_CATEGORY).unwrap();
        let failed = Category::new(FAILED_CATEGORY).unwrap();
        audit.redacted(&event("red-1", &codename, Some("nightjarplan")));
        audit.redacted(&event("red-2", &secret, None));
        audit.redacted(&Event {
            reason: Reason::ClassifierFailed(Failure::Timeout),
            ..event("red-3", &failed, None)
        });
        audit.overridden(&event("red-4", &codename, Some("nightjarplan")));
        tags
    }

    #[test]
    fn a_category_overridden_half_the_time_is_called_out() {
        assert_eq!(override_rate(0, 0), "");
        assert_eq!(override_rate(0, 2), "");
        assert_eq!(override_rate(4, 0), " (0% overridden)");
        assert_eq!(override_rate(4, 1), " (25% overridden)");
        assert_eq!(override_rate(3, 1), " (33% overridden)");
        assert_eq!(
            override_rate(2, 1),
            " (50% overridden: check this category)"
        );
        assert_eq!(
            override_rate(1, 1),
            " (100% overridden: check this category)"
        );
    }

    #[test]
    fn a_reason_is_spelled_one_way_in_the_log() {
        assert_eq!(reason_name(Reason::Flagged), "flagged");
        assert_eq!(reason_name(Reason::NoRetry), "no-retry");
        assert_eq!(
            reason_name(Reason::ClassifierFailed(Failure::Error)),
            "classifier-error"
        );
        assert_eq!(
            reason_name(Reason::ClassifierFailed(Failure::Panic)),
            "classifier-panic"
        );
        assert_eq!(
            reason_name(Reason::ClassifierFailed(Failure::Timeout)),
            "classifier-timeout"
        );
        assert!(now() > 1_700_000_000);
        assert_eq!(when(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            when(u64::MAX),
            u64::MAX.to_string(),
            "out of range: the number"
        );
    }

    #[test]
    fn the_records_live_in_an_owner_only_directory() {
        let dir = scratch("red-dir");
        let red = dir.0.join("red");
        ensure_dir(&red).unwrap();
        let mode = std::fs::metadata(&red).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        std::fs::set_permissions(&red, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            ensure_dir(&red),
            Err(Error::InvalidConfig {
                key: "HATCHLING_REDACTION_DIR",
                ..
            })
        ));
        let log = dir.0.join("log");
        append(&log, "one").unwrap();
        append(&log, "two").unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "one\ntwo\n");
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn the_tag_key_is_made_once_sealed_and_kept() {
        let (_dir, redaction) = deployment("red-tag-key");
        let raw_dir = redaction.dir.join(RAW_DIR);
        let key = MasterKey::generate().unwrap();
        let made = TagKey::load_or_create(&raw_dir, &key).unwrap();
        let loaded = TagKey::load_or_create(&raw_dir, &key).unwrap();
        assert_eq!(made.tag(b"nightjarplan"), loaded.tag(b"nightjarplan"));
        assert_ne!(made.tag(b"nightjarplan"), made.tag(b"nightjarplans"));
        assert_eq!(format!("{made:?}"), "TagKey(..)");
        // The tag is HMAC-SHA-256 under the sealed key: no unkeyed hash.
        let vault =
            SealedVault::open_in(&raw_dir, &key, hatchling_vault::Store::REDACTIONS).unwrap();
        let stored = vault.fetch(&TAG_KEY_NAME.parse().unwrap()).unwrap();
        let mut mac = <Hmac<Sha256>>::new_from_slice(stored.as_bytes()).unwrap();
        mac.update(b"nightjarplan");
        assert_eq!(
            made.tag(b"nightjarplan"),
            crate::approval::hex_token("", &mac.finalize().into_bytes())
        );
        assert_ne!(made.tag(b"nightjarplan"), sha256_hex(b"nightjarplan"));
        // Another vault's key can't open it; a damaged one is refused.
        let stranger = MasterKey::generate().unwrap();
        assert!(matches!(
            TagKey::load_or_create(&raw_dir, &stranger),
            Err(Error::Vault { .. })
        ));
        vault
            .store(
                &TAG_KEY_NAME.parse().unwrap(),
                &Secret::new(b"short".to_vec()).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            TagKey::load_or_create(&raw_dir, &key),
            Err(Error::Redaction { reason }) if reason.contains("not 32 bytes")
        ));
        // Without a key, nothing is tagged.
        let codename = Category::new("codename").unwrap();
        let untagged = judged(&event("red-1", &codename, Some("nightjarplan")), None);
        assert_eq!(
            (untagged.tag, untagged.flagged_tag, untagged.sha256),
            (None, None, None)
        );
    }

    #[test]
    fn the_log_holds_tags_and_sizes_never_the_text_or_an_unkeyed_hash() {
        let (_dir, redaction) = deployment("red-log");
        let log = redaction.dir.join(AUDIT_FILE);
        assert_eq!(
            read_log(&log).unwrap(),
            (Vec::new(), 0),
            "no log, no records"
        );
        let tags = redactions(&redaction);
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(!text.contains("nightjar"), "{text}");
        for unkeyed in [
            sha256_hex(b"the nightjar plan"),
            sha256_hex(b"nightjarplan"),
        ] {
            assert!(!text.contains(&unkeyed), "{text}");
        }
        let (records, unreadable) = read_log(&log).unwrap();
        assert_eq!((records.len(), unreadable), (5, 0));
        let Some(Record::Redacted {
            event: recorded,
            raw,
        }) = records.get(1)
        else {
            unreachable!("{records:?}");
        };
        assert_eq!(
            *recorded,
            Judged {
                event_id: String::from("red-1"),
                at: 1_790_000_000,
                scope: String::from("session:a"),
                sender: String::from("uid:1"),
                classifier: String::from("c@1"),
                category: String::from("codename"),
                reason: String::from("flagged"),
                tag: Some(tags.tag(b"the nightjar plan")),
                sha256: None,
                bytes: 17,
                flagged_tag: Some(tags.tag(b"nightjarplan")),
                flagged_sha256: None,
            }
        );
        assert_eq!(*raw, RawOutcome::Off);
        assert!(
            matches!(records.get(4), Some(Record::Overridden { event }) if event.event_id == "red-4")
        );
        let codename = Category::new("codename").unwrap();
        assert_eq!(
            judged(
                &event("red-1", &codename, Some("nightjarplan")),
                Some(&tags)
            ),
            *recorded
        );
        assert_eq!(
            recorded.flagged(),
            Some(Flagged::Tag(tags.tag(b"nightjarplan")))
        );
        // A record from before the tags still reads.
        let legacy = r#"{"kind":"redacted","event_id":"red-0","at":1,"scope":"s","sender":"u","classifier":"c","category":"codename","reason":"flagged","sha256":"aa","bytes":3,"flagged_sha256":"bb","raw":"off"}"#;
        let Ok(Record::Redacted { event: old, .. }) = serde_json::from_str::<Record>(legacy) else {
            unreachable!("a record from before the tags must read");
        };
        assert_eq!(
            (
                old.sha256.as_deref(),
                old.flagged_sha256.as_deref(),
                old.tag
            ),
            (Some("aa"), Some("bb"), None)
        );

        // A line that is not a record is counted, not trusted.
        append(&log, "{not json").unwrap();
        append(&log, "").unwrap();
        assert_eq!(read_log(&log).unwrap().1, 1);
        assert!(matches!(
            read_log(&redaction.dir),
            Err(Error::Redaction { .. })
        ));
    }

    #[test]
    fn a_settings_change_is_recorded_with_what_it_was() {
        let (_dir, redaction) = deployment("red-config");
        let audit = FileAudit::new(&redaction.dir, None);
        audit.configured(settings("on")).unwrap();
        audit.configured(settings("on")).unwrap();
        audit.configured(settings("off")).unwrap();
        let (records, _) = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        let changed: Vec<Option<&str>> = records
            .iter()
            .map(|record| match record {
                Record::Config { changed_from, .. } => {
                    changed_from.as_ref().map(|was| was.gate.as_str())
                }
                Record::Redacted { .. }
                | Record::Overridden { .. }
                | Record::Override(_)
                | Record::RawRead { .. } => Some("not a config record"),
            })
            .collect();
        assert_eq!(changed, [None, None, Some("on")]);
        assert!(
            records.iter().all(
                |record| matches!(record, Record::Config { by, .. } if *by == hatchling_sys::euid())
            ),
            "each settings record names who started the broker"
        );
        audit
            .write(&Record::RawRead {
                event_id: String::from("red-1"),
                at: 1,
                by: 0,
            })
            .unwrap();
        audit.configured(settings("off")).unwrap();
        let (records, _) = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        assert!(
            matches!(
                records.last(),
                Some(Record::Config {
                    changed_from: None,
                    ..
                })
            ),
            "compared with the last config record, past other records"
        );

        // Unwritable: the error is returned at start, and only logged after.
        let broken = FileAudit::new(&redaction.dir.join("missing"), None);
        assert!(matches!(
            broken.write(&Record::RawRead {
                event_id: String::new(),
                at: 0,
                by: 0,
            }),
            Err(Error::Redaction { .. })
        ));
        broken.record(&Record::RawRead {
            event_id: String::new(),
            at: 0,
            by: 0,
        });
        let codename = Category::new("codename").unwrap();
        broken.redacted(&event("red-9", &codename, None));
        assert!(!redaction.dir.join("missing").exists());
    }

    #[test]
    fn an_override_holds_for_its_category_form_and_scope() {
        let (_dir, redaction) = deployment("red-overrides");
        let overrides = FileOverrides::new(&redaction.dir);
        let codename = Category::new("codename").unwrap();
        assert!(overrides.entries().unwrap().is_empty());
        assert!(!overrides.allows("session:a", &codename, "nightjarplan"));
        // An override from before the tags names its form unkeyed.
        let entry = |scope: &str| Override {
            event_id: String::from("red-1"),
            at: 1,
            scope: scope.to_owned(),
            category: String::from("codename"),
            flagged_tag: None,
            flagged_sha256: Some(sha256_hex(b"nightjarplan")),
            by: 1,
        };
        let path = redaction.dir.join(OVERRIDES_FILE);
        append(&path, &serde_json::to_string(&entry("session:a")).unwrap()).unwrap();
        append(&path, "garbage").unwrap();
        assert_eq!(overrides.entries().unwrap(), [entry("session:a")]);
        assert!(overrides.allows("session:a", &codename, "nightjarplan"));
        assert!(!overrides.allows("session:b", &codename, "nightjarplan"));
        assert!(!overrides.allows(
            "session:a",
            &Category::new("other").unwrap(),
            "nightjarplan"
        ));
        assert!(!overrides.allows("session:a", &codename, "anotherform"));
        append(&path, &serde_json::to_string(&entry(ANY_SCOPE)).unwrap()).unwrap();
        assert!(overrides.allows("session:b", &codename, "nightjarplan"));
        // One made since names it by its tag, which only the key matches.
        let tags = tag_key(&redaction);
        let tagged = Override {
            flagged_tag: Some(tags.tag(b"anotherform")),
            flagged_sha256: None,
            ..entry("session:a")
        };
        append(&path, &serde_json::to_string(&tagged).unwrap()).unwrap();
        assert!(!overrides.allows("session:a", &codename, "anotherform"));
        let keyed = FileOverrides::new(&redaction.dir).with_tags(Arc::clone(&tags));
        assert!(keyed.allows("session:a", &codename, "anotherform"));
        assert!(!keyed.allows("session:b", &codename, "anotherform"));
        assert!(keyed.allows("session:b", &codename, "nightjarplan"));
        assert!(!keyed.allows("session:a", &codename, "yetanother"));
        assert!(tagged.names(Some(&tags.tag(b"anotherform")), "anotherform"));
        assert!(!tagged.names(None, "anotherform"));
        assert!(entry("s").names(None, "nightjarplan"));
        assert!(!entry("s").names(Some(&tags.tag(b"nightjarplan")), "anotherform"));
        assert!(tagged.same_form(&tagged.clone()));
        assert!(!tagged.same_form(&entry("session:a")));
        assert!(entry("x").same_form(&entry("y")));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            matches!(overrides.entries(), Err(Error::Redaction { .. })),
            "an unreadable file is an error, not an empty list"
        );
        assert!(
            !overrides.allows("session:b", &codename, "nightjarplan"),
            "unreadable: none apply, the gate stays shut"
        );
    }

    /// `shown` gives red-1's tags under `tags`: the message's and its
    /// flagged form's.
    fn shows_tags(shown: &str, tags: &TagKey) {
        for line in [
            format!("  tag         {}", tags.tag(b"the nightjar plan")),
            format!("  flagged     {}", tags.tag(b"nightjarplan")),
        ] {
            assert!(shown.contains(&line), "{shown}");
        }
    }

    #[test]
    fn the_operator_reviews_redactions_by_tag() {
        let (_dir, redaction) = deployment("red-review");
        let no_key = || -> Result<MasterKey, Error> { unreachable!("only raw needs the key") };
        assert_eq!(
            command(&redaction, &["list"], no_key).unwrap(),
            "no redactions"
        );
        let tags = redactions(&redaction);
        let log = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        let listed = list(&log);
        assert_eq!(listed.lines().count(), 4, "{listed}");
        assert!(listed.starts_with("red-1  2026-09-21T"), "{listed}");
        assert!(
            listed.contains("redacted  codename (flagged)  session:a  17 bytes"),
            "{listed}"
        );
        assert!(listed.contains("delivered on override"), "{listed}");
        assert!(!listed.contains("nightjar"));
        let counted = stats(&log);
        assert_eq!(
            counted,
            "3 redacted, 1 delivered on override, 0 override(s)\n  \
             category classifier-failure: 1 redacted, 0 override(s) (0% overridden)\n  \
             category codename: 1 redacted, 0 override(s) (0% overridden)\n  \
             category secret: 1 redacted, 0 override(s) (0% overridden)\n  \
             reason classifier-timeout: 1\n  reason flagged: 2"
        );
        assert_eq!(command(&redaction, &["stats"], no_key).unwrap(), counted);
        let shown = show(&log.0, &FileOverrides::new(&redaction.dir), "red-1").unwrap();
        assert!(shown.contains("  category    codename"), "{shown}");
        shows_tags(&shown, &tags);
        assert!(
            shown.contains("not kept (HATCHLING_REDACTION_KEEP_RAW is off)"),
            "{shown}"
        );
        assert!(shown.ends_with("  overridden  no"), "{shown}");
        let whole = command(&redaction, &["show", "red-2"], no_key).unwrap();
        assert!(whole.contains("(the whole message)"), "{whole}");
        assert!(
            find(&log.0, "red-4").is_none(),
            "an override delivery is not a redaction"
        );
        assert!(
            matches!(unknown("red-x"), Error::Redaction { reason } if reason.contains("\"red-x\""))
        );
        assert!(matches!(
            command(&redaction, &["show", "red-x"], no_key),
            Err(Error::Redaction { .. })
        ));

        // Unreadable lines are reported, not hidden.
        append(&redaction.dir.join(AUDIT_FILE), "{").unwrap();
        let log = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        assert!(list(&log).ends_with("warning: 1 unreadable line(s) in the audit log"));
        assert!(stats(&log).ends_with("warning: 1 unreadable line(s) in the audit log"));

        let bads: [&[&str]; 5] = [
            &[],
            &["nope"],
            &["show"],
            &["override"],
            &["override", "red-1", "--all"],
        ];
        for bad in bads {
            assert!(
                matches!(command(&redaction, bad, no_key), Err(Error::Usage { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn an_override_is_appended_once_and_recorded() {
        let (_dir, redaction) = deployment("red-override");
        let no_key = || -> Result<MasterKey, Error> { unreachable!("only raw needs the key") };
        let tags = redactions(&redaction);
        let (records, _) = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        for (id, why) in [
            ("red-2", "it carried a secret the broker holds"),
            ("red-3", "the classifier failed"),
            ("red-x", "no redaction"),
        ] {
            let refused = add_override(&redaction, &records, id, false);
            assert!(
                matches!(&refused, Err(Error::Redaction { reason }) if reason.contains(why)),
                "{refused:?}"
            );
        }
        let done = command(&redaction, &["override", "red-1"], no_key).unwrap();
        assert!(done.contains("in session:a"), "{done}");
        let again = add_override(&redaction, &records, "red-1", false).unwrap();
        assert_eq!(again, "red-1 is already overridden in session:a");
        let everywhere =
            command(&redaction, &["override", "red-1", "--any-scope"], no_key).unwrap();
        assert!(everywhere.contains("in *"), "{everywhere}");
        let overrides = FileOverrides::new(&redaction.dir).with_tags(tags);
        let codename = Category::new("codename").unwrap();
        assert!(overrides.allows("session:z", &codename, "nightjarplan"));
        assert_eq!(overrides.entries().unwrap().len(), 2);
        assert!(
            overrides
                .entries()
                .unwrap()
                .iter()
                .all(|entry| entry.flagged_tag.is_some() && entry.flagged_sha256.is_none()),
            "an override names its form by tag alone"
        );
        let (records, _) = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        let logged = records
            .iter()
            .filter(|record| matches!(record, Record::Override(_)))
            .count();
        assert_eq!(logged, 2, "each override is in the audit log");
        let shown = command(&redaction, &["show", "red-1"], no_key).unwrap();
        assert!(shown.ends_with("  overridden  session:a, *"), "{shown}");
        let file = redaction.dir.join(OVERRIDES_FILE);
        let kept = std::fs::read(&file).unwrap();
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        for args in [&["show", "red-1"][..], &["override", "red-1"]] {
            assert!(
                matches!(
                    command(&redaction, args, no_key),
                    Err(Error::Redaction { .. })
                ),
                "{args:?} must report an unreadable overrides file"
            );
        }
        std::fs::remove_dir(&file).unwrap();
        std::fs::write(&file, kept).unwrap();
        let counted = stats(&read_log(&redaction.dir.join(AUDIT_FILE)).unwrap());
        assert!(
            counted.contains(
                "category codename: 1 redacted, 2 override(s) (200% overridden: check this category)"
            ),
            "{counted}"
        );
        let entries = overrides.entries().unwrap();
        assert!(
            entries
                .iter()
                .all(|entry| entry.by == hatchling_sys::euid()),
            "{entries:?}"
        );

        // A redaction with no flagged form names nothing to override.
        let audit = FileAudit::new(&redaction.dir, None);
        audit.redacted(&event("red-5", &codename, None));
        let (records, _) = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        let refused = add_override(&redaction, &records, "red-5", false);
        assert!(
            matches!(&refused, Err(Error::Redaction { reason }) if reason.contains("no flagged form")),
            "{refused:?}"
        );
    }

    #[test]
    fn a_redaction_from_before_the_tags_is_overridden_by_its_hash() {
        let (_dir, redaction) = deployment("red-legacy");
        let no_key = || -> Result<MasterKey, Error> { unreachable!("only raw needs the key") };
        let codename = Category::new("codename").unwrap();
        let legacy = Judged {
            event_id: String::from("red-6"),
            tag: None,
            sha256: Some(sha256_hex(b"the nightjar plan")),
            flagged_tag: None,
            flagged_sha256: Some(sha256_hex(b"kestrel")),
            ..judged(&event("red-6", &codename, None), None)
        };
        assert_eq!(
            legacy.flagged(),
            Some(Flagged::Sha256(sha256_hex(b"kestrel")))
        );
        FileAudit::new(&redaction.dir, None)
            .write(&Record::Redacted {
                event: legacy,
                raw: RawOutcome::Off,
            })
            .unwrap();
        let shown = command(&redaction, &["show", "red-6"], no_key).unwrap();
        assert!(
            shown.contains("(unkeyed, recorded before the tags)"),
            "{shown}"
        );
        command(&redaction, &["override", "red-6"], no_key).unwrap();
        assert!(FileOverrides::new(&redaction.dir).allows("session:a", &codename, "kestrel"));
        assert_eq!(shown_tag(None, None), "(none)");
    }

    #[test]
    fn the_categories_are_listed_built_ins_first() {
        let (_dir, redaction) = deployment("red-categories");
        let no_key = || -> Result<MasterKey, Error> { unreachable!("only raw needs the key") };
        let none = categories(&redaction).unwrap();
        assert!(none.starts_with("secret (built in, on:"), "{none}");
        assert!(none.contains("private-key (built in, on:"), "{none}");
        let fewer = RedactionConfig {
            builtins: crate::output_gate::Builtins {
                secrets: false,
                private_keys: true,
            },
            ..redaction.clone()
        };
        let shown = categories(&fewer).unwrap();
        assert!(shown.starts_with("secret (built in, off:"), "{shown}");
        assert!(shown.contains("private-key (built in, on:"), "{shown}");
        assert!(none.ends_with("no categories file"), "{none}");
        std::fs::write(
            redaction.dir.join(crate::output_gate::CATEGORIES_FILE),
            "codename: nightjar\ncodename: kestrel\nlaunch: go time\n",
        )
        .unwrap();
        let some = command(&redaction, &["categories"], no_key).unwrap();
        assert!(
            some.contains("\ncodename (2 phrase(s))\nlaunch (1 phrase(s))\n"),
            "{some}"
        );
        assert!(
            !some.contains("nightjar"),
            "names and counts, not phrases: {some}"
        );
        assert!(some.contains("categories file sha256 "), "{some}");
    }

    #[test]
    fn kept_text_is_read_back_under_the_key_and_the_read_recorded() {
        let (_dir, redaction) = deployment("red-raw");
        let key = MasterKey::generate().unwrap();
        let audit = FileAudit::new(&redaction.dir, None);
        let codename = Category::new("codename").unwrap();
        audit.redacted(&event("red-1", &codename, None));
        audit
            .write(&Record::Redacted {
                event: judged(&event("red-2", &codename, None), None),
                raw: RawOutcome::Kept,
            })
            .unwrap();
        audit
            .write(&Record::Redacted {
                event: judged(&event(TAG_KEY_NAME, &codename, None), None),
                raw: RawOutcome::Kept,
            })
            .unwrap();
        SealedVault::open_in(
            &redaction.dir.join(RAW_DIR),
            &key,
            hatchling_vault::Store::REDACTIONS,
        )
        .unwrap()
        .store(
            &"red-2".parse().unwrap(),
            &Secret::new(b"the nightjar plan".to_vec()).unwrap(),
        )
        .unwrap();
        let (records, _) = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        let not_kept = raw(&redaction, &records, "red-1", || unreachable!("not needed"));
        assert!(
            matches!(&not_kept, Err(Error::Redaction { reason }) if reason.contains("was not kept"))
        );
        let wrong = command(&redaction, &["raw", "red-2"], || {
            MasterKey::generate().map_err(|source| Error::Vault { source })
        });
        assert!(
            matches!(wrong, Err(Error::Vault { .. })),
            "raw reaches the sealed store, and the wrong key opens nothing: {wrong:?}"
        );
        let text = raw(&redaction, &records, "red-2", || Ok(key)).unwrap();
        assert_eq!(text, "the nightjar plan");
        let (records, _) = read_log(&redaction.dir.join(AUDIT_FILE)).unwrap();
        assert!(
            matches!(records.last(), Some(Record::RawRead { event_id, by, .. }) if event_id == "red-2" && *by == hatchling_sys::euid())
        );
        assert!(raw(&redaction, &records, "red-x", || unreachable!("not needed")).is_err());
        let refused = raw(&redaction, &records, TAG_KEY_NAME, || {
            unreachable!("not needed")
        });
        assert!(
            matches!(&refused, Err(Error::Redaction { reason }) if reason.contains("audit-tag key")),
            "a forged record never reads the key out: {refused:?}"
        );
        let shown = command(&redaction, &["show", "red-2"], || {
            unreachable!("not needed")
        })
        .unwrap();
        assert!(
            shown.contains("kept, sealed: redactions raw shows it"),
            "{shown}"
        );
    }

    #[test]
    fn the_operator_commands_share_one_dispatch_and_name_the_rest() {
        let dir = scratch("red-operator");
        let mut config = owner_config(None);
        config.vault.root = dir.0.join("vault");
        run_operator("vault-audit", &config, [String::from("log")]).unwrap();
        run_operator("redactions", &config, [String::from("list")]).unwrap();
        let unknown = run_operator("nope", &config, Vec::new());
        assert!(
            matches!(&unknown, Err(Error::Usage { reason }) if reason.starts_with("unknown subcommand \"nope\"; usage: hatchling-broker [")
                && reason.contains("| vault-audit [log [count]] | clean [--dry-run]]")),
            "{unknown:?}"
        );
        assert!(!SUBCOMMANDS.contains("  "), "{SUBCOMMANDS}");
    }

    #[test]
    fn the_cli_reads_the_deployments_own_records() {
        let dir = scratch("red-cli");
        let mut config = owner_config(None);
        config.vault.root = dir.0.join("vault");
        run_redactions(&config, [String::from("list")]).unwrap();
        assert!(matches!(
            run_redactions(&config, [String::from("nope")]),
            Err(Error::Usage { .. })
        ));
    }
}
