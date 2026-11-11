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
    use std::time::{Duration, Instant};

    use hatchling_core::framing::{read_frame, write_frame};
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
        the_room_filter_allows_the_supervisor_to_start_poll_and_stop_a_real_child();
        the_supervisor_ends_on_a_malformed_or_relative_start();
        the_supervisor_reaps_live_work_on_disconnect_or_bad_input();
        let _ = writeln!(
            std::io::stdout(),
            "three supervisor proofs passed under the room filter"
        );
    }

    fn boot() -> (Child, UnixStream) {
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
                    &[path],
                    &[("ROOM_TEST_PARENT", "set")],
                    || hatchling_sys::seccomp::install(&profile),
                );
                hatchling_sys::exit_child(121);
            }
        }
    }

    fn exchange(peer: &UnixStream, request: &[&str]) -> Vec<u8> {
        write_frame(peer, &serde_json::to_vec(request).unwrap()).unwrap();
        read_frame(peer).unwrap()
    }

    fn reap_job(peer: &UnixStream, expected: &[u8]) {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let reply = exchange(peer, &["poll"]);
            if reply != b"running" {
                assert_eq!(reply, expected);
                break;
            }
            assert!(Instant::now() < until, "the child did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn the_room_filter_allows_the_supervisor_to_start_poll_and_stop_a_real_child() {
        let (supervisor, peer) = boot();
        assert_eq!(read_frame(&peer).unwrap(), b"ready");
        assert_eq!(exchange(&peer, &["poll"]), b"idle");
        assert_eq!(exchange(&peer, &["stop"]), b"idle");
        assert_eq!(
            exchange(
                &peer,
                &[
                    "start",
                    "/bin/sh",
                    "-c",
                    "test ! -e /proc/self/fd/3 && test -z \"$ROOM_TEST_PARENT\" && exit 23; exit 24",
                ],
            ),
            b"started"
        );
        reap_job(&peer, b"exit:23");
        assert_eq!(exchange(&peer, &["start", "/bin/sleep", "60"]), b"started");
        assert_eq!(exchange(&peer, &["start", "/bin/sh"]), b"busy");
        assert_eq!(exchange(&peer, &["poll"]), b"running");
        assert_eq!(exchange(&peer, &["stop"]), b"stop-requested");
        reap_job(&peer, b"signal:9");
        // One command after another on the one connection, each answered
        // with its own nonce: the Room is kept between commands.
        for nonce in ["first", "second"] {
            let reply = exchange(&peer, &["run", "/bin/true", nonce]);
            let (echo, result): (String, serde_json::Value) =
                serde_json::from_slice(&reply).unwrap();
            assert_eq!(echo, nonce);
            assert!(result.get("status").is_some(), "{result}");
        }
        assert_eq!(exchange(&peer, &["poll"]), b"idle");
        drop(peer);
        assert_eq!(supervisor.wait_deadline(Duration::from_secs(4)).unwrap(), 0);
    }

    fn the_supervisor_ends_on_a_malformed_or_relative_start() {
        for request in [b"[".as_slice(), b"[\"start\",\"bin/sh\"]"] {
            let (supervisor, peer) = boot();
            assert_eq!(read_frame(&peer).unwrap(), b"ready");
            write_frame(&peer, request).unwrap();
            assert!(read_frame(&peer).is_err());
            let status = supervisor.wait_deadline(Duration::from_secs(4)).unwrap();
            assert_eq!(hatchling_sys::child_exit_code(status), 1);
        }
    }

    fn the_supervisor_reaps_live_work_on_disconnect_or_bad_input() {
        for malformed in [false, true] {
            let (supervisor, peer) = boot();
            assert_eq!(read_frame(&peer).unwrap(), b"ready");
            assert_eq!(exchange(&peer, &["start", "/bin/sleep", "60"]), b"started");
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
        }
    }
}
