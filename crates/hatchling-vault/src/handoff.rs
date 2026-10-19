//! Store keys handed from one process to another (WO-017).
//!
//! The keeper holds the master key and unwraps every store's key; it
//! hands the broker the keys of the broker's own stores and keeps the
//! rest. [`StoreKeys::only`] picks the stores to hand over,
//! [`StoreKeys::to_bytes`] writes them for the channel between the two
//! processes, and [`StoreKeys::from_bytes`] reads them back. The bytes are
//! key material: they are wiped when dropped, and the reader refuses
//! anything but exactly what the writer writes.
//!
//! Layout, every length bounded: `HKH1`; a count (one byte, at most
//! [`MAX_HANDED`]); then for each store its label (one length byte, a
//! known label), its root (two length bytes, big-endian, an absolute path
//! of at most [`MAX_ROOT_BYTES`]), its 32-byte key, and a pending flag
//! (`0`, or `1` followed by the wrapped key a store not made yet keeps).

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::Error;
use crate::seal::{Held, Store, StoreKey, StoreKeys, WRAPPED_STORE_KEY_LEN};

/// The handoff's magic.
const MAGIC: &[u8; 4] = b"HKH1";
/// The most stores one handoff carries.
pub const MAX_HANDED: usize = 16;
/// The longest root a handoff carries, in bytes.
pub const MAX_ROOT_BYTES: usize = 4096;

impl StoreKeys {
    /// The keys of `stores` alone, each with its root, as held here: what
    /// one process hands another. A store not held here is left out.
    #[must_use]
    pub fn only(&self, stores: &[Store]) -> Self {
        Self {
            held: self
                .held
                .iter()
                .filter(|held| stores.contains(&held.store))
                .map(|held| Held {
                    store: held.store,
                    root: held.root.clone(),
                    key: StoreKey(held.key.0),
                    pending: held.pending.clone(),
                })
                .collect(),
        }
    }

    /// These keys as bytes for the process they are handed to, wiped when
    /// dropped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HandoffMalformed`] when there are more than
    /// [`MAX_HANDED`] stores, or a root that isn't absolute or is longer
    /// than [`MAX_ROOT_BYTES`]: what [`from_bytes`](Self::from_bytes)
    /// would refuse is never written.
    pub fn to_bytes(&self) -> Result<Zeroizing<Vec<u8>>, Error> {
        let count = u8::try_from(self.held.len())
            .ok()
            .filter(|&count| usize::from(count) <= MAX_HANDED)
            .ok_or(Error::HandoffMalformed {
                reason: "too many stores",
            })?;
        let mut bytes = Zeroizing::new(Vec::new());
        bytes.extend_from_slice(MAGIC);
        bytes.push(count);
        for held in &self.held {
            let label = held.store.label().as_bytes();
            let root = root_bytes(&held.root)?;
            bytes.push(u8::try_from(label.len()).map_err(|_| Error::HandoffMalformed {
                reason: "a label too long",
            })?);
            bytes.extend_from_slice(label);
            bytes.extend_from_slice(&u16::try_from(root.len()).map_err(|_| Error::HandoffMalformed {
                reason: "a root too long",
            })?.to_be_bytes());
            bytes.extend_from_slice(root);
            bytes.extend_from_slice(&held.key.0);
            match &held.pending {
                None => bytes.push(0),
                Some(wrapped) => {
                    bytes.push(1);
                    bytes.extend_from_slice(wrapped);
                }
            }
        }
        Ok(bytes)
    }

    /// The keys [`to_bytes`](Self::to_bytes) wrote.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HandoffMalformed`] for anything else: another
    /// magic, too many stores, an unknown label, a root that isn't
    /// absolute or is too long, a short key or wrapped key, a flag other
    /// than `0` or `1`, or bytes left over.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let mut reader = Reader(bytes);
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(malformed("not a store-key handoff"));
        }
        let count = usize::from(reader.byte()?);
        if count > MAX_HANDED {
            return Err(malformed("too many stores"));
        }
        let mut held = Vec::with_capacity(count);
        for _ in 0..count {
            let label_len = usize::from(reader.byte()?);
            let label = std::str::from_utf8(reader.take(label_len)?)
                .ok()
                .and_then(Store::from_label)
                .ok_or_else(|| malformed("an unknown store"))?;
            let root_len = usize::from(u16::from_be_bytes([reader.byte()?, reader.byte()?]));
            let root = PathBuf::from(OsStr::from_bytes(reader.take(root_len)?));
            root_bytes(&root)?;
            let mut key = StoreKey([0; 32]);
            key.0.copy_from_slice(reader.take(32)?);
            let pending = match reader.byte()? {
                0 => None,
                1 => Some(reader.take(WRAPPED_STORE_KEY_LEN)?.to_vec()),
                _ => return Err(malformed("a pending flag other than 0 or 1")),
            };
            held.push(Held {
                store: label,
                root,
                key,
                pending,
            });
        }
        if !reader.0.is_empty() {
            return Err(malformed("bytes after the last store"));
        }
        Ok(Self { held })
    }
}

/// `root`'s bytes, when it can be handed: absolute, and at most
/// [`MAX_ROOT_BYTES`].
fn root_bytes(root: &Path) -> Result<&[u8], Error> {
    let bytes = root.as_os_str().as_bytes();
    if !root.is_absolute() {
        return Err(malformed("a root that isn't absolute"));
    }
    if bytes.len() > MAX_ROOT_BYTES {
        return Err(malformed("a root too long"));
    }
    Ok(bytes)
}

const fn malformed(reason: &'static str) -> Error {
    Error::HandoffMalformed { reason }
}

/// The bytes not read yet.
struct Reader<'b>(&'b [u8]);

impl<'b> Reader<'b> {
    /// The next `len` bytes.
    fn take(&mut self, len: usize) -> Result<&'b [u8], Error> {
        if self.0.len() < len {
            return Err(malformed("cut short"));
        }
        let (taken, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(taken)
    }

    /// The next byte.
    fn byte(&mut self) -> Result<u8, Error> {
        self.take(1).map(|taken| taken.first().copied().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MasterKey, SealedVault, Secret};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hatchling-handoff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Handed keys open what the keys they came from open, a store not
    /// made yet keeps its key when first opened, and the store left out
    /// stays out.
    #[test]
    fn handed_keys_open_what_they_were_handed_and_nothing_else() {
        let master = MasterKey::generate().unwrap();
        let (secrets, spaces, later) = (scratch("secrets"), scratch("spaces"), scratch("later"));
        let name = "token".parse().unwrap();
        SealedVault::open_in(&spaces, &master, Store::SPACES)
            .unwrap()
            .store(&name, &Secret::new(b"s".to_vec()).unwrap())
            .unwrap();
        let all = StoreKeys::unwrap(
            &master,
            &[(Store::SECRETS, &secrets), (Store::SPACES, &spaces), (Store::ARTIFACTS, &later)],
        )
        .unwrap();
        let bytes = all.only(&[Store::SPACES, Store::ARTIFACTS]).to_bytes().unwrap();
        let handed = StoreKeys::from_bytes(&bytes).unwrap();
        assert_eq!(
            handed.stores().collect::<Vec<_>>(),
            [(Store::SPACES, spaces.as_path()), (Store::ARTIFACTS, later.as_path())]
        );
        assert_eq!(
            SealedVault::open_in(&spaces, &handed, Store::SPACES)
                .unwrap()
                .fetch(&name)
                .unwrap()
                .as_bytes(),
            b"s"
        );
        SealedVault::open_in(&later, &handed, Store::ARTIFACTS)
            .unwrap()
            .store(&name, &Secret::new(b"a".to_vec()).unwrap())
            .unwrap();
        assert_eq!(
            SealedVault::open_in(&later, &master, Store::ARTIFACTS)
                .unwrap()
                .fetch(&name)
                .unwrap()
                .as_bytes(),
            b"a"
        );
        assert!(matches!(
            SealedVault::open(&secrets, &handed),
            Err(Error::StoreNotHeld { store: "secrets" })
        ));
        for dir in [secrets, spaces, later] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// Anything but exactly what was written is refused: every
    /// truncation, a byte more, and each field out of bounds.
    #[test]
    fn a_handoff_is_read_exactly_or_refused() {
        let master = MasterKey::generate().unwrap();
        let root = scratch("exact");
        let later = scratch("exact-later");
        let keys =
            StoreKeys::unwrap(&master, &[(Store::SPACES, &root), (Store::SCHEDULES, &later)]).unwrap();
        let bytes = keys.to_bytes().unwrap();
        for len in 0..bytes.len() {
            assert!(
                matches!(
                    StoreKeys::from_bytes(bytes.get(..len).unwrap()),
                    Err(Error::HandoffMalformed { .. })
                ),
                "cut at {len}"
            );
        }
        let mut longer = bytes.to_vec();
        longer.push(0);
        let refused = |bytes: &[u8], reason: &str| {
            assert!(
                matches!(StoreKeys::from_bytes(bytes), Err(Error::HandoffMalformed { reason: got }) if got == reason),
                "{reason}"
            );
        };
        refused(&longer, "bytes after the last store");
        let mut magic = bytes.to_vec();
        if let Some(first) = magic.first_mut() {
            *first = b'X';
        }
        refused(&magic, "not a store-key handoff");
        let mut count = bytes.to_vec();
        if let Some(byte) = count.get_mut(4) {
            *byte = 17;
        }
        refused(&count, "too many stores");
        let label = b"spaces";
        let at = 6;
        let mut unknown = bytes.to_vec();
        if let Some(byte) = unknown.get_mut(at) {
            *byte = b'X';
        }
        assert_eq!(bytes.get(at..at + label.len()), Some(label.as_slice()));
        refused(&unknown, "an unknown store");
        let flag_at = at + label.len() + 2 + root.as_os_str().len() + 32;
        let mut flag = bytes.to_vec();
        if let Some(byte) = flag.get_mut(flag_at) {
            *byte = 2;
        }
        refused(&flag, "a pending flag other than 0 or 1");
        let relative = StoreKeys {
            held: vec![Held {
                store: Store::SPACES,
                root: PathBuf::from("relative"),
                key: StoreKey([0; 32]),
                pending: None,
            }],
        };
        assert!(matches!(
            relative.to_bytes(),
            Err(Error::HandoffMalformed { reason: "a root that isn't absolute" })
        ));
        let long = StoreKeys {
            held: vec![Held {
                store: Store::SPACES,
                root: PathBuf::from(format!("/{}", "r".repeat(MAX_ROOT_BYTES))),
                key: StoreKey([0; 32]),
                pending: None,
            }],
        };
        assert!(matches!(
            long.to_bytes(),
            Err(Error::HandoffMalformed { reason: "a root too long" })
        ));
        let many = StoreKeys {
            held: (0..=MAX_HANDED)
                .map(|_| Held {
                    store: Store::SPACES,
                    root: PathBuf::from("/r"),
                    key: StoreKey([0; 32]),
                    pending: None,
                })
                .collect(),
        };
        assert!(matches!(
            many.to_bytes(),
            Err(Error::HandoffMalformed { reason: "too many stores" })
        ));
        assert_eq!(Store::from_label("spaces"), Some(Store::SPACES));
        assert_eq!(Store::from_label("vault"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
