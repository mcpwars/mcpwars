//! Library half of the harness: scripted bots, tool callers, and the
//! evaluation runner (`eval`). The `af-harness` binary adds the live
//! orchestrator and the CLI.

pub mod bots;
pub mod client;
pub mod eval;
