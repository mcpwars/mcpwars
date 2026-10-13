#!/usr/bin/env bash
# Fails if owner-facing material is tracked (or staged) in the public repository.
# Usage: scripts/check-public-tree.sh            (tracked tree: scripts/ci.sh and CI)
#        scripts/check-public-tree.sh --staged   (the index: scripts/hooks/pre-commit)
# Three checks:
#  1. names: owner material paths (private/, reports, plans, reviews, logs, env files, ...);
#  2. submodules or embedded repositories (mode 160000), never allowed;
#  3. contents: fixed strings from a local deny file, one per line (PUBLIC_DENY_FILE, default
#     private/tools/deny-strings.txt in this checkout or its main worktree). The file stays on the
#     owner's machine and is never an Actions secret (pull requests run untrusted code in CI). When
#     it is absent this check is skipped; CI therefore runs only checks 1 and 2.
# Never prints a deny string, only the paths that contain one.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
staged=0
if [ "${1:-}" = "--staged" ]; then
  staged=1
  paths=$(git diff --cached --name-only --diff-filter=ACMR)
else
  paths=$(git ls-files)
fi

deny='^private(/|$)|^reports/|^research_notes/|^docs/research/|^docs/(HANDOFF|SPEC)\.md$|^docs/private/|^eval/results/[^/]*heldout'
deny="$deny"'|(^|/)[^/]*(report|handoff|audit|review|legal|counsel|budget|plan|deny|secret)[^/]*\.(md|txt|pdf|docx|log)$'
deny="$deny"'|(^|/)conduct[^/]*\.(md|json|txt)$|\.log$|\.swp$|(^|/)\.env($|\.)'
bad=""
if [ -n "$paths" ]; then
  bad=$(grep -iE -- "$deny" <<< "$paths" || true)
fi

# A submodule or embedded repository (mode 160000) is never allowed.
links=$(git ls-files -s | awk '$1=="160000"{print $4}')

# The deny file: given, or in this checkout, or in the main worktree when this is a linked one.
deny_file="${PUBLIC_DENY_FILE:-}"
if [ -z "$deny_file" ]; then
  deny_file=private/tools/deny-strings.txt
  if [ ! -f "$deny_file" ]; then
    main_tree=$(cd "$(git rev-parse --git-common-dir)/.." && pwd)
    [ -f "$main_tree/$deny_file" ] && deny_file="$main_tree/$deny_file"
  fi
fi
content=""
if [ -f "$deny_file" ] && [ -n "$paths" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  # Drop blank lines (a blank pattern matches every file) and carriage returns.
  tr -d '\r' < "$deny_file" > "$tmp/raw"
  grep -v -E '^[[:space:]]*$' "$tmp/raw" > "$tmp/patterns" || true
  if [ -s "$tmp/patterns" ]; then
    while IFS= read -r f; do
      [ -n "$f" ] || continue
      if [ "$staged" = 1 ]; then
        # The staged blob, which is what the commit will contain.
        git show ":$f" > "$tmp/blob" 2>/dev/null || continue
        target="$tmp/blob"
      else
        [ -f "$f" ] || continue
        target="$f"
      fi
      if grep -q -I -i -F -f "$tmp/patterns" -- "$target"; then
        content="$content$f"$'\n'
      fi
    done <<< "$paths"
  fi
  echo "deny strings: checked"
else
  echo "deny strings: no local deny file, content check skipped"
fi

if [ -n "$bad$links$content" ]; then
  echo "Owner-facing material in the public tree. It belongs in private/ (the private docs repository):" >&2
  while IFS= read -r p; do
    if [ -n "$p" ]; then printf '  %s\n' "$p" >&2; fi
  done <<< "$bad"$'\n'"$links"$'\n'"$content"
  exit 1
fi
echo "public tree clean"
