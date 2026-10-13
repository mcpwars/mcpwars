//! The language lifecycle (engine 1.58): permanent ids, known versions, folded version notices,
//! stale and retired languages, the operator's retire and restore, and the preview.
use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::language::{self, LangRef, Status};
use af_core::model::*;
use af_core::{World, query};
use serde_json::{Value, json};

struct Sim {
    w: World,
    journal: Vec<Entry>,
    cfg: GenesisConfig,
}

impl Sim {
    fn new(seed: u64, sectors: u32) -> Self {
        Self::with(seed, sectors, |_| {})
    }
    /// A Sim whose rules are set before anything is journaled, so replay sees them too.
    fn with(seed: u64, sectors: u32, f: impl Fn(&mut af_core::rules::Rules)) -> Self {
        let mut cfg = GenesisConfig::new(seed, sectors);
        f(&mut cfg.rules);
        Sim {
            w: generate(&cfg),
            journal: vec![],
            cfg,
        }
    }
    fn run(&mut self, actor: Actor, cmd: Command) -> Result<af_core::Outcome, af_core::Refusal> {
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
    fn ticks_to(&mut self, t: u64) {
        while self.w.tick < t {
            self.tick();
        }
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
}

fn replay_matches(s: &Sim) {
    let mut w = generate(&s.cfg);
    for e in &s.journal {
        let _ = apply(&mut w, e);
    }
    assert_eq!(w.state_hash(), s.w.state_hash(), "replay reproduces the world");
}

/// Ten ticks a game day, so lifecycle tests run in a few dozen ticks.
fn short_days(r: &mut af_core::rules::Rules) {
    r.tick_ms = 8_640_000;
}

fn lang(s: &mut Sim, pid: PlayerId, args: Value) -> Result<af_core::Outcome, af_core::Refusal> {
    s.run(Actor::Player(pid), Command::Language(serde_json::from_value(args).unwrap()))
}

fn define(s: &mut Sim, pid: PlayerId, name: &str, share: &str) -> Value {
    lang(
        s,
        pid,
        json!({"action": "define", "name": name, "explanation": format!("How {name} works."), "share": share}),
    )
    .unwrap()
    .reply
}

fn lang_read(s: &Sim, pid: PlayerId, args: Value) -> Result<Value, af_core::Refusal> {
    language::read(&s.w, pid, &serde_json::from_value(args).unwrap())
}

fn admin(s: &mut Sim, args: Value) -> Result<af_core::Outcome, af_core::Refusal> {
    s.run(
        Actor::System,
        Command::Admin(AdminArgs::Language(serde_json::from_value(args).unwrap())),
    )
}

fn mark_read(s: &mut Sim, pid: PlayerId, ids: Vec<u64>) {
    s.run(
        Actor::Player(pid),
        Command::MarkRead(MarkReadArgs {
            notice_up_to: 0,
            radio_up_to: 0,
            announcements_up_to: None,
            notice_ids: Some(ids),
            radio_ids: None,
        }),
    )
    .unwrap();
}

/// Make the store look as a 1.57 universe left it: no ids, nothing recorded as known.
fn as_before_1_58(w: &mut World) {
    for l in w.languages.values_mut() {
        l.id = 0;
        l.known.clear();
    }
    w.next_language = 0;
}

fn language_notices(s: &Sim, pid: PlayerId) -> Vec<Notice> {
    s.p(pid).inbox.iter().filter(|n| n.kind == "language").cloned().collect()
}

#[test]
fn a_tick_assigns_ids_in_creation_order_and_backfills_known() {
    let mut s = Sim::new(301, 60);
    let (a, b) = (s.join("Ada"), s.join("Bex"));
    define(&mut s, a, "Zeta", "captains");
    define(&mut s, b, "Alpha", "self");
    define(&mut s, a, "Zeta", "captains");
    lang(&mut s, b, json!({"action": "adopt", "name": "Zeta"})).unwrap();
    // A new universe gives ids at insertion.
    assert_eq!((s.w.languages["zeta"].id, s.w.languages["alpha"].id, s.w.next_language), (1, 2, 3));
    replay_matches(&s);

    as_before_1_58(&mut s.w);
    let predicted = (language::id_of(&s.w, "zeta"), language::id_of(&s.w, "alpha"));
    assert_eq!(predicted, (1, 2), "creation order, not name order");
    // What the backfill will record is what `known` reads already.
    assert_eq!(language::known(&s.w.languages["zeta"], b), 2);
    s.tick();
    assert_eq!((s.w.languages["zeta"].id, s.w.languages["alpha"].id), predicted);
    assert_eq!(s.w.next_language, 3);
    assert_eq!(s.w.languages["zeta"].known, std::collections::BTreeMap::from([(a, 2), (b, 2)]));
    assert_eq!(s.w.languages["alpha"].known, std::collections::BTreeMap::from([(b, 1)]));
    // A later definition keeps the old ids and takes the next one.
    assert_eq!(define(&mut s, a, "Mid", "self")["id"], 3);
    assert_eq!(s.w.next_language, 4);
    assert_eq!((s.w.languages["zeta"].id, s.w.languages["alpha"].id), (1, 2));
    // The list and the operator see the same ids.
    let ids: Vec<u64> = lang_read(&s, a, json!({"action": "list"})).unwrap()["languages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [3, 1], "Mid and Zeta, in name order");
    let op = language::operator_view(&s.w);
    assert_eq!(op["languages"][0]["id"], 2);
    assert_eq!(op["languages"][2]["known"][b.to_string()], 2);
}

#[test]
fn a_define_before_any_tick_records_what_adopters_knew() {
    let mut s = Sim::new(302, 60);
    let (a, b) = (s.join("Ada"), s.join("Bex"));
    define(&mut s, a, "Dockcant", "captains");
    define(&mut s, a, "Dockcant", "captains");
    lang(&mut s, b, json!({"action": "adopt", "name": "Dockcant"})).unwrap();
    as_before_1_58(&mut s.w);
    let before = language_notices(&s, b).len();
    define(&mut s, a, "Dockcant", "captains");
    let l = &s.w.languages["dockcant"];
    assert_eq!((l.id, l.known[&b], l.known[&a]), (1, 2, 3));
    let notes = language_notices(&s, b);
    assert_eq!(notes.len(), before + 1);
    let n = notes.last().unwrap();
    assert_eq!(
        n.fold,
        Some(Fold::Language {
            language: 1,
            from_version: 3,
            to_version: 3,
            by: vec![a]
        })
    );
    assert_eq!(n.quote.as_deref(), Some("Dockcant"), "the name is player text");
    assert!(n.text.contains("#1") && !n.text.contains("Dockcant"), "{}", n.text);
    assert!(language_notices(&s, a).is_empty(), "the author is not told of its own version");
}

#[test]
fn version_notices_fold_while_unread_and_supersede() {
    let mut s = Sim::new(303, 60);
    let (a, b, c) = (s.join("Ada"), s.join("Bex"), s.join("Cyd"));
    define(&mut s, a, "Quill", "captains");
    lang(&mut s, b, json!({"action": "adopt", "id": 1})).unwrap();
    lang(&mut s, c, json!({"action": "adopt", "id": 1})).unwrap();
    define(&mut s, a, "Quill", "captains");
    let first = language_notices(&s, b)[0].clone();
    assert_eq!(
        first.text,
        "Language #1 has a new version 2 by Ada. language action=read id=1 version=2; language action=adopt id=1 marks it known."
    );
    let mark = query::notice_watermark(&s.w, b).0;
    define(&mut s, a, "Quill", "captains");
    define(&mut s, c, "Quill", "captains");
    let notes = language_notices(&s, b);
    assert_eq!(notes.len(), 3);
    assert!(notes[0].read && notes[1].read, "superseded notices are marked read");
    assert_eq!(notes[0].superseded_by, Some(notes[1].id));
    assert_eq!(notes[1].superseded_by, Some(notes[2].id));
    let last = &notes[2];
    assert!(!last.read && last.superseded_by.is_none());
    assert_eq!(
        last.fold,
        Some(Fold::Language {
            language: 1,
            from_version: 2,
            to_version: 4,
            by: vec![a, c]
        })
    );
    assert!(last.text.contains("new versions 2 to 4 by Ada and Cyd"), "{}", last.text);
    assert!(query::notice_watermark(&s.w, b).0 > mark, "waiters wake");
    // The creator hears of the adopter's version, from the first one it does not know.
    let to_a = language_notices(&s, a);
    assert_eq!(to_a.len(), 1);
    assert!(matches!(
        to_a[0].fold,
        Some(Fold::Language {
            from_version: 4,
            to_version: 4,
            ..
        })
    ));
    // A late receipt for an old id never marks the newer notice read.
    mark_read(&mut s, b, vec![first.id]);
    assert!(!language_notices(&s, b)[2].read);
    // Once read, the next version brings a fresh notice.
    mark_read(&mut s, b, vec![last.id]);
    define(&mut s, a, "Quill", "captains");
    let notes = language_notices(&s, b);
    assert_eq!(notes.len(), 4);
    assert!(notes[2].superseded_by.is_none());
    assert!(!notes[3].read);
    assert!(matches!(notes[3].fold, Some(Fold::Language { to_version: 5, .. })));
    // Adopting again marks the latest version known, so the next notice starts after it.
    let r = lang(&mut s, b, json!({"action": "adopt", "id": 1})).unwrap();
    assert_eq!(r.reply, json!({"name": "Quill", "adopted": true, "id": 1, "version": 5}));
    assert!(r.summary.contains("marked version 5 known"), "{}", r.summary);
    mark_read(&mut s, b, vec![notes[3].id]);
    define(&mut s, a, "Quill", "captains");
    assert!(matches!(
        language_notices(&s, b).last().unwrap().fold,
        Some(Fold::Language {
            from_version: 6,
            to_version: 6,
            ..
        })
    ));
    replay_matches(&s);
}

#[test]
fn read_and_adopt_take_an_id_and_define_and_list_refuse_one() {
    let mut s = Sim::new(304, 60);
    let (a, b, c) = (s.join("Ada"), s.join("Bex"), s.join("Cyd"));
    define(&mut s, a, "Dockcant", "captains");
    define(&mut s, a, "Secret", "self");
    let v = lang_read(&s, b, json!({"action": "read", "id": 1})).unwrap();
    assert_eq!(
        (v["language"]["id"].clone(), v["language"]["name"].clone()),
        (json!(1), json!("Dockcant"))
    );
    assert_eq!(v["language"]["status"], "active");
    assert_eq!(v["language"]["known_version"], 0);
    assert_eq!(v["language"]["tagged"], 0);
    // An id the captain cannot read is as unknown as one that does not exist.
    for id in [2, 99] {
        assert_eq!(
            lang_read(&s, c, json!({"action": "read", "id": id})).unwrap_err().code,
            "unknown_language"
        );
    }
    let r = lang(&mut s, b, json!({"action": "adopt", "id": 1})).unwrap();
    assert_eq!(r.reply, json!({"name": "Dockcant", "adopted": true, "id": 1, "version": 1}));
    assert_eq!(s.w.languages["dockcant"].known[&b], 1);
    // The creator may adopt its own language: it only marks it known.
    lang(&mut s, a, json!({"action": "adopt", "name": "Dockcant"})).unwrap();
    assert!(!s.w.languages["dockcant"].adopters.contains(&a));
    let before = s.w.state_hash();
    let e = lang(
        &mut s,
        a,
        json!({"action": "define", "id": 1, "name": "Dockcant", "explanation": "x"}),
    )
    .unwrap_err();
    assert_eq!(
        (e.code.as_str(), e.message.as_str()),
        ("invalid_args", "define and list take a name, not an id.")
    );
    assert_eq!(s.w.state_hash(), before);
    assert_eq!(
        lang_read(&s, a, json!({"action": "list", "id": 1})).unwrap_err().code,
        "invalid_args"
    );
    // Define replies with the id.
    assert_eq!(
        define(&mut s, a, "Dockcant", "captains"),
        json!({"name": "Dockcant", "version": 2, "share": "captains", "id": 1})
    );
    replay_matches(&s);
}

#[test]
fn stale_status_and_daily_retirement_follow_the_rules() {
    let mut s = Sim::with(305, 60, |r| {
        short_days(r);
        r.language_stale_days = 2;
        r.language_retire_days = 3;
        r.language_adopted_grace_days = 2;
    });
    let (a, b, c) = (s.join("Ada"), s.join("Bex"), s.join("Cyd"));
    assert_eq!(s.w.tick, 0);
    define(&mut s, a, "Old", "self");
    define(&mut s, b, "Kept", "captains");
    lang(&mut s, a, json!({"action": "adopt", "name": "Kept"})).unwrap();
    assert_eq!(query::game_info(&s.w)["languages"]["automatic_retirement"], true);
    let status = |s: &Sim, k: &str| language::status(&s.w, &s.w.languages[k]);
    s.ticks_to(19);
    assert_eq!(status(&s, "old"), Status::Active);
    s.ticks_to(20);
    assert_eq!(status(&s, "old"), Status::Stale);
    assert_eq!(status(&s, "kept"), Status::Active, "an adopted language has its grace");
    assert_eq!(
        lang_read(&s, a, json!({"action": "list"})).unwrap()["languages"][1]["status"],
        "stale"
    );
    // With the stale rule at 0 nothing is ever stale.
    let mut off = s.w.clone();
    off.rules.language_stale_days = 0;
    assert_eq!(language::status(&off, &off.languages["old"]), Status::Active);
    s.ticks_to(29);
    assert!(s.w.languages["old"].retired.is_none());
    s.ticks_to(30);
    let r = s.w.languages["old"].retired.clone().unwrap();
    assert_eq!((r.by_operator, r.idle_days, r.tick), (false, 3, 30));
    assert!(s.w.languages["kept"].retired.is_none());
    let notes = language_notices(&s, a);
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[0].text,
        "Language #1 was retired after a long time without tagged messages: it stays readable but can no longer tag radio messages. The creator restores it by defining a new version."
    );
    assert!(language_notices(&s, b).is_empty());
    s.ticks_to(50);
    assert!(s.w.languages["kept"].retired.is_some(), "retire days plus the grace");
    for pid in [a, b] {
        assert!(
            language_notices(&s, pid)
                .last()
                .unwrap()
                .text
                .starts_with("Language #2 was retired")
        );
    }
    // A retired language leaves the list, stays readable, and refuses adoption and tags.
    assert_eq!(lang_read(&s, b, json!({"action": "list"})).unwrap()["languages"], json!([]));
    assert_eq!(
        lang_read(&s, c, json!({"action": "read", "id": 2})).unwrap()["language"]["status"],
        "retired"
    );
    let before = s.w.state_hash();
    assert_eq!(
        lang(&mut s, c, json!({"action": "adopt", "id": 2})).unwrap_err().code,
        "language_retired"
    );
    assert_eq!(s.w.state_hash(), before);
    assert_eq!(
        language::check_tag(&s.w, b, &LangRef::Id(2), None).unwrap_err().code,
        "language_retired"
    );
    assert_eq!(
        query::briefing(&s.w, a).unwrap()["comms"]["languages"]["yours"],
        json!(["Kept (retired)", "Old (retired)"])
    );
    // Its creator restores an idle retirement by defining a new version; nobody else can.
    assert_eq!(
        lang(&mut s, a, json!({"action": "define", "name": "Kept", "explanation": "x"}))
            .unwrap_err()
            .code,
        "language_retired"
    );
    let r = define(&mut s, b, "Kept", "captains");
    assert_eq!(r["restored"], true);
    let l = &s.w.languages["kept"];
    assert!(l.retired.is_none());
    assert_eq!(l.revived_tick, Some(s.w.tick));
    assert_eq!(language::idle_days(&s.w, l), 0, "the idle clock restarts");
    replay_matches(&s);
}

#[test]
fn nothing_retires_while_the_rule_is_zero() {
    let mut s = Sim::with(306, 60, short_days);
    let a = s.join("Ada");
    define(&mut s, a, "Old", "self");
    s.ticks_to(60);
    assert!(s.w.languages["old"].retired.is_none());
    assert_eq!(language::status(&s.w, &s.w.languages["old"]), Status::Active);
    assert!(language_notices(&s, a).is_empty());
    assert_eq!(query::game_info(&s.w)["languages"]["automatic_retirement"], false);
    replay_matches(&s);
}

#[test]
fn the_operator_retires_and_restores_languages() {
    let mut s = Sim::new(307, 60);
    let (a, b, c) = (s.join("Ada"), s.join("Bex"), s.join("Cyd"));
    define(&mut s, a, "Alpha", "captains");
    define(&mut s, a, "Beta", "self");
    lang(&mut s, b, json!({"action": "adopt", "name": "Alpha"})).unwrap();
    // Validated whole: nothing changes on a refusal.
    let before = s.w.state_hash();
    let e = admin(&mut s, json!({"decision": "retire", "ids": [1, 99, 98]})).unwrap_err();
    assert_eq!(e.code, "unknown_language");
    assert!(e.message.contains("#98 and #99"), "{}", e.message);
    assert_eq!(
        admin(&mut s, json!({"decision": "retire", "ids": []})).unwrap_err().code,
        "invalid_args"
    );
    assert_eq!(s.w.state_hash(), before);

    let out = admin(&mut s, json!({"decision": "retire", "ids": [2, 1], "reason": "Spam."})).unwrap();
    assert_eq!(out.reply, json!({"retired": [1, 2], "skipped": []}));
    let r = s.w.languages["alpha"].retired.clone().unwrap();
    assert_eq!((r.by_operator, r.reason.as_deref()), (true, Some("Spam.")));
    assert_eq!(
        language_notices(&s, a).last().unwrap().text,
        "The operator retired languages #1 and #2: they stay readable but can no longer tag radio messages."
    );
    assert_eq!(
        language_notices(&s, b).last().unwrap().text,
        "The operator retired language #1: it stays readable but can no longer tag radio messages."
    );
    assert!(language_notices(&s, c).is_empty());
    assert_eq!(
        admin(&mut s, json!({"decision": "retire", "ids": [1]})).unwrap().reply,
        json!({"retired": [], "skipped": [1]})
    );
    // Hidden from the list, still readable, refused for adoption and tags.
    assert_eq!(lang_read(&s, c, json!({"action": "list"})).unwrap()["languages"], json!([]));
    assert!(lang_read(&s, c, json!({"action": "read", "name": "Alpha"})).is_ok());
    assert_eq!(
        lang(&mut s, c, json!({"action": "adopt", "name": "Alpha"})).unwrap_err().code,
        "language_retired"
    );
    assert_eq!(
        language::check_tag(&s.w, a, &LangRef::Name("Beta".into()), None).unwrap_err().code,
        "language_retired"
    );
    // The creator cannot undo an operator retirement.
    assert_eq!(
        lang(&mut s, a, json!({"action": "define", "name": "Alpha", "explanation": "x"}))
            .unwrap_err()
            .code,
        "language_retired"
    );
    let out = admin(&mut s, json!({"decision": "unretire", "ids": [1]})).unwrap();
    assert_eq!(out.reply, json!({"unretired": [1], "skipped": []}));
    for pid in [a, b] {
        assert_eq!(
            language_notices(&s, pid).last().unwrap().text,
            "The operator restored language #1: it may tag radio messages again."
        );
    }
    let l = &s.w.languages["alpha"];
    assert!(l.retired.is_none());
    assert_eq!(l.revived_tick, Some(s.w.tick));
    lang(&mut s, c, json!({"action": "adopt", "id": 1})).unwrap();
    assert_eq!(
        language::check_tag(&s.w, c, &LangRef::Id(1), None).unwrap().1,
        language::LangTag { id: 1, version: 1 }
    );
    // Restoring a language that is not retired is skipped.
    assert_eq!(
        admin(&mut s, json!({"decision": "unretire", "ids": [1]})).unwrap().reply,
        json!({"unretired": [], "skipped": [1]})
    );
    replay_matches(&s);
}

#[test]
fn the_operator_preview_reads_rules_overrides_or_suggestions() {
    let mut s = Sim::with(308, 60, short_days);
    let (a, b) = (s.join("Ada"), s.join("Bex"));
    define(&mut s, a, "Old", "self");
    define(&mut s, b, "Kept", "captains");
    lang(&mut s, a, json!({"action": "adopt", "name": "Kept"})).unwrap();
    s.ticks_to(30);
    let p = language::retirement_preview(&s.w, None, None, None);
    assert_eq!(
        (
            p["source"].clone(),
            p["stale_days"].clone(),
            p["retire_days"].clone(),
            p["grace_days"].clone()
        ),
        (
            json!("suggested"),
            json!(language::PREVIEW_STALE_DAYS),
            json!(language::PREVIEW_RETIRE_DAYS),
            json!(30)
        )
    );
    assert_eq!(p["counts"], json!({"active": 2, "stale": 0, "retire": 0}));
    let p = language::retirement_preview(&s.w, Some(1), Some(3), Some(1));
    assert_eq!(p["source"], "override");
    assert_eq!(p["counts"], json!({"active": 0, "stale": 1, "retire": 1}));
    let c = p["candidates"].as_array().unwrap();
    assert_eq!((c[0]["id"].clone(), c[0]["would"].clone()), (json!(1), json!("retire")));
    assert_eq!((c[1]["id"].clone(), c[1]["would"].clone()), (json!(2), json!("stale")));
    assert_eq!(c[1]["adopters"], 1);
    assert_eq!(c[0]["idle_days"], 3);
    assert_eq!(c[0]["creator_name"], "Ada");
    // The preview changes nothing.
    assert!(s.w.languages.values().all(|l| l.retired.is_none()));
    // With rules set, the preview reads them.
    let mut ruled = s.w.clone();
    ruled.rules.language_retire_days = 5;
    let p = language::retirement_preview(&ruled, None, None, None);
    assert_eq!((p["source"].clone(), p["retire_days"].clone()), (json!("rules"), json!(5)));
    // The operator's rows carry the lifecycle.
    let op = language::operator_view(&s.w);
    let kept = &op["languages"][0];
    assert_eq!(
        (kept["id"].clone(), kept["status"].clone(), kept["idle_days"].clone()),
        (json!(2), json!("active"), json!(3))
    );
    assert_eq!(kept["known_count"], 2);
    assert_eq!(kept["tagged"], 0);
    assert_eq!(kept["retired"], Value::Null);
    replay_matches(&s);
}

fn radio(s: &mut Sim, pid: PlayerId, args: Value) -> Result<af_core::Outcome, af_core::Refusal> {
    s.run(Actor::Player(pid), Command::Radio(serde_json::from_value(args).unwrap()))
}

#[test]
fn radio_tags_check_the_language_and_count_its_uses() {
    let mut s = Sim::new(309, 60);
    let (a, b) = (s.join("Ada"), s.join("Bex"));
    define(&mut s, a, "Dockcant", "self");
    define(&mut s, a, "Tradecode", "captains");
    s.tick();
    let before = s.w.state_hash();
    let refuse = |s: &mut Sim, pid, args: Value| radio(s, pid, args).unwrap_err().code;
    // A language the sender cannot read is refused exactly like one that does not exist.
    assert_eq!(
        refuse(&mut s, b, json!({"message": "x", "channel": "public", "language": "Dockcant"})),
        "unknown_language"
    );
    assert_eq!(
        refuse(&mut s, b, json!({"message": "x", "channel": "public", "language": 1})),
        "unknown_language"
    );
    assert_eq!(
        refuse(&mut s, a, json!({"message": "x", "channel": "public", "language": "Nope"})),
        "unknown_language"
    );
    assert_eq!(
        refuse(
            &mut s,
            a,
            json!({"message": "x", "channel": "public", "language": "Dockcant", "language_version": 2})
        ),
        "unknown_version"
    );
    assert_eq!(
        refuse(&mut s, a, json!({"message": "x", "channel": "public", "language_version": 1})),
        "invalid_args"
    );
    assert_eq!(s.w.state_hash(), before, "refusals change nothing");

    let out = radio(&mut s, a, json!({"message": "o12 g8", "channel": "public", "language": "Dockcant"})).unwrap();
    assert_eq!(out.reply["language"], json!({"id": 1, "name": "Dockcant", "version": 1}));
    assert_eq!(s.w.radio.last().unwrap().language, Some(language::LangTag { id: 1, version: 1 }));
    let l = &s.w.languages["dockcant"];
    assert_eq!((l.tagged, l.last_tagged_tick), (1, Some(s.w.tick)));
    // By id, on any channel, and by a reader who is not the creator.
    radio(
        &mut s,
        b,
        json!({"message": "t1", "channel": "private", "recipient_id": "Ada", "language": 2}),
    )
    .unwrap();
    assert_eq!(s.w.languages["tradecode"].tagged, 1);

    admin(&mut s, json!({"decision": "retire", "ids": [1]})).unwrap();
    let before = s.w.state_hash();
    assert_eq!(
        refuse(&mut s, a, json!({"message": "x", "channel": "public", "language": "Dockcant"})),
        "language_retired"
    );
    assert_eq!(s.w.state_hash(), before);
    replay_matches(&s);
}

#[test]
fn a_tag_assigns_ids_before_it_is_stored() {
    let mut s = Sim::new(310, 60);
    let a = s.join("Ada");
    define(&mut s, a, "Dockcant", "self");
    as_before_1_58(&mut s.w);
    let expected = language::id_of(&s.w, "dockcant");
    radio(&mut s, a, json!({"message": "o12", "channel": "public", "language": "Dockcant"})).unwrap();
    assert_eq!(s.w.languages["dockcant"].id, expected);
    assert_eq!(s.w.radio.last().unwrap().language.unwrap().id, expected);
}

#[test]
fn non_readers_see_only_the_id_and_unfamiliar_versions_clear() {
    let mut s = Sim::new(311, 60);
    let (a, b) = (s.join("Ada"), s.join("Bex"));
    define(&mut s, a, "Dockcant", "self");
    define(&mut s, a, "Tradecode", "captains");
    lang(&mut s, b, json!({"action": "adopt", "id": 2})).unwrap();
    define(&mut s, a, "Tradecode", "captains");
    s.tick();
    radio(&mut s, a, json!({"message": "o12", "channel": "public", "language": "Dockcant"})).unwrap();
    let hidden = s.w.radio.last().unwrap().id;
    radio(&mut s, a, json!({"message": "t2", "channel": "public", "language": "Tradecode"})).unwrap();
    let newer = s.w.radio.last().unwrap().id;

    // The radio row: id and version only for a captain who cannot read the language.
    let (view, _, _) = query::inbox(&s.w, b, &Default::default()).unwrap();
    let row = |id: u64| {
        view["radio"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == json!(id))
            .unwrap()
            .clone()
    };
    assert_eq!(row(hidden)["language"], json!({"id": 1, "version": 1, "you_can_read": false}));
    assert_eq!(
        row(newer)["language"],
        json!({"id": 2, "name": {"untrusted_player_text": "Tradecode"}, "version": 2, "you_can_read": true})
    );
    assert!(!view.to_string().contains("Dockcant"), "a non-reader never sees the name");

    // The unfamiliar block, newest first, with untrusted names.
    let comms = query::briefing(&s.w, b).unwrap()["comms"].clone();
    let rows = comms["languages"]["unfamiliar"]["languages"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2, "{comms}");
    assert_eq!(rows[0]["id"], 2);
    assert_eq!(rows[0]["status"], "unfamiliar");
    assert_eq!(rows[0]["name"], json!({"untrusted_player_text": "Tradecode"}));
    assert_eq!((rows[0]["version"].clone(), rows[0]["messages"].clone()), (json!(2), json!(1)));
    assert_eq!(rows[0]["latest_from"], "Ada");
    assert_eq!(rows[1]["status"], "not_shared");
    assert!(rows[1].get("name").is_none());
    assert!(!comms.to_string().contains("Dockcant"));
    // The writer's own messages never count.
    assert!(query::briefing(&s.w, a).unwrap()["comms"]["languages"].get("unfamiliar").is_none());

    // A retired language is skipped.
    admin(&mut s, json!({"decision": "retire", "ids": [1]})).unwrap();
    let rows = query::briefing(&s.w, b).unwrap()["comms"]["languages"]["unfamiliar"]["languages"].clone();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    // Adopting again (by id) marks the latest version known.
    let out = lang(&mut s, b, json!({"action": "adopt", "id": 2})).unwrap();
    assert_eq!(out.reply["version"], 2);
    assert!(query::briefing(&s.w, b).unwrap()["comms"]["languages"].get("unfamiliar").is_none());

    // Marking the messages read clears it too.
    admin(&mut s, json!({"decision": "unretire", "ids": [1]})).unwrap();
    assert!(query::briefing(&s.w, b).unwrap()["comms"]["languages"].get("unfamiliar").is_some());
    s.run(
        Actor::Player(b),
        Command::MarkRead(MarkReadArgs {
            notice_up_to: 0,
            radio_up_to: 0,
            announcements_up_to: None,
            notice_ids: None,
            radio_ids: Some(vec![hidden, newer]),
        }),
    )
    .unwrap();
    assert!(query::briefing(&s.w, b).unwrap()["comms"]["languages"].get("unfamiliar").is_none());
    replay_matches(&s);
}
