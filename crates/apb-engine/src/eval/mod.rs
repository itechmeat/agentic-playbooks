//! Playbook eval suites (0.24.0): the checks of one repetition and the
//! stored results with their configuration key and comparison. The case
//! format and its validation live in `apb_core::eval`; the runner that
//! materializes fixtures and starts runs is `apb eval` in the CLI.

pub mod checks;
pub mod store;
