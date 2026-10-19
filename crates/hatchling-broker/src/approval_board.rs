//! The owner's approvals, wired (WO-027): each broker's one ledger, what
//! reaches the owner's app, and the worker that runs what the owner
//! approved.
//!
//! - **A proposal is frozen, then asked.** When a session proposes an
//!   activation ([`Board::propose_activation`]), the broker builds the
//!   record of what would run from its own pinned request (the idea and
//!   its revision, the kind, the builder, each input's digest, the request
//!   hash) and puts it to the owner on the UI stream: a
//!   [`ConfirmationRequest`] with the action's digest and its fields, the
//!   broker's first and the agent's words labeled as the agent's.
//! - **Only the owner decides** ([`Board::decide`]), from the app, by the
//!   request's id, revision and digest. An approval is queued for the
//!   worker; the decision goes out on the stream either way.
//! - **Only the worker runs it** ([`work_forever`]). It claims the request,
//!   mints the activation's confirmation, and activates exactly the pinned
//!   request, which builds only if the idea still pins it unchanged. The
//!   outcome goes to the owner on the local channel, as the broker's.
//!
//! One board per broker, found by the broker's socket path: each broker,
//! and each test with its own paths, has its own ledger.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use hatchling_channel::LocalHub;
use hatchling_core::ui::{
    ConfirmationField, ConfirmationRequest, Decision, DecisionRejection, FieldSource, UiEventBody,
};
use hatchling_core::{ActivationProposal, ArtifactResult};
use hatchling_scheduler::{Delivery as _, Timestamp};
use hatchling_vault::MasterKey;
use serde_json::json;

use crate::Config;
use crate::Skills;
use crate::api_stream::Bus;
use crate::approvals::{Approvals, Proposal, Refusal, Requested, State};

/// What an approved activation runs: the broker's own record of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivationRun {
    /// The activation id the idea pins.
    pub activation: String,
    /// The idea.
    pub idea: String,
}

/// How long the worker waits for an approval before looking again.
pub const WORKER_WAIT: Duration = Duration::from_secs(1);

/// The chat a confirmation belongs to: the owner's local chat.
pub const CONFIRMATION_CHAT: &str = "main";

/// One broker's approvals.
#[derive(Debug)]
pub struct Board {
    approvals: Approvals<ActivationRun>,
    bus: Mutex<Option<Arc<Bus>>>,
    queue: Mutex<VecDeque<String>>,
    wake: Condvar,
}

/// The boards, by the socket path of the broker each belongs to.
static BOARDS: Mutex<BTreeMap<String, Arc<Board>>> = Mutex::new(BTreeMap::new());

/// The board of the broker `config` configures.
#[must_use]
pub fn board(config: &Config) -> Arc<Board> {
    Arc::clone(
        BOARDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(config.socket.as_path().display().to_string())
            .or_insert_with(|| {
                Arc::new(Board {
                    approvals: Approvals::new(),
                    bus: Mutex::new(None),
                    queue: Mutex::new(VecDeque::new()),
                    wake: Condvar::new(),
                })
            }),
    )
}

impl Board {
    /// Sends what the board asks and decides to the owner's app on `bus`.
    pub fn attach(&self, bus: Arc<Bus>) {
        *self.bus.lock().unwrap_or_else(PoisonError::into_inner) = Some(bus);
    }

    /// The ledger.
    #[must_use]
    pub const fn approvals(&self) -> &Approvals<ActivationRun> {
        &self.approvals
    }

    /// Puts the activation `proposal`, which task `task` proposed, to the
    /// owner at `now`: the request, and whether it was new (a new one is
    /// sent to the app; an identical one waiting already was).
    ///
    /// # Errors
    ///
    /// [`Refusal`]: the owner denied this effect, requests are paused or
    /// full, or the record has no canonical form.
    pub fn propose_activation(
        &self,
        proposal: &ActivationProposal,
        task: &str,
        now: i64,
    ) -> Result<Requested, Refusal> {
        let inputs: Vec<_> = proposal
            .inputs
            .iter()
            .map(|input| json!({ "name": input.name, "sha256": input.sha256 }))
            .collect();
        let effect = json!({
            "op": "artifact.activate",
            "idea": proposal.idea.idea,
            "build_kind": proposal.build_kind,
            "builder": proposal.builder,
            "request_hash": proposal.request_hash,
        });
        let action = json!({
            "format": 1,
            "op": "artifact.activate",
            "activation": proposal.activation,
            "idea": {
                "id": proposal.idea.idea,
                "revision": proposal.idea.revision,
                "sha256": proposal.idea.sha256,
            },
            "build_kind": proposal.build_kind,
            "builder": proposal.builder,
            "description": proposal.description,
            "inputs": inputs,
            "request_hash": proposal.request_hash,
        });
        let requested = self.approvals.propose(
            Proposal {
                run: ActivationRun {
                    activation: proposal.activation.clone(),
                    idea: proposal.idea.idea.clone(),
                },
                action,
                fingerprint: effect,
                task: task.to_owned(),
            },
            now,
        )?;
        if requested.new {
            self.publish(UiEventBody::Confirmation(confirmation(
                proposal, &requested,
            )));
        }
        Ok(requested)
    }

    /// The owner's `decision` on request `id`, at the `revision` and
    /// `digest` they were shown, at `now`. An approval is queued for the
    /// worker, and the decision goes out on the stream.
    ///
    /// # Errors
    ///
    /// [`DecisionRejection`], and nothing changes.
    pub fn decide(
        &self,
        id: &str,
        revision: u64,
        digest: &str,
        decision: Decision,
        now: i64,
    ) -> Result<State, DecisionRejection> {
        let decided = self.approvals.decide(id, revision, digest, decision, now)?;
        self.publish(UiEventBody::ConfirmationDecided {
            confirmation_id: id.to_owned(),
            revision,
            decision,
        });
        if decided == State::Approved {
            self.queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_back(id.to_owned());
            self.wake.notify_all();
        }
        Ok(decided)
    }

    /// The next approved request for the worker, waiting up to `wait`.
    #[must_use]
    pub fn next_approved(&self, wait: Duration) -> Option<String> {
        self.wake
            .wait_timeout_while(
                self.queue.lock().unwrap_or_else(PoisonError::into_inner),
                wait,
                |queue| queue.is_empty(),
            )
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .pop_front()
    }

    /// Pushes `body` to the owner's app, if one is attached.
    fn publish(&self, body: UiEventBody) {
        let bus = self
            .bus
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(bus) = bus {
            bus.push(Timestamp::now().to_string(), body);
        }
    }
}

/// The confirmation the owner is shown for `proposal`, requested as
/// `requested`: the broker's fields first, then the agent's words.
fn confirmation(proposal: &ActivationProposal, requested: &Requested) -> ConfirmationRequest {
    let broker = |label: &str, value: String| ConfirmationField {
        label: label.to_owned(),
        value,
        source: FieldSource::Broker,
    };
    let mut fields = vec![
        broker("Operation", String::from("Build an idea into an artifact")),
        broker(
            "Idea",
            format!(
                "{} at revision {} (text sha256 {})",
                proposal.idea.idea, proposal.idea.revision, proposal.idea.sha256
            ),
        ),
        broker("Build kind", proposal.build_kind.clone()),
        broker("Builder", proposal.builder.clone()),
    ];
    for input in &proposal.inputs {
        fields.push(broker(
            "Input",
            format!("{} (sha256 {})", input.name, input.sha256),
        ));
    }
    fields.push(broker("Request hash", proposal.request_hash.clone()));
    fields.push(ConfirmationField {
        label: String::from("What the build is for"),
        value: proposal.description.clone(),
        source: FieldSource::Agent,
    });
    ConfirmationRequest {
        confirmation_id: requested.id.clone(),
        revision: requested.revision,
        chat_id: Some(String::from(CONFIRMATION_CHAT)),
        summary: String::from("Build an idea into an artifact"),
        expires_at: Timestamp::from_second(requested.expires_at)
            .ok()
            .map(|at| at.to_string()),
        digest: requested.digest.clone(),
        fields,
    }
}

/// The worker: forever, runs each approval the owner gives on `config`'s
/// board, with the broker's `key` and `skills`, and tells the owner on
/// `hub` how it went.
pub fn work_forever(
    config: &Config,
    key: &MasterKey,
    skills: &Skills,
    hub: Option<&LocalHub>,
) -> ! {
    let board = board(config);
    loop {
        if let Some(id) = board.next_approved(WORKER_WAIT) {
            let told = run_approved(
                &board,
                &id,
                config,
                key,
                skills,
                hub,
                Timestamp::now().as_second(),
            );
            tell_owner(hub, &told);
        }
    }
}

/// Runs approved request `id` once, at `now`: what the owner is told.
fn run_approved(
    board: &Board,
    id: &str,
    config: &Config,
    key: &MasterKey,
    skills: &Skills,
    hub: Option<&LocalHub>,
    now: i64,
) -> String {
    let Some((run, claim)) = board.approvals().claim(id) else {
        return format!("Approval {id} was not run: it is no longer approved.");
    };
    let token = match crate::activation::confirm_activation(config, key, &run.activation, now) {
        Ok(token) => token,
        Err(error) => {
            claim.failed();
            tracing::warn!(%id, ?error, "an approved activation could not be confirmed");
            return format!(
                "Approved: build idea {}. It was not built: the idea no longer pins that request.",
                run.idea
            );
        }
    };
    match crate::artifact_ops::activate_confirmed(
        config,
        key,
        skills,
        hub,
        &run.activation,
        &token,
        now,
    ) {
        ArtifactResult::Built {
            build, output_hash, ..
        } => {
            claim.succeeded();
            format!(
                "Approved: build idea {}. Built {build} (sha256 {output_hash}).",
                run.idea
            )
        }
        ArtifactResult::ArtifactError => {
            // Whether the build happened isn't known: the claim, dropped,
            // leaves the request outcome_unknown, never run again.
            drop(claim);
            format!(
                "Approved: build idea {}. Whether it was built isn't known; the broker's log says why.",
                run.idea
            )
        }
        other @ (ArtifactResult::BuildFailed { .. }
        | ArtifactResult::Delivered { .. }
        | ArtifactResult::Refused { .. }
        | ArtifactResult::Described { .. }
        | ArtifactResult::UnknownBuild
        | ArtifactResult::IdeaSaved { .. }
        | ArtifactResult::Ideas { .. }
        | ArtifactResult::ActivationProposed { .. }
        | ArtifactResult::ApprovalRefused { .. }
        | ArtifactResult::IdeaRefused { .. }
        | ArtifactResult::Denied) => {
            claim.failed();
            tracing::warn!(%id, result = ?other, "an approved activation did not build");
            format!(
                "Approved: build idea {}. It was not built; the broker's log says why.",
                run.idea
            )
        }
    }
}

/// Tells the owner `text` on the local channel, as the broker's.
fn tell_owner(hub: Option<&LocalHub>, text: &str) {
    if let Err(error) = crate::schedule_ops::HubDelivery::new(hub).deliver("local", text) {
        tracing::warn!(%error, "an approval's outcome was not delivered");
    }
}
