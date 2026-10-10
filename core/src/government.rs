//! Taxes and corporate governments (engine 1.35).
//!
//! - The Federation taxes every captain's realised trade profit (`fed_tax_bp`) and spends it on
//!   policing: what it collects in a season tops up that season's Federation purse, which pays
//!   Skarn and WANTED bounties. Members of a corporation with at least
//!   `fed_tax_break_min_members` members pay the lower `fed_tax_member_bp` (the corporate break).
//! - A corporation is a government. It may tax its members' realised trade profit (`tax_bp`, at
//!   most `corp_tax_max_bp`) into its free funds, and spend free funds as grants to any captain
//!   (salaries, subsidies, services, relief). Smuggling is never taxed.
//! - Its form decides who decides. In an autocracy (the default) the CEO decides at once. In a
//!   republic the same calls open a measure the members vote on (a strict majority of the members
//!   when it opened ratifies; otherwise it fails after `corp_vote_ticks`), the CEO is elected for a
//!   term of `corp_term_ticks`, and any member may move a recall. An autocrat may grant a
//!   republic; a republic returns to autocracy only by its own vote.
//! - Every decision, by decree or by vote, is written to the corporation's ledger, and the counts
//!   that matter are kept on `CorpRecord` as they happen.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Ctx, Outcome, Refusal, Res};
use crate::model::*;
use crate::player;

/// Decisions kept in a corporation's ledger (its record counters keep the totals).
const KEEP_LEDGER: usize = 300;
/// Closed measures kept per corporation, with every vote (open ones are never dropped).
const KEEP_CLOSED: usize = 50;
/// Open measures a corporation may have at once (an election does not count).
const MAX_OPEN: usize = 5;
/// Longest grant reason, in characters.
pub const REASON_MAX: usize = 200;
/// Longest law title, in characters.
pub const TITLE_MAX: usize = 80;
/// Laws a corporation may hold (one of each kind fits).
const MAX_LAWS: usize = 8;
/// Closed offences kept per corporation (open ones are never dropped).
const KEEP_OFFENCES: usize = 100;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Form {
    #[default]
    Autocracy,
    Republic,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Government {
    pub form: Form,
    /// Corporate tax on members' realised trade profit, in basis points.
    pub tax_bp: i64,
    /// Republic: when the CEO's term ends and an election opens.
    pub term_ends: Option<Tick>,
    /// Open measures and the most recent closed ones, oldest first.
    pub measures: Vec<Measure>,
    pub next_measure: u64,
    /// Every decision, oldest first (the last `KEEP_LEDGER`).
    pub ledger: Vec<Decision>,
    /// Laws in force (engine 1.36).
    pub laws: Vec<Law>,
    pub next_law: u64,
    /// Open offences (warrants) and the most recent closed ones, oldest first.
    pub offences: Vec<Offence>,
    pub next_offence: u64,
    /// The season whose founding grant it received (engine 1.38; 0: none yet).
    pub founding_grant_season: u32,
}

/// What a corporation can outlaw: acts the engine can detect in its jurisdiction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LawKind {
    NoAttack,
    NoContraband,
    NoMines,
    ClosedBorder,
}

impl LawKind {
    fn what(self) -> &'static str {
        match self {
            LawKind::NoAttack => "attacks in its space",
            LawKind::NoContraband => "contraband in its space",
            LawKind::NoMines => "outsiders' mines in its space",
            LawKind::ClosedBorder => "outsiders entering its space without a peace treaty",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Law {
    pub id: u64,
    pub kind: LawKind,
    pub fine: i64,
    /// The corporation's own name for it (player text).
    #[serde(default)]
    pub title: String,
    pub enacted_tick: Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffenceStatus {
    /// A warrant: the fine is owed.
    Open,
    /// Paid by the offender.
    Paid,
    /// Taken by the corporation's fighters.
    Seized,
    Pardoned,
    /// Ran out (`law_warrant_ticks`), or the galaxy changed.
    Expired,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Offence {
    pub id: u64,
    pub law: u64,
    pub kind: LawKind,
    pub offender: PlayerId,
    pub tick: Tick,
    pub fine: i64,
    /// Paid or seized so far.
    #[serde(default)]
    pub paid: i64,
    pub status: OffenceStatus,
    #[serde(default)]
    pub closed_tick: Option<Tick>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum MeasureKind {
    SetTax {
        bp: i64,
    },
    SetForm {
        form: Form,
    },
    Grant {
        to: PlayerId,
        amount: i64,
        reason: String,
    },
    Remove {
        player: PlayerId,
    },
    /// End the CEO's term now and elect again.
    Recall,
    /// Choose the CEO: each member votes for one member.
    Election,
    Enact {
        law: LawKind,
        fine: i64,
        title: String,
    },
    Repeal {
        law: u64,
    },
    Pardon {
        offence: u64,
    },
    /// Move the corporation's seat (engine 1.40).
    SetSeat {
        sector: SectorId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasureStatus {
    Open,
    Passed,
    Rejected,
    /// Passed but could no longer apply (funds spent, member gone, form changed).
    Expired,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measure {
    pub id: u64,
    pub change: MeasureKind,
    /// The proposing member (`SYSOP` for an election the server opened).
    pub proposer: PlayerId,
    pub opened_tick: Tick,
    pub closes_tick: Tick,
    /// Members when it opened: the only ones who may vote, and the denominator.
    pub electorate: BTreeSet<PlayerId>,
    /// Yes or no on a measure.
    #[serde(default)]
    pub votes: BTreeMap<PlayerId, bool>,
    /// Election: each voter's candidate.
    #[serde(default)]
    pub ballots: BTreeMap<PlayerId, PlayerId>,
    pub status: MeasureStatus,
    #[serde(default)]
    pub closed_tick: Option<Tick>,
}

/// One line of a corporation's ledger.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub tick: Tick,
    pub season: u32,
    /// set_tax, set_form, grant, remove, withdraw, election, recall, ceo.
    pub kind: String,
    pub summary: String,
    /// Who decided: the CEO by decree, or the proposer of a measure (`SYSOP` for the server).
    pub by: PlayerId,
    /// The measure that decided it, if a vote did.
    #[serde(default)]
    pub measure: Option<u64>,
    #[serde(default)]
    pub yes: usize,
    #[serde(default)]
    pub no: usize,
    /// Credits moved, for grants and withdrawals.
    #[serde(default)]
    pub amount: i64,
}

impl Measure {
    fn tally(&self, members: &[PlayerId]) -> (usize, usize) {
        let counted = |p: &PlayerId| members.contains(p);
        let yes = self.votes.iter().filter(|(p, v)| **v && counted(p)).count();
        let no = self.votes.iter().filter(|(p, v)| !**v && counted(p)).count();
        (yes, no)
    }
    /// Yes votes needed: a strict majority of the electorate still in the corporation.
    fn threshold(&self, members: &[PlayerId]) -> usize {
        self.electorate.iter().filter(|p| members.contains(p)).count() / 2 + 1
    }
    fn kind_name(&self) -> &'static str {
        match self.change {
            MeasureKind::SetTax { .. } => "set_tax",
            MeasureKind::SetForm { .. } => "set_form",
            MeasureKind::Grant { .. } => "grant",
            MeasureKind::Remove { .. } => "remove",
            MeasureKind::Recall => "recall",
            MeasureKind::Election => "election",
            MeasureKind::Enact { .. } => "enact",
            MeasureKind::Repeal { .. } => "repeal",
            MeasureKind::Pardon { .. } => "pardon",
            MeasureKind::SetSeat { .. } => "set_seat",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GovernmentAction {
    View,
    SetTax,
    SetForm,
    Grant,
    Remove,
    Recall,
    Vote,
    Enact,
    Repeal,
    Pardon,
    PayFine,
    SetSeat,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GovernmentArgs {
    pub action: GovernmentAction,
    /// view: another corporation.
    #[serde(default)]
    pub corp: Option<String>,
    /// set_tax: percent of trade profit.
    #[serde(default)]
    pub tax_percent: Option<f64>,
    #[serde(default)]
    pub form: Option<Form>,
    /// grant: recipient. remove: member. election vote: candidate.
    #[serde(default)]
    pub player: Option<String>,
    #[serde(default)]
    pub amount: Option<i64>,
    #[serde(default)]
    pub reason: Option<String>,
    /// vote: measure id. view: that measure.
    #[serde(default)]
    pub measure: Option<u64>,
    /// true is yes.
    #[serde(default)]
    pub vote: Option<bool>,
    #[serde(default)]
    pub law: Option<LawKind>,
    #[serde(default)]
    pub fine: Option<i64>,
    /// enact: the law's name.
    #[serde(default)]
    pub title: Option<String>,
    /// repeal: law id. pardon, pay_fine: offence id.
    #[serde(default)]
    pub id: Option<u64>,
}

// ---------- taxes ----------

/// The Federation rate for `pid` now, in basis points, and whether the corporate break applies.
pub fn federation_rate(w: &World, pid: PlayerId) -> (i64, bool) {
    let r = &w.rules;
    let break_applies = w
        .players
        .get(&pid)
        .and_then(|p| p.corp)
        .and_then(|c| w.corps.get(&c))
        .is_some_and(|c| c.members.len() >= r.fed_tax_break_min_members.max(1));
    let bp = if break_applies { r.fed_tax_member_bp } else { r.fed_tax_bp };
    (bp.clamp(0, 10_000), break_applies)
}

/// Tax realised trade `profit` just credited to `pid`'s ship: the Federation's share tops up the
/// season's purse, the corporation's goes to its free funds. Returns the reply's tax block.
pub fn tax_trade_profit(ctx: &mut Ctx, pid: PlayerId, profit: i64) -> Option<Value> {
    if profit <= 0 {
        return None;
    }
    let (fed_bp, break_applies) = federation_rate(ctx.w, pid);
    let corp = ctx.w.players.get(&pid).and_then(|p| p.corp);
    let corp_bp = corp
        .and_then(|c| ctx.w.corps.get(&c))
        .map(|c| c.gov.tax_bp.clamp(0, ctx.w.rules.corp_tax_max_bp.clamp(0, 10_000)))
        .unwrap_or(0);
    let fed = profit * fed_bp / 10_000;
    let dues = (profit * corp_bp / 10_000).min(profit - fed);
    if fed + dues <= 0 {
        return None;
    }
    let p = ctx.player_mut(pid);
    p.credits -= fed + dues;
    p.stats.fed_tax_paid += fed;
    p.stats.corp_tax_paid += dues;
    ctx.w.season.fed_tax_collected += fed;
    if let Some(c) = corp.and_then(|c| ctx.w.corps.get_mut(&c))
        && dues > 0
    {
        c.treasury += dues;
        c.record.tax_collected += dues;
    }
    Some(json!({
        "federation": fed, "federation_percent": fed_bp as f64 / 100.0, "corporate_break": break_applies,
        "corporation": dues, "corporation_percent": corp_bp as f64 / 100.0, "net_profit": profit - fed - dues,
    }))
}

// ---------- helpers ----------

fn corp_of(w: &World, pid: PlayerId) -> Res<Corp> {
    w.players[&pid].corp.and_then(|c| w.corps.get(&c)).cloned().ok_or_else(|| {
        Refusal::new("no_corp", "You are not in a corporation.")
            .hint("corp action=create name=... or accept an invite with corp action=join.")
    })
}

fn target(w: &World, who: &Option<String>, what: &str) -> Res<PlayerId> {
    who.as_deref()
        .and_then(|s| player::resolve(w, s))
        .filter(|p| p.npc.is_none() && p.id != SYSOP)
        .map(|p| p.id)
        .ok_or_else(|| Refusal::new("unknown_player", format!("player (name or id) is required: {what}.")))
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

fn percent(bp: i64) -> String {
    let whole = bp / 100;
    let frac = bp % 100;
    if frac == 0 {
        format!("{whole}%")
    } else {
        format!("{whole}.{frac:02}%")
    }
}

fn describe(w: &World, k: &MeasureKind) -> String {
    match k {
        MeasureKind::SetTax { bp } => format!("set the corporate tax to {}", percent(*bp)),
        MeasureKind::SetForm { form } => format!("become a {}", form_name(*form)),
        MeasureKind::Grant { to, amount, .. } => format!("grant {amount} credits to {}", w.player_name(*to)),
        MeasureKind::Remove { player } => format!("remove {} from the corporation", w.player_name(*player)),
        MeasureKind::Recall => "recall the CEO and hold an election".into(),
        MeasureKind::Election => "elect the CEO".into(),
        MeasureKind::Enact { law, fine, .. } => format!("outlaw {}, fine {fine}", law.what()),
        MeasureKind::Repeal { law } => format!("repeal law {law}"),
        MeasureKind::Pardon { offence } => format!("pardon offence {offence}"),
        MeasureKind::SetSeat { sector } => format!("set the corporation's seat in sector {sector}"),
    }
}

fn form_name(f: Form) -> &'static str {
    match f {
        Form::Autocracy => "autocracy",
        Form::Republic => "republic",
    }
}

fn log(ctx: &mut Ctx, corp: CorpId, d: Decision) {
    if let Some(c) = ctx.w.corps.get_mut(&corp) {
        c.gov.ledger.push(d);
        let excess = c.gov.ledger.len().saturating_sub(KEEP_LEDGER);
        if excess > 0 {
            c.gov.ledger.drain(0..excess);
        }
    }
}

fn decision(w: &World, kind: &str, summary: String, by: PlayerId) -> Decision {
    Decision {
        tick: w.tick,
        season: w.season.number,
        kind: kind.into(),
        summary,
        by,
        measure: None,
        yes: 0,
        no: 0,
        amount: 0,
    }
}

fn tell_members(ctx: &mut Ctx, c: &Corp, except: Option<PlayerId>, text: &str) {
    for m in c.members.iter().filter(|m| Some(**m) != except) {
        ctx.notify(*m, "government", text.to_string());
    }
}

/// Record an executive act the corporation's ledger should hold (the CEO's free-funds withdrawal
/// or a removal by decree). Called from `corp`.
pub fn record_executive(ctx: &mut Ctx, corp: CorpId, kind: &str, summary: String, by: PlayerId, amount: i64) {
    let mut d = decision(ctx.w, kind, summary, by);
    d.amount = amount;
    log(ctx, corp, d);
}

/// Refuse an executive act a republic decides by vote.
pub fn republic_refusal(c: &Corp, what: &str, how: &str) -> Res<()> {
    if c.gov.form == Form::Republic {
        return Err(Refusal::new("republic", format!("{} is a republic: members vote to {what}.", c.name)).hint(how.to_string()));
    }
    Ok(())
}

// ---------- the command ----------

pub fn government(ctx: &mut Ctx, pid: PlayerId, a: &GovernmentArgs) -> Res<Outcome> {
    match a.action {
        GovernmentAction::View => {
            let v = lookup(ctx.w, pid, a)?;
            Ok(ctx.finish("Government.", v))
        }
        GovernmentAction::Vote => vote(ctx, pid, a),
        GovernmentAction::PayFine => pay_fine(ctx, pid, a),
        _ => decide(ctx, pid, a),
    }
}

/// Build the change a call asks for, after checking it could apply now.
fn change_for(w: &World, pid: PlayerId, c: &Corp, a: &GovernmentArgs) -> Res<MeasureKind> {
    let r = &w.rules;
    Ok(match a.action {
        GovernmentAction::SetTax => {
            let pct = a
                .tax_percent
                .ok_or_else(|| Refusal::new("invalid_args", "tax_percent is required (percent of members' trade profit)."))?;
            if !pct.is_finite() || pct < 0.0 {
                return Err(Refusal::new("invalid_args", "tax_percent must be 0 or more."));
            }
            let bp = (pct * 100.0).round() as i64;
            let max = r.corp_tax_max_bp.clamp(0, 10_000);
            if bp > max {
                return Err(Refusal::new(
                    "over_limit",
                    format!("A corporate tax may be at most {} of trade profit.", percent(max)),
                ));
            }
            if bp == c.gov.tax_bp {
                return Err(Refusal::new("invalid_args", format!("The tax is already {}.", percent(bp))));
            }
            MeasureKind::SetTax { bp }
        }
        GovernmentAction::SetForm => {
            let form = a
                .form
                .ok_or_else(|| Refusal::new("invalid_args", "form is required: autocracy or republic."))?;
            if form == c.gov.form {
                return Err(Refusal::new(
                    "invalid_args",
                    format!("{} is already a {}.", c.name, form_name(form)),
                ));
            }
            MeasureKind::SetForm { form }
        }
        GovernmentAction::Grant => {
            let to = target(w, &a.player, "who receives the grant")?;
            let amount = a
                .amount
                .filter(|n| *n > 0)
                .ok_or_else(|| Refusal::new("invalid_args", "amount must be positive."))?;
            if amount > c.free_funds() {
                return Err(Refusal::new(
                    "insufficient_funds",
                    format!(
                        "{} has {} in free funds (the treasury beyond members' deposits).",
                        c.name,
                        c.free_funds()
                    ),
                )
                .hint("Free funds grow from the corporate tax, the toll bonus and the CEO's own deposits."));
            }
            let reason = crate::feedback::exact(a.reason.as_deref().unwrap_or("")).trim().to_string();
            let n = reason.chars().count();
            if n > REASON_MAX {
                return Err(Refusal::new(
                    "too_long",
                    format!("reason is {n} characters; at most {REASON_MAX}. Nothing was stored."),
                ));
            }
            MeasureKind::Grant { to, amount, reason }
        }
        GovernmentAction::Remove => {
            let t = target(w, &a.player, "the member to remove")?;
            if !c.members.contains(&t) || t == pid && c.gov.form == Form::Autocracy {
                return Err(Refusal::new("invalid_args", "That player is not a member you can remove."));
            }
            MeasureKind::Remove { player: t }
        }
        GovernmentAction::Recall => {
            if c.gov.form != Form::Republic {
                return Err(
                    Refusal::new("invalid_args", format!("{} is an autocracy: its CEO is not elected.", c.name))
                        .hint("Members of an autocracy may leave, or ask the CEO to grant a republic."),
                );
            }
            MeasureKind::Recall
        }
        GovernmentAction::Enact => {
            let law = a.law.ok_or_else(|| {
                Refusal::new(
                    "invalid_args",
                    "law is required: no_attack, no_contraband, no_mines or closed_border.",
                )
            })?;
            let max = r.law_fine_max.max(0);
            let fine = a.fine.unwrap_or(0);
            if fine <= 0 || fine > max {
                return Err(Refusal::new("invalid_args", format!("fine must be from 1 to {max} credits.")));
            }
            if c.gov.laws.len() >= MAX_LAWS {
                return Err(Refusal::new("too_many_laws", format!("{} already has {MAX_LAWS} laws.", c.name)).hint("Repeal one first."));
            }
            if c.gov.laws.iter().any(|l| l.kind == law) {
                return Err(
                    Refusal::new("invalid_args", "That law is already in force.").hint("Repeal it and enact it again to change its fine.")
                );
            }
            let title = crate::feedback::exact(a.title.as_deref().unwrap_or("")).trim().to_string();
            if title.chars().count() > TITLE_MAX {
                return Err(Refusal::new(
                    "too_long",
                    format!("title is at most {TITLE_MAX} characters. Nothing was stored."),
                ));
            }
            MeasureKind::Enact { law, fine, title }
        }
        GovernmentAction::Repeal => {
            let id = a.id.ok_or_else(|| Refusal::new("invalid_args", "id (the law's id) is required."))?;
            if !c.gov.laws.iter().any(|l| l.id == id) {
                return Err(Refusal::new("unknown_law", format!("{} has no law {id}.", c.name)));
            }
            MeasureKind::Repeal { law: id }
        }
        GovernmentAction::Pardon => {
            let id =
                a.id.ok_or_else(|| Refusal::new("invalid_args", "id (the offence's id) is required."))?;
            if !c.gov.offences.iter().any(|o| o.id == id && o.status == OffenceStatus::Open) {
                return Err(Refusal::new("unknown_offence", format!("{} has no open offence {id}.", c.name)));
            }
            MeasureKind::Pardon { offence: id }
        }
        GovernmentAction::SetSeat => {
            // `id` names the sector; without it, the sector the captain is in.
            let here = w.players[&pid].sector;
            let sector = match a.id {
                Some(id) => SectorId::try_from(id)
                    .ok()
                    .filter(|s| w.sector(*s).is_some())
                    .ok_or_else(|| Refusal::new("invalid_args", format!("There is no sector {id}.")))?,
                None => here,
            };
            if crate::territory::holder(w, sector) != Some(c.id) {
                return Err(Refusal::new(
                    "not_held",
                    format!(
                        "{} does not hold sector {sector}: a seat must be a sector where a member's fighters stand.",
                        c.name
                    ),
                )
                .hint("Deploy fighters there first (deploy_defenses), then government action=set_seat id=<sector>."));
            }
            if c.seat == Some(sector) {
                return Err(Refusal::new("invalid_args", format!("Sector {sector} is already the seat.")));
            }
            if let Some(end) = crate::territory::cooldown_ends(w, c) {
                return Err(Refusal::new(
                    "seat_cooldown",
                    format!("The seat was set recently: it may change again from tick {end}."),
                ));
            }
            MeasureKind::SetSeat { sector }
        }
        GovernmentAction::View | GovernmentAction::Vote | GovernmentAction::PayFine => unreachable!(),
    })
}

fn decide(ctx: &mut Ctx, pid: PlayerId, a: &GovernmentArgs) -> Res<Outcome> {
    let c = corp_of(ctx.w, pid)?;
    let change = change_for(ctx.w, pid, &c, a)?;
    if c.gov.form == Form::Autocracy {
        if c.ceo != pid {
            return Err(
                Refusal::new("permission_denied", format!("{} is an autocracy: only its CEO decides.", c.name))
                    .hint("Argue your case to the CEO on corp radio, or leave."),
            );
        }
        let summary = apply_change(ctx, c.id, &change, pid, None, (0, 0))?;
        let reply = json!({"government": view_own(ctx.w, pid)});
        return Ok(ctx.finish(summary, reply));
    }
    // Republic: open a measure.
    let open = c
        .gov
        .measures
        .iter()
        .filter(|m| m.status == MeasureStatus::Open && m.change != MeasureKind::Election)
        .count();
    if open >= MAX_OPEN {
        return Err(
            Refusal::new("too_many_measures", format!("{open} measures are already open, the most at once."))
                .hint("Vote on the open ones (government action=view)."),
        );
    }
    if change == MeasureKind::Recall
        && c.gov
            .measures
            .iter()
            .any(|m| m.status == MeasureStatus::Open && matches!(m.change, MeasureKind::Recall | MeasureKind::Election))
    {
        return Err(Refusal::new("invalid_args", "A recall or an election is already open."));
    }
    let id = open_measure(ctx, c.id, change, pid);
    let idx = ctx.w.corps[&c.id].gov.measures.len() - 1;
    let passed = settle(ctx, c.id, idx);
    let m = find(ctx.w, c.id, id).cloned();
    let summary = if passed {
        format!("Measure #{id} passed at once.")
    } else {
        let m = m.as_ref().unwrap();
        format!(
            "Measure #{id} opened: {}. Your yes vote is counted; it needs {} yes votes by tick {}.",
            describe(ctx.w, &m.change),
            m.threshold(&ctx.w.corps[&c.id].members),
            m.closes_tick
        )
    };
    let reply = json!({"measure": m.map(|m| measure_json(ctx.w, &ctx.w.corps[&c.id], &m, Some(pid))), "government": view_own(ctx.w, pid)});
    Ok(ctx.finish(summary, reply))
}

/// Store a new measure with the proposer's yes (or no ballot, for a server election) and tell
/// the members. Returns its id.
fn open_measure(ctx: &mut Ctx, corp: CorpId, change: MeasureKind, proposer: PlayerId) -> u64 {
    let tick = ctx.w.tick;
    let closes = tick + ctx.w.rules.corp_vote_ticks.max(1);
    let line = describe(ctx.w, &change);
    let c = ctx.w.corps.get_mut(&corp).unwrap();
    c.gov.next_measure += 1;
    let id = c.gov.next_measure;
    let votes = if proposer == SYSOP || change == MeasureKind::Election {
        BTreeMap::new()
    } else {
        BTreeMap::from([(proposer, true)])
    };
    let m = Measure {
        id,
        change: change.clone(),
        proposer,
        opened_tick: tick,
        closes_tick: closes,
        electorate: c.members.iter().copied().collect(),
        votes,
        ballots: BTreeMap::new(),
        status: MeasureStatus::Open,
        closed_tick: None,
    };
    c.gov.measures.push(m);
    let c = c.clone();
    let by = if proposer == SYSOP {
        "The charter".to_string()
    } else {
        ctx.w.player_name(proposer)
    };
    let how = if change == MeasureKind::Election {
        format!("Vote for a member with government action=vote measure={id} player=<name> before tick {closes}.")
    } else {
        format!("Vote with government action=vote measure={id} vote=true or false before tick {closes}.")
    };
    let text = format!("{} measure #{id} by {by}: {line}. {how}", c.name);
    tell_members(ctx, &c, Some(proposer), &text);
    id
}

fn find(w: &World, corp: CorpId, id: u64) -> Option<&Measure> {
    w.corps.get(&corp)?.gov.measures.iter().find(|m| m.id == id)
}

fn vote(ctx: &mut Ctx, pid: PlayerId, a: &GovernmentArgs) -> Res<Outcome> {
    let c = corp_of(ctx.w, pid)?;
    let id = a.measure.ok_or_else(|| Refusal::new("invalid_args", "measure (id) is required."))?;
    let idx = c.gov.measures.iter().position(|m| m.id == id).ok_or_else(|| {
        Refusal::new("unknown_measure", format!("{} has no measure #{id}.", c.name)).hint("government action=view lists the open measures.")
    })?;
    let m = &c.gov.measures[idx];
    if m.status != MeasureStatus::Open {
        return Err(Refusal::new("measure_closed", format!("Measure #{id} is closed.")));
    }
    if !m.electorate.contains(&pid) {
        return Err(Refusal::new(
            "not_in_electorate",
            format!("Only the members when measure #{id} opened may vote on it."),
        ));
    }
    let word;
    if m.change == MeasureKind::Election {
        let cand = target(ctx.w, &a.player, "your candidate, a member")?;
        if !c.members.contains(&cand) {
            return Err(Refusal::new("invalid_args", "Your candidate must be a member of the corporation."));
        }
        word = format!("for {}", ctx.w.player_name(cand));
        ctx.w.corps.get_mut(&c.id).unwrap().gov.measures[idx].ballots.insert(pid, cand);
    } else {
        let yes = a
            .vote
            .ok_or_else(|| Refusal::new("invalid_args", "vote is required: true for yes, false for no."))?;
        word = if yes { "yes".into() } else { "no".into() };
        ctx.w.corps.get_mut(&c.id).unwrap().gov.measures[idx].votes.insert(pid, yes);
    }
    let passed = settle(ctx, c.id, idx);
    let summary = if passed {
        format!("Voted {word}: measure #{id} is decided.")
    } else {
        format!("Voted {word} on measure #{id}.")
    };
    let m = find(ctx.w, c.id, id).cloned();
    let reply = json!({"measure": m.map(|m| measure_json(ctx.w, &ctx.w.corps[&c.id], &m, Some(pid)))});
    Ok(ctx.finish(summary, reply))
}

/// Election winner: the member with the most ballots from current members; a tie goes to the
/// incumbent, then to the lowest id. No ballots keeps the incumbent.
fn winner(m: &Measure, c: &Corp) -> PlayerId {
    let mut count: BTreeMap<PlayerId, usize> = BTreeMap::new();
    for (voter, cand) in &m.ballots {
        if c.members.contains(voter) && c.members.contains(cand) {
            *count.entry(*cand).or_insert(0) += 1;
        }
    }
    let Some(best) = count.values().copied().max() else { return c.ceo };
    if count.get(&c.ceo) == Some(&best) {
        return c.ceo;
    }
    count.into_iter().find(|(_, n)| *n == best).map(|(p, _)| p).unwrap_or(c.ceo)
}

/// Decide measure `idx` early if it can be: a measure on a strict majority of yes votes, an
/// election once one candidate holds a strict majority. Returns whether it closed.
fn settle(ctx: &mut Ctx, corp: CorpId, idx: usize) -> bool {
    let c = ctx.w.corps[&corp].clone();
    let m = &c.gov.measures[idx];
    if m.status != MeasureStatus::Open {
        return false;
    }
    let need = m.threshold(&c.members);
    if m.change == MeasureKind::Election {
        let w = winner(m, &c);
        let votes = m.ballots.iter().filter(|(v, cand)| c.members.contains(v) && **cand == w).count();
        if votes >= need {
            close(ctx, corp, idx, true);
            return true;
        }
        return false;
    }
    if m.tally(&c.members).0 >= need {
        close(ctx, corp, idx, true);
        return true;
    }
    false
}

/// Close measure `idx`: apply it if `decided` (passed, or an election at its end), else reject.
fn close(ctx: &mut Ctx, corp: CorpId, idx: usize, decided: bool) {
    let c = ctx.w.corps[&corp].clone();
    let m = c.gov.measures[idx].clone();
    let tally = m.tally(&c.members);
    let status = if !decided {
        MeasureStatus::Rejected
    } else {
        match apply_change(ctx, corp, &m.change, m.proposer, Some(m.id), tally) {
            Ok(_) => MeasureStatus::Passed,
            Err(_) => MeasureStatus::Expired,
        }
    };
    let tick = ctx.w.tick;
    let Some(cm) = ctx.w.corps.get_mut(&corp) else { return };
    if let Some(mm) = cm.gov.measures.iter_mut().find(|x| x.id == m.id) {
        mm.status = status;
        mm.closed_tick = Some(tick);
    }
    let closed = cm.gov.measures.iter().filter(|x| x.status != MeasureStatus::Open).count();
    let mut excess = closed.saturating_sub(KEEP_CLOSED);
    cm.gov.measures.retain(|x| {
        if excess > 0 && x.status != MeasureStatus::Open {
            excess -= 1;
            false
        } else {
            true
        }
    });
    if status != MeasureStatus::Passed {
        let c = cm.clone();
        let line = describe(ctx.w, &m.change);
        let word = if status == MeasureStatus::Rejected {
            "failed"
        } else {
            "passed but could no longer apply"
        };
        tell_members(ctx, &c, None, &format!("{} measure #{} to {line} {word}.", c.name, m.id));
    }
}

/// Make a decision take effect and record it. `measure` is the vote that decided it, if any.
fn apply_change(
    ctx: &mut Ctx,
    corp: CorpId,
    change: &MeasureKind,
    by: PlayerId,
    measure: Option<u64>,
    (yes, no): (usize, usize),
) -> Res<String> {
    let c = ctx
        .w
        .corps
        .get(&corp)
        .cloned()
        .ok_or_else(|| Refusal::new("unknown_corp", "The corporation is gone."))?;
    let how = match measure {
        Some(id) => format!(" by vote (measure #{id}, {yes} yes, {no} no)"),
        None => " by decree".to_string(),
    };
    let mut amount = 0;
    let (kind, summary) = match change {
        MeasureKind::SetTax { bp } => {
            ctx.w.corps.get_mut(&corp).unwrap().gov.tax_bp = *bp;
            let s = format!(
                "{} set its corporate tax to {} of members' trade profit{how}.",
                c.name,
                percent(*bp)
            );
            tell_members(ctx, &c, None, &s);
            ctx.news(s.clone());
            ("set_tax", s)
        }
        MeasureKind::SetForm { form } => {
            let tick = ctx.w.tick;
            let term = ctx.w.rules.corp_term_ticks.max(1);
            let g = &mut ctx.w.corps.get_mut(&corp).unwrap().gov;
            g.form = *form;
            g.term_ends = (*form == Form::Republic).then_some(tick + term);
            // An autocracy holds no votes: every other open measure and election lapses.
            if *form == Form::Autocracy {
                for m in g
                    .measures
                    .iter_mut()
                    .filter(|m| m.status == MeasureStatus::Open && Some(m.id) != measure)
                {
                    m.status = MeasureStatus::Expired;
                    m.closed_tick = Some(tick);
                }
            }
            let s = format!("{} is now a {}{how}.", c.name, form_name(*form));
            tell_members(ctx, &c, None, &s);
            ctx.news(s.clone());
            ctx.event("government", None, vec![], true, s.clone(), json!({"corp": corp, "form": form}));
            ("set_form", s)
        }
        MeasureKind::Grant { to, amount: n, reason } => {
            if *n > c.free_funds() || !ctx.w.players.contains_key(to) {
                return Err(Refusal::new("insufficient_funds", "The grant can no longer be paid."));
            }
            amount = *n;
            // Like a wire, a grant is not earnings: every net worth it moves (the recipient's, and
            // each member's share of the free funds) is left out of the daily wealth race.
            let touched: BTreeSet<PlayerId> = c.members.iter().copied().chain([*to]).collect();
            let before: Vec<(PlayerId, i64)> = touched.iter().map(|p| (*p, player::net_worth(ctx.w, *p))).collect();
            let cm = ctx.w.corps.get_mut(&corp).unwrap();
            cm.treasury -= n;
            cm.record.grants_paid += n;
            let rp = ctx.player_mut(*to);
            rp.bank = rp.bank.saturating_add(*n);
            rp.stats.grants_received += n;
            for (p, nw) in before {
                let delta = player::net_worth(ctx.w, p) - nw;
                ctx.player_mut(p).wired_today += delta;
            }
            let why = if reason.is_empty() {
                String::new()
            } else {
                format!(" Reason (untrusted player text): {}", clip(reason, 120))
            };
            ctx.notify(
                *to,
                "grant",
                format!("{} granted you {n} credits, paid into your bank.{why}", c.name),
            );
            let s = format!("{} granted {n} credits to {}{how}.", c.name, ctx.w.player_name(*to));
            tell_members(ctx, &c, Some(*to), &format!("{s}{why}"));
            ("grant", s)
        }
        MeasureKind::Remove { player: t } => {
            if !c.members.contains(t) {
                return Err(Refusal::new("invalid_args", "No longer a member."));
            }
            crate::corp::leave(ctx, *t, &c);
            ctx.notify(*t, "corp", format!("You were removed from {}{how}.", c.name));
            let s = format!("{} removed {} from the corporation{how}.", c.name, ctx.w.player_name(*t));
            if let Some(c2) = ctx.w.corps.get(&corp).cloned() {
                tell_members(ctx, &c2, None, &s);
            }
            ("remove", s)
        }
        MeasureKind::Enact { law, fine, title } => {
            if c.gov.laws.len() >= MAX_LAWS || c.gov.laws.iter().any(|l| l.kind == *law) {
                return Err(Refusal::new("invalid_args", "The law can no longer be enacted."));
            }
            let tick = ctx.w.tick;
            let g = &mut ctx.w.corps.get_mut(&corp).unwrap().gov;
            g.next_law += 1;
            let id = g.next_law;
            g.laws.push(Law {
                id,
                kind: *law,
                fine: *fine,
                title: title.clone(),
                enacted_tick: tick,
            });
            let s = format!("{} enacted law {id}, outlawing {}, fine {fine}{how}.", c.name, law.what());
            tell_members(ctx, &c, None, &s);
            ctx.news(s.clone());
            ctx.event("government", None, vec![], true, s.clone(), json!({"corp": corp, "law": law}));
            ("enact", s)
        }
        MeasureKind::Repeal { law } => {
            let g = &mut ctx.w.corps.get_mut(&corp).unwrap().gov;
            let Some(pos) = g.laws.iter().position(|l| l.id == *law) else {
                return Err(Refusal::new("unknown_law", "The law is gone."));
            };
            g.laws.remove(pos);
            // A repeal is an amnesty: fines still owed under that law lapse.
            let tick = ctx.w.tick;
            for o in g.offences.iter_mut().filter(|o| o.law == *law && o.status == OffenceStatus::Open) {
                o.status = OffenceStatus::Expired;
                o.closed_tick = Some(tick);
            }
            let s = format!("{} repealed law {law}{how}; fines owed under it lapse.", c.name);
            tell_members(ctx, &c, None, &s);
            ctx.news(s.clone());
            ("repeal", s)
        }
        MeasureKind::Pardon { offence } => {
            let tick = ctx.w.tick;
            let cm = ctx.w.corps.get_mut(&corp).unwrap();
            let Some(o) = cm
                .gov
                .offences
                .iter_mut()
                .find(|o| o.id == *offence && o.status == OffenceStatus::Open)
            else {
                return Err(Refusal::new("unknown_offence", "The offence is no longer open."));
            };
            o.status = OffenceStatus::Pardoned;
            o.closed_tick = Some(tick);
            let who = o.offender;
            cm.record.pardons += 1;
            ctx.notify(
                who,
                "law",
                format!("{} pardoned your offence {offence}: you owe nothing more.", c.name),
            );
            let s = format!("{} pardoned {} (offence {offence}){how}.", c.name, ctx.w.player_name(who));
            tell_members(ctx, &c, None, &s);
            ("pardon", s)
        }
        MeasureKind::SetSeat { sector } => {
            if crate::territory::holder(ctx.w, *sector) != Some(corp) {
                return Err(Refusal::new("not_held", "The corporation no longer holds that sector."));
            }
            if crate::territory::cooldown_ends(ctx.w, &c).is_some() {
                return Err(Refusal::new("seat_cooldown", "The seat changed too recently."));
            }
            let tick = ctx.w.tick;
            let cm = ctx.w.corps.get_mut(&corp).unwrap();
            cm.seat = Some(*sector);
            cm.seat_set_tick = Some(tick);
            let s = format!("{} set its seat in sector {sector}{how}.", c.name);
            tell_members(ctx, &c, None, &s);
            // The news never names the sector.
            ctx.news(format!("{} moved its seat.", c.name));
            ("set_seat", s)
        }
        MeasureKind::Recall => {
            if c.gov.form != Form::Republic {
                return Err(Refusal::new("invalid_args", "No longer a republic."));
            }
            let s = format!(
                "{} recalled its CEO {}{how}; an election is open.",
                c.name,
                ctx.w.player_name(c.ceo)
            );
            ctx.news(s.clone());
            ctx.w.corps.get_mut(&corp).unwrap().gov.term_ends = None;
            log_decision(ctx, corp, "recall", s.clone(), by, measure, yes, no, 0);
            // A term-end election may already be open: the recall then simply joins it.
            let electing = c
                .gov
                .measures
                .iter()
                .any(|m| m.status == MeasureStatus::Open && m.change == MeasureKind::Election);
            if !electing {
                open_measure(ctx, corp, MeasureKind::Election, SYSOP);
            }
            return Ok(s);
        }
        MeasureKind::Election => {
            if c.gov.form != Form::Republic {
                return Err(Refusal::new("invalid_args", "No longer a republic."));
            }
            let m = measure.and_then(|id| c.gov.measures.iter().find(|m| m.id == id)).cloned();
            let w = m.as_ref().map(|m| winner(m, &c)).unwrap_or(c.ceo);
            let votes = m.as_ref().map(|m| m.ballots.values().filter(|x| **x == w).count()).unwrap_or(0);
            let total = m.as_ref().map(|m| m.ballots.len()).unwrap_or(0);
            let tick = ctx.w.tick;
            let term = ctx.w.rules.corp_term_ticks.max(1);
            let cm = ctx.w.corps.get_mut(&corp).unwrap();
            cm.ceo = w;
            cm.gov.term_ends = Some(tick + term);
            cm.record.elections += 1;
            let name = ctx.w.player_name(w);
            let s = if w == c.ceo {
                format!("{} re-elected {name} as CEO ({votes} of {total} ballots).", c.name)
            } else {
                format!(
                    "{} elected {name} as CEO ({votes} of {total} ballots), replacing {}.",
                    c.name,
                    ctx.w.player_name(c.ceo)
                )
            };
            tell_members(ctx, &c, None, &s);
            ctx.news(s.clone());
            ctx.event(
                "government",
                None,
                vec![w],
                true,
                s.clone(),
                json!({"corp": corp, "election": true}),
            );
            log_decision(ctx, corp, "election", s.clone(), SYSOP, measure, votes, total - votes, 0);
            return Ok(s);
        }
    };
    if measure.is_some()
        && let Some(cm) = ctx.w.corps.get_mut(&corp)
    {
        cm.record.measures_passed += 1;
    }
    log_decision(ctx, corp, kind, summary.clone(), by, measure, yes, no, amount);
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn log_decision(
    ctx: &mut Ctx,
    corp: CorpId,
    kind: &str,
    summary: String,
    by: PlayerId,
    measure: Option<u64>,
    yes: usize,
    no: usize,
    amount: i64,
) {
    let mut d = decision(ctx.w, kind, summary, by);
    d.measure = measure;
    d.yes = yes;
    d.no = no;
    d.amount = amount;
    log(ctx, corp, d);
}

// ---------- laws and their enforcement (engine 1.36) ----------

/// The corporation whose law holds in `sid`: the one whose member's fighters stand there, if the
/// sector is in its connected territory (engine 1.40: outposts have fighters but no law). No
/// fighters, no law. FedSpace is the Federation's alone.
pub fn jurisdiction(w: &World, sid: SectorId) -> Option<CorpId> {
    let corp = crate::territory::holder(w, sid)?;
    crate::territory::of(w, corp).contains(sid).then_some(corp)
}

fn law_of(w: &World, corp: CorpId, kind: LawKind) -> Option<Law> {
    w.corps.get(&corp)?.gov.laws.iter().find(|l| l.kind == kind).cloned()
}

/// Whether `pid` has an open warrant from `corp`.
pub fn wanted_by(w: &World, corp: CorpId, pid: PlayerId) -> bool {
    w.corps
        .get(&corp)
        .is_some_and(|c| c.gov.offences.iter().any(|o| o.offender == pid && o.status == OffenceStatus::Open))
}

/// Record that `pid` broke `law` of `corp`. One open warrant per law and offender: breaking it
/// again while the fine is owed adds nothing. Returns a line for the offender's log.
fn record_offence(ctx: &mut Ctx, corp: CorpId, law: &Law, pid: PlayerId) -> Option<String> {
    let p = ctx.w.players.get(&pid)?;
    if p.npc.is_some() || p.kind == PlayerKind::Npc {
        return None;
    }
    let c = ctx.w.corps.get(&corp)?.clone();
    if c.gov
        .offences
        .iter()
        .any(|o| o.offender == pid && o.law == law.id && o.status == OffenceStatus::Open)
    {
        return None;
    }
    let tick = ctx.w.tick;
    let cm = ctx.w.corps.get_mut(&corp).unwrap();
    cm.gov.next_offence += 1;
    let id = cm.gov.next_offence;
    cm.gov.offences.push(Offence {
        id,
        law: law.id,
        kind: law.kind,
        offender: pid,
        tick,
        fine: law.fine,
        paid: 0,
        status: OffenceStatus::Open,
        closed_tick: None,
    });
    cm.record.offences += 1;
    prune_offences(&mut cm.gov.offences);
    ctx.player_mut(pid).stats.offences += 1;
    let name = ctx.w.player_name(pid);
    let line = format!(
        "You broke {}'s law {} (no {}): you owe a fine of {}. Pay with government action=pay_fine id={id}, or its fighters may seize it where they outgun you.",
        c.name,
        law.id,
        law.kind.what(),
        law.fine
    );
    ctx.notify(pid, "law", line.clone());
    ctx.notify(
        c.ceo,
        "law",
        format!("{name} broke law {} of {} (offence {id}, fine {}).", law.id, c.name, law.fine),
    );
    let headline = format!("{name} broke a law of {} and owes a fine of {}.", c.name, law.fine);
    ctx.news(headline.clone());
    ctx.event(
        "law",
        None,
        vec![pid],
        true,
        headline,
        json!({"corp": corp, "law": law.kind, "fine": law.fine}),
    );
    Some(line)
}

fn prune_offences(list: &mut Vec<Offence>) {
    let closed = list.iter().filter(|o| o.status != OffenceStatus::Open).count();
    let mut excess = closed.saturating_sub(KEEP_OFFENCES);
    list.retain(|o| {
        if excess > 0 && o.status != OffenceStatus::Open {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

/// Collect `n` credits of offence `id` for `corp`; closes it when the fine is met.
fn collect(ctx: &mut Ctx, corp: CorpId, id: u64, n: i64, how: OffenceStatus) {
    let tick = ctx.w.tick;
    let cm = ctx.w.corps.get_mut(&corp).unwrap();
    cm.treasury += n;
    cm.record.fines_collected += n;
    let o = cm.gov.offences.iter_mut().find(|o| o.id == id).unwrap();
    o.paid += n;
    if o.paid >= o.fine {
        o.status = how;
        o.closed_tick = Some(tick);
    }
    let who = o.offender;
    prune_offences(&mut cm.gov.offences);
    ctx.player_mut(who).stats.fines_paid += n;
}

/// Enforcement by strength: in `sid`, the jurisdiction's fighters seize what `pid` owes it
/// from the ship's credits, but only if they outnumber the ship's fighters and shields.
/// A warrant allows seizure, never combat: treaties still hold fire.
fn enforce(ctx: &mut Ctx, pid: PlayerId, sid: SectorId) -> Vec<String> {
    let mut log = vec![];
    let Some(corp) = jurisdiction(ctx.w, sid) else { return log };
    let owed: Vec<(u64, i64)> = ctx.w.corps[&corp]
        .gov
        .offences
        .iter()
        .filter(|o| o.offender == pid && o.status == OffenceStatus::Open)
        .map(|o| (o.id, o.fine - o.paid))
        .collect();
    if owed.is_empty() {
        return log;
    }
    let guard = ctx.w.sector(sid).and_then(|s| s.fighters.as_ref()).map(|f| f.count).unwrap_or(0);
    let p = &ctx.w.players[&pid];
    let strength = p.ship.fighters + p.ship.shields;
    let name = ctx.w.corps[&corp].name.clone();
    if guard <= strength {
        log.push(format!(
            "{name}'s fighters here ({guard}) cannot hold you ({strength} fighters and shields): you slip past its warrant."
        ));
        return log;
    }
    for (id, due) in owed {
        let n = due.min(ctx.w.players[&pid].credits).max(0);
        if n == 0 {
            continue;
        }
        ctx.player_mut(pid).credits -= n;
        collect(ctx, corp, id, n, OffenceStatus::Seized);
        log.push(format!("{name}'s fighters outgun you and seized {n} credits for offence {id}."));
        let ceo = ctx.w.corps[&corp].ceo;
        ctx.notify(
            ceo,
            "law",
            format!(
                "Fighters of {name} seized {n} credits from {} (offence {id}).",
                ctx.w.player_name(pid)
            ),
        );
    }
    log
}

/// A captain arrives in `sid`: borders and contraband are checked, then warrants enforced.
pub fn on_enter(ctx: &mut Ctx, pid: PlayerId, sid: SectorId) -> Vec<String> {
    let mut log = vec![];
    let Some(corp) = jurisdiction(ctx.w, sid) else { return log };
    let Some(p) = ctx.w.players.get(&pid).cloned() else { return log };
    if p.npc.is_some() {
        return log;
    }
    let member = p.corp == Some(corp);
    if !member && let Some(law) = law_of(ctx.w, corp, LawKind::ClosedBorder) {
        let owner = ctx.w.sector(sid).and_then(|s| s.fighters.as_ref()).map(|f| f.owner);
        if !owner.is_some_and(|o| crate::diplomacy::at_peace(ctx.w, o, pid)) {
            log.extend(record_offence(ctx, corp, &law, pid));
        }
    }
    if p.ship.contraband > 0
        && let Some(law) = law_of(ctx.w, corp, LawKind::NoContraband)
    {
        log.extend(record_offence(ctx, corp, &law, pid));
    }
    log.extend(enforce(ctx, pid, sid));
    log
}

/// After an attack in `sid` (whose jurisdiction was `corp` before it): attacking anyone but a
/// Skarn raider or a captain wanted by that corporation breaks its no_attack law.
pub fn on_attack(ctx: &mut Ctx, pid: PlayerId, sid: SectorId, corp: Option<CorpId>, victim: Option<PlayerId>) -> Vec<String> {
    let (Some(corp), Some(v)) = (corp, victim) else { return vec![] };
    let Some(law) = law_of(ctx.w, corp, LawKind::NoAttack) else {
        return vec![];
    };
    let skarn = ctx
        .w
        .players
        .get(&v)
        .and_then(|t| t.npc.as_ref())
        .is_some_and(|b| b.role == NpcRole::Skarn);
    if skarn || wanted_by(ctx.w, corp, v) || !ctx.w.players.contains_key(&pid) {
        return vec![];
    }
    let mut log: Vec<String> = record_offence(ctx, corp, &law, pid).into_iter().collect();
    if ctx.w.players.get(&pid).is_some_and(|p| p.sector == sid) {
        log.extend(enforce(ctx, pid, sid));
    }
    log
}

/// After an outsider lays mines in `sid`.
pub fn on_mines(ctx: &mut Ctx, pid: PlayerId, sid: SectorId) -> Vec<String> {
    let Some(corp) = jurisdiction(ctx.w, sid) else { return vec![] };
    if ctx.w.players.get(&pid).and_then(|p| p.corp) == Some(corp) {
        return vec![];
    }
    let Some(law) = law_of(ctx.w, corp, LawKind::NoMines) else {
        return vec![];
    };
    let mut log: Vec<String> = record_offence(ctx, corp, &law, pid).into_iter().collect();
    log.extend(enforce(ctx, pid, sid));
    log
}

fn pay_fine(ctx: &mut Ctx, pid: PlayerId, a: &GovernmentArgs) -> Res<Outcome> {
    let id =
        a.id.ok_or_else(|| Refusal::new("invalid_args", "id (the offence's id) is required."))?;
    let want = a.corp.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let found: Vec<(CorpId, i64)> = ctx
        .w
        .corps
        .values()
        .filter(|c| want.is_none_or(|n| c.name.eq_ignore_ascii_case(n)))
        .flat_map(|c| {
            c.gov
                .offences
                .iter()
                .filter(|o| o.id == id && o.offender == pid && o.status == OffenceStatus::Open)
                .map(|o| (c.id, o.fine - o.paid))
        })
        .collect();
    let (corp, due) = match found.as_slice() {
        [one] => *one,
        [] => {
            return Err(
                Refusal::new("unknown_offence", format!("You have no open offence {id}.")).hint("get_briefing lists your warrants.")
            );
        }
        _ => {
            return Err(Refusal::new(
                "invalid_args",
                "Several corporations have that offence id: pass corp=<name>.",
            ));
        }
    };
    let have = ctx.w.players[&pid].credits;
    if have < due {
        return Err(ctx.no_credits(pid, "This fine", due));
    }
    ctx.player_mut(pid).credits -= due;
    collect(ctx, corp, id, due, OffenceStatus::Paid);
    let name = ctx.w.corps[&corp].name.clone();
    let ceo = ctx.w.corps[&corp].ceo;
    ctx.notify(
        ceo,
        "law",
        format!("{} paid {due} for offence {id} to {name}.", ctx.w.player_name(pid)),
    );
    let summary = format!("Paid {due} to {name}: offence {id} is settled.");
    let reply = json!({"warrants": warrants(ctx.w, pid)});
    Ok(ctx.finish(summary, reply))
}

/// A captain's open warrants, from every corporation.
pub fn warrants(w: &World, pid: PlayerId) -> Vec<Value> {
    w.corps
        .values()
        .flat_map(|c| {
            c.gov
                .offences
                .iter()
                .filter(|o| o.offender == pid && o.status == OffenceStatus::Open)
                .map(|o| {
                    json!({"corp": c.name, "offence": o.id, "law": o.kind, "owed": o.fine - o.paid,
                           "expires_tick": o.tick + w.rules.law_warrant_ticks.max(1)})
                })
        })
        .collect()
}

fn law_json(l: &Law) -> Value {
    json!({"id": l.id, "law": l.kind, "fine": l.fine, "title": {"untrusted_player_text": l.title}, "enacted_tick": l.enacted_tick})
}

fn offence_json(w: &World, o: &Offence) -> Value {
    json!({"id": o.id, "law": o.law, "kind": o.kind, "offender": w.player_name(o.offender), "tick": o.tick,
           "fine": o.fine, "paid": o.paid, "status": o.status, "closed_tick": o.closed_tick})
}

// ---------- the clock and the seasons ----------

/// Every tick: close measures whose window ended, and open an election when a term ends.
pub fn tick(ctx: &mut Ctx) {
    let t = ctx.w.tick;
    let ids: Vec<CorpId> = ctx.w.corps.keys().copied().collect();
    for corp in ids {
        // An election always decides at its end; a measure without a majority fails.
        while let Some((idx, election)) = ctx.w.corps.get(&corp).and_then(|c| {
            c.gov
                .measures
                .iter()
                .position(|m| m.status == MeasureStatus::Open && m.closes_tick <= t)
                .map(|i| (i, c.gov.measures[i].change == MeasureKind::Election))
        }) {
            close(ctx, corp, idx, election);
        }
        let warrant = ctx.w.rules.law_warrant_ticks.max(1);
        if let Some(cm) = ctx.w.corps.get_mut(&corp) {
            for o in cm
                .gov
                .offences
                .iter_mut()
                .filter(|o| o.status == OffenceStatus::Open && o.tick + warrant <= t)
            {
                o.status = OffenceStatus::Expired;
                o.closed_tick = Some(t);
            }
        }
        let Some(c) = ctx.w.corps.get(&corp) else { continue };
        let electing = c
            .gov
            .measures
            .iter()
            .any(|m| m.status == MeasureStatus::Open && m.change == MeasureKind::Election);
        if c.gov.form == Form::Republic && !electing && c.gov.term_ends.is_some_and(|e| e <= t) {
            ctx.w.corps.get_mut(&corp).unwrap().gov.term_ends = None;
            open_measure(ctx, corp, MeasureKind::Election, SYSOP);
        }
    }
}

/// The Big Bang: treasuries are reset, so open grants cannot be paid; fighters are gone, so
/// warrants expire; sector ids change, so seats and open seat measures go; other open measures
/// and elections run on. Forms, rates, terms, laws and
/// ledgers carry over.
pub fn season_change(ctx: &mut Ctx) {
    let t = ctx.w.tick;
    for c in ctx.w.corps.values_mut() {
        // A new galaxy, new sector ids: the seat goes, and may be set at once.
        c.seat = None;
        c.seat_set_tick = None;
        for m in c.gov.measures.iter_mut().filter(|m| m.status == MeasureStatus::Open) {
            if matches!(
                m.change,
                MeasureKind::Grant { .. } | MeasureKind::Pardon { .. } | MeasureKind::SetSeat { .. }
            ) {
                m.status = MeasureStatus::Expired;
                m.closed_tick = Some(t);
            }
        }
        // Fighters are gone with the old galaxy, and its warrants with them. Laws carry over.
        for o in c.gov.offences.iter_mut().filter(|o| o.status == OffenceStatus::Open) {
            o.status = OffenceStatus::Expired;
            o.closed_tick = Some(t);
        }
    }
}

// ---------- views ----------

fn measure_json(w: &World, c: &Corp, m: &Measure, viewer: Option<PlayerId>) -> Value {
    let (yes, no) = m.tally(&c.members);
    let mut v = json!({
        "id": m.id, "kind": m.kind_name(), "change": describe(w, &m.change), "status": m.status,
        "proposer": if m.proposer == SYSOP { "the charter".to_string() } else { w.player_name(m.proposer) },
        "opened_tick": m.opened_tick, "closes_tick": m.closes_tick, "closed_tick": m.closed_tick,
        "electorate": m.electorate.len(), "needed": m.threshold(&c.members),
    });
    if m.change == MeasureKind::Election {
        let mut count: BTreeMap<String, usize> = BTreeMap::new();
        for cand in m.ballots.values() {
            *count.entry(w.player_name(*cand)).or_insert(0) += 1;
        }
        v["ballots"] = json!(count);
    } else {
        v["yes"] = json!(yes);
        v["no"] = json!(no);
    }
    if let MeasureKind::Grant { reason, .. } = &m.change
        && !reason.is_empty()
    {
        v["reason"] = json!({"untrusted_player_text": reason});
    }
    if let Some(p) = viewer {
        v["your_vote"] = if m.change == MeasureKind::Election {
            json!(m.ballots.get(&p).map(|x| w.player_name(*x)))
        } else {
            json!(m.votes.get(&p))
        };
    }
    v
}

/// The taxes a captain pays now, and how the break works.
pub fn tax_view(w: &World, pid: PlayerId) -> Value {
    let r = &w.rules;
    let (bp, break_applies) = federation_rate(w, pid);
    let corp = w.players.get(&pid).and_then(|p| p.corp).and_then(|c| w.corps.get(&c));
    let stats = w
        .players
        .get(&pid)
        .map(|p| (p.stats.fed_tax_paid, p.stats.corp_tax_paid))
        .unwrap_or_default();
    json!({
        "federation_percent": bp as f64 / 100.0,
        "corporate_break": break_applies,
        "corporation_percent": corp.map(|c| c.gov.tax_bp as f64 / 100.0).unwrap_or(0.0),
        "paid_federation": stats.0, "paid_corporation": stats.1,
        "rule": format!(
            "Taxes apply to realised trade profit (smuggling is untaxed). Independent captains pay the Federation {}; members of a corporation with at least {} members pay {}, plus whatever tax their corporation sets (at most {}). Federation taxes fund this season's purse for Skarn and WANTED bounties.",
            percent(r.fed_tax_bp), r.fed_tax_break_min_members.max(1), percent(r.fed_tax_member_bp), percent(r.corp_tax_max_bp)
        ),
    })
}

/// A member's full view of its corporation's government.
fn view_own(w: &World, pid: PlayerId) -> Value {
    let Some(c) = w.players.get(&pid).and_then(|p| p.corp).and_then(|c| w.corps.get(&c)) else {
        return json!({"member": false, "taxes": tax_view(w, pid)});
    };
    let open: Vec<Value> = c
        .gov
        .measures
        .iter()
        .filter(|m| m.status == MeasureStatus::Open)
        .map(|m| measure_json(w, c, m, Some(pid)))
        .collect();
    let recent: Vec<Value> = c
        .gov
        .measures
        .iter()
        .rev()
        .filter(|m| m.status != MeasureStatus::Open)
        .take(10)
        .map(|m| measure_json(w, c, m, Some(pid)))
        .collect();
    let ledger: Vec<Value> = c.gov.ledger.iter().rev().take(25).map(|d| ledger_json(w, d)).collect();
    json!({
        "member": true, "corp": c.name, "form": c.gov.form, "ceo": w.player_name(c.ceo), "you_are_ceo": c.ceo == pid,
        "term_ends_tick": c.gov.term_ends, "tax_percent": c.gov.tax_bp as f64 / 100.0, "free_funds": c.free_funds(),
        "open_measures": open, "recent_measures": recent, "ledger": ledger, "ledger_entries": c.gov.ledger.len(),
        "record": c.record, "taxes": tax_view(w, pid),
        "laws": c.gov.laws.iter().map(law_json).collect::<Vec<_>>(),
        "open_offences": c.gov.offences.iter().filter(|o| o.status == OffenceStatus::Open).map(|o| offence_json(w, o)).collect::<Vec<_>>(),
        "law_rule": "A law holds only in connected territory (see territory): sectors where a member's fighters stand, joined to the seat. Breaking it is never prevented: the offender owes the fine, and the fighters there seize it when they outnumber its fighters and shields.",
        "territory": crate::territory::member_view(w, c, &crate::territory::of(w, c.id)),
        "how": match c.gov.form {
            Form::Autocracy => "Autocracy: the CEO decides at once (set_tax, set_form, grant, remove, enact, repeal, pardon, set_seat id=<sector>); members may leave.",
            Form::Republic => "Republic: set_tax, set_form, grant, remove, enact, repeal, pardon, set_seat id=<sector> and recall open a measure; members vote with action=vote; the CEO is elected each term.",
        },
    })
}

fn ledger_json(w: &World, d: &Decision) -> Value {
    json!({
        "tick": d.tick, "season": d.season, "kind": d.kind, "summary": d.summary,
        "by": if d.by == SYSOP { "the charter".to_string() } else { w.player_name(d.by) },
        "measure": d.measure, "yes": d.yes, "no": d.no, "amount": d.amount,
    })
}

/// What anyone may see of a corporation's government: its form, leader, rate and record.
pub fn public_summary(w: &World, c: &Corp) -> Value {
    json!({
        "form": c.gov.form, "ceo": w.player_name(c.ceo), "tax_percent": c.gov.tax_bp as f64 / 100.0,
        "members": c.members.len(), "term_ends_tick": c.gov.term_ends,
        "corporate_break": c.members.len() >= w.rules.fed_tax_break_min_members.max(1),
        "tax_collected": c.record.tax_collected, "grants_paid": c.record.grants_paid,
        "elections": c.record.elections, "measures_passed": c.record.measures_passed,
        "laws": c.gov.laws.iter().map(law_json).collect::<Vec<_>>(),
        "offences": c.record.offences, "fines_collected": c.record.fines_collected,
        "territory": crate::territory::public_counts(w, c, &crate::territory::of(w, c.id)),
    })
}

pub fn lookup(w: &World, pid: PlayerId, a: &GovernmentArgs) -> Res<Value> {
    let mine = w.players.get(&pid).and_then(|p| p.corp);
    let other = match a.corp.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) => Some(
            w.corps
                .values()
                .find(|c| c.name.eq_ignore_ascii_case(s) || s.parse::<CorpId>().ok() == Some(c.id))
                .ok_or_else(|| Refusal::new("unknown_corp", "No such corporation."))?,
        ),
    };
    if let Some(c) = other
        && Some(c.id) != mine
    {
        return Ok(json!({"corp": c.name, "government": public_summary(w, c), "your_taxes": tax_view(w, pid)}));
    }
    if let Some(id) = a.measure {
        let c = corp_of(w, pid)?;
        let m = c
            .gov
            .measures
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| Refusal::new("unknown_measure", format!("{} has no measure #{id}.", c.name)))?;
        return Ok(json!({"measure": measure_json(w, &c, m, Some(pid))}));
    }
    Ok(view_own(w, pid))
}

/// Briefing block: the captain's taxes, and the measures awaiting its vote.
pub fn briefing(w: &World, pid: PlayerId) -> Value {
    let mut v = json!({"taxes": tax_view(w, pid)});
    let owed = warrants(w, pid);
    if !owed.is_empty() {
        v["warrants"] = json!(owed);
        v["warrant_rule"] = json!(
            "Pay with government action=pay_fine id=<offence>. Unpaid, a corporation's fighters seize it where they outnumber your fighters and shields."
        );
    }
    if let Some(c) = w.players.get(&pid).and_then(|p| p.corp).and_then(|c| w.corps.get(&c)) {
        let waiting: Vec<Value> = c
            .gov
            .measures
            .iter()
            .filter(|m| {
                m.status == MeasureStatus::Open
                    && m.electorate.contains(&pid)
                    && !m.votes.contains_key(&pid)
                    && !m.ballots.contains_key(&pid)
            })
            .map(|m| json!({"measure": m.id, "change": describe(w, &m.change), "closes_tick": m.closes_tick}))
            .collect();
        v["form"] = json!(c.gov.form);
        v["awaiting_your_vote"] = json!(waiting);
        v["how"] = json!("government action=view for your corporation's government, ledger and open measures.");
    }
    v
}

/// The operator's view of one corporation's government: everything, including every ballot.
pub fn sysop_view(w: &World, c: &Corp) -> Value {
    // Open measures and the last 20 closed; the newest 100 ledger lines (the state is polled).
    let closed = c.gov.measures.iter().filter(|m| m.status != MeasureStatus::Open).count();
    let measures: Vec<Value> = c
        .gov
        .measures
        .iter()
        .filter(|m| {
            m.status == MeasureStatus::Open
                || closed <= 20
                || m.closed_tick >= c.gov.measures.iter().filter_map(|x| x.closed_tick).rev().nth(19)
        })
        .map(|m| {
            let mut v = measure_json(w, c, m, None);
            v["votes"] = json!(m.votes.iter().map(|(p, y)| (w.player_name(*p), *y)).collect::<BTreeMap<_, _>>());
            v["ballot_by_voter"] = json!(
                m.ballots
                    .iter()
                    .map(|(p, x)| (w.player_name(*p), w.player_name(*x)))
                    .collect::<BTreeMap<_, _>>()
            );
            v
        })
        .collect();
    json!({
        "form": c.gov.form, "tax_bp": c.gov.tax_bp, "term_ends_tick": c.gov.term_ends,
        "measures": measures, "ledger_entries": c.gov.ledger.len(),
        "laws": c.gov.laws.iter().map(law_json).collect::<Vec<_>>(),
        "offences": c.gov.offences.iter().rev().take(50).map(|o| offence_json(w, o)).collect::<Vec<_>>(),
        "ledger": c.gov.ledger.iter().rev().take(100).map(|d| ledger_json(w, d)).collect::<Vec<_>>(),
    })
}
