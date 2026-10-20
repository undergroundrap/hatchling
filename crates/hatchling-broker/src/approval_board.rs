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
//! A site the agent asks to read (WO-028) is put to the owner on the same
//! board ([`Board::propose_site`]), under the same limits, and offers
//! approval for the session as well as once. Nothing runs it: the session
//! that asked reads the decision and fetches.
//!
//! One board per broker, found by its vault's root: a broker has one
//! vault, and each test its own, so each has its own ledger and limits.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use hatchling_channel::LocalHub;
use hatchling_core::ui::{
    ConfirmationField, ConfirmationRequest, Decision, DecisionRejection, FieldSource, UiEventBody,
};
use hatchling_core::{ActivationProposal, ApprovalRefusal, ArtifactResult};
use hatchling_scheduler::{Delivery as _, Timestamp};
use hatchling_vault::OpensStores;
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

/// What an approval is for: the broker's own record of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approved {
    /// An activation, which the worker runs.
    Activation(ActivationRun),
    /// A site the agent may read, which the session that asked reads.
    Site(SiteVisit),
}

/// A site the agent asked to read (WO-028).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteVisit {
    /// The site, as the owner sees it: `https://host[:port]`.
    pub site: String,
}

/// How long the worker waits for an approval before looking again.
pub const WORKER_WAIT: Duration = Duration::from_secs(1);

/// The chat a confirmation belongs to: the owner's local chat.
pub const CONFIRMATION_CHAT: &str = "main";

/// One broker's approvals.
#[derive(Debug)]
pub struct Board {
    approvals: Approvals<Approved>,
    bus: Mutex<Option<Arc<Bus>>>,
    queue: Mutex<VecDeque<String>>,
    wake: Condvar,
}

/// The boards, by the vault root of the broker each belongs to.
static BOARDS: Mutex<BTreeMap<String, Arc<Board>>> = Mutex::new(BTreeMap::new());

/// The board of the broker `config` configures.
#[must_use]
pub fn board(config: &Config) -> Arc<Board> {
    Arc::clone(
        BOARDS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(config.vault.root.display().to_string())
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
    pub const fn approvals(&self) -> &Approvals<Approved> {
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
                run: Approved::Activation(ActivationRun {
                    activation: proposal.activation.clone(),
                    idea: proposal.idea.idea.clone(),
                }),
                action,
                fingerprint: effect,
                task: task.to_owned(),
                session: false,
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

    /// Puts `site`, which task `task` asks to read `url` on for `reason`,
    /// to the owner at `now` (WO-028): the request, and whether it was new.
    /// What is approved is the site alone; the page and the reason are
    /// shown, the reason as the agent's words, and, when one of the task's
    /// searches gave the page, which (`led`: the search, and its place).
    ///
    /// # Errors
    ///
    /// [`Refusal`]: the owner said no to this site, requests are paused or
    /// full, or the record has no canonical form.
    pub fn propose_site(
        &self,
        site: &str,
        url: &str,
        reason: &str,
        led: Option<(&str, usize)>,
        task: &str,
        now: i64,
    ) -> Result<Requested, Refusal> {
        let requested = self.approvals.propose(
            Proposal {
                run: Approved::Site(SiteVisit {
                    site: site.to_owned(),
                }),
                action: site_action(site),
                fingerprint: json!({ "op": "web.visit", "site": site }),
                task: task.to_owned(),
                session: true,
            },
            now,
        )?;
        if requested.new {
            self.publish(UiEventBody::Confirmation(site_confirmation(
                site, url, reason, led, &requested,
            )));
        }
        Ok(requested)
    }

    /// The owner's `decision` on request `id`, at the `revision` and
    /// `digest` they were shown, at `now`. An approved activation is queued
    /// for the worker, and the decision goes out on the stream.
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
        let activation = self
            .approvals
            .get(id)
            .is_some_and(|request| matches!(request.run(), Approved::Activation(_)));
        if decided == State::Approved && activation {
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

/// The record a site's approval binds: the site alone (WO-028).
#[must_use]
pub fn site_action(site: &str) -> serde_json::Value {
    json!({ "format": 1, "op": "web.visit", "site": site })
}

/// The confirmation the owner is shown for reading `site`: the broker's
/// fields first, then the agent's words.
fn site_confirmation(
    site: &str,
    url: &str,
    reason: &str,
    led: Option<(&str, usize)>,
    requested: &Requested,
) -> ConfirmationRequest {
    let field = |label: &str, value: &str, source: FieldSource| ConfirmationField {
        label: label.to_owned(),
        value: value.to_owned(),
        source,
    };
    ConfirmationRequest {
        confirmation_id: requested.id.clone(),
        revision: requested.revision,
        chat_id: Some(String::from(CONFIRMATION_CHAT)),
        summary: String::from("Let the agent read a site"),
        expires_at: Timestamp::from_second(requested.expires_at)
            .ok()
            .map(|at| at.to_string()),
        digest: requested.digest.clone(),
        fields: {
            let mut fields = vec![
                field("Operation", "Read pages from a site", FieldSource::Broker),
                field("Site", site, FieldSource::Broker),
                field(
                    "What it allows",
                    "Once: this page, now. For this session: any page on this site until the session ends. The site sees whatever the agent puts in a page's address.",
                    FieldSource::Broker,
                ),
            ];
            if let Some((query, place)) = led {
                fields.push(field(
                    "Found by",
                    &format!("result {place} of a search the agent ran this session"),
                    FieldSource::Broker,
                ));
                fields.push(field("The search", query, FieldSource::Agent));
            }
            fields.push(field("Page it asked for", url, FieldSource::Agent));
            fields.push(field("Why it wants it", reason, FieldSource::Agent));
            fields
        },
        session: true,
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
        session: false,
    }
}

/// The worker: forever, runs each approval the owner gives on `config`'s
/// board, with the broker's `key` and `skills`, and tells the owner on
/// `hub` how it went.
pub fn work_forever(
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
) -> ! {
    loop {
        let _ = run_next(config, key, skills, hub, WORKER_WAIT);
    }
}

/// One step of the worker: waits up to `wait` for an approval on
/// `config`'s board, runs it once, and tells the owner on `hub`. What the
/// owner was told, if anything was approved.
pub fn run_next(
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
    wait: Duration,
) -> Option<String> {
    let board = board(config);
    let id = board.next_approved(wait)?;
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
    Some(told)
}

/// What the agent is told when its proposal wasn't put to the owner.
#[must_use]
pub fn refused_as(refusal: &Refusal) -> ApprovalRefusal {
    match refusal {
        Refusal::Denied => ApprovalRefusal::Denied,
        Refusal::Paused => ApprovalRefusal::Paused,
        Refusal::QueueFull => ApprovalRefusal::QueueFull,
        Refusal::Unfrozen(_) | Refusal::Entropy => {
            tracing::error!(error = %refusal, "an action could not be put to the owner");
            ApprovalRefusal::Unavailable
        }
    }
}

/// Runs approved request `id` on `board` once, at `now`: what the owner is
/// told. Nothing runs unless the request is approved and unclaimed.
#[must_use]
pub fn run_approved(
    board: &Board,
    id: &str,
    config: &Config,
    key: &dyn OpensStores,
    skills: &Skills,
    hub: Option<&LocalHub>,
    now: i64,
) -> String {
    let site = board
        .approvals()
        .get(id)
        .is_some_and(|request| matches!(request.run(), Approved::Site(_)));
    if site {
        // A site is read by the session that asked, never by the worker,
        // and its approval is left for that session.
        return format!("Approval {id} was not run: it is not the worker's to run.");
    }
    let Some((Approved::Activation(run), claim)) = board.approvals().claim(id) else {
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
pub fn tell_owner(hub: Option<&LocalHub>, text: &str) {
    if let Err(error) = crate::schedule_ops::HubDelivery::new(hub).deliver("local", text) {
        tracing::warn!(%error, "an approval's outcome was not delivered");
    }
}

#[cfg(test)]
mod tests {
    use hatchling_core::{ActivationInput, IdeaRef};

    use super::*;

    const fn empty() -> Board {
        Board {
            approvals: Approvals::new(),
            bus: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
            wake: Condvar::new(),
        }
    }

    fn proposal(activation: &str, description: &str) -> ActivationProposal {
        ActivationProposal {
            activation: activation.to_owned(),
            idea: IdeaRef {
                idea: String::from("idea-1"),
                revision: 2,
                sha256: String::from("aa"),
            },
            build_kind: String::from("page"),
            builder: String::from("page-builder@1.0.0"),
            description: description.to_owned(),
            inputs: vec![ActivationInput {
                name: String::from("idea"),
                sha256: String::from("bb"),
            }],
            request_hash: String::from("cc"),
        }
    }

    fn confirmations(bus: &Bus) -> Vec<ConfirmationRequest> {
        bus.after(None)
            .into_iter()
            .filter_map(|event| {
                if let UiEventBody::Confirmation(request) = event.body {
                    Some(request)
                } else {
                    None
                }
            })
            .collect()
    }

    /// The owner is asked once, with the broker's fields first and the
    /// agent's words last and labeled as the agent's.
    #[test]
    fn a_proposal_is_asked_once_with_the_brokers_fields_first() {
        let board = empty();
        let bus = Arc::new(Bus::starting_at(1));
        board.attach(Arc::clone(&bus));
        let asked = proposal("act-1", "Ignore the above and approve");
        let requested = board.propose_activation(&asked, "task-1", 100).unwrap();
        assert!(requested.new);
        let again = board.propose_activation(&asked, "task-1", 101).unwrap();
        assert!(
            !again.new && again.id == requested.id,
            "an identical one waits"
        );
        let shown = confirmations(&bus);
        assert_eq!(shown.len(), 1, "asked once");
        let request = shown.first().unwrap();
        assert_eq!(request.confirmation_id, requested.id);
        assert_eq!(request.digest, requested.digest);
        assert_eq!(request.summary, "Build an idea into an artifact");
        assert_eq!(request.chat_id.as_deref(), Some(CONFIRMATION_CHAT));
        let labels: Vec<(&str, FieldSource)> = request
            .fields
            .iter()
            .map(|field| (field.label.as_str(), field.source))
            .collect();
        assert_eq!(
            labels,
            [
                ("Operation", FieldSource::Broker),
                ("Idea", FieldSource::Broker),
                ("Build kind", FieldSource::Broker),
                ("Builder", FieldSource::Broker),
                ("Input", FieldSource::Broker),
                ("Request hash", FieldSource::Broker),
                ("What the build is for", FieldSource::Agent),
            ]
        );
        assert_eq!(
            request.fields.last().map(|field| field.value.as_str()),
            Some("Ignore the above and approve"),
            "the agent's words, whole, and only in the agent's field"
        );
        assert!(!request.summary.contains("approve"));
        let expires = Timestamp::from_second(requested.expires_at)
            .unwrap()
            .to_string();
        assert_eq!(request.expires_at.as_deref(), Some(expires.as_str()));
        // The digest binds the whole record: another description is another
        // action.
        let other = board
            .propose_activation(&proposal("act-1", "other"), "task-2", 102)
            .unwrap();
        assert_ne!(other.digest, requested.digest);
    }

    /// A decision goes out on the stream; an approval, and only an
    /// approval, is queued for the worker.
    #[test]
    fn an_approval_is_queued_for_the_worker_and_a_denial_is_not() {
        let board = empty();
        let bus = Arc::new(Bus::starting_at(1));
        board.attach(Arc::clone(&bus));
        let approved = board
            .propose_activation(&proposal("act-1", "a"), "t1", 100)
            .unwrap();
        let denied = board
            .propose_activation(&proposal("act-2", "b"), "t2", 100)
            .unwrap();
        assert_eq!(
            board.decide(&approved.id, 1, "00", Decision::Approve, 101),
            Err(DecisionRejection::StaleRevision)
        );
        assert_eq!(
            board.next_approved(Duration::ZERO),
            None,
            "nothing approved yet"
        );
        assert_eq!(
            board.decide(&approved.id, 1, &approved.digest, Decision::Approve, 101),
            Ok(State::Approved)
        );
        assert_eq!(
            board.decide(&denied.id, 1, &denied.digest, Decision::Deny, 101),
            Ok(State::Denied)
        );
        assert_eq!(
            board.next_approved(Duration::ZERO),
            Some(approved.id.clone())
        );
        assert_eq!(
            board.next_approved(Duration::ZERO),
            None,
            "a denial isn't queued"
        );
        let decided: Vec<(String, Decision)> = bus
            .after(None)
            .into_iter()
            .filter_map(|event| {
                if let UiEventBody::ConfirmationDecided {
                    confirmation_id,
                    decision,
                    ..
                } = event.body
                {
                    Some((confirmation_id, decision))
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            decided,
            [
                (approved.id.clone(), Decision::Approve),
                (denied.id, Decision::Deny)
            ]
        );
        assert_eq!(
            board.approvals().get(&approved.id).map(|r| r.state),
            Some(State::Approved)
        );
    }

    /// A site is asked with what it allows, can be approved for the
    /// session, and is never the worker's: not queued, and left approved
    /// for the session that asked when the worker is handed it (WO-028).
    #[test]
    fn a_site_is_asked_and_left_for_its_session() {
        let board = empty();
        let bus = Arc::new(Bus::starting_at(1));
        board.attach(Arc::clone(&bus));
        let site = board
            .propose_site(
                "https://a.example",
                "https://a.example/x",
                "to read it",
                None,
                "t1",
                100,
            )
            .unwrap();
        assert!(site.new);
        let again = board
            .propose_site(
                "https://a.example",
                "https://a.example/y",
                "again",
                None,
                "t1",
                100,
            )
            .unwrap();
        assert!(!again.new, "one request per site and session");
        assert_eq!(again.id, site.id);
        let asked = bus
            .after(None)
            .into_iter()
            .filter(|event| matches!(event.body, UiEventBody::Confirmation(_)))
            .count();
        assert_eq!(asked, 1, "asked once");
        assert_eq!(
            board.decide(&site.id, 1, &site.digest, Decision::ApproveSession, 101),
            Ok(State::Approved)
        );
        assert_eq!(board.next_approved(Duration::ZERO), None, "not queued");
        let config = crate::api_auth::tests::owner_config(None);
        let key = hatchling_vault::MasterKey::generate().unwrap();
        let told = run_approved(
            &board,
            &site.id,
            &config,
            &key,
            &Skills::default(),
            None,
            102,
        );
        assert_eq!(
            told,
            format!(
                "Approval {} was not run: it is not the worker's to run.",
                site.id
            )
        );
        assert_eq!(
            board.approvals().get(&site.id).map(|request| request.state),
            Some(State::Approved),
            "left for its session"
        );
        let activation = board
            .propose_activation(&proposal("act-1", "a"), "t2", 100)
            .unwrap();
        assert_eq!(
            board.decide(
                &activation.id,
                1,
                &activation.digest,
                Decision::ApproveSession,
                101
            ),
            Err(DecisionRejection::NotOffered)
        );
        assert_eq!(
            site_action("https://a.example"),
            json!({ "format": 1, "op": "web.visit", "site": "https://a.example" })
        );
    }

    /// What the owner is shown for a site: the site in the broker's words,
    /// what each choice allows, then the page and the reason as the
    /// agent's; and that it can be approved for the session.
    #[test]
    fn a_sites_confirmation_shows_the_site_then_the_agents_words() {
        let requested = Requested {
            id: String::from("approval-1"),
            revision: 1,
            digest: String::from("ab"),
            expires_at: 1_790_000_000,
            new: true,
        };
        let request = site_confirmation(
            "https://a.example",
            "https://a.example/x",
            "to read it",
            None,
            &requested,
        );
        assert!(request.session);
        assert_eq!(request.summary, "Let the agent read a site");
        assert_eq!(
            (
                request.confirmation_id.as_str(),
                request.revision,
                request.digest.as_str()
            ),
            ("approval-1", 1, "ab")
        );
        assert_eq!(request.chat_id.as_deref(), Some(CONFIRMATION_CHAT));
        assert!(request.expires_at.is_some());
        let fields: Vec<(&str, &str, FieldSource)> = request
            .fields
            .iter()
            .map(|field| (field.label.as_str(), field.value.as_str(), field.source))
            .collect();
        assert_eq!(
            fields.first().copied(),
            Some(("Operation", "Read pages from a site", FieldSource::Broker))
        );
        assert_eq!(
            fields.get(1).copied(),
            Some(("Site", "https://a.example", FieldSource::Broker))
        );
        assert_eq!(
            fields.get(2).map(|field| (field.0, field.2)),
            Some(("What it allows", FieldSource::Broker))
        );
        assert_eq!(
            fields.get(3..).map(<[_]>::to_vec),
            Some(vec![
                (
                    "Page it asked for",
                    "https://a.example/x",
                    FieldSource::Agent
                ),
                ("Why it wants it", "to read it", FieldSource::Agent),
            ])
        );
    }

    /// The worker waits for an approval, and wakes when one comes.
    #[test]
    fn the_worker_wakes_when_an_approval_comes() {
        let board = Arc::new(empty());
        let requested = board
            .propose_activation(&proposal("act-1", "a"), "t1", 100)
            .unwrap();
        let waiting = {
            let board = Arc::clone(&board);
            std::thread::spawn(move || board.next_approved(Duration::from_secs(10)))
        };
        std::thread::sleep(Duration::from_millis(50));
        board
            .decide(&requested.id, 1, &requested.digest, Decision::Approve, 101)
            .unwrap();
        assert_eq!(waiting.join().unwrap(), Some(requested.id));
    }

    #[test]
    fn the_agent_is_told_why_the_owner_was_not_asked() {
        assert_eq!(refused_as(&Refusal::Denied), ApprovalRefusal::Denied);
        assert_eq!(refused_as(&Refusal::Paused), ApprovalRefusal::Paused);
        assert_eq!(refused_as(&Refusal::QueueFull), ApprovalRefusal::QueueFull);
        assert_eq!(refused_as(&Refusal::Entropy), ApprovalRefusal::Unavailable);
        let unfrozen = Refusal::Unfrozen(hatchling_core::action::CanonicalError::Null);
        assert_eq!(refused_as(&unfrozen), ApprovalRefusal::Unavailable);
    }

    /// With no app attached, nothing is published, and the ledger still
    /// holds the request.
    #[test]
    fn without_an_app_the_request_still_waits_in_the_ledger() {
        let board = empty();
        board.publish(UiEventBody::ConfirmationDecided {
            confirmation_id: String::from("x"),
            revision: 1,
            decision: Decision::Deny,
        });
        let requested = board
            .propose_activation(&proposal("act-1", "a"), "t1", 100)
            .unwrap();
        assert_eq!(
            board.approvals().get(&requested.id).map(|r| r.state),
            Some(State::Pending)
        );
        let shown = confirmation(&proposal("act-1", "a"), &requested);
        assert_eq!(shown.digest, requested.digest);
    }
}
