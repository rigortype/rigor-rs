#!/usr/bin/env bash
# The quick local gate: run it before every push to a draft PR.
#
# Usage: harness/gate.sh [--base <rev>]    (default base: origin/master)
#
# It covers what the draft PR's CI would otherwise report 5-6 minutes later,
# for only the crates the branch touches:
#   1. docs_check.py (docs budgets and links);
#   2. `cargo test -p <crate>` for each crate changed since the merge base
#      with <rev>, committed or not;
#   3. a debug build of rigor-cli and `run_snapshot.rb` (fixture parity, 0
#      unregistered FP), when any crate or the harness changed.
#
# Left to CI on push (AGENTS.md → Gates): the full workspace test on Linux and
# macOS, clippy 1.88 with --all-targets, and the live-reference snapshot
# check. Left to the pre-ready step: the FP sweep and the review gate.
# Quiet on success; on the first failing step, prints its last 30 lines and
# exits with its status.
set -euo pipefail

BASE=origin/master
if [[ ${1:-} == --base ]]; then BASE=${2:?--base needs a revision}; shift 2; fi
[[ $# -eq 0 ]] || { echo "usage: $0 [--base <rev>]" >&2; exit 2; }

cd "$(dirname "$0")/.."
MB=$(git merge-base HEAD "$BASE")
CHANGED=$( { git diff --name-only "$MB"; git ls-files --others --exclude-standard; } | sort -u)

LOG=$(mktemp -t rigor-gate)
step() {
  local label=$1; shift
  local start=$SECONDS
  printf '== %s ... ' "$label"
  if "$@" >"$LOG" 2>&1; then
    echo "ok ($((SECONDS - start))s)"
  else
    local code=$?
    echo "FAILED (exit $code)"
    tail -30 "$LOG"
    echo "(full output: $LOG)"
    exit "$code"
  fi
}

step "docs_check" python3 harness/docs_check.py

CRATES=$(grep -o '^crates/[^/]*' <<<"$CHANGED" | sort -u | sed 's|^crates/||' || true)
for c in $CRATES; do
  [[ -f crates/$c/Cargo.toml ]] || continue
  step "cargo test -p $c" cargo test -q --locked --offline -p "$c"
done

if [[ -n $CRATES ]] || grep -q '^harness/' <<<"$CHANGED"; then
  step "build rigor-cli (debug)" cargo build -q --locked --offline -p rigor-cli
  step "run_snapshot.rb" ruby harness/run_snapshot.rb
else
  echo "== run_snapshot.rb skipped (no crates/ or harness/ change since ${MB:0:7})"
fi

echo "gate: pass. Push, then read CI with: gh pr checks --watch"
