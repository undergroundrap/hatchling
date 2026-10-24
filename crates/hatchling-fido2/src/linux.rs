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

use std::fs::File;
use std::io::ErrorKind;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, poll};

use crate::Bus;
use crate::ctaphid::{Hid, REPORT, TransportError};
use crate::device::{Device, PinPrompt};

/// The CTAPHID usage page.
const FIDO_USAGE_PAGE: u32 = 0xf1d0;
/// The CTAPHID usage on that page.
const CTAPHID_USAGE: u32 = 0x01;

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
        let millis = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let ready = loop {
            let mut fds = [PollFd::new(&self.file, PollFlags::IN)];
            match poll(&mut fds, millis) {
                Ok(ready) => break ready,
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => {
                    return Err(TransportError::Io(format!("{}: {error}", self.name)));
                }
            }
        };
        if ready == 0 {
            return Ok(None);
        }
        let mut report = [0u8; REPORT];
        match self.file.read(&mut report) {
            Ok(REPORT) => Ok(Some(report)),
            Ok(0) => Err(TransportError::Io(format!(
                "{}: the key was unplugged",
                self.name
            ))),
            Ok(_) => Err(TransportError::Protocol("a short HID report")),
            Err(error) if error.kind() == ErrorKind::Interrupted => Ok(None),
            Err(error) => Err(TransportError::Io(format!("{}: {error}", self.name))),
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
                std::fs::read(entry.path().join("device").join("report_descriptor"))
                    .is_ok_and(|descriptor| is_fido(&descriptor))
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
}

impl Bus for Hidraw {
    type Key = Device<HidFile<File>>;

    fn keys(&mut self) -> Result<Vec<Self::Key>, String> {
        if let Some(opened) = &self.opened {
            return opened
                .iter()
                .map(|(node, file)| {
                    let file = file
                        .try_clone()
                        .map_err(|error| format!("cannot reuse {node}: {error}"))?;
                    Device::open(HidFile::new(file, node), Some(self.prompt()))
                        .map_err(|error| format!("{node}: {error}"))
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
                Device::open(HidFile::new(file, &node), Some(self.prompt()))
                    .map_err(|error| format!("{}: {error}", path.display()))
            })
            .collect()
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
        }
        std::fs::create_dir_all(&root).unwrap();
        root
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
        fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
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
            let got = hid.receive(Duration::from_secs(5));
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
