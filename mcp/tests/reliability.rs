//! Operator targeting, durable history and tolerant captain contracts.
use af_core::{
    Command,
    engine::Actor,
    galaxy::{GenesisConfig, generate},
    model::PlayerKind,
};
use af_mcp::{
    game::Game,
    http::{HttpConfig, run_admin, sysop_journal_search},
    sysop_mcp::JournalArgs,
    tools,
};
use serde_json::{Value, json};

fn config() -> HttpConfig {
    HttpConfig {
        public_url: "http://localhost".into(),
        admin_token: Some("test".into()),
        spectator_key: None,
        open_registration: true,
        allowed_hosts: vec![],
        backup_dir: None,
    }
}
async fn admin(game: &Game, action: &str, args: Value) -> Result<Value, af_core::Refusal> {
    run_admin(game, &config(), action, args).await.unwrap()
}
async fn search(game: &Game, args: Value) -> Value {
    sysop_journal_search(game, serde_json::from_value(args).unwrap()).await.unwrap()
}

#[tokio::test]
async fn supplied_message_recipient_never_becomes_a_broadcast() {
    let game = Game::spawn(generate(&GenesisConfig::new(17, 60)), None);
    let (a, _) = game
        .register("Target Captain", Some("test".into()), PlayerKind::Human)
        .await
        .unwrap();
    let (b, _) = game
        .register("Other Captain", Some("test".into()), PlayerKind::Human)
        .await
        .unwrap();
    for target in [json!("target captain"), json!(a), json!(a.to_string())] {
        let r = admin(&game, "message", json!({"player":target, "text":"private delivery"}))
            .await
            .unwrap();
        assert_eq!(r["recipients"], json!([a]));
    }
    let before = game.read(|w| w.state_hash()).await;
    for target in [
        json!("missing"),
        Value::Null,
        json!(""),
        json!(true),
        json!(0),
        json!(-1),
        json!(4294967297u64),
        json!(1.5),
        json!({}),
    ] {
        assert!(
            admin(&game, "message", json!({"player":target,"text":"must not send"}))
                .await
                .is_err()
        );
    }
    assert_eq!(game.read(|w| w.state_hash()).await, before);
    let r = admin(&game, "message", json!({"text":"intentional broadcast"})).await.unwrap();
    assert!(r["recipients"].as_array().unwrap().contains(&json!(a)));
    assert!(r["recipients"].as_array().unwrap().contains(&json!(b)));
    let messages = game
        .read(move |w| w.players[&b].inbox.iter().filter(|n| n.text.contains("private delivery")).count())
        .await;
    assert_eq!(messages, 0);
}

#[tokio::test]
async fn journal_search_survives_restart_and_finds_commands_behind_ticks() {
    let path = std::env::temp_dir().join(format!("af-journal-search-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (w, store, _) = af_mcp::open_world(path.to_str().unwrap(), 17, 40, 60_000, false).unwrap();
    let game = Game::spawn(w, Some(store));
    let (a, _) = game
        .register("Journal Reader", Some("test".into()), PlayerKind::Human)
        .await
        .unwrap();
    for i in 0..3 {
        admin(&game, "message", json!({"player": a, "text":format!("historic needle {i}")}))
            .await
            .unwrap();
    }
    admin(&game, "set_player", json!({"player":"Journal Reader", "sector":30}))
        .await
        .unwrap();
    assert!(
        tools::call(&game, a, "write_note", &json!({"key":"plan", "value":"Journal actor test"}))
            .await
            .ok
    );
    let feedback = tools::call(&game, a, "send_feedback", &json!({"about":"test", "text":"historical feedback"})).await;
    assert!(feedback.ok);
    admin(
        &game,
        "resolve_feedback",
        json!({"id":feedback.data["result"]["id"], "note":"historical feedback reply"}),
    )
    .await
    .unwrap();
    for _ in 0..1_010 {
        game.exec(Actor::System, Command::Tick).await.unwrap();
    }
    game.release().await;
    assert!(sysop_journal_search(&game, JournalArgs::default()).await.is_err());
    let (w, store, _) = af_mcp::open_world(path.to_str().unwrap(), 17, 40, 60_000, false).unwrap();
    let restored = Game::spawn(w, Some(store));
    assert!(restored.journal_tail.lock().unwrap().is_empty());
    let first = search(
        &restored,
        json!({"actor":"sysop", "command":"admin:message", "query":"NEEDLE", "affected_player":"Journal Reader", "limit":2}),
    )
    .await;
    assert_eq!(first["entries"].as_array().unwrap().len(), 2);
    assert_eq!(first["has_more"], true);
    admin(&restored, "message", json!({"text":"new historic needle"})).await.unwrap();
    let second = search(&restored, first["next_arguments"].clone()).await;
    assert_eq!(second["entries"].as_array().unwrap().len(), 1);
    assert_eq!(second["has_more"], false);
    assert!(second["entries"][0]["seq"].as_u64() < first["entries"][1]["seq"].as_u64());
    let actor = search(&restored, json!({"actor": "Journal Reader"})).await;
    assert!(actor["entries"].as_array().unwrap().iter().any(|e| e["cmd"]["type"] == "note"));
    let feedback = search(
        &restored,
        json!({"actor":"sysop", "command":"resolve_feedback", "query":"historical feedback reply"}),
    )
    .await;
    assert_eq!(feedback["entries"].as_array().unwrap().len(), 1);
    let moved = search(&restored, json!({"command":"set_player", "affected_player":a.to_string()})).await;
    assert_eq!(moved["entries"].as_array().unwrap().len(), 1);
    assert_eq!(search(&restored, json!({"actor":"nobody"})).await["entries"], json!([]));
    restored.release().await;
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn aliases_and_unknown_equipment_fields_do_not_buy_the_maximum() {
    let game = Game::spawn(generate(&GenesisConfig::new(17, 60)), None);
    let (a, _) = game
        .register("Careful Buyer", Some("test".into()), PlayerKind::Human)
        .await
        .unwrap();
    admin(&game, "set_player", json!({"player":a,"sector":1,"credits":1_000_000}))
        .await
        .unwrap();
    let mut state = game.read(move |w| serde_json::to_value(&w.players[&a]).unwrap()).await;
    for args in [
        json!({"item":"holds","quanity":1}),
        json!({"item":"holds","quantity":1,"qty":2}),
        json!({"item":"holds","dry_run":true}),
    ] {
        let reply = tools::call(&game, a, "buy_equipment", &args).await;
        assert!(!reply.ok);
        assert_eq!(reply.data["error"]["code"], "invalid_args");
        assert_eq!(game.read(move |w| serde_json::to_value(&w.players[&a]).unwrap()).await, state);
    }
    for key in ["qty", "count", "quantity"] {
        let before = game.read(move |w| w.players[&a].ship.holds).await;
        let reply = tools::call(&game, a, "buy_equipment", &json!({"item":"holds",key:"1"})).await;
        assert!(reply.ok, "{}", reply.text);
        assert_eq!(game.read(move |w| w.players[&a].ship.holds).await, before + 1);
    }
    assert!(tools::call(&game, a, "constitution", &json!({"action":"list"})).await.ok);
    state = tools::coerce_args(&tools::catalog(), "read_inbox", &json!({"peek":"True", "include_read":"FALSE"}));
    assert_eq!(state, json!({"peek":true,"include_read":false}));
}

#[test]
fn wait_timeout_leaves_response_margin() {
    for (requested, expected) in [
        (None, 30),
        (Some(0), 1),
        (Some(10), 10),
        (Some(60), 45),
        (Some(110), 45),
        (Some(u64::MAX), 45),
    ] {
        assert_eq!(tools::wait_timeout(requested), expected);
    }
    assert_eq!(af_mcp::server::server_config().server_info.version, af_core::ENGINE_VERSION);
}

#[tokio::test]
async fn repeated_refusals_get_a_hint_until_the_tool_succeeds() {
    use af_mcp::activity::Call;
    let game = Game::spawn(generate(&GenesisConfig::new(17, 60)), None);
    let (pid, _) = game
        .register("Learning Captain", Some("test".into()), PlayerKind::Human)
        .await
        .unwrap();
    for _ in 0..3 {
        game.record(Call::new("mcp", "captain", "buy_equipment", Some(pid)).failed("invalid_args"));
    }
    let briefing = tools::call(&game, pid, "get_briefing", &json!({"compact":true})).await;
    assert_eq!(briefing.data["result"]["server"]["refusal_hint"]["count"], 3);
    assert_eq!(briefing.data["result"]["server"]["refusal_hint"]["tool"], "buy_equipment");
    game.record(Call {
        ok: true,
        ..Call::new("mcp", "captain", "buy_equipment", Some(pid))
    });
    let briefing = tools::call(&game, pid, "get_briefing", &json!({"compact":true})).await;
    assert!(briefing.data["result"]["server"]["refusal_hint"].is_null());
}
