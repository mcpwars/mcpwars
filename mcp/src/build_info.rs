//! What this process is running: the build's commit and when it started. Served at `/version`
//! with the engine, the live rules version and the documentation revision, so an operator (or a
//! script) can tell which build a deploy put live without logging in.

use std::sync::OnceLock;

use serde_json::{Value, json};

/// The commit is fixed at startup; `started_ms` is the wall-clock time the process recorded it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildInfo {
    pub commit: String,
    pub started_ms: u64,
}

static INFO: OnceLock<BuildInfo> = OnceLock::new();

/// Record the commit and start time. Call it once, early in startup; later calls (and the first
/// `/version` request in a process that never called it) return what was recorded first.
pub fn init() -> &'static BuildInfo {
    INFO.get_or_init(|| BuildInfo {
        commit: commit_from(std::env::var("RENDER_GIT_COMMIT").ok().as_deref(), option_env!("AF_GIT_COMMIT")),
        started_ms: crate::game::now_ms(),
    })
}

/// The commit: the platform's runtime variable (Render sets `RENDER_GIT_COMMIT`), then the
/// value baked in at compile time (`AF_GIT_COMMIT`), then "unknown". Only a plain hex commit id
/// is accepted, so a stray value in either variable never reaches a public reply.
pub fn commit_from(runtime: Option<&str>, compiled: Option<&str>) -> String {
    [runtime, compiled]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|c| is_commit(c))
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "unknown".to_string())
}

fn is_commit(c: &str) -> bool {
    (7..=64).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The `/version` body: exactly `{commit, engine, rules_version, doc_revision, started_ms}`.
pub fn version_json(rules_version: String) -> Value {
    let info = init();
    json!({
        "commit": info.commit,
        "engine": af_core::ENGINE_VERSION,
        "rules_version": rules_version,
        "doc_revision": crate::documentation::revision(),
        "started_ms": info.started_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::commit_from;

    const SHA: &str = "83c418f0a1b2c3d4e5f60718293a4b5c6d7e8f90";

    #[test]
    fn the_runtime_commit_wins_then_the_compiled_one_then_unknown() {
        assert_eq!(commit_from(Some(SHA), Some("abcdef1")), SHA);
        assert_eq!(commit_from(None, Some("abcdef1")), "abcdef1");
        assert_eq!(commit_from(Some(""), Some("abcdef1")), "abcdef1");
        assert_eq!(commit_from(None, None), "unknown");
    }

    #[test]
    fn only_a_hex_commit_id_is_reported() {
        assert_eq!(commit_from(Some(" ABCDEF1\n"), None), "abcdef1");
        assert_eq!(commit_from(Some("main"), None), "unknown");
        assert_eq!(commit_from(Some("abc"), None), "unknown");
        assert_eq!(commit_from(Some("postgres://user:pw@host/db"), Some("not-a-sha")), "unknown");
        assert_eq!(commit_from(Some(&"a".repeat(65)), None), "unknown");
    }
}
