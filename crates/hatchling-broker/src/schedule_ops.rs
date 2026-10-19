//! The broker's schedule store, the schedule session ops, and the
//! delivery path for scheduled output.
//!
//! **The store is sealed and broker-private.** It is a [`SealedVault`]
//! under the broker's master key, at its own root
//! (`HATCHLING_SCHEDULER_DIR`). Config parsing keeps that root apart
//! from the credential vault's, so no vault op or grant can name an
//! entry. A cell creates a schedule and learns its id; it lists what
//! it may see (never a task's text) and cancels by id.
//!
//! **Its layout:** one sealed entry per schedule (`schedule-<id>`), an
//! index of ids (the vault deliberately has no list op), and parked
//! results the same way (`parked-<n>` plus `parked-index`). All of it is
//! bounded: [`MAX_SCHEDULES`], [`MAX_PARKED`], and each entry within the
//! vault's size limit.
//!
//! **Who made it.** Each schedule records its [`Creator`]: the owner,
//! through the local API, or the agent, through a session (a cell, or a
//! run asking for its follow-up). A cell lists and cancels only the
//! agent's; the owner's it can neither see nor cancel, and a cancel of
//! one answers `NotFound`, as for an id that doesn't exist. Cells aren't
//! told apart from each other: every agent session runs as the one
//! agent uid and names its own cell, so until WO-007 binds a session to
//! the cell the broker spawned, "the agent's" is every agent session's.
//!
//! **"The creator's grants"** are the broker's grants for the session
//! peer, the one authorized uid, translated into the schedule vocabulary
//! by [`held_grants`]. A schedule may hold only those, and a fire gets
//! only those it still holds.
//!
//! **Hooks ** live in the same sealed store (`hook-<id>` plus
//! `hook-index`, at most [`MAX_HOOKS`]), created and cancelled through the
//! peer-authorized session like schedules. A hook's event source must be
//! one the broker can authenticate for the creator
//! ([`authenticated_channels`], or an existing schedule).
//!
//! **Follow-ups ** are one-shot schedules a run asks for through
//! its own session, the one scheduler op that session has
//! ([`FollowupScope`]). The broker, not the request, supplies the run's
//! grants, channel, and depth; each run gets one.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hatchling_channel::{ChannelId, LocalHub, Message, SenderId};
use hatchling_core::{
    FeedSpec, HookOp, HookResult, HookSource, MAX_SECRET_BYTES, ScheduleGrant, ScheduleKind,
    ScheduleOp, ScheduleRefusal, ScheduleResult, ScheduleSpec, ScheduleSummary, SecretName,
};
use hatchling_scheduler::{
    Creator, Delivery, Dispatcher, FollowupParent, Hook, Launcher, Parked, Schedule, Services,
    Sources, Store, TickReport, Timestamp, validate, validate_followup, validate_hook,
};
use hatchling_vault::{MasterKey, SealedVault, Secret};

use crate::config::FollowupScope;
use crate::{Config, Error};

/// Most schedules the store keeps.
pub const MAX_SCHEDULES: usize = 64;

/// Most parked results the store keeps. Past it the oldest is dropped,
/// loudly.
pub const MAX_PARKED: usize = 64;

/// Longest parked text kept, in bytes. Longer output is cut at a char
/// boundary and marked, so every parked entry fits the vault's bound.
pub const MAX_PARKED_TEXT_BYTES: usize = 16 * 1024;

/// Every schedule id starts with this.
pub const SCHEDULE_ID_PREFIX: &str = "sch-";

/// Most hooks the store keeps.
pub const MAX_HOOKS: usize = 64;

/// Every hook id starts with this, so a hook id is never a schedule id.
pub const HOOK_ID_PREFIX: &str = "hk-";

const INDEX: &str = "schedule-index";
const PARKED_INDEX: &str = "parked-index";
const HOOK_INDEX: &str = "hook-index";

/// The runs that have taken their one follow-up, by run token. A run's
/// entry is released when the run ends, so this holds live runs only.
static FOLLOWUPS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Takes `run`'s one follow-up. False when it already has.
fn claim_followup(run: &str) -> bool {
    FOLLOWUPS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(run.to_owned())
}

/// Releases `run`'s follow-up claim: the run ended, or its follow-up was
/// not stored after all.
pub(crate) fn release_followup(run: &str) {
    FOLLOWUPS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(run);
}

/// The channels whose messages the broker authenticates for the peer.
///
/// That is the local channel, when the broker has an ingress for it (its
/// kernel peer gate is the authentication) and the peer has the
/// channel. No other channel has an authenticated ingress in v0.1.
#[must_use]
pub fn authenticated_channels(config: &Config) -> BTreeSet<String> {
    let local = ChannelId::local().0;
    if config.channel_ingress.is_some() && config.channels.contains(&local) {
        BTreeSet::from([local])
    } else {
        BTreeSet::new()
    }
}

/// Serializes every store change in the process: sessions create and
/// cancel on their own threads, and the dispatcher updates beside them.
static STORE: Mutex<()> = Mutex::new(());

/// The creator's grants, in the schedule vocabulary: every grant the
/// broker's configuration gives the authorized peer.
#[must_use]
pub fn held_grants(config: &Config) -> BTreeSet<ScheduleGrant> {
    let vault = config
        .vault
        .grants
        .iter()
        .map(|grant| ScheduleGrant::Vault {
            action: grant.action.to_string(),
            name: grant.name.as_str().to_owned(),
        });
    let egress = config
        .egress
        .grants
        .iter()
        .map(|destination| ScheduleGrant::Egress {
            destination: destination.clone(),
        });
    let memory = config
        .memory
        .grants
        .iter()
        .map(|grant| ScheduleGrant::Memory {
            scope: grant.scope.clone(),
            action: grant.action.name().to_owned(),
        });
    let goals = config
        .goals
        .grants
        .iter()
        .map(|action| ScheduleGrant::Goals {
            action: action.name().to_owned(),
        });
    let channels = config
        .channels
        .iter()
        .map(|channel| ScheduleGrant::ChannelSend {
            channel: channel.clone(),
        });
    vault
        .chain(egress)
        .chain(memory)
        .chain(goals)
        .chain(channels)
        .collect()
}

/// The sealed schedule store at `root`.
#[derive(Debug)]
pub struct SealedScheduleStore<'k> {
    root: &'k Path,
    key: &'k MasterKey,
}

impl<'k> SealedScheduleStore<'k> {
    /// The store at `root`, sealed under `key`. Nothing is opened until
    /// an operation needs it.
    #[must_use]
    pub const fn new(root: &'k Path, key: &'k MasterKey) -> Self {
        Self { root, key }
    }

    fn vault(&self) -> Result<SealedVault<'k>, Error> {
        SealedVault::open_in(self.root, self.key, hatchling_vault::Store::SCHEDULES)
            .map_err(|source| Error::Vault { source })
    }

    /// Stores a new schedule, unless the store is full.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure; `Ok(false)` means full.
    pub fn insert(&self, schedule: &Schedule) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, INDEX)?.unwrap_or_default();
        if index.len() >= MAX_SCHEDULES {
            return Ok(false);
        }
        write_json(&vault, &entry_name(&schedule.id), schedule)?;
        index.push(schedule.id.clone());
        write_json(&vault, INDEX, &index)?;
        Ok(true)
    }

    /// Stores a new hook, unless the store already holds [`MAX_HOOKS`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure; `Ok(false)` means full.
    pub fn insert_hook(&self, hook: &Hook) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, HOOK_INDEX)?.unwrap_or_default();
        if index.len() >= MAX_HOOKS {
            return Ok(false);
        }
        write_json(&vault, &hook_entry_name(&hook.id), hook)?;
        index.push(hook.id.clone());
        write_json(&vault, HOOK_INDEX, &index)?;
        Ok(true)
    }

    /// Removes the schedule `id`, if `by` reaches it (see
    /// [`Creator::reaches`]). Returns whether it did: `false` for one that
    /// doesn't exist and for one `by` can't touch alike, so a cancel
    /// learns nothing about schedules it can't reach.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn cancel(&self, id: &str, by: Creator) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, INDEX)?.unwrap_or_default();
        let Some(position) = index.iter().position(|stored| stored == id) else {
            return Ok(false);
        };
        let stored: Option<Schedule> = read_json(&vault, &entry_name(id))?;
        if !stored.is_some_and(|schedule| by.reaches(schedule.creator)) {
            return Ok(false);
        }
        index.remove(position);
        write_json(&vault, INDEX, &index)?;
        let _ = vault.remove(&secret_name(&entry_name(id))?);
        Ok(true)
    }

    /// Removes a hook. Returns whether it existed.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn remove_hook(&self, id: &str) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, HOOK_INDEX)?.unwrap_or_default();
        let Some(position) = index.iter().position(|stored| stored == id) else {
            return Ok(false);
        };
        index.remove(position);
        write_json(&vault, HOOK_INDEX, &index)?;
        let _ = vault.remove(&secret_name(&hook_entry_name(id))?);
        Ok(true)
    }

    /// The parked results, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn parked(&self) -> Result<Vec<Parked>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, PARKED_INDEX)?.unwrap_or_default();
        index
            .iter()
            .filter_map(|name| read_json(&vault, name).transpose())
            .collect()
    }
}

impl Store for SealedScheduleStore<'_> {
    type Error = Error;

    fn load(&self) -> Result<Vec<Schedule>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, INDEX)?.unwrap_or_default();
        index
            .iter()
            .filter_map(|id| read_json(&vault, &entry_name(id)).transpose())
            .collect()
    }

    fn update(&self, schedule: &Schedule) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, INDEX)?.unwrap_or_default();
        if !index.contains(&schedule.id) {
            return Ok(false);
        }
        write_json(&vault, &entry_name(&schedule.id), schedule)?;
        Ok(true)
    }

    fn remove(&self, id: &str) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, INDEX)?.unwrap_or_default();
        let Some(position) = index.iter().position(|stored| stored == id) else {
            return Ok(false);
        };
        index.remove(position);
        write_json(&vault, INDEX, &index)?;
        let _ = vault.remove(&secret_name(&entry_name(id))?);
        Ok(true)
    }

    fn hooks(&self) -> Result<Vec<Hook>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, HOOK_INDEX)?.unwrap_or_default();
        index
            .iter()
            .filter_map(|id| read_json(&vault, &hook_entry_name(id)).transpose())
            .collect()
    }

    fn park(&self, parked: &Parked) -> Result<(), Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, PARKED_INDEX)?.unwrap_or_default();
        while index.len() >= MAX_PARKED {
            let oldest = index.remove(0);
            tracing::warn!(parked = %oldest, "parked results full: dropping the oldest");
            let _ = vault.remove(&secret_name(&oldest)?);
        }
        let next = index
            .iter()
            .filter_map(|name| name.strip_prefix("parked-")?.parse::<u64>().ok())
            .max()
            .map_or(0, |last| last.saturating_add(1));
        let name = format!("parked-{next}");
        let mut kept = parked.clone();
        kept.text = bounded(&parked.text);
        write_json(&vault, &name, &kept)?;
        index.push(name);
        write_json(&vault, PARKED_INDEX, &index)
    }
}

/// `text`, cut to [`MAX_PARKED_TEXT_BYTES`] at a char boundary and
/// marked when it was longer.
fn bounded(text: &str) -> String {
    if text.len() <= MAX_PARKED_TEXT_BYTES {
        return text.to_owned();
    }
    let mut kept = String::with_capacity(MAX_PARKED_TEXT_BYTES);
    for c in text.chars() {
        if kept.len() + c.len_utf8() > MAX_PARKED_TEXT_BYTES {
            break;
        }
        kept.push(c);
    }
    format!("{kept}… [cut at {MAX_PARKED_TEXT_BYTES} bytes]")
}

fn entry_name(id: &str) -> String {
    format!("schedule-{id}")
}

fn hook_entry_name(id: &str) -> String {
    format!("hook-{id}")
}

fn secret_name(name: &str) -> Result<SecretName, Error> {
    name.parse().map_err(|_| Error::ScheduleStore {
        reason: format!("not a store entry name: {name:?}"),
    })
}

fn read_json<T: serde::de::DeserializeOwned>(
    vault: &SealedVault<'_>,
    name: &str,
) -> Result<Option<T>, Error> {
    match vault.fetch(&secret_name(name)?) {
        Ok(secret) => serde_json::from_slice(secret.as_bytes())
            .map(Some)
            .map_err(|e| Error::ScheduleStore {
                reason: format!("entry {name} is corrupt: {e}"),
            }),
        Err(hatchling_vault::Error::NotFound { .. }) => Ok(None),
        Err(source) => Err(Error::Vault { source }),
    }
}

fn write_json<T: serde::Serialize>(
    vault: &SealedVault<'_>,
    name: &str,
    value: &T,
) -> Result<(), Error> {
    let bytes = serde_json::to_vec(value).map_err(|e| Error::ScheduleStore {
        reason: format!("entry {name} does not encode: {e}"),
    })?;
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(Error::ScheduleStore {
            reason: format!("entry {name} is {} bytes, over the bound", bytes.len()),
        });
    }
    let secret = Secret::new(bytes).map_err(|source| Error::Vault { source })?;
    vault
        .store(&secret_name(name)?, &secret)
        .map_err(|source| Error::Vault { source })
}

/// Handles one [`ScheduleOp`] from `caller`, at `now`.
///
/// A create records `caller` as the creator; a list shows, and a cancel
/// reaches, only what `caller` reaches ([`Creator::reaches`]).
/// `Create` and `Cancel` are the authorized peer's: `Denied` when
/// scheduling is off, which it always is in a run's own session.
/// `Followup` is a run's own session's, and `Denied` in any other. A
/// typed `Refused` names a fault in the request; `ScheduleError`,
/// opaquely, a backend failure. The session survives all of them.
pub fn handle_schedule_op(
    config: &Config,
    key: &MasterKey,
    op: &ScheduleOp,
    now: Timestamp,
    caller: Creator,
) -> ScheduleResult {
    match op {
        ScheduleOp::Followup {
            task,
            delay_secs,
            grants,
        } => config
            .scheduler
            .followup
            .as_ref()
            .map_or(ScheduleResult::Denied, |scope| {
                request_followup(scope, key, task, *delay_secs, grants, now)
            }),
        ScheduleOp::List { after } => {
            list_schedules(config, key, &held_grants(config), after.as_deref(), caller)
        }
        ScheduleOp::Create { .. } | ScheduleOp::CreateFeed { .. } | ScheduleOp::Cancel { .. }
            if !config.scheduler.enabled() =>
        {
            ScheduleResult::Denied
        }
        ScheduleOp::CreateFeed {
            when,
            channel,
            feed,
        } => create_feed(config, key, when, channel, feed, now, caller),
        ScheduleOp::Create {
            when,
            task,
            grants,
            channel,
        } => create_schedule(config, key, when, task, grants, channel, now, caller),
        ScheduleOp::Cancel { id } => {
            match SealedScheduleStore::new(&config.scheduler.root, key).cancel(id, caller) {
                Ok(true) => ScheduleResult::Cancelled,
                Ok(false) => ScheduleResult::NotFound,
                Err(error) => {
                    tracing::error!(%error, "schedule store failed on cancel");
                    ScheduleResult::ScheduleError
                }
            }
        }
    }
}

/// One page of the schedules `caller`, holding `grants`, may see.
///
/// Those are the ones it reaches ([`Creator::reaches`]) whose every
/// grant it holds, in id order, after `after`. Nothing says how many others exist. A page is bounded as every
/// multi-item reply is ([`hatchling_memory::paging`]), so it always fits a
/// frame.
#[must_use]
pub fn list_schedules(
    config: &Config,
    key: &MasterKey,
    grants: &BTreeSet<ScheduleGrant>,
    after: Option<&str>,
    caller: Creator,
) -> ScheduleResult {
    if !config.scheduler.enabled() {
        return ScheduleResult::Denied;
    }
    let schedules = match SealedScheduleStore::new(&config.scheduler.root, key).load() {
        Ok(schedules) => schedules,
        Err(error) => {
            tracing::error!(%error, "schedule store failed on list");
            return ScheduleResult::ScheduleError;
        }
    };
    let mut visible: Vec<ScheduleSummary> = schedules
        .iter()
        .filter(|schedule| caller.reaches(schedule.creator))
        .filter(|schedule| schedule.grants.iter().all(|grant| grants.contains(grant)))
        .filter(|schedule| after.is_none_or(|after| schedule.id.as_str() > after))
        .map(summary)
        .collect();
    visible.sort_by(|a, b| a.id.cmp(&b.id));
    page_of(visible, hatchling_memory::paging::PAGE_BUDGET_BYTES)
}

/// The first page of `visible` within `budget_bytes`, each schedule
/// counted at its size encoded inside the reply, and the cursor that
/// resumes after it.
fn page_of(mut visible: Vec<ScheduleSummary>, budget_bytes: usize) -> ScheduleResult {
    let records = visible.iter().map(|shown| {
        serde_json::to_string(shown)
            .map(|json| (json, shown.id.clone()))
            .map_err(|_| hatchling_memory::Error::Corrupt)
    });
    match hatchling_memory::paging::page(records, budget_bytes) {
        Ok(page) => {
            visible.truncate(page.items_json.len());
            ScheduleResult::Listed {
                schedules: visible,
                next: page.next,
            }
        }
        Err(error) => {
            tracing::error!(%error, "schedule list could not be paged");
            ScheduleResult::ScheduleError
        }
    }
}

/// What a list shows of `schedule`: never its task text.
fn summary(schedule: &Schedule) -> ScheduleSummary {
    ScheduleSummary {
        id: schedule.id.clone(),
        kind: if schedule.feed.is_some() {
            ScheduleKind::Feed
        } else {
            ScheduleKind::Task
        },
        when: schedule.when.clone(),
        channel: schedule.channel.clone(),
        grants: schedule.grants.clone(),
        next_due: schedule.next_due,
    }
}

/// A schedule create, recorded as `creator`'s.
#[allow(
    clippy::too_many_arguments,
    reason = "the create's own fields, and who asked: each the request's or the session's, none derivable from another"
)]
fn create_schedule(
    config: &Config,
    key: &MasterKey,
    when: &ScheduleSpec,
    task: &str,
    grants: &[ScheduleGrant],
    channel: &str,
    now: Timestamp,
    creator: Creator,
) -> ScheduleResult {
    let Ok(id) = crate::approval::random_hex(SCHEDULE_ID_PREFIX) else {
        tracing::error!("schedule id entropy unavailable");
        return ScheduleResult::ScheduleError;
    };
    let mut schedule = match validate(
        id,
        when.clone(),
        task.to_owned(),
        grants.to_vec(),
        channel.to_owned(),
        &held_grants(config),
        now,
    ) {
        Ok(schedule) => schedule,
        Err(reason) => return ScheduleResult::Refused { reason },
    };
    schedule.creator = creator;
    match SealedScheduleStore::new(&config.scheduler.root, key).insert(&schedule) {
        Ok(true) => ScheduleResult::Created { id: schedule.id },
        Ok(false) => ScheduleResult::Refused {
            reason: ScheduleRefusal::Full,
        },
        Err(error) => {
            tracing::error!(%error, "schedule store failed on create");
            ScheduleResult::ScheduleError
        }
    }
}

/// A peer's feed create: a schedule whose grants are derived
/// from the spec, one read grant per kind of section and sending on the
/// delivery channel, so a feed can never hold a write. They must be a
/// subset of the creator's, as for any schedule.
fn create_feed(
    config: &Config,
    key: &MasterKey,
    when: &ScheduleSpec,
    channel: &str,
    feed: &FeedSpec,
    now: Timestamp,
    creator: Creator,
) -> ScheduleResult {
    let Some(spec) = hatchling_feed::Spec::parse(feed) else {
        return ScheduleResult::Refused {
            reason: ScheduleRefusal::BadTask,
        };
    };
    let Ok(id) = crate::approval::random_hex(SCHEDULE_ID_PREFIX) else {
        tracing::error!("schedule id entropy unavailable");
        return ScheduleResult::ScheduleError;
    };
    let mut grants = spec.grants();
    grants.push(ScheduleGrant::ChannelSend {
        channel: channel.to_owned(),
    });
    let mut schedule = match validate(
        id,
        when.clone(),
        String::from(crate::feed_run::FEED_TASK),
        grants,
        channel.to_owned(),
        &held_grants(config),
        now,
    ) {
        Ok(schedule) => schedule,
        Err(reason) => return ScheduleResult::Refused { reason },
    };
    schedule.feed = Some(feed.clone());
    schedule.creator = creator;
    match SealedScheduleStore::new(&config.scheduler.root, key).insert(&schedule) {
        Ok(true) => ScheduleResult::Created { id: schedule.id },
        Ok(false) => ScheduleResult::Refused {
            reason: ScheduleRefusal::Full,
        },
        Err(error) => {
            tracing::error!(%error, "schedule store failed on a feed create");
            ScheduleResult::ScheduleError
        }
    }
}

/// A run's one follow-up: validated against the run's own grants and
/// depth (never the creator's), claimed once per run, and stored as a
/// one-shot schedule that the dispatcher fires like any other.
fn request_followup(
    scope: &FollowupScope,
    key: &MasterKey,
    task: &str,
    delay_secs: u64,
    grants: &[ScheduleGrant],
    now: Timestamp,
) -> ScheduleResult {
    let Ok(id) = crate::approval::random_hex(SCHEDULE_ID_PREFIX) else {
        tracing::error!("schedule id entropy unavailable");
        return ScheduleResult::ScheduleError;
    };
    let parent = FollowupParent {
        grants: &scope.grants,
        channel: &scope.channel,
        depth: scope.depth,
    };
    let schedule = match validate_followup(
        id,
        &parent,
        task.to_owned(),
        delay_secs,
        grants.to_vec(),
        now,
    ) {
        Ok(schedule) => schedule,
        Err(reason) => {
            tracing::warn!(run = %scope.run, ?reason, "follow-up refused");
            return ScheduleResult::Refused { reason };
        }
    };
    if !claim_followup(&scope.run) {
        return ScheduleResult::Refused {
            reason: ScheduleRefusal::FollowupTaken,
        };
    }
    let stored = SealedScheduleStore::new(&scope.root, key).insert(&schedule);
    if !matches!(stored, Ok(true)) {
        release_followup(&scope.run);
    }
    match stored {
        Ok(true) => {
            tracing::info!(run = %scope.run, followup = %schedule.id, depth = schedule.depth, "follow-up scheduled");
            ScheduleResult::Created { id: schedule.id }
        }
        Ok(false) => ScheduleResult::Refused {
            reason: ScheduleRefusal::Full,
        },
        Err(error) => {
            tracing::error!(%error, "schedule store failed on a follow-up");
            ScheduleResult::ScheduleError
        }
    }
}

/// Handles one [`HookOp`] from the authorized peer.
///
/// `Denied` when scheduling is off, which it always is in a run's own
/// session: a run can neither register nor cancel a hook. A typed
/// `Refused` names a fault in the request, including a source the broker
/// cannot authenticate; `HookError`, opaquely, a backend failure.
pub fn handle_hook_op(config: &Config, key: &MasterKey, op: &HookOp) -> HookResult {
    if !config.scheduler.enabled() {
        return HookResult::Denied;
    }
    let store = SealedScheduleStore::new(&config.scheduler.root, key);
    match op {
        HookOp::Create {
            source,
            task,
            grants,
            channel,
        } => {
            let Ok(id) = crate::approval::random_hex(HOOK_ID_PREFIX) else {
                tracing::error!("hook id entropy unavailable");
                return HookResult::HookError;
            };
            let schedules: BTreeSet<String> = match store.load() {
                Ok(schedules) => schedules.into_iter().map(|schedule| schedule.id).collect(),
                Err(error) => {
                    tracing::error!(%error, "schedule store failed on a hook create");
                    return HookResult::HookError;
                }
            };
            let channels = authenticated_channels(config);
            let hook = match validate_hook(
                id,
                source.clone(),
                task.clone(),
                grants.clone(),
                channel.clone(),
                &held_grants(config),
                &Sources {
                    channels: &channels,
                    schedules: &schedules,
                },
            ) {
                Ok(hook) => hook,
                Err(reason) => return HookResult::Refused { reason },
            };
            match store.insert_hook(&hook) {
                Ok(true) => HookResult::Created { id: hook.id },
                Ok(false) => HookResult::Refused {
                    reason: ScheduleRefusal::Full,
                },
                Err(error) => {
                    tracing::error!(%error, "schedule store failed on a hook create");
                    HookResult::HookError
                }
            }
        }
        HookOp::Cancel { id } => match store.remove_hook(id) {
            Ok(true) => HookResult::Cancelled,
            Ok(false) => HookResult::NotFound,
            Err(error) => {
                tracing::error!(%error, "schedule store failed on a hook cancel");
                HookResult::HookError
            }
        },
    }
}

/// Delivers scheduled output: to the local channel's ingress client,
/// sent as the broker's scheduler. Other channels have no delivery path
/// yet, so delivering to one fails, and the dispatcher parks.
#[derive(Debug, Clone, Copy)]
pub struct HubDelivery<'a> {
    hub: Option<&'a LocalHub>,
}

impl<'a> HubDelivery<'a> {
    /// Delivery through `hub`, the broker's ingress, if it has one.
    #[must_use]
    pub const fn new(hub: Option<&'a LocalHub>) -> Self {
        Self { hub }
    }
}

/// The sender stamp on scheduled output: the broker's scheduler, never
/// a peer.
pub const SCHEDULER_SENDER: &str = "broker:scheduler";

impl Delivery for HubDelivery<'_> {
    fn deliver(&self, channel: &str, text: &str) -> Result<(), String> {
        self.deliver_verbatim(channel, text).map(|_| ())
    }

    fn deliver_verbatim(&self, channel: &str, text: &str) -> Result<bool, String> {
        if channel != "local" {
            return Err(format!("no delivery path for channel {channel:?}"));
        }
        let hub = self
            .hub
            .ok_or_else(|| String::from("the local channel has no ingress"))?;
        let delivered = hub.deliver(
            &Message {
                sender: SenderId(String::from(SCHEDULER_SENDER)),
                channel: ChannelId::local(),
                text: text.to_owned(),
            },
            crate::output_gate::SCHEDULER_SCOPE,
        );
        match delivered {
            // The gate's notice reached the person in its place:
            // that is a delivery, and the dispatcher must not retry or park
            // the flagged output.
            Err(hatchling_channel::Error::Redacted { category, event_id }) => {
                tracing::warn!(%category, %event_id, "scheduled output was redacted");
                Ok(false)
            }
            other => other.map(|()| true).map_err(|error| error.to_string()),
        }
    }
}

/// One dispatcher tick at `now` against the broker's real store,
/// grants, and delivery path, with `launcher` starting the runs.
///
/// The tick's outside events come from `hub` alone: one
/// `channel-message` on the local channel if any message arrived since
/// the last tick. Only the ingress queues into the hub, after its kernel
/// peer gate, so an arrival is an authenticated one.
///
/// # Errors
///
/// Returns [`Error`] if the store cannot be loaded.
pub fn dispatch_once<L: Launcher>(
    dispatcher: &mut Dispatcher<L::Run>,
    config: &Config,
    key: &MasterKey,
    hub: Option<&LocalHub>,
    launcher: &L,
    now: Timestamp,
) -> Result<TickReport, Error> {
    if !config.scheduler.enabled() {
        return Ok(TickReport::default());
    }
    let store = SealedScheduleStore::new(&config.scheduler.root, key);
    let held = held_grants(config);
    let delivery = HubDelivery::new(hub);
    let authenticated = hub.map_or_else(BTreeSet::new, |_| authenticated_channels(config));
    let events = if hub.is_some_and(|hub| hub.take_arrivals() > 0) {
        vec![HookSource::ChannelMessage {
            channel: ChannelId::local().0,
        }]
    } else {
        Vec::new()
    };
    dispatcher.tick(
        now,
        &Services {
            store: &store,
            launcher,
            delivery: &delivery,
            held: &held,
            events: &events,
            authenticated: &authenticated,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Flags any message that says "hunter2".
    struct Flags;

    impl hatchling_channel::Classifier for Flags {
        fn id(&self) -> &'static str {
            "flags@test"
        }

        fn classify(
            &self,
            request: &hatchling_channel::Request<'_>,
        ) -> Result<hatchling_channel::Verdict, String> {
            Ok(if request.text.contains("hunter2") {
                hatchling_channel::Verdict::Redact {
                    category: hatchling_channel::Category::new("secret")
                        .ok_or_else(|| String::from("a category"))?,
                    flagged: Some(String::from("hunter2")),
                }
            } else {
                hatchling_channel::Verdict::Allow
            })
        }
    }

    /// WO-026: the hub's delivery says the person was shown an answer as
    /// written only when the gate let it through; its notice in the
    /// answer's place is still a delivery, but not that answer.
    #[test]
    fn a_delivery_says_whether_the_person_saw_the_answer_itself() {
        let hub = LocalHub::new(hatchling_channel::OutputGate::new(
            std::sync::Arc::new(Flags),
            std::sync::Arc::new(hatchling_channel::NoAudit),
            std::sync::Arc::new(hatchling_channel::NoOverrides),
            std::time::Duration::from_secs(2),
        ));
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let sink_seen = std::sync::Arc::clone(&seen);
        let _ = hub.attach(Box::new(move |cleared: &hatchling_channel::Cleared| {
            if let Ok(mut seen) = sink_seen.lock() {
                seen.push(cleared.message().text.clone());
            }
            Ok(())
        }));
        let delivery = HubDelivery::new(Some(&hub));
        assert_eq!(delivery.deliver_verbatim("local", "Coffee."), Ok(true));
        assert_eq!(
            delivery.deliver_verbatim("local", "the key is hunter2"),
            Ok(false)
        );
        assert_eq!(
            delivery.deliver("local", "the key is hunter2 again"),
            Ok(())
        );
        assert!(delivery.deliver_verbatim("elsewhere", "Coffee.").is_err());
        assert!(
            HubDelivery::new(None)
                .deliver_verbatim("local", "Coffee.")
                .is_err()
        );
        let seen = seen.lock().map(|seen| seen.clone()).unwrap_or_default();
        assert_eq!(seen.first().map(String::as_str), Some("Coffee."));
        assert!(
            !seen.iter().any(|text| text.contains("hunter2")),
            "{seen:?}"
        );
    }

    #[test]
    fn a_run_claims_one_followup_until_it_is_released() {
        let run = format!("run-claim-{}", std::process::id());
        assert!(claim_followup(&run));
        assert!(!claim_followup(&run), "one per run");
        assert!(
            claim_followup(&format!("{run}-other")),
            "per run, not global"
        );
        release_followup(&run);
        assert!(claim_followup(&run), "released when the run ends");
        release_followup(&run);
        release_followup(&format!("{run}-other"));
    }

    /// A config with only `channels` and (or not) the ingress set.
    fn channel_config(channels: &[&str], ingress: bool) -> Config {
        let vault_root = PathBuf::from("/nonexistent/hatchling-vault");
        Config {
            socket: "/tmp/hatchling-test-schedule-ops.sock".parse().unwrap(),
            agent_uid: hatchling_core::PeerUid::from_kernel(1000),
            socket_mode: 0o600,
            max_connections: 1,
            scheduler: crate::SchedulerConfig::disabled(),
            log_level: hatchling_core::LogLevel::Error,
            vault: crate::VaultConfig {
                key_file: crate::VaultConfig::default_key_file(&vault_root),
                root: vault_root,
                grants: Vec::new(),
            },
            egress: crate::EgressConfig {
                allowlist: crate::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: channels.iter().map(|c| (*c).to_owned()).collect(),
            channel_accounts: Vec::new(),
            channel_ingress: ingress.then(|| "/tmp/hatchling-test-ingress.sock".parse().unwrap()),
            skills: crate::SkillConfig::disabled(),
            memory: crate::MemoryConfig {
                root: PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: crate::ArtifactConfig::disabled(),
            spaces: crate::SpaceConfig::disabled(),
            goals: crate::GoalsConfig::disabled(),
        }
    }

    #[test]
    fn a_summary_shows_a_schedule_without_its_task_and_no_scheduler_lists_nothing() {
        let schedule = Schedule {
            id: String::from("sch-1"),
            when: ScheduleSpec::Cron {
                expr: String::from("0 8 * * *"),
                tz: String::from("UTC"),
            },
            task: String::from("the task text"),
            grants: vec![ScheduleGrant::ChannelSend {
                channel: String::from("local"),
            }],
            channel: String::from("local"),
            next_due: 42,
            depth: 0,
            space: None,
            feed: None,
            creator: Creator::Agent,
        };
        let shown = summary(&schedule);
        assert_eq!(
            shown,
            ScheduleSummary {
                id: String::from("sch-1"),
                kind: ScheduleKind::Task,
                when: schedule.when.clone(),
                channel: String::from("local"),
                grants: schedule.grants.clone(),
                next_due: 42,
            }
        );
        assert!(!serde_json::to_string(&shown).unwrap().contains("task text"));
        let feed = Schedule {
            feed: Some(FeedSpec {
                goals: None,
                tracking: true,
                tracking_source: None,
                memory_tags: Vec::new(),
                lookback_hours: 24,
                max_items: 20,
            }),
            ..schedule
        };
        assert_eq!(summary(&feed).kind, ScheduleKind::Feed);
        // A schedule stored before creators were recorded is the owner's.
        let mut stored = serde_json::to_value(&feed).unwrap();
        stored.as_object_mut().unwrap().remove("creator");
        let old: Schedule = serde_json::from_value(stored).unwrap();
        assert_eq!(old.creator, Creator::Owner);
        let key = MasterKey::generate().unwrap();
        assert_eq!(
            list_schedules(
                &channel_config(&["local"], false),
                &key,
                &BTreeSet::new(),
                None,
                Creator::Agent
            ),
            ScheduleResult::Denied
        );
    }

    #[test]
    fn a_list_page_holds_what_its_budget_allows_and_says_where_to_resume() {
        let shown = |id: &str| ScheduleSummary {
            id: id.to_owned(),
            kind: ScheduleKind::Task,
            when: ScheduleSpec::Cron {
                expr: String::from("0 * * * *"),
                tz: String::from("UTC"),
            },
            channel: String::from("local"),
            grants: Vec::new(),
            next_due: 0,
        };
        let cost = |summary: &ScheduleSummary| {
            hatchling_memory::reply_encoded_len(serde_json::to_string(summary).unwrap().as_bytes())
                .unwrap()
                + 1
        };
        let both = vec![shown("a"), shown("b")];
        let whole = cost(&shown("a")) + cost(&shown("b"));
        assert_eq!(
            page_of(both.clone(), whole),
            ScheduleResult::Listed {
                schedules: both.clone(),
                next: None
            }
        );
        assert_eq!(
            page_of(both, whole - 1),
            ScheduleResult::Listed {
                schedules: vec![shown("a")],
                next: Some(String::from("a"))
            }
        );
        assert_eq!(
            page_of(Vec::new(), 0),
            ScheduleResult::Listed {
                schedules: Vec::new(),
                next: None
            }
        );
    }

    #[test]
    fn only_the_local_channel_with_an_ingress_is_authenticated() {
        let local = BTreeSet::from([String::from("local")]);
        assert_eq!(
            authenticated_channels(&channel_config(&["local"], true)),
            local
        );
        assert_eq!(
            authenticated_channels(&channel_config(&["local", "mail"], true)),
            local,
            "no other channel has an authenticated ingress"
        );
        assert!(authenticated_channels(&channel_config(&["local"], false)).is_empty());
        assert!(authenticated_channels(&channel_config(&["mail"], true)).is_empty());
    }

    #[test]
    fn a_schedule_is_created_as_its_callers_and_cancelled_only_by_who_reaches_it() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-schedule-creator-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let mut config = channel_config(&["local"], false);
        config.scheduler.root.clone_from(&root);
        let when = ScheduleSpec::Cron {
            expr: String::from("0 8 * * *"),
            tz: String::from("UTC"),
        };
        let grants = [ScheduleGrant::ChannelSend {
            channel: String::from("local"),
        }];
        let now = Timestamp::now();
        let create = |creator| {
            let result = create_schedule(&config, &key, &when, "t", &grants, "local", now, creator);
            if let ScheduleResult::Created { id } = result {
                id
            } else {
                format!("not created: {result:?}")
            }
        };
        let owners = create(Creator::Owner);
        let agents = create(Creator::Agent);
        let store = SealedScheduleStore::new(&root, &key);
        let creators: Vec<(String, Creator)> = store
            .load()
            .unwrap()
            .into_iter()
            .map(|schedule| (schedule.id, schedule.creator))
            .collect();
        assert!(
            creators.contains(&(owners.clone(), Creator::Owner)),
            "{creators:?}"
        );
        assert!(
            creators.contains(&(agents.clone(), Creator::Agent)),
            "{creators:?}"
        );
        // The agent can't cancel the owner's, nor an id that doesn't exist.
        assert!(!store.cancel(&owners, Creator::Agent).unwrap());
        assert!(!store.cancel("sch-none", Creator::Owner).unwrap());
        assert_eq!(store.load().unwrap().len(), 2);
        // It cancels its own, once; the owner cancels any.
        assert!(store.cancel(&agents, Creator::Agent).unwrap());
        assert!(!store.cancel(&agents, Creator::Agent).unwrap());
        assert!(store.cancel(&owners, Creator::Owner).unwrap());
        assert!(store.load().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_hook_store_is_bounded_and_kept_apart_from_schedules() {
        let root =
            std::env::temp_dir().join(format!("hatchling-test-hook-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let store = SealedScheduleStore::new(&root, &key);
        let hook = |n: usize| Hook {
            id: format!("{HOOK_ID_PREFIX}{n}"),
            source: HookSource::ChannelMessage {
                channel: String::from("local"),
            },
            task: String::from("t"),
            grants: Vec::new(),
            channel: String::from("local"),
        };
        for n in 0..MAX_HOOKS {
            assert!(store.insert_hook(&hook(n)).unwrap(), "{n}");
        }
        assert!(!store.insert_hook(&hook(MAX_HOOKS)).unwrap(), "full");
        assert_eq!(store.hooks().unwrap().len(), MAX_HOOKS);
        assert!(store.remove_hook(&hook(0).id).unwrap());
        assert!(!store.remove_hook(&hook(0).id).unwrap());
        assert!(store.insert_hook(&hook(MAX_HOOKS)).unwrap(), "room again");
        assert_eq!(store.hooks().unwrap().len(), MAX_HOOKS);
        // Hooks and schedules are kept apart.
        assert!(store.load().unwrap().is_empty());
        assert!(!store.remove(&hook(1).id).unwrap());
        assert_eq!(store.hooks().unwrap().len(), MAX_HOOKS);
        assert_eq!(hook_entry_name("hk-1"), "hook-hk-1");
        assert!(secret_name(&hook_entry_name("hk-1")).is_ok());
        assert_eq!((MAX_HOOKS, HOOK_ID_PREFIX), (64, "hk-"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parked_text_is_bounded_at_a_char_boundary_and_marked() {
        let short = "x".repeat(MAX_PARKED_TEXT_BYTES);
        assert_eq!(bounded(&short), short);
        let long = "é".repeat(MAX_PARKED_TEXT_BYTES);
        let cut = bounded(&long);
        assert!(
            cut.ends_with(&format!("[cut at {MAX_PARKED_TEXT_BYTES} bytes]")),
            "{cut}"
        );
        let kept = cut.split('…').next().unwrap_or_default();
        assert_eq!(
            kept.len(),
            MAX_PARKED_TEXT_BYTES,
            "é is 2 bytes: an even cut"
        );
        let odd = format!("x{}", "é".repeat(MAX_PARKED_TEXT_BYTES));
        let kept_odd = bounded(&odd);
        let kept_odd = kept_odd.split('…').next().unwrap_or_default();
        assert_eq!(
            kept_odd.len(),
            MAX_PARKED_TEXT_BYTES - 1,
            "backs off to a boundary"
        );
        assert_eq!(
            (MAX_SCHEDULES, MAX_PARKED, MAX_PARKED_TEXT_BYTES),
            (64, 64, 16 * 1024)
        );
    }

    #[test]
    fn an_entry_at_the_vault_bound_is_stored_and_one_byte_over_is_refused() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-schedule-entry-bound-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let vault = SealedVault::open_in(&root, &key, hatchling_vault::Store::SCHEDULES).unwrap();
        // A JSON string is its text plus two quotes.
        let at_bound = "x".repeat(MAX_SECRET_BYTES - 2);
        assert!(write_json(&vault, "probe", &at_bound).is_ok());
        let read: Option<String> = read_json(&vault, "probe").unwrap();
        assert_eq!(read.map(|s| s.len()), Some(MAX_SECRET_BYTES - 2));
        let over = "x".repeat(MAX_SECRET_BYTES - 1);
        let error = write_json(&vault, "probe", &over).unwrap_err();
        assert!(
            matches!(&error, Error::ScheduleStore { reason } if reason.contains("over the bound")),
            "{error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn entry_names_are_vault_names() {
        assert_eq!(entry_name("sch-ab12"), "schedule-sch-ab12");
        assert!(secret_name(&entry_name("sch-ab12")).is_ok());
        assert!(secret_name(&entry_name("../etc")).is_err());
        assert!(secret_name("parked-3").is_ok());
    }

    #[test]
    fn the_store_seals_under_its_own_label() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-schedules-label-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let name: SecretName = "probe".parse().unwrap();
        let secret = Secret::new(b"p".to_vec()).unwrap();
        SealedScheduleStore::new(&root, &key)
            .vault()
            .unwrap()
            .store(&name, &secret)
            .unwrap();
        let own = SealedVault::open_in(&root, &key, hatchling_vault::Store::SCHEDULES)
            .unwrap()
            .fetch(&name);
        let secrets = SealedVault::open(&root, &key).unwrap().fetch(&name);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(own.unwrap().as_bytes(), b"p");
        assert!(secrets.is_err(), "opened under the secrets label");
    }
}
