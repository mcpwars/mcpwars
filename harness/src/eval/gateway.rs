//! The evaluation boundary. Every evaluated client (scripted policy or API
//! model) reaches the game only through a `Gateway`, which enforces the tool
//! allowlist and the per-wake call budget before anything touches the engine,
//! and records every attempt, executed or not.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use af_core::model::PlayerId;
use af_mcp::game::Game;
use af_mcp::tools::{self, ToolDef};
use serde::Serialize;
use serde_json::{Value, json};

use crate::bots::ToolCaller;

#[derive(Clone, Debug, Serialize)]
pub struct ToolEvent {
    pub wake: u32,
    /// Position of this attempt within the wake (1-based), counting refusals.
    pub idx: u32,
    pub tool: String,
    /// Parsed arguments (null when they did not parse; see `raw_args`).
    pub args: Value,
    /// The arguments exactly as the client sent them, when they were not valid JSON.
    pub raw_args: Option<String>,
    /// Reached the engine (false for permission or budget refusals).
    pub executed: bool,
    pub ok: bool,
    /// Refusal code: an engine refusal code, or `budget_exhausted` / `not_permitted` /
    /// `invalid_arguments` from the gateway.
    pub code: Option<String>,
    /// Short human-readable summary (truncated).
    pub summary: String,
    /// The complete tool response the client saw (never truncated).
    pub result: Value,
    /// The exact text returned to a model client.
    pub text: String,
    pub latency_us: u64,
    /// Simulation tick when the call was made.
    pub tick: u64,
    /// The same tool with the same arguments was already refused earlier in this wake.
    pub repeated_invalid: bool,
}

#[derive(Default)]
struct Inner {
    wake: u32,
    budget_left: u32,
    idx: u32,
    events: Vec<ToolEvent>,
    refused: BTreeSet<String>,
}

#[derive(Clone)]
pub struct Gateway {
    game: Game,
    pid: PlayerId,
    allowed: Arc<BTreeSet<String>>,
    inner: Arc<Mutex<Inner>>,
}

impl Gateway {
    /// `deny`: tools this client may not call. join_game is always denied.
    pub fn new(game: Game, pid: PlayerId, deny: &[String]) -> Gateway {
        let allowed: BTreeSet<String> = tools::catalog()
            .iter()
            .map(|t| t.name.to_string())
            .filter(|n| n != "join_game" && !deny.contains(n))
            .collect();
        Gateway {
            game,
            pid,
            allowed: Arc::new(allowed),
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }

    pub fn pid(&self) -> PlayerId {
        self.pid
    }

    /// Open a wake with a fresh budget.
    pub fn begin_wake(&self, wake: u32, budget: u32) {
        let mut i = self.inner.lock().unwrap();
        i.wake = wake;
        i.budget_left = budget;
        i.idx = 0;
        i.refused.clear();
    }

    pub fn budget_left(&self) -> u32 {
        self.inner.lock().unwrap().budget_left
    }

    pub fn take_events(&self) -> Vec<ToolEvent> {
        std::mem::take(&mut self.inner.lock().unwrap().events)
    }

    pub fn permitted(&self, tool: &str) -> bool {
        self.allowed.contains(tool)
    }

    /// The tool definitions this client is offered (same text as the live MCP catalog).
    pub fn catalog(&self) -> Vec<ToolDef> {
        tools::catalog().iter().filter(|t| self.allowed.contains(t.name)).cloned().collect()
    }

    /// Execute (or refuse) one call with already-parsed arguments.
    /// Returns (ok, JSON envelope, text for a model).
    pub async fn call_full(&self, tool: &str, args: Value) -> (bool, Value, String) {
        self.attempt(tool, Ok(args)).await
    }

    /// Execute (or refuse) one call whose arguments arrive as raw text (model clients).
    /// Text that is not a JSON object is refused as `invalid_arguments`: charged, recorded
    /// verbatim, never executed, and never "repaired" into default arguments.
    pub async fn call_raw(&self, tool: &str, raw: &str) -> (bool, Value, String) {
        let parsed = match serde_json::from_str::<Value>(if raw.trim().is_empty() { "{}" } else { raw }) {
            Ok(v) if v.is_object() => Ok(v),
            _ => Err(raw.to_string()),
        };
        self.attempt(tool, parsed).await
    }

    /// Every attempt is charged against the wake's budget, whatever its outcome:
    /// unpermitted tools and malformed arguments cost a call like any other.
    async fn attempt(&self, tool: &str, args: Result<Value, String>) -> (bool, Value, String) {
        let gate = {
            let mut i = self.inner.lock().unwrap();
            i.idx += 1;
            if i.budget_left == 0 {
                Err((
                    "budget_exhausted",
                    "The tool-call budget for this wake is spent; end your turn.".to_string(),
                ))
            } else {
                i.budget_left -= 1;
                if !self.allowed.contains(tool) {
                    Err(("not_permitted", format!("The tool '{tool}' is not available in this evaluation.")))
                } else if args.is_err() {
                    Err((
                        "invalid_arguments",
                        "The arguments are not a valid JSON object; nothing was executed.".to_string(),
                    ))
                } else {
                    Ok(())
                }
            }
        };
        let tick = self.game.read(|w| w.tick).await;
        let (args, raw_args) = match args {
            Ok(v) => (v, None),
            Err(raw) => (Value::Null, Some(raw)),
        };
        let key = format!("{tool} {args} {}", raw_args.as_deref().unwrap_or(""));
        let (executed, ok, code, data, text, latency_us) = match gate {
            Err((code, message)) => {
                let data = json!({"ok": false, "error": {"code": code, "message": message}, "note": "Nothing was executed."});
                (false, false, Some(code.to_string()), data, format!("REFUSED [{code}] {message}"), 0)
            }
            Ok(()) => {
                let t0 = Instant::now();
                let r = tools::call(&self.game, self.pid, tool, &args).await;
                let latency = t0.elapsed().as_micros() as u64;
                let code = (!r.ok).then(|| r.data["error"]["code"].as_str().unwrap_or("refused").to_string());
                (true, r.ok, code, r.data, r.text, latency)
            }
        };
        let summary = if ok {
            data["summary"].as_str().unwrap_or("").to_string()
        } else {
            data["error"]["message"].as_str().unwrap_or("").to_string()
        };
        {
            let mut i = self.inner.lock().unwrap();
            let repeated_invalid = !ok && i.refused.contains(&key);
            if !ok {
                i.refused.insert(key);
            }
            let ev = ToolEvent {
                wake: i.wake,
                idx: i.idx,
                tool: tool.to_string(),
                args,
                raw_args,
                executed,
                ok,
                code,
                summary: summary.chars().take(300).collect(),
                result: data.clone(),
                text: text.clone(),
                latency_us,
                tick,
                repeated_invalid,
            };
            i.events.push(ev);
        }
        (ok, data, text)
    }
}

impl ToolCaller for Gateway {
    async fn call(&self, name: &str, args: Value) -> (bool, Value) {
        let (ok, v, _) = self.call_full(name, args).await;
        (ok, v)
    }
}
