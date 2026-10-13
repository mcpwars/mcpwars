#!/usr/bin/env bash
# CI gate: run before every merge, and by .github/workflows/ci.yml (job "gate"). Fails fast.
# Commit check base: CI_BASE (and CI_HEAD, default HEAD) when set, otherwise origin/main when it
# shares history with HEAD; with neither, the commit check is skipped (never in a pull request).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== public tree"; scripts/check-public-tree.sh

echo "== commits"
base="${CI_BASE:-}"
if [ -z "$base" ] && git rev-parse --verify -q origin/main > /dev/null &&
  git merge-base origin/main HEAD > /dev/null 2>&1; then
  base=origin/main
fi
if [ -z "$base" ] || [ "$base" = 0000000000000000000000000000000000000000 ] ||
  ! git rev-parse --verify -q "$base^{commit}" > /dev/null; then
  # A pull request always knows its base, so there a missing one is a failure, never a skip.
  if [ "${GITHUB_EVENT_NAME:-}" = pull_request ]; then
    echo "pull request base '${base}' is not in this clone: cannot check its commits" >&2; exit 1
  fi
  echo "no usable base ref ('${base}': none, a new branch or a force push): commit check skipped"
else
  scripts/check-commits.sh "$base" "${CI_HEAD:-HEAD}"
fi

echo "== fmt";      cargo fmt --all -- --check
echo "== clippy";   cargo clippy --locked --workspace --all-targets -- -D warnings
echo "== tests";    cargo test --locked --workspace --release -q

echo "== core stays pure (no async runtime, storage, network)"
if cargo tree --locked -p af-core -e normal --prefix none | grep -E '^(tokio|redb|postgres|axum|reqwest|rmcp|hyper) ' ; then
  echo "af-core must not depend on IO crates" >&2; exit 1
fi
# Opt in where Playwright/Chromium is installed, like the optional Postgres backend tests.
if [[ "${AF_TEST_SYSOP_UI:-0}" == "1" ]]; then
  echo "== sysop browser regression"
  cargo build --locked --release -p af-mcp --bin af-server
  node scripts/test-sysop-scroll.mjs
fi
echo "CI gate passed."
