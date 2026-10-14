//! A bounded, journaled sequence of ordinary trades and autopilot legs. Never waits for AP.
use std::collections::BTreeSet;

use crate::cmd::{TradeAction, TradeArgs};
use crate::engine::{Ctx, Outcome, Refusal, Res};
use crate::model::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Args {
    pub buy_at: SectorId,
    pub sell_at: SectorId,
    pub commodity: Commodity,
    /// Maximum full laps, 1-20. Stops when AP, stock, demand or safety prevents progress.
    pub laps: u32,
    #[serde(default)]
    pub quantity: Option<i64>,
    /// Include complete per-operation replies (default false: totals and stop reason).
    #[serde(default)]
    pub full: bool,
    /// Sectors neither leg may enter, checked before buying (default none; at most one per galaxy sector).
    #[serde(default)]
    pub avoid_sectors: Vec<SectorId>,
    /// Pay tolls up to this each without turning back (default 0). Charted tolls within it are planned through and their lap total kept from the purchase; an uncharted one met in flight may turn the ship back.
    #[serde(default)]
    pub max_toll: Option<i64>,
}
pub fn execute(ctx: &mut Ctx, pid: PlayerId, a: &Args) -> Res<Outcome> {
    let p = ctx.player(pid)?;
    if !(1..=20).contains(&a.laps) {
        return Err(Refusal::new("invalid_args", format!("laps must be 1..=20 (got {}).", a.laps)));
    }
    if a.buy_at == a.sell_at {
        return Err(Refusal::new("invalid_args", "buy_at and sell_at must be distinct sectors."));
    }
    if p.sector != a.buy_at {
        return Err(Refusal::new(
            "invalid_args",
            format!("The ship is in sector {}, not buy_at ({}).", p.sector, a.buy_at),
        ));
    }
    if a.quantity.is_some_and(|q| q <= 0) {
        return Err(Refusal::new("invalid_args", "quantity must be positive."));
    }
    if p.ship.cargo[a.commodity.idx()] != 0 {
        return Err(Refusal::new(
            "invalid_args",
            format!(
                "The ship already carries {} units of {} and trade_loop requires starting with 0.",
                p.ship.cargo[a.commodity.idx()],
                a.commodity.name()
            ),
        ));
    }
    if a.max_toll.is_some_and(|t| t < 0) {
        return Err(Refusal::new("invalid_args", "max_toll must be >= 0."));
    }
    // The list is journaled; more entries than the galaxy has sectors can only be a mistake.
    if a.avoid_sectors.len() > ctx.w.sector_count() as usize {
        return Err(Refusal::new(
            "invalid_args",
            format!(
                "avoid_sectors lists more entries than the galaxy has sectors ({}).",
                ctx.w.sector_count()
            ),
        ));
    }
    let initial_ap = p.ap;
    let initial_credits = p.credits;
    let mut laps = 0;
    let mut units_bought = 0;
    let mut units_sold = 0;
    let mut steps: Vec<Value> = vec![];
    let mut stop: Option<String> = None;
    let mut events = vec![];
    // The policy (avoid_sectors, max_toll) is checked for the whole lap before its purchase, so a
    // lap it stops makes no change. Omitted, it is the old behaviour: no exclusions, toll cap 0.
    // Planning and both legs share one toll allowance: a charted toll the ship would pay (doctrine
    // pay_tolls on, toll within max_toll) is not a hazard, so a toll paid on one leg, and charted
    // by paying it, does not divert or strand a later leg or lap.
    let avoid: BTreeSet<SectorId> = a.avoid_sectors.iter().copied().collect();
    let applied_toll = a.max_toll.unwrap_or(0);
    let toll_cap = if p.doctrine.pay_tolls { applied_toll } else { 0 };
    let mut planned: Option<(Vec<SectorId>, Vec<SectorId>)> = None;
    let mut toll_reserve = 0;
    'laps: for _ in 0..a.laps {
        // path::plot_on always lets the destination through, so an excluded port is caught here.
        if let Some(port) = [a.buy_at, a.sell_at].into_iter().find(|s| avoid.contains(s)) {
            stop = Some(format!(
                "Sector {port} is one of this loop's ports and is in avoid_sectors: no lap can avoid it."
            ));
            break;
        }
        let chart = crate::knowledge::merged(ctx.w, pid);
        let outbound = crate::path::plot_within(ctx.w, pid, &chart, a.buy_at, a.sell_at, &avoid, true, toll_cap);
        let inbound = crate::path::plot_within(ctx.w, pid, &chart, a.sell_at, a.buy_at, &avoid, true, toll_cap);
        let (Some(outbound), Some(inbound)) = (outbound, inbound) else {
            stop = Some(if avoid.is_empty() {
                "No safe known round-trip route.".into()
            } else {
                "No safe known round-trip route avoids avoid_sectors.".into()
            });
            break;
        };
        // Defensive: the plotted lap must not touch an excluded sector anywhere.
        if outbound.path.iter().chain(inbound.path.iter()).any(|s| avoid.contains(s)) {
            stop = Some("No safe known round-trip route avoids avoid_sectors.".into());
            break;
        }
        planned = Some((outbound.path.clone(), inbound.path.clone()));
        // Credits kept back from the purchase (engine 1.57): every charted toll this lap's planned
        // legs would pay (the planner's own predicate, entry into each sector after a leg's start,
        // a sector crossed twice counted twice), so a lap that buys does not reach a toll it cannot
        // pay. Purchases are untaxed and the sale's tax comes out of its proceeds, so nothing else
        // is reserved. An uncharted toll met in flight cannot be known here. Allowance 0: none.
        toll_reserve = if toll_cap > 0 {
            outbound
                .path
                .iter()
                .skip(1)
                .chain(inbound.path.iter().skip(1))
                .filter_map(|s| chart.get(s))
                .filter(|k| crate::knowledge::known_payable_toll(ctx.w, pid, k, toll_cap))
                .filter_map(|k| k.toll)
                .sum()
        } else {
            0
        };
        if !outbound.hazards.is_empty() || !inbound.hazards.is_empty() {
            stop = Some("Known hazard at a port.".into());
            break;
        }
        let warp_ap = ctx.w.rules.ship(ctx.w.players[&pid].ship.class).warp_ap;
        let needed = (outbound.path.len() + inbound.path.len() - 2) as i64 * warp_ap + 2 * ctx.w.rules.ap_trade;
        if ctx.w.players[&pid].ap < needed {
            stop = Some(format!(
                "Insufficient AP for a full lap: need {needed} ({} hops out, {} hops back to buy_at at {warp_ap} AP each, 2 trades).",
                outbound.path.len() - 1,
                inbound.path.len() - 1
            ));
            break;
        }
        let mut buy = TradeArgs {
            commodity: a.commodity,
            action: TradeAction::Buy,
            quantity: a.quantity,
            contraband: false,
            dry_run: false,
        };
        // Use only the observed buyer's demand, never hidden live remote prices.
        let Some(port) = chart.get(&a.sell_at).and_then(|k| k.port.as_ref()) else {
            stop = Some("Buyer is not charted.".into());
            break;
        };
        if port.code.as_bytes()[a.commodity.idx()] != b'B' || port.fill_pct[a.commodity.idx()] == 0 {
            stop = Some("Observed buyer has no demand for this commodity.".into());
            break;
        }
        if outbound.path.len() - 1 > ctx.w.rules.autopilot_max_hops as usize
            || inbound.path.len() - 1 > ctx.w.rules.autopilot_max_hops as usize
        {
            stop = Some("A leg exceeds the autopilot hop limit.".into());
            break;
        }
        let capacity = ctx
            .w
            .sector(a.sell_at)
            .and_then(|s| s.port.as_ref())
            .map(|p| p.capacity[a.commodity.idx()])
            .unwrap_or(0);
        let demand = capacity * port.fill_pct[a.commodity.idx()] / 100;
        buy.quantity = Some(buy.quantity.unwrap_or(i64::MAX).min(demand));
        let mut q = match crate::trade::quote(ctx.w, pid, &buy) {
            Ok(q) => q,
            Err(r) => {
                stop = Some(r.to_string());
                break;
            }
        };
        let discount = crate::economy::exp_discount(&ctx.w.rules, ctx.w.players[&pid].experience);
        let credits = ctx.w.players[&pid].credits;
        if toll_reserve > 0 && credits - q.total < toll_reserve {
            let seller = ctx.w.sector(a.buy_at).and_then(|s| s.port.as_ref());
            let cap = seller.map_or(0, |sp| {
                crate::economy::max_affordable(&ctx.w.rules, sp, a.commodity.idx(), credits - toll_reserve, q.filled, discount)
            });
            if cap <= 0 {
                stop = Some(format!(
                    "Credits cannot buy one unit and keep the toll reserve of {toll_reserve} credits for the charted tolls this lap plans to pay."
                ));
                break;
            }
            buy.quantity = Some(cap);
            q = match crate::trade::quote(ctx.w, pid, &buy) {
                Ok(q) => q,
                Err(r) => {
                    stop = Some(r.to_string());
                    break;
                }
            };
        }
        let bid = (port.prices[a.commodity.idx()] + discount) * (100 + crate::events::boom_pct(ctx.w, a.sell_at, a.commodity.idx())) / 100;
        // With a toll reserve the margin must also cover the tolls the lap pays; a lap that would
        // lose money to them stops before buying (1.57). Without one this is the 1.55 check.
        if bid * q.filled <= q.total + toll_reserve {
            stop = Some(if toll_reserve > 0 {
                format!(
                    "Observed buyer quote does not cover the purchase cost plus the {toll_reserve} credits of charted tolls this lap pays."
                )
            } else {
                "Observed buyer quote does not cover the current purchase cost.".into()
            });
            break;
        }
        match crate::trade::trade(ctx, pid, &buy) {
            Ok(out) => {
                units_bought += out.reply["trade"]["filled"].as_i64().unwrap_or(0);
                steps.push(out.reply);
                events.extend(out.events);
            }
            Err(r) => {
                stop = Some(r.to_string());
                break;
            }
        }
        for (destination, sell) in [(a.sell_at, true), (a.buy_at, false)] {
            let before_notices = ctx.w.players[&pid].next_notice;
            match crate::nav::autopilot_within(ctx, pid, destination, &avoid, true, Some(applied_toll), toll_cap) {
                Ok(out) => {
                    let arrived = out.reply["arrived"] == true;
                    let reason = out.reply["stop_reason"].as_str().map(str::to_string);
                    steps.push(out.reply);
                    events.extend(out.events);
                    if !arrived || ctx.w.players[&pid].next_notice > before_notices {
                        stop = reason.or(Some(
                            "Navigation interruption or new notice: inspect your briefing before continuing.".into(),
                        ));
                        break 'laps;
                    }
                }
                Err(r) => {
                    stop = Some(r.to_string());
                    break 'laps;
                }
            }
            if sell {
                let trade = TradeArgs {
                    commodity: a.commodity,
                    action: TradeAction::Sell,
                    quantity: None,
                    contraband: false,
                    dry_run: false,
                };
                match crate::trade::trade(ctx, pid, &trade) {
                    Ok(out) => {
                        units_sold += out.reply["trade"]["filled"].as_i64().unwrap_or(0);
                        steps.push(out.reply);
                        events.extend(out.events);
                    }
                    Err(r) => {
                        stop = Some(r.to_string());
                        break 'laps;
                    }
                }
                if ctx.w.players[&pid].ship.cargo[a.commodity.idx()] > 0 {
                    stop = Some("Buyer only partially filled the sale; cargo remains.".into());
                    break 'laps;
                }
            }
        }
        laps += 1;
    }
    let p = &ctx.w.players[&pid];
    let mut reply = json!({"completed_laps": laps, "requested_laps": a.laps, "ap_spent": initial_ap - p.ap, "credit_change_after_tax": p.credits - initial_credits,
        "units_bought": units_bought, "units_sold": units_sold, "sector": p.sector, "cargo_remaining": p.ship.cargo[a.commodity.idx()], "stop_reason": stop,
        "steps": if a.full { Some(steps) } else { None }, "note": "Each step uses normal AP, prices, taxes and navigation. Completed steps remain committed on interruption; check final sector and cargo before retrying. Never waits for AP or guarantees a profit."});
    // A default call's reply is unchanged: the policy appears only when one was given or full=true.
    if !avoid.is_empty() || a.max_toll.is_some() || a.full {
        let mut policy = json!({"avoid_sectors": avoid, "max_toll": applied_toll, "avoid_hazards": true});
        // Only a call that names max_toll can reserve, so only its reply gains the field (1.57).
        if a.max_toll.is_some() {
            policy["toll_reserve"] = json!(toll_reserve);
        }
        if a.full {
            let (outbound, inbound) = planned.unzip();
            policy["planned_outbound"] = json!(outbound);
            policy["planned_inbound"] = json!(inbound);
        }
        reply["policy"] = policy;
    }
    let mut out = ctx.finish(format!("Trade loop completed {laps} of {} laps.", a.laps), reply);
    out.events.extend(events);
    Ok(out)
}
