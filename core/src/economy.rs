//! Port pricing (without haggling). Integer math only.
//!
//! A port that sells a commodity asks `B·(1 − t·stock/(100·cap))`: cheap when
//! full, near the base price when empty. A port that buys pays
//! `B·(1 + t·demand/(100·cap))`: up to ~1.9·B when it is hungry. `t` is a
//! per-port trade strength, so some port pairs are far better than others.
//! Trades are priced unit-by-unit (in 1%-of-capacity bands) against the
//! marginal stock, so bulk orders cannot exploit the opening quote.

use crate::model::{Port, PortTrade};
use crate::rules::Rules;

/// Experience discount in credits per unit (exp/100, capped).
pub fn exp_discount(rules: &Rules, exp: i64) -> i64 {
    (exp.max(0) / 100).min(rules.exp_discount_cap)
}

/// Price the port charges for one unit when it holds `stock`.
pub fn ask_unit(rules: &Rules, port: &Port, idx: usize, stock: i64) -> i64 {
    let cap = port.capacity[idx].max(1) as i128;
    let s = stock.clamp(0, port.capacity[idx]) as i128;
    let t = port.strength[idx] as i128;
    let b = rules.base_price[idx] as i128;
    let p = (b * (100 * cap - t * s) + 50 * cap) / (100 * cap);
    (p as i64).max(rules.min_ask)
}

/// Price the port pays for one unit when `demand` units of demand remain.
pub fn bid_unit(rules: &Rules, port: &Port, idx: usize, demand: i64) -> i64 {
    let cap = port.capacity[idx].max(1) as i128;
    let d = demand.clamp(0, port.capacity[idx]) as i128;
    let t = port.strength[idx] as i128;
    let b = rules.base_price[idx] as i128;
    ((b * (100 * cap + t * d) + 50 * cap) / (100 * cap)).max(1) as i64
}

/// Current single-unit quote for a commodity slot (ask if the port sells, bid if it buys).
pub fn quote(rules: &Rules, port: &Port, idx: usize) -> i64 {
    match port.trades[idx] {
        PortTrade::Sells => ask_unit(rules, port, idx, port.stock[idx]),
        PortTrade::Buys => bid_unit(rules, port, idx, port.stock[idx]),
    }
}

fn band(port: &Port, idx: usize) -> i64 {
    (port.capacity[idx] / 100).max(1)
}

/// Total cost of buying `qty` units (stock falls as you buy), less `discount` per unit (min 1/unit).
pub fn total_ask(rules: &Rules, port: &Port, idx: usize, qty: i64, discount: i64) -> i64 {
    let mut total = 0;
    let mut s = port.stock[idx];
    let mut left = qty;
    while left > 0 {
        let n = left.min(band(port, idx));
        total += (ask_unit(rules, port, idx, s - n / 2) - discount).max(1) * n;
        s -= n;
        left -= n;
    }
    total
}

/// Total revenue from selling `qty` units (demand falls as you sell), plus `bonus` per unit.
pub fn total_bid(rules: &Rules, port: &Port, idx: usize, qty: i64, bonus: i64) -> i64 {
    let mut total = 0;
    let mut d = port.stock[idx];
    let mut left = qty;
    while left > 0 {
        let n = left.min(band(port, idx));
        total += (bid_unit(rules, port, idx, d - n / 2) + bonus) * n;
        d -= n;
        left -= n;
    }
    total
}

/// Largest quantity affordable with `credits` (at most `limit` and the port's stock).
pub fn max_affordable(rules: &Rules, port: &Port, idx: usize, credits: i64, limit: i64, discount: i64) -> i64 {
    let (mut lo, mut hi) = (0, limit.min(port.stock[idx]).max(0));
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if total_ask(rules, port, idx, mid, discount) <= credits {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PortTrade::*;

    fn port(trade: PortTrade, stock: i64, t: i64) -> Port {
        Port {
            name: "T".into(),
            class: 1,
            trades: [trade; 3],
            stock: [stock; 3],
            capacity: [1_000; 3],
            strength: [t; 3],
            volume: [0; 3],
            last_trade: 0,
            stardock: false,
            black_market: false,
            contraband_demand: 0,
            regen_carry: [0; 3],
        }
    }

    #[test]
    fn tw_price_curve_matches_rule_trade_002() {
        let r = Rules::default();
        // Selling port with t=90 at full stock asks ~0.1B; empty, ~B.
        assert_eq!(ask_unit(&r, &port(Sells, 1_000, 90), 0, 1_000), 4); // 25*0.1 = 2.5 -> floor 4
        assert_eq!(ask_unit(&r, &port(Sells, 0, 90), 2, 0), 90);
        assert_eq!(ask_unit(&r, &port(Sells, 500, 40), 2, 500), 72); // 90*(1-0.2)
        // Buying port with t=90 and full demand pays ~1.9B.
        assert_eq!(bid_unit(&r, &port(Buys, 1_000, 90), 2, 1_000), 171);
        assert_eq!(bid_unit(&r, &port(Buys, 0, 90), 2, 0), 90);
    }

    #[test]
    fn bulk_buy_costs_more_than_opening_quote() {
        let r = Rules::default();
        let p = port(Sells, 1_000, 60);
        assert!(total_ask(&r, &p, 0, 500, 0) > ask_unit(&r, &p, 0, 1_000) * 500);
    }

    #[test]
    fn affordability_is_exact_and_discount_applies() {
        let r = Rules::default();
        let p = port(Sells, 1_000, 50);
        let q = max_affordable(&r, &p, 1, 5_000, 10_000, 0);
        assert!(total_ask(&r, &p, 1, q, 0) <= 5_000);
        assert!(total_ask(&r, &p, 1, q + 1, 0) > 5_000);
        assert!(total_ask(&r, &p, 1, 100, 5) < total_ask(&r, &p, 1, 100, 0));
        assert_eq!(exp_discount(&r, 250), 2);
        assert_eq!(exp_discount(&r, 1_000_000), r.exp_discount_cap);
    }
}
