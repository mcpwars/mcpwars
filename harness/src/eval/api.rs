//! Model candidates over HTTP APIs, playing through the evaluation `Gateway`.
//!
//! Each wake is a fresh conversation (no carried context): a fixed system
//! prompt, then one user message with the wake number, the budget, the
//! operator's objective and whatever memory the scenario allows. The model
//! sees only the tools the gateway permits, and the gateway (not this loop)
//! enforces the budget. The loop ends when the model stops calling tools, when
//! a call is refused for budget, or after a hard cap on model rounds.
//!
//! The exact model-visible conversation is recorded as it happens into a shared
//! `WakeRecord`, so a wake cut off by a timeout still leaves a complete partial
//! transcript. Provider and transport failures are returned as `InfraError`
//! so the runner marks the episode as an infrastructure error rather than an
//! agent failure.

use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use super::gateway::Gateway;

#[derive(Clone, Debug, Serialize)]
pub struct ApiConfig {
    /// "openai" (any OpenAI-compatible chat completions endpoint) or "anthropic".
    pub backend: String,
    pub model: String,
    pub base_url: String,
    /// Name of the environment variable holding the key (the key itself is never recorded).
    pub api_key_env: String,
    pub max_tokens: u32,
    /// Per-HTTP-request timeout.
    pub request_timeout_secs: u64,
}

impl ApiConfig {
    /// Parse "openai:MODEL" or "anthropic:MODEL"; an optional "|BASE_URL" suffix overrides the endpoint.
    pub fn parse(spec: &str) -> anyhow::Result<ApiConfig> {
        let (backend, rest) = spec
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("candidate spec {spec}: expected backend:model"))?;
        let (model, base) = match rest.split_once('|') {
            Some((m, b)) => (m.to_string(), Some(b.to_string())),
            None => (rest.to_string(), None),
        };
        let (default_base, key) = match backend {
            "openai" => ("https://api.openai.com/v1", "OPENAI_API_KEY"),
            "anthropic" => ("https://api.anthropic.com", "ANTHROPIC_API_KEY"),
            other => anyhow::bail!("unknown API backend {other} (openai, anthropic)"),
        };
        Ok(ApiConfig {
            backend: backend.to_string(),
            model,
            base_url: base.unwrap_or_else(|| default_base.to_string()),
            api_key_env: key.to_string(),
            max_tokens: 8_000,
            request_timeout_secs: 300,
        })
    }
}

/// Everything a model saw and produced in one wake, written as it happens.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WakeRecord {
    pub model_rounds: u32,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub stop: String,
    /// The exact system prompt and messages exchanged, in order (tool results in full).
    pub transcript: Vec<Value>,
}

/// A failure of the provider, the network or the harness, not of the agent.
#[derive(Clone, Debug, Serialize)]
pub struct InfraError {
    /// "missing_key", "http_status", "transport", "bad_response", "timeout", "config".
    pub kind: String,
    pub http_status: Option<u16>,
    pub message: String,
}

pub fn infra(kind: &str, status: Option<u16>, message: impl Into<String>) -> InfraError {
    InfraError {
        kind: kind.into(),
        http_status: status,
        message: message.into().chars().take(2_000).collect(),
    }
}

async fn send(req: reqwest::RequestBuilder) -> Result<Value, InfraError> {
    let resp = req
        .send()
        .await
        .map_err(|e| infra(if e.is_timeout() { "timeout" } else { "transport" }, None, e.to_string()))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| infra("transport", Some(status.as_u16()), e.to_string()))?;
    if !status.is_success() {
        return Err(infra("http_status", Some(status.as_u16()), body));
    }
    serde_json::from_str::<Value>(&body).map_err(|e| infra("bad_response", Some(status.as_u16()), format!("{e}: {body}")))
}

/// One wake for a model candidate. Usage and the transcript accumulate in `rec`.
pub async fn play(cfg: &ApiConfig, gw: &Gateway, system: &str, user: &str, rec: Arc<Mutex<WakeRecord>>) -> Result<(), InfraError> {
    let key = std::env::var(&cfg.api_key_env).map_err(|_| infra("missing_key", None, format!("{} is not set", cfg.api_key_env)))?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(cfg.request_timeout_secs))
        .build()
        .map_err(|e| infra("transport", None, e.to_string()))?;
    {
        let mut r = rec.lock().unwrap();
        r.input_tokens = Some(0);
        r.output_tokens = Some(0);
        r.transcript.push(json!({"role": "system", "content": system}));
        r.transcript.push(json!({"role": "user", "content": user}));
    }
    let max_rounds = gw.budget_left() + 5;
    let catalog = gw.catalog();
    let push = |v: Value| rec.lock().unwrap().transcript.push(v);
    let add_usage = |i: Option<i64>, o: Option<i64>| {
        let mut r = rec.lock().unwrap();
        r.model_rounds += 1;
        // A response without usage makes the wake's count unknown, not zero.
        r.input_tokens = r.input_tokens.zip(i).map(|(a, b)| a + b);
        r.output_tokens = r.output_tokens.zip(o).map(|(a, b)| a + b);
    };
    let finish = |stop: &str| rec.lock().unwrap().stop = stop.to_string();
    let rounds = || rec.lock().unwrap().model_rounds;
    match cfg.backend.as_str() {
        "openai" => {
            let tools: Vec<Value> = catalog
                .iter()
                .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": Value::Object(t.schema.clone())}}))
                .collect();
            let mut messages = vec![
                json!({"role": "system", "content": system}),
                json!({"role": "user", "content": user}),
            ];
            loop {
                let body = json!({"model": cfg.model, "messages": messages, "tools": tools, "max_completion_tokens": cfg.max_tokens});
                let v = send(
                    http.post(format!("{}/chat/completions", cfg.base_url.trim_end_matches('/')))
                        .bearer_auth(&key)
                        .json(&body),
                )
                .await?;
                add_usage(v["usage"]["prompt_tokens"].as_i64(), v["usage"]["completion_tokens"].as_i64());
                let msg = v["choices"][0]["message"].clone();
                if !msg.is_object() {
                    return Err(infra("bad_response", None, format!("no message in response: {v}")));
                }
                push(msg.clone());
                messages.push(msg.clone());
                let calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();
                if calls.is_empty() {
                    finish(v["choices"][0]["finish_reason"].as_str().unwrap_or("stop"));
                    return Ok(());
                }
                let mut exhausted = false;
                for tc in calls {
                    let name = tc["function"]["name"].as_str().unwrap_or("");
                    // Arguments go to the gateway verbatim: malformed JSON is refused, never repaired.
                    let raw = match &tc["function"]["arguments"] {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let (_, data, text) = gw.call_raw(name, &raw).await;
                    exhausted |= data["error"]["code"] == "budget_exhausted";
                    let m = json!({"role": "tool", "tool_call_id": tc["id"], "content": text});
                    push(m.clone());
                    messages.push(m);
                }
                if exhausted || rounds() >= max_rounds {
                    finish(if exhausted { "budget_exhausted" } else { "round_cap" });
                    return Ok(());
                }
            }
        }
        "anthropic" => {
            let tools: Vec<Value> = catalog
                .iter()
                .map(|t| json!({"name": t.name, "description": t.description, "input_schema": Value::Object(t.schema.clone())}))
                .collect();
            let mut messages = vec![json!({"role": "user", "content": user})];
            loop {
                let body =
                    json!({"model": cfg.model, "max_tokens": cfg.max_tokens, "system": system, "tools": tools, "messages": messages});
                let v = send(
                    http.post(format!("{}/v1/messages", cfg.base_url.trim_end_matches('/')))
                        .header("x-api-key", &key)
                        .header("anthropic-version", "2023-06-01")
                        .json(&body),
                )
                .await?;
                add_usage(v["usage"]["input_tokens"].as_i64(), v["usage"]["output_tokens"].as_i64());
                let content = v["content"].clone();
                if !content.is_array() {
                    return Err(infra("bad_response", None, format!("no content in response: {v}")));
                }
                let m = json!({"role": "assistant", "content": content});
                push(m.clone());
                messages.push(m);
                if v["stop_reason"] != "tool_use" {
                    finish(v["stop_reason"].as_str().unwrap_or(""));
                    return Ok(());
                }
                let mut results = vec![];
                let mut exhausted = false;
                for b in content.as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
                    let name = b["name"].as_str().unwrap_or("");
                    let (ok, data, text) = if b["input"].is_object() {
                        gw.call_full(name, b["input"].clone()).await
                    } else {
                        gw.call_raw(name, &b["input"].to_string()).await
                    };
                    exhausted |= data["error"]["code"] == "budget_exhausted";
                    results.push(json!({"type": "tool_result", "tool_use_id": b["id"], "content": text, "is_error": !ok}));
                }
                let m = json!({"role": "user", "content": results});
                push(m.clone());
                messages.push(m);
                if exhausted || rounds() >= max_rounds {
                    finish(if exhausted { "budget_exhausted" } else { "round_cap" });
                    return Ok(());
                }
            }
        }
        other => Err(infra("config", None, format!("unknown backend {other}"))),
    }
}
