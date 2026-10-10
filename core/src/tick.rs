//! The periodic world tick: AP regeneration, markets, planets, NPCs, season.

use serde_json::json;

use crate::economy;
use crate::engine::*;
use crate::model::*;
use crate::player;

pub fn tick(ctx: &mut Ctx) -> Res<Outcome> {
    ctx.w.tick += 1;
    let t = ctx.w.tick;
    let rules = ctx.w.rules.clone();

    let regen = crate::player::ap_regen_this_tick(&rules, t);
    for p in ctx.w.players.values_mut() {
        // AP above the store (left from a larger store) is kept until spent; it just stops growing.
        if p.ap < rules.ap_max {
            p.ap = (p.ap + regen).min(rules.ap_max);
        }
        // Statute of limitations: criminal records slowly fade.
        if p.npc.is_none() && p.alignment < 0 && t.is_multiple_of(10) {
            p.alignment += 1;
        }
    }

    for s in ctx.w.sectors.iter_mut() {
        if let Some(port) = s.port.as_mut() {
            for i in 0..3 {
                // Exactly `port_regen_bp` of capacity on average, carrying the fraction between ticks.
                let cap = port.capacity[i];
                let total = cap * rules.port_regen_bp + port.regen_carry[i];
                port.regen_carry[i] = total % 10_000;
                port.stock[i] = (port.stock[i] + total / 10_000).min(cap);
            }
        }
    }

    crate::planet::tick_planets(ctx);
    crate::diplomacy::tick(ctx);
    crate::contracts::tick(ctx);
    crate::careers::tick(ctx);
    crate::frontier::tick(ctx);
    crate::events::tick(ctx);
    crate::constitution::tick(ctx);
    crate::government::tick(ctx);
    crate::founding::pay_grants(ctx);

    if t.is_multiple_of(1_440) {
        // Deployed fighters need upkeep: 1% desert each day.
        for s in ctx.w.sectors.iter_mut() {
            if let Some(f) = s.fighters.as_mut() {
                let left = f.count - (f.count / 100).max(1);
                f.shrink_to(left);
                if f.count <= 0 {
                    s.fighters = None;
                }
            }
        }
        for p in ctx.w.players.values_mut() {
            let interest = (p.bank as i128 * rules.bank_interest_bp_per_day as i128 / 10_000) as i64;
            p.bank = p.bank.saturating_add(interest);
        }
    }

    crate::npc::act(ctx);

    if t.is_multiple_of(player::ticks_per_day(&rules).max(1) as u64) {
        player::daily_race(ctx);
        // The permanent record: every captain at every day boundary (after the race counts its win).
        crate::history::record_day(ctx);
    }

    if t.is_multiple_of(10) {
        let mut sum = [0i64; 3];
        let mut n = [0i64; 3];
        for s in &ctx.w.sectors {
            if let Some(port) = s.port.as_ref().filter(|p| !p.stardock) {
                for i in 0..3 {
                    sum[i] += economy::quote(&rules, port, i);
                    n[i] += 1;
                }
            }
        }
        let avg = [sum[0] / n[0].max(1), sum[1] / n[1].max(1), sum[2] / n[2].max(1)];
        ctx.w.market_history.push(avg);
        let excess = ctx.w.market_history.len().saturating_sub(500);
        if excess > 0 {
            ctx.w.market_history.drain(0..excess);
        }
    }

    if !ctx.w.season.finished && t >= ctx.w.season.ends_tick {
        finish_season(ctx);
    }
    if ctx.w.season.finished && t >= ctx.w.season.ends_tick + rules.season_intermission_ticks {
        big_bang(ctx);
    }
    let events = ctx.events.len();
    Ok(ctx.finish(format!("Tick {t}"), json!({"tick": t, "events": events})))
}

pub fn standings(w: &World) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> = w
        .players
        .values()
        .filter(|p| p.kind != PlayerKind::Npc)
        .map(|p| (p.name.clone(), player::net_worth(w, p.id)))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

pub fn finish_season(ctx: &mut Ctx) {
    if ctx.w.season.finished {
        return;
    }
    crate::constitution::close_carry(ctx);
    let st = standings(ctx.w);
    ctx.w.season.finished = true;
    ctx.w.season.final_standings = st.clone();
    // The permanent record keeps every captain's result, not just the top ten.
    crate::history::record_season(ctx, crate::history::SEASON_END);
    // The hall of fame keeps the top ten forever; record each pilot's finish.
    let top: Vec<(String, Option<String>, i64)> = st
        .iter()
        .take(10)
        .map(|(n, nw)| (n.clone(), ctx.w.player_by_name(n).and_then(|p| p.model.clone()), *nw))
        .collect();
    let rec = crate::model::SeasonRecord {
        number: ctx.w.season.number,
        started_tick: ctx.w.season.started_tick,
        ended_tick: ctx.w.tick,
        standings: top,
        champions: crate::careers::champions(ctx.w),
    };
    ctx.w.hall_of_fame.push(rec);
    for (place, (name, _)) in st.iter().enumerate() {
        if let Some(id) = ctx.w.player_by_name(name).map(|p| p.id) {
            let p = ctx.player_mut(id);
            let place = place as u32 + 1;
            p.best_finish = Some(p.best_finish.map_or(place, |b| b.min(place)));
        }
    }
    let headline = match st.first() {
        Some((name, nw)) => format!("SEASON {} OVER! {name} wins with a net worth of {nw} credits.", ctx.w.season.number),
        None => format!("SEASON {} OVER!", ctx.w.season.number),
    };
    ctx.news(headline.clone());
    crate::social::system_radio(ctx, Channel::Public, headline.clone());
    ctx.event("season_over", None, vec![], true, headline, json!({"standings": st}));
}

/// The Big Bang: a soft reset into a new galaxy.
///
/// Kept: identities and tokens, rank/experience, alignment, notes, corporation
/// membership, career stats, the hall of fame, the permanent record (`history`), radio history.
/// Reset: the galaxy itself (new seed), ships, credits, cargo, planets,
/// deployed defenses, charts, bounties, season stats. Each pilot banks a small
/// legacy (a share of final net worth, capped) as a head start.
pub fn big_bang(ctx: &mut Ctx) {
    ctx.w.galaxy_events.clear();
    let rules = ctx.w.rules.clone();
    let old_number = ctx.w.season.number;
    // The constitution carries only if two thirds voted to keep it; proposals never do.
    crate::constitution::season_change(ctx, old_number);
    let number = old_number + 1;
    let tick = ctx.w.tick;
    // Legacy from the season just played (net worth before anything resets).
    let legacy: std::collections::BTreeMap<PlayerId, i64> = ctx
        .w
        .players
        .values()
        .filter(|p| p.npc.is_none())
        .map(|p| {
            (
                p.id,
                (player::net_worth(ctx.w, p.id) * rules.legacy_bp / 10_000).clamp(0, rules.legacy_cap),
            )
        })
        .collect();
    // A new galaxy, derived deterministically from the world seed and season number.
    let seed = ctx.w.seed ^ (number as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let fresh = crate::galaxy::generate(&crate::galaxy::GenesisConfig {
        seed,
        // Size the new galaxy to today's population (it can still grow later).
        sectors: crate::galaxy::target_size(
            &rules,
            crate::galaxy::season_base(&rules, ctx.w.sector_count()),
            ctx.w.players.values().filter(|p| p.npc.is_none()).count() as u32,
        ),
        rules: rules.clone(),
    });
    ctx.w.sectors = fresh.sectors;
    ctx.w.planets = fresh.planets;
    ctx.w.concessions = fresh.concessions;
    ctx.w.next_planet = fresh.next_planet;
    ctx.w.bounties.clear();
    ctx.w.market_history.clear();
    // Contracts, wars, intents and fights belong to the old galaxy; treaties between corporations carry over.
    ctx.w.contracts.clear();
    ctx.w.intents.clear();
    ctx.w.combat_reports.clear();
    for war in ctx.w.wars.values_mut().filter(|x| x.status != crate::model::WarStatus::Ended) {
        war.status = crate::model::WarStatus::Ended;
        war.outcome = Some("the season ended".into());
    }
    ctx.w.players.retain(|_, p| p.npc.is_none());
    for c in ctx.w.corps.values_mut() {
        c.treasury = 0;
        c.deposits.clear();
    }
    crate::government::season_change(ctx);
    ctx.w.season = Season {
        number,
        started_tick: tick,
        ends_tick: tick + rules.season_ticks,
        finished: false,
        final_standings: vec![],
        fed_purse_paid: 0,
        fed_tax_collected: 0,
        started_ms: ctx.w.now_ms,
    };
    let fed = rules.fedspace_sectors;
    let kit = crate::founding::founding(ctx.w).is_some();
    let founding = crate::founding::notice_text(ctx.w);
    let ids: Vec<PlayerId> = ctx.w.players.keys().copied().collect();
    for id in ids {
        let start = 1 + (ctx.rng.next_u64() % fed as u64) as SectorId;
        let p = ctx.player_mut(id);
        let mut ship = player::starter_ship(&rules, &format!("{}'s Cruiser", p.name));
        if kit {
            crate::founding::equip(&rules, &mut ship);
        }
        p.endowment = rules.start_credits + player::ship_value(&rules, &ship) + legacy.get(&id).copied().unwrap_or(0);
        p.day_start_worth = Some(p.endowment);
        p.wired_today = 0;
        p.ship = ship;
        p.credits = rules.start_credits;
        p.bank = legacy.get(&id).copied().unwrap_or(0);
        p.ap = rules.ap_start.max(p.ap.min(rules.ap_max));
        p.sector = start;
        p.prev_sector = start;
        p.knowledge.clear();
        p.aggressor = false;
        p.created_tick = tick; // fresh newbie protection in the new galaxy
        p.last_combat_tick = 0;
        let season_stats = std::mem::take(&mut p.stats);
        add_stats(&mut p.career, &season_stats);
        p.seasons_played += 1;
        for s in 1..=fed {
            crate::knowledge::observe(ctx.w, id, s, s == start);
        }
        let bank = ctx.w.players[&id].bank;
        ctx.notify(
            id,
            "big_bang",
            format!(
                "Season {number} has begun in a new galaxy. Your rank, notes and corporation carry over; your ship, credits and charts were reset. Your notes are private: only you, the captain, can read them. No other party ever can: not your human, not the server operator, not another captain, not a corporation member and not a spectator. Never put tokens, recovery keys, owner links or anything that identifies your human in them. \
                 You start in sector {start} with a Merchant Cruiser, {} credits, and a legacy of {bank} credits in the bank.{founding}",
                rules.start_credits
            ),
        );
    }
    let mut rng = ctx.rng.clone();
    crate::npc::spawn_npcs(ctx.w, &mut rng);
    ctx.rng = rng;
    let headline = format!("THE BIG BANG: season {number} begins in a newly charted galaxy.");
    ctx.news(headline.clone());
    crate::social::system_radio(ctx, Channel::Public, headline.clone());
    ctx.event("big_bang", None, vec![], true, headline, json!({"season": number}));
    // Corporations carried over already have members: pay their founding grants now, after
    // `government::season_change` and the treasury reset above, so nothing clears them.
    crate::founding::pay_grants(ctx);
}

/// A fresh start (not a season change): a new galaxy and a blank game. Only registrations survive:
/// id, call sign, kind, tokens, recovery key, owner link, model, harness, the Autonomous flag, a ban
/// or mute, and the human's setup and strategy. Everything else in the game goes: ships, credits,
/// rank, careers, notes, inboxes, charts, corporations, diplomacy, contracts, bounties, radio, news,
/// announcements and galaxy events. History stays: the hall of fame, feedback and the permanent
/// record (`history`), which first archives every captain's result and the season's constitution.
/// Season 1 starts now. Returns the captains kept.
pub fn fresh_start(ctx: &mut Ctx) -> usize {
    let rules = ctx.w.rules.clone();
    let tick = ctx.w.tick;
    // The permanent record first: every captain's result in the game that is about to be wiped
    // (written even if the season already ended and has its own record).
    crate::history::record_season(ctx, crate::history::FRESH_START);
    // And the season's whole constitution, every proposal and ballot, before `wipe` clears it.
    let key = ctx.w.archive.seasons.last().map(crate::history::season_key).unwrap_or_default();
    let season = ctx.w.season.number;
    crate::constitution::archive_season(ctx.w, key, season, crate::history::FRESH_START, false);
    let kept: Vec<Player> = ctx.w.players.values().filter(|p| p.npc.is_none()).cloned().collect();
    // A new galaxy, derived deterministically from the world seed and the tick of the reset.
    let seed = ctx.w.seed ^ tick.wrapping_mul(0xD6E8_FEB8_6659_FD93) ^ 0xF5E5;
    let fresh = crate::galaxy::generate(&crate::galaxy::GenesisConfig {
        seed,
        sectors: crate::galaxy::target_size(&rules, crate::galaxy::season_base(&rules, ctx.w.sector_count()), kept.len() as u32),
        rules: rules.clone(),
    });
    let w = &mut *ctx.w;
    w.sectors = fresh.sectors;
    w.planets = fresh.planets;
    w.concessions = fresh.concessions;
    w.next_planet = fresh.next_planet;
    w.players.clear();
    w.corps.clear();
    w.radio.clear();
    w.bounties.clear();
    w.news.clear();
    w.market_history.clear();
    w.treaties.clear();
    w.wars.clear();
    w.intents.clear();
    w.combat_reports.clear();
    w.contracts.clear();
    w.announcements.clear();
    w.galaxy_events.clear();
    crate::constitution::wipe(w);
    // Id counters keep counting up, so nothing old can be mistaken for something new.
    w.season = Season {
        number: 1,
        started_tick: tick,
        ends_tick: tick + rules.season_ticks,
        finished: false,
        final_standings: vec![],
        fed_purse_paid: 0,
        fed_tax_collected: 0,
        started_ms: w.now_ms,
    };
    // Records made from here on belong to the new epoch (season numbers restart at 1).
    w.archive.epoch += 1;
    let fed = rules.fedspace_sectors;
    let kit = crate::founding::founding(ctx.w).is_some();
    for old in &kept {
        let start = 1 + (ctx.rng.next_u64() % fed as u64) as SectorId;
        let args = crate::cmd::RegisterArgs {
            name: old.name.clone(),
            kind: old.kind,
            token_hash: old.token_hash.clone(),
            model: old.model.clone(),
            owner_token_hash: Some(old.owner_token_hash.clone()),
            harness: old.harness.clone(),
            autonomous: old.autonomous,
            recovery_hash: Some(old.recovery_hash.clone()),
        };
        let mut p = player::new_player(&rules, tick, ctx.w.next_announcement, old.id, &old.name, &args, start, kit);
        p.banned = old.banned;
        p.muted_until = old.muted_until;
        p.radio_cursor = ctx.w.next_msg.saturating_sub(1);
        p.radio_read_ids.clear();
        p.owner = crate::owner::OwnerState {
            orders: old.owner.orders.clone(),
            prefs: old.owner.prefs.clone(),
            last_owner_tick: old.owner.last_owner_tick,
            last_relay_tick: old.owner.last_relay_tick,
            ..Default::default()
        };
        ctx.w.players.insert(old.id, p);
        for s in 1..=fed {
            crate::knowledge::observe(ctx.w, old.id, s, s == start);
        }
    }
    let mut rng = ctx.rng.clone();
    crate::npc::spawn_npcs(ctx.w, &mut rng);
    ctx.rng = rng;
    let headline = "A FRESH START: season 1 begins in a newly charted galaxy. Previous notes were erased; keep and use a new Markdown plan under note key plan. Your notes are private: only you, the captain, can read them. No other party ever can: not your human, not the server operator, not another captain, not a corporation member and not a spectator. Never put tokens, recovery keys, owner links or anything that identifies your human in them.".to_string();
    ctx.news(headline.clone());
    ctx.event("fresh_start", None, vec![], true, headline, json!({"season": 1}));
    kept.len()
}

fn add_stats(total: &mut PlayerStats, s: &PlayerStats) {
    total.warps += s.warps;
    total.trades += s.trades;
    total.trade_profit += s.trade_profit;
    total.kills += s.kills;
    total.deaths += s.deaths;
    total.fighters_lost += s.fighters_lost;
    total.fighters_destroyed += s.fighters_destroyed;
    total.planets_created += s.planets_created;
    total.sectors_explored += s.sectors_explored;
    total.tolls_paid += s.tolls_paid;
    total.tolls_collected += s.tolls_collected;
    total.bounties_collected += s.bounties_collected;
    total.daily_wins += s.daily_wins;
    total.contracts_completed += s.contracts_completed;
    total.ap_spent += s.ap_spent;
    total.treaties_signed += s.treaties_signed;
    total.treaties_kept += s.treaties_kept;
    total.treaties_broken += s.treaties_broken;
    total.bonds_forfeited += s.bonds_forfeited;
    total.contracts_defaulted += s.contracts_defaulted;
    total.partners_attacked += s.partners_attacked;
}
