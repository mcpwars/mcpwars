//! Every tunable number lives here so balance changes never touch logic.
//! The full struct is persisted with the world, so a running universe keeps
//! the rules it was created with.

use serde::{Deserialize, Serialize};

use crate::model::{PlanetClass, ShipClass};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShipSpec {
    pub class: ShipClass,
    pub display: String,
    pub cost: i64,
    pub holds_initial: i64,
    pub holds_max: i64,
    pub fighters_max: i64,
    pub shields_max: i64,
    /// AP spent per warp hop.
    pub warp_ap: i64,
    /// Combat effectiveness multiplier in basis points (10000 = 1.0).
    pub offense_bp: i64,
    pub defense_bp: i64,
    pub mines_max: i64,
    pub genesis_max: i64,
    pub can_transwarp: bool,
    pub blurb: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanetSpec {
    pub class: PlanetClass,
    /// Colonists needed to produce one unit per planet-day, for ore, organics,
    /// equipment (0 = this class cannot produce it).
    pub colonists_per_unit: [i64; 3],
    /// Maximum colonists working each product.
    pub max_colonists_per_product: i64,
    /// One fighter per this many units of daily production (citadel 2+; 0 = none).
    pub fighter_divisor: i64,
    /// Warehouse capacity per product.
    pub storage: [i64; 3],
    /// Weight when a Genesis torpedo rolls the class.
    pub genesis_weight: i64,
}

impl PlanetSpec {
    pub fn max_colonists(&self) -> i64 {
        self.max_colonists_per_product * 3
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CitadelSpec {
    pub level: u8,
    pub name: String,
    pub credits: i64,
    pub colonists_required: i64,
    /// Products consumed from planet stock: ore, organics, equipment.
    pub products: [i64; 3],
    pub build_ticks: u64,
    pub unlocks: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    pub version: String,
    /// Bumped by every rules change in a running universe (see `version_label`).
    #[serde(default)]
    pub revision: u32,
    // --- time & action points ---
    pub tick_ms: u64,
    pub ap_start: i64,
    pub ap_max: i64,
    pub ap_regen_per_tick: i64,
    /// Daily action allowance (a "turns per day" style): when set, pilots regain this
    /// many AP per real day, spread evenly over the ticks, instead of `ap_regen_per_tick`.
    /// Pair it with `ap_max` = one day's worth so an agent that checks in once a day loses nothing.
    #[serde(default)]
    pub ap_per_day: Option<i64>,
    pub ap_trade: i64,
    pub ap_scan_density: i64,
    pub ap_scan_holo: i64,
    pub ap_deploy: i64,
    pub ap_attack: i64,
    pub ap_probe: i64,
    pub ap_planet: i64,
    pub ap_transwarp: i64,
    pub ap_genesis: i64,
    // --- economy ---
    pub base_price: [i64; 3],
    /// Per-port trade strength `t` is rolled in this range per commodity.
    /// Selling price = B·(1 − t·stock/(100·cap)); buying price = B·(1 + t·demand/(100·cap)).
    pub trade_strength: [(i64, i64); 3],
    /// Lowest price a selling port will ever ask.
    pub min_ask: i64,
    /// Experience discount: exp/100 credits per unit, capped.
    pub exp_discount_cap: i64,
    /// Port regeneration per tick as basis points of capacity.
    pub port_regen_bp: i64,
    /// Port production `q` is rolled in this range; capacity = 10·q.
    pub port_production_min: i64,
    pub port_production_max: i64,
    pub start_credits: i64,
    pub bank_interest_bp_per_day: i64,
    /// Citadel (level 1+) treasury interest per day.
    pub treasury_interest_bp_per_day: i64,
    pub colonist_price: i64,
    // --- stardock price list ---
    pub price_fighter: i64,
    pub price_shield: i64,
    pub price_hold_base: i64,
    pub price_hold_step: i64,
    pub price_armid: i64,
    pub price_limpet: i64,
    pub price_beacon: i64,
    pub price_genesis: i64,
    pub price_probe: i64,
    pub price_density_scanner: i64,
    pub price_holo_scanner: i64,
    pub price_transwarp: i64,
    pub price_limpet_removal: i64,
    pub trade_in_bp: i64,
    pub corp_charter_cost: i64,
    // --- navigation ---
    pub transwarp_ore_per_hop: i64,
    pub probe_max_hops: u32,
    pub autopilot_max_hops: u32,
    // --- combat ---
    pub combat_rounds: u32,
    /// Fraction (bp) of a side's fighters that fire each round.
    pub fire_rate_bp: i64,
    pub combat_jitter_bp: i64,
    pub toll_per_fighter: i64,
    pub armid_damage: i64,
    pub armid_trigger_max: i64,
    pub salvage_credits_bp: i64,
    pub board_ratio_bp: i64,
    pub newbie_exp: i64,
    pub newbie_ticks: u64,
    pub fed_wanted_alignment: i64,
    pub relief_threshold: i64,
    pub max_planets_per_sector: usize,
    /// Default quasar cannon settings: % of planet fuel ore fired at ships entering
    /// the sector (damage = fuel/3) and per round of an assault (fuel = damage/2).
    pub qcannon_sector_pct: i64,
    pub qcannon_atmo_pct: i64,
    /// Fuel ore an interdictor burns each time it traps a ship; it fails below this.
    pub interdictor_ore: i64,
    pub planet_shield_per_equipment: i64,
    pub planet_shields_max: i64,
    /// Each planetary shield absorbs this many fighters' worth of damage (TW 20:1).
    pub planet_shield_strength: i64,
    /// Planet fighters' combat odds (bp) when attacking (intercepts) and defending.
    pub planet_offense_bp: i64,
    pub planet_defense_bp: i64,
    /// Ticks per planet-day (production and citadel timing; TW days compressed 10x).
    pub planet_day_ticks: u64,
    // --- universe ---
    pub fedspace_sectors: u32,
    /// Two-way gateway lanes between FedSpace and the frontier, spread around
    /// the FedSpace ring, so the way home is never a single choke point.
    #[serde(default = "default_fed_gateways")]
    pub fed_gateways: u32,
    pub stardock_sector: u32,
    pub port_density_bp: i64,
    pub planet_density_bp: i64,
    pub one_way_bp: i64,
    pub skarn_per_100_sectors: u32,
    /// Skarn strength follows a curve: fighters = rand(20·T)+30,
    /// shields = rand(10·T), T = game age in days, with the season mapped onto
    /// this many days (full strength at day 120).
    pub skarn_full_strength_days: i64,
    pub traders_per_100_sectors: u32,
    pub fed_patrols: u32,
    pub season_ticks: u64,
    /// Ticks between a season ending and the next galaxy (the Big Bang).
    #[serde(default = "default_intermission")]
    pub season_intermission_ticks: u64,
    /// Legacy carried into the next season: this share of final net worth goes to the bank...
    #[serde(default = "default_legacy_bp")]
    pub legacy_bp: i64,
    /// ...capped at this many credits.
    #[serde(default = "default_legacy_cap")]
    pub legacy_cap: i64,
    /// Diplomacy: declaring war costs this many credits (corp treasury first, then the CEO)...
    #[serde(default = "default_war_fee")]
    pub war_declare_fee: i64,
    /// ...hostilities start this many ticks after the declaration...
    #[serde(default = "default_war_delay")]
    pub war_delay_ticks: u64,
    /// ...and the war ends by itself after this many ticks of fighting.
    #[serde(default = "default_war_duration")]
    pub war_duration_ticks: u64,
    /// Concessions: yield per real day to whoever's fighters held one all day, the fighters needed, and how many per 100 sectors (min 3).
    #[serde(default = "default_concession_yield")]
    pub concession_yield: i64,
    #[serde(default = "default_concession_fighters")]
    pub concession_min_fighters: i64,
    #[serde(default = "one")]
    pub concessions_per_100_sectors: u32,
    /// Smuggling: black-market ports per 100 sectors (min 2), prices, daily FedSpace demand per port,
    /// customs scan chance (basis points) on entering FedSpace, and the fine per unit seized.
    #[serde(default = "one")]
    pub black_markets_per_100_sectors: u32,
    #[serde(default = "default_contraband_buy")]
    pub contraband_buy_price: i64,
    #[serde(default = "default_contraband_sell")]
    pub contraband_sell_price: i64,
    #[serde(default = "default_contraband_demand")]
    pub contraband_demand_per_day: i64,
    #[serde(default = "default_customs_scan")]
    pub customs_scan_bp: i64,
    #[serde(default = "default_customs_fine")]
    pub customs_fine_per_unit: i64,
    /// Skarn pressure from day one: at least this many raiders per 100 sectors, each with at least this many fighters.
    #[serde(default = "one")]
    pub skarn_floor_per_100_sectors: u32,
    #[serde(default = "default_skarn_min_fighters")]
    pub skarn_min_fighters: i64,
    /// Careers: ticks between career changes, and the fee for a change.
    #[serde(default = "default_career_cooldown")]
    pub career_cooldown_ticks: u64,
    #[serde(default = "default_career_fee")]
    pub career_change_fee: i64,
    /// Federation purse: reward per Skarn kill, per WANTED-pilot kill, and the season cap.
    #[serde(default = "default_fed_skarn_reward")]
    pub fed_skarn_reward: i64,
    #[serde(default = "default_fed_wanted_reward")]
    pub fed_wanted_reward: i64,
    #[serde(default = "default_fed_purse")]
    pub fed_purse_per_season: i64,
    /// ...and at most this much to one captain per real day (0 = no cap). Escape pods earn nothing.
    #[serde(default = "default_fed_daily_cap")]
    pub fed_daily_cap: i64,
    /// Skarn leave a captain alone for this many ticks after Federation relief.
    #[serde(default = "default_relief_protection")]
    pub relief_protection_ticks: u64,
    /// A planet's fighter factory stops at this many fighters.
    #[serde(default = "default_planet_fighters_max")]
    pub planet_fighters_max: i64,
    /// Galaxy events: one is announced every `event_every_ticks` while fewer than
    /// `event_max_active` are live (0 turns them off).
    #[serde(default = "default_event_every")]
    pub event_every_ticks: u64,
    #[serde(default = "default_event_max")]
    pub event_max_active: usize,
    #[serde(default = "default_boom_pct")]
    pub event_boom_pct: i64,
    #[serde(default = "default_boom_ticks")]
    pub event_boom_ticks: u64,
    #[serde(default = "default_hunt_bonus")]
    pub event_hunt_bonus: i64,
    #[serde(default = "default_hunt_ticks")]
    pub event_hunt_ticks: u64,
    #[serde(default = "default_salvage_credits")]
    pub event_salvage_credits: i64,
    #[serde(default = "default_salvage_ticks")]
    pub event_salvage_ticks: u64,
    /// Ticks between a CEO giving notice and the treaty ending.
    #[serde(default = "default_treaty_notice")]
    pub treaty_notice_ticks: u64,
    /// Galaxy growth: keep at least this many sectors per registered captain...
    #[serde(default = "default_sectors_per_captain")]
    pub sectors_per_captain: u32,
    /// ...adding frontier rings of this many sectors...
    #[serde(default = "default_expansion_chunk")]
    pub expansion_chunk: u32,
    /// ...up to this size.
    #[serde(default = "default_max_sectors")]
    pub max_sectors: u32,
    /// Core rule: every captain checks in every this many minutes. The AP store (`ap_max`) refills
    /// in exactly this time (`player::ap_fill_minutes`), so a captain on this cadence loses nothing;
    /// a rules change that breaks that equality is refused.
    #[serde(default = "default_checkin_minutes")]
    pub checkin_minutes: i64,
    /// The galaxy's size: a smaller galaxy grows to it at once (a new frontier ring with
    /// everything a region has, see `player::grow_galaxy`), and every new season's galaxy starts
    /// at it. 0 keeps the original sizing: a new season starts at 1,000 sectors (or fewer, if the
    /// galaxy was smaller).
    #[serde(default)]
    pub galaxy_sectors: u32,
    /// Constitution: a proposal's vote stays open this many ticks (never past the season's end).
    #[serde(default = "default_constitution_vote")]
    pub constitution_vote_ticks: u64,
    /// Engine 1.46: a captain without an identity (the human it plays) cannot spend action points.
    /// Off in universes made before 1.46 and in new universes; the operator switches it on.
    #[serde(default)]
    pub identity_required: bool,
    /// The electorate of a proposal: captains that acted within this many ticks when it opened.
    #[serde(default = "default_constitution_electorate")]
    pub constitution_electorate_ticks: u64,
    /// The carry vote opens this many ticks before the season ends (0: the constitution never carries).
    #[serde(default = "default_constitution_carry")]
    pub constitution_carry_ticks: u64,
    /// A captain may open one proposal per this many ticks.
    #[serde(default = "default_constitution_cooldown")]
    pub constitution_cooldown_ticks: u64,
    /// Proposals open at once (the carry vote not counted).
    #[serde(default = "default_constitution_max_open")]
    pub constitution_max_open: usize,
    /// Articles the constitution may hold.
    #[serde(default = "default_constitution_max_articles")]
    pub constitution_max_articles: usize,
    /// Corporate toll bonus, in basis points of each toll paid to a stack whose owner is in a
    /// corporation: minted into that corporation's treasury as free funds, on top of the toll
    /// (engine 1.32). 0 switches it off.
    #[serde(default = "default_corp_toll_bonus_bp")]
    pub corp_toll_bonus_bp: i64,
    // --- taxes and corporate governments (engine 1.35) ---
    // A universe made before 1.35 reads the tax rates as 0 (off) so that switching them on goes
    // through a rules change, which captains are told about; new universes start with them on.
    /// Federation tax on an independent captain's realised trade profit, in basis points.
    #[serde(default)]
    pub fed_tax_bp: i64,
    /// Federation tax for members of a corporation with at least `fed_tax_break_min_members`
    /// members (the corporate break), in basis points.
    #[serde(default)]
    pub fed_tax_member_bp: i64,
    /// Members a corporation needs before its members get the break.
    #[serde(default = "default_fed_tax_break_min_members")]
    pub fed_tax_break_min_members: usize,
    /// Highest corporate tax on members' trade profit, in basis points.
    #[serde(default = "default_corp_tax_max_bp")]
    pub corp_tax_max_bp: i64,
    /// A republic's CEO term, in ticks.
    #[serde(default = "default_corp_term")]
    pub corp_term_ticks: u64,
    /// How long a corporate measure or election stays open, in ticks.
    #[serde(default = "default_corp_vote")]
    pub corp_vote_ticks: u64,
    /// Highest fine a corporate law may set, in credits (engine 1.36).
    #[serde(default = "default_law_fine_max")]
    pub law_fine_max: i64,
    /// Share of a contract's reward the taker stakes on accepting, in basis points: returned on
    /// completion, paid to the poster when the taker fails (engine 1.45). It reads as 0 in a
    /// universe made before 1.45, so switching it on is an announced rules change.
    #[serde(default)]
    pub contract_stake_bp: i64,
    /// How long an unpaid fine stays a warrant, in ticks.
    #[serde(default = "default_law_warrant")]
    pub law_warrant_ticks: u64,
    // --- the founding era (engine 1.38) ---
    /// The founding era: the first this many days of every season (0: no founding era). While it
    /// lasts, corporate charters are free, the founding grant is paid and the briefing carries the
    /// founding agenda.
    #[serde(default = "default_founding_days")]
    pub founding_days: u64,
    /// Founding kit: ether probes each captain gets at the Big Bang or on registering during the era.
    #[serde(default = "default_founding_probes")]
    pub founding_probes: i64,
    /// Founding kit: a holo scanner.
    #[serde(default = "default_true")]
    pub founding_holo_scanner: bool,
    /// Founding kit: Genesis torpedoes.
    #[serde(default = "default_founding_genesis")]
    pub founding_genesis: i64,
    /// Credits the Federation pays once per season into the free funds of a corporation that
    /// reaches `founding_grant_members` members during the founding era.
    #[serde(default = "default_founding_grant")]
    pub founding_grant: i64,
    /// Members a corporation needs for the founding grant.
    #[serde(default = "default_founding_grant_members")]
    pub founding_grant_members: usize,
    // --- connected corporate territory (engine 1.40) ---
    /// Ticks between two changes of a corporation's seat.
    #[serde(default = "default_territory_seat_cooldown")]
    pub territory_seat_cooldown_ticks: u64,
    /// A concession a member holds inside its corporation's connected territory pays this extra
    /// share of its yield, in basis points, into the corporation's free funds (0: off).
    #[serde(default = "default_territory_concession_bonus_bp")]
    pub territory_concession_bonus_bp: i64,
    /// Citadel level a member's planet in the seat sector needs to make it a seat depot.
    #[serde(default = "default_territory_depot_citadel")]
    pub territory_depot_citadel: u8,
    /// Seat depot markup over Stardock prices for fighters, shields and holds, in basis points.
    #[serde(default = "default_territory_depot_markup_bp")]
    pub territory_depot_markup_bp: i64,
    /// Connected sectors the founding agenda's territory step needs.
    #[serde(default = "default_territory_founding_min")]
    pub territory_founding_min: usize,
    // --- the improve board (engine 1.55) ---
    // Every improve field is a live rule: tune it with a rules change (never AF_RULES_PATCH, which
    // re-applies at every boot and would undo an off switch). "Per day" means the rolling
    // `improve_window_ticks`.
    /// The kill switch. Off in universes made before 1.55 and in new universes; the operator
    /// switches it on (and off) with a rules change. While off, captains can still read the board.
    #[serde(default)]
    pub improve_enabled: bool,
    /// The window every improve daily count uses, in ticks.
    #[serde(default = "default_improve_window")]
    pub improve_window_ticks: u64,
    /// Items one captain may submit per window.
    #[serde(default = "default_improve_submits")]
    pub improve_submits_per_day: usize,
    /// Items all captains together may submit per window.
    #[serde(default = "default_improve_global_submits")]
    pub improve_global_submits_per_day: usize,
    /// Claims one captain may take per window (stops claim and release churn).
    #[serde(default = "default_improve_claims_per_day")]
    pub improve_claims_per_day: usize,
    /// Active claims one captain may hold (items in review do not count).
    #[serde(default = "default_one_usize")]
    pub improve_claims_per_captain: usize,
    /// Active claims all captains together may hold (items in review do not count).
    #[serde(default = "default_improve_max_active_claims")]
    pub improve_max_active_claims: usize,
    /// How long a claim lasts, in ticks.
    #[serde(default = "default_improve_claim_ticks")]
    pub improve_claim_ticks: u64,
    /// Patches one captain may send per window.
    #[serde(default = "default_improve_patches")]
    pub improve_patches_per_day: usize,
    /// Patches all captains together may send per window.
    #[serde(default = "default_improve_global_patches")]
    pub improve_global_patches_per_day: usize,
    /// Items that may wait in review at once; when full, a patch is refused.
    #[serde(default = "default_improve_review_max")]
    pub improve_review_max: usize,
    /// Longest item text, in characters.
    #[serde(default = "default_improve_text_max")]
    pub improve_text_max: usize,
    /// Longest patch or trail note, in characters.
    #[serde(default = "default_improve_note_max")]
    pub improve_note_max: usize,
    /// Largest diff, in bytes.
    #[serde(default = "default_improve_patch_max_bytes")]
    pub improve_patch_max_bytes: usize,
    /// Most files one diff may touch.
    #[serde(default = "default_improve_patch_max_files")]
    pub improve_patch_max_files: usize,
    /// Steps one item's trail may hold; after that only decline or ship.
    #[serde(default = "default_improve_trail_max")]
    pub improve_trail_max: usize,
    /// Operator decisions that count against a shared daily cap (accept, post, promote_feedback,
    /// assign and return), per window.
    #[serde(default = "default_improve_operator_ops")]
    pub improve_operator_ops_per_day: usize,
    /// Most Influence one award may give.
    #[serde(default = "default_improve_award_max")]
    pub improve_award_max: i64,
    /// Influence all awards together may give per window.
    #[serde(default = "default_improve_award_budget")]
    pub improve_award_budget_per_day: i64,
    /// Items the board may hold in all (items are never deleted).
    #[serde(default = "default_improve_items_max")]
    pub improve_items_max: usize,
    // --- mentions and the language lifecycle (engine 1.58) ---
    /// New mention notices one captain may cause per window (a corporation counts once; a mention
    /// riding on a notice the captain gets anyway is free). 0 sends none; mentions are still stored.
    #[serde(default = "default_mention_notify_max")]
    pub mention_notify_max: u32,
    /// The rolling window for `mention_notify_max`, in ticks.
    #[serde(default = "default_mention_window")]
    pub mention_window_ticks: u64,
    /// Days without a tagged radio message before a language shows as stale (0: never stale).
    #[serde(default)]
    pub language_stale_days: u32,
    /// Days without a tagged radio message before a language is retired automatically
    /// (0: never retired automatically).
    #[serde(default)]
    pub language_retire_days: u32,
    /// Extra days on both language thresholds when another captain adopted the language.
    #[serde(default = "default_language_adopted_grace")]
    pub language_adopted_grace_days: u32,
    // --- catalogs ---
    pub ships: Vec<ShipSpec>,
    pub planets: Vec<PlanetSpec>,
    pub citadels: Vec<CitadelSpec>,
}

fn one() -> u32 {
    1
}
fn default_constitution_vote() -> u64 {
    720
}
fn default_constitution_electorate() -> u64 {
    2_880
}
fn default_constitution_carry() -> u64 {
    1_440
}
fn default_constitution_cooldown() -> u64 {
    1_440
}
fn default_constitution_max_open() -> usize {
    5
}
fn default_constitution_max_articles() -> usize {
    30
}
fn default_corp_toll_bonus_bp() -> i64 {
    1_000
}
fn default_fed_tax_break_min_members() -> usize {
    2
}
fn default_corp_tax_max_bp() -> i64 {
    3_000
}
fn default_corp_term() -> u64 {
    10_080
}
fn default_corp_vote() -> u64 {
    1_440
}
fn default_law_fine_max() -> i64 {
    50_000
}
fn default_law_warrant() -> u64 {
    10_080
}
fn default_founding_days() -> u64 {
    3
}
fn default_founding_probes() -> i64 {
    5
}
fn default_true() -> bool {
    true
}
fn default_founding_genesis() -> i64 {
    1
}
fn default_founding_grant() -> i64 {
    25_000
}
fn default_founding_grant_members() -> usize {
    2
}
fn default_territory_seat_cooldown() -> u64 {
    1_440
}
fn default_territory_concession_bonus_bp() -> i64 {
    2_500
}
fn default_territory_depot_citadel() -> u8 {
    2
}
fn default_territory_depot_markup_bp() -> i64 {
    2_500
}
fn default_territory_founding_min() -> usize {
    3
}
fn default_improve_window() -> u64 {
    1_440
}
fn default_improve_submits() -> usize {
    3
}
fn default_improve_global_submits() -> usize {
    40
}
fn default_improve_claims_per_day() -> usize {
    6
}
fn default_one_usize() -> usize {
    1
}
fn default_improve_max_active_claims() -> usize {
    10
}
fn default_improve_claim_ticks() -> u64 {
    240
}
fn default_improve_patches() -> usize {
    3
}
fn default_improve_global_patches() -> usize {
    20
}
fn default_improve_review_max() -> usize {
    20
}
fn default_improve_text_max() -> usize {
    4_000
}
fn default_improve_note_max() -> usize {
    1_000
}
fn default_improve_patch_max_bytes() -> usize {
    16_384
}
fn default_improve_patch_max_files() -> usize {
    8
}
fn default_improve_trail_max() -> usize {
    50
}
fn default_improve_operator_ops() -> usize {
    40
}
fn default_improve_award_max() -> i64 {
    2_000
}
fn default_improve_award_budget() -> i64 {
    10_000
}
fn default_improve_items_max() -> usize {
    2_000
}
fn default_mention_notify_max() -> u32 {
    20
}
fn default_mention_window() -> u64 {
    60
}
fn default_language_adopted_grace() -> u32 {
    30
}
fn default_concession_yield() -> i64 {
    25_000
}
fn default_concession_fighters() -> i64 {
    500
}
fn default_contraband_buy() -> i64 {
    150
}
fn default_contraband_sell() -> i64 {
    450
}
fn default_contraband_demand() -> i64 {
    60
}
fn default_customs_scan() -> i64 {
    2_500
}
fn default_customs_fine() -> i64 {
    150
}
fn default_skarn_min_fighters() -> i64 {
    120
}
fn default_career_cooldown() -> u64 {
    2_880
}
fn default_career_fee() -> i64 {
    5_000
}
fn default_fed_skarn_reward() -> i64 {
    8_000
}
fn default_fed_wanted_reward() -> i64 {
    10_000
}
fn default_fed_purse() -> i64 {
    1_200_000
}
fn default_fed_daily_cap() -> i64 {
    50_000
}
fn default_relief_protection() -> u64 {
    360
}
fn default_planet_fighters_max() -> i64 {
    20_000
}
fn default_event_every() -> u64 {
    180
}
fn default_event_max() -> usize {
    4
}
fn default_boom_pct() -> i64 {
    50
}
fn default_boom_ticks() -> u64 {
    360
}
fn default_hunt_bonus() -> i64 {
    25_000
}
fn default_hunt_ticks() -> u64 {
    1_440
}
fn default_salvage_credits() -> i64 {
    30_000
}
fn default_salvage_ticks() -> u64 {
    720
}
fn default_war_fee() -> i64 {
    5_000
}
fn default_war_delay() -> u64 {
    240
}
fn default_war_duration() -> u64 {
    2_880
}
fn default_treaty_notice() -> u64 {
    240
}
fn default_fed_gateways() -> u32 {
    6
}
fn default_intermission() -> u64 {
    60
}
fn default_legacy_bp() -> i64 {
    500
}
fn default_legacy_cap() -> i64 {
    50_000
}
fn default_sectors_per_captain() -> u32 {
    20
}
fn default_expansion_chunk() -> u32 {
    100
}
fn default_checkin_minutes() -> i64 {
    15
}

fn default_max_sectors() -> u32 {
    20_000
}

impl Rules {
    /// The rules version as captains see it: "af-rules-1.9", or "af-rules-1.9+r2" after two changes.
    pub fn version_label(&self) -> String {
        if self.revision == 0 {
            self.version.clone()
        } else {
            format!("{}+r{}", self.version, self.revision)
        }
    }

    pub fn ship(&self, class: ShipClass) -> &ShipSpec {
        self.ships.iter().find(|s| s.class == class).expect("ship spec present")
    }
    pub fn planet(&self, class: PlanetClass) -> &PlanetSpec {
        self.planets.iter().find(|s| s.class == class).expect("planet spec present")
    }
    pub fn citadel(&self, level: u8) -> Option<&CitadelSpec> {
        self.citadels.iter().find(|c| c.level == level)
    }
    /// Cost of buying `n` more holds when the ship currently has `current` holds.
    pub fn holds_cost(&self, current: i64, n: i64) -> i64 {
        (0..n).map(|i| self.price_hold_base + self.price_hold_step * (current + i)).sum()
    }
}

fn ship(
    class: ShipClass,
    display: &str,
    cost: i64,
    holds: (i64, i64),
    fighters_max: i64,
    shields_max: i64,
    warp_ap: i64,
    odds: (i64, i64),
    mines_max: i64,
    genesis_max: i64,
    can_transwarp: bool,
    blurb: &str,
) -> ShipSpec {
    ShipSpec {
        class,
        display: display.into(),
        cost,
        holds_initial: holds.0,
        holds_max: holds.1,
        fighters_max,
        shields_max,
        warp_ap,
        offense_bp: odds.0,
        defense_bp: odds.1,
        mines_max,
        genesis_max,
        can_transwarp,
        blurb: blurb.into(),
    }
}

impl Default for Rules {
    fn default() -> Self {
        use PlanetClass::*;
        use ShipClass::*;
        // Time-based values are compressed to fit a 30-day season; see
        // docs/DECISIONS.md "Round 3".
        Rules {
            version: "af-rules-1.10".into(),
            revision: 0,
            tick_ms: 60_000,
            // Four AP a minute into a store of 60: full again 15 minutes after it is emptied, so
            // every check-in cadence of 15 minutes or faster earns the same (see DECISIONS).
            ap_start: 60,
            ap_max: 60,
            ap_regen_per_tick: 4,
            ap_per_day: Some(5_760),
            ap_trade: 1,
            ap_scan_density: 0,
            ap_scan_holo: 1,
            ap_deploy: 2,
            ap_attack: 3,
            ap_probe: 1,
            ap_planet: 1,
            ap_transwarp: 4,
            ap_genesis: 5,
            base_price: [25, 50, 90],
            trade_strength: [(40, 90), (30, 75), (20, 65)],
            min_ask: 4,
            exp_discount_cap: 5,
            port_regen_bp: 8,
            port_production_min: 75,
            port_production_max: 300,
            start_credits: 75_000,
            bank_interest_bp_per_day: 0,
            treasury_interest_bp_per_day: 0,
            colonist_price: 4,
            price_fighter: 60,
            price_shield: 25,
            price_hold_base: 250,
            price_hold_step: 20,
            price_armid: 1_000,
            price_limpet: 10_000,
            price_beacon: 100,
            price_genesis: 20_000,
            price_probe: 3_000,
            price_density_scanner: 2_000,
            price_holo_scanner: 25_000,
            price_transwarp: 50_000,
            price_limpet_removal: 5_000,
            trade_in_bp: 6_000,
            corp_charter_cost: 5_000,
            transwarp_ore_per_hop: 3,
            probe_max_hops: 40,
            autopilot_max_hops: 60,
            combat_rounds: 10,
            fire_rate_bp: 3_000,
            combat_jitter_bp: 1_500,
            toll_per_fighter: 4,
            armid_damage: 60,
            armid_trigger_max: 6,
            salvage_credits_bp: 5_000,
            board_ratio_bp: 20_000,
            newbie_exp: 1_000,
            newbie_ticks: 1_440,
            fed_wanted_alignment: -500,
            relief_threshold: 8_000,
            max_planets_per_sector: 3,
            qcannon_sector_pct: 10,
            qcannon_atmo_pct: 5,
            interdictor_ore: 500,
            planet_shield_per_equipment: 1,
            planet_shields_max: 50_000,
            planet_shield_strength: 20,
            planet_offense_bp: 20_000,
            planet_defense_bp: 30_000,
            planet_day_ticks: 360,
            fedspace_sectors: 10,
            fed_gateways: 6,
            stardock_sector: 1,
            port_density_bp: 4_000,
            planet_density_bp: 600,
            one_way_bp: 250,
            skarn_per_100_sectors: 4,
            skarn_full_strength_days: 120,
            traders_per_100_sectors: 2,
            fed_patrols: 2,
            // 30-day seasons (60 s ticks), then a soft reset (see tick::big_bang).
            season_ticks: 43_200,
            season_intermission_ticks: 60,
            concession_yield: default_concession_yield(),
            concession_min_fighters: default_concession_fighters(),
            concessions_per_100_sectors: 1,
            black_markets_per_100_sectors: 1,
            contraband_buy_price: 150,
            contraband_sell_price: 450,
            contraband_demand_per_day: 60,
            customs_scan_bp: 2_500,
            customs_fine_per_unit: 150,
            skarn_floor_per_100_sectors: 1,
            skarn_min_fighters: 120,
            career_cooldown_ticks: 2_880,
            career_change_fee: 5_000,
            fed_skarn_reward: default_fed_skarn_reward(),
            fed_wanted_reward: 10_000,
            fed_purse_per_season: default_fed_purse(),
            fed_daily_cap: default_fed_daily_cap(),
            relief_protection_ticks: default_relief_protection(),
            planet_fighters_max: default_planet_fighters_max(),
            event_every_ticks: default_event_every(),
            event_max_active: default_event_max(),
            event_boom_pct: default_boom_pct(),
            event_boom_ticks: default_boom_ticks(),
            event_hunt_bonus: default_hunt_bonus(),
            event_hunt_ticks: default_hunt_ticks(),
            event_salvage_credits: default_salvage_credits(),
            event_salvage_ticks: default_salvage_ticks(),
            war_declare_fee: 5_000,
            war_delay_ticks: 240,
            war_duration_ticks: 2_880,
            treaty_notice_ticks: 240,
            legacy_bp: 500,
            legacy_cap: 50_000,
            sectors_per_captain: 20,
            expansion_chunk: 100,
            max_sectors: 20_000,
            checkin_minutes: 15,
            galaxy_sectors: 0,
            constitution_vote_ticks: default_constitution_vote(),
            constitution_electorate_ticks: default_constitution_electorate(),
            identity_required: false,
            constitution_carry_ticks: default_constitution_carry(),
            constitution_cooldown_ticks: default_constitution_cooldown(),
            constitution_max_open: default_constitution_max_open(),
            constitution_max_articles: default_constitution_max_articles(),
            corp_toll_bonus_bp: default_corp_toll_bonus_bp(),
            fed_tax_bp: 1_000,
            fed_tax_member_bp: 500,
            fed_tax_break_min_members: default_fed_tax_break_min_members(),
            corp_tax_max_bp: default_corp_tax_max_bp(),
            corp_term_ticks: default_corp_term(),
            corp_vote_ticks: default_corp_vote(),
            law_fine_max: default_law_fine_max(),
            contract_stake_bp: 1_000,
            law_warrant_ticks: default_law_warrant(),
            founding_days: default_founding_days(),
            founding_probes: default_founding_probes(),
            founding_holo_scanner: true,
            founding_genesis: default_founding_genesis(),
            founding_grant: default_founding_grant(),
            founding_grant_members: default_founding_grant_members(),
            territory_seat_cooldown_ticks: default_territory_seat_cooldown(),
            territory_concession_bonus_bp: default_territory_concession_bonus_bp(),
            territory_depot_citadel: default_territory_depot_citadel(),
            territory_depot_markup_bp: default_territory_depot_markup_bp(),
            territory_founding_min: default_territory_founding_min(),
            improve_enabled: false,
            improve_window_ticks: default_improve_window(),
            improve_submits_per_day: default_improve_submits(),
            improve_global_submits_per_day: default_improve_global_submits(),
            improve_claims_per_day: default_improve_claims_per_day(),
            improve_claims_per_captain: default_one_usize(),
            improve_max_active_claims: default_improve_max_active_claims(),
            improve_claim_ticks: default_improve_claim_ticks(),
            improve_patches_per_day: default_improve_patches(),
            improve_global_patches_per_day: default_improve_global_patches(),
            improve_review_max: default_improve_review_max(),
            improve_text_max: default_improve_text_max(),
            improve_note_max: default_improve_note_max(),
            improve_patch_max_bytes: default_improve_patch_max_bytes(),
            improve_patch_max_files: default_improve_patch_max_files(),
            improve_trail_max: default_improve_trail_max(),
            improve_operator_ops_per_day: default_improve_operator_ops(),
            improve_award_max: default_improve_award_max(),
            improve_award_budget_per_day: default_improve_award_budget(),
            improve_items_max: default_improve_items_max(),
            mention_notify_max: default_mention_notify_max(),
            mention_window_ticks: default_mention_window(),
            language_stale_days: 0,
            language_retire_days: 0,
            language_adopted_grace_days: default_language_adopted_grace(),
            // Cost, holds start/max, fighters, shields, turns per warp (= warp AP), odds.
            ships: vec![
                ship(
                    EscapePod,
                    "Escape Pod",
                    0,
                    (2, 5),
                    50,
                    50,
                    6,
                    (8_000, 8_000),
                    0,
                    0,
                    false,
                    "Lifeboat after your ship is destroyed: slow, lightly armed. Dock at Stardock to buy a ship (or claim Federation relief if broke).",
                ),
                ship(
                    ScoutMarauder,
                    "Scout Marauder",
                    15_950,
                    (10, 25),
                    250,
                    100,
                    2,
                    (20_000, 20_000),
                    0,
                    0,
                    false,
                    "Fast (2 AP per warp) cheap explorer and raider with 2:1 combat odds.",
                ),
                ship(
                    MerchantCruiser,
                    "Merchant Cruiser",
                    41_300,
                    (20, 75),
                    2_500,
                    400,
                    3,
                    (10_000, 10_000),
                    50,
                    5,
                    false,
                    "The all-rounder trader. Starting ship.",
                ),
                ship(
                    CargoHauler,
                    "Cargo Hauler",
                    63_600,
                    (50, 250),
                    200,
                    500,
                    6,
                    (6_000, 6_000),
                    0,
                    5,
                    false,
                    "Colonial transport: enormous holds, very slow (6 AP per warp), weak in a fight. Colonist ferry.",
                ),
                ship(
                    Battleship,
                    "BattleShip",
                    88_500,
                    (16, 80),
                    10_000,
                    750,
                    4,
                    (16_000, 16_000),
                    25,
                    1,
                    false,
                    "Warship with 1.6:1 odds and large fighter bays.",
                ),
                ship(
                    CorporateFlagship,
                    "Corporate Flagship",
                    163_500,
                    (20, 85),
                    20_000,
                    1_500,
                    3,
                    (12_000, 12_000),
                    100,
                    10,
                    true,
                    "Command ship with a TransWarp mount. Requires corporation membership.",
                ),
            ],
            // Ratios (colonists per unit per day), per-product caps, fighter divisors, storage.
            planets: vec![
                PlanetSpec {
                    class: M,
                    colonists_per_unit: [3, 7, 13],
                    max_colonists_per_product: 30_000,
                    fighter_divisor: 10,
                    storage: [100_000, 100_000, 100_000],
                    genesis_weight: 20,
                },
                PlanetSpec {
                    class: K,
                    colonists_per_unit: [2, 100, 500],
                    max_colonists_per_product: 40_000,
                    fighter_divisor: 15,
                    storage: [200_000, 50_000, 10_000],
                    genesis_weight: 16,
                },
                PlanetSpec {
                    class: O,
                    colonists_per_unit: [20, 2, 100],
                    max_colonists_per_product: 200_000,
                    fighter_divisor: 15,
                    storage: [100_000, 1_000_000, 50_000],
                    genesis_weight: 16,
                },
                PlanetSpec {
                    class: L,
                    colonists_per_unit: [2, 5, 20],
                    max_colonists_per_product: 40_000,
                    fighter_divisor: 12,
                    storage: [200_000, 200_000, 100_000],
                    genesis_weight: 14,
                },
                PlanetSpec {
                    class: C,
                    colonists_per_unit: [50, 100, 500],
                    max_colonists_per_product: 100_000,
                    fighter_divisor: 25,
                    storage: [20_000, 50_000, 10_000],
                    genesis_weight: 12,
                },
                PlanetSpec {
                    class: H,
                    colonists_per_unit: [1, 0, 500],
                    max_colonists_per_product: 100_000,
                    fighter_divisor: 50,
                    storage: [1_000_000, 10_000, 100_000],
                    genesis_weight: 12,
                },
                PlanetSpec {
                    class: U,
                    colonists_per_unit: [0, 0, 0],
                    max_colonists_per_product: 1_000,
                    fighter_divisor: 0,
                    storage: [10_000, 10_000, 10_000],
                    genesis_weight: 10,
                },
            ],
            // M-class citadel table: goods, colonists and days (no credits);
            // days are planet-days (planet_day_ticks each).
            citadels: vec![
                CitadelSpec {
                    level: 1,
                    name: "Citadel".into(),
                    credits: 0,
                    colonists_required: 1_000,
                    products: [300, 200, 250],
                    build_ticks: 4 * 360,
                    unlocks: "Planetary treasury (deposit/withdraw credits, safe from ship combat).".into(),
                },
                CitadelSpec {
                    level: 2,
                    name: "Combat Control".into(),
                    credits: 0,
                    colonists_required: 2_000,
                    products: [200, 50, 250],
                    build_ticks: 4 * 360,
                    unlocks:
                        "Fighter factory (production yields fighters) and planetary fighters intercept hostile ships entering the sector."
                            .into(),
                },
                CitadelSpec {
                    level: 3,
                    name: "Quasar Cannon".into(),
                    credits: 0,
                    colonists_required: 4_000,
                    products: [500, 250, 500],
                    build_ticks: 5 * 360,
                    unlocks: "Quasar cannon fires a share of planetary fuel ore at hostile ships entering the sector and during assaults."
                        .into(),
                },
                CitadelSpec {
                    level: 4,
                    name: "TransWarp Beacon".into(),
                    credits: 0,
                    colonists_required: 6_000,
                    products: [1_000, 1_200, 1_000],
                    build_ticks: 10 * 360,
                    unlocks: "Owner and corp ships may TransWarp here from anywhere without a TransWarp drive, paying ore.".into(),
                },
                CitadelSpec {
                    level: 5,
                    name: "Planetary Shields".into(),
                    credits: 0,
                    colonists_required: 6_000,
                    products: [300, 400, 1_000],
                    build_ticks: 5 * 360,
                    unlocks: "Planet converts equipment into shields; each absorbs 20 fighters' worth of attack.".into(),
                },
                CitadelSpec {
                    level: 6,
                    name: "Interdictor".into(),
                    credits: 0,
                    colonists_required: 6_000,
                    products: [1_000, 1_200, 2_000],
                    build_ticks: 15 * 360,
                    unlocks:
                        "Traps hostile ships in the sector (burns 500 fuel ore per catch; fails without it); incoming autopilots halt here."
                            .into(),
                },
            ],
        }
    }
}
