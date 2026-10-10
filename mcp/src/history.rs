//! The scoreboard over time. Every few minutes the server reads the public scoreboard
//! (`af_core::public::scoreboard`); once an hour it keeps a point (each captain's earned, action
//! points spent and trading profit) for the season's trend lines, and when the season changes (a Big
//! Bang or a fresh start) it files the season's last scoreboard in the archive. All of it sits
//! beside the universe in the meta table (`console:public:history`, `console:public:history_last`,
//! `console:public:seasons`), saved as it changes and before a shutdown or handover. Never
//! journaled, never replayed, not exported.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use af_core::model::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::game::{Game, now_ms};

const KEY_POINTS: &str = "public:history";
const KEY_LAST: &str = "public:history_last";
const KEY_SEASONS: &str = "public:seasons";
/// Read the scoreboard this often (minutes); keep a trend point every `POINT_EVERY_MS`.
const SAMPLE_EVERY: u64 = 5;
const POINT_EVERY_MS: u64 = 3_600_000;
/// Points kept: a 30-day season of hourly points, with a margin.
const KEEP_POINTS: usize = 800;
/// Seasons kept in the archive.
const KEEP_SEASONS: usize = 60;

/// One hourly point: per captain, [earned, action points spent, trading profit].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Point {
    pub tick: Tick,
    pub at_ms: u64,
    pub rows: BTreeMap<String, [i64; 3]>,
}

/// This season's trend points.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Points {
    pub season: u32,
    pub season_start: Tick,
    pub points: Vec<Point>,
}

#[derive(Default)]
pub struct History {
    pub points: Points,
    /// The latest scoreboard of the season being recorded (filed when the season changes).
    pub last: Option<Value>,
    pub seasons: Vec<Value>,
}

pub type HistoryState = Arc<Mutex<History>>;

pub fn state() -> HistoryState {
    Arc::new(Mutex::new(History::default()))
}

fn season_of(board: &Value) -> (u32, Tick) {
    (
        board["season"]["number"].as_u64().unwrap_or(0) as u32,
        board["season"]["started_tick"].as_u64().unwrap_or(0),
    )
}

impl Game {
    /// Restore the trend points, the latest scoreboard and the archive saved by the previous server.
    /// If the archive is empty and exactly one season has finished, rebuild that season from career
    /// totals (`af_core::public::season_from_career`).
    pub async fn load_history(&self) {
        let points = self
            .console_get(KEY_POINTS)
            .await
            .and_then(|v| serde_json::from_value::<Points>(v).ok());
        let last = self.console_get(KEY_LAST).await.filter(|v| !v.is_null());
        let seasons = self
            .console_get(KEY_SEASONS)
            .await
            .and_then(|v| serde_json::from_value::<Vec<Value>>(v).ok())
            .unwrap_or_default();
        let rebuilt = if seasons.is_empty() {
            self.read(af_core::public::season_from_career).await
        } else {
            None
        };
        let added = rebuilt.is_some();
        {
            let mut h = self.history.lock().unwrap_or_else(|p| p.into_inner());
            h.points = points.unwrap_or_default();
            h.last = last;
            h.seasons = seasons;
            h.seasons.extend(rebuilt);
            tracing::info!(
                "public history: {} points, {} archived seasons",
                h.points.points.len(),
                h.seasons.len()
            );
        }
        if added {
            self.save_seasons().await;
        }
    }

    async fn save_points(&self) {
        let p = self.history.lock().unwrap_or_else(|p| p.into_inner()).points.clone();
        let _ = self.console_put(KEY_POINTS, &json!(p)).await;
    }

    async fn save_last(&self) {
        let l = self.history.lock().unwrap_or_else(|p| p.into_inner()).last.clone();
        let _ = self.console_put(KEY_LAST, &json!(l)).await;
    }

    async fn save_seasons(&self) {
        let s = self.history.lock().unwrap_or_else(|p| p.into_inner()).seasons.clone();
        let _ = self.console_put(KEY_SEASONS, &json!(s)).await;
    }

    /// Save everything (before a shutdown or handover; refused, harmlessly, after a handover).
    pub async fn save_history(&self) {
        self.save_points().await;
        self.save_last().await;
        self.save_seasons().await;
    }

    /// Read the scoreboard now: file the previous season if it changed, keep the board, and add a
    /// trend point when the last one is an hour old.
    pub async fn sample_history(&self) {
        let board = self.read(af_core::public::scoreboard).await;
        let (season, start) = season_of(&board);
        let now = now_ms();
        let (filed, pointed) = {
            let mut h = self.history.lock().unwrap_or_else(|p| p.into_inner());
            let mut filed = false;
            if h.points.season != season || h.points.season_start != start {
                if let Some(last) = h.last.take() {
                    let (old, old_start) = season_of(&last);
                    if old > 0 && !h.seasons.iter().any(|s| season_of_record(s) == (old, old_start)) {
                        let how = if season == old + 1 { "season end" } else { "fresh start" };
                        let rec = json!({
                            "season": old, "started_tick": old_start, "ended_tick": last["tick"],
                            "ended_at_ms": now, "how": how, "rows": last["rows"],
                        });
                        h.seasons.push(rec);
                        let n = h.seasons.len();
                        if n > KEEP_SEASONS {
                            h.seasons.drain(0..n - KEEP_SEASONS);
                        }
                        filed = true;
                    }
                }
                h.points = Points {
                    season,
                    season_start: start,
                    points: vec![],
                };
            }
            let due = h.points.points.last().is_none_or(|p| now.saturating_sub(p.at_ms) >= POINT_EVERY_MS);
            if due {
                let rows = board["rows"]
                    .as_array()
                    .map(|rs| {
                        rs.iter()
                            .filter_map(|r| {
                                let n = r["name"].as_str()?.to_string();
                                let v = |k: &str| r[k].as_i64().unwrap_or(0);
                                Some((n, [v("earned"), v("ap_spent"), v("trade_profit")]))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                h.points.points.push(Point {
                    tick: board["tick"].as_u64().unwrap_or(0),
                    at_ms: now,
                    rows,
                });
                let n = h.points.points.len();
                if n > KEEP_POINTS {
                    h.points.points.drain(0..n - KEEP_POINTS);
                }
            }
            h.last = Some(board);
            (filed, due)
        };
        if filed {
            self.save_seasons().await;
        }
        if pointed || filed {
            self.save_points().await;
        }
        self.save_last().await;
    }

    /// Sample the scoreboard every few minutes of wall-clock time.
    pub fn start_history(&self) {
        let g = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(SAMPLE_EVERY * 60));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                iv.tick().await;
                g.sample_history().await;
            }
        });
    }

    /// This season's trend: names once, then per point the tick and each captain's
    /// [earned, action points spent, trading profit] (null where a captain had not flown yet).
    pub fn public_history(&self) -> Value {
        let h = self.history.lock().unwrap_or_else(|p| p.into_inner());
        let mut names: Vec<&String> = h.points.points.iter().flat_map(|p| p.rows.keys()).collect();
        names.sort();
        names.dedup();
        let points: Vec<Value> = h
            .points
            .points
            .iter()
            .map(|p| json!({"tick": p.tick, "at_ms": p.at_ms, "v": names.iter().map(|n| p.rows.get(*n)).collect::<Vec<_>>()}))
            .collect();
        json!({"season": h.points.season, "season_start": h.points.season_start, "names": names, "points": points})
    }

    /// Finished seasons' final scoreboards, newest first.
    pub fn public_seasons(&self) -> Value {
        let h = self.history.lock().unwrap_or_else(|p| p.into_inner());
        json!({"seasons": h.seasons.iter().rev().collect::<Vec<_>>()})
    }
}

fn season_of_record(s: &Value) -> (u32, Tick) {
    (s["season"].as_u64().unwrap_or(0) as u32, s["started_tick"].as_u64().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn season_keys_read_from_board_and_record() {
        let board = json!({"season": {"number": 2, "started_tick": 3630}, "tick": 4000, "rows": []});
        assert_eq!(season_of(&board), (2, 3630));
        let rec = json!({"season": 1, "started_tick": 1586});
        assert_eq!(season_of_record(&rec), (1, 1586));
    }
}
