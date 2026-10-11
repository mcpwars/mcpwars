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

#[test]
fn permanent_private_archive_is_atomic_and_survives_reset_removal_and_copy() {
    use af_core::memory::{Action, Args};
    use af_store::private_archive::{ArchiveQuery, Capture};
    for loc in locations("private_archive") {
        let store = loc.open();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play(&store, &mut w, 1);
        let pid = w.players.values().find(|p| p.npc.is_none()).unwrap().id;
        store.backfill_private(&w).unwrap();
        let run = |w: &mut World, actor, cmd| {
            let e = Entry {
                seq: w.seq + 1,
                at_ms: (w.seq + 1) * 10,
                actor,
                cmd,
            };
            let before = Capture::before(w, &e);
            apply(w, &e).unwrap();
            let rows = before.after(w, &e).unwrap();
            store.append_with_private(&e, Some(&w.state_hash()), &rows).unwrap();
            e
        };
        let args = |action, key: &str, data, ttl_ticks| Args {
            action,
            key: Some(key.into()),
            data,
            expected_revision: None,
            ttl_ticks,
            path: None,
            include_archived: false,
            offset: None,
            limit: None,
        };
        run(
            &mut w,
            Actor::Player(pid),
            Command::Note(NoteArgs {
                key: "research".into(),
                value: "  α 🚀\noriginal  ".into(),
                mode: None,
            }),
        );
        run(
            &mut w,
            Actor::Player(pid),
            Command::Note(NoteArgs {
                key: "research".into(),
                value: "\nnext".into(),
                mode: Some(NoteMode::Append),
            }),
        );
        run(
            &mut w,
            Actor::Player(pid),
            Command::Note(NoteArgs {
                key: "research".into(),
                value: String::new(),
                mode: Some(NoteMode::Archive),
            }),
        );
        run(
            &mut w,
            Actor::Player(pid),
            Command::Memory(args(
                Action::Save,
                "facts",
                Some(serde_json::json!({"research":"never lose this"})),
                Some(1),
            )),
        );
        run(&mut w, Actor::System, Command::Tick);
        assert!(w.players[&pid].memory.is_empty());
        run(
            &mut w,
            Actor::Player(pid),
            Command::Memory(args(
                Action::Save,
                "facts",
                Some(serde_json::json!({"research":"next generation"})),
                None,
            )),
        );
        run(&mut w, Actor::System, Command::Admin(AdminArgs::FreshStart));
        let e = run(&mut w, Actor::System, Command::Admin(AdminArgs::RemovePlayer { player: pid }));
        assert!(!w.players.contains_key(&pid));
        let q = ArchiveQuery {
            player: Some(u64::from(pid)),
            limit: 100,
            ..Default::default()
        };
        let rows = store.private_archive(&q).unwrap();
        let bodies: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::from_slice(&r.data).unwrap()).collect();
        assert!(
            bodies
                .iter()
                .any(|v| v["reason"] == "archived" && v["body"]["text"] == "  α 🚀\noriginal  \n\nnext")
        );
        assert!(
            bodies
                .iter()
                .any(|v| v["reason"] == "expired" && v["body"]["versions"][0]["data"]["research"] == "never lose this")
        );
        assert!(
            bodies
                .iter()
                .any(|v| v["reason"] == "fresh_start" && v["body"]["versions"][0]["revision"] == 2)
        );
        let mut forged = rows[0].clone();
        forged.seq = e.seq + 100;
        forged.ordinal = 0;
        assert!(store.append_with_private(&e, None, &[forged]).is_err());
        assert_eq!(rows, store.private_archive(&q).unwrap());
        let mut cursor = ArchiveQuery { limit: 1, ..q.clone() };
        let mut paged = vec![];
        loop {
            let page = store.private_archive(&cursor).unwrap();
            let Some(last) = page.last() else { break };
            cursor.after_seq = last.seq;
            cursor.after_ordinal = last.ordinal;
            paged.extend(page);
        }
        assert_eq!(rows, paged);
        assert_eq!(store.replay_from_base().unwrap().state_hash(), w.state_hash());
        store.save_snapshot(&w).unwrap();
        let dest_path = std::env::temp_dir().join(format!(
            "af-private-copy-{}-{}.redb",
            std::process::id(),
            if loc.0.starts_with("postgres") { "pg" } else { "file" }
        ));
        let _ = std::fs::remove_file(&dest_path);
        let dest = Store::open(&dest_path).unwrap();
        store.copy_into(&dest).unwrap();
        assert_eq!(rows, dest.private_archive(&q).unwrap());
        assert_eq!(dest.load_world().unwrap().0.state_hash(), w.state_hash());
        drop(dest);
        drop(store);
        let reopened = loc.open();
        assert_eq!(rows, reopened.private_archive(&q).unwrap());
    }
}

#[test]
fn private_history_backfill_preserves_overwritten_and_deleted_notes_and_memory_versions() {
    use af_core::memory::{Action, Args};
    use af_store::private_archive::ArchiveQuery;
    for loc in locations("private_backfill") {
        let store = loc.open();
        let mut w = store.init(&genesis(ENGINE_VERSION)).unwrap();
        play(&store, &mut w, 1);
        let pid = w.players.values().find(|p| p.npc.is_none()).unwrap().id;
        let mut run = |cmd| {
            let e = Entry {
                seq: w.seq + 1,
                at_ms: w.seq * 10,
                actor: Actor::Player(pid),
                cmd,
            };
            apply(&mut w, &e).unwrap();
            store.append(&e, None).unwrap();
        };
        for (mode, value) in [
            (NoteMode::Replace, "original\nα"),
            (NoteMode::Append, "next"),
            (NoteMode::Delete, ""),
        ] {
            run(Command::Note(NoteArgs {
                key: "old_note".into(),
                value: value.into(),
                mode: Some(mode),
            }));
        }
        for (action, data, expected_revision) in [
            (Action::Save, serde_json::json!({"field":"before"}), Some(0)),
            (Action::Patch, serde_json::json!({"field":"after"}), Some(1)),
        ] {
            run(Command::Memory(Args {
                action,
                key: Some("old_memory".into()),
                data: Some(data),
                expected_revision,
                ttl_ticks: None,
                path: None,
                include_archived: false,
                offset: None,
                limit: None,
            }));
        }
        store.backfill_private(&w).unwrap();
        let q = ArchiveQuery {
            player: Some(u64::from(pid)),
            limit: 100,
            ..Default::default()
        };
        let rows = store.private_archive(&q).unwrap();
        let values: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::from_slice(&r.data).unwrap()).collect();
        assert!(
            values
                .iter()
                .any(|v| v["reason"] == "archived" && v["body"]["text"] == "original\nα\nnext")
        );
        assert!(values.iter().any(|v| v["body"]["version"]["data"]["field"] == "before"));
        assert!(values.iter().any(|v| v["body"]["version"]["data"]["field"] == "after"));
        store.backfill_private(&w).unwrap();
        assert_eq!(rows, store.private_archive(&q).unwrap());
    }
}
