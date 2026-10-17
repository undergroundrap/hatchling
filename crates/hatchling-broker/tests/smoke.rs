//! Smoke: the real broker binary's startup path.
//!
//! Spawning the binary (instead of calling library functions) proves
//! `main` wires configuration, logging, and startup together. As a
//! side effect it forces cargo to build the broker binary during
//! `cargo test` — `env!("CARGO_BIN_EXE_hatchling-broker")` is a build
//! dependency of this test target — so the binary the agent's
//! end-to-end tests spawn is always fresh, never stale.

use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Runs the broker binary to completion, with a watchdog: a broker
/// regression must fail the test, never hang the suite.
fn run_broker(args: &[&str], vars: &[(&str, &str)], unset: &[&str]) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    command
        .args(args)
        .envs(vars.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in unset {
        command.env_remove(key);
    }
    let mut child = command.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child
            .try_wait()
            .map_err(|e| format!("wait failed: {e}"))?
            .is_some()
        {
            return child
                .wait_with_output()
                .map_err(|e| format!("output failed: {e}"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return Err(String::from("broker did not exit within 10s"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Unique scratch dir per test. The broker provisions its key under
/// `<vault>.key/` here, so parallel tests must not share a parent.
fn scratch_dir(name: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join(format!("hatchling-smoke-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Minimal valid broker env for operator subcommands (which never
/// bind the socket). `HATCHLING_AGENT_BIN` is scrubbed: with it set
/// the broker would try to spawn a cell instead of running the
/// subcommand.
fn operator_env(scratch: &std::path::Path, vault: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "HATCHLING_BROKER_SOCKET",
            scratch.join("broker.sock").to_string_lossy().into_owned(),
        ),
        (
            "HATCHLING_AGENT_UID",
            rustix::process::getuid().as_raw().to_string(),
        ),
        ("HATCHLING_LOG_LEVEL", "error".to_owned()),
        (
            "HATCHLING_VAULT_DIR",
            scratch.join(vault).to_string_lossy().into_owned(),
        ),
    ]
}

fn operator_env_refs<'a>(env: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    env.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

#[test]
fn missing_agent_uid_fails_closed() {
    // Configuration is parsed before anything is bound: without
    // HATCHLING_AGENT_UID the broker must exit non-zero, never serve.
    // This pins `main`'s wiring — a `main` that always succeeds, or a
    // `run` that always returns Ok, fails here.
    let output = run_broker(
        &[],
        &[(
            "HATCHLING_BROKER_SOCKET",
            "/tmp/hatchling-smoke-missing-uid.sock",
        )],
        &["HATCHLING_AGENT_UID"],
    )
    .unwrap();

    assert!(
        !output.status.success(),
        "broker started without HATCHLING_AGENT_UID"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("HATCHLING_AGENT_UID"),
        "stderr should name the missing variable, was: {stderr}"
    );
}

/// An unknown subcommand is a usage error: exit non-zero, usage on
/// stderr. Pins `main`'s dispatch — a dispatch that always serves, or
/// always succeeds, fails here.
#[test]
fn unknown_subcommand_is_usage_error() {
    let scratch = scratch_dir("unknown-subcommand").unwrap();
    let env = operator_env(&scratch, "vault");
    let output = run_broker(
        &["frobnicate"],
        &operator_env_refs(&env),
        &["HATCHLING_AGENT_BIN"],
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&scratch);

    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with("Usage: hatchling-broker "),
        "stderr should name the misuse, was: {stderr}"
    );
}

/// `rekey-vault` without a keyring and without the new key file path is a
/// usage error, not a rotation against a default.
#[test]
fn rekey_vault_without_new_key_file_is_usage_error() {
    let scratch = scratch_dir("rekey-no-arg").unwrap();
    let env = operator_env(&scratch, "vault");
    let output = run_broker(
        &["rekey-vault"],
        &operator_env_refs(&env),
        &["HATCHLING_AGENT_BIN"],
    )
    .unwrap();
    let _ = std::fs::remove_dir_all(&scratch);

    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rekey-vault <new-key-file>"),
        "stderr should name the missing argument, was: {stderr}"
    );
}

/// `migrate-vault` encrypts a plaintext store in place: the file that
/// held raw bytes becomes an `HVL2` envelope, and the report says so.
#[test]
fn migrate_vault_seals_plaintext_store() {
    let scratch = scratch_dir("migrate").unwrap();
    let vault_dir = scratch.join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    std::fs::set_permissions(&vault_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    // A plaintext secret, as a pre- store would hold it.
    std::fs::write(vault_dir.join("tok"), b"plaintext-secret").unwrap();

    let env = operator_env(&scratch, "vault");
    let output = run_broker(
        &["migrate-vault"],
        &operator_env_refs(&env),
        &["HATCHLING_AGENT_BIN"],
    )
    .unwrap();

    assert!(
        output.status.success(),
        "migrate-vault failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("migrate-vault: secrets: sealed 1, re-sealed 0 old, skipped 0 file(s)"),
        "missing migration report, stdout was: {stdout}"
    );
    let sealed = std::fs::read(vault_dir.join("tok")).unwrap();
    assert!(
        sealed.starts_with(b"HVL2"),
        "migrated file is not a sealed envelope"
    );
    assert_ne!(
        sealed.as_slice(),
        b"plaintext-secret",
        "plaintext survived migration"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// `vault-keyring` reads its settings from the environment: with
/// `HATCHLING_VAULT_KEYRING` set, `init` adopts a keyring there and
/// `status` reports the vault's protection, through the real binary.
#[test]
fn vault_keyring_takes_its_settings_from_the_environment() {
    let scratch = scratch_dir("vault-keyring").unwrap();
    let vault_dir = scratch.join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    std::fs::set_permissions(&vault_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut env = operator_env(&scratch, "vault");
    let made = run_broker(
        &["migrate-vault"],
        &operator_env_refs(&env),
        &["HATCHLING_AGENT_BIN"],
    )
    .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let keyring = scratch.join("vault.key").join("keyring");
    env.push((
        "HATCHLING_VAULT_KEYRING",
        keyring.to_string_lossy().into_owned(),
    ));
    let run = |action: &str| {
        run_broker(
            &["vault-keyring", action],
            &operator_env_refs(&env),
            &["HATCHLING_AGENT_BIN"],
        )
        .unwrap()
    };
    let init = run("init");
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(keyring.is_file(), "the keyring is where the setting says");
    let status = run("status");
    let _ = std::fs::remove_dir_all(&scratch);
    assert_eq!(
        String::from_utf8_lossy(&status.stdout),
        "vault-keyring: tier file, 0 backup code(s) left\n  ways in: a key file, 0 backup code(s) left, no OS keychain (none in this build)\n"
    );
}

/// `rekey-vault <new-key-file>` re-encrypts under the new key: the old
/// key stops decrypting, the new key works, and the bytes change
/// (fresh nonce per seal).
#[test]
fn rekey_vault_rotates_key() {
    let scratch = scratch_dir("rekey").unwrap();
    let vault_dir = scratch.join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    std::fs::set_permissions(&vault_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(vault_dir.join("tok"), b"plaintext-secret").unwrap();

    let env = operator_env(&scratch, "vault");
    let refs = operator_env_refs(&env);
    let unset = &["HATCHLING_AGENT_BIN"];
    let migrate = run_broker(&["migrate-vault"], &refs, unset).unwrap();
    assert!(migrate.status.success());

    let before = std::fs::read(vault_dir.join("tok")).unwrap();
    // The new key file's parent must be owner-only (0700): FileKey
    // fails closed otherwise, by design.
    let new_key_dir = scratch.join("newkey");
    std::fs::create_dir_all(&new_key_dir).unwrap();
    std::fs::set_permissions(&new_key_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let new_key = new_key_dir.join("key");
    let output = run_broker(&["rekey-vault", new_key.to_str().unwrap()], &refs, unset).unwrap();
    assert!(
        output.status.success(),
        "rekey-vault failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("rekey-vault: secrets: re-sealed 1 under the new key, skipped 0 file(s)"),
        "missing rekey report, stdout was: {stdout}"
    );

    // The bytes changed: re-encryption draws a fresh nonce.
    let after = std::fs::read(vault_dir.join("tok")).unwrap();
    assert_ne!(before, after, "rekey left the envelope bytes untouched");

    // The new key decrypts; the old key no longer does.
    let old_key_file = scratch.join("vault.key").join("key");
    let old_key = read_key_file(&old_key_file).unwrap();
    let new_key_bytes = read_key_file(&new_key).unwrap();
    let name: hatchling_vault::SecretName = "tok".parse().unwrap();
    let old_err = hatchling_vault::SealedVault::open(&vault_dir, &old_key)
        .unwrap()
        .fetch(&name)
        .unwrap_err();
    assert!(
        matches!(old_err, hatchling_vault::Error::DecryptFailed { .. }),
        "old key should fail closed, got: {old_err:?}"
    );
    let secret = hatchling_vault::SealedVault::open(&vault_dir, &new_key_bytes)
        .unwrap()
        .fetch(&name)
        .unwrap();
    assert_eq!(secret.as_bytes(), b"plaintext-secret");
    let _ = std::fs::remove_dir_all(&scratch);
}

/// `rekey-vault` with a keyring configured refuses by name and writes no
/// key file: a plain key the keyring never learns of would leave the
/// next unlock opening nothing.
#[test]
fn rekey_vault_refuses_a_keyring_vault_and_writes_no_key() {
    let scratch = scratch_dir("rekey-keyring").unwrap();
    let mut env = operator_env(&scratch, "vault");
    let keyring = scratch.join("vault.key").join("keyring");
    env.push((
        "HATCHLING_VAULT_KEYRING",
        keyring.to_string_lossy().into_owned(),
    ));
    let new_key = scratch.join("new.key");
    let output = run_broker(
        &["rekey-vault", new_key.to_str().unwrap()],
        &operator_env_refs(&env),
        &["HATCHLING_AGENT_BIN"],
    )
    .unwrap();
    let written = new_key.exists();
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        !output.status.success(),
        "rekey-vault ran on a keyring vault"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("HATCHLING_VAULT_KEYRING") && stderr.contains(&*keyring.to_string_lossy()),
        "{stderr}"
    );
    assert!(!written, "a plain key file was written");
}

/// `rekey-vault` on a keyring vault rotates inside the keyring, through
/// the binary: every store is re-sealed, no key file is written, and the
/// keyring alone opens the vault after.
#[test]
fn rekey_vault_rotates_a_keyring_vault_inside_its_keyring() {
    let scratch = scratch_dir("rekey-in-keyring").unwrap();
    let vault_dir = scratch.join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    std::fs::set_permissions(&vault_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(vault_dir.join("tok"), b"plaintext-secret").unwrap();
    std::fs::set_permissions(
        vault_dir.join("tok"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut env = operator_env(&scratch, "vault");
    let unset = &["HATCHLING_AGENT_BIN"];
    let made = run_broker(&["migrate-vault"], &operator_env_refs(&env), unset).unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    env.push((
        "HATCHLING_VAULT_KEYRING",
        scratch
            .join("vault.key")
            .join("keyring")
            .to_string_lossy()
            .into_owned(),
    ));
    let adopted = run_broker(&["vault-keyring", "init"], &operator_env_refs(&env), unset).unwrap();
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let before = std::fs::read(vault_dir.join("tok")).unwrap();
    let rekeyed = run_broker(&["rekey-vault"], &operator_env_refs(&env), unset).unwrap();
    let after = std::fs::read(vault_dir.join("tok")).unwrap();
    let minted = scratch.join("vault.key").join("key").exists();
    // migrate-vault opens every current envelope to verify it: under the
    // keyring's key, the re-sealed file opens.
    let verified = run_broker(&["migrate-vault"], &operator_env_refs(&env), unset).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        rekeyed.status.success(),
        "{}",
        String::from_utf8_lossy(&rekeyed.stderr)
    );
    let stdout = String::from_utf8_lossy(&rekeyed.stdout);
    assert!(
        stdout.contains("rekey-vault: secrets: re-sealed 1 under the new key"),
        "{stdout}"
    );
    assert_ne!(before, after, "re-sealed under the new key");
    assert!(!minted, "no plain key file");
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert!(
        String::from_utf8_lossy(&verified.stdout)
            .contains("secrets: sealed 0, re-sealed 0 old, skipped 1"),
        "the keyring's key opens the re-sealed file"
    );
}

/// A vault adopted into a keyring, started without
/// `HATCHLING_VAULT_KEYRING`, refuses to serve instead of minting a
/// fresh plain key: the socket never appears and no key file is made.
#[test]
fn a_keyring_vault_started_without_its_keyring_refuses_to_serve() {
    let scratch = scratch_dir("keyring-unset").unwrap();
    let vault_dir = scratch.join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    std::fs::set_permissions(&vault_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut env = operator_env(&scratch, "vault");
    let unset = &["HATCHLING_AGENT_BIN"];
    let made = run_broker(&["migrate-vault"], &operator_env_refs(&env), unset).unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    env.push((
        "HATCHLING_VAULT_KEYRING",
        scratch
            .join("vault.key")
            .join("keyring")
            .to_string_lossy()
            .into_owned(),
    ));
    let adopted = run_broker(&["vault-keyring", "init"], &operator_env_refs(&env), unset).unwrap();
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    env.pop();
    let key_file = scratch.join("vault.key").join("key");
    assert!(!key_file.exists());
    let output = run_broker(
        &[],
        &operator_env_refs(&env),
        &["HATCHLING_AGENT_BIN", "HATCHLING_VAULT_KEYRING"],
    );
    let (minted, bound) = (key_file.exists(), scratch.join("broker.sock").exists());
    let _ = std::fs::remove_dir_all(&scratch);
    let output = output.expect("the broker must stop at start-up, not serve");
    assert!(!output.status.success(), "the broker started");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("HATCHLING_VAULT_KEYRING"), "{stderr}");
    assert!(!minted, "a fresh plain key was minted");
    assert!(!bound, "the broker bound its socket");
}

/// Reads a 32-byte key file the broker provisioned.
fn read_key_file(path: &std::path::Path) -> std::io::Result<hatchling_vault::MasterKey> {
    let bytes = std::fs::read(path)?;
    if bytes.len() != 32 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "key file is not 32 bytes",
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(hatchling_vault::MasterKey::from_bytes(arr))
}

/// A misconfigured vault stops the broker before it binds: the socket
/// never appears and the exit is non-zero. Pins the fail-fast
/// `validate_sealed_vault` in the serve path — without it the broker
/// would bind and only fail on the first agent connection.
/// The local API's settings come from the environment (, pinned in
/// ): asking for a network listener without the opt-in stops the
/// broker at start-up, with the sentence that says why. A broker that
/// ignored the variable would serve instead, and be killed at the deadline.
#[test]
fn the_api_takes_its_settings_from_the_environment() {
    let scratch = scratch_dir("api-env").unwrap();
    let mut env = operator_env(&scratch, "vault");
    env.push(("HATCHLING_API_NETWORK", String::from("127.0.0.1:9")));
    let output = run_broker(
        &[],
        &operator_env_refs(&env),
        &["HATCHLING_AGENT_BIN", "HATCHLING_API_ALLOW_NETWORK"],
    );
    let _ = std::fs::remove_dir_all(&scratch);
    let output = output.expect("the broker must stop at start-up, not serve");
    assert!(!output.status.success(), "the broker started");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("HATCHLING_API_NETWORK") && stderr.contains("never the default"),
        "{stderr}"
    );
}

/// A sealed store holding anything but current envelopes stops the
/// broker before it binds, naming the store, the file and the command
/// that re-seals it. Pins the serve path's `check_sealed_stores`: a
/// broker that skipped it would serve, and be killed at the deadline.
#[test]
fn a_stale_sealed_store_stops_the_broker_before_bind() {
    let scratch = scratch_dir("stale-store").unwrap();
    let vault_dir = scratch.join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    std::fs::set_permissions(&vault_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let planted = vault_dir.join("tok");
    std::fs::write(&planted, b"plaintext-secret").unwrap();
    std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o600)).unwrap();

    let env = operator_env(&scratch, "vault");
    let socket = scratch.join("broker.sock");
    let output = run_broker(&[], &operator_env_refs(&env), &["HATCHLING_AGENT_BIN"]);
    let bound = socket.exists();
    let _ = std::fs::remove_dir_all(&scratch);
    let output = output.expect("the broker must stop at start-up, not serve");
    assert!(!output.status.success(), "the broker started");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("the secrets store")
            && stderr.contains("tok")
            && stderr.contains("migrate-vault"),
        "{stderr}"
    );
    assert!(
        !bound,
        "the broker bound its socket before checking its stores"
    );
}

#[test]
fn bad_vault_root_fails_before_bind() {
    let scratch = scratch_dir("bad-root").unwrap();
    let vault_dir = scratch.join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    // 0777 root: `Vault::open` must refuse it.
    std::fs::set_permissions(&vault_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

    let env = operator_env(&scratch, "vault");
    let socket = scratch.join("broker.sock");
    let output = run_broker(&[], &operator_env_refs(&env), &["HATCHLING_AGENT_BIN"]).unwrap();

    assert!(
        !output.status.success(),
        "broker served with an insecure vault root"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("vault"),
        "stderr should blame the vault, was: {stderr}"
    );
    assert!(
        !socket.exists(),
        "broker bound the socket before validating the vault"
    );
    // Restore permissions so the scratch dir can be removed.
    std::fs::set_permissions(&vault_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_starting_broker_sweeps_leftovers_unless_one_already_serves() {
    let scratch = scratch_dir("sweep").unwrap();
    let mut env = operator_env(&scratch, "vault");
    for (key, value) in &mut env {
        if *key == "HATCHLING_LOG_LEVEL" {
            *value = String::from("info");
        }
    }
    env.push((
        "HATCHLING_SCHEDULER_DIR",
        scratch.join("schedules").to_string_lossy().into_owned(),
    ));
    // The socket's path is a plain file: a start stops at bind, after
    // its lock and its sweep.
    let socket = scratch.join("broker.sock");
    std::fs::write(&socket, b"not a socket").unwrap();
    let leftover = scratch
        .join("schedules.runs")
        .join(format!("run-{}", "ab".repeat(12)));
    std::fs::create_dir_all(leftover.join("tmp")).unwrap();
    std::fs::write(leftover.join("tmp").join("out"), b"o").unwrap();
    let refs = operator_env_refs(&env);
    let unset = ["HATCHLING_AGENT_BIN"];
    let said = |output: &Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    };

    // While another broker holds the serving lock: a start and a clean
    // are refused, and nothing is swept.
    let serving = hatchling_sys::try_lock(&scratch.join("broker.sock.lock"))
        .unwrap()
        .unwrap();
    for args in [&[][..], &["clean"]] {
        let refused = run_broker(args, &refs, &unset).unwrap();
        assert!(!refused.status.success(), "{args:?}");
        assert!(
            said(&refused).contains("a broker is serving this socket"),
            "{args:?}: {}",
            said(&refused)
        );
    }
    assert!(leftover.join("tmp").join("out").exists());
    drop(serving);

    // A dry run lists it and keeps it.
    let dry = run_broker(&["clean", "--dry-run"], &refs, &unset).unwrap();
    assert!(dry.status.success(), "{}", said(&dry));
    assert!(
        said(&dry).contains(&format!("would remove {}", leftover.display())),
        "{}",
        said(&dry)
    );
    assert!(leftover.exists());

    // A start sweeps it, and says so, before it stops at bind.
    let started = run_broker(&[], &refs, &unset).unwrap();
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(!started.status.success(), "the plain file can't be bound");
    assert!(
        said(&started).contains("removed a leftover")
            && said(&started).contains(&leftover.display().to_string()),
        "{}",
        said(&started)
    );
    assert!(!leftover.exists(), "the start swept it");
}
