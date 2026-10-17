//! No copy of a secret is left in memory the vault frees (WO-011).
//!
//! The allocator below zeroes every block it hands out, so a freed block
//! holds only what was written in its own life, never a reused block's
//! stale bytes; copies on every `realloc`, as a growing buffer may; and,
//! on a thread that armed it, scans each block it frees for any
//! [`WINDOW`]-byte stretch of the test's canaries. A secret dropped without
//! being wiped, or a buffer that grew while holding one, leaves such a
//! stretch behind. Each scenario arms it around one vault operation only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use hatchling_vault::{
    Kek, Keyring, MasterKey, SealedVault, Secret, SecretName, Store, Vault, totp_code,
};

/// The shortest stretch of a canary that counts as a copy.
const WINDOW: usize = 16;

/// The canaries' stretches, and which first bytes start one.
struct Canaries {
    windows: Vec<[u8; WINDOW]>,
    starts: [bool; 256],
}

static CANARIES: Mutex<Canaries> = Mutex::new(Canaries {
    windows: Vec::new(),
    starts: [false; 256],
});

/// Whether a freed block held a stretch of a canary.
static FOUND: AtomicBool = AtomicBool::new(false);

/// One scenario at a time: the canaries are the whole binary's.
static SERIAL: Mutex<()> = Mutex::new(());

thread_local! {
    /// Whether this thread's frees are scanned.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// Whether `block` holds a stretch of a canary.
fn holds_a_canary(block: &[u8]) -> bool {
    let Ok(canaries) = CANARIES.try_lock() else {
        return false;
    };
    block.windows(WINDOW).any(|at| {
        at.first()
            .is_some_and(|&first| canaries.starts.get(usize::from(first)) == Some(&true))
            && canaries.windows.iter().any(|window| window.as_slice() == at)
    })
}

/// Scans `size` bytes at `ptr` if this thread is armed.
#[allow(
    unsafe_code,
    reason = "reads a block its owner is freeing, from the test allocator below"
)]
fn scan(ptr: *mut u8, size: usize) {
    if ARMED.try_with(Cell::get).unwrap_or(false) {
        // Disarmed while scanning: the scan itself allocates nothing, but
        // the lock's first use might.
        ARMED.with(|armed| armed.set(false));
        // SAFETY: the caller frees `size` bytes at `ptr`, which it
        // allocated and still owns; nothing else writes them meanwhile.
        let block = unsafe { std::slice::from_raw_parts(ptr, size) };
        if holds_a_canary(block) {
            FOUND.store(true, Ordering::SeqCst);
        }
        ARMED.with(|armed| armed.set(true));
    }
}

/// The system allocator, scanning what armed threads free.
struct Scanning;

#[allow(
    unsafe_code,
    reason = "test-only GlobalAlloc: delegates to System, and reads each block it is handed back before freeing it"
)]
// SAFETY: every call delegates to the system allocator with the caller's
// layout; `realloc` allocates, copies and frees through it, which is a valid
// `realloc`; the scan only reads a block its owner is freeing.
unsafe impl GlobalAlloc for Scanning {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegation with the caller's layout.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        scan(ptr, layout.size());
        // SAFETY: delegation; `ptr` came from this allocator with `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let Ok(grown) = Layout::from_size_align(new_size, layout.align()) else {
            return std::ptr::null_mut();
        };
        // SAFETY: delegation with a valid layout.
        let new = unsafe { System.alloc_zeroed(grown) };
        if !new.is_null() {
            // SAFETY: both blocks are live, distinct, and at least this long.
            unsafe { std::ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size)) };
            // SAFETY: as `dealloc`: the old block is freed, after its scan.
            unsafe { self.dealloc(ptr, layout) };
        }
        new
    }
}

#[global_allocator]
static SCANNING: Scanning = Scanning;

/// Sets the canaries, each at least [`WINDOW`] bytes.
#[allow(
    clippy::unwrap_used,
    reason = "test setup: a poisoned lock or a short canary must fail the test loudly"
)]
fn watch_for(canaries: &[&[u8]]) {
    let mut set = CANARIES.lock().unwrap();
    set.windows.clear();
    set.starts = [false; 256];
    for canary in canaries {
        assert!(canary.len() >= WINDOW, "a canary of {} bytes", canary.len());
        for window in canary.windows(WINDOW) {
            set.windows.push(window.try_into().unwrap());
            if let Some(start) = window
                .first()
                .and_then(|&first| set.starts.get_mut(usize::from(first)))
            {
                *start = true;
            }
        }
    }
    drop(set);
}

/// Runs `operation` armed: true if a block it freed held a canary.
fn leaves_a_copy(operation: impl FnOnce()) -> bool {
    FOUND.store(false, Ordering::SeqCst);
    ARMED.with(|armed| armed.set(true));
    operation();
    ARMED.with(|armed| armed.set(false));
    FOUND.load(Ordering::SeqCst)
}

#[allow(
    clippy::unwrap_used,
    reason = "test setup: a directory that can't be made must fail the test loudly"
)]
fn test_dir(name: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("hatchling-freed-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// Lower-case hex, as the keyring stores a seed.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Base32 back to bytes, as an authenticator app reads a seed.
#[allow(
    clippy::unwrap_used,
    reason = "test helper: text that isn't base32 must fail the test loudly"
)]
fn unbase32(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let (mut out, mut buffer, mut bits) = (Vec::new(), 0u32, 0u32);
    for byte in text.bytes() {
        let value = ALPHABET.iter().position(|&letter| letter == byte).unwrap();
        buffer = (buffer << 5) | u32::try_from(value).unwrap();
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buffer >> bits) & 0xff).unwrap());
        }
    }
    out
}

#[test]
fn the_scanner_finds_a_copy_left_behind_and_not_one_wiped() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    // A control: without it, a scanner that sees nothing passes everything.
    let canary = [0x5a_u8; 64];
    watch_for(&[&canary]);
    assert!(leaves_a_copy(|| drop(canary.to_vec())));
    assert!(leaves_a_copy(|| {
        let mut growing = Vec::with_capacity(1);
        growing.extend_from_slice(&canary);
        growing.extend_from_slice(&canary);
        zeroize::Zeroize::zeroize(&mut growing);
    }));
    assert!(!leaves_a_copy(|| drop(zeroize::Zeroizing::new(canary.to_vec()))));
}

#[test]
fn migrating_and_checking_a_plaintext_store_frees_no_copy_of_it() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = test_dir("migrate");
    let plaintext: Vec<u8> = (0..200u8).map(|at| at.wrapping_mul(37) ^ 0xa5).collect();
    let name: SecretName = "token".parse().unwrap();
    Vault::open(&dir)
        .unwrap()
        .store(&name, &Secret::new(plaintext.clone()).unwrap())
        .unwrap();
    let key = MasterKey::generate().unwrap();
    watch_for(&[&plaintext]);
    assert!(
        !leaves_a_copy(|| {
            assert!(hatchling_vault::check(&dir, Store::SECRETS).is_err());
            assert!(!hatchling_vault::holds_envelopes(&dir).unwrap());
        }),
        "checking a plaintext store"
    );
    assert!(
        !leaves_a_copy(|| {
            hatchling_vault::migrate(&dir, &key, Store::SECRETS).unwrap();
        }),
        "migrating it"
    );
    assert!(
        !leaves_a_copy(|| {
            let fetched = SealedVault::open(&dir, &key).unwrap().fetch(&name).unwrap();
            assert_eq!(fetched.as_bytes(), plaintext.as_slice());
        }),
        "fetching it sealed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_totp_keyring_loaded_unlocked_and_saved_frees_no_copy_of_its_seed() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = test_dir("totp");
    let path = dir.join("keys").join("keyring");
    let dek = MasterKey::generate().unwrap();
    let kek = Kek::from_bytes([7; 32]);
    let mut keyring = Keyring::create(&path, &dek, &kek).unwrap();
    let from_file = keyring.unlock_file(&kek).unwrap();
    let pending = keyring.begin_totp(&from_file).unwrap();
    let base32 = pending
        .uri()
        .split("secret=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_owned();
    let seed = unbase32(&base32);
    let now = 1_700_000_000;
    let rotated = keyring
        .confirm_totp(&from_file, &pending, &totp_code(&seed, now), now)
        .unwrap();
    keyring.finish_rotation(&rotated.rotation).unwrap();
    drop((keyring, pending, rotated));
    let seed_hex = hex(&seed);
    watch_for(&[seed_hex.as_bytes(), base32.as_bytes(), &seed]);
    let later = now + 30;
    let code = totp_code(&seed, later);
    assert!(
        !leaves_a_copy(|| {
            let mut keyring = Keyring::load(&path).unwrap();
            assert!(keyring.unlock_totp(&code, later).is_ok());
        }),
        "loading the keyring, unlocking it and saving the step"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recovering_with_a_backup_code_frees_no_copy_of_it() {
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = test_dir("backup");
    let path = dir.join("keys").join("keyring");
    let dek = MasterKey::generate().unwrap();
    let kek = Kek::from_bytes([7; 32]);
    let mut keyring = Keyring::create(&path, &dek, &kek).unwrap();
    let codes: Vec<String> = keyring
        .generate_backup_codes(&keyring.unlock_file(&kek).unwrap())
        .unwrap()
        .codes()
        .map(str::to_owned)
        .collect();
    drop(keyring);
    let code = codes.first().unwrap();
    let plain = code.replace('-', "");
    watch_for(&[code.as_bytes(), plain.as_bytes()]);
    assert!(
        !leaves_a_copy(|| {
            let mut keyring = Keyring::load(&path).unwrap();
            assert!(keyring.recover(code).is_ok());
        }),
        "recovering with a typed code"
    );
    assert!(!Path::new(&path).with_extension("tmp").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loading_the_key_file_frees_no_copy_of_the_key() {
    use hatchling_vault::{FileKey, KeySource as _};
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = test_dir("key-file");
    let path = dir.join("keys").join("key");
    FileKey::new(path.clone()).load_or_create().unwrap();
    let key = std::fs::read(&path).unwrap();
    watch_for(&[&key]);
    assert!(
        !leaves_a_copy(|| drop(FileKey::new(path.clone()).load().unwrap())),
        "loading the key file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
