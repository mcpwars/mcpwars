//! Agentic Frontier server library (also used by the harness for in-process tests).

pub mod activity;
pub mod activity_seasons;
pub mod archive;
pub mod backup;
pub mod build_info;
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

/// The environment variable that lets a server create a new universe on Postgres.
pub const CREATE_UNIVERSE_ENV: &str = "AF_CREATE_UNIVERSE";

/// Whether `AF_CREATE_UNIVERSE=1` is set (see `open_world`).
pub fn create_allowed() -> bool {
    std::env::var(CREATE_UNIVERSE_ENV).as_deref() == Ok("1")
}

/// Open (or create) a persistent universe, as its writer, and return the restored world.
/// `loc` is a database file path or a `postgres://` URL. `adopt`: rebase a universe
/// recorded by another engine version onto this one. This blocks (the store clients
/// are synchronous): call it outside async code, e.g. in `spawn_blocking`.
///
/// A database file with no universe gets a new one. A Postgres schema with no universe gets
/// one only when `AF_CREATE_UNIVERSE=1` is set: an empty schema there is far more often a wrong
/// `DATABASE_URL` or a universe that was moved away (a relaunch in progress or cut short) than
/// a wish for a new galaxy, and a server must never quietly start one in its place.
pub fn open_world(loc: &str, seed: u64, sectors: u32, tick_ms: u64, adopt: bool) -> anyhow::Result<(af_core::World, Store, bool)> {
    open_world_with(loc, seed, sectors, tick_ms, adopt, create_allowed())
}

/// `open_world` with the Postgres creation switch given (`create_on_postgres`) instead of read
/// from the environment.
pub fn open_world_with(
    loc: &str,
    seed: u64,
    sectors: u32,
    tick_ms: u64,
    adopt: bool,
    create_on_postgres: bool,
) -> anyhow::Result<(af_core::World, Store, bool)> {
    if !af_store::pg::is_postgres(loc)
        && let Some(dir) = std::path::Path::new(loc).parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir)?;
    }
    let store = Store::open_location(loc, af_store::Access::Writer)?;
    let created = if store.genesis()?.is_none() {
        if store.is_postgres() && !create_on_postgres {
            anyhow::bail!(
                "{} holds no universe, and a server creates one on Postgres only when {CREATE_UNIVERSE_ENV}=1 is set. \
                 Check DATABASE_URL; to restore a universe use `af-server import` (or `af-server reseed --rollback` after a relaunch); \
                 to start a new one on purpose set {CREATE_UNIVERSE_ENV}=1",
                store.describe()
            );
        }
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
    tracing::info!(
        "stored JSON depth: deepest read {} levels (stored-state reader limit {}; requests keep serde_json's 128)",
        af_store::deepest_read(),
        af_store::MAX_STORED_DEPTH
    );
    Ok((world, store, created))
}
