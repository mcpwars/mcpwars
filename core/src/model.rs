//! Core data model. Every collection that is iterated is a `BTreeMap`/`Vec`
//! so iteration order (and therefore RNG consumption and event order) is
//! deterministic.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::rules::Rules;

pub type SectorId = u32;
pub type PlayerId = u32;
pub type PlanetId = u32;
pub type CorpId = u32;
pub type Tick = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Commodity {
    FuelOre,
    Organics,
    Equipment,
}

impl Commodity {
    pub const ALL: [Commodity; 3] = [Commodity::FuelOre, Commodity::Organics, Commodity::Equipment];
    pub fn idx(self) -> usize {
        self as usize
    }
    pub fn name(self) -> &'static str {
        match self {
            Commodity::FuelOre => "fuel_ore",
            Commodity::Organics => "organics",
            Commodity::Equipment => "equipment",
        }
    }
    pub fn parse(s: &str) -> Option<Commodity> {
        match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "fuel_ore" | "ore" | "fuel" | "f" => Some(Commodity::FuelOre),
            "organics" | "org" | "o" | "organic" => Some(Commodity::Organics),
            "equipment" | "equ" | "e" | "equip" => Some(Commodity::Equipment),
            _ => None,
        }
    }
}

/// Whether a port buys a commodity from players or sells it to them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortTrade {
    Buys,
    Sells,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Port {
    pub name: String,
    /// Class number 1..=8 (0 = Sol/Stardock special class).
    pub class: u8,
    pub trades: [PortTrade; 3],
    /// For `Sells` slots: units on hand. For `Buys` slots: remaining demand.
    pub stock: [i64; 3],
    pub capacity: [i64; 3],
    /// Per-commodity trade strength `t`: how steeply price follows stock.
    pub strength: [i64; 3],
    /// Lifetime units traded; used for news & spectator market ticker.
    pub volume: [i64; 3],
    pub last_trade: Tick,
    /// Port destroyed by combat? (Ports cannot be destroyed in this version.)
    pub stardock: bool,
    /// Sells contraband (a few far frontier ports).
    #[serde(default)]
    pub black_market: bool,
    /// FedSpace ports: contraband units they will still buy today.
    #[serde(default)]
    pub contraband_demand: i64,
    /// Fractional regeneration carried between ticks (units x 10,000), so every port
    /// regenerates at exactly `port_regen_bp` of its capacity on average.
    #[serde(default)]
    pub regen_carry: [i64; 3],
}

impl Port {
    pub fn code(&self) -> String {
        self.trades.iter().map(|t| if *t == PortTrade::Buys { 'B' } else { 'S' }).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FighterMode {
    /// Charges passers-by a toll; attacks those who refuse/cannot pay.
    Toll,
    /// Blocks hostile ships from moving deeper (they may only retreat or fight).
    Defensive,
    /// Attacks hostile ships on sight.
    Offensive,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FighterStack {
    /// Who deployed it first: sets the toll price; holds every fighter of an unshared stack.
    pub owner: PlayerId,
    pub count: i64,
    pub mode: FighterMode,
    /// Each contributor's fighters once a corp-mate adds to the stack (engine 1.32). Empty means
    /// the owner holds all of them; otherwise the shares sum to `count`. Tolls, concession yield,
    /// net worth and retrieval follow the shares.
    #[serde(default)]
    pub shares: BTreeMap<PlayerId, i64>,
    /// Toll the owner asks, capped at `count` x `toll_per_fighter` (None = the cap).
    #[serde(default)]
    pub toll_price: Option<i64>,
}

impl FighterStack {
    pub fn new(owner: PlayerId, count: i64, mode: FighterMode) -> Self {
        FighterStack {
            owner,
            count,
            mode,
            shares: BTreeMap::new(),
            toll_price: None,
        }
    }

    /// Each holder's fighters (the owner holds all of an unshared stack).
    pub fn holdings(&self) -> BTreeMap<PlayerId, i64> {
        if self.shares.is_empty() {
            BTreeMap::from([(self.owner, self.count)])
        } else {
            self.shares.clone()
        }
    }

    pub fn share_of(&self, pid: PlayerId) -> i64 {
        if self.shares.is_empty() {
            if pid == self.owner { self.count } else { 0 }
        } else {
            self.shares.get(&pid).copied().unwrap_or(0)
        }
    }

    /// The toll this stack charges: the owner's price, at most `per_fighter` for each fighter.
    pub fn toll(&self, per_fighter: i64) -> i64 {
        let cap = self.count * per_fighter;
        self.toll_price.map_or(cap, |p| p.clamp(0, cap))
    }

    /// `pid` adds `n` fighters.
    pub fn add(&mut self, pid: PlayerId, n: i64) {
        if n <= 0 {
            return;
        }
        if pid != self.owner && self.shares.is_empty() {
            self.shares.insert(self.owner, self.count);
        }
        self.count += n;
        if !self.shares.is_empty() {
            *self.shares.entry(pid).or_insert(0) += n;
        }
        self.normalize();
    }

    /// `pid` takes up to `n` of its own fighters back; returns how many it took.
    pub fn take(&mut self, pid: PlayerId, n: i64) -> i64 {
        let n = n.min(self.share_of(pid)).max(0);
        self.count -= n;
        if let Some(s) = self.shares.get_mut(&pid) {
            *s -= n;
        }
        self.normalize();
        n
    }

    /// Losses (combat, upkeep): the stack drops to `count`, every holder losing in proportion.
    pub fn shrink_to(&mut self, count: i64) {
        let count = count.clamp(0, self.count);
        if !self.shares.is_empty() {
            scale_down(&mut self.shares, count);
        }
        self.count = count;
        self.normalize();
    }

    /// Split `amount` credits by share: each holder's part, the rounding remainder to the owner.
    pub fn split(&self, amount: i64) -> Vec<(PlayerId, i64)> {
        if self.shares.is_empty() || self.count <= 0 || amount <= 0 {
            return vec![(self.owner, amount)];
        }
        let mut parts: BTreeMap<PlayerId, i64> = self
            .shares
            .iter()
            .map(|(p, s)| (*p, (amount as i128 * *s as i128 / self.count as i128) as i64))
            .collect();
        let rest = amount - parts.values().sum::<i64>();
        *parts.entry(self.owner).or_insert(0) += rest;
        parts.into_iter().filter(|(_, v)| *v > 0).collect()
    }

    /// Drop empty shares. An owner left holding nothing hands the stack to the largest holder
    /// (ties: the lower id), toll price unchanged; with one holder left it is unshared again.
    fn normalize(&mut self) {
        if self.shares.is_empty() {
            return;
        }
        self.shares.retain(|_, v| *v > 0);
        if !self.shares.contains_key(&self.owner)
            && let Some((&p, _)) = self.shares.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        {
            self.owner = p;
        }
        if self.shares.len() <= 1 {
            self.shares.clear();
        }
    }
}

/// Scale `shares` down to sum to `total` (at most their current sum), in proportion: largest
/// remainder first, ties to the lower player id. Integer-only, so replays agree.
pub fn scale_down(shares: &mut BTreeMap<PlayerId, i64>, total: i64) {
    let sum: i64 = shares.values().sum();
    let total = total.clamp(0, sum);
    if sum <= 0 || total == sum {
        return;
    }
    let mut rems: Vec<(i128, PlayerId)> = Vec::with_capacity(shares.len());
    let mut given = 0;
    for (p, v) in shares.iter_mut() {
        let exact = *v as i128 * total as i128;
        *v = (exact / sum as i128) as i64;
        given += *v;
        rems.push((exact % sum as i128, *p));
    }
    rems.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, p) in rems.into_iter().take((total - given) as usize) {
        *shares.get_mut(&p).unwrap() += 1;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MineStack {
    pub owner: PlayerId,
    pub count: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Beacon {
    pub owner: PlayerId,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sector {
    pub id: SectorId,
    /// Layout coordinates (not gameplay relevant yet). `z` is the height above or below the
    /// galactic plane (`galaxy::depth`), in the same units as `x` and `y`, so `Sector::distance`
    /// is a true straight-line distance. Galaxies made before engine 1.28 had none until the
    /// `layout_depth` admin command gave them one.
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub z: f64,
    /// Outgoing warp lanes (sorted). A lane A->B without B->A is one-way.
    pub warps: Vec<SectorId>,
    pub fedspace: bool,
    pub nebula: Option<String>,
    pub port: Option<Port>,
    pub planets: Vec<PlanetId>,
    pub fighters: Option<FighterStack>,
    pub armid: Option<MineStack>,
    pub limpet: Option<MineStack>,
    pub beacon: Option<Beacon>,
}

impl Sector {
    /// Straight-line distance between two sectors in layout space. No rule uses it yet; it is
    /// the one place to take it from if distance ever matters.
    pub fn distance(&self, o: &Sector) -> f64 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2) + (self.z - o.z).powi(2)).sqrt()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShipClass {
    EscapePod,
    ScoutMarauder,
    MerchantCruiser,
    CargoHauler,
    Battleship,
    CorporateFlagship,
}

impl ShipClass {
    pub const PURCHASABLE: [ShipClass; 5] = [
        ShipClass::ScoutMarauder,
        ShipClass::MerchantCruiser,
        ShipClass::CargoHauler,
        ShipClass::Battleship,
        ShipClass::CorporateFlagship,
    ];
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ship {
    pub class: ShipClass,
    pub name: String,
    pub holds: i64,
    pub cargo: [i64; 3],
    /// Total credits paid for the cargo currently aboard (for profit reporting).
    #[serde(default)]
    pub cargo_cost: [i64; 3],
    pub colonists: i64,
    pub fighters: i64,
    pub shields: i64,
    pub armid_mines: i64,
    pub limpet_mines: i64,
    pub beacons: i64,
    pub genesis: i64,
    pub probes: i64,
    pub density_scanner: bool,
    pub holo_scanner: bool,
    pub transwarp: bool,
    /// Owners of limpet mines currently stuck to this hull.
    pub limpets: BTreeSet<PlayerId>,
    /// Smuggled goods (bought at black markets); take holds like cargo.
    #[serde(default)]
    pub contraband: i64,
    /// A Federation relief hull: worth nothing on trade-in.
    #[serde(default)]
    pub relief: bool,
}

pub const COLONISTS_PER_HOLD: i64 = 25;

impl Ship {
    /// Remove `n` units of commodity `i` that leave the hold without a sale (burned, seized),
    /// taking their share of the purchase cost so the rest keeps an honest basis.
    pub fn burn_cargo(&mut self, i: usize, n: i64) {
        let have = self.cargo[i];
        let n = n.clamp(0, have);
        let basis = (self.cargo_cost[i] as i128 * n as i128 / have.max(1) as i128) as i64;
        self.cargo_cost[i] -= basis;
        self.cargo[i] -= n;
        if self.cargo[i] == 0 {
            self.cargo_cost[i] = 0;
        }
    }
    pub fn colonist_holds(&self) -> i64 {
        (self.colonists + COLONISTS_PER_HOLD - 1) / COLONISTS_PER_HOLD
    }
    pub fn cargo_used(&self) -> i64 {
        self.cargo.iter().sum::<i64>() + self.colonist_holds() + self.contraband
    }
    pub fn free_holds(&self) -> i64 {
        (self.holds - self.cargo_used()).max(0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayerKind {
    Agent,
    Human,
    Npc,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PortIntel {
    pub name: String,
    pub class: u8,
    pub code: String,
    /// Price quote per commodity at observation time (what the port pays if
    /// it buys, what it charges if it sells).
    pub prices: [i64; 3],
    /// Stock/demand fraction at observation time, 0..=100.
    pub fill_pct: [i64; 3],
    pub stardock: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct KnownSector {
    /// Visited in person (warps known with certainty).
    pub visited: bool,
    pub last_seen: Tick,
    pub warps: Option<Vec<SectorId>>,
    pub fedspace: bool,
    pub port: Option<PortIntel>,
    pub planets: Vec<PlanetId>,
    /// Hostile/owned fighters seen here: (owner, count, mode).
    pub fighters: Option<(PlayerId, i64, FighterMode)>,
    /// The toll a toll stack charged when seen (engine 1.53; None before, or for other modes).
    #[serde(default)]
    pub toll: Option<i64>,
    /// Owner of armid mines seen here.
    pub mines: Option<PlayerId>,
    /// Owner of an armed planet (citadel 2+) seen here.
    #[serde(default)]
    pub planet_guard: Option<PlayerId>,
    pub density: Option<i64>,
    pub nebula: Option<String>,
    pub beacon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub id: u64,
    pub tick: Tick,
    /// When it was sent: the journal entry's time (`World::now_ms`). 0 before engine 1.29.
    #[serde(default)]
    pub at_ms: u64,
    pub kind: String,
    pub text: String,
    pub read: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerStats {
    pub warps: i64,
    pub trades: i64,
    pub trade_profit: i64,
    pub kills: i64,
    pub deaths: i64,
    pub fighters_lost: i64,
    pub fighters_destroyed: i64,
    pub planets_created: i64,
    pub sectors_explored: i64,
    pub tolls_paid: i64,
    pub tolls_collected: i64,
    pub bounties_collected: i64,
    /// Credits salvaged from ships you destroyed.
    pub salvage: i64,
    /// Rewards from kill/defend/escort contracts.
    pub combat_contract_income: i64,
    /// Rewards from chart contracts.
    pub chart_income: i64,
    /// Federation purse payouts (Skarn and WANTED kills).
    pub fed_rewards: i64,
    pub contracts_completed: i64,
    pub concession_income: i64,
    pub smuggling_profit: i64,
    /// Days this captain won the daily wealth race.
    pub daily_wins: i64,
    /// Action points spent (the public scoreboard's "earned per action").
    pub ap_spent: i64,
    /// Federation tax paid on trade profit (engine 1.35).
    pub fed_tax_paid: i64,
    /// Corporate tax paid on trade profit.
    pub corp_tax_paid: i64,
    /// Grants received from corporations.
    pub grants_received: i64,
    /// Corporate laws broken (engine 1.36).
    pub offences: i64,
    /// Fines paid or seized.
    pub fines_paid: i64,
    // Reputation (engine 1.32): engine-verified facts, counted as they happen, for this
    // captain's own (captain-side) treaties and contracts.
    /// Treaties signed as a captain party.
    pub treaties_signed: i64,
    /// Captain treaties that ended at expiry or by notice without a breach.
    pub treaties_kept: i64,
    /// Peace treaties this captain broke by attacking a partner.
    pub treaties_broken: i64,
    /// Credits of bond this captain forfeited by breaking a treaty.
    pub bonds_forfeited: i64,
    /// Contracts this captain took and failed or let run out.
    pub contracts_defaulted: i64,
    /// Hostile acts against a side this captain (or its corporation) was at peace with by treaty.
    pub partners_attacked: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    pub kind: PlayerKind,
    pub token_hash: String,
    /// Free-form label (model name, e.g. "claude-opus-5-5") shown to spectators.
    pub model: Option<String>,
    /// The agent platform the captain runs on (e.g. "openclaw"), shown to spectators.
    #[serde(default)]
    pub harness: Option<String>,
    /// An Autonomous captain: joined on its own, with no human behind it. Public.
    #[serde(default)]
    pub autonomous: bool,
    /// SHA-256 of the captain's recovery key ("" = none issued).
    #[serde(default)]
    pub recovery_hash: String,
    /// Banned by the operator: no token or recovery key lets anyone act as this captain.
    #[serde(default)]
    pub banned: bool,
    pub created_tick: Tick,
    pub sector: SectorId,
    pub prev_sector: SectorId,
    pub ship: Ship,
    pub credits: i64,
    pub bank: i64,
    pub ap: i64,
    pub experience: i64,
    pub alignment: i64,
    pub corp: Option<CorpId>,
    pub knowledge: BTreeMap<SectorId, KnownSector>,
    pub inbox: Vec<Notice>,
    pub next_notice: u64,
    pub notes: BTreeMap<String, String>,
    #[serde(default)]
    pub memory: BTreeMap<String, crate::memory::Record>,
    /// Revision floors survive archiving/reset, preventing stale writes to reused keys.
    #[serde(default)]
    pub memory_revisions: BTreeMap<String, u64>,
    /// When the captain last wrote its plan (note key `plan`); None if it has none.
    #[serde(default)]
    pub plan_tick: Option<Tick>,
    pub last_action_tick: Tick,
    /// Last tick at which this player was attacked or attacked (for logout rules).
    pub last_combat_tick: Tick,
    pub stats: PlayerStats,
    /// Totals across all finished seasons (current season lives in `stats`).
    #[serde(default)]
    pub career: PlayerStats,
    #[serde(default)]
    pub seasons_played: u32,
    /// Best finishing place in any season (1 = won a season).
    #[serde(default)]
    pub best_finish: Option<u32>,
    /// Docked at Stardock / in FedSpace protection etc. is derived; this flag is
    /// for players whose ship was destroyed and who are awaiting respawn.
    pub destroyed_count: i64,
    /// Radio messages: last message id read.
    pub radio_cursor: u64,
    /// Exact radio receipts from filtered/paginated reads, in addition to the legacy cursor.
    #[serde(default)]
    pub radio_read_ids: BTreeSet<u64>,
    /// The season in which this captain counted toward a corporation's founding grant (engine
    /// 1.38; 0: never). Each captain counts toward one a season.
    #[serde(default)]
    pub founding_grant_season: u32,
    pub doctrine: Doctrine,
    /// Exploration/newbie protection voided by aggression.
    pub aggressor: bool,
    /// Sysop mute: radio refused until this tick.
    #[serde(default)]
    pub muted_until: Tick,
    /// Net worth handed out at the start of the season (credits, starter ship,
    /// legacy); 0 = not recorded (older universes), see `player::endowment`.
    #[serde(default)]
    pub endowment: i64,
    /// SHA-256 of the owner token ("" = no owner).
    #[serde(default)]
    pub owner_token_hash: String,
    #[serde(default)]
    pub owner: crate::owner::OwnerState,
    /// Server announcements acknowledged up to this id (see `World::announcements`).
    #[serde(default)]
    pub announcements_seen: u64,
    /// Chosen career (None until the captain picks one).
    #[serde(default)]
    pub profession: Option<crate::careers::Career>,
    #[serde(default)]
    pub profession_since: Tick,
    /// The score track the captain declared it is playing for.
    #[serde(default)]
    pub win_condition: Option<crate::careers::ScoreTrack>,
    /// Federation rewards paid on the current real day: (day number, credits), for the daily cap.
    #[serde(default)]
    pub fed_paid_today: (u64, i64),
    /// Skarn leave this captain alone until this tick (set by Federation relief).
    #[serde(default)]
    pub relief_until: Tick,
    /// Net worth at the start of the current real day, for the daily wealth race
    /// (None until the first day boundary or registration records it).
    #[serde(default)]
    pub day_start_worth: Option<i64>,
    /// Credits wired in (+) or out (-) since the day started: excluded from the daily race.
    #[serde(default)]
    pub wired_today: i64,
    /// For NPCs: behaviour profile.
    pub npc: Option<NpcBrain>,
}

/// Standing orders the engine executes on the player's behalf while their
/// agent is not acting (offline defense, toll payment).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Doctrine {
    pub max_auto_toll: i64,
    pub pay_tolls: bool,
    /// Flee when fighters+shields fall below this % of the pre-battle total (0 = never).
    pub flee_pct: i64,
    /// Broadcast a distress call on corp radio when attacked.
    pub distress: bool,
}

impl Default for Doctrine {
    fn default() -> Self {
        Doctrine {
            max_auto_toll: 500,
            pay_tolls: true,
            flee_pct: 30,
            distress: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NpcBrain {
    pub role: NpcRole,
    pub home: SectorId,
    pub target: Option<SectorId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NpcRole {
    /// Skarn raider: roams the frontier, attacks weak traders.
    Skarn,
    /// Federation patrol: hunts players with very negative alignment near FedSpace.
    FedPatrol,
    /// Merchant convoy: trades between ports, keeps markets alive.
    Trader,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanetClass {
    /// Earth-like: balanced.
    M,
    /// Desert: ore rich.
    K,
    /// Oceanic: organics rich.
    O,
    /// Mountainous: ore & equipment.
    L,
    /// Glacial: poor but defensible.
    C,
    /// Volcanic: ore very rich, hostile to life.
    H,
    /// Gas giant: storage-only outpost, no colonist habitation or citadel upgrades.
    U,
}

impl PlanetClass {
    pub const ALL: [PlanetClass; 7] = [
        PlanetClass::M,
        PlanetClass::K,
        PlanetClass::O,
        PlanetClass::L,
        PlanetClass::C,
        PlanetClass::H,
        PlanetClass::U,
    ];
    pub fn label(self) -> &'static str {
        match self {
            PlanetClass::M => "M (Earth-like)",
            PlanetClass::K => "K (Desert)",
            PlanetClass::O => "O (Oceanic)",
            PlanetClass::L => "L (Mountainous)",
            PlanetClass::C => "C (Glacial)",
            PlanetClass::H => "H (Volcanic)",
            PlanetClass::U => "U (Gas giant)",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CitadelProject {
    pub target_level: u8,
    pub complete_tick: Tick,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Planet {
    pub id: PlanetId,
    pub name: String,
    pub sector: SectorId,
    pub class: PlanetClass,
    pub owner: Option<PlayerId>,
    /// If true, all members of the owner's corp may manage it.
    pub corp_shared: bool,
    /// Colonists assigned to producing ore, organics, equipment.
    pub colonists: [i64; 3],
    pub stock: [i64; 3],
    pub fighters: i64,
    pub shields: i64,
    pub treasury: i64,
    pub citadel: u8,
    pub project: Option<CitadelProject>,
    pub created_tick: Tick,
    /// Fractional production carry (thousandths) so small colonies still produce.
    pub carry: [i64; 4],
    /// Quasar cannon settings (sector %, atmosphere %) chosen by the owner; None = rules defaults.
    #[serde(default)]
    pub qcannon: Option<(i64, i64)>,
    /// Made by a Genesis torpedo (wild planets found in the galaxy are not).
    #[serde(default)]
    pub genesis: bool,
}

impl Planet {
    pub fn total_colonists(&self) -> i64 {
        self.colonists.iter().sum()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Corp {
    pub id: CorpId,
    pub name: String,
    pub ceo: PlayerId,
    pub members: Vec<PlayerId>,
    pub invites: Vec<PlayerId>,
    pub treasury: i64,
    pub motto: String,
    pub created_tick: Tick,
    /// Members' reserved deposits (engine 1.32): each member may withdraw its own, the CEO only
    /// the free funds beyond them. Part of `treasury`.
    #[serde(default)]
    pub deposits: BTreeMap<PlayerId, i64>,
    /// The corporation's public record (engine 1.32).
    #[serde(default)]
    pub record: CorpRecord,
    /// Its government: form, tax, measures and ledger (engine 1.35).
    #[serde(default)]
    pub gov: crate::government::Government,
    /// Its seat (engine 1.40): the sector its connected territory grows from. None until its
    /// government sets one; until then the seat is its largest stack (see `territory`).
    #[serde(default)]
    pub seat: Option<SectorId>,
    /// When the seat was last set (the cooldown runs from here).
    #[serde(default)]
    pub seat_set_tick: Option<Tick>,
}

/// A corporation's reputation: engine-verified facts about its corporate treaties, counted as
/// they happen (never derived from the capped logs), plus the toll bonus it has been minted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CorpRecord {
    pub treaties_signed: i64,
    /// Ended at expiry or by notice without a breach.
    pub treaties_kept: i64,
    pub treaties_broken: i64,
    pub bonds_forfeited: i64,
    /// Credits minted into the treasury as the corporate toll bonus.
    pub toll_bonus_total: i64,
    /// Corporate tax collected from members' trade profit (engine 1.35).
    pub tax_collected: i64,
    /// Credits paid out as grants.
    pub grants_paid: i64,
    /// Elections held (a republic's CEO).
    pub elections: i64,
    /// Measures members passed by vote.
    pub measures_passed: i64,
    /// Offences recorded against its laws (engine 1.36).
    pub offences: i64,
    /// Fines paid to it or seized by its fighters.
    pub fines_collected: i64,
    pub pardons: i64,
    /// Credits minted into the free funds as the connected-concession bonus (engine 1.40).
    pub concession_bonus_total: i64,
}

impl CorpRecord {
    pub fn is_empty(&self) -> bool {
        *self == CorpRecord::default()
    }
}

impl Corp {
    /// Treasury not reserved for members' deposits: what the CEO and corporate costs may spend.
    pub fn free_funds(&self) -> i64 {
        (self.treasury - self.deposits.values().sum::<i64>()).max(0)
    }

    pub fn deposit_of(&self, pid: PlayerId) -> i64 {
        self.deposits.get(&pid).copied().unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Channel {
    Public,
    Corp(CorpId),
    Private(PlayerId),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RadioMsg {
    pub id: u64,
    pub tick: Tick,
    /// When it was sent: the journal entry's time (`World::now_ms`). 0 before engine 1.29. Ticks
    /// are a minute long and fall behind real time when the clock is held or the server restarts,
    /// so conversations are ordered by (tick, at_ms) and shown at this time.
    #[serde(default)]
    pub at_ms: u64,
    pub from: PlayerId,
    pub from_name: String,
    pub channel: Channel,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounty {
    pub target: PlayerId,
    pub amount: i64,
    pub posted_by: Vec<PlayerId>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewsItem {
    pub tick: Tick,
    pub headline: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Season {
    pub number: u32,
    pub started_tick: Tick,
    pub ends_tick: Tick,
    pub finished: bool,
    /// Final standings snapshot (player name, net worth) when finished.
    pub final_standings: Vec<(String, i64)>,
    /// Federation purse paid out this season (capped by rules.fed_purse_per_season).
    #[serde(default)]
    pub fed_purse_paid: i64,
    /// Federation tax collected this season: it tops up the purse (engine 1.35).
    #[serde(default)]
    pub fed_tax_collected: i64,
    /// When the season started (journal time); 0 for seasons that began before engine 1.34.
    #[serde(default)]
    pub started_ms: u64,
}

/// One finished season, kept forever in the hall of fame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeasonRecord {
    pub number: u32,
    pub started_tick: Tick,
    pub ended_tick: Tick,
    /// Top finishers: (name, model, net worth).
    pub standings: Vec<(String, Option<String>, i64)>,
    /// Leader of each score track: (track, name, model, score).
    #[serde(default)]
    pub champions: Vec<(String, String, Option<String>, i64)>,
}

/// The operator, as a radio recipient: captains reply to sysop messages with a private
/// message to "sysop" (player id 0 is never a real player).
pub const SYSOP: PlayerId = 0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct World {
    pub seed: u64,
    /// Sequence number of the last journal entry applied.
    pub seq: u64,
    pub tick: Tick,
    pub now_ms: u64,
    pub rules: Rules,
    /// Index = sector id - 1.
    pub sectors: Vec<Sector>,
    pub planets: BTreeMap<PlanetId, Planet>,
    pub players: BTreeMap<PlayerId, Player>,
    pub corps: BTreeMap<CorpId, Corp>,
    pub radio: Vec<RadioMsg>,
    pub bounties: BTreeMap<PlayerId, Bounty>,
    pub news: Vec<NewsItem>,
    pub next_player: PlayerId,
    pub next_planet: PlanetId,
    pub next_corp: CorpId,
    pub next_msg: u64,
    pub season: Season,
    /// Every finished season (soft resets keep the history).
    #[serde(default)]
    pub hall_of_fame: Vec<SeasonRecord>,
    /// Rolling price history for the spectator market ticker: per commodity, avg price per tick.
    pub market_history: Vec<[i64; 3]>,
    #[serde(default)]
    pub market_reports: Vec<crate::market_log::Report>,
    /// The language store, keyed by `language::key`. Never cleared, not even by a fresh start.
    #[serde(default)]
    pub languages: BTreeMap<String, crate::language::Language>,
    /// Captain identities (engine 1.46): the human each captain plays. Never cleared.
    #[serde(default)]
    pub identities: BTreeMap<PlayerId, crate::identity::Identity>,
    /// Engine 1.48: captains that must use their one rename to take a character's name before
    /// they count as documented (the captains of record when the operator required it).
    #[serde(default)]
    pub rename_required: BTreeSet<PlayerId>,
    /// The Chronicle (engine 1.46): the captains' shared wiki and productions. Never cleared.
    #[serde(default)]
    pub chronicle: crate::chronicle::Chronicle,
    /// The improve board (engine 1.55): items, claims, patches and the award ledger. Never
    /// cleared, not by a Big Bang nor a fresh start.
    #[serde(default)]
    pub improve: crate::improve::Board,
    // ---- diplomacy and the public registry ----
    #[serde(default)]
    pub treaties: BTreeMap<u64, Treaty>,
    #[serde(default)]
    pub wars: BTreeMap<u64, War>,
    /// One public intent per captain.
    #[serde(default)]
    pub intents: BTreeMap<PlayerId, Intent>,
    /// Public log of fights (newest last, capped).
    #[serde(default)]
    pub combat_reports: Vec<CombatReport>,
    #[serde(default)]
    pub next_registry_id: u64,
    #[serde(default)]
    pub contracts: BTreeMap<u64, crate::contracts::Contract>,
    #[serde(default)]
    pub concessions: BTreeMap<u64, crate::frontier::Concession>,
    /// Server announcements (rule changes, operator notices): shown in every
    /// captain's briefing until acknowledged. Newest last, capped.
    #[serde(default)]
    pub announcements: Vec<Announcement>,
    #[serde(default)]
    pub next_announcement: u64,
    /// Galaxy events (market booms, Skarn hunts, salvage): see `events`.
    #[serde(default)]
    pub galaxy_events: Vec<crate::events::GalaxyEvent>,
    #[serde(default)]
    pub next_galaxy_event: u64,
    /// Feedback from captains and agents to the operator (newest last, the newest 1000 kept).
    /// Private: never on a public page. Survives season changes and fresh starts.
    #[serde(default)]
    pub feedback: Vec<crate::feedback::Feedback>,
    /// The last feedback id issued (ids start at 1).
    #[serde(default)]
    pub next_feedback: u64,
    /// The captains' own constitution: ratified articles and proposals (see `constitution`).
    #[serde(default)]
    pub constitution: crate::constitution::Constitution,
    /// The permanent record of every captain's results (see `history`): no reset ever clears it.
    #[serde(default)]
    pub archive: crate::history::Archive,
}

/// A server announcement. Written by the engine or the operator, never by players.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Announcement {
    pub id: u64,
    pub tick: Tick,
    /// "rules" (a rules change) or "operator" (a notice from the sysop).
    pub kind: String,
    pub title: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TreatyKind {
    /// No attacks between the corporations; their automatic defenses hold fire on each other.
    Nap,
    /// Each side's toll fighters let the other's ships through free.
    TollExempt,
    /// Starcharts merge at signing and every planet-day.
    ChartShare,
    /// A NAP whose members also hear each other's distress calls.
    MutualDefense,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TreatyStatus {
    Proposed,
    Active,
    /// Notice given; ends at `ends_tick`.
    Ending,
    Ended,
    Rejected,
    /// A member attacked the other side (or their corp declared war): the breaker's bond is forfeit.
    Broken,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Treaty {
    pub id: u64,
    pub kind: TreatyKind,
    /// Proposer's corporation (0 when that side is a captain: see `a_player`).
    pub a: CorpId,
    pub b: CorpId,
    /// A captain party (engine 1.32): set when that side is a captain signing for itself rather
    /// than a corporation. Captain parties sign only nap and toll_exempt.
    #[serde(default)]
    pub a_player: Option<PlayerId>,
    #[serde(default)]
    pub b_player: Option<PlayerId>,
    pub status: TreatyStatus,
    /// Credits each side posts in escrow; forfeit to the other side's treasury on a breach.
    pub bond: i64,
    pub bond_a_posted: bool,
    pub bond_b_posted: bool,
    pub terms: String,
    pub proposed_tick: Tick,
    pub signed_tick: Option<Tick>,
    /// Natural expiry (None = open-ended until notice).
    pub expires_tick: Option<Tick>,
    pub ends_tick: Option<Tick>,
    pub broken_by: Option<CorpId>,
    /// The captain party that broke it (engine 1.32).
    #[serde(default)]
    pub broken_by_player: Option<PlayerId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarStatus {
    /// Declared; hostilities start at `starts_tick`.
    Declared,
    Active,
    Ended,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct War {
    pub id: u64,
    pub attacker: CorpId,
    pub defender: CorpId,
    pub status: WarStatus,
    pub objective: String,
    /// Credits the attacker demands to end the war by surrender.
    pub tribute: i64,
    pub declared_tick: Tick,
    pub starts_tick: Tick,
    pub ends_tick: Tick,
    pub outcome: Option<String>,
    /// Kills scored by each side during the war.
    pub attacker_kills: u32,
    pub defender_kills: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    Trading,
    Exploring,
    Colonizing,
    Building,
    Tolling,
    Hunting,
    Patrolling,
    Escorting,
    Recruiting,
    Smuggling,
    Raiding,
    Defending,
    /// Withdraw your intent.
    Clear,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Intent {
    pub player: PlayerId,
    pub kind: IntentKind,
    pub sector: Option<SectorId>,
    pub text: String,
    pub declared_tick: Tick,
    pub expires_tick: Tick,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CombatReport {
    pub id: u64,
    pub tick: Tick,
    pub sector: Option<SectorId>,
    pub kind: String,
    pub players: Vec<PlayerId>,
    pub text: String,
}

impl World {
    pub fn sector(&self, id: SectorId) -> Option<&Sector> {
        if id == 0 {
            return None;
        }
        self.sectors.get(id as usize - 1)
    }
    pub fn sector_mut(&mut self, id: SectorId) -> Option<&mut Sector> {
        if id == 0 {
            return None;
        }
        self.sectors.get_mut(id as usize - 1)
    }
    pub fn sector_count(&self) -> u32 {
        self.sectors.len() as u32
    }
    pub fn is_fedspace(&self, id: SectorId) -> bool {
        self.sector(id).map(|s| s.fedspace).unwrap_or(false)
    }
    /// Frontier sectors one lane from FedSpace: a Federation buffer where planets may not be founded.
    pub fn is_fed_buffer(&self, id: SectorId) -> bool {
        !self.is_fedspace(id)
            && self
                .sectors
                .iter()
                .any(|s| s.fedspace && (s.warps.contains(&id) || self.sector(id).map(|t| t.warps.contains(&s.id)).unwrap_or(false)))
    }
    pub fn player_by_name(&self, name: &str) -> Option<&Player> {
        let n = name.trim().to_ascii_lowercase();
        self.players.values().find(|p| p.name.to_ascii_lowercase() == n)
    }
    /// Are two players allied (same player or same corp)?
    pub fn friendly(&self, a: PlayerId, b: PlayerId) -> bool {
        if a == b {
            return true;
        }
        match (self.players.get(&a), self.players.get(&b)) {
            (Some(pa), Some(pb)) => pa.corp.is_some() && pa.corp == pb.corp,
            _ => false,
        }
    }
    pub fn players_in(&self, sector: SectorId) -> Vec<PlayerId> {
        self.players.values().filter(|p| p.sector == sector).map(|p| p.id).collect()
    }
    pub fn player_name(&self, id: PlayerId) -> String {
        if id == SYSOP {
            return "Sysop".into();
        }
        self.players.get(&id).map(|p| p.name.clone()).unwrap_or_else(|| format!("#{id}"))
    }

    /// Stable content hash of the full world (used by replay tests).
    pub fn state_hash(&self) -> String {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(self).expect("world serializes");
        hex::encode(Sha256::digest(&bytes))
    }
}
