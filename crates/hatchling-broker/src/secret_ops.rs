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

use std::io::{BufRead, IsTerminal as _, Read, Write};
use std::path::Path;

use hatchling_vault::{MAX_SECRET_BYTES, MasterKey, SealedVault, Secret, SecretName};
use zeroize::Zeroizing;

use crate::{Config, Error};

/// How `secret` is used.
const USAGE: &str = "usage: hatchling-broker secret add <name> [--replace] | secret add --for <host:port> [--replace | --if-missing]";

/// What to do when the name is in the vault already.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Taken {
    /// Refuse, before anything is read.
    Refuse,
    /// Ask, and replace it.
    Replace,
    /// Keep it, and ask nothing: the command succeeds with it.
    Keep,
}

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
    let (name, taken) = target(&args)?;
    let key = crate::vault_ops::load_master_key(&config.vault)?;
    let said = add(&config.vault.root, &key, &name, taken, read_secret)?;
    writeln!(std::io::stdout().lock(), "{said}")
        .map_err(|err| refused(format!("cannot write the report: {err}")))
}

fn refused(reason: impl Into<String>) -> Error {
    Error::Secret {
        reason: reason.into(),
    }
}

/// The secret's name, and what to do when it's in the vault already.
fn target(args: &[&str]) -> Result<(SecretName, Taken), Error> {
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
    let taken = match rest {
        [] => Taken::Refuse,
        ["--replace"] => Taken::Replace,
        ["--if-missing"] => Taken::Keep,
        _ => return Err(usage()),
    };
    let parsed = name
        .parse::<SecretName>()
        .map_err(|err| refused(format!("{name:?} can't name a secret: {err}")))?;
    Ok((parsed, taken))
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
    taken: Taken,
    read: impl FnOnce(&str) -> Result<Secret, Error>,
) -> Result<String, Error> {
    let vault = SealedVault::open(root, key).map_err(|source| Error::Vault { source })?;
    let had = vault.exists(name);
    match (had, taken) {
        (true, Taken::Refuse) => {
            return Err(refused(format!(
                "{} is in the vault already: --replace replaces it",
                name.as_str()
            )));
        }
        (true, Taken::Keep) => {
            return Ok(format!(
                "{} is in the vault already: it's used as it is (`secret add --for ... --replace` changes it).",
                name.as_str()
            ));
        }
        (true, Taken::Replace) | (false, _) => {}
    }
    let secret = read(&format!(
        "The secret for {} (each character shows as *, never itself; Enter when done):",
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
pub(crate) fn read_secret(prompt: &str) -> Result<Secret, Error> {
    let stdin = std::io::stdin();
    let terminal = stdin.is_terminal();
    let mut input = stdin.lock();
    let mut said = std::io::stderr();
    if !terminal {
        return secret_from(&mut input, &mut said);
    }
    let _ = write!(said, "{prompt} ");
    let _ = said.flush();
    let read = {
        let _keys = hatchling_sys::KeyByKey::on_stdin();
        typed(&mut input, &mut said)
    };
    let _ = writeln!(said);
    finish(read?, &mut said)
}

/// The control keys the prompt answers.
const CTRL_C: u8 = 0x03;
const CTRL_D: u8 = 0x04;
const BACKSPACE: u8 = 0x08;
const CTRL_U: u8 = 0x15;
const ESCAPE: u8 = 0x1b;
const DELETE: u8 = 0x7f;

/// What's typed at the prompt, a key at a time, a mark `*` shown on
/// `shown` for each character as it arrives, so a paste is seen to land.
///
/// Enter (or a pasted line break) ends it; Backspace takes back the last
/// character and Ctrl-U all of them; Ctrl-C, or Ctrl-D with nothing typed,
/// cancels. An escape sequence (an arrow key) and any other control key
/// are passed over. Read into a buffer sized up front and wiped after:
/// past the longest secret, characters are counted, not kept.
fn typed(input: &mut impl Read, shown: &mut impl Write) -> Result<Zeroizing<Vec<u8>>, Error> {
    let room = MAX_SECRET_BYTES.saturating_add(1);
    let mut bytes = Zeroizing::new(Vec::with_capacity(room));
    let mut key = [0u8; 1];
    let mut next = |input: &mut dyn Read| -> Result<Option<u8>, Error> {
        match input.read(&mut key) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(key.first().copied()),
            Err(err) => Err(refused(format!("cannot read the secret: {err}"))),
        }
    };
    let cancelled = || refused("cancelled: nothing was stored");
    loop {
        let Some(byte) = next(input)? else {
            break;
        };
        match byte {
            b'\r' | b'\n' => break,
            CTRL_C => return Err(cancelled()),
            CTRL_D if bytes.is_empty() => return Err(cancelled()),
            CTRL_D => break,
            BACKSPACE | DELETE => {
                if bytes.pop().is_some() {
                    let _ = shown.write_all(b"\x08 \x08");
                }
            }
            CTRL_U => {
                for _ in 0..bytes.len() {
                    let _ = shown.write_all(b"\x08 \x08");
                }
                bytes.clear();
            }
            ESCAPE => {
                // CSI (`ESC [`) or SS3 (`ESC O`), up to its final byte.
                if matches!(next(input)?, Some(b'[' | b'O')) {
                    while let Some(part) = next(input)? {
                        if (0x40..=0x7e).contains(&part) {
                            break;
                        }
                    }
                }
            }
            control if control < 0x20 => {}
            character => {
                if bytes.len() < room {
                    bytes.push(character);
                }
                let _ = shown.write_all(b"*");
            }
        }
        let _ = shown.flush();
    }
    Ok(bytes)
}

/// A secret from a pipe, `input`: all of it, its trailing line breaks
/// not part of it. Read into a buffer sized up front and wiped after, so
/// no copy is left by one that grew; what's read is said on `said`.
fn secret_from(input: &mut impl BufRead, said: &mut impl Write) -> Result<Secret, Error> {
    // Room for the longest secret and a line ending: anything past that
    // is too long, and isn't read.
    let room = MAX_SECRET_BYTES.saturating_add(2);
    let mut bytes = Zeroizing::new(Vec::with_capacity(room));
    let mut bounded = input.take(u64::try_from(room).unwrap_or(u64::MAX));
    bounded
        .read_to_end(&mut bytes)
        .map_err(|err| refused(format!("cannot read the secret: {err}")))?;
    finish(bytes, said)
}

/// The secret `bytes` read, once checked: its trailing line breaks taken
/// off, a copy's stray ones, and then one line, from 1 byte to the
/// longest a secret may be. How many characters were read is said on
/// `said`, which shows a paste that went wrong without showing it.
fn finish(mut bytes: Zeroizing<Vec<u8>>, said: &mut impl Write) -> Result<Secret, Error> {
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
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
    // A character is each byte that doesn't continue one before it.
    let characters = bytes.iter().filter(|byte| **byte & 0xc0 != 0x80).count();
    let _ = writeln!(
        said,
        "Read {characters} character{}.",
        if characters == 1 { "" } else { "s" }
    );
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
            |list: &[&str]| target(list).map(|(name, taken)| (name.as_str().to_owned(), taken));
        assert_eq!(
            named(&["add", "search-brave"]).unwrap(),
            ("search-brave".into(), Taken::Refuse)
        );
        assert_eq!(
            named(&["add", "search-brave", "--replace"]).unwrap(),
            ("search-brave".into(), Taken::Replace)
        );
        // A hosted model's key, under the name its egress reads.
        assert_eq!(
            named(&["add", "--for", "openrouter.ai:443"]).unwrap(),
            ("egress-openrouter-ai-443".into(), Taken::Refuse)
        );
        assert_eq!(
            named(&["add", "--for", "api.example:8443", "--replace"]).unwrap(),
            ("egress-api-example-8443".into(), Taken::Replace)
        );
        assert_eq!(
            named(&["add", "--for", "openrouter.ai:443", "--if-missing"]).unwrap(),
            ("egress-openrouter-ai-443".into(), Taken::Keep)
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
            &[
                "add",
                "--for",
                "openrouter.ai:443",
                "--replace",
                "--if-missing",
            ],
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
    fn a_piped_secret_is_one_line_without_its_endings() {
        let read = |text: &str| {
            let mut said = Vec::new();
            let secret = secret_from(&mut text.as_bytes(), &mut said)
                .map(|secret| secret.as_bytes().to_vec());
            (secret, String::from_utf8(said).unwrap())
        };
        for given in [
            "sk-or-v1-key",
            "sk-or-v1-key\n",
            "sk-or-v1-key\r\n",
            "sk-or-v1-key\r\n\r\n",
            "sk-or-v1-key\n\n\n",
        ] {
            let (secret, said) = read(given);
            assert_eq!(secret.unwrap(), b"sk-or-v1-key", "{given:?}");
            assert_eq!(said, "Read 12 characters.\n");
        }
        for nothing in ["", "\n", "\r\n"] {
            assert!(matches!(read(nothing).0, Err(Error::Secret { .. })));
        }
        for broken in ["a\rb", "first\nsecond\n", "\nkey"] {
            let (secret, said) = read(broken);
            assert!(matches!(secret, Err(Error::Secret { .. })), "{broken:?}");
            assert_eq!(said, "", "nothing is said of a secret refused");
        }
        assert_eq!(read("k").1, "Read 1 character.\n");
        assert_eq!(
            read("\u{e9}t\u{e9}").1,
            "Read 3 characters.\n",
            "characters, not bytes"
        );
        // Built before the asserts: a format! in one trips 1.85's clippy.
        let longest = "k".repeat(MAX_SECRET_BYTES) + "\n";
        let too_long = "k".repeat(MAX_SECRET_BYTES + 1) + "\n";
        assert_eq!(read(&longest).0.unwrap().len(), MAX_SECRET_BYTES);
        assert!(matches!(read(&too_long).0, Err(Error::Secret { .. })));
    }

    /// What's typed, a key at a time, and what's shown for it.
    fn type_in(keys: &[u8]) -> (Result<Vec<u8>, Error>, Vec<u8>) {
        let mut shown = Vec::new();
        let typed = typed(&mut &keys[..], &mut shown).map(|bytes| bytes.to_vec());
        (typed, shown)
    }

    #[test]
    fn each_key_typed_shows_a_mark_and_enter_ends_it() {
        let (typed, shown) = type_in(b"abc\r");
        assert_eq!(typed.unwrap(), b"abc");
        assert_eq!(shown, b"***");
        // A paste arrives at once, its line break ending it.
        let (typed, shown) = type_in(b"sk-or-v1-key\r\nleft over");
        assert_eq!(typed.unwrap(), b"sk-or-v1-key");
        assert_eq!(shown, b"************");
        assert_eq!(type_in(b"abc\n").0.unwrap(), b"abc");
        assert_eq!(type_in(b"abc").0.unwrap(), b"abc", "the input ended");
        assert_eq!(type_in(b"abc\x04").0.unwrap(), b"abc", "Ctrl-D ends it");
    }

    #[test]
    fn backspace_and_ctrl_u_take_back_what_was_typed() {
        let (typed, shown) = type_in(b"abx\x7fc\r");
        assert_eq!(typed.unwrap(), b"abc");
        assert_eq!(shown, b"***\x08 \x08*");
        let (typed, shown) = type_in(b"ab\x08\x08\x08c\r");
        assert_eq!(typed.unwrap(), b"c");
        assert_eq!(
            shown, b"**\x08 \x08\x08 \x08*",
            "nothing to take back, nothing shown"
        );
        let (typed, shown) = type_in(b"xyz\x15abc\r");
        assert_eq!(typed.unwrap(), b"abc");
        assert_eq!(shown, b"***\x08 \x08\x08 \x08\x08 \x08***");
    }

    #[test]
    fn ctrl_c_cancels_and_arrows_and_controls_are_passed_over() {
        for keys in [&b"ab\x03"[..], b"\x03", b"\x04"] {
            let (typed, _) = type_in(keys);
            assert!(
                matches!(&typed, Err(Error::Secret { reason }) if reason.starts_with("cancelled")),
                "{keys:?}: {typed:?}"
            );
        }
        let (typed, shown) = type_in(b"\x1b[Aa\x1bOBb\x1b[1;5Cc\x1bxd\t\x01e\r");
        assert_eq!(typed.unwrap(), b"abcde");
        assert_eq!(shown, b"*****");
        assert!(
            type_in(b"\x1b[").0.unwrap().is_empty(),
            "an escape cut short"
        );
    }

    #[test]
    fn past_the_longest_secret_a_typed_one_is_counted_not_kept() {
        let mut keys = vec![b'k'; MAX_SECRET_BYTES + 50];
        keys.push(b'\r');
        let (typed, shown) = type_in(&keys);
        let typed = typed.unwrap();
        assert_eq!(typed.len(), MAX_SECRET_BYTES + 1);
        assert_eq!(shown.len(), MAX_SECRET_BYTES + 50, "a mark for each");
        let mut said = Vec::new();
        assert!(matches!(
            finish(Zeroizing::new(typed), &mut said),
            Err(Error::Secret { .. })
        ));
        assert!(said.is_empty());
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
        let said = add(&root, &key, &name, Taken::Refuse, |prompt| {
            assert!(prompt.contains("egress-openrouter-ai-443"), "{prompt}");
            Ok(secret(b"first-key"))
        })
        .unwrap();
        assert_eq!(said, "Sealed egress-openrouter-ai-443 in the vault.");
        assert_eq!(fetched(), b"first-key");
        // Taken: refused before anything is read, and left as it was.
        let asked = Cell::new(false);
        let taken = add(&root, &key, &name, Taken::Refuse, |_| {
            asked.set(true);
            Ok(secret(b"second-key"))
        });
        assert!(matches!(taken, Err(Error::Secret { .. })));
        assert!(!asked.get());
        assert_eq!(fetched(), b"first-key");
        let said = add(&root, &key, &name, Taken::Replace, |_| {
            Ok(secret(b"second-key"))
        })
        .unwrap();
        assert_eq!(said, "Replaced egress-openrouter-ai-443 in the vault.");
        assert_eq!(fetched(), b"second-key");
        // Kept: nothing is asked, and it succeeds with the one there.
        let asked = Cell::new(false);
        let kept = add(&root, &key, &name, Taken::Keep, |_| {
            asked.set(true);
            Ok(secret(b"third-key"))
        })
        .unwrap();
        assert!(
            kept.starts_with("egress-openrouter-ai-443 is in the vault already"),
            "{kept}"
        );
        assert!(!asked.get());
        assert_eq!(fetched(), b"second-key");
        // Nothing read, nothing stored.
        let other: SecretName = "search-brave".parse().unwrap();
        let unread = add(&root, &key, &other, Taken::Refuse, |_| {
            Err(refused("no secret"))
        });
        assert!(matches!(unread, Err(Error::Secret { .. })));
        assert!(!SealedVault::open(&root, &key).unwrap().exists(&other));
        // Missing, kept asks for it.
        let said = add(&root, &key, &other, Taken::Keep, |_| {
            Ok(secret(b"search-key"))
        })
        .unwrap();
        assert_eq!(said, "Sealed search-brave in the vault.");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}
