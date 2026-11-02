//! Harness manifests (WO-018): a coding CLI the operator adds by
//! configuration, run as the built-in harnesses are.
//!
//! A manifest is one JSON file, `<name>.json`, in the directory
//! `HATCHLING_HARNESS_MANIFESTS` names. It says how to run the CLI: its
//! binary and any companion files, each pinned by SHA-256; the API it
//! speaks (`anthropic` or `openai`, the relay's two wires) and its one
//! destination; its command line, with `{task}` where the task goes; and
//! the environment that points it at its relay (`{relay}`) and carries
//! its surrogate key (`{key}`).
//!
//! Everything else stays the broker's: the cell, the relay and the swap
//! of the surrogate for the vault's key, the request policy, the work
//! tree's copy, the patch, the activity feed, the timeout. The vendor key
//! is the vault secret `harness-<name>`, as for the built-in harnesses.
//!
//! **Who may define one.** A harness gets a network and a key that spends
//! money on the operator's account, so only the operator defines one: the
//! directory and every manifest must be the broker's user's or root's,
//! and writable by no one else. The agent can't add, change or pick one
//! that isn't configured.
//!
//! **Read once, at the start.** Each manifest is checked whole: a field
//! that's wrong refuses the start, naming the file and the field, so a
//! harness never runs half-configured. Its binary and companions must
//! hash to their pins then, and before every run, as the built-in
//! harnesses' must.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use hatchling_egress::TlsClient;
use serde::Deserialize;

use crate::Error;
use crate::harness::{HarnessKind, PinnedBinary};
use crate::harness_relay::{Vendor, Wire};

/// The setting naming the manifests' directory.
pub(crate) const MANIFESTS_KEY: &str = crate::environment::HATCHLING_HARNESS_MANIFESTS.name;

/// The manifest format's version.
const VERSION: u32 = 1;

/// The most a manifest file may hold.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

/// The most manifests a directory may hold.
const MAX_MANIFESTS: usize = 32;

/// The longest harness name: grants and secrets name it.
const MAX_NAME_BYTES: usize = 32;

/// The most arguments a command line may have.
const MAX_ARGS: usize = 64;

/// The most variables a manifest may set.
const MAX_ENV: usize = 64;

/// The most companion files a harness may have.
const MAX_COMPANIONS: usize = 16;

/// The longest argument or variable value.
const MAX_VALUE_BYTES: usize = 16 * 1024;

/// Where the task goes: in one argument.
const TASK: &str = "{task}";

/// The relay's base URL, `http://127.0.0.1:<port>`.
const RELAY: &str = "{relay}";

/// The run's surrogate key: in the environment only.
const KEY: &str = "{key}";

/// Names a companion can't take: what a cell's root already holds.
const RESERVED_NAMES: [&str; 20] = [
    "agent", "work", "scratch", "dev", "proc", "sys", "tmp", "etc", "usr", "bin", "sbin", "lib",
    "lib64", "run", "var", "opt", "home", "root", "fonts", "browser",
];

/// A manifest as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    name: String,
    binary: StoredPin,
    #[serde(default)]
    companions: Vec<StoredCompanion>,
    api: String,
    destination: String,
    args: Vec<String>,
    #[serde(default)]
    env: Vec<StoredVar>,
}

/// A pinned program as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPin {
    path: String,
    sha256: String,
}

/// A companion file as written: pinned, and the name the cell sees it at,
/// beside the CLI.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCompanion {
    path: String,
    sha256: String,
    name: String,
}

/// One environment variable as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredVar {
    name: String,
    value: String,
}

/// A harness an operator's manifest defines, checked whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    name: String,
    skill_id: String,
    binary: PinnedBinary,
    companions: Vec<(PinnedBinary, String)>,
    wire: Wire,
    args: Vec<String>,
    env: Vec<(String, String)>,
}

impl Manifest {
    /// The harness's name, as grants name it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The harness skill's id, as the agent invokes it: `<name>@1`.
    #[must_use]
    pub fn skill_id(&self) -> &str {
        &self.skill_id
    }

    /// Its pinned binary.
    pub(crate) const fn binary(&self) -> &PinnedBinary {
        &self.binary
    }

    /// Its pinned companion files, each with the name the cell sees it at.
    pub(crate) fn companions(&self) -> &[(PinnedBinary, String)] {
        &self.companions
    }

    /// The API its relay speaks.
    pub(crate) const fn wire(&self) -> Wire {
        self.wire
    }

    /// Its command line for `task`, the relay on `relay`.
    #[must_use]
    pub fn args(&self, task: &str, relay: u16) -> Vec<String> {
        let relay = relay_url(relay);
        self.args
            .iter()
            .map(|arg| expand(arg, &[(TASK, task), (RELAY, &relay)]))
            .collect()
    }

    /// Its own variables for the relay on `relay` and the surrogate key
    /// `surrogate`.
    #[must_use]
    pub fn env(&self, relay: u16, surrogate: &str) -> Vec<(String, String)> {
        let relay = relay_url(relay);
        self.env
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    expand(value, &[(RELAY, &relay), (KEY, surrogate)]),
                )
            })
            .collect()
    }
}

/// The relay's base URL in the cell.
fn relay_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

/// `template` with each placeholder of `values` replaced, in one pass:
/// what a value holds is never read as a placeholder.
fn expand(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some((before, after)) = rest.split_once('{') {
        out.push_str(before);
        let found = values.iter().find_map(|(placeholder, value)| {
            placeholder
                .strip_prefix('{')
                .and_then(|name| after.strip_prefix(name))
                .map(|tail| (value, tail))
        });
        if let Some((value, tail)) = found {
            out.push_str(value);
            rest = tail;
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// A refusal naming the manifest and its field.
fn refused(path: &Path, field: &str, reason: impl Into<String>) -> Error {
    Error::HarnessManifest {
        path: path.to_path_buf(),
        field: field.to_owned(),
        reason: reason.into(),
    }
}

/// Whether a file or directory with `uid` and `mode` may define a
/// harness for a process running as `euid`: its user's or root's, and
/// writable by no one else.
const fn trusted(uid: u32, mode: u32, euid: u32) -> bool {
    (uid == euid || uid == 0) && mode & 0o022 == 0
}

/// Checks `meta`, `path`'s, is trusted to define a harness.
fn check_owner(path: &Path, meta: &std::fs::Metadata, euid: u32) -> Result<(), Error> {
    if trusted(meta.uid(), meta.mode(), euid) {
        Ok(())
    } else {
        Err(refused(
            path,
            "owner",
            format!(
                "must be uid {euid}'s or root's, and writable by no one else: a manifest \
                 another user can change could run their program with the operator's key"
            ),
        ))
    }
}

/// The manifests in the directory `raw` names, each checked, with its
/// vendor reached through `tls`. None when `raw` is unset or blank.
///
/// # Errors
///
/// [`Error::InvalidConfig`] for a directory that isn't absolute;
/// [`Error::HarnessManifest`], naming the file and its field, for a
/// directory or manifest that isn't the operator's alone, or a manifest
/// that's wrong in any way.
pub(crate) fn load(raw: Option<&str>, tls: &TlsClient) -> Result<Vec<(Manifest, Vendor)>, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(Vec::new());
    };
    let dir = PathBuf::from(raw);
    if !dir.is_absolute() {
        return Err(Error::InvalidConfig {
            key: MANIFESTS_KEY,
            reason: format!("must be an absolute directory: {raw}"),
        });
    }
    let euid = hatchling_sys::euid();
    let meta = std::fs::symlink_metadata(&dir)
        .map_err(|error| refused(&dir, "directory", format!("is unreadable: {error}")))?;
    if !meta.file_type().is_dir() {
        return Err(refused(&dir, "directory", "is not a directory"));
    }
    check_owner(&dir, &meta, euid)?;
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .map_err(|error| refused(&dir, "directory", format!("is unreadable: {error}")))?
    {
        let entry =
            entry.map_err(|error| refused(&dir, "directory", format!("is unreadable: {error}")))?;
        paths.push(entry.path());
    }
    if paths.len() > MAX_MANIFESTS {
        return Err(refused(
            &dir,
            "directory",
            format!("holds more than {MAX_MANIFESTS} files"),
        ));
    }
    paths.sort();
    // Each name is its file's, so no two manifests share one.
    paths.iter().map(|path| read(path, euid, tls)).collect()
}

/// The manifest at `path`, read and checked.
fn read(path: &Path, euid: u32, tls: &TlsClient) -> Result<(Manifest, Vendor), Error> {
    use std::io::Read as _;
    let stem = path
        .extension()
        .filter(|extension| *extension == "json")
        .and_then(|_| path.file_stem())
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            refused(
                path,
                "file",
                "a manifest is a <name>.json file, and nothing else",
            )
        })?;
    let unreadable =
        |error: std::io::Error| refused(path, "file", format!("is unreadable: {error}"));
    let meta = std::fs::symlink_metadata(path).map_err(unreadable)?;
    if !meta.file_type().is_file() {
        return Err(refused(path, "file", "is not a regular file"));
    }
    check_owner(path, &meta, euid)?;
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let opened = file.metadata().map_err(unreadable)?;
    if (opened.dev(), opened.ino()) != (meta.dev(), meta.ino()) {
        return Err(refused(path, "file", "changed as it was read"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(refused(
            path,
            "file",
            format!("is over {MAX_MANIFEST_BYTES} bytes"),
        ));
    }
    let stored: Stored =
        serde_json::from_slice(&bytes).map_err(|error| refused(path, "json", error.to_string()))?;
    check(path, stem, stored, tls)
}

/// A manifest from what `path` holds, its name `stem`, every field
/// checked.
fn check(
    path: &Path,
    stem: &str,
    stored: Stored,
    tls: &TlsClient,
) -> Result<(Manifest, Vendor), Error> {
    if stored.version != VERSION {
        return Err(refused(
            path,
            "version",
            format!("is {VERSION}, the one format there is"),
        ));
    }
    check_name(&stored.name).map_err(|reason| refused(path, "name", reason))?;
    if stored.name != stem {
        return Err(refused(
            path,
            "name",
            format!("must be the file's name: {stem:?}"),
        ));
    }
    if HarnessKind::ALL
        .iter()
        .any(|kind| kind.name() == stored.name)
    {
        return Err(refused(
            path,
            "name",
            format!("{:?} is a built-in harness", stored.name),
        ));
    }
    let binary = PinnedBinary::checked(&stored.binary.path, &stored.binary.sha256)
        .map_err(|(field, reason)| refused(path, &format!("binary.{field}"), reason))?;
    let companions = check_companions(path, stored.companions)?;
    let wire = match stored.api.as_str() {
        "anthropic" => Wire::Anthropic,
        "openai" => Wire::OpenAi,
        other => {
            return Err(refused(
                path,
                "api",
                format!("{other:?} isn't an API the relay speaks: anthropic or openai"),
            ));
        }
    };
    let vendor = crate::harness::vendor(&stored.destination, wire, tls)
        .map_err(|reason| refused(path, "destination", reason))?;
    check_args(&stored.args).map_err(|reason| refused(path, "args", reason))?;
    let env = check_env(&stored.env).map_err(|reason| refused(path, "env", reason))?;
    let relayed = stored
        .args
        .iter()
        .chain(env.iter().map(|(_, value)| value))
        .any(|text| text.contains(RELAY));
    if !relayed {
        return Err(refused(
            path,
            "env",
            format!("nothing points the CLI at its relay: put {RELAY} in its base URL's variable"),
        ));
    }
    let manifest = Manifest {
        skill_id: format!("{}@1", stored.name),
        name: stored.name,
        binary,
        companions,
        wire,
        args: stored.args,
        env,
    };
    Ok((manifest, vendor))
}

/// Why `name` can't name a harness: lower-case letters, digits and
/// inner dashes, starting with a letter.
fn check_name(name: &str) -> Result<(), String> {
    let well_formed = name.len() <= MAX_NAME_BYTES
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if well_formed {
        Ok(())
    } else {
        Err(format!(
            "{name:?} isn't a harness name: up to {MAX_NAME_BYTES} lower-case letters, digits \
             and inner dashes, starting with a letter"
        ))
    }
}

/// The companions, each pinned, at a name of its own beside the CLI.
fn check_companions(
    path: &Path,
    stored: Vec<StoredCompanion>,
) -> Result<Vec<(PinnedBinary, String)>, Error> {
    if stored.len() > MAX_COMPANIONS {
        return Err(refused(
            path,
            "companions",
            format!("more than {MAX_COMPANIONS}"),
        ));
    }
    let mut companions: Vec<(PinnedBinary, String)> = Vec::new();
    for (index, companion) in stored.into_iter().enumerate() {
        let field = |name: &str| format!("companions[{index}].{name}");
        let mut components = Path::new(&companion.name).components();
        let plain = matches!(
            (components.next(), components.next()),
            (Some(Component::Normal(_)), None)
        );
        let named = plain
            && !companion.name.starts_with('.')
            && companion
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            && !RESERVED_NAMES.contains(&companion.name.as_str());
        if !named {
            return Err(refused(
                path,
                &field("name"),
                format!(
                    "{:?} isn't a companion's name: one file name of letters, digits and ._-, \
                     not one the cell's root holds",
                    companion.name
                ),
            ));
        }
        if companions.iter().any(|(_, known)| *known == companion.name) {
            return Err(refused(
                path,
                &field("name"),
                format!("{:?} is named twice", companion.name),
            ));
        }
        let pinned = PinnedBinary::checked(&companion.path, &companion.sha256)
            .map_err(|(name, reason)| refused(path, &field(name), reason))?;
        companions.push((pinned, companion.name));
    }
    Ok(companions)
}

/// Why `args` can't be a command line: it needs the task in one argument,
/// never the key, within bounds.
fn check_args(args: &[String]) -> Result<(), String> {
    if args.len() > MAX_ARGS {
        return Err(format!("more than {MAX_ARGS} arguments"));
    }
    if let Some(arg) = args
        .iter()
        .find(|arg| arg.len() > MAX_VALUE_BYTES || arg.contains('\0'))
    {
        return Err(format!(
            "an argument is over {MAX_VALUE_BYTES} bytes or holds a NUL: {:?}",
            arg.chars().take(64).collect::<String>()
        ));
    }
    match args
        .iter()
        .map(|arg| arg.matches(TASK).count())
        .sum::<usize>()
    {
        0 => {
            return Err(format!(
                "no {TASK} placeholder: the CLI would never get its task"
            ));
        }
        1 => {}
        _ => return Err(format!("{TASK} more than once: the task goes in one place")),
    }
    if args.iter().any(|arg| arg.contains(KEY)) {
        return Err(format!(
            "{KEY} is in an argument: the surrogate key goes in the environment"
        ));
    }
    Ok(())
}

/// The variables, checked: each name a plain one the cell doesn't set
/// itself, each value bounded, the key in one at least, the task in none.
fn check_env(vars: &[StoredVar]) -> Result<Vec<(String, String)>, String> {
    if vars.len() > MAX_ENV {
        return Err(format!("more than {MAX_ENV} variables"));
    }
    let mut env: Vec<(String, String)> = Vec::new();
    for variable in vars {
        let plain = variable
            .name
            .starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && variable
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if !plain {
            return Err(format!("{:?} isn't a variable name", variable.name));
        }
        if ["PATH", "HOME", "TMPDIR"].contains(&variable.name.as_str())
            || variable.name.starts_with("HATCHLING_")
        {
            return Err(format!(
                "{} is the cell's to set: PATH, HOME, TMPDIR and HATCHLING_ variables are",
                variable.name
            ));
        }
        if env.iter().any(|(name, _)| *name == variable.name) {
            return Err(format!("{} is set twice", variable.name));
        }
        if variable.value.len() > MAX_VALUE_BYTES || variable.value.contains('\0') {
            return Err(format!(
                "{} is over {MAX_VALUE_BYTES} bytes or holds a NUL",
                variable.name
            ));
        }
        if variable.value.contains(TASK) {
            return Err(format!(
                "{} holds {TASK}: the task goes in an argument",
                variable.name
            ));
        }
        env.push((variable.name.clone(), variable.value.clone()));
    }
    if !env.iter().any(|(_, value)| value.contains(KEY)) {
        return Err(format!(
            "no {KEY} placeholder: the CLI would have no key to present to its relay"
        ));
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use hatchling_skill::CodeHash;
    use serde_json::{Value, json};

    use super::*;

    /// A scratch directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hatchling-manifest-unit-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("manifests")).unwrap();
            set_mode(&dir.join("manifests"), 0o755);
            Self(dir)
        }

        fn manifests(&self) -> PathBuf {
            self.0.join("manifests")
        }

        /// A program at `name` holding `bytes`; its path and SHA-256.
        fn program(&self, name: &str, bytes: &[u8]) -> (String, String) {
            let path = self.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            set_mode(&path, 0o755);
            (path.display().to_string(), CodeHash::of(bytes).to_string())
        }

        /// `manifest` written as `<name>.json`, `0644`.
        fn write(&self, name: &str, manifest: &Value) -> PathBuf {
            let path = self.manifests().join(format!("{name}.json"));
            std::fs::write(&path, manifest.to_string()).unwrap();
            set_mode(&path, 0o644);
            path
        }

        /// What loading the directory gives.
        fn load(&self) -> Result<Vec<(Manifest, Vendor)>, Error> {
            load(
                Some(&self.manifests().display().to_string()),
                &TlsClient::bundled().unwrap(),
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn set_mode(path: &Path, mode: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A manifest of `OpenCode`'s shape, its binary in `scratch`.
    fn opencode(scratch: &Scratch) -> Value {
        let (path, sha256) = scratch.program("opencode", b"opencode bytes");
        json!({
            "version": 1,
            "name": "opencode",
            "binary": {"path": path, "sha256": sha256},
            "api": "anthropic",
            "destination": "local:localhost:8443",
            "args": ["run", "--model", "anthropic/claude", "{task}"],
            "env": [
                {"name": "ANTHROPIC_API_KEY", "value": "{key}"},
                {"name": "OPENCODE_CONFIG_CONTENT",
                 "value": "{\"provider\":{\"anthropic\":{\"options\":{\"baseURL\":\"{relay}/v1\"}}}}"}
            ]
        })
    }

    /// The field a refusal names, and its reason.
    fn refusal(result: Result<Vec<(Manifest, Vendor)>, Error>) -> (String, String) {
        match result {
            Err(Error::HarnessManifest { field, reason, .. }) => (field, reason),
            other => (String::from("not refused"), format!("{other:?}")),
        }
    }

    /// `manifest` with `pointer` set to `value`, or removed for `None`.
    fn with(mut manifest: Value, pointer: &str, value: Option<Value>) -> Value {
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        match (manifest.pointer_mut(parent).unwrap(), value) {
            (Value::Object(map), Some(value)) => {
                map.insert(key.to_owned(), value);
            }
            (Value::Object(map), None) => {
                map.remove(key);
            }
            (Value::Array(items), Some(value)) => {
                *items.get_mut(key.parse::<usize>().unwrap()).unwrap() = value;
            }
            (Value::Array(items), None) => {
                items.remove(key.parse::<usize>().unwrap());
            }
            (other, _) => unreachable!("{other}"),
        }
        manifest
    }

    /// The refusal of `manifest`, written as `opencode.json`.
    fn refused_as(scratch: &Scratch, manifest: &Value) -> (String, String) {
        scratch.write("opencode", manifest);
        refusal(scratch.load())
    }

    #[test]
    fn a_manifest_defines_a_harness_run_as_it_says() {
        let scratch = Scratch::new("good");
        let (companion, companion_sha) = scratch.program("helper", b"helper bytes");
        let manifest = with(
            opencode(&scratch),
            "/companions",
            Some(json!([{"path": companion, "sha256": companion_sha, "name": "helper"}])),
        );
        scratch.write("opencode", &manifest);
        let loaded = scratch.load().unwrap();
        let [(manifest, vendor)] = loaded.as_slice() else {
            unreachable!("one manifest: {loaded:?}")
        };
        assert_eq!(manifest.name(), "opencode");
        assert_eq!(manifest.skill_id(), "opencode@1");
        assert_eq!(manifest.wire(), Wire::Anthropic);
        assert_eq!(
            manifest.binary().sha256,
            CodeHash::of(b"opencode bytes").to_string()
        );
        let [(pinned, name)] = manifest.companions() else {
            unreachable!("one companion: {:?}", manifest.companions())
        };
        assert_eq!(name, "helper");
        assert_eq!(pinned.sha256, companion_sha);
        assert_eq!(vendor.wire, Wire::Anthropic);
        assert_eq!(vendor.destination, "localhost:8443");
        // The task goes in its argument whole, and what it says is never
        // read as a placeholder.
        assert_eq!(
            manifest.args("Say {key} and {relay}.", 8787),
            [
                "run",
                "--model",
                "anthropic/claude",
                "Say {key} and {relay}."
            ]
        );
        assert_eq!(
            manifest.env(8787, "sk-ant-hatchling-x"),
            [
                (
                    String::from("ANTHROPIC_API_KEY"),
                    String::from("sk-ant-hatchling-x")
                ),
                (
                    String::from("OPENCODE_CONFIG_CONTENT"),
                    String::from(
                        "{\"provider\":{\"anthropic\":{\"options\":{\"baseURL\":\"http://127.0.0.1:8787/v1\"}}}}"
                    )
                ),
            ]
        );
        // Unset or blank, there are none.
        let tls = TlsClient::bundled().unwrap();
        assert!(load(None, &tls).unwrap().is_empty());
        assert!(load(Some("  "), &tls).unwrap().is_empty());
        // An empty directory defines none.
        std::fs::remove_file(scratch.manifests().join("opencode.json")).unwrap();
        assert!(scratch.load().unwrap().is_empty());
    }

    #[test]
    fn placeholders_are_replaced_once_and_nothing_else_is() {
        let values = [(TASK, "{relay}"), (RELAY, "http://r")];
        assert_eq!(expand("{task}", &values), "{relay}");
        assert_eq!(expand("a{relay}b{task}c", &values), "ahttp://rb{relay}c");
        assert_eq!(expand("{x}{{relay}}{", &values), "{x}{http://r}{");
        assert_eq!(expand("", &values), "");
        assert_eq!(expand("plain", &values), "plain");
        assert_eq!(relay_url(9), "http://127.0.0.1:9");
    }

    #[test]
    fn only_its_user_or_root_may_write_a_manifest() {
        assert!(trusted(7, 0o100_644, 7));
        assert!(trusted(0, 0o100_644, 7), "root's");
        assert!(trusted(7, 0o040_755, 7));
        assert!(!trusted(8, 0o100_644, 7), "another user's");
        assert!(!trusted(7, 0o100_664, 7), "group-writable");
        assert!(!trusted(7, 0o100_646, 7), "world-writable");
        assert!(!trusted(0, 0o100_666, 7));
        let scratch = Scratch::new("owner");
        let path = scratch.write("opencode", &opencode(&scratch));
        set_mode(&path, 0o664);
        let (field, reason) = refusal(scratch.load());
        assert_eq!(field, "owner");
        assert!(reason.contains("writable by no one else"), "{reason}");
        set_mode(&path, 0o644);
        assert!(scratch.load().is_ok());
        set_mode(&scratch.manifests(), 0o777);
        assert_eq!(refusal(scratch.load()).0, "owner");
        set_mode(&scratch.manifests(), 0o755);
    }

    #[test]
    fn a_directory_holds_manifests_and_nothing_else() {
        let scratch = Scratch::new("directory");
        let tls = TlsClient::bundled().unwrap();
        assert!(matches!(
            load(Some("relative/dir"), &tls),
            Err(Error::InvalidConfig {
                key: MANIFESTS_KEY,
                ..
            })
        ));
        let missing = scratch.0.join("missing").display().to_string();
        assert_eq!(refusal(load(Some(&missing), &tls)).0, "directory");
        scratch.write("opencode", &opencode(&scratch));
        let file = scratch.0.join("opencode").display().to_string();
        assert_eq!(
            refusal(load(Some(&file), &tls)),
            (
                String::from("directory"),
                String::from("is not a directory")
            )
        );
        std::fs::write(scratch.manifests().join("notes.txt"), "x").unwrap();
        let (field, reason) = refusal(scratch.load());
        assert_eq!(field, "file");
        assert!(reason.contains("<name>.json"), "{reason}");
        std::fs::remove_file(scratch.manifests().join("notes.txt")).unwrap();
        // A link, even to a manifest that would pass, is never followed.
        let elsewhere = scratch.0.join("elsewhere.json");
        std::fs::rename(scratch.manifests().join("opencode.json"), &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, scratch.manifests().join("opencode.json")).unwrap();
        assert_eq!(
            refusal(scratch.load()),
            (String::from("file"), String::from("is not a regular file"))
        );
        std::fs::remove_file(scratch.manifests().join("opencode.json")).unwrap();
        std::fs::rename(&elsewhere, scratch.manifests().join("opencode.json")).unwrap();
        assert!(
            scratch.load().is_ok(),
            "the same manifest, in place, passes"
        );
        let big = scratch.manifests().join("big.json");
        std::fs::write(&big, vec![b' '; 64 * 1024 + 1]).unwrap();
        set_mode(&big, 0o644);
        let (field, reason) = refusal(scratch.load());
        assert_eq!(field, "file");
        assert!(reason.contains("is over 65536 bytes"), "{reason}");
        std::fs::write(&big, vec![b' '; 64 * 1024]).unwrap();
        assert_eq!(
            refusal(scratch.load()).0,
            "json",
            "at the bound, read whole"
        );
        std::fs::remove_file(&big).unwrap();
        for index in 0..MAX_MANIFESTS {
            let path = scratch.manifests().join(format!("m{index}.json"));
            std::fs::write(path, "{}").unwrap();
        }
        let (field, reason) = refusal(scratch.load());
        assert_eq!(field, "directory");
        assert!(reason.contains("more than 32"), "{reason}");
    }

    /// Each case is `manifest` with `pointer` set (or removed), refused
    /// naming `field`, its reason holding `reason`.
    fn refuses_each(
        scratch: &Scratch,
        good: &Value,
        cases: Vec<(&str, Option<Value>, &str, &str)>,
    ) {
        for (pointer, value, field, reason) in cases {
            let refused = refused_as(scratch, &with(good.clone(), pointer, value));
            assert_eq!(refused.0, field, "{pointer}: {refused:?}");
            assert!(refused.1.contains(reason), "{pointer}: {refused:?}");
        }
    }

    #[test]
    fn a_wrong_field_refuses_the_start_naming_it() {
        let scratch = Scratch::new("fields");
        let good = opencode(&scratch);
        let (other, other_sha) = scratch.program("other", b"other bytes");
        let gone = format!("{other}-gone");
        refuses_each(
            &scratch,
            &good,
            vec![
                ("/version", Some(json!(2)), "version", "the one format"),
                (
                    "/name",
                    Some(json!("OpenCode")),
                    "name",
                    "isn't a harness name",
                ),
                ("/name", Some(json!("other")), "name", "the file's name"),
                (
                    "/binary/sha256",
                    Some(json!(other_sha)),
                    "binary.sha256",
                    "hashes to",
                ),
                (
                    "/binary/sha256",
                    Some(json!("abc")),
                    "binary.sha256",
                    "64 hexadecimal",
                ),
                (
                    "/binary/path",
                    Some(json!("opencode")),
                    "binary.path",
                    "absolute",
                ),
                (
                    "/binary/path",
                    Some(json!(gone)),
                    "binary.path",
                    "unreadable",
                ),
                ("/api", Some(json!("gemini")), "api", "anthropic or openai"),
                (
                    "/destination",
                    Some(json!("a:1,b:2")),
                    "destination",
                    "one vendor API",
                ),
                (
                    "/destination",
                    Some(json!("")),
                    "destination",
                    "one vendor API",
                ),
                ("/unknown", Some(json!(1)), "json", "unknown field"),
                ("/args", None, "json", "missing field"),
            ],
        );
    }

    #[test]
    fn a_wrong_command_line_or_variable_refuses_the_start_naming_it() {
        let scratch = Scratch::new("command");
        let good = opencode(&scratch);
        refuses_each(
            &scratch,
            &good,
            vec![
                (
                    "/args/3",
                    Some(json!("go")),
                    "args",
                    "no {task} placeholder",
                ),
                ("/args/2", Some(json!("{task}")), "args", "more than once"),
                (
                    "/args/0",
                    Some(json!("--key={key}")),
                    "args",
                    "goes in the environment",
                ),
                ("/args/0", Some(json!("a\u{0}b")), "args", "NUL"),
                (
                    "/env/0/name",
                    Some(json!("PATH")),
                    "env",
                    "the cell's to set",
                ),
                (
                    "/env/0/name",
                    Some(json!("HATCHLING_X")),
                    "env",
                    "the cell's to set",
                ),
                (
                    "/env/0/name",
                    Some(json!("1BAD")),
                    "env",
                    "isn't a variable name",
                ),
                (
                    "/env/1/name",
                    Some(json!("ANTHROPIC_API_KEY")),
                    "env",
                    "set twice",
                ),
                (
                    "/env/1/value",
                    Some(json!("{task}")),
                    "env",
                    "goes in an argument",
                ),
                (
                    "/env/0/value",
                    Some(json!("fixed")),
                    "env",
                    "no {key} placeholder",
                ),
                (
                    "/env/1/value",
                    Some(json!("{}")),
                    "env",
                    "nothing points the CLI at its relay",
                ),
            ],
        );
        // The relay may be named in an argument instead.
        let relayed_in_args = with(
            with(good, "/env/1", None),
            "/args/0",
            Some(json!("--base={relay}")),
        );
        scratch.write("opencode", &relayed_in_args);
        assert!(scratch.load().is_ok());
    }

    #[test]
    fn a_manifest_names_no_built_in_harness_and_stays_within_bounds() {
        let scratch = Scratch::new("bounds");
        let good = opencode(&scratch);
        // A built-in harness's name is never a manifest's.
        let builtin = with(good.clone(), "/name", Some(json!("claude-code")));
        scratch.write("claude-code", &builtin);
        let (field, reason) = refusal(scratch.load());
        assert_eq!(field, "name");
        assert!(reason.contains("built-in"), "{reason}");
        std::fs::remove_file(scratch.manifests().join("claude-code.json")).unwrap();
        // Within bounds, and no more.
        let many: Vec<Value> = (0..=MAX_ARGS).map(|_| json!("x")).collect();
        let refused = refused_as(&scratch, &with(good.clone(), "/args", Some(json!(many))));
        assert_eq!(refused.0, "args");
        assert!(refused.1.contains("more than 64"), "{refused:?}");
        let long = "x".repeat(MAX_VALUE_BYTES + 1);
        let refused = refused_as(
            &scratch,
            &with(good.clone(), "/env/1/value", Some(json!(long))),
        );
        assert!(refused.1.contains("over 16384 bytes"), "{refused:?}");
        let vars: Vec<Value> = (0..=MAX_ENV)
            .map(|index| json!({"name": format!("V{index}"), "value": "{key}{relay}"}))
            .collect();
        let refused = refused_as(&scratch, &with(good, "/env", Some(json!(vars))));
        assert!(refused.1.contains("more than 64 variables"), "{refused:?}");
    }

    #[test]
    fn a_companion_is_pinned_at_a_name_of_its_own() {
        let scratch = Scratch::new("companions");
        let (path, sha256) = scratch.program("helper", b"helper bytes");
        let companion = |name: &str| json!({"path": path, "sha256": sha256, "name": name});
        let good = opencode(&scratch);
        for bad in ["agent", "usr", "a/b", "..", ".hidden", "sp ace", ""] {
            let manifest = with(good.clone(), "/companions", Some(json!([companion(bad)])));
            let (field, reason) = refused_as(&scratch, &manifest);
            assert_eq!(field, "companions[0].name", "{bad}");
            assert!(
                reason.contains("isn't a companion's name"),
                "{bad}: {reason}"
            );
        }
        let twice = with(
            good.clone(),
            "/companions",
            Some(json!([companion("helper"), companion("helper")])),
        );
        let (field, reason) = refused_as(&scratch, &twice);
        assert_eq!(field, "companions[1].name");
        assert!(reason.contains("named twice"), "{reason}");
        let changed = with(
            good.clone(),
            "/companions",
            Some(json!([{"path": path, "sha256": "0".repeat(64), "name": "helper"}])),
        );
        assert_eq!(refused_as(&scratch, &changed).0, "companions[0].sha256");
        let many: Vec<Value> = (0..=MAX_COMPANIONS)
            .map(|index| companion(&format!("c{index}")))
            .collect();
        let (field, reason) = refused_as(&scratch, &with(good, "/companions", Some(json!(many))));
        assert_eq!(field, "companions");
        assert!(reason.contains("more than 16"), "{reason}");
    }

    #[test]
    fn each_part_of_a_manifest_is_checked_on_its_own() {
        let strings = |items: &[&str]| {
            items
                .iter()
                .map(|item| (*item).to_owned())
                .collect::<Vec<_>>()
        };
        assert!(check_args(&strings(&["run", "{task}"])).is_ok());
        assert!(check_args(&strings(&["--base={relay}", "--", "do: {task}"])).is_ok());
        for bad in [
            strings(&["run"]),
            strings(&["{task}", "{task}"]),
            strings(&["{task}{task}"]),
            strings(&["{task}", "{key}"]),
        ] {
            assert!(check_args(&bad).is_err(), "{bad:?}");
        }
        let stored = |name: &str, value: &str| StoredVar {
            name: name.to_owned(),
            value: value.to_owned(),
        };
        assert_eq!(
            check_env(&[stored("API_KEY", "{key}"), stored("_X1", "v")]),
            Ok(vec![
                (String::from("API_KEY"), String::from("{key}")),
                (String::from("_X1"), String::from("v")),
            ])
        );
        for bad in [
            vec![stored("A", "v")],
            vec![stored("A", "{key}"), stored("A", "v")],
            vec![stored("HOME", "{key}")],
            vec![stored("TMPDIR", "{key}")],
            vec![stored("HATCHLING_ANY", "{key}")],
            vec![stored("A-B", "{key}")],
            vec![stored("A", "{key}"), stored("B", "{task}")],
            vec![stored("A", "{key}\u{0}")],
        ] {
            assert!(
                check_env(&bad).is_err(),
                "{:?}",
                bad.first().map(|stored| &stored.name)
            );
        }
        let scratch = Scratch::new("parts");
        let (path, sha256) = scratch.program("helper", b"helper bytes");
        let companion = |name: &str, sha256: &str| StoredCompanion {
            path: path.clone(),
            sha256: sha256.to_owned(),
            name: name.to_owned(),
        };
        let file = scratch.manifests().join("opencode.json");
        let checked = check_companions(&file, vec![companion("helper", &sha256)]).unwrap();
        assert_eq!(checked.len(), 1);
        assert_eq!(
            checked.first().map(|(_, name)| name.as_str()),
            Some("helper")
        );
        assert!(check_companions(&file, Vec::new()).unwrap().is_empty());
        assert!(matches!(
            check_companions(&file, vec![companion("liba", &"0".repeat(64))]),
            Err(Error::HarnessManifest { field, .. }) if field == "companions[0].sha256"
        ));
        assert!(matches!(
            check_companions(&file, vec![companion("tmp", &sha256)]),
            Err(Error::HarnessManifest { field, .. }) if field == "companions[0].name"
        ));
    }

    #[test]
    fn a_harness_name_is_a_short_plain_word() {
        let longest = "a".repeat(MAX_NAME_BYTES);
        for good in ["opencode", "a", "aider-2", &longest] {
            assert!(check_name(good).is_ok(), "{good}");
        }
        let longer = "a".repeat(MAX_NAME_BYTES + 1);
        for bad in ["", "2go", "-a", "a-", "Open", "a_b", "a.b", &longer] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }
}
