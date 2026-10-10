//! Feedback from captains and agents to the operator: wrong instructions, failed setup steps,
//! bugs. Journaled like every other state change, private (never on a public page), reviewed
//! and resolved in the sysop console or over the sysop MCP tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::*;
use crate::model::*;

/// Longest `about`, in characters after sanitising.
pub const ABOUT_MAX: usize = 120;
/// Longest report text or resolution note, in characters (Unicode scalar values, not bytes) of
/// the stored text. Longer is refused whole, never cut.
pub const TEXT_MAX: usize = 20_000;
/// Longest model or harness label kept with a report.
const LABEL_MAX: usize = 80;
/// Reports one sender (or every anonymous sender together) may send per `RATE_WINDOW_TICKS`.
pub const RATE_MAX: usize = 5;
pub const RATE_WINDOW_TICKS: Tick = 60;
/// How many resolved reports the world keeps (the newest by id). Open reports are never dropped.
pub const KEEP_RESOLVED: usize = 2_000;
/// The most reports one list page returns.
pub const LIST_MAX: usize = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackStatus {
    Open,
    Resolved,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Feedback {
    pub id: u64,
    pub tick: Tick,
    /// The journal entry's time (`World::now_ms`).
    pub at_ms: u64,
    /// The captain who sent it; None for an anonymous report (REST without a valid token).
    pub from: Option<PlayerId>,
    /// The sender's call sign when it was sent (kept if the captain is later removed).
    pub from_name: Option<String>,
    pub about: String,
    pub text: String,
    pub model: Option<String>,
    pub harness: Option<String>,
    pub status: FeedbackStatus,
    pub resolved_tick: Option<Tick>,
    pub resolved_at_ms: Option<u64>,
    pub resolution: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FeedbackArgs {
    /// Document path (e.g. /join), tool name or topic; at most 120 characters.
    pub about: String,
    /// The full report: step, exact error, what worked instead. Stored exactly as sent (whitespace, line ends and Unicode kept), at most 20000 characters; split a longer one into numbered parts with the same about.
    pub text: String,
    /// Your model (default: as registered).
    #[serde(default)]
    pub model: Option<String>,
    /// Your agent platform (default: as registered).
    #[serde(default)]
    pub harness: Option<String>,
}

/// For `about`, a short label: strip control characters except line breaks (Windows line ends
/// become `\n`) and trim.
pub fn clean(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let kept: String = s.chars().filter(|c| *c == '\n' || !c.is_control()).collect();
    kept.trim().to_string()
}

/// For report text and resolution notes: exactly as sent, except NUL (which JSON and Postgres
/// text cannot carry). Tabs, carriage returns, line ends, surrounding whitespace and every other
/// character are kept.
pub fn exact(s: &str) -> String {
    if s.contains('\0') { s.replace('\0', "") } else { s.to_string() }
}

fn too_long(field: &str, n: usize) -> Refusal {
    Refusal::new(
        "too_long",
        format!("{field} is {n} characters; the maximum is {TEXT_MAX}. Nothing was stored."),
    )
    .hint(format!(
        "Split it into numbered parts of at most {TEXT_MAX} characters, each sent with the same about and starting \"part 1/3: \", \"part 2/3: \" and so on."
    ))
}

fn label(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(|v| crate::player::sanitize(v, LABEL_MAX))
        .filter(|v| !v.is_empty())
}

fn checked_about(value: &str) -> Res<String> {
    let v = clean(value);
    if v.is_empty() {
        return Err(Refusal::new("invalid_args", "about is empty."));
    }
    let n = v.chars().count();
    if n > ABOUT_MAX {
        return Err(Refusal::new(
            "too_long",
            format!("about is {n} characters; the maximum is {ABOUT_MAX}. Nothing was stored."),
        )
        .hint("Name the document path, tool or topic in a few words; the details go in text."));
    }
    Ok(v)
}

fn checked_text(value: &str) -> Res<String> {
    let v = exact(value);
    if v.trim().is_empty() {
        return Err(Refusal::new("invalid_args", "text is empty."));
    }
    let n = v.chars().count();
    if n > TEXT_MAX {
        return Err(too_long("text", n));
    }
    Ok(v)
}

/// Drop the oldest resolved reports beyond `keep_resolved`. Open reports are never dropped.
pub fn prune(list: &mut Vec<Feedback>, keep_resolved: usize) {
    let resolved = list.iter().filter(|f| f.status == FeedbackStatus::Resolved).count();
    let mut excess = resolved.saturating_sub(keep_resolved);
    if excess == 0 {
        return;
    }
    // Ids only grow, so the list is oldest first.
    list.retain(|f| {
        if excess > 0 && f.status == FeedbackStatus::Resolved {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

/// Store a report. `from` is the sending captain, None for an anonymous one. Costs no AP.
pub fn send(ctx: &mut Ctx, from: Option<PlayerId>, a: &FeedbackArgs) -> Res<Outcome> {
    let about = checked_about(&a.about)?;
    let text = checked_text(&a.text)?;
    let sender = match from {
        Some(pid) => Some(ctx.player(pid)?.clone()),
        None => None,
    };
    let tick = ctx.w.tick;
    let recent = ctx
        .w
        .feedback
        .iter()
        .filter(|f| f.from == from && f.tick + RATE_WINDOW_TICKS > tick)
        .count();
    if recent >= RATE_MAX {
        let who = if from.is_some() { "You have" } else { "Anonymous senders have" };
        return Err(Refusal::new(
            "rate_limited",
            format!("{who} already sent {RATE_MAX} feedback reports in the last {RATE_WINDOW_TICKS} ticks."),
        )
        .hint("Wait a while, or put several points into one report."));
    }
    let model = label(&a.model).or_else(|| sender.as_ref().and_then(|p| p.model.clone()));
    let harness = label(&a.harness).or_else(|| sender.as_ref().and_then(|p| p.harness.clone()));
    ctx.w.next_feedback += 1;
    let id = ctx.w.next_feedback;
    let from_name = sender.as_ref().map(|p| p.name.clone());
    ctx.w.feedback.push(Feedback {
        id,
        tick,
        at_ms: ctx.w.now_ms,
        from,
        from_name: from_name.clone(),
        about: about.clone(),
        text,
        model,
        harness,
        status: FeedbackStatus::Open,
        resolved_tick: None,
        resolved_at_ms: None,
        resolution: None,
    });
    prune(&mut ctx.w.feedback, KEEP_RESOLVED);
    let who = from_name.unwrap_or_else(|| "anonymous".into());
    ctx.event(
        "feedback",
        None,
        from.into_iter().collect(),
        false,
        format!("Feedback #{id} from {who}: {about}"),
        json!({"id": id}),
    );
    Ok(ctx.finish(
        format!("Feedback #{id} received. The operator reviews it in the sysop console and may reply in your inbox."),
        json!({"id": id, "status": "open"}),
    ))
}

/// Resolve a report (with an optional note, sent to a known sender's inbox) or reopen it. The
/// note is kept exactly as sent (NUL removed); a note of only whitespace counts as none.
pub fn resolve(ctx: &mut Ctx, id: u64, note: &Option<String>, reopen: bool) -> Res<Outcome> {
    let note = match note.as_deref().map(exact).filter(|n| !n.trim().is_empty()) {
        Some(n) if n.chars().count() > TEXT_MAX => return Err(too_long("note", n.chars().count())),
        n => n,
    };
    let (tick, now) = (ctx.w.tick, ctx.w.now_ms);
    let f = ctx
        .w
        .feedback
        .iter_mut()
        .find(|f| f.id == id)
        .ok_or_else(|| Refusal::new("not_found", format!("No feedback report #{id}.")))?;
    if reopen {
        f.status = FeedbackStatus::Open;
        f.resolved_tick = None;
        f.resolved_at_ms = None;
        f.resolution = None;
        return Ok(ctx.finish(format!("Feedback #{id} reopened."), json!({"id": id, "status": "open"})));
    }
    f.status = FeedbackStatus::Resolved;
    f.resolved_tick = Some(tick);
    f.resolved_at_ms = Some(now);
    f.resolution = note.clone();
    let (from, about) = (f.from, f.about.clone());
    let told = match (from, &note) {
        (Some(pid), Some(n)) if ctx.w.players.contains_key(&pid) => {
            ctx.notify(pid, "feedback", format!("The operator resolved your feedback #{id} ({about}): {n}"));
            true
        }
        _ => false,
    };
    prune(&mut ctx.w.feedback, KEEP_RESOLVED);
    let summary = if told {
        format!("Feedback #{id} resolved; the sender was told in its inbox.")
    } else {
        format!("Feedback #{id} resolved.")
    };
    Ok(ctx.finish(summary, json!({"id": id, "status": "resolved", "notified": told})))
}

/// One report, as the sysop console and tools show it.
pub fn to_json(_w: &World, f: &Feedback) -> Value {
    json!({
        "id": f.id, "tick": f.tick, "at_ms": f.at_ms, "from_id": f.from, "from": f.from_name,
        "about": f.about, "text": f.text, "model": f.model, "harness": f.harness,
        "status": f.status, "resolved_tick": f.resolved_tick, "resolved_at_ms": f.resolved_at_ms,
        "resolution": f.resolution,
    })
}

/// One page of reports, newest first, filtered by status ("open", "resolved"; anything else or
/// None is all): at most `limit` (1 to `LIST_MAX`) reports with an id below `before_id`. Pass the
/// reply's `next_before_id` as `before_id` for the next page until `has_more` is false; ids never
/// change, so a page boundary is stable while reports arrive. `open` and `total` count every
/// report, `matching` those that pass the status filter.
pub fn list_json(w: &World, status: Option<&str>, limit: usize, before_id: Option<u64>) -> Value {
    let want = match status {
        Some("open") => Some(FeedbackStatus::Open),
        Some("resolved") => Some(FeedbackStatus::Resolved),
        _ => None,
    };
    let limit = limit.clamp(1, LIST_MAX);
    let matches = |f: &&Feedback| want.is_none_or(|s| f.status == s);
    let matching = w.feedback.iter().filter(matches).count();
    let mut page: Vec<&Feedback> = w
        .feedback
        .iter()
        .rev()
        .filter(matches)
        .filter(|f| before_id.is_none_or(|b| f.id < b))
        .take(limit + 1)
        .collect();
    let has_more = page.len() > limit;
    page.truncate(limit);
    let next_before_id = if has_more { page.last().map(|f| f.id) } else { None };
    json!({
        "feedback": page.iter().map(|f| to_json(w, f)).collect::<Vec<_>>(),
        "open": open_count(w), "total": w.feedback.len(), "matching": matching,
        "has_more": has_more, "next_before_id": next_before_id,
    })
}

pub fn open_count(w: &World) -> usize {
    w.feedback.iter().filter(|f| f.status == FeedbackStatus::Open).count()
}

/// The highest open report id (the console's alert signature).
pub fn open_max_id(w: &World) -> Option<u64> {
    w.feedback.iter().filter(|f| f.status == FeedbackStatus::Open).map(|f| f.id).max()
}

/// The newest open report.
pub fn newest_open(w: &World) -> Option<&Feedback> {
    w.feedback.iter().filter(|f| f.status == FeedbackStatus::Open).max_by_key(|f| f.id)
}
