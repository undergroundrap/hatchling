//! Test scratch directories shared by the keyring modules.

use std::path::{Path, PathBuf};

/// A test's scratch directory, created `0700` and removed on drop, so a
/// failing assertion cannot leak it. Unique per test name and process.
pub struct TestDir(PathBuf);

impl std::ops::Deref for TestDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Finishes `rotated` as the broker does, for a test's one store.
///
/// The store at `root` (the secrets store, if it exists) is re-sealed
/// under the new key, then the retiring key dropped. Returns the new
/// backup codes, as text.
///
/// # Panics
///
/// If a step fails: the test's vault is then broken.
pub fn settle(keyring: &mut crate::Keyring, rotated: crate::Rotated, root: &Path) -> Vec<String> {
    assert!(keyring.rotation_pending(), "committed, not yet finished");
    if root.is_dir() {
        crate::rekey(
            root,
            rotated.rotation.old(),
            rotated.rotation.new_key(),
            crate::Store::SECRETS,
        )
        .unwrap();
    }
    keyring.finish_rotation(&rotated.rotation).unwrap();
    assert!(!keyring.rotation_pending());
    rotated
        .codes
        .map(|codes| codes.codes().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// A fresh, existing, owner-only directory for `name`.
///
/// # Panics
///
/// If the directory cannot be made: the test cannot run without it.
#[must_use]
pub fn test_dir(name: &str) -> TestDir {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!(
        "hatchling-keyring-test-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    TestDir(dir)
}
