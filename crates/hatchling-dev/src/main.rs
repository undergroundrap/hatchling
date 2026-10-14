//! `hatchling-dev`: the development-only audit binary.
//!
//! Thin dispatcher over the checks in the library: `audit` runs the
//! fast checks (also covered by the crate's unit tests, so they gate
//! every push), `mutants` and `coverage` run the slow scheduled checks,
//! `mutants-cached` runs the mutation gate through the content-hash
//! cache. Exit code is the contract: 0 means every check passed.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    hatchling_dev::cli::main(&args)
}
