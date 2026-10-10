//! Things worth fighting over, and things worth hiding.
//!
//! - Concessions: a few rich frontier sectors far from FedSpace. Whoever's
//!   fighters hold one (at least `concession_min_fighters`) collects a yield into
//!   their bank one full day after taking it and every day after; control changes
//!   are headline news.
//! - Black markets: a few frontier ports sell contraband; FedSpace ports buy it
//!   dear, but Federation customs scan ships entering FedSpace.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cmd::TradeAction;
use crate::cmd::TradeArgs;
use crate::engine::{Ctx, Outcome, Refusal, Res};
use crate::model::*;

const NAMES: [&str; 16] = [
    "Dilithium Rift",
    "Helix Vein",
    "Old Relay",
    "Quasar Spring",
    "Ghost Dock",
    "Iron Choir",
    "Saffron Belt",
    "Verdant Shoal",
    "Glass Reef",
    "Cinder Vault",
    "Tidewater Gate",
    "Hollow Crown",
    "Amber Drift",
    "Silent Forge",
    "Lantern Deep",
    "Obsidian Well",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Concession {
    pub id: u64,
    pub name: String,
    pub sector: SectorId,
    pub holder: Option<PlayerId>,
    pub held_since: Tick,
    pub paid_total: i64,
}

/// Warps from Stardock to every sector (u32::MAX if unreachable).
fn hops_from_stardock(w: &World) -> Vec<u32> {
    let n = w.sectors.len();
    let mut d = vec![u32::MAX; n];
    if n == 0 {
        return d;
    }
    d[0] = 0;
    let mut q = VecDeque::from([0usize]);
    while let Some(u) = q.pop_front() {
        for &t in &w.sectors[u].warps {
            let v = t as usize - 1;
            if v < n && d[v] == u32::MAX {
                d[v] = d[u] + 1;
                q.push_back(v);
            }
        }
    }
    d
}

/// Pick `count` sectors from `pool` (sorted best-first), each at least `gap` units from the others.
fn spread(w: &World, pool: &[usize], count: usize) -> Vec<usize> {
    let span = w.sectors.iter().map(|s| s.x.abs().max(s.y.abs())).fold(1.0, f64::max);
    let gap2 = (span * 0.35).powi(2);
    let mut picked: Vec<usize> = vec![];
    for &i in pool {
        if picked.len() >= count {
            break;
        }
        let (x, y) = (w.sectors[i].x, w.sectors[i].y);
        if picked
            .iter()
            .all(|&j| (w.sectors[j].x - x).powi(2) + (w.sectors[j].y - y).powi(2) >= gap2)
        {
            picked.push(i);
        }
    }
    // Small galaxies: relax the spacing rather than place too few.
    for &i in pool {
        if picked.len() >= count {
            break;
        }
        if !picked.contains(&i) {
            picked.push(i);
        }
    }
    picked
}

/// Place concessions and black markets (deterministic; idempotent: skips what already exists).
pub fn place(w: &mut World) -> (usize, usize) {
    let d = hops_from_stardock(w);
    let n = w.sectors.len();
    let mut far: Vec<usize> = (0..n).filter(|&i| !w.sectors[i].fedspace && d[i] != u32::MAX).collect();
    far.sort_by_key(|&i| (std::cmp::Reverse(d[i]), i));
    let mut placed = (0, 0);
    if w.concessions.is_empty() {
        let want = ((n as u32 * w.rules.concessions_per_100_sectors / 100) as usize)
            .max(3)
            .min(NAMES.len());
        let pool: Vec<usize> = far
            .iter()
            .copied()
            .filter(|&i| w.sectors[i].planets.is_empty() && w.sectors[i].warps.len() >= 2 && !w.is_fed_buffer(i as SectorId + 1))
            .collect();
        for (k, i) in spread(w, &pool, want).into_iter().enumerate() {
            w.next_registry_id = w.next_registry_id.max(1);
            let id = w.next_registry_id;
            w.next_registry_id += 1;
            w.concessions.insert(
                id,
                Concession {
                    id,
                    name: NAMES[k % NAMES.len()].to_string(),
                    sector: i as SectorId + 1,
                    holder: None,
                    held_since: w.tick,
                    paid_total: 0,
                },
            );
            placed.0 += 1;
        }
    }
    if !w.sectors.iter().any(|s| s.port.as_ref().is_some_and(|p| p.black_market)) {
        let want = ((n as u32 * w.rules.black_markets_per_100_sectors / 100) as usize).max(2);
        let pool: Vec<usize> = far
            .iter()
            .copied()
            .filter(|&i| w.sectors[i].port.as_ref().is_some_and(|p| !p.stardock))
            .collect();
        for i in spread(w, &pool, want) {
            if let Some(p) = w.sectors[i].port.as_mut() {
                p.black_market = true;
                placed.1 += 1;
            }
        }
    }
    placed
}

/// Concessions and black markets for a frontier ring just added to a live galaxy (sectors from
/// index `n0` on), at the same density per sector as genesis (rounded to nearest): concessions
/// in the ring's farthest open sectors (no planets, at least two lanes), each under a name not
/// yet in use, and black markets at its farthest ports. Returns the new concessions' names and
/// how many black markets opened.
pub fn place_region(w: &mut World, n0: usize) -> (Vec<String>, usize) {
    let n = w.sectors.len();
    if n <= n0 {
        return (vec![], 0);
    }
    let added = (n - n0) as u32;
    let d = hops_from_stardock(w);
    let mut far: Vec<usize> = (n0..n).filter(|&i| d[i] != u32::MAX).collect();
    far.sort_by_key(|&i| (std::cmp::Reverse(d[i]), i));
    let used: std::collections::BTreeSet<String> = w.concessions.values().map(|c| c.name.clone()).collect();
    let mut names: Vec<String> = NAMES.iter().map(|s| s.to_string()).filter(|s| !used.contains(s)).collect();
    names.extend(NAMES.iter().map(|s| format!("{s} II")).filter(|s| !used.contains(s)));
    let want = ((added * w.rules.concessions_per_100_sectors + 50) / 100) as usize;
    let pool: Vec<usize> = far
        .iter()
        .copied()
        .filter(|&i| w.sectors[i].planets.is_empty() && w.sectors[i].warps.len() >= 2 && !w.is_fed_buffer(i as SectorId + 1))
        .collect();
    let mut placed = vec![];
    for (i, name) in spread(w, &pool, want.min(names.len())).into_iter().zip(names) {
        w.next_registry_id = w.next_registry_id.max(1);
        let id = w.next_registry_id;
        w.next_registry_id += 1;
        w.concessions.insert(
            id,
            Concession {
                id,
                name: name.clone(),
                sector: i as SectorId + 1,
                holder: None,
                held_since: w.tick,
                paid_total: 0,
            },
        );
        placed.push(name);
    }
    let want = ((added * w.rules.black_markets_per_100_sectors + 50) / 100) as usize;
    let pool: Vec<usize> = far
        .iter()
        .copied()
        .filter(|&i| w.sectors[i].port.as_ref().is_some_and(|p| !p.stardock && !p.black_market))
        .collect();
    let mut markets = 0;
    for i in spread(w, &pool, want) {
        if let Some(p) = w.sectors[i].port.as_mut() {
            p.black_market = true;
            markets += 1;
        }
    }
    (placed, markets)
}

/// Control and yield of concessions; FedSpace contraband demand.
pub fn tick(ctx: &mut Ctx) {
    let t = ctx.w.tick;
    let rules = ctx.w.rules.clone();
    if t.is_multiple_of(OWNERSHIP_CHECK_TICKS) {
        let ids: Vec<u64> = ctx.w.concessions.keys().copied().collect();
        for id in ids {
            let c = ctx.w.concessions[&id].clone();
            let holder = ctx
                .w
                .sector(c.sector)
                .and_then(|s| s.fighters.as_ref())
                .filter(|f| f.count >= rules.concession_min_fighters)
                .map(|f| f.owner);
            if holder != c.holder {
                let cm = ctx.w.concessions.get_mut(&id).unwrap();
                cm.holder = holder;
                cm.held_since = t;
                let text = match holder {
                    Some(h) => format!("CONCESSION: {} seized {} (sector {}).", ctx.w.player_name(h), c.name, c.sector),
                    None => format!("CONCESSION: {} (sector {}) lies unclaimed.", c.name, c.sector),
                };
                ctx.news(text.clone());
                ctx.event(
                    "concession",
                    Some(c.sector),
                    holder.into_iter().collect(),
                    true,
                    text,
                    json!({"concession": id}),
                );
            }
        }
    }
    // Paid on each holder's own anniversaries: one full day after it took the concession, then
    // every day it keeps holding it (engine 1.27; before, only at fixed day boundaries, which could
    // delay a first payout by almost two days). Losing it, or dropping below the fighters, restarts
    // the clock, so there is no payday sniping.
    let day = crate::player::ticks_per_day(&rules).max(1) as u64;
    // Territories, computed at most once this tick and only when a payout needs them.
    let mut territories: Option<BTreeMap<CorpId, crate::territory::Territory>> = None;
    {
        let paying: Vec<(u64, PlayerId)> = ctx
            .w
            .concessions
            .values()
            .filter(|c| t > c.held_since && (t - c.held_since).is_multiple_of(day))
            .filter_map(|c| c.holder.map(|h| (c.id, h)))
            .collect();
        for (id, h) in paying {
            // A shared stack's yield is split by share (engine 1.32); the holder keeps the remainder.
            let sector = ctx.w.concessions[&id].sector;
            let parts = match ctx.w.sector(sector).and_then(|s| s.fighters.as_ref()).filter(|f| f.owner == h) {
                Some(f) => f.split(rules.concession_yield),
                None => vec![(h, rules.concession_yield)],
            };
            for (holder, part) in parts {
                if let Some(p) = ctx.w.players.get_mut(&holder) {
                    p.bank += part;
                    p.stats.concession_income += part;
                }
            }
            ctx.w.concessions.get_mut(&id).unwrap().paid_total += rules.concession_yield;
            territory_bonus(ctx, id, h, &mut territories);
        }
    }
    if t.is_multiple_of(rules.planet_day_ticks.max(1)) {
        for s in ctx.w.sectors.iter_mut().filter(|s| s.fedspace) {
            if let Some(p) = s.port.as_mut() {
                p.contraband_demand = rules.contraband_demand_per_day;
            }
        }
    }
}

/// The connected-concession bonus (engine 1.40): a concession a member holds inside its
/// corporation's connected territory pays `territory_concession_bonus_bp` of its yield more into
/// the corporation's free funds, recorded in its ledger.
fn territory_bonus(ctx: &mut Ctx, id: u64, holder: PlayerId, territories: &mut Option<BTreeMap<CorpId, crate::territory::Territory>>) {
    let bonus = ctx.w.rules.concession_yield * ctx.w.rules.territory_concession_bonus_bp.max(0) / 10_000;
    let Some(corp) = ctx
        .w
        .players
        .get(&holder)
        .and_then(|p| p.corp)
        .filter(|c| ctx.w.corps.contains_key(c))
    else {
        return;
    };
    if bonus <= 0 {
        return;
    }
    let c = &ctx.w.concessions[&id];
    let (name, sector) = (c.name.clone(), c.sector);
    let inside = territories
        .get_or_insert_with(|| crate::territory::all(ctx.w))
        .get(&corp)
        .is_some_and(|t| t.contains(sector));
    if !inside {
        return;
    }
    let cm = ctx.w.corps.get_mut(&corp).unwrap();
    cm.treasury += bonus;
    cm.record.concession_bonus_total += bonus;
    let who = ctx.w.player_name(holder);
    crate::government::record_executive(
        ctx,
        corp,
        "concession_bonus",
        format!("The concession {name}, held by {who} in connected territory, paid a bonus of {bonus} credits into the free funds."),
        SYSOP,
        bonus,
    );
}

/// Concessions held by a pilot (for the influence score).
pub fn held_by(w: &World, pid: PlayerId) -> i64 {
    w.concessions.values().filter(|c| c.holder == Some(pid)).count() as i64
}

/// How often concession ownership is checked (the tick sweep above). A short dip below the
/// fighters is forgiven if it is topped up before the next check.
pub const OWNERSHIP_CHECK_TICKS: u64 = 10;

/// The first tick at which ownership is next checked.
pub fn next_ownership_check(t: Tick) -> Tick {
    (t / OWNERSHIP_CHECK_TICKS + 1) * OWNERSHIP_CHECK_TICKS
}

/// What a deployment in a concession sector means for its holder, for the deploy reply.
pub fn deploy_note(w: &World, sector: SectorId, owner: PlayerId, count: i64) -> Option<String> {
    let c = w.concessions.values().find(|c| c.sector == sector)?;
    let min = w.rules.concession_min_fighters;
    let yield_ = w.rules.concession_yield;
    let day = crate::player::ticks_per_day(&w.rules).max(1) as u64;
    if c.holder == Some(owner) {
        return Some(format!(" This holds the concession {} ({count} fighters; {min}+ needed).", c.name));
    }
    if count < min {
        return Some(format!(
            " This is the concession {}: holding it needs {min}+ fighters here (you have {count}).",
            c.name
        ));
    }
    let at = next_ownership_check(w.tick);
    Some(format!(
        " This is the concession {}: ownership is checked every {OWNERSHIP_CHECK_TICKS} ticks, so you become its holder at tick {at} if {min}+ fighters remain. It pays {yield_} credits into your bank one full day after that (tick {}), then every day you hold it.",
        c.name,
        at + day
    ))
}

/// When the holder is next paid (one full day after taking it, then daily), or None if unheld.
pub fn next_payout(w: &World, c: &Concession) -> Option<Tick> {
    c.holder?;
    let day = crate::player::ticks_per_day(&w.rules).max(1) as u64;
    let held = w.tick.saturating_sub(c.held_since);
    Some(c.held_since + (held / day + 1) * day)
}

pub fn concessions_json(w: &World) -> Vec<Value> {
    w.concessions
        .values()
        .map(|c| {
            json!({"id": c.id, "name": c.name, "sector": c.sector, "holder": c.holder.map(|h| w.player_name(h)),
                   "holder_corp": c.holder.and_then(|h| crate::diplomacy::corp_of(w, h)).and_then(|cid| w.corps.get(&cid)).map(|c| c.name.clone()),
                   "held_since": c.held_since, "next_payout_tick": next_payout(w, c), "yield_per_day": w.rules.concession_yield, "paid_total": c.paid_total,
                   "needs_fighters": w.rules.concession_min_fighters,
                   "how_to_hold": format!("Keep {}+ of your fighters in its sector. Ownership is checked every {OWNERSHIP_CHECK_TICKS} ticks (next at tick {}); it pays one full day after you become the holder, then every day you hold it.", w.rules.concession_min_fighters, next_ownership_check(w.tick))})
        })
        .collect()
}

/// trade_port with contraband=true: buy at a black market, sell at a FedSpace port.
pub fn smuggle(ctx: &mut Ctx, pid: PlayerId, a: &TradeArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    let sector = ctx
        .w
        .sector(p.sector)
        .cloned()
        .ok_or_else(|| Refusal::new("no_port", "No port here."))?;
    let port = sector
        .port
        .clone()
        .ok_or_else(|| Refusal::new("no_port", "There is no port in this sector."))?;
    let ap = rules.ap_trade;
    match a.action {
        TradeAction::Buy => {
            if !port.black_market {
                return Err(Refusal::new("no_black_market", "This port does not deal in contraband.")
                    .hint("Black markets hide in far frontier ports; scan and explore."));
            }
            let price = rules.contraband_buy_price;
            let n = a
                .quantity
                .unwrap_or(i64::MAX)
                .min(p.ship.free_holds())
                .min(p.credits / price.max(1));
            if n <= 0 {
                return Err(Refusal::new("cannot_buy", "No free holds or not enough credits."));
            }
            if a.dry_run {
                return Ok(ctx.finish(
                    format!("Quote: {n} contraband for {} credits.", n * price),
                    json!({"dry_run": true, "units": n, "price": price}),
                ));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            let pm = ctx.player_mut(pid);
            pm.credits -= n * price;
            pm.ship.contraband += n;
            pm.stats.smuggling_profit -= n * price;
            Ok(ctx.finish(
                format!(
                    "Loaded {n} units of contraband for {} credits. Federation customs scan ships entering FedSpace.",
                    n * price
                ),
                json!({"units": n, "price": price, "contraband_aboard": n + p.ship.contraband}),
            ))
        }
        TradeAction::Sell => {
            if !sector.fedspace {
                return Err(Refusal::new(
                    "no_buyer",
                    "Contraband sells only at FedSpace ports (sectors 1-10), past the customs scan.",
                ));
            }
            let price = rules.contraband_sell_price;
            let n = a.quantity.unwrap_or(i64::MAX).min(p.ship.contraband).min(port.contraband_demand);
            if n <= 0 {
                return Err(Refusal::new(
                    "cannot_sell",
                    "You carry no contraband, or this port's appetite is spent for today.",
                ));
            }
            if a.dry_run {
                return Ok(ctx.finish(
                    format!("Quote: {n} contraband for {} credits.", n * price),
                    json!({"dry_run": true, "units": n, "price": price}),
                ));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            ctx.w.sector_mut(p.sector).unwrap().port.as_mut().unwrap().contraband_demand -= n;
            let pm = ctx.player_mut(pid);
            pm.credits += n * price;
            pm.ship.contraband -= n;
            pm.stats.smuggling_profit += n * price;
            Ok(ctx.finish(
                format!("Sold {n} units of contraband for {} credits.", n * price),
                json!({"units": n, "price": price}),
            ))
        }
    }
}

/// Federation customs on entering FedSpace with contraband aboard.
pub fn customs(ctx: &mut Ctx, pid: PlayerId, sid: SectorId) -> Option<String> {
    let p = ctx.w.players.get(&pid)?;
    if p.ship.contraband <= 0 || p.npc.is_some() {
        return None;
    }
    let rules = ctx.w.rules.clone();
    let chance = if p.profession == Some(crate::careers::Career::Smuggler) {
        rules.customs_scan_bp / 2
    } else {
        rules.customs_scan_bp
    };
    if ctx.rng.range(0, 9_999) >= chance {
        return None;
    }
    let units = p.ship.contraband;
    let fine = (units * rules.customs_fine_per_unit).min(p.credits);
    let name = p.name.clone();
    let pm = ctx.player_mut(pid);
    pm.ship.contraband = 0;
    pm.credits -= fine;
    pm.alignment -= 20;
    let text =
        format!("CUSTOMS: Federation inspectors seized {units} units of contraband from {name} in sector {sid} and fined {fine} credits.");
    ctx.news(text.clone());
    ctx.event("customs", Some(sid), vec![pid], true, text.clone(), json!({}));
    Some(text)
}
