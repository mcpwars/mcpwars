//! Adopting a 1.54 universe into engine 1.55: a world serialized before the improve board
//! existed (no `World.improve`, no `improve_*` rules) loads with the board empty, the kill switch
//! off and every limit at its non-zero default, hashes exactly like a native 1.55 world, and the
//! board then works on it once switched on.

use af_core::cmd::*;
use af_core::engine::{Actor, Entry, apply};
use af_core::galaxy::{GenesisConfig, generate};
use af_core::model::*;
use af_core::rules::Rules;
use af_core::{Outcome, Refusal, World};
use serde_json::{Map, Value, json};

/// Drop every key the 1.55 engine added: `improve` from the world, `improve_*` from its rules.
fn as_1_54(w: &World) -> (Value, usize) {
    let mut v = serde_json::to_value(w).unwrap();
    let o = v.as_object_mut().unwrap();
    assert!(o.remove("improve").is_some(), "a 1.55 world serializes its board");
    let rules = o.get_mut("rules").and_then(Value::as_object_mut).unwrap();
    let keys: Vec<String> = rules.keys().filter(|k| k.starts_with("improve_")).cloned().collect();
    for k in &keys {
        rules.remove(k);
    }
    (v, keys.len())
}

fn improve_rules(r: &Rules) -> Map<String, Value> {
    serde_json::to_value(r)
        .unwrap()
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, _)| k.starts_with("improve_"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

struct Run {
    w: World,
    seq: u64,
}

impl Run {
    fn exec(&mut self, actor: Actor, cmd: Command) -> Result<Outcome, Refusal> {
        self.seq += 1;
        let e = Entry {
            seq: self.seq,
            at_ms: self.seq * 1_000,
            actor,
            cmd,
        };
        // Through JSON, as the store journals it.
        let e: Entry = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        apply(&mut self.w, &e)
    }
}

fn register(r: &mut Run, name: &str) -> PlayerId {
    let out = r
        .exec(
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

fn improve(v: Value) -> Command {
    Command::Improve(serde_json::from_value(v).unwrap())
}

#[test]
fn a_1_54_world_without_the_board_loads_with_safe_defaults_and_hashes_like_1_55() {
    let mut r = Run {
        w: generate(&GenesisConfig::new(154, 60)),
        seq: 0,
    };
    let a = register(&mut r, "Alder");
    register(&mut r, "Birch");
    r.exec(Actor::System, Command::Tick).unwrap();

    let (old, removed) = as_1_54(&r.w);
    assert!(removed >= 20, "every improve rule is a 1.55 field ({removed} removed)");
    let text = serde_json::to_string(&old).unwrap();
    assert!(!text.contains("improve"), "the 1.54 shape names nothing of the board");
    let adopted: World = serde_json::from_str(&text).expect("a 1.54 world loads into 1.55");

    // The board is empty and closed; every limit is its default, and none is zero.
    assert_eq!(adopted.improve, Default::default());
    assert!(!adopted.rules.improve_enabled);
    let limits = improve_rules(&adopted.rules);
    assert_eq!(limits, improve_rules(&Rules::default()));
    for (k, v) in &limits {
        if k != "improve_enabled" {
            assert!(v.as_f64().is_some_and(|n| n > 0.0), "{k} defaults to {v} in an adopted universe");
        }
    }
    // Rules read alone (a genesis config or a rules patch base) fill the same defaults.
    let rules_only: Rules = serde_json::from_value(old["rules"].clone()).unwrap();
    assert_eq!(improve_rules(&rules_only), limits);
    // Nothing else moved: the adopted world is the native one, hash for hash.
    assert_eq!(adopted.state_hash(), r.w.state_hash());

    // The adopted universe runs the board once the operator switches it on.
    let mut r = Run { w: adopted, seq: r.seq };
    let submit = json!({"action":"submit","kind":"bug","title":"T","text":"x"});
    let before = r.w.state_hash();
    let refused = r.exec(Actor::Player(a), improve(submit.clone())).unwrap_err();
    assert_eq!(refused.code, "improve_disabled");
    assert_eq!(r.w.state_hash(), before, "a refusal leaves the adopted world unchanged");
    r.exec(
        Actor::System,
        Command::Admin(AdminArgs::SetRules {
            patch: json!({"improve_enabled": true}),
        }),
    )
    .unwrap();
    let out = r.exec(Actor::Player(a), improve(submit)).unwrap();
    assert_eq!(out.reply["id"], 1, "ids start at 1 on an adopted board");
    assert_eq!(out.reply["claimed"], true);
    assert_eq!(r.w.improve.items.len(), 1);
}
