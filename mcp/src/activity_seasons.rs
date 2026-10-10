//! The interaction log by season, for the operator's Activity view: the live season, every
//! archived season and, when the log reaches further back than the first archived season,
//! "earlier". Served at `/api/sysop/activity?season=`, `/api/sysop/activity/seasons`,
//! `/api/sysop/calls?season=` and by the `sysop_activity` and `sysop_calls` tools. Never public.
//!
//! Totals are computed exactly where the record allows: every part of the season still inside
//! the raw interaction log is read from raw rows (split at the season's start and end by `at_ms`),
//! and whole UTC days older than the raw log come from the permanent daily summaries
//! (`activity_daily`). Both go through the same `activity::rollup_day`, so reconnects after a
//! restart and operator calls are left out of every count, the same way on both sides. A season
//! starts and ends mid-day, so a boundary day older than the raw log can only be taken whole from
//! its daily summary: the reply then says `exact: false` and lists it in `approximate_days`.
//! Sessions, recent calls, timings and the transport (`by_via`) exist only in raw rows.

use std::collections::{BTreeMap, BTreeSet};

use af_core::World;
use serde_json::{Value, json};

use crate::activity::{Call, DailyActivity, Identities, ToolDay};
use crate::archive::{DAY_MS, ViewErr, day_floor, day_of, parse_day};
use crate::game::{Game, HistOp, HistOut};

/// Calls shown in `recent` (the rest through `sysop_calls`).
const RECENT: usize = 50;

/// One season's time range, as the Activity view offers it.
#[derive(Clone, Debug, PartialEq)]
pub struct SeasonRange {
    /// An archived season's key (`af_core::history::season_key`), "current" or "earlier".
    pub key: String,
    /// None for "earlier".
    pub number: Option<u32>,
    pub label: String,
    pub from_ms: u64,
    /// None for the live season (it runs to now).
    pub to_ms: Option<u64>,
    pub current: bool,
    /// The captains the season recorded (id, call sign): its archived results, or the captains
    /// in the universe now for the live season. Empty for "earlier".
    pub captains: Vec<(u64, String)>,
}

impl SeasonRange {
    pub fn json(&self, has_raw: bool) -> Value {
        json!({"key": self.key, "number": self.number, "label": self.label, "from_ms": self.from_ms,
               "to_ms": self.to_ms, "current": self.current, "has_raw": has_raw})
    }
}

/// Every season the Activity view can show, newest first: the live season, each archived season
/// record, then "earlier" when activity (raw or daily) exists before the first archived season
/// (before the live season when nothing is archived). `earliest_ms`: the oldest activity known.
///
/// Ranges use journal time, as the archive does: an archived season runs from its `started_ms`
/// to its `ended_ms`; the live season from `history::season_started_ms` (recorded since engine
/// 1.34, else estimated from ticks: the journal time now minus the ticks since the season began
/// times the tick length) to now. A record without a start time starts where the previous one
/// ended.
pub fn ranges(w: &World, earliest_ms: Option<u64>) -> Vec<SeasonRange> {
    let seasons = &w.archive.seasons;
    let mut out = vec![SeasonRange {
        key: "current".into(),
        number: Some(w.season.number),
        label: format!("Season {}", w.season.number),
        from_ms: af_core::history::season_started_ms(w),
        to_ms: None,
        current: true,
        captains: w
            .players
            .values()
            .filter(|p| p.npc.is_none())
            .map(|p| (p.id as u64, p.name.clone()))
            .collect(),
    }];
    let dup = |n: u32| seasons.iter().filter(|s| s.number == n).count() > 1 || n == w.season.number;
    for (i, s) in seasons.iter().enumerate().rev() {
        let from = if s.started_ms > 0 {
            s.started_ms
        } else {
            i.checked_sub(1).map(|j| seasons[j].ended_ms).unwrap_or(0)
        };
        let label = if dup(s.number) {
            format!("Season {} (epoch {})", s.number, s.epoch)
        } else {
            format!("Season {}", s.number)
        };
        out.push(SeasonRange {
            key: af_core::history::season_key(s),
            number: Some(s.number),
            label,
            from_ms: from,
            to_ms: Some(s.ended_ms.max(from)),
            current: false,
            captains: s.captains.iter().map(|c| (c.id as u64, c.name.clone())).collect(),
        });
    }
    let first = out.iter().min_by_key(|r| r.from_ms).cloned();
    if let (Some(first), Some(earliest)) = (first, earliest_ms)
        && earliest < first.from_ms
    {
        out.push(SeasonRange {
            key: "earlier".into(),
            number: None,
            label: format!("Before season {}", first.number.unwrap_or(1)),
            from_ms: day_floor(earliest),
            to_ms: Some(first.from_ms),
            current: false,
            captains: vec![],
        });
    }
    out
}

/// One UTC day's share of a season: [from_ms, to_ms) within the day starting at `day_ms`, read
/// from raw rows (`raw`) or from that day's daily summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayPart {
    pub day_ms: u64,
    pub from_ms: u64,
    pub to_ms: u64,
    pub raw: bool,
}

impl DayPart {
    /// The season covers the whole day.
    pub fn whole(&self) -> bool {
        self.from_ms == self.day_ms && self.to_ms == self.day_ms + DAY_MS
    }
    /// Counted exactly: from raw rows split at the season's edges, or a whole day's summary.
    pub fn exact(&self) -> bool {
        self.raw || self.whole()
    }
}

/// Split [from, to) into UTC days. A day's part is read from raw rows when it starts at or after
/// `raw_from` (the raw log is complete from there), else from the day's summary.
pub fn plan(from: u64, to: u64, raw_from: Option<u64>) -> Vec<DayPart> {
    let mut out = vec![];
    let mut d = day_floor(from);
    while d < to {
        let a = from.max(d);
        let b = to.min(d + DAY_MS);
        out.push(DayPart {
            day_ms: d,
            from_ms: a,
            to_ms: b,
            raw: raw_from.is_some_and(|r| a >= r),
        });
        d += DAY_MS;
    }
    out
}

/// Where the raw log is complete from. Pruning deletes rows older than a cutoff and never touches
/// a newer row, so the log is complete from its oldest row on; the oldest row's whole day counts
/// too when nothing of that day was pruned: its raw rows add up to at least its daily summary (or
/// it has no summary yet). None: no raw rows at all.
pub fn raw_complete_from(oldest: Option<u64>, oldest_day_raw_calls: u64, oldest_day_summary_calls: Option<u64>) -> Option<u64> {
    let o = oldest?;
    match oldest_day_summary_calls {
        Some(s) if oldest_day_raw_calls < s => Some(o),
        _ => Some(day_floor(o)),
    }
}

/// Who a call is about: everyone, one captain, or the calls nobody was logged in for (0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    All,
    Player(u64),
}

impl Subject {
    fn has(&self, pid: u64) -> bool {
        match self {
            Subject::All => true,
            Subject::Player(p) => *p == pid,
        }
    }
}

/// Everything the season's summaries are made from.
pub struct SeasonData {
    pub range: SeasonRange,
    /// The end used for the live season (now).
    pub to_ms: u64,
    pub parts: Vec<DayPart>,
    /// Daily rows for every part: raw parts rolled up from their raw rows, the others the stored
    /// summary of that day. (day_ms, row).
    pub rows: Vec<(u64, DailyActivity)>,
    /// Raw calls inside the season (operator calls left out), newest first.
    pub raw: Vec<Call>,
    pub raw_from: Option<u64>,
}

#[derive(Default)]
struct Acc {
    calls: u64,
    ok: u64,
    refused: u64,
    codes: BTreeMap<String, u64>,
    tools: BTreeMap<String, ToolDay>,
    first: Option<u64>,
    last: Option<u64>,
    days: BTreeMap<u64, (u64, u64, u64)>,
}

impl Acc {
    fn add(&mut self, day_ms: u64, r: &DailyActivity) {
        self.calls += r.calls;
        self.ok += r.ok;
        self.refused += r.refused;
        for (k, v) in &r.refusal_codes {
            *self.codes.entry(k.clone()).or_default() += v;
        }
        for (k, t) in &r.tools {
            let e = self.tools.entry(k.clone()).or_default();
            e.calls += t.calls;
            e.refused += t.refused;
        }
        if r.calls > 0 {
            self.first = Some(self.first.map_or(r.first_ms, |f| f.min(r.first_ms)));
            self.last = Some(self.last.map_or(r.last_ms, |l| l.max(r.last_ms)));
        }
        let d = self.days.entry(day_ms).or_default();
        d.0 += r.calls;
        d.1 += r.ok;
        d.2 += r.refused;
    }
}

impl SeasonData {
    fn season_json(&self) -> Value {
        let approx: Vec<String> = self.parts.iter().filter(|p| !p.exact()).map(|p| day_of(p.day_ms)).collect();
        let exact = approx.is_empty();
        let note = if exact {
            "Exact: raw rows split at the season's start and end, and whole days of the daily summaries where the raw log no longer reaches.".to_string()
        } else {
            format!(
                "Approximate on {}: the season starts or ends on that day, which is older than the raw log, so the whole day's summary is counted (it may include calls from the neighbouring season). Every other day is exact.",
                approx.join(", ")
            )
        };
        json!({
            "key": self.range.key, "number": self.range.number, "label": self.range.label,
            "from_ms": self.range.from_ms, "to_ms": self.range.to_ms, "current": self.range.current,
            "exact": exact, "approximate_days": approx, "raw_from_ms": self.raw_from,
            "note": note,
            "counting": "Reconnects after a restart and operator calls are not counted. by_via, avg_ms, sessions and recent come only from the raw log (raw_from_ms on).",
        })
    }

    fn acc(&self, s: Subject) -> Acc {
        let mut a = Acc::default();
        for (d, r) in self.rows.iter().filter(|(_, r)| s.has(r.player_id)) {
            a.add(*d, r);
        }
        a
    }

    fn raw_of(&self, s: Subject) -> Vec<Call> {
        self.raw
            .iter()
            .filter(|c| s.has(c.player.map(u64::from).unwrap_or(0)))
            .cloned()
            .collect()
    }

    fn by_day(&self, a: &Acc) -> Vec<Value> {
        self.parts
            .iter()
            .map(|p| {
                let (calls, ok, refused) = a.days.get(&p.day_ms).copied().unwrap_or_default();
                json!({"day": day_of(p.day_ms), "day_start_ms": p.day_ms, "from_ms": p.from_ms, "to_ms": p.to_ms,
                       "calls": calls, "ok": ok, "refused": refused,
                       "source": if p.raw { "raw" } else { "daily" }, "exact": p.exact()})
            })
            .collect()
    }

    /// One subject's summary: the shape of `activity::summarize`, with `by_day` for `by_hour`.
    pub fn summary(&self, s: Subject, scope: Option<String>) -> Value {
        let a = self.acc(s);
        let raw = self.raw_of(s);
        // Raw extras per tool: timing, last call and refusal codes.
        // Per tool: (calls, total ms, last call, refusal codes).
        type Extra = (u64, u64, u64, BTreeMap<String, u64>);
        let mut extra: BTreeMap<&str, Extra> = BTreeMap::new();
        let mut via: BTreeMap<&str, u64> = BTreeMap::new();
        let mut reconnects = 0u64;
        for c in &raw {
            if c.is_reconnect() {
                reconnects += 1;
                continue;
            }
            *via.entry(&c.via).or_default() += 1;
            let e = extra.entry(&c.tool).or_default();
            e.0 += 1;
            e.1 += c.ms;
            e.2 = e.2.max(c.at_ms);
            if c.is_refusal() {
                *e.3.entry(c.code.clone().unwrap_or_else(|| "error".into())).or_default() += 1;
            }
        }
        let mut tools: Vec<(&String, &ToolDay)> = a.tools.iter().collect();
        tools.sort_by(|x, y| y.1.calls.cmp(&x.1.calls).then(x.0.cmp(y.0)));
        let by_tool: Vec<Value> = tools
            .into_iter()
            .map(|(t, d)| {
                let x = extra.get(t.as_str());
                json!({"tool": t, "calls": d.calls, "ok": d.calls - d.refused, "refused": d.refused,
                       "refusal_codes": x.map(|x| x.3.clone()).unwrap_or_default(),
                       "avg_ms": x.map(|x| x.1 / x.0.max(1)), "last_ms": x.map(|x| x.2)})
            })
            .collect();
        let preview = crate::activity::summarize(&raw, self.to_ms, 1);
        json!({
            "season": self.season_json(),
            "calls": a.calls, "ok": a.ok, "refused": a.refused, "reconnects": reconnects,
            "first_ms": a.first, "last_ms": a.last,
            "by_via": via, "refusal_codes": a.codes, "by_tool": by_tool,
            "by_day": self.by_day(&a),
            "sessions": preview["sessions"], "sessions_continuation": preview["sessions_continuation"],
            "recent": raw.iter().take(RECENT).collect::<Vec<_>>(),
            "recent_continuation": {
                "has_more": raw.len() > RECENT,
                "remaining_in_season": raw.len().saturating_sub(RECENT),
                "retrieval": {"tool": "sysop_calls", "arguments": {"captain": scope, "season": self.range.key, "limit": RECENT}},
            },
            "truncated": false,
        })
    }

    /// Every captain's totals, the calls nobody was logged in for, and everyone together.
    pub fn overview(&self, who: &Identities) -> Value {
        let mut names: BTreeMap<u64, String> = self.range.captains.iter().cloned().collect();
        for (_, r) in &self.rows {
            if r.player_id != 0 {
                names.entry(r.player_id).or_insert_with(|| r.name.clone());
            }
        }
        for (id, n) in names.iter_mut() {
            if n.starts_with('#')
                && let Some((name, _, _)) = who.get(id)
            {
                *n = name.clone();
            }
        }
        let mut per: BTreeMap<u64, (Acc, BTreeMap<&str, u64>)> = BTreeMap::new();
        for (d, r) in self.rows.iter().filter(|(_, r)| r.player_id != 0) {
            let e = per.entry(r.player_id).or_default();
            e.0.add(*d, r);
            for (t, x) in &r.tools {
                *e.1.entry(t).or_default() += x.calls;
            }
        }
        let mut players: Vec<Value> = names
            .iter()
            .map(|(id, name)| match per.get(id) {
                Some((a, tools)) => {
                    let mut top: Vec<(&&str, &u64)> = tools.iter().collect();
                    top.sort_by(|x, y| y.1.cmp(x.1).then(x.0.cmp(y.0)));
                    let top: Vec<String> = top.iter().take(3).map(|(t, n)| format!("{t} {n}")).collect();
                    json!({"id": id, "name": name, "calls": a.calls, "ok": a.ok, "refused": a.refused,
                           "refusal_codes": a.codes, "first_call_ms": a.first, "last_call_ms": a.last, "top_tools": top})
                }
                None => json!({"id": id, "name": name, "calls": 0, "ok": 0, "refused": 0, "refusal_codes": {},
                               "first_call_ms": null, "last_call_ms": null, "top_tools": []}),
            })
            .collect();
        players.sort_by(|a, b| b["last_call_ms"].as_u64().cmp(&a["last_call_ms"].as_u64()));
        let mut v = self.summary(Subject::All, None);
        let obj = v.as_object_mut().expect("summary");
        // Everyone's sessions and calls are read page by page with sysop_calls.
        for k in ["sessions", "sessions_continuation", "recent", "recent_continuation"] {
            obj.remove(k);
        }
        v["players"] = json!(players);
        v["not_logged_in"] = self.summary(Subject::Player(0), Some("@anon".into()));
        v
    }
}

fn unread() -> ViewErr {
    (503, "The activity log could not be read.".into())
}

/// The oldest raw row and the first day with daily summaries.
async fn earliest(game: &Game) -> Result<(Option<u64>, Option<u64>), ViewErr> {
    game.flush_activity().await;
    let oldest = match game.hist(HistOp::Oldest).await {
        Some(HistOut::Oldest(o)) => o,
        _ => return Err(unread()),
    };
    let first_day = match game.hist(HistOp::DailyDays).await {
        Some(HistOut::Days(d)) => d.iter().filter_map(|d| parse_day(d)).min(),
        _ => return Err(unread()),
    };
    Ok((oldest, first_day))
}

/// The season list for the console's tabs (see `ranges`), with whether the raw log reaches into
/// each season (sessions and the call log are available).
pub async fn seasons(game: &Game) -> Result<Value, ViewErr> {
    let (oldest, first_day) = earliest(game).await?;
    let earliest = [oldest, first_day].into_iter().flatten().min();
    let list = game.read(move |w| ranges(w, earliest)).await;
    let seasons: Vec<Value> = list
        .iter()
        .map(|r| r.json(oldest.is_some_and(|o| r.to_ms.is_none_or(|t| o < t))))
        .collect();
    Ok(json!({"seasons": seasons, "raw_from_ms": oldest, "retention_days": crate::activity::retention_days()}))
}

/// A season by key ("current", "earlier", an archived season's key) or by number (the newest
/// season with that number).
pub async fn resolve(game: &Game, key: &str) -> Result<SeasonRange, ViewErr> {
    let key = key.trim().to_string();
    let (oldest, first_day) = earliest(game).await?;
    let earliest = [oldest, first_day].into_iter().flatten().min();
    let list = game.read(move |w| ranges(w, earliest)).await;
    let n: Option<u32> = key.parse().ok();
    list.into_iter()
        .find(|r| r.key == key || (n.is_some() && r.number == n))
        .ok_or_else(|| (404, format!("No season with key {key}. List them at /api/sysop/activity/seasons.")))
}

fn rows_of(rows: Vec<af_store::DailyRow>) -> Vec<DailyActivity> {
    rows.iter().filter_map(|r| serde_json::from_slice(&r.data).ok()).collect()
}

/// Gather a season's raw rows and daily summaries (see the module comment).
pub async fn load(game: &Game, range: SeasonRange) -> Result<SeasonData, ViewErr> {
    game.flush_activity().await;
    let now = crate::game::now_ms();
    let to = range.to_ms.unwrap_or(now + 1).max(range.from_ms);
    let oldest = match game.hist(HistOp::Oldest).await {
        Some(HistOut::Oldest(o)) => o,
        _ => return Err(unread()),
    };
    let who = game.read(crate::archive::identities).await;
    let calls_of = |rows: Vec<af_store::ActivityRow>| -> Vec<Call> {
        rows.iter()
            .filter_map(|r| serde_json::from_slice::<Call>(&r.data).ok())
            .filter(|c| c.role != "sysop")
            .collect()
    };
    // Is the oldest raw row's whole day still there? Only matters when the season reaches it.
    let raw_from = match oldest {
        Some(o) if range.from_ms < day_floor(o) + DAY_MS => {
            let d0 = day_floor(o);
            let summary = match game.hist(HistOp::DailyQuery(day_of(d0), day_of(d0), None, usize::MAX)).await {
                Some(HistOut::Daily(r)) => rows_of(r),
                _ => return Err(unread()),
            };
            let summary_calls = (!summary.is_empty()).then(|| summary.iter().map(|r| r.calls).sum());
            let raw = match game.hist(HistOp::Between(d0, d0 + DAY_MS)).await {
                Some(HistOut::Raw(r)) => calls_of(r),
                _ => return Err(unread()),
            };
            let raw_calls = crate::activity::rollup_day(&day_of(d0), &raw, &who).iter().map(|r| r.calls).sum();
            raw_complete_from(oldest, raw_calls, summary_calls)
        }
        o => raw_complete_from(o, 0, None),
    };
    let parts = plan(range.from_ms, to, raw_from);
    let mut rows: Vec<(u64, DailyActivity)> = vec![];
    // Raw parts are a run at the end: one read covers them.
    let mut raw: Vec<Call> = vec![];
    if let Some(first_raw) = parts.iter().find(|p| p.raw) {
        let hi = if range.to_ms.is_none() { i64::MAX as u64 } else { to };
        raw = match game.hist(HistOp::Between(first_raw.from_ms, hi)).await {
            Some(HistOut::Raw(r)) => calls_of(r),
            _ => return Err(unread()),
        };
        let mut by_day: BTreeMap<u64, Vec<Call>> = BTreeMap::new();
        for c in &raw {
            by_day.entry(day_floor(c.at_ms)).or_default().push(c.clone());
        }
        for (d, calls) in by_day {
            for r in crate::activity::rollup_day(&day_of(d), &calls, &who) {
                rows.push((d, r));
            }
        }
        raw.reverse();
    }
    let daily: BTreeSet<u64> = parts.iter().filter(|p| !p.raw).map(|p| p.day_ms).collect();
    if let (Some(lo), Some(hi)) = (daily.first(), daily.last()) {
        match game.hist(HistOp::DailyQuery(day_of(*lo), day_of(*hi), None, usize::MAX)).await {
            Some(HistOut::Daily(r)) => {
                for row in r {
                    if let (Some(d), Ok(a)) = (parse_day(&row.day), serde_json::from_slice::<DailyActivity>(&row.data))
                        && daily.contains(&d)
                    {
                        rows.push((d, a));
                    }
                }
            }
            _ => return Err(unread()),
        }
    }
    Ok(SeasonData {
        range,
        to_ms: to,
        parts,
        rows,
        raw,
        raw_from,
    })
}

/// `/api/sysop/activity?season=`: every captain's totals over the season (with `name`: one
/// captain's detail; "@anon" or "anonymous": the calls nobody was logged in for).
pub async fn view(game: &Game, key: &str, name: Option<String>) -> Result<Value, ViewErr> {
    let range = resolve(game, key).await?;
    let subject = match name.as_ref().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        Some(n) => {
            let q = n.to_string();
            let found = game
                .read(move |w| crate::archive::player_id(w, &q).map(|id| (id, crate::archive::identities(w))))
                .await;
            match found {
                Some((id, who)) => Some((id, who)),
                None => return Err((404, format!("No captain named or numbered {n}."))),
            }
        }
        None => None,
    };
    let data = load(game, range).await?;
    match subject {
        Some((0, _)) => {
            let mut v = data.summary(Subject::Player(0), Some("@anon".into()));
            v["player_id"] = json!(0);
            v["name"] = json!("Not logged in");
            Ok(v)
        }
        Some((id, who)) => {
            let name = data
                .range
                .captains
                .iter()
                .find(|c| c.0 == id)
                .map(|c| c.1.clone())
                .or_else(|| who.get(&id).map(|w| w.0.clone()))
                .or_else(|| data.rows.iter().find(|r| r.1.player_id == id).map(|r| r.1.name.clone()))
                .unwrap_or_else(|| format!("#{id}"));
            let mut v = data.summary(Subject::Player(id), Some(id.to_string()));
            v["player_id"] = json!(id);
            v["name"] = json!(name);
            Ok(v)
        }
        None => {
            let who = game.read(crate::archive::identities).await;
            Ok(data.overview(&who))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 3_600_000;

    #[test]
    fn a_season_splits_into_utc_days_and_only_old_boundary_days_are_approximate() {
        let d = 100 * DAY_MS;
        // From mid-day 100 to mid-day 103; raw complete from day 102.
        let parts = plan(d + 5 * H, d + 3 * DAY_MS + 7 * H, Some(d + 2 * DAY_MS));
        assert_eq!(parts.len(), 4);
        assert_eq!((parts[0].from_ms, parts[0].to_ms), (d + 5 * H, d + DAY_MS));
        assert!(
            !parts[0].raw && !parts[0].whole() && !parts[0].exact(),
            "start boundary from the summary"
        );
        assert!(!parts[1].raw && parts[1].exact(), "a whole day's summary is exact");
        assert!(parts[2].raw && parts[2].exact());
        assert_eq!((parts[3].from_ms, parts[3].to_ms), (d + 3 * DAY_MS, d + 3 * DAY_MS + 7 * H));
        assert!(parts[3].raw && parts[3].exact(), "end boundary split by at_ms in raw rows");
        // Raw complete from mid-day: a day that starts before it is read from its summary.
        let parts = plan(d + 5 * H, d + DAY_MS + H, Some(d + 9 * H));
        assert!(!parts[0].raw && parts[1].raw);
        // Raw complete from before the season: everything raw and exact, boundaries included.
        assert!(plan(d + 5 * H, d + 2 * DAY_MS + H, Some(d)).iter().all(|p| p.raw && p.exact()));
        // No raw log: summaries only.
        assert!(plan(d, d + DAY_MS, None).iter().all(|p| !p.raw && p.exact()));
    }

    #[test]
    fn the_raw_log_counts_from_its_oldest_day_only_when_that_day_was_not_pruned() {
        let o = 50 * DAY_MS + 9 * H;
        assert_eq!(raw_complete_from(None, 0, None), None);
        assert_eq!(raw_complete_from(Some(o), 3, None), Some(50 * DAY_MS), "no summary yet");
        assert_eq!(raw_complete_from(Some(o), 7, Some(7)), Some(50 * DAY_MS), "nothing pruned");
        assert_eq!(raw_complete_from(Some(o), 4, Some(7)), Some(o), "partly pruned");
    }
}
