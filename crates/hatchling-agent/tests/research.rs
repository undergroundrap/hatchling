//! Component tests: a research run from a `Ready` cell, with
//! every source fetched through the real broker's egress pipeline:
//! kernel peer gate, egress grants, and the allowlist.

pub(crate) mod common;

use common::{BrokerBehavior, FixtureBroker};
use hatchling_agent::{Cell, Config, Error, LogLevel, PeerUid, Ready};
use hatchling_loop::{FakeModel, Message, ModelAction, ToolCall};
use hatchling_research::{Approver, Bounds, Decision, Fetch, Plan, SourceUrl};
use serde_json::json;

/// A `Ready` cell against the fixture broker's socket.
fn ready_cell(broker: &FixtureBroker) -> Result<Cell<Ready>, Error> {
    let uid = rustix::process::getuid().as_raw();
    let config = Config {
        cell_id: "cell-research".parse()?,
        broker: hatchling_agent::BrokerAddress::Socket(broker.socket_path.parse()?),
        broker_uid: PeerUid::from_kernel(uid),
        log_level: LogLevel::Error,
        model_endpoint: None,
        model_name: None,
        model_max_steps: 10,
        model_context_window: None,
        model_reasoning: hatchling_core::ModelReasoning::ProviderDefault,
        run_subagent: false,
    };
    Cell::new(config)
        .check_preconditions()?
        .connect_broker()?
        .authorize()?
        .handshake()
}

/// Approves everything.
struct Yes;

impl Approver for Yes {
    fn review_plan(&self, _plan: &Plan) -> Decision {
        Decision::Approve
    }

    fn review_amendment(&self, _plan: &Plan, _url: &SourceUrl, _why: &str) -> Decision {
        Decision::Approve
    }
}

fn fetch(id: &str) -> ModelAction {
    ModelAction::ToolCalls(vec![ToolCall {
        name: String::from("fetch_source"),
        arguments: json!({ "source": id }),
    }])
}

fn text(reply: &str) -> ModelAction {
    ModelAction::Text(reply.to_owned())
}

#[test]
fn gathering_reaches_nothing_the_egress_allowlist_and_grants_refuse() {
    // Allowlisted and granted: example.com only. Not granted:
    // evil.example. Granted but not allowlisted: example.org.
    let broker = FixtureBroker::spawn(
        "research-egress",
        BrokerBehavior::Egress {
            allowlist: String::from("public:example.com:443"),
            grants: vec![
                String::from("example.com:443"),
                String::from("example.org:443"),
            ],
        },
    )
    .unwrap();
    let cell = ready_cell(&broker).unwrap();
    let model = FakeModel::new(vec![
        text(
            &json!({"strategy": "s", "sources": [
                {"url": "https://evil.example/steal", "why": "w"},
                {"url": "https://example.org/page", "why": "w"}
            ]})
            .to_string(),
        ),
        fetch("s1"),
        fetch("s2"),
        text("done"),
        text(
            &json!({"title": "t", "summary": "s", "findings": [
                {"claim": "c", "cites": [{"source": "s1", "depth": "primary"}]}
            ]})
            .to_string(),
        ),
    ]);
    let outcome = cell
        .run_research(&model, &Yes, Bounds::standard(), "Q?")
        .unwrap();
    let fetches: Vec<&Fetch> = outcome.gathered.records.iter().map(|r| &r.fetch).collect();
    assert_eq!(fetches, [&Fetch::Denied, &Fetch::Denied]);
    // Nothing was read, so synthesis saw no evidence, and the cite binds
    // to nothing.
    let synthesis = model.seen_histories().last().cloned().unwrap();
    assert_eq!(synthesis.len(), 2);
    assert!(matches!(synthesis.first(), Some(Message::System(_))));
    let inputs = outcome.inputs();
    let sources = &inputs.get(6).unwrap().1;
    assert!(
        sources.starts_with("- [s1] https://evil.example/steal: refused "),
        "{sources}"
    );
    assert!(
        sources.contains("\n- [s2] https://example.org/page: refused "),
        "{sources}"
    );
    assert_eq!(
        sources.matches(": denied by egress").count(),
        2,
        "{sources}"
    );
    assert!(
        inputs
            .get(1)
            .unwrap()
            .1
            .starts_with("Status: draft. No finding is bound to a source the run read.")
    );
}
