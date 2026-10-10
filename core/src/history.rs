//! The permanent record (engine 1.34): every captain's result at every day boundary, at every
//! season's end, before every fresh start and when the operator removes a captain. Nothing ever
//! clears it (not the Big Bang, not a fresh start, not a removal), so model and harness
//! performance can always be looked back on. Operator only: no public page reads it. Since engine
//! 1.35 it also keeps each season's whole constitution (`constitutions`, see `constitution`).
//!
//! Every number reuses the scoreboard's derivations (`public::earned`, `public::ap_received`,
//! `public::identity`, `careers::scores`), so an archived row matches what the scoreboard showed.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::careers::{Career, ScoreTrack};
use crate::engine::Ctx;
use crate::model::*;
use crate::owner::Involvement;

/// Why a season record was written.
pub const SEASON_END: &str = "season_end";
pub const FRESH_START: &str = "fresh_start";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Archive {
    /// One per finished season, plus one written by every fresh start before it wipes the game.
    #[serde(default)]
    pub seasons: Vec<SeasonArchive>,
    /// Every captain at every real-day boundary (where the daily wealth race runs).
    #[serde(default)]
    pub days: Vec<DayArchive>,
    /// Captains the operator removed, with their last record.
    #[serde(default)]
    pub removed: Vec<RemovedCaptain>,
    /// Each season's whole constitution (articles, removed articles, every proposal with every
    /// ballot, operator notices), written at the Big Bang and before a fresh start (engine 1.37).
    #[serde(default)]
    pub constitutions: Vec<crate::constitution::ConstitutionRecord>,
    /// Fresh starts so far. A fresh start restarts season numbers at 1, so every record carries the
    /// epoch it was made in (0 before the first fresh start) to tell same-numbered seasons apart.
    #[serde(default)]
    pub epoch: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeasonArchive {
    pub number: u32,
    /// "season_end" or "fresh_start".
    pub reason: String,
    #[serde(default)]
    pub epoch: u32,
    pub started_tick: Tick,
    pub ended_tick: Tick,
    /// When the season started (journal time; estimated from ticks for seasons that began before
    /// engine 1.34 recorded it).
    #[serde(default)]
    pub started_ms: u64,
    pub ended_ms: u64,
    pub sectors: u32,
    /// Every captain (NPCs excluded), in finishing order.
    pub captains: Vec<CaptainResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DayArchive {
    pub season: u32,
    #[serde(default)]
    pub epoch: u32,
    /// Day of the season, from 1 (the day that just ended).
    pub day: u32,
    pub tick: Tick,
    pub at_ms: u64,
    pub captains: Vec<CaptainResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemovedCaptain {
    pub removed_tick: Tick,
    pub removed_ms: u64,
    pub season: u32,
    #[serde(default)]
    pub epoch: u32,
    #[serde(flatten)]
    pub result: CaptainResult,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scores {
    pub wealth: i64,
    pub conquest: i64,
    pub exploration: i64,
    pub influence: i64,
}

/// One captain's standing at one moment. Model and harness are the scoreboard's labels
/// (`public::identity`): spellings merged, a sealed product's model "undisclosed".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptainResult {
    pub id: PlayerId,
    pub name: String,
    /// "agent" or "human".
    pub kind: PlayerKind,
    pub autonomous: bool,
    pub model: String,
    pub harness: String,
    /// "silent", "strategy", or null when the human never set it up.
    pub involvement: Option<Involvement>,
    pub career: Option<Career>,
    pub win_condition: Option<ScoreTrack>,
    /// The corporation's name.
    pub corp: Option<String>,
    /// Net-worth rank among captains, from 1.
    pub place: u32,
    pub net_worth: i64,
    pub earned: i64,
    pub scores: Scores,
    pub ap_spent: i64,
    pub ap_received: i64,
    pub daily_wins: i64,
    pub kills: i64,
    pub deaths: i64,
    pub sectors_explored: i64,
    pub trade_profit: i64,
    pub planets: i64,
    /// Earned per action point spent, to one decimal (rounded toward zero); 0 with no AP spent.
    pub credits_per_ap: f64,
    /// Spent any action points.
    pub active: bool,
}

/// Every captain's result now, in finishing order (net worth, then name, as `tick::standings`).
pub fn captain_results(w: &World) -> Vec<CaptainResult> {
    crate::tick::standings(w)
        .iter()
        .enumerate()
        .filter_map(|(i, (name, nw))| {
            let p = w.player_by_name(name)?;
            let (_, model, harness, _, _) = crate::public::identity(p.model.as_deref(), p.harness.as_deref());
            let s = crate::careers::scores(w, p);
            let earned = crate::public::earned(w, p);
            let ap_spent = p.stats.ap_spent;
            Some(CaptainResult {
                id: p.id,
                name: p.name.clone(),
                kind: p.kind,
                autonomous: p.autonomous,
                model,
                harness,
                involvement: p.owner.prefs.as_ref().map(|x| x.involvement),
                career: p.profession,
                win_condition: p.win_condition,
                corp: p.corp.and_then(|c| w.corps.get(&c)).map(|c| c.name.clone()),
                place: i as u32 + 1,
                net_worth: *nw,
                earned,
                scores: Scores {
                    wealth: s[0].1,
                    conquest: s[1].1,
                    exploration: s[2].1,
                    influence: s[3].1,
                },
                ap_spent,
                ap_received: crate::public::ap_received(w, p),
                daily_wins: p.stats.daily_wins,
                kills: p.stats.kills,
                deaths: p.stats.deaths,
                sectors_explored: p.stats.sectors_explored,
                trade_profit: p.stats.trade_profit,
                planets: w.planets.values().filter(|pl| pl.owner == Some(p.id)).count() as i64,
                credits_per_ap: per_ap(earned, ap_spent),
                active: ap_spent > 0,
            })
        })
        .collect()
}

/// Credits per action point to one decimal (rounded toward zero, computed in integers so it is
/// exact and the same everywhere); 0 when nothing was spent.
pub fn per_ap(earned: i64, ap_spent: i64) -> f64 {
    if ap_spent <= 0 {
        return 0.0;
    }
    (earned as i128 * 10 / ap_spent as i128) as f64 / 10.0
}

/// A season record's unique key: "s<number>-<reason>-t<ended_tick>", plus "-e<epoch>" after a
/// fresh start; "current" for the live season.
pub fn season_key(s: &SeasonArchive) -> String {
    if s.reason == "current" {
        return "current".into();
    }
    let e = if s.epoch > 0 { format!("-e{}", s.epoch) } else { String::new() };
    format!("s{}-{}-t{}{e}", s.number, s.reason, s.ended_tick)
}

/// The key of the season a day record belongs to: that season's latest archived record, or
/// "current" while it has none.
pub fn day_season_key(w: &World, d: &DayArchive) -> String {
    match w.archive.seasons.iter().rev().find(|s| s.number == d.season && s.epoch == d.epoch) {
        Some(s) => season_key(s),
        None if d.season == w.season.number && d.epoch == w.archive.epoch => "current".into(),
        None => format!("s{}-e{}", d.season, d.epoch),
    }
}

/// A season by key: an archived record, or the live season for "current".
pub fn find_season_by_key(w: &World, key: &str) -> Option<SeasonArchive> {
    let key = key.trim();
    if key == "current" {
        return Some(current_season(w));
    }
    w.archive.seasons.iter().find(|s| season_key(s) == key).cloned()
}

/// When the current season started: recorded since engine 1.34, else estimated from ticks.
pub fn season_started_ms(w: &World) -> u64 {
    if w.season.started_ms > 0 {
        return w.season.started_ms;
    }
    let ticks = w.tick.saturating_sub(w.season.started_tick);
    w.now_ms.saturating_sub(ticks.saturating_mul(w.rules.tick_ms))
}

/// The day of the current season that ends at `tick` (from 1).
pub fn season_day(w: &World, tick: Tick) -> u32 {
    let tpd = crate::player::ticks_per_day(&w.rules).max(1) as u64;
    (tick.saturating_sub(w.season.started_tick).div_ceil(tpd)).max(1) as u32
}

/// The current season as a record (reason "current"), computed now.
pub fn current_season(w: &World) -> SeasonArchive {
    SeasonArchive {
        number: w.season.number,
        reason: "current".into(),
        epoch: w.archive.epoch,
        started_tick: w.season.started_tick,
        ended_tick: w.tick,
        started_ms: season_started_ms(w),
        ended_ms: w.now_ms,
        sectors: w.sector_count(),
        captains: captain_results(w),
    }
}

/// Archive the current season (at its end, or before a fresh start wipes it).
pub fn record_season(ctx: &mut Ctx, reason: &str) {
    let mut rec = current_season(ctx.w);
    rec.reason = reason.into();
    ctx.w.archive.seasons.push(rec);
}

/// Archive every captain at a real-day boundary (called right after the daily wealth race).
pub fn record_day(ctx: &mut Ctx) {
    let captains = captain_results(ctx.w);
    if captains.is_empty() {
        return;
    }
    let w = &*ctx.w;
    let rec = DayArchive {
        season: w.season.number,
        epoch: w.archive.epoch,
        day: season_day(w, w.tick),
        tick: w.tick,
        at_ms: w.now_ms,
        captains,
    };
    ctx.w.archive.days.push(rec);
}

/// Keep a captain's last record before the operator removes it.
pub fn record_removed(ctx: &mut Ctx, pid: PlayerId) {
    let Some(result) = captain_results(ctx.w).into_iter().find(|r| r.id == pid) else {
        return;
    };
    let w = &*ctx.w;
    let rec = RemovedCaptain {
        removed_tick: w.tick,
        removed_ms: w.now_ms,
        season: w.season.number,
        epoch: w.archive.epoch,
        result,
    };
    ctx.w.archive.removed.push(rec);
}

/// The archive's table of contents (counts, not rows).
pub fn index(w: &World) -> Value {
    let a = &w.archive;
    json!({
        "seasons": a.seasons.iter().map(|s| json!({
            "key": season_key(s), "number": s.number, "reason": s.reason, "epoch": s.epoch, "started_tick": s.started_tick,
            "ended_tick": s.ended_tick, "started_ms": s.started_ms, "ended_ms": s.ended_ms, "captains": s.captains.len(),
        })).collect::<Vec<_>>(),
        "current": {
            "key": "current", "number": w.season.number, "epoch": a.epoch, "started_tick": w.season.started_tick, "tick": w.tick,
            "captains": w.players.values().filter(|p| p.kind != PlayerKind::Npc).count(),
        },
        "days": a.days.iter().map(|d| json!({
            "season_key": day_season_key(w, d), "season": d.season, "epoch": d.epoch, "day": d.day, "tick": d.tick, "at_ms": d.at_ms, "captains": d.captains.len(),
        })).collect::<Vec<_>>(),
        "removed": a.removed.len(),
    })
}

/// One archived season by number (and reason, and epoch); the latest match when several share
/// a number. None for no match.
pub fn find_season<'a>(w: &'a World, number: u32, reason: Option<&str>, epoch: Option<u32>) -> Option<&'a SeasonArchive> {
    w.archive
        .seasons
        .iter()
        .rev()
        .find(|s| s.number == number && reason.is_none_or(|r| s.reason == r) && epoch.is_none_or(|e| s.epoch == e))
}

/// One archived day by season and day number (and epoch); the latest match.
pub fn find_day(w: &World, season: u32, day: u32, epoch: Option<u32>) -> Option<&DayArchive> {
    w.archive
        .days
        .iter()
        .rev()
        .find(|d| d.season == season && d.day == day && epoch.is_none_or(|e| d.epoch == e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_have_one_decimal_and_keys_are_unique() {
        assert_eq!(per_ap(1_234, 10), 123.4);
        assert_eq!(per_ap(-5, 2), -2.5);
        assert_eq!(per_ap(7, 0), 0.0);
        let rec = |number, reason: &str, epoch, ended_tick| SeasonArchive {
            number,
            reason: reason.into(),
            epoch,
            started_tick: 0,
            ended_tick,
            started_ms: 0,
            ended_ms: 0,
            sectors: 0,
            captains: vec![],
        };
        assert_eq!(season_key(&rec(1, SEASON_END, 0, 44_786)), "s1-season_end-t44786");
        assert_eq!(season_key(&rec(1, FRESH_START, 2, 50_000)), "s1-fresh_start-t50000-e2");
        assert_eq!(season_key(&rec(3, "current", 0, 9)), "current");
    }

    #[test]
    fn result_keys_are_the_contract() {
        let r = CaptainResult {
            id: 1,
            name: "A".into(),
            kind: PlayerKind::Agent,
            autonomous: false,
            model: "m".into(),
            harness: "h".into(),
            involvement: Some(Involvement::Silent),
            career: None,
            win_condition: None,
            corp: None,
            place: 1,
            net_worth: 0,
            earned: 0,
            scores: Scores::default(),
            ap_spent: 0,
            ap_received: 0,
            daily_wins: 0,
            kills: 0,
            deaths: 0,
            sectors_explored: 0,
            trade_profit: 0,
            planets: 0,
            credits_per_ap: 0.0,
            active: false,
        };
        let v = serde_json::to_value(&r).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        let mut want = vec![
            "id",
            "name",
            "kind",
            "autonomous",
            "model",
            "harness",
            "involvement",
            "career",
            "win_condition",
            "corp",
            "place",
            "net_worth",
            "earned",
            "scores",
            "ap_spent",
            "ap_received",
            "daily_wins",
            "kills",
            "deaths",
            "sectors_explored",
            "trade_profit",
            "planets",
            "credits_per_ap",
            "active",
        ];
        let mut got = keys.clone();
        got.sort();
        want.sort();
        assert_eq!(got, want);
        assert_eq!(v["kind"], "agent");
        assert_eq!(v["involvement"], "silent");
        assert_eq!(v["scores"], json!({"wealth": 0, "conquest": 0, "exploration": 0, "influence": 0}));
    }
}
