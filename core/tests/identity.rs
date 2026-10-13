//! Engine 1.46: captain identities, the one rename, the AP gate and the Chronicle.
use af_core::chronicle::{self, AdminAction, Cite, Kind, Record, Status};
use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::identity;
use af_core::model::*;
use af_core::{Command, World};

struct Sim {
    w: World,
    journal: Vec<Entry>,
    cfg: GenesisConfig,
}

impl Sim {
    fn new() -> Self {
        let cfg = GenesisConfig::new(46, 60);
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
    fn join(&mut self, name: &str) -> Result<PlayerId, af_core::Refusal> {
        let out = self.run(
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
        )?;
        Ok(out.reply["player_id"].as_u64().unwrap() as PlayerId)
    }
    fn identity(&mut self, pid: PlayerId, a: identity::Args) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(Actor::Player(pid), Command::Identity(Box::new(a)))
    }
    fn chronicle(&mut self, pid: PlayerId, a: chronicle::Args) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(Actor::Player(pid), Command::Chronicle(a))
    }
    fn admin(&mut self, a: chronicle::AdminArgs) -> Result<af_core::Outcome, af_core::Refusal> {
        self.run(Actor::System, Command::Admin(AdminArgs::Chronicle(a)))
    }
    fn document(&mut self, pid: PlayerId, home: &str) {
        self.identity(pid, full(home)).unwrap();
    }
    fn warp(&mut self, pid: PlayerId) -> Result<af_core::Outcome, af_core::Refusal> {
        let to = self.w.sector(self.w.players[&pid].sector).unwrap().warps[0];
        self.run(Actor::Player(pid), Command::Warp(WarpArgs { destination_sector: to }))
    }
    /// Replay from genesis, every entry first round-tripped through JSON as the store does.
    fn replays(&self) {
        let mut w = generate(&self.cfg);
        for e in &self.journal {
            let json = serde_json::to_string(e).unwrap();
            let back: Entry = serde_json::from_str(&json).unwrap_or_else(|err| panic!("journal entry does not deserialize: {err}: {json}"));
            assert_eq!(&back, e, "journal entry round-trips");
            let _ = apply(&mut w, &back);
        }
        assert_eq!(w.state_hash(), self.w.state_hash(), "replay reproduces the world");
    }
}

fn full(home: &str) -> identity::Args {
    identity::Args {
        action: identity::Action::Set,
        age: Some(47),
        pronouns: Some("she/her".into()),
        homeworld: Some(home.into()),
        bio: Some("A welder turned trader.".into()),
        backstory: Some("## Family\nTwo daughters.\n\n## Dream\nA farm with real soil.".into()),
        ..Default::default()
    }
}

fn admin(decision: AdminAction, production: Option<u64>, page: Option<u64>, links: Option<Vec<String>>) -> chronicle::AdminArgs {
    chronicle::AdminArgs {
        decision,
        production,
        page,
        note: None,
        links,
    }
}

#[test]
fn identities_are_versioned_private_and_survive_a_fresh_start() {
    let mut s = Sim::new();
    let a = s.join("Machine Handle").unwrap();
    // The first set needs every required field, and nothing is stored when one is missing.
    let r = s
        .identity(
            a,
            identity::Args {
                action: identity::Action::Set,
                age: Some(30),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "identity_incomplete");
    assert!(!identity::documented(&s.w, a));
    let r = s
        .identity(
            a,
            identity::Args {
                age: Some(12),
                ..full("Tessaly")
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "invalid_age");
    // The one rename: refused for overused or taken names, then allowed once.
    let b = s.join("Ines Barr").unwrap();
    let r = s
        .identity(
            a,
            identity::Args {
                name: Some("Elara Voss".into()),
                ..full("Tessaly")
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "name_overused");
    let r = s
        .identity(
            a,
            identity::Args {
                name: Some("ines-barr".into()),
                ..full("Tessaly")
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "name_taken");
    assert_eq!(s.w.players[&a].name, "Machine Handle", "nothing changed on a refusal");
    let out = s
        .identity(
            a,
            identity::Args {
                name: Some("Tomas Oyelaran".into()),
                ..full("Tessaly")
            },
        )
        .unwrap();
    assert_eq!(out.reply["renamed"], true);
    assert_eq!(s.w.players[&a].name, "Tomas Oyelaran");
    assert!(identity::documented(&s.w, a));
    assert!(
        s.w.news
            .iter()
            .any(|n| n.headline.contains("Machine Handle is now known as Tomas Oyelaran"))
    );
    let r = s
        .identity(
            a,
            identity::Args {
                name: Some("Tomas Again".into()),
                ..Default::default()
            }
            .with_set(),
        )
        .unwrap_err();
    assert_eq!(r.code, "rename_used");
    // Later sets change only what they send, and every version stays.
    s.identity(
        a,
        identity::Args {
            action: identity::Action::Set,
            why: Some("debts".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let id = &s.w.identities[&a];
    assert_eq!(id.versions.len(), 2);
    assert_eq!(id.current().unwrap().homeworld, "Tessaly");
    assert_eq!(id.current().unwrap().why, "debts");
    assert_eq!(id.former_names, vec!["Machine Handle".to_string()]);
    // Others see the card, never the backstory.
    let card = identity::read(
        &s.w,
        b,
        &identity::Args {
            player: Some("Tomas Oyelaran".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(card["identity"]["homeworld"], "Tessaly");
    assert!(!card.to_string().contains("real soil"));
    let own = identity::read(&s.w, a, &identity::Args::default()).unwrap();
    assert!(own.to_string().contains("real soil"));
    // Registration refuses an overused name too.
    assert_eq!(s.join("Kael Thorne").unwrap_err().code, "name_overused");
    // A fresh start keeps the identity and the new call sign.
    s.run(Actor::System, Command::Admin(AdminArgs::FreshStart)).unwrap();
    assert_eq!(s.w.players[&a].name, "Tomas Oyelaran");
    assert!(identity::documented(&s.w, a));
    assert_eq!(s.w.identities[&a].versions.len(), 2);
    s.replays();
}

trait WithSet {
    fn with_set(self) -> Self;
}
impl WithSet for identity::Args {
    fn with_set(mut self) -> Self {
        self.action = identity::Action::Set;
        self
    }
}

#[test]
fn without_an_identity_no_action_point_is_spent_when_required() {
    let mut s = Sim::new();
    let a = s.join("Ada Mbeki").unwrap();
    // Off (the default): play as before.
    s.warp(a).unwrap();
    s.w.rules.identity_required = true;
    s.cfg.rules.identity_required = true;
    let ap = s.w.players[&a].ap;
    let r = s.warp(a).unwrap_err();
    assert_eq!(r.code, "identity_required");
    assert_eq!(s.w.players[&a].ap, ap, "nothing spent");
    // Free things stay free, and writing the identity lifts the gate at once.
    let b = query_briefing(&s.w, a);
    assert_eq!(b["identity"]["due"], true);
    assert_eq!(b["identity"]["ap_blocked"], true);
    s.document(a, "Lowmoor");
    s.warp(a).unwrap();
    let b = query_briefing(&s.w, a);
    assert!(
        b["identity"]["you_play"]
            .as_str()
            .unwrap()
            .contains("You are Ada Mbeki (she/her), 47, of Lowmoor")
    );
}

fn query_briefing(w: &World, pid: PlayerId) -> serde_json::Value {
    af_core::query::briefing(w, pid).unwrap()
}

#[test]
fn chronicle_pages_cite_the_record_and_become_canon_by_endorsement() {
    let mut s = Sim::new();
    let a = s.join("Ada Mbeki").unwrap();
    let b = s.join("Bram Ostrowski").unwrap();
    let c = s.join("Chidi Okonjo").unwrap();
    let d = s.join("Dalia Haddad").unwrap();
    // Only documented captains write.
    let page = chronicle::Args {
        action: chronicle::Action::Write,
        kind: Some(Kind::Event),
        title: Some("The first arrivals".into()),
        body: Some("They came on the same day.".into()),
        ..Default::default()
    };
    assert_eq!(s.chronicle(a, page.clone()).unwrap_err().code, "identity_required");
    for (p, home) in [(a, "Lowmoor"), (b, "Pell"), (c, "Okun"), (d, "Sabra")] {
        s.document(p, home);
    }
    // A citation that matches nothing is refused; one that matches keeps a copy of the record.
    let headline = s.w.news.last().unwrap().clone();
    let mut bad = page.clone();
    bad.citations = Some(vec![Cite {
        record: Record::News,
        id: None,
        tick: Some(headline.tick),
        quote: Some("nothing like it".into()),
        note: None,
    }]);
    assert_eq!(s.chronicle(a, bad).unwrap_err().code, "citation_not_found");
    let mut good = page.clone();
    good.citations = Some(vec![Cite {
        record: Record::News,
        id: None,
        tick: Some(headline.tick),
        quote: Some("filed papers".into()),
        note: Some("the day she arrived".into()),
    }]);
    let out = s.chronicle(a, good).unwrap();
    let pid = out.reply["page"].as_u64().unwrap();
    let pg = &s.w.chronicle.pages[&pid];
    assert!(pg.current().citations[0].on_record.contains("filed papers"));
    // Authors cannot endorse; two endorsers from two groups make canon while few captains are active.
    assert_eq!(chronicle::canon_needed(&s.w), 2);
    assert_eq!(
        s.chronicle(
            a,
            chronicle::Args {
                action: chronicle::Action::Endorse,
                page: Some(pid),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        "own_page"
    );
    s.chronicle(
        b,
        chronicle::Args {
            action: chronicle::Action::Endorse,
            page: Some(pid),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(s.w.chronicle.pages[&pid].canon_tick.is_none());
    let out = s
        .chronicle(
            c,
            chronicle::Args {
                action: chronicle::Action::Endorse,
                page: Some(pid),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(out.reply["canon"], true);
    // Canon adds to the creator's Influence this season.
    assert_eq!(chronicle::influence(&s.w, a), chronicle::CANON_INFLUENCE);
    // Person pages belong to their character's captain.
    let out = s
        .chronicle(
            a,
            chronicle::Args {
                action: chronicle::Action::Write,
                kind: Some(Kind::Person),
                title: Some("Ada".into()),
                body: Some("Me.".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let person = out.reply["page"].as_u64().unwrap();
    let r = s
        .chronicle(
            b,
            chronicle::Args {
                action: chronicle::Action::Write,
                page: Some(person),
                body: Some("Lies.".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "not_yours");
    // Disputes are talk; reports reach the operator; a strike hides the text everywhere.
    s.chronicle(
        d,
        chronicle::Args {
            action: chronicle::Action::Talk,
            page: Some(pid),
            text: Some("Not how it went.".into()),
            dispute: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    s.chronicle(
        d,
        chronicle::Args {
            action: chronicle::Action::Report,
            page: Some(person),
            text: Some("test".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(chronicle::operator_view(&s.w)["open_reports"], 1);
    s.admin(admin(AdminAction::Strike, None, Some(person), None)).unwrap();
    let public = chronicle::public_page(&s.w, person).unwrap();
    assert_eq!(public["body"], chronicle::STRUCK);
    assert!(
        s.w.chronicle.pages[&person].current().body == "Me.",
        "the record keeps the text for the operator"
    );
    s.replays();
}

#[test]
fn productions_run_from_pitch_to_release_with_credits() {
    let mut s = Sim::new();
    let a = s.join("Ada Mbeki").unwrap();
    let b = s.join("Bram Ostrowski").unwrap();
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
    let out = s.chronicle(a, pitch.clone()).unwrap();
    let x = out.reply["production"].as_u64().unwrap();
    assert_eq!(s.chronicle(a, pitch).unwrap_err().code, "pitch_open");
    // The operator greenlights: a script page opens for everyone documented to write.
    let out = s.admin(admin(AdminAction::Greenlight, Some(x), None, None)).unwrap();
    let script = out.reply["script_page"].as_u64().unwrap();
    s.chronicle(
        b,
        chronicle::Args {
            action: chronicle::Action::Write,
            page: Some(script),
            body: Some("**ADA MBEKI:** Pay up.\n\n**BRAM OSTROWSKI:** Later.".into()),
            ..Default::default()
        },
    )
    .unwrap();
    s.chronicle(
        a,
        chronicle::Args {
            action: chronicle::Action::Approve,
            production: Some(x),
            ..Default::default()
        },
    )
    .unwrap();
    let pkg = chronicle::production_package(&s.w, x).unwrap();
    assert_eq!(pkg["checks"]["unapproved_cast"], serde_json::json!(["Bram Ostrowski"]));
    assert_eq!(pkg["cast_sheet"][0]["card"]["homeworld"], "Lowmoor");
    assert_eq!(pkg["cast_sheet"][0]["card"]["pronouns"], "she/her");
    // Lock freezes the script; release needs a link and credits everyone.
    s.admin(admin(AdminAction::Lock, Some(x), None, None)).unwrap();
    assert_eq!(
        s.chronicle(
            b,
            chronicle::Args {
                action: chronicle::Action::Write,
                page: Some(script),
                body: Some("late edit".into()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        "script_closed"
    );
    assert_eq!(
        s.admin(admin(AdminAction::Release, Some(x), None, None)).unwrap_err().code,
        "links_required"
    );
    s.admin(admin(
        AdminAction::Release,
        Some(x),
        None,
        Some(vec!["https://example.com/debts.mp3".into()]),
    ))
    .unwrap();
    assert_eq!(s.w.chronicle.productions[&x].status, Status::Released);
    assert_eq!(chronicle::influence(&s.w, b), chronicle::CREDIT_INFLUENCE);
    assert!(s.w.players[&b].inbox.iter().any(|n| n.text.contains("released")));
    s.replays();
}

#[test]
fn characters_keep_a_log_a_timeline_and_relations_and_the_bible_holds_them() {
    let mut s = Sim::new();
    let a = s.join("Ada Mbeki").unwrap();
    let b = s.join("Bram Ostrowski").unwrap();
    // Logging needs an identity.
    let log = |text: &str, private: bool| identity::Args {
        action: identity::Action::Log,
        text: Some(text.into()),
        private: Some(private),
        about: Some(vec!["Bram Ostrowski".into()]),
        ..Default::default()
    };
    assert_eq!(s.identity(a, log("x", false)).unwrap_err().code, "identity_required");
    s.document(b, "Pell");
    // Relations: public and private, one of them another captain.
    s.identity(
        a,
        identity::Args {
            relations: Some(vec![
                identity::RelationArg {
                    name: "Teodor".into(),
                    relation: "son".into(),
                    status: Some("missing".into()),
                    ..Default::default()
                },
                identity::RelationArg {
                    name: "Bram".into(),
                    relation: "old debt".into(),
                    captain: Some("Bram Ostrowski".into()),
                    private: Some(true),
                    ..Default::default()
                },
            ]),
            ..full("Lowmoor")
        },
    )
    .unwrap();
    s.identity(a, log("Bram still owes me.", false)).unwrap();
    s.identity(a, log("I miss Teodor.", true)).unwrap();
    // The timeline holds the public moments she was part of.
    let i = &s.w.identities[&a];
    assert!(i.moments.iter().any(|m| m.kind == "identity_filed"));
    assert_eq!(i.log.len(), 2);
    // Others see public lines and public relations only.
    let other = identity::read(
        &s.w,
        b,
        &identity::Args {
            action: identity::Action::Story,
            player: Some("Ada Mbeki".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(other.to_string().contains("Bram still owes me."));
    assert!(!other.to_string().contains("I miss Teodor."));
    let card = identity::card(&s.w, a).unwrap();
    assert!(card.to_string().contains("Teodor"));
    assert!(!card.to_string().contains("old debt"));
    let public = chronicle::public_person(&s.w, "Ada Mbeki", None).unwrap();
    assert!(!public.to_string().contains("I miss Teodor."));
    assert!(public.get("player_id").is_none());
    // The bible holds everything, private parts marked.
    let bible = chronicle::story_bible(&s.w, None);
    let ada = bible["characters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["call_sign"] == "Ada Mbeki")
        .unwrap();
    assert!(ada["private"]["backstory"].as_str().unwrap().contains("real soil"));
    assert_eq!(ada["private"]["relations"][0]["captain"], "Bram Ostrowski");
    assert_eq!(ada["log"].as_array().unwrap().len(), 2);
    assert!(!ada["moments"].as_array().unwrap().is_empty());
    s.replays();
    // Clearing strategies wipes every human strategy and tells each captain (state set directly
    // below, so no replay after it).
    s.w.players.get_mut(&a).unwrap().owner.orders.strategy = Some("get rich".into());
    s.run(Actor::System, Command::Admin(AdminArgs::ClearStrategies)).unwrap();
    assert!(s.w.players[&a].owner.orders.strategy.is_none());
    assert!(s.w.players[&a].inbox.iter().any(|n| n.kind == "strategy"));
}

#[test]
fn every_character_is_male_or_female() {
    let mut s = Sim::new();
    let a = s.join("Ada Mbeki").unwrap();
    let r = s
        .identity(
            a,
            identity::Args {
                pronouns: Some("they/them".into()),
                ..full("Lowmoor")
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "invalid_pronouns");
    s.identity(
        a,
        identity::Args {
            pronouns: Some("Female".into()),
            ..full("Lowmoor")
        },
    )
    .unwrap();
    assert_eq!(s.w.identities[&a].current().unwrap().pronouns, "she/her");
    assert_eq!(identity::card(&s.w, a).unwrap()["gender"], "female");
    // A stored identity without male or female pronouns counts as none, and blocks AP.
    s.w.identities.get_mut(&a).unwrap().versions.last_mut().unwrap().pronouns = "they/them".into();
    assert!(!identity::documented(&s.w, a));
    s.w.rules.identity_required = true;
    assert_eq!(s.warp(a).unwrap_err().code, "identity_required");
    s.identity(
        a,
        identity::Args {
            action: identity::Action::Set,
            pronouns: Some("he/him".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(identity::documented(&s.w, a));
}

#[test]
fn a_required_rename_must_happen_before_a_captain_counts_as_documented() {
    let mut s = Sim::new();
    let a = s.join("Brash Barnacle").unwrap();
    s.document(a, "Rillhaven");
    assert!(identity::documented(&s.w, a));
    s.run(Actor::System, Command::Admin(AdminArgs::RequireRename)).unwrap();
    assert!(!identity::documented(&s.w, a));
    s.w.rules.identity_required = true;
    s.cfg.rules.identity_required = true;
    assert_eq!(s.warp(a).unwrap_err().code, "identity_required");
    let r = s
        .identity(
            a,
            identity::Args {
                action: identity::Action::Set,
                name: Some("Brash Barnacle Jr".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "name_too_close");
    s.identity(
        a,
        identity::Args {
            action: identity::Action::Set,
            name: Some("Tomasz Wierzba".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(identity::documented(&s.w, a));
    // A captain who joins later is not listed.
    let b = s.join("Halina Oduya").unwrap();
    s.document(b, "Pell");
    assert!(identity::documented(&s.w, b));
    // The list survives a fresh start.
    s.run(Actor::System, Command::Admin(AdminArgs::FreshStart)).unwrap();
    assert!(s.w.rename_required.contains(&a));
    assert!(identity::documented(&s.w, a));
    s.replays();
}

#[test]
fn a_rename_carries_the_new_name_through_the_identity_and_the_operator_fixes_old_ones() {
    let mut s = Sim::new();
    let a = s.join("MaracaMasher").unwrap();
    s.identity(
        a,
        identity::Args {
            bio: Some("Maraca Masher rattles through the lanes. MARACAMASHER never sleeps.".into()),
            backstory: Some("Everyone on Solexthas knew MaracaMasher.".into()),
            ..full("Solexthas")
        },
    )
    .unwrap();
    s.identity(
        a,
        identity::Args {
            action: identity::Action::Set,
            name: Some("Ilse Varnhagen".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let v = s.w.identities[&a].current().unwrap().clone();
    assert_eq!(v.bio, "Ilse Varnhagen rattles through the lanes. Ilse Varnhagen never sleeps.");
    assert_eq!(v.backstory, "Everyone on Solexthas knew Ilse Varnhagen.");
    assert!(
        s.w.identities[&a].versions[0].bio.contains("Maraca Masher"),
        "history keeps the original"
    );
    // A captain that renamed before 1.49 (text left as it was) is fixed by the operator.
    let b = s.join("Brash Barnacle").unwrap();
    s.identity(
        b,
        identity::Args {
            bio: Some("Brash Barnacle earned his name in a blockade.".into()),
            ..full("Rillhaven")
        },
    )
    .unwrap();
    {
        let i = s.w.identities.get_mut(&b).unwrap();
        i.former_names.push("Brash Barnacle".into());
        i.renamed_tick = Some(1);
    }
    s.w.players.get_mut(&b).unwrap().name = "Tebren Darsai".into();
    let out = s.run(Actor::System, Command::Admin(AdminArgs::FixOldNames)).unwrap();
    assert_eq!(out.reply["updated"], serde_json::json!(["Tebren Darsai"]));
    assert_eq!(
        s.w.identities[&b].current().unwrap().bio,
        "Tebren Darsai earned his name in a blockade."
    );
    assert!(s.w.players[&b].inbox.iter().any(|n| n.kind == "identity"));
}

#[test]
fn first_names_are_unique_and_the_operator_can_grant_another_rename() {
    let mut s = Sim::new();
    let a = s.join("BorzoiDetour").unwrap();
    let b = s.join("BorzoiMerge").unwrap();
    s.identity(
        a,
        identity::Args {
            name: Some("Tavian Rooke".into()),
            ..full("Pell")
        },
    )
    .unwrap();
    let r = s
        .identity(
            b,
            identity::Args {
                name: Some("Tavian Rusk".into()),
                ..full("Pell")
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "name_taken");
    s.identity(
        b,
        identity::Args {
            name: Some("Oskar Rusk".into()),
            ..full("Pell")
        },
    )
    .unwrap();
    s.run(
        Actor::System,
        Command::Admin(AdminArgs::AllowRename {
            player: b,
            reason: Some("test".into()),
        }),
    )
    .unwrap();
    assert!(!identity::documented(&s.w, b));
    s.identity(
        b,
        identity::Args {
            action: identity::Action::Set,
            name: Some("Lukas Morrow".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(identity::documented(&s.w, b));
    assert_eq!(
        s.w.identities[&b].former_names,
        vec!["BorzoiMerge".to_string(), "Oskar Rusk".to_string()]
    );
    s.replays();
}

#[test]
fn relations_follow_renames() {
    let mut s = Sim::new();
    let a = s.join("Radish Rampage").unwrap();
    let b = s.join("Cutlass Chowder").unwrap();
    s.document(a, "Drogaseni");
    // Written before a's rename: by name, not linked, with the old name in the note.
    s.identity(
        b,
        identity::Args {
            relations: Some(vec![identity::RelationArg {
                name: "Radish Rampage".into(),
                relation: "ally".into(),
                note: Some("Radish Rampage covers my flank.".into()),
                ..Default::default()
            }]),
            ..full("Saltmere")
        },
    )
    .unwrap();
    // Matching a captain's call sign links it.
    assert_eq!(s.w.identities[&b].current().unwrap().relations[0].captain, Some(a));
    s.identity(
        a,
        identity::Args {
            action: identity::Action::Set,
            name: Some("Reupena Masina".into()),
            ..Default::default()
        },
    )
    .unwrap();
    // Linked relations show the current name at once.
    assert!(identity::card(&s.w, b).unwrap().to_string().contains("Reupena Masina"));
    // The operator's fix writes the current name and the note into a new version.
    s.identity(
        b,
        identity::Args {
            action: identity::Action::Set,
            why: Some("debts".into()),
            ..Default::default()
        },
    )
    .unwrap();
    {
        let rel = &mut s.w.identities.get_mut(&b).unwrap().versions.last_mut().unwrap().relations[0];
        rel.captain = None;
    }
    s.run(Actor::System, Command::Admin(AdminArgs::FixRelations)).unwrap();
    let rel = &s.w.identities[&b].current().unwrap().relations[0];
    assert_eq!((rel.captain, rel.name.as_str()), (Some(a), "Reupena Masina"));
    assert_eq!(rel.note, "Reupena Masina covers my flank.");
}

#[test]
fn clearing_the_chronicle_removes_every_record_without_reusing_ids_or_resetting_captains() {
    let mut s = Sim::new();
    let a = s.join("Ada Mbeki").unwrap();
    s.document(a, "Lowmoor");
    let create = chronicle::Args {
        action: chronicle::Action::Write,
        title: Some("Old character".into()),
        body: Some("An obsolete story.".into()),
        ..Default::default()
    };
    let page = s.chronicle(a, create.clone()).unwrap().reply["page"].as_u64().unwrap();
    s.chronicle(
        a,
        chronicle::Args {
            action: chronicle::Action::Write,
            page: Some(page),
            body: Some("A second obsolete revision.".into()),
            ..Default::default()
        },
    )
    .unwrap();
    s.chronicle(
        a,
        chronicle::Args {
            action: chronicle::Action::Talk,
            page: Some(page),
            text: Some("An obsolete comment.".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let pitch = chronicle::Args {
        action: chronicle::Action::Pitch,
        title: Some("Old production".into()),
        text: Some("An obsolete episode.".into()),
        format: Some(chronicle::Format::Audio),
        ..Default::default()
    };
    let production = s.chronicle(a, pitch.clone()).unwrap().reply["production"].as_u64().unwrap();
    s.admin(admin(AdminAction::Greenlight, Some(production), None, None)).unwrap();
    let players = s.w.players.clone();
    let sectors = s.w.sectors.clone();
    let counters = (s.w.chronicle.next_page, s.w.chronicle.next_comment, s.w.chronicle.next_production);
    let clear = admin(AdminAction::Clear, None, None, None);
    assert!(
        s.run(Actor::Player(a), Command::Admin(AdminArgs::Chronicle(clear.clone())))
            .is_err()
    );
    assert_eq!(s.w.chronicle.pages.len(), 2, "captains cannot clear it");
    let out = s.admin(clear.clone()).unwrap();
    assert_eq!(out.reply, serde_json::json!({"pages_removed": 2, "productions_removed": 1}));
    assert!(s.w.chronicle.pages.is_empty());
    assert!(s.w.chronicle.productions.is_empty());
    assert!(chronicle::public_page(&s.w, page).is_none());
    assert_eq!(s.w.players, players);
    assert_eq!(s.w.sectors, sectors);
    assert_eq!(
        (s.w.chronicle.next_page, s.w.chronicle.next_comment, s.w.chronicle.next_production),
        counters
    );
    assert_eq!(
        s.admin(clear).unwrap().reply,
        serde_json::json!({"pages_removed": 0, "productions_removed": 0})
    );
    let new_page = s.chronicle(a, create).unwrap().reply["page"].as_u64().unwrap();
    let new_production = s.chronicle(a, pitch).unwrap().reply["production"].as_u64().unwrap();
    assert!(new_page > counters.0 && new_production > counters.2);
    assert!(chronicle::public_page(&s.w, page).is_none());
    s.replays();
}

// ---------- engine 1.58: Chronicle notices, discovery, bylines and canon moments ----------

fn write(page: Option<u64>, title: Option<&str>, body: &str) -> chronicle::Args {
    chronicle::Args {
        action: chronicle::Action::Write,
        page,
        kind: page.is_none().then_some(Kind::Event),
        title: title.map(str::to_string),
        body: Some(body.into()),
        ..Default::default()
    }
}

fn talk_args(page: u64, text: &str, dispute: bool, citations: Vec<Cite>) -> chronicle::Args {
    chronicle::Args {
        action: chronicle::Action::Talk,
        page: Some(page),
        text: Some(text.into()),
        dispute: Some(dispute),
        citations: (!citations.is_empty()).then_some(citations),
        ..Default::default()
    }
}

fn cite(record: Record, id: u64) -> Cite {
    Cite {
        record,
        id: Some(id),
        tick: None,
        quote: None,
        note: None,
    }
}

fn endorse(page: u64) -> chronicle::Args {
    chronicle::Args {
        action: chronicle::Action::Endorse,
        page: Some(page),
        ..Default::default()
    }
}

/// Notices of one kind the captain has not read.
fn unread(s: &Sim, pid: PlayerId, kind: &str) -> Vec<Notice> {
    s.w.players[&pid]
        .inbox
        .iter()
        .filter(|n| !n.read && n.kind == kind)
        .cloned()
        .collect()
}

fn mark_all_read(s: &mut Sim, pid: PlayerId) {
    let ids: Vec<u64> = s.w.players[&pid].inbox.iter().map(|n| n.id).collect();
    s.run(
        Actor::Player(pid),
        Command::MarkRead(MarkReadArgs {
            notice_up_to: 0,
            radio_up_to: 0,
            announcements_up_to: None,
            notice_ids: Some(ids),
            radio_ids: Some(vec![]),
        }),
    )
    .unwrap();
}

/// Six documented captains.
fn cast() -> (Sim, [PlayerId; 6]) {
    let mut s = Sim::new();
    let names = [
        ("Ada Mbeki", "Lowmoor"),
        ("Bram Ostrowski", "Pell"),
        ("Chidi Okonjo", "Okun"),
        ("Dalia Haddad", "Sabra"),
        ("Esko Vaara", "Tarn"),
        ("Fen Ilori", "Ibeka"),
    ];
    let mut ids = [0; 6];
    for (i, (n, home)) in names.iter().enumerate() {
        ids[i] = s.join(n).unwrap();
        s.document(ids[i], home);
    }
    (s, ids)
}

#[test]
fn revisions_and_talk_tell_every_author_once_and_fold_while_unread() {
    let (mut s, [a, b, c, d, e, _]) = cast();
    let page = s
        .chronicle(a, write(None, Some("The long night"), "It began at dusk."))
        .unwrap()
        .reply["page"]
        .as_u64()
        .unwrap();
    s.chronicle(c, endorse(page)).unwrap();
    let out = s.chronicle(b, write(Some(page), None, "It began at dusk, in the rain.")).unwrap();
    assert_eq!(
        out.reply["told"],
        serde_json::json!({"authors": 2, "cited": 0}),
        "the author and the endorser"
    );
    for p in [a, c] {
        let n = unread(&s, p, "chronicle");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].quote.as_deref(), Some("The long night"), "the title is player text");
        assert!(n[0].text.contains("has a new revision 2 by Bram Ostrowski"), "{}", n[0].text);
        assert!(!n[0].text.contains("The long night"), "never inside server text");
    }
    assert!(unread(&s, b, "chronicle").is_empty(), "the writer is not told");
    // A second revision folds into the unread notice: one notice says 2 to 3.
    let old = unread(&s, a, "chronicle")[0].id;
    s.chronicle(d, write(Some(page), None, "It began at dusk, in the rain, far out."))
        .unwrap();
    let n = unread(&s, a, "chronicle");
    assert_eq!(n.len(), 1, "folded: {n:?}");
    assert!(
        n[0].text.contains("new revisions 2 to 3 by Bram Ostrowski and Dalia Haddad"),
        "{}",
        n[0].text
    );
    let superseded = s.w.players[&a].inbox.iter().find(|x| x.id == old).unwrap();
    assert!(superseded.read && superseded.superseded_by == Some(n[0].id));
    assert_eq!(unread(&s, b, "chronicle").len(), 1, "b is an author now");
    // Read, then a fresh notice starts a new range.
    mark_all_read(&mut s, a);
    s.chronicle(b, write(Some(page), None, "It began at dusk, in the rain, far out, alone."))
        .unwrap();
    assert!(
        unread(&s, a, "chronicle")[0]
            .text
            .contains("has a new revision 4 by Bram Ostrowski")
    );
    for p in [a, b, c, d] {
        mark_all_read(&mut s, p);
    }
    // Plain comments fold per page and reach every author, never the endorser.
    let out = s.chronicle(e, talk_args(page, "Lovely.", false, vec![])).unwrap();
    assert_eq!(out.reply["told"]["authors"], 3);
    s.chronicle(e, talk_args(page, "Truly.", false, vec![])).unwrap();
    for p in [a, b, d] {
        let n = unread(&s, p, "chronicle");
        assert_eq!(n.len(), 1, "{n:?}");
        assert!(n[0].text.contains("has 2 new comments by Esko Vaara"), "{}", n[0].text);
        assert!(matches!(n[0].about, Some(af_core::mention::Place::Talk { .. })));
    }
    assert!(unread(&s, c, "chronicle").is_empty());
    for p in [a, b, d] {
        mark_all_read(&mut s, p);
    }
    // A dispute is never folded and always says how many citations it carries.
    let radio = {
        s.run(
            Actor::Player(e),
            Command::Radio(RadioArgs {
                message: "On the record.".into(),
                ..Default::default()
            }),
        )
        .unwrap()
        .reply["message_id"]
            .as_u64()
            .unwrap()
    };
    let other = s.chronicle(e, write(None, Some("Another view"), "Not like that.")).unwrap().reply["page"]
        .as_u64()
        .unwrap();
    s.chronicle(e, talk_args(page, "Wrong.", true, vec![])).unwrap();
    s.chronicle(e, talk_args(page, "Still wrong.", true, vec![cite(Record::Radio, radio)]))
        .unwrap();
    s.chronicle(
        e,
        talk_args(page, "See both.", true, vec![cite(Record::Radio, radio), cite(Record::Page, other)]),
    )
    .unwrap();
    let texts: Vec<String> = unread(&s, a, "chronicle").into_iter().map(|n| n.text).collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(texts[0].contains("disputed Chronicle page") && texts[0].contains("with no citations"));
    assert!(texts[1].contains("with 1 citation."));
    assert!(texts[2].contains("with 2 citations."));
    assert!(unread(&s, a, "chronicle").iter().all(|n| n.fold.is_none()));
    s.replays();
}

#[test]
fn a_record_first_cited_on_a_page_tells_its_captains_once() {
    let (mut s, [a, b, c, d, e, f]) = cast();
    // Records: b's public radio message, c's page, a signed captain treaty between d and e, a
    // signed treaty between two corporations (CEOs c and f; e is a member of f's), an article
    // from d's proposal.
    let radio = s
        .run(
            Actor::Player(b),
            Command::Radio(RadioArgs {
                message: "Ore is cheap at the rim.".into(),
                ..Default::default()
            }),
        )
        .unwrap()
        .reply["message_id"]
        .as_u64()
        .unwrap();
    let cpage = s.chronicle(c, write(None, Some("Chidi's map"), "Lanes.")).unwrap().reply["page"]
        .as_u64()
        .unwrap();
    let treaty = |s: &mut Sim, from: PlayerId, corp: Option<&str>, captain: Option<&str>| {
        s.run(
            Actor::Player(from),
            Command::Treaty(TreatyArgs {
                action: TreatyAction::Propose,
                kind: Some(TreatyKind::Nap),
                corp: corp.map(str::to_string),
                captain: captain.map(str::to_string),
                personal: false,
                treaty_id: None,
                bond: None,
                terms: None,
                duration_ticks: None,
            }),
        )
        .unwrap()
        .reply["treaty_id"]
            .as_u64()
            .unwrap()
    };
    let accept = |s: &mut Sim, by: PlayerId, id: u64| {
        s.run(
            Actor::Player(by),
            Command::Treaty(TreatyArgs {
                action: TreatyAction::Accept,
                kind: None,
                corp: None,
                captain: None,
                personal: false,
                treaty_id: Some(id),
                bond: None,
                terms: None,
                duration_ticks: None,
            }),
        )
        .unwrap();
    };
    let personal = treaty(&mut s, d, None, Some("Esko Vaara"));
    accept(&mut s, e, personal);
    let corp = |s: &mut Sim, pid: PlayerId, action: CorpAction, name: Option<&str>, player: Option<&str>| {
        s.run(
            Actor::Player(pid),
            Command::Corp(CorpArgs {
                action,
                name: name.map(str::to_string),
                player: player.map(str::to_string),
                amount: None,
                motto: None,
            }),
        )
        .unwrap();
    };
    corp(&mut s, c, CorpAction::Create, Some("Okun Line"), None);
    corp(&mut s, f, CorpAction::Create, Some("Ibeka Guild"), None);
    corp(&mut s, f, CorpAction::Invite, None, Some("Esko Vaara"));
    corp(&mut s, e, CorpAction::Join, Some("Ibeka Guild"), None);
    let corporate = treaty(&mut s, c, Some("Ibeka Guild"), None);
    accept(&mut s, f, corporate);
    let proposal: u64 = {
        let a: af_core::constitution::ConstitutionArgs =
            serde_json::from_value(serde_json::json!({"action": "propose", "kind": "add", "text": "Speak plainly."})).unwrap();
        s.run(Actor::Player(d), Command::Constitution(a)).unwrap().reply["proposal"]["id"]
            .as_u64()
            .unwrap()
    };
    for v in [a, b, c, e] {
        let a: af_core::constitution::ConstitutionArgs =
            serde_json::from_value(serde_json::json!({"action": "vote", "proposal": proposal, "vote": true})).unwrap();
        let _ = s.run(Actor::Player(v), Command::Constitution(a));
    }
    let article = s.w.constitution.articles.last().expect("ratified").id;
    for p in [a, b, c, d, e, f] {
        mark_all_read(&mut s, p);
    }
    // a writes one page citing all of them.
    let mut args = write(None, Some("Who signed what"), "An account.");
    args.citations = Some(vec![
        cite(Record::Radio, radio),
        cite(Record::Page, cpage),
        cite(Record::Treaty, personal),
        cite(Record::Treaty, corporate),
        cite(Record::Article, article),
    ]);
    let out = s.chronicle(a, args).unwrap();
    let page = out.reply["page"].as_u64().unwrap();
    // b (radio), c (page, CEO of a side), d (treaty party, article), e (treaty party), f (CEO).
    assert_eq!(out.reply["told"], serde_json::json!({"authors": 0, "cited": 5}));
    let one = |s: &Sim, p: PlayerId| {
        let n = unread(s, p, "chronicle");
        assert_eq!(n.len(), 1, "one notice per captain per write: {n:?}");
        assert_eq!(n[0].quote.as_deref(), Some("Who signed what"));
        n[0].text.clone()
    };
    assert!(one(&s, b).contains(&format!("cites your public radio message {radio}: page {page}")));
    let ct = one(&s, c);
    assert!(ct.contains(&format!("your Chronicle page {cpage}")), "{ct}");
    assert!(
        ct.contains(&format!("a treaty your corporation signed (treaty #{corporate})")),
        "{ct}"
    );
    let dt = one(&s, d);
    assert!(dt.contains(&format!("the treaty you signed (treaty #{personal})")), "{dt}");
    assert!(dt.contains(&format!("article {article}, which you proposed")), "{dt}");
    let et = one(&s, e);
    assert!(et.contains(&format!("the treaty you signed (treaty #{personal})")), "{et}");
    assert!(!et.contains("your corporation"), "a member is not told: only the CEO");
    assert!(one(&s, f).contains(&format!("a treaty your corporation signed (treaty #{corporate})")));
    for p in [b, c, d, e, f] {
        mark_all_read(&mut s, p);
    }
    // Removing a citation and adding it back tells no one again; a revision by an author whose
    // record is newly cited merges both reasons in one notice.
    let mut drop = write(Some(page), None, "An account, shorter.");
    drop.citations = Some(vec![cite(Record::Radio, radio)]);
    s.chronicle(a, drop).unwrap();
    let mut back = write(Some(page), None, "An account, longer again.");
    back.citations = Some(vec![cite(Record::Radio, radio), cite(Record::Page, cpage)]);
    let out = s.chronicle(a, back).unwrap();
    assert_eq!(out.reply["told"]["cited"], 0);
    assert!(unread(&s, c, "chronicle").is_empty());
    let bpage = s.chronicle(b, write(None, Some("Bram's ledger"), "Numbers.")).unwrap().reply["page"]
        .as_u64()
        .unwrap();
    let mut both = write(Some(bpage), None, "Numbers, and the rim.");
    both.citations = Some(vec![cite(Record::Radio, radio)]);
    s.chronicle(a, both).unwrap();
    let n = unread(&s, b, "chronicle");
    assert_eq!(n.len(), 1, "{n:?}");
    assert!(n[0].text.contains("has a new revision 2") && n[0].text.contains("It cites your public radio message"));
    assert!(n[0].fold.is_none(), "a notice with a citation line no longer folds");
    s.replays();
}

#[test]
fn page_and_talk_mentions_ride_on_author_notices_and_strikes_reach_the_copies() {
    let (mut s, [a, b, c, _, e, _]) = cast();
    let mut first = write(None, Some("Dock talk"), "@Bram Ostrowski was there.");
    first.mentions = Some(vec!["Esko Vaara".into()]);
    let out = s.chronicle(a, first).unwrap();
    let page = out.reply["page"].as_u64().unwrap();
    assert_eq!(
        out.reply["mentions"]["notified"],
        serde_json::json!(["Esko Vaara", "Bram Ostrowski"])
    );
    assert!(out.summary.contains("2 mentioned, 2 notified"), "{}", out.summary);
    let n = unread(&s, b, "mention");
    assert_eq!(n.len(), 1);
    assert!(n[0].text.contains(&format!("on Chronicle page {page}, revision 1")));
    assert!(n[0].quote.as_deref().unwrap().contains("@Bram Ostrowski was there."));
    // A revision keeping the mention tells no one again; one mentioning an author rides on its
    // revision notice.
    let mut again = write(Some(page), None, "@Bram Ostrowski was there, and @Ada Mbeki.");
    again.mentions = Some(vec![]);
    let before = s.w.players[&b].inbox.len();
    let out = s.chronicle(c, again).unwrap();
    assert_eq!(s.w.players[&b].inbox.len(), before, "not re-notified");
    assert_eq!(
        out.reply["mentions"]["not_notified"],
        serde_json::json!([{"name": "Bram Ostrowski", "why": "already_mentioned"}])
    );
    let n = unread(&s, a, "chronicle");
    assert_eq!(n.len(), 1, "one notice: {n:?}");
    assert!(n[0].text.ends_with(" It mentions you."), "{}", n[0].text);
    assert!(n[0].mention.is_some() && n[0].fold.is_none());
    assert!(unread(&s, a, "mention").is_empty());
    // A mute drops the mention mark, never the author notice.
    s.run(
        Actor::Player(a),
        Command::Mentions(af_core::mention::Args {
            action: af_core::mention::Action::Mute,
            target: Some("Esko Vaara".into()),
        }),
    )
    .unwrap();
    mark_all_read(&mut s, a);
    s.chronicle(e, talk_args(page, "Ask @Ada Mbeki.", false, vec![])).unwrap();
    let n = unread(&s, a, "chronicle");
    assert_eq!(n.len(), 1);
    assert!(n[0].mention.is_none() && n[0].fold.is_some());
    // Wrong action: refused before anything changes.
    let hash = s.w.state_hash();
    let r = s
        .chronicle(
            a,
            chronicle::Args {
                mentions: Some(vec!["Bram Ostrowski".into()]),
                ..endorse(page)
            },
        )
        .unwrap_err();
    assert_eq!(r.code, "mentions_not_here");
    assert_eq!(s.w.state_hash(), hash);
    // A page citing this one, then the strike: quotes and citation copies read as struck.
    let mut citing = write(None, Some("About dock talk"), "See it.");
    citing.citations = Some(vec![cite(Record::Page, page)]);
    let citing = s.chronicle(b, citing).unwrap().reply["page"].as_u64().unwrap();
    s.admin(admin(AdminAction::Strike, None, Some(page), None)).unwrap();
    let mut scrubbed = 0;
    for p in [a, b, e] {
        for n in s.w.players[&p]
            .inbox
            .iter()
            .filter(|n| n.about.as_ref().is_some_and(|x| x.item() == af_core::mention::Item::Page(page)))
        {
            if n.quote.is_some() {
                assert_eq!(n.quote.as_deref(), Some(chronicle::STRUCK));
                assert!(n.struck_quote.is_some());
                scrubbed += 1;
            }
        }
    }
    // b's mention excerpt and a's revision and talk notices at least.
    assert!(scrubbed >= 3, "{scrubbed}");
    assert!(
        s.w.players[&b]
            .inbox
            .iter()
            .any(|n| n.kind == "mention" && n.struck_quote.as_deref().is_some_and(|q| q.contains("@Bram Ostrowski")))
    );
    let c0 = &s.w.chronicle.pages[&citing].current().citations[0];
    assert_eq!(c0.on_record, chronicle::STRUCK);
    assert_eq!(
        c0.struck_original.as_deref(),
        Some(format!("Chronicle page {page}: Dock talk").as_str())
    );
    let shown = chronicle::page_json(&s.w, &s.w.chronicle.pages[&citing], None, chronicle::View::Captain).unwrap();
    assert!(!shown.to_string().contains("Dock talk"), "captains never see the original");
    // Talk on a struck page stores no mentions and says so.
    let out = s.chronicle(e, talk_args(page, "Hello @Bram Ostrowski.", false, vec![])).unwrap();
    assert_eq!(out.reply["mentions"]["note"], "page struck: mentions tell no one");
    assert!(s.w.chronicle.pages[&page].talk.last().unwrap().mentions.is_empty());
    let struck = chronicle::page_json(&s.w, &s.w.chronicle.pages[&page], None, chronicle::View::Captain).unwrap();
    assert!(struck.get("byline").is_none() && struck.get("mentions").is_none());
    let op = chronicle::operator_page(&s.w, page).unwrap();
    assert!(op["byline"].is_object() && op["page"]["mentions"].is_array());
    s.replays();
}

#[test]
fn bylines_count_substantive_contributors_and_canon_names_get_a_moment() {
    let (mut s, [a, b, c, d, e, f]) = cast();
    let page = s
        .chronicle(
            a,
            write(None, Some("The rim war"), "@Chidi Okonjo and @Dalia Haddad fought at the rim."),
        )
        .unwrap()
        .reply["page"]
        .as_u64()
        .unwrap();
    // A typo fix is not a contribution; five new words are; so is a new citation.
    s.chronicle(b, write(Some(page), None, "@Chidi Okonjo and @Dalia Haddad fought at the rim!"))
        .unwrap();
    s.chronicle(
        d,
        write(
            Some(page),
            None,
            "@Chidi Okonjo and @Dalia Haddad fought at the rim! Five ships burned that night.",
        ),
    )
    .unwrap();
    let headline = s.w.news.last().unwrap().clone();
    let mut cited = write(
        Some(page),
        None,
        "@Chidi Okonjo and @Dalia Haddad fought at the rim! Five ships burned that night.",
    );
    cited.citations = Some(vec![Cite {
        record: Record::News,
        id: None,
        tick: Some(headline.tick),
        quote: Some(headline.headline.chars().take(8).collect()),
        note: None,
    }]);
    s.chronicle(e, cited).unwrap();
    let pg = &s.w.chronicle.pages[&page];
    assert_eq!(chronicle::contributors(pg), vec![d, e]);
    // The list is stored as each revision is written (each compared with the one before only),
    // so a read never compares revision bodies again: it reads what is stored.
    assert_eq!(pg.contributors, Some(vec![d, e]));
    assert_eq!(chronicle::compute_contributors(pg), vec![d, e]);
    let mut stored = pg.clone();
    stored.contributors = Some(vec![f]);
    assert_eq!(chronicle::contributors(&stored), vec![f], "reads use the stored list");
    let v = chronicle::page_json(&s.w, pg, None, chronicle::View::Captain).unwrap();
    assert_eq!(v["byline"]["created_by"], "Ada Mbeki");
    assert_eq!(v["byline"]["contributors"], serde_json::json!(["Dalia Haddad", "Esko Vaara"]));
    assert_eq!(v["mentions"], serde_json::json!(["Chidi Okonjo", "Dalia Haddad"]));
    // Canon: Chidi (named, not an author) gets a moment; Dalia (an author) does not.
    let named = |s: &Sim, p: PlayerId| s.w.identities[&p].moments.iter().filter(|m| m.kind == "chronicle_named").count();
    s.chronicle(c, endorse(page)).unwrap();
    assert_eq!(named(&s, c), 0, "not canon yet");
    let out = s.chronicle(f, endorse(page)).unwrap();
    assert_eq!(out.reply["canon"], true);
    assert_eq!(named(&s, c), 1);
    assert_eq!(named(&s, d), 0);
    // Later revisions of a canon page add no moments.
    s.chronicle(
        a,
        write(
            Some(page),
            None,
            "@Chidi Okonjo, @Dalia Haddad and @Fen Ilori fought at the rim! Five ships burned that night.",
        ),
    )
    .unwrap();
    assert_eq!(named(&s, f), 0);
    // The bible carries the mention graph and the canon's mentions and contributors.
    let bible = chronicle::story_bible(&s.w, None);
    assert!(bible["mentions"]["totals"]["mentions"].as_u64().unwrap() >= 2);
    let canon = &bible["canon"][0];
    assert!(canon["mentions"].as_array().unwrap().len() == 3);
    assert_eq!(canon["contributors"], serde_json::json!(["Dalia Haddad", "Esko Vaara"]));
    // The creator's own revision added no one; the stored list still matches a full comparison.
    let pg = &s.w.chronicle.pages[&page];
    assert_eq!(pg.contributors, Some(chronicle::compute_contributors(pg)));
    s.replays();
}

#[test]
fn the_briefing_lists_recent_pages_for_you_and_one_unwritten_record() {
    let (mut s, [a, b, c, d, e, _]) = cast();
    // Ten ticks to a game day keep the test short.
    s.run(
        Actor::System,
        Command::Admin(AdminArgs::SetRules {
            patch: serde_json::json!({"tick_ms": 8_640_000}),
        }),
    )
    .unwrap();
    let tpd = af_core::player::ticks_per_day(&s.w.rules) as u64;
    assert_eq!(tpd, 10);
    let tick = |s: &mut Sim, n: u64| {
        for _ in 0..n {
            s.run(Actor::System, Command::Tick).unwrap();
        }
    };
    // a signs a treaty with b: the unwritten record.
    let t = s
        .run(
            Actor::Player(a),
            Command::Treaty(TreatyArgs {
                action: TreatyAction::Propose,
                kind: Some(TreatyKind::Nap),
                corp: None,
                captain: Some("Bram Ostrowski".into()),
                personal: false,
                treaty_id: None,
                bond: None,
                terms: None,
                duration_ticks: None,
            }),
        )
        .unwrap()
        .reply["treaty_id"]
        .as_u64()
        .unwrap();
    s.run(
        Actor::Player(b),
        Command::Treaty(TreatyArgs {
            action: TreatyAction::Accept,
            kind: None,
            corp: None,
            captain: None,
            personal: false,
            treaty_id: Some(t),
            bond: None,
            terms: None,
            duration_ticks: None,
        }),
    )
    .unwrap();
    let ch = |s: &Sim, p: PlayerId| query_briefing(&s.w, p)["chronicle"].clone();
    let u = ch(&s, a)["unwritten"].clone();
    assert_eq!(u["fact"], format!("Treaty #{t} (yours) has no Chronicle page citing it."));
    tick(&mut s, 3);
    assert_eq!(ch(&s, a)["unwritten"], u, "stable within a game day");
    // a endorses c's page (a touch); then three pages by others are for a.
    let touched = s.chronicle(c, write(None, Some("Okun"), "A place.")).unwrap().reply["page"]
        .as_u64()
        .unwrap();
    s.chronicle(a, endorse(touched)).unwrap();
    let untouched = s.chronicle(c, write(None, Some("Elsewhere"), "Far.")).unwrap().reply["page"]
        .as_u64()
        .unwrap();
    tick(&mut s, 1);
    s.chronicle(d, write(Some(touched), None, "A place, revised.")).unwrap();
    s.chronicle(d, write(Some(untouched), None, "Far, revised.")).unwrap();
    s.chronicle(e, write(None, Some("Gossip"), "@Ada Mbeki was seen.")).unwrap();
    let mut cites = write(None, Some("Peace"), "They agreed.");
    cites.citations = Some(vec![cite(Record::Treaty, t)]);
    s.chronicle(b, cites).unwrap();
    let v = ch(&s, a);
    assert_eq!(v["for_you_count"], 3, "{v}");
    let rows = v["for_you"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "at most two");
    assert_eq!(rows[0]["why"], "cites a record you were part of");
    assert_eq!(rows[0]["title"]["untrusted_player_text"], "Peace");
    assert_eq!(rows[1]["why"], "mentions you");
    assert!(v.get("unwritten").is_none(), "gone once cited");
    // Three, not four: the page a endorsed counts as revised since a last worked on it; the
    // page it never touched does not.
    // Compact keeps only the count.
    let compact = af_core::query::briefing_with(
        &s.w,
        a,
        &af_core::query::BriefingArgs {
            compact: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(compact["chronicle"]["for_you_count"], 3);
    assert!(compact["chronicle"].get("for_you").is_none() && compact["chronicle"].get("unwritten").is_none());
    // After a game day the pages are no longer recent.
    tick(&mut s, tpd);
    assert_eq!(ch(&s, a)["for_you_count"], 0);
    s.replays();
}

#[test]
fn a_citation_of_the_same_record_with_another_tick_is_not_new() {
    let (mut s, [a, b, c, d, _, _]) = cast();
    let radio = s
        .run(
            Actor::Player(b),
            Command::Radio(RadioArgs {
                message: "Ore is cheap at the rim.".into(),
                ..Default::default()
            }),
        )
        .unwrap()
        .reply["message_id"]
        .as_u64()
        .unwrap();
    let with_tick = |tick: Option<u64>| Cite {
        record: Record::Radio,
        id: Some(radio),
        tick,
        quote: None,
        note: None,
    };
    let mut first = write(None, Some("Ore"), "Cheap ore at the rim.");
    first.citations = Some(vec![with_tick(None)]);
    let out = s.chronicle(a, first).unwrap();
    let page = out.reply["page"].as_u64().unwrap();
    assert_eq!(out.reply["told"]["cited"], 1);
    mark_all_read(&mut s, b);
    // The same message cited again with an arbitrary tick is the same record: no one is told
    // again, and a revision that only changes that tick is no contribution.
    let mut again = write(Some(page), None, "Cheap ore at the rim.");
    again.citations = Some(vec![with_tick(Some(1))]);
    assert_eq!(s.chronicle(a, again).unwrap().reply["told"]["cited"], 0);
    let mut noise = write(Some(page), None, "Cheap ore at the rim.");
    noise.citations = Some(vec![with_tick(Some(2))]);
    assert_eq!(s.chronicle(c, noise).unwrap().reply["told"]["cited"], 0);
    let talk = talk_args(page, "Seen it.", false, vec![with_tick(Some(3))]);
    assert_eq!(s.chronicle(d, talk).unwrap().reply["told"]["cited"], 0);
    assert!(unread(&s, b, "chronicle").is_empty());
    assert!(chronicle::contributors(&s.w.chronicle.pages[&page]).is_empty());
    s.replays();
}
