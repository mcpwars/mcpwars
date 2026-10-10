//! Planets: Genesis, claiming, colonists, production logistics, citadels.

use serde_json::json;

use crate::cmd::*;
use crate::engine::*;
use crate::galaxy;
use crate::model::*;
use crate::player;
use crate::query;

pub fn genesis(ctx: &mut Ctx, pid: PlayerId, a: &GenesisArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    let sid = p.sector;
    if p.ship.genesis <= 0 {
        return Err(Refusal::new("no_genesis", "You carry no Genesis torpedoes.")
            .hint(format!("Buy one at Stardock ({} cr).", rules.price_genesis)));
    }
    if ctx.w.is_fedspace(sid) {
        return Err(Refusal::new(
            "protected_fedspace",
            "The Federation forbids planet creation in FedSpace.",
        ));
    }
    if ctx.w.is_fed_buffer(sid) {
        return Err(Refusal::new(
            "fed_buffer",
            "This sector borders FedSpace; the Federation forbids founding planets in its buffer zone.",
        )
        .hint("Go at least two lanes beyond FedSpace."));
    }
    if ctx.w.sector(sid).unwrap().planets.len() >= rules.max_planets_per_sector {
        return Err(Refusal::new(
            "sector_full",
            format!("Sector {sid} already has {} planets.", rules.max_planets_per_sector),
        ));
    }
    let name = match a.planet_name.as_deref().map(|n| player::sanitize(n, 24)).filter(|n| !n.is_empty()) {
        Some(n) => n,
        None => galaxy::make_name(&mut ctx.rng.clone()),
    };
    ctx.require_ap(pid, rules.ap_genesis)?;
    ctx.spend_ap(pid, rules.ap_genesis);
    let class = galaxy::roll_planet_class(&mut ctx.rng, &rules);
    let id = ctx.w.next_planet;
    ctx.w.next_planet += 1;
    let mut pl = galaxy::new_planet(id, name.clone(), sid, class, ctx.w.tick);
    pl.owner = Some(pid);
    pl.genesis = true;
    ctx.w.planets.insert(id, pl);
    ctx.w.sector_mut(sid).unwrap().planets.push(id);
    let pm = ctx.player_mut(pid);
    pm.ship.genesis -= 1;
    pm.stats.planets_created += 1;
    pm.experience += 25;
    pm.alignment += if pm.alignment >= 0 { 5 } else { -5 };
    let pname = pm.name.clone();
    crate::knowledge::observe(ctx.w, pid, sid, true);
    ctx.news(format!(
        "{pname} detonated a Genesis torpedo in sector {sid}: planet {name} (class {}) is born.",
        class.label()
    ));
    ctx.event(
        "planet_created",
        Some(sid),
        vec![pid],
        true,
        format!("{pname} created planet {name} in sector {sid}."),
        json!({"planet": id, "class": class}),
    );
    let reply = json!({"planet": query::planet_view(ctx.w, pid, id), "you": brief(ctx.w, pid)});
    Ok(ctx.finish(
        format!("Genesis! Planet {name} (id {id}, class {}) created in sector {sid}.", class.label()),
        reply,
    ))
}

pub fn claim(ctx: &mut Ctx, pid: PlayerId, a: &ClaimPlanetArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let pl = ctx
        .w
        .planets
        .get(&a.planet_id)
        .filter(|pl| pl.sector == p.sector)
        .cloned()
        .ok_or_else(|| Refusal::new("target_unavailable", "No such planet in this sector.").hint("view_sector lists planets here."))?;
    if pl.owner.is_some() {
        return Err(Refusal::new("owned", format!("Planet {} is already owned.", pl.name))
            .hint("Hostile planets can be captured with attack_target(target_type=planet)."));
    }
    let cost = ctx.w.rules.ap_planet;
    ctx.require_ap(pid, cost)?;
    ctx.spend_ap(pid, cost);
    ctx.w.planets.get_mut(&a.planet_id).unwrap().owner = Some(pid);
    ctx.player_mut(pid).experience += 10;
    ctx.news(format!("{} claimed the wild planet {} in sector {}.", p.name, pl.name, p.sector));
    ctx.event(
        "planet_claimed",
        Some(p.sector),
        vec![pid],
        true,
        format!("{} claimed planet {}.", p.name, pl.name),
        json!({"planet": a.planet_id}),
    );
    let reply = json!({"planet": query::planet_view(ctx.w, pid, a.planet_id), "you": brief(ctx.w, pid)});
    Ok(ctx.finish(format!("Claimed planet {} ({}).", pl.name, pl.class.label()), reply))
}

/// (sector %, atmosphere %) quasar cannon settings in effect for a planet.
pub fn qcannon_settings(rules: &crate::rules::Rules, pl: &Planet) -> (i64, i64) {
    pl.qcannon.unwrap_or((rules.qcannon_sector_pct, rules.qcannon_atmo_pct))
}

pub fn can_manage(w: &World, pid: PlayerId, pl: &Planet) -> bool {
    match pl.owner {
        Some(o) if o == pid => true,
        Some(o) => pl.corp_shared && w.friendly(o, pid),
        None => false,
    }
}

pub fn manage(ctx: &mut Ctx, pid: PlayerId, a: &PlanetArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    let pl = ctx
        .w
        .planets
        .get(&a.planet_id)
        .cloned()
        .filter(|pl| crate::query::planet_visible(ctx.w, pid, pl))
        .ok_or_else(|| Refusal::new("target_unavailable", "No such planet visible to you."))?;
    if pl.sector != p.sector {
        return Err(Refusal::new(
            "not_here",
            format!("Planet {} is in sector {}; you are in sector {}.", pl.name, pl.sector, p.sector),
        )
        .hint(format!("autopilot_travel destination_sector={}", pl.sector)));
    }
    // A hauler under contract may unload the contracted goods onto someone else's planet.
    let hauling = a.action == PlanetAction::UnloadCargo && a.commodity.is_some_and(|c| crate::contracts::hauls_to(ctx.w, pid, pl.id, c));
    if !can_manage(ctx.w, pid, &pl) && !hauling {
        return Err(
            Refusal::new("permission_denied", format!("Planet {} is not yours.", pl.name)).hint(if pl.owner.is_none() {
                "claim_planet first."
            } else {
                "Capture it with attack_target(target_type=planet)."
            }),
        );
    }
    let need_commodity = || {
        a.commodity
            .ok_or_else(|| Refusal::new("invalid_args", "commodity is required for this action."))
    };
    let ap = match a.action {
        PlanetAction::Rename | PlanetAction::SetShared | PlanetAction::SetQcannon => 0,
        _ => rules.ap_planet,
    };
    let spec = rules.planet(pl.class).clone();
    let ship_spec = rules.ship(p.ship.class).clone();
    let summary: String;
    match a.action {
        PlanetAction::DepositColonists => {
            let c = need_commodity()?;
            let room = spec.max_colonists_per_product - pl.colonists[c.idx()];
            let n = a.quantity.unwrap_or(p.ship.colonists).min(p.ship.colonists).min(room);
            if n <= 0 {
                return Err(Refusal::new(
                    "cannot_deposit",
                    if p.ship.colonists == 0 {
                        "You carry no colonists (buy_colonists at Stardock).".to_string()
                    } else {
                        format!(
                            "{} {} already has the maximum {} colonists working {}.",
                            pl.class.label(),
                            pl.name,
                            spec.max_colonists_per_product,
                            c.name()
                        )
                    },
                )
                .hint("Assign colonists to another commodity (production per colonist differs by planet class: see view_planets)."));
            }
            if spec.colonists_per_unit[c.idx()] == 0 {
                return Err(Refusal::new(
                    "cannot_produce",
                    format!("A class {} planet cannot produce {}.", pl.class.label(), c.name()),
                )
                .hint("view_planets shows what this planet can produce."));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            ctx.player_mut(pid).ship.colonists -= n;
            ctx.w.planets.get_mut(&a.planet_id).unwrap().colonists[c.idx()] += n;
            summary = format!("Landed {n} colonists on {} to produce {}.", pl.name, c.name());
        }
        PlanetAction::ReassignColonists => {
            let to = need_commodity()?;
            let from = a
                .from_commodity
                .ok_or_else(|| Refusal::new("invalid_args", "from_commodity is required."))?;
            // The destination line has the same cap as a deposit.
            let room = (rules.planet(pl.class).max_colonists_per_product - pl.colonists[to.idx()]).max(0);
            let n = a
                .quantity
                .unwrap_or(pl.colonists[from.idx()])
                .min(pl.colonists[from.idx()])
                .min(room);
            if from == to {
                return Err(Refusal::new("invalid_args", "Nothing to reassign."));
            }
            if n <= 0 {
                return Err(Refusal::new(
                    "at_capacity",
                    format!("{} already has the maximum colonists working {}.", pl.name, to.name()),
                ));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            let plm = ctx.w.planets.get_mut(&a.planet_id).unwrap();
            plm.colonists[from.idx()] -= n;
            plm.colonists[to.idx()] += n;
            summary = format!("Reassigned {n} colonists from {} to {}.", from.name(), to.name());
        }
        PlanetAction::LoadCargo => {
            let c = need_commodity()?;
            let n = a.quantity.unwrap_or(i64::MAX).min(pl.stock[c.idx()]).min(p.ship.free_holds());
            if n <= 0 {
                return Err(Refusal::new("cannot_load", "No stock to load or no free holds."));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            ctx.w.planets.get_mut(&a.planet_id).unwrap().stock[c.idx()] -= n;
            ctx.player_mut(pid).ship.cargo[c.idx()] += n;
            crate::contracts::on_cargo(ctx, pid, a.planet_id, c, -n);
            summary = format!("Loaded {n} {} from {}.", c.name(), pl.name);
        }
        PlanetAction::UnloadCargo => {
            let c = need_commodity()?;
            let have = p.ship.cargo[c.idx()];
            let n = a.quantity.unwrap_or(have).min(have);
            if n <= 0 {
                return Err(Refusal::new("cannot_unload", format!("You carry no {}.", c.name())));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            let pm = ctx.player_mut(pid);
            let basis = pm.ship.cargo_cost[c.idx()] * n / have.max(1);
            pm.ship.cargo_cost[c.idx()] -= basis;
            pm.ship.cargo[c.idx()] -= n;
            ctx.w.planets.get_mut(&a.planet_id).unwrap().stock[c.idx()] += n;
            crate::contracts::on_cargo(ctx, pid, a.planet_id, c, n);
            summary = format!("Unloaded {n} {} onto {}.", c.name(), pl.name);
        }
        PlanetAction::DepositFighters => {
            let n = a.quantity.unwrap_or(p.ship.fighters).min(p.ship.fighters);
            if n <= 0 {
                return Err(Refusal::new("insufficient_fighters", "You carry no fighters."));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            ctx.player_mut(pid).ship.fighters -= n;
            ctx.w.planets.get_mut(&a.planet_id).unwrap().fighters += n;
            summary = format!("Stationed {n} fighters on {}.", pl.name);
        }
        PlanetAction::WithdrawFighters => {
            let room = ship_spec.fighters_max - p.ship.fighters;
            let n = a.quantity.unwrap_or(pl.fighters).min(pl.fighters).min(room);
            if n <= 0 {
                return Err(Refusal::new("cannot_withdraw", "No planetary fighters or no room aboard."));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            ctx.w.planets.get_mut(&a.planet_id).unwrap().fighters -= n;
            ctx.player_mut(pid).ship.fighters += n;
            summary = format!("Took {n} fighters aboard from {}.", pl.name);
        }
        PlanetAction::BuildCitadel => {
            if pl.class == PlanetClass::U {
                return Err(Refusal::new(
                    "citadel_unavailable",
                    "Gas giants are storage-only outposts and do not support citadel upgrades.",
                )
                .hint("Choose a habitable planet for a citadel; view_planets reports citadel_available."));
            }
            if let Some(pr) = &pl.project {
                return Err(Refusal::new(
                    "busy",
                    format!(
                        "Citadel level {} is already under construction (done at tick {}).",
                        pr.target_level, pr.complete_tick
                    ),
                ));
            }
            let next = pl.citadel + 1;
            let c = rules
                .citadel(next)
                .cloned()
                .ok_or_else(|| Refusal::new("max_level", "The citadel is already at level 6."))?;
            let mut missing = vec![];
            if pl.total_colonists() < c.colonists_required {
                missing.push(format!(
                    "{} colonists on the planet (has {})",
                    c.colonists_required,
                    pl.total_colonists()
                ));
            }
            for i in 0..3 {
                if pl.stock[i] < c.products[i] {
                    missing.push(format!(
                        "{} {} in planet stock (has {})",
                        c.products[i],
                        Commodity::ALL[i].name(),
                        pl.stock[i]
                    ));
                }
            }
            if p.credits < c.credits {
                missing.push(format!("{} credits on your ship (have {})", c.credits, p.credits));
            }
            if !missing.is_empty() {
                return Err(Refusal::new(
                    "requirements_unmet",
                    format!("Citadel level {next} ({}) needs: {}.", c.name, missing.join("; ")),
                )
                .hint("Colonists produce goods every tick; you can also unload cargo onto the planet (planet action=unload_cargo)."));
            }
            ctx.require_ap(pid, ap)?;
            ctx.spend_ap(pid, ap);
            ctx.player_mut(pid).credits -= c.credits;
            let done = ctx.w.tick + c.build_ticks;
            let plm = ctx.w.planets.get_mut(&a.planet_id).unwrap();
            for i in 0..3 {
                plm.stock[i] -= c.products[i];
            }
            plm.project = Some(CitadelProject {
                target_level: next,
                complete_tick: done,
            });
            summary = format!(
                "Construction of citadel level {next} ({}) begun on {}; completes at tick {done}. Unlocks: {}",
                c.name, pl.name, c.unlocks
            );
            ctx.event(
                "citadel_started",
                Some(pl.sector),
                vec![pid],
                true,
                format!("{} began citadel level {next} on {}.", p.name, pl.name),
                json!({}),
            );
        }
        PlanetAction::DepositCredits | PlanetAction::WithdrawCredits => {
            if pl.citadel < 1 {
                return Err(Refusal::new("no_treasury", "A treasury needs citadel level 1.").hint("planet action=build_citadel"));
            }
            ctx.require_ap(pid, ap)?;
            if a.action == PlanetAction::DepositCredits {
                let n = a.quantity.unwrap_or(p.credits).min(p.credits);
                if n <= 0 {
                    return Err(Refusal::new("invalid_args", "Nothing to deposit."));
                }
                ctx.spend_ap(pid, ap);
                ctx.player_mut(pid).credits -= n;
                ctx.w.planets.get_mut(&a.planet_id).unwrap().treasury += n;
                summary = format!("Deposited {n} credits in {}'s treasury.", pl.name);
            } else {
                let n = a.quantity.unwrap_or(pl.treasury).min(pl.treasury);
                if n <= 0 {
                    return Err(Refusal::new("invalid_args", "The treasury is empty."));
                }
                ctx.spend_ap(pid, ap);
                ctx.player_mut(pid).credits += n;
                ctx.w.planets.get_mut(&a.planet_id).unwrap().treasury -= n;
                summary = format!("Withdrew {n} credits from {}'s treasury.", pl.name);
            }
        }
        PlanetAction::Rename => {
            let n = a
                .name
                .as_deref()
                .map(|s| player::sanitize(s, 24))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Refusal::new("invalid_args", "name is required."))?;
            ctx.w.planets.get_mut(&a.planet_id).unwrap().name = n.clone();
            summary = format!("Planet renamed to {n}.");
        }
        PlanetAction::SetQcannon => {
            if pl.owner != Some(pid) {
                return Err(Refusal::new("permission_denied", "Only the owner can set the quasar cannon."));
            }
            let (ds, da) = qcannon_settings(&rules, &pl);
            let s = a.sector_pct.unwrap_or(ds);
            let at = a.atmo_pct.unwrap_or(da);
            if !(0..=100).contains(&s) || !(0..=100).contains(&at) {
                return Err(Refusal::new("invalid_args", "sector_pct and atmo_pct must be 0-100."));
            }
            ctx.w.planets.get_mut(&a.planet_id).unwrap().qcannon = Some((s, at));
            summary = format!(
                "Quasar cannon on {} set to {s}% sector fire, {at}% atmospheric fire{}.",
                pl.name,
                if pl.citadel < 3 { " (takes effect at citadel level 3)" } else { "" }
            );
        }
        PlanetAction::SetShared => {
            if pl.owner != Some(pid) {
                return Err(Refusal::new("permission_denied", "Only the owner can change sharing."));
            }
            let v = a
                .shared
                .ok_or_else(|| Refusal::new("invalid_args", "shared (true/false) is required."))?;
            ctx.w.planets.get_mut(&a.planet_id).unwrap().corp_shared = v;
            summary = format!("Corp access to {} {}.", pl.name, if v { "enabled" } else { "disabled" });
        }
    }
    let reply = json!({"planet": query::planet_view(ctx.w, pid, a.planet_id), "you": brief(ctx.w, pid)});
    Ok(ctx.finish(summary, reply))
}

/// Units (thousandths) produced per tick by `colonists` working product `i`.
pub fn production_milli(rules: &crate::rules::Rules, spec: &crate::rules::PlanetSpec, i: usize, colonists: i64) -> i64 {
    let ratio = spec.colonists_per_unit[i];
    if ratio <= 0 {
        return 0;
    }
    (colonists as i128 * 1_000 / (ratio as i128 * rules.planet_day_ticks.max(1) as i128)) as i64
}

/// Per-tick planetary economy. Called from the tick engine.
pub fn tick_planets(ctx: &mut Ctx) {
    let rules = ctx.w.rules.clone();
    let tick = ctx.w.tick;
    let ids: Vec<PlanetId> = ctx.w.planets.keys().copied().collect();
    for id in ids {
        let mut done_msg = None;
        {
            let pl = ctx.w.planets.get_mut(&id).unwrap();
            let spec = rules.planet(pl.class);
            // One unit per `colonists_per_unit` colonists per planet-day.
            let mut produced_milli = 0;
            for i in 0..3 {
                let made = production_milli(&rules, spec, i, pl.colonists[i]);
                // The fighter factory only runs on output that fits in the warehouse.
                if pl.stock[i] < spec.storage[i] {
                    produced_milli += made;
                }
                pl.carry[i] += made;
                pl.stock[i] = (pl.stock[i] + pl.carry[i] / 1_000).min(spec.storage[i].max(pl.stock[i]));
                pl.carry[i] %= 1_000;
            }
            // Fighter factory: one fighter per `fighter_divisor` units produced.
            if pl.citadel >= 2 && spec.fighter_divisor > 0 && pl.fighters < rules.planet_fighters_max {
                pl.carry[3] += produced_milli / spec.fighter_divisor;
                pl.fighters = (pl.fighters + pl.carry[3] / 1_000).min(rules.planet_fighters_max);
                pl.carry[3] %= 1_000;
            }
            if pl.citadel >= 5 && pl.stock[2] > 0 && pl.shields < rules.planet_shields_max {
                let eq = pl.stock[2].min(10);
                pl.stock[2] -= eq;
                pl.shields = (pl.shields + eq * rules.planet_shield_per_equipment).min(rules.planet_shields_max);
            }
            // Slow natural growth: 0.1% every 10 ticks (~15%/day) for established colonies.
            let total = pl.total_colonists();
            if total >= 100 && tick.is_multiple_of(10) {
                let biggest = (0..3).max_by_key(|i| (pl.colonists[*i], 2 - *i)).unwrap();
                let grow = (total / 1_000).max(1).min(spec.max_colonists_per_product - pl.colonists[biggest]);
                if grow > 0 {
                    pl.colonists[biggest] += grow;
                }
            }
            if pl.citadel >= 1 && pl.treasury > 0 && tick.is_multiple_of(1_440) {
                let interest = (pl.treasury as i128 * rules.treasury_interest_bp_per_day as i128 / 10_000) as i64;
                pl.treasury = pl.treasury.saturating_add(interest);
            }
            if let Some(pr) = pl.project.clone()
                && tick >= pr.complete_tick
            {
                pl.citadel = pr.target_level;
                pl.project = None;
                done_msg = Some((pl.owner, pl.name.clone(), pl.sector, pr.target_level));
            }
        }
        if let Some((owner, name, sector, lvl)) = done_msg {
            let cname = rules.citadel(lvl).map(|c| c.name.clone()).unwrap_or_default();
            if let Some(o) = owner {
                ctx.notify(
                    o,
                    "citadel_complete",
                    format!("Citadel level {lvl} ({cname}) completed on {name} in sector {sector}."),
                );
                let oname = ctx.w.player_name(o);
                ctx.news(format!("{oname}'s planet {name} completed citadel level {lvl}: {cname}."));
                ctx.event(
                    "citadel_complete",
                    Some(sector),
                    vec![o],
                    true,
                    format!("{name} reached citadel level {lvl}."),
                    json!({"planet": id, "level": lvl}),
                );
            }
        }
    }
}
