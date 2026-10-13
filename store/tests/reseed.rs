//! A relaunch at the store: a universe started from a reseeded world (`init_with_world`), and on
//! Postgres the move of the previous universe to a private schema and back
//! (`relocate_universe`, `restore_relocated`). The Postgres half runs when `AF_TEST_POSTGRES_URL`
//! is set (the CI gate sets it and `AF_REQUIRE_POSTGRES=1`).

use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::model::PlayerKind;
use af_core::reseed::{self, ReseedOpts};
use af_core::rules::Rules;
use af_core::{ENGINE_VERSION, World};
use af_store::{Access, ActivityRow, Genesis, Store, StoreError, pg};

fn pg_url() -> Option<String> {
    let url = std::env::var("AF_TEST_POSTGRES_URL").ok().filter(|s| !s.is_empty());
    if url.is_none() && std::env::var("AF_REQUIRE_POSTGRES").as_deref() == Ok("1") {
        panic!("AF_REQUIRE_POSTGRES=1 but AF_TEST_POSTGRES_URL is unset: the Postgres tests would be skipped");
    }
    url
}

fn file(name: &str) -> String {
    let f = std::env::temp_dir().join(format!("af-store-reseed-{name}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&f);
    f.display().to_string()
}

/// A Postgres location in a fresh schema of its own (dropped first, with `extra` schemas).
fn pg_loc(url: &str, schema: &str, extra: &[&str]) -> String {
    let mut c = pg::connect(url).unwrap();
    for s in extra.iter().chain([&schema]) {
        c.batch_execute(&format!("DROP SCHEMA IF EXISTS {s} CASCADE")).unwrap();
    }
    c.batch_execute(&format!("CREATE SCHEMA {schema}")).unwrap();
    with_schema(url, schema)
}

fn with_schema(url: &str, schema: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}options=-csearch_path%3D{schema}")
}

fn genesis(seed: u64, sectors: u32) -> Genesis {
    Genesis {
        seed,
        sectors,
        rules: Rules::default(),
        created_unix_ms: 0,
        engine: ENGINE_VERSION.into(),
    }
}

/// Append `cmd` through the store the way the server actor does (checkpoint every 7th).
fn run(store: &Store, w: &mut World, actor: Actor, cmd: Command) -> bool {
    let e = Entry {
        seq: w.seq + 1,
        at_ms: w.now_ms + 60_000,
        actor,
        cmd,
    };
    if apply(w, &e).is_err() {
        return false;
    }
    let cp = w.seq.is_multiple_of(7).then(|| w.state_hash());
    store.append(&e, cp.as_deref()).unwrap();
    true
}

fn register(store: &Store, w: &mut World, name: &str) {
    assert!(run(
        store,
        w,
        Actor::System,
        Command::Register(RegisterArgs {
            name: name.into(),
            kind: PlayerKind::Agent,
            token_hash: format!("tok-{name}"),
            model: None,
            owner_token_hash: None,
            harness: None,
            autonomous: false,
            recovery_hash: Some(format!("rec-{name}")),
        }),
    ));
}

/// Some history: two captains, ticks and moves.
fn play(store: &Store, w: &mut World, rounds: u32) {
    if w.players.values().all(|p| p.npc.is_some()) {
        register(store, w, "Zed");
        register(store, w, "Yara");
    }
    let pid = w.players.values().find(|p| p.npc.is_none()).unwrap().id;
    for i in 0..rounds {
        run(store, w, Actor::System, Command::Tick);
        let warps = w.sector(w.players[&pid].sector).unwrap().warps.clone();
        run(
            store,
            w,
            Actor::Player(pid),
            Command::Warp(WarpArgs {
                destination_sector: warps[i as usize % warps.len()],
            }),
        );
    }
}

fn provenance(old: &World) -> serde_json::Value {
    serde_json::json!({"source_seq": old.seq, "source_tick": old.tick, "source_hash": old.state_hash(), "captains_kept": reseed::kept_ids(old).len()})
}

/// Seed `store` from `old` and check what boot would see.
fn seed_into(store: &Store, old: &World) -> World {
    let new = reseed::reseed(old, &ReseedOpts::for_world(old));
    let g = Genesis {
        seed: new.seed,
        sectors: new.sector_count(),
        rules: new.rules.clone(),
        created_unix_ms: 1,
        engine: ENGINE_VERSION.into(),
    };
    store.init_with_world(&g, &new, &provenance(old)).unwrap();
    let (loaded, replayed) = store.load_world().unwrap();
    assert_eq!(replayed, 0);
    assert_eq!(loaded.state_hash(), new.state_hash());
    assert_eq!(store.reseed_provenance().unwrap().unwrap()["source_seq"], old.seq);
    new
}

#[test]
fn a_reseeded_store_boots_verifies_and_round_trips_through_export() {
    let (src, dst, copy) = (file("src"), file("dst"), file("copy"));
    let old = {
        let s = Store::open(&src).unwrap();
        let mut w = s.init(&genesis(4, 60)).unwrap();
        play(&s, &mut w, 40);
        w
    };
    let store = Store::open(&dst).unwrap();
    let new = seed_into(&store, &old);
    // Every captain keeps its credentials byte for byte.
    for id in reseed::kept_ids(&old) {
        let (o, n) = (&old.players[&id], &new.players[&id]);
        assert_eq!(
            (&o.token_hash, &o.recovery_hash, &o.owner_token_hash),
            (&n.token_hash, &n.recovery_hash, &n.owner_token_hash)
        );
    }
    // A second seed, or a seed over a universe made by genesis, is refused and changes nothing.
    assert!(matches!(
        store.init_with_world(&genesis(1, 60), &new, &serde_json::json!({})),
        Err(StoreError::NotEmpty)
    ));
    let other = Store::open(file("genesis")).unwrap();
    other.init(&genesis(2, 60)).unwrap();
    assert!(matches!(
        other.init_with_world(&genesis(1, 60), &new, &serde_json::json!({})),
        Err(StoreError::NotEmpty)
    ));
    // A world that is not at seq 0 is refused on an empty store.
    let empty = Store::open(file("empty")).unwrap();
    assert!(empty.init_with_world(&genesis(1, 60), &old, &serde_json::json!({})).is_err());
    assert!(empty.genesis().unwrap().is_none());
    // It plays on, replays from its base exactly, and copies with its provenance.
    let mut w = new.clone();
    play(&store, &mut w, 30);
    store.save_snapshot(&w).unwrap();
    play(&store, &mut w, 5);
    assert_eq!(store.replay_from_base().unwrap().state_hash(), w.state_hash());
    assert_eq!(store.load_world().unwrap().0.state_hash(), w.state_hash());
    let backup = Store::create_export(&copy).unwrap();
    store.copy_into(&backup).unwrap();
    assert_eq!(backup.load_world().unwrap().0.state_hash(), w.state_hash());
    assert_eq!(backup.replay_from_base().unwrap().state_hash(), w.state_hash());
    assert_eq!(backup.reseed_provenance().unwrap(), store.reseed_provenance().unwrap());
    // Relocation needs Postgres.
    assert!(store.relocate_universe("pre_relaunch").is_err());
    assert!(store.restore_relocated("pre_relaunch", true).is_err());
    for f in [src, dst, copy] {
        let _ = std::fs::remove_file(f);
    }
}

#[test]
fn archive_schema_names_are_checked() {
    for ok in ["pre_relaunch", "a", "archive_2026_10_13"] {
        assert!(pg::valid_archive_schema(ok).is_ok(), "{ok}");
    }
    for bad in [
        "",
        "Pre",
        "1abc",
        "pre-relaunch",
        "pre relaunch",
        "public",
        "pg_x",
        "x\"y",
        &"a".repeat(42),
    ] {
        assert!(pg::valid_archive_schema(bad).is_err(), "{bad}");
    }
}

#[test]
fn postgres_relocation_moves_the_universe_and_back() {
    let Some(url) = pg_url() else { return };
    let pid = std::process::id();
    let (schema, archive) = (format!("t_reloc_{pid}"), format!("a_reloc_{pid}"));
    let loc = pg_loc(&url, &schema, &[&archive]);
    let store = Store::open_location(&loc, Access::Writer).unwrap();
    let mut old = store.init(&genesis(6, 60)).unwrap();
    play(&store, &mut old, 30);
    store.save_snapshot(&old).unwrap();
    store
        .activity_append(&[ActivityRow {
            at_ms: 5,
            player: Some(1),
            data: b"{}".to_vec(),
        }])
        .unwrap();
    store
        .daily_replace(
            "2026-10-12",
            &[af_store::DailyRow {
                day: "2026-10-12".into(),
                player: 1,
                data: b"{}".to_vec(),
            }],
        )
        .unwrap();
    let max_id = |schema: &str| -> i64 {
        let mut c = pg::connect(&url).unwrap();
        c.query_one(&format!("SELECT COALESCE(MAX(id), 0) FROM {schema}.af_activity"), &[])
            .unwrap()
            .get(0)
    };
    let id_before = max_id(&schema);
    assert!(id_before > 0);

    // Move it out: the current schema is empty and stamped, the archive holds the old universe.
    let moved = store.relocate_universe(&archive).unwrap();
    assert_eq!((moved.from.as_str(), moved.to.as_str()), (schema.as_str(), archive.as_str()));
    assert_eq!(moved.tables.len(), 7);
    assert!(moved.tables.iter().any(|(t, n)| t == "af_journal" && *n == old.seq));
    assert!(store.genesis().unwrap().is_none());
    assert_eq!(store.last_seq().unwrap(), 0);
    assert_eq!(store.daily_days().unwrap(), Vec::<String>::new());
    assert!(store.backend().meta_get("schema_version").unwrap().is_some());
    {
        let archived = Store::open_location(&with_schema(&url, &archive), Access::Snapshot).unwrap();
        assert_eq!(archived.load_world().unwrap().0.state_hash(), old.state_hash());
        assert_eq!(archived.daily_days().unwrap(), vec!["2026-10-12".to_string()]);
    }
    // A second move into the same archive is refused and moves nothing.
    assert!(store.relocate_universe(&archive).is_err());

    // The new universe, on the same writer connection; then restoring needs force.
    let new = seed_into(&store, &old);
    let mut w = new.clone();
    play(&store, &mut w, 3);
    let err = store.restore_relocated(&archive, false).unwrap_err();
    assert!(err.to_string().contains("force"), "{err}");
    assert_eq!(
        store.load_world().unwrap().0.state_hash(),
        w.state_hash(),
        "a refused restore changes nothing"
    );
    let back = store.restore_relocated(&archive, true).unwrap();
    assert_eq!((back.from.as_str(), back.to.as_str()), (archive.as_str(), schema.as_str()));
    assert_eq!(store.load_world().unwrap().0.state_hash(), old.state_hash());
    assert!(store.reseed_provenance().unwrap().is_none());
    assert_eq!(store.daily_days().unwrap(), vec!["2026-10-12".to_string()]);
    // The archive schema is gone, and the activity id sequence came back with its table.
    let mut c = pg::connect(&url).unwrap();
    let n: i64 = c
        .query_one("SELECT COUNT(*) FROM pg_namespace WHERE nspname = $1", &[&archive])
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
    store
        .activity_append(&[ActivityRow {
            at_ms: 6,
            player: Some(1),
            data: b"{}".to_vec(),
        }])
        .unwrap();
    assert!(max_id(&schema) > id_before);
    // The restored universe keeps playing.
    play(&store, &mut old, 3);
    drop(store);
    let reopened = Store::open_location(&loc, Access::Writer).unwrap();
    assert_eq!(reopened.load_world().unwrap().0.state_hash(), old.state_hash());
    drop(reopened);
    c.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).unwrap();
}

#[test]
fn postgres_relocation_refuses_a_name_in_use_and_bad_names() {
    let Some(url) = pg_url() else { return };
    let pid = std::process::id();
    let (schema, archive) = (format!("t_relocx_{pid}"), format!("a_relocx_{pid}"));
    let loc = pg_loc(&url, &schema, &[&archive]);
    // An archive schema that already holds a universe's tables is refused.
    let mut c = pg::connect(&url).unwrap();
    c.batch_execute(&format!("CREATE SCHEMA {archive}; CREATE TABLE {archive}.af_meta (key TEXT)"))
        .unwrap();
    let store = Store::open_location(&loc, Access::Writer).unwrap();
    let mut w = store.init(&genesis(8, 60)).unwrap();
    play(&store, &mut w, 3);
    let err = store.relocate_universe(&archive).unwrap_err();
    assert!(err.to_string().contains("already holds"), "{err}");
    assert!(store.relocate_universe("Bad-Name").is_err());
    assert!(store.restore_relocated(&format!("nothing_{pid}"), true).is_err());
    assert_eq!(store.load_world().unwrap().0.state_hash(), w.state_hash());
    // A reader never relocates.
    let reader = Store::open_location(&loc, Access::Reader).unwrap();
    assert!(reader.relocate_universe(&format!("other_{pid}")).is_err());
    drop((store, reader));
    c.batch_execute(&format!(
        "DROP SCHEMA IF EXISTS {schema} CASCADE; DROP SCHEMA IF EXISTS {archive} CASCADE"
    ))
    .unwrap();
}
