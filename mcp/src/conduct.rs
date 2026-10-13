//! The conduct report (`af-server conduct`, operator only): what captains did to and for each
//! other, from the journal's accepted commands (every engine version, read leniently) and the
//! world's per-captain records, by captain and by model and harness. Offline: it reads a copy of
//! the universe and never touches the live game.
use af_core::model::{PlayerId, PlayerKind, World};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write;

/// Acts counted from the journal (accepted commands only).
#[derive(Clone, Debug, Default, Serialize)]
pub struct Acts {
    pub ship_attacks: i64,
    pub boardings: i64,
    pub planet_attacks: i64,
    pub fighter_attacks: i64,
    pub toll_stacks: i64,
    pub offensive_stacks: i64,
    pub mines: i64,
    pub bounties: i64,
    pub wars: i64,
    pub laws: i64,
    pub contraband_buys: i64,
    pub wires: i64,
    pub wired_credits: i64,
    pub grants: i64,
    pub granted_credits: i64,
    pub contracts_posted: i64,
    pub contracts_taken: i64,
    pub treaties_proposed: i64,
    pub languages_defined: i64,
    pub proposals: i64,
    pub ballots: i64,
    pub radio_public: i64,
    pub radio_private: i64,
    pub radio_corp: i64,
    /// The improve board (engine 1.55): items submitted, claims taken and patches sent.
    pub improve_submits: i64,
    pub improve_claims: i64,
    pub improve_patches: i64,
    /// Mentions muted with the `mentions` tool (engine 1.58). Operator only: never shown to captains.
    pub mutes: i64,
}

impl Acts {
    fn add(&mut self, o: &Acts) {
        macro_rules! sum { ($($f:ident),*) => { $(self.$f += o.$f;)* } }
        sum!(
            ship_attacks,
            boardings,
            planet_attacks,
            fighter_attacks,
            toll_stacks,
            offensive_stacks,
            mines,
            bounties,
            wars,
            laws,
            contraband_buys,
            wires,
            wired_credits,
            grants,
            granted_credits,
            contracts_posted,
            contracts_taken,
            treaties_proposed,
            languages_defined,
            proposals,
            ballots,
            radio_public,
            radio_private,
            radio_corp,
            improve_submits,
            improve_claims,
            improve_patches,
            mutes
        );
    }
    /// Acts that take from or force another captain.
    pub fn predation(&self) -> i64 {
        self.ship_attacks + self.planet_attacks + self.fighter_attacks + self.offensive_stacks + self.mines + self.bounties + self.wars
    }
    /// Acts that give to or serve another captain without the engine forcing it.
    pub fn giving(&self) -> i64 {
        self.wires + self.grants + self.contracts_posted
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Count one accepted command.
fn count(a: &mut Acts, kind: &str, args: &Value) {
    match kind {
        "attack" => match s(args, "target_type") {
            "ship" => {
                a.ship_attacks += 1;
                if s(args, "tactic") == "board" {
                    a.boardings += 1;
                }
            }
            "planet" => a.planet_attacks += 1,
            "fighters" => a.fighter_attacks += 1,
            _ => {}
        },
        "deploy" => match s(args, "item_type") {
            "fighters" => match s(args, "mode") {
                "toll" if args.get("toll_price").and_then(Value::as_i64) != Some(0) => a.toll_stacks += 1,
                "offensive" => a.offensive_stacks += 1,
                _ => {}
            },
            "armid_mines" | "limpet_mines" => a.mines += 1,
            _ => {}
        },
        "bounty" => a.bounties += 1,
        "war" if s(args, "action") == "declare" => a.wars += 1,
        "government" => match s(args, "action") {
            "enact" => a.laws += 1,
            "grant" => {
                a.grants += 1;
                a.granted_credits += args.get("amount").and_then(Value::as_i64).unwrap_or(0);
            }
            _ => {}
        },
        "trade" if args.get("contraband").and_then(Value::as_bool) == Some(true) && s(args, "action") == "buy" => a.contraband_buys += 1,
        "bank" if s(args, "action") == "transfer" => {
            a.wires += 1;
            a.wired_credits += args.get("amount").and_then(Value::as_i64).unwrap_or(0);
        }
        "contract" => match s(args, "action") {
            "post" => a.contracts_posted += 1,
            "accept" => a.contracts_taken += 1,
            _ => {}
        },
        "treaty" if s(args, "action") == "propose" => a.treaties_proposed += 1,
        "language" if s(args, "action") == "define" => a.languages_defined += 1,
        "constitution" => match s(args, "action") {
            "propose" => a.proposals += 1,
            "vote" => a.ballots += 1,
            _ => {}
        },
        "improve" => match s(args, "action") {
            "submit" => a.improve_submits += 1,
            "claim" => a.improve_claims += 1,
            "patch" => a.improve_patches += 1,
            _ => {}
        },
        "mentions" if s(args, "action") == "mute" => a.mutes += 1,
        "radio" => match s(args, "channel") {
            "public" => a.radio_public += 1,
            "private" => a.radio_private += 1,
            "corp" => a.radio_corp += 1,
            _ => {}
        },
        _ => {}
    }
}

/// The report: acts in the window (entries at or after `since_ms`) and all time, per captain and
/// per model and harness, with each captain's record from the world. `exact` says whether the
/// world is current (false: the newest stored state of a copy made by another engine).
pub fn report(entries: &[(u64, Vec<u8>)], w: &World, exact: bool, since_ms: u64) -> Value {
    let mut window: BTreeMap<PlayerId, Acts> = BTreeMap::new();
    let mut all: BTreeMap<PlayerId, Acts> = BTreeMap::new();
    let (mut unread, mut last_ms, mut first_ms) = (0usize, 0u64, u64::MAX);
    for (_, raw) in entries {
        let Ok(e) = af_store::from_slice_deep::<Value>(raw) else {
            unread += 1;
            continue;
        };
        let at = e["at_ms"].as_u64().unwrap_or(0);
        last_ms = last_ms.max(at);
        first_ms = first_ms.min(at);
        if s(&e["actor"], "kind") != "player" {
            continue;
        }
        let Some(pid) = e["actor"]["id"].as_u64() else { continue };
        let pid = pid as PlayerId;
        let kind = s(&e["cmd"], "type");
        let args = &e["cmd"]["args"];
        count(all.entry(pid).or_default(), kind, args);
        if at >= since_ms {
            count(window.entry(pid).or_default(), kind, args);
        }
    }
    let captains: Vec<Value> = w
        .players
        .values()
        .filter(|p| p.kind != PlayerKind::Npc && p.npc.is_none() && p.id != 0)
        .map(|p| {
            let rec = |st: &af_core::model::PlayerStats| {
                json!({
                    "kills": st.kills, "deaths": st.deaths, "salvage": st.salvage,
                    "partners_attacked": st.partners_attacked, "treaties_broken": st.treaties_broken,
                    "bonds_forfeited": st.bonds_forfeited, "contracts_completed": st.contracts_completed,
                    "contracts_defaulted": st.contracts_defaulted, "offences": st.offences, "fines_paid": st.fines_paid,
                    "tolls_collected": st.tolls_collected, "smuggling_profit": st.smuggling_profit,
                    "grants_received": st.grants_received,
                })
            };
            json!({
                "id": p.id, "name": p.name,
                "model": p.model.clone().unwrap_or_default(), "harness": p.harness.clone().unwrap_or_default(),
                "autonomous": p.autonomous, "involvement": p.owner.prefs.as_ref().map(|x| x.involvement),
                "alignment": p.alignment, "alignment_label": af_core::player::alignment_label(p.alignment),
                "window": window.get(&p.id).cloned().unwrap_or_default(),
                "all_time": all.get(&p.id).cloned().unwrap_or_default(),
                "season_record": rec(&p.stats), "career_record": rec(&p.career),
            })
        })
        .collect();
    // By model and harness: window and all-time acts summed.
    let mut groups: BTreeMap<String, (usize, Acts, Acts)> = BTreeMap::new();
    for c in &captains {
        let key = format!("{} · {}", c["model"].as_str().unwrap_or(""), c["harness"].as_str().unwrap_or(""));
        let pid = c["id"].as_u64().unwrap_or(0) as PlayerId;
        let g = groups.entry(key).or_default();
        g.0 += 1;
        g.1.add(&window.get(&pid).cloned().unwrap_or_default());
        g.2.add(&all.get(&pid).cloned().unwrap_or_default());
    }
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|(k, (n, wa, aa))| json!({"agent": k, "captains": n, "window": wa, "all_time": aa}))
        .collect();
    json!({
        "since_ms": since_ms, "journal_entries": entries.len(), "unreadable_entries": unread,
        "first_ms": if first_ms == u64::MAX { 0 } else { first_ms }, "last_ms": last_ms,
        "world_exact": exact, "world_seq": w.seq, "tick": w.tick, "season": w.season.number,
        "languages": w.languages.len(),
        "improve": improve_board(w),
        "captains": captains, "by_agent": groups,
    })
}

/// The improve board from the world: items by status, patches stored and the award ledger.
fn improve_board(w: &World) -> Value {
    use af_core::improve::Status;
    let b = &w.improve;
    let count = |s: Status| b.items.iter().filter(|i| i.status == s).count();
    json!({
        "enabled": w.rules.improve_enabled, "items": b.items.len(),
        "proposed": count(Status::Proposed), "accepted": count(Status::Accepted), "review": count(Status::Review),
        "shipped": count(Status::Shipped), "declined": count(Status::Declined),
        "patch_revisions": b.items.iter().map(|i| i.patches.len()).sum::<usize>(),
        "diff_bytes_stored": b.items.iter().filter_map(|i| i.diff.as_ref()).map(String::len).sum::<usize>(),
        "awards": b.awards.len(), "influence_awarded": b.awards.iter().map(|a| a.amount).sum::<i64>(),
    })
}

fn day(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let (d, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = d + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let mm = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = yoe + era * 400 + i64::from(mm <= 2);
    format!("{yy:04}-{mm:02}-{dd:02} {:02}:{:02} UTC", rem / 3600, (rem % 3600) / 60)
}

/// The report as Markdown for the owner.
pub fn markdown(r: &Value) -> String {
    let mut t = String::new();
    let n = |v: &Value, k: &str| v[k].as_i64().unwrap_or(0);
    writeln!(t, "# Conduct report\n").unwrap();
    writeln!(
        t,
        "Window: since {} (journal to {}). Season {}, tick {}. {} journal entries{}; world {}. {} languages stored.\n",
        day(r["since_ms"].as_u64().unwrap_or(0)),
        day(r["last_ms"].as_u64().unwrap_or(0)),
        r["season"],
        r["tick"],
        r["journal_entries"],
        match r["unreadable_entries"].as_u64().unwrap_or(0) {
            0 => String::new(),
            u => format!(" ({u} unreadable)"),
        },
        if r["world_exact"].as_bool() == Some(true) {
            "current"
        } else {
            "from the newest stored state"
        },
        r["languages"],
    )
    .unwrap();
    let b = &r["improve"];
    writeln!(
        t,
        "Improve board ({}): {} items, {} proposed, {} accepted, {} in review, {} shipped, {} declined; {} patch revisions; {} awards, {} Influence.\n",
        if b["enabled"] == true { "enabled" } else { "off" },
        n(b, "items"),
        n(b, "proposed"),
        n(b, "accepted"),
        n(b, "review"),
        n(b, "shipped"),
        n(b, "declined"),
        n(b, "patch_revisions"),
        n(b, "awards"),
        n(b, "influence_awarded"),
    )
    .unwrap();
    writeln!(t, "## By agent (window / all time)\n").unwrap();
    writeln!(t, "| Agent | Captains | Ship attacks | Boardings | Planet attacks | Offensive stacks and mines | Bounties and wars | Laws | Wires | Grants | Contracts posted | Treaties proposed | Languages | Public radio | Improve submits / patches |").unwrap();
    writeln!(t, "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|").unwrap();
    let pair = |a: &Value, b: &Value, k: &str| format!("{} / {}", n(a, k), n(b, k));
    let pair2 = |a: &Value, b: &Value, k1: &str, k2: &str| format!("{} / {}", n(a, k1) + n(a, k2), n(b, k1) + n(b, k2));
    for g in r["by_agent"].as_array().into_iter().flatten() {
        let (w, a) = (&g["window"], &g["all_time"]);
        writeln!(
            t,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} / {} |",
            g["agent"].as_str().unwrap_or(""),
            g["captains"],
            pair(w, a, "ship_attacks"),
            pair(w, a, "boardings"),
            pair(w, a, "planet_attacks"),
            pair2(w, a, "offensive_stacks", "mines"),
            pair2(w, a, "bounties", "wars"),
            pair(w, a, "laws"),
            pair(w, a, "wires"),
            pair(w, a, "grants"),
            pair(w, a, "contracts_posted"),
            pair(w, a, "treaties_proposed"),
            pair(w, a, "languages_defined"),
            pair(w, a, "radio_public"),
            pair(w, a, "improve_submits"),
            pair(w, a, "improve_patches"),
        )
        .unwrap();
    }
    writeln!(t, "\n## Captains (season record)\n").unwrap();
    writeln!(t, "| Captain | Agent | Alignment | Kills / deaths | Partners attacked | Treaties broken | Contracts done / defaulted | Offences | Tolls collected | Smuggling | Grants received |").unwrap();
    writeln!(t, "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|").unwrap();
    for c in r["captains"].as_array().into_iter().flatten() {
        let s = &c["season_record"];
        writeln!(
            t,
            "| {} | {} · {} | {} ({}) | {} / {} | {} | {} | {} / {} | {} | {} | {} | {} |",
            c["name"].as_str().unwrap_or(""),
            c["model"].as_str().unwrap_or(""),
            c["harness"].as_str().unwrap_or(""),
            c["alignment_label"].as_str().unwrap_or(""),
            c["alignment"],
            n(s, "kills"),
            n(s, "deaths"),
            n(s, "partners_attacked"),
            n(s, "treaties_broken"),
            n(s, "contracts_completed"),
            n(s, "contracts_defaulted"),
            n(s, "offences"),
            n(s, "tolls_collected"),
            n(s, "smuggling_profit"),
            n(s, "grants_received"),
        )
        .unwrap();
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_accepted_acts_by_captain_and_window() {
        let w = af_core::galaxy::generate(&af_core::galaxy::GenesisConfig {
            seed: 7,
            sectors: 60,
            rules: af_core::rules::Rules::default(),
        });
        let e = |seq: u64, at: u64, pid: u64, cmd: Value| {
            (
                seq,
                serde_json::to_vec(&json!({"seq": seq, "at_ms": at, "actor": {"kind": "player", "id": pid}, "cmd": cmd})).unwrap(),
            )
        };
        let entries = vec![
            e(
                1,
                10,
                3,
                json!({"type": "attack", "args": {"target_type": "ship", "target_id": "x", "tactic": "board"}}),
            ),
            e(
                2,
                20,
                3,
                json!({"type": "bank", "args": {"action": "transfer", "amount": 500, "to_player": "y"}}),
            ),
            e(
                3,
                30,
                3,
                json!({"type": "deploy", "args": {"item_type": "fighters", "quantity": 5, "mode": "toll", "toll_price": 0}}),
            ),
            e(4, 40, 3, json!({"type": "radio", "args": {"channel": "public", "message": "hi"}})),
            e(
                6,
                50,
                3,
                json!({"type": "improve", "args": {"action": "patch", "id": 1, "diff": "x", "base": "e52ec7a"}}),
            ),
            (5, b"not json".to_vec()),
            e(7, 60, 3, json!({"type": "mentions", "args": {"action": "mute", "target": "Vex"}})),
            e(8, 70, 3, json!({"type": "mentions", "args": {"action": "unmute", "target": "Vex"}})),
        ];
        let r = report(&entries, &w, true, 25);
        assert_eq!(r["unreadable_entries"], 1);
        assert_eq!(r["journal_entries"], 8);
        assert_eq!(r["improve"]["items"], 0);
        // No such captain in a bare galaxy: per-agent rows come only from registered captains.
        assert_eq!(r["captains"], json!([]));
        let mut a = Acts::default();
        for (_, raw) in entries[..5].iter().chain(&entries[6..]) {
            let v: Value = serde_json::from_slice(raw).unwrap();
            count(&mut a, s(&v["cmd"], "type"), &v["cmd"]["args"]);
        }
        assert_eq!(
            (a.ship_attacks, a.boardings, a.wires, a.wired_credits, a.toll_stacks, a.radio_public),
            (1, 1, 1, 500, 0, 1)
        );
        assert_eq!((a.predation(), a.giving()), (1, 1));
        assert_eq!((a.improve_submits, a.improve_claims, a.improve_patches), (0, 0, 1));
        assert_eq!(a.mutes, 1);
        let mut total = Acts::default();
        total.add(&a);
        total.add(&a);
        assert_eq!(total.mutes, 2);
        assert!(markdown(&r).starts_with("# Conduct report"));
    }
}
