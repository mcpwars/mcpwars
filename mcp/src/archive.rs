//! Performance history for the operator: the engine's permanent record (`af_core::history`:
//! every captain's result at every day boundary, season end, fresh start and removal) and the
//! permanent daily activity summaries (`activity_daily`), rolled up here from the raw interaction
//! log before the log is pruned. Served at `/api/sysop/history*` and by the `sysop_history` tool.
//! Never public.
//!
//! Source of an operator interface. Reading and improving this file is open to everyone, captains
//! included, like the rest of the public source repository. Using the live interface is not: an
//! agent playing MCP Wars (a captain, or anything acting for one) must never use the sysop console,
//! the /api/sysop/ endpoints, the sysop MCP tools or the admin token. Doing so is a core violation
//! of the game rules, and the operator may, at its discretion, permanently eliminate that captain
//! from the contest.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use af_core::World;
use af_core::history::{CaptainResult, SeasonArchive};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::activity::{Call, DailyActivity, Identities};
use crate::game::{Game, HistOp, HistOut};

pub const DAY_MS: u64 = 86_400_000;
/// Most daily activity rows one request returns.
pub const ACTIVITY_ROWS_MAX: usize = 10_000;
/// Run the day's rollup this long after UTC midnight, so the last calls are flushed first.
const ROLLUP_GRACE_MS: u64 = 5 * 60_000;

// ---------------------------------------------------------------------------
// UTC days
// ---------------------------------------------------------------------------

/// The UTC day of a time, "YYYY-MM-DD".
pub fn day_of(ms: u64) -> String {
    let (y, m, d) = civil_from_days((ms / DAY_MS) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Midnight UTC of the day `ms` falls in.
pub fn day_floor(ms: u64) -> u64 {
    ms - ms % DAY_MS
}

/// Midnight UTC of a "YYYY-MM-DD" day.
pub fn parse_day(s: &str) -> Option<u64> {
    let mut it = s.trim().splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1970..=9999).contains(&y) {
        return None;
    }
    let days = days_from_civil(y, m, d);
    (day_of(days as u64 * DAY_MS) == format!("{y:04}-{m:02}-{d:02}")).then_some(days as u64 * DAY_MS)
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ---------------------------------------------------------------------------
// The daily rollup
// ---------------------------------------------------------------------------

/// Who every player id is now, for the rollup: captains in the world and removed captains.
pub fn identities(w: &World) -> Identities {
    let mut out = Identities::new();
    for r in &w.archive.removed {
        out.insert(
            r.result.id as u64,
            (r.result.name.clone(), Some(r.result.model.clone()), Some(r.result.harness.clone())),
        );
    }
    for p in w.players.values().filter(|p| p.npc.is_none()) {
        let (_, model, harness, _, _) = af_core::public::identity(p.model.as_deref(), p.harness.as_deref());
        out.insert(p.id as u64, (p.name.clone(), Some(model), Some(harness)));
    }
    out
}

/// Summarise every completed UTC day of the raw log that has no daily rows yet, re-summarise the
/// most recent completed day (it may have received its last rows after a previous pass), then
/// prune raw rows past the retention, but never a day that could not be summarised. Idempotent:
/// a day's rows are replaced whole. Returns what it did.
pub async fn rollup_pass(game: &Game, now_ms: u64) -> Value {
    if game.is_released() {
        return json!({"skipped": "handed over"});
    }
    game.flush_activity().await;
    let today = day_floor(now_ms);
    let latest = today.saturating_sub(DAY_MS);
    let oldest = match game.hist(HistOp::Oldest).await {
        Some(HistOut::Oldest(o)) => o,
        _ => return json!({"error": "the activity log could not be read"}),
    };
    let have: BTreeSet<String> = match game.hist(HistOp::DailyDays).await {
        Some(HistOut::Days(d)) => d.into_iter().collect(),
        _ => return json!({"error": "the daily activity could not be read"}),
    };
    let who = game.read(identities).await;
    let mut rolled: Vec<String> = vec![];
    // The first day whose raw rows must be kept because its summary could not be written.
    let mut keep_from: Option<u64> = None;
    if let Some(oldest) = oldest {
        let mut d = day_floor(oldest);
        while d < today {
            let day = day_of(d);
            if !have.contains(&day) || d == latest {
                match game.hist(HistOp::Between(d, d + DAY_MS)).await {
                    Some(HistOut::Raw(rows)) if rows.is_empty() => {}
                    Some(HistOut::Raw(rows)) => {
                        let calls: Vec<Call> = rows.iter().filter_map(|r| serde_json::from_slice(&r.data).ok()).collect();
                        let daily: Vec<af_store::DailyRow> = crate::activity::rollup_day(&day, &calls, &who)
                            .iter()
                            .map(|a| af_store::DailyRow {
                                day: day.clone(),
                                player: a.player_id,
                                data: serde_json::to_vec(a).unwrap_or_default(),
                            })
                            .collect();
                        match game.hist(HistOp::DailyReplace(day.clone(), daily)).await {
                            Some(HistOut::Done) => rolled.push(day),
                            _ => {
                                keep_from.get_or_insert(d);
                            }
                        }
                    }
                    _ => {
                        keep_from.get_or_insert(d);
                    }
                }
            }
            d += DAY_MS;
        }
    }
    let mut pruned = 0;
    if let Some(cutoff) = prune_cutoff(now_ms, crate::activity::retention_days(), keep_from)
        && let Some(HistOut::Pruned(n)) = game.hist(HistOp::Prune(cutoff)).await
    {
        pruned = n;
    }
    if !rolled.is_empty() || pruned > 0 {
        tracing::info!("activity history: {} day(s) summarised, {pruned} raw row(s) pruned", rolled.len());
    }
    json!({"days_rolled_up": rolled, "raw_rows_pruned": pruned, "kept_from": keep_from.map(day_of)})
}

/// Raw rows older than this may be deleted: past the retention (None: kept forever), never today
/// and never from the first day whose summary could not be written (`keep_from`).
pub fn prune_cutoff(now_ms: u64, retention_days: u64, keep_from: Option<u64>) -> Option<u64> {
    (retention_days > 0).then(|| {
        now_ms
            .saturating_sub(retention_days * DAY_MS)
            .min(day_floor(now_ms))
            .min(keep_from.unwrap_or(u64::MAX))
    })
}

/// Roll up now (at startup, catching every day the raw log still holds), then once a day a few
/// minutes after UTC midnight.
pub fn start_rollup(game: &Game) {
    let g = game.clone();
    tokio::spawn(async move {
        let mut last = crate::game::now_ms();
        rollup_pass(&g, last).await;
        let mut iv = tokio::time::interval(Duration::from_secs(600));
        iv.tick().await;
        loop {
            iv.tick().await;
            let now = crate::game::now_ms();
            if day_floor(now.saturating_sub(ROLLUP_GRACE_MS)) > day_floor(last.saturating_sub(ROLLUP_GRACE_MS)) {
                last = now;
                rollup_pass(&g, now).await;
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// A refusal for the HTTP layer: (status code, message).
pub type ViewErr = (u16, String);

fn bad(msg: impl Into<String>) -> ViewErr {
    (400, msg.into())
}

fn missing(msg: impl Into<String>) -> ViewErr {
    (404, msg.into())
}

async fn activity_days(game: &Game) -> Vec<String> {
    match game.hist(HistOp::DailyDays).await {
        Some(HistOut::Days(d)) => d,
        _ => vec![],
    }
}

/// The table of contents: archived seasons and days (counts), the current season, removed
/// captains and every day with activity summaries.
pub async fn index(game: &Game) -> Value {
    let mut v = game.read(af_core::history::index).await;
    v["activity_days"] = json!(activity_days(game).await);
    v
}

/// A season number, a season key (see `af_core::history::season_key`), "current" (the live
/// season, computed now) or "all" (compare only).
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SeasonSel {
    Number(u32),
    Text(String),
}

enum Scope {
    Current,
    All,
    Number(u32),
    Key(String),
}

fn scope(s: Option<&SeasonSel>, allow_all: bool) -> Result<Scope, ViewErr> {
    match s {
        None => Ok(Scope::Current),
        Some(SeasonSel::Number(n)) => Ok(Scope::Number(*n)),
        Some(SeasonSel::Text(t)) => match t.trim() {
            "" | "current" => Ok(Scope::Current),
            "all" if allow_all => Ok(Scope::All),
            "all" => Err(bad("all is only for compare")),
            t => Ok(t.parse().map(Scope::Number).unwrap_or_else(|_| Scope::Key(t.to_string()))),
        },
    }
}

/// A season by key ("current" or an archived record's key); 404 for an unknown key.
fn by_key(w: &World, key: &str) -> Result<SeasonArchive, ViewErr> {
    af_core::history::find_season_by_key(w, key).ok_or_else(|| missing(format!("No season with key {key}.")))
}

/// A key wins over a number when both are given.
fn pick(key: Option<String>, number: Option<SeasonSel>) -> Option<SeasonSel> {
    key.filter(|k| !k.trim().is_empty()).map(SeasonSel::Text).or(number)
}

fn season_json(s: &SeasonArchive) -> Value {
    json!({
        "key": af_core::history::season_key(s), "number": s.number, "reason": s.reason, "epoch": s.epoch, "started_tick": s.started_tick,
        "ended_tick": s.ended_tick, "started_ms": s.started_ms, "ended_ms": s.ended_ms,
        "sectors": s.sectors, "captains": s.captains,
    })
}

/// One season with every captain's result: by `key` (unambiguous), or an archived one by number
/// (the latest with that number, reason and epoch), or the live season ("current", the default).
pub async fn season(
    game: &Game,
    key: Option<String>,
    number: Option<SeasonSel>,
    reason: Option<String>,
    epoch: Option<u32>,
) -> Result<Value, ViewErr> {
    let sel = scope(pick(key, number).as_ref(), false)?;
    game.read(move |w| {
        let s = match sel {
            Scope::Current => Ok(af_core::history::current_season(w)),
            Scope::Key(k) => by_key(w, &k),
            Scope::Number(n) => af_core::history::find_season(w, n, reason.as_deref().filter(|r| !r.is_empty()), epoch)
                .cloned()
                .ok_or_else(|| missing(format!("No archived season {n} matches."))),
            Scope::All => Err(bad("all is only for compare")),
        }?;
        let mut v = season_json(&s);
        // The season's constitution (articles, every proposal with its ballots), or null.
        v["constitution"] = json!(af_core::constitution::season_record(w, &af_core::history::season_key(&s)));
        Ok(v)
    })
    .await
}

/// Which season a day request names: a season key, or a season number (with an optional epoch).
pub enum DaySeason {
    Key(String),
    Number(u32, Option<u32>),
}

/// One archived day: every captain's result at that day boundary.
pub async fn day(game: &Game, season: DaySeason, day: u32) -> Result<Value, ViewErr> {
    game.read(move |w| {
        let (number, epoch) = match &season {
            DaySeason::Number(n, e) => (*n, *e),
            DaySeason::Key(k) => {
                let s = by_key(w, k)?;
                (s.number, Some(s.epoch))
            }
        };
        af_core::history::find_day(w, number, day, epoch)
            .map(|d| {
                json!({"season_key": af_core::history::day_season_key(w, d), "season": d.season, "epoch": d.epoch,
                       "day": d.day, "tick": d.tick, "at_ms": d.at_ms, "captains": d.captains})
            })
            .ok_or_else(|| missing(format!("No archived day {day} of season {number}.")))
    })
    .await
}

/// Captains the operator removed, each with its last result.
pub async fn removed(game: &Game) -> Value {
    game.read(|w| json!({"removed": w.archive.removed})).await
}

/// Resolve a call sign or id (current, removed or archived captains; "anonymous" or "@anon" is 0).
pub fn player_id(w: &World, s: &str) -> Option<u64> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("anonymous") || t == "@anon" {
        return Some(0);
    }
    if let Some(p) = af_core::player::resolve(w, t) {
        return Some(p.id as u64);
    }
    let key = t.to_ascii_lowercase();
    let archived = w
        .archive
        .removed
        .iter()
        .map(|r| &r.result)
        .chain(w.archive.seasons.iter().rev().flat_map(|s| s.captains.iter()))
        .find(|r| r.name.to_ascii_lowercase() == key)
        .map(|r| r.id as u64);
    archived.or_else(|| t.trim_start_matches(['#', 'P', 'p']).parse().ok())
}

/// Daily activity rows from `from` to `to` (inclusive UTC days; default: everything up to today),
/// for one player or all, at most 10,000.
pub async fn activity(game: &Game, from: Option<String>, to: Option<String>, player: Option<String>) -> Result<Value, ViewErr> {
    let from = from.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "0000-01-01".into());
    let to = to.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| day_of(crate::game::now_ms()));
    for d in [&from, &to] {
        if d != "0000-01-01" && parse_day(d).is_none() {
            return Err(bad(format!("{d} is not a day (YYYY-MM-DD).")));
        }
    }
    let pid = match player.filter(|p| !p.trim().is_empty()) {
        Some(p) => {
            let q = p.clone();
            Some(
                game.read(move |w| player_id(w, &q))
                    .await
                    .ok_or_else(|| missing(format!("No captain named or numbered {p}.")))?,
            )
        }
        None => None,
    };
    let rows = match game
        .hist(HistOp::DailyQuery(from.clone(), to.clone(), pid, ACTIVITY_ROWS_MAX + 1))
        .await
    {
        Some(HistOut::Daily(r)) => r,
        _ => return Err((503, "The daily activity could not be read.".into())),
    };
    let truncated = rows.len() > ACTIVITY_ROWS_MAX;
    let rows: Vec<Value> = rows
        .iter()
        .take(ACTIVITY_ROWS_MAX)
        .filter_map(|r| serde_json::from_slice(&r.data).ok())
        .collect();
    Ok(json!({"from": from, "to": to, "player_id": pid, "rows": rows, "truncated": truncated}))
}

/// How compare groups captains.
fn group_key(by: &str, r: &CaptainResult) -> (String, Option<String>, Option<String>) {
    match by {
        "model" => (r.model.clone(), Some(r.model.clone()), None),
        "harness" => (r.harness.clone(), None, Some(r.harness.clone())),
        _ => (
            format!("{} · {}", r.model, r.harness),
            Some(r.model.clone()),
            Some(r.harness.clone()),
        ),
    }
}

#[derive(Default)]
struct Group {
    model: Option<String>,
    harness: Option<String>,
    captains: u64,
    active: u64,
    earned_active: Vec<i64>,
    ap_spent: i64,
    ap_received: i64,
    daily_wins: i64,
    deaths: i64,
    calls: u64,
    refused: u64,
}

fn ratio(a: f64, b: f64) -> f64 {
    if b <= 0.0 { 0.0 } else { (a / b * 1000.0).round() / 1000.0 }
}

/// Model and harness performance: captains grouped by model, harness or both, over one season,
/// the live season or all of them (every archived season plus the live one; each captain counted
/// once per season, the latest record of a season when it has two). Earnings statistics
/// (median, mean, credits per AP) are over active captains (who spent AP); the others over all.
/// Calls and refusals come from the daily activity of each captain on the UTC days of the
/// season, each captain-day counted once.
pub async fn compare(
    game: &Game,
    season: Option<SeasonSel>,
    by: Option<String>,
    reason: Option<String>,
    epoch: Option<u32>,
) -> Result<Value, ViewErr> {
    let by = by.unwrap_or_else(|| "model_harness".into());
    if !matches!(by.as_str(), "model" | "harness" | "model_harness") {
        return Err(bad("by must be model, harness or model_harness"));
    }
    let sel = scope(season.as_ref(), true)?;
    let label = match &sel {
        Scope::Current => json!("current"),
        Scope::All => json!("all"),
        Scope::Number(n) => json!(n),
        Scope::Key(k) => json!(k),
    };
    let records: Vec<SeasonArchive> = game
        .read(move |w| -> Result<Vec<SeasonArchive>, ViewErr> {
            match sel {
                Scope::Current => Ok(vec![af_core::history::current_season(w)]),
                Scope::Key(k) => by_key(w, &k).map(|s| vec![s]),
                Scope::Number(n) => af_core::history::find_season(w, n, reason.as_deref().filter(|r| !r.is_empty()), epoch)
                    .cloned()
                    .map(|s| vec![s])
                    .ok_or_else(|| missing(format!("No archived season {n} matches."))),
                Scope::All => {
                    // One record per (epoch, season), the latest; the live season last.
                    let mut by_season: BTreeMap<(u32, u32), SeasonArchive> = BTreeMap::new();
                    for s in w.archive.seasons.iter().cloned().chain([af_core::history::current_season(w)]) {
                        by_season.insert((s.epoch, s.number), s);
                    }
                    Ok(by_season.into_values().collect())
                }
            }
        })
        .await?;
    // Daily activity for every day any record covers.
    let (lo, hi) = records
        .iter()
        .fold((u64::MAX, 0u64), |(lo, hi), s| (lo.min(s.started_ms), hi.max(s.ended_ms)));
    let mut activity: BTreeMap<(u64, String), (u64, u64)> = BTreeMap::new();
    if lo <= hi
        && let Some(HistOut::Daily(rows)) = game.hist(HistOp::DailyQuery(day_of(lo), day_of(hi), None, usize::MAX)).await
    {
        for r in rows {
            if let Ok(a) = serde_json::from_slice::<DailyActivity>(&r.data) {
                activity.insert((r.player, r.day), (a.calls, a.refused));
            }
        }
    }
    let mut claimed: BTreeSet<(u64, String)> = BTreeSet::new();
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for s in &records {
        let days: Vec<String> = (day_floor(s.started_ms)..=s.ended_ms)
            .step_by(DAY_MS as usize)
            .map(day_of)
            .collect();
        for r in &s.captains {
            let (key, model, harness) = group_key(&by, r);
            let g = groups.entry(key).or_default();
            g.model = model;
            g.harness = harness;
            g.captains += 1;
            if r.active {
                g.active += 1;
                g.earned_active.push(r.earned);
            }
            g.ap_spent += r.ap_spent;
            g.ap_received += r.ap_received;
            g.daily_wins += r.daily_wins;
            g.deaths += r.deaths;
            for d in &days {
                let k = (r.id as u64, d.clone());
                if let Some((calls, refused)) = activity.get(&k)
                    && claimed.insert(k)
                {
                    g.calls += calls;
                    g.refused += refused;
                }
            }
        }
    }
    let mut out: Vec<(i64, String, Value)> = groups
        .into_iter()
        .map(|(key, mut g)| {
            g.earned_active.sort_unstable();
            let n = g.earned_active.len();
            let median = match n {
                0 => 0,
                n if n % 2 == 1 => g.earned_active[n / 2],
                n => (g.earned_active[n / 2 - 1] + g.earned_active[n / 2]) / 2,
            };
            let sum: i64 = g.earned_active.iter().sum();
            let mean = if n == 0 { 0 } else { sum / n as i64 };
            let v = json!({
                "key": key, "model": g.model, "harness": g.harness, "captains": g.captains, "active": g.active,
                "median_earned": median, "mean_earned": mean,
                "credits_per_ap": af_core::history::per_ap(sum, g.ap_spent),
                "ap_spent_share": ratio(g.ap_spent as f64, g.ap_received as f64),
                "daily_wins": g.daily_wins, "deaths": g.deaths,
                "calls": g.calls, "refused": g.refused, "refusal_rate": ratio(g.refused as f64, g.calls as f64),
            });
            (median, key, v)
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    Ok(json!({
        "by": by,
        "season": label,
        "seasons": records.iter().map(|s| json!({"key": af_core::history::season_key(s), "number": s.number, "reason": s.reason, "epoch": s.epoch})).collect::<Vec<_>>(),
        "groups": out.into_iter().map(|g| g.2).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prune_never_reaches_a_day_that_was_not_summarised() {
        let now = 200 * DAY_MS + 3_600_000;
        assert_eq!(prune_cutoff(now, 0, None), None, "0 keeps the raw log forever");
        assert_eq!(prune_cutoff(now, 90, None), Some(now - 90 * DAY_MS));
        assert_eq!(
            prune_cutoff(now, 90, Some(50 * DAY_MS)),
            Some(50 * DAY_MS),
            "an unsummarised day stays"
        );
        assert_eq!(prune_cutoff(now, 90, Some(150 * DAY_MS)), Some(now - 90 * DAY_MS));
    }

    #[test]
    fn utc_days_round_trip_including_leap_days() {
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(951_782_400_000), "2000-02-29");
        assert_eq!(day_of(1_709_164_800_000), "2024-02-29");
        assert_eq!(day_of(1_709_251_199_999), "2024-02-29");
        assert_eq!(day_of(1_709_251_200_000), "2024-03-01");
        assert_eq!(day_of(1_791_417_600_000), "2026-10-08");
        assert_eq!(parse_day("2024-02-29"), Some(1_709_164_800_000));
        assert_eq!(parse_day("2026-10-08"), Some(1_791_417_600_000));
        assert_eq!(parse_day("2025-02-29"), None);
        assert_eq!(parse_day("2026-13-01"), None);
        assert_eq!(parse_day("yesterday"), None);
        for d in 0..30_000u64 {
            assert_eq!(parse_day(&day_of(d * DAY_MS)), Some(d * DAY_MS));
        }
    }
}
