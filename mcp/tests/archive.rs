//! Performance history: the daily activity rollup (on a real redb store) and the operator's
//! history endpoints and tool.

use af_core::galaxy::{GenesisConfig, generate};
use af_core::model::PlayerKind;
use af_mcp::activity::Call;
use af_mcp::archive::{DAY_MS, day_floor, day_of, rollup_pass};
use af_mcp::game::{Game, now_ms};
use af_mcp::http::{HttpConfig, router};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse, ContentBlock};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{Value, json};

fn call(at_ms: u64, player: Option<u32>, tool: &str, ok: bool) -> Call {
    Call {
        at_ms,
        player,
        role: if player.is_some() { "captain" } else { "anon" }.into(),
        via: "mcp".into(),
        tool: tool.into(),
        ok,
        code: (!ok).then(|| "no_ap".into()),
        ms: 3,
        session: Some(format!("s{}", player.unwrap_or(0))),
        client: Some("claude-code/2.1".into()),
        ..Default::default()
    }
}

async fn rows(game: &Game) -> Vec<Value> {
    af_mcp::archive::activity(game, None, None, None).await.unwrap()["rows"]
        .as_array()
        .unwrap()
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completed_days_roll_up_idempotently_before_the_raw_log_is_pruned() {
    let path = std::env::temp_dir().join(format!("af-history-rollup-{}.redb", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let (world, store, _) = af_mcp::open_world(&path.display().to_string(), 3, 120, 60_000, false).unwrap();
    let game = Game::spawn(world, Some(store));
    let (pid, _, _, _) = game
        .register_full(
            "Roller",
            Some("claude-opus-5-5".into()),
            Some("claude code".into()),
            false,
            PlayerKind::Agent,
        )
        .await
        .unwrap();
    let now = now_ms();
    let today = day_floor(now);
    let old = today - 100 * DAY_MS; // past the 90-day retention
    let d2 = today - 2 * DAY_MS;
    let d1 = today - DAY_MS;
    for c in [
        call(old + 1_000, Some(pid), "get_briefing", true),
        call(d2 + 5_000, Some(pid), "trade_port", true),
        call(d2 + 6_000, Some(pid), "trade_port", false),
        call(d2 + 7_000, None, "join_game", false),
        call(d1 + 1_000, Some(pid), "warp", true),
        call(today + 1, Some(pid), "warp", true),
    ] {
        game.record(c);
    }
    let report = rollup_pass(&game, now).await;
    assert_eq!(report["days_rolled_up"], json!([day_of(old), day_of(d2), day_of(d1)]), "{report}");
    let first = rows(&game).await;
    assert_eq!(first.len(), 4, "{first:?}");
    let d2_row = first.iter().find(|r| r["day"] == day_of(d2) && r["player_id"] == pid).unwrap();
    assert_eq!(d2_row["name"], "Roller");
    assert_eq!(d2_row["model"], "claude-opus-5-5");
    assert_eq!(d2_row["harness"], "Claude Code");
    assert_eq!(d2_row["platform"], "Claude Code");
    assert_eq!((d2_row["calls"].as_u64(), d2_row["refused"].as_u64()), (Some(2), Some(1)));
    assert_eq!(d2_row["refusal_codes"]["no_ap"], 1);
    assert_eq!(d2_row["tools"]["trade_port"], json!({"calls": 2, "refused": 1}));
    let anon = first.iter().find(|r| r["player_id"] == 0).unwrap();
    assert_eq!(anon["name"], "anonymous");
    // The day past the retention was summarised, then its raw rows pruned; today was not touched.
    assert_eq!(report["raw_rows_pruned"], 1, "{report}");
    let raw = game.activity(None, 0, 1_000).await;
    assert!(raw.iter().all(|c| c.at_ms >= d2), "{raw:?}");
    assert!(raw.iter().any(|c| c.at_ms > today));
    assert!(first.iter().any(|r| r["day"] == day_of(old)), "the summary outlives the raw rows");

    // Idempotent: a second pass changes nothing.
    rollup_pass(&game, now).await;
    assert_eq!(rows(&game).await, first);
    // The latest completed day is summarised again on every pass (late rows are caught).
    game.record(call(d1 + 2_000, Some(pid), "warp", true));
    rollup_pass(&game, now).await;
    let again = rows(&game).await;
    let d1_row = again.iter().find(|r| r["day"] == day_of(d1) && r["player_id"] == pid).unwrap();
    assert_eq!(d1_row["calls"], 2);
    assert_eq!(again.len(), 4, "replaced, not duplicated");
    // Filters: one captain by name, a day range.
    let mine = af_mcp::archive::activity(&game, Some(day_of(d2)), Some(day_of(d2)), Some("Roller".into()))
        .await
        .unwrap();
    assert_eq!(mine["rows"].as_array().unwrap().len(), 1, "{mine}");
    assert!(
        af_mcp::archive::activity(&game, Some("not-a-day".into()), None, None)
            .await
            .is_err()
    );
    drop(game);
    let _ = std::fs::remove_file(&path);
}

async fn start() -> (String, Game) {
    let game = Game::spawn(generate(&GenesisConfig::new(5, 120)), None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(
        game.clone(),
        HttpConfig {
            public_url: url.clone(),
            admin_token: Some("adm".into()),
            spectator_key: None,
            open_registration: true,
            allowed_hosts: vec![],
            backup_dir: None,
        },
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, game)
}

fn text(r: &CallToolResponse) -> (bool, String) {
    match r {
        CallToolResponse::Complete(c) => (
            !c.is_error.unwrap_or(false),
            c.content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => (false, String::new()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_operator_reads_history_and_a_fresh_start_loses_nothing() {
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let get = |path: &str| {
        let req = http.get(format!("{url}{path}")).bearer_auth("adm");
        async move {
            let r = req.send().await.unwrap();
            let status = r.status().as_u16();
            (status, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    let post = |path: &str, body: Value| {
        let req = http.post(format!("{url}{path}")).bearer_auth("adm").json(&body);
        async move { req.send().await.unwrap().json::<Value>().await.unwrap() }
    };
    for (name, model) in [("Alpha", "claude-opus-5-5"), ("Beta", "gpt-6")] {
        let reg: Value = http
            .post(format!("{url}/api/register"))
            .json(&json!({"name": name, "model": model, "harness": "codex"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let token = reg["token"].as_str().unwrap().to_string();
        let client = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{token}"))).await.unwrap();
        let p = CallToolRequestParams::new("get_briefing".to_string());
        let (ok, t) = text(&client.peer().call_tool_once(p).await.unwrap());
        assert!(ok, "{t}");
    }
    // Operator only.
    assert_eq!(http.get(format!("{url}/api/sysop/history")).send().await.unwrap().status(), 401);
    assert_eq!(
        http.get(format!("{url}/api/sysop/history/compare")).send().await.unwrap().status(),
        401
    );

    // A day boundary archives every captain.
    let day = af_core::player::ticks_per_day(&game.read(|w| w.rules.clone()).await) as u64;
    post("/api/sysop/action/tick", json!({"count": day})).await;
    let (status, idx) = get("/api/sysop/history").await;
    assert_eq!(status, 200, "{idx}");
    assert_eq!(idx["days"].as_array().unwrap().len(), 1, "{idx}");
    assert_eq!(idx["days"][0]["captains"], 2);
    assert_eq!(idx["days"][0]["season_key"], "current", "a day of the live season");
    assert_eq!(idx["current"]["key"], "current");
    assert_eq!(idx["current"]["number"], 1);
    assert_eq!(idx["removed"], 0);
    let (status, d) = get("/api/sysop/history/day?season=1&day=1").await;
    assert_eq!(status, 200, "{d}");
    assert_eq!(d["captains"].as_array().unwrap().len(), 2);
    assert_eq!(get("/api/sysop/history/day?season=1&day=9").await.0, 404);
    assert_eq!(get("/api/sysop/history/day?season=1").await.0, 400);
    let (_, cur) = get("/api/sysop/history/season?number=current").await;
    assert_eq!(cur["reason"], "current");
    let alpha = cur["captains"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Alpha")
        .unwrap()
        .clone();
    assert_eq!(alpha["model"], "claude-opus-5-5");
    assert_eq!(alpha["harness"], "Codex CLI");
    assert_eq!(alpha["kind"], "agent");
    assert!(alpha["scores"]["wealth"].is_i64() && alpha["place"].as_u64().unwrap() >= 1);

    // A fresh start archives every captain first and keeps the activity log.
    let calls_before = game.activity(None, 0, 10_000).await.len();
    assert!(calls_before > 0);
    let r = post("/api/sysop/action/fresh_start", json!({"confirm": "fresh start"})).await;
    assert_eq!(r["activity_kept"], true, "{r}");
    assert!(r.get("activity_rows_cleared").is_none());
    assert!(
        game.activity(None, 0, 10_000).await.len() >= calls_before,
        "fresh start keeps activity"
    );
    let (_, idx) = get("/api/sysop/history").await;
    assert_eq!(idx["seasons"][0]["reason"], "fresh_start", "{idx}");
    assert_eq!(idx["days"][0]["season_key"], idx["seasons"][0]["key"]);
    assert_eq!(idx["seasons"][0]["captains"], 2);
    assert_eq!(idx["current"]["epoch"], 1);
    let (status, s) = get("/api/sysop/history/season?number=1&reason=fresh_start").await;
    assert_eq!(status, 200, "{s}");
    assert_eq!(s["captains"].as_array().unwrap().len(), 2);
    assert_eq!(get("/api/sysop/history/season?number=7").await.0, 404);
    assert_eq!(get("/api/sysop/history/season?number=soon").await.0, 404, "an unknown key");
    // Keys are unambiguous: season 1 is now an archived record and the live season.
    let key = idx["seasons"][0]["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("s1-fresh_start-t"), "{key}");
    assert_eq!(s["key"], key.as_str());
    let (status, by_key) = get(&format!("/api/sysop/history/season?key={key}")).await;
    assert_eq!(status, 200, "{by_key}");
    assert_eq!(by_key["reason"], "fresh_start");
    assert_eq!(get("/api/sysop/history/season?key=current").await.1["reason"], "current");
    assert_eq!(get("/api/sysop/history/season?key=s9-season_end-t1").await.0, 404);
    let (status, d) = get(&format!("/api/sysop/history/day?key={key}&day=1")).await;
    assert_eq!(status, 200, "{d}");
    assert_eq!(d["season_key"], key.as_str(), "a day carries its season's key");
    assert_eq!(
        get("/api/sysop/history/day?key=current&day=1").await.0,
        404,
        "no day yet in the new epoch"
    );
    let (status, c) = get(&format!("/api/sysop/history/compare?season={key}&by=model")).await;
    assert_eq!(status, 200, "{c}");
    assert_eq!(c["seasons"][0]["key"], key.as_str());

    // Summarise today (as if it had ended) and compare across everything.
    rollup_pass(&game, now_ms() + DAY_MS).await;
    let (_, idx) = get("/api/sysop/history").await;
    assert_eq!(idx["activity_days"], json!([day_of(now_ms())]), "{idx}");
    let (status, a) = get(&format!(
        "/api/sysop/history/activity?from={0}&to={0}&player=Alpha",
        day_of(now_ms())
    ))
    .await;
    assert_eq!(status, 200, "{a}");
    assert_eq!(a["rows"].as_array().unwrap().len(), 1, "{a}");
    assert_eq!(a["truncated"], false);
    let alpha_calls = a["rows"][0]["calls"].as_u64().unwrap();
    assert!(alpha_calls > 0);
    let (status, cmp) = get("/api/sysop/history/compare?season=all&by=model").await;
    assert_eq!(status, 200, "{cmp}");
    assert_eq!((cmp["by"].as_str(), cmp["season"].as_str()), (Some("model"), Some("all")));
    let g = cmp["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["key"] == "claude-opus-5-5")
        .unwrap();
    assert_eq!(g["captains"], 2, "one result per season (archived and live): {cmp}");
    assert_eq!(g["calls"].as_u64(), Some(alpha_calls), "each captain-day counted once: {cmp}");
    assert!(g["harness"].is_null());
    assert!(g["credits_per_ap"].is_number());
    let frac = |v: &Value| v.as_f64().is_some_and(|x| (0.0..=1.0).contains(&x));
    assert!(frac(&g["ap_spent_share"]) && frac(&g["refusal_rate"]), "fractions 0..1: {g}");
    for k in [
        "active",
        "median_earned",
        "mean_earned",
        "credits_per_ap",
        "ap_spent_share",
        "daily_wins",
        "deaths",
        "refused",
        "refusal_rate",
    ] {
        assert!(!g[k].is_null(), "{k}: {g}");
    }
    let (_, cmp) = get("/api/sysop/history/compare?season=current&by=model_harness").await;
    assert!(
        cmp["groups"].as_array().unwrap().iter().any(|g| g["key"] == "gpt-6 · Codex CLI"),
        "{cmp}"
    );
    assert_eq!(get("/api/sysop/history/compare?by=maker").await.0, 400);

    // The same data over MCP, with a readable line first.
    let sysop = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let tool = |args: Value| {
        let mut p = CallToolRequestParams::new("sysop_history".to_string());
        p.arguments = args.as_object().cloned();
        sysop.peer().call_tool_once(p)
    };
    let (ok, t) = text(&tool(json!({})).await.unwrap());
    assert!(ok && t.starts_with("1 archived season record(s), 1 day record(s)"), "{t}");
    let (ok, t) = text(&tool(json!({"view": "compare", "season": "all", "by": "harness"})).await.unwrap());
    assert!(ok && t.contains("group(s) by harness"), "{t}");
    let (ok, t) = text(&tool(json!({"view": "season", "number": 1, "reason": "fresh_start"})).await.unwrap());
    assert!(
        ok && t.starts_with(&format!("Season 1 (fresh_start, key {key}): 2 captain(s).")),
        "{t}"
    );
    let (ok, t) = text(&tool(json!({"view": "day", "season": "1", "day": 1})).await.unwrap());
    assert!(ok && t.starts_with(&format!("Season 1 ({key}), day 1: 2 captain(s).")), "{t}");
    let (ok, t) = text(&tool(json!({"view": "day", "key": key, "day": 1})).await.unwrap());
    assert!(ok && t.contains("2 captain(s)"), "{t}");
    let (ok, t) = text(&tool(json!({"view": "season", "key": "current"})).await.unwrap());
    assert!(ok && t.contains("key current"), "{t}");
    let (ok, t) = text(&tool(json!({"view": "activity", "player": "Beta"})).await.unwrap());
    assert!(ok && t.starts_with("1 daily activity row(s)"), "{t}");
    let (ok, t) = text(&tool(json!({"view": "nonsense"})).await.unwrap());
    assert!(!ok && t.contains("invalid_args"), "{t}");
}
