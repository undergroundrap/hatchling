//! Component: the artifact pipeline end to end. Builds
//! run through the real skill gate, in real skill cells, from the real
//! signed fixture builder and the real document and page builders;
//! provenance, with the inspector's report, lands in the real sealed
//! store; deliveries go through the one delivery function to the real hub
//! or a workspace file. Ideas are captured, refined, and built through
//! the same pipeline.
#![allow(
    clippy::unwrap_used,
    reason = "fixtures: a failed setup must fail the calling test loudly"
)]

#[allow(
    dead_code,
    clippy::redundant_pub_crate,
    reason = "shared test helpers: this target uses a subset"
)]
mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::{
    DOCUMENT_BUILDER, FIXTURE_BUILDER, FIXTURE_SIGNING_KEY, FIXTURE_SKILL, PAGE_BUILDER, TestDir,
    artifact_skills, guard_cell_available, test_dir,
};
use hatchling_artifact::{
    BuildRecord, BuildStatus, KindProfile, Kinds, MEDIA_MANIFEST_TYPE, MediaProfile, PinnedRequest,
    TextProfile, Verdict, attribution, finding, sha256_hex,
};
use hatchling_broker::activation::CONFIRMATION_TTL_SECS;
use hatchling_broker::{
    ArtifactConfig, Config, ConfirmError, EgressConfig, GoalsAction, MemoryAction, MemoryConfig,
    MemoryGrant, SchedulerConfig, SealedArtifactStore, Skills, VaultConfig, VaultGrant,
    confirm_activation, handle_artifact_op, serve_connection_with_ingress,
};
use hatchling_channel::{LocalHub, Message};
use hatchling_core::framing::{read_frame, write_frame};
use hatchling_core::{
    ActivationProposal, AgentMessage, ArtifactInput, ArtifactInspection, ArtifactOp,
    ArtifactResult, ArtifactView, BrokerMessage, BuildFailure, BuildState, DeliveryRefusal,
    DeliveryTarget, IdeaRef, IdeaRefusal, IdeaSource, IdeaState, IdeaSummary, InspectionFinding,
    InspectionVerdict, LogLevel, PROTOCOL_VERSION, PeerUid, SessionRequest, SessionResponse,
    VaultAction,
};
use hatchling_goals::GoalsStore;
use hatchling_memory::{MemoryPrincipal, MemoryScope, MemoryStore, RetentionPolicy, SessionId};
use hatchling_vault::{MasterKey, SealedVault, Secret};

/// The vault secret a lying builder embeds.
const SECRET: &str = "sk-live-0123456789abcdef";

/// One master key for the process, as the broker has one.
fn key() -> &'static MasterKey {
    static KEY: OnceLock<MasterKey> = OnceLock::new();
    KEY.get_or_init(|| MasterKey::generate().unwrap())
}

/// A test's world: its scratch base, its skills, and its config.
struct World {
    base: TestDir,
    skills: Skills,
    config: Config,
}

impl World {
    /// The base, the skills, and a config with the build store, a
    /// workspace, a review policy, and the vault secret `app-token`.
    fn new(name: &str) -> Self {
        let base = test_dir(&format!("artifact-{name}"));
        std::fs::create_dir_all(&*base).unwrap();
        let skills = artifact_skills(&base, Duration::from_secs(20)).unwrap();
        let vault_root = base.join("vault");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let vault = SealedVault::open(&vault_root, key()).unwrap();
        vault
            .store(
                &"app-token".parse().unwrap(),
                &Secret::new(SECRET.as_bytes().to_vec()).unwrap(),
            )
            .unwrap();
        let config = Config {
            socket: "/tmp/hatchling-test-artifacts-dummy.sock".parse().unwrap(),
            agent_uid: PeerUid::from_kernel(rustix::process::getuid().as_raw()),
            socket_mode: 0o600,
            max_connections: hatchling_broker::DEFAULT_MAX_CONNECTIONS,
            scheduler: SchedulerConfig::disabled(),
            log_level: LogLevel::Error,
            vault: VaultConfig {
                key_file: VaultConfig::default_key_file(&vault_root),
                root: vault_root,
                grants: vec![VaultGrant {
                    action: VaultAction::Fetch,
                    name: "app-token".parse().unwrap(),
                }],
            },
            egress: EgressConfig {
                allowlist: hatchling_broker::EgressAllowlist::default(),
                grants: Vec::new(),
            },
            channels: vec![String::from("local")],
            channel_accounts: Vec::new(),
            channel_ingress: None,
            skills: hatchling_broker::SkillConfig::disabled(),
            memory: MemoryConfig {
                root: PathBuf::new(),
                grants: Vec::new(),
            },
            artifacts: ArtifactConfig {
                root: base.join("artifacts"),
                workspace: Some(workspace),
                policy: Some(hatchling_artifact::ReviewPolicy {
                    max_bytes: 4096,
                    deny: Vec::new(),
                }),
                kinds: test_kinds(),
            },
            goals: hatchling_broker::GoalsConfig::disabled(),
            spaces: hatchling_broker::SpaceConfig::disabled(),
        };
        Self {
            base,
            skills,
            config,
        }
    }

    fn op(&self, hub: Option<&LocalHub>, op: &ArtifactOp) -> ArtifactResult {
        handle_artifact_op(
            &self.config,
            key(),
            &self.skills,
            hub,
            &principal(),
            op,
            NOW,
        )
    }

    fn build(&self, kind: &str, builder: &str, inputs: &[(&str, &str)]) -> ArtifactResult {
        self.op(
            None,
            &ArtifactOp::Build {
                build_kind: kind.to_owned(),
                builder: builder.to_owned(),
                inputs: inputs
                    .iter()
                    .map(|(name, content)| ArtifactInput {
                        name: (*name).to_owned(),
                        content: (*content).to_owned(),
                    })
                    .collect(),
            },
        )
    }

    fn deliver(&self, hub: Option<&LocalHub>, build: &str, to: DeliveryTarget) -> ArtifactResult {
        self.op(
            hub,
            &ArtifactOp::Deliver {
                build: build.to_owned(),
                to,
            },
        )
    }

    fn store(&self) -> SealedArtifactStore<'_> {
        SealedArtifactStore::new(&self.config.artifacts.root, key())
    }

    fn record(&self, build: &str) -> BuildRecord {
        self.store().get(build).unwrap().unwrap()
    }
}

/// The standard kinds, plus the plain kinds the fixture builder's modes
/// are named for.
fn test_kinds() -> Kinds {
    ["echo", "forge", "fail"]
        .into_iter()
        .fold(Kinds::standard(), |kinds, kind| {
            kinds.with(TextProfile::new(kind, "text/plain"))
        })
}

fn local() -> DeliveryTarget {
    DeliveryTarget::Channel {
        channel: String::from("local"),
    }
}

fn workspace(name: &str) -> DeliveryTarget {
    DeliveryTarget::Workspace {
        name: name.to_owned(),
    }
}

/// The build id from a `Built` reply, or a description of what came
/// back (which the caller's assertions then reject).
fn built(result: &ArtifactResult) -> String {
    match result {
        ArtifactResult::Built { build, .. } => build.clone(),
        other @ (ArtifactResult::BuildFailed { .. }
        | ArtifactResult::Delivered { .. }
        | ArtifactResult::Refused { .. }
        | ArtifactResult::Described { .. }
        | ArtifactResult::UnknownBuild
        | ArtifactResult::IdeaSaved { .. }
        | ArtifactResult::Ideas { .. }
        | ArtifactResult::ActivationProposed { .. }
        | ArtifactResult::IdeaRefused { .. }
        | ArtifactResult::Denied
        | ArtifactResult::ArtifactError) => format!("not built: {other:?}"),
    }
}

/// A hub with a client attached, recording what it receives.
fn hub_with_client() -> (LocalHub, Arc<Mutex<Vec<Message>>>) {
    let hub = LocalHub::new(hatchling_channel::OutputGate::disabled());
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let _ = hub.attach(Box::new(move |m: &hatchling_channel::Cleared| {
        sink.lock().unwrap().push(m.message().clone());
        Ok(())
    }));
    (hub, received)
}

fn signer_fingerprint() -> String {
    let verifying = ed25519_dalek::SigningKey::from_bytes(&FIXTURE_SIGNING_KEY)
        .verifying_key()
        .to_bytes();
    sha256_hex(&verifying)
}

#[test]
fn a_build_runs_through_the_skill_gate_and_delivers_with_broker_provenance() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("e2e");
    let (hub, received) = hub_with_client();
    // Through a real broker session, as the agent would ask.
    let (client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let ask = |request: &SessionRequest| -> ArtifactResult {
        write_frame(&client, &serde_json::to_vec(request).unwrap()).unwrap();
        let reply: SessionResponse = serde_json::from_slice(&read_frame(&client).unwrap()).unwrap();
        let SessionResponse::Artifact(result) = reply else {
            return ArtifactResult::ArtifactError;
        };
        result
    };
    std::thread::scope(|scope| {
        // However the test leaves this scope, the client closes first, so
        // the session thread ends and the scope's join cannot hang.
        let _close = Close(&client);
        scope.spawn(|| {
            serve_connection_with_ingress(&server, &world.config, key(), &world.skills, Some(&hub))
        });
        let hello = AgentMessage::Hello {
            v: PROTOCOL_VERSION,
            cell_id: String::from("cell-artifacts"),
        };
        write_frame(&client, &serde_json::to_vec(&hello).unwrap()).unwrap();
        let welcome: BrokerMessage = serde_json::from_slice(&read_frame(&client).unwrap()).unwrap();
        assert!(
            matches!(welcome, BrokerMessage::Welcome { .. }),
            "{welcome:?}"
        );
        let inputs = vec![
            ArtifactInput {
                name: String::from("title"),
                content: String::from("Weekly report"),
            },
            ArtifactInput {
                name: String::from("body"),
                content: String::from("All quiet."),
            },
        ];
        let reply = ask(&SessionRequest::Artifact(ArtifactOp::Build {
            build_kind: String::from("echo"),
            builder: String::from(FIXTURE_BUILDER),
            inputs: inputs.clone(),
        }));
        let expected = "title: Weekly report\nbody: All quiet.";
        let ArtifactResult::Built {
            build,
            output_hash,
            builder,
            signer,
            attribution: shown,
            inspection,
        } = reply
        else {
            panic_free_fail(&format!("{reply:?}"));
            return;
        };
        assert!(build.starts_with("art-"), "{build}");
        assert_eq!(output_hash, sha256_hex(expected.as_bytes()));
        assert_eq!(builder, FIXTURE_BUILDER);
        // The signing identity, not the name.
        assert_eq!(signer, signer_fingerprint());
        assert_eq!(
            shown,
            format!(
                "built by verified builder builder@1.0.0 (signer {})",
                signer_fingerprint().chars().take(16).collect::<String>()
            )
        );
        assert_eq!(inspection.verdict, InspectionVerdict::Pass);
        assert_eq!(inspection.content_type, "text/plain");
        let delivered = ask(&SessionRequest::Artifact(ArtifactOp::Deliver {
            build: build.clone(),
            to: local(),
        }));
        assert_eq!(
            delivered,
            ArtifactResult::Delivered {
                inspection: inspection.clone()
            }
        );
        client.shutdown(std::net::Shutdown::Both).unwrap();
        check_e2e_record(&world.record(&build), &inputs, &inspection, &shown);
    });
    let received = received.lock().unwrap().clone();
    let message = received.first().unwrap();
    assert_eq!(message.text, "title: Weekly report\nbody: All quiet.");
    assert_eq!(message.sender.0, "broker:artifact");
}

/// The end-to-end build's record: broker-asserted provenance, with the
/// inspection and attribution the replies carried, and the gate's verdict.
fn check_e2e_record(
    record: &BuildRecord,
    inputs: &[ArtifactInput],
    inspection: &ArtifactInspection,
    shown: &str,
) {
    assert_eq!(record.status, BuildStatus::Completed);
    let provenance = record.provenance.clone().unwrap();
    assert_eq!(
        provenance.inputs_hash,
        PinnedRequest::pin("echo", inputs).unwrap().inputs_hash()
    );
    assert_eq!(provenance.builder.skill, FIXTURE_BUILDER);
    assert_eq!(provenance.builder.signer, signer_fingerprint());
    assert_eq!(
        provenance.output_hash,
        sha256_hex(b"title: Weekly report\nbody: All quiet.")
    );
    assert_eq!(provenance.summary_claim.as_deref(), Some("clean"));
    assert!(provenance.deterministic_claim);
    assert_eq!(&provenance.inspection, inspection);
    assert_eq!(provenance.idea, None);
    assert_eq!(
        attribution(Some(&provenance.builder), Some(&signer_fingerprint())),
        shown
    );
    let delivery = record.deliveries.first().unwrap();
    assert_eq!(delivery.target, "channel:local");
    assert_eq!(delivery.verdict, Some(Verdict::Allow));
    assert!(delivery.delivered());
}

/// Shuts a session's client down when dropped.
struct Close<'a>(&'a UnixStream);

impl Drop for Close<'_> {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

/// Fails the calling test with `what`, without a `panic!`.
fn panic_free_fail(what: &str) {
    assert!(what.is_empty(), "{what}");
}

#[test]
fn identical_pinned_inputs_rebuild_byte_identical() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("repro");
    let inputs = [("body", "the same input"), ("note", "and another")];
    let first = built(&world.build("echo", FIXTURE_BUILDER, &inputs));
    let second = built(&world.build("echo", FIXTURE_BUILDER, &inputs));
    assert_ne!(first, second, "two builds, two records");
    let (a, b) = (world.record(&first), world.record(&second));
    assert_eq!(a.content, b.content, "byte-identical content");
    let (pa, pb) = (a.provenance.unwrap(), b.provenance.unwrap());
    assert_eq!(pa.output_hash, pb.output_hash);
    assert_eq!(pa.inputs_hash, pb.inputs_hash);
    // Different inputs, different bytes.
    let third = built(&world.build("echo", FIXTURE_BUILDER, &[("body", "a different input")]));
    assert_ne!(
        world.record(&third).provenance.unwrap().output_hash,
        pa.output_hash
    );
}

#[test]
fn an_incomplete_build_is_never_delivered() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("incomplete");
    let (hub, received) = hub_with_client();
    // A pending build: requested, never finished.
    let pending = BuildRecord::pending(
        String::from("art-pending"),
        PinnedRequest::pin("echo", &[]).unwrap(),
    );
    assert!(world.store().insert(&pending).unwrap());
    assert_eq!(
        world.deliver(Some(&hub), "art-pending", local()),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::Incomplete
        }
    );
    // A failed build.
    assert_eq!(
        world.build("fail", FIXTURE_BUILDER, &[]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::BuilderFailed
        }
    );
    assert!(received.lock().unwrap().is_empty());
    assert_eq!(
        world.deliver(Some(&hub), "art-nonexistent", local()),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::UnknownBuild
        }
    );
    let record = world.record("art-pending");
    assert_eq!(
        record.deliveries.first().and_then(|d| d.refusal),
        Some(DeliveryRefusal::Incomplete)
    );
}

#[test]
fn the_gate_blocks_a_builder_that_embeds_a_vault_secret_and_calls_it_clean() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("lying");
    let (hub, received) = hub_with_client();
    let carried = format!("totally harmless, honest: {SECRET}");
    let build = built(&world.build("echo", FIXTURE_BUILDER, &[("body", &carried)]));
    let record = world.record(&build);
    // The builder said "clean"; the bytes say otherwise.
    assert_eq!(
        record
            .provenance
            .as_ref()
            .and_then(|p| p.summary_claim.as_deref()),
        Some("clean")
    );
    for to in [local(), workspace("leak.md")] {
        assert_eq!(
            world.deliver(Some(&hub), &build, to),
            ArtifactResult::Refused {
                reason: DeliveryRefusal::Blocked
            }
        );
    }
    assert!(
        received.lock().unwrap().is_empty(),
        "nothing reached the channel"
    );
    assert!(
        !world.base.join("workspace/leak.md").exists(),
        "nothing reached the workspace"
    );
    let deliveries = world.record(&build).deliveries;
    assert_eq!(deliveries.len(), 2);
    assert!(
        deliveries
            .iter()
            .all(|d| matches!(d.verdict, Some(Verdict::Block { .. })) && !d.delivered())
    );
}

#[test]
fn a_gate_that_cannot_judge_blocks() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let mut world = World::new("gate-error");
    let (hub, received) = hub_with_client();
    let build = built(&world.build("echo", FIXTURE_BUILDER, &[("body", "clean text")]));
    // No review policy: the gate cannot judge, so it blocks.
    let policy = world.config.artifacts.policy.take();
    assert_eq!(
        world.deliver(Some(&hub), &build, local()),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::GateError
        }
    );
    // A vault the gate cannot read: the held secrets are unknown, so it
    // blocks.
    world.config.artifacts.policy = policy;
    let unreadable = world.base.join("not-a-vault");
    std::fs::write(&unreadable, b"a file, not a directory").unwrap();
    world.config.vault.root = unreadable;
    assert_eq!(
        world.deliver(Some(&hub), &build, local()),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::GateError
        }
    );
    assert!(received.lock().unwrap().is_empty());
    let deliveries = world.record(&build).deliveries;
    assert!(
        deliveries
            .iter()
            .all(|d| matches!(d.verdict, Some(Verdict::Error { .. })))
    );
}

#[test]
fn bytes_changed_after_completion_are_refused() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("toctou");
    let (hub, received) = hub_with_client();
    let build = built(&world.build("echo", FIXTURE_BUILDER, &[("body", "reviewed text")]));
    let mut record = world.record(&build);
    // The stored bytes change after the hash was recorded.
    record.content = Some(format!("reviewed text, then {SECRET}"));
    assert!(world.store().put(&record).unwrap());
    assert_eq!(
        world.deliver(Some(&hub), &build, local()),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::HashMismatch
        }
    );
    assert!(received.lock().unwrap().is_empty());
    assert_eq!(
        world
            .record(&build)
            .deliveries
            .first()
            .and_then(|d| d.verdict.clone()),
        None
    );
}

#[test]
fn a_builder_without_the_build_capability_is_denied_by_the_skill_gate() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("undeclared");
    // A signed, admitted skill, but one that never declared artifact.build.
    assert_eq!(
        world.build("echo", FIXTURE_SKILL, &[("body", "x")]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::CapabilityDenied
        }
    );
    assert_eq!(
        world.build("echo", "nobody@1.0.0", &[]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::UnknownBuilder
        }
    );
    assert_eq!(
        world.build("echo", "not an id", &[]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::UnknownBuilder
        }
    );
    // A malformed request never reaches the gate either.
    assert_eq!(
        world.build("Echo", FIXTURE_BUILDER, &[]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::BadRequest
        }
    );
    // Refused before anything was recorded.
    assert!(world.store().get("art-anything").unwrap().is_none());
    assert!(!world.config.artifacts.root.join("build-index").exists());
}

#[test]
fn a_builder_that_writes_its_own_provenance_is_rejected() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("forge");
    let (hub, received) = hub_with_client();
    assert_eq!(
        world.build("forge", FIXTURE_BUILDER, &[("body", "x")]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::BadOutput
        }
    );
    // Its record is a failure: nothing to deliver.
    let index_entry = world.config.artifacts.root.join("build-index");
    assert!(index_entry.exists(), "the attempt is on record");
    assert!(received.lock().unwrap().is_empty());
    drop(hub);
}

#[test]
fn workspace_delivery_writes_a_new_owner_only_file_and_bad_targets_are_refused() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let mut world = World::new("workspace");
    let build = built(&world.build("echo", FIXTURE_BUILDER, &[("body", "to the workspace")]));
    assert!(matches!(
        world.deliver(None, &build, workspace("report.md")),
        ArtifactResult::Delivered { .. }
    ));
    let file = world.base.join("workspace/report.md");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "body: to the workspace"
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Never replaces a file.
    assert_eq!(
        world.deliver(None, &build, workspace("report.md")),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::DeliveryFailed
        }
    );
    for to in [
        workspace("../escape.md"),
        workspace(".hidden"),
        DeliveryTarget::Channel {
            channel: String::from("telegram"),
        },
        // The local channel with no ingress client hub for this session.
        local(),
    ] {
        assert_eq!(
            world.deliver(None, &build, to.clone()),
            ArtifactResult::Refused {
                reason: DeliveryRefusal::BadTarget
            },
            "{to:?}"
        );
    }
    // A hub is there, but only the local channel reaches it, and only for
    // a session that has the local channel.
    let (hub, received) = hub_with_client();
    let telegram = DeliveryTarget::Channel {
        channel: String::from("telegram"),
    };
    assert_eq!(
        world.deliver(Some(&hub), &build, telegram),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::BadTarget
        }
    );
    let channels = std::mem::take(&mut world.config.channels);
    assert_eq!(
        world.deliver(Some(&hub), &build, local()),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::BadTarget
        }
    );
    world.config.channels = channels;
    assert!(received.lock().unwrap().is_empty());
    world.config.artifacts.workspace = None;
    assert_eq!(
        world.deliver(None, &build, workspace("other.md")),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::BadTarget
        }
    );
    assert!(!world.base.join("escape.md").exists());
    // Every attempt is on record, under the target it named.
    let targets: Vec<(String, Option<DeliveryRefusal>)> = world
        .record(&build)
        .deliveries
        .into_iter()
        .map(|d| (d.target, d.refusal))
        .collect();
    let bad = Some(DeliveryRefusal::BadTarget);
    assert_eq!(
        targets,
        [
            (String::from("workspace:report.md"), None),
            (
                String::from("workspace:report.md"),
                Some(DeliveryRefusal::DeliveryFailed)
            ),
            (String::from("workspace:../escape.md"), bad),
            (String::from("workspace:.hidden"), bad),
            (String::from("channel:telegram"), bad),
            (String::from("channel:local"), bad),
            (String::from("channel:telegram"), bad),
            (String::from("channel:local"), bad),
            (String::from("workspace:other.md"), bad),
        ]
    );
}

#[test]
fn artifacts_are_denied_without_a_store_and_in_a_run_session() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let mut world = World::new("off");
    let run_request = hatchling_scheduler::RunRequest {
        schedule_id: String::from("sch-1"),
        task: String::from("t"),
        grants: Vec::new(),
        due: hatchling_scheduler::Timestamp::from_second(1_790_000_000).unwrap(),
        missed: false,
        channel: String::from("local"),
        depth: 0,
        event: None,
        space: None,
        feed: None,
        chat: false,
    };
    let run = hatchling_broker::scoped_config(
        &world.config,
        &run_request,
        "run-x",
        "/tmp/hatchling-test-artifacts-run.sock".parse().unwrap(),
    );
    assert!(!run.artifacts.enabled(), "a run cannot build or deliver");
    world.config.artifacts = ArtifactConfig::disabled();
    assert_eq!(
        world.build("echo", FIXTURE_BUILDER, &[]),
        ArtifactResult::Denied
    );
    assert_eq!(
        world.deliver(None, "art-1", local()),
        ArtifactResult::Denied
    );
    world.config = run;
    assert_eq!(
        world.build("echo", FIXTURE_BUILDER, &[]),
        ArtifactResult::Denied
    );
}

// --- real builders, the inspector, media kinds, identity, ideas.

impl World {
    fn describe(&self, build: &str) -> ArtifactResult {
        self.op(
            None,
            &ArtifactOp::Describe {
                build: build.to_owned(),
            },
        )
    }

    /// The view `Describe` gives of `build`, or a view that names what
    /// came back instead (which the caller's assertions then reject).
    fn view(&self, build: &str) -> ArtifactView {
        let result = self.describe(build);
        if let ArtifactResult::Described { view } = result {
            view
        } else {
            ArtifactView {
                build: format!("not described: {result:?}"),
                kind: String::new(),
                state: BuildState::Failed,
                attribution: None,
                inspection: None,
                output_hash: None,
                idea: None,
                delivered: 0,
            }
        }
    }

    fn content(&self, build: &str) -> String {
        self.record(build).content.unwrap()
    }
}

/// The inspection a `Built` reply carried, if it is one.
fn inspection_of(result: &ArtifactResult) -> Option<ArtifactInspection> {
    if let ArtifactResult::Built { inspection, .. } = result {
        Some(inspection.clone())
    } else {
        None
    }
}

/// The checks an inspection's findings name, in order.
fn checks(inspection: &ArtifactInspection) -> Vec<String> {
    inspection
        .findings
        .iter()
        .map(|finding| finding.check.clone())
        .collect()
}

fn verified(builder: &str) -> String {
    format!(
        "built by verified builder {builder} (signer {})",
        signer_fingerprint().chars().take(16).collect::<String>()
    )
}

#[test]
fn the_document_and_page_builders_build_end_to_end_with_broker_provenance() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("real-builders");
    let inputs = [
        ("title", "Weekly report"),
        ("body", "All quiet."),
        ("risks", "None."),
    ];
    let reply = world.build("document", DOCUMENT_BUILDER, &inputs);
    let ArtifactResult::Built {
        build,
        builder,
        signer,
        attribution: shown,
        inspection,
        ..
    } = reply.clone()
    else {
        panic_free_fail(&format!("{reply:?}"));
        return;
    };
    assert_eq!(
        (builder.as_str(), signer),
        (DOCUMENT_BUILDER, signer_fingerprint())
    );
    assert_eq!(shown, verified(DOCUMENT_BUILDER));
    assert_eq!(
        inspection.verdict,
        InspectionVerdict::Pass,
        "{inspection:?}"
    );
    assert_eq!(inspection.content_type, "text/markdown");
    let document = "# Weekly report\n\nAll quiet.\n\n## risks\n\nNone.\n";
    assert_eq!(world.content(&build), document);
    let provenance = world.record(&build).provenance.unwrap();
    assert_eq!(provenance.builder.skill, DOCUMENT_BUILDER);
    assert_eq!(provenance.output_hash, sha256_hex(document.as_bytes()));
    assert_eq!(
        provenance.summary_claim.as_deref(),
        Some("a document with 2 sections")
    );
    assert!(provenance.deterministic_claim);
    // Deterministic, as claimed: the same pinned inputs, the same bytes.
    let again = built(&world.build("document", DOCUMENT_BUILDER, &inputs));
    assert_eq!(world.content(&again), document);
    // The page builder escapes everything it is given.
    let page_inputs = [
        ("title", "Notes <b>"),
        ("body", "<script>alert(1)</script>\n\n- one"),
    ];
    let page_reply = world.build("page", PAGE_BUILDER, &page_inputs);
    let page = built(&page_reply);
    let page_inspection = inspection_of(&page_reply).unwrap();
    assert_eq!(
        page_inspection.verdict,
        InspectionVerdict::Pass,
        "{page_inspection:?}"
    );
    assert_eq!(page_inspection.content_type, "text/html");
    let html = world.content(&page);
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(html.contains("<title>Notes &lt;b&gt;</title>"));
    assert!(
        html.contains("<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>\n<ul>\n<li>one</li>\n</ul>")
    );
    assert!(!html.contains("<script"));
    // Delivered as built, to a workspace file.
    assert_eq!(
        world.deliver(None, &page, workspace("notes.html")),
        ArtifactResult::Delivered {
            inspection: page_inspection
        }
    );
    assert_eq!(
        std::fs::read_to_string(world.base.join("workspace/notes.html")).unwrap(),
        html
    );
    // Each builder builds only its own kind: asked for another, it fails
    // in its cell, and nothing is built.
    assert_eq!(
        world.build("page", DOCUMENT_BUILDER, &inputs),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::BuilderFailed
        }
    );
    // And it needs a title.
    assert_eq!(
        world.build("document", DOCUMENT_BUILDER, &[("body", "\n")]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::BuilderFailed
        }
    );
}

#[test]
fn the_inspector_catches_malformed_output_and_records_it_in_provenance() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("inspector");
    // A builder that claims "clean" output, which is no page at all.
    let reply = world.build(
        "page",
        FIXTURE_BUILDER,
        &[("verbatim", "<p onclick=go()>hi")],
    );
    let build = built(&reply);
    let inspection = inspection_of(&reply).unwrap();
    assert_eq!(inspection.verdict, InspectionVerdict::Fail);
    assert_eq!(inspection.content_type, "text/html");
    assert_eq!(
        checks(&inspection),
        ["doctype", "title", "closed", "handler"]
    );
    // Recorded in provenance, beside the builder's own claim, and shown.
    let provenance = world.record(&build).provenance.unwrap();
    assert_eq!(provenance.inspection, inspection);
    assert_eq!(provenance.summary_claim.as_deref(), Some("clean"));
    assert_eq!(world.view(&build).inspection, Some(inspection));
    // Generic checks run for every kind: an empty document.
    let empty = world.build("document", FIXTURE_BUILDER, &[("verbatim", "")]);
    assert_eq!(
        inspection_of(&empty).as_ref().map(checks),
        Some(vec![String::from("empty"), String::from("title")])
    );
}

#[test]
fn an_inspector_pass_never_lets_bytes_the_gate_refuses_leave() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let mut world = World::new("pass-block");
    let (hub, received) = hub_with_client();
    // A well-formed document, carrying a vault secret.
    let reply = world.build(
        "document",
        DOCUMENT_BUILDER,
        &[("title", "Status"), ("body", &format!("token: {SECRET}"))],
    );
    let build = built(&reply);
    assert_eq!(
        inspection_of(&reply).map(|i| i.verdict),
        Some(InspectionVerdict::Pass)
    );
    for to in [local(), workspace("status.md")] {
        assert_eq!(
            world.deliver(Some(&hub), &build, to),
            ArtifactResult::Refused {
                reason: DeliveryRefusal::Blocked
            }
        );
    }
    // A clean, well-formed document, and a gate that cannot judge.
    let clean = built(&world.build(
        "document",
        DOCUMENT_BUILDER,
        &[("title", "Status"), ("body", "all clear")],
    ));
    world.config.artifacts.policy = None;
    assert_eq!(
        world.deliver(Some(&hub), &clean, local()),
        ArtifactResult::Refused {
            reason: DeliveryRefusal::GateError
        }
    );
    assert!(received.lock().unwrap().is_empty(), "nothing left");
    assert!(!world.base.join("workspace/status.md").exists());
    assert_eq!(world.view(&build).delivered, 0);
}

#[test]
fn a_gate_pass_delivers_despite_an_inspector_failure_and_surfaces_it() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("fail-pass");
    let (hub, received) = hub_with_client();
    // Clean bytes that are not a document: no title.
    let bytes = "no heading, just a line";
    let reply = world.build("document", FIXTURE_BUILDER, &[("verbatim", bytes)]);
    let build = built(&reply);
    let inspection = inspection_of(&reply).unwrap();
    assert_eq!(inspection.verdict, InspectionVerdict::Fail);
    assert_eq!(checks(&inspection), ["title"]);
    // The gate passes them, so they are delivered, byte for byte, and the
    // failure is surfaced with the delivery.
    assert_eq!(
        world.deliver(Some(&hub), &build, local()),
        ArtifactResult::Delivered {
            inspection: inspection.clone()
        }
    );
    assert_eq!(
        received
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>(),
        [bytes]
    );
    let record = world.record(&build);
    assert_eq!(
        record.deliveries.first().and_then(|d| d.verdict.clone()),
        Some(Verdict::Allow)
    );
    let view = world.view(&build);
    assert_eq!((view.inspection, view.delivered), (Some(inspection), 1));
}

/// A kind no crate ships, defined here in the test: comma-separated rows
/// of one width. It plugs in as a profile; the pipeline is untouched.
struct Table;

impl KindProfile for Table {
    fn kind(&self) -> &'static str {
        "table"
    }

    fn content_type(&self) -> &'static str {
        "text/csv"
    }

    fn check(&self, _request: &PinnedRequest, content: &str) -> Vec<InspectionFinding> {
        let widths: Vec<usize> = content
            .lines()
            .map(|line| line.split(',').count())
            .collect();
        if widths.windows(2).all(|pair| pair.first() == pair.last()) {
            Vec::new()
        } else {
            vec![finding("ragged", "the rows differ in width")]
        }
    }
}

#[test]
fn a_new_media_kind_plugs_into_the_pipeline_without_pipeline_changes() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let mut world = World::new("media");
    let manifest =
        r#"{"media_type":"audio/ogg","sources":["verbatim"],"steps":[{"op":"normalize"}]}"#;
    // Not registered: refused before anything is recorded.
    for kind in ["audio-clip", "table"] {
        assert_eq!(
            world.build(kind, FIXTURE_BUILDER, &[("verbatim", manifest)]),
            ArtifactResult::BuildFailed {
                reason: BuildFailure::UnknownKind
            }
        );
    }
    assert!(!world.config.artifacts.root.join("build-index").exists());
    // Registered: one profile each, and nothing else changes.
    world.config.artifacts.kinds = test_kinds()
        .with(MediaProfile::new("audio-clip", "audio/ogg"))
        .with(Table);
    let reply = world.build("audio-clip", FIXTURE_BUILDER, &[("verbatim", manifest)]);
    let clip = built(&reply);
    let inspection = inspection_of(&reply).unwrap();
    assert_eq!(
        inspection.verdict,
        InspectionVerdict::Pass,
        "{inspection:?}"
    );
    assert_eq!(inspection.content_type, MEDIA_MANIFEST_TYPE);
    assert_eq!(
        world.deliver(None, &clip, workspace("clip.json")),
        ArtifactResult::Delivered { inspection }
    );
    // The new kind's own checks run: a manifest for the wrong medium.
    let wrong = r#"{"media_type":"video/mp4","sources":["verbatim"],"steps":[]}"#;
    assert_eq!(
        inspection_of(&world.build("audio-clip", FIXTURE_BUILDER, &[("verbatim", wrong)]))
            .as_ref()
            .map(checks),
        Some(vec![String::from("media-type")])
    );
    // The standard video kind is the same interface.
    assert_eq!(
        inspection_of(&world.build("video", FIXTURE_BUILDER, &[("verbatim", wrong)]))
            .map(|i| (i.verdict, i.content_type)),
        Some((InspectionVerdict::Pass, String::from(MEDIA_MANIFEST_TYPE)))
    );
    // And a kind defined only in this test.
    let even = world.build("table", FIXTURE_BUILDER, &[("verbatim", "a,b\n1,2")]);
    let ragged = world.build("table", FIXTURE_BUILDER, &[("verbatim", "a,b\n1,2,3")]);
    assert_eq!(
        inspection_of(&even).map(|i| (i.verdict, i.content_type)),
        Some((InspectionVerdict::Pass, String::from("text/csv")))
    );
    assert_eq!(
        inspection_of(&ragged).as_ref().map(checks),
        Some(vec![String::from("ragged")])
    );
}

#[test]
fn a_builder_identity_renders_as_verified_only_while_the_broker_vouches_for_it() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("identity");
    let build = built(&world.build(
        "document",
        DOCUMENT_BUILDER,
        &[("title", "T"), ("body", "b")],
    ));
    let view = world.view(&build);
    assert_eq!(view.attribution, Some(verified(DOCUMENT_BUILDER)));
    assert_eq!(
        (view.kind.as_str(), view.state, view.output_hash),
        (
            "document",
            BuildState::Completed,
            Some(sha256_hex(b"# T\n\nb\n"))
        )
    );
    let stored = world.record(&build);
    let forge = |edit: &dyn Fn(&mut BuildRecord)| {
        let mut record = stored.clone();
        edit(&mut record);
        assert!(world.store().put(&record).unwrap());
        world.view(&build).attribution.unwrap_or_default()
    };
    // Another key's fingerprint under the real builder's name.
    let other_key = forge(&|record: &mut BuildRecord| {
        if let Some(p) = record.provenance.as_mut() {
            p.builder.signer = "cd".repeat(32);
        }
    });
    assert_eq!(other_key, "built by document@1.0.0: not verified");
    // The real key, under a builder that is not registered.
    let ghost = forge(&|record: &mut BuildRecord| {
        if let Some(p) = record.provenance.as_mut() {
            p.builder.skill = String::from("ghost@1.0.0");
        }
    });
    assert_eq!(ghost, "built by ghost@1.0.0: not verified");
    // A name written to read like a verdict.
    let posing = forge(&|record: &mut BuildRecord| {
        if let Some(p) = record.provenance.as_mut() {
            p.builder.skill = String::from("verified builder document@1.0.0");
        }
    });
    assert_eq!(posing, "built by an unnamed builder: not verified");
    // No identity at all.
    let missing = forge(&|record: &mut BuildRecord| record.provenance = None);
    assert_eq!(missing, "builder unknown: not verified");
    for shown in [other_key, ghost, posing, missing] {
        assert!(!shown.contains("verified builder"), "{shown}");
    }
    // Nothing to attribute until a build completes.
    let pending = BuildRecord::pending(
        String::from("art-pending"),
        PinnedRequest::pin("document", &[]).unwrap(),
    );
    assert!(world.store().insert(&pending).unwrap());
    let view = world.view("art-pending");
    assert_eq!(
        (view.state, view.attribution, view.inspection),
        (BuildState::Pending, None, None)
    );
    assert_eq!(
        world.build("fail", FIXTURE_BUILDER, &[]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::BuilderFailed
        }
    );
    assert_eq!(
        world.describe("art-nonexistent"),
        ArtifactResult::UnknownBuild
    );
}

/// When every op of a world runs, in Unix seconds.
const NOW: i64 = 1_790_000_000;

/// The broker-asserted principal ops run as: the test's own uid.
fn principal() -> MemoryPrincipal {
    MemoryPrincipal {
        peer_uid: rustix::process::getuid().as_raw(),
        session_id: SessionId::generate().unwrap(),
    }
}

/// The idea id and revision from an `IdeaSaved` reply, or a description
/// of what came back.
fn saved(result: ArtifactResult) -> (String, u32) {
    if let ArtifactResult::IdeaSaved { idea, revision } = result {
        (idea, revision)
    } else {
        (format!("not saved: {result:?}"), 0)
    }
}

/// Each input's name and content hash.
fn hashes<'a>(inputs: impl IntoIterator<Item = (&'a String, &'a String)>) -> Vec<(String, String)> {
    inputs
        .into_iter()
        .map(|(name, sha256)| (name.clone(), sha256.clone()))
        .collect()
}

/// The proposal from an `ActivationProposed` reply.
fn proposed(result: &ArtifactResult) -> Option<ActivationProposal> {
    if let ArtifactResult::ActivationProposed { proposal } = result {
        Some(proposal.clone())
    } else {
        None
    }
}

const fn idea_refused(reason: IdeaRefusal) -> ArtifactResult {
    ArtifactResult::IdeaRefused { reason }
}

impl World {
    fn capture(&self, text: &str, from: Option<IdeaSource>) -> ArtifactResult {
        self.op(
            None,
            &ArtifactOp::CaptureIdea {
                text: text.to_owned(),
                from,
            },
        )
    }

    fn refine(&self, idea: &str, text: &str) -> ArtifactResult {
        self.op(
            None,
            &ArtifactOp::RefineIdea {
                idea: idea.to_owned(),
                text: text.to_owned(),
            },
        )
    }

    fn note(&self, idea: &str, note: &str) -> ArtifactResult {
        self.op(
            None,
            &ArtifactOp::NoteIdea {
                idea: idea.to_owned(),
                note: note.to_owned(),
            },
        )
    }

    fn list(&self, after: Option<&str>) -> ArtifactResult {
        self.op(
            None,
            &ArtifactOp::ListIdeas {
                after: after.map(str::to_owned),
            },
        )
    }

    fn propose(&self, idea: &str, kind: &str, builder: &str, description: &str) -> ArtifactResult {
        self.op(
            None,
            &ArtifactOp::ProposeActivation {
                idea: idea.to_owned(),
                build_kind: kind.to_owned(),
                builder: builder.to_owned(),
                description: description.to_owned(),
            },
        )
    }

    fn activate(&self, activation: &str, confirmation: &str) -> ArtifactResult {
        self.op(
            None,
            &ArtifactOp::Activate {
                activation: activation.to_owned(),
                confirmation: confirmation.to_owned(),
            },
        )
    }

    /// The user confirms `activation` at `now`: the broker-side hook.
    fn confirm(&self, activation: &str, now: i64) -> String {
        confirm_activation(&self.config, key(), activation, now).unwrap()
    }

    /// The one idea's summary, from a list of exactly one.
    fn summary(&self) -> Option<IdeaSummary> {
        if let ArtifactResult::Ideas { ideas, next: None } = self.list(None) {
            (ideas.len() == 1).then(|| ideas.first().cloned()).flatten()
        } else {
            None
        }
    }
}

/// Checks the launch idea's proposal pins exactly what it says, and
/// returns the request it pins.
fn check_launch_proposal(
    proposal: &ActivationProposal,
    expected: &IdeaRef,
    refined: &str,
) -> PinnedRequest {
    assert!(proposal.activation.starts_with("act-"), "{proposal:?}");
    assert_eq!(&proposal.idea, expected);
    assert_eq!(
        (
            proposal.build_kind.as_str(),
            proposal.builder.as_str(),
            proposal.description.as_str()
        ),
        ("page", PAGE_BUILDER, "the launch page")
    );
    let pinned = PinnedRequest::pin(
        "page",
        &[
            ArtifactInput {
                name: String::from("idea"),
                content: String::from(refined),
            },
            ArtifactInput {
                name: String::from("notes"),
                content: String::from("- invite design"),
            },
        ],
    )
    .unwrap();
    assert_eq!(
        hashes(proposal.inputs.iter().map(|i| (&i.name, &i.sha256))),
        hashes(pinned.inputs.iter().map(|i| (&i.name, &i.sha256)))
    );
    pinned
}

#[test]
fn an_idea_is_captured_developed_and_activated_only_once_confirmed() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("ideas");
    let (idea, first) = saved(world.capture("Launch notes", Some(IdeaSource::Chat)));
    assert!(idea.starts_with("idea-"), "{idea}");
    assert_eq!(first, 1);
    let refined = "Launch notes\n\nShip on Friday.\n- tell the team";
    assert_eq!(saved(world.refine(&idea, refined)), (idea.clone(), 2));
    assert_eq!(
        world.refine(&idea, "  "),
        idea_refused(IdeaRefusal::BadText)
    );
    assert_eq!(
        saved(world.note(&idea, "- invite design")),
        (idea.clone(), 2)
    );
    let listed = |state| IdeaSummary {
        idea: idea.clone(),
        title: String::from("Launch notes"),
        revision: 2,
        notes: 1,
        state,
        origin: String::from("captured from chat"),
    };
    assert_eq!(world.summary(), Some(listed(IdeaState::Developing)));
    let proposal =
        proposed(&world.propose(&idea, "page", PAGE_BUILDER, "the launch page")).unwrap();
    let expected = IdeaRef {
        idea: idea.clone(),
        revision: 2,
        sha256: sha256_hex(refined.as_bytes()),
    };
    let pinned = check_launch_proposal(&proposal, &expected, refined);
    assert_eq!(
        world.summary(),
        Some(listed(IdeaState::AwaitingConfirmation))
    );
    // Without a confirmation the broker minted for it: refused.
    for forged in ["", "confirm-forged", proposal.activation.as_str()] {
        assert_eq!(
            world.activate(&proposal.activation, forged),
            idea_refused(IdeaRefusal::NotConfirmed),
            "{forged}"
        );
    }
    assert_eq!(
        world.summary(),
        Some(listed(IdeaState::AwaitingConfirmation))
    );
    // Confirmed by the user: built, exactly as pinned.
    let confirmation = world.confirm(&proposal.activation, NOW);
    assert!(confirmation.starts_with("confirm-"), "{confirmation}");
    let reply = world.activate(&proposal.activation, &confirmation);
    let build = built(&reply);
    let record = world.record(&build);
    assert_eq!(record.request, pinned);
    assert_eq!(record.idea.as_ref(), Some(&expected));
    assert_eq!(
        record.provenance.as_ref().and_then(|p| p.idea.clone()),
        Some(expected.clone())
    );
    assert_eq!(world.view(&build).idea, Some(expected));
    let html = world.content(&build);
    assert!(html.contains("<title>Launch notes</title>"), "{html}");
    assert!(html.contains("<p>Ship on Friday.</p>\n<ul>\n<li>tell the team</li>"));
    // Once: the activation is done.
    assert_eq!(
        world.activate(&proposal.activation, &confirmation),
        idea_refused(IdeaRefusal::NoProposal)
    );
    assert_eq!(world.summary(), Some(listed(IdeaState::Activated)));
    assert!(matches!(
        world.deliver(None, &build, workspace("launch.html")),
        ArtifactResult::Delivered { .. }
    ));
}

#[test]
fn the_build_is_what_was_confirmed_and_nothing_else() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("idea-bait");
    let (idea, _) = saved(world.capture("Short zine", None));
    let first = proposed(&world.propose(&idea, "page", PAGE_BUILDER, "a zine")).unwrap();
    let token = world.confirm(&first.activation, NOW);
    // The idea changes after the user confirmed: what they confirmed is
    // withdrawn, and the confirmation activates nothing.
    assert_eq!(saved(world.refine(&idea, "Long zine")), (idea.clone(), 2));
    assert_eq!(
        world.activate(&first.activation, &token),
        idea_refused(IdeaRefusal::NoProposal)
    );
    assert_eq!(
        confirm_activation(&world.config, key(), &first.activation, NOW),
        Err(ConfirmError::NoProposal)
    );
    // A confirmation is bound to its activation: one minted for a
    // replaced proposal activates neither.
    let second = proposed(&world.propose(&idea, "page", PAGE_BUILDER, "a zine")).unwrap();
    let stale = world.confirm(&second.activation, NOW);
    let third = proposed(&world.propose(&idea, "page", PAGE_BUILDER, "a zine, again")).unwrap();
    assert_ne!(second.activation, third.activation);
    assert_eq!(
        world.activate(&third.activation, &stale),
        idea_refused(IdeaRefusal::NotConfirmed)
    );
    assert_eq!(
        world.activate(&second.activation, &stale),
        idea_refused(IdeaRefusal::NoProposal)
    );
    // An expired confirmation activates nothing, and is spent.
    let expired = world.confirm(&third.activation, NOW - CONFIRMATION_TTL_SECS);
    assert_eq!(
        world.activate(&third.activation, &expired),
        idea_refused(IdeaRefusal::NotConfirmed)
    );
    // Confirmed in time: the build is the pinned request, nothing else.
    let good = world.confirm(&third.activation, NOW);
    let build = built(&world.activate(&third.activation, &good));
    let record = world.record(&build);
    assert_eq!(
        hashes(record.request.inputs.iter().map(|i| (&i.name, &i.sha256))),
        hashes(third.inputs.iter().map(|i| (&i.name, &i.sha256)))
    );
    assert_eq!(
        record
            .request
            .inputs
            .first()
            .map(|input| input.content.as_str()),
        Some("Long zine")
    );
    assert_eq!(record.idea.map(|idea| idea.revision), Some(2));
}

#[test]
fn idea_ops_refuse_what_they_cannot_do_and_list_a_page_at_a_time() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("idea-refusals");
    let unknown = idea_refused(IdeaRefusal::UnknownIdea);
    assert_eq!(world.refine("idea-nonexistent", "x"), unknown);
    // There is no build from an idea without a confirmed activation.
    let (idea, _) = saved(world.capture("a zine", Some(IdeaSource::Command)));
    assert_eq!(
        world.op(
            None,
            &ArtifactOp::BuildIdea {
                idea: idea.clone(),
                build_kind: String::from("page"),
                builder: String::from(PAGE_BUILDER),
            },
        ),
        idea_refused(IdeaRefusal::ConfirmationRequired)
    );
    assert_eq!(world.note("idea-nonexistent", "x"), unknown);
    assert_eq!(
        world.propose("../build-index", "page", PAGE_BUILDER, "d"),
        unknown
    );
    assert_eq!(
        world.activate("act-nonexistent", "confirm-x"),
        idea_refused(IdeaRefusal::NoProposal)
    );
    assert_eq!(
        confirm_activation(&world.config, key(), "act-nonexistent", NOW),
        Err(ConfirmError::NoProposal)
    );
    for bad in [String::new(), String::from("\n\t "), "x".repeat(4097)] {
        assert_eq!(
            world.capture(&bad, None),
            idea_refused(IdeaRefusal::BadText)
        );
    }
    assert_eq!(
        world.summary().map(|s| (s.state, s.origin)),
        Some((
            IdeaState::Captured,
            String::from("captured by explicit command")
        ))
    );
    assert_eq!(
        world.note(&idea, "a\tb"),
        idea_refused(IdeaRefusal::BadNote)
    );
    for n in 0..hatchling_artifact::MAX_NOTES {
        assert_eq!(saved(world.note(&idea, &format!("note {n}"))).1, 1);
    }
    assert_eq!(
        world.note(&idea, "one more"),
        idea_refused(IdeaRefusal::TooManyNotes)
    );
    assert_eq!(
        world.propose(&idea, "page", PAGE_BUILDER, "a\nb"),
        idea_refused(IdeaRefusal::BadDescription)
    );
    for (kind, builder, reason) in [
        ("zine", PAGE_BUILDER, BuildFailure::UnknownKind),
        ("Not A Kind", PAGE_BUILDER, BuildFailure::BadRequest),
        ("page", "not a skill id", BuildFailure::UnknownBuilder),
    ] {
        assert_eq!(
            world.propose(&idea, kind, builder, "d"),
            ArtifactResult::BuildFailed { reason },
            "{kind} {builder}"
        );
    }
    // A confirmed activation still goes through the skill gate.
    let gated = proposed(&world.propose(&idea, "page", FIXTURE_SKILL, "d")).unwrap();
    let token = world.confirm(&gated.activation, NOW);
    assert_eq!(
        world.activate(&gated.activation, &token),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::CapabilityDenied
        }
    );
}

#[test]
fn ideas_fill_the_store_and_list_a_page_at_a_time() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("idea-pages");
    assert!(!world.config.artifacts.root.join("idea-index").exists());
    assert!(matches!(
        world.capture("a zine", None),
        ArtifactResult::IdeaSaved { .. }
    ));
    // The store fills, and lists a page at a time.
    for n in 1..hatchling_broker::artifact_ops::MAX_IDEAS {
        assert!(
            matches!(
                world.capture(&format!("idea {n}"), None),
                ArtifactResult::IdeaSaved { .. }
            ),
            "{n}"
        );
    }
    assert_eq!(
        world.capture("one more", None),
        idea_refused(IdeaRefusal::Full)
    );
    let mut after: Option<String> = None;
    let mut seen = Vec::new();
    for page in 0..4 {
        let ArtifactResult::Ideas { ideas, next } = world.list(after.as_deref()) else {
            panic_free_fail(&format!("page {page}"));
            return;
        };
        assert_eq!(
            ideas.len(),
            hatchling_broker::artifact_ops::IDEA_PAGE,
            "{page}"
        );
        seen.extend(ideas.iter().map(|summary| summary.title.clone()));
        assert_eq!(next.is_some(), page < 3, "{page}");
        after = next;
    }
    let expected: Vec<String> = std::iter::once(String::from("a zine"))
        .chain((1..64).map(|n| format!("idea {n}")))
        .collect();
    assert_eq!(seen, expected);
    assert_eq!(
        world.list(Some("idea-unknown")),
        ArtifactResult::Ideas {
            ideas: Vec::new(),
            next: None
        }
    );
    // Ideas and builds are kept apart: a full idea shelf is not a full
    // build shelf.
    assert!(matches!(
        world.build("echo", FIXTURE_BUILDER, &[("body", "x")]),
        ArtifactResult::Built { .. }
    ));
}

#[test]
fn an_idea_captured_from_the_feed_keeps_its_items_whole_trust_chain() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let mut world = World::new("idea-feed");
    let goals_root = world.base.join("goals");
    let memory_root = world.base.join("memory");
    world.config.goals = hatchling_broker::GoalsConfig {
        root: goals_root.clone(),
        grants: vec![GoalsAction::QueryGoals, GoalsAction::QueryTracking],
    };
    world.config.memory = MemoryConfig {
        root: memory_root.clone(),
        grants: vec![MemoryGrant {
            scope: String::from("user"),
            action: MemoryAction::Recall,
        }],
    };
    let me = principal();
    let goals = GoalsStore::open(&goals_root).unwrap();
    let entry = goals
        .append_tracking(&me, &"whatsapp".parse().unwrap(), "buy the paper", 1)
        .unwrap();
    let goal = goals.create_goal(&me, "Run 5k", "", 1).unwrap();
    MemoryStore::open(&memory_root)
        .unwrap()
        .write(
            &me,
            &MemoryScope::User,
            &"m-1".parse().unwrap(),
            "passport expires in March",
            &["travel".parse().unwrap()],
            RetentionPolicy::Keep,
            1,
        )
        .unwrap();
    let feed = |store: &str, id: &str, tag: Option<&str>| {
        Some(IdeaSource::Feed {
            store: store.to_owned(),
            id: id.to_owned(),
            tag: tag.map(str::to_owned),
        })
    };
    let origin_of = |from| {
        let (idea, _) = saved(world.capture("an idea", from));
        let ArtifactResult::Ideas { ideas, .. } = world.list(None) else {
            return String::from("no list");
        };
        ideas
            .into_iter()
            .find(|summary| summary.idea == idea)
            .map_or_else(|| String::from("not listed"), |summary| summary.origin)
    };
    assert_eq!(
        origin_of(feed("tracking", &entry.seq.to_string(), None)),
        "captured from the feed: from tracking (originally from whatsapp)"
    );
    assert_eq!(
        origin_of(feed("goal", &goal.id.to_string(), None)),
        "captured from the feed: from goal (created by the user)"
    );
    assert_eq!(
        origin_of(feed("memory", "m-1", Some("travel"))),
        "captured from the feed: from memory (origin not recorded)"
    );
    // An item the session cannot read is not a source.
    let unreadable = idea_refused(IdeaRefusal::UnreadableSource);
    for from in [
        feed("tracking", "999", None),
        feed("tracking", "x", None),
        feed("goal", "not a goal id", None),
        feed("memory", "m-1", None),
        feed("memory", "m-1", Some("work")),
        feed("vault", "app-token", None),
    ] {
        assert_eq!(
            world.capture("an idea", from.clone()),
            unreadable,
            "{from:?}"
        );
    }
    world.config.goals.grants = vec![GoalsAction::QueryGoals];
    assert_eq!(
        world.capture("an idea", feed("tracking", &entry.seq.to_string(), None)),
        unreadable
    );
    world.config.memory.grants = Vec::new();
    assert_eq!(
        world.capture("an idea", feed("memory", "m-1", Some("travel"))),
        unreadable
    );
}

#[test]
fn a_full_build_store_refuses_the_next_build() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("builds-full");
    for n in 0..hatchling_broker::artifact_ops::MAX_BUILDS {
        let pending =
            BuildRecord::pending(format!("art-{n}"), PinnedRequest::pin("echo", &[]).unwrap());
        assert!(world.store().insert(&pending).unwrap(), "{n}");
    }
    let next = BuildRecord::pending(
        String::from("art-next"),
        PinnedRequest::pin("echo", &[]).unwrap(),
    );
    assert!(!world.store().insert(&next).unwrap());
    assert_eq!(
        world.build("echo", FIXTURE_BUILDER, &[("body", "x")]),
        ArtifactResult::BuildFailed {
            reason: BuildFailure::Full
        }
    );
    // Delivery counts in a view are only the deliveries that happened.
    assert_eq!(world.view("art-0").delivered, 0);
}

/// Research sources by URL.
struct Sources(Vec<(&'static str, &'static str)>);

impl hatchling_research::Fetcher for Sources {
    fn fetch(
        &self,
        url: &hatchling_research::SourceUrl,
    ) -> Result<hatchling_research::Fetched, hatchling_research::FetchFailure> {
        self.0
            .iter()
            .find(|(at, _)| *at == url.as_str())
            .map(|(_, body)| hatchling_research::Fetched {
                status: 200,
                body: body.as_bytes().to_vec(),
            })
            .ok_or(hatchling_research::FetchFailure::Failed)
    }
}

/// A user who approves the plan.
struct Approves;

impl hatchling_research::Approver for Approves {
    fn review_plan(&self, _plan: &hatchling_research::Plan) -> hatchling_research::Decision {
        hatchling_research::Decision::Approve
    }

    fn review_amendment(
        &self,
        _plan: &hatchling_research::Plan,
        _url: &hatchling_research::SourceUrl,
        _why: &str,
    ) -> hatchling_research::Decision {
        hatchling_research::Decision::Decline {
            reason: String::from("no"),
        }
    }
}

/// A finished research run over two conflicting sources.
fn research_outcome() -> hatchling_research::Outcome {
    use hatchling_loop::{FakeModel, ModelAction, ToolCall};
    let fetch = |id: &str| {
        ModelAction::ToolCalls(vec![ToolCall {
            name: String::from("fetch_source"),
            arguments: serde_json::json!({ "source": id }),
        }])
    };
    let model = FakeModel::new(vec![
        ModelAction::Text(
            serde_json::json!({"strategy": "Compare.", "sources": [
                {"url": "https://spec.example/rfc", "why": "the spec"},
                {"url": "https://blog.example/post", "why": "a dissent"}
            ]})
            .to_string(),
        ),
        fetch("s1"),
        fetch("s2"),
        ModelAction::Text(String::from("done")),
        ModelAction::Text(
            serde_json::json!({
                "title": "Does X hold?",
                "summary": "The spec says so; a blog disagrees.",
                "findings": [{"claim": "X holds.", "cites": [{"source": "s1", "depth": "primary"}]}],
                "disagreements": [{"topic": "whether X holds", "sides": [
                    {"position": "X holds", "cites": [{"source": "s1", "depth": "primary"}]},
                    {"position": "X does not hold", "cites": [{"source": "s2", "depth": "primary"}]}
                ]}]
            })
            .to_string(),
        ),
    ]);
    let sources = Sources(vec![
        ("https://spec.example/rfc", "The spec says X holds."),
        ("https://blog.example/post", "X does not hold."),
    ]);
    hatchling_research::Research {
        model: &model,
        fetcher: &sources,
        approver: &Approves,
        clock: &hatchling_research::SystemClock,
        bounds: hatchling_research::Bounds::standard(),
    }
    .run("Does X hold?")
    .unwrap()
}

#[test]
fn a_research_report_is_built_and_delivered_as_a_cited_document() {
    if !guard_cell_available().unwrap() {
        return;
    }
    let world = World::new("research-report");
    let outcome = research_outcome();
    let inputs = outcome.inputs();
    let pairs: Vec<(&str, &str)> = inputs
        .iter()
        .map(|(name, text)| (*name, text.as_str()))
        .collect();
    let reply = world.build("document", DOCUMENT_BUILDER, &pairs);
    let build = built(&reply);
    let inspection = inspection_of(&reply).unwrap();
    assert_eq!(
        inspection.verdict,
        InspectionVerdict::Pass,
        "{inspection:?}"
    );
    let document = world.content(&build);
    assert!(
        document.starts_with("# Does X hold?\n\n## status\n\nStatus: cited report.\n\nFreshness: the sources were fetched from "),
        "{document}"
    );
    assert!(
        document.contains("\n## findings\n\n1. X holds. [s1]\n"),
        "{document}"
    );
    assert!(
        document.contains(
            "\n## disagreements\n\nOn whether X holds:\n- X holds [s1]\n- X does not hold [s2]\n"
        ),
        "{document}"
    );
    assert!(
        document.contains("\n## sources\n\n- [s1] https://spec.example/rfc: read "),
        "{document}"
    );
    // The broker pinned every section of the report.
    let provenance = world.record(&build).provenance.unwrap();
    let pinned: Vec<(String, String)> = provenance
        .inputs
        .iter()
        .map(|input| (input.name.clone(), input.sha256.clone()))
        .collect();
    let expected: Vec<(String, String)> = inputs
        .iter()
        .map(|(name, text)| ((*name).to_owned(), sha256_hex(text.as_bytes())))
        .collect();
    assert_eq!(pinned, expected);
    // Delivered as built, through the one delivery function.
    assert_eq!(
        world.deliver(None, &build, workspace("report.md")),
        ArtifactResult::Delivered { inspection }
    );
    assert_eq!(
        std::fs::read_to_string(world.base.join("workspace/report.md")).unwrap(),
        document
    );
}
