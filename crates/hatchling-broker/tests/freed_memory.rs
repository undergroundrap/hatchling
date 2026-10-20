//! No copy of a held secret is left in memory the broker's gates free
//! (WO-011).
//!
//! The allocator below is the vault's (`hatchling-vault/tests/freed_memory.rs`):
//! it zeroes every block it hands out, so a freed block holds only what was
//! written in its own life; copies on every `realloc`, as a growing buffer
//! may; and, on a thread that armed it, scans each block it frees for any
//! [`WINDOW`]-byte stretch of the test's canaries. Each scenario arms it
//! around one operation only.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use hatchling_artifact::SecretSet;
use hatchling_broker::{
    ArtifactConfig, Config, EgressConfig, MemoryConfig, SchedulerConfig, VaultConfig, VaultGrant,
    held_secrets,
};
use hatchling_core::{LogLevel, PeerUid, SecretB64, VaultAction};
use hatchling_vault::{MasterKey, SealedVault, Secret};

/// The shortest stretch of a canary that counts as a copy.
const WINDOW: usize = 16;

/// The held secret: every byte value a base64 or hex form spreads apart.
const SECRET: &[u8; 48] = b"nightjar-7f3a-91c2-0d4e-b6a8-plover-55e1-c3f9-zz";

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
            && canaries
                .windows
                .iter()
                .any(|window| window.as_slice() == at)
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
fn watch_for(canaries: &[Vec<u8>]) {
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

/// [`SECRET`] in every form the gates look for: raw, hex in both cases,
/// and both base64 alphabets at each alignment.
fn forms() -> Vec<Vec<u8>> {
    use std::fmt::Write as _;
    let hex = SECRET.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    });
    let mut forms = vec![
        SECRET.to_vec(),
        hex.to_ascii_uppercase().into_bytes(),
        hex.into_bytes(),
    ];
    for skip in 0..3 {
        let tail = SECRET.get(skip..).unwrap_or_default();
        let aligned = tail.get(..tail.len() / 3 * 3).unwrap_or_default();
        forms.push(STANDARD_NO_PAD.encode(aligned).into_bytes());
        forms.push(URL_SAFE_NO_PAD.encode(aligned).into_bytes());
    }
    forms
}

/// A scratch directory, owner-only, gone when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A config whose vault holds [`SECRET`] under a fetch grant, in `dir`.
#[allow(
    clippy::unwrap_used,
    reason = "test setup: a vault that can't be made must fail the test loudly"
)]
fn holding(name: &str, key: &MasterKey) -> (Scratch, Config) {
    let dir = Scratch(std::env::temp_dir().join(format!(
        "hatchling-broker-freed-{name}-{}",
        std::process::id()
    )));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap();
    let root = dir.0.join("vault");
    SealedVault::open(&root, key)
        .unwrap()
        .store(
            &"app-token".parse().unwrap(),
            &Secret::new(SECRET.to_vec()).unwrap(),
        )
        .unwrap();
    let config = Config {
        socket: "/tmp/hatchling-test-freed-dummy.sock".parse().unwrap(),
        agent_uid: PeerUid::from_kernel(rustix::process::getuid().as_raw()),
        socket_mode: 0o600,
        max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
        scheduler: SchedulerConfig::disabled(),
        log_level: LogLevel::Error,
        vault: VaultConfig {
            key_file: VaultConfig::default_key_file(&root),
            root,
            grants: vec![VaultGrant {
                action: VaultAction::Fetch,
                name: "app-token".parse().unwrap(),
            }],
        },
        egress: EgressConfig {
            allowlist: hatchling_broker::EgressAllowlist::default(),
            grants: Vec::new(),
        },
        channels: vec![String::from("local")],
        channel_accounts: Vec::new(),
        channel_ingress: None,
        skills: hatchling_broker::SkillConfig::disabled(),
        memory: MemoryConfig {
            root: PathBuf::new(),
            grants: Vec::new(),
        },
        artifacts: ArtifactConfig::disabled(),
        goals: hatchling_broker::GoalsConfig::disabled(),
        spaces: hatchling_broker::SpaceConfig::disabled(),
        web: hatchling_broker::WebConfig::off(),
    };
    (dir, config)
}

#[test]
fn the_scanner_finds_a_copy_left_behind_and_not_one_wiped() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A control: without it, a scanner that sees nothing passes everything.
    let forms = forms();
    watch_for(&forms);
    for form in &forms {
        assert!(leaves_a_copy(|| drop(form.clone())));
        assert!(!leaves_a_copy(|| drop(zeroize::Zeroizing::new(
            form.clone()
        ))));
    }
}

#[test]
fn the_gates_secret_set_frees_no_copy_of_a_secret_in_any_form() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let forms = forms();
    watch_for(&forms);
    assert!(
        !leaves_a_copy(|| {
            let set = SecretSet::new([SECRET.to_vec()]);
            assert!(forms.iter().all(|form| set.exposed_in(form)));
        }),
        "building, using and dropping the set"
    );
}

#[test]
#[allow(
    clippy::unwrap_used,
    reason = "a failed read must fail the test loudly"
)]
fn reading_the_held_secrets_frees_no_copy_of_them() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = MasterKey::generate().unwrap();
    let (_dir, config) = holding("held", &key);
    watch_for(&forms());
    assert!(
        !leaves_a_copy(|| {
            let set = held_secrets(&config, &key).unwrap();
            assert!(set.exposed_in(SECRET));
        }),
        "reading the vault for the gate"
    );
}

#[test]
#[allow(
    clippy::unwrap_used,
    reason = "a failed decode must fail the test loudly"
)]
fn a_secret_on_the_wire_frees_no_copy_of_it() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sent = serde_json::to_vec(&SecretB64::encode(SECRET)).unwrap();
    watch_for(&forms());
    assert!(
        !leaves_a_copy(|| {
            let wire = SecretB64::encode(SECRET);
            assert_eq!(*wire.decode().unwrap(), SECRET.as_slice());
            let read: SecretB64 = serde_json::from_slice(&sent).unwrap();
            assert_eq!(read, wire);
        }),
        "encoding, reading and decoding it"
    );
}
