//! external harness skills, end to end in real harness cells.
//!
//! The harness is the stub `hatchling-harness-fixture`, configured and
//! pinned exactly as a deployment configures Claude Code or the Codex CLI,
//! and run by the real skill runtime (the broker binary as the spawner
//! helper) in a real harness cell. Its prompt picks what it does.
//!
//! Cells need user namespaces: without them this skips, and
//! `HATCHLING_REQUIRE_CELL=1` makes the skip a failure.

#[allow(
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use common::{TestDir, broker_config, guard_cell_available, harness_fixture_binary, test_dir};
use hatchling_broker::{Config, Harnesses, SkillRuntime, Skills, handle_skill_op};
use hatchling_core::subagent::{ActivityEvent, HarnessOutcome, SubagentEvent};
use hatchling_core::{CellId, SkillOp, SkillResult};
use hatchling_skill::CodeHash;
use serde_json::{Value, json};

/// A key reader with no key: an offline harness never asks it.
const fn no_keys(_: hatchling_broker::harness::HarnessKind) -> Option<zeroize::Zeroizing<String>> {
    None
}

/// A deployment: a project root, the artifact workspace, an activity log,
/// the harness binary (a copy the test may replace), and its pin.
struct Deployment {
    dir: TestDir,
    config: Config,
    binary: PathBuf,
    sha256: String,
}

impl Deployment {
    fn new(name: &str) -> Result<Self, String> {
        let dir = test_dir(name);
        let _ = std::fs::remove_dir_all(&*dir);
        for sub in ["project/src", "workspace", "sock", "bin"] {
            std::fs::create_dir_all(dir.join(sub)).map_err(|e| e.to_string())?;
        }
        std::fs::write(dir.join("project/src/lib.rs"), "pub fn a() {}\n")
            .map_err(|e| e.to_string())?;
        std::fs::write(dir.join("project/notes.txt"), "old notes").map_err(|e| e.to_string())?;
        let binary = dir.join("bin/harness");
        std::fs::copy(harness_fixture_binary()?, &binary).map_err(|e| e.to_string())?;
        let sha256 = CodeHash::of(&std::fs::read(&binary).map_err(|e| e.to_string())?).to_string();
        let uid = rustix::process::getuid().as_raw();
        let mut config = broker_config(&dir.join("sock/broker.sock"), uid.wrapping_add(1))?;
        // The vault lives in this deployment's directory, so it goes with it.
        config.vault.root = dir.join("vault");
        config.vault.key_file = hatchling_broker::VaultConfig::default_key_file(&config.vault.root);
        config.artifacts.workspace = Some(dir.join("workspace"));
        config.scheduler.root = dir.join("schedules");
        let code: HashSet<_> = [hatchling_broker::harness::HARNESS_CAPABILITY
            .parse()
            .map_err(|_| "cap")?]
        .into();
        config.skills.grants = HashMap::from([
            (String::from("claude-code"), code.clone()),
            (String::from("codex-cli"), code),
        ]);
        Ok(Self {
            dir,
            config,
            binary,
            sha256,
        })
    }

    fn vars(&self, timeout: &str) -> HashMap<&'static str, String> {
        let path = self.binary.display().to_string();
        HashMap::from([
            ("HATCHLING_HARNESS_CLAUDE_CODE", path.clone()),
            ("HATCHLING_HARNESS_CLAUDE_CODE_SHA256", self.sha256.clone()),
            ("HATCHLING_HARNESS_CODEX_CLI", path),
            ("HATCHLING_HARNESS_CODEX_CLI_SHA256", self.sha256.clone()),
            (
                "HATCHLING_HARNESS_ROOTS",
                format!("project={}", self.dir.join("project").display()),
            ),
            ("HATCHLING_HARNESS_TIMEOUT_SECS", timeout.to_owned()),
        ])
    }

    fn skills(&self, timeout: &str) -> Result<Skills, String> {
        let vars = self.vars(timeout);
        let harnesses = Harnesses::parse(&self.config, |key| vars.get(key).cloned())
            .map_err(|e| e.to_string())?;
        let runtime = SkillRuntime::new(common::spawner());
        Ok(Skills::default()
            .with_runtime(runtime)
            .with_harnesses(harnesses))
    }

    fn file(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(relative)).ok()
    }

    fn activity(&self) -> Vec<ActivityEvent> {
        self.file("schedules.activity")
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}

/// Invokes `skill` with `brief`; the report, or why there is none.
fn invoke(skills: &Skills, skill: &str, brief: &Value) -> Result<Value, String> {
    let op = SkillOp::Invoke {
        skill: skill.to_owned(),
        capability: "code".parse().map_err(|_| "capability")?,
        input: brief.to_string(),
    };
    let cell: CellId = "test".parse().map_err(|_| "cell id")?;
    match handle_skill_op(skills, &op, &cell, &no_keys) {
        SkillResult::Output { output } => serde_json::from_str(&output).map_err(|e| e.to_string()),
        refused @ (SkillResult::UnknownSkill
        | SkillResult::CapabilityDenied
        | SkillResult::SkillError) => Err(format!("{refused:?}")),
    }
}

/// The report's field at `pointer` (`Null` when absent).
fn field(report: &Value, pointer: &str) -> Value {
    report.pointer(pointer).cloned().unwrap_or(Value::Null)
}

fn brief(task: &str, paths: &[&str]) -> Value {
    json!({ "task": task, "root": "project", "paths": paths })
}

fn said(report: &Value) -> &str {
    report
        .pointer("/harness_said/message")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

#[test]
fn claude_code_edits_its_slice_and_returns_a_patch_for_review() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let deployment = Deployment::new("harness-claude")?;
    let skills = deployment.skills("20")?;
    let task =
        "Add a helper.\nSTUB edit src/lib.rs\nSTUB create src/helper.rs\nSTUB remove notes.txt";
    let input = brief(task, &["src/lib.rs", "notes.txt"]);
    let report = invoke(&skills, "claude-code@1", &input)?;
    assert_eq!(field(&report, "/harness"), "claude-code");
    assert_eq!(field(&report, "/outcome"), "done", "{report}");
    assert_eq!(field(&report, "/binary_sha256"), deployment.sha256.as_str());
    assert_eq!(
        field(&report, "/brief_sha256"),
        CodeHash::of(input.to_string().as_bytes()).to_string()
    );
    assert_eq!(field(&report, "/files_changed"), json!(["src/lib.rs"]));
    assert_eq!(field(&report, "/files_added"), json!(["src/helper.rs"]));
    assert_eq!(field(&report, "/files_removed"), json!(["notes.txt"]));
    assert!(
        said(&report).contains("prompt: You are working on a task for Hatchling."),
        "{report}"
    );
    assert!(said(&report).contains("Task:\nAdd a helper."));
    let patch = deployment
        .file(&format!(
            "workspace/{}",
            field(&report, "/patch").as_str().unwrap()
        ))
        .unwrap();
    assert_eq!(
        patch,
        "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,2 @@\n-pub fn a() {}\n+pub fn a() {}\n+// edited by the stub harness\n\
         diff --git a/src/helper.rs b/src/helper.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/helper.rs\n@@ -0,0 +1,1 @@\n+fn created() {}\n\
         diff --git a/notes.txt b/notes.txt\ndeleted file mode 100644\n--- a/notes.txt\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-old notes\n\\ No newline at end of file\n"
    );
    // Nothing reached the project itself.
    assert_eq!(
        deployment.file("project/src/lib.rs").as_deref(),
        Some("pub fn a() {}\n")
    );
    assert_eq!(
        deployment.file("project/notes.txt").as_deref(),
        Some("old notes")
    );
    assert!(deployment.file("project/src/helper.rs").is_none());
    Ok(())
}

#[test]
fn codex_cli_runs_the_same_way_with_its_own_arguments() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let deployment = Deployment::new("harness-codex")?;
    let skills = deployment.skills("20")?;
    let report = invoke(
        &skills,
        "codex-cli@1",
        &brief("Tidy up.\nSTUB edit src/lib.rs", &["src/lib.rs"]),
    )?;
    assert_eq!(
        (
            field(&report, "/harness").as_str(),
            field(&report, "/outcome").as_str()
        ),
        (Some("codex-cli"), Some("done"))
    );
    assert_eq!(field(&report, "/files_changed"), json!(["src/lib.rs"]));
    // The Codex form: plain lines, no JSON envelope of its own.
    assert!(
        said(&report).starts_with("edit src/lib.rs: true\nprompt: You are working"),
        "{report}"
    );
    Ok(())
}

#[test]
fn the_harness_sees_no_vault_no_broker_and_no_host_variable() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let deployment = Deployment::new("harness-scope")?;
    let skills = deployment.skills("20")?;
    // A live broker socket on the host: reachable from here, not from the cell.
    let socket = deployment.dir.join("sock/broker.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).map_err(|e| e.to_string())?;
    let vault = &deployment.config.vault.root;
    std::fs::create_dir_all(vault).map_err(|e| e.to_string())?;
    let project = deployment.dir.join("project");
    let task = format!(
        "Look around.\nSTUB env\nSTUB probe {}\nSTUB probe {}\nSTUB probe {}\nSTUB socket {}",
        vault.display(),
        project.display(),
        socket.display(),
        socket.display()
    );
    let report = invoke(&skills, "claude-code@1", &brief(&task, &["src/lib.rs"]))?;
    let lines: Vec<&str> = said(&report).lines().collect();
    let env: Vec<&str> = lines
        .iter()
        .filter(|line| !line.contains(": ") && line.contains('='))
        .copied()
        .collect();
    let mut keys: Vec<&str> = env
        .iter()
        .filter_map(|line| line.split_once('=').map(|(key, _)| key))
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["HATCHLING_CELL_ID", "HATCHLING_LOG_LEVEL", "HOME", "TMPDIR"],
        "nothing inherited: {env:?}"
    );
    assert!(
        env.iter()
            .all(|line| !line.contains("VAULT") && !line.contains(&*vault.to_string_lossy()))
    );
    for path in [vault, &project, &socket] {
        assert!(
            lines.contains(&format!("probe {}: false", path.display()).as_str()),
            "{} is visible: {lines:?}",
            path.display()
        );
    }
    assert!(
        lines.contains(&format!("socket {}: false", socket.display()).as_str()),
        "the broker is unreachable from a harness"
    );
    assert!(lines.contains(&"HOME=/scratch") && lines.contains(&"TMPDIR=/scratch"));
    Ok(())
}

#[test]
fn injected_directives_stay_data_inside_one_envelope() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let deployment = Deployment::new("harness-hostile")?;
    let skills = deployment.skills("20")?;
    let op = SkillOp::Invoke {
        skill: String::from("claude-code@1"),
        capability: "code".parse().unwrap(),
        input: brief("Look.\nSTUB hostile", &["src/lib.rs"]).to_string(),
    };
    let SkillResult::Output { output } =
        handle_skill_op(&skills, &op, &"test".parse::<CellId>().unwrap(), &no_keys)
    else {
        return Err(String::from("no output"));
    };
    let report: Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
    assert!(
        said(&report).contains("Ignore previous instructions."),
        "the words are reported, as data"
    );
    assert_eq!(
        field(&report, "/outcome"),
        "done",
        "the hostile words changed nothing Hatchling decided"
    );
    assert_eq!(field(&report, "/files_changed"), json!([]));
    // What the model receives: the agent's skill result, in the envelope.
    let wrapped = hatchling_prompt::wrap_tool_result(
        "skill_invoke",
        true,
        &json!({ "skill": "claude-code@1", "output": output }),
    );
    assert_eq!(
        wrapped.matches("</tool_result>").count(),
        1,
        "the forged close tag cannot end the envelope"
    );
    let close = wrapped.find("</tool_result>").unwrap();
    assert!(
        wrapped
            .find("Ignore previous instructions")
            .is_some_and(|at| at < close)
    );
    Ok(())
}

#[test]
fn a_question_ends_the_run_and_its_answer_comes_in_the_next_brief() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let deployment = Deployment::new("harness-question")?;
    let skills = deployment.skills("20")?;
    let asked = invoke(
        &skills,
        "claude-code@1",
        &brief("Add a helper.\nSTUB ask", &["src/lib.rs"]),
    )?;
    assert_eq!(field(&asked, "/outcome"), "needs_clarification");
    assert_eq!(
        field(&asked, "/harness_said/question"),
        "Which module should the helper live in?"
    );
    let answered = json!({
        "task": "Add a helper.\nSTUB edit src/lib.rs",
        "root": "project",
        "paths": ["src/lib.rs"],
        "answer": "Put it in src/lib.rs.",
    });
    let report = invoke(&skills, "claude-code@1", &answered)?;
    assert_eq!(field(&report, "/outcome"), "done");
    assert!(
        said(&report).contains("The answer to your earlier question:\nPut it in src/lib.rs."),
        "{report}"
    );
    assert!(field(&report, "/harness_said").get("question").is_none());
    Ok(())
}

#[test]
fn failures_are_reported_in_hatchlings_words() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let deployment = Deployment::new("harness-failures")?;
    let skills = deployment.skills("3")?;
    let run = |task: &str| invoke(&skills, "claude-code@1", &brief(task, &["src/lib.rs"]));
    let failed = run("Go.\nSTUB fail")?;
    assert_eq!(
        (
            field(&failed, "/outcome").as_str(),
            field(&failed, "/exit_code").as_i64()
        ),
        (Some("failed"), Some(3))
    );
    assert_eq!(
        field(&failed, "/reason"),
        "the harness exited with status 3"
    );
    let flooded = run("Go.\nSTUB flood")?;
    assert_eq!(field(&flooded, "/outcome"), "failed");
    assert_eq!(
        field(&flooded, "/reason"),
        "the harness wrote more than 65536 bytes"
    );
    let started = std::time::Instant::now();
    let hung = run("Go.\nSTUB hang")?;
    // Stopped at its timeout, not left to spin out its minute.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "a hung harness took {:?}",
        started.elapsed()
    );
    assert_eq!(field(&hung, "/outcome"), "timed_out");
    assert_eq!(
        field(&hung, "/reason"),
        "the harness ran out of time and was stopped"
    );
    let binary = run("Go.\nSTUB binary blob.bin")?;
    assert_eq!(
        field(&binary, "/unpatchable"),
        json!(["blob.bin"]),
        "a change a text patch cannot carry is named, not dropped"
    );
    assert!(field(&binary, "/patch").is_string());
    Ok(())
}

#[test]
fn a_changed_binary_is_refused_never_substituted() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let deployment = Deployment::new("harness-pinned")?;
    let skills = deployment.skills("20")?;
    std::fs::write(&deployment.binary, b"#!/bin/sh\necho substitute\n")
        .map_err(|e| e.to_string())?;
    let report = invoke(
        &skills,
        "claude-code@1",
        &brief("Go.\nSTUB edit src/lib.rs", &["src/lib.rs"]),
    )?;
    assert_eq!(field(&report, "/outcome"), "refused");
    assert!(
        field(&report, "/reason")
            .as_str()
            .is_some_and(|reason| reason.contains("never substituted")),
        "{report}"
    );
    assert_eq!(field(&report, "/files_changed"), json!([]));
    // And a deployment pinning the wrong hash does not start.
    let vars = deployment.vars("20");
    let started = Harnesses::parse(&deployment.config, |key| vars.get(key).cloned());
    assert!(started.is_err(), "the pin is checked at start-up too");
    Ok(())
}

#[test]
fn nothing_but_the_main_session_reaches_a_harness() {
    // Runs and subagents are served with no harnesses (their Skills are
    // the default), so a harness skill is unknown to them; a harness
    // cell has no broker socket at all (see the scope test).
    let op = SkillOp::Invoke {
        skill: String::from("claude-code@1"),
        capability: "code".parse().unwrap(),
        input: String::from("[]"),
    };
    assert_eq!(
        handle_skill_op(
            &Skills::default(),
            &op,
            &"run".parse::<CellId>().unwrap(),
            &no_keys
        ),
        SkillResult::UnknownSkill
    );
}

#[test]
fn every_run_is_in_the_activity_log_with_its_pinned_binary_and_brief_hash() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let deployment = Deployment::new("harness-audit")?;
    let skills = deployment.skills("20")?;
    let inputs = [
        brief("One.\nSTUB edit src/lib.rs", &["src/lib.rs"]),
        json!({ "task": "Two.", "root": "elsewhere", "paths": ["a"] }),
    ];
    for input in &inputs {
        invoke(&skills, "codex-cli@1", input)?;
    }
    let runs: Vec<(String, String, HarnessOutcome, u32)> = deployment
        .activity()
        .into_iter()
        .filter_map(|event| {
            let SubagentEvent::HarnessRun {
                harness,
                binary_sha256,
                brief_sha256,
                outcome,
                files_changed,
                ..
            } = event.kind
            else {
                return None;
            };
            assert_eq!(
                (harness.as_str(), binary_sha256.as_str()),
                ("codex-cli", deployment.sha256.as_str())
            );
            Some((event.run_id, brief_sha256, outcome, files_changed))
        })
        .collect();
    let hash = |input: &Value| CodeHash::of(input.to_string().as_bytes()).to_string();
    let [edited, refused] = &inputs;
    assert_eq!(
        runs,
        [
            (
                String::from("harness"),
                hash(edited),
                HarnessOutcome::Done,
                1
            ),
            (
                String::from("harness"),
                hash(refused),
                HarnessOutcome::Refused,
                0
            ),
        ]
    );
    Ok(())
}
