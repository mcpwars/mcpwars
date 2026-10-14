//! Adopting a 1.57 universe into engine 1.58: a world serialized before mentions, language ids
//! and kept strike originals existed loads with every new field at its default, writes its hot
//! records (radio, notices, pages, talk, log lines, proposals, articles) back byte for byte,
//! hashes like a native world with the same contents, and the first tick then gives its
//! languages their ids and its Chronicle pages their stored contributor lists.

use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::model::*;
use af_core::rules::Rules;
use af_core::{Outcome, Refusal, World};
use af_core::{chronicle, language};
use serde_json::{Map, Value, json};

struct Sim {
    w: World,
    journal: Vec<Entry>,
}

impl Sim {
    fn run(&mut self, actor: Actor, cmd: Command) -> Result<Outcome, Refusal> {
        // Continue the world's own sequence and clock, so an adopted world runs on from its head.
        let e = Entry {
            seq: self.w.seq + 1,
            at_ms: self.w.now_ms + 1000,
            actor,
            cmd,
        };
        // Through JSON, as the store journals it.
        let e: Entry = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        self.journal.push(e.clone());
        apply(&mut self.w, &e)
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
    fn player(&mut self, pid: PlayerId, cmd: Command) -> Value {
        self.run(Actor::Player(pid), cmd).unwrap().reply
    }
}

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

fn cmd<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
}

/// A world that used every 1.58 field: tagged radio with mentions, mention and folded notices,
/// pages and talk with mentions, a public log line, a proposal struck after a page cited it.
fn rich_world() -> Sim {
    let mut s = Sim {
        w: generate(&GenesisConfig::new(156, 60)),
        journal: vec![],
    };
    let a = s.join("Ada Quill");
    let b = s.join("Bram Hale");
    let z = s.join("Zed Orrin");
    document(&mut s, a);
    document(&mut s, b);
    s.run(Actor::System, Command::Tick).unwrap();
    let define = |name: &str, share: &str| {
        Command::Language(cmd(json!({
            "action": "define", "name": name, "explanation": format!("How {name} works."), "share": share
        })))
    };
    s.player(a, define("Trade Cant", "captains"));
    s.player(z, define("Zed Code", "self"));
    s.player(b, Command::Language(cmd(json!({"action": "adopt", "name": "Trade Cant"}))));
    s.player(
        a,
        Command::Radio(cmd(json!({
            "channel": "public", "message": "Meet me at the dock, @Bram Hale.", "language": "Trade Cant"
        }))),
    );
    let radio = s.w.radio.last().unwrap().id;
    let proposal = s.player(
        a,
        Command::Constitution(cmd(json!({
            "action": "propose", "kind": "add", "text": "Ports stay open to @Zed Orrin."
        }))),
    )["proposal"]["id"]
        .as_u64()
        .unwrap();
    let page = s.player(
        a,
        Command::Chronicle(cmd(json!({
            "action": "write", "kind": "event", "title": "The dock",
            "body": "Nobody saw @Zed Orrin leave.",
            "citations": [{"record": "radio", "id": radio}]
        }))),
    )["page"]
        .as_u64()
        .unwrap();
    s.player(
        b,
        Command::Chronicle(cmd(json!({
            "action": "write", "page": page, "summary": "the proposal",
            "body": "Nobody saw @Zed Orrin leave. The proposal says ports stay open.",
            "citations": [{"record": "radio", "id": radio}, {"record": "proposal", "id": proposal}]
        }))),
    );
    s.player(
        b,
        Command::Chronicle(cmd(json!({"action": "talk", "page": page, "text": "@Ada Quill wrote this well."}))),
    );
    // Two plain comments: the second supersedes the first's notice to Ada (a fold).
    for text in ["The dock was empty.", "The dock is still empty."] {
        s.player(b, Command::Chronicle(cmd(json!({"action": "talk", "page": page, "text": text}))));
    }
    s.player(
        a,
        Command::Identity(cmd(json!({
            "action": "log", "text": "Waiting for @Bram Hale.",
            "citations": [{"record": "proposal", "id": proposal}]
        }))),
    );
    s.run(
        Actor::System,
        Command::Admin(AdminArgs::ConstitutionStrike {
            article: None,
            proposal: Some(proposal),
            reason: None,
        }),
    )
    .unwrap();
    s
}

/// Remove `key` from an object, keeping the order of the others; count it when present.
fn drop_key(v: &mut Value, key: &str, n: &mut usize) {
    if let Some(o) = v.as_object_mut()
        && o.shift_remove(key).is_some()
    {
        *n += 1;
    }
}

fn drop_everywhere(v: &mut Value, key: &str, n: &mut usize) {
    match v {
        Value::Object(o) => {
            if o.shift_remove(key).is_some() {
                *n += 1;
            }
            o.values_mut().for_each(|c| drop_everywhere(c, key, n));
        }
        Value::Array(a) => a.iter_mut().for_each(|c| drop_everywhere(c, key, n)),
        _ => {}
    }
}

fn each<'a>(v: &'a mut Value, path: &str) -> Vec<&'a mut Value> {
    let mut at = vec![v];
    for part in path.split('.').filter(|p| !p.is_empty()) {
        at = at
            .into_iter()
            .flat_map(|v| -> Vec<&mut Value> {
                match (part, v) {
                    ("*", Value::Object(o)) => o.values_mut().collect(),
                    ("[]", Value::Array(a)) => a.iter_mut().collect(),
                    (k, Value::Object(o)) => o.get_mut(k).into_iter().collect(),
                    _ => vec![],
                }
            })
            .collect();
    }
    at
}

/// The world as a 1.57 engine wrote it: every 1.58 key removed by its path. Returns how many of
/// each were found, so the fixture is known to have used them.
fn as_1_57(w: &World) -> (Value, Map<String, Value>) {
    let mut v = serde_json::to_value(w).unwrap();
    let mut found = Map::new();
    let mut strip = |v: &mut Value, path: &str, keys: &[&str]| {
        for key in keys {
            let mut n = 0;
            for item in each(v, path) {
                drop_key(item, key, &mut n);
            }
            found.insert(format!("{path}.{key}"), json!(n));
        }
    };
    strip(&mut v, "", &["mentions", "next_language"]);
    strip(
        &mut v,
        "languages.*",
        &["id", "known", "tagged", "last_tagged_tick", "retired", "revived_tick"],
    );
    strip(&mut v, "radio.[]", &["mentions", "language"]);
    strip(
        &mut v,
        "players.*.inbox.[]",
        &["quote", "about", "mention", "fold", "superseded_by", "struck_quote"],
    );
    strip(&mut v, "chronicle.pages.*", &["contributors"]);
    strip(&mut v, "chronicle.pages.*.revisions.[]", &["mentions"]);
    strip(&mut v, "chronicle.pages.*.talk.[]", &["mentions"]);
    strip(&mut v, "chronicle.pages.*.revisions.[].citations.[]", &["struck_original"]);
    strip(&mut v, "chronicle.pages.*.talk.[].citations.[]", &["struck_original"]);
    strip(&mut v, "identities.*.log.[]", &["mentions"]);
    strip(&mut v, "identities.*.log.[].citations.[]", &["struck_original"]);
    for root in ["constitution", "archive.constitutions"] {
        let mut n = 0;
        for item in each(&mut v, root) {
            drop_everywhere(item, "mentions", &mut n);
            drop_everywhere(item, "struck_original", &mut n);
        }
        found.insert(root.to_string(), json!(n));
    }
    let rules = v["rules"].as_object_mut().unwrap();
    let keys: Vec<String> = rules
        .keys()
        .filter(|k| k.starts_with("mention_") || k.starts_with("language_"))
        // The 1.59 society rewards share the prefix; their adoption is tested in society_rewards.rs.
        .filter(|k| !["language_adopt_credits", "language_credits_per_day"].contains(&k.as_str()))
        .cloned()
        .collect();
    for k in &keys {
        rules.shift_remove(k);
    }
    found.insert("rules".into(), json!(keys.len()));
    (v, found)
}

/// The native world with every 1.58 value reset to what an adopted world reads: the same
/// contents, as the 1.57 engine would have held them.
fn without_1_58(w: &World) -> World {
    let mut w = w.clone();
    w.mentions = Default::default();
    w.next_language = 0;
    for l in w.languages.values_mut() {
        l.id = 0;
        l.known.clear();
        l.tagged = 0;
        l.last_tagged_tick = None;
        l.retired = None;
        l.revived_tick = None;
    }
    for m in &mut w.radio {
        m.mentions = Default::default();
        m.language = None;
    }
    for p in w.players.values_mut() {
        for n in &mut p.inbox {
            n.quote = None;
            n.about = None;
            n.mention = None;
            n.fold = None;
            n.superseded_by = None;
            n.struck_quote = None;
        }
    }
    for page in w.chronicle.pages.values_mut() {
        page.contributors = None;
        for r in &mut page.revisions {
            r.mentions = Default::default();
            r.citations.iter_mut().for_each(|c| c.struck_original = None);
        }
        for c in &mut page.talk {
            c.mentions = Default::default();
            c.citations.iter_mut().for_each(|c| c.struck_original = None);
        }
    }
    for id in w.identities.values_mut() {
        for e in &mut id.log {
            e.mentions = Default::default();
            e.citations.iter_mut().for_each(|c| c.struck_original = None);
        }
    }
    let articles = |a: &mut af_core::constitution::Article| {
        a.struck_original = None;
        a.history.iter_mut().for_each(|v| v.struck_original = None);
    };
    let c = &mut w.constitution;
    c.articles.iter_mut().for_each(articles);
    c.removed.iter_mut().for_each(|r| articles(&mut r.article));
    c.archive.iter_mut().flat_map(|s| s.articles.iter_mut()).for_each(articles);
    for p in &mut c.proposals {
        p.mentions = Default::default();
        p.struck_original = None;
    }
    for r in &mut w.archive.constitutions {
        r.articles.iter_mut().for_each(articles);
        r.removed.iter_mut().for_each(|r| articles(&mut r.article));
        for p in &mut r.proposals {
            p.mentions = Default::default();
            p.struck_original = None;
        }
    }
    w
}

/// Every hot record written again from the adopted world equals, byte for byte, what the 1.57
/// engine stored.
fn hot_records_round_trip(old: &Value, w: &World) {
    let bytes = |v: &Value| serde_json::to_vec(v).unwrap();
    for (i, m) in w.radio.iter().enumerate() {
        assert_eq!(serde_json::to_vec(m).unwrap(), bytes(&old["radio"][i]), "radio {i}");
    }
    for (id, p) in &w.players {
        for (i, n) in p.inbox.iter().enumerate() {
            let stored = &old["players"][id.to_string()]["inbox"][i];
            assert_eq!(serde_json::to_vec(n).unwrap(), bytes(stored), "notice {i} of {id}");
        }
    }
    for (id, page) in &w.chronicle.pages {
        let stored = &old["chronicle"]["pages"][id.to_string()];
        for (i, r) in page.revisions.iter().enumerate() {
            assert_eq!(
                serde_json::to_vec(r).unwrap(),
                bytes(&stored["revisions"][i]),
                "page {id} revision {i}"
            );
        }
        for (i, c) in page.talk.iter().enumerate() {
            assert_eq!(serde_json::to_vec(c).unwrap(), bytes(&stored["talk"][i]), "page {id} talk {i}");
        }
    }
    for (id, ident) in &w.identities {
        for (i, e) in ident.log.iter().enumerate() {
            let stored = &old["identities"][id.to_string()]["log"][i];
            assert_eq!(serde_json::to_vec(e).unwrap(), bytes(stored), "log {i} of {id}");
        }
    }
    assert_eq!(
        serde_json::to_vec(&w.constitution).unwrap(),
        bytes(&old["constitution"]),
        "the constitution, its proposals and articles"
    );
}

#[test]
fn a_1_57_world_loads_with_1_58_defaults_and_hashes_like_a_native_one() {
    let s = rich_world();
    let mut replayed = generate(&GenesisConfig::new(156, 60));
    for e in &s.journal {
        let _ = apply(&mut replayed, e);
    }
    assert_eq!(replayed.state_hash(), s.w.state_hash(), "the fixture replays");
    let (old, found) = as_1_57(&s.w);
    // The fixture used every 1.58 field it strips.
    for key in [
        ".mentions",
        ".next_language",
        "languages.*.id",
        "languages.*.known",
        "languages.*.tagged",
        "languages.*.last_tagged_tick",
        "radio.[].mentions",
        "radio.[].language",
        "players.*.inbox.[].quote",
        "players.*.inbox.[].about",
        "players.*.inbox.[].mention",
        "players.*.inbox.[].fold",
        "players.*.inbox.[].superseded_by",
        "players.*.inbox.[].struck_quote",
        "chronicle.pages.*.contributors",
        "chronicle.pages.*.revisions.[].mentions",
        "chronicle.pages.*.talk.[].mentions",
        "chronicle.pages.*.revisions.[].citations.[].struck_original",
        "identities.*.log.[].mentions",
        "identities.*.log.[].citations.[].struck_original",
        "constitution",
        "rules",
    ] {
        assert!(found[key].as_u64().unwrap() > 0, "the fixture sets {key}: {found:?}");
    }
    assert_eq!(found["rules"], json!(5), "five 1.58 rules");
    let text = serde_json::to_string(&old).unwrap();
    assert!(!text.contains("struck_original") && !text.contains("struck_quote"));
    assert!(!text.contains("next_language") && !text.contains("superseded_by"));

    let adopted: World = serde_json::from_str(&text).expect("a 1.57 world loads into 1.58");
    hot_records_round_trip(&old, &adopted);
    assert_eq!(adopted.mentions, Default::default());
    assert_eq!(adopted.next_language, 0);
    assert!(adopted.languages.values().all(|l| l.id == 0 && l.known.is_empty()));
    // A page from before stored contributor lists still reads its byline (worked out once).
    assert!(adopted.chronicle.pages.values().all(|p| p.contributors.is_none()));
    for (id, p) in &adopted.chronicle.pages {
        assert_eq!(chronicle::contributors(p), s.w.chronicle.pages[id].contributors.clone().unwrap());
    }
    // The new rules read as their defaults, alone and in the world.
    let defaults = Rules::default();
    let rules_only: Rules = serde_json::from_value(old["rules"].clone()).unwrap();
    for r in [&adopted.rules, &rules_only] {
        assert_eq!(r.mention_notify_max, defaults.mention_notify_max);
        assert_eq!(r.mention_window_ticks, defaults.mention_window_ticks);
        assert_eq!(r.language_stale_days, defaults.language_stale_days);
        assert_eq!(r.language_retire_days, defaults.language_retire_days);
        assert_eq!(r.language_adopted_grace_days, defaults.language_adopted_grace_days);
    }
    // Nothing else moved: the adopted world is the native one with the 1.58 values reset.
    assert_eq!(adopted.state_hash(), without_1_58(&s.w).state_hash());

    // Ids the readers predict now are the ids the first tick assigns, in creation order, and the
    // creator and adopters are recorded as knowing the versions that existed.
    let predicted: Vec<(String, u64)> = adopted
        .languages
        .keys()
        .map(|k| (k.clone(), language::id_of(&adopted, k)))
        .collect();
    let mut a = Sim {
        w: adopted.clone(),
        journal: vec![],
    };
    let mut b = Sim {
        w: adopted,
        journal: vec![],
    };
    for sim in [&mut a, &mut b] {
        sim.run(Actor::System, Command::Tick).unwrap();
    }
    for (k, id) in &predicted {
        assert_eq!(a.w.languages[k].id, *id, "{k} keeps the id read before the tick");
    }
    assert_eq!(a.w.languages["tradecant"].id, 1, "the first language defined");
    assert_eq!(a.w.languages["zedcode"].id, 2);
    assert_eq!(a.w.next_language, 3);
    let cant = &a.w.languages["tradecant"];
    assert_eq!(cant.known.values().copied().collect::<Vec<_>>(), vec![1, 1], "creator and adopter");
    // The first tick stores every page's contributor list: the one a native world holds.
    for (id, p) in &a.w.chronicle.pages {
        assert_eq!(p.contributors, s.w.chronicle.pages[id].contributors, "page {id}");
    }
    assert!(a.w.chronicle.pages.values().any(|p| !p.contributors.as_ref().unwrap().is_empty()));
    // The adopted universe runs on: the same entries give the same world.
    assert_eq!(a.w.state_hash(), b.w.state_hash(), "replay of the adopted world matches");
}
