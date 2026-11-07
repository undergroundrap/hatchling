//! The harness's secrets check against the real vault keeper: the broker's
//! binary as `vault-keeper`, on a socket pair, its secrets sealed by
//! `migrate-vault` and its sign-ins kept through it, as on an install.
//!
//! `tests/harness.rs` checks the run's logic against a stand-in vault, and
//! a stand-in is how the check once passed while never seeing a real key:
//! with a keeper, what it scanned for was only what a cell had already had.
//! So here nothing stands in for the keeper, and no cell has had any of
//! these secrets, as [`no_cell_has_had_them`] proves.
//!
//! A run refused before its cell starts is told from one that went on by
//! a spawner that isn't there: one that goes on fails on it. The runs that
//! need a cell need user namespaces, as in `tests/harness.rs`.
#![allow(
    clippy::indexing_slicing,
    reason = "fixtures: a report without a field, or a text too short, fails the calling test"
)]

#[allow(
    clippy::redundant_pub_crate,
    reason = "test helpers use pub(crate); the private `mod common` makes it technically redundant"
)]
mod common;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::os::fd::{AsFd as _, AsRawFd as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::Engine as _;
use common::{TestDir, broker_config, guard_cell_available, harness_fixture_binary, test_dir};
use hatchling_broker::browser_credentials::{SignIn, SignInAnswer, SignInRequest};
use hatchling_broker::harness::{HARNESS_CAPABILITY, vault_keys_through};
use hatchling_broker::held_check::{CHECK_PURPOSE, HeldHit, MAX_CHECK_BYTES, MIN_SCAN_BYTES};
use hatchling_broker::keeper::Keeper;
use hatchling_broker::vault_access::{Access, CHECK_ACTION, Caller, Outcome, read_log};
use hatchling_broker::{Config, Harnesses, SkillRuntime, Skills, handle_skill_op};
use hatchling_core::framing::{read_frame, read_frame_or_eof, write_frame};
use hatchling_core::ui::Concealed;
use hatchling_core::{CellId, MAX_FRAME_BYTES, SkillOp, SkillResult};
use hatchling_scheduler::SentTo;
use hatchling_skill::CodeHash;
use hatchling_vault::StoreKeys;
use serde_json::{Value, json};

/// An egress credential, in the vault under the name a grant reads it by.
const EGRESS_KEY: &str = "sk-egress-no-cell-has-had-0123456";
/// A harness's vendor key.
const VENDOR_KEY: &str = "sk-ant-vendor-no-cell-has-had-789";
/// Search's key.
const SEARCH_KEY: &str = "search-key-no-cell-has-had-abcdef";
/// A channel account's credential.
const ACCOUNT_KEY: &str = "mail-account-no-cell-has-had-1234";
/// A key whose JSON would escape it: its quote and backslash.
const QUOTED_KEY: &str = r#"sk-quote"and\slash-no-cell-had"#;
/// A sign-in's password, kept through the keeper.
const PASSWORD: &str = "correct-horse-sign-in-password";
/// A sign-in's username, which isn't a secret.
const USERNAME: &str = "zoe-the-username";
/// A key shorter than anything looked for inside a text.
const SHORT_KEY: &str = "pw-1234";

/// The keys in the vault, by the names an install stores them under.
const STORED: [(&str, &str); 6] = [
    ("egress-localhost-18089", EGRESS_KEY),
    ("harness-claude-code", VENDOR_KEY),
    ("search-brave", SEARCH_KEY),
    ("mail-work", ACCOUNT_KEY),
    ("quoted", QUOTED_KEY),
    ("short", SHORT_KEY),
];

/// An install: a sealed vault, its keeper running, a project and a harness
/// configured against them.
struct Install {
    dir: TestDir,
    config: Config,
    keeper: Keeper,
    keys: StoreKeys,
    /// The authority's end of the keeper, until a test takes it.
    authority: Option<UnixStream>,
    spawner: UnixStream,
    vars: HashMap<&'static str, String>,
}

/// The keeper's environment under `dir`.
fn environment(dir: &TestDir) -> Vec<(&'static str, String)> {
    let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
    vec![
        ("TMPDIR", path("tmp")),
        ("HATCHLING_BROKER_SOCKET", path("sock/broker.sock")),
        (
            "HATCHLING_AGENT_UID",
            rustix::process::getuid().as_raw().to_string(),
        ),
        ("HATCHLING_LOG_LEVEL", String::from("error")),
        ("HATCHLING_VAULT_DIR", path("vault")),
        ("HATCHLING_EGRESS_GRANTS", String::from("localhost:18089")),
        ("HATCHLING_ARTIFACT_DIR", path("artifacts")),
        ("HATCHLING_BROWSER_PROFILES_DIR", path("profiles")),
        ("HATCHLING_BROWSER_CREDENTIALS_DIR", path("sign-ins")),
    ]
}

/// Seals `secrets` in the vault under `dir`, as an operator does.
fn seal(dir: &TestDir, secrets: &[(&str, &str)]) -> Result<(), String> {
    let vault = dir.join("vault");
    std::fs::create_dir(&vault).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&vault, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| e.to_string())?;
    for (name, value) in secrets {
        std::fs::write(vault.join(name), value).map_err(|e| e.to_string())?;
    }
    let migrated = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("migrate-vault")
        .envs(environment(dir))
        .env_remove("HATCHLING_BROWSER_PROFILES_DIR")
        .env_remove("HATCHLING_BROWSER_CREDENTIALS_DIR")
        .output()
        .map_err(|e| e.to_string())?;
    if migrated.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&migrated.stderr).into_owned())
    }
}

/// The keeper as the broker starts it.
fn program(dir: &TestDir) -> Command {
    let mut program = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"));
    program
        .envs(environment(dir))
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    program
}

impl Install {
    /// An install holding `secrets`, a sign-in, and a harness.
    fn holding(name: &str, secrets: &[(&str, &str)]) -> Result<Self, String> {
        let dir = test_dir(name);
        let _ = std::fs::remove_dir_all(&*dir);
        for sub in ["project/src", "workspace", "sock", "bin", "tmp"] {
            std::fs::create_dir_all(dir.join(sub)).map_err(|e| e.to_string())?;
        }
        std::fs::write(dir.join("project/src/lib.rs"), "pub fn a() {}\n")
            .map_err(|e| e.to_string())?;
        seal(&dir, secrets)?;
        let (keeper, keys, authority, spawner) =
            Keeper::start_with(program(&dir)).map_err(|e| e.to_string())?;
        let kept = keeper
            .sign_ins(SignInRequest::Keep {
                profile_id: String::from("profile-1"),
                origin: String::from("https://a.example"),
                label: String::from("Work"),
                sign_in: SignIn {
                    username: Concealed::new(String::from(USERNAME)),
                    password: Concealed::new(String::from(PASSWORD)),
                },
                now: 1,
            })
            .map_err(|e| e.to_string())?;
        if !matches!(kept, SignInAnswer::Kept { .. }) {
            return Err(String::from("the sign-in wasn't kept"));
        }
        let binary = dir.join("bin/harness");
        std::fs::copy(harness_fixture_binary()?, &binary).map_err(|e| e.to_string())?;
        let sha256 = CodeHash::of(&std::fs::read(&binary).map_err(|e| e.to_string())?).to_string();
        let uid = rustix::process::getuid().as_raw();
        let mut config = broker_config(&dir.join("sock/broker.sock"), uid.wrapping_add(1))?;
        config.vault.root = dir.join("vault");
        config.vault.key_file = hatchling_broker::VaultConfig::default_key_file(&config.vault.root);
        config.artifacts.workspace = Some(dir.join("workspace"));
        config.scheduler.root = dir.join("schedules");
        let code: HashSet<_> = [HARNESS_CAPABILITY.parse().map_err(|_| "cap")?].into();
        config.skills.grants = HashMap::from([(String::from("claude-code"), code)]);
        let vars = HashMap::from([
            (
                "HATCHLING_HARNESS_CLAUDE_CODE",
                binary.display().to_string(),
            ),
            ("HATCHLING_HARNESS_CLAUDE_CODE_SHA256", sha256),
            (
                "HATCHLING_HARNESS_ROOTS",
                format!("project={}", dir.join("project").display()),
            ),
            ("HATCHLING_HARNESS_TIMEOUT_SECS", String::from("20")),
        ]);
        Ok(Self {
            dir,
            config,
            keeper,
            keys,
            authority: Some(authority),
            spawner,
            vars,
        })
    }

    /// An install holding every key above.
    fn new(name: &str) -> Result<Self, String> {
        Self::holding(name, &STORED)
    }

    /// Its harnesses, started with `spawner`.
    fn skills(&self, spawner: PathBuf) -> Result<Skills, String> {
        let harnesses = Harnesses::parse(&self.config, |key| self.vars.get(key).cloned())
            .map_err(|e| e.to_string())?;
        Ok(Skills::default()
            .with_runtime(SkillRuntime::new(spawner))
            .with_harnesses(harnesses))
    }

    /// Its harnesses, with no spawner: a run that isn't refused first fails
    /// where its cell would start.
    fn no_cell(&self) -> Result<Skills, String> {
        self.skills(PathBuf::from("/nonexistent/hatchling-spawner"))
    }

    /// `task` run by `skills`, on `paths`, as the broker runs it, asking
    /// `keeper`: the report, or why there's none.
    fn run_with(
        &self,
        skills: &Skills,
        keeper: &Keeper,
        input: &Value,
    ) -> Result<Result<Value, SkillResult>, String> {
        let op = SkillOp::Invoke {
            skill: String::from("claude-code@1"),
            capability: "code".parse().map_err(|_| "capability")?,
            input: input.to_string(),
        };
        let cell: CellId = "test".parse().map_err(|_| "cell id")?;
        let keys = vault_keys_through(Some(keeper), &self.config, &self.keys);
        match handle_skill_op(skills, &op, &cell, &keys) {
            SkillResult::Output { output } => serde_json::from_str(&output)
                .map(Ok)
                .map_err(|e| e.to_string()),
            refused @ (SkillResult::UnknownSkill
            | SkillResult::CapabilityDenied
            | SkillResult::SkillError) => Ok(Err(refused)),
        }
    }

    /// `input` run by `skills` for an agent, whose run `hit` ends: the report.
    fn run_in(&self, skills: &Skills, input: &Value, hit: &HeldHit) -> Result<Value, String> {
        let op = SkillOp::Invoke {
            skill: String::from("claude-code@1"),
            capability: "code".parse().map_err(|_| "capability")?,
            input: input.to_string(),
        };
        let cell: CellId = "test".parse().map_err(|_| "cell id")?;
        let keys = vault_keys_through(Some(&self.keeper), &self.config, &self.keys).in_run(hit);
        match handle_skill_op(skills, &op, &cell, &keys) {
            SkillResult::Output { output } => {
                serde_json::from_str(&output).map_err(|e| e.to_string())
            }
            refused @ (SkillResult::UnknownSkill
            | SkillResult::CapabilityDenied
            | SkillResult::SkillError) => Err(format!("{refused:?}")),
        }
    }

    fn run(&self, skills: &Skills, task: &str, paths: &[&str]) -> Result<Value, String> {
        let input = json!({ "task": task, "root": "project", "paths": paths });
        self.run_with(skills, &self.keeper, &input)?
            .map_err(|refused| format!("{refused:?}"))
    }

    /// Writes `text` at `name` in the project.
    fn file(&self, name: &str, text: &str) -> Result<(), String> {
        std::fs::write(self.dir.join("project").join(name), text).map_err(|e| e.to_string())
    }

    fn accesses(&self) -> Result<Vec<Access>, String> {
        let (accesses, unreadable) =
            read_log(&self.dir.join("vault")).map_err(|e| e.to_string())?;
        assert_eq!(unreadable, 0);
        Ok(accesses)
    }

    /// The checks in the access log.
    fn checks(&self) -> Result<Vec<Access>, String> {
        let mut accesses = self.accesses()?;
        accesses.retain(|access| access.action == CHECK_ACTION);
        Ok(accesses)
    }

    /// That the access log as a file holds no secret.
    fn log_is_clean(&self) -> Result<(), String> {
        let log = std::fs::read_to_string(self.dir.join("vault/.access.log"))
            .map_err(|e| e.to_string())?;
        for (_, secret) in STORED {
            assert!(!log.contains(secret), "the access log holds a secret");
        }
        assert!(!log.contains(PASSWORD), "the access log holds a password");
        Ok(())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

fn broker() -> Caller {
    Caller::Broker {
        purpose: CHECK_PURPOSE.to_owned(),
    }
}

/// That a run was refused before its cell started: with no spawner, one
/// that went on would have failed there, and the harness said nothing.
fn refused_before_its_cell(report: &Value, why: &str, secret: &str) {
    assert_eq!(report["outcome"], "refused", "{report}");
    assert_eq!(report["harness_said"]["message"], "", "{report}");
    assert_eq!(report["patch"], Value::Null, "{report}");
    assert!(
        report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains(why)),
        "{report}"
    );
    assert!(
        !report.to_string().contains(secret),
        "the report shows the secret"
    );
}

/// None of these keys is in what the gates scan for: no cell has had one.
/// With only that set, every refusal below would be a run that went on.
fn no_cell_has_had_them(install: &Install) -> Result<(), String> {
    assert!(
        install
            .keeper
            .scan("output-gate")
            .map_err(|e| e.to_string())?
            .is_empty(),
        "a cell has had one of these"
    );
    Ok(())
}

/// A key no cell has had, in a file given to a harness, stops the run
/// before its cell starts, whichever kind of key: a credential, a vendor's,
/// search's, a channel account's, or a sign-in's password. The refusal
/// names the file and not the key, and the log has the check. The same run
/// with a clean file goes on to its cell, so what stopped them was this.
#[test]
fn a_key_no_cell_has_had_in_a_file_stops_the_run_before_its_cell() -> Result<(), String> {
    let install = Install::new("hk-files")?;
    no_cell_has_had_them(&install)?;
    let skills = install.no_cell()?;
    let keys = [
        ("egress.env", EGRESS_KEY),
        ("vendor.env", VENDOR_KEY),
        ("search.env", SEARCH_KEY),
        ("account.env", ACCOUNT_KEY),
        ("sign-in.env", PASSWORD),
    ];
    for (file, secret) in keys {
        install.file(file, &format!("KEY={secret}\nMORE=1\n"))?;
        let report = install.run(&skills, "Tidy the settings.", &[file])?;
        refused_before_its_cell(&report, file, secret);
    }
    let checks = install.checks()?;
    let found: Vec<&Access> = checks
        .iter()
        .filter(|check| check.found == Some(true))
        .collect();
    assert_eq!(found.len(), keys.len(), "{checks:?}");
    for (check, (_, secret)) in found.iter().zip(keys) {
        assert_eq!(check.caller, broker());
        assert_eq!(check.outcome, Outcome::Allowed);
        assert!(check.names.is_empty(), "a check named a secret: {check:?}");
        let size = format!("KEY={secret}\nMORE=1\n").len();
        assert_eq!(check.size, Some(u64::try_from(size).unwrap_or(u64::MAX)));
        assert!(check.at > 0);
    }
    install.log_is_clean()?;

    install.file("clean.env", "NOTHING=held\n")?;
    let report = install.run(&skills, "Tidy the settings.", &["clean.env"])?;
    assert_eq!(report["outcome"], "failed", "{report}");
    assert_eq!(report["reason"], "the harness could not run", "{report}");
    Ok(())
}

/// A secret with a quote and a backslash is escaped in the brief's JSON,
/// so a scan of the JSON doesn't find it. The check reads the task as the
/// harness is told it. Nor does a task stop at the raw form: its hex does
/// it too. And a whole answer, or a file's name, is a text of its own.
#[test]
fn a_secret_in_the_task_is_found_as_it_would_be_sent() -> Result<(), String> {
    let install = Install::new("hk-task")?;
    let skills = install.no_cell()?;
    let task = format!("Log in with {QUOTED_KEY} please.");
    let escaped = json!({ "task": task, "root": "project", "paths": ["src/lib.rs"] }).to_string();
    assert!(
        !escaped.contains(QUOTED_KEY),
        "the JSON doesn't hide it, so this proves nothing"
    );
    let report = install.run(&skills, &task, &["src/lib.rs"])?;
    refused_before_its_cell(&report, "the task holds a secret", QUOTED_KEY);

    let hex = hex(EGRESS_KEY.as_bytes());
    let report = install.run(&skills, &format!("Use {hex}."), &["src/lib.rs"])?;
    refused_before_its_cell(&report, "the task holds a secret", &hex);

    let answered = json!({
        "task": "Carry on.", "root": "project", "paths": ["src/lib.rs"], "answer": SHORT_KEY,
    });
    let report = install
        .run_with(&skills, &install.keeper, &answered)?
        .map_err(|refused| format!("{refused:?}"))?;
    refused_before_its_cell(&report, "the task holds a secret", SHORT_KEY);
    install.log_is_clean()
}

/// A brief is the agent's own text, so a secret in any of it is a guess
/// that ends the agent's run, and the refusal says the same as ever. A
/// project's file holding one is a mistake in the project: refused, and
/// the run goes on. So is a brief with nothing held, which fails only where
/// its cell would start, and a run that isn't an agent's: the owner, in the
/// app, ends nothing.
#[test]
fn only_the_agents_own_text_ends_its_run() -> Result<(), String> {
    let install = Install::new("hk-hit")?;
    let skills = install.no_cell()?;
    let cell: CellId = "test".parse().map_err(|_| "cell id")?;
    let run = |input: &Value, hit: Option<&HeldHit>| -> Result<Value, String> {
        let op = SkillOp::Invoke {
            skill: String::from("claude-code@1"),
            capability: "code".parse().map_err(|_| "capability")?,
            input: input.to_string(),
        };
        let keys = vault_keys_through(Some(&install.keeper), &install.config, &install.keys);
        let keys = match hit {
            Some(hit) => keys.in_run(hit),
            None => keys,
        };
        match handle_skill_op(&skills, &op, &cell, &keys) {
            SkillResult::Output { output } => {
                serde_json::from_str(&output).map_err(|e| e.to_string())
            }
            refused @ (SkillResult::UnknownSkill
            | SkillResult::CapabilityDenied
            | SkillResult::SkillError) => Err(format!("{refused:?}")),
        }
    };
    let brief = |task: &str| json!({ "task": task, "root": "project", "paths": ["src/lib.rs"] });

    install.file("settings.env", &format!("KEY={EGRESS_KEY}\n"))?;
    let hit = HeldHit::default();
    let holding_file = json!({ "task": "Go.", "root": "project", "paths": ["settings.env"] });
    let report = run(&holding_file, Some(&hit))?;
    refused_before_its_cell(&report, "settings.env", EGRESS_KEY);
    assert_eq!(hit.seen(), None, "a project's file is no guess");

    let report = run(&brief("Tidy up."), Some(&hit))?;
    assert_eq!(report["outcome"], "failed", "{report}");
    assert_eq!(hit.seen(), None, "a clean brief ends nothing");

    let report = run(&brief(&format!("Use {VENDOR_KEY}.")), None)?;
    refused_before_its_cell(&report, "the task holds a secret", VENDOR_KEY);

    let briefs = [
        brief(&format!("Use {EGRESS_KEY}.")),
        json!({ "task": "Go.", "root": "project", "paths": ["src/lib.rs"], "answer": SHORT_KEY }),
        json!({ "task": "Go.", "root": "project", "paths": [ACCOUNT_KEY] }),
    ];
    for input in briefs {
        let hit = HeldHit::default();
        let report = run(&input, Some(&hit))?;
        refused_before_its_cell(&report, "the task holds a secret", SHORT_KEY);
        assert_eq!(hit.seen(), Some(SentTo::Harness), "{input}");
    }
    let found = install.checks()?;
    assert!(found.iter().any(|check| check.found == Some(true)));
    install.log_is_clean()
}

/// What a real run hands back goes through the real keeper too: a patch
/// and a message holding a key no cell was given are withheld, and with
/// nothing held both come through.
#[test]
fn what_a_run_hands_back_is_checked_by_the_keeper() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let task = "Add a helper.\nSTUB edit src/lib.rs";
    let holding = Install::holding(
        "hk-back",
        &[
            ("patch-text", "edited by the stub harness"),
            ("message-text", "You are working on a task"),
        ],
    )?;
    let skills = holding.skills(common::spawner())?;
    let report = holding.run(&skills, task, &["src/lib.rs"])?;
    assert_eq!(report["outcome"], "done", "{report}");
    assert_eq!(report["patch"], Value::Null, "{report}");
    assert_eq!(report["patch_withheld"], true, "{report}");
    let message = report["harness_said"]["message"].as_str().unwrap_or("");
    assert!(message.starts_with("[withheld"), "{report}");
    assert!(
        std::fs::read_dir(holding.dir.join("workspace"))
            .map_err(|e| e.to_string())?
            .next()
            .is_none(),
        "the patch was written"
    );

    let clear = Install::holding("hk-clear", &[("unrelated", "held-but-not-in-this-run")])?;
    let skills = clear.skills(common::spawner())?;
    let report = clear.run(&skills, task, &["src/lib.rs"])?;
    assert_eq!(report["outcome"], "done", "{report}");
    assert!(report["patch"].is_string(), "{report}");
    assert_eq!(report["patch_withheld"], Value::Null, "{report}");
    assert!(
        report["harness_said"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("You are working on a task")),
        "{report}"
    );
    Ok(())
}

/// What a run hands back is the run's own doing: the files it was given
/// were checked clean. So a secret in the patch or in any text of the report
/// ends an agent's run, in the words that say what happened, whichever it
/// was found in. An owner's run is only withheld from, as above, and a run
/// with nothing held goes on.
#[test]
fn a_secret_handed_back_ends_an_agents_run() -> Result<(), String> {
    if !guard_cell_available()? {
        return Ok(());
    }
    let task = "Add a helper.\nSTUB edit src/lib.rs";
    for (tag, name, value) in [
        ("hk-end-patch", "patch-text", "edited by the stub harness"),
        ("hk-end-told", "message-text", "You are working on a task"),
    ] {
        let install = Install::holding(tag, &[(name, value)])?;
        let skills = install.skills(common::spawner())?;
        let input = json!({ "task": task, "root": "project", "paths": ["src/lib.rs"] });
        let hit = HeldHit::default();
        let report = install.run_in(&skills, &input, &hit)?;
        assert_eq!(report["outcome"], "done", "{name}");
        assert_eq!(hit.seen(), Some(SentTo::HarnessReturn), "{name}");
    }
    let clear = Install::holding("hk-end-clear", &[("unrelated", "held-but-not-in-this-run")])?;
    let skills = clear.skills(common::spawner())?;
    let input = json!({ "task": task, "root": "project", "paths": ["src/lib.rs"] });
    let hit = HeldHit::default();
    let report = clear.run_in(&skills, &input, &hit)?;
    assert!(report["patch"].is_string(), "the patch was withheld");
    assert_eq!(hit.seen(), None, "nothing held, nothing ended");
    Ok(())
}

/// A brief that asks for something unconfigured is quoted back, and a brief
/// holding a key would be quoted with it. Nothing ran: that's the agent's
/// own text, not a harness's hand-back, and the words must say so.
#[test]
fn a_key_quoted_back_from_a_refused_brief_is_the_briefs() -> Result<(), String> {
    let install = Install::new("hk-quoted")?;
    let skills = install.no_cell()?;
    let input = json!({ "task": "Go.", "root": EGRESS_KEY, "paths": ["src/lib.rs"] });
    let hit = HeldHit::default();
    let report = install.run_in(&skills, &input, &hit)?;
    assert_eq!(report["outcome"], "refused", "the brief's refusal");
    assert!(
        report["reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("[withheld")),
        "what was quoted is withheld"
    );
    assert_eq!(hit.seen(), Some(SentTo::Harness));
    Ok(())
}

/// The minimum length is the keeper's rule: a short key is the whole text
/// or nothing, and one of the minimum length is found anywhere. A text
/// checks as one text, however many frames it takes, so a key across a
/// frame's end is found, and in the base64 and hex it arrives in too.
#[test]
fn the_keeper_finds_whole_values_across_frames_in_every_form() -> Result<(), String> {
    let at_minimum = "x7-eight";
    assert_eq!(at_minimum.len(), MIN_SCAN_BYTES);
    assert!(SHORT_KEY.len() < MIN_SCAN_BYTES);
    let install = Install::holding(
        "hk-rules",
        &[
            ("short", SHORT_KEY),
            ("long", EGRESS_KEY),
            ("minimum", at_minimum),
        ],
    )?;
    let check = |text: &[u8]| install.keeper.contains(text).map_err(|e| e.to_string());

    // The minimum length.
    assert!(check(SHORT_KEY.as_bytes())?, "the whole text");
    assert!(
        !check(format!("a {SHORT_KEY} b").as_bytes())?,
        "inside a text"
    );
    assert!(
        !check(format!("{SHORT_KEY}\n").as_bytes())?,
        "one byte more"
    );
    assert!(!check(&SHORT_KEY.as_bytes()[1..])?, "a part of it");
    assert!(
        check(format!("a {at_minimum} b").as_bytes())?,
        "at the minimum"
    );

    // Hex and base64.
    let long = EGRESS_KEY.as_bytes();
    let hex = hex(long);
    assert!(check(format!("id={hex};").as_bytes())?, "hex");
    assert!(
        check(hex.to_ascii_uppercase().as_bytes())?,
        "upper-case hex"
    );
    let base64 = base64::engine::general_purpose::STANDARD;
    let url_safe = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    for lead in ["", "a", "ab"] {
        let stream = [lead.as_bytes(), long, b"more"].concat();
        assert!(check(base64.encode(&stream).as_bytes())?, "after {lead:?}");
        assert!(
            check(url_safe.encode(&stream).as_bytes())?,
            "after {lead:?}"
        );
    }
    let without_a_byte = &long[..long.len() - 1];
    assert!(!check(without_a_byte)?, "a value is found whole");

    // A key across each kind of frame's end: it starts in one frame and
    // ends in the next, or the one after.
    let fill = |bytes: usize| vec![b'.'; bytes];
    let half = long.len() / 2;
    let split = [fill(MAX_FRAME_BYTES - half), long.to_vec(), fill(10)].concat();
    assert!(
        !split[..MAX_FRAME_BYTES]
            .windows(long.len())
            .any(|at| at == long)
            && !split[MAX_FRAME_BYTES..]
                .windows(long.len())
                .any(|at| at == long),
        "neither frame holds the whole key, so this proves nothing"
    );
    assert!(check(&split)?, "across one frame's end");
    let later = [
        fill(3 * MAX_FRAME_BYTES - half),
        long.to_vec(),
        fill(MAX_FRAME_BYTES),
    ]
    .concat();
    assert!(check(&later)?, "across a later frame's end");
    assert!(!check(&fill(MAX_FRAME_BYTES + 10))?, "the same size, clean");
    let exact = [fill(MAX_FRAME_BYTES - long.len()), long.to_vec()].concat();
    assert!(check(&exact)?, "ending exactly at a frame's end");

    // The empty text holds nothing, and what isn't held isn't found.
    assert!(!check(b"")?);
    assert!(!check(b"nothing held in this")?);
    assert_eq!(
        install.checks()?.last().and_then(|check| check.found),
        Some(false)
    );
    Ok(())
}

/// A text too long to be a run's is refused before a byte of it is sent,
/// on the record, and the keeper goes on answering.
#[test]
fn a_text_past_the_bound_is_refused_and_recorded() -> Result<(), String> {
    let install = Install::new("hk-long")?;
    let long = vec![b'.'; usize::try_from(MAX_CHECK_BYTES + 1).map_err(|e| e.to_string())?];
    let refused = install.keeper.contains(&long).map_or_else(
        |error| error.to_string(),
        |found| format!("answered {found}"),
    );
    assert!(refused.contains("too long"), "{refused}");
    let checks = install.checks()?;
    let [only] = checks.as_slice() else {
        return Err(format!("{checks:?}"));
    };
    assert_eq!(only.outcome, Outcome::Denied);
    assert_eq!(
        only.size,
        Some(u64::try_from(long.len()).unwrap_or(u64::MAX))
    );
    assert_eq!(only.found, None);
    assert_eq!(install.keeper.contains(b"a clean text").ok(), Some(false));
    Ok(())
}

/// The egress authority asks and is refused, on its own channel, before
/// any text moves: refused and recorded as denied, as its and not the
/// broker's, and its channel goes on serving its reads.
#[test]
fn the_egress_authority_is_refused_a_check() -> Result<(), String> {
    let mut install = Install::new("hk-authority")?;
    let authority = install.authority.take().ok_or("no authority's channel")?;
    let authority = Keeper::over(authority).map_err(|e| e.to_string())?;
    let refused = authority.contains(EGRESS_KEY.as_bytes());
    let told = format!("{refused:?}");
    assert!(told.contains("only reads"), "{told}");
    let checks = install.checks()?;
    let [only] = checks.as_slice() else {
        return Err(format!("{checks:?}"));
    };
    assert_eq!(
        only.caller,
        Caller::Authority {
            purpose: CHECK_PURPOSE.to_owned()
        }
    );
    assert_eq!((only.outcome, only.found), (Outcome::Denied, None));
    let name = "egress-localhost-18089"
        .parse()
        .map_err(|_| "a secret's name")?;
    let read = authority
        .read("egress", &[name])
        .map_err(|e| e.to_string())?;
    assert_eq!(
        read.first()
            .and_then(Option::as_ref)
            .map(hatchling_vault::Secret::as_bytes),
        Some(EGRESS_KEY.as_bytes())
    );
    Ok(())
}

/// A cell's channel carries vault ops alone: a check there ends the
/// channel, unanswered and unrecorded, whatever grants it was named with.
#[test]
fn a_cell_is_refused_a_check() -> Result<(), String> {
    let install = Install::new("hk-cell")?;
    let (ours, theirs) = UnixStream::pair().map_err(|e| e.to_string())?;
    ours.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let named = json!({ "cell": "cell-asks", "uid": 1000, "grants": ["store:quoted"] });
    write_frame(&install.spawner, named.to_string().as_bytes()).map_err(|e| e.to_string())?;
    hatchling_sys::net::send_fd(install.spawner.as_fd(), theirs.as_fd())
        .map_err(|e| e.to_string())?;
    drop(theirs);
    let bytes = EGRESS_KEY.len();
    write_frame(
        &ours,
        json!({ "request": "contains", "bytes": bytes })
            .to_string()
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    assert!(
        read_frame_or_eof(&ours).is_ok_and(|frame| frame.is_none()),
        "a cell was answered"
    );
    assert!(install.checks()?.is_empty(), "a cell's check was made");
    Ok(())
}

/// The keeper started by hand: its child, and the broker's end, its start
/// answered, so a test can stop it.
fn keeper_to_stop(dir: &TestDir) -> Result<(Child, Keeper, StoreKeys), String> {
    let vault = |error: hatchling_vault::Error| error.to_string();
    let (ours, theirs) = UnixStream::pair().map_err(|e| e.to_string())?;
    hatchling_sys::keep_across_exec(theirs.as_fd()).map_err(|e| e.to_string())?;
    let child = Command::new(env!("CARGO_BIN_EXE_hatchling-broker"))
        .arg("vault-keeper")
        .envs(environment(dir))
        .env("HATCHLING_KEEPER_FD", theirs.as_raw_fd().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    drop(theirs);
    ours.set_read_timeout(Some(Duration::from_secs(20)))
        .map_err(|e| e.to_string())?;
    let asking = read_frame(&ours).map_err(|e| e.to_string())?;
    let Some((&2, asking)) = asking.split_first() else {
        return Err(String::from("the keeper didn't ask for its keys"));
    };
    let asked = hatchling_vault::handoff::asked(asking).map_err(vault)?;
    let kept = asked
        .iter()
        .map(|(store, root)| hatchling_vault::wrapped_store_key(root, *store))
        .collect::<Result<Vec<_>, _>>()
        .map_err(vault)?;
    let kept = hatchling_vault::handoff::kept_bytes(&kept).map_err(vault)?;
    write_frame(&ours, &kept).map_err(|e| e.to_string())?;
    let answer = read_frame(&ours).map_err(|e| e.to_string())?;
    let Some((&0, handed)) = answer.split_first() else {
        return Err(String::from("the keeper handed nothing over"));
    };
    let keys = StoreKeys::from_bytes(handed).map_err(vault)?;
    Ok((child, Keeper::over(ours).map_err(|e| e.to_string())?, keys))
}

/// A keeper that can't answer stops the run: it isn't started, and nothing
/// of it is reported. It answers before it goes, so this is the going.
#[test]
fn a_keeper_that_cannot_answer_stops_the_run() -> Result<(), String> {
    let install = Install::new("hk-gone")?;
    let skills = install.no_cell()?;
    let (mut child, keeper, _keys) = keeper_to_stop(&install.dir)?;
    assert_eq!(keeper.contains(b"a clean text").ok(), Some(false));
    let input = json!({ "task": "Go.", "root": "project", "paths": ["src/lib.rs"] });
    child.kill().map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    let stopped = install.run_with(&skills, &keeper, &input)?;
    assert!(
        matches!(stopped, Err(SkillResult::SkillError)),
        "{stopped:?}"
    );
    // No answer isn't a hit: nothing says the brief held one.
    let hit = HeldHit::default();
    let op = SkillOp::Invoke {
        skill: String::from("claude-code@1"),
        capability: "code".parse().map_err(|_| "capability")?,
        input: input.to_string(),
    };
    let cell: CellId = "test".parse().map_err(|_| "cell id")?;
    let keys = vault_keys_through(Some(&keeper), &install.config, &install.keys).in_run(&hit);
    let _ = handle_skill_op(&skills, &op, &cell, &keys);
    assert_eq!(hit.seen(), None);
    let again = format!("{:?}", keeper.contains(b"a clean text"));
    assert!(again.contains("lost"), "{again}");
    Ok(())
}
