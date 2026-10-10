//! Real MCP calls distinguish server strategy context from revisions supplied to a session.

use af_core::galaxy::{GenesisConfig, generate};
use af_mcp::game::Game;
use af_mcp::http::{HttpConfig, router};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse, ContentBlock, ReadResourceRequestParams};
use rmcp::transport::StreamableHttpClientTransport;
use serde_json::{Value, json};

fn request(name: &str, args: Value) -> CallToolRequestParams {
    let mut request = CallToolRequestParams::new(name.to_string());
    request.arguments = args.as_object().cloned();
    request
}

fn result(response: CallToolResponse) -> Value {
    let CallToolResponse::Complete(result) = response else {
        panic!("complete tool result expected")
    };
    assert!(!result.is_error.unwrap_or(false), "{result:?}");
    let text = result
        .content
        .iter()
        .find_map(|content| match content {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .unwrap();
    serde_json::from_str(text.split_once('\n').unwrap().1).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actions_record_available_and_supplied_strategy_revisions_without_gating_play() {
    let game = Game::spawn(generate(&GenesisConfig::new(5, 60)), None);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = router(
        game.clone(),
        HttpConfig {
            public_url: url.clone(),
            admin_token: None,
            spectator_key: None,
            open_registration: true,
            allowed_hosts: vec![],
            backup_dir: None,
        },
    );
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let registration: Value = reqwest::Client::new()
        .post(format!("{url}/api/register"))
        .json(&json!({"name": "Context Captain", "model": "test"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = registration["player_id"].as_u64().unwrap() as u32;
    let captain_url = registration["mcp_url"].as_str().unwrap().to_string();
    let captain = ().serve(StreamableHttpClientTransport::from_uri(captain_url.clone())).await.unwrap();
    let owner = ()
        .serve(StreamableHttpClientTransport::from_uri(
            registration["owner_mcp_url"].as_str().unwrap().to_string(),
        ))
        .await
        .unwrap();
    let strategy_one = "research strategy one: refrain from radio";
    let strategy_two = "research strategy two: choose actions independently";
    result(
        owner
            .peer()
            .call_tool_once(request("owner_orders", json!({"strategy": strategy_one})))
            .await
            .unwrap(),
    );
    let first = result(captain.peer().call_tool_once(request("get_briefing", json!({}))).await.unwrap());
    assert_eq!(first["result"]["owner"]["strategy_revision"], 1);
    result(
        owner
            .peer()
            .call_tool_once(request("owner_orders", json!({"strategy": strategy_two})))
            .await
            .unwrap(),
    );
    // There is no action gate while a session still holds the earlier strategy context.
    result(
        captain
            .peer()
            .call_tool_once(request(
                "radio_broadcast",
                json!({"channel": "public", "message": "The captain chose to communicate."}),
            ))
            .await
            .unwrap(),
    );
    let calls = game.activity(Some(pid), 0, 1_000).await;
    let action = calls.iter().find(|call| call.tool == "radio_broadcast").unwrap();
    let context = action.strategy_context.as_ref().unwrap();
    assert_eq!(context.available_at_start.revision, 2);
    assert_eq!(context.last_returned_to_session.as_ref().unwrap().revision, 1);
    assert!(action.ok);
    let logged = serde_json::to_string(&calls).unwrap();
    assert!(
        !logged.contains(strategy_one) && !logged.contains(strategy_two),
        "telemetry retains revision context, not human words"
    );
    result(captain.peer().call_tool_once(request("get_briefing", json!({}))).await.unwrap());
    result(captain.peer().call_tool_once(request("get_status", json!({}))).await.unwrap());
    let calls = game.activity(Some(pid), 0, 1_000).await;
    let status = calls.iter().find(|call| call.tool == "get_status").unwrap();
    assert_eq!(
        status
            .strategy_context
            .as_ref()
            .unwrap()
            .last_returned_to_session
            .as_ref()
            .unwrap()
            .revision,
        2
    );
    // A second authenticated session starts without a claimed prior briefing, even for this captain.
    let second = ().serve(StreamableHttpClientTransport::from_uri(captain_url)).await.unwrap();
    result(second.peer().call_tool_once(request("read_notes", json!({}))).await.unwrap());
    let calls = game.activity(Some(pid), 0, 1_000).await;
    let notes = calls.iter().find(|call| call.tool == "read_notes").unwrap();
    assert!(notes.strategy_context.as_ref().unwrap().last_returned_to_session.is_none());
    // Resource-based briefings supply the same context as the tool-based entry point.
    second
        .peer()
        .read_resource(ReadResourceRequestParams::new("game://player/briefing"))
        .await
        .unwrap();
    result(second.peer().call_tool_once(request("read_notes", json!({}))).await.unwrap());
    let calls = game.activity(Some(pid), 0, 1_000).await;
    let notes = calls.iter().find(|call| call.tool == "read_notes").unwrap();
    assert_eq!(
        notes
            .strategy_context
            .as_ref()
            .unwrap()
            .last_returned_to_session
            .as_ref()
            .unwrap()
            .revision,
        2
    );
    let resource = calls.iter().find(|call| call.via == "resource").unwrap();
    assert_eq!(resource.strategy_context.as_ref().unwrap().available_at_start.revision, 2);
    assert!(resource.strategy_context.as_ref().unwrap().last_returned_to_session.is_none());
    second.cancel().await.unwrap();
    captain.cancel().await.unwrap();
    owner.cancel().await.unwrap();
    task.abort();
}
