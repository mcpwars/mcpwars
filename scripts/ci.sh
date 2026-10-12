#!/usr/bin/env bash
# CI gate: run before every merge. Fails fast.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== fmt";      cargo fmt --all -- --check
echo "== clippy";   cargo clippy --workspace --all-targets -- -D warnings
echo "== tests";    cargo test --workspace --release -q

echo "== core stays pure (no async runtime, storage, network)"
if cargo tree -p af-core -e normal --prefix none | grep -E '^(tokio|redb|postgres|axum|reqwest|rmcp|hyper) ' ; then
  echo "af-core must not depend on IO crates" >&2; exit 1
fi
# Opt in where Playwright/Chromium is installed, like the optional Postgres backend tests.
if [[ "${AF_TEST_SYSOP_UI:-0}" == "1" ]]; then
  echo "== sysop browser regression"
  cargo build --release -p af-mcp --bin af-server
  node scripts/test-sysop-scroll.mjs
fi
echo "CI gate passed."
