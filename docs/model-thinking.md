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

`disabled` adds only `"reasoning":{"enabled":false}` to capability probes
and task requests. An endpoint that does not support this extension may
refuse it. There is no automatic retry, model substitution or thinking-mode
fallback. Text-only or malformed capability replies still stop the task.
Hatchling does not replay provider-specific reasoning history.

Scheduled runs and children receive the broker setting. The child-spawn
API has no thinking-mode override. This configures the ordinary client;
it is not broker enforcement against arbitrary requests from a hostile
cell. Destination grants, verified TLS, surrogate authentication and
redaction continue to apply independently.

The hosted-model guide in [WO-001](workorders/workorder-001-help-and-tested-quickstart.md)
will use this setting for the specific tested Nemotron example. Other models
may work with the provider default. This option does not require a paid plan
or another API key.
