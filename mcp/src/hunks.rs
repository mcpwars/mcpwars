//! The improve patch pre-check: every hunk's line counts must match its header, as `git apply`
//! reads them. It runs in the MCP layer before the command, never in `af_core::improve::check_diff`:
//! journal entries accepted before it existed must still replay to the same world.
//!
//! `git apply` stops reading a hunk when its header's counts are used up and skips what follows,
//! so a hand-edited header can make a patch apply with lines silently dropped, or fail. The board
//! refuses such a diff (`bad_hunk`) with nothing stored.

const TAIL: &str = "Make the diff with git diff; nothing was stored. Guide: /improve.txt";

/// Header lines of a `diff --git` section that change a file without any hunk.
const NO_HUNK_HEADERS: &[&str] = &[
    "old mode ",
    "new mode ",
    "new file mode ",
    "deleted file mode ",
    "rename from ",
    "rename to ",
    "copy from ",
    "copy to ",
    "similarity index ",
    "dissimilarity index ",
    // Refused by the board's own check, with its own message.
    "Binary files ",
    "GIT binary patch",
];

struct Section {
    name: String,
    git: bool,
    /// Has its `--- ` and `+++ ` lines.
    pair: bool,
    /// Has a header that changes the file without a hunk.
    headers: bool,
    hunks: usize,
}

struct Hunk {
    n: usize,
    header: (u64, u64, u64, u64),
    old: u64,
    new: u64,
}

impl Hunk {
    fn missing(&self) -> (u64, u64) {
        (self.header.1.saturating_sub(self.old), self.header.3.saturating_sub(self.new))
    }
}

/// `@@ -a[,b] +c[,d] @@`, a missing count meaning 1.
fn header(line: &str) -> Option<(u64, u64, u64, u64)> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let number = |s: &str| -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let range = |r: &str| -> Option<(u64, u64)> {
        match r.split_once(',') {
            Some((start, count)) => Some((number(start)?, number(count)?)),
            None => Some((number(r)?, 1)),
        }
    };
    let (a, b) = range(old)?;
    let (c, d) = range(new)?;
    Some((a, b, c, d))
}

/// A file name as a header shows it: before a tab or timestamp, without its `a/` or `b/`.
fn shown(raw: &str) -> String {
    let raw = raw.split('\t').next().unwrap_or(raw);
    let raw = if raw.starts_with('"') {
        raw
    } else {
        raw.split(' ').next().unwrap_or(raw)
    };
    raw.strip_prefix("a/").or_else(|| raw.strip_prefix("b/")).unwrap_or(raw).to_string()
}

fn pair_name(minus: &str, plus: &str) -> String {
    let plus = shown(plus);
    if plus == "/dev/null" { shown(minus) } else { plus }
}

fn git_name(rest: &str) -> String {
    match rest.rfind(" b/") {
        Some(at) => rest[at + 3..].to_string(),
        None => rest.to_string(),
    }
}

fn finish_hunk(h: &Hunk, s: &Section) -> Result<(), String> {
    let (a, b, c, d) = h.header;
    if (h.old, h.new) != (b, d) {
        return Err(format!(
            "{}: hunk {} says -{a},{b} +{c},{d} but carries {} old and {} new lines. {TAIL}",
            s.name, h.n, h.old, h.new
        ));
    }
    Ok(())
}

fn finish_section(s: Option<Section>) -> Result<(), String> {
    match s {
        Some(s) if s.pair && s.hunks == 0 => Err(format!("{}: has '--- ' and '+++ ' lines but no hunk. {TAIL}", s.name)),
        Some(s) if s.git && !s.pair && !s.headers => Err(format!(
            "{}: has no hunk, and no rename, mode change, new or deleted file. {TAIL}",
            s.name
        )),
        _ => Ok(()),
    }
}

/// Check every hunk of a unified diff: its counts against its lines, no stray line inside it, and
/// at least one hunk in each file section that has `--- ` and `+++ ` lines (a `diff --git` section
/// without them must rename, copy, change a mode, or add or delete an empty file). The message
/// names the file and the hunk.
pub fn check(diff: &str) -> Result<(), String> {
    let lines: Vec<&str> = diff.lines().map(|l| l.strip_suffix('\r').unwrap_or(l)).collect();
    let mut section: Option<Section> = None;
    let mut hunk: Option<Hunk> = None;
    // After a format-patch signature ("-- "), until the next file or hunk.
    let mut trailer = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let pair = line.starts_with("--- ") && lines.get(i + 1).is_some_and(|n| n.starts_with("+++ "));
        if let Some(h) = hunk.as_mut() {
            let (old_left, new_left) = h.missing();
            let full = old_left == 0 && new_left == 0;
            // A '--- '/'+++ ' pair is two hunk lines while both counts still need one, as git reads it.
            let boundary = line.starts_with("@@") || line.starts_with("diff --git ") || (pair && (old_left == 0 || new_left == 0));
            if !boundary {
                match line.as_bytes().first() {
                    // An empty line is a context line whose space was stripped; after a full hunk
                    // it is only a blank line.
                    None if full => {}
                    None | Some(b' ') => {
                        h.old += 1;
                        h.new += 1;
                    }
                    Some(b'-') if full && line == "-- " => {
                        finish_hunk(h, section.as_ref().expect("a hunk has a section"))?;
                        hunk = None;
                        trailer = true;
                    }
                    Some(b'-') => h.old += 1,
                    Some(b'+') => h.new += 1,
                    Some(b'\\') => {}
                    _ if full => {
                        // Another header (an index line, a plain diff's command line): the hunk is done.
                        finish_hunk(h, section.as_ref().expect("a hunk has a section"))?;
                        hunk = None;
                        continue;
                    }
                    _ => {
                        let s = section.as_ref().expect("a hunk has a section");
                        return Err(format!(
                            "{}: hunk {} holds a line that is not context, '-' or '+' (diff line {}). {TAIL}",
                            s.name,
                            h.n,
                            i + 1
                        ));
                    }
                }
                i += 1;
                continue;
            }
            finish_hunk(h, section.as_ref().expect("a hunk has a section"))?;
            hunk = None;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish_section(section.take())?;
            section = Some(Section {
                name: git_name(rest),
                git: true,
                pair: false,
                headers: false,
                hunks: 0,
            });
            trailer = false;
            i += 1;
            continue;
        }
        if pair {
            let name = pair_name(&line[4..], &lines[i + 1][4..]);
            match section.as_mut() {
                Some(s) if s.git && !s.pair && s.hunks == 0 => {
                    s.pair = true;
                    s.name = name;
                }
                _ => {
                    finish_section(section.take())?;
                    section = Some(Section {
                        name,
                        git: false,
                        pair: true,
                        headers: false,
                        hunks: 0,
                    });
                }
            }
            trailer = false;
            i += 2;
            continue;
        }
        if line.starts_with("@@") {
            trailer = false;
            let Some(s) = section.as_mut().filter(|s| s.pair) else {
                let name = section.as_ref().map_or("the diff".to_string(), |s| s.name.clone());
                return Err(format!(
                    "{name}: a hunk comes before its '--- ' and '+++ ' lines (diff line {}). {TAIL}",
                    i + 1
                ));
            };
            s.hunks += 1;
            let Some(parsed) = header(line) else {
                return Err(format!(
                    "{}: hunk {} has a malformed header (diff line {}); it must read @@ -a,b +c,d @@. {TAIL}",
                    s.name,
                    s.hunks,
                    i + 1
                ));
            };
            hunk = Some(Hunk {
                n: s.hunks,
                header: parsed,
                old: 0,
                new: 0,
            });
            i += 1;
            continue;
        }
        if !trailer
            && let Some(s) = section.as_mut()
            && s.git
            && !s.pair
            && NO_HUNK_HEADERS.iter().any(|k| line.starts_with(k))
        {
            s.headers = true;
        }
        i += 1;
    }
    if let Some(h) = &hunk {
        finish_hunk(h, section.as_ref().expect("a hunk has a section"))?;
    }
    finish_section(section)
}

#[cfg(test)]
mod tests {
    use super::check;

    fn ok(diff: &str) {
        assert_eq!(check(diff), Ok(()), "{diff}");
    }

    fn bad(diff: &str, want: &str) {
        let err = check(diff).expect_err(diff);
        assert!(err.contains(want), "{err}");
        assert!(
            err.ends_with("Make the diff with git diff; nothing was stored. Guide: /improve.txt"),
            "{err}"
        );
    }

    #[test]
    fn git_diffs_pass() {
        // A modification with context, two hunks.
        ok(
            "diff --git a/docs/x.md b/docs/x.md\nindex 1111111..2222222 100644\n--- a/docs/x.md\n+++ b/docs/x.md\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n@@ -10,2 +10,3 @@ fn ctx()\n ten\n+ten and a half\n eleven\n",
        );
        // A new file, a deleted file and no newline at the end of a file.
        ok(
            "diff --git a/new.md b/new.md\nnew file mode 100644\nindex 0000000..3333333\n--- /dev/null\n+++ b/new.md\n@@ -0,0 +1,2 @@\n+first\n+second\n\\ No newline at end of file\ndiff --git a/old.md b/old.md\ndeleted file mode 100644\nindex 4444444..0000000\n--- a/old.md\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n",
        );
        // A last line changed where neither side ends with a newline.
        ok(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-x\n\\ No newline at end of file\n+y\n\\ No newline at end of file\n",
        );
        // An empty new file, a rename and a mode change have no hunk.
        ok(
            "diff --git a/empty.md b/empty.md\nnew file mode 100644\nindex 0000000..e69de29\ndiff --git a/a.md b/b.md\nsimilarity index 100%\nrename from a.md\nrename to b.md\ndiff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n",
        );
        // Deleted and added lines that look like file headers, counted as git counts them.
        ok("diff --git a/n.md b/n.md\n--- a/n.md\n+++ b/n.md\n@@ -1,2 +1,2 @@\n--- a dash line\n+++ a plus line\n keep\n");
        // A context line whose space an editor stripped, and CRLF line ends.
        ok("--- a/c.md\r\n+++ b/c.md\r\n@@ -1,3 +1,3 @@\r\n one\r\n\r\n-three\r\n+THREE\r\n");
        // A plain unified diff of two files, with a timestamp and a trailing blank line.
        ok(
            "--- a/p.md\t2026-10-14 10:00:00\n+++ b/p.md\t2026-10-14 10:00:01\n@@ -1 +1 @@\n-p\n+q\n\n--- a/r.md\n+++ b/r.md\n@@ -2 +2 @@\n-r\n+s\n",
        );
        // git format-patch adds a signature after the last hunk.
        ok("diff --git a/s.md b/s.md\n--- a/s.md\n+++ b/s.md\n@@ -1 +1 @@\n-a\n+b\n-- \n2.47.0\n\n");
    }

    #[test]
    fn wrong_counts_are_refused_with_the_file_and_the_hunk() {
        // The header promises a line the hunk does not carry.
        bad(
            "diff --git a/docs/x.md b/docs/x.md\n--- a/docs/x.md\n+++ b/docs/x.md\n@@ -1,2 +1,3 @@\n one\n+two\n",
            "docs/x.md: hunk 1 says -1,2 +1,3 but carries 1 old and 2 new lines.",
        );
        // An added line beyond the count (git apply would drop it).
        bad(
            "--- a/y.md\n+++ b/y.md\n@@ -1 +1 @@\n-a\n+b\n+c\n",
            "y.md: hunk 1 says -1,1 +1,1 but carries 1 old and 2 new lines.",
        );
        // The second hunk is short, before the next file.
        bad(
            "diff --git a/z.md b/z.md\n--- a/z.md\n+++ b/z.md\n@@ -1 +1 @@\n-a\n+b\n@@ -5,2 +5,2 @@\n-e\n+E\ndiff --git a/w.md b/w.md\n--- a/w.md\n+++ b/w.md\n@@ -1 +1 @@\n-a\n+b\n",
            "z.md: hunk 2 says -5,2 +5,2 but carries 1 old and 1 new lines.",
        );
        // Short at the end of the diff.
        bad(
            "--- a/e.md\n+++ b/e.md\n@@ -1,2 +1,2 @@\n-a\n+b\n",
            "e.md: hunk 1 says -1,2 +1,2 but carries 1 old and 1 new lines.",
        );
    }

    #[test]
    fn stray_lines_missing_hunks_and_bad_headers_are_refused() {
        bad(
            "--- a/s.md\n+++ b/s.md\n@@ -1,2 +1,2 @@\n-a\nnot a diff line\n+b\n",
            "s.md: hunk 1 holds a line that is not context, '-' or '+' (diff line 5).",
        );
        bad(
            "diff --git a/h.md b/h.md\n--- a/h.md\n+++ b/h.md\n",
            "h.md: has '--- ' and '+++ ' lines but no hunk.",
        );
        bad("diff --git a/g.md b/g.md\nindex 1111111..2222222 100644\n", "g.md: has no hunk");
        bad(
            "--- a/m.md\n+++ b/m.md\n@@ -1,x +1 @@\n-a\n+b\n",
            "m.md: hunk 1 has a malformed header (diff line 3)",
        );
        bad("--- a/m.md\n+++ b/m.md\n@@ -1 +1\n-a\n+b\n", "m.md: hunk 1 has a malformed header");
        bad("@@ -1 +1 @@\n-a\n+b\n", "the diff: a hunk comes before its '--- ' and '+++ ' lines");
    }
}
