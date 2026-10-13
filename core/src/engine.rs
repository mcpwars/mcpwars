//! The deterministic state transition function.
//!
//! `apply(world, entry)` validates a command fully before mutating anything,
//! so a `Refusal` always leaves the world untouched (property-tested).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cmd::*;
use crate::model::*;
use crate::rng::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum Actor {
    System,
    Player(PlayerId),
    /// A captain's owner (owner token): may only issue `Command::Owner`.
    Owner(PlayerId),
}

/// One journaled input. The journal is the sole source of truth: replaying
/// every entry against the genesis world reproduces the exact state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub at_ms: u64,
    pub actor: Actor,
    pub cmd: Command,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Refusal {
    pub code: String,
    pub message: String,
    /// What the agent can do instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Refusal {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Refusal {
            code: code.into(),
            message: message.into(),
            hint: None,
        }
    }
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        if let Some(h) = &self.hint {
            write!(f, " Hint: {h}")?;
        }
        Ok(())
    }
}

pub type Res<T> = Result<T, Refusal>;

/// Something that happened, for spectators and notification fan-out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub tick: Tick,
    pub kind: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sector: Option<SectorId>,
    pub players: Vec<PlayerId>,
    /// Visible on the public spectator feed.
    pub public: bool,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    /// One-line narrative for the acting agent.
    pub summary: String,
    /// Structured result for the acting agent.
    pub reply: Value,
    pub events: Vec<Event>,
}

/// The engine 1.58 notice fields `Ctx::notify_with` sets beyond the kind and server text.
#[derive(Clone, Debug, Default)]
pub struct NoticeExtra {
    /// Player-written words, shown only as untrusted player text.
    pub quote: Option<String>,
    pub about: Option<crate::mention::Place>,
    pub mention: Option<crate::mention::MentionRef>,
    pub fold: Option<Fold>,
}

/// Mutable execution context threaded through command handlers.
pub struct Ctx<'a> {
    pub w: &'a mut World,
    pub rng: Rng,
    pub events: Vec<Event>,
}

impl<'a> Ctx<'a> {
    pub fn event(
        &mut self,
        kind: &str,
        sector: Option<SectorId>,
        players: Vec<PlayerId>,
        public: bool,
        text: impl Into<String>,
        data: Value,
    ) {
        self.events.push(Event {
            tick: self.w.tick,
            kind: kind.into(),
            text: text.into(),
            sector,
            players,
            public,
            data,
        });
    }

    pub fn news(&mut self, headline: impl Into<String>) {
        let h = headline.into();
        self.w.news.push(NewsItem {
            tick: self.w.tick,
            headline: h,
        });
        let excess = self.w.news.len().saturating_sub(300);
        if excess > 0 {
            self.w.news.drain(0..excess);
        }
    }

    /// Publish a server announcement: every captain sees it in its briefing until it acknowledges it.
    pub fn announce(&mut self, kind: &str, title: impl Into<String>, text: impl Into<String>) -> u64 {
        self.w.next_announcement += 1;
        let id = self.w.next_announcement;
        self.w.announcements.push(Announcement {
            id,
            tick: self.w.tick,
            kind: kind.into(),
            title: title.into(),
            text: text.into(),
        });
        id
    }

    /// Push a private notice into a player's inbox.
    pub fn notify(&mut self, pid: PlayerId, kind: &str, text: impl Into<String>) {
        self.notify_with(pid, kind, text, NoticeExtra::default());
    }

    /// Push a notice with the engine 1.58 fields (a quote, what it is about, a mention mark, a
    /// fold). Returns the new notice id, or None for an NPC or an unknown player.
    pub fn notify_with(&mut self, pid: PlayerId, kind: &str, text: impl Into<String>, extra: NoticeExtra) -> Option<u64> {
        let tick = self.w.tick;
        let at_ms = self.w.now_ms;
        let p = self.w.players.get_mut(&pid)?;
        if p.kind == PlayerKind::Npc {
            return None;
        }
        let id = p.next_notice;
        p.next_notice += 1;
        p.inbox.push(Notice {
            id,
            tick,
            at_ms,
            kind: kind.into(),
            text: text.into(),
            read: false,
            quote: extra.quote,
            about: extra.about,
            mention: extra.mention,
            fold: extra.fold,
            superseded_by: None,
            struck_quote: None,
        });
        Some(id)
    }

    /// Push a folding notice (engine 1.58): when the captain still has an unread notice of the
    /// same kind and fold key, the new notice carries the merged range and the old one is marked
    /// read and points at it. The new notice always has a new id, so waiters wake and a late read
    /// receipt for the old id never marks unseen content read.
    pub fn notify_fold(
        &mut self,
        pid: PlayerId,
        kind: &str,
        fold: Fold,
        text_of: impl Fn(&World, &Fold) -> String,
        quote: Option<String>,
        about: Option<crate::mention::Place>,
    ) -> Option<u64> {
        // Read phase: find the newest unread notice of the same key and merge.
        let (old, merged) = {
            let w: &World = self.w;
            let p = w.players.get(&pid)?;
            let found = p
                .inbox
                .iter()
                .enumerate()
                .rev()
                .find(|(_, n)| !n.read && n.kind == kind && n.fold.as_ref().map(Fold::key) == Some(fold.key()));
            match found {
                Some((i, n)) => {
                    let mut m = n.fold.clone().expect("matched a fold");
                    m.merge(&fold);
                    (Some(i), m)
                }
                None => (None, fold),
            }
        };
        let text = text_of(self.w, &merged);
        // Write phase.
        let new_id = self.notify_with(
            pid,
            kind,
            text,
            NoticeExtra {
                quote,
                about,
                mention: None,
                fold: Some(merged),
            },
        )?;
        if let Some(i) = old
            && let Some(n) = self.w.players.get_mut(&pid).and_then(|p| p.inbox.get_mut(i))
        {
            n.read = true;
            n.superseded_by = Some(new_id);
        }
        Some(new_id)
    }

    pub fn player(&self, pid: PlayerId) -> Res<&Player> {
        self.w
            .players
            .get(&pid)
            .ok_or_else(|| Refusal::new("unknown_player", "No such player."))
    }

    pub fn player_mut(&mut self, pid: PlayerId) -> &mut Player {
        self.w.players.get_mut(&pid).expect("validated player")
    }

    pub fn require_ap(&self, pid: PlayerId, cost: i64) -> Res<()> {
        let p = self.player(pid)?;
        if cost > 0 {
            crate::identity::gate(self.w, pid)?;
        }
        if p.ap < cost {
            let per = self.w.rules.ap_regen_per_tick.max(1);
            let wait = (cost - p.ap + per - 1) / per;
            return Err(
                Refusal::new("insufficient_ap", format!("This needs {cost} AP but you have {}.", p.ap)).hint(format!(
                    "AP regenerates {per}/tick; about {wait} tick(s) to wait. Read-only tools cost 0 AP."
                )),
            );
        }
        Ok(())
    }

    /// Refusal for "not enough credits on your ship", pointing at the bank when that would help.
    pub fn no_credits(&self, pid: PlayerId, what: &str, cost: i64) -> Refusal {
        let (credits, bank) = self.w.players.get(&pid).map(|p| (p.credits, p.bank)).unwrap_or((0, 0));
        let r = Refusal::new(
            "insufficient_credits",
            format!("{what} costs {cost} credits; you have {credits} on your ship."),
        );
        if credits + bank >= cost {
            r.hint(format!(
                "You have {bank} in the bank: withdraw at Stardock with bank action=withdraw (purchases use on-ship credits)."
            ))
        } else {
            r.hint("Earn more by trading (find_trade_routes).")
        }
    }

    pub fn spend_ap(&mut self, pid: PlayerId, cost: i64) {
        let tick = self.w.tick;
        let p = self.player_mut(pid);
        p.ap -= cost;
        p.stats.ap_spent += cost;
        p.last_action_tick = tick;
    }

    pub fn finish(&mut self, summary: impl Into<String>, reply: Value) -> Outcome {
        Outcome {
            summary: summary.into(),
            reply,
            events: std::mem::take(&mut self.events),
        }
    }
}

/// Apply one journal entry. On `Err`, the world is completely unchanged.
pub fn apply(w: &mut World, e: &Entry) -> Res<Outcome> {
    let (prev_seq, prev_now) = (w.seq, w.now_ms);
    let r = apply_inner(w, e);
    match &r {
        Ok(out) => {
            crate::diplomacy::record_combat(w, &out.events);
            crate::identity::record_moments(w, &out.events);
        }
        Err(_) => {
            w.seq = prev_seq;
            w.now_ms = prev_now;
        }
    }
    r
}

fn apply_inner(w: &mut World, e: &Entry) -> Res<Outcome> {
    w.seq = e.seq;
    w.now_ms = e.at_ms;
    let rng = Rng::for_command(w.seed, e.seq);
    let mut ctx = Ctx { w, rng, events: vec![] };
    match (&e.actor, &e.cmd) {
        (_, Command::Register(a)) => crate::player::register(&mut ctx, a),
        (Actor::System, Command::Recover(a)) => crate::player::recover(&mut ctx, a),
        (Actor::System, Command::Tick) => crate::tick::tick(&mut ctx),
        (Actor::System, Command::Admin(a)) => crate::player::admin(&mut ctx, a),
        // Anonymous feedback (REST without a valid captain token).
        (Actor::System, Command::Feedback(a)) => crate::feedback::send(&mut ctx, None, a),
        (Actor::System, _) => Err(Refusal::new(
            "bad_actor",
            "System actor can only tick, register, administer, recover or send anonymous feedback.",
        )),
        (Actor::Owner(pid), Command::Owner(a)) => crate::owner::apply(&mut ctx, *pid, a),
        (Actor::Owner(_), _) => Err(Refusal::new(
            "forbidden",
            "Owners steer through the strategy and a note; only the captain flies the ship.",
        )),
        (Actor::Player(_), Command::Tick | Command::Admin(_) | Command::Owner(_) | Command::Recover(_)) => {
            Err(Refusal::new("forbidden", "Only the server may do that."))
        }
        (Actor::Player(pid), cmd) => {
            let pid = *pid;
            ctx.player(pid)?;
            // Feedback is not a game action: it works during an intermission and is not logged
            // to the captain's human.
            if let Command::Feedback(a) = cmd {
                return crate::feedback::send(&mut ctx, Some(pid), a);
            }
            // The improve board is about the server, not the galaxy: it also works during an
            // intermission, and it is not logged to the captain's human.
            if let Command::Improve(a) = cmd {
                return crate::improve::write(&mut ctx, pid, a);
            }
            // Muting mentions is about the captain's own inbox: it works during an intermission
            // and is not logged to the captain's human.
            if let Command::Mentions(a) = cmd {
                return crate::mention::write(&mut ctx, pid, a);
            }
            if ctx.w.season.finished {
                return Err(
                    Refusal::new("season_over", "The season has ended. Final standings are on the leaderboard.")
                        .hint("Read-only tools still work. The next season begins automatically after a short intermission (see get_leaderboard: next_season_tick)."),
                );
            }
            // Nothing is gated: the captain plays its human's strategy on its own judgement.
            let out = dispatch(&mut ctx, pid, cmd)?;
            crate::owner::log(ctx.w, pid, &out.summary);
            Ok(out)
        }
    }
}

fn dispatch(ctx: &mut Ctx, pid: PlayerId, cmd: &Command) -> Res<Outcome> {
    use crate::{combat, corp, nav, planet, player, social, stardock, trade};
    let out = match cmd {
        Command::Warp(a) => nav::warp(ctx, pid, a)?,
        Command::Autopilot(a) => nav::autopilot(ctx, pid, a)?,
        Command::Transwarp(a) => nav::transwarp(ctx, pid, a)?,
        Command::Scan(a) => nav::scan(ctx, pid, a)?,
        Command::Probe(a) => nav::probe(ctx, pid, a)?,
        Command::Trade(a) => trade::trade(ctx, pid, a)?,
        Command::TradeLoop(a) => crate::trade_loop::execute(ctx, pid, a)?,
        Command::Deploy(a) => {
            let mut out = combat::deploy(ctx, pid, a)?;
            if matches!(a.item_type, DeployItem::ArmidMines | DeployItem::LimpetMines) {
                let sid = ctx.w.players[&pid].sector;
                law_lines(&mut out, crate::government::on_mines(ctx, pid, sid));
                out.events.extend(std::mem::take(&mut ctx.events));
            }
            out
        }
        Command::Retrieve(a) => combat::retrieve(ctx, pid, a)?,
        Command::Attack(a) => {
            let victim = crate::diplomacy::attack_victim(ctx.w, pid, a);
            let sid = ctx.w.players.get(&pid).map(|p| p.sector).unwrap_or(0);
            let juris = crate::government::jurisdiction(ctx.w, sid);
            let mut out = combat::attack(ctx, pid, a)?;
            law_lines(&mut out, crate::government::on_attack(ctx, pid, sid, juris, victim));
            out.events.extend(std::mem::take(&mut ctx.events));
            if let Some(v) = victim {
                // Attacking a treaty partner's ship, fighters or planet breaks the treaty.
                let events = std::mem::take(&mut ctx.events);
                crate::diplomacy::on_hostile_act(ctx, pid, v);
                let mut out = out;
                out.events.extend(events);
                out.events.extend(std::mem::take(&mut ctx.events));
                out
            } else {
                out
            }
        }
        Command::Treaty(a) => crate::diplomacy::treaty(ctx, pid, a)?,
        Command::War(a) => crate::diplomacy::war(ctx, pid, a)?,
        Command::Intent(a) => crate::diplomacy::intent(ctx, pid, a)?,
        Command::Contract(a) => crate::contracts::contract(ctx, pid, a)?,
        Command::Career(a) => crate::careers::career(ctx, pid, a)?,
        Command::AskOwner(a) => crate::owner::ask(ctx, pid, a)?,
        Command::Identify(a) => crate::player::identify(ctx, pid, a)?,
        Command::BuyShip(a) => stardock::buy_ship(ctx, pid, a)?,
        Command::BuyEquipment(a) => stardock::buy_equipment(ctx, pid, a)?,
        Command::Bank(a) => stardock::bank(ctx, pid, a)?,
        Command::BuyColonists(a) => stardock::buy_colonists(ctx, pid, a)?,
        Command::Relief => stardock::relief(ctx, pid)?,
        Command::Genesis(a) => planet::genesis(ctx, pid, a)?,
        Command::ClaimPlanet(a) => planet::claim(ctx, pid, a)?,
        Command::Planet(a) => planet::manage(ctx, pid, a)?,
        Command::Corp(a) => corp::corp(ctx, pid, a)?,
        Command::Radio(a) => social::radio(ctx, pid, a)?,
        Command::Bounty(a) => social::bounty(ctx, pid, a)?,
        Command::Note(a) => social::note(ctx, pid, a)?,
        Command::Memory(a) => crate::memory::write(ctx, pid, a)?,
        Command::MarketReport(a) => crate::market_log::report(ctx, pid, a)?,
        Command::Language(a) => crate::language::write(ctx, pid, a)?,
        Command::MarkRead(a) => social::mark_read(ctx, pid, a)?,
        Command::Configure(a) => player::configure(ctx, pid, a)?,
        Command::Constitution(a) => crate::constitution::constitution(ctx, pid, a)?,
        Command::Government(a) => crate::government::government(ctx, pid, a)?,
        Command::Identity(a) => crate::identity::write(ctx, pid, a)?,
        Command::Chronicle(a) => crate::chronicle::write(ctx, pid, a)?,
        Command::Register(_)
        | Command::Tick
        | Command::Admin(_)
        | Command::Owner(_)
        | Command::Recover(_)
        | Command::Feedback(_)
        | Command::Improve(_)
        | Command::Mentions(_) => {
            unreachable!()
        }
    };
    Ok(out)
}

/// Compact snapshot of the actor attached to every mutating reply so agents
/// rarely need a follow-up status call.
pub fn brief(w: &World, pid: PlayerId) -> Value {
    let Some(p) = w.players.get(&pid) else { return Value::Null };
    json!({
        "sector": p.sector,
        "ap": p.ap,
        "credits": p.credits,
        "holds": {"total": p.ship.holds, "free": p.ship.free_holds(),
                  "fuel_ore": p.ship.cargo[0], "organics": p.ship.cargo[1], "equipment": p.ship.cargo[2],
                  "colonists": p.ship.colonists},
        "fighters": p.ship.fighters,
        "shields": p.ship.shields,
        "ship": p.ship.class,
    })
}

/// Add corporate-law lines (offences, seizures) to an action's reply and summary.
fn law_lines(out: &mut Outcome, lines: Vec<String>) {
    if lines.is_empty() {
        return;
    }
    out.summary.push_str(&format!(" {}", lines.join(" ")));
    if let Some(o) = out.reply.as_object_mut() {
        o.insert("law".into(), serde_json::json!(lines));
    }
}
