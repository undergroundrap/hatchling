//! Run the Room image's own programs under the Room's syscall profile.
//!
//! Each command is everyday work, started the way the Room's launcher
//! starts its supervisor: into the image, root given up, the profile
//! installed, then the program.
//!
//! This test target has no libtest harness. Its main stays the only
//! thread, so a forked child may allocate while it prepares its exec.
//!
//! It needs root, and a directory laid out as a Room's root is: the image,
//! with a workspace and a session directory the Room's user may write, and
//! a `/proc` of processes only.
//! `scripts/room-image-tools-proof.sh` makes one from a built image and
//! names it in `HATCHLING_ROOM_IMAGE_ROOT`, with the user's id in
//! `HATCHLING_ROOM_IMAGE_USER`. With no root named this does nothing,
//! unless `HATCHLING_REQUIRE_ROOM_IMAGE` says that is a failure.
#![allow(
    clippy::unwrap_used,
    reason = "a failed fixture or proof must fail its test loudly"
)]

fn main() {
    #[cfg(target_arch = "x86_64")]
    proof::run();
}

#[cfg(target_arch = "x86_64")]
mod proof {
    use std::fs::File;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use hatchling_core::room::{JOB_PATH, SESSION_DIR, WORKSPACE};
    use hatchling_sys::{ForkResult, SeccompProfile};

    /// How a command must end.
    #[derive(Clone, Copy)]
    enum Ends {
        /// With exit status 0.
        Well,
        /// With this exit status, saying this. The Room answers a call it
        /// doesn't grant, and the tool says what it was refused.
        Refused(i32, &'static str),
    }

    /// A command: its name here, what `sh` is given, and how it must end.
    type Command = (&'static str, &'static str, Ends);

    /// Git's commands are given who commits: the Room's user has no name.
    macro_rules! git {
        ($text:literal) => {
            concat!(
                "export GIT_AUTHOR_NAME=a GIT_AUTHOR_EMAIL=a@invalid ",
                "GIT_COMMITTER_NAME=a GIT_COMMITTER_EMAIL=a@invalid; ",
                $text
            )
        };
    }

    /// Everyday work, in an order each command can follow the last in.
    /// Each runs in `/workspace/t`, which the first one fills.
    const COMMANDS: &[Command] = &[
        (
            "fill",
            "mkdir -p /session/home && seq 1 2000 > numbers.txt && printf 'b\\na\\nc\\na\\n' > letters.txt \
             && mkdir -p dir/deep && cp numbers.txt dir/n.txt && cp letters.txt dir/deep/l.txt && ln -s n.txt dir/link",
            Ends::Well,
        ),
        // The file tools, plain.
        ("cat", "cat numbers.txt", Ends::Well),
        ("cp", "cp numbers.txt copy.txt", Ends::Well),
        ("mv", "mv copy.txt moved.txt", Ends::Well),
        ("rm", "rm moved.txt", Ends::Well),
        ("ls", "ls -la .", Ends::Well),
        ("head", "head -n 3 numbers.txt", Ends::Well),
        ("tail", "tail -n 3 numbers.txt", Ends::Well),
        ("wc", "wc -l numbers.txt", Ends::Well),
        ("sort", "sort letters.txt", Ends::Well),
        ("uniq", "uniq letters.txt", Ends::Well),
        ("cut", "cut -c1 letters.txt", Ends::Well),
        ("tr", "echo abc | tr a-z A-Z", Ends::Well),
        ("mkdir", "mkdir -p made/deeper", Ends::Well),
        ("touch", "touch made/file", Ends::Well),
        ("stat", "stat numbers.txt", Ends::Well),
        ("du", "du -s .", Ends::Well),
        ("df", "df .", Ends::Well),
        ("ln -s", "ln -s numbers.txt link", Ends::Well),
        ("chmod", "chmod 600 made/file", Ends::Well),
        ("sha256sum", "sha256sum numbers.txt", Ends::Well),
        ("md5sum", "md5sum numbers.txt", Ends::Well),
        ("tee", "echo teed | tee teed.txt", Ends::Well),
        ("seq", "seq 1 5", Ends::Well),
        ("date", "date", Ends::Well),
        ("env", "env", Ends::Well),
        ("id", "id", Ends::Well),
        (
            "dd",
            "dd if=numbers.txt of=dd.txt bs=512 count=2",
            Ends::Well,
        ),
        ("od", "od -c letters.txt", Ends::Well),
        ("mktemp", "mktemp -p . && mktemp", Ends::Well),
        ("realpath", "realpath link", Ends::Well),
        (
            "install",
            "install -m 600 numbers.txt installed.txt",
            Ends::Well,
        ),
        ("truncate", "truncate -s 10 installed.txt", Ends::Well),
        ("split", "split -l 500 numbers.txt part-", Ends::Well),
        ("shuf", "shuf letters.txt", Ends::Well),
        ("grep", "grep -c 1 numbers.txt", Ends::Well),
        ("sed", "sed -n 2p numbers.txt", Ends::Well),
        ("awk", "awk 'END { print NR }' numbers.txt", Ends::Well),
        ("find", "find . -name '*.txt'", Ends::Well),
        ("xargs", "echo a b | xargs -n1 echo", Ends::Well),
        ("diff", "diff numbers.txt dir/n.txt", Ends::Well),
        ("tar -c", "tar -cf made.tar dir", Ends::Well),
        ("tar -t", "tar -tf made.tar", Ends::Well),
        ("tar -x", "tar -xf made.tar -C made", Ends::Well),
        ("gzip", "gzip -kf letters.txt", Ends::Well),
        ("gunzip", "gunzip -c letters.txt.gz", Ends::Well),
        // Their other options.
        ("cp -a", "cp -a dir dir-a", Ends::Well),
        ("cp -r", "cp -r dir dir-r", Ends::Well),
        ("cp -p", "cp -p numbers.txt p.txt", Ends::Well),
        ("mv, to the session", "mv p.txt /session/p.txt", Ends::Well),
        (
            "mv, a directory to the session",
            "mv dir-r /session/dir-r",
            Ends::Well,
        ),
        ("mv, back", "mv /session/p.txt back.txt", Ends::Well),
        ("ls -lR", "ls -lR .", Ends::Well),
        // Where a file system can't list a file's attributes, as the
        // image's own directories can't, `ls -l` asks for the file's
        // security label by name. With `-a`, `..` of the workspace is
        // the image's root.
        ("ls -la, the workspace", "ls -la /workspace", Ends::Well),
        ("ls -la, the root", "ls -la /", Ends::Well),
        ("ls -la, /tmp", "ls -la /tmp", Ends::Well),
        ("ls -la, the session", "ls -la /session", Ends::Well),
        ("ls -ld", "ls -ld / /tmp /session /workspace", Ends::Well),
        (
            "ls -l, the image's files",
            "ls -l /etc/os-release /usr/bin/ls",
            Ends::Well,
        ),
        ("ls -lL, the root", "ls -lL /", Ends::Well),
        (
            "cp -a, from the image",
            "cp -a /etc/os-release from-image.txt",
            Ends::Well,
        ),
        ("stat, the root", "stat / /tmp", Ends::Well),
        ("readlink", "readlink dir/link", Ends::Well),
        ("touch -d", "touch -d 2026-01-01 back.txt", Ends::Well),
        ("chmod -R", "chmod -R u+rwX dir-a", Ends::Well),
        ("chown", "chown \"$(id -u):$(id -g)\" back.txt", Ends::Well),
        ("stat -f", "stat -f .", Ends::Well),
        ("df -h", "df -h", Ends::Well),
        ("du -sh", "du -sh .", Ends::Well),
        ("head -c", "head -c 100 numbers.txt", Ends::Well),
        ("tail -n +", "tail -n +1990 numbers.txt", Ends::Well),
        // `tail -f` goes on until it is stopped: it must be `timeout`
        // that ends it.
        (
            "tail -f",
            "timeout 1 tail -f numbers.txt; [ $? = 124 ]",
            Ends::Well,
        ),
        ("sort -u", "sort -u -o sorted.txt letters.txt", Ends::Well),
        (
            "sort, with files of its own",
            "seq 1 300000 | sort -S 1M -r | tail -1",
            Ends::Well,
        ),
        ("tee -a", "echo more | tee -a teed.txt", Ends::Well),
        ("sleep", "sleep 0.1", Ends::Well),
        ("timeout", "timeout 2 sleep 0.1", Ends::Well),
        ("nproc", "nproc", Ends::Well),
        ("uname", "uname -a", Ends::Well),
        ("pwd", "pwd", Ends::Well),
        ("printf", "printf '%s\\n' a", Ends::Well),
        ("expr", "expr 1 + 1", Ends::Well),
        ("nice", "nice true", Ends::Well),
        ("nohup", "nohup true", Ends::Well),
        ("base64", "base64 letters.txt", Ends::Well),
        ("tac", "tac letters.txt", Ends::Well),
        ("nl", "nl letters.txt", Ends::Well),
        ("paste", "paste letters.txt letters.txt", Ends::Well),
        ("comm", "comm letters.txt letters.txt", Ends::Well),
        ("sync", "sync", Ends::Well),
        ("rm -rf", "rm -rf dir-a", Ends::Well),
        ("which", "command -v ls && which ls", Ends::Well),
        ("grep -r", "grep -r 1999 .", Ends::Well),
        ("sed -i", "sed -i s/a/A/ letters.txt", Ends::Well),
        (
            "find -exec",
            "find . -type f -name '*.txt' -exec true {} +",
            Ends::Well,
        ),
        ("find -newer", "find . -newer letters.txt", Ends::Well),
        (
            "find -delete",
            "find /session/dir-r -depth -delete",
            Ends::Well,
        ),
        ("diff -r", "diff -r dir dir", Ends::Well),
        ("cmp", "cmp numbers.txt numbers.txt", Ends::Well),
        ("tar -cz", "tar -czf z.tar.gz dir", Ends::Well),
        (
            "tar -xz, to the session",
            "mkdir -p /session/x1 && tar -xzf z.tar.gz -C /session/x1",
            Ends::Well,
        ),
        (
            "tar -xp",
            "mkdir -p x2 && tar -xpf z.tar.gz -C x2",
            Ends::Well,
        ),
        ("xargs -P", "echo a b c | xargs -P 2 -n 1 true", Ends::Well),
        ("perl", "perl perl.pl", Ends::Well),
        // What the Room doesn't grant, said plainly by the tool.
        (
            "ln, a hard link",
            "ln numbers.txt hard.txt",
            Ends::Refused(1, "Operation not permitted"),
        ),
        // `ln` asks for a link by the newer of the two calls, and Perl by
        // the older, as Git does.
        (
            "perl, a hard link",
            "perl -e 'link(q{numbers.txt}, q{hard2.txt}) or die qq{$!\\n}'",
            Ends::Refused(1, "Operation not permitted"),
        ),
        (
            "mkfifo",
            "mkfifo pipe",
            Ends::Refused(1, "Operation not permitted"),
        ),
        // The image's other programs.
        ("rg", "rg -c 1999 .", Ends::Well),
        ("rg --files", "rg --files | head -3", Ends::Well),
        (
            "jq",
            "echo '{\"a\": [1, 2]}' | jq '.a | length'",
            Ends::Well,
        ),
        (
            "sqlite3",
            "sqlite3 cli.db 'create table t (a); insert into t values (1); select count(*) from t;'",
            Ends::Well,
        ),
        ("zip", "zip -q -r z.zip dir", Ends::Well),
        ("make", "make", Ends::Well),
        ("make -j", "rm -f out.txt && make -j 2", Ends::Well),
        ("bash, a script", "bash script.sh", Ends::Well),
        (
            "sh, a script",
            "sh -c 'set -e; x=$(echo a | tr a b); (sleep 0.1) & wait; trap \"echo bye\" EXIT; \
             cd dir && cd ..; test -f numbers.txt; umask 022; kill -0 $$'",
            Ends::Well,
        ),
        (
            "python3, files and processes",
            "python3 files.py",
            Ends::Well,
        ),
        ("python3, numpy and pandas", "python3 data.py", Ends::Well),
        ("python3, documents", "python3 documents.py", Ends::Well),
        (
            "python3 -m venv",
            "python3 -m venv --without-pip venv",
            Ends::Well,
        ),
        (
            "python3 -m json.tool",
            "echo '{\"a\": 1}' | python3 -m json.tool",
            Ends::Well,
        ),
        ("node --version", "node --version", Ends::Well),
        ("node, files and processes", "node files.js", Ends::Well),
        ("node, watching", "node watch.js", Ends::Well),
        ("git init", git!("git init -q repo"), Ends::Well),
        (
            "git add",
            git!("cp numbers.txt repo/ && git -C repo add ."),
            Ends::Well,
        ),
        (
            "git commit",
            git!("git -C repo commit -q -m first"),
            Ends::Well,
        ),
        ("git status", git!("git -C repo status"), Ends::Well),
        (
            "git diff",
            git!("seq 1 10 >> repo/numbers.txt && git -C repo --no-pager diff"),
            Ends::Well,
        ),
        (
            "git log",
            git!("git -C repo --no-pager log --stat"),
            Ends::Well,
        ),
        (
            "git checkout -b",
            git!("git -C repo checkout -q -b other"),
            Ends::Well,
        ),
        ("git stash", git!("git -C repo stash"), Ends::Well),
        ("git clone", git!("git clone -q repo repo2"), Ends::Well),
        ("git gc", git!("git -C repo gc -q"), Ends::Well),
        ("git grep", git!("git -C repo grep -c 1"), Ends::Well),
        (
            "git config",
            git!("git config --global user.name a"),
            Ends::Well,
        ),
    ];

    /// What the commands read that they don't make themselves.
    const FILES: &[(&str, &str)] = &[
        ("Makefile", include_str!("room_image_tools/Makefile")),
        ("script.sh", include_str!("room_image_tools/script.sh")),
        ("perl.pl", include_str!("room_image_tools/perl.pl")),
        ("files.py", include_str!("room_image_tools/files.py")),
        ("data.py", include_str!("room_image_tools/data.py")),
        (
            "documents.py",
            include_str!("room_image_tools/documents.py"),
        ),
        ("files.js", include_str!("room_image_tools/files.js")),
        ("watch.js", include_str!("room_image_tools/watch.js")),
    ];

    /// How long one command may take.
    const LIMIT: Duration = Duration::from_secs(120);

    /// How a command ended: its exit status, or the signal that ended it.
    #[derive(Debug, PartialEq, Eq)]
    enum Ended {
        Exit(i32),
        Signal(i32),
        NotInTime,
    }

    #[allow(
        clippy::redundant_pub_crate,
        reason = "only the parent test entry point calls this proof"
    )]
    pub(super) fn run() {
        let mut out = std::io::stdout();
        let Some(root) = std::env::var_os("HATCHLING_ROOM_IMAGE_ROOT") else {
            assert!(
                std::env::var_os("HATCHLING_REQUIRE_ROOM_IMAGE").is_none(),
                "no Room image root was named"
            );
            let _ = writeln!(
                out,
                "room image tools: skipped, no Room image root is named"
            );
            return;
        };
        let root = PathBuf::from(root);
        let user: u32 = std::env::var("HATCHLING_ROOM_IMAGE_USER")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            hatchling_sys::euid(),
            0,
            "root is needed, to give it up as the launcher does"
        );

        // The root's `/proc` is a Room's: processes, and nothing else of
        // the machine (its memory, its processors, the kernel's settings,
        // which a whole one has together). A program that reads the
        // machine there must find here what it finds in a Room.
        let proc = root.join("proc");
        assert!(
            proc.join("self").exists(),
            "the root named has no /proc mounted"
        );
        assert!(
            !proc.join("sys").exists(),
            "the root's /proc has the kernel's settings (`sys`), so it is a whole one: \
             a Room's is mounted with subset=pid"
        );

        // The directory the commands work in, and what they read, the
        // Room's user's own.
        let work = root.join(WORKSPACE.trim_start_matches('/')).join("t");
        std::fs::create_dir(&work).unwrap();
        for (name, text) in FILES {
            std::fs::write(work.join(name), text).unwrap();
            std::os::unix::fs::chown(work.join(name), Some(user), Some(user)).unwrap();
        }
        std::os::unix::fs::chown(&work, Some(user), Some(user)).unwrap();

        let said =
            std::env::temp_dir().join(format!("hatchling-room-image-tools-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&said);
        std::fs::create_dir(&said).unwrap();
        let mut failed = 0_usize;
        for (index, (name, text, ends)) in COMMANDS.iter().enumerate() {
            let kept = said.join(format!("{index}.txt"));
            let ended = command(&root, user, text, &kept);
            let output = std::fs::read_to_string(&kept).unwrap_or_default();
            let as_it_must = match *ends {
                Ends::Well => ended == Ended::Exit(0),
                Ends::Refused(status, saying) => {
                    ended == Ended::Exit(status) && output.contains(saying)
                }
            };
            if !as_it_must {
                failed += 1;
                let _ = writeln!(out, "{name}: {}", describe(&ended));
                for line in output
                    .lines()
                    .rev()
                    .take(6)
                    .collect::<Vec<_>>()
                    .iter()
                    .rev()
                {
                    let _ = writeln!(out, "    {line}");
                }
            }
        }
        let _ = std::fs::remove_dir_all(&said);
        assert_eq!(
            failed,
            0,
            "{failed} of {} commands didn't end as they must under the Room's profile",
            COMMANDS.len()
        );
        let _ = writeln!(
            out,
            "{} commands of the Room image's programs ended as they must under the Room's profile",
            COMMANDS.len()
        );
    }

    /// What became of a command, for a reader.
    fn describe(ended: &Ended) -> String {
        match *ended {
            // A shell says 128 and the signal's number for a command a
            // signal ended; `SIGSYS` is how the profile kills.
            Ended::Signal(libc::SIGSYS) | Ended::Exit(159) => {
                String::from("killed by the profile (SIGSYS)")
            }
            Ended::Signal(signal) => format!("ended by signal {signal}"),
            Ended::Exit(status) => format!("exit status {status}"),
            Ended::NotInTime => format!("didn't end in {} s", LIMIT.as_secs()),
        }
    }

    /// Runs `text` with `sh` in the Room at `root` as `user`, under the
    /// Room's profile, what it writes kept in `kept`.
    fn command(root: &Path, user: u32, text: &str, kept: &Path) -> Ended {
        let kept = File::create(kept).unwrap();
        let profile = SeccompProfile::room().unwrap();
        let script = format!("cd {WORKSPACE}/t && {text}");
        let home = format!("{SESSION_DIR}/home");
        match hatchling_sys::fork().unwrap() {
            ForkResult::Parent { child } => {
                drop(kept);
                match child.wait_deadline(LIMIT) {
                    Ok(status) if libc::WIFEXITED(status) => Ended::Exit(libc::WEXITSTATUS(status)),
                    Ok(status) => Ended::Signal(libc::WTERMSIG(status)),
                    Err(_) => {
                        let _ = child.terminate_group();
                        let _ = child.wait_deadline(Duration::from_secs(5));
                        Ended::NotInTime
                    }
                }
            }
            ForkResult::Child => {
                // What the Room's launcher does for the supervisor, whose
                // jobs then have what it has: a group of its own, no
                // input, into the image, root given up, no way back to
                // more, and the profile last, as the program starts.
                let ready = rustix::process::setsid().is_ok()
                    && hatchling_sys::fds::null_stdio().is_ok()
                    && rustix::stdio::dup2_stdout(&kept).is_ok()
                    && rustix::stdio::dup2_stderr(&kept).is_ok()
                    && std::os::unix::fs::chroot(root).is_ok()
                    && std::env::set_current_dir("/").is_ok()
                    && hatchling_sys::drop_to(user, user).is_ok()
                    && hatchling_sys::set_no_new_privs().is_ok();
                if !ready {
                    hatchling_sys::exit_child(120);
                }
                let _ = hatchling_sys::exec(
                    Path::new("/bin/sh"),
                    &["/bin/sh", "-c", &script],
                    &[
                        ("PATH", JOB_PATH),
                        ("HOME", &home),
                        ("TMPDIR", SESSION_DIR),
                        ("LANG", "C.UTF-8"),
                    ],
                    || hatchling_sys::seccomp::install(&profile),
                );
                hatchling_sys::exit_child(121);
            }
        }
    }
}
