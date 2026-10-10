//! Store behaviour, run against every backend: the embedded file always, and
//! Postgres when `AF_TEST_POSTGRES_URL` points at a disposable database (each
//! test gets its own schema there). For example:
//!
//! ```sh
//! docker run -d --name af-pg -e POSTGRES_PASSWORD=pw -p 55432:5432 postgres:17
//! AF_TEST_POSTGRES_URL=postgres://postgres:pw@127.0.0.1:55432/postgres cargo test -p af-store
//! ```

use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::model::PlayerKind;
use af_core::rules::Rules;
use af_core::{ENGINE_VERSION, World};
use af_store::{Access, Genesis, Store, StoreError, Table, pg};

/// Where a fresh, empty universe can be opened (repeatedly).
#[derive(Clone)]
struct Loc(String);

impl Loc {
    fn open(&self) -> Store {
        Store::open_location(&self.0, Access::Writer).unwrap()
    }
}

fn pg_url() -> Option<String> {
    std::env::var("AF_TEST_POSTGRES_URL").ok().filter(|s| !s.is_empty())
}

/// A fresh file location, plus a fresh Postgres schema when configured.
fn locations(name: &str) -> Vec<Loc> {
    let file = std::env::temp_dir().join(format!("af-store-{name}-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let mut out = vec![Loc(file.display().to_string())];
    if let Some(url) = pg_url() {
        let schema = format!("t_{name}_{}", std::process::id());
        let mut c = pg::connect(&url).unwrap();
        c.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"))
            .unwrap();
        let sep = if url.contains('?') { '&' } else { '?' };
        out.push(Loc(format!("{url}{sep}options=-csearch_path%3D{schema}")));
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

/// Play `n` rounds through the store like the server actor does.
fn play(store: &Store, w: &mut World, rounds: u32) {
    let mut seq = w.seq;
    let mut run = |w: &mut World, actor: Actor, cmd: Command| {
        let e = Entry {
            seq: seq + 1,
            at_ms: (seq + 1) * 10,
            actor,
            cmd,
        };
        if apply(w, &e).is_ok() {
            seq += 1;
            let cp = seq.is_multiple_of(7).then(|| w.state_hash());
            store.append(&e, cp.as_deref()).unwrap();
        }
    };
    if w.players.values().all(|p| p.npc.is_some()) {
        run(
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
        );
    }
    let pid = w.players.values().find(|p| p.npc.is_none()).unwrap().id;
    for i in 0..rounds {
        run(w, Actor::System, Command::Tick);
        let here = w.players[&pid].sector;
        let warps = w.sector(here).unwrap().warps.clone();
        run(
            w,
            Actor::Player(pid),
            Command::Warp(WarpArgs {
                destination_sector: warps[i as usize % warps.len()],
            }),
        );
    }
}

#[test]
fn journal_snapshot_checkpoints_and_replay_agree() {
    for loc in locations("agree") {
        let store = loc.open();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play(&store, &mut w, 100);
        store.save_snapshot(&w).unwrap();
        play(&store, &mut w, 100);
        let (loaded, replayed) = store.load_world().unwrap();
        assert!(replayed > 0 && replayed < 250, "{}", loc.0);
        assert_eq!(loaded.state_hash(), w.state_hash());
        assert_eq!(store.replay_from_base().unwrap().state_hash(), w.state_hash());
        // Out-of-order appends are refused.
        let e = store.entries_after(0).unwrap().remove(3);
        assert!(matches!(store.append(&e, None), Err(StoreError::OutOfOrder { .. })));
        drop(store);
        // Reopening keeps the schema version and still loads.
        let store = loc.open();
        assert_eq!(store.load_world().unwrap().0.state_hash(), w.state_hash());
    }
}

#[test]
fn long_feedback_survives_a_snapshot_and_reload_exactly() {
    for loc in locations("feedback") {
        let store = loc.open();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play(&store, &mut w, 3);
        let pid = w.players.values().find(|p| p.npc.is_none()).unwrap().id;
        let mut text = String::from("  \tleading whitespace\r\n");
        while text.chars().count() < 15_000 {
            text.push_str("Ελληνικά 中文 العربية 🚀👩‍🚀\ttab\r\n");
        }
        text.push_str("end  \n ");
        let e = Entry {
            seq: w.seq + 1,
            at_ms: (w.seq + 1) * 10,
            actor: Actor::Player(pid),
            cmd: Command::Feedback(af_core::feedback::FeedbackArgs {
                about: "/join".into(),
                text: text.clone(),
                model: None,
                harness: None,
            }),
        };
        apply(&mut w, &e).unwrap();
        store.append(&e, None).unwrap();
        store.save_snapshot(&w).unwrap();
        play(&store, &mut w, 2);
        drop(store);
        let store = loc.open();
        let (loaded, _) = store.load_world().unwrap();
        assert_eq!(loaded.state_hash(), w.state_hash(), "{}", loc.0);
        assert_eq!(loaded.feedback[0].text.as_bytes(), text.as_bytes(), "{}", loc.0);
        assert_eq!(store.replay_from_base().unwrap().feedback[0].text, text);
    }
}

#[test]
fn tampered_journal_reports_first_divergent_checkpoint() {
    for loc in locations("tamper") {
        let store = loc.open();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play(&store, &mut w, 60);
        // Rewrite the registration's token hash: replay still succeeds but the world differs.
        let raw = store.backend().debug_get(Table::Journal, 1).unwrap().unwrap();
        let mut e: Entry = serde_json::from_slice(&raw).unwrap();
        if let Command::Register(a) = &mut e.cmd {
            a.token_hash = "tampered".into();
        }
        store
            .backend()
            .debug_put(Table::Journal, 1, &serde_json::to_vec(&e).unwrap())
            .unwrap();
        match store.replay_from_base() {
            Err(StoreError::Diverged { seq, .. }) => assert_eq!(seq, 7, "first checkpoint after the tampered entry"),
            other => panic!("expected divergence, got {other:?}"),
        }
    }
}

#[test]
fn refuses_unversioned_and_newer_stores() {
    for loc in locations("versions") {
        drop(loc.open());
        // A pre-release store: a genesis but no schema version.
        set_meta_raw(&loc.0, "schema_version", None);
        set_meta_raw(&loc.0, "genesis", Some(&serde_json::to_vec(&genesis("old")).unwrap()));
        assert!(matches!(Store::open_location(&loc.0, Access::Writer), Err(StoreError::Unversioned)));
        // A store from a newer build.
        set_meta_raw(&loc.0, "schema_version", Some(b"99"));
        assert!(matches!(
            Store::open_location(&loc.0, Access::Writer),
            Err(StoreError::NewerFile { found: 99, .. })
        ));
    }
}

/// Set or delete a meta key without the schema check (simulates another build's store).
fn set_meta_raw(loc: &str, key: &str, value: Option<&[u8]>) {
    if pg::is_postgres(loc) {
        let mut c = pg::connect(loc).unwrap();
        c.execute("DELETE FROM af_meta WHERE key = $1", &[&key]).unwrap();
        if let Some(v) = value {
            c.execute("INSERT INTO af_meta (key, value) VALUES ($1, $2)", &[&key, &v]).unwrap();
        }
    } else {
        let db = redb::Database::create(loc).unwrap();
        let tx = db.begin_write().unwrap();
        {
            let mut t = tx.open_table(redb::TableDefinition::<&str, &[u8]>::new("meta")).unwrap();
            t.remove(key).unwrap();
            if let Some(v) = value {
                t.insert(key, v).unwrap();
            }
        }
        tx.commit().unwrap();
    }
}

#[test]
fn engine_mismatch_is_refused_until_adopted_at_head() {
    for loc in locations("adopt") {
        let store = loc.open();
        let mut w = store.init(&genesis("af-engine-0.9")).unwrap();
        play(&store, &mut w, 30);
        assert!(matches!(store.load_world(), Err(StoreError::EngineMismatch { .. })));
        assert!(matches!(store.adopt_universe(), Err(StoreError::NotAtHead { .. })));
        store.save_snapshot(&w).unwrap();
        // The new engine may serialize the world differently (a new rules field),
        // so the old snapshot's stored hash no longer matches: adoption must cope.
        let raw = store.backend().debug_get(Table::Snapshots, w.seq).unwrap().unwrap();
        let mut row: serde_json::Value = serde_json::from_slice(&raw).unwrap();
        row["hash"] = "hash-from-an-older-schema".into();
        store
            .backend()
            .debug_put(Table::Snapshots, w.seq, &serde_json::to_vec(&row).unwrap())
            .unwrap();
        let base = store.adopt_universe().unwrap();
        assert_eq!(base.seq, w.seq);
        assert_eq!(base.history, vec![(0, "af-engine-0.9".to_string())]);
        let (loaded, replayed) = store.load_world().unwrap();
        assert_eq!(replayed, 0);
        assert_eq!(loaded.state_hash(), w.state_hash());
        // Play on under the new engine; verification replays from the new base only.
        let mut w2 = loaded;
        play(&store, &mut w2, 20);
        assert_eq!(store.replay_from_base().unwrap().state_hash(), w2.state_hash());
    }
}

#[test]
fn copies_between_backends_and_backs_up_consistently() {
    for loc in locations("copy") {
        let store = loc.open();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play(&store, &mut w, 80);
        store.save_snapshot(&w).unwrap();
        play(&store, &mut w, 40);
        // Backup (the portable file format) restores to the same world.
        let backup = std::env::temp_dir().join(format!("af-store-copy-backup-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&backup);
        let job = store.detached_backup();
        assert_eq!(job.is_some(), store.is_postgres());
        match job {
            Some(job) => job(&backup).unwrap(),
            None => store.backup_to(&backup).unwrap(),
        };
        let restored = Store::open(&backup).unwrap();
        assert_eq!(restored.load_world().unwrap().0.state_hash(), w.state_hash());
        assert_eq!(restored.replay_from_base().unwrap().state_hash(), w.state_hash());
        // Copying into a fresh store of every kind (an import) round-trips too.
        for dest in locations("copy_dest") {
            let d = dest.open();
            assert_eq!(restored.copy_into(&d).unwrap(), store.journal_len().unwrap());
            assert!(matches!(restored.copy_into(&d), Err(StoreError::NotEmpty)));
            assert_eq!(d.load_world().unwrap().0.state_hash(), w.state_hash());
            assert_eq!(d.replay_from_base().unwrap().state_hash(), w.state_hash());
        }
        let _ = std::fs::remove_file(&backup);
    }
}

#[test]
fn postgres_has_one_writer_and_hands_over_on_request() {
    let Some(_) = pg_url() else { return };
    let loc = locations("writer").pop().unwrap();
    let first = loc.open();
    first.init(&genesis(ENGINE_VERSION)).unwrap();
    // A second writer is refused; readers are fine but cannot write.
    assert!(matches!(Store::open_location(&loc.0, Access::Writer), Err(StoreError::Busy(_))));
    let reader = Store::open_location(&loc.0, Access::Reader).unwrap();
    assert!(reader.genesis().unwrap().is_some());
    assert!(reader.save_snapshot(&reader.load_world().unwrap().0).is_err());
    // The writer listens; a successor asks; the writer hears it.
    let url = loc.0.clone();
    let listener = std::thread::spawn(move || pg::wait_for_handoff(&url).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(300));
    pg::request_handoff(&loc.0).unwrap();
    assert!(listener.join().unwrap(), "handoff request received");
    // Releasing the connection releases the lock.
    drop(first);
    let second = Store::open_location(&loc.0, Access::Writer).unwrap();
    assert!(second.load_world().is_ok());
}

#[test]
fn activity_log_is_kept_beside_the_universe_not_in_it() {
    for loc in locations("activity") {
        let store = loc.open();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play(&store, &mut w, 20);
        let row = |at_ms, player: Option<u64>, d: &str| af_store::ActivityRow {
            at_ms,
            player,
            data: d.as_bytes().to_vec(),
        };
        store
            .activity_append(&[row(100, Some(1), "a"), row(200, None, "b"), row(200, Some(2), "c")])
            .unwrap();
        store.activity_append(&[row(300, Some(1), "d")]).unwrap();
        let all = store.activity(None, 0, 10).unwrap();
        assert_eq!(all.len(), 4, "{}", loc.0);
        assert_eq!(all[0].data, b"d", "newest first");
        let mine: Vec<_> = store.activity(Some(1), 0, 10).unwrap().into_iter().map(|r| r.data).collect();
        assert_eq!(mine, vec![b"d".to_vec(), b"a".to_vec()]);
        assert_eq!(store.activity(None, 150, 10).unwrap().len(), 3);
        assert_eq!(store.activity(None, 0, 2).unwrap().len(), 2);
        assert_eq!(store.activity(None, 200, 10).unwrap()[2].player, None);
        assert_eq!(store.activity_prune(250).unwrap(), 3);
        assert_eq!(store.activity(None, 0, 10).unwrap().len(), 1);
        // Telemetry never touches the universe: it still loads and replays to the same hash.
        assert_eq!(store.load_world().unwrap().0.state_hash(), w.state_hash());
        assert_eq!(store.replay_from_base().unwrap().state_hash(), w.state_hash());
        // On Postgres the server writes the log on a connection of its own (one multi-row
        // insert); the writer's connection sees the same rows.
        match store.activity_log() {
            None => assert!(!pg::is_postgres(&loc.0)),
            Some(mut log) => {
                log.append(&[row(400, Some(3), "e"), row(500, None, "f")]).unwrap();
                log.append(&[]).unwrap();
                assert_eq!(store.activity(None, 350, 10).unwrap().len(), 2);
                assert_eq!(log.query(Some(3), 0, 10).unwrap()[0].data, b"e");
                assert_eq!(log.prune(450).unwrap(), 2);
                assert_eq!(log.query(None, 0, 10).unwrap().len(), 1);
            }
        }
    }
}

#[test]
fn daily_activity_is_kept_forever_replaced_by_day_and_exported() {
    for loc in locations("daily") {
        let store = loc.open();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play(&store, &mut w, 10);
        let raw = |at_ms, player: Option<u64>| af_store::ActivityRow {
            at_ms,
            player,
            data: b"{}".to_vec(),
        };
        let daily = |day: &str, player: u64, d: &str| af_store::DailyRow {
            day: day.into(),
            player,
            data: d.as_bytes().to_vec(),
        };
        // Raw rows by half-open range, oldest first; the oldest row's time.
        assert_eq!(store.activity_oldest().unwrap(), None, "{}", loc.0);
        store
            .activity_append(&[raw(300, Some(1)), raw(100, None), raw(200, Some(2)), raw(400, Some(1))])
            .unwrap();
        assert_eq!(store.activity_oldest().unwrap(), Some(100));
        let between: Vec<u64> = store.activity_between(100, 400).unwrap().iter().map(|r| r.at_ms).collect();
        assert_eq!(between, vec![100, 200, 300]);
        assert_eq!(store.activity_between(100, 400).unwrap()[0].player, None);
        assert!(store.activity_between(400, 400).unwrap().is_empty());

        store
            .daily_replace("2026-10-01", &[daily("2026-10-01", 2, "b"), daily("2026-10-01", 0, "anon")])
            .unwrap();
        store.daily_replace("2026-10-02", &[daily("2026-10-02", 2, "c")]).unwrap();
        store.daily_replace("2026-09-30", &[daily("2026-09-30", 7, "a")]).unwrap();
        assert_eq!(store.daily_days().unwrap(), vec!["2026-09-30", "2026-10-01", "2026-10-02"]);
        let all = store.daily_query("2026-09-01", "2026-12-31", None, 100).unwrap();
        let keys: Vec<(String, u64)> = all.iter().map(|r| (r.day.clone(), r.player)).collect();
        assert_eq!(
            keys,
            vec![
                ("2026-09-30".into(), 7),
                ("2026-10-01".into(), 0),
                ("2026-10-01".into(), 2),
                ("2026-10-02".into(), 2)
            ]
        );
        assert_eq!(store.daily_query("2026-10-01", "2026-10-01", Some(2), 100).unwrap()[0].data, b"b");
        assert_eq!(store.daily_query("2026-09-01", "2026-12-31", None, 2).unwrap().len(), 2);
        // Replacing a day replaces all of it (idempotent re-rollups) and touches no other day.
        store.daily_replace("2026-10-01", &[daily("2026-10-01", 3, "x")]).unwrap();
        store.daily_replace("2026-10-01", &[daily("2026-10-01", 3, "x")]).unwrap();
        let day1 = store.daily_query("2026-10-01", "2026-10-01", None, 100).unwrap();
        assert_eq!(day1, vec![daily("2026-10-01", 3, "x")]);
        assert_eq!(store.daily_days().unwrap().len(), 3);
        // The raw log's prune never touches the daily summaries.
        store.activity_prune(u64::MAX / 2).unwrap();
        assert_eq!(store.daily_days().unwrap().len(), 3);

        // On Postgres the server's own connection reads and writes the same table.
        if let Some(mut log) = store.activity_log() {
            log.append(&[raw(500, Some(4))]).unwrap();
            assert_eq!(log.oldest().unwrap(), Some(500));
            assert_eq!(log.between(0, 1_000).unwrap().len(), 1);
            log.daily_replace("2026-10-03", &[daily("2026-10-03", 4, "d")]).unwrap();
            assert_eq!(log.daily_days().unwrap().len(), 4);
            assert_eq!(log.daily_query("2026-10-03", "2026-10-03", Some(4), 10).unwrap()[0].data, b"d");
            assert_eq!(store.daily_query("2026-10-03", "2026-10-03", None, 10).unwrap().len(), 1);
        }
        let days = store.daily_days().unwrap();

        // Export and import carry every daily row (the raw log stays behind).
        let backup = std::env::temp_dir().join(format!("af-store-daily-backup-{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&backup);
        match store.detached_backup() {
            Some(job) => job(&backup).unwrap(),
            None => store.backup_to(&backup).unwrap(),
        };
        let restored = Store::open(&backup).unwrap();
        assert_eq!(restored.daily_days().unwrap(), days);
        for dest in locations("daily_dest") {
            let d = dest.open();
            restored.copy_into(&d).unwrap();
            assert_eq!(d.daily_days().unwrap(), days, "{}", dest.0);
            assert_eq!(
                d.daily_query("2000-01-01", "2999-12-31", None, 1_000).unwrap(),
                store.daily_query("2000-01-01", "2999-12-31", None, 1_000).unwrap()
            );
            assert_eq!(d.load_world().unwrap().0.state_hash(), w.state_hash());
        }
        let _ = std::fs::remove_file(&backup);
    }
}
