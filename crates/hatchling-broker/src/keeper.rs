//! The vault keeper (WO-017, step 4): the process that holds the master
//! key and the secrets store's key, so that the broker holds neither.
//!
//! [`start`] runs first in the broker's start, before it touches a key. It
//! starts the broker's own binary again as `vault-keeper`, joined to the
//! broker by a socket pair. The keeper reads the configuration from its
//! own environment, the same as the broker's, and the operator's terminal
//! stays its own for a keyring's prompts. It loads the master key, unwraps
//! every sealed store's key, and hands the broker the keys of the
//! broker's own stores: every one but the secrets. Then it drops the
//! master key and answers the broker until the broker goes:
//!
//! - **a read for one of the broker's purposes**, when every name is one
//!   the configuration gives that purpose (an egress destination's
//!   credential, a configured harness vendor's key, a channel account's
//!   credential, the gates' held secrets);
//! - **a cell's vault op**, checked against the configuration's grants.
//!
//! Each is recorded in the vault's access log before the keeper answers,
//! and one that can't be recorded is answered with nothing
//! ([`crate::secrets`]). A read the configuration doesn't give its
//! purpose is refused and recorded as denied.
//!
//! **What it holds against.** The broker never has the master key or the
//! secrets store's key: a broker that is compromised can't read the
//! vault's files, a copy of them or a backup, and gets only the secrets
//! the configuration names, each release recorded by a process it
//! doesn't share memory with. The keeper isn't dumpable, so a process of
//! the same user without `CAP_SYS_PTRACE` can't read its memory. It is
//! not yet an operating-system boundary: the broker starts it as its own
//! user, and a root broker can reach any process. The spawner split, next,
//! gives the broker a user of its own.

use std::collections::BTreeSet;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::{CellId, SecretName, VaultOp, VaultResult};
use hatchling_vault::{Secret, Store, StoreKeys};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::secrets::{Secrets, handle_vault_op};
use crate::vault_access::{Caller, Outcome};
use crate::{Config, Error, RedactionConfig};

/// The keeper's subcommand: `hatchling-broker vault-keeper`, started only
/// by the broker.
pub const VAULT_KEEPER_SUBCOMMAND: &str = "vault-keeper";

/// The secret recording what cells have had: the secrets a gate scans for.
///
/// It holds the name of every secret a cell has been given or has stored.
/// It is sealed in the secrets store under that store's key, so only the
/// keeper writes it, and no grant may name it.
pub const RELEASED_RECORD: &str = "keeper-released";

/// The setting that names the keeper's end of the socket pair.
const KEEPER_FD: &str = crate::environment::HATCHLING_KEEPER_FD.name;

/// The setting that names the keeper's end of its pair with the egress
/// authority.
const AUTHORITY_FD: &str = crate::environment::HATCHLING_KEEPER_AUTHORITY_FD.name;

/// Who a request came from: the channel it came on, never its words. The
/// broker keeps neither end of the authority's channel, so what reaches
/// the keeper on it is the authority's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// The broker, on the channel of its start.
    Broker,
    /// The egress authority (WO-017), on the channel the broker made for
    /// it and closed.
    Authority,
}

impl Role {
    /// Whom a read by this role for `purpose` is recorded as.
    fn caller(self, purpose: &str) -> Caller {
        let purpose = purpose.to_owned();
        match self {
            Self::Broker => Caller::Broker { purpose },
            Self::Authority => Caller::Authority { purpose },
        }
    }
}

/// What the broker asks the keeper.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case", deny_unknown_fields)]
enum Request {
    /// The broker's own read of `names`, for `purpose`.
    Read { purpose: String, names: Vec<String> },
    /// A cell's vault op, from the cell `cell` whose peer uid is `uid`.
    Cell { op: VaultOp, cell: String, uid: u32 },
    /// The values a gate scans outgoing text for, for `purpose`: the
    /// keeper chooses them.
    Scan { purpose: String },
}

/// What the keeper answers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "kebab-case", deny_unknown_fields)]
enum Reply {
    /// Which of the names held a secret. Each one that did follows in a
    /// frame of its own: its bytes, raw.
    Read { found: Vec<bool> },
    /// A cell op's result.
    Cell { result: VaultResult },
    /// How many values a scan holds. Each follows in a frame of its own:
    /// its bytes, raw.
    Scan { count: usize },
    /// The cell's request was malformed: its session ends.
    Protocol { reason: String },
    /// Refused: not a read the configuration gives the purpose, or one
    /// that couldn't be made or recorded.
    Refused { reason: String },
}

/// The broker's end of the keeper: one request at a time.
#[derive(Debug)]
pub struct Keeper {
    stream: Mutex<UnixStream>,
    /// Set once an exchange broke off (the keeper didn't answer in time,
    /// answered out of turn, or went): a late answer could otherwise be
    /// read as the next request's, so no request is sent again.
    lost: AtomicBool,
    /// Kept so the keeper isn't left a zombie; it leaves when the broker
    /// does. The egress authority's keeper has no child: the broker
    /// started it.
    _child: Option<Child>,
}

/// How long the broker waits for each of the keeper's answers once it
/// has started. Answers are local and quick; one this late means a keeper
/// that is stuck, and every request after it fails rather than waits.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

/// The broker's keeper, once started.
static KEEPER: OnceLock<Keeper> = OnceLock::new();

/// The keeper this broker started, if it started one.
pub(crate) fn keeper() -> Option<&'static Keeper> {
    KEEPER.get()
}

/// Starts the keeper and takes the keys of the broker's stores from it:
/// [`Keeper::start_with`] the broker's own binary, kept as this broker's
/// keeper.
///
/// Also returns the egress authority's end of its channel to the keeper,
/// for [`crate::egress_authority::start`]. Call it once, while the broker has one thread: the keeper's ends of
/// its socket pairs are the descriptors its exec keeps, and they would be
/// inherited by anything another thread started meanwhile.
///
/// # Errors
///
/// As [`Keeper::start_with`], and [`Error::Keeper`] when the broker's
/// binary can't be found or a keeper is already running.
pub fn start() -> Result<(StoreKeys, UnixStream), Error> {
    let program = std::env::current_exe().map_err(|e| Error::Keeper {
        reason: format!("cannot locate the broker binary: {e}"),
    })?;
    let (keeper, keys, authority) = Keeper::start_with(Command::new(program))?;
    KEEPER.set(keeper).map_err(|_| Error::Keeper {
        reason: String::from("a keeper is already running"),
    })?;
    Ok((keys, authority))
}

impl Keeper {
    /// Starts the keeper as `program`, the broker's binary with its
    /// environment, given its subcommand and its ends of two socket pairs
    /// (one with the broker, one with the egress authority), and takes the
    /// keys of the broker's stores from it. Returns the authority's end of
    /// its pair, which the broker hands the authority and closes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper can't be started, refuses
    /// to start (its reason, as the keeper gave it: a key that doesn't
    /// load, a store that doesn't open), or hands over keys that don't
    /// read back.
    pub fn start_with(mut program: Command) -> Result<(Self, StoreKeys, UnixStream), Error> {
        let failed = |reason: String| Error::Keeper { reason };
        let pair =
            || UnixStream::pair().map_err(|e| failed(format!("cannot make a socket pair: {e}")));
        let (ours, theirs) = pair()?;
        let (for_authority, authoritys) = pair()?;
        for end in [theirs.as_fd(), for_authority.as_fd()] {
            hatchling_sys::keep_across_exec(end)
                .map_err(|e| failed(format!("cannot hand the keeper its socket: {e}")))?;
        }
        let child = program
            .arg(VAULT_KEEPER_SUBCOMMAND)
            .env(KEEPER_FD, theirs.as_raw_fd().to_string())
            .env(AUTHORITY_FD, for_authority.as_raw_fd().to_string())
            .spawn()
            .map_err(|e| failed(format!("cannot start: {e}")))?;
        drop(theirs);
        drop(for_authority);
        let first =
            Zeroizing::new(read_frame(&ours).map_err(|e| failed(format!("gave no keys: {e}")))?);
        let keys = match first.split_first() {
            Some((0, handed)) => {
                StoreKeys::from_bytes(handed).map_err(|source| Error::Vault { source })?
            }
            Some((1, reason)) => return Err(failed(String::from_utf8_lossy(reason).into_owned())),
            _ => {
                return Err(failed(String::from(
                    "its first answer was neither keys nor a reason",
                )));
            }
        };
        let keeper = Self {
            _child: Some(child),
            ..Self::over(ours)?
        };
        Ok((keeper, keys, authoritys))
    }

    /// A keeper already running, at the other end of `stream`: the egress
    /// authority's, which the broker started.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when its answers can't be bounded.
    pub fn over(stream: UnixStream) -> Result<Self, Error> {
        stream
            .set_read_timeout(Some(ANSWER_TIMEOUT))
            .map_err(|e| Error::Keeper {
                reason: format!("cannot bound its answers: {e}"),
            })?;
        Ok(Self {
            stream: Mutex::new(stream),
            lost: AtomicBool::new(false),
            _child: None,
        })
    }

    /// The broker's own read of `names`, for `purpose`, answered by the
    /// keeper: as [`Secrets::broker_read`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper refuses the read or
    /// can't be reached.
    pub fn read(&self, purpose: &str, names: &[SecretName]) -> Result<Vec<Option<Secret>>, Error> {
        let request = Request::Read {
            purpose: purpose.to_owned(),
            names: names.iter().map(|name| name.as_str().to_owned()).collect(),
        };
        let (reply, frames) = self.exchange(|stream| {
            let reply = ask(stream, &request)?;
            let mut frames = Vec::new();
            if let Reply::Read { found } = &reply {
                if found.len() != names.len() {
                    return Err(unexpected(&reply));
                }
                for _ in found.iter().filter(|found| **found) {
                    frames.push(Zeroizing::new(read_frame(stream).map_err(keeper_gone)?));
                }
            }
            Ok((reply, frames))
        })?;
        let found = match reply {
            Reply::Read { found } => found,
            Reply::Refused { reason } => return Err(Error::Keeper { reason }),
            other @ (Reply::Cell { .. } | Reply::Protocol { .. } | Reply::Scan { .. }) => {
                return Err(unexpected(&other));
            }
        };
        let mut frames = frames.into_iter();
        found
            .into_iter()
            .map(|found| {
                if !found {
                    return Ok(None);
                }
                let bytes = frames
                    .next()
                    .ok_or_else(|| unexpected(&Reply::Read { found: Vec::new() }))?;
                Secret::new(bytes.to_vec())
                    .map(Some)
                    .map_err(|source| Error::Vault { source })
            })
            .collect()
    }

    /// The values a gate scans outgoing text for, for `purpose`, as the
    /// keeper chooses them: the held secrets a cell has been given or has
    /// stored, and the broker's own credentials.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper refuses the scan or can't
    /// be reached.
    pub fn scan(&self, purpose: &str) -> Result<Vec<Secret>, Error> {
        let request = Request::Scan {
            purpose: purpose.to_owned(),
        };
        let (reply, frames) = self.exchange(|stream| {
            let reply = ask(stream, &request)?;
            let mut frames = Vec::new();
            if let Reply::Scan { count } = &reply {
                for _ in 0..*count {
                    frames.push(Zeroizing::new(read_frame(stream).map_err(keeper_gone)?));
                }
            }
            Ok((reply, frames))
        })?;
        match reply {
            Reply::Scan { .. } => frames
                .into_iter()
                .map(|bytes| Secret::new(bytes.to_vec()).map_err(|source| Error::Vault { source }))
                .collect(),
            Reply::Refused { reason } => Err(Error::Keeper { reason }),
            other @ (Reply::Read { .. } | Reply::Cell { .. } | Reply::Protocol { .. }) => {
                Err(unexpected(&other))
            }
        }
    }

    /// A cell's vault op, answered by the keeper: as
    /// [`handle_vault_op`]. The keeper can't be reached: the op failed,
    /// and the session goes on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SessionProtocol`] when the op was malformed.
    pub fn cell_op(&self, op: &VaultOp, cell: &CellId, uid: u32) -> Result<VaultResult, Error> {
        let request = Request::Cell {
            op: op.clone(),
            cell: cell.to_string(),
            uid,
        };
        match self.exchange(|stream| ask(stream, &request)) {
            Ok(Reply::Cell { result }) => Ok(result),
            Ok(Reply::Protocol { reason }) => Err(Error::SessionProtocol { reason }),
            Ok(other @ (Reply::Read { .. } | Reply::Refused { .. } | Reply::Scan { .. })) => {
                tracing::error!(error = %unexpected(&other), "vault op failed at the keeper");
                Ok(VaultResult::VaultError)
            }
            Err(error) => {
                tracing::error!(%error, "vault op failed at the keeper");
                Ok(VaultResult::VaultError)
            }
        }
    }

    /// Runs one exchange with the keeper, alone. An exchange that breaks
    /// off loses the keeper: every request after it fails at once.
    fn exchange<T>(&self, run: impl FnOnce(&UnixStream) -> Result<T, Error>) -> Result<T, Error> {
        let stream = self.stream.lock().map_err(|_| Error::Keeper {
            reason: String::from("an earlier request to it panicked"),
        })?;
        if self.lost.load(Ordering::SeqCst) {
            return Err(Error::Keeper {
                reason: String::from("lost: an earlier exchange broke off"),
            });
        }
        let outcome = run(&stream);
        if outcome.is_err() {
            self.lost.store(true, Ordering::SeqCst);
        }
        drop(stream);
        outcome
    }
}

/// Sends `request`, and reads the reply.
fn ask(stream: &UnixStream, request: &Request) -> Result<Reply, Error> {
    let bytes = Zeroizing::new(serde_json::to_vec(request).map_err(|e| Error::Keeper {
        reason: format!("cannot write a request: {e}"),
    })?);
    write_frame(stream, &bytes).map_err(keeper_gone)?;
    let reply = Zeroizing::new(read_frame(stream).map_err(keeper_gone)?);
    serde_json::from_slice(&reply).map_err(|e| Error::Keeper {
        reason: format!("an answer that doesn't parse: {e}"),
    })
}

fn keeper_gone(error: impl std::fmt::Display) -> Error {
    Error::Keeper {
        reason: format!("cannot be reached: {error}"),
    }
}

fn unexpected(reply: &Reply) -> Error {
    let kind = match reply {
        Reply::Read { .. } => "read",
        Reply::Cell { .. } => "cell",
        Reply::Scan { .. } => "scan",
        Reply::Protocol { .. } => "protocol",
        Reply::Refused { .. } => "refused",
    };
    Error::Keeper {
        reason: format!("an answer that doesn't answer the request ({kind})"),
    }
}

/// What the configuration lets the broker read, by purpose.
#[derive(Debug, Default)]
struct Releases {
    egress: BTreeSet<String>,
    search: BTreeSet<String>,
    harness: BTreeSet<String>,
    accounts: BTreeSet<String>,
    held: BTreeSet<String>,
}

impl Releases {
    /// Each purpose's names, from `config` and the harnesses it sets up.
    fn of(config: &Config, harness: BTreeSet<String>) -> Self {
        Self {
            egress: config
                .egress
                .grants
                .iter()
                .map(|destination| crate::serve::destination_to_secret_name(destination))
                .collect(),
            search: if config.web.search {
                BTreeSet::from([String::from(crate::web_ops::SEARCH_KEY)])
            } else {
                BTreeSet::new()
            },
            harness,
            accounts: config
                .channel_accounts
                .iter()
                .map(|account| account.secret.as_str().to_owned())
                .collect(),
            held: crate::artifact_ops::held_secret_names(config),
        }
    }

    /// Whether `role` may read every one of `names` for `purpose`. The
    /// egress destinations' credentials, the harnesses' vendor keys and
    /// search's key are the egress authority's alone; the channel
    /// accounts' are the broker's.
    fn allow(&self, role: Role, purpose: &str, names: &[SecretName]) -> bool {
        let given = match (role, purpose) {
            (Role::Authority, "egress") => &self.egress,
            (Role::Authority, "search") => &self.search,
            (Role::Authority, "harness") => &self.harness,
            (Role::Broker, "channel-account") => &self.accounts,
            _ => return false,
        };
        names.iter().all(|name| given.contains(name.as_str()))
    }

    /// Whether `purpose` is a gate's, which scans rather than reads.
    fn scans(purpose: &str) -> bool {
        matches!(purpose, "artifact-gate" | "output-gate")
    }

    /// What a gate scans for: the held secrets a cell has been given or
    /// has stored (`released`), and the channel accounts' and egress
    /// destinations' credentials, which the broker handles itself. A held
    /// secret no cell has had isn't one a cell can leak.
    fn scan_set(&self, released: &BTreeSet<String>) -> Vec<SecretName> {
        self.held
            .iter()
            .filter(|name| {
                released.contains(*name)
                    || self.accounts.contains(*name)
                    || self.egress.contains(*name)
            })
            .filter_map(|name| name.parse().ok())
            .collect()
    }
}

/// The keeper: `hatchling-broker vault-keeper`.
#[must_use]
pub fn vault_keeper_main() -> ExitCode {
    let inherited = |fd: Result<String, std::env::VarError>| {
        fd.ok()
            .and_then(|fd| fd.parse::<i32>().ok())
            .map(hatchling_sys::net::inherited_unix_stream)
    };
    let stream = match inherited(std::env::var(KEEPER_FD)) {
        Some(Ok(stream)) => stream,
        Some(Err(error)) => return refuse_to_run(&error.to_string()),
        None => return refuse_to_run("only the broker starts it"),
    };
    // The authority's channel, when the broker made one: without one, no
    // read is the authority's.
    let authority = match inherited(std::env::var(AUTHORITY_FD)).transpose() {
        Ok(authority) => authority,
        Err(error) => return refuse_to_run(&error.to_string()),
    };
    let begun = hatchling_sys::set_not_dumpable()
        .map_err(|source| Error::Keeper {
            reason: format!("cannot keep its memory to itself: {source}"),
        })
        .and_then(|()| Config::from_env())
        .and_then(|config| {
            let _ = tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_max_level(config.log_level.to_tracing())
                .try_init();
            begin(&stream, config)
        });
    let state = match begun {
        Ok(state) => state,
        Err(error) => {
            let mut reason = vec![1];
            reason.extend_from_slice(error.to_string().as_bytes());
            let _ = write_frame(&stream, &reason);
            return refuse_to_run(&error.to_string());
        }
    };
    let state = Arc::new(Mutex::new(state));
    if let Some(authority) = authority {
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            if let Err(error) = serve(&authority, &shared, Role::Authority) {
                tracing::error!(%error, "the egress authority's channel ended");
            }
        });
    }
    // The keeper leaves when the broker does, the authority's channel
    // with it.
    match serve(&stream, &state, Role::Broker) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => refuse_to_run(&error.to_string()),
    }
}

fn refuse_to_run(reason: &str) -> ExitCode {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "hatchling-broker vault-keeper: {reason}");
    ExitCode::FAILURE
}

/// What the keeper keeps after its start.
struct Kept {
    config: Config,
    /// The secrets store's key: the only one it keeps.
    keys: StoreKeys,
    releases: Releases,
    /// The secrets a cell has been given or has stored
    /// ([`RELEASED_RECORD`]).
    released: BTreeSet<String>,
}

/// The keeper's start under `config`: every store's key, the broker's
/// handed over on `stream`, the master key dropped. A start that fails
/// hands nothing over.
fn begin(stream: &UnixStream, config: Config) -> Result<Kept, Error> {
    let redaction = config
        .channel_ingress
        .as_ref()
        .map(|_| RedactionConfig::from_env(&config))
        .transpose()?;
    let harness = crate::harness::Harnesses::from_env(&config)?.vendor_secret_names();
    let stores = crate::sealed_stores(&config, redaction.as_ref());
    crate::check_sealed_stores(&stores)?;
    let master = crate::load_master_key(&config.vault)?;
    crate::validate_sealed_vault(&config.vault, &master)?;
    let all = crate::hand_store_keys(master, &stores)?;
    let brokers: Vec<Store> = stores
        .iter()
        .map(|(store, _)| *store)
        .filter(|store| *store != Store::SECRETS)
        .collect();
    let handed = all
        .only(&brokers)
        .to_bytes()
        .map_err(|source| Error::Vault { source })?;
    let keys = all.only(&[Store::SECRETS]);
    drop(all);
    let mut frame = Zeroizing::new(Vec::with_capacity(handed.len() + 1));
    frame.push(0);
    frame.extend_from_slice(&handed);
    write_frame(stream, &frame).map_err(keeper_gone)?;
    let rules = Releases::of(&config, harness);
    let released = read_released(&config, &keys)?;
    Ok(Kept {
        config,
        keys,
        releases: rules,
        released,
    })
}

/// The released record, as the secrets store holds it: none yet is none.
///
/// # Errors
///
/// Returns [`Error::Vault`] when it can't be read, and [`Error::Keeper`]
/// when it isn't a list of names: a record that doesn't read stops the
/// keeper, rather than leave the gates scanning for less.
fn read_released(config: &Config, keys: &StoreKeys) -> Result<BTreeSet<String>, Error> {
    let name: SecretName = RELEASED_RECORD.parse().map_err(|_| Error::Keeper {
        reason: String::from("the released record's name doesn't parse"),
    })?;
    let vault = hatchling_vault::SealedVault::open(&config.vault.root, keys)
        .map_err(|source| Error::Vault { source })?;
    match vault.fetch(&name) {
        Ok(record) => serde_json::from_slice(record.as_bytes()).map_err(|e| Error::Keeper {
            reason: format!("the released record doesn't read: {e}"),
        }),
        Err(hatchling_vault::Error::NotFound { .. }) => Ok(BTreeSet::new()),
        Err(source) => Err(Error::Vault { source }),
    }
}

/// Records `name` as released, and keeps the record, before the cell is
/// given it or stores it. A record that can't be kept releases nothing.
fn record_release(kept: &mut Kept, name: &str) -> Result<(), Error> {
    if kept.released.contains(name) {
        return Ok(());
    }
    let mut released = kept.released.clone();
    released.insert(name.to_owned());
    let record = serde_json::to_vec(&released).map_err(|e| Error::Keeper {
        reason: format!("cannot write the released record: {e}"),
    })?;
    let secret = Secret::new(record).map_err(|source| Error::Vault { source })?;
    let record_name: SecretName = RELEASED_RECORD.parse().map_err(|_| Error::Keeper {
        reason: String::from("the released record's name doesn't parse"),
    })?;
    hatchling_vault::SealedVault::open(&kept.config.vault.root, &kept.keys)
        .and_then(|vault| vault.store(&record_name, &secret))
        .map_err(|source| Error::Vault { source })?;
    kept.released = released;
    Ok(())
}

/// The name a cell's op would release, when the configuration grants it:
/// a fetch's or a store's. A removal releases nothing.
fn releases_to_cell(config: &Config, op: &VaultOp) -> Option<String> {
    let action = op.action();
    if !matches!(op, VaultOp::Fetch { .. } | VaultOp::Store { .. }) {
        return None;
    }
    let name = op.name();
    config
        .vault
        .grants
        .iter()
        .any(|grant| grant.action == action && grant.name.as_str() == name)
        .then(|| name.to_owned())
}

/// Answers `role` on `stream` until it goes. The egress authority only
/// reads: its scans and cells' ops are refused.
fn serve(stream: &UnixStream, kept: &Mutex<Kept>, role: Role) -> Result<(), Error> {
    while let Some(frame) = read_frame_or_eof(stream).map_err(keeper_gone)? {
        let frame = Zeroizing::new(frame);
        let request: Request = serde_json::from_slice(&frame).map_err(|e| Error::Keeper {
            reason: format!("a request that doesn't parse: {e}"),
        })?;
        if role == Role::Authority && !matches!(request, Request::Read { .. }) {
            tracing::warn!("refused the egress authority a request that isn't a read");
            send(
                stream,
                &Reply::Refused {
                    reason: String::from("the egress authority only reads"),
                },
            )?;
            continue;
        }
        let mut guard = kept.lock().map_err(|_| Error::Keeper {
            reason: String::from("an earlier answer panicked"),
        })?;
        let kept = &mut *guard;
        match request {
            Request::Read { purpose, names } => {
                answer_read(stream, kept, role, &purpose, &names)?;
            }
            Request::Scan { purpose } => answer_scan(stream, kept, &purpose)?,
            Request::Cell { op, cell, uid } => {
                if let Some(name) = releases_to_cell(&kept.config, &op) {
                    if let Err(error) = record_release(kept, &name) {
                        tracing::error!(%error, "a release not recorded: the op is refused");
                        send(
                            stream,
                            &Reply::Cell {
                                result: VaultResult::VaultError,
                            },
                        )?;
                        continue;
                    }
                }
                let reply = cell
                    .parse::<CellId>()
                    .map_err(|_| Error::SessionProtocol {
                        reason: String::from("a cell id that doesn't parse"),
                    })
                    .and_then(|cell| {
                        handle_vault_op(&kept.config.vault, &kept.keys, &op, &cell, uid)
                    });
                let reply = match reply {
                    Ok(result) => Reply::Cell { result },
                    Err(Error::SessionProtocol { reason }) => Reply::Protocol { reason },
                    Err(error) => {
                        tracing::error!(%error, "vault op failed");
                        Reply::Cell {
                            result: VaultResult::VaultError,
                        }
                    }
                };
                send(stream, &reply)?;
            }
        }
        drop(guard);
    }
    Ok(())
}

/// Answers one read by `role`: refused and recorded as denied unless the
/// configuration gives `role` every one of `names` for `purpose`.
fn answer_read(
    stream: &UnixStream,
    kept: &Kept,
    role: Role,
    purpose: &str,
    names: &[String],
) -> Result<(), Error> {
    let parsed: Option<Vec<SecretName>> = names.iter().map(|name| name.parse().ok()).collect();
    let Some(parsed) = parsed.filter(|parsed| kept.releases.allow(role, purpose, parsed)) else {
        let root: PathBuf = kept.config.vault.root.clone();
        let caller = role.caller(purpose);
        if let Err(error) =
            crate::vault_access::record(&root, caller, "read", names.to_vec(), Outcome::Denied)
        {
            tracing::error!(%error, "a refused read not recorded");
        }
        tracing::warn!(
            purpose,
            "refused a read the configuration doesn't give its purpose"
        );
        return send(
            stream,
            &Reply::Refused {
                reason: format!("the configuration gives {purpose:?} no such read"),
            },
        );
    };
    match Secrets::new(&kept.config.vault.root, &kept.keys)
        .recorded_read(role.caller(purpose), &parsed)
    {
        Ok(read) => {
            send(
                stream,
                &Reply::Read {
                    found: read.iter().map(Option::is_some).collect(),
                },
            )?;
            for secret in read.iter().flatten() {
                write_frame(stream, secret.as_bytes()).map_err(keeper_gone)?;
            }
            Ok(())
        }
        Err(error) => send(
            stream,
            &Reply::Refused {
                reason: error.to_string(),
            },
        ),
    }
}

/// Answers one scan: the values of the scan set, for a gate's purpose
/// alone, read and recorded as [`Secrets::broker_read`] does.
fn answer_scan(stream: &UnixStream, kept: &Kept, purpose: &str) -> Result<(), Error> {
    if !Releases::scans(purpose) {
        tracing::warn!(purpose, "refused a scan for a purpose that isn't a gate's");
        return send(
            stream,
            &Reply::Refused {
                reason: format!("{purpose:?} doesn't scan"),
            },
        );
    }
    let names = kept.releases.scan_set(&kept.released);
    match Secrets::new(&kept.config.vault.root, &kept.keys).broker_read(purpose, &names) {
        Ok(read) => {
            let values: Vec<&Secret> = read.iter().flatten().collect();
            send(
                stream,
                &Reply::Scan {
                    count: values.len(),
                },
            )?;
            for secret in values {
                write_frame(stream, secret.as_bytes()).map_err(keeper_gone)?;
            }
            Ok(())
        }
        Err(error) => send(
            stream,
            &Reply::Refused {
                reason: error.to_string(),
            },
        ),
    }
}

fn send(stream: &UnixStream, reply: &Reply) -> Result<(), Error> {
    let bytes = Zeroizing::new(serde_json::to_vec(reply).map_err(|e| Error::Keeper {
        reason: format!("cannot write an answer: {e}"),
    })?);
    write_frame(stream, &bytes).map_err(keeper_gone)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<SecretName> {
        list.iter().map(|name| name.parse().unwrap()).collect()
    }

    /// Each purpose reads only the names the configuration gives it, and
    /// an unknown purpose reads nothing.
    #[test]
    fn a_purpose_reads_only_what_the_configuration_gives_it() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.egress.grants = vec![String::from("api.example.com:443")];
        config.vault.grants = vec![crate::VaultGrant {
            action: hatchling_core::VaultAction::Fetch,
            name: "token".parse().unwrap(),
        }];
        config.channel_accounts = vec![crate::ChannelAccountSpec {
            channel: String::from("mail"),
            label: String::from("work"),
            secret: "mail-work".parse().unwrap(),
        }];
        let harness = BTreeSet::from([String::from("harness-claude-code")]);
        let rules = Releases::of(&config, harness);
        let broker =
            |purpose: &str, list: &[&str]| rules.allow(Role::Broker, purpose, &names(list));
        let authority =
            |purpose: &str, list: &[&str]| rules.allow(Role::Authority, purpose, &names(list));
        let egress = "egress-api-example-com-443";
        // The egress destinations' credentials: the authority's alone
        // (WO-017, step 2).
        assert!(authority("egress", &[egress]));
        assert!(!authority("egress", &[egress, "token"]));
        assert!(!broker("egress", &[egress]));
        assert!(authority("harness", &["harness-claude-code"]));
        assert!(!authority("harness", &["harness-codex-cli"]));
        assert!(!broker("harness", &["harness-claude-code"]));
        assert!(broker("channel-account", &["mail-work"]));
        assert!(!broker("channel-account", &["token"]));
        // The egress authority reads none of the broker's.
        assert!(!authority("channel-account", &["mail-work"]));
        // Search's key, only while search is on (WO-028), and only to the
        // egress authority (WO-017).
        assert!(!rules.allow(Role::Authority, "search", &names(&["search-brave"])));
        config.web.search = true;
        let searching = Releases::of(&config, BTreeSet::new());
        let search = names(&["search-brave"]);
        assert!(searching.allow(Role::Authority, "search", &search));
        assert!(!searching.allow(Role::Broker, "search", &search));
        assert!(!searching.allow(Role::Authority, "search", &names(&["token"])));
        assert!(!searching.allow(Role::Broker, "egress", &search));
        // The gates scan: they read nothing by name.
        for gate in ["artifact-gate", "output-gate"] {
            assert!(Releases::scans(gate));
            assert!(!broker(gate, &["token"]));
        }
        assert!(!Releases::scans("egress"));
        // A gate scans for the broker's own credentials, and for a held
        // secret only once a cell has had it.
        let scanned = |released: &[&str]| -> Vec<String> {
            let released = released.iter().map(|name| (*name).to_owned()).collect();
            rules
                .scan_set(&released)
                .iter()
                .map(|name| name.as_str().to_owned())
                .collect()
        };
        assert_eq!(scanned(&[]), [egress, "mail-work"]);
        assert_eq!(
            scanned(&["token", "harness-claude-code"]),
            [egress, "mail-work", "token"]
        );
        assert!(!broker("scan", &["token"]));
        assert!(authority("egress", &[]));
        assert_eq!(
            Role::Authority.caller("search"),
            Caller::Authority {
                purpose: String::from("search")
            }
        );
        assert_eq!(
            Role::Broker.caller("egress"),
            Caller::Broker {
                purpose: String::from("egress")
            }
        );
    }

    /// The protocol reads exactly its own shapes: an unknown field or
    /// request is refused.
    #[test]
    fn the_protocol_refuses_what_it_does_not_define() {
        let read = r#"{"request":"read","purpose":"egress","names":["a"]}"#;
        assert!(matches!(
            serde_json::from_str::<Request>(read),
            Ok(Request::Read { .. })
        ));
        for refused in [
            r#"{"request":"read","purpose":"egress","names":["a"],"extra":1}"#,
            r#"{"request":"unwrap","store":"secrets"}"#,
            r#"{"purpose":"egress","names":["a"]}"#,
        ] {
            assert!(
                serde_json::from_str::<Request>(refused).is_err(),
                "{refused}"
            );
        }
        assert!(
            serde_json::from_str::<Reply>(r#"{"reply":"read","found":[true],"key":"x"}"#).is_err()
        );
        let gone = unexpected(&Reply::Protocol {
            reason: String::new(),
        });
        assert!(gone.to_string().contains("(protocol)"), "{gone}");
    }

    /// No keeper runs in a process that didn't start one; one the broker
    /// can't reach says so; and the keeper refuses to run unless the
    /// broker started it.
    #[test]
    fn the_keeper_runs_only_when_started_and_says_why_not() {
        assert!(keeper().is_none());
        assert!(
            keeper_gone("closed")
                .to_string()
                .contains("cannot be reached: closed")
        );
        let failure = format!("{:?}", ExitCode::FAILURE);
        assert_eq!(format!("{:?}", refuse_to_run("a reason")), failure);
        assert_eq!(format!("{:?}", vault_keeper_main()), failure);
    }

    /// A start whose key doesn't load hands the broker nothing.
    #[test]
    fn a_keeper_that_cannot_start_hands_nothing_over() {
        let config = crate::api_auth::tests::owner_config(None);
        let (ours, theirs) = UnixStream::pair().unwrap();
        assert!(begin(&theirs, config).is_err());
        drop(theirs);
        ours.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert!(read_frame_or_eof(&ours).unwrap().is_none());
    }

    /// A read the configuration gives its purpose is answered, found or
    /// not, and recorded; any other is refused and recorded as denied.
    #[test]
    fn answer_read_answers_only_what_the_configuration_gives() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-answer");
        config.vault.root = dir.0.join("vault");
        config.egress.grants = vec![String::from("api.example.com:443")];
        let master = hatchling_vault::MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&config.vault.root, &master)
            .unwrap()
            .store(
                &"unlisted".parse().unwrap(),
                &Secret::new(b"kept".to_vec()).unwrap(),
            )
            .unwrap();
        let keys = StoreKeys::unwrap(&master, &[(Store::SECRETS, &config.vault.root)]).unwrap();
        let releases = Releases::of(&config, BTreeSet::new());
        let kept = Kept {
            config,
            keys,
            releases,
            released: BTreeSet::new(),
        };
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let egress = String::from("egress-api-example-com-443");
        let read = |role, name: &str| {
            answer_read(&ours, &kept, role, "egress", &[name.to_owned()]).unwrap();
            String::from_utf8(read_frame(&theirs).unwrap()).unwrap()
        };
        assert_eq!(
            read(Role::Authority, &egress),
            r#"{"reply":"read","found":[false]}"#
        );
        let refused = read(Role::Authority, "unlisted");
        assert!(refused.starts_with(r#"{"reply":"refused""#), "{refused}");
        assert!(!refused.contains("kept"), "{refused}");
        // The authority's read, asked by the broker: refused, and recorded
        // as the broker's.
        let refused = read(Role::Broker, &egress);
        assert!(refused.starts_with(r#"{"reply":"refused""#), "{refused}");
        let (accesses, _) = crate::vault_access::read_log(&kept.config.vault.root).unwrap();
        let outcomes: Vec<_> = accesses
            .iter()
            .map(|access| (access.caller.clone(), access.outcome))
            .collect();
        let broker = Caller::Broker {
            purpose: String::from("egress"),
        };
        let authority = Caller::Authority {
            purpose: String::from("egress"),
        };
        assert_eq!(
            outcomes,
            [
                (authority.clone(), Outcome::NotFound),
                (authority, Outcome::Denied),
                (broker, Outcome::Denied)
            ]
        );
    }

    /// On the egress authority's channel, the keeper answers the
    /// authority's reads, recorded as its own, and refuses everything
    /// else: a scan, a cell's op, the broker's reads.
    #[test]
    fn the_authoritys_channel_answers_only_the_authoritys_reads() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-authority");
        config.vault.root = dir.0.join("vault");
        config.web.search = true;
        config.vault.grants = vec![crate::VaultGrant {
            action: hatchling_core::VaultAction::Fetch,
            name: "token".parse().unwrap(),
        }];
        let master = hatchling_vault::MasterKey::generate().unwrap();
        let vault = hatchling_vault::SealedVault::open(&config.vault.root, &master).unwrap();
        for (name, value) in [("search-brave", b"k-brave"), ("token", b"t-token")] {
            vault
                .store(
                    &name.parse().unwrap(),
                    &Secret::new(value.to_vec()).unwrap(),
                )
                .unwrap();
        }
        let keys = StoreKeys::unwrap(&master, &[(Store::SECRETS, &config.vault.root)]).unwrap();
        let releases = Releases::of(&config, BTreeSet::new());
        let kept = Mutex::new(Kept {
            config,
            keys,
            releases,
            released: BTreeSet::new(),
        });
        let (ours, theirs) = UnixStream::pair().unwrap();
        let root = dir.0.join("vault");
        let serving = std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve(&theirs, &kept, Role::Authority));
            let keeper = Keeper::over(ours).unwrap();
            let search: Vec<SecretName> = names(&["search-brave"]);
            let read = keeper.read("search", &search).unwrap();
            let key = read.first().and_then(Option::as_ref).unwrap();
            assert_eq!(key.as_bytes(), b"k-brave");
            assert!(keeper.read("egress", &names(&["token"])).is_err());
            let scan = format!("{:?}", keeper.scan("output-gate"));
            assert!(scan.contains("only reads"), "{scan}");
            let op = VaultOp::Fetch {
                name: String::from("token"),
            };
            let cell: CellId = "c".parse().unwrap();
            assert_eq!(
                keeper.cell_op(&op, &cell, 7).unwrap(),
                VaultResult::VaultError
            );
            drop(keeper);
            serving.join().unwrap()
        });
        assert!(serving.is_ok(), "ends when the authority goes");
        assert!(
            kept.lock().unwrap().released.is_empty(),
            "nothing released to a cell"
        );
        let (accesses, _) = crate::vault_access::read_log(&root).unwrap();
        let logged: Vec<_> = accesses
            .iter()
            .map(|access| (access.caller.clone(), access.outcome))
            .collect();
        assert_eq!(
            logged,
            [
                (
                    Caller::Authority {
                        purpose: String::from("search")
                    },
                    Outcome::Allowed
                ),
                (
                    Caller::Authority {
                        purpose: String::from("egress")
                    },
                    Outcome::Denied
                ),
            ]
        );
    }

    /// On the broker's channel, the keeper answers the broker as before:
    /// its reads, its gates' scans and cells' ops; search's key is not the
    /// broker's.
    #[test]
    fn the_brokers_channel_serves_the_broker_and_not_search() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-broker-channel");
        config.vault.root = dir.0.join("vault");
        config.web.search = true;
        let master = hatchling_vault::MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&config.vault.root, &master)
            .unwrap()
            .store(
                &"search-brave".parse().unwrap(),
                &Secret::new(b"k-brave".to_vec()).unwrap(),
            )
            .unwrap();
        let keys = StoreKeys::unwrap(&master, &[(Store::SECRETS, &config.vault.root)]).unwrap();
        let releases = Releases::of(&config, BTreeSet::new());
        let kept = Mutex::new(Kept {
            config,
            keys,
            releases,
            released: BTreeSet::new(),
        });
        let (ours, theirs) = UnixStream::pair().unwrap();
        std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve(&theirs, &kept, Role::Broker));
            let keeper = Keeper::over(ours).unwrap();
            let refused = format!("{:?}", keeper.read("search", &names(&["search-brave"])));
            assert!(refused.contains("no such read"), "{refused}");
            assert!(keeper.scan("output-gate").unwrap().is_empty());
            let op = VaultOp::Fetch {
                name: String::from("token"),
            };
            let cell: CellId = "c".parse().unwrap();
            assert_eq!(keeper.cell_op(&op, &cell, 7).unwrap(), VaultResult::Denied);
            drop(keeper);
            assert!(serving.join().unwrap().is_ok());
        });
    }

    /// A keeper that answers a read out of step is lost: the broker sends
    /// it nothing more, and every request after fails at once.
    #[test]
    fn an_exchange_that_breaks_off_loses_the_keeper() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let keeper = Keeper::over(ours).unwrap();
        let stand_in = std::thread::spawn(move || {
            let request = read_frame(&theirs).unwrap();
            // Two names asked for, one answered for.
            write_frame(&theirs, br#"{"reply":"read","found":[true]}"#).unwrap();
            let after = read_frame_or_eof(&theirs).unwrap();
            (request, after)
        });
        // An exchange that goes through is the keeper's answer.
        assert_eq!(keeper.exchange(|_| Ok(7)).unwrap(), 7);
        let names: Vec<SecretName> = ["a", "b"]
            .iter()
            .map(|name| name.parse().unwrap())
            .collect();
        assert!(matches!(
            keeper.read("egress", &names),
            Err(Error::Keeper { .. })
        ));
        // Lost: not even an exchange that needs nothing from it runs.
        assert!(keeper.exchange(|_| Ok(7)).is_err());
        let after = keeper.read("egress", &names);
        let refused = format!("{after:?}");
        assert!(refused.contains("lost"), "{refused}");
        drop(keeper);
        let (request, after) = stand_in.join().unwrap();
        assert!(
            String::from_utf8(request)
                .unwrap()
                .contains(r#""purpose":"egress""#)
        );
        assert!(
            after.is_none(),
            "a request was sent after the keeper was lost"
        );
    }

    /// A release is recorded, sealed in the secrets store, before the cell
    /// has it, once; the record reads back; a scan answers with the scan
    /// set's values, for a gate alone; only a granted fetch or store
    /// releases anything.
    #[test]
    fn a_release_is_recorded_and_a_gate_scans_for_it() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-released");
        config.vault.root = dir.0.join("vault");
        config.vault.grants = vec![
            crate::VaultGrant {
                action: hatchling_core::VaultAction::Fetch,
                name: "token".parse().unwrap(),
            },
            crate::VaultGrant {
                action: hatchling_core::VaultAction::Remove,
                name: "old".parse().unwrap(),
            },
        ];
        let master = hatchling_vault::MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&config.vault.root, &master)
            .unwrap()
            .store(
                &"token".parse().unwrap(),
                &Secret::new(b"t".to_vec()).unwrap(),
            )
            .unwrap();
        let fetch = |name: &str| VaultOp::Fetch {
            name: name.to_owned(),
        };
        assert_eq!(
            releases_to_cell(&config, &fetch("token")).as_deref(),
            Some("token")
        );
        assert_eq!(releases_to_cell(&config, &fetch("other")), None);
        let remove = VaultOp::Remove {
            name: String::from("old"),
        };
        assert_eq!(releases_to_cell(&config, &remove), None);
        let keys = StoreKeys::unwrap(&master, &[(Store::SECRETS, &config.vault.root)]).unwrap();
        assert!(read_released(&config, &keys).unwrap().is_empty());
        let releases = Releases::of(&config, BTreeSet::new());
        let mut kept = Kept {
            config,
            keys,
            releases,
            released: BTreeSet::new(),
        };
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        answer_scan(&ours, &kept, "output-gate").unwrap();
        assert_eq!(
            read_frame(&theirs).unwrap(),
            br#"{"reply":"scan","count":0}"#
        );
        record_release(&mut kept, "token").unwrap();
        record_release(&mut kept, "token").unwrap();
        assert_eq!(
            read_released(&kept.config, &kept.keys).unwrap(),
            BTreeSet::from([String::from("token")])
        );
        answer_scan(&ours, &kept, "output-gate").unwrap();
        assert_eq!(
            read_frame(&theirs).unwrap(),
            br#"{"reply":"scan","count":1}"#
        );
        assert_eq!(read_frame(&theirs).unwrap(), b"t");
        answer_scan(&ours, &kept, "egress").unwrap();
        let reply = String::from_utf8(read_frame(&theirs).unwrap()).unwrap();
        assert!(reply.starts_with(r#"{"reply":"refused""#), "{reply}");
    }
}
