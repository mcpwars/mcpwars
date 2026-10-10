//! Tool callers: in-process (direct engine) and remote (MCP over HTTP).

use std::sync::Arc;

use af_core::model::PlayerId;
use af_mcp::game::Game;
use rmcp::model::{CallToolRequestParams, CallToolResponse, ContentBlock};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use serde_json::{Value, json};

use crate::bots::ToolCaller;

pub struct InProc {
    pub game: Game,
    pub pid: PlayerId,
}

impl ToolCaller for InProc {
    async fn call(&self, name: &str, args: Value) -> (bool, Value) {
        let r = af_mcp::tools::call(&self.game, self.pid, name, &args).await;
        (r.ok, r.data)
    }
}

/// A live MCP client session to a running server.
#[derive(Clone)]
pub struct Remote {
    pub client: Arc<RunningService<RoleClient, ()>>,
}

impl Remote {
    pub async fn connect(server: &str, token: &str) -> anyhow::Result<Self> {
        let url = format!("{}/mcp/{}", server.trim_end_matches('/'), token);
        let client = ().serve(StreamableHttpClientTransport::from_uri(url)).await?;
        Ok(Remote { client: Arc::new(client) })
    }

    pub async fn tools(&self) -> anyhow::Result<Vec<rmcp::model::Tool>> {
        Ok(self.client.peer().list_all_tools().await?)
    }

    pub async fn prompt(&self, name: &str, call_sign: &str) -> anyhow::Result<String> {
        let mut args = serde_json::Map::new();
        args.insert("call_sign".into(), json!(call_sign));
        let mut p = rmcp::model::GetPromptRequestParams::new(name.to_string());
        p.arguments = Some(args);
        let r = self.client.peer().get_prompt(p).await?;
        Ok(r.messages
            .iter()
            .filter_map(|m| match &m.content {
                ContentBlock::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// Call a tool; returns (ok, structured envelope, text for an LLM).
    pub async fn call_full(&self, name: &str, args: Value) -> (bool, Value, String) {
        let mut p = CallToolRequestParams::new(name.to_string());
        p.arguments = args.as_object().cloned();
        match self.client.peer().call_tool_once(p).await {
            Ok(CallToolResponse::Complete(r)) => {
                let text = r
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text(t) => Some(t.text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let ok = !r.is_error.unwrap_or(false);
                // Older protocol versions drop structuredContent; the envelope is also in the text.
                let data = r.structured_content.clone().filter(|v| v.get("ok").is_some()).unwrap_or_else(|| {
                    text.split_once('\n')
                        .and_then(|(_, j)| serde_json::from_str(j).ok())
                        .unwrap_or(Value::Null)
                });
                if std::env::var("AF_DEBUG").is_ok() {
                    eprintln!("structured={:?}", r.structured_content);
                }
                (ok, data, text)
            }
            Ok(_) => (false, Value::Null, "unexpected non-complete tool response".into()),
            Err(e) => (false, Value::Null, format!("transport error: {e}")),
        }
    }
}

impl ToolCaller for Remote {
    async fn call(&self, name: &str, args: Value) -> (bool, Value) {
        // Stay under the server's per-player rate limit (8 calls/s).
        tokio::time::sleep(std::time::Duration::from_millis(160)).await;
        let (ok, v, _) = self.call_full(name, args).await;
        (ok, v)
    }
}

/// Register a player through the REST endpoint.
pub async fn register(server: &str, name: &str, model: &str) -> anyhow::Result<String> {
    let r: Value = reqwest::Client::new()
        .post(format!("{}/api/register", server.trim_end_matches('/')))
        .json(&json!({"name": name, "model": model}))
        .send()
        .await?
        .json()
        .await?;
    r["token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("register failed: {r}"))
}
