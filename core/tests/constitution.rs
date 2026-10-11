//! The captains' constitution: proposals, votes, ratification, the carry vote and moderation.

use af_core::World;
use af_core::cmd::*;
use af_core::constitution::{ChangeKind, ConstitutionAction, ConstitutionArgs, Proposal, ProposalKind, ProposalStatus};
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::model::*;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

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
    fn tick(&mut self) {
        self.run(Actor::System, Command::Tick).unwrap();
    }
    fn ticks(&mut self, n: u64) {
        for _ in 0..n {
            self.tick();
        }
    }
    fn admin(&mut self, a: AdminArgs) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(Actor::System, Command::Admin(a))
    }
    fn rules(&mut self, patch: serde_json::Value) {
        self.admin(AdminArgs::SetRules { patch }).unwrap();
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
    fn c(&mut self, pid: PlayerId, a: ConstitutionArgs) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(Actor::Player(pid), Command::Constitution(a))
    }
    fn propose(&mut self, pid: PlayerId, kind: ChangeKind, article: Option<u64>, text: &str) -> Result<u64, af_core::Refusal> {
        let out = self.c(
            pid,
            ConstitutionArgs {
                action: ConstitutionAction::Propose,
                kind: Some(kind),
                text: (!text.is_empty()).then(|| text.to_string()),
                article,
                proposal: None,
                vote: None,
            },
        )?;
        Ok(out.reply["proposal"]["id"].as_u64().unwrap())
    }
    fn vote(&mut self, pid: PlayerId, proposal: u64, yes: bool) -> Result<af_core::Outcome, af_core::Refusal> {
        self.c(
            pid,
            ConstitutionArgs {
                action: ConstitutionAction::Vote,
                kind: None,
                text: None,
                article: None,
                proposal: Some(proposal),
                vote: Some(yes),
            },
        )
    }
    fn proposal(&self, id: u64) -> &Proposal {
        self.w.constitution.proposals.iter().find(|p| p.id == id).unwrap()
    }
    fn replays(&self) {
        let mut r = generate(&self.cfg);
        for e in &self.journal {
            let _ = apply(&mut r, e);
        }
        assert_eq!(r.state_hash(), self.w.state_hash(), "replay diverged");
    }
}

fn three() -> (Sim, PlayerId, PlayerId, PlayerId) {
    let mut s = Sim::new(41);
    let a = s.join("Ada");
    let b = s.join("Bram");
    let c = s.join("Cyd");
    (s, a, b, c)
}

#[test]
fn propose_vote_ratify_amend_repeal_and_replay() {
    let (mut s, a, b, c) = three();
    let ap = s.w.players[&a].ap;
    let id = s.propose(a, ChangeKind::Add, None, "No raids on captains in escape pods.").unwrap();
    let p = s.proposal(id);
    assert_eq!(p.status, ProposalStatus::Open);
    assert_eq!(p.electorate, BTreeSet::from([a, b, c]), "the electorate is snapshotted at open");
    assert_eq!(p.votes, BTreeMap::from([(a, true)]), "the proposal counts as the proposer's yes");
    assert_eq!(p.threshold(), 2, "a strict majority of three");
    assert_eq!(p.closes_tick, s.w.tick + 720);
    assert_eq!(s.w.players[&a].ap, ap, "proposing costs no AP");
    // The rest of the electorate is told once, in the inbox.
    for x in [b, c] {
        assert_eq!(s.w.players[&x].inbox.iter().filter(|n| n.kind == "constitution").count(), 1);
    }
    assert!(s.w.players[&a].inbox.iter().all(|n| n.kind != "constitution"));
    // The briefing asks B for its vote.
    let br = af_core::query::briefing(&s.w, b).unwrap();
    assert_eq!(br["constitution"]["awaiting_your_vote"][0]["proposal"], json!(id));
    assert!(
        af_core::query::briefing(&s.w, a).unwrap()["constitution"]["awaiting_your_vote"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A no vote, changed to yes: ratified at once.
    let out = s.vote(b, id, false).unwrap();
    assert!(out.summary.contains("1 yes, 1 no"), "{}", out.summary);
    let out = s.vote(b, id, true).unwrap();
    assert!(out.summary.contains("ratified"), "{}", out.summary);
    assert_eq!(s.proposal(id).status, ProposalStatus::Ratified);
    assert_eq!(s.w.constitution.articles.len(), 1);
    let art = s.w.constitution.articles[0].clone();
    assert_eq!((art.id, art.proposed_by.as_str(), art.season), (1, "Ada", 1));
    assert!(s.w.news.iter().any(|n| n.headline.contains("ratified article 1")));
    assert_eq!(s.vote(c, id, true).unwrap_err().code, "proposal_closed");
    // Amend it (B), then repeal it (C).
    let am = s.propose(b, ChangeKind::Amend, Some(1), "No raids on escape pods, ever.").unwrap();
    s.vote(c, am, true).unwrap();
    assert_eq!(s.w.constitution.articles[0].text, "No raids on escape pods, ever.");
    assert!(s.w.constitution.articles[0].amended_tick.is_some());
    let rp = s.propose(c, ChangeKind::Repeal, Some(1), "").unwrap();
    s.vote(a, rp, true).unwrap();
    assert!(s.w.constitution.articles.is_empty());
    assert_eq!(s.propose(a, ChangeKind::Repeal, Some(1), "").unwrap_err().code, "unknown_article");
    assert_eq!(s.propose(a, ChangeKind::Add, None, "Again.").unwrap_err().code, "cooldown");
    // The view agrees with the refusal (feedback #38): no, and when.
    let v = af_core::constitution::view(&s.w, a).unwrap();
    assert_eq!(v["you_may_propose"], false);
    assert!(
        v["next_proposal_tick"].as_u64().is_some_and(|t| t > s.w.tick),
        "{}",
        v["next_proposal_tick"]
    );
    s.replays();
}

#[test]
fn an_article_removed_meanwhile_expires_the_change() {
    let (mut s, a, b, c) = three();
    let id = s.propose(a, ChangeKind::Add, None, "Trade fairly.").unwrap();
    s.vote(b, id, true).unwrap();
    let r1 = s.propose(b, ChangeKind::Repeal, Some(1), "").unwrap();
    let r2 = s.propose(c, ChangeKind::Amend, Some(1), "Trade very fairly.").unwrap();
    s.vote(a, r1, true).unwrap();
    assert!(s.w.constitution.articles.is_empty());
    s.vote(a, r2, true).unwrap();
    assert_eq!(s.proposal(r2).status, ProposalStatus::Expired);
    assert!(s.w.news.iter().any(|n| n.headline.contains("expired")));
    s.replays();
}

#[test]
fn proposals_fail_at_close_without_a_majority() {
    let (mut s, a, b, _c) = three();
    s.rules(json!({"constitution_vote_ticks": 5}));
    let id = s.propose(a, ChangeKind::Add, None, "Pay tolls without complaint.").unwrap();
    s.vote(b, id, false).unwrap();
    s.ticks(4);
    assert_eq!(s.proposal(id).status, ProposalStatus::Open);
    s.tick();
    let p = s.proposal(id);
    assert_eq!((p.status, p.closed_tick), (ProposalStatus::Rejected, Some(s.w.tick)));
    assert!(s.w.constitution.articles.is_empty());
    assert!(s.w.news.iter().any(|n| n.headline.contains("failed: 1 yes, 1 no")));
    s.replays();
}

#[test]
fn electorate_is_recent_captains_fixed_at_open() {
    let (mut s, a, b, c) = three();
    s.rules(json!({"constitution_electorate_ticks": 10}));
    s.ticks(20);
    // Nobody has acted for 20 ticks: no electorate, so nobody may propose.
    assert_eq!(s.propose(a, ChangeKind::Add, None, "x").unwrap_err().code, "not_in_electorate");
    // Recent action puts A and B back in; C stays out.
    let t = s.w.tick;
    s.w.players.get_mut(&a).unwrap().last_action_tick = t;
    let before = s.w.state_hash();
    assert_eq!(
        s.propose(a, ChangeKind::Add, None, "x").unwrap_err().code,
        "electorate_too_small",
        "one active captain is not enough"
    );
    assert_eq!(s.w.state_hash(), before, "a refusal changes nothing");
    s.w.players.get_mut(&b).unwrap().last_action_tick = t;
    let id = s.propose(a, ChangeKind::Add, None, "Share Skarn sightings.").unwrap();
    assert_eq!(s.proposal(id).electorate, BTreeSet::from([a, b]));
    assert_eq!(s.proposal(id).threshold(), 2);
    assert_eq!(s.vote(c, id, true).unwrap_err().code, "not_in_electorate");
    // A captain who joins later is not in this proposal's electorate either.
    let d = s.join("Dex");
    assert_eq!(s.vote(d, id, true).unwrap_err().code, "not_in_electorate");
    s.vote(b, id, true).unwrap();
    assert_eq!(s.w.constitution.articles.len(), 1);
}

#[test]
fn thresholds_are_a_majority_and_two_thirds_for_carry() {
    let p = |n: u32, change: ProposalKind| Proposal {
        id: 1,
        change,
        proposer: 1,
        proposer_name: "x".into(),
        season: 1,
        opened_tick: 0,
        closes_tick: 1,
        electorate: (1..=n).collect(),
        votes: BTreeMap::new(),
        status: ProposalStatus::Open,
        closed_tick: None,
        ballots: vec![],
        by_operator: false,
        comments: vec![],
        struck: None,
    };
    let add = || ProposalKind::Add { text: "t".into() };
    for (n, majority, carry) in [(2, 2, 2), (3, 2, 2), (4, 3, 3), (5, 3, 4), (6, 4, 4), (9, 5, 6), (10, 6, 7)] {
        assert_eq!(p(n, add()).threshold(), majority, "majority of {n}");
        assert_eq!(p(n, ProposalKind::Carry).threshold(), carry, "two thirds of {n}");
    }
}

#[test]
fn limits_cooldown_max_open_max_articles_and_length() {
    let (mut s, a, b, c) = three();
    let before = s.w.state_hash();
    let long = "y".repeat(2_001);
    let r = s.propose(a, ChangeKind::Add, None, &long).unwrap_err();
    assert_eq!(r.code, "too_long");
    assert!(
        r.message.contains("2001") && r.message.contains("Nothing was stored"),
        "{}",
        r.message
    );
    assert_eq!(s.propose(a, ChangeKind::Add, None, " \n ").unwrap_err().code, "invalid_args");
    assert_eq!(s.propose(a, ChangeKind::Amend, Some(9), "x").unwrap_err().code, "unknown_article");
    assert_eq!(
        s.c(
            a,
            ConstitutionArgs {
                action: ConstitutionAction::Propose,
                kind: None,
                text: Some("x".into()),
                article: None,
                proposal: None,
                vote: None
            }
        )
        .unwrap_err()
        .code,
        "invalid_args"
    );
    assert_eq!(s.vote(a, 99, true).unwrap_err().code, "unknown_proposal");
    assert_eq!(s.w.state_hash(), before, "refusals change nothing");
    // Exactly the maximum is accepted, whitespace and Unicode kept.
    let max: String = format!(" {}\n", "é".repeat(1_998));
    let id = s.propose(a, ChangeKind::Add, None, &max).unwrap();
    assert!(matches!(&s.proposal(id).change, ProposalKind::Add { text } if *text == max));
    // Cooldown: one proposal per captain per window.
    assert_eq!(s.propose(a, ChangeKind::Add, None, "again").unwrap_err().code, "cooldown");
    // Max open.
    s.rules(json!({"constitution_max_open": 2}));
    s.propose(b, ChangeKind::Add, None, "second").unwrap();
    assert_eq!(s.propose(c, ChangeKind::Add, None, "third").unwrap_err().code, "too_many_proposals");
    // Max articles: a full constitution refuses another add.
    s.rules(json!({"constitution_max_articles": 0}));
    s.rules(json!({"constitution_max_open": 5}));
    assert_eq!(s.propose(c, ChangeKind::Add, None, "fourth").unwrap_err().code, "constitution_full");
    // Muted captains cannot propose.
    s.rules(json!({"constitution_max_articles": 30}));
    s.admin(AdminArgs::Mute { player: c, ticks: 50 }).unwrap();
    assert_eq!(s.propose(c, ChangeKind::Add, None, "fifth").unwrap_err().code, "muted");
    s.replays();
}

/// Set up a ratified article, then run the season out with `yes` of three captains voting to carry.
fn season_with_carry(yes: usize) -> Sim {
    let (mut s, a, b, c) = three();
    s.rules(json!({"constitution_carry_ticks": 5}));
    let id = s.propose(a, ChangeKind::Add, None, "Captains keep their word.").unwrap();
    s.vote(b, id, true).unwrap();
    let ends = s.w.tick + 10;
    s.admin(AdminArgs::SetSeasonEnd { ends_tick: ends }).unwrap();
    s.ticks(4);
    assert!(
        !s.w.constitution.proposals.iter().any(|p| p.change == ProposalKind::Carry),
        "the carry vote opens only in the last window"
    );
    s.tick();
    let carry =
        s.w.constitution
            .proposals
            .iter()
            .find(|p| p.change == ProposalKind::Carry)
            .unwrap()
            .clone();
    assert_eq!(
        (carry.closes_tick, carry.threshold(), carry.status),
        (ends, 2, ProposalStatus::Open)
    );
    assert_eq!(s.w.players[&c].inbox.iter().filter(|n| n.kind == "constitution").count(), 2);
    for &pid in [a, b, c].iter().take(yes) {
        s.vote(pid, carry.id, true).unwrap();
    }
    assert_eq!(
        s.proposal(carry.id).status,
        ProposalStatus::Open,
        "a carry vote is decided at the season's end"
    );
    while !s.w.season.finished {
        s.tick();
    }
    s.admin(AdminArgs::NewSeason).unwrap();
    assert_eq!(s.w.season.number, 2);
    s
}

#[test]
fn the_constitution_carries_on_two_thirds() {
    let s = season_with_carry(2);
    assert_eq!(s.w.constitution.articles.len(), 1);
    assert_eq!(s.w.constitution.articles[0].text, "Captains keep their word.");
    assert!(s.w.news.iter().any(|n| n.headline.contains("carries into the new galaxy")));
    assert!(
        s.w.constitution.proposals.iter().all(|p| p.status != ProposalStatus::Open),
        "proposals never carry"
    );
    s.replays();
}

#[test]
fn the_constitution_lapses_below_two_thirds() {
    let s = season_with_carry(1);
    assert!(s.w.constitution.articles.is_empty());
    assert!(s.w.news.iter().any(|n| n.headline.contains("lapsed with season 1")));
    s.replays();
}

#[test]
fn fresh_start_wipes_and_admin_strikes() {
    let (mut s, a, b, c) = three();
    let id = s.propose(a, ChangeKind::Add, None, "Article one.").unwrap();
    s.vote(b, id, true).unwrap();
    let open = s.propose(c, ChangeKind::Add, None, "Something abusive.").unwrap();
    // Strike: exactly one target.
    assert_eq!(
        s.admin(AdminArgs::ConstitutionStrike {
            article: Some(1),
            proposal: Some(open),
            reason: None
        })
        .unwrap_err()
        .code,
        "invalid_args"
    );
    assert_eq!(
        s.admin(AdminArgs::ConstitutionStrike {
            article: Some(7),
            proposal: None,
            reason: None
        })
        .unwrap_err()
        .code,
        "unknown_article"
    );
    s.admin(AdminArgs::ConstitutionStrike {
        article: None,
        proposal: Some(open),
        reason: Some("abuse".into()),
    })
    .unwrap();
    let p = s.proposal(open);
    assert_eq!(p.status, ProposalStatus::Cancelled);
    assert!(matches!(&p.change, ProposalKind::Add { text } if text == "[struck by the operator]"));
    assert_eq!(s.vote(a, open, true).unwrap_err().code, "proposal_closed");
    s.admin(AdminArgs::ConstitutionStrike {
        article: Some(1),
        proposal: None,
        reason: None,
    })
    .unwrap();
    assert!(s.w.constitution.articles.is_empty());
    assert!(s.w.news.iter().any(|n| n.headline.contains("struck article 1")));
    // A fresh start wipes it all; ids keep counting.
    let id = s.propose(b, ChangeKind::Add, None, "Article again.").unwrap();
    s.vote(a, id, true).unwrap();
    let next = s.w.constitution.next_id;
    s.admin(AdminArgs::FreshStart).unwrap();
    assert!(s.w.constitution.articles.is_empty() && s.w.constitution.proposals.is_empty());
    assert_eq!(s.w.constitution.next_id, next);
    s.replays();
}

#[test]
fn old_snapshots_and_rules_load_without_a_constitution() {
    let (mut s, a, b, _c) = three();
    let id = s.propose(a, ChangeKind::Add, None, "Kept.").unwrap();
    s.vote(b, id, true).unwrap();
    let mut v = serde_json::to_value(&s.w).unwrap();
    let o = v.as_object_mut().unwrap();
    o.remove("constitution");
    let rules = o.get_mut("rules").unwrap().as_object_mut().unwrap();
    for k in [
        "constitution_vote_ticks",
        "constitution_electorate_ticks",
        "constitution_carry_ticks",
        "constitution_cooldown_ticks",
        "constitution_max_open",
        "constitution_max_articles",
    ] {
        assert!(rules.remove(k).is_some(), "{k}");
    }
    let old: World = serde_json::from_value(v).unwrap();
    assert!(old.constitution.articles.is_empty() && old.constitution.proposals.is_empty());
    assert_eq!(
        (
            old.rules.constitution_vote_ticks,
            old.rules.constitution_electorate_ticks,
            old.rules.constitution_carry_ticks,
            old.rules.constitution_cooldown_ticks,
            old.rules.constitution_max_open,
            old.rules.constitution_max_articles
        ),
        (720, 2_880, 1_440, 1_440, 5, 30)
    );
    // And a world with one round-trips exactly.
    let back: World = serde_json::from_value(serde_json::to_value(&s.w).unwrap()).unwrap();
    assert_eq!(back.state_hash(), s.w.state_hash());
}

#[test]
fn views_show_tallies_and_the_public_never_sees_votes() {
    let (mut s, a, b, _c) = three();
    let id = s.propose(a, ChangeKind::Add, None, "Meet in sector 12 to sign.").unwrap();
    let v = af_core::constitution::view(&s.w, b).unwrap();
    let p = &v["open_proposals"][0];
    assert_eq!(
        (p["yes"].clone(), p["no"].clone(), p["threshold"].clone()),
        (json!(1), json!(0), json!(2))
    );
    assert_eq!(p["can_vote"], json!(true));
    assert_eq!(p["your_vote"], json!(null));
    assert_eq!(p["text"]["untrusted_player_text"], json!("Meet in sector 12 to sign."));
    assert!(
        v["rules"]["ratify"].as_str().unwrap().contains("720 ticks"),
        "live numbers in the reply"
    );
    s.vote(b, id, true).unwrap();
    let public = af_core::public::state(&s.w)["constitution"].clone();
    assert_eq!(public["articles"][0]["text"], json!("Meet to sign."), "scrubbed of sector numbers");
    let text = public.to_string();
    assert!(!text.contains("votes") && !text.contains("electorate_ids"));
    // The operator sees every vote.
    let sys = af_core::spectator::state(&s.w)["constitution"].clone();
    assert_eq!(sys["proposals"][0]["votes"].as_array().unwrap().len(), 2);
    // View through the command path is read-only and free.
    let ap = s.w.players[&a].ap;
    let out = s
        .c(
            a,
            ConstitutionArgs {
                action: ConstitutionAction::View,
                kind: None,
                text: None,
                article: None,
                proposal: None,
                vote: None,
            },
        )
        .unwrap();
    assert_eq!(out.reply["articles"].as_array().unwrap().len(), 1);
    assert_eq!(s.w.players[&a].ap, ap);
}

// ---------- engine 1.33: the record ----------

fn view_args(article: Option<u64>, proposal: Option<u64>) -> ConstitutionArgs {
    ConstitutionArgs {
        action: ConstitutionAction::View,
        kind: None,
        text: None,
        article,
        proposal,
        vote: None,
    }
}

#[test]
fn every_version_repeal_and_season_is_kept_on_the_record() {
    let (mut s, a, b, c) = three();
    let id = s.propose(a, ChangeKind::Add, None, "Captains keep their word.").unwrap();
    s.vote(b, id, true).unwrap();
    let am = s
        .propose(b, ChangeKind::Amend, Some(1), "Captains keep their word, or pay.")
        .unwrap();
    s.vote(c, am, false).unwrap();
    s.vote(a, am, true).unwrap();
    let h = &s.w.constitution.articles[0].history;
    assert_eq!(h.len(), 2, "add and amend are both versions");
    assert_eq!(
        (h[0].proposal, h[0].change.as_str(), h[0].yes, h[0].no, h[0].electorate),
        (id, "add", 2, 0, 3)
    );
    assert_eq!((h[1].proposal, h[1].change.as_str(), h[1].yes, h[1].no), (am, "amend", 2, 1));
    assert_eq!(h[1].text, "Captains keep their word, or pay.");
    // A captain reads one article's history, or one proposal.
    let v = af_core::constitution::lookup(&s.w, c, &view_args(Some(1), None)).unwrap();
    assert_eq!(v["status"], "in force");
    assert_eq!(v["article"]["history"].as_array().unwrap().len(), 2);
    let v = af_core::constitution::lookup(&s.w, c, &view_args(None, Some(am))).unwrap();
    assert_eq!(
        (v["proposal"]["yes"].clone(), v["proposal"]["your_vote"].clone()),
        (json!(2), json!(false))
    );
    // A repeal moves the article, with its history, to the record of removed articles.
    s.ticks(1440);
    let rp = s.propose(c, ChangeKind::Repeal, Some(1), "").unwrap();
    s.vote(a, rp, true).unwrap();
    assert!(s.w.constitution.articles.is_empty());
    let r = &s.w.constitution.removed[0];
    assert_eq!(
        (r.article.id, r.proposal, r.how.as_str(), r.article.history.len()),
        (1, Some(rp), "repealed", 2)
    );
    let v = af_core::constitution::lookup(&s.w, a, &view_args(Some(1), None)).unwrap();
    assert_eq!(v["status"], "repealed");
    assert_eq!(
        af_core::constitution::lookup(&s.w, a, &view_args(Some(9), None)).unwrap_err().code,
        "unknown_article"
    );
    // The operator sees every vote and who has not voted.
    let sv = af_core::constitution::sysop_lookup(&s.w, Some(am), None, None).unwrap();
    assert_eq!(sv["votes"].as_array().unwrap().len(), 3);
    assert!(sv["not_voted"].as_array().unwrap().is_empty());
    let sv = af_core::constitution::sysop_lookup(&s.w, None, Some(1), None).unwrap();
    assert_eq!(sv["records"][0]["where"], "repealed");
    s.replays();
}

#[test]
fn each_season_is_archived_whether_or_not_it_carried() {
    let s = season_with_carry(2);
    let ar = &s.w.constitution.archive;
    assert_eq!((ar.len(), ar[0].season, ar[0].carried, ar[0].articles.len()), (1, 1, true, 1));
    let s = season_with_carry(1);
    let ar = &s.w.constitution.archive;
    assert_eq!((ar[0].season, ar[0].carried), (1, false));
    assert_eq!(
        ar[0].articles[0].text, "Captains keep their word.",
        "a lapsed constitution stays on the record"
    );
    let public = af_core::constitution::public_view(&s.w);
    assert_eq!(public["archive"][0]["carried"], false);
    assert!(public["archive"][0]["articles"][0]["history"].is_array());
    s.replays();
}

#[test]
fn struck_text_leaves_the_record_too() {
    let (mut s, a, b, _c) = three();
    let id = s.propose(a, ChangeKind::Add, None, "Something the operator must remove.").unwrap();
    s.vote(b, id, true).unwrap();
    // Striking the proposal scrubs the version it ratified; the article stays until struck.
    s.admin(AdminArgs::ConstitutionStrike {
        article: None,
        proposal: Some(id),
        reason: None,
    })
    .unwrap();
    assert_eq!(s.w.constitution.articles[0].history[0].text, "[struck by the operator]");
    s.admin(AdminArgs::ConstitutionStrike {
        article: Some(1),
        proposal: None,
        reason: Some("abuse".into()),
    })
    .unwrap();
    let r = &s.w.constitution.removed[0];
    assert_eq!(
        (r.how.as_str(), r.proposal, r.article.text.as_str()),
        ("struck", None, "[struck by the operator]")
    );
    assert!(!serde_json::to_string(&s.w.constitution).unwrap().contains("must remove"));
    // Striking an article also blanks the proposals that wrote it, so no public list keeps the text.
    let id = s.propose(b, ChangeKind::Add, None, "Buy my ore at double price or else.").unwrap();
    s.vote(a, id, true).unwrap();
    let art = s.w.constitution.articles[0].id;
    s.admin(AdminArgs::ConstitutionStrike {
        article: Some(art),
        proposal: None,
        reason: None,
    })
    .unwrap();
    assert!(!serde_json::to_string(&s.w.constitution).unwrap().contains("double price"));
    assert!(!af_core::constitution::public_view(&s.w).to_string().contains("double price"));
    s.replays();
}

#[test]
fn constitutions_from_engine_1_32_load_without_the_record() {
    let old = json!({
        "articles": [{"id": 1, "text": "Old.", "proposed_by": "Ada", "ratified_tick": 3, "season": 1, "amended_tick": null}],
        "proposals": [], "next_id": 1, "next_article": 1
    });
    let c: af_core::constitution::Constitution = serde_json::from_value(old).unwrap();
    assert!(c.articles[0].history.is_empty() && c.removed.is_empty() && c.archive.is_empty());
}

// ---------- engine 1.37: every ballot, a permanent record, the operator takes part ----------

fn four() -> (Sim, PlayerId, PlayerId, PlayerId, PlayerId) {
    let (mut s, a, b, c) = three();
    let d = s.join("Dov");
    (s, a, b, c, d)
}

#[test]
fn a_changed_vote_keeps_every_ballot_and_the_tally_uses_the_latest() {
    let (mut s, a, b, c, d) = four();
    let id = s.propose(a, ChangeKind::Add, None, "Trade fairly.").unwrap();
    let t0 = s.w.tick;
    s.vote(b, id, true).unwrap();
    s.ticks(3);
    let out = s.vote(b, id, false).unwrap();
    assert!(out.summary.starts_with("Changed your vote to no"), "{}", out.summary);
    let p = s.proposal(id);
    // The tally is each captain's latest vote; thresholds are unchanged.
    assert_eq!(p.votes, BTreeMap::from([(a, true), (b, false)]));
    assert_eq!((p.threshold(), p.status), (3, ProposalStatus::Open));
    // Every ballot is kept in order: the proposer's own yes first, then both of B's.
    let got: Vec<(PlayerId, &str, bool, u64, bool)> = p
        .ballots
        .iter()
        .map(|x| (x.player_id, x.name.as_str(), x.yes, x.tick, x.changed))
        .collect();
    assert_eq!(
        got,
        vec![
            (a, "Ada", true, t0, false),
            (b, "Bram", true, t0, false),
            (b, "Bram", false, t0 + 3, true)
        ]
    );
    assert!(p.ballots[2].at_ms > p.ballots[1].at_ms, "journal time is recorded");
    // The operator sees the ballots, the changed votes and who never voted.
    let sv = af_core::constitution::sysop_lookup(&s.w, Some(id), None, None).unwrap();
    assert_eq!(sv["ballots"].as_array().unwrap().len(), 3);
    assert_eq!(sv["ballots"][2]["changed"], json!(true));
    assert_eq!(sv["changed_votes"], json!(1));
    assert_eq!(sv["not_voted"], json!(["Cyd", "Dov"]));
    assert_eq!(sv["not_voted_ids"], json!([c, d]));
    // Captains and the public never see ballots.
    let public = af_core::constitution::public_view(&s.w).to_string();
    assert!(!public.contains("ballots") && !public.contains("Bram"), "{public}");
    let mine = af_core::constitution::lookup(&s.w, c, &view_args(None, Some(id)))
        .unwrap()
        .to_string();
    assert!(!mine.contains("ballots"), "{mine}");
    // Flipping is capped per captain and proposal; the latest vote stands.
    for i in 0..(af_core::constitution::BALLOTS_PER_VOTER - 2) {
        s.vote(b, id, i % 2 == 0).unwrap();
    }
    assert_eq!(s.vote(b, id, true).unwrap_err().code, "too_many_votes");
    assert_eq!(s.proposal(id).ballots.len(), af_core::constitution::BALLOTS_PER_VOTER + 1);
    s.replays();
}

#[test]
fn closed_proposals_beyond_the_old_cap_are_never_dropped() {
    let (mut s, a, _b, _c) = three();
    s.rules(json!({"constitution_cooldown_ticks": 0}));
    for i in 0..505 {
        let id = s.propose(a, ChangeKind::Add, None, &format!("Draft {i}.")).unwrap();
        s.admin(AdminArgs::ConstitutionStrike {
            article: None,
            proposal: Some(id),
            reason: Some(format!("draft {i}")),
        })
        .unwrap();
    }
    let ps = &s.w.constitution.proposals;
    assert_eq!(ps.len(), 505, "the old cap was 500 closed proposals");
    assert_eq!(ps[0].id, 1);
    assert!(ps.iter().all(|p| p.status == ProposalStatus::Cancelled && p.ballots.len() == 1));
    assert_eq!(ps[0].struck.as_ref().unwrap().reason.as_deref(), Some("draft 0"));
    s.replays();
}

#[test]
fn the_big_bang_archives_the_whole_constitution_with_ballots() {
    let s = season_with_carry(2);
    let w = &s.w;
    assert!(w.constitution.proposals.is_empty(), "the season's proposals moved to the record");
    assert_eq!(w.archive.constitutions.len(), 1);
    let r = &w.archive.constitutions[0];
    let season = w.archive.seasons.iter().find(|x| x.reason == "season_end").unwrap();
    assert_eq!(
        r.season_key,
        af_core::history::season_key(season),
        "keyed like the season's results"
    );
    assert_eq!(
        (r.season, r.reason.as_str(), r.carried, r.articles.len()),
        (1, "season_end", true, 1)
    );
    assert_eq!(r.proposals.len(), 2, "the article's proposal and the carry vote");
    let add = &r.proposals[0];
    assert_eq!((add.status, add.ballots.len()), (ProposalStatus::Ratified, 2));
    let carry = &r.proposals[1];
    assert_eq!((carry.change.clone(), carry.ballots.len()), (ProposalKind::Carry, 2));
    // Captains can still read an archived proposal; the operator reads the season by number or key.
    let v = af_core::constitution::lookup(w, add.proposer, &view_args(None, Some(add.id))).unwrap();
    assert_eq!(v["proposal"]["status"], "ratified");
    let by_number = af_core::constitution::sysop_lookup(w, None, None, Some("1")).unwrap();
    let by_key = af_core::constitution::sysop_lookup(w, None, None, Some(&r.season_key)).unwrap();
    assert_eq!(by_number, by_key);
    assert_eq!(by_key["proposals"][1]["ballots"].as_array().unwrap().len(), 2);
    let one = af_core::constitution::sysop_lookup(w, Some(add.id), None, None).unwrap();
    assert_eq!(one["archived_in"], json!(r.season_key));
    assert_eq!(
        af_core::constitution::sysop_lookup(w, None, None, Some("9")).unwrap_err().code,
        "unknown_season"
    );
    let live = af_core::constitution::sysop_view(w);
    assert_eq!(live["seasons"][0]["ballots"], json!(4));
    assert!(af_core::constitution::season_record(w, &r.season_key).is_some());
    s.replays();
}

#[test]
fn a_fresh_start_archives_before_wiping_and_the_record_survives() {
    let (mut s, a, b, c) = three();
    let id = s.propose(a, ChangeKind::Add, None, "Article one.").unwrap();
    s.vote(b, id, false).unwrap();
    s.vote(b, id, true).unwrap();
    let open = s.propose(c, ChangeKind::Add, None, "Still open.").unwrap();
    s.admin(AdminArgs::ConstitutionNotice {
        text: "Read article one.".into(),
        pin: false,
    })
    .unwrap();
    s.admin(AdminArgs::FreshStart).unwrap();
    assert!(s.w.constitution.proposals.is_empty() && s.w.constitution.articles.is_empty());
    let r = s.w.archive.constitutions.last().unwrap().clone();
    let fresh = s.w.archive.seasons.last().unwrap();
    assert_eq!(r.season_key, af_core::history::season_key(fresh));
    assert_eq!((r.reason.as_str(), r.epoch, r.carried), ("fresh_start", 0, false));
    assert_eq!(r.articles.len(), 1);
    let p = r.proposals.iter().find(|p| p.id == id).unwrap();
    assert_eq!(
        p.ballots.iter().map(|x| (x.yes, x.changed)).collect::<Vec<_>>(),
        vec![(true, false), (false, false), (true, true)]
    );
    assert_eq!(r.proposals.iter().find(|p| p.id == open).unwrap().status, ProposalStatus::Open);
    assert_eq!(r.notices[0].text, "Read article one.");
    // A second fresh start and a season change add records; none removes one.
    let a2 = s.propose(a, ChangeKind::Add, None, "New epoch.").unwrap();
    s.vote(b, a2, true).unwrap();
    s.admin(AdminArgs::NewSeason).unwrap();
    s.admin(AdminArgs::FreshStart).unwrap();
    let keys: Vec<(u32, String)> = s.w.archive.constitutions.iter().map(|r| (r.epoch, r.reason.clone())).collect();
    assert_eq!(keys, vec![(0, "fresh_start".into()), (1, "season_end".into())]);
    assert_eq!(s.w.archive.constitutions[0], r);
    // Striking an archived proposal scrubs its text everywhere and keeps its ballots and the reason.
    s.admin(AdminArgs::ConstitutionStrike {
        article: None,
        proposal: Some(id),
        reason: Some("abusive".into()),
    })
    .unwrap();
    let p = s.w.archive.constitutions[0].proposals.iter().find(|p| p.id == id).unwrap();
    assert_eq!(
        (p.ballots.len(), p.struck.as_ref().unwrap().reason.as_deref()),
        (3, Some("abusive"))
    );
    assert!(matches!(&p.change, ProposalKind::Add { text } if text == "[struck by the operator]"));
    // An archived article (no longer in force anywhere) can be struck from the record too.
    let art = s.w.archive.constitutions[0].articles[0].id;
    assert!(s.w.constitution.articles.iter().all(|a| a.id != art));
    s.admin(AdminArgs::ConstitutionStrike {
        article: Some(art),
        proposal: None,
        reason: None,
    })
    .unwrap();
    assert!(!serde_json::to_string(&s.w.archive).unwrap().contains("Article one."));
    assert_eq!(
        s.admin(AdminArgs::ConstitutionStrike {
            article: Some(999),
            proposal: None,
            reason: None
        })
        .unwrap_err()
        .code,
        "unknown_article"
    );
    s.replays();
}

#[test]
fn the_operator_proposes_and_the_captains_ratify_it() {
    let (mut s, a, b, c) = three();
    let ap = s.w.players[&a].ap;
    let out = s
        .admin(AdminArgs::ConstitutionPropose {
            kind: ChangeKind::Add,
            text: Some("Captains answer distress calls.".into()),
            article: None,
            note: Some("Proposed after several pods were left stranded.".into()),
        })
        .unwrap();
    let id = out.reply["proposal"]["id"].as_u64().unwrap();
    let p = s.proposal(id).clone();
    assert_eq!((p.proposer, p.proposer_name.as_str(), p.by_operator), (SYSOP, "operator", true));
    assert!(p.votes.is_empty() && p.ballots.is_empty(), "the operator does not vote");
    assert_eq!((p.electorate.len(), p.threshold()), (3, 2));
    assert_eq!(p.comments[0].text, "Proposed after several pods were left stranded.");
    // Every captain is told: an announcement in the briefing, the inbox for its electorate, radio.
    for x in [a, b, c] {
        let br = af_core::query::briefing(&s.w, x).unwrap();
        let anns = br["server"]["announcements"].to_string();
        assert!(anns.contains(&format!("Operator proposal #{id}")), "{anns}");
        assert_eq!(br["constitution"]["awaiting_your_vote"][0]["by_operator"], json!(true));
        assert_eq!(s.w.players[&x].inbox.iter().filter(|n| n.kind == "constitution").count(), 1);
    }
    assert!(s.w.radio.iter().any(|m| {
        m.text
            .contains(&format!("OPERATOR proposes to add an article (constitution proposal #{id})"))
    }));
    let v = af_core::constitution::view(&s.w, a).unwrap();
    assert_eq!(v["open_proposals"][0]["by_operator"], json!(true));
    assert_eq!(
        v["open_proposals"][0]["text"],
        json!("Captains answer distress calls."),
        "operator text is trusted"
    );
    // Captains vote it through.
    s.vote(a, id, true).unwrap();
    s.vote(b, id, true).unwrap();
    assert_eq!(s.proposal(id).status, ProposalStatus::Ratified);
    let art = s.w.constitution.articles[0].clone();
    assert_eq!((art.proposed_by.as_str(), art.by_operator), ("operator", true));
    assert!(s.w.news.iter().any(|n| n.headline.contains("proposed by the operator")));
    assert_eq!(s.w.players[&a].ap, ap);
    // Same limits as captains' proposals: text length; the cooldown does not bind the operator.
    let long = "x".repeat(af_core::constitution::TEXT_MAX + 1);
    let err = s
        .admin(AdminArgs::ConstitutionPropose {
            kind: ChangeKind::Add,
            text: Some(long),
            article: None,
            note: None,
        })
        .unwrap_err();
    assert_eq!(err.code, "too_long");
    s.admin(AdminArgs::ConstitutionPropose {
        kind: ChangeKind::Repeal,
        text: None,
        article: Some(art.id),
        note: None,
    })
    .unwrap();
    s.replays();
}

#[test]
fn operator_comments_and_notices_reach_captains_and_the_public() {
    let (mut s, a, b, c) = three();
    let id = s.propose(a, ChangeKind::Add, None, "Neutral ground.").unwrap();
    // Stored exactly: long Unicode text and whitespace survive; 2,000 characters is the limit.
    let text = format!("  Before you vote: is sector 12 meant?\n{}é", "ü".repeat(1_900));
    assert_eq!(text.chars().count(), 1_940);
    s.admin(AdminArgs::ConstitutionComment {
        proposal: id,
        text: text.clone(),
    })
    .unwrap();
    let too_long = "é".repeat(af_core::constitution::OPERATOR_TEXT_MAX + 1);
    assert_eq!(
        s.admin(AdminArgs::ConstitutionComment {
            proposal: id,
            text: too_long.clone()
        })
        .unwrap_err()
        .code,
        "too_long"
    );
    assert_eq!(
        s.admin(AdminArgs::ConstitutionComment {
            proposal: 99,
            text: "Hi.".into()
        })
        .unwrap_err()
        .code,
        "unknown_proposal"
    );
    assert_eq!(s.proposal(id).comments.len(), 1);
    assert_eq!(s.proposal(id).comments[0].text, text);
    assert!(s.w.radio.iter().any(|m| m.text == format!("OPERATOR on proposal #{id}: {text}")));
    // The captain's view and briefing show it; the public sees it scrubbed of sector numbers.
    let v = af_core::constitution::lookup(&s.w, b, &view_args(None, Some(id))).unwrap();
    assert_eq!(v["proposal"]["operator_comments"][0]["text"], json!(text));
    let br = af_core::query::briefing(&s.w, b).unwrap();
    assert_eq!(br["constitution"]["awaiting_your_vote"][0]["latest_operator_comment"], json!(text));
    let public = af_core::constitution::public_view(&s.w);
    let shown = public["open_proposals"][0]["operator_comments"][0]["text"].as_str().unwrap();
    assert!(shown.starts_with("  Before you vote: is a sector meant?"), "{shown}");
    // A pinned notice is an announcement; an unpinned one an operator message to every captain.
    assert_eq!(
        s.admin(AdminArgs::ConstitutionNotice {
            text: too_long,
            pin: false
        })
        .unwrap_err()
        .code,
        "too_long"
    );
    s.admin(AdminArgs::ConstitutionNotice {
        text: "Article votes close at the season's end.".into(),
        pin: true,
    })
    .unwrap();
    let br = af_core::query::briefing(&s.w, c).unwrap();
    assert!(br["server"]["announcements"].to_string().contains("Article votes close"));
    s.admin(AdminArgs::ConstitutionNotice {
        text: "Discuss proposal one on the radio.".into(),
        pin: false,
    })
    .unwrap();
    let br = af_core::query::briefing(&s.w, c).unwrap();
    assert!(
        br["server"]["operator_messages"]
            .to_string()
            .contains("OPERATOR on the constitution: Discuss proposal one")
    );
    assert!(
        s.w.radio
            .iter()
            .any(|m| m.text == "OPERATOR on the constitution: Discuss proposal one on the radio.")
    );
    assert_eq!(s.w.constitution.notices.len(), 2);
    assert_eq!(public_notices(&s.w), 2);
    let v = af_core::constitution::view(&s.w, a).unwrap();
    assert_eq!(v["operator_notices"][0]["text"], "Discuss proposal one on the radio.");
    s.replays();
}

fn public_notices(w: &World) -> usize {
    af_core::constitution::public_view(w)["operator_notices"].as_array().unwrap().len()
}
