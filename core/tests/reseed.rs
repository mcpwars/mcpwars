//! The relaunch reseed (`af_core::reseed`): a new universe that keeps only call signs,
//! credentials, the latest identity card and each human's involvement setting.
use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::model::*;
use af_core::reseed::{self, ReseedOpts};
use af_core::{Command, World, identity};
use serde_json::{Value, json};

struct Sim {
    w: World,
    seq: u64,
}

impl Sim {
    fn run(&mut self, actor: Actor, cmd: Command) -> Result<af_core::Outcome, af_core::Refusal> {
        self.seq += 1;
        let e = Entry {
            seq: self.seq,
            at_ms: self.seq * 1000,
            actor,
            cmd,
        };
        let r = apply(&mut self.w, &e);
        if r.is_err() {
            self.seq -= 1;
        }
        r
    }
    fn ok(&mut self, actor: Actor, cmd: Value) -> af_core::Outcome {
        let c: Command = serde_json::from_value(cmd.clone()).unwrap_or_else(|e| panic!("{e}: {cmd}"));
        self.run(actor, c).unwrap_or_else(|r| panic!("{r}: {cmd}"))
    }
    fn join(&mut self, name: &str) -> PlayerId {
        let out = self
            .run(
                Actor::System,
                Command::Register(RegisterArgs {
                    name: name.into(),
                    kind: PlayerKind::Agent,
                    token_hash: format!("token-hash-{name}"),
                    model: Some("test-model".into()),
                    owner_token_hash: Some(format!("owner-hash-{name}")),
                    harness: Some("test-harness".into()),
                    autonomous: false,
                    recovery_hash: Some(format!("recovery-hash-{name}")),
                }),
            )
            .unwrap();
        out.reply["player_id"].as_u64().unwrap() as PlayerId
    }
}

fn card(home: &str) -> identity::Args {
    identity::Args {
        action: identity::Action::Set,
        age: Some(41),
        pronouns: Some("she/her".into()),
        homeworld: Some(home.into()),
        bio: Some("A pilot with a long memory.".into()),
        backstory: Some("Grew up on a freighter.".into()),
        ..Default::default()
    }
}

/// Unique strings put in every place past data can live. None may survive the reseed.
const SENTINELS: [&str; 25] = [
    "SENTINEL_NOTE_TEXT",
    "SENTINEL_NOTE_KEY",
    "SENTINEL_MEMORY_KEY",
    "SENTINEL_MEMORY_BODY",
    "SENTINEL_PRIVATE_RADIO",
    "SENTINEL_PUBLIC_RADIO",
    "SENTINELCORP",
    "SENTINEL_CHRONICLE_TITLE",
    "SENTINEL_CHRONICLE_BODY",
    "SENTINEL_IMPROVE_TITLE",
    "SENTINEL_IMPROVE_TEXT",
    "SENTINEL_FEEDBACK_ABOUT",
    "SENTINEL_FEEDBACK_TEXT",
    "SENTINEL_LANGUAGE_EXPLANATION",
    "SENTINEL_OLD_HOMEWORLD",
    "SENTINEL_IDENTITY_LOG",
    "SENTINEL_STRATEGY",
    "SENTINEL_MARKET_CLAIM",
    "SENTINEL_CONSTITUTION",
    "SENTINEL_SUMMARY",
    "SENTINEL_OWNER_STRATEGY",
    "SENTINEL_PUBLIC_LABEL",
    "SENTINEL_COACH",
    "SENTINEL_OPERATOR_MESSAGE",
    "SENTINEL_TREATY_TERMS",
];

/// A universe with two captains who used every system that keeps text.
fn played() -> (Sim, PlayerId, PlayerId) {
    let mut s = Sim {
        w: generate(&GenesisConfig::new(58, 120)),
        seq: 0,
    };
    s.ok(
        Actor::System,
        json!({"type": "admin", "args": {"action": "set_rules", "patch": {"improve_enabled": true}}}),
    );
    let a = s.join("Ada Mbeki");
    let b = s.join("Bram Ostrowski");
    let (pa, pb) = (Actor::Player(a), Actor::Player(b));
    // Identity: an old version, a log line, then the latest card.
    s.run(pa, Command::Identity(Box::new(card("SENTINEL_OLD_HOMEWORLD")))).unwrap();
    s.run(pb, Command::Identity(Box::new(card("Pell")))).unwrap();
    s.run(
        pa,
        Command::Identity(Box::new(identity::Args {
            action: identity::Action::Log,
            text: Some("SENTINEL_IDENTITY_LOG".into()),
            mood: Some("calm".into()),
            ..Default::default()
        })),
    )
    .unwrap();
    s.run(pa, Command::Identity(Box::new(card("Lowmoor Reach")))).unwrap();
    // Notes, memory, radio and the human channel.
    s.ok(
        pa,
        json!({"type": "note", "args": {"key": "plan", "value": "# Plan\nSENTINEL_NOTE_TEXT"}}),
    );
    s.ok(pa, json!({"type": "note", "args": {"key": "SENTINEL_NOTE_KEY", "value": "x"}}));
    s.ok(
        pa,
        json!({"type": "memory", "args": {"action": "save", "key": "SENTINEL_MEMORY_KEY", "data": {"v": "SENTINEL_MEMORY_BODY"}}}),
    );
    s.ok(
        pb,
        json!({"type": "radio", "args": {"channel": "private", "recipient_id": "Ada Mbeki", "message": "SENTINEL_PRIVATE_RADIO"}}),
    );
    s.ok(
        pa,
        json!({"type": "radio", "args": {"channel": "public", "message": "SENTINEL_PUBLIC_RADIO"}}),
    );
    s.ok(
        pa,
        json!({"type": "ask_owner", "args": {"action": "setup", "involvement": "strategy", "style": "trader"}}),
    );
    s.ok(
        pa,
        json!({"type": "ask_owner", "args": {"action": "strategy", "text": "SENTINEL_STRATEGY"}}),
    );
    s.ok(
        pa,
        json!({"type": "ask_owner", "args": {"action": "summary", "text": "SENTINEL_SUMMARY"}}),
    );
    s.ok(
        pa,
        json!({"type": "mentions", "args": {"action": "mute", "target": "Bram Ostrowski"}}),
    );
    // Shared places: a corporation (news), the Chronicle, the improve board, feedback, a language,
    // a market record and a constitution proposal.
    s.ok(pb, json!({"type": "corp", "args": {"action": "create", "name": "SENTINELCORP"}}));
    s.ok(
        pa,
        json!({"type": "chronicle", "args": {"action": "write", "kind": "event", "title": "SENTINEL_CHRONICLE_TITLE", "body": "SENTINEL_CHRONICLE_BODY"}}),
    );
    s.ok(
        pa,
        json!({"type": "improve", "args": {"action": "submit", "kind": "bug", "title": "SENTINEL_IMPROVE_TITLE", "text": "SENTINEL_IMPROVE_TEXT"}}),
    );
    s.ok(
        pa,
        json!({"type": "feedback", "args": {"about": "SENTINEL_FEEDBACK_ABOUT", "text": "SENTINEL_FEEDBACK_TEXT"}}),
    );
    s.ok(
        pa,
        json!({"type": "language", "args": {"action": "define", "name": "Qorvish", "explanation": "SENTINEL_LANGUAGE_EXPLANATION", "share": "captains"}}),
    );
    let port = s.w.sectors.iter().find(|x| x.fedspace && x.port.is_some()).unwrap().id;
    s.ok(
        Actor::System,
        json!({"type": "admin", "args": {"action": "set_player", "player": a, "sector": port, "ap": 500}}),
    );
    s.ok(
        pa,
        json!({"type": "market_report", "args": {"action": "report", "claim": {"note": "SENTINEL_MARKET_CLAIM"}}}),
    );
    s.ok(
        pa,
        json!({"type": "constitution", "args": {"action": "propose", "kind": "add", "text": "SENTINEL_CONSTITUTION"}}),
    );
    // The owner link (strategy, public label, coach note), an operator message and treaty terms.
    s.ok(
        Actor::Owner(b),
        json!({"type": "owner", "args": {"action": "set_orders", "strategy": "SENTINEL_OWNER_STRATEGY", "public_label": "SENTINEL_PUBLIC_LABEL"}}),
    );
    s.ok(
        Actor::Owner(b),
        json!({"type": "owner", "args": {"action": "coach", "text": "SENTINEL_COACH"}}),
    );
    s.ok(
        Actor::System,
        json!({"type": "admin", "args": {"action": "message", "player": a, "text": "SENTINEL_OPERATOR_MESSAGE"}}),
    );
    s.ok(
        pa,
        json!({"type": "treaty", "args": {"action": "propose", "kind": "nap", "captain": "Bram Ostrowski", "terms": "SENTINEL_TREATY_TERMS"}}),
    );
    for _ in 0..30 {
        s.ok(Actor::System, json!({"type": "tick"}));
    }
    (s, a, b)
}

#[test]
fn reseed_keeps_identities_only_and_counters_keep_counting() {
    let (s, a, b) = played();
    let old = &s.w;
    let old_json = serde_json::to_string(old).unwrap();
    for x in SENTINELS {
        assert!(old_json.contains(x), "the old world holds {x} (the test reached that place)");
    }
    let opts = ReseedOpts::for_world(old);
    let new = reseed::reseed(old, &opts);
    let new_json = serde_json::to_string(&new).unwrap();
    for x in SENTINELS {
        assert!(!new_json.contains(x), "{x} survived the reseed");
    }
    // The latest card is kept, and only it, renumbered 1.
    assert!(new_json.contains("Lowmoor Reach"));
    let card = &new.identities[&a];
    assert_eq!(card.versions.len(), 1);
    assert_eq!(card.versions[0].version, 1);
    assert_eq!(card.versions[0].homeworld, "Lowmoor Reach");
    assert!(card.log.is_empty() && card.moments.is_empty());
    assert!(identity::documented(&new, a) && identity::documented(&new, b));
    // Credentials byte for byte, and the human's setup; the strategy is cleared.
    for id in [a, b] {
        let (o, n) = (&old.players[&id], &new.players[&id]);
        assert_eq!(
            (&o.name, &o.token_hash, &o.owner_token_hash, &o.recovery_hash),
            (&n.name, &n.token_hash, &n.owner_token_hash, &n.recovery_hash)
        );
        assert_eq!(
            (&o.model, &o.harness, o.autonomous, o.kind),
            (&n.model, &n.harness, n.autonomous, n.kind)
        );
        assert_eq!(o.owner.prefs, n.owner.prefs);
        assert!(n.owner.orders.strategy.is_none() && n.owner.orders.public_label.is_none());
        assert!(n.owner.messages.is_empty() && n.owner.log.is_empty() && n.owner.coach.is_none());
        assert!(n.notes.is_empty() && n.memory.is_empty() && n.memory_revisions.is_empty() && n.inbox.is_empty());
        assert!(n.next_notice >= o.next_notice && n.owner.next_message >= o.owner.next_message);
        assert_eq!(n.radio_cursor, new.next_msg - 1);
        assert!(new.is_fedspace(n.sector));
        assert!(n.corp.is_none());
        assert_eq!(n.announcements_seen, new.next_announcement);
    }
    assert_eq!(new.players[&a].owner.strategy_revision, old.players[&a].owner.strategy_revision + 1);
    assert_eq!(new.players[&a].owner.prefs.as_ref().unwrap().style.as_deref(), Some("trader"));
    // Captain mutes carry.
    assert!(new.mentions.mutes[&a].captains.contains(&b));
    // Every counter is at least where it was; the epoch moves on.
    let (oc, nc) = (reseed::counters(old), reseed::counters(&new));
    for (k, v) in &oc {
        if k == "archive.epoch" {
            assert_eq!(nc[k], v + 1);
        } else {
            assert!(nc[k] >= *v, "{k}: {} < {v}", nc[k]);
        }
    }
    // NPCs come after every captain, and no id is reused.
    let npcs: Vec<PlayerId> = new.players.values().filter(|p| p.npc.is_some()).map(|p| p.id).collect();
    assert!(!npcs.is_empty());
    assert!(npcs.iter().all(|n| *n >= old.next_player), "NPC ids above every old id");
    assert!(new.next_player > *npcs.iter().max().unwrap());
    let concession_ids: Vec<u64> = new.concessions.keys().copied().collect();
    assert!(
        concession_ids
            .iter()
            .all(|c| *c >= old.next_registry_id && *c < new.next_registry_id)
    );
    assert!(new.planets.keys().all(|p| *p < new.next_planet));
    // Planets are numbered from the old counter: a cached planet id never names a new planet.
    assert!(!new.planets.is_empty());
    assert!(new.planets.iter().all(|(k, p)| *k == p.id && *k >= old.next_planet));
    for sec in &new.sectors {
        assert!(sec.planets.iter().all(|id| new.planets[id].sector == sec.id));
    }
    // A new season 1, in its founding era: the founding kit is aboard.
    assert_eq!(new.seq, 0);
    assert_eq!((new.season.number, new.season.started_tick, new.tick), (1, old.tick, old.tick));
    assert_eq!(new.archive.epoch, old.archive.epoch + 1);
    assert!(af_core::founding::founding(&new).is_some());
    let ship = &new.players[&a].ship;
    assert!(ship.genesis >= new.rules.founding_genesis && ship.probes >= new.rules.founding_probes);
    assert_eq!(new.news.len(), 1);
    assert!(new.news[0].headline.starts_with("A NEW UNIVERSE"));
    let counts = reseed::counts(&new);
    for k in [
        "corps",
        "radio",
        "chronicle_pages",
        "improve_items",
        "feedback",
        "languages",
        "market_reports",
        "constitution_proposals",
        "notes",
        "memory",
        "inbox",
        "strategies",
    ] {
        assert_eq!(counts[k], 0, "{k}");
    }
    assert_eq!(counts["captains"], 2);
    // Deterministic: the same input builds the same world; another seed another galaxy.
    assert_eq!(reseed::reseed(old, &opts).state_hash(), new.state_hash());
    assert_ne!(
        reseed::reseed(old, &ReseedOpts { seed: opts.seed ^ 1 }).state_hash(),
        new.state_hash()
    );
}

#[test]
fn a_reseeded_world_plays_and_replays_identically() {
    let (s, a, _) = played();
    let base = reseed::reseed(&s.w, &ReseedOpts::for_world(&s.w));
    let mut w = base.clone();
    let mut journal = vec![];
    let mut push = |w: &mut World, actor: Actor, cmd: Command| {
        let e = Entry {
            seq: w.seq + 1,
            at_ms: w.now_ms + 60_000,
            actor,
            cmd,
        };
        let out = apply(w, &e);
        if out.is_ok() {
            journal.push(e);
        }
        out
    };
    push(&mut w, Actor::System, Command::Tick).unwrap();
    let to = w.sector(w.players[&a].sector).unwrap().warps[0];
    push(&mut w, Actor::Player(a), Command::Warp(WarpArgs { destination_sector: to })).unwrap();
    push(
        &mut w,
        Actor::Player(a),
        serde_json::from_value(json!({"type": "note", "args": {"key": "plan", "value": "# New plan"}})).unwrap(),
    )
    .unwrap();
    push(&mut w, Actor::System, Command::Tick).unwrap();
    assert_eq!(w.seq, 4);
    assert_eq!(w.players[&a].sector, to);
    let mut again = base.clone();
    for e in &journal {
        let back: Entry = serde_json::from_str(&serde_json::to_string(e).unwrap()).unwrap();
        apply(&mut again, &back).unwrap();
    }
    assert_eq!(again.state_hash(), w.state_hash());
    // The base itself round-trips through JSON (the store writes it as base_world).
    let back: World = serde_json::from_str(&serde_json::to_string(&base).unwrap()).unwrap();
    assert_eq!(back.state_hash(), base.state_hash());
}
