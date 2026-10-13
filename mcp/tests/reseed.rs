//! A relaunched universe at the server: the captain token, owner token and recovery key issued
//! before the reseed still work, and nothing a captain wrote before it can be read. On Postgres,
//! a server never creates a universe in an empty schema unless told to (`AF_CREATE_UNIVERSE`).

use af_core::Command;
use af_core::engine::Actor;
use af_core::galaxy::{GenesisConfig, generate};
use af_core::model::PlayerKind;
use af_core::reseed::{self, ReseedOpts};
use af_mcp::game::Game;
use af_mcp::tools;
use serde_json::json;

#[tokio::test]
async fn old_credentials_work_in_a_reseeded_universe() {
    let old_game = Game::spawn(generate(&GenesisConfig::new(31, 80)), None);
    let (pid, token, owner, recovery) = old_game
        .register_full(
            "Nadia Kerr",
            Some("test-model".into()),
            Some("test-harness".into()),
            false,
            PlayerKind::Agent,
        )
        .await
        .unwrap();
    let r = tools::call(
        &old_game,
        pid,
        "write_note",
        &json!({"key": "plan", "value": "# Plan\nOLD_PLAN_TEXT"}),
    )
    .await;
    assert!(r.ok, "{}", r.data);
    for _ in 0..5 {
        old_game.exec(Actor::System, Command::Tick).await.unwrap();
    }
    let old = old_game.read(|w| w.clone()).await;

    // Seed a database file the way `af-server reseed` seeds Postgres, then boot it like a server.
    let path = std::env::temp_dir().join(format!("af-mcp-reseed-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let new = reseed::reseed(&old, &ReseedOpts::for_world(&old));
        let store = af_store::Store::open(&path).unwrap();
        let g = af_store::Genesis {
            seed: new.seed,
            sectors: new.sector_count(),
            rules: new.rules.clone(),
            created_unix_ms: 1,
            engine: af_core::ENGINE_VERSION.into(),
        };
        store.init_with_world(&g, &new, &json!({"source_seq": old.seq})).unwrap();
    }
    let loc = path.display().to_string();
    let (world, store, created) = tokio::task::spawn_blocking(move || af_mcp::open_world(&loc, 1, 40, 60_000, false).unwrap())
        .await
        .unwrap();
    assert!(!created, "the seeded universe is resumed, not replaced");
    assert_eq!(world.seq, 0);
    let game = Game::spawn(world, Some(store));

    // The captain and owner tokens still authenticate, as the same captain.
    assert_eq!(game.auth(&token).await, Some(pid));
    assert_eq!(game.auth_role(&owner).await, Some((pid, true)));
    let r = tools::call(&game, pid, "get_status", &json!({})).await;
    assert!(r.ok, "{}", r.data);
    // Nothing written before the relaunch is readable.
    let r = tools::call(&game, pid, "read_notes", &json!({})).await;
    assert!(r.ok, "{}", r.data);
    assert!(!r.data.to_string().contains("OLD_PLAN_TEXT"));
    // The recovery key still replaces a lost token.
    let (rid, fresh) = game.recover("Nadia Kerr", &recovery).await.unwrap();
    assert_eq!(rid, pid);
    assert_eq!(game.auth(&fresh).await, Some(pid));
    assert_eq!(game.auth(&token).await, None);
    game.snapshot().await;
    drop(game);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_server_creates_a_universe_on_postgres_only_when_told_to() {
    let Some(url) = std::env::var("AF_TEST_POSTGRES_URL").ok().filter(|s| !s.is_empty()) else {
        return;
    };
    let schema = format!("t_mcp_genesis_guard_{}", std::process::id());
    let mut c = af_store::pg::connect(&url).unwrap();
    c.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema}"))
        .unwrap();
    let sep = if url.contains('?') { '&' } else { '?' };
    let loc = format!("{url}{sep}options=-csearch_path%3D{schema}");
    let err = af_mcp::open_world_with(&loc, 3, 40, 60_000, false, false).err().expect("refused");
    let msg = err.to_string();
    assert!(msg.contains("AF_CREATE_UNIVERSE=1"), "{msg}");
    assert!(!msg.contains(":pw@"), "the URL is redacted: {msg}");
    let (_, store, created) = af_mcp::open_world_with(&loc, 3, 40, 60_000, false, true).unwrap();
    assert!(created);
    drop(store);
    // Once it exists, the switch no longer matters.
    let (_, store, created) = af_mcp::open_world_with(&loc, 3, 40, 60_000, false, false).unwrap();
    assert!(!created);
    drop(store);
    c.batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE")).unwrap();
}
