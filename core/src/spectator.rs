//! Omniscient live projection for the sysop console (never public; see `public`).

use serde_json::{Value, json};

use crate::model::*;
use crate::player;

/// Static topology: sent once per client.
pub fn map(w: &World) -> Value {
    let sectors: Vec<Value> = w
        .sectors
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "x": s.x, "y": s.y, "z": s.z, "w": s.warps, "fed": s.fedspace, "neb": s.nebula,
                "port": s.port.as_ref().map(|p| if p.stardock { "STARDOCK".to_string() } else { p.code() }),
                "bm": s.port.as_ref().is_some_and(|p| p.black_market),
            })
        })
        .collect();
    json!({"sectors": sectors, "seed": w.seed})
}

/// Dynamic state: refreshed periodically.
pub fn state(w: &World) -> Value {
    let players: Vec<Value> = w
        .players
        .values()
        .map(|p| {
            json!({
                "id": p.id, "name": p.name, "kind": p.kind, "model": p.model, "harness": p.harness, "autonomous": p.autonomous,
                "announcements_unacked": w.announcements.iter().filter(|a| a.id > p.announcements_seen).count(),
                "operator_unread": p.inbox.iter().filter(|n| n.kind == "sysop" && !n.read).count(), "sector": p.sector,
                "ship": p.ship.class, "fighters": p.ship.fighters, "shields": p.ship.shields,
                "credits": p.credits, "net_worth": if p.kind == PlayerKind::Npc { 0 } else { player::net_worth(w, p.id) },
                "corp": p.corp.and_then(|c| w.corps.get(&c)).map(|c| c.name.clone()),
                "npc": p.npc.as_ref().map(|b| b.role), "alignment": p.alignment, "rank": player::rank(p.experience),
                "ap": p.ap, "kills": p.stats.kills, "deaths": p.stats.deaths, "bank": p.bank,
                "earned": if p.kind == PlayerKind::Npc { 0 } else { player::net_worth(w, p.id) - player::endowment(w, p) },
                "protected": player::is_newbie(w, p.id), "newbie_protected": player::is_newbie(w, p.id),
                "muted_until": p.muted_until, "aggressor": p.aggressor, "banned": p.banned,
                "created_tick": p.created_tick, "last_action_tick": p.last_action_tick,
                "plan_tick": p.plan_tick, "has_plan": p.notes.contains_key(crate::social::PLAN_KEY), "plan_due": p.npc.is_none() && crate::query::plan_view(p)["due"] == true,
                "coached": crate::owner::coached(p), "label": p.owner.orders.public_label,
                "documented": crate::identity::documented(w, p.id),
                "identity": w.identities.get(&p.id).and_then(crate::identity::Identity::current).map(|v| json!({
                    "age": v.age, "homeworld": v.homeworld, "bio": v.bio, "why": v.why, "version": v.version, "updated_tick": v.tick})),
                "career_path": p.profession, "win_condition": p.win_condition,
                "scores": if p.kind == PlayerKind::Npc { Value::Null } else { crate::careers::scores_json(w, p) },
            })
        })
        .collect();
    let planets: Vec<Value> = w
        .planets
        .values()
        .map(|pl| {
            json!({
                "id": pl.id, "name": pl.name, "sector": pl.sector, "class": pl.class, "owner": pl.owner.map(|o| w.player_name(o)),
                "citadel": pl.citadel, "colonists": pl.total_colonists(), "fighters": pl.fighters,
            })
        })
        .collect();
    let defenses: Vec<Value> = w
        .sectors
        .iter()
        .filter(|s| s.fighters.is_some() || s.armid.is_some())
        .map(|s| {
            json!({
                "sector": s.id,
                "fighters": s.fighters.as_ref().map(|f| json!({"owner": w.player_name(f.owner), "count": f.count, "mode": f.mode,
                    "toll": (f.mode == FighterMode::Toll).then(|| f.toll(w.rules.toll_per_fighter)),
                    "shares": f.shares.iter().map(|(p, n)| (w.player_name(*p), *n)).collect::<std::collections::BTreeMap<_, _>>()})),
                "mines": s.armid.as_ref().map(|m| m.count),
            })
        })
        .collect();
    let territories = crate::territory::all(w);
    let corps: Vec<Value> = w
        .corps
        .values()
        .map(|c| {
            let territory = territories.get(&c.id).map(|t| crate::territory::member_view(w, c, t));
            json!({
                "id": c.id, "name": c.name, "members": c.members.iter().map(|m| w.player_name(*m)).collect::<Vec<_>>(),
                "member_ids": c.members, "ceo": w.player_name(c.ceo), "ceo_id": c.ceo, "motto": c.motto, "created_tick": c.created_tick,
                "invites": c.invites.iter().map(|m| w.player_name(*m)).collect::<Vec<_>>(), "treasury": c.treasury,
                "free_funds": c.free_funds(),
                "deposits": c.deposits.iter().map(|(p, n)| (w.player_name(*p), *n)).collect::<std::collections::BTreeMap<_, _>>(),
                "net_worth": c.members.iter().map(|m| player::net_worth(w, *m)).sum::<i64>(),
                "government": crate::government::sysop_view(w, c), "record": c.record,
                "seat": c.seat, "seat_set_tick": c.seat_set_tick, "territory": territory,
            })
        })
        .collect();
    let radio: Vec<Value> = w.radio.iter().rev().take(COMMS_KEEP).map(|m| radio_json(w, m)).collect();
    let reg = crate::diplomacy::registry(w, 60);
    json!({
        "tick": w.tick, "seq": w.seq, "season": w.season, "sectors": w.sector_count(),
        "intermission_ticks": w.rules.season_intermission_ticks, "tick_ms": w.rules.tick_ms,
        "hall_of_fame": w.hall_of_fame.iter().rev().take(10).collect::<Vec<_>>(),
        "players": players, "planets": planets, "defenses": defenses, "corps": corps,
        "news": w.news.iter().rev().take(40).collect::<Vec<_>>(),
        "radio": radio,
        "market": w.market_history.iter().rev().take(120).rev().collect::<Vec<_>>(),
        "leaderboard": crate::query::leaderboard(w, &crate::query::LeaderboardArgs { limit: Some(20) }),
        "treaties": reg["treaties"], "wars": reg["wars"], "intents": reg["intents"], "combat_reports": reg["combat_reports"],
        "contracts": reg["contracts"], "champions": crate::careers::champions(w), "concessions": reg["concessions"],
        "feedback_open": crate::feedback::open_count(w), "feedback_open_max_id": crate::feedback::open_max_id(w),
        // The improve board at a glance (engine 1.55): the console's alerts and badge read it.
        "improve": crate::improve::summary(w),
        "chronicle": {
            "pages": w.chronicle.pages.len(),
            "pitched": w.chronicle.productions.values().filter(|x| x.status == crate::chronicle::Status::Pitched).map(|x| x.id).collect::<Vec<_>>(),
            "reported": w.chronicle.pages.values().filter(|p| p.struck.is_none() && !p.reports.is_empty()).map(|p| p.id).collect::<Vec<_>>(),
        },
        "identity_required": w.rules.identity_required,
        "feedback_recent": w.feedback.iter().rev().take(FEEDBACK_RECENT).map(|f| crate::feedback::to_json(w, f)).collect::<Vec<_>>(),
        "constitution": crate::constitution::sysop_view(w),
    })
}

/// The radio history the world still holds (all channels, newest first, at most `limit`),
/// for the sysop Comms workspace.
pub fn radio(w: &World, limit: usize) -> Value {
    json!(w.radio.iter().rev().take(limit).map(|m| radio_json(w, m)).collect::<Vec<_>>())
}

/// How many recent feedback reports (any status) the sysop state carries.
const FEEDBACK_RECENT: usize = 20;

/// How many recent radio messages (all channels) the spectator state carries.
const COMMS_KEEP: usize = 200;

/// One radio message, flattened for the spectator Comms view. The spectator is
/// omniscient, so corp and private traffic is included with names resolved.
fn radio_json(w: &World, m: &RadioMsg) -> Value {
    let sender = w.players.get(&m.from);
    let (channel, to, to_id, corp, corp_id) = match &m.channel {
        Channel::Public => ("public", None, None, None, None),
        Channel::Corp(c) => {
            let name = w.corps.get(c).map(|k| k.name.clone()).unwrap_or_else(|| format!("Corp #{c}"));
            ("corp", None, None, Some(name), Some(*c))
        }
        Channel::Private(to) => ("private", Some(w.player_name(*to)), Some(*to), None, None),
    };
    json!({
        "id": m.id, "tick": m.tick, "at_ms": m.at_ms, "channel": channel, "text": m.text,
        "from": m.from_name, "from_id": m.from, "system": m.from == 0,
        "from_model": sender.and_then(|p| p.model.clone()),
        "from_corp": sender.and_then(|p| p.corp).and_then(|c| w.corps.get(&c)).map(|c| c.name.clone()),
        "to": to, "to_id": to_id, "corp": corp, "corp_id": corp_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::galaxy::{GenesisConfig, generate};

    #[test]
    fn spectator_radio_covers_every_channel() {
        let mut w = generate(&GenesisConfig::new(3, 40));
        let msg = |id, from: PlayerId, from_name: &str, channel| RadioMsg {
            id,
            tick: 5,
            at_ms: 0,
            from,
            from_name: from_name.into(),
            channel,
            text: format!("m{id}"),
        };
        w.radio.push(msg(1, 0, "Federation Relay", Channel::Public));
        w.radio.push(msg(2, 1, "A", Channel::Private(9_999)));
        w.radio.push(msg(3, 1, "A", Channel::Corp(999)));
        let v = state(&w);
        let r = v["radio"].as_array().unwrap();
        assert_eq!(r.len(), 3);
        // Newest first.
        assert_eq!(r[0]["channel"], "corp");
        assert_eq!(r[0]["corp"], "Corp #999");
        assert_eq!(r[1]["channel"], "private");
        assert_eq!(r[1]["to"], "#9999");
        assert_eq!(r[1]["to_id"], 9_999);
        assert_eq!(r[2]["system"], true);
        assert_eq!(r[2]["text"], "m1");
        for i in 0..300 {
            w.radio.push(msg(10 + i, 1, "A", Channel::Public));
        }
        assert_eq!(state(&w)["radio"].as_array().unwrap().len(), COMMS_KEEP);
    }
}
