//! Agentic Frontier rules engine: pure, deterministic, IO-free.
//!
//! The world advances only through [`engine::apply`] on journaled
//! [`engine::Entry`] values, so replaying a journal reproduces the exact state.

// Large `json!` views (status, spectator state) exceed the default macro recursion depth.
#![recursion_limit = "256"]

pub mod careers;
pub mod chronicle;
pub mod cmd;
pub mod combat;
pub mod constitution;
pub mod contracts;
pub mod corp;
pub mod diplomacy;
pub mod economy;
pub mod engine;
pub mod events;
pub mod feedback;
pub mod founding;
pub mod frontier;
pub mod galaxy;
pub mod government;
pub mod history;
pub mod identity;
pub mod knowledge;
pub mod language;
pub mod market_log;
pub mod memory;
pub mod model;
pub mod nav;
pub mod npc;
pub mod owner;
pub mod path;
pub mod planet;
pub mod player;
pub mod public;
pub mod query;
pub mod rng;
pub mod rules;
pub mod social;
pub mod spectator;
pub mod stardock;
pub mod territory;
pub mod tick;
pub mod trade;
pub mod trade_loop;

/// Bump when rules *code* changes in a way that alters replay of old journals.
pub const ENGINE_VERSION: &str = "af-engine-1.51";

pub use cmd::Command;
pub use engine::{Actor, Entry, Event, Outcome, Refusal, apply};
pub use galaxy::{GenesisConfig, generate};
pub use model::World;
