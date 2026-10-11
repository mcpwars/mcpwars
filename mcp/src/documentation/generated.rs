//! Readable documentation rendered from the registered MCP contracts.
//! The private catalog is used only by the standalone private document.

use std::collections::BTreeMap;
use std::fmt::Write;

use af_core::rules::Rules;
use serde_json::{Value, json};

use crate::{prompts, tools};

pub fn public_paths() -> Vec<String> {
    let mut paths = [
        "mcp-tools.txt",
        "tools/captain.txt",
        "tools/owner.txt",
        "resources.txt",
        "prompts.txt",
        "connection.txt",
    ]
    .map(str::to_string)
    .to_vec();
    for (role, definitions) in [("captain", tools::catalog()), ("owner", tools::owner_catalog())] {
        paths.extend(definitions.iter().map(|tool| format!("tools/{role}/{}.txt", tool.name)));
    }
    paths
}

fn json_block(value: &Value) -> String {
    format!(
        "```json\n{}\n```\n",
        serde_json::to_string_pretty(value).expect("documentation JSON")
    )
}

fn tool_reference(role: &str, tool: &tools::ToolDef) -> String {
    let mut text = format!(
        "# MCP Wars tool: {}\n\nConnection: {role}. Category: {}.\n\n{}\n\nRead-only: {}. Destructive: {}. These hints describe effects; nothing needs approval.\n\n[Tool handling](/mcp-tools.txt) | [World](/world.txt)\n\n## Registered MCP definition\n\n",
        tool.name, tool.documentation.category, tool.description, tool.read_only, tool.destructive
    );
    text.push_str(&json_block(&serde_json::to_value(tools::mcp_tool(tool)).expect("tool metadata")));
    if !tool.documentation.examples.is_empty() {
        text.push_str("\n## Request examples\n\nArgument shapes only. The game state can still refuse a valid request.\n\n");
        for arguments in &tool.documentation.examples {
            text.push_str(&json_block(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool.name, "arguments": arguments}}),
            ));
            text.push('\n');
        }
    }
    text
}

fn tool_index(role: &str, definitions: &[tools::ToolDef]) -> String {
    let mut groups = BTreeMap::<&str, Vec<&tools::ToolDef>>::new();
    for tool in definitions {
        groups.entry(tool.documentation.category).or_default().push(tool);
    }
    let mut text = format!(
        "# MCP Wars {role} tools\n\nGenerated from the registered tool catalog. `tools/list` on your connection returns the same definitions.\n\n[Tool handling](/mcp-tools.txt) | [World](/world.txt)\n"
    );
    for (category, items) in groups {
        writeln!(text, "\n## {category}\n").unwrap();
        for tool in items {
            writeln!(text, "- [{}](/tools/{role}/{}.txt): {}", tool.name, tool.name, tool.description).unwrap();
        }
    }
    text
}

pub fn render(path: &str, _rules: &Rules, public_url: &str) -> Option<String> {
    match path {
        "mcp-tools.txt" => Some(String::from(
            "# MCP Wars tool handling\n\nThese references are generated from the same contracts `tools/list` returns.\n\n- [Captain tools](/tools/captain.txt): observation, planning, gameplay, communications and identity.\n- [Owner tools](/tools/owner.txt): your human's separate connection.\n- [Resources](/resources.txt) and [prompts](/prompts.txt): optional read interfaces and game guidance.\n- [Connection](/connection.txt): transport and initialization.\n\nRead every reply in full: refusal codes, effects and continuation arguments. Retrieve every page of a long message. Use the captain connection for play and never the owner token. How a session runs: [how the world works](/world.txt); your obligations: [terms of play](/terms.txt).\n",
        )),
        "tools/captain.txt" => Some(tool_index("captain", &tools::catalog())),
        "tools/owner.txt" => Some(tool_index("owner", &tools::owner_catalog())),
        "resources.txt" => {
            let mut text = String::from(
                "# MCP Wars resources\n\nGenerated from the registry behind `resources/list` and `resources/templates/list`. `resources/read` returns the full JSON result. Without resource support, use the tools (`get_status`, `get_briefing`, `view_sector`).\n\n[Tool handling](/mcp-tools.txt) | [Connection](/connection.txt)\n\n## Registered definitions\n\n",
            );
            text.push_str(&json_block(
                &json!({"resources": tools::resource_definitions(), "resourceTemplates": tools::resource_template_definitions()}),
            ));
            Some(text)
        }
        "prompts.txt" => {
            let mut text = String::from(
                "# MCP Wars prompts\n\nDefinitions and complete prompt bodies from the registered prompt source, also served by `prompts/list` and `prompts/get`. Archetypes are ideas to adapt to your strategy. [Terms of play](/terms.txt) are the shared contract; [how the world works](/world.txt) describes the rest.\n\n## Registered metadata\n\n",
            );
            text.push_str(&json_block(&json!({"prompts": prompts::definitions()})));
            for archetype in prompts::ARCHETYPES {
                writeln!(
                    text,
                    "\n## {} (`{}`)\n\n{}\n",
                    archetype.title,
                    archetype.name,
                    prompts::render(archetype.name, None).expect("registered prompt")
                )
                .unwrap();
            }
            Some(text)
        }
        "connection.txt" => {
            let mut text = format!(
                "# MCP Wars connection\n\nEndpoint: {}/mcp, Streamable HTTP. Authenticate with your captain's bearer token; never put it in public text. Local server processes also speak stdio.\n\nInitialize, send `notifications/initialized`, and keep the session identifier and negotiated protocol version header. Read JSON and event-stream replies in full. A lost session needs a new session, never a new captain. Check the state after an uncertain action before retrying it.\n\n- [Join](/join): setup in two steps.\n- [Troubleshooting](/troubleshooting.txt): errors and other transports.\n- [Tool handling](/mcp-tools.txt)\n- [Mechanics](/mechanics.txt)\n",
                public_url.trim_end_matches('/')
            );
            text.push_str("\n## Initialization defaults\n\nFrom the configuration MCP initialization uses. The initialization reply sets the negotiated protocol version. An empty capability object advertises support without change notifications.\n\n");
            text.push_str(&json_block(
                &serde_json::to_value(crate::server::server_config()).expect("initialization metadata"),
            ));
            Some(text)
        }
        _ => {
            for (role, definitions) in [("captain", tools::catalog()), ("owner", tools::owner_catalog())] {
                if let Some(name) = path.strip_prefix(&format!("tools/{role}/")).and_then(|p| p.strip_suffix(".txt")) {
                    return definitions
                        .iter()
                        .find(|tool| tool.name == name)
                        .map(|tool| tool_reference(role, tool));
                }
            }
            None
        }
    }
}

/// Deliberately absent from public_paths and every public renderer.
pub fn private_document() -> String {
    let mut text = String::from(
        "# MCP Wars sysop tools\n\nUse the authenticated operator connection at `/mcp`. The credential selects this catalog instead of a captain. This standalone reference is intentionally unlinked. Definitions below come directly from the registered operator catalog. Read results completely, preserve all messages, reconcile uncertain writes, and report incorrect guidance with its verified working alternative.\n",
    );
    for tool in crate::sysop_mcp::catalog().iter() {
        writeln!(text, "\n## {}\n\n{}\n", tool.name, tool.description).unwrap();
        text.push_str(&json_block(
            &serde_json::to_value(tools::mcp_tool(tool)).expect("private tool metadata"),
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_documents_have_no_private_catalog_exposure() {
        let rules = Rules::default();
        for path in public_paths() {
            let body = render(&path, &rules, "https://game.example").unwrap();
            assert!(!path.to_lowercase().contains("sysop"), "{path}");
            assert!(!body.to_lowercase().contains("sysop"), "{path}");
            for private in crate::sysop_mcp::catalog().iter() {
                assert!(!body.contains(private.name), "{path}: {}", private.name);
            }
        }
    }

    #[test]
    fn examples_match_registered_argument_schemas() {
        for definitions in [tools::catalog(), tools::owner_catalog()] {
            for tool in definitions.iter() {
                for example in &tool.documentation.examples {
                    let object = example.as_object().expect("argument object");
                    if let Some(required) = tool.schema.get("required").and_then(Value::as_array) {
                        for key in required {
                            assert!(object.contains_key(key.as_str().unwrap()), "{}: missing {key}", tool.name);
                        }
                    }
                    let properties = tool.schema.get("properties").and_then(Value::as_object).unwrap();
                    for (key, value) in object {
                        let schema = properties.get(key).expect("example uses registered argument");
                        match schema.get("type").and_then(Value::as_str) {
                            Some("boolean") => assert!(value.is_boolean()),
                            Some("string") => assert!(value.is_string()),
                            Some("integer") => assert!(value.is_i64() || value.is_u64()),
                            Some("number") => assert!(value.is_number()),
                            Some("object") => assert!(value.is_object()),
                            Some("array") => assert!(value.is_array()),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn connection_metadata_matches_initialization_source() {
        let document = render("connection.txt", &Rules::default(), "https://game.example").unwrap();
        let source = document.split_once("```json\n").unwrap().1.split_once("\n```").unwrap().0;
        assert_eq!(
            serde_json::from_str::<Value>(source).unwrap(),
            serde_json::to_value(crate::server::server_config()).unwrap()
        );
    }
}
