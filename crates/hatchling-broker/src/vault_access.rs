//! The vault's access log, and the owner's view of every credential
//! .
//!
//! Every credential access is one JSON [`Access`] line in
//! `<vault root>/.access.log`, mode `0600`, inside the owner-only vault
//! root: when, which caller (an agent session, by its kernel uid and the
//! cell id it gave, or the broker reading for its own purpose), the
//! action, the names, and the outcome. Names only, never a value. The
//! leading dot keeps the log out of the store: no secret name can collide
//! with it, and migration and rekeying skip it.
//!
//! The log never creates the vault root. A denial before the vault exists
//! is traced instead, so a denied op still touches no file.
//!
//! `hatchling-broker vault-audit` shows the owner every credential, stored
//! or configured, with exactly who may reach it and its last access;
//! `vault-audit log [n]` the last `n` accesses. Isolation keeps workloads
//! from each other's secrets, never the owner from theirs.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

use hatchling_core::SecretName;
use serde::{Deserialize, Serialize};

use crate::{Config, Error};

/// The access log, in the vault root.
pub const ACCESS_LOG: &str = ".access.log";

/// How many accesses `vault-audit log` shows by default.
pub const DEFAULT_LOG_LINES: usize = 50;

/// Who touched a credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "caller", rename_all = "kebab-case")]
pub enum Caller {
    /// An agent session, by its kernel peer uid and the cell id it gave
    /// (its own label, not proof).
    Session {
        /// The peer's uid, from `SO_PEERCRED`.
        uid: u32,
        /// The cell id the session named itself with.
        cell: String,
    },
    /// The broker itself, reading for its own purpose.
    Broker {
        /// What for: `egress`, `channel-account`, `artifact-gate`,
        /// `output-gate` or `browser-fill`, a sign-in's secrets for the
        /// browser to fill (WO-031).
        purpose: String,
    },
    /// The egress authority (WO-017), reading for the request it makes.
    Authority {
        /// What for: `search`.
        purpose: String,
    },
}

/// What came of an access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// It went ahead.
    Allowed,
    /// No grant allowed it.
    Denied,
    /// Allowed, but nothing is stored under the name.
    NotFound,
    /// Allowed, but the vault failed.
    Failed,
}

/// One line of the access log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Access {
    /// When, in Unix seconds.
    pub at: u64,
    /// Who.
    #[serde(flatten)]
    pub caller: Caller,
    /// `fetch`, `store`, `remove`, or `read` for the broker's own reads.
    pub action: String,
    /// The secret names touched.
    pub names: Vec<String>,
    /// What came of it.
    pub outcome: Outcome,
}

/// Unix seconds now.
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Records one access in the log under `root`.
///
/// # Errors
///
/// Returns [`Error::VaultAccessLog`] when the record cannot be written.
/// Without a vault root there is nothing to write to and nothing is
/// created: the access is traced, and this returns `Ok`.
pub fn record(
    root: &Path,
    caller: Caller,
    action: &str,
    names: Vec<String>,
    outcome: Outcome,
) -> Result<(), Error> {
    let access = Access {
        at: now(),
        caller,
        action: action.to_owned(),
        names,
        outcome,
    };
    if !root.is_dir() {
        tracing::warn!(
            ?access,
            "vault access traced only: the vault root does not exist yet"
        );
        return Ok(());
    }
    let line = serde_json::to_string(&access).map_err(|e| Error::VaultAccessLog {
        reason: format!("cannot encode an access record: {e}"),
    })?;
    let path = root.join(ACCESS_LOG);
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"))
        .map_err(|e| Error::VaultAccessLog {
            reason: format!("{}: {e}", path.display()),
        })
}

/// Every access the log under `root` holds, and how many lines did not
/// parse. No log is no accesses.
///
/// # Errors
///
/// Returns [`Error::VaultAccessLog`] when the log exists but cannot be
/// read.
pub fn read_log(root: &Path) -> Result<(Vec<Access>, usize), Error> {
    let path = root.join(ACCESS_LOG);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(Error::VaultAccessLog {
                reason: format!("{}: {error}", path.display()),
            });
        }
    };
    let mut accesses = Vec::new();
    let mut unreadable = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(access) => accesses.push(access),
            Err(_) => unreadable += 1,
        }
    }
    Ok((accesses, unreadable))
}

/// The names stored under `root`: every entry that is a secret name.
/// Dotfiles (the log) and anything else are not secrets.
///
/// # Errors
///
/// Returns [`Error::VaultAccessLog`] when the root exists but cannot be
/// listed.
pub fn stored_names(root: &Path) -> Result<Vec<String>, Error> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::VaultAccessLog {
                reason: format!("{}: {error}", root.display()),
            });
        }
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') && name.parse::<SecretName>().is_ok())
        .collect();
    names.sort();
    Ok(names)
}

/// Who may reach each name `config` mentions: the agent's grants, the
/// channel accounts the broker holds, the egress credentials it swaps in.
fn reach(config: &Config) -> BTreeMap<String, Vec<String>> {
    let mut reach: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut add = |name: String, who: String| reach.entry(name).or_default().push(who);
    for grant in &config.vault.grants {
        add(
            grant.name.as_str().to_owned(),
            format!(
                "agent sessions may {} it (scheduled runs and subagents only within that)",
                grant.action
            ),
        );
    }
    for account in &config.channel_accounts {
        add(
            account.secret.as_str().to_owned(),
            format!(
                "the broker holds it for channel account {}:{}",
                account.channel, account.label
            ),
        );
    }
    for destination in &config.egress.grants {
        add(
            crate::serve::destination_to_secret_name(destination),
            format!("the broker swaps it into egress to {destination}"),
        );
    }
    reach
}

/// The last access of each name in `accesses`.
fn last_access(accesses: &[Access]) -> BTreeMap<&str, &Access> {
    let mut last = BTreeMap::new();
    for access in accesses {
        for name in &access.names {
            last.insert(name.as_str(), access);
        }
    }
    last
}

/// One access as the owner reads it.
fn describe(access: &Access) -> String {
    let who = match &access.caller {
        Caller::Session { uid, cell } => format!("session uid {uid} ({cell})"),
        Caller::Broker { purpose } => format!("broker ({purpose})"),
        Caller::Authority { purpose } => format!("egress authority ({purpose})"),
    };
    let outcome = match access.outcome {
        Outcome::Allowed => "allowed",
        Outcome::Denied => "denied",
        Outcome::NotFound => "not found",
        Outcome::Failed => "failed",
    };
    format!("{} {who} {} {outcome}", when(access.at), access.action)
}

/// A time as a UTC timestamp.
fn when(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(|at| hatchling_scheduler::Timestamp::from_second(at).ok())
        .map_or_else(|| at.to_string(), |at| at.to_string())
}

/// The owner's view: every credential stored or configured, who may
/// reach it, and its last access. Values are never shown.
///
/// # Errors
///
/// As [`stored_names`] and [`read_log`].
pub fn report(config: &Config) -> Result<String, Error> {
    let root = &config.vault.root;
    let stored = stored_names(root)?;
    let reach = reach(config);
    let (accesses, unreadable) = read_log(root)?;
    let last = last_access(&accesses);
    let mut names: Vec<&str> = stored.iter().map(String::as_str).collect();
    names.extend(reach.keys().map(String::as_str));
    names.sort_unstable();
    names.dedup();
    let mut lines = vec![format!(
        "vault {}: {} stored, {} configured; values are never shown",
        root.display(),
        stored.len(),
        reach.len()
    )];
    for name in names {
        let state = if stored.iter().any(|stored| stored == name) {
            "stored"
        } else {
            "not stored"
        };
        lines.push(format!("{name} ({state})"));
        match reach.get(name) {
            Some(who) => lines.extend(who.iter().map(|who| format!("  {who}"))),
            None => lines.push(String::from(
                "  nothing may reach it: no grant, account or egress names it",
            )),
        }
        if let Some(access) = last.get(name) {
            lines.push(format!("  last access: {}", describe(access)));
        }
    }
    if unreadable > 0 {
        lines.push(format!(
            "warning: {unreadable} unreadable line(s) in the access log"
        ));
    }
    Ok(lines.join("\n"))
}

/// The last `count` accesses, oldest first.
///
/// # Errors
///
/// As [`read_log`].
pub fn recent(config: &Config, count: usize) -> Result<String, Error> {
    let (accesses, unreadable) = read_log(&config.vault.root)?;
    let skip = accesses.len().saturating_sub(count);
    let mut lines: Vec<String> = accesses
        .iter()
        .skip(skip)
        .map(|access| format!("{}: {}", describe(access), access.names.join(", ")))
        .collect();
    if lines.is_empty() {
        lines.push(String::from("no vault accesses recorded"));
    }
    if unreadable > 0 {
        lines.push(format!(
            "warning: {unreadable} unreadable line(s) in the access log"
        ));
    }
    Ok(lines.join("\n"))
}

/// The actions `vault-audit` takes.
pub const USAGE: &str = "usage: hatchling-broker vault-audit [log [count]]";

/// `vault-audit` with `args`: the owner's view, or the recent accesses.
///
/// # Errors
///
/// Returns [`Error::Usage`] for anything else, and the errors of
/// [`report`] and [`recent`].
pub fn command(config: &Config, args: &[&str]) -> Result<String, Error> {
    let usage = || Error::Usage {
        reason: String::from(USAGE),
    };
    match args {
        [] => report(config),
        ["log"] => recent(config, DEFAULT_LOG_LINES),
        ["log", count] => recent(config, count.parse().map_err(|_| usage())?),
        _ => Err(usage()),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use hatchling_vault::{MasterKey, SealedVault, Secret};

    use hatchling_core::VaultAction;

    use super::*;
    use crate::api_auth::tests::{owner_config, scratch};

    fn session() -> Caller {
        Caller::Session {
            uid: 1000,
            cell: String::from("cell-a"),
        }
    }

    #[test]
    fn an_access_is_one_owner_only_line_and_no_root_is_made_for_it() {
        let dir = scratch("access-record");
        let missing = dir.0.join("missing");
        record(
            &missing,
            session(),
            "fetch",
            vec![String::from("t")],
            Outcome::Denied,
        )
        .unwrap();
        assert!(!missing.exists(), "the log never creates the vault root");
        assert_eq!(read_log(&missing).unwrap(), (Vec::new(), 0));
        record(
            &dir.0,
            session(),
            "fetch",
            vec![String::from("token")],
            Outcome::Allowed,
        )
        .unwrap();
        record(
            &dir.0,
            Caller::Broker {
                purpose: String::from("egress"),
            },
            "read",
            vec![String::from("egress-x-1")],
            Outcome::NotFound,
        )
        .unwrap();
        let path = dir.0.join(ACCESS_LOG);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let (accesses, unreadable) = read_log(&dir.0).unwrap();
        assert_eq!(unreadable, 0);
        let [first, second] = accesses.as_slice() else {
            unreachable!("{accesses:?}");
        };
        assert_eq!(
            (
                first.caller.clone(),
                first.action.as_str(),
                first.names.clone(),
                first.outcome
            ),
            (
                session(),
                "fetch",
                vec![String::from("token")],
                Outcome::Allowed
            )
        );
        assert!(first.at > 1_700_000_000);
        assert_eq!(
            (
                second.caller.clone(),
                second.action.as_str(),
                second.outcome
            ),
            (
                Caller::Broker {
                    purpose: String::from("egress")
                },
                "read",
                Outcome::NotFound
            )
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"caller\":\"session\""), "{text}");
        std::fs::write(&path, format!("{text}not json\n\n")).unwrap();
        assert_eq!(read_log(&dir.0).unwrap().1, 1);
        // A log that cannot be written or read is an error, never a silence.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(
            record(&dir.0, session(), "fetch", Vec::new(), Outcome::Allowed),
            Err(Error::VaultAccessLog { .. })
        ));
        assert!(matches!(
            read_log(&dir.0),
            Err(Error::VaultAccessLog { .. })
        ));
        assert!(matches!(
            record(
                &dir.0,
                Caller::Broker {
                    purpose: String::from("egress"),
                },
                "read",
                Vec::new(),
                Outcome::Allowed,
            ),
            Err(Error::VaultAccessLog { .. })
        ));
        assert!(now() > 1_700_000_000);
    }

    #[test]
    fn only_secret_names_are_listed_as_stored() {
        let dir = scratch("access-stored");
        assert!(stored_names(&dir.0.join("missing")).unwrap().is_empty());
        for name in ["token", "a-key", ".access.log", "not.a.name"] {
            std::fs::write(dir.0.join(name), b"x").unwrap();
        }
        std::fs::create_dir(dir.0.join("subdir")).unwrap();
        assert_eq!(stored_names(&dir.0).unwrap(), ["a-key", "token"]);
        let file = dir.0.join("token");
        assert!(matches!(
            stored_names(&file),
            Err(Error::VaultAccessLog { .. })
        ));
    }

    #[test]
    fn the_owner_sees_every_credential_and_who_may_reach_it() {
        let dir = scratch("access-report");
        let mut config = owner_config(None);
        config.vault.root = dir.0.join("vault");
        config.vault.grants = vec![crate::VaultGrant {
            action: VaultAction::Fetch,
            name: "token".parse().unwrap(),
        }];
        config.channel_accounts = vec![crate::ChannelAccountSpec {
            channel: String::from("mail"),
            label: String::from("work"),
            secret: "mail-work".parse().unwrap(),
        }];
        config.egress.grants = vec![String::from("api.example.com:443")];
        let key = MasterKey::generate().unwrap();
        let vault = SealedVault::open(&config.vault.root, &key).unwrap();
        for name in ["token", "orphan"] {
            vault
                .store(
                    &name.parse().unwrap(),
                    &Secret::new(b"hidden-value".to_vec()).unwrap(),
                )
                .unwrap();
        }
        record(
            &config.vault.root,
            session(),
            "fetch",
            vec![String::from("token")],
            Outcome::Allowed,
        )
        .unwrap();
        let shown = command(&config, &[]).unwrap();
        assert!(
            !shown.contains("warning"),
            "a clean log brings no warning: {shown}"
        );
        assert!(
            shown.contains(": 2 stored, 3 configured; values are never shown"),
            "{shown}"
        );
        assert!(
            shown.contains("token (stored)\n  agent sessions may fetch it"),
            "{shown}"
        );
        assert!(shown.contains("  last access: "), "{shown}");
        assert!(
            shown.contains("session uid 1000 (cell-a) fetch allowed"),
            "{shown}"
        );
        assert!(
            shown.contains(
                "mail-work (not stored)\n  the broker holds it for channel account mail:work"
            ),
            "{shown}"
        );
        assert!(
            shown.contains("egress-api-example-com-443 (not stored)\n  the broker swaps it into egress to api.example.com:443"),
            "{shown}"
        );
        assert!(
            shown.contains("orphan (stored)\n  nothing may reach it"),
            "{shown}"
        );
        assert!(!shown.contains("hidden-value"), "values are never shown");
        let log = command(&config, &["log"]).unwrap();
        assert!(
            !log.contains("warning"),
            "a clean log brings no warning: {log}"
        );
        assert!(log.ends_with("fetch allowed: token"), "{log}");
        assert_eq!(
            command(&config, &["log", "0"]).unwrap(),
            "no vault accesses recorded"
        );
        for bad in [&["nope"][..], &["log", "x"], &["log", "1", "2"]] {
            assert!(
                matches!(command(&config, bad), Err(Error::Usage { .. })),
                "{bad:?}"
            );
        }
        std::fs::write(config.vault.root.join(ACCESS_LOG), b"garbage\n").unwrap();
        assert!(
            report(&config)
                .unwrap()
                .ends_with("warning: 1 unreadable line(s) in the access log")
        );
        assert!(
            recent(&config, 5)
                .unwrap()
                .ends_with("warning: 1 unreadable line(s) in the access log")
        );
    }

    #[test]
    fn each_part_of_the_view_says_one_thing() {
        assert_eq!(when(0), "1970-01-01T00:00:00Z");
        assert_eq!(when(u64::MAX), u64::MAX.to_string());
        let access = |name: &str, outcome| Access {
            at: 0,
            caller: Caller::Broker {
                purpose: String::from("output-gate"),
            },
            action: String::from("read"),
            names: vec![name.to_owned()],
            outcome,
        };
        let accesses = [
            access("a", Outcome::Denied),
            access("b", Outcome::NotFound),
            access("a", Outcome::Failed),
        ];
        let last = last_access(&accesses);
        assert_eq!(
            last.get("a").map(|access| access.outcome),
            Some(Outcome::Failed)
        );
        assert_eq!(
            last.get("b").map(|access| access.outcome),
            Some(Outcome::NotFound)
        );
        assert_eq!(
            describe(&accesses[0]),
            "1970-01-01T00:00:00Z broker (output-gate) read denied"
        );
        assert!(describe(&accesses[1]).ends_with("read not found"));
        assert!(describe(&accesses[2]).ends_with("read failed"));
        let by_authority = Access {
            caller: Caller::Authority {
                purpose: String::from("search"),
            },
            ..access("k", Outcome::Allowed)
        };
        assert_eq!(
            describe(&by_authority),
            "1970-01-01T00:00:00Z egress authority (search) read allowed"
        );
        let mut config = owner_config(None);
        config.vault.grants = vec![crate::VaultGrant {
            action: VaultAction::Store,
            name: "t".parse().unwrap(),
        }];
        assert_eq!(
            reach(&config).get("t").map(Vec::as_slice),
            Some(
                &[String::from(
                    "agent sessions may store it (scheduled runs and subagents only within that)"
                )][..]
            )
        );
        assert_eq!(DEFAULT_LOG_LINES, 50);
    }
}
