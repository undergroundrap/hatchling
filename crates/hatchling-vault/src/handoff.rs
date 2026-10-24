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
//!
//! The keeper doesn't open the broker's stores: it asks the broker for
//! their keys as they keep them, wrapped ([`ask_bytes`], [`asked`]), and
//! the broker answers ([`kept_bytes`], [`kept`]). The asking is `HKA1`, a
//! count, and each store's label and root, as above; the answer is
//! `HKK1`, a count, and for each store asked a flag (`0`, or `1` followed
//! by its wrapped key).

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::Error;
use crate::seal::{Held, Store, StoreKey, StoreKeys, WRAPPED_STORE_KEY_LEN};

/// The handoff's magic.
const MAGIC: &[u8; 4] = b"HKH1";
/// The asking's magic: the stores a keeper asks their keys of.
const ASK_MAGIC: &[u8; 4] = b"HKA1";
/// The answer's magic: each store's key as it keeps it, wrapped.
const KEPT_MAGIC: &[u8; 4] = b"HKK1";
/// The most stores one handoff carries: enough for every store, and few
/// enough that the longest handoff fits one protocol frame.
pub const MAX_HANDED: usize = 8;
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
            push_store(&mut bytes, held.store, &held.root)?;
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
            let (label, root) = reader.store()?;
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

/// `stores`, each with its root, as bytes: what a keeper asks the owner of
/// those stores their keys of.
///
/// # Errors
///
/// Returns [`Error::HandoffMalformed`] for what [`asked`] would refuse:
/// more than [`MAX_HANDED`] stores, or a root that isn't absolute or is
/// too long.
pub fn ask_bytes(stores: &[(Store, &Path)]) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(ASK_MAGIC);
    bytes.push(count_of(stores.len())?);
    for &(store, root) in stores {
        push_store(&mut bytes, store, root)?;
    }
    Ok(bytes)
}

/// The stores [`ask_bytes`] wrote.
///
/// # Errors
///
/// Returns [`Error::HandoffMalformed`] for anything else, as
/// [`StoreKeys::from_bytes`] refuses its own.
pub fn asked(bytes: &[u8]) -> Result<Vec<(Store, PathBuf)>, Error> {
    let mut reader = Reader(bytes);
    if reader.take(ASK_MAGIC.len())? != ASK_MAGIC {
        return Err(malformed("not a store-key asking"));
    }
    let count = reader.count()?;
    let mut stores = Vec::with_capacity(count);
    for _ in 0..count {
        stores.push(reader.store()?);
    }
    reader.done()?;
    Ok(stores)
}

/// Each store's key as it keeps it, wrapped, in the order asked, as
/// bytes: the owner's answer to [`asked`].
///
/// # Errors
///
/// Returns [`Error::HandoffMalformed`] for more than [`MAX_HANDED`]
/// stores, or a wrapped key of any other length than a wrapped key's.
pub fn kept_bytes(kept: &[Option<Vec<u8>>]) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(KEPT_MAGIC);
    bytes.push(count_of(kept.len())?);
    for wrapped in kept {
        match wrapped {
            None => bytes.push(0),
            Some(wrapped) if wrapped.len() == WRAPPED_STORE_KEY_LEN => {
                bytes.push(1);
                bytes.extend_from_slice(wrapped);
            }
            Some(_) => return Err(malformed("a wrapped key of another length")),
        }
    }
    Ok(bytes)
}

/// The keys [`kept_bytes`] wrote.
///
/// # Errors
///
/// Returns [`Error::HandoffMalformed`] for anything else.
pub fn kept(bytes: &[u8]) -> Result<Vec<Option<Vec<u8>>>, Error> {
    let mut reader = Reader(bytes);
    if reader.take(KEPT_MAGIC.len())? != KEPT_MAGIC {
        return Err(malformed("not the keys a store keeps"));
    }
    let count = reader.count()?;
    let mut kept = Vec::with_capacity(count);
    for _ in 0..count {
        kept.push(match reader.byte()? {
            0 => None,
            1 => Some(reader.take(WRAPPED_STORE_KEY_LEN)?.to_vec()),
            _ => return Err(malformed("a kept flag other than 0 or 1")),
        });
    }
    reader.done()?;
    Ok(kept)
}

/// `len` as a count, when one handoff carries that many stores.
fn count_of(len: usize) -> Result<u8, Error> {
    u8::try_from(len)
        .ok()
        .filter(|&count| usize::from(count) <= MAX_HANDED)
        .ok_or(malformed("too many stores"))
}

/// Writes `store`'s label and `root` to `bytes`.
fn push_store(bytes: &mut Vec<u8>, store: Store, root: &Path) -> Result<(), Error> {
    let label = store.label().as_bytes();
    let root = root_bytes(root)?;
    bytes.push(u8::try_from(label.len()).map_err(|_| malformed("a label too long"))?);
    bytes.extend_from_slice(label);
    bytes.extend_from_slice(
        &u16::try_from(root.len())
            .map_err(|_| malformed("a root too long"))?
            .to_be_bytes(),
    );
    bytes.extend_from_slice(root);
    Ok(())
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
    const fn take(&mut self, len: usize) -> Result<&'b [u8], Error> {
        if self.0.len() < len {
            return Err(malformed("cut short"));
        }
        let (taken, rest) = self.0.split_at(len);
        self.0 = rest;
        Ok(taken)
    }

    /// The next byte.
    fn byte(&mut self) -> Result<u8, Error> {
        self.take(1)
            .map(|taken| taken.first().copied().unwrap_or_default())
    }

    /// A count of stores, at most [`MAX_HANDED`].
    fn count(&mut self) -> Result<usize, Error> {
        let count = usize::from(self.byte()?);
        if count > MAX_HANDED {
            return Err(malformed("too many stores"));
        }
        Ok(count)
    }

    /// A store's label and root, as [`push_store`] wrote them.
    fn store(&mut self) -> Result<(Store, PathBuf), Error> {
        let label_len = usize::from(self.byte()?);
        let store = std::str::from_utf8(self.take(label_len)?)
            .ok()
            .and_then(Store::from_label)
            .ok_or_else(|| malformed("an unknown store"))?;
        let root_len = usize::from(u16::from_be_bytes([self.byte()?, self.byte()?]));
        let root = PathBuf::from(OsStr::from_bytes(self.take(root_len)?));
        root_bytes(&root)?;
        Ok((store, root))
    }

    /// Nothing left over.
    const fn done(&self) -> Result<(), Error> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(malformed("bytes after the last store"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MasterKey, SealedVault, Secret};

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hatchling-handoff-{name}-{}", std::process::id()));
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
            &[
                (Store::SECRETS, &secrets),
                (Store::SPACES, &spaces),
                (Store::ARTIFACTS, &later),
            ],
        )
        .unwrap();
        let bytes = all
            .only(&[Store::SPACES, Store::ARTIFACTS])
            .to_bytes()
            .unwrap();
        let handed = StoreKeys::from_bytes(&bytes).unwrap();
        assert_eq!(
            handed.stores().collect::<Vec<_>>(),
            [
                (Store::SPACES, spaces.as_path()),
                (Store::ARTIFACTS, later.as_path())
            ]
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
        let _ = std::fs::remove_dir_all(&secrets);
        let _ = std::fs::remove_dir_all(&spaces);
        let _ = std::fs::remove_dir_all(&later);
    }

    /// Anything but exactly what was written is refused: every
    /// truncation, a byte more, and each field out of bounds.
    #[test]
    fn a_handoff_is_read_exactly_or_refused() {
        let master = MasterKey::generate().unwrap();
        let root = scratch("exact");
        let later = scratch("exact-later");
        let keys = StoreKeys::unwrap(
            &master,
            &[(Store::SPACES, &root), (Store::SCHEDULES, &later)],
        )
        .unwrap();
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
            Err(Error::HandoffMalformed {
                reason: "a root that isn't absolute"
            })
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
            Err(Error::HandoffMalformed {
                reason: "a root too long"
            })
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
            Err(Error::HandoffMalformed {
                reason: "too many stores"
            })
        ));
        assert_eq!(Store::from_label("spaces"), Some(Store::SPACES));
        assert_eq!(Store::from_label("sites"), Some(Store::SITES));
        assert_eq!(Store::from_label("profiles"), Some(Store::PROFILES));
        assert_eq!(Store::from_label("vault"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The reader's parts, called directly: a root is handed only when
    /// absolute and short enough, a byte is read until none is left, and
    /// each refusal says why.
    #[test]
    fn the_handoff_parts_called_directly() {
        assert_eq!(
            root_bytes(Path::new("/srv/spaces")).unwrap(),
            b"/srv/spaces"
        );
        assert!(matches!(
            root_bytes(Path::new("spaces")),
            Err(Error::HandoffMalformed {
                reason: "a root that isn't absolute"
            })
        ));
        assert!(matches!(
            malformed("cut short"),
            Error::HandoffMalformed {
                reason: "cut short"
            }
        ));
        let mut reader = Reader(&[7, 9]);
        assert_eq!(reader.byte().unwrap(), 7);
        assert_eq!(reader.byte().unwrap(), 9);
        assert!(matches!(
            reader.byte(),
            Err(Error::HandoffMalformed {
                reason: "cut short"
            })
        ));
    }

    /// An asking and its answer read back as written, and anything else is
    /// refused: every truncation, a byte more, another magic, too many
    /// stores, an unknown store, a flag out of bounds; and what would be
    /// refused is never written.
    #[test]
    fn an_asking_and_its_answer_are_read_exactly_or_refused() {
        let (spaces, later) = (
            PathBuf::from("/srv/h/spaces"),
            PathBuf::from("/srv/h/later"),
        );
        let stores = [
            (Store::SPACES, spaces.as_path()),
            (Store::SCHEDULES, later.as_path()),
        ];
        let ask = ask_bytes(&stores).unwrap();
        assert_eq!(
            asked(&ask).unwrap(),
            vec![(Store::SPACES, spaces.clone()), (Store::SCHEDULES, later)]
        );
        let wrapped = vec![7u8; WRAPPED_STORE_KEY_LEN];
        let answer = kept_bytes(&[Some(wrapped.clone()), None]).unwrap();
        assert_eq!(kept(&answer).unwrap(), vec![Some(wrapped), None]);
        let read_ask = |bytes: &[u8]| asked(bytes).map(drop);
        let read_kept = |bytes: &[u8]| kept(bytes).map(drop);
        for len in 0..ask.len() {
            assert!(
                read_ask(ask.get(..len).unwrap()).is_err(),
                "asking cut at {len}"
            );
        }
        for len in 0..answer.len() {
            assert!(
                read_kept(answer.get(..len).unwrap()).is_err(),
                "answer cut at {len}"
            );
        }
        let refused = |result: Result<(), Error>, reason: &str| {
            assert!(
                matches!(result, Err(Error::HandoffMalformed { reason: got }) if got == reason),
                "{reason}"
            );
        };
        let changed = |bytes: &[u8], at: usize, to: u8| {
            let mut bytes = bytes.to_vec();
            if let Some(byte) = bytes.get_mut(at) {
                *byte = to;
            }
            bytes
        };
        let mut longer = ask.clone();
        longer.push(0);
        refused(read_ask(&longer), "bytes after the last store");
        let mut longer = answer.clone();
        longer.push(0);
        refused(read_kept(&longer), "bytes after the last store");
        refused(read_ask(&answer), "not a store-key asking");
        refused(read_kept(&ask), "not the keys a store keeps");
        refused(read_ask(&changed(&ask, 4, 9)), "too many stores");
        refused(read_kept(&changed(&answer, 4, 9)), "too many stores");
        refused(read_ask(&changed(&ask, 6, b'X')), "an unknown store");
        refused(
            read_kept(&changed(&answer, 5, 2)),
            "a kept flag other than 0 or 1",
        );
        refused(
            kept_bytes(&[Some(vec![0; 3])]).map(drop),
            "a wrapped key of another length",
        );
        refused(
            kept_bytes(&vec![None; MAX_HANDED + 1]).map(drop),
            "too many stores",
        );
        refused(
            ask_bytes(&vec![(Store::SPACES, spaces.as_path()); MAX_HANDED + 1]).map(drop),
            "too many stores",
        );
        refused(
            ask_bytes(&[(Store::SPACES, Path::new("relative"))]).map(drop),
            "a root that isn't absolute",
        );
    }

    /// A count is at most [`MAX_HANDED`], and a store is written as its
    /// label's length and label, then its root's length and root.
    #[test]
    fn counts_and_stores_are_written_as_read() {
        assert_eq!(count_of(MAX_HANDED).unwrap(), 8);
        assert_eq!(count_of(0).unwrap(), 0);
        assert!(count_of(MAX_HANDED + 1).is_err());
        let mut bytes = Vec::new();
        push_store(&mut bytes, Store::SPACES, Path::new("/s")).unwrap();
        let mut written = vec![6];
        written.extend_from_slice(b"spaces");
        written.extend_from_slice(&[0, 2]);
        written.extend_from_slice(b"/s");
        assert_eq!(bytes, written);
        assert_eq!(
            Reader(&bytes).store().unwrap(),
            (Store::SPACES, PathBuf::from("/s"))
        );
        assert!(push_store(&mut bytes, Store::SPACES, Path::new("s")).is_err());
    }

    /// A keeper unwraps the keys the stores' owner read of them, and opens
    /// no store: the keys open the stores, a store keeping none is given a
    /// fresh key it keeps on its first open, and a key given as another
    /// store's is refused.
    #[test]
    fn kept_keys_unwrap_without_the_keeper_opening_a_store() {
        let master = MasterKey::generate().unwrap();
        let (spaces, later) = (scratch("kept-spaces"), scratch("kept-later"));
        let name = "token".parse().unwrap();
        SealedVault::open_in(&spaces, &master, Store::SPACES)
            .unwrap()
            .store(&name, &Secret::new(b"s".to_vec()).unwrap())
            .unwrap();
        let read = vec![
            crate::wrapped_store_key(&spaces, Store::SPACES).unwrap(),
            crate::wrapped_store_key(&later, Store::ARTIFACTS).unwrap(),
        ];
        assert!(matches!(read.as_slice(), [Some(_), None]), "{read:?}");
        assert!(!later.exists(), "reading a store not made made it");
        let stores = [
            (Store::SPACES, spaces.as_path()),
            (Store::ARTIFACTS, later.as_path()),
        ];
        let keys = StoreKeys::unwrap_kept(&master, &stores, &read).unwrap();
        assert_eq!(
            SealedVault::open_in(&spaces, &keys, Store::SPACES)
                .unwrap()
                .fetch(&name)
                .unwrap()
                .as_bytes(),
            b"s"
        );
        SealedVault::open_in(&later, &keys, Store::ARTIFACTS)
            .unwrap()
            .store(&name, &Secret::new(b"a".to_vec()).unwrap())
            .unwrap();
        assert_eq!(
            SealedVault::open_in(&later, &master, Store::ARTIFACTS)
                .unwrap()
                .fetch(&name)
                .unwrap()
                .as_bytes(),
            b"a",
            "the fresh key was kept, wrapped under the master key"
        );
        let first = read.get(..1).unwrap();
        assert!(matches!(
            StoreKeys::unwrap_kept(&master, &[(Store::SCHEDULES, spaces.as_path())], first),
            Err(Error::StoreKeyRefused { .. })
        ));
        assert!(matches!(
            StoreKeys::unwrap_kept(&master, &[(Store::SPACES, spaces.as_path())], &read),
            Err(Error::HandoffMalformed { .. })
        ));
        std::fs::remove_file(spaces.join(crate::STORE_KEY_FILE)).unwrap();
        assert!(matches!(
            crate::wrapped_store_key(&spaces, Store::SPACES),
            Err(Error::StoreKeyMissing { .. })
        ));
        let _ = std::fs::remove_dir_all(&spaces);
        let _ = std::fs::remove_dir_all(&later);
    }
}
