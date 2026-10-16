//! The host's settings, shared by their reader and its help.

hatchling_core::declare_environment! {
    pub,
    HATCHLING_UI_SOCKET: ("required", "The broker's UI socket; its token is read from <socket>.token."),
    HATCHLING_HOST_UI: ("required", "Directory of the built frontend, as `npm run build` leaves it in frontend/dist."),
    HATCHLING_HOST_PORT: ("optional; default 7340", "Port on 127.0.0.1 to serve the UI on; 0 picks a free one."),
}
