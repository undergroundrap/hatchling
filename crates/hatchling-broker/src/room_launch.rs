//! One-shot Room admission through the installed spawner. A trusted init
//! mounts the view and reaps the confined image supervisor after readiness.

use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use hatchling_core::framing::{read_frame, write_frame};
use hatchling_sys::cgroup::{CellCgroup, CgroupLimits};
use hatchling_sys::room_image::{
    PendingRoomImageMapper, PinnedRoomImage, RoomImageActivationError, RoomImageMapper,
};
use hatchling_sys::room_mounts::RoomMounts;
use serde::{Deserialize, Serialize};

use crate::room_disk::RoomDisk;
use crate::spawner::ROOM_MOUNT_SUBCOMMAND;

const RECORD: &str = "/etc/hatchling/room-image.admission";
const IMAGE_DIR: &str = "/usr/local/share/hatchling/room";
const UNSHARE: &str = "/usr/bin/unshare";
const SUPERVISOR: &str = "/usr/local/libexec/hatchling-room";
const SUPERVISOR_FD: i32 = 3;
const READY: &[u8; 9] = b"\0\0\0\x05ready";
const UNSUPPORTED_ARCH: &str = "the Room supervisor requires x86_64 Linux";
const DEADLINE: Duration = Duration::from_secs(30);

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

enum Pending {
    Activation(PendingRoomImageMapper),
    Mount {
        mapper: RoomImageMapper,
        launcher: Option<OwnedFd>,
        group: CellCgroup,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MountSpec {
    volume: PathBuf,
    device: String,
    staging: PathBuf,
    uid: u32,
    gid: u32,
}

/// The installer must publish this record from separately attested
/// release data. The image directory itself has no authority over it.
fn read_admission(path: &Path) -> Result<(String, String), String> {
    let parent = path.parent().ok_or("no room image admission directory")?;
    hatchling_sys::trusted_dir_chain(parent, &[0]).map_err(|error| error.to_string())?;
    let meta = fs::symlink_metadata(path).map_err(|error| {
        format!("cannot read the installed room image admission record: {error}")
    })?;
    if !meta.is_file()
        || meta.uid() != 0
        || meta.mode() & 0o022 != 0
        || meta.nlink() != 1
        || meta.len() != 130
    {
        return Err(String::from(
            "the installed room image admission record is invalid",
        ));
    }
    let bytes = hatchling_sys::read_file_head(path, 130).map_err(|error| error.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
    let (root, digest) = text
        .strip_suffix('\n')
        .and_then(|lines| lines.split_once('\n'))
        .filter(|(root, digest)| root.len() == 64 && !digest.contains('\n'))
        .ok_or("the installed room image admission record is malformed")?;
    Ok((root.to_owned(), digest.to_owned()))
}

fn reconcile(pending: &mut Option<Pending>) -> Result<(), String> {
    match pending {
        Some(Pending::Activation(mapper)) => mapper
            .close()
            .map_err(|error| format!("cannot remove room mapper {}: {error}", mapper.name()))?,
        Some(Pending::Mount {
            mapper,
            launcher,
            group,
        }) => {
            after_launcher_exit(
                launcher.as_ref().map(OwnedFd::as_fd),
                || group.finish().map_err(|error| error.to_string()),
                || {
                    mapper.close().map_err(|error| {
                        format!(
                            "cannot remove room mapper {}: {error}",
                            mapper.device().display()
                        )
                    })
                },
            )?;
        }
        None => {}
    }
    *pending = None;
    Ok(())
}

fn after_launcher_exit(
    launcher: Option<BorrowedFd<'_>>,
    drain: impl FnOnce() -> Result<(), String>,
    close: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if let Some(launcher) = launcher {
        hatchling_sys::stop_pidfd_and_confirm_exit(launcher)
            .map_err(|error| format!("cannot confirm room mount helper exit: {error}"))?;
    }
    // An exited launcher is not proof that every descendant has left.
    // Failed drain leaves the mapper owned for a later cleanup attempt.
    drain()?;
    close()
}

pub(crate) fn retry_pending() -> Result<(), String> {
    reconcile(&mut PENDING.lock().unwrap_or_else(PoisonError::into_inner))
}

fn remember(error: RoomImageActivationError, pending: &mut Option<Pending>) -> String {
    let reason = error.to_string();
    if let RoomImageActivationError::StartupCleanup {
        mapper,
        cleanup: Some(_),
        ..
    } = error
    {
        *pending = Some(Pending::Activation(mapper));
    }
    reason
}

fn room_limits(half_memory: u64, available_cores: u64) -> CgroupLimits {
    CgroupLimits {
        processes: Some(512),
        memory: Some((half_memory / 2).min(4 << 30)),
        cpu_millis: Some(available_cores.saturating_mul(500)),
    }
}

fn room_group(parent: Option<&Path>) -> Result<CellCgroup, String> {
    let parent = parent.ok_or("room launch requires a delegated cgroup v2 parent")?;
    let memory = hatchling_sys::cgroup::default_memory().map_err(|error| error.to_string())?;
    let cores = std::thread::available_parallelism().map_err(|error| error.to_string())?;
    let cores = u64::try_from(cores.get()).map_err(|error| error.to_string())?;
    CellCgroup::create_without_swap(parent, room_limits(memory, cores))
        .map_err(|error| format!("cannot create the room cgroup: {error}"))
}

pub(crate) fn check_installed(
    program: &Path,
    disk: &RoomDisk,
    staging: PathBuf,
    cgroup_parent: Option<&Path>,
    identity: impl FnOnce() -> Result<(u32, u32), String>,
) -> Result<(), String> {
    crate::room_disk::with_open_volume(disk, |volume, device| {
        if !cfg!(target_arch = "x86_64") {
            return Err(UNSUPPORTED_ARCH.into());
        }
        let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
        reconcile(&mut pending)?;
        let (root, digest) = read_admission(Path::new(RECORD))?;
        let image_dir = Path::new(IMAGE_DIR);
        hatchling_sys::trusted_dir_chain(image_dir, &[0]).map_err(|error| error.to_string())?;
        let directory = File::open(image_dir).map_err(|error| error.to_string())?;
        let image = PinnedRoomImage::pin(directory.as_fd()).map_err(|error| error.to_string())?;
        let group = room_group(cgroup_parent)?;
        let (uid, gid) = identity()?;
        let mapper = image
            .activate(&root, &digest)
            .map_err(|error| remember(error, &mut pending))?;
        let spec = MountSpec {
            volume: volume.to_owned(),
            device: device.to_owned(),
            staging,
            uid,
            gid,
        };
        let mut launcher = None;
        let result = check_mount(program, &spec, &group, &mut launcher, |owner| {
            mapper.send_to_launcher(owner)
        });
        *pending = Some(Pending::Mount {
            mapper,
            launcher,
            group,
        });
        reconcile(&mut pending)?;
        drop(pending);
        result
    })
}

fn check_mount(
    program: &Path,
    spec: &MountSpec,
    group: &CellCgroup,
    launcher_pidfd: &mut Option<OwnedFd>,
    handoff: impl FnOnce(&UnixStream) -> Result<(), hatchling_sys::Error>,
) -> Result<(), String> {
    let (owner, launcher) = UnixStream::pair().map_err(|error| error.to_string())?;
    let (supervisor, endpoint) = UnixStream::pair().map_err(|error| error.to_string())?;
    owner
        .set_write_timeout(Some(DEADLINE))
        .map_err(|error| error.to_string())?;
    owner
        .set_read_timeout(Some(DEADLINE))
        .map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(spec).map_err(|error| error.to_string())?;
    let mut command = mount_command(program, launcher);
    group
        .join_on_exec(&mut command)
        .map_err(|error| error.to_string())?;
    let mut process = command
        .spawn()
        .map_err(|error| format!("cannot start the room mount helper: {error}"))?;
    // The command holds its own copy of the launch end. Drop it so a
    // helper that exits early delivers EOF instead of a handshake timeout.
    drop(command);
    let child = i32::try_from(process.id())
        .ok()
        .and_then(hatchling_sys::ChildPid::new)
        .ok_or_else(|| String::from("the room mount helper has an invalid PID"))
        .and_then(|pid| hatchling_sys::Child::new(pid).map_err(|error| error.to_string()));
    let child = match child {
        Ok(child) => child,
        Err(reason) => {
            let _ = process.kill();
            let _ = process.wait();
            return Err(reason);
        }
    };
    let sent = (|| {
        // No mapper or volume inputs cross until the spawner has an
        // independent exit witness for the actual namespace process.
        *launcher_pidfd =
            Some(hatchling_sys::net::recv_fd(owner.as_fd()).map_err(|error| error.to_string())?);
        write_frame(&owner, &bytes).map_err(|error| error.to_string())?;
        hatchling_sys::net::send_fd(owner.as_fd(), endpoint.as_fd())
            .map_err(|error| error.to_string())?;
        handoff(&owner).map_err(|error| error.to_string())
    })();
    drop((owner, endpoint));
    let deadline = Instant::now() + DEADLINE;
    let ready = sent.and_then(|()| {
        wait_for_ready(deadline, |bytes, remaining| {
            supervisor.set_read_timeout(Some(remaining))?;
            (&supervisor).read(bytes)
        })
    });
    // After ready the supervisor waits for EOF before it exits.
    drop(supervisor);
    let status = child
        .wait_deadline(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| error.to_string())?;
    if status != 0 {
        return Err(format!(
            "the trusted room mount helper failed (status {})",
            hatchling_sys::child_exit_code(status)
        ));
    }
    ready
}

fn wait_for_ready(
    deadline: Instant,
    mut read: impl FnMut(&mut [u8], Duration) -> io::Result<usize>,
) -> Result<(), String> {
    let mut bytes = [0; READY.len()];
    let mut unread = bytes.as_mut_slice();
    while !unread.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(String::from(
                "the Room supervisor did not become ready in time",
            ));
        }
        match read(unread, remaining) {
            Ok(0) => return Err(String::from("the Room supervisor closed before readiness")),
            Ok(count) => {
                unread = unread
                    .get_mut(count..)
                    .ok_or("invalid Room readiness length")?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("cannot read Room supervisor readiness: {error}")),
        }
    }
    if &bytes == READY {
        Ok(())
    } else {
        Err(String::from("the Room supervisor sent invalid readiness"))
    }
}

fn mount_command(program: &Path, launcher: UnixStream) -> Command {
    let mut command = Command::new(UNSHARE);
    // The original socket stays close-on-exec in the spawner. Only this
    // child's stdio setup installs the launch end as descriptor zero.
    command
        .args(["--pid", "--fork", "--kill-child", "--net", "--ipc"])
        .arg(program)
        .arg(ROOM_MOUNT_SUBCOMMAND)
        .env_clear()
        .stdin(Stdio::from(OwnedFd::from(launcher)))
        .stdout(Stdio::null());
    command
}

fn enter_from_spawner(channel: UnixStream) -> Result<(), String> {
    channel
        .set_read_timeout(Some(DEADLINE))
        .map_err(|error| error.to_string())?;
    let namespace = RoomMounts::prepare_namespace().map_err(|error| error.to_string())?;
    let pidfd = hatchling_sys::self_pidfd().map_err(|error| error.to_string())?;
    hatchling_sys::net::send_fd(channel.as_fd(), pidfd.as_fd())
        .map_err(|error| error.to_string())?;
    drop(pidfd);
    let bytes = read_frame(&channel).map_err(|error| error.to_string())?;
    let spec: MountSpec = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let endpoint =
        hatchling_sys::net::recv_fd(channel.as_fd()).map_err(|error| error.to_string())?;
    let profile = room_profile()?;
    hatchling_sys::hold_core_dumps_off().map_err(|error| error.to_string())?;
    let staging = File::open(spec.staging).map_err(|error| error.to_string())?;
    RoomMounts::enter_handed_off_volume(
        namespace,
        channel,
        &spec.volume,
        &spec.device,
        staging.into(),
        spec.uid,
        spec.gid,
    )
    .map_err(|error| error.to_string())?;
    match hatchling_sys::fork().map_err(|error| error.to_string())? {
        hatchling_sys::ForkResult::Parent { child } => {
            drop(endpoint);
            let status = hatchling_sys::reap_as_init(&child);
            if status != 0 {
                return Err(format!("the Room supervisor exited with status {status}"));
            }
            Ok(())
        }
        hatchling_sys::ForkResult::Child => {
            let result = (|| -> Result<(), hatchling_sys::Error> {
                hatchling_sys::fds::null_stdio()?;
                // No host descriptors cross exec. Only the separate,
                // untrusted supervisor socket is installed on its fixed number.
                hatchling_sys::cloexec_fds_above_stderr()?;
                hatchling_sys::place_across_exec(&[endpoint.as_fd()], SUPERVISOR_FD)?;
                hatchling_sys::drop_to(spec.uid, spec.gid)?;
                Err(hatchling_sys::exec(
                    Path::new(SUPERVISOR),
                    &[SUPERVISOR],
                    &[],
                    || hatchling_sys::seccomp::install(&profile),
                ))
            })();
            if let Err(error) = result {
                let _ = writeln!(io::stderr().lock(), "hatchling Room supervisor: {error}");
            }
            hatchling_sys::exit_child(1);
        }
    }
}

fn room_profile() -> Result<hatchling_sys::SeccompProfile, String> {
    #[cfg(target_arch = "x86_64")]
    {
        hatchling_sys::SeccompProfile::room().map_err(|error| error.to_string())
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        Err(UNSUPPORTED_ARCH.into())
    }
}

pub(crate) fn helper_main(
    take_stdin: impl FnOnce() -> Result<OwnedFd, hatchling_sys::Error>,
) -> ExitCode {
    let result = if hatchling_sys::euid() == 0 {
        take_stdin()
            .map_err(|error| error.to_string())
            .and_then(|fd| enter_from_spawner(UnixStream::from(fd)))
    } else {
        Err(String::from("the room mount helper requires root"))
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            let _ = writeln!(io::stderr().lock(), "hatchling room mount helper: {reason}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room_disk::{RoomDiskOp, tests::Bench};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::process::ExitStatusExt as _;

    #[test]
    fn room_mount_admission_requires_an_open_disk() {
        let bench = Bench::new("admission-needs-open-disk");
        bench.run(RoomDiskOp::Create).unwrap();
        let reason = check_installed(
            Path::new("/bin/false"),
            &bench.disk,
            bench.root.clone(),
            None,
            || unreachable!("a closed disk is refused before identity lookup"),
        )
        .unwrap_err();
        assert!(reason.contains("isn't mapped"), "{reason}");
    }

    #[test]
    fn room_limits_bound_memory_and_cpu_independently() {
        for (memory, cores, expected_memory, cpu) in
            [(3 << 30, 1, 1536 << 20, 500), (12 << 30, 7, 4 << 30, 3500)]
        {
            let limits = room_limits(memory, cores);
            assert_eq!(limits.memory, Some(expected_memory));
            assert_eq!(limits.processes, Some(512));
            assert_eq!(limits.cpu_millis, Some(cpu));
        }
        assert!(
            room_group(None)
                .unwrap_err()
                .contains("requires a delegated")
        );
        assert!(
            room_group(Some(Path::new("/tmp")))
                .unwrap_err()
                .contains("not cgroup v2")
        );
    }

    #[test]
    fn room_mount_record_refuses_an_untrusted_parent() {
        let reason = read_admission(Path::new("/proc/self/room-image.admission")).unwrap_err();
        assert!(reason.contains("isn't a directory itself"), "{reason}");
    }

    #[test]
    fn room_mount_cleanup_of_an_unactivated_image_is_a_no_op() {
        let mut pending = None;
        let failure = io::Error::other("image verification failed").into();
        assert_eq!(remember(failure, &mut pending), "image verification failed");
        assert!(pending.is_none());
        reconcile(&mut pending).unwrap();
        retry_pending().unwrap();
    }

    #[test]
    fn room_mount_check_refuses_a_helper_that_exits_unsuccessfully() {
        let Some(parent) = std::env::var_os("HATCHLING_TEST_CGROUP") else {
            assert_ne!(
                std::env::var("HATCHLING_REQUIRE_CGROUP").as_deref(),
                Ok("1")
            );
            return;
        };
        let group = room_group(Some(Path::new(&parent))).unwrap();
        let spec = MountSpec {
            volume: "/missing/volume".into(),
            device: "0:0".into(),
            staging: "/missing/staging".into(),
            uid: 123,
            gid: 123,
        };
        let mut launcher = None;
        let started = Instant::now();
        assert!(
            check_mount(
                Path::new("/bin/false"),
                &spec,
                &group,
                &mut launcher,
                |_| Ok(())
            )
            .is_err()
        );
        assert!(launcher.is_none());
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "launcher exit did not deliver EOF promptly"
        );
    }

    #[test]
    fn room_mount_socket_ends_cannot_reach_an_unrelated_exec() {
        let (owner, launcher) = UnixStream::pair().unwrap();
        let socket =
            |end: &UnixStream| fs::read_link(format!("/proc/self/fd/{}", end.as_raw_fd())).unwrap();
        let names = [socket(&owner), socket(&launcher)];
        let command = mount_command(Path::new("/bin/false"), launcher);
        let unrelated = Command::new("/usr/bin/ls")
            .args(["-l", "/proc/self/fd"])
            .output()
            .unwrap();
        assert!(unrelated.status.success());
        let descriptors = String::from_utf8(unrelated.stdout).unwrap();
        for name in &names {
            assert!(
                !descriptors.contains(name.to_str().unwrap()),
                "{descriptors}"
            );
        }
        drop((command, owner));
    }

    #[test]
    fn room_supervisor_requires_the_complete_readiness_frame() {
        let read = |bytes: &[u8]| {
            let (peer, mut endpoint) = UnixStream::pair().unwrap();
            endpoint.write_all(bytes).unwrap();
            drop(endpoint);
            wait_for_ready(
                Instant::now() + Duration::from_secs(2),
                |bytes, remaining| {
                    peer.set_read_timeout(Some(remaining))?;
                    (&peer).read(bytes)
                },
            )
        };
        assert_eq!(read(READY), Ok(()));
        assert_eq!(
            read(b"\0\0\0\x05other"),
            Err("the Room supervisor sent invalid readiness".into())
        );
        assert_eq!(
            read(b"\0\0\0\x05read"),
            Err("the Room supervisor closed before readiness".into())
        );
        assert_eq!(
            wait_for_ready(Instant::now(), |_, _| Err(io::Error::other(
                "expired readiness attempted a read"
            ))),
            Err("the Room supervisor did not become ready in time".into())
        );
    }

    #[test]
    fn room_profile_is_available_only_on_its_supported_architecture() {
        assert_eq!(room_profile().is_ok(), cfg!(target_arch = "x86_64"));
        #[cfg(target_arch = "x86_64")]
        assert_eq!(
            room_profile().unwrap().allowed_numbers(),
            hatchling_sys::SeccompProfile::room()
                .unwrap()
                .allowed_numbers()
        );
    }

    #[test]
    fn room_readiness_retries_interruptions_and_preserves_other_errors() {
        for kind in [io::ErrorKind::Interrupted, io::ErrorKind::WouldBlock] {
            let mut reads = 0;
            let received = wait_for_ready(
                Instant::now() + Duration::from_secs(2),
                |bytes, remaining| {
                    reads += 1;
                    assert!(remaining > Duration::ZERO && remaining <= Duration::from_secs(2));
                    if reads == 1 {
                        return Err(io::Error::from(kind));
                    }
                    bytes.copy_from_slice(READY);
                    Ok(READY.len())
                },
            );
            assert_eq!(received.is_ok(), kind == io::ErrorKind::Interrupted);
            assert_eq!(
                reads,
                if kind == io::ErrorKind::Interrupted {
                    2
                } else {
                    1
                }
            );
        }
    }

    #[test]
    fn room_mount_cleanup_waits_for_a_live_launcher_before_closing() {
        let mut launcher = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let pid = rustix::process::Pid::from_raw(i32::try_from(launcher.id()).unwrap()).unwrap();
        let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).unwrap();
        let mut closed = false;
        let cleaned = after_launcher_exit(
            Some(pidfd.as_fd()),
            || Ok(()),
            || {
                closed = true;
                let status = launcher.try_wait().map_err(|error| error.to_string())?;
                if status.and_then(|status| status.signal()) != Some(9) {
                    return Err(String::from("mapper close reached before launcher exit"));
                }
                Ok(())
            },
        );
        if cleaned.is_err() || !closed {
            let _ = launcher.kill();
            let _ = launcher.wait();
        }
        cleaned.unwrap();
        assert!(closed, "cleanup skipped mapper close");
    }

    #[test]
    fn room_mapper_close_requires_a_confirmed_cgroup_drain() {
        let reached = std::cell::Cell::new(false);
        let close = || {
            reached.set(true);
            Ok(())
        };
        let reason =
            after_launcher_exit(None, || Err("room remains populated".into()), close).unwrap_err();
        assert_eq!(reason, "room remains populated");
        assert!(!reached.get(), "mapper close ran before the cgroup drained");
        after_launcher_exit(None, || Ok(()), close).unwrap();
        assert!(reached.get());
    }

    #[test]
    fn room_mount_helper_refuses_a_process_outside_the_launcher_namespace() {
        let (_owner, launcher) = UnixStream::pair().unwrap();
        let reason = enter_from_spawner(launcher).unwrap_err();
        assert!(reason.contains("expected namespace init"), "{reason}");

        let (_owner, launcher) = UnixStream::pair().unwrap();
        assert_eq!(
            helper_main(|| {
                assert_eq!(hatchling_sys::euid(), 0, "helper used stdin without root");
                Ok(launcher.into())
            }),
            ExitCode::FAILURE
        );
    }
}
