//! Port trading.

use serde_json::{Value, json};

use crate::cmd::*;
use crate::economy;
use crate::engine::*;
use crate::knowledge;
use crate::model::*;

#[derive(Debug, Clone)]
pub struct Quote {
    pub idx: usize,
    pub action: TradeAction,
    pub requested: Option<i64>,
    pub filled: i64,
    pub total: i64,
    pub limited_by: &'static str,
}

/// Pure pricing of a trade for `pid` at their current port.
pub fn quote(w: &World, pid: PlayerId, a: &TradeArgs) -> Res<Quote> {
    let p = w
        .players
        .get(&pid)
        .ok_or_else(|| Refusal::new("unknown_player", "No such player."))?;
    let sector = w.sector(p.sector).unwrap();
    let port = sector.port.as_ref().ok_or_else(|| {
        Refusal::new("no_port", format!("There is no port in sector {}.", p.sector))
            .hint("Use find_trade_routes or list_ports to locate ports you have charted.")
    })?;
    let idx = a.commodity.idx();
    let rules = &w.rules;
    let discount = economy::exp_discount(rules, p.experience);
    if let Some(q) = a.quantity
        && q <= 0
    {
        return Err(Refusal::new(
            "invalid_args",
            "quantity must be positive (or omitted for the maximum).",
        ));
    }
    match a.action {
        TradeAction::Buy => {
            if port.trades[idx] != PortTrade::Sells {
                return Err(Refusal::new(
                    "port_does_not_sell",
                    format!("{} ({}) does not sell {}.", port.name, port.code(), a.commodity.name()),
                )
                .hint(
                    "Port codes list fuel_ore/organics/equipment in order: S = port sells (you can buy), B = port buys (you can sell).",
                ));
            }
            let free = p.ship.free_holds();
            let stock = port.stock[idx];
            let afford = economy::max_affordable(rules, port, idx, p.credits, free.min(stock), discount);
            let want = a.quantity.unwrap_or(i64::MAX);
            let filled = want.min(afford);
            let limited_by = if filled == want {
                "request"
            } else if afford == free {
                "cargo holds"
            } else if afford == stock {
                "port stock"
            } else {
                "credits"
            };
            if filled <= 0 {
                let why = if free == 0 {
                    "your holds are full"
                } else if stock == 0 {
                    "the port is sold out"
                } else {
                    "you cannot afford a single unit"
                };
                return Err(Refusal::new("cannot_buy", format!("Cannot buy {}: {why}.", a.commodity.name()))
                    .hint("Sell cargo first, or check get_status."));
            }
            let total = economy::total_ask(rules, port, idx, filled, discount);
            Ok(Quote {
                idx,
                action: a.action,
                requested: a.quantity,
                filled,
                total,
                limited_by,
            })
        }
        TradeAction::Sell => {
            if port.trades[idx] != PortTrade::Buys {
                return Err(Refusal::new(
                    "port_does_not_buy",
                    format!("{} ({}) does not buy {}.", port.name, port.code(), a.commodity.name()),
                )
                .hint("Find a port with B in that commodity's position (fuel_ore, organics, equipment)."));
            }
            let have = p.ship.cargo[idx];
            let demand = port.stock[idx];
            let want = a.quantity.unwrap_or(have);
            let filled = want.min(have).min(demand);
            let limited_by = if filled == want {
                "request"
            } else if filled == have {
                "cargo aboard"
            } else {
                "port demand"
            };
            if filled <= 0 {
                let why = if have == 0 {
                    format!("you carry no {}", a.commodity.name())
                } else {
                    "the port's demand is exhausted".to_string()
                };
                return Err(Refusal::new("cannot_sell", format!("Cannot sell: {why}.")));
            }
            let boom = crate::events::boom_pct(w, sector.id, idx);
            let total = economy::total_bid(rules, port, idx, filled, discount) * (100 + boom) / 100;
            Ok(Quote {
                idx,
                action: a.action,
                requested: a.quantity,
                filled,
                total,
                limited_by,
            })
        }
    }
}

pub fn quote_json(w: &World, pid: PlayerId, q: &Quote) -> Value {
    let p = &w.players[&pid];
    let port = w.sector(p.sector).unwrap().port.as_ref().unwrap();
    json!({
        "action": q.action,
        "commodity": Commodity::ALL[q.idx],
        "requested": q.requested,
        "filled": q.filled,
        "total_credits": q.total,
        "avg_unit_price": if q.filled > 0 { q.total / q.filled } else { 0 },
        "limited_by": q.limited_by,
        "port": port.name,
        "port_code": port.code(),
    })
}

pub fn trade(ctx: &mut Ctx, pid: PlayerId, a: &TradeArgs) -> Res<Outcome> {
    if a.contraband {
        return crate::frontier::smuggle(ctx, pid, a);
    }
    let q = quote(ctx.w, pid, a)?;
    if a.dry_run {
        let reply = json!({"dry_run": true, "quote": quote_json(ctx.w, pid, &q), "you": brief(ctx.w, pid)});
        return Ok(ctx.finish(
            format!(
                "Quote: {} {} {} for {} credits.",
                if q.action == TradeAction::Buy { "buy" } else { "sell" },
                q.filled,
                a.commodity.name(),
                q.total
            ),
            reply,
        ));
    }
    let cost = ctx.w.rules.ap_trade;
    ctx.require_ap(pid, cost)?;
    ctx.spend_ap(pid, cost);
    let qj = quote_json(ctx.w, pid, &q);
    let sid = ctx.w.players[&pid].sector;
    let tick = ctx.w.tick;
    let idx = q.idx;
    let mut profit = None;
    {
        let port = ctx.w.sector_mut(sid).unwrap().port.as_mut().unwrap();
        port.stock[idx] -= q.filled;
        port.volume[idx] += q.filled;
        port.last_trade = tick;
    }
    let p = ctx.player_mut(pid);
    match q.action {
        TradeAction::Buy => {
            p.credits -= q.total;
            if p.ship.cargo[idx] == 0 {
                // Nothing aboard carries a cost: clears basis left by an older engine.
                p.ship.cargo_cost[idx] = 0;
            }
            p.ship.cargo[idx] += q.filled;
            p.ship.cargo_cost[idx] += q.total;
        }
        TradeAction::Sell => {
            let basis = if p.ship.cargo[idx] > 0 {
                p.ship.cargo_cost[idx] * q.filled / p.ship.cargo[idx]
            } else {
                0
            };
            p.ship.cargo_cost[idx] -= basis;
            p.ship.cargo[idx] -= q.filled;
            p.credits += q.total;
            let pr = q.total - basis;
            p.stats.trade_profit += pr;
            profit = Some(pr);
        }
    }
    p.stats.trades += 1;
    p.experience += 1 + q.filled / 50;
    let name = p.name.clone();
    knowledge::observe(ctx.w, pid, sid, true);
    let taxes = profit.and_then(|pr| crate::government::tax_trade_profit(ctx, pid, pr));
    let port = ctx.w.sector(sid).unwrap().port.as_ref().unwrap();
    let new_quote = economy::quote(&ctx.w.rules, port, idx);
    let verb = if q.action == TradeAction::Buy { "bought" } else { "sold" };
    let mut summary = format!(
        "{} {} {} for {} credits (avg {}/unit).",
        capitalize(verb),
        q.filled,
        a.commodity.name(),
        q.total,
        q.total / q.filled.max(1)
    );
    if let Some(pr) = profit {
        summary.push_str(&format!(" Profit vs purchase cost: {pr}."));
    }
    if let Some(t) = &taxes {
        summary.push_str(&format!(
            " Taxes: {} to the Federation, {} to your corporation; you keep {}.",
            t["federation"], t["corporation"], t["net_profit"]
        ));
    }
    if q.limited_by != "request" && a.quantity.is_some() {
        summary.push_str(&format!(" Partially filled (limited by {}).", q.limited_by));
    }
    ctx.event(
        "trade",
        Some(sid),
        vec![pid],
        q.total >= 2_000,
        format!("{name} {verb} {} {} at sector {sid}.", q.filled, a.commodity.name()),
        json!({"commodity": a.commodity, "qty": q.filled, "total": q.total}),
    );
    let reply = json!({"trade": qj, "profit": profit, "taxes": taxes, "port_quote_now": new_quote, "you": brief(ctx.w, pid)});
    Ok(ctx.finish(summary, reply))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}
