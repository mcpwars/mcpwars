//! A bounded, journaled sequence of ordinary trades and autopilot legs. Never waits for AP.
use crate::cmd::{AutopilotArgs, TradeAction, TradeArgs};
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
}
pub fn execute(ctx: &mut Ctx, pid: PlayerId, a: &Args) -> Res<Outcome> {
    let p = ctx.player(pid)?;
    if !(1..=20).contains(&a.laps)
        || a.buy_at == a.sell_at
        || p.sector != a.buy_at
        || a.quantity.is_some_and(|q| q <= 0)
        || p.ship.cargo[a.commodity.idx()] != 0
    {
        return Err(Refusal::new(
            "invalid_args",
            "Start at buy_at with no cargo of this commodity; choose distinct ports, positive quantity and 1-20 laps.",
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
    'laps: for _ in 0..a.laps {
        let chart = crate::knowledge::merged(ctx.w, pid);
        let outbound = crate::path::plot_on(ctx.w, pid, &chart, a.buy_at, a.sell_at, &Default::default(), true);
        let inbound = crate::path::plot_on(ctx.w, pid, &chart, a.sell_at, a.buy_at, &Default::default(), true);
        let (Some(outbound), Some(inbound)) = (outbound, inbound) else {
            stop = Some("No safe known round-trip route.".into());
            break;
        };
        if !outbound.hazards.is_empty() || !inbound.hazards.is_empty() {
            stop = Some("Known hazard at a port.".into());
            break;
        }
        let warp_ap = ctx.w.rules.ship(ctx.w.players[&pid].ship.class).warp_ap;
        let needed = (outbound.path.len() + inbound.path.len() - 2) as i64 * warp_ap + 2 * ctx.w.rules.ap_trade;
        if ctx.w.players[&pid].ap < needed {
            stop = Some(format!("Insufficient AP for a full lap: need {needed}."));
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
        match crate::trade::quote(ctx.w, pid, &buy) {
            Ok(q) => {
                let discount = crate::economy::exp_discount(&ctx.w.rules, ctx.w.players[&pid].experience);
                let bid = (port.prices[a.commodity.idx()] + discount)
                    * (100 + crate::events::boom_pct(ctx.w, a.sell_at, a.commodity.idx()))
                    / 100;
                if bid * q.filled <= q.total {
                    stop = Some("Observed buyer quote does not cover the current purchase cost.".into());
                    break;
                }
            }
            Err(r) => {
                stop = Some(r.to_string());
                break;
            }
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
            let nav = AutopilotArgs {
                destination_sector: destination,
                avoid_sectors: vec![],
                avoid_hazards: true,
                max_toll: Some(0),
            };
            match crate::nav::autopilot(ctx, pid, &nav) {
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
    let reply = json!({"completed_laps": laps, "requested_laps": a.laps, "ap_spent": initial_ap - p.ap, "credit_change_after_tax": p.credits - initial_credits,
        "units_bought": units_bought, "units_sold": units_sold, "sector": p.sector, "cargo_remaining": p.ship.cargo[a.commodity.idx()], "stop_reason": stop,
        "steps": if a.full { Some(steps) } else { None }, "note": "Each step uses normal AP, prices, taxes and navigation. Completed steps remain committed on interruption; check final sector and cargo before retrying. Never waits for AP or guarantees a profit."});
    let mut out = ctx.finish(format!("Trade loop completed {laps} of {} laps.", a.laps), reply);
    out.events.extend(events);
    Ok(out)
}
