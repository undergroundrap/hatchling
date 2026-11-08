//! The room disk with stand-ins: a cryptsetup and an mke2fs this test
//! writes, which record what they're run with, and a keeper that answers
//! each ask with a key of the test's own. Run as anyone; nothing here is
//! root's, or mounts.

use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};
use std::sync::Arc;

use hatchling_core::framing::read_frame_or_eof;

use super::*;

/// A key of the test's own, from the OS's randomness: what the keeper
/// stand-in gives every ask.
fn test_key() -> Vec<u8> {
    use std::io::Read as _;
    let mut key = vec![0u8; 64];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut key))
        .unwrap();
    key
}

fn mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// One program's run, as the stand-in recorded it.
#[derive(Debug)]
pub(crate) struct Ran {
    /// Its argument list, its own name first.
    pub argv: Vec<String>,
    /// Its environment, as it started.
    pub environ: Vec<String>,
    /// What it read on stdin.
    pub stdin: Vec<u8>,
}

/// A room disk of stand-ins, in a scratch directory of its own, checked
/// from there down, owned by this process's user.
pub(crate) struct Bench {
    pub root: PathBuf,
    pub disk: RoomDisk,
    pub keeper: Arc<Mutex<UnixStream>>,
    /// Each ask the keeper stand-in took: the image, and what for.
    pub asked: Arc<Mutex<Vec<RoomKeyAsk>>>,
    pub key: Vec<u8>,
}

impl Bench {
    pub(crate) fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("hatchling-room-disk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["", "tools", "sys", "log"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            mode(&root.join(dir), 0o755);
        }
        let (ours, keepers) = UnixStream::pair().unwrap();
        let key = test_key();
        let asked = Arc::new(Mutex::new(Vec::new()));
        std::thread::spawn({
            let key = key.clone();
            let asked = Arc::clone(&asked);
            move || keeper_stand_in(&keepers, &key, &asked)
        });
        let bench = Self {
            disk: RoomDisk {
                dir: root.join("room"),
                size: MIN_SIZE,
                cryptsetup: root.join("tools/cryptsetup"),
                mke2fs: root.join("tools/mke2fs"),
                trusted: vec![hatchling_sys::euid()],
                top: root.clone(),
                sys_block: root.join("sys"),
                timeout: Duration::from_secs(20),
            },
            root,
            keeper: Arc::new(Mutex::new(ours)),
            asked,
            key,
        };
        bench.write_tools();
        bench
    }

    /// The two stand-ins: each records its argument list, its environment
    /// and its stdin. cryptsetup also does what the real one would to the
    /// test's world: a format writes a LUKS2 header with the UUID it was
    /// given, an open lists a mapping, a close takes it away. A file
    /// `fail-<command>` fails that command; `hang` makes every one wait.
    fn write_tools(&self) {
        let root = self.root.display();
        let record = format!(
            "#!/bin/sh\n\
             n=$(ls {root}/log | wc -l)\n\
             d={root}/log/$n\n\
             mkdir \"$d\"\n\
             printf '%s\\n' \"$0\" \"$@\" > \"$d/argv\"\n\
             cat /proc/$$/environ > \"$d/environ\"\n\
             cat > \"$d/stdin\"\n\
             [ -e {root}/hang ] && sleep 30\n\
             [ -e \"{root}/fail-$1\" ] && {{ echo \"the stand-in fails $1\" >&2; exit 3; }}\n"
        );
        let cryptsetup = format!(
            "{record}\
             for last in \"$@\"; do :; done\n\
             case \"$1\" in\n\
             luksFormat)\n\
               uuid=$(printf '%s\\n' \"$@\" | grep -A1 -x -- --uuid | tail -n 1)\n\
               printf '%s' \"$uuid\" | tr -d - > {root}/uuid-hex\n\
               {{ printf 'LUKS\\272\\276\\000\\002'; head -c 160 /dev/zero; printf '%s' \"$uuid\"; }} | dd of=\"$last\" conv=notrunc status=none ;;\n\
             open)\n\
               mkdir -p {root}/sys/dm-7/dm\n\
               printf '%s\\n' \"$last\" > {root}/sys/dm-7/dm/name\n\
               printf 'CRYPT-LUKS2-%s-%s\\n' \"$(cat {root}/uuid-hex)\" \"$last\" > {root}/sys/dm-7/dm/uuid ;;\n\
             close)\n\
               [ -e {root}/keep-mapped ] || rm -rf {root}/sys/dm-7 ;;\n\
             esac\n\
             exit 0\n"
        );
        for (name, script) in [
            ("cryptsetup", cryptsetup),
            ("mke2fs", format!("{record}exit 0\n")),
        ] {
            let path = self.root.join("tools").join(name);
            std::fs::write(&path, script).unwrap();
            mode(&path, 0o755);
        }
    }

    /// Does `op`.
    pub(crate) fn run(&self, op: RoomDiskOp) -> Result<Option<RoomDiskStatus>, String> {
        run(&self.disk, &self.keeper, op)
    }

    /// Each program run so far, in order.
    pub(crate) fn ran(&self) -> Vec<Ran> {
        let count = std::fs::read_dir(self.root.join("log")).unwrap().count();
        (0..count)
            .map(|at| {
                let dir = self.root.join("log").join(at.to_string());
                let lines = |name: &str, split: char| -> Vec<String> {
                    std::fs::read_to_string(dir.join(name))
                        .unwrap()
                        .split(split)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect()
                };
                Ran {
                    argv: lines("argv", '\n'),
                    environ: lines("environ", '\0'),
                    stdin: std::fs::read(dir.join("stdin")).unwrap(),
                }
            })
            .collect()
    }

    /// Forgets the programs run so far.
    pub(crate) fn forget_runs(&self) {
        std::fs::remove_dir_all(self.root.join("log")).unwrap();
        std::fs::create_dir(self.root.join("log")).unwrap();
    }

    /// Sets a stand-in's switch: `fail-<command>`, `hang` or `keep-mapped`.
    pub(crate) fn switch(&self, name: &str, on: bool) {
        let path = self.root.join(name);
        if on {
            std::fs::write(path, b"").unwrap();
        } else {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Lists a mapping named `name` with the device-mapper UUID `uuid`, as
    /// the kernel would.
    pub(crate) fn list_mapping(&self, name: &str, uuid: &str) {
        let dm = self.root.join("sys/dm-3/dm");
        std::fs::create_dir_all(&dm).unwrap();
        std::fs::write(dm.join("name"), format!("{name}\n")).unwrap();
        std::fs::write(dm.join("uuid"), format!("{uuid}\n")).unwrap();
    }

    /// Lists this image's own mapping, as an open of it would: the one the
    /// cryptsetup stand-in's close takes away. Its name.
    pub(crate) fn list_own_mapping(&self) -> String {
        let image = self.image_id();
        let name = mapping_name(&image);
        let dm = self.root.join("sys/dm-7/dm");
        std::fs::create_dir_all(&dm).unwrap();
        std::fs::write(dm.join("name"), format!("{name}\n")).unwrap();
        std::fs::write(dm.join("uuid"), format!("{}\n", dm_uuid(&image, &name))).unwrap();
        name
    }

    /// The image's UUID, as made.
    pub(crate) fn image_id(&self) -> RoomImageId {
        self.disk.image_id().unwrap()
    }
}

impl Drop for Bench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The keeper, standing in: each ask is taken whole, and its pipe given
/// `key`.
fn keeper_stand_in(channel: &UnixStream, key: &[u8], asked: &Mutex<Vec<RoomKeyAsk>>) {
    while let Ok(Some(frame)) = read_frame_or_eof(channel) {
        let Ok(ask) = serde_json::from_slice::<RoomKeyAsk>(&frame) else {
            return;
        };
        let Ok(pipe) = hatchling_sys::net::recv_fd(channel.as_fd()) else {
            return;
        };
        asked.lock().unwrap().push(ask);
        let _ = hatchling_sys::write_whole_to_pipe(pipe, key);
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| (*arg).to_owned()).collect()
}

/// The settings: a plain absolute directory, and a size in bytes or whole
/// M or G, from 64M to 1024G. Unset or empty, the defaults.
#[test]
fn the_settings_are_a_plain_directory_and_a_size_in_bounds() {
    let parsed = |dir: Option<&str>, size: Option<&str>| {
        RoomDisk::parse(|key| match key {
            "HATCHLING_ROOM_DISK_DIR" => dir.map(str::to_owned),
            "HATCHLING_ROOM_DISK_SIZE" => size.map(str::to_owned),
            _ => None,
        })
    };
    let default = parsed(None, None).unwrap();
    assert_eq!(default.dir(), Path::new(DEFAULT_DIR));
    assert_eq!(default.size, DEFAULT_SIZE);
    assert_eq!(default, parsed(Some(" "), Some("")).unwrap());
    assert_eq!(default.trusted, [0], "the room disk isn't root's");
    assert_eq!(default.cryptsetup, Path::new("/usr/sbin/cryptsetup"));
    assert_eq!(default.mke2fs, Path::new("/usr/sbin/mke2fs"));
    assert_eq!(default.top, Path::new("/"));
    assert_eq!(
        default.image(),
        Path::new("/var/lib/hatchling/room/disk.img")
    );
    assert_eq!(
        default.mount_point(),
        Path::new("/var/lib/hatchling/room/mnt")
    );
    for (given, size) in [
        ("64M", MIN_SIZE),
        ("1024G", MAX_SIZE),
        ("10G", DEFAULT_SIZE),
        ("10737418240", DEFAULT_SIZE),
    ] {
        assert_eq!(parsed(None, Some(given)).unwrap().size, size, "{given}");
    }
    for refused in [
        "63M",
        "1025G",
        "10K",
        "1.5G",
        "-1",
        "G",
        "ten",
        "99999999999999999999G",
    ] {
        assert!(
            parsed(None, Some(refused)).is_err(),
            "{refused} was taken as a size"
        );
    }
    for refused in ["relative/room", "/var/lib/../room"] {
        assert!(
            parsed(Some(refused), None).is_err(),
            "{refused} was taken as the room directory"
        );
    }
    assert_eq!(parse_size(" 64M ").unwrap(), MIN_SIZE);
}

/// A LUKS2 header names its image by the UUID field, after its magic and
/// version; anything else isn't one of ours.
#[test]
fn an_image_is_named_by_its_headers_uuid() {
    let uuid = "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0";
    let mut head = vec![0u8; 512];
    head.splice(..8, *b"LUKS\xba\xbe\x00\x02");
    head.splice(168..168 + 36, uuid.bytes());
    assert_eq!(header_id(&head).unwrap().as_str(), uuid);
    let mut luks1 = head.clone();
    luks1.splice(6..8, [0, 1]);
    assert!(header_id(&luks1).is_err(), "a LUKS1 header taken");
    assert!(
        header_id(head.get(..100).unwrap()).is_err(),
        "a header cut short taken"
    );
    let mut foreign = head.clone();
    foreign.splice(168..172, *b"ABCD");
    assert!(header_id(&foreign).is_err(), "a UUID not ours taken");
    assert!(header_id(&[0u8; 512]).is_err(), "no header taken");
}

/// A fresh image's UUID is a version 4 one, new each time, and its mapping
/// is named from it; the mapping's device-mapper UUID is cryptsetup's form.
#[test]
fn a_new_image_has_a_uuid_and_a_mapping_name_of_its_own() {
    let one = new_image_id().unwrap();
    let other = new_image_id().unwrap();
    assert_ne!(one, other, "two images had one UUID");
    let text = one.as_str();
    assert_eq!(text.chars().nth(14), Some('4'), "not version 4");
    assert!(
        text.chars()
            .nth(19)
            .is_some_and(|variant| "89ab".contains(variant)),
        "not the RFC 4122 variant"
    );
    let image: RoomImageId = "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0".parse().unwrap();
    assert_eq!(mapping_name(&image), "hatchling-room-0f1e2d3c4b5a");
    assert_eq!(
        dm_uuid(&image, "hatchling-room-0f1e2d3c4b5a"),
        "CRYPT-LUKS2-0f1e2d3c4b5a49688776a5b4c3d2e1f0-hatchling-room-0f1e2d3c4b5a"
    );
}

/// The programs' argument lists are the decided ones, and nothing else: no
/// discards, PBKDF2 at its floor, the head at 16 MiB.
#[test]
fn the_programs_are_run_with_the_decided_arguments() {
    let image: RoomImageId = "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0".parse().unwrap();
    let strings = |list: Vec<OsString>| -> Vec<String> {
        list.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    };
    assert_eq!(
        strings(format_args(&image, Path::new("/r/disk.img.new"))),
        args(&[
            "luksFormat",
            "--batch-mode",
            "--type",
            "luks2",
            "--cipher",
            "aes-xts-plain64",
            "--key-size",
            "512",
            "--hash",
            "sha256",
            "--pbkdf",
            "pbkdf2",
            "--pbkdf-force-iterations",
            "1000",
            "--offset",
            "32768",
            "--uuid",
            "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0",
            "--label",
            "hatchling-room",
            "--key-file",
            "-",
            "--keyfile-size",
            "64",
            "/r/disk.img.new",
        ])
    );
    assert_eq!(
        strings(open_args(Path::new("/r/disk.img"), "m")),
        args(&[
            "open",
            "--type",
            "luks2",
            "--key-file",
            "-",
            "--keyfile-size",
            "64",
            "/r/disk.img",
            "m"
        ])
    );
    assert_eq!(
        strings(mkfs_args("m")),
        args(&[
            "-t",
            "ext4",
            "-q",
            "-m",
            "0",
            "-E",
            "nodiscard,root_owner=0:0",
            "-L",
            "hatchling-room",
            "/dev/mapper/m"
        ])
    );
    assert_eq!(
        HEADER_BYTES,
        32768 * 512,
        "the wipe isn't the header's size"
    );
}

/// Making a room disk runs exactly its four programs, in order, each with
/// only `PATH` and `LC_ALL`, and the key only on the stdin of the two that
/// take it, never in an argument or the environment. Each ask names the new
/// image, and what for. The image is put in place, private, at its size,
/// with its record; nothing of it is left as new.
#[test]
fn making_a_room_disk_runs_its_programs_with_the_key_on_stdin_alone() {
    let bench = Bench::new("create");
    assert_eq!(bench.run(RoomDiskOp::Create).unwrap(), None);
    let ran = bench.ran();
    let image = bench.image_id();
    let name = mapping_name(&image);
    let new = bench.disk.dir.join("disk.img.new");
    let cryptsetup = bench.disk.cryptsetup.display().to_string();
    let mke2fs = bench.disk.mke2fs.display().to_string();
    let argv: Vec<Vec<String>> = ran.iter().map(|ran| ran.argv.clone()).collect();
    let with = |program: &str, list: Vec<OsString>| -> Vec<String> {
        std::iter::once(program.to_owned())
            .chain(list.into_iter().map(|arg| arg.into_string().unwrap()))
            .collect()
    };
    assert_eq!(
        argv,
        [
            with(&cryptsetup, format_args(&image, &new)),
            with(&cryptsetup, open_args(&new, &name)),
            with(&mke2fs, mkfs_args(&name)),
            with(
                &cryptsetup,
                vec![OsString::from("close"), OsString::from(&name)]
            ),
        ],
        "not the programs a create runs"
    );
    for one in &ran {
        let mut environ = one.environ.clone();
        environ.sort();
        assert_eq!(
            environ,
            ["LC_ALL=C", "PATH=/usr/sbin:/usr/bin"],
            "a program's environment wasn't cleared"
        );
        assert!(
            !one.argv
                .iter()
                .any(|arg| arg.contains("discard") && arg != "nodiscard,root_owner=0:0"),
            "a program was asked to discard"
        );
    }
    let stdin: Vec<bool> = ran.iter().map(|ran| ran.stdin == bench.key).collect();
    assert_eq!(
        stdin,
        [true, true, false, false],
        "the key wasn't on the stdin of the format and the open alone"
    );
    assert!(
        ran.get(2..).unwrap().iter().all(|ran| ran.stdin.is_empty()),
        "a program without the key was given something on stdin"
    );
    let hex = bench.key.iter().fold(String::new(), |mut hex, byte| {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
        hex
    });
    let anywhere = ran.iter().any(|ran| {
        ran.argv.iter().chain(&ran.environ).any(|text| {
            text.contains(&hex)
                || text
                    .as_bytes()
                    .windows(64)
                    .any(|window| window == bench.key)
        })
    });
    assert!(!anywhere, "the key was in an argument or the environment");
    let asked: Vec<(String, RoomKeyFor)> = bench
        .asked
        .lock()
        .unwrap()
        .iter()
        .map(|ask| (ask.image.clone(), ask.room_key))
        .collect();
    assert_eq!(
        asked,
        [
            (image.to_string(), RoomKeyFor::Create),
            (image.to_string(), RoomKeyFor::Open)
        ]
    );
    let meta = std::fs::symlink_metadata(bench.disk.image()).unwrap();
    assert_eq!(meta.len(), MIN_SIZE);
    assert_eq!(meta.mode() & 0o777, 0o600, "the image isn't private");
    assert_eq!(meta.nlink(), 1);
    assert!(!new.exists(), "the new image was left");
    bench.disk.check_record(&image).unwrap();
    assert_eq!(
        std::fs::metadata(bench.disk.dir.join("disk.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600,
        "the record isn't private"
    );
    assert_eq!(
        std::fs::metadata(&bench.disk.dir).unwrap().mode() & 0o777,
        0o700,
        "the room directory isn't private"
    );
}

/// A room disk is never made over one, and what a create cut short left is
/// wiped before another is made. A create that fails part way leaves no
/// image, new or old.
#[test]
fn a_room_disk_is_made_only_where_there_is_none() {
    let bench = Bench::new("create-twice");
    bench.run(RoomDiskOp::Create).unwrap();
    bench.forget_runs();
    let said = bench.run(RoomDiskOp::Create).unwrap_err();
    assert!(
        said.contains("is there already"),
        "made over a room disk: {said}"
    );
    assert!(bench.ran().is_empty(), "a program ran for a refused create");

    let bench = Bench::new("create-leftover");
    bench.disk.ensure_dir().unwrap();
    let new = bench.disk.dir.join("disk.img.new");
    std::fs::write(&new, vec![7u8; 4096]).unwrap();
    mode(&new, 0o600);
    bench.run(RoomDiskOp::Create).unwrap();
    assert!(bench.disk.image().exists() && !new.exists());

    let bench = Bench::new("create-fails");
    bench.switch("fail-open", true);
    let said = bench.run(RoomDiskOp::Create).unwrap_err();
    assert!(said.contains("the stand-in fails open"), "{said}");
    assert!(
        !bench.disk.image().exists() && !new.exists(),
        "a create that failed left an image"
    );
}

/// Opening never formats, and opens without discards: here, where nothing
/// can be mounted, the open is followed by a close, and that's all.
#[test]
fn an_open_never_formats() {
    let bench = Bench::new("open");
    bench.run(RoomDiskOp::Create).unwrap();
    bench.forget_runs();
    let said = bench.run(RoomDiskOp::Open).unwrap_err();
    assert!(said.contains("can't be mounted"), "{said}");
    let image = bench.image_id();
    let name = mapping_name(&image);
    let commands: Vec<Vec<String>> = bench
        .ran()
        .into_iter()
        .map(|ran| ran.argv.into_iter().skip(1).collect())
        .collect();
    let strings = |list: Vec<OsString>| -> Vec<String> {
        list.into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    };
    assert_eq!(
        commands,
        [
            strings(open_args(&bench.disk.image(), &name)),
            args(&["close", &name])
        ],
        "an open ran something more than its open, and the close after"
    );
    assert_eq!(
        bench.ran().first().map(|ran| ran.stdin == bench.key),
        Some(true),
        "the open wasn't given the key"
    );
}

/// Each check stops the op before any program runs: an untrusted room
/// directory or one above it, a program that isn't root's alone, an image
/// that's a link, shared, or has another name, a record for another image,
/// a mapping of the same name that's another's, and a mount point in use.
#[test]
fn each_check_stops_the_op_before_a_program_runs() {
    let refused = |bench: &Bench, op: RoomDiskOp, why: &str| {
        bench.forget_runs();
        let said = bench.run(op).unwrap_err();
        assert!(said.contains(why), "refused for something else: {said}");
        assert!(
            bench.ran().is_empty(),
            "a program ran past a failed check: {why}"
        );
    };
    let made = |name: &str| {
        let bench = Bench::new(name);
        bench.run(RoomDiskOp::Create).unwrap();
        bench
    };

    let bench = made("check-dir");
    mode(&bench.disk.dir, 0o770);
    refused(&bench, RoomDiskOp::Open, "can't be trusted");
    mode(&bench.disk.dir, 0o711);
    refused(&bench, RoomDiskOp::Open, "isn't private");
    mode(&bench.disk.dir, 0o700);
    mode(&bench.root, 0o775);
    refused(&bench, RoomDiskOp::Open, "can't be run");
    refused(&bench, RoomDiskOp::Close, "can't be trusted");
    refused(&bench, RoomDiskOp::Status, "can't be trusted");
    mode(&bench.root, 0o755);

    let bench = made("check-programs");
    mode(&bench.disk.cryptsetup, 0o775);
    refused(&bench, RoomDiskOp::Open, "others can write to it");
    mode(&bench.disk.cryptsetup, 0o755);
    mode(&bench.root.join("tools"), 0o777);
    refused(&bench, RoomDiskOp::Open, "can't be run");
    mode(&bench.root.join("tools"), 0o755);
    let real = bench.root.join("tools/cryptsetup.real");
    std::fs::rename(&bench.disk.cryptsetup, &real).unwrap();
    std::os::unix::fs::symlink(&real, &bench.disk.cryptsetup).unwrap();
    refused(&bench, RoomDiskOp::Open, "isn't a regular file itself");
    std::fs::remove_file(&bench.disk.cryptsetup).unwrap();
    std::fs::rename(&real, &bench.disk.cryptsetup).unwrap();

    let bench = made("check-image");
    let image = bench.disk.image();
    mode(&image, 0o644);
    refused(&bench, RoomDiskOp::Open, "others may read or write it");
    mode(&image, 0o600);
    let other = bench.disk.dir.join("other");
    std::fs::hard_link(&image, &other).unwrap();
    refused(&bench, RoomDiskOp::Open, "has another name");
    std::fs::remove_file(&other).unwrap();
    let elsewhere = bench.disk.dir.join("elsewhere.img");
    std::fs::rename(&image, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &image).unwrap();
    refused(&bench, RoomDiskOp::Open, "isn't a regular file itself");
    refused(&bench, RoomDiskOp::Create, "isn't a regular file itself");
    refused(&bench, RoomDiskOp::Wipe, "isn't a regular file itself");
    std::fs::remove_file(&image).unwrap();
    std::fs::rename(&elsewhere, &image).unwrap();

    let bench = made("check-record");
    let record = bench.disk.dir.join("disk.json");
    let text = std::fs::read_to_string(&record).unwrap();
    let image = bench.image_id();
    let swapped = text.replace(image.as_str(), "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0");
    std::fs::write(&record, swapped).unwrap();
    refused(&bench, RoomDiskOp::Open, "isn't the one its record says");

    let bench = made("check-mapping");
    let name = mapping_name(&bench.image_id());
    bench.list_mapping(&name, &format!("CRYPT-LUKS2-{}-{name}", "0".repeat(32)));
    refused(&bench, RoomDiskOp::Open, "isn't this room disk's");
    refused(&bench, RoomDiskOp::Close, "isn't this room disk's");

    let bench = made("check-mount");
    let mount = bench.disk.mount_point();
    std::fs::create_dir(&mount).unwrap();
    mode(&mount, 0o700);
    std::fs::write(mount.join("left"), b"").unwrap();
    refused(&bench, RoomDiskOp::Open, "isn't empty");
    std::fs::remove_file(mount.join("left")).unwrap();
    mode(&mount, 0o755);
    refused(&bench, RoomDiskOp::Open, "isn't a private directory");
}

/// A program past its deadline is killed, and the op fails saying so,
/// within the deadline and not after the program's own time.
#[test]
fn a_program_past_its_deadline_is_killed() {
    let mut bench = Bench::new("deadline");
    bench.disk.timeout = Duration::from_secs(1);
    bench.switch("hang", true);
    let started = std::time::Instant::now();
    let done = bench.run(RoomDiskOp::Create);
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the deadline didn't hold"
    );
    let said = done.unwrap_err();
    assert!(said.contains("didn't finish"), "{said}");
}

/// A close closes this image's mapping, and confirms it's gone; a mapping
/// still listed after its close fails it. Nothing open, nothing to close.
#[test]
fn a_close_closes_the_mapping_and_confirms_it_gone() {
    let bench = Bench::new("close");
    bench.run(RoomDiskOp::Create).unwrap();
    bench.forget_runs();
    bench.run(RoomDiskOp::Close).unwrap();
    assert!(
        bench.ran().is_empty(),
        "a close ran a program with nothing open"
    );
    let image = bench.image_id();
    let name = mapping_name(&image);
    bench.list_mapping(&name, &dm_uuid(&image, &name));
    bench.switch("keep-mapped", true);
    let said = bench.run(RoomDiskOp::Close).unwrap_err();
    assert!(said.contains("still mapped"), "{said}");
    bench.switch("keep-mapped", false);
    std::fs::remove_dir_all(bench.root.join("sys/dm-3")).unwrap();
    assert_eq!(bench.list_own_mapping(), name);
    bench.forget_runs();
    bench.run(RoomDiskOp::Close).unwrap();
    let commands: Vec<Vec<String>> = bench
        .ran()
        .into_iter()
        .map(|ran| ran.argv.into_iter().skip(1).collect())
        .collect();
    assert_eq!(commands, [args(&["close", &name])]);
    assert!(bench.disk.mapping_uuid(&name).unwrap().is_none());
}

/// The status is numbers alone: none made, then made and closed, at its
/// size, holding little of the disk below.
#[test]
fn the_status_says_how_it_is_in_numbers() {
    let bench = Bench::new("status");
    let absent = bench.run(RoomDiskOp::Status).unwrap().unwrap();
    assert_eq!(absent.state, RoomDiskState::Absent);
    assert_eq!(absent.cap, MIN_SIZE);
    assert_eq!((absent.image_bytes, absent.used), (None, None));
    assert!(absent.host_free.is_some());
    bench.run(RoomDiskOp::Create).unwrap();
    let closed = bench.run(RoomDiskOp::Status).unwrap().unwrap();
    assert_eq!(closed.state, RoomDiskState::Closed);
    assert_eq!(closed.image_bytes, Some(MIN_SIZE));
    assert!(
        closed.on_disk.is_some_and(|bytes| bytes < MIN_SIZE / 2),
        "the image isn't sparse"
    );
    assert_eq!((closed.used, closed.free), (None, None));
    let json = serde_json::to_string(&closed).unwrap();
    assert!(!json.contains('/'), "a status names a path: {json}");
}

/// A wipe closes, then overwrites the head where the key slots are, and
/// removes the image and its record. Nothing there is nothing to wipe.
#[test]
fn a_wipe_overwrites_the_head_and_removes_the_disk() {
    let bench = Bench::new("wipe");
    bench.run(RoomDiskOp::Wipe).unwrap();
    bench.run(RoomDiskOp::Create).unwrap();
    let held = std::fs::File::open(bench.disk.image()).unwrap();
    bench.run(RoomDiskOp::Wipe).unwrap();
    assert!(!bench.disk.image().exists(), "the image is still there");
    assert!(
        !bench.disk.dir.join("disk.json").exists(),
        "the record is still there"
    );
    let mut zeroed = vec![1u8; 512];
    std::os::unix::fs::FileExt::read_exact_at(&held, &mut zeroed, 0).unwrap();
    assert!(
        zeroed.iter().all(|byte| *byte == 0),
        "the header wasn't overwritten"
    );
    assert_eq!(
        bench.run(RoomDiskOp::Status).unwrap().unwrap().state,
        RoomDiskState::Absent
    );
}

/// The room directory must be apart from everything Hatchling keeps: one
/// that is, or holds, or is held by, a kept place is refused, through a
/// link too. A packaged install's places are all apart from the default.
#[test]
fn the_room_directory_is_apart_from_everything_kept() {
    let root = std::env::temp_dir().join(format!("hatchling-room-apart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("state/vault")).unwrap();
    let vault = root.join("state/vault");
    let kept = [vault.clone(), root.join("state/memory")];
    assert!(check_apart(&root.join("state/room"), &kept).is_ok());
    for overlapping in [
        vault.clone(),
        vault.join("room"),
        root.join("state"),
        root.join("state/memory/deeper"),
    ] {
        assert!(
            check_apart(&overlapping, &kept).is_err(),
            "a room directory that overlaps a kept place was taken"
        );
    }
    let link = root.join("link");
    std::os::unix::fs::symlink(&vault, &link).unwrap();
    assert!(
        check_apart(&link.join("room"), &kept).is_err(),
        "a room directory in a kept place, through a link, was taken"
    );
    assert!(
        check_apart(&root.join("state/room"), &[link]).is_ok(),
        "a link to a kept place was taken as holding the room"
    );
    std::fs::remove_dir_all(&root).unwrap();

    // A packaged install, as install.sh writes it.
    let text = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/hatchling.env"),
    )
    .unwrap()
    .replace("@OWNER_UID@", "1000")
    .replace("hatchling-cell", "4242")
    .replace("@MODEL_ENDPOINT@", "http://localhost:11434/v1")
    .replace("@MODEL_NAME@", "model")
    .replace("@MODEL_HOSTPORT@", "localhost:11434");
    let settings = crate::doctor::parse_settings(&text);
    let get = |key: &str| settings.get(key).cloned();
    let config = crate::Config::parse(get).unwrap();
    let redaction = crate::RedactionConfig::parse(&config, get).unwrap();
    let keyring = crate::vault_keyring::KeyringConfig::parse(get).unwrap();
    let apart = kept_apart(&config, Some(&redaction), &keyring);
    assert!(apart.contains(&PathBuf::from("/var/lib/hatchling/vault")));
    assert!(apart.contains(&PathBuf::from("/var/lib/hatchling/sock")));
    assert!(apart.contains(&PathBuf::from("/var/lib/hatchling/memory")));
    for place in &apart {
        assert!(
            check_apart(Path::new(DEFAULT_DIR), std::slice::from_ref(place)).is_ok(),
            "a packaged install's place overlaps the default room directory: {}",
            place.display()
        );
    }
}

/// Each part, by name: where the new image goes; the room directory, a
/// program, an image and the mount point each checked as the ops check
/// them; a program run and its error output read; a new image formatted and
/// put in place; a mapping closed; a wipe; and the close as the broker goes.
#[test]
fn each_part_does_what_it_says() {
    let bench = Bench::new("parts");
    let disk = &bench.disk;
    assert_eq!(disk.new_image(), disk.dir.join("disk.img.new"));
    assert!(
        disk.check_dir().is_err(),
        "a room directory not made yet passed"
    );
    disk.ensure_dir().unwrap();
    disk.check_dir().unwrap();
    disk.trusted_program(&disk.cryptsetup).unwrap();
    assert!(
        disk.trusted_program(&bench.root.join("tools/none"))
            .is_err(),
        "a program that isn't there passed"
    );
    assert!(disk.image_at(&disk.image()).unwrap().is_none());
    assert!(
        !disk.mount_point_mounted().unwrap(),
        "a mount point mounted"
    );
    assert!(disk.mount_point().is_dir(), "the mount point wasn't made");

    disk.run_tool(&disk.mke2fs, &[OsString::from("-V")], None)
        .unwrap();
    let ran = bench.ran();
    assert_eq!(ran.len(), 1);
    assert!(ran.first().unwrap().stdin.is_empty());
    bench.switch("fail--V", true);
    let said = disk
        .run_tool(&disk.mke2fs, &[OsString::from("-V")], None)
        .unwrap_err();
    assert!(
        said.contains("failed with status 3: the stand-in fails -V"),
        "a failure didn't say what the program said: {said}"
    );
    let mut child = Command::new("/bin/sh")
        .args(["-c", "echo said >&2"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let said = read_said(child.stderr.take().unwrap());
    child.wait().unwrap();
    assert_eq!(
        said.recv_timeout(Duration::from_secs(10)).unwrap(),
        "said\n"
    );

    hatchling_sys::make_sparse_file(&disk.new_image(), disk.size).unwrap();
    let image = new_image_id().unwrap();
    let name = mapping_name(&image);
    disk.format(&bench.keeper, &image, &name).unwrap();
    assert!(disk.image_at(&disk.image()).unwrap().is_some());
    assert_eq!(bench.image_id(), image);
    bench.list_own_mapping();
    disk.close_mapping(&name).unwrap();
    assert!(disk.mapping_uuid(&name).unwrap().is_none());

    bench.list_own_mapping();
    bench.forget_runs();
    close_on_leaving(disk);
    assert_eq!(
        bench.ran().len(),
        1,
        "the close as the broker went didn't close"
    );
    disk.wipe().unwrap();
    assert!(disk.image_at(&disk.image()).unwrap().is_none());
}

/// What's missing is told from what can't be looked at: an image, or the
/// kernel's list of block devices, that isn't there is none, and one that
/// can't be reached is refused. A size that's a sign and digits isn't one.
/// A disk with no room below it isn't made. A busy unmount is said as busy,
/// and any other failure as itself. An open disk is told from a closed one
/// by its mount and its mapping, both.
#[test]
fn what_cant_be_looked_at_is_refused_and_what_isnt_there_is_none() {
    let mut bench = Bench::new("looked-at");
    bench.disk.ensure_dir().unwrap();
    let file = bench.disk.dir.join("file");
    std::fs::write(&file, b"").unwrap();
    assert!(
        bench
            .disk
            .image_at(&bench.disk.dir.join("none"))
            .unwrap()
            .is_none()
    );
    assert!(
        bench.disk.image_at(&file.join("disk.img")).is_err(),
        "an image that can't be looked at was taken as none"
    );
    let name = "hatchling-room-000000000000";
    assert_eq!(bench.disk.mapping_uuid(name).unwrap(), None);
    bench.disk.sys_block = bench.root.join("no-sys");
    assert_eq!(
        bench.disk.mapping_uuid(name).unwrap(),
        None,
        "no list of block devices wasn't taken as no mapping"
    );
    bench.disk.sys_block.clone_from(&file);
    assert!(
        bench.disk.mapping_uuid(name).is_err(),
        "a list of block devices that can't be read was taken as none"
    );
    bench.disk.sys_block = bench.root.join("sys");
    assert!(parse_size("+64M").is_err(), "a signed size was taken");

    let mut huge = Bench::new("no-room");
    huge.disk.size = MAX_SIZE;
    let below = hatchling_sys::space(&huge.root).unwrap().available;
    if below < MAX_SIZE {
        let said = huge.run(RoomDiskOp::Create).unwrap_err();
        assert!(
            said.contains("needs"),
            "a disk with no room below it was made: {said}"
        );
        assert!(huge.ran().is_empty());
    }

    let busy = |kind| {
        unmount_refusal(&hatchling_sys::Error::Mount {
            operation: "unmount",
            source: std::io::Error::from(kind),
        })
    };
    assert!(busy(std::io::ErrorKind::ResourceBusy).contains("busy"));
    let other = busy(std::io::ErrorKind::PermissionDenied);
    assert!(
        other.contains("can't be unmounted") && !other.contains("busy"),
        "an unmount that failed otherwise was said as busy: {other}"
    );

    bench.run(RoomDiskOp::Create).unwrap();
    bench.list_own_mapping();
    std::fs::create_dir(bench.disk.mount_point()).unwrap();
    assert_eq!(
        bench.run(RoomDiskOp::Status).unwrap().unwrap().state,
        RoomDiskState::Closed,
        "a disk mapped but not mounted was said to be open"
    );
}
