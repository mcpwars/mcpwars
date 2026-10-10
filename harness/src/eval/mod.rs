//! Evaluation mode: controlled, repeatable episodes on top of the engine.
//!
//! See `docs/EVAL.md` for the contract. In short: one episode is one candidate
//! configuration on one scenario version, condition and seed, run in process on
//! a fresh world with a synthetic clock; every client goes through the same
//! `Gateway`, which enforces permissions and the per-wake tool budget; outcomes
//! are scored from engine state against fixed targets and scripted baselines.

pub mod api;
pub mod gateway;
pub mod policies;
pub mod report;
pub mod runner;
pub mod scenario;
pub mod score;
