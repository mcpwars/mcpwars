//! The public fleet, an hour late. Once a minute the server records where every active captain
//! is (`af_core::public::fleet_frame`) and keeps a little over two hours of frames; the home page
//! is served the newest frame at least an hour old (`af_core::public::fleet_view`). The frames sit
//! beside the universe in the meta table under `console:public:fleet`, saved every few minutes and before
//! a shutdown or handover, so a deploy does not blank the map for an hour. Never journaled, never
//! replayed, not exported.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use af_core::public::{FLEET_DELAY_MS, FleetFrame, fleet_frame, fleet_view};
use serde_json::{Value, json};

use crate::game::{Game, now_ms};

/// Frames kept: two hours (an hour's delay, then an hour back for the jump count) and a margin.
pub const KEEP: usize = 130;
const META_KEY: &str = "public:fleet";
/// Save the frames every this many minutes.
const SAVE_EVERY: u64 = 5;

pub type FleetRing = Arc<Mutex<VecDeque<FleetFrame>>>;

pub fn ring() -> FleetRing {
    Arc::new(Mutex::new(VecDeque::new()))
}

impl Game {
    /// Restore the frames saved by the previous server (dropping any too old to matter).
    pub async fn load_fleet(&self) {
        let Some(v) = self.console_get(META_KEY).await else { return };
        let Ok(frames) = serde_json::from_value::<Vec<FleetFrame>>(v) else {
            tracing::warn!("public fleet: saved frames unreadable, starting empty");
            return;
        };
        let cutoff = now_ms().saturating_sub(KEEP as u64 * 60_000);
        let mut r = self.fleet.lock().unwrap_or_else(|p| p.into_inner());
        r.clear();
        r.extend(frames.into_iter().filter(|f| f.at_ms >= cutoff));
        tracing::info!("public fleet: restored {} frames", r.len());
    }

    /// Save the frames beside the universe (refused, harmlessly, once this server has handed over).
    pub async fn save_fleet(&self) {
        let frames: Vec<FleetFrame> = self.fleet.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect();
        if !self.console_put(META_KEY, &json!(frames)).await {
            tracing::debug!("public fleet: frames not saved");
        }
    }

    /// Record the fleet now. A new galaxy (Big Bang or fresh start) clears the older frames.
    pub async fn record_fleet(&self) {
        let now = now_ms();
        let f = self.read(move |w| fleet_frame(w, now)).await;
        let mut r = self.fleet.lock().unwrap_or_else(|p| p.into_inner());
        if r.back()
            .is_some_and(|last| last.season != f.season || last.season_start != f.season_start)
        {
            r.clear();
        }
        r.push_back(f);
        while r.len() > KEEP {
            r.pop_front();
        }
    }

    /// Record a frame every minute of wall-clock time (paused ticks included) and save every few.
    pub fn start_fleet(&self) {
        let g = self.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_secs(60));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut n: u64 = 0;
            loop {
                iv.tick().await;
                g.record_fleet().await;
                n += 1;
                if n.is_multiple_of(SAVE_EVERY) {
                    g.save_fleet().await;
                }
            }
        });
    }

    /// The public fleet as of an hour ago, plus the live season clock. `warming` until the
    /// server has an hour of frames.
    pub async fn public_fleet(&self) -> Value {
        let frames: Vec<FleetFrame> = self.fleet.lock().unwrap_or_else(|p| p.into_inner()).iter().cloned().collect();
        let now = now_ms();
        let at = frames.iter().rposition(|f| f.at_ms + FLEET_DELAY_MS <= now);
        self.read(move |w| {
            let mut v = match at {
                Some(i) => fleet_view(w, &frames, i),
                None => json!({"warming": true, "captains": 0, "ships": [], "jumps_hour": null}),
            };
            v["delay_ms"] = json!(FLEET_DELAY_MS);
            v["now_ms"] = json!(now);
            v["tick"] = json!(w.tick);
            v["tick_ms"] = json!(w.rules.tick_ms);
            v["season"] = json!({
                "number": w.season.number, "started_tick": w.season.started_tick,
                "ends_tick": w.season.ends_tick, "finished": w.season.finished,
            });
            v
        })
        .await
    }
}
