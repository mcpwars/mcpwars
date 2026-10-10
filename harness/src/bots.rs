//! Scripted (non-LLM) bots that play through the *same JSON tool interface*
//! LLM agents use. They exercise the full stack, populate universes, and
//! drive balance simulations without any API key.

use serde_json::{Value, json};

/// Anything that can execute a game tool and return its JSON envelope.
#[allow(async_fn_in_trait)]
pub trait ToolCaller {
    async fn call(&self, name: &str, args: Value) -> (bool, Value);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Merchant,
    Corsair,
    Builder,
    /// A strong AI trader: sells and buys at every port, ranks routes by profit per AP
    /// including the trip to them, spends AP down to the last warp, buys holds, banks.
    Expert,
    /// An expert that moves up to a Cargo Hauler and fills its holds.
    Hauler,
}

impl Style {
    pub fn parse(s: &str) -> Style {
        match s {
            "corsair" => Style::Corsair,
            "builder" | "empire" => Style::Builder,
            "expert" => Style::Expert,
            "hauler" => Style::Hauler,
            _ => Style::Merchant,
        }
    }
}

impl Style {
    /// The career a bot of this style registers.
    pub fn career(self) -> &'static str {
        match self {
            Style::Corsair => "corsair",
            Style::Builder => "builder",
            _ => "merchant",
        }
    }
}

/// AP per warp for a ship class (rules defaults).
fn warp_ap(class: &Value) -> i64 {
    match class.as_str().unwrap_or("") {
        "scout_marauder" => 2,
        "battleship" => 4,
        "cargo_hauler" | "escape_pod" => 6,
        _ => 3,
    }
}

const GOODS: [&str; 3] = ["fuel_ore", "organics", "equipment"];

#[derive(Default, Debug, Clone)]
pub struct BotStats {
    pub calls: u64,
    pub refusals: u64,
}

pub struct Bot {
    pub style: Style,
    pub stats: BotStats,
    /// Rotating exploration target.
    explore_seed: u64,
    phase: u32,
    /// Sectors already checked for a claimable wild planet.
    tried_planets: std::collections::BTreeSet<u64>,
    /// Career to choose on the first turn (None: never choose one).
    pub career: Option<String>,
    career_done: bool,
}

fn r(v: &Value) -> &Value {
    &v["result"]
}

impl Bot {
    pub fn new(style: Style, seed: u64) -> Self {
        Bot {
            style,
            stats: BotStats::default(),
            explore_seed: seed.wrapping_mul(2654435761) | 1,
            phase: 0,
            tried_planets: Default::default(),
            career: None,
            career_done: false,
        }
    }

    fn next_rand(&mut self) -> u64 {
        self.explore_seed ^= self.explore_seed << 13;
        self.explore_seed ^= self.explore_seed >> 7;
        self.explore_seed ^= self.explore_seed << 17;
        self.explore_seed
    }

    async fn call<C: ToolCaller>(&mut self, c: &C, name: &str, args: Value) -> (bool, Value) {
        self.stats.calls += 1;
        let trace = std::env::var("AF_BOT_TRACE").ok().is_some_and(|t| t == format!("{:?}", self.style));
        let (ok, v) = c.call(name, args.clone()).await;
        if !ok {
            self.stats.refusals += 1;
        }
        if trace {
            let detail = if ok {
                v["summary"].to_string()
            } else {
                v["error"]["message"].to_string()
            };
            eprintln!(
                "[{:?}] {name} {args} -> {} {}",
                self.style,
                if ok { "ok" } else { "REFUSED" },
                detail.chars().take(140).collect::<String>()
            );
        }
        (ok, v)
    }

    /// Play one "turn": a bounded number of actions until AP runs low.
    pub async fn turn<C: ToolCaller>(&mut self, c: &C, max_actions: usize) {
        if !self.career_done {
            self.career_done = true;
            if let Some(career) = self.career.clone() {
                self.call(c, "career", json!({"action": "choose", "career": career})).await;
            }
        }
        let expert = matches!(self.style, Style::Expert | Style::Hauler);
        for _ in 0..max_actions {
            let (_, b) = self.call(c, "get_briefing", json!({})).await;
            let st = &r(&b)["status"];
            let ap = st["ap"].as_i64().unwrap_or(0);
            // Naive bots stop below 8 AP; experts spend down to the last trade.
            if ap < if expert { 1 } else { 8 } {
                return;
            }
            let acted = match self.style {
                Style::Merchant => self.merchant_step(c, &b).await,
                Style::Corsair => self.corsair_step(c, &b).await,
                Style::Builder => self.builder_step(c, &b).await,
                Style::Expert | Style::Hauler => self.expert_step(c, &b).await,
            };
            if expert {
                if !acted {
                    if ap >= warp_ap(&st["ship"]["class"]) {
                        self.explore(c, &b).await;
                    } else {
                        return;
                    }
                }
                continue;
            }
            if !acted {
                self.explore(c, &b).await;
            }
        }
    }

    async fn at_stardock<C: ToolCaller>(&mut self, c: &C, b: &Value) -> bool {
        let sec = r(b)["status"]["sector"].as_u64().unwrap_or(0);
        if sec != 1 {
            let (ok, _) = self.call(c, "autopilot_travel", json!({"destination_sector": 1})).await;
            return ok;
        }
        true
    }

    /// Shared housekeeping: escape pods, banking, upgrades.
    async fn housekeeping<C: ToolCaller>(&mut self, c: &C, b: &Value) -> bool {
        let st = &r(b)["status"];
        let credits = st["credits"].as_i64().unwrap_or(0);
        let bank = st["bank"].as_i64().unwrap_or(0);
        if st["ship"]["class"] == "escape_pod" {
            if self.at_stardock(c, b).await {
                if credits + bank < 8_000 {
                    self.call(c, "request_relief", json!({})).await;
                } else {
                    self.call(c, "bank", json!({"action": "withdraw"})).await;
                    let (ok, _) = self.call(c, "buy_ship", json!({"ship_class": "merchant_cruiser"})).await;
                    if !ok {
                        self.call(c, "buy_ship", json!({"ship_class": "scout_marauder"})).await;
                    }
                }
            }
            return true;
        }
        let sector = st["sector"].as_u64().unwrap_or(0);
        if sector == 1 {
            if credits > 30_000 {
                let holds = st["ship"]["holds"].as_i64().unwrap_or(0);
                if holds < 75 && self.style != Style::Corsair {
                    self.call(c, "buy_equipment", json!({"item": "holds", "quantity": 5})).await;
                }
                let fighters = st["ship"]["fighters"].as_i64().unwrap_or(0);
                if fighters < 150 {
                    self.call(c, "buy_equipment", json!({"item": "fighters", "quantity": 100})).await;
                }
            }
            if credits > 60_000 && self.style == Style::Merchant {
                self.call(c, "bank", json!({"action": "deposit", "amount": credits - 25_000})).await;
            }
        }
        false
    }

    async fn merchant_step<C: ToolCaller>(&mut self, c: &C, b: &Value) -> bool {
        if self.housekeeping(c, b).await {
            return true;
        }
        let st = &r(b)["status"];
        let here = st["sector"].as_u64().unwrap_or(0);
        let cargo = &st["ship"]["cargo"];
        let carrying = ["fuel_ore", "organics", "equipment"]
            .iter()
            .find(|k| cargo[**k].as_i64().unwrap_or(0) > 0)
            .map(|s| s.to_string());
        let port = r(b)["sector"]["port"].clone();
        // Occasionally explore to discover better markets.
        self.phase += 1;
        if self.phase.is_multiple_of(9) {
            return false;
        }
        let (_, tr) = self.call(c, "find_trade_routes", json!({"limit": 20, "max_hops": 6})).await;
        let routes = r(&tr)["routes"].as_array().cloned().unwrap_or_default();
        if let Some(com) = carrying {
            if port["goods"][&com]["port"] == "buys" && port["goods"][&com]["units"].as_i64().unwrap_or(0) > 5 {
                let (ok, _) = self.call(c, "trade_port", json!({"commodity": com, "action": "sell"})).await;
                if ok {
                    // Back-haul only along a known route starting here.
                    if let Some(rt) = routes.iter().find(|rt| rt["buy_at"]["sector"].as_u64() == Some(here)) {
                        let (bok, _) = self
                            .call(c, "trade_port", json!({"commodity": rt["commodity"], "action": "buy"}))
                            .await;
                        if bok {
                            self.call(c, "autopilot_travel", json!({"destination_sector": rt["sell_at"]["sector"]}))
                                .await;
                        }
                    }
                    return true;
                }
            }
            let (_, lp) = self
                .call(c, "list_ports", json!({"commodity": com, "side": "buys", "limit": 5}))
                .await;
            let dest = r(&lp)["ports"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .filter(|p| p["sector"].as_u64() != Some(here) && p["hazard"] != true)
                        .find(|p| p["port"]["goods"][&com]["fill_pct"].as_i64().unwrap_or(0) > 10)
                })
                .and_then(|p| p["sector"].as_u64());
            if let Some(dest) = dest {
                let (ok, _) = self
                    .call(c, "autopilot_travel", json!({"destination_sector": dest, "avoid_hazards": true}))
                    .await;
                return ok;
            }
            return false;
        }
        // Empty holds: choose the route with the best profit per AP including the trip to it.
        let best = routes
            .iter()
            .filter(|rt| rt["buy_at"]["stock_pct"].as_i64().unwrap_or(0) > 15 && rt["sell_at"]["demand_pct"].as_i64().unwrap_or(0) > 15)
            .max_by_key(|rt| {
                let p = rt["est_profit_one_way"].as_i64().unwrap_or(0);
                let ap = rt["hops_between"].as_i64().unwrap_or(1) + rt["hops_from_you"].as_i64().unwrap_or(10) + 2;
                p * 100 / ap.max(1)
            });
        if let Some(route) = best {
            let buy_at = route["buy_at"]["sector"].as_u64().unwrap_or(0);
            if buy_at == here {
                let (ok, _) = self
                    .call(c, "trade_port", json!({"commodity": route["commodity"], "action": "buy"}))
                    .await;
                if ok {
                    self.call(c, "autopilot_travel", json!({"destination_sector": route["sell_at"]["sector"]}))
                        .await;
                }
                return ok;
            }
            let (ok, _) = self.call(c, "autopilot_travel", json!({"destination_sector": buy_at})).await;
            return ok;
        }
        false
    }

    /// Autopilot, and if every charted route crosses a hazard, take the risk (pay the toll) rather than idle.
    async fn go<C: ToolCaller>(&mut self, c: &C, dest: &Value) -> bool {
        let (ok, _) = self.call(c, "autopilot_travel", json!({"destination_sector": dest})).await;
        if ok {
            return true;
        }
        let (ok, _) = self
            .call(c, "autopilot_travel", json!({"destination_sector": dest, "avoid_hazards": false}))
            .await;
        ok
    }

    /// Expert housekeeping: holds, a Cargo Hauler for the hauler style, banking. True if it used the step.
    async fn expert_housekeeping<C: ToolCaller>(&mut self, c: &C, b: &Value) -> bool {
        let st = &r(b)["status"];
        if st["ship"]["class"] == "escape_pod" {
            return self.housekeeping(c, b).await;
        }
        let credits = st["credits"].as_i64().unwrap_or(0);
        let holds = st["ship"]["holds"].as_i64().unwrap_or(0);
        let class = st["ship"]["class"].as_str().unwrap_or("");
        let max_holds = if class == "cargo_hauler" { 250 } else { 75 };
        let want_hauler = self.style == Style::Hauler && class != "cargo_hauler" && credits > 120_000;
        // A hold pays for itself within hours: buy them as soon as there is a float left to fill them.
        let want_holds = holds < max_holds && credits > 8_000 + holds * 100 + 5 * (250 + 20 * holds);
        let want_bank = credits > 150_000;
        // Skarn only ambush ships carrying under 80% of their fighters, and grow with the season
        // (up to 20 x 4 x day + 30): stay above the strongest raider of the day.
        let left = st["season_ticks_left"].as_i64().unwrap_or(43_200);
        let day = (43_200 - left).max(0) / 1_440 + 2;
        // Never more than the hull can carry (fighters_max + shields_max).
        let hull_cap = match class {
            "cargo_hauler" => 700,
            "scout_marauder" => 350,
            _ => 2_900,
        };
        let guard = ((20 * 4 * day + 30) * 8 / 10 + 50).min(2_400).min(hull_cap);
        let fighters = st["ship"]["fighters"].as_i64().unwrap_or(0);
        let shields = st["ship"]["shields"].as_i64().unwrap_or(0);
        let want_arms = fighters + shields < guard && credits > 20_000 + (guard - fighters - shields) * 60;
        if !(want_hauler || want_holds || want_bank || want_arms) {
            return false;
        }
        let sector = st["sector"].as_u64().unwrap_or(0);
        if sector != 1 {
            if st["ap"].as_i64().unwrap_or(0) < warp_ap(&st["ship"]["class"]) {
                return false;
            }
            return self.go(c, &json!(1)).await;
        }
        if want_hauler && st["bank"].as_i64().unwrap_or(0) > 0 {
            self.call(c, "bank", json!({"action": "withdraw"})).await;
        }
        if want_hauler {
            self.call(c, "buy_ship", json!({"ship_class": "cargo_hauler"})).await;
        }
        // Holds, keeping a trading float.
        let (_, s2) = self.call(c, "get_status", json!({})).await;
        let cr = r(&s2)["credits"].as_i64().unwrap_or(0);
        let h = r(&s2)["ship"]["holds"]["total"].as_i64().unwrap_or(holds);
        let hm = r(&s2)["ship"]["holds"]["max"].as_i64().unwrap_or(max_holds);
        let mut n = 0;
        let mut cost = 0;
        while h + n < hm && cost + 250 + 20 * (h + n) <= cr - 4_000 - (h + n) * 100 {
            cost += 250 + 20 * (h + n);
            n += 1;
        }
        if n > 0 {
            self.call(c, "buy_equipment", json!({"item": "holds", "quantity": n})).await;
        }
        if want_arms {
            self.call(c, "buy_equipment", json!({"item": "shields"})).await;
            let (_, s4) = self.call(c, "get_status", json!({})).await;
            let have = r(&s4)["ship"]["fighters"].as_i64().unwrap_or(0) + r(&s4)["ship"]["shields"].as_i64().unwrap_or(0);
            if have < guard {
                self.call(c, "buy_equipment", json!({"item": "fighters", "quantity": guard - have}))
                    .await;
            }
        }
        let (_, s3) = self.call(c, "get_status", json!({})).await;
        let left = r(&s3)["credits"].as_i64().unwrap_or(0);
        if left > 30_000 {
            self.call(c, "bank", json!({"action": "deposit", "amount": left - 30_000})).await;
        }
        true
    }

    /// One expert trading step: sell what this port buys, then take the best route by profit per AP.
    async fn expert_step<C: ToolCaller>(&mut self, c: &C, b: &Value) -> bool {
        if self.expert_housekeeping(c, b).await {
            return true;
        }
        let st = &r(b)["status"];
        let here = st["sector"].as_u64().unwrap_or(0);
        let mut ap = st["ap"].as_i64().unwrap_or(0);
        let wap = warp_ap(&st["ship"]["class"]);
        let holds = st["ship"]["holds"].as_i64().unwrap_or(0);
        let port = r(b)["sector"]["port"].clone();
        let mut cargo: Vec<i64> = GOODS.iter().map(|g| st["ship"]["cargo"][*g].as_i64().unwrap_or(0)).collect();
        let mut acted = false;
        if !port.is_null() {
            for (i, g) in GOODS.iter().enumerate() {
                if ap >= 1 && cargo[i] > 0 && port["goods"][*g]["port"] == "buys" && port["goods"][*g]["units"].as_i64().unwrap_or(0) > 0 {
                    let (ok, v) = self.call(c, "trade_port", json!({"commodity": g, "action": "sell"})).await;
                    if ok {
                        cargo[i] -= r(&v)["trade"]["filled"].as_i64().unwrap_or(cargo[i]);
                        ap -= 1;
                        acted = true;
                    }
                }
            }
        }
        // Still carrying something this port will not take: carry it to the nearest buyer.
        if let Some(i) = (0..3).find(|i| cargo[*i] > 0) {
            if ap < wap {
                return acted;
            }
            // The buyer with the best price for what it can still absorb, per AP of travel.
            let (_, lp) = self
                .call(c, "list_ports", json!({"commodity": GOODS[i], "side": "buys", "limit": 15}))
                .await;
            let dest = r(&lp)["ports"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .filter(|p| p["sector"].as_u64() != Some(here) && p["hazard"] != true && !p["hops"].is_null())
                        .filter(|p| p["port"]["goods"][GOODS[i]]["fill_pct"].as_i64().unwrap_or(0) > 25)
                        .max_by_key(|p| {
                            let g = &p["port"]["goods"][GOODS[i]];
                            g["price"].as_i64().unwrap_or(0) * 100 / (p["hops"].as_i64().unwrap_or(9) * wap + 2)
                        })
                })
                .and_then(|p| p["sector"].as_u64());
            if let Some(d) = dest {
                let ok = self.go(c, &json!(d)).await;
                return ok || acted;
            }
            return acted;
        }
        let (_, tr) = self.call(c, "find_trade_routes", json!({"limit": 20, "max_hops": 8})).await;
        let mut routes = r(&tr)["routes"].as_array().cloned().unwrap_or_default();
        // Too weak for the Skarn of the day: trade inside FedSpace (sectors 1-10), where they never go.
        let left = st["season_ticks_left"].as_i64().unwrap_or(43_200);
        let guard = ((20 * 4 * ((43_200 - left).max(0) / 1_440 + 2) + 30) * 8 / 10 + 50).min(2_400);
        let armed = st["ship"]["fighters"].as_i64().unwrap_or(0) + st["ship"]["shields"].as_i64().unwrap_or(0) >= guard;
        let in_fed =
            |rt: &Value| rt["buy_at"]["sector"].as_u64().unwrap_or(99) <= 10 && rt["sell_at"]["sector"].as_u64().unwrap_or(99) <= 10;
        if !armed && routes.iter().any(in_fed) {
            routes.retain(in_fed);
        }
        let live_units = |g: &Value| port["goods"][g.as_str().unwrap_or("")]["units"].as_i64().unwrap_or(0);
        let best = routes
            .iter()
            .filter(|rt| rt["buy_at"]["stock_pct"].as_i64().unwrap_or(0) > 20 && rt["sell_at"]["demand_pct"].as_i64().unwrap_or(0) > 25)
            .filter(|rt| rt["buy_at"]["sector"].as_u64() != Some(here) || live_units(&rt["commodity"]) > holds / 3)
            .max_by_key(|rt| {
                let back = rt["backhaul"]["margin_per_unit"].as_i64().unwrap_or(0);
                let profit = (rt["margin_per_unit"].as_i64().unwrap_or(0) + back) * holds;
                let approach = rt["hops_from_you"].as_i64().unwrap_or(20) * wap;
                profit * 100 / (approach + rt["cycle_ap"].as_i64().unwrap_or(10)).max(1)
            })
            .cloned();
        let Some(route) = best else { return acted };
        let buy_at = route["buy_at"]["sector"].as_u64().unwrap_or(0);
        if buy_at == here {
            if ap < 1 {
                return acted;
            }
            let (ok, _) = self
                .call(c, "trade_port", json!({"commodity": route["commodity"], "action": "buy"}))
                .await;
            ap -= 1;
            if ok && ap >= wap {
                self.go(c, &route["sell_at"]["sector"]).await;
            }
            return ok || acted;
        }
        if ap < wap {
            return acted;
        }
        let ok = self.go(c, &json!(buy_at)).await;
        ok || acted
    }

    async fn corsair_step<C: ToolCaller>(&mut self, c: &C, b: &Value) -> bool {
        if self.housekeeping(c, b).await {
            return true;
        }
        let st = &r(b)["status"];
        let credits = st["credits"].as_i64().unwrap_or(0);
        let fighters = st["ship"]["fighters"].as_i64().unwrap_or(0);
        // Fund the war chest by trading first.
        if fighters < 400 && credits < 60_000 {
            return self.merchant_step(c, b).await;
        }
        if st["sector"] == 1 && credits > 100_000 && st["ship"]["class"] != "battleship" {
            self.call(c, "buy_ship", json!({"ship_class": "battleship"})).await;
            self.call(c, "buy_equipment", json!({"item": "fighters"})).await;
            return true;
        }
        if fighters < 400 && credits >= 60_000 {
            if self.at_stardock(c, b).await {
                self.call(c, "buy_equipment", json!({"item": "fighters"})).await;
            }
            return true;
        }
        let sector = &r(b)["sector"];
        // Attack a weaker non-friendly ship here.
        if let Some(ships) = sector["ships"].as_array() {
            for s in ships {
                if s["friendly"] == true || s["newbie_protected"] == true || s["npc_role"] == "fed_patrol" {
                    continue;
                }
                let (ok, _) = self.call(c, "attack_target", json!({"target_type": "ship", "target_id": s["player"], "tactic": if s["npc_role"] == "trader" { "board" } else { "destroy" }})).await;
                if ok {
                    return true;
                }
            }
        }
        // Hold choke points with toll fighters.
        if sector["fedspace"] == false
            && sector["fighters"].is_null()
            && sector["warps"].as_array().map(|w| w.len() <= 3).unwrap_or(false)
            && fighters > 300
        {
            self.call(
                c,
                "deploy_defenses",
                json!({"item_type": "fighters", "quantity": 100, "mode": "toll"}),
            )
            .await;
            return true;
        }
        false
    }

    async fn builder_step<C: ToolCaller>(&mut self, c: &C, b: &Value) -> bool {
        if self.housekeeping(c, b).await {
            return true;
        }
        let st = &r(b)["status"];
        let credits = st["credits"].as_i64().unwrap_or(0);
        let planets = st["planets_owned"].as_i64().unwrap_or(0);
        let colonists = st["ship"]["cargo"]["colonists"].as_i64().unwrap_or(0);
        let (_, pv) = self.call(c, "view_planets", json!({})).await;
        let my_planets = r(&pv)["planets"].as_array().cloned().unwrap_or_default();
        if planets == 0 {
            // A wild planet right here is free to claim (1 AP): no Genesis needed.
            let wild = r(b)["sector"]["planets"]
                .as_array()
                .and_then(|ps| ps.iter().find(|p| p["owner"].is_null()).and_then(|p| p["id"].as_u64()));
            let here = r(b)["status"]["sector"].as_u64().unwrap_or(0);
            if let Some(id) = wild {
                let (ok, _) = self.call(c, "claim_planet", json!({"planet_id": id})).await;
                if ok {
                    return true;
                }
            }
            self.tried_planets.insert(here);
            // Head for the nearest charted sector that has planets and hasn't been checked.
            let (_, sc) = self.call(c, "get_starchart", json!({"radius": 4})).await;
            let target = r(&sc)["sectors"].as_object().and_then(|m| {
                m.iter()
                    .filter(|(_, v)| v["planets"].as_i64().unwrap_or(0) > 0 && v["hazard"].is_null() && v["fed"].is_null())
                    .filter_map(|(k, _)| k.parse::<u64>().ok())
                    .find(|s| !self.tried_planets.contains(s))
            });
            if let Some(t) = target {
                let (ok, _) = self.call(c, "autopilot_travel", json!({"destination_sector": t})).await;
                if ok {
                    return true;
                }
                self.tried_planets.insert(t);
            }
            let (_, full) = self.call(c, "get_status", json!({})).await;
            let torpedoes = r(&full)["ship"]["genesis_torpedoes"].as_i64().unwrap_or(0);
            if torpedoes > 0 {
                // Carry the torpedo out of FedSpace to a quiet sector, then fire.
                if r(b)["sector"]["fedspace"] == true || r(b)["sector"]["planets"].as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                    let (_, sc) = self.call(c, "get_starchart", json!({"radius": 4})).await;
                    let target = r(&sc)["sectors"].as_object().and_then(|m| {
                        m.iter()
                            .find(|(k, v)| {
                                k.parse::<u64>().map(|n| n > 10).unwrap_or(false)
                                    && v["fed"].is_null()
                                    && v["hazard"].is_null()
                                    && v["planets"].is_null()
                                    && v["unexplored"].is_null()
                            })
                            .map(|(k, _)| k.parse::<u64>().unwrap())
                    });
                    match target {
                        Some(t) => {
                            self.call(c, "autopilot_travel", json!({"destination_sector": t})).await;
                        }
                        None => self.explore(c, b).await,
                    }
                    return true;
                }
                let (ok, _) = self.call(c, "launch_genesis", json!({})).await;
                if !ok {
                    // e.g. Federation buffer zone or crowded sector: push further out.
                    self.explore(c, b).await;
                }
                return true;
            }
            if credits < 60_000 {
                return self.merchant_step(c, b).await;
            }
            if self.at_stardock(c, b).await {
                self.call(c, "buy_equipment", json!({"item": "genesis_torpedoes", "quantity": 1}))
                    .await;
                self.call(c, "buy_colonists", json!({})).await;
                // Head somewhere quiet in the frontier.
                self.phase = 1;
                for _ in 0..4 {
                    self.explore(c, b).await;
                }
            }
            let (_, g) = self.call(c, "launch_genesis", json!({})).await;
            let _ = g;
            return true;
        }
        let planet = &my_planets[0];
        let pid = planet["id"].as_u64().unwrap_or(0);
        let psector = planet["sector"].as_u64().unwrap_or(0);
        let here = st["sector"].as_u64().unwrap_or(0);
        let next = &planet["next_citadel"];
        let building = !planet["construction"].is_null();
        // What the next citadel level still needs on the planet.
        let missing: Vec<(&str, i64)> = GOODS
            .iter()
            .map(|g| {
                (
                    *g,
                    next["products"][*g].as_i64().unwrap_or(0) - planet["stock"][*g].as_i64().unwrap_or(0),
                )
            })
            .filter(|(_, m)| *m > 0)
            .collect();
        let need_colonists = next["colonists_required"].as_i64().unwrap_or(0) - planet["colonists"]["total"].as_i64().unwrap_or(0);
        let cargo = &st["ship"]["cargo"];
        // 1. Deliver colonists and needed goods we are carrying.
        let carrying_needed: Vec<&str> = missing
            .iter()
            .filter(|(g, _)| cargo[*g].as_i64().unwrap_or(0) > 0)
            .map(|(g, _)| *g)
            .collect();
        if colonists > 0 || !carrying_needed.is_empty() {
            if here != psector {
                let (ok, _) = self.call(c, "autopilot_travel", json!({"destination_sector": psector})).await;
                return ok;
            }
            if colonists > 0 {
                // Settle colonists on the cheapest product this class can make.
                let best = GOODS
                    .iter()
                    .filter(|g| planet["colonists_per_unit_per_planet_day"][**g].as_i64().unwrap_or(0) > 0)
                    .min_by_key(|g| planet["colonists_per_unit_per_planet_day"][**g].as_i64().unwrap_or(i64::MAX))
                    .copied()
                    .unwrap_or("fuel_ore");
                self.call(
                    c,
                    "manage_planet",
                    json!({"planet_id": pid, "action": "deposit_colonists", "commodity": best}),
                )
                .await;
            }
            for g in carrying_needed {
                self.call(
                    c,
                    "manage_planet",
                    json!({"planet_id": pid, "action": "unload_cargo", "commodity": g}),
                )
                .await;
            }
            self.call(c, "manage_planet", json!({"planet_id": pid, "action": "build_citadel"}))
                .await;
            return true;
        }
        if next.is_null() || building {
            // Maxed out or under construction: go earn money.
            return self.merchant_step(c, b).await;
        }
        // 2. Requirements met? Build.
        if missing.is_empty() && need_colonists <= 0 {
            if here != psector {
                let (ok, _) = self.call(c, "autopilot_travel", json!({"destination_sector": psector})).await;
                return ok;
            }
            let (ok, _) = self
                .call(c, "manage_planet", json!({"planet_id": pid, "action": "build_citadel"}))
                .await;
            return ok || self.merchant_step(c, b).await;
        }
        // 3. Fetch colonists from Stardock.
        if need_colonists > 0 {
            if credits < 5_000 {
                return self.merchant_step(c, b).await;
            }
            if self.at_stardock(c, b).await {
                self.call(c, "buy_colonists", json!({})).await;
            }
            return true;
        }
        // 4. Haul the first missing good from the nearest port that sells it.
        let (good, amount) = missing[0];
        let port = &r(b)["sector"]["port"];
        if port["goods"][good]["port"] == "sells" {
            let free = st["ship"]["free_holds"].as_i64().unwrap_or(0);
            let (ok, _) = self
                .call(
                    c,
                    "trade_port",
                    json!({"commodity": good, "action": "buy", "quantity": amount.min(free).max(1)}),
                )
                .await;
            if ok {
                self.call(c, "autopilot_travel", json!({"destination_sector": psector})).await;
                return true;
            }
        }
        let (_, lp) = self
            .call(c, "list_ports", json!({"commodity": good, "side": "sells", "limit": 5}))
            .await;
        let dest = r(&lp)["ports"]
            .as_array()
            .and_then(|a| a.iter().find(|p| p["hazard"] != true && p["sector"].as_u64() != Some(here)))
            .and_then(|p| p["sector"].as_u64());
        match dest {
            Some(d) => {
                let (ok, _) = self.call(c, "autopilot_travel", json!({"destination_sector": d})).await;
                ok
            }
            None => self.merchant_step(c, b).await,
        }
    }

    async fn explore<C: ToolCaller>(&mut self, c: &C, b: &Value) {
        let all: Vec<Value> = r(b)["sector"]["warps"].as_array().cloned().unwrap_or_default();
        let safe: Vec<Value> = all.iter().filter(|w| w["hazard"] != true).cloned().collect();
        // If every lane out is hazardous (e.g. a toll blockade on a dead end), risk it rather than idle forever.
        let warps = if safe.is_empty() { all } else { safe };
        if warps.is_empty() {
            return;
        }
        let unexplored: Vec<&Value> = warps.iter().filter(|w| w["explored"] == false).collect();
        let pick = if !unexplored.is_empty() {
            unexplored[(self.next_rand() as usize) % unexplored.len()]
        } else {
            &warps[(self.next_rand() as usize) % warps.len()]
        };
        if let Some(to) = pick["to"].as_u64() {
            self.call(c, "warp", json!({"destination_sector": to})).await;
        }
    }
}
