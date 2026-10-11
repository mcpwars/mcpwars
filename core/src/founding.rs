//! The founding era (engine 1.38): the first `founding_days` days of every season.
//!
//! - Every captain starts the season with the founding kit (`founding_probes` ether probes, a holo
//!   scanner when `founding_holo_scanner`, `founding_genesis` Genesis torpedoes): at the Big Bang,
//!   at a fresh start, and on registering while the era lasts.
//! - Corporate charters are free while it lasts.
//! - The founding grant: the first time a corporation counts `founding_grant_members` members who
//!   have not yet counted toward a founding grant this season, the Federation pays it
//!   `founding_grant` credits into its free funds, once per corporation and per captain each season
//!   (so dissolving and refounding pays nothing more). Recorded in its ledger and the news.
//! - The briefing's `server.founding` carries the founding agenda: six steps that build the
//!   galaxy's society (corporation, government, constitution, public word, plan, and from engine
//!   1.40 a connected home), each with whether this captain has done it, and the whole galaxy's
//!   progress. After the era it is one line.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::constitution::ProposalStatus;
use crate::engine::Ctx;
use crate::government::Form;
use crate::model::*;
use crate::rules::Rules;

/// Where the founding era is explained in full.
pub const MORE: &str = "/society.txt, section Founding era";

/// Where the season stands in its founding era.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FoundingInfo {
    /// Day of the era, from 1.
    pub day: u64,
    /// Days the era lasts.
    pub days: u64,
    /// The first tick after the era.
    pub ends_tick: Tick,
}

/// The founding era, if the season is in it.
pub fn founding(w: &World) -> Option<FoundingInfo> {
    let days = w.rules.founding_days;
    if days == 0 || w.season.finished {
        return None;
    }
    let per_day = crate::player::ticks_per_day(&w.rules).max(1) as u64;
    let start = w.season.started_tick;
    let ends_tick = start + days * per_day;
    if w.tick < start || w.tick >= ends_tick {
        return None;
    }
    Some(FoundingInfo {
        day: (w.tick - start) / per_day + 1,
        days,
        ends_tick,
    })
}

/// The tick the season's founding era ends (whether or not it is still on).
fn era_end(w: &World) -> Tick {
    w.season.started_tick + w.rules.founding_days * crate::player::ticks_per_day(&w.rules).max(1) as u64
}

/// The kit in words, from the live rules ("5 ether probes, a holo scanner and 1 Genesis torpedo").
pub fn kit_text(r: &Rules) -> String {
    let mut parts = vec![];
    if r.founding_probes > 0 {
        parts.push(format!("{} ether probe{}", r.founding_probes, plural(r.founding_probes)));
    }
    if r.founding_holo_scanner {
        parts.push("a holo scanner".to_string());
    }
    if r.founding_genesis > 0 {
        parts.push(format!(
            "{} Genesis torpedo{}",
            r.founding_genesis,
            if r.founding_genesis == 1 { "" } else { "es" }
        ));
    }
    match parts.len() {
        0 => "nothing".into(),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().unwrap();
            format!("{} and {last}", parts.join(", "))
        }
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Put the founding kit on a season's starting ship (within the ship's own limits). Call before
/// the endowment is computed, so the kit is part of the start and never counts as earnings.
pub fn equip(r: &Rules, ship: &mut Ship) {
    let spec = r.ship(ship.class);
    ship.probes = (ship.probes + r.founding_probes.max(0)).min(crate::stardock::PROBES_MAX.max(ship.probes));
    ship.genesis = (ship.genesis + r.founding_genesis.max(0)).min(spec.genesis_max.max(ship.genesis));
    ship.holo_scanner |= r.founding_holo_scanner;
}

/// What a corporate charter costs now: nothing during the founding era.
pub fn charter_fee(w: &World) -> i64 {
    if founding(w).is_some() { 0 } else { w.rules.corp_charter_cost }
}

/// The notice text about the era, for the season-start and welcome notices ("" when it is off).
pub fn notice_text(w: &World) -> String {
    let Some(f) = founding(w) else { return String::new() };
    let r = &w.rules;
    format!(
        " Founding era, day {} of {}: your ship carries the founding kit ({}); corporate charters are free; a corporation that reaches {} members receives a founding grant of {} credits of free funds. New: corporate territory. A corporation's laws and toll bonus hold only in its connected territory, the member-held sectors joined to its seat; a concession there pays the corporation a {}% bonus, and a seat with a member's citadel {}+ is a depot selling fighters, shields and holds. Choose a seat with `government action=set_seat` and hold the sectors around it. Your briefing's server.founding lists what the era offers.",
        f.day,
        f.days,
        kit_text(r),
        r.founding_grant_members.max(1),
        r.founding_grant.max(0),
        r.territory_concession_bonus_bp / 100,
        r.territory_depot_citadel
    )
}

// ---------- the founding grant ----------

/// Pay the founding grant to every corporation that qualifies now. Called every tick, after a
/// member joins and at the Big Bang (after the treasuries are reset).
pub fn pay_grants(ctx: &mut Ctx) {
    if founding(ctx.w).is_none() || ctx.w.rules.founding_grant <= 0 {
        return;
    }
    let season = ctx.w.season.number;
    let need = ctx.w.rules.founding_grant_members.max(1);
    let amount = ctx.w.rules.founding_grant;
    let ids: Vec<CorpId> = ctx.w.corps.keys().copied().collect();
    for id in ids {
        let c = &ctx.w.corps[&id];
        if c.gov.founding_grant_season == season {
            continue;
        }
        // Each captain counts toward one founding grant a season.
        let fresh: Vec<PlayerId> = c
            .members
            .iter()
            .copied()
            .filter(|m| ctx.w.players.get(m).is_some_and(|p| p.founding_grant_season != season))
            .collect();
        if fresh.len() < need {
            continue;
        }
        for m in &fresh {
            ctx.player_mut(*m).founding_grant_season = season;
        }
        let c = ctx.w.corps.get_mut(&id).unwrap();
        c.gov.founding_grant_season = season;
        c.treasury += amount;
        let (name, members) = (c.name.clone(), c.members.clone());
        crate::government::record_executive(
            ctx,
            id,
            "founding_grant",
            format!("The Federation paid a founding grant of {amount} credits into the free funds."),
            SYSOP,
            amount,
        );
        for m in members {
            ctx.notify(
                m,
                "government",
                format!(
                    "{name} received a founding grant of {amount} credits in its free funds. Your government decides what to do with it (government action=grant pays it out)."
                ),
            );
        }
        let text = format!("{name} receives a founding grant of {amount} credits.");
        ctx.news(text.clone());
        ctx.event("founding_grant", None, vec![], true, text, json!({"corp": id, "amount": amount}));
    }
}

// ---------- the agenda ----------

/// What the galaxy has done this season, computed once per briefing.
struct Season<'a> {
    w: &'a World,
    /// Captains who spoke on public radio this season.
    speakers: BTreeSet<PlayerId>,
    /// Every corporation's territory now.
    territories: BTreeMap<CorpId, crate::territory::Territory>,
}

impl<'a> Season<'a> {
    fn new(w: &'a World) -> Self {
        let start = w.season.started_tick;
        let speakers = w
            .radio
            .iter()
            .rev()
            .take_while(|m| m.tick >= start)
            .filter(|m| m.channel == Channel::Public)
            .map(|m| m.from)
            .collect();
        Season {
            w,
            speakers,
            territories: crate::territory::all(w),
        }
    }

    fn corp(&self, p: &Player) -> Option<&'a Corp> {
        p.corp.and_then(|c| self.w.corps.get(&c))
    }

    /// A decision its members made this season (the server's own lines do not count).
    fn governed(&self, c: &Corp) -> bool {
        c.gov.ledger.iter().any(|d| d.season == self.w.season.number && d.by != SYSOP)
    }

    fn shaped_constitution(&self, pid: PlayerId) -> bool {
        let season = self.w.season.number;
        self.w
            .constitution
            .proposals
            .iter()
            .filter(|p| p.season == season)
            .any(|p| p.proposer == pid || p.ballots.iter().any(|b| b.player_id == pid))
    }

    /// A held seat with at least `territory_founding_min` connected sectors.
    fn home(&self, c: &Corp) -> bool {
        self.territories
            .get(&c.id)
            .is_some_and(|t| t.seat_held && t.connected.len() >= self.w.rules.territory_founding_min.max(1))
    }

    fn done(&self, p: &Player) -> [bool; 5] {
        [
            self.corp(p).is_some(),
            self.corp(p).is_some_and(|c| self.governed(c)),
            self.shaped_constitution(p.id),
            self.speakers.contains(&p.id),
            self.corp(p).is_some_and(|c| self.home(c)),
        ]
    }
}

fn captains(w: &World) -> impl Iterator<Item = &Player> {
    w.players
        .values()
        .filter(|p| p.id != SYSOP && p.npc.is_none() && p.kind != PlayerKind::Npc && !p.banned)
}

/// The briefing's `server.founding`: the agenda while the era lasts, one line after it (until the
/// season ends), nothing when the era is off.
pub fn briefing(w: &World, pid: PlayerId) -> Option<Value> {
    let p = w.players.get(&pid)?;
    let Some(f) = founding(w) else {
        if w.rules.founding_days == 0 || w.season.finished || w.tick < era_end(w) {
            return None;
        }
        return Some(json!(format!(
            "The founding era ended at tick {}. Charters cost {} credits again. More: {MORE}.",
            era_end(w),
            w.rules.corp_charter_cost
        )));
    };
    let r = &w.rules;
    let s = Season::new(w);
    let done = s.done(p);
    let corp = s.corp(p);
    let need = r.founding_grant_members.max(1);

    let corp_how = match w.corps.values().find(|c| c.invites.contains(&pid)) {
        Some(c) if corp.is_none() => format!("corp {{\"action\":\"join\",\"name\":\"{}\"}} (you are invited)", c.name),
        _ => format!(
            "corp {{\"action\":\"create\",\"name\":\"<name>\"}} (free now; the {}% tax rate needs {} members)",
            r.fed_tax_member_bp as f64 / 100.0,
            r.fed_tax_break_min_members.max(1)
        ),
    };
    let gov_how = match corp {
        None => "Do step 1 first.".to_string(),
        Some(c) if c.gov.form == Form::Autocracy && c.ceo != pid => {
            "Only your CEO decides in an autocracy: urge it on radio_broadcast {\"channel\":\"corp\"}.".to_string()
        }
        Some(c) => {
            let open = c.gov.measures.iter().find(|m| {
                m.status == crate::government::MeasureStatus::Open
                    && m.change != crate::government::MeasureKind::Election
                    && m.electorate.contains(&pid)
                    && !m.votes.contains_key(&pid)
            });
            match open {
                Some(m) => format!("government {{\"action\":\"vote\",\"measure\":{},\"vote\":true}} (or false)", m.id),
                None => "Keep the current form/tax if they suit you (unchanged settings are refused); make a meaningful decision this season, for example government {\"action\":\"enact\",\"law\":\"<kind>\",\"fine\":<n>,\"title\":\"<name>\"}, a grant, or a changed form/tax.".to_string(),
            }
        }
    };
    let awaiting: Vec<u64> = w
        .constitution
        .proposals
        .iter()
        .filter(|x| x.status == ProposalStatus::Open && x.electorate.contains(&pid) && !x.votes.contains_key(&pid))
        .map(|x| x.id)
        .collect();
    let con_how = match awaiting.first() {
        Some(id) => format!(
            "constitution {{\"action\":\"vote\",\"proposal\":{id},\"vote\":true}} (or false); awaiting your vote: {}",
            awaiting.iter().map(|i| format!("#{i}")).collect::<Vec<_>>().join(", ")
        ),
        None => "constitution {\"action\":\"propose\",\"kind\":\"add\",\"text\":\"<article>\"}".to_string(),
    };
    let steps = [
        (
            "Join or found a corporation",
            format!("A {}-credit grant at {need} members", r.founding_grant.max(0)),
            corp_how,
        ),
        (
            "Give it a government: form, tax, a law or a measure",
            "Ledger decisions decide what the corporation does with its funds and its laws".to_string(),
            gov_how,
        ),
        (
            "Shape the constitution: propose or vote",
            "The constitution is what captains vote it to be".to_string(),
            con_how,
        ),
        (
            "Speak on public radio",
            "Alliances, deals and threats start there".to_string(),
            "radio_broadcast {\"channel\":\"public\",\"message\":\"<your message>\"}".to_string(),
        ),
        (
            "Claim a connected home: set a seat and hold the sectors around it",
            format!(
                "Laws and bonuses hold only in connected territory ({}+ sectors)",
                r.territory_founding_min.max(1)
            ),
            match corp {
                None => "Do step 1 first.".to_string(),
                Some(c) if c.gov.form == Form::Autocracy && c.ceo != pid => {
                    "deploy_defenses in sectors next to your corporation's; your CEO sets the seat".to_string()
                }
                Some(_) => "deploy_defenses in adjacent sectors, then government {\"action\":\"set_seat\"} where you stand".to_string(),
            },
        ),
    ];
    let agenda: Vec<Value> = steps
        .iter()
        .zip(done)
        // A finished step needs no instructions: keep the briefing short.
        .map(|((step, why, how), done)| {
            if done {
                json!({"step": step, "done": true})
            } else {
                json!({"step": step, "why": why, "how": how, "done": false})
            }
        })
        .collect();
    let next = steps
        .iter()
        .zip(done)
        .find(|(_, d)| !*d)
        .map(|((_, _, how), _)| how.clone())
        .unwrap_or_else(|| "Agenda done.".to_string());

    // The whole galaxy's progress.
    let season = w.season.number;
    let corps = w.corps.values();
    let all: Vec<&Player> = captains(w).collect();
    let progress = json!({
        "captains": all.len(),
        "captains_done": all.iter().filter(|p| s.done(p).iter().all(|d| *d)).count(),
        "corporations_with_2_plus": corps.clone().filter(|c| c.members.len() >= 2).count(),
        "founding_grants_paid": corps.clone().filter(|c| c.gov.founding_grant_season == season).count(),
        "governments_formed": corps.clone().filter(|c| s.governed(c)).count(),
        "laws_in_force": corps.clone().map(|c| c.gov.laws.len()).sum::<usize>(),
        "connected_homes": corps.filter(|c| s.home(c)).count(),
        "articles_in_force": w.constitution.articles.len(),
        "open_proposals": w.constitution.proposals.iter().filter(|x| x.status == ProposalStatus::Open).count(),
    });
    Some(json!({
        "era": format!(
            "Founding era, day {} of {}: charters are free and the founding grant is available. The agenda lists what the era offers; what you do with it is your call.",
            f.day, f.days
        ),
        "ends_tick": f.ends_tick,
        "agenda": agenda,
        "next": next,
        "progress": progress,
        "more": MORE,
    }))
}

/// game_rules' founding block.
pub fn rules_view(w: &World) -> Value {
    let r = &w.rules;
    json!({
        "days": r.founding_days,
        "kit": kit_text(r),
        "charter": "free during the founding era",
        "grant": r.founding_grant,
        "grant_members": r.founding_grant_members.max(1),
        "now": founding(w).map(|f| json!({"day": f.day, "of": f.days, "ends_tick": f.ends_tick})),
        "rule": format!(
            "The first {} days of every season are the founding era. Every captain starts the season with {}, and so does a captain who registers during the era. Corporate charters are free. A corporation that first counts {} members who have not yet counted toward a founding grant this season receives {} credits of free funds from the Federation, once a season. The briefing's server.founding lists what the era offers. More: {MORE}.",
            r.founding_days, kit_text(r), r.founding_grant_members.max(1), r.founding_grant.max(0)
        ),
    })
}
