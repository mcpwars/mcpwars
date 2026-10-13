//! Mentions (engine 1.58): radio mentions, who is told, the limit, mutes, the inbox, the
//! briefing and moderation of the copies.
use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::mention::{self, SeasonSel};
use af_core::model::*;
use af_core::query::{self, BriefingArgs, InboxArgs};
use af_core::{Outcome, Refusal, World};
use serde_json::{Value, json};

struct Sim {
    w: World,
    journal: Vec<Entry>,
    cfg: GenesisConfig,
}

impl Sim {
    fn new(seed: u64) -> Self {
        Self::with(seed, |_| {})
    }
    /// A Sim whose rules are set before anything is journaled, so replay sees them too.
    fn with(seed: u64, f: impl Fn(&mut af_core::rules::Rules)) -> Self {
        let mut cfg = GenesisConfig::new(seed, 60);
        f(&mut cfg.rules);
        Sim {
            w: generate(&cfg),
            journal: vec![],
            cfg,
        }
    }
    fn run(&mut self, actor: Actor, cmd: Command) -> Result<Outcome, Refusal> {
        let e = Entry {
            seq: self.journal.len() as u64 + 1,
            at_ms: self.journal.len() as u64 * 1000,
            actor,
            cmd,
        };
        self.journal.push(e.clone());
        apply(&mut self.w, &e)
    }
    fn tick(&mut self) {
        self.run(Actor::System, Command::Tick).unwrap();
    }
    fn join(&mut self, name: &str) -> PlayerId {
        let out = self
            .run(
                Actor::System,
                Command::Register(RegisterArgs {
                    name: name.into(),
                    kind: PlayerKind::Agent,
                    token_hash: format!("h-{name}"),
                    model: Some("test".into()),
                    owner_token_hash: None,
                    harness: None,
                    autonomous: false,
                    recovery_hash: None,
                }),
            )
            .unwrap();
        out.reply["player_id"].as_u64().unwrap() as PlayerId
    }
    fn p(&self, id: PlayerId) -> &Player {
        &self.w.players[&id]
    }
    fn radio(&mut self, pid: PlayerId, args: Value) -> Result<Outcome, Refusal> {
        self.run(Actor::Player(pid), Command::Radio(serde_json::from_value(args).unwrap()))
    }
    fn mentions(&mut self, pid: PlayerId, args: Value) -> Result<Outcome, Refusal> {
        self.run(Actor::Player(pid), Command::Mentions(serde_json::from_value(args).unwrap()))
    }
    fn corp(&mut self, pid: PlayerId, args: Value) {
        self.run(Actor::Player(pid), Command::Corp(serde_json::from_value(args).unwrap()))
            .unwrap();
    }
    /// A corporation `name` led by `ceo` with `members`.
    fn found(&mut self, ceo: PlayerId, name: &str, members: &[PlayerId]) -> CorpId {
        self.corp(ceo, json!({"action": "create", "name": name}));
        for m in members {
            let who = self.p(*m).name.clone();
            self.corp(ceo, json!({"action": "invite", "player": who}));
            self.corp(*m, json!({"action": "join", "name": name}));
        }
        self.p(ceo).corp.unwrap()
    }
    fn mention_notices(&self, pid: PlayerId) -> Vec<Notice> {
        self.p(pid).inbox.iter().filter(|n| n.kind == "mention").cloned().collect()
    }
}

fn replay_matches(s: &Sim) {
    let mut w = generate(&s.cfg);
    for e in &s.journal {
        let _ = apply(&mut w, e);
    }
    assert_eq!(w.state_hash(), s.w.state_hash(), "replay reproduces the world");
}

fn mark_read(s: &mut Sim, pid: PlayerId, notices: Vec<u64>, radio: Vec<u64>) {
    s.run(
        Actor::Player(pid),
        Command::MarkRead(MarkReadArgs {
            notice_up_to: 0,
            radio_up_to: 0,
            announcements_up_to: None,
            notice_ids: Some(notices),
            radio_ids: Some(radio),
        }),
    )
    .unwrap();
}

#[test]
fn an_unknown_entry_refuses_with_the_closest_names_and_changes_nothing() {
    let mut s = Sim::new(501);
    let a = s.join("Ada Quill");
    s.join("Vex Arden");
    let before = s.w.state_hash();
    let next = s.w.next_msg;
    let e = s
        .radio(a, json!({"message": "hello", "channel": "public", "mentions": ["Vex Ardn"]}))
        .unwrap_err();
    assert_eq!(e.code, "unknown_mention");
    assert!(e.message.contains("Vex Ardn"), "{}", e.message);
    assert!(e.hint.as_deref().unwrap_or("").contains("Vex Arden"), "{:?}", e.hint);
    assert_eq!(s.w.state_hash(), before, "a refusal changes nothing");
    assert_eq!(s.w.next_msg, next);

    let many: Vec<String> = (0..21).map(|_| "Vex Arden".to_string()).collect();
    let e = s
        .radio(a, json!({"message": "hello", "channel": "public", "mentions": many}))
        .unwrap_err();
    assert_eq!(e.code, "too_many_mentions");
    assert_eq!(s.w.state_hash(), before);
    replay_matches(&s);
}

#[test]
fn private_radio_takes_no_mentions_and_inline_names_there_stay_text() {
    let mut s = Sim::new(502);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    let e = s
        .radio(
            a,
            json!({"message": "psst", "channel": "private", "recipient_id": "Vex Arden", "mentions": ["Vex Arden"]}),
        )
        .unwrap_err();
    assert_eq!(e.code, "mentions_not_here");
    assert!(e.hint.as_deref().unwrap_or("").contains("public and corp radio"));
    let out = s
        .radio(
            a,
            json!({"message": "psst @Vex Arden", "channel": "private", "recipient_id": "Vex Arden"}),
        )
        .unwrap();
    assert!(out.reply.get("mentions").is_none());
    assert!(
        s.w.radio.last().unwrap().mentions.is_empty(),
        "private inline mentions are not stored"
    );
    assert!(s.mention_notices(v).is_empty());
    // An unknown inline name on public radio is plain text: nothing stored, nothing refused.
    let out = s
        .radio(a, json!({"message": "@Nobody Here and a@b.c", "channel": "public"}))
        .unwrap();
    assert!(out.reply.get("mentions").is_none());
    assert!(s.w.radio.last().unwrap().mentions.is_empty());
    replay_matches(&s);
}

#[test]
fn public_mentions_notify_with_an_untrusted_excerpt_once() {
    let mut s = Sim::new(503);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    let out = s
        .radio(
            a,
            json!({"message": "Ore at the dock, @vex-arden: come and get it. And me, @Ada Quill.", "channel": "public"}),
        )
        .unwrap();
    assert_eq!(out.reply["mentions"]["resolved"], json!(["Vex Arden", "Ada Quill"]));
    assert_eq!(out.reply["mentions"]["notified"], json!(["Vex Arden"]));
    assert!(out.summary.contains("1 mentioned, 1 notified"), "{}", out.summary);
    let msg = s.w.radio.last().unwrap().clone();
    assert_eq!(msg.mentions.captains, vec![v, a], "stored in first-appearance order, self included");
    // Self is never told.
    assert!(s.mention_notices(a).is_empty());
    let ns = s.mention_notices(v);
    assert_eq!(ns.len(), 1);
    let n = &ns[0];
    assert!(n.text.starts_with("Ada Quill mentioned you on public radio"), "{}", n.text);
    assert!(!n.text.contains("come and get it"), "player words never enter the server text");
    assert!(n.quote.as_deref().unwrap().contains("@vex-arden"));
    assert_eq!(n.about, Some(mention::Place::Radio { id: msg.id, corp: None }));

    // read_inbox kind=mention: the quote is untrusted player text and the mention says where.
    let (view, _, _) = query::inbox(
        &s.w,
        v,
        &InboxArgs {
            kind: Some("mention".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let rows = view["notices"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0]["quote"]["untrusted_player_text"].as_str().unwrap().contains("@vex-arden"));
    assert_eq!(rows[0]["mention"]["from"], "Ada Quill");
    assert_eq!(rows[0]["mention"]["where"]["surface"], "radio");
    assert!(view["radio"].as_array().unwrap().is_empty(), "a kind filter excludes radio");
    // The radio row names who was mentioned and flags the reader.
    let (all, _, _) = query::inbox(&s.w, v, &InboxArgs::default()).unwrap();
    let row = all["radio"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!(msg.id))
        .unwrap()
        .clone();
    assert_eq!(row["mentions"], json!(["Vex Arden", "Ada Quill"]));
    assert_eq!(row["mentions_you"], true);

    // Digest and acknowledge_only strip the quote.
    let (digest, _, _) = query::inbox(
        &s.w,
        v,
        &InboxArgs {
            digest: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(digest["notices"].as_array().unwrap().iter().all(|r| r.get("quote").is_none()));
    replay_matches(&s);
}

#[test]
fn corporation_mentions_reach_members_once_and_cost_one_unit() {
    let mut s = Sim::new(504);
    let a = s.join("Ada Quill");
    let (b, c, d) = (s.join("Bex Hollow"), s.join("Cato Rhee"), s.join("Dov Pell"));
    let outsider = s.join("Ezra Lune");
    let k = s.found(b, "Iron Tide", &[c, d]);
    // a is a member too: the sender is never told.
    s.corp(b, json!({"action": "invite", "player": "Ada Quill"}));
    s.corp(a, json!({"action": "join", "name": "Iron Tide"}));
    let left = mention::left(&s.w, a);
    let out = s
        .radio(a, json!({"message": "Muster at dawn, @corp:Iron Tide.", "channel": "public"}))
        .unwrap();
    assert_eq!(mention::left(&s.w, a), left - 1, "a corporation counts once");
    for m in [b, c, d] {
        assert_eq!(s.mention_notices(m).len(), 1, "one notice per member");
    }
    assert!(s.mention_notices(a).is_empty(), "the sender is never told");
    let n = &s.mention_notices(b)[0];
    assert!(n.text.contains("your corporation Iron Tide"), "{}", n.text);
    assert_eq!(n.mention.as_ref().unwrap().via_corp.as_ref().unwrap().id, k);
    assert_eq!(out.reply["mentions"]["notified"], json!(["corp:Iron Tide (3 members)"]));
    assert!(out.reply["mentions"].get("not_notified").is_none(), "{}", out.reply);

    // A captain named directly and through its corporation gets one notice.
    s.tick();
    let out = s
        .radio(
            a,
            json!({"message": "@Cato Rhee leads; @corp:Iron Tide follows.", "channel": "public"}),
        )
        .unwrap();
    assert_eq!(
        out.reply["mentions"]["notified"],
        json!(["Cato Rhee", "corp:Iron Tide (2 members)"])
    );
    for m in [b, c, d] {
        assert_eq!(s.mention_notices(m).len(), 2, "one more notice each");
    }
    assert!(s.mention_notices(c).last().unwrap().mention.as_ref().unwrap().via_corp.is_none());

    // Corporation radio: an outsider cannot read it.
    let out = s
        .radio(a, json!({"message": "Inside talk about @Ezra Lune.", "channel": "corp"}))
        .unwrap();
    assert_eq!(
        out.reply["mentions"]["not_notified"],
        json!([{"name": "Ezra Lune", "why": "cannot_read"}])
    );
    assert!(s.mention_notices(outsider).is_empty());
    assert_eq!(
        s.w.radio.last().unwrap().mentions.captains,
        vec![outsider],
        "stored even when no one is told"
    );
    // A member mentioned on corporation radio is told, and the notice says where.
    s.radio(a, json!({"message": "@Dov Pell, the cargo.", "channel": "corp"})).unwrap();
    let last = s.mention_notices(d).last().unwrap().clone();
    assert!(last.text.contains("on corporation radio"), "{}", last.text);
    replay_matches(&s);
}

#[test]
fn the_limit_stores_every_mention_and_says_who_was_not_told() {
    let mut s = Sim::with(505, |r| {
        r.mention_notify_max = 2;
        r.mention_window_ticks = 5;
    });
    let a = s.join("Ada Quill");
    let names = ["Bex Hollow", "Cato Rhee", "Dov Pell"];
    let ids: Vec<PlayerId> = names.iter().map(|n| s.join(n)).collect();
    let t0 = s.w.tick;
    s.radio(a, json!({"message": "@Bex Hollow hi", "channel": "public"})).unwrap();
    s.tick();
    let out = s.radio(a, json!({"message": "@Cato Rhee hi", "channel": "public"})).unwrap();
    assert_eq!(out.reply["mentions"]["cap"]["left"], 0);
    let out = s.radio(a, json!({"message": "@Dov Pell hi", "channel": "public"})).unwrap();
    assert_eq!(
        out.reply["mentions"]["not_notified"],
        json!([{"name": "Dov Pell", "why": "over_cap"}])
    );
    assert_eq!(out.reply["mentions"]["cap"]["frees_at_tick"], json!(t0 + 5));
    assert_eq!(
        s.w.radio.last().unwrap().mentions.captains,
        vec![ids[2]],
        "the item and its mentions are stored"
    );
    assert!(s.mention_notices(ids[2]).is_empty());
    // The window slides at exactly the named tick.
    while s.w.tick < t0 + 5 {
        s.tick();
    }
    let out = s.radio(a, json!({"message": "@Dov Pell again", "channel": "public"})).unwrap();
    assert_eq!(out.reply["mentions"]["notified"], json!(["Dov Pell"]));
    assert_eq!(s.mention_notices(ids[2]).len(), 1);
    replay_matches(&s);
}

#[test]
fn a_limit_of_zero_tells_no_one_and_names_no_tick() {
    let mut s = Sim::with(506, |r| r.mention_notify_max = 0);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    let out = s.radio(a, json!({"message": "@Vex Arden", "channel": "public"})).unwrap();
    assert_eq!(out.reply["mentions"]["not_notified"][0]["why"], "over_cap");
    assert_eq!(out.reply["mentions"]["cap"]["left"], 0);
    assert!(out.reply["mentions"]["cap"].get("frees_at_tick").is_none());
    assert!(s.mention_notices(v).is_empty());
    assert_eq!(s.w.radio.last().unwrap().mentions.captains, vec![v]);
    replay_matches(&s);
}

#[test]
fn a_mute_drops_the_notice_silently_and_unmute_restores_it() {
    let mut s = Sim::new(507);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    let out = s.mentions(v, json!({"action": "mute", "target": "ada quill"})).unwrap();
    assert_eq!(out.reply, json!({"muted": "Ada Quill", "changed": true}));
    let out = s.mentions(v, json!({"action": "mute", "target": "Ada Quill"})).unwrap();
    assert_eq!(out.reply["changed"], false);
    assert_eq!(
        s.mentions(v, json!({"action": "mute", "target": "Vex Arden"})).unwrap_err().code,
        "invalid_target"
    );
    assert_eq!(
        s.mentions(v, json!({"action": "mute", "target": "Nobody"})).unwrap_err().code,
        "unknown_mention"
    );
    let out = s.radio(a, json!({"message": "@Vex Arden", "channel": "public"})).unwrap();
    assert_eq!(
        out.reply["mentions"]["notified"],
        json!(["Vex Arden"]),
        "the sender is not told about mutes"
    );
    assert!(s.mention_notices(v).is_empty());
    let listed = mention::read(&s.w, v, &serde_json::from_value(json!({"action": "list"})).unwrap()).unwrap();
    assert_eq!(listed["muted"]["captains"], json!(["Ada Quill"]));
    s.mentions(v, json!({"action": "unmute", "target": "Ada Quill"})).unwrap();
    s.tick();
    s.radio(a, json!({"message": "@Vex Arden", "channel": "public"})).unwrap();
    assert_eq!(s.mention_notices(v).len(), 1);
    // The operator view counts the mentions, the muted one and the mute pairs.
    s.mentions(v, json!({"action": "mute", "target": "Ada Quill"})).unwrap();
    let ov = mention::operator_view(&s.w, SeasonSel::Current, None, None, 10, None);
    assert_eq!(ov["totals"]["mentions"], 2);
    assert_eq!(ov["totals"]["notified"], 1);
    assert_eq!(ov["totals"]["muted"], 1);
    assert_eq!(ov["mutes"]["pairs"].as_array().unwrap().len(), 1);
    replay_matches(&s);
}

#[test]
fn mutes_work_in_an_intermission_and_survive_a_fresh_start() {
    let mut s = Sim::new(508);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    s.mentions(v, json!({"action": "mute", "target": "Ada Quill"})).unwrap();
    s.run(Actor::System, Command::Admin(AdminArgs::FreshStart)).unwrap();
    assert!(s.w.mentions.mutes[&v].captains.contains(&a));
    replay_matches(&s);
    // During an intermission the tool still works (the world is edited directly here).
    s.w.season.finished = true;
    let out = s.mentions(v, json!({"action": "unmute", "target": "Ada Quill"})).unwrap();
    assert_eq!(out.reply["changed"], true);
    assert!(!s.w.mentions.mutes.contains_key(&v));
}

#[test]
fn the_briefing_counts_mentions_apart_from_other_notices() {
    let mut s = Sim::new(509);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    s.radio(a, json!({"message": "Hello @Vex Arden", "channel": "public"})).unwrap();
    let b = query::briefing(&s.w, v).unwrap();
    assert!(
        b["unread_notices"].as_array().unwrap().iter().all(|n| n["kind"] != "mention"),
        "pure mention notices leave the preview"
    );
    assert_eq!(b["comms"]["mentions"]["unread"], 1);
    assert!(
        b["comms"]["mentions"]["latest"]
            .as_str()
            .unwrap()
            .starts_with("Ada Quill on public radio")
    );
    assert_eq!(b["inbox_continuation"]["unread_mentions_total"], 1);
    assert_eq!(query::status(&s.w, v).unwrap()["unread_mentions"], 1);
    let only = query::briefing_with(
        &s.w,
        v,
        &BriefingArgs {
            sections: Some(vec![]),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(only["inbox_continuation"]["unread_mentions_total"], 1);
    let compact = query::briefing_with(
        &s.w,
        v,
        &BriefingArgs {
            compact: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(compact["comms"]["mentions"]["unread"], 1);

    // The hint: only mentions unread, then mentions beside other notices.
    let pure: Vec<u64> = s
        .p(v)
        .inbox
        .iter()
        .filter(|n| n.kind != "mention" && !n.read)
        .map(|n| n.id)
        .collect();
    mark_read(&mut s, v, pure, vec![]);
    let h = query::hints(&s.w, v, true);
    assert!(h.iter().any(|l| l == "1 unread mention(s): read_inbox kind=mention."), "{h:?}");
    s.run(
        Actor::System,
        Command::Admin(AdminArgs::Message {
            player: Some(v),
            text: "Operator test".into(),
            tell_owner: false,
        }),
    )
    .unwrap();
    let h = query::hints(&s.w, v, true);
    assert!(h.iter().any(|l| l == "1 unread notice(s): read_inbox (1 mention(s))."), "{h:?}");
    let ids: Vec<u64> = s.p(v).inbox.iter().map(|n| n.id).collect();
    mark_read(&mut s, v, ids, vec![]);
    let b = query::briefing(&s.w, v).unwrap();
    assert!(b["comms"].get("mentions").is_none());
    replay_matches(&s);
}

#[test]
fn a_rider_notice_counts_as_a_mention_and_compact_strips_quotes() {
    let mut s = Sim::new(510);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    // A notice the captain got anyway, carrying a mention (as Chronicle and constitution writes
    // send them).
    let p = s.w.players.get_mut(&v).unwrap();
    let id = p.next_notice;
    p.next_notice += 1;
    p.inbox.push(Notice {
        id,
        tick: 0,
        at_ms: 0,
        kind: "constitution".into(),
        text: "Proposal #1 is open. It mentions you.".into(),
        read: false,
        quote: Some("A rule for @Vex Arden".into()),
        about: Some(mention::Place::Proposal { id: 1 }),
        mention: Some(mention::MentionRef {
            from: a,
            from_name: "Ada Quill".into(),
            via_corp: None,
        }),
        fold: None,
        superseded_by: None,
        struck_quote: None,
    });
    let (view, _, _) = query::inbox(
        &s.w,
        v,
        &InboxArgs {
            kind: Some("mention".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(view["notices"].as_array().unwrap().len(), 1, "riders are mentions too");
    let b = query::briefing(&s.w, v).unwrap();
    let row = b["unread_notices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == json!(id))
        .unwrap()
        .clone();
    assert_eq!(
        row["quote"]["untrusted_player_text"], "A rule for @Vex Arden",
        "riders stay in the preview"
    );
    assert_eq!(b["comms"]["mentions"]["latest"], "Ada Quill on constitution proposal #1");
    let compact = query::briefing_with(
        &s.w,
        v,
        &BriefingArgs {
            compact: true,
            ..Default::default()
        },
    )
    .unwrap();
    let row = compact["unread_notices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == json!(id))
        .unwrap()
        .clone();
    assert!(row.get("quote").is_none() && row.get("text").is_none());
}

#[test]
fn deleting_a_radio_message_scrubs_the_quotes_of_its_notices() {
    let mut s = Sim::new(511);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    s.radio(a, json!({"message": "Rude words for @Vex Arden", "channel": "public"}))
        .unwrap();
    let id = s.w.radio.last().unwrap().id;
    s.run(Actor::System, Command::Admin(AdminArgs::DeleteRadio { id })).unwrap();
    let n = &s.mention_notices(v)[0];
    assert_eq!(n.quote.as_deref(), Some(mention::STRUCK));
    assert!(n.struck_quote.as_deref().unwrap().contains("Rude words"));
    let (view, _, _) = query::inbox(
        &s.w,
        v,
        &InboxArgs {
            include_read: true,
            ..Default::default()
        },
    )
    .unwrap();
    let text = view.to_string();
    assert!(!text.contains("Rude words"), "{text}");
    assert!(!text.contains("struck_quote"));
    replay_matches(&s);
}

#[test]
fn the_mentions_tool_is_not_logged_to_the_human_and_never_spends_ap() {
    let mut s = Sim::new(512);
    s.join("Ada Quill");
    let v = s
        .run(
            Actor::System,
            Command::Register(RegisterArgs {
                name: "Vex Arden".into(),
                kind: PlayerKind::Agent,
                token_hash: "h-vex".into(),
                model: Some("test".into()),
                owner_token_hash: Some("o-vex".into()),
                harness: None,
                autonomous: false,
                recovery_hash: None,
            }),
        )
        .unwrap()
        .reply["player_id"]
        .as_u64()
        .unwrap() as PlayerId;
    // An ordinary action is logged to the human; muting is not.
    s.radio(v, json!({"message": "hello", "channel": "public"})).unwrap();
    assert!(!s.p(v).owner.log.is_empty());
    let ap = s.p(v).ap;
    let logged = s.p(v).owner.log.len();
    s.mentions(v, json!({"action": "mute", "target": "Ada Quill"})).unwrap();
    assert_eq!(s.p(v).ap, ap);
    assert_eq!(s.p(v).owner.log.len(), logged);
    replay_matches(&s);
}

#[test]
fn naming_only_yourself_mentions_no_one() {
    let mut s = Sim::new(513);
    let a = s.join("Ada Quill");
    let out = s
        .radio(a, json!({"message": "Note to self, @Ada Quill.", "channel": "public"}))
        .unwrap();
    assert_eq!(out.summary, "Message transmitted.", "no \"0 mentioned\" line");
    assert!(out.reply["mentions"].get("notified").is_none());
    assert!(s.mention_notices(a).is_empty());
    assert!(s.w.mentions.edges.is_empty(), "self never enters the record");
    replay_matches(&s);
}

#[test]
fn a_corporation_mute_drops_its_members_mentions_and_acknowledgements_strip_quotes() {
    let mut s = Sim::new(514);
    let a = s.join("Ada Quill");
    let b = s.join("Bex Hollow");
    let v = s.join("Vex Arden");
    s.found(a, "Iron Tide", &[b]);
    s.mentions(v, json!({"action": "mute", "target": "corp:iron tide"})).unwrap();
    let out = s.radio(b, json!({"message": "@Vex Arden, a word.", "channel": "public"})).unwrap();
    assert_eq!(out.reply["mentions"]["notified"], json!(["Vex Arden"]), "the sender is not told");
    assert!(s.mention_notices(v).is_empty(), "a member of a muted corporation tells no one");
    s.corp(b, json!({"action": "leave"}));
    s.radio(b, json!({"message": "@Vex Arden, now on my own.", "channel": "public"}))
        .unwrap();
    assert_eq!(s.mention_notices(v).len(), 1, "the mute follows the sender's current corporation");
    let (view, _, _) = query::inbox(
        &s.w,
        v,
        &InboxArgs {
            acknowledge_only: true,
            kind: Some("mention".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let rows = view["notices"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].get("quote").is_none() && rows[0].get("text").is_none());
    assert_eq!(rows[0]["mention"]["from"], "Bex Hollow");
    replay_matches(&s);
}

#[test]
fn a_mute_whose_target_is_gone_can_still_be_undone() {
    let mut s = Sim::new(515);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    let gone = s.join("Ghost Wren");
    let k = s.found(a, "Iron Tide", &[]);
    s.mentions(v, json!({"action": "mute", "target": "corp:Iron Tide"})).unwrap();
    s.mentions(v, json!({"action": "mute", "target": "Ghost Wren"})).unwrap();
    s.run(Actor::System, Command::Admin(AdminArgs::DisbandCorp { corp: k })).unwrap();
    s.run(Actor::System, Command::Admin(AdminArgs::RemovePlayer { player: gone }))
        .unwrap();
    let listed = mention::read(&s.w, v, &serde_json::from_value(json!({"action": "list"})).unwrap()).unwrap();
    assert_eq!(listed["muted"]["corps"], json!([format!("corp #{k}")]));
    assert_eq!(listed["muted"]["captains"], json!([format!("#{gone}")]));
    // The names are gone, so they no longer resolve...
    let before = s.w.state_hash();
    assert_eq!(
        s.mentions(v, json!({"action": "unmute", "target": "Iron Tide"})).unwrap_err().code,
        "unknown_mention"
    );
    // ...and the forms the list shows never mute anything new.
    assert_eq!(
        s.mentions(v, json!({"action": "mute", "target": format!("#{gone}")}))
            .unwrap_err()
            .code,
        "unknown_mention"
    );
    assert_eq!(s.w.state_hash(), before);
    // The forms the list shows undo them.
    let out = s.mentions(v, json!({"action": "unmute", "target": format!("corp #{k}")})).unwrap();
    assert_eq!(out.reply, json!({"unmuted": format!("corp #{k}"), "changed": true}));
    let out = s.mentions(v, json!({"action": "unmute", "target": format!("#{gone}")})).unwrap();
    assert_eq!(out.reply, json!({"unmuted": format!("#{gone}"), "changed": true}));
    assert!(!s.w.mentions.mutes.contains_key(&v));
    // A live captain's id form is not a name: it still refuses.
    s.mentions(v, json!({"action": "mute", "target": "Ada Quill"})).unwrap();
    assert_eq!(
        s.mentions(v, json!({"action": "unmute", "target": format!("#{a}")}))
            .unwrap_err()
            .code,
        "unknown_mention"
    );
    replay_matches(&s);
}

// ---------- identity log lines and constitution proposals (S5) ----------

fn document(s: &mut Sim, pid: PlayerId) {
    let a = af_core::identity::Args {
        action: af_core::identity::Action::Set,
        age: Some(40),
        pronouns: Some("she/her".into()),
        homeworld: Some("Lowmoor".into()),
        bio: Some("A pilot.".into()),
        backstory: Some("## Family\nNone.".into()),
        ..Default::default()
    };
    s.run(Actor::Player(pid), Command::Identity(Box::new(a))).unwrap();
}

fn identity(s: &mut Sim, pid: PlayerId, args: Value) -> Result<Outcome, Refusal> {
    s.run(
        Actor::Player(pid),
        Command::Identity(Box::new(serde_json::from_value(args).unwrap())),
    )
}

fn constitution(s: &mut Sim, pid: PlayerId, args: Value) -> Result<Outcome, Refusal> {
    s.run(Actor::Player(pid), Command::Constitution(serde_json::from_value(args).unwrap()))
}

#[test]
fn public_log_lines_mention_and_private_ones_tell_no_one() {
    let mut s = Sim::new(521);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    document(&mut s, a);
    // Structured mentions on a private line or another identity action are refused, changing nothing.
    let before = s.w.state_hash();
    for args in [
        json!({"action": "log", "text": "I think of Vex.", "private": true, "mentions": ["Vex Arden"]}),
        json!({"action": "set", "bio": "A pilot who knows Vex.", "mentions": ["Vex Arden"]}),
    ] {
        let e = identity(&mut s, a, args).unwrap_err();
        assert_eq!(e.code, "mentions_not_here", "{e}");
        assert!(e.hint.as_deref().unwrap_or("").contains("public log lines"));
    }
    let e = af_core::identity::read(
        &s.w,
        a,
        &serde_json::from_value(json!({"action": "story", "mentions": ["Vex Arden"]})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, "mentions_not_here");
    assert_eq!(s.w.state_hash(), before, "a refusal changes nothing");
    // An inline name on a private line is plain text: nothing stored, no one told.
    let out = identity(
        &mut s,
        a,
        json!({"action": "log", "text": "I think of @Vex Arden.", "private": true}),
    )
    .unwrap();
    assert!(out.reply.get("mentions").is_none());
    assert!(s.w.identities[&a].log.last().unwrap().mentions.is_empty());
    assert!(s.mention_notices(v).is_empty());
    // A public line tells the captain it names.
    let left = mention::left(&s.w, a);
    let out = identity(&mut s, a, json!({"action": "log", "text": "Met @Vex Arden at the dock today."})).unwrap();
    assert_eq!(out.reply["n"], json!(2));
    assert_eq!(out.reply["mentions"]["notified"], json!(["Vex Arden"]));
    assert_eq!(out.summary, "Log entry 2 written (public); 1 mentioned, 1 notified.");
    assert_eq!(mention::left(&s.w, a), left - 1);
    assert_eq!(s.w.identities[&a].log[1].mentions.captains, vec![v]);
    let ns = s.mention_notices(v);
    assert_eq!(ns.len(), 1);
    assert_eq!(
        ns[0].text,
        "Ada Quill mentioned you in a public log line (entry 2). identity action=story name=Ada Quill."
    );
    assert_eq!(ns[0].about, Some(mention::Place::Log { owner: a, n: 2 }));
    assert!(ns[0].quote.as_deref().unwrap().contains("@Vex Arden at the dock"));
    // Another captain reads the public line with its mentions; the private one stays hidden.
    let story = af_core::identity::read(
        &s.w,
        v,
        &serde_json::from_value(json!({"action": "story", "player": "Ada Quill"})).unwrap(),
    )
    .unwrap();
    let log = story["log"].as_array().unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["mentions"], json!(["Vex Arden"]));
    // The public story leaves a removed captain out of the names; captains read "#id".
    s.run(Actor::System, Command::Admin(AdminArgs::RemovePlayer { player: v })).unwrap();
    let public = af_core::identity::public_story(&s.w, a, None, 10);
    assert!(public["log"][0].get("mentions").is_none(), "{public}");
    let captain = af_core::identity::story(&s.w, a, false, None, 10);
    assert_eq!(captain["log"][0]["mentions"], json!([format!("#{v}")]));
    replay_matches(&s);
}

#[test]
fn a_proposal_mention_rides_on_the_electorate_notice_and_strikes_reach_every_quote() {
    let mut s = Sim::with(522, |r| r.constitution_electorate_ticks = 5);
    let a = s.join("Ada Quill");
    let b = s.join("Bram Hale");
    let z = s.join("Zed Orrin");
    for _ in 0..10 {
        s.tick();
    }
    // Ada and Bram act, so the electorate is the two of them; Zed is not in it.
    for pid in [a, b] {
        let to = s.w.sector(s.p(pid).sector).unwrap().warps[0];
        s.run(Actor::Player(pid), Command::Warp(WarpArgs { destination_sector: to }))
            .unwrap();
    }
    // Structured mentions on a vote or a view are refused, changing nothing.
    let before = s.w.state_hash();
    let e = constitution(&mut s, a, json!({"action": "view", "mentions": ["Bram Hale"]})).unwrap_err();
    assert_eq!(e.code, "mentions_not_here");
    let e = constitution(
        &mut s,
        a,
        json!({"action": "propose", "kind": "repeal", "article": 1, "mentions": ["Bram Hale"]}),
    )
    .unwrap_err();
    assert_eq!(e.code, "mentions_not_here");
    let e = constitution(
        &mut s,
        a,
        json!({"action": "propose", "kind": "add", "text": "x", "mentions": ["Nobody Here"]}),
    )
    .unwrap_err();
    assert_eq!(e.code, "unknown_mention");
    assert_eq!(s.w.state_hash(), before, "a refusal changes nothing");

    let left = mention::left(&s.w, a);
    let out = constitution(
        &mut s,
        a,
        json!({"action": "propose", "kind": "add", "text": "Ports stay open to @Bram Hale and @Zed Orrin."}),
    )
    .unwrap();
    let id = out.reply["proposal"]["id"].as_u64().unwrap();
    assert_eq!(out.reply["mentions"]["notified"], json!(["Bram Hale", "Zed Orrin"]));
    assert!(out.summary.ends_with("; 2 mentioned, 2 notified."), "{}", out.summary);
    assert_eq!(out.reply["proposal"]["mentions"], json!(["Bram Hale", "Zed Orrin"]));
    // Only Zed, who got no notice anyway, spent a unit: Bram's mark rode on the proposal notice.
    assert_eq!(mention::left(&s.w, a), left - 1);
    let p = s.w.constitution.proposals.iter().find(|p| p.id == id).unwrap();
    assert_eq!(p.mentions.captains, vec![b, z]);
    // Bram: one notice, the constitution one, marked; its preview is player text in `quote`.
    let bn: Vec<&Notice> = s
        .p(b)
        .inbox
        .iter()
        .filter(|n| n.kind == "constitution" || n.kind == "mention")
        .collect();
    assert_eq!(bn.len(), 1);
    assert_eq!(bn[0].kind, "constitution");
    assert!(bn[0].mention.is_some());
    assert!(bn[0].text.ends_with(" It mentions you."), "{}", bn[0].text);
    assert!(!bn[0].text.contains("Ports stay open"), "player words never enter the server text");
    assert!(bn[0].quote.as_deref().unwrap().contains("Ports stay open"));
    assert_eq!(bn[0].about, Some(mention::Place::Proposal { id }));
    // Zed: a mention notice.
    let zn = s.mention_notices(z);
    assert_eq!(zn.len(), 1);
    assert_eq!(
        zn[0].text,
        format!("Ada Quill mentioned you in constitution proposal #{id}. constitution action=view proposal={id}.")
    );
    assert!(s.p(z).inbox.iter().all(|n| n.kind != "constitution"));

    // A proposal without mentions still carries its preview as a quote, never in the text.
    let out = constitution(&mut s, b, json!({"action": "propose", "kind": "add", "text": "Plain words only."})).unwrap();
    let plain = out.reply["proposal"]["id"].as_u64().unwrap();
    assert!(out.reply.get("mentions").is_none());
    let an = s.p(a).inbox.iter().rev().find(|n| n.kind == "constitution").unwrap();
    assert_eq!(an.quote.as_deref(), Some("Plain words only."));
    assert!(an.mention.is_none() && !an.text.contains("Plain words"));
    assert!(
        an.text
            .contains(&format!("constitution action=view proposal={plain} shows its text."))
    );

    // Striking the proposal reaches every quote about it; the operator keeps the original.
    s.run(
        Actor::System,
        Command::Admin(AdminArgs::ConstitutionStrike {
            article: None,
            proposal: Some(id),
            reason: None,
        }),
    )
    .unwrap();
    for n in [
        s.p(b).inbox.iter().find(|n| n.kind == "constitution").unwrap(),
        &s.mention_notices(z)[0],
    ] {
        assert_eq!(n.quote.as_deref(), Some(mention::STRUCK));
        assert!(n.struck_quote.as_deref().unwrap().contains("Ports stay open"));
    }
    let view = af_core::constitution::lookup(&s.w, b, &serde_json::from_value(json!({"action": "view", "proposal": id})).unwrap()).unwrap();
    assert!(
        view["proposal"].get("mentions").is_none(),
        "a struck proposal's mentions are hidden"
    );
    for pid in [b, z] {
        let (v, _, _) = query::inbox(
            &s.w,
            pid,
            &InboxArgs {
                include_read: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!v.to_string().contains("Ports stay open"), "{v}");
    }
    let op = af_core::constitution::sysop_lookup(&s.w, Some(id), None, None).unwrap();
    assert_eq!(op["mentions"], json!(["Bram Hale", "Zed Orrin"]));
    assert_eq!(op["struck_original"], json!("Ports stay open to @Bram Hale and @Zed Orrin."));
    // The operator graph counts the proposal surface.
    let g = mention::operator_view(&s.w, SeasonSel::Current, None, None, 10, None);
    assert_eq!(g["totals"]["mentions"], json!(2));
    replay_matches(&s);
}

#[test]
fn a_muted_captain_s_log_line_tells_no_one() {
    let mut s = Sim::new(523);
    let a = s.join("Ada Quill");
    let v = s.join("Vex Arden");
    document(&mut s, a);
    s.run(Actor::System, Command::Admin(AdminArgs::Mute { player: a, ticks: 30 }))
        .unwrap();
    // A public line that would tell another captain is refused, inline or structured, and
    // changes nothing.
    let before = s.w.state_hash();
    for args in [
        json!({"action": "log", "text": "Met @Vex Arden today."}),
        json!({"action": "log", "text": "Met an old friend today.", "mentions": ["Vex Arden"]}),
    ] {
        let e = identity(&mut s, a, args).unwrap_err();
        assert_eq!(e.code, "muted", "{e}");
    }
    assert_eq!(s.w.state_hash(), before, "a refusal changes nothing");
    assert!(s.mention_notices(v).is_empty());
    // A line that tells no one is still kept: plain, private, or naming only the writer.
    for args in [
        json!({"action": "log", "text": "A quiet day."}),
        json!({"action": "log", "text": "I think of @Vex Arden.", "private": true}),
        json!({"action": "log", "text": "I am @Ada Quill."}),
    ] {
        identity(&mut s, a, args).unwrap();
    }
    assert!(s.mention_notices(v).is_empty());
    // Once the mute is lifted the line goes out.
    s.run(Actor::System, Command::Admin(AdminArgs::Mute { player: a, ticks: 0 }))
        .unwrap();
    identity(&mut s, a, json!({"action": "log", "text": "Met @Vex Arden today."})).unwrap();
    assert_eq!(s.mention_notices(v).len(), 1);
    replay_matches(&s);
}

#[test]
fn a_repeal_notice_quotes_nothing_and_names_no_text() {
    let mut s = Sim::with(524, |r| r.constitution_electorate_ticks = 5);
    let a = s.join("Ada Quill");
    let b = s.join("Bram Hale");
    let c = s.join("Cass Moor");
    for _ in 0..10 {
        s.tick();
    }
    for pid in [a, b, c] {
        let to = s.w.sector(s.p(pid).sector).unwrap().warps[0];
        s.run(Actor::Player(pid), Command::Warp(WarpArgs { destination_sector: to }))
            .unwrap();
    }
    let out = constitution(
        &mut s,
        a,
        json!({"action": "propose", "kind": "add", "text": "Keep the lanes open."}),
    )
    .unwrap();
    let id = out.reply["proposal"]["id"].as_u64().unwrap();
    constitution(&mut s, b, json!({"action": "vote", "proposal": id, "vote": true})).unwrap();
    let article = s.w.constitution.articles[0].id;
    let out = constitution(&mut s, b, json!({"action": "propose", "kind": "repeal", "article": article})).unwrap();
    let rid = out.reply["proposal"]["id"].as_u64().unwrap();
    let n = s
        .p(a)
        .inbox
        .iter()
        .rev()
        .find(|n| n.kind == "constitution" && n.text.contains(&format!("#{rid}")))
        .unwrap();
    assert!(n.quote.is_none() && n.about.is_none(), "{n:?}");
    assert!(!n.text.contains("shows its text"), "{}", n.text);
    assert!(n.text.ends_with("vote=true or false."), "{}", n.text);
}

// ---------- public views (S6) ----------

#[test]
fn public_profiles_list_where_captains_and_corporations_are_mentioned() {
    let mut s = Sim::new(531);
    let a = s.join("Ada Quill");
    let short = s.join("Vex");
    let v = s.join("Vex Arden");
    document(&mut s, a);
    let tide = s.found(v, "Iron Tide", &[short]);
    s.radio(
        a,
        json!({"message": "Good trade with @Vex Arden in sector 12.", "channel": "public"}),
    )
    .unwrap();
    let radio_id = s.w.radio.last().unwrap().id;
    s.radio(a, json!({"message": "CORP-WORDS for @Vex Arden", "channel": "corp"}))
        .unwrap_err();
    s.radio(
        a,
        json!({"message": "Ships wanted, @corp:Iron Tide is hiring.", "channel": "public"}),
    )
    .unwrap();
    let out = s
        .run(
            Actor::Player(a),
            Command::Chronicle(af_core::chronicle::Args {
                action: af_core::chronicle::Action::Write,
                kind: Some(af_core::chronicle::Kind::Event),
                title: Some("The quiet dock".into()),
                body: Some("Nobody saw @Vex Arden leave.".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    let page = out.reply["page"].as_u64().unwrap();
    identity(&mut s, a, json!({"action": "log", "text": "Thinking about @Vex Arden again."})).unwrap();
    identity(
        &mut s,
        a,
        json!({"action": "log", "text": "PRIVATE-THOUGHT on @Vex Arden.", "private": true}),
    )
    .unwrap();
    s.run(
        Actor::Player(a),
        Command::Chronicle(af_core::chronicle::Args {
            action: af_core::chronicle::Action::Talk,
            page: Some(page),
            text: Some("Still looking for @Vex Arden.".into()),
            ..Default::default()
        }),
    )
    .unwrap();

    // Newest first, one row per item, with no ids beyond page and proposal.
    let profile = af_core::public::player(&s.w, "Vex Arden").unwrap();
    let rows = profile["mentioned_in"].as_array().unwrap();
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        ["talk", "log", "page", "radio"],
        "a page and its talk are separate rows: {rows:?}"
    );
    assert_eq!(rows[0]["page"], json!(page));
    assert_eq!(rows[0]["text"], "Still looking for @Vex Arden.");
    assert_eq!(rows[1]["by"], "Ada Quill");
    assert_eq!(rows[2]["page"], json!(page));
    assert_eq!(rows[2]["title"], "The quiet dock");
    assert_eq!(rows[3]["text"], "Good trade with @Vex Arden.", "scrubbed of sector numbers");
    let text = profile["mentioned_in"].to_string();
    assert!(!text.contains("PRIVATE-THOUGHT") && !text.contains("CORP-WORDS"));
    for k in ["from_id", "player_id", "id", "message", "entry"] {
        assert!(rows.iter().all(|r| r.get(k).is_none()), "no `{k}`");
    }
    // "Vex" is not credited with mentions or headlines about "Vex Arden".
    assert!(
        af_core::public::player(&s.w, "Vex").unwrap()["mentioned_in"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let person = af_core::chronicle::public_person(&s.w, "Vex Arden", None).unwrap();
    assert_eq!(person["mentioned_in"], profile["mentioned_in"]);

    // The corporation's profile: members, government and its own mentions; no treasury.
    let c = af_core::public::corp(&s.w, "iron-tide").unwrap();
    assert_eq!(c["name"], "Iron Tide");
    assert_eq!(c["ceo"], "Vex Arden");
    assert_eq!(c["members"], json!(["Vex Arden", "Vex"]));
    assert_eq!(c["mentioned_in"].as_array().unwrap().len(), 1);
    assert_eq!(c["mentioned_in"][0]["kind"], "radio");
    assert!(c.get("treasury").is_none() && c.get("space").is_none());
    assert!(af_core::public::corp(&s.w, "No Such Corp").is_none());
    assert_eq!(s.p(v).corp, Some(tide));

    // Public radio rows carry the mentioned names.
    let st = af_core::public::state(&s.w);
    let r = st["radio"].as_array().unwrap().iter().find(|r| r["id"] == json!(radio_id)).unwrap();
    assert_eq!(r["mentions"], json!(["Vex Arden"]));

    // Moderation drops items at read time: a deleted message and a struck page leave the list.
    s.run(Actor::System, Command::Admin(AdminArgs::DeleteRadio { id: radio_id }))
        .unwrap();
    s.run(
        Actor::System,
        Command::Admin(AdminArgs::Chronicle(af_core::chronicle::AdminArgs {
            decision: af_core::chronicle::AdminAction::Strike,
            production: None,
            page: Some(page),
            note: None,
            links: None,
        })),
    )
    .unwrap();
    let rows = af_core::public::player(&s.w, "Vex Arden").unwrap()["mentioned_in"].clone();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["kind"], "log");
    replay_matches(&s);

    // Exact news attribution: the longest name wins at each word.
    let mut w = s.w.clone();
    w.news.push(NewsItem {
        tick: w.tick,
        headline: "Vex Arden won the daily race.".into(),
    });
    let news = |w: &World, n: &str| af_core::public::player(w, n).unwrap()["news"].to_string();
    assert!(news(&w, "Vex Arden").contains("won the daily race"));
    assert!(!news(&w, "Vex").contains("won the daily race"));
    // The cheap word-key test before the tokenizer keeps every match: a former call sign, and a
    // name written with other case and punctuation.
    let arden = w.players.values().find(|p| p.name == "Vex Arden").unwrap().id;
    w.identities.entry(arden).or_default().former_names.push("Quiet Comet".into());
    for headline in ["QUIET-COMET docked at the rim.", "A toast to vex_arden tonight."] {
        w.news.push(NewsItem {
            tick: w.tick,
            headline: headline.into(),
        });
    }
    let arden_news = news(&w, "Vex Arden");
    assert!(arden_news.contains("QUIET-COMET docked"), "{arden_news}");
    assert!(arden_news.contains("vex_arden tonight"), "{arden_news}");
    assert!(!news(&w, "Vex").contains("QUIET-COMET"));
}

/// A proposal's mentions reach the public "Mentioned in"; striking the article it wrote strikes
/// the proposal's text without setting `struck`, and its mentions leave every public and captain
/// view all the same.
#[test]
fn an_article_strike_takes_its_proposal_out_of_mentioned_in() {
    let mut s = Sim::with(532, |r| r.constitution_electorate_ticks = 5);
    let a = s.join("Ada Quill");
    let b = s.join("Bram Hale");
    s.join("Zed Orrin");
    for _ in 0..10 {
        s.tick();
    }
    for pid in [a, b] {
        let to = s.w.sector(s.p(pid).sector).unwrap().warps[0];
        s.run(Actor::Player(pid), Command::Warp(WarpArgs { destination_sector: to }))
            .unwrap();
    }
    let out = constitution(
        &mut s,
        a,
        json!({"action": "propose", "kind": "add", "text": "Safe passage for @Zed Orrin."}),
    )
    .unwrap();
    let id = out.reply["proposal"]["id"].as_u64().unwrap();
    constitution(&mut s, b, json!({"action": "vote", "proposal": id, "vote": true})).unwrap();
    let article = s.w.constitution.articles.last().unwrap().id;

    let rows = af_core::public::player(&s.w, "Zed Orrin").unwrap()["mentioned_in"].clone();
    assert_eq!(rows.as_array().unwrap().len(), 1, "{rows}");
    assert_eq!(rows[0]["kind"], "proposal");
    assert_eq!(rows[0]["proposal"], json!(id));
    assert_eq!(rows[0]["by"], "Ada Quill");
    assert_eq!(rows[0]["text"], "Safe passage for @Zed Orrin.");
    let public = af_core::constitution::public_view(&s.w);
    assert!(public.to_string().contains("\"mentions\":[\"Zed Orrin\"]"), "{public}");

    s.run(
        Actor::System,
        Command::Admin(AdminArgs::ConstitutionStrike {
            article: Some(article),
            proposal: None,
            reason: None,
        }),
    )
    .unwrap();
    let p = s.w.constitution.proposals.iter().find(|p| p.id == id).unwrap();
    assert!(
        p.struck.is_none() && p.struck_original.is_some(),
        "the article strike struck the text only"
    );
    let rows = af_core::public::player(&s.w, "Zed Orrin").unwrap()["mentioned_in"].clone();
    assert!(rows.as_array().unwrap().is_empty(), "{rows}");
    let person = af_core::chronicle::public_person(&s.w, "Zed Orrin", None).unwrap();
    assert!(person["mentioned_in"].as_array().unwrap().is_empty());
    let public = af_core::constitution::public_view(&s.w);
    assert!(!public.to_string().contains("Zed Orrin"), "{public}");
    let view = af_core::constitution::lookup(&s.w, b, &serde_json::from_value(json!({"action": "view", "proposal": id})).unwrap()).unwrap();
    assert!(view["proposal"].get("mentions").is_none(), "{view}");
    // The operator still sees who it named.
    let op = af_core::constitution::sysop_lookup(&s.w, Some(id), None, None).unwrap();
    assert_eq!(op["mentions"], json!(["Zed Orrin"]));
    replay_matches(&s);
}
