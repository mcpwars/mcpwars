//! Authored page templates by public path. Do not maintain physical .txt copies or
//! duplicate generated schemas here. Numbers come only from `{{NAME}}` placeholders.

#[path = "guides/mod.rs"]
mod guides;

use guides::setup::Mode;

pub(super) use guides::{BETTER_METHOD, CHECKIN_PROMPT, RECONCILE};

/// Public paths of authored pages. `join` is served at /join.
pub(super) const PATHS: &[&str] = &[
    "join",
    "agents.txt",
    "troubleshooting.txt",
    "terms.txt",
    "world.txt",
    "memory.txt",
    "constitution.txt",
    "society.txt",
    "scheduled-play.txt",
    "platforms.txt",
];

pub(super) fn template(path: &str) -> Option<String> {
    Some(match path {
        "join" => guides::setup::page(Mode::Human),
        "agents.txt" => guides::setup::page(Mode::SelfDirected),
        "troubleshooting.txt" => guides::troubleshooting::PAGE.into(),
        "terms.txt" => guides::terms::PAGE.into(),
        "world.txt" => guides::world::PAGE.into(),
        "memory.txt" => guides::memory::PAGE.into(),
        "constitution.txt" => guides::constitution::PAGE.into(),
        "society.txt" => guides::society::PAGE.into(),
        "scheduled-play.txt" => guides::scheduled_play::PAGE.into(),
        "platforms.txt" => guides::platforms::PAGE.into(),
        _ => {
            let slug = path.strip_prefix("platforms/")?.strip_suffix(".txt")?;
            let (_, title, body) = platform_sections().into_iter().find(|(s, _, _)| s == slug)?;
            format!("# MCP Wars platform: {title}\n\n{body}\n\nEvery platform: [platform details](/platforms.txt). Setup: [join](/join).\n")
        }
    })
}

/// One page per `## ` section of /platforms.txt: (slug, title, body). Adding a section there
/// adds its page, its route and its link in /join's table, with nothing copied.
pub(super) fn platform_sections() -> Vec<(String, String, String)> {
    guides::platforms::PAGE
        .split("\n## ")
        .skip(1)
        .map(|chunk| {
            let (title, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
            let title = title.trim().to_string();
            let slug = title
                .to_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            (slug, title, body.trim().to_string())
        })
        .collect()
}
