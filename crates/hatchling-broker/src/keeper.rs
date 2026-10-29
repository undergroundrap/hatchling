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
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::{
    CellId, MAX_FRAME_BYTES, SecretName, SessionRequest, SessionResponse, VaultOp, VaultResult,
};
use hatchling_vault::{MasterKey, Secret, Store, StoreKeys};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::secrets::{Secrets, handle_vault_op};
use crate::vault_access::{Caller, Outcome};
use crate::vault_keyring::KeyringConfig;
use crate::{Config, Error, RedactionConfig, VaultConfig, VaultGrant};

/// The keeper's subcommand: `hatchling-broker vault-keeper`, started only
/// by the broker.
pub const VAULT_KEEPER_SUBCOMMAND: &str = "vault-keeper";

/// The vault keeper's user when the broker runs as root (WO-017, step 3):
/// it owns the vault, its key and its keyring, and the keeper runs as it.
pub const KEEPER_USER: &str = "hatchling-keeper";

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

/// The keeper's end of its pair with the spawner, which hands it each
/// cell's vault channel (WO-031).
const SPAWNER_FD: &str = crate::environment::HATCHLING_KEEPER_SPAWNER_FD.name;

/// The security keys opened for the keeper ([`open_security_keys`]).
pub(crate) const KEEPER_KEYS: &str = crate::environment::HATCHLING_KEEPER_KEYS.name;

/// What the keeper's first frames are, by their first byte: the keys it
/// hands the broker,
const HANDED: u8 = 0;
/// why it won't,
const REFUSED: u8 = 1;
/// or the broker's stores it asks the keys of, as they keep them.
const ASKING: u8 = 2;

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
    /// How many values a scan holds. Each follows in a frame of its own:
    /// its bytes, raw.
    Scan { count: usize },
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
/// for [`crate::egress_authority::start`], and the spawner's, on which it
/// hands the keeper each cell's vault channel (WO-031). Call it once, while the broker has one thread: the keeper's ends of
/// its socket pairs are the descriptors its exec keeps, and they would be
/// inherited by anything another thread started meanwhile.
///
/// # Errors
///
/// As [`Keeper::start_with`], and [`Error::Keeper`] when the broker's
/// binary can't be found or a keeper is already running.
pub fn start() -> Result<(StoreKeys, UnixStream, UnixStream), Error> {
    let program = std::env::current_exe().map_err(|e| Error::Keeper {
        reason: format!("cannot locate the broker binary: {e}"),
    })?;
    let (keeper, keys, authority, spawner) = Keeper::start_with(Command::new(program))?;
    KEEPER.set(keeper).map_err(|_| Error::Keeper {
        reason: String::from("a keeper is already running"),
    })?;
    Ok((keys, authority, spawner))
}

/// The keeper's user, its uid and gid, when this process runs as root;
/// none otherwise, when the keeper runs as this process's own user.
///
/// # Errors
///
/// [`Error::Keeper`] when this process runs as root and there's no such
/// user (saying how to make one), or it is root itself.
pub fn keeper_user() -> Result<Option<(u32, u32)>, Error> {
    service_user(KEEPER_USER, "its vault keeper").map_err(|reason| Error::Keeper { reason })
}

/// The uid and gid of `name`, the user the broker gives `part` of itself
/// when it runs as root (WO-017); none otherwise, when every part runs as
/// the broker's own user.
///
/// # Errors
///
/// Why not, when this process runs as root and there's no such user
/// (saying how to make one), or it is root itself.
pub(crate) fn service_user(name: &str, part: &str) -> Result<Option<(u32, u32)>, String> {
    if hatchling_sys::euid() != 0 {
        return Ok(None);
    }
    match hatchling_sys::subid::local_account(name).map_err(|e| e.to_string())? {
        Some((0, _)) => Err(format!("{name} is root: it must be a user of its own")),
        Some(ids) => Ok(Some(ids)),
        None => Err(format!(
            "run as root, the broker gives {part} a user of its own, and there is no {name}: \
             create it with \
             `useradd --system --user-group --no-create-home --shell /usr/sbin/nologin {name}`"
        )),
    }
}

/// The directories the keeper's user owns: the vault's, its key file's,
/// its keyring's and its file tier's KEK's, each once.
#[must_use]
pub fn keeper_dirs(vault: &VaultConfig, keyring: &KeyringConfig) -> Vec<PathBuf> {
    let mut dirs = vec![vault.root.clone()];
    let files = [
        Some(vault.key_file.clone()),
        keyring.keyring.clone(),
        Some(keyring.kek_file(vault)),
    ];
    for dir in files.iter().flatten().filter_map(|file| file.parent()) {
        if !dirs.iter().any(|known| known == dir) {
            dirs.push(dir.to_path_buf());
        }
    }
    dirs
}

/// Hands `dirs` to the service user `user`, `uid` and `gid`, before it
/// uses them (WO-017): each made if absent, and it and everything in it
/// given to the user, each change logged. A directory the user couldn't
/// reach, below one it may not search, is refused, naming that one.
///
/// # Errors
///
/// Why, for a directory that can't be made, reached or given.
pub(crate) fn hand_to(user: &str, dirs: &[PathBuf], uid: u32, gid: u32) -> Result<(), String> {
    let failed =
        |dir: &Path, why: String| format!("cannot hand {} to {user}: {why}", dir.display());
    for dir in dirs {
        if let Some(closed) = dir
            .ancestors()
            .skip(1)
            .find(|above| !searchable(above, uid, gid))
        {
            return Err(failed(
                dir,
                format!(
                    "{user} can't reach it through {}, which it may not search",
                    closed.display()
                ),
            ));
        }
        if std::fs::symlink_metadata(dir).is_err() {
            hatchling_sys::create_private_dir(dir).map_err(|e| failed(dir, e.to_string()))?;
        }
        // Handed over once: on a later start everything is the user's.
        let changed = not_theirs(dir, uid, gid, NOT_THEIRS_DEPTH);
        if changed > 0 {
            hatchling_sys::chown_tree(dir, uid, gid).map_err(|e| failed(dir, e.to_string()))?;
            tracing::info!(dir = %dir.display(), changed, user, "handed to its service user");
        }
    }
    Ok(())
}

/// How deep [`not_theirs`] looks before it counts what is below as
/// changed, leaving [`hatchling_sys::chown_tree`] to refuse it.
const NOT_THEIRS_DEPTH: usize = 16;

/// How many entries of the tree at `dir`, itself included, aren't `uid`'s
/// and `gid`'s: what [`hatchling_sys::chown_tree`] would change. Nothing
/// is followed; one that can't be looked at, or lies past `depth`, counts,
/// so the walk that changes it reports it.
fn not_theirs(dir: &Path, uid: u32, gid: u32, depth: usize) -> usize {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return 1;
    };
    if depth == 0 {
        return 1;
    }
    let own = usize::from(meta.uid() != uid || meta.gid() != gid);
    if !meta.is_dir() {
        return own;
    }
    std::fs::read_dir(dir).map_or(own + 1, |entries| {
        entries.fold(own, |count, entry| {
            count + entry.map_or(1, |entry| not_theirs(&entry.path(), uid, gid, depth - 1))
        })
    })
}

/// Whether the user `uid`, in group `gid`, may search the directory `dir`
/// (by its mode, as the kernel decides for a user that isn't root).
fn searchable(dir: &Path, uid: u32, gid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(dir).is_ok_and(|meta| {
        let mode = meta.mode();
        if meta.uid() == uid {
            mode & 0o100 != 0
        } else if meta.gid() == gid {
            mode & 0o010 != 0
        } else {
            mode & 0o001 != 0
        }
    })
}

/// The security keys plugged in now, each opened by its node's name: what
/// the keeper's user may not open itself. One that can't be opened is
/// left out, said in the log.
fn open_security_keys() -> Vec<(String, std::fs::File)> {
    let bus = hatchling_fido2::linux::Hidraw::new(Box::new(|| None));
    let nodes = bus.nodes().unwrap_or_else(|error| {
        tracing::warn!(%error, "cannot list the security keys for the vault keeper");
        Vec::new()
    });
    nodes
        .into_iter()
        .filter_map(|node| {
            let path = Path::new("/dev").join(&node);
            match std::fs::File::options().read(true).write(true).open(&path) {
                Ok(file) => Some((node, file)),
                Err(error) => {
                    tracing::warn!(key = %path.display(), %error, "cannot open a security key for the vault keeper");
                    None
                }
            }
        })
        .collect()
}

/// `keys` as the keeper reads them ([`KEEPER_KEYS`]): `node=descriptor`,
/// comma separated.
fn keys_env(keys: &[(String, std::fs::File)]) -> String {
    keys.iter()
        .map(|(node, file)| format!("{node}={}", file.as_raw_fd()))
        .collect::<Vec<_>>()
        .join(",")
}

/// The security keys `handed` names ([`keys_env`]), adopted: each a
/// device this process inherited. One that isn't is left out, said in
/// the log.
pub(crate) fn handed_keys(handed: &str) -> Vec<(String, std::fs::File)> {
    handed
        .split(',')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let adopted = pair
                .split_once('=')
                .and_then(|(node, fd)| Some((node, fd.parse::<i32>().ok()?)))
                .ok_or_else(|| String::from("not node=descriptor"))
                .and_then(|(node, fd)| {
                    hatchling_sys::inherited_device(fd)
                        .map(|file| (node.to_owned(), file))
                        .map_err(|error| error.to_string())
                });
            adopted
                .map_err(
                    |error| tracing::warn!(%pair, %error, "a security key handed over was refused"),
                )
                .ok()
        })
        .collect()
}

/// Lets an operator's command, run as root, act for the keeper's user in
/// its directories ([`hatchling_vault::act_for`]).
///
/// They are handed to that user first, as the broker hands them: the
/// vault, its key and its keyring are opened as that user's, and what the
/// command makes there is given to it. Not run as root, it changes
/// nothing.
///
/// # Errors
///
/// As [`keeper_user`], and [`Error::Keeper`] when the directories can't be
/// handed over or the acting is refused.
pub fn act_for_keeper(config: &Config) -> Result<(), Error> {
    let Some((uid, gid)) = keeper_user()? else {
        return Ok(());
    };
    let dirs = keeper_dirs(&config.vault, &KeyringConfig::from_env()?);
    hand_to(KEEPER_USER, &dirs, uid, gid).map_err(|reason| Error::Keeper { reason })?;
    let dirs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    hatchling_vault::act_for(uid, &dirs).map_err(|e| Error::Keeper {
        reason: format!("cannot act for {KEEPER_USER}: {e}"),
    })
}

impl Keeper {
    /// Starts the keeper as `program`, the broker's binary with its
    /// environment, given its subcommand and its ends of three socket
    /// pairs (with the broker, the egress authority and the spawner), and
    /// takes the keys of the broker's stores from it. Returns the
    /// authority's and the spawner's ends of their pairs, which the broker
    /// hands on and closes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper can't be started, refuses
    /// to start (its reason, as the keeper gave it: a key that doesn't
    /// load, a store that doesn't open), or hands over keys that don't
    /// read back.
    pub fn start_with(
        mut program: Command,
    ) -> Result<(Self, StoreKeys, UnixStream, UnixStream), Error> {
        let failed = |reason: String| Error::Keeper { reason };
        // As root, the keeper runs as its own user, which is given the
        // vault, its key and its keyring first. The security keys it may
        // not open are opened here and handed to it (the owner's choice,
        // 2026-10-24), held until it has started.
        let _held_keys = match keeper_user()? {
            Some((uid, gid)) => {
                let config = Config::from_env()?;
                hand_to(
                    KEEPER_USER,
                    &keeper_dirs(&config.vault, &KeyringConfig::from_env()?),
                    uid,
                    gid,
                )
                .map_err(failed)?;
                let keys = open_security_keys();
                for (_, key) in &keys {
                    hatchling_sys::keep_across_exec(key.as_fd()).map_err(|e| {
                        failed(format!("cannot hand the keeper a security key: {e}"))
                    })?;
                }
                program.env(KEEPER_KEYS, keys_env(&keys)).uid(uid).gid(gid);
                keys
            }
            None => Vec::new(),
        };
        let pair =
            || UnixStream::pair().map_err(|e| failed(format!("cannot make a socket pair: {e}")));
        let (ours, theirs) = pair()?;
        let (for_authority, authoritys) = pair()?;
        let (for_spawner, spawners) = pair()?;
        for end in [theirs.as_fd(), for_authority.as_fd(), for_spawner.as_fd()] {
            hatchling_sys::keep_across_exec(end)
                .map_err(|e| failed(format!("cannot hand the keeper its socket: {e}")))?;
        }
        let child = program
            .arg(VAULT_KEEPER_SUBCOMMAND)
            .env(KEEPER_FD, theirs.as_raw_fd().to_string())
            .env(AUTHORITY_FD, for_authority.as_raw_fd().to_string())
            .env(SPAWNER_FD, for_spawner.as_raw_fd().to_string())
            .spawn()
            .map_err(|e| failed(format!("cannot start: {e}")))?;
        drop(theirs);
        drop(for_authority);
        drop(for_spawner);
        // The broker's stores are the broker's: the keeper asks for their
        // keys as they keep them, wrapped, and unwraps them, opening none.
        let first = read_frame(&ours).map_err(|e| failed(format!("asked for no keys: {e}")))?;
        let asked = match first.split_first() {
            Some((&ASKING, asking)) => {
                hatchling_vault::handoff::asked(asking).map_err(|source| Error::Vault { source })?
            }
            Some((&REFUSED, reason)) => {
                return Err(failed(String::from_utf8_lossy(reason).into_owned()));
            }
            _ => {
                return Err(failed(String::from(
                    "its first answer was neither an asking nor a reason",
                )));
            }
        };
        write_frame(&ours, &kept_keys(&asked)?)
            .map_err(|e| failed(format!("cannot answer its asking: {e}")))?;
        let handed =
            Zeroizing::new(read_frame(&ours).map_err(|e| failed(format!("gave no keys: {e}")))?);
        let keys = match handed.split_first() {
            Some((&HANDED, handed)) => {
                StoreKeys::from_bytes(handed).map_err(|source| Error::Vault { source })?
            }
            Some((&REFUSED, reason)) => {
                return Err(failed(String::from_utf8_lossy(reason).into_owned()));
            }
            _ => {
                return Err(failed(String::from(
                    "its answer was neither keys nor a reason",
                )));
            }
        };
        let keeper = Self {
            _child: Some(child),
            ..Self::over(ours)?
        };
        Ok((keeper, keys, authoritys, spawners))
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
            other @ Reply::Scan { .. } => return Err(unexpected(&other)),
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
            other @ Reply::Read { .. } => Err(unexpected(&other)),
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

/// The broker's answer to the keeper's asking, `asked`: each store's key
/// as the store keeps it, wrapped, read here, where the stores are the
/// broker's. The vault is the keeper's own: asking for it is refused.
///
/// # Errors
///
/// [`Error::Keeper`] for an asking for the vault, and [`Error::Vault`]
/// for a store whose key can't be read.
fn kept_keys(asked: &[(Store, PathBuf)]) -> Result<Vec<u8>, Error> {
    let mut kept = Vec::with_capacity(asked.len());
    for (store, root) in asked {
        if *store == Store::SECRETS {
            return Err(Error::Keeper {
                reason: String::from("it asked for the vault's key, which is its own"),
            });
        }
        kept.push(
            hatchling_vault::wrapped_store_key(root, *store)
                .map_err(|source| Error::Vault { source })?,
        );
    }
    hatchling_vault::handoff::kept_bytes(&kept).map_err(|source| Error::Vault { source })
}

/// The keys of the broker's `stores` under `master`: asked of the broker
/// on `stream` as its stores keep them, wrapped, and unwrapped here,
/// where none of them is opened.
///
/// # Errors
///
/// [`Error::Keeper`] when the broker can't be asked or doesn't answer,
/// and [`Error::Vault`] for an answer that isn't one, or a key that
/// doesn't unwrap as its store's.
fn broker_keys(
    stream: &UnixStream,
    master: &MasterKey,
    stores: &[(Store, PathBuf)],
) -> Result<StoreKeys, Error> {
    let stores: Vec<(Store, &Path)> = stores
        .iter()
        .map(|(store, root)| (*store, root.as_path()))
        .collect();
    let asking =
        hatchling_vault::handoff::ask_bytes(&stores).map_err(|source| Error::Vault { source })?;
    let mut frame = Vec::with_capacity(asking.len() + 1);
    frame.push(ASKING);
    frame.extend_from_slice(&asking);
    write_frame(stream, &frame).map_err(keeper_gone)?;
    let answer = read_frame(stream).map_err(keeper_gone)?;
    let kept = hatchling_vault::handoff::kept(&answer).map_err(|source| Error::Vault { source })?;
    StoreKeys::unwrap_kept(master, &stores, &kept).map_err(|source| Error::Vault { source })
}

fn keeper_gone(error: impl std::fmt::Display) -> Error {
    Error::Keeper {
        reason: format!("cannot be reached: {error}"),
    }
}

fn unexpected(reply: &Reply) -> Error {
    let kind = match reply {
        Reply::Read { .. } => "read",
        Reply::Scan { .. } => "scan",
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
    /// has stored (`released`), and the channel accounts' credentials,
    /// which the broker handles itself. A held secret no cell has had isn't
    /// one a cell can leak. The egress destinations' credentials are the
    /// egress authority's alone (WO-017, step 2): the broker never holds
    /// one, so it can't leak one, and a scan never hands it one.
    fn scan_set(&self, released: &BTreeSet<String>) -> Vec<SecretName> {
        self.held
            .iter()
            .filter(|name| released.contains(*name) || self.accounts.contains(*name))
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
    // The spawner's, likewise: without one, no cell has a channel here.
    let spawner = match inherited(std::env::var(SPAWNER_FD)).transpose() {
        Ok(spawner) => spawner,
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
            // Which user it runs as: as root, the keeper's own.
            tracing::info!(euid = hatchling_sys::euid(), "the vault keeper started");
            begin(&stream, config)
        });
    let state = match begun {
        Ok(state) => state,
        Err(error) => {
            let mut reason = vec![REFUSED];
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
    if let Some(spawner) = spawner {
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            if let Err(error) = take_cells(&spawner, &shared) {
                tracing::error!(%error, "the spawner's channel ended");
            }
        });
    }
    // The keeper leaves when the broker does, the other channels with it.
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
    // The vault is the keeper's alone; the other stores are the broker's,
    // which checks them, and whose keys it asks the broker for.
    let (vault, brokers): (Vec<_>, Vec<_>) = crate::sealed_stores(&config, redaction.as_ref())
        .into_iter()
        .partition(|(store, _)| *store == Store::SECRETS);
    crate::check_sealed_stores(&vault)?;
    let master = crate::load_master_key(&config.vault)?;
    crate::validate_sealed_vault(&config.vault, &master)?;
    let handed = broker_keys(stream, &master, &brokers)?
        .to_bytes()
        .map_err(|source| Error::Vault { source })?;
    let keys = crate::hand_store_keys(master, &vault)?;
    let mut frame = Zeroizing::new(Vec::with_capacity(handed.len() + 1));
    frame.push(HANDED);
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

/// A grant as the broker names it to the keeper: `action:name`.
pub(crate) fn grant_word(grant: &VaultGrant) -> String {
    format!("{}:{}", grant.action, grant.name.as_str())
}

/// The vault configuration a cell's op is answered under: the keeper's
/// own, with only the grants the cell's session holds too. A scheduled
/// run, a subagent or a chat holds fewer than the agent: its session's
/// grants narrow the keeper's, and never widen them.
fn held_by(vault: &VaultConfig, held: &[String]) -> VaultConfig {
    let mut vault = vault.clone();
    vault
        .grants
        .retain(|grant| held.contains(&grant_word(grant)));
    vault
}

/// The name a cell's op would release, when `vault` grants it: a fetch's
/// or a store's. A removal releases nothing.
fn releases_to_cell(vault: &VaultConfig, op: &VaultOp) -> Option<String> {
    let action = op.action();
    if !matches!(op, VaultOp::Fetch { .. } | VaultOp::Store { .. }) {
        return None;
    }
    let name = op.name();
    vault
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
        }
        drop(guard);
    }
    Ok(())
}

/// A cell's vault op, answered under the grants its session holds where
/// the keeper's configuration makes them too, and recorded as that
/// cell's. A fetch or a store is recorded as released first: one that
/// can't be recorded fails, and so does a vault that can't be used.
///
/// # Errors
///
/// Why the op, or the cell's id, is malformed: the cell's session ends.
fn cell_op_result(
    kept: &mut Kept,
    op: &VaultOp,
    cell: &str,
    uid: u32,
    grants: &[String],
) -> Result<VaultResult, String> {
    let cell: CellId = cell
        .parse()
        .map_err(|_| String::from("a cell id that doesn't parse"))?;
    let vault = held_by(&kept.config.vault, grants);
    if let Some(name) = releases_to_cell(&vault, op) {
        if let Err(error) = record_release(kept, &name) {
            tracing::error!(%error, "a release not recorded: the op is refused");
            return Ok(VaultResult::VaultError);
        }
    }
    match handle_vault_op(&vault, &kept.keys, op, &cell, uid) {
        Ok(result) => Ok(result),
        Err(Error::SessionProtocol { reason }) => Err(reason),
        Err(error) => {
            tracing::error!(%error, "vault op failed");
            Ok(VaultResult::VaultError)
        }
    }
}

/// A cell's own channel to the keeper (WO-031), as the spawner names it
/// when it hands it over: which cell, its peer uid, and the grants its
/// session holds (each `action:name`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CellChannel {
    pub cell: String,
    pub uid: u32,
    pub grants: Vec<String>,
}

/// Takes each cell's channel the spawner hands over, named first, and
/// answers it on a thread of its own, until the spawner goes.
fn take_cells(spawner: &UnixStream, kept: &Arc<Mutex<Kept>>) -> Result<(), Error> {
    while let Some(frame) = read_frame_or_eof(spawner).map_err(keeper_gone)? {
        let named: CellChannel = serde_json::from_slice(&frame).map_err(|e| Error::Keeper {
            reason: format!("a cell's channel named in a frame that doesn't parse: {e}"),
        })?;
        let channel = hatchling_sys::net::recv_fd(spawner.as_fd()).map_err(|e| Error::Keeper {
            reason: format!("a cell's channel wasn't handed over: {e}"),
        })?;
        let channel = UnixStream::from(channel);
        let kept = Arc::clone(kept);
        std::thread::spawn(move || {
            if let Err(error) = serve_cell(&channel, &kept, &named) {
                tracing::warn!(cell = %named.cell, %error, "a cell's vault channel ended");
            }
        });
    }
    Ok(())
}

/// Answers a cell on its own channel: vault ops alone, each a session
/// request, under the grants the channel was named with, whatever the
/// cell says. Anything else, or a malformed op, ends the channel.
fn serve_cell(stream: &UnixStream, kept: &Mutex<Kept>, cell: &CellChannel) -> Result<(), Error> {
    while let Some(frame) = read_frame_or_eof(stream).map_err(keeper_gone)? {
        let frame = Zeroizing::new(frame);
        let Ok(SessionRequest::Vault(op)) = serde_json::from_slice(&frame) else {
            return Err(Error::SessionProtocol {
                reason: String::from("a cell's vault channel carries vault ops alone"),
            });
        };
        let mut guard = kept.lock().map_err(|_| Error::Keeper {
            reason: String::from("an earlier answer panicked"),
        })?;
        let result = cell_op_result(&mut guard, &op, &cell.cell, cell.uid, &cell.grants);
        drop(guard);
        let result = result.map_err(|reason| Error::SessionProtocol { reason })?;
        let reply = crate::serve::encode_reply(&SessionResponse::Vault(result), MAX_FRAME_BYTES)?;
        write_frame(stream, &reply).map_err(keeper_gone)?;
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

    /// A grant is named to the keeper as `action:name`, and a session's
    /// grants keep only those of the keeper's they name: never one the
    /// keeper's configuration doesn't make.
    #[test]
    fn a_sessions_grants_narrow_the_keepers_and_never_widen_them() {
        use hatchling_core::VaultAction;
        let grant = |action, name: &str| VaultGrant {
            action,
            name: name.parse().unwrap(),
        };
        assert_eq!(
            grant_word(&grant(VaultAction::Store, "token")),
            "store:token"
        );
        let mut vault = crate::api_auth::tests::owner_config(None).vault;
        vault.grants = vec![
            grant(VaultAction::Fetch, "token"),
            grant(VaultAction::Store, "token"),
        ];
        let held = |words: &[&str]| {
            let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
            held_by(&vault, &words).grants
        };
        assert_eq!(held(&["store:token"]), [grant(VaultAction::Store, "token")]);
        assert_eq!(held(&["fetch:token", "store:token"]), vault.grants);
        assert!(held(&[]).is_empty());
        assert!(held(&["fetch:other", "remove:token"]).is_empty());
    }

    /// Security keys cross to the keeper as `node=descriptor` pairs, and
    /// are adopted there; a pair that isn't one, or a descriptor that
    /// isn't a device, is left out. A machine without keys hands none.
    #[test]
    fn security_keys_cross_to_the_keeper_by_their_descriptors() {
        use std::os::fd::IntoRawFd as _;
        let null = || std::fs::File::open("/dev/null").unwrap();
        let (one, four) = (null(), null());
        let expected = format!("hidraw1={},hidraw4={}", one.as_raw_fd(), four.as_raw_fd());
        let keys = vec![
            (String::from("hidraw1"), one),
            (String::from("hidraw4"), four),
        ];
        assert_eq!(keys_env(&keys), expected);
        let raw = null().into_raw_fd();
        let (pipe, _write) = hatchling_sys::pipe().unwrap();
        let adopted = handed_keys(&format!(
            "hidraw2={raw},nonsense,hidraw3=x,hidraw5={},",
            pipe.as_raw_fd()
        ));
        let found: Vec<(&str, i32)> = adopted
            .iter()
            .map(|(node, file)| (node.as_str(), file.as_raw_fd()))
            .collect();
        assert_eq!(found, [("hidraw2", raw)]);
        assert!(handed_keys("").is_empty());
        if !Path::new("/sys/class/hidraw").exists() {
            assert!(open_security_keys().is_empty());
        }
    }

    fn names(list: &[&str]) -> Vec<SecretName> {
        list.iter().map(|name| name.parse().unwrap()).collect()
    }

    /// A service user is looked for only as root, where it must exist and
    /// not be root; not as root, there is none.
    #[test]
    fn a_service_user_is_root_modes_alone() {
        let found = service_user("root", "a part");
        let missing = service_user("no-such-hatchling-user", "a part");
        if hatchling_sys::euid() != 0 {
            assert_eq!(found, Ok(None));
            assert_eq!(missing, Ok(None));
            return;
        }
        assert!(found.unwrap_err().contains("is root"));
        let missing = missing.unwrap_err();
        assert!(
            missing.contains("gives a part a user of its own") && missing.contains("useradd"),
            "{missing}"
        );
    }

    /// The keeper's user is looked for only as root, where its absence
    /// stops the broker, saying how to make it; not as root, the keeper is
    /// this process's user, and an operator's command acts for no one.
    #[test]
    fn the_keepers_user_is_root_modes_alone() {
        let config = crate::api_auth::tests::owner_config(None);
        if hatchling_sys::euid() != 0 {
            assert_eq!(keeper_user().unwrap(), None);
            act_for_keeper(&config).unwrap();
            return;
        }
        match hatchling_sys::subid::local_account(KEEPER_USER).unwrap() {
            None => {
                let refused = keeper_user().unwrap_err().to_string();
                assert!(refused.contains("useradd --system"), "{refused}");
                assert!(act_for_keeper(&config).is_err());
            }
            Some(ids) => assert_eq!(keeper_user().unwrap(), Some(ids)),
        }
    }

    /// The keeper's directories: the vault's, and those of its key file,
    /// keyring and KEK, each once.
    #[test]
    fn the_keepers_directories_are_the_vaults_and_its_keys() {
        let vault = VaultConfig {
            root: PathBuf::from("/srv/h/vault"),
            key_file: PathBuf::from("/srv/h/vault.key/key"),
            grants: Vec::new(),
        };
        assert_eq!(
            keeper_dirs(&vault, &KeyringConfig::default()),
            [
                PathBuf::from("/srv/h/vault"),
                PathBuf::from("/srv/h/vault.key")
            ]
        );
        let keyring = KeyringConfig {
            keyring: Some(PathBuf::from("/srv/h/ring/keyring")),
            kek_file: Some(PathBuf::from("/srv/h/kek/kek")),
            ..KeyringConfig::default()
        };
        assert_eq!(
            keeper_dirs(&vault, &keyring),
            [
                PathBuf::from("/srv/h/vault"),
                PathBuf::from("/srv/h/vault.key"),
                PathBuf::from("/srv/h/ring"),
                PathBuf::from("/srv/h/kek"),
            ]
        );
    }

    /// The keeper's directories are made if absent and handed over; one
    /// below a directory the keeper's user may not search is refused,
    /// naming that directory, before anything changes.
    #[test]
    fn a_directory_is_handed_to_the_keeper_only_where_it_can_reach() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let base =
            std::env::temp_dir().join(format!("hatchling-keeper-hand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o711)).unwrap();
        let (uid, gid) = (hatchling_sys::euid(), rustix::process::getegid().as_raw());
        let vault = base.join("vault");
        hand_to(KEEPER_USER, std::slice::from_ref(&vault), uid, gid).unwrap();
        let made = std::fs::symlink_metadata(&vault).unwrap();
        assert!(made.is_dir());
        assert_eq!((made.uid(), made.mode() & 0o777), (uid, 0o700));
        // What isn't the user's yet is counted, and nothing else.
        std::fs::write(vault.join("secret"), b"s").unwrap();
        assert_eq!(not_theirs(&vault, uid, gid, 2), 0);
        assert_eq!(not_theirs(&vault, uid.wrapping_add(4242), gid, 2), 2);
        assert_eq!(not_theirs(&vault, uid, gid.wrapping_add(4242), 2), 2);
        assert_eq!(not_theirs(&base.join("absent"), uid, gid, 2), 1);
        assert_eq!(
            not_theirs(&vault, uid, gid, 1),
            1,
            "past its depth, counted"
        );
        assert!(searchable(
            &base,
            uid.wrapping_add(4242),
            gid.wrapping_add(4242)
        ));
        let closed = base.join("closed");
        std::fs::create_dir(&closed).unwrap();
        std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o700)).unwrap();
        let other = (uid.wrapping_add(4242), gid.wrapping_add(4242));
        assert!(searchable(&closed, uid, gid), "its owner searches it");
        assert!(!searchable(&closed, other.0, other.1));
        let refused =
            hand_to("a-service-user", &[closed.join("vault")], other.0, other.1).unwrap_err();
        assert!(refused.contains(&closed.display().to_string()), "{refused}");
        assert!(
            refused.contains("a-service-user can't reach it"),
            "{refused}"
        );
        assert!(!closed.join("vault").exists(), "nothing was made");
        assert!(!searchable(&base.join("absent"), uid, gid));
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// The keeper asks the broker for its stores' keys as they keep them,
    /// and unwraps what the broker read; an answer that isn't one is
    /// refused, and so is an asking for the vault's own key.
    #[test]
    fn the_keeper_unwraps_the_keys_the_broker_reads_of_its_stores() {
        use hatchling_vault::SealedVault;
        let master = MasterKey::generate().unwrap();
        let root =
            std::env::temp_dir().join(format!("hatchling-keeper-asks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        SealedVault::open_in(&root, &master, Store::SPACES).unwrap();
        let stores = vec![(Store::SPACES, root.clone())];
        let (keeper, broker) = UnixStream::pair().unwrap();
        for end in [&keeper, &broker] {
            end.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        }
        let answering = std::thread::spawn(move || {
            let asking = read_frame(&broker).unwrap();
            let (kind, asking) = asking.split_first().unwrap();
            assert_eq!(*kind, ASKING);
            let asked = hatchling_vault::handoff::asked(asking).unwrap();
            write_frame(&broker, &kept_keys(&asked).unwrap()).unwrap();
            // Asked again, it answers something else.
            read_frame(&broker).unwrap();
            write_frame(&broker, b"no").unwrap();
        });
        let keys = broker_keys(&keeper, &master, &stores).unwrap();
        assert_eq!(
            keys.stores().collect::<Vec<_>>(),
            [(Store::SPACES, root.as_path())]
        );
        assert!(SealedVault::open_in(&root, &keys, Store::SPACES).is_ok());
        assert!(matches!(
            broker_keys(&keeper, &master, &stores),
            Err(Error::Vault { .. })
        ));
        answering.join().unwrap();
        assert!(matches!(
            kept_keys(&[(Store::SECRETS, root.clone())]),
            Err(Error::Keeper { .. })
        ));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Each purpose reads only the names the configuration gives it, and
    /// an unknown purpose reads nothing.
    #[test]
    fn a_purpose_reads_only_what_the_configuration_gives_it() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.egress.grants = vec![String::from("api.example.com:443")];
        config.vault.grants = vec![VaultGrant {
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
        // Never an egress credential: the authority's alone.
        assert_eq!(scanned(&[]), ["mail-work"]);
        assert_eq!(
            scanned(&["token", "harness-claude-code"]),
            ["mail-work", "token"]
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
        let gone = unexpected(&Reply::Scan { count: 0 });
        assert!(gone.to_string().contains("(scan)"), "{gone}");
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
        let master = MasterKey::generate().unwrap();
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
        config.vault.grants = vec![VaultGrant {
            action: hatchling_core::VaultAction::Fetch,
            name: "token".parse().unwrap(),
        }];
        let master = MasterKey::generate().unwrap();
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

    /// A cell's own channel, handed over by the spawner and named with
    /// its cell, uid and grants, is answered vault ops alone, under those
    /// grants: a channel named with none is refused, and anything but a
    /// vault op ends it. A cell id that doesn't parse is a malformed op,
    /// and a naming frame that doesn't parse stops the spawner's channel.
    #[test]
    fn a_cells_own_channel_is_answered_vault_ops_under_its_grants() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-cell-channels");
        config.vault.root = dir.0.join("vault");
        config.vault.grants = vec![VaultGrant {
            action: hatchling_core::VaultAction::Fetch,
            name: "token".parse().unwrap(),
        }];
        let master = MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&config.vault.root, &master)
            .unwrap()
            .store(
                &"token".parse().unwrap(),
                &Secret::new(b"t".to_vec()).unwrap(),
            )
            .unwrap();
        let keys = StoreKeys::unwrap(&master, &[(Store::SECRETS, &config.vault.root)]).unwrap();
        let kept = Arc::new(Mutex::new(Kept {
            releases: Releases::of(&config, BTreeSet::new()),
            config,
            keys,
            released: BTreeSet::new(),
        }));
        let fetch = SessionRequest::Vault(VaultOp::Fetch {
            name: String::from("token"),
        });
        let (spawners, keepers) = UnixStream::pair().unwrap();
        let taking = std::thread::spawn({
            let kept = Arc::clone(&kept);
            move || take_cells(&keepers, &kept)
        });
        let hand = |grants: &[&str]| {
            let (ours, theirs) = UnixStream::pair().unwrap();
            ours.set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let named = serde_json::json!({ "cell": "run-1", "uid": 7, "grants": grants });
            write_frame(&spawners, named.to_string().as_bytes()).unwrap();
            hatchling_sys::net::send_fd(spawners.as_fd(), theirs.as_fd()).unwrap();
            ours
        };
        let ask = |channel: &UnixStream, request: &[u8]| {
            write_frame(channel, request).unwrap();
            read_frame_or_eof(channel)
                .unwrap()
                .map(|reply| serde_json::from_slice::<SessionResponse>(&reply).unwrap())
        };
        let fetching = serde_json::to_vec(&fetch).unwrap();
        let granted = hand(&["fetch:token"]);
        let Some(SessionResponse::Vault(VaultResult::Fetched { secret_b64 })) =
            ask(&granted, &fetching)
        else {
            unreachable!("the granted fetch wasn't answered with the secret");
        };
        assert_eq!(secret_b64.decode().unwrap().as_slice(), b"t");
        let ungranted = hand(&[]);
        assert!(matches!(
            ask(&ungranted, &fetching),
            Some(SessionResponse::Vault(VaultResult::Denied))
        ));
        assert!(
            ask(&ungranted, b"{}").is_none(),
            "a request that isn't a vault op"
        );
        // A cell that leaves ends its channel quietly.
        let (gone, left) = UnixStream::pair().unwrap();
        drop(gone);
        let named = CellChannel {
            cell: String::from("run-2"),
            uid: 7,
            grants: Vec::new(),
        };
        assert!(serve_cell(&left, &kept, &named).is_ok());
        let mut guard = kept.lock().unwrap();
        let op = VaultOp::Fetch {
            name: String::from("token"),
        };
        assert!(cell_op_result(&mut guard, &op, "", 7, &[]).is_err());
        drop(guard);
        write_frame(&spawners, b"{}").unwrap();
        assert!(
            taking.join().unwrap().is_err(),
            "a naming frame that doesn't parse"
        );
        // The granted channel is still answered after the spawner's ended.
        assert!(matches!(
            ask(&granted, &fetching),
            Some(SessionResponse::Vault(VaultResult::Fetched { .. }))
        ));
    }

    /// With a keeper running, a session's vault op sent to the broker is
    /// denied, whatever its grants: it is the keeper's, on the cell's own
    /// channel, and the broker reads no secret for it. Without one, the
    /// broker answers it as before.
    #[test]
    fn a_vault_op_sent_to_the_broker_is_denied_when_a_keeper_runs() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-session-denied");
        config.vault.root = dir.0.join("vault");
        config.vault.grants = vec![VaultGrant {
            action: hatchling_core::VaultAction::Fetch,
            name: "token".parse().unwrap(),
        }];
        let master = MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&config.vault.root, &master)
            .unwrap()
            .store(
                &"token".parse().unwrap(),
                &Secret::new(b"t".to_vec()).unwrap(),
            )
            .unwrap();
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let keeper = Keeper::over(ours).unwrap();
        let op = VaultOp::Fetch {
            name: String::from("token"),
        };
        let cell: CellId = "run-1".parse().unwrap();
        let through = |keeper: Option<&Keeper>| {
            crate::secrets::vault_op_through(keeper, &config.vault, &master, &op, &cell, 7)
                .unwrap()
        };
        assert_eq!(through(Some(&keeper)), VaultResult::Denied);
        assert!(matches!(through(None), VaultResult::Fetched { .. }));
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
        let master = MasterKey::generate().unwrap();
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
            // A cell's vault op isn't a request the broker can make.
            assert!(
                serde_json::from_str::<Request>(r#"{"request":"cell","cell":"c","uid":7}"#)
                    .is_err()
            );
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
            VaultGrant {
                action: hatchling_core::VaultAction::Fetch,
                name: "token".parse().unwrap(),
            },
            VaultGrant {
                action: hatchling_core::VaultAction::Remove,
                name: "old".parse().unwrap(),
            },
        ];
        let master = MasterKey::generate().unwrap();
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
            releases_to_cell(&config.vault, &fetch("token")).as_deref(),
            Some("token")
        );
        assert_eq!(releases_to_cell(&config.vault, &fetch("other")), None);
        let remove = VaultOp::Remove {
            name: String::from("old"),
        };
        assert_eq!(releases_to_cell(&config.vault, &remove), None);
        // A session that doesn't hold the grant releases nothing.
        assert_eq!(
            releases_to_cell(&held_by(&config.vault, &[]), &fetch("token")),
            None
        );
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
