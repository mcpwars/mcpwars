#!/usr/bin/env bash
# Fails on a commit this public repository must not carry.
# Usage: scripts/check-commits.sh <base> [<head>]   every commit in base..head (head defaults to HEAD)
#        scripts/check-commits.sh --message <file>  one proposed message and the identity git will
#                                                   record (scripts/hooks/commit-msg)
# Rules: the author is the project account below; the committer is that account or GitHub (merges
# made on github.com); no message carries an agent session trailer or session link.
set -euo pipefail
account='Custos <1263658+Custos@users.noreply.github.com>'
web='GitHub <noreply@github.com>'
session='Claude-Session:|claude\.ai/code/session_'

fail=0
complain() { echo "check-commits: $*" >&2; fail=1; }

# "Name <email> 1791850497 -0300" -> "Name <email>"
ident() { sed -E 's/ [0-9]+ [+-][0-9]{4}$//' <<< "$1"; }

if [ "${1:-}" = "--message" ]; then
  msg="${2:?usage: check-commits.sh --message <file>}"
  author=$(ident "$(git var GIT_AUTHOR_IDENT)")
  committer=$(ident "$(git var GIT_COMMITTER_IDENT)")
  [ "$author" = "$account" ] || complain "author is \"$author\", expected \"$account\" (git commit --amend --reset-author)"
  [ "$committer" = "$account" ] || complain "committer is \"$committer\", expected \"$account\""
  # Only the message itself: git drops comment lines and everything below the scissors line.
  body=$(awk '/^# -+ >8 -+$/ {exit} !/^#/ {print}' "$msg")
  if grep -qE -- "$session" <<< "$body"; then
    complain "the message carries a session trailer or session link; remove it"
  fi
  [ "$fail" = 0 ] || exit 1
  exit 0
fi

base="${1:?usage: check-commits.sh <base> [<head>] | --message <file>}"
head="${2:-HEAD}"
git rev-parse --verify -q "$base^{commit}" > /dev/null || { echo "check-commits: unknown base $base" >&2; exit 2; }
git rev-parse --verify -q "$head^{commit}" > /dev/null || { echo "check-commits: unknown head $head" >&2; exit 2; }

commits=$(git rev-list "$base..$head")
n=0
while IFS= read -r c; do
  [ -n "$c" ] || continue
  n=$((n + 1))
  short=$(git rev-parse --short "$c")
  author=$(git log -1 --format='%an <%ae>' "$c")
  committer=$(git log -1 --format='%cn <%ce>' "$c")
  [ "$author" = "$account" ] || complain "$short author is \"$author\", expected \"$account\""
  [ "$committer" = "$account" ] || [ "$committer" = "$web" ] ||
    complain "$short committer is \"$committer\", expected \"$account\" or \"$web\""
  body=$(git log -1 --format=%B "$c")
  if grep -qE -- "$session" <<< "$body"; then
    complain "$short message carries a session trailer or session link"
  fi
done <<< "$commits"

if [ "$fail" != 0 ]; then
  echo "check-commits: refused. Fix with git commit --amend --reset-author or git rebase, then retry." >&2
  exit 1
fi
echo "commits clean ($n in $base..$head)"
