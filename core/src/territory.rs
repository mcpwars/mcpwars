//! Connected corporate territory (engine 1.40).
//!
//! - A corporation **holds** a sector when the fighter stack there belongs to a member (the
//!   stack's owner, as for jurisdiction). FedSpace is never held.
//! - Its **seat** is the sector its territory grows from. Its government sets one
//!   (`government action=set_seat`: the CEO of an autocracy at once, a republic by measure), at
//!   most once per `territory_seat_cooldown_ticks`, and only in a sector it holds. Until one is
//!   set, the seat is its largest stack (ties to the lowest sector id).
//! - Its **connected territory** is the held sectors joined to the seat through held sectors; a
//!   lane in either direction connects. Every other held sector is an **outpost**: fighters, but
//!   no law and no corporate bonuses. A set seat that is no longer held leaves no connected
//!   territory at all until the corporation holds it again or sets another.
//! - What territory does: laws hold only in it (`government::jurisdiction`); the corporate toll
//!   bonus is paid only on tolls collected in it; a concession a member holds in it pays
//!   `territory_concession_bonus_bp` of its yield into the free funds; and a held seat with a
//!   member's planet of citadel `territory_depot_citadel` or more is a seat depot that sells
//!   fighters, shields and holds at Stardock prices plus `territory_depot_markup_bp`.
//!
//! Computing a corporation's territory is one pass over the sectors plus a search over its held
//! sectors' lanes. Sector ids are never public: public views carry counts only.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Value, json};

use crate::cmd::EquipmentItem;
use crate::model::*;

/// One corporation's territory now.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Territory {
    /// The seat in effect: the one its government set, or else its largest stack.
    pub seat: Option<SectorId>,
    /// Whether its government set the seat (false: the default seat).
    pub seat_explicit: bool,
    /// Whether the corporation holds its seat now (a default seat always is).
    pub seat_held: bool,
    /// Held sectors joined to the seat through held sectors (the seat included).
    pub connected: BTreeSet<SectorId>,
    /// Held sectors outside the connected territory.
    pub outposts: BTreeSet<SectorId>,
}

impl Territory {
    pub fn contains(&self, sid: SectorId) -> bool {
        self.connected.contains(&sid)
    }
}

/// Items a seat depot sells.
pub const DEPOT_ITEMS: [EquipmentItem; 3] = [EquipmentItem::Fighters, EquipmentItem::Shields, EquipmentItem::Holds];

/// The corporation whose member owns the fighters in `sid` (FedSpace: none).
pub fn holder(w: &World, sid: SectorId) -> Option<CorpId> {
    let s = w.sector(sid).filter(|s| !s.fedspace)?;
    let f = s.fighters.as_ref().filter(|f| f.count > 0)?;
    w.players.get(&f.owner)?.corp.filter(|c| w.corps.contains_key(c))
}

/// Every corporation's held sectors and their fighters, in one pass over the sectors.
fn held_all(w: &World) -> BTreeMap<CorpId, BTreeMap<SectorId, i64>> {
    let mut out: BTreeMap<CorpId, BTreeMap<SectorId, i64>> = BTreeMap::new();
    for s in &w.sectors {
        if let Some(c) = holder(w, s.id) {
            let n = s.fighters.as_ref().map(|f| f.count).unwrap_or(0);
            out.entry(c).or_default().insert(s.id, n);
        }
    }
    out
}

/// The territory from a corporation's held sectors.
fn build(w: &World, c: &Corp, held: &BTreeMap<SectorId, i64>) -> Territory {
    let (seat, explicit) = match c.seat {
        Some(s) => (Some(s), true),
        // The largest stack; the first in id order wins a tie.
        None => {
            let best = held.values().copied().max();
            (best.and_then(|b| held.iter().find(|(_, n)| **n == b).map(|(s, _)| *s)), false)
        }
    };
    let seat_held = seat.is_some_and(|s| held.contains_key(&s));
    let mut connected = BTreeSet::new();
    if let Some(start) = seat.filter(|_| seat_held) {
        // Lanes between held sectors, either direction: each such lane is in one of their lists.
        let mut adj: BTreeMap<SectorId, Vec<SectorId>> = BTreeMap::new();
        for &u in held.keys() {
            for &v in w.sector(u).map(|s| s.warps.as_slice()).unwrap_or(&[]) {
                if v != u && held.contains_key(&v) {
                    adj.entry(u).or_default().push(v);
                    adj.entry(v).or_default().push(u);
                }
            }
        }
        connected.insert(start);
        let mut q = VecDeque::from([start]);
        while let Some(u) = q.pop_front() {
            for &v in adj.get(&u).map(Vec::as_slice).unwrap_or(&[]) {
                if connected.insert(v) {
                    q.push_back(v);
                }
            }
        }
    }
    let outposts = held.keys().copied().filter(|s| !connected.contains(s)).collect();
    Territory {
        seat,
        seat_explicit: explicit,
        seat_held,
        connected,
        outposts,
    }
}

/// One corporation's territory (empty for an unknown corporation).
pub fn of(w: &World, corp: CorpId) -> Territory {
    let Some(c) = w.corps.get(&corp) else { return Territory::default() };
    let mut held = BTreeMap::new();
    for s in &w.sectors {
        if holder(w, s.id) == Some(corp) {
            held.insert(s.id, s.fighters.as_ref().map(|f| f.count).unwrap_or(0));
        }
    }
    build(w, c, &held)
}

/// Every corporation's territory, from a single pass over the sectors.
pub fn all(w: &World) -> BTreeMap<CorpId, Territory> {
    let held = held_all(w);
    let empty = BTreeMap::new();
    w.corps
        .values()
        .map(|c| (c.id, build(w, c, held.get(&c.id).unwrap_or(&empty))))
        .collect()
}

/// Whether `sid` is in `corp`'s connected territory.
pub fn connected(w: &World, corp: CorpId, sid: SectorId) -> bool {
    // Cheap test first: a sector the corporation does not hold is never connected.
    holder(w, sid) == Some(corp) && of(w, corp).contains(sid)
}

/// The seat depot's planet: a member's planet in a held seat with a high enough citadel.
pub fn depot(w: &World, c: &Corp, t: &Territory) -> Option<PlanetId> {
    let seat = t.seat.filter(|_| t.seat_held)?;
    let need = w.rules.territory_depot_citadel;
    w.sector(seat)?
        .planets
        .iter()
        .filter_map(|id| w.planets.get(id))
        .filter(|pl| pl.citadel >= need && pl.owner.is_some_and(|o| c.members.contains(&o)))
        .map(|pl| pl.id)
        .min()
}

/// The seat depot `pid` could use: its corporation's, with the seat sector.
pub fn depot_of(w: &World, pid: PlayerId) -> Option<(SectorId, PlanetId)> {
    let c = w.players.get(&pid)?.corp.and_then(|c| w.corps.get(&c))?;
    let t = of(w, c.id);
    depot(w, c, &t).map(|pl| (t.seat.unwrap_or(0), pl))
}

/// When the seat may next change (None: now).
pub fn cooldown_ends(w: &World, c: &Corp) -> Option<Tick> {
    let end = c.seat_set_tick? + w.rules.territory_seat_cooldown_ticks;
    (w.tick < end).then_some(end)
}

fn percent(bp: i64) -> String {
    let v = bp as f64 / 100.0;
    if v.fract() == 0.0 { format!("{v:.0}%") } else { format!("{v}%") }
}

/// The rule in one paragraph, from the live rules.
pub fn rule(w: &World) -> String {
    let r = &w.rules;
    format!(
        "Your corporation holds a sector where a member's fighters stand. Its connected territory is the held sectors joined to its seat through held sectors (a lane either way connects); the rest are outposts. Laws and the {} toll bonus hold only in connected territory; a member's concession there pays {} of its yield more into the free funds; a member's planet with citadel {}+ in a held seat is a depot selling fighters, shields and holds at Stardock prices +{}. Until your government sets a seat (government action=set_seat id=<sector you hold>; default: where you are; once per {} ticks) the seat is your largest stack. Lose a set seat and all territory is outposts until you hold it again or set another.",
        percent(r.corp_toll_bonus_bp),
        percent(r.territory_concession_bonus_bp),
        r.territory_depot_citadel,
        percent(r.territory_depot_markup_bp),
        r.territory_seat_cooldown_ticks
    )
}

/// What anyone may see: counts and flags, never a sector.
pub fn public_counts(w: &World, c: &Corp, t: &Territory) -> Value {
    json!({
        "connected_sectors": t.connected.len(), "outposts": t.outposts.len(),
        "has_seat": t.seat_held, "seat_chosen": t.seat_explicit, "depot": depot(w, c, t).is_some(),
    })
}

/// A member's view (and the operator's): the sectors themselves.
pub fn member_view(w: &World, c: &Corp, t: &Territory) -> Value {
    let depot = depot(w, c, t).map(|pl| {
        json!({"planet": pl, "sector": t.seat, "markup_percent": w.rules.territory_depot_markup_bp as f64 / 100.0,
               "sells": ["fighters", "shields", "holds"], "how": "buy_equipment in the seat sector"})
    });
    json!({
        "seat": t.seat, "seat_chosen": t.seat_explicit, "seat_held": t.seat_held,
        "connected": t.connected, "outposts": t.outposts,
        "connected_sectors": t.connected.len(), "outpost_count": t.outposts.len(),
        "depot": depot, "seat_cooldown_ends_tick": cooldown_ends(w, c),
        "rule": rule(w),
    })
}

/// The briefing's one line.
pub fn briefing_line(w: &World, c: &Corp, t: &Territory) -> String {
    let seat = match (t.seat, t.seat_held, t.seat_explicit) {
        (None, _, _) => "no seat (deploy fighters)".to_string(),
        (Some(s), true, true) => format!("seat {s} held"),
        (Some(s), true, false) => format!("seat {s} held (default: your largest stack)"),
        (Some(s), false, _) => format!("seat {s} LOST: no connected territory until you hold it or set another"),
    };
    format!(
        "Territory: {} connected sector{}, {} outpost{}; {seat}; depot {}. Laws and corporate bonuses hold only in connected territory: government action=view.",
        t.connected.len(),
        if t.connected.len() == 1 { "" } else { "s" },
        t.outposts.len(),
        if t.outposts.len() == 1 { "" } else { "s" },
        if depot(w, c, t).is_some() { "yes" } else { "no" }
    )
}
