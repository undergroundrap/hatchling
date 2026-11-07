//! External harness skills: Hatchling drives, the harness
//! executes.
//!
//! A harness skill runs an external coding CLI (v0.1: Claude Code and the
//! Codex CLI) on a scoped task. Hatchling holds the memory, the plan and
//! the trust boundary; the harness only edits a copy of the files the task
//! names.
//!
//! - **Scoped inputs.** The agent names a project root (one of
//!   `HATCHLING_HARNESS_ROOTS`) and the files the task needs. Only those
//!   are copied into the run's work tree. The harness runs in a harness
//!   cell ([`CellConfig::is_harness_cell`]): no broker socket, no vault,
//!   nothing from the broker's environment, and only its work tree and
//!   scratch directory writable.
//! - **Structured results, computed by Hatchling.** Which files changed,
//!   and the patch, come from comparing the work tree with its pristine
//!   copy, never from what the harness says. The patch goes to the
//!   artifact workspace for review; nothing is written into the project.
//! - **Untrusted output.** What the harness says (its final message, and
//!   any question) is returned under `harness_said`, and reaches the
//!   model only inside the tool-result envelope.
//! - **No harness-to-harness invocation.** A harness cell has no socket
//!   to reach the broker with, and runs and subagents are served with no
//!   harnesses at all.
//! - **Pinned identity.** Each harness is configured as a path and its
//!   SHA-256, checked at start-up and again before every run, and run
//!   only from a verified copy; the hash is in the report and in the
//!   activity feed. A changed binary is refused, never substituted.
//! - **Clarification is a round trip.** A harness that needs an answer
//!   writes one question to `/scratch/question` and stops. The agent asks
//!   again with the answer in the brief.
//!
//! - **The real CLIs.** v0.1 ran single-process harnesses with
//!   no network; revises that, deliberately. A harness with a vendor
//!   API (`HATCHLING_HARNESS_<NAME>_API`) runs in a CLI cell: threads and
//!   child processes, a pinned set of tools, and one network, a relay on
//!   its own loopback that the egress authority serves
//!   ([`crate::harness_relay`]). Its login is a surrogate key: the vendor
//!   key stays in the vault (secret [`HarnessKind::secret_name`]), read by
//!   the authority for the run's relay, never given to the cell or the
//!   broker. A harness without an API runs offline, as before.
//! - **Harnesses by manifest** (WO-018). Beside the two built in, an
//!   operator may define a harness by a manifest
//!   ([`crate::harness_manifest`]): its binary and companions pinned, its
//!   API and destination, its command line and environment. It runs as a
//!   built-in harness does, every check the broker's.
//! - **Approved, pinned tools** (WO-019). A CLI cell's tools are the
//!   operator's approved tools ([`crate::approved_tools`]), each pinned to
//!   the SHA-256 it was approved with, and read when the broker starts.
//!   Every CLI cell runs a copy of them, in a directory of the broker's
//!   own, made at the first run that gets them from bytes that still hash
//!   to their pins: a write to a tool's path never reaches a cell. Before
//!   every such run, each tool is checked: one whose file changed is
//!   hashed again, and if it's another program the run is refused, saying
//!   which tool.
//! - **Held secrets are checked both ways.** What a harness is given goes
//!   to its vendor. So the task, the answer, the names and every byte of
//!   each file are checked before a cell starts, as parsed text, not the
//!   JSON they arrived in, and the patch and everything in the report
//!   after: [`HarnessVault::contains`], which covers every secret held,
//!   whether or not a cell has had it. With a keeper running, the keeper
//!   answers, and the broker never sees one ([`crate::held_check`]). A
//!   check that can't be answered stops the run, or the delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use hatchling_core::subagent::HarnessNetworkUse;
use hatchling_egress::{Allowlist, TlsClient};
use hatchling_vault::OpensStores;
use zeroize::Zeroizing;

use hatchling_core::subagent::{ActivityEvent, HarnessOutcome, SubagentEvent};
use hatchling_core::ui::operator::CellKind;
use hatchling_core::{CapabilityName, SkillOp, SkillResult};
use hatchling_scheduler::Timestamp;
use hatchling_skill::CodeHash;
use serde::{Deserialize, Serialize};

use crate::cell::{CellIds, HarnessDirs, HarnessNetwork};
use crate::environment as env;
use crate::harness_manifest::Manifest;
use crate::harness_relay::{RELAY_PORT, Relay, RunRelay, Vendor, Wire};
use crate::harness_tree::{TreeOp, change_trees};
use crate::skill_cell::VerifiedCode;
use crate::skill_run::{SkillRunError, SkillRuntime, run_cell};
use crate::{CellConfig, Config, Error};

/// The capability a harness skill call names.
pub const HARNESS_CAPABILITY: &str = "code";

/// Longest task text.
pub const MAX_TASK_BYTES: usize = 4096;

/// Longest answer to a harness's question.
pub const MAX_ANSWER_BYTES: usize = 2048;

/// Most files a brief may name.
pub const MAX_SLICE_FILES: usize = 64;

/// Most bytes the named files may hold together.
pub const MAX_SLICE_BYTES: u64 = 1024 * 1024;

/// Most files the work tree may hold after a run.
pub const MAX_WORK_FILES: usize = 1024;

/// Most bytes the work tree may hold after a run.
pub const MAX_WORK_BYTES: u64 = 8 * 1024 * 1024;

/// Most a harness may write on stdout.
pub const MAX_HARNESS_OUTPUT_BYTES: usize = 64 * 1024;

/// Most of the harness's question the report carries.
pub const MAX_QUESTION_BYTES: usize = 2048;

/// Most of the harness's final message the report carries.
pub const MAX_MESSAGE_BYTES: usize = 2048;

/// A run's default bound, spawn included.
pub const DEFAULT_HARNESS_TIMEOUT: Duration = Duration::from_secs(600);

/// The longest bound a deployment may set.
pub const MAX_HARNESS_TIMEOUT_SECS: u64 = 3600;

/// Where in its scratch directory a harness leaves a question.
const QUESTION_FILE: &str = "question";

/// The name the Codex CLI's code-mode host has beside the CLI: the CLI
/// looks for it next to its own path, which in its cell is `/agent`.
const CODEX_HOST: &str = "codex-code-mode-host";

/// The settings pinning the Codex CLI's code-mode host: its
/// default model has no tools without it.
const CODEX_HOST_KEY: &str = env::HATCHLING_HARNESS_CODEX_CLI_HOST.name;
const CODEX_HOST_HASH_KEY: &str = env::HATCHLING_HARNESS_CODEX_CLI_HOST_SHA256.name;

/// The setting naming a CLI cell's tools.
const TOOLS_KEY: &str = env::HATCHLING_HARNESS_TOOLS.name;

/// The tools a CLI cell gets unless `HATCHLING_HARNESS_TOOLS` names
/// others: a shell, bash, git, ripgrep and the coreutils a coding
/// CLI runs, each at its own path. One this host lacks is left out.
const DEFAULT_TOOLS: [&str; 46] = [
    "/bin/sh",
    "/bin/bash",
    "/usr/bin/bash",
    "/usr/bin/sh",
    "/usr/bin/env",
    "/usr/bin/git",
    "/usr/bin/rg",
    "/usr/bin/ls",
    "/usr/bin/cat",
    "/usr/bin/head",
    "/usr/bin/tail",
    "/usr/bin/wc",
    "/usr/bin/cut",
    "/usr/bin/tr",
    "/usr/bin/sort",
    "/usr/bin/uniq",
    "/usr/bin/mkdir",
    "/usr/bin/rm",
    "/usr/bin/rmdir",
    "/usr/bin/cp",
    "/usr/bin/mv",
    "/usr/bin/touch",
    "/usr/bin/echo",
    "/usr/bin/printf",
    "/usr/bin/pwd",
    "/usr/bin/basename",
    "/usr/bin/dirname",
    "/usr/bin/date",
    "/usr/bin/sleep",
    "/usr/bin/test",
    "/usr/bin/[",
    "/usr/bin/true",
    "/usr/bin/false",
    "/usr/bin/chmod",
    "/usr/bin/ln",
    "/usr/bin/readlink",
    "/usr/bin/realpath",
    "/usr/bin/stat",
    "/usr/bin/tee",
    "/usr/bin/mktemp",
    "/usr/bin/seq",
    "/usr/bin/od",
    "/usr/bin/expr",
    "/usr/bin/id",
    "/usr/bin/uname",
    "/usr/bin/nproc",
];

/// How long a run's relay outlives its timeout, so an answer in flight
/// when the cell is stopped ends on its own.
const RELAY_GRACE: Duration = Duration::from_secs(30);

/// What a harness run needs from the vault.
pub trait HarnessVault {
    /// A harness's vendor key, the vault secret it names, for its relay,
    /// in-process, with no egress authority running: with one, the
    /// authority reads it.
    fn key(&self, secret: &str) -> Option<Zeroizing<String>>;

    /// Whether `text` holds any secret Hatchling keeps, by the rules of
    /// [`crate::held_check`]: what a run may be given, or may hand back.
    ///
    /// # Errors
    ///
    /// Why that couldn't be told, which is treated as holding one: the run
    /// doesn't start, and nothing it wrote is delivered.
    fn contains(&self, text: &[u8]) -> Result<bool, String>;
}

/// A vault for a run that can't hold anything: a key lookup alone.
impl<F: Fn(&str) -> Option<Zeroizing<String>>> HarnessVault for F {
    fn key(&self, secret: &str) -> Option<Zeroizing<String>> {
        self(secret)
    }

    fn contains(&self, _text: &[u8]) -> Result<bool, String> {
        Ok(false)
    }
}

/// The vault a harness run reads.
pub type HarnessKeys<'a> = &'a dyn HarnessVault;

/// What stands in for anything a run wrote that holds a secret.
const WITHHELD: &str = "[withheld: this would expose a secret Hatchling holds]";

/// Why a check stops a run before it starts.
#[derive(Debug, PartialEq, Eq)]
enum Stopped {
    /// What it was given holds a secret: what the report says.
    Holds(String),
    /// The check couldn't be answered: the detail is in the broker's log.
    Unchecked,
}

/// Whether `text` holds a secret; one that can't be told stops the run.
fn holding(keys: HarnessKeys<'_>, text: &[u8]) -> Result<bool, Stopped> {
    keys.contains(text).map_err(|reason| {
        tracing::error!(%reason, "a harness run stopped: its secrets check wasn't answered");
        Stopped::Unchecked
    })
}

/// The activity thread every harness run is recorded on.
pub const HARNESS_RUN_ID: &str = "harness";

/// The harnesses v0.1 knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessKind {
    /// Anthropic's Claude Code CLI.
    ClaudeCode,
    /// `OpenAI`'s Codex CLI.
    CodexCli,
}

impl HarnessKind {
    /// Every harness.
    pub const ALL: [Self; 2] = [Self::ClaudeCode, Self::CodexCli];

    /// The harness skill's name, as grants name it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::CodexCli => "codex-cli",
        }
    }

    /// The harness skill's id, as the agent invokes it.
    #[must_use]
    pub const fn skill_id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code@1",
            Self::CodexCli => "codex-cli@1",
        }
    }

    /// The harness whose skill id is `id`.
    #[must_use]
    pub fn from_skill_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.skill_id() == id)
    }

    /// The setting that names the harness binary.
    const fn path_key(self) -> &'static str {
        match self {
            Self::ClaudeCode => env::HATCHLING_HARNESS_CLAUDE_CODE.name,
            Self::CodexCli => env::HATCHLING_HARNESS_CODEX_CLI.name,
        }
    }

    /// The setting that pins the binary's SHA-256.
    const fn hash_key(self) -> &'static str {
        match self {
            Self::ClaudeCode => env::HATCHLING_HARNESS_CLAUDE_CODE_SHA256.name,
            Self::CodexCli => env::HATCHLING_HARNESS_CODEX_CLI_SHA256.name,
        }
    }

    /// The setting that names the harness's vendor API.
    const fn api_key(self) -> &'static str {
        match self {
            Self::ClaudeCode => env::HATCHLING_HARNESS_CLAUDE_CODE_API.name,
            Self::CodexCli => env::HATCHLING_HARNESS_CODEX_CLI_API.name,
        }
    }

    /// The API its relay speaks.
    const fn wire(self) -> Wire {
        match self {
            Self::ClaudeCode => Wire::Anthropic,
            Self::CodexCli => Wire::OpenAi,
        }
    }

    /// The vault secret holding its vendor key: the egress authority
    /// reads it for the harness's relay, and no vault grant may name it.
    #[must_use]
    pub const fn secret_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "harness-claude-code",
            Self::CodexCli => "harness-codex-cli",
        }
    }

    /// The surrogate key's prefix: the shape each CLI expects of a key.
    const fn surrogate_prefix(self) -> &'static str {
        match self {
            Self::ClaudeCode => "sk-ant-hatchling-",
            Self::CodexCli => "sk-hatchling-",
        }
    }

    /// The harness's arguments for `prompt`, as verified against the real
    /// binaries (Claude Code 2.1.286, the Codex CLI 0.162.0): the
    /// non-interactive mode, machine-readable output, the edit and shell
    /// tools allowed without prompting (the cell is the sandbox; Claude
    /// Code refuses its own bypass as root, and the Codex CLI's sandbox
    /// would need namespaces its cell cannot make), and, for the Codex CLI,
    /// its relay as a model provider (it ignores `OPENAI_BASE_URL`) and no
    /// git repository required.
    #[must_use]
    pub fn args(self, prompt: &str, relay: u16) -> Vec<String> {
        let owned = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        match self {
            Self::ClaudeCode => {
                let mut args = owned(&["-p", prompt, "--output-format", "json"]);
                args.extend(owned(&[
                    "--permission-mode",
                    "acceptEdits",
                    "--allowedTools",
                    "Bash,Edit,Write,Read,Glob,Grep",
                ]));
                args
            }
            Self::CodexCli => {
                let base_url =
                    format!("model_providers.hatchling.base_url=\"http://127.0.0.1:{relay}/v1\"");
                let mut args = owned(&[
                    "exec",
                    "--skip-git-repo-check",
                    "--ephemeral",
                    "--ignore-user-config",
                    "--json",
                    "--dangerously-bypass-approvals-and-sandbox",
                    "-c",
                    "model_provider=hatchling",
                    "-c",
                    "model_providers.hatchling.name=\"hatchling\"",
                    "-c",
                ]);
                args.push(base_url);
                args.extend(owned(&[
                    "-c",
                    "model_providers.hatchling.env_key=\"HATCHLING_HARNESS_KEY\"",
                    "-c",
                    "model_providers.hatchling.wire_api=\"responses\"",
                    "-c",
                    "model_providers.hatchling.supports_websockets=false",
                    prompt,
                ]));
                args
            }
        }
    }

    /// A CLI's environment for its relay on `relay` and its surrogate key
    ///: the relay's URL, the key, the tools' `PATH`, and no
    /// update, telemetry or error report traffic, which could only fail.
    #[must_use]
    pub fn env(self, relay: u16, surrogate: &str) -> Vec<(String, String)> {
        let pair = |key: &str, value: &str| (key.to_owned(), value.to_owned());
        let mut env = vec![pair("PATH", "/usr/bin:/bin"), pair("LANG", "C.UTF-8")];
        match self {
            Self::ClaudeCode => env.extend([
                pair("ANTHROPIC_BASE_URL", &format!("http://127.0.0.1:{relay}")),
                pair("ANTHROPIC_API_KEY", surrogate),
                pair("DISABLE_AUTOUPDATER", "1"),
                pair("DISABLE_TELEMETRY", "1"),
                pair("DISABLE_ERROR_REPORTING", "1"),
                pair("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ]),
            Self::CodexCli => env.push(pair("HATCHLING_HARNESS_KEY", surrogate)),
        }
        env
    }
}

/// The vault secret holding the vendor key of the harness named `name`:
/// `harness-<name>`, for the built-in harnesses and manifests alike.
#[must_use]
pub fn secret_name(name: &str) -> String {
    format!("harness-{name}")
}

/// A configured harness: one built in, or one an operator's manifest
/// defines ([`crate::harness_manifest`], WO-018). Both run the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Harness {
    /// Claude Code or the Codex CLI.
    BuiltIn(HarnessKind),
    /// A CLI the operator's manifest defines.
    Defined(Arc<Manifest>),
}

impl Harness {
    /// The harness skill's name, as grants name it.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::BuiltIn(kind) => kind.name(),
            Self::Defined(manifest) => manifest.name(),
        }
    }

    /// The harness skill's id, as the agent invokes it.
    #[must_use]
    pub fn skill_id(&self) -> &str {
        match self {
            Self::BuiltIn(kind) => kind.skill_id(),
            Self::Defined(manifest) => manifest.skill_id(),
        }
    }

    /// The vault secret holding its vendor key.
    #[must_use]
    pub fn secret_name(&self) -> String {
        secret_name(self.name())
    }

    /// The surrogate key's prefix: the shape a key of its API has.
    fn surrogate_prefix(&self) -> &'static str {
        match self {
            Self::BuiltIn(kind) => kind.surrogate_prefix(),
            Self::Defined(manifest) => match manifest.wire() {
                Wire::Anthropic => HarnessKind::ClaudeCode.surrogate_prefix(),
                Wire::OpenAi => HarnessKind::CodexCli.surrogate_prefix(),
            },
        }
    }

    /// Its pinned companion files, each with the name the cell sees it
    /// at: a manifest's own; the Codex host is the broker's setting.
    fn companions(&self) -> &[(PinnedBinary, String)] {
        match self {
            Self::BuiltIn(_) => &[],
            Self::Defined(manifest) => manifest.companions(),
        }
    }

    /// Its arguments for `prompt`, its relay on `relay`.
    #[must_use]
    pub fn args(&self, prompt: &str, relay: u16) -> Vec<String> {
        match self {
            Self::BuiltIn(kind) => kind.args(prompt, relay),
            Self::Defined(manifest) => manifest.args(prompt, relay),
        }
    }

    /// Its environment for its relay on `relay` and its surrogate key: a
    /// manifest's variables after the tools' `PATH` and, unless it sets
    /// its own, `LANG`.
    #[must_use]
    pub fn env(&self, relay: u16, surrogate: &str) -> Vec<(String, String)> {
        match self {
            Self::BuiltIn(kind) => kind.env(relay, surrogate),
            Self::Defined(manifest) => {
                let own = manifest.env(relay, surrogate);
                let mut env = vec![(String::from("PATH"), String::from("/usr/bin:/bin"))];
                if !own.iter().any(|(name, _)| name == "LANG") {
                    env.push((String::from("LANG"), String::from("C.UTF-8")));
                }
                env.extend(own);
                env
            }
        }
    }
}

/// A harness binary and the SHA-256 it is pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedBinary {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
}

impl PinnedBinary {
    /// The program at `path`, pinned to `sha256`, if it hashes to it now;
    /// otherwise the field that's wrong (`path` or `sha256`) and why.
    pub(crate) fn checked(path: &str, sha256: &str) -> Result<Self, (&'static str, String)> {
        let path = PathBuf::from(path.trim());
        let sha256 = sha256.trim().to_ascii_lowercase();
        if !path.is_absolute() {
            return Err((
                "path",
                format!("must be an absolute path: {}", path.display()),
            ));
        }
        if !is_sha256(&sha256) {
            return Err(("sha256", String::from("a SHA-256 is 64 hexadecimal digits")));
        }
        let bytes = read_regular(&path).map_err(|reason| ("path", reason))?;
        let actual = CodeHash::of(&bytes).to_string();
        if actual != sha256 {
            return Err((
                "sha256",
                format!("the program at {} hashes to {actual}", path.display()),
            ));
        }
        Ok(Self { path, sha256 })
    }

    /// The binary's bytes, if they still hash to the pinned value.
    ///
    /// # Errors
    ///
    /// A sentence for the report: the binary is unreadable, a symlink, or
    /// no longer the pinned one.
    fn verified(&self) -> Result<Vec<u8>, String> {
        let bytes =
            read_regular(&self.path).map_err(|reason| format!("the harness binary {reason}"))?;
        let actual = CodeHash::of(&bytes).to_string();
        if actual == self.sha256 {
            Ok(bytes)
        } else {
            Err(format!(
                "the harness binary's SHA-256 is {actual}, not the pinned {}: it is never substituted",
                self.sha256
            ))
        }
    }
}

/// What a run's report says when its binary can't be read or no longer
/// hashes to its pin; the detail stays in the broker's log.
const BINARY_REFUSED: &str =
    "the harness binary is unreadable or not the pinned one: it is never substituted";

/// What a run's report says when the Codex host binary can't be read or
/// no longer hashes to its pin.
const HOST_REFUSED: &str =
    "the Codex host binary is unreadable or not the pinned one: it is never substituted";

/// What a run's report says when a manifest's companion file can't be
/// read or no longer hashes to its pin.
const COMPANION_REFUSED: &str =
    "a companion file of the harness is unreadable or not the pinned one: it is never substituted";

/// The most a tool may hold: a program, never a file that would fill the
/// broker's memory as it's read.
const MAX_TOOL_BYTES: u64 = 256 * 1024 * 1024;

/// A program a CLI cell may run, as configured: its host path, and the
/// name the cell sees it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tool {
    path: PathBuf,
    name: String,
}

impl Tool {
    /// The tool at `path`: an absolute path of plain names, with no
    /// control character. Nothing is opened.
    ///
    /// # Errors
    ///
    /// Why `path` can't name a tool.
    pub(crate) fn at(path: &str) -> Result<Self, String> {
        let program = PathBuf::from(path);
        let plain = program.is_absolute()
            && program
                .components()
                .skip(1)
                .all(|component| matches!(component, Component::Normal(_)));
        if !plain {
            return Err(format!("{} is not an absolute path", path.escape_debug()));
        }
        if path.chars().any(char::is_control) {
            return Err(format!("{} holds a control character", path.escape_debug()));
        }
        Ok(Self {
            path: program,
            name: path.trim_start_matches('/').to_owned(),
        })
    }

    /// The name a cell sees the tool at: its path, without the leading `/`.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// What a tool's path named: the file, and when it last changed. The
/// kernel sets a file's change time on every write to it, and only root
/// can set it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Seen {
    dev: u64,
    ino: u64,
    size: u64,
    ctime: (i64, i64),
}

impl Seen {
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

/// What each of `tools` holds now: its SHA-256 and what its path names,
/// or why it can't be read. A file named more than once (coreutils is one
/// program under many names) is read once.
pub(crate) fn read_tools(tools: &[Tool]) -> Vec<Result<(String, Seen), String>> {
    let mut hashed: BTreeMap<(u64, u64), (Seen, String)> = BTreeMap::new();
    tools
        .iter()
        .map(|tool| {
            let (file, meta) = open_program(&tool.path)?;
            let seen = Seen::of(&meta);
            if let Some((_, sha256)) = hashed
                .get(&(seen.dev, seen.ino))
                .filter(|(was, _)| *was == seen)
            {
                return Ok((sha256.clone(), seen));
            }
            let bytes = read_file(file, &tool.path, MAX_TOOL_BYTES)?;
            let sha256 = CodeHash::of(&bytes).to_string();
            hashed.insert((seen.dev, seen.ino), (seen, sha256.clone()));
            Ok((sha256, seen))
        })
        .collect()
}

/// A tool as the broker pinned it: the SHA-256 it was approved with, and
/// what its path named when the broker found it that program; none when
/// it didn't, so every run reads it again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PinnedTool {
    tool: Tool,
    sha256: String,
    seen: Option<Seen>,
}

impl PinnedTool {
    /// Whether the tool is still the program pinned. One whose path names
    /// the file it did, unchanged since, is; any other is hashed again.
    ///
    /// # Errors
    ///
    /// What the run's report says, naming the tool, and the detail for
    /// the broker's log: the tool is unreadable, or another program.
    fn check(&self) -> Result<(), (String, String)> {
        let path = &self.tool.path;
        let unchanged = self.seen.is_some_and(|seen| {
            std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && Seen::of(&meta) == seen)
        });
        if unchanged {
            return Ok(());
        }
        let (bytes, _) = read_program(path).map_err(|detail| (self.refusal(), detail))?;
        let actual = CodeHash::of(&bytes).to_string();
        if actual == self.sha256 {
            Ok(())
        } else {
            Err((self.refusal(), self.differs(&actual)))
        }
    }

    /// What a run's report says when this tool isn't the program pinned.
    fn refusal(&self) -> String {
        format!(
            "the tool {} is unreadable or not the program approved: it is never substituted",
            self.tool.path.display()
        )
    }

    /// The broker log's detail when this tool hashes to `actual`.
    fn differs(&self, actual: &str) -> String {
        format!(
            "the tool {}'s SHA-256 is {actual}, not the approved {}",
            self.tool.path.display(),
            self.sha256
        )
    }
}

/// A CLI cell's tools, pinned when the broker starts, and the copy every
/// CLI cell runs them from, made at the first run that gets them: by
/// then the broker runs as its own user (WO-017), whose the copy must be.
#[derive(Debug)]
pub(crate) struct PinnedTools {
    tools: Vec<PinnedTool>,
    copy: std::sync::Mutex<Option<Arc<ToolCopy>>>,
}

impl PinnedTools {
    /// Pins each of `approved`, a tool and the SHA-256 it was approved
    /// with, given what [`read_tools`] found it holds now. One that isn't
    /// the program approved, or can't be read, is pinned all the same, and
    /// logged: every run that would get it is refused until it is.
    fn pin(approved: Vec<(Tool, String)>, read: Vec<Result<(String, Seen), String>>) -> Self {
        let tools = approved
            .into_iter()
            .zip(read)
            .map(|((tool, sha256), read)| {
                let mut pinned = PinnedTool {
                    tool,
                    sha256,
                    seen: None,
                };
                let detail = match read {
                    Ok((actual, seen)) if actual == pinned.sha256 => {
                        pinned.seen = Some(seen);
                        return pinned;
                    }
                    Ok((actual, _)) => pinned.differs(&actual),
                    Err(detail) => detail,
                };
                tracing::warn!(
                    tool = %pinned.tool.path.display(),
                    detail,
                    "a tool isn't the program approved: a run that would get it is refused \
                     until it is, or it's approved again (tools add) and the broker restarts"
                );
                pinned
            })
            .collect();
        Self {
            tools,
            copy: std::sync::Mutex::new(None),
        }
    }

    /// Whether every tool is still the program pinned.
    ///
    /// # Errors
    ///
    /// As [`PinnedTool::check`], for the first tool that isn't.
    fn check(&self) -> Result<(), (String, String)> {
        self.tools.iter().try_for_each(PinnedTool::check)
    }

    /// The copy cells run the tools from: made the first time, and the
    /// same one after.
    ///
    /// # Errors
    ///
    /// As [`ToolCopy::take`]: the next run tries again.
    fn copy(&self) -> Result<Arc<ToolCopy>, ToolsRefused> {
        let mut kept = self
            .copy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(copy) = kept.as_ref() {
            return Ok(Arc::clone(copy));
        }
        let copy = Arc::new(ToolCopy::take(&self.tools)?);
        *kept = Some(Arc::clone(&copy));
        drop(kept);
        Ok(copy)
    }
}

/// Why a run wasn't given its tools.
#[derive(Debug, PartialEq, Eq)]
enum ToolsRefused {
    /// The broker didn't pin them at its start.
    Unpinned,
    /// A tool changed or went: what the report says, and the detail for
    /// the broker's log.
    Changed(String, String),
    /// Their copy couldn't be made: the broker's failure.
    Failed(String),
}

/// The tools' copy: a directory of the broker's own, holding one file per
/// program, read and execute only, and each other name the program has as
/// a hard link to it. Removed when dropped. It's named as a staging
/// directory, so one a broker leaves is the next one's sweep's.
#[derive(Debug)]
struct ToolCopy {
    staging: PathBuf,
    dir: PathBuf,
    names: Vec<String>,
}

impl ToolCopy {
    /// Copies `pinned`, each program from bytes that still hash to its pin.
    ///
    /// # Errors
    ///
    /// A tool that changed or went since it was pinned, or a copy that
    /// can't be made.
    fn take(pinned: &[PinnedTool]) -> Result<Self, ToolsRefused> {
        let made = |error: std::io::Error| {
            ToolsRefused::Failed(format!("cannot make the tools' copy: {error}"))
        };
        let random = crate::approval::random_hex("").map_err(made)?;
        let staging = std::env::temp_dir().join(format!(
            "hatchling-staging-{}-{}",
            std::process::id(),
            random.chars().take(32).collect::<String>()
        ));
        // Searchable by all, listed and written by the broker alone, as the
        // browser's snapshot is: a cell's setup opens what it binds from
        // here where the broker's user may not be mapped.
        std::fs::DirBuilder::new()
            .mode(0o711)
            .create(&staging)
            .map_err(made)?;
        // Held from here: a failure below removes what was copied.
        let copy = Self {
            dir: staging.join("tools"),
            staging,
            names: pinned
                .iter()
                .map(|pinned| pinned.tool.name.clone())
                .collect(),
        };
        set_mode(&copy.staging, 0o711).map_err(made)?;
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&copy.dir)
            .map_err(made)?;
        set_mode(&copy.dir, 0o755).map_err(made)?;
        let mut copied: BTreeMap<&str, &str> = BTreeMap::new();
        for pinned in pinned {
            let name = pinned.tool.name.as_str();
            if let Some(first) = copied.get(pinned.sha256.as_str()) {
                copy.link(first, name).map_err(made)?;
                continue;
            }
            let path = &pinned.tool.path;
            let told = || pinned.refusal();
            let (bytes, _) =
                read_program(path).map_err(|detail| ToolsRefused::Changed(told(), detail))?;
            let actual = CodeHash::of(&bytes).to_string();
            if actual != pinned.sha256 {
                return Err(ToolsRefused::Changed(told(), pinned.differs(&actual)));
            }
            copy.add(name, &bytes).map_err(made)?;
            copied.insert(pinned.sha256.as_str(), name);
        }
        Ok(copy)
    }

    /// Writes `bytes` at `name` in the copy, read and execute only.
    fn add(&self, name: &str, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write as _;
        let path = self.place(name)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o555)
            .open(&path)?;
        file.write_all(bytes)?;
        set_mode(&path, 0o555)
    }

    /// Gives the program at `first` in the copy the name `name` too.
    fn link(&self, first: &str, name: &str) -> std::io::Result<()> {
        let path = self.place(name)?;
        std::fs::hard_link(self.dir.join(first), path)
    }

    /// Where `name` goes in the copy, its directories made, each `0755`
    /// whatever the umask.
    fn place(&self, name: &str) -> std::io::Result<PathBuf> {
        let mut dir = self.dir.clone();
        let parts: Vec<_> = Path::new(name).components().collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            dir.push(part);
            match std::fs::DirBuilder::new().mode(0o755).create(&dir) {
                Ok(()) => set_mode(&dir, 0o755)?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Ok(self.dir.join(name))
    }

    /// The tools a run's cell is given: each one's copy, at its own name.
    /// Never a tool's host path.
    fn for_cell(&self) -> Vec<(PathBuf, String)> {
        self.names
            .iter()
            .map(|name| (self.dir.join(name), name.clone()))
            .collect()
    }
}

impl Drop for ToolCopy {
    fn drop(&mut self) {
        // Through `remove_tree`: nothing outside the copy goes.
        let _ = hatchling_sys::remove_dir_tree(&self.staging);
    }
}

/// Sets `path`'s mode, whatever the umask.
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// The program at `path`, through its links (`/bin/sh` is one on most
/// hosts), open to read, and what it is.
///
/// # Errors
///
/// Why it can't be: missing, or not a regular file.
fn open_program(path: &Path) -> Result<(std::fs::File, std::fs::Metadata), String> {
    let unreadable = |err: std::io::Error| format!("{} is unreadable: {err}", path.display());
    let file = hatchling_sys::open_regular(path)
        .map_err(unreadable)?
        .ok_or_else(|| format!("{} is not a regular file", path.display()))?;
    let meta = file.metadata().map_err(unreadable)?;
    Ok((file, meta))
}

/// The program at `path`, as [`open_program`] opens it, read whole if it
/// holds at most [`MAX_TOOL_BYTES`].
///
/// # Errors
///
/// As [`open_program`] and [`read_file`].
fn read_program(path: &Path) -> Result<(Vec<u8>, Seen), String> {
    let (file, meta) = open_program(path)?;
    Ok((read_file(file, path, MAX_TOOL_BYTES)?, Seen::of(&meta)))
}

/// `file`, opened at `path`, if it holds at most `limit` bytes.
///
/// # Errors
///
/// It can't be read, or holds more.
fn read_file(file: std::fs::File, path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| format!("{} is unreadable: {err}", path.display()))?;
    if bytes.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
        return Err(format!("{} is more than {limit} bytes", path.display()));
    }
    Ok(bytes)
}

/// Up to `limit` bytes of the file at `path`, and whether it holds more.
///
/// A file a cell wrote is never read whole: its size is the cell's to
/// choose (up to its file-size limit, 8 GiB by default), and reading it
/// all before checking a bound would let it exhaust the broker's memory.
fn read_prefix(path: &Path, limit: u64) -> std::io::Result<(Vec<u8>, bool)> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let keep = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = bytes.len() > keep;
    bytes.truncate(keep);
    Ok((bytes, more))
}

/// [`read_prefix`] of `path`, a regular file and not a symlink.
fn read_regular_prefix(path: &Path, limit: u64) -> Result<(Vec<u8>, bool), String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|err| format!("{} is unreadable: {err}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    read_prefix(path, limit).map_err(|err| format!("{} is unreadable: {err}", path.display()))
}

/// Reads `path`, a regular file and not a symlink.
fn read_regular(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|err| format!("{} is unreadable: {err}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    std::fs::read(path).map_err(|err| format!("{} is unreadable: {err}", path.display()))
}

/// The broker's harness skills: what is configured, and how each runs.
#[derive(Debug, Clone, Default)]
pub struct Harnesses {
    /// Each configured harness, by name, and its pinned binary.
    binaries: BTreeMap<String, (Harness, PinnedBinary)>,
    /// Each networked harness's vendor, by name.
    vendors: BTreeMap<String, Vendor>,
    tools: Vec<Tool>,
    /// Whether `HATCHLING_HARNESS_TOOLS` names the tools.
    named: bool,
    /// The approved-tools list's directory ([`crate::approved_tools`]).
    approved: PathBuf,
    pinned: Option<Arc<PinnedTools>>,
    codex_host: Option<PinnedBinary>,
    granted: BTreeSet<String>,
    roots: BTreeMap<String, PathBuf>,
    timeout: Duration,
    workspace: Option<PathBuf>,
    activity_log: Option<PathBuf>,
    /// Where a harness cell's ids come from (`HATCHLING_CELL_IDS`).
    ids: CellIds,
}

impl Harnesses {
    /// The harnesses the environment configures, checked against
    /// `config`.
    ///
    /// # Errors
    ///
    /// As [`Harnesses::parse`].
    pub fn from_env(config: &Config) -> Result<Self, Error> {
        Self::parse(config, |key| std::env::var(key).ok())
    }

    /// The harnesses `get` configures, checked against `config`.
    ///
    /// - `HATCHLING_HARNESS_CLAUDE_CODE` / `HATCHLING_HARNESS_CODEX_CLI`:
    ///   the binary's absolute path, with its SHA-256 in the matching
    ///   `..._SHA256` setting. The binary must hash to it now.
    /// - `HATCHLING_HARNESS_ROOTS`: `name=/absolute/dir,...`, the project
    ///   roots a brief may name. Required with any harness.
    /// - `HATCHLING_HARNESS_TIMEOUT_SECS`: 1 to 3600, default 600.
    /// - `HATCHLING_HARNESS_CLAUDE_CODE_API` / `HATCHLING_HARNESS_CODEX_CLI_API`
    ///   (the harness's API): the vendor API, `host:port` over HTTPS, or a
    ///   local endpoint `local:localhost:PORT`; resolved and pinned now.
    ///   Its key is the vault secret [`HarnessKind::secret_name`].
    /// - `HATCHLING_HARNESS_TOOLS`: the programs a CLI cell may run,
    ///   absolute paths, comma-separated; by default a shell, bash, git,
    ///   ripgrep and the usual coreutils.
    /// - `HATCHLING_HARNESS_CODEX_CLI_HOST` and its `_SHA256`: the Codex
    ///   CLI's code-mode host, pinned like a harness binary.
    /// - `HATCHLING_HARNESS_MANIFESTS`: a directory of manifests, each a
    ///   harness of the operator's ([`crate::harness_manifest`]).
    ///
    /// A harness runs only with its skill grant (`HATCHLING_SKILL_GRANTS`,
    /// e.g. `claude-code:code`), and needs `HATCHLING_ARTIFACT_WORKSPACE`
    /// for its patches. Runs are recorded in the activity log when
    /// scheduling is on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] for a malformed setting, a path
    /// without its hash (or the reverse), a binary that does not hash to
    /// its pin, a bad root, or a harness with no workspace or no roots;
    /// [`Error::HarnessManifest`] for a manifest that's wrong or open to
    /// another user.
    pub fn parse(config: &Config, get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let mut binaries = BTreeMap::new();
        for kind in HarnessKind::ALL {
            if let Some(binary) = parse_binary(
                kind,
                get(kind.path_key()).as_deref(),
                get(kind.hash_key()).as_deref(),
            )? {
                binaries.insert(kind.name().to_owned(), (Harness::BuiltIn(kind), binary));
            }
        }
        let tls = TlsClient::bundled()
            .map_err(|source| invalid("HATCHLING_EGRESS_CA_BUNDLE", source.to_string()))?;
        let mut vendors = BTreeMap::new();
        for kind in HarnessKind::ALL {
            if let Some(vendor) = parse_vendor(kind, get(kind.api_key()).as_deref(), &tls)? {
                if !binaries.contains_key(kind.name()) {
                    return Err(invalid(
                        kind.api_key(),
                        String::from("a vendor API needs its harness binary"),
                    ));
                }
                vendors.insert(kind.name().to_owned(), vendor);
            }
        }
        let manifests = crate::harness_manifest::load(
            get(crate::harness_manifest::MANIFESTS_KEY).as_deref(),
            &tls,
        )?;
        for (manifest, vendor) in manifests {
            let name = manifest.name().to_owned();
            refuse_key_grants(config, &name)?;
            let binary = manifest.binary().clone();
            binaries.insert(name.clone(), (Harness::Defined(Arc::new(manifest)), binary));
            vendors.insert(name, vendor);
        }
        let codex_host = parse_pin(
            (CODEX_HOST_KEY, CODEX_HOST_HASH_KEY),
            get(CODEX_HOST_KEY).as_deref(),
            get(CODEX_HOST_HASH_KEY).as_deref(),
        )?;
        if codex_host.is_some() && !binaries.contains_key(HarnessKind::CodexCli.name()) {
            return Err(invalid(
                CODEX_HOST_KEY,
                String::from("the code-mode host needs the Codex CLI"),
            ));
        }
        let named = get(TOOLS_KEY).is_some_and(|raw| !raw.trim().is_empty());
        let tools = parse_tools(get(TOOLS_KEY).as_deref(), !vendors.is_empty())?;
        let roots = parse_roots(get(env::HATCHLING_HARNESS_ROOTS.name).as_deref(), config)?;
        let timeout = parse_timeout(get(env::HATCHLING_HARNESS_TIMEOUT_SECS.name).as_deref())?;
        let ids = CellIds::parse(&get)?;
        if !binaries.is_empty() {
            if roots.is_empty() {
                return Err(invalid(
                    "HATCHLING_HARNESS_ROOTS",
                    String::from("a harness needs at least one project root"),
                ));
            }
            if config.artifacts.workspace.is_none() {
                return Err(invalid(
                    "HATCHLING_ARTIFACT_WORKSPACE",
                    String::from(
                        "a harness's patches go to the artifact workspace, which is not set",
                    ),
                ));
            }
        }
        let capability = harness_capability();
        let granted =
            binaries
                .keys()
                .filter(|name| {
                    config.skills.grants.get(name.as_str()).is_some_and(|caps| {
                        capability.as_ref().is_some_and(|cap| caps.contains(cap))
                    })
                })
                .cloned()
                .collect();
        Ok(Self {
            binaries,
            vendors,
            tools,
            named,
            approved: crate::approved_tools::dir(config),
            pinned: None,
            codex_host,
            granted,
            roots,
            timeout,
            workspace: config.artifacts.workspace.clone(),
            activity_log: config
                .scheduler
                .enabled()
                .then(|| config.scheduler.activity_log()),
            ids,
        })
    }

    /// The same harnesses, reaching their vendors through `tls`: the
    /// broker's egress root store, with the operator's roots.
    #[must_use]
    pub fn with_tls(mut self, tls: &TlsClient) -> Self {
        for vendor in self.vendors.values_mut() {
            vendor.tls = tls.clone();
        }
        self
    }

    /// The same harnesses, their tools the approved ones, pinned to the
    /// SHA-256 each was approved with (WO-019). The broker does this once,
    /// at its start, when a harness has a network. A run checks each tool
    /// is still the program approved, and its cell runs a copy of them,
    /// made from bytes that hash to their pins.
    ///
    /// The tools are the approved-tools list's ([`crate::approved_tools`]).
    /// With no list yet, they're the tools `HATCHLING_HARNESS_TOOLS` names,
    /// or the defaults this host has, and the list is made of them, each
    /// hashed now: the cells get what they did before.
    ///
    /// # Errors
    ///
    /// [`Error::ApprovedTools`] when the list can't be read or made, or
    /// is open to another user; [`Error::InvalidConfig`] naming
    /// `HATCHLING_HARNESS_TOOLS`, when it names other tools than the list,
    /// or, with no list, a tool that can't be read.
    pub fn pin_tools(mut self) -> Result<Self, Error> {
        use crate::approved_tools::{Approved, read, write};
        if self.vendors.is_empty() {
            return Ok(self);
        }
        let (approved, found) = if let Some(approved) = read(&self.approved)? {
            if self.named && !approved.names_the_same(&self.tools) {
                return Err(invalid(
                    TOOLS_KEY,
                    format!(
                        "names other tools than the approved-tools list in {}: change the list \
                         with `tools add` or `tools remove`, and unset {TOOLS_KEY}",
                        self.approved.display()
                    ),
                ));
            }
            let found = read_tools(&approved.tools());
            (approved, found)
        } else {
            let found = read_tools(&self.tools);
            let approved = Approved::of(&self.tools, &found, crate::approved_tools::now())
                .map_err(|reason| invalid(TOOLS_KEY, reason))?;
            write(&self.approved, &approved)?;
            tracing::info!(
                dir = %self.approved.display(),
                tools = self.tools.len(),
                "the approved-tools list is made, of the tools the broker was given"
            );
            (approved, found)
        };
        self.tools = approved.tools();
        self.pinned = Some(Arc::new(PinnedTools::pin(approved.pins(), found)));
        Ok(self)
    }

    /// Whether no harness is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
    }

    /// Each harness's vendor.
    #[must_use]
    pub(crate) const fn vendors(&self) -> &BTreeMap<String, Vendor> {
        &self.vendors
    }

    /// The project roots a task's files are copied from.
    pub(crate) fn roots(&self) -> impl Iterator<Item = &Path> {
        self.roots.values().map(PathBuf::as_path)
    }

    /// The host files a run reads again before it starts: each harness's
    /// binary and companions, the Codex host, and the tools.
    pub(crate) fn pinned_paths(&self) -> impl Iterator<Item = &Path> {
        self.binaries
            .values()
            .flat_map(|(harness, binary)| {
                std::iter::once(binary).chain(harness.companions().iter().map(|(file, _)| file))
            })
            .chain(&self.codex_host)
            .map(|binary| binary.path.as_path())
            .chain(self.tools.iter().map(|tool| tool.path.as_path()))
    }

    /// The vault secrets holding the keys of the vendors these harnesses
    /// relay to: the only harness keys the egress authority may read.
    #[must_use]
    pub fn vendor_secret_names(&self) -> BTreeSet<String> {
        self.vendors.keys().map(|name| secret_name(name)).collect()
    }

    /// Handles `op` when it names a harness skill; `None` for any other
    /// skill. `runtime` starts the cells; `keys` reads a vendor key.
    #[must_use]
    pub fn handle(
        &self,
        op: &SkillOp,
        runtime: Option<&SkillRuntime>,
        keys: HarnessKeys<'_>,
    ) -> Option<SkillResult> {
        let SkillOp::Invoke {
            skill,
            capability,
            input,
        } = op;
        let Some((harness, binary)) = self
            .binaries
            .values()
            .find(|(harness, _)| harness.skill_id() == skill.as_str())
        else {
            HarnessKind::from_skill_id(skill)?;
            tracing::warn!(skill = %skill, "denied skill op: the harness is not configured");
            return Some(SkillResult::UnknownSkill);
        };
        if capability.as_str() != HARNESS_CAPABILITY || !self.granted.contains(harness.name()) {
            tracing::warn!(skill = %skill, capability = %capability, "denied skill op: the harness grant does not cover it");
            return Some(SkillResult::CapabilityDenied);
        }
        let Some(runtime) = runtime else {
            tracing::error!(skill = %skill, "harness run impossible: the broker has no skill runtime");
            return Some(SkillResult::SkillError);
        };
        let report = self.run(harness, binary, input, runtime, keys);
        self.record(harness, &report);
        // What it wrote can't be told apart from a secret if the check
        // can't be answered, so none of it goes.
        let report = match report.screened(keys) {
            Ok(report) => report,
            Err(reason) => {
                tracing::error!(%reason, skill = %skill, "a harness report wasn't delivered: its secrets check wasn't answered");
                return Some(SkillResult::SkillError);
            }
        };
        match serde_json::to_string(&report.fitted()) {
            Ok(output) => Some(SkillResult::Output { output }),
            Err(error) => {
                tracing::error!(%error, "cannot encode a harness report");
                Some(SkillResult::SkillError)
            }
        }
    }

    /// One run: the brief, the slice, the cell, the changes.
    #[allow(
        clippy::too_many_lines,
        reason = "one run's steps, in the order they happen: the brief, the slice, the cell and its trees, the run, the changes"
    )]
    fn run(
        &self,
        harness: &Harness,
        binary: &PinnedBinary,
        input: &str,
        runtime: &SkillRuntime,
        keys: HarnessKeys<'_>,
    ) -> Report {
        let mut report = Report::new(harness, binary, input);
        let brief = match Brief::parse(input) {
            Ok(brief) => brief,
            Err(reason) => return report.refused(reason),
        };
        let Some(root) = self.roots.get(&brief.root) else {
            return report.refused(format!("{:?} is not a configured project root", brief.root));
        };
        // A harness cell holds a stand-in key, so a held secret can only
        // come in with what it's given, and what it's given goes to its
        // vendor. Refused here: the relay runs where only one key is held.
        if let Err(stopped) = brief.free_of_secrets(keys) {
            return report.stopped(stopped);
        }
        let code = match binary.verified() {
            Ok(code) => code,
            Err(reason) => return report.refused_for(BINARY_REFUSED, &reason),
        };
        let scratch = match RunDirs::create() {
            Ok(dirs) => dirs,
            Err(reason) => return report.failed(&reason),
        };
        if let Err(reason) = stage(root, &brief.paths, &scratch) {
            return report.refused(reason);
        }
        if let Err(stopped) = free_of_secrets(&scratch.pristine, keys) {
            return report.stopped(stopped);
        }
        let verified = match VerifiedCode::write("harness", "harness", &code) {
            Ok(verified) => verified,
            Err(error) => return report.failed(&error.to_string()),
        };
        let companions = match self.companion_copies(harness) {
            Ok(companions) => companions,
            Err((told, reason)) => return report.refused_for(told, &reason),
        };
        let tools = match self.tools_for(harness) {
            Ok(tools) => tools,
            Err(ToolsRefused::Changed(told, detail)) => return report.refused_for(&told, &detail),
            Err(ToolsRefused::Unpinned) => {
                return report.failed("the harness's tools weren't pinned at start");
            }
            Err(ToolsRefused::Failed(reason)) => return report.failed(&reason),
        };
        let (network, relaying) = match self.vendors.get(harness.name()) {
            None => (None, None),
            Some(vendor) => match Self::network(
                harness,
                vendor,
                keys,
                &scratch,
                (&companions, tools.as_deref()),
            ) {
                Ok((network, relaying)) => (Some(network), Some(relaying)),
                Err(reason) => return report.refused(reason),
            },
        };
        let cell = match harness_cell(
            harness,
            &brief.prompt(),
            verified.dir(),
            &scratch,
            network,
            self.ids,
        ) {
            Ok(cell) => cell,
            Err(error) => return report.failed(&error.to_string()),
        };
        if let Err(reason) = trees(runtime, &cell, &scratch, TreeOp::Give) {
            return report.failed(&reason);
        }
        let relay = relaying.map(|relaying| relaying.start(self.timeout + RELAY_GRACE));
        let ran = run_cell(
            &runtime.clone().with_timeout(self.timeout),
            &cell,
            CellKind::Harness,
            &[],
            MAX_HARNESS_OUTPUT_BYTES,
        );
        drop(verified);
        drop(companions);
        report.finish_relay(harness.name(), relay);
        report.outcome = match &ran {
            Ok(_) => HarnessOutcome::Done,
            Err(SkillRunError::TimedOut) => HarnessOutcome::TimedOut,
            Err(_) => HarnessOutcome::Failed,
        };
        match ran {
            Ok(stdout) => {
                report.harness_said.message = truncate(&final_message(&stdout), MAX_MESSAGE_BYTES);
            }
            Err(SkillRunError::Exited { code, .. }) => {
                report.exit_code = Some(code);
                report.reason = Some(format!("the harness exited with status {code}"));
            }
            Err(error) => report.reason = Some(run_failure(&error)),
        }
        // The cell is gone by now: its helper was waited for, or killed,
        // and its output closed.
        if let Err(reason) = trees(runtime, &cell, &scratch, TreeOp::Reclaim) {
            tracing::error!(%reason, "cannot take a harness's trees back");
            return report.failed(&format!("cannot take the work tree back: {reason}"));
        }
        if let Some(question) = read_question(&scratch.scratch) {
            report.harness_said.question = Some(question);
            if report.outcome == HarnessOutcome::Done {
                report.outcome = HarnessOutcome::NeedsClarification;
            }
        }
        match changes(&scratch.work, &scratch.pristine) {
            Ok(found) => {
                report.record_changes(&found);
                if !found.is_empty() {
                    let patch = patch(&found);
                    if holding(keys, patch.as_bytes()) == Ok(false) {
                        report.patch = self.deliver_patch(&patch);
                    } else {
                        tracing::warn!(
                            harness = harness.name(),
                            "a harness patch held a secret, or couldn't be checked: not delivered"
                        );
                        report.patch_withheld = true;
                    }
                }
            }
            Err(reason) => {
                report.outcome = HarnessOutcome::Failed;
                report.reason = Some(reason);
            }
        }
        report
    }

    /// The pinned files `harness`'s run is given beside its binary, each
    /// copied from its verified bytes, with the name its cell sees it at:
    /// the Codex host for a Codex run with one pinned, a manifest's
    /// companions for its harness, and none otherwise.
    ///
    /// # Errors
    ///
    /// What the report says, and the detail for the broker's log: a file
    /// is unreadable or not the one pinned, or couldn't be copied.
    fn companion_copies(
        &self,
        harness: &Harness,
    ) -> Result<Vec<(VerifiedCode, String)>, (&'static str, String)> {
        let pinned: Vec<(&PinnedBinary, &str, &'static str)> = match harness {
            Harness::BuiltIn(HarnessKind::CodexCli) => self
                .codex_host
                .iter()
                .map(|host| (host, CODEX_HOST, HOST_REFUSED))
                .collect(),
            Harness::BuiltIn(HarnessKind::ClaudeCode) => Vec::new(),
            Harness::Defined(_) => harness
                .companions()
                .iter()
                .map(|(file, name)| (file, name.as_str(), COMPANION_REFUSED))
                .collect(),
        };
        pinned
            .into_iter()
            .map(|(file, name, told)| {
                let bytes = file.verified().map_err(|reason| (told, reason))?;
                VerifiedCode::write("harness-companion", name, &bytes)
                    .map(|copy| (copy, name.to_owned()))
                    .map_err(|error| (told, error.to_string()))
            })
            .collect()
    }

    /// The copy of the pinned tools `kind`'s run is given, after checking
    /// each is still the program pinned; none for a harness without a
    /// network.
    ///
    /// # Errors
    ///
    /// The tools weren't pinned, one changed or went, or their copy
    /// couldn't be made.
    fn tools_for(&self, harness: &Harness) -> Result<Option<Arc<ToolCopy>>, ToolsRefused> {
        if !self.vendors.contains_key(harness.name()) || self.tools.is_empty() {
            return Ok(None);
        }
        let pinned = self.pinned.as_ref().ok_or(ToolsRefused::Unpinned)?;
        pinned
            .check()
            .map_err(|(told, detail)| ToolsRefused::Changed(told, detail))?;
        pinned.copy().map(Some)
    }

    /// A real CLI's network for one run: a fresh surrogate, its relay
    /// made ready with its vendor key, the handoff socket in the run's own
    /// directory, its tools (the pinned tools' copies, and its companions':
    /// the Codex host's, or a manifest's), and its environment. The relay's parts come back
    /// with it.
    ///
    /// The egress authority makes the relay ready, reading the key itself
    /// (WO-017, step 2), when one runs; in-process, with none, the broker
    /// reads it with `keys`. A run with no key is refused here, before its
    /// cell starts.
    fn network(
        harness: &Harness,
        vendor: &Vendor,
        keys: HarnessKeys<'_>,
        dirs: &RunDirs,
        (companions, tools): (&[(VerifiedCode, String)], Option<&ToolCopy>),
    ) -> Result<(HarnessNetwork, Relaying), String> {
        // Set-up failures are the broker's, and their detail stays in its
        // log: the report says only that the network couldn't be made.
        let unavailable = |what: &str, error: &dyn std::fmt::Display| {
            tracing::warn!(%error, what, "harness network setup failed");
            String::from("the harness network could not be set up")
        };
        let surrogate = crate::approval::random_hex(harness.surrogate_prefix())
            .map(Zeroizing::new)
            .map_err(|error| unavailable("surrogate key", &error))?;
        let secret = harness.secret_name();
        let ready = match crate::egress_authority::authority() {
            Some(authority) => {
                let pending = authority
                    .connect()
                    .and_then(|connection| connection.relay(harness.name(), &surrogate))
                    .map_err(|error| unavailable("relay", &error))??;
                Ready::Authority(pending)
            }
            None => Ready::Local(
                vendor.clone(),
                keys.key(&secret).ok_or_else(|| no_key(&secret))?,
                surrogate.clone(),
            ),
        };
        let handoff_path = dirs.base.join("relay.sock");
        // The broker's alone, whatever its umask: whoever connects first
        // hands in the run's relay listener, so no other user may.
        let handoff = crate::listener::OwnerOnlyListener::bind(&handoff_path)
            .and_then(|bound| bound.with_mode(0o600))
            .map_err(|error| unavailable("relay handoff", &error))?;
        let mut tools = tools.map_or_else(Vec::new, ToolCopy::for_cell);
        tools.extend(
            companions
                .iter()
                .map(|(copy, name)| (copy.dir().join(name), name.clone())),
        );
        let network = HarnessNetwork {
            relay: RELAY_PORT,
            handoff: handoff_path,
            tools,
            env: harness.env(RELAY_PORT, &surrogate),
        };
        let relaying = Relaying {
            handoff,
            destination: vendor.destination.clone(),
            ready,
        };
        Ok((network, relaying))
    }

    /// Writes `patch` to the artifact workspace, as a new file, through
    /// the spawner when one runs, as the owner's (WO-017); its name, or
    /// `None` when it could not be written (logged).
    fn deliver_patch(&self, patch: &str) -> Option<String> {
        let workspace = self.workspace.as_ref()?;
        let name = format!("{}.patch", crate::approval::random_hex("harness-").ok()?);
        let written = crate::artifact_ops::write_workspace(
            crate::spawner::spawner(),
            &workspace.join(&name),
            &name,
            patch.as_bytes(),
        );
        match written {
            Ok(()) => Some(name),
            Err(error) => {
                tracing::error!(%error, workspace = %workspace.display(), "cannot write a harness patch");
                None
            }
        }
    }

    /// Records one run in the activity log (when scheduling is on) and
    /// the broker's log.
    fn record(&self, harness: &Harness, report: &Report) {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let event = ActivityEvent {
            event_id: format!(
                "{HARNESS_RUN_ID}:{}:{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ),
            ts: Timestamp::now().to_string(),
            run_id: String::from(HARNESS_RUN_ID),
            kind: SubagentEvent::HarnessRun {
                harness: harness.name().to_owned(),
                binary_sha256: report.binary_sha256.clone(),
                brief_sha256: report.brief_sha256.clone(),
                outcome: report.outcome,
                files_changed: report.change_count(),
                network: report.network.clone(),
            },
        };
        let Ok(line) = serde_json::to_string(&event) else {
            return;
        };
        tracing::info!(target: "activity", event = %line, "harness run");
        if let Some(log) = &self.activity_log {
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(log)
                .and_then(|mut file| writeln!(file, "{line}"));
            if let Err(error) = written {
                tracing::error!(%error, "cannot append a harness run to the activity log");
            }
        }
    }
}

/// The capability every harness call must name.
fn harness_capability() -> Option<CapabilityName> {
    HARNESS_CAPABILITY.parse().ok()
}

/// A configuration refusal.
const fn invalid(key: &'static str, reason: String) -> Error {
    Error::InvalidConfig { key, reason }
}

/// The refusal of a run whose vendor key, the vault secret `secret`,
/// isn't stored.
fn no_key(secret: &str) -> String {
    format!("the vendor key is not in the vault: store it as the secret {secret}")
}

/// Refuses a vault grant naming the vendor key of the manifest's harness
/// `name`, as [`Config`] does the built-in harnesses': a grant would hand
/// it to the agent's cell, defeating the surrogate.
fn refuse_key_grants(config: &Config, name: &str) -> Result<(), Error> {
    let secret = secret_name(name);
    config
        .vault
        .grants
        .iter()
        .find(|grant| grant.name.as_str() == secret)
        .map_or(Ok(()), |grant| {
            Err(invalid(
                "HATCHLING_VAULT_GRANTS",
                format!(
                    "grant {}:{secret} names the vendor key the broker holds for the {name} harness",
                    grant.action
                ),
            ))
        })
}

/// What a run's relay starts from: the handoff socket, the vendor's
/// destination, and the relay made ready.
struct Relaying {
    handoff: UnixListener,
    destination: String,
    ready: Ready,
}

/// A relay made ready: in the egress authority, or the broker's own, with
/// its vendor, the vendor key and the surrogate the cell holds.
enum Ready {
    Authority(crate::egress_authority::PendingRelay),
    Local(Vendor, Zeroizing<String>, Zeroizing<String>),
}

impl Relaying {
    /// Starts the relay, serving for `serving`: its destination, and the
    /// relay, or why it couldn't start.
    fn start(self, serving: Duration) -> (String, RunRelay) {
        let relay = match self.ready {
            Ready::Authority(pending) => match pending.start(&self.handoff, serving) {
                Ok(relay) => RunRelay::Authority(relay),
                Err(error) => RunRelay::Unstarted(error.to_string()),
            },
            Ready::Local(vendor, key, surrogate) => RunRelay::Local(Relay::start(
                self.handoff,
                vendor,
                surrogate,
                key,
                Instant::now() + serving,
            )),
        };
        (self.destination, relay)
    }
}

/// One harness's binary and pin: both or neither.
fn parse_binary(
    kind: HarnessKind,
    path: Option<&str>,
    hash: Option<&str>,
) -> Result<Option<PinnedBinary>, Error> {
    parse_pin((kind.path_key(), kind.hash_key()), path, hash)
}

/// A pinned program from its `(path, hash)` settings: both or neither.
fn parse_pin(
    (path_key, hash_key): (&'static str, &'static str),
    path: Option<&str>,
    hash: Option<&str>,
) -> Result<Option<PinnedBinary>, Error> {
    let path = path.map(str::trim).filter(|raw| !raw.is_empty());
    let hash = hash.map(str::trim).filter(|raw| !raw.is_empty());
    let (path, hash) = match (path, hash) {
        (None, None) => return Ok(None),
        (Some(path), Some(hash)) => (PathBuf::from(path), hash.to_ascii_lowercase()),
        (Some(_), None) => {
            return Err(invalid(
                hash_key,
                String::from("a harness binary needs its pinned SHA-256"),
            ));
        }
        (None, Some(_)) => {
            return Err(invalid(
                path_key,
                String::from("a pinned SHA-256 needs its harness binary"),
            ));
        }
    };
    if !path.is_absolute() {
        return Err(invalid(
            path_key,
            format!(
                "the harness binary must be an absolute path: {}",
                path.display()
            ),
        ));
    }
    if !is_sha256(&hash) {
        return Err(invalid(
            hash_key,
            String::from("a SHA-256 is 64 hexadecimal digits"),
        ));
    }
    let binary = PinnedBinary { path, sha256: hash };
    binary
        .verified()
        .map_err(|reason| invalid(path_key, reason))?;
    Ok(Some(binary))
}

/// Whether `hash` is a SHA-256 as hex.
fn is_sha256(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A harness's vendor API: one HTTPS destination `host:port` (written
/// `internal:host:port` when it is meant to resolve inside the operator's
/// network), or one local endpoint `local:localhost:PORT`, resolved and
/// pinned now, under the allowlist's rules.
fn parse_vendor(
    kind: HarnessKind,
    raw: Option<&str>,
    tls: &TlsClient,
) -> Result<Option<Vendor>, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(None);
    };
    vendor(raw, kind.wire(), tls)
        .map(Some)
        .map_err(|reason| invalid(kind.api_key(), reason))
}

/// The vendor `raw` names, speaking `wire`: as [`parse_vendor`], for a
/// built-in harness's setting or a manifest's destination.
pub(crate) fn vendor(raw: &str, wire: Wire, tls: &TlsClient) -> Result<Vendor, String> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.contains(',')
        || raw.starts_with("public:")
        || raw.starts_with("local-keyless:")
    {
        return Err(String::from(
            "one vendor API: host:port, internal:host:port, or local:localhost:PORT",
        ));
    }
    let allowlist = Allowlist::parse(raw).map_err(|source| source.to_string())?;
    Ok(Vendor {
        wire,
        destination: [
            hatchling_egress::LOCAL_PREFIX,
            hatchling_egress::INTERNAL_PREFIX,
        ]
        .iter()
        .find_map(|prefix| raw.strip_prefix(prefix))
        .unwrap_or(raw)
        .to_owned(),
        allowlist: Arc::new(allowlist),
        tls: tls.clone(),
    })
}

/// A CLI cell's tools, each with the name the cell sees it at: those
/// `raw` names, each an absolute path to a program, or else the defaults
/// this host has. None for harnesses without a network. They're pinned
/// at the broker's start ([`Harnesses::pin_tools`]).
pub(crate) fn parse_tools(raw: Option<&str>, networked: bool) -> Result<Vec<Tool>, Error> {
    let explicit = raw.map(str::trim).filter(|raw| !raw.is_empty());
    if !networked {
        return match explicit {
            Some(_) => Err(invalid(
                TOOLS_KEY,
                String::from("tools serve a harness with a vendor API, and none is set"),
            )),
            None => Ok(Vec::new()),
        };
    }
    let paths: Vec<&str> = explicit.map_or_else(
        || DEFAULT_TOOLS.to_vec(),
        |raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .collect()
        },
    );
    let mut tools: Vec<Tool> = Vec::new();
    for path in paths {
        let pinned = Tool::at(path).and_then(|tool| open_program(&tool.path).map(|_| tool));
        match pinned {
            Ok(tool) if tools.iter().any(|known| known.name == tool.name) => {
                return Err(invalid(TOOLS_KEY, format!("{path} is named twice")));
            }
            Ok(tool) => tools.push(tool),
            Err(reason) if explicit.is_some() => {
                return Err(invalid(
                    TOOLS_KEY,
                    format!("{path} is not a program to run: {reason}"),
                ));
            }
            Err(_) => {}
        }
    }
    Ok(tools)
}

/// The vendor keys the broker reads for its harness relays, served
/// in-process, with no egress authority running, and the secrets check,
/// by this broker's keeper when it started one.
///
/// Each key is the vault secret [`secret_name`] names, read with the
/// broker's own keys and logged as the broker's read before it is used
/// ([`crate::secrets::Secrets::broker_read`]). No vault grant may
/// name it.
#[must_use]
pub fn vault_keys<'a>(config: &'a Config, key: &'a dyn OpensStores) -> VaultKeys<'a> {
    vault_keys_through(crate::keeper::keeper(), config, key)
}

/// [`vault_keys`], the check asked of `keeper`; with none, made here with
/// `key`, as a broker with no keeper makes it.
#[must_use]
pub const fn vault_keys_through<'a>(
    keeper: Option<&'a crate::keeper::Keeper>,
    config: &'a Config,
    key: &'a dyn OpensStores,
) -> VaultKeys<'a> {
    VaultKeys {
        config,
        key,
        keeper,
    }
}

/// The broker's own vault, as a harness run reads it.
pub struct VaultKeys<'a> {
    config: &'a Config,
    key: &'a dyn OpensStores,
    keeper: Option<&'a crate::keeper::Keeper>,
}

impl HarnessVault for VaultKeys<'_> {
    fn key(&self, secret: &str) -> Option<Zeroizing<String>> {
        vendor_key(&self.config.vault.root, self.key, secret)
    }

    fn contains(&self, text: &[u8]) -> Result<bool, String> {
        self.keeper.map_or_else(
            || crate::held_check::check_here(self.config, self.key, text),
            |keeper| keeper.contains(text).map_err(|error| error.to_string()),
        )
    }
}

/// The vendor key `secret`, as the broker's own logged vault read.
fn vendor_key(vault_root: &Path, key: &dyn OpensStores, secret: &str) -> Option<Zeroizing<String>> {
    let name = secret.parse::<hatchling_core::SecretName>().ok()?;
    let secret = crate::secrets::Secrets::new(vault_root, key)
        .broker_read("harness", std::slice::from_ref(&name))
        .ok()?
        .pop()??;
    key_text(&secret)
}

/// A vendor key as its relay sends it: the secret's text, trimmed, or
/// `None` when it isn't text or is blank.
pub(crate) fn key_text(secret: &hatchling_vault::Secret) -> Option<Zeroizing<String>> {
    std::str::from_utf8(secret.as_bytes())
        .ok()
        .map(|text| Zeroizing::new(text.trim().to_owned()))
        .filter(|text| !text.is_empty())
}

/// The project roots: `name=/absolute/dir`, each a directory outside the
/// vault and the broker socket's directory.
fn parse_roots(raw: Option<&str>, config: &Config) -> Result<BTreeMap<String, PathBuf>, Error> {
    let key = "HATCHLING_HARNESS_ROOTS";
    let mut roots = BTreeMap::new();
    for entry in raw
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let Some((name, dir)) = entry.split_once('=') else {
            return Err(invalid(key, format!("{entry:?} is not name=/absolute/dir")));
        };
        let (name, dir) = (name.trim(), PathBuf::from(dir.trim()));
        if !crate::is_workspace_name(name) {
            return Err(invalid(
                key,
                format!("{name:?} is not a root name: letters, digits, '.', '_' and '-'"),
            ));
        }
        if !dir.is_absolute() || !dir.is_dir() {
            return Err(invalid(
                key,
                format!(
                    "the root {name:?} must be an existing absolute directory: {}",
                    dir.display()
                ),
            ));
        }
        let near = |other: &Path| {
            !other.as_os_str().is_empty() && (dir.starts_with(other) || other.starts_with(&dir))
        };
        let socket_dir = config
            .socket
            .as_path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        if near(&config.vault.root) || near(&socket_dir) {
            return Err(invalid(
                key,
                format!("the root {name:?} overlaps the vault or the broker socket directory"),
            ));
        }
        if roots.insert(name.to_owned(), dir).is_some() {
            return Err(invalid(key, format!("the root {name:?} is named twice")));
        }
    }
    Ok(roots)
}

/// A run's bound: 1 to [`MAX_HARNESS_TIMEOUT_SECS`] seconds.
fn parse_timeout(raw: Option<&str>) -> Result<Duration, Error> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(DEFAULT_HARNESS_TIMEOUT);
    };
    match raw.parse::<u64>() {
        Ok(secs) if (1..=MAX_HARNESS_TIMEOUT_SECS).contains(&secs) => Ok(Duration::from_secs(secs)),
        _ => Err(invalid(
            "HATCHLING_HARNESS_TIMEOUT_SECS",
            format!("must be whole seconds from 1 to {MAX_HARNESS_TIMEOUT_SECS}, not {raw:?}"),
        )),
    }
}

/// What the agent asks a harness to do.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Brief {
    /// The task, in the agent's words.
    task: String,
    /// The project root, by name.
    root: String,
    /// The files the task needs, relative to the root.
    paths: Vec<String>,
    /// The answer to the harness's earlier question, if this run follows
    /// one.
    #[serde(default)]
    answer: Option<String>,
}

impl Brief {
    /// The brief `input` carries, checked.
    fn parse(input: &str) -> Result<Self, String> {
        let brief: Self = serde_json::from_str(input)
            .map_err(|err| format!("the brief is not a harness brief: {err}"))?;
        if brief.task.trim().is_empty() || brief.task.len() > MAX_TASK_BYTES {
            return Err(format!("the task must be 1 to {MAX_TASK_BYTES} bytes"));
        }
        if brief
            .answer
            .as_ref()
            .is_some_and(|answer| answer.len() > MAX_ANSWER_BYTES)
        {
            return Err(format!(
                "the answer must be at most {MAX_ANSWER_BYTES} bytes"
            ));
        }
        if brief.paths.is_empty() || brief.paths.len() > MAX_SLICE_FILES {
            return Err(format!("a brief names 1 to {MAX_SLICE_FILES} files"));
        }
        if brief.paths.iter().collect::<BTreeSet<_>>().len() != brief.paths.len() {
            return Err(String::from("the brief names a file twice"));
        }
        if let Some(bad) = brief.paths.iter().find(|path| !safe_relative(path)) {
            return Err(format!(
                "{bad:?} is not a plain relative path inside the root"
            ));
        }
        if std::iter::once(&brief.task)
            .chain(&brief.answer)
            .any(|text| text.contains('\0'))
        {
            return Err(String::from("the brief may not contain NUL bytes"));
        }
        Ok(brief)
    }

    /// That nothing this brief tells the harness holds a secret: the task,
    /// the answer and each file's name, each as the text [`Self::prompt`]
    /// sends, not as the JSON it came in, where a quote or a backslash in
    /// one would hide it.
    fn free_of_secrets(&self, keys: HarnessKeys<'_>) -> Result<(), Stopped> {
        let texts = std::iter::once(&self.task)
            .chain(&self.answer)
            .chain(&self.paths);
        for text in texts {
            if holding(keys, text.as_bytes())? {
                return Err(Stopped::Holds(String::from(
                    "the task holds a secret Hatchling keeps, so the harness wasn't started",
                )));
            }
        }
        Ok(())
    }

    /// What the harness is told: the task, the answer if any, the files,
    /// and how to ask a question.
    fn prompt(&self) -> String {
        let mut prompt = format!(
            "You are working on a task for Hatchling.\n\nTask:\n{}\n\n",
            self.task
        );
        if let Some(answer) = &self.answer {
            let _ = write!(prompt, "The answer to your earlier question:\n{answer}\n\n");
        }
        let _ = write!(
            prompt,
            "The files are in your working directory: {}. Change only what the task needs; new files go beside them.\n\
             If you cannot continue without an answer, write one question to /scratch/{QUESTION_FILE} and stop.",
            self.paths.join(", ")
        );
        prompt
    }
}

/// A plain relative path: normal components only, each of letters,
/// digits, `.`, `_` and `-`.
fn safe_relative(path: &str) -> bool {
    let components: Vec<Component<'_>> = Path::new(path).components().collect();
    !components.is_empty()
        && components.iter().all(|component| {
            matches!(component, Component::Normal(part) if part.to_str().is_some_and(|part| {
                part.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            }))
        })
}

/// One run's private directories: the work tree the harness edits, a
/// pristine copy to compare it with, and the harness's scratch. Removed
/// when dropped.
#[derive(Debug)]
struct RunDirs {
    base: PathBuf,
    work: PathBuf,
    pristine: PathBuf,
    scratch: PathBuf,
}

impl RunDirs {
    /// Fresh, owner-only directories under the temp dir.
    fn create() -> Result<Self, String> {
        // A name no one can guess, made exclusively below: another local
        // user can't pre-create it to block the run.
        let name = crate::approval::random_hex("hatchling-harness-")
            .map_err(|err| format!("cannot name the run directory: {err}"))?;
        let base = std::env::temp_dir().join(name);
        // Searchable by all, listed and written by the broker alone: a
        // cell's setup opens what it binds from here inside a user namespace
        // where the broker's user, once it isn't root (WO-017), isn't
        // mapped, so only these bits let it through. The name is random.
        // What is in it stays owner-only.
        std::fs::DirBuilder::new()
            .mode(0o711)
            .create(&base)
            .map_err(|err| format!("cannot create the run directory: {err}"))?;
        let dirs = Self {
            work: base.join("work"),
            pristine: base.join("pristine"),
            scratch: base.join("scratch"),
            base,
        };
        for dir in [&dirs.work, &dirs.pristine, &dirs.scratch] {
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(dir)
                .map_err(|err| format!("cannot create {}: {err}", dir.display()))?;
        }
        Ok(dirs)
    }
}

impl Drop for RunDirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// Copies the named files from `root` into the work tree and the
/// pristine copy, refusing anything that is not a plain text file
/// reached without a symlink.
fn stage(root: &Path, paths: &[String], dirs: &RunDirs) -> Result<(), String> {
    let mut total = 0_u64;
    for path in paths {
        let mut at = root.to_path_buf();
        for component in Path::new(path).components() {
            at.push(component);
            let meta = std::fs::symlink_metadata(&at)
                .map_err(|_| format!("{path:?} is not in the root"))?;
            if meta.file_type().is_symlink() {
                return Err(format!("{path:?} goes through a symlink"));
            }
        }
        let (bytes, more) = read_regular_prefix(&at, MAX_SLICE_BYTES.saturating_sub(total))
            .map_err(|_| format!("{path:?} is not a regular file"))?;
        total = total
            .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::from(more));
        if total > MAX_SLICE_BYTES {
            return Err(format!("the files hold more than {MAX_SLICE_BYTES} bytes"));
        }
        if std::str::from_utf8(&bytes).is_err() {
            return Err(format!("{path:?} is not text"));
        }
        for tree in [&dirs.work, &dirs.pristine] {
            write_new(&tree.join(path), &bytes).map_err(|error| {
                tracing::warn!(%error, ?path, "cannot stage a harness file");
                format!("cannot stage {path:?}")
            })?;
        }
    }
    Ok(())
}

/// That no file under `root` holds a secret, each its own check: why not,
/// naming the file and never what it holds.
fn free_of_secrets(root: &Path, keys: HarnessKeys<'_>) -> Result<(), Stopped> {
    let tree = read_tree(root).map_err(Stopped::Holds)?;
    for (path, bytes) in &tree.files {
        if holding(keys, bytes)? {
            return Err(Stopped::Holds(format!(
                "{path:?} holds a secret Hatchling keeps, so the harness wasn't started"
            )));
        }
    }
    Ok(())
}

/// Writes `bytes` at `path`, creating owner-only parent directories.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(parent)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
}

/// The harness cell for one run of `kind` on `prompt`.
fn harness_cell(
    harness: &Harness,
    prompt: &str,
    code_dir: &Path,
    dirs: &RunDirs,
    network: Option<HarnessNetwork>,
    ids: CellIds,
) -> Result<CellConfig, Error> {
    let mut cell = CellConfig::skill_on("harness", ids)?;
    cell.skill_code = Some(code_dir.to_path_buf());
    cell.agent_bin = code_dir.join("harness");
    cell.harness = Some(HarnessDirs {
        work: dirs.work.clone(),
        scratch: dirs.scratch.clone(),
        args: harness.args(prompt, RELAY_PORT),
        network,
    });
    Ok(cell)
}

/// Moves a harness run's work tree and scratch directory between the
/// broker and `cell`: `Give` before the run, `Reclaim` after it.
///
/// Through the spawner when one runs (WO-017): on the cell's own ids it
/// gives them and takes them back by chown, held to its policy, so a
/// broker that isn't root reads what the cell left. Without one, a root
/// broker gives them by chown ([`hand_over`]) and needs nothing back: it
/// reads anything. On the owner's subordinate ids both go through the
/// tree helper ([`change_trees`]), since the broker can neither give a
/// file away nor read, or remove, what the cell left.
///
/// # Errors
///
/// Why they couldn't be moved.
fn trees(
    runtime: &SkillRuntime,
    cell: &CellConfig,
    dirs: &RunDirs,
    op: TreeOp,
) -> Result<(), String> {
    let changed = [dirs.work.as_path(), dirs.scratch.as_path()];
    if let Some(spawner) = crate::spawner::spawner() {
        return spawner.trees(op, &changed, cell.identity());
    }
    match (cell.ids, op) {
        (CellIds::Own, TreeOp::Give) => hand_over(dirs, cell.uid_base),
        (CellIds::Own, TreeOp::Reclaim) => Ok(()),
        (CellIds::Subordinate, _) => change_trees(runtime.spawner(), op, &changed, cell.identity()),
    }
}

/// Gives the work tree and scratch directory to `uid`, the cell's map
/// base. A root broker's cell runs as an unprivileged uid, which must own
/// them to write; an unprivileged broker's cell runs as the broker's own
/// uid, so handing them over changes nothing. The group is left alone:
/// the owner bits are the only ones set.
fn hand_over(dirs: &RunDirs, uid: u32) -> Result<(), String> {
    for top in [&dirs.work, &dirs.scratch] {
        let mut pending = vec![top.clone()];
        while let Some(path) = pending.pop() {
            std::os::unix::fs::lchown(&path, Some(uid), None)
                .map_err(|err| format!("cannot hand {} to the cell: {err}", path.display()))?;
            if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir()) {
                let entries = std::fs::read_dir(&path)
                    .map_err(|err| format!("cannot read {}: {err}", path.display()))?;
                pending.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
            }
        }
    }
    Ok(())
}

/// What the harness finally said: a JSON object's `result` (Claude
/// Code's form), the last agent message of its JSON lines (the Codex
/// CLI's), the last text part of its JSON lines (`OpenCode`'s), or its
/// whole output.
fn final_message(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    let result = serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.as_str().map(str::to_owned))
        });
    let last_message = || {
        text.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| {
                event.get("type").and_then(serde_json::Value::as_str) == Some("item.completed")
            })
            .filter_map(|event| {
                let item = event.get("item")?;
                (item.get("type")?.as_str()? == "agent_message")
                    .then(|| item.get("text")?.as_str().map(str::to_owned))?
            })
            .next_back()
    };
    let last_text = || {
        text.lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event.get("type").and_then(serde_json::Value::as_str) == Some("text"))
            .filter_map(|event| event.get("part")?.get("text")?.as_str().map(str::to_owned))
            .next_back()
    };
    result
        .or_else(last_message)
        .or_else(last_text)
        .unwrap_or_else(|| text.into_owned())
}

/// A failed run, in Hatchling's words.
fn run_failure(error: &SkillRunError) -> String {
    match error {
        SkillRunError::TimedOut => String::from("the harness ran out of time and was stopped"),
        SkillRunError::OutputTooLarge => {
            format!("the harness wrote more than {MAX_HARNESS_OUTPUT_BYTES} bytes")
        }
        other @ (SkillRunError::Unavailable(_)
        | SkillRunError::InputTooLarge { .. }
        | SkillRunError::Prepare(_)
        | SkillRunError::Spawn(_)
        | SkillRunError::NotUtf8
        | SkillRunError::Exited { .. }) => {
            tracing::error!(error = ?other, "harness run failed");
            String::from("the harness could not run")
        }
    }
}

/// How much of the question file is read: the question's own bound, with
/// room for whitespace before it.
const QUESTION_READ_BYTES: u64 = 4 * 2048;

/// The harness's question, if it left one.
fn read_question(scratch: &Path) -> Option<String> {
    let (bytes, _) = read_regular_prefix(&scratch.join(QUESTION_FILE), QUESTION_READ_BYTES).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let question = text.trim();
    (!question.is_empty()).then(|| truncate(question, MAX_QUESTION_BYTES))
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn truncate(text: &str, max: usize) -> String {
    let mut out = String::new();
    for character in text.chars() {
        if out.len() + character.len_utf8() > max {
            break;
        }
        out.push(character);
    }
    out
}

/// A tree's files: relative path to bytes. Symlinks are listed apart.
#[derive(Debug, Default)]
struct Tree {
    files: BTreeMap<String, Vec<u8>>,
    links: BTreeSet<String>,
}

/// Reads `root`'s tree, within the work tree's bounds.
fn read_tree(root: &Path) -> Result<Tree, String> {
    let mut tree = Tree::default();
    let mut pending = vec![PathBuf::new()];
    let mut total = 0_u64;
    while let Some(relative) = pending.pop() {
        // A read failure's OS detail stays in the broker's log; the report
        // names only the work tree, or the file, in the cell's own terms.
        let unreadable = |shown: &str, error: std::io::Error| {
            tracing::warn!(%error, shown, "cannot read the harness work tree");
            if shown.is_empty() {
                String::from("cannot read the work tree")
            } else {
                format!("cannot read {shown}")
            }
        };
        let entries = std::fs::read_dir(root.join(&relative)).map_err(|err| unreadable("", err))?;
        for entry in entries {
            let entry = entry.map_err(|err| unreadable("", err))?;
            let name = relative.join(entry.file_name());
            let shown = name.to_string_lossy().into_owned();
            let kind = entry.file_type().map_err(|err| unreadable("", err))?;
            if kind.is_dir() {
                pending.push(name);
            } else if kind.is_file() {
                let (bytes, more) =
                    read_prefix(&root.join(&name), MAX_WORK_BYTES.saturating_sub(total))
                        .map_err(|err| unreadable(&shown, err))?;
                total = total
                    .saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                    .saturating_add(u64::from(more));
                tree.files.insert(shown, bytes);
            } else {
                tree.links.insert(shown);
            }
            if tree.files.len() + tree.links.len() > MAX_WORK_FILES || total > MAX_WORK_BYTES {
                return Err(format!(
                    "the harness left more than {MAX_WORK_FILES} files or {MAX_WORK_BYTES} bytes in its work tree"
                ));
            }
        }
    }
    Ok(tree)
}

/// What a run changed, comparing the work tree with the pristine copy.
#[derive(Debug, Default, PartialEq, Eq)]
struct Changes {
    /// Text files changed: path, before, after.
    changed: Vec<(String, String, String)>,
    /// Text files added: path, content.
    added: Vec<(String, String)>,
    /// Files removed: path, former content.
    removed: Vec<(String, String)>,
    /// Changes a text patch cannot carry (binary files, links).
    unpatchable: Vec<String>,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.changed.is_empty()
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.unpatchable.is_empty()
    }
}

/// The changes between the work tree and the pristine copy.
fn changes(work: &Path, pristine: &Path) -> Result<Changes, String> {
    let (after, before) = (read_tree(work)?, read_tree(pristine)?);
    let mut found = Changes::default();
    let text = |bytes: &[u8]| std::str::from_utf8(bytes).ok().map(str::to_owned);
    for (path, new) in &after.files {
        match (before.files.get(path), text(new)) {
            (Some(old), _) if old == new => {}
            (Some(old), Some(new)) => match text(old) {
                Some(old) => found.changed.push((path.clone(), old, new)),
                None => found.unpatchable.push(path.clone()),
            },
            (None, Some(new)) => found.added.push((path.clone(), new)),
            (_, None) => found.unpatchable.push(path.clone()),
        }
    }
    for (path, old) in &before.files {
        if !after.files.contains_key(path) {
            match text(old) {
                Some(old) => found.removed.push((path.clone(), old)),
                None => found.unpatchable.push(path.clone()),
            }
        }
    }
    found.unpatchable.extend(after.links);
    Ok(found)
}

/// `text`'s lines, each without its `\n` (a `\r` before it stays, so a
/// CRLF file's patch keeps its line ends), and whether the last line
/// lacks a newline.
fn lines(text: &str) -> (Vec<&str>, bool) {
    let lines = text
        .split_inclusive('\n')
        .map(|line| line.strip_suffix('\n').unwrap_or(line))
        .collect();
    (lines, !text.is_empty() && !text.ends_with('\n'))
}

/// One hunk side: `sign` before each line, and the no-newline marker.
fn hunk_side(out: &mut String, sign: char, text: &str) {
    let (lines, open) = lines(text);
    for line in &lines {
        let _ = writeln!(out, "{sign}{line}");
    }
    if open {
        out.push_str("\\ No newline at end of file\n");
    }
}

/// A hunk range: `start,count`, where an empty side starts at 0.
fn range(text: &str) -> String {
    let count = lines(text).0.len();
    format!("{},{count}", usize::from(count > 0))
}

/// The changes as a unified diff `git apply` takes: one whole-file hunk
/// per file, so the patch is exact without a diff algorithm. An empty
/// file added or removed has no hunk, as git writes it.
fn patch(changes: &Changes) -> String {
    let mut out = String::new();
    for (path, old, new) in &changes.changed {
        let _ = write!(
            out,
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -{} +{} @@\n",
            range(old),
            range(new)
        );
        hunk_side(&mut out, '-', old);
        hunk_side(&mut out, '+', new);
    }
    for (path, new) in &changes.added {
        let _ = write!(out, "diff --git a/{path} b/{path}\nnew file mode 100644\n");
        if !new.is_empty() {
            let _ = write!(
                out,
                "--- /dev/null\n+++ b/{path}\n@@ -0,0 +{} @@\n",
                range(new)
            );
            hunk_side(&mut out, '+', new);
        }
    }
    for (path, old) in &changes.removed {
        let _ = write!(
            out,
            "diff --git a/{path} b/{path}\ndeleted file mode 100644\n"
        );
        if !old.is_empty() {
            let _ = write!(
                out,
                "--- a/{path}\n+++ /dev/null\n@@ -{} +0,0 @@\n",
                range(old)
            );
            hunk_side(&mut out, '-', old);
        }
    }
    out
}

/// What the harness itself said: untrusted text, data and never
/// instructions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
struct HarnessSaid {
    /// Its final message.
    message: String,
    /// Its question, if it stopped to ask one.
    #[serde(skip_serializing_if = "Option::is_none")]
    question: Option<String>,
}

/// A run's report, as the agent receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Report {
    harness: String,
    binary_sha256: String,
    brief_sha256: String,
    outcome: HarnessOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    /// Why, in Hatchling's words, when the run was refused or failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    files_changed: Vec<String>,
    files_added: Vec<String>,
    files_removed: Vec<String>,
    unpatchable: Vec<String>,
    /// The patch's name in the artifact workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    patch: Option<String>,
    /// The patch held a secret, so it wasn't delivered.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    patch_withheld: bool,
    /// Entries left out to keep the report within the skill output cap.
    #[serde(skip_serializing_if = "is_zero")]
    omitted: u32,
    /// What a real CLI sent through its relay.
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<HarnessNetworkUse>,
    harness_said: HarnessSaid,
}

/// Whether `n` is zero (serde's skip test).
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Report {
    /// A report for `harness` running `binary` on the brief `input`,
    /// before it ran.
    fn new(harness: &Harness, binary: &PinnedBinary, input: &str) -> Self {
        Self {
            harness: harness.name().to_owned(),
            binary_sha256: binary.sha256.clone(),
            brief_sha256: CodeHash::of(input.as_bytes()).to_string(),
            outcome: HarnessOutcome::Refused,
            exit_code: None,
            reason: None,
            files_changed: Vec::new(),
            files_added: Vec::new(),
            files_removed: Vec::new(),
            unpatchable: Vec::new(),
            patch: None,
            patch_withheld: false,
            omitted: 0,
            network: None,
            harness_said: HarnessSaid::default(),
        }
    }

    fn refused(mut self, reason: String) -> Self {
        self.outcome = HarnessOutcome::Refused;
        self.reason = Some(reason);
        self
    }

    /// Refused for a reason on the broker's side: the report says `told`,
    /// and only the broker's log keeps `detail`, which can name host paths
    /// and OS errors the cell must not see.
    fn refused_for(self, told: &str, detail: &str) -> Self {
        tracing::warn!(%detail, told, "harness run refused");
        self.refused(String::from(told))
    }

    /// Stopped by a check, before its cell started.
    fn stopped(self, stopped: Stopped) -> Self {
        match stopped {
            Stopped::Holds(reason) => self.refused(reason),
            Stopped::Unchecked => self.failed("its secrets check wasn't answered"),
        }
    }

    fn failed(mut self, reason: &str) -> Self {
        tracing::error!(%reason, "harness run failed before the harness started");
        self.outcome = HarnessOutcome::Failed;
        self.reason = Some(String::from("the harness could not run"));
        self
    }

    /// The relay's account of the run, once its cell has ended:
    /// where the requests went and how many, for the activity event.
    fn finish_relay(&mut self, harness: &str, relay: Option<(String, RunRelay)>) {
        let Some((destination, relay)) = relay else {
            return;
        };
        let (count, problem) = relay.finish();
        if let Some(problem) = problem {
            tracing::warn!(%problem, harness, "harness relay");
        }
        self.network = Some(HarnessNetworkUse {
            destination,
            requests: count.forwarded,
            refused: count.refused,
        });
    }

    fn record_changes(&mut self, changes: &Changes) {
        self.files_changed = changes
            .changed
            .iter()
            .map(|(path, _, _)| path.clone())
            .collect();
        self.files_added = changes.added.iter().map(|(path, _)| path.clone()).collect();
        self.files_removed = changes
            .removed
            .iter()
            .map(|(path, _)| path.clone())
            .collect();
        self.unpatchable.clone_from(&changes.unpatchable);
    }

    /// This report, with nothing the run wrote that holds a secret. A
    /// file's name or a message can carry one as well as a patch can.
    ///
    /// # Errors
    ///
    /// Why a text couldn't be checked: none of it can be delivered.
    fn screened(mut self, keys: HarnessKeys<'_>) -> Result<Self, String> {
        let withheld = |text: &mut String| {
            if keys.contains(text.as_bytes())? {
                WITHHELD.clone_into(text);
            }
            Ok::<(), String>(())
        };
        withheld(&mut self.harness_said.message)?;
        self.harness_said
            .question
            .iter_mut()
            .try_for_each(withheld)?;
        self.reason.iter_mut().try_for_each(withheld)?;
        for names in [
            &mut self.files_changed,
            &mut self.files_added,
            &mut self.files_removed,
            &mut self.unpatchable,
        ] {
            names.iter_mut().try_for_each(withheld)?;
        }
        Ok(self)
    }

    /// Files the run changed, added, removed, or changed beyond a patch.
    fn change_count(&self) -> u32 {
        let count = self.files_changed.len()
            + self.files_added.len()
            + self.files_removed.len()
            + self.unpatchable.len();
        u32::try_from(count)
            .unwrap_or(u32::MAX)
            .saturating_add(self.omitted)
    }

    /// The report within the skill output cap: the harness's message
    /// first, then list entries, are left out until it fits.
    fn fitted(&self) -> Self {
        let mut report = self.clone();
        let fits = |report: &Self| {
            serde_json::to_string(report)
                .is_ok_and(|json| json.len() <= hatchling_core::MAX_SKILL_OUTPUT_BYTES)
        };
        if !fits(&report) {
            report.harness_said.message.clear();
        }
        while !fits(&report) {
            let dropped = [
                &mut report.unpatchable,
                &mut report.files_removed,
                &mut report.files_added,
                &mut report.files_changed,
            ]
            .into_iter()
            .find_map(Vec::pop);
            if dropped.is_none() {
                report.harness_said.question = None;
                break;
            }
            report.omitted = report.omitted.saturating_add(1);
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use hatchling_core::{LogLevel, PeerUid};
    use serde_json::{Value, json};

    use super::*;
    use crate::{
        ArtifactConfig, EgressConfig, GoalsConfig, MemoryConfig, SchedulerConfig, SkillConfig,
        SpaceConfig, VaultConfig, WebConfig,
    };
    use hatchling_vault::MasterKey;
    use hatchling_vault::SealedVault;

    /// A scratch directory, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hatchling-harness-unit-{}-{name}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn join(&self, path: &str) -> PathBuf {
            self.0.join(path)
        }

        fn write(&self, path: &str, bytes: &[u8]) -> PathBuf {
            let at = self.join(path);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(&at, bytes).unwrap();
            at
        }

        fn dir(&self, path: &str) -> PathBuf {
            let at = self.join(path);
            std::fs::create_dir_all(&at).unwrap();
            at
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A broker config with its vault, socket and workspace in `scratch`.
    fn config(scratch: &Scratch) -> Config {
        let vault = scratch.join("vault");
        Config {
            socket: scratch
                .join("sock/broker.sock")
                .to_str()
                .unwrap()
                .parse()
                .unwrap(),
            agent_uid: PeerUid::from_kernel(1000),
            socket_mode: 0o600,
            max_connections: 16,
            scheduler: SchedulerConfig::disabled(),
            log_level: LogLevel::Error,
            vault: VaultConfig {
                key_file: VaultConfig::default_key_file(&vault),
                root: vault,
                grants: Vec::new(),
            },
            egress: EgressConfig {
                allowlist: crate::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: Vec::new(),
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: SkillConfig::disabled(),
            memory: MemoryConfig {
                root: PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: ArtifactConfig {
                workspace: Some(scratch.dir("workspace")),
                ..ArtifactConfig::disabled()
            },
            spaces: SpaceConfig::disabled(),
            web: WebConfig::off(),
            goals: GoalsConfig::disabled(),
        }
    }

    /// A stub binary at `scratch/bin/harness`, its pin, and the settings
    /// that configure it as Claude Code over `scratch/project`.
    fn settings(scratch: &Scratch) -> (PinnedBinary, HashMap<&'static str, String>) {
        let path = scratch.write("bin/harness", b"stub harness");
        let sha256 = CodeHash::of(b"stub harness").to_string();
        let project = scratch.dir("project");
        let vars = HashMap::from([
            ("HATCHLING_HARNESS_CLAUDE_CODE", path.display().to_string()),
            ("HATCHLING_HARNESS_CLAUDE_CODE_SHA256", sha256.clone()),
            (
                "HATCHLING_HARNESS_ROOTS",
                format!("project={}", project.display()),
            ),
            ("HATCHLING_HARNESS_TIMEOUT_SECS", String::from("7")),
        ]);
        (PinnedBinary { path, sha256 }, vars)
    }

    fn parsed(config: &Config, vars: &HashMap<&'static str, String>) -> Result<Harnesses, Error> {
        Harnesses::parse(config, |key| vars.get(key).cloned())
    }

    fn grant(config: &mut Config, skill: &str, capability: &str) {
        config.skills.grants.insert(
            String::from(skill),
            HashSet::from([capability.parse().unwrap()]),
        );
    }

    /// The setting a configuration refusal names, and why.
    fn refusal<T: std::fmt::Debug>(result: Result<T, Error>) -> (&'static str, String) {
        let error = result.unwrap_err();
        if let Error::InvalidConfig { key, reason } = error {
            (key, reason)
        } else {
            ("not a configuration refusal", error.to_string())
        }
    }

    fn invoke(skill: &str, capability: &str, input: &str) -> SkillOp {
        SkillOp::Invoke {
            skill: String::from(skill),
            capability: capability.parse().unwrap(),
            input: String::from(input),
        }
    }

    fn owner(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().uid()
    }

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn the_harnesses_are_named_as_grants_skills_and_settings_name_them() {
        assert_eq!(
            HarnessKind::ALL.map(HarnessKind::name),
            ["claude-code", "codex-cli"]
        );
        assert_eq!(
            HarnessKind::ALL.map(HarnessKind::skill_id),
            ["claude-code@1", "codex-cli@1"]
        );
        assert_eq!(
            HarnessKind::ALL.map(HarnessKind::path_key),
            [
                "HATCHLING_HARNESS_CLAUDE_CODE",
                "HATCHLING_HARNESS_CODEX_CLI"
            ]
        );
        assert_eq!(
            HarnessKind::ALL.map(HarnessKind::hash_key),
            [
                "HATCHLING_HARNESS_CLAUDE_CODE_SHA256",
                "HATCHLING_HARNESS_CODEX_CLI_SHA256"
            ]
        );
        for kind in HarnessKind::ALL {
            assert_eq!(HarnessKind::from_skill_id(kind.skill_id()), Some(kind));
        }
        for other in ["claude-code", "codex-cli@2", "fixture@1.0.0", ""] {
            assert_eq!(HarnessKind::from_skill_id(other), None, "{other}");
        }
        // As verified against the real binaries.
        assert_eq!(
            HarnessKind::ClaudeCode.args("Do it.", 9),
            [
                "-p",
                "Do it.",
                "--output-format",
                "json",
                "--permission-mode",
                "acceptEdits",
                "--allowedTools",
                "Bash,Edit,Write,Read,Glob,Grep"
            ]
        );
        assert_eq!(
            HarnessKind::CodexCli.args("Do it.", 9),
            [
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config",
                "--json",
                "--dangerously-bypass-approvals-and-sandbox",
                "-c",
                "model_provider=hatchling",
                "-c",
                "model_providers.hatchling.name=\"hatchling\"",
                "-c",
                "model_providers.hatchling.base_url=\"http://127.0.0.1:9/v1\"",
                "-c",
                "model_providers.hatchling.env_key=\"HATCHLING_HARNESS_KEY\"",
                "-c",
                "model_providers.hatchling.wire_api=\"responses\"",
                "-c",
                "model_providers.hatchling.supports_websockets=false",
                "Do it."
            ]
        );
        assert_eq!(
            harness_capability()
                .map(|cap| cap.as_str().to_owned())
                .as_deref(),
            Some("code")
        );
        assert!(matches!(
            invalid("KEY", String::from("why")),
            Error::InvalidConfig { key: "KEY", reason } if reason == "why"
        ));
    }

    #[test]
    fn each_real_cli_has_its_api_setting_wire_vault_secret_and_key_shape() {
        assert_eq!(
            HarnessKind::ALL.map(HarnessKind::api_key),
            [
                "HATCHLING_HARNESS_CLAUDE_CODE_API",
                "HATCHLING_HARNESS_CODEX_CLI_API"
            ]
        );
        assert_eq!(
            HarnessKind::ALL.map(HarnessKind::wire),
            [Wire::Anthropic, Wire::OpenAi]
        );
        assert_eq!(
            HarnessKind::ALL.map(HarnessKind::secret_name),
            ["harness-claude-code", "harness-codex-cli"]
        );
        assert_eq!(
            HarnessKind::ALL.map(HarnessKind::surrogate_prefix),
            ["sk-ant-hatchling-", "sk-hatchling-"]
        );
    }

    /// A vendor key is its secret's text, trimmed, and none when blank or
    /// not text; a run with none is refused naming the secret to store;
    /// no harness configured is no vendor.
    #[test]
    fn a_vendor_key_is_its_trimmed_text_and_its_absence_names_the_secret() {
        let secret = |bytes: &[u8]| hatchling_vault::Secret::new(bytes.to_vec()).unwrap();
        assert_eq!(
            key_text(&secret(b" sk-key \n"))
                .as_deref()
                .map(String::as_str),
            Some("sk-key")
        );
        assert!(key_text(&secret(b"  \n")).is_none());
        assert!(key_text(&secret(&[0xff, 0xfe])).is_none());
        assert_eq!(
            no_key(HarnessKind::CodexCli.secret_name()),
            "the vendor key is not in the vault: store it as the secret harness-codex-cli"
        );
        assert!(Harnesses::default().vendors().is_empty());
        assert!(Harnesses::default().tools.is_empty());
    }

    #[test]
    fn a_relayed_run_reports_where_its_requests_went() {
        let mut report = sample_report();
        report.finish_relay("claude-code", None);
        assert_eq!(report.network, None, "no relay, no network use");
        let scratch = Scratch::new("relay-report");
        let handoff = UnixListener::bind(scratch.join("handoff")).unwrap();
        let destination = String::from("localhost:1");
        let vendor = Vendor {
            wire: Wire::Anthropic,
            allowlist: Arc::new(Allowlist::parse("local:localhost:1").unwrap()),
            destination: destination.clone(),
            tls: TlsClient::bundled().unwrap(),
        };
        let relay = Relay::start(
            handoff,
            vendor,
            Zeroizing::new(String::from("surrogate")),
            Zeroizing::new(String::from("key")),
            Instant::now(),
        );
        report.finish_relay(
            "claude-code",
            Some((destination.clone(), RunRelay::Local(relay))),
        );
        assert_eq!(
            report.network,
            Some(HarnessNetworkUse {
                destination,
                requests: 0,
                refused: 0
            })
        );
    }

    #[test]
    fn a_checked_pin_names_the_part_that_is_wrong() {
        let scratch = Scratch::new("checked-pin");
        let path = scratch.write("bin/tool", b"tool bytes");
        let hash = CodeHash::of(b"tool bytes").to_string();
        let shown = path.display().to_string();
        let pinned = PinnedBinary::checked(&format!(" {shown} "), &hash.to_uppercase()).unwrap();
        assert_eq!(
            pinned,
            PinnedBinary {
                path,
                sha256: hash.clone()
            }
        );
        let field =
            |path: &str, hash: &str| PinnedBinary::checked(path, hash).map_err(|(field, _)| field);
        assert_eq!(field("bin/tool", &hash), Err("path"));
        assert_eq!(field(&format!("{shown}-gone"), &hash), Err("path"));
        assert_eq!(field(&shown, "abc"), Err("sha256"));
        assert_eq!(field(&shown, &"0".repeat(64)), Err("sha256"));
        let (_, reason) = PinnedBinary::checked(&shown, &"0".repeat(64)).unwrap_err();
        assert!(reason.contains(&hash), "{reason}");
        assert!(is_sha256(&hash));
        assert!(is_sha256(&"A".repeat(64)));
        for bad in [
            "0".repeat(63),
            "0".repeat(65),
            "g".repeat(64),
            String::new(),
        ] {
            assert!(!is_sha256(&bad), "{bad}");
        }
    }

    #[test]
    fn a_vault_grant_on_a_manifests_key_refuses_its_harness() {
        let scratch = Scratch::new("key-grants");
        let mut config = config(&scratch);
        assert!(refuse_key_grants(&config, "opencode").is_ok());
        config.vault.grants = vec![crate::VaultGrant {
            action: hatchling_core::VaultAction::Store,
            name: "harness-opencode".parse().unwrap(),
        }];
        assert_eq!(
            refusal(refuse_key_grants(&config, "opencode")).0,
            "HATCHLING_VAULT_GRANTS"
        );
        assert!(refuse_key_grants(&config, "aider").is_ok());
    }

    #[test]
    fn a_pin_is_a_program_and_its_hash_or_neither_under_its_own_settings() {
        let scratch = Scratch::new("pin");
        let path = scratch.write("bin/host", b"host");
        let hash = CodeHash::of(b"host").to_string();
        let shown = path.display().to_string();
        let keys = ("PIN_PATH", "PIN_HASH");
        assert_eq!(parse_pin(keys, None, None).unwrap(), None);
        assert_eq!(
            refusal(parse_pin(keys, Some(&shown), None)),
            (
                "PIN_HASH",
                String::from("a harness binary needs its pinned SHA-256")
            )
        );
        assert_eq!(
            refusal(parse_pin(keys, None, Some(&hash))),
            (
                "PIN_PATH",
                String::from("a pinned SHA-256 needs its harness binary")
            )
        );
        assert_eq!(
            refusal(parse_pin(keys, Some("bin/host"), Some(&hash))).0,
            "PIN_PATH"
        );
        assert_eq!(
            refusal(parse_pin(keys, Some(&shown), Some("beef"))).0,
            "PIN_HASH"
        );
        assert_eq!(
            refusal(parse_pin(keys, Some(&shown), Some(&"0".repeat(64)))).0,
            "PIN_PATH",
            "a program that does not match its pin"
        );
        assert_eq!(
            parse_pin(keys, Some(&shown), Some(&hash)).unwrap(),
            Some(PinnedBinary { path, sha256: hash })
        );
    }

    #[test]
    fn a_vendor_api_is_one_destination_pinned_now() {
        let tls = TlsClient::bundled().unwrap();
        let kind = HarnessKind::CodexCli;
        assert!(parse_vendor(kind, None, &tls).unwrap().is_none());
        assert!(parse_vendor(kind, Some("  "), &tls).unwrap().is_none());
        for bad in [
            "a.example:443,b.example:443",
            "public:a.example:443",
            "local-keyless:localhost:1",
        ] {
            assert_eq!(
                refusal(parse_vendor(kind, Some(bad), &tls)),
                (
                    kind.api_key(),
                    String::from(
                        "one vendor API: host:port, internal:host:port, or local:localhost:PORT"
                    )
                ),
                "{bad}"
            );
        }
        assert_eq!(
            refusal(parse_vendor(kind, Some("no-port"), &tls)).0,
            kind.api_key()
        );
        let local = parse_vendor(kind, Some(" local:localhost:8443 "), &tls)
            .unwrap()
            .unwrap();
        assert_eq!(local.wire, Wire::OpenAi);
        assert_eq!(local.destination, "localhost:8443");
        let claude = parse_vendor(HarnessKind::ClaudeCode, Some("local:localhost:1"), &tls)
            .unwrap()
            .unwrap();
        assert_eq!(claude.wire, Wire::Anthropic);
        assert_eq!(claude.destination, "localhost:1");
        // Held to the allowlist's rule on internal addresses: a vendor that
        // resolves inside is refused unless written internal:, then served
        // as an ordinary HTTPS vendor under its bare name.
        let (key, reason) = refusal(parse_vendor(kind, Some("localhost:8443"), &tls));
        assert_eq!(key, kind.api_key());
        assert!(
            reason.contains("internal address") && reason.contains("internal:localhost:8443"),
            "{reason}"
        );
        let internal = parse_vendor(kind, Some("internal:localhost:8443"), &tls)
            .unwrap()
            .unwrap();
        assert_eq!(internal.destination, "localhost:8443");
        assert!(internal.allowlist.allows("localhost:8443"));
        assert!(internal.allowlist.local("localhost:8443").is_none());
    }

    #[test]
    fn a_cli_cells_tools_are_absolute_programs_or_the_hosts_defaults() {
        let scratch = Scratch::new("tools");
        let tool = scratch.write("bin/tool", b"#!/bin/sh\n");
        let shown = tool.display().to_string();
        assert_eq!(parse_tools(None, false).unwrap(), Vec::new());
        assert_eq!(parse_tools(Some(" "), false).unwrap(), Vec::new());
        assert_eq!(
            refusal(parse_tools(Some(&shown), false)),
            (
                TOOLS_KEY,
                String::from("tools serve a harness with a vendor API, and none is set")
            )
        );
        assert_eq!(
            parse_tools(Some(&format!(" ,{shown}, ")), true).unwrap(),
            vec![named(&tool)]
        );
        assert_eq!(
            refusal(parse_tools(Some(&format!("{shown},{shown}")), true)),
            (TOOLS_KEY, format!("{shown} is named twice"))
        );
        let fifo = scratch.join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )
        .unwrap();
        for (bad, why) in [
            (
                String::from("bin/tool"),
                String::from("bin/tool is not an absolute path"),
            ),
            (
                format!("{}/../tool", scratch.join("bin").display()),
                format!(
                    "{}/../tool is not an absolute path",
                    scratch.join("bin").display()
                ),
            ),
            (
                scratch.join("bin").display().to_string(),
                format!("{} is not a regular file", scratch.join("bin").display()),
            ),
            // A FIFO is refused, not waited on.
            (
                fifo.display().to_string(),
                format!("{} is not a regular file", fifo.display()),
            ),
            (
                scratch.join("missing").display().to_string(),
                format!("{} is unreadable: ", scratch.join("missing").display()),
            ),
        ] {
            let (key, reason) = refusal(parse_tools(Some(&bad), true));
            assert_eq!(key, TOOLS_KEY);
            assert!(
                reason.starts_with(&format!("{bad} is not a program to run: {why}")),
                "{reason}"
            );
        }
        // The defaults: each one that is a program on this host, named by
        // its own path.
        let expected: Vec<Tool> = DEFAULT_TOOLS
            .iter()
            .map(Path::new)
            .filter(|path| path.is_file())
            .map(named)
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(parse_tools(None, true).unwrap(), expected);
    }

    /// `path`, as `parse_tools` names it.
    fn named(path: &Path) -> Tool {
        Tool {
            path: path.to_path_buf(),
            name: path
                .display()
                .to_string()
                .trim_start_matches('/')
                .to_owned(),
        }
    }

    /// `tools`, each pinned as the program it holds now.
    fn pinned_as_now(tools: &[Tool]) -> PinnedTools {
        let found = read_tools(tools);
        let pins = tools
            .iter()
            .zip(&found)
            .map(|(tool, found)| (tool.clone(), found.as_ref().unwrap().0.clone()))
            .collect();
        PinnedTools::pin(pins, found)
    }

    /// WO-019: a program is read through its links, and never past its
    /// bound.
    #[test]
    fn a_program_is_read_through_its_links_within_its_bound() {
        let scratch = Scratch::new("tool-read");
        let path = scratch.write("tool", b"abc");
        let link = scratch.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let (bytes, seen) = read_program(&link).unwrap();
        assert_eq!(bytes, b"abc");
        assert_eq!(seen, Seen::of(&std::fs::metadata(&path).unwrap()));
        let (file, _) = open_program(&path).unwrap();
        assert_eq!(read_file(file, &path, 3).unwrap(), b"abc");
        let (file, _) = open_program(&path).unwrap();
        assert_eq!(
            read_file(file, &path, 2).unwrap_err(),
            format!("{} is more than 2 bytes", path.display())
        );
    }

    /// WO-019: each program is pinned, and copied once: read and execute
    /// only, with its other names hard links to it. The copy is what a cell
    /// is given, never a host path, and it goes when dropped.
    #[test]
    fn the_tools_are_pinned_and_copied_once_per_program() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        let scratch = Scratch::new("tool-pins");
        let first = scratch.write("bin/first", b"one program");
        let same = scratch.write("usr/bin/same", b"one program");
        let linked = scratch.join("bin/linked");
        std::fs::hard_link(&first, &linked).unwrap();
        let other = scratch.write("usr/bin/other", b"another program");
        let tools = [named(&first), named(&same), named(&linked), named(&other)];
        let pinned = pinned_as_now(&tools);
        let pins: Vec<_> = pinned
            .tools
            .iter()
            .map(|pinned| (pinned.tool.clone(), pinned.sha256.clone()))
            .collect();
        let one = CodeHash::of(b"one program").to_string();
        let another = CodeHash::of(b"another program").to_string();
        let [first, same, linked, other] = &tools;
        assert_eq!(
            pins,
            [
                (first.clone(), one.clone()),
                (same.clone(), one.clone()),
                (linked.clone(), one),
                (other.clone(), another)
            ]
        );
        assert_eq!(pinned.check(), Ok(()));
        let copy = pinned.copy().unwrap();
        assert!(Arc::ptr_eq(&copy, &pinned.copy().unwrap()), "made once");
        let at = |tool: &Tool| copy.dir.join(&tool.name);
        assert_eq!(
            copy.for_cell(),
            tools
                .iter()
                .map(|tool| (at(tool), tool.name.clone()))
                .collect::<Vec<_>>()
        );
        for (tool, bytes) in tools.iter().zip([
            "one program",
            "one program",
            "one program",
            "another program",
        ]) {
            assert_eq!(std::fs::read_to_string(at(tool)).unwrap(), bytes);
            let mode = std::fs::metadata(at(tool)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o555, "{}", tool.name);
        }
        let file = |tool: &Tool| std::fs::metadata(at(tool)).unwrap().ino();
        assert_eq!(file(first), file(same), "one copy per program");
        assert_eq!(file(first), file(linked), "one copy per program");
        assert_ne!(file(first), file(other));
        copy.link(&first.name, "more/names/first").unwrap();
        assert_eq!(
            std::fs::metadata(copy.dir.join("more/names/first"))
                .unwrap()
                .ino(),
            file(first),
            "a link is the same program"
        );
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&copy.staging), 0o711);
        assert_eq!(mode(&copy.dir), 0o755);
        assert_eq!(
            mode(at(same).parent().unwrap()),
            0o755,
            "a directory made for a tool"
        );
        let staging = copy.staging.clone();
        drop(copy);
        drop(pinned);
        assert!(!staging.exists(), "the copy goes when dropped");
    }

    /// WO-019: a tool is given only while it's the program pinned. One
    /// written over, or replaced, or gone, is refused, naming it, and only
    /// the log hears its hashes; one written back as it was is fine. A
    /// copy made once a tool changed is refused too, and one made before
    /// keeps what was pinned.
    #[test]
    fn a_tool_is_given_only_while_it_is_the_program_pinned() {
        let scratch = Scratch::new("tool-check");
        let path = scratch.write("bin/tool", b"the program");
        let tool = named(&path);
        let pinned = pinned_as_now(std::slice::from_ref(&tool));
        let copy = pinned.copy().unwrap();
        assert_eq!(pinned.check(), Ok(()));
        let told = format!(
            "the tool {} is unreadable or not the program approved: it is never substituted",
            path.display()
        );
        let differs = format!(
            "the tool {}'s SHA-256 is {}, not the approved {}",
            path.display(),
            CodeHash::of(b"another program"),
            CodeHash::of(b"the program")
        );
        let pin = pinned.tools.first().unwrap();
        assert_eq!(pin.refusal(), told);
        assert_eq!(
            pin.differs(&CodeHash::of(b"another program").to_string()),
            differs
        );
        std::fs::write(&path, b"another program").unwrap();
        assert_eq!(pinned.check(), Err((told.clone(), differs.clone())));
        // Pinned while it's another program: it's read again for every run.
        let stale = PinnedTools::pin(
            vec![(tool.clone(), CodeHash::of(b"the program").to_string())],
            read_tools(std::slice::from_ref(&tool)),
        );
        assert_eq!(stale.tools.first().unwrap().seen, None);
        assert_eq!(
            ToolCopy::take(&stale.tools).unwrap_err(),
            ToolsRefused::Changed(told.clone(), differs)
        );
        let in_copy = copy.dir.join(&tool.name);
        assert_eq!(std::fs::read(&in_copy).unwrap(), b"the program");
        std::fs::write(&path, b"the program").unwrap();
        assert_eq!(pinned.check(), Ok(()), "the same program, written again");
        let third = scratch.write("bin/third", b"a third program");
        std::fs::rename(&third, &path).unwrap();
        assert!(
            matches!(pinned.check(), Err((said, _)) if said == told),
            "replaced"
        );
        std::fs::remove_file(&path).unwrap();
        let Err((said, detail)) = pinned.check() else {
            unreachable!("a tool that's gone is refused")
        };
        assert_eq!(said, told);
        assert!(
            detail.starts_with(&format!("{} is unreadable: ", path.display())),
            "{detail}"
        );
        assert_eq!(std::fs::read(&in_copy).unwrap(), b"the program");
    }

    /// WO-019: only a networked harness's run is given tools: the copy of
    /// the pinned ones, after their check. Unpinned, it isn't run at all.
    #[test]
    fn a_networked_run_is_given_the_pinned_tools_once_checked() {
        let scratch = Scratch::new("tools-for");
        let config = config(&scratch);
        let (binary, mut vars) = settings(&scratch);
        let tool = scratch.write("bin/tool", b"tool");
        vars.insert(
            "HATCHLING_HARNESS_CLAUDE_CODE_API",
            String::from("local:localhost:8443"),
        );
        vars.insert(TOOLS_KEY, tool.display().to_string());
        let unpinned = parsed(&config, &vars).unwrap();
        assert!(unpinned.pinned.is_none());
        assert_eq!(
            unpinned.tools_for(&CLAUDE).unwrap_err(),
            ToolsRefused::Unpinned
        );
        let harnesses = unpinned.pin_tools().unwrap();
        assert!(harnesses.tools_for(&CODEX).unwrap().is_none());
        let given = harnesses.tools_for(&CLAUDE).unwrap().unwrap();
        assert_eq!(given.names, [named(&tool).name]);
        // What a run reads again before it starts: the binary, the tool.
        assert_eq!(
            harnesses.pinned_paths().collect::<Vec<_>>(),
            [binary.path.as_path(), tool.as_path()]
        );
        // The cell's network carries the copy, never the host path.
        let dirs = RunDirs::create().unwrap();
        let vendor = harnesses.vendors.get("claude-code").unwrap();
        let keys = |_: &str| Some(Zeroizing::new(String::from("vendor-key")));
        let (network, relaying) =
            Harnesses::network(&CLAUDE, vendor, &keys, &dirs, (&[], Some(&given))).unwrap();
        assert_eq!(network.tools, given.for_cell());
        assert_eq!(network.relay, RELAY_PORT);
        // The handoff is the broker's alone: the first to connect hands
        // in the run's relay listener.
        assert_eq!(
            std::fs::symlink_metadata(&network.handoff)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(relaying);
        std::fs::write(&tool, b"changed").unwrap();
        assert!(matches!(
            harnesses.tools_for(&CLAUDE),
            Err(ToolsRefused::Changed(told, _)) if told.starts_with(&format!("the tool {} is", tool.display()))
        ));
        std::fs::remove_file(&tool).unwrap();
        assert_eq!(
            refusal(parsed(&config, &vars).and_then(Harnesses::pin_tools)).0,
            TOOLS_KEY
        );
        // No tools: nothing to pin.
        assert!(Harnesses::default().pin_tools().unwrap().pinned.is_none());
    }

    /// WO-019: the first start with a networked harness makes the
    /// approved-tools list, of the tools it was given, each hashed. Later
    /// starts take their tools from the list, pinned to what was approved:
    /// a setting naming other tools stops the start. A tool that changed
    /// or went before a start doesn't stop it: every run that would get it
    /// is refused, until it's the program approved again. A list open to
    /// another user stops the start.
    #[test]
    fn the_approved_list_names_the_tools_from_the_first_start() {
        use crate::approved_tools::{FILE, dir, read};
        let scratch = Scratch::new("tools-list");
        let config = config(&scratch);
        let (_, mut vars) = settings(&scratch);
        let tool = scratch.write("bin/tool", b"tool");
        let other = scratch.write("bin/other", b"other");
        vars.insert(
            "HATCHLING_HARNESS_CLAUDE_CODE_API",
            String::from("local:localhost:8443"),
        );
        vars.insert(TOOLS_KEY, tool.display().to_string());
        let start = |vars: &HashMap<&'static str, String>| {
            parsed(&config, vars).and_then(Harnesses::pin_tools)
        };
        let dir = dir(&config);
        assert_eq!(read(&dir).unwrap(), None);
        let first = start(&vars).unwrap();
        let approved = read(&dir).unwrap().unwrap();
        assert_eq!(
            approved.pins(),
            [(named(&tool), CodeHash::of(b"tool").to_string())]
        );
        assert!(first.tools_for(&CLAUDE).unwrap().is_some());
        // Later starts: the list's tools, whether the setting names them or not.
        assert_eq!(start(&vars).unwrap().tools, [named(&tool)]);
        vars.remove(TOOLS_KEY);
        assert_eq!(start(&vars).unwrap().tools, [named(&tool)]);
        assert_eq!(read(&dir).unwrap(), Some(approved), "kept as it was");
        vars.insert(TOOLS_KEY, other.display().to_string());
        let (key, reason) = refusal(start(&vars));
        assert_eq!(key, TOOLS_KEY);
        assert!(
            reason.starts_with(&format!(
                "names other tools than the approved-tools list in {}",
                dir.display()
            )),
            "{reason}"
        );
        vars.remove(TOOLS_KEY);
        // Changed before the start: started, and its runs refused until
        // it's the program approved.
        std::fs::write(&tool, b"changed").unwrap();
        let changed = start(&vars).unwrap();
        assert!(matches!(
            changed.tools_for(&CLAUDE),
            Err(ToolsRefused::Changed(told, _)) if told.starts_with(&format!("the tool {} is", tool.display()))
        ));
        std::fs::write(&tool, b"tool").unwrap();
        assert!(changed.tools_for(&CLAUDE).unwrap().is_some());
        std::fs::remove_file(&tool).unwrap();
        let gone = start(&vars).unwrap();
        assert!(matches!(
            gone.tools_for(&CLAUDE),
            Err(ToolsRefused::Changed(..))
        ));
        // Open to another user: the start stops.
        std::fs::set_permissions(dir.join(FILE), std::fs::Permissions::from_mode(0o620)).unwrap();
        assert!(matches!(
            start(&vars).unwrap_err(),
            Error::ApprovedTools { reason } if reason.contains("writable by no one else")
        ));
    }

    /// WO-019: with no list and no tools named, the list is made of the
    /// defaults this host has, and the cells get those, as before it.
    #[test]
    fn a_first_start_approves_the_default_tools() {
        let scratch = Scratch::new("tools-defaults");
        let config = config(&scratch);
        let (_, mut vars) = settings(&scratch);
        vars.insert(
            "HATCHLING_HARNESS_CLAUDE_CODE_API",
            String::from("local:localhost:8443"),
        );
        let harnesses = parsed(&config, &vars)
            .and_then(Harnesses::pin_tools)
            .unwrap();
        let defaults = parse_tools(None, true).unwrap();
        assert!(!defaults.is_empty());
        assert_eq!(harnesses.tools, defaults);
        let approved = crate::approved_tools::read(&crate::approved_tools::dir(&config))
            .unwrap()
            .unwrap();
        assert_eq!(approved.tools(), defaults);
        let given = harnesses.tools_for(&CLAUDE).unwrap().unwrap();
        assert_eq!(given.names.len(), defaults.len());
        // Without a network, no list is made.
        let scratch = Scratch::new("tools-offline");
        let config = self::config(&scratch);
        let (_, offline) = settings(&scratch);
        parsed(&config, &offline)
            .and_then(Harnesses::pin_tools)
            .unwrap();
        assert!(!crate::approved_tools::dir(&config).exists());
    }

    #[test]
    fn a_real_cli_needs_its_binary_and_the_code_mode_host_needs_the_codex_cli() {
        let scratch = Scratch::new("network-parse");
        let config = config(&scratch);
        let (_, mut vars) = settings(&scratch);
        let tool = scratch.write("bin/tool", b"tool");
        vars.insert(
            "HATCHLING_HARNESS_CLAUDE_CODE_API",
            String::from("local:localhost:8443"),
        );
        vars.insert(TOOLS_KEY, tool.display().to_string());
        let networked = parsed(&config, &vars).unwrap();
        assert_eq!(
            networked
                .vendors
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["claude-code"]
        );
        // Only a relayed vendor's key is one the broker may read.
        assert_eq!(
            networked.vendor_secret_names(),
            BTreeSet::from([String::from("harness-claude-code")])
        );
        assert_eq!(networked.tools.len(), 1);
        let mut binaryless = vars.clone();
        binaryless.insert(
            "HATCHLING_HARNESS_CODEX_CLI_API",
            String::from("local:localhost:8443"),
        );
        assert_eq!(
            refusal(parsed(&config, &binaryless)),
            (
                "HATCHLING_HARNESS_CODEX_CLI_API",
                String::from("a vendor API needs its harness binary")
            )
        );
        let host = scratch.write("bin/host", b"host");
        let mut hostless = vars.clone();
        hostless.insert(CODEX_HOST_KEY, host.display().to_string());
        hostless.insert(CODEX_HOST_HASH_KEY, CodeHash::of(b"host").to_string());
        assert_eq!(
            refusal(parsed(&config, &hostless)),
            (
                CODEX_HOST_KEY,
                String::from("the code-mode host needs the Codex CLI")
            )
        );
        let mut offline = vars;
        offline.remove("HATCHLING_HARNESS_CLAUDE_CODE_API");
        assert_eq!(refusal(parsed(&config, &offline)).0, TOOLS_KEY);
    }

    #[test]
    fn the_brokers_root_store_reaches_the_vendors_and_changes_nothing_else() {
        let scratch = Scratch::new("with-tls");
        let config = config(&scratch);
        let (binary, mut vars) = settings(&scratch);
        vars.insert(
            "HATCHLING_HARNESS_CLAUDE_CODE_API",
            String::from("local:localhost:8443"),
        );
        let tls = TlsClient::bundled().unwrap();
        let harnesses = parsed(&config, &vars).unwrap().with_tls(&tls);
        assert_eq!(
            harnesses.binaries,
            BTreeMap::from([(String::from("claude-code"), (CLAUDE, binary))])
        );
        assert_eq!(
            harnesses
                .vendors
                .get("claude-code")
                .map(|vendor| vendor.destination.as_str()),
            Some("localhost:8443")
        );
        assert_eq!(
            harnesses.roots,
            BTreeMap::from([(String::from("project"), scratch.join("project"))])
        );
        assert_eq!(
            harnesses.roots().collect::<Vec<_>>(),
            [scratch.join("project").as_path()]
        );
        assert_eq!(harnesses.timeout, Duration::from_secs(7));
        assert!(!harnesses.tools.is_empty(), "the host's default tools");
    }

    #[test]
    fn only_a_codex_run_gets_the_pinned_code_mode_host() {
        let scratch = Scratch::new("host-copy");
        let config = config(&scratch);
        let (_, mut vars) = settings(&scratch);
        let codex = scratch.write("bin/codex", b"codex");
        vars.insert("HATCHLING_HARNESS_CODEX_CLI", codex.display().to_string());
        vars.insert(
            "HATCHLING_HARNESS_CODEX_CLI_SHA256",
            CodeHash::of(b"codex").to_string(),
        );
        let unhosted = parsed(&config, &vars).unwrap();
        assert!(unhosted.companion_copies(&CODEX).unwrap().is_empty());
        let host = scratch.write("bin/host", b"host bytes");
        vars.insert(CODEX_HOST_KEY, host.display().to_string());
        vars.insert(CODEX_HOST_HASH_KEY, CodeHash::of(b"host bytes").to_string());
        let hosted = parsed(&config, &vars).unwrap();
        assert!(hosted.companion_copies(&CLAUDE).unwrap().is_empty());
        let copies = hosted.companion_copies(&CODEX).unwrap();
        let [(copy, name)] = copies.as_slice() else {
            unreachable!("one host: {copies:?}")
        };
        assert_eq!(name, CODEX_HOST);
        assert_eq!(
            std::fs::read(copy.dir().join(CODEX_HOST)).unwrap(),
            b"host bytes"
        );
        std::fs::write(&host, b"changed").unwrap();
        let (told, refused) = hosted.companion_copies(&CODEX).unwrap_err();
        assert_eq!(told, HOST_REFUSED);
        assert!(refused.contains("not the pinned"), "{refused}");
    }

    #[test]
    fn the_vendor_keys_are_the_brokers_own_logged_vault_reads() {
        use crate::vault_access::{Caller, Outcome, read_log};
        let scratch = Scratch::new("keys");
        let root = scratch.join("vault");
        let key = MasterKey::generate().unwrap();
        let vault = SealedVault::open(&root, &key).unwrap();
        vault
            .store(
                &"harness-claude-code".parse().unwrap(),
                &hatchling_vault::Secret::new(b" sk-ant-real \n".to_vec()).unwrap(),
            )
            .unwrap();
        let keys = |secret: &str| vendor_key(&root, &key, secret);
        assert_eq!(
            keys("harness-claude-code").as_deref().map(String::as_str),
            Some("sk-ant-real")
        );
        assert!(keys("harness-codex-cli").is_none(), "not stored");
        vault
            .store(
                &"harness-codex-cli".parse().unwrap(),
                &hatchling_vault::Secret::new(b" \n".to_vec()).unwrap(),
            )
            .unwrap();
        assert!(keys("harness-codex-cli").is_none(), "a blank key is none");
        let other = MasterKey::generate().unwrap();
        assert!(
            vendor_key(&root, &other, "harness-claude-code").is_none(),
            "another key reads nothing"
        );
        let (log, unparsed) = read_log(&root).unwrap();
        assert_eq!(unparsed, 0);
        let broker = Caller::Broker {
            purpose: String::from("harness"),
        };
        assert_eq!(
            log.iter()
                .map(|access| (
                    access.caller.clone(),
                    access.action.as_str(),
                    access.names.clone(),
                    access.outcome
                ))
                .collect::<Vec<_>>(),
            [
                ("harness-claude-code", Outcome::Allowed),
                ("harness-codex-cli", Outcome::NotFound),
                ("harness-codex-cli", Outcome::Allowed),
                ("harness-claude-code", Outcome::Failed),
            ]
            .map(|(name, outcome)| (
                broker.clone(),
                "read",
                vec![String::from(name)],
                outcome
            ))
        );
    }

    #[test]
    fn a_binary_needs_its_pin_and_runs_only_while_it_matches() {
        let scratch = Scratch::new("binary");
        let path = scratch.write("bin/harness", b"stub harness");
        let hash = CodeHash::of(b"stub harness").to_string();
        let shown = path.display().to_string();
        let kind = HarnessKind::CodexCli;
        assert_eq!(parse_binary(kind, None, None).unwrap(), None);
        assert_eq!(
            parse_binary(kind, Some(" "), Some("")).unwrap(),
            None,
            "blank is unset"
        );
        assert_eq!(
            refusal(parse_binary(kind, Some(&shown), None)).0,
            kind.hash_key()
        );
        assert_eq!(
            refusal(parse_binary(kind, None, Some(&hash))).0,
            kind.path_key()
        );
        let (key, reason) = refusal(parse_binary(kind, Some("bin/harness"), Some(&hash)));
        assert_eq!(key, kind.path_key());
        assert!(reason.contains("must be an absolute path"), "{reason}");
        for bad in ["0".repeat(63), "0".repeat(65), "g".repeat(64)] {
            let (key, reason) = refusal(parse_binary(kind, Some(&shown), Some(&bad)));
            assert_eq!(
                (key, reason.as_str()),
                (kind.hash_key(), "a SHA-256 is 64 hexadecimal digits")
            );
        }
        let pinned = parse_binary(
            kind,
            Some(&format!(" {shown} ")),
            Some(&hash.to_ascii_uppercase()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            pinned,
            PinnedBinary {
                path: path.clone(),
                sha256: hash.clone()
            }
        );
        assert_eq!(pinned.verified().unwrap(), b"stub harness");
        let (key, reason) = refusal(parse_binary(kind, Some(&shown), Some(&"0".repeat(64))));
        assert_eq!(
            key,
            kind.path_key(),
            "a binary that does not match its pin does not start"
        );
        assert!(reason.contains("never substituted"), "{reason}");
        std::fs::write(&path, b"changed").unwrap();
        let changed = CodeHash::of(b"changed");
        assert_eq!(
            pinned.verified().unwrap_err(),
            format!(
                "the harness binary's SHA-256 is {changed}, not the pinned {hash}: it is never substituted"
            )
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            pinned
                .verified()
                .unwrap_err()
                .starts_with("the harness binary ")
        );
    }

    #[test]
    fn a_prefix_is_read_up_to_its_limit_and_says_if_there_is_more() {
        let scratch = Scratch::new("prefix");
        let file = scratch.write("file", b"0123456789");
        assert_eq!(
            read_prefix(&file, 10).unwrap(),
            (b"0123456789".to_vec(), false)
        );
        assert_eq!(
            read_prefix(&file, 11).unwrap(),
            (b"0123456789".to_vec(), false)
        );
        assert_eq!(
            read_prefix(&file, 9).unwrap(),
            (b"012345678".to_vec(), true)
        );
        assert_eq!(read_prefix(&file, 0).unwrap(), (Vec::new(), true));
        let empty = scratch.write("empty", b"");
        assert_eq!(read_prefix(&empty, 0).unwrap(), (Vec::new(), false));
        assert!(read_prefix(&scratch.join("missing"), 5).is_err());
        assert_eq!(
            read_regular_prefix(&file, 4).unwrap(),
            (b"0123".to_vec(), true)
        );
        std::os::unix::fs::symlink(&file, scratch.join("link")).unwrap();
        assert!(
            read_regular_prefix(&scratch.join("link"), 4)
                .unwrap_err()
                .ends_with("is not a regular file")
        );
    }

    /// A 64 GiB file, sparse, so it costs no disk: read whole, it would
    /// take 64 GiB of the broker's memory.
    fn huge(path: &Path) {
        std::fs::File::create(path)
            .unwrap()
            .set_len(64 << 30)
            .unwrap();
    }

    /// A cell can leave files as large as its file-size limit. The
    /// broker reads only what each bound allows, so a huge one is
    /// refused (or its question cut) without being read whole.
    #[test]
    fn a_huge_file_from_the_cell_is_never_read_whole() {
        let scratch = Scratch::new("huge");
        std::fs::create_dir(scratch.join("tree")).unwrap();
        huge(&scratch.join("tree").join("big"));
        let refused = read_tree(&scratch.join("tree")).unwrap_err();
        assert!(refused.contains("bytes in its work tree"), "{refused}");
        // Nor a huge file of the owner's that the cell asks to stage.
        let dirs = RunDirs::create().unwrap();
        let refused = stage(&scratch.join("tree"), &[String::from("big")], &dirs).unwrap_err();
        assert!(refused.contains("hold more than"), "{refused}");
        // Within the bound, just: still read.
        let exact = scratch.join("exact");
        std::fs::create_dir(&exact).unwrap();
        std::fs::File::create(exact.join("full"))
            .unwrap()
            .set_len(MAX_WORK_BYTES)
            .unwrap();
        let full = read_tree(&exact).unwrap();
        assert_eq!(full.files.get("full").map(Vec::len), Some(8 << 20));
        std::fs::File::create(exact.join("one more"))
            .unwrap()
            .set_len(1)
            .unwrap();
        assert!(read_tree(&exact).is_err(), "one byte past the bound");
        // The question: cut to its bound.
        std::fs::create_dir(scratch.join("scratch")).unwrap();
        huge(&scratch.join("scratch").join(QUESTION_FILE));
        let question = read_question(&scratch.join("scratch")).unwrap();
        assert!(question.len() <= MAX_QUESTION_BYTES);
    }

    #[test]
    fn read_regular_refuses_links_directories_and_missing_files() {
        let scratch = Scratch::new("regular");
        let file = scratch.write("file", b"bytes");
        std::os::unix::fs::symlink(&file, scratch.join("link")).unwrap();
        assert_eq!(read_regular(&file).unwrap(), b"bytes");
        for refused in [scratch.join("link"), scratch.0.clone()] {
            assert_eq!(
                read_regular(&refused).unwrap_err(),
                format!("{} is not a regular file", refused.display())
            );
        }
        assert!(
            read_regular(&scratch.join("missing"))
                .unwrap_err()
                .contains(" is unreadable: ")
        );
    }

    #[test]
    fn roots_are_named_directories_apart_from_the_vault_and_the_socket() {
        let scratch = Scratch::new("roots");
        let mut config = config(&scratch);
        let project = scratch.dir("project").display().to_string();
        let other = scratch.dir("other").display().to_string();
        let roots = parse_roots(Some(&format!(" a={project} , b.2 = {other} ,")), &config).unwrap();
        assert_eq!(
            roots,
            BTreeMap::from([
                (String::from("a"), PathBuf::from(&project)),
                (String::from("b.2"), PathBuf::from(&other)),
            ])
        );
        assert!(parse_roots(None, &config).unwrap().is_empty());
        assert!(parse_roots(Some(" , "), &config).unwrap().is_empty());
        let refused = |raw: String, config: &Config| {
            let (key, reason) = refusal(parse_roots(Some(&raw), config));
            assert_eq!(key, "HATCHLING_HARNESS_ROOTS");
            reason
        };
        assert!(refused(project.clone(), &config).ends_with("is not name=/absolute/dir"));
        for name in [".a", "a b", "a/b", ""] {
            let entry = format!("{name}={project}");
            assert!(
                refused(entry, &config).contains("is not a root name"),
                "{name}"
            );
        }
        let file = scratch.write("file", b"x").display().to_string();
        for dir in [String::from("project"), format!("{project}/missing"), file] {
            let entry = format!("a={dir}");
            assert!(refused(entry, &config).contains("must be an existing absolute directory"));
        }
        let inside_vault = scratch.dir("vault/inner").display().to_string();
        let socket_dir = scratch.dir("sock").display().to_string();
        let around_both = scratch.0.display().to_string();
        for dir in [inside_vault, socket_dir, around_both.clone()] {
            let reason = refused(format!("a={dir}"), &config);
            assert_eq!(
                reason,
                "the root \"a\" overlaps the vault or the broker socket directory"
            );
        }
        assert_eq!(
            refused(format!("a={project},a={other}"), &config),
            "the root \"a\" is named twice"
        );
        // An unset vault overlaps nothing; the socket's directory still does.
        config.vault.root = PathBuf::new();
        config.socket = "/run/hatchling-elsewhere/broker.sock".parse().unwrap();
        assert_eq!(
            parse_roots(Some(&format!("all={around_both}")), &config)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn the_timeout_is_whole_seconds_within_its_bound() {
        assert_eq!(DEFAULT_HARNESS_TIMEOUT, Duration::from_secs(600));
        assert_eq!(parse_timeout(None).unwrap(), DEFAULT_HARNESS_TIMEOUT);
        assert_eq!(parse_timeout(Some(" ")).unwrap(), DEFAULT_HARNESS_TIMEOUT);
        assert_eq!(parse_timeout(Some(" 1 ")).unwrap(), Duration::from_secs(1));
        assert_eq!(
            parse_timeout(Some("3600")).unwrap(),
            Duration::from_secs(MAX_HARNESS_TIMEOUT_SECS)
        );
        for bad in ["0", "3601", "-1", "1.5", "ten"] {
            let (key, reason) = refusal(parse_timeout(Some(bad)));
            assert_eq!(key, "HATCHLING_HARNESS_TIMEOUT_SECS");
            assert_eq!(
                reason,
                format!("must be whole seconds from 1 to 3600, not {bad:?}")
            );
        }
    }

    #[test]
    fn a_harness_needs_roots_a_workspace_and_its_own_grant() {
        let scratch = Scratch::new("parse");
        let mut config = config(&scratch);
        let (binary, vars) = settings(&scratch);
        let none = parsed(&config, &HashMap::new()).unwrap();
        assert!(none.is_empty());
        assert_eq!(none.timeout, DEFAULT_HARNESS_TIMEOUT);
        let from_env = Harnesses::from_env(&config).unwrap();
        assert!(
            from_env.is_empty(),
            "nothing in this environment configures a harness"
        );
        assert_eq!(from_env.timeout, DEFAULT_HARNESS_TIMEOUT);
        let ungranted = parsed(&config, &vars).unwrap();
        assert!(!ungranted.is_empty());
        assert_eq!(
            ungranted.binaries,
            BTreeMap::from([(String::from("claude-code"), (CLAUDE, binary))])
        );
        assert!(ungranted.granted.is_empty(), "configured is not granted");
        assert_eq!(
            ungranted.roots,
            BTreeMap::from([(String::from("project"), scratch.join("project"))])
        );
        assert_eq!(ungranted.timeout, Duration::from_secs(7));
        assert_eq!(ungranted.workspace, Some(scratch.join("workspace")));
        assert_eq!(ungranted.activity_log, None, "scheduling is off");
        grant(&mut config, "claude-code", "text.transform");
        assert!(
            parsed(&config, &vars).unwrap().granted.is_empty(),
            "another capability is not code"
        );
        grant(&mut config, "claude-code", HARNESS_CAPABILITY);
        grant(&mut config, "codex-cli", HARNESS_CAPABILITY);
        config.scheduler.root = scratch.join("schedules");
        let granted = parsed(&config, &vars).unwrap();
        assert_eq!(
            granted.granted,
            BTreeSet::from([String::from("claude-code")]),
            "a grant without a binary is nothing"
        );
        assert_eq!(
            granted.activity_log,
            Some(scratch.join("schedules.activity"))
        );
        let mut rootless = vars.clone();
        rootless.remove("HATCHLING_HARNESS_ROOTS");
        assert_eq!(
            refusal(parsed(&config, &rootless)).0,
            "HATCHLING_HARNESS_ROOTS"
        );
        let mut binaryless = vars.clone();
        binaryless.retain(|key, _| !key.starts_with("HATCHLING_HARNESS_CLAUDE_CODE"));
        config.artifacts.workspace = None;
        assert!(
            parsed(&config, &binaryless).unwrap().is_empty(),
            "roots alone need nothing"
        );
        assert_eq!(
            refusal(parsed(&config, &vars)).0,
            "HATCHLING_ARTIFACT_WORKSPACE"
        );
    }

    #[test]
    fn a_call_is_refused_unless_its_harness_is_configured_granted_and_runnable() {
        let scratch = Scratch::new("handle");
        let mut config = config(&scratch);
        let (_, vars) = settings(&scratch);
        let runtime = SkillRuntime::new(PathBuf::from("/nonexistent/hatchling-spawner"));
        let ungranted = parsed(&config, &vars).unwrap();
        grant(&mut config, "claude-code", HARNESS_CAPABILITY);
        let harnesses = parsed(&config, &vars).unwrap();
        let brief = r#"{"task":"Go.","root":"project","paths":["a"]}"#;
        assert_eq!(
            harnesses.handle(
                &invoke("fixture@1.0.0", "code", brief),
                Some(&runtime),
                &no_keys
            ),
            None
        );
        assert_eq!(
            harnesses.handle(
                &invoke("codex-cli@1", "code", brief),
                Some(&runtime),
                &no_keys
            ),
            Some(SkillResult::UnknownSkill)
        );
        assert_eq!(
            harnesses.handle(
                &invoke("claude-code@1", "text.transform", brief),
                Some(&runtime),
                &no_keys
            ),
            Some(SkillResult::CapabilityDenied)
        );
        assert_eq!(
            ungranted.handle(
                &invoke("claude-code@1", "code", brief),
                Some(&runtime),
                &no_keys
            ),
            Some(SkillResult::CapabilityDenied)
        );
        assert_eq!(
            harnesses.handle(&invoke("claude-code@1", "code", brief), None, &no_keys),
            Some(SkillResult::SkillError)
        );
    }

    /// Writes two manifests into `scratch`'s `manifests`: `opencode`,
    /// speaking Anthropic's API with a companion, and `aider`, speaking
    /// `OpenAI`'s with a `LANG` of its own; the directory and the companion.
    fn manifests(scratch: &Scratch) -> (PathBuf, PathBuf) {
        let opencode = scratch.write("bin/opencode", b"opencode");
        let aider = scratch.write("bin/aider", b"aider");
        let helper = scratch.write("bin/helper", b"helper");
        let dir = scratch.dir("manifests");
        let pin = |path: &Path, bytes: &[u8]| json!({"path": path.display().to_string(), "sha256": CodeHash::of(bytes).to_string()});
        let helper_pin = json!({
            "path": helper.display().to_string(),
            "sha256": CodeHash::of(b"helper").to_string(),
            "name": "helper",
        });
        let write = |name: &str, manifest: &Value| {
            let path = dir.join(format!("{name}.json"));
            std::fs::write(&path, manifest.to_string()).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        };
        write(
            "opencode",
            &json!({
                "version": 1,
                "name": "opencode",
                "binary": pin(&opencode, b"opencode"),
                "companions": [helper_pin],
                "api": "anthropic",
                "destination": "local:localhost:8443",
                "args": ["run", "{task}"],
                "env": [
                    {"name": "ANTHROPIC_API_KEY", "value": "{key}"},
                    {"name": "ANTHROPIC_BASE_URL", "value": "{relay}"}
                ]
            }),
        );
        write(
            "aider",
            &json!({
                "version": 1,
                "name": "aider",
                "binary": pin(&aider, b"aider"),
                "api": "openai",
                "destination": "local:localhost:8444",
                "args": ["--message", "{task}", "--openai-api-base", "{relay}/v1"],
                "env": [
                    {"name": "OPENAI_API_KEY", "value": "{key}"},
                    {"name": "LANG", "value": "en_GB.UTF-8"}
                ]
            }),
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, helper)
    }

    /// A configuration with the two manifests of [`manifests`]: the
    /// config, its settings, the manifests' directory and the companion.
    fn with_manifests(
        scratch: &Scratch,
    ) -> (Config, HashMap<&'static str, String>, PathBuf, PathBuf) {
        let config = config(scratch);
        let (_, mut vars) = settings(scratch);
        let (dir, helper) = manifests(scratch);
        vars.insert(
            crate::harness_manifest::MANIFESTS_KEY,
            dir.display().to_string(),
        );
        (config, vars, dir, helper)
    }

    #[test]
    fn a_manifests_harness_is_configured_granted_and_named_as_a_built_in_one() {
        let scratch = Scratch::new("manifests");
        let (mut config, vars, _, helper) = with_manifests(&scratch);
        let ungranted = parsed(&config, &vars).unwrap();
        assert_eq!(
            ungranted
                .binaries
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["aider", "claude-code", "opencode"]
        );
        assert!(ungranted.granted.is_empty());
        grant(&mut config, "opencode", HARNESS_CAPABILITY);
        let harnesses = parsed(&config, &vars).unwrap();
        assert_eq!(
            harnesses.granted,
            BTreeSet::from([String::from("opencode")])
        );
        let (opencode, binary) = harnesses.binaries.get("opencode").unwrap();
        assert_eq!(opencode.name(), "opencode");
        assert_eq!(opencode.skill_id(), "opencode@1");
        assert_eq!(opencode.secret_name(), "harness-opencode");
        assert_eq!(binary.sha256, CodeHash::of(b"opencode").to_string());
        let vendor = |name: &str| harnesses.vendors.get(name).unwrap();
        assert_eq!(vendor("opencode").destination, "localhost:8443");
        assert_eq!(vendor("aider").wire, Wire::OpenAi);
        assert_eq!(
            harnesses.vendor_secret_names(),
            BTreeSet::from([
                String::from("harness-aider"),
                String::from("harness-opencode")
            ])
        );
        // Its binary and companion are read again before each run.
        let pinned: Vec<&Path> = harnesses.pinned_paths().collect();
        assert!(pinned.contains(&binary.path.as_path()), "{pinned:?}");
        assert!(pinned.contains(&helper.as_path()), "{pinned:?}");
        // Each name's secret is the vault's `harness-<name>`, the built-in
        // harnesses' too.
        for kind in HarnessKind::ALL {
            assert_eq!(secret_name(kind.name()), kind.secret_name());
            assert_eq!(Harness::BuiltIn(kind).secret_name(), kind.secret_name());
        }
        // The key's shape and the environment are its API's and its own.
        let (aider, _) = harnesses.binaries.get("aider").unwrap();
        assert_eq!(opencode.surrogate_prefix(), "sk-ant-hatchling-");
        assert_eq!(aider.surrogate_prefix(), "sk-hatchling-");
        assert_eq!(opencode.args("Go.", 9), ["run", "Go."]);
        let pair = |name: &str, value: &str| (name.to_owned(), value.to_owned());
        assert_eq!(
            opencode.env(9, "sk-x"),
            [
                pair("PATH", "/usr/bin:/bin"),
                pair("LANG", "C.UTF-8"),
                pair("ANTHROPIC_API_KEY", "sk-x"),
                pair("ANTHROPIC_BASE_URL", "http://127.0.0.1:9"),
            ]
        );
        assert_eq!(
            aider.env(9, "sk-y"),
            [
                pair("PATH", "/usr/bin:/bin"),
                pair("OPENAI_API_KEY", "sk-y"),
                pair("LANG", "en_GB.UTF-8"),
            ]
        );
    }

    #[test]
    fn a_manifests_harness_runs_with_its_own_key_companions_and_grant() {
        let scratch = Scratch::new("manifest-runs");
        let (mut config, vars, dir, helper) = with_manifests(&scratch);
        grant(&mut config, "opencode", HARNESS_CAPABILITY);
        let harnesses = parsed(&config, &vars).unwrap();
        let (opencode, _) = harnesses.binaries.get("opencode").unwrap();
        let (aider, _) = harnesses.binaries.get("aider").unwrap();
        let vendor = |name: &str| harnesses.vendors.get(name).unwrap();
        // Its companion is copied from verified bytes, at its name, and
        // refused once it isn't the program pinned.
        assert!(harnesses.companion_copies(aider).unwrap().is_empty());
        let copies = harnesses.companion_copies(opencode).unwrap();
        let [(copy, name)] = copies.as_slice() else {
            unreachable!("one companion: {copies:?}")
        };
        assert_eq!(name, "helper");
        assert_eq!(std::fs::read(copy.dir().join("helper")).unwrap(), b"helper");
        // Its run's network: its key read from its own secret, its
        // surrogate its API's shape, its companion among its tools.
        let dirs = RunDirs::create().unwrap();
        let keys = |secret: &str| {
            (secret == "harness-opencode").then(|| Zeroizing::new(String::from("vendor-key")))
        };
        let (network, _) =
            Harnesses::network(opencode, vendor("opencode"), &keys, &dirs, (&copies, None))
                .unwrap();
        assert_eq!(
            network.tools,
            [(copy.dir().join("helper"), String::from("helper"))]
        );
        let key = network
            .env
            .iter()
            .find(|(name, _)| name == "ANTHROPIC_API_KEY")
            .map(|(_, value)| value.as_str())
            .unwrap();
        assert!(key.starts_with("sk-ant-hatchling-"), "{key}");
        drop(copies);
        let dirs = RunDirs::create().unwrap();
        assert_eq!(
            Harnesses::network(aider, vendor("aider"), &keys, &dirs, (&[], None)).err(),
            Some(no_key("harness-aider"))
        );
        std::fs::write(&helper, b"changed").unwrap();
        let (told, detail) = harnesses.companion_copies(opencode).unwrap_err();
        assert_eq!(told, COMPANION_REFUSED);
        assert!(detail.contains("not the pinned"), "{detail}");
        std::fs::write(&helper, b"helper").unwrap();
        // The agent calls it by its skill id; its grant and a runtime are
        // needed, as for the built-in harnesses.
        let brief = r#"{"task":"Go.","root":"project","paths":["a"]}"#;
        assert_eq!(
            harnesses.handle(&invoke("opencode@1", "code", brief), None, &no_keys),
            Some(SkillResult::SkillError)
        );
        assert_eq!(
            harnesses.handle(&invoke("aider@1", "code", brief), None, &no_keys),
            Some(SkillResult::CapabilityDenied)
        );
        assert_eq!(
            Harnesses::default().handle(&invoke("opencode@1", "code", brief), None, &no_keys),
            None,
            "unconfigured, it's no harness"
        );
        // A vault grant naming its key refuses the start.
        config.vault.grants = vec![crate::VaultGrant {
            action: hatchling_core::VaultAction::Store,
            name: "harness-opencode".parse().unwrap(),
        }];
        assert_eq!(
            refusal(parsed(&config, &vars)),
            (
                "HATCHLING_VAULT_GRANTS",
                String::from(
                    "grant store:harness-opencode names the vendor key the broker holds for the \
                     opencode harness"
                )
            )
        );
        config.vault.grants = vec![crate::VaultGrant {
            action: hatchling_core::VaultAction::Store,
            name: "harness-other".parse().unwrap(),
        }];
        assert!(parsed(&config, &vars).is_ok());
        // A wrong manifest refuses the start, naming it.
        std::fs::write(dir.join("aider.json"), "{}").unwrap();
        assert!(matches!(
            parsed(&config, &vars),
            Err(Error::HarnessManifest { field, .. }) if field == "json"
        ));
    }

    /// The report's field at `pointer` (`Null` when absent).
    fn field(report: &Value, pointer: &str) -> Value {
        report.pointer(pointer).cloned().unwrap_or(Value::Null)
    }

    #[test]
    fn skills_hand_harness_calls_to_their_harnesses_and_nothing_else() {
        let scratch = Scratch::new("skills");
        let mut config = config(&scratch);
        let (_, vars) = settings(&scratch);
        grant(&mut config, "claude-code", HARNESS_CAPABILITY);
        let call = invoke("claude-code@1", "code", "[]");
        assert_eq!(
            crate::Skills::default().handle_harness(&call, &no_keys),
            Some(SkillResult::UnknownSkill)
        );
        let skills = crate::Skills::default().with_harnesses(parsed(&config, &vars).unwrap());
        assert_eq!(
            skills.handle_harness(&invoke("fixture@1.0.0", "code", "[]"), &no_keys),
            None
        );
        assert_eq!(
            skills.handle_harness(&call, &no_keys),
            Some(SkillResult::SkillError),
            "no runtime, no run"
        );
        let runtime = SkillRuntime::new(PathBuf::from("/nonexistent/hatchling-spawner"));
        let running = crate::Skills::default()
            .with_runtime(runtime)
            .with_harnesses(parsed(&config, &vars).unwrap());
        assert!(matches!(
            running.handle_harness(&call, &no_keys),
            Some(SkillResult::Output { .. })
        ));
    }

    /// The report a call returns, decoded.
    fn reported(harnesses: &Harnesses, input: &str) -> Value {
        let runtime = SkillRuntime::new(PathBuf::from("/nonexistent/hatchling-spawner"));
        let Some(SkillResult::Output { output }) = harnesses.handle(
            &invoke("claude-code@1", "code", input),
            Some(&runtime),
            &no_keys,
        ) else {
            unreachable!("no report for {input}");
        };
        serde_json::from_str(&output).unwrap()
    }

    #[test]
    fn a_run_refused_before_its_cell_is_reported_and_recorded() {
        let scratch = Scratch::new("refused");
        let mut config = config(&scratch);
        let (binary, vars) = settings(&scratch);
        grant(&mut config, "claude-code", HARNESS_CAPABILITY);
        config.scheduler.root = scratch.join("schedules");
        let harnesses = parsed(&config, &vars).unwrap();
        let input = r#"{"task":"Go.","root":"elsewhere","paths":["a"]}"#;
        let report = reported(&harnesses, input);
        assert_eq!(
            report,
            json!({
                "harness": "claude-code",
                "binary_sha256": binary.sha256,
                "brief_sha256": CodeHash::of(input.as_bytes()).to_string(),
                "outcome": "refused",
                "reason": "\"elsewhere\" is not a configured project root",
                "files_changed": [], "files_added": [], "files_removed": [], "unpatchable": [],
                "harness_said": { "message": "" },
            })
        );
        let not_a_brief = reported(&harnesses, "[]");
        assert!(
            field(&not_a_brief, "/reason")
                .as_str()
                .unwrap()
                .starts_with("the brief is not a harness brief")
        );
        let missing = reported(
            &harnesses,
            r#"{"task":"Go.","root":"project","paths":["a"]}"#,
        );
        assert_eq!(field(&missing, "/reason"), "\"a\" is not in the root");
        let log = std::fs::read_to_string(scratch.join("schedules.activity")).unwrap();
        assert_eq!(mode(&scratch.join("schedules.activity")), 0o600);
        let events: Vec<ActivityEvent> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.len(), 3, "every call is recorded");
        let ids: BTreeSet<&str> = events.iter().map(|event| event.event_id.as_str()).collect();
        assert_eq!(ids.len(), 3, "each with an id of its own");
        let first = events.first().unwrap();
        let prefix = format!("harness:{}:", std::process::id());
        assert!(first.event_id.starts_with(&prefix));
        assert_eq!(first.run_id, HARNESS_RUN_ID);
        assert_eq!(
            first.kind,
            SubagentEvent::HarnessRun {
                harness: String::from("claude-code"),
                binary_sha256: binary.sha256.clone(),
                brief_sha256: CodeHash::of(input.as_bytes()).to_string(),
                outcome: HarnessOutcome::Refused,
                files_changed: 0,
                network: None,
            }
        );
        // A binary changed since start-up is refused at the run.
        std::fs::write(&binary.path, b"substitute").unwrap();
        let substituted = reported(
            &harnesses,
            r#"{"task":"Go.","root":"project","paths":["a"]}"#,
        );
        // The cell is told a fixed sentence: not the binary's path, not
        // the hashes, not an OS error. Those are in the broker's log.
        assert_eq!(field(&substituted, "/reason"), BINARY_REFUSED);
    }

    #[test]
    fn a_refusal_on_the_brokers_side_tells_the_cell_only_its_sentence() {
        let scratch = Scratch::new("refused-for");
        let path = scratch.write("bin/harness", b"stub harness");
        let binary = PinnedBinary {
            path,
            sha256: CodeHash::of(b"stub harness").to_string(),
        };
        let report = Report::new(&CLAUDE, &binary, "{}").refused_for(
            HOST_REFUSED,
            "/opt/codex/bin/host is unreadable: Permission denied (os error 13)",
        );
        assert_eq!(report.outcome, HarnessOutcome::Refused);
        assert_eq!(report.reason.as_deref(), Some(HOST_REFUSED));
    }

    #[test]
    fn a_harness_that_cannot_start_failed_and_changed_nothing() {
        let scratch = Scratch::new("unstartable");
        let config = config(&scratch);
        let (binary, vars) = settings(&scratch);
        scratch.write("project/a", b"a\n");
        let harnesses = parsed(&config, &vars).unwrap();
        let runtime = SkillRuntime::new(PathBuf::from("/nonexistent/hatchling-spawner"));
        let input = r#"{"task":"Go.","root":"project","paths":["a"]}"#;
        let report = harnesses.run(&CLAUDE, &binary, input, &runtime, &no_keys);
        assert_eq!(report.outcome, HarnessOutcome::Failed);
        assert_eq!(report.reason.as_deref(), Some("the harness could not run"));
        assert_eq!((report.change_count(), report.patch.as_ref()), (0, None));
        assert_eq!(
            std::fs::read_dir(scratch.join("workspace"))
                .unwrap()
                .count(),
            0,
            "no patch for no change"
        );
    }

    #[test]
    fn a_brief_names_a_task_a_root_and_a_bounded_slice() {
        let parse = |value: Value| Brief::parse(&value.to_string());
        assert_eq!(
            parse(json!({ "task": "Do it.", "root": "project", "paths": ["src/lib.rs", "b"] }))
                .unwrap(),
            Brief {
                task: String::from("Do it."),
                root: String::from("project"),
                paths: vec![String::from("src/lib.rs"), String::from("b")],
                answer: None,
            }
        );
        assert!(
            Brief::parse("[]")
                .unwrap_err()
                .starts_with("the brief is not a harness brief: ")
        );
        let extra = parse(json!({ "task": "t", "root": "r", "paths": ["a"], "run": "x" }));
        assert!(
            extra
                .unwrap_err()
                .starts_with("the brief is not a harness brief: "),
            "unknown fields are refused"
        );
        let task = |task: &str| parse(json!({ "task": task, "root": "r", "paths": ["a"] }));
        assert!(task(&"x".repeat(MAX_TASK_BYTES)).is_ok());
        for bad in [
            "x".repeat(MAX_TASK_BYTES.saturating_add(1)),
            String::from(" \n"),
            String::new(),
        ] {
            assert_eq!(task(&bad).unwrap_err(), "the task must be 1 to 4096 bytes");
        }
        assert_eq!(
            task("a\0b").unwrap_err(),
            "the brief may not contain NUL bytes"
        );
        let answer = |answer: &str| {
            parse(json!({ "task": "t", "root": "r", "paths": ["a"], "answer": answer }))
        };
        let longest = "y".repeat(MAX_ANSWER_BYTES);
        assert_eq!(answer(&longest).unwrap().answer, Some(longest.clone()));
        assert_eq!(
            answer(&format!("{longest}y")).unwrap_err(),
            "the answer must be at most 2048 bytes"
        );
        assert_eq!(
            answer("a\0").unwrap_err(),
            "the brief may not contain NUL bytes"
        );
        let paths = |paths: Vec<String>| parse(json!({ "task": "t", "root": "r", "paths": paths }));
        let many = |n: usize| (0..n).map(|i| format!("f{i}")).collect::<Vec<_>>();
        assert!(paths(many(MAX_SLICE_FILES)).is_ok());
        for bad in [many(MAX_SLICE_FILES.saturating_add(1)), Vec::new()] {
            assert_eq!(paths(bad).unwrap_err(), "a brief names 1 to 64 files");
        }
        let twice = vec![String::from("a"), String::from("a")];
        assert_eq!(paths(twice).unwrap_err(), "the brief names a file twice");
        let escaping = vec![String::from("a"), String::from("../b")];
        assert_eq!(
            paths(escaping).unwrap_err(),
            "\"../b\" is not a plain relative path inside the root"
        );
    }

    #[test]
    fn the_prompt_carries_the_task_the_answer_the_files_and_how_to_ask() {
        let mut brief = Brief {
            task: String::from("Add a helper."),
            root: String::from("project"),
            paths: vec![String::from("src/lib.rs"), String::from("README")],
            answer: None,
        };
        let tail = "The files are in your working directory: src/lib.rs, README. Change only what the task needs; new files go beside them.\n\
                    If you cannot continue without an answer, write one question to /scratch/question and stop.";
        assert_eq!(
            brief.prompt(),
            format!("You are working on a task for Hatchling.\n\nTask:\nAdd a helper.\n\n{tail}")
        );
        brief.answer = Some(String::from("In src/lib.rs."));
        assert_eq!(
            brief.prompt(),
            format!(
                "You are working on a task for Hatchling.\n\nTask:\nAdd a helper.\n\nThe answer to your earlier question:\nIn src/lib.rs.\n\n{tail}"
            )
        );
    }

    #[test]
    fn only_plain_relative_paths_are_safe() {
        for safe in ["a", "src/lib.rs", "a-b_c.d/e", "a//b", "a/b/"] {
            assert!(safe_relative(safe), "{safe}");
        }
        for unsafe_path in [
            "", "/a", "../a", "a/../b", "./a", "a b", "a:b", "é", "a\\b", "~", ".", "..",
        ] {
            assert!(!safe_relative(unsafe_path), "{unsafe_path}");
        }
    }

    #[test]
    fn staging_copies_plain_text_files_into_private_trees_and_nothing_else() {
        let scratch = Scratch::new("stage");
        let root = scratch.dir("root");
        scratch.write("root/src/lib.rs", b"fn a() {}\n");
        scratch.write("root/notes", b"n");
        scratch.write("root/blob", &[0xff, 0x00]);
        scratch.dir("root/dir");
        std::os::unix::fs::symlink(root.join("src"), root.join("linked")).unwrap();
        let dirs = RunDirs::create().unwrap();
        let base = dirs.base.clone();
        assert_eq!(
            [&dirs.work, &dirs.pristine, &dirs.scratch]
                .map(|dir| dir.strip_prefix(&base).unwrap().to_path_buf()),
            [
                PathBuf::from("work"),
                PathBuf::from("pristine"),
                PathBuf::from("scratch")
            ]
        );
        assert_eq!(
            mode(&dirs.base),
            0o711,
            "searchable, listed by its owner alone"
        );
        for dir in [&dirs.work, &dirs.pristine, &dirs.scratch] {
            assert_eq!(mode(dir), 0o700, "{}", dir.display());
        }
        stage(
            &root,
            &[String::from("src/lib.rs"), String::from("notes")],
            &dirs,
        )
        .unwrap();
        for tree in [&dirs.work, &dirs.pristine] {
            assert_eq!(
                std::fs::read(tree.join("src/lib.rs")).unwrap(),
                b"fn a() {}\n"
            );
            assert_eq!(std::fs::read(tree.join("notes")).unwrap(), b"n");
            assert_eq!(
                (mode(&tree.join("src")), mode(&tree.join("notes"))),
                (0o700, 0o600)
            );
        }
        let refused = |path: &str| stage(&root, &[String::from(path)], &dirs).unwrap_err();
        assert_eq!(
            refused("linked/lib.rs"),
            "\"linked/lib.rs\" goes through a symlink"
        );
        assert_eq!(refused("linked"), "\"linked\" goes through a symlink");
        assert_eq!(refused("missing"), "\"missing\" is not in the root");
        assert_eq!(refused("dir"), "\"dir\" is not a regular file");
        assert_eq!(refused("blob"), "\"blob\" is not text");
        // Staging never overwrites, and says so in the brief's own terms:
        // no host path, no OS error.
        assert_eq!(refused("notes"), "cannot stage \"notes\"");
        let other = RunDirs::create().unwrap();
        assert_ne!(other.base, base);
        drop(dirs);
        assert!(!base.exists(), "a run's directories go with it");
    }

    #[test]
    fn a_slice_holds_at_most_its_byte_bound() {
        let scratch = Scratch::new("slice-bound");
        let root = scratch.dir("root");
        let bound = usize::try_from(MAX_SLICE_BYTES).unwrap();
        scratch.write("root/half", &vec![b'a'; bound / 2]);
        scratch.write("root/rest", &vec![b'b'; bound - bound / 2]);
        scratch.write("root/one", b"c");
        let names = |names: &[&str]| {
            names
                .iter()
                .map(|name| String::from(*name))
                .collect::<Vec<_>>()
        };
        assert!(
            stage(
                &root,
                &names(&["half", "rest"]),
                &RunDirs::create().unwrap()
            )
            .is_ok(),
            "exactly the bound"
        );
        assert_eq!(
            stage(
                &root,
                &names(&["half", "rest", "one"]),
                &RunDirs::create().unwrap()
            )
            .unwrap_err(),
            "the files hold more than 1048576 bytes"
        );
    }

    /// The built-in harnesses.
    const CLAUDE: Harness = Harness::BuiltIn(HarnessKind::ClaudeCode);
    const CODEX: Harness = Harness::BuiltIn(HarnessKind::CodexCli);

    /// A key reader with no key: an offline harness never asks it.
    const fn no_keys(_: &str) -> Option<Zeroizing<String>> {
        None
    }

    #[test]
    fn a_harness_cell_runs_the_verified_copy_on_the_run_trees() {
        let dirs = RunDirs::create().unwrap();
        let cell = harness_cell(
            &CODEX,
            "The prompt.",
            Path::new("/staged/code"),
            &dirs,
            None,
            CellIds::Own,
        )
        .unwrap();
        assert_eq!(cell.skill_code.as_deref(), Some(Path::new("/staged/code")));
        assert_eq!(cell.agent_bin, PathBuf::from("/staged/code/harness"));
        assert_eq!(
            cell.harness,
            Some(HarnessDirs {
                work: dirs.work.clone(),
                scratch: dirs.scratch.clone(),
                args: HarnessKind::CodexCli.args("The prompt.", RELAY_PORT),
                network: None,
            })
        );
        assert!(cell.is_harness_cell());
    }

    #[test]
    fn a_runs_trees_move_by_chown_or_through_the_helper() {
        let dirs = RunDirs::create().unwrap();
        let mut cell = CellConfig::skill("harness").unwrap();
        cell.ids = CellIds::Own;
        let runtime = SkillRuntime::new(PathBuf::from("/nonexistent/hatchling-broker"));
        // A root broker takes nothing back: it reads anything.
        assert_eq!(trees(&runtime, &cell, &dirs, TreeOp::Reclaim), Ok(()));
        // On subordinate ids, both ways go through the helper: with no
        // helper binary, nothing moves.
        cell.ids = CellIds::Subordinate;
        for op in [TreeOp::Give, TreeOp::Reclaim] {
            assert!(trees(&runtime, &cell, &dirs, op).is_err(), "{op:?}");
        }
    }

    #[test]
    fn the_run_trees_are_handed_to_the_cells_uid() {
        let dirs = RunDirs::create().unwrap();
        write_new(&dirs.work.join("src/a"), b"a").unwrap();
        write_new(&dirs.scratch.join("b"), b"b").unwrap();
        let everything = [
            dirs.work.clone(),
            dirs.work.join("src"),
            dirs.work.join("src/a"),
            dirs.scratch.clone(),
            dirs.scratch.join("b"),
        ];
        let me = hatchling_sys::euid();
        hand_over(&dirs, me).unwrap();
        assert!(everything.iter().all(|path| owner(path) == me));
        if me == 0 {
            hand_over(&dirs, 65534).unwrap();
            assert!(
                everything.iter().all(|path| owner(path) == 65534),
                "every path, nested ones too"
            );
            assert_eq!(
                owner(&dirs.pristine),
                0,
                "the pristine copy stays the broker's"
            );
        } else {
            let refused = hand_over(&dirs, 0).unwrap_err();
            assert!(refused.starts_with("cannot hand "), "{refused}");
        }
    }

    #[test]
    fn what_the_harness_said_is_its_result_or_its_words_cut_to_size() {
        assert_eq!(
            final_message(br#"{"type":"result","result":"All done."}"#),
            "All done."
        );
        assert_eq!(final_message(b" {\"result\":\"x\"}\n"), "x");
        assert_eq!(final_message(br#"{"result":3}"#), r#"{"result":3}"#);
        assert_eq!(final_message(b"plain words\n"), "plain words\n");
        assert_eq!(final_message(&[b'f', 0xff]), "f\u{fffd}");
        // The Codex CLI's JSONL: the last completed agent message,
        // not a started one, not another kind of item.
        let codex = concat!(
            r#"{"type":"thread.started","thread_id":"t"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"first"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"command_execution","text":"ls"}}"#,
            "\n",
            r#"{"type":"item.started","item":{"type":"agent_message","text":"started"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"last"}}"#,
            "\n",
            r#"{"type":"item.updated","item":{"type":"agent_message","text":"updated"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{}}"#,
            "\n",
        );
        assert_eq!(final_message(codex.as_bytes()), "last");
        let unanswered = concat!(
            r#"{"type":"item.started","item":{"type":"agent_message","text":"started"}}"#,
            "\n",
            r#"{"type":"item.completed","item":{"type":"command_execution","text":"ls"}}"#,
            "\n",
        );
        assert_eq!(
            final_message(unanswered.as_bytes()),
            unanswered,
            "no completed agent message: the words as they came"
        );
        // OpenCode's JSON lines: the last text part, not a tool's output.
        let opencode = concat!(
            r#"{"type":"step_start","part":{"type":"step-start"}}"#,
            "\n",
            r#"{"type":"text","part":{"type":"text","text":"first"}}"#,
            "\n",
            r#"{"type":"tool_use","part":{"type":"tool","text":"tool"}}"#,
            "\n",
            r#"{"type":"text","part":{"type":"text","text":"last"}}"#,
            "\n",
            r#"{"type":"text","text":"no part"}"#,
            "\n",
            r#"{"type":"step_finish","part":{"type":"step-finish","text":"finish"}}"#,
            "\n",
        );
        assert_eq!(final_message(opencode.as_bytes()), "last");
        assert_eq!(truncate("h\u{e9}llo", 3), "h\u{e9}");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(
            run_failure(&SkillRunError::TimedOut),
            "the harness ran out of time and was stopped"
        );
        assert_eq!(
            run_failure(&SkillRunError::OutputTooLarge),
            "the harness wrote more than 65536 bytes"
        );
        assert_eq!(
            run_failure(&SkillRunError::Spawn(String::from("detail"))),
            "the harness could not run"
        );
        let scratch = Scratch::new("question");
        assert_eq!(read_question(&scratch.0), None);
        scratch.write("question", b" \n");
        assert_eq!(read_question(&scratch.0), None, "a blank question is none");
        scratch.write("question", b"  Which module?\n");
        assert_eq!(read_question(&scratch.0).as_deref(), Some("Which module?"));
        scratch.write(
            "question",
            "q".repeat(MAX_QUESTION_BYTES.saturating_mul(2)).as_bytes(),
        );
        assert_eq!(
            read_question(&scratch.0).map(|question| question.len()),
            Some(MAX_QUESTION_BYTES)
        );
    }

    #[test]
    fn changes_are_computed_from_the_trees_never_from_what_the_harness_says() {
        let scratch = Scratch::new("changes");
        let (work, pristine) = (scratch.dir("work"), scratch.dir("pristine"));
        for (path, bytes) in [
            ("same", &b"s\n"[..]),
            ("edit", &b"a\nb\n"[..]),
            ("gone", &b"bye"[..]),
            ("deep/x", &b"x\n"[..]),
            ("old-binary", &[0xff][..]),
            ("binary-now-text", &[0xfe][..]),
            ("text-now-binary", &b"t\n"[..]),
        ] {
            scratch.write(&format!("pristine/{path}"), bytes);
        }
        for (path, bytes) in [
            ("same", &b"s\n"[..]),
            ("edit", &b"a\nc\n"[..]),
            ("deep/x", &b"x\n"[..]),
            ("deep/new", &b""[..]),
            ("new", &b"n\n"[..]),
            ("binary", &[0xff, 0x00][..]),
            ("binary-now-text", &b"text"[..]),
            ("text-now-binary", &[0xff][..]),
        ] {
            scratch.write(&format!("work/{path}"), bytes);
        }
        std::os::unix::fs::symlink("/etc/passwd", work.join("link")).unwrap();
        let found = changes(&work, &pristine).unwrap();
        let owned = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(a, b)| (String::from(*a), String::from(*b)))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            found,
            Changes {
                changed: vec![(
                    String::from("edit"),
                    String::from("a\nb\n"),
                    String::from("a\nc\n")
                )],
                added: owned(&[("deep/new", ""), ("new", "n\n")]),
                removed: owned(&[("gone", "bye")]),
                unpatchable: [
                    "binary",
                    "binary-now-text",
                    "text-now-binary",
                    "old-binary",
                    "link"
                ]
                .map(String::from)
                .to_vec(),
            }
        );
        assert!(
            changes(&work, &scratch.join("missing"))
                .unwrap_err()
                .starts_with("cannot read the work tree")
        );
    }

    #[test]
    fn a_work_tree_is_read_within_its_bounds() {
        let scratch = Scratch::new("tree-bounds");
        let files = scratch.dir("files");
        for n in 0..MAX_WORK_FILES {
            scratch.write(&format!("files/f{n}"), b"");
        }
        assert_eq!(
            read_tree(&files).unwrap().files.len(),
            MAX_WORK_FILES,
            "exactly the bound"
        );
        std::os::unix::fs::symlink("f0", files.join("one-more")).unwrap();
        assert!(
            read_tree(&files)
                .unwrap_err()
                .starts_with("the harness left more than 1024 files")
        );
        let bytes = scratch.dir("bytes");
        let bound = usize::try_from(MAX_WORK_BYTES).unwrap();
        scratch.write("bytes/half", &vec![b'a'; bound / 2]);
        scratch.write("bytes/rest", &vec![b'b'; bound - bound / 2]);
        let tree = read_tree(&bytes).unwrap();
        assert_eq!(
            tree.files.values().map(Vec::len).sum::<usize>(),
            bound,
            "exactly the bound"
        );
        assert!(tree.links.is_empty());
        scratch.write("bytes/sub/one", b"c");
        assert!(
            read_tree(&bytes).unwrap_err().contains("or 8388608 bytes"),
            "nested files count too"
        );
    }

    #[test]
    fn the_patch_is_exact_whole_file_hunks_git_can_apply() {
        assert_eq!(lines(""), (Vec::<&str>::new(), false));
        assert_eq!(lines("a"), (vec!["a"], true));
        assert_eq!(lines("a\n\nb\n"), (vec!["a", "", "b"], false));
        assert_eq!(
            lines("a\r\nb"),
            (vec!["a\r", "b"], true),
            "a CRLF file keeps its line ends"
        );
        assert_eq!(
            [range(""), range("a"), range("a\nb\n")],
            ["0,0", "1,1", "1,2"]
        );
        let mut side = String::new();
        hunk_side(&mut side, '+', "a\nb");
        assert_eq!(side, "+a\n+b\n\\ No newline at end of file\n");
        let owned = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(a, b)| (String::from(*a), String::from(*b)))
                .collect::<Vec<_>>()
        };
        let found = Changes {
            changed: vec![
                (
                    String::from("edit"),
                    String::from("a\nb\n"),
                    String::from("a\nc"),
                ),
                (String::from("emptied"), String::from("e\n"), String::new()),
            ],
            added: owned(&[("deep/new", "n\n"), ("empty", "")]),
            removed: owned(&[("gone", "bye\n"), ("was-empty", "")]),
            unpatchable: vec![String::from("binary")],
        };
        assert_eq!(
            patch(&found),
            "diff --git a/edit b/edit\n--- a/edit\n+++ b/edit\n@@ -1,2 +1,2 @@\n-a\n-b\n+a\n+c\n\\ No newline at end of file\n\
             diff --git a/emptied b/emptied\n--- a/emptied\n+++ b/emptied\n@@ -1,1 +0,0 @@\n-e\n\
             diff --git a/deep/new b/deep/new\nnew file mode 100644\n--- /dev/null\n+++ b/deep/new\n@@ -0,0 +1,1 @@\n+n\n\
             diff --git a/empty b/empty\nnew file mode 100644\n\
             diff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-bye\n\
             diff --git a/was-empty b/was-empty\ndeleted file mode 100644\n"
        );
        assert!(Changes::default().is_empty());
        let one = |field: usize| {
            let mut changes = Changes::default();
            match field {
                0 => changes
                    .changed
                    .push((String::from("a"), String::new(), String::new())),
                1 => changes.added.push((String::from("a"), String::new())),
                2 => changes.removed.push((String::from("a"), String::new())),
                _ => changes.unpatchable.push(String::from("a")),
            }
            changes
        };
        assert!(
            (0..4).all(|field| !one(field).is_empty()),
            "any change is a change"
        );
    }

    #[test]
    fn a_patch_is_delivered_as_a_new_owner_only_file_in_the_workspace() {
        let scratch = Scratch::new("deliver");
        let config = config(&scratch);
        let (_, vars) = settings(&scratch);
        let mut harnesses = parsed(&config, &vars).unwrap();
        let name = harnesses.deliver_patch("the patch").unwrap();
        assert!(name.starts_with("harness-"), "{name}");
        assert_eq!(
            Path::new(&name).extension().and_then(|ext| ext.to_str()),
            Some("patch")
        );
        let at = scratch.join("workspace").join(&name);
        assert_eq!(std::fs::read_to_string(&at).unwrap(), "the patch");
        assert_eq!(mode(&at), 0o600);
        assert_ne!(
            harnesses.deliver_patch("again").unwrap(),
            name,
            "never over an earlier patch"
        );
        harnesses.workspace = Some(scratch.join("missing"));
        assert_eq!(harnesses.deliver_patch("lost"), None);
        harnesses.workspace = None;
        assert_eq!(harnesses.deliver_patch("nowhere"), None);
        // With no activity log a run is still recorded in the broker's log.
        harnesses.record(&CLAUDE, &Report::new(&CLAUDE, &settings(&scratch).0, "[]"));
        assert!(!scratch.join("schedules.activity").exists());
    }

    fn sample_report() -> Report {
        let binary = PinnedBinary {
            path: PathBuf::from("/h"),
            sha256: "ab".repeat(32),
        };
        Report::new(&CODEX, &binary, "{\"brief\":1}")
    }

    #[test]
    fn a_name_or_a_reason_holding_a_secret_is_withheld() {
        let mut report = sample_report();
        report.reason = Some(String::from("failed on hidden"));
        report.record_changes(&Changes {
            changed: vec![(String::from("hidden.rs"), String::new(), String::new())],
            added: vec![(String::from("hidden-too"), String::new())],
            removed: vec![(String::from("hidden.txt"), String::new())],
            unpatchable: vec![String::from("hidden.bin"), String::from("fine.bin")],
        });
        let screened = report.screened(&Holding("hidden")).unwrap();
        let told = serde_json::to_string(&screened).unwrap();
        assert!(!told.contains("hidden"), "{told}");
        assert_eq!(screened.unpatchable.get(1).unwrap(), "fine.bin");
    }

    /// A report whose text can't be checked isn't one to deliver, whichever
    /// text it was.
    #[test]
    fn a_report_that_cannot_be_checked_is_not_delivered() {
        let mut report = sample_report();
        report.harness_said.message = String::from("said");
        report.harness_said.question = Some(String::from("asked"));
        report.reason = Some(String::from("why"));
        report.files_changed = vec![String::from("changed")];
        report.files_added = vec![String::from("added")];
        report.files_removed = vec![String::from("removed")];
        report.unpatchable = vec![String::from("beyond")];
        assert!(report.clone().screened(&Holding("absent")).is_ok());
        for text in [
            "said", "asked", "why", "changed", "added", "removed", "beyond",
        ] {
            assert!(
                report.clone().screened(&Unanswered(text)).is_err(),
                "{text} went unchecked"
            );
        }
    }

    /// A check that can't be answered for a text.
    struct Unanswered(&'static str);

    impl HarnessVault for Unanswered {
        fn key(&self, _secret: &str) -> Option<Zeroizing<String>> {
            None
        }

        fn contains(&self, text: &[u8]) -> Result<bool, String> {
            if text == self.0.as_bytes() {
                Err(String::from("the keeper went"))
            } else {
                Ok(false)
            }
        }
    }

    /// A vault holding one secret, found anywhere in a text.
    struct Holding(&'static str);

    impl HarnessVault for Holding {
        fn key(&self, _secret: &str) -> Option<Zeroizing<String>> {
            None
        }

        fn contains(&self, text: &[u8]) -> Result<bool, String> {
            Ok(text
                .windows(self.0.len())
                .any(|window| window == self.0.as_bytes()))
        }
    }

    /// Each thing a brief tells a harness is checked, as its own text:
    /// parsed, so what JSON escapes isn't hidden, and a whole text can be
    /// a short secret.
    #[test]
    fn a_brief_is_checked_as_the_text_it_sends() {
        let brief = |json: &str| Brief::parse(json).unwrap();
        let plain = brief(r#"{"task":"Go.","root":"p","paths":["a"]}"#);
        assert_eq!(plain.free_of_secrets(&Holding("absent")), Ok(()));
        let escaped = brief(r#"{"task":"use s3cr\"et\\v","root":"p","paths":["a"]}"#);
        assert!(matches!(
            escaped.free_of_secrets(&Holding("s3cr\"et\\v")),
            Err(Stopped::Holds(_))
        ));
        let answered = brief(r#"{"task":"Go.","root":"p","paths":["a"],"answer":"hidden"}"#);
        assert!(matches!(
            answered.free_of_secrets(&Holding("hidden")),
            Err(Stopped::Holds(_))
        ));
        let named = brief(r#"{"task":"Go.","root":"p","paths":["a","hidden"]}"#);
        assert!(matches!(
            named.free_of_secrets(&Holding("hidden")),
            Err(Stopped::Holds(_))
        ));
        assert_eq!(
            plain.free_of_secrets(&Unanswered("Go.")),
            Err(Stopped::Unchecked)
        );
    }

    #[test]
    fn a_tree_with_a_secret_names_the_file_and_not_the_secret() {
        let scratch = Scratch::new("free-of-secrets");
        scratch.write("tree/clean.txt", b"nothing here");
        scratch.write("tree/sub/config.txt", b"token=hidden");
        let root = scratch.join("tree");
        let Err(Stopped::Holds(refused)) = free_of_secrets(&root, &Holding("hidden")) else {
            unreachable!("the file's secret wasn't found");
        };
        assert!(refused.contains("sub/config.txt"), "{refused}");
        assert!(!refused.contains("hidden"), "{refused}");
        assert_eq!(free_of_secrets(&root, &Holding("absent")), Ok(()));
        assert_eq!(
            free_of_secrets(&root, &Unanswered("nothing here")),
            Err(Stopped::Unchecked)
        );
    }

    #[test]
    fn a_report_starts_refused_and_says_only_what_hatchling_found() {
        let report = sample_report();
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            json!({
                "harness": "codex-cli",
                "binary_sha256": "ab".repeat(32),
                "brief_sha256": CodeHash::of(b"{\"brief\":1}").to_string(),
                "outcome": "refused",
                "files_changed": [], "files_added": [], "files_removed": [], "unpatchable": [],
                "harness_said": { "message": "" },
            })
        );
        let refused = report.clone().refused(String::from("why"));
        assert_eq!(
            (refused.outcome, refused.reason.as_deref()),
            (HarnessOutcome::Refused, Some("why"))
        );
        let failed = report.clone().failed("an internal detail");
        assert_eq!(
            (failed.outcome, failed.reason.as_deref()),
            (HarnessOutcome::Failed, Some("the harness could not run")),
            "internal detail stays in the broker's log"
        );
        let mut changed = report;
        changed.record_changes(&Changes {
            changed: vec![(String::from("a"), String::new(), String::new())],
            added: vec![
                (String::from("b"), String::new()),
                (String::from("c"), String::new()),
            ],
            removed: vec![(String::from("d"), String::new())],
            unpatchable: vec![String::from("e")],
        });
        assert_eq!(
            [
                &changed.files_changed,
                &changed.files_added,
                &changed.files_removed,
                &changed.unpatchable
            ],
            [
                &vec![String::from("a")],
                &vec![String::from("b"), String::from("c")],
                &vec![String::from("d")],
                &vec![String::from("e")]
            ]
        );
        assert_eq!(changed.change_count(), 5);
        changed.omitted = 2;
        assert_eq!(changed.change_count(), 7, "what was left out still counts");
        assert_eq!(
            field(&serde_json::to_value(&changed).unwrap(), "/omitted"),
            2
        );
        changed.omitted = u32::MAX;
        assert_eq!(changed.change_count(), u32::MAX);
        assert!(is_zero(&0) && !is_zero(&1));
    }

    #[test]
    fn a_report_is_fitted_to_the_skill_output_cap_without_losing_its_counts() {
        let fits = |report: &Report| {
            serde_json::to_string(report).unwrap().len() <= hatchling_core::MAX_SKILL_OUTPUT_BYTES
        };
        let mut small = sample_report();
        small.harness_said.message = String::from("Done.");
        assert_eq!(small.fitted(), small, "a report that fits is untouched");
        let mut chatty = sample_report();
        chatty.harness_said.message = "m".repeat(hatchling_core::MAX_SKILL_OUTPUT_BYTES);
        chatty.files_changed = vec![String::from("a")];
        let fitted = chatty.fitted();
        assert_eq!(
            (
                fitted.harness_said.message.as_str(),
                fitted.files_changed.len(),
                fitted.omitted
            ),
            ("", 1, 0)
        );
        let mut busy = sample_report();
        busy.harness_said.message = String::from("Done.");
        busy.files_changed = (0..400).map(|n| format!("src/changed-{n:04}.rs")).collect();
        busy.unpatchable = (0..100)
            .map(|n| format!("assets/binary-{n:04}.png"))
            .collect();
        let fitted = busy.fitted();
        assert!(fits(&fitted) && !fits(&busy));
        assert_eq!(fitted.change_count(), 500, "the count survives the cut");
        assert!(
            fitted.unpatchable.is_empty(),
            "unpatchable entries go first"
        );
        assert!(fitted.omitted > 100 && fitted.harness_said.message.is_empty());
        let mut asking = sample_report();
        asking.harness_said.question = Some("q".repeat(hatchling_core::MAX_SKILL_OUTPUT_BYTES));
        let fitted = asking.fitted();
        assert_eq!((fitted.harness_said.question, fitted.omitted), (None, 0));
    }
}
