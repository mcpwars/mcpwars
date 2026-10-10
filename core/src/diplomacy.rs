//! Diplomacy and the public registry: treaties between corporations and
//! captains, declared wars, captains' public intents, and the combat-report log.
//!
//! Treaties are engine objects rather than radio promises: a NAP makes the
//! partners' automatic defenses hold fire, a toll exemption waves partners
//! through toll stacks, chart sharing merges starcharts, and attacking a
//! partner breaks the treaty and forfeits the breaker's bond. Wars are declared,
//! delayed, bounded in time, and make kills between the belligerents
//! alignment-free.
//!
//! A side of a treaty is a corporation (signed by its CEO, covering every
//! member) or, since engine 1.32, a captain signing for itself (covering only
//! that captain). Captain parties sign only nap and toll_exempt; a war between
//! corporations does not void a captain's own treaty.

use serde_json::{Value, json};

use crate::cmd::*;
use crate::engine::{Ctx, Event, Outcome, Refusal, Res};
use crate::model::*;
use crate::player;

const MAX_REPORTS: usize = 300;
/// Unsigned offers lapse after this many ticks.
const OFFER_TTL: u64 = 720;
const COMBAT_KINDS: [&str; 9] = [
    "combat",
    "ship_destroyed",
    "boarding",
    "planet_captured",
    "skarn_raid",
    "fed_intercept",
    "toll_turned_back",
    "fighters_destroyed",
    "war",
];

// ---------- relations ----------

pub fn corp_of(w: &World, pid: PlayerId) -> Option<CorpId> {
    w.players.get(&pid).and_then(|p| p.corp)
}

fn live(t: &Treaty) -> bool {
    matches!(t.status, TreatyStatus::Active | TreatyStatus::Ending)
}

/// One side of a treaty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Party {
    Corp(CorpId),
    Captain(PlayerId),
}

fn side_a(t: &Treaty) -> Party {
    t.a_player.map_or(Party::Corp(t.a), Party::Captain)
}

fn side_b(t: &Treaty) -> Party {
    t.b_player.map_or(Party::Corp(t.b), Party::Captain)
}

/// Has a captain party (engine 1.32)?
fn personal(t: &Treaty) -> bool {
    t.a_player.is_some() || t.b_player.is_some()
}

/// Does `party` cover pilot `pid` (a corporation covers its members, a captain only itself)?
fn covers(w: &World, party: Party, pid: PlayerId) -> bool {
    match party {
        Party::Corp(c) => corp_of(w, pid) == Some(c),
        Party::Captain(p) => p == pid,
    }
}

/// Does `t` bind pilots `x` and `y` to each other?
fn binds(w: &World, t: &Treaty, x: PlayerId, y: PlayerId) -> bool {
    let (a, b) = (side_a(t), side_b(t));
    (covers(w, a, x) && covers(w, b, y)) || (covers(w, a, y) && covers(w, b, x))
}

/// Can `pid` act for `party` (the captain itself, or the corporation's CEO)?
fn acts_for(w: &World, pid: PlayerId, party: Party) -> bool {
    match party {
        Party::Corp(c) => w.corps.get(&c).is_some_and(|c| c.ceo == pid),
        Party::Captain(p) => p == pid,
    }
}

/// Who answers for `party`: the captain, or the corporation's CEO.
fn leader(w: &World, party: Party) -> Option<PlayerId> {
    match party {
        Party::Corp(c) => w.corps.get(&c).map(|c| c.ceo),
        Party::Captain(p) => w.players.contains_key(&p).then_some(p),
    }
}

/// A corporation-to-corporation treaty between `a` and `b`.
fn pair(t: &Treaty, a: CorpId, b: CorpId) -> bool {
    !personal(t) && ((t.a == a && t.b == b) || (t.a == b && t.b == a))
}

/// Is there a live treaty of one of `kinds` between corporations `a` and `b`?
pub fn corps_have(w: &World, a: CorpId, b: CorpId, kinds: &[TreatyKind]) -> bool {
    w.treaties.values().any(|t| live(t) && pair(t, a, b) && kinds.contains(&t.kind))
}

/// Is there a live treaty with a captain party, of one of `kinds`, binding pilots `x` and `y`?
fn captains_have(w: &World, x: PlayerId, y: PlayerId, kinds: &[TreatyKind]) -> bool {
    w.treaties
        .values()
        .any(|t| live(t) && personal(t) && kinds.contains(&t.kind) && binds(w, t, x, y))
}

pub fn corps_at_war(w: &World, a: CorpId, b: CorpId) -> bool {
    w.wars
        .values()
        .any(|x| x.status == WarStatus::Active && ((x.attacker == a && x.defender == b) || (x.attacker == b && x.defender == a)))
}

/// Are these two pilots' corporations at war?
pub fn at_war(w: &World, a: PlayerId, b: PlayerId) -> bool {
    match (corp_of(w, a), corp_of(w, b)) {
        (Some(ca), Some(cb)) if ca != cb => corps_at_war(w, ca, cb),
        _ => false,
    }
}

/// Same corporation, a live NAP / mutual-defense treaty between theirs, or a captain's own
/// NAP binding them: automatic defenses (fighters, mines, planets) hold fire.
pub fn at_peace(w: &World, a: PlayerId, b: PlayerId) -> bool {
    if w.friendly(a, b) {
        return true;
    }
    let corps = match (corp_of(w, a), corp_of(w, b)) {
        (Some(ca), Some(cb)) => !corps_at_war(w, ca, cb) && corps_have(w, ca, cb, &[TreatyKind::Nap, TreatyKind::MutualDefense]),
        _ => false,
    };
    corps || captains_have(w, a, b, &[TreatyKind::Nap])
}

/// `owner`'s toll fighters let `pid` through free.
pub fn toll_exempt(w: &World, owner: PlayerId, pid: PlayerId) -> bool {
    if at_peace(w, owner, pid) {
        return true;
    }
    let corps = match (corp_of(w, owner), corp_of(w, pid)) {
        (Some(ca), Some(cb)) => !corps_at_war(w, ca, cb) && corps_have(w, ca, cb, &[TreatyKind::TollExempt]),
        _ => false,
    };
    corps || captains_have(w, owner, pid, &[TreatyKind::TollExempt])
}

/// Corporations whose charts a member of `corp` sees (its chart-share partners).
pub fn chart_partners(w: &World, corp: CorpId) -> Vec<CorpId> {
    w.treaties
        .values()
        .filter(|t| live(t) && !personal(t) && t.kind == TreatyKind::ChartShare && (t.a == corp || t.b == corp))
        .map(|t| if t.a == corp { t.b } else { t.a })
        .collect()
}

/// Corporations bound to answer `corp`'s distress calls.
pub fn defense_partners(w: &World, corp: CorpId) -> Vec<CorpId> {
    w.treaties
        .values()
        .filter(|t| live(t) && !personal(t) && t.kind == TreatyKind::MutualDefense && (t.a == corp || t.b == corp))
        .map(|t| if t.a == corp { t.b } else { t.a })
        .collect()
}

// ---------- helpers ----------

fn corp_name(w: &World, c: CorpId) -> String {
    w.corps.get(&c).map(|c| c.name.clone()).unwrap_or_else(|| format!("Corp #{c}"))
}

fn party_name(w: &World, p: Party) -> String {
    match p {
        Party::Corp(c) => corp_name(w, c),
        Party::Captain(x) => w.player_name(x),
    }
}

/// The side `pid` signs for: its corporation if it is the CEO (unless `personal`), else itself.
fn my_party(w: &World, pid: PlayerId, personal: bool) -> Party {
    match corp_of(w, pid) {
        Some(c) if !personal && w.corps.get(&c).is_some_and(|c| c.ceo == pid) => Party::Corp(c),
        _ => Party::Captain(pid),
    }
}

fn ceo_corp(ctx: &Ctx, pid: PlayerId) -> Res<CorpId> {
    let p = ctx.player(pid)?;
    let cid = p.corp.ok_or_else(|| {
        Refusal::new("no_corp", "Wars are between corporations.").hint("corp action=create (a one-pilot corporation is fine).")
    })?;
    let c = &ctx.w.corps[&cid];
    if c.ceo != pid {
        return Err(Refusal::new("not_ceo", format!("Only the CEO of {} can do that.", c.name)));
    }
    Ok(cid)
}

fn corp_by_name(w: &World, name: &str) -> Option<CorpId> {
    let n = name.trim().to_lowercase();
    w.corps.values().find(|c| c.name.to_lowercase() == n).map(|c| c.id)
}

/// Charge a corporation: the treasury's free funds first (members' deposits are reserved and
/// never spent), then its CEO's ship credits.
fn charge(ctx: &mut Ctx, corp: CorpId, ceo: PlayerId, amount: i64, what: &str) -> Res<()> {
    if amount <= 0 {
        return Ok(());
    }
    let free = ctx.w.corps.get(&corp).map(|c| c.free_funds()).unwrap_or(0);
    let credits = ctx.player(ceo)?.credits;
    if free + credits < amount {
        return Err(Refusal::new(
            "insufficient_credits",
            format!(
                "{what} needs {amount} credits; the treasury has {free} free (members' deposits are reserved) and you carry {credits}."
            ),
        ));
    }
    let from_treasury = free.min(amount);
    if let Some(c) = ctx.w.corps.get_mut(&corp) {
        c.treasury -= from_treasury;
    }
    ctx.player_mut(ceo).credits -= amount - from_treasury;
    Ok(())
}

/// Charge a treaty side: a corporation as above, a captain from its own ship credits.
fn charge_party(ctx: &mut Ctx, party: Party, pid: PlayerId, amount: i64, what: &str) -> Res<()> {
    match party {
        Party::Corp(c) => charge(ctx, c, pid, amount, what),
        Party::Captain(x) => {
            if amount <= 0 {
                return Ok(());
            }
            let credits = ctx.player(x)?.credits;
            if credits < amount {
                return Err(Refusal::new(
                    "insufficient_credits",
                    format!("{what} needs {amount} credits; you carry {credits}."),
                ));
            }
            ctx.player_mut(x).credits -= amount;
            Ok(())
        }
    }
}

fn pay(ctx: &mut Ctx, corp: CorpId, amount: i64) {
    if amount > 0
        && let Some(c) = ctx.w.corps.get_mut(&corp)
    {
        c.treasury += amount;
    }
}

/// Pay a treaty side: into a corporation's treasury (free funds) or a captain's ship credits.
fn pay_party(ctx: &mut Ctx, party: Party, amount: i64) {
    match party {
        Party::Corp(c) => pay(ctx, c, amount),
        Party::Captain(x) => {
            if amount > 0
                && let Some(p) = ctx.w.players.get_mut(&x)
            {
                p.credits += amount;
            }
        }
    }
}

fn next_id(w: &mut World) -> u64 {
    w.next_registry_id = w.next_registry_id.max(1);
    let id = w.next_registry_id;
    w.next_registry_id += 1;
    id
}

/// Headline + public radio bulletin + spectator event.
fn proclaim(ctx: &mut Ctx, players: Vec<PlayerId>, text: String) {
    ctx.news(text.clone());
    crate::social::system_radio(ctx, Channel::Public, text.clone());
    ctx.event("diplomacy", None, players, true, text, json!({}));
}

fn kind_name(k: TreatyKind) -> &'static str {
    match k {
        TreatyKind::Nap => "non-aggression pact",
        TreatyKind::TollExempt => "toll exemption",
        TreatyKind::ChartShare => "chart-sharing pact",
        TreatyKind::MutualDefense => "mutual-defense pact",
    }
}

/// Return the bonds both sides posted.
fn refund(ctx: &mut Ctx, t: &Treaty) {
    if t.bond_a_posted {
        pay_party(ctx, side_a(t), t.bond);
    }
    if t.bond_b_posted {
        pay_party(ctx, side_b(t), t.bond);
    }
}

/// A reputation fact about one treaty side.
#[derive(Clone, Copy)]
enum Fact {
    Signed,
    Kept,
    Broken,
    Forfeited(i64),
}

/// Count `fact` on `party`'s public record: the corporation's record for a corporate side, the
/// captain's own stats for a captain side (engine 1.32).
fn note(ctx: &mut Ctx, party: Party, fact: Fact) {
    match party {
        Party::Corp(c) => {
            if let Some(c) = ctx.w.corps.get_mut(&c) {
                let r = &mut c.record;
                match fact {
                    Fact::Signed => r.treaties_signed += 1,
                    Fact::Kept => r.treaties_kept += 1,
                    Fact::Broken => r.treaties_broken += 1,
                    Fact::Forfeited(n) => r.bonds_forfeited += n,
                }
            }
        }
        Party::Captain(p) => {
            if let Some(p) = ctx.w.players.get_mut(&p) {
                let s = &mut p.stats;
                match fact {
                    Fact::Signed => s.treaties_signed += 1,
                    Fact::Kept => s.treaties_kept += 1,
                    Fact::Broken => s.treaties_broken += 1,
                    Fact::Forfeited(n) => s.bonds_forfeited += n,
                }
            }
        }
    }
}

/// Why `me` may not sign with `other`, if it may not.
fn same_side(w: &World, me: Party, other: Party) -> Option<&'static str> {
    match (me, other) {
        (Party::Corp(x), Party::Corp(y)) if x == y => Some("That is your own corporation."),
        (Party::Corp(c), Party::Captain(p)) | (Party::Captain(p), Party::Corp(c)) if corp_of(w, p) == Some(c) => {
            Some("That captain and that corporation are one side: a member is already friendly.")
        }
        (Party::Captain(x), Party::Captain(y)) if x == y => Some("That is you."),
        (Party::Captain(x), Party::Captain(y)) if w.friendly(x, y) => Some("You are in the same corporation: already friendly."),
        _ => None,
    }
}

// ---------- commands ----------

pub fn treaty(ctx: &mut Ctx, pid: PlayerId, a: &TreatyArgs) -> Res<Outcome> {
    ctx.player(pid)?;
    let tick = ctx.w.tick;
    match a.action {
        TreatyAction::Propose => {
            let kind = a
                .kind
                .ok_or_else(|| Refusal::new("invalid_args", "kind is required: nap, toll_exempt, chart_share or mutual_defense."))?;
            let me = my_party(ctx.w, pid, a.personal);
            let other = match (a.captain.as_deref(), a.corp.as_deref()) {
                (Some(n), _) => Party::Captain(
                    player::resolve(ctx.w, n)
                        .filter(|p| p.npc.is_none())
                        .map(|p| p.id)
                        .ok_or_else(|| Refusal::new("unknown_player", "captain must name another captain."))?,
                ),
                (None, Some(n)) => Party::Corp(corp_by_name(ctx.w, n).ok_or_else(|| {
                    Refusal::new("unknown_corp", "corp must name another corporation.").hint("view_registry lists corporations.")
                })?),
                (None, None) => {
                    return Err(Refusal::new(
                        "invalid_args",
                        "Name the other side: corp (a corporation) or captain.",
                    ));
                }
            };
            if let Some(why) = same_side(ctx.w, me, other) {
                return Err(Refusal::new("invalid_args", why));
            }
            let captain_side = matches!(me, Party::Captain(_)) || matches!(other, Party::Captain(_));
            if captain_side && matches!(kind, TreatyKind::ChartShare | TreatyKind::MutualDefense) {
                return Err(
                    Refusal::new("corp_only", format!("A {} is signed only between corporations.", kind_name(kind)))
                        .hint("A captain may sign nap or toll_exempt for itself; a CEO signs for its corporation (omit personal)."),
                );
            }
            if let (Party::Corp(x), Party::Corp(y)) = (me, other)
                && corps_at_war(ctx.w, x, y)
            {
                return Err(Refusal::new("at_war", "You are at war with them.").hint("End the war first (war action=end or surrender)."));
            }
            if ctx.w.treaties.values().any(|t| {
                t.kind == kind
                    && (live(t) || t.status == TreatyStatus::Proposed)
                    && ((side_a(t) == me && side_b(t) == other) || (side_a(t) == other && side_b(t) == me))
            }) {
                return Err(Refusal::new(
                    "duplicate",
                    format!("A {} with them is already proposed or in force.", kind_name(kind)),
                ));
            }
            let bond = a.bond.unwrap_or(0).max(0);
            charge_party(ctx, me, pid, bond, "The bond")?;
            let id = next_id(ctx.w);
            let terms = a.terms.clone().unwrap_or_default();
            let (corp_a, cap_a) = match me {
                Party::Corp(c) => (c, None),
                Party::Captain(p) => (0, Some(p)),
            };
            let (corp_b, cap_b) = match other {
                Party::Corp(c) => (c, None),
                Party::Captain(p) => (0, Some(p)),
            };
            ctx.w.treaties.insert(
                id,
                Treaty {
                    id,
                    kind,
                    a: corp_a,
                    b: corp_b,
                    a_player: cap_a,
                    b_player: cap_b,
                    status: TreatyStatus::Proposed,
                    bond,
                    bond_a_posted: bond > 0,
                    bond_b_posted: false,
                    terms,
                    proposed_tick: tick,
                    signed_tick: None,
                    expires_tick: a.duration_ticks.map(|d| d.max(1)),
                    ends_tick: None,
                    broken_by: None,
                    broken_by_player: None,
                },
            );
            let (mine, theirs) = (party_name(ctx.w, me), party_name(ctx.w, other));
            let answerer = leader(ctx.w, other);
            if let Some(l) = answerer {
                ctx.notify(
                    l,
                    "treaty",
                    format!(
                        "{mine} proposes a {} (treaty #{id}, bond {bond} each). Answer with treaty action=accept or reject, treaty_id={id}.",
                        kind_name(kind)
                    ),
                );
            }
            ctx.event(
                "diplomacy",
                None,
                std::iter::once(pid).chain(answerer).collect(),
                true,
                format!("{mine} offered {theirs} a {}.", kind_name(kind)),
                json!({"treaty": id}),
            );
            let as_whom = if matches!(me, Party::Captain(_)) {
                " as a captain, for yourself"
            } else {
                ""
            };
            Ok(ctx.finish(
                format!("Offered {theirs} a {}{as_whom} (treaty #{id}).", kind_name(kind)),
                json!({"treaty_id": id, "bond_escrowed": bond}),
            ))
        }
        TreatyAction::Accept | TreatyAction::Reject | TreatyAction::Withdraw | TreatyAction::Cancel => {
            let id = a.treaty_id.ok_or_else(|| Refusal::new("invalid_args", "treaty_id is required."))?;
            let t = ctx
                .w
                .treaties
                .get(&id)
                .cloned()
                .ok_or_else(|| Refusal::new("unknown_treaty", format!("No treaty #{id}.")))?;
            let (sa, sb) = (side_a(&t), side_b(&t));
            let (an, bn) = (party_name(ctx.w, sa), party_name(ctx.w, sb));
            match a.action {
                TreatyAction::Accept | TreatyAction::Reject => {
                    if !acts_for(ctx.w, pid, sb) || t.status != TreatyStatus::Proposed {
                        return Err(Refusal::new(
                            "not_offered",
                            format!("Treaty #{id} is not an open offer to you or a corporation you lead."),
                        ));
                    }
                    if a.action == TreatyAction::Reject {
                        refund(ctx, &t);
                        ctx.w.treaties.get_mut(&id).unwrap().status = TreatyStatus::Rejected;
                        if let Some(l) = leader(ctx.w, sa) {
                            ctx.notify(l, "treaty", format!("{bn} rejected treaty #{id}."));
                        }
                        return Ok(ctx.finish(format!("Rejected treaty #{id}."), json!({})));
                    }
                    if !personal(&t) && corps_at_war(ctx.w, t.a, t.b) {
                        return Err(Refusal::new("at_war", "You are at war with them."));
                    }
                    charge_party(ctx, sb, pid, t.bond, "Your bond")?;
                    let tt = ctx.w.treaties.get_mut(&id).unwrap();
                    tt.status = TreatyStatus::Active;
                    tt.bond_b_posted = t.bond > 0;
                    tt.signed_tick = Some(tick);
                    tt.expires_tick = t.expires_tick.map(|d| tick + d);
                    note(ctx, sa, Fact::Signed);
                    note(ctx, sb, Fact::Signed);
                    proclaim(
                        ctx,
                        vec![pid],
                        format!(
                            "TREATY: {an} and {bn} signed a {} (treaty #{id}, bond {} each).",
                            kind_name(t.kind),
                            t.bond
                        ),
                    );
                    Ok(ctx.finish(format!("Signed treaty #{id} with {an}."), json!({"treaty_id": id})))
                }
                TreatyAction::Withdraw => {
                    if !acts_for(ctx.w, pid, sa) || t.status != TreatyStatus::Proposed {
                        return Err(Refusal::new("not_yours", format!("Treaty #{id} is not your open offer.")));
                    }
                    refund(ctx, &t);
                    ctx.w.treaties.get_mut(&id).unwrap().status = TreatyStatus::Ended;
                    Ok(ctx.finish(format!("Withdrew treaty #{id}; your bond was returned."), json!({})))
                }
                _ => {
                    let mine = if acts_for(ctx.w, pid, sa) {
                        sa
                    } else if acts_for(ctx.w, pid, sb) {
                        sb
                    } else {
                        return Err(Refusal::new(
                            "not_active",
                            format!("Treaty #{id} is not an active treaty of yours."),
                        ));
                    };
                    if t.status != TreatyStatus::Active {
                        return Err(Refusal::new(
                            "not_active",
                            format!("Treaty #{id} is not an active treaty of yours."),
                        ));
                    }
                    let ends = tick + ctx.w.rules.treaty_notice_ticks;
                    let tt = ctx.w.treaties.get_mut(&id).unwrap();
                    tt.status = TreatyStatus::Ending;
                    tt.ends_tick = Some(ends);
                    let who = party_name(ctx.w, mine);
                    proclaim(
                        ctx,
                        vec![pid],
                        format!("TREATY: {who} gave notice on treaty #{id} ({an} / {bn}); it ends at tick {ends}."),
                    );
                    Ok(ctx.finish(
                        format!("Notice given: treaty #{id} ends at tick {ends}, then bonds return."),
                        json!({"ends_tick": ends}),
                    ))
                }
            }
        }
    }
}

pub fn war(ctx: &mut Ctx, pid: PlayerId, a: &WarArgs) -> Res<Outcome> {
    let me = ceo_corp(ctx, pid)?;
    let tick = ctx.w.tick;
    match a.action {
        WarAction::Declare => {
            let other = a.corp.as_deref().and_then(|n| corp_by_name(ctx.w, n)).ok_or_else(|| {
                Refusal::new("unknown_corp", "corp must name another corporation.").hint("view_registry lists corporations.")
            })?;
            if other == me {
                return Err(Refusal::new("invalid_args", "That is your own corporation."));
            }
            if ctx.w.wars.values().any(|x| {
                x.status != WarStatus::Ended && ((x.attacker == me && x.defender == other) || (x.attacker == other && x.defender == me))
            }) {
                return Err(Refusal::new(
                    "already_at_war",
                    "A war between you is already declared or under way.",
                ));
            }
            let fee = ctx.w.rules.war_declare_fee;
            charge(ctx, me, pid, fee, "Declaring war")?;
            // Declaring war on a partner breaks every treaty with them: peace treaties forfeit our bond.
            let ids: Vec<u64> = ctx
                .w
                .treaties
                .values()
                .filter(|t| live(t) && pair(t, me, other))
                .map(|t| t.id)
                .collect();
            for tid in ids {
                break_treaty(ctx, tid, Party::Corp(me));
            }
            let id = next_id(ctx.w);
            let starts = tick + ctx.w.rules.war_delay_ticks;
            let ends = starts + ctx.w.rules.war_duration_ticks;
            let objective = a.objective.clone().unwrap_or_else(|| "unstated".into());
            let tribute = a.tribute.unwrap_or(0).max(0);
            ctx.w.wars.insert(
                id,
                War {
                    id,
                    attacker: me,
                    defender: other,
                    status: WarStatus::Declared,
                    objective: objective.clone(),
                    tribute,
                    declared_tick: tick,
                    starts_tick: starts,
                    ends_tick: ends,
                    outcome: None,
                    attacker_kills: 0,
                    defender_kills: 0,
                },
            );
            let (mine, theirs) = (corp_name(ctx.w, me), corp_name(ctx.w, other));
            proclaim(
                ctx,
                vec![pid],
                format!(
                    "WAR: {mine} declared war on {theirs} (war #{id}). Aim: {objective}. Tribute to end it: {tribute}. Hostilities open at tick {starts}."
                ),
            );
            Ok(ctx.finish(
                format!("War #{id} declared on {theirs}; fighting starts at tick {starts} and ends by tick {ends}."),
                json!({"war_id": id, "starts_tick": starts, "ends_tick": ends, "fee": fee}),
            ))
        }
        WarAction::End | WarAction::Surrender => {
            let id = a.war_id.ok_or_else(|| Refusal::new("invalid_args", "war_id is required."))?;
            let x = ctx
                .w
                .wars
                .get(&id)
                .cloned()
                .ok_or_else(|| Refusal::new("unknown_war", format!("No war #{id}.")))?;
            if x.status == WarStatus::Ended {
                return Err(Refusal::new("over", format!("War #{id} is already over.")));
            }
            let (an, dn) = (corp_name(ctx.w, x.attacker), corp_name(ctx.w, x.defender));
            let outcome = if a.action == WarAction::End {
                if x.attacker != me {
                    return Err(
                        Refusal::new("not_yours", "Only the corporation that declared a war can call it off.")
                            .hint("The defender can surrender."),
                    );
                }
                format!("{an} called off the war")
            } else {
                if x.defender != me {
                    return Err(Refusal::new(
                        "not_yours",
                        "Only the corporation a war was declared on can surrender.",
                    ));
                }
                let free = ctx.w.corps.get(&me).map(|c| c.free_funds()).unwrap_or(0);
                let credits = ctx.player(pid)?.credits;
                let paid = x.tribute.min(free + credits);
                charge(ctx, me, pid, paid, "Tribute")?;
                pay(ctx, x.attacker, paid);
                format!("{dn} surrendered and paid {paid} credits in tribute")
            };
            let w = ctx.w.wars.get_mut(&id).unwrap();
            w.status = WarStatus::Ended;
            w.outcome = Some(outcome.clone());
            w.ends_tick = w.ends_tick.min(tick);
            proclaim(ctx, vec![pid], format!("PEACE: war #{id} ({an} vs {dn}) is over: {outcome}."));
            Ok(ctx.finish(format!("War #{id} is over: {outcome}."), json!({"war_id": id})))
        }
    }
}

pub fn intent(ctx: &mut Ctx, pid: PlayerId, a: &IntentArgs) -> Res<Outcome> {
    ctx.player(pid)?;
    if a.kind == IntentKind::Clear {
        ctx.w.intents.remove(&pid);
        return Ok(ctx.finish("Intent withdrawn.", json!({})));
    }
    if let Some(s) = a.sector
        && ctx.w.sector(s).is_none()
    {
        return Err(Refusal::new("invalid_args", format!("No sector {s}.")));
    }
    let tick = ctx.w.tick;
    let ttl = a.ttl_ticks.unwrap_or(360).clamp(10, 1_440);
    let text = a.text.clone().unwrap_or_default();
    ctx.w.intents.insert(
        pid,
        Intent {
            player: pid,
            kind: a.kind,
            sector: a.sector,
            text,
            declared_tick: tick,
            expires_tick: tick + ttl,
        },
    );
    Ok(ctx.finish(
        format!("Public intent declared until tick {}.", tick + ttl),
        json!({"expires_tick": tick + ttl}),
    ))
}

// ---------- hooks ----------

/// Mark a treaty broken by side `breaker`; on a peace treaty the breaker's bond goes to the other side.
fn break_treaty(ctx: &mut Ctx, id: u64, breaker: Party) {
    let Some(t) = ctx.w.treaties.get(&id).cloned() else { return };
    let breaker_is_a = side_a(&t) == breaker;
    let victim = if breaker_is_a { side_b(&t) } else { side_a(&t) };
    let breaker_posted = if breaker_is_a { t.bond_a_posted } else { t.bond_b_posted };
    let victim_posted = if breaker_is_a { t.bond_b_posted } else { t.bond_a_posted };
    let peace = matches!(t.kind, TreatyKind::Nap | TreatyKind::MutualDefense);
    if victim_posted {
        pay_party(ctx, victim, t.bond);
    }
    if breaker_posted {
        pay_party(ctx, if peace { victim } else { breaker }, t.bond);
    }
    let tt = ctx.w.treaties.get_mut(&id).unwrap();
    tt.status = if peace { TreatyStatus::Broken } else { TreatyStatus::Ended };
    if peace {
        match breaker {
            Party::Corp(c) => tt.broken_by = Some(c),
            Party::Captain(p) => tt.broken_by_player = Some(p),
        }
    }
    tt.ends_tick = Some(ctx.w.tick);
    if peace {
        note(ctx, breaker, Fact::Broken);
        if breaker_posted && t.bond > 0 {
            note(ctx, breaker, Fact::Forfeited(t.bond));
        }
        let (bn, vn) = (party_name(ctx.w, breaker), party_name(ctx.w, victim));
        let forfeit = if breaker_posted && t.bond > 0 {
            format!(" {bn}'s bond of {} goes to {vn}.", t.bond)
        } else {
            String::new()
        };
        proclaim(
            ctx,
            vec![],
            format!(
                "TREATY BROKEN: {bn} broke its {} with {vn} (treaty #{id}).{forfeit}",
                kind_name(t.kind)
            ),
        );
    }
}

/// `attacker` just attacked something owned by `victim`: break every peace treaty binding them.
/// The breaker is the side covering the attacker: its corporation (any member's attack breaks a
/// corporate treaty) or the captain itself (only its own attack breaks a captain's treaty).
pub fn on_hostile_act(ctx: &mut Ctx, attacker: PlayerId, victim: PlayerId) {
    if attacker == victim || ctx.w.friendly(attacker, victim) {
        return;
    }
    let w = &*ctx.w;
    let broken: Vec<(u64, Party)> = w
        .treaties
        .values()
        .filter(|t| live(t) && matches!(t.kind, TreatyKind::Nap | TreatyKind::MutualDefense))
        .filter_map(|t| {
            let (a, b) = (side_a(t), side_b(t));
            if covers(w, a, attacker) && covers(w, b, victim) {
                Some((t.id, a))
            } else if covers(w, b, attacker) && covers(w, a, victim) {
                Some((t.id, b))
            } else {
                None
            }
        })
        .collect();
    if !broken.is_empty()
        && let Some(p) = ctx.w.players.get_mut(&attacker)
    {
        p.stats.partners_attacked += 1;
    }
    for (id, breaker) in broken {
        break_treaty(ctx, id, breaker);
    }
}

/// Who owns what an attack targets (for breach detection), evaluated before the attack.
pub fn attack_victim(w: &World, pid: PlayerId, a: &AttackArgs) -> Option<PlayerId> {
    let sid = w.players.get(&pid)?.sector;
    match a.target_type {
        TargetType::Ship => player::resolve(w, a.target_id.as_deref()?)
            .filter(|t| t.sector == sid)
            .map(|t| t.id),
        TargetType::Fighters => w.sector(sid)?.fighters.as_ref().map(|f| f.owner),
        TargetType::Planet => {
            let id: PlanetId = a.target_id.as_deref()?.trim().parse().ok()?;
            w.planets.get(&id).filter(|pl| pl.sector == sid).and_then(|pl| pl.owner)
        }
    }
}

/// A kill between belligerents: count it toward the war.
pub fn war_kill(w: &mut World, killer: PlayerId, victim: PlayerId) {
    let (Some(ck), Some(cv)) = (corp_of(w, killer), corp_of(w, victim)) else {
        return;
    };
    for x in w.wars.values_mut().filter(|x| x.status == WarStatus::Active) {
        if x.attacker == ck && x.defender == cv {
            x.attacker_kills += 1;
        } else if x.attacker == cv && x.defender == ck {
            x.defender_kills += 1;
        }
    }
}

/// Per-tick upkeep: open and close wars, expire treaties, offers and intents.
pub fn tick(ctx: &mut Ctx) {
    let t = ctx.w.tick;
    let opening: Vec<u64> = ctx
        .w
        .wars
        .values()
        .filter(|x| x.status == WarStatus::Declared && t >= x.starts_tick)
        .map(|x| x.id)
        .collect();
    for id in opening {
        let x = ctx.w.wars.get_mut(&id).unwrap();
        x.status = WarStatus::Active;
        let (a, d, ends) = (x.attacker, x.defender, x.ends_tick);
        let (an, dn) = (corp_name(ctx.w, a), corp_name(ctx.w, d));
        proclaim(
            ctx,
            vec![],
            format!("WAR: hostilities between {an} and {dn} are open (war #{id}) until tick {ends}."),
        );
    }
    let closing: Vec<u64> = ctx
        .w
        .wars
        .values()
        .filter(|x| x.status == WarStatus::Active && t >= x.ends_tick)
        .map(|x| x.id)
        .collect();
    for id in closing {
        let x = ctx.w.wars.get_mut(&id).unwrap();
        x.status = WarStatus::Ended;
        let score = format!("{}-{}", x.attacker_kills, x.defender_kills);
        x.outcome = Some(format!("ran its course ({score} in kills)"));
        let (a, d) = (x.attacker, x.defender);
        let (an, dn) = (corp_name(ctx.w, a), corp_name(ctx.w, d));
        proclaim(
            ctx,
            vec![],
            format!("PEACE: war #{id} between {an} and {dn} ran its course ({score} in kills)."),
        );
    }
    let ending: Vec<u64> = ctx
        .w
        .treaties
        .values()
        .filter(|x| {
            (x.status == TreatyStatus::Active && x.expires_tick.is_some_and(|e| t >= e))
                || (x.status == TreatyStatus::Ending && x.ends_tick.is_some_and(|e| t >= e))
                || (x.status == TreatyStatus::Proposed && t >= x.proposed_tick + OFFER_TTL)
        })
        .map(|x| x.id)
        .collect();
    for id in ending {
        let tr = ctx.w.treaties[&id].clone();
        refund(ctx, &tr);
        let x = ctx.w.treaties.get_mut(&id).unwrap();
        let lapsed = x.status == TreatyStatus::Proposed;
        x.status = if lapsed { TreatyStatus::Rejected } else { TreatyStatus::Ended };
        x.ends_tick = Some(t);
        if !lapsed {
            note(ctx, side_a(&tr), Fact::Kept);
            note(ctx, side_b(&tr), Fact::Kept);
            let (an, bn) = (party_name(ctx.w, side_a(&tr)), party_name(ctx.w, side_b(&tr)));
            ctx.news(format!(
                "Treaty #{id} ({} between {an} and {bn}) has ended; bonds returned.",
                kind_name(tr.kind)
            ));
        }
    }
    ctx.w.intents.retain(|_, i| i.expires_tick > t);
}

/// Append the combat events of one applied entry to the public combat log.
pub fn record_combat(w: &mut World, events: &[Event]) {
    for e in events.iter().filter(|e| COMBAT_KINDS.contains(&e.kind.as_str())) {
        let id = next_id(w);
        w.combat_reports.push(CombatReport {
            id,
            tick: e.tick,
            sector: e.sector,
            kind: e.kind.clone(),
            players: e.players.clone(),
            text: e.text.clone(),
        });
    }
    if w.combat_reports.len() > MAX_REPORTS {
        let cut = w.combat_reports.len() - MAX_REPORTS;
        w.combat_reports.drain(..cut);
    }
}

// ---------- views ----------

fn treaty_json(w: &World, t: &Treaty) -> Value {
    let kind_of = |p: Party| match p {
        Party::Corp(_) => "corporation",
        Party::Captain(_) => "captain",
    };
    let (a, b) = (side_a(t), side_b(t));
    let broken_by = t
        .broken_by
        .map(|c| corp_name(w, c))
        .or_else(|| t.broken_by_player.map(|p| w.player_name(p)));
    json!({
        "id": t.id, "kind": t.kind, "parties": [party_name(w, a), party_name(w, b)], "party_types": [kind_of(a), kind_of(b)],
        "proposer": party_name(w, a),
        "status": t.status, "bond": t.bond, "terms": crate::query::untrusted(&t.terms), "proposed_tick": t.proposed_tick,
        "signed_tick": t.signed_tick, "expires_tick": t.expires_tick, "ends_tick": t.ends_tick,
        "broken_by": broken_by,
    })
}

/// Is `pid` a party to `t`, itself or through the corporation it belongs to?
fn party_to(w: &World, t: &Treaty, pid: PlayerId) -> bool {
    covers(w, side_a(t), pid) || covers(w, side_b(t), pid)
}

fn war_json(w: &World, x: &War) -> Value {
    json!({
        "id": x.id, "attacker": corp_name(w, x.attacker), "defender": corp_name(w, x.defender), "status": x.status,
        "objective": crate::query::untrusted(&x.objective), "tribute": x.tribute, "declared_tick": x.declared_tick,
        "starts_tick": x.starts_tick, "ends_tick": x.ends_tick, "outcome": x.outcome,
        "kills": {"attacker": x.attacker_kills, "defender": x.defender_kills},
    })
}

fn intent_json(w: &World, i: &Intent) -> Value {
    json!({"player": w.player_name(i.player), "kind": i.kind, "sector": i.sector, "text": crate::query::untrusted(&i.text),
           "declared_tick": i.declared_tick, "expires_tick": i.expires_tick})
}

fn report_json(w: &World, r: &CombatReport) -> Value {
    json!({"id": r.id, "tick": r.tick, "sector": r.sector, "kind": r.kind,
           "players": r.players.iter().map(|p| w.player_name(*p)).collect::<Vec<_>>(), "text": r.text})
}

fn recent(w: &World, t: &Treaty) -> bool {
    !matches!(t.status, TreatyStatus::Ended | TreatyStatus::Rejected | TreatyStatus::Broken)
        || t.ends_tick.is_some_and(|e| w.tick < e + 1_440)
}

/// The public registry: what everyone can see. `limit` caps the combat log.
/// A captain's public record across seasons (career totals plus this season): engine-verified
/// facts about its own treaties, contracts and attacks on treaty partners. None when empty.
pub fn captain_record(p: &Player) -> Option<Value> {
    let (c, s) = (&p.career, &p.stats);
    let r = [
        ("treaties_signed", c.treaties_signed + s.treaties_signed),
        ("treaties_kept", c.treaties_kept + s.treaties_kept),
        ("treaties_broken", c.treaties_broken + s.treaties_broken),
        ("bonds_forfeited", c.bonds_forfeited + s.bonds_forfeited),
        ("contracts_completed", c.contracts_completed + s.contracts_completed),
        ("contracts_defaulted", c.contracts_defaulted + s.contracts_defaulted),
        ("partners_attacked", c.partners_attacked + s.partners_attacked),
    ];
    r.iter()
        .any(|(_, n)| *n != 0)
        .then(|| Value::Object(r.iter().map(|(k, n)| (k.to_string(), json!(n))).collect()))
}

/// A corporation's public record (its corporate treaties and toll bonus). None when empty.
pub fn corp_record(c: &Corp) -> Option<Value> {
    (!c.record.is_empty()).then(|| serde_json::to_value(&c.record).unwrap_or_default())
}

/// What a corporation holds, as counts only (where is fog of war): sectors guarded by a stack
/// a member owns or shares in, planets its members own, concessions its members hold, and its
/// territory's counts (engine 1.40).
pub fn corp_space(w: &World, c: &Corp) -> Value {
    let member = |p: &PlayerId| c.members.contains(p);
    let guarded = w
        .sectors
        .iter()
        .filter_map(|s| s.fighters.as_ref())
        .filter(|f| member(&f.owner) || f.shares.keys().any(member))
        .count();
    let planets = w.planets.values().filter(|pl| pl.owner.as_ref().is_some_and(member)).count();
    let concessions = w.concessions.values().filter(|x| x.holder.as_ref().is_some_and(member)).count();
    let t = crate::territory::of(w, c.id);
    json!({"guarded_sectors": guarded, "planets": planets, "concessions": concessions,
           "territory": crate::territory::public_counts(w, c, &t)})
}

fn reputation(w: &World) -> Value {
    let captains: Vec<Value> = w
        .players
        .values()
        .filter(|p| p.kind != PlayerKind::Npc)
        .filter_map(|p| {
            captain_record(p).map(|mut r| {
                r["name"] = json!(p.name);
                r
            })
        })
        .collect();
    let corporations: Vec<Value> = w
        .corps
        .values()
        .filter_map(|c| {
            corp_record(c).map(|mut r| {
                r["name"] = json!(c.name);
                r
            })
        })
        .collect();
    json!({"captains": captains, "corporations": corporations})
}

pub fn registry(w: &World, limit: usize) -> Value {
    json!({
        "treaties": w.treaties.values().filter(|t| recent(w, t)).map(|t| treaty_json(w, t)).collect::<Vec<_>>(),
        "wars": w.wars.values().filter(|x| x.status != WarStatus::Ended || w.tick < x.ends_tick + 2_880).map(|x| war_json(w, x)).collect::<Vec<_>>(),
        "intents": w.intents.values().map(|i| intent_json(w, i)).collect::<Vec<_>>(),
        "combat_reports": w.combat_reports.iter().rev().take(limit).map(|r| report_json(w, r)).collect::<Vec<_>>(),
        "contracts": crate::contracts::listing(w),
        "concessions": crate::frontier::concessions_json(w),
        "corporations": w.corps.values().map(|c| json!({"name": c.name, "ceo": w.player_name(c.ceo), "members": c.members.len(), "treasury": c.treasury, "space": corp_space(w, c), "government": crate::government::public_summary(w, c)})).collect::<Vec<_>>(),
        "reputation": reputation(w),
        "bounties": w.bounties.iter().map(|(t, b)| json!({"target": w.player_name(*t), "amount": b.amount})).collect::<Vec<_>>(),
    })
}

/// What a pilot's briefing shows about its own and its corporation's diplomacy.
pub fn briefing(w: &World, pid: PlayerId) -> Value {
    let treaties: Vec<Value> = w
        .treaties
        .values()
        .filter(|t| party_to(w, t, pid) && (live(t) || t.status == TreatyStatus::Proposed))
        .map(|t| treaty_json(w, t))
        .collect();
    let Some(c) = corp_of(w, pid) else {
        return json!({"treaties": treaties,
            "note": "Not in a corporation: wars are between corporations, but you may sign nap and toll_exempt treaties for yourself."});
    };
    let wars: Vec<Value> = w
        .wars
        .values()
        .filter(|x| (x.attacker == c || x.defender == c) && x.status != WarStatus::Ended)
        .map(|x| war_json(w, x))
        .collect();
    json!({"treaties": treaties, "wars": wars})
}
