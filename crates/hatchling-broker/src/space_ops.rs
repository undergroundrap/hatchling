//! The broker's space ops: broker-asserted membership,
//! admission factors, a broker-owned store, and space tasks.
//!
//! **Who is asking is the broker's to say.** A session's principal comes
//! from the session itself ([`SpaceSession::for_session`]): the agent's
//! session is its peer's kernel uid; a space task's run session is that
//! task run, bound by [`crate::scoped_config`] to its one space. Nothing
//! the cell sends names a principal.
//!
//! **Every op on a space is re-authorized** : the ticket
//! is checked factor by factor ([`hatchling_space::admission`]), including
//! that the space the request names is byte-for-byte the one the ticket
//! admits, and then the space is re-read and the principal's membership
//! re-checked. Naming a space the caller is not a member of gets
//! `no-such-space`, the same as naming one that does not exist.
//!
//! **The store is sealed and broker-private,** like the schedule and build
//! stores: a [`SealedVault`] at `HATCHLING_SPACE_DIR`. Spaces are
//! `space-<id>` entries with an index, at most [`MAX_SPACES`]; each item
//! is its own entry (`item-<space>-<key>`), and so is each task
//! (`task-<id>`). There is no op that lists or exports any of it, and no
//! store method that does.
//!
//! **A space task runs as a one-shot schedule** bound to its space, so it
//! goes through the dispatcher like any run: grants narrowed to those the
//! creator still holds, and a run session whose only space is its own.
//!
//! **Sync, pairing, and media ** are space ops too, so every one
//! of them goes through the same admission and membership checks:
//!
//! - A **pull** returns only the named space's items changed after a
//!   version, paged to fit a frame; a **push** writes only into it,
//!   last-writer-wins, and each write it overwrote unseen goes to the
//!   space's **conflict log** (`conflict-<space>-<seq>`, the lost value
//!   beside it), read back with the conflicts op.
//! - **Pairing** issues a single-use, short-lived code for one space; only
//!   the device endpoint ([`crate::device`]) redeems it.
//! - **Media** is admitted only after the broker has bounded it and typed
//!   it from its own bytes (`media-<id>`, its record `mediameta-<id>`), and
//!   is served with the broker's headers.
//!
//! A device's session is [`Principal::Device`]: like a task run it may use
//! only spaces it was paired into, and may not create spaces or tasks, run
//! tasks, or pair other devices.

use std::sync::{Mutex, PoisonError};

use base64::Engine as _;
use hatchling_core::{
    ConflictEntry, MAX_SECRET_BYTES, PeerCredentials, ScheduleGrant, SecretName, SpaceOp,
    SpaceRefusal, SpaceResult, SyncChange, SyncItem,
};
use hatchling_memory::SessionId;
use hatchling_scheduler::Timestamp;
use hatchling_space::{
    Admissions, DeviceRecord, PAGE_BYTES, PeerPin, Principal, SessionPin, SpaceRecord, SpaceTask,
    check_key, check_push, check_value, overwritten, page, served_headers,
};
use hatchling_vault::{MasterKey, SealedVault, Secret};

use crate::schedule_ops::{SCHEDULE_ID_PREFIX, SealedScheduleStore, held_grants};
use crate::{Config, Error};

/// Most spaces the store keeps.
pub const MAX_SPACES: usize = 64;

/// Every space id starts with this.
pub const SPACE_ID_PREFIX: &str = "spc-";

/// Every admission ticket starts with this.
pub const TICKET_PREFIX: &str = "adm-";

/// Every task id starts with this.
pub const TASK_ID_PREFIX: &str = "tsk-";

/// Every media id starts with this.
pub const MEDIA_ID_PREFIX: &str = "med-";

/// Every pairing code starts with this.
pub const PAIRING_CODE_PREFIX: &str = "pair-";

/// Most paired devices the broker keeps.
pub const MAX_DEVICES: usize = 16;

const SPACE_INDEX: &str = "space-index";
const DEVICE_INDEX: &str = "device-index";
const BROKER_DEVICE_KEY: &str = "broker-device-key";

/// Serializes every store change in the process.
static STORE: Mutex<()> = Mutex::new(());

/// Every live admission, across sessions: a ticket replayed in another
/// session is found here, and refused as stale.
static ADMISSIONS: Mutex<Admissions> = Mutex::new(Admissions::empty());

fn admissions() -> std::sync::MutexGuard<'static, Admissions> {
    ADMISSIONS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every pairing code the broker has issued, and its log of refusals
/// . Process-wide: the agent's session issues, the device
/// endpoint redeems.
static PAIRINGS: Mutex<hatchling_space::Pairings> = Mutex::new(hatchling_space::Pairings::empty());

pub(crate) fn pairings() -> std::sync::MutexGuard<'static, hatchling_space::Pairings> {
    PAIRINGS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One session, as the space ops see it: its broker-asserted principal,
/// and the pin its admissions are bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceSession {
    /// Who is asking.
    pub principal: Principal,
    /// The session id and its kernel-attested peer.
    pub pin: SessionPin,
}

impl SpaceSession {
    /// The space session for a broker session with `config`, the peer
    /// `creds`, and the broker-minted `session` id.
    ///
    /// A session whose config is bound to a space (a space task's run) is
    /// that task run; any other is the agent, by its peer's uid.
    #[must_use]
    pub fn for_session(config: &Config, creds: &PeerCredentials, session: &SessionId) -> Self {
        let uid = creds.uid().as_u32();
        Self {
            principal: config
                .spaces
                .bound
                .clone()
                .map_or(Principal::Agent { uid }, |space| Principal::TaskRun {
                    space,
                }),
            pin: SessionPin {
                session: session.as_str().to_owned(),
                peer: PeerPin {
                    uid,
                    pid: creds.pid().as_u32(),
                },
            },
        }
    }
}

/// Ends a session's admissions when dropped: its tickets die with it,
/// however it ends.
#[derive(Debug)]
pub struct EndAdmissions(pub String);

impl Drop for EndAdmissions {
    fn drop(&mut self) {
        admissions().end_session(&self.0);
    }
}

/// The sealed space store at `root`.
#[derive(Debug)]
pub struct SealedSpaceStore<'k> {
    root: &'k std::path::Path,
    key: &'k MasterKey,
}

impl<'k> SealedSpaceStore<'k> {
    /// The store at `root`, sealed under `key`. Nothing is opened until an
    /// operation needs it.
    #[must_use]
    pub const fn new(root: &'k std::path::Path, key: &'k MasterKey) -> Self {
        Self { root, key }
    }

    fn vault(&self) -> Result<SealedVault<'k>, Error> {
        SealedVault::open_in(self.root, self.key, hatchling_vault::Store::SPACES)
            .map_err(|source| Error::Vault { source })
    }

    /// Stores a new space, unless the store is full.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure; `Ok(false)` means full.
    pub fn insert(&self, record: &SpaceRecord) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, SPACE_INDEX)?.unwrap_or_default();
        if index.len() >= MAX_SPACES {
            return Ok(false);
        }
        write_json(&vault, &space_entry(&record.id), record)?;
        index.push(record.id.clone());
        write_json(&vault, SPACE_INDEX, &index)?;
        Ok(true)
    }

    /// Replaces a stored space, if it exists. Returns whether it did.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn put(&self, record: &SpaceRecord) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, SPACE_INDEX)?.unwrap_or_default();
        if !index.contains(&record.id) {
            return Ok(false);
        }
        write_json(&vault, &space_entry(&record.id), record)?;
        Ok(true)
    }

    /// The space `id`, if it exists.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn get(&self, id: &str) -> Result<Option<SpaceRecord>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, SPACE_INDEX)?.unwrap_or_default();
        if !index.iter().any(|stored| stored == id) {
            return Ok(None);
        }
        read_json(&vault, &space_entry(id))
    }

    /// Stores `task`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn put_task(&self, task: &SpaceTask) -> Result<(), Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        write_json(&self.vault()?, &task_entry(&task.id), task)
    }

    /// The task `id`, if it is stored.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn get_task(&self, id: &str) -> Result<Option<SpaceTask>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        read_json(&self.vault()?, &task_entry(id))
    }

    /// Stores `value` as the item `key` of `space`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn put_item(&self, space: &str, key: &str, value: &str) -> Result<(), Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        write_raw(&self.vault()?, &item_entry(space, key), value.as_bytes())
    }

    /// The item `key` of `space`, if it is stored.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure, or for a value that is not
    /// text.
    pub fn get_item(&self, space: &str, key: &str) -> Result<Option<String>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        read_text(&self.vault()?, &item_entry(space, key))
    }

    /// Removes the item `key` of `space`, if it is stored.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn remove_item(&self, space: &str, key: &str) -> Result<(), Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        remove_entry(&self.vault()?, &item_entry(space, key))
    }

    /// Stores `entry` in `space`'s conflict log: its record, and the lost
    /// value beside it as raw bytes, so any value fits.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn put_conflict(&self, space: &str, entry: &ConflictEntry) -> Result<(), Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let record = ConflictEntry {
            lost: None,
            ..entry.clone()
        };
        write_json(&vault, &conflict_entry(space, entry.seq), &record)?;
        let lost = conflict_value_entry(space, entry.seq);
        entry.lost.as_ref().map_or_else(
            || remove_entry(&vault, &lost),
            |value| write_raw(&vault, &lost, value.as_bytes()),
        )
    }

    /// The conflict `seq` of `space`, if it is stored.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn get_conflict(&self, space: &str, seq: u64) -> Result<Option<ConflictEntry>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let Some(mut entry) = read_json::<ConflictEntry>(&vault, &conflict_entry(space, seq))?
        else {
            return Ok(None);
        };
        entry.lost = read_text(&vault, &conflict_value_entry(space, seq))?;
        Ok(Some(entry))
    }

    /// Removes the conflict `seq` of `space`, if it is stored.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn remove_conflict(&self, space: &str, seq: u64) -> Result<(), Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        remove_entry(&vault, &conflict_entry(space, seq))?;
        remove_entry(&vault, &conflict_value_entry(space, seq))
    }

    /// Stores the media `meta` describes, with its `bytes`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn put_media(&self, meta: &MediaMeta, bytes: &[u8]) -> Result<(), Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        write_raw(&vault, &media_entry(&meta.id), bytes)?;
        write_json(&vault, &media_meta_entry(&meta.id), meta)
    }

    /// The media `id` and its bytes, if it is stored.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn get_media(&self, id: &str) -> Result<Option<(MediaMeta, Vec<u8>)>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let Some(meta) = read_json::<MediaMeta>(&vault, &media_meta_entry(id))? else {
            return Ok(None);
        };
        match vault.fetch(&entry_secret_name(&media_entry(id))?) {
            Ok(secret) => Ok(Some((meta, secret.as_bytes().to_vec()))),
            Err(hatchling_vault::Error::NotFound { .. }) => Ok(None),
            Err(source) => Err(Error::Vault { source }),
        }
    }

    /// Stores `device`, new or not, unless it is new and the store already
    /// holds [`MAX_DEVICES`]. Returns whether it did.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn put_device(&self, device: &DeviceRecord) -> Result<bool, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let mut index: Vec<String> = read_json(&vault, DEVICE_INDEX)?.unwrap_or_default();
        if !index.contains(&device.id) {
            if index.len() >= MAX_DEVICES {
                return Ok(false);
            }
            index.push(device.id.clone());
            write_json(&vault, DEVICE_INDEX, &index)?;
        }
        write_json(&vault, &device_entry(&device.id), device)?;
        Ok(true)
    }

    /// The paired device `id`, if there is one.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure.
    pub fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let index: Vec<String> = read_json(&vault, DEVICE_INDEX)?.unwrap_or_default();
        if !index.iter().any(|stored| stored == id) {
            return Ok(None);
        }
        read_json(&vault, &device_entry(id))
    }

    /// The broker's device signing key: sealed in the store, made on
    /// first use. Devices pin its public half at pairing.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on a backend failure, or if no entropy is
    /// available for a new key.
    pub fn broker_key(&self) -> Result<ed25519_dalek::SigningKey, Error> {
        let _serial = STORE.lock().unwrap_or_else(PoisonError::into_inner);
        let vault = self.vault()?;
        let name = entry_secret_name(BROKER_DEVICE_KEY)?;
        // The seed is the signing key: wiped when dropped, like the key.
        let seed: zeroize::Zeroizing<[u8; 32]> = match vault.fetch(&name) {
            Ok(secret) => zeroize::Zeroizing::new(secret.as_bytes().try_into().map_err(|_| {
                Error::SpaceStore {
                    reason: String::from("the broker device key is corrupt"),
                }
            })?),
            Err(hatchling_vault::Error::NotFound { .. }) => {
                let mut seed = zeroize::Zeroizing::new([0u8; 32]);
                std::fs::File::open("/dev/urandom")
                    .and_then(|mut rng| std::io::Read::read_exact(&mut rng, seed.as_mut_slice()))
                    .map_err(|source| Error::SpaceStore {
                        reason: format!("no entropy for the broker device key: {source}"),
                    })?;
                write_raw(&vault, BROKER_DEVICE_KEY, seed.as_slice())?;
                seed
            }
            Err(source) => return Err(Error::Vault { source }),
        };
        Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
    }
}

/// What the store keeps about one piece of media.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MediaMeta {
    /// The media id.
    pub id: String,
    /// The space it belongs to.
    pub space: String,
    /// The content type the broker assigned from its bytes.
    pub content_type: String,
    /// Its size in bytes.
    pub bytes: u32,
    /// Who uploaded it ([`Principal::label`]).
    pub uploader: String,
    /// When, in Unix seconds.
    pub at: i64,
}

fn space_entry(id: &str) -> String {
    format!("space-{id}")
}

fn task_entry(id: &str) -> String {
    format!("task-{id}")
}

fn item_entry(space: &str, key: &str) -> String {
    format!("item-{space}-{key}")
}

fn conflict_entry(space: &str, seq: u64) -> String {
    format!("conflict-{space}-{seq}")
}

fn conflict_value_entry(space: &str, seq: u64) -> String {
    format!("conflictv-{space}-{seq}")
}

fn media_entry(id: &str) -> String {
    format!("media-{id}")
}

fn media_meta_entry(id: &str) -> String {
    format!("mediameta-{id}")
}

fn device_entry(id: &str) -> String {
    format!("device-{id}")
}

fn write_raw(vault: &SealedVault<'_>, name: &str, bytes: &[u8]) -> Result<(), Error> {
    let secret = Secret::new(bytes.to_vec()).map_err(|source| Error::Vault { source })?;
    vault
        .store(&entry_secret_name(name)?, &secret)
        .map_err(|source| Error::Vault { source })
}

fn read_text(vault: &SealedVault<'_>, name: &str) -> Result<Option<String>, Error> {
    match vault.fetch(&entry_secret_name(name)?) {
        Ok(secret) => String::from_utf8(secret.as_bytes().to_vec())
            .map(Some)
            .map_err(|_| Error::SpaceStore {
                reason: format!("entry {name} is not text"),
            }),
        Err(hatchling_vault::Error::NotFound { .. }) => Ok(None),
        Err(source) => Err(Error::Vault { source }),
    }
}

fn remove_entry(vault: &SealedVault<'_>, name: &str) -> Result<(), Error> {
    match vault.remove(&entry_secret_name(name)?) {
        Ok(()) | Err(hatchling_vault::Error::NotFound { .. }) => Ok(()),
        Err(source) => Err(Error::Vault { source }),
    }
}

fn entry_secret_name(name: &str) -> Result<SecretName, Error> {
    name.parse().map_err(|_| Error::SpaceStore {
        reason: format!("not a store entry name: {name:?}"),
    })
}

fn read_json<T: serde::de::DeserializeOwned>(
    vault: &SealedVault<'_>,
    name: &str,
) -> Result<Option<T>, Error> {
    match vault.fetch(&entry_secret_name(name)?) {
        Ok(secret) => serde_json::from_slice(secret.as_bytes())
            .map(Some)
            .map_err(|e| Error::SpaceStore {
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
    let bytes = serde_json::to_vec(value).map_err(|e| Error::SpaceStore {
        reason: format!("entry {name} does not encode: {e}"),
    })?;
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(Error::SpaceStore {
            reason: format!("entry {name} is {} bytes, over the bound", bytes.len()),
        });
    }
    let secret = Secret::new(bytes).map_err(|source| Error::Vault { source })?;
    vault
        .store(&entry_secret_name(name)?, &secret)
        .map_err(|source| Error::Vault { source })
}

/// Handles one [`SpaceOp`] for `session` at `now`.
///
/// `Denied` when spaces are off, which they are in every run session but
/// a space task's.
#[must_use]
pub fn handle_space_op(
    config: &Config,
    key: &MasterKey,
    session: &SpaceSession,
    op: &SpaceOp,
    now: Timestamp,
) -> SpaceResult {
    if !config.spaces.enabled() {
        return SpaceResult::Denied;
    }
    let spaces = Spaces {
        config,
        key,
        session,
        store: SealedSpaceStore::new(&config.spaces.root, key),
        now,
    };
    let result = match op {
        SpaceOp::Create { name, grants } => spaces.create(name, grants),
        SpaceOp::Join { space } => spaces.join(space),
        SpaceOp::Put {
            space,
            ticket,
            key,
            value,
        } => spaces.put(space, ticket, key, value),
        SpaceOp::Get { space, ticket, key } => spaces.get(space, ticket, key),
        SpaceOp::Remove { space, ticket, key } => spaces.remove(space, ticket, key),
        SpaceOp::AddTask {
            space,
            ticket,
            task,
            grants,
            channel,
        } => spaces.add_task(space, ticket, task, grants, channel),
        SpaceOp::RunTask {
            space,
            ticket,
            task,
        } => spaces.run_task(space, ticket, task),
        SpaceOp::Pair { space, ticket } => spaces.pair(space, ticket),
        SpaceOp::SyncPull {
            space,
            ticket,
            since,
        } => spaces.sync_pull(space, ticket, *since),
        SpaceOp::SyncPush {
            space,
            ticket,
            changes,
        } => spaces.sync_push(space, ticket, changes),
        SpaceOp::Conflicts {
            space,
            ticket,
            after,
        } => spaces.conflicts(space, ticket, *after),
        SpaceOp::PutMedia {
            media_kind,
            space,
            ticket,
            data,
        } => spaces.put_media(space, ticket, media_kind, data),
        SpaceOp::GetMedia {
            space,
            ticket,
            media,
        } => spaces.get_media(space, ticket, media),
    };
    result.unwrap_or_else(|outcome| outcome)
}

/// A refusal, as a reply.
const fn refused(reason: SpaceRefusal) -> SpaceResult {
    SpaceResult::Refused { reason }
}

/// A store failure, logged, as the opaque reply.
fn store_failed(error: &Error, doing: &str) -> SpaceResult {
    tracing::error!(%error, doing, "space store failed");
    SpaceResult::SpaceError
}

/// One space op's view of the session.
struct Spaces<'a> {
    config: &'a Config,
    key: &'a MasterKey,
    session: &'a SpaceSession,
    store: SealedSpaceStore<'a>,
    now: Timestamp,
}

/// An op's outcome: its reply either way.
type Outcome = Result<SpaceResult, SpaceResult>;

impl Spaces<'_> {
    /// Refuses a task run or a device anything but its spaces' contents.
    const fn agent_only(&self) -> Result<(), SpaceResult> {
        match self.session.principal {
            Principal::Agent { .. } => Ok(()),
            Principal::TaskRun { .. } | Principal::Device { .. } => {
                Err(refused(SpaceRefusal::NotAllowed))
            }
        }
    }

    /// A broker-minted id with `prefix`.
    fn mint(prefix: &str) -> Result<String, SpaceResult> {
        crate::approval::random_hex(prefix).map_err(|error| {
            tracing::error!(%error, "space id entropy unavailable");
            SpaceResult::SpaceError
        })
    }

    /// The space `id`, if the session's principal may use it.
    fn member_space(&self, id: &str) -> Result<SpaceRecord, SpaceResult> {
        match self.store.get(id) {
            Ok(Some(record)) if record.admits(&self.session.principal) => Ok(record),
            Ok(_) => Err(refused(SpaceRefusal::NoSuchSpace)),
            Err(error) => Err(store_failed(&error, "reading a space")),
        }
    }

    /// The space `space`, if `ticket` is a live admission to exactly it
    /// for this session and peer, and the principal is still a member.
    fn admitted(&self, space: &str, ticket: &str) -> Result<SpaceRecord, SpaceResult> {
        admissions()
            .check(ticket, space, &self.session.pin, self.now.as_second())
            .map_err(|reason| {
                tracing::warn!(?reason, "space admission refused");
                refused(reason)
            })?;
        self.member_space(space)
    }

    fn save(&self, record: &SpaceRecord) -> Result<(), SpaceResult> {
        match self.store.put(record) {
            Ok(true) => Ok(()),
            Ok(false) => Err(refused(SpaceRefusal::NoSuchSpace)),
            Err(error) => Err(store_failed(&error, "saving a space")),
        }
    }

    fn create(&self, name: &str, grants: &[ScheduleGrant]) -> Outcome {
        self.agent_only()?;
        let id = Self::mint(SPACE_ID_PREFIX)?;
        let record = SpaceRecord::create(
            id,
            name,
            self.session.principal.clone(),
            grants.to_vec(),
            &held_grants(self.config),
            self.now.as_second(),
        )
        .map_err(refused)?;
        match self.store.insert(&record) {
            Ok(true) => Ok(SpaceResult::Created { space: record.id }),
            Ok(false) => Err(refused(SpaceRefusal::Full)),
            Err(error) => Err(store_failed(&error, "creating a space")),
        }
    }

    fn join(&self, space: &str) -> Outcome {
        let record = self.member_space(space)?;
        let ticket = Self::mint(TICKET_PREFIX)?;
        let now = self.now.as_second();
        let admitted = {
            let mut table = admissions();
            table.sweep(now);
            table.admit(ticket.clone(), &record.id, &self.session.pin, now)
        };
        let admission = admitted.map_err(refused)?;
        Ok(SpaceResult::Admitted {
            space: admission.space,
            ticket,
            expires_at: admission.expires_at,
        })
    }

    fn put(&self, space: &str, ticket: &str, key: &str, value: &str) -> Outcome {
        let mut record = self.admitted(space, ticket)?;
        check_key(key).map_err(refused)?;
        check_value(value).map_err(refused)?;
        record
            .write_item(key, &self.session.principal.label(), false)
            .map_err(refused)?;
        self.store
            .put_item(&record.id, key, value)
            .map_err(|error| store_failed(&error, "storing an item"))?;
        self.save(&record)?;
        Ok(SpaceResult::Stored)
    }

    fn get(&self, space: &str, ticket: &str, key: &str) -> Outcome {
        let record = self.admitted(space, ticket)?;
        check_key(key).map_err(refused)?;
        if !record.has_item(key) {
            return Err(refused(SpaceRefusal::NoSuchItem));
        }
        match self.store.get_item(&record.id, key) {
            Ok(Some(value)) => Ok(SpaceResult::Value { value }),
            Ok(None) => Err(refused(SpaceRefusal::NoSuchItem)),
            Err(error) => Err(store_failed(&error, "reading an item")),
        }
    }

    fn remove(&self, space: &str, ticket: &str, key: &str) -> Outcome {
        let mut record = self.admitted(space, ticket)?;
        check_key(key).map_err(refused)?;
        if !record.has_item(key) {
            return Err(refused(SpaceRefusal::NoSuchItem));
        }
        record
            .write_item(key, &self.session.principal.label(), true)
            .map_err(refused)?;
        self.store
            .remove_item(&record.id, key)
            .map_err(|error| store_failed(&error, "removing an item"))?;
        self.save(&record)?;
        Ok(SpaceResult::Removed)
    }

    fn add_task(
        &self,
        space: &str,
        ticket: &str,
        task: &str,
        grants: &[ScheduleGrant],
        channel: &str,
    ) -> Outcome {
        self.agent_only()?;
        let mut record = self.admitted(space, ticket)?;
        let task = SpaceTask {
            id: Self::mint(TASK_ID_PREFIX)?,
            space: record.id.clone(),
            task: task.to_owned(),
            grants: grants.to_vec(),
            channel: channel.to_owned(),
        };
        record.register_task(&task, self.now).map_err(refused)?;
        self.store
            .put_task(&task)
            .map_err(|error| store_failed(&error, "storing a task"))?;
        self.save(&record)?;
        Ok(SpaceResult::TaskAdded { task: task.id })
    }

    fn run_task(&self, space: &str, ticket: &str, task: &str) -> Outcome {
        self.agent_only()?;
        let record = self.admitted(space, ticket)?;
        if !self.config.scheduler.enabled() {
            return Err(refused(SpaceRefusal::SchedulerOff));
        }
        if !record.has_task(task) {
            return Err(refused(SpaceRefusal::NoSuchTask));
        }
        let task = match self.store.get_task(task) {
            Ok(Some(task)) => task,
            Ok(None) => return Err(refused(SpaceRefusal::NoSuchTask)),
            Err(error) => return Err(store_failed(&error, "reading a task")),
        };
        let schedule = record
            .schedule_task(&task, Self::mint(SCHEDULE_ID_PREFIX)?, self.now)
            .map_err(refused)?;
        match SealedScheduleStore::new(&self.config.scheduler.root, self.key).insert(&schedule) {
            Ok(true) => Ok(SpaceResult::TaskQueued {
                schedule: schedule.id,
            }),
            Ok(false) => Err(refused(SpaceRefusal::Full)),
            Err(error) => Err(store_failed(&error, "queueing a task run")),
        }
    }

    fn pair(&self, space: &str, ticket: &str) -> Outcome {
        self.agent_only()?;
        let record = self.admitted(space, ticket)?;
        let code = Self::mint(PAIRING_CODE_PREFIX)?;
        let expires_at = pairings()
            .issue(code.clone(), &record.id, self.now.as_second())
            .map_err(refused)?;
        tracing::info!(space = %record.id, "pairing code issued");
        Ok(SpaceResult::PairingCode { code, expires_at })
    }

    fn sync_pull(&self, space: &str, ticket: &str, since: u64) -> Outcome {
        let record = self.admitted(space, ticket)?;
        let mut pending = Vec::new();
        for item in record.changes_since(since) {
            let value = if item.removed {
                None
            } else {
                self.store
                    .get_item(&record.id, &item.key)
                    .map_err(|error| store_failed(&error, "reading an item to sync"))?
            };
            pending.push(SyncItem {
                key: item.key.clone(),
                value,
                version: item.version,
                writer: item.writer.clone(),
            });
        }
        let (changes, more) = page(pending, PAGE_BYTES);
        let head = changes
            .last()
            .map_or_else(|| since.max(record.head), |last| last.version);
        Ok(SpaceResult::Changes {
            changes,
            head,
            more,
        })
    }

    fn sync_push(&self, space: &str, ticket: &str, changes: &[SyncChange]) -> Outcome {
        let mut record = self.admitted(space, ticket)?;
        check_push(&record, changes).map_err(refused)?;
        let writer = self.session.principal.label();
        let mut conflicts = 0u32;
        for change in changes {
            let lost = overwritten(&record, change).cloned();
            // A removal's value entry is already gone, so it reads as none.
            let lost_value = match &lost {
                Some(_) => self
                    .store
                    .get_item(&record.id, &change.key)
                    .map_err(|error| store_failed(&error, "reading an overwritten item"))?,
                None => None,
            };
            let version = record
                .write_item(&change.key, &writer, change.value.is_none())
                .map_err(refused)?;
            match &change.value {
                Some(value) => self.store.put_item(&record.id, &change.key, value),
                None => self.store.remove_item(&record.id, &change.key),
            }
            .map_err(|error| store_failed(&error, "writing a synced item"))?;
            if let Some(meta) = lost {
                let (seq, dropped) = record.log_conflict();
                let entry = ConflictEntry {
                    seq,
                    key: change.key.clone(),
                    lost: lost_value,
                    lost_version: meta.version,
                    lost_writer: meta.writer,
                    winner: writer.clone(),
                    won_version: version,
                    at: self.now.as_second(),
                };
                self.store
                    .put_conflict(&record.id, &entry)
                    .map_err(|error| store_failed(&error, "logging a conflict"))?;
                if let Some(dropped) = dropped {
                    self.store
                        .remove_conflict(&record.id, dropped)
                        .map_err(|error| store_failed(&error, "dropping a conflict"))?;
                }
                tracing::warn!(space = %record.id, key = %change.key, seq, "sync conflict: last writer won, the overwritten write is logged");
                conflicts = conflicts.saturating_add(1);
            }
        }
        self.save(&record)?;
        Ok(SpaceResult::Pushed {
            head: record.head,
            conflicts,
        })
    }

    fn conflicts(&self, space: &str, ticket: &str, after: u64) -> Outcome {
        let record = self.admitted(space, ticket)?;
        let mut logged = Vec::new();
        for seq in record.conflicts_after(after) {
            if let Some(entry) = self
                .store
                .get_conflict(&record.id, seq)
                .map_err(|error| store_failed(&error, "reading a conflict"))?
            {
                logged.push(entry);
            }
        }
        let (entries, more) = page(logged, PAGE_BYTES);
        Ok(SpaceResult::ConflictLog {
            entries,
            dropped: record.conflicts_dropped,
            more,
        })
    }

    fn put_media(&self, space: &str, ticket: &str, kind: &str, data: &str) -> Outcome {
        let mut record = self.admitted(space, ticket)?;
        let (bytes, content_type) = hatchling_space::admit(kind, data).map_err(|reason| {
            tracing::warn!(?reason, "media refused before admission");
            refused(reason)
        })?;
        let id = Self::mint(MEDIA_ID_PREFIX)?;
        record.add_media(&id).map_err(refused)?;
        let meta = MediaMeta {
            id: id.clone(),
            space: record.id.clone(),
            content_type: content_type.to_owned(),
            bytes: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            uploader: self.session.principal.label(),
            at: self.now.as_second(),
        };
        self.store
            .put_media(&meta, &bytes)
            .map_err(|error| store_failed(&error, "storing media"))?;
        self.save(&record)?;
        Ok(SpaceResult::MediaStored {
            media: id,
            content_type: meta.content_type,
            bytes: meta.bytes,
        })
    }

    fn get_media(&self, space: &str, ticket: &str, media: &str) -> Outcome {
        let record = self.admitted(space, ticket)?;
        if !record.has_media(media) {
            return Err(refused(SpaceRefusal::NoSuchMedia));
        }
        match self.store.get_media(media) {
            Ok(Some((meta, bytes))) if meta.space == record.id => Ok(SpaceResult::Media {
                media: meta.id,
                headers: served_headers(&meta.content_type),
                content_type: meta.content_type,
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            }),
            Ok(_) => Err(refused(SpaceRefusal::NoSuchMedia)),
            Err(error) => Err(store_failed(&error, "reading media")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_entry_names_are_vault_names() {
        let space = "spc-0123456789abcdef0123456789abcdef";
        assert_eq!(space_entry(space), format!("space-{space}"));
        assert_eq!(task_entry("tsk-ab"), "task-tsk-ab");
        assert_eq!(item_entry(space, "plan"), format!("item-{space}-plan"));
        // The longest item entry still names a vault entry.
        let key = "k".repeat(hatchling_space::MAX_KEY_BYTES);
        assert!(entry_secret_name(&item_entry(space, &key)).is_ok());
        assert!(entry_secret_name(&space_entry("../x")).is_err());
        assert_eq!(
            (
                MAX_SPACES,
                SPACE_ID_PREFIX,
                TICKET_PREFIX,
                TASK_ID_PREFIX,
                SPACE_INDEX
            ),
            (64, "spc-", "adm-", "tsk-", "space-index")
        );
    }

    #[test]
    fn an_entry_over_the_vault_bound_is_refused() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-space-entry-bound-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let vault = SealedVault::open_in(&root, &key, hatchling_vault::Store::SPACES).unwrap();
        let at_bound = "x".repeat(MAX_SECRET_BYTES - 2);
        assert!(write_json(&vault, "probe", &at_bound).is_ok());
        let read: Option<String> = read_json(&vault, "probe").unwrap();
        assert_eq!(read.map(|s| s.len()), Some(MAX_SECRET_BYTES - 2));
        let error = write_json(&vault, "probe", &"x".repeat(MAX_SECRET_BYTES - 1)).unwrap_err();
        assert!(
            matches!(&error, Error::SpaceStore { reason } if reason.contains("over the bound")),
            "{error:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The broker's device key is made once, sealed, and the same key on
    /// every later read; a corrupt seed is refused, never used.
    #[test]
    fn the_broker_key_is_made_once_and_read_back_the_same() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-spaces-broker-key-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let store = SealedSpaceStore::new(&root, &key);
        let first = store.broker_key().unwrap().verifying_key();
        let again = SealedSpaceStore::new(&root, &key)
            .broker_key()
            .unwrap()
            .verifying_key();
        let name = entry_secret_name(BROKER_DEVICE_KEY).unwrap();
        let sealed = store.vault().unwrap().fetch(&name).unwrap();
        assert_eq!(sealed.as_bytes().len(), 32);
        store
            .vault()
            .unwrap()
            .store(&name, &Secret::new(vec![7; 31]).unwrap())
            .unwrap();
        let corrupt = store.broker_key();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(first, again);
        assert!(matches!(corrupt, Err(Error::SpaceStore { .. })));
    }

    #[test]
    fn the_store_seals_under_its_own_label() {
        let root = std::env::temp_dir().join(format!(
            "hatchling-test-spaces-label-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let key = MasterKey::generate().unwrap();
        let name: SecretName = "probe".parse().unwrap();
        let secret = Secret::new(b"p".to_vec()).unwrap();
        SealedSpaceStore::new(&root, &key)
            .vault()
            .unwrap()
            .store(&name, &secret)
            .unwrap();
        let own = SealedVault::open_in(&root, &key, hatchling_vault::Store::SPACES)
            .unwrap()
            .fetch(&name);
        let secrets = SealedVault::open(&root, &key).unwrap().fetch(&name);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(own.unwrap().as_bytes(), b"p");
        assert!(secrets.is_err(), "opened under the secrets label");
    }
}
