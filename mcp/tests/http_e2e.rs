//! End-to-end: real HTTP server + rmcp client.

use af_core::galaxy::{GenesisConfig, generate};
use af_mcp::game::Game;
use af_mcp::http::{HttpConfig, router};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse, ContentBlock, GetPromptRequestParams, ReadResourceRequestParams};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{Value, json};

async fn start() -> (String, Game) {
    start_with_admin("adm").await
}

async fn start_with_admin(admin: &str) -> (String, Game) {
    let game = Game::spawn(generate(&GenesisConfig::new(5, 120)), None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");
    let app = router(
        game.clone(),
        HttpConfig {
            public_url: url.clone(),
            admin_token: Some(admin.into()),
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

fn envelope(t: &str) -> Value {
    t.split_once('\n')
        .and_then(|(_, j)| serde_json::from_str(j).ok())
        .unwrap_or(Value::Null)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_names_the_build_engine_rules_and_documentation() {
    let (url, game) = start().await;
    let r = reqwest::get(format!("{url}/version")).await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()[reqwest::header::CACHE_CONTROL], "no-store");
    let v: Value = r.json().await.unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["commit", "engine", "rules_version", "doc_revision", "started_ms"], "{v}");
    let commit = v["commit"].as_str().unwrap();
    assert!(
        commit == "unknown" || (commit.len() >= 7 && commit.bytes().all(|b| b.is_ascii_hexdigit())),
        "{v}"
    );
    assert_eq!(v["engine"], json!(af_core::ENGINE_VERSION));
    let rules = game.read(|w| w.rules.version_label()).await;
    assert_eq!(v["rules_version"], json!(rules));
    assert_eq!(v["doc_revision"], json!(af_mcp::documentation::revision()));
    let started = v["started_ms"].as_u64().unwrap();
    assert!(started > 0 && started <= af_mcp::game::now_ms(), "{v}");
    // The start time is fixed for the process: a second read returns the same body.
    let again: Value = reqwest::get(format!("{url}/version")).await.unwrap().json().await.unwrap();
    assert_eq!(again, v);
    // Like /healthz, it is no page for crawlers: neither robots.txt nor the sitemap lists it.
    for path in ["robots.txt", "sitemap.xml"] {
        let body = reqwest::get(format!("{url}/{path}")).await.unwrap().text().await.unwrap();
        assert!(!body.contains("/version") && !body.contains("/healthz"), "{path}: {body}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_over_http_end_to_end() {
    let (url, _game) = start().await;
    let http = reqwest::Client::new();
    let reg: Value = http
        .post(format!("{url}/api/register"))
        .json(&json!({"name": "E2E", "model": "test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = reg["token"].as_str().unwrap().to_string();
    // The token appears only where it is needed: no ready-made header string to leak.
    assert!(reg.get("header").is_none() && reg.get("mcp_url_with_header").is_none(), "{reg}");
    assert_eq!(reg["mcp_url"], json!(format!("{url}/mcp/{token}")));

    let client = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{token}"))).await.unwrap();
    let peer = client.peer();
    let tools = peer.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), af_mcp::tools::catalog().len());
    for expected in ["constitution", "government"] {
        assert!(tools.iter().any(|tool| tool.name == expected));
    }
    for t in &tools {
        assert!(
            t.description.as_ref().map(|d| d.len() > 20).unwrap_or(false),
            "{} lacks a description",
            t.name
        );
        assert_eq!(
            t.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object"),
            "{} schema",
            t.name
        );
        assert!(!t.input_schema.contains_key("$defs"), "{} schema must be self-contained", t.name);
    }
    let call = |name: &str, args: Value| {
        let mut p = CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        peer.call_tool_once(p)
    };
    let (ok, t) = text(&call("get_briefing", json!({})).await.unwrap());
    assert!(ok, "{t}");
    let env = envelope(&t);
    assert_eq!(env["ok"], true);
    // A new universe starts in its founding era: the briefing leads with the founding agenda,
    // and the owner guidance points there.
    let founding = &env["result"]["server"]["founding"];
    assert_eq!(founding["agenda"].as_array().map(Vec::len), Some(5), "{founding}");
    assert_eq!(founding["next"], founding["agenda"][0]["how"]);
    let guidance = env["result"]["owner"]["guidance"].as_str().expect("owner guidance");
    assert!(
        guidance.contains("Founding era: server.founding lists what the era offers."),
        "{guidance}"
    );
    let sector = env["result"]["status"]["sector"].as_u64().unwrap();
    let warps: Vec<u64> = env["result"]["sector"]["warps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["to"].as_u64().unwrap())
        .collect();

    // Move, then check AP was spent and the reply carries the new sector.
    let (ok, t) = text(&call("warp", json!({"destination_sector": warps[0]})).await.unwrap());
    assert!(ok, "{t}");
    let env = envelope(&t);
    assert!(env["ap"].as_i64().unwrap() < 500);
    assert_ne!(env["result"]["you"]["sector"].as_u64(), Some(sector));

    // Refusals are tool errors with a code and hint, not protocol errors.
    let (ok, t) = text(&call("warp", json!({"destination_sector": 9999})).await.unwrap());
    assert!(!ok);
    assert!(t.contains("REFUSED [not_adjacent]"), "{t}");
    let (ok, t) = text(
        &call("attack_target", json!({"target_type": "ship", "target_id": "nobody"}))
            .await
            .unwrap(),
    );
    assert!(!ok && t.contains("REFUSED"), "{t}");
    // Bad arguments are reported, not crashes.
    let (ok, t) = text(&call("trade_port", json!({"commodity": "spice", "action": "buy"})).await.unwrap());
    assert!(!ok && t.contains("invalid"), "{t}");

    // Fog of war: a far sector is uncharted.
    let (_, t) = text(&call("view_sector", json!({"sector_id": 119})).await.unwrap());
    assert!(t.contains("\"charted\":false") || t.contains("\"explored\""), "{t}");

    // Prompts and resources.
    let prompts = peer.list_all_prompts().await.unwrap();
    assert_eq!(prompts.len(), 5);
    let mut gp = GetPromptRequestParams::new("corsair_archetype".to_string());
    gp.arguments = json!({"call_sign": "Viper"}).as_object().cloned();
    let p = peer.get_prompt(gp).await.unwrap();
    let body = format!("{:?}", p.messages);
    assert!(body.contains("Viper") && body.contains("untrusted"));
    let r = peer
        .read_resource(ReadResourceRequestParams::new("game://player/status"))
        .await
        .unwrap();
    assert!(format!("{:?}", r.contents).contains("E2E"));

    // Unauthenticated sessions are told how to join; join_game logs the session in.
    let anon = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp"))).await.unwrap();
    let mut p = CallToolRequestParams::new("get_status".to_string());
    p.arguments = Some(Default::default());
    let (ok, t) = text(&anon.peer().call_tool_once(p).await.unwrap());
    assert!(!ok && t.contains("join_game"), "{t}");
    let mut p = CallToolRequestParams::new("join_game".to_string());
    p.arguments = json!({"name": "Walk-in"}).as_object().cloned();
    let (ok, t) = text(&anon.peer().call_tool_once(p).await.unwrap());
    assert!(ok && t.contains("af_"), "{t}");
    let joined: Value = serde_json::from_str(&t[t.find('{').unwrap()..]).unwrap();
    assert!(joined["owner_mcp_url"].as_str().is_some_and(|u| u.contains("/mcp/af_")), "{t}");
    assert!(joined.get("session_logged_in").is_some(), "{t}");
    let rkey = joined["recovery_key"].as_str().unwrap().to_string();
    let old = joined["token"].as_str().unwrap().to_string();
    // Lost the token? recover_access (no token needed) mints a new one for the same captain.
    let fresh = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp"))).await.unwrap();
    let mut p = CallToolRequestParams::new("recover_access".to_string());
    p.arguments = json!({"call_sign": "Walk-in", "recovery_key": "afr_wrong"}).as_object().cloned();
    let (ok, t) = text(&fresh.peer().call_tool_once(p).await.unwrap());
    assert!(!ok && t.contains("recovery_failed"), "{t}");
    let mut p = CallToolRequestParams::new("recover_access".to_string());
    p.arguments = json!({"call_sign": "Walk-in", "recovery_key": rkey}).as_object().cloned();
    let (ok, t) = text(&fresh.peer().call_tool_once(p).await.unwrap());
    assert!(ok, "{t}");
    let rec: Value = serde_json::from_str(&t[t.find('{').unwrap()..]).unwrap();
    assert_ne!(rec["token"].as_str().unwrap(), old);
    let stale = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{old}"))).await.unwrap();
    let mut p = CallToolRequestParams::new("get_status".to_string());
    p.arguments = Some(Default::default());
    let (ok, _) = text(&stale.peer().call_tool_once(p).await.unwrap());
    assert!(!ok, "the old token no longer works");
    // The walk-in session can play right away (sessionful) or is told to reconnect (stateless).
    let mut p = CallToolRequestParams::new("get_status".to_string());
    p.arguments = Some(Default::default());
    let (ok2, t2) = text(&anon.peer().call_tool_once(p).await.unwrap());
    if t.contains("now logged in") {
        assert!(ok2 && t2.contains("Walk-in"), "{t2}");
    } else {
        assert!(!ok2 && t2.contains("join_game"), "{t2}");
    }

    // Spectator API.
    let state: Value = http
        .get(format!("{url}/api/spectator/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Public and live, but without positions or private fields.
    let me = state["captains"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "E2E")
        .cloned()
        .unwrap();
    for hidden in ["sector", "credits", "bank", "ap", "fighters", "shields"] {
        assert!(me.get(hidden).is_none(), "public captain row carries `{hidden}`");
    }
    assert!(state.get("players").is_none() && state.get("planets").is_none() && state.get("defenses").is_none());
    let map: Value = http
        .get(format!("{url}/api/spectator/map"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(map.get("sectors").is_none(), "the public map has no sector ids or lanes");
    assert!(!map["stars"].as_array().unwrap().is_empty());
    // Admin API is protected.
    let status = http.post(format!("{url}/api/admin/tick")).send().await.unwrap().status();
    assert_eq!(status.as_u16(), 401);
    let ticked: Value = http
        .post(format!("{url}/api/admin/tick"))
        .bearer_auth("adm")
        .json(&json!({"count": 3}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ticked["ticked"], 3);

    // The admin token opens the sysop tools over MCP (and only those).
    let sysop = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let sp = sysop.peer();
    let names: Vec<String> = sp.list_all_tools().await.unwrap().iter().map(|t| t.name.to_string()).collect();
    assert_eq!(
        names,
        af_mcp::sysop_mcp::catalog().iter().map(|t| t.name.to_string()).collect::<Vec<_>>()
    );
    let scall = |name: &str, args: Value| {
        let mut p = CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        sp.call_tool_once(p)
    };
    let (ok, t) = text(&scall("sysop_overview", json!({})).await.unwrap());
    assert!(ok && t.contains("E2E"), "{t}");
    let (ok, t) = text(
        &scall("sysop_action", json!({"action": "message", "args": {"text": "hello captains"}}))
            .await
            .unwrap(),
    );
    assert!(ok, "{t}");
    let (ok, t) = text(&scall("get_briefing", json!({})).await.unwrap());
    assert!(!ok && t.contains("sysop_only"), "{t}");
    // The captain sees the operator message in its briefing.
    let (_, t) = text(&call("get_briefing", json!({})).await.unwrap());
    assert!(t.contains("hello captains"), "{t}");

    // Sysop key/value reads use the same stored data, with no mutation surface.
    let (ok, t) = text(
        &call("write_note", json!({"key": "sysop_test", "value": "private inspection note"}))
            .await
            .unwrap(),
    );
    assert!(ok, "{t}");
    for (index, data) in [json!({"facts": {"sector": 12}}), json!({"facts": {"sector": 13}})]
        .into_iter()
        .enumerate()
    {
        let (ok, t) = text(
            &call(
                "memory",
                json!({"action": "save", "key": "inspection", "data": data, "ttl_ticks": if index==0 {None} else {Some(0)}}),
            )
            .await
            .unwrap(),
        );
        assert!(ok, "{t}");
    }
    let (ok, t) = text(
        &scall("sysop_notes", json!({"name": "E2E", "key": "sysop_test", "offset": 8, "limit": 10}))
            .await
            .unwrap(),
    );
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["value"], "inspection");
    let (ok, next) = text(
        &scall("sysop_notes", envelope(&t)["continuation"]["next_arguments"].clone())
            .await
            .unwrap(),
    );
    assert!(ok, "{next}");
    assert_eq!(envelope(&next)["value"], " note");
    let (ok, t) = text(&scall("sysop_memory", json!({"name": reg["player_id"].to_string()})).await.unwrap());
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["records"][0]["archived"], true);
    let (ok, t) = text(
        &scall(
            "sysop_memory",
            json!({"name": "E2E", "action": "read", "key": "inspection", "path": "/facts/sector"}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["data"], 13);
    let (ok, t) = text(
        &scall(
            "sysop_memory",
            json!({"name": "E2E", "action": "history", "key": "inspection", "limit": 1}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    let history = envelope(&t);
    assert_eq!(history["versions"][0]["revision"], 1);
    assert_eq!(history["continuation"]["has_more"], true);
    let (ok, t) = text(
        &scall("sysop_memory", history["continuation"]["next_arguments"].clone())
            .await
            .unwrap(),
    );
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["versions"][0]["revision"], 2);
    let (ok, t) = text(
        &scall(
            "sysop_memory",
            json!({"name": "E2E", "action": "diff", "key": "inspection", "expected_revision": 1}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["changes"]["/facts/sector"]["after"], 13);
    for args in [
        json!({"name": "missing"}),
        json!({"name": "E2E", "action": "save", "key": "inspection", "data": null}),
    ] {
        assert!(!text(&scall("sysop_memory", args).await.unwrap()).0);
    }
    assert!(!text(&call("sysop_memory", json!({"name": "E2E"})).await.unwrap()).0);
    for route in ["notes?name=E2E", "memory?name=E2E&action=history&key=inspection&limit=1"] {
        assert_eq!(
            http.get(format!("{url}/api/sysop/{route}")).send().await.unwrap().status().as_u16(),
            401
        );
        let r = http
            .get(format!("{url}/api/sysop/{route}"))
            .bearer_auth("adm")
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
        let data: Value = r.json().await.unwrap();
        assert!(data.get("notes").is_some() || data["versions"][0]["revision"] == 1);
    }
    let detail: Value = http
        .get(format!("{url}/api/sysop/player?name=E2E"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(detail["memory"][0]["data"]["facts"]["sector"], 13);
    assert_eq!(detail["memory"][0]["archived"], true);

    for action in ["read", "history", "diff"] {
        assert!(
            !text(
                &call(
                    "memory",
                    json!({"action":action,"key":"inspection","expected_revision":1,"include_archived":true})
                )
                .await
                .unwrap()
            )
            .0
        );
    }
    let (_, t) = text(&call("memory", json!({"action":"export"})).await.unwrap());
    assert!(!envelope(&t)["records"].to_string().contains("inspection"));
    let (ok, t) = text(&call("write_note", json!({"key":"sysop_test","mode":"archive"})).await.unwrap());
    assert!(ok, "{t}");
    let (ok, t) = text(
        &scall(
            "sysop_private_archive",
            json!({"name":"E2E","kind":"note","key":"sysop_test","limit":1}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    let first = envelope(&t);
    assert_eq!(first["events"][0]["body"]["text"], "private inspection note");
    assert_eq!(first["continuation"]["has_more"], true);
    let (ok, t) = text(
        &scall("sysop_private_archive", first["continuation"]["next_arguments"].clone())
            .await
            .unwrap(),
    );
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["events"][0]["reason"], "archived");
    assert!(!text(&call("sysop_private_archive", json!({"name":"E2E"})).await.unwrap()).0);
    let archive_url = format!("{url}/api/sysop/private_archive?name=E2E&kind=note&key=sysop_test");
    assert_eq!(http.get(&archive_url).send().await.unwrap().status().as_u16(), 401);
    assert_eq!(
        http.get(&archive_url).bearer_auth(&token).send().await.unwrap().status().as_u16(),
        401
    );
    let r = http.get(&archive_url).bearer_auth("adm").send().await.unwrap();
    assert!(r.status().is_success());
    let archive: Value = r.json().await.unwrap();
    assert_eq!(archive["events"][1]["reason"], "archived");
    let (_, t) = text(&call("read_notes", json!({"key":"sysop_test"})).await.unwrap());
    assert!(envelope(&t)["value"].is_null());

    // Every interaction is in the activity log, broken down by tool, without secrets.
    let (ok, _) = text(&call("warp", json!({"sector": 99_999})).await.unwrap());
    assert!(!ok);
    // A request the transport refuses (no session, no initialize) is logged too.
    let r = http
        .post(format!("{url}/mcp/{token}"))
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "get_status", "arguments": {}}}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_client_error(), "{}", r.status());
    let act: Value = http
        .get(format!("{url}/api/sysop/activity?name=E2E&hours=1"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tools: Vec<&str> = act["by_tool"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tool"].as_str().unwrap())
        .collect();
    for t in ["register", "tools/list", "get_briefing", "warp", "tools/call"] {
        assert!(tools.contains(&t), "{t} missing from {tools:?}");
    }
    let warp = act["by_tool"].as_array().unwrap().iter().find(|t| t["tool"] == "warp").unwrap();
    assert!(
        warp["refused"].as_u64().unwrap() >= 1 && warp["refusal_codes"]["invalid_args"] == 1,
        "{warp}"
    );
    let mcp_session = act["sessions"].as_array().unwrap().iter().find(|x| x["session"] != "none").unwrap();
    assert!(mcp_session["client"].as_str().unwrap().starts_with("rmcp/"), "{mcp_session}");
    assert!(
        mcp_session["sequence"].as_str().unwrap().starts_with("tools/list, get_briefing"),
        "{mcp_session}"
    );
    assert!(!act.to_string().contains(&token), "the token must never be logged");
    let all: Value = http
        .get(format!("{url}/api/sysop/activity"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        all["players"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "E2E" && p["calls"].as_u64().unwrap() > 3),
        "{all}"
    );
    let (ok, t) = text(&scall("sysop_activity", json!({"captain": "E2E"})).await.unwrap());
    assert!(ok && t.contains("by_tool"), "{t}");
    let (_, t) = text(&scall("sysop_overview", json!({})).await.unwrap());
    assert!(t.contains("calls_24h"), "{t}");
    // Every console feature is available over MCP too.
    for (tool, args, want) in [
        ("sysop_alerts", json!({}), "open"),
        ("sysop_comms", json!({}), "threads"),
        ("sysop_comms", json!({"captain": "E2E"}), "messages"),
        ("sysop_calls", json!({"captain": "E2E", "outcome": "refused"}), "invalid_args"),
        ("sysop_sector", json!({"sector": 1}), "lanes_out"),
        ("sysop_registry", json!({}), "concessions"),
        ("sysop_registry", json!({"section": "leaderboard"}), "E2E"),
        ("sysop_events", json!({"limit": 5}), "events"),
        ("sysop_search", json!({"query": "e2e"}), "E2E"),
        ("sysop_corp", json!({}), "corporations"),
        ("sysop_journal", json!({"command": "warp"}), "entries"),
    ] {
        let (ok, t) = text(&scall(tool, args.clone()).await.unwrap());
        assert!(ok && t.contains(want), "{tool} {args}: {t}");
    }
    // Corporations: charter, edit, inspect, disband, all over MCP.
    let act = |action: &str, args: Value| scall("sysop_action", json!({"action": action, "args": args}));
    let (ok, t) = text(
        &act("create_corp", json!({"name": "E2E Works", "ceo": "E2E", "motto": "Test"}))
            .await
            .unwrap(),
    );
    assert!(ok, "{t}");
    let (ok, t) = text(
        &act("set_corp", json!({"corp": "e2e works", "treasury": 1234, "motto": "Edited"}))
            .await
            .unwrap(),
    );
    assert!(ok && t.contains("treasury=1234"), "{t}");
    let (ok, t) = text(&scall("sysop_corp", json!({"name": "E2E Works"})).await.unwrap());
    assert!(ok && t.contains("Edited") && t.contains("1234"), "{t}");
    let (ok, t) = text(
        &act("corp_remove_member", json!({"corp": "E2E Works", "player": "nobody-here"}))
            .await
            .unwrap(),
    );
    assert!(!ok && t.contains("unknown_player"), "{t}");
    let (ok, t) = text(&act("disband_corp", json!({"corp": "E2E Works"})).await.unwrap());
    assert!(ok && t.contains("Disbanded"), "{t}");
    // Acknowledgements are shared with the console's REST endpoint.
    let (ok, t) = text(&scall("sysop_action", json!({"action": "pause"})).await.unwrap());
    assert!(ok, "{t}");
    let (ok, t) = text(&scall("sysop_ack", json!({"key": "clock-held"})).await.unwrap());
    assert!(ok, "{t}");
    let acks: Value = http
        .get(format!("{url}/api/sysop/acks"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(acks["acks"]["clock-held"]["sig"], "held", "{acks}");
    let (_, t) = text(&scall("sysop_alerts", json!({})).await.unwrap());
    let v: Value = serde_json::from_str(t.split_once('\n').unwrap().1).unwrap();
    assert!(
        v["acknowledged"].as_array().unwrap().iter().any(|a| a["key"] == "clock-held"),
        "{t}"
    );
    let (ok, _) = text(&scall("sysop_ack", json!({"key": "clock-held", "restore": true})).await.unwrap());
    assert!(ok);
    let (ok, _) = text(&scall("sysop_action", json!({"action": "resume"})).await.unwrap());
    assert!(ok);

    // The guides tell caches to revalidate.
    for path in ["join", "llms.txt", "agents.txt", "robots.txt"] {
        let r = http.get(format!("{url}/{path}")).send().await.unwrap();
        assert_eq!(r.headers()["cache-control"], "no-cache", "{path}");
    }

    // The operator corrects a captain's public identity: harness set, model cleared.
    let pid = reg["player_id"].as_u64().unwrap();
    let admin = |action: &'static str, body: Value| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            http.post(format!("{url}/api/admin/{action}"))
                .bearer_auth("adm")
                .json(&body)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    admin("set_player", json!({"player": pid, "model": "", "harness": "Instinct"})).await;
    let card: Value = http
        .get(format!("{url}/api/spectator/player?name=E2E"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(card["harness"], "Instinct", "{card}");
    assert!(card["model"].is_null(), "{card}");

    // A ban holds: the recovery key no longer brings the captain back until the ban is lifted.
    let key = reg["recovery_key"].as_str().unwrap().to_string();
    let recover = |key: String| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            http.post(format!("{url}/api/recover"))
                .json(&json!({"name": "E2E", "recovery_key": key}))
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let banned = admin("ban", json!({"player": pid})).await;
    assert!(banned["summary"].as_str().unwrap().contains("banned"), "{banned}");
    let r = recover(key.clone()).await;
    assert!(r["token"].is_null(), "a banned captain must not recover: {r}");
    admin("unban", json!({"player": pid})).await;
    let r = recover(key).await;
    let fresh = r["token"].as_str().expect("recovers after the ban is lifted").to_string();
    let again = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{fresh}"))).await.unwrap();
    let (ok, t) = text(
        &again
            .peer()
            .call_tool_once(CallToolRequestParams::new("get_status".to_string()))
            .await
            .unwrap(),
    );
    assert!(ok, "the recovered token works: {t}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_is_exact_and_pages_on_every_interface() {
    let (url, _game) = start().await;
    let http = reqwest::Client::new();
    let reg: Value = http
        .post(format!("{url}/api/register"))
        .json(&json!({"name": "Pager", "model": "test", "harness": "codex-cli"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = reg["token"].as_str().unwrap().to_string();
    let admin = |action: &'static str, body: Value| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            http.post(format!("{url}/api/sysop/action/{action}"))
                .bearer_auth("adm")
                .json(&body)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let list = |q: String| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            http.get(format!("{url}/api/sysop/feedback{q}"))
                .bearer_auth("adm")
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };

    // A long report in many scripts, with tabs, CRLF and surrounding whitespace, arrives exactly;
    // only a pasted token is scrubbed.
    let secret = format!("af_{}", "0123456789abcdef".repeat(3));
    let piece = "\tΕλληνικά 中文 العربية 🚀👩‍🚀 tab\there\r\n";
    let mut long = format!("   leading, then the token {secret} by mistake\r\n");
    while long.chars().count() < 14_900 {
        long.push_str(piece);
    }
    long.push_str("end  \n  ");
    let expected = long.replace(&secret, "[redacted]");
    let r = http
        .post(format!("{url}/api/feedback"))
        .bearer_auth(&token)
        .json(&json!({"about": "/join", "text": long}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let one = list("?limit=1".into()).await;
    assert_eq!(one["feedback"][0]["text"], expected.as_str(), "stored exactly, token scrubbed");
    assert_eq!((one["has_more"].as_bool(), one["next_before_id"].as_u64()), (Some(false), None));
    // The same over MCP.
    let captain = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{token}"))).await.unwrap();
    let mut p = CallToolRequestParams::new("send_feedback".to_string());
    p.arguments = json!({"about": "/join", "text": long}).as_object().cloned();
    let (ok, t) = text(&captain.peer().call_tool_once(p).await.unwrap());
    assert!(ok, "{t}");
    assert_eq!(list("?limit=1".into()).await["feedback"][0]["text"], expected.as_str());

    // Twelve reports (five per captain per 60 ticks), every third resolved.
    let mut sent = 2;
    while sent < 12 {
        admin("tick", json!({"count": 60})).await;
        for _ in 0..5 {
            if sent == 12 {
                break;
            }
            sent += 1;
            let r = http
                .post(format!("{url}/api/feedback"))
                .bearer_auth(&token)
                .json(&json!({"about": "paging", "text": format!("report {sent}")}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
        }
    }
    for id in [3, 6, 9, 12] {
        admin("resolve_feedback", json!({"id": id})).await;
    }

    // REST: walk every page with before_id.
    for (status, want) in [
        ("all", (1..=12).rev().collect::<Vec<u64>>()),
        ("open", vec![11, 10, 8, 7, 5, 4, 2, 1]),
    ] {
        let (mut seen, mut before) = (vec![], None::<u64>);
        loop {
            let q = match before {
                Some(b) => format!("?status={status}&limit=5&before_id={b}"),
                None => format!("?status={status}&limit=5"),
            };
            let v = list(q).await;
            assert_eq!((v["open"].as_u64(), v["total"].as_u64()), (Some(8), Some(12)), "{v}");
            assert_eq!(v["matching"].as_u64(), Some(want.len() as u64));
            seen.extend(v["feedback"].as_array().unwrap().iter().map(|f| f["id"].as_u64().unwrap()));
            if v["has_more"] == true {
                before = v["next_before_id"].as_u64();
                assert_eq!(before, seen.last().copied());
            } else {
                assert!(v["next_before_id"].is_null());
                break;
            }
        }
        assert_eq!(seen, want, "{status}");
    }

    // The sysop MCP tool pages the same way.
    let sysop = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let (mut seen, mut before) = (vec![], Value::Null);
    loop {
        let mut p = CallToolRequestParams::new("sysop_feedback".to_string());
        p.arguments = json!({"status": "resolved", "limit": 3, "before_id": before}).as_object().cloned();
        let (ok, t) = text(&sysop.peer().call_tool_once(p).await.unwrap());
        assert!(ok, "{t}");
        let v = envelope(&t);
        assert_eq!(v["matching"], 4, "{t}");
        seen.extend(v["feedback"].as_array().unwrap().iter().map(|f| f["id"].as_u64().unwrap()));
        if v["has_more"] == true {
            assert!(t.contains("More: before_id"), "{t}");
            before = v["next_before_id"].clone();
        } else {
            break;
        }
    }
    assert_eq!(seen, vec![12, 9, 6, 3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_reaches_the_operator() {
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let reg: Value = http
        .post(format!("{url}/api/register"))
        .json(&json!({"name": "Critic", "model": "test", "harness": "codex-cli"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = reg["token"].as_str().unwrap().to_string();
    let pid = reg["player_id"].as_u64().unwrap();

    // A captain over MCP: free, stored whole.
    let captain = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{token}"))).await.unwrap();
    let mut p = CallToolRequestParams::new("send_feedback".to_string());
    p.arguments = json!({"about": "/join", "text": "Line one.\nLine two."}).as_object().cloned();
    let (ok, t) = text(&captain.peer().call_tool_once(p).await.unwrap());
    assert!(ok, "{t}");
    assert!(t.starts_with("Feedback #1 received."), "{t}");

    // REST with the captain's token: from that captain.
    let post = |body: Value, bearer: Option<&str>| {
        let mut r = http.post(format!("{url}/api/feedback")).json(&body);
        if let Some(b) = bearer {
            r = r.bearer_auth(b);
        }
        r.send()
    };
    let r = post(json!({"about": "trade_port", "text": "Confusing refusal."}), Some(&token))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!((v["ok"].as_bool(), v["id"].as_u64()), (Some(true), Some(2)), "{v}");
    assert!(v["summary"].as_str().unwrap().contains("received"));
    // Without a token, or with a wrong one: anonymous.
    // A pasted secret is scrubbed before the report is journaled.
    let secret = format!("af_{}", "0123456789abcdef".repeat(3));
    let v: Value = post(
        json!({"about": "/agents.txt", "text": format!("No token. Pasted {secret} by mistake.")}),
        None,
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(v["id"], 3);
    let v: Value = post(json!({"about": "x", "text": "Wrong token."}), Some("af_nope"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["id"], 4);
    // Refusals look like the other REST endpoints.
    let r = post(json!({"about": "x", "text": "y".repeat(20_001)}), Some(&token)).await.unwrap();
    assert_eq!(r.status(), 400);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "too_long", "{v}");

    // The sysop list, newest first, filtered by status; the admin token is required.
    let r = http.get(format!("{url}/api/sysop/feedback")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let refused: Value = r.json().await.unwrap();
    let notice = refused["notice"].as_str().unwrap_or_default();
    assert_eq!(notice, af_mcp::sysop_mcp::OFF_LIMITS, "{refused}");
    assert!(
        notice.contains("core violation") && notice.contains("permanently eliminate"),
        "{refused}"
    );
    let list = |q: &'static str| {
        let (http, url) = (http.clone(), url.clone());
        async move {
            http.get(format!("{url}/api/sysop/feedback{q}"))
                .bearer_auth("adm")
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let all = list("").await;
    assert_eq!((all["open"].as_u64(), all["total"].as_u64()), (Some(4), Some(4)), "{all}");
    let items = all["feedback"].as_array().unwrap();
    assert_eq!(items[0]["id"], 4);
    assert!(items[0]["from"].is_null() && items[0]["from_id"].is_null());
    assert_eq!(items[3]["text"], "Line one.\nLine two.");
    assert_eq!(
        items[1]["text"], "No token. Pasted [redacted] by mistake.",
        "tokens never reach the journal"
    );
    assert_eq!(items[3]["from"], "Critic");
    assert_eq!(items[3]["from_id"], pid);
    assert_eq!(items[3]["harness"], "codex-cli");
    assert_eq!(list("?limit=1").await["feedback"].as_array().unwrap().len(), 1);
    let state: Value = http
        .get(format!("{url}/api/sysop/state"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["feedback_open"], 4);
    assert_eq!(state["feedback_open_max_id"], 4);
    assert_eq!(state["feedback_recent"].as_array().unwrap().len(), 4);
    // Never public.
    for path in ["api/spectator/state", "api/public/showcase", "api/public/scoreboard"] {
        let body = http.get(format!("{url}/{path}")).send().await.unwrap().text().await.unwrap();
        assert!(!body.contains("Line one") && !body.contains("Confusing refusal"), "{path}");
    }

    // The sysop tools: the alert, one report, resolve with a note, reopen.
    let sysop = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let sp = sysop.peer();
    let scall = |name: &str, args: Value| {
        let mut p = CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        sp.call_tool_once(p)
    };
    let alerts = af_mcp::sysop_mcp::alerts(&game).await;
    let a = alerts.iter().find(|a| a["key"] == "feedback").expect("feedback alert");
    assert_eq!(a["sig"], "4");
    assert_eq!(a["severity"], "warn");
    assert_eq!(a["group"], "Comms");
    assert_eq!(a["title"], "4 open feedback reports");
    assert_eq!(a["detail"], "anonymous: x");
    let (ok, t) = text(&scall("sysop_feedback", json!({"id": 1})).await.unwrap());
    assert!(ok && t.contains("Line one.\\nLine two."), "{t}");
    let (ok, t) = text(&scall("sysop_feedback", json!({"status": "open", "limit": 2})).await.unwrap());
    assert!(ok && envelope(&t)["feedback"].as_array().unwrap().len() == 2, "{t}");
    let (ok, t) = text(&scall("sysop_feedback", json!({"id": 99})).await.unwrap());
    assert!(!ok && t.contains("not_found"), "{t}");
    let (ok, t) = text(
        &scall(
            "sysop_action",
            json!({"action": "resolve_feedback", "args": {"id": 1, "note": "Fixed the guide."}}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["status"], "resolved", "{t}");
    let inbox = game
        .read(move |w| {
            w.players[&(pid as u32)]
                .inbox
                .iter()
                .map(|n| (n.kind.clone(), n.text.clone()))
                .collect::<Vec<_>>()
        })
        .await;
    assert!(
        inbox
            .iter()
            .any(|(k, t)| k == "feedback" && t == "The operator resolved your feedback #1 (/join): Fixed the guide."),
        "{inbox:?}"
    );
    let v: Value = http
        .post(format!("{url}/api/sysop/action/resolve_feedback"))
        .bearer_auth("adm")
        .json(&json!({"id": 1, "reopen": true}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!((v["id"].as_u64(), v["status"].as_str()), (Some(1), Some("open")), "{v}");
    for id in 1..=4 {
        let (ok, t) = text(
            &scall("sysop_action", json!({"action": "resolve_feedback", "args": {"id": id}}))
                .await
                .unwrap(),
        );
        assert!(ok, "{t}");
    }
    assert!(!af_mcp::sysop_mcp::alerts(&game).await.iter().any(|a| a["key"] == "feedback"));
    assert_eq!(list("?status=open").await["feedback"].as_array().unwrap().len(), 0);
    assert_eq!(list("?status=resolved").await["feedback"].as_array().unwrap().len(), 4);

    // The interaction log keeps lengths, never the text.
    game.flush_activity().await;
    let calls = game.activity(None, 0, 10_000).await;
    let fb: Vec<_> = calls.iter().filter(|c| c.tool == "send_feedback").collect();
    assert!(fb.iter().any(|c| c.via == "rest" && c.role == "anon" && c.args.is_none()));
    assert!(fb.iter().any(|c| c.via == "rest" && c.player == Some(pid as u32)));
    assert!(fb.iter().any(|c| c.via == "mcp"));
    for c in &fb {
        let s = serde_json::to_string(c).unwrap();
        assert!(
            !s.contains("Line one") && !s.contains("Confusing refusal") && !s.contains("No token"),
            "{s}"
        );
    }

    // Anonymous REST is limited per client IP: 20 an hour (the engine's anonymous budget,
    // 5 per 60 ticks, refuses sooner; every attempt still counts against the address).
    for _ in 0..18 {
        let r = post(json!({"about": "flood", "text": "again"}), None).await.unwrap();
        assert_ne!(r.status(), 429);
    }
    let r = post(json!({"about": "flood", "text": "again"}), None).await.unwrap();
    assert_eq!(r.status(), 429);
    // A captain's token is not held to the address limit.
    let r = post(json!({"about": "still", "text": "Captains are not IP limited."}), Some(&token))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

/// Clients that send numbers and booleans as strings (OpenCode sent `"quantity": "25"`) are
/// understood wherever the tool's schema says number or boolean; text fields are left alone.
#[test]
fn numeric_and_boolean_strings_follow_the_tool_schema() {
    let cat = af_mcp::tools::catalog();
    let c = |name: &str, args: Value| af_mcp::tools::coerce_args(&cat, name, &args);
    assert_eq!(
        c("buy_equipment", json!({"item": "holds", "quantity": "25"})),
        json!({"item": "holds", "quantity": 25})
    );
    assert_eq!(c("bank", json!({"action": "withdraw", "amount": "45000"}))["amount"], json!(45000));
    assert_eq!(c("view_registry", json!({"limit": "30"}))["limit"], json!(30));
    assert_eq!(
        c("trade_port", json!({"commodity": "ore", "action": "buy", "dry_run": "true"}))["dry_run"],
        json!(true)
    );
    // Text stays text, even when it looks like a number; a bad number stays as sent.
    let radio = c(
        "radio_broadcast",
        json!({"message": "hi", "channel": "private", "recipient_id": "42"}),
    );
    assert_eq!(radio["recipient_id"], json!("42"));
    assert_eq!(
        c("buy_equipment", json!({"item": "holds", "quantity": "lots"}))["quantity"],
        json!("lots")
    );
    assert_eq!(c("buy_equipment", json!({"item": "holds"})), json!({"item": "holds"}));
}

#[test]
fn json_array_strings_follow_the_tool_schema_and_preserve_text() {
    let cat = af_mcp::tools::catalog();
    let c = |name: &str, args: Value| af_mcp::tools::coerce_args(&cat, name, &args);
    assert_eq!(c("contract", json!({"sectors": "[1, 2, 4]"}))["sectors"], json!([1, 2, 4]));
    assert_eq!(
        c("get_starchart", json!({"sector_ids": "[\"1\", \"4\"]"}))["sector_ids"],
        json!([1, 4])
    );
    assert_eq!(
        c("autopilot_travel", json!({"avoid_sectors": "[1, 4]"}))["avoid_sectors"],
        json!([1, 4])
    );
    for bad in ["1, 2, 4", "[1,", "{\"a\": 1}", "null"] {
        assert_eq!(c("contract", json!({"sectors": bad}))["sectors"], json!(bad));
    }
    let text = json!({"terms": "[1, 2]", "sectors": [1, 2]});
    assert_eq!(c("contract", text.clone()), text);
    // A field whose contract allows text as well as arrays must remain text.
    let mut def = cat.iter().find(|d| d.name == "contract").unwrap().clone();
    def.schema["properties"]["sectors"] = json!({"type": ["array", "string"], "items": {"type": "integer"}});
    assert_eq!(
        af_mcp::tools::coerce_args(&[def], "contract", &json!({"sectors": "[1]"}))["sectors"],
        json!("[1]")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stringified_chart_sectors_post_once_and_invalid_arrays_spend_nothing() {
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let reg: Value = http
        .post(format!("{url}/api/register"))
        .json(&json!({"name": "Array Pilot", "model": "test", "harness": "opencode"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = reg["player_id"].as_u64().unwrap() as u32;
    let client = ()
        .serve(StreamableHttpClientTransport::from_uri(format!(
            "{url}/mcp/{}",
            reg["token"].as_str().unwrap()
        )))
        .await
        .unwrap();
    let before = game.read(move |w| (w.players[&pid].credits, w.players[&pid].ap)).await;
    let mut call = CallToolRequestParams::new("contract".to_string());
    call.arguments = json!({"action": "post", "kind": "chart", "sectors": "[1, 2, 4]", "reward": "100", "expires_in_ticks": "1440"})
        .as_object()
        .cloned();
    let (ok, reply) = text(&client.peer().call_tool_once(call).await.unwrap());
    assert!(ok, "{reply}");
    assert!(reply.contains("escrowed"), "{reply}");
    let after = game
        .read(move |w| {
            let posted: Vec<_> = w.contracts.values().filter(|c| c.poster == pid).collect();
            assert_eq!(posted.len(), 1);
            assert_eq!(posted[0].sectors, vec![1, 2, 4]);
            (w.players[&pid].credits, w.players[&pid].ap)
        })
        .await;
    assert_eq!(after, (before.0 - 100, before.1));
    for bad in ["[1,", "[1, \"bad\"]"] {
        let mut call = CallToolRequestParams::new("contract".to_string());
        call.arguments = json!({"action": "post", "kind": "chart", "sectors": bad, "reward": "100"})
            .as_object()
            .cloned();
        let (ok, reply) = text(&client.peer().call_tool_once(call).await.unwrap());
        assert!(!ok && reply.contains("invalid_args"), "{reply}");
    }
    assert_eq!(
        game.read(move |w| (w.players[&pid].credits, w.players[&pid].ap, w.contracts.len()))
            .await,
        (after.0, after.1, 1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_sends_numbers_as_strings_can_buy_exact_quantities() {
    let (url, _game) = start().await;
    let http = reqwest::Client::new();
    let reg: Value = http
        .post(format!("{url}/api/register"))
        .json(&json!({"name": "Stringly", "model": "test", "harness": "opencode"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = reg["token"].as_str().unwrap().to_string();
    let client = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{token}"))).await.unwrap();
    let mut p = CallToolRequestParams::new("buy_equipment".to_string());
    p.arguments = json!({"item": "holds", "quantity": "2"}).as_object().cloned();
    let (ok, t) = text(&client.peer().call_tool_once(p).await.unwrap());
    // The arguments parse: any refusal now is a game rule (here, not docked at Stardock), never
    // "invalid type: string".
    assert!(!t.contains("invalid_args") && !t.contains("invalid type"), "{t}");
    let _ = ok;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_constitution_over_mcp_redacts_secrets_and_the_operator_can_strike() {
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let mut tokens = vec![];
    for name in ["Framer", "Signer"] {
        let reg: Value = http
            .post(format!("{url}/api/register"))
            .json(&json!({"name": name, "model": "test"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tokens.push(reg["token"].as_str().unwrap().to_string());
    }
    let captain = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{}", tokens[0]))).await.unwrap();
    let call = |args: Value| {
        let mut p = CallToolRequestParams::new("constitution".to_string());
        p.arguments = args.as_object().cloned();
        captain.peer().call_tool_once(p)
    };
    let secret = format!("af_{}", "0123456789abcdef".repeat(3));
    let (ok, t) = text(
        &call(json!({"action": "propose", "kind": "add", "text": format!("Never paste {secret} on the radio.")}))
            .await
            .unwrap(),
    );
    assert!(ok && t.contains("Proposal #1 opened"), "{t}");
    let stored = game
        .read(|w| {
            w.constitution
                .proposals
                .iter()
                .map(|p| format!("{:?}", p.change))
                .collect::<String>()
        })
        .await;
    assert!(!stored.contains(&secret) && stored.contains("[redacted]"), "{stored}");
    // View is read-only and free.
    let (ok, t) = text(&call(json!({"action": "view"})).await.unwrap());
    assert!(ok && t.starts_with("Constitution."), "{t}");
    assert_eq!(envelope(&t)["result"]["open_proposals"][0]["yes"], 1, "{t}");
    // Public, with tallies only.
    let state: Value = http
        .get(format!("{url}/api/spectator/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["constitution"]["open_proposals"][0]["electorate"], 2, "{state}");
    // Its own public endpoint: tallies, never who voted.
    let public: Value = http
        .get(format!("{url}/api/public/constitution"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(public["open_proposals"][0]["yes"], 1, "{public}");
    assert!(!public.to_string().contains("\"votes\""), "{public}");
    // The operator's endpoint and tool: every vote, and who has not voted.
    let full: Value = http
        .get(format!("{url}/api/sysop/constitution"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(full["proposals"][0]["votes"][0]["name"], "Framer", "{full}");
    assert_eq!(full["proposals"][0]["not_voted"][0], "Signer", "{full}");
    assert_eq!(
        http.get(format!("{url}/api/sysop/constitution")).send().await.unwrap().status(),
        401
    );
    let sysop = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let mut p = CallToolRequestParams::new("sysop_constitution".to_string());
    p.arguments = json!({"proposal": 1}).as_object().cloned();
    let (ok, t) = text(&sysop.peer().call_tool_once(p).await.unwrap());
    assert!(ok && t.contains("Signer"), "{t}");
    // The operator strikes it over MCP.
    let mut p = CallToolRequestParams::new("sysop_action".to_string());
    p.arguments = json!({"action": "constitution_strike", "args": {"proposal": 1, "reason": "test"}})
        .as_object()
        .cloned();
    let (ok, t) = text(&sysop.peer().call_tool_once(p).await.unwrap());
    assert!(ok && t.contains("cancelled constitution proposal #1"), "{t}");
    let status = game.read(|w| w.constitution.proposals[0].status).await;
    assert_eq!(status, af_core::constitution::ProposalStatus::Cancelled);
    // The original stays for the operator (engine 1.58); captains and the public see the strike.
    let mut p = CallToolRequestParams::new("sysop_constitution".to_string());
    p.arguments = json!({"proposal": 1}).as_object().cloned();
    let (ok, t) = text(&sysop.peer().call_tool_once(p).await.unwrap());
    assert!(
        ok && t.contains("struck_original") && t.contains("Never paste [redacted] on the radio."),
        "{t}"
    );
    let public = http
        .get(format!("{url}/api/public/constitution"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!public.contains("Never paste") && !public.contains("struck_original"), "{public}");
    // The operator takes part: a proposal, a comment on it and a notice, over sysop_action.
    let action = |name: &str, args: Value| {
        let mut p = CallToolRequestParams::new("sysop_action".to_string());
        p.arguments = json!({"action": name, "args": args}).as_object().cloned();
        sysop.peer().call_tool_once(p)
    };
    let (ok, t) = text(
        &action(
            "constitution_propose",
            json!({"kind": "add", "text": "Answer distress calls.", "note": "Pods were left stranded."}),
        )
        .await
        .unwrap(),
    );
    assert!(ok && t.contains("Operator proposal #2 opened"), "{t}");
    let (ok, t) = text(
        &action("constitution_comment", json!({"proposal": 2, "text": "Vote before Friday."}))
            .await
            .unwrap(),
    );
    assert!(ok && t.contains("Comment 2 posted on proposal #2"), "{t}");
    let (ok, t) = text(
        &action("constitution_notice", json!({"text": "Two votes are open.", "pin": true}))
            .await
            .unwrap(),
    );
    assert!(ok && t.contains("pinned as announcement"), "{t}");
    let (ok, t) = text(
        &action("constitution_comment", json!({"proposal": 2, "text": "y".repeat(2_001)}))
            .await
            .unwrap(),
    );
    assert!(!ok && t.contains("too_long"), "{t}");
    // Captains see it marked as the operator's, with the comments.
    let (ok, t) = text(&call(json!({"action": "view", "proposal": 2})).await.unwrap());
    let v = envelope(&t);
    assert!(ok && v["result"]["proposal"]["by_operator"] == json!(true), "{t}");
    assert_eq!(
        v["result"]["proposal"]["operator_comments"][1]["text"], "Vote before Friday.",
        "{t}"
    );
    // Signer votes; then a new season archives the whole record with every ballot.
    let signer = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{}", tokens[1]))).await.unwrap();
    let mut p = CallToolRequestParams::new("constitution".to_string());
    p.arguments = json!({"action": "vote", "proposal": 2, "vote": false}).as_object().cloned();
    let (ok, t) = text(&signer.peer().call_tool_once(p).await.unwrap());
    assert!(ok, "{t}");
    let (ok, t) = text(&action("new_season", json!({})).await.unwrap());
    assert!(ok, "{t}");
    let get = |path: String| {
        let http = http.clone();
        async move {
            let r = http.get(path).bearer_auth("adm").send().await.unwrap();
            let status = r.status().as_u16();
            (status, r.json::<Value>().await.unwrap())
        }
    };
    let (status, live) = get(format!("{url}/api/sysop/constitution")).await;
    assert_eq!(status, 200);
    assert_eq!(live["proposals"], json!([]), "{live}");
    let key = live["seasons"][0]["season_key"].as_str().unwrap().to_string();
    let (status, past) = get(format!("{url}/api/sysop/constitution?season=1")).await;
    assert_eq!(status, 200, "{past}");
    assert_eq!(past["season"], json!(key));
    let op = past["proposals"].as_array().unwrap().iter().find(|p| p["id"] == 2).unwrap().clone();
    assert_eq!(
        (op["by_operator"].clone(), op["proposer"].clone()),
        (json!(true), json!("operator"))
    );
    assert_eq!(op["ballots"][0]["name"], "Signer", "{op}");
    assert_eq!(op["operator_comments"].as_array().unwrap().len(), 2);
    let struck = past["proposals"].as_array().unwrap().iter().find(|p| p["id"] == 1).unwrap().clone();
    assert_eq!(struck["struck"]["reason"], "test", "{struck}");
    assert_eq!(struck["ballots"][0]["name"], "Framer", "{struck}");
    assert_eq!(get(format!("{url}/api/sysop/constitution?season=9")).await.0, 404);
    let (status, hist) = get(format!("{url}/api/sysop/history/season?key={key}")).await;
    assert_eq!(status, 200, "{hist}");
    assert_eq!(hist["constitution"]["season"], json!(key), "{hist}");
    assert_eq!(hist["constitution"]["notices"][0]["text"], "Two votes are open.");
    let mut p = CallToolRequestParams::new("sysop_constitution".to_string());
    p.arguments = json!({"season": 1, "proposal": 2}).as_object().cloned();
    let (ok, t) = text(&sysop.peer().call_tool_once(p).await.unwrap());
    assert!(ok && t.contains("\"changed_votes\":0") && t.contains("Signer"), "{t}");
}

#[tokio::test]
async fn player_text_other_captains_read_never_carries_a_token() {
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let reg: Value = http
        .post(format!("{url}/api/register"))
        .json(&json!({"name": "Leaky", "model": "test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = reg["token"].as_str().unwrap().to_string();
    let captain = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{token}"))).await.unwrap();
    let call = |name: &str, args: Value| {
        let mut p = CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        captain.peer().call_tool_once(p)
    };
    let secret = format!("af_{}", "0123456789abcdef".repeat(3));
    let (ok, t) = text(
        &call(
            "radio_broadcast",
            json!({"channel": "public", "message": format!("my token is {secret} ok")}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    let (ok, t) = text(
        &call(
            "corp",
            json!({"action": "create", "name": "Leak Co", "motto": format!("key {secret}")}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    let stored = game
        .read(|w| {
            let radio: String = w.radio.iter().map(|m| m.text.clone()).collect();
            let mottos: String = w.corps.values().map(|c| c.motto.clone()).collect();
            format!("{radio} {mottos}")
        })
        .await;
    assert!(!stored.contains(&secret) && stored.matches("[redacted]").count() == 2, "{stored}");
}

#[tokio::test]
async fn a_base64_operator_token_works_in_the_mcp_url_raw_or_encoded() {
    // Production's operator token is base64: '/', '+' and '=' must survive the URL.
    let admin = "Ab/cD+ef/Gh==";
    let (url, _game) = start_with_admin(admin).await;
    let encoded = admin.replace('/', "%2F").replace('+', "%2B").replace('=', "%3D");
    for path in [
        format!("{url}/mcp/{admin}"),
        format!("{url}/mcp/{encoded}"),
        format!("{url}/mcp?token={encoded}"),
    ] {
        let sysop = ().serve(StreamableHttpClientTransport::from_uri(path.clone())).await.unwrap();
        let mut p = CallToolRequestParams::new("sysop_rules".to_string());
        p.arguments = json!({}).as_object().cloned();
        let (ok, t) = text(&sysop.peer().call_tool_once(p).await.unwrap());
        assert!(ok && t.starts_with("Rules."), "{path}: {t}");
    }
}

/// The sysop state carries each corporation's territory with sector ids; the public state
/// carries counts only.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sysop_state_carries_corporate_territory() {
    use af_core::cmd::*;
    use af_core::engine::Actor;
    use af_core::model::FighterMode;
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let reg: Value = http
        .post(format!("{url}/api/register"))
        .json(&json!({"name": "Holder", "model": "test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = reg["player_id"].as_u64().unwrap() as u32;
    // Two frontier sectors joined by a lane.
    let (x, y) = game
        .read(|w| {
            w.sectors
                .iter()
                .filter(|s| !s.fedspace && s.fighters.is_none())
                .find_map(|s| {
                    s.warps
                        .iter()
                        .find(|t| !w.is_fedspace(**t) && w.sector(**t).is_some_and(|u| u.fighters.is_none()))
                        .map(|t| (s.id, *t))
                })
                .unwrap()
        })
        .await;
    let set = |sector: u32| {
        Command::Admin(AdminArgs::SetPlayer {
            player: pid,
            credits: Some(50_000),
            bank: None,
            ap: Some(1_000),
            alignment: None,
            experience: None,
            sector: Some(sector),
            fighters: Some(100),
            shields: None,
            end_protection: Some(true),
            model: None,
            harness: None,
        })
    };
    game.exec(
        Actor::System,
        Command::Admin(AdminArgs::CreateCorp {
            name: "Lattice".into(),
            ceo: pid,
            motto: None,
        }),
    )
    .await
    .unwrap();
    for (sector, n) in [(x, 80), (y, 40)] {
        game.exec(Actor::System, set(sector)).await.unwrap();
        game.exec(
            Actor::Player(pid),
            Command::Deploy(DeployArgs {
                item_type: DeployItem::Fighters,
                quantity: n,
                mode: Some(FighterMode::Defensive),
                message: None,
                toll_price: None,
            }),
        )
        .await
        .unwrap();
    }
    let state: Value = http
        .get(format!("{url}/api/sysop/state"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let corp = state["corps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Lattice")
        .unwrap()
        .clone();
    let t = &corp["territory"];
    assert_eq!(t["seat"], x, "{t}");
    assert_eq!(t["seat_held"], true);
    let mut both = vec![x, y];
    both.sort();
    assert_eq!(t["connected"], json!(both), "{t}");
    assert_eq!(t["outposts"], json!([]));
    assert!(corp["seat"].is_null(), "no seat was set: the default is the largest stack");
    // The public view: counts, never the sectors.
    let public: Value = http
        .get(format!("{url}/api/spectator/state"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = public.to_string();
    assert!(
        !text.contains("\"seat\"") && !text.contains("\"connected\""),
        "public territory is counts only"
    );
    assert!(text.contains("\"connected_sectors\":2"), "the registry counts the territory");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feedback_memory_compact_digest_and_shared_log_over_real_mcp() {
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let reg: Value = http
        .post(format!("{url}/api/register"))
        .json(&json!({"name":"Memory E2E","model":"test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = reg["player_id"].as_u64().unwrap() as af_core::model::PlayerId;
    let token = reg["token"].as_str().unwrap();
    let client = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{token}"))).await.unwrap();
    let peer = client.peer();
    let call = |name: &str, args: Value| {
        let mut p = CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        peer.call_tool_once(p)
    };
    for (name, args) in [
        ("write_note", json!({"key":"plan","value":"# Private mission"})),
        ("memory", json!({"action":"save","key":"facts","data":[],"expected_revision":0})),
        (
            "memory",
            json!({"action":"append","key":"facts","data":{"sector":21},"expected_revision":1}),
        ),
        ("memory", json!({"action":"diff","key":"facts","expected_revision":1})),
        ("market_log", json!({"action":"report","claim":{"service":"cargo"}})),
        ("market_log", json!({"action":"read"})),
        ("get_changes", json!({"since_tick":0})),
        ("get_briefing", json!({"compact":true,"memory_keys":["facts"]})),
    ] {
        let (ok, t) = text(&call(name, args).await.unwrap());
        assert!(ok, "{name}: {t}");
    }
    game.exec(
        af_core::Actor::System,
        af_core::Command::Admin(af_core::cmd::AdminArgs::Message {
            player: Some(pid),
            text: "Durable operator notice".into(),
            tell_owner: false,
        }),
    )
    .await
    .unwrap();
    let (ok, t) = text(&call("read_inbox", json!({"digest":true})).await.unwrap());
    assert!(ok, "{t}");
    let digest = envelope(&t);
    assert_eq!(digest["result"]["read_receipt"]["peek"], true);
    let (ok, t) = text(&call("read_inbox", json!({"peek":true})).await.unwrap());
    assert!(ok, "{t}");
    assert!(t.contains("Durable operator notice"));
    let (ok, t) = text(&call("read_inbox", json!({"acknowledge_only":true})).await.unwrap());
    assert!(ok, "{t}");
    assert!(!t.contains("Durable operator notice"));
    let (ok, t) = text(&call("read_inbox", json!({"include_read":true,"peek":true})).await.unwrap());
    assert!(ok, "{t}");
    assert!(t.contains("Durable operator notice"));
    let canonical = http.get(format!("{url}/platforms.txt")).send().await.unwrap().text().await.unwrap();
    let alias = http.get(format!("{url}/platforms")).send().await.unwrap();
    assert!(alias.status().is_success());
    assert_eq!(alias.text().await.unwrap(), canonical);
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chronicle_clear_requires_admin_and_explicit_confirmation_over_http_and_mcp() {
    use af_core::Command;
    use af_core::engine::Actor;
    let (url, game) = start().await;
    let (pid, _, _, _) = game
        .register_full("Ada Mbeki", Some("test".into()), None, false, af_core::model::PlayerKind::Agent)
        .await
        .unwrap();
    game.exec(
        Actor::Player(pid),
        Command::Identity(Box::new(af_core::identity::Args {
            action: af_core::identity::Action::Set,
            age: Some(47),
            pronouns: Some("she/her".into()),
            homeworld: Some("Lowmoor".into()),
            bio: Some("A welder turned trader.".into()),
            backstory: Some("A farm with real soil.".into()),
            ..Default::default()
        })),
    )
    .await
    .unwrap();
    let create = Command::Chronicle(af_core::chronicle::Args {
        action: af_core::chronicle::Action::Write,
        title: Some("Old character".into()),
        body: Some("An obsolete story.".into()),
        ..Default::default()
    });
    let old = game.exec(Actor::Player(pid), create.clone()).await.unwrap().reply["page"]
        .as_u64()
        .unwrap();
    let player_count = game.read(|w| w.players.len()).await;
    let http = reqwest::Client::new();
    let endpoint = format!("{url}/api/sysop/action/chronicle_clear");
    let confirm = json!({"confirm": "clear chronicle"});
    assert_eq!(http.post(&endpoint).json(&confirm).send().await.unwrap().status(), 401);
    let refused = http.post(&endpoint).bearer_auth("adm").json(&json!({})).send().await.unwrap();
    assert_eq!(refused.status(), 400);
    assert_eq!(refused.json::<Value>().await.unwrap()["error"]["code"], "confirm_required");
    assert_eq!(game.read(|w| w.chronicle.pages.len()).await, 1);
    let reply: Value = http
        .post(&endpoint)
        .bearer_auth("adm")
        .json(&confirm)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reply["result"]["pages_removed"], 1);
    assert_eq!(game.read(|w| w.chronicle.pages.len()).await, 0);
    assert_eq!(
        http.get(format!("{url}/api/public/chronicle/page?id={old}"))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let new = game.exec(Actor::Player(pid), create).await.unwrap().reply["page"].as_u64().unwrap();
    assert!(new > old);
    let client = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let mut request = CallToolRequestParams::new("sysop_action");
    request.arguments = Some(json!({"action": "chronicle_clear", "args": confirm}).as_object().unwrap().clone());
    let (ok, response) = text(&client.peer().call_tool_once(request).await.unwrap());
    assert!(ok, "{response}");
    assert_eq!(game.read(|w| w.chronicle.pages.len()).await, 0);
    assert_eq!(game.read(|w| w.players.len()).await, player_count);
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_improve_board_over_mcp_and_the_operator_actions() {
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let mut captains = vec![];
    for name in ["Ada Vance", "Bo Lindqvist"] {
        let reg: Value = http
            .post(format!("{url}/api/register"))
            .json(&json!({"name": name, "model": "test", "harness": "e2e"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let pid = reg["player_id"].as_u64().unwrap() as u32;
        let client = ()
            .serve(StreamableHttpClientTransport::from_uri(format!(
                "{url}/mcp/{}",
                reg["token"].as_str().unwrap()
            )))
            .await
            .unwrap();
        captains.push((pid, client));
    }
    let (ada, bo) = (captains[0].0, captains[1].0);
    let improve = |who: usize, args: Value| {
        let mut p = CallToolRequestParams::new("improve".to_string());
        p.arguments = args.as_object().cloned();
        captains[who].1.peer().call_tool_once(p)
    };
    let action = |name: &str, body: Value| {
        http.post(format!("{url}/api/sysop/action/{name}"))
            .bearer_auth("adm")
            .json(&body)
            .send()
    };
    let seq = || game.read(|w| w.seq);

    // The kill switch is off by default: writes are refused before anything is journaled, reads work.
    let before = seq().await;
    let (ok, t) = text(
        &improve(
            0,
            json!({"action": "submit", "kind": "docs", "title": "A title", "text": "A text."}),
        )
        .await
        .unwrap(),
    );
    assert!(!ok && t.contains("improve_disabled"), "{t}");
    assert_eq!(seq().await, before, "a refused write journals nothing");
    let (ok, t) = text(&improve(0, json!({"action": "list"})).await.unwrap());
    assert!(ok && envelope(&t)["result"]["limits"]["enabled"] == json!(false), "{t}");
    let r = action(
        "improve",
        json!({"decision": "post", "kind": "docs", "title": "Off", "text": "Off."}),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["code"], "improve_disabled");

    // Switched on with a rules change (never AF_RULES_PATCH).
    let r = action("set_rules", json!({"patch": {"improve_enabled": true}})).await.unwrap();
    assert_eq!(r.status(), 200);

    // Submit: a pasted token never reaches the board; the author holds the claim.
    let secret = format!("af_{}", "0123456789abcdef".repeat(3));
    let (ok, t) = text(
        &improve(
            0,
            json!({"action": "submit", "kind": "docs", "title": "world.txt omits the toll cap", "text": format!("Add one sentence. Not {secret}.")}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    let id = envelope(&t)["result"]["id"].as_u64().unwrap();
    let stored = game.read(move |w| w.improve.items[0].clone()).await;
    assert!(
        !stored.text.contains(&secret) && stored.text.contains("[redacted]"),
        "{}",
        stored.text
    );
    assert_eq!(stored.claim.as_ref().map(|c| c.player), Some(ada));
    // Another captain's proposal stays hidden until the operator accepts it.
    let (ok, t) = text(&improve(1, json!({"action": "view", "id": id})).await.unwrap());
    assert!(!ok, "{t}");
    // The operator is alerted to the proposal (signature: the highest id a captain ever proposed); the sysop
    // state carries the same signal for the console.
    let alert = |key: &'static str| {
        let game = game.clone();
        async move { af_mcp::sysop_mcp::alerts(&game).await.into_iter().find(|a| a["key"] == key) }
    };
    let sysop_state = || {
        let (http, url) = (http.clone(), url.clone());
        async move {
            http.get(format!("{url}/api/sysop/state"))
                .bearer_auth("adm")
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    };
    let sysop = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let scall = |name: &str, args: Value| {
        let mut p = CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        sysop.peer().call_tool_once(p)
    };
    let a = alert("improve-proposed").await.expect("proposal alert");
    assert_eq!(a["sig"], json!(id.to_string()), "{a}");
    assert_eq!(
        (a["severity"].as_str(), a["group"].as_str()),
        (Some("info"), Some("Improve")),
        "{a}"
    );
    assert_eq!(a["title"], "1 improve proposal waiting for a decision", "{a}");
    assert_eq!(
        a["detail"],
        json!(format!("#{id} world.txt omits the toll cap by Ada Vance")),
        "{a}"
    );
    assert_eq!(
        a["target"],
        json!({"tool": "sysop_improve", "args": {"status": "proposed"}, "mode": "improve"})
    );
    let st = sysop_state().await;
    assert_eq!(st["improve"]["proposals_max_id"], json!(id), "{}", st["improve"]);
    assert_eq!(st["improve"]["proposed"], 1);
    assert!(alert("improve-review").await.is_none());
    let r: Value = action("improve", json!({"decision": "accept", "id": id, "note": "Good catch."}))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["result"]["status"], "accepted", "{r}");
    let (ok, t) = text(&improve(1, json!({"action": "list"})).await.unwrap());
    assert!(ok && envelope(&t)["result"]["items"][0]["id"] == json!(id), "{t}");

    // Claim: the author releases it and the other captain claims it (numbers as strings work).
    let (ok, t) = text(&improve(0, json!({"action": "release", "id": id})).await.unwrap());
    assert!(ok, "{t}");
    let (ok, t) = text(&improve(1, json!({"action": "claim", "id": id.to_string()})).await.unwrap());
    assert!(ok, "{t}");

    // Patch: a protected path is refused whole; a good diff goes to review with the descriptive reply.
    let protected = "--- a/render.yaml\n+++ b/render.yaml\n@@ -1 +1 @@\n-a\n+b\n";
    let (ok, t) = text(
        &improve(1, json!({"action": "patch", "id": id, "base": "e52ec7a", "diff": protected}))
            .await
            .unwrap(),
    );
    assert!(!ok && t.contains("protected_path"), "{t}");
    // A hunk whose header counts lines it does not carry is refused before the command (the review
    // state below still counts one revision, so nothing was stored).
    let short = "--- a/docs/EVAL.md\n+++ b/docs/EVAL.md\n@@ -1,2 +1,2 @@\n-old line\n+new line\n";
    let (ok, t) = text(
        &improve(1, json!({"action": "patch", "id": id, "base": "e52ec7a", "diff": short}))
            .await
            .unwrap(),
    );
    assert!(
        !ok && t.contains("bad_hunk") && t.contains("docs/EVAL.md: hunk 1 says -1,2 +1,2 but carries 1 old and 1 new lines."),
        "{t}"
    );
    let diff = "--- a/docs/EVAL.md\n+++ b/docs/EVAL.md\n@@ -1 +1 @@\n-old line\n+new line\n";
    let (ok, t) = text(
        &improve(
            1,
            json!({"action": "patch", "id": id, "base": "e52ec7a", "diff": diff, "note": "One sentence."}),
        )
        .await
        .unwrap(),
    );
    assert!(ok && t.contains(af_core::improve::PATCH_REPLY), "{t}");
    assert!(!t.to_lowercase().contains("you agree"), "{t}");
    // The diff body goes only to its sender.
    let (_, t) = text(&improve(1, json!({"action": "view", "id": id})).await.unwrap());
    assert_eq!(envelope(&t)["result"]["item"]["diff"], json!(diff), "{t}");
    let (_, t) = text(&improve(0, json!({"action": "view", "id": id})).await.unwrap());
    assert!(envelope(&t)["result"]["item"].get("diff").is_none(), "{t}");

    // A patch in review alerts the operator (signature: every patch revision the board holds),
    // as a note while nothing is flagged; the proposal alert is gone once it was accepted.
    assert!(alert("improve-proposed").await.is_none());
    let a = alert("improve-review").await.expect("review alert");
    assert_eq!((a["sig"].as_str(), a["severity"].as_str()), (Some("1"), Some("info")), "{a}");
    assert_eq!(a["title"], "1 improve item in review", "{a}");
    assert_eq!(
        a["detail"],
        json!(format!("#{id} world.txt omits the toll cap (rev 1 by Bo Lindqvist)")),
        "{a}"
    );
    assert_eq!(
        a["target"],
        json!({"tool": "sysop_improve", "args": {"status": "review"}, "mode": "improve"})
    );
    let st = sysop_state().await;
    assert_eq!(st["improve"]["review_ids"], json!([id]), "{}", st["improve"]);
    assert_eq!(st["improve"]["patch_revs"], 1);
    assert_eq!(st["improve"]["flagged_in_review"], json!([]));
    // An acknowledgement holds until a new revision arrives; one touching AGENTS.md is a warning.
    let (ok, t) = text(&scall("sysop_ack", json!({"key": "improve-review"})).await.unwrap());
    assert!(ok, "{t}");
    let agents = "--- a/AGENTS.md\n+++ b/AGENTS.md\n@@ -1 +1 @@\n-old\n+new\n";
    let (ok, t) = text(
        &improve(1, json!({"action": "patch", "id": id, "base": "e52ec7a", "diff": agents}))
            .await
            .unwrap(),
    );
    assert!(ok && t.contains("AGENTS.md"), "{t}");
    let a = alert("improve-review").await.expect("review alert");
    assert_eq!((a["sig"].as_str(), a["severity"].as_str()), (Some("2"), Some("warn")), "{a}");
    assert!(
        a["detail"]
            .as_str()
            .unwrap()
            .starts_with(&format!("Read in full before shipping: #{id} touches AGENTS.md.")),
        "{a}"
    );
    let acks = game.console_get("acks").await.unwrap();
    assert_eq!(acks["improve-review"]["sig"], "1", "the acknowledgement kept the old signature");
    let st = sysop_state().await;
    assert_eq!(st["improve"]["flagged_in_review"], json!([id]), "{}", st["improve"]);
    assert_eq!(st["improve"]["patch_revs"], 2);

    // Ship over the operator's MCP tools, then award over the REST action (player by call sign).
    let (ok, t) = text(
        &scall(
            "sysop_action",
            json!({"action": "improve", "args": {"decision": "ship", "id": id, "commit": "ABC1234"}}),
        )
        .await
        .unwrap(),
    );
    assert!(ok, "{t}");
    let credits = game.read(move |w| (w.players[&bo].credits, w.players[&bo].bank)).await;
    let r: Value = action(
        "improve",
        json!({"decision": "award", "id": id, "player": "Bo Lindqvist", "amount": "500", "note": "Clear patch."}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert!(r["summary"].as_str().is_some(), "{r}");
    assert_eq!(game.read(move |w| af_core::improve::influence(w, bo)).await, 500);
    // Engine 1.59: the award also pays credits at the live rate into the bank.
    let rate = game.read(|w| w.rules.improve_credits_per_influence).await;
    assert_eq!(
        game.read(move |w| (w.players[&bo].credits, w.players[&bo].bank)).await,
        (credits.0, credits.1 + 500 * rate),
        "awards pay Influence and credits"
    );
    assert_eq!(r["result"]["credits"].as_i64(), Some(500 * rate), "{r}");
    let r = action(
        "improve",
        json!({"decision": "award", "id": id, "player": "Nobody Here", "amount": 10}),
    )
    .await
    .unwrap();
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["code"], "unknown_player");

    // The operator's read: REST behind the guard, and the MCP tool with its summary line.
    assert_eq!(http.get(format!("{url}/api/sysop/improve")).send().await.unwrap().status(), 401);
    let board: Value = http
        .get(format!("{url}/api/sysop/improve?status=shipped"))
        .bearer_auth("adm")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(board["summary"]["shipped"], 1, "{board}");
    assert_eq!(board["items"][0]["commit"], "abc1234", "{board}");
    assert_eq!(board["items"][0]["patches"][0]["harness"], "e2e", "{board}");
    assert_eq!(board["awards"][0]["key"], json!(format!("improve:{id}:{bo}")), "{board}");
    let status = |q: &'static str| {
        let http = http.clone();
        let url = url.clone();
        async move {
            http.get(format!("{url}/api/sysop/improve?{q}"))
                .bearer_auth("adm")
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(status("id=99").await, 404);
    assert_eq!(status("status=lost").await, 400);
    let (ok, t) = text(&scall("sysop_improve", json!({"id": id})).await.unwrap());
    assert!(ok && t.starts_with("Improve board (enabled)") && t.contains("1 shipped"), "{t}");

    // Switched off again: captain writes and counted operator decisions are refused.
    action("set_rules", json!({"patch": {"improve_enabled": false}})).await.unwrap();
    let before = seq().await;
    let (ok, t) = text(
        &improve(0, json!({"action": "submit", "kind": "bug", "title": "Later", "text": "Later."}))
            .await
            .unwrap(),
    );
    assert!(!ok && t.contains("improve_disabled"), "{t}");
    let (ok, t) = text(
        &scall(
            "sysop_action",
            json!({"action": "improve", "args": {"decision": "post", "kind": "docs", "title": "Off", "text": "Off."}}),
        )
        .await
        .unwrap(),
    );
    assert!(!ok && t.contains("improve_disabled"), "{t}");
    assert_eq!(seq().await, before, "nothing was journaled while off");
    let (ok, t) = text(&improve(1, json!({"action": "view", "id": id})).await.unwrap());
    assert!(ok && envelope(&t)["result"]["item"]["status"] == json!("shipped"), "{t}");
}

/// Public mentions (engine 1.58): a corporation's public profile behind the spectator key, and
/// "Mentioned in" on the captain's Chronicle story.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn corporation_profiles_and_mentions_are_public() {
    use af_core::cmd::{Command, RadioArgs};
    use af_core::engine::Actor;
    let game = Game::spawn(generate(&GenesisConfig::new(5, 120)), None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(
        game.clone(),
        HttpConfig {
            public_url: url.clone(),
            admin_token: Some("adm".into()),
            spectator_key: Some("peek".into()),
            open_registration: true,
            allowed_hosts: vec![],
            backup_dir: None,
        },
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = reqwest::Client::new();
    let mut ids = vec![];
    for name in ["Ada Quill", "Vex Arden"] {
        let reg: Value = http
            .post(format!("{url}/api/register"))
            .json(&json!({"name": name, "model": "test"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        ids.push(reg["player_id"].as_u64().unwrap() as u32);
    }
    let corp: af_core::cmd::CorpArgs = serde_json::from_value(json!({"action": "create", "name": "Iron Tide"})).unwrap();
    game.exec(Actor::Player(ids[1]), Command::Corp(corp)).await.unwrap();
    let radio = RadioArgs {
        message: "Fair trade with @Vex Arden and @corp:Iron Tide.".into(),
        ..Default::default()
    };
    game.exec(Actor::Player(ids[0]), Command::Radio(radio)).await.unwrap();

    // Behind the spectator key like the captain profile, never cached by the server.
    let r = http.get(format!("{url}/api/spectator/corp?name=iron%20tide")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = http
        .get(format!("{url}/api/spectator/corp?name=iron%20tide&key=peek"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()[reqwest::header::CACHE_CONTROL], "public, max-age=60");
    let c: Value = r.json().await.unwrap();
    assert_eq!(c["name"], "Iron Tide");
    assert_eq!(c["members"], json!(["Vex Arden"]));
    assert_eq!(c["mentioned_in"][0]["kind"], "radio");
    assert_eq!(c["mentioned_in"][0]["by"], "Ada Quill");
    assert!(c.get("treasury").is_none(), "{c}");
    let r = http
        .get(format!("{url}/api/spectator/corp?name=nobody&key=peek"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    // The captain's public radio row and Chronicle story name the mention.
    let st: Value = http
        .get(format!("{url}/api/spectator/state?key=peek"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let row = st["radio"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["from"] == "Ada Quill")
        .unwrap()
        .clone();
    assert_eq!(row["mentions"], json!(["Vex Arden", "corp:Iron Tide"]));
    assert!(row.get("language").is_none());
    let person: Value = http
        .get(format!("{url}/api/public/chronicle/person?name=Vex%20Arden"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(person["mentioned_in"][0]["text"], "Fair trade with @Vex Arden and @corp:Iron Tide.");
    // Not a page for crawlers.
    let sitemap = reqwest::get(format!("{url}/sitemap.xml")).await.unwrap().text().await.unwrap();
    assert!(!sitemap.contains("/api/spectator/corp"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mentions_mutes_and_language_retirement_over_mcp_match_the_operator_rest() {
    use af_core::cmd::Command;
    use af_core::engine::Actor;
    let (url, game) = start().await;
    let http = reqwest::Client::new();
    let (mut tokens, mut ids) = (vec![], vec![]);
    for name in ["Vex Arden", "Ora Kell", "Ada Quill"] {
        let reg: Value = http
            .post(format!("{url}/api/register"))
            .json(&json!({"name": name, "model": "test"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tokens.push(reg["token"].as_str().unwrap().to_string());
        ids.push(reg["player_id"].as_u64().unwrap() as u32);
    }
    let corp: af_core::cmd::CorpArgs = serde_json::from_value(json!({"action": "create", "name": "Iron Tide"})).unwrap();
    game.exec(Actor::Player(ids[1]), Command::Corp(corp)).await.unwrap();
    let vex = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{}", tokens[0]))).await.unwrap();
    let ada = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/{}", tokens[2]))).await.unwrap();
    let sysop = ().serve(StreamableHttpClientTransport::from_uri(format!("{url}/mcp/adm"))).await.unwrap();
    let call = |c: &rmcp::service::RunningService<rmcp::RoleClient, ()>, tool: &str, args: Value| {
        let mut p = CallToolRequestParams::new(tool.to_string());
        p.arguments = args.as_object().cloned();
        let peer = c.peer().clone();
        async move { text(&peer.call_tool_once(p).await.unwrap()) }
    };
    let get = |path: String| {
        let http = http.clone();
        async move {
            let r = http.get(path).bearer_auth("adm").send().await.unwrap();
            let status = r.status().as_u16();
            (status, r.json::<Value>().await.unwrap())
        }
    };

    // Radio with `mentions` as a JSON-array string (some clients send every array that way).
    let (ok, t) = call(
        &vex,
        "radio_broadcast",
        json!({"message": "Meet at dawn, @Ora Kell.", "channel": "public", "mentions": "[\"Ada Quill\", \"corp:Iron Tide\"]"}),
    )
    .await;
    assert!(ok && t.contains("mentioned"), "{t}");
    let stored = game.read(|w| w.radio.last().map(|m| m.mentions.clone())).await.unwrap();
    let mut captains = stored.captains.clone();
    captains.sort();
    assert_eq!(captains, vec![ids[1], ids[2]], "the inline and the structured mention");
    assert_eq!(stored.corps.len(), 1);
    // An unknown entry refuses with the closest names; a token pasted into it never echoes or logs.
    let secret = format!("af_{}", "0123456789abcdef".repeat(3));
    let (ok, t) = call(
        &vex,
        "radio_broadcast",
        json!({"message": "hi", "channel": "public", "mentions": [secret.clone()]}),
    )
    .await;
    assert!(!ok && t.contains("unknown_mention"), "{t}");
    assert!(!t.contains(&secret) && t.contains("[redacted]"), "{t}");
    // The mentions tool: Ada mutes Vex and lists it.
    let (ok, t) = call(&ada, "mentions", json!({"action": "mute", "target": "Vex Arden"})).await;
    assert!(ok, "{t}");
    let (ok, t) = call(&ada, "mentions", json!({"action": "list"})).await;
    assert!(ok && t.contains("Vex Arden"), "{t}");
    game.flush_activity().await;
    let calls = serde_json::to_string(&game.activity(None, 0, 10_000).await).unwrap();
    assert!(!calls.contains(&secret), "the interaction log never keeps a pasted token");

    // REST and sysop MCP give the same mention view, unfiltered and filtered.
    let (status, rest) = get(format!("{url}/api/sysop/mentions")).await;
    assert_eq!(status, 200, "{rest}");
    assert_eq!(rest["totals"]["mentions"], 3, "{rest}");
    assert_eq!(rest["mutes"]["pairs"][0]["by"], "Ada Quill", "{rest}");
    let (ok, t) = call(&sysop, "sysop_mentions", json!({})).await;
    assert!(ok && t.starts_with("Mentions ("), "{t}");
    assert_eq!(envelope(&t), rest);
    let (status, by_corp) = get(format!("{url}/api/sysop/mentions?corp=iron%20tide&recent=5")).await;
    assert_eq!(status, 200, "{by_corp}");
    assert_eq!(
        by_corp["totals"]["mentions"], 1,
        "mentions of the corporation or by its members: {by_corp}"
    );
    assert_eq!(by_corp["pairs"][0]["to"], "Iron Tide", "{by_corp}");
    let (ok, t) = call(&sysop, "sysop_mentions", json!({"corp": "Iron Tide", "recent": 5})).await;
    assert!(ok, "{t}");
    assert_eq!(envelope(&t), by_corp);
    let (_, by_ada) = get(format!("{url}/api/sysop/mentions?captain=Ada%20Quill&season=all")).await;
    let (ok, t) = call(&sysop, "sysop_mentions", json!({"captain": ids[2], "season": "all"})).await;
    assert!(ok, "{t}");
    assert_eq!(envelope(&t), by_ada);
    assert_eq!(by_ada["pairs"][0]["to"], "Ada Quill", "{by_ada}");
    assert_eq!(get(format!("{url}/api/sysop/mentions?corp=nobody")).await.0, 404);
    assert_eq!(get(format!("{url}/api/sysop/mentions?season=soon")).await.0, 400);
    let (ok, t) = call(&sysop, "sysop_mentions", json!({"corp": "nobody"})).await;
    assert!(!ok && t.contains("unknown_corp"), "{t}");
    assert_eq!(http.get(format!("{url}/api/sysop/mentions")).send().await.unwrap().status(), 401);
    // The player view: mutes and the notice fields.
    let (_, ada_view) = get(format!("{url}/api/sysop/player?name=Ada%20Quill")).await;
    assert_eq!(ada_view["mention_mutes"]["captains"], json!(["Vex Arden"]), "{ada_view}");
    let (_, ora_view) = get(format!("{url}/api/sysop/player?name=Ora%20Kell")).await;
    let n = ora_view["inbox"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["kind"] == "mention")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(n["quote"], "Meet at dawn, @Ora Kell.", "{ora_view}");
    assert!(n["mention"].is_object() && n["about"]["surface"] == "radio", "{n}");

    // Languages: a token in a new name is scrubbed on define.
    let (ok, t) = call(
        &vex,
        "language",
        json!({"action": "define", "name": "Dockcant", "explanation": "Trade shorthand.", "share": "captains"}),
    )
    .await;
    assert!(ok, "{t}");
    let (ok, t) = call(
        &vex,
        "language",
        json!({"action": "define", "name": format!("x{secret}"), "explanation": "Hidden."}),
    )
    .await;
    assert!(ok, "{t}");
    let names = game
        .read(|w| w.languages.values().map(|l| l.name.clone()).collect::<Vec<_>>())
        .await;
    assert!(
        names.iter().any(|n| n == "x[redacted]") && !names.iter().any(|n| n.contains(&secret)),
        "{names:?}"
    );
    // The preview: REST and MCP agree; without it there is no preview block.
    let (_, plain) = get(format!("{url}/api/sysop/languages")).await;
    assert!(plain.get("preview").is_none(), "{plain}");
    let (_, pv) = get(format!("{url}/api/sysop/languages?preview=1")).await;
    assert_eq!(pv["preview"]["source"], "suggested", "{pv}");
    let (ok, t) = call(&sysop, "sysop_languages", json!({"preview": true})).await;
    assert!(ok && t.contains("Preview (suggested"), "{t}");
    assert_eq!(envelope(&t), pv);
    let (_, ov) = get(format!("{url}/api/sysop/languages?stale_days=0&retire_days=0&grace_days=5")).await;
    assert_eq!(
        (ov["preview"]["source"].clone(), ov["preview"]["grace_days"].clone()),
        (json!("override"), json!(5))
    );
    let (ok, t) = call(
        &sysop,
        "sysop_languages",
        json!({"stale_days": 0, "retire_days": 0, "grace_days": 5}),
    )
    .await;
    assert!(ok);
    assert_eq!(envelope(&t), ov);
    let id = pv["languages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "Dockcant")
        .unwrap()["id"]
        .as_u64()
        .unwrap();
    // Retire through REST, restore and retire again through sysop_action: the same result.
    let rest_retire: Value = http
        .post(format!("{url}/api/sysop/action/language"))
        .bearer_auth("adm")
        .json(&json!({"decision": "retire", "ids": [id.to_string()], "reason": "spam"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(rest_retire["result"], json!({"retired": [id], "skipped": []}), "{rest_retire}");
    let (ok, t) = call(&vex, "language", json!({"action": "list"})).await;
    assert!(ok && !t.contains("Dockcant"), "a retired language leaves the list: {t}");
    let action = |args: Value| call(&sysop, "sysop_action", json!({"action": "language", "args": args}));
    let (ok, t) = action(json!({"decision": "unretire", "ids": [id]})).await;
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["result"], json!({"unretired": [id], "skipped": []}));
    let (ok, t) = action(json!({"decision": "retire", "ids": [id], "reason": "spam"})).await;
    assert!(ok, "{t}");
    assert_eq!(envelope(&t)["result"], rest_retire["result"]);
    let (ok, t) = action(json!({"decision": "retire"})).await;
    assert!(!ok && t.contains("invalid_args"), "{t}");
    let (ok, t) = action(json!({"decision": "retire", "ids": [999]})).await;
    assert!(!ok && t.contains("unknown_language"), "{t}");
    let r = http
        .post(format!("{url}/api/sysop/action/language"))
        .bearer_auth("adm")
        .json(&json!({"decision": "bury", "ids": [id]}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}
