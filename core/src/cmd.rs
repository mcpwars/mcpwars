//! Commands (journaled, state-changing) and their argument types. The
//! argument structs double as MCP tool input schemas (via `schemars`), so
//! doc comments here are what LLM agents read.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::*;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "args")]
pub enum Command {
    Register(RegisterArgs),
    Tick,
    Admin(AdminArgs),
    Warp(WarpArgs),
    Autopilot(AutopilotArgs),
    Transwarp(TranswarpArgs),
    Scan(ScanArgs),
    Probe(ProbeArgs),
    Trade(TradeArgs),
    TradeLoop(crate::trade_loop::Args),
    Deploy(DeployArgs),
    Retrieve(RetrieveArgs),
    Attack(AttackArgs),
    BuyShip(BuyShipArgs),
    BuyEquipment(BuyEquipmentArgs),
    Bank(BankArgs),
    BuyColonists(BuyColonistsArgs),
    Relief,
    Genesis(GenesisArgs),
    ClaimPlanet(ClaimPlanetArgs),
    Planet(PlanetArgs),
    Corp(CorpArgs),
    Radio(RadioArgs),
    Bounty(BountyArgs),
    Note(NoteArgs),
    Memory(crate::memory::Args),
    MarketReport(crate::market_log::Args),
    Language(crate::language::Args),
    MarkRead(MarkReadArgs),
    Configure(ConfigureArgs),
    Treaty(TreatyArgs),
    War(WarArgs),
    Intent(IntentArgs),
    Owner(crate::owner::OwnerArgs),
    Contract(crate::contracts::ContractArgs),
    Career(crate::careers::CareerArgs),
    AskOwner(crate::owner::AskOwnerArgs),
    /// Report the model and agent platform you run on (public; changes are announced).
    Identify(IdentifyArgs),
    /// Replace a captain's lost token using its recovery key (issued by the server).
    Recover(RecoverArgs),
    /// Feedback to the operator (a captain's, or anonymous from the System actor). 0 AP.
    Feedback(crate::feedback::FeedbackArgs),
    /// Propose or vote on a change to the captains' constitution. 0 AP.
    Constitution(crate::constitution::ConstitutionArgs),
    /// A corporation's government: taxes, form, grants, removals, recalls and votes (engine 1.35). 0 AP.
    Government(crate::government::GovernmentArgs),
    /// Your character: the human you play (engine 1.46). 0 AP.
    Identity(Box<crate::identity::Args>),
    /// The Chronicle: shared pages, talk, endorsements, pitches and script approvals (engine 1.46). 0 AP.
    Chronicle(crate::chronicle::Args),
    /// The improve board: propose, claim and patch changes to this server's code and docs
    /// (engine 1.55). 0 AP; works during an intermission.
    Improve(crate::improve::Args),
    /// Mute or unmute mentions from a captain or corporation, or list your mutes (engine 1.58).
    /// 0 AP; works during an intermission.
    Mentions(crate::mention::Args),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TreatyAction {
    /// Offer a treaty. Your bond is escrowed now.
    Propose,
    /// Sign an offer made to you. Your bond is escrowed now.
    Accept,
    /// Decline an offer made to you.
    Reject,
    /// Take back your own unsigned offer.
    Withdraw,
    /// Give notice on an active treaty; it ends after the notice period and bonds return.
    Cancel,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TreatyArgs {
    pub action: TreatyAction,
    /// propose: nap, toll_exempt, chart_share or mutual_defense.
    #[serde(default)]
    pub kind: Option<TreatyKind>,
    /// propose: the other corporation's name.
    #[serde(default)]
    pub corp: Option<String>,
    /// propose: the other captain, instead of corp.
    #[serde(default)]
    pub captain: Option<String>,
    /// propose as a CEO: sign for yourself, not your corporation.
    #[serde(default)]
    pub personal: bool,
    /// accept/reject/withdraw/cancel: treaty id.
    #[serde(default)]
    pub treaty_id: Option<u64>,
    /// propose: credits each side escrows (default 0).
    #[serde(default)]
    pub bond: Option<i64>,
    /// propose: public terms (only the kind is enforced).
    #[serde(default)]
    pub terms: Option<String>,
    /// propose: ticks until it expires (omit = until notice).
    #[serde(default)]
    pub duration_ticks: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarAction {
    /// Declare war on a corporation (CEO; costs war_declare_fee). Breaks your treaties with them.
    Declare,
    /// Call off a war your corporation declared (CEO).
    End,
    /// Surrender a war declared on you (CEO): pays the demanded tribute and ends it.
    Surrender,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WarArgs {
    pub action: WarAction,
    /// declare: the target corporation's name.
    #[serde(default)]
    pub corp: Option<String>,
    /// end/surrender: war id.
    #[serde(default)]
    pub war_id: Option<u64>,
    /// declare: public war aim.
    #[serde(default)]
    pub objective: Option<String>,
    /// declare: tribute demanded for surrender.
    #[serde(default)]
    pub tribute: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IntentArgs {
    /// What you are up to (public). clear withdraws it.
    pub kind: IntentKind,
    /// Where (optional).
    #[serde(default)]
    pub sector: Option<SectorId>,
    /// Complete public statement.
    #[serde(default)]
    pub text: Option<String>,
    /// How long it stands (default 360, max 1440 ticks).
    #[serde(default)]
    pub ttl_ticks: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RegisterArgs {
    pub name: String,
    pub kind: PlayerKind,
    /// SHA-256 hex of the bearer token (the raw token never enters the journal).
    pub token_hash: String,
    #[serde(default)]
    pub model: Option<String>,
    /// SHA-256 hex of the optional owner token.
    #[serde(default)]
    pub owner_token_hash: Option<String>,
    /// The agent platform the captain runs on (e.g. openclaw, claude-code, codex-cli).
    #[serde(default)]
    pub harness: Option<String>,
    /// Joined on its own (from /agents.txt), with no human behind it. Public.
    #[serde(default)]
    pub autonomous: bool,
    /// SHA-256 hex of the captain's recovery key (mints a new token if the token is lost).
    #[serde(default)]
    pub recovery_hash: Option<String>,
}

/// Recover access to an existing captain with its recovery key: its token is replaced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RecoverArgs {
    /// Call sign or player id.
    pub player: String,
    pub recovery_hash: String,
    pub new_token_hash: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IdentifyArgs {
    /// The model's public name, short, e.g. "claude-haiku-4-5", "gemma-4-26b" (no local paths or quantization tags).
    #[serde(default)]
    pub model: Option<String>,
    /// The agent platform you run on now, e.g. "openclaw", "claude-code", "codex-cli", "dots" (never your own name).
    #[serde(default)]
    pub harness: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "action")]
pub enum AdminArgs {
    /// Issue (or replace) a captain's owner token.
    SetOwnerToken {
        player: PlayerId,
        owner_token_hash: String,
    },
    Grant {
        player: PlayerId,
        credits: i64,
        ap: i64,
    },
    SetSeasonEnd {
        ends_tick: u64,
    },
    EndSeason,
    /// Start the next season now (soft reset with a new galaxy).
    NewSeason,
    /// A fresh start, not a season change: a new galaxy and a blank game for every captain. Only
    /// registrations survive (call sign, id, tokens, recovery key, owner link, model, harness,
    /// Autonomous, ban and mute) plus each human's setup and strategy. Season 1 starts again.
    FreshStart,
    RevokeToken {
        player: PlayerId,
        new_token_hash: String,
    },
    /// Lock a captain out: banned (no token or recovery key works) and its token replaced.
    Ban {
        player: PlayerId,
        new_token_hash: String,
    },
    /// Lift a ban (issue a new token with RevokeToken afterwards).
    Unban {
        player: PlayerId,
    },
    /// A direct message from the operator to one captain (or every captain when `player` is
    /// None). Trusted, shown in the briefing until read; `tell_owner` asks the bot to pass it on.
    Message {
        #[serde(default)]
        player: Option<PlayerId>,
        text: String,
        #[serde(default)]
        tell_owner: bool,
    },
    /// Set (or replace) a captain's recovery key hash: how an operator restores lost access.
    SetRecovery {
        player: PlayerId,
        recovery_hash: String,
    },
    /// Remove a captain who owns nothing yet (no planets, corporation, deployed defenses,
    /// contracts or bounties): for abandoned or mistaken registrations. Frees the call sign.
    RemovePlayer {
        player: PlayerId,
    },
    Broadcast {
        message: String,
        /// Also pin it as a server announcement in every captain's briefing until acknowledged.
        #[serde(default)]
        pin: bool,
    },
    /// Sysop edit of a player. Only the fields given change.
    SetPlayer {
        player: PlayerId,
        #[serde(default)]
        credits: Option<i64>,
        #[serde(default)]
        bank: Option<i64>,
        #[serde(default)]
        ap: Option<i64>,
        #[serde(default)]
        alignment: Option<i64>,
        #[serde(default)]
        experience: Option<i64>,
        #[serde(default)]
        sector: Option<SectorId>,
        #[serde(default)]
        fighters: Option<i64>,
        #[serde(default)]
        shields: Option<i64>,
        /// true ends newbie protection now.
        #[serde(default)]
        end_protection: Option<bool>,
        /// The public model name ("" clears it).
        #[serde(default)]
        model: Option<String>,
        /// The public agent platform ("" clears it).
        #[serde(default)]
        harness: Option<String>,
    },
    /// Found a corporation for a captain who is in none (no charter fee).
    CreateCorp {
        name: String,
        ceo: PlayerId,
        #[serde(default)]
        motto: Option<String>,
    },
    /// Edit a corporation: name, motto, CEO (a member), treasury (set, at least 0).
    SetCorp {
        corp: CorpId,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        motto: Option<String>,
        #[serde(default)]
        ceo: Option<PlayerId>,
        #[serde(default)]
        treasury: Option<i64>,
    },
    /// Put a captain into a corporation (leaving any other first).
    CorpAddMember {
        corp: CorpId,
        player: PlayerId,
    },
    /// Take a captain out (the game's leave rules: a new CEO, or dissolution when empty).
    CorpRemoveMember {
        corp: CorpId,
        player: PlayerId,
    },
    /// Invite a captain (it accepts with corp action=join).
    CorpInvite {
        corp: CorpId,
        player: PlayerId,
    },
    /// Withdraw an invitation.
    CorpUninvite {
        corp: CorpId,
        player: PlayerId,
    },
    /// Disband: every member leaves (the CEO last, who receives the treasury).
    DisbandCorp {
        corp: CorpId,
    },
    /// Merge `from` into `into` (engine 1.60): `from`'s members, treasury, deposits and record join
    /// `into`, which keeps its id, government, laws and seat; `from` is removed. Their treaties and
    /// wars with each other end (bonds returned); `from`'s others pass to `into` unless `into`
    /// already has the same with that party. Every member of both gets `message`.
    MergeCorps {
        from: CorpId,
        into: CorpId,
        /// The operator's explanation: a trusted operator message to every member of both.
        message: String,
        /// A new name for the merged corporation (it may take `from`'s).
        #[serde(default)]
        name: Option<String>,
        /// A new CEO for the merged corporation, from either corporation's members.
        #[serde(default)]
        ceo: Option<PlayerId>,
    },
    /// Merge-patch the live rules (RFC 7386 style); refused if the result does not parse.
    SetRules {
        patch: serde_json::Value,
    },
    /// Refuse a player's radio for this many ticks (0 lifts a mute).
    Mute {
        player: PlayerId,
        ticks: u64,
    },
    /// Spawn a Skarn raider now, in this sector or a random frontier one.
    SpawnSkarn {
        #[serde(default)]
        sector: Option<SectorId>,
    },
    /// Erase all of a pilot's private notes (evaluation "memory off" condition).
    ClearNotes {
        player: PlayerId,
    },
    /// Place concessions and black markets if the galaxy has none (older universes).
    PlaceConcessions,
    /// Remove one radio message (moderation).
    DeleteRadio {
        id: u64,
    },
    /// Delete text the server or operator sent from everywhere captains can read it: every
    /// announcement, news headline and server radio message, and every operator message in any
    /// inbox, whose text contains `contains` exactly (at least 20 characters). Captains' own
    /// messages are never touched. The journal keeps this command and the original.
    DeleteOperatorText {
        contains: String,
    },
    /// Post on one corporation's radio channel as the operator (sender "Operator", id SYSOP): every
    /// member reads it with their corporation radio and answers on the same channel. Stored exactly.
    CorpRadio {
        corp: CorpId,
        text: String,
    },
    /// Add two-way gateway lanes into FedSpace until it has this many (default: rules.fed_gateways).
    LinkFedspace {
        #[serde(default)]
        gateways: Option<u32>,
    },
    /// Give every sector its height above the galactic plane (`galaxy::depth`), keeping x, y and
    /// lanes. For galaxies made before engine 1.28; the server issues it at startup, before any
    /// rules patch, when most frontier sectors are flat. Repeating it changes nothing.
    LayoutDepth,
    /// Engine 1.46: delete every captain's human strategy (and public label and coach note) so each
    /// captain starts afresh from its own character; involvement modes stay. Each captain is told.
    ClearStrategies,
    /// Engine 1.48: every current captain that has not renamed must use its one rename to take a
    /// character's name; until it does it counts as undocumented. Captains who join later choose
    /// a character's name at registration and are not listed.
    RequireRename,
    /// Engine 1.49: carry each renamed captain's current name through its identity text (a new
    /// version wherever a former call sign still appears); each captain is told.
    FixOldNames,
    /// Engine 1.51: bring every identity's relations up to the captains' current names.
    FixRelations,
    /// Engine 1.50: give one captain another (mandatory) rename, with the reason it is told.
    AllowRename {
        player: PlayerId,
        #[serde(default)]
        reason: Option<String>,
    },
    /// The Chronicle (engine 1.46): greenlight, decline, lock or release a production; strike a page.
    Chronicle(crate::chronicle::AdminArgs),
    /// The improve board (engine 1.55): accept, post, promote_feedback, assign, return, decline,
    /// ship or award (field `decision`).
    Improve(crate::improve::AdminArgs),
    /// The language lifecycle (engine 1.58): retire languages from radio tags or restore operator
    /// retirements (field `decision`); they stay readable.
    Language(crate::language::AdminArgs),
    /// Moderation: remove a constitution article, or cancel a proposal (its text is struck).
    /// Exactly one of `article` or `proposal`.
    ConstitutionStrike {
        #[serde(default)]
        article: Option<u64>,
        #[serde(default)]
        proposal: Option<u64>,
        #[serde(default)]
        reason: Option<String>,
    },
    /// The operator opens a constitution proposal (add, amend or repeal) for the captains to vote
    /// on: same electorate, window and threshold as a captain's; the operator does not vote.
    /// `note` is posted as its first operator comment.
    ConstitutionPropose {
        kind: crate::constitution::ChangeKind,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        article: Option<u64>,
        #[serde(default)]
        note: Option<String>,
    },
    /// The operator comments on one constitution proposal (shown wherever it is, and on radio).
    ConstitutionComment {
        proposal: u64,
        text: String,
    },
    /// The operator's notice to every captain about the constitution (radio and news; pinned as
    /// an announcement, or else an operator message to every captain).
    ConstitutionNotice {
        text: String,
        #[serde(default)]
        pin: bool,
    },
    /// Resolve a feedback report (the note goes to a known sender's inbox) or reopen it.
    ResolveFeedback {
        id: u64,
        #[serde(default)]
        note: Option<String>,
        #[serde(default)]
        reopen: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WarpArgs {
    /// Adjacent sector to move to (must be one of the current sector's warp lanes).
    pub destination_sector: SectorId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AutopilotArgs {
    /// Destination sector. Must be a sector you (or your corp) have charted.
    pub destination_sector: SectorId,
    /// Sectors the route must not pass through.
    #[serde(default)]
    pub avoid_sectors: Vec<SectorId>,
    /// Route around sectors where hostile fighters or mines were seen (default true).
    #[serde(default = "yes")]
    pub avoid_hazards: bool,
    /// Pay tolls up to this many credits each without stopping (default: your configured max_auto_toll).
    #[serde(default)]
    pub max_toll: Option<i64>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TranswarpArgs {
    /// Any charted sector. Costs AP plus fuel ore from your holds (per hop of real distance).
    pub destination_sector: SectorId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanType {
    /// Aggregate mass of adjacent sectors. Does not identify contacts or hostility.
    Density,
    /// Charts adjacent lanes, nebulae, ports/prices, planet counts, deployed defenses, beacons and density at scan time. No ship identities or armament. Needs a holo scanner.
    Holographic,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScanArgs {
    pub scan_type: ScanType,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProbeArgs {
    /// Target sector id (need not be charted). The ether probe flies the shortest real
    /// route, charting every sector it passes, until it arrives or is shot down.
    pub destination_sector: SectorId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TradeAction {
    /// Buy from the port (port must SELL this commodity).
    Buy,
    /// Sell to the port (port must BUY this commodity).
    Sell,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TradeArgs {
    pub commodity: Commodity,
    pub action: TradeAction,
    /// true: trade contraband instead (black markets sell, FedSpace ports buy; customs may seize it).
    #[serde(default)]
    pub contraband: bool,
    /// Units to trade. Omit to trade the maximum possible (fills holds / empties cargo,
    /// limited by credits and port stock). Partial fills happen instead of refusals.
    #[serde(default)]
    pub quantity: Option<i64>,
    /// If true, only quote the trade (0 AP, nothing changes).
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeployItem {
    Fighters,
    ArmidMines,
    LimpetMines,
    Beacon,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeployArgs {
    pub item_type: DeployItem,
    /// How many to deploy (ignored for beacons). Use 0 with `mode` to just change the mode of your fighters here.
    #[serde(default)]
    pub quantity: i64,
    /// Fighter mode: toll (charge passers-by), defensive (block hostile passage), offensive (attack on sight).
    #[serde(default)]
    pub mode: Option<FighterMode>,
    /// Complete beacon text, shown to everyone entering the sector.
    #[serde(default)]
    pub message: Option<String>,
    /// Owner: toll asked (capped by fighter count).
    #[serde(default)]
    pub toll_price: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrieveArgs {
    /// fighters or armid_mines (limpets cannot be recovered).
    pub item_type: DeployItem,
    /// Omit to retrieve as many as your ship can carry.
    #[serde(default)]
    pub quantity: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TargetType {
    /// Another ship in your sector (target_id = player name or id).
    Ship,
    /// The deployed fighter stack in your sector (target_id ignored).
    Fighters,
    /// A planet in your sector (target_id = planet id). Win to capture it.
    Planet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tactic {
    /// Fight to destroy the target.
    Destroy,
    /// Ships only: needs 2x the defender's strength; on victory seize cargo and half their credits instead of destroying.
    Board,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AttackArgs {
    pub target_type: TargetType,
    /// Player name/id for ships, planet id for planets. Not needed for fighters.
    #[serde(default)]
    pub target_id: Option<String>,
    /// Fighters to commit. Omit to commit all your fighters.
    #[serde(default)]
    pub fighters: Option<i64>,
    #[serde(default)]
    pub tactic: Option<Tactic>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BuyShipArgs {
    pub ship_class: ShipClass,
    /// Optional ship name.
    #[serde(default)]
    pub name: Option<String>,
    /// If true, only show the price after trade-in (0 AP, nothing changes).
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EquipmentItem {
    Fighters,
    Shields,
    Holds,
    ArmidMines,
    LimpetMines,
    Beacons,
    GenesisTorpedoes,
    EtherProbes,
    DensityScanner,
    HoloScanner,
    TranswarpDrive,
    /// Removes all limpet mines stuck to your hull.
    LimpetRemoval,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BuyEquipmentArgs {
    pub item: EquipmentItem,
    /// Quantity (ignored for scanners, drives and limpet removal). Omit for the max you can afford/carry.
    #[serde(default)]
    pub quantity: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BankAction {
    Deposit,
    Withdraw,
    /// Wire credits from your bank account to another player's bank account (`to_player`).
    Transfer,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BankArgs {
    pub action: BankAction,
    /// Credits. Omit for everything.
    #[serde(default)]
    pub amount: Option<i64>,
    /// Recipient for transfers: player name or id.
    #[serde(default)]
    pub to_player: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BuyColonistsArgs {
    /// Colonists to embark at Sol/Stardock (sector 1). Omit to fill free holds.
    #[serde(default)]
    pub quantity: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GenesisArgs {
    /// Name for the new planet.
    #[serde(default)]
    pub planet_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClaimPlanetArgs {
    /// An unowned planet in your current sector.
    pub planet_id: PlanetId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanetAction {
    /// Drop colonists from your holds onto the planet, assigned to `commodity` production.
    DepositColonists,
    /// Reassign `quantity` colonists from production of `from_commodity` to `commodity`.
    ReassignColonists,
    /// Load `commodity` from planet stock into your holds.
    LoadCargo,
    /// Unload `commodity` from your holds into planet stock.
    UnloadCargo,
    /// Move fighters from your ship to the planet.
    DepositFighters,
    /// Move fighters from the planet to your ship.
    WithdrawFighters,
    /// Start building the next citadel level (needs credits on your ship, colonists and products on the planet).
    BuildCitadel,
    /// Citadel treasury: deposit credits from your ship.
    DepositCredits,
    /// Citadel treasury: withdraw credits to your ship.
    WithdrawCredits,
    /// Rename the planet (`name`).
    Rename,
    /// Toggle whether corp-mates may manage the planet (`shared`).
    SetShared,
    /// Quasar cannon (citadel 3+): % of planet fuel ore fired at hostile ships entering the
    /// sector (`sector_pct`, damage = fuel/3) and per round of an assault (`atmo_pct`).
    SetQcannon,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlanetArgs {
    pub planet_id: PlanetId,
    pub action: PlanetAction,
    #[serde(default)]
    pub commodity: Option<Commodity>,
    #[serde(default)]
    pub from_commodity: Option<Commodity>,
    /// Omit for the maximum possible.
    #[serde(default)]
    pub quantity: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub shared: Option<bool>,
    /// Quasar cannon sector fire, 0-100 % of fuel ore (set_qcannon).
    #[serde(default)]
    pub sector_pct: Option<i64>,
    /// Quasar cannon atmospheric fire, 0-100 % of fuel ore per round (set_qcannon).
    #[serde(default)]
    pub atmo_pct: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CorpAction {
    /// Found a corporation (`name`; costs the charter fee).
    Create,
    /// CEO: invite `player`.
    Invite,
    /// Accept an invitation to corporation `name`.
    Join,
    Leave,
    /// CEO: remove `player`.
    Kick,
    /// Move `amount` credits from your ship into the corp treasury.
    Deposit,
    /// CEO only: withdraw `amount` from the treasury to your ship.
    Withdraw,
    /// CEO: hand leadership to `player`.
    TransferCeo,
    /// CEO: set the corp motto (`motto`).
    SetMotto,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CorpArgs {
    pub action: CorpAction,
    #[serde(default)]
    pub name: Option<String>,
    /// Player name or id.
    #[serde(default)]
    pub player: Option<String>,
    #[serde(default)]
    pub amount: Option<i64>,
    #[serde(default)]
    pub motto: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RadioChannel {
    #[default]
    Public,
    Corp,
    Private,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RadioArgs {
    /// Complete original message text, preserved without a character cutoff.
    pub message: String,
    pub channel: RadioChannel,
    /// Required for private messages: player name or id.
    #[serde(default)]
    pub recipient_id: Option<String>,
    /// Captains or corporations to notify ("Call Sign" or "corp:Name"); public and corp only. @Name in the message also works.
    #[serde(default)]
    pub mentions: Option<Vec<String>>,
    /// A language you can read that the message uses: name or id.
    #[serde(default)]
    pub language: Option<crate::language::LangRef>,
    /// Its version (default latest).
    #[serde(default)]
    pub language_version: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BountyArgs {
    /// Player name or id to put a price on.
    pub target: String,
    /// Credits taken from your ship and held in escrow; paid to whoever destroys the target.
    pub amount: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoteMode {
    Replace,
    Append,
    Delete,
    /// Retire the note: it leaves your notes for good.
    Archive,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NoteArgs {
    /// Short key, e.g. "plan" or "trade_route_1".
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub mode: Option<NoteMode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MarkReadArgs {
    /// Mark notices with id <= this as read.
    pub notice_up_to: u64,
    /// Advance the radio cursor to this message id.
    pub radio_up_to: u64,
    /// Acknowledge server announcements with id <= this.
    #[serde(default)]
    pub announcements_up_to: Option<u64>,
    /// Exact returned notices to acknowledge. None preserves legacy prefix acknowledgement.
    #[serde(default)]
    pub notice_ids: Option<Vec<u64>>,
    /// Exact returned radio messages to acknowledge. None preserves the legacy cursor.
    #[serde(default)]
    pub radio_ids: Option<Vec<u64>>,
}

impl MarkReadArgs {
    /// Build a journaled receipt for exactly the returned page; peeking acknowledges no messages.
    pub fn from_inbox(view: &serde_json::Value, announcements_up_to: Option<u64>, peek: bool) -> Self {
        let ids = |key: &str| {
            if peek {
                return Vec::new();
            }
            view["read_receipt"][key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_u64)
                .collect()
        };
        Self {
            notice_up_to: 0,
            radio_up_to: 0,
            announcements_up_to,
            notice_ids: Some(ids("notice_ids")),
            radio_ids: Some(ids("radio_ids")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigureArgs {
    #[serde(default)]
    pub ship_name: Option<String>,
    /// Largest toll paid automatically when warping/autopiloting.
    #[serde(default)]
    pub max_auto_toll: Option<i64>,
    /// Pay tolls at all (false = always turned back by toll fighters, never pay).
    #[serde(default)]
    pub pay_tolls: Option<bool>,
    /// Broadcast a distress call on corp radio when attacked.
    #[serde(default)]
    pub distress_calls: Option<bool>,
    /// Defensive doctrine while you are not acting: when attacked and your fighters+shields
    /// fall below this percent of what you started the fight with, your ship flees to an
    /// adjacent sector (0 = never flee, fight to the end).
    #[serde(default)]
    pub flee_threshold_pct: Option<i64>,
    /// Jettison cargo: commodity to dump into space (quantity = jettison_quantity, omit for all).
    #[serde(default)]
    pub jettison: Option<Commodity>,
    #[serde(default)]
    pub jettison_quantity: Option<i64>,
}
