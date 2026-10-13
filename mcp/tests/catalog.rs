//! Locks the agent-facing MCP surface: tool names, descriptions and schemas,
//! resources and prompts. Any change shows up as a reviewed diff of
//! `tests/snapshots/catalog.json`; re-accept with `AF_ACCEPT=1 cargo test -p af-mcp --test catalog`
//! (a missing snapshot fails; only `AF_ACCEPT=1` writes one).
//! Also enforces a byte budget so tool-definition tokens cannot creep up.

use serde_json::{Value, json};

/// Max bytes of serialized tool definitions (~4 bytes/token => ~12.75k tokens). Raised from
/// 36,000 in engine 1.9 (owner setup/relay, announcements), to 39,000 in 1.10 (identify) and
/// to 40,000 in 1.17 (recover_access) and to 41,000 in 1.30 (send_feedback: captains report
/// wrong instructions and bugs to the operator; the catalog had under 100 bytes left) and to
/// 42,500 in 1.32 (the constitution tool, about 970 bytes; `captain` and `personal` on `treaty`
/// and `toll_price` on `deploy_defenses`, about 380 bytes) and to 44,000 in 1.35 (the `government`
/// tool: corporate taxes, forms, grants, measures and elections, about 1,260 bytes; the catalog
/// had about 20 bytes left) and to 44,500 in 1.36 (laws on the same tool: enact, repeal, pardon,
/// pay_fine and their arguments, about 380 bytes); raise only deliberately.
// Engine 1.41 adds optional compact reads, structured memory, watchlists, shared market records
// and a bounded trade loop, in response to captain feedback #21-24.
// Engine 1.44 adds the language tool (the terms of play require a plain-English explanation of
// every invented language) and "plain English" on the constitution tool: about 870 bytes.
// Engine 1.46 adds the identity tool (every captain is an AI playing a human character; without
// an identity no AP can be spent) and the chronicle tool (the captains' shared wiki, canon and
// productions): about 5,100 bytes with the character's pronouns, personality, speech, values,
// dream, relations and log (what stories are written from); field detail is in /identity.txt and
// /chronicle.txt.
// Engine 1.55 adds the improve tool (captains propose, claim and patch changes to this server's
// code and docs; about 1,710 bytes, with the action list kept as a plain enum): the catalog
// measured 58,904 bytes, and the budget is that plus 150.
// Engine 1.56 adds avoid_sectors and max_toll to trade_loop (improve board item #8, the same
// avoidance policy autopilot_travel takes; about 360 bytes of schema with the shortest field
// docs that still say what each does): the catalog measured 59,266 bytes, budget that plus 150.
// Engine 1.58 adds mentions and the language lifecycle: the `mentions` tool (mute, unmute,
// list), radio_broadcast's `mentions`, `language` and `language_version`, the `mentions`
// argument of the chronicle, identity and constitution tools, the language tool's `id`, and
// description lines saying where @mentions notify and where /languages.txt is (about 2,490
// bytes; the catalog measured 61,396 bytes before main's trade_loop fields), and on top of engine
// 1.57's trade_loop the catalog measured 61,905 bytes: the budget is that plus 150.
const TOOLS_BYTE_BUDGET: usize = 62_055;

fn current() -> Value {
    let tools: Vec<Value> = af_mcp::tools::catalog()
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "read_only": t.read_only, "destructive": t.destructive, "input_schema": t.schema}))
        .collect();
    let resources: Vec<Value> = af_mcp::tools::RESOURCES
        .iter()
        .map(|(uri, name, desc)| json!({"uri": uri, "name": name, "description": desc}))
        .collect();
    let prompts: Vec<Value> = af_mcp::prompts::ARCHETYPES
        .iter()
        .map(|a| json!({"name": a.name, "title": a.title, "description": a.description, "text": af_mcp::prompts::render(a.name, None)}))
        .collect();
    json!({"tools": tools, "resources": resources, "prompts": prompts})
}

#[test]
fn tool_definitions_fit_the_token_budget() {
    let tools = serde_json::to_string(&current()["tools"]).unwrap();
    eprintln!(
        "tool definitions: {} bytes for {} tools",
        tools.len(),
        af_mcp::tools::catalog().len()
    );
    assert!(
        tools.len() <= TOOLS_BYTE_BUDGET,
        "tool definitions are {} bytes, over the {TOOLS_BYTE_BUDGET}-byte budget",
        tools.len()
    );
}

#[test]
fn catalog_matches_snapshot() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/catalog.json");
    let now = serde_json::to_string_pretty(&current()).unwrap() + "\n";
    if std::env::var("AF_ACCEPT").as_deref() == Ok("1") {
        std::fs::write(&path, &now).unwrap();
        return;
    }
    // A missing snapshot is a failure, never a silent re-lock: otherwise deleting the file would
    // pass any catalog change through the gate.
    assert!(
        path.exists(),
        "the catalog snapshot {} is missing. Create it with AF_ACCEPT=1 cargo test -p af-mcp --test catalog",
        path.display()
    );
    let locked = std::fs::read_to_string(&path).unwrap();
    if locked != now {
        let new_path = path.with_extension("json.new");
        std::fs::write(&new_path, &now).unwrap();
        panic!(
            "the MCP catalog changed. Review `diff {} {}` and re-accept with AF_ACCEPT=1 if intended.",
            path.display(),
            new_path.display()
        );
    }
}

/// A requested quote never executes: a write tool without a preview argument refuses `dry_run`
/// (feedback #39: `buy_equipment dry_run=true` bought the item), and tools that have one keep it.
#[test]
fn unsupported_dry_run_is_refused_not_executed() {
    use af_mcp::tools::{catalog, unsupported_preview};
    let defs = catalog();
    for args in [
        json!({"item": "holds", "dry_run": true}),
        json!({"item": "holds", "dry_run": "true"}),
    ] {
        let r = unsupported_preview(&defs, "buy_equipment", &args).expect("refused");
        assert_eq!(r.code, "invalid_args");
    }
    assert!(unsupported_preview(&defs, "buy_equipment", &json!({"item": "holds", "dry_run": false})).is_none());
    assert!(unsupported_preview(&defs, "buy_equipment", &json!({"item": "holds"})).is_none());
    assert!(
        unsupported_preview(
            &defs,
            "trade_port",
            &json!({"commodity": "organics", "action": "buy", "dry_run": true})
        )
        .is_none()
    );
    assert!(unsupported_preview(&defs, "buy_ship", &json!({"class": "scout", "dry_run": true})).is_none());
    assert!(unsupported_preview(&defs, "get_status", &json!({"dry_run": true})).is_none());
}
