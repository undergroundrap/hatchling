//! Settings read by the agent; the broker supplies them inside its cell.

hatchling_core::declare_environment! {
    pub,
    HATCHLING_CELL_ID: ("required", "Identity supplied by the broker."),
    HATCHLING_BROKER_SOCKET: ("required without a connection", "Absolute broker socket path inside the cell."),
    HATCHLING_CONNECTION_FD: ("set by the cell", "Inherited descriptor of the cell's own broker connection; used instead of the socket."),
    HATCHLING_BROKER_UID: ("required", "Expected kernel uid of the broker peer."),
    HATCHLING_LOG_LEVEL: ("optional; default info", "Log verbosity: error, warn, info, debug or trace."),
    HATCHLING_MODEL_ENDPOINT: ("required for run", "OpenAI-compatible API base URL, reached through the broker."),
    HATCHLING_MODEL_NAME: ("required for run", "Provider's exact model identifier."),
    HATCHLING_MODEL_REASONING: ("optional; default provider-default", "Thinking mode: provider-default or disabled; endpoint support required."),
    HATCHLING_MODEL_MAX_STEPS: ("optional; default 10", "Maximum agent-loop steps."),
    HATCHLING_MODEL_CONTEXT_WINDOW: ("optional", "Declared model context window in tokens."),
    HATCHLING_RUN_SUBAGENT: ("optional; default 0", "Broker-supplied subagent mode, 0 or 1."),
    HATCHLING_RUN_CHAT: ("optional; default 0", "Broker-supplied: the run answers the user's chat message, 0 or 1."),
}
