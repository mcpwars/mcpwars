//! The interaction log: every tool call, resource read, REST registration or recovery and failed
//! MCP request, with who made it, the outcome, the time it took, the MCP session and the client.
//! Telemetry only: it lives beside the universe (`af_store::ActivityRow`), outside the journal, so
//! it never affects replay. Secrets never enter it: credential-bearing tools are logged without
//! their arguments, anything shaped like a token or recovery key is redacted wherever it appears,
//! and replies are never logged. Human words (a relayed answer, an owner's text) and note bodies
//! are kept only as their length, and calls from anyone not logged in carry no arguments.

use std::collections::BTreeMap;

use af_core::model::PlayerId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::ToolReply;

/// Days of raw interaction log kept by default (three seasons). Every completed UTC day is
/// summarised first into the permanent `activity_daily` table (see `history`), so pruning raw rows
/// never loses the record.
pub const DEFAULT_RETENTION_DAYS: u64 = 90;

/// Days of raw interaction log kept: `AF_ACTIVITY_RETENTION_DAYS` (0 keeps it forever), else 90.
pub fn retention_days() -> u64 {
    static DAYS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *DAYS.get_or_init(|| {
        std::env::var("AF_ACTIVITY_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_RETENTION_DAYS)
    })
}

/// The longest window, in hours, a raw-log reader may ask for (the retention, or ten years when
/// the log is kept forever).
pub fn max_window_hours() -> u64 {
    match retention_days() {
        0 => 3_650 * 24,
        d => d * 24,
    }
}
/// Calls whose arguments or replies carry credentials: logged without arguments.
const SECRET_TOOLS: &[&str] = &["join_game", "recover_access", "register", "recover"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<PlayerId>,
    /// captain, owner, sysop or anon (not logged in).
    pub role: String,
    /// mcp (tool call), resource, rest, or http (an MCP request the transport refused).
    pub via: String,
    pub tool: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub ms: u64,
    /// A short, stable fingerprint of the MCP session (not the session id itself).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The client's self-reported name/version from MCP initialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// Server strategy context sampled before the call; telemetry only, never an adherence judgment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_context: Option<StrategyContext>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrategySnapshot {
    pub revision: u64,
    #[serde(default)]
    pub set_tick: Option<u64>,
    pub world_tick: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrategyContext {
    pub available_at_start: StrategySnapshot,
    /// Last strategy revision supplied in a successful briefing on this authenticated session.
    /// A returned reply is evidence of what the server supplied, not proof the model read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_returned_to_session: Option<StrategySnapshot>,
}

impl Call {
    pub fn new(via: &str, role: &str, tool: &str, player: Option<PlayerId>) -> Self {
        Call {
            at_ms: crate::game::now_ms(),
            player,
            role: role.into(),
            via: via.into(),
            tool: tool.into(),
            ok: true,
            ..Default::default()
        }
    }

    /// Outcome from a tool reply: ok, or the refusal code.
    pub fn outcome(mut self, r: &ToolReply) -> Self {
        self.ok = r.ok;
        if !r.ok {
            self.code = r.data["error"]["code"].as_str().map(String::from).or_else(|| Some("error".into()));
        }
        self
    }

    /// A request on an MCP session the server no longer has (it restarted, as it does on every
    /// update). The client opens a new session and carries on: a reconnect, not a refusal.
    pub fn is_reconnect(&self) -> bool {
        self.via == "http"
            && (self.code.as_deref() == Some("session_expired") || (self.code.as_deref() == Some("http_404") && self.session.is_some()))
    }

    /// Refused by the game or the transport, not counting reconnects after a restart.
    pub fn is_refusal(&self) -> bool {
        !self.ok && !self.is_reconnect()
    }

    pub fn failed(mut self, code: &str) -> Self {
        self.ok = false;
        self.code = Some(code.into());
        self
    }

    pub fn session(mut self, sid: Option<&str>) -> Self {
        self.session = sid.map(|s| crate::game::hash_token(s)[..10].to_string());
        self
    }

    /// Keep complete redacted arguments unless the call carries credentials or comes
    /// from someone not logged in.
    pub fn args(mut self, args: &Value) -> Self {
        if SECRET_TOOLS.contains(&self.tool.as_str())
            || self.role == "anon"
            || args.as_object().is_some_and(|o| o.is_empty())
            || args.is_null()
        {
            return self;
        }
        let mut args = args.clone();
        redact(&mut args);
        // Fields kept only as their length (None: keep all; empty: every string field).
        let private: Option<&[&str]> = match (self.role.as_str(), self.tool.as_str()) {
            // Everything an owner types is the human's own words.
            ("owner", _) => Some(&[]),
            ("captain", "write_note") => Some(&["value"]),
            // Feedback is read in full in the feedback list, never duplicated into the log.
            ("captain", "send_feedback") => Some(&["about", "text"]),
            // Explanations live in the language store, never duplicated into the log.
            ("captain", "language") => Some(&["explanation", "examples"]),
            // Identities and Chronicle pages live in their stores, never duplicated into the log.
            ("captain", "identity") => Some(&[
                "backstory",
                "bio",
                "why",
                "appearance",
                "voice",
                "text",
                "personality",
                "speech",
                "values",
                "dream",
            ]),
            ("captain", "chronicle") => Some(&["body", "text", "summary"]),
            // Improve items and patches live on the board, never duplicated into the log.
            ("captain", "improve") => Some(&["title", "text", "diff", "note"]),
            // The strategy and setup a captain records are its human's words.
            ("captain", "ask_owner") if args["action"] == "strategy" || args["action"] == "setup" => Some(&["text"]),
            _ => None,
        };
        if let Some(keys) = private
            && let Some(o) = args.as_object_mut()
        {
            for (k, v) in o.iter_mut() {
                if let Value::String(t) = v
                    && (keys.is_empty() || keys.contains(&k.as_str()))
                {
                    *v = json!({"chars": t.chars().count()});
                }
            }
        }
        if self.role == "captain"
            && self.tool == "memory"
            && let Some(o) = args.as_object_mut()
            && let Some(data) = o.get_mut("data")
        {
            *data = json!({"bytes": data.to_string().len()});
        }
        self.args = Some(args);
        self
    }
}

/// Replace anything shaped like a token or recovery key (`af_` or `afr_` and 32 or more hex
/// digits), in every string of `v`, with `[redacted]`.
pub(crate) fn redact(v: &mut Value) {
    match v {
        Value::String(s) => {
            if s.contains("af_") || s.contains("afr_") {
                *s = redact_str(s);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(redact),
        Value::Object(o) => o.values_mut().for_each(redact),
        _ => {}
    }
}

/// Text an agent wrote, with anything shaped like a token or recovery key replaced by
/// `[redacted]`, so a pasted secret never reaches the journal (feedback reports).
pub fn redact_secrets(s: &str) -> String {
    if s.contains("af_") || s.contains("afr_") {
        redact_str(s)
    } else {
        s.to_string()
    }
}

fn redact_str(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let prefix = if s[i..].starts_with("afr_") {
            4
        } else if s[i..].starts_with("af_") {
            3
        } else {
            0
        };
        if prefix > 0 {
            let hex = b[i + prefix..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            if hex >= 32 {
                out.push_str("[redacted]");
                i += prefix + hex;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[derive(Default)]
struct ToolAgg {
    calls: u64,
    refused: u64,
    ms: u64,
    last_ms: u64,
    codes: BTreeMap<String, u64>,
}

/// Collapse a session's tool sequence: "get_briefing, trade_port x3, write_note".
fn sequence(tools: &[&str]) -> String {
    let mut out: Vec<String> = vec![];
    let mut i = 0;
    while i < tools.len() {
        let mut n = 1;
        while i + n < tools.len() && tools[i + n] == tools[i] {
            n += 1;
        }
        out.push(if n > 1 {
            format!("{} x{n}", tools[i])
        } else {
            tools[i].to_string()
        });
        i += n;
    }
    out.join(", ")
}

/// A short hint only for repeated, still-unresolved failures in the supplied recent sample.
/// Success on the same tool clears older failures, so repaired problems stop consuming context.
pub fn refusal_hint(calls: &[Call]) -> Option<Value> {
    let mut resolved = std::collections::BTreeSet::new();
    let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for call in calls {
        if call.role != "captain" {
            continue;
        }
        if call.ok {
            resolved.insert(call.tool.as_str());
        }
        if call.is_refusal() && !resolved.contains(call.tool.as_str()) {
            *counts.entry((&call.tool, call.code.as_deref().unwrap_or("error"))).or_default() += 1;
        }
    }
    let ((tool, code), count) = counts.into_iter().filter(|(_, n)| *n >= 3).max_by_key(|(_, n)| *n)?;
    let fix = match code {
        "invalid_args" => "Use the tool's current input schema and example; correct the rejected field before retrying.",
        "unknown_tool" => "Refresh tools/list on this captain connection; reconnect if the tool catalog stays stale.",
        "not_at_stardock" => "This action requires Stardock; check your sector and chart a route there first.",
        "no_ap" | "insufficient_ap" => "Check current AP and live game_rules costs; choose an affordable action before retrying.",
        _ => "Read the refusal reason and change the arguments or plan before retrying this action.",
    };
    Some(json!({"tool":tool,"code":code,"count":count,"fix":fix,
        "scope":"last 1000 calls within 6 hours, since the latest success on this tool"}))
}

/// One captain's activity over the window: totals, a per-tool breakdown, refusal codes, calls per
/// hour, sessions (each with its tool sequence) and the most recent calls. `calls` is newest first.
pub fn summarize(calls: &[Call], now_ms: u64, hours: u64) -> Value {
    let mut tools: BTreeMap<&str, ToolAgg> = BTreeMap::new();
    let mut codes: BTreeMap<&str, u64> = BTreeMap::new();
    let mut via: BTreeMap<&str, u64> = BTreeMap::new();
    for c in calls {
        let t = tools.entry(&c.tool).or_default();
        t.calls += 1;
        t.ms += c.ms;
        t.last_ms = t.last_ms.max(c.at_ms);
        if c.is_refusal() {
            t.refused += 1;
            let code = c.code.as_deref().unwrap_or("error");
            *t.codes.entry(code.to_string()).or_default() += 1;
            *codes.entry(code).or_default() += 1;
        }
        *via.entry(&c.via).or_default() += 1;
    }
    let mut by_tool: Vec<(&str, ToolAgg)> = tools.into_iter().collect();
    by_tool.sort_by(|a, b| b.1.calls.cmp(&a.1.calls).then(a.0.cmp(b.0)));
    let by_tool: Vec<Value> = by_tool
        .into_iter()
        .map(|(tool, a)| {
            json!({"tool": tool, "calls": a.calls, "ok": a.calls - a.refused, "refused": a.refused,
                   "refusal_codes": a.codes, "avg_ms": a.ms / a.calls.max(1), "last_ms": a.last_ms})
        })
        .collect();

    // Calls per hour, oldest first, for at most the last 48 hours of the window.
    let hour = 3_600_000;
    let n_hours = hours.clamp(1, 48);
    let start = (now_ms / hour + 1).saturating_sub(n_hours) * hour;
    let mut buckets = vec![0u64; n_hours as usize];
    for c in calls.iter().filter(|c| c.at_ms >= start) {
        let i = ((c.at_ms - start) / hour) as usize;
        if let Some(b) = buckets.get_mut(i) {
            *b += 1;
        }
    }
    let by_hour: Vec<Value> = buckets
        .iter()
        .enumerate()
        .map(|(i, n)| json!({"hour_start_ms": start + i as u64 * hour, "calls": n}))
        .collect();

    // Sessions, newest first. Calls without a session (REST, stdio) group as "none".
    let mut order: Vec<&str> = vec![];
    let mut sessions: BTreeMap<&str, Vec<&Call>> = BTreeMap::new();
    for c in calls {
        let k = c.session.as_deref().unwrap_or("none");
        if !sessions.contains_key(k) {
            order.push(k);
        }
        sessions.entry(k).or_default().push(c);
    }
    let sessions: Vec<Value> = order
        .iter()
        .take(30)
        .map(|k| {
            let cs = &sessions[k];
            let seq: Vec<&str> = cs.iter().rev().map(|c| c.tool.as_str()).collect();
            json!({
                "session": k, "client": cs.iter().find_map(|c| c.client.clone()),
                "first_ms": cs.last().map(|c| c.at_ms), "last_ms": cs.first().map(|c| c.at_ms),
                "calls": cs.len(), "refused": cs.iter().filter(|c| c.is_refusal()).count(),
                "sequence": sequence(&seq),
            })
        })
        .collect();

    let captain_scope = calls.first().and_then(|first| {
        if calls.iter().all(|call| call.player == first.player) {
            first
                .player
                .map(|player| player.to_string())
                .or_else(|| calls.iter().all(|call| call.role == "anon").then(|| "@anon".to_string()))
        } else {
            None
        }
    });
    let refused = calls.iter().filter(|c| c.is_refusal()).count();
    let reconnects = calls.iter().filter(|c| c.is_reconnect()).count();
    json!({
        "window_hours": hours,
        "calls": calls.len(),
        "ok": calls.len() - refused - reconnects,
        "refused": refused,
        "reconnects": reconnects,
        "first_ms": calls.last().map(|c| c.at_ms),
        "last_ms": calls.first().map(|c| c.at_ms),
        "by_via": via,
        "refusal_codes": codes,
        "by_tool": by_tool,
        "by_hour": by_hour,
        "sessions": sessions,
        "recent": calls.iter().take(50).collect::<Vec<_>>(),
        "recent_continuation": {
            "has_more": calls.len() > 50,
            "remaining_in_window": calls.len().saturating_sub(50),
            "retrieval": {
                "tool": "sysop_calls",
                "arguments": {
                    "captain": captain_scope,
                    "hours": hours, "snapshot_ms": now_ms,
                    "since_ms": now_ms.saturating_sub(hours.saturating_mul(3_600_000)),
                    "limit": 50,
                }
            },
            "note": "Recent calls and sessions are previews. Retrieve raw retained calls with the supplied tool, then follow continuation.next_arguments until has_more is false. This window excludes earlier retained calls; request an earlier since_ms when needed."
        },
        "sessions_continuation": {"has_more": order.len() > 30, "remaining_in_window": order.len().saturating_sub(30)},
    })
}

/// Per-captain totals across everyone, for the overview. `calls` is newest first.
pub fn per_player(calls: &[Call]) -> BTreeMap<PlayerId, Value> {
    // (calls, refused, last call, calls per tool)
    type Totals<'a> = (u64, u64, u64, BTreeMap<&'a str, u64>);
    let mut out: BTreeMap<PlayerId, Totals> = BTreeMap::new();
    for c in calls {
        let Some(p) = c.player else { continue };
        let e = out.entry(p).or_default();
        e.0 += 1;
        if c.is_refusal() {
            e.1 += 1;
        }
        e.2 = e.2.max(c.at_ms);
        *e.3.entry(&c.tool).or_default() += 1;
    }
    out.into_iter()
        .map(|(p, (n, refused, last, tools))| {
            let mut top: Vec<(&str, u64)> = tools.into_iter().collect();
            top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            let top: Vec<String> = top.iter().take(3).map(|(t, n)| format!("{t} {n}")).collect();
            (p, json!({"calls": n, "refused": refused, "last_call_ms": last, "top_tools": top}))
        })
        .collect()
}

/// The agent platform a call's MCP client says it is: a known platform's canonical name, "other"
/// for a client the server does not know, None without a client. Never the raw client string.
pub fn client_platform(c: &Call) -> Option<&'static str> {
    let client = c.client.as_deref().and_then(|s| s.split('/').next())?;
    (!client.trim().is_empty()).then(|| af_core::public::client_platform(client).unwrap_or("other"))
}

/// The platform seen most often (ties: the alphabetically first).
fn most_seen(clients: BTreeMap<&'static str, u64>) -> Option<&'static str> {
    clients.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0))).map(|(k, _)| k)
}

/// Who a player id was when a day is rolled up: (call sign, model, harness), the scoreboard's labels.
pub type Identities = BTreeMap<u64, (String, Option<String>, Option<String>)>;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDay {
    pub calls: u64,
    pub refused: u64,
}

/// One row of the permanent daily activity summary: one captain's calls on one UTC day (player
/// 0: everyone not logged in). Reconnects after a restart and operator calls are not counted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyActivity {
    pub day: String,
    pub player_id: u64,
    /// Call sign when rolled up ("anonymous" for player 0, "#id" if unknown).
    pub name: String,
    pub model: Option<String>,
    pub harness: Option<String>,
    /// The known agent platform the MCP client reported most often (never the raw client string).
    pub platform: Option<String>,
    pub calls: u64,
    pub ok: u64,
    pub refused: u64,
    pub refusal_codes: BTreeMap<String, u64>,
    pub tools: BTreeMap<String, ToolDay>,
    /// Distinct MCP sessions.
    pub sessions: u64,
    pub first_ms: u64,
    pub last_ms: u64,
}

/// Summarise one UTC day's raw calls into one row per player (and one for player 0, the calls
/// nobody was logged in for). `who` names the players as they are now.
pub fn rollup_day(day: &str, calls: &[Call], who: &Identities) -> Vec<DailyActivity> {
    struct Acc {
        row: DailyActivity,
        sessions: std::collections::BTreeSet<String>,
        clients: BTreeMap<&'static str, u64>,
    }
    let mut by: BTreeMap<u64, Acc> = BTreeMap::new();
    for c in calls.iter().filter(|c| !c.is_reconnect() && c.role != "sysop") {
        let pid = c.player.map(u64::from).unwrap_or(0);
        let a = by.entry(pid).or_insert_with(|| {
            let (name, model, harness) = match (pid, who.get(&pid)) {
                (0, _) => ("anonymous".to_string(), None, None),
                (_, Some((n, m, h))) => (n.clone(), m.clone(), h.clone()),
                (_, None) => (format!("#{pid}"), None, None),
            };
            Acc {
                row: DailyActivity {
                    day: day.to_string(),
                    player_id: pid,
                    name,
                    model,
                    harness,
                    platform: None,
                    calls: 0,
                    ok: 0,
                    refused: 0,
                    refusal_codes: BTreeMap::new(),
                    tools: BTreeMap::new(),
                    sessions: 0,
                    first_ms: c.at_ms,
                    last_ms: c.at_ms,
                },
                sessions: Default::default(),
                clients: BTreeMap::new(),
            }
        });
        let r = &mut a.row;
        r.calls += 1;
        let t = r.tools.entry(c.tool.clone()).or_default();
        t.calls += 1;
        if c.is_refusal() {
            r.refused += 1;
            t.refused += 1;
            *r.refusal_codes.entry(c.code.clone().unwrap_or_else(|| "error".into())).or_default() += 1;
        } else {
            r.ok += 1;
        }
        r.first_ms = r.first_ms.min(c.at_ms);
        r.last_ms = r.last_ms.max(c.at_ms);
        if let Some(s) = &c.session {
            a.sessions.insert(s.clone());
        }
        if let Some(k) = client_platform(c) {
            *a.clients.entry(k).or_default() += 1;
        }
    }
    by.into_values()
        .map(|mut a| {
            a.row.sessions = a.sessions.len() as u64;
            a.row.platform = most_seen(a.clients).map(String::from);
            a.row
        })
        .collect()
}

/// What the public scoreboard may say about each captain's last day of calls: the agent platform
/// its MCP client says it is (only when it is a known platform, never the raw client string),
/// its calls and its refused calls. `calls` is newest first.
pub fn public_signals(calls: &[Call]) -> BTreeMap<PlayerId, Value> {
    // (calls, refused, calls per known client)
    let mut out: BTreeMap<PlayerId, (u64, u64, BTreeMap<&'static str, u64>)> = BTreeMap::new();
    for c in calls {
        let Some(p) = c.player else { continue };
        let e = out.entry(p).or_default();
        if c.is_reconnect() {
            continue;
        }
        e.0 += 1;
        if c.is_refusal() {
            e.1 += 1;
        }
        if let Some(k) = client_platform(c) {
            *e.2.entry(k).or_default() += 1;
        }
    }
    out.into_iter()
        .map(|(p, (n, refused, clients))| {
            let seen = most_seen(clients);
            (p, json!({"seen_as": seen, "calls_24h": n, "refused_24h": refused}))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(at: u64, tool: &str, ok: bool, session: &str) -> Call {
        Call {
            at_ms: at,
            player: Some(7),
            role: "captain".into(),
            via: "mcp".into(),
            tool: tool.into(),
            ok,
            code: (!ok).then(|| "no_ap".into()),
            ms: 10,
            session: Some(session.into()),
            ..Default::default()
        }
    }

    #[test]
    fn public_signals_name_only_known_platforms() {
        let mut a = call(1, "get_briefing", true, "a");
        a.client = Some("openclaw/2026.10".into());
        let mut b = call(2, "trade_port", false, "a");
        b.client = Some("home-laptop-rig/1".into());
        let mut c = call(3, "warp", true, "a");
        c.client = Some("openclaw/2026.10".into());
        let s = public_signals(&[c, b, a]);
        assert_eq!(s[&7]["seen_as"], "OpenClaw");
        assert_eq!(s[&7]["calls_24h"], 3);
        assert_eq!(s[&7]["refused_24h"], 1);
        assert!(!s[&7].to_string().contains("home-laptop"), "raw client strings never leave the log");
    }

    #[test]
    fn a_day_rolls_up_per_captain_without_reconnects_or_operator_calls() {
        let mut a = call(10, "get_briefing", true, "s1");
        a.client = Some("claude-code/2.1".into());
        let mut b = call(20, "trade_port", false, "s1");
        b.client = Some("claude-code/2.1".into());
        let c = call(5, "warp", true, "s2");
        let reconnect = Call::new("http", "captain", "ping", Some(7))
            .failed("session_expired")
            .session(Some("x"));
        let anon = Call::new("mcp", "anon", "join_game", None).failed("rate_limited");
        let op = Call::new("mcp", "sysop", "sysop_overview", None);
        let mut who = Identities::new();
        who.insert(7, ("Seven".into(), Some("claude-opus-5-5".into()), Some("Claude Code".into())));
        let rows = rollup_day("2026-10-01", &[a, b, c, reconnect, anon, op], &who);
        assert_eq!(rows.len(), 2);
        let (anon, seven) = (&rows[0], &rows[1]);
        assert_eq!(
            (anon.player_id, anon.name.as_str(), anon.calls, anon.refused),
            (0, "anonymous", 1, 1)
        );
        assert_eq!(seven.name, "Seven");
        assert_eq!((seven.calls, seven.ok, seven.refused, seven.sessions), (3, 2, 1, 2));
        assert_eq!(seven.refusal_codes["no_ap"], 1);
        assert_eq!(seven.tools["trade_port"], ToolDay { calls: 1, refused: 1 });
        assert_eq!((seven.first_ms, seven.last_ms), (5, 20));
        assert_eq!(seven.platform.as_deref(), Some("Claude Code"));
        let v = serde_json::to_value(seven).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "calls",
                "day",
                "first_ms",
                "harness",
                "last_ms",
                "model",
                "name",
                "ok",
                "platform",
                "player_id",
                "refusal_codes",
                "refused",
                "sessions",
                "tools"
            ]
        );
    }

    #[test]
    fn summary_breaks_down_tools_codes_and_sessions() {
        let h = 3_600_000;
        let b = 100 * h;
        // Newest first.
        let calls = vec![
            call(b + 5 * h + 40, "write_note", true, "b"),
            call(b + 5 * h + 30, "trade_port", false, "b"),
            call(b + 5 * h + 20, "trade_port", true, "b"),
            call(b + 5 * h + 10, "trade_port", true, "b"),
            call(b + 5 * h, "get_briefing", true, "b"),
            call(b + 2 * h, "get_briefing", true, "a"),
        ];
        let s = summarize(&calls, b + 5 * h + 50, 24);
        assert_eq!(s["calls"], 6);
        assert_eq!(s["refused"], 1);
        assert_eq!(s["refusal_codes"]["no_ap"], 1);
        assert_eq!(s["by_tool"][0]["tool"], "trade_port");
        assert_eq!(s["by_tool"][0]["refused"], 1);
        assert_eq!(s["sessions"][0]["session"], "b");
        assert_eq!(s["sessions"][0]["sequence"], "get_briefing, trade_port x3, write_note");
        assert_eq!(s["sessions"][1]["sequence"], "get_briefing");
        let by_hour = s["by_hour"].as_array().unwrap();
        assert_eq!(by_hour.len(), 24);
        assert_eq!(by_hour.last().unwrap()["calls"], 5);
        let pp = per_player(&calls);
        assert_eq!(pp[&7]["calls"], 6);
        assert_eq!(pp[&7]["top_tools"][0], "trade_port 3");
    }

    #[test]
    fn summary_previews_expose_retrieval_and_old_rows_have_unknown_strategy_context() {
        let calls: Vec<Call> = (1..=70)
            .rev()
            .map(|at| call(at, "get_status", true, &format!("session-{at}")))
            .collect();
        let summary = summarize(&calls, 71, 1);
        assert_eq!(summary["recent"].as_array().unwrap().len(), 50);
        assert_eq!(summary["recent_continuation"]["has_more"], true);
        assert_eq!(summary["recent_continuation"]["remaining_in_window"], 20);
        assert_eq!(summary["recent_continuation"]["retrieval"]["tool"], "sysop_calls");
        assert_eq!(summary["recent_continuation"]["retrieval"]["arguments"]["captain"], "7");
        assert_eq!(summary["sessions_continuation"]["has_more"], true);
        let old = serde_json::to_value(&calls[0]).unwrap();
        assert!(old.get("strategy_context").is_none());
        assert!(serde_json::from_value::<Call>(old).unwrap().strategy_context.is_none());
    }

    #[test]
    fn a_reconnect_after_a_restart_is_not_a_refusal() {
        let mut c = Call::new("http", "captain", "ping", Some(7))
            .failed("session_expired")
            .session(Some("abc"));
        assert!(c.is_reconnect() && !c.is_refusal());
        c.code = Some("http_404".into());
        assert!(c.is_reconnect(), "older rows with a session count too");
        let s = summarize(&[c.clone(), Call::new("mcp", "captain", "warp", Some(7)).failed("no_ap")], 1, 1);
        assert_eq!((s["refused"].as_u64(), s["reconnects"].as_u64()), (Some(1), Some(1)));
        assert_eq!(per_player(&[c])[&7]["refused"], 0);
    }

    #[test]
    fn secrets_and_big_arguments_stay_out() {
        let c = Call::new("mcp", "anon", "recover_access", None).args(&json!({"call_sign": "X", "recovery_key": "afr_secret"}));
        assert!(c.args.is_none());
        let big = json!({"value": "x".repeat(5000)});
        let c = Call::new("mcp", "captain", "write_note", Some(1)).args(&big);
        assert!(serde_json::to_string(&c.args).unwrap().len() < 700);
        let c = Call::new("mcp", "captain", "warp", Some(1)).args(&json!({"sector": 5}));
        assert_eq!(c.args, Some(json!({"sector": 5})));
    }

    #[test]
    fn radio_arguments_keep_complete_multibyte_text() {
        let text = "🌌é\n".repeat(10_000);
        let args = json!({"message": text, "channel": "public"});
        let call = Call::new("mcp", "captain", "radio_broadcast", Some(1)).args(&args);
        assert_eq!(call.args, Some(args));
    }

    #[test]
    fn tokens_human_words_and_anonymous_arguments_are_not_kept() {
        let tok = format!("af_{}", "ab12".repeat(12));
        let key = format!("afr_{}", "cd34".repeat(12));
        // A token pasted anywhere, even inside a long message, is redacted.
        let c = Call::new("mcp", "captain", "radio_broadcast", Some(1))
            .args(&json!({"message": format!("mine is {tok} and {key}; see https://x/mcp/{tok}"), "channel": "public"}));
        let s = serde_json::to_string(&c.args).unwrap();
        assert!(!s.contains(&tok) && !s.contains(&key) && s.contains("[redacted]"), "{s}");
        let long = json!({"message": format!("{}{tok}", "x".repeat(590))});
        let s = serde_json::to_string(&Call::new("mcp", "captain", "radio_broadcast", Some(1)).args(&long).args).unwrap();
        assert!(!s.contains(&tok[..20]), "{s}");
        // Note bodies, relayed human words and owner text keep only their length.
        let c = Call::new("mcp", "captain", "write_note", Some(1)).args(&json!({"key": "setup", "value": "player 7"}));
        assert_eq!(c.args, Some(json!({"key": "setup", "value": {"chars": 8}})));
        let c = Call::new("mcp", "captain", "ask_owner", Some(1)).args(&json!({"action": "strategy", "text": "trade fuel"}));
        assert_eq!(c.args.unwrap()["text"], json!({"chars": 10}));
        let c = Call::new("mcp", "captain", "ask_owner", Some(1)).args(&json!({"action": "summary", "text": "Up 4k today."}));
        assert_eq!(c.args.unwrap()["text"], "Up 4k today.");
        let c = Call::new("mcp", "owner", "owner_coach", Some(1)).args(&json!({"text": "trade more"}));
        assert_eq!(c.args, Some(json!({"text": {"chars": 10}})));
        // Nobody logged in: no arguments at all.
        let c = Call::new("mcp", "anon", "get_briefing", None).args(&json!({"x": "y"}));
        assert!(c.args.is_none());
    }
}

#[cfg(test)]
mod mention_tests {
    use super::*;

    #[test]
    fn mentions_language_tags_and_mute_targets_are_kept_after_redaction() {
        let tok = format!("af_{}", "ab12".repeat(12));
        // Radio mentions and a language tag are kept in full, with token-shaped strings redacted.
        let args = json!({"message": "@Vex hi", "channel": "public", "mentions": ["Vex", format!("corp:{tok}")],
                          "language": "Dockcant", "language_version": 2});
        let c = Call::new("mcp", "captain", "radio_broadcast", Some(1)).args(&args);
        let kept = c.args.unwrap();
        assert_eq!(kept["mentions"], json!(["Vex", "corp:[redacted]"]));
        assert_eq!(
            (kept["language"].clone(), kept["language_version"].clone()),
            (json!("Dockcant"), json!(2))
        );
        assert_eq!(kept["message"], "@Vex hi");
        let c = Call::new("mcp", "captain", "radio_broadcast", Some(1)).args(&json!({"message": "x", "language": 7}));
        assert_eq!(c.args.unwrap()["language"], 7);
        // The mentions tool's arguments are kept.
        let c = Call::new("mcp", "captain", "mentions", Some(1)).args(&json!({"action": "mute", "target": "corp:Iron Tide"}));
        assert_eq!(c.args, Some(json!({"action": "mute", "target": "corp:Iron Tide"})));
        // A language name is kept (redacted when token-shaped); explanations stay lengths only.
        let c = Call::new("mcp", "captain", "language", Some(1))
            .args(&json!({"action": "define", "name": tok.clone(), "explanation": "secret words"}));
        let kept = c.args.unwrap();
        assert_eq!(kept["name"], "[redacted]");
        assert_eq!(kept["explanation"], json!({"chars": 12}));
        let c = Call::new("mcp", "captain", "language", Some(1)).args(&json!({"action": "read", "id": 3}));
        assert_eq!(c.args, Some(json!({"action": "read", "id": 3})));
        // Mention entries on the Chronicle, identity log and constitution are kept; bodies are not.
        let c = Call::new("mcp", "captain", "chronicle", Some(1))
            .args(&json!({"action": "talk", "page": 1, "text": "page words", "mentions": ["Vex"]}));
        let kept = c.args.unwrap();
        assert_eq!(
            (kept["mentions"].clone(), kept["text"].clone()),
            (json!(["Vex"]), json!({"chars": 10}))
        );
        let c = Call::new("mcp", "captain", "identity", Some(1)).args(&json!({"action": "log", "text": "log words", "mentions": ["Vex"]}));
        assert_eq!(c.args.unwrap()["mentions"], json!(["Vex"]));
    }
}

#[cfg(test)]
mod private_memory_tests {
    use super::*;
    #[test]
    fn nested_memory_and_import_bodies_are_lengths_only() {
        let c = Call::new("mcp", "captain", "memory", Some(1))
            .args(&json!({"action":"import", "data":{"records":{"secret":{"data":"private words"}}}}));
        let text = serde_json::to_string(&c).unwrap();
        assert!(!text.contains("private words"));
        assert!(!text.contains("secret"));
        assert!(text.contains("bytes"));
    }

    #[test]
    fn improve_titles_texts_diffs_and_notes_are_lengths_only() {
        let c = Call::new("mcp", "captain", "improve", Some(1)).args(&json!({
            "action": "patch", "id": 4, "base": "e52ec7a", "title": "board words", "text": "item words",
            "diff": "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-diff words\n+more\n", "note": "note words"
        }));
        let text = serde_json::to_string(&c).unwrap();
        for words in ["board words", "item words", "diff words", "note words"] {
            assert!(!text.contains(words), "{words}: {text}");
        }
        let args = c.args.unwrap();
        assert_eq!(args["title"], json!({"chars": 11}));
        assert_eq!(
            (args["action"].clone(), args["id"].clone(), args["base"].clone()),
            (json!("patch"), json!(4), json!("e52ec7a"))
        );
    }
}
