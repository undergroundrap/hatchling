//! The probe attack implementations.
//!
//! This module runs **inside a real production-spawned cell**: the
//! broker's `spawn_cell` execs the `hatchling-probe` binary under the
//! production mount plan (read-only root, read-only binds) and the
//! production seccomp profile (`SECCOMP_RET_KILL_PROCESS` on anything
//! outside the allowlist), with `NoNewPrivs` set. It performs exactly
//! one attack, selected by the parsed [`Attack`], and writes a single
//! JSON [`Report`](crate::Report) to stdout.
//!
//! The probe is deliberately constrained by the same profile it
//! attacks, so every attack is honest:
//!
//! * No `fork`/`clone`, no directory listing (`getdents64` is denied),
//!   no `/proc` (not mounted), no `prctl`, no `mount`, no `mkdir`, no
//!   `getuid`/`getegid` (the report's ids come from the auxiliary
//!   vector) — the probe cannot enumerate what it cannot name, so
//!   path dictionaries are compiled in and the committed inventory
//!   pins the exact observed set. A construction change that adds a
//!   file changes the inventory and fails loudly.
//! * Stdout is the report channel: the probe writes nothing else
//!   there. Diagnostics go to stderr; the driver reads them on
//!   failure.
//! * Signal-verdict attacks (`seccomp-*`, `privesc-unshare`,
//!   `net-bind`, `tcp-socket`) attempt the raw operation first. If the
//!   filter works the probe dies with `SIGSYS` and never writes a
//!   report — the driver's verdict is the death signal. If the
//!   operation returns, the probe reports the survival (the hole) and
//!   the driver fails.
//!
//! The probe uses only safe wrappers (`rustix`, `hatchling-sys`):
//! its one raw syscall — the seccomp probe itself — goes through
//! `hatchling_sys::process::raw_syscall_inert`, the documented
//! test-only primitive. There is no `unsafe` in this module.

use std::error::Error as _;
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::os::linux::net::SocketAddrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::time::Duration;

use crate::dictionaries::{
    ABSENT_PATHS, ABSTRACT_CANDIDATES, DECOY_SOCKET, FS_SOCKET_CANDIDATES, LIB_CANDIDATES,
};
use crate::{
    Attack, HeldLimit, NO_ERRNO, Outcome, PresenceCheck, Report, ScannedFile, SocketAttempt,
    WriteAttempt, failed_call_ret, os_errno, read_until_eof,
};
use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::{
    AgentMessage, BrokerMessage, EgressOp, PROTOCOL_VERSION, SessionRequest, SessionResponse,
    VaultOp,
};

/// Run one attack inside the cell.
///
/// Reads `HATCHLING_CELL_ID` to select the attack, performs it, and
/// writes the JSON report to stdout. Signal-verdict attacks print the
/// `READY <slug>` marker before the denied operation.
///
/// # Errors
///
/// Returns a message when the cell ID is missing/unparseable, an
/// attack fails to execute, or the report cannot be written.
pub fn run() -> Result<(), String> {
    let cell_id =
        std::env::var("HATCHLING_CELL_ID").map_err(|e| format!("HATCHLING_CELL_ID: {e}"))?;
    let attack = Attack::parse(&cell_id).map_err(|e| format!("cannot parse attack: {e}"))?;
    // Signal-verdict attacks: the raw operation comes first. If the
    // filter works the probe dies here and never reaches the report;
    // any return is a survival the driver must fail on. The READY
    // marker proves the attack itself ran: without it, a SIGSYS during
    // probe startup (a broken allowlist, not a working filter) would
    // be indistinguishable from a kill on the attack.
    if attack.verdict_is_signal() {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        out.write_all(format!("READY {}\n", attack.slug()).as_bytes())
            .map_err(|e| format!("write ready marker: {e}"))?;
        out.flush()
            .map_err(|e| format!("flush ready marker: {e}"))?;
        drop(out);
    }
    match attack {
        Attack::Seccomp(nr) => {
            // Only ever inside a cell: without the filter some numbers
            // really run with `-1` args (see `raw_syscall_inert`). PID 2,
            // the program under a cell's init, means a fresh pid namespace:
            // on a host, pid 2 is the kernel's own thread.
            if std::process::id() != 2 {
                return Err(format!(
                    "refusing seccomp probe {nr}: not PID 2 inside a cell"
                ));
            }
            let ret = raw_syscall_nr(nr);
            emit(attack, Outcome::SyscallSurvived { nr, ret }, None)
        }
        Attack::PrivescUnshare => {
            let nr = hatchling_sys::seccomp::syscall_number("unshare").unwrap_or(0);
            match hatchling_sys::unshare_user() {
                Ok(()) => emit(attack, Outcome::SyscallSurvived { nr, ret: 0 }, None),
                Err(e) => emit(
                    attack,
                    Outcome::SyscallSurvived {
                        nr,
                        ret: failed_call_ret(
                            e.source()
                                .and_then(|source| source.downcast_ref::<std::io::Error>())
                                .map_or(NO_ERRNO, os_errno),
                        ),
                    },
                    Some(format!("unshare returned instead of SIGSYS-killing: {e}")),
                ),
            }
        }
        Attack::NetBind => {
            let nr = hatchling_sys::seccomp::syscall_number("bind").unwrap_or(0);
            let addr = SocketAddr::from_pathname("/tmp/hatchling-probe-bind.sock")
                .map_err(|e| format!("bind sockaddr: {e}"))?;
            // `bind(2)` is outside the allowlist: this must SIGSYS-kill
            // the probe. Any return — Ok or Err — is a survival.
            match UnixListener::bind_addr(&addr) {
                Ok(listener) => {
                    drop(listener);
                    emit(attack, Outcome::SyscallSurvived { nr, ret: 0 }, None)
                }
                Err(e) => emit(
                    attack,
                    Outcome::SyscallSurvived {
                        nr,
                        ret: failed_call_ret(os_errno(&e)),
                    },
                    None,
                ),
            }
        }
        Attack::TcpSocket => {
            let nr = hatchling_sys::seccomp::syscall_number("socket").unwrap_or(0);
            // AF_INET socket(2) to test the filter's family-argument
            // check. `rustix` owns the fd; a surviving call (no SIGSYS)
            // is the failure this attack hunts for.
            let ret = match rustix::net::socket(
                rustix::net::AddressFamily::INET,
                rustix::net::SocketType::STREAM,
                None,
            ) {
                Ok(fd) => i64::from(fd.as_raw_fd()),
                Err(e) => failed_call_ret(e.raw_os_error()),
            };
            emit(attack, Outcome::SyscallSurvived { nr, ret }, None)
        }
        Attack::SocketSweep
        | Attack::MountEscape
        | Attack::SetuidScan
        | Attack::PeerDecoy
        | Attack::PeerGarbage
        | Attack::SessionUniform
        | Attack::NegativeSweep
        | Attack::Groups
        | Attack::RaiseLimits => {
            let outcome = run_report_attack(attack)?;
            emit(attack, outcome, None)
        }
        Attack::ClearDeathSignal => {
            let outcome = outlive_the_supervisor(OUTLIVE_FOR)?;
            emit(attack, outcome, None)
        }
    }
}
/// The raw call's return in the kernel's own form, like every survival
/// report: `-errno` for a failure, which libc's `syscall` answers with
/// -1 and the code left in errno (the CLI profile's soft
/// refusals are told apart by it).
fn raw_syscall_nr(nr: u32) -> i64 {
    let ret = hatchling_sys::process::raw_syscall_inert(nr);
    if ret == -1 {
        failed_call_ret(os_errno(&std::io::Error::last_os_error()))
    } else {
        ret
    }
}

fn run_report_attack(attack: Attack) -> Result<Outcome, String> {
    match attack {
        Attack::SocketSweep => socket_sweep(),
        Attack::MountEscape => Ok(mount_escape()),
        Attack::SetuidScan => Ok(setuid_scan()),
        Attack::PeerDecoy => peer_decoy(),
        Attack::PeerGarbage => peer_garbage(),
        Attack::SessionUniform => session_uniform(),
        Attack::NegativeSweep => Ok(negative_sweep()),
        Attack::Groups => groups(),
        Attack::RaiseLimits => Ok(raise_limits()),
        Attack::Seccomp(_) | Attack::PrivescUnshare | Attack::NetBind | Attack::TcpSocket => {
            Err(String::from("signal-verdict attack has no report form"))
        }
        Attack::ClearDeathSignal => Err(String::from(
            "the death-signal attack reports only if it outlives its supervisor",
        )),
    }
}

fn broker_socket() -> Result<String, String> {
    std::env::var("HATCHLING_BROKER_SOCKET").map_err(|e| format!("HATCHLING_BROKER_SOCKET: {e}"))
}

/// Write the report: one JSON object on stdout, the probe's only
/// stdout write.
fn emit(attack: Attack, outcome: Outcome, error: Option<String>) -> Result<(), String> {
    // `getuid`/`getegid` are outside the allowlist: calling them would
    // SIGSYS-kill the probe after every completed attack.
    let (uid, gid) = hatchling_sys::process::exec_uid_egid();
    let report = Report {
        attack: attack.slug(),
        uid,
        gid,
        pid: std::process::id(),
        outcome,
        error,
    };
    let json = serde_json::to_string(&report).map_err(|e| format!("encode report: {e}"))?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(json.as_bytes())
        .map_err(|e| format!("write report: {e}"))?;
    out.write_all(b"\n")
        .map_err(|e| format!("write report: {e}"))?;
    out.flush().map_err(|e| format!("flush report: {e}"))?;
    Ok(())
}

fn errno_of(error: &std::io::Error) -> String {
    format!("errno:{}", os_errno(error))
}

/// Socket reachability sweep: the broker socket must connect; every
/// other candidate must fail.
fn socket_sweep() -> Result<Outcome, String> {
    let mut attempts = Vec::new();
    let mut fs_sockets = vec![broker_socket()?, String::from(DECOY_SOCKET)];
    fs_sockets.extend(FS_SOCKET_CANDIDATES.iter().map(|s| String::from(*s)));
    for socket in fs_sockets {
        let connect = match UnixStream::connect(&socket) {
            Ok(_) => String::from("ok"),
            Err(e) => errno_of(&e),
        };
        attempts.push(SocketAttempt {
            socket,
            kind: String::from("filesystem"),
            connect,
        });
    }
    for name in ABSTRACT_CANDIDATES {
        let addr = SocketAddr::from_abstract_name(name)
            .map_err(|e| format!("abstract address {name}: {e}"))?;
        let connect = match UnixStream::connect_addr(&addr) {
            Ok(_) => String::from("ok"),
            Err(e) => errno_of(&e),
        };
        attempts.push(SocketAttempt {
            socket: String::from(*name),
            kind: String::from("abstract"),
            connect,
        });
    }
    Ok(Outcome::SocketSweep { attempts })
}

/// Mount-escape probe: create files and directories, and open known
/// files for writing. Every attempt must fail at the syscall level.
fn mount_escape() -> Outcome {
    let mut writes = Vec::with_capacity(32);
    let pid = std::process::id();
    // Directory writability: try to create a file in each.
    for dir in [
        "/", "/sock", "/old", "/tmp", "/etc", "/var", "/lib", "/lib64", "/usr", "/dev", "/proc",
        "/sys",
    ] {
        let path = format!("{dir}/hatchling-probe-{pid}");
        let errno = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => 0,
            Err(e) => os_errno(&e),
        };
        writes.push(WriteAttempt {
            path,
            op: String::from("create"),
            errno,
        });
    }
    // No mkdir attempts: `mkdir`/`mkdirat` are outside the allowlist,
    // so the filter kills them before the mount layer is consulted.
    // That kill is proven per-number by the denied-catalog attack.
    // File writability: open O_WRONLY on every known file.
    let mut files = vec![
        String::from("/agent"),
        String::from("/sock/broker.sock"),
        String::from(DECOY_SOCKET),
    ];
    files.extend(LIB_CANDIDATES.iter().map(|s| String::from(*s)));
    for path in files {
        let errno = match std::fs::OpenOptions::new().write(true).open(&path) {
            Ok(_) => 0,
            Err(e) => os_errno(&e),
        };
        writes.push(WriteAttempt {
            path,
            op: String::from("open_write"),
            errno,
        });
    }
    Outcome::MountEscape { writes }
}

/// Setuid/setgid scan: no visible file may carry either bit. The
/// `NoNewPrivs` bit is asserted by construction (the spawn path sets
/// it before exec); this scan covers the file-based half of the
/// privilege-escalation surface.
fn setuid_scan() -> Outcome {
    let mut files = Vec::new();
    let mut paths = vec![
        String::from("/agent"),
        String::from("/sock/broker.sock"),
        String::from(DECOY_SOCKET),
    ];
    paths.extend(LIB_CANDIDATES.iter().map(|s| String::from(*s)));
    for path in paths {
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let mode = metadata.mode();
        files.push(ScannedFile {
            path,
            setuid: mode & 0o4000 != 0,
            setgid: mode & 0o2000 != 0,
        });
    }
    Outcome::SetuidScan { files }
}

/// Wrong-peer decoy: connect to a socket whose broker expects a
/// different uid, send garbage, and observe the close. The peer gate
/// must answer with zero bytes — no rejection oracle.
fn peer_decoy() -> Result<Outcome, String> {
    let mut stream = match UnixStream::connect(DECOY_SOCKET) {
        Ok(stream) => stream,
        Err(e) => {
            return Ok(Outcome::PeerDecoy {
                connect_errno: Some(os_errno(&e)),
                bytes_read: 0,
                eof: false,
            });
        }
    };
    let _ = stream.write_all(b"this is not a handshake frame");
    let (bytes_read, eof) = read_until_eof(&stream, Duration::from_secs(5))?;
    Ok(Outcome::PeerDecoy {
        connect_errno: None,
        bytes_read,
        eof,
    })
}

/// Authorized-peer garbage: the broker must answer the designed
/// `REJECT` frame — not crash, hang, or leak a different shape.
///
/// The payload is a *complete* frame carrying invalid handshake
/// bytes: that is the exact input the production `REJECT` path is
/// specified for. A framing failure would close without answering,
/// which proves nothing about the reject path.
fn peer_garbage() -> Result<Outcome, String> {
    let socket = broker_socket()?;
    let stream = UnixStream::connect(&socket).map_err(|e| format!("connect: {e}"))?;
    write_frame(&stream, b"{\"kind\":\"nonsense\"}").map_err(|e| format!("write: {e}"))?;
    match read_frame_or_eof(&stream) {
        Ok(Some(frame)) => {
            let message: BrokerMessage =
                serde_json::from_slice(&frame).map_err(|e| format!("decode reply: {e}"))?;
            match message {
                BrokerMessage::Reject { .. } => Ok(Outcome::PeerGarbage {
                    response: String::from("reject"),
                }),
                BrokerMessage::Welcome { .. } => Ok(Outcome::PeerGarbage {
                    response: String::from("other:welcome-to-garbage"),
                }),
            }
        }
        Ok(None) => Ok(Outcome::PeerGarbage {
            response: String::from("closed_no_bytes"),
        }),
        Err(e) => Ok(Outcome::PeerGarbage {
            response: format!("other:{e}"),
        }),
    }
}

/// Session uniform-deny: four ungranted ops through the real session
/// protocol. Every reply must be the identical `Denied`.
fn session_uniform() -> Result<Outcome, String> {
    let socket = broker_socket()?;
    let cell_id =
        std::env::var("HATCHLING_CELL_ID").map_err(|e| format!("HATCHLING_CELL_ID: {e}"))?;
    let stream = UnixStream::connect(&socket).map_err(|e| format!("connect: {e}"))?;
    let hello = serde_json::to_vec(&AgentMessage::Hello {
        v: PROTOCOL_VERSION,
        cell_id,
    })
    .map_err(|e| format!("encode hello: {e}"))?;
    write_frame(&stream, &hello).map_err(|e| format!("write hello: {e}"))?;
    let frame = read_frame(&stream).map_err(|e| format!("read welcome: {e}"))?;
    let welcome: BrokerMessage =
        serde_json::from_slice(&frame).map_err(|e| format!("decode welcome: {e}"))?;
    if !matches!(welcome, BrokerMessage::Welcome { .. }) {
        return Err(format!("expected Welcome, got {welcome:?}"));
    }
    let ops = [
        SessionRequest::Vault(VaultOp::Fetch {
            name: String::from("ungranted-a"),
        }),
        SessionRequest::Vault(VaultOp::Store {
            name: String::from("ungranted-b"),
            secret_b64: String::from("eA=="),
        }),
        SessionRequest::Egress(EgressOp::MintSurrogate {
            destination: String::from("example.com:443"),
            ttl_secs: 60,
        }),
        SessionRequest::Egress(EgressOp::MintSurrogate {
            destination: String::from("other.example:80"),
            ttl_secs: 60,
        }),
    ];
    let mut responses = Vec::with_capacity(ops.len());
    for op in ops {
        let payload = serde_json::to_vec(&op).map_err(|e| format!("encode op: {e}"))?;
        write_frame(&stream, &payload).map_err(|e| format!("write op: {e}"))?;
        let frame = read_frame(&stream).map_err(|e| format!("read response: {e}"))?;
        let response: SessionResponse =
            serde_json::from_slice(&frame).map_err(|e| format!("decode response: {e}"))?;
        responses
            .push(serde_json::to_string(&response).map_err(|e| format!("encode response: {e}"))?);
    }
    Ok(Outcome::SessionUniform { responses })
}

/// Negative sweep: none of these paths may exist in the cell.
/// How long the death-signal attack waits: far past the seconds the
/// driver gives the cell to die once its supervisor is killed.
const OUTLIVE_FOR: Duration = Duration::from_secs(60);

/// Clears the probe's own parent-death signal, says `CLEARED` on stdout,
/// and waits for `spin` ([`OUTLIVE_FOR`] in a cell): the driver kills the
/// cell's supervisor meanwhile, and the probe must die with it. Reaching
/// the end is the hole: the probe outlived its supervisor.
fn outlive_the_supervisor(spin: Duration) -> Result<Outcome, String> {
    rustix::process::set_parent_process_death_signal(None)
        .map_err(|e| format!("clear the death signal: {e}"))?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(b"CLEARED\n")
        .and_then(|()| out.flush())
        .map_err(|e| format!("write CLEARED: {e}"))?;
    drop(out);
    let started = std::time::Instant::now();
    // The CLI profile allows nanosleep: the probe waits, rather than
    // timing a loop.
    std::thread::sleep(spin);
    Ok(Outcome::OutlivedSupervisor {
        spun_secs: started.elapsed().as_secs(),
    })
}

/// The cell's supplementary groups, from its own `/proc/self/status`.
fn groups() -> Result<Outcome, String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| format!("/proc/self/status: {e}"))?;
    let groups = status
        .lines()
        .find_map(|line| line.strip_prefix("Groups:"))
        .ok_or_else(|| String::from("/proc/self/status has no Groups line"))?;
    Ok(Outcome::Groups {
        groups: groups.trim().to_owned(),
    })
}

/// The cell's resource limits, each after trying to lift it to none.
fn raise_limits() -> Outcome {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    let limits = [
        ("processes", Resource::Nproc),
        ("address-space", Resource::As),
        ("file-size", Resource::Fsize),
        ("open-files", Resource::Nofile),
    ]
    .into_iter()
    .map(|(name, resource)| {
        let none = Rlimit {
            current: None,
            maximum: None,
        };
        let raise_errno =
            setrlimit(resource, none).map_or_else(rustix::io::Errno::raw_os_error, |()| 0);
        let held = getrlimit(resource);
        HeldLimit {
            name: name.to_owned(),
            soft: held.current,
            hard: held.maximum,
            raise_errno,
        }
    })
    .collect();
    Outcome::Limits { limits }
}

fn negative_sweep() -> Outcome {
    let mut checks = Vec::with_capacity(ABSENT_PATHS.len());
    for path in ABSENT_PATHS {
        checks.push(PresenceCheck {
            path: String::from(*path),
            exists: std::fs::symlink_metadata(path).is_ok(),
        });
    }
    Outcome::NegativeSweep { checks }
}

#[cfg(test)]
mod tests {
    use super::raw_syscall_nr;
    use crate::HeldLimit;

    #[test]
    fn the_death_signal_attack_clears_it_says_so_and_reports_its_spin() {
        // Outside a cell there's no signal armed to clear, and no spin.
        assert_eq!(
            super::outlive_the_supervisor(std::time::Duration::ZERO),
            Ok(crate::Outcome::OutlivedSupervisor { spun_secs: 0 })
        );
        assert_eq!(
            rustix::process::parent_process_death_signal()
                .ok()
                .flatten(),
            None
        );
        assert_eq!(super::OUTLIVE_FOR.as_secs(), 60);
    }

    #[test]
    fn the_groups_report_is_the_status_files_groups_line() {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let line = status
            .lines()
            .find_map(|line| line.strip_prefix("Groups:"))
            .unwrap()
            .trim()
            .to_owned();
        let expected = Ok(crate::Outcome::Groups { groups: line });
        assert_eq!(super::groups(), expected);
        assert_eq!(super::run_report_attack(crate::Attack::Groups), expected);
    }

    #[test]
    fn the_limits_report_is_each_limit_after_trying_to_lift_it() {
        // In a child held to limits of its own: lifting one is refused
        // with EPERM, unless root may (any but open files, which stops at
        // the kernel's own bound).
        use hatchling_sys::{ForkResult, child_exit_code, exit_child, fork};
        use rustix::process::{Resource, Rlimit, setrlimit};
        let child = match fork().unwrap() {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                let reported = std::panic::catch_unwind(|| {
                    let root = hatchling_sys::euid() == 0;
                    let expected = [
                        ("processes", Resource::Nproc, 100, root),
                        ("address-space", Resource::As, 1 << 40, root),
                        ("file-size", Resource::Fsize, 1 << 30, root),
                        ("open-files", Resource::Nofile, 64, false),
                    ]
                    .map(|(name, resource, value, lifted)| {
                        let held = Some(value);
                        let both = Rlimit {
                            current: held,
                            maximum: held,
                        };
                        assert!(setrlimit(resource, both).is_ok());
                        let held = if lifted { None } else { held };
                        HeldLimit {
                            name: name.to_owned(),
                            soft: held,
                            hard: held,
                            raise_errno: i32::from(!lifted),
                        }
                    });
                    let expected = crate::Outcome::Limits {
                        limits: expected.to_vec(),
                    };
                    super::raise_limits() == expected
                        && super::run_report_attack(crate::Attack::RaiseLimits) == Ok(expected)
                });
                exit_child(match reported {
                    Ok(true) => 0,
                    Ok(false) => 1,
                    Err(_) => 2,
                });
            }
        };
        let status = child
            .wait_deadline(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(child_exit_code(status), 0, "1: another report; 2: panicked");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_raw_call_answers_like_the_kernel() {
        // Two numbers inert with `-1` arguments outside a cell: `getpid`
        // takes none, and `close(-1)` is EBADF.
        assert_eq!(raw_syscall_nr(39), i64::from(std::process::id()), "getpid");
        assert_eq!(raw_syscall_nr(3), -9, "close(-1): -EBADF");
    }
}
