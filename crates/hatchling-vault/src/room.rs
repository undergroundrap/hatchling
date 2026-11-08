//! The room disk's keys (0.9).
//!
//! The agent's room keeps its work on a disk image of its own, encrypted
//! with LUKS2. Its key is never stored: it's derived from the vault's
//! master key, so whoever can open the vault can open the room disk, and
//! nobody else can.
//!
//! - [`RoomRootKey`] is HKDF-SHA-256 of the master key, with an info
//!   string of its own. The vault keeper derives it at its start, while it
//!   still has the master key, and keeps it after the master key is gone.
//! - [`RoomKey`] is HKDF-SHA-256 of that root key, with each image's own
//!   LUKS UUID ([`RoomImageId`]): 64 bytes, what LUKS takes as the image's
//!   passphrase. Two images never share a key.
//!
//! Neither is a secret in any store, so no grant can name one, and nothing
//! lists one. Both follow the master key: rotating the vault's key gives
//! every image a new key, and an old backup's keyring gives the keys of its
//! day. Both are zeroized on drop and never shown in `Debug`.
//!
//! The info strings carry a version: changing either would lose every
//! room disk made under it, and the known-answer tests pin them.

use std::fmt;
use std::str::FromStr;

use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::seal::MasterKey;

/// How many bytes a room disk's key has: what LUKS2 reads as its key file.
pub const ROOM_KEY_BYTES: usize = 64;

/// The HKDF info of the room root key, under the master key.
const ROOT_INFO: &[u8] = b"hatchling room disk root v1";

/// The HKDF info of an image's key, under the room root key, before the
/// image's UUID.
const IMAGE_INFO: &[u8] = b"hatchling room disk image v1:";

/// The room disks' root key: HKDF-SHA-256 of the master key. What the
/// vault keeper keeps for the room disks once the master key is dropped.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RoomRootKey([u8; 32]);

impl RoomRootKey {
    /// The room root key of `master`.
    #[must_use]
    pub fn of(master: &MasterKey) -> Self {
        let mut root = [0_u8; 32];
        // HKDF-SHA-256 expands up to 8160 bytes: 32 always fits.
        let _ = Hkdf::<Sha256>::new(None, master.expose()).expand(ROOT_INFO, &mut root);
        Self(root)
    }

    /// The key of the room disk `image`.
    #[must_use]
    pub fn image_key(&self, image: &RoomImageId) -> RoomKey {
        let mut key = RoomKey([0_u8; ROOM_KEY_BYTES]);
        // 64 bytes always fits, as above. The info is the prefix and the
        // UUID, which is of fixed length: no two images' infos run together.
        let _ = Hkdf::<Sha256>::new(None, &self.0)
            .expand_multi_info(&[IMAGE_INFO, image.0.as_bytes()], &mut key.0);
        key
    }
}

impl fmt::Debug for RoomRootKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoomRootKey([redacted])")
    }
}

/// A room disk's key: what its LUKS2 key slot opens with.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct RoomKey([u8; ROOM_KEY_BYTES]);

impl RoomKey {
    /// The key's bytes, for the pipe they go to cryptsetup through, and
    /// nowhere else.
    #[must_use]
    pub const fn expose(&self) -> &[u8; ROOM_KEY_BYTES] {
        &self.0
    }
}

impl fmt::Debug for RoomKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoomKey([redacted])")
    }
}

/// A room disk image's LUKS UUID, in the one form it's written in.
///
/// That's `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, lowercase hex. Nothing
/// else is one, so a key is derived for exactly the image named, and what
/// it's named with in a log can't be anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomImageId(String);

impl RoomImageId {
    /// The UUID, as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RoomImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a text that isn't a [`RoomImageId`] is refused with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotARoomImageId;

impl fmt::Display for NotARoomImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a room disk's UUID: 8-4-4-4-12 lowercase hex digits")
    }
}

impl std::error::Error for NotARoomImageId {}

impl FromStr for RoomImageId {
    type Err = NotARoomImageId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let groups: Vec<&str> = text.split('-').collect();
        let lengths: Vec<usize> = groups.iter().map(|group| group.len()).collect();
        let hex = groups.iter().all(|group| {
            group
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        });
        if lengths == [8, 4, 4, 4, 12] && hex {
            Ok(Self(text.to_owned()))
        } else {
            Err(NotARoomImageId)
        }
    }
}

#[cfg(test)]
mod tests {
    use hmac::{Hmac, Mac as _};
    use sha2::Digest as _;

    use super::*;

    /// A master key made from the bytes 0 to 31: the known answers' input.
    fn counting_master() -> MasterKey {
        let mut bytes = [0_u8; 32];
        for (byte, value) in bytes.iter_mut().zip(0_u8..) {
            *byte = value;
        }
        MasterKey::from_bytes(bytes)
    }

    fn image(text: &str) -> RoomImageId {
        text.parse().unwrap()
    }

    /// The known answers' image.
    const KNOWN_IMAGE: &str = "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0";

    fn fingerprint(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut hex = String::new();
        for byte in Sha256::digest(bytes) {
            let _ = write!(hex, "{byte:02x}");
        }
        hex
    }

    /// The derivation is pinned: the root key and an image's key for a
    /// known master key and UUID, by their SHA-256, as computed apart from
    /// this code (Python's `hmac`, RFC 5869 by hand). A change to either
    /// info string, or to how they're put together, would leave every room
    /// disk unopenable, and fails here first.
    #[test]
    fn the_derivation_gives_its_known_answers() {
        let root = RoomRootKey::of(&counting_master());
        assert_eq!(
            fingerprint(&root.0),
            "2d78f4d16e66662cba073047ee40831d632a4f44154c15975c57221b64d9aff5",
            "the room root key isn't the one pinned for this master key"
        );
        let key = root.image_key(&image(KNOWN_IMAGE));
        assert_eq!(
            fingerprint(key.expose()),
            "5d2b09ebc6b243e53f14a1c916e6668408450eecd6662970d12540124b7b1185",
            "the image's key isn't the one pinned for this root key and UUID"
        );
    }

    /// HKDF-SHA-256 as RFC 5869 writes it, with no salt: for checking the
    /// derivation against the specification, step by step.
    fn rfc5869(ikm: &[u8], info: &[u8], length: usize) -> Vec<u8> {
        let mut extract = Hmac::<Sha256>::new_from_slice(&[0_u8; 32]).unwrap();
        extract.update(ikm);
        let prk = extract.finalize().into_bytes();
        let mut okm = Vec::new();
        let mut block = Vec::new();
        for counter in 1_u8.. {
            if okm.len() >= length {
                break;
            }
            let mut expand = Hmac::<Sha256>::new_from_slice(&prk).unwrap();
            expand.update(&block);
            expand.update(info);
            expand.update(&[counter]);
            block = expand.finalize().into_bytes().to_vec();
            okm.extend_from_slice(&block);
        }
        okm.truncate(length);
        okm
    }

    /// For a master key of the OS's randomness, both keys are what RFC 5869
    /// gives for the info strings written out here, and not just for the
    /// pinned input.
    #[test]
    fn the_derivation_is_hkdf_sha256_with_the_written_info() {
        let master = MasterKey::generate().unwrap();
        let root = RoomRootKey::of(&master);
        assert!(
            rfc5869(master.expose(), b"hatchling room disk root v1", 32) == root.0,
            "the root key isn't HKDF-SHA-256 of the master key with its info"
        );
        let id = image(KNOWN_IMAGE);
        let mut info = b"hatchling room disk image v1:".to_vec();
        info.extend_from_slice(KNOWN_IMAGE.as_bytes());
        assert!(
            rfc5869(&root.0, &info, ROOM_KEY_BYTES) == root.image_key(&id).expose(),
            "the image's key isn't HKDF-SHA-256 of the root key with its info"
        );
    }

    /// The key changes with the master key, and with the image: a rotated
    /// vault, or another image, never opens with the same key. The same
    /// master key and image always give the same one.
    #[test]
    fn the_key_changes_with_the_master_key_and_the_image() {
        let one = MasterKey::generate().unwrap();
        let other = MasterKey::generate().unwrap();
        let first = image(KNOWN_IMAGE);
        let second = image("0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f1");
        let key = |master: &MasterKey, image: &RoomImageId| {
            RoomRootKey::of(master).image_key(image).expose().to_vec()
        };
        let again = key(&one, &first);
        assert!(
            key(&one, &first) == again,
            "the same master key and image gave two keys"
        );
        assert!(
            key(&one, &first) != key(&other, &first),
            "two master keys gave one image the same key"
        );
        assert!(
            key(&one, &first) != key(&one, &second),
            "two images had the same key under one master key"
        );
        assert!(
            RoomRootKey::of(&one).0 != *one.expose(),
            "the root key is the master key itself"
        );
    }

    /// An image's UUID is the canonical form only: a capital, a missing or
    /// extra group, a wrong length or a stray character is refused, so no
    /// text a log could be misled by is ever named as one.
    #[test]
    fn an_image_is_named_by_its_canonical_uuid_alone() {
        assert_eq!(image(KNOWN_IMAGE).as_str(), KNOWN_IMAGE);
        assert_eq!(image(KNOWN_IMAGE).to_string(), KNOWN_IMAGE);
        assert_eq!(
            NotARoomImageId.to_string(),
            "not a room disk's UUID: 8-4-4-4-12 lowercase hex digits",
            "a refusal doesn't say what an image's UUID is"
        );
        for refused in [
            "",
            "0F1E2D3C-4B5A-4968-8776-A5B4C3D2E1F0",
            "0f1e2d3c4b5a49688776a5b4c3d2e1f0",
            "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f",
            "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f00",
            "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0-00",
            "0f1e2d3g-4b5a-4968-8776-a5b4c3d2e1f0",
            "0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f\n",
            " 0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0",
            "../../../../etc/passwd",
        ] {
            assert_eq!(
                refused.parse::<RoomImageId>(),
                Err(NotARoomImageId),
                "a text that isn't a canonical UUID was taken as one"
            );
        }
    }

    /// Neither key shows its bytes in `Debug`.
    #[test]
    fn neither_key_is_shown() {
        let root = RoomRootKey::of(&MasterKey::generate().unwrap());
        let key = root.image_key(&image(KNOWN_IMAGE));
        assert_eq!(format!("{root:?}"), "RoomRootKey([redacted])");
        assert_eq!(format!("{key:?}"), "RoomKey([redacted])");
    }
}
