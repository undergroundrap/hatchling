//! Run the actual supervisor executable and its child under the room filter.
//! This test target has no libtest harness. Its main stays single-threaded
//! so the forked launch fixture may allocate while preparing exec and seccomp.
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
    use std::io::Write as _;
    use std::os::fd::AsFd as _;
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::time::Duration;

    use hatchling_core::framing::{read_frame, write_frame};
    use hatchling_core::room::{Ask, Captured, End, Ended, JobId, Say, Stream};
    use hatchling_sys::{Child, ForkResult, SeccompProfile};
    use rustix::event::{PollFd, PollFlags, poll};
    use rustix::process::{
        Pid, PidfdFlags, Signal, WaitOptions, getpid, pidfd_open, pidfd_send_signal,
        set_child_subreaper, waitpid,
    };

    #[allow(
        clippy::redundant_pub_crate,
        reason = "only the parent test entry point calls this proof"
    )]
    pub(super) fn run() {
        // Orphaned grandchildren must come here, so init cannot mask a missing wait.
        set_child_subreaper(Some(getpid())).unwrap();
        the_room_filter_allows_the_supervisor_to_run_stop_and_capture_real_jobs();
        the_supervisor_ends_on_what_it_cannot_read();
        the_supervisor_reaps_live_work_on_disconnect_or_bad_input();
        let _ = writeln!(
            std::io::stdout(),
            "three supervisor proofs passed under the room filter"
        );
    }

    /// A directory to stand for the Room's session directory, made anew.
    fn session(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-room-proof-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn boot(session: &Path) -> (Child, UnixStream) {
        let session = session.to_str().unwrap();
        let (peer, endpoint) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
        let profile = SeccompProfile::room().unwrap();
        let path = env!("CARGO_BIN_EXE_hatchling-room");
        match hatchling_sys::fork().unwrap() {
            ForkResult::Parent { child } => {
                drop(endpoint);
                (child, peer)
            }
            ForkResult::Child => {
                drop(peer);
                let setup = hatchling_sys::place_across_exec(&[endpoint.as_fd()], 3)
                    .and_then(|()| hatchling_sys::set_no_new_privs());
                if setup.is_err() {
                    hatchling_sys::exit_child(120);
                }
                let _ = hatchling_sys::exec(
                    Path::new(path),
                    &[path, session],
                    &[("ROOM_TEST_PARENT", "set")],
                    || hatchling_sys::seccomp::install(&profile),
                );
                hatchling_sys::exit_child(121);
            }
        }
    }

    fn job(name: &str) -> JobId {
        name.parse().unwrap()
    }

    /// Asks, and takes the answer: it must echo the ask's nonce.
    fn ask(peer: &UnixStream, ask: &Ask) -> Say {
        write_frame(peer, &serde_json::to_vec(ask).unwrap()).unwrap();
        let said: Say = serde_json::from_slice(&read_frame(peer).unwrap()).unwrap();
        assert_eq!(said.nonce(), Some(ask.nonce()), "{said:?}");
        said
    }

    fn start(peer: &UnixStream, name: &str, argv: &[&str], dir: &Path, stdin: &[u8]) {
        let said = ask(
            peer,
            &Ask::Start {
                nonce: format!("start-{name}"),
                job: job(name),
                argv: argv.iter().map(|part| (*part).to_owned()).collect(),
                dir: dir.to_str().unwrap().to_owned(),
                stdin: stdin.to_vec(),
            },
        );
        assert!(matches!(said, Say::Started { .. }), "{said:?}");
    }

    /// The next thing said: the end of the job `name`.
    fn ended(peer: &UnixStream, name: &str) -> (Ended, Captured, Captured) {
        let said: Say = serde_json::from_slice(&read_frame(peer).unwrap()).unwrap();
        let Say::Ended {
            job: id,
            how,
            stdout,
            stderr,
        } = said
        else {
            unreachable!("{said:?} isn't the end of {name}");
        };
        assert_eq!(id, job(name));
        (how, stdout, stderr)
    }

    fn piece(peer: &UnixStream, name: &str, stream: Stream) -> Vec<u8> {
        let said = ask(
            peer,
            &Ask::Piece {
                nonce: format!("piece-{name}"),
                job: job(name),
                stream,
                end: End::Head,
            },
        );
        let Say::Piece { bytes, .. } = said else {
            unreachable!("{said:?} isn't a piece of {name}");
        };
        bytes
    }

    fn the_room_filter_allows_the_supervisor_to_run_stop_and_capture_real_jobs() {
        let dir = session("jobs");
        let (supervisor, peer) = boot(&dir);
        assert_eq!(read_frame(&peer).unwrap(), b"ready");
        // A job gets none of the supervisor's descriptors or environment.
        let apart =
            "test ! -e /proc/self/fd/3 && test -z \"$ROOM_TEST_PARENT\" && exit 23; exit 24";
        start(&peer, "apart", &["/bin/sh", "-c", apart], &dir, b"");
        assert_eq!(ended(&peer, "apart").0, Ended::Exited(23));
        // A stop kills a job that is running.
        start(&peer, "sleeper", &["sleep", "60"], &dir, b"");
        let stopped = ask(
            &peer,
            &Ask::Stop {
                nonce: String::from("stop"),
                job: job("sleeper"),
            },
        );
        assert!(matches!(stopped, Say::Stopping { .. }), "{stopped:?}");
        assert_eq!(ended(&peer, "sleeper").0, Ended::Signaled(9));
        // What a job reads and writes is kept, in memory and in its
        // files, in the session directory.
        let both = "cat; echo err >&2";
        start(&peer, "writer", &["sh", "-c", both], &dir, b"fed\n");
        let (how, stdout, stderr) = ended(&peer, "writer");
        assert_eq!(how, Ended::Exited(0));
        assert_eq!((stdout.head, stderr.head), (4, 4));
        stdout.check().unwrap();
        assert_eq!(piece(&peer, "writer", Stream::Stdout), b"fed\n");
        assert_eq!(piece(&peer, "writer", Stream::Stderr), b"err\n");
        let kept = dir.join("jobs");
        assert_eq!(std::fs::read(kept.join("writer.stdout")).unwrap(), b"fed\n");
        assert_eq!(std::fs::read(kept.join("writer.stderr")).unwrap(), b"err\n");
        assert!(!kept.join("writer.stdin").exists());
        drop(peer);
        assert_eq!(supervisor.wait_deadline(Duration::from_secs(4)).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn the_supervisor_ends_on_what_it_cannot_read() {
        let unnamed: &[u8] =
            br#"{"ask":"start","nonce":"n","job":"../j","argv":["true"],"dir":"/","stdin":""}"#;
        for request in [b"[".as_slice(), b"[\"start\",\"/bin/sh\"]", unnamed] {
            let dir = session("unread");
            let (supervisor, peer) = boot(&dir);
            assert_eq!(read_frame(&peer).unwrap(), b"ready");
            write_frame(&peer, request).unwrap();
            assert!(read_frame(&peer).is_err());
            let status = supervisor.wait_deadline(Duration::from_secs(4)).unwrap();
            assert_eq!(hatchling_sys::child_exit_code(status), 1);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    fn the_supervisor_reaps_live_work_on_disconnect_or_bad_input() {
        for malformed in [false, true] {
            let dir = session("reaped");
            let (supervisor, peer) = boot(&dir);
            assert_eq!(read_frame(&peer).unwrap(), b"ready");
            start(&peer, "sleeper", &["/bin/sleep", "60"], &dir, b"");
            let pid = supervisor.pid().get();
            let children =
                std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap();
            let child = Pid::from_raw(children.trim().parse().unwrap()).unwrap();
            let watched = pidfd_open(child, PidfdFlags::empty()).unwrap();

            if malformed {
                write_frame(&peer, b"[").unwrap();
            }
            drop(peer);
            let ended = supervisor.wait_deadline(Duration::from_secs(4));
            let adopted = waitpid(Some(child), WaitOptions::NOHANG);
            if matches!(adopted, Ok(None)) {
                // Clean up the exact adopted child before failing the proof.
                pidfd_send_signal(&watched, Signal::Kill).unwrap();
                let mut poll_child = [PollFd::new(&watched, PollFlags::IN)];
                assert_eq!(poll(&mut poll_child, 1000).unwrap(), 1);
                assert!(waitpid(Some(child), WaitOptions::NOHANG).unwrap().is_some());
            }
            assert!(
                matches!(adopted, Err(rustix::io::Errno::CHILD)),
                "the supervisor left its direct child for the test to reap: {adopted:?}"
            );
            assert_eq!(
                hatchling_sys::child_exit_code(ended.unwrap()),
                i32::from(malformed)
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
