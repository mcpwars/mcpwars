//! Fog of war: each player keeps a private chart; corporations share theirs.

use std::collections::BTreeMap;

use crate::economy;
use crate::model::*;

/// Density reading of a live sector (additive scale).
pub fn density(w: &World, sid: SectorId) -> i64 {
    let Some(s) = w.sector(sid) else { return 0 };
    let mut d = 0;
    if s.port.is_some() {
        d += 100;
    }
    d += 500 * s.planets.len() as i64;
    if let Some(f) = &s.fighters {
        d += 5 * f.count;
    }
    if let Some(m) = &s.armid {
        d += 10 * m.count;
    }
    if let Some(m) = &s.limpet {
        d += 2 * m.count;
    }
    if s.beacon.is_some() {
        d += 1;
    }
    d += 40 * w.players.values().filter(|p| p.sector == sid).count() as i64;
    d
}

pub fn port_intel(w: &World, port: &Port) -> PortIntel {
    let mut prices = [0; 3];
    let mut fill = [0; 3];
    for i in 0..3 {
        prices[i] = economy::quote(&w.rules, port, i);
        fill[i] = port.stock[i] * 100 / port.capacity[i].max(1);
    }
    PortIntel {
        name: port.name.clone(),
        class: port.class,
        code: port.code(),
        prices,
        fill_pct: fill,
        stardock: port.stardock,
    }
}

/// Record what `pid` can see of sector `sid`. `visited` = physically present
/// (warp lanes become known with certainty); otherwise a remote observation.
pub fn observe(w: &mut World, pid: PlayerId, sid: SectorId, visited: bool) {
    let Some(sector) = w.sector(sid) else { return };
    let tick = w.tick;
    let port = sector.port.as_ref().map(|p| port_intel(w, p));
    let planets = sector.planets.clone();
    let fighters = sector.fighters.as_ref().map(|f| (f.owner, f.count, f.mode));
    let toll = sector
        .fighters
        .as_ref()
        .filter(|f| f.mode == FighterMode::Toll)
        .map(|f| f.toll(w.rules.toll_per_fighter));
    let mines = sector.armid.as_ref().map(|m| m.owner);
    let planet_guard = sector
        .planets
        .iter()
        .filter_map(|id| w.planets.get(id))
        .find(|pl| pl.citadel >= 2 && pl.fighters > 0)
        .and_then(|pl| pl.owner);
    let warps = sector.warps.clone();
    let fedspace = sector.fedspace;
    let nebula = sector.nebula.clone();
    let beacon = sector.beacon.as_ref().map(|b| b.message.clone());
    let dens = density(w, sid);
    let Some(p) = w.players.get_mut(&pid) else { return };
    let first = !p.knowledge.get(&sid).map(|k| k.visited).unwrap_or(false);
    let k = p.knowledge.entry(sid).or_default();
    k.last_seen = tick;
    k.visited |= visited;
    k.warps = Some(warps.clone());
    k.fedspace = fedspace;
    k.port = port;
    k.planets = planets;
    k.fighters = fighters;
    k.toll = toll;
    k.mines = mines;
    k.planet_guard = planet_guard;
    k.density = Some(dens);
    k.nebula = nebula;
    k.beacon = beacon;
    if visited && first {
        p.stats.sectors_explored += 1;
        p.experience += 2;
    }
    // Neighbours become known to exist (unexplored).
    for n in warps {
        p.knowledge.entry(n).or_default();
    }
}

/// A player's chart merged with their corp-mates' (most recent observation wins,
/// warp knowledge is unioned).
pub fn merged(w: &World, pid: PlayerId) -> BTreeMap<SectorId, KnownSector> {
    let Some(p) = w.players.get(&pid) else { return BTreeMap::new() };
    let mut out = p.knowledge.clone();
    if let Some(cid) = p.corp
        && let Some(c) = w.corps.get(&cid)
    {
        // Corp-mates, plus members of corporations with a chart-sharing treaty.
        let mut members = c.members.clone();
        for partner in crate::diplomacy::chart_partners(w, cid) {
            if let Some(pc) = w.corps.get(&partner) {
                members.extend(pc.members.iter().copied());
            }
        }
        for m in &members {
            if *m == pid {
                continue;
            }
            let Some(mp) = w.players.get(m) else { continue };
            for (sid, k) in &mp.knowledge {
                match out.get_mut(sid) {
                    None => {
                        out.insert(*sid, k.clone());
                    }
                    Some(mine) => {
                        let visited = mine.visited || k.visited;
                        let warps = mine.warps.clone().or_else(|| k.warps.clone());
                        if k.last_seen > mine.last_seen {
                            *mine = k.clone();
                        }
                        mine.visited = visited;
                        if mine.warps.is_none() {
                            mine.warps = warps;
                        }
                    }
                }
            }
        }
    }
    out
}

/// Single-sector lookup in the merged chart (cheaper than `merged`).
pub fn known(w: &World, pid: PlayerId, sid: SectorId) -> Option<KnownSector> {
    let p = w.players.get(&pid)?;
    let mut out = p.knowledge.get(&sid).cloned();
    if let Some(cid) = p.corp
        && let Some(c) = w.corps.get(&cid)
    {
        let mut members = c.members.clone();
        for partner in crate::diplomacy::chart_partners(w, cid) {
            if let Some(c) = w.corps.get(&partner) {
                members.extend(c.members.iter().copied());
            }
        }
        for m in members {
            if m == pid {
                continue;
            }
            let Some(k) = w.players.get(&m).and_then(|p| p.knowledge.get(&sid)) else {
                continue;
            };
            match &mut out {
                None => out = Some(k.clone()),
                Some(mine) => {
                    let visited = mine.visited || k.visited;
                    let warps = mine.warps.clone().or_else(|| k.warps.clone());
                    if k.last_seen > mine.last_seen {
                        *mine = k.clone();
                    }
                    mine.visited = visited;
                    if mine.warps.is_none() {
                        mine.warps = warps;
                    }
                }
            }
        }
    }
    out
}

/// Hostile hazard flag from a known observation, from `pid`'s perspective.
pub fn known_hazard(w: &World, pid: PlayerId, k: &KnownSector) -> bool {
    use crate::diplomacy::at_peace;
    if let Some((owner, _, mode)) = k.fighters
        && !at_peace(w, owner, pid)
        && !(mode == FighterMode::Toll && (k.toll == Some(0) || crate::diplomacy::toll_exempt(w, owner, pid)))
    {
        return true;
    }
    matches!(k.mines, Some(o) if !at_peace(w, o, pid)) || matches!(k.planet_guard, Some(o) if !at_peace(w, o, pid))
}
