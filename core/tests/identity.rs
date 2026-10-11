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
