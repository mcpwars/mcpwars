//! The public view: live, and safe for anyone to read, captains included.
//!
//! The rule: the public view carries only
//! * what every captain can already learn through public in-game tools
//!   (`leaderboard`, `view_registry`, public radio, `game_info`);
//! * news headlines, with sector numbers removed;
//! * aggregates (counts, totals, market averages);
//! * a map of the galaxy's shape, with activity binned into coarse regions;
//! * the fleet an hour late (`fleet_view`): every active captain's star and last
//!   jump as coordinates, coloured by who sets its strategy, with no names, ids
//!   or sector numbers. The server keeps the frames (outside the journal) and
//!   serves only ones at least an hour old, so watching never beats exploring;
//! * anonymous scoreboard rows (`scoreboard`): model, harness, who sets the
//!   strategy, and what the captain earned this season.
//!
//! It never carries live ship, planet, defense or Skarn positions at sector or
//! coordinate precision; lanes, sector ids or which star is which sector;
//! per-captain credits, bank, turns, cargo, fighters or shields; corp or private
//! radio; inboxes, notes or owner orders; or individual trades.
//! Everything else is for the sysop console only (`spectator`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::Event;
use crate::model::*;
use crate::query;

/// Target size of a map region. Large enough that knowing which region a
/// captain is in does not help find them.
pub const SECTORS_PER_REGION: usize = 40;

/// Event kinds that reach the public feed (text cleaned of sector numbers).
/// Left out on purpose: `move`, `trade`, `deploy`, `beacon` and
/// `planet_created` (positions), `transfer` (private money), and corp and
/// private radio.
const PUBLIC_EVENTS: &[&str] = &[
    "player_joined",
    "galaxy_event",
    "identity",
    "career",
    "corp_created",
    "diplomacy",
    "contract",
    "bounty",
    "combat",
    "boarding",
    "ship_destroyed",
    "planet_captured",
    "planet_claimed",
    "citadel_started",
    "citadel_complete",
    "concession",
    "customs",
    "ship_purchase",
    "npc_spawn",
    "skarn_raid",
    "fed_intercept",
    "radio",
    "galaxy_grew",
    "season_over",
    "big_bang",
    "fresh_start",
    "constitution",
];

/// Remove sector numbers from a line of text: "attacked Vex in sector 412."
/// becomes "attacked Vex.", and any other "sector 412" becomes "a sector".
pub fn scrub(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if lower[i..].starts_with("sector") {
            let mut j = i + "sector".len();
            while j < b.len() && b[j] == b' ' {
                j += 1;
            }
            let digits = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits {
                let trimmed = out.trim_end_matches(' ');
                let lo = trimmed.to_ascii_lowercase();
                let prep = [" in", " at", " near", " to", " from", " of"]
                    .iter()
                    .find(|p| lo.ends_with(**p))
                    .map(|p| p.len());
                if let Some(n) = prep.filter(|_| out.len() > trimmed.len()) {
                    out.truncate(trimmed.len() - n);
                } else {
                    out.push_str("a sector");
                }
                i = j;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The in-game registry as the public sees it: the same entries every captain gets from
/// `view_registry`, with sector ids dropped and sector numbers scrubbed from every text.
fn public_registry(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(k, _)| k != "sector")
                .map(|(k, x)| (k, public_registry(x)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(public_registry).collect()),
        Value::String(t) => Value::String(scrub(&t)),
        x => x,
    }
}

/// The public form of an engine event, or None if it stays private.
pub fn event(e: &Event) -> Option<Event> {
    if !PUBLIC_EVENTS.contains(&e.kind.as_str()) {
        return None;
    }
    Some(Event {
        tick: e.tick,
        kind: e.kind.clone(),
        text: scrub(&e.text),
        sector: None,
        players: e.players.clone(),
        public: true,
        data: Value::Null,
    })
}

/// Coarse map regions: a grid over the galaxy's coordinates, sized so each
/// cell holds about `SECTORS_PER_REGION` sectors. Empty cells are dropped.
pub struct Regions {
    /// Region index per sector (by position in `World::sectors`).
    of_sector: Vec<usize>,
    /// Region centres (mean of member coordinates: x, y, z) and sizes.
    centers: Vec<(f64, f64, f64, usize)>,
}

impl Regions {
    pub fn new(w: &World) -> Regions {
        let n = w.sectors.len();
        if n == 0 {
            return Regions {
                of_sector: vec![],
                centers: vec![],
            };
        }
        let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for s in &w.sectors {
            x0 = x0.min(s.x);
            x1 = x1.max(s.x);
            y0 = y0.min(s.y);
            y1 = y1.max(s.y);
        }
        let (wd, ht) = ((x1 - x0).max(1e-9), (y1 - y0).max(1e-9));
        let cells = n.div_ceil(SECTORS_PER_REGION).max(1);
        let cols = ((cells as f64 * wd / ht).sqrt().round() as usize).clamp(1, cells);
        let rows = cells.div_ceil(cols);
        let cell = |s: &Sector| {
            let cx = (((s.x - x0) / wd * cols as f64) as usize).min(cols - 1);
            let cy = (((s.y - y0) / ht * rows as f64) as usize).min(rows - 1);
            cy * cols + cx
        };
        // Compact non-empty cells into region ids, in grid order.
        let mut index = vec![usize::MAX; cols * rows];
        let mut sums: Vec<(f64, f64, f64, usize)> = vec![];
        let mut of_sector = Vec::with_capacity(n);
        let raw: Vec<usize> = w.sectors.iter().map(cell).collect();
        let mut used: Vec<usize> = raw.clone();
        used.sort_unstable();
        used.dedup();
        for c in used {
            index[c] = sums.len();
            sums.push((0.0, 0.0, 0.0, 0));
        }
        for (s, c) in w.sectors.iter().zip(&raw) {
            let r = index[*c];
            of_sector.push(r);
            sums[r].0 += s.x;
            sums[r].1 += s.y;
            sums[r].2 += s.z;
            sums[r].3 += 1;
        }
        let centers = sums
            .into_iter()
            .map(|(x, y, z, k)| (x / k as f64, y / k as f64, round1(z / k as f64), k))
            .collect();
        Regions { of_sector, centers }
    }

    pub fn len(&self) -> usize {
        self.centers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.centers.is_empty()
    }

    /// The region of a sector (None for an unknown sector).
    pub fn of(&self, sid: SectorId) -> Option<usize> {
        (sid as usize).checked_sub(1).and_then(|i| self.of_sector.get(i).copied())
    }

    pub fn center(&self, r: usize) -> (f64, f64, f64) {
        let (x, y, z, _) = self.centers[r];
        (x, y, z)
    }
}

/// Activity per region, keyed by region id.
#[derive(Default, Clone)]
struct Activity {
    captains: i64,
    raiders: i64,
    colonies: i64,
    citadels: i64,
    fighters: i64,
}

fn activity(w: &World, regions: &Regions) -> Vec<Activity> {
    let mut a = vec![Activity::default(); regions.len()];
    for p in w.players.values() {
        let Some(r) = regions.of(p.sector) else { continue };
        if p.kind != PlayerKind::Npc {
            a[r].captains += 1;
        } else if p.npc.as_ref().is_some_and(|b| b.role == NpcRole::Skarn) {
            a[r].raiders += 1;
        }
    }
    for pl in w.planets.values().filter(|pl| pl.owner.is_some()) {
        if let Some(r) = regions.of(pl.sector) {
            a[r].colonies += 1;
            a[r].citadels += i64::from(pl.citadel > 0);
        }
    }
    for s in &w.sectors {
        if let (Some(f), Some(r)) = (&s.fighters, regions.of(s.id)) {
            a[r].fighters += f.count;
        }
    }
    a
}

/// Static map: every star at its coordinates in shuffled order (positions mean
/// nothing to captains, who never see coordinates; there are no sector ids or
/// lanes to tie a star to a sector), plus the region centres.
pub fn map(w: &World) -> Value {
    let regions = Regions::new(w);
    json!({
        "stars": stars(w),
        "regions": (0..regions.len()).map(|r| {
            let (x, y, z, n) = regions.centers[r];
            json!({"id": r, "x": x, "y": y, "z": z, "sectors": n})
        }).collect::<Vec<_>>(),
    })
}

/// `[x, y, kind, z]` per star, shuffled: kind 0 empty, 1 port, 2 FedSpace, 3 Stardock (z last,
/// so older pages that read `[x, y, kind]` keep working).
fn stars(w: &World) -> Vec<Value> {
    let mut rng = crate::rng::Rng::new(w.seed ^ 0x5EED_CA5E);
    let mut stars: Vec<Value> = w
        .sectors
        .iter()
        .map(|s| {
            let kind = if s.port.as_ref().is_some_and(|p| p.stardock) {
                3
            } else if s.fedspace {
                2
            } else if s.port.is_some() {
                1
            } else {
                0
            };
            json!([s.x, s.y, kind, s.z])
        })
        .collect();
    rng.shuffle(&mut stars);
    stars
}

fn radio_json(w: &World, m: &RadioMsg) -> Value {
    let sender = w.players.get(&m.from);
    json!({
        // Captains see sector numbers on the radio; spectators never do (bulletins, player chatter).
        "id": m.id, "tick": m.tick, "channel": "public", "text": scrub(&m.text),
        "from": m.from_name, "from_id": m.from, "system": m.from == 0,
        "from_model": sender.and_then(|p| p.model.clone()),
        "from_corp": sender.and_then(|p| p.corp).and_then(|c| w.corps.get(&c)).map(|c| c.name.clone()),
    })
}

fn public_radio(w: &World) -> impl DoubleEndedIterator<Item = &RadioMsg> {
    w.radio.iter().filter(|m| m.channel == Channel::Public)
}

fn season_json(w: &World) -> Value {
    let len = (w.season.ends_tick - w.season.started_tick).max(1);
    json!({
        "number": w.season.number, "started_tick": w.season.started_tick, "ends_tick": w.season.ends_tick,
        "finished": w.season.finished, "intermission_ticks": w.rules.season_intermission_ticks,
        "progress": ((w.tick.saturating_sub(w.season.started_tick)) as f64 / len as f64).min(1.0),
        "days_left": (w.season.ends_tick.saturating_sub(w.tick) * w.rules.tick_ms) as f64 / 86_400_000.0,
    })
}

/// The live public state.
pub fn state(w: &World) -> Value {
    let regions = Regions::new(w);
    let act = activity(w, &regions);
    let mut captains: Vec<(i64, Value)> = w
        .players
        .values()
        .filter(|p| p.kind != PlayerKind::Npc)
        .map(|p| {
            let mut row = query::leaderboard_row(w, p);
            row["label"] = json!(p.owner.orders.public_label);
            row["character"] = character(w, p.id, false);
            row["documented"] = json!(crate::identity::documented(w, p.id));
            (row["net_worth"].as_i64().unwrap_or(0), row)
        })
        .collect();
    captains.sort_by_key(|r| std::cmp::Reverse(r.0));
    let region_rows: Vec<Value> = act
        .iter()
        .enumerate()
        .filter(|(_, a)| a.captains + a.raiders + a.colonies + a.fighters > 0)
        .map(|(r, a)| {
            json!({"region": r, "captains": a.captains, "raiders": a.raiders, "colonies": a.colonies,
                   "citadels": a.citadels, "fighters": a.fighters})
        })
        .collect();
    let lb = query::leaderboard(w, &query::LeaderboardArgs { limit: Some(50) });
    json!({
        "tick": w.tick, "tick_ms": w.rules.tick_ms, "sectors": w.sector_count(),
        "season": season_json(w),
        "captains": captains.into_iter().map(|(_, v)| v).collect::<Vec<_>>(),
        "corps": lb["corporations"],
        "hall_of_fame": w.hall_of_fame.iter().rev().take(10).collect::<Vec<_>>(),
        "champions": crate::careers::champions(w),
        "registry": public_registry(crate::diplomacy::registry(w, 60)),
        "news": w.news.iter().rev().take(40).map(|n| json!({"tick": n.tick, "headline": scrub(&n.headline)})).collect::<Vec<_>>(),
        "radio": public_radio(w).rev().take(200).map(|m| radio_json(w, m)).collect::<Vec<_>>(),
        "market": w.market_history.iter().rev().take(120).rev().collect::<Vec<_>>(),
        "galaxy_events": crate::events::active(w, true),
        "constitution": crate::constitution::public_view(w),
        "regions": region_rows,
        "stats": {
            "captains": w.players.values().filter(|p| p.kind != PlayerKind::Npc).count(),
            "raiders": act.iter().map(|a| a.raiders).sum::<i64>(),
            "colonies": act.iter().map(|a| a.colonies).sum::<i64>(),
            "fighters": act.iter().map(|a| a.fighters).sum::<i64>(),
            "corps": w.corps.len(),
            "sectors": w.sector_count(),
        },
    })
}

/// A captain's public identity card (never the private backstory), sector numbers scrubbed.
/// `full`: every public field; otherwise the short card for lists.
fn character(w: &World, pid: crate::model::PlayerId, full: bool) -> Value {
    let Some(v) = w.identities.get(&pid).and_then(crate::identity::Identity::current) else {
        return Value::Null;
    };
    let mut c = json!({"age": v.age, "pronouns": scrub(&v.pronouns), "homeworld": scrub(&v.homeworld), "bio": scrub(&v.bio)});
    if full {
        c["personality"] = json!(scrub(&v.personality));
        c["speech"] = json!(scrub(&v.speech));
        c["values"] = json!(scrub(&v.values));
        c["dream"] = json!(scrub(&v.dream));
        c["trade"] = json!(scrub(&v.trade));
        c["appearance"] = json!(scrub(&v.appearance));
        c["voice"] = json!(scrub(&v.voice));
        c["why"] = json!(scrub(&v.why));
    }
    c
}

/// A captain's public profile: their standing, plus public items that mention them.
pub fn player(w: &World, name: &str) -> Option<Value> {
    let p = w.player_by_name(name).filter(|p| p.kind != PlayerKind::Npc)?;
    let mut v = query::leaderboard_row(w, p);
    v["label"] = json!(p.owner.orders.public_label);
    v["character"] = character(w, p.id, true);
    v["documented"] = json!(crate::identity::documented(w, p.id));
    v["chronicle_pages"] = json!(
        w.chronicle
            .pages
            .values()
            .filter(|pg| pg.struck.is_none() && pg.authors().contains(&p.id))
            .rev()
            .take(15)
            .map(|pg| json!({"id": pg.id, "kind": pg.kind, "title": scrub(&pg.current().title), "canon": pg.canon_tick.is_some()}))
            .collect::<Vec<_>>()
    );
    v["radio"] = json!(
        public_radio(w)
            .rev()
            .filter(|m| m.from == p.id)
            .take(15)
            .map(|m| radio_json(w, m))
            .collect::<Vec<_>>()
    );
    v["news"] = json!(
        w.news
            .iter()
            .rev()
            .filter(|n| n.headline.contains(&p.name))
            .take(15)
            .map(|n| json!({"tick": n.tick, "headline": scrub(&n.headline)}))
            .collect::<Vec<_>>()
    );
    let reg = public_registry(crate::diplomacy::registry(w, 200));
    v["combat_reports"] = json!(
        reg["combat_reports"]
            .as_array()
            .map(|a| a
                .iter()
                .filter(|r| r["players"].as_array().is_some_and(|ps| ps.iter().any(|x| x == &json!(p.name))))
                .take(15)
                .cloned()
                .collect::<Vec<_>>())
            .unwrap_or_default()
    );
    Some(v)
}

/// How long the public fleet runs behind the game.
pub const FLEET_DELAY_MS: u64 = 3_600_000;

/// One minute of the public fleet: where every active captain was. The server records one
/// a minute, keeps about two hours of them beside the universe (never journaled) and serves
/// the one an hour old through `fleet_view`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FleetFrame {
    pub at_ms: u64,
    pub tick: Tick,
    pub season: u32,
    pub season_start: Tick,
    /// Jumps made so far this season by every captain (the hour's jump count is a difference).
    pub warps: i64,
    /// (captain, its sector, the sector its last jump came from).
    pub ships: Vec<(PlayerId, SectorId, SectorId)>,
}

/// A captain counted on the public pages: not a Skarn or Federation ship, not banned,
/// and has acted this season.
fn listed(w: &World, p: &Player) -> bool {
    p.kind != PlayerKind::Npc && !p.banned && p.last_action_tick >= w.season.started_tick
}

/// Who sets this captain's strategy, as the public pages group it.
pub fn group(p: &Player) -> &'static str {
    if p.autonomous {
        return "auto";
    }
    match p.owner.prefs.as_ref().map(|x| x.involvement) {
        Some(crate::owner::Involvement::Strategy) => "strategy",
        Some(crate::owner::Involvement::Silent) => "silent",
        None => "unset",
    }
}

/// The fleet now, to be served an hour from now. `at_ms` is the server's wall clock.
pub fn fleet_frame(w: &World, at_ms: u64) -> FleetFrame {
    FleetFrame {
        at_ms,
        tick: w.tick,
        season: w.season.number,
        season_start: w.season.started_tick,
        warps: w
            .players
            .values()
            .filter(|p| p.kind != PlayerKind::Npc)
            .map(|p| p.stats.warps)
            .sum(),
        ships: w
            .players
            .values()
            .filter(|p| listed(w, p))
            .map(|p| (p.id, p.sector, p.prev_sector))
            .collect(),
    }
}

/// The public fleet as of `frames[at]` (oldest frame first): for each captain its star, the
/// star its last jump came from, its group and the minutes since that jump (null when the
/// jump is older than the frames kept). Coordinates only: no names, ids or sector numbers.
pub fn fleet_view(w: &World, frames: &[FleetFrame], at: usize) -> Value {
    let Some(f) = frames.get(at) else {
        return json!({"ships": [], "captains": 0});
    };
    let pos = |s: SectorId| w.sector(s).map(|x| (round1(x.x), round1(x.y), round1(x.z)));
    let same_galaxy = f.season == w.season.number && f.season_start == w.season.started_tick;
    let mut ships = vec![];
    if same_galaxy {
        for &(pid, sector, prev) in &f.ships {
            let Some(p) = w.players.get(&pid) else { continue };
            if p.banned {
                continue;
            }
            let Some((x, y, z)) = pos(sector) else { continue };
            let (px, py, pz) = pos(prev).filter(|_| prev != sector).unwrap_or((x, y, z));
            // Minutes since the captain last changed star, looking back through older frames.
            let mut moved = Value::Null;
            for k in (0..at).rev() {
                match frames[k].ships.iter().find(|s| s.0 == pid) {
                    Some(s) if s.1 == sector => continue,
                    _ => {
                        moved = json!(f.at_ms.saturating_sub(frames[k + 1].at_ms) / 60_000);
                        break;
                    }
                }
            }
            ships.push(json!([x, y, px, py, group(p), moved, z, pz]));
        }
    }
    // Jumps in the hour before the frame, when the frames reach back that far.
    let jumps = frames[..at]
        .iter()
        .rev()
        .find(|o| o.at_ms + FLEET_DELAY_MS <= f.at_ms && o.season == f.season && o.season_start == f.season_start)
        .map(|o| (f.warps - o.warps).max(0));
    json!({
        "as_of_ms": f.at_ms,
        "captains": ships.len(),
        "ships": ships,
        "jumps_hour": jumps,
    })
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Provider prefixes some captains put before a model's public name ("openai-gpt-6.1-sol",
/// "anthropic/claude-opus-5-5"): the scoreboard drops them, since the maker is shown on its own.
const PROVIDER_PREFIXES: &[&str] = &[
    "openai",
    "anthropic",
    "google",
    "xai",
    "x-ai",
    "moonshot",
    "moonshotai",
    "meta",
    "mistral",
    "mistralai",
    "deepseek",
    "alibaba",
    "zhipu",
    "openrouter",
];

/// A model name as the scoreboard groups it: lower case, spaces as hyphens, no provider prefix.
pub fn model_label(m: Option<&str>) -> String {
    let mut m = m
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    for p in PROVIDER_PREFIXES {
        for sep in ['-', '/', ':', '_'] {
            if let Some(rest) = m.strip_prefix(&format!("{p}{sep}")).filter(|r| !r.is_empty()) {
                m = rest.to_string();
            }
        }
    }
    if m.is_empty() { "unknown".into() } else { m }
}

/// One spelling per model across the board: names that differ only in punctuation ("gpt-61-sol"
/// and "gpt-6.1-sol") take the spelling most captains use (with dots on a tie).
fn canonical_models(names: &[String]) -> BTreeMap<String, String> {
    let squash = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>();
    let mut by_key: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    for n in names.iter().filter(|n| *n != "unknown" && *n != "undisclosed") {
        *by_key.entry(squash(n)).or_default().entry(n.as_str()).or_default() += 1;
    }
    let mut out = BTreeMap::new();
    for spellings in by_key.values() {
        let best = spellings
            .iter()
            .max_by_key(|(s, n)| (**n, s.matches('.').count(), std::cmp::Reverse(s.len())))
            .map(|(s, _)| s.to_string())
            .unwrap_or_default();
        for s in spellings.keys() {
            out.insert(s.to_string(), best.clone());
        }
    }
    out
}

/// An agent platform as the scoreboard groups it, so "claude-code", "Claude Code" and "cc"
/// share a row. Names it does not know keep their own spelling.
pub fn harness_label(h: Option<&str>) -> String {
    let raw = h.unwrap_or("").trim();
    match known_harness(raw) {
        Some(k) => k.into(),
        None => raw.chars().take(32).collect(),
    }
}

/// The canonical name of a known agent platform (or "unknown" for none), from any spelling.
pub fn known_harness(raw: &str) -> Option<&'static str> {
    let key: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    let known = match key.as_str() {
        "" => "unknown",
        "claudecode" | "cc" | "claudecodecli" | "claudecli" => "Claude Code",
        "claudedesktop" => "Claude Desktop",
        "claudeai" | "claude" => "Claude",
        "codex" | "codexcli" | "openaicodex" => "Codex CLI",
        "openclaw" => "OpenClaw",
        "cursor" | "cursoragent" | "cursorcli" => "Cursor",
        "opencode" => "opencode",
        "grok" | "grokcli" | "grokcode" => "Grok CLI",
        "antigravity" | "agy" | "googleantigravity" => "Antigravity",
        "gemini" | "geminicli" => "Gemini CLI",
        "kimi" | "kimicli" | "kimicode" => "Kimi CLI",
        "chatgpt" => "ChatGPT",
        "openaidot" | "openaidots" | "dots" | "dot" => "OpenAI Dots",
        "hermes" | "hermesagent" => "Hermes",
        "grokbot" => "Grok Bot",
        "instinct" => "Instinct",
        "hark" | "harkrouted" => "Hark",
        "goose" => "Goose",
        "cline" => "Cline",
        "windsurf" => "Windsurf",
        "aider" => "Aider",
        "codexmcpclient" => "Codex CLI",
        _ => return None,
    };
    Some(known)
}

/// The platform an MCP client's self-reported name points to: a known spelling, or a client name
/// that contains a distinctive platform name (an OpenClaw gateway that calls itself
/// "openclaw-gateway", say). None when it names nothing known.
pub fn client_platform(raw: &str) -> Option<&'static str> {
    if let Some(k) = known_harness(raw).filter(|k| *k != "unknown") {
        return Some(k);
    }
    let k = key(Some(raw));
    const PARTS: &[(&str, &str)] = &[
        ("openclaw", "OpenClaw"),
        ("hermes", "Hermes"),
        ("instinct", "Instinct"),
        ("antigravity", "Antigravity"),
        ("claudecode", "Claude Code"),
        ("kimi", "Kimi CLI"),
        ("opencode", "opencode"),
        ("cursor", "Cursor"),
        ("grokbot", "Grok Bot"),
        ("grokcli", "Grok CLI"),
        ("openaidots", "OpenAI Dots"),
        ("codex", "Codex CLI"),
        ("geminicli", "Gemini CLI"),
        ("hark", "Hark"),
    ];
    PARTS.iter().find(|(part, _)| k.contains(part)).map(|(_, name)| *name)
}

/// Products that run their own model and do not say which: model and harness are one thing.
/// Keyed like `harness_label` (lower-case letters and digits only); value: (product, maker).
const SEALED: &[(&str, &str, &str)] = &[
    ("grokbot", "Grok Bot", "xAI"),
    ("grok", "Grok Bot", "xAI"),
    ("openaidot", "OpenAI Dots", "OpenAI"),
    ("openaidots", "OpenAI Dots", "OpenAI"),
    ("dots", "OpenAI Dots", "OpenAI"),
    ("dot", "OpenAI Dots", "OpenAI"),
    ("instinct", "Instinct", "Undisclosed"),
    ("hark", "Hark", "Undisclosed"),
    ("harkrouted", "Hark", "Undisclosed"),
];

fn key(s: Option<&str>) -> String {
    s.unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Who makes a model, from its public name.
pub fn maker(model: &str) -> &'static str {
    let m = model.to_ascii_lowercase();
    let starts = |ps: &[&str]| ps.iter().any(|p| m.starts_with(p));
    if starts(&["claude"]) {
        "Anthropic"
    } else if starts(&["gpt", "o1", "o3", "o4", "o5", "codex"]) {
        "OpenAI"
    } else if starts(&["grok"]) {
        "xAI"
    } else if starts(&["gemini", "gemma"]) {
        "Google"
    } else if starts(&["kimi", "k2", "k3", "k4"]) {
        "Moonshot"
    } else if starts(&["qwen"]) {
        "Alibaba"
    } else if starts(&["deepseek"]) {
        "DeepSeek"
    } else if starts(&["llama"]) {
        "Meta"
    } else if starts(&["mistral", "devstral", "magistral"]) {
        "Mistral"
    } else if starts(&["glm"]) {
        "Zhipu"
    } else {
        "Other"
    }
}

/// A captain's agent as the scoreboard shows it: (agent, model, harness, maker, sealed).
/// A sealed product (named by either field) has its model undisclosed.
pub fn identity(model: Option<&str>, harness: Option<&str>) -> (String, String, String, String, bool) {
    let sealed = |k: &str| SEALED.iter().find(|s| s.0 == k);
    let (mk, hk) = (key(model), key(harness));
    if let Some(&(_, product, mk)) = sealed(&hk).or_else(|| if model.is_some() { sealed(&mk) } else { None }) {
        return (product.into(), "undisclosed".into(), product.into(), mk.into(), true);
    }
    let m = model_label(model);
    let h = harness_label(harness);
    let maker = if m == "unknown" { "Undisclosed" } else { maker(&m) };
    (format!("{m} · {h}"), m, h, maker.into(), false)
}

/// The latest model or harness change this season, from the news: (tick, what it ran before).
fn switched(w: &World, name: &str) -> Option<(Tick, String)> {
    let head = format!("{name} now runs on ");
    w.news
        .iter()
        .rev()
        .take_while(|n| n.tick >= w.season.started_tick)
        .find(|n| n.headline.starts_with(&head))
        .map(|n| {
            let was = n
                .headline
                .rsplit_once("(was ")
                .map(|(_, r)| r.trim_end_matches(").").to_string())
                .unwrap_or_default();
            (n.tick, scrub(&was))
        })
}

/// The tick action points were first counted (`stats.ap_spent`, engine 1.24 reached production a
/// little over an hour into the season that began at tick 1586). Activity measures received action
/// points from here; any season that starts later is unaffected.
const AP_COUNTED_FROM: Tick = 1663;

/// Rows for the public scoreboard, one per captain that has spent action points this season and
/// says what it runs on (a model, a harness or a sealed product): call sign,
/// agent (model and harness, or a sealed product), maker, who sets its strategy, what it plays
/// for, what it earned, its four score tracks, days played, action points spent and received,
/// and any model or harness change this season. No ids, positions or holdings.
/// What a captain earned this season, as the scoreboard counts it: net worth minus what it
/// started with, plus contraband in the hold at what it cost (so a smuggler's score does not drop
/// between buying and selling; the game's net worth leaves it out).
pub fn earned(w: &World, p: &Player) -> i64 {
    let held = p.ship.contraband * w.rules.contraband_buy_price;
    crate::player::net_worth(w, p.id) - crate::player::endowment(w, p) + held
}

/// Action points a captain has received this season (the start allowance plus a day's allowance,
/// or the per-tick regeneration, for every day since it joined, counted from `AP_COUNTED_FROM`).
pub fn ap_received(w: &World, p: &Player) -> i64 {
    let day_ms = 86_400_000.0;
    let per_day = w
        .rules
        .ap_per_day
        .unwrap_or(w.rules.ap_regen_per_tick * (86_400_000 / w.rules.tick_ms.max(1)) as i64);
    let since = p.created_tick.max(w.season.started_tick);
    let ap_days = (w.tick.saturating_sub(since.max(AP_COUNTED_FROM)) * w.rules.tick_ms) as f64 / day_ms;
    w.rules.ap_start + (per_day as f64 * ap_days).round() as i64
}

pub fn scoreboard(w: &World) -> Value {
    let day_ms = 86_400_000.0;
    let names: Vec<String> = w
        .players
        .values()
        .filter(|p| listed(w, p))
        .map(|p| identity(p.model.as_deref(), p.harness.as_deref()).1)
        .collect();
    let canon = canonical_models(&names);
    let mut rows: Vec<(i64, Value)> = w
        .players
        .values()
        .filter(|p| listed(w, p) && p.stats.ap_spent > 0)
        .filter(|p| {
            let (_, model, harness, _, sealed) = identity(p.model.as_deref(), p.harness.as_deref());
            sealed || model != "unknown" || harness != "unknown"
        })
        .map(|p| {
            let since = p.created_tick.max(w.season.started_tick);
            let ticks = w.tick.saturating_sub(since) as i64;
            let days = (ticks as u64 * w.rules.tick_ms) as f64 / day_ms;
            let earned = earned(w, p);
            let (mut agent, mut model, harness, mut maker, sealed) = identity(p.model.as_deref(), p.harness.as_deref());
            if let Some(c) = canon.get(&model).filter(|c| **c != model) {
                model = c.clone();
                agent = format!("{model} · {harness}");
                maker = self::maker(&model).into();
            }
            let planets = w.planets.values().filter(|pl| pl.owner == Some(p.id)).count();
            let switch = switched(w, &p.name).map(|(tick, was)| json!({"tick": tick, "was": was}));
            let row = json!({
                "name": p.name, "agent": agent, "model": model, "harness": harness, "maker": maker, "sealed": sealed,
                "group": group(p), "plays_for": p.win_condition, "earned": earned,
                "scores": crate::careers::scores_json(w, p),
                "days": (days * 1000.0).round() / 1000.0, "ap_spent": p.stats.ap_spent,
                "ap_received": ap_received(w, p),
                "daily_wins": p.stats.daily_wins, "deaths": p.stats.deaths,
                "trade_profit": p.stats.trade_profit,
                "planets": planets, "explored": p.stats.sectors_explored,
                "switched": switch,
                "character": character(w, p.id, false),
                "documented": crate::identity::documented(w, p.id),
            });
            (earned, row)
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    json!({
        "rows": rows.into_iter().map(|r| r.1).collect::<Vec<_>>(),
        "season": season_json(w),
        "tick": w.tick,
        "tick_ms": w.rules.tick_ms,
    })
}

/// The last finished season rebuilt from career totals, for an archive that began after it ended.
/// Exact only while one season has finished since the last fresh start (career totals then hold
/// that season alone), so it returns None otherwise. Rows carry what career totals know: trading
/// profit, action points spent, daily wins, deaths and sectors explored, plus the final net worth
/// of the captains the hall of fame lists.
pub fn season_from_career(w: &World) -> Option<Value> {
    if w.hall_of_fame.len() != 1 {
        return None;
    }
    let rec = &w.hall_of_fame[0];
    let mut rows: Vec<(i64, Value)> = w
        .players
        .values()
        .filter(|p| p.kind != PlayerKind::Npc && !p.banned && p.career.ap_spent > 0)
        .map(|p| {
            let (agent, model, harness, maker, sealed) = identity(p.model.as_deref(), p.harness.as_deref());
            let nw = rec.standings.iter().find(|s| s.0 == p.name).map(|s| s.2);
            let c = &p.career;
            (
                c.trade_profit,
                json!({
                    "name": p.name, "agent": agent, "model": model, "harness": harness, "maker": maker, "sealed": sealed,
                    "group": group(p), "trade_profit": c.trade_profit, "ap_spent": c.ap_spent, "net_worth": nw,
                    "daily_wins": c.daily_wins, "deaths": c.deaths, "explored": c.sectors_explored,
                }),
            )
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    Some(json!({
        "season": rec.number, "started_tick": rec.started_tick, "ended_tick": rec.ended_tick,
        "how": "rebuilt", "rows": rows.into_iter().map(|r| r.1).collect::<Vec<_>>(),
    }))
}

/// The marketing page's galaxy: the public map with activity drawn at region
/// centres, plus headline numbers and standings.
pub fn showcase(w: &World) -> Value {
    let regions = Regions::new(w);
    let act = activity(w, &regions);
    let at = |pick: fn(&Activity) -> i64| -> Vec<Value> {
        act.iter()
            .enumerate()
            .filter(|(_, a)| pick(a) > 0)
            .map(|(r, a)| {
                let (x, y, z) = regions.center(r);
                json!([x, y, pick(a), z])
            })
            .collect()
    };
    let lb = query::leaderboard(w, &query::LeaderboardArgs { limit: Some(8) });
    let standings: Vec<Value> = lb["players"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| json!({"name": p["name"], "model": p["model"], "net_worth": p["net_worth"], "corp": p["corp"]}))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "tick": w.tick,
        "stars": stars(w),
        "ships": at(|a| a.captains),
        "raiders": at(|a| a.raiders),
        "colonies": at(|a| a.colonies),
        "defenses": at(|a| a.fighters),
        "standings": standings,
        "headlines": w.news.iter().rev().take(12).map(|n| scrub(&n.headline)).collect::<Vec<_>>(),
        "stats": {
            "captains": w.players.values().filter(|p| p.kind != PlayerKind::Npc).count(),
            "raiders": act.iter().map(|a| a.raiders).sum::<i64>(),
            "colonies": act.iter().map(|a| a.colonies).sum::<i64>(),
            "fighters": act.iter().map(|a| a.fighters).sum::<i64>(),
            "sectors": w.sector_count(),
        },
        "season": season_json(w),
        "hall_of_fame": w.hall_of_fame.iter().rev().take(5).map(|r| json!({"season": r.number, "winner": r.standings.first()})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::galaxy::{GenesisConfig, generate};

    #[test]
    fn sealed_products_hide_the_model_and_open_stacks_keep_it() {
        let id = |m: Option<&str>, h: Option<&str>| identity(m, h);
        assert_eq!(id(Some("grok-bot"), None).0, "Grok Bot");
        assert_eq!(id(Some("grok-bot"), Some("grok-bot")), id(Some("grok-bot"), None));
        let dots = id(None, Some("OpenAI dot"));
        assert_eq!(
            (dots.0.as_str(), dots.1.as_str(), dots.3.as_str(), dots.4),
            ("OpenAI Dots", "undisclosed", "OpenAI", true)
        );
        assert_eq!(id(None, Some("instinct")).3, "Undisclosed");
        let sol = id(Some("gpt-6.1-sol"), Some("openclaw"));
        assert_eq!(
            (sol.0.as_str(), sol.1.as_str(), sol.2.as_str(), sol.3.as_str(), sol.4),
            ("gpt-6.1-sol · OpenClaw", "gpt-6.1-sol", "OpenClaw", "OpenAI", false)
        );
        assert_eq!(
            id(Some("grok-4.2"), Some("grok-cli")).1,
            "grok-4.2",
            "a disclosed grok model is not sealed"
        );
        assert_eq!(id(None, Some("hermes")).3, "Undisclosed");
        assert_eq!(id(Some("k3"), Some("kimi-code")).3, "Moonshot");
        assert_eq!(client_platform("openclaw-gateway"), Some("OpenClaw"));
        assert_eq!(client_platform("hermes-agent/0.9"), Some("Hermes"));
        assert_eq!(client_platform("codex-mcp-client"), Some("Codex CLI"));
        assert_eq!(client_platform("mcp-remote"), None);
        let hark = id(Some("hark-routed"), Some("hark"));
        assert_eq!((hark.0.as_str(), hark.4), ("Hark", true));
    }

    #[test]
    fn a_finished_season_rebuilds_from_career_totals_only_when_it_is_the_only_one() {
        let mut w = generate(&GenesisConfig::new(5, 200));
        let mut p = w.players.values().next().unwrap().clone();
        p.id = 8;
        p.name = "Lark".into();
        p.kind = PlayerKind::Agent;
        p.npc = None;
        p.model = Some("gpt-6.1-sol".into());
        p.harness = Some("openclaw".into());
        p.career.trade_profit = 50_000;
        p.career.ap_spent = 1_000;
        w.players.insert(8, p);
        assert!(season_from_career(&w).is_none(), "no finished season yet");
        w.hall_of_fame.push(SeasonRecord {
            number: 1,
            started_tick: 10,
            ended_tick: 100,
            standings: vec![("Lark".into(), Some("gpt-6.1-sol".into()), 400_000)],
            champions: vec![],
        });
        let s = season_from_career(&w).unwrap();
        assert_eq!(s["season"], 1);
        assert_eq!(s["how"], "rebuilt");
        let r = &s["rows"][0];
        assert_eq!(
            (
                r["name"].as_str(),
                r["trade_profit"].as_i64(),
                r["ap_spent"].as_i64(),
                r["net_worth"].as_i64()
            ),
            (Some("Lark"), Some(50_000), Some(1_000), Some(400_000))
        );
        assert_eq!(r["harness"], "OpenClaw");
        w.hall_of_fame.push(w.hall_of_fame[0].clone());
        assert!(season_from_career(&w).is_none(), "two seasons in career totals cannot be split");
    }

    #[test]
    fn harness_and_model_labels_merge_spellings() {
        for h in ["claude-code", "Claude Code", "cc", " CLAUDE_CODE "] {
            assert_eq!(harness_label(Some(h)), "Claude Code");
        }
        assert_eq!(harness_label(Some("agy")), "Antigravity");
        assert_eq!(harness_label(None), "unknown");
        assert_eq!(harness_label(Some("Homebrew Rig")), "Homebrew Rig");
        assert_eq!(model_label(Some(" Claude Opus 5 ")), "claude-opus-5");
        assert_eq!(model_label(Some("")), "unknown");
        assert_eq!(model_label(Some("openai-gpt-61-sol")), "gpt-61-sol");
        assert_eq!(model_label(Some("anthropic/claude-opus-5-5")), "claude-opus-5-5");
        assert_eq!(model_label(Some("openai")), "openai", "a bare provider name is kept");
        let c = canonical_models(&["gpt-6.1-sol".into(), "gpt-6.1-sol".into(), "gpt-61-sol".into(), "k3".into()]);
        assert_eq!(c["gpt-61-sol"], "gpt-6.1-sol");
        assert_eq!(c["k3"], "k3");
        let c = canonical_models(&["gpt-61-sol".into(), "gpt-6.1-sol".into()]);
        assert_eq!(c["gpt-61-sol"], "gpt-6.1-sol", "dots win a tie");
    }

    #[test]
    fn fleet_counts_jumps_and_minutes_since_a_move() {
        let mut w = generate(&GenesisConfig::new(5, 200));
        let mut p = w.players.values().next().unwrap().clone();
        p.id = 7;
        p.kind = PlayerKind::Agent;
        p.npc = None;
        p.sector = 10;
        p.prev_sector = 10;
        p.last_action_tick = w.season.started_tick;
        w.players.insert(7, p);
        let mut frames = vec![];
        for m in 0..=120u64 {
            if m == 100 {
                let p = w.players.get_mut(&7).unwrap();
                p.prev_sector = 10;
                p.sector = 11;
                p.stats.warps += 1;
            }
            frames.push(fleet_frame(&w, m * 60_000));
        }
        let v = fleet_view(&w, &frames, 120);
        let ship = v["ships"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s[0] == json!(round1(w.sector(11).unwrap().x)))
            .unwrap()
            .clone();
        assert_eq!(ship[5], json!(20), "moved 20 minutes before the frame");
        assert_eq!(v["jumps_hour"], json!(1));
        // A frame from an older galaxy shows nothing.
        w.season.started_tick += 1;
        assert_eq!(fleet_view(&w, &frames, 120)["captains"], json!(0));
    }

    #[test]
    fn scrub_removes_sector_numbers() {
        assert_eq!(scrub("Kai attacked Vex in sector 412."), "Kai attacked Vex.");
        assert_eq!(scrub("Kai bought 30 ore at sector 9."), "Kai bought 30 ore.");
        assert_eq!(scrub("Defend Sector 77 for a week"), "Defend a sector for a week");
        assert_eq!(scrub("sector 5"), "a sector");
        assert_eq!(scrub("The sector is quiet; 12 ships."), "The sector is quiet; 12 ships.");
        assert_eq!(scrub("Naïve pilot in sector 3 → home"), "Naïve pilot → home");
    }

    #[test]
    fn private_events_stay_private_and_public_ones_lose_positions() {
        let e = |kind: &str, text: &str| Event {
            tick: 1,
            kind: kind.into(),
            text: text.into(),
            sector: Some(42),
            players: vec![1],
            public: true,
            data: json!({"from": 41, "to": 42}),
        };
        for k in [
            "move",
            "trade",
            "deploy",
            "beacon",
            "planet_created",
            "transfer",
            "radio_corp",
            "radio_private",
            "feedback",
        ] {
            assert!(event(&e(k, "x in sector 42")).is_none(), "{k} must not be public");
        }
        let c = event(&e("combat", "Kai attacked Vex in sector 42.")).unwrap();
        assert_eq!(c.sector, None);
        assert_eq!(c.data, Value::Null);
        assert_eq!(c.text, "Kai attacked Vex.");
    }

    /// Every key anywhere in a JSON value.
    fn keys(v: &Value, out: &mut Vec<String>) {
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    out.push(k.clone());
                    keys(x, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| keys(x, out)),
            _ => {}
        }
    }

    #[test]
    fn public_projections_carry_no_positions_lanes_or_private_fields() {
        let mut w = generate(&GenesisConfig::new(5, 400));
        // A captain with money, cargo and private radio.
        let mut p = w.players.values().next().unwrap().clone();
        p.id = 9_001;
        p.name = "Vex".into();
        p.kind = PlayerKind::Agent;
        p.npc = None;
        p.sector = 123;
        w.players.insert(p.id, p);
        w.radio.push(RadioMsg {
            id: 1,
            tick: 1,
            at_ms: 0,
            from: 9_001,
            from_name: "Vex".into(),
            channel: Channel::Private(2),
            text: "secret plan".into(),
        });
        // Public radio from a captain and a Federation bulletin, both naming the sector.
        w.radio.push(RadioMsg {
            id: 2,
            tick: 1,
            at_ms: 0,
            from: 9_001,
            from_name: "Vex".into(),
            channel: Channel::Public,
            text: "Ore for sale, meet me in sector 123.".into(),
        });
        w.radio.push(RadioMsg {
            id: 3,
            tick: 1,
            at_ms: 0,
            from: 0,
            from_name: "Federation".into(),
            channel: Channel::Public,
            text: "FEDERATION BULLETIN: Salvage drifts in Sector 123. First to arrive takes it.".into(),
        });
        w.news.push(NewsItem {
            tick: 1,
            headline: "Vex claimed the wild planet Oro in sector 123.".into(),
        });
        let forbidden = [
            "sector",
            "seat",
            "connected",
            "warps",
            "w",
            "credits",
            "bank",
            "ap",
            "cargo",
            "holds",
            "fighters_aboard",
            "shields",
            "inbox",
            "notes",
            "orders",
            "strategy",
            "feedback",
            "feedback_open",
            "feedback_recent",
            "votes",
            "electorate_ids",
            "backstory",
            "reports",
        ];
        // An identity (engine 1.46): the card is public and scrubbed; the backstory never leaves.
        w.identities.insert(
            9_001,
            crate::identity::Identity {
                versions: vec![crate::identity::Version {
                    version: 1,
                    name: "Vex".into(),
                    age: 40,
                    homeworld: "Station in sector 123".into(),
                    bio: "Hauls ore out of sector 123.".into(),
                    backstory: "SECRET-BACKSTORY".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        // A Chronicle page naming a sector, with a content report.
        w.chronicle.next_page = 1;
        w.chronicle.pages.insert(
            1,
            crate::chronicle::Page {
                id: 1,
                kind: crate::chronicle::Kind::Event,
                created_by: 9_001,
                created_tick: 1,
                season: 1,
                epoch: 0,
                revisions: vec![crate::chronicle::Revision {
                    revision: 1,
                    author: 9_001,
                    author_name: "Vex".into(),
                    tick: 1,
                    at_ms: 0,
                    season: 1,
                    epoch: 0,
                    title: "The ambush at sector 123".into(),
                    body: "It happened in sector 123.".into(),
                    summary: String::new(),
                    citations: vec![],
                }],
                talk: vec![],
                endorsements: vec![],
                canon_tick: None,
                canon_season: None,
                reports: vec![crate::chronicle::ContentReport {
                    player: 9_001,
                    name: "Vex".into(),
                    tick: 1,
                    at_ms: 0,
                    reason: "PRIVATE-REPORT".into(),
                    revision: 1,
                }],
                struck: None,
                production: None,
            },
        );
        // Feedback to the operator is private: no public projection carries it.
        w.feedback.push(crate::feedback::Feedback {
            id: 1,
            tick: 1,
            at_ms: 0,
            from: Some(9_001),
            from_name: Some("Vex".into()),
            about: "/join".into(),
            text: "private feedback report".into(),
            model: None,
            harness: None,
            status: crate::feedback::FeedbackStatus::Open,
            resolved_tick: None,
            resolved_at_ms: None,
            resolution: None,
        });
        w.next_feedback = 1;
        // The constitution is public, but its player-written text is scrubbed and votes stay private.
        w.constitution.articles.push(crate::constitution::Article {
            id: 1,
            text: "Neutral ground in sector 123.".into(),
            proposed_by: "Vex".into(),
            ratified_tick: 1,
            season: 1,
            amended_tick: None,
            history: vec![crate::constitution::ArticleVersion {
                proposal: 1,
                change: "add".into(),
                text: "Neutral ground in sector 123.".into(),
                tick: 1,
                season: 1,
                yes: 2,
                no: 0,
                electorate: 3,
            }],
            by_operator: false,
        });
        // The operator's comments, notices and strike reasons are scrubbed the same way.
        w.constitution.notices.push(crate::constitution::OperatorNotice {
            tick: 1,
            at_ms: 0,
            season: 1,
            text: "Vote on the sector 123 article.".into(),
            announcement: None,
        });
        w.constitution.proposals.push(crate::constitution::Proposal {
            id: 2,
            change: crate::constitution::ProposalKind::Amend {
                article: 1,
                text: "Neutral ground in sector 123, always.".into(),
            },
            proposer: 9_001,
            proposer_name: "Vex".into(),
            season: 1,
            opened_tick: 1,
            closes_tick: 100,
            electorate: [9_001].into_iter().collect(),
            votes: [(9_001, true)].into_iter().collect(),
            status: crate::constitution::ProposalStatus::Open,
            closed_tick: None,
            ballots: vec![],
            by_operator: false,
            comments: vec![crate::constitution::OperatorComment {
                tick: 2,
                at_ms: 0,
                text: "Is sector 123 the right place?".into(),
            }],
            struck: Some(crate::constitution::StrikeNote {
                tick: 3,
                at_ms: 0,
                reason: Some("names sector 123".into()),
            }),
        });
        assert!(state(&w)["constitution"].to_string().contains("Neutral ground."));
        assert!(state(&w)["constitution"]["open_proposals"][0]["operator_comments"].is_array());
        // Registry entries that name a sector (public in game through `view_registry`).
        w.intents.insert(
            9_001,
            Intent {
                player: 9_001,
                kind: IntentKind::Trading,
                sector: Some(123),
                text: "Buying ore in sector 123.".into(),
                declared_tick: 1,
                expires_tick: 100,
            },
        );
        w.combat_reports.push(CombatReport {
            id: 1,
            tick: 1,
            sector: Some(123),
            kind: "combat".into(),
            players: vec![9_001],
            text: "Vex fought Skarn Mardo in sector 123.".into(),
        });
        // Reputation and corporate space are public as names and counts, never as sectors.
        w.players.get_mut(&9_001).unwrap().stats.treaties_broken = 1;
        w.players.get_mut(&9_001).unwrap().corp = Some(77);
        w.corps.insert(
            77,
            Corp {
                id: 77,
                name: "Vex Holdings".into(),
                ceo: 9_001,
                members: vec![9_001],
                invites: vec![],
                treasury: 0,
                motto: String::new(),
                created_tick: 0,
                deposits: [(9_001, 0)].into_iter().collect(),
                record: CorpRecord {
                    treaties_signed: 2,
                    ..Default::default()
                },
                gov: Default::default(),
                // Its seat (sector 123, held below): public only as counts and flags.
                seat: Some(123),
                seat_set_tick: Some(1),
            },
        );
        w.sectors[122].fighters = Some(FighterStack {
            owner: 9_001,
            count: 50,
            mode: FighterMode::Toll,
            shares: Default::default(),
            toll_price: None,
        });
        let reg = state(&w)["registry"].clone();
        assert!(
            reg["reputation"]["captains"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["name"] == "Vex" && r["treaties_broken"] == 1)
        );
        assert!(
            reg["reputation"]["corporations"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["name"] == "Vex Holdings")
        );
        let row = reg["corporations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "Vex Holdings")
            .unwrap()
            .clone();
        assert_eq!(row["space"]["guarded_sectors"], 1);
        // Territory: counts and flags only, in the registry and the public government summary.
        for t in [&row["space"]["territory"], &row["government"]["territory"]] {
            assert_eq!(t["connected_sectors"], 1);
            assert_eq!(t["outposts"], 0);
            assert_eq!(t["has_seat"], true);
            assert_eq!(t["depot"], false);
            assert!(t.as_object().unwrap().values().all(|v| v.is_number() || v.is_boolean()), "{t}");
        }
        assert_eq!(player(&w, "Vex").unwrap()["reputation"]["treaties_broken"], 1);
        let mut reg_keys = vec![];
        keys(&state(&w)["registry"], &mut reg_keys);
        assert!(!reg_keys.iter().any(|k| k == "sector"), "the public registry carries no sector ids");
        for k in ["seat", "connected", "seat_set_tick"] {
            assert!(
                !reg_keys.iter().any(|x| x == k),
                "the public registry carries no territory sectors (`{k}`)"
            );
        }
        assert!(
            !state(&w)["registry"].to_string().to_ascii_lowercase().contains("sector 4"),
            "registry texts are scrubbed"
        );
        for (name, v) in [
            ("state", state(&w)),
            ("map", map(&w)),
            ("showcase", showcase(&w)),
            ("player", player(&w, "Vex").unwrap()),
            ("chronicle", crate::chronicle::public_view(&w, &Default::default())),
            ("chronicle_page", crate::chronicle::public_page(&w, 1).unwrap()),
        ] {
            // The registry is the in-game `view_registry` tool, public by design.
            let mut v = v.clone();
            if let Some(o) = v.as_object_mut() {
                o.remove("registry");
                o.remove("combat_reports");
            }
            let mut ks = vec![];
            keys(&v, &mut ks);
            for f in forbidden {
                assert!(!ks.iter().any(|k| k == f), "{name} carries `{f}`");
            }
            let text = v.to_string();
            assert!(!text.contains("secret plan"), "{name} leaks private radio");
            assert!(!text.contains("private feedback report"), "{name} leaks feedback");
            assert!(!text.contains("SECRET-BACKSTORY"), "{name} leaks a backstory");
            assert!(!text.contains("PRIVATE-REPORT"), "{name} leaks a content report");
            if name.starts_with("chronicle") {
                assert!(!ks.iter().any(|k| k == "player_id"), "{name} carries player ids");
            }
            if name == "state" || name == "player" {
                assert!(text.contains("Ore for sale, meet me."), "{name} keeps public radio, scrubbed");
            }
            assert!(!text.to_ascii_lowercase().contains("sector 123"), "{name} leaks a sector number");
        }
        // The hour-old fleet and the scoreboard: coordinates and labels, never names, ids or sectors.
        w.players.get_mut(&9_001).unwrap().prev_sector = 122;
        w.players.get_mut(&9_001).unwrap().last_action_tick = w.season.started_tick;
        w.players.get_mut(&9_001).unwrap().stats.ap_spent = 12;
        w.players.get_mut(&9_001).unwrap().model = Some("test-model".into());
        let frames = vec![fleet_frame(&w, 0), fleet_frame(&w, 60_000)];
        assert!(frames[1].ships.iter().any(|s| s.0 == 9_001));
        for (name, v) in [("fleet", fleet_view(&w, &frames, 1)), ("scoreboard", scoreboard(&w))] {
            let mut ks = vec![];
            keys(&v, &mut ks);
            for f in forbidden.iter().chain(&["id", "player", "player_id", "prev_sector"]) {
                assert!(!ks.iter().any(|k| k == f), "{name} carries `{f}`");
            }
            let text = v.to_string();
            assert!(!text.contains("9001"), "{name} carries a player id");
            assert!(!text.contains("SECRET-BACKSTORY"), "{name} leaks a backstory");
            assert!(!text.contains("private feedback report"), "{name} leaks feedback");
            // The map is anonymous; the scoreboard names captains (public on /watch already).
            assert_eq!(
                text.contains("Vex"),
                name == "scoreboard",
                "{name}: call signs only on the scoreboard"
            );
        }
        let fleet = fleet_view(&w, &frames, 1);
        let ship = fleet["ships"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s[0] == json!(round1(w.sector(123).unwrap().x)))
            .unwrap();
        assert!(
            ship.as_array().unwrap()[..4].iter().all(|v| v.is_f64()),
            "positions are coordinates"
        );
        let regions = Regions::new(&w);
        assert!(
            regions.len() >= 400 / SECTORS_PER_REGION / 2,
            "about {SECTORS_PER_REGION} sectors per region"
        );
        assert!((1..=400).all(|s| regions.of(s).is_some()));
    }
}
