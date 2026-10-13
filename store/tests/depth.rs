//! Deeply nested captain data survives every stored-state read: snapshot, boot (`load_world`),
//! `world_for_report`, `adopt_universe`, replay from the base and `verify`. A memory value at
//! depth 120 to 128, or a market claim at 124, is accepted at intake, but once it is wrapped in
//! the world or a journal entry it sits deeper than serde_json's default limit of 128.
//! Runs on the embedded file always, and on Postgres when `AF_TEST_POSTGRES_URL` is set.

use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::model::{PlayerId, PlayerKind};
use af_core::rules::Rules;
use af_core::{ENGINE_VERSION, World};
use af_store::private_archive::{ArchiveQuery, Capture};
use af_store::{Access, Genesis, MAX_STORED_DEPTH, Store, Table, from_slice_deep, json_depth, pg};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};

fn pg_url() -> Option<String> {
    std::env::var("AF_TEST_POSTGRES_URL").ok().filter(|s| !s.is_empty())
}

/// A fresh file location, plus a fresh Postgres schema when configured.
fn locations(name: &str) -> Vec<String> {
    let file = std::env::temp_dir().join(format!("af-store-depth-{name}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let mut out = vec![file.display().to_string()];
    if let Some(url) = pg_url() {
        let schema = format!("t_depth_{name}_{}", std::process::id());
        let mut c = pg::connect(&url).unwrap();
        c.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"))
            .unwrap();
        let sep = if url.contains('?') { '&' } else { '?' };
        out.push(format!("{url}{sep}options=-csearch_path%3D{schema}"));
    }
    out
}

fn genesis(engine: &str) -> Genesis {
    Genesis {
        seed: 9,
        sectors: 40,
        rules: Rules::default(),
        created_unix_ms: 0,
        engine: engine.into(),
    }
}

/// A JSON value with exactly `levels` nested arrays and objects (alternating).
fn nested(levels: usize) -> Value {
    let mut v = json!("bottom");
    for i in 0..levels {
        v = if i % 2 == 0 { json!([v]) } else { json!({ "d": v }) };
    }
    v
}

/// Apply and append one command like the server actor does (private rows and a checkpoint
/// with every entry). Returns whether the engine accepted it.
fn run(store: &Store, w: &mut World, actor: Actor, cmd: Command) -> bool {
    let e = Entry {
        seq: w.seq + 1,
        at_ms: (w.seq + 1) * 10,
        actor,
        cmd,
    };
    let before = Capture::before(w, &e);
    if apply(w, &e).is_err() {
        return false;
    }
    let rows = before.after(w, &e).unwrap();
    store.append_with_private(&e, Some(&w.state_hash()), &rows).unwrap();
    true
}

fn memory_save(key: &str, data: Value) -> Command {
    Command::Memory(af_core::memory::Args {
        action: af_core::memory::Action::Save,
        key: Some(key.into()),
        data: Some(data),
        expected_revision: None,
        ttl_ticks: None,
        path: None,
        include_archived: false,
        offset: None,
        limit: None,
    })
}

/// Register a captain, store memory values 120 to 128 levels deep (one as a JSON string, the
/// way some clients send it), fly to the nearest port and publish a market claim 124 deep.
fn play_deep(store: &Store, w: &mut World) -> PlayerId {
    assert!(run(
        store,
        w,
        Actor::System,
        Command::Register(RegisterArgs {
            name: "Zed".into(),
            kind: PlayerKind::Agent,
            token_hash: "t".into(),
            model: None,
            owner_token_hash: None,
            harness: None,
            autonomous: false,
            recovery_hash: None,
        }),
    ));
    let pid = w.players.values().find(|p| p.npc.is_none()).unwrap().id;
    for d in 120..=128 {
        assert!(run(store, w, Actor::Player(pid), memory_save(&format!("deep{d}"), nested(d))));
    }
    // A string holding JSON is parsed into a value (127 levels is the parser's default limit).
    let text = serde_json::to_string(&nested(127)).unwrap();
    assert!(run(store, w, Actor::Player(pid), memory_save("as_text", Value::String(text))));
    assert!(w.players[&pid].memory["as_text"].versions[0].data.is_array());
    // Nearest port, by breadth-first search over the lanes.
    let start = w.players[&pid].sector;
    let mut prev = BTreeMap::new();
    let mut queue = VecDeque::from([start]);
    let mut port = None;
    while let Some(s) = queue.pop_front() {
        if w.sector(s).unwrap().port.is_some() {
            port = Some(s);
            break;
        }
        for &n in &w.sector(s).unwrap().warps {
            if n != start && !prev.contains_key(&n) {
                prev.insert(n, s);
                queue.push_back(n);
            }
        }
    }
    let mut path = vec![port.expect("a reachable port")];
    while *path.last().unwrap() != start {
        path.push(prev[path.last().unwrap()]);
    }
    path.pop();
    for &next in path.iter().rev() {
        let warp = || Command::Warp(WarpArgs { destination_sector: next });
        let mut tries = 0;
        while !run(store, w, Actor::Player(pid), warp()) {
            assert!(run(store, w, Actor::System, Command::Tick));
            tries += 1;
            assert!(tries < 100, "warp to {next} kept failing");
        }
    }
    let report = Command::MarketReport(af_core::market_log::Args {
        action: af_core::market_log::Action::Report,
        corp_only: false,
        sectors: vec![],
        claim: Some(nested(124)),
        since_tick: None,
        after_id: None,
        limit: None,
    });
    assert!(run(store, w, Actor::Player(pid), report));
    assert!(run(store, w, Actor::System, Command::Tick));
    pid
}

/// What `af-server verify` checks: snapshot plus replay equals a full replay from the base.
fn verify(loc: &str) -> String {
    let store = Store::open_location(loc, Access::Snapshot).unwrap();
    let (restored, _) = store.load_world().unwrap();
    let full = store.replay_from_base().unwrap();
    let norm = |w: &World| {
        let mut c = w.clone();
        c.now_ms = 0;
        c.state_hash()
    };
    assert_eq!(norm(&restored), norm(&full));
    norm(&full)
}

#[test]
fn deep_memory_and_claims_survive_snapshot_boot_replay_and_verify() {
    for loc in locations("boot") {
        let store = Store::open_location(&loc, Access::Writer).unwrap();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        let pid = play_deep(&store, &mut w);
        store.save_snapshot(&w).unwrap();
        // The snapshot really is deeper than serde_json's default limit.
        let raw = store.backend().debug_get(Table::Snapshots, w.seq).unwrap().unwrap();
        assert!(json_depth(&raw) > 128, "{}", json_depth(&raw));
        assert!(serde_json::from_slice::<Value>(&raw).is_err(), "the default limit would refuse it");
        // A few entries after the snapshot are replayed on boot.
        assert!(run(&store, &mut w, Actor::Player(pid), memory_save("after", nested(126))));
        assert!(run(&store, &mut w, Actor::System, Command::Tick));
        let (loaded, replayed) = store.load_world().unwrap();
        assert_eq!(replayed, 2);
        assert_eq!(loaded.state_hash(), w.state_hash());
        assert_eq!(loaded.players[&pid].memory["deep128"].versions[0].data, nested(128));
        assert_eq!(loaded.market_reports.last().unwrap().claim, Some(nested(124)));
        assert_eq!(store.replay_from_base().unwrap().state_hash(), w.state_hash());
        let (report, exact) = store.world_for_report().unwrap();
        assert!(exact);
        assert_eq!(report.state_hash(), w.state_hash());
        // Journal reads: typed entries and the reverse search pages.
        let entries = store.entries_after(0).unwrap();
        assert_eq!(entries.len() as u64, w.seq);
        let page = store.journal_page(None, 1_000).unwrap();
        assert_eq!(page.len() as u64, w.seq);
        // The permanent private archive keeps the deep versions; the operator view parses them.
        let rows = store
            .private_archive(&ArchiveQuery {
                player: Some(u64::from(pid)),
                kind: Some("memory".into()),
                key: Some("deep128".into()),
                after_seq: 0,
                after_ordinal: 0,
                limit: 10,
            })
            .unwrap();
        assert!(!rows.is_empty());
        for r in &rows {
            from_slice_deep::<Value>(&r.data).unwrap();
        }
        drop(store);
        assert_eq!(verify(&loc), {
            let mut c = w.clone();
            c.now_ms = 0;
            c.state_hash()
        });
    }
}

#[test]
fn deep_state_is_adopted_by_a_new_engine_and_still_replays() {
    for loc in locations("adopt") {
        let store = Store::open_location(&loc, Access::Writer).unwrap();
        let mut w = store.init(&genesis("af-engine-0.9")).unwrap();
        let pid = play_deep(&store, &mut w);
        // The previous build writes a snapshot at the head when it hands over.
        store.save_snapshot(&w).unwrap();
        let base = store.adopt_universe().unwrap();
        assert_eq!(base.seq, w.seq);
        let (loaded, replayed) = store.load_world().unwrap();
        assert_eq!(replayed, 0);
        assert_eq!(loaded.state_hash(), w.state_hash());
        // The adopted base world is the deep snapshot; play on and replay from it.
        let mut w2 = loaded;
        assert!(run(&store, &mut w2, Actor::Player(pid), memory_save("later", nested(125))));
        assert_eq!(store.replay_from_base().unwrap().state_hash(), w2.state_hash());
        drop(store);
        verify(&loc);
    }
}

#[test]
fn private_backfill_reads_deep_journal_entries() {
    for loc in locations("backfill") {
        let store = Store::open_location(&loc, Access::Writer).unwrap();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play_deep(&store, &mut w);
        store.backfill_private(&w).unwrap();
    }
}

#[test]
fn journal_page_returns_a_raw_row_130_deep() {
    for loc in locations("page130") {
        let store = Store::open_location(&loc, Access::Writer).unwrap();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        assert!(run(&store, &mut w, Actor::System, Command::Tick));
        assert!(run(&store, &mut w, Actor::System, Command::Tick));
        // Rewrite entry 2 into a shape this engine no longer knows, nested 130 levels.
        let row = json!({"seq": 2, "at_ms": 20, "actor": "system", "cmd": {"type": "obsolete", "args": nested(128)}});
        let bytes = serde_json::to_vec(&row).unwrap();
        assert_eq!(json_depth(&bytes), 130);
        store.backend().debug_put(Table::Journal, 2, &bytes).unwrap();
        let page = store.journal_page(None, 10).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0], row);
    }
}

#[test]
fn the_reader_never_overflows_the_stack() {
    // The limit itself parses on a default 2 MiB thread, typed and untyped, in any build.
    let at_limit = serde_json::to_vec(&nested(MAX_STORED_DEPTH)).unwrap();
    let too_deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            assert_eq!(json_depth(&at_limit), MAX_STORED_DEPTH);
            let v: Value = from_slice_deep(&at_limit).unwrap();
            assert_eq!(v, nested(MAX_STORED_DEPTH));
            let wrapped = serde_json::to_vec(&json!({"versions": [{"data": v}]})).unwrap();
            let e = from_slice_deep::<Value>(&wrapped).unwrap_err();
            assert!(e.to_string().contains("deeper than the reader's limit"), "{e}");
            let e = from_slice_deep::<Value>(too_deep.as_bytes()).unwrap_err();
            assert!(e.to_string().contains("100000 levels"), "{e}");
            // Brackets inside strings, escaped quotes included, are not nesting.
            assert_eq!(json_depth(br#"{"a":"[[[{{\"[[","b":[1]}"#), 2);
        })
        .unwrap()
        .join()
        .unwrap();
}
