# Model thinking mode

Hatchling uses an OpenAI-compatible chat-completions API and checks tool
calling before every task. Some endpoints need thinking disabled to return
the required tool call. Choose this explicitly for an endpoint you tested:

```sh
# Set on the broker before starting scheduled runs or subagents:
export HATCHLING_SCHEDULER_MODEL_REASONING=disabled
# For an agent process you run directly:
export HATCHLING_MODEL_REASONING=disabled
```

Both settings accept exactly `provider-default` or `disabled`. Unset means
`provider-default`: Hatchling adds no reasoning parameter and the endpoint
chooses its behavior. Invalid values fail during configuration parsing,
even when scheduling is off. Help lists these settings from the declarations
the configuration readers use.

`disabled` adds only `"reasoning":{"enabled":false,"effort":"none"}` to
capability probes and task requests: the two names an endpoint may know
"off" by, in the one field. OpenRouter documents `effort` and takes
`enabled`; Ollama's OpenAI-style endpoint heeds only `effort`. An endpoint
that does not support this extension may refuse it. There is no model
substitution. Text-only or malformed capability replies still stop the
task. Hatchling does not replay provider-specific reasoning history.

## When a model's answer goes into its thinking

Some thinking models, on some servers, now and then put their whole
answer where their thinking goes and leave the answer itself empty.
`qwen3.5:9b` on Ollama did it on every answer that followed a tool's
result, on the machine it was measured on (2026-11, Ollama 0.40.13). The
chat used to say the model "answered with nothing".

Hatchling doesn't take a model's thinking for its answer. When a task
reply has thinking and nothing else, no text and no tool call, it asks
the same thing again with thinking off, and keeps thinking off for the
rest of that run. The next run starts as you set it. This happens only
with `provider-default`: with `disabled`, thinking is off from the first
request and there is nothing left to turn off.

If the model's server refuses a request with thinking off, thinking is
left as it was for the rest of the run, and the chat says the model
thought about its answer and gave none. The same is said when the server
takes the request and the model only thinks all the same. The capability
probe never changes the mode.

To save the one request each run spends finding this out, set `disabled`
for a model that does it.

Scheduled runs and children receive the broker setting. The child-spawn
API has no thinking-mode override. This configures the ordinary client;
it is not broker enforcement against arbitrary requests from a hostile
cell. Destination grants, verified TLS, surrogate authentication and
redaction continue to apply independently.

The hosted-model guide in [WO-001](workorders/workorder-001-help-and-tested-quickstart.md)
will use this setting for the specific tested Nemotron example. Other models
may work with the provider default. This option does not require a paid plan
or another API key.
