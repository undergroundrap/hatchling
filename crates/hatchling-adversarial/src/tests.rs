//! Unit tests for the adversarial harness helpers: the
//! mutation-kill-floor-1.0 targets. Every pure helper is pinned here;
//! the probe binary and the integration driver are exercised by the
//! suite itself, not by mutation.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::*;

#[test]
fn attack_parse_round_trips_every_variant() -> Result<(), String> {
    let variants = [
        Attack::SocketSweep,
        Attack::MountEscape,
        Attack::Seccomp(0),
        Attack::Seccomp(41),
        Attack::Seccomp(451),
        Attack::PrivescUnshare,
        Attack::SetuidScan,
        Attack::PeerDecoy,
        Attack::PeerGarbage,
        Attack::SessionUniform,
        Attack::NegativeSweep,
        Attack::NetBind,
        Attack::TcpSocket,
        Attack::Groups,
    ];
    for attack in variants {
        let cell_id = attack.cell_id();
        if !cell_id.starts_with(CELL_ID_PREFIX) {
            return Err(format!("cell id {cell_id:?} lacks the prefix"));
        }
        let parsed =
            Attack::parse(&cell_id).map_err(|e| format!("could not re-parse {cell_id:?}: {e}"))?;
        if parsed != attack {
            return Err(format!("round trip changed {attack:?} into {parsed:?}"));
        }
    }
    Ok(())
}

#[test]
fn attack_parse_rejects_garbage() {
    assert_eq!(
        Attack::parse("no-prefix-here"),
        Err(ParseAttackError::MissingPrefix)
    );
    assert_eq!(Attack::parse(""), Err(ParseAttackError::MissingPrefix));
    assert_eq!(
        Attack::parse("wo016-"),
        Err(ParseAttackError::UnknownAttack(String::new()))
    );
    assert_eq!(
        Attack::parse("wo016-frobnicate"),
        Err(ParseAttackError::UnknownAttack(String::from("frobnicate")))
    );
    assert_eq!(
        Attack::parse("wo016-seccomp-"),
        Err(ParseAttackError::BadSyscallNumber(String::new()))
    );
    assert_eq!(
        Attack::parse("wo016-seccomp-abc"),
        Err(ParseAttackError::BadSyscallNumber(String::from("abc")))
    );
    // u32 overflow is not a syscall number.
    assert_eq!(
        Attack::parse("wo016-seccomp-99999999999"),
        Err(ParseAttackError::BadSyscallNumber(String::from(
            "99999999999"
        )))
    );
    // The seccomp prefix without the dash is a different (unknown) slug.
    assert_eq!(
        Attack::parse("wo016-seccomp41"),
        Err(ParseAttackError::UnknownAttack(String::from("seccomp41")))
    );
}

#[test]
fn attack_slugs_are_cell_id_safe() -> Result<(), String> {
    // CellId allows ASCII alphanumerics and '-': the probe's cell id is
    // also the broker handshake's cell_id, so every slug must parse as
    // a CellId.
    for attack in [
        Attack::SocketSweep,
        Attack::MountEscape,
        Attack::Seccomp(165),
        Attack::PrivescUnshare,
        Attack::SetuidScan,
        Attack::PeerDecoy,
        Attack::PeerGarbage,
        Attack::SessionUniform,
        Attack::NegativeSweep,
        Attack::NetBind,
        Attack::TcpSocket,
        Attack::Groups,
    ] {
        let id = attack.cell_id();
        if id.chars().any(|c| !(c.is_ascii_alphanumeric() || c == '-')) {
            return Err(format!("cell id {id:?} is not CellId-safe"));
        }
        if id.len() > 64 {
            return Err(format!("cell id {id:?} exceeds the id length limit"));
        }
    }
    Ok(())
}

#[test]
fn signal_verdict_covers_exactly_the_killed_attacks() {
    assert!(Attack::Seccomp(1).verdict_is_signal());
    assert!(Attack::PrivescUnshare.verdict_is_signal());
    assert!(Attack::NetBind.verdict_is_signal());
    assert!(Attack::TcpSocket.verdict_is_signal());
    assert!(!Attack::SocketSweep.verdict_is_signal());
    assert!(!Attack::MountEscape.verdict_is_signal());
    assert!(!Attack::SetuidScan.verdict_is_signal());
    assert!(!Attack::PeerDecoy.verdict_is_signal());
    assert!(!Attack::PeerGarbage.verdict_is_signal());
    assert!(!Attack::SessionUniform.verdict_is_signal());
    assert!(!Attack::NegativeSweep.verdict_is_signal());
    assert!(!Attack::Groups.verdict_is_signal());
}

#[test]
fn denial_errno_classifier_accepts_only_kernel_denials() {
    // Linux errno ABI: EPERM=1, EACCES=13, EROFS=30, ENOENT=2,
    // ECONNREFUSED=111.
    assert!(is_denial_errno(1));
    assert!(is_denial_errno(13));
    assert!(is_denial_errno(30));
    assert!(!is_denial_errno(0));
    assert!(!is_denial_errno(2));
    assert!(!is_denial_errno(111));
    assert!(!is_denial_errno(22));
    assert!(!is_denial_errno(-1));
}

#[test]
fn refused_write_classifier_pins_each_non_mount_refusal_to_its_entry() {
    // Linux errno ABI: EPERM=1, ENOENT=2, ENXIO=6, EACCES=13,
    // ETXTBSY=26, EROFS=30, EINVAL=22.
    for errno in [1, 2, 13, 30] {
        assert!(is_refused_write("/", "create", errno), "errno {errno}");
    }
    assert!(!is_refused_write("/", "create", 0));
    assert!(!is_refused_write("/", "create", 22));
    // ETXTBSY: only /agent, only opened for writing.
    assert!(is_refused_write("/agent", "open_write", 26));
    assert!(!is_refused_write("/agent", "create", 26));
    assert!(!is_refused_write(
        "/lib/x86_64-linux-gnu/libc.so.6",
        "open_write",
        26
    ));
    assert!(!is_refused_write("/agent", "open_write", 6));
    assert!(!is_refused_write("/agent", "open_write", 0));
    // ENXIO: only sockets, only opened for writing.
    assert!(is_refused_write("/sock/broker.sock", "open_write", 6));
    assert!(!is_refused_write("/sock/broker.sock", "create", 6));
    assert!(!is_refused_write("/sock/broker.sock", "open_write", 0));
    assert!(!is_refused_write("/sock/broker.sock", "open_write", 26));
}

#[test]
fn os_errno_reads_the_kernel_code_or_the_sentinel() {
    assert_eq!(os_errno(&std::io::Error::from_raw_os_error(13)), 13);
    assert_eq!(os_errno(&std::io::Error::other("no os code")), NO_ERRNO);
    assert_eq!(NO_ERRNO, i32::MIN);
}

#[test]
fn failed_call_ret_is_the_negated_errno() {
    assert_eq!(failed_call_ret(13), -13);
    assert_eq!(failed_call_ret(1), -1);
    assert_eq!(failed_call_ret(0), 0);
}

/// A connected pair whose reads give up after 5 s, so a broken
/// `read_until_eof` fails its test instead of hanging it.
fn socket_pair() -> Result<(UnixStream, UnixStream), String> {
    let (ours, theirs) = UnixStream::pair().map_err(|e| format!("socketpair: {e}"))?;
    ours.set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("read timeout: {e}"))?;
    Ok((ours, theirs))
}

#[test]
fn read_until_eof_counts_what_a_chatty_peer_sends() -> Result<(), String> {
    // The positive control the in-cell decoy check cannot give: a peer
    // that does send bytes before closing is seen.
    let (ours, mut theirs) = socket_pair()?;
    theirs
        .write_all(b"hello")
        .map_err(|e| format!("peer write: {e}"))?;
    drop(theirs);
    assert_eq!(read_until_eof(&ours, Duration::from_secs(5))?, (5, true));
    Ok(())
}

#[test]
fn read_until_eof_treats_a_reset_close_as_closed() -> Result<(), String> {
    // Our bytes unread on the peer's side when it closes: Linux AF_UNIX
    // answers the next read with ECONNRESET, the peer gate's case.
    let (mut ours, theirs) = socket_pair()?;
    ours.write_all(b"garbage")
        .map_err(|e| format!("our write: {e}"))?;
    drop(theirs);
    assert_eq!(read_until_eof(&ours, Duration::from_secs(5))?, (0, true));
    Ok(())
}

#[test]
fn read_until_eof_reports_other_read_errors() -> Result<(), String> {
    // /dev/null opened write-only polls readable and then fails the read
    // with EBADF: an error that is not a peer close must not pass as one.
    let null = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .map_err(|e| format!("open /dev/null: {e}"))?;
    assert!(read_until_eof(&null, Duration::from_secs(5)).is_err());
    Ok(())
}

#[test]
fn read_until_eof_times_out_on_a_silent_live_peer() -> Result<(), String> {
    let (ours, _theirs) = socket_pair()?;
    assert_eq!(
        read_until_eof(&ours, Duration::from_millis(200))?,
        (0, false)
    );
    Ok(())
}

#[test]
fn not_found_classifier_accepts_only_enoent() {
    assert!(is_not_found_errno(2));
    assert!(!is_not_found_errno(0));
    assert!(!is_not_found_errno(13));
    assert!(!is_not_found_errno(1));
}

#[test]
fn responses_uniform_needs_identical_nonempty() {
    assert!(!responses_uniform(&[]));
    let one = [serde_json::json!({"kind": "vault", "r": "denied"})];
    assert!(responses_uniform(&one));
    let uniform = [
        serde_json::json!({"kind": "vault", "r": "denied"}),
        serde_json::json!({"kind": "vault", "r": "denied"}),
        serde_json::json!({"kind": "vault", "r": "denied"}),
    ];
    assert!(responses_uniform(&uniform));
    let different = [
        serde_json::json!({"kind": "vault", "r": "denied"}),
        serde_json::json!({"kind": "vault", "r": "denied", "why": "x"}),
    ];
    assert!(!responses_uniform(&different));
    // Field order does not matter; value equality does.
    let reordered = [
        serde_json::json!({"kind": "vault", "a": 1, "b": 2}),
        serde_json::json!({"kind": "vault", "b": 2, "a": 1}),
    ];
    assert!(responses_uniform(&reordered));
    // Missing kind is not uniform (not a valid session response).
    let no_kind = [serde_json::json!({"r": "denied"})];
    assert!(!responses_uniform(&no_kind));
}

#[test]
fn responses_uniform_groups_by_kind() {
    // Vault and egress denies differ ONLY in the kind tag: that is
    // routing, not fingerprinting. Within each kind they must match.
    let mixed = [
        serde_json::json!({"kind": "vault", "r": "denied"}),
        serde_json::json!({"kind": "vault", "r": "denied"}),
        serde_json::json!({"kind": "egress", "r": "denied"}),
        serde_json::json!({"kind": "egress", "r": "denied"}),
    ];
    assert!(responses_uniform(&mixed));
    // But a vault deny that varies by op leaks information.
    let vault_fingerprint = [
        serde_json::json!({"kind": "vault", "r": "denied"}),
        serde_json::json!({"kind": "vault", "r": "denied", "op": "fetch"}),
        serde_json::json!({"kind": "egress", "r": "denied"}),
    ];
    assert!(!responses_uniform(&vault_fingerprint));
}

#[test]
fn all_denied_needs_every_response_denied() {
    assert!(!all_denied(&[]));
    let denied = [
        serde_json::json!({"r": "denied"}),
        serde_json::json!({"r": "denied"}),
    ];
    assert!(all_denied(&denied));
    let mixed = [
        serde_json::json!({"r": "denied"}),
        serde_json::json!({"r": "vault-error"}),
    ];
    assert!(!all_denied(&mixed));
    // The tag must be exactly "denied": a vault-error is not a
    // denial, and a missing tag is not a denial. (A reason-bearing
    // deny is still a denial — uniformity against reason fields is
    // `responses_uniform`'s job.)
    let qualified = [serde_json::json!({"r": "denied", "reason": "no grant"})];
    assert!(all_denied(&qualified));
    let missing_tag = [serde_json::json!({"denied": true})];
    assert!(!all_denied(&missing_tag));
}

#[test]
fn check_inventory_accepts_exact_equality() -> Result<(), String> {
    let outcome = Outcome::NegativeSweep {
        checks: vec![PresenceCheck {
            path: String::from("/etc/crontab"),
            exists: false,
        }],
    };
    check_inventory("negative-sweep", &outcome, &outcome)
}

#[test]
fn check_inventory_reports_both_documents_on_mismatch() {
    let expected = Outcome::NegativeSweep {
        checks: vec![PresenceCheck {
            path: String::from("/etc/crontab"),
            exists: false,
        }],
    };
    let actual = Outcome::NegativeSweep {
        checks: vec![PresenceCheck {
            path: String::from("/etc/crontab"),
            exists: true,
        }],
    };
    let err = check_inventory("negative-sweep", &actual, &expected).expect_err("must mismatch");
    assert!(err.contains("negative-sweep"), "names the attack: {err}");
    assert!(err.contains("/etc/crontab"), "shows the path: {err}");
    assert!(
        err.contains("HATCHLING_ADVERSARIAL_REGEN"),
        "points at regeneration: {err}"
    );
}

#[test]
fn parse_report_round_trips() -> Result<(), String> {
    let report = Report {
        attack: String::from("socket-sweep"),
        uid: 0,
        gid: 0,
        pid: 1,
        outcome: Outcome::SocketSweep {
            attempts: vec![SocketAttempt {
                socket: String::from("/sock/broker.sock"),
                kind: String::from("filesystem"),
                connect: String::from("ok"),
            }],
        },
        error: None,
    };
    let line = serde_json::to_string(&report).map_err(|e| e.to_string())?;
    let parsed = parse_report(&line)?;
    if parsed != report {
        return Err(String::from("report did not survive the round trip"));
    }
    Ok(())
}

#[test]
fn parse_report_rejects_garbage() {
    assert!(parse_report("not json").is_err());
    assert!(parse_report("").is_err());
    assert!(parse_report("{\"attack\": \"x\"}").is_err());
}

#[test]
fn parse_attack_error_display_names_the_problem() {
    assert!(format!("{}", ParseAttackError::MissingPrefix).contains("wo016-"));
    assert!(format!("{}", ParseAttackError::UnknownAttack(String::from("zzz"))).contains("zzz"));
    assert!(format!("{}", ParseAttackError::BadSyscallNumber(String::from("qq"))).contains("qq"));
}

/// The catalog is exactly the header table minus the allowlist: every
/// table row is either allowed or catalogued, never both, never
/// neither. This is the 1:1 synchronization pin — a profile change
/// moves rows between the sets with no catalog edit.
#[test]
#[cfg(target_arch = "x86_64")]
fn denied_catalog_is_the_allowlist_complement() -> Result<(), String> {
    let allowed = hatchling_sys::seccomp::SeccompProfile::agent()
        .map_err(|e| format!("agent profile: {e}"))?
        .allowed_numbers();
    let catalog = denied_syscalls().map_err(|e| format!("catalog: {e}"))?;
    if catalog.is_empty() {
        return Err(String::from("catalog is empty on x86_64"));
    }
    // Sorted, no duplicates.
    let mut nrs: Vec<u32> = catalog.iter().map(|entry| entry.nr).collect();
    nrs.sort_unstable();
    nrs.dedup();
    if nrs.len() != catalog.len() {
        return Err(String::from("catalog has duplicate numbers"));
    }
    if catalog.iter().map(|entry| entry.nr).collect::<Vec<_>>() != nrs {
        return Err(String::from("catalog is not sorted by number"));
    }
    // Disjoint from the allowlist, and jointly exhaustive over the table.
    for row in syscall_table::SYSCALL_TABLE {
        let is_allowed = allowed.contains(&row.nr);
        let is_catalogued = nrs.contains(&row.nr);
        if is_allowed == is_catalogued {
            return Err(format!(
                "syscall {} ({}) is both/neither allowed and catalogued",
                row.name, row.nr
            ));
        }
    }
    // Spot-check the famous denials and the famous allowances.
    for (name, nr) in [("mount", 165), ("clone", 56), ("ptrace", 101)] {
        if !nrs.contains(&nr) {
            return Err(format!("{name} ({nr}) missing from the denied catalog"));
        }
    }
    for (name, nr) in [("read", 0), ("write", 1), ("execve", 59)] {
        if nrs.contains(&nr) {
            return Err(format!("{name} ({nr}) wrongly in the denied catalog"));
        }
    }
    Ok(())
}

#[test]
fn a_catalog_is_the_table_less_what_is_decided_in_number_order() {
    let whole = catalog_without(&[]);
    assert_eq!(whole.len(), syscall_table::SYSCALL_TABLE.len());
    assert!(
        whole
            .windows(2)
            .all(|pair| matches!(pair, [a, b] if a.nr < b.nr))
    );
    let decided = [0, 1, 59];
    let rest = catalog_without(&decided);
    assert_eq!(rest.len(), whole.len() - decided.len());
    assert!(rest.iter().all(|entry| !decided.contains(&entry.nr)));
}

#[test]
#[cfg(target_arch = "x86_64")]
fn the_cli_catalog_is_everything_the_cli_profile_does_not_decide() -> Result<(), String> {
    let profile = hatchling_sys::seccomp::SeccompProfile::harness_cli()
        .map_err(|e| format!("cli profile: {e}"))?;
    let allowed = profile.allowed_numbers();
    let ruled = profile.ruled_numbers();
    let catalog = cli_denied_syscalls().map_err(|e| format!("catalog: {e}"))?;
    let nrs: Vec<u32> = catalog.iter().map(|entry| entry.nr).collect();
    let mut sorted = nrs.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted != nrs {
        return Err(String::from("catalog is not sorted and unique"));
    }
    for row in syscall_table::SYSCALL_TABLE {
        let decided = allowed.contains(&row.nr) || ruled.contains(&row.nr);
        if decided == nrs.contains(&row.nr) {
            return Err(format!(
                "syscall {} ({}) is both/neither decided and catalogued",
                row.name, row.nr
            ));
        }
    }
    // Narrower than the agent's: everything the CLI profile kills, the
    // agent's kills too.
    let agent: Vec<u32> = denied_syscalls()
        .map_err(|e| format!("agent catalog: {e}"))?
        .iter()
        .map(|entry| entry.nr)
        .collect();
    if let Some(nr) = nrs.iter().find(|nr| !agent.contains(nr)) {
        return Err(format!("{nr} is killed for the CLIs but not for the agent"));
    }
    for (name, nr) in [("mount", 165), ("ptrace", 101), ("unshare", 272)] {
        if !nrs.contains(&nr) {
            return Err(format!("{name} ({nr}) missing from the CLI catalog"));
        }
    }
    for (name, nr) in [
        ("clone", 56),
        ("socket", 41),
        ("clone3", 435),
        ("wait4", 61),
    ] {
        if nrs.contains(&nr) {
            return Err(format!("{name} ({nr}) wrongly in the CLI catalog"));
        }
    }
    Ok(())
}

/// The generated table covers the live kernel headers: every number
/// they define is in the table under the same name, so a table
/// generated from older headers fails loudly here, pointing at the
/// generator script.
///
/// Extra numbers are allowed. The table is generated on the machine
/// with the newest headers (the newer box's headers define syscalls the older VM's
/// lack), and exact equality could then never hold on both. An extra
/// number is still outside the allowlist, and the adversarial suite
/// proves the filter kills it.
#[test]
#[cfg(target_arch = "x86_64")]
fn syscall_table_matches_live_headers() -> Result<(), String> {
    let header = "/usr/include/x86_64-linux-gnu/asm/unistd_64.h";
    let text = std::fs::read_to_string(header)
        .map_err(|e| format!("cannot read kernel headers at {header}: {e}"))?;
    let mut live: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("#define __NR_") else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let (Some(name), Some(nr)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Ok(nr): Result<u32, _> = nr.parse() else {
            continue;
        };
        live.entry(nr).or_insert_with(|| name.to_owned());
    }
    if live.is_empty() {
        return Err(format!("no __NR_ defines parsed from {header}"));
    }
    let table: std::collections::BTreeMap<u32, &str> = syscall_table::SYSCALL_TABLE
        .iter()
        .map(|row| (row.nr, row.name))
        .collect();
    if table.len() != syscall_table::SYSCALL_TABLE.len() {
        return Err(String::from("generated table has duplicate numbers"));
    }
    let missing: Vec<u32> = live
        .keys()
        .filter(|nr| !table.contains_key(nr))
        .copied()
        .collect();
    let renamed: Vec<String> = live
        .iter()
        .filter_map(|(nr, name)| match table.get(nr) {
            Some(row) if *row != name.as_str() => Some(format!("{nr}: {row} vs {name}")),
            _ => None,
        })
        .collect();
    if !missing.is_empty() || !renamed.is_empty() {
        return Err(format!(
            "generated table is behind {header}: missing {missing:?}, renamed {renamed:?}. \
             Regenerate with scripts/gen_seccomp_catalog.py on the machine with the newest headers"
        ));
    }
    Ok(())
}
