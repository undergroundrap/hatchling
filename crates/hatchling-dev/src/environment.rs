//! Environment read by the development checks, also used by help.

hatchling_core::declare_environment! {
    pub,
    HOME: ("required for mutants", "Home directory containing the mutation scratch tree."),
    HATCHLING_CARGO: ("optional; default cargo", "Cargo executable used by the mutation-cache driver."),
    HATCHLING_REQUIRE_CELL: ("optional", "Set to 1 to require real-cell tests; part of mutation cache identity."),
}
