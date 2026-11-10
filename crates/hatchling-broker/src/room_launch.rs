//! One-shot mount admission through the installed spawner. The trusted
//! helper enters the room view and exits before any image program can run.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

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
) -> Result<(), String> {
    crate::room_disk::with_open_volume(disk, |volume, device| {
        let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
        reconcile(&mut pending)?;
        let (root, digest) = read_admission(Path::new(RECORD))?;
        let image_dir = Path::new(IMAGE_DIR);
        hatchling_sys::trusted_dir_chain(image_dir, &[0]).map_err(|error| error.to_string())?;
        let directory = File::open(image_dir).map_err(|error| error.to_string())?;
        let image = PinnedRoomImage::pin(directory.as_fd()).map_err(|error| error.to_string())?;
        let group = room_group(cgroup_parent)?;
        let mapper = image
            .activate(&root, &digest)
            .map_err(|error| remember(error, &mut pending))?;
        let spec = MountSpec {
            volume: volume.to_owned(),
            device: device.to_owned(),
            staging,
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
        handoff(&owner).map_err(|error| error.to_string())
    })();
    drop(owner);
    let status = child
        .wait_deadline(DEADLINE)
        .map_err(|error| error.to_string())?;
    sent?;
    if status != 0 {
        return Err(format!(
            "the trusted room mount helper failed (status {})",
            hatchling_sys::child_exit_code(status)
        ));
    }
    Ok(())
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
    let staging = File::open(spec.staging).map_err(|error| error.to_string())?;
    RoomMounts::enter_handed_off_volume(
        namespace,
        channel,
        &spec.volume,
        &spec.device,
        staging.into(),
        0,
        0,
    )
    .map_err(|error| error.to_string())
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
        };
        let mut launcher = None;
        let started = std::time::Instant::now();
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
