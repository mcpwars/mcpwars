//! Non-player factions, driven deterministically by the tick engine.
//! Skarn raid weak traders in the frontier; merchant convoys roam port
//! lanes (fat targets for corsairs); Federation patrols hunt the wanted.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::combat::{self, Force};
use crate::engine::*;
use crate::model::*;
use crate::player;
use crate::rng::Rng;
use crate::rules::Rules;

const SKARN_NAMES: [&str; 12] = [
    "Vorrak", "Skell", "Mardo", "Quist", "Haddix", "Trell", "Osk", "Pyrr", "Drelvo", "Kavvo", "Thune", "Zabb",
];
const TRADER_NAMES: [&str; 10] = [
    "Vega Line",
    "Orion Freight",
    "Cygnus Haulage",
    "Lyra Mercantile",
    "Draco Shipping",
    "Altair Cargo",
    "Rigel Transit",
    "Deneb Supply",
    "Sirius Logistics",
    "Antares Trade",
];
const FED_NAMES: [&str; 3] = ["FSS Vigilant", "FSS Resolute", "FSS Steadfast"];

/// Game age in TW-equivalent days: the season is mapped onto
/// `skarn_full_strength_days` (TW Skarn reach full strength at day 120).
pub fn skarn_age_days(w: &World) -> i64 {
    let season = w.rules.season_ticks.max(1) as i64;
    let age = w.tick.saturating_sub(w.season.started_tick) as i64;
    (age * w.rules.skarn_full_strength_days / season).clamp(0, w.rules.skarn_full_strength_days)
}

/// Raider strength: fighters = rand(20·T)+30, shields = rand(10·T).
pub fn skarn_strength(rng: &mut Rng, w: &World) -> (i64, i64) {
    let t = skarn_age_days(w);
    ((rng.range(0, 20 * t) + 30).max(w.rules.skarn_min_fighters), rng.range(0, 10 * t))
}

/// How many Skarn should exist now: grows from 1 to the full count with game age.
pub fn skarn_target(w: &World) -> u32 {
    let full = (w.sector_count() * w.rules.skarn_per_100_sectors / 100).max(1) as i64;
    let t = skarn_age_days(w);
    let floor = w.sector_count() * w.rules.skarn_floor_per_100_sectors / 100;
    ((1 + (full - 1) * t / w.rules.skarn_full_strength_days.max(1)) as u32).max(floor)
}

pub fn npc_ship(rules: &Rules, role: NpcRole) -> Ship {
    let (class, f, s) = match role {
        NpcRole::Skarn => (ShipClass::Battleship, 600, 300),
        NpcRole::Trader => (ShipClass::CargoHauler, 60, 300),
        NpcRole::FedPatrol => (ShipClass::CorporateFlagship, 8_000, 4_000),
    };
    let mut ship = player::new_ship(rules, class, "");
    ship.fighters = f;
    ship.shields = s;
    if role == NpcRole::Trader {
        ship.holds = 120;
        ship.cargo = [40, 40, 40];
    }
    ship
}

pub fn make_npc(w: &mut World, name: String, role: NpcRole, sector: SectorId, rng: &mut Rng) -> PlayerId {
    let id = w.next_player;
    w.next_player += 1;
    let mut ship = npc_ship(&w.rules, role);
    ship.name = name.clone();
    if role == NpcRole::Skarn {
        let (f, sh) = skarn_strength(rng, w);
        ship.fighters = f;
        ship.shields = sh;
    }
    let credits = match role {
        NpcRole::Skarn => rng.range(3_000, 9_000),
        NpcRole::Trader => rng.range(8_000, 20_000),
        NpcRole::FedPatrol => 0,
    };
    w.players.insert(
        id,
        Player {
            announcements_seen: 0,
            harness: None,
            autonomous: false,
            recovery_hash: String::new(),
            banned: false,
            id,
            name,
            kind: PlayerKind::Npc,
            token_hash: String::new(),
            model: Some(format!("npc:{role:?}").to_lowercase()),
            created_tick: 0,
            sector,
            prev_sector: sector,
            ship,
            credits,
            bank: 0,
            ap: 0,
            experience: 500,
            alignment: match role {
                NpcRole::Skarn => -2_000,
                NpcRole::Trader => 200,
                NpcRole::FedPatrol => 10_000,
            },
            corp: None,
            knowledge: BTreeMap::new(),
            inbox: vec![],
            next_notice: 1,
            notes: BTreeMap::new(),
            memory: BTreeMap::new(),
            memory_revisions: BTreeMap::new(),
            plan_tick: None,
            last_action_tick: 0,
            last_combat_tick: 0,
            stats: PlayerStats::default(),
            career: PlayerStats::default(),
            seasons_played: 0,
            best_finish: None,
            doctrine: Doctrine {
                max_auto_toll: 5_000,
                pay_tolls: true,
                flee_pct: if role == NpcRole::Trader { 50 } else { 0 },
                distress: false,
            },
            aggressor: true,
            fed_paid_today: (0, 0),
            relief_until: 0,
            day_start_worth: None,
            wired_today: 0,
            society: Vec::new(),
            muted_until: 0,
            endowment: 0,
            owner_token_hash: String::new(),
            owner: Default::default(),
            profession: None,
            profession_since: 0,
            win_condition: None,
            destroyed_count: 0,
            radio_cursor: 0,
            radio_read_ids: Default::default(),
            founding_grant_season: 0,
            npc: Some(NpcBrain {
                role,
                home: sector,
                target: None,
            }),
        },
    );
    id
}

pub fn spawn_npcs(w: &mut World, rng: &mut Rng) {
    let n = w.sector_count();
    let fed = w.rules.fedspace_sectors;
    let frontier: Vec<SectorId> = (fed + 1..=n).collect();
    if frontier.is_empty() {
        return;
    }
    // Skarn start as a single weak raider and multiply as the season ages (see act()).
    let skarn = 1;
    for i in 0..skarn {
        let s = *rng.pick(&frontier).unwrap();
        let name = format!("Skarn {}", SKARN_NAMES[i as usize % SKARN_NAMES.len()]);
        let name = if i as usize >= SKARN_NAMES.len() {
            format!("{name} {}", i)
        } else {
            name
        };
        make_npc(w, name, NpcRole::Skarn, s, rng);
    }
    let port_sectors: Vec<SectorId> = w.sectors.iter().filter(|s| s.port.is_some() && !s.fedspace).map(|s| s.id).collect();
    let traders = (n * w.rules.traders_per_100_sectors / 100).max(1);
    for i in 0..traders {
        let s = *rng.pick(&port_sectors).unwrap_or(&frontier[0]);
        let name = TRADER_NAMES[i as usize % TRADER_NAMES.len()].to_string();
        let name = if i as usize >= TRADER_NAMES.len() {
            format!("{name} {}", i)
        } else {
            name
        };
        make_npc(w, name, NpcRole::Trader, s, rng);
    }
    for i in 0..w.rules.fed_patrols.min(3) {
        let s = 1 + (i % fed);
        make_npc(w, FED_NAMES[i as usize].to_string(), NpcRole::FedPatrol, s, rng);
    }
}

/// NPC traders for a new frontier ring: `count` of them at random ports among `ports`, named on
/// from the traders already flying. Returns how many were added.
pub fn add_traders(w: &mut World, rng: &mut Rng, ports: &[SectorId], count: u32) -> u32 {
    if ports.is_empty() {
        return 0;
    }
    let flying = w
        .players
        .values()
        .filter(|p| p.npc.as_ref().is_some_and(|n| n.role == NpcRole::Trader))
        .count();
    for k in 0..count as usize {
        let i = flying + k;
        let name = TRADER_NAMES[i % TRADER_NAMES.len()].to_string();
        let name = if i >= TRADER_NAMES.len() { format!("{name} {i}") } else { name };
        let s = *rng.pick(ports).unwrap();
        make_npc(w, name, NpcRole::Trader, s, rng);
    }
    count
}

fn wander(ctx: &mut Ctx, pid: PlayerId, allow: impl Fn(&World, SectorId) -> bool) {
    let here = ctx.w.players[&pid].sector;
    let opts: Vec<SectorId> = ctx
        .w
        .sector(here)
        .unwrap()
        .warps
        .iter()
        .copied()
        .filter(|s| allow(ctx.w, *s))
        .collect();
    if let Some(&to) = ctx.rng.pick(&opts) {
        crate::nav::hop(ctx, pid, to, 0, false);
    }
}

/// Skarn regrow toward the average strength for the current game age.
fn rearm(ctx: &mut Ctx, id: PlayerId) {
    let t = skarn_age_days(ctx.w);
    let (avg_f, avg_s) = ((10 * t + 30).max(ctx.w.rules.skarn_min_fighters), 5 * t);
    let p = ctx.player_mut(id);
    if p.ship.fighters < avg_f {
        p.ship.fighters += ((avg_f - p.ship.fighters) / 20).max(1);
    }
    if p.ship.shields < avg_s {
        p.ship.shields += ((avg_s - p.ship.shields) / 20).max(1);
    }
}

/// Spawn one more Skarn raider when the population is below the age target.
fn grow_skarn(ctx: &mut Ctx) {
    let have = ctx
        .w
        .players
        .values()
        .filter(|p| p.npc.as_ref().map(|b| b.role == NpcRole::Skarn).unwrap_or(false))
        .count() as u32;
    if have >= skarn_target(ctx.w) {
        return;
    }
    spawn_skarn(ctx, None);
}

/// Spawn one Skarn raider in `sector`, or a random unguarded frontier sector.
pub fn spawn_skarn(ctx: &mut Ctx, sector: Option<SectorId>) -> Option<PlayerId> {
    let have = ctx
        .w
        .players
        .values()
        .filter(|p| p.npc.as_ref().map(|b| b.role == NpcRole::Skarn).unwrap_or(false))
        .count() as u32;
    let s = match sector {
        Some(s) => s,
        None => {
            let n = ctx.w.sector_count();
            let fed = ctx.w.rules.fedspace_sectors;
            let frontier: Vec<SectorId> = (fed + 1..=n)
                .filter(|s| !guarded(ctx.w, *s, 0) && !ctx.w.is_fed_buffer(*s))
                .collect();
            *ctx.rng.pick(&frontier)?
        }
    };
    let i = have as usize;
    let base = format!("Skarn {}", SKARN_NAMES[i % SKARN_NAMES.len()]);
    let name = if i >= SKARN_NAMES.len() {
        format!("{base} {}", i / SKARN_NAMES.len() + 1)
    } else {
        base
    };
    let mut rng = ctx.rng.clone();
    let id = make_npc(ctx.w, name.clone(), NpcRole::Skarn, s, &mut rng);
    ctx.rng = rng;
    let f = ctx.w.players[&id].ship.fighters;
    ctx.news(format!("Long-range scans detect a new Skarn raider, {name}, with {f} fighters."));
    ctx.event(
        "npc_spawn",
        Some(s),
        vec![id],
        true,
        format!("{name} entered the frontier."),
        json!({}),
    );
    Some(id)
}

pub fn act(ctx: &mut Ctx) {
    grow_skarn(ctx);
    let npcs: Vec<(PlayerId, NpcRole)> = ctx
        .w
        .players
        .values()
        .filter_map(|p| p.npc.as_ref().map(|b| (p.id, b.role)))
        .collect();
    for (id, role) in npcs {
        match role {
            NpcRole::Skarn => skarn(ctx, id),
            NpcRole::Trader => {
                if ctx.rng.chance(0.4) {
                    wander(ctx, id, |w, s| !w.is_fedspace(s));
                    // Restock cargo when visiting a port (they are prey, not market makers).
                    let here = ctx.w.players[&id].sector;
                    if ctx.w.sector(here).unwrap().port.is_some() {
                        let p = ctx.player_mut(id);
                        p.ship.cargo = [40, 40, 40];
                        p.credits = (p.credits + 300).min(30_000);
                    }
                }
            }
            NpcRole::FedPatrol => fed_patrol(ctx, id),
        }
    }
}

/// Raiders avoid sectors guarded by player fighters or mines that outgun a raider of `strength`
/// fighters (0: any defence at all), or by armed planets (citadel 2+). A token fighter no
/// longer keeps them out.
fn guarded(w: &World, s: SectorId, strength: i64) -> bool {
    let Some(sec) = w.sector(s) else { return true };
    let player_owned = |o: PlayerId| w.players.get(&o).map(|p| p.npc.is_none()).unwrap_or(false);
    sec.fighters.as_ref().is_some_and(|f| player_owned(f.owner) && f.count >= strength)
        || sec
            .armid
            .as_ref()
            .is_some_and(|m| player_owned(m.owner) && m.count * w.rules.armid_damage >= strength)
        || sec
            .planets
            .iter()
            .filter_map(|id| w.planets.get(id))
            .any(|pl| pl.owner.is_some() && pl.citadel >= 2)
}

fn skarn(ctx: &mut Ctx, id: PlayerId) {
    if ctx.rng.chance(0.35) {
        let strength = ctx.w.players[&id].ship.fighters;
        wander(ctx, id, move |w, s| !w.is_fedspace(s) && !guarded(w, s, strength));
    }
    let me = ctx.w.players[&id].clone();
    let sid = me.sector;
    if ctx.w.is_fedspace(sid) {
        return;
    }
    let strength = me.ship.fighters;
    let prey: Vec<PlayerId> = ctx
        .w
        .players
        .values()
        .filter(|p| p.sector == sid && p.kind != PlayerKind::Npc && p.ship.class != ShipClass::EscapePod)
        .filter(|p| !player::is_newbie(ctx.w, p.id))
        // Federation relief: a few hours' grace to arm.
        .filter(|p| ctx.w.tick >= p.relief_until)
        // Don't farm the same (possibly offline) pilot: 90-tick grace after any fight.
        .filter(|p| p.last_combat_tick == 0 || ctx.w.tick >= p.last_combat_tick + 90)
        .filter(|p| (p.ship.fighters + p.ship.shields) * 10 < strength * 8)
        .map(|p| p.id)
        .collect();
    let Some(&target) = ctx.rng.pick(&prey) else { return };
    if !ctx.rng.chance(0.6) {
        return;
    }
    let rules = ctx.w.rules.clone();
    let t = ctx.w.players[&target].clone();
    let mut atk = combat::ship_force(ctx.w, &me);
    atk.flee_at = 0;
    let mut def: Force = combat::ship_force(ctx.w, &t);
    let b = combat::battle(&mut ctx.rng, &rules, &mut atk, &mut def);
    ctx.player_mut(id).ship.fighters = atk.fighters.max(0);
    let now = ctx.w.tick;
    let tp = ctx.player_mut(target);
    tp.stats.fighters_lost += tp.ship.fighters - def.fighters.max(0);
    tp.ship.fighters = def.fighters.max(0);
    tp.ship.shields = def.shields.max(0);
    tp.last_combat_tick = now;
    let mut line = format!("{} ambushed {} in sector {sid}", me.name, t.name);
    if b.defender_broken {
        if ctx.rng.chance(0.7) {
            let loot = t.credits / 2;
            let cargo = t.ship.cargo;
            let tp = ctx.player_mut(target);
            tp.credits -= loot;
            tp.ship.cargo = [0; 3];
            tp.ship.cargo_cost = [0; 3];
            ctx.player_mut(id).credits += loot;
            line.push_str(&format!(" and plundered {loot} credits and cargo {cargo:?}."));
            // The stripped ship limps away instead of sitting in the raider's sector.
            let fled = combat::flee(ctx, target);
            ctx.notify(target, "skarn", format!("{line} Your ship survived with no fighters or shields and limped to sector {}. Carry more fighters in the frontier or raise flee_threshold_pct.",
                fled.map(|s| s.to_string()).unwrap_or_else(|| "nowhere (trapped)".into())));
        } else {
            line.push('.');
            combat::destroy_ship(ctx, target, Some(id), "Skarn raid");
        }
    } else if b.defender_fled {
        let to = combat::flee(ctx, target);
        line.push_str(&format!(
            "; {} fled{}.",
            t.name,
            to.map(|s| format!(" to sector {s}")).unwrap_or_default()
        ));
        ctx.notify(
            target,
            "skarn",
            format!(
                "{line} Losses: {} fighters, {} shields.",
                b.defender_fighters_lost, b.defender_shields_lost
            ),
        );
    } else {
        line.push_str(&format!("; the attack was beaten off ({} Skarn fighters lost).", b.attacker_lost));
        ctx.notify(target, "skarn", line.clone());
    }
    ctx.event("skarn_raid", Some(sid), vec![id, target], true, line.clone(), json!({}));
    ctx.news(line);
    rearm(ctx, id);
}

fn fed_patrol(ctx: &mut Ctx, id: PlayerId) {
    let rules = ctx.w.rules.clone();
    let fed = rules.fedspace_sectors;
    // Sectors under Federation jurisdiction: FedSpace and its immediate neighbours.
    let mut juris: BTreeSet<SectorId> = (1..=fed).collect();
    for s in 1..=fed {
        if let Some(sec) = ctx.w.sector(s) {
            juris.extend(sec.warps.iter().copied());
        }
    }
    let wanted: Vec<PlayerId> = ctx
        .w
        .players
        .values()
        .filter(|p| {
            p.kind != PlayerKind::Npc
                && p.alignment <= rules.fed_wanted_alignment
                && juris.contains(&p.sector)
                && p.ship.class != ShipClass::EscapePod
        })
        .map(|p| p.id)
        .collect();
    if let Some(&target) = wanted.first() {
        let ts = ctx.w.players[&target].sector;
        let me = ctx.player_mut(id);
        me.prev_sector = me.sector;
        me.sector = ts;
        let me = ctx.w.players[&id].clone();
        let t = ctx.w.players[&target].clone();
        let mut atk = combat::ship_force(ctx.w, &me);
        let mut def = combat::ship_force(ctx.w, &t);
        def.flee_at = 0;
        let b = combat::battle(&mut ctx.rng, &rules, &mut atk, &mut def);
        let tp = ctx.player_mut(target);
        tp.ship.fighters = def.fighters.max(0);
        tp.ship.shields = def.shields.max(0);
        ctx.player_mut(id).ship = npc_ship(&rules, NpcRole::FedPatrol);
        let line = format!("{} intercepted the wanted criminal {} in sector {ts}", me.name, t.name);
        if b.defender_broken {
            combat::destroy_ship(ctx, target, Some(id), "Federation justice");
        } else {
            ctx.notify(
                target,
                "federation",
                format!("{line}. You survived: leave Federation jurisdiction."),
            );
        }
        ctx.event("fed_intercept", Some(ts), vec![id, target], true, format!("{line}."), json!({}));
    } else if ctx.rng.chance(0.3) {
        wander(ctx, id, move |_, s| s <= fed);
    }
}
