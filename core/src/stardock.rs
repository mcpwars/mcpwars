//! Stardock services: shipyard, hardware emporium, Galactic Bank, colonist
//! recruitment, Federation relief.

use serde_json::json;

/// Ether probes a ship can carry.
pub const PROBES_MAX: i64 = 10;

use crate::cmd::*;
use crate::engine::*;
use crate::model::*;
use crate::player;

/// Shipyard and bank serve wanted pilots (for a bribe); the emporium does not.
fn require_stardock_basic(w: &World, pid: PlayerId) -> Res<()> {
    let p = &w.players[&pid];
    let here = w
        .sector(p.sector)
        .and_then(|s| s.port.as_ref())
        .map(|p| p.stardock)
        .unwrap_or(false);
    if !here {
        return Err(Refusal::new(
            "not_at_stardock",
            format!("You must be docked at Stardock (sector {}).", w.rules.stardock_sector),
        )
        .hint(format!("autopilot_travel destination_sector={}", w.rules.stardock_sector)));
    }
    Ok(())
}

fn wanted(w: &World, pid: PlayerId) -> bool {
    w.players[&pid].alignment <= w.rules.fed_wanted_alignment
}

fn require_stardock(w: &World, pid: PlayerId) -> Res<()> {
    let p = &w.players[&pid];
    let here = w
        .sector(p.sector)
        .and_then(|s| s.port.as_ref())
        .map(|p| p.stardock)
        .unwrap_or(false);
    if !here {
        return Err(Refusal::new(
            "not_at_stardock",
            format!("You must be docked at Stardock (sector {}).", w.rules.stardock_sector),
        )
        .hint(format!("autopilot_travel destination_sector={}", w.rules.stardock_sector)));
    }
    if p.alignment <= w.rules.fed_wanted_alignment {
        return Err(
            Refusal::new("wanted", "Stardock refuses service to wanted criminals (alignment too low).")
                .hint("Raise alignment by destroying Skarn raiders or evil players, or posting bounties on evil players."),
        );
    }
    Ok(())
}

/// Trade-in pays only for what is actually surrendered: the hull, its hold
/// upgrades, and a TransWarp drive the new hull cannot mount. Fighters,
/// shields, mines and other gear move to the new ship and are never appraised
/// (appraising them let players mint credits by re-buying the same hull).
/// Extra holds bought for `ship` that carry over to a `new_class` hull (up to its maximum).
pub fn carried_holds(w: &World, ship: &Ship, new_class: ShipClass) -> i64 {
    let extra = (ship.holds - w.rules.ship(ship.class).holds_initial).max(0);
    let new = w.rules.ship(new_class);
    extra.min((new.holds_max - new.holds_initial).max(0))
}

/// What Stardock pays for the old hull: the hull itself, plus any purchased
/// holds that cannot move to the new hull, at the trade-in rate.
pub fn trade_in_value(w: &World, ship: &Ship, new_class: ShipClass) -> i64 {
    if ship.class == ShipClass::EscapePod {
        return 0;
    }
    let r = &w.rules;
    let spec = r.ship(ship.class);
    let extra = (ship.holds - spec.holds_initial).max(0);
    let lost = extra - carried_holds(w, ship, new_class);
    let hull = if ship.relief { 0 } else { spec.cost };
    let mut v = hull + r.holds_cost(spec.holds_initial + extra - lost, lost);
    if ship.transwarp && !r.ship(new_class).can_transwarp {
        v += r.price_transwarp / 2;
    }
    v * r.trade_in_bp / 10_000
}

pub fn buy_ship(ctx: &mut Ctx, pid: PlayerId, a: &BuyShipArgs) -> Res<Outcome> {
    require_stardock_basic(ctx.w, pid)?;
    let bribe = wanted(ctx.w, pid);
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    if a.ship_class == ShipClass::EscapePod {
        return Err(Refusal::new("invalid_args", "Escape pods are not for sale."));
    }
    if a.ship_class == ShipClass::CorporateFlagship && p.corp.is_none() {
        return Err(
            Refusal::new("corp_required", "The Corporate Flagship is sold only to corporation members.")
                .hint("corp action=create or join one."),
        );
    }
    let spec = rules.ship(a.ship_class).clone();
    let trade_in = trade_in_value(ctx.w, &p.ship, a.ship_class);
    let price = if bribe { spec.cost * 5 / 4 } else { spec.cost };
    let net = price - trade_in;
    let warship = matches!(a.ship_class, ShipClass::Battleship | ShipClass::CorporateFlagship);
    let ends_protection = warship && player::is_newbie(ctx.w, pid);
    let reply_quote = json!({"ship_class": a.ship_class, "price": price, "trade_in": trade_in, "net_cost": net, "credits": p.credits,
        "wanted_surcharge": bribe, "holds_carried_over": carried_holds(ctx.w, &p.ship, a.ship_class),
        "ends_newbie_protection": ends_protection});
    if a.dry_run {
        return Ok(ctx.finish(
            format!("{} costs {} ({} after trade-in).", spec.display, spec.cost, net),
            json!({"dry_run": true, "quote": reply_quote}),
        ));
    }
    if p.credits < net {
        return Err(ctx.no_credits(pid, &format!("A {} (after trade-in)", spec.display), net));
    }
    let name = a
        .name
        .as_deref()
        .map(|n| player::sanitize(n, 30))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{}'s {}", p.name, spec.display));
    let mut ship = player::new_ship(&rules, a.ship_class, &name);
    let old = &p.ship;
    ship.holds += carried_holds(ctx.w, old, a.ship_class);
    ship.fighters = old.fighters.min(spec.fighters_max);
    ship.shields = old.shields.min(spec.shields_max);
    ship.armid_mines = old.armid_mines.min(spec.mines_max);
    ship.limpet_mines = old.limpet_mines.min(spec.mines_max - ship.armid_mines);
    ship.beacons = old.beacons;
    ship.genesis = old.genesis.min(spec.genesis_max);
    ship.probes = old.probes;
    ship.density_scanner = true;
    ship.holo_scanner = old.holo_scanner;
    ship.transwarp = old.transwarp && spec.can_transwarp;
    let mut room = ship.holds;
    for i in 0..3 {
        let n = old.cargo[i].min(room);
        ship.cargo[i] = n;
        ship.cargo_cost[i] = if old.cargo[i] > 0 {
            old.cargo_cost[i] * n / old.cargo[i]
        } else {
            0
        };
        room -= n;
    }
    ship.colonists = old.colonists.min(room * COLONISTS_PER_HOLD);
    let pm = ctx.player_mut(pid);
    pm.credits -= net;
    pm.ship = ship;
    if warship {
        // Commissioning a warship is arming yourself: newbie protection ends.
        pm.aggressor = true;
    }
    let pname = pm.name.clone();
    ctx.news(format!("{pname} commissioned a new {} at Stardock.", spec.display));
    ctx.event(
        "ship_purchase",
        Some(rules.stardock_sector),
        vec![pid],
        true,
        format!("{pname} bought a {}.", spec.display),
        json!({"class": a.ship_class}),
    );
    Ok(ctx.finish(
        format!(
            "Purchased a {} for {net} credits after trade-in.{}",
            spec.display,
            if ends_protection {
                " Commissioning a warship ended your newbie protection."
            } else {
                ""
            }
        ),
        json!({"quote": reply_quote, "you": brief(ctx.w, pid)}),
    ))
}

/// Where `pid` may buy `item`: Stardock (markup 0), or its corporation's seat depot (engine
/// 1.40) for fighters, shields and holds at the depot markup. The depot is the corporation's own:
/// the Federation's refusal of wanted pilots does not apply there.
fn equipment_market(w: &World, pid: PlayerId, item: EquipmentItem) -> Res<Option<i64>> {
    let refusal = match require_stardock(w, pid) {
        Ok(()) => return Ok(None),
        Err(e) if e.code == "not_at_stardock" => e,
        Err(e) => return Err(e),
    };
    let Some((seat, _)) = crate::territory::depot_of(w, pid) else {
        return Err(refusal);
    };
    let at_depot = w.players[&pid].sector == seat;
    if at_depot && crate::territory::DEPOT_ITEMS.contains(&item) {
        return Ok(Some(w.rules.territory_depot_markup_bp.max(0)));
    }
    let pct = w.rules.territory_depot_markup_bp as f64 / 100.0;
    let depot = format!("Your corporation's seat depot in sector {seat} sells fighters, shields and holds at Stardock prices +{pct}%.");
    Err(if at_depot {
        Refusal::new(
            "not_at_stardock",
            format!("The seat depot sells only fighters, shields and holds. {}", refusal.message),
        )
        .hint(format!("autopilot_travel destination_sector={}", w.rules.stardock_sector))
    } else {
        let hint = refusal.hint.clone().unwrap_or_default();
        refusal.hint(format!("{hint}. {depot}"))
    })
}

pub fn buy_equipment(ctx: &mut Ctx, pid: PlayerId, a: &BuyEquipmentArgs) -> Res<Outcome> {
    let markup = equipment_market(ctx.w, pid, a.item)?;
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    let spec = rules.ship(p.ship.class).clone();
    let s = &p.ship;
    use EquipmentItem::*;
    // (unit price, max quantity allowed by ship) for countable items.
    let (unit, cap): (i64, i64) = match a.item {
        Fighters => (rules.price_fighter, spec.fighters_max - s.fighters),
        Shields => (rules.price_shield, spec.shields_max - s.shields),
        Holds => (0, spec.holds_max - s.holds),
        ArmidMines => (rules.price_armid, spec.mines_max - s.armid_mines - s.limpet_mines),
        LimpetMines => (rules.price_limpet, spec.mines_max - s.armid_mines - s.limpet_mines),
        Beacons => (rules.price_beacon, 10 - s.beacons),
        GenesisTorpedoes => (rules.price_genesis, spec.genesis_max - s.genesis),
        EtherProbes => (rules.price_probe, PROBES_MAX - s.probes),
        DensityScanner => (rules.price_density_scanner, if s.density_scanner { 0 } else { 1 }),
        HoloScanner => (rules.price_holo_scanner, if s.holo_scanner { 0 } else { 1 }),
        TranswarpDrive => (rules.price_transwarp, if s.transwarp || !spec.can_transwarp { 0 } else { 1 }),
        LimpetRemoval => (rules.price_limpet_removal, if s.limpets.is_empty() { 0 } else { 1 }),
    };
    if cap <= 0 {
        let why = match a.item {
            TranswarpDrive if !spec.can_transwarp => format!("A {} hull cannot mount a TransWarp drive.", spec.display),
            LimpetRemoval => "No limpets are attached to your hull.".to_string(),
            DensityScanner | HoloScanner | TranswarpDrive => "Already installed.".to_string(),
            _ => format!("Your {} is at capacity for that item.", spec.display),
        };
        return Err(Refusal::new("at_capacity", why).hint("A bigger ship carries more (buy_ship dry_run=true to compare)."));
    }
    let perk = crate::careers::price_bp(&p, a.item);
    let mk = 10_000 + markup.unwrap_or(0);
    let price_of = |n: i64| (if a.item == Holds { rules.holds_cost(s.holds, n) } else { unit * n }) * perk / 10_000 * mk / 10_000;
    let mut n = a.quantity.unwrap_or(cap).min(cap);
    if a.quantity.is_none() {
        if a.item == Holds {
            while n > 0 && price_of(n) > p.credits {
                n -= 1;
            }
        } else if markup.is_some() {
            // The most the credits buy at the depot price (exact: price_of rounds down).
            let most = p.credits as i128 * 100_000_000 / (unit.max(1) as i128 * perk.max(1) as i128 * mk as i128);
            n = n.min(most.min(i64::MAX as i128) as i64);
        } else {
            n = n.min(p.credits / unit.max(1));
        }
        if n <= 0 {
            let item = serde_json::to_value(a.item).unwrap().as_str().unwrap_or("item").replace('_', " ");
            return Err(ctx.no_credits(pid, &format!("One {item}"), price_of(1)));
        }
    }
    if n <= 0 {
        return Err(Refusal::new("invalid_args", "Quantity must be positive."));
    }
    let cost = price_of(n);
    if cost > p.credits {
        let item = serde_json::to_value(a.item).unwrap().as_str().unwrap_or("item").replace('_', " ");
        return Err(ctx.no_credits(pid, &format!("{n} {item}"), cost));
    }
    let pm = ctx.player_mut(pid);
    pm.credits -= cost;
    let sh = &mut pm.ship;
    match a.item {
        Fighters => sh.fighters += n,
        Shields => sh.shields += n,
        Holds => sh.holds += n,
        ArmidMines => sh.armid_mines += n,
        LimpetMines => sh.limpet_mines += n,
        Beacons => sh.beacons += n,
        GenesisTorpedoes => sh.genesis += n,
        EtherProbes => sh.probes += n,
        DensityScanner => sh.density_scanner = true,
        HoloScanner => sh.holo_scanner = true,
        TranswarpDrive => sh.transwarp = true,
        LimpetRemoval => sh.limpets.clear(),
    }
    let item = serde_json::to_value(a.item).unwrap();
    let (at, extra) = match markup {
        Some(m) => (
            " at the seat depot",
            json!({"seat_depot": true, "markup_percent": m as f64 / 100.0}),
        ),
        None => ("", json!({})),
    };
    let mut reply = json!({"item": item, "quantity": n, "cost": cost, "you": brief(ctx.w, pid)});
    if let (Some(o), Some(e)) = (reply.as_object_mut(), extra.as_object()) {
        o.extend(e.clone());
    }
    Ok(ctx.finish(
        format!("Bought {n} {} for {cost} credits{at}.", item.as_str().unwrap_or("items")),
        reply,
    ))
}

pub fn bank(ctx: &mut Ctx, pid: PlayerId, a: &BankArgs) -> Res<Outcome> {
    require_stardock_basic(ctx.w, pid)?;
    let p = ctx.player(pid)?.clone();
    match a.action {
        BankAction::Deposit => {
            let n = a.amount.unwrap_or(p.credits).min(p.credits);
            if n <= 0 {
                return Err(Refusal::new("invalid_args", "Nothing to deposit."));
            }
            let pm = ctx.player_mut(pid);
            pm.credits -= n;
            pm.bank += n;
            Ok(ctx.finish(
                format!("Deposited {n}. Bank balance {}.", pm_bank(ctx.w, pid)),
                json!({"bank": pm_bank(ctx.w, pid), "you": brief(ctx.w, pid)}),
            ))
        }
        BankAction::Withdraw => {
            let n = a.amount.unwrap_or(p.bank).min(p.bank);
            if n <= 0 {
                return Err(Refusal::new("invalid_args", "Nothing to withdraw."));
            }
            let pm = ctx.player_mut(pid);
            pm.credits += n;
            pm.bank -= n;
            Ok(ctx.finish(
                format!("Withdrew {n}. Bank balance {}.", pm_bank(ctx.w, pid)),
                json!({"bank": pm_bank(ctx.w, pid), "you": brief(ctx.w, pid)}),
            ))
        }
        BankAction::Transfer => {
            let to = a
                .to_player
                .as_deref()
                .and_then(|t| player::resolve(ctx.w, t))
                .map(|t| t.id)
                .ok_or_else(|| Refusal::new("unknown_player", "to_player not found."))?;
            if to == pid {
                return Err(Refusal::new("invalid_args", "Cannot wire money to yourself."));
            }
            let n = a
                .amount
                .ok_or_else(|| Refusal::new("invalid_args", "amount is required for transfers."))?;
            if n <= 0 || n > p.bank {
                return Err(Refusal::new("insufficient_credits", format!("Your bank balance is {}.", p.bank)));
            }
            ctx.player_mut(pid).bank -= n;
            ctx.player_mut(pid).wired_today -= n;
            ctx.player_mut(to).bank = ctx.w.players[&to].bank.saturating_add(n);
            ctx.player_mut(to).wired_today += n;
            let tname = ctx.w.player_name(to);
            ctx.notify(to, "bank", format!("{} wired {n} credits to your bank account.", p.name));
            ctx.event(
                "transfer",
                None,
                vec![pid, to],
                true,
                format!("{} wired {n} credits to {tname}.", p.name),
                json!({"amount": n}),
            );
            Ok(ctx.finish(format!("Wired {n} credits to {tname}."), json!({"bank": pm_bank(ctx.w, pid)})))
        }
    }
}

fn pm_bank(w: &World, pid: PlayerId) -> i64 {
    w.players[&pid].bank
}

pub fn buy_colonists(ctx: &mut Ctx, pid: PlayerId, a: &BuyColonistsArgs) -> Res<Outcome> {
    require_stardock(ctx.w, pid)?;
    let p = ctx.player(pid)?.clone();
    let price = (ctx.w.rules.colonist_price * crate::careers::colonist_price_bp(ctx.player(pid)?) / 10_000).max(1);
    // Room: free holds plus slack in the colonist hold already in use.
    let room = p.ship.free_holds() * COLONISTS_PER_HOLD + (p.ship.colonist_holds() * COLONISTS_PER_HOLD - p.ship.colonists);
    if room <= 0 {
        return Err(Refusal::new("no_room", "Your holds are full.").hint(format!(
            "Colonists take 1 hold per {COLONISTS_PER_HOLD}; sell or unload cargo first."
        )));
    }
    let n = a.quantity.unwrap_or(room).min(room).min(p.credits / price.max(1));
    if n <= 0 {
        return Err(ctx.no_credits(pid, "One colonist", price));
    }
    let pm = ctx.player_mut(pid);
    pm.credits -= n * price;
    pm.ship.colonists += n;
    Ok(ctx.finish(
        format!("Embarked {n} colonists for {} credits.", n * price),
        json!({"colonists": n, "you": brief(ctx.w, pid)}),
    ))
}

pub fn relief(ctx: &mut Ctx, pid: PlayerId) -> Res<Outcome> {
    require_stardock_basic(ctx.w, pid)?;
    let p = ctx.player(pid)?.clone();
    let rules = ctx.w.rules.clone();
    if p.ship.class != ShipClass::EscapePod {
        return Err(Refusal::new(
            "not_eligible",
            "Federation relief is only for pilots in an escape pod.",
        ));
    }
    let worth = p.credits + p.bank;
    if worth >= rules.relief_threshold {
        return Err(Refusal::new(
            "not_eligible",
            format!(
                "You have {worth} credits; relief is for pilots with under {}.",
                rules.relief_threshold
            ),
        )
        .hint("Buy a ship with buy_ship."));
    }
    let mut ship = player::new_ship(&rules, ShipClass::MerchantCruiser, &format!("{} Relief Cruiser", p.name));
    ship.fighters = 20;
    ship.shields = 20;
    // A relief hull is a loan: it is worth nothing on trade-in.
    ship.relief = true;
    let until = ctx.w.tick + rules.relief_protection_ticks;
    let pm = ctx.player_mut(pid);
    pm.ship = ship;
    pm.credits = pm.credits.max(1_500);
    pm.relief_until = until;
    let hours = rules.relief_protection_ticks / (crate::player::ticks_per_day(&rules).max(24) as u64 / 24);
    Ok(ctx.finish(
        format!(
            "The Federation issues you a basic Merchant Cruiser (a relief hull: no trade-in value) and 1,500 credits of seed money. Skarn will leave you alone for about {hours} hours unless you attack; arm before then."
        ),
        json!({"you": brief(ctx.w, pid)}),
    ))
}
