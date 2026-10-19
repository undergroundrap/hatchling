//! The model provider's transport over the broker-mediated egress path.
//!
//! [`EgressTransport`] implements [`Transport`][hatchling_model::Transport]
//! by minting nothing itself: the surrogate is minted once per task by
//! [`Cell::run_task`][crate::Cell::run_task] and handed in here. The
//! request goes out as a broker egress op; the proxy validates the
//! surrogate, swaps in the real credential for the destination, and
//! sets the `Authorization` header itself — the agent is forbidden from
//! setting that header, so the transport rejects any request that tries
//! (fail fast, instead of sending a request the broker will deny).

use std::os::unix::net::UnixStream;

use hatchling_loop::LoopResult;
use hatchling_model::{
    ChatCompletionsProvider, Endpoint, ProviderConfig, Redactor, Transport, TransportError,
};

use crate::cell::{Cell, Ready};
use crate::egress::{EgressResponse, egress_request};
use crate::{Config, Error};

/// TTL for the model-endpoint surrogate, minted once per task. One mint
/// covers the whole task; per-turn minting would widen the surrogate's
/// exposure window for no benefit.
pub const MODEL_SURROGATE_TTL_SECS: u64 = 3600;

/// [`Transport`][hatchling_model::Transport] over the agent's broker
/// session.
///
/// The provider was built for exactly one endpoint; a request naming
/// any other destination is a host wiring bug and fails closed.
pub struct EgressTransport<'s> {
    stream: &'s UnixStream,
    surrogate: String,
    destination: String,
}

impl<'s> EgressTransport<'s> {
    /// Builds the transport over the broker session.
    ///
    /// `surrogate` must have been minted for `destination`; the broker
    /// re-checks that binding on every request.
    #[must_use]
    pub const fn new(stream: &'s UnixStream, surrogate: String, destination: String) -> Self {
        Self {
            stream,
            surrogate,
            destination,
        }
    }

    /// Maps the egress outcome onto the transport vocabulary. Pure, so
    /// unit tests pin every branch without a broker.
    fn translate(result: Result<EgressResponse, Error>) -> Result<Vec<u8>, TransportError> {
        match result {
            Ok(response) if (200..=299).contains(&response.status) => Ok(response.body),
            Ok(response) => Err(TransportError::HttpStatus(response.status)),
            Err(error) => Err(TransportError::Failed(error.to_string())),
        }
    }
}

impl Transport for EgressTransport<'_> {
    fn post(
        &self,
        destination: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<Vec<u8>, TransportError> {
        if destination != self.destination {
            return Err(TransportError::Failed(String::from(
                "transport destination mismatch",
            )));
        }
        if headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        {
            return Err(TransportError::Failed(String::from(
                "the egress proxy sets the Authorization header; the agent must not",
            )));
        }
        let headers: Vec<(String, String)> = headers
            .iter()
            .map(|&(name, value)| (name.to_owned(), value.to_owned()))
            .collect();
        let result = egress_request(
            self.stream,
            &self.surrogate,
            "POST",
            destination,
            path,
            headers,
            body,
        );
        Self::translate(result)
    }
}

impl Cell<Ready> {
    /// Runs one agent task against the configured model provider.
    ///
    /// Mints a surrogate for the endpoint's destination (derived from
    /// the endpoint URL itself — a single source of truth, so the mint
    /// can never disagree with where the requests go), builds the
    /// provider over the [`EgressTransport`], runs the capability
    /// handshake, and drives the loop to a final answer.
    ///
    /// The same steps hold for a cloud endpoint and a local one
    ///: the provider redacts every secret the run fetches
    /// before any request leaves, and the handshake runs every session,
    /// before the task, so an unreachable endpoint or a model that does
    /// not call tools (say, one swapped in behind the endpoint since the
    /// last session) is refused loudly before the loop starts.
    /// The provider sends no `Authorization` header: the egress proxy
    /// swaps the surrogate for the real credential bound to the
    /// destination and sets the header — the only way it gets set.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidConfig`] for an unusable endpoint, model
    /// name, or declared context window, propagates surrogate-mint
    /// failures, returns [`Error::ModelRefused`] when the capability
    /// handshake fails, and wraps loop failures in [`Error::LoopFailed`].
    pub fn run_task(
        &self,
        kind: hatchling_loop::PromptKind,
        task: String,
        provider: ProviderConfig,
        max_steps: u32,
    ) -> Result<LoopResult, Error> {
        let endpoint =
            Endpoint::parse(&provider.endpoint).map_err(|error| Error::InvalidConfig {
                key: "HATCHLING_MODEL_ENDPOINT",
                reason: error.to_string(),
            })?;
        if provider.model.is_empty() {
            return Err(Error::InvalidConfig {
                key: "HATCHLING_MODEL_NAME",
                reason: String::from("model name must be non-empty"),
            });
        }
        hatchling_model::check_context_window(provider.context_window).map_err(|error| {
            Error::InvalidConfig {
                key: "HATCHLING_MODEL_CONTEXT_WINDOW",
                reason: error.to_string(),
            }
        })?;
        let surrogate =
            self.egress_mint_surrogate(endpoint.destination(), MODEL_SURROGATE_TTL_SECS)?;
        let transport =
            EgressTransport::new(self.broker(), surrogate, endpoint.destination().to_owned());
        let redactor = Redactor::default();
        let provider = ChatCompletionsProvider::new(provider, transport)
            .map_err(|error| Error::InvalidConfig {
                key: "HATCHLING_MODEL_ENDPOINT",
                reason: error.to_string(),
            })?
            .with_reasoning(self.config().model_reasoning)
            .with_redactor(redactor.clone());
        provider
            .verify_capabilities()
            .map_err(|error| Error::ModelRefused(error.to_string()))?;
        let model_grant = hatchling_core::ScheduleGrant::Egress {
            destination: endpoint.destination().to_owned(),
        };
        self.run_loop_delegating(
            kind,
            &provider,
            task,
            hatchling_loop::LoopBudget { max_steps },
            &redactor,
            Some(model_grant),
        )
        .map_err(|error| {
            if let hatchling_loop::Error::BudgetExhausted { steps } = error {
                Error::LoopBudgetExhausted { steps }
            } else {
                Error::LoopFailed(error.to_string())
            }
        })
    }

    /// Runs one channel-driven turn: receive → loop → reply.
    ///
    /// Receives a message on `channel` via the broker session, runs the
    /// loop with the message text as the user task (below the system
    /// prompt in precedence, per the instruction hierarchy), and sends
    /// the final answer back as a conversational reply.
    ///
    /// Returns `Ok(None)` when no message is waiting. The caller drives
    /// the polling loop; this is one turn.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] on channel failure, model failure, or loop
    /// failure. A state-changing reply is never sent from here: the
    /// loop's answer is always a conversational reply. If the agent
    /// needs to send a state-changing message, it uses
    /// [`ChannelClient::send_state_changing`] with a rail approval —
    /// that path is explicit, not the default turn.
    pub fn run_channel_turn(
        &self,
        channel: &crate::ChannelClient<'_>,
        provider: ProviderConfig,
        max_steps: u32,
    ) -> Result<Option<String>, Error> {
        let turn = Self::channel_turn(channel, |task| {
            self.run_task(hatchling_loop::PromptKind::Chat, task, provider, max_steps)
        })?;
        Ok(turn.map(|result| result.answer))
    }

    /// Runs one channel-driven turn against a caller-supplied `model`.
    ///
    /// The same turn as [`run_channel_turn`](Self::run_channel_turn) —
    /// receive, loop with the message text as the user task, send the
    /// answer as a conversational reply — but the model is the caller's
    /// instead of the configured chat-completions provider, and the full
    /// [`LoopResult`](hatchling_loop::LoopResult) comes back. For hosts
    /// that bring their own model, and for tests that must see the
    /// history the channel text produced.
    ///
    /// # Errors
    ///
    /// As [`run_channel_turn`](Self::run_channel_turn), minus provider
    /// configuration.
    pub fn run_channel_turn_with_model(
        &self,
        channel: &crate::ChannelClient<'_>,
        model: &dyn hatchling_loop::Model,
        max_steps: u32,
    ) -> Result<Option<LoopResult>, Error> {
        Self::channel_turn(channel, |task| {
            self.run_loop(
                hatchling_loop::PromptKind::Chat,
                model,
                task,
                hatchling_loop::LoopBudget { max_steps },
            )
            .map_err(|error| Error::LoopFailed(error.to_string()))
        })
    }

    /// The channel turn both entry points share: receive → `run` → reply.
    fn channel_turn(
        channel: &crate::ChannelClient<'_>,
        run: impl FnOnce(String) -> Result<LoopResult, Error>,
    ) -> Result<Option<LoopResult>, Error> {
        let Some(msg) = channel.receive()? else {
            return Ok(None);
        };
        // The message text enters the loop as the user task: below the
        // system prompt, above tool output. The hierarchy holds by
        // construction — the loop places System first, then User.
        let result = run(msg.text)?;
        channel.send_reply(result.answer.clone())?;
        Ok(Some(result))
    }
}

/// Runs the `run` subcommand's full flow as library code.
///
/// The binary stays thin: this joins the task arguments, loads the
/// config, assembles the provider config, runs the cell lifecycle,
/// and returns the final answer. The caller prints the answer to
/// stdout; logs go to stderr so the answer stays machine-readable.
///
/// Nothing partial is produced: either the final answer or an error.
///
/// # Errors
///
/// Returns [`Error::Usage`] for an empty task,
/// [`Error::MissingConfig`] for absent model configuration,
/// [`Error::InvalidConfig`] for an unusable endpoint or model
/// name, and propagates mint and loop failures.
pub fn run_command(task_args: &[String]) -> Result<String, Error> {
    let task = task_args.join(" ");
    if task.trim().is_empty() {
        return Err(Error::Usage(String::from(
            "usage: hatchling-agent run \"task\"",
        )));
    }
    let config = Config::from_env()?;
    let provider = ProviderConfig {
        endpoint: config
            .model_endpoint
            .clone()
            .ok_or(Error::MissingConfig("HATCHLING_MODEL_ENDPOINT"))?,
        model: config
            .model_name
            .clone()
            .ok_or(Error::MissingConfig("HATCHLING_MODEL_NAME"))?,
        // No bearer: the egress proxy swaps the minted surrogate for
        // the real credential and sets the Authorization header — the
        // only way it gets set.
        bearer: None,
        context_window: config.model_context_window,
    };
    let max_steps = config.model_max_steps;
    let config_subagent = config.run_subagent;
    let prompt = prompt_for(&config);
    // Best-effort: a test harness calling this twice must not panic
    // on the second init.
    let _ = tracing_subscriber::fmt()
        .with_max_level(config.log_level.to_tracing())
        .with_writer(std::io::stderr)
        .try_init();
    let cell = Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()?;
    // Only the broker's run launcher starts `run`: a scheduled or hook
    // run or a subagent, which is background work, or a chat turn,
    // which answers the user.
    let outcome = cell.run_task(prompt, task, provider, max_steps);
    if config_subagent {
        return subagent_report(outcome);
    }
    Ok(outcome?.answer)
}

/// The prompt a `run` speaks with: the chat prompt when the broker says
/// it answers the user's message, the worker's for background work.
#[must_use]
pub const fn prompt_for(config: &Config) -> hatchling_loop::PromptKind {
    if config.run_chat {
        hatchling_loop::PromptKind::Chat
    } else {
        hatchling_loop::PromptKind::Worker
    }
}

/// What a child run prints: how it ended, as a typed envelope.
///
/// Its parent's broker reads it: an answer, or a spent budget. Any other
/// failure fails the run as usual, and the broker records the cell's
/// failure.
///
/// # Errors
///
/// The run's own error, for anything but a spent budget.
pub fn subagent_report(outcome: Result<LoopResult, Error>) -> Result<String, Error> {
    let envelope = match outcome {
        Ok(result) => hatchling_core::ChildEnvelope::Done {
            verdict: hatchling_core::ChildVerdict::Ok,
            answer: result.answer,
            steps: result.steps,
        },
        Err(Error::LoopBudgetExhausted { steps }) => hatchling_core::ChildEnvelope::Failed {
            failure: hatchling_core::ChildFailure::BudgetExhausted,
            steps,
        },
        Err(error) => return Err(error),
    };
    serde_json::to_string(&envelope).map_err(|error| Error::SubagentProtocol {
        reason: format!("cannot encode the child's report: {error}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn response(status: u16) -> EgressResponse {
        EgressResponse {
            status,
            headers: Vec::new(),
            body: b"reply".to_vec(),
        }
    }

    #[test]
    fn a_chat_turn_speaks_with_the_chat_prompt_and_background_work_the_workers() {
        let mut config = Config {
            cell_id: "cell-01".parse().unwrap(),
            broker: crate::BrokerAddress::Socket("/tmp/broker.sock".parse().unwrap()),
            broker_uid: hatchling_core::PeerUid::from_kernel(0),
            log_level: hatchling_core::LogLevel::Info,
            model_endpoint: None,
            model_name: None,
            model_max_steps: 10,
            model_context_window: None,
            model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
            run_subagent: false,
            run_chat: false,
            run_history: Vec::new(),
        };
        assert_eq!(prompt_for(&config), hatchling_loop::PromptKind::Worker);
        config.run_chat = true;
        assert_eq!(prompt_for(&config), hatchling_loop::PromptKind::Chat);
    }

    #[test]
    fn translate_accepts_2xx_and_returns_body() {
        for status in [200, 201, 299] {
            let out = EgressTransport::translate(Ok(response(status))).unwrap();
            assert_eq!(out, b"reply");
        }
    }

    #[test]
    fn translate_rejects_non_2xx_with_status() {
        for status in [199, 300, 404, 500] {
            let error = EgressTransport::translate(Ok(response(status))).unwrap_err();
            assert!(
                matches!(error, TransportError::HttpStatus(got) if got == status),
                "status {status} should surface as HttpStatus"
            );
        }
    }

    #[test]
    fn translate_maps_egress_errors_to_failed() {
        let error = EgressTransport::translate(Err(Error::EgressFailed)).unwrap_err();
        assert!(
            matches!(error, TransportError::Failed(reason) if reason.contains("egress backend failed"))
        );
    }

    /// Builds a transport over a stream pair whose far end is never
    /// read: the fail-fast branches return before any I/O, so the
    /// owned end just needs to stay alive for the test. The read
    /// timeout keeps a regression that reaches I/O (e.g. an inverted
    /// guard) failing in seconds instead of hanging the suite.
    fn idle_transport(stream: &UnixStream) -> EgressTransport<'_> {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        EgressTransport::new(
            stream,
            String::from("surrogate"),
            String::from("models.example.com:443"),
        )
    }

    #[test]
    fn run_command_rejects_empty_task_before_touching_config() {
        for args in [vec![], vec![String::from("   ")]] {
            let error = run_command(&args).unwrap_err();
            assert!(
                matches!(error, Error::Usage(_)),
                "empty task must fail with Usage, got {error:?}"
            );
        }
    }

    #[test]
    fn post_rejects_destination_mismatch_without_io() {
        let (stream, _held) = UnixStream::pair().unwrap();
        let transport = idle_transport(&stream);
        let error = transport
            .post("other.example.com:443", "/v1/chat/completions", &[], b"{}")
            .unwrap_err();
        assert!(matches!(error, TransportError::Failed(reason) if reason.contains("mismatch")));
    }

    #[test]
    fn post_rejects_authorization_header_without_io() {
        let (stream, _held) = UnixStream::pair().unwrap();
        let transport = idle_transport(&stream);
        let error = transport
            .post(
                "models.example.com:443",
                "/v1/chat/completions",
                &[("Authorization", "Bearer x")],
                b"{}",
            )
            .unwrap_err();
        assert!(
            matches!(error, TransportError::Failed(reason) if reason.contains("Authorization"))
        );
    }

    #[test]
    fn a_child_reports_how_it_ended_in_its_envelope() {
        let done = subagent_report(Ok(LoopResult {
            answer: String::from("a"),
            history: Vec::new(),
            steps: 2,
        }))
        .unwrap();
        let done_wire = r#"{"status":"done","verdict":"ok","answer":"a","steps":2}"#;
        assert_eq!(done, done_wire);
        let spent = subagent_report(Err(Error::LoopBudgetExhausted { steps: 4 })).unwrap();
        let spent_wire = r#"{"status":"failed","failure":"budget_exhausted","steps":4}"#;
        assert_eq!(spent, spent_wire);
        let other = subagent_report(Err(Error::LoopFailed(String::from("x")))).unwrap_err();
        assert!(
            matches!(&other, Error::LoopFailed(reason) if reason == "x"),
            "{other:?}"
        );
    }
}
