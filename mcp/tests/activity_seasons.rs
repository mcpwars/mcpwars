//! Activity by season: the operator's Activity view over the live season, archived seasons and
//! the time before them, mixing raw rows and daily summaries.

use af_core::galaxy::{GenesisConfig, generate};
use af_core::history::SeasonArchive;
use af_core::model::PlayerKind;
use af_mcp::activity::{Call, Identities, rollup_day};
use af_mcp::archive::{DAY_MS, day_floor, day_of};
use af_mcp::game::{Game, HistOp, HistOut, now_ms};
use af_mcp::http::{HttpConfig, router};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse, ContentBlock};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{Value, json};

const H: u64 = 3_600_000;

fn call(at_ms: u64, player: Option<u32>, tool: &str, ok: bool) -> Call {
    Call {
        at_ms,
        player,
        role: if player.is_some() { "captain" } else { "anon" }.into(),
        via: "mcp".into(),
        tool: tool.into(),
        ok,
        code: (!ok).then(|| "no_ap".into()),
        ms: 4,
        session: Some(format!("s{}-{}", player.unwrap_or(0), at_ms / DAY_MS)),
        client: Some("claude-code/2.1".into()),
        ..Default::default()
    }
}

fn archived(number: u32, started_ms: u64, ended_ms: u64, ended_tick: u64) -> SeasonArchive {
    SeasonArchive {
        number,
        reason: "season_end".into(),
        epoch: 0,
        started_tick: 0,
        ended_tick,
        started_ms,
        ended_ms,
        sectors: 120,
        captains: vec![],
    }
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

fn day_calls(v: &Value, day: u64) -> u64 {
    v["by_day"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["day"] == day_of(day))
        .map(|d| d["calls"].as_u64().unwrap())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn seasons_mix_raw_rows_and_daily_summaries_and_split_boundary_days_by_time() {
    let now = now_ms();
    let today = day_floor(now);
    // Season 1: long past the raw retention, from mid-day to mid-day. Season 2: inside the raw
    // log, ending yesterday at noon, where season 3 (the live one) starts.
    let s1 = (today - 200 * DAY_MS + 6 * H, today - 198 * DAY_MS + 18 * H);
    let s2 = (today - 3 * DAY_MS + 10 * H, today - DAY_MS + 12 * H);
    let mut w = generate(&GenesisConfig::new(5, 120));
    w.archive.seasons.push(archived(1, s1.0, s1.1, 1000));
    w.archive.seasons.push(archived(2, s2.0, s2.1, 4888));
    w.season.number = 3;
    w.season.started_ms = s2.1;
    let game = Game::spawn(w, None);
    let (pid, _, _, _) = game
        .register_full(
            "Alpha",
            Some("claude-opus-5-5".into()),
            Some("claude code".into()),
            false,
            PlayerKind::Agent,
        )
        .await
        .unwrap();

    // Daily summaries only (the raw rows are long gone): a day before season 1 and season 1's days.
    let mut who = Identities::new();
    who.insert(pid as u64, ("Alpha".into(), None, None));
    let summaries = [
        (today - 300 * DAY_MS, 2u64),
        (today - 200 * DAY_MS, 3),
        (today - 199 * DAY_MS, 5),
        (today - 198 * DAY_MS, 7),
    ];
    for (day, n) in summaries {
        let calls: Vec<Call> = (0..n)
            .map(|i| call(day + (i + 1) * 60_000, Some(pid), "trade_port", i != 0))
            .collect();
        let rows: Vec<af_store::DailyRow> = rollup_day(&day_of(day), &calls, &who)
            .iter()
            .map(|a| af_store::DailyRow {
                day: day_of(day),
                player: a.player_id,
                data: serde_json::to_vec(a).unwrap(),
            })
            .collect();
        assert!(matches!(
            game.hist(HistOp::DailyReplace(day_of(day), rows)).await,
            Some(HistOut::Done)
        ));
    }
    // Raw rows: one between seasons 1 and 2 (in no season), season 2 on both sides of its edges.
    for c in [
        call(s2.0 - H, Some(pid), "warp", true),
        call(s2.0 + H, Some(pid), "get_briefing", true),
        call(today - 2 * DAY_MS + 5 * H, Some(pid), "warp", false),
        call(today - 2 * DAY_MS + 6 * H, None, "join_game", false),
        call(s2.1 - H, Some(pid), "warp", true),
        call(s2.1 + H, Some(pid), "trade_port", true),
        call(now - 1_000, Some(pid), "trade_port", true),
        Call::new("mcp", "sysop", "sysop_overview", None),
    ] {
        game.record(c);
    }

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
    let http = reqwest::Client::new();
    let get = |path: String| {
        let req = http.get(format!("{url}{path}")).bearer_auth("adm");
        async move {
            let r = req.send().await.unwrap();
            let status = r.status().as_u16();
            (status, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };
    assert_eq!(
        http.get(format!("{url}/api/sysop/activity/seasons")).send().await.unwrap().status(),
        401,
        "operator only"
    );

    // The tabs: newest first, with "earlier" for the summaries before season 1.
    let (status, list) = get("/api/sysop/activity/seasons".into()).await;
    assert_eq!(status, 200, "{list}");
    let seasons = list["seasons"].as_array().unwrap();
    let keys: Vec<&str> = seasons.iter().map(|s| s["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["current", "s2-season_end-t4888", "s1-season_end-t1000", "earlier"], "{list}");
    assert_eq!(seasons[0]["label"], "Season 3");
    assert_eq!((seasons[0]["current"].as_bool(), seasons[0]["to_ms"].is_null()), (Some(true), true));
    assert_eq!(seasons[0]["from_ms"], s2.1);
    assert_eq!(
        (seasons[1]["from_ms"].as_u64(), seasons[1]["to_ms"].as_u64()),
        (Some(s2.0), Some(s2.1))
    );
    assert_eq!(seasons[1]["has_raw"], true);
    assert_eq!(seasons[2]["has_raw"], false, "season 1 is older than the raw log");
    assert_eq!(seasons[3]["label"], "Before season 1");
    assert_eq!(
        (seasons[3]["from_ms"].as_u64(), seasons[3]["to_ms"].as_u64()),
        (Some(today - 300 * DAY_MS), Some(s1.0))
    );

    // The live season: raw rows from yesterday noon on, exact.
    let (status, cur) = get("/api/sysop/activity?season=current".into()).await;
    assert_eq!(status, 200, "{cur}");
    assert_eq!(cur["season"]["exact"], true, "{cur}");
    assert!(cur["season"]["to_ms"].is_null());
    assert_eq!(cur["calls"], 2, "operator calls are not counted: {cur}");
    let alpha = cur["players"].as_array().unwrap().iter().find(|p| p["name"] == "Alpha").unwrap();
    assert_eq!(alpha["calls"], 2);
    assert_eq!(alpha["top_tools"][0], "trade_port 2");
    assert_eq!(day_calls(&cur, today - DAY_MS), 1, "split at noon: {cur}");
    assert_eq!(day_calls(&cur, today), 1);
    assert!(cur.get("by_hour").is_none());

    // Season 2: raw rows only, split exactly at both edges.
    let (status, s2v) = get("/api/sysop/activity?season=s2-season_end-t4888&name=Alpha".into()).await;
    assert_eq!(status, 200, "{s2v}");
    assert_eq!(s2v["season"]["exact"], true, "{s2v}");
    assert_eq!(s2v["name"], "Alpha");
    assert_eq!((s2v["calls"].as_u64(), s2v["refused"].as_u64()), (Some(3), Some(1)), "{s2v}");
    assert_eq!(s2v["refusal_codes"]["no_ap"], 1);
    let days: Vec<&str> = s2v["by_day"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["day"].as_str().unwrap())
        .collect();
    assert_eq!(days, [day_of(s2.0), day_of(today - 2 * DAY_MS), day_of(today - DAY_MS)]);
    assert_eq!(day_calls(&s2v, today - 3 * DAY_MS), 1, "the call before the season is left out");
    assert_eq!(day_calls(&s2v, today - DAY_MS), 1, "the call after the season is left out");
    assert!(s2v["by_day"].as_array().unwrap().iter().all(|d| d["source"] == "raw"));
    assert_eq!(s2v["recent"].as_array().unwrap().len(), 3);
    assert!(!s2v["sessions"].as_array().unwrap().is_empty());
    let warp = s2v["by_tool"].as_array().unwrap().iter().find(|t| t["tool"] == "warp").unwrap();
    assert_eq!((warp["calls"].as_u64(), warp["avg_ms"].as_u64()), (Some(2), Some(4)));
    let (_, s2all) = get("/api/sysop/activity?season=s2-season_end-t4888".into()).await;
    assert_eq!(s2all["calls"], 4, "{s2all}");
    assert_eq!(s2all["not_logged_in"]["calls"], 1);
    let (_, anon) = get("/api/sysop/activity?season=s2-season_end-t4888&name=@anon".into()).await;
    assert_eq!((anon["calls"].as_u64(), anon["player_id"].as_u64()), (Some(1), Some(0)), "{anon}");

    // Season 1: daily summaries only. Its first and last days are boundary days counted whole.
    let (status, s1v) = get("/api/sysop/activity?season=s1-season_end-t1000&name=Alpha".into()).await;
    assert_eq!(status, 200, "{s1v}");
    assert_eq!(s1v["season"]["exact"], false, "{s1v}");
    assert_eq!(
        s1v["season"]["approximate_days"],
        json!([day_of(today - 200 * DAY_MS), day_of(today - 198 * DAY_MS)])
    );
    assert!(s1v["season"]["note"].as_str().unwrap().starts_with("Approximate on "));
    assert_eq!((s1v["calls"].as_u64(), s1v["refused"].as_u64()), (Some(15), Some(3)), "{s1v}");
    let by_day = s1v["by_day"].as_array().unwrap();
    assert_eq!(by_day.len(), 3);
    assert_eq!(
        by_day.iter().map(|d| d["exact"].as_bool().unwrap()).collect::<Vec<_>>(),
        [false, true, false]
    );
    assert!(by_day.iter().all(|d| d["source"] == "daily"));
    assert!(s1v["recent"].as_array().unwrap().is_empty(), "no raw rows that old");
    let t = s1v["by_tool"][0].clone();
    assert_eq!((t["tool"].as_str(), t["calls"].as_u64()), (Some("trade_port"), Some(15)));
    assert!(t["avg_ms"].is_null(), "timings exist only in raw rows");

    // Earlier: up to season 1's start, mid-day, so that day's summary is counted whole.
    let (_, early) = get("/api/sysop/activity?season=earlier".into()).await;
    assert_eq!(early["calls"], 2 + 3, "{early}");
    assert_eq!(early["season"]["exact"], false);
    assert_eq!(early["season"]["approximate_days"], json!([day_of(today - 200 * DAY_MS)]));
    assert_eq!(early["players"][0]["name"], "Alpha");

    // Unknown season, unknown captain.
    assert_eq!(get("/api/sysop/activity?season=s9-season_end-t1".into()).await.0, 404);
    assert_eq!(get("/api/sysop/activity?season=current&name=Nobody".into()).await.0, 404);
    // Hours still work as before.
    let (status, h) = get("/api/sysop/activity?hours=24".into()).await;
    assert_eq!((status, h["window_hours"].as_u64()), (200, Some(24)), "{h}");

    // Raw calls within a season.
    let (status, c) = get("/api/sysop/calls?season=s2-season_end-t4888&limit=10".into()).await;
    assert_eq!(status, 200, "{c}");
    assert_eq!(c["matching"], 4, "{c}");
    assert_eq!(c["season"]["key"], "s2-season_end-t4888");
    assert!(
        c["calls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| (s2.0..s2.1).contains(&x["at_ms"].as_u64().unwrap()))
    );
    let (_, c) = get("/api/sysop/calls?season=s2-season_end-t4888&limit=2".into()).await;
    assert_eq!(c["continuation"]["next_arguments"]["season"], "s2-season_end-t4888");

    // The same over MCP.
    let sysop = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let tool = |name: &str, args: Value| {
        let mut p = CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        sysop.peer().call_tool_once(p)
    };
    let (ok, t) = text(
        &tool("sysop_activity", json!({"season": "s1-season_end-t1000", "captain": "Alpha"}))
            .await
            .unwrap(),
    );
    assert!(ok && t.starts_with("Season 1: 15 call(s), 3 refused. Approximate on "), "{t}");
    let (ok, t) = text(&tool("sysop_activity", json!({"season": "current"})).await.unwrap());
    assert!(ok && t.starts_with("Season 3: 2 call(s), 0 refused. Exact"), "{t}");
    let (ok, t) = text(&tool("sysop_activity", json!({"season": "nope"})).await.unwrap());
    assert!(!ok && t.contains("not_found"), "{t}");
    let (ok, t) = text(
        &tool("sysop_calls", json!({"season": "s2-season_end-t4888", "captain": "Alpha"}))
            .await
            .unwrap(),
    );
    assert!(ok && t.starts_with("3 of 3 matching calls in Season 2"), "{t}");
}
