//! The owner's approvals (WO-027): frozen actions waiting for the owner's
//! decision, and the record of what became of each.
//!
//! A cell can propose an action that needs the owner's yes. The broker
//! freezes it first ([`hatchling_core::action`]): the record of everything
//! its effect depends on, and that record's digest. The ledger holds the
//! request to approve it, and moves it through these states only:
//!
//! ```text
//! pending -> approved -> started -> succeeded | failed | outcome_unknown
//!    \-> denied | expired | withdrawn (before it starts)
//! ```
//!
//! - **Only the owner decides** ([`Approvals::decide`]), by the request's
//!   id, its revision and the action's digest, all as shown. No session op
//!   reaches it.
//! - **Only the broker runs the action** ([`Approvals::claim`]): the claim
//!   from approved to started is atomic, so two clicks or two workers give
//!   one execution, and it is the stored action that runs. A claim dropped
//!   before its outcome is recorded leaves the request `outcome_unknown`,
//!   never to be run again: whether its effect happened isn't known.
//! - **Asking is bounded.** At most [`MAX_PENDING`] requests wait, one per
//!   task, each for [`DECIDE_SECS`], and at most [`MAX_REQUESTS_PER_HOUR`]
//!   are made in an hour. An identical pending proposal gets the request
//!   that's already waiting.
//! - **A denial sticks.** Its fingerprint (the effect, without request ids,
//!   timestamps or a rationale) is refused for the broker's life, however
//!   it's worded and from whichever cell, and every new request waits out
//!   [`DENIAL_COOLDOWN_SECS`].
//!
//! The ledger lives in the broker's memory: a restart withdraws every
//! request, which fails safe, and forgets denials, which a cell can't
//! cause (WO-027's notes).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use hatchling_core::action::{self, CanonicalError};
use hatchling_core::ui::{Decision, DecisionRejection};
use serde_json::Value;

/// Most requests waiting for the owner at once.
pub const MAX_PENDING: usize = 5;
/// Most requests one task has waiting at once.
pub const MAX_PENDING_PER_TASK: usize = 1;
/// How long the owner has to decide, in seconds.
pub const DECIDE_SECS: i64 = 15 * 60;
/// Most requests made in any hour.
pub const MAX_REQUESTS_PER_HOUR: usize = 20;
/// How long every new request waits after a denial, in seconds.
pub const DENIAL_COOLDOWN_SECS: i64 = 10 * 60;
/// Most decided requests kept, to answer a late decision with what
/// became of them.
pub const MAX_KEPT: usize = 256;
/// Every request id starts with this.
pub const REQUEST_PREFIX: &str = "approval-";

/// A frozen action, proposed for the owner's approval.
#[derive(Debug, Clone)]
pub struct Proposal<A> {
    /// The action as it will run: the broker's own record.
    pub run: A,
    /// The record the digest binds: everything the effect depends on.
    pub action: Value,
    /// The effect without ids, timestamps or rationale: what a denial
    /// refuses.
    pub fingerprint: Value,
    /// The task that proposed it, as the broker knows it.
    pub task: String,
}

/// Where a request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Waiting for the owner.
    Pending,
    /// Approved, waiting for a worker.
    Approved,
    /// A worker is running it.
    Started,
    /// It ran.
    Succeeded,
    /// It ran and failed, or couldn't run.
    Failed,
    /// It started, and whether its effect happened isn't known.
    OutcomeUnknown,
    /// The owner said no.
    Denied,
    /// The owner didn't decide in time.
    Expired,
    /// The broker took it back: what it would act on changed.
    Withdrawn,
}

/// A request to approve one frozen action.
#[derive(Debug, Clone)]
pub struct Request<A> {
    /// Its id.
    pub id: String,
    /// Its revision: a decision names the one it was shown.
    pub revision: u64,
    /// The digest of the action it would run.
    pub digest: String,
    /// The task that proposed it.
    pub task: String,
    /// When it lapses, in Unix seconds.
    pub expires_at: i64,
    /// Where it is.
    pub state: State,
    run: A,
    fingerprint: String,
}

/// What a proposal got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requested {
    /// The request's id.
    pub id: String,
    /// Its revision.
    pub revision: u64,
    /// The action's digest.
    pub digest: String,
    /// When it lapses.
    pub expires_at: i64,
    /// Whether it's new, so the owner is asked; an identical waiting
    /// request isn't asked twice.
    pub new: bool,
}

/// Why a proposal wasn't put to the owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// The owner denied this effect.
    #[error("the owner denied this action")]
    Denied,
    /// Requests are paused: a denial's cooldown, or the hourly limit.
    #[error("approval requests are paused")]
    Paused,
    /// Too many requests are waiting, for the owner or for this task.
    #[error("too many approval requests are waiting")]
    QueueFull,
    /// The action has no canonical form.
    #[error("the action can't be frozen: {0}")]
    Unfrozen(#[from] CanonicalError),
    /// No id could be made.
    #[error("no request id could be made")]
    Entropy,
}

/// The ledger: one per broker, shared by the owner's sockets and the
/// workers.
#[derive(Debug)]
pub struct Approvals<A> {
    inner: Mutex<Ledger<A>>,
}

#[derive(Debug)]
struct Ledger<A> {
    requests: BTreeMap<String, Request<A>>,
    /// Decided requests, oldest first, for pruning.
    decided: VecDeque<String>,
    denied: BTreeSet<String>,
    paused_until: i64,
    made: VecDeque<i64>,
}

impl<A: Clone> Default for Approvals<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Clone> Approvals<A> {
    /// An empty ledger.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(Ledger {
                requests: BTreeMap::new(),
                decided: VecDeque::new(),
                denied: BTreeSet::new(),
                paused_until: 0,
                made: VecDeque::new(),
            }),
        }
    }

    fn ledger(&self) -> MutexGuard<'_, Ledger<A>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Puts `proposal` to the owner at `now`, or says why not.
    ///
    /// # Errors
    ///
    /// [`Refusal`], before anything is recorded.
    pub fn propose(&self, proposal: Proposal<A>, now: i64) -> Result<Requested, Refusal> {
        let digest = action::digest(&proposal.action)?;
        let fingerprint = action::digest(&proposal.fingerprint)?;
        self.ledger().propose(proposal, digest, fingerprint, now)
    }

    /// The owner's `decision` on request `id`, at the `revision` and with
    /// the action `digest` they were shown, at `now`.
    ///
    /// # Errors
    ///
    /// [`DecisionRejection`], and nothing changes: no such request, a
    /// revision or digest other than the request's, a request that lapsed,
    /// or one already decided (the earlier decision stands).
    pub fn decide(
        &self,
        id: &str,
        revision: u64,
        digest: &str,
        decision: Decision,
        now: i64,
    ) -> Result<State, DecisionRejection> {
        self.ledger().decide(id, revision, digest, decision, now)
    }

    /// Claims approved request `id` for its one run: the action to run,
    /// and the claim that records how it went. `None` unless it is
    /// approved and unclaimed, so a second claim gets nothing.
    #[must_use]
    pub fn claim(&self, id: &str) -> Option<(A, Claim<'_, A>)> {
        let run = self.ledger().start(id)?;
        Some((
            run,
            Claim {
                approvals: self,
                id: id.to_owned(),
                done: false,
            },
        ))
    }

    /// Takes back request `id` while it waits or is approved and unclaimed:
    /// what it would act on changed. Whether it was taken back.
    pub fn withdraw(&self, id: &str) -> bool {
        self.ledger().withdraw(id)
    }

    /// Request `id`, as it stands.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Request<A>> {
        self.ledger().requests.get(id).cloned()
    }

    /// Records the end of request `id`'s run.
    fn finish(&self, id: &str, end: State) {
        self.ledger().finish(id, end);
    }
}

impl<A: Clone> Ledger<A> {
    /// [`Approvals::propose`], with the action's and the fingerprint's
    /// digests.
    fn propose(
        &mut self,
        proposal: Proposal<A>,
        digest: String,
        fingerprint: String,
        now: i64,
    ) -> Result<Requested, Refusal> {
        let ledger = self;
        ledger.expire(now);
        if ledger.denied.contains(&fingerprint) {
            return Err(Refusal::Denied);
        }
        if let Some(waiting) = ledger.requests.values().find(|request| {
            request.state == State::Pending
                && request.digest == digest
                && request.task == proposal.task
        }) {
            return Ok(Requested {
                id: waiting.id.clone(),
                revision: waiting.revision,
                digest,
                expires_at: waiting.expires_at,
                new: false,
            });
        }
        let pending: Vec<&Request<A>> = ledger
            .requests
            .values()
            .filter(|request| request.state == State::Pending)
            .collect();
        let of_task = pending
            .iter()
            .filter(|request| request.task == proposal.task)
            .count();
        if pending.len() >= MAX_PENDING || of_task >= MAX_PENDING_PER_TASK {
            return Err(Refusal::QueueFull);
        }
        while ledger
            .made
            .front()
            .is_some_and(|made| now.saturating_sub(*made) >= 3600)
        {
            ledger.made.pop_front();
        }
        if now < ledger.paused_until || ledger.made.len() >= MAX_REQUESTS_PER_HOUR {
            return Err(Refusal::Paused);
        }
        let id = crate::approval::random_hex(REQUEST_PREFIX).map_err(|_| Refusal::Entropy)?;
        let expires_at = now.saturating_add(DECIDE_SECS);
        ledger.made.push_back(now);
        ledger.requests.insert(
            id.clone(),
            Request {
                id: id.clone(),
                revision: 1,
                digest: digest.clone(),
                task: proposal.task,
                expires_at,
                state: State::Pending,
                run: proposal.run,
                fingerprint,
            },
        );
        Ok(Requested {
            id,
            revision: 1,
            digest,
            expires_at,
            new: true,
        })
    }

    /// [`Approvals::decide`].
    fn decide(
        &mut self,
        id: &str,
        revision: u64,
        digest: &str,
        decision: Decision,
        now: i64,
    ) -> Result<State, DecisionRejection> {
        let ledger = self;
        ledger.expire(now);
        let request = ledger
            .requests
            .get_mut(id)
            .ok_or(DecisionRejection::Unknown)?;
        match request.state {
            State::Pending => {}
            State::Expired => return Err(DecisionRejection::Expired),
            State::Approved
            | State::Started
            | State::Succeeded
            | State::Failed
            | State::OutcomeUnknown
            | State::Denied
            | State::Withdrawn => return Err(DecisionRejection::AlreadyDecided),
        }
        if request.revision != revision || request.digest != digest {
            return Err(DecisionRejection::StaleRevision);
        }
        let fingerprint = request.fingerprint.clone();
        let decided = match decision {
            Decision::Approve => State::Approved,
            Decision::Deny => State::Denied,
        };
        request.state = decided;
        if decided == State::Denied {
            ledger.denied.insert(fingerprint);
            ledger.paused_until = now.saturating_add(DENIAL_COOLDOWN_SECS);
        }
        ledger.keep_decided(id);
        Ok(decided)
    }

    /// Moves approved request `id` to started: its action, if it was
    /// approved and unclaimed.
    fn start(&mut self, id: &str) -> Option<A> {
        let request = self.requests.get_mut(id)?;
        if request.state != State::Approved {
            return None;
        }
        request.state = State::Started;
        Some(request.run.clone())
    }

    /// Moves started request `id` to `end`.
    fn finish(&mut self, id: &str, end: State) {
        if let Some(request) = self.requests.get_mut(id) {
            if request.state == State::Started {
                request.state = end;
            }
        }
    }

    /// [`Approvals::withdraw`].
    fn withdraw(&mut self, id: &str) -> bool {
        let Some(request) = self.requests.get_mut(id) else {
            return false;
        };
        if !matches!(request.state, State::Pending | State::Approved) {
            return false;
        }
        request.state = State::Withdrawn;
        self.keep_decided(id);
        true
    }

    /// Lapses every pending request past its time.
    fn expire(&mut self, now: i64) {
        let lapsed: Vec<String> = self
            .requests
            .values()
            .filter(|request| request.state == State::Pending && now >= request.expires_at)
            .map(|request| request.id.clone())
            .collect();
        for id in lapsed {
            if let Some(request) = self.requests.get_mut(&id) {
                request.state = State::Expired;
            }
            self.keep_decided(&id);
        }
    }

    /// Remembers `id` as decided, forgetting the oldest past [`MAX_KEPT`].
    fn keep_decided(&mut self, id: &str) {
        self.decided.push_back(id.to_owned());
        // One bounded drain, never a loop on the length: nothing here can
        // spin.
        let over = self.decided.len().saturating_sub(MAX_KEPT);
        let oldest: Vec<String> = self.decided.drain(..over).collect();
        for id in oldest {
            let running = self
                .requests
                .get(&id)
                .is_some_and(|request| matches!(request.state, State::Approved | State::Started));
            if !running {
                self.requests.remove(&id);
            }
        }
    }
}

/// One claimed run. [`Claim::succeeded`] or [`Claim::failed`] records how
/// it went; dropped unrecorded, it leaves the request `outcome_unknown`.
#[derive(Debug)]
pub struct Claim<'a, A: Clone> {
    approvals: &'a Approvals<A>,
    id: String,
    done: bool,
}

impl<A: Clone> Claim<'_, A> {
    /// The run's effect happened.
    pub fn succeeded(mut self) {
        self.done = true;
        self.approvals.finish(&self.id, State::Succeeded);
    }

    /// The run failed, and its effect is known not to have happened.
    pub fn failed(mut self) {
        self.done = true;
        self.approvals.finish(&self.id, State::Failed);
    }
}

impl<A: Clone> Drop for Claim<'_, A> {
    fn drop(&mut self) {
        if !self.done {
            self.approvals.finish(&self.id, State::OutcomeUnknown);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn proposal(effect: &str, task: &str) -> Proposal<String> {
        Proposal {
            run: format!("run {effect}"),
            action: json!({ "op": "activate", "effect": effect, "task": task }),
            fingerprint: json!({ "op": "activate", "effect": effect }),
            task: task.to_owned(),
        }
    }

    fn approve(
        ledger: &Approvals<String>,
        requested: &Requested,
        now: i64,
    ) -> Result<State, DecisionRejection> {
        ledger.decide(
            &requested.id,
            requested.revision,
            &requested.digest,
            Decision::Approve,
            now,
        )
    }

    #[test]
    fn a_request_moves_from_pending_to_approved_to_started_to_its_end() {
        let ledger = Approvals::new();
        let requested = ledger.propose(proposal("one", "t1"), 1000).unwrap();
        assert!(requested.new && requested.id.starts_with(REQUEST_PREFIX));
        assert_eq!(requested.revision, 1);
        assert_eq!(requested.expires_at, 1000 + DECIDE_SECS);
        assert_eq!(
            requested.digest,
            action::digest(&json!({ "op": "activate", "effect": "one", "task": "t1" })).unwrap()
        );
        assert_eq!(ledger.get(&requested.id).unwrap().state, State::Pending);
        assert!(
            ledger.claim(&requested.id).is_none(),
            "nothing runs unapproved"
        );
        assert_eq!(approve(&ledger, &requested, 1001), Ok(State::Approved));
        let (run, claim) = ledger.claim(&requested.id).unwrap();
        assert_eq!(run, "run one", "the stored action runs");
        assert_eq!(ledger.get(&requested.id).unwrap().state, State::Started);
        assert!(ledger.claim(&requested.id).is_none(), "one claim");
        claim.succeeded();
        assert_eq!(ledger.get(&requested.id).unwrap().state, State::Succeeded);

        let other = ledger.propose(proposal("two", "t2"), 1000).unwrap();
        approve(&ledger, &other, 1001).unwrap();
        let (_, claim) = ledger.claim(&other.id).unwrap();
        claim.failed();
        assert_eq!(ledger.get(&other.id).unwrap().state, State::Failed);
    }

    /// A claim dropped before its outcome is recorded (a worker that
    /// panicked) leaves the request `outcome_unknown`, never run again.
    #[test]
    fn a_claim_dropped_unrecorded_is_outcome_unknown_and_never_runs_again() {
        let ledger = Approvals::new();
        let requested = ledger.propose(proposal("one", "t1"), 0).unwrap();
        approve(&ledger, &requested, 1).unwrap();
        let claimed = ledger.claim(&requested.id);
        assert!(claimed.is_some());
        drop(claimed);
        assert_eq!(
            ledger.get(&requested.id).unwrap().state,
            State::OutcomeUnknown
        );
        assert!(ledger.claim(&requested.id).is_none());
        assert_eq!(
            approve(&ledger, &requested, 2),
            Err(DecisionRejection::AlreadyDecided)
        );
    }

    /// Two approvals racing, and two workers racing: one decision, one run.
    #[test]
    #[expect(
        clippy::needless_collect,
        reason = "both threads start before either is joined: that is the race"
    )]
    fn two_approvals_and_two_workers_give_one_run() {
        let ledger = std::sync::Arc::new(Approvals::new());
        let requested = ledger.propose(proposal("one", "t1"), 0).unwrap();
        let decisions: Vec<_> = (0..2)
            .map(|_| {
                let (ledger, requested) = (std::sync::Arc::clone(&ledger), requested.clone());
                std::thread::spawn(move || approve(&ledger, &requested, 1))
            })
            .collect();
        let decided: Vec<_> = decisions.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(
            decided.iter().filter(|d| d.is_ok()).count(),
            1,
            "{decided:?}"
        );
        assert!(decided.contains(&Err(DecisionRejection::AlreadyDecided)));
        let runs: Vec<_> = (0..2)
            .map(|_| {
                let (ledger, id) = (std::sync::Arc::clone(&ledger), requested.id.clone());
                std::thread::spawn(move || {
                    ledger.claim(&id).map(|(run, claim)| {
                        claim.succeeded();
                        run
                    })
                })
            })
            .collect();
        let ran: Vec<_> = runs.into_iter().filter_map(|t| t.join().unwrap()).collect();
        assert_eq!(ran, ["run one"]);
    }

    #[test]
    fn a_decision_names_the_request_revision_and_digest_it_was_shown() {
        let ledger = Approvals::new();
        let requested = ledger.propose(proposal("one", "t1"), 0).unwrap();
        let decide = |id: &str, revision, digest: &str| {
            ledger.decide(id, revision, digest, Decision::Approve, 1)
        };
        assert_eq!(
            decide("approval-none", 1, &requested.digest),
            Err(DecisionRejection::Unknown)
        );
        assert_eq!(
            decide(&requested.id, 2, &requested.digest),
            Err(DecisionRejection::StaleRevision)
        );
        assert_eq!(
            decide(&requested.id, 1, "00"),
            Err(DecisionRejection::StaleRevision)
        );
        assert_eq!(
            ledger.get(&requested.id).unwrap().state,
            State::Pending,
            "nothing changed"
        );
        assert_eq!(
            decide(&requested.id, 1, &requested.digest),
            Ok(State::Approved)
        );
        assert_eq!(
            decide(&requested.id, 1, &requested.digest),
            Err(DecisionRejection::AlreadyDecided)
        );
    }

    #[test]
    fn an_undecided_request_lapses_and_can_no_longer_be_approved() {
        let ledger = Approvals::new();
        let requested = ledger.propose(proposal("one", "t1"), 0).unwrap();
        assert_eq!(
            approve(&ledger, &requested, DECIDE_SECS),
            Err(DecisionRejection::Expired)
        );
        assert_eq!(ledger.get(&requested.id).unwrap().state, State::Expired);
        assert!(ledger.claim(&requested.id).is_none());
        // Lapsed, it no longer holds the task's place.
        assert!(
            ledger
                .propose(proposal("two", "t1"), DECIDE_SECS)
                .unwrap()
                .new
        );
    }

    #[test]
    fn asking_is_bounded_and_an_identical_proposal_gets_the_waiting_request() {
        let ledger = Approvals::new();
        let first = ledger.propose(proposal("one", "t1"), 0).unwrap();
        let again = ledger.propose(proposal("one", "t1"), 5).unwrap();
        assert_eq!((again.id.as_str(), again.new), (first.id.as_str(), false));
        assert_eq!(again.digest, first.digest);
        assert_eq!(
            ledger.propose(proposal("two", "t1"), 5),
            Err(Refusal::QueueFull),
            "one per task"
        );
        for n in 2..=MAX_PENDING {
            ledger.propose(proposal("x", &format!("t{n}")), 5).unwrap();
        }
        assert_eq!(
            ledger.propose(proposal("x", "t-last"), 5),
            Err(Refusal::QueueFull),
            "five in all"
        );
        // Decided ones free their places; the hour's limit still holds.
        let ledger = Approvals::new();
        for n in 0..MAX_REQUESTS_PER_HOUR {
            let requested = ledger.propose(proposal(&format!("e{n}"), "t"), 10).unwrap();
            approve(&ledger, &requested, 11).unwrap();
        }
        assert_eq!(
            ledger.propose(proposal("late", "t"), 3000),
            Err(Refusal::Paused)
        );
        assert!(
            ledger.propose(proposal("late", "t"), 3610).unwrap().new,
            "an hour on"
        );
    }

    /// A denied effect stays denied, reworded or from another task or a
    /// fresh cell, and every new request waits out the cooldown.
    #[test]
    fn a_denial_sticks_and_pauses_every_new_request() {
        let ledger = Approvals::new();
        let requested = ledger.propose(proposal("one", "t1"), 0).unwrap();
        assert_eq!(
            ledger.decide(&requested.id, 1, &requested.digest, Decision::Deny, 10),
            Ok(State::Denied)
        );
        assert!(ledger.claim(&requested.id).is_none());
        let reworded = Proposal {
            action: json!({ "op": "activate", "effect": "one", "task": "t9", "why": "please" }),
            ..proposal("one", "t9")
        };
        assert_eq!(
            ledger.propose(reworded, 10 + DENIAL_COOLDOWN_SECS + 1),
            Err(Refusal::Denied)
        );
        assert_eq!(
            ledger.propose(proposal("two", "t2"), 20),
            Err(Refusal::Paused)
        );
        assert!(
            ledger
                .propose(proposal("two", "t2"), 10 + DENIAL_COOLDOWN_SECS)
                .unwrap()
                .new
        );
    }

    #[test]
    fn a_withdrawn_request_neither_decides_nor_runs() {
        let ledger = Approvals::new();
        let waiting = ledger.propose(proposal("one", "t1"), 0).unwrap();
        assert!(ledger.withdraw(&waiting.id));
        assert_eq!(
            approve(&ledger, &waiting, 1),
            Err(DecisionRejection::AlreadyDecided)
        );
        let approved = ledger.propose(proposal("two", "t2"), 0).unwrap();
        approve(&ledger, &approved, 1).unwrap();
        assert!(ledger.withdraw(&approved.id), "approved but unclaimed");
        assert!(ledger.claim(&approved.id).is_none());
        let running = ledger.propose(proposal("three", "t3"), 0).unwrap();
        approve(&ledger, &running, 1).unwrap();
        let (_, claim) = ledger.claim(&running.id).unwrap();
        assert!(
            !ledger.withdraw(&running.id),
            "a started run isn't taken back"
        );
        claim.succeeded();
        assert!(!ledger.withdraw("approval-none"));
        assert_eq!(ledger.get(&running.id).unwrap().state, State::Succeeded);
    }

    /// The ledger's own steps: lapsing what is due, and keeping decided
    /// requests to their bound, oldest forgotten first.
    #[test]
    fn the_ledger_lapses_what_is_due_and_forgets_the_oldest_decided() {
        let approvals = Approvals::new();
        let first = approvals.propose(proposal("one", "t1"), 0).unwrap();
        let second = approvals.propose(proposal("two", "t2"), 100).unwrap();
        approvals.ledger().expire(DECIDE_SECS - 1);
        assert_eq!(approvals.get(&first.id).unwrap().state, State::Pending);
        approvals.ledger().expire(DECIDE_SECS);
        assert_eq!(approvals.get(&first.id).unwrap().state, State::Expired);
        assert_eq!(approvals.get(&second.id).unwrap().state, State::Pending);
        let mut ledger = approvals.ledger();
        assert_eq!(ledger.decided.len(), 1);
        for n in 0..MAX_KEPT {
            ledger.keep_decided(&format!("approval-x{n}"));
        }
        let (kept, forgot_first, kept_second) = (
            ledger.decided.len(),
            !ledger.requests.contains_key(&first.id),
            ledger.requests.contains_key(&second.id),
        );
        drop(ledger);
        assert_eq!(kept, MAX_KEPT);
        assert!(forgot_first, "the oldest decided is forgotten");
        assert!(kept_second, "a pending one isn't decided");
        assert_eq!(approvals.get(&second.id).unwrap().state, State::Pending);
    }

    #[test]
    fn an_action_with_no_canonical_form_is_refused() {
        let ledger: Approvals<String> = Approvals::default();
        let unfrozen = Proposal {
            action: json!({ "amount": 1.5 }),
            ..proposal("one", "t1")
        };
        assert!(matches!(
            ledger.propose(unfrozen, 0),
            Err(Refusal::Unfrozen(_))
        ));
        let no_fingerprint = Proposal {
            fingerprint: json!(null),
            ..proposal("one", "t1")
        };
        assert!(matches!(
            ledger.propose(no_fingerprint, 0),
            Err(Refusal::Unfrozen(_))
        ));
    }

    /// Decided requests are kept to a bound; a running one never goes.
    #[test]
    fn decided_requests_are_kept_to_a_bound_and_a_running_one_never_goes() {
        let ledger = Approvals::new();
        let running = ledger.propose(proposal("keep", "t-run"), 0).unwrap();
        approve(&ledger, &running, 1).unwrap();
        let mut ids = Vec::new();
        for n in 0..=MAX_KEPT {
            let mut ledger_time = i64::try_from(n).unwrap() * 3600;
            ledger_time += 1;
            let requested = ledger
                .propose(proposal(&format!("e{n}"), "t"), ledger_time)
                .unwrap();
            ledger
                .decide(
                    &requested.id,
                    1,
                    &requested.digest,
                    Decision::Approve,
                    ledger_time,
                )
                .unwrap();
            let (_, claim) = ledger.claim(&requested.id).unwrap();
            claim.succeeded();
            ids.push(requested.id);
        }
        assert!(
            ledger.get(ids.first().unwrap()).is_none(),
            "the oldest decided is forgotten"
        );
        assert!(ledger.get(ids.last().unwrap()).is_some());
        assert_eq!(
            ledger.get(&running.id).unwrap().state,
            State::Approved,
            "kept while unclaimed"
        );
    }
}
