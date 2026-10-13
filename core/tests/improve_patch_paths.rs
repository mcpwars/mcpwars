//! The improve board's patch intake (engine 1.55): every way a diff can name a file that
//! `git apply` (default -p1) or `patch -p1` would write is checked against the protected and
//! invalid paths, and symlink or submodule modes are refused.

use af_core::improve::check_diff;

fn code(diff: &str) -> String {
    match check_diff(diff, 16_384, 8) {
        Ok(i) => format!("ok {:?}", i.files),
        Err(r) => r.code.to_string(),
    }
}

const HUNK: &str = "@@ -1 +1 @@\n-a\n+b\n";

#[test]
fn protected_files_cannot_hide_behind_another_prefix_or_quoting() {
    let cases: Vec<(String, &str)> = vec![
        // A traditional diff with other prefixes: git apply -p1 strips the first component.
        (format!("--- x/render.yaml\n+++ y/render.yaml\n{HUNK}"), "protected_path"),
        (format!("--- c/scripts/ci.sh\n+++ c/scripts/ci.sh\n{HUNK}"), "protected_path"),
        (format!("--- orig/NOTICE\n+++ new/NOTICE\n{HUNK}"), "protected_path"),
        // A timestamp after a space, as diff -u writes it.
        (
            format!("--- a/render.yaml 2026-10-12 10:00:00.000000000 +0000\n+++ b/render.yaml 2026-10-12 10:00:01.000000000 +0000\n{HUNK}"),
            "protected_path",
        ),
        // C-style quoted names: git unquotes \056 to '.'.
        (
            format!("--- \"a/render\\056yaml\"\n+++ \"b/render\\056yaml\"\n{HUNK}"),
            "protected_path",
        ),
        // A header-only section (mode change) with a quoted or non a/ b/ header.
        (
            format!(
                "--- a/docs/x.md\n+++ b/docs/x.md\n{HUNK}diff --git \"a/scripts/ci.sh\" \"b/scripts/ci.sh\"\nold mode 100755\nnew mode 100644\n"
            ),
            "protected_path",
        ),
        (
            format!(
                "--- a/docs/x.md\n+++ b/docs/x.md\n{HUNK}diff --git c/render.yaml d/render.yaml\nnew file mode 100644\nindex 0000000..e69de29\n"
            ),
            "protected_path",
        ),
        // .git in another case (case-insensitive file systems).
        (format!("--- a/.GIT/config\n+++ b/.GIT/config\n{HUNK}"), "invalid_path"),
        // Empty and '.' components resolve away.
        (
            format!("--- a/mcp/src/./documentation//guides/terms.rs\n+++ b/mcp/src/./documentation//guides/terms.rs\n{HUNK}"),
            "protected_path",
        ),
        (format!("--- a//render.yaml\n+++ b//render.yaml\n{HUNK}"), "invalid_path"),
        (format!("--- \"a/x\n+++ b/x\n{HUNK}"), "invalid_path"),
        (format!("--- \"a/docs/x\\nmore\"\n+++ b/x\n{HUNK}"), "invalid_path"),
    ];
    for (diff, want) in cases {
        assert_eq!(code(&diff), want, "{diff}");
    }
}

#[test]
fn symlinks_and_submodules_are_refused() {
    for mode in [
        "new file mode 120000\n",
        "old mode 100644\nnew mode 120000\n",
        "new file mode 160000\n",
        "index 0000000..1234567 120000\n",
    ] {
        let diff = format!(
            "diff --git a/web/x.txt b/web/x.txt\n{mode}--- /dev/null\n+++ b/web/x.txt\n@@ -0,0 +1 @@\n+../../.env\n\\ No newline at end of file\n"
        );
        assert_eq!(code(&diff), "invalid_patch", "{diff}");
    }
}

#[test]
fn bidirectional_controls_are_refused() {
    for c in ['\u{202E}', '\u{2067}'] {
        let diff = format!("--- a/web/watch.js\n+++ b/web/watch.js\n@@ -1 +1 @@\n-a\n+if (ok) {{ /* {c} }} admin() {{ */\n");
        assert_eq!(code(&diff), "invalid_patch");
    }
}

#[test]
fn ordinary_patches_still_pass() {
    let d =
        format!("diff --git a/docs/EVAL.md b/docs/EVAL.md\nindex 1111111..2222222 100644\n--- a/docs/EVAL.md\n+++ b/docs/EVAL.md\n{HUNK}");
    assert_eq!(code(&d), "ok [\"docs/EVAL.md\"]");
    let d = "diff --git a/docs/new.md b/docs/new.md\nnew file mode 100644\nindex 0000000..2222222\n--- /dev/null\n+++ b/docs/new.md\n@@ -0,0 +1 @@\n+x\n";
    assert_eq!(code(d), "ok [\"docs/new.md\"]");
    let d = "diff --git a/docs/a.md b/docs/b.md\nsimilarity index 100%\nrename from docs/a.md\nrename to docs/b.md\n--- a/docs/x.md\n+++ b/docs/x.md\n@@ -1 +1 @@\n-a\n+b\n";
    assert_eq!(code(d), "ok [\"docs/a.md\", \"docs/b.md\", \"docs/x.md\"]");
}
