//! Careers the captains choose for themselves, their small server-enforced
//! perks, the season score tracks (wealth, conquest, exploration, influence),
//! and the Federation purse that pays for policing.
//!
//! Every tool stays open to every career: a career is a licence and a badge,
//! not a cage. Perks are deliberately small so no career is mandatory.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cmd::EquipmentItem;
use crate::engine::{Ctx, Outcome, Refusal, Res};
use crate::model::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Career {
    Merchant,
    Corsair,
    Builder,
    Diplomat,
    Marshal,
    Mercenary,
    Smuggler,
    Cartographer,
}

pub const CAREERS: [Career; 8] = [
    Career::Merchant,
    Career::Corsair,
    Career::Builder,
    Career::Diplomat,
    Career::Marshal,
    Career::Mercenary,
    Career::Smuggler,
    Career::Cartographer,
];

impl Career {
    pub fn perk(self) -> &'static str {
        match self {
            Career::Merchant => "Cargo holds cost 10% less at Stardock.",
            Career::Corsair => "Boarding seizes 60% of the target's on-hand credits (others take 50%).",
            Career::Builder => "Colonists cost 25% less at Stardock.",
            Career::Diplomat => "Influence score counts 1.5x.",
            Career::Marshal => {
                "Federation rewards for Skarn and WANTED kills are doubled. Requires alignment >= 0 and no charged tolls; lost if your alignment goes negative."
            }
            Career::Mercenary => "Fighters cost 10% less at Stardock; combat contracts are your trade.",
            Career::Smuggler => "Federation customs scans at FedSpace catch you half as often.",
            Career::Cartographer => "Ether probes cost half; chart contracts count double toward exploration.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScoreTrack {
    /// Net worth earned this season (net worth minus what you started with).
    Wealth,
    /// Combat income: salvage, bounties, tolls, combat contracts, Federation rewards, plus kills.
    Conquest,
    /// Sectors explored, planets founded, charts sold.
    Exploration,
    /// Corporation leadership, treaties in force, contracts completed.
    Influence,
}

pub const TRACKS: [ScoreTrack; 4] = [
    ScoreTrack::Wealth,
    ScoreTrack::Conquest,
    ScoreTrack::Exploration,
    ScoreTrack::Influence,
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CareerArgs {
    /// choose (first time, anywhere, free), change (at Stardock; fee and cooldown), list (the menu).
    pub action: CareerAction,
    #[serde(default)]
    pub career: Option<Career>,
    /// The score track you are playing for (public).
    #[serde(default)]
    pub win_condition: Option<ScoreTrack>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CareerAction {
    Choose,
    Change,
    List,
}

/// Price multiplier in basis points for a Stardock item, after career perks.
pub fn price_bp(p: &Player, item: EquipmentItem) -> i64 {
    match (p.profession, item) {
        (Some(Career::Merchant), EquipmentItem::Holds) => 9_000,
        (Some(Career::Mercenary), EquipmentItem::Fighters) => 9_000,
        (Some(Career::Cartographer), EquipmentItem::EtherProbes) => 5_000,
        _ => 10_000,
    }
}

pub fn colonist_price_bp(p: &Player) -> i64 {
    if p.profession == Some(Career::Builder) { 7_500 } else { 10_000 }
}

/// Share of the target's on-hand credits a successful boarding takes.
pub fn board_share_bp(p: &Player) -> i64 {
    if p.profession == Some(Career::Corsair) { 6_000 } else { 5_000 }
}

/// Influence for a member's contribution to its corporation, from what it holds now (never
/// lifetime totals, so depositing and withdrawing in a loop earns nothing): one point per
/// `DEPOSIT_CREDITS_PER_POINT` credits of reserved deposit, and `SHARE_POINTS_PER_FIGHTER` per
/// fighter it holds in a stack a corp-mate owns. Each part is capped (engine 1.32).
pub const DEPOSIT_CREDITS_PER_POINT: i64 = 50;
pub const DEPOSIT_INFLUENCE_CAP: i64 = 5_000;
pub const SHARE_POINTS_PER_FIGHTER: i64 = 5;
pub const SHARE_INFLUENCE_CAP: i64 = 5_000;

fn contribution_influence(w: &World, p: &Player, c: &Corp) -> i64 {
    let deposit = (c.deposit_of(p.id).min(c.treasury) / DEPOSIT_CREDITS_PER_POINT).min(DEPOSIT_INFLUENCE_CAP);
    let shared: i64 = w
        .sectors
        .iter()
        .filter_map(|s| s.fighters.as_ref())
        .filter(|f| f.owner != p.id && c.members.contains(&f.owner))
        .map(|f| f.share_of(p.id))
        .sum();
    deposit + (shared * SHARE_POINTS_PER_FIGHTER).min(SHARE_INFLUENCE_CAP)
}

/// Score on each track for one pilot.
pub fn scores(w: &World, p: &Player) -> [(ScoreTrack, i64); 4] {
    let s = &p.stats;
    let wealth = crate::player::net_worth(w, p.id) - crate::player::endowment(w, p);
    let conquest = s.salvage
        + s.bounties_collected
        + s.tolls_collected
        + s.combat_contract_income
        + s.fed_rewards
        + s.concession_income
        + 2_000 * s.kills;
    let chart = if p.profession == Some(Career::Cartographer) {
        2 * s.chart_income
    } else {
        s.chart_income
    };
    let exploration = 100 * s.sectors_explored + 2_000 * s.planets_created + chart;
    let mut influence = 1_000 * s.contracts_completed + 5_000 * crate::frontier::held_by(w, p.id);
    if let Some(c) = p.corp.and_then(|c| w.corps.get(&c)) {
        if c.ceo == p.id {
            influence += 2_000 * c.members.len() as i64;
        }
        let treaties = w
            .treaties
            .values()
            .filter(|t| matches!(t.status, TreatyStatus::Active | TreatyStatus::Ending) && (t.a == c.id || t.b == c.id))
            .count() as i64;
        influence += 3_000 * treaties;
        influence += contribution_influence(w, p, c);
    }
    // A captain's own live treaties count like its corporation's (engine 1.32).
    let personal = w
        .treaties
        .values()
        .filter(|t| {
            matches!(t.status, TreatyStatus::Active | TreatyStatus::Ending) && (t.a_player == Some(p.id) || t.b_player == Some(p.id))
        })
        .count() as i64;
    influence += 3_000 * personal;
    influence += crate::chronicle::influence(w, p.id);
    if p.profession == Some(Career::Diplomat) {
        influence = influence * 3 / 2;
    }
    // No rarity bonus (engine 1.21): it rewarded the label, not the play, and applied
    // retroactively to a whole season's score.
    [
        (ScoreTrack::Wealth, wealth),
        (ScoreTrack::Conquest, conquest),
        (ScoreTrack::Exploration, exploration),
        (ScoreTrack::Influence, influence),
    ]
}

/// What to do with AP, by career: the briefing's advice follows the path the captain chose.
pub fn ap_moves(p: &Player) -> &'static str {
    match p.profession {
        Some(Career::Merchant) | None => {
            "trade leg by leg (find_trade_routes, then at each port trade_port and autopilot_travel to the next), chase market booms, upgrade holds at Stardock"
        }
        Some(Career::Corsair) => {
            "hunt Skarn raiders (check galaxy_events for hunt bonuses), set toll fighters on busy choke points, board loaded traders who are not newbie-protected"
        }
        Some(Career::Builder) => {
            "launch Genesis torpedoes in quiet sectors, ferry colonists (buy_colonists), and raise citadels with manage_planet"
        }
        Some(Career::Diplomat) => "found or grow a corporation, propose treaties, post and broker contracts, and talk on the radio",
        Some(Career::Marshal) => "hunt Skarn raiders and WANTED pilots: the Federation pays you double",
        Some(Career::Mercenary) => "take kill, defend and escort contracts, and keep your fighters up",
        Some(Career::Smuggler) => "buy contraband at black markets and sell it at FedSpace ports (customs may scan you)",
        Some(Career::Cartographer) => "explore with probes and autopilot_travel, and sell charts through chart contracts",
    }
}

pub fn scores_json(w: &World, p: &Player) -> Value {
    let s = scores(w, p);
    json!({"wealth": s[0].1, "conquest": s[1].1, "exploration": s[2].1, "influence": s[3].1})
}

fn track_name(t: ScoreTrack) -> &'static str {
    match t {
        ScoreTrack::Wealth => "wealth",
        ScoreTrack::Conquest => "conquest",
        ScoreTrack::Exploration => "exploration",
        ScoreTrack::Influence => "influence",
    }
}

/// The leader of every track (captains only): (track, name, model, score).
pub fn champions(w: &World) -> Vec<(String, String, Option<String>, i64)> {
    TRACKS
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            w.players
                .values()
                .filter(|p| p.kind != PlayerKind::Npc)
                .map(|p| (scores(w, p)[i].1, p))
                .max_by(|a, b| a.0.cmp(&b.0).then(b.1.id.cmp(&a.1.id)))
                .filter(|(s, _)| *s > 0)
                .map(|(s, p)| (track_name(*t).to_string(), p.name.clone(), p.model.clone(), s))
        })
        .collect()
}

fn population(w: &World) -> (Vec<(Career, usize)>, usize) {
    let mut counts: Vec<(Career, usize)> = CAREERS.iter().map(|c| (*c, 0)).collect();
    let mut total = 0;
    for p in w.players.values().filter(|p| p.kind != PlayerKind::Npc) {
        if let Some(c) = p.profession {
            total += 1;
            if let Some(e) = counts.iter_mut().find(|(k, _)| *k == c) {
                e.1 += 1;
            }
        }
    }
    (counts, total)
}

/// The career menu with live population counts.
pub fn menu(w: &World) -> Value {
    let (counts, total) = population(w);
    json!({
        "careers": counts.iter().map(|(c, n)| json!({"career": c, "captains": n, "perk": c.perk()})).collect::<Vec<_>>(),
        "variety": "Every career has a working path: the briefing's ap_advice follows the one you choose. Any career is open to anyone.",
        "captains_with_careers": total,
        "win_conditions": TRACKS.iter().map(|t| track_name(*t)).collect::<Vec<_>>(),
        "how": "career action=choose career=<one> win_condition=<track>. The first choice is free; changing later costs credits at Stardock and has a cooldown. Every tool stays open to every career.",
    })
}

pub fn career(ctx: &mut Ctx, pid: PlayerId, a: &CareerArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    if a.action == CareerAction::List {
        let m = menu(ctx.w);
        return Ok(ctx.finish("Career menu.", m));
    }
    let career = a
        .career
        .ok_or_else(|| Refusal::new("invalid_args", "career is required (career action=list shows the menu)."))?;
    let rules = ctx.w.rules.clone();
    let changing = p.profession.is_some();
    if a.action == CareerAction::Choose && changing {
        return Err(Refusal::new("already_chosen", "You already have a career.").hint("Use action=change at Stardock."));
    }
    if changing {
        if p.sector != rules.stardock_sector {
            return Err(Refusal::new(
                "not_at_stardock",
                "Career changes are registered at Stardock (sector 1).",
            ));
        }
        if ctx.w.tick < p.profession_since + rules.career_cooldown_ticks {
            return Err(Refusal::new(
                "cooldown",
                format!(
                    "You can change career again at tick {}.",
                    p.profession_since + rules.career_cooldown_ticks
                ),
            ));
        }
        if p.credits < rules.career_change_fee {
            return Err(ctx.no_credits(pid, "A career change", rules.career_change_fee));
        }
    }
    if career == Career::Marshal && p.alignment < 0 {
        return Err(Refusal::new("not_eligible", "Marshals need an alignment of 0 or better."));
    }
    // No population cap: any career is always open.
    let tick = ctx.w.tick;
    let pm = ctx.player_mut(pid);
    if changing {
        pm.credits -= rules.career_change_fee;
    }
    pm.profession = Some(career);
    pm.profession_since = tick;
    if let Some(wc) = a.win_condition {
        pm.win_condition = Some(wc);
    }
    let name = pm.name.clone();
    let wc = pm.win_condition;
    let text = format!(
        "{name} {} {career:?}{}.",
        if changing { "is now a" } else { "chose the career of" },
        wc.map(|t| format!(", playing for {}", track_name(t))).unwrap_or_default()
    );
    ctx.news(text.clone());
    ctx.event("career", None, vec![pid], true, text, json!({"career": career}));
    Ok(ctx.finish(
        format!("Career: {career:?}. Perk: {}", career.perk()),
        json!({"career": career, "win_condition": wc}),
    ))
}

fn fed_purse_remaining(w: &World) -> i64 {
    (w.rules.fed_purse_per_season + w.season.fed_tax_collected - w.season.fed_purse_paid).max(0)
}

fn fed_daily_allowance(w: &World, p: &Player) -> (u64, i64, i64) {
    let day = w.tick / crate::player::ticks_per_day(&w.rules).max(1) as u64;
    let paid = if p.fed_paid_today.0 == day { p.fed_paid_today.1 } else { 0 };
    let remaining = if w.rules.fed_daily_cap > 0 {
        (w.rules.fed_daily_cap - paid).max(0)
    } else {
        i64::MAX
    };
    (day, paid, remaining)
}

/// Read-only bounty quotes share the payout's purse and daily allowance calculations.
pub fn federation_rewards(w: &World, p: Option<&Player>) -> Value {
    let r = &w.rules;
    let remaining = fed_purse_remaining(w);
    let mut v = json!({
        "skarn_base": r.fed_skarn_reward, "wanted_base": r.fed_wanted_reward,
        "marshal_multiplier": 2, "daily_cap": r.fed_daily_cap,
        "season_base_purse": r.fed_purse_per_season, "season_tax_collected": w.season.fed_tax_collected,
        "season_paid": w.season.fed_purse_paid, "purse_remaining": remaining,
        "formula": "min(base reward x career multiplier, shared purse remaining, captain daily allowance remaining). NPC and WANTED captains are ineligible. daily_cap <= 0 means unlimited. Quotes are current estimates, not reservations; checked again at the kill. Daily allowance resets at game-day boundaries (tick / ticks_per_day), not UTC midnight. Boarding and salvage are separate.",
        "ticks_per_day": crate::player::ticks_per_day(r).max(1),
        "check_before_combat": "get_status or get_briefing: federation_rewards",
    });
    if let Some(p) = p {
        let (day, paid, allowance) = fed_daily_allowance(w, p);
        let eligible = p.npc.is_none() && p.alignment > r.fed_wanted_alignment;
        let multiplier = if p.profession == Some(Career::Marshal) { 2 } else { 1 };
        let quote = |base: i64| {
            if eligible {
                (base * multiplier).min(remaining).min(allowance).max(0)
            } else {
                0
            }
        };
        v["eligible"] = json!(eligible);
        v["career_multiplier"] = json!(multiplier);
        v["paid_today"] = json!(paid);
        v["daily_remaining"] = if r.fed_daily_cap > 0 { json!(allowance) } else { Value::Null };
        v["daily_resets_tick"] = json!((day + 1) * crate::player::ticks_per_day(r).max(1) as u64);
        v["skarn_reward_now"] = json!(quote(r.fed_skarn_reward));
        v["wanted_reward_now"] = json!(quote(r.fed_wanted_reward));
    }
    v
}

/// Federation purse: pay `killer` for a Skarn or WANTED kill (doubled for marshals), within the season cap.
pub fn fed_reward(ctx: &mut Ctx, killer: PlayerId, victim_was_skarn: bool, victim_alignment: i64) {
    let rules = ctx.w.rules.clone();
    let Some(k) = ctx.w.players.get(&killer) else { return };
    if k.npc.is_some() || k.alignment <= rules.fed_wanted_alignment {
        return;
    }
    let base = if victim_was_skarn {
        rules.fed_skarn_reward
    } else if victim_alignment <= rules.fed_wanted_alignment {
        rules.fed_wanted_reward
    } else {
        return;
    };
    let mult = if k.profession == Some(Career::Marshal) { 2 } else { 1 };
    // Federation taxes collected this season top up the purse (engine 1.35).
    let left = fed_purse_remaining(ctx.w);
    // At most `fed_daily_cap` to one captain per real day, so no single hunter drains the purse.
    let (day, paid_today, cap_left) = fed_daily_allowance(ctx.w, k);
    let pay = (base * mult).min(left).min(cap_left);
    if pay <= 0 {
        return;
    }
    ctx.w.season.fed_purse_paid += pay;
    let km = ctx.player_mut(killer);
    km.credits += pay;
    km.stats.fed_rewards += pay;
    km.fed_paid_today = (day, paid_today + pay);
    ctx.notify(killer, "reward", format!("The Federation paid you {pay} credits for that kill."));
}

/// Marshals who stop behaving like marshals lose the licence.
pub fn tick(ctx: &mut Ctx) {
    let mut revoked = vec![];
    for p in ctx.w.players.values_mut() {
        if p.profession == Some(Career::Marshal) && p.alignment < 0 {
            p.profession = None;
            revoked.push(p.id);
        }
    }
    for id in revoked {
        ctx.notify(
            id,
            "career",
            "Your marshal's licence was revoked: alignment below zero. Choose a new career.".to_string(),
        );
    }
}

/// What the briefing shows about the pilot's career and scores.
pub fn briefing(w: &World, p: &Player) -> Value {
    match p.profession {
        None => json!({"career": null, "choose_one": menu(w)}),
        Some(c) => {
            json!({"career": c, "perk": c.perk(), "win_condition": p.win_condition, "scores": scores_json(w, p)})
        }
    }
}
