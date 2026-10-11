//! Fighters, mines, battles, ship destruction and sector-entry hazards.

use serde_json::json;

use crate::cmd::*;
use crate::engine::*;
use crate::knowledge;
use crate::model::*;
use crate::player::{self, is_newbie};
use crate::rng::Rng;
use crate::rules::Rules;

#[derive(Clone, Debug, Default)]
pub struct Force {
    pub fighters: i64,
    pub shields: i64,
    pub off_bp: i64,
    pub def_bp: i64,
    /// Flee when fighters+shields drops below this absolute value (0 = never).
    pub flee_at: i64,
    /// Extra damage per round (quasar cannon).
    pub cannon: i64,
}

#[derive(Clone, Debug, Default)]
pub struct Battle {
    pub rounds: u32,
    pub attacker_lost: i64,
    pub defender_fighters_lost: i64,
    pub defender_shields_lost: i64,
    pub defender_fled: bool,
    pub defender_broken: bool,
    pub attacker_broken: bool,
}

fn volley(rng: &mut Rng, rules: &Rules, fighters: i64, off_bp: i64) -> i64 {
    if fighters <= 0 {
        return 0;
    }
    let base = fighters as f64 * rules.fire_rate_bp as f64 / 10_000.0 * off_bp as f64 / 10_000.0;
    (base * rng.jitter(rules.combat_jitter_bp as f64 / 10_000.0)).ceil() as i64
}

fn absorb(f: &mut Force, dmg: i64) -> (i64, i64) {
    let s = f.shields.min(dmg);
    f.shields -= s;
    let fl = f.fighters.min(dmg - s);
    f.fighters -= fl;
    (fl, s)
}

/// Simultaneous-fire battle of up to `rules.combat_rounds` rounds.
pub fn battle(rng: &mut Rng, rules: &Rules, a: &mut Force, d: &mut Force) -> Battle {
    let mut b = Battle::default();
    let a0 = a.fighters;
    for round in 1..=rules.combat_rounds {
        if a.fighters <= 0 || (d.fighters <= 0 && d.shields <= 0) {
            break;
        }
        b.rounds = round;
        let a_fire = volley(rng, rules, a.fighters, a.off_bp) + a.cannon;
        let d_fire = volley(rng, rules, d.fighters, d.off_bp) + d.cannon;
        let dmg_d = a_fire * 10_000 / d.def_bp.max(1);
        let dmg_a = d_fire * 10_000 / a.def_bp.max(1);
        let (fl, sl) = absorb(d, dmg_d);
        b.defender_fighters_lost += fl;
        b.defender_shields_lost += sl;
        absorb(a, dmg_a);
        let left = d.fighters + d.shields;
        if d.flee_at > 0 && left > 0 && left < d.flee_at && a.fighters > 0 {
            b.defender_fled = true;
            break;
        }
    }
    b.attacker_lost = a0 - a.fighters;
    b.defender_broken = d.fighters <= 0 && d.shields <= 0;
    b.attacker_broken = a.fighters <= 0;
    b
}

pub fn ship_force(w: &World, p: &Player) -> Force {
    let spec = w.rules.ship(p.ship.class);
    let flee_at = if p.doctrine.flee_pct > 0 {
        (p.ship.fighters + p.ship.shields) * p.doctrine.flee_pct / 100
    } else {
        0
    };
    Force {
        fighters: p.ship.fighters,
        shields: p.ship.shields,
        off_bp: spec.offense_bp,
        def_bp: spec.defense_bp,
        flee_at,
        cannon: 0,
    }
}

/// Destroy `victim`'s ship. Returns a narrative line.
pub fn destroy_ship(ctx: &mut Ctx, victim: PlayerId, killer: Option<PlayerId>, cause: &str) -> String {
    let w = &mut *ctx.w;
    let rules = w.rules.clone();
    let Some(v) = w.players.get(&victim) else { return String::new() };
    let vname = v.name.clone();
    let vkind = v.kind;
    let sector = v.sector;
    let was_pod = v.ship.class == ShipClass::EscapePod;
    let v_exp = v.experience;
    let v_align = v.alignment;
    let on_hand = v.credits;
    let v_cargo = v.ship.cargo;
    let is_skarn = v.npc.as_ref().map(|n| n.role == NpcRole::Skarn).unwrap_or(false);
    let bounty = w.bounties.remove(&victim).map(|b| b.amount).unwrap_or(0);

    let mut line = format!(
        "{vname}'s {} was destroyed ({cause}) in sector {sector}.",
        if was_pod { "escape pod" } else { "ship" }
    );
    if let Some(k) = killer.filter(|k| *k != victim) {
        // Kills between corporations at war cost no alignment and count toward the war.
        let war = crate::diplomacy::at_war(w, k, victim);
        if war {
            crate::diplomacy::war_kill(w, k, victim);
        }
        let salvage = on_hand * rules.salvage_credits_bp / 10_000;
        let kp = w.players.get_mut(&k).expect("killer exists");
        kp.credits += salvage + bounty;
        kp.stats.salvage += salvage;
        kp.stats.kills += 1;
        kp.stats.bounties_collected += bounty;
        kp.experience += 50 + v_exp / 10;
        if is_skarn {
            kp.alignment += 30;
            kp.experience += 100;
        } else if v_align < 0 {
            kp.alignment += 10 + (-v_align) / 4;
        } else if vkind != PlayerKind::Npc && !war {
            kp.alignment -= 25 + v_align / 10;
        }
        if salvage > 0 {
            line.push_str(&format!(" Salvaged {salvage} credits."));
        }
        // Half the victim's cargo survives as salvage, as far as the victor's holds allow.
        if kp.npc.is_none() {
            let mut room = kp.ship.free_holds();
            let mut got = [0i64; 3];
            for i in 0..3 {
                let n = (v_cargo[i] / 2).min(room);
                got[i] = n;
                room -= n;
                kp.ship.cargo[i] += n;
            }
            if got.iter().any(|n| *n > 0) {
                line.push_str(&format!(" Recovered cargo {got:?} (ore/org/equ)."));
            }
        }
        if bounty > 0 {
            line.push_str(&format!(" Bounty of {bounty} credits collected by {}.", kp.name));
        }
    }
    let stardock = rules.stardock_sector;
    let n_sectors = w.sector_count();
    let roll = ctx.rng.next_u64();
    let ferr = crate::npc::skarn_strength(&mut crate::rng::Rng::new(roll), ctx.w);
    let w = &mut *ctx.w;
    let v = w.players.get_mut(&victim).expect("victim exists");
    v.credits = 0;
    v.stats.deaths += 1;
    v.destroyed_count += 1;
    if let Some(brain) = &v.npc {
        // NPCs respawn with a fresh hull somewhere in the frontier and little
        // cash, so a fortified sector cannot farm the same raider for loot.
        let role = brain.role;
        let fed = rules.fedspace_sectors;
        let respawn = if role == NpcRole::FedPatrol {
            brain.home
        } else {
            (fed + 1 + (roll % (n_sectors - fed) as u64) as u32).min(n_sectors)
        };
        v.ship = crate::npc::npc_ship(&rules, role);
        v.sector = respawn;
        if role == NpcRole::Skarn {
            v.ship.fighters = ferr.0;
            v.ship.shields = ferr.1;
        }
        v.prev_sector = respawn;
        v.credits = 1_000;
    } else {
        let name = v.ship.name.clone();
        v.ship = player::new_ship(&rules, ShipClass::EscapePod, &name);
        let pod = rules.ship(ShipClass::EscapePod);
        v.ship.fighters = pod.fighters_max;
        v.ship.shields = pod.shields_max;
        v.sector = stardock;
        v.prev_sector = stardock;
    }
    let players = killer.into_iter().chain([victim]).collect();
    ctx.event(
        "ship_destroyed",
        Some(sector),
        players,
        true,
        line.clone(),
        json!({"victim": vname, "cause": cause}),
    );
    ctx.news(line.clone());
    if let Some(k) = killer.filter(|k| *k != victim) {
        crate::contracts::on_kill(ctx, k, victim);
        if !was_pod {
            crate::careers::fed_reward(ctx, k, is_skarn, v_align);
        }
        if is_skarn {
            crate::events::on_skarn_destroyed(ctx, k, victim);
        }
    }
    ctx.notify(victim, "ship_destroyed", format!("{line} An emergency beacon carried your escape pod to Stardock (sector {stardock}). Bank balance, planets and notes are safe. Buy a new ship with buy_ship (or request_relief if broke)."));
    if let Some(k) = killer {
        ctx.notify(k, "kill", line.clone());
    }
    line
}

/// Move a fleeing ship to a safe adjacent sector (no AP cost). Returns the new sector.
pub fn flee(ctx: &mut Ctx, pid: PlayerId) -> Option<SectorId> {
    let p = ctx.w.players.get(&pid)?;
    let here = p.sector;
    let prev = p.prev_sector;
    if interdicted(ctx.w, pid, here) {
        return None;
    }
    let warps = ctx.w.sector(here)?.warps.clone();
    let safe = |w: &World, s: SectorId| {
        w.sector(s)
            .map(|sec| sec.fighters.as_ref().map(|f| w.friendly(f.owner, pid)).unwrap_or(true) && sec.armid.is_none())
            .unwrap_or(false)
    };
    let dest = if warps.contains(&prev) && safe(ctx.w, prev) {
        prev
    } else {
        *warps.iter().find(|s| safe(ctx.w, **s)).or(warps.first())?
    };
    let p = ctx.player_mut(pid);
    p.prev_sector = here;
    p.sector = dest;
    knowledge::observe(ctx.w, pid, dest, true);
    Some(dest)
}

/// Is a hostile interdictor (citadel 6 with fighters) holding `pid` in sector?
pub fn interdicted(w: &World, pid: PlayerId, sid: SectorId) -> bool {
    interdictor_planet(w, pid, sid).is_some()
}

/// The hostile citadel-6 planet with fighters and enough fuel ore to hold `pid` here.
pub fn interdictor_planet(w: &World, pid: PlayerId, sid: SectorId) -> Option<PlanetId> {
    let s = w.sector(sid)?;
    s.planets
        .iter()
        .filter_map(|id| w.planets.get(id))
        .find(|pl| {
            pl.citadel >= 6
                && pl.fighters > 0
                && pl.stock[0] >= w.rules.interdictor_ore
                && pl.owner.map(|o| !crate::diplomacy::at_peace(w, o, pid)).unwrap_or(false)
        })
        .map(|pl| pl.id)
}

#[derive(Clone, Debug, Default)]
pub struct EntryResult {
    pub destroyed: bool,
    /// Ship ended up somewhere other than the sector it entered.
    pub displaced_to: Option<SectorId>,
    /// Reason an autopilot should stop here.
    pub halt: Option<String>,
    pub log: Vec<String>,
}

/// Resolve all hazards for `pid` entering `sid` from `from`.
pub fn on_enter(ctx: &mut Ctx, pid: PlayerId, sid: SectorId, from: SectorId, blind: bool) -> EntryResult {
    let mut r = EntryResult::default();
    let Some(sector) = ctx.w.sector(sid).cloned() else { return r };
    if let Some(b) = &sector.beacon {
        r.log.push(format!("Beacon (untrusted player text): \"{}\"", b.message));
    }
    if sector.fedspace {
        if let Some(t) = crate::frontier::customs(ctx, pid, sid) {
            r.log.push(t);
        }
        return r;
    }
    // Corporate law (engine 1.36): borders and contraband, then warrants enforced by strength.
    let law = crate::government::on_enter(ctx, pid, sid);
    if let Some(first) = law.first() {
        // An offence or a seizure stops an autopilot so the captain sees it.
        r.halt = Some(first.clone());
    }
    r.log.extend(law);
    let strength = if blind { 2 } else { 1 };
    let rules = ctx.w.rules.clone();
    // Newbie protection covers every automatic attack on entry (engine 1.45): armid mines, the
    // quasar cannon and offensive stacks hold fire, as toll stacks and planet intercepts already did.
    let newbie = is_newbie(ctx.w, pid);
    let pname = ctx.w.player_name(pid);

    // 1. Limpet mines (silent to the victim).
    if let Some(m) = sector.limpet.clone().filter(|m| !ctx.w.friendly(m.owner, pid) && m.count > 0) {
        let p = ctx.player_mut(pid);
        if p.ship.limpets.insert(m.owner) {
            let s = ctx.w.sector_mut(sid).unwrap();
            s.limpet.as_mut().unwrap().count -= 1;
            if s.limpet.as_ref().unwrap().count <= 0 {
                s.limpet = None;
            }
            ctx.notify(
                m.owner,
                "limpet",
                format!("A limpet mine attached to {pname} in sector {sid}. Track them with get_status (limpet_tracks)."),
            );
        }
    }

    // 2. Armid mines.
    if let Some(m) = sector
        .armid
        .clone()
        .filter(|m| !newbie && !crate::diplomacy::at_peace(ctx.w, m.owner, pid) && m.count > 0)
    {
        let n = m.count.min(ctx.rng.range(1, rules.armid_trigger_max) * strength);
        let dmg = n * rules.armid_damage;
        let p = &ctx.w.players[&pid];
        let total = p.ship.shields + p.ship.fighters;
        let s = ctx.w.sector_mut(sid).unwrap();
        s.armid.as_mut().unwrap().count -= n;
        if s.armid.as_ref().unwrap().count <= 0 {
            s.armid = None;
        }
        let owner = m.owner;
        ctx.notify(
            owner,
            "mines",
            format!("{n} of your armid mines detonated on {pname} in sector {sid}."),
        );
        if dmg >= total {
            r.log.push(format!("{n} armid mines detonated for {dmg} damage."));
            destroy_ship(ctx, pid, Some(owner), "armid mines");
            r.destroyed = true;
            return r;
        }
        let mut f = ship_force(ctx.w, &ctx.w.players[&pid]);
        absorb(&mut f, dmg);
        apply_ship_losses(ctx, pid, &f);
        r.log
            .push(format!("{n} armid mines detonated: {dmg} damage absorbed by shields/fighters."));
        r.halt.get_or_insert_with(|| "Hit a minefield.".into());
    }

    // 3-4. Hostile planets: quasar cannon, then fighter intercepts.
    let planet_ids = sector.planets.clone();
    for plid in planet_ids {
        let Some(pl) = ctx.w.planets.get(&plid).cloned() else { continue };
        let Some(owner) = pl.owner else { continue };
        if crate::diplomacy::at_peace(ctx.w, owner, pid) {
            continue;
        }
        let (sector_pct, _) = crate::planet::qcannon_settings(&rules, &pl);
        if pl.citadel >= 3 && pl.stock[0] > 0 && sector_pct > 0 && !newbie {
            // Fuel = ore × sector% ; damage = fuel / 3.
            let ore = (pl.stock[0] * sector_pct / 100).max(1);
            let dmg = ore / 3 * strength;
            ctx.w.planets.get_mut(&plid).unwrap().stock[0] -= ore;
            let p = &ctx.w.players[&pid];
            let total = p.ship.shields + p.ship.fighters;
            ctx.notify(
                owner,
                "defense",
                format!("Quasar cannon on {} fired on {pname} for {dmg} damage.", pl.name),
            );
            if dmg >= total {
                r.log.push(format!("Quasar cannon on planet {} fired for {dmg} damage.", pl.name));
                destroy_ship(ctx, pid, Some(owner), "quasar cannon");
                r.destroyed = true;
                return r;
            }
            let mut f = ship_force(ctx.w, &ctx.w.players[&pid]);
            absorb(&mut f, dmg);
            apply_ship_losses(ctx, pid, &f);
            r.log.push(format!("Quasar cannon on planet {} hit you for {dmg}.", pl.name));
            r.halt.get_or_insert_with(|| "Under planetary cannon fire.".into());
        }
        if pl.citadel >= 2 && pl.fighters > 0 && !is_newbie(ctx.w, pid) {
            let sortie = (pl.fighters / 4).max(1);
            let mut atk = Force {
                fighters: sortie * strength,
                shields: 0,
                off_bp: rules.planet_offense_bp,
                def_bp: rules.planet_defense_bp,
                flee_at: 0,
                cannon: 0,
            };
            let mut def = ship_force(ctx.w, &ctx.w.players[&pid]);
            let start = atk.fighters;
            let b = battle(&mut ctx.rng, &rules, &mut atk, &mut def);
            apply_ship_losses(ctx, pid, &def);
            let lost = ((start - atk.fighters) / strength).min(pl.fighters);
            ctx.w.planets.get_mut(&plid).unwrap().fighters -= lost;
            r.log.push(format!(
                "Planet {} launched {sortie} fighters at you; you lost {} fighters/{} shields.",
                pl.name, b.defender_fighters_lost, b.defender_shields_lost
            ));
            ctx.notify(owner, "defense", format!("Planet {} intercepted {pname} in sector {sid}.", pl.name));
            if b.defender_broken {
                destroy_ship(ctx, pid, Some(owner), "planetary fighters");
                r.destroyed = true;
                return r;
            }
            r.halt.get_or_insert_with(|| "Intercepted by planetary fighters.".into());
        }
    }
    // 5. Sector fighters (toll / offensive / defensive).
    if let Some(stack) = sector
        .fighters
        .clone()
        .filter(|f| !crate::diplomacy::at_peace(ctx.w, f.owner, pid) && f.count > 0)
    {
        let owner_name = ctx.w.player_name(stack.owner);
        match stack.mode {
            FighterMode::Toll => {
                let toll = stack.toll(rules.toll_per_fighter);
                let p = &ctx.w.players[&pid];
                if newbie {
                    r.log
                        .push(format!("Toll fighters ({owner_name}) waved you through: newbie protection."));
                } else if crate::diplomacy::toll_exempt(ctx.w, stack.owner, pid) {
                    r.log.push(format!("Toll fighters ({owner_name}) waved you through: treaty."));
                } else if toll == 0 || (p.doctrine.pay_tolls && toll <= p.doctrine.max_auto_toll && p.credits >= toll) {
                    ctx.player_mut(pid).credits -= toll;
                    ctx.player_mut(pid).stats.tolls_paid += toll;
                    // A shared stack's toll is split by share (the rounding remainder to the owner).
                    for (holder, part) in stack.split(toll) {
                        if let Some(o) = ctx.w.players.get_mut(&holder) {
                            o.credits += part;
                            o.stats.tolls_collected += part;
                        }
                    }
                    // The corporate toll bonus: minted into the owner's corporation as free funds,
                    // on top of the toll (engine 1.32), only in its connected territory (1.40).
                    let bonus = toll * rules.corp_toll_bonus_bp.max(0) / 10_000;
                    let corp =
                        crate::diplomacy::corp_of(ctx.w, stack.owner).filter(|c| bonus > 0 && crate::territory::connected(ctx.w, *c, sid));
                    if let Some(c) = corp.and_then(|c| ctx.w.corps.get_mut(&c)) {
                        c.treasury += bonus;
                        c.record.toll_bonus_total += bonus;
                    }
                    let shared = if stack.shares.is_empty() { "" } else { " (split by share)" };
                    r.log.push(format!("Paid a toll of {toll} credits to {owner_name}'s fighters."));
                    ctx.notify(
                        stack.owner,
                        "toll",
                        format!("{pname} paid {toll} credits toll in sector {sid}{shared}."),
                    );
                } else {
                    let p = ctx.player_mut(pid);
                    p.sector = from;
                    p.prev_sector = sid;
                    r.displaced_to = Some(from);
                    r.halt = Some(format!(
                        "Turned back by {} toll fighters ({owner_name}) demanding {toll} credits (your doctrine/credits did not allow paying).",
                        stack.count
                    ));
                    r.log.push(r.halt.clone().unwrap());
                    knowledge::observe(ctx.w, pid, sid, false);
                    return r;
                }
            }
            FighterMode::Offensive if newbie => {
                r.log
                    .push(format!("Offensive fighters ({owner_name}) held fire: newbie protection."));
            }
            FighterMode::Offensive => {
                let mut atk = Force {
                    fighters: stack.count * strength,
                    shields: 0,
                    off_bp: 10_000,
                    def_bp: 10_000,
                    flee_at: 0,
                    cannon: 0,
                };
                let mut def = ship_force(ctx.w, &ctx.w.players[&pid]);
                let start = atk.fighters;
                let b = battle(&mut ctx.rng, &rules, &mut atk, &mut def);
                let lost_stack = (start - atk.fighters) / strength;
                apply_ship_losses(ctx, pid, &def);
                set_stack(ctx, sid, stack.count - lost_stack);
                ctx.player_mut(stack.owner).stats.fighters_destroyed += b.defender_fighters_lost;
                let msg = format!(
                    "{owner_name}'s offensive fighters attacked: you lost {} fighters and {} shields; they lost {lost_stack}.",
                    b.defender_fighters_lost, b.defender_shields_lost
                );
                r.log.push(msg.clone());
                ctx.notify(
                    stack.owner,
                    "defense",
                    format!("Your fighters in sector {sid} engaged {pname}: {msg}"),
                );
                ctx.event(
                    "combat",
                    Some(sid),
                    vec![pid, stack.owner],
                    true,
                    format!("Offensive fighters of {owner_name} hit {pname} in sector {sid}."),
                    json!({}),
                );
                if b.defender_broken {
                    destroy_ship(ctx, pid, Some(stack.owner), "offensive sector fighters");
                    r.destroyed = true;
                    return r;
                }
                if b.defender_fled
                    && let Some(to) = flee(ctx, pid)
                {
                    r.displaced_to = Some(to);
                    r.halt = Some(format!("Your doctrine ordered a retreat to sector {to}."));
                    r.log.push(r.halt.clone().unwrap());
                    return r;
                }
                r.halt.get_or_insert_with(|| "Engaged by offensive fighters.".into());
            }
            FighterMode::Defensive => {
                r.halt = Some(format!(
                    "{} defensive fighters ({owner_name}) block passage. You may only retreat to sector {from} or attack them (attack_target fighters).",
                    stack.count
                ));
                r.log.push(r.halt.clone().unwrap());
            }
        }
    }

    if interdicted(ctx.w, pid, sid) {
        // Each catch burns fuel ore; an interdictor without fuel lets ships go.
        if let Some(plid) = interdictor_planet(ctx.w, pid, sid) {
            let cost = rules.interdictor_ore;
            ctx.w.planets.get_mut(&plid).unwrap().stock[0] -= cost;
        }
        r.halt = Some("Caught in a hostile Interdictor field: you cannot warp out until the planet's fighters are destroyed or its fuel ore runs out.".into());
        r.log.push(r.halt.clone().unwrap());
    }
    r
}

fn apply_ship_losses(ctx: &mut Ctx, pid: PlayerId, f: &Force) {
    let p = ctx.player_mut(pid);
    p.stats.fighters_lost += (p.ship.fighters - f.fighters).max(0);
    p.ship.fighters = f.fighters.max(0);
    p.ship.shields = f.shields.max(0);
}

/// A sector stack after losses: every holder loses in proportion to its share.
fn set_stack(ctx: &mut Ctx, sid: SectorId, count: i64) {
    let s = ctx.w.sector_mut(sid).unwrap();
    if count <= 0 {
        s.fighters = None;
    } else if let Some(f) = s.fighters.as_mut() {
        f.shrink_to(count);
    }
}

pub fn deploy(ctx: &mut Ctx, pid: PlayerId, a: &DeployArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let sid = p.sector;
    if ctx.w.is_fedspace(sid) {
        return Err(Refusal::new(
            "protected_fedspace",
            "The Federation forbids deploying defenses in FedSpace (sectors 1-10).",
        )
        .hint("Move to a frontier sector first."));
    }
    let cost = if a.item_type == DeployItem::Beacon {
        1
    } else {
        ctx.w.rules.ap_deploy
    };
    let sector = ctx.w.sector(sid).unwrap().clone();
    let msg;
    match a.item_type {
        DeployItem::Fighters => {
            let mode = a.mode;
            if let Some(f) = &sector.fighters
                && !ctx.w.friendly(f.owner, pid)
            {
                return Err(
                    Refusal::new("sector_held", format!("{} hostile fighters already hold this sector.", f.count))
                        .hint("Destroy them first with attack_target(target_type=fighters)."),
                );
            }
            let effective_mode = mode
                .or_else(|| sector.fighters.as_ref().map(|f| f.mode))
                .unwrap_or(FighterMode::Defensive);
            let free_passage = a.toll_price.or_else(|| sector.fighters.as_ref().and_then(|f| f.toll_price)) == Some(0);
            if effective_mode == FighterMode::Toll && !free_passage && p.profession == Some(crate::careers::Career::Marshal) {
                return Err(Refusal::new("marshal", "Federation marshals do not charge tolls.")
                    .hint("For free passage use mode=toll with toll_price=0, or choose defensive/offensive mode."));
            }
            if a.quantity < 0 || a.quantity > p.ship.fighters {
                return Err(Refusal::new(
                    "insufficient_fighters",
                    format!("You carry {} fighters.", p.ship.fighters),
                ));
            }
            if a.quantity == 0 && ((mode.is_none() && a.toll_price.is_none()) || sector.fighters.is_none()) {
                return Err(Refusal::new(
                    "invalid_args",
                    "Deploy at least 1 fighter (or pass quantity 0 with a mode or toll_price to re-task fighters already here).",
                ));
            }
            if let Some(price) = a.toll_price {
                if price < 0 {
                    return Err(Refusal::new("invalid_args", "toll_price must be at least 0."));
                }
                if let Some(f) = &sector.fighters
                    && f.owner != pid
                {
                    return Err(Refusal::new(
                        "not_owner",
                        format!("Only {}, who owns this stack, sets its toll.", ctx.w.player_name(f.owner)),
                    ));
                }
            }
            ctx.require_ap(pid, cost)?;
            ctx.spend_ap(pid, cost);
            ctx.player_mut(pid).ship.fighters -= a.quantity;
            let s = ctx.w.sector_mut(sid).unwrap();
            match s.fighters.as_mut() {
                Some(f) => {
                    f.add(pid, a.quantity);
                    if let Some(m) = mode {
                        f.mode = m;
                    }
                }
                None => s.fighters = Some(FighterStack::new(pid, a.quantity, mode.unwrap_or(FighterMode::Defensive))),
            }
            let f = s.fighters.as_mut().unwrap();
            if a.toll_price.is_some() {
                f.toll_price = a.toll_price;
            }
            let f = f.clone();
            let mut armed_note = "";
            if matches!(f.mode, FighterMode::Offensive | FighterMode::Toll) {
                if crate::player::is_newbie(ctx.w, pid) {
                    armed_note = " Arming the lanes ended your newbie protection.";
                }
                ctx.player_mut(pid).aggressor = true;
            }
            let concession_note = crate::frontier::deploy_note(ctx.w, sid, f.owner, f.count).unwrap_or_default();
            let toll_note = if f.mode == FighterMode::Toll {
                format!(" Toll: {} credits.", f.toll(ctx.w.rules.toll_per_fighter))
            } else {
                String::new()
            };
            let share_note = if f.shares.is_empty() {
                String::new()
            } else {
                format!(
                    " Shared stack: {} of them are yours, and tolls and concession yield are split by share.",
                    f.share_of(pid)
                )
            };
            msg = format!(
                "{} fighters now guard sector {sid} in {} mode.{toll_note}{share_note}{armed_note}{concession_note}",
                f.count,
                format!("{:?}", f.mode).to_lowercase()
            );
            ctx.event(
                "deploy",
                Some(sid),
                vec![pid],
                true,
                format!("{} deployed fighters in sector {sid}.", p.name),
                json!({"count": f.count}),
            );
        }
        DeployItem::ArmidMines | DeployItem::LimpetMines => {
            let armid = a.item_type == DeployItem::ArmidMines;
            let have = if armid { p.ship.armid_mines } else { p.ship.limpet_mines };
            let existing = if armid { &sector.armid } else { &sector.limpet };
            if let Some(m) = existing
                && !ctx.w.friendly(m.owner, pid)
            {
                return Err(Refusal::new("sector_held", "Another player's mines of that type are already here."));
            }
            if a.quantity <= 0 || a.quantity > have {
                return Err(Refusal::new("insufficient_mines", format!("You carry {have} of those mines."))
                    .hint("Buy mines at Stardock with buy_equipment."));
            }
            ctx.require_ap(pid, cost)?;
            ctx.spend_ap(pid, cost);
            let pl = ctx.player_mut(pid);
            if armid {
                pl.ship.armid_mines -= a.quantity;
                // Armid mines are a weapon: laying them ends newbie protection.
                pl.aggressor = true;
            } else {
                pl.ship.limpet_mines -= a.quantity;
            }
            let s = ctx.w.sector_mut(sid).unwrap();
            let slot = if armid { &mut s.armid } else { &mut s.limpet };
            match slot {
                Some(m) => m.count += a.quantity,
                None => {
                    *slot = Some(MineStack {
                        owner: pid,
                        count: a.quantity,
                    })
                }
            }
            msg = format!(
                "Laid {} {} mines in sector {sid}.",
                a.quantity,
                if armid { "armid" } else { "limpet" }
            );
        }
        DeployItem::Beacon => {
            if p.ship.beacons <= 0 {
                return Err(Refusal::new("insufficient_beacons", "You have no beacons.").hint("Buy beacons at Stardock."));
            }
            let text = a.message.clone().unwrap_or_default();
            if text.is_empty() {
                return Err(Refusal::new("invalid_args", "A beacon needs a message."));
            }
            ctx.require_ap(pid, cost)?;
            ctx.spend_ap(pid, cost);
            ctx.player_mut(pid).ship.beacons -= 1;
            ctx.w.sector_mut(sid).unwrap().beacon = Some(Beacon {
                owner: pid,
                message: text.clone(),
            });
            msg = format!("Beacon placed in sector {sid}.");
            ctx.event(
                "beacon",
                Some(sid),
                vec![pid],
                true,
                format!("{} placed a beacon in sector {sid}: {text}", p.name),
                json!({}),
            );
        }
    }
    knowledge::observe(ctx.w, pid, sid, true);
    let reply = json!({"sector": sid, "you": brief(ctx.w, pid)});
    Ok(ctx.finish(msg, reply))
}

pub fn retrieve(ctx: &mut Ctx, pid: PlayerId, a: &RetrieveArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    let sid = p.sector;
    let spec = ctx.w.rules.ship(p.ship.class).clone();
    let sector = ctx.w.sector(sid).unwrap().clone();
    let (have, room) = match a.item_type {
        // Only your own share of a stack: a corp-mate's fighters stay theirs.
        DeployItem::Fighters => match &sector.fighters {
            Some(f) if f.share_of(pid) > 0 => (f.share_of(pid), spec.fighters_max - p.ship.fighters),
            Some(f) if ctx.w.friendly(f.owner, pid) => {
                return Err(Refusal::new(
                    "not_yours",
                    "None of these fighters are yours: you can retrieve only what you deployed.",
                ));
            }
            _ => return Err(Refusal::new("nothing_here", "No fighters of yours in this sector.")),
        },
        DeployItem::ArmidMines => match &sector.armid {
            Some(m) if ctx.w.friendly(m.owner, pid) => (m.count, spec.mines_max - p.ship.armid_mines - p.ship.limpet_mines),
            _ => return Err(Refusal::new("nothing_here", "No armid mines of yours in this sector.")),
        },
        _ => return Err(Refusal::new("invalid_args", "Only fighters and armid mines can be retrieved.")),
    };
    let n = a.quantity.unwrap_or(have).min(have).min(room.max(0));
    if n <= 0 {
        return Err(Refusal::new("no_room", "Your ship has no room for more of those."));
    }
    ctx.require_ap(pid, 1)?;
    ctx.spend_ap(pid, 1);
    let s = ctx.w.sector_mut(sid).unwrap();
    match a.item_type {
        DeployItem::Fighters => {
            let f = s.fighters.as_mut().unwrap();
            f.take(pid, n);
            if f.count == 0 {
                s.fighters = None;
            }
            ctx.player_mut(pid).ship.fighters += n;
        }
        _ => {
            let m = s.armid.as_mut().unwrap();
            m.count -= n;
            if m.count == 0 {
                s.armid = None;
            }
            ctx.player_mut(pid).ship.armid_mines += n;
        }
    }
    let reply = json!({"retrieved": n, "you": brief(ctx.w, pid)});
    Ok(ctx.finish(format!("Retrieved {n} from sector {sid}."), reply))
}

pub fn attack(ctx: &mut Ctx, pid: PlayerId, a: &AttackArgs) -> Res<Outcome> {
    let me = ctx.player(pid)?.clone();
    let sid = me.sector;
    if ctx.w.is_fedspace(sid) {
        return Err(Refusal::new(
            "protected_fedspace",
            "Combat is forbidden in FedSpace (sectors 1-10). Federation police enforce this.",
        )
        .hint("Find targets in the frontier (sector ids above 10); density scans reveal ships."));
    }
    let committed = a.fighters.unwrap_or(me.ship.fighters).min(me.ship.fighters);
    if committed <= 0 {
        return Err(Refusal::new("insufficient_fighters", "You have no fighters to commit.")
            .hint("Buy fighters at Stardock (buy_equipment item=fighters)."));
    }
    let rules = ctx.w.rules.clone();
    let spec = rules.ship(me.ship.class).clone();
    let tactic = a.tactic.unwrap_or(Tactic::Destroy);
    match a.target_type {
        TargetType::Ship => {
            let tid = a
                .target_id
                .as_deref()
                .ok_or_else(|| Refusal::new("invalid_args", "target_id (player name or id) is required."))?;
            let target = player::resolve(ctx.w, tid)
                .filter(|t| t.sector == sid && t.id != pid)
                .cloned()
                .ok_or_else(|| {
                    Refusal::new("target_unavailable", "No such ship in this sector.").hint("view_sector lists ships present.")
                })?;
            if ctx.w.friendly(pid, target.id) {
                return Err(Refusal::new("friendly", "That ship belongs to your corporation."));
            }
            if is_newbie(ctx.w, target.id) {
                return Err(Refusal::new(
                    "protected_newbie",
                    format!("{} is under Federation newbie protection.", target.name),
                )
                .hint(format!(
                    "Protection lasts until {} experience or {} ticks of age, or until they act aggressively.",
                    rules.newbie_exp, rules.newbie_ticks
                )));
            }
            let tspec = rules.ship(target.ship.class).clone();
            if tactic == Tactic::Board {
                let need = rules.board_ratio_bp * (target.ship.fighters * tspec.defense_bp / 10_000 + target.ship.shields) / 10_000;
                if committed * spec.offense_bp / 10_000 < need {
                    return Err(Refusal::new(
                        "insufficient_force",
                        format!("Boarding needs at least {need} effective fighters; you committed {committed}."),
                    )
                    .hint("Commit more fighters or use tactic=destroy."));
                }
            }
            ctx.require_ap(pid, rules.ap_attack)?;
            ctx.spend_ap(pid, rules.ap_attack);
            ctx.player_mut(pid).aggressor = true;
            ctx.player_mut(pid).relief_until = 0;
            ctx.player_mut(pid).last_combat_tick = ctx.w.tick;
            ctx.player_mut(target.id).last_combat_tick = ctx.w.tick;
            let war = crate::diplomacy::at_war(ctx.w, pid, target.id);
            if target.alignment >= 0 && target.kind != PlayerKind::Npc && !war {
                ctx.player_mut(pid).alignment -= 10;
            }
            let mut atk = Force {
                fighters: committed,
                shields: 0,
                off_bp: spec.offense_bp,
                def_bp: spec.defense_bp,
                flee_at: 0,
                cannon: 0,
            };
            let mut def = ship_force(ctx.w, &target);
            let b = battle(&mut ctx.rng, &rules, &mut atk, &mut def);
            ctx.player_mut(pid).ship.fighters -= b.attacker_lost;
            ctx.player_mut(pid).stats.fighters_lost += b.attacker_lost;
            ctx.player_mut(pid).stats.fighters_destroyed += b.defender_fighters_lost;
            apply_ship_losses(ctx, target.id, &def);
            let mut summary = format!(
                "Battle with {} ({} rounds): you lost {} fighters; they lost {} fighters and {} shields.",
                target.name, b.rounds, b.attacker_lost, b.defender_fighters_lost, b.defender_shields_lost
            );
            if target.doctrine.distress
                && let Some(cid) = target.corp
            {
                let call = format!("DISTRESS: {} is under attack by {} in sector {sid}!", target.name, me.name);
                crate::social::system_radio(ctx, Channel::Corp(cid), call.clone());
                // Mutual-defense partners hear it too.
                for partner in crate::diplomacy::defense_partners(ctx.w, cid) {
                    crate::social::system_radio(ctx, Channel::Corp(partner), format!("{call} (mutual-defense ally)"));
                }
            }
            if b.defender_broken {
                if tactic == Tactic::Board {
                    let loot = target.credits * crate::careers::board_share_bp(&me) / 10_000;
                    let mut taken = [0i64; 3];
                    let free = ctx.w.players[&pid].ship.free_holds();
                    let mut room = free;
                    for i in 0..3 {
                        taken[i] = target.ship.cargo[i].min(room);
                        room -= taken[i];
                    }
                    let t = ctx.player_mut(target.id);
                    t.credits -= loot;
                    for i in 0..3 {
                        t.ship.cargo[i] -= taken[i];
                    }
                    let me_mut = ctx.player_mut(pid);
                    me_mut.credits += loot;
                    for i in 0..3 {
                        me_mut.ship.cargo[i] += taken[i];
                    }
                    if !war {
                        me_mut.alignment -= 15;
                    }
                    summary.push_str(&format!(" Boarded! Seized {loot} credits and cargo {taken:?} (ore/org/equ)."));
                    ctx.notify(
                        target.id,
                        "boarded",
                        format!("{} boarded your ship in sector {sid} and seized {loot} credits and cargo.", me.name),
                    );
                    ctx.event(
                        "boarding",
                        Some(sid),
                        vec![pid, target.id],
                        true,
                        format!("{} boarded {} in sector {sid}.", me.name, target.name),
                        json!({}),
                    );
                } else {
                    summary.push(' ');
                    summary.push_str(&destroy_ship(ctx, target.id, Some(pid), &format!("attacked by {}", me.name)));
                }
            } else if b.defender_fled {
                let to = flee(ctx, target.id);
                summary.push_str(&format!(
                    " {} fled{}.",
                    target.name,
                    to.map(|s| format!(" to sector {s}")).unwrap_or_default()
                ));
                ctx.notify(
                    target.id,
                    "combat",
                    format!(
                        "{} attacked you in sector {sid}. Your doctrine ordered a retreat{}. Losses: {} fighters, {} shields.",
                        me.name,
                        to.map(|s| format!(" to sector {s}")).unwrap_or_default(),
                        b.defender_fighters_lost,
                        b.defender_shields_lost
                    ),
                );
            } else {
                ctx.notify(
                    target.id,
                    "combat",
                    format!(
                        "{} attacked you in sector {sid}. You lost {} fighters and {} shields; they lost {}.",
                        me.name, b.defender_fighters_lost, b.defender_shields_lost, b.attacker_lost
                    ),
                );
            }
            ctx.event(
                "combat",
                Some(sid),
                vec![pid, target.id],
                true,
                format!("{} attacked {} in sector {sid}.", me.name, target.name),
                json!({"rounds": b.rounds, "attacker_lost": b.attacker_lost, "defender_lost": b.defender_fighters_lost}),
            );
            let reply = json!({"battle": battle_json(&b), "target_destroyed": b.defender_broken && tactic == Tactic::Destroy, "you": brief(ctx.w, pid)});
            Ok(ctx.finish(summary, reply))
        }
        TargetType::Fighters => {
            let stack = ctx
                .w
                .sector(sid)
                .unwrap()
                .fighters
                .clone()
                .filter(|f| !ctx.w.friendly(f.owner, pid))
                .ok_or_else(|| Refusal::new("target_unavailable", "There are no hostile fighters in this sector."))?;
            ctx.require_ap(pid, rules.ap_attack)?;
            ctx.spend_ap(pid, rules.ap_attack);
            let mut atk = Force {
                fighters: committed,
                shields: 0,
                off_bp: spec.offense_bp,
                def_bp: spec.defense_bp,
                flee_at: 0,
                cannon: 0,
            };
            let mut def = Force {
                fighters: stack.count,
                shields: 0,
                off_bp: 10_000,
                def_bp: 10_000,
                flee_at: 0,
                cannon: 0,
            };
            let b = battle(&mut ctx.rng, &rules, &mut atk, &mut def);
            ctx.player_mut(pid).ship.fighters -= b.attacker_lost;
            ctx.player_mut(pid).stats.fighters_lost += b.attacker_lost;
            ctx.player_mut(pid).stats.fighters_destroyed += b.defender_fighters_lost;
            ctx.player_mut(stack.owner).stats.fighters_lost += b.defender_fighters_lost;
            set_stack(ctx, sid, def.fighters);
            let owner = ctx.w.player_name(stack.owner);
            let summary = if def.fighters <= 0 {
                ctx.news(format!("{} destroyed {owner}'s fighter blockade in sector {sid}.", me.name));
                format!(
                    "Destroyed all {} of {owner}'s fighters (lost {} of yours). Sector {sid} is clear.",
                    stack.count, b.attacker_lost
                )
            } else {
                format!(
                    "Destroyed {} of {owner}'s fighters, {} remain. You lost {}.",
                    b.defender_fighters_lost, def.fighters, b.attacker_lost
                )
            };
            ctx.notify(
                stack.owner,
                "defense",
                format!(
                    "{} attacked your fighters in sector {sid}: {} destroyed, {} remain.",
                    me.name,
                    b.defender_fighters_lost,
                    def.fighters.max(0)
                ),
            );
            ctx.event(
                "combat",
                Some(sid),
                vec![pid, stack.owner],
                true,
                format!("{} attacked {owner}'s fighters in sector {sid}.", me.name),
                json!({}),
            );
            knowledge::observe(ctx.w, pid, sid, true);
            let reply = json!({"battle": battle_json(&b), "remaining_enemy_fighters": def.fighters.max(0), "you": brief(ctx.w, pid)});
            Ok(ctx.finish(summary, reply))
        }
        TargetType::Planet => {
            let plid: PlanetId = a
                .target_id
                .as_deref()
                .and_then(|s| s.trim().trim_start_matches(['P', 'L', 'p', 'l', '#']).parse().ok())
                .ok_or_else(|| Refusal::new("invalid_args", "target_id must be a planet id."))?;
            let pl = ctx
                .w
                .planets
                .get(&plid)
                .filter(|p| p.sector == sid)
                .cloned()
                .ok_or_else(|| Refusal::new("target_unavailable", "No such planet in this sector."))?;
            let owner = pl
                .owner
                .ok_or_else(|| Refusal::new("unowned", "That planet is unclaimed.").hint("Use claim_planet instead."))?;
            if ctx.w.friendly(owner, pid) {
                return Err(Refusal::new("friendly", "That planet belongs to you or your corporation."));
            }
            let cost = rules.ap_attack + 2;
            ctx.require_ap(pid, cost)?;
            ctx.spend_ap(pid, cost);
            ctx.player_mut(pid).aggressor = true;
            ctx.player_mut(pid).relief_until = 0;
            // Atmospheric quasar: damage = ore × atmo% per round, burning damage/2 ore.
            let (_, atmo_pct) = crate::planet::qcannon_settings(&rules, &pl);
            let cannon = if pl.citadel >= 3 { pl.stock[0] * atmo_pct / 100 } else { 0 };
            let ore_per_round = cannon / 2;
            // Planet shields: each absorbs `planet_shield_strength` fighters' worth (TW 20:1).
            let ss = rules.planet_shield_strength.max(1);
            let mut atk = Force {
                fighters: committed,
                shields: 0,
                off_bp: spec.offense_bp,
                def_bp: spec.defense_bp,
                flee_at: 0,
                cannon: 0,
            };
            let mut def = Force {
                fighters: pl.fighters,
                shields: pl.shields * ss,
                off_bp: rules.planet_offense_bp,
                def_bp: rules.planet_defense_bp,
                flee_at: 0,
                cannon,
            };
            let b = battle(&mut ctx.rng, &rules, &mut atk, &mut def);
            ctx.player_mut(pid).ship.fighters -= b.attacker_lost;
            ctx.player_mut(pid).stats.fighters_lost += b.attacker_lost;
            ctx.player_mut(pid).stats.fighters_destroyed += b.defender_fighters_lost;
            let owner_name = ctx.w.player_name(owner);
            let plm = ctx.w.planets.get_mut(&plid).unwrap();
            plm.fighters = def.fighters.max(0);
            plm.shields = (def.shields.max(0) + ss - 1) / ss;
            plm.stock[0] -= (ore_per_round * b.rounds as i64).min(plm.stock[0]);
            let mut summary = format!(
                "Assaulted planet {} ({} rounds): destroyed {} fighters and {} shields; you lost {}.",
                pl.name,
                b.rounds,
                b.defender_fighters_lost,
                b.defender_shields_lost / ss,
                b.attacker_lost
            );
            if b.defender_broken {
                let plunder = plm.treasury / 2;
                plm.treasury -= plunder;
                plm.owner = Some(pid);
                plm.project = None;
                ctx.player_mut(pid).credits += plunder;
                ctx.player_mut(pid).experience += 100 + 50 * pl.citadel as i64;
                summary.push_str(&format!(" Planet captured! Plundered {plunder} credits from its treasury."));
                ctx.news(format!(
                    "{} captured planet {} (citadel {}) from {owner_name} in sector {sid}!",
                    me.name, pl.name, pl.citadel
                ));
                ctx.notify(
                    owner,
                    "planet_lost",
                    format!("{} captured your planet {} in sector {sid}.", me.name, pl.name),
                );
                ctx.event(
                    "planet_captured",
                    Some(sid),
                    vec![pid, owner],
                    true,
                    format!("{} captured planet {} from {owner_name}.", me.name, pl.name),
                    json!({"planet": plid}),
                );
            } else {
                ctx.notify(
                    owner,
                    "planet_attacked",
                    format!(
                        "{} attacked your planet {} in sector {sid}; it holds with {} fighters.",
                        me.name, pl.name, def.fighters
                    ),
                );
                ctx.event(
                    "combat",
                    Some(sid),
                    vec![pid, owner],
                    true,
                    format!("{} assaulted planet {}.", me.name, pl.name),
                    json!({}),
                );
            }
            let reply = json!({"battle": battle_json(&b), "captured": b.defender_broken, "you": brief(ctx.w, pid)});
            Ok(ctx.finish(summary, reply))
        }
    }
}

fn battle_json(b: &Battle) -> serde_json::Value {
    json!({
        "rounds": b.rounds,
        "your_fighters_lost": b.attacker_lost,
        "enemy_fighters_destroyed": b.defender_fighters_lost,
        "enemy_shields_destroyed": b.defender_shields_lost,
        "enemy_fled": b.defender_fled,
        "enemy_defeated": b.defender_broken,
        "you_were_repelled": b.attacker_broken,
    })
}
