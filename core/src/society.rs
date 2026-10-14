//! Society rewards (engine 1.59): credits for the work that builds the captains' society.
//!
//! Three sources pay credits on top of the Influence they already give:
//! - **The Chronicle:** a page that becomes canon pays its creator `chronicle_canon_credits`
//!   (the creator alone, as canon Influence is attributed); a released production pays each
//!   captain in its credits `chronicle_credit_credits`. Both share `chronicle_credits_per_day`.
//! - **Languages:** the first time another captain adopts a language, its creator receives
//!   `language_adopt_credits`, within `language_credits_per_day`. Defining pays nothing.
//! - **The improve board:** an operator award pays `improve_credits_per_influence` credits per
//!   Influence point (no cap of its own: the award budget bounds it).
//!
//! Credits go into the bank (safe from ship loss) and count in net worth, `earned` and the daily
//! wealth race like any income (they are not wires, so `wired_today` is untouched), and in
//! `stats.society_rewards`. Each payout is made inside the journaled command it belongs to, so
//! replay reproduces it. A cap counts the trailing `ticks_per_day` ticks per captain and source,
//! from the captain's own ledger (`Player.society`, pruned to that window). A payout over a cap
//! pays what remains of it, and nothing when none remains; it is never paid later. A rule of 0
//! (an amount or a cap) switches that payout off.
use crate::engine::Ctx;
use crate::model::{PlayerId, PlayerKind, Tick, World};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Chronicle,
    Language,
    Improve,
}

/// One payout in a captain's trailing-day ledger.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Payout {
    pub tick: Tick,
    pub source: Source,
    pub amount: i64,
}

/// What a payout did: credits paid, and the part a daily cap held back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Paid {
    pub paid: i64,
    pub capped: i64,
}

fn window(w: &World) -> Tick {
    crate::player::ticks_per_day(&w.rules).max(1) as Tick
}

/// Credits `pid` received from `source` in the trailing day.
pub fn paid_in_window(w: &World, pid: PlayerId, source: Source) -> i64 {
    let Some(p) = w.players.get(&pid) else { return 0 };
    let (now, span) = (w.tick, window(w));
    p.society
        .iter()
        .filter(|x| x.source == source && now.saturating_sub(x.tick) < span)
        .map(|x| x.amount)
        .sum()
}

/// A captain who may receive society credits: a known, unbanned, non-NPC player.
pub fn eligible(w: &World, pid: PlayerId) -> bool {
    w.players
        .get(&pid)
        .is_some_and(|p| p.npc.is_none() && p.kind != PlayerKind::Npc && !p.banned)
}

/// Pay `amount` credits from `source` into `pid`'s bank, within `cap` per trailing day (`None`:
/// no cap). Returns what was paid and what the cap held back. Nothing happens (the payout is
/// switched off) for an amount or a cap of 0 or less, or an ineligible player.
pub fn pay(ctx: &mut Ctx, pid: PlayerId, source: Source, amount: i64, cap: Option<i64>) -> Paid {
    if amount <= 0 || cap.is_some_and(|c| c <= 0) || !eligible(ctx.w, pid) {
        return Paid::default();
    }
    let room = match cap {
        None => amount,
        Some(c) => c.saturating_sub(paid_in_window(ctx.w, pid, source)).max(0),
    };
    let paid = amount.min(room);
    let (tick, span) = (ctx.w.tick, window(ctx.w));
    let p = ctx.player_mut(pid);
    p.society.retain(|x| tick.saturating_sub(x.tick) < span);
    if paid > 0 {
        p.bank = p.bank.saturating_add(paid);
        p.stats.society_rewards = p.stats.society_rewards.saturating_add(paid);
        p.society.push(Payout {
            tick,
            source,
            amount: paid,
        });
    }
    Paid {
        paid,
        capped: amount - paid,
    }
}

/// The sentence a notice adds about a payout ("" when nothing was due).
pub fn note(p: Paid, cap: Option<i64>) -> String {
    let cap = cap.unwrap_or(0).max(0);
    match (p.paid, p.capped) {
        (0, 0) => String::new(),
        (paid, 0) => format!(" {paid} credits were paid into your bank."),
        (0, held) => format!(
            " It would have paid {held} credits, but you reached the daily cap of {cap} credits for this reward, so nothing was paid."
        ),
        (paid, held) => {
            format!(" {paid} credits were paid into your bank ({held} more were over the daily cap of {cap} credits for this reward).")
        }
    }
}
