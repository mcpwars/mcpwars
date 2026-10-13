//! Procedural universe generation. Pure function of (seed, sector count, rules).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::model::*;
use crate::rng::Rng;
use crate::rules::Rules;

#[derive(Clone, Debug)]
pub struct GenesisConfig {
    pub seed: u64,
    pub sectors: u32,
    pub rules: Rules,
}

impl GenesisConfig {
    pub fn new(seed: u64, sectors: u32) -> Self {
        GenesisConfig {
            seed,
            sectors,
            rules: Rules::default(),
        }
    }
}

const SYL_A: [&str; 24] = [
    "Al", "Bel", "Cor", "Dra", "Eri", "Fal", "Gal", "Hel", "Ix", "Jor", "Kal", "Lyr", "Mor", "Nex", "Or", "Pra", "Quel", "Ryn", "Sol",
    "Tor", "Ul", "Vex", "Wyr", "Zan",
];
const SYL_B: [&str; 20] = [
    "a", "e", "i", "o", "u", "ar", "en", "is", "on", "ul", "ax", "ea", "io", "or", "yn", "ae", "ir", "os", "um", "ex",
];
const SYL_C: [&str; 18] = [
    "dor", "nis", "tar", "rus", "lon", "mir", "vos", "thas", "dra", "kon", "pheus", "gard", "ria", "nova", "x", "th", "ra", "mus",
];
const PORT_SUFFIX: [&str; 10] = [
    "Station",
    "Exchange",
    "Depot",
    "Outpost",
    "Hub",
    "Emporium",
    "Market",
    "Terminal",
    "Bazaar",
    "Waystation",
];
const NEBULAE: [&str; 10] = [
    "Crimson Veil",
    "Orion Drift",
    "The Shroud",
    "Azure Expanse",
    "Widow's Reach",
    "Ember Cloud",
    "Silent Deep",
    "Gilded Mist",
    "The Maelstrom",
    "The Long Drift",
];

pub fn make_name(rng: &mut Rng) -> String {
    let mut s = String::new();
    s.push_str(rng.pick(&SYL_A).unwrap());
    s.push_str(rng.pick(&SYL_B).unwrap());
    if rng.chance(0.7) {
        s.push_str(rng.pick(&SYL_C).unwrap());
    }
    s
}

/// Port class -> (ore, organics, equipment) trade directions.
pub fn class_trades(class: u8) -> [PortTrade; 3] {
    use PortTrade::*;
    match class {
        1 => [Buys, Buys, Sells],
        2 => [Buys, Sells, Buys],
        3 => [Sells, Buys, Buys],
        4 => [Sells, Sells, Buys],
        5 => [Sells, Buys, Sells],
        6 => [Buys, Sells, Sells],
        7 => [Sells, Sells, Sells],
        _ => [Buys, Buys, Buys],
    }
}

/// A port: production `q` per commodity, capacity 10·q, starting about
/// half full, with a rolled trade strength per commodity.
pub fn make_port(rng: &mut Rng, rules: &Rules, class: u8, name: String) -> Port {
    let trades = class_trades(class);
    let mut capacity = [0i64; 3];
    let mut stock = [0i64; 3];
    let mut strength = [0i64; 3];
    for i in 0..3 {
        let q = rng.range(rules.port_production_min, rules.port_production_max);
        capacity[i] = 10 * q;
        stock[i] = capacity[i] * rng.range(40, 60) / 100;
        let (lo, hi) = rules.trade_strength[i];
        strength[i] = rng.range(lo, hi);
    }
    Port {
        name,
        class,
        trades,
        stock,
        capacity,
        strength,
        volume: [0; 3],
        last_trade: 0,
        stardock: false,
        black_market: false,
        contraband_demand: 0,
        regen_carry: [0; 3],
    }
}

fn dist2(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// A sector's height above or below the galactic plane, in the same units as `x` and `y`.
/// A pure function of the world seed, the sector id and its (stored, rounded) position, so
/// genesis, growth and the `layout_depth` backfill of an older flat galaxy all agree, and no
/// RNG stream is consumed. The galaxy is a thick disk: rolling hills that grow towards the rim,
/// an S-shaped twist of the outer disk (rising on one side, falling on the other) and a round
/// bulge at the core, with FedSpace in the plane. Heights change smoothly across the disk, so
/// lanes (chosen in the plane) stay short.
pub fn depth(seed: u64, id: SectorId, x: f64, y: f64, fedspace: bool) -> f64 {
    if fedspace {
        return 0.0;
    }
    let mut field = Rng::new(seed ^ 0x0DE9_7400_F1E1_D000);
    let tau = std::f64::consts::TAU;
    let mut angle = || field.f64() * tau;
    let (p1, p2, p3, a, tilt) = (angle(), angle(), angle(), angle(), angle());
    let mut own = Rng::new(seed ^ 0x0DE9_7400_5EC7_0000 ^ (id as u64).wrapping_mul(0xD1B5_4A32_D192_ED03));
    own.next_u64();
    let r = (x * x + y * y).sqrt();
    let hills =
        (22.0 + 0.18 * r) * (0.6 * (x / 48.0 + p1).sin() * (y / 55.0 + p2).cos() + 0.4 * ((x * a.cos() + y * a.sin()) / 31.0 + p3).sin());
    let twist = 0.2 * r * (r / 300.0).min(1.0) * (y.atan2(x) - tilt).sin();
    let spread = 5.0 + 35.0 * (-(r / 80.0).powi(2)).exp();
    let jitter = (own.f64() + own.f64() - 1.0) * spread;
    round1(hills + twist + jitter)
}

/// Give every sector its height (`depth`); x, y and lanes are unchanged. For a galaxy made
/// before engine 1.28 (admin command `layout_depth`); on any other it changes nothing.
/// Returns how many sectors changed.
pub fn layout_depth(w: &mut World) -> usize {
    let seed = w.seed;
    let mut changed = 0;
    for s in &mut w.sectors {
        let z = depth(seed, s.id, s.x, s.y, s.fedspace);
        if z != s.z {
            s.z = z;
            changed += 1;
        }
    }
    changed
}

/// Can every node reach `root` and be reached from it?
pub fn strongly_connected(adj: &[BTreeSet<usize>], root: usize) -> bool {
    let n = adj.len();
    let mut seen = vec![false; n];
    let mut q = VecDeque::from([root]);
    seen[root] = true;
    while let Some(u) = q.pop_front() {
        for &v in &adj[u] {
            if !seen[v] {
                seen[v] = true;
                q.push_back(v);
            }
        }
    }
    if seen.iter().any(|s| !s) {
        return false;
    }
    let mut radj = vec![Vec::new(); n];
    for (u, vs) in adj.iter().enumerate() {
        for &v in vs {
            radj[v].push(u);
        }
    }
    let mut seen = vec![false; n];
    let mut q = VecDeque::from([root]);
    seen[root] = true;
    while let Some(u) = q.pop_front() {
        for &v in &radj[u] {
            if !seen[v] {
                seen[v] = true;
                q.push_back(v);
            }
        }
    }
    seen.iter().all(|s| *s)
}

/// Give FedSpace at least `want` two-way gateway lanes to the frontier. Each new
/// gateway joins the FedSpace border sector angularly farthest from the existing
/// gateways to its nearest eligible frontier sector that is not yet linked into
/// FedSpace. Pure geometry, no RNG. Returns the new (fedspace, frontier) pairs.
fn add_fed_gateways(
    pos: &[(f64, f64)],
    adj: &mut [BTreeSet<usize>],
    fed: usize,
    want: usize,
    eligible: impl Fn(usize, &BTreeSet<usize>) -> bool,
) -> Vec<(usize, usize)> {
    use std::f64::consts::TAU;
    let angle = |i: usize| pos[i].1.atan2(pos[i].0);
    let mut gates: Vec<usize> = (1..fed).filter(|&b| adj[b].iter().any(|&j| j >= fed)).collect();
    let mut made = vec![];
    while gates.len() < want {
        let pick = (1..fed).filter(|b| !gates.contains(b)).max_by_key(|&b| {
            let gap = gates
                .iter()
                .map(|&g| {
                    let d = (angle(b) - angle(g)).abs();
                    d.min(TAU - d)
                })
                .fold(TAU, f64::min);
            ((gap * 1e6) as i64, std::cmp::Reverse(b))
        });
        let Some(b) = pick else { break };
        gates.push(b);
        let target = (fed..pos.len())
            .filter(|&j| !adj[j].iter().any(|&k| k < fed) && eligible(j, &adj[j]))
            .min_by_key(|&j| ((dist2(pos[b], pos[j]) * 1000.0) as i64, j));
        if let Some(j) = target {
            adj[b].insert(j);
            adj[j].insert(b);
            made.push((b, j));
        }
    }
    made
}

/// Retrofit FedSpace gateways onto a live galaxy (admin command). Frontier ends
/// avoid dead ends and sectors with planets. Pilots who charted either end
/// learn the new lane. Returns the new (fedspace, frontier) sector ids.
pub fn link_fedspace(w: &mut World, want: u32) -> Vec<(SectorId, SectorId)> {
    let fed = w.sectors.iter().take_while(|s| s.fedspace).count();
    let pos: Vec<(f64, f64)> = w.sectors.iter().map(|s| (s.x, s.y)).collect();
    let mut adj: Vec<BTreeSet<usize>> = w
        .sectors
        .iter()
        .map(|s| s.warps.iter().map(|&t| t as usize - 1).collect())
        .collect();
    let sectors = &w.sectors;
    let made = add_fed_gateways(&pos, &mut adj, fed, want as usize, |j, a| {
        a.len() >= 2 && a.len() < 6 && sectors[j].planets.is_empty()
    });
    let mut out = vec![];
    for (b, j) in made {
        let (sb, sj) = (b as SectorId + 1, j as SectorId + 1);
        for (from, to) in [(b, sj), (j, sb)] {
            let warps = &mut w.sectors[from].warps;
            if let Err(i) = warps.binary_search(&to) {
                warps.insert(i, to);
            }
        }
        for p in w.players.values_mut() {
            for (s, to) in [(sb, sj), (sj, sb)] {
                if let Some(ws) = p.knowledge.get_mut(&s).and_then(|k| k.warps.as_mut())
                    && let Err(i) = ws.binary_search(&to)
                {
                    ws.insert(i, to);
                }
            }
        }
        out.push((sb, sj));
    }
    out
}

pub fn generate(cfg: &GenesisConfig) -> World {
    let rules = cfg.rules.clone();
    let n = cfg.sectors.max(rules.fedspace_sectors + 10) as usize;
    let fed = rules.fedspace_sectors as usize;
    let mut rng = Rng::new(cfg.seed ^ 0x6A1A_C71C);

    // ---- positions: FedSpace clustered at the core, frontier spread in a disk.
    let radius = (n as f64).sqrt() * 10.0;
    let min_d2 = (radius * 0.9 / (n as f64).sqrt()).powi(2);
    let mut pos: Vec<(f64, f64)> = Vec::with_capacity(n);
    pos.push((0.0, 0.0));
    for i in 1..fed {
        let a = i as f64 / (fed - 1) as f64 * std::f64::consts::TAU;
        let r = radius * 0.09;
        pos.push((a.cos() * r, a.sin() * r));
    }
    while pos.len() < n {
        let mut best = None;
        for _attempt in 0..30 {
            let a = rng.f64() * std::f64::consts::TAU;
            let r = radius * (0.16 + 0.84 * rng.f64().sqrt());
            let p = (a.cos() * r, a.sin() * r);
            if pos.iter().all(|q| dist2(p, *q) >= min_d2) {
                best = Some(p);
                break;
            }
            best.get_or_insert(p);
        }
        pos.push(best.unwrap());
    }

    // ---- lanes: k-nearest neighbours (some nodes deliberately sparse = dead ends).
    let mut adj: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    let link = |adj: &mut Vec<BTreeSet<usize>>, a: usize, b: usize| {
        if a != b {
            adj[a].insert(b);
            adj[b].insert(a);
        }
    };
    // FedSpace: ring plus spokes to sector 1 (index 0).
    for i in 1..fed {
        link(&mut adj, 0, i);
        let next = if i + 1 < fed { i + 1 } else { 1 };
        link(&mut adj, i, next);
    }
    let mut order: Vec<usize> = (fed..n).collect();
    rng.shuffle(&mut order);
    // ~20% of frontier sectors are dead-end pockets (~20% one-warp
    // sectors). Other sectors never link into them, so they keep a single lane.
    let dead: BTreeSet<usize> = order.iter().copied().filter(|_| rng.chance(0.21)).collect();
    for &i in &order {
        let k = if dead.contains(&i) { 1 } else { rng.range(2, 3) as usize };
        let mut near: Vec<(i64, usize)> = (0..n)
            .filter(|&j| j != i)
            .map(|j| ((dist2(pos[i], pos[j]) * 1000.0) as i64, j))
            .collect();
        near.sort();
        let mut added = 0;
        for &(_, j) in &near {
            if added >= k {
                break;
            }
            if adj[j].len() >= 6 && !adj[i].is_empty() {
                continue;
            }
            // Keep FedSpace compact: only a few frontier links into it.
            if j < fed && j != 0 && adj[j].len() >= 4 {
                continue;
            }
            if j == 0 || dead.contains(&j) {
                continue;
            }
            link(&mut adj, i, j);
            added += 1;
        }
    }
    // Connect components (union by nearest pair to the component containing sector 1).
    loop {
        let mut comp = vec![usize::MAX; n];
        let mut q = VecDeque::from([0usize]);
        comp[0] = 0;
        while let Some(u) = q.pop_front() {
            for &v in &adj[u] {
                if comp[v] == usize::MAX {
                    comp[v] = 0;
                    q.push_back(v);
                }
            }
        }
        let outside: Vec<usize> = (0..n).filter(|&i| comp[i] != 0).collect();
        if outside.is_empty() {
            break;
        }
        let mut best = (f64::MAX, 0, 0);
        for &o in &outside {
            for i in 0..n {
                if comp[i] == 0 && i >= fed {
                    let d = dist2(pos[o], pos[i]);
                    if d < best.0 {
                        best = (d, o, i);
                    }
                }
            }
        }
        if best.0 == f64::MAX {
            // Only FedSpace is connected so far: hook to a FedSpace border sector.
            let o = outside[0];
            link(&mut adj, o, 1 + (o % (fed - 1)));
        } else {
            link(&mut adj, best.1, best.2);
        }
    }

    add_fed_gateways(&pos, &mut adj, fed, rules.fed_gateways as usize, |j, a| {
        !dead.contains(&j) && a.len() < 6
    });

    // ---- one-way lanes: drop reverse direction while keeping strong connectivity.
    let mut edges: Vec<(usize, usize)> = Vec::new();
    for (a, vs) in adj.iter().enumerate() {
        for &b in vs {
            if a < b && a >= fed && b >= fed {
                edges.push((a, b));
            }
        }
    }
    rng.shuffle(&mut edges);
    let target = edges.len() as i64 * rules.one_way_bp / 10_000;
    let mut made = 0;
    for (a, b) in edges {
        if made >= target {
            break;
        }
        let (from, to) = if rng.chance(0.5) { (a, b) } else { (b, a) };
        if adj[to].len() <= 1 {
            continue;
        }
        adj[to].remove(&from);
        if strongly_connected(&adj, 0) {
            made += 1;
        } else {
            adj[to].insert(from);
        }
    }
    // ---- wormholes: a handful of long-range one-way lanes.
    let wormholes = (n / 60).max(1);
    for _ in 0..wormholes {
        let a = rng.range(fed as i64, n as i64 - 1) as usize;
        let b = rng.range(fed as i64, n as i64 - 1) as usize;
        if a != b && adj[a].len() < 7 && dist2(pos[a], pos[b]) > (radius * 0.6).powi(2) {
            adj[a].insert(b);
        }
    }
    debug_assert!(strongly_connected(&adj, 0));

    // ---- nebulae (cosmetic regions).
    let mut nebula_of: BTreeMap<usize, String> = BTreeMap::new();
    let nebula_count = (n / 120).clamp(1, NEBULAE.len());
    for k in 0..nebula_count {
        let c = rng.range(fed as i64, n as i64 - 1) as usize;
        let r2 = (radius * 0.12).powi(2);
        for i in fed..n {
            if dist2(pos[i], pos[c]) < r2 {
                nebula_of.entry(i).or_insert_with(|| NEBULAE[k].to_string());
            }
        }
    }

    // ---- sectors, ports, planets.
    let mut sectors: Vec<Sector> = (0..n)
        .map(|i| Sector {
            id: i as u32 + 1,
            x: round1(pos[i].0),
            y: round1(pos[i].1),
            // Height is added on top of the flat layout: lanes and every RNG draw are unchanged.
            z: depth(cfg.seed, i as u32 + 1, round1(pos[i].0), round1(pos[i].1), i < fed),
            warps: adj[i].iter().map(|&j| j as u32 + 1).collect(),
            fedspace: i < fed,
            nebula: nebula_of.get(&i).cloned(),
            port: None,
            planets: vec![],
            fighters: None,
            armid: None,
            limpet: None,
            beacon: None,
        })
        .collect();

    let mut used_names = BTreeSet::new();
    let mut unique_name = |rng: &mut Rng| loop {
        let nm = make_name(rng);
        if used_names.insert(nm.clone()) {
            return nm;
        }
    };

    // Stardock in sector 1: services only, plus a small Sol market.
    {
        let mut p = make_port(&mut rng, &rules, 7, "Stardock".into());
        p.stardock = true;
        p.class = 0;
        for i in 0..3 {
            p.capacity[i] = 3_000;
            p.stock[i] = 3_000;
            p.strength[i] = 20;
        }
        sectors[0].port = Some(p);
    }
    // FedSpace: a shuffled rotation of classes 1-6 so every commodity is both
    // bought and sold in safe space, at half capacity (margins deplete fast,
    // nudging pilots outward).
    let mut fed_classes: Vec<u8> = (0..fed.saturating_sub(1)).map(|i| 1 + (i % 6) as u8).collect();
    rng.shuffle(&mut fed_classes);
    for i in 1..fed {
        if i % 3 == 0 {
            continue; // leave some FedSpace sectors empty
        }
        let class = fed_classes[i - 1];
        let name = format!("{} {}", unique_name(&mut rng), rng.pick(&PORT_SUFFIX).unwrap());
        let mut port = make_port(&mut rng, &rules, class, name);
        for k in 0..3 {
            port.capacity[k] /= 2;
            port.stock[k] /= 2;
        }
        sectors[i].port = Some(port);
    }
    for i in fed..n {
        if rng.range(0, 9_999) < rules.port_density_bp {
            // Classes 1-3 are built about twice as often as 4-6; 7 and 8 are rare.
            let roll = rng.range(0, 99);
            let class = match roll {
                0..=59 => 1 + (roll % 3) as u8,
                60..=89 => 4 + (roll % 3) as u8,
                90..=94 => 7,
                _ => 8,
            };
            let name = format!("{} {}", unique_name(&mut rng), rng.pick(&PORT_SUFFIX).unwrap());
            sectors[i].port = Some(make_port(&mut rng, &rules, class, name));
        }
    }

    let mut planets = BTreeMap::new();
    let mut next_planet = 1;
    let buffer: BTreeSet<usize> = (0..fed).flat_map(|f| adj[f].iter().copied()).filter(|t| *t >= fed).collect();
    for i in fed..n {
        if buffer.contains(&i) || (0..fed).any(|f| adj[i].contains(&f)) {
            continue; // Federation buffer zone
        }
        // Dead ends are twice as likely to hide a planet.
        let mut p = rules.planet_density_bp;
        if adj[i].len() <= 1 {
            p *= 2;
        }
        if rng.range(0, 9_999) < p {
            let class = roll_planet_class(&mut rng, &rules);
            let id = next_planet;
            next_planet += 1;
            planets.insert(id, new_planet(id, unique_name(&mut rng), i as u32 + 1, class, 0));
            sectors[i].planets.push(id);
        }
    }

    let mut world = World {
        announcements: vec![],
        next_announcement: 0,
        galaxy_events: vec![],
        next_galaxy_event: 0,
        feedback: vec![],
        next_feedback: 0,
        constitution: Default::default(),
        archive: Default::default(),
        seed: cfg.seed,
        seq: 0,
        tick: 0,
        now_ms: 0,
        rules: rules.clone(),
        sectors,
        planets,
        players: BTreeMap::new(),
        corps: BTreeMap::new(),
        radio: vec![],
        bounties: BTreeMap::new(),
        news: vec![NewsItem {
            tick: 0,
            headline: format!("The Federation opens the frontier: {n} sectors charted for colonisation."),
        }],
        next_player: 1,
        next_planet,
        next_corp: 1,
        next_msg: 1,
        hall_of_fame: vec![],
        season: Season {
            number: 1,
            started_tick: 0,
            ends_tick: rules.season_ticks,
            finished: false,
            final_standings: vec![],
            fed_purse_paid: 0,
            fed_tax_collected: 0,
            started_ms: 0,
        },
        market_history: vec![],
        market_reports: vec![],
        languages: BTreeMap::new(),
        identities: BTreeMap::new(),
        rename_required: Default::default(),
        chronicle: Default::default(),
        improve: Default::default(),
        treaties: BTreeMap::new(),
        wars: BTreeMap::new(),
        intents: BTreeMap::new(),
        combat_reports: vec![],
        next_registry_id: 1,
        contracts: BTreeMap::new(),
        concessions: BTreeMap::new(),
    };
    crate::frontier::place(&mut world);
    crate::npc::spawn_npcs(&mut world, &mut rng);
    world
}

/// Names for nebulae beyond the first ring of the galaxy, after `NEBULAE`.
const OUTER_NEBULAE: [&str; 10] = [
    "The Outer Reach",
    "Farlight Shoals",
    "The Hollow Verge",
    "Cinder Wake",
    "The Pale Rim",
    "Lantern Drift",
    "The Ashen Gate",
    "Sable Current",
    "The Far Choir",
    "Glass Tide",
];

/// What `furnish` gave a new frontier ring.
#[derive(Debug, Default, Clone)]
pub struct Furnished {
    pub nebulae: Vec<String>,
    /// One-way long-range lanes (from, to).
    pub wormholes: Vec<(SectorId, SectorId)>,
}

/// Give the sectors from index `n0` on (a ring just added by `expand`) what genesis gives a
/// region and `expand` does not: named nebulae and a few wormholes (one-way, long-range) that
/// tie the new ring to the old frontier, half leading out and half leading home. Adding lanes
/// never breaks strong connectivity. Deterministic given `rng`.
pub fn furnish(w: &mut World, rng: &mut Rng, n0: usize) -> Furnished {
    let n1 = w.sectors.len();
    let mut out = Furnished::default();
    if n1 <= n0 {
        return out;
    }
    let added = n1 - n0;
    let radius = w.sectors.iter().map(|s| (s.x * s.x + s.y * s.y).sqrt()).fold(1.0, f64::max);
    // Nebulae: one per ~120 new sectors, each a cloud over the new sectors near a centre.
    let used: BTreeSet<String> = w.sectors.iter().filter_map(|s| s.nebula.clone()).collect();
    let mut names = NEBULAE.iter().chain(OUTER_NEBULAE.iter()).filter(|n| !used.contains(**n));
    for _ in 0..(added / 120).max(1) {
        let Some(name) = names.next() else { break };
        let c = rng.range(n0 as i64, n1 as i64 - 1) as usize;
        let (cx, cy, r2) = (w.sectors[c].x, w.sectors[c].y, (radius * 0.12).powi(2));
        let mut any = false;
        for s in &mut w.sectors[n0..] {
            if s.nebula.is_none() && (s.x - cx).powi(2) + (s.y - cy).powi(2) < r2 {
                s.nebula = Some(name.to_string());
                any = true;
            }
        }
        if any {
            out.nebulae.push(name.to_string());
        }
    }
    // Wormholes: one per ~60 new sectors, from an open old frontier sector out to the new ring,
    // or from the new ring back to the old frontier. Both ends are open (two lanes or more): a
    // wormhole never leads out of, or into, a one-lane pocket.
    let open = |w: &World, i: usize| !w.sectors[i].fedspace && w.sectors[i].warps.len() >= 2 && w.sectors[i].warps.len() < 7;
    for k in 0..(added / 60).max(1) {
        for _ in 0..30 {
            let old = rng.range(0, n0 as i64 - 1) as usize;
            let new = rng.range(n0 as i64, n1 as i64 - 1) as usize;
            let (a, b) = if k % 2 == 0 { (old, new) } else { (new, old) };
            let (sa, sb) = (&w.sectors[a], &w.sectors[b]);
            let far = (sa.x - sb.x).powi(2) + (sa.y - sb.y).powi(2) > (radius * 0.5).powi(2);
            if !open(w, a) || !open(w, b) || !far || sa.warps.contains(&sb.id) {
                continue;
            }
            let to = sb.id;
            let warps = &mut w.sectors[a].warps;
            if let Err(p) = warps.binary_search(&to) {
                warps.insert(p, to);
            }
            out.wormholes.push((a as SectorId + 1, to));
            break;
        }
    }
    out
}

pub fn roll_planet_class(rng: &mut Rng, rules: &Rules) -> PlanetClass {
    let total: i64 = rules.planets.iter().map(|p| p.genesis_weight).sum();
    let mut r = rng.range(0, total - 1);
    for p in &rules.planets {
        if r < p.genesis_weight {
            return p.class;
        }
        r -= p.genesis_weight;
    }
    PlanetClass::M
}

pub fn new_planet(id: PlanetId, name: String, sector: SectorId, class: PlanetClass, tick: Tick) -> Planet {
    Planet {
        id,
        name,
        sector,
        class,
        owner: None,
        corp_shared: true,
        colonists: [0; 3],
        stock: [0; 3],
        fighters: 0,
        shields: 0,
        treasury: 0,
        citadel: 0,
        project: None,
        created_tick: tick,
        carry: [0; 4],
        qcannon: None,
        genesis: false,
    }
}

/// Sector count the galaxy should have for `captains` registered pilots.
/// `base` is kept as it is; only growth for captains rounds up to whole `expansion_chunk` rings.
pub fn target_size(rules: &Rules, base: u32, captains: u32) -> u32 {
    let chunk = rules.expansion_chunk.max(1);
    let for_captains = captains.saturating_mul(rules.sectors_per_captain).div_ceil(chunk) * chunk;
    for_captains.max(base).min(rules.max_sectors.max(base))
}

/// The size a new season's galaxy starts from (before growth for captains).
pub fn season_base(rules: &Rules, current: u32) -> u32 {
    if rules.galaxy_sectors > 0 {
        rules.galaxy_sectors
    } else {
        1_000.min(current)
    }
}

/// Grow the galaxy by `count` sectors: a new frontier ring beyond the current
/// rim, at the same density, linked inward with two-way lanes (so the galaxy
/// stays strongly connected), with ports, wild planets and dead-end pockets
/// generated by the same rules as genesis. Deterministic given `rng`.
pub fn expand(w: &mut World, rng: &mut Rng, count: u32) -> u32 {
    let n0 = w.sectors.len();
    let count = count.min(w.rules.max_sectors.saturating_sub(n0 as u32)) as usize;
    if count == 0 {
        return 0;
    }
    let rules = w.rules.clone();
    let r0 = w
        .sectors
        .iter()
        .map(|s| (s.x * s.x + s.y * s.y).sqrt())
        .fold(0.0f64, f64::max)
        .max(10.0);
    // Same density: area grows in proportion to sector count.
    let r1 = r0 * (((n0 + count) as f64) / n0 as f64).sqrt();
    let spacing2 = (r1 * 0.9 / ((n0 + count) as f64).sqrt()).powi(2);
    let mut pos: Vec<(f64, f64)> = w.sectors.iter().map(|s| (s.x, s.y)).collect();
    for _ in 0..count {
        let mut best = None;
        for _ in 0..30 {
            let a = rng.f64() * std::f64::consts::TAU;
            let r = (r0 * r0 + rng.f64() * (r1 * r1 - r0 * r0)).sqrt();
            let p = (a.cos() * r, a.sin() * r);
            if pos[n0..]
                .iter()
                .chain(pos[..n0].iter().rev().take(400))
                .all(|q| dist2(p, *q) >= spacing2)
            {
                best = Some(p);
                break;
            }
            best.get_or_insert(p);
        }
        pos.push(best.unwrap());
    }
    let n1 = n0 + count;
    let mut adj: Vec<BTreeSet<usize>> = w
        .sectors
        .iter()
        .map(|s| s.warps.iter().map(|t| *t as usize - 1).collect())
        .collect();
    adj.resize(n1, BTreeSet::new());
    let dead: BTreeSet<usize> = (n0..n1).filter(|_| rng.chance(0.21)).collect();
    // Candidates for links: the new ring plus the outer part of the old galaxy.
    let rim: Vec<usize> = (0..n0)
        .filter(|&i| !w.sectors[i].fedspace && (pos[i].0.powi(2) + pos[i].1.powi(2)).sqrt() > r0 * 0.75)
        .collect();
    let candidates: Vec<usize> = rim.into_iter().chain(n0..n1).collect();
    for i in n0..n1 {
        let k = if dead.contains(&i) { 1 } else { rng.range(2, 3) as usize };
        let mut near: Vec<(i64, usize)> = candidates
            .iter()
            .filter(|&&j| j != i)
            .map(|&j| ((dist2(pos[i], pos[j]) * 1000.0) as i64, j))
            .collect();
        near.sort();
        let mut added = 0;
        for &(_, j) in &near {
            if added >= k {
                break;
            }
            // Old one-lane pockets stay sealed: someone may be hiding a planet there.
            if dead.contains(&j) || (j < n0 && adj[j].len() <= 1) || (adj[j].len() >= 6 && !adj[i].is_empty()) {
                continue;
            }
            adj[i].insert(j);
            adj[j].insert(i);
            added += 1;
        }
    }
    // Any new sector not yet reachable from sector 1 gets a lane to its nearest connected neighbour.
    loop {
        let mut seen = vec![false; n1];
        let mut q = VecDeque::from([0usize]);
        seen[0] = true;
        while let Some(u) = q.pop_front() {
            for &v in &adj[u] {
                if !seen[v] {
                    seen[v] = true;
                    q.push_back(v);
                }
            }
        }
        let Some(o) = (n0..n1).find(|&i| !seen[i]) else { break };
        let target = (0..n1)
            .filter(|&j| seen[j] && !w.sectors.get(j).map(|s| s.fedspace).unwrap_or(false) && (j >= n0 || adj[j].len() >= 2))
            .min_by_key(|&j| (dist2(pos[o], pos[j]) * 1000.0) as i64)
            .unwrap_or(0);
        adj[o].insert(target);
        adj[target].insert(o);
    }
    // Existing sectors keep their records; only their lane lists change.
    for i in 0..n0 {
        w.sectors[i].warps = adj[i].iter().map(|&j| j as u32 + 1).collect();
    }
    let mut used: BTreeSet<String> = w.sectors.iter().filter_map(|s| s.port.as_ref().map(|p| p.name.clone())).collect();
    for i in n0..n1 {
        let id = i as u32 + 1;
        let port = if rng.range(0, 9_999) < rules.port_density_bp {
            let roll = rng.range(0, 99);
            let class = match roll {
                0..=59 => 1 + (roll % 3) as u8,
                60..=89 => 4 + (roll % 3) as u8,
                90..=94 => 7,
                _ => 8,
            };
            let name = loop {
                let nm = format!("{} {}", make_name(rng), rng.pick(&PORT_SUFFIX).unwrap());
                if used.insert(nm.clone()) {
                    break nm;
                }
            };
            Some(make_port(rng, &rules, class, name))
        } else {
            None
        };
        w.sectors.push(Sector {
            id,
            x: round1(pos[i].0),
            y: round1(pos[i].1),
            z: depth(w.seed, id, round1(pos[i].0), round1(pos[i].1), false),
            warps: adj[i].iter().map(|&j| j as u32 + 1).collect(),
            fedspace: false,
            nebula: None,
            port,
            planets: vec![],
            fighters: None,
            armid: None,
            limpet: None,
            beacon: None,
        });
        let chance = if adj[i].len() <= 1 {
            rules.planet_density_bp * 2
        } else {
            rules.planet_density_bp
        };
        if rng.range(0, 9_999) < chance {
            let pid = w.next_planet;
            w.next_planet += 1;
            let class = roll_planet_class(rng, &rules);
            let tick = w.tick;
            w.planets.insert(pid, new_planet(pid, make_name(rng), id, class, tick));
            w.sectors[i].planets.push(pid);
        }
    }
    count as u32
}
