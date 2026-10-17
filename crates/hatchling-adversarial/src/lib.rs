//! cell adversarial suite: attack taxonomy, probe report protocol,
//! denied-syscall catalog, and harness assertion helpers.
//!
//! The suite attacks a **real production-spawned cell**, never a mock:
//! the `hatchling-probe` binary runs inside the cell under the
//! production seccomp profile and mount plan, performs one attack
//! selected by its `HATCHLING_CELL_ID`, and writes a JSON [`Report`] to
//! stdout. The integration driver
//! (`crates/hatchling-broker/tests/adversarial.rs`) spawns one cell per
//! attack, collects the report, and validates it with the pure helpers
//! in this library.
//!
//! The helpers are the WO's mutation-kill-floor-1.0 targets: they are
//! pure, heavily unit-tested, and contain no spawning or I/O. The
//! driver itself is glue and is not a mutation target.
//!
//! Methodology note (kept here, not in a second document): every attack
//! asserts at the syscall level — the probe issues raw syscalls and
//! reports the kernel's errno or the signal that killed it. The
//! harness never reads mode bits or trusts the cell's word for what
//! the kernel did.

#[allow(
    clippy::redundant_pub_crate,
    reason = "explicit `pub(crate)` documents the table as crate API; \
              `pub` would trip `unreachable_pub`"
)]
pub(crate) mod syscall_table;

/// Path dictionaries the in-cell probes sweep (shared with the probe
/// binary and the dictionary-coverage test).
pub mod dictionaries;

/// The in-cell probe attack implementations (the `hatchling-probe`
/// binary is a thin entry point over this module).
pub mod probe;

use std::fmt;

use serde::{Deserialize, Serialize};

/// Prefix every adversarial cell id carries.
///
/// The probe reads `HATCHLING_CELL_ID`, strips this prefix, and dispatches on the rest.
/// Using the cell id as the attack channel keeps the production spawn
/// path untouched: no extra env vars, no argv changes, no test seams.
pub const CELL_ID_PREFIX: &str = "wo016-";

/// One attack the probe can perform inside the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attack {
    /// Sweep filesystem and abstract unix sockets; only the broker
    /// socket may connect.
    SocketSweep,
    /// Attempt writes to every visible mount; every write must fail at
    /// the syscall level.
    MountEscape,
    /// Issue raw syscall `nr`; the seccomp filter must kill the probe
    /// with `SIGSYS`.
    Seccomp(u32),
    /// `unshare(CLONE_NEWUSER)` from inside the cell: must `SIGSYS`.
    PrivescUnshare,
    /// Scan visible files for setuid/setgid bits: none may exist.
    SetuidScan,
    /// Connect to a decoy socket whose broker expects a *different*
    /// uid: the peer gate must close with zero bytes, no oracle.
    PeerDecoy,
    /// Send garbage to the real broker socket as the authorized peer:
    /// the designed `REJECT` frame, not a crash or hang.
    PeerGarbage,
    /// Drive the real session protocol with ungranted/invalid ops:
    /// every answer must be the uniform `Denied`.
    SessionUniform,
    /// Assert the cell holds no cron/systemd/PATH/FIFO/TCP residue.
    NegativeSweep,
    /// `bind(2)` on a fresh `AF_UNIX` socket: must `SIGSYS`.
    NetBind,
    /// `socket(AF_INET, ...)` : must `SIGSYS` (the filter constrains the
    /// family argument to `AF_UNIX`).
    TcpSocket,
    /// Report the cell's supplementary groups: a root broker's cell must
    /// have none.
    Groups,
    /// Clear the probe's own parent-death signal, say so, and spin for a
    /// minute: the driver kills the cell's supervisor, and the probe must
    /// die with it all the same.
    ClearDeathSignal,
    /// Try to lift each of the cell's resource limits, and report them:
    /// each must stay as the cell was given it.
    RaiseLimits,
    /// Fork sleeping children until the kernel refuses, bounded at 128 attempts.
    ForkBomb,
}

/// Why a cell id did not name an attack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseAttackError {
    /// The id does not start with [`CELL_ID_PREFIX`].
    MissingPrefix,
    /// The suffix after the prefix is not a known attack slug.
    UnknownAttack(String),
    /// The `seccomp-` suffix is not a valid syscall number.
    BadSyscallNumber(String),
}

impl fmt::Display for ParseAttackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPrefix => write!(f, "cell id lacks the {CELL_ID_PREFIX:?} prefix"),
            Self::UnknownAttack(slug) => write!(f, "unknown attack slug {slug:?}"),
            Self::BadSyscallNumber(raw) => {
                write!(f, "bad syscall number {raw:?} in seccomp attack")
            }
        }
    }
}

impl Attack {
    /// Parse the attack out of a probe cell id (`wo016-<slug>`).
    ///
    /// # Errors
    ///
    /// Returns [`ParseAttackError`] when the cell id lacks the
    /// `wo016-` prefix or names an unknown attack.
    pub fn parse(cell_id: &str) -> Result<Self, ParseAttackError> {
        let slug = cell_id
            .strip_prefix(CELL_ID_PREFIX)
            .ok_or(ParseAttackError::MissingPrefix)?;
        if let Some(nr) = slug.strip_prefix("seccomp-") {
            let nr: u32 = nr
                .parse()
                .map_err(|_| ParseAttackError::BadSyscallNumber(nr.to_owned()))?;
            return Ok(Self::Seccomp(nr));
        }
        match slug {
            "socket-sweep" => Ok(Self::SocketSweep),
            "mount-escape" => Ok(Self::MountEscape),
            "privesc-unshare" => Ok(Self::PrivescUnshare),
            "setuid-scan" => Ok(Self::SetuidScan),
            "peer-decoy" => Ok(Self::PeerDecoy),
            "peer-garbage" => Ok(Self::PeerGarbage),
            "session-uniform" => Ok(Self::SessionUniform),
            "negative-sweep" => Ok(Self::NegativeSweep),
            "net-bind" => Ok(Self::NetBind),
            "tcp-socket" => Ok(Self::TcpSocket),
            "groups" => Ok(Self::Groups),
            "clear-death-signal" => Ok(Self::ClearDeathSignal),
            "raise-limits" => Ok(Self::RaiseLimits),
            "fork-bomb" => Ok(Self::ForkBomb),
            _ => Err(ParseAttackError::UnknownAttack(slug.to_owned())),
        }
    }

    /// The attack slug: the cell-id suffix and the report's attack name.
    #[must_use]
    pub fn slug(&self) -> String {
        match self {
            Self::SocketSweep => String::from("socket-sweep"),
            Self::MountEscape => String::from("mount-escape"),
            Self::Seccomp(nr) => format!("seccomp-{nr}"),
            Self::PrivescUnshare => String::from("privesc-unshare"),
            Self::SetuidScan => String::from("setuid-scan"),
            Self::PeerDecoy => String::from("peer-decoy"),
            Self::PeerGarbage => String::from("peer-garbage"),
            Self::SessionUniform => String::from("session-uniform"),
            Self::NegativeSweep => String::from("negative-sweep"),
            Self::NetBind => String::from("net-bind"),
            Self::TcpSocket => String::from("tcp-socket"),
            Self::Groups => String::from("groups"),
            Self::ClearDeathSignal => String::from("clear-death-signal"),
            Self::RaiseLimits => String::from("raise-limits"),
            Self::ForkBomb => String::from("fork-bomb"),
        }
    }

    /// The full cell id the driver hands to `CellConfig::parse_test`.
    #[must_use]
    pub fn cell_id(&self) -> String {
        format!("{CELL_ID_PREFIX}{}", self.slug())
    }

    /// `true` for the attacks whose verdict is the cell's death signal
    /// (`SIGSYS`) rather than a report: the probe cannot write a report
    /// after the filter kills it.
    #[must_use]
    pub const fn verdict_is_signal(&self) -> bool {
        matches!(
            self,
            Self::Seccomp(_) | Self::PrivescUnshare | Self::NetBind | Self::TcpSocket
        )
    }
}

/// One socket-connect attempt in the reachability sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocketAttempt {
    /// Filesystem path or abstract name (without the leading NUL).
    pub socket: String,
    /// `"filesystem"` or `"abstract"`.
    pub kind: String,
    /// `"ok"` on connect success, otherwise `"errno:<n>"`.
    pub connect: String,
}

/// One write attempt in the mount-escape probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteAttempt {
    /// Path written to (for creates, the parent tried).
    pub path: String,
    /// `"create"` or `"open_write"`.
    pub op: String,
    /// The kernel's errno, or 0 if the write unexpectedly succeeded.
    pub errno: i32,
}

/// One file in the setuid/setgid scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedFile {
    /// Path scanned.
    pub path: String,
    /// Whether the setuid bit was set.
    pub setuid: bool,
    /// Whether the setgid bit was set.
    pub setgid: bool,
}

/// One of the cell's resource limits, after the probe tried to lift it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldLimit {
    /// `processes`, `address-space`, `file-size`, `open-files`, `core`
    /// (lifted) or `core-lowered` (lowered to 0).
    pub name: String,
    /// The soft limit; `None` for none.
    pub soft: Option<u64>,
    /// The hard limit; `None` for none.
    pub hard: Option<u64>,
    /// The errno of lifting both to none (lowering both to 0 for
    /// `core-lowered`), or 0 if that worked.
    pub raise_errno: i32,
}

/// One absence check in the negative sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceCheck {
    /// Path that must not exist.
    pub path: String,
    /// Whether it existed.
    pub exists: bool,
}

/// The probe's structured outcome. Every variant is deterministic
/// except where noted; the committed inventory pins the exact shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// Fork pressure: a cgroup must refuse with EAGAIN before the safety cap.
    ForkBomb {
        /// Successfully forked sleeping children.
        forks: usize,
        /// The final refusal's errno, or zero if the safety cap was reached.
        errno: i32,
    },
    /// Socket reachability sweep results.
    SocketSweep {
        /// One entry per probed socket, in probe order.
        attempts: Vec<SocketAttempt>,
    },
    /// Mount-escape write results.
    MountEscape {
        /// One entry per write attempt, in probe order.
        writes: Vec<WriteAttempt>,
    },
    /// A signal-verdict attack survived: the syscall returned instead
    /// of killing the probe. This is the hole report — the driver
    /// treats any report from a signal-verdict attack as a failure.
    SyscallSurvived {
        /// Syscall number attempted.
        nr: u32,
        /// Its return value.
        ret: i64,
    },
    /// Setuid/setgid scan results.
    SetuidScan {
        /// One entry per existing scanned file.
        files: Vec<ScannedFile>,
    },
    /// Wrong-peer decoy-socket observation.
    PeerDecoy {
        /// `None` when `connect` succeeded; `Some(errno)` otherwise.
        connect_errno: Option<i32>,
        /// Bytes read before EOF/close.
        bytes_read: usize,
        /// Whether the peer closed without sending anything.
        eof: bool,
    },
    /// Authorized-peer garbage-traffic observation.
    PeerGarbage {
        /// `"reject"` for the designed REJECT frame,
        /// `"closed_no_bytes"` for a silent close, anything else is
        /// reported verbatim as `"other:<detail>"`.
        response: String,
    },
    /// Session uniform-deny observation: the JSON of each broker reply.
    SessionUniform {
        /// One JSON reply per op, in request order.
        responses: Vec<String>,
    },
    /// Negative-sweep absence checks.
    NegativeSweep {
        /// One entry per checked path, in probe order.
        checks: Vec<PresenceCheck>,
    },
    /// The probe cleared its death signal and spun out its minute: it
    /// outlived its supervisor.
    OutlivedSupervisor {
        /// How long it spun, in seconds.
        spun_secs: u64,
    },
    /// The cell's supplementary groups.
    Groups {
        /// The `Groups:` value of its `/proc/self/status`, as the kernel
        /// wrote it: empty when it has none.
        groups: String,
    },
    /// The cell's resource limits, each after the probe tried to lift it.
    Limits {
        /// One entry per limit, in [`HeldLimit::name`]'s order.
        limits: Vec<HeldLimit>,
    },
}

/// The probe's whole report: one JSON object on stdout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// Attack slug (see [`Attack::slug`]).
    pub attack: String,
    /// Observed uid inside the cell (for the record; always 0).
    pub uid: u32,
    /// Observed gid inside the cell (for the record; always 0).
    pub gid: u32,
    /// Observed pid inside the cell (for the record; always 2, the cell's
    /// program under its init).
    pub pid: u32,
    /// The structured outcome.
    pub outcome: Outcome,
    /// `Some` when the probe could not complete the attack.
    pub error: Option<String>,
}

/// Parse one report line from the probe's stdout.
///
/// # Errors
///
/// Returns a message when the line is not valid JSON or does not
/// match the [`Report`] shape.
pub fn parse_report(line: &str) -> Result<Report, String> {
    serde_json::from_str(line).map_err(|e| format!("probe report is not valid JSON: {e}"))
}

/// A syscall the agent seccomp profile denies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeniedSyscall {
    /// Kernel-header name (`mount`, `clone`, ...).
    pub name: &'static str,
    /// `x86_64` syscall number.
    pub nr: u32,
}

/// The denied-syscall catalog: every syscall in the kernel-header table
/// that `SeccompProfile::agent()` does not allow.
///
/// The catalog is computed at runtime as the table minus the
/// allowlist, so it is synchronized 1:1 with the allowlist's
/// complement by construction — adding a syscall to the profile
/// removes it from the catalog with no catalog edit. The table itself
/// is generated from the kernel headers (see
/// `scripts/gen_seccomp_catalog.py`); a unit test re-parses the live
/// headers and fails on drift.
///
/// Sorted by number. Empty on non-`x86_64` (the table is `x86_64`).
///
/// # Errors
///
/// Propagates [`hatchling_sys::Error`] when the seccomp profile's
/// allowlist cannot be computed.
pub fn denied_syscalls() -> Result<Vec<DeniedSyscall>, hatchling_sys::Error> {
    let allowed = hatchling_sys::seccomp::SeccompProfile::agent()?.allowed_numbers();
    Ok(catalog_without(&allowed))
}

/// The harness CLI profile's denied-syscall catalog.
///
/// Every syscall in the table that `SeccompProfile::harness_cli()`
/// neither allows nor decides by a rule, so its filter kills each one.
/// The ruled calls are covered apart: their refusals are not kills.
/// Computed like [`denied_syscalls`]; sorted by number, empty on
/// non-`x86_64`.
///
/// # Errors
///
/// Propagates [`hatchling_sys::Error`] when the profile cannot be built.
pub fn cli_denied_syscalls() -> Result<Vec<DeniedSyscall>, hatchling_sys::Error> {
    let profile = hatchling_sys::seccomp::SeccompProfile::harness_cli()?;
    let mut decided = profile.allowed_numbers();
    decided.extend(profile.ruled_numbers());
    Ok(catalog_without(&decided))
}

/// Every syscall in the table but `decided`, sorted by number.
fn catalog_without(decided: &[u32]) -> Vec<DeniedSyscall> {
    let mut catalog: Vec<DeniedSyscall> = syscall_table::SYSCALL_TABLE
        .iter()
        .filter(|row| !decided.contains(&row.nr))
        .map(|row| DeniedSyscall {
            name: row.name,
            nr: row.nr,
        })
        .collect();
    catalog.sort_by_key(|entry| entry.nr);
    catalog
}

/// Syscall-level denials the mount-escape probe accepts: the kernel
/// refused the write (`EPERM`, `EACCES`, `EROFS`). Anything else —
/// success (0) or an unexpected errno — is a hole or a bug.
///
/// Errno numbers are the stable Linux ABI (1, 13, 30); the probe
/// does not link `libc` for constants.
#[must_use]
pub const fn is_denial_errno(errno: i32) -> bool {
    matches!(errno, 1 | 13 | 30)
}

/// `ENOENT` (2): the path does not exist in the cell. Expected for the
/// dictionary entries that name paths the construction never creates.
#[must_use]
pub const fn is_not_found_errno(errno: i32) -> bool {
    errno == 2
}

/// Errno stand-in for an [`std::io::Error`] that carries no OS error
/// code.
///
/// Every error the probe records comes from a syscall, so this never
/// appears in a real report; `i32::MIN` is no errno the kernel can
/// return, so it can never be mistaken for one.
pub const NO_ERRNO: i32 = i32::MIN;

/// The kernel errno behind `error`, or [`NO_ERRNO`].
#[must_use]
pub fn os_errno(error: &std::io::Error) -> i32 {
    error.raw_os_error().unwrap_or(NO_ERRNO)
}

/// The `ret` a surviving call reports when it failed: the raw-syscall
/// `-errno` convention, so a survival report reads like the kernel's
/// own return value.
#[must_use]
pub fn failed_call_ret(errno: i32) -> i64 {
    -i64::from(errno)
}

/// Read from `stream` until the peer closes or `timeout` passes:
/// `(bytes_read, closed)`.
///
/// The peer-gate attacks' observer. A close is EOF, or `ECONNRESET`:
/// `AF_UNIX` reports a peer close that way when bytes we sent were still
/// unread on its side, which is exactly what the peer gate does to the
/// decoy's garbage (observed on a real kernel), and whether our write
/// lands before the close is a race. Any other read error is an error.
///
/// Generic over the stream so the host unit tests can drive it with a
/// socket pair and a failing fd; the probe passes `&UnixStream`.
///
/// # Errors
///
/// Returns a message if `poll` fails or a read fails other than by a
/// peer reset.
pub fn read_until_eof<S>(
    mut stream: S,
    timeout: std::time::Duration,
) -> Result<(usize, bool), String>
where
    S: std::io::Read + std::os::fd::AsFd,
{
    let start = std::time::Instant::now();
    let mut total: usize = 0;
    let mut buf = [0u8; 4096];
    loop {
        if start.elapsed() >= timeout {
            return Ok((total, false));
        }
        // Poll with a 500 ms step; rustix owns the fd borrow, so there
        // is no unsafe here.
        let ready = {
            let mut fds = [rustix::event::PollFd::new(
                &stream,
                rustix::event::PollFlags::IN,
            )];
            rustix::event::poll(&mut fds, 500).map_err(|e| format!("poll failed: {e}"))?
        };
        if ready == 0 {
            continue;
        }
        match stream.read(&mut buf) {
            Ok(0) => return Ok((total, true)),
            Ok(n) => total += n,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                return Ok((total, true));
            }
            Err(e) => return Err(format!("read failed: {e}")),
        }
    }
}

/// Whether a mount-escape write attempt was refused by the kernel.
///
/// The mount-layer denials ([`is_denial_errno`]) are the proof the
/// suite wants. Three more refusals are expected for specific
/// entries, and the committed inventory pins exactly where each
/// occurs (observed on a real kernel (6.6)):
///
/// * `ENOENT` (2): the path is not in the cell at all, so there is no
///   mount to write through ([`is_not_found_errno`]);
/// * `ETXTBSY` (26) opening `/agent` for writing: it is the probe's
///   own running image, and the kernel refuses the write before it
///   consults the mount;
/// * `ENXIO` (6) opening a `/sock/` entry for writing: the entries
///   there are sockets, which cannot be `open(2)`ed.
///
/// The last two say nothing about the mount's read-only-ness; the
/// `create` attempts in `/` and `/sock` do.
#[must_use]
pub fn is_refused_write(path: &str, op: &str, errno: i32) -> bool {
    is_denial_errno(errno)
        || is_not_found_errno(errno)
        || (op == "open_write" && path == "/agent" && errno == 26)
        || (op == "open_write" && path.starts_with("/sock/") && errno == 6)
}

/// Every session response is the identical JSON value: the broker's
/// deny is uniform across routes, leaking no per-route fingerprint.
///
/// Vault and egress responses differ ONLY in the outer `"kind"` tag
/// (`"vault"` vs `"egress"`), which is protocol routing the client
/// already knows from the request it sent — not a fingerprint. Within
/// each kind, the deny must be byte-identical: a vault deny that
/// varied by op name would leak which names exist.
#[must_use]
pub fn responses_uniform(responses: &[serde_json::Value]) -> bool {
    let mut by_kind: std::collections::HashMap<&str, &serde_json::Value> =
        std::collections::HashMap::new();
    for response in responses {
        let kind = response.get("kind").and_then(serde_json::Value::as_str);
        let Some(kind) = kind else {
            return false;
        };
        match by_kind.get(kind) {
            None => {
                by_kind.insert(kind, response);
            }
            Some(first) => {
                if *first != response {
                    return false;
                }
            }
        }
    }
    !by_kind.is_empty()
}

/// Every session response is a `Denied` result.
///
/// Vault denies serialize as `{"kind":"vault","r":"denied"}`, egress
/// denies as `{"kind":"egress","r":"denied"}`. The `"r"` field is the
/// verdict; `"kind"` is routing.
#[must_use]
pub fn all_denied(responses: &[serde_json::Value]) -> bool {
    !responses.is_empty()
        && responses
            .iter()
            .all(|response| response.get("r").and_then(serde_json::Value::as_str) == Some("denied"))
}

/// Compare a regenerated inventory against the committed one.
///
/// Returns `Ok` on exact equality. On mismatch returns a diff-style
/// `Err` with both JSON documents; the driver writes the regenerated
/// document next to the committed one (`.new`) and fails loudly.
/// Regeneration itself is opt-in (`HATCHLING_ADVERSARIAL_REGEN=1`) and
/// must follow human review of the diff: the inventory is a security
/// assertion, not a snapshot to blindly update.
///
/// # Errors
///
/// Returns a message describing the mismatch when `actual` differs
/// from the committed `expected` inventory.
pub fn check_inventory<T>(attack: &str, actual: &T, expected: &T) -> Result<(), String>
where
    T: PartialEq + Serialize + fmt::Debug,
{
    if actual == expected {
        return Ok(());
    }
    let actual_json =
        serde_json::to_string_pretty(actual).unwrap_or_else(|_| format!("{actual:?}"));
    let expected_json =
        serde_json::to_string_pretty(expected).unwrap_or_else(|_| format!("{expected:?}"));
    Err(format!(
        "inventory mismatch for {attack}: the cell's observed shape changed.\n\
         Review the diff: if the construction changed deliberately, regenerate\n\
         with HATCHLING_ADVERSARIAL_REGEN=1 after review; otherwise this is a\n\
         boundary regression.\n\
         --- committed ---\n{expected_json}\n--- observed ---\n{actual_json}"
    ))
}

#[cfg(test)]
mod tests;
