//! The vault keeper (WO-017, step 4): the process that holds the master
//! key, the secrets store's key and the sign-ins' (WO-031), so that the
//! broker holds none of them.
//!
//! [`start`] runs first in the broker's start, before it touches a key. It
//! starts the broker's own binary again as `vault-keeper`, joined to the
//! broker by a socket pair. The keeper reads the configuration from its
//! own environment, the same as the broker's, and the operator's terminal
//! stays its own for a keyring's prompts. It loads the master key, unwraps
//! every sealed store's key, and hands the broker the keys of the
//! broker's own stores: every one but the secrets and the sign-ins. Then
//! it drops the master key and answers the broker until the broker goes:
//!
//! - **a read for a purpose the asker's channel may read**, when every name
//!   is one the configuration gives that purpose: for the broker, a channel
//!   account's credential; for the egress authority, on its own channel, an
//!   egress destination's credential, search's key or a configured harness
//!   vendor's key;
//! - **a gate's scan**, for the broker: the values the keeper chooses
//!   (`Releases::scan_set`);
//! - **a cell's vault op**, checked against the configuration's grants;
//! - **the sign-ins** ([`crate::browser_credentials`]): the owner's keep,
//!   list and delete, and a fill;
//! - **a check of a text** ([`Keeper::contains`]): whether it holds any
//!   secret the keeper holds ([`crate::held_check`]), for the broker alone.
//!
//! On the spawner's own channel it takes each cell's vault channel, and
//! answers one thing more: **a room disk's key** (0.9), derived from the
//! master key ([`hatchling_vault::RoomRootKey`]) and written into the pipe
//! the spawner hands it, whose other end is cryptsetup's. The spawner and
//! the broker never hold it, and nothing else may ask for it: on the
//! broker's channel and the authority's it isn't a request at all.
//!
//! A read, a cell's op, a fill and a check are each recorded in the
//! vault's access log before the keeper answers. One that released or
//! changed something and can't be recorded is answered with nothing, or as
//! failed; one that did neither, a denial or a failure, still gets its
//! answer, and that it wasn't recorded goes to the keeper's log
//! ([`crate::secrets`]). A read the
//! configuration doesn't give its purpose is refused and recorded as
//! denied. Keeping, listing and deleting a sign-in release no secret, and
//! aren't recorded there.
//!
//! **The check is an oracle, and the access log is what answers that.**
//! Whoever can make the broker ask (an agent naming a harness's task, a
//! compromised broker) can try a guess at a secret and read the boolean.
//! So it says as little as it can: one answer for the whole text, whole
//! values only, a short value only against a whole text
//! ([`crate::held_check`]), never which secret or where. A check can't be
//! prevented without giving the broker the secrets, so it's made visible:
//! each is one line, who, what for, in which cell's run, when, how many
//! bytes and what came of it, and repeated slightly different submissions
//! stand out there (`hatchling-broker vault-audit log`). No line holds a
//! value or a name. What it was for is one of a fixed set
//! ([`crate::held_check::CheckFor`]), and a cell's id is held to what a
//! [`CellId`] may be: nothing else the channel carries reaches a line.
//!
//! **What it holds against.** The broker never has the master key or the
//! secrets store's key: a broker that is compromised can't read the
//! vault's files, a copy of them or a backup, and gets only the secrets
//! the configuration names, each release recorded by a process it
//! doesn't share memory with. The keeper isn't dumpable, so a process of
//! the same user without `CAP_SYS_PTRACE` can't read its memory. A broker
//! run as root starts it as [`KEEPER_USER`], its own user, and hands the
//! vault to that user; not run as root, it's the broker's own user, and
//! not dumpable is all that stands between them. Root can reach any
//! process either way.

use std::collections::{BTreeMap, BTreeSet};
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::ui::VaultProtection;
use hatchling_core::{
    CellId, MAX_FRAME_BYTES, SecretName, SessionRequest, SessionResponse, VaultOp, VaultResult,
};
use hatchling_vault::{MasterKey, RoomImageId, RoomRootKey, Secret, Store, StoreKeys};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::browser_credentials::{Credentials, SignInAnswer, SignInRequest};
use crate::held_check::CheckFor;
use crate::secrets::{Secrets, recorded_vault_op};
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
    /// An operation on the sign-ins (WO-031).
    SignIn { op: SignInRequest },
    /// Fill the sign-in `id` into the browser cell `cell`, whose page the
    /// broker says is on `origin` (WO-031): sent to the cell, never to the
    /// broker.
    Fill {
        id: String,
        cell: String,
        origin: String,
    },
    /// Who can open the vault: how, and how many, for the app's checklist.
    Protection,
    /// Does a text of `bytes` bytes hold a secret? Answered [`Reply::Ready`]
    /// or [`Reply::Refused`] before any of it is sent. After `Ready`, it
    /// follows raw, in frames of its own, however many that takes: the
    /// keeper reads it whole, so a secret across two frames is one text.
    /// It's for `check`, and if it's a run's, for that run's `cell`.
    Contains {
        bytes: u64,
        check: CheckFor,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cell: Option<String>,
    },
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
    /// The sign-ins' answer.
    SignIn { answer: SignInAnswer },
    /// The fill was sent to the cell, as `nonce`: the broker names it to
    /// the cell's controller.
    Filling { nonce: String },
    /// Who can open the vault. It holds nothing that opens it.
    Protection { protection: VaultProtection },
    /// Send the text.
    Ready,
    /// Whether the text held a secret, and nothing else: not which, how
    /// many, or where.
    Contains { found: bool },
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

/// How long the broker waits for a check's answer once it has sent the
/// text: the keeper looks through all of it, for every form of every
/// secret. A check as long as a run can ask takes seconds, not this.
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);

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
    install(keeper)?;
    Ok((keys, authority, spawner))
}

/// Makes `keeper` the one this broker asks: once, as [`start`] does, and
/// for a test's own process, which has no broker binary to start.
///
/// # Errors
///
/// [`Error::Keeper`] when a keeper is already running.
pub fn install(keeper: Keeper) -> Result<(), Error> {
    KEEPER.set(keeper).map_err(|_| Error::Keeper {
        reason: String::from("a keeper is already running"),
    })
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

/// The directories the keeper's user owns: the vault's, the sign-ins'
/// when they're kept (WO-031), its key file's, its keyring's and its file
/// tier's KEK's, each once.
#[must_use]
pub fn keeper_dirs(config: &Config, keyring: &KeyringConfig) -> Vec<PathBuf> {
    let vault = &config.vault;
    let mut dirs = vec![vault.root.clone()];
    if !config.web.credentials.as_os_str().is_empty() {
        dirs.push(config.web.credentials.clone());
    }
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
    // Where it waits to be unlocked, when its vault needs a person: with
    // any keyring, since root can't read the keeper's keyring here to know
    // its tier (it opens only its own), and the directory costs nothing.
    if keyring.keyring.is_some() {
        dirs.push(crate::unlock_relay::unlock_dir(vault));
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
pub(crate) fn open_security_keys() -> Vec<(String, std::fs::File)> {
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
    let dirs = keeper_dirs(config, &KeyringConfig::from_env()?);
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
                    &keeper_dirs(&config, &KeyringConfig::from_env()?),
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
        Self::bounded(stream, ANSWER_TIMEOUT)
    }

    /// [`Self::over`], each read and write bounded by `bound`: a keeper
    /// that stops reading a text mustn't hold the broker either.
    fn bounded(stream: UnixStream, bound: Duration) -> Result<Self, Error> {
        stream
            .set_read_timeout(Some(bound))
            .and_then(|()| stream.set_write_timeout(Some(bound)))
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
            other @ (Reply::Scan { .. }
            | Reply::SignIn { .. }
            | Reply::Filling { .. }
            | Reply::Protection { .. }
            | Reply::Ready
            | Reply::Contains { .. }) => return Err(unexpected(&other)),
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
            other @ (Reply::Read { .. }
            | Reply::SignIn { .. }
            | Reply::Filling { .. }
            | Reply::Protection { .. }
            | Reply::Ready
            | Reply::Contains { .. }) => Err(unexpected(&other)),
        }
    }

    /// `op` on the sign-ins, which the keeper holds (WO-031): its answer,
    /// which never holds a sign-in's secrets.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper refuses it or can't be
    /// reached.
    pub fn sign_ins(&self, op: SignInRequest) -> Result<SignInAnswer, Error> {
        let request = Request::SignIn { op };
        match self.exchange(|stream| ask(stream, &request))? {
            Reply::SignIn { answer } => Ok(answer),
            Reply::Refused { reason } => Err(Error::Keeper { reason }),
            other @ (Reply::Read { .. }
            | Reply::Scan { .. }
            | Reply::Filling { .. }
            | Reply::Protection { .. }
            | Reply::Ready
            | Reply::Contains { .. }) => Err(unexpected(&other)),
        }
    }

    /// Who can open the vault: how, and how many, as the keyring says now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper can't say, or can't be
    /// reached.
    pub fn protection(&self) -> Result<VaultProtection, Error> {
        match self.exchange(|stream| ask(stream, &Request::Protection))? {
            Reply::Protection { protection } => Ok(protection),
            Reply::Refused { reason } => Err(Error::Keeper { reason }),
            other @ (Reply::Read { .. }
            | Reply::Scan { .. }
            | Reply::SignIn { .. }
            | Reply::Filling { .. }
            | Reply::Ready
            | Reply::Contains { .. }) => Err(unexpected(&other)),
        }
    }

    /// Whether `text` holds a secret the keeper holds: any of them, a cell
    /// having had it or not ([`crate::held_check`]). The keeper reads every
    /// value itself and the answer is this one boolean: the broker learns
    /// no value, no name and no position. Recorded by the keeper, as for
    /// `check`, in the run of `cell` if it's a run's.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper refuses the check, can't
    /// make it, or can't be reached: nothing is known, and a caller treats
    /// that as holding one.
    pub fn contains(
        &self,
        check: CheckFor,
        cell: Option<&CellId>,
        text: &[u8],
    ) -> Result<bool, Error> {
        let request = Request::Contains {
            bytes: u64::try_from(text.len()).unwrap_or(u64::MAX),
            check,
            cell: cell.map(|cell| cell.as_str().to_owned()),
        };
        let reply = self.exchange(|stream| match ask(stream, &request)? {
            Reply::Ready => {
                for frame in text.chunks(MAX_FRAME_BYTES) {
                    write_frame(stream, frame).map_err(keeper_gone)?;
                }
                let bound =
                    |timeout: Duration| stream.set_read_timeout(Some(timeout)).map_err(keeper_gone);
                bound(CHECK_TIMEOUT)?;
                let reply = receive(stream)?;
                bound(ANSWER_TIMEOUT)?;
                Ok(reply)
            }
            refused @ Reply::Refused { .. } => Ok(refused),
            other @ (Reply::Read { .. }
            | Reply::Scan { .. }
            | Reply::SignIn { .. }
            | Reply::Filling { .. }
            | Reply::Protection { .. }
            | Reply::Contains { .. }) => Err(unexpected(&other)),
        })?;
        match reply {
            Reply::Contains { found } => Ok(found),
            Reply::Refused { reason } => Err(Error::Keeper { reason }),
            other @ (Reply::Read { .. }
            | Reply::Scan { .. }
            | Reply::SignIn { .. }
            | Reply::Filling { .. }
            | Reply::Protection { .. }
            | Reply::Ready) => Err(unexpected(&other)),
        }
    }

    /// Asks the keeper to fill the sign-in `id` into the browser cell
    /// `cell`, whose page is on `origin` (WO-031): the fill's nonce, which
    /// the broker names to the cell's controller. The sign-in's secrets go
    /// to the cell alone.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper refuses it (the sign-in
    /// isn't that browser's profile's, or kept for `origin`, or there's no
    /// such browser) or can't be reached.
    pub fn fill(&self, id: &str, cell: &str, origin: &str) -> Result<String, Error> {
        let request = Request::Fill {
            id: id.to_owned(),
            cell: cell.to_owned(),
            origin: origin.to_owned(),
        };
        match self.exchange(|stream| ask(stream, &request))? {
            Reply::Filling { nonce } => Ok(nonce),
            Reply::Refused { reason } => Err(Error::Keeper { reason }),
            other @ (Reply::Read { .. }
            | Reply::Scan { .. }
            | Reply::SignIn { .. }
            | Reply::Protection { .. }
            | Reply::Ready
            | Reply::Contains { .. }) => Err(unexpected(&other)),
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
    receive(stream)
}

/// Reads a reply.
fn receive(stream: &UnixStream) -> Result<Reply, Error> {
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
        if crate::vault_ops::KEEPERS_STORES.contains(store) {
            return Err(Error::Keeper {
                reason: format!(
                    "it asked for the key of {}, which is its own",
                    store.label()
                ),
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
        Reply::SignIn { .. } => "sign-in",
        Reply::Filling { .. } => "filling",
        Reply::Protection { .. } => "protection",
        Reply::Ready => "ready",
        Reply::Contains { .. } => "contains",
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
    /// which the broker handles itself. A held secret no cell was given
    /// isn't one Hatchling handed it. The egress destinations' credentials
    /// are the egress authority's alone (WO-017, step 2), so a scan never
    /// hands the broker one. That is what a scan knows, not all a cell can
    /// know: the same value in a file it can read isn't looked for here,
    /// and only the harness's check ([`Keeper::contains`]) covers that.
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
    /// The keys of the secrets store and the sign-ins': the only stores'
    /// keys it keeps.
    keys: StoreKeys,
    /// The room disks' root key, derived from the master key at the start
    /// (0.9): each image's key is derived from it, for the spawner alone.
    room: RoomRootKey,
    releases: Releases,
    /// The secrets a cell has been given or has stored
    /// ([`RELEASED_RECORD`]).
    released: BTreeSet<String>,
    /// The channels of the browser cells started for a profile's sign-ins,
    /// by cell (WO-031), until each cell ends its own.
    browsers: BTreeMap<String, BrowserChannel>,
}

/// A browser cell's vault channel, as the keeper holds it for fills.
struct BrowserChannel {
    /// The profile the cell was started for: only its sign-ins are filled.
    profile: String,
    /// The keeper's end, written by fills alone.
    stream: UnixStream,
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
    // The vault and the sign-ins are the keeper's alone; the other stores
    // are the broker's, which checks them, and whose keys it asks the
    // broker for.
    let (vault, brokers): (Vec<_>, Vec<_>) = crate::sealed_stores(&config, redaction.as_ref())
        .into_iter()
        .partition(|(store, _)| crate::vault_ops::KEEPERS_STORES.contains(store));
    crate::check_sealed_stores(&vault)?;
    // Started with nobody at its terminal, a vault that needs a person
    // waits, locked, to be unlocked (crate::unlock_relay).
    let master = crate::unlock_relay::master_key_for_start(&config.vault)?;
    crate::validate_sealed_vault(&config.vault, &master)?;
    let handed = broker_keys(stream, &master, &brokers)?
        .to_bytes()
        .map_err(|source| Error::Vault { source })?;
    // The last moment the master key is here: the room disks' keys are
    // derived from it, then it goes.
    let room = RoomRootKey::of(&master);
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
        room,
        releases: rules,
        released,
        browsers: BTreeMap::new(),
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

/// Records `name` as released, in `released` and in the store under
/// `config` opened with `keys`, before the cell stores it. A record that
/// can't be kept releases nothing.
fn record_release(
    config: &Config,
    keys: &StoreKeys,
    released: &mut BTreeSet<String>,
    name: &str,
) -> Result<(), Error> {
    if released.contains(name) {
        return Ok(());
    }
    let mut now = released.clone();
    now.insert(name.to_owned());
    let record = serde_json::to_vec(&now).map_err(|e| Error::Keeper {
        reason: format!("cannot write the released record: {e}"),
    })?;
    let secret = Secret::new(record).map_err(|source| Error::Vault { source })?;
    let record_name: SecretName = RELEASED_RECORD.parse().map_err(|_| Error::Keeper {
        reason: String::from("the released record's name doesn't parse"),
    })?;
    hatchling_vault::SealedVault::open(&config.vault.root, keys)
        .and_then(|vault| vault.store(&record_name, &secret))
        .map_err(|source| Error::Vault { source })?;
    *released = now;
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
            if let Request::Contains { bytes, check, .. } = request {
                refuse_on_record(kept, role, check, bytes)?;
            }
            send(
                stream,
                &Reply::Refused {
                    reason: String::from("the egress authority only reads"),
                },
            )?;
            continue;
        }
        // Before the lock: reading a text takes as long as the broker takes
        // to send it, and looking through it as long as it's long.
        if let Request::Contains { bytes, check, cell } = request {
            answer_contains(stream, kept, role, (bytes, check), cell.as_deref())?;
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
            Request::SignIn { op } => answer_sign_in(stream, kept, op)?,
            Request::Fill { id, cell, origin } => answer_fill(stream, kept, &id, &cell, &origin)?,
            Request::Protection => answer_protection(stream)?,
            // Answered above.
            Request::Contains { .. } => {}
        }
        drop(guard);
    }
    Ok(())
}

/// A cell's vault op, answered under the grants its session holds where
/// the keeper's configuration makes them too, and recorded as that
/// cell's. A store is recorded as released just before it's written, once
/// it's granted, its value reads and the vault opens: one whose release
/// can't be recorded fails. Nothing else releases a name, so a malformed
/// store doesn't put a value no cell had into the gates' scan, and every
/// op leaves a line, as failed if that's how it went.
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
    let vault = held_by(&kept.config.vault, grants);
    let Ok(cell) = cell.parse::<CellId>() else {
        let caller = Caller::Session {
            uid,
            cell: String::from("(not a cell id)"),
        };
        let action = op.action().to_string();
        if let Err(error) =
            crate::vault_access::record(&vault.root, caller, &action, Vec::new(), Outcome::Failed)
        {
            tracing::error!(%error, "a vault op from a bad cell id not recorded");
        }
        return Err(String::from("a cell id that doesn't parse"));
    };
    let Kept {
        config,
        keys,
        released,
        ..
    } = kept;
    let mut release = |name: &SecretName| {
        let recorded = record_release(config, keys, released, name.as_str());
        if let Err(error) = &recorded {
            tracing::error!(%error, "a release not recorded: the op is refused");
        }
        recorded.is_ok()
    };
    match recorded_vault_op(&vault, keys, op, &cell, uid, &mut release) {
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
/// session holds (each `action:name`); or, for a browser cell, the profile
/// it was started for, whose sign-ins may be filled into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CellChannel {
    pub cell: String,
    pub uid: u32,
    pub grants: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

/// What the spawner hands the keeper on its channel: a frame naming it,
/// then a descriptor. The two shapes share no field, and each refuses any
/// field it doesn't have, so a frame is one or the other, or neither, and
/// a cell's frame is written as it always was.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FromSpawner {
    /// A cell's vault channel, named.
    Cell(CellChannel),
    /// An ask for a room disk's key (0.9): the descriptor is the writing
    /// end of the pipe it goes into.
    RoomKey(RoomKeyAsk),
}

/// The spawner's ask for a room disk's key: which image, by the LUKS UUID
/// the spawner read from it or is making it with, and what for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoomKeyAsk {
    pub image: String,
    pub room_key: RoomKeyFor,
}

/// What a room disk's key is asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RoomKeyFor {
    /// To make a new image with.
    Create,
    /// To open one.
    Open,
}

impl RoomKeyFor {
    /// What it's for, as the access log says it.
    const fn purpose(self) -> &'static str {
        match self {
            Self::Create => "room disk create",
            Self::Open => "room disk open",
        }
    }
}

/// The action a room disk's key is recorded as.
const ROOM_KEY_ACTION: &str = "room-key";

/// How long a fill may take to be written to a browser cell's channel: a
/// cell that doesn't read it fails the fill, and doesn't hold the keeper.
const FILL_TIMEOUT: Duration = Duration::from_secs(5);

/// Takes each cell's channel the spawner hands over, named first, and
/// answers it on a thread of its own, until the spawner goes; and answers
/// each ask for a room disk's key ([`answer_room_key`]).
fn take_cells(spawner: &UnixStream, kept: &Arc<Mutex<Kept>>) -> Result<(), Error> {
    while let Some(frame) = read_frame_or_eof(spawner).map_err(keeper_gone)? {
        let named: FromSpawner = serde_json::from_slice(&frame).map_err(|e| Error::Keeper {
            reason: format!("a frame from the spawner that doesn't parse: {e}"),
        })?;
        let handed = hatchling_sys::net::recv_fd(spawner.as_fd()).map_err(|e| Error::Keeper {
            reason: format!("what the spawner named wasn't handed over: {e}"),
        })?;
        let named = match named {
            FromSpawner::Cell(named) => named,
            FromSpawner::RoomKey(asked) => {
                answer_room_key(kept, &asked, handed);
                continue;
            }
        };
        let channel = UnixStream::from(handed);
        if let Some(profile) = named.profile.clone() {
            if let Err(error) = hold_browser(channel, &named.cell, profile, kept) {
                tracing::warn!(cell = %named.cell, %error, "a browser cell's channel isn't held");
            }
            continue;
        }
        let kept = Arc::clone(kept);
        std::thread::spawn(move || {
            if let Err(error) = serve_cell(&channel, &kept, &named) {
                // Escaped: what ended it may quote what the cell sent.
                tracing::warn!(cell = %named.cell, ?error, "a cell's vault channel ended");
            }
        });
    }
    Ok(())
}

/// Answers the spawner's ask for a room disk's key, `asked`: the key goes
/// into `into`, the writing end of a pipe whose reading end is
/// cryptsetup's, and never to the spawner or the broker.
///
/// The image must be named by its canonical LUKS UUID, which the key is
/// derived for, and `into` must be a pipe: never a file, a socket or a
/// terminal, where the key would be kept or sent on. Each ask is recorded
/// in the vault's access log, as the spawner's, before anything is
/// written; one that can't be recorded writes nothing. Nothing comes back
/// on the spawner's channel: cryptsetup's own answer says whether the key
/// opened the image, and a refusal leaves it with nothing to read.
fn answer_room_key(kept: &Mutex<Kept>, asked: &RoomKeyAsk, into: OwnedFd) {
    let Ok(guard) = kept.lock() else {
        tracing::error!("a room disk's key wasn't given: an earlier answer panicked");
        return;
    };
    let root = guard.config.vault.root.clone();
    let recorded = |names: Vec<String>, outcome| {
        let caller = Caller::Spawner {
            purpose: asked.room_key.purpose().to_owned(),
        };
        crate::vault_access::record(&root, caller, ROOM_KEY_ACTION, names, outcome)
    };
    let denied = |names: Vec<String>, why: &str| {
        if let Err(error) = recorded(names, Outcome::Denied) {
            tracing::error!(%error, "a refused room disk's key not recorded");
        }
        tracing::warn!(why, "refused a room disk's key");
    };
    let Ok(image) = asked.image.parse::<RoomImageId>() else {
        return denied(
            vec![String::from("room disk (not an image's UUID)")],
            "not an image's UUID",
        );
    };
    let names = vec![format!("room disk {image}")];
    if !hatchling_sys::is_pipe(into.as_fd()) {
        return denied(names, "it would go somewhere other than a pipe");
    }
    if let Err(error) = recorded(names, Outcome::Allowed) {
        tracing::error!(%error, "a room disk's key not recorded: nothing is written");
        return;
    }
    let key = guard.room.image_key(&image);
    drop(guard);
    // Written whole or not at all: a pipe takes this much at once, and one
    // already full fails here rather than hold the keeper.
    if let Err(error) = hatchling_sys::write_whole_to_pipe(into, key.expose()) {
        tracing::warn!(%error, "a room disk's key couldn't be written into its pipe");
    }
}

/// Holds the channel of the browser cell `cell`, started for `profile`,
/// for the fills the broker asks of that profile's sign-ins (WO-031),
/// until the cell ends it. The cell sends nothing on it: anything it sends
/// ends it too. A second channel for a cell already held is refused.
fn hold_browser(
    channel: UnixStream,
    cell: &str,
    profile: String,
    kept: &Arc<Mutex<Kept>>,
) -> Result<(), Error> {
    let failed = |what: &str, error: std::io::Error| Error::Keeper {
        reason: format!("cannot {what}: {error}"),
    };
    channel
        .set_write_timeout(Some(FILL_TIMEOUT))
        .map_err(|error| failed("bound its fills", error))?;
    let watched = channel
        .try_clone()
        .map_err(|error| failed("watch it", error))?;
    let mut guard = kept.lock().map_err(|_| Error::Keeper {
        reason: String::from("an earlier answer panicked"),
    })?;
    if guard.browsers.contains_key(cell) {
        return Err(Error::Keeper {
            reason: String::from("a second channel for a browser cell already held"),
        });
    }
    guard.browsers.insert(
        cell.to_owned(),
        BrowserChannel {
            profile,
            stream: channel,
        },
    );
    drop(guard);
    let kept = Arc::clone(kept);
    let cell = cell.to_owned();
    std::thread::spawn(move || {
        use std::io::Read as _;
        // Until the cell ends the channel, or sends anything at all.
        let _ = (&watched).read(&mut [0u8; 1]);
        if let Ok(mut guard) = kept.lock() {
            guard.browsers.remove(&cell);
        }
    });
    Ok(())
}

/// Answers a cell on its own channel: vault ops alone, each a session
/// request, under the grants the channel was named with, whatever the
/// cell says. Anything else, or a malformed op, ends the channel.
fn serve_cell(stream: &UnixStream, kept: &Mutex<Kept>, cell: &CellChannel) -> Result<(), Error> {
    while let Some(frame) = read_frame_or_eof(stream).map_err(keeper_gone)? {
        let frame = Zeroizing::new(frame);
        // Read strictly, as the broker reads a cell (WO-027).
        if hatchling_core::strict_json::check(&frame).is_err() {
            return Err(Error::SessionProtocol {
                reason: String::from("a cell's vault op isn't strict JSON"),
            });
        }
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

/// Records a check for `check` that `role` wasn't given, of the `bytes` it
/// said it had. No cell: one a refused asker named isn't on the record.
fn refuse_on_record(
    kept: &Mutex<Kept>,
    role: Role,
    check: CheckFor,
    bytes: u64,
) -> Result<(), Error> {
    let root = vault_root(kept)?;
    let caller = role.caller(check.purpose());
    if let Err(error) =
        crate::vault_access::record_check(&root, caller, None, bytes, None, Outcome::Denied)
    {
        tracing::error!(%error, "a refused check not recorded");
    }
    Ok(())
}

/// The vault's root.
fn vault_root(kept: &Mutex<Kept>) -> Result<PathBuf, Error> {
    let guard = kept.lock().map_err(|_| Error::Keeper {
        reason: String::from("an earlier answer panicked"),
    })?;
    Ok(guard.config.vault.root.clone())
}

/// A text's size in memory, if it's one the keeper will read.
fn within_bound(bytes: u64) -> Option<usize> {
    if bytes > crate::held_check::MAX_CHECK_BYTES {
        return None;
    }
    usize::try_from(bytes).ok()
}

/// Reads a text of `bytes` bytes, in frames that add up to exactly that.
/// Wiped when dropped, and made at its full size, so no copy is left in a
/// freed buffer.
///
/// # Errors
///
/// [`Error::Keeper`] when a frame doesn't fit, which ends the channel, or
/// the broker goes.
fn read_text(stream: &UnixStream, bytes: usize) -> Result<Zeroizing<Vec<u8>>, Error> {
    let mut text = Zeroizing::new(Vec::with_capacity(bytes));
    while text.len() < bytes {
        let frame = Zeroizing::new(read_frame(stream).map_err(keeper_gone)?);
        if frame.is_empty() || frame.len() > bytes - text.len() {
            return Err(Error::Keeper {
                reason: String::from("a check's text isn't the size it said"),
            });
        }
        text.extend_from_slice(&frame);
    }
    Ok(text)
}

/// Answers one check of a text of `bytes` bytes for `check`, in `cell`'s run
/// if it's a run's: reads the text, looks for every secret held in it,
/// records the check, and says yes or no. The values are read here and
/// dropped here. One that's too long, or names what can't be a cell, is
/// refused before a byte of it comes, and one that can't be made or
/// recorded isn't answered: the broker treats that as holding a secret.
fn answer_contains(
    stream: &UnixStream,
    kept: &Mutex<Kept>,
    role: Role,
    (bytes, check): (u64, CheckFor),
    cell: Option<&str>,
) -> Result<(), Error> {
    use crate::held_check::{held_values, holds, on_record};
    let Ok(cell) = cell.map(str::parse::<CellId>).transpose() else {
        refuse_on_record(kept, role, check, bytes)?;
        return send(
            stream,
            &Reply::Refused {
                reason: String::from("the check names what can't be a cell"),
            },
        );
    };
    let Some(size) = within_bound(bytes) else {
        refuse_on_record(kept, role, check, bytes)?;
        return send(
            stream,
            &Reply::Refused {
                reason: String::from("the text is too long to check"),
            },
        );
    };
    send(stream, &Reply::Ready)?;
    let text = read_text(stream, size)?;
    let guard = kept.lock().map_err(|_| Error::Keeper {
        reason: String::from("an earlier answer panicked"),
    })?;
    let held = held_values(&guard.config, &guard.keys);
    let root = guard.config.vault.root.clone();
    drop(guard);
    let answer = held.map(|values| holds(&values, &text));
    match on_record(
        &root,
        role.caller(check.purpose()),
        cell.as_ref(),
        bytes,
        answer,
    ) {
        Ok(found) => send(stream, &Reply::Contains { found }),
        Err(reason) => {
            tracing::error!(%reason, "a check wasn't made or recorded: it isn't answered");
            send(
                stream,
                &Reply::Refused {
                    reason: String::from("the check couldn't be made"),
                },
            )
        }
    }
}

/// Answers the broker's `op` on the sign-ins, from the keeper's own store:
/// never with a sign-in's secrets, which go to the browser a fill is for
/// alone ([`answer_fill`]).
fn answer_sign_in(stream: &UnixStream, kept: &Kept, op: SignInRequest) -> Result<(), Error> {
    let Some(credentials) = Credentials::through(None, &kept.config, &kept.keys) else {
        return send(
            stream,
            &Reply::Refused {
                reason: String::from("the configuration keeps no sign-ins"),
            },
        );
    };
    let answer = crate::browser_credentials::answer(&credentials, op);
    send(stream, &Reply::SignIn { answer })
}

/// Answers who can open the vault, as the keyring says now: it changes
/// while the service runs.
fn answer_protection(stream: &UnixStream) -> Result<(), Error> {
    match KeyringConfig::from_env().and_then(|keyring| crate::vault_keyring::who_can_open(&keyring))
    {
        Ok(protection) => send(stream, &Reply::Protection { protection }),
        Err(error) => send(
            stream,
            &Reply::Refused {
                reason: error.to_string(),
            },
        ),
    }
}

/// Answers the broker's fill of the sign-in `id` into the browser cell
/// `cell`, whose page it says is on `origin` (WO-031). The sign-in must be
/// the profile's the cell was started for, and kept for `origin`: one that
/// isn't, or a cell the keeper holds no channel of, is refused and recorded
/// as denied. A fill is a release: recorded before it is sent, on the
/// cell's own channel, and never to the broker, which is told its nonce
/// alone. A channel a fill can't be written to is dropped.
fn answer_fill(
    stream: &UnixStream,
    kept: &mut Kept,
    id: &str,
    cell: &str,
    origin: &str,
) -> Result<(), Error> {
    let refuse = |reason: &str| {
        send(
            stream,
            &Reply::Refused {
                reason: reason.to_owned(),
            },
        )
    };
    let Some(credentials) = Credentials::through(None, &kept.config, &kept.keys) else {
        return refuse("the configuration keeps no sign-ins");
    };
    let root = kept.config.vault.root.clone();
    let recorded = |outcome| {
        let caller = Caller::Broker {
            purpose: String::from("browser-fill"),
        };
        crate::vault_access::record(&root, caller, "fill", vec![id.to_owned()], outcome)
    };
    let denied = |reason: &str| {
        if let Err(error) = recorded(Outcome::Denied) {
            tracing::error!(%error, "a refused fill not recorded");
        }
        tracing::warn!(cell, reason, "refused a fill");
        refuse(reason)
    };
    let failed = |reason: &str| {
        if let Err(error) = recorded(Outcome::Failed) {
            tracing::error!(%error, "a failed fill not recorded");
        }
        refuse(reason)
    };
    let Some(profile) = kept.browsers.get(cell).map(|held| held.profile.clone()) else {
        return denied("no browser of a signed-in profile is that cell");
    };
    let kept_for = match credentials.list() {
        Ok(listed) => listed.into_iter().find(|credential| credential.id == id),
        Err(reason) => return failed(&reason),
    };
    let Some(kept_for) = kept_for else {
        return failed("no such sign-in");
    };
    if kept_for.profile_id != profile {
        return denied("the sign-in isn't the profile's the browser was started for");
    }
    if kept_for.origin != origin {
        return denied("the sign-in isn't kept for the page's site");
    }
    let sign_in = match credentials.sign_in(id) {
        Ok(sign_in) => sign_in,
        Err(reason) => return failed(&reason),
    };
    if let Err(error) = recorded(Outcome::Allowed) {
        tracing::error!(%error, "a fill not recorded: nothing is sent");
        return refuse("the fill couldn't be recorded");
    }
    let nonce = crate::approval::random_hex("fill-").map_err(|error| Error::Keeper {
        reason: format!("cannot name a fill: {error}"),
    })?;
    let header = serde_json::to_vec(&hatchling_core::browser::SignInFill {
        nonce: nonce.clone(),
        origin: kept_for.origin,
    })
    .map_err(|error| Error::Keeper {
        reason: format!("cannot write a fill: {error}"),
    })?;
    let sent = kept.browsers.get(cell).map(|held| {
        write_frame(&held.stream, &header)
            .and_then(|()| write_frame(&held.stream, sign_in.username.expose().as_bytes()))
            .and_then(|()| write_frame(&held.stream, sign_in.password.expose().as_bytes()))
    });
    if let Some(Err(error)) = sent {
        tracing::warn!(cell, %error, "a fill couldn't be sent: the browser's channel is dropped");
        if let Some(held) = kept.browsers.remove(cell) {
            let _ = held.stream.shutdown(std::net::Shutdown::Both);
        }
        return refuse("the browser's channel broke");
    }
    send(stream, &Reply::Filling { nonce })
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
    use crate::browser_credentials::SignIn;

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
            grant(VaultAction::Store, "token"),
            grant(VaultAction::Remove, "token"),
        ];
        let held = |words: &[&str]| {
            let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
            held_by(&vault, &words).grants
        };
        assert_eq!(held(&["store:token"]), [grant(VaultAction::Store, "token")]);
        assert_eq!(held(&["store:token", "remove:token"]), vault.grants);
        assert!(held(&[]).is_empty());
        assert!(held(&["store:other", "fetch:token"]).is_empty());
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

    /// A room root key of a master key no test keeps.
    fn any_room() -> RoomRootKey {
        RoomRootKey::of(&MasterKey::generate().unwrap())
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

    /// The keeper's directories: the vault's, the sign-ins' when they're
    /// kept, and those of its key file, keyring and KEK, each once.
    #[test]
    fn the_keepers_directories_are_the_vaults_and_its_keys() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.vault = VaultConfig {
            root: PathBuf::from("/srv/h/vault"),
            key_file: PathBuf::from("/srv/h/vault.key/key"),
            grants: Vec::new(),
        };
        config.web.credentials = PathBuf::new();
        assert_eq!(
            keeper_dirs(&config, &KeyringConfig::default()),
            [
                PathBuf::from("/srv/h/vault"),
                PathBuf::from("/srv/h/vault.key")
            ]
        );
        config.web.credentials = PathBuf::from("/srv/h/sign-ins");
        assert_eq!(
            keeper_dirs(&config, &KeyringConfig::default()),
            [
                PathBuf::from("/srv/h/vault"),
                PathBuf::from("/srv/h/sign-ins"),
                PathBuf::from("/srv/h/vault.key")
            ]
        );
        config.web.credentials = PathBuf::new();
        let keyring = KeyringConfig {
            keyring: Some(PathBuf::from("/srv/h/ring/keyring")),
            kek_file: Some(PathBuf::from("/srv/h/kek/kek")),
            ..KeyringConfig::default()
        };
        assert_eq!(
            keeper_dirs(&config, &keyring),
            [
                PathBuf::from("/srv/h/vault"),
                PathBuf::from("/srv/h/vault.key"),
                PathBuf::from("/srv/h/ring"),
                PathBuf::from("/srv/h/kek"),
                PathBuf::from("/srv/h/vault.unlock"),
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
        for own in [Store::SECRETS, Store::CREDENTIALS] {
            assert!(
                matches!(kept_keys(&[(own, root.clone())]), Err(Error::Keeper { .. })),
                "the broker read the key of {}, the keeper's",
                own.label()
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Each purpose reads only the names the configuration gives it, and
    /// an unknown purpose reads nothing.
    #[test]
    fn a_purpose_reads_only_what_the_configuration_gives_it() {
        let mut config = crate::api_auth::tests::owner_config(None);
        config.egress.grants = vec![String::from("api.example.com:443")];
        config.vault.grants = vec![VaultGrant {
            action: hatchling_core::VaultAction::Store,
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
        let contains = r#"{"request":"contains","bytes":3,"check":"web"}"#;
        assert!(matches!(
            serde_json::from_str::<Request>(contains),
            Ok(Request::Contains {
                bytes: 3,
                check: CheckFor::Web,
                cell: None
            })
        ));
        // A check's answer has no room for anything but its one boolean.
        for refused in [
            r#"{"reply":"contains","found":true,"count":1}"#,
            r#"{"reply":"contains","found":true,"name":"token"}"#,
            r#"{"reply":"contains","found":true,"at":3}"#,
        ] {
            assert!(serde_json::from_str::<Reply>(refused).is_err(), "{refused}");
        }
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
            browsers: BTreeMap::new(),
            room: any_room(),
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
            action: hatchling_core::VaultAction::Store,
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
            browsers: BTreeMap::new(),
            room: any_room(),
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

    /// What the vault at `root` holds for `token`, as the tests' own read.
    fn held_token(root: &Path, master: &MasterKey) -> Vec<u8> {
        hatchling_vault::SealedVault::open(root, master)
            .unwrap()
            .fetch(&"token".parse().unwrap())
            .unwrap()
            .as_bytes()
            .to_vec()
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
        let root = config.vault.root.clone();
        config.vault.grants = vec![VaultGrant {
            action: hatchling_core::VaultAction::Store,
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
            browsers: BTreeMap::new(),
            room: any_room(),
        }));
        let store = SessionRequest::Vault(VaultOp::Store {
            name: String::from("token"),
            secret_b64: hatchling_core::SecretB64::encode(b"t2"),
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
        let storing = serde_json::to_vec(&store).unwrap();
        let ungranted = hand(&[]);
        assert!(matches!(
            ask(&ungranted, &storing),
            Some(SessionResponse::Vault(VaultResult::Denied))
        ));
        assert_eq!(held_token(&root, &master), b"t", "a denied store wrote");
        let granted = hand(&["store:token"]);
        assert!(matches!(
            ask(&granted, &storing),
            Some(SessionResponse::Vault(VaultResult::Stored))
        ));
        assert_eq!(held_token(&root, &master), b"t2", "the store didn't land");
        assert!(
            ask(&ungranted, b"{}").is_none(),
            "a request that isn't a vault op"
        );
        // Read strictly (WO-027), even in a field the op doesn't use.
        let strict = hand(&["store:token"]);
        let loose =
            br#"{"kind":"vault","op":"store","name":"token","secret_b64":"dA==","note":1.5}"#;
        assert!(ask(&strict, loose).is_none(), "an inexact number, answered");
        // A cell that leaves ends its channel quietly.
        let (gone, left) = UnixStream::pair().unwrap();
        drop(gone);
        let named = CellChannel {
            cell: String::from("run-2"),
            uid: 7,
            grants: Vec::new(),
            profile: None,
        };
        assert!(serve_cell(&left, &kept, &named).is_ok());
        let mut guard = kept.lock().unwrap();
        let op = VaultOp::Remove {
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
            ask(&granted, &storing),
            Some(SessionResponse::Vault(VaultResult::Stored))
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
            action: hatchling_core::VaultAction::Store,
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
        let op = VaultOp::Store {
            name: String::from("token"),
            secret_b64: hatchling_core::SecretB64::encode(b"t2"),
        };
        let cell: CellId = "run-1".parse().unwrap();
        let through = |keeper: Option<&Keeper>| {
            crate::secrets::vault_op_through(keeper, &config.vault, &master, &op, &cell, 7).unwrap()
        };
        let held = || held_token(&config.vault.root, &master);
        assert_eq!(through(Some(&keeper)), VaultResult::Denied);
        assert_eq!(held(), b"t", "the broker answered the keeper's op");
        assert_eq!(through(None), VaultResult::Stored);
        assert_eq!(held(), b"t2");
    }

    /// A config parses with no fetch grant, but the keeper holds if one
    /// ever gets through: a fetch a hand-built config and the cell's
    /// channel both grant, for a stored secret, is denied, the secret
    /// doesn't come back, and nothing is recorded as released.
    #[test]
    fn a_granted_fetch_is_denied_on_the_keepers_path() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-no-fetch");
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
                &Secret::new(b"s3cret-value".to_vec()).unwrap(),
            )
            .unwrap();
        let fetch = VaultOp::Fetch {
            name: String::from("token"),
        };
        let keys = StoreKeys::unwrap(&master, &[(Store::SECRETS, &config.vault.root)]).unwrap();
        let kept = Mutex::new(Kept {
            releases: Releases::of(&config, BTreeSet::new()),
            config,
            keys,
            released: BTreeSet::new(),
            browsers: BTreeMap::new(),
            room: any_room(),
        });
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let named = CellChannel {
            cell: String::from("run-1"),
            uid: 7,
            grants: vec![String::from("fetch:token")],
            profile: None,
        };
        let request = serde_json::to_vec(&SessionRequest::Vault(fetch)).unwrap();
        let served = std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve_cell(&theirs, &kept, &named));
            write_frame(&ours, &request).unwrap();
            let reply = read_frame_or_eof(&ours).unwrap().unwrap();
            let encoded = hatchling_core::SecretB64::encode(b"s3cret-value");
            let shown = String::from_utf8(reply.clone()).unwrap();
            assert!(
                !shown.contains(encoded.as_str()) && !shown.contains("s3cret-value"),
                "{shown}"
            );
            assert_eq!(
                serde_json::from_slice::<SessionResponse>(&reply).unwrap(),
                SessionResponse::Vault(VaultResult::Denied)
            );
            drop(ours);
            serving.join().unwrap()
        });
        assert!(served.is_ok(), "ends when the cell goes");
        let kept = kept.into_inner().unwrap();
        assert!(kept.released.is_empty(), "a refused fetch released a name");
        assert!(read_released(&kept.config, &kept.keys).unwrap().is_empty());
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
            browsers: BTreeMap::new(),
            room: any_room(),
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

    /// The broker asks who can open the vault, and the keeper says: with no
    /// keyring, its key file. The egress authority may not ask.
    #[test]
    fn the_keeper_says_who_can_open_the_vault() {
        use hatchling_core::ui::VaultTier;
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-protection");
        config.vault.root = dir.0.join("vault");
        let master = MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&config.vault.root, &master).unwrap();
        let own = [(Store::SECRETS, config.vault.root.as_path())];
        let kept = Mutex::new(Kept {
            releases: Releases::of(&config, BTreeSet::new()),
            keys: StoreKeys::unwrap(&master, &own).unwrap(),
            config,
            released: BTreeSet::new(),
            browsers: BTreeMap::new(),
            room: any_room(),
        });
        let (ours, theirs) = UnixStream::pair().unwrap();
        let (authoritys, authority_end) = UnixStream::pair().unwrap();
        std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve(&theirs, &kept, Role::Broker));
            let serving_authority = scope.spawn(|| serve(&authority_end, &kept, Role::Authority));
            let keeper = Keeper::over(ours).unwrap();
            assert_eq!(
                keeper.protection().unwrap(),
                VaultProtection {
                    tier: VaultTier::File,
                    keys: 0,
                    codes: 0,
                    codes_unconfirmed: false,
                }
            );
            let authority = Keeper::over(authoritys).unwrap();
            let refused = format!("{:?}", authority.protection());
            assert!(refused.contains("only reads"), "{refused}");
            drop((keeper, authority));
            assert!(serving.join().unwrap().is_ok());
            assert!(serving_authority.join().unwrap().is_ok());
        });
    }

    /// The sign-ins are the keeper's (WO-031): the broker, whose keys open
    /// no store of them, keeps, lists and deletes them by asking, and can't
    /// ask for one's secrets: the keeper sends those to a browser it fills
    /// alone. The egress authority may ask nothing, and a keeper whose
    /// configuration keeps none refuses.
    #[test]
    fn the_sign_ins_are_the_keepers_and_the_broker_asks_for_them() {
        use crate::browser_profiles::Profiles;
        use hatchling_core::ui::{Concealed, SignInRefusal};
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-sign-ins");
        config.vault.root = dir.0.join("vault");
        config.web.profiles = dir.0.join("profiles");
        config.web.credentials = dir.0.join("sign-ins");
        let master = MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&config.vault.root, &master).unwrap();
        let own = [
            (Store::SECRETS, config.vault.root.as_path()),
            (Store::CREDENTIALS, config.web.credentials.as_path()),
        ];
        let keepers = StoreKeys::unwrap(&master, &own).unwrap();
        let brokers =
            StoreKeys::unwrap(&master, &[(Store::PROFILES, config.web.profiles.as_path())])
                .unwrap();
        let profile = Profiles::of(&config, &brokers)
            .unwrap()
            .keep(&[String::from("a.example:443")], 1)
            .unwrap();
        let kept = Mutex::new(Kept {
            releases: Releases::of(&config, BTreeSet::new()),
            config: config.clone(),
            keys: keepers,
            released: BTreeSet::new(),
            browsers: BTreeMap::new(),
            room: any_room(),
        });
        let secret = SignIn {
            username: Concealed::new(String::from("zoe-user")),
            password: Concealed::new(String::from("pw \"quoted\" \\ secret")),
        };
        let (ours, theirs) = UnixStream::pair().unwrap();
        let (authoritys, authority_end) = UnixStream::pair().unwrap();
        std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve(&theirs, &kept, Role::Broker));
            let serving_authority = scope.spawn(|| serve(&authority_end, &kept, Role::Authority));
            let keeper = Keeper::over(ours).unwrap();
            let asking = Credentials::through(Some(&keeper), &config, &brokers).unwrap();
            let shown = format!("{asking:?}");
            assert!(shown.contains("kept: true"), "{shown}");
            // Refused by the broker's own check: not one of the profile's sites.
            assert_eq!(
                asking.store(&profile, "https://b.example", "Work", &secret, 1),
                Ok(Err(SignInRefusal::NotTheProfiles))
            );
            let stored = asking
                .store(&profile, "https://a.example", "Work", &secret, 7)
                .unwrap()
                .unwrap();
            assert_eq!(asking.list(), Ok(vec![stored.clone()]));
            let read = asking.sign_in(&stored.id);
            assert!(
                read.as_ref()
                    .is_err_and(|error| error.contains("never to the broker")),
                "the broker read a sign-in's secrets"
            );
            assert!(
                crate::vault_access::read_log(&config.vault.root)
                    .unwrap()
                    .0
                    .is_empty(),
                "nothing was released"
            );
            let request =
                serde_json::json!({"request": "sign-in", "op": {"op": "read", "id": stored.id}});
            assert!(
                serde_json::from_value::<Request>(request).is_err(),
                "the keeper's wire carries no read of a sign-in"
            );
            // The broker's keys open no store of the sign-ins.
            let local = Credentials::through(None, &config, &brokers).unwrap();
            assert!(local.list().is_err(), "the broker opened the sign-ins");
            // The egress authority may ask nothing of them.
            let authority = Keeper::over(authoritys).unwrap();
            let refused = format!("{:?}", authority.sign_ins(SignInRequest::List));
            assert!(refused.contains("only reads"), "{refused}");
            drop(keeper);
            drop(authority);
            assert!(serving.join().unwrap().is_ok());
            assert!(serving_authority.join().unwrap().is_ok());
        });
        // A keeper whose configuration keeps no sign-ins refuses them.
        let mut guard = kept.lock().unwrap();
        guard.config.web.credentials = PathBuf::new();
        let (ours, theirs) = UnixStream::pair().unwrap();
        answer_sign_in(&theirs, &guard, SignInRequest::List).unwrap();
        drop(guard);
        let reply: Reply = serde_json::from_slice(&read_frame(&ours).unwrap()).unwrap();
        assert!(
            matches!(&reply, Reply::Refused { reason } if reason.contains("keeps no sign-ins")),
            "{reply:?}"
        );
    }

    /// A browser cell's channel, named with the profile it was started
    /// for, is held for fills until the cell ends it (WO-031). A fill sends
    /// the sign-in on that channel alone, and the broker is told its nonce.
    /// A fill of another profile's sign-in, for another site, or into a
    /// browser the keeper holds no channel of is refused and recorded as
    /// denied; each fill is recorded before it's sent, and one that can't
    /// be sends nothing. A second channel for a cell already held is
    /// refused, and one a fill can't be written to is dropped.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one browser's channel from its handover to its end, and each fill on it"
    )]
    fn a_sign_in_is_filled_on_its_profiles_browser_channel_alone() {
        use crate::browser_profiles::Profiles;
        use hatchling_core::browser::SignInFill;
        use hatchling_core::ui::Concealed;
        use std::io::Read as _;
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-fills");
        config.vault.root = dir.0.join("vault");
        config.web.profiles = dir.0.join("profiles");
        config.web.credentials = dir.0.join("sign-ins");
        let master = MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&config.vault.root, &master).unwrap();
        let own = [
            (Store::SECRETS, config.vault.root.as_path()),
            (Store::CREDENTIALS, config.web.credentials.as_path()),
        ];
        let keepers = StoreKeys::unwrap(&master, &own).unwrap();
        let brokers =
            StoreKeys::unwrap(&master, &[(Store::PROFILES, config.web.profiles.as_path())])
                .unwrap();
        let profiles = Profiles::of(&config, &brokers).unwrap();
        let profile = profiles.keep(&[String::from("a.example:443")], 1).unwrap();
        let other = profiles.keep(&[String::from("b.example:443")], 2).unwrap();
        let secret = SignIn {
            username: Concealed::new(String::from("zoe-user")),
            password: Concealed::new(String::from("pw-secret")),
        };
        let local = Credentials::through(None, &config, &keepers).unwrap();
        let work = local
            .store(&profile, "https://a.example", "Work", &secret, 1)
            .unwrap()
            .unwrap();
        let home = local
            .store(&other, "https://b.example", "Home", &secret, 2)
            .unwrap()
            .unwrap();
        let kept = Arc::new(Mutex::new(Kept {
            releases: Releases::of(&config, BTreeSet::new()),
            config: config.clone(),
            keys: keepers,
            released: BTreeSet::new(),
            browsers: BTreeMap::new(),
            room: any_room(),
        }));
        let (spawners, keepers) = UnixStream::pair().unwrap();
        let taking = std::thread::spawn({
            let kept = Arc::clone(&kept);
            move || take_cells(&keepers, &kept)
        });
        let hand = |cell: &str| {
            let (ours, theirs) = UnixStream::pair().unwrap();
            ours.set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let named = serde_json::json!({
                "cell": cell, "uid": 7, "grants": [], "profile": profile.id,
            });
            write_frame(&spawners, named.to_string().as_bytes()).unwrap();
            hatchling_sys::net::send_fd(spawners.as_fd(), theirs.as_fd()).unwrap();
            ours
        };
        let held = |cell: &str| kept.lock().unwrap().browsers.contains_key(cell);
        let until = |done: &dyn Fn() -> bool| {
            (0..500).any(|_| {
                std::thread::sleep(Duration::from_millis(10));
                done()
            })
        };
        let browser = hand("browser-1");
        assert!(until(&|| held("browser-1")), "held for fills");
        let second = hand("browser-1");
        assert_eq!(
            (&second).read(&mut [0u8; 1]).unwrap(),
            0,
            "a second channel for the cell is refused"
        );
        let (ours, theirs) = UnixStream::pair().unwrap();
        std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve(&theirs, &kept, Role::Broker));
            let keeper = Keeper::over(ours).unwrap();
            let nonce = keeper
                .fill(&work.id, "browser-1", "https://a.example")
                .unwrap();
            let fill: SignInFill = serde_json::from_slice(&read_frame(&browser).unwrap()).unwrap();
            assert_eq!(
                fill,
                SignInFill {
                    nonce,
                    origin: String::from("https://a.example"),
                }
            );
            assert_eq!(read_frame(&browser).unwrap(), b"zoe-user");
            assert_eq!(read_frame(&browser).unwrap(), b"pw-secret");
            let refused = |id: &str, cell: &str, origin: &str| {
                keeper.fill(id, cell, origin).unwrap_err().to_string()
            };
            let another = refused(&home.id, "browser-1", "https://b.example");
            assert!(another.contains("the profile's"), "{another}");
            let elsewhere = refused(&work.id, "browser-1", "https://b.example");
            assert!(elsewhere.contains("the page's site"), "{elsewhere}");
            let nowhere = refused(&work.id, "browser-2", "https://a.example");
            assert!(nowhere.contains("no browser"), "{nowhere}");
            // As the broker's sessions ask it.
            let asked = {
                use crate::browser_ops::Fills as _;
                keeper.fill_sign_in(&work.id, "browser-2", "https://a.example")
            };
            assert!(asked.is_err_and(|error| error.contains("no browser")));
            let missing = refused("cred-none", "browser-1", "https://a.example");
            assert!(missing.contains("no such sign-in"), "{missing}");
            let (log, _) = crate::vault_access::read_log(&config.vault.root).unwrap();
            let fills: Vec<_> = log
                .iter()
                .map(|access| (access.action.as_str(), access.names.clone(), access.outcome))
                .collect();
            let fill = |id: &str, outcome| ("fill", vec![id.to_owned()], outcome);
            assert_eq!(
                fills,
                [
                    fill(&work.id, Outcome::Allowed),
                    fill(&home.id, Outcome::Denied),
                    fill(&work.id, Outcome::Denied),
                    fill(&work.id, Outcome::Denied),
                    fill(&work.id, Outcome::Denied),
                    fill("cred-none", Outcome::Failed),
                ]
            );
            let filler = Caller::Broker {
                purpose: String::from("browser-fill"),
            };
            assert!(log.iter().all(|access| access.caller == filler));
            // A fill that can't be recorded sends nothing.
            let log = config.vault.root.join(crate::vault_access::ACCESS_LOG);
            std::fs::remove_file(&log).unwrap();
            std::fs::create_dir(&log).unwrap();
            let unrecorded = refused(&work.id, "browser-1", "https://a.example");
            assert!(unrecorded.contains("recorded"), "{unrecorded}");
            browser.set_nonblocking(true).unwrap();
            assert!(
                read_frame(&browser).is_err(),
                "sent the browser a refused fill"
            );
            // The cell ends its channel: no fill reaches it after.
            drop(browser);
            assert!(until(&|| !held("browser-1")), "let go");
            assert!(refused(&work.id, "browser-1", "https://a.example").contains("no browser"));
            drop(keeper);
            assert!(serving.join().unwrap().is_ok());
        });
        drop(spawners);
        assert!(taking.join().unwrap().is_ok());
        // Held directly, as the spawner's channel holds it: once.
        let (_four, end) = UnixStream::pair().unwrap();
        hold_browser(end, "browser-4", profile.id.clone(), &kept).unwrap();
        assert!(held("browser-4"));
        let (_again, end) = UnixStream::pair().unwrap();
        let refused = hold_browser(end, "browser-4", profile.id.clone(), &kept);
        assert!(refused.is_err_and(|error| error.to_string().contains("second channel")));
        // A channel a fill can't be written to is dropped.
        let mut guard = kept.lock().unwrap();
        std::fs::remove_dir(config.vault.root.join(crate::vault_access::ACCESS_LOG)).unwrap();
        let (gone, end) = UnixStream::pair().unwrap();
        drop(gone);
        guard.browsers.insert(
            String::from("browser-3"),
            BrowserChannel {
                profile: profile.id.clone(),
                stream: end,
            },
        );
        let (ours, theirs) = UnixStream::pair().unwrap();
        answer_fill(
            &theirs,
            &mut guard,
            &work.id,
            "browser-3",
            "https://a.example",
        )
        .unwrap();
        assert!(!guard.browsers.contains_key("browser-3"), "kept");
        let reply: Reply = serde_json::from_slice(&read_frame(&ours).unwrap()).unwrap();
        assert!(
            matches!(&reply, Reply::Refused { reason } if reason.contains("broke")),
            "{reply:?}"
        );
        // A keeper whose configuration keeps no sign-ins fills none.
        guard.config.web.credentials = PathBuf::new();
        answer_fill(
            &theirs,
            &mut guard,
            &work.id,
            "browser-3",
            "https://a.example",
        )
        .unwrap();
        drop(guard);
        let reply: Reply = serde_json::from_slice(&read_frame(&ours).unwrap()).unwrap();
        assert!(
            matches!(&reply, Reply::Refused { reason } if reason.contains("keeps no sign-ins")),
            "{reply:?}"
        );
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

    /// A check goes out as its size and then its text, in whole frames, and
    /// what comes back is one boolean. The wait for it, longer than the
    /// usual, doesn't outlast it.
    #[test]
    fn a_check_sends_its_text_and_takes_one_boolean() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let keeper = Keeper::over(ours).unwrap();
        let stand_in = std::thread::spawn(move || {
            let request = read_frame(&theirs).unwrap();
            write_frame(&theirs, br#"{"reply":"ready"}"#).unwrap();
            let text = read_text(&theirs, MAX_FRAME_BYTES + 1).unwrap();
            write_frame(&theirs, br#"{"reply":"contains","found":true}"#).unwrap();
            (request, text)
        });
        let text = vec![b'.'; MAX_FRAME_BYTES + 1];
        let cell: CellId = "run-1".parse().unwrap();
        assert!(
            keeper
                .contains(CheckFor::Typed, Some(&cell), &text)
                .unwrap()
        );
        let (request, sent) = stand_in.join().unwrap();
        assert_eq!(
            String::from_utf8(request).unwrap(),
            format!(
                r#"{{"request":"contains","bytes":{},"check":"typed","cell":"run-1"}}"#,
                text.len()
            )
        );
        assert_eq!(sent.as_slice(), text.as_slice());
        assert_eq!(
            keeper.stream.lock().unwrap().read_timeout().unwrap(),
            Some(ANSWER_TIMEOUT)
        );
    }

    /// A check answered out of turn, at either step, isn't an answer.
    #[test]
    fn a_check_answered_out_of_turn_is_no_answer() {
        for replies in [
            &[r#"{"reply":"scan","count":0}"#][..],
            &[r#"{"reply":"ready"}"#, r#"{"reply":"scan","count":0}"#][..],
            &[r#"{"reply":"ready"}"#, r#"{"reply":"ready"}"#][..],
            &[r#"{"reply":"contains","found":false}"#][..],
        ] {
            let (ours, theirs) = UnixStream::pair().unwrap();
            theirs
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let keeper = Keeper::over(ours).unwrap();
            let owned: Vec<String> = replies.iter().map(|reply| (*reply).to_owned()).collect();
            let stand_in = std::thread::spawn(move || {
                read_frame(&theirs).unwrap();
                for reply in owned {
                    write_frame(&theirs, reply.as_bytes()).unwrap();
                    let _ = read_frame(&theirs);
                }
            });
            let answered = keeper.contains(CheckFor::Web, None, b"text");
            assert!(answered.is_err(), "{replies:?}: {answered:?}");
            drop(keeper);
            stand_in.join().unwrap();
        }
    }

    /// A keeper that stops reading a text is lost, in time: the broker
    /// isn't held on a write the kernel can't take.
    #[test]
    fn a_keeper_that_stops_reading_does_not_hold_the_broker() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let keeper = Keeper::bounded(ours, Duration::from_millis(100)).unwrap();
        let stand_in = std::thread::spawn(move || {
            read_frame(&theirs).unwrap();
            write_frame(&theirs, br#"{"reply":"ready"}"#).unwrap();
            // Reads nothing more, until the broker gives up.
            let mut held = [0_u8; 1];
            let _ = rustix::net::recv(
                &theirs,
                &mut held,
                rustix::net::RecvFlags::PEEK | rustix::net::RecvFlags::TRUNC,
            );
            std::thread::sleep(Duration::from_secs(2));
        });
        // Past anything a socket buffers.
        let answered = keeper.contains(CheckFor::Harness, None, &vec![b'.'; 8 * 1024 * 1024]);
        let told = format!("{answered:?}");
        assert!(told.contains("cannot be reached"), "{told}");
        assert!(keeper.exchange(|_| Ok(())).is_err(), "not lost");
        stand_in.join().unwrap();
    }

    /// What the channel can put in a check's line is a word of a fixed set
    /// and a cell's id, each held to what it may be. A cell that can't be
    /// one is refused and recorded with none, a word outside the set is a
    /// request that doesn't parse, and the check's line says what's true.
    #[test]
    fn a_checks_line_holds_only_what_it_may() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-check-line");
        config.vault.root = dir.0.join("vault");
        let root = config.vault.root.clone();
        let master = MasterKey::generate().unwrap();
        hatchling_vault::SealedVault::open(&root, &master)
            .unwrap()
            .store(
                &"token".parse().unwrap(),
                &Secret::new(b"t0ken-value".to_vec()).unwrap(),
            )
            .unwrap();
        let keys = StoreKeys::unwrap(&master, &[(Store::SECRETS, &root)]).unwrap();
        let kept = Mutex::new(Kept {
            releases: Releases::of(&config, BTreeSet::new()),
            config,
            keys,
            released: BTreeSet::new(),
            browsers: BTreeMap::new(),
            room: any_room(),
        });
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        // A test that fails mid-way isn't left waiting on the keeper's read.
        theirs
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let told = std::thread::scope(|scope| {
            let serving = scope.spawn(|| serve(&theirs, &kept, Role::Broker));
            let ask = |request: &str| {
                write_frame(&ours, request.as_bytes()).unwrap();
                String::from_utf8(read_frame(&ours).unwrap()).unwrap()
            };
            let forged = "run\n1970-01-01T00:00:00Z broker (egress) read allowed";
            let refused = ask(&serde_json::json!({
                "request": "contains", "bytes": 1, "check": "web", "cell": forged,
            })
            .to_string());
            assert!(refused.contains("refused"), "a cell that can't be one");
            let ready = ask(r#"{"request":"contains","bytes":1,"check":"search","cell":"run-1"}"#);
            write_frame(&ours, b"x").unwrap();
            let answer = String::from_utf8(read_frame(&ours).unwrap()).unwrap();
            // Past the set, the channel ends: the broker doesn't speak so.
            let unknown = r#"{"request":"contains","bytes":1,"check":"anything"}"#;
            write_frame(&ours, unknown.as_bytes()).unwrap();
            assert!(serving.join().unwrap().is_err(), "a word outside the set");
            (ready, answer)
        });
        assert_eq!(told.0, r#"{"reply":"ready"}"#);
        assert_eq!(told.1, r#"{"reply":"contains","found":false}"#);
        let (lines, unreadable) = crate::vault_access::read_log(&root).unwrap();
        assert_eq!(unreadable, 0);
        let [refused, made] = lines.as_slice() else {
            unreachable!("two checks, {} lines", lines.len());
        };
        let broker = |purpose: &str| Caller::Broker {
            purpose: purpose.to_owned(),
        };
        assert_eq!(refused.caller, broker("web-check"));
        assert_eq!(
            (refused.outcome, refused.run.as_deref()),
            (Outcome::Denied, None)
        );
        assert_eq!(made.caller, broker("search-check"));
        assert_eq!(
            (made.outcome, made.found, made.run.as_deref()),
            (Outcome::Allowed, Some(false), Some("run-1"))
        );
    }

    /// A check's size is one the keeper will take, up to the bound exactly.
    #[test]
    fn a_text_is_read_up_to_its_bound_and_no_further() {
        let bound = crate::held_check::MAX_CHECK_BYTES;
        assert_eq!(within_bound(0), Some(0));
        assert_eq!(within_bound(bound), usize::try_from(bound).ok());
        assert_eq!(within_bound(bound + 1), None);
        assert_eq!(within_bound(u64::MAX), None);
    }

    /// A text is exactly the frames its size says, read to the end: a frame
    /// past it, or an empty one, ends the channel, and an empty text reads
    /// nothing.
    #[test]
    fn a_text_that_is_not_the_size_it_said_ends_the_channel() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        ours.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&theirs, b"abc").unwrap();
        write_frame(&theirs, b"de").unwrap();
        write_frame(&theirs, b"after").unwrap();
        assert_eq!(read_text(&ours, 5).unwrap().as_slice(), b"abcde");
        assert!(read_text(&ours, 0).unwrap().is_empty(), "it read a frame");
        assert!(read_text(&ours, 4).is_err(), "a frame past its size");
        write_frame(&theirs, b"").unwrap();
        assert!(read_text(&ours, 2).is_err(), "an empty frame");
        drop(theirs);
        assert!(read_text(&ours, 2).is_err(), "the broker went");
    }

    /// A release is recorded, sealed in the secrets store, before the cell
    /// has it, once; the record reads back; a scan answers with the scan
    /// set's values, for a gate alone; only a granted store releases
    /// anything.
    #[test]
    fn a_release_is_recorded_and_a_gate_scans_for_it() {
        let mut config = crate::api_auth::tests::owner_config(None);
        let dir = crate::api_auth::tests::scratch("keeper-released");
        config.vault.root = dir.0.join("vault");
        config.vault.grants = vec![
            VaultGrant {
                action: hatchling_core::VaultAction::Store,
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
        let store = |name: &str| VaultOp::Store {
            name: name.to_owned(),
            secret_b64: hatchling_core::SecretB64::encode(b"t"),
        };
        let remove = VaultOp::Remove {
            name: String::from("old"),
        };
        let keys = StoreKeys::unwrap(&master, &[(Store::SECRETS, &config.vault.root)]).unwrap();
        assert!(read_released(&config, &keys).unwrap().is_empty());
        let releases = Releases::of(&config, BTreeSet::new());
        let mut kept = Kept {
            config,
            keys,
            releases,
            released: BTreeSet::new(),
            browsers: BTreeMap::new(),
            room: any_room(),
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
        // Only a store this session holds the grant for releases anything:
        // not one of another name, not a removal, and not one it wasn't
        // granted.
        let both = [String::from("store:token"), String::from("remove:old")];
        for (op, held) in [
            (store("other"), both.as_slice()),
            (remove, both.as_slice()),
            (store("token"), &[]),
        ] {
            assert!(cell_op_result(&mut kept, &op, "run-1", 7, held).is_ok());
            assert!(kept.released.is_empty(), "released by {}", op.action());
        }
        let stored = cell_op_result(&mut kept, &store("token"), "run-1", 7, &both);
        assert!(matches!(stored, Ok(VaultResult::Stored)));
        let Kept {
            config,
            keys,
            released,
            ..
        } = &mut kept;
        record_release(config, keys, released, "token").unwrap();
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

    /// A keeper's state for a room disk's tests: an empty vault at
    /// `root`, made owner-only, and the room root key of `master`.
    fn room_kept(root: &Path, master: &MasterKey) -> Arc<Mutex<Kept>> {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(root).unwrap();
        let mut config = crate::api_auth::tests::owner_config(None);
        config.vault.root = root.to_path_buf();
        Arc::new(Mutex::new(Kept {
            releases: Releases::of(&config, BTreeSet::new()),
            keys: StoreKeys::unwrap(master, &[(Store::SECRETS, root)]).unwrap(),
            config,
            released: BTreeSet::new(),
            browsers: BTreeMap::new(),
            room: RoomRootKey::of(master),
        }))
    }

    /// What a pipe's reading end gets before its writing ends are all
    /// closed.
    fn drained(read: OwnedFd) -> Vec<u8> {
        let mut got = Vec::new();
        std::io::Read::read_to_end(&mut std::fs::File::from(read), &mut got).unwrap();
        got
    }

    /// The spawner's end of its channel to the keeper over `kept`, and the
    /// keeper's taking of it, on a thread of its own.
    fn spawners_channel(
        kept: &Arc<Mutex<Kept>>,
    ) -> (UnixStream, std::thread::JoinHandle<Result<(), Error>>) {
        let (spawners, keepers) = UnixStream::pair().unwrap();
        let kept = Arc::clone(kept);
        (
            spawners,
            std::thread::spawn(move || take_cells(&keepers, &kept)),
        )
    }

    /// Every line of the vault's access log at `root`.
    fn accesses(root: &Path) -> Vec<crate::vault_access::Access> {
        let (accesses, unreadable) = crate::vault_access::read_log(root).unwrap();
        assert_eq!(unreadable, 0, "the access log has a line that doesn't read");
        accesses
    }

    /// The spawner asks for a room disk's key on its own channel: the key,
    /// derived for that image from the master key, goes into the pipe it
    /// hands over and nowhere else, and the ask is recorded first, as the
    /// spawner's, naming the image. A descriptor that isn't a pipe (a file,
    /// a socket) gets nothing, and neither does a name that isn't an
    /// image's UUID: each is recorded as denied. An ask that can't be
    /// recorded writes nothing. A frame with a field neither shape has ends
    /// the channel.
    #[test]
    fn a_room_disks_key_goes_into_its_pipe_alone_and_is_recorded_first() {
        use crate::vault_access::Caller;
        let dir = crate::api_auth::tests::scratch("keeper-room-key");
        let root = dir.0.join("vault");
        let master = MasterKey::generate().unwrap();
        let kept = room_kept(&root, &master);
        let (spawners, taking) = spawners_channel(&kept);
        let image = "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0";
        let ask = |image: &str, room_key: &str, into: std::os::fd::BorrowedFd<'_>| {
            let asked = serde_json::json!({ "image": image, "room_key": room_key });
            write_frame(&spawners, asked.to_string().as_bytes()).unwrap();
            hatchling_sys::net::send_fd(spawners.as_fd(), into).unwrap();
        };
        // Each ask is answered before the next is read: a pipe's answer
        // says every ask before it is done.
        let through_pipe = |image: &str, room_key: &str| {
            let (read, write) = hatchling_sys::pipe().unwrap();
            ask(image, room_key, write.as_fd());
            drop(write);
            drained(read)
        };
        let expected = |image: &str| {
            RoomRootKey::of(&master)
                .image_key(&image.parse().unwrap())
                .expose()
                .to_vec()
        };
        let spawner = |purpose: &str| Caller::Spawner {
            purpose: purpose.to_owned(),
        };

        let got = through_pipe(image, "open");
        assert!(
            got == expected(image),
            "the pipe didn't get the image's key, whole"
        );
        let line = accesses(&root).pop().unwrap();
        assert_eq!(line.caller, spawner("room disk open"));
        assert_eq!(line.action, "room-key");
        assert_eq!(line.names, [format!("room disk {image}")]);
        assert_eq!(line.outcome, Outcome::Allowed);

        let other = "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f1";
        let got = through_pipe(other, "create");
        assert!(
            got == expected(other) && got != expected(image),
            "another image didn't get a key of its own"
        );
        let line = accesses(&root).pop().unwrap();
        assert_eq!(line.caller, spawner("room disk create"));
        assert_eq!(line.names, [format!("room disk {other}")]);

        // A file: the key would stay there. Answered here directly, as
        // the spawner's channel answers it.
        let file_path = dir.0.join("not-a-pipe");
        let file = std::fs::File::create(&file_path).unwrap();
        let asked = RoomKeyAsk {
            image: image.to_owned(),
            room_key: RoomKeyFor::Open,
        };
        answer_room_key(&kept, &asked, OwnedFd::from(file));
        // A socket: the key would go on to whatever holds the other end.
        let (socket, peer) = UnixStream::pair().unwrap();
        ask(image, "open", socket.as_fd());
        drop(socket);
        assert!(
            through_pipe("not-a-uuid", "open").is_empty(),
            "a key was given for what isn't an image"
        );
        assert_eq!(
            std::fs::metadata(&file_path).unwrap().len(),
            0,
            "the key was written into a file"
        );
        peer.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut sent = Vec::new();
        std::io::Read::read_to_end(&mut &peer, &mut sent).unwrap();
        assert!(sent.is_empty(), "the key was written into a socket");
        let refused: Vec<(Outcome, bool)> = accesses(&root)
            .iter()
            .skip(2)
            .map(|access| (access.outcome, access.names.join("").contains(image)))
            .collect();
        assert_eq!(
            refused,
            [
                (Outcome::Denied, true),
                (Outcome::Denied, true),
                (Outcome::Denied, false),
            ],
            "a refused ask wasn't recorded as denied, naming what it named"
        );

        // An access log that can't be written to: nothing is given.
        let log = root.join(crate::vault_access::ACCESS_LOG);
        std::fs::remove_file(&log).unwrap();
        std::fs::create_dir(&log).unwrap();
        assert!(
            through_pipe(image, "open").is_empty(),
            "a key was given that wasn't recorded"
        );

        let extra = serde_json::json!({ "image": image, "room_key": "open", "uid": 7 });
        write_frame(&spawners, extra.to_string().as_bytes()).unwrap();
        assert!(
            taking.join().unwrap().is_err(),
            "a frame of neither shape was taken"
        );
    }

    /// A room disk's key is the spawner's to ask for alone: on the broker's
    /// channel and on the egress authority's it isn't a request at all, so
    /// the channel ends there, and a pipe sent with it gets nothing.
    #[test]
    fn a_room_disks_key_is_no_request_on_the_brokers_channel_or_the_authoritys() {
        let dir = crate::api_auth::tests::scratch("keeper-room-key-refused");
        let root = dir.0.join("vault");
        let master = MasterKey::generate().unwrap();
        let kept = room_kept(&root, &master);
        assert_eq!(RoomKeyFor::Open.purpose(), "room disk open");
        assert_eq!(RoomKeyFor::Create.purpose(), "room disk create");
        for asked in [
            r#"{"request":"room-key","image":"0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0","room_key":"open"}"#,
            r#"{"image":"0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0","room_key":"open"}"#,
        ] {
            assert!(
                serde_json::from_str::<Request>(asked).is_err(),
                "an ask for a room disk's key parsed as a request"
            );
            for role in [Role::Broker, Role::Authority] {
                let (ours, theirs) = UnixStream::pair().unwrap();
                let (read, write) = hatchling_sys::pipe().unwrap();
                write_frame(&ours, asked.as_bytes()).unwrap();
                hatchling_sys::net::send_fd(ours.as_fd(), write.as_fd()).unwrap();
                drop(write);
                assert!(
                    serve(&theirs, &kept, role).is_err(),
                    "the channel went on after an ask for a room disk's key"
                );
                drop((ours, theirs));
                assert!(drained(read).is_empty(), "a room disk's key was given");
            }
        }
        assert!(
            accesses(&root).is_empty(),
            "a refused request reached the access log"
        );
    }
}
