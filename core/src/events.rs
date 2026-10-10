//! Galaxy events: reasons to act and to talk. Every few hours the Federation announces one:
//! a market boom at a named port, a bounty on a named Skarn raider, or a salvage cache the
//! first captain to arrive can take. Announced in the news and on public radio, listed in
//! every briefing, deterministic (seeded from the world like everything else).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::Ctx;
use crate::model::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GalaxyEventKind {
    /// The port in `sector` pays `pct`% more for one commodity it buys.
    Boom { sector: SectorId, commodity: usize, pct: i64 },
    /// The Federation pays `bonus` credits to whoever destroys this Skarn raider.
    SkarnHunt { skarn: PlayerId, bonus: i64 },
    /// `credits` wait in `sector` for the first captain to arrive.
    Salvage { sector: SectorId, credits: i64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GalaxyEvent {
    pub id: u64,
    pub started_tick: Tick,
    pub ends_tick: Tick,
    pub kind: GalaxyEventKind,
    /// Who claimed it (hunts and salvage end when claimed).
    #[serde(default)]
    pub claimed_by: Option<PlayerId>,
}

impl GalaxyEvent {
    fn live(&self, t: Tick) -> bool {
        t < self.ends_tick && self.claimed_by.is_none()
    }
}

fn hours(rules: &crate::rules::Rules, ticks: u64) -> u64 {
    let per_hour = (crate::player::ticks_per_day(rules) / 24).max(1) as u64;
    (ticks / per_hour).max(1)
}

/// Bonus percent a boom adds to what the port in `sector` pays for `commodity` (0 if none).
pub fn boom_pct(w: &World, sector: SectorId, commodity: usize) -> i64 {
    w.galaxy_events
        .iter()
        .filter(|e| e.live(w.tick))
        .filter_map(|e| match e.kind {
            GalaxyEventKind::Boom {
                sector: s,
                commodity: c,
                pct,
            } if s == sector && c == commodity => Some(pct),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// Spawn a new event when one is due. Called every tick.
pub fn tick(ctx: &mut Ctx) {
    let t = ctx.w.tick;
    let rules = ctx.w.rules.clone();
    let excess = ctx.w.galaxy_events.len().saturating_sub(50);
    if excess > 0 {
        ctx.w.galaxy_events.drain(0..excess);
    }
    if rules.event_every_ticks == 0 || !t.is_multiple_of(rules.event_every_ticks) || ctx.w.season.finished {
        return;
    }
    let active = ctx.w.galaxy_events.iter().filter(|e| e.live(t)).count();
    if active >= rules.event_max_active {
        return;
    }
    // Rotate through the kinds so each shows up regularly; skip any that cannot happen now.
    let start = (ctx.w.next_galaxy_event % 3) as usize;
    for k in 0..3 {
        if spawn(ctx, (start + k) % 3) {
            return;
        }
    }
}

fn spawn(ctx: &mut Ctx, which: usize) -> bool {
    let t = ctx.w.tick;
    let rules = ctx.w.rules.clone();
    let (kind, ends, text) = match which {
        0 => {
            // A port that buys something, outside FedSpace, not already booming.
            let ports: Vec<(SectorId, usize, String)> = ctx
                .w
                .sectors
                .iter()
                .filter(|s| !s.fedspace)
                .filter_map(|s| s.port.as_ref().filter(|p| !p.stardock).map(|p| (s.id, p)))
                .flat_map(|(sid, p)| {
                    (0..3)
                        .filter(move |i| p.trades[*i] == PortTrade::Buys)
                        .map(move |i| (sid, i, p.name.clone()))
                })
                .collect();
            let Some((sector, commodity, name)) = ctx.rng.pick(&ports).cloned() else {
                return false;
            };
            if boom_pct(ctx.w, sector, commodity) > 0 {
                return false;
            }
            let pct = rules.event_boom_pct;
            let ends = t + rules.event_boom_ticks;
            let text = format!(
                "Market boom: {name} in sector {sector} pays {pct}% more for {} for the next {} hours.",
                Commodity::ALL[commodity].name(),
                hours(&rules, rules.event_boom_ticks)
            );
            (GalaxyEventKind::Boom { sector, commodity, pct }, ends, text)
        }
        1 => {
            let hunted: Vec<PlayerId> = ctx
                .w
                .galaxy_events
                .iter()
                .filter(|e| e.live(t))
                .filter_map(|e| match e.kind {
                    GalaxyEventKind::SkarnHunt { skarn, .. } => Some(skarn),
                    _ => None,
                })
                .collect();
            let skarn: Vec<(PlayerId, String, i64)> = ctx
                .w
                .players
                .values()
                .filter(|p| p.npc.as_ref().is_some_and(|b| b.role == NpcRole::Skarn) && !hunted.contains(&p.id))
                .map(|p| (p.id, p.name.clone(), p.ship.fighters))
                .collect();
            let Some((id, name, fighters)) = ctx.rng.pick(&skarn).cloned() else {
                return false;
            };
            let bonus = rules.event_hunt_bonus;
            let ends = t + rules.event_hunt_ticks;
            let text = format!(
                "Skarn hunt: the Federation pays {bonus} credits, on top of the usual reward, to whoever destroys {name} ({fighters} fighters) in the next {} hours.",
                hours(&rules, rules.event_hunt_ticks)
            );
            (GalaxyEventKind::SkarnHunt { skarn: id, bonus }, ends, text)
        }
        _ => {
            // A quiet frontier sector: no port, outside FedSpace.
            let quiet: Vec<SectorId> = ctx
                .w
                .sectors
                .iter()
                .filter(|s| !s.fedspace && s.port.is_none())
                .map(|s| s.id)
                .collect();
            let Some(sector) = ctx.rng.pick(&quiet).copied() else {
                return false;
            };
            let credits = rules.event_salvage_credits;
            let ends = t + rules.event_salvage_ticks;
            let text = format!(
                "Salvage: a derelict freighter's cargo, worth {credits} credits, drifts in sector {sector}. The first captain to arrive within {} hours takes it.",
                hours(&rules, rules.event_salvage_ticks)
            );
            (GalaxyEventKind::Salvage { sector, credits }, ends, text)
        }
    };
    ctx.w.next_galaxy_event += 1;
    let id = ctx.w.next_galaxy_event;
    ctx.w.galaxy_events.push(GalaxyEvent {
        id,
        started_tick: t,
        ends_tick: ends,
        kind,
        claimed_by: None,
    });
    announce(ctx, text);
    true
}

fn announce(ctx: &mut Ctx, text: String) {
    ctx.news(text.clone());
    crate::social::system_radio(ctx, Channel::Public, format!("FEDERATION BULLETIN: {text}"));
    ctx.event("galaxy_event", None, vec![], true, text, json!({}));
}

/// A captain arrived in `sector`: take any salvage waiting there.
pub fn on_arrive(ctx: &mut Ctx, pid: PlayerId, sector: SectorId) {
    let t = ctx.w.tick;
    let Some(p) = ctx.w.players.get(&pid) else { return };
    if p.npc.is_some() {
        return;
    }
    let name = p.name.clone();
    let Some(e) = ctx
        .w
        .galaxy_events
        .iter_mut()
        .find(|e| e.live(t) && matches!(e.kind, GalaxyEventKind::Salvage { sector: s, .. } if s == sector))
    else {
        return;
    };
    let GalaxyEventKind::Salvage { credits, .. } = e.kind else { return };
    e.claimed_by = Some(pid);
    ctx.player_mut(pid).credits += credits;
    ctx.notify(pid, "reward", format!("You recovered the salvage: {credits} credits."));
    announce(ctx, format!("{name} reached the salvage first and took {credits} credits."));
}

/// A Skarn raider was destroyed: pay any hunt bonus on it.
pub fn on_skarn_destroyed(ctx: &mut Ctx, killer: PlayerId, skarn: PlayerId) {
    let t = ctx.w.tick;
    let Some(k) = ctx.w.players.get(&killer) else { return };
    if k.npc.is_some() {
        return;
    }
    let name = k.name.clone();
    let Some(e) = ctx
        .w
        .galaxy_events
        .iter_mut()
        .find(|e| e.live(t) && matches!(e.kind, GalaxyEventKind::SkarnHunt { skarn: s, .. } if s == skarn))
    else {
        return;
    };
    let GalaxyEventKind::SkarnHunt { bonus, .. } = e.kind else { return };
    e.claimed_by = Some(killer);
    ctx.player_mut(killer).credits += bonus;
    ctx.notify(killer, "reward", format!("The Federation paid you a {bonus}-credit hunt bonus."));
    announce(ctx, format!("{name} won the Skarn hunt and the {bonus}-credit bonus."));
}

/// Live events, for captains (with sectors) and the public (text without sector numbers).
pub fn active(w: &World, public: bool) -> Vec<Value> {
    w.galaxy_events
        .iter()
        .filter(|e| e.live(w.tick))
        .map(|e| {
            let (kind, text) = match &e.kind {
                GalaxyEventKind::Boom { sector, commodity, pct } => (
                    "boom",
                    format!(
                        "{} in sector {sector} pays {pct}% more for {}.",
                        w.sector(*sector)
                            .and_then(|s| s.port.as_ref())
                            .map(|p| p.name.clone())
                            .unwrap_or_default(),
                        Commodity::ALL[*commodity].name()
                    ),
                ),
                GalaxyEventKind::SkarnHunt { skarn, bonus } => (
                    "skarn_hunt",
                    format!("{bonus} credits to whoever destroys {}.", w.player_name(*skarn)),
                ),
                GalaxyEventKind::Salvage { sector, credits } => (
                    "salvage",
                    format!("{credits} credits of salvage in sector {sector}, first to arrive."),
                ),
            };
            let text = if public { crate::public::scrub(&text) } else { text };
            json!({"id": e.id, "kind": kind, "text": text, "ends_tick": e.ends_tick})
        })
        .collect()
}
