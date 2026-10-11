//! Escrowed contracts: work one captain pays another to do, settled by the
//! engine, never by trust. The reward leaves the poster's ship when the
//! contract is posted and is paid only on verified completion (or refunded).
//!
//! - kill: destroy the target pilot's ship before the deadline.
//! - defend: keep your (or your corp's) fighters in a sector for the term.
//! - escort: stay in the client's sector for most of the term while they survive.
//! - haul: unload a quantity of a commodity onto a planet.
//! - chart: deliver your charts of a list of sectors.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Ctx, Outcome, Refusal, Res};
use crate::model::*;
use crate::player;

const CHECK_EVERY: u64 = 10;
const KEEP_CLOSED: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContractKind {
    Kill,
    Defend,
    Escort,
    Haul,
    Chart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    Open,
    Taken,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Contract {
    pub id: u64,
    pub kind: ContractKind,
    pub poster: PlayerId,
    pub taker: Option<PlayerId>,
    pub target: Option<PlayerId>,
    pub sector: Option<SectorId>,
    pub planet: Option<PlanetId>,
    pub commodity: Option<Commodity>,
    pub quantity: i64,
    pub delivered: i64,
    pub sectors: Vec<SectorId>,
    pub reward: i64,
    pub status: ContractStatus,
    pub terms: String,
    pub posted_tick: Tick,
    /// Open offers lapse at this tick.
    pub expires_tick: Tick,
    /// Term length once taken (defend, escort).
    pub term_ticks: u64,
    pub taken_tick: Option<Tick>,
    pub deadline: Option<Tick>,
    pub checks: u32,
    pub checks_ok: u32,
    /// What the taker staked on accepting: returned on completion, the poster's on failure.
    #[serde(default)]
    pub stake: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContractAction {
    /// Post a contract; the reward is escrowed from your ship now.
    Post,
    /// Take an open contract (you become the only one who can complete it).
    Accept,
    /// chart contracts: hand over your charts and get paid.
    Deliver,
    /// Withdraw your own contract while nobody has taken it (refund).
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContractArgs {
    pub action: ContractAction,
    /// post: kill, defend, escort, haul or chart.
    #[serde(default)]
    pub kind: Option<ContractKind>,
    /// accept/deliver/cancel: contract id.
    #[serde(default)]
    pub contract_id: Option<u64>,
    /// kill: target pilot; escort: client (default: you).
    #[serde(default)]
    pub target: Option<String>,
    /// defend: the sector to hold.
    #[serde(default)]
    pub sector: Option<SectorId>,
    /// haul: the destination planet id.
    #[serde(default)]
    pub planet_id: Option<PlanetId>,
    /// haul: what to deliver.
    #[serde(default)]
    pub commodity: Option<Commodity>,
    /// haul: how many units.
    #[serde(default)]
    pub quantity: Option<i64>,
    /// chart: sectors wanted (max 40).
    #[serde(default)]
    pub sectors: Option<Vec<SectorId>>,
    /// post: reward credits (min 100).
    #[serde(default)]
    pub reward: Option<i64>,
    /// post: ticks the offer stays open (default 1440); also the deadline for kill/haul/chart.
    #[serde(default)]
    pub expires_in_ticks: Option<u64>,
    /// defend/escort: term in ticks (default 360).
    #[serde(default)]
    pub term_ticks: Option<u64>,
    /// post: public note.
    #[serde(default)]
    pub terms: Option<String>,
}

fn next_id(w: &mut World) -> u64 {
    w.next_registry_id = w.next_registry_id.max(1);
    let id = w.next_registry_id;
    w.next_registry_id += 1;
    id
}

fn describe(w: &World, c: &Contract) -> String {
    let who = |p: Option<PlayerId>| p.map(|p| w.player_name(p)).unwrap_or_else(|| "?".into());
    match c.kind {
        ContractKind::Kill => format!("kill {}", who(c.target)),
        ContractKind::Defend => format!("hold sector {} for {} ticks", c.sector.unwrap_or(0), c.term_ticks),
        ContractKind::Escort => format!("escort {} for {} ticks", who(c.target), c.term_ticks),
        ContractKind::Haul => format!(
            "haul {} {} to planet #{}",
            c.quantity,
            c.commodity.map(|x| x.name()).unwrap_or("?"),
            c.planet.unwrap_or(0)
        ),
        ContractKind::Chart => format!("chart {} sectors", c.sectors.len()),
    }
}

pub fn contract(ctx: &mut Ctx, pid: PlayerId, a: &ContractArgs) -> Res<Outcome> {
    let me = ctx.player(pid)?.clone();
    let tick = ctx.w.tick;
    match a.action {
        ContractAction::Post => {
            let kind = a
                .kind
                .ok_or_else(|| Refusal::new("invalid_args", "kind is required: kill, defend, escort, haul or chart."))?;
            let reward = a.reward.unwrap_or(0);
            if reward < 100 {
                return Err(Refusal::new("invalid_args", "reward must be at least 100 credits."));
            }
            if me.credits < reward {
                return Err(ctx.no_credits(pid, "The escrowed reward", reward));
            }
            let mut c = Contract {
                id: 0,
                kind,
                poster: pid,
                taker: None,
                target: None,
                sector: None,
                planet: None,
                commodity: None,
                quantity: 0,
                delivered: 0,
                sectors: vec![],
                reward,
                status: ContractStatus::Open,
                terms: a.terms.clone().unwrap_or_default(),
                posted_tick: tick,
                expires_tick: tick + a.expires_in_ticks.unwrap_or(1_440).clamp(30, 8_640),
                term_ticks: a.term_ticks.unwrap_or(360).clamp(30, 2_880),
                taken_tick: None,
                deadline: None,
                checks: 0,
                checks_ok: 0,
                stake: 0,
            };
            match kind {
                ContractKind::Kill | ContractKind::Escort => {
                    let t = match (&a.target, kind) {
                        (Some(n), _) => player::resolve(ctx.w, n).map(|p| p.id),
                        (None, ContractKind::Escort) => Some(pid),
                        _ => None,
                    }
                    .ok_or_else(|| Refusal::new("unknown_player", "target must name an existing pilot."))?;
                    if kind == ContractKind::Kill && t == pid {
                        return Err(Refusal::new("invalid_args", "You cannot put a contract on yourself."));
                    }
                    c.target = Some(t);
                }
                ContractKind::Defend => {
                    let s = a.sector.filter(|s| ctx.w.sector(*s).is_some_and(|x| !x.fedspace)).ok_or_else(|| {
                        Refusal::new(
                            "invalid_args",
                            "sector must be a frontier sector (FedSpace forbids deployed defenses).",
                        )
                    })?;
                    c.sector = Some(s);
                }
                ContractKind::Haul => {
                    let pl = a
                        .planet_id
                        .and_then(|id| ctx.w.planets.get(&id))
                        .ok_or_else(|| Refusal::new("unknown_planet", "planet_id must name a planet."))?;
                    let q = a.quantity.unwrap_or(0);
                    if q <= 0 || a.commodity.is_none() {
                        return Err(Refusal::new("invalid_args", "haul needs a commodity and a positive quantity."));
                    }
                    c.planet = Some(pl.id);
                    c.commodity = a.commodity;
                    c.quantity = q;
                }
                ContractKind::Chart => {
                    let mut s: Vec<SectorId> = a
                        .sectors
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|s| ctx.w.sector(*s).is_some())
                        .collect();
                    s.sort();
                    s.dedup();
                    s.truncate(40);
                    if s.is_empty() {
                        return Err(Refusal::new("invalid_args", "sectors must list the sectors you want charted."));
                    }
                    c.sectors = s;
                }
            }
            c.id = next_id(ctx.w);
            let id = c.id;
            let what = describe(ctx.w, &c);
            ctx.player_mut(pid).credits -= reward;
            ctx.w.contracts.insert(id, c);
            let text = format!("CONTRACT #{id}: {} offers {reward} credits to {what}.", me.name);
            ctx.news(text.clone());
            ctx.event("contract", None, vec![pid], true, text, json!({"contract": id}));
            Ok(ctx.finish(
                format!("Contract #{id} posted ({what}); {reward} credits escrowed."),
                json!({"contract_id": id}),
            ))
        }
        ContractAction::Accept | ContractAction::Deliver | ContractAction::Cancel => {
            let id = a
                .contract_id
                .ok_or_else(|| Refusal::new("invalid_args", "contract_id is required."))?;
            let c = ctx
                .w
                .contracts
                .get(&id)
                .cloned()
                .ok_or_else(|| Refusal::new("unknown_contract", format!("No contract #{id}.")))?;
            match a.action {
                ContractAction::Accept => {
                    if c.status != ContractStatus::Open {
                        return Err(Refusal::new("not_open", format!("Contract #{id} is not open.")));
                    }
                    if c.poster == pid || c.target == Some(pid) && c.kind == ContractKind::Kill {
                        return Err(Refusal::new("invalid_args", "You cannot take that contract."));
                    }
                    let deadline = match c.kind {
                        ContractKind::Defend | ContractKind::Escort => tick + c.term_ticks,
                        _ => c.expires_tick,
                    };
                    // The taker stakes a share of the reward, so taking a job and sitting on it
                    // costs something (engine 1.45).
                    let stake = (c.reward * ctx.w.rules.contract_stake_bp.max(0) / 10_000).max(0);
                    if ctx.w.players[&pid].credits < stake {
                        return Err(Refusal::new(
                            "stake_required",
                            format!(
                                "Taking contract #{id} stakes {stake} credits from your ship; you carry {}.",
                                ctx.w.players[&pid].credits
                            ),
                        )
                        .hint("The stake comes back when you complete it and goes to the poster if you fail."));
                    }
                    ctx.player_mut(pid).credits -= stake;
                    let cm = ctx.w.contracts.get_mut(&id).unwrap();
                    cm.stake = stake;
                    cm.status = ContractStatus::Taken;
                    cm.taker = Some(pid);
                    cm.taken_tick = Some(tick);
                    cm.deadline = Some(deadline);
                    let what = describe(ctx.w, &c);
                    ctx.notify(c.poster, "contract", format!("{} took contract #{id} ({what}).", me.name));
                    ctx.event(
                        "contract",
                        None,
                        vec![pid, c.poster],
                        true,
                        format!("{} took contract #{id}: {what}.", me.name),
                        json!({}),
                    );
                    Ok(ctx.finish(
                        format!(
                            "You took contract #{id} ({what}); complete it by tick {deadline}.{}",
                            if stake > 0 {
                                format!(" You staked {stake} credits: returned on completion, the poster's if you fail.")
                            } else {
                                String::new()
                            }
                        ),
                        json!({"deadline": deadline, "stake": stake}),
                    ))
                }
                ContractAction::Deliver => {
                    if c.kind != ContractKind::Chart || c.status != ContractStatus::Taken || c.taker != Some(pid) {
                        return Err(Refusal::new(
                            "not_yours",
                            format!("Contract #{id} is not a chart contract you hold."),
                        ));
                    }
                    let missing: Vec<SectorId> = c
                        .sectors
                        .iter()
                        .copied()
                        .filter(|s| !me.knowledge.get(s).map(|k| k.visited || k.warps.is_some()).unwrap_or(false))
                        .collect();
                    if !missing.is_empty() {
                        return Err(Refusal::new("incomplete", format!("You have not charted sectors {missing:?} yet.")));
                    }
                    let charts: Vec<(SectorId, KnownSector)> = c
                        .sectors
                        .iter()
                        .filter_map(|s| me.knowledge.get(s).map(|k| (*s, k.clone())))
                        .collect();
                    let poster = ctx.player_mut(c.poster);
                    for (s, k) in charts {
                        let e = poster.knowledge.entry(s).or_default();
                        if k.last_seen >= e.last_seen {
                            *e = k;
                        }
                    }
                    complete(ctx, id);
                    Ok(ctx.finish(format!("Charts delivered: contract #{id} paid {} credits.", c.reward), json!({})))
                }
                _ => {
                    if c.poster != pid || c.status != ContractStatus::Open {
                        return Err(Refusal::new(
                            "not_cancellable",
                            "Only the poster can withdraw a contract, and only while nobody has taken it.",
                        ));
                    }
                    settle(ctx, id, ContractStatus::Cancelled);
                    Ok(ctx.finish(format!("Contract #{id} withdrawn; {} credits refunded.", c.reward), json!({})))
                }
            }
        }
    }
}

/// Pay the taker and close the contract.
fn complete(ctx: &mut Ctx, id: u64) {
    let Some(c) = ctx.w.contracts.get(&id).cloned() else { return };
    let Some(t) = c.taker else { return };
    let combat = matches!(c.kind, ContractKind::Kill | ContractKind::Defend | ContractKind::Escort);
    if let Some(tp) = ctx.w.players.get_mut(&t) {
        tp.credits += c.reward + c.stake;
        tp.stats.contracts_completed += 1;
        if combat {
            tp.stats.combat_contract_income += c.reward;
        } else if c.kind == ContractKind::Chart {
            tp.stats.chart_income += c.reward;
        }
    }
    ctx.w.contracts.get_mut(&id).unwrap().status = ContractStatus::Completed;
    let what = describe(ctx.w, &c);
    let text = format!(
        "CONTRACT #{id} completed by {}: {what} ({} credits paid).",
        ctx.w.player_name(t),
        c.reward
    );
    ctx.news(text.clone());
    ctx.notify(c.poster, "contract", text.clone());
    ctx.notify(t, "contract", text.clone());
    ctx.event("contract", c.sector, vec![t, c.poster], true, text, json!({}));
}

/// Close without payment: refund the poster, who also takes a failed taker's stake.
fn settle(ctx: &mut Ctx, id: u64, status: ContractStatus) {
    let Some(c) = ctx.w.contracts.get(&id).cloned() else { return };
    let forfeit = if status != ContractStatus::Cancelled && c.taker.is_some() {
        c.stake
    } else {
        0
    };
    if let Some(p) = ctx.w.players.get_mut(&c.poster) {
        p.credits += c.reward + forfeit;
    }
    ctx.w.contracts.get_mut(&id).unwrap().status = status;
    if status != ContractStatus::Cancelled {
        let what = describe(ctx.w, &c);
        ctx.notify(
            c.poster,
            "contract",
            format!("Contract #{id} ({what}) {status:?}; your {} credits were refunded.", c.reward).to_lowercase(),
        );
        if let Some(t) = c.taker {
            if let Some(tp) = ctx.w.players.get_mut(&t) {
                tp.stats.contracts_defaulted += 1;
            }
            let lost = if forfeit > 0 {
                format!(" Your {forfeit}-credit stake went to the poster.")
            } else {
                String::new()
            };
            ctx.notify(t, "contract", format!("Contract #{id} ({what}) failed: deadline passed.{lost}"));
        }
    }
}

/// A kill: settle kill contracts held by the killer on that victim; escort contracts fail.
pub fn on_kill(ctx: &mut Ctx, killer: PlayerId, victim: PlayerId) {
    let done: Vec<u64> = ctx
        .w
        .contracts
        .values()
        .filter(|c| {
            c.status == ContractStatus::Taken && c.kind == ContractKind::Kill && c.taker == Some(killer) && c.target == Some(victim)
        })
        .map(|c| c.id)
        .collect();
    for id in done {
        complete(ctx, id);
    }
    let lost: Vec<u64> = ctx
        .w
        .contracts
        .values()
        .filter(|c| c.status == ContractStatus::Taken && c.kind == ContractKind::Escort && c.target == Some(victim))
        .map(|c| c.id)
        .collect();
    for id in lost {
        settle(ctx, id, ContractStatus::Failed);
    }
}

/// Cargo moved onto (+) or off (-) a planet by `pid`: progress haul contracts.
pub fn on_cargo(ctx: &mut Ctx, pid: PlayerId, planet: PlanetId, commodity: Commodity, delta: i64) {
    let ids: Vec<u64> = ctx
        .w
        .contracts
        .values()
        .filter(|c| {
            c.status == ContractStatus::Taken
                && c.kind == ContractKind::Haul
                && c.taker == Some(pid)
                && c.planet == Some(planet)
                && c.commodity == Some(commodity)
        })
        .map(|c| c.id)
        .collect();
    for id in ids {
        let c = ctx.w.contracts.get_mut(&id).unwrap();
        c.delivered = (c.delivered + delta).max(0);
        if c.delivered >= c.quantity {
            complete(ctx, id);
        }
    }
}

/// Does `pid` hold a haul contract for this planet and commodity?
pub fn hauls_to(w: &World, pid: PlayerId, planet: PlanetId, commodity: Commodity) -> bool {
    w.contracts.values().any(|c| {
        c.status == ContractStatus::Taken
            && c.kind == ContractKind::Haul
            && c.taker == Some(pid)
            && c.planet == Some(planet)
            && c.commodity == Some(commodity)
    })
}

/// Defend/escort checks, deadlines and lapsed offers.
pub fn tick(ctx: &mut Ctx) {
    let t = ctx.w.tick;
    let ids: Vec<u64> = ctx
        .w
        .contracts
        .values()
        .filter(|c| matches!(c.status, ContractStatus::Open | ContractStatus::Taken))
        .map(|c| c.id)
        .collect();
    for id in ids {
        let c = ctx.w.contracts[&id].clone();
        if c.status == ContractStatus::Open {
            if t >= c.expires_tick {
                settle(ctx, id, ContractStatus::Expired);
            }
            continue;
        }
        let taker = c.taker.unwrap_or(0);
        let since = c.taken_tick.unwrap_or(t);
        if matches!(c.kind, ContractKind::Defend | ContractKind::Escort) && t > since && (t - since).is_multiple_of(CHECK_EVERY) {
            let ok = match c.kind {
                ContractKind::Defend => c
                    .sector
                    .and_then(|s| ctx.w.sector(s))
                    .and_then(|s| s.fighters.as_ref())
                    .is_some_and(|f| f.count > 0 && ctx.w.friendly(f.owner, taker)),
                _ => {
                    let client = c.target.and_then(|p| ctx.w.players.get(&p)).map(|p| p.sector);
                    let me = ctx.w.players.get(&taker).map(|p| p.sector);
                    client.is_some() && client == me
                }
            };
            let cm = ctx.w.contracts.get_mut(&id).unwrap();
            cm.checks += 1;
            if ok {
                cm.checks_ok += 1;
            }
        }
        if c.deadline.is_some_and(|d| t >= d) {
            let c = ctx.w.contracts[&id].clone();
            let pass = match c.kind {
                ContractKind::Defend => c.checks > 0 && c.checks_ok * 10 >= c.checks * 8,
                ContractKind::Escort => c.checks > 0 && c.checks_ok * 10 >= c.checks * 7,
                _ => false,
            };
            if pass {
                complete(ctx, id);
            } else {
                settle(ctx, id, ContractStatus::Failed);
            }
        }
    }
    // Forget old closed contracts.
    let closed: Vec<u64> = ctx
        .w
        .contracts
        .values()
        .filter(|c| !matches!(c.status, ContractStatus::Open | ContractStatus::Taken))
        .map(|c| c.id)
        .collect();
    if closed.len() > KEEP_CLOSED {
        for id in &closed[..closed.len() - KEEP_CLOSED] {
            ctx.w.contracts.remove(id);
        }
    }
}

pub fn contract_json(w: &World, c: &Contract) -> Value {
    json!({
        "id": c.id, "kind": c.kind, "what": describe(w, c), "poster": w.player_name(c.poster),
        "taker": c.taker.map(|t| w.player_name(t)), "target": c.target.map(|t| w.player_name(t)),
        "reward": c.reward, "status": c.status, "terms": crate::query::untrusted(&c.terms),
        "posted_tick": c.posted_tick, "expires_tick": c.expires_tick, "deadline": c.deadline,
        "progress": match c.kind {
            ContractKind::Haul => json!({"delivered": c.delivered, "quantity": c.quantity}),
            ContractKind::Defend | ContractKind::Escort => json!({"checks_ok": c.checks_ok, "checks": c.checks}),
            _ => Value::Null,
        },
    })
}

/// Open and in-progress contracts, plus recently closed ones (public).
pub fn listing(w: &World) -> Vec<Value> {
    let mut v: Vec<&Contract> = w.contracts.values().collect();
    v.sort_by_key(|c| (matches!(c.status, ContractStatus::Open | ContractStatus::Taken) as u8, c.id));
    v.iter().rev().take(60).map(|c| contract_json(w, c)).collect()
}
