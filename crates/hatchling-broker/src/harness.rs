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
//!   its own loopback that the broker serves
//!   ([`crate::harness_relay`]). Its login is a surrogate key: the vendor
//!   key stays in the vault (secret [`HarnessKind::secret_name`]), read by
//!   the broker for its relay, never given to the cell. A harness without
//!   an API runs offline, as before.

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
use hatchling_vault::{MasterKey, SealedVault};
use zeroize::Zeroizing;

use hatchling_core::subagent::{ActivityEvent, HarnessOutcome, SubagentEvent};
use hatchling_core::ui::operator::CellKind;
use hatchling_core::{CapabilityName, SkillOp, SkillResult};
use hatchling_scheduler::Timestamp;
use hatchling_skill::CodeHash;
use serde::{Deserialize, Serialize};

use crate::cell::{HarnessDirs, HarnessNetwork};
use crate::environment as env;
use crate::harness_relay::{RELAY_PORT, Relay, Vendor, Wire};
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

/// Reads a harness's vendor key for its relay.
pub type HarnessKeys<'a> = &'a dyn Fn(HarnessKind) -> Option<Zeroizing<String>>;

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

    /// The vault secret holding its vendor key: the broker reads
    /// it for the harness's relay, and no vault grant may name it.
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

/// A harness binary and the SHA-256 it is pinned to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedBinary {
    path: PathBuf,
    sha256: String,
}

impl PinnedBinary {
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
    binaries: BTreeMap<HarnessKind, PinnedBinary>,
    vendors: BTreeMap<HarnessKind, Vendor>,
    tools: Vec<(PathBuf, String)>,
    codex_host: Option<PinnedBinary>,
    granted: BTreeSet<HarnessKind>,
    roots: BTreeMap<String, PathBuf>,
    timeout: Duration,
    workspace: Option<PathBuf>,
    activity_log: Option<PathBuf>,
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
    /// its pin, a bad root, or a harness with no workspace or no roots.
    pub fn parse(config: &Config, get: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let mut binaries = BTreeMap::new();
        for kind in HarnessKind::ALL {
            if let Some(binary) = parse_binary(
                kind,
                get(kind.path_key()).as_deref(),
                get(kind.hash_key()).as_deref(),
            )? {
                binaries.insert(kind, binary);
            }
        }
        let tls = TlsClient::bundled()
            .map_err(|source| invalid("HATCHLING_EGRESS_CA_BUNDLE", source.to_string()))?;
        let mut vendors = BTreeMap::new();
        for kind in HarnessKind::ALL {
            if let Some(vendor) = parse_vendor(kind, get(kind.api_key()).as_deref(), &tls)? {
                if !binaries.contains_key(&kind) {
                    return Err(invalid(
                        kind.api_key(),
                        String::from("a vendor API needs its harness binary"),
                    ));
                }
                vendors.insert(kind, vendor);
            }
        }
        let codex_host = parse_pin(
            (CODEX_HOST_KEY, CODEX_HOST_HASH_KEY),
            get(CODEX_HOST_KEY).as_deref(),
            get(CODEX_HOST_HASH_KEY).as_deref(),
        )?;
        if codex_host.is_some() && !binaries.contains_key(&HarnessKind::CodexCli) {
            return Err(invalid(
                CODEX_HOST_KEY,
                String::from("the code-mode host needs the Codex CLI"),
            ));
        }
        let tools = parse_tools(get(TOOLS_KEY).as_deref(), !vendors.is_empty())?;
        let roots = parse_roots(get(env::HATCHLING_HARNESS_ROOTS.name).as_deref(), config)?;
        let timeout = parse_timeout(get(env::HATCHLING_HARNESS_TIMEOUT_SECS.name).as_deref())?;
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
                .copied()
                .filter(|kind| {
                    config.skills.grants.get(kind.name()).is_some_and(|caps| {
                        capability.as_ref().is_some_and(|cap| caps.contains(cap))
                    })
                })
                .collect();
        Ok(Self {
            binaries,
            vendors,
            tools,
            codex_host,
            granted,
            roots,
            timeout,
            workspace: config.artifacts.workspace.clone(),
            activity_log: config
                .scheduler
                .enabled()
                .then(|| config.scheduler.activity_log()),
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

    /// Whether no harness is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
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
        let kind = HarnessKind::from_skill_id(skill)?;
        let Some(binary) = self.binaries.get(&kind) else {
            tracing::warn!(skill = %skill, "denied skill op: the harness is not configured");
            return Some(SkillResult::UnknownSkill);
        };
        if capability.as_str() != HARNESS_CAPABILITY || !self.granted.contains(&kind) {
            tracing::warn!(skill = %skill, capability = %capability, "denied skill op: the harness grant does not cover it");
            return Some(SkillResult::CapabilityDenied);
        }
        let Some(runtime) = runtime else {
            tracing::error!(skill = %skill, "harness run impossible: the broker has no skill runtime");
            return Some(SkillResult::SkillError);
        };
        let report = self.run(kind, binary, input, runtime, keys);
        self.record(kind, &report);
        match serde_json::to_string(&report.fitted()) {
            Ok(output) => Some(SkillResult::Output { output }),
            Err(error) => {
                tracing::error!(%error, "cannot encode a harness report");
                Some(SkillResult::SkillError)
            }
        }
    }

    /// One run: the brief, the slice, the cell, the changes.
    fn run(
        &self,
        kind: HarnessKind,
        binary: &PinnedBinary,
        input: &str,
        runtime: &SkillRuntime,
        keys: HarnessKeys<'_>,
    ) -> Report {
        let mut report = Report::new(kind, binary, input);
        let brief = match Brief::parse(input) {
            Ok(brief) => brief,
            Err(reason) => return report.refused(reason),
        };
        let Some(root) = self.roots.get(&brief.root) else {
            return report.refused(format!("{:?} is not a configured project root", brief.root));
        };
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
        let verified = match VerifiedCode::write("harness", "harness", &code) {
            Ok(verified) => verified,
            Err(error) => return report.failed(&error.to_string()),
        };
        let host = match self.host_copy(kind) {
            Ok(host) => host,
            Err(reason) => return report.refused_for(HOST_REFUSED, &reason),
        };
        let (network, relaying) = match self.vendors.get(&kind) {
            None => (None, None),
            Some(vendor) => match self.network(kind, vendor, keys, &scratch, host.as_ref()) {
                Ok((network, relaying)) => (Some(network), Some(relaying)),
                Err(reason) => return report.refused(reason),
            },
        };
        let cell = match harness_cell(kind, &brief.prompt(), verified.dir(), &scratch, network) {
            Ok(cell) => cell,
            Err(error) => return report.failed(&error.to_string()),
        };
        if let Err(reason) = hand_over(&scratch, cell.uid_base) {
            return report.failed(&reason);
        }
        let relay = relaying.map(|(handoff, vendor, surrogate, key)| {
            let destination = vendor.destination.clone();
            let deadline = Instant::now() + self.timeout + RELAY_GRACE;
            (
                destination,
                Relay::start(handoff, vendor, surrogate, key, deadline),
            )
        });
        let ran = run_cell(
            &runtime.clone().with_timeout(self.timeout),
            &cell,
            CellKind::Harness,
            &[],
            MAX_HARNESS_OUTPUT_BYTES,
        );
        drop(verified);
        drop(host);
        report.finish_relay(kind, relay);
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
                    report.patch = self.deliver_patch(&patch(&found));
                }
            }
            Err(reason) => {
                report.outcome = HarnessOutcome::Failed;
                report.reason = Some(reason);
            }
        }
        report
    }

    /// The Codex CLI's code-mode host, copied from its verified bytes, for
    /// a Codex run with one pinned; `None` otherwise.
    fn host_copy(&self, kind: HarnessKind) -> Result<Option<VerifiedCode>, String> {
        let Some(host) = self
            .codex_host
            .as_ref()
            .filter(|_| kind == HarnessKind::CodexCli)
        else {
            return Ok(None);
        };
        let bytes = host.verified()?;
        VerifiedCode::write("harness-host", CODEX_HOST, &bytes)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    /// A real CLI's network for one run: its vendor key, a fresh
    /// surrogate, the handoff socket in the run's own directory, its
    /// tools, and its environment. The relay's parts come back with it.
    fn network(
        &self,
        kind: HarnessKind,
        vendor: &Vendor,
        keys: HarnessKeys<'_>,
        dirs: &RunDirs,
        host: Option<&VerifiedCode>,
    ) -> Result<(HarnessNetwork, Relaying), String> {
        let key = keys(kind).ok_or_else(|| {
            format!(
                "the vendor key is not in the vault: store it as the secret {}",
                kind.secret_name()
            )
        })?;
        // Set-up failures are the broker's, and their detail stays in its
        // log: the report says only that the network couldn't be made.
        let unavailable = |what: &str, error: std::io::Error| {
            tracing::warn!(%error, what, "harness network setup failed");
            String::from("the harness network could not be set up")
        };
        let surrogate = crate::approval::random_hex(kind.surrogate_prefix())
            .map(Zeroizing::new)
            .map_err(|error| unavailable("surrogate key", error))?;
        let handoff_path = dirs.base.join("relay.sock");
        let handoff = UnixListener::bind(&handoff_path)
            .map_err(|error| unavailable("relay handoff", error))?;
        let mut tools = self.tools.clone();
        if let Some(host) = host {
            tools.push((host.dir().join(CODEX_HOST), String::from(CODEX_HOST)));
        }
        let network = HarnessNetwork {
            relay: RELAY_PORT,
            handoff: handoff_path,
            tools,
            env: kind.env(RELAY_PORT, &surrogate),
        };
        Ok((network, (handoff, vendor.clone(), surrogate, key)))
    }

    /// Writes `patch` to the artifact workspace, as a new file; its name,
    /// or `None` when it could not be written (logged).
    fn deliver_patch(&self, patch: &str) -> Option<String> {
        let workspace = self.workspace.as_ref()?;
        let name = format!("{}.patch", crate::approval::random_hex("harness-").ok()?);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(workspace.join(&name))
            .and_then(|mut file| file.write_all(patch.as_bytes()));
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
    fn record(&self, kind: HarnessKind, report: &Report) {
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
                harness: String::from(kind.name()),
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

/// What a run's relay starts from: the handoff socket, the vendor, the
/// surrogate the cell holds, and the vendor key.
type Relaying = (UnixListener, Vendor, Zeroizing<String>, Zeroizing<String>);

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
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
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
    if raw.contains(',') || raw.starts_with("public:") || raw.starts_with("local-keyless:") {
        return Err(invalid(
            kind.api_key(),
            String::from("one vendor API: host:port, internal:host:port, or local:localhost:PORT"),
        ));
    }
    let allowlist =
        Allowlist::parse(raw).map_err(|source| invalid(kind.api_key(), source.to_string()))?;
    Ok(Some(Vendor {
        wire: kind.wire(),
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
    }))
}

/// A CLI cell's tools, each with the name the cell sees it at:
/// those `raw` names, each an absolute path to a program, or else the
/// defaults this host has. None for harnesses without a network.
fn parse_tools(raw: Option<&str>, networked: bool) -> Result<Vec<(PathBuf, String)>, Error> {
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
    let mut tools = Vec::new();
    for path in paths {
        let program = PathBuf::from(path);
        let plain = program.is_absolute()
            && program
                .components()
                .skip(1)
                .all(|component| matches!(component, Component::Normal(_)));
        if plain && std::fs::metadata(&program).is_ok_and(|meta| meta.is_file()) {
            tools.push((program, path.trim_start_matches('/').to_owned()));
        } else if explicit.is_some() {
            return Err(invalid(
                TOOLS_KEY,
                format!("{path} is not an absolute path to a program"),
            ));
        }
    }
    Ok(tools)
}

/// The vendor keys the broker reads for its harness relays.
///
/// Each is the vault secret [`HarnessKind::secret_name`], read with the
/// broker's own key and logged as the broker's read. No vault grant may
/// name it.
pub fn vault_keys<'a>(
    vault_root: &'a Path,
    key: &'a MasterKey,
) -> impl Fn(HarnessKind) -> Option<Zeroizing<String>> + 'a {
    move |kind| {
        let name = kind.secret_name();
        let fetched = name
            .parse::<hatchling_core::SecretName>()
            .ok()
            .and_then(|secret| {
                let vault = SealedVault::open(vault_root, key).ok()?;
                Some(vault.fetch(&secret))
            });
        let outcome = match &fetched {
            Some(Ok(_)) => crate::vault_access::Outcome::Allowed,
            Some(Err(hatchling_vault::Error::NotFound { .. })) => {
                crate::vault_access::Outcome::NotFound
            }
            Some(Err(_)) | None => crate::vault_access::Outcome::Failed,
        };
        crate::vault_access::record_broker_read(
            vault_root,
            "harness",
            vec![name.to_owned()],
            outcome,
        );
        let secret = fetched?.ok()?;
        std::str::from_utf8(secret.as_bytes())
            .ok()
            .map(|text| Zeroizing::new(text.trim().to_owned()))
            .filter(|text| !text.is_empty())
    }
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
        std::fs::DirBuilder::new()
            .mode(0o700)
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
        let bytes = read_regular(&at).map_err(|_| format!("{path:?} is not a regular file"))?;
        if std::str::from_utf8(&bytes).is_err() {
            return Err(format!("{path:?} is not text"));
        }
        total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        if total > MAX_SLICE_BYTES {
            return Err(format!("the files hold more than {MAX_SLICE_BYTES} bytes"));
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
    kind: HarnessKind,
    prompt: &str,
    code_dir: &Path,
    dirs: &RunDirs,
    network: Option<HarnessNetwork>,
) -> Result<CellConfig, Error> {
    let mut cell = CellConfig::skill("harness")?;
    cell.skill_code = Some(code_dir.to_path_buf());
    cell.agent_bin = code_dir.join("harness");
    cell.harness = Some(HarnessDirs {
        work: dirs.work.clone(),
        scratch: dirs.scratch.clone(),
        args: kind.args(prompt, RELAY_PORT),
        network,
    });
    Ok(cell)
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
/// CLI's), or its whole output.
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
    result
        .or_else(last_message)
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

/// The harness's question, if it left one.
fn read_question(scratch: &Path) -> Option<String> {
    let bytes = read_regular(&scratch.join(QUESTION_FILE)).ok()?;
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
                let bytes =
                    std::fs::read(root.join(&name)).map_err(|err| unreadable(&shown, err))?;
                total = total.saturating_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
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
    harness: &'static str,
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
    /// A report for `kind` running `binary` on the brief `input`, before
    /// it ran.
    fn new(kind: HarnessKind, binary: &PinnedBinary, input: &str) -> Self {
        Self {
            harness: kind.name(),
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

    fn failed(mut self, reason: &str) -> Self {
        tracing::error!(%reason, "harness run failed before the harness started");
        self.outcome = HarnessOutcome::Failed;
        self.reason = Some(String::from("the harness could not run"));
        self
    }

    /// The relay's account of the run, once its cell has ended:
    /// where the requests went and how many, for the activity event.
    fn finish_relay(&mut self, kind: HarnessKind, relay: Option<(String, Relay)>) {
        let Some((destination, relay)) = relay else {
            return;
        };
        let (count, problem) = relay.finish();
        if let Some(problem) = problem {
            tracing::warn!(%problem, harness = kind.name(), "harness relay");
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
        SpaceConfig, VaultConfig,
    };

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

    #[test]
    fn a_relayed_run_reports_where_its_requests_went() {
        let mut report = sample_report();
        report.finish_relay(HarnessKind::ClaudeCode, None);
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
        report.finish_relay(HarnessKind::ClaudeCode, Some((destination.clone(), relay)));
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
            vec![(tool, shown.trim_start_matches('/').to_owned())]
        );
        for bad in [
            String::from("bin/tool"),
            format!("{}/../tool", scratch.join("bin").display()),
            scratch.join("bin").display().to_string(),
            scratch.join("missing").display().to_string(),
        ] {
            assert_eq!(
                refusal(parse_tools(Some(&bad), true)),
                (
                    TOOLS_KEY,
                    format!("{bad} is not an absolute path to a program")
                )
            );
        }
        // The defaults: each one that is a program on this host, named by
        // its own path.
        let expected: Vec<(PathBuf, String)> = DEFAULT_TOOLS
            .iter()
            .filter(|path| Path::new(path).is_file())
            .map(|path| (PathBuf::from(path), path.trim_start_matches('/').to_owned()))
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(parse_tools(None, true).unwrap(), expected);
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
            networked.vendors.keys().copied().collect::<Vec<_>>(),
            [HarnessKind::ClaudeCode]
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
            BTreeMap::from([(HarnessKind::ClaudeCode, binary)])
        );
        assert_eq!(
            harnesses
                .vendors
                .get(&HarnessKind::ClaudeCode)
                .map(|vendor| vendor.destination.as_str()),
            Some("localhost:8443")
        );
        assert_eq!(
            harnesses.roots,
            BTreeMap::from([(String::from("project"), scratch.join("project"))])
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
        assert!(unhosted.host_copy(HarnessKind::CodexCli).unwrap().is_none());
        let host = scratch.write("bin/host", b"host bytes");
        vars.insert(CODEX_HOST_KEY, host.display().to_string());
        vars.insert(CODEX_HOST_HASH_KEY, CodeHash::of(b"host bytes").to_string());
        let hosted = parsed(&config, &vars).unwrap();
        assert!(hosted.host_copy(HarnessKind::ClaudeCode).unwrap().is_none());
        let copy = hosted.host_copy(HarnessKind::CodexCli).unwrap().unwrap();
        assert_eq!(
            std::fs::read(copy.dir().join(CODEX_HOST)).unwrap(),
            b"host bytes"
        );
        std::fs::write(&host, b"changed").unwrap();
        let refused = hosted.host_copy(HarnessKind::CodexCli).unwrap_err();
        assert!(refused.contains("never substituted"), "{refused}");
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
        let keys = vault_keys(&root, &key);
        assert_eq!(
            keys(HarnessKind::ClaudeCode).as_deref().map(String::as_str),
            Some("sk-ant-real")
        );
        assert!(keys(HarnessKind::CodexCli).is_none(), "not stored");
        vault
            .store(
                &"harness-codex-cli".parse().unwrap(),
                &hatchling_vault::Secret::new(b" \n".to_vec()).unwrap(),
            )
            .unwrap();
        assert!(keys(HarnessKind::CodexCli).is_none(), "a blank key is none");
        let other = MasterKey::generate().unwrap();
        assert!(
            vault_keys(&root, &other)(HarnessKind::ClaudeCode).is_none(),
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
            BTreeMap::from([(HarnessKind::ClaudeCode, binary)])
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
            BTreeSet::from([HarnessKind::ClaudeCode]),
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
        let report = Report::new(HarnessKind::ClaudeCode, &binary, "{}").refused_for(
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
        let report = harnesses.run(HarnessKind::ClaudeCode, &binary, input, &runtime, &no_keys);
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
        for dir in [&dirs.base, &dirs.work, &dirs.pristine, &dirs.scratch] {
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

    /// A key reader with no key: an offline harness never asks it.
    const fn no_keys(_: HarnessKind) -> Option<Zeroizing<String>> {
        None
    }

    #[test]
    fn a_harness_cell_runs_the_verified_copy_on_the_run_trees() {
        let dirs = RunDirs::create().unwrap();
        let cell = harness_cell(
            HarnessKind::CodexCli,
            "The prompt.",
            Path::new("/staged/code"),
            &dirs,
            None,
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
        harnesses.record(
            HarnessKind::ClaudeCode,
            &Report::new(HarnessKind::ClaudeCode, &settings(&scratch).0, "[]"),
        );
        assert!(!scratch.join("schedules.activity").exists());
    }

    fn sample_report() -> Report {
        let binary = PinnedBinary {
            path: PathBuf::from("/h"),
            sha256: "ab".repeat(32),
        };
        Report::new(HarnessKind::CodexCli, &binary, "{\"brief\":1}")
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
