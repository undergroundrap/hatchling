//! Broker listen-socket lifecycle.
//!
//! Binding is the broker's first trust decision: a stale socket from a
//! previous run must not block startup, but the broker must never unlink
//! a file it does not own. The rule is exact — unlink if and only if the
//! existing path is a socket — and anything else fails closed.
//!
//! The socket is never more permissive than intended, not even for an
//! instant: `bind` runs under a `077` umask, so the socket is
//! created owner-only, and only then is the configured mode applied.
//! The order is enforced by type, not by comment: the configured mode
//! can only be set through [`OwnerOnlyListener::with_mode`], and an
//! [`OwnerOnlyListener`] only comes from [`OwnerOnlyListener::bind`].

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
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
    let path = socket.as_path();
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_socket() {
                std::fs::remove_file(path).map_err(|source| Error::SocketSetup { source })?;
                tracing::debug!(%socket, "removed stale broker socket");
            } else {
                return Err(Error::SocketPathNotSocket {
                    path: socket.clone(),
                });
            }
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
        std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(mode))
            .map_err(|source| Error::SocketSetup { source })?;
        Ok(self.listener)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VaultConfig;
    use hatchling_core::{LogLevel, PeerUid};
    use std::os::unix::fs::MetadataExt;

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
