//! Locks the agent-facing MCP surface: tool names, descriptions and schemas,
//! resources and prompts. Any change shows up as a reviewed diff of
//! `tests/snapshots/catalog.json`; re-accept with `AF_ACCEPT=1 cargo test -p af-mcp --test catalog`.
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
const TOOLS_BYTE_BUDGET: usize = 51_000;

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
    if std::env::var("AF_ACCEPT").is_ok() || !path.exists() {
        std::fs::write(&path, &now).unwrap();
        return;
    }
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
