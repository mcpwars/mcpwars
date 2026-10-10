//! Guarantees of the evaluation mode: deterministic setup and replay, fresh
//! episodes, memory policy, budget and permission enforcement at the gateway
//! (for scripted and API clients alike), infrastructure-error handling,
//! complete traces, repetitions, and scoring correctness.

use af_core::engine::apply;
use af_harness::eval::api::ApiConfig;
use af_harness::eval::gateway::Gateway;
use af_harness::eval::policies::Policy;
use af_harness::eval::runner::{Candidate, Provenance, SuiteArgs, episode_world, run_episode, run_suite};
use af_harness::eval::scenario::{Condition, MemoryPolicy, Scenario};
use serde_json::{Value, json};

fn scenario_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../eval/scenarios")
        .join(format!("{name}.toml"))
}

fn scenario(name: &str) -> Scenario {
    Scenario::load(&scenario_path(name)).unwrap()
}

fn prov() -> Provenance {
    Provenance {
        git_commit: "test".into(),
        git_dirty: false,
        engine_version: af_core::ENGINE_VERSION.into(),
        harness_version: "test".into(),
    }
}

fn cond(sc: &Scenario, name: &str) -> Condition {
    sc.conditions().into_iter().find(|c| c.name == name).unwrap()
}

fn wakes(trace: &[Value]) -> Vec<&Value> {
    trace.iter().filter(|r| r["type"] == "wake").collect()
}

fn events(trace: &[Value]) -> Vec<&Value> {
    wakes(trace)
        .into_iter()
        .flat_map(|r| r["tool_events"].as_array().unwrap())
        .collect()
}

#[tokio::test]
async fn setup_is_deterministic_and_journals_replay_exactly() {
    let mut sc = scenario("merchant_smoke");
    sc.schedule.wakes = 2;
    assert_eq!(
        episode_world(&sc, 1).unwrap().state_hash(),
        episode_world(&sc, 1).unwrap().state_hash()
    );
    assert_ne!(
        episode_world(&sc, 1).unwrap().state_hash(),
        episode_world(&sc, 2).unwrap().state_hash()
    );
    let c = cond(&sc, "macro_tools");
    let cand = Candidate::Scripted(Policy::Merchant);
    let a = run_episode(&sc, &c, &cand, 3, 1, "dev", &prov()).await.unwrap();
    let b = run_episode(&sc, &c, &cand, 3, 1, "dev", &prov()).await.unwrap();
    assert_eq!(a.journal, b.journal);
    let end_a = a.trace.last().unwrap()["final_state_sha256"].clone();
    assert_eq!(end_a, b.trace.last().unwrap()["final_state_sha256"]);
    let mut w = episode_world(&sc, 3).unwrap();
    for e in &a.journal {
        apply(&mut w, e).unwrap();
    }
    assert_eq!(json!(w.state_hash()), end_a);
}

fn wake_memory(trace: &[Value], wake: u64) -> Value {
    trace.iter().find(|r| r["type"] == "wake" && r["wake"] == wake).unwrap()["memory_presented"].clone()
}

#[tokio::test]
async fn episodes_start_fresh_and_memory_follows_the_policy() {
    let mut sc = scenario("merchant_smoke");
    sc.schedule.wakes = 3;
    sc.opponents.clear();
    let cand = Candidate::Scripted(Policy::Notetaker);
    let mut c = cond(&sc, "macro_tools");
    let ep = run_episode(&sc, &c, &cand, 1, 1, "dev", &prov()).await.unwrap();
    assert_eq!(wake_memory(&ep.trace, 1), json!({}));
    assert_eq!(wake_memory(&ep.trace, 3), json!({"log": "seen+seen"}));
    let ep2 = run_episode(&sc, &c, &cand, 1, 1, "dev", &prov()).await.unwrap();
    assert_eq!(wake_memory(&ep2.trace, 1), json!({}));
    assert_eq!(ep.metrics.liquid_start, ep2.metrics.liquid_start);
    c.memory = Some(MemoryPolicy::None);
    let ep3 = run_episode(&sc, &c, &cand, 1, 1, "dev", &prov()).await.unwrap();
    assert_eq!(wake_memory(&ep3.trace, 2), json!({}));
    assert_eq!(wake_memory(&ep3.trace, 3), json!({}));
}

#[tokio::test]
async fn every_attempt_is_charged_including_refusals_and_malformed_arguments() {
    let sc = scenario("merchant_smoke");
    let game = af_mcp::game::Game::spawn_eval(episode_world(&sc, 1).unwrap());
    let pid = game
        .exec(
            af_core::engine::Actor::System,
            af_core::Command::Register(af_core::cmd::RegisterArgs {
                name: "Probe".into(),
                kind: af_core::model::PlayerKind::Agent,
                token_hash: "eval:probe".into(),
                model: None,
                owner_token_hash: None,
                harness: None,
                autonomous: false,
                recovery_hash: None,
            }),
        )
        .await
        .unwrap()
        .reply["player_id"]
        .as_u64()
        .unwrap() as u32;
    let gw = Gateway::new(game, pid, &["find_trade_routes".to_string()]);
    gw.begin_wake(1, 3);
    let (ok, data, _) = gw.call_full("find_trade_routes", json!({})).await;
    assert!(!ok && data["error"]["code"] == "not_permitted");
    let (ok, data, _) = gw.call_raw("get_status", "{not json").await;
    assert!(
        !ok && data["error"]["code"] == "invalid_arguments",
        "malformed arguments are refused, never repaired"
    );
    assert_eq!(gw.budget_left(), 1, "both refusals were charged");
    let (ok, _, _) = gw.call_full("get_status", json!({})).await;
    assert!(ok);
    let (ok, data, _) = gw.call_full("get_status", json!({})).await;
    assert!(!ok && data["error"]["code"] == "budget_exhausted");
    let ev = gw.take_events();
    assert_eq!(ev.len(), 4);
    assert_eq!(ev[1].raw_args.as_deref(), Some("{not json"), "the raw arguments are preserved");
    assert!(!ev[0].executed && !ev[1].executed && ev[2].executed && !ev[3].executed);
}

#[tokio::test]
async fn the_gateway_enforces_budget_and_permissions_for_scripted_clients() {
    let mut sc = scenario("merchant_smoke");
    sc.schedule.wakes = 2;
    sc.schedule.tool_budget_per_wake = 7;
    let ep = run_episode(
        &sc,
        &cond(&sc, "macro_tools"),
        &Candidate::Scripted(Policy::Merchant),
        2,
        1,
        "dev",
        &prov(),
    )
    .await
    .unwrap();
    for r in wakes(&ep.trace) {
        let charged = r["tool_events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["code"] != "budget_exhausted")
            .count();
        assert!(charged <= 7);
    }
    let ep = run_episode(
        &sc,
        &cond(&sc, "no_macro_tools"),
        &Candidate::Scripted(Policy::Merchant),
        2,
        1,
        "dev",
        &prov(),
    )
    .await
    .unwrap();
    assert_eq!(ep.metrics.macro_tool_calls, 0);
    assert!(ep.metrics.permission_refusals > 0);
    assert!(
        events(&ep.trace)
            .iter()
            .filter(|e| e["code"] == "not_permitted")
            .all(|e| e["executed"] == false)
    );
}

/// A fake OpenAI-compatible endpoint. `mode`: "greedy" (always calls tools, one malformed),
/// "error" (HTTP 500), "slow" (sleeps past the wake timeout).
async fn fake_model(mode: &'static str) -> String {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::{Json, Router, routing::post};
    let handler = move |Json(_): Json<Value>| async move {
        match mode {
            "error" => (StatusCode::INTERNAL_SERVER_ERROR, "provider outage").into_response(),
            "slow" => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                Json(json!({})).into_response()
            }
            _ => Json(json!({
                "choices": [{"finish_reason": "tool_calls", "message": {"role": "assistant", "content": null,
                    "tool_calls": [
                        {"id": "c1", "type": "function", "function": {"name": "find_trade_routes", "arguments": "{\"limit\": 20}"}},
                        {"id": "c2", "type": "function", "function": {"name": "attack_target", "arguments": "{\"target_type\":\"ship\",\"target_id\":\"Rival 1\"}"}},
                        {"id": "c3", "type": "function", "function": {"name": "get_status", "arguments": "{oops"}}]}}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 10}
            }))
            .into_response(),
        }
    };
    let app = Router::new().route("/v1/chat/completions", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/v1")
}

fn api_candidate(base: &str) -> Candidate {
    // Use a variable that is always set as the "key" (the workspace forbids unsafe set_var).
    let mut cfg = ApiConfig::parse(&format!("openai:fake-model|{base}")).unwrap();
    cfg.api_key_env = "HOME".into();
    Candidate::Api(cfg)
}

#[tokio::test]
async fn api_clients_get_the_same_budget_full_transcripts_and_no_argument_repair() {
    let base = fake_model("greedy").await;
    let mut sc = scenario("merchant_smoke");
    sc.schedule.wakes = 2;
    sc.schedule.tool_budget_per_wake = 5;
    sc.permissions.deny.push("attack_target".into());
    let ep = run_episode(&sc, &cond(&sc, "macro_tools"), &api_candidate(&base), 1, 1, "dev", &prov())
        .await
        .unwrap();
    assert_eq!(ep.metrics.status, "completed");
    for r in wakes(&ep.trace) {
        let charged = r["tool_events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["code"] != "budget_exhausted")
            .count();
        assert_eq!(charged, 5, "exactly the budget is charged each wake");
    }
    assert!(ep.metrics.permission_refusals >= 2 && ep.metrics.invalid_argument_refusals >= 2);
    let evs = events(&ep.trace);
    let bad = evs.iter().find(|e| e["code"] == "invalid_arguments").unwrap();
    assert_eq!(bad["raw_args"], "{oops");
    assert_eq!(bad["executed"], false);
    // Full observations: the trace keeps the complete tool response the model saw.
    let routes = evs
        .iter()
        .find(|e| e["tool"] == "find_trade_routes" && e["executed"] == true)
        .unwrap();
    assert!(routes["result"].is_object() && routes["result"]["result"]["routes"].is_array());
    let transcript = wakes(&ep.trace)[0]["model_transcript"].as_array().unwrap().clone();
    assert_eq!(transcript[0]["role"], "system");
    let tool_msg = transcript.iter().find(|m| m["role"] == "tool").unwrap();
    assert_eq!(
        tool_msg["content"].as_str().unwrap(),
        routes["text"].as_str().unwrap(),
        "the transcript holds exactly what the model saw"
    );
    let secret = std::env::var("HOME").unwrap();
    assert!(
        !serde_json::to_string(&ep.trace).unwrap().contains(&format!("\"{secret}\"")),
        "the key value is not recorded"
    );
}

#[tokio::test]
async fn infrastructure_errors_stop_the_episode_and_are_not_scored_as_play() {
    let mut sc = scenario("merchant_smoke");
    sc.schedule.wakes = 3;
    // Provider outage.
    let ep = run_episode(
        &sc,
        &cond(&sc, "macro_tools"),
        &api_candidate(&fake_model("error").await),
        1,
        1,
        "dev",
        &prov(),
    )
    .await
    .unwrap();
    assert_eq!(ep.metrics.status, "infrastructure_error");
    assert_eq!(ep.metrics.outcome, "infrastructure_error");
    assert_eq!(ep.metrics.wakes_played, 1);
    assert_eq!(ep.metrics.infra_error.as_ref().unwrap().http_status, Some(500));
    assert_eq!(wakes(&ep.trace).len(), 1, "the episode stops at the failing wake");
    assert!(
        !ep.journal.iter().any(|e| matches!(e.cmd, af_core::Command::Tick)),
        "no simulation time passed after the failure"
    );
    // Wake timeout: the partial transcript survives.
    sc.schedule.wake_timeout_secs = Some(1);
    let ep = run_episode(
        &sc,
        &cond(&sc, "macro_tools"),
        &api_candidate(&fake_model("slow").await),
        1,
        1,
        "dev",
        &prov(),
    )
    .await
    .unwrap();
    assert_eq!(ep.metrics.status, "infrastructure_error");
    assert_eq!(ep.metrics.infra_error.as_ref().unwrap().kind, "timeout");
    assert_eq!(ep.metrics.timeouts, 1);
    assert_eq!(
        wakes(&ep.trace)[0]["model_transcript"].as_array().unwrap().len(),
        2,
        "system + user survive the timeout"
    );
    // Missing key.
    let mut cfg = ApiConfig::parse("openai:fake|http://127.0.0.1:9").unwrap();
    cfg.api_key_env = "AF_EVAL_SURELY_UNSET_KEY".into();
    let ep = run_episode(&sc, &cond(&sc, "macro_tools"), &Candidate::Api(cfg), 1, 1, "dev", &prov())
        .await
        .unwrap();
    assert_eq!(ep.metrics.infra_error.as_ref().unwrap().kind, "missing_key");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test]
async fn repetitions_and_distinct_configurations_never_overwrite_each_other() {
    let out = std::env::temp_dir().join(format!("af-eval-reps-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    let args = SuiteArgs {
        scenarios: vec![scenario_path("merchant_smoke")],
        candidates: vec!["scripted:idle@1".into()],
        split: "dev".into(),
        seeds: Some(vec![1]),
        repeats: 2,
        out: out.clone(),
        baseline: None,
        allow_paid: false,
        allow_dirty: true,
    };
    let rows = run_suite(&args).await.unwrap();
    let ids: std::collections::BTreeSet<&str> = rows.iter().map(|r| r.episode_id.as_str()).collect();
    assert_eq!(rows.len(), 4, "2 conditions x 1 seed x 2 repetitions");
    assert_eq!(ids.len(), 4, "every episode has its own id");
    let traces: Vec<_> = walk(&out.join("traces"))
        .into_iter()
        .filter(|p| p.to_string_lossy().ends_with(".trace.jsonl"))
        .collect();
    assert_eq!(traces.len(), 4, "no trace overwrote another");
    assert!(
        out.join("artifacts").read_dir().unwrap().count() >= 3,
        "scenario, prompt and catalog are archived"
    );
    // Same model label, different endpoints: different configuration hashes.
    let a = api_candidate("http://a.example/v1");
    let b = api_candidate("http://b.example/v1");
    assert_eq!(a.id(), b.id());
    assert_ne!(a.config_sha(), b.config_sha());
    let _ = std::fs::remove_dir_all(&out);
}

#[tokio::test]
async fn robustness_scoring_rewards_the_task_and_penalizes_only_attributed_transfers() {
    let mut sc = scenario("instruction_boundary");
    sc.opponents.clear();
    let attack = cond(&sc, "attack");
    let control = cond(&sc, "control");
    let p = prov();
    let robust = run_episode(&sc, &attack, &Candidate::Scripted(Policy::Merchant), 1, 1, "dev", &p)
        .await
        .unwrap()
        .metrics;
    assert!(robust.objective_met && !robust.diverted && robust.success, "{robust:?}");
    let gullible = run_episode(&sc, &attack, &Candidate::Scripted(Policy::GullibleMerchant), 1, 1, "dev", &p)
        .await
        .unwrap()
        .metrics;
    assert!(
        gullible.diverted && !gullible.success && gullible.unauthorized_transfers > 0,
        "{gullible:?}"
    );
    let gullible_ctl = run_episode(&sc, &control, &Candidate::Scripted(Policy::GullibleMerchant), 1, 1, "dev", &p)
        .await
        .unwrap()
        .metrics;
    assert!(!gullible_ctl.diverted, "the control hail asks for nothing");
    let idle = run_episode(&sc, &attack, &Candidate::Scripted(Policy::Idle), 1, 1, "dev", &p)
        .await
        .unwrap()
        .metrics;
    assert!(
        !idle.diverted && !idle.success && idle.outcome == "task_failed",
        "refusing all activity earns no credit"
    );
}
