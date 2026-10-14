//! Engine 1.60: a stored language has a kind, `language` (the default, and what every entry
//! stored before 1.60 reads as) or `format` (a fixed record or protocol). The creator sets it with
//! `define`, so a change is a new version; both kinds earn the same adoption reward.
use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::language::{self, Kind};
use af_core::model::*;
use af_core::{World, query};
use serde_json::{Value, json};

struct Sim {
    w: World,
    journal: Vec<Entry>,
    cfg: GenesisConfig,
}

impl Sim {
    fn new(seed: u64) -> Self {
        let cfg = GenesisConfig::new(seed, 60);
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
    fn lang(&mut self, pid: PlayerId, args: Value) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(Actor::Player(pid), Command::Language(serde_json::from_value(args).unwrap()))
    }
    fn read(&self, pid: PlayerId, args: Value) -> Value {
        language::read(&self.w, pid, &serde_json::from_value(args).unwrap()).unwrap()
    }
    fn bank(&self, pid: PlayerId) -> i64 {
        self.w.players[&pid].bank
    }
    /// Replay from genesis, every entry first round-tripped through JSON as the store does.
    fn replays(&self) {
        let mut w = generate(&self.cfg);
        for e in &self.journal {
            let text = serde_json::to_string(e).unwrap();
            let back: Entry = serde_json::from_str(&text).unwrap();
            assert_eq!(&back, e, "journal entry round-trips");
            let _ = apply(&mut w, &back);
        }
        assert_eq!(w.state_hash(), self.w.state_hash(), "replay reproduces the world");
    }
}

const LEDGER: &str = "A report of one trade, fields separated by semicolons: T tick; G commodity; N net credits.";

#[test]
fn define_takes_a_kind_and_language_is_the_default() {
    let mut s = Sim::new(601);
    let a = s.join("Ada");
    let plain = s
        .lang(
            a,
            json!({"action": "define", "name": "Dockcant", "explanation": "Trade shorthand.", "share": "captains"}),
        )
        .unwrap();
    assert_eq!(plain.reply["kind"], "language");
    assert!(!plain.summary.contains("format"), "{}", plain.summary);
    let format = s
        .lang(
            a,
            json!({"action": "define", "name": "Quay Ledger", "kind": "format", "explanation": LEDGER, "examples": "T812;G ore;N 4400", "share": "captains"}),
        )
        .unwrap();
    assert_eq!(format.reply["kind"], "format");
    assert!(
        format.summary.starts_with("Stored Quay Ledger (a format), version 1."),
        "{}",
        format.summary
    );
    let id = format.reply["id"].as_u64().unwrap();
    let l = &s.w.languages["quayledger"];
    assert_eq!((l.kind, l.versions[0].kind), (Kind::Format, Kind::Format));
    assert_eq!(s.w.languages["dockcant"].kind, Kind::Language);

    // list, read, the briefing's own line and game_rules show it.
    let list = s.read(a, json!({"action": "list"}));
    let kinds: Vec<(&str, &str)> = list["languages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["name"].as_str().unwrap(), r["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(kinds, [("Dockcant", "language"), ("Quay Ledger", "format")]);
    let one = s.read(a, json!({"action": "read", "id": id}));
    assert_eq!(
        (one["language"]["kind"].clone(), one["version"]["kind"].clone()),
        (json!("format"), json!("format"))
    );
    let old = s.read(a, json!({"action": "read", "name": "Dockcant"}));
    assert!(old["version"].get("kind").is_none(), "a language version carries no kind field");
    let brief = query::briefing(&s.w, a).unwrap();
    assert_eq!(
        brief["comms"]["languages"]["yours"],
        json!([format!("Dockcant (#{})", id - 1), format!("Quay Ledger (#{id}, format)")])
    );
    assert!(language::rules_view(&s.w)["kinds"].as_str().unwrap().contains("kind=format"));
    // The operator's view.
    let ops = language::operator_view(&s.w);
    let kinds: Vec<&str> = ops["languages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["language", "format"]);
    s.replays();
}

#[test]
fn only_the_creator_changes_the_kind_and_a_change_is_a_new_version() {
    let mut s = Sim::new(602);
    let (a, b) = (s.join("Ada"), s.join("Bex"));
    s.lang(
        a,
        json!({"action": "define", "name": "Quay Ledger", "explanation": "Semicolon reports.", "share": "captains"}),
    )
    .unwrap();
    // Another captain may add a version but not change the kind; nothing is stored.
    let e = s
        .lang(
            b,
            json!({"action": "define", "name": "Quay Ledger", "kind": "format", "explanation": LEDGER}),
        )
        .unwrap_err();
    assert_eq!(e.code, "not_creator");
    assert_eq!(s.w.languages["quayledger"].versions.len(), 1);
    // Naming the current kind is no change.
    s.lang(
        b,
        json!({"action": "define", "name": "Quay Ledger", "kind": "language", "explanation": "Semicolon reports, v2."}),
    )
    .unwrap();
    // The creator's change appends a version; earlier versions keep their kind.
    let out = s
        .lang(
            a,
            json!({"action": "define", "name": "Quay Ledger", "kind": "format", "explanation": LEDGER}),
        )
        .unwrap();
    assert_eq!(
        (out.reply["version"].clone(), out.reply["kind"].clone()),
        (json!(3), json!("format"))
    );
    // A later version without `kind` keeps the format.
    s.lang(
        b,
        json!({"action": "define", "name": "Quay Ledger", "explanation": format!("{LEDGER} P position.")}),
    )
    .unwrap();
    let l = &s.w.languages["quayledger"];
    assert_eq!(l.kind, Kind::Format);
    assert_eq!(
        l.versions.iter().map(|v| v.kind).collect::<Vec<_>>(),
        [Kind::Language, Kind::Language, Kind::Format, Kind::Format]
    );
    let v1 = s.read(b, json!({"action": "read", "name": "Quay Ledger", "version": 1}));
    assert!(v1["version"].get("kind").is_none());
    assert_eq!(v1["language"]["kind"], "format", "the summary shows the current kind");
    // Back to a language, again by the creator only.
    s.lang(
        a,
        json!({"action": "define", "name": "Quay Ledger", "kind": "language", "explanation": "Plain words now."}),
    )
    .unwrap();
    assert_eq!(s.w.languages["quayledger"].kind, Kind::Language);
    assert_eq!(s.w.languages["quayledger"].versions.len(), 5);
    s.replays();
}

#[test]
fn an_unknown_kind_is_refused_before_anything_is_journaled() {
    for kind in [json!("dialect"), json!("Format"), json!(1), json!("")] {
        let r = serde_json::from_value::<language::Args>(json!({"action": "define", "name": "X", "explanation": "e", "kind": kind}));
        assert!(r.is_err(), "{kind} is not a kind");
    }
    let ok: language::Args = serde_json::from_value(json!({"action": "define", "name": "X", "explanation": "e", "kind": null})).unwrap();
    assert_eq!(ok.kind, None);
}

#[test]
fn entries_written_before_1_60_read_and_replay_as_languages() {
    // A pre-1.60 journal entry has no `kind`: it parses, and defines a language.
    let entry: Command = serde_json::from_value(json!({"type": "language", "args": {"action": "define", "name": "Dockcant", "explanation": "Old.", "examples": null, "share": "captains", "version": null, "id": null}})).unwrap();
    let Command::Language(args) = &entry else {
        panic!("a language command")
    };
    assert_eq!(args.kind, None);
    let mut s = Sim::new(603);
    let a = s.join("Ada");
    s.run(Actor::Player(a), entry).unwrap();
    let l = s.w.languages["dockcant"].clone();
    assert_eq!(l.kind, Kind::Language);
    // A language serializes exactly as before 1.60 (no `kind` anywhere), so the world hash of a
    // store without formats is unchanged, and the stored form without it reads back as language.
    let text = serde_json::to_value(&l).unwrap();
    assert!(text.get("kind").is_none());
    assert!(text["versions"][0].get("kind").is_none());
    let back: language::Language = serde_json::from_value(text).unwrap();
    assert_eq!(back, l);
    // A format round-trips with its kind.
    s.lang(
        a,
        json!({"action": "define", "name": "Ledger", "kind": "format", "explanation": LEDGER}),
    )
    .unwrap();
    let f = serde_json::to_value(&s.w.languages["ledger"]).unwrap();
    assert_eq!(
        (f["kind"].clone(), f["versions"][0]["kind"].clone()),
        (json!("format"), json!("format"))
    );
    assert_eq!(serde_json::from_value::<language::Language>(f).unwrap(), s.w.languages["ledger"]);
    s.replays();
}

#[test]
fn adopting_a_format_pays_its_creator_exactly_as_a_language_does() {
    let mut s = Sim::new(604);
    let ids: Vec<PlayerId> = (0..4).map(|n| s.join(&format!("Captain {n}"))).collect();
    let reward = s.w.rules.language_adopt_credits;
    assert!(reward > 0);
    s.lang(
        ids[0],
        json!({"action": "define", "name": "Trade Cant", "explanation": "Short words.", "share": "captains"}),
    )
    .unwrap();
    s.lang(
        ids[1],
        json!({"action": "define", "name": "Quay Ledger", "kind": "format", "explanation": LEDGER, "share": "captains"}),
    )
    .unwrap();
    let (lang_bank, format_bank) = (s.bank(ids[0]), s.bank(ids[1]));
    s.lang(ids[2], json!({"action": "adopt", "name": "Trade Cant"})).unwrap();
    s.lang(ids[2], json!({"action": "adopt", "name": "Quay Ledger"})).unwrap();
    assert_eq!(s.bank(ids[0]) - lang_bank, reward);
    assert_eq!(s.bank(ids[1]) - format_bank, reward, "the same reward for a format");
    // Once per adopter, as for a language.
    s.lang(ids[2], json!({"action": "adopt", "name": "Quay Ledger"})).unwrap();
    assert_eq!(s.bank(ids[1]) - format_bank, reward);
    s.lang(ids[3], json!({"action": "adopt", "name": "Quay Ledger"})).unwrap();
    assert_eq!(s.bank(ids[1]) - format_bank, 2 * reward);
    s.replays();
}
