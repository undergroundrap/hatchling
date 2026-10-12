//! Subagent delegation: the broker side.
//!
//! A parent session asks for a child with [`SubagentOp::Spawn`]. The
//! broker, never the parent, decides everything that bounds it:
//!
//! - **Depth.** A child's session is served with its depth recorded, and
//!   a session at [`MAX_DEPTH`] refuses every spawn. Structural, not a
//!   suggestion: see the threat note in [`hatchling_core::subagent`].
//! - **Narrowing.** The child's grants are exactly the capabilities asked
//!   for, and each must be one the parent holds; a request for anything
//!   else is refused before a cell exists.
//! - **Budget.** Steps are drawn from the parent session's [`Ledger`],
//!   which only fills: an allocation is spent whether the child uses it
//!   or not, so serial children cannot launder budget. The child's own
//!   loop is bounded by its allocation, set by the broker.
//! - **Isolation.** The child runs in its own run cell with its own
//!   socket and a session scoped to its grants, the cell the scheduler's
//!   runs use ([`CellLauncher`]): never the parent's cell, never a bare
//!   process.
//! - **The report.** The parent gets a typed [`ChildReport`], never the
//!   child's tool stream. An answer that would expose a secret the
//!   child's grants reach is withheld whole.
//! - **Accountability.** The brief's hash and the result's hash are both
//!   recorded in the activity log, so data a parent smuggles through its
//!   own child leaves a trail.
//! - **Kills and orphans.** A kill records the child's partial result
//!   when there is one. When the parent's session ends, every child
//!   still running is killed and recorded the same way: no child
//!   outlives its parent.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;
use std::thread::Scope;
use std::time::{Duration, Instant};

use hatchling_artifact::SecretSet;
use hatchling_artifact::request::sha256_hex;
use hatchling_core::subagent::{
    MAX_BRIEF_BYTES, MAX_CHILD_STEPS, MAX_CHILDREN, MAX_DEPTH, MAX_LEDGER_STEPS, MAX_REASON_BYTES,
    ORPHANED,
};
use hatchling_core::{
    ActivityEvent, ChildEnvelope, ChildFailure, ChildOutcome, ChildReport, ChildVerdict,
    ScheduleGrant, SocketPath, SubagentEvent, SubagentOp, SubagentRefusal, SubagentResult,
    grant_label,
};
use hatchling_scheduler::{RunRequest, Timestamp};
use hatchling_vault::MasterKey;

use crate::cell::ScheduledRun;
use crate::run_launcher::{CellRun, RunEnd, captured};
use crate::{CellLauncher, Config, RunEnv, SchedulerConfig, held_grants, scoped_config};

/// What a withheld answer reads as.
pub const WITHHELD: &str = "[withheld: the child's answer would expose a secret its grants reach]";

/// How often a wait checks on its child.
const WAIT_POLL: Duration = Duration::from_millis(50);

/// How far past a child's own time bound a wait still waits for the
/// bound to be enforced before it kills the child itself.
pub const WAIT_GRACE: Duration = Duration::from_secs(5);

/// A parent session's subagent ledger: steps and children allocated.
/// It only fills. Allocation is spend: nothing is refunded when a child
/// ends early, fails, or is killed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Ledger {
    steps: u32,
    children: u32,
}

impl Ledger {
    /// Spends `budget` steps on one more child, or refuses: a budget of
    /// zero or over [`MAX_CHILD_STEPS`] is bad; one the ledger cannot
    /// cover, or a child past [`MAX_CHILDREN`], finds the ledger spent.
    ///
    /// # Errors
    ///
    /// [`SubagentRefusal::BadBudget`] or [`SubagentRefusal::LedgerSpent`].
    pub fn spend(&mut self, budget: u32) -> Result<(), SubagentRefusal> {
        if budget == 0 || budget > MAX_CHILD_STEPS {
            return Err(SubagentRefusal::BadBudget);
        }
        let steps = self
            .steps
            .checked_add(budget)
            .filter(|steps| *steps <= MAX_LEDGER_STEPS)
            .ok_or(SubagentRefusal::LedgerSpent)?;
        if self.children >= MAX_CHILDREN {
            return Err(SubagentRefusal::LedgerSpent);
        }
        self.steps = steps;
        self.children += 1;
        Ok(())
    }

    /// Steps spent so far.
    #[must_use]
    pub const fn steps(&self) -> u32 {
        self.steps
    }

    /// Children spawned so far.
    #[must_use]
    pub const fn children(&self) -> u32 {
        self.children
    }
}

/// Checks a spawn against the parent's session before anything is
/// spent: the depth bound, the brief, and narrowing.
///
/// # Errors
///
/// [`SubagentRefusal::TooDeep`] in a session at [`MAX_DEPTH`],
/// [`SubagentRefusal::BadTask`] for an empty or oversized brief, and
/// [`SubagentRefusal::ExceedsGrants`] for any capability the parent does
/// not hold.
pub fn check_spawn(
    config: &Config,
    task: &str,
    capabilities: &[ScheduleGrant],
) -> Result<(), SubagentRefusal> {
    if config.scheduler.subagent_depth >= MAX_DEPTH {
        return Err(SubagentRefusal::TooDeep);
    }
    if task.trim().is_empty() || task.len() > MAX_BRIEF_BYTES {
        return Err(SubagentRefusal::BadTask);
    }
    let held = held_grants(config);
    if capabilities.iter().any(|grant| !held.contains(grant)) {
        return Err(SubagentRefusal::ExceedsGrants);
    }
    Ok(())
}

/// The configuration a child's session is served under: `parent`
/// narrowed to `capabilities` by the scheduled-run scoping, with no
/// scheduler surface but its depth, one deeper than its parent's.
#[must_use]
pub fn child_config(
    parent: &Config,
    capabilities: &[ScheduleGrant],
    token: &str,
    socket: SocketPath,
) -> Config {
    let request = RunRequest {
        schedule_id: token.to_owned(),
        task: String::new(),
        grants: capabilities.to_vec(),
        due: Timestamp::UNIX_EPOCH,
        missed: false,
        channel: String::new(),
        depth: 0,
        event: None,
        space: None,
        feed: None,
    };
    let mut child = scoped_config(parent, &request, token, socket);
    child.scheduler = SchedulerConfig {
        subagent_depth: parent.scheduler.subagent_depth.saturating_add(1),
        ..SchedulerConfig::disabled()
    };
    child
}

/// `text`, or [`WITHHELD`] if it would expose one of `secrets`.
fn withhold(text: String, secrets: &SecretSet) -> String {
    if secrets.exposed_in(text.as_bytes()) {
        String::from(WITHHELD)
    } else {
        text
    }
}

/// The steps a child spent and how it ended, from how its cell ended.
///
/// A clean exit carries the child's envelope: its answer and steps
/// (never more than its budget), or its typed failure. Output that is
/// not an envelope is the answer itself, its steps unknown, so the whole
/// budget counts. Anything else is a failure: a timeout, or a cell that
/// failed or ended without a readable report, again at the whole budget.
#[must_use]
pub(crate) fn outcome_of(end: RunEnd, budget: u32, secrets: &SecretSet) -> (u32, ChildOutcome) {
    let failed = |failure: ChildFailure, detail: &str| ChildOutcome::Failed {
        failure,
        detail: detail.to_owned(),
    };
    match end {
        RunEnd::Exited {
            code: 0, output, ..
        } => {
            let result_hash = sha256_hex(&output);
            let Ok(text) = captured(output) else {
                return (
                    budget,
                    failed(ChildFailure::CellFailed, "the child's report is unreadable"),
                );
            };
            match serde_json::from_str::<ChildEnvelope>(&text) {
                Ok(ChildEnvelope::Done {
                    verdict,
                    answer,
                    steps,
                }) => (
                    steps.min(budget),
                    ChildOutcome::Finished {
                        verdict,
                        answer: withhold(answer, secrets),
                        result_hash,
                    },
                ),
                Ok(ChildEnvelope::Failed { failure, steps }) => (
                    steps.min(budget),
                    failed(failure, "the child ended without an answer"),
                ),
                Err(_) => (
                    budget,
                    ChildOutcome::Finished {
                        verdict: ChildVerdict::Ok,
                        answer: withhold(text, secrets),
                        result_hash,
                    },
                ),
            }
        }
        RunEnd::Exited { code, .. } => (
            budget,
            failed(
                ChildFailure::CellFailed,
                &format!("the child's cell exited {code}"),
            ),
        ),
        RunEnd::TimedOut => (
            budget,
            failed(ChildFailure::Timeout, "the child ran past its time bound"),
        ),
        RunEnd::Failed(_) => (
            budget,
            failed(
                ChildFailure::CellFailed,
                "the child's cell could not be waited on",
            ),
        ),
    }
}

/// What a killed child leaves: its partial result (withheld if it would
/// expose a secret, absent if unreadable) and that result's hash, which
/// exists whenever the child wrote anything at all.
#[must_use]
pub fn partial_of(output: Vec<u8>, secrets: &SecretSet) -> (Option<String>, Option<String>) {
    if output.is_empty() {
        return (None, None);
    }
    let hash = sha256_hex(&output);
    let partial = captured(output)
        .ok()
        .filter(|text| !text.is_empty())
        .map(|text| withhold(text, secrets));
    (partial, Some(hash))
}

/// Where a parent session's activity events go: the activity log (one
/// JSON event per line, owner-only) and the broker's log.
#[derive(Debug)]
struct Activity {
    log: Option<PathBuf>,
    run_id: String,
    sequence: u64,
}

impl Activity {
    fn emit(&mut self, kind: SubagentEvent) {
        self.sequence += 1;
        let event = ActivityEvent {
            event_id: format!("{}:{}", self.run_id, self.sequence),
            ts: Timestamp::now().to_string(),
            run_id: self.run_id.clone(),
            kind,
        };
        let Ok(line) = serde_json::to_string(&event) else {
            return;
        };
        tracing::info!(target: "activity", event = %line, "subagent activity");
        if let Some(log) = &self.log {
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(log)
                .and_then(|mut file| writeln!(file, "{line}"));
            if let Err(error) = written {
                tracing::warn!(%error, "cannot append to the activity log");
            }
        }
    }
}

/// One child in flight.
struct Child<'scope> {
    run: CellRun<'scope>,
    budget: u32,
    secrets: SecretSet,
}

/// A parent session's children: its launcher, ledger, children in
/// flight, and activity record. Dropping it kills every child still
/// running (the orphan rule).
pub struct Subagents<'scope, 'env> {
    config: &'env Config,
    key: &'env MasterKey,
    launcher: Option<CellLauncher<'scope, 'env>>,
    wait_bound: Duration,
    ledger: Ledger,
    children: BTreeMap<String, Child<'scope>>,
    activity: Activity,
}

impl std::fmt::Debug for Subagents<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subagents")
            .field("ledger", &self.ledger)
            .field("children", &self.children.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl<'scope, 'env> Subagents<'scope, 'env> {
    /// The children of the session `run_id`, started on `scope` through
    /// `env` (none: children cannot start), recording activity in the
    /// scheduler's activity log.
    #[must_use]
    pub fn new(
        scope: &'scope Scope<'scope, 'env>,
        config: &'env Config,
        key: &'env MasterKey,
        env: Option<RunEnv>,
        run_id: String,
    ) -> Self {
        Self {
            config,
            key,
            wait_bound: env
                .as_ref()
                .map_or(WAIT_GRACE, |env| env.timeout.saturating_add(WAIT_GRACE)),
            launcher: env.map(|env| CellLauncher::new(scope, config, key, env)),
            ledger: Ledger::default(),
            children: BTreeMap::new(),
            activity: Activity {
                log: config
                    .scheduler
                    .enabled()
                    .then(|| config.scheduler.activity_log()),
                run_id,
                sequence: 0,
            },
        }
    }

    /// The ledger, as it stands.
    #[must_use]
    pub const fn ledger(&self) -> Ledger {
        self.ledger
    }

    /// Handles one op from the parent.
    pub fn handle(&mut self, op: &SubagentOp) -> SubagentResult {
        let result = match op {
            SubagentOp::Spawn {
                task,
                budget,
                capabilities,
            } => self.spawn(task, *budget, capabilities),
            SubagentOp::Wait { child } => self.wait(child),
            SubagentOp::Kill { child, reason } => self.kill(child, reason),
        };
        result.unwrap_or_else(|reason| SubagentResult::Refused { reason })
    }

    fn spawn(
        &mut self,
        task: &str,
        budget: u32,
        capabilities: &[ScheduleGrant],
    ) -> Result<SubagentResult, SubagentRefusal> {
        check_spawn(self.config, task, capabilities)?;
        let Some(launcher) = &self.launcher else {
            return Err(SubagentRefusal::Unavailable);
        };
        // The secrets the child's grants reach, for withholding its
        // answer: known before anything is spent, or nothing starts.
        let scope = child_config(
            self.config,
            capabilities,
            "scope",
            self.config.socket.clone(),
        );
        let Ok(secrets) = crate::held_secrets(&scope, self.key) else {
            return Ok(SubagentResult::SubagentError);
        };
        self.ledger.spend(budget)?;
        let depth = self.config.scheduler.subagent_depth.saturating_add(1);
        let run = ScheduledRun {
            task: task.to_owned(),
            missed: false,
            due: Timestamp::now().as_second(),
            model_endpoint: self.config.scheduler.model_endpoint.clone(),
            model_name: self.config.scheduler.model_name.clone(),
            depth: 0,
            event: None,
            max_steps: Some(budget),
            subagent: true,
        };
        let config = self.config;
        let started = launcher.launch_cell("subagent", run, |token, socket| {
            child_config(config, capabilities, token, socket)
        });
        let run = match started {
            Ok(run) => run,
            Err(error) => {
                tracing::warn!(%error, "a subagent could not start");
                return Ok(SubagentResult::SubagentError);
            }
        };
        let child = run.token().to_owned();
        let brief_hash = sha256_hex(task.as_bytes());
        self.activity.emit(SubagentEvent::Spawned {
            child_id: child.clone(),
            parent_run_id: self.activity.run_id.clone(),
            task_brief_hash: brief_hash.clone(),
            budget_steps: u64::from(budget),
            capabilities: capabilities.iter().map(grant_label).collect(),
            depth,
        });
        self.children.insert(
            child.clone(),
            Child {
                run,
                budget,
                secrets,
            },
        );
        Ok(SubagentResult::Spawned {
            child,
            brief_hash,
            budget,
            capabilities: capabilities.to_vec(),
        })
    }

    fn wait(&mut self, child: &str) -> Result<SubagentResult, SubagentRefusal> {
        let mut entry = self
            .children
            .remove(child)
            .ok_or(SubagentRefusal::UnknownChild)?;
        // A child's run enforces its own time bound; past it and a grace,
        // the wait enforces it too, so no wait outlasts its child's bound.
        let deadline = Instant::now() + self.wait_bound;
        let end = loop {
            if let Some(end) = entry.run.poll_end() {
                break end;
            }
            if Instant::now() >= deadline {
                let _ = entry.run.kill();
                break RunEnd::TimedOut;
            }
            std::thread::sleep(WAIT_POLL);
        };
        let (steps_spent, outcome) = outcome_of(end, entry.budget, &entry.secrets);
        self.record(child, steps_spent, &outcome);
        Ok(SubagentResult::Report(ChildReport {
            child: child.to_owned(),
            steps_spent,
            outcome,
        }))
    }

    fn kill(&mut self, child: &str, reason: &str) -> Result<SubagentResult, SubagentRefusal> {
        if reason.len() > MAX_REASON_BYTES {
            return Err(SubagentRefusal::BadTask);
        }
        let entry = self
            .children
            .remove(child)
            .ok_or(SubagentRefusal::UnknownChild)?;
        Ok(SubagentResult::Report(self.killed(child, entry, reason)))
    }

    /// Kills `entry` and records it: its partial result survives it.
    fn killed(&mut self, child: &str, mut entry: Child<'scope>, reason: &str) -> ChildReport {
        let (partial, partial_hash) = partial_of(entry.run.kill(), &entry.secrets);
        let outcome = ChildOutcome::Killed {
            reason: reason.to_owned(),
            partial,
            partial_hash,
        };
        self.record(child, entry.budget, &outcome);
        ChildReport {
            child: child.to_owned(),
            steps_spent: entry.budget,
            outcome,
        }
    }

    /// Records how a child ended in the activity log, next to the brief
    /// hash its spawn recorded.
    fn record(&mut self, child: &str, steps_spent: u32, outcome: &ChildOutcome) {
        let child_id = child.to_owned();
        let steps_spent = u64::from(steps_spent);
        self.activity.emit(match outcome {
            ChildOutcome::Finished {
                verdict,
                result_hash,
                ..
            } => SubagentEvent::Finished {
                child_id,
                result_hash: result_hash.clone(),
                steps_spent,
                verdict: *verdict,
            },
            ChildOutcome::Killed {
                reason,
                partial_hash,
                ..
            } => SubagentEvent::Killed {
                child_id,
                reason: reason.clone(),
                partial_result_hash: partial_hash.clone(),
                steps_spent,
            },
            ChildOutcome::Failed { failure, detail } => SubagentEvent::Failed {
                child_id,
                failure: *failure,
                detail: detail.clone(),
                steps_spent,
            },
        });
    }
}

impl Drop for Subagents<'_, '_> {
    fn drop(&mut self) {
        // The orphan rule: no child outlives its parent.
        let children = std::mem::take(&mut self.children);
        for (child, entry) in children {
            let _ = self.killed(&child, entry, ORPHANED);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArtifactConfig, EgressConfig, GoalsConfig, MemoryConfig, SkillConfig, SpaceConfig,
        VaultConfig, VaultGrant,
    };
    use hatchling_core::{LogLevel, PeerUid, VaultAction};
    use std::path::PathBuf;

    fn config() -> Config {
        let root = PathBuf::from("/srv/hatchling/vault");
        Config {
            socket: "/run/hatchling/sock/broker.sock".parse().unwrap(),
            agent_uid: PeerUid::from_kernel(1000),
            socket_mode: 0o600,
            max_connections: 16,
            scheduler: SchedulerConfig {
                root: PathBuf::from("/srv/hatchling/schedules"),
                ..SchedulerConfig::disabled()
            },
            log_level: LogLevel::Error,
            vault: VaultConfig {
                key_file: VaultConfig::default_key_file(&root),
                root,
                grants: vec![VaultGrant {
                    action: VaultAction::Fetch,
                    name: "app-token".parse().unwrap(),
                }],
            },
            egress: EgressConfig {
                allowlist: crate::EgressAllowlist::new("api.example.com:443"),
                grants: vec![String::from("api.example.com:443")],
            },
            channels: vec![String::from("local")],
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: SkillConfig::disabled(),
            memory: MemoryConfig {
                root: PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: ArtifactConfig::disabled(),
            spaces: SpaceConfig::disabled(),
            goals: GoalsConfig::disabled(),
        }
    }

    fn vault(name: &str) -> ScheduleGrant {
        ScheduleGrant::Vault {
            action: String::from("fetch"),
            name: name.to_owned(),
        }
    }

    fn egress() -> ScheduleGrant {
        ScheduleGrant::Egress {
            destination: String::from("api.example.com:443"),
        }
    }

    #[test]
    fn the_ledger_only_fills_and_refuses_at_its_exact_bounds() {
        let mut ledger = Ledger::default();
        assert_eq!(ledger.spend(0), Err(SubagentRefusal::BadBudget));
        assert_eq!(
            ledger.spend(MAX_CHILD_STEPS + 1),
            Err(SubagentRefusal::BadBudget)
        );
        assert_eq!((ledger.steps(), ledger.children()), (0, 0));
        assert_eq!(ledger.spend(MAX_CHILD_STEPS), Ok(()));
        assert_eq!(ledger.spend(MAX_CHILD_STEPS), Ok(()));
        assert_eq!(ledger.steps(), MAX_LEDGER_STEPS);
        // Full: even one step is refused, and nothing comes back.
        assert_eq!(ledger.spend(1), Err(SubagentRefusal::LedgerSpent));
        assert_eq!((ledger.steps(), ledger.children()), (64, 2));
    }

    #[test]
    fn the_ledger_caps_children_however_small_their_budgets() {
        let mut ledger = Ledger::default();
        for _ in 0..MAX_CHILDREN {
            assert_eq!(ledger.spend(1), Ok(()));
        }
        assert_eq!(ledger.spend(1), Err(SubagentRefusal::LedgerSpent));
        assert_eq!((ledger.steps(), ledger.children()), (8, 8));
        // A budget that would overflow is spent, not wrapped.
        let mut near = Ledger {
            steps: u32::MAX,
            children: 0,
        };
        assert_eq!(near.spend(1), Err(SubagentRefusal::LedgerSpent));
    }

    #[test]
    fn a_spawn_is_checked_for_depth_brief_and_narrowing() {
        let parent = config();
        assert_eq!(check_spawn(&parent, "do it", &[vault("app-token")]), Ok(()));
        assert_eq!(check_spawn(&parent, "do it", &[]), Ok(()));
        // Widening: a capability the parent does not hold.
        assert_eq!(
            check_spawn(&parent, "do it", &[vault("other")]),
            Err(SubagentRefusal::ExceedsGrants)
        );
        assert_eq!(
            check_spawn(&parent, "do it", &[egress(), vault("other")]),
            Err(SubagentRefusal::ExceedsGrants)
        );
        // The brief.
        for task in ["", "   "] {
            assert_eq!(
                check_spawn(&parent, task, &[]),
                Err(SubagentRefusal::BadTask)
            );
        }
        let longest = "a".repeat(MAX_BRIEF_BYTES);
        assert_eq!(check_spawn(&parent, &longest, &[]), Ok(()));
        let over = "a".repeat(MAX_BRIEF_BYTES + 1);
        assert_eq!(
            check_spawn(&parent, &over, &[]),
            Err(SubagentRefusal::BadTask)
        );
        // A child cannot spawn: depth is checked first.
        let mut child = config();
        child.scheduler.subagent_depth = 1;
        assert_eq!(
            check_spawn(&child, "", &[vault("other")]),
            Err(SubagentRefusal::TooDeep)
        );
    }

    #[test]
    fn a_childs_session_holds_its_capabilities_and_its_depth_only() {
        let parent = config();
        let socket: SocketPath = "/tmp/hatchling-child/broker.sock".parse().unwrap();
        let child = child_config(&parent, &[vault("app-token")], "run-1", socket.clone());
        assert_eq!(held_grants(&child), [vault("app-token")].into());
        assert_eq!(child.socket, socket);
        assert_eq!(child.scheduler.subagent_depth, 1);
        assert!(child.scheduler.followup.is_none());
        assert!(!child.scheduler.enabled());
        assert!(child.egress.grants.is_empty());
        assert!(child.channels.is_empty());
        let none = child_config(&parent, &[], "run-2", socket);
        assert!(held_grants(&none).is_empty());
        assert_eq!(
            check_spawn(&child, "grandchild", &[]),
            Err(SubagentRefusal::TooDeep)
        );
    }

    fn secrets() -> SecretSet {
        SecretSet::new([b"s3cr3t-value".to_vec()])
    }

    fn exited(code: i32, output: &str) -> RunEnd {
        RunEnd::Exited {
            code,
            output: output.as_bytes().to_vec(),
            errors: b"stderr".to_vec(),
        }
    }

    fn finished(answer: &str, output: &str) -> ChildOutcome {
        ChildOutcome::Finished {
            verdict: ChildVerdict::Ok,
            answer: answer.to_owned(),
            result_hash: sha256_hex(output.as_bytes()),
        }
    }

    #[test]
    fn a_childs_envelope_is_its_report() {
        let done = r#"{"status":"done","verdict":"partial","answer":"half","steps":3}"#;
        assert_eq!(
            outcome_of(exited(0, done), 8, &secrets()),
            (
                3,
                ChildOutcome::Finished {
                    verdict: ChildVerdict::Partial,
                    answer: String::from("half"),
                    result_hash: sha256_hex(done.as_bytes()),
                }
            )
        );
        // A child cannot claim more steps than it was given.
        let greedy = r#"{"status":"done","verdict":"ok","answer":"a","steps":99}"#;
        assert_eq!(outcome_of(exited(0, greedy), 8, &secrets()).0, 8);
        let failed = r#"{"status":"failed","failure":"budget_exhausted","steps":8}"#;
        assert_eq!(
            outcome_of(exited(0, failed), 8, &secrets()),
            (
                8,
                ChildOutcome::Failed {
                    failure: ChildFailure::BudgetExhausted,
                    detail: String::from("the child ended without an answer"),
                }
            )
        );
        let fewer = r#"{"status":"failed","failure":"budget_exhausted","steps":2}"#;
        assert_eq!(outcome_of(exited(0, fewer), 8, &secrets()).0, 2);
    }

    #[test]
    fn plain_output_is_the_answer_at_the_whole_budget() {
        assert_eq!(
            outcome_of(exited(0, "  plain answer \n"), 5, &secrets()),
            (5, finished("plain answer", "  plain answer \n"))
        );
    }

    #[test]
    fn an_answer_that_would_expose_a_secret_is_withheld_whole() {
        let leaky =
            r#"{"status":"done","verdict":"ok","answer":"the key is s3cr3t-value","steps":1}"#;
        assert_eq!(
            outcome_of(exited(0, leaky), 4, &secrets()),
            (1, finished(WITHHELD, leaky))
        );
        assert_eq!(
            outcome_of(exited(0, "raw s3cr3t-value"), 4, &secrets()),
            (4, finished(WITHHELD, "raw s3cr3t-value"))
        );
        assert_eq!(
            WITHHELD,
            "[withheld: the child's answer would expose a secret its grants reach]"
        );
    }

    #[test]
    fn a_child_that_does_not_end_cleanly_is_a_typed_failure() {
        let failure = |end: RunEnd| outcome_of(end, 6, &secrets());
        assert_eq!(
            failure(exited(3, "anything")),
            (
                6,
                ChildOutcome::Failed {
                    failure: ChildFailure::CellFailed,
                    detail: String::from("the child's cell exited 3"),
                }
            )
        );
        assert_eq!(
            failure(RunEnd::TimedOut),
            (
                6,
                ChildOutcome::Failed {
                    failure: ChildFailure::Timeout,
                    detail: String::from("the child ran past its time bound"),
                }
            )
        );
        assert_eq!(
            failure(RunEnd::Failed(String::from("x"))),
            (
                6,
                ChildOutcome::Failed {
                    failure: ChildFailure::CellFailed,
                    detail: String::from("the child's cell could not be waited on"),
                }
            )
        );
        let unreadable = RunEnd::Exited {
            code: 0,
            output: vec![0xff, 0xfe],
            errors: Vec::new(),
        };
        assert_eq!(
            failure(unreadable),
            (
                6,
                ChildOutcome::Failed {
                    failure: ChildFailure::CellFailed,
                    detail: String::from("the child's report is unreadable"),
                }
            )
        );
    }

    #[test]
    fn a_killed_childs_last_words_survive_it() {
        assert_eq!(partial_of(Vec::new(), &secrets()), (None, None));
        assert_eq!(
            partial_of(b"halfway\n".to_vec(), &secrets()),
            (
                Some(String::from("halfway")),
                Some(sha256_hex(b"halfway\n"))
            )
        );
        // Unreadable or blank, it still leaves its hash.
        assert_eq!(
            partial_of(vec![0xff], &secrets()),
            (None, Some(sha256_hex(&[0xff])))
        );
        assert_eq!(
            partial_of(b"  \n".to_vec(), &secrets()),
            (None, Some(sha256_hex(b"  \n")))
        );
        assert_eq!(
            partial_of(b"s3cr3t-value".to_vec(), &secrets()),
            (
                Some(String::from(WITHHELD)),
                Some(sha256_hex(b"s3cr3t-value"))
            )
        );
        assert_eq!(ORPHANED, "the parent's run ended");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hatchling-subagent-unit-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_session_without_runs_refuses_to_spawn_and_bounds_its_ops() {
        let parent = config();
        let key = MasterKey::generate().unwrap();
        std::thread::scope(|scope| {
            let mut subagents = Subagents::new(scope, &parent, &key, None, String::from("run"));
            let spawn = |subagents: &mut Subagents<'_, '_>, capabilities: Vec<ScheduleGrant>| {
                subagents.handle(&SubagentOp::Spawn {
                    task: String::from("do it"),
                    budget: 2,
                    capabilities,
                })
            };
            let refused = |reason| SubagentResult::Refused { reason };
            // Checks come first; then a session with no runs cannot start one.
            assert_eq!(
                spawn(&mut subagents, vec![vault("other")]),
                refused(SubagentRefusal::ExceedsGrants)
            );
            assert_eq!(
                spawn(&mut subagents, Vec::new()),
                refused(SubagentRefusal::Unavailable)
            );
            assert_eq!(subagents.ledger(), Ledger::default());
            // A kill's reason is bounded, before the child is looked up.
            let kill = |subagents: &mut Subagents<'_, '_>, reason: String| {
                subagents.handle(&SubagentOp::Kill {
                    child: String::from("run-x"),
                    reason,
                })
            };
            assert_eq!(
                kill(&mut subagents, "r".repeat(MAX_REASON_BYTES)),
                refused(SubagentRefusal::UnknownChild)
            );
            assert_eq!(
                kill(&mut subagents, "r".repeat(MAX_REASON_BYTES + 1)),
                refused(SubagentRefusal::BadTask)
            );
            assert_eq!(
                subagents.handle(&SubagentOp::Wait {
                    child: String::from("run-x"),
                }),
                refused(SubagentRefusal::UnknownChild)
            );
            let shown = format!("{subagents:?}");
            assert!(
                shown.starts_with(
                    "Subagents { ledger: Ledger { steps: 0, children: 0 }, children: []"
                )
            );
        });
    }

    #[test]
    fn a_spawn_whose_cell_cannot_start_has_still_spent_its_budget() {
        let base = scratch("spent");
        let mut parent = config();
        parent.vault.root = base.join("vault");
        parent.vault.key_file = VaultConfig::default_key_file(&parent.vault.root);
        let key = MasterKey::generate().unwrap();
        // The vault opens, so the child's secrets are known; the cell
        // does not start, because the run helper is not there.
        let _ = hatchling_vault::SealedVault::open(&parent.vault.root, &key).unwrap();
        let env = RunEnv {
            spawner: base.join("no-such-broker"),
            agent_bin: base.join("no-such-agent"),
            run_root: base.join("runs"),
            timeout: Duration::from_secs(5),
        };
        std::thread::scope(|scope| {
            let mut subagents =
                Subagents::new(scope, &parent, &key, Some(env), String::from("run"));
            assert_eq!(
                subagents.handle(&SubagentOp::Spawn {
                    task: String::from("do it"),
                    budget: 3,
                    capabilities: vec![vault("app-token")],
                }),
                SubagentResult::SubagentError
            );
            // Allocation is spend: the failed start is not refunded.
            assert_eq!(
                (subagents.ledger().steps(), subagents.ledger().children()),
                (3, 1)
            );
        });
        // A vault that cannot be opened stops a spawn before any spend.
        let mut locked = config();
        locked.vault.root = base.join("missing").join("vault");
        let env = RunEnv {
            spawner: base.join("no-such-broker"),
            agent_bin: base.join("no-such-agent"),
            run_root: base.join("runs"),
            timeout: Duration::from_secs(5),
        };
        let other_key = MasterKey::generate().unwrap();
        std::thread::scope(|scope| {
            let mut subagents =
                Subagents::new(scope, &locked, &other_key, Some(env), String::from("run"));
            let _ = std::fs::write(base.join("missing"), b"not a directory");
            assert_eq!(
                subagents.handle(&SubagentOp::Spawn {
                    task: String::from("do it"),
                    budget: 3,
                    capabilities: Vec::new(),
                }),
                SubagentResult::SubagentError
            );
            assert_eq!(subagents.ledger(), Ledger::default());
        });
        let _ = std::fs::remove_dir_all(&base);
    }
}
