//! Engine 1.59: society rewards. Credits for the Chronicle (canon pages, production credits),
//! languages (another captain's first adoption) and improve awards, paid into the bank once,
//! within trailing-day caps, counted in net worth, `earned` and the daily wealth race, and shown
//! in the full briefing with live amounts.
use af_core::chronicle::{self, AdminAction, Kind};
use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::identity;
use af_core::model::*;
use af_core::query::BriefingArgs;
use af_core::{Command, World, player};
use serde_json::{Value, json};

struct Sim {
    w: World,
    journal: Vec<Entry>,
    cfg: GenesisConfig,
}

impl Sim {
    /// Ten ticks a game day, so the daily caps reset in a few ticks.
    fn new(seed: u64) -> Self {
        let mut cfg = GenesisConfig::new(seed, 60);
        cfg.rules.tick_ms = 8_640_000;
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
    fn next_day(&mut self) {
        let tpd = player::ticks_per_day(&self.w.rules) as u64;
        let to = self.w.tick + tpd;
        while self.w.tick < to {
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
    fn document(&mut self, pid: PlayerId, home: &str) {
        let a = identity::Args {
            action: identity::Action::Set,
            age: Some(40),
            pronouns: Some("she/her".into()),
            homeworld: Some(home.into()),
            bio: Some("A trader.".into()),
            backstory: Some("## Dream\nA farm.".into()),
            ..Default::default()
        };
        self.run(Actor::Player(pid), Command::Identity(Box::new(a))).unwrap();
    }
    fn chronicle(&mut self, pid: PlayerId, a: chronicle::Args) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(Actor::Player(pid), Command::Chronicle(a))
    }
    fn admin(&mut self, decision: AdminAction, production: u64, links: Option<Vec<String>>) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(
            Actor::System,
            Command::Admin(AdminArgs::Chronicle(chronicle::AdminArgs {
                decision,
                production: Some(production),
                page: None,
                note: None,
                links,
            })),
        )
    }
    fn rules(&mut self, patch: Value) {
        self.run(Actor::System, Command::Admin(AdminArgs::SetRules { patch })).unwrap();
    }
    fn lang(&mut self, pid: PlayerId, args: Value) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(Actor::Player(pid), Command::Language(serde_json::from_value(args).unwrap()))
    }
    fn p(&self, pid: PlayerId) -> &Player {
        &self.w.players[&pid]
    }
    fn bank(&self, pid: PlayerId) -> i64 {
        self.p(pid).bank
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

fn write(title: &str) -> chronicle::Args {
    chronicle::Args {
        action: chronicle::Action::Write,
        kind: Some(Kind::Event),
        title: Some(title.into()),
        body: Some(format!("What happened at {title}.")),
        ..Default::default()
    }
}

fn endorse(page: u64) -> chronicle::Args {
    chronicle::Args {
        action: chronicle::Action::Endorse,
        page: Some(page),
        ..Default::default()
    }
}

/// A page by `author` endorsed into canon by two independent captains.
fn canon_page(s: &mut Sim, author: PlayerId, endorsers: [PlayerId; 2], title: &str) -> u64 {
    let page = s.chronicle(author, write(title)).unwrap().reply["page"].as_u64().unwrap();
    s.chronicle(endorsers[0], endorse(page)).unwrap();
    let out = s.chronicle(endorsers[1], endorse(page)).unwrap();
    assert_eq!(out.reply["canon"], true);
    page
}

/// Net worth, earned this season and earned today: a society payout raises all three alike.
fn worth(s: &Sim, pid: PlayerId) -> (i64, i64, i64) {
    let p = s.p(pid);
    (
        player::net_worth(&s.w, pid),
        af_core::public::earned(&s.w, p),
        player::earned_today(&s.w, pid),
    )
}

fn plus(a: (i64, i64, i64), n: i64) -> (i64, i64, i64) {
    (a.0 + n, a.1 + n, a.2 + n)
}

fn last_text(s: &Sim, pid: PlayerId, kind: &str) -> String {
    s.p(pid)
        .inbox
        .iter()
        .rev()
        .find(|n| n.kind == kind)
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

#[test]
fn canon_pays_the_creator_once_within_the_daily_cap_and_zero_switches_it_off() {
    let mut s = Sim::new(159);
    let [a, b, c, d] = ["Ada Mbeki", "Bram Ostrowski", "Chidi Okonjo", "Dalia Haddad"].map(|n| s.join(n));
    for (p, home) in [(a, "Lowmoor"), (b, "Pell"), (c, "Okun"), (d, "Sabra")] {
        s.document(p, home);
    }
    let r = s.w.rules.clone();
    assert_eq!(
        (r.chronicle_canon_credits, r.chronicle_credit_credits, r.chronicle_credits_per_day),
        (20_000, 40_000, 100_000),
        "the defaults"
    );
    let (bank_a, bank_b, before) = (s.bank(a), s.bank(b), worth(&s, a));
    let credits_a = s.p(a).credits;
    let page = canon_page(&mut s, a, [b, c], "The first arrivals");
    assert_eq!(s.bank(a), bank_a + 20_000, "the creator is paid into the bank");
    assert_eq!(s.p(a).credits, credits_a, "never onto the ship");
    assert_eq!(s.bank(b), bank_b, "an endorser is not paid");
    assert_eq!(worth(&s, a), plus(before, 20_000), "net worth, earned and the daily race count it");
    assert_eq!(s.p(a).stats.society_rewards, 20_000);
    assert_eq!(s.p(a).wired_today, 0, "income, not a wire");
    assert!(last_text(&s, a, "chronicle").contains("20000 credits were paid into your bank"));
    // Influence is unchanged in kind: the creator still gets canon Influence.
    assert_eq!(chronicle::influence(&s.w, a), chronicle::CANON_INFLUENCE);

    // A later revision (by anyone) and a further endorsement pay nothing more.
    let mut rev = write("The first arrivals");
    rev.kind = None;
    rev.page = Some(page);
    rev.body = Some("What happened at the first arrivals, retold by a second hand.".into());
    s.chronicle(b, rev.clone()).unwrap();
    rev.body = Some("And retold again by its creator, at length.".into());
    s.chronicle(a, rev).unwrap();
    let out = s.chronicle(d, endorse(page)).unwrap();
    assert_eq!(out.reply["canon"], true);
    assert_eq!((s.bank(a), s.bank(b)), (bank_a + 20_000, bank_b), "paid once per page");

    // The daily cap: four more canon pages reach 100,000; the sixth pays nothing, with a note.
    for n in 2..=5 {
        canon_page(&mut s, a, [b, c], &format!("Page {n}"));
    }
    assert_eq!(s.bank(a), bank_a + 100_000);
    canon_page(&mut s, a, [b, c], "Page 6");
    assert_eq!(s.bank(a), bank_a + 100_000, "over the daily cap nothing is paid");
    assert!(last_text(&s, a, "chronicle").contains("daily cap of 100000"));
    assert_eq!(
        af_core::society::paid_in_window(&s.w, a, af_core::society::Source::Chronicle),
        100_000
    );
    // The skipped payout is never paid later; the next day's canon is paid again.
    s.next_day();
    assert_eq!(s.bank(a), bank_a + 100_000);
    canon_page(&mut s, a, [b, c], "Page 7");
    assert_eq!(s.bank(a), bank_a + 120_000);
    assert!(s.p(a).society.len() <= 1, "the ledger keeps only the trailing day");

    // 0 switches the payout off: no credits and no credit sentence.
    s.rules(json!({"chronicle_canon_credits": 0}));
    canon_page(&mut s, a, [b, c], "Page 8");
    assert_eq!(s.bank(a), bank_a + 120_000);
    assert!(!last_text(&s, a, "chronicle").contains("credits"));
    s.replays();
}

#[test]
fn a_release_pays_each_credited_captain_once() {
    let mut s = Sim::new(160);
    let [a, b] = ["Ada Mbeki", "Bram Ostrowski"].map(|n| s.join(n));
    s.document(a, "Lowmoor");
    s.document(b, "Pell");
    let pitch = chronicle::Args {
        action: chronicle::Action::Pitch,
        title: Some("Debts".into()),
        text: Some("A welder pays off her brother's debts.".into()),
        format: Some(chronicle::Format::Audio),
        cast: Some(vec!["Ada Mbeki".into(), "Bram Ostrowski".into()]),
        ..Default::default()
    };
    let x = s.chronicle(a, pitch).unwrap().reply["production"].as_u64().unwrap();
    s.admin(AdminAction::Greenlight, x, None).unwrap();
    s.admin(AdminAction::Lock, x, None).unwrap();
    let (bank_a, bank_b, before) = (s.bank(a), s.bank(b), worth(&s, b));
    let out = s
        .admin(AdminAction::Release, x, Some(vec!["https://example.com/debts.mp3".into()]))
        .unwrap();
    assert_eq!(s.bank(a), bank_a + 40_000);
    assert_eq!(s.bank(b), bank_b + 40_000);
    assert_eq!(worth(&s, b), plus(before, 40_000));
    assert_eq!(out.reply["credits_paid"][b.to_string()], 40_000);
    assert!(last_text(&s, b, "chronicle").contains("40000 credits were paid into your bank"));
    // A production is released once.
    let again = s.admin(AdminAction::Release, x, Some(vec!["https://example.com/2".into()]));
    assert_eq!(again.unwrap_err().code, "not_ready");
    assert_eq!((s.bank(a), s.bank(b)), (bank_a + 40_000, bank_b + 40_000));
    s.replays();
}

/// A captain outside the cast who makes a trivial edit to a greenlit script is in the credits
/// (and their Influence) but is not paid; a substantive script writer is.
#[test]
fn a_trivial_script_edit_earns_no_release_credits() {
    let mut s = Sim::new(166);
    let [a, c, d] = ["Ada Mbeki", "Chidi Okonjo", "Dara Quill"].map(|n| s.join(n));
    for (p, home) in [(a, "Lowmoor"), (c, "Okun"), (d, "Vess")] {
        s.document(p, home);
    }
    let pitch = chronicle::Args {
        action: chronicle::Action::Pitch,
        title: Some("Debts".into()),
        text: Some("A welder pays off her brother's debts.".into()),
        format: Some(chronicle::Format::Audio),
        cast: Some(vec!["Ada Mbeki".into()]),
        ..Default::default()
    };
    let x = s.chronicle(a, pitch).unwrap().reply["production"].as_u64().unwrap();
    s.admin(AdminAction::Greenlight, x, None).unwrap();
    let script = s.w.chronicle.productions[&x].script_page.unwrap();
    let edit = |body: String| chronicle::Args {
        action: chronicle::Action::Write,
        page: Some(script),
        body: Some(body),
        ..Default::default()
    };
    let body = s.w.chronicle.pages[&script].current().body.clone();
    s.chronicle(c, edit(format!("{body}!"))).unwrap();
    let body = s.w.chronicle.pages[&script].current().body.clone();
    s.chronicle(
        d,
        edit(format!(
            "{body}\n\n**NARRATOR:** The welder counts her brother's debts by lamplight, one coin at a time."
        )),
    )
    .unwrap();
    s.admin(AdminAction::Lock, x, None).unwrap();
    let (bank_a, bank_c, bank_d) = (s.bank(a), s.bank(c), s.bank(d));
    let out = s
        .admin(AdminAction::Release, x, Some(vec!["https://example.com/debts.mp3".into()]))
        .unwrap();
    let credits: Vec<PlayerId> = out.reply["credits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as PlayerId)
        .collect();
    assert!(credits.contains(&c) && credits.contains(&d), "both writers are in the credits");
    assert_eq!(s.bank(a), bank_a + 40_000, "the pitcher and cast are paid");
    assert_eq!(s.bank(d), bank_d + 40_000, "a substantive script writer is paid");
    assert_eq!(s.bank(c), bank_c, "a trivial edit is not paid");
    assert!(out.reply["credits_paid"].get(c.to_string()).is_none());
    assert!(!last_text(&s, c, "chronicle").contains("paid into your bank"));
    s.replays();
}

#[test]
fn the_first_adoption_by_another_captain_pays_the_creator_once_within_the_cap() {
    let mut s = Sim::new(161);
    let ids: Vec<PlayerId> = (0..9).map(|n| s.join(&format!("Captain {n}"))).collect();
    let creator = ids[0];
    s.lang(
        creator,
        json!({"action":"define","name":"Trade Cant","explanation":"Short words for prices.","share":"captains"}),
    )
    .unwrap();
    let (bank, before, adopter_bank) = (s.bank(creator), worth(&s, creator), s.bank(ids[1]));
    // Defining and adopting your own language pay nothing.
    s.lang(creator, json!({"action":"adopt","name":"Trade Cant"})).unwrap();
    assert_eq!(s.bank(creator), bank);
    // Another captain's first adoption pays the creator; adopting again does not.
    s.lang(ids[1], json!({"action":"adopt","name":"Trade Cant"})).unwrap();
    assert_eq!(s.bank(creator), bank + 5_000);
    assert_eq!(worth(&s, creator), plus(before, 5_000));
    let id = s.w.languages.values().find(|l| l.name == "Trade Cant").unwrap().id;
    let text = last_text(&s, creator, "reward");
    assert!(text.contains(&format!("adopted your language #{id}")), "{text}");
    assert!(!text.contains("Trade Cant"), "server text never carries the captain-chosen name");
    assert!(!last_text(&s, creator, "reward").contains("Captain 1"), "the adopter is not named");
    s.lang(ids[1], json!({"action":"adopt","name":"Trade Cant"})).unwrap();
    assert_eq!(s.bank(creator), bank + 5_000, "once per adopter");
    assert_eq!(s.bank(ids[1]), adopter_bank, "the adopter is not paid");
    // An NPC adopting pays nothing.
    if let Some(npc) = s.w.players.values().find(|p| p.npc.is_some()).map(|p| p.id) {
        let _ = s.lang(npc, json!({"action":"adopt","name":"Trade Cant"}));
        assert_eq!(s.bank(creator), bank + 5_000, "no payout for an NPC adopter");
    }
    // The cap: 25,000 a day, so the sixth adopter that day pays nothing (with a note).
    for p in &ids[2..6] {
        s.lang(*p, json!({"action":"adopt","name":"Trade Cant"})).unwrap();
    }
    assert_eq!(s.bank(creator), bank + 25_000);
    s.lang(ids[6], json!({"action":"adopt","name":"Trade Cant"})).unwrap();
    assert_eq!(s.bank(creator), bank + 25_000);
    assert!(last_text(&s, creator, "reward").contains("daily cap of 25000"));
    s.next_day();
    s.lang(ids[7], json!({"action":"adopt","name":"Trade Cant"})).unwrap();
    assert_eq!(s.bank(creator), bank + 30_000, "the next day pays again");
    assert_eq!(s.p(creator).stats.society_rewards, 30_000);
    // 0 switches it off, with no notice.
    s.rules(json!({"language_adopt_credits": 0}));
    let notices = s.p(creator).inbox.len();
    s.lang(ids[8], json!({"action":"adopt","name":"Trade Cant"})).unwrap();
    assert_eq!((s.bank(creator), s.p(creator).inbox.len()), (bank + 30_000, notices));
    s.replays();
}

#[test]
fn society_credits_count_in_the_daily_wealth_race() {
    let mut s = Sim::new(162);
    let [a, b] = ["Ada Mbeki", "Bram Ostrowski"].map(|n| s.join(n));
    s.next_day();
    s.lang(
        a,
        json!({"action":"define","name":"Hand Talk","explanation":"Signs.","share":"captains"}),
    )
    .unwrap();
    s.lang(b, json!({"action":"adopt","name":"Hand Talk"})).unwrap();
    assert_eq!(player::earned_today(&s.w, a), 5_000);
    let wins = s.p(a).stats.daily_wins;
    s.next_day();
    assert_eq!(s.p(a).stats.daily_wins, wins + 1, "the payout won the day");
    s.replays();
}

// ---------------------------------------------------------------- the briefing

fn briefing(s: &Sim, pid: PlayerId, compact: bool, sections: Option<Vec<&str>>) -> Value {
    let a = BriefingArgs {
        compact,
        sections: sections.map(|v| v.into_iter().map(String::from).collect()),
        ..Default::default()
    };
    af_core::query::briefing_with(&s.w, pid, &a).unwrap()
}

fn improve_on(s: &mut Sim) {
    s.rules(json!({"improve_enabled": true}));
}

fn post(s: &mut Sim, title: &str, text: &str) -> u64 {
    let out = s
        .run(
            Actor::System,
            Command::Admin(
                serde_json::from_value(json!({"action":"improve","decision":"post","kind":"docs","title":title,"text":text})).unwrap(),
            ),
        )
        .unwrap();
    out.reply["id"].as_u64().unwrap()
}

#[test]
fn the_full_briefing_shows_each_reward_with_live_values_and_compact_keeps_counts() {
    let mut s = Sim::new(163);
    let [a, b, c] = ["Ada Mbeki", "Bram Ostrowski", "Chidi Okonjo"].map(|n| s.join(n));
    for (p, home) in [(a, "Lowmoor"), (b, "Pell"), (c, "Okun")] {
        s.document(p, home);
    }
    // The board is off: no block, in either form.
    assert!(briefing(&s, a, false, None).get("improve").is_none());
    assert!(briefing(&s, a, true, None).get("improve").is_none());

    // Languages: an invitation with the live amount while the captain keeps none.
    let full = briefing(&s, a, false, None);
    let invite = full["comms"]["languages"]["invite"].as_str().unwrap();
    assert!(
        invite.contains("5000 credits") && invite.contains("language action=define"),
        "{invite}"
    );
    let compact = briefing(&s, a, true, None);
    // Compact keeps one short line each: the scheduled check-in reads the compact briefing.
    let line = compact["comms"]["languages"]["invite"].as_str().unwrap();
    assert_eq!(
        line,
        "language action=define share=captains; 5000 credits per adopter; /languages.txt"
    );
    assert_eq!(compact["comms"]["languages"].as_object().unwrap().len(), 1, "only the line");

    // The Chronicle: no canon yet, so a line about the first canon pages.
    let ch = &full["chronicle"];
    let reward = ch["reward"].as_str().unwrap();
    assert!(
        reward.contains("no canon page yet") && reward.contains("20000 credits and 2000 Influence"),
        "{reward}"
    );
    assert_eq!(
        compact["chronicle"]["reward"],
        "canon page: 20000 credits and 2000 Influence; release: 40000 credits and 3000 Influence"
    );
    assert!(!compact["chronicle"]["how"].as_str().unwrap().contains("credits"));
    for short in [&compact["comms"]["languages"]["invite"], &compact["chronicle"]["reward"]] {
        assert!(short.as_str().unwrap().len() <= 100, "a short line: {short}");
    }

    // The improve board, switched on, with two items (one tiered).
    improve_on(&mut s);
    post(&mut s, "Fix the hint", "Plain text.");
    let tiered = post(&mut s, "Tidy the guide", "Tier M. Rewrite the docs section.");
    let full = briefing(&s, a, false, None);
    let im = &full["improve"];
    assert_eq!(im["claimable"], 2);
    assert_eq!(im["your_claim"], Value::Null);
    assert_eq!(im["items"][0]["id"], tiered, "newest first");
    assert_eq!(im["items"][0]["tier"], "M");
    assert_eq!(im["items"][0]["title"]["untrusted_player_text"], "Tidy the guide");
    assert!(im["items"][1].get("tier").is_none());
    let reward = im["reward"].as_str().unwrap();
    assert!(
        reward.contains("25 credits per Influence point") && reward.contains("How: /improve.txt."),
        "{reward}"
    );
    let compact = briefing(&s, a, true, None);
    assert_eq!(
        compact["improve"],
        json!({
            "claimable": 2,
            "your_claim": null,
            "items": im["items"],
            "reward": "shipped patch: S/M/L Influence + 25 credits/Influence; improve action=list; /improve.txt",
        })
    );
    assert!(compact["improve"]["reward"].as_str().unwrap().len() <= 100);
    // `improve` is a valid section name.
    let only = briefing(&s, a, false, Some(vec!["improve"]));
    assert!(only.get("improve").is_some() && only["retrieval"].get("unknown_sections").is_none());
    // A claim shows as yours, and the item leaves the claimable list.
    let out = s
        .run(
            Actor::Player(a),
            Command::Improve(serde_json::from_value(json!({"action":"claim","id":tiered})).unwrap()),
        )
        .unwrap();
    let im = briefing(&s, a, false, None)["improve"].clone();
    assert_eq!(im["your_claim"], json!({"id": tiered, "expires_tick": out.reply["expires_tick"]}));
    assert_eq!(im["claimable"], 1);
    assert_eq!(briefing(&s, b, false, None)["improve"]["claimable"], 1);

    // After canon and a language, the lines change: `how` carries the amounts, the invitation goes.
    canon_page(&mut s, a, [b, c], "Founding day");
    s.lang(
        a,
        json!({"action":"define","name":"Trade Cant","explanation":"Prices.","share":"captains"}),
    )
    .unwrap();
    let full = briefing(&s, a, false, None);
    assert!(full["chronicle"].get("reward").is_none());
    let how = full["chronicle"]["how"].as_str().unwrap();
    assert!(
        how.contains("20000 credits and 2000 Influence") && how.contains("40000 credits and 3000 Influence"),
        "{how}"
    );
    assert!(full["comms"]["languages"].get("invite").is_none());
    let id = af_core::language::id_of(&s.w, "tradecant");
    assert_eq!(full["comms"]["languages"]["yours"], json!([format!("Trade Cant (#{id})")]));

    // Live values: a rules change shows at once; 0 drops the credits from the text.
    s.rules(json!({"language_adopt_credits": 7000, "improve_credits_per_influence": 0, "chronicle_canon_credits": 0}));
    let full = briefing(&s, b, false, None);
    assert!(full["comms"]["languages"]["invite"].as_str().unwrap().contains("7000 credits"));
    assert!(
        briefing(&s, b, true, None)["comms"]["languages"]["invite"]
            .as_str()
            .unwrap()
            .contains("7000 credits per adopter")
    );
    assert!(!full["improve"]["reward"].as_str().unwrap().contains("credits"));
    assert!(
        !briefing(&s, b, true, None)["improve"]["reward"]
            .as_str()
            .unwrap()
            .contains("credits")
    );
    let how = full["chronicle"]["how"].as_str().unwrap();
    assert!(how.contains("a canon page pays its creator 2000 Influence)"), "{how}");

    // game_rules lists the live amounts.
    let info = af_core::query::game_info(&s.w);
    assert_eq!(info["chronicle"]["canon_credits"], 0);
    assert_eq!(info["chronicle"]["credit_credits"], 40_000);
    assert_eq!(info["languages"]["adopt_credits"], 7000);
    assert_eq!(info["improve"]["credits_per_influence"], 0);
    s.replays();
}

/// The tier comes from the operator: an accept note, or the text of an item the operator wrote.
/// A captain's own proposal text never sets it.
#[test]
fn the_tier_comes_from_the_operator_never_the_proposal_text() {
    let mut s = Sim::new(166);
    let [a, b] = ["Ada Mbeki", "Bram Ostrowski"].map(|n| s.join(n));
    s.document(a, "Lowmoor");
    s.document(b, "Pell");
    improve_on(&mut s);
    let submit = |s: &mut Sim, who: PlayerId, title: &str| -> u64 {
        let out = s
            .run(
                Actor::Player(who),
                Command::Improve(
                    serde_json::from_value(json!({"action":"submit","kind":"docs","title":title,"text":"Tier L. Rewrite everything."}))
                        .unwrap(),
                ),
            )
            .unwrap();
        out.reply["id"].as_u64().unwrap()
    };
    let op = |v: Value| Command::Admin(serde_json::from_value(v).unwrap());
    let item = |s: &Sim, id: u64| s.w.improve.items.iter().find(|i| i.id == id).unwrap().clone();
    let plain = submit(&mut s, a, "A proposal accepted without a tier");
    let noted = submit(&mut s, b, "A proposal accepted as tier S");
    assert_eq!(af_core::improve::tier(&item(&s, plain)), None, "the captain's text is ignored");
    s.run(Actor::System, op(json!({"action":"improve","decision":"accept","id":plain})))
        .unwrap();
    s.run(
        Actor::System,
        op(json!({"action":"improve","decision":"accept","id":noted,"note":"Tier S: one guide line."})),
    )
    .unwrap();
    assert_eq!(af_core::improve::tier(&item(&s, plain)), None);
    assert_eq!(af_core::improve::tier(&item(&s, noted)), Some("S"));
    // An operator post's own text sets it.
    let posted = post(&mut s, "Operator item", "Tier M. A table in the world guide.");
    assert_eq!(af_core::improve::tier(&item(&s, posted)), Some("M"));
    s.replays();
}

/// The tier table is policy text, but no tier may promise more than one award can give.
#[test]
fn the_award_table_stays_within_the_award_cap() {
    let awards = af_core::improve::AWARDS;
    let numbers: Vec<i64> = awards
        .split(|c: char| !(c.is_ascii_digit() || c == ','))
        .map(|t| t.trim_matches(',').replace(',', ""))
        .filter(|t| !t.is_empty())
        .map(|t| t.parse().unwrap())
        .collect();
    let max = *numbers.iter().max().unwrap();
    assert!(max > 0 && max <= af_core::rules::Rules::default().improve_award_max, "{numbers:?}");
    assert!(awards.contains("touches only what the item asks for") && awards.contains("needs no failing test"));
    let view = af_core::improve::rules_view(&generate(&GenesisConfig::new(1, 40)));
    assert!(
        view["about"]
            .as_str()
            .unwrap()
            .ends_with("Source: https://github.com/mcpwars/mcpwars; how to patch: /improve.txt.")
    );
}

/// Prints the three briefing blocks a captain sees with the default rules (for the report).
#[test]
fn example_briefing_blocks() {
    let mut s = Sim::new(164);
    let [a, b] = ["Ada Mbeki", "Bram Ostrowski"].map(|n| s.join(n));
    s.document(a, "Lowmoor");
    s.document(b, "Pell");
    improve_on(&mut s);
    post(&mut s, "Name the toll owner in refusals", "Tier S. One line in the refusal text.");
    post(&mut s, "Document trade_loop stop reasons", "Tier M. A table in the world guide.");
    let full = briefing(&s, a, false, None);
    let example = json!({
        "improve": full["improve"],
        "chronicle": full["chronicle"],
        "comms.languages": full["comms"]["languages"],
    });
    println!("EXAMPLE {}", serde_json::to_string_pretty(&example).unwrap());
    let compact = briefing(&s, a, true, None);
    println!(
        "COMPACT {}",
        serde_json::to_string(
            &json!({"improve": compact["improve"], "chronicle": compact["chronicle"], "comms.languages": compact["comms"]["languages"]})
        )
        .unwrap()
    );
}

/// A 1.58-shaped world (no society rules, ledger, stat or award credits) adopts with the rules at
/// their defaults, its awards with 0 credits, and the same state hash as a native world with
/// nothing paid yet.
#[test]
fn a_1_58_world_adopts_with_default_rewards() {
    let mut s = Sim::new(165);
    let a = s.join("Ada Mbeki");
    s.document(a, "Lowmoor");
    // An award on the ledger (awards exist since 1.55, without credits): made at a rate of 0 so
    // the native world paid nothing, then the rate is back at its default.
    improve_on(&mut s);
    s.rules(json!({"improve_credits_per_influence": 0}));
    let item = post(&mut s, "Fix", "Tier S. A typo.");
    let op = |v: Value| Command::Admin(serde_json::from_value(v).unwrap());
    s.run(
        Actor::System,
        op(json!({"action":"improve","decision":"ship","id":item,"commit":"abcdef1"})),
    )
    .unwrap();
    s.run(
        Actor::System,
        op(json!({"action":"improve","decision":"award","id":item,"player":a,"amount":100})),
    )
    .unwrap();
    s.rules(json!({"improve_credits_per_influence": af_core::rules::Rules::default().improve_credits_per_influence}));
    assert_eq!(s.w.improve.awards.len(), 1);
    let mut v = serde_json::to_value(&s.w).unwrap();
    for award in v["improve"]["awards"].as_array_mut().unwrap() {
        assert!(award.as_object_mut().unwrap().remove("credits").is_some());
    }
    let rules = v["rules"].as_object_mut().unwrap();
    for k in [
        "chronicle_canon_credits",
        "chronicle_credit_credits",
        "chronicle_credits_per_day",
        "improve_credits_per_influence",
        "language_adopt_credits",
        "language_credits_per_day",
    ] {
        assert!(rules.remove(k).is_some(), "{k}");
    }
    for p in v["players"].as_object_mut().unwrap().values_mut() {
        assert!(p.get("society").is_none(), "an empty ledger is not serialized");
        p["stats"].as_object_mut().unwrap().remove("society_rewards");
        p["career"].as_object_mut().unwrap().remove("society_rewards");
    }
    let adopted: World = serde_json::from_value(v).unwrap();
    assert_eq!(adopted.improve.awards[0].credits, 0, "a 1.58 award reads as paying no credits");
    let d = af_core::rules::Rules::default();
    assert_eq!(adopted.rules.chronicle_canon_credits, d.chronicle_canon_credits);
    assert_eq!(adopted.rules.improve_credits_per_influence, d.improve_credits_per_influence);
    assert_eq!(adopted.rules.language_credits_per_day, d.language_credits_per_day);
    assert_eq!(adopted.state_hash(), s.w.state_hash());
}
