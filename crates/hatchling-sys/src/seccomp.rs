//! Seccomp-bpf allowlist profiles.
//!
//! A [`SeccompProfile`] is an allowlist of syscall names. [`build`]ing
//! it produces a classic-BPF program enforcing *fail-closed*: the
//! process may invoke exactly the listed syscalls (with the listed
//! argument constraints); anything else kills the process with
//! `SECCOMP_RET_KILL_PROCESS`.
//!
//! Design rules, all deliberate:
//!
//! - Syscalls are named, never numbered, at the API boundary. Numbers
//!   differ per architecture; the per-arch tables below are the only
//!   place numbers appear, and [`SeccompProfile::allow`] rejects names
//!   the table does not know. A typo'd syscall can never silently become
//!   an unfiltered hole.
//! - `socket` is constrained to `AF_UNIX`: the agent's only legitimate
//!   socket family is the broker socket. The constraint is enforced in
//!   the filter itself (on `args[0]`), not by convention.
//! - The filter checks the audit architecture first, so a syscall
//!   number can never be confused across architectures.
//! - The default action is `KILL_PROCESS`, unconditionally. There is no
//!   "log" mode: this profile is a security boundary, not a debugging aid.
//!   The one errno answer is a rule's, on a call it names: a real
//!   CLI's probe for a socket family, `prctl` option or `ioctl` it may not
//!   use fails as on a box without it, instead of killing the CLI.

use crate::Error;

/// One classic-BPF instruction.
///
/// `#[repr(C)]` with the same field order as `libc::sock_filter`, so
/// the layout matches `struct sock_filter` exactly. A dedicated type
/// (rather than `libc`'s) keeps `PartialEq` for the structural unit
/// tests that pin the program layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct SockFilter {
    /// The BPF opcode.
    pub code: u16,
    /// Jump offset if true.
    pub jt: u8,
    /// Jump offset if false.
    pub jf: u8,
    /// Immediate / offset operand.
    pub k: u32,
}

// Composite BPF instruction codes, precomputed as literals.
//
// The components (from linux/filter.h) have non-overlapping bits
// (several are zero), so `|` and `^` would compute the same value —
// writing the composites as literals keeps the encoding exact and
// leaves no operator for mutation to target. Derivations:
//   BPF_LD_W_ABS = BPF_LD(0x00) | BPF_W(0x00) | BPF_ABS(0x20) = 0x20
//   BPF_JMP_JEQ_K = BPF_JMP(0x05) | BPF_JEQ(0x10) | BPF_K(0x00) = 0x15
//   BPF_JMP_JA = BPF_JMP(0x05) | BPF_JA(0x00) = 0x05
//   BPF_RET_K = BPF_RET(0x06) | BPF_K(0x00) = 0x06
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_JMP_JA: u16 = 0x05;
const BPF_RET_K: u16 = 0x06;
//   BPF_JMP_JSET_K = BPF_JMP(0x05) | BPF_JSET(0x40) | BPF_K(0x00) = 0x45
const BPF_JMP_JSET_K: u16 = 0x45;

// seccomp return actions (linux/seccomp.h).
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
// The errno sits in the low 16 bits, under the action: added, as the bits
// never overlap.
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

// `struct seccomp_data` field offsets.
const SECCOMP_DATA_NR: u32 = 0;
const SECCOMP_DATA_ARCH: u32 = 4;
const SECCOMP_DATA_ARGS: u32 = 16;

// AF_UNIX, for the socket family constraint.
const AF_UNIX: u32 = 1;

/// Per-architecture syscall table: (name, number).
///
/// The agent's synchronous surface: framed socket IO, env reads,
/// logging to stderr, the Rust runtime's startup needs, and the one
/// `execve` the spawner performs under the filter. Anything the agent
/// does not demonstrably need is absent — additions require a design
/// review (see the constraints).
#[cfg(target_arch = "x86_64")]
const SYSCALL_TABLE: &[(&str, u32)] = &[
    ("read", 0),
    ("write", 1),
    ("close", 3),
    ("mmap", 9),
    ("mprotect", 10),
    ("munmap", 11),
    ("brk", 12),
    ("rt_sigaction", 13),
    ("rt_sigprocmask", 14),
    ("rt_sigreturn", 15),
    ("socket", 41),
    ("connect", 42),
    ("getsockopt", 55),
    ("execve", 59),
    ("exit", 60),
    ("arch_prctl", 158),
    ("sigaltstack", 131),
    ("getpid", 39),
    ("gettid", 186),
    ("futex", 202),
    ("clock_gettime", 228),
    ("exit_group", 231),
    ("openat", 257),
    // glibc >= 2.43's ld.so calls access("/etc/ld.so.preload") before
    // main; without it the agent dies with SIGSYS at startup.
    ("access", 21),
    // strace of the agent's real startup (x86_64, glibc 2.43):
    // ld.so issues pread64/fstat/statx while loading libc, Rust's std
    // probes fds with poll, and early runtime init calls lseek,
    // sched_getaffinity, and prlimit64 — all before main. Without them
    // the agent dies with SIGSYS at startup on newer distros. Numbers
    // verified against /usr/include/x86_64-linux-gnu/asm/unistd_64.h.
    // aarch64 has these syscalls too, but their numbers are unverified
    // against kernel headers here, so they stay x86_64-only for now.
    // Cargo's test env sets LD_LIBRARY_PATH, which makes ld.so probe
    // glibc-hwcaps subdirectories with newfstatat while resolving
    // libgcc_s — without this the agent dies with SIGSYS at startup
    // whenever LD_LIBRARY_PATH is set. Number verified against
    // /usr/include/x86_64-linux-gnu/asm/unistd_64.h; x86_64-only until
    // aarch64 numbers are verified against kernel headers.
    ("fstatat", 262),
    ("fstat", 5),
    ("poll", 7),
    ("lseek", 8),
    ("pread64", 17),
    ("sched_getaffinity", 204),
    ("prlimit64", 302),
    ("statx", 332),
    // Handshake I/O (x86_64, observed via strace on the agent's real
    // handshake): std's UnixStream writes HELLO with sendto and reads
    // WELCOME with recvfrom, and issues fcntl(fd, F_GETFD) once on the
    // broker socket during teardown — inside std, not our code, on the
    // success path. Without these the agent dies with SIGSYS partway
    // through the handshake. Numbers verified against
    // /usr/include/x86_64-linux-gnu/asm/unistd_64.h; x86_64-only until
    // aarch64 numbers are verified against kernel headers.
    ("sendto", 44),
    ("recvfrom", 45),
    ("fcntl", 72),
    ("set_tid_address", 218),
    ("set_robust_list", 273),
    ("getrandom", 318),
    ("rseq", 334),
    // The harness profile's file calls: a harness edits its work
    // tree. Numbers verified against
    // /usr/include/x86_64-linux-gnu/asm/unistd_64.h; x86_64-only until
    // aarch64 numbers are verified against kernel headers.
    ("pwrite64", 18),
    ("fsync", 74),
    ("fdatasync", 75),
    ("ftruncate", 77),
    ("getcwd", 79),
    ("chdir", 80),
    ("rename", 82),
    ("mkdir", 83),
    ("rmdir", 84),
    ("unlink", 87),
    ("readlink", 89),
    ("fchmod", 91),
    ("getdents64", 217),
    ("mkdirat", 258),
    ("unlinkat", 263),
    ("renameat", 264),
    ("readlinkat", 267),
    ("fchmodat", 268),
    ("utimensat", 280),
    ("renameat2", 316),
    // The real harness CLIs' calls: Claude Code 2.1.286 and the
    // Codex CLI 0.162.0, with the bash, git, ripgrep and coreutils they
    // ran, traced end to end (strace -f, x86_64, glibc 2.43).
    // Threads and children, pipes, epoll, inotify, the identity getters,
    // and the TCP calls to the relay on the cell's own loopback. Numbers
    // verified against /usr/include/x86_64-linux-gnu/asm/unistd_64.h;
    // x86_64-only until aarch64 numbers are verified.
    ("open", 2),
    ("stat", 4),
    ("lstat", 6),
    ("ioctl", 16),
    ("readv", 19),
    ("writev", 20),
    ("pipe", 22),
    ("sched_yield", 24),
    ("madvise", 28),
    ("dup", 32),
    ("dup2", 33),
    ("nanosleep", 35),
    ("shutdown", 48),
    ("bind", 49),
    ("listen", 50),
    ("getsockname", 51),
    ("getpeername", 52),
    ("socketpair", 53),
    ("setsockopt", 54),
    ("clone", 56),
    ("vfork", 58),
    ("wait4", 61),
    ("kill", 62),
    ("uname", 63),
    ("flock", 73),
    ("symlink", 88),
    ("chmod", 90),
    ("fchown", 93),
    ("umask", 95),
    ("getrusage", 98),
    ("sysinfo", 99),
    ("getuid", 102),
    ("getgid", 104),
    ("geteuid", 107),
    ("getegid", 108),
    ("setpgid", 109),
    ("getppid", 110),
    ("getpgrp", 111),
    ("setsid", 112),
    ("getgroups", 115),
    ("getresuid", 118),
    ("getresgid", 120),
    ("statfs", 137),
    ("sched_getparam", 143),
    ("sched_getscheduler", 145),
    ("prctl", 157),
    ("listxattr", 194),
    ("restart_syscall", 219),
    ("clock_nanosleep", 230),
    ("epoll_ctl", 233),
    ("inotify_add_watch", 254),
    ("inotify_rm_watch", 255),
    ("pselect6", 270),
    ("splice", 275),
    ("epoll_pwait", 281),
    ("eventfd2", 290),
    ("epoll_create1", 291),
    ("pipe2", 293),
    ("inotify_init1", 294),
    ("copy_file_range", 326),
    // Seen only in a real cell: Claude Code reads its children's pipes
    // with preadv2(RWF_NOWAIT).
    ("preadv2", 327),
    ("pidfd_open", 434),
    ("clone3", 435),
    ("close_range", 436),
    ("faccessat2", 439),
    ("epoll_pwait2", 441),
];

/// aarch64 has no `arch_prctl`; the rest of the surface is the same
/// shape with different numbers.
#[cfg(target_arch = "aarch64")]
const SYSCALL_TABLE: &[(&str, u32)] = &[
    ("read", 63),
    ("write", 64),
    ("close", 57),
    ("mmap", 222),
    ("mprotect", 226),
    ("munmap", 215),
    ("brk", 214),
    ("rt_sigaction", 134),
    ("rt_sigprocmask", 135),
    ("rt_sigreturn", 139),
    ("socket", 198),
    ("connect", 203),
    ("getsockopt", 209),
    ("execve", 221),
    ("exit", 93),
    ("sigaltstack", 132),
    ("getpid", 172),
    ("gettid", 178),
    ("futex", 98),
    ("clock_gettime", 113),
    ("exit_group", 94),
    ("openat", 56),
    ("set_tid_address", 96),
    ("set_robust_list", 99),
    ("getrandom", 278),
    ("rseq", 293),
];

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!(
    "hatchling-sys seccomp: no audited syscall table for this architecture; \
     add one (with numbers verified against the kernel headers) or the cell \
     cannot be built here"
);

/// The audit architecture value `seccomp_data.arch` must equal.
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000_003E; // AUDIT_ARCH_X86_64
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC000_00B7; // AUDIT_ARCH_AARCH64

/// Looks up a syscall number by name for this architecture.
#[must_use]
pub fn syscall_number(name: &str) -> Option<u32> {
    SYSCALL_TABLE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, nr)| *nr)
}

/// What a filtered call gets when its check fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The process is killed, as for any call not allowed.
    Kill,
    /// The call fails with this errno and the process goes on: for a probe
    /// the program handles, such as `socket(AF_INET6)` on a box without it.
    Errno(u32),
}

/// One argument check on an allowed call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Check {
    /// The argument numbered `arg` (its low 32 bits) is one of `values`.
    OneOf { arg: u32, values: Vec<u32> },
    /// The first argument (its low 32 bits) has none of `mask`'s bits.
    NoneOf { mask: u32 },
    /// Never: the call always gets the refusal.
    Never,
}

/// A call whose arguments decide, and what it gets otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    nr: u32,
    check: Check,
    otherwise: Refusal,
}

/// A seccomp allowlist profile under construction.
#[derive(Debug, Clone, Default)]
pub struct SeccompProfile {
    allowed: Vec<u32>,
    rules: Vec<Rule>,
}

// The argument values the harness CLI profile admits. Each is the
// kernel's own constant (linux/sched.h, linux/prctl.h, asm-generic/ioctls.h,
// linux/fs.h), written as a literal so no operator is left for mutation.
// Namespace flags `clone` may not carry: NEWNS 0x0002_0000, NEWCGROUP
// 0x0200_0000, NEWUTS 0x0400_0000, NEWIPC 0x0800_0000, NEWUSER
// 0x1000_0000, NEWPID 0x2000_0000, NEWNET 0x4000_0000.
const CLONE_NAMESPACES: u32 = 0x7E02_0000;
const AF_INET: u32 = 2;
const EPERM: u32 = 1;
const ENOTTY: u32 = 25;
const ENOSYS: u32 = 38;
const EAFNOSUPPORT: u32 = 97;
/// `prctl` options the CLIs were seen to use: `PR_SET_PDEATHSIG`,
/// `PR_SET_NAME`, `PR_GET_AUXV`, `PR_SET_VMA`.
const PRCTL_OPTIONS: [u32; 4] = [1, 15, 0x4155_5856, 0x5356_4D41];
/// `ioctl` requests the CLIs were seen to use: `TIOCGPGRP`, `TIOCGWINSZ`,
/// `FIONBIO`, `TIOCGPTN`, `TCGETS2`, `FICLONE`: terminal and pipe queries,
/// and reflink copies. Nothing that writes a terminal (`TIOCSTI`).
const IOCTL_REQUESTS: [u32; 6] = [
    0x540F,
    0x5413,
    0x5421,
    0x8004_5430,
    0x802C_542A,
    0x4004_9409,
];

/// The calls the real harness CLIs were traced making, beyond the harness
/// profile and the ones [`SeccompProfile::harness_cli`] checks by argument.
#[cfg(target_arch = "x86_64")]
const CLI_CALLS: [&str; 61] = [
    "open",
    "stat",
    "lstat",
    "readv",
    "writev",
    "pipe",
    "sched_yield",
    "madvise",
    "dup",
    "dup2",
    "nanosleep",
    "shutdown",
    "bind",
    "listen",
    "getsockname",
    "getpeername",
    "setsockopt",
    "vfork",
    "wait4",
    "kill",
    "uname",
    "flock",
    "symlink",
    "chmod",
    "fchown",
    "umask",
    "getrusage",
    "sysinfo",
    "getuid",
    "getgid",
    "geteuid",
    "getegid",
    "setpgid",
    "getppid",
    "getpgrp",
    "setsid",
    "getgroups",
    "getresuid",
    "getresgid",
    "statfs",
    "sched_getparam",
    "sched_getscheduler",
    "listxattr",
    "restart_syscall",
    "clock_nanosleep",
    "epoll_ctl",
    "inotify_add_watch",
    "inotify_rm_watch",
    "pselect6",
    "splice",
    "epoll_pwait",
    "eventfd2",
    "epoll_create1",
    "pipe2",
    "inotify_init1",
    "copy_file_range",
    "preadv2",
    "pidfd_open",
    "close_range",
    "faccessat2",
    "epoll_pwait2",
];

impl SeccompProfile {
    /// Allows a syscall by name when `check` passes; otherwise it gets
    /// `otherwise`.
    fn allow_if(&mut self, name: &str, check: Check, otherwise: Refusal) -> Result<(), Error> {
        self.allow(name)?;
        let nr = syscall_number(name).ok_or_else(|| Error::UnknownSyscall {
            name: name.to_owned(),
        })?;
        self.rules.retain(|rule| rule.nr != nr);
        self.rules.push(Rule {
            nr,
            check,
            otherwise,
        });
        Ok(())
    }

    /// Answers a syscall by name with `errno`, never running it.
    fn refuse(&mut self, name: &str, errno: u32) -> Result<(), Error> {
        let nr = syscall_number(name).ok_or_else(|| Error::UnknownSyscall {
            name: name.to_owned(),
        })?;
        self.allowed.retain(|allowed| *allowed != nr);
        self.rules.retain(|rule| rule.nr != nr);
        self.rules.push(Rule {
            nr,
            check: Check::Never,
            otherwise: Refusal::Errno(errno),
        });
        Ok(())
    }
    /// Allows a syscall by name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownSyscall`] if `name` is not in the
    /// per-arch table. Unknown names fail here, at build time — never
    /// as a silent hole in the installed filter.
    pub fn allow(&mut self, name: &str) -> Result<(), Error> {
        let nr = syscall_number(name).ok_or_else(|| Error::UnknownSyscall {
            name: name.to_owned(),
        })?;
        if !self.allowed.contains(&nr) {
            self.allowed.push(nr);
        }
        Ok(())
    }

    /// The agent's curated allowlist.
    ///
    /// Covers the synchronous agent's real surface: socket IO to the
    /// broker, env reads, stderr logging, the Rust runtime's startup
    /// (TLS, signal handling, allocator, `rseq`), the dynamic loader's
    /// pre-main probes (glibc >= 2.43: `access`, `pread64`, `fstat`,
    /// `fstatat`, `statx`) and the runtime's early init (`poll`,
    /// `lseek`, `sched_getaffinity`, `prlimit64`), the handshake's
    /// socket IO (`sendto`, `recvfrom`, `fcntl`), and the spawner's
    /// single `execve`. `socket` is further constrained to `AF_UNIX` in
    /// the built program.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownSyscall`] if a curated name is missing
    /// from the table — a compile-time table bug surfacing loudly
    /// instead of shipping a broken filter.
    pub fn agent() -> Result<Self, Error> {
        let mut profile = Self::default();
        for name in [
            "read",
            "write",
            "openat",
            "close",
            "mmap",
            "mprotect",
            "munmap",
            "brk",
            "rt_sigaction",
            "rt_sigprocmask",
            "rt_sigreturn",
            "sigaltstack",
            "socket",
            "connect",
            "getsockopt",
            "execve",
            "exit",
            "exit_group",
            "getpid",
            "gettid",
            "getrandom",
            "futex",
            "clock_gettime",
            "set_tid_address",
            "set_robust_list",
            "rseq",
        ] {
            profile.allow(name)?;
        }
        #[cfg(target_arch = "x86_64")]
        profile.allow("arch_prctl")?;
        // aarch64 has no `access` syscall (it is `faccessat` there);
        // the table carries no entry, so this stays x86_64-only.
        #[cfg(target_arch = "x86_64")]
        profile.allow("access")?;
        // The startup set from the table above: without these the
        // agent dies with SIGSYS before main on glibc >= 2.43.
        // aarch64 numbers are unverified (see table), so x86_64-only.
        #[cfg(target_arch = "x86_64")]
        for name in [
            "fstatat",
            "fstat",
            "poll",
            "lseek",
            "pread64",
            "sched_getaffinity",
            "prlimit64",
            "statx",
        ] {
            profile.allow(name)?;
        }
        // Handshake I/O from the table above: without sendto/recvfrom
        // the agent cannot exchange HELLO/WELCOME, and without fcntl
        // it dies with SIGSYS during teardown (observed via strace on
        // the success path; the call is inside std, not our code).
        // aarch64 numbers are unverified (see table), so x86_64-only.
        #[cfg(target_arch = "x86_64")]
        for name in ["sendto", "recvfrom", "fcntl"] {
            profile.allow(name)?;
        }
        Ok(profile)
    }

    /// The harness cell's profile: the agent's, plus the file
    /// calls a harness needs to edit its work tree (create, write,
    /// truncate, sync, rename, remove, list, chmod, and the working
    /// directory). Still no process creation (`clone`, `fork`), no
    /// network beyond `AF_UNIX`, and no mount, namespace or tracing call:
    /// a harness that needs those waits for the follow-up WO that gives
    /// harnesses their network and their own processes.
    ///
    /// # Errors
    ///
    /// As [`SeccompProfile::agent`].
    pub fn harness() -> Result<Self, Error> {
        let mut profile = Self::agent()?;
        // aarch64 numbers are unverified (see table), so x86_64-only.
        #[cfg(target_arch = "x86_64")]
        for name in [
            "pwrite64",
            "fsync",
            "fdatasync",
            "ftruncate",
            "getcwd",
            "chdir",
            "rename",
            "mkdir",
            "rmdir",
            "unlink",
            "readlink",
            "fchmod",
            "getdents64",
            "mkdirat",
            "unlinkat",
            "renameat",
            "readlinkat",
            "fchmodat",
            "utimensat",
            "renameat2",
        ] {
            profile.allow(name)?;
        }
        Ok(profile)
    }

    /// The real harness CLIs' profile: the harness profile, plus
    /// the calls Claude Code and the Codex CLI were traced making, with
    /// the shell, git, ripgrep and coreutils they ran. That is threads and
    /// child processes, pipes, polling, and TCP on the cell's own
    /// loopback, where the broker's relay is the only listener.
    ///
    /// Argument checks keep it the smallest such surface:
    /// - `clone` never takes a namespace flag: that is the kill. `clone3`,
    ///   whose flags sit behind a pointer the filter cannot read, answers
    ///   `ENOSYS`, and threads fall back to `clone`.
    /// - `socket` is `AF_UNIX` or `AF_INET` only; any other family answers
    ///   `EAFNOSUPPORT`. `socketpair` is `AF_UNIX` only, answered the
    ///   same way otherwise: a pair is never a network socket.
    /// - `prctl` and `ioctl` take only the options and requests the CLIs
    ///   were seen to use; anything else answers `EPERM` or `ENOTTY`.
    ///
    /// Still nothing that mounts, traces, makes namespaces, loads code
    /// into the kernel, or reaches another process's memory.
    ///
    /// # Errors
    ///
    /// As [`SeccompProfile::agent`].
    pub fn harness_cli() -> Result<Self, Error> {
        let mut profile = Self::harness()?;
        #[cfg(target_arch = "x86_64")]
        {
            for name in CLI_CALLS {
                profile.allow(name)?;
            }
            profile.allow_if(
                "socket",
                Check::OneOf {
                    arg: 0,
                    values: vec![AF_UNIX, AF_INET],
                },
                Refusal::Errno(EAFNOSUPPORT),
            )?;
            profile.allow_if(
                "socketpair",
                Check::OneOf {
                    arg: 0,
                    values: vec![AF_UNIX],
                },
                Refusal::Errno(EAFNOSUPPORT),
            )?;
            profile.allow_if(
                "clone",
                Check::NoneOf {
                    mask: CLONE_NAMESPACES,
                },
                Refusal::Kill,
            )?;
            profile.refuse("clone3", ENOSYS)?;
            profile.allow_if(
                "prctl",
                Check::OneOf {
                    arg: 0,
                    values: PRCTL_OPTIONS.to_vec(),
                },
                Refusal::Errno(EPERM),
            )?;
            profile.allow_if(
                "ioctl",
                Check::OneOf {
                    arg: 1,
                    values: IOCTL_REQUESTS.to_vec(),
                },
                Refusal::Errno(ENOTTY),
            )?;
        }
        Ok(profile)
    }

    /// The allowed syscall numbers, sorted.
    #[must_use]
    pub fn allowed_numbers(&self) -> Vec<u32> {
        let mut numbers = self.allowed.clone();
        numbers.sort_unstable();
        numbers
    }

    /// The syscall numbers the profile decides by their arguments, or
    /// answers with an errno, sorted. A ruled number may also
    /// be allowed: the rule decides which of its calls run.
    #[must_use]
    pub fn ruled_numbers(&self) -> Vec<u32> {
        let mut numbers: Vec<u32> = self.rules.iter().map(|rule| rule.nr).collect();
        numbers.sort_unstable();
        numbers
    }

    /// Compiles the profile to a classic-BPF program.
    ///
    /// Layout (all jumps forward — classic BPF offsets are unsigned):
    ///
    /// ```text
    /// 0: A = arch; if A != AUDIT_ARCH goto kill
    /// 1: A = nr
    /// 2: [if socket allowed] if A == socket_nr goto check_af_unix
    /// 3..: if A == allowed[i] goto allow   (linear scan)
    /// scan_end: goto kill
    /// check_af_unix: A = args[0]; if A == AF_UNIX goto allow else goto kill
    /// allow: return ALLOW
    /// kill: return KILL_PROCESS
    /// ```
    ///
    /// The linear scan is O(n) in the allowlist size (~28 syscalls):
    /// simple to audit, and the filter runs once per syscall on a
    /// non-hot path (the agent is IO-bound on the broker socket).
    #[must_use]
    pub fn build(&self) -> Vec<SockFilter> {
        assemble(&self.allowed_numbers(), &self.effective_rules())
    }

    /// The rules the program enforces: the standing `socket` rule first
    /// (an allowed `socket` is `AF_UNIX` or the kill) unless the profile
    /// set its own, then the profile's.
    fn effective_rules(&self) -> Vec<Rule> {
        let standing = syscall_number("socket")
            .filter(|socket| {
                self.allowed.contains(socket) && !self.rules.iter().any(|rule| rule.nr == *socket)
            })
            .map(|socket| Rule {
                nr: socket,
                check: Check::OneOf {
                    arg: 0,
                    values: vec![AF_UNIX],
                },
                otherwise: Refusal::Kill,
            });
        standing
            .into_iter()
            .chain(self.rules.iter().cloned())
            .collect()
    }
}

/// Where a jump lands, before the program is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Label {
    /// The next instruction (offset 0).
    Next,
    /// The `ALLOW` return.
    Allow,
    /// The `KILL_PROCESS` return.
    Kill,
    /// The argument check of the rule at this index.
    Block(usize),
    /// The `ERRNO` return for this errno.
    Errno(u32),
}

/// An instruction whose jump targets are still labels.
struct Pending {
    ins: SockFilter,
    jt: Label,
    jf: Label,
    /// The target of an unconditional jump (`BPF_JA`), which the `k`
    /// operand carries.
    ja: Option<Label>,
}

impl Pending {
    /// An instruction with no jump.
    const fn plain(code: u16, k: u32) -> Self {
        Self {
            ins: SockFilter {
                code,
                jt: 0,
                jf: 0,
                k,
            },
            jt: Label::Next,
            jf: Label::Next,
            ja: None,
        }
    }

    /// A conditional jump.
    const fn branch(code: u16, k: u32, jt: Label, jf: Label) -> Self {
        Self {
            ins: SockFilter {
                code,
                jt: 0,
                jf: 0,
                k,
            },
            jt,
            jf,
            ja: None,
        }
    }
}

/// The label a refusal returns through.
const fn refusal(otherwise: Refusal) -> Label {
    match otherwise {
        Refusal::Kill => Label::Kill,
        Refusal::Errno(errno) => Label::Errno(errno),
    }
}

/// Compiles an allowlist and its argument rules to classic BPF.
///
/// Layout (all jumps forward, as classic BPF requires):
///
/// ```text
/// A = arch; if A != AUDIT_ARCH goto kill
/// A = nr
/// for each rule: if A == rule.nr goto its block (or straight to its refusal)
/// for each other allowed nr: if A == nr goto allow
/// goto kill
/// each rule's block: A = args[n]; compare; goto allow or its refusal
/// allow: return ALLOW
/// one return per errno a rule refuses with: return ERRNO | errno
/// kill: return KILL_PROCESS
/// ```
fn assemble(allowed: &[u32], rules: &[Rule]) -> Vec<SockFilter> {
    let mut pending = vec![
        Pending::plain(BPF_LD_W_ABS, SECCOMP_DATA_ARCH),
        Pending::branch(BPF_JMP_JEQ_K, AUDIT_ARCH, Label::Next, Label::Kill),
        Pending::plain(BPF_LD_W_ABS, SECCOMP_DATA_NR),
    ];
    for (index, rule) in rules.iter().enumerate() {
        let target = match rule.check {
            Check::Never => refusal(rule.otherwise),
            Check::OneOf { .. } | Check::NoneOf { .. } => Label::Block(index),
        };
        pending.push(Pending::branch(BPF_JMP_JEQ_K, rule.nr, target, Label::Next));
    }
    for nr in allowed {
        if !rules.iter().any(|rule| rule.nr == *nr) {
            pending.push(Pending::branch(
                BPF_JMP_JEQ_K,
                *nr,
                Label::Allow,
                Label::Next,
            ));
        }
    }
    pending.push(Pending {
        ja: Some(Label::Kill),
        ..Pending::plain(BPF_JMP_JA, 0)
    });
    let mut positions: Vec<(Label, usize)> = Vec::new();
    for (index, rule) in rules.iter().enumerate() {
        let otherwise = refusal(rule.otherwise);
        match &rule.check {
            Check::Never => {}
            Check::OneOf { arg, values } => {
                positions.push((Label::Block(index), pending.len()));
                pending.push(Pending::plain(BPF_LD_W_ABS, SECCOMP_DATA_ARGS + arg * 8));
                for (at, value) in values.iter().enumerate() {
                    let last = at + 1 == values.len();
                    let jf = if last { otherwise } else { Label::Next };
                    pending.push(Pending::branch(BPF_JMP_JEQ_K, *value, Label::Allow, jf));
                }
            }
            Check::NoneOf { mask } => {
                positions.push((Label::Block(index), pending.len()));
                pending.push(Pending::plain(BPF_LD_W_ABS, SECCOMP_DATA_ARGS));
                pending.push(Pending::branch(
                    BPF_JMP_JSET_K,
                    *mask,
                    otherwise,
                    Label::Allow,
                ));
            }
        }
    }
    positions.push((Label::Allow, pending.len()));
    pending.push(Pending::plain(BPF_RET_K, SECCOMP_RET_ALLOW));
    for rule in rules {
        let Refusal::Errno(errno) = rule.otherwise else {
            continue;
        };
        if !positions
            .iter()
            .any(|(label, _)| *label == Label::Errno(errno))
        {
            positions.push((Label::Errno(errno), pending.len()));
            pending.push(Pending::plain(BPF_RET_K, SECCOMP_RET_ERRNO + errno));
        }
    }
    positions.push((Label::Kill, pending.len()));
    pending.push(Pending::plain(BPF_RET_K, SECCOMP_RET_KILL_PROCESS));
    let at = |label: Label| {
        positions
            .iter()
            .find(|(placed, _)| *placed == label)
            .map_or(0, |(_, position)| *position)
    };
    pending
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let offset = |label: Label| match label {
                Label::Next => 0,
                Label::Allow | Label::Kill | Label::Block(_) | Label::Errno(_) => {
                    jump_offset(index, at(label))
                }
            };
            let mut ins = step.ins;
            ins.jt = offset(step.jt);
            ins.jf = offset(step.jf);
            if let Some(label) = step.ja {
                ins.k = jump_distance(index, at(label));
            }
            ins
        })
        .collect()
}

/// Forward BPF jump offset from instruction `at` to `target`:
/// `target - (at + 1)`, always forward by construction. The program is
/// a few dozen instructions, so the offset fits in a `u8`; the layout
/// tests pin the exact program and would fail before any truncation
/// could occur.
const fn jump_offset(at: usize, target: usize) -> u8 {
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "forward jump within a tiny program; offset < 256 by construction, pinned by layout tests"
    )]
    let offset = (target - (at + 1)) as u8;
    offset
}

/// Forward `BPF_JA` distance from instruction `at` to `target`.
/// Unconditional jump within a tiny program: distance < 2^32 by
/// construction.
const fn jump_distance(at: usize, target: usize) -> u32 {
    #[allow(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "unconditional jump within a tiny program; distance < 2^32 by construction"
    )]
    let distance = (target - (at + 1)) as u32;
    distance
}

/// Installs a compiled profile on the current process.
///
/// Irreversible: once installed, the filter applies to the process and
/// all its future children. The caller must hold `NoNewPrivs` or
/// `CAP_SYS_ADMIN`, and typically installs immediately before `exec`.
///
/// # Errors
///
/// Returns [`Error::SeccompInstall`] if the `prctl` fails.
pub fn install(profile: &SeccompProfile) -> Result<(), Error> {
    let prog = profile.build();
    let len = u16::try_from(prog.len()).map_err(|_| Error::SeccompInstall {
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "seccomp program exceeds u16 length",
        ),
    })?;
    // Convert to libc's sock_filter: identical #[repr(C)] layout, copied
    // field by field so no transmute is needed.
    let raw: Vec<libc::sock_filter> = prog
        .iter()
        .map(|ins| libc::sock_filter {
            code: ins.code,
            jt: ins.jt,
            jf: ins.jf,
            k: ins.k,
        })
        .collect();
    // SAFETY: sock_fprog borrows the converted program for the duration
    // of the prctl call; the kernel copies it. All pointers are valid
    // and the layout matches struct sock_fprog.
    let prog = libc::sock_fprog {
        len,
        filter: raw.as_ptr().cast_mut(),
    };
    // prctl takes the program pointer as an unsigned long; there is no
    // safe conversion for a raw pointer into a syscall argument.
    #[allow(
        clippy::as_conversions,
        reason = "prctl(2) ABI takes the sock_fprog pointer as c_ulong; no safe conversion exists"
    )]
    let prog_arg = std::ptr::addr_of!(prog) as libc::c_ulong;
    // SAFETY: `prctl` with `PR_SET_SECCOMP`/`SECCOMP_MODE_FILTER` takes
    // the `sock_fprog` pointer as `c_ulong`; `prog` outlives the call
    // and the kernel copies the filter before returning.
    let ret = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER,
            prog_arg,
            0,
            0,
        )
    };
    if ret != 0 {
        return Err(Error::SeccompInstall {
            source: std::io::Error::last_os_error(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_syscall_names_are_rejected() {
        // The typo pin: "wriet" must fail here, not become a hole.
        let err = SeccompProfile::default().allow("wriet").unwrap_err();
        assert!(matches!(err, Error::UnknownSyscall { .. }));
        assert!(SeccompProfile::default().allow("").is_err());
    }

    #[test]
    fn allow_is_idempotent() {
        let mut profile = SeccompProfile::default();
        profile.allow("read").unwrap();
        profile.allow("read").unwrap();
        assert_eq!(
            profile.allowed_numbers(),
            vec![syscall_number("read").unwrap()]
        );
    }

    #[test]
    fn the_harness_profile_adds_file_calls_and_nothing_that_escapes() {
        let agent = SeccompProfile::agent().unwrap().allowed_numbers();
        let harness = SeccompProfile::harness().unwrap().allowed_numbers();
        for number in &agent {
            assert!(
                harness.contains(number),
                "the harness profile keeps the agent's call {number}"
            );
        }
        #[cfg(target_arch = "x86_64")]
        for name in [
            "pwrite64",
            "fsync",
            "fdatasync",
            "ftruncate",
            "getcwd",
            "chdir",
            "rename",
            "mkdir",
            "rmdir",
            "unlink",
            "readlink",
            "fchmod",
            "getdents64",
            "mkdirat",
            "unlinkat",
            "renameat",
            "readlinkat",
            "fchmodat",
            "utimensat",
            "renameat2",
        ] {
            assert!(
                harness.contains(&syscall_number(name).unwrap()),
                "the harness may {name}"
            );
            assert!(
                !agent.contains(&syscall_number(name).unwrap()),
                "the agent may not {name}"
            );
        }
        // Still no new processes, mounts, namespaces or tracing.
        for name in [
            "clone",
            "clone3",
            "fork",
            "vfork",
            "mount",
            "unshare",
            "setns",
            "ptrace",
            "pivot_root",
        ] {
            assert!(
                syscall_number(name).is_none_or(|nr| !harness.contains(&nr)),
                "{name} stays out"
            );
        }
        // And sockets stay AF_UNIX-only in the built filter, as the agent's.
        let built = SeccompProfile::harness().unwrap().build();
        assert!(built.len() > SeccompProfile::agent().unwrap().build().len());
    }

    #[test]
    fn agent_profile_builds() {
        // If a curated name ever leaves the table, this fails loudly
        // instead of shipping a filter missing a syscall the agent
        // needs (which would SIGSYS-kill the agent at startup).
        let profile = SeccompProfile::agent().unwrap();
        let numbers = profile.allowed_numbers();
        assert!(numbers.contains(&syscall_number("read").unwrap()));
        assert!(numbers.contains(&syscall_number("socket").unwrap()));
        assert!(numbers.contains(&syscall_number("execve").unwrap()));
        // glibc >= 2.43's ld.so probes /etc/ld.so.preload with access()
        // before main; dropping it from the profile SIGSYS-kills the
        // agent at startup on newer distros.
        #[cfg(target_arch = "x86_64")]
        assert!(numbers.contains(&syscall_number("access").unwrap()));
        // The startup set (x86_64, glibc >= 2.43): ld.so's
        // pread64/fstat/fstatat/statx, std's poll, and the runtime's
        // lseek, sched_getaffinity, and prlimit64 — all before main.
        // Dropping any of them SIGSYS-kills the agent at startup.
        // (`fstatat` was caught by the seccomp_startup test, not the
        // strace: cargo's LD_LIBRARY_PATH makes ld.so probe
        // glibc-hwcaps subdirs with newfstatat.)
        #[cfg(target_arch = "x86_64")]
        for name in [
            "fstatat",
            "fstat",
            "poll",
            "lseek",
            "pread64",
            "sched_getaffinity",
            "prlimit64",
            "statx",
        ] {
            assert!(
                numbers.contains(&syscall_number(name).unwrap()),
                "startup syscall {name} must be in the agent profile"
            );
        }
        // Handshake I/O (x86_64): std's UnixStream writes HELLO with
        // sendto and reads WELCOME with recvfrom; fcntl(fd, F_GETFD)
        // fires once on the broker socket during teardown. Dropping any
        // of them SIGSYS-kills the agent partway through the handshake.
        #[cfg(target_arch = "x86_64")]
        for name in ["sendto", "recvfrom", "fcntl"] {
            assert!(
                numbers.contains(&syscall_number(name).unwrap()),
                "handshake syscall {name} must be in the agent profile"
            );
        }
        // Sorted, for the deterministic program layout.
        let mut sorted = numbers.clone();
        sorted.sort_unstable();
        assert_eq!(numbers, sorted);
    }

    #[test]
    fn program_without_socket_has_no_af_check() {
        // Without socket in the allowlist there is no AF_UNIX check:
        // the program is arch check, nr load, jump-to-kill, allow,
        // kill. This pins the conditional layout — an unconditional
        // AF_UNIX check would let socket(AF_UNIX) through a profile
        // that denies socket entirely.
        let prog = SeccompProfile::default().build();
        assert_eq!(prog.len(), 6);
        // scan_end is an unconditional jump to kill.
        let scan_end = prog.get(3).expect("layout pins scan_end at 3");
        assert_eq!(scan_end.code, BPF_JMP_JA);
        let allow = prog.get(4).expect("layout pins allow at 4");
        assert_eq!(allow.k, SECCOMP_RET_ALLOW);
        let kill = prog.get(5).expect("layout pins kill at 5");
        assert_eq!(kill.k, SECCOMP_RET_KILL_PROCESS);
        assert!(
            !prog
                .iter()
                .any(|ins| ins.code == BPF_JMP_JEQ_K && ins.k == AF_UNIX),
            "no AF_UNIX check without socket allowed"
        );
    }

    #[test]
    fn program_with_socket_ends_with_af_check_then_allow_then_kill() {
        // Structural pin on the fail-closed layout with socket
        // allowed: the AF_UNIX check precedes the allow return, and
        // kill sits last so every rejection jumps forward to it.
        // Reordering these would invert the policy.
        let mut profile = SeccompProfile::default();
        profile.allow("socket").unwrap();
        let prog = profile.build();
        let n = prog.len();
        assert!(n >= 9, "program too short to hold the fixed layout");
        let kill = prog.get(n - 1).expect("kill is last by layout");
        assert_eq!(kill.k, SECCOMP_RET_KILL_PROCESS);
        assert_eq!(kill.code, BPF_RET_K);
        let allow = prog.get(n - 2).expect("allow precedes kill by layout");
        assert_eq!(allow.k, SECCOMP_RET_ALLOW);
        assert_eq!(allow.code, BPF_RET_K);
        // The AF_UNIX check sits before allow and jumps both ways.
        let check = prog
            .get(n - 3)
            .expect("AF_UNIX check precedes allow by layout");
        assert_eq!(check.code, BPF_JMP_JEQ_K);
        assert_eq!(check.k, AF_UNIX);
        // jt -> allow (offset 0), jf -> kill (offset 0: kill is next
        // after allow... no: kill is last, allow is n-2, so jf from
        // n-3 to n-1 is offset 1).
        assert_eq!(check.jt, 0, "jt lands on allow");
        assert_eq!(check.jf, 1, "jf lands on kill");
    }

    #[test]
    fn program_starts_with_arch_and_nr_loads() {
        let prog = SeccompProfile::default().build();
        assert_eq!(
            prog.first().expect("arch load is first by layout"),
            &SockFilter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: SECCOMP_DATA_ARCH,
            }
        );
        let arch_check = prog.get(1).expect("arch check is second by layout");
        assert_eq!(arch_check.k, AUDIT_ARCH);
        assert_eq!(arch_check.code, BPF_JMP_JEQ_K);
        // A wrong architecture must jump to the kill return.
        let kill_idx = prog
            .iter()
            .position(|ins| ins.code == BPF_RET_K && ins.k == SECCOMP_RET_KILL_PROCESS)
            .unwrap();
        assert_eq!(usize::from(arch_check.jf), kill_idx - 2);
        assert_eq!(
            prog.get(2).expect("nr load is third by layout"),
            &SockFilter {
                code: BPF_LD_W_ABS,
                jt: 0,
                jf: 0,
                k: SECCOMP_DATA_NR,
            }
        );
    }

    #[test]
    fn every_allowed_syscall_has_a_jeq_to_allow() {
        // Pins the scan completeness: dropping a JEQ would silently
        // deny (kill) a syscall the profile claims to allow.
        let profile = SeccompProfile::agent().unwrap();
        let prog = profile.build();
        let allow_idx = prog
            .iter()
            .position(|ins| ins.code == BPF_RET_K && ins.k == SECCOMP_RET_ALLOW)
            .unwrap();
        let socket_nr = syscall_number("socket").unwrap();
        for nr in profile.allowed_numbers() {
            if nr == socket_nr {
                continue; // routed through the AF_UNIX check instead
            }
            let found = prog.iter().enumerate().any(|(i, ins)| {
                ins.code == BPF_JMP_JEQ_K && ins.k == nr && i + 1 + usize::from(ins.jt) == allow_idx
            });
            assert!(found, "no JEQ to allow for syscall nr {nr}");
        }
    }

    #[test]
    fn socket_jumps_to_the_af_unix_check() {
        // The socket special-case pin: socket must NOT jump straight
        // to allow, or the AF_UNIX constraint is bypassed.
        let mut profile = SeccompProfile::default();
        profile.allow("socket").unwrap();
        let prog = profile.build();
        let socket_nr = syscall_number("socket").unwrap();
        let allow_idx = prog
            .iter()
            .position(|ins| ins.code == BPF_RET_K && ins.k == SECCOMP_RET_ALLOW)
            .unwrap();
        let (i, ins) = prog
            .iter()
            .enumerate()
            .find(|(_, ins)| ins.code == BPF_JMP_JEQ_K && ins.k == socket_nr)
            .expect("socket JEQ present");
        let target = i + 1 + usize::from(ins.jt);
        assert_ne!(target, allow_idx, "socket must not jump directly to allow");
        // The target is the AF_UNIX check: it loads args[0].
        let target_ins = prog
            .get(target)
            .expect("socket jump target is in-bounds by layout");
        assert_eq!(target_ins.k, SECCOMP_DATA_ARGS);
    }

    #[test]
    fn af_unix_check_jumps_to_allow_on_true_and_kill_on_false() {
        // The AF_UNIX check is two instructions: LD args[0] at
        // check_idx, then JEQ AF_UNIX at check_idx + 1, whose jt must
        // land on allow and jf on kill. Mutants
        // retargeting the patch (check_idx + 1 -> check_idx - 1, or
        // check_idx + 1 -> check_idx * 1 == check_idx) patch the
        // wrong instruction, leaving the JEQ with placeholder zeros
        // — socket(AF_UNIX) would fall through to kill, or worse,
        // socket(non-UNIX) would fall through toward allow.
        let mut profile = SeccompProfile::default();
        profile.allow("socket").unwrap();
        let prog = profile.build();
        let socket_nr = syscall_number("socket").unwrap();
        let allow_idx = prog
            .iter()
            .position(|ins| ins.code == BPF_RET_K && ins.k == SECCOMP_RET_ALLOW)
            .unwrap();
        let kill_idx = prog
            .iter()
            .position(|ins| ins.code == BPF_RET_K && ins.k == SECCOMP_RET_KILL_PROCESS)
            .unwrap();
        let (i, ins) = prog
            .iter()
            .enumerate()
            .find(|(_, ins)| ins.code == BPF_JMP_JEQ_K && ins.k == socket_nr)
            .expect("socket JEQ present");
        let check_idx = i + 1 + usize::from(ins.jt);
        // check_idx loads args[0]; check_idx + 1 is the JEQ AF_UNIX.
        assert_eq!(
            prog.get(check_idx).expect("check_idx in bounds").k,
            SECCOMP_DATA_ARGS
        );
        let jeq_idx = check_idx + 1;
        let jeq = prog.get(jeq_idx).expect("JEQ AF_UNIX in bounds");
        assert_eq!(jeq.code, BPF_JMP_JEQ_K);
        assert_eq!(jeq.k, AF_UNIX);
        assert_eq!(
            jeq.jt,
            jump_offset(jeq_idx, allow_idx),
            "JEQ AF_UNIX true-branch must land on allow"
        );
        assert_eq!(
            jeq.jf,
            jump_offset(jeq_idx, kill_idx),
            "JEQ AF_UNIX false-branch must land on kill"
        );
    }

    #[test]
    fn jump_offset_is_target_minus_next() {
        // Direct pin: offset = target - (at + 1). A mutant returning
        // 0, 1, or the wrong arithmetic would misroute every patched
        // jump, so this is tested directly, not just via the layout.
        assert_eq!(jump_offset(0, 1), 0);
        assert_eq!(jump_offset(0, 5), 4);
        assert_eq!(jump_offset(3, 10), 6);
        assert_eq!(jump_distance(0, 1), 0);
        assert_eq!(jump_distance(0, 5), 4);
        assert_eq!(jump_distance(3, 10), 6);
    }

    #[test]
    fn install_actually_installs_the_filter() {
        // Fork-based: install() is irreversible, so it runs in a
        // child. The child sets NoNewPrivs, installs a profile that
        // denies getpid but ALLOWS exit_group, then checks install's
        // return value before probing the filter.
        //
        // Why exit_group must be allowed: install() calls prctl first
        // and checks its return after. A mutant inverting the check
        // (!= to ==) still installs the filter, then returns Err.
        // If exit_group were denied, the child's attempt to report
        // that Err via exit_child would itself die with SIGSYS —
        // the exact outcome the parent asserts — and the mutant
        // would survive. Allowing exit_group lets the child report
        // the return value distinctly from the filter's side effect:
        //   install Ok + filter installed  -> getpid dies SIGSYS (159)
        //   install Ok + filter NOT installed (mutant: Ok(()))
        //                                -> getpid succeeds, exit 42
        //   install Err (mutant: != to ==) -> exit 20
        //   install Err (environment)      -> exit 20, test fails honestly
        //     (the environment must support seccomp for this test)
        use crate::process::{ForkResult, child_exit_code, exit_child, fork, set_no_new_privs};

        let child = match fork().expect("fork for install test") {
            ForkResult::Parent { child } => child,
            ForkResult::Child => {
                // Child: minimal, async-signal-safe until we know the
                // filter state. set_no_new_privs first (required for
                // unprivileged seccomp install).
                if set_no_new_privs().is_err() {
                    exit_child(99);
                }
                let mut profile = SeccompProfile::default();
                // exit_group must stay usable so the child can report
                // install's return value; getpid stays denied so the
                // filter's presence is still observable.
                profile.allow("exit_group").expect("exit_group known");
                match install(&profile) {
                    Ok(()) => {
                        // SAFETY: getpid takes no args; if the filter
                        // is installed this never returns.
                        unsafe { libc::getpid() };
                        // If we reach here, the filter was NOT
                        // installed (mutant: replace with Ok(())).
                        exit_child(42);
                    }
                    Err(_) => {
                        // install reported failure. With the !=/==
                        // mutant this happens after a successful
                        // prctl (filter IS installed); in a broken
                        // environment prctl itself failed. Either
                        // way the return value is wrong for a
                        // working install, so fail distinctly from
                        // the SIGSYS path.
                        exit_child(20);
                    }
                }
            }
        };

        let status = child
            .wait_deadline(std::time::Duration::from_secs(30))
            .expect("wait for install test");
        let code = child_exit_code(status);
        // 128 + 31 = 159: killed by SIGSYS (31). The filter was
        // installed, install returned Ok, and getpid was denied.
        assert_eq!(
            code, 159,
            "child should die with SIGSYS (159), got {code}: filter not installed or install misreported"
        );
    }

    #[test]
    fn scan_end_jump_lands_on_kill() {
        // The scan_end JA instruction must jump exactly to kill: its
        // k field is jump_distance(scan_end_idx, kill_idx). If
        // jump_distance were mutated (0 or 1), this k would be wrong
        // and the program would misroute rejections.
        let prog = SeccompProfile::default().build();
        let scan_end_idx = 3; // pinned by layout: arch(0), arch_check(1), nr(2), scan_end(3)
        let kill_idx = prog.len() - 1;
        let scan_end = prog.get(scan_end_idx).expect("scan_end at 3 by layout");
        assert_eq!(scan_end.code, BPF_JMP_JA);
        assert_eq!(
            scan_end.k,
            jump_distance(scan_end_idx, kill_idx),
            "JA distance must equal jump_distance(scan_end, kill)"
        );
        // And the distance must be the actual forward distance: kill
        // is at 5, scan_end at 3, so distance = 5 - (3 + 1) = 1.
        assert_eq!(scan_end.k, 1);
    }

    /// Runs `prog` as the kernel would for one call: syscall `nr` with
    /// `args`, on `arch`. Returns the action.
    fn run(prog: &[SockFilter], arch: u32, nr: u32, args: [u64; 6]) -> u32 {
        let mut a = 0u32;
        let mut pc = 0usize;
        loop {
            let ins = *prog.get(pc).expect("the program never runs off its end");
            match ins.code {
                BPF_LD_W_ABS => {
                    a = match ins.k {
                        SECCOMP_DATA_NR => nr,
                        SECCOMP_DATA_ARCH => arch,
                        k => {
                            let word = usize::try_from(k - SECCOMP_DATA_ARGS).unwrap();
                            let arg = *args.get(word / 8).unwrap();
                            let half = if word % 8 == 0 { arg } else { arg >> 32 };
                            u32::try_from(half & 0xFFFF_FFFF).unwrap()
                        }
                    };
                    pc += 1;
                }
                BPF_JMP_JEQ_K | BPF_JMP_JSET_K => {
                    let taken = if ins.code == BPF_JMP_JEQ_K {
                        a == ins.k
                    } else {
                        a & ins.k != 0
                    };
                    pc += 1 + usize::from(if taken { ins.jt } else { ins.jf });
                }
                BPF_JMP_JA => pc += 1 + usize::try_from(ins.k).unwrap(),
                BPF_RET_K => return ins.k,
                // No action: any assertion on an unknown opcode fails.
                _ => return u32::MAX,
            }
        }
    }

    fn call(prog: &[SockFilter], name: &str, args: [u64; 6]) -> u32 {
        run(prog, AUDIT_ARCH, syscall_number(name).unwrap(), args)
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_cli_cell_pairs_unix_sockets_only() {
        let prog = SeccompProfile::harness_cli().unwrap().build();
        let errno = |e: u32| SECCOMP_RET_ERRNO + e;
        // A socket pair: AF_UNIX only, any other family a soft refusal.
        for (family, action) in [
            (AF_UNIX, SECCOMP_RET_ALLOW),
            (AF_INET, errno(EAFNOSUPPORT)),
            (10, errno(EAFNOSUPPORT)),
            (16, errno(EAFNOSUPPORT)),
        ] {
            let args = [u64::from(family), 1, 0, 0, 0, 0];
            assert_eq!(
                call(&prog, "socketpair", args),
                action,
                "socketpair family {family}"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_harness_cli_profile_decides_each_call_as_designed() {
        let prog = SeccompProfile::harness_cli().unwrap().build();
        let no = [0u64; 6];
        let errno = |e: u32| SECCOMP_RET_ERRNO + e;
        // Plain allowed calls, the harness profile's and the new ones.
        for name in [
            "read",
            "renameat2",
            "wait4",
            "pipe2",
            "epoll_pwait2",
            "vfork",
            "kill",
        ] {
            assert_eq!(call(&prog, name, no), SECCOMP_RET_ALLOW, "{name}");
        }
        // Another architecture, or a call never allowed: the kill.
        assert_eq!(run(&prog, AUDIT_ARCH + 1, 0, no), SECCOMP_RET_KILL_PROCESS);
        for nr in [101, 165, 272, 308, 321, 298, 310, 425, 323, 155, 161, 317] {
            // ptrace, mount, unshare, setns, bpf, perf_event_open,
            // process_vm_readv, io_uring_setup, userfaultfd, pivot_root,
            // chroot, seccomp.
            assert_eq!(
                run(&prog, AUDIT_ARCH, nr, no),
                SECCOMP_RET_KILL_PROCESS,
                "{nr}"
            );
        }
        // Sockets: AF_UNIX and AF_INET, any other family a soft refusal.
        for (family, action) in [
            (AF_UNIX, SECCOMP_RET_ALLOW),
            (AF_INET, SECCOMP_RET_ALLOW),
            (10, errno(EAFNOSUPPORT)),
            (16, errno(EAFNOSUPPORT)),
            (17, errno(EAFNOSUPPORT)),
        ] {
            let args = [u64::from(family), 1, 0, 0, 0, 0];
            assert_eq!(call(&prog, "socket", args), action, "family {family}");
        }
        // clone: threads and forks, never a namespace flag; clone3 answers
        // ENOSYS whatever it asks for.
        let thread = 0x003D_0F00u64;
        assert_eq!(
            call(&prog, "clone", [thread, 0, 0, 0, 0, 0]),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            call(&prog, "clone", [0x0120_0011, 0, 0, 0, 0, 0]),
            SECCOMP_RET_ALLOW
        );
        for flag in [
            0x0002_0000u64,
            0x0200_0000,
            0x0400_0000,
            0x0800_0000,
            0x1000_0000,
            0x2000_0000,
            0x4000_0000,
        ] {
            let args = [thread + flag, 0, 0, 0, 0, 0];
            assert_eq!(
                call(&prog, "clone", args),
                SECCOMP_RET_KILL_PROCESS,
                "{flag:#x}"
            );
        }
        assert_eq!(call(&prog, "clone3", no), errno(ENOSYS));
        assert_eq!(call(&prog, "clone3", [0, 88, 0, 0, 0, 0]), errno(ENOSYS));
        // prctl and ioctl: only what the CLIs use; the request is the
        // second argument, and only its low 32 bits count, as for the kernel.
        for option in PRCTL_OPTIONS {
            assert_eq!(
                call(&prog, "prctl", [u64::from(option), 0, 0, 0, 0, 0]),
                SECCOMP_RET_ALLOW
            );
        }
        for option in [22u64, 38, 4, 35] {
            // PR_SET_SECCOMP, PR_SET_NO_NEW_PRIVS, PR_SET_DUMPABLE, PR_SET_MM.
            assert_eq!(
                call(&prog, "prctl", [option, 0, 0, 0, 0, 0]),
                errno(EPERM),
                "{option}"
            );
        }
        for request in IOCTL_REQUESTS {
            assert_eq!(
                call(&prog, "ioctl", [1, u64::from(request), 0, 0, 0, 0]),
                SECCOMP_RET_ALLOW
            );
            assert_eq!(
                call(&prog, "ioctl", [u64::from(request), 0, 0, 0, 0, 0]),
                errno(ENOTTY),
                "the request is the second argument"
            );
        }
        assert_eq!(
            call(&prog, "ioctl", [1, 0x5412, 0, 0, 0, 0]),
            errno(ENOTTY),
            "TIOCSTI"
        );
        assert_eq!(
            call(&prog, "ioctl", [1, 0x1_0000_5413, 0, 0, 0, 0]),
            SECCOMP_RET_ALLOW,
            "the high half is not the request"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_effective_rules_are_the_profiles_own_or_the_standing_af_unix_one() {
        assert_eq!(
            SeccompProfile::agent().unwrap().effective_rules(),
            [Rule {
                nr: 41,
                check: Check::OneOf {
                    arg: 0,
                    values: vec![AF_UNIX],
                },
                otherwise: Refusal::Kill,
            }]
        );
        let cli = SeccompProfile::harness_cli().unwrap();
        assert_eq!(
            cli.effective_rules(),
            cli.rules,
            "its own socket rule, no standing one"
        );
        let socketless = SeccompProfile {
            allowed: vec![0, 1],
            rules: Vec::new(),
        };
        assert!(socketless.effective_rules().is_empty());
        // Rules for other calls leave a plainly allowed socket its standing
        // AF_UNIX rule, first.
        let clone = Rule {
            nr: 56,
            check: Check::NoneOf {
                mask: CLONE_NAMESPACES,
            },
            otherwise: Refusal::Kill,
        };
        let other_rules = SeccompProfile {
            allowed: vec![41, 56],
            rules: vec![clone.clone()],
        };
        assert_eq!(
            other_rules.effective_rules(),
            [
                Rule {
                    nr: 41,
                    check: Check::OneOf {
                        arg: 0,
                        values: vec![AF_UNIX],
                    },
                    otherwise: Refusal::Kill,
                },
                clone
            ]
        );
    }

    #[test]
    fn a_pending_instruction_is_plain_or_a_branch_until_laid_out() {
        let plain = Pending::plain(BPF_RET_K, 7);
        assert_eq!(
            plain.ins,
            SockFilter {
                code: BPF_RET_K,
                jt: 0,
                jf: 0,
                k: 7
            }
        );
        assert_eq!(
            (plain.jt, plain.jf, plain.ja),
            (Label::Next, Label::Next, None)
        );
        let branch = Pending::branch(BPF_JMP_JEQ_K, 41, Label::Allow, Label::Block(2));
        assert_eq!(
            branch.ins,
            SockFilter {
                code: BPF_JMP_JEQ_K,
                jt: 0,
                jf: 0,
                k: 41
            }
        );
        assert_eq!(
            (branch.jt, branch.jf, branch.ja),
            (Label::Allow, Label::Block(2), None)
        );
    }

    #[test]
    fn only_the_cli_profile_rules_any_call() {
        assert!(SeccompProfile::agent().unwrap().ruled_numbers().is_empty());
        assert!(
            SeccompProfile::harness()
                .unwrap()
                .ruled_numbers()
                .is_empty()
        );
        #[cfg(target_arch = "x86_64")]
        assert_eq!(
            SeccompProfile::harness_cli().unwrap().ruled_numbers(),
            // ioctl, socket, socketpair, clone, prctl, clone3.
            vec![16, 41, 53, 56, 157, 435]
        );
    }

    #[test]
    fn the_older_profiles_decide_as_before() {
        let no = [0u64; 6];
        for profile in [
            SeccompProfile::agent().unwrap(),
            SeccompProfile::harness().unwrap(),
        ] {
            let prog = profile.build();
            assert_eq!(call(&prog, "read", no), SECCOMP_RET_ALLOW);
            assert_eq!(
                call(&prog, "socket", [u64::from(AF_UNIX), 1, 0, 0, 0, 0]),
                SECCOMP_RET_ALLOW
            );
            assert_eq!(
                call(&prog, "socket", [u64::from(AF_INET), 1, 0, 0, 0, 0]),
                SECCOMP_RET_KILL_PROCESS,
                "the agent's sockets stay AF_UNIX, killed otherwise"
            );
            let returns: Vec<u32> = prog
                .iter()
                .filter(|ins| ins.code == BPF_RET_K)
                .map(|ins| ins.k)
                .collect();
            assert_eq!(
                returns,
                [SECCOMP_RET_ALLOW, SECCOMP_RET_KILL_PROCESS],
                "allow and kill only: no errno answers"
            );
            #[cfg(target_arch = "x86_64")]
            assert_eq!(
                run(&prog, AUDIT_ARCH, 56, no),
                SECCOMP_RET_KILL_PROCESS,
                "no clone"
            );
        }
    }

    #[test]
    fn a_rule_replaces_the_standing_socket_rule_and_refusals_share_returns() {
        let mut profile = SeccompProfile::default();
        profile.allow("socket").unwrap();
        profile
            .allow_if(
                "socket",
                Check::OneOf {
                    arg: 0,
                    values: vec![AF_INET],
                },
                Refusal::Errno(EPERM),
            )
            .unwrap();
        profile
            .allow_if(
                "getrandom",
                Check::NoneOf { mask: 2 },
                Refusal::Errno(EPERM),
            )
            .unwrap();
        profile.refuse("futex", ENOSYS).unwrap();
        profile.refuse("futex", ENOSYS).unwrap();
        let prog = profile.build();
        assert_eq!(
            call(&prog, "socket", [u64::from(AF_INET), 0, 0, 0, 0, 0]),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            call(&prog, "socket", [u64::from(AF_UNIX), 0, 0, 0, 0, 0]),
            SECCOMP_RET_ERRNO + EPERM,
            "its own rule, not the standing AF_UNIX one"
        );
        assert_eq!(
            call(&prog, "getrandom", [1, 0, 0, 0, 0, 0]),
            SECCOMP_RET_ALLOW
        );
        assert_eq!(
            call(&prog, "getrandom", [3, 0, 0, 0, 0, 0]),
            SECCOMP_RET_ERRNO + EPERM
        );
        assert_eq!(call(&prog, "futex", [0; 6]), SECCOMP_RET_ERRNO + ENOSYS);
        assert_eq!(
            profile.allowed_numbers().len(),
            2,
            "a refused call is not allowed"
        );
        let errno_returns = prog
            .iter()
            .filter(|ins| ins.code == BPF_RET_K && ins.k & 0xFFFF_0000 == SECCOMP_RET_ERRNO)
            .count();
        assert_eq!(errno_returns, 2, "one return per errno");
        assert_eq!(prog.last().map(|ins| ins.k), Some(SECCOMP_RET_KILL_PROCESS));
        assert!(matches!(
            profile.allow_if("wriet", Check::Never, Refusal::Kill),
            Err(Error::UnknownSyscall { .. })
        ));
        assert!(matches!(
            profile.refuse("wriet", 1),
            Err(Error::UnknownSyscall { .. })
        ));
    }
}
