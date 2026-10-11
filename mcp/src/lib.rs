//! Agentic Frontier server library (also used by the harness for in-process tests).

pub mod activity;
pub mod activity_seasons;
pub mod archive;
pub mod callsigns;
pub mod conduct;
pub mod documentation;
pub mod fleet;
pub mod game;
pub mod gateway;
pub mod history;
pub mod http;
pub mod prompts;
pub mod server;
pub mod site;
pub mod sysop_mcp;
pub mod tools;

use af_store::{Genesis, Store};

/// Open (or create) a persistent universe, as its writer, and return the restored world.
/// `loc` is a database file path or a `postgres://` URL. `adopt`: rebase a universe
/// recorded by another engine version onto this one. This blocks (the store clients
/// are synchronous): call it outside async code, e.g. in `spawn_blocking`.
pub fn open_world(loc: &str, seed: u64, sectors: u32, tick_ms: u64, adopt: bool) -> anyhow::Result<(af_core::World, Store, bool)> {
    if !af_store::pg::is_postgres(loc)
        && let Some(dir) = std::path::Path::new(loc).parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let store = Store::open_location(loc, af_store::Access::Writer)?;
    let created = if store.genesis()?.is_none() {
        let mut rules = af_core::rules::Rules::default();
        rules.tick_ms = tick_ms;
        store.init(&Genesis {
            seed,
            sectors,
            rules,
            created_unix_ms: game::now_ms(),
            engine: af_core::ENGINE_VERSION.into(),
        })?;
        true
    } else {
        false
    };
    if adopt && store.base()?.engine != af_core::ENGINE_VERSION {
        let base = store.adopt_universe()?;
        tracing::info!(
            "universe rebased onto engine {} at seq {} (history: {:?})",
            base.engine,
            base.seq,
            base.history
        );
    }
    let (world, replayed) = store.load_world()?;
    tracing::info!(
        "universe loaded: {} sectors, tick {}, seq {}, replayed {replayed} journal entries",
        world.sector_count(),
        world.tick,
        world.seq
    );
    Ok((world, store, created))
}
