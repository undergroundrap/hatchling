//! `secret add`: a key into the vault, typed where nothing keeps it.
//!
//! A hosted model's key, or a search API's, goes into the vault under the
//! name the broker reads it by. The docs had the owner run a script that
//! wrote the key as a plaintext file and then sealed the vault: the key
//! sat on disk unsealed between the two, and the broker had to be
//! stopped meanwhile. Here it's read at a prompt that doesn't show it, or
//! from a pipe (a password manager's), and never from an argument or the
//! environment, which the process table and the shell's history keep. It's
//! sealed at once, in memory wiped after. The keeper reads the vault at
//! each request, so a running broker uses it from its next request.

use std::io::{BufRead, IsTerminal as _, Read as _, Write as _};
use std::path::Path;

use hatchling_vault::{MAX_SECRET_BYTES, MasterKey, SealedVault, Secret, SecretName};
use zeroize::Zeroizing;

use crate::{Config, Error};

/// How `secret` is used.
const USAGE: &str = "usage: hatchling-broker secret add <name> [--replace] | secret add --for <host:port> [--replace]";

/// Runs `secret` with `args`, for the vault `config` names.
///
/// # Errors
///
/// [`Error::Usage`] for arguments it doesn't take, [`Error::Secret`] for
/// a name that's taken (without `--replace`) or a secret that isn't one
/// line, and the vault's errors.
pub fn run_secret(config: &Config, args: impl IntoIterator<Item = String>) -> Result<(), Error> {
    let args: Vec<String> = args.into_iter().collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (name, replace) = target(&args)?;
    let key = crate::vault_ops::load_master_key(&config.vault)?;
    let said = add(&config.vault.root, &key, &name, replace, read_secret)?;
    writeln!(std::io::stdout().lock(), "{said}")
        .map_err(|err| refused(format!("cannot write the report: {err}")))
}

fn refused(reason: impl Into<String>) -> Error {
    Error::Secret {
        reason: reason.into(),
    }
}

/// The secret's name, and whether one already there may be replaced.
fn target(args: &[&str]) -> Result<(SecretName, bool), Error> {
    let usage = || Error::Usage {
        reason: String::from(USAGE),
    };
    let (name, rest) = match args {
        ["add", "--for", destination, rest @ ..] => {
            (egress_name(destination).ok_or_else(usage)?, rest)
        }
        ["add", name, rest @ ..] if !name.starts_with('-') => ((*name).to_owned(), rest),
        _ => return Err(usage()),
    };
    let replace = match rest {
        [] => false,
        ["--replace"] => true,
        _ => return Err(usage()),
    };
    let parsed = name
        .parse::<SecretName>()
        .map_err(|err| refused(format!("{name:?} can't name a secret: {err}")))?;
    Ok((parsed, replace))
}

/// The name a hosted API's key at `destination` (`host:port`) is kept
/// under: the broker's own mapping, so it's the name its egress reads.
fn egress_name(destination: &str) -> Option<String> {
    let (host, port) = destination.rsplit_once(':')?;
    (!host.is_empty() && port.parse::<u16>().is_ok_and(|port| port != 0))
        .then(|| crate::serve::destination_to_secret_name(destination))
}

/// Seals what `read` gives as `name` in the vault at `root`. A name
/// already there is refused before anything is read, unless `replace`.
fn add(
    root: &Path,
    key: &MasterKey,
    name: &SecretName,
    replace: bool,
    read: impl FnOnce(&str) -> Result<Secret, Error>,
) -> Result<String, Error> {
    let vault = SealedVault::open(root, key).map_err(|source| Error::Vault { source })?;
    let had = vault.exists(name);
    if had && !replace {
        return Err(refused(format!(
            "{} is in the vault already: --replace replaces it",
            name.as_str()
        )));
    }
    let secret = read(&format!(
        "The secret for {} (it isn't shown as you type):",
        name.as_str()
    ))?;
    vault
        .store(name, &secret)
        .map_err(|source| Error::Vault { source })?;
    let done = if had { "Replaced" } else { "Sealed" };
    Ok(format!("{done} {} in the vault.", name.as_str()))
}

/// The secret: at a prompt that doesn't show it, when stdin is a
/// terminal; else all of stdin.
fn read_secret(prompt: &str) -> Result<Secret, Error> {
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    let mut input = stdin.lock();
    if !terminal {
        return secret_from(&mut input, true);
    }
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "{prompt} ");
    let _ = stderr.flush();
    let read = {
        let _echo = hatchling_sys::EchoOff::on_stdin();
        secret_from(&mut input, false)
    };
    // The newline typed wasn't shown.
    let _ = writeln!(stderr);
    read
}

/// A secret from `input`: one line, or all of it when `whole` (from a
/// pipe), its line ending not part of it. Read into a buffer sized up
/// front and wiped after, so no copy is left by one that grew.
fn secret_from(input: &mut impl BufRead, whole: bool) -> Result<Secret, Error> {
    // Room for the longest secret and its line ending: anything past
    // that is too long, and isn't read.
    let room = MAX_SECRET_BYTES.saturating_add(2);
    let mut bytes = Zeroizing::new(Vec::with_capacity(room));
    let mut bounded = input.take(u64::try_from(room).unwrap_or(u64::MAX));
    let read = if whole {
        bounded.read_to_end(&mut bytes)
    } else {
        bounded.read_until(b'\n', &mut bytes)
    };
    read.map_err(|err| refused(format!("cannot read the secret: {err}")))?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.is_empty() {
        return Err(refused("no secret was given: nothing was stored"));
    }
    if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(refused(
            "the secret is more than one line: nothing was stored",
        ));
    }
    if bytes.len() > MAX_SECRET_BYTES {
        return Err(refused(format!(
            "the secret is longer than {MAX_SECRET_BYTES} bytes: nothing was stored"
        )));
    }
    Secret::new(std::mem::take(&mut *bytes)).map_err(|source| Error::Vault { source })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn a_secret_is_named_or_found_by_its_destination() {
        let named =
            |list: &[&str]| target(list).map(|(name, replace)| (name.as_str().to_owned(), replace));
        assert_eq!(
            named(&["add", "search-brave"]).unwrap(),
            ("search-brave".into(), false)
        );
        assert_eq!(
            named(&["add", "search-brave", "--replace"]).unwrap(),
            ("search-brave".into(), true)
        );
        // A hosted model's key, under the name its egress reads.
        assert_eq!(
            named(&["add", "--for", "openrouter.ai:443"]).unwrap(),
            ("egress-openrouter-ai-443".into(), false)
        );
        assert_eq!(
            named(&["add", "--for", "api.example:8443", "--replace"]).unwrap(),
            ("egress-api-example-8443".into(), true)
        );
        for wrong in [
            &["add"][..],
            &["add", "--for"],
            &["add", "--for", "openrouter.ai"],
            &["add", "--for", ":443"],
            &["add", "--for", "host:0"],
            &["add", "--for", "host:http"],
            &["add", "--replace"],
            &["add", "name", "--force"],
            &["add", "name", "--replace", "extra"],
            &["list"],
        ] {
            assert!(
                matches!(named(wrong), Err(Error::Usage { .. })),
                "{wrong:?}"
            );
        }
        assert!(matches!(named(&["add", "a/b"]), Err(Error::Secret { .. })));
    }

    #[test]
    fn a_secret_is_one_line_without_its_ending() {
        let read = |text: &str, whole: bool| {
            secret_from(&mut text.as_bytes(), whole).map(|secret| secret.as_bytes().to_vec())
        };
        for whole in [false, true] {
            for given in ["sk-or-v1-key", "sk-or-v1-key\n", "sk-or-v1-key\r\n"] {
                assert_eq!(read(given, whole).unwrap(), b"sk-or-v1-key");
            }
            for nothing in ["", "\n", "\r\n"] {
                assert!(matches!(read(nothing, whole), Err(Error::Secret { .. })));
            }
            assert!(matches!(read("a\rb", whole), Err(Error::Secret { .. })));
        }
        // From a pipe, the whole of it: a second line is refused.
        assert!(matches!(
            read("first\nsecond\n", true),
            Err(Error::Secret { .. })
        ));
        // At a terminal, the line typed.
        assert_eq!(read("first\nsecond\n", false).unwrap(), b"first");
        // Built before the asserts: a format! in one trips 1.85's clippy.
        let longest = "k".repeat(MAX_SECRET_BYTES) + "\n";
        let too_long = "k".repeat(MAX_SECRET_BYTES + 1) + "\n";
        assert_eq!(read(&longest, true).unwrap().len(), MAX_SECRET_BYTES);
        assert!(matches!(read(&too_long, true), Err(Error::Secret { .. })));
    }

    /// A vault root under a fresh owner-only directory, and its key.
    fn vault(name: &str) -> (std::path::PathBuf, MasterKey) {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-secret-ops-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        (dir.join("vault"), MasterKey::generate().unwrap())
    }

    fn secret(bytes: &[u8]) -> Secret {
        Secret::new(bytes.to_vec()).unwrap()
    }

    #[test]
    fn a_secret_is_sealed_and_one_there_is_replaced_only_when_asked() {
        let (root, key) = vault("add");
        let name: SecretName = "egress-openrouter-ai-443".parse().unwrap();
        let fetched = || {
            SealedVault::open(&root, &key)
                .and_then(|vault| vault.fetch(&name))
                .unwrap()
                .as_bytes()
                .to_vec()
        };
        let said = add(&root, &key, &name, false, |prompt| {
            assert!(prompt.contains("egress-openrouter-ai-443"), "{prompt}");
            Ok(secret(b"first-key"))
        })
        .unwrap();
        assert_eq!(said, "Sealed egress-openrouter-ai-443 in the vault.");
        assert_eq!(fetched(), b"first-key");
        // Taken: refused before anything is read, and left as it was.
        let asked = Cell::new(false);
        let taken = add(&root, &key, &name, false, |_| {
            asked.set(true);
            Ok(secret(b"second-key"))
        });
        assert!(matches!(taken, Err(Error::Secret { .. })));
        assert!(!asked.get());
        assert_eq!(fetched(), b"first-key");
        let said = add(&root, &key, &name, true, |_| Ok(secret(b"second-key"))).unwrap();
        assert_eq!(said, "Replaced egress-openrouter-ai-443 in the vault.");
        assert_eq!(fetched(), b"second-key");
        // Nothing read, nothing stored.
        let other: SecretName = "search-brave".parse().unwrap();
        let unread = add(&root, &key, &other, false, |_| Err(refused("no secret")));
        assert!(matches!(unread, Err(Error::Secret { .. })));
        assert!(!SealedVault::open(&root, &key).unwrap().exists(&other));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
