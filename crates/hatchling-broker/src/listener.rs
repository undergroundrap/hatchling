//! Broker listen-socket lifecycle.
//!
//! Binding is the broker's first trust decision: a stale socket from a
//! previous run must not block startup, but the broker must never unlink
//! a file it does not own. The rule is exact — unlink if and only if the
//! existing path is a socket the broker's user owns — and anything else
//! fails closed. Each part is its own check (the file type, the raw mode
//! bits, the owner), so no one changed operator lets a file through.
//!
//! The socket is never more permissive than intended, not even for an
//! instant: `bind` runs under a `077` umask, so the socket is
//! created owner-only, and only then is the configured mode applied.
//! The order is enforced by type, not by comment: the configured mode
//! can only be set through [`OwnerOnlyListener::with_mode`], and an
//! [`OwnerOnlyListener`] only comes from [`OwnerOnlyListener::bind`].

use std::fs::Metadata;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use hatchling_core::SocketPath;

use crate::{Config, Error};

/// Binds the broker's listen socket.
///
/// Removes a stale socket at the configured path, binds, and applies the
/// configured socket mode. Fails closed if the path exists and is not a
/// socket, or if any step of the setup fails.
///
/// The socket is created owner-only and then given the configured mode
/// ([`OwnerOnlyListener`]), so it never carries the process umask's
/// default permissions, not even between `bind` and the mode change.
/// The peer-authorization gate stays the enforcement point; the mode is
/// a real second layer with no transient hole.
///
/// # Errors
///
/// Returns [`Error::SocketPathNotSocket`] if the path exists and is not a
/// socket, or [`Error::SocketSetup`] if removal, bind, or permission
/// setting fails.
pub fn bind_listener(config: &Config) -> Result<UnixListener, Error> {
    bind_socket(&config.socket, config.socket_mode)
}

/// Binds a broker-owned socket at `socket` with permission bits `mode`.
///
/// The same stale-socket rule as the listen socket applies: unlink only
/// an existing socket, fail closed on anything else. The channel ingress
/// binds through here too.
///
/// # Errors
///
/// As [`bind_listener`].
pub fn bind_socket(socket: &SocketPath, mode: u32) -> Result<UnixListener, Error> {
    bind_socket_as(socket, mode, hatchling_sys::euid())
}

/// Whether `metadata` is a socket's, by its file type.
fn socket_by_type(metadata: &Metadata) -> bool {
    metadata.file_type().is_socket()
}

/// The file-type bits of a raw mode (`S_IFMT`).
const MODE_TYPE: u32 = 0o170_000;

/// A socket's file-type bits (`S_IFSOCK`).
const MODE_SOCKET: u32 = 0o140_000;

/// Whether `metadata` is a socket's, by its raw mode bits: the same fact
/// as [`socket_by_type`], read through other code, so one changed
/// operator can't make a file look like a socket.
fn socket_by_mode(metadata: &Metadata) -> bool {
    metadata.mode() & MODE_TYPE == MODE_SOCKET
}

/// [`bind_socket`], with `owner` the uid a stale socket must have.
fn bind_socket_as(socket: &SocketPath, mode: u32, owner: u32) -> Result<UnixListener, Error> {
    let path = socket.as_path();
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            // Two checks of one fact, each on its own: no one changed
            // operator lets a file through.
            if !socket_by_type(&metadata) {
                return Err(Error::SocketPathNotSocket {
                    path: socket.clone(),
                });
            }
            if !socket_by_mode(&metadata) {
                return Err(Error::SocketPathNotSocket {
                    path: socket.clone(),
                });
            }
            if metadata.uid() != owner {
                return Err(Error::SocketPathNotOurs {
                    path: socket.clone(),
                    owner: metadata.uid(),
                });
            }
            std::fs::remove_file(path).map_err(|source| Error::SocketSetup { source })?;
            tracing::debug!(%socket, "removed stale broker socket");
        }
        Err(source) => {
            // Only "not found" means "no stale socket". Any other probe
            // failure fails closed: the broker must not bind over a path
            // whose state it could not determine.
            if source.kind() == std::io::ErrorKind::NotFound {
                // No stale socket; fall through to bind below.
            } else {
                return Err(Error::SocketSetup { source });
            }
        }
    }
    OwnerOnlyListener::bind(path)?.with_mode(mode)
}

/// The umask `bind` runs under: no group or other bits, ever.
pub const BIND_UMASK: u32 = 0o077;

/// A socket just bound owner-only, not yet given its configured mode.
///
/// The only way from a path to a listener with a configured mode: the
/// bind comes first, under [`BIND_UMASK`], and the mode can only be
/// applied to what it produced.
#[derive(Debug)]
pub struct OwnerOnlyListener {
    listener: UnixListener,
    path: PathBuf,
}

impl OwnerOnlyListener {
    /// Binds `path` with the process umask narrowed to [`BIND_UMASK`]
    /// for the `bind` call alone. The previous umask is restored before
    /// this returns, on success and on failure alike
    /// ([`hatchling_sys::with_umask`]).
    ///
    /// # Errors
    ///
    /// Returns [`Error::SocketSetup`] if the bind fails.
    pub fn bind(path: &Path) -> Result<Self, Error> {
        let listener = hatchling_sys::with_umask(BIND_UMASK, || UnixListener::bind(path))
            .map_err(|source| Error::SocketSetup { source })?;
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
    }

    /// Applies the configured `mode` and hands over the listener.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SocketSetup`] if the mode cannot be set.
    pub fn with_mode(self, mode: u32) -> Result<UnixListener, Error> {
        // Through a handle opened without following symlinks: a symlink
        // swapped in at the path after the bind can't redirect the change.
        hatchling_sys::chmod_socket(&self.path, mode).map_err(|error| Error::SocketSetup {
            source: if let hatchling_sys::Error::Ownership { source, .. } = error {
                source
            } else {
                std::io::Error::other(error.to_string())
            },
        })?;
        Ok(self.listener)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VaultConfig;
    use hatchling_core::{LogLevel, PeerUid};

    fn test_path(name: &str) -> crate::test_socket::TestSocket {
        crate::test_socket::TestSocket::new(format!(
            "/tmp/hatchling-test-lst-{}-{name}.sock",
            std::process::id()
        ))
    }

    fn config_for(socket: &str) -> Config {
        let vault_root =
            std::env::temp_dir().join(format!("hatchling-test-lst-vault-{}", std::process::id()));
        Config {
            socket: socket.parse().unwrap(),
            agent_uid: PeerUid::from_kernel(1000),
            socket_mode: 0o600,
            max_connections: crate::DEFAULT_MAX_CONNECTIONS,
            scheduler: crate::SchedulerConfig::disabled(),
            log_level: LogLevel::Error,
            vault: VaultConfig {
                key_file: VaultConfig::default_key_file(&vault_root),
                root: vault_root,
                grants: Vec::new(),
            },
            egress: crate::EgressConfig {
                allowlist: crate::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: Vec::new(),
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: crate::SkillConfig::disabled(),
            memory: crate::MemoryConfig {
                root: PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: crate::ArtifactConfig::disabled(),
            spaces: crate::SpaceConfig::disabled(),
            web: crate::WebConfig::off(),
            goals: crate::GoalsConfig::disabled(),
        }
    }

    fn mode_of(path: &str) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o777
    }

    #[test]
    fn binds_and_applies_socket_mode() {
        let path = test_path("mode");
        let _ = std::fs::remove_file(&path);
        let config = config_for(&path);

        let _listener = bind_listener(&config).unwrap();
        assert_eq!(mode_of(&path), 0o600);
    }

    #[test]
    fn honors_a_custom_socket_mode() {
        let path = test_path("custom-mode");
        let _ = std::fs::remove_file(&path);
        let mut config = config_for(&path);
        config.socket_mode = 0o640;

        let _listener = bind_listener(&config).unwrap();
        assert_eq!(mode_of(&path), 0o640);
    }

    #[test]
    fn removes_a_stale_socket() {
        let path = test_path("stale");
        let _ = std::fs::remove_file(&path);
        // A leftover socket from a previous run.
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);

        let config = config_for(&path);
        let _listener = bind_listener(&config).unwrap();
        // If the stale socket had not been removed, bind would have
        // failed with AddrInUse.
        assert_eq!(mode_of(&path), 0o600);
    }

    #[test]
    fn refuses_to_unlink_a_non_socket() {
        let path = test_path("not-a-socket");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"someone else's file").unwrap();

        let config = config_for(&path);
        let err = bind_listener(&config).unwrap_err();
        assert!(
            matches!(err, Error::SocketPathNotSocket { .. }),
            "expected SocketPathNotSocket, got {err:?}"
        );
        // The file must be untouched.
        assert_eq!(std::fs::read(&path).unwrap(), b"someone else's file");
    }

    #[test]
    fn a_socket_is_one_by_its_type_and_by_its_mode_alike() {
        let path = test_path("kinds");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let socket = std::fs::symlink_metadata(&*path).unwrap();
        assert!(socket_by_type(&socket) && socket_by_mode(&socket));
        drop(listener);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"a file").unwrap();
        let file = std::fs::symlink_metadata(&*path).unwrap();
        assert!(!socket_by_type(&file) && !socket_by_mode(&file));
        let dir = std::fs::symlink_metadata("/tmp").unwrap();
        assert!(!socket_by_type(&dir) && !socket_by_mode(&dir));
    }

    #[test]
    fn another_users_socket_is_left_in_place() {
        let path = test_path("foreign");
        let _ = std::fs::remove_file(&path);
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        let socket: SocketPath = path.parse().unwrap();
        let owner = std::fs::symlink_metadata(&*path).unwrap().uid();
        let err = bind_socket_as(&socket, 0o600, owner.wrapping_add(1)).unwrap_err();
        assert!(
            matches!(err, Error::SocketPathNotOurs { owner: found, .. } if found == owner),
            "{err:?}"
        );
        assert!(
            std::fs::symlink_metadata(&*path)
                .unwrap()
                .file_type()
                .is_socket(),
            "still there"
        );
        // Its owner's own broker takes it over.
        let _listener = bind_socket_as(&socket, 0o600, owner).unwrap();
    }

    #[test]
    fn non_not_found_metadata_errors_fail_closed() {
        // The parent is a file, not a directory: `symlink_metadata`
        // fails with ENOTDIR, not NotFound. The error must surface as
        // `SocketSetup` — not be swallowed as "no stale socket, bind
        // anyway". Pins the `NotFound` match guard: replacing it with
        // `true` would swallow this error.
        let parent = test_path("parent-file");
        let _ = std::fs::remove_file(&parent);
        std::fs::write(&parent, b"not a directory").unwrap();
        let path = format!("{}/broker.sock", &*parent);

        let config = config_for(&path);
        let err = bind_listener(&config).unwrap_err();
        assert!(
            matches!(err, Error::SocketSetup { .. }),
            "expected SocketSetup, got {err:?}"
        );
    }
}
