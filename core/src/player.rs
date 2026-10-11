//! Player lifecycle, helpers shared by every module, and admin commands.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::cmd::*;
use crate::engine::*;
use crate::knowledge;
use crate::model::*;
use crate::rules::Rules;

pub const RANKS: [(i64, &str); 23] = [
    (0, "Civilian"),
    (2, "Private"),
    (4, "Private 1st Class"),
    (8, "Lance Corporal"),
    (16, "Corporal"),
    (32, "Sergeant"),
    (64, "Staff Sergeant"),
    (128, "Gunnery Sergeant"),
    (256, "1st Sergeant"),
    (512, "Sergeant Major"),
    (1_024, "Warrant Officer"),
    (2_048, "Chief Warrant Officer"),
    (4_096, "Ensign"),
    (8_192, "Lieutenant J.G."),
    (16_384, "Lieutenant"),
    (32_768, "Lieutenant Commander"),
    (65_536, "Commander"),
    (131_072, "Captain"),
    (262_144, "Commodore"),
    (524_288, "Rear Admiral"),
    (1_048_576, "Vice Admiral"),
    (2_097_152, "Admiral"),
    (4_194_304, "Fleet Admiral"),
];

pub fn rank(exp: i64) -> &'static str {
    RANKS.iter().rev().find(|(e, _)| exp >= *e).map(|(_, n)| *n).unwrap_or("Civilian")
}

/// The Federation's view of a captain, never a verdict on right and wrong: the captains decide
/// what good and evil mean in this galaxy.
pub fn alignment_label(a: i64) -> &'static str {
    match a {
        i64::MIN..=-500 => "Wanted",
        -499..=-100 => "Outlaw",
        -99..=99 => "Neutral",
        100..=999 => "Lawful",
        _ => "Commissioned",
    }
}

pub fn new_ship(rules: &Rules, class: ShipClass, name: &str) -> Ship {
    let spec = rules.ship(class);
    Ship {
        class,
        name: name.to_string(),
        holds: spec.holds_initial,
        cargo: [0; 3],
        cargo_cost: [0; 3],
        colonists: 0,
        fighters: 0,
        shields: 0,
        armid_mines: 0,
        limpet_mines: 0,
        beacons: 0,
        genesis: 0,
        probes: 0,
        density_scanner: true,
        holo_scanner: false,
        transwarp: false,
        limpets: BTreeSet::new(),
        contraband: 0,
        relief: false,
    }
}

/// Strip control characters and cap length (player-authored text is untrusted).
pub fn sanitize(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().filter(|c| !c.is_control()).collect();
    let trimmed = cleaned.trim();
    trimmed.chars().take(max).collect()
}

pub fn valid_name(s: &str) -> bool {
    let n = s.trim();
    (2..=24).contains(&n.chars().count())
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == ' ' || c == '-' || c == '_' || c == '\'' || c == '.')
}

/// Resolve "name", "#12", "P12" or "12" to a player.
pub fn resolve<'a>(w: &'a World, s: &str) -> Option<&'a Player> {
    let t = s.trim();
    let digits = t.trim_start_matches(['#', 'P', 'p']);
    if let Ok(id) = digits.parse::<PlayerId>()
        && let Some(p) = w.players.get(&id)
    {
        return Some(p);
    }
    w.player_by_name(t)
}

pub fn is_newbie(w: &World, pid: PlayerId) -> bool {
    let Some(p) = w.players.get(&pid) else { return false };
    p.kind != PlayerKind::Npc && !p.aggressor && p.experience < w.rules.newbie_exp && w.tick < p.created_tick + w.rules.newbie_ticks
}

/// What the ship would fetch: hull, purchased holds and gear, all at the
/// Stardock trade-in rate (so buying never looks like free score, and the
/// starting cruiser is not mistaken for earnings).
pub fn ship_value(rules: &Rules, ship: &Ship) -> i64 {
    if ship.class == ShipClass::EscapePod {
        return 0;
    }
    let spec = rules.ship(ship.class);
    let hull = if ship.relief { 0 } else { spec.cost };
    let list = hull
        + rules.holds_cost(spec.holds_initial, (ship.holds - spec.holds_initial).max(0))
        + ship.fighters * rules.price_fighter
        + ship.shields * rules.price_shield
        + if ship.transwarp { rules.price_transwarp } else { 0 }
        + if ship.holo_scanner { rules.price_holo_scanner } else { 0 };
    list * rules.trade_in_bp / 10_000
}

/// What a captain started the season with (credits, starter ship, legacy bank),
/// so `earned = net_worth - endowment` measures play rather than the handout.
pub fn endowment(w: &World, p: &Player) -> i64 {
    if p.endowment > 0 {
        return p.endowment;
    }
    w.rules.start_credits + ship_value(&w.rules, &starter_ship(&w.rules, ""))
}

/// The ship every captain starts a season with.
pub fn starter_ship(rules: &Rules, name: &str) -> Ship {
    let mut ship = new_ship(rules, ShipClass::MerchantCruiser, name);
    ship.fighters = 30;
    ship.shields = 50;
    ship
}

/// The daily wealth race: net worth gained since the last day boundary (or registration),
/// excluding credits wired in or out.
pub fn earned_today(w: &World, pid: PlayerId) -> i64 {
    let Some(p) = w.players.get(&pid) else { return 0 };
    let nw = net_worth(w, pid);
    nw - p.day_start_worth.unwrap_or(nw) - p.wired_today
}

/// At each real-day boundary: name the captain who earned the most that day (news, `daily_wins`),
/// then start a new day for everyone.
pub fn daily_race(ctx: &mut Ctx) {
    let ids: Vec<PlayerId> = ctx
        .w
        .players
        .values()
        .filter(|p| p.kind != PlayerKind::Npc && !p.banned)
        .map(|p| p.id)
        .collect();
    let best = ids
        .iter()
        .map(|id| (earned_today(ctx.w, *id), *id))
        .filter(|(e, _)| *e > 0)
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    if let Some((earned, id)) = best
        && !ctx.w.season.finished
    {
        let pm = ctx.player_mut(id);
        pm.stats.daily_wins += 1;
        let (name, wins) = (pm.name.clone(), pm.stats.daily_wins);
        let text = format!("Daily wealth race: {name} earned the most today, {earned} credits (daily wins this season: {wins}).");
        ctx.news(text.clone());
        ctx.event("daily_champion", None, vec![id], true, text, json!({"earned": earned}));
    }
    for id in ids {
        let nw = net_worth(ctx.w, id);
        let pm = ctx.player_mut(id);
        pm.day_start_worth = Some(nw);
        pm.wired_today = 0;
    }
}

/// Score used for leaderboards and season results.
pub fn net_worth(w: &World, pid: PlayerId) -> i64 {
    let Some(p) = w.players.get(&pid) else { return 0 };
    let rules = &w.rules;
    let cargo_val: i64 = (0..3).map(|i| p.ship.cargo[i] * rules.base_price[i]).sum();
    let mut nw = p.credits + p.bank + ship_value(rules, &p.ship) + cargo_val;
    for pl in w.planets.values().filter(|pl| pl.owner == Some(pid)) {
        nw += planet_value(w, pl);
    }
    for s in &w.sectors {
        if let Some(f) = &s.fighters {
            nw += f.share_of(pid) * rules.price_fighter / 2;
        }
    }
    // Your own reserved deposit, plus an equal share of the free funds beyond the deposits.
    if let Some(c) = p.corp.and_then(|c| w.corps.get(&c)) {
        nw += c.deposit_of(pid).min(c.treasury) + c.free_funds() / c.members.len().max(1) as i64;
    }
    nw
}

/// What a planet adds to net worth (engine 1.21). Colonists grow and produce for free, so a
/// planet pays through production sold at ports (which costs AP), not through its own valuation:
/// colonists and unsold stock count nothing, citadel goods count at 10% of base like the stock they
/// were, a Genesis planet keeps half the torpedo's price (a claimed wild planet nothing), and the
/// treasury and fighters count as cash and deployed fighters do.
pub fn planet_value(w: &World, pl: &Planet) -> i64 {
    let rules = &w.rules;
    let mut v = pl.treasury + pl.fighters * rules.price_fighter / 2;
    if pl.genesis {
        v += rules.price_genesis / 2;
    }
    for lvl in 1..=pl.citadel {
        if let Some(c) = rules.citadel(lvl) {
            v += c.credits + (0..3).map(|i| c.products[i] * rules.base_price[i] / 10).sum::<i64>();
        }
    }
    v
}

pub fn register(ctx: &mut Ctx, a: &RegisterArgs) -> Res<Outcome> {
    let name = a.name.trim().to_string();
    if !valid_name(&name) {
        return Err(Refusal::new(
            "invalid_name",
            "Names are 2-24 characters: letters, digits, spaces, - _ ' .",
        ));
    }
    if let Some(word) = crate::identity::overused(&name) {
        return Err(Refusal::new(
            "name_overused",
            format!("'{word}' is one of the names models use over and over; invent a rarer name for your character."),
        )
        .hint("Your call sign is the name of the human character you will play: invent an original one."));
    }
    // Unique ignoring case, spaces and punctuation: "QuietComet" and "quiet-comet" are the same name.
    let key = name_key(&name);
    if let Some(other) = ctx.w.players.values().find(|p| name_key(&p.name) == key) {
        return Err(Refusal::new(
            "name_taken",
            format!("'{name}' is taken (too close to the existing captain '{}').", other.name),
        )
        .hint("Invent a different, original call sign; never reuse example names."));
    }
    let rules = ctx.w.rules.clone();
    let id = ctx.w.next_player;
    ctx.w.next_player += 1;
    let start = 1 + (ctx.rng.next_u64() % rules.fedspace_sectors as u64) as SectorId;
    let kit = crate::founding::founding(ctx.w).is_some();
    let p = new_player(&rules, ctx.w.tick, ctx.w.next_announcement, id, &name, a, start, kit);
    ctx.w.players.insert(id, p);
    grow_galaxy(ctx);
    // Every pilot starts with the FedSpace chart.
    for sid in 1..=rules.fedspace_sectors {
        knowledge::observe(ctx.w, id, sid, sid == start);
    }
    ctx.notify(
        id,
        "welcome",
        format!(
            "Welcome to the frontier, {name}. You command a Merchant Cruiser in sector {start} (FedSpace) with {} credits. \
         Start with get_briefing, then find_trade_routes or explore. Stardock is sector {}.{}",
            rules.start_credits,
            rules.stardock_sector,
            crate::founding::notice_text(ctx.w)
        ),
    );
    ctx.event(
        "player_joined",
        Some(start),
        vec![id],
        true,
        format!(
            "{name} joined the game{}.",
            a.model.as_ref().map(|m| format!(" ({m})")).unwrap_or_default()
        ),
        json!({"id": id}),
    );
    ctx.news(format!("New pilot {name} registered with the Federation."));
    Ok(ctx.finish(
        format!("Registered {name} as player {id}."),
        json!({"player_id": id, "name": name, "sector": start}),
    ))
}

/// A brand-new captain: what registration creates (and what a fresh start resets each captain to).
/// `kit`: the season is in its founding era, so the ship carries the founding kit.
#[allow(clippy::too_many_arguments)]
pub fn new_player(
    rules: &crate::rules::Rules,
    tick: Tick,
    next_announcement: u64,
    id: PlayerId,
    name: &str,
    a: &RegisterArgs,
    start: SectorId,
    kit: bool,
) -> Player {
    let mut ship = starter_ship(rules, &format!("{name}'s Cruiser"));
    if kit {
        crate::founding::equip(rules, &mut ship);
    }
    let endowment = rules.start_credits + ship_value(rules, &ship);
    Player {
        // A new captain starts with the current rules: no backlog of old announcements.
        announcements_seen: next_announcement,
        endowment,
        owner_token_hash: a.owner_token_hash.clone().unwrap_or_default(),
        owner: Default::default(),
        profession: None,
        profession_since: 0,
        win_condition: None,
        fed_paid_today: (0, 0),
        relief_until: 0,
        // The daily race counts a newcomer's day from registration.
        day_start_worth: Some(endowment),
        wired_today: 0,
        id,
        name: name.to_string(),
        kind: a.kind,
        token_hash: a.token_hash.clone(),
        model: a.model.as_ref().map(|m| sanitize(m, 60)).filter(|m| !m.is_empty()),
        harness: a.harness.as_ref().map(|h| sanitize(h, 40)).filter(|h| !h.is_empty()),
        autonomous: a.autonomous,
        recovery_hash: a.recovery_hash.clone().unwrap_or_default(),
        banned: false,
        created_tick: tick,
        sector: start,
        prev_sector: start,
        ship,
        credits: rules.start_credits,
        bank: 0,
        ap: rules.ap_start,
        experience: 0,
        alignment: 0,
        corp: None,
        knowledge: BTreeMap::new(),
        inbox: vec![],
        next_notice: 1,
        notes: BTreeMap::new(),
        memory: BTreeMap::new(),
        memory_revisions: BTreeMap::new(),
        plan_tick: None,
        last_action_tick: tick,
        last_combat_tick: 0,
        stats: PlayerStats::default(),
        career: PlayerStats::default(),
        seasons_played: 0,
        best_finish: None,
        doctrine: Doctrine::default(),
        aggressor: false,
        muted_until: 0,
        destroyed_count: 0,
        radio_cursor: 0,
        radio_read_ids: Default::default(),
        founding_grant_season: 0,
        npc: None,
    }
}

/// Grow the galaxy to `rules.galaxy_sectors`, and keep it roomy as pilots arrive (at least
/// `sectors_per_captain` sectors per registered captain). A growth is a whole new region, not
/// just sectors: a frontier ring beyond the rim (`galaxy::expand`: lanes, ports, wild planets,
/// dead-end pockets, heights) with named nebulae and wormholes to the old frontier
/// (`galaxy::furnish`), concessions and black markets (`frontier::place_region`) and NPC traders,
/// all at genesis density. Pilots' charts of old sectors pick up their new lanes, and every
/// captain hears about it (announcement, news, a public bulletin).
pub fn grow_galaxy(ctx: &mut Ctx) {
    let captains = ctx.w.players.values().filter(|p| p.npc.is_none()).count() as u32;
    let have = ctx.w.sector_count();
    let want = crate::galaxy::target_size(&ctx.w.rules, have.max(ctx.w.rules.galaxy_sectors), captains);
    if want <= have {
        return;
    }
    let n0 = have as usize;
    let before: Vec<Vec<SectorId>> = ctx.w.sectors.iter().map(|s| s.warps.clone()).collect();
    let mut rng = ctx.rng.clone();
    let added = crate::galaxy::expand(ctx.w, &mut rng, want - have);
    if added == 0 {
        ctx.rng = rng;
        return;
    }
    let region = crate::galaxy::furnish(ctx.w, &mut rng, n0);
    let (concessions, markets) = crate::frontier::place_region(ctx.w, n0);
    let ports: Vec<SectorId> = ctx.w.sectors[n0..].iter().filter(|s| s.port.is_some()).map(|s| s.id).collect();
    let traders = crate::npc::add_traders(ctx.w, &mut rng, &ports, added * ctx.w.rules.traders_per_100_sectors / 100);
    ctx.rng = rng;
    // Old sectors that gained lanes: pilots who charted them learn the new lanes (as link_fedspace).
    for (i, old) in before.iter().enumerate() {
        let now = ctx.w.sectors[i].warps.clone();
        if &now == old {
            continue;
        }
        let sid = i as SectorId + 1;
        for p in ctx.w.players.values_mut() {
            if let Some(k) = p.knowledge.get_mut(&sid)
                && k.warps.is_some()
            {
                k.warps = Some(now.clone());
            }
        }
    }
    let total = ctx.w.sector_count();
    let mut parts = vec![format!("{} ports", ctx.w.sectors[n0..].iter().filter(|s| s.port.is_some()).count())];
    if !concessions.is_empty() {
        parts.push(format!(
            "{} new concession{} ({})",
            concessions.len(),
            if concessions.len() == 1 { "" } else { "s" },
            concessions.join(", ")
        ));
    }
    if markets > 0 {
        parts.push(format!("{markets} black market{}", if markets == 1 { "" } else { "s" }));
    }
    if !region.wormholes.is_empty() {
        let n = region.wormholes.len();
        parts.push(format!(
            "{n} wormhole{} to and from the old frontier",
            if n == 1 { "" } else { "s" }
        ));
    }
    let names = if region.nebulae.is_empty() {
        String::new()
    } else {
        format!(" ({})", region.nebulae.join(" and "))
    };
    // News and the public bulletin carry no sector numbers (they reach the public view); the
    // announcement, which only captains read, says where the new ring is.
    let msg = format!(
        "The Federation charts {added} new frontier sectors beyond the rim{names}: {}. The galaxy is now {total} sectors.",
        parts.join(", ")
    );
    ctx.news(msg.clone());
    crate::social::system_radio(ctx, Channel::Public, format!("FEDERATION BULLETIN: {msg}"));
    ctx.announce(
        "galaxy",
        format!("The galaxy grows to {total} sectors"),
        format!(
            "{msg} The new sectors are {} to {total}; unexplored sectors count toward exploration, and the new concessions are unheld.",
            n0 + 1
        ),
    );
    ctx.event(
        "galaxy_grew",
        None,
        vec![],
        true,
        msg,
        json!({"sectors": total, "added": added, "nebulae": region.nebulae, "concessions": concessions, "black_markets": markets, "wormholes": region.wormholes.len(), "traders": traders}),
    );
}

pub fn configure(ctx: &mut Ctx, pid: PlayerId, a: &ConfigureArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?.clone();
    if let Some(t) = a.max_auto_toll
        && t < 0
    {
        return Err(Refusal::new("invalid_args", "max_auto_toll must be >= 0."));
    }
    if let Some(f) = a.flee_threshold_pct
        && !(0..=100).contains(&f)
    {
        return Err(Refusal::new("invalid_args", "flee_threshold_pct must be 0..100."));
    }
    let jettison = match a.jettison {
        Some(c) => {
            let have = p.ship.cargo[c.idx()];
            let n = a.jettison_quantity.unwrap_or(have).min(have);
            if n <= 0 {
                return Err(Refusal::new("insufficient_cargo", format!("You carry no {}.", c.name())));
            }
            Some((c, n))
        }
        None => None,
    };
    let mut changes = vec![];
    let pm = ctx.player_mut(pid);
    if let Some(n) = &a.ship_name {
        let n = sanitize(n, 30);
        if !n.is_empty() {
            pm.ship.name = n.clone();
            changes.push(format!("ship renamed to {n}"));
        }
    }
    if let Some(t) = a.max_auto_toll {
        pm.doctrine.max_auto_toll = t;
        changes.push(format!("max_auto_toll={t}"));
    }
    if let Some(f) = a.flee_threshold_pct {
        pm.doctrine.flee_pct = f;
        changes.push(format!("flee_threshold_pct={f}"));
    }
    if let Some(v) = a.pay_tolls {
        pm.doctrine.pay_tolls = v;
        changes.push(format!("pay_tolls={v}"));
    }
    if let Some(v) = a.distress_calls {
        pm.doctrine.distress = v;
        changes.push(format!("distress_calls={v}"));
    }
    if let Some((c, n)) = jettison {
        let have = pm.ship.cargo[c.idx()];
        let basis = (pm.ship.cargo_cost[c.idx()] as i128 * n as i128 / have.max(1) as i128) as i64;
        pm.ship.cargo_cost[c.idx()] -= basis;
        pm.ship.cargo[c.idx()] -= n;
        if pm.ship.cargo[c.idx()] == 0 {
            pm.ship.cargo_cost[c.idx()] = 0;
        }
        changes.push(format!("jettisoned {n} {}", c.name()));
    }
    let doctrine = pm.doctrine.clone();
    let summary = if changes.is_empty() {
        "No changes.".to_string()
    } else {
        format!("Updated: {}.", changes.join(", "))
    };
    Ok(ctx.finish(summary, json!({"doctrine": doctrine_json(&doctrine), "you": brief(ctx.w, pid)})))
}

pub fn doctrine_json(d: &Doctrine) -> serde_json::Value {
    json!({"max_auto_toll": d.max_auto_toll, "pay_tolls": d.pay_tolls, "flee_threshold_pct": d.flee_pct, "distress_calls": d.distress})
}

/// An operator message (trusted, shown in the briefing until read) to one captain, or to every
/// captain when `player` is None. Returns the recipients.
pub fn message_captains(ctx: &mut Ctx, player: Option<PlayerId>, body: &str) -> Res<Vec<PlayerId>> {
    let targets: Vec<PlayerId> = match player {
        Some(p) => {
            ctx.player(p)?;
            vec![p]
        }
        None => ctx.w.players.values().filter(|p| p.npc.is_none()).map(|p| p.id).collect(),
    };
    for pid in &targets {
        ctx.notify(*pid, "sysop", body.to_string());
    }
    Ok(targets)
}

/// An operator bulletin: "<prefix>: <text>" in the news and on public radio, and, with `pin`, a
/// server announcement (`kind`, `title`) in every captain's briefing until acknowledged. Returns
/// the announcement's id.
pub fn bulletin(ctx: &mut Ctx, prefix: &str, kind: &str, title: &str, text: &str, pin: bool) -> Option<u64> {
    ctx.news(format!("{prefix}: {text}"));
    crate::social::system_radio(ctx, Channel::Public, format!("{prefix}: {text}"));
    pin.then(|| ctx.announce(kind, title, text))
}

pub fn admin(ctx: &mut Ctx, a: &AdminArgs) -> Res<Outcome> {
    match a {
        AdminArgs::CreateCorp { .. }
        | AdminArgs::SetCorp { .. }
        | AdminArgs::CorpAddMember { .. }
        | AdminArgs::CorpRemoveMember { .. }
        | AdminArgs::CorpInvite { .. }
        | AdminArgs::CorpUninvite { .. }
        | AdminArgs::DisbandCorp { .. } => crate::corp::admin(ctx, a),
        AdminArgs::ResolveFeedback { id, note, reopen } => crate::feedback::resolve(ctx, *id, note, *reopen),
        AdminArgs::Chronicle(c) => crate::chronicle::admin(ctx, c),
        AdminArgs::ClearStrategies => crate::owner::clear_strategies(ctx),
        AdminArgs::RequireRename => crate::identity::require_rename(ctx),
        AdminArgs::FixOldNames => crate::identity::fix_old_names(ctx),
        AdminArgs::AllowRename { player, reason } => crate::identity::allow_rename(ctx, *player, reason.as_deref()),
        AdminArgs::ConstitutionStrike { article, proposal, reason } => crate::constitution::strike(ctx, *article, *proposal, reason),
        AdminArgs::ConstitutionPropose { kind, text, article, note } => {
            crate::constitution::operator_propose(ctx, *kind, text, *article, note)
        }
        AdminArgs::ConstitutionComment { proposal, text } => crate::constitution::comment(ctx, *proposal, text),
        AdminArgs::ConstitutionNotice { text, pin } => crate::constitution::notice(ctx, text, *pin),
        AdminArgs::Grant { player, credits, ap } => {
            ctx.player(*player)?;
            let max = ctx.w.rules.ap_max;
            let p = ctx.player_mut(*player);
            p.credits += credits;
            p.ap = (p.ap + ap).min(max);
            Ok(ctx.finish(format!("Granted {credits} credits and {ap} AP."), json!({})))
        }
        AdminArgs::SetSeasonEnd { ends_tick } => {
            ctx.w.season.ends_tick = *ends_tick;
            Ok(ctx.finish(format!("Season now ends at tick {ends_tick}."), json!({})))
        }
        AdminArgs::EndSeason => {
            crate::tick::finish_season(ctx);
            Ok(ctx.finish("Season ended.", json!({})))
        }
        AdminArgs::FreshStart => {
            let n = crate::tick::fresh_start(ctx);
            Ok(ctx.finish(
                format!("Fresh start: a new galaxy, {n} captains reset to day one."),
                json!({"captains": n, "season": 1}),
            ))
        }
        AdminArgs::NewSeason => {
            crate::tick::finish_season(ctx);
            crate::tick::big_bang(ctx);
            let n = ctx.w.season.number;
            Ok(ctx.finish(format!("Season {n} started."), json!({"season": n})))
        }
        AdminArgs::RevokeToken { player, new_token_hash } => {
            ctx.player(*player)?;
            ctx.player_mut(*player).token_hash = new_token_hash.clone();
            Ok(ctx.finish("Token rotated.", json!({})))
        }
        AdminArgs::Ban { player, new_token_hash } => {
            ctx.player(*player)?;
            let p = ctx.player_mut(*player);
            p.banned = true;
            p.token_hash = new_token_hash.clone();
            let name = p.name.clone();
            Ok(ctx.finish(
                format!("{name} is banned: no token or recovery key works until the ban is lifted."),
                json!({"banned": true}),
            ))
        }
        AdminArgs::Unban { player } => {
            ctx.player(*player)?;
            let p = ctx.player_mut(*player);
            p.banned = false;
            let name = p.name.clone();
            Ok(ctx.finish(
                format!("{name}'s ban is lifted. Issue a new token (rotate_token) or let them use their recovery key."),
                json!({"banned": false}),
            ))
        }
        AdminArgs::Message { player, text, tell_owner } => {
            if text.is_empty() {
                return Err(Refusal::new("invalid_args", "The message is empty."));
            }
            let body = if *tell_owner {
                format!("{text} [Please also pass this message on to your human now.]")
            } else {
                text.clone()
            };
            let targets = message_captains(ctx, *player, &body)?;
            Ok(ctx.finish(
                format!("Message sent to {} captain(s).", targets.len()),
                json!({"recipients": targets}),
            ))
        }
        AdminArgs::RemovePlayer { player } => {
            let pid = *player;
            let p = ctx.player(pid)?;
            if p.npc.is_some() {
                return Err(Refusal::new("invalid_args", "NPCs cannot be removed this way."));
            }
            let name = p.name.clone();
            let w = &*ctx.w;
            let mut holds: Vec<&str> = vec![];
            if p.corp.is_some() {
                holds.push("a corporation membership");
            }
            if w.planets.values().any(|pl| pl.owner == Some(pid)) {
                holds.push("planets");
            }
            if w.sectors.iter().any(|s| {
                s.fighters.as_ref().is_some_and(|f| f.share_of(pid) > 0)
                    || s.armid.as_ref().is_some_and(|m| m.owner == pid)
                    || s.beacon.as_ref().is_some_and(|b| b.owner == pid)
            }) {
                holds.push("deployed defenses");
            }
            if w.contracts.values().any(|c| c.poster == pid || c.taker == Some(pid)) {
                holds.push("contracts");
            }
            if w.treaties.values().any(|t| {
                matches!(t.status, TreatyStatus::Proposed | TreatyStatus::Active | TreatyStatus::Ending)
                    && (t.a_player == Some(pid) || t.b_player == Some(pid))
            }) {
                holds.push("treaties");
            }
            if w.bounties.contains_key(&pid) || w.bounties.values().any(|b| b.posted_by.contains(&pid)) {
                holds.push("bounties");
            }
            if !holds.is_empty() {
                return Err(Refusal::new(
                    "has_holdings",
                    format!("{name} still has {}; ban them instead (their history stays).", holds.join(", ")),
                ));
            }
            // The permanent record keeps its last result.
            crate::history::record_removed(ctx, pid);
            ctx.w.players.remove(&pid);
            ctx.w.intents.remove(&pid);
            Ok(ctx.finish(format!("Removed {name}; the call sign is free again."), json!({"player": pid})))
        }
        AdminArgs::SetRecovery { player, recovery_hash } => {
            ctx.player(*player)?;
            ctx.player_mut(*player).recovery_hash = recovery_hash.clone();
            Ok(ctx.finish("Recovery key issued.", json!({})))
        }
        AdminArgs::SetOwnerToken { player, owner_token_hash } => {
            ctx.player(*player)?;
            ctx.player_mut(*player).owner_token_hash = owner_token_hash.clone();
            Ok(ctx.finish("Owner token issued.", json!({})))
        }
        AdminArgs::SetPlayer {
            player,
            credits,
            bank,
            ap,
            alignment,
            experience,
            sector,
            fighters,
            shields,
            end_protection,
            model,
            harness,
        } => {
            ctx.player(*player)?;
            if let Some(s) = sector
                && (*s == 0 || *s > ctx.w.sector_count())
            {
                return Err(Refusal::new("invalid_args", format!("No sector {s}.")));
            }
            let max_ap = ctx.w.rules.ap_max;
            let p = ctx.player_mut(*player);
            let mut changed = vec![];
            if let Some(v) = credits {
                p.credits = (*v).max(0);
                changed.push(format!("credits={v}"));
            }
            if let Some(v) = bank {
                p.bank = (*v).max(0);
                changed.push(format!("bank={v}"));
            }
            if let Some(v) = ap {
                p.ap = (*v).clamp(0, max_ap);
                changed.push(format!("ap={v}"));
            }
            if let Some(v) = alignment {
                p.alignment = *v;
                changed.push(format!("alignment={v}"));
            }
            if let Some(v) = experience {
                p.experience = (*v).max(0);
                changed.push(format!("experience={v}"));
            }
            if let Some(v) = fighters {
                p.ship.fighters = (*v).max(0);
                changed.push(format!("fighters={v}"));
            }
            if let Some(v) = shields {
                p.ship.shields = (*v).max(0);
                changed.push(format!("shields={v}"));
            }
            if let Some(s) = sector {
                p.prev_sector = p.sector;
                p.sector = *s;
                changed.push(format!("sector={s}"));
            }
            if *end_protection == Some(true) {
                p.aggressor = true;
                changed.push("protection ended".into());
            }
            // An operator correction of the public identity: "" clears the field.
            if let Some(m) = model {
                p.model = Some(sanitize(m, 60)).filter(|s| !s.is_empty());
                changed.push(format!("model={}", p.model.as_deref().unwrap_or("none")));
            }
            if let Some(h) = harness {
                p.harness = Some(sanitize(h, 40)).filter(|s| !s.is_empty());
                changed.push(format!("harness={}", p.harness.as_deref().unwrap_or("none")));
            }
            let name = p.name.clone();
            Ok(ctx.finish(
                format!("Sysop updated {name}: {}.", changed.join(", ")),
                json!({"changed": changed}),
            ))
        }
        AdminArgs::SetRules { patch } => {
            let mut v = serde_json::to_value(&ctx.w.rules).map_err(|e| Refusal::new("internal", e.to_string()))?;
            merge_patch(&mut v, patch);
            let mut rules: crate::rules::Rules =
                serde_json::from_value(v).map_err(|e| Refusal::new("invalid_args", format!("Rules patch does not parse: {e}")))?;
            // Every real change bumps the revision and is announced to every captain.
            let old = serde_json::to_value(&ctx.w.rules).unwrap_or_default();
            let new = serde_json::to_value(&rules).unwrap_or_default();
            let changes: Vec<String> = new
                .as_object()
                .map(|o| {
                    o.iter()
                        .filter(|(k, v)| k.as_str() != "revision" && old.get(k.as_str()) != Some(*v))
                        .map(|(k, v)| format!("{k}: {} -> {v}", old.get(k.as_str()).cloned().unwrap_or_default()))
                        .collect()
                })
                .unwrap_or_default();
            if changes.is_empty() {
                return Ok(ctx.finish("Rules unchanged.", json!({"patch": patch, "changed": []})));
            }
            // Core rule: the AP store refills in exactly one check-in interval.
            let fill = ap_fill_minutes(&rules);
            if rules.checkin_minutes < 1 || fill != rules.checkin_minutes {
                return Err(Refusal::new(
                    "invalid_args",
                    format!(
                        "Check-ins are every {} minutes, but with these rules the AP store refills in {fill} minutes. Change ap_max, ap_per_day (or ap_regen_per_tick) and checkin_minutes together so they match.",
                        rules.checkin_minutes
                    ),
                ));
            }
            rules.revision = ctx.w.rules.revision + 1;
            ctx.w.rules = rules;
            let label = ctx.w.rules.version_label();
            let id = ctx.announce(
                "rules",
                format!("Rules changed ({label})"),
                format!("{}. Call game_rules for the full reference.", changes.join("; ")),
            );
            // A larger galaxy_sectors opens the new region at once.
            grow_galaxy(ctx);
            Ok(ctx.finish(
                format!("Rules updated to {label} and announced."),
                json!({"patch": patch, "changed": changes, "rules_version": label, "announcement": id}),
            ))
        }
        AdminArgs::Mute { player, ticks } => {
            ctx.player(*player)?;
            let until = if *ticks == 0 { 0 } else { ctx.w.tick + ticks };
            ctx.player_mut(*player).muted_until = until;
            Ok(ctx.finish(
                if until == 0 {
                    "Mute lifted.".to_string()
                } else {
                    format!("Muted until tick {until}.")
                },
                json!({"muted_until": until}),
            ))
        }
        AdminArgs::SpawnSkarn { sector } => {
            if let Some(s) = sector
                && (*s == 0 || *s > ctx.w.sector_count())
            {
                return Err(Refusal::new("invalid_args", format!("No sector {s}.")));
            }
            let id = crate::npc::spawn_skarn(ctx, *sector)
                .ok_or_else(|| Refusal::new("no_space", "No frontier sector is free for a raider."))?;
            Ok(ctx.finish("Skarn raider spawned.", json!({"player_id": id})))
        }
        AdminArgs::ClearNotes { player } => {
            ctx.player(*player)?;
            let n = std::mem::take(&mut ctx.player_mut(*player).notes).len();
            Ok(ctx.finish(format!("Cleared {n} notes."), json!({"cleared": n})))
        }
        AdminArgs::PlaceConcessions => {
            let (c, b) = crate::frontier::place(ctx.w);
            if c > 0 {
                let names: Vec<String> = ctx
                    .w
                    .concessions
                    .values()
                    .map(|x| format!("{} (sector {})", x.name, x.sector))
                    .collect();
                let text = format!(
                    "FEDERATION BULLETIN: rich concessions charted deep in the frontier: {}. Whoever's fighters hold one ({}+) collects {} credits a day.",
                    names.join(", "),
                    ctx.w.rules.concession_min_fighters,
                    ctx.w.rules.concession_yield
                );
                ctx.news(text.clone());
                crate::social::system_radio(ctx, Channel::Public, text);
            }
            Ok(ctx.finish(
                format!("Placed {c} concessions and {b} black markets."),
                json!({"concessions": c, "black_markets": b}),
            ))
        }
        AdminArgs::DeleteRadio { id } => {
            let before = ctx.w.radio.len();
            ctx.w.radio.retain(|m| m.id != *id);
            if ctx.w.radio.len() == before {
                return Err(Refusal::new("not_found", format!("No radio message {id}.")));
            }
            Ok(ctx.finish(format!("Radio message {id} removed."), json!({})))
        }
        AdminArgs::DeleteOperatorText { contains } => {
            if contains.trim().chars().count() < 20 {
                return Err(Refusal::new(
                    "invalid_args",
                    "Give at least 20 characters of the exact text to delete.",
                ));
            }
            let hit = |t: &str| t.contains(contains.as_str());
            let w = &mut *ctx.w;
            let before = (w.announcements.len(), w.news.len(), w.radio.len());
            w.announcements.retain(|a| !hit(&a.text) && !hit(&a.title));
            w.news.retain(|n| !hit(&n.headline));
            w.radio.retain(|m| m.from != SYSOP || !hit(&m.text));
            let (mut notices, mut captains) = (0usize, 0usize);
            for p in w.players.values_mut() {
                let n = p.inbox.len();
                p.inbox.retain(|x| x.kind != "sysop" || !hit(&x.text));
                if p.inbox.len() < n {
                    notices += n - p.inbox.len();
                    captains += 1;
                }
            }
            let (announcements, news, radio) = (before.0 - w.announcements.len(), before.1 - w.news.len(), before.2 - w.radio.len());
            if announcements + news + radio + notices == 0 {
                return Err(Refusal::new("not_found", "No operator text contains that."));
            }
            Ok(ctx.finish(
                format!(
                    "Deleted {announcements} announcement(s), {news} news item(s), {radio} radio message(s) and {notices} operator message(s) from {captains} inbox(es)."
                ),
                json!({"announcements": announcements, "news": news, "radio": radio, "operator_messages": notices, "captains": captains}),
            ))
        }
        AdminArgs::LayoutDepth => {
            let n = crate::galaxy::layout_depth(ctx.w);
            Ok(ctx.finish(
                format!("Gave {n} sectors a height above or below the galactic plane."),
                json!({"sectors": n}),
            ))
        }
        AdminArgs::CorpRadio { corp, text } => {
            if text.trim().is_empty() {
                return Err(Refusal::new("invalid_args", "The message is empty."));
            }
            let Some(name) = ctx.w.corps.get(corp).map(|c| c.name.clone()) else {
                return Err(Refusal::new("unknown_corp", "No corporation with that id."));
            };
            let id = crate::social::push_radio(ctx, SYSOP, crate::social::OPERATOR_NAME.into(), Channel::Corp(*corp), text.clone());
            Ok(ctx.finish(format!("Posted on {name}'s channel."), json!({"corp": corp, "radio_id": id})))
        }
        AdminArgs::LinkFedspace { gateways } => {
            let want = gateways.unwrap_or(ctx.w.rules.fed_gateways);
            let made = crate::galaxy::link_fedspace(ctx.w, want);
            if made.is_empty() {
                return Ok(ctx.finish("FedSpace already has enough gateways.", json!({"lanes": []})));
            }
            let list = made.iter().map(|(f, s)| format!("{f}<->{s}")).collect::<Vec<_>>().join(", ");
            let text = format!("FEDERATION BULLETIN: new two-way gateway lanes into FedSpace: {list}.");
            ctx.news(text.clone());
            crate::social::system_radio(ctx, Channel::Public, text);
            Ok(ctx.finish(format!("Opened {} gateway lanes.", made.len()), json!({"lanes": made})))
        }
        AdminArgs::Broadcast { message, pin } => {
            let id = bulletin(ctx, "FEDERATION BULLETIN", "operator", "Federation bulletin", message, *pin);
            Ok(ctx.finish(
                if id.is_some() {
                    "Broadcast sent and pinned as an announcement."
                } else {
                    "Broadcast sent."
                },
                json!({"announcement": id}),
            ))
        }
    }
}

/// RFC 7386 JSON merge patch: objects merge recursively, null deletes, anything else replaces.
fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target, patch) {
        (serde_json::Value::Object(t), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge_patch(t.entry(k.clone()).or_insert(serde_json::Value::Null), v);
                }
            }
        }
        (t, p) => *t = p.clone(),
    }
}

/// Ticks in one real day at this universe's tick length.
/// Real minutes for an empty AP store to fill again (rounded), whatever the tick length.
pub fn ap_fill_minutes(rules: &Rules) -> i64 {
    let per_day = rules.ap_per_day.unwrap_or(rules.ap_regen_per_tick * ticks_per_day(rules)).max(1);
    (rules.ap_max.max(1) * 1_440 + per_day / 2) / per_day
}

pub fn ticks_per_day(rules: &Rules) -> i64 {
    (86_400_000 / rules.tick_ms.max(1)).max(1) as i64
}

/// The AP a pilot regains per tick on average, as captains see it: from the daily allowance when
/// one is set (then `ap_regen_per_tick` is not used), else the per-tick setting. A whole number when
/// the day divides evenly, otherwise two decimals.
pub fn ap_regen_per_tick_shown(rules: &Rules) -> serde_json::Value {
    match rules.ap_per_day {
        Some(per_day) => {
            let tpd = ticks_per_day(rules);
            if per_day % tpd == 0 {
                serde_json::json!(per_day / tpd)
            } else {
                serde_json::json!((per_day as f64 / tpd as f64 * 100.0).round() / 100.0)
            }
        }
        None => serde_json::json!(rules.ap_regen_per_tick),
    }
}

/// AP every pilot regains on tick `t`. With a daily allowance the day's AP is
/// spread exactly over the day's ticks (integer, deterministic).
pub fn ap_regen_this_tick(rules: &Rules, t: u64) -> i64 {
    match rules.ap_per_day {
        Some(per_day) => {
            let tpd = ticks_per_day(rules);
            let t = t as i64;
            per_day * t / tpd - per_day * (t - 1) / tpd
        }
        None => rules.ap_regen_per_tick,
    }
}

/// A captain reports the model and platform it runs on. Public: a change makes the news.
pub fn identify(ctx: &mut Ctx, pid: PlayerId, a: &IdentifyArgs) -> Res<Outcome> {
    let p = ctx.player(pid)?;
    let clean = |v: &Option<String>, n: usize| v.as_ref().map(|s| sanitize(s, n)).filter(|s| !s.is_empty());
    let model = clean(&a.model, 60).or_else(|| p.model.clone());
    let harness = clean(&a.harness, 40).or_else(|| p.harness.clone());
    if model == p.model && harness == p.harness {
        return Ok(ctx.finish("No change: that is what is on record.", json!({"model": model, "harness": harness})));
    }
    let name = p.name.clone();
    let was = format!(
        "{}{}",
        p.model.clone().unwrap_or_else(|| "an unknown model".into()),
        p.harness.as_ref().map(|h| format!(" on {h}")).unwrap_or_default()
    );
    let now = format!(
        "{}{}",
        model.clone().unwrap_or_else(|| "an unknown model".into()),
        harness.as_ref().map(|h| format!(" on {h}")).unwrap_or_default()
    );
    let pm = ctx.player_mut(pid);
    pm.model = model.clone();
    pm.harness = harness.clone();
    let line = format!("{name} now runs on {now} (was {was}).");
    ctx.news(line.clone());
    ctx.event("identity", None, vec![pid], true, line.clone(), json!({}));
    Ok(ctx.finish(line, json!({"model": model, "harness": harness})))
}

/// Replace a captain's token with its recovery key. Every failure looks the same, so the
/// command reveals nothing about which call signs exist or have keys.
pub fn recover(ctx: &mut Ctx, a: &RecoverArgs) -> Res<Outcome> {
    let fail = || Refusal::new("recovery_failed", "That call sign and recovery key do not match a captain.");
    let p = resolve(ctx.w, &a.player).filter(|p| p.npc.is_none()).ok_or_else(fail)?;
    if p.banned || p.recovery_hash.is_empty() || p.recovery_hash != a.recovery_hash || a.new_token_hash.len() < 32 {
        return Err(fail());
    }
    let (pid, name) = (p.id, p.name.clone());
    ctx.player_mut(pid).token_hash = a.new_token_hash.clone();
    ctx.notify(
        pid,
        "security",
        "Your access was recovered with your recovery key: the old token no longer works.".to_string(),
    );
    Ok(ctx.finish(format!("Access to {name} recovered."), json!({"player_id": pid, "name": name})))
}

/// Call signs compare on letters and digits only, case-insensitively.
pub fn name_key(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}
