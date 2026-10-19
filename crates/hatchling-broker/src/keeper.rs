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
use std::sync::{Mutex, OnceLock};

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

/// The setting that names the keeper's end of the socket pair.
const KEEPER_FD: &str = "HATCHLING_KEEPER_FD";

/// What the broker asks the keeper.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "kebab-case", deny_unknown_fields)]
enum Request {
    /// The broker's own read of `names`, for `purpose`.
    Read { purpose: String, names: Vec<String> },
    /// A cell's vault op, from the cell `cell` whose peer uid is `uid`.
    Cell { op: VaultOp, cell: String, uid: u32 },
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
    /// Kept so the keeper isn't left a zombie; it leaves when the broker
    /// does.
    _child: Child,
}

/// The broker's keeper, once started.
static KEEPER: OnceLock<Keeper> = OnceLock::new();

/// The keeper this broker started, if it started one.
pub(crate) fn keeper() -> Option<&'static Keeper> {
    KEEPER.get()
}

/// Starts the keeper and takes the keys of the broker's stores from it.
///
/// Call it once, while the broker has one thread: the keeper's end of
/// the socket pair is the one descriptor its exec keeps, and it would be
/// inherited by anything another thread started meanwhile.
///
/// # Errors
///
/// Returns [`Error::Keeper`] when the keeper can't be started, refuses to
/// start (its reason, as the keeper gave it: a key that doesn't load, a
/// store that doesn't open), or hands over keys that don't read back.
pub fn start() -> Result<StoreKeys, Error> {
    let failed = |reason: String| Error::Keeper { reason };
    let program = std::env::current_exe()
        .map_err(|e| failed(format!("cannot locate the broker binary: {e}")))?;
    let (ours, theirs) =
        UnixStream::pair().map_err(|e| failed(format!("cannot make the socket pair: {e}")))?;
    hatchling_sys::keep_across_exec(theirs.as_fd())
        .map_err(|e| failed(format!("cannot hand the keeper its socket: {e}")))?;
    let child = Command::new(program)
        .arg(VAULT_KEEPER_SUBCOMMAND)
        .env(KEEPER_FD, theirs.as_raw_fd().to_string())
        .spawn()
        .map_err(|e| failed(format!("cannot start: {e}")))?;
    drop(theirs);
    let first = Zeroizing::new(
        read_frame(&ours).map_err(|e| failed(format!("gave no keys: {e}")))?,
    );
    let keys = match first.split_first() {
        Some((0, handed)) => StoreKeys::from_bytes(handed).map_err(|source| Error::Vault { source })?,
        Some((1, reason)) => return Err(failed(String::from_utf8_lossy(reason).into_owned())),
        _ => return Err(failed(String::from("its first answer was neither keys nor a reason"))),
    };
    KEEPER
        .set(Keeper {
            stream: Mutex::new(ours),
            _child: child,
        })
        .map_err(|_| failed(String::from("a keeper is already running")))?;
    Ok(keys)
}

impl Keeper {
    /// The broker's own read of `names`, for `purpose`, answered by the
    /// keeper: as [`Secrets::broker_read`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Keeper`] when the keeper refuses the read or
    /// can't be reached.
    pub(crate) fn read(
        &self,
        purpose: &str,
        names: &[SecretName],
    ) -> Result<Vec<Option<Secret>>, Error> {
        let stream = self.lock()?;
        let request = Request::Read {
            purpose: purpose.to_owned(),
            names: names.iter().map(|name| name.as_str().to_owned()).collect(),
        };
        let found = match ask(&stream, &request)? {
            Reply::Read { found } if found.len() == names.len() => found,
            Reply::Refused { reason } => return Err(Error::Keeper { reason }),
            other => return Err(unexpected(&other)),
        };
        found
            .into_iter()
            .map(|found| {
                if !found {
                    return Ok(None);
                }
                let bytes = Zeroizing::new(read_frame(&stream).map_err(keeper_gone)?);
                Secret::new(bytes.to_vec())
                    .map(Some)
                    .map_err(|source| Error::Vault { source })
            })
            .collect()
    }

    /// A cell's vault op, answered by the keeper: as
    /// [`handle_vault_op`]. The keeper can't be reached: the op failed,
    /// and the session goes on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SessionProtocol`] when the op was malformed.
    pub(crate) fn cell_op(
        &self,
        op: &VaultOp,
        cell: &CellId,
        uid: u32,
    ) -> Result<VaultResult, Error> {
        let request = Request::Cell {
            op: op.clone(),
            cell: cell.to_string(),
            uid,
        };
        let reply = self.lock().and_then(|stream| ask(&stream, &request));
        match reply {
            Ok(Reply::Cell { result }) => Ok(result),
            Ok(Reply::Protocol { reason }) => Err(Error::SessionProtocol { reason }),
            Ok(other) => {
                tracing::error!(error = %unexpected(&other), "vault op failed at the keeper");
                Ok(VaultResult::VaultError)
            }
            Err(error) => {
                tracing::error!(%error, "vault op failed at the keeper");
                Ok(VaultResult::VaultError)
            }
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, UnixStream>, Error> {
        self.stream.lock().map_err(|_| Error::Keeper {
            reason: String::from("an earlier request to it panicked"),
        })
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
            harness,
            accounts: config
                .channel_accounts
                .iter()
                .map(|account| account.secret.as_str().to_owned())
                .collect(),
            held: crate::artifact_ops::held_secret_names(config),
        }
    }

    /// Whether the broker may read every one of `names` for `purpose`.
    fn allow(&self, purpose: &str, names: &[SecretName]) -> bool {
        let given = match purpose {
            "egress" => &self.egress,
            "harness" => &self.harness,
            "channel-account" => &self.accounts,
            "artifact-gate" | "output-gate" => &self.held,
            _ => return false,
        };
        names.iter().all(|name| given.contains(name.as_str()))
    }
}

/// The keeper: `hatchling-broker vault-keeper`.
#[must_use]
pub fn vault_keeper_main() -> ExitCode {
    let stream = match std::env::var(KEEPER_FD)
        .ok()
        .and_then(|fd| fd.parse::<i32>().ok())
        .map(hatchling_sys::net::inherited_unix_stream)
    {
        Some(Ok(stream)) => stream,
        Some(Err(error)) => return refuse_to_run(&error.to_string()),
        None => return refuse_to_run("only the broker starts it"),
    };
    let state = match begin(&stream) {
        Ok(state) => state,
        Err(error) => {
            let mut reason = vec![1];
            reason.extend_from_slice(error.to_string().as_bytes());
            let _ = write_frame(&stream, &reason);
            return refuse_to_run(&error.to_string());
        }
    };
    match serve(&stream, &state) {
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
}

/// The keeper's start: the configuration, every store's key, the broker's
/// handed over, the master key dropped.
fn begin(stream: &UnixStream) -> Result<Kept, Error> {
    hatchling_sys::set_not_dumpable().map_err(|source| Error::Keeper {
        reason: format!("cannot keep its memory to itself: {source}"),
    })?;
    let config = Config::from_env()?;
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(config.log_level.to_tracing())
        .try_init();
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
    let releases = Releases::of(&config, harness);
    Ok(Kept {
        config,
        keys,
        releases,
    })
}

/// Answers the broker until it goes.
fn serve(stream: &UnixStream, kept: &Kept) -> Result<(), Error> {
    while let Some(frame) = read_frame_or_eof(stream).map_err(keeper_gone)? {
        let frame = Zeroizing::new(frame);
        let request: Request = serde_json::from_slice(&frame).map_err(|e| Error::Keeper {
            reason: format!("a request that doesn't parse: {e}"),
        })?;
        match request {
            Request::Read { purpose, names } => answer_read(stream, kept, &purpose, &names)?,
            Request::Cell { op, cell, uid } => {
                let reply = cell
                    .parse::<CellId>()
                    .map_err(|_| Error::SessionProtocol {
                        reason: String::from("a cell id that doesn't parse"),
                    })
                    .and_then(|cell| handle_vault_op(&kept.config.vault, &kept.keys, &op, &cell, uid));
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
    }
    Ok(())
}

/// Answers one read: refused and recorded as denied unless the
/// configuration gives `purpose` every one of `names`.
fn answer_read(stream: &UnixStream, kept: &Kept, purpose: &str, names: &[String]) -> Result<(), Error> {
    let parsed: Option<Vec<SecretName>> = names.iter().map(|name| name.parse().ok()).collect();
    let Some(parsed) = parsed.filter(|parsed| kept.releases.allow(purpose, parsed)) else {
        let root: PathBuf = kept.config.vault.root.clone();
        let caller = Caller::Broker {
            purpose: purpose.to_owned(),
        };
        if let Err(error) = crate::vault_access::record(&root, caller, "read", names.to_vec(), Outcome::Denied) {
            tracing::error!(%error, "a refused read not recorded");
        }
        tracing::warn!(purpose, "refused a read the configuration doesn't give its purpose");
        return send(
            stream,
            &Reply::Refused {
                reason: format!("the configuration gives {purpose:?} no such read"),
            },
        );
    };
    match Secrets::new(&kept.config.vault.root, &kept.keys).broker_read(purpose, &parsed) {
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
        let releases = Releases::of(&config, harness);
        let egress = "egress-api-example-com-443";
        assert!(releases.allow("egress", &names(&[egress])));
        assert!(!releases.allow("egress", &names(&[egress, "token"])));
        assert!(releases.allow("harness", &names(&["harness-claude-code"])));
        assert!(!releases.allow("harness", &names(&["harness-codex-cli"])));
        assert!(releases.allow("channel-account", &names(&["mail-work"])));
        assert!(!releases.allow("channel-account", &names(&["token"])));
        for gate in ["artifact-gate", "output-gate"] {
            assert!(releases.allow(gate, &names(&["token", egress, "mail-work"])));
            assert!(!releases.allow(gate, &names(&["harness-claude-code"])));
        }
        assert!(!releases.allow("scan", &names(&["token"])));
        assert!(releases.allow("egress", &[]));
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
            assert!(serde_json::from_str::<Request>(refused).is_err(), "{refused}");
        }
        assert!(serde_json::from_str::<Reply>(r#"{"reply":"read","found":[true],"key":"x"}"#).is_err());
        let gone = unexpected(&Reply::Protocol {
            reason: String::new(),
        });
        assert!(gone.to_string().contains("(protocol)"), "{gone}");
    }
}
