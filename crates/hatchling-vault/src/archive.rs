//! One sealed file holding a copy of Hatchling's state: a backup.
//!
//! Much of the state isn't sealed where it lives (memory, goals, schedule
//! records), so the whole file is. A fresh key seals its contents, and that
//! key is sealed under the vault's key, so a copy of the file opens only
//! with what opens the vault: a security key and its PIN, a backup code, or
//! the key file. The keyring of the day travels in the clear beside it, as
//! it's what a restore on a new machine starts from, and it's sealed
//! already.
//!
//! **What it stops:** someone with a copy of the file reading it, changing
//! it, cutting it short, or swapping the keyring beside it: the header and
//! every chunk are authenticated, each chunk says where it goes, and the
//! last says it's the last.
//!
//! The layout: [`MAGIC`], the keyring's length and bytes, the sealed key,
//! then chunks of the entries, each its length, whether it's the last, its
//! nonce, and its ciphertext.

use std::io::{Read, Write};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use crate::error::Error;
use crate::seal::MasterKey;

/// What every backup starts with.
pub const MAGIC: &[u8; 8] = b"HATCHBK1";
/// How much of the entries one chunk holds.
const CHUNK: usize = 1 << 20;
/// `XChaCha20` nonce length.
const NONCE: usize = 24;
/// The sealed key's length: its 32 bytes and a tag.
const SEALED_KEY: usize = 48;
/// The longest keyring taken: far past any real one.
const MAX_KEYRING: usize = 1 << 22;
/// The longest path taken.
const MAX_PATH: usize = 4096;

/// One file in a backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where it goes, relative: `<store>/<path>`.
    pub path: String,
    /// Its permission bits.
    pub mode: u32,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

const fn broken(reason: &'static str) -> Error {
    Error::Backup { reason }
}

fn io(error: &std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        broken("it ends early")
    } else {
        Error::BackupIo {
            reason: error.to_string(),
        }
    }
}

/// The nonce a fresh seal uses.
fn nonce() -> Result<[u8; NONCE], Error> {
    let mut bytes = [0u8; NONCE];
    getrandom::fill(&mut bytes).map_err(|source| Error::Random { source })?;
    Ok(bytes)
}

/// What authenticates chunk `index`.
fn chunk_label(index: u64, last: bool) -> [u8; 17] {
    let mut label = [0u8; 17];
    let (magic, rest) = label.split_at_mut(8);
    magic.copy_from_slice(MAGIC);
    let (number, flag) = rest.split_at_mut(8);
    number.copy_from_slice(&index.to_be_bytes());
    if let Some(flag) = flag.first_mut() {
        *flag = u8::from(last);
    }
    label
}

/// What authenticates the sealed key: the header, keyring included.
fn header_label(keyring: &[u8]) -> Vec<u8> {
    [&MAGIC[..], keyring].concat()
}

/// Writes a backup, entry by entry.
pub struct ArchiveWriter<W: Write> {
    out: W,
    cipher: XChaCha20Poly1305,
    index: u64,
    pending: Zeroizing<Vec<u8>>,
}

impl<W: Write> ArchiveWriter<W> {
    /// Starts a backup on `out`, opened by `key`, with `keyring` beside it.
    ///
    /// # Errors
    ///
    /// [`Error::Random`] with no randomness, or [`Error::BackupIo`].
    pub fn new(mut out: W, key: &MasterKey, keyring: &[u8]) -> Result<Self, Error> {
        let length = u32::try_from(keyring.len()).map_err(|_| broken("the keyring is too big"))?;
        let mut own = Zeroizing::new([0u8; 32]);
        getrandom::fill(own.as_mut_slice()).map_err(|source| Error::Random { source })?;
        let sealed_with = nonce()?;
        let label = header_label(keyring);
        let sealed = key
            .cipher()
            .encrypt(
                &XNonce::from(sealed_with),
                Payload {
                    msg: own.as_slice(),
                    aad: &label,
                },
            )
            .map_err(|_| broken("its key couldn't be sealed"))?;
        for part in [
            &MAGIC[..],
            &length.to_be_bytes(),
            keyring,
            &sealed_with,
            &sealed,
        ] {
            out.write_all(part).map_err(|error| io(&error))?;
        }
        Ok(Self {
            out,
            cipher: XChaCha20Poly1305::new(&(*own).into()),
            index: 0,
            pending: Zeroizing::new(Vec::with_capacity(CHUNK)),
        })
    }

    /// Adds `entry`.
    ///
    /// # Errors
    ///
    /// As [`ArchiveWriter::new`].
    pub fn add(&mut self, entry: &Entry) -> Result<(), Error> {
        let path = u32::try_from(entry.path.len()).map_err(|_| broken("a path is too long"))?;
        self.take(&path.to_be_bytes())?;
        self.take(entry.path.as_bytes())?;
        self.take(&entry.mode.to_be_bytes())?;
        self.take(
            &u64::try_from(entry.bytes.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        )?;
        self.take(&entry.bytes)
    }

    fn take(&mut self, mut bytes: &[u8]) -> Result<(), Error> {
        while !bytes.is_empty() {
            let room = CHUNK.saturating_sub(self.pending.len());
            let (now, later) = bytes.split_at(room.min(bytes.len()));
            self.pending.extend_from_slice(now);
            bytes = later;
            if self.pending.len() == CHUNK {
                self.flush_chunk(false)?;
            }
        }
        Ok(())
    }

    fn flush_chunk(&mut self, last: bool) -> Result<(), Error> {
        let sealed_with = nonce()?;
        let label = chunk_label(self.index, last);
        let sealed = self
            .cipher
            .encrypt(
                &XNonce::from(sealed_with),
                Payload {
                    msg: self.pending.as_slice(),
                    aad: &label,
                },
            )
            .map_err(|_| broken("a chunk couldn't be sealed"))?;
        let length = u32::try_from(sealed.len()).map_err(|_| broken("a chunk is too big"))?;
        for part in [
            &length.to_be_bytes()[..],
            &[u8::from(last)],
            &sealed_with,
            &sealed,
        ] {
            self.out.write_all(part).map_err(|error| io(&error))?;
        }
        self.index += 1;
        self.pending.clear();
        Ok(())
    }

    /// Ends the backup, and hands back `out`.
    ///
    /// # Errors
    ///
    /// As [`ArchiveWriter::new`].
    pub fn finish(mut self) -> Result<W, Error> {
        self.flush_chunk(true)?;
        self.out.flush().map_err(|error| io(&error))?;
        Ok(self.out)
    }
}

/// The keyring beside a backup, read before anything opens it.
///
/// # Errors
///
/// [`Error::Backup`] if this isn't a backup.
pub fn archive_keyring<R: Read>(input: &mut R) -> Result<Vec<u8>, Error> {
    let mut magic = [0u8; 8];
    input.read_exact(&mut magic).map_err(|error| io(&error))?;
    if &magic != MAGIC {
        return Err(broken("this isn't a Hatchling backup"));
    }
    let mut length = [0u8; 4];
    input.read_exact(&mut length).map_err(|error| io(&error))?;
    let length = usize::try_from(u32::from_be_bytes(length)).unwrap_or(usize::MAX);
    if length > MAX_KEYRING {
        return Err(broken("its keyring is too big"));
    }
    let mut keyring = vec![0u8; length];
    input.read_exact(&mut keyring).map_err(|error| io(&error))?;
    Ok(keyring)
}

/// Reads a backup, entry by entry, past [`archive_keyring`].
pub struct ArchiveReader<R: Read> {
    input: R,
    cipher: XChaCha20Poly1305,
    index: u64,
    open: Zeroizing<Vec<u8>>,
    at: usize,
    ended: bool,
}

impl<R: Read> ArchiveReader<R> {
    /// Opens the rest of a backup with `key`, as `keyring` came with it.
    ///
    /// # Errors
    ///
    /// [`Error::Backup`] if `key` doesn't open it, or it was changed.
    pub fn new(mut input: R, key: &MasterKey, keyring: &[u8]) -> Result<Self, Error> {
        let mut sealed_with = [0u8; NONCE];
        let mut sealed = [0u8; SEALED_KEY];
        input
            .read_exact(&mut sealed_with)
            .map_err(|error| io(&error))?;
        input.read_exact(&mut sealed).map_err(|error| io(&error))?;
        let label = header_label(keyring);
        let own = Zeroizing::new(
            key.cipher()
                .decrypt(
                    &XNonce::from(sealed_with),
                    Payload {
                        msg: &sealed,
                        aad: &label,
                    },
                )
                .map_err(|_| broken("this key doesn't open it, or it was changed"))?,
        );
        let own: [u8; 32] = own
            .as_slice()
            .try_into()
            .map_err(|_| broken("its key is the wrong length"))?;
        let own = Zeroizing::new(own);
        Ok(Self {
            input,
            cipher: XChaCha20Poly1305::new(&(*own).into()),
            index: 0,
            open: Zeroizing::new(Vec::new()),
            at: 0,
            ended: false,
        })
    }

    /// The next chunk, opened, or `false` after the last.
    fn next_chunk(&mut self) -> Result<bool, Error> {
        if self.ended {
            return Ok(false);
        }
        let mut length = [0u8; 4];
        self.input
            .read_exact(&mut length)
            .map_err(|error| io(&error))?;
        let length = usize::try_from(u32::from_be_bytes(length)).unwrap_or(usize::MAX);
        if length > CHUNK + 16 {
            return Err(broken("a chunk is too big"));
        }
        let mut flag = [0u8; 1];
        self.input
            .read_exact(&mut flag)
            .map_err(|error| io(&error))?;
        let last = flag == [1];
        let mut sealed_with = [0u8; NONCE];
        self.input
            .read_exact(&mut sealed_with)
            .map_err(|error| io(&error))?;
        let mut sealed = vec![0u8; length];
        self.input
            .read_exact(&mut sealed)
            .map_err(|error| io(&error))?;
        let label = chunk_label(self.index, last);
        let open = self
            .cipher
            .decrypt(
                &XNonce::from(sealed_with),
                Payload {
                    msg: &sealed,
                    aad: &label,
                },
            )
            .map_err(|_| broken("it was changed, or part of it is missing"))?;
        self.open = Zeroizing::new(open);
        self.at = 0;
        self.index += 1;
        self.ended = last;
        if last {
            let mut extra = [0u8; 1];
            let read = self.input.read(&mut extra).map_err(|error| io(&error))?;
            if read != 0 {
                return Err(broken("there's more after its end"));
            }
        }
        Ok(true)
    }

    /// Fills `bytes` from the chunks; `false` if there was nothing left.
    fn fill(&mut self, bytes: &mut [u8]) -> Result<bool, Error> {
        let mut done = 0;
        while done < bytes.len() {
            if self.at == self.open.len() && !self.next_chunk()? {
                return if done == 0 {
                    Ok(false)
                } else {
                    Err(broken("it stops inside a file"))
                };
            }
            let have = self.open.get(self.at..).unwrap_or(&[]);
            let want = bytes.get_mut(done..).unwrap_or(&mut []);
            let count = have.len().min(want.len());
            if let (Some(slot), Some(from)) = (want.get_mut(..count), have.get(..count)) {
                slot.copy_from_slice(from);
            }
            self.at += count;
            done += count;
        }
        Ok(true)
    }

    /// The next entry, or `None` after the last.
    ///
    /// # Errors
    ///
    /// [`Error::Backup`] if it was changed, or a path isn't one to write.
    pub fn next_entry(&mut self) -> Result<Option<Entry>, Error> {
        let mut length = [0u8; 4];
        if !self.fill(&mut length)? {
            return Ok(None);
        }
        let length = usize::try_from(u32::from_be_bytes(length)).unwrap_or(usize::MAX);
        if length == 0 || length > MAX_PATH {
            return Err(broken("a path is the wrong length"));
        }
        let mut path = vec![0u8; length];
        self.must_fill(&mut path)?;
        let path = String::from_utf8(path).map_err(|_| broken("a path isn't text"))?;
        if !safe_path(&path) {
            return Err(broken("a path points outside the backup"));
        }
        let mut mode = [0u8; 4];
        self.must_fill(&mut mode)?;
        let mut size = [0u8; 8];
        self.must_fill(&mut size)?;
        let size =
            usize::try_from(u64::from_be_bytes(size)).map_err(|_| broken("a file is too big"))?;
        let mut bytes = Vec::new();
        let mut left = size;
        // A piece at a time: a size that lies runs out of chunks, rather
        // than asking for the memory.
        while left > 0 {
            let mut piece = vec![0u8; left.min(CHUNK)];
            self.must_fill(&mut piece)?;
            left -= piece.len();
            bytes.extend_from_slice(&piece);
        }
        Ok(Some(Entry {
            path,
            mode: u32::from_be_bytes(mode),
            bytes,
        }))
    }

    fn must_fill(&mut self, bytes: &mut [u8]) -> Result<(), Error> {
        if self.fill(bytes)? {
            Ok(())
        } else {
            Err(broken("it stops inside a file"))
        }
    }
}

/// Whether `path` stays where it's put: relative, with no way up.
#[must_use]
pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> MasterKey {
        MasterKey::generate().unwrap()
    }

    fn entry(path: &str, bytes: &[u8]) -> Entry {
        Entry {
            path: path.to_owned(),
            mode: 0o600,
            bytes: bytes.to_vec(),
        }
    }

    fn written(key: &MasterKey, keyring: &[u8], entries: &[Entry]) -> Vec<u8> {
        let mut writer = ArchiveWriter::new(Vec::new(), key, keyring).unwrap();
        for entry in entries {
            writer.add(entry).unwrap();
        }
        writer.finish().unwrap()
    }

    fn read(key: &MasterKey, bytes: &[u8]) -> Result<(Vec<u8>, Vec<Entry>), Error> {
        let mut input = bytes;
        let keyring = archive_keyring(&mut input)?;
        let mut reader = ArchiveReader::new(input, key, &keyring)?;
        let mut entries = Vec::new();
        while let Some(entry) = reader.next_entry()? {
            entries.push(entry);
        }
        Ok((keyring, entries))
    }

    fn refused(result: Result<(Vec<u8>, Vec<Entry>), Error>) -> &'static str {
        match result {
            Err(Error::Backup { reason }) => reason,
            other => unreachable!("{other:?}"),
        }
    }

    /// What goes in comes out: an empty file, a small one, and one that
    /// spans chunks.
    #[test]
    fn a_backup_gives_back_what_went_in() {
        let key = key();
        let entries = [
            entry("memory/empty", b""),
            entry("vault/token", b"sealed already"),
            Entry {
                path: String::from("artifacts/big"),
                mode: 0o640,
                bytes: vec![7; CHUNK * 2 + 100],
            },
        ];
        let bytes = written(&key, b"the keyring", &entries);
        let (keyring, read) = read(&key, &bytes).unwrap();
        assert_eq!(keyring, b"the keyring");
        assert_eq!(read, entries);
        let none = written(&key, b"", &[]);
        assert!(self::read(&key, &none).unwrap().1.is_empty());
    }

    /// Nothing inside shows, and two backups of the same state differ.
    #[test]
    fn its_contents_dont_show() {
        let key = key();
        let entries = [entry("memory/notes", b"my secret note")];
        let bytes = written(&key, b"", &entries);
        let shows = |needle: &[u8]| bytes.windows(needle.len()).any(|part| part == needle);
        assert!(!shows(b"secret note"));
        assert!(!shows(b"memory/notes"));
        assert_ne!(bytes, written(&key, b"", &entries));
    }

    #[test]
    fn only_its_key_opens_it() {
        let bytes = written(&key(), b"k", &[entry("a", b"b")]);
        assert_eq!(
            refused(read(&key(), &bytes)),
            "this key doesn't open it, or it was changed"
        );
    }

    /// A changed byte, wherever it is, is found.
    #[test]
    fn every_byte_is_protected() {
        let key = key();
        let bytes = written(&key, b"ring", &[entry("a", b"some bytes")]);
        for at in 8..bytes.len() {
            let mut changed = bytes.clone();
            *changed.get_mut(at).unwrap() ^= 0x20;
            assert!(read(&key, &changed).is_err(), "byte {at}");
        }
        let mut changed = bytes;
        *changed.first_mut().unwrap() ^= 0x20;
        assert_eq!(
            refused(read(&key, &changed)),
            "this isn't a Hatchling backup"
        );
    }

    /// Cut short anywhere, at a chunk's edge too, or added to, it's found.
    #[test]
    fn a_backup_cut_short_is_found() {
        let key = key();
        let entries = [Entry {
            path: String::from("a"),
            mode: 0o600,
            bytes: vec![1; CHUNK + 10],
        }];
        let bytes = written(&key, b"", &entries);
        let edge = 8 + 4 + NONCE + SEALED_KEY + 4 + 1 + NONCE + CHUNK + 16;
        for length in [0, 7, 12, edge - 1, edge, edge + 1, bytes.len() - 1] {
            assert!(
                read(&key, bytes.get(..length).unwrap()).is_err(),
                "{length}"
            );
        }
        let longer = [&bytes[..], b"x"].concat();
        assert_eq!(refused(read(&key, &longer)), "there's more after its end");
    }

    /// Chunks can't be swapped: each says where it goes.
    #[test]
    fn chunks_stay_in_order() {
        assert_ne!(chunk_label(0, false), chunk_label(1, false));
        assert_ne!(chunk_label(0, false), chunk_label(0, true));
        let key = key();
        let entries = [Entry {
            path: String::from("a"),
            mode: 0o600,
            bytes: vec![1; CHUNK * 3],
        }];
        let bytes = written(&key, b"", &entries);
        let start = 8 + 4 + NONCE + SEALED_KEY;
        let size = 4 + 1 + NONCE + CHUNK + 16;
        let mut swapped = bytes;
        let (first, rest) = swapped.get_mut(start..).unwrap().split_at_mut(size);
        first.swap_with_slice(rest.get_mut(..size).unwrap());
        assert_eq!(
            refused(read(&key, &swapped)),
            "it was changed, or part of it is missing"
        );
    }

    #[test]
    fn a_path_stays_inside() {
        for good in ["a", "memory/notes", "a/b/c.txt", "..a/b"] {
            assert!(safe_path(good), "{good}");
        }
        for bad in ["", "/etc/passwd", "a/../b", "..", ".", "a//b", "a/", "a\0b"] {
            assert!(!safe_path(bad), "{bad}");
        }
        let key = key();
        let bytes = written(&key, b"", &[entry("../outside", b"x")]);
        assert_eq!(
            refused(read(&key, &bytes)),
            "a path points outside the backup"
        );
    }

    #[test]
    fn a_keyring_too_big_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(refused(read(&key(), &bytes)), "its keyring is too big");
    }
}
