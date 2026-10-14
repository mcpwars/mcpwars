//! Movement, scanning and probes.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::cmd::*;
use crate::combat::{self, EntryResult};
use crate::engine::*;
use crate::knowledge;
use crate::model::*;
use crate::path;
use crate::query;

/// Why a pilot may not leave their current sector toward `dest` (None = free to go).
pub fn departure_block(w: &World, pid: PlayerId, dest: SectorId) -> Option<Refusal> {
    let p = w.players.get(&pid)?;
    let here = p.sector;
    if combat::interdicted(w, pid, here) {
        return Some(
            Refusal::new("interdicted", "A hostile Interdictor citadel holds your ship in this sector.")
                .hint("Destroy the planet's fighters (attack_target planet) to break the field."),
        );
    }
    let s = w.sector(here)?;
    if let Some(f) = &s.fighters
        && f.mode == FighterMode::Defensive
        && !crate::diplomacy::at_peace(w, f.owner, pid)
        && f.count > 0
        && dest != p.prev_sector
        && !s.fedspace
    {
        return Some(
            Refusal::new(
                "blocked",
                format!("{} defensive fighters ({}) block onward passage.", f.count, w.player_name(f.owner)),
            )
            .hint(format!(
                "Retreat to sector {} or clear them with attack_target(target_type=fighters).",
                p.prev_sector
            )),
        );
    }
    None
}

pub struct Hop {
    pub entry: EntryResult,
    pub at: SectorId,
}

/// Move one lane (AP already validated by caller). Resolves entry hazards.
pub fn hop(ctx: &mut Ctx, pid: PlayerId, to: SectorId, ap: i64, blind: bool) -> Hop {
    ctx.spend_ap(pid, ap);
    let p = ctx.player_mut(pid);
    let from = p.sector;
    p.prev_sector = from;
    p.sector = to;
    p.stats.warps += 1;
    knowledge::observe(ctx.w, pid, to, true);
    let entry = combat::on_enter(ctx, pid, to, from, blind);
    let at = ctx.w.players[&pid].sector;
    if at != to && !entry.destroyed {
        knowledge::observe(ctx.w, pid, at, true);
    }
    if !entry.destroyed {
        crate::events::on_arrive(ctx, pid, at);
    }
    if ctx.w.players[&pid].kind != PlayerKind::Npc {
        let name = ctx.w.players[&pid].name.clone();
        ctx.event(
            "move",
            Some(at),
            vec![pid],
            true,
            format!("{name} warped {from} -> {at}"),
            json!({"from": from, "to": at}),
        );
    }
    Hop { entry, at }
}

fn warp_cost(w: &World, pid: PlayerId) -> i64 {
    w.rules.ship(w.players[&pid].ship.class).warp_ap
}

pub fn warp(ctx: &mut Ctx, pid: PlayerId, a: &WarpArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let here = ctx.w.sector(p.sector).unwrap().clone();
    let dest = a.destination_sector;
    if dest == p.sector {
        return Err(Refusal::new("already_here", format!("You are already in sector {dest}.")));
    }
    if !here.warps.contains(&dest) {
        return Err(Refusal::new(
            "not_adjacent",
            format!(
                "Sector {dest} is not adjacent to sector {}. Adjacent sectors: {:?}.",
                p.sector, here.warps
            ),
        )
        .hint("Use autopilot_travel for multi-hop journeys (plot_course to preview)."));
    }
    if let Some(r) = departure_block(ctx.w, pid, dest) {
        return Err(r);
    }
    let cost = warp_cost(ctx.w, pid);
    ctx.require_ap(pid, cost)?;
    let h = hop(ctx, pid, dest, cost, false);
    let mut summary = if h.entry.destroyed {
        "Your ship was destroyed on arrival.".to_string()
    } else if h.at != dest {
        format!("Entered sector {dest} but ended in sector {}.", h.at)
    } else {
        format!("Warped to sector {dest}.")
    };
    if !h.entry.log.is_empty() {
        summary.push(' ');
        summary.push_str(&h.entry.log.join(" "));
    }
    let view = query::sector_view(ctx.w, pid, ctx.w.players[&pid].sector);
    let reply = json!({"log": h.entry.log, "destroyed": h.entry.destroyed, "sector": view, "you": brief(ctx.w, pid)});
    Ok(ctx.finish(summary, reply))
}

pub fn autopilot(ctx: &mut Ctx, pid: PlayerId, a: &AutopilotArgs) -> Res<Outcome> {
    let avoid: BTreeSet<SectorId> = a.avoid_sectors.iter().copied().collect();
    autopilot_within(ctx, pid, a.destination_sector, &avoid, a.avoid_hazards, a.max_toll, 0)
}

/// `autopilot` with the exclusions already collected and a toll allowance for planning: charted
/// tolls up to `toll_cap` are not hazards (see `path::plot_within`). `autopilot_travel` passes 0,
/// which plans exactly as before; `trade_loop` passes the toll it will pay, so its legs fly the
/// route its full-lap preflight planned.
pub(crate) fn autopilot_within(
    ctx: &mut Ctx,
    pid: PlayerId,
    dest: SectorId,
    avoid: &BTreeSet<SectorId>,
    avoid_hazards: bool,
    max_toll: Option<i64>,
    toll_cap: i64,
) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    if dest == 0 || dest > ctx.w.sector_count() {
        return Err(Refusal::new(
            "unknown_sector",
            format!("Sector {dest} does not exist (1..={}).", ctx.w.sector_count()),
        ));
    }
    if dest == p.sector {
        return Err(Refusal::new("already_here", format!("You are already in sector {dest}.")));
    }
    let chart = knowledge::merged(ctx.w, pid);
    let plotted = path::plot_within(ctx.w, pid, &chart, p.sector, dest, avoid, avoid_hazards, toll_cap)
        .or_else(|| {
            if avoid_hazards {
                None
            } else {
                path::plot_within(ctx.w, pid, &chart, p.sector, dest, avoid, false, toll_cap)
            }
        })
        .ok_or_else(|| {
            let charted = knowledge::known(ctx.w, pid, dest).is_some();
            Refusal::new(
                "no_known_route",
                if charted {
                    format!(
                        "No route to sector {dest} through your charted space{}.",
                        if avoid_hazards { " that avoids known hazards" } else { "" }
                    )
                } else {
                    format!("Sector {dest} is not on your chart.")
                },
            )
            .hint("Explore toward it, launch an ether probe (launch_probe) to chart a route, or retry with avoid_hazards=false. Lanes can be one-way: a route uses only lanes charted in its direction (each sector's w lists its outgoing lanes), so check the w of every sector on the route you expect.")
        })?;
    let route = plotted.path.clone();
    let cost = warp_cost(ctx.w, pid);
    ctx.require_ap(pid, cost)?;
    if let Some(r) = departure_block(ctx.w, pid, route[1]) {
        return Err(r);
    }
    let saved_toll = p.doctrine.max_auto_toll;
    if let Some(t) = max_toll {
        ctx.player_mut(pid).doctrine.max_auto_toll = t.max(0);
    }
    let mut log: Vec<String> = vec![];
    let mut taken = vec![p.sector];
    let mut reason: Option<String> = None;
    let mut destroyed = false;
    let mut ap_spent = 0;
    let max_hops = ctx.w.rules.autopilot_max_hops as usize;
    for (i, &next) in route.iter().enumerate().skip(1) {
        if i > max_hops {
            reason = Some(format!("Autopilot hop limit ({max_hops}) reached."));
            break;
        }
        if ctx.w.players[&pid].ap < cost {
            reason = Some(format!("Out of AP (need {cost} per hop)."));
            break;
        }
        if let Some(r) = departure_block(ctx.w, pid, next) {
            reason = Some(r.message);
            break;
        }
        let h = hop(ctx, pid, next, cost, false);
        ap_spent += cost;
        log.extend(h.entry.log.iter().cloned());
        if h.entry.destroyed {
            destroyed = true;
            reason = Some("Ship destroyed.".into());
            break;
        }
        taken.push(h.at);
        if h.at != next {
            reason = h.entry.halt.clone().or(Some(format!("Displaced to sector {}.", h.at)));
            break;
        }
        if let Some(halt) = h.entry.halt
            && next != dest
        {
            reason = Some(halt);
            break;
        }
    }
    if max_toll.is_some()
        && let Some(pm) = ctx.w.players.get_mut(&pid)
    {
        pm.doctrine.max_auto_toll = saved_toll;
    }
    let at = ctx.w.players[&pid].sector;
    let arrived = at == dest && !destroyed;
    let remaining: Vec<SectorId> = route.iter().copied().skip_while(|s| *s != at).skip(1).collect();
    let summary = if arrived {
        format!("Autopilot arrived at sector {dest} after {} hops ({ap_spent} AP).", taken.len() - 1)
    } else {
        format!("Autopilot stopped at sector {at}: {}", reason.clone().unwrap_or_default())
    };
    log.truncate(30);
    let reply = json!({
        "arrived": arrived,
        "stopped_at": at,
        "stop_reason": reason,
        "path_taken": taken,
        "remaining_path": if arrived { vec![] } else { remaining },
        "ap_spent": ap_spent,
        "log": log,
        "destroyed": destroyed,
        "sector": query::sector_view(ctx.w, pid, at),
        "you": brief(ctx.w, pid),
    });
    Ok(ctx.finish(summary, reply))
}

pub fn transwarp(ctx: &mut Ctx, pid: PlayerId, a: &TranswarpArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    let dest = a.destination_sector;
    if dest == p.sector {
        return Err(Refusal::new("already_here", "You are already there."));
    }
    if knowledge::known(ctx.w, pid, dest).is_none() {
        return Err(Refusal::new(
            "unknown_sector",
            format!("Sector {dest} is not on your chart; TransWarp needs charted coordinates."),
        )
        .hint("Chart it first by exploring or with launch_probe."));
    }
    let beacon = ctx
        .w
        .sector(dest)
        .map(|s| {
            s.planets
                .iter()
                .filter_map(|id| ctx.w.planets.get(id))
                .any(|pl| pl.citadel >= 4 && pl.owner.map(|o| ctx.w.friendly(o, pid)).unwrap_or(false))
        })
        .unwrap_or(false);
    if !p.ship.transwarp && !beacon {
        return Err(Refusal::new(
            "no_transwarp",
            "Your ship has no TransWarp drive and the destination has no friendly TransWarp beacon (citadel 4+).",
        )
        .hint(format!(
            "Buy a transwarp_drive at Stardock ({} cr) on a capable hull, or use autopilot_travel.",
            rules.price_transwarp
        )));
    }
    if combat::interdicted(ctx.w, pid, p.sector) {
        return Err(Refusal::new("interdicted", "A hostile Interdictor holds your ship here."));
    }
    let dist = path::real_distance(ctx.w, p.sector, dest).unwrap_or(50) as i64;
    let mut ore = dist * rules.transwarp_ore_per_hop;
    if beacon {
        ore /= 2;
    }
    if p.ship.cargo[0] < ore {
        return Err(Refusal::new(
            "insufficient_fuel",
            format!(
                "The jump spans {dist} hops and needs {ore} fuel ore in your holds; you carry {}.",
                p.ship.cargo[0]
            ),
        )
        .hint("Buy fuel_ore at a port that sells it."));
    }
    ctx.require_ap(pid, rules.ap_transwarp)?;
    let sec = ctx.w.sector(dest).unwrap();
    let safe = sec.fighters.as_ref().map(|f| ctx.w.friendly(f.owner, pid)).unwrap_or(false) || beacon || sec.fedspace;
    let pm = ctx.player_mut(pid);
    pm.ship.burn_cargo(0, ore);
    let h = hop(ctx, pid, dest, rules.ap_transwarp, !safe);
    let mut summary = format!("TransWarp jump to sector {dest} ({dist} hops, {ore} ore).");
    if !safe {
        summary.push_str(" Blind jump: hostile defenses strike at double strength.");
    }
    if !h.entry.log.is_empty() {
        summary.push(' ');
        summary.push_str(&h.entry.log.join(" "));
    }
    let reply = json!({"log": h.entry.log, "destroyed": h.entry.destroyed, "ore_used": ore,
        "sector": query::sector_view(ctx.w, pid, ctx.w.players[&pid].sector), "you": brief(ctx.w, pid)});
    Ok(ctx.finish(summary, reply))
}

pub fn scan(ctx: &mut Ctx, pid: PlayerId, a: &ScanArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    let warps = ctx.w.sector(p.sector).unwrap().warps.clone();
    match a.scan_type {
        ScanType::Density => {
            if !p.ship.density_scanner {
                return Err(Refusal::new("no_scanner", "No density scanner installed."));
            }
            ctx.require_ap(pid, rules.ap_scan_density)?;
            ctx.spend_ap(pid, rules.ap_scan_density);
            let tick = ctx.w.tick;
            let mut rows: Vec<Value> = vec![];
            for s in &warps {
                let d = knowledge::density(ctx.w, *s);
                let sec = ctx.w.sector(*s).unwrap();
                let anomaly = sec.limpet.is_some();
                let nwarps = sec.warps.len();
                let neb = sec.nebula.clone();
                let k = ctx.player_mut(pid).knowledge.entry(*s).or_default();
                k.density = Some(d);
                k.last_seen = k.last_seen.max(tick);
                rows.push(json!({"sector": s, "density": d, "warps": nwarps, "anomaly": anomaly, "nebula": neb}));
            }
            let reply = json!({"scan": "density", "legend": "ship 40, port 100, planet 500, 5/fighter, 10/armid mine, beacon 1; anomaly = limpet mines",
                "adjacent": rows, "you": brief(ctx.w, pid)});
            Ok(ctx.finish(format!("Density scan of {} adjacent sectors.", warps.len()), reply))
        }
        ScanType::Holographic => {
            if !p.ship.holo_scanner {
                return Err(Refusal::new("no_scanner", "No holo scanner installed.").hint(format!(
                    "Buy one at Stardock: buy_equipment item=holo_scanner ({} cr).",
                    rules.price_holo_scanner
                )));
            }
            ctx.require_ap(pid, rules.ap_scan_holo)?;
            ctx.spend_ap(pid, rules.ap_scan_holo);
            let mut rows = vec![];
            for s in &warps {
                knowledge::observe(ctx.w, pid, *s, false);
                let mut row = query::sector_view(ctx.w, pid, *s);
                // The live toll at scan time (feedback #42); the chart keeps owner, count and mode.
                if let Some(f) = ctx
                    .w
                    .sector(*s)
                    .and_then(|x| x.fighters.as_ref())
                    .filter(|f| f.mode == FighterMode::Toll)
                    && row["fighters_seen"].is_object()
                {
                    row["fighters_seen"]["toll"] = json!(f.toll(rules.toll_per_fighter));
                }
                rows.push(row);
            }
            let reply = json!({"scan": "holographic", "adjacent": rows, "you": brief(ctx.w, pid)});
            Ok(ctx.finish(format!("Holo scan charted {} adjacent sectors.", warps.len()), reply))
        }
    }
}

pub fn probe(ctx: &mut Ctx, pid: PlayerId, a: &ProbeArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    if p.ship.probes <= 0 {
        return Err(
            Refusal::new("no_probes", "You carry no ether probes.").hint(format!("Buy them at Stardock ({} cr each).", rules.price_probe))
        );
    }
    let dest = a.destination_sector;
    if dest == 0 || dest > ctx.w.sector_count() {
        return Err(Refusal::new("unknown_sector", format!("Sector {dest} does not exist.")));
    }
    let route = path::real_path(ctx.w, p.sector, dest, rules.probe_max_hops).ok_or_else(|| {
        Refusal::new(
            "out_of_range",
            format!("Sector {dest} is beyond probe range ({} hops).", rules.probe_max_hops),
        )
    })?;
    ctx.require_ap(pid, rules.ap_probe)?;
    ctx.spend_ap(pid, rules.ap_probe);
    ctx.player_mut(pid).ship.probes -= 1;
    let mut charted = vec![];
    let mut destroyed_at = None;
    for &s in route.iter().skip(1) {
        knowledge::observe(ctx.w, pid, s, false);
        charted.push(s);
        let hostile = ctx
            .w
            .sector(s)
            .unwrap()
            .fighters
            .as_ref()
            .map(|f| !crate::diplomacy::at_peace(ctx.w, f.owner, pid) && f.count > 0)
            .unwrap_or(false);
        if hostile {
            destroyed_at = Some(s);
            if let Some(owner) = ctx.w.sector(s).unwrap().fighters.as_ref().map(|f| f.owner) {
                let n = ctx.w.player_name(pid);
                ctx.notify(
                    owner,
                    "probe",
                    format!("Your fighters in sector {s} destroyed an ether probe launched by {n}."),
                );
            }
            break;
        }
    }
    let summary = match destroyed_at {
        Some(s) => format!("Probe charted {} sectors before being destroyed in sector {s}.", charted.len()),
        None => format!("Probe reached sector {dest}, charting {} sectors.", charted.len()),
    };
    let shown_route: Vec<SectorId> = if destroyed_at.is_some() {
        std::iter::once(p.sector).chain(charted.iter().copied()).collect()
    } else {
        route.clone()
    };
    let reply = json!({"route": shown_route, "destination": dest, "charted": charted, "destroyed_at": destroyed_at,
        "destination_view": if destroyed_at.is_none() { query::sector_view(ctx.w, pid, dest) } else { Value::Null },
        "you": brief(ctx.w, pid)});
    Ok(ctx.finish(summary, reply))
}
