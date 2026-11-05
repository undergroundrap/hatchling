//! FIDO2 keys on Linux, through `hidraw`.
//!
//! A FIDO device's HID report descriptor declares usage page `0xF1D0`,
//! usage `0x01` (CTAPHID). Each `hidraw` node's descriptor is in sysfs
//! (`/sys/class/hidraw/<node>/device/report_descriptor`), so keys are found
//! without an `ioctl`, and reports are plain reads and writes on
//! `/dev/<node>`: no `unsafe` here.
//!
//! Opening a node needs permission: distributions grant it to the seated
//! user through a udev rule (libfido2's `70-u2f.rules`). A key that cannot
//! be opened is reported, not skipped.
//!
//! **USB only.** A PIN's hash goes to whatever answers as a key, sealed to
//! that device. Something on Bluetooth that calls itself a security key,
//! within radio range, could take it and guess the PIN at leisure, so
//! nodes on any other bus aren't keys here.
//!
//! **One program at a time.** A key serves one conversation: a node is
//! locked (`flock`, as libfido2 does) while a [`Device`] has it, so two
//! programs don't interleave, and the second is told why it can't.

use std::fs::File;
use std::io::ErrorKind;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, poll};
use rustix::fs::{FlockOperation, flock};

use crate::Bus;
use crate::ctap2::Ctap2Error;
use crate::ctaphid::{Hid, REPORT, TransportError};
use crate::device::{Device, PinPrompt};

/// The CTAPHID usage page.
const FIDO_USAGE_PAGE: u32 = 0xf1d0;
/// The CTAPHID usage on that page.
const CTAPHID_USAGE: u32 = 0x01;
/// How many times a key that's in use is tried.
const LOCK_TRIES: u32 = 20;
/// How long between those tries.
const LOCK_WAIT: Duration = Duration::from_millis(100);

/// Whether a HID report descriptor declares a FIDO (CTAPHID) device.
#[must_use]
pub fn is_fido(descriptor: &[u8]) -> bool {
    let mut page = 0u32;
    let mut rest = descriptor;
    while let Some((&prefix, after)) = rest.split_first() {
        if prefix == 0xfe {
            // A long item: its data size, a tag, then the data.
            let size = usize::from(after.first().copied().unwrap_or(0));
            rest = after.get(2 + size..).unwrap_or(&[]);
            continue;
        }
        let size = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 4,
        };
        let Some((data, after)) = after.split_at_checked(size) else {
            return false;
        };
        let value = data
            .iter()
            .rev()
            .fold(0u32, |value, byte| (value << 8) + u32::from(*byte));
        match prefix & 0xfc {
            // Usage Page (global).
            0x04 => page = value,
            // Usage (local): a 4-byte usage carries its own page.
            0x08 => {
                let (usage_page, usage) = if size == 4 {
                    (value >> 16, value & 0xffff)
                } else {
                    (page, value)
                };
                if usage_page == FIDO_USAGE_PAGE && usage == CTAPHID_USAGE {
                    return true;
                }
            }
            _ => {}
        }
        rest = after;
    }
    false
}

/// Whether a HID device's `uevent` says it's on USB.
fn on_usb(uevent: &str) -> bool {
    uevent.lines().any(|line| line.starts_with("HID_ID=0003:"))
}

/// A node locked for one session: released when this goes, as an open
/// file shared with another process keeps a lock otherwise.
pub struct Locked(File);

impl Locked {
    /// Locks `file`, node `node`, waiting a moment for another program.
    ///
    /// # Errors
    ///
    /// Returns a message if it's still in use, or can't be locked.
    pub fn take(file: File, node: &str) -> Result<Self, String> {
        for _ in 0..LOCK_TRIES {
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(Self(file)),
                Err(rustix::io::Errno::WOULDBLOCK) => std::thread::sleep(LOCK_WAIT),
                Err(error) => return Err(format!("cannot lock {node}: {error}")),
            }
        }
        Err(format!(
            "another program is using the security key ({node}): close it, and try again"
        ))
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        let _ = flock(&self.0, FlockOperation::Unlock);
    }
}

impl std::io::Read for Locked {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(bytes)
    }
}

impl std::io::Write for Locked {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl AsFd for Locked {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

/// One open `hidraw` node, or anything that reads and writes reports
/// like one.
pub struct HidFile<T> {
    file: T,
    name: String,
}

impl<T> HidFile<T> {
    /// Wraps an open node; `name` is for messages.
    pub fn new(file: T, name: &str) -> Self {
        Self {
            file,
            name: name.to_owned(),
        }
    }
}

impl<T: std::io::Read + std::io::Write + AsFd> Hid for HidFile<T> {
    fn send(&mut self, report: &[u8; REPORT]) -> Result<(), TransportError> {
        // Report id 0 ahead: FIDO devices use unnumbered reports.
        let framed = [&[0u8][..], report].concat();
        self.file
            .write_all(&framed)
            .map_err(|error| TransportError::Io(format!("{}: {error}", self.name)))
    }

    fn receive(&mut self, timeout: Duration) -> Result<Option<[u8; REPORT]>, TransportError> {
        // A signal cuts a wait short, not the time the key is given.
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let millis = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
            let mut fds = [PollFd::new(&self.file, PollFlags::IN)];
            match poll(&mut fds, millis) {
                Ok(0) => return Ok(None),
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => {
                    return Err(TransportError::Io(format!("{}: {error}", self.name)));
                }
            }
            let mut report = [0u8; REPORT];
            match self.file.read(&mut report) {
                Ok(REPORT) => return Ok(Some(report)),
                Ok(0) => {
                    return Err(TransportError::Io(format!(
                        "{}: the key was unplugged",
                        self.name
                    )));
                }
                Ok(_) => return Err(TransportError::Protocol("a short HID report")),
                Err(error) if error.kind() == ErrorKind::Interrupted => {
                    if left.is_zero() {
                        return Ok(None);
                    }
                }
                Err(error) => {
                    return Err(TransportError::Io(format!("{}: {error}", self.name)));
                }
            }
        }
    }
}

/// The FIDO keys plugged into this machine, through `hidraw`.
pub struct Hidraw {
    sysfs: PathBuf,
    dev: PathBuf,
    pin: std::rc::Rc<std::cell::RefCell<PinPrompt>>,
    /// The keys opened already, by node: used in place of opening any.
    opened: Option<Vec<(String, File)>>,
}

impl Hidraw {
    /// The keys under the system's `hidraw` class, asking `pin` when one
    /// needs its PIN.
    #[must_use]
    pub fn new(pin: PinPrompt) -> Self {
        Self::at(Path::new("/sys/class/hidraw"), Path::new("/dev"), pin)
    }

    /// The keys described under `sysfs`, opened under `dev`.
    #[must_use]
    pub fn at(sysfs: &Path, dev: &Path, pin: PinPrompt) -> Self {
        Self {
            sysfs: sysfs.to_path_buf(),
            dev: dev.to_path_buf(),
            pin: std::rc::Rc::new(std::cell::RefCell::new(pin)),
            opened: None,
        }
    }

    /// The keys `opened` already, each by its node's name: what a launcher
    /// that may open them hands a process that may not. Only these are
    /// its keys; one plugged in later isn't.
    #[must_use]
    pub fn opened(opened: Vec<(String, File)>, pin: PinPrompt) -> Self {
        Self {
            opened: Some(opened),
            ..Self::at(Path::new(""), Path::new(""), pin)
        }
    }

    /// The names of the FIDO nodes, in name order.
    ///
    /// # Errors
    ///
    /// Returns a message if the class directory cannot be read (anything
    /// but its absence, which is no keys).
    pub fn nodes(&self) -> Result<Vec<String>, String> {
        let entries = match std::fs::read_dir(&self.sysfs) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(format!("cannot list {}: {error}", self.sysfs.display())),
        };
        let mut nodes: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                let device = entry.path().join("device");
                std::fs::read(device.join("report_descriptor"))
                    .is_ok_and(|descriptor| is_fido(&descriptor))
                    && std::fs::read_to_string(device.join("uevent"))
                        .is_ok_and(|uevent| on_usb(&uevent))
            })
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        nodes.sort();
        Ok(nodes)
    }

    /// A PIN prompt that asks through the shared one.
    fn prompt(&self) -> PinPrompt {
        let shared = std::rc::Rc::clone(&self.pin);
        Box::new(move || (shared.borrow_mut())())
    }

    /// Every key's node, open.
    fn files(&self) -> Result<Vec<(String, File)>, String> {
        if let Some(opened) = &self.opened {
            return opened
                .iter()
                .map(|(node, file)| {
                    file.try_clone()
                        .map(|file| (node.clone(), file))
                        .map_err(|error| format!("cannot reuse {node}: {error}"))
                })
                .collect();
        }
        self.nodes()?
            .into_iter()
            .map(|node| {
                let path = self.dev.join(&node);
                let file = File::options()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .map_err(|error| {
                        if error.kind() == ErrorKind::PermissionDenied {
                            format!(
                                "{} is a security key this user may not open: a udev rule must grant it (libfido2's 70-u2f.rules does)",
                                path.display()
                            )
                        } else {
                            format!("cannot open {}: {error}", path.display())
                        }
                    })?;
                Ok((node, file))
            })
            .collect()
    }
}

/// The keys in `answers`, each node's. An older U2F-only key is passed
/// over, so it doesn't stand in the way of a key that works, and said when
/// it's all there is. Any other failure is said: nothing's silently left
/// out.
fn usable<K>(answers: Vec<(String, Result<K, Ctap2Error>)>) -> Result<Vec<K>, String> {
    let mut keys = Vec::new();
    let mut too_old = None;
    for (node, answer) in answers {
        match answer {
            Ok(key) => keys.push(key),
            Err(Ctap2Error::Transport(TransportError::NotCtap2)) => {
                too_old = Some(format!("{node}: {}", TransportError::NotCtap2));
            }
            Err(error) => return Err(format!("{node}: {error}")),
        }
    }
    match too_old {
        Some(said) if keys.is_empty() => Err(said),
        _ => Ok(keys),
    }
}

impl Bus for Hidraw {
    type Key = Device<HidFile<Locked>>;

    fn keys(&mut self) -> Result<Vec<Self::Key>, String> {
        let mut answers = Vec::new();
        for (node, file) in self.files()? {
            let locked = Locked::take(file, &node)?;
            let answer = Device::open(HidFile::new(locked, &node), Some(self.prompt()));
            answers.push((node, answer));
        }
        usable(answers)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;

    /// A `YubiKey` 5's FIDO interface descriptor.
    const FIDO: [u8; 34] = [
        0x06, 0xd0, 0xf1, 0x09, 0x01, 0xa1, 0x01, 0x09, 0x20, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75,
        0x08, 0x95, 0x40, 0x81, 0x02, 0x09, 0x21, 0x15, 0x00, 0x26, 0xff, 0x00, 0x75, 0x08, 0x95,
        0x40, 0x91, 0x02, 0xc0,
    ];
    /// A keyboard's descriptor, opening.
    const KEYBOARD: [u8; 8] = [0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07];

    #[test]
    fn a_fido_descriptor_is_recognized_and_others_are_not() {
        assert!(is_fido(&FIDO));
        assert!(!is_fido(&KEYBOARD));
        assert!(!is_fido(&[]));
        // A 4-byte usage carrying the FIDO page itself.
        assert!(is_fido(&[0x0b, 0x01, 0x00, 0xd0, 0xf1]));
        // The FIDO page with another usage, or usage 1 on another page.
        assert!(!is_fido(&[0x06, 0xd0, 0xf1, 0x09, 0x02]));
        assert!(!is_fido(&[0x06, 0xd0, 0xf0, 0x09, 0x01]));
        assert!(!is_fido(&[0x05, 0x01, 0x09, 0x01]));
        // A long item is skipped whole: its size byte, its tag, its data.
        assert!(is_fido(&[
            0xfe, 0x02, 0x10, 0x09, 0x01, 0x06, 0xd0, 0xf1, 0x09, 0x01
        ]));
        assert!(is_fido(&[
            0xfe, 0x01, 0x10, 0xaa, 0x06, 0xd0, 0xf1, 0x09, 0x01
        ]));
        assert!(is_fido(&[
            0xfe, 0x03, 0x10, 0x01, 0x02, 0x03, 0x06, 0xd0, 0xf1, 0x09, 0x01
        ]));
        // A zero-size item (End Collection) takes no data.
        assert!(is_fido(&[0xc0, 0x06, 0xd0, 0xf1, 0x09, 0x01]));
        // A truncated item ends the scan.
        assert!(!is_fido(&[0x06, 0xd0]));
        assert!(!is_fido(&[0x06, 0xd0, 0xf1, 0x0a, 0x01]));
    }

    fn no_pin() -> PinPrompt {
        Box::new(|| None)
    }

    /// A fake sysfs class directory with the given nodes and descriptors.
    fn sysfs(name: &str, nodes: &[(&str, &[u8])]) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hatchling-fido2-sysfs-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (node, descriptor) in nodes {
            let device = root.join(node).join("device");
            std::fs::create_dir_all(&device).unwrap();
            std::fs::write(device.join("report_descriptor"), descriptor).unwrap();
            std::fs::write(
                device.join("uevent"),
                "DRIVER=hid-generic\nHID_ID=0003:00001050:00000407\nHID_NAME=Yubico YubiKey\n",
            )
            .unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Over Bluetooth, or with no way to tell, a thing that calls itself
    /// a key isn't given a PIN.
    #[test]
    fn only_a_key_on_usb_is_a_key() {
        assert!(on_usb("DRIVER=x\nHID_ID=0003:00001050:00000407\n"));
        assert!(!on_usb("HID_ID=0005:00001050:00000407\n"));
        assert!(!on_usb("HID_NAME=HID_ID=0003:\n"));
        assert!(!on_usb(""));
        let root = sysfs(
            "usb",
            &[("hidraw1", &FIDO), ("hidraw2", &FIDO), ("hidraw3", &FIDO)],
        );
        std::fs::write(
            root.join("hidraw2/device/uevent"),
            "HID_ID=0005:00001050:00000407\n",
        )
        .unwrap();
        std::fs::remove_file(root.join("hidraw3/device/uevent")).unwrap();
        let bus = Hidraw::at(&root, Path::new("/nonexistent"), no_pin());
        assert_eq!(bus.nodes().unwrap(), ["hidraw1"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn scratch_file(name: &str) -> (PathBuf, File) {
        let path = std::env::temp_dir().join(format!(
            "hatchling-fido2-lock-{name}-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"").unwrap();
        let file = File::options().read(true).write(true).open(&path).unwrap();
        (path, file)
    }

    /// A key in use is said to be, and free again once it's let go.
    #[test]
    fn a_key_is_held_for_one_session_at_a_time() {
        let (path, file) = scratch_file("held");
        let locked = Locked::take(file, "hidraw3").unwrap();
        let again = File::open(&path).unwrap();
        let busy = Locked::take(again, "hidraw3").err().unwrap();
        assert!(busy.contains("another program"), "{busy}");
        assert!(busy.contains("hidraw3"), "{busy}");
        // Let go, while another descriptor of the same file stays open, as
        // one handed to another process does.
        let kept = locked.0.try_clone().unwrap();
        drop(locked);
        let free = File::open(&path).unwrap();
        assert!(Locked::take(free, "hidraw3").is_ok());
        drop(kept);
        let _ = std::fs::remove_file(&path);
    }

    /// Another program's moment with a key is waited out.
    #[test]
    fn a_key_let_go_in_a_moment_is_taken() {
        let (path, file) = scratch_file("moment");
        let held = Locked::take(file, "hidraw4").unwrap();
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            drop(held);
        });
        let again = File::open(&path).unwrap();
        assert!(Locked::take(again, "hidraw4").is_ok());
        letting_go.join().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    /// A locked node reads and writes as the node itself does.
    #[test]
    fn a_locked_node_is_still_the_node() {
        use std::io::{Read as _, Write as _};
        let (path, file) = scratch_file("io");
        let mut locked = Locked::take(file, "hidraw5").unwrap();
        assert_eq!(locked.write(b"abc").unwrap(), 3);
        drop(locked);
        assert_eq!(std::fs::read(&path).unwrap(), b"abc");
        let mut locked = Locked::take(File::open(&path).unwrap(), "hidraw5").unwrap();
        let mut bytes = [0u8; 8];
        assert_eq!(locked.read(&mut bytes).unwrap(), 3);
        assert_eq!(bytes.get(..3), Some(&b"abc"[..]));
        let _ = std::fs::remove_file(&path);
    }

    /// A key may take its time: the wait is as long as it was given.
    #[test]
    fn a_slow_key_is_waited_for() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let mut hid = HidFile::new(ours, "pair");
        let answering = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            std::io::Write::write_all(&mut theirs, &[3u8; REPORT]).unwrap();
            theirs
        });
        assert_eq!(
            hid.receive(Duration::from_secs(5)).unwrap(),
            Some([3u8; REPORT])
        );
        drop(answering.join().unwrap());
    }

    /// A node whose first read is interrupted, as by a signal.
    struct Interrupted {
        socket: UnixStream,
        first: bool,
    }

    impl std::io::Read for Interrupted {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            if std::mem::take(&mut self.first) {
                return Err(std::io::Error::from(ErrorKind::Interrupted));
            }
            std::io::Read::read(&mut self.socket, bytes)
        }
    }

    impl std::io::Write for Interrupted {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.socket, bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl AsFd for Interrupted {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.socket.as_fd()
        }
    }

    /// An interrupted read isn't the key's silence: the wait goes on.
    #[test]
    fn a_signal_doesnt_end_the_wait() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        std::io::Write::write_all(&mut theirs, &[6u8; REPORT]).unwrap();
        let node = Interrupted {
            socket: ours,
            first: true,
        };
        let mut hid = HidFile::new(node, "interrupted");
        assert_eq!(
            hid.receive(Duration::from_secs(5)).unwrap(),
            Some([6u8; REPORT])
        );
    }

    /// An old key is passed over for one that works, and said when alone.
    #[test]
    fn an_old_key_doesnt_stand_in_the_way() {
        let old = || Err(Ctap2Error::Transport(TransportError::NotCtap2));
        let keys = usable(vec![
            (String::from("hidraw1"), old()),
            (String::from("hidraw2"), Ok(7)),
        ]);
        assert_eq!(keys, Ok(vec![7]));
        let alone = usable::<u8>(vec![(String::from("hidraw1"), old())]).unwrap_err();
        assert!(
            alone.contains("U2F") && alone.starts_with("hidraw1: "),
            "{alone}"
        );
        assert_eq!(usable::<u8>(Vec::new()), Ok(Vec::new()));
        // Anything else is said, whatever else works.
        let broken = usable(vec![
            (String::from("hidraw1"), Ok(7)),
            (
                String::from("hidraw2"),
                Err(Ctap2Error::Transport(TransportError::Timeout)),
            ),
        ]);
        assert!(broken.unwrap_err().starts_with("hidraw2: "));
    }

    #[test]
    fn only_fido_nodes_are_listed_in_name_order() {
        let root = sysfs(
            "list",
            &[
                ("hidraw3", &FIDO),
                ("hidraw0", &KEYBOARD),
                ("hidraw1", &FIDO),
            ],
        );
        std::fs::create_dir_all(root.join("hidraw9")).unwrap();
        let bus = Hidraw::at(&root, Path::new("/nonexistent"), no_pin());
        assert_eq!(bus.nodes().unwrap(), ["hidraw1", "hidraw3"]);
        let missing = Hidraw::at(&root.join("absent"), Path::new("/dev"), no_pin());
        assert_eq!(missing.nodes().unwrap(), Vec::<String>::new());
        let not_a_dir = Hidraw::at(
            &root.join("hidraw0/device/report_descriptor"),
            Path::new("/dev"),
            no_pin(),
        );
        assert!(not_a_dir.nodes().unwrap_err().starts_with("cannot list "));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_key_that_cannot_be_opened_is_reported_not_skipped() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = sysfs("open", &[("hidraw2", &FIDO)]);
        let dev = root.join("dev");
        std::fs::create_dir_all(&dev).unwrap();
        let mut bus = Hidraw::at(&root, &dev, no_pin());
        let missing = bus.keys().err().unwrap();
        assert!(missing.starts_with("cannot open "), "{missing}");
        std::fs::write(dev.join("hidraw2"), b"").unwrap();
        std::fs::set_permissions(dev.join("hidraw2"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        if File::open(dev.join("hidraw2")).is_ok() {
            // Root opens anything: no permission refusal to see here.
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let denied = bus.keys().err().unwrap();
        assert!(denied.contains("udev rule"), "{denied}");
        // A node that opens but is no key: the handshake fails, named.
        std::fs::set_permissions(dev.join("hidraw2"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let mute = bus.keys().err().unwrap();
        assert!(mute.contains("hidraw2"), "{mute}");
        let _ = std::fs::remove_dir_all(&root);
        // No nodes: no keys.
        let empty = sysfs("none", &[]);
        assert!(
            Hidraw::at(&empty, &dev, no_pin())
                .keys()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&empty);
    }

    /// Keys opened elsewhere are the bus's keys, by their names, and no
    /// node is opened: one that is no key fails its handshake, named.
    #[test]
    fn keys_opened_elsewhere_are_the_ones_used() {
        let root = sysfs("opened", &[("hidraw5", &FIDO)]);
        let mute = root.join("mute");
        std::fs::write(&mute, b"").unwrap();
        let file = File::options().read(true).write(true).open(&mute).unwrap();
        let mut bus = Hidraw::opened(vec![(String::from("hidraw7"), file)], no_pin());
        let failed = bus.keys().err().unwrap();
        assert!(failed.starts_with("hidraw7: "), "{failed}");
        assert!(
            Hidraw::opened(Vec::new(), no_pin())
                .keys()
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_go_out_with_report_id_zero_and_come_back_whole() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        // Neither side can hang the test: a read before poll says ready
        // fails here, and so does a send that writes nothing.
        ours.set_nonblocking(true).unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut hid = HidFile::new(ours, "pair");
        hid.send(&[7u8; REPORT]).unwrap();
        let mut sent = [0u8; REPORT + 1];
        std::io::Read::read_exact(&mut theirs, &mut sent).unwrap();
        assert_eq!(sent.first(), Some(&0));
        assert!(sent.get(1..).unwrap().iter().all(|byte| *byte == 7));
        assert_eq!(hid.receive(Duration::from_millis(20)).unwrap(), None);
        std::io::Write::write_all(&mut theirs, &[9u8; REPORT]).unwrap();
        assert_eq!(
            hid.receive(Duration::from_secs(5)).unwrap(),
            Some([9u8; REPORT])
        );
        std::io::Write::write_all(&mut theirs, &[1u8; 10]).unwrap();
        assert_eq!(
            hid.receive(Duration::from_secs(5)),
            Err(TransportError::Protocol("a short HID report"))
        );
        drop(theirs);
        let gone = hid.receive(Duration::from_secs(5)).unwrap_err();
        assert!(
            matches!(gone, TransportError::Io(ref text) if text.contains("unplugged")),
            "{gone}"
        );
        let gone = hid.send(&[0; REPORT]).unwrap_err();
        assert!(
            matches!(gone, TransportError::Io(ref text) if text.starts_with("pair: ")),
            "{gone}"
        );
    }

    /// A node whose reads fail with `kind`, on a socket that polls as
    /// readable.
    struct Failing {
        socket: UnixStream,
        kind: ErrorKind,
    }

    impl std::io::Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.kind))
        }
    }

    impl std::io::Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.socket, bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl AsFd for Failing {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.socket.as_fd()
        }
    }

    #[test]
    fn only_an_interrupted_read_is_nothing_yet() {
        for (kind, nothing_yet) in [
            (ErrorKind::Interrupted, true),
            (ErrorKind::BrokenPipe, false),
        ] {
            let (ours, mut theirs) = UnixStream::pair().unwrap();
            std::io::Write::write_all(&mut theirs, &[1u8; REPORT]).unwrap();
            let mut hid = HidFile::new(Failing { socket: ours, kind }, "failing");
            let got = hid.receive(Duration::from_millis(50));
            if nothing_yet {
                assert_eq!(got, Ok(None));
            } else {
                assert!(
                    matches!(&got, Err(TransportError::Io(text)) if text.starts_with("failing: ")),
                    "{got:?}"
                );
            }
        }
    }

    #[test]
    fn the_shared_pin_prompt_is_asked_through_each_key() {
        let asked = std::rc::Rc::new(std::cell::Cell::new(0));
        let counter = std::rc::Rc::clone(&asked);
        let bus = Hidraw::new(Box::new(move || {
            counter.set(counter.get() + 1);
            Some(zeroize::Zeroizing::new(String::from("1234")))
        }));
        let (mut one, mut two) = (bus.prompt(), bus.prompt());
        assert_eq!(one().as_deref().map(String::as_str), Some("1234"));
        assert_eq!(two().as_deref().map(String::as_str), Some("1234"));
        assert_eq!(asked.get(), 2);
        assert_eq!(bus.sysfs, Path::new("/sys/class/hidraw"));
        assert_eq!(bus.dev, Path::new("/dev"));
    }
}
