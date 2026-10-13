//! Route finding: over a player's *known* chart (fog-respecting) and over
//! the real graph (probes, TransWarp distance, NPCs).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};

use crate::knowledge;
use crate::model::*;

/// Shortest hop path over the true lane graph.
pub fn real_path(w: &World, from: SectorId, to: SectorId, max_hops: u32) -> Option<Vec<SectorId>> {
    if from == to {
        return Some(vec![from]);
    }
    let mut prev: BTreeMap<SectorId, SectorId> = BTreeMap::new();
    let mut depth: BTreeMap<SectorId, u32> = BTreeMap::from([(from, 0)]);
    let mut q = VecDeque::from([from]);
    while let Some(u) = q.pop_front() {
        let d = depth[&u];
        if d >= max_hops {
            continue;
        }
        for &v in &w.sector(u)?.warps {
            if depth.contains_key(&v) {
                continue;
            }
            depth.insert(v, d + 1);
            prev.insert(v, u);
            if v == to {
                let mut path = vec![to];
                let mut c = to;
                while let Some(&p) = prev.get(&c) {
                    path.push(p);
                    c = p;
                }
                path.reverse();
                return Some(path);
            }
            q.push_back(v);
        }
    }
    None
}

pub fn real_distance(w: &World, from: SectorId, to: SectorId) -> Option<u32> {
    real_path(w, from, to, 200).map(|p| p.len() as u32 - 1)
}

#[derive(Clone, Debug)]
pub struct Plotted {
    pub path: Vec<SectorId>,
    /// Hazardous sectors (by last observation) that the path still crosses.
    pub hazards: Vec<SectorId>,
}

/// Dijkstra over the known chart. Edges exist only out of sectors whose warp
/// lanes are known. Hazard sectors are avoided (or heavily penalised if
/// `avoid_hazards` is false they are allowed at normal cost).
pub fn plot(w: &World, pid: PlayerId, from: SectorId, to: SectorId, avoid: &BTreeSet<SectorId>, avoid_hazards: bool) -> Option<Plotted> {
    let chart = knowledge::merged(w, pid);
    plot_on(w, pid, &chart, from, to, avoid, avoid_hazards)
}

pub fn plot_on(
    w: &World,
    pid: PlayerId,
    chart: &BTreeMap<SectorId, KnownSector>,
    from: SectorId,
    to: SectorId,
    avoid: &BTreeSet<SectorId>,
    avoid_hazards: bool,
) -> Option<Plotted> {
    plot_within(w, pid, chart, from, to, avoid, avoid_hazards, 0)
}

/// `plot_on` for a pilot who pays charted tolls up to `toll_cap` each: those sectors are not
/// hazards, and among equally short routes the one with fewer of them wins. With `toll_cap` 0 it
/// is exactly `plot_on` (every cost is scaled by the same factor, which keeps the same choices).
#[allow(clippy::too_many_arguments)]
pub fn plot_within(
    w: &World,
    pid: PlayerId,
    chart: &BTreeMap<SectorId, KnownSector>,
    from: SectorId,
    to: SectorId,
    avoid: &BTreeSet<SectorId>,
    avoid_hazards: bool,
    toll_cap: i64,
) -> Option<Plotted> {
    const HOP: u64 = 1_000;
    if from == to {
        return Some(Plotted {
            path: vec![from],
            hazards: vec![],
        });
    }
    let hazard = |s: SectorId| {
        chart
            .get(&s)
            .map(|k| knowledge::known_hazard_within(w, pid, k, toll_cap))
            .unwrap_or(false)
    };
    let toll = |s: SectorId| chart.get(&s).is_some_and(|k| knowledge::known_payable_toll(w, pid, k, toll_cap));
    let mut dist: BTreeMap<SectorId, u64> = BTreeMap::from([(from, 0)]);
    let mut prev: BTreeMap<SectorId, SectorId> = BTreeMap::new();
    let mut heap = BinaryHeap::from([Reverse((0u64, from))]);
    while let Some(Reverse((d, u))) = heap.pop() {
        if u == to {
            break;
        }
        if d > *dist.get(&u).unwrap_or(&u64::MAX) {
            continue;
        }
        // The real lanes are visible only for the sector the pilot is standing in.
        let here = w.players.get(&pid).map(|p| p.sector) == Some(from);
        let warps: Vec<SectorId> = if u == from && here {
            w.sector(u).map(|s| s.warps.clone()).unwrap_or_default()
        } else {
            match chart.get(&u).and_then(|k| k.warps.clone()) {
                Some(ws) => ws,
                None => continue,
            }
        };
        for v in warps {
            if avoid.contains(&v) && v != to {
                continue;
            }
            let mut cost = HOP;
            if hazard(v) {
                if avoid_hazards && v != to {
                    continue;
                }
                cost += 50 * HOP;
            } else if toll(v) {
                // Fewer than HOP tolls can never make a route a hop longer: a tie-break only.
                cost += 1;
            }
            let nd = d + cost;
            if nd < *dist.get(&v).unwrap_or(&u64::MAX) {
                dist.insert(v, nd);
                prev.insert(v, u);
                heap.push(Reverse((nd, v)));
            }
        }
    }
    if !prev.contains_key(&to) {
        return None;
    }
    let mut path = vec![to];
    let mut c = to;
    while let Some(&p) = prev.get(&c) {
        path.push(p);
        c = p;
    }
    path.reverse();
    let hazards = path.iter().copied().filter(|s| *s != from && hazard(*s)).collect();
    Some(Plotted { path, hazards })
}

/// Hop distances from `from` over the known chart (BFS, up to `max_hops`).
/// `real_start`: use the true lanes of `from` (only when the player is standing there).
pub fn known_distances(
    chart: &BTreeMap<SectorId, KnownSector>,
    w: &World,
    from: SectorId,
    max_hops: u32,
    real_start: bool,
) -> BTreeMap<SectorId, u32> {
    let mut depth = BTreeMap::from([(from, 0u32)]);
    let mut q = VecDeque::from([from]);
    while let Some(u) = q.pop_front() {
        let d = depth[&u];
        if d >= max_hops {
            continue;
        }
        let warps = if u == from && real_start {
            w.sector(u).map(|s| s.warps.clone()).unwrap_or_default()
        } else {
            chart.get(&u).and_then(|k| k.warps.clone()).unwrap_or_default()
        };
        for v in warps {
            if let std::collections::btree_map::Entry::Vacant(e) = depth.entry(v) {
                e.insert(d + 1);
                q.push_back(v);
            }
        }
    }
    depth
}
